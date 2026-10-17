# NIRUWM

A small X11 tiling window manager in Rust, with [NIRUBAR](https://github.com/nirucon/nirubar)
as its status bar.

**A personal hobby project.** I build this for my own machines and my own
workflow. It is public so others can read it and try it, but there is no
support promise. Early software, used at your own risk.

## What it is

- dwm style workspaces, layouts, gaps and keybindings.
- Niru Noir colours: dark background, light text, no decoration.
- A separate bar process showing clickable workspaces, the date and the time,
  plus a battery and network indicator when the machine has them.
- X11 only. One screen for now.

## How it works

A session script starts the bar, then the window manager, then your
applications. It works on its own, and reuses the helpers from
[NIRUPI](https://github.com/nirucon/nirupi) when they are installed. Logging
out stops everything it started.

The programs share one config file, `~/.config/niruwm/config.toml`, so
installing or removing either of them is safe.

Optional companions with the same look:

- [niruscreensaver](https://github.com/nirucon/niruscreensaver), the idle screen
- [nirulock](https://github.com/nirucon/nirulock), the lock screen

## Install

You need an X11 session and a monospace font. No Rust or compiler: the
installer downloads static binaries.

```sh
git clone https://github.com/nirucon/niruwm
cd niruwm
./install.sh --register-session   # adds NIRUWM to SDDM (sudo for that file only)
```

Log out, pick **NIRUWM** in SDDM, and log in.

To build from source (Rust 1.75 or newer):

```sh
./install.sh --build --register-session
```

Other commands:

```sh
./install.sh --check      # verify what is installed
./install.sh --uninstall  # remove NIRUWM only
```

NIRUWM never modifies a NIRUPI installation. It does not write to `dwm`,
`dwm-status.sh`, `nirupi-session`, `~/.xinitrc` or NIRUPI's manifests, so you
can switch between both sessions in SDDM.

### Checking your machine

```sh
niruwm --doctor
```

Lists the programs the default bindings use, whether each is present, and the
package name on Arch, Debian and Void. Nothing is changed.

## Keys

`Super` is the Windows key. The defaults follow NIRUPI's dwm.

| Keys | Action |
| --- | --- |
| `Super+Enter` | terminal |
| `Super+B` | browser |
| `Super+P` | application launcher |
| `Super+1` … `9` | switch workspace |
| `Super+Shift+1` … `9` | move window to workspace |
| `Super+J` / `Super+K` | focus next / previous window |
| `Super+T` / `Super+M` | tile / monocle |
| `Super+Q` | close window |
| `Super+R` | reload configuration |
| `Super+Escape` | lock the screen |
| `Super+Ctrl+Shift+Q` | quit to SDDM |

Every binding can be changed in `config.toml`; see `config.example.toml`.

## Settings

`~/.config/niruwm/config.toml`:

```toml
theme = "niru-noir"
gap = 8
border_width = 2
bar_height = 28
master_ratio = 0.55

terminal = ["kitty"]
terminal_alt = []
browser = ["brave"]
launcher = ["rofi", "-show", "drun"]

[wallpaper]
# NIRUWM only owns the wallpaper when this is true. Leave it false if
# NIRUPI's wallrotate.sh should keep managing the background.
enabled = false
dir = "~/Pictures/Wallpapers"
mode = "random"          # random, fixed or single
file = ""
rotation_minutes = 10

[bar]
enabled = true
title = false
cell_width = 30

[[keys]]
keys = "super+ctrl+Return"
action = "start alacritty"
```

Session options live in `~/.config/niruwm/session.conf`:

```sh
KEYBOARD_LAYOUT=se
START_DUNST=1
START_PICOM=1
START_IDLE=1
```

## Status

Version 0.3.0. Works for a normal session on one laptop screen: start from
SDDM, open and tile windows, switch workspaces, close windows, log out.

Missing:

- multiple monitors (the whole X screen is used)
- window rules, swallow, scratchpad
- more than one workspace visible at a time
- a bar with media, updates or popups

## Build needs

Only for `--build`: Rust 1.75 or newer, a C linker, `fontconfig` and a
monospace font.

## License

MIT. Copyright 2026 Nicklas Rudolfsson.
