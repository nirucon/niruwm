# NIRUWM

Small Rust/X11 tiling window manager with per-monitor workspaces, seven layouts,
configurable shortcuts and four NIRU themes. Supports **Void glibc, Debian and
Arch Linux**; release binaries target x86_64/glibc.

## Install

Use [NIRUX](https://github.com/nirucon/nirux#install) for the complete desktop
and OS dependencies. On an existing X11 system, install the WM/session and pinned bar:

```sh
git clone https://github.com/nirucon/niruwm
cd niruwm
./install.sh --register-session
```

Choose **NIRUWM** at login. `./install.sh --build` requires Rust 1.75+ and a C
linker; it builds WM, niructl and niruidle. Install the independent bar separately.
Maintain NIRUX-owned installations with `nirux update`.

## Controls and settings

| Keys | Action |
|---|---|
| Super+Enter / Super+B | Terminal / configured browser |
| Super+P / Super+Shift+P | Apps / apps and desktop actions |
| Super+1–9 / Super+Shift+1–9 | Select workspace / move window |
| Super+J / Super+K | Next / previous window |
| Super+T / Super+M | Tile / monocle |
| Super+Shift+E / Super+Shift+S | Fullscreen / scratchpad |
| Super+, / Super+. | Previous / next monitor |
| Brightness keys / Super+N | Brightness / night light |
| Super+Escape / Super+R | Lock / reload |
| Super+Q / Super+Ctrl+Shift+Q | Close window / log out |

Shared settings: `~/.config/niruwm/config.toml` (XDG respected).
See [config.example.toml](config.example.toml). Individual bindings override only
that key; `action = "unbind"` releases a default. `nirupi_compat = true` explicitly
enables NIRUPI helpers. Set `browser` to an installed browser.

```sh
niruwm --check-config
niructl reload
niructl restart-bar
niructl theme niru-nord
```

Session options: `~/.config/niruwm/session.conf`, including
`WELCOME_NOTIFICATION=always|once|off`; idle timing:
`~/.config/niruui/screensaver.toml`. Notifications default to native when installed,
with Dunst fallback. `niructl dnd on|off|toggle|status` controls whichever provider
is running, so the launcher, keyboard and bar can share one command.
`niructl audio outputs|sources|output ID|next|source ID|next|mute|mic-mute|volume up|down`
provides the same provider-neutral control for PipeWire/WirePlumber. Themes: Noir, Nord, Satie, Carl Larsson and custom TOML files.
On a theme change or reload, `nirux-theme-hook` from `~/.local/bin` or `PATH` is
run when present. NIRUX uses it to update kitty/fish colours; NIRUWM itself does
not depend on it.

[Roadmap](https://github.com/nirucon/nirux/blob/main/ROADMAP.md) ·
[Hardware/session notes](docs/BARE-METAL.md) · [X11 tests](tests/x11.rs)

MIT. Copyright 2026 Nicklas Rudolfsson.
