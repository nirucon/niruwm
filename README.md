# NIRUWM

A small X11 tiling window manager in Rust, with [NIRUBAR](https://github.com/nirucon/nirubar)
as its status bar.

**A personal hobby project.** I build this for my own machines and my own
workflow. The keybindings, the theme and the session are tuned for my setup
(the NIRUPI desktop). It is public so others can read it and try it, but there
is no support promise. Treat it as alpha software and use it at your own risk.

## What it is

- dwm-style workspaces (nine), layouts, gaps and keybindings.
- Niru Noir colours: dark background, light text, no decoration.
- A separate bar process drawing clickable workspaces, the Swedish date and
  `hh:mm`.
- X11 only. One screen for now.

## How it works

A session script starts the bar, then the window manager, then your
applications. It works standalone, but reuses NIRUPI helper scripts
(`nirupi-lock.sh`, `nirupi-battery.sh`, ...) when they exist. Logging out
stops everything it started.

Both programs share one config file, `~/.config/niruwm/config.toml`, so
installing or removing either one is safe.

## Install

You need an X11 session and a TrueType monospace font. No Rust or compiler is
required: the installer downloads static binaries.

```sh
git clone https://github.com/nirucon/niruwm
cd niruwm
./install.sh --register-session   # adds NIRUWM to SDDM (uses sudo for that file)
```

Log out, pick **NIRUWM** in SDDM, and log in.

To build from source instead (Rust 1.75+):

```sh
./install.sh --build --register-session
```

Other commands:

```sh
./install.sh --check      # verify what is installed
./install.sh --uninstall  # remove NIRUWM only
```

NIRUWM does not touch a NIRUPI DWM installation. It never writes to `dwm`,
`dwm-status.sh`, `nirupi-session`, `~/.xinitrc` or NIRUPI's manifests, so you
can switch between both sessions in SDDM.

## Keys

`Super` is the Windows key. The defaults copy NIRUPI DWM.

| Keys | Action |
| --- | --- |
| `Super+Enter` | terminal |
| `Super+B` | browser |
| `Super+P` | application launcher |
| `Super+1`…`9` | switch workspace |
| `Super+Shift+1`…`9` | move window to workspace |
| `Super+J` / `Super+K` | focus next / previous window |
| `Super+T` / `Super+M` | tile / monocle |
| `Super+Q` | close window |
| `Super+R` | reload configuration |
| `Super+Ctrl+Shift+Q` | quit to SDDM |

Every binding can be changed in `config.toml`; see `config.example.toml` for
the full list and the accepted actions.

## Status

Version 0.2.0. Works for a normal session: start from SDDM, open and tile
windows, switch workspaces, close windows, log out.

Not there yet:

- multiple monitors (the whole X screen is used)
- window rules, swallow, scratchpad
- showing more than one workspace at a time
- a bar with battery, network, media or popups

## Build needs (only for `--build`)

Rust 1.75+, a C linker, `fontconfig` and a monospace font.

## License

MIT. Copyright 2026 Nicklas Rudolfsson.
