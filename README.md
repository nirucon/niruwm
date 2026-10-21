# NIRUWM

Small Rust/X11 tiling window manager for the NIRU X11 desktop, with
per-monitor workspaces, seven layouts, configurable shortcuts, themes and the
`niructl`/`niruidle` session tools.

Part of the [NIRUX suite](https://github.com/nirucon/nirux): [NIRUWM](https://github.com/nirucon/niruwm) · [NIRUBAR](https://github.com/nirucon/nirubar) · [NIRUSCREENSAVER](https://github.com/nirucon/niruscreensaver) · [NIRULOCK](https://github.com/nirucon/nirulock) · [NIRULAUNCHER](https://github.com/nirucon/nirulauncher) · [NIRUNOTIFY](https://github.com/nirucon/nirunotify) · [NIRUSHOT](https://github.com/nirucon/nirushot).

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

## Controls

| Keys | Action |
|---|---|
| Super+Enter / Super+B | Terminal / browser |
| Super+P / Super+Shift+P | Applications / applications and actions |
| Super+1–9 / Super+Shift+1–9 | Select workspace / move window |
| Super+J / Super+K | Next / previous window |
| Super+T / Super+M | Tile / monocle |
| Super+Shift+E / Super+Shift+S | Fullscreen / scratchpad |
| Super+, / Super+. | Previous / next monitor |
| Super+N / brightness keys | Night light / brightness |
| Super+Escape / Super+R | Lock / reload |
| Super+Q / Super+Ctrl+Shift+Q | Close window / log out |
| Super+Print / Print / Super+Shift+Print | Full / region / window screenshot |

## Settings

Shared settings live in `~/.config/niruwm/config.toml`; start from
[config.example.toml](config.example.toml). XDG paths are respected. Individual
bindings override only that key; `action = "unbind"` removes a default.
`nirupi_compat = true` explicitly enables NIRUPI helpers. Set `browser` to an
installed browser command.

```sh
niruwm --check-config
niructl reload
niructl restart-bar
niructl theme niru-nord
```

Session options live in `~/.config/niruwm/session.conf`. Idle timing lives in
`~/.config/niruui/screensaver.toml`. Notifications use NIRUNOTIFY when installed,
with Dunst fallback.

`niructl` provides provider-neutral controls for audio, microphone, Bluetooth,
display profiles, power profiles, DND, diagnostics, work modes and workspace
recipes. Examples:

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

Themes: Niru Noir, Nord, Satie, Carl Larsson and custom TOML files. On a theme
change or reload, `nirux-theme-hook` from `~/.local/bin` or `PATH` runs when
present; NIRUX uses it to update terminal colours.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

MIT. Copyright 2026 Nicklas Rudolfsson.
