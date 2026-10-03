use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;

use anyhow::Context;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, Layer},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity},
};

use super::{DisplayItem, PickerChoice, WindowItem};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 64;

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Config {
    native_picker: NativePickerConfig,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct NativePickerConfig {
    style: Style,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Style {
    background: Color,
    hover_background: Color,
    pressed_background: Color,
    foreground: Color,
    hover_foreground: Color,
    pressed_foreground: Color,
    separator: Color,
    cancel_background: Color,
    cancel_hover_background: Color,
    cancel_pressed_background: Color,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            background: Color(0xff243344),
            hover_background: Color(0xff3a5068),
            pressed_background: Color(0xff172433),
            foreground: Color(0xffeeeeee),
            hover_foreground: Color(0xffffffff),
            pressed_foreground: Color(0xffeeeeee),
            separator: Color(0xff667788),
            cancel_background: Color(0xff663344),
            cancel_hover_background: Color(0xff884455),
            cancel_pressed_background: Color(0xff442233),
        }
    }
}

impl Style {
    fn colors(&self, button: Button, hovered: bool, pressed: bool) -> (u32, u32) {
        let pressed = hovered && pressed;
        let foreground = if pressed {
            self.pressed_foreground
        } else if hovered {
            self.hover_foreground
        } else {
            self.foreground
        };
        let background = if button == Button::Cancel {
            if pressed {
                self.cancel_pressed_background
            } else if hovered {
                self.cancel_hover_background
            } else {
                self.cancel_background
            }
        } else if pressed {
            self.pressed_background
        } else if hovered {
            self.hover_background
        } else {
            self.background
        };
        (background.0, foreground.0)
    }
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
struct Color(u32);

impl TryFrom<String> for Color {
    type Error = anyhow::Error;

