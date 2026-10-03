#[cfg(feature = "picker")]
#[path = "gtk.rs"]
mod backend;
#[cfg(all(feature = "native-picker", not(feature = "picker")))]
#[path = "native.rs"]
mod backend;

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::niri_ipc::{NiriOutput, NiriWindow};

#[derive(Debug, Clone)]
pub enum PickerChoice {
    Monitor(String),
    Window(u64),
}

#[derive(Clone)]
pub(super) struct DisplayItem {
    pub(super) name: String,
    pub(super) width: i32,
    pub(super) height: i32,
}

#[derive(Clone)]
pub(super) struct WindowItem {
    pub(super) id: u64,
    pub(super) title: String,
    pub(super) app_id: String,
    pub(super) width: i32,
    pub(super) height: i32,
}

/// Holds the picker child so Session.Close can kill it on cancel/retry.
pub type PickerChildSlot = Arc<std::sync::Mutex<Option<std::process::Child>>>;

pub fn show_picker_cancellable(
    outputs: &[NiriOutput],
    windows: &[NiriWindow],
    child_slot: Option<PickerChildSlot>,
) -> Option<PickerChoice> {
    let displays: Vec<DisplayItem> = outputs.iter().map(DisplayItem::from).collect();
    let wins: Vec<WindowItem> = windows.iter().map(WindowItem::from).collect();

    if displays.is_empty() && wins.is_empty() {
        return None;
    }

    // Even when only one target exists, screen capture still crosses a permission
    // boundary. Never turn "one possible target" into implicit user consent.
    let bin = picker_bin();
    let mut child = match Command::new(&bin)
        .arg("--picker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to spawn picker {}: {e}", bin.display());
            return None;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = write_picker_targets(&mut stdin, &displays, &wins) {
            tracing::error!("failed to write picker targets: {e}");
            let _ = child.kill();
            return None;
        }
    }

    let stdout = child.stdout.take();

    let status = if let Some(slot) = &child_slot {
        if let Ok(mut guard) = slot.lock() {
            if let Some(mut old) = guard.take() {
                let _ = old.kill();
                let _ = old.wait();
            }
            *guard = Some(child);
        }

        loop {
            let finished = {
                let mut guard = slot.lock().ok();
                match guard.as_mut().and_then(|g| g.as_mut()) {
                    Some(c) => match c.try_wait() {
                        Ok(Some(status)) => Some(Ok(status)),
                        Ok(None) => None,
                        Err(e) => Some(Err(e)),
                    },
                    None => Some(Err(std::io::Error::other("picker killed"))),
                }
            };
            match finished {
                Some(result) => break result,
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    } else {
        child.wait()
    };

    if let Some(slot) = &child_slot {
        if let Ok(mut guard) = slot.lock() {
            if let Some(mut c) = guard.take() {
                let _ = c.wait();
            }
        }
    }

    let status = match status {
        Ok(s) => s,
        Err(e) => {
            tracing::info!("picker wait ended: {e}");
            return None;
        }
    };

    if !status.success() {
        tracing::info!("picker exited with {status}");
        return None;
    }

    let stdout = stdout?;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("M:") {
            return Some(PickerChoice::Monitor(name.to_string()));
        }
        if let Some(id) = line.strip_prefix("W:") {
            if let Ok(id) = id.parse::<u64>() {
                return Some(PickerChoice::Window(id));
            }
        }
    }
    None
}

