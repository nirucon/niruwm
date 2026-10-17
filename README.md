# NIRUWM

An independently written, minimal Rust/X11 tiling window manager for the NIRU desktop.
Companion bar: [NIRUBAR](https://github.com/nirucon/nirubar).

**0.1.0-alpha.1 — an experimental foundation, not yet a replacement for the
complete patched NIRUPI/dwm desktop.** Existing dwm installation and configuration
are not modified. Start in a nested X server first.

## Implemented

- Nine dwm-style workspaces, window assignment and keyboard focus navigation.
- Right/left master-stack, monocle, centered master, fibonacci and floating layouts.
- Adjustable master ratio, smart gaps/borders and keyboard promotion to master.
- Floating transient/dialog windows; client-requested floating geometry.
- Fullscreen requests, close protocol and active-window requests through X11/EWMH.
- Safe tag hiding with delayed-UnmapNotify bookkeeping (covered by burst-switch tests).
- `niructl` commands, shared TOML configuration and runtime reload.
- Niru Noir by default, file-based custom themes, optional separate NIRUBAR process.
- Refuses to take over an existing window manager's session.

## Install and try

Install NIRUBAR first, then:

```sh
git clone https://github.com/nirucon/niruwm.git
cd niruwm
./install.sh
~/.local/bin/niruwm --check-config
```

The installer builds the locked release, installs under `~/.local/bin`, backs up
existing binaries, and creates configuration only when absent. It does not edit
`.xinitrc`, change the current desktop or automatically switch sessions.

For a nested test (Xephyr is `xserver-xephyr` on Debian):

```sh
Xephyr :2 -screen 1280x800 -ac -noreset &
DISPLAY=:2 ~/.local/bin/niruwm-session
```

For a separate display-manager entry, run `./install.sh --register-session`.
That explicit option uses sudo only to install `/usr/share/xsessions/niruwm.desktop`.
Keep your dwm entry and select **NIRUWM (alpha)** manually when ready to test.
This session currently launches the WM and bar only; it does not provision NIRUPI's
idle/lock, notification, compositor or power services.

## Keys

| Keys | Action |
| --- | --- |
| Super+Enter | Kitty |
| Super+P / Super+Shift+P | Launcher |
| Super+1..9 | Show workspace |
| Super+Shift+1..9 | Assign focused window |
| Super+J/K or Super+Tab | Next/previous window |
| Super+T / Y | Right/left stack |
| Super+M / C | Monocle / centered master |
| Super+Ctrl+E / Super+F | Fibonacci / floating layout |
| Super+H/L | Change master ratio |
| Super+Shift+Space | Toggle focused window floating |
| Super+Shift+Z | Promote focused window to master |
| Super+Shift+E | Toggle fullscreen |
| Super+Q | Close focused window |
| Super+R | Reload configuration and WM theme |
| Super+Ctrl+Shift+Q | Exit the test session |

Use `niructl reload` to reload both the WM and bar. Caps Lock and Num Lock are
accounted for in key grabs. Swedish-layout letters are used for the main actions.

```sh
niructl status
niructl tag 3
niructl layout centered
niructl theme niru-noir
niructl reload
niructl quit
```

## Explicit alpha limits and next milestones

The current layout uses the X screen as one rectangle. **Independent physical
monitor geometry/tags and hotplug are the next compatibility milestone**; avoid
using this alpha as your daily multi-monitor session. Runtime application rules,
swallow, scratchpad, sticky windows, mouse drag/resize, saved layout state, tray,
and full NIRUPI keybinding parity are still pending. ICCCM/EWMH coverage is partial;
Java, games, unusual focus models and input methods need dedicated testing.

The intended progression is: monitor model and hotplug → rules and saved state →
swallow/scratchpad/sticky → NIRUPI integration → measured performance and daily-use
qualification. Never replace the working dwm session before those gates pass.

## Build requirements

Rust/Cargo 1.75 or newer, a C linker, and an X11 session. No systemd dependency.
The bar needs fontconfig plus a TrueType font (JetBrainsMono Nerd Font is preferred,
DejaVu Sans Mono works). This first renderer targets 24/32-bit TrueColor screens.

```sh
# Arch
sudo pacman -S --needed rust base-devel fontconfig ttf-dejavu kitty rofi
# Debian
sudo apt install cargo rustc build-essential fontconfig fonts-dejavu-core kitty rofi
# Void (glibc)
sudo xbps-install -S rust cargo base-devel fontconfig dejavu-fonts-ttf kitty rofi
```

Void musl is not yet a tested target. CI builds on Debian, Arch and Void glibc;
these builds do not replace real hardware/session testing.

## Configuration and themes

Both programs read `~/.config/niru/config.toml` (or `$XDG_CONFIG_HOME/niru/config.toml`).
See `config.example.toml`. Commands are argument arrays, never passed through a shell.
**Niru Noir is the built-in default**, using NIRUPI's original palette:
`0f0f10`, `e5e5e5`, `a8a8a8`, `2a2a2d`, `5a5a60`.

Additional themes are files in `~/.config/niru/themes/NAME.toml`, following
`themes/niru-noir.toml`. Set `theme = "NAME"` and run `niructl reload`, or use
`niructl theme NAME`. This rewrites the TOML configuration while preserving values
(comments are not preserved). Invalid configurations/themes are rejected.
The working runtime configuration is retained when reload fails.

`font_size` and optional `font_path` control the bar's Unicode TrueType renderer.
Changing themes does not modify GTK, terminals or other applications yet.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
# Isolated X11 tests (install xvfb and xauth first):
xvfb-run -a cargo test --locked --test x11 -- --ignored --test-threads=1
```

Initial local verification: Rust 1.75, unit tests and real Xvfb integration tests.
No performance advantage over dwm is claimed without measurements.

MIT. Copyright 2026 Nicklas Rudolfsson.
