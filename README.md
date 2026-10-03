# niri-screenshare

An XDG Desktop Portal ScreenCast backend for niri, with GTK4 display/window selection and PipeWire integration.

[![CI](https://github.com/pantarune/niri-screenshare/actions/workflows/ci.yml/badge.svg)](https://github.com/pantarune/niri-screenshare/actions/workflows/ci.yml)
[![AUR version](https://img.shields.io/aur/version/niri-screenshare)](https://aur.archlinux.org/packages/niri-screenshare)
[![License: GPL-3.0](https://img.shields.io/github/license/pantarune/niri-screenshare)](LICENSE)

It implements `org.freedesktop.impl.portal.ScreenCast` and can replace
`xdg-desktop-portal-gnome` for screen sharing while keeping other portal
interfaces on the user's existing backends.

## install

### arch

```sh
paru -S niri-screenshare
```

### other

The default picker build requires the `gtk4` and `libadwaita` system packages.
Build without the picker via `cargo build --release --no-default-features`.

Build the native Wayland/Niri picker without GTK or libadwaita:

```sh
cargo build --release --no-default-features --features native-picker
# Or with Nix:
nix build .#native-picker
```

The `picker` and `native-picker` features are mutually exclusive. Enabling both
produces a compile-time error. Neither feature is required for a pickerless build.
The Nix package's `withPicker` option accepts `true` or `"gtk"` for GTK,
`"native"` for the native picker, and `false` for no picker.

```sh
git clone https://github.com/pantarune/niri-screenshare
cd niri-screenshare
cargo build --release
sudo cp target/release/niri-screenshare /usr/lib/
sudo cp data/niri.portal /usr/share/xdg-desktop-portal/portals/
sudo cp data/org.freedesktop.impl.portal.desktop.niri.service /usr/share/dbus-1/services/
cp data/niri-screenshare.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now niri-screenshare.service
```

No manual config is normally needed. On first service start the backend adds only
`org.freedesktop.impl.portal.ScreenCast=niri` to the user's portal preferences;
it does not replace the default backend for unrelated portal interfaces.

## Home Manager

Import the flake module and enable it:

```nix
{
  imports = [ inputs.niri-screenshare.homeModules.default ];

  services.niri-screenshare = {
    enable = true;
    package = "native"; # Default; use "gtk" for the GTK picker.
    settings.native_picker.style.hover_background = "#3a5068";
  };
}
```

The module installs the package and its portal/D-Bus metadata, manages the user
service, and selects `niri` for Niri's ScreenCast portal. Remove any existing
explicit `xdg.portal.config.niri."org.freedesktop.impl.portal.ScreenCast"`
assignment to another backend, such as `"gnome"`, to avoid conflicting definitions.
Other portal backends and preferences can remain in your Home Manager config.

`package` also accepts a derivation, including a package override:

```nix
services.niri-screenshare.package =
  inputs.niri-screenshare.packages.${pkgs.stdenv.hostPlatform.system}.default.override {
    withPicker = "native";
  };
```

Nonempty `settings` are written as TOML to `$XDG_CONFIG_HOME/niri-screenshare/config.toml`.
The module is also exported as `homeModules.niri-screenshare` and
`homeManagerModules.default`.

## Cachix

Use the public binary cache to download prebuilt packages:

```sh
cachix use jcdickinson-niri-screenshare
```

Or configure Nix directly:

```nix
nix.settings = {
  extra-substituters = [ "https://jcdickinson-niri-screenshare.cachix.org" ];
  extra-trusted-public-keys = [
    "jcdickinson-niri-screenshare.cachix.org-1:6a3GUuvwF77lKjdqBxShB34qomCUIsMe9PFq6P8H5S8="
  ];
};
```

The separate `Cachix` workflow builds and uploads the GTK and native Nix packages
on pushes or manual runs. Set the repository variable `CACHIX_CACHE` to your cache
name and the Actions secret `CACHIX_AUTH_TOKEN` to a write token. If either is
unset, the workflow skips its work. Self-signed caches can also provide the
optional `CACHIX_SIGNING_KEY` secret.

Uploads are limited to the two package outputs and their runtime dependencies;
build dependencies and intermediate build outputs are not uploaded automatically.

## behavior

**picker mode (default)** — a GTK4 dialog with Displays / Windows tabs appears
when an app requests screen sharing (OBS, Discord, Firefox, etc.). The requested
XDG source types are respected, and the user must explicitly approve sharing
even when only one capture target is available.

Set `NIRI_SCREENSHARE_NO_PICKER=1` to skip the dialog. Pickerless mode currently
auto-selects the focused output and therefore supports monitor capture only; it
does **not** claim to implement niri's synthetic Dynamic Cast Target.

| build | behavior |
|-------|----------|
| `default` | GTK4 picker dialog for requested displays/windows |
| `--no-default-features --features native-picker` | Wayland monitor icons and Niri click-to-select windows |
| `--no-default-features` | pickerless focused-output capture |

**Native picker:** a monitor icon appears near the top of each available monitor.
Click it to share that monitor. When window sharing is also allowed, the adjacent
window icon starts Niri's native window picker; click the window to share.
Window-only requests open Niri's picker directly. The X button cancels the popup.
Escape cancels either picker;
right-click also cancels the monitor overlay. Overlays close before capture starts.
The overlay uses layer-shell and shared-memory drawing, with no widget toolkit.

If the requesting app cancels while Niri's window picker is active, capture is
cancelled but the crosshair can remain until you press Escape. Niri currently
has no IPC request to cancel `PickWindow`, and disconnecting its socket does
not cancel the compositor's input grab.

### native picker theme

Buttons show hover and pressed states. A click activates only if it is released
on the same button where it started; releasing outside cancels that click.

The native picker merges `niri-screenshare/config.toml` files in this order:

1. Built-in defaults.
2. Directories in `$XDG_CONFIG_DIRS` (default `/etc/xdg`), from last to first.
3. `$XDG_CONFIG_HOME` (default `$HOME/.config`).

Earlier entries in `$XDG_CONFIG_DIRS` have higher priority, and the user config
has the highest priority. Empty environment variables use their defaults;
relative XDG paths are ignored. Each file overrides only the values it specifies,
using `serde-toml-merge`. Missing files are normal. Unreadable or invalid files
emit a warning and are skipped, retaining the last valid configuration.
Config is read each time the picker opens.

Example `~/.config/niri-screenshare/config.toml` (all values shown are defaults):

```toml
[native_picker.style]
background = "#243344"
hover_background = "#3a5068"
pressed_background = "#172433"
foreground = "#eeeeee"
hover_foreground = "#ffffff"
pressed_foreground = "#eeeeee"
separator = "#667788"
cancel_background = "#663344"
cancel_hover_background = "#884455"
cancel_pressed_background = "#442233"
```

Colors use `#RRGGBB`. These settings style the native Wayland popup; Niri draws
its own window-selection crosshair. Theme loading and TOML dependencies are
only compiled with `native-picker`.

### env vars

| variable | effect |
|----------|--------|
| `NIRI_SCREENSHARE_NO_PICKER=1` | skip picker and auto-select the focused output |
| `NIRI_BIN=/path/to/niri` | override niri binary path (for NixOS etc.) |
| `NIRI_SOCKET=/path/to/socket` | explicitly select the niri IPC socket |

If more than one niri IPC socket exists and none can be matched to
`WAYLAND_DISPLAY`, the backend refuses to guess; set `NIRI_SOCKET` explicitly.

### debug

Open the picker dialog standalone without starting a portal session:

```sh
niri-screenshare --debug-picker
```

Run basic environment checks:

```sh
niri-screenshare check
```

## configuration

The portal daemon reads `$XDG_CONFIG_HOME/xdg-desktop-portal/portals.conf`
(or `~/.config/xdg-desktop-portal/portals.conf` when `XDG_CONFIG_HOME` is unset)
to decide which backend to use. niri-screenshare adds its ScreenCast preference
without changing unrelated defaults. Override it by editing that file before
starting the service.

## how it works

```text
app → xdg-desktop-portal → niri-screenshare → Mutter.ScreenCast → PipeWire
```

1. an app calls `CreateSession` and `SelectSources` on the portal frontend
2. xdg-desktop-portal forwards the request to niri-screenshare
3. niri-screenshare filters targets to the requested source types and asks for consent
4. the app calls `Start`; niri-screenshare asks niri to start the selected stream
5. the PipeWire node id is returned to xdg-desktop-portal
6. xdg-desktop-portal exposes the appropriate PipeWire remote to the app

Failed starts are cleaned up and may be retried; closing a portal session also
stops the corresponding compositor screencast session.

## dependencies

- **runtime:** `xdg-desktop-portal`, `pipewire`, `niri`, `gtk4`, `libadwaita`
- **build:** `cargo`, `gtk4`, `libadwaita`

GTK4/libadwaita are only required by the `picker` feature. The `native-picker`
feature uses Rust Wayland protocol bindings and requires Niri's `PickWindow` IPC,
layer-shell, and Wayland output names (`wl_output` version 4).

## troubleshooting

**picker doesn't appear** — make sure you built with default features
(`cargo build --release`) and the service has `NIRI_SCREENSHARE_NO_PICKER`
unset. Check the service log:

```sh
journalctl --user -u niri-screenshare -n 20
```

**obs/discord shows "no capture sources"** — verify the portal backend is
registered:

```sh
busctl list | grep niri
```

If nothing shows, restart the service:

```sh
systemctl --user restart niri-screenshare
```

**portal daemon crashes on screenshare** — some `xdg-desktop-portal` 1.22.1
builds have a bug in session initialization. Upgrading or reinstalling usually
fixes it.

## credits

- [Ly-sec](https://github.com/Ly-sec) — GTK4 picker, cancel fix, NixOS packaging
