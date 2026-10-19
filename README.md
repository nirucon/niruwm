# NIRUWM

Small Rust/X11 tiling window manager with per-monitor workspaces, seven layouts,
configurable shortcuts and four NIRU themes. Personal software for the author's
machines, public for others to try.

## Install

For a standalone desktop on **Void glibc, Arch or Debian**, use
[NIRUX](https://github.com/nirucon/nirux#install): it installs dependencies,
[nirubar](https://github.com/nirucon/nirubar),
[niruscreensaver](https://github.com/nirucon/niruscreensaver) and
[nirulock](https://github.com/nirucon/nirulock), independently of NIRUPI.
Current release binaries target x86_64/glibc.

To install the prebuilt WM/session and its pinned bar on an existing X11 system:

```sh
git clone https://github.com/nirucon/niruwm
cd niruwm
./install.sh --register-session
```

Choose **NIRUWM** at login. `./install.sh --build` builds only the WM, niructl and
niruidle (Rust 1.75+, C linker); install the independent bar separately if wanted.
Existing NIRUX-owned installations are maintained with `nirux update`.

## Essential controls

| Keys | Action |
|---|---|
| Super+Enter / Super+B / Super+P | Terminal / browser / application launcher |
| Super+1–9 / Super+Shift+1–9 | Select workspace / move window |
| Super+J / Super+K | Focus next / previous |
| Super+T / Super+M | Tile / monocle |
| Super+Shift+E / Super+Shift+S | Fullscreen / scratchpad |
| Super+, / Super+. | Previous / next monitor |
| Super+Escape / Super+R | Lock / reload |
| Super+Q / Super+Ctrl+Shift+Q | Close window / log out |

Settings: `~/.config/niruwm/config.toml` (XDG respected), shared with the bar.
See [config.example.toml](config.example.toml) for layouts, rules, monitors,
wallpaper and keybindings. Individual `[[keys]]` entries replace that key only;
`action = "unbind"` releases a default shortcut.

```sh
niruwm --check-config
niruwm --dump-config
niruwm --doctor
niructl reload
niructl restart-bar
niructl themes
niructl theme niru-nord
```

Themes: `niru-noir` (default), `niru-nord`, `niru-satie`, `niru-carl-larsson`.
Custom themes: `~/.config/niruwm/themes/NAME.toml`. Session options:
`~/.config/niruwm/session.conf`; idle timings: `~/.config/niruui/screensaver.toml`.

**0.16.5:** consistent partial key overrides/unbinding, event-loop child reaping,
full-monitor fullscreen, idle wallpaper rotation and border updates without a bar.
Multi-monitor WM/bar protocol, session restoration and suite-wide theme propagation
still have tracked work in the [roadmap](https://github.com/nirucon/nirux/blob/main/ROADMAP.md).

[Audit](https://github.com/nirucon/nirux/blob/main/AUDIT.md) ·
[Hardware/session notes](docs/BARE-METAL.md) · [X11 regressions](tests/x11.rs)

MIT. Copyright 2026 Nicklas Rudolfsson.
