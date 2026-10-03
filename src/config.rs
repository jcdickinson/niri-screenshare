use std::ffi::OsStr;
use std::path::PathBuf;

use serde::{de::DeserializeOwned, Serialize};

/// Merge system and user overrides without replacing a valid config with a bad file.
pub fn load<T: Serialize + DeserializeOwned>(defaults: T) -> T {
    let paths = config_paths(
        std::env::var_os("XDG_CONFIG_DIRS").as_deref(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    );
    load_paths(defaults, &paths)
}

fn config_paths(
    config_dirs: Option<&OsStr>,
    config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Vec<PathBuf> {
    let dirs = config_dirs
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsStr::new("/etc/xdg"));
    let mut paths: Vec<_> = std::env::split_paths(dirs)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("niri-screenshare/config.toml"))
        .collect();
    // XDG_CONFIG_DIRS is most-important-first; apply lower priorities first.
    paths.reverse();
    let user_dir = config_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".config"))
        });
    if let Some(dir) = user_dir {
        paths.push(dir.join("niri-screenshare/config.toml"));
    }
    paths
}

fn load_paths<T: Serialize + DeserializeOwned>(mut current: T, paths: &[PathBuf]) -> T {
    let mut merged = match toml::Value::try_from(&current) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("could not serialize config defaults: {error}");
            return current;
        }
    };
    for path in paths {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!("could not read config {}: {error}", path.display());
                continue;
            }
        };
        let candidate = (|| -> anyhow::Result<(toml::Value, T)> {
            let value = toml::from_str(&text)?;
            let value = serde_toml_merge::merge(merged.clone(), value)?;
            let config = value.clone().try_into()?;
            Ok((value, config))
        })();
        match candidate {
            Ok((value, config)) => {
                merged = value;
                current = config;
            }
            Err(error) => tracing::warn!("ignoring invalid config {}: {error:#}", path.display()),
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_paths_follow_priority_and_defaults() {
        assert_eq!(
            config_paths(None, None, Some(OsStr::new("/home/test"))),
            vec![
                PathBuf::from("/etc/xdg/niri-screenshare/config.toml"),
                PathBuf::from("/home/test/.config/niri-screenshare/config.toml"),
            ]
        );
        assert_eq!(
            config_paths(
                Some(OsStr::new("/first:relative::/last")),
                Some(OsStr::new("/user")),
                Some(OsStr::new("/home/test"))
            ),
            vec![
                PathBuf::from("/last/niri-screenshare/config.toml"),
                PathBuf::from("/first/niri-screenshare/config.toml"),
                PathBuf::from("/user/niri-screenshare/config.toml"),
            ]
        );
    }

    #[test]
    fn empty_and_relative_user_paths_fall_back_to_home() {
        for user_dir in ["", "relative"] {
            assert_eq!(
                config_paths(
                    Some(OsStr::new("")),
                    Some(OsStr::new(user_dir)),
                    Some(OsStr::new("/home/test"))
                ),
                config_paths(None, None, Some(OsStr::new("/home/test")))
            );
        }
        assert_eq!(
            config_paths(None, None, None),
            vec![PathBuf::from("/etc/xdg/niri-screenshare/config.toml")]
        );
    }

    #[test]
    fn overrides_merge_nested_keys_and_bad_files_are_nonfatal() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = [
            "system",
            "bad-syntax",
            "bad-type",
            "user",
            "directory",
            "missing",
        ]
        .into_iter()
        .map(|name| dir.path().join(name))
        .collect();
        std::fs::write(&paths[0], "[native_picker.style]\nbackground = '#111111'\n").unwrap();
        std::fs::write(&paths[1], "invalid [").unwrap();
        std::fs::write(&paths[2], "[native_picker.style]\nforeground = 123\n").unwrap();
        std::fs::write(&paths[3], "[native_picker.style]\nbackground = '#222222'\n").unwrap();
        std::fs::create_dir(&paths[4]).unwrap();
        // Generic data keeps this module independent of picker-specific types.
        let defaults: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
        > = toml::from_str(
            "[native_picker.style]\nbackground = '#000000'\nforeground = '#ffffff'\n",
        )
        .unwrap();
        let config = load_paths(defaults, &paths);
        assert_eq!(config["native_picker"]["style"]["background"], "#222222");
        assert_eq!(config["native_picker"]["style"]["foreground"], "#ffffff");
    }
}
