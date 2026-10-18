# NIRUWM

A small X11 tiling window manager in Rust, with [NIRUBAR](https://github.com/nirucon/nirubar)
as its status bar.

**A personal hobby project.** I build this for my own machines and my own
workflow. It is public so others can read it and try it, but there is no
support promise. Early software, used at your own risk.

## What it is

dwm style workspaces and layouts, Niru colours, and a bar in a separate process.
X11. One or several screens; each screen keeps its own workspaces.

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

## Screens

Every connected screen gets its own nine workspaces, like dwm. Plug a screen in
or out and NIRUWM follows: windows move to a remaining screen and each screen
keeps the workspace it was showing.

`Super+,` and `Super+.` move focus between screens, `Super+Shift+,` and
`Super+Shift+.` move the focused window. `niructl monitors` lists the screens.

```toml
[monitors]
workspaces = 9

[[monitors.profiles]]
name = "HDMI-1"
workspaces = ["web", "code", "chat"]
primary = true
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

**Sessions.** With `restore_session = true` at the top of the config, the
programs open when a session ends are started again at the next login, on the
same workspace and screen. `Super+Shift+W` (`niructl save-session`) saves on
demand; `niructl clear-session` starts clean next time. A program that is no
longer installed is named in a notification and skipped. `niructl
save-session` without a window manager lists the saved windows instead.

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

```toml
[scratchpad]
command = ["kitty", "--class", "scratchpad"]
width = 0.6
height = 0.6

[[rules]]
class = "firefox"
workspace = 2
floating = true
```

The hardware media keys are bound by NIRUWM itself, so they work with no
daemon. They need `wpctl` (or `amixer`) for volume and `playerctl` for playback.

`niruwm --replay FILE` runs a scripted session: key presses, expectations about
client and workspace counts. It is how a bug gets reproduced without a second
machine.

```text
expect wm NIRUWM
key super+Return
wait 1200
expect clients 1
key super+3
wait 400
expect tag 3
```

`niruwm --dump-config` prints every value in effect, including the full key
table, so you can see what the programs are really using.

Session options live in `~/.config/niruwm/session.conf` (`KEYBOARD_LAYOUT`,
`START_DUNST`, `START_PICOM`, `START_IDLE`). Idle times live in
`~/.config/niruui/screensaver.toml`.

**Reload after editing:** `niructl reload`, or Super+R. That reloads the window
manager, the bar and the theme. An invalid file leaves the previous settings in
place and prints the error. Check without applying: `niruwm --check-config`.

## Status

Version 0.16.0. Works for a normal session on one or several screens.

The default terminal is kitty; when it is not installed, Super+Enter starts the
first of alacritty, foot and xterm that is. A binding whose program is missing
now sends a notification naming it instead of failing silently.

Building from source needs Rust 1.75 or newer. The released binaries run on
Debian bookworm; its cargo 1.63 cannot build them, so use a newer toolchain
there (rustup, or the nirulock package's instructions).

Missing: swallow, more than one workspace visible at a time, a graphical
monitor arrangement editor.

## Build needs

Only for `--build`: Rust 1.75 or newer, a C linker, `fontconfig` and a
monospace font.

## License

MIT. Copyright 2026 Nicklas Rudolfsson.