    fn try_from(value: String) -> anyhow::Result<Self> {
        let hex = value.strip_prefix('#').context("color must start with #")?;
        anyhow::ensure!(
            hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "color must use #RRGGBB"
        );
        Ok(Self(0xff000000 | u32::from_str_radix(hex, 16)?))
    }
}

impl From<Color> for String {
    fn from(color: Color) -> Self {
        format!("#{:06x}", color.0 & 0xffffff)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Monitor,
    Window,
    Cancel,
}

fn button_at(x: f64, y: f64, width: u32, height: u32, allow_windows: bool) -> Option<Button> {
    if !(0.0..f64::from(width)).contains(&x) || !(0.0..f64::from(height)).contains(&y) {
        return None;
    }
    let source_width = width * 3 / 4;
    Some(if x >= f64::from(source_width) {
        Button::Cancel
    } else if allow_windows && x >= f64::from(source_width / 2) {
        Button::Window
    } else {
        Button::Monitor
    })
}

pub(super) fn run(
    displays: Vec<DisplayItem>,
    windows: Vec<WindowItem>,
) -> anyhow::Result<Option<PickerChoice>> {
    if displays.is_empty() {
        return pick_window(&windows);
    }
    let conn = Connection::connect_to_env()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut state = Picker {
        style: crate::config::load(Config::default()).native_picker.style,
        ..Picker::default()
    };
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;
    let compositor = state.compositor.as_ref().context("missing wl_compositor")?;
    let shell = state.shell.as_ref().context("missing layer-shell")?;
    anyhow::ensure!(state.shm.is_some(), "missing wl_shm");
    state.allow_windows = !windows.is_empty();

    for output in &state.outputs {
        if !displays.iter().any(|display| display.name == output.name) {
            continue;
        }
        let surface = compositor.create_surface(&qh, ());
        let layer = shell.get_layer_surface(
            &surface,
            Some(&output.proxy),
            Layer::Overlay,
            "niri-screenshare".into(),
            &qh,
            (),
        );
        layer.set_size(WIDTH, HEIGHT);
        layer.set_anchor(Anchor::Top);
        layer.set_margin(24, 0, 0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        surface.set_buffer_scale(output.scale);
        surface.commit();
        state.tiles.push(Tile {
            surface,
            layer,
            name: output.name.clone(),
            scale: output.scale,
            width: WIDTH,
            height: HEIGHT,
            configured: false,
            hovered: None,
            pressed: None,
        });
    }
    anyhow::ensure!(
        !state.tiles.is_empty(),
        "no requested Wayland outputs available"
    );
    while !state.done {
        queue.blocking_dispatch(&mut state)?;
        if let Some(error) = state.error.take() {
            return Err(error);
        }
    }
    for tile in &state.tiles {
        tile.layer.destroy();
        tile.surface.destroy();
    }
    // Ensure overlays and their keyboard grab are gone before Niri takes input
    // for window selection or the parent starts capture.
    queue.roundtrip(&mut state)?;
    if state.pick_window {
        pick_window(&windows)
    } else {
        Ok(state.choice)
    }
}

fn pick_window(windows: &[WindowItem]) -> anyhow::Result<Option<PickerChoice>> {
    if windows.is_empty() {
        return Ok(None);
    }
    let path = std::env::var("NIRI_SOCKET")
        .ok()
        .or_else(crate::niri_ipc::find_niri_socket)
        .context("cannot find Niri IPC socket")?;
    let mut socket = UnixStream::connect(path)?;
    pick_window_on_socket(&mut socket, windows)
}

fn pick_window_on_socket(
    socket: &mut UnixStream,
    windows: &[WindowItem],
) -> anyhow::Result<Option<PickerChoice>> {
    socket.write_all(b"\"PickWindow\"\n")?;
    let mut reply = String::new();
    BufReader::new(socket).read_line(&mut reply)?;
    let reply: serde_json::Value = serde_json::from_str(&reply)?;
    if let Some(error) = reply.get("Err") {
        anyhow::bail!("Niri window picker: {error}");
    }
    let window = reply
        .get("Ok")
        .and_then(|ok| ok.get("PickedWindow"))
        .context("unexpected Niri PickWindow reply")?;
    if window.is_null() {
        return Ok(None);
    }
    let id = window
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .context("picked window has no ID")?;
    Ok(windows
        .iter()
        .any(|window| window.id == id)
        .then_some(PickerChoice::Window(id)))
}

struct Output {
    proxy: wl_output::WlOutput,
    name: String,
    scale: i32,
}

struct Tile {
    surface: wl_surface::WlSurface,
    layer: zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
    name: String,
    scale: i32,
    width: u32,
    height: u32,
    configured: bool,
    hovered: Option<Button>,
    pressed: Option<Button>,
}

#[derive(Default)]
struct Picker {
    compositor: Option<wl_compositor::WlCompositor>,
    shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    shm: Option<wl_shm::WlShm>,
    outputs: Vec<Output>,
    tiles: Vec<Tile>,
    pointer_surface: Option<wl_surface::WlSurface>,
    pointer_x: f64,
    pointer_y: f64,
    style: Style,
    allow_windows: bool,
    pick_window: bool,
    choice: Option<PickerChoice>,
    done: bool,
    error: Option<anyhow::Error>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Picker {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
                "zwlr_layer_shell_v1" => {
                    state.shell = Some(registry.bind(name, version.min(4), qh, ()))
                }
                "wl_output" if version >= 4 => state.outputs.push(Output {
                    proxy: registry.bind(name, 4, qh, ()),
                    name: String::new(),
                    scale: 1,
                }),
                "wl_seat" => {
                    registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(7), qh, ());
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Picker {
    fn event(
        state: &mut Self,
        proxy: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let Some(output) = state
            .outputs
            .iter_mut()
            .find(|output| output.proxy == *proxy)
        {
            match event {
                wl_output::Event::Name { name } => output.name = name,
                wl_output::Event::Scale { factor } => output.scale = factor.clamp(1, 8),
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Picker {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Picker {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key: 1,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            state.done = true;
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Picker {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if state.done {
            return;
        }
        let mut left_button = None;
        match event {
            wl_pointer::Event::Enter {
                surface,
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_surface = Some(surface);
                state.pointer_x = surface_x;
                state.pointer_y = surface_y;
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_x = surface_x;
                state.pointer_y = surface_y;
            }
            wl_pointer::Event::Leave { .. } => state.pointer_surface = None,
            wl_pointer::Event::Button {
                button,
                state: WEnum::Value(button_state),
                ..
            } => {
                if button == 0x111 && button_state == wl_pointer::ButtonState::Released {
                    state.done = true;
                }
                if button == 0x110 {
                    left_button = Some(button_state);
                }
            }
            _ => {}
        }
        for tile in &mut state.tiles {
            let previous = (tile.hovered, tile.pressed);
            tile.hovered = if Some(&tile.surface) == state.pointer_surface.as_ref() {
                button_at(
                    state.pointer_x,
                    state.pointer_y,
                    tile.width,
                    tile.height,
                    state.allow_windows,
                )
            } else {
                None
            };
            match left_button {
                Some(wl_pointer::ButtonState::Pressed) => tile.pressed = tile.hovered,
                Some(wl_pointer::ButtonState::Released) => {
                    if let Some(button) = tile
                        .pressed
                        .filter(|pressed| Some(*pressed) == tile.hovered)
                    {
                        match button {
                            Button::Monitor => {
                                state.choice = Some(PickerChoice::Monitor(tile.name.clone()))
                            }
                            Button::Window => state.pick_window = true,
                            Button::Cancel => {}
                        }
                        state.done = true;
                    }
                    tile.pressed = None;
                }
                _ => {}
            }
            if tile.configured && previous != (tile.hovered, tile.pressed) {
                if let Err(error) = draw(
                    tile,
                    state.shm.as_ref().unwrap(),
                    qh,
                    state.allow_windows,
                    &state.style,
                ) {
                    state.error = Some(error);
                    state.done = true;
                }
            }
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Picker {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                if let Some(tile) = state.tiles.iter_mut().find(|tile| tile.layer == *layer) {
                    tile.width = if width == 0 { WIDTH } else { width };
                    tile.height = if height == 0 { HEIGHT } else { height };
                    tile.configured = true;
                    if let Err(error) = draw(
                        tile,
                        state.shm.as_ref().unwrap(),
                        qh,
                        state.allow_windows,
                        &state.style,
                    ) {
                        state.error = Some(error);
                        state.done = true;
                    }
                }
            }
            zwlr_layer_surface_v1::Event::Closed => state.done = true,
            _ => {}
        }
    }
}

fn draw(
    tile: &Tile,
    shm: &wl_shm::WlShm,
    qh: &QueueHandle<Picker>,
    allow_windows: bool,
    style: &Style,
) -> anyhow::Result<()> {
    let (width, height) = (tile.width, tile.height);
    anyhow::ensure!(
        width <= 4096 && height <= 4096,
        "invalid picker surface size"
    );
    let scale = tile.scale as u32;
    let (w, h) = (width * scale, height * scale);
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (x, y) = (x / scale, y / scale);
            let source_width = width * 3 / 4;
            let cancel = x >= source_width;
            let window = allow_windows && x >= source_width / 2;
            let center = if cancel {
                source_width + (width - source_width) / 2
            } else if allow_windows {
                if window {
                    source_width * 3 / 4
                } else {
                    source_width / 4
                }
            } else {
                source_width / 2
            };
            let dx = i64::from(x) - i64::from(center);
            let dy = i64::from(y) - i64::from(height / 2);
            let outline = ((-20..=20).contains(&dx) && dy.abs() == 14)
                || (dx.abs() == 20 && (-14..=14).contains(&dy));
            let detail = if window {
                (-19..=19).contains(&dx) && dy == -8
            } else {
                (dx == 0 && (15..=20).contains(&dy)) || (dx.abs() <= 10 && dy == 20)
            };
            let icon = if cancel {
                dx.abs() <= 10 && (dx - dy).abs().min((dx + dy).abs()) <= 1
            } else {
                outline || detail
            };
            let button = if cancel {
                Button::Cancel
            } else if window {
                Button::Window
            } else {
                Button::Monitor
            };
            let (background, foreground) = style.colors(
                button,
                tile.hovered == Some(button),
                tile.pressed == Some(button),
            );
            let color = if icon {
                foreground
            } else if x == source_width || (allow_windows && x == source_width / 2) {
                style.separator.0
            } else {
                background
            };
            pixels.extend_from_slice(&color.to_ne_bytes());
        }
    }
    let mut file = tempfile::tempfile()?;
    file.write_all(&pixels)?;
    let pool = shm.create_pool(file.as_fd(), pixels.len() as i32, qh, ());
    let buffer = pool.create_buffer(
        0,
        w as i32,
        h as i32,
        (w * 4) as i32,
        wl_shm::Format::Argb8888,
        qh,
        (),
    );
    pool.destroy();
    tile.surface.attach(Some(&buffer), 0, 0);
    tile.surface.damage_buffer(0, 0, w as i32, h as i32);
    tile.surface.commit();
    Ok(())
}

impl Dispatch<wl_buffer::WlBuffer, ()> for Picker {
    fn event(
        _: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

delegate_noop!(Picker: ignore wl_compositor::WlCompositor);
delegate_noop!(Picker: ignore wl_surface::WlSurface);
delegate_noop!(Picker: ignore wl_shm::WlShm);
delegate_noop!(Picker: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Picker: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_style_overrides_keep_unspecified_defaults() {
        let defaults = toml::Value::try_from(Config::default()).unwrap();
        let override_config =
            toml::from_str("[native_picker.style]\nhover_background = '#ABCDEF'\n").unwrap();
        let config: Config = serde_toml_merge::merge(defaults, override_config)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(config.native_picker.style.hover_background.0, 0xffabcdef);
        assert_eq!(
            config.native_picker.style.background.0,
            Style::default().background.0
        );
        for color in ["red", "#123", "#gggggg", "#+12345", "#12345678"] {
            assert!(Color::try_from(color.to_owned()).is_err());
        }
    }

    #[test]
    fn button_hit_testing_respects_boundaries_and_source_types() {
        assert_eq!(
            button_at(0.0, 0.0, WIDTH, HEIGHT, true),
            Some(Button::Monitor)
        );
        assert_eq!(
            button_at(96.0, 32.0, WIDTH, HEIGHT, true),
            Some(Button::Window)
        );
        assert_eq!(
            button_at(96.0, 32.0, WIDTH, HEIGHT, false),
            Some(Button::Monitor)
        );
        assert_eq!(
            button_at(192.0, 32.0, WIDTH, HEIGHT, true),
            Some(Button::Cancel)
        );
        assert_eq!(
            button_at(255.0, 32.0, WIDTH, HEIGHT, false),
            Some(Button::Cancel)
        );
        assert_eq!(button_at(256.0, 32.0, WIDTH, HEIGHT, true), None);
        assert_eq!(button_at(-1.0, 32.0, WIDTH, HEIGHT, true), None);
        assert_eq!(button_at(32.0, 64.0, WIDTH, HEIGHT, true), None);
    }

    #[test]
    fn pressed_style_only_applies_while_hovering_the_pressed_button() {
        let style = Style::default();
        for button in [Button::Monitor, Button::Window, Button::Cancel] {
            let normal = style.colors(button, false, false);
            let hover = style.colors(button, true, false);
            let pressed = style.colors(button, true, true);
            assert_ne!(normal, hover);
            assert_ne!(hover, pressed);
            assert_eq!(style.colors(button, false, true), normal);
        }
    }

    fn pick_reply(reply: &str) -> anyhow::Result<Option<PickerChoice>> {
        let (mut client, mut server) = UnixStream::pair()?;
        let reply = reply.to_owned();
        let server = std::thread::spawn(move || {
            let mut request = String::new();
            BufReader::new(&mut server).read_line(&mut request).unwrap();
            assert_eq!(request, "\"PickWindow\"\n");
            writeln!(server, "{reply}").unwrap();
        });
        let windows = [WindowItem {
            id: 42,
            title: String::new(),
            app_id: String::new(),
            width: 800,
            height: 600,
        }];
        let choice = pick_window_on_socket(&mut client, &windows);
        server.join().unwrap();
        choice
    }

    #[test]
    fn window_picker_uses_niri_ipc_and_requested_targets() {
        assert!(matches!(
            pick_reply(r#"{"Ok":{"PickedWindow":{"id":42}}}"#).unwrap(),
            Some(PickerChoice::Window(42))
        ));
        assert!(pick_reply(r#"{"Ok":{"PickedWindow":{"id":99}}}"#)
            .unwrap()
            .is_none());
    }

    #[test]
    fn window_picker_handles_cancellation_and_errors() {
        assert!(pick_reply(r#"{"Ok":{"PickedWindow":null}}"#)
            .unwrap()
            .is_none());
        assert!(pick_reply(r#"{"Err":"picker already active"}"#).is_err());
        assert!(pick_reply(r#"{"Ok":{"PickedWindow":{}}}"#).is_err());
        assert!(pick_reply("not json").is_err());
    }
}