pub fn kill_slotted_picker(slot: &PickerChildSlot) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(mut child) = guard.take() {
            tracing::info!("killing in-flight picker pid={}", child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// CLI entry for `--picker` / `--debug-picker`. Targets from stdin, else niri IPC.
pub fn run_picker_process() -> Option<PickerChoice> {
    let (displays, windows) = read_targets_or_query_niri();
    match backend::run(displays, windows) {
        Ok(choice) => choice,
        Err(error) => {
            tracing::error!("picker failed: {error:#}");
            None
        }
    }
}

fn picker_bin() -> PathBuf {
    // Prefer argv[0] so a Nix GApps wrapper is reused when packaged.
    if let Some(arg0) = std::env::args_os().next() {
        let p = PathBuf::from(arg0);
        if p.exists() {
            return p;
        }
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("niri-screenshare"))
}

fn write_picker_targets(
    out: &mut impl Write,
    displays: &[DisplayItem],
    windows: &[WindowItem],
) -> std::io::Result<()> {
    // D:name\tw\th  W:id\ttitle\tapp\tw\th  END
    for d in displays {
        writeln!(
            out,
            "D:{}\t{}\t{}",
            escape_field(&d.name),
            d.width,
            d.height
        )?;
    }
    for w in windows {
        writeln!(
            out,
            "W:{}\t{}\t{}\t{}\t{}",
            w.id,
            escape_field(&w.title),
            escape_field(&w.app_id),
            w.width,
            w.height
        )?;
    }
    writeln!(out, "END")?;
    out.flush()
}

fn escape_field(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn unescape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn read_targets_or_query_niri() -> (Vec<DisplayItem>, Vec<WindowItem>) {
    if let Some(targets) = read_targets_from_stdin() {
        return targets;
    }
    let displays = crate::niri_ipc::list_outputs()
        .unwrap_or_default()
        .iter()
        .map(DisplayItem::from)
        .collect();
    let windows = crate::niri_ipc::list_windows()
        .unwrap_or_default()
        .iter()
        .map(WindowItem::from)
        .collect();
    (displays, windows)
}

fn read_targets_from_stdin() -> Option<(Vec<DisplayItem>, Vec<WindowItem>)> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut displays = Vec::new();
    let mut windows = Vec::new();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let line = line.trim_end().to_string();
        if line == "END" {
            return Some((displays, windows));
        }
        if let Some(rest) = line.strip_prefix("D:") {
            let parts: Vec<&str> = rest.split('\t').collect();
            if parts.len() >= 3 {
                displays.push(DisplayItem {
                    name: unescape_field(parts[0]),
                    width: parts[1].parse().unwrap_or(0),
                    height: parts[2].parse().unwrap_or(0),
                });
            }
        } else if let Some(rest) = line.strip_prefix("W:") {
            let parts: Vec<&str> = rest.split('\t').collect();
            if parts.len() >= 5 {
                if let Ok(id) = parts[0].parse::<u64>() {
                    windows.push(WindowItem {
                        id,
                        title: unescape_field(parts[1]),
                        app_id: unescape_field(parts[2]),
                        width: parts[3].parse().unwrap_or(0),
                        height: parts[4].parse().unwrap_or(0),
                    });
                }
            }
        }
    }
    if displays.is_empty() && windows.is_empty() {
        None
    } else {
        Some((displays, windows))
    }
}

impl From<&NiriOutput> for DisplayItem {
    fn from(o: &NiriOutput) -> Self {
        Self {
            name: o.name.clone(),
            width: o.logical.width,
            height: o.logical.height,
        }
    }
}

impl From<&NiriWindow> for WindowItem {
    fn from(w: &NiriWindow) -> Self {
        Self {
            id: w.id,
            title: w.title.clone(),
            app_id: w.app_id.clone(),
            width: w.size.width,
            height: w.size.height,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip_plain() {
        assert_eq!(unescape_field(&escape_field("hello")), "hello");
    }

    #[test]
    fn escape_roundtrip_special_chars() {
        let original = "tab\there\nnewline\\backslash";
        assert_eq!(unescape_field(&escape_field(original)), original);
    }

    #[test]
    fn escape_produces_tab_safe_output() {
        let escaped = escape_field("a\tb");
        assert!(!escaped.contains('\t'));
    }

    #[test]
    fn unescape_handles_trailing_backslash() {
        assert_eq!(unescape_field("end\\"), "end\\");
    }

    #[test]
    fn unescape_handles_unknown_escape() {
        assert_eq!(unescape_field("a\\xb"), "a\\xb");
    }
}
