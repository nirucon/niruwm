# NIRUWM

**Part of [NIRUX - A minimal X11 desktop by Nicklas Rudolfsson](https://github.com/nirucon/nirux).**

**Early beta.** A small Rust/X11 tiling window manager and session layer for the
NIRU desktop, inspired by suckless dwm's keyboard-first, config-as-text model. It provides per-monitor workspaces, seven layouts, configurable
shortcuts, floating and fullscreen windows, a scratchpad, session restore,
themes and the `niructl`/`niruidle` tools.

## Features

- Seven layouts: tile, left, monocle, centered, centered-floating, fibonacci and
  floating.
- Ten workspaces per monitor, selected with `Super+1`…`Super+0`.
- Tiling, floating, fullscreen, scratchpad, master ratio, master count, gaps and
  window swapping.
- dwm-style mouse move/resize with **Super+left-drag** and **Super+right-drag**.
- Session save/restore, autostart, workspace recipes and focus/presentation modes
  through `niructl`.
- Native notification, DND, audio, microphone, Bluetooth, display-profile, power-
  profile, brightness, night-light and screenshot integration.
- Shared NIRU themes, font metrics and appearance contract.
- `niructl`: provider-neutral controls, diagnostics, work modes and recipes.
- `niruidle`: idle, lock and DPMS timing without a separate idle daemon.

## Install

For the complete desktop and OS dependencies, use [NIRUX](https://github.com/nirucon/nirux#install).
To install NIRUWM and its session tools on an existing X11 system:

```sh
git clone https://github.com/nirucon/niruwm
cd niruwm
./install.sh --register-session
```

Choose **NIRUWM** at login. `./install.sh --build` needs Rust 1.75+ and a C
linker. The bar is a separate component; NIRUX installs it. NIRUX-owned
installations are maintained with `nirux update`.

## Keybindings

### Applications and session

| Keys | Action |
|---|---|
| Super+Enter | Terminal |
| Super+Shift+Enter | Alternative terminal |
| Super+B | Browser |
| Super+P | Application launcher |
| Super+Shift+P | Applications, windows and actions |
| Super+Shift+M | Unified launcher |
| Super+W | Window switcher |
| Super+Shift+F | File manager |
| Super+Escape | Lock |
| Super+Shift+R | Reload |
| Super+R / Super+Ctrl+Shift+R | Reload |
| Super+Shift+D | Toggle DND |
| Super+Shift+C | Toggle idle inhibition |
| Super+Shift+W | Save session |
| Super+G | Toggle gaps |
| Super+N | Toggle night light |
| Super+Q | Close window |
| Super+Ctrl+Shift+Q | Log out |

### Layouts

| Keys | Action |
|---|---|
| Super+T | Tile |
| Super+Y | Left stack |
| Super+M | Monocle |
| Super+C | Centered |
| Super+U | Centered floating master |
| Super+F | Floating layout |
| Super+Ctrl+E | Fibonacci |
| Super+Ctrl+, / Super+Ctrl+. | Previous / next layout |
| Super+Shift+Space | Toggle floating |
| Super+Shift+E | Toggle fullscreen |
| Super+Shift+S | Scratchpad |
| Super+H / Super+L | Decrease / increase master ratio |
| Super+I / Super+D | Increase / decrease master count |
| Super+Shift+J / Super+Shift+K | Swap with next / previous window |

### Focus and windows

| Keys | Action |
|---|---|
| Super+J / Super+K | Next / previous window |
| Super+Right or Down | Next window |
| Super+Left or Up | Previous window |
| Super+Shift+Right or Down | Move window next |
| Super+Shift+Left or Up | Move window previous |
| Super+, / Super+. | Previous / next monitor |
| Super+Shift+, / Super+Shift+. | Move window to previous / next monitor |

### Workspaces

| Keys | Action |
|---|---|
| Super+1…9 | Select workspace 1…9 |
| Super+0 | Select workspace 10 |
| Super+Shift+1…9 | Move window to workspace 1…9 |
| Super+Shift+0 | Move window to workspace 10 |
| Super+Ctrl+1…9 / 0 | Toggle workspace 1…9 / 10 on the current view |
| Super+Ctrl+Shift+1…9 / 0 | Toggle window on workspace 1…9 / 10 and move it there |
| Super+Tab / Super+Shift+Tab | Next / previous workspace |

### Floating windows

| Keys | Action |
|---|---|
| Super+Ctrl+Left / Right / Up / Down | Move floating window |
| Super+Ctrl+Shift+Left / Right | Resize floating window horizontally |
| Super+Ctrl+Shift+Up / Down | Resize floating window vertically |

### Screenshots and hardware keys

| Keys | Action |
|---|---|
| Super+Print | Full-screen capture and review |
| Print | Region capture and review |
| Super+Shift+Print | Active-window capture and review |
| XF86AudioRaiseVolume / LowerVolume | Volume up / down |
| XF86AudioMute | Mute |
| XF86AudioPlay / Pause | Play / pause |
| XF86AudioNext / Prev | Next / previous track |
| XF86MonBrightnessUp / Down | Brightness up / down |

### Mouse

| Input | Action |
|---|---|
| Super+left-drag | Move window; a tiled window becomes floating |
| Super+right-drag | Resize from the nearest corner |
| Click title/bar module | Normal application interaction; no Super required |


## Settings

Shared settings live in `~/.config/niruwm/config.toml`; start from
[config.example.toml](config.example.toml). XDG paths are respected. The default
workspace labels are numbers 1–10. To use icons or names, edit the `[bar] tags`
line using the commented icon/text presets in the example; key bindings stay the
same. Individual
bindings override only that key; `action = "unbind"` removes a default. Set
`browser` to an installed browser command.

```sh
niruwm --check-config
niruwm --reset-config   # restore the full example; keeps .niruwm-backup
niructl reload
niructl restart-bar
niructl theme niru-nord
```

Session options live in `~/.config/niruwm/session.conf`. Screensaver and lock
timing live in the shared config under `[idle]`:

```toml
[idle]
screensaver_timeout = 15      # idle minutes before the screensaver
lock_after_screensaver = 15   # screensaver minutes before locking
dpms_off_after_lock = 15      # minutes after locking before the display sleeps
```

`niruidle` owns both timers in one process, so the ladder
active -> screensaver -> locked has a single timer on each step. A legacy
`~/.config/niruui/screensaver.toml` is still read for any value `[idle]` does
not set, so an older installation keeps its timing. Notifications use
NIRUNOTIFY when installed, with Dunst fallback.

For a TTY login, run `startx "$HOME/.local/bin/niruwm-session"` (or plain
`startx` when `.xinitrc` selects NIRUWM). The session keeps a valid login-provided
`XDG_RUNTIME_DIR`; without one it uses a private, mode-700 directory under
`${XDG_CACHE_HOME:-$HOME/.cache}/niruwm/runtime` before starting D-Bus/audio.
Startup errors and WM errors are recorded in
`${XDG_STATE_HOME:-$HOME/.local/state}/niruwm/session.log`.

### Theme wallpapers

`niructl wallpaper status` shows the configured theme, resolved theme image and
whether wallpaper following is disabled, overridden by a single file, or missing
an asset. Image setter failures are reported in the session log. NIRUX installs
`feh` and the theme images; localized XDG Pictures paths are supported.

A config without a `[wallpaper]` section uses the built-in `enabled = false`
default, so theme wallpapers never change while the bar still follows the theme.
NIRUWM says so in the session log, and `niructl theme NAME` repairs such a file:
missing sections are restored from `config.example.toml`, the previous file is
kept as `config.toml.bak`, and a file that is not valid TOML is refused instead
of overwritten. The repair works without an X server, so it also applies from a
TTY or over ssh.

Selecting a file with `niructl wallpaper set PATH` keeps that file across theme
changes. To explicitly return to theme wallpapers:

```sh
niructl wallpaper follow-theme on
niructl theme niru-nord
```

This enables wallpapers and changes a single-file override to fixed mode; the
manual file remains in the config for `niructl wallpaper mode single`.

`niructl` provides provider-neutral controls for audio, microphone, Bluetooth,
display profiles, power profiles, DND, diagnostics, work modes and workspace
recipes:

```sh
niructl audio outputs
niructl audio mic-status
niructl bluetooth status
niructl display preview Desk 15
niructl powerprofile set balanced
niructl explain
niructl mode focus
niructl recipe run Development
```

Shutdown, reboot and suspend use the shared logind/elogind login1 D-Bus API
through `dbus-send`, with normal interactive authorization and inhibitor
checks. Systemd's `loginctl` does not implement elogind's power commands.
Without `dbus-send`, power actions use `systemctl` or a capability-checked
elogind `loginctl`; rejected requests are never retried through another
backend. Logout remains a `loginctl` session operation, and suspend still
requires verified screen-lock readiness.

Compositor: `niructl compositor status|on|off|toggle` controls picom. Inactive
window transparency and shadows are configured in `~/.config/picom.conf`; the
toggle does not rewrite that file. A conservative starting point is
`inactive-opacity = 0.94`, `active-opacity = 1.0` and a small shadow radius.
Floating windows can opt into centered placement with a `[[rules]]` entry:
`class = "localsend"`, `floating = true`, `center = true`.

Themes: Niru Noir, Nord, Satie, Carl Larsson, Forest Vigil, Blood of Urdr,
Root and Stone, Blue Horizon, The Norns, Winter Nights and custom TOML files. On a theme
change or reload, `nirux-theme-hook` from `~/.local/bin` or `PATH` runs when
present; NIRUX uses it to update terminal colours.

## NIRUX suite

[NIRUX](https://github.com/nirucon/nirux) · [NIRUWM](https://github.com/nirucon/niruwm) · [NIRUBAR](https://github.com/nirucon/nirubar) · [NIRUSCREENSAVER](https://github.com/nirucon/niruscreensaver) · [NIRULOCK](https://github.com/nirucon/nirulock) · [NIRULAUNCHER](https://github.com/nirucon/nirulauncher) · [NIRUNOTIFY](https://github.com/nirucon/nirunotify) · [NIRUSHOT](https://github.com/nirucon/nirushot)

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

MIT. Copyright 2026 Nicklas Rudolfsson.
