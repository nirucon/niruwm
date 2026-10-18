# NIRUWM

A small X11 tiling window manager in Rust, with [NIRUBAR](https://github.com/nirucon/nirubar)
as its status bar.

**A personal hobby project.** I build this for my own machines and my own
workflow. It is public so others can read it and try it, but there is no
support promise. Early software, used at your own risk.

## What it is

dwm style workspaces and layouts, Niru Noir colours, and a bar in a separate
process. X11 only, one screen for now.

Companions with the same look, each optional:

- [nirubar](https://github.com/nirucon/nirubar), the status bar
- [niruscreensaver](https://github.com/nirucon/niruscreensaver), the idle screen
- [nirulock](https://github.com/nirucon/nirulock), the lock screen

## How it works

A session script starts the bar, then the window manager, then your
applications. `niruidle` runs the idle ladder: screensaver, then lock, then
display off. Everything works on its own, and NIRUPI's helpers are used when
they are installed. Logging out stops everything it started.

NIRUWM never starts in a NIRUPI DWM session, and NIRUPI never starts a NIRU
program.

## Install

You need an X11 session and a monospace font. No Rust or compiler.

```sh
git clone https://github.com/nirucon/niruwm
cd niruwm
./install.sh --register-session   # adds NIRUWM to SDDM (sudo for that file only)
```

Log out, pick **NIRUWM** in SDDM, and log in. To build from source (Rust 1.75+):
`./install.sh --build --register-session`.

Other commands:

```sh
./install.sh --check      # verify what is installed
./install.sh --uninstall  # remove NIRUWM only
niruwm --doctor           # what is installed, what a binding needs
```

## Layouts

| Name | Keys | What it does |
| --- | --- | --- |
| `tile` | Super+T | master left, stack right |
| `left` | Super+Y | master right, stack left |
| `monocle` | Super+M | one window fills the screen |
| `centered` | Super+C | master in the middle, stack either side |
| `centeredfloat` | Super+U | master centred, the rest floating |
| `fibonacci` | Super+Ctrl+E | spiral split |
| `floating` | Super+F | nothing tiled |

Cycle with Super+Ctrl+period and Super+Ctrl+comma, or list them with
`niructl layouts`.

## Themes

`niru-noir`, `niru-nord`, `niru-satie`, `niru-carl-larsson`. Choose one with
`niructl theme NAME`, or set `theme` in the config. Put your own in
`~/.config/niruwm/themes/NAME.toml`. List them with `niructl themes`.

## Keys

`Super` is the Windows key. Every binding is in the config; the defaults follow
NIRUPI's dwm.

| Keys | Action |
| --- | --- |
| Super+Enter | terminal |
| Super+B | browser |
| Super+P | application launcher |
| Super+1 .. 9 | switch workspace |
| Super+Shift+1 .. 9 | move window to workspace |
| Super+J / Super+K | focus next / previous |
| Super+T / Super+M | tile / monocle |
| Super+Q | close window |
| Super+Escape | lock the screen |
| Super+R | reload the configuration |
| Super+Ctrl+Shift+Q | quit to SDDM |

## Settings

`~/.config/niruwm/config.toml`, shared with NIRUBAR and documented inline in
`config.example.toml`. It covers appearance, programs, wallpaper, the bar and
every key binding.

```toml
theme = "niru-noir"
gap = 8

[wallpaper]
enabled = false
dir = "~/Pictures/Wallpapers"
mode = "random"      # random, fixed, single
rotation_minutes = 10
scale = "fill"       # fill, fit, stretch, center
```

Session options live in `~/.config/niruwm/session.conf` (`KEYBOARD_LAYOUT`,
`START_DUNST`, `START_PICOM`, `START_IDLE`). Idle times live in
`~/.config/niruui/screensaver.toml`.

**Reload after editing:** `niructl reload`, or Super+R. That reloads the window
manager, the bar and the theme. An invalid file leaves the previous settings in
place and prints the error. Check without applying: `niruwm --check-config`.

## Status

Version 0.5.0. Works for a normal laptop session on one screen.

Missing: multiple monitors, window rules, swallow, scratchpad, more than one
workspace visible at a time.

## Build needs

Only for `--build`: Rust 1.75 or newer, a C linker, `fontconfig` and a
monospace font.

## License

MIT. Copyright 2026 Nicklas Rudolfsson.
