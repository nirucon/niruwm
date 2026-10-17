# NIRUWM

A small, fast X11 tiling window manager in Rust, with **NIRUBAR** as its status
bar. Niru Noir styling, dwm-style keybindings and a session that sits next to
NIRUPI DWM in SDDM — log out of one, log into the other, and back.

**0.2.0** — first build intended for real daily testing on a laptop with one
screen. Read *Scope* below before switching your main session.

## What works in 0.2.0

- Nine dwm-style workspaces with clickable cells in the bar.
- Layouts: tile, left stack, monocle, centred master, fibonacci, floating.
- Focus follows mouse, keyboard focus cycling, floating dialogs.
- Fullscreen, close protocol, EWMH desktop/active-window/client-list
  properties, `_NET_SUPPORTED` advertisement.
- The bar reserves space through `_NET_WM_STRUT_PARTIAL`; tiles never sit
  underneath it.
- Swedish keyboard layout by default (`setxkbmap se`), changeable.
- `niructl` for tags, layouts, theme, reload and a clean quit. Commands are
  synchronous: they return once the window manager has applied them.
- A session script that reuses NIRUPI helpers when they exist (lock, battery,
  autostart) and works with **no NIRUPI and no Rust** when they do not.
- Static binaries: no Rust, gcc or `-dev` packages needed to install.

## Scope and known limits (be honest with yourself)

- **One screen.** Coordinates already go through a monitor type, but 0.2.0
  uses the whole X screen. Do not use it on a multi-monitor desk yet.
- **X11 only.** Wayland is out of scope; NIRUPI's helpers are X11 programs.
- No swallow, scratchpad, sticky windows or rule engine yet.
- Mouse drag/resize of floating windows is not implemented.
- The bar shows tags, the Swedish date and `hh:mm`. It is not a NIRUPI status
  bar replacement: no battery, network, MPRIS or clickable popups yet.
- One workspace visible at a time; dwm's tag bitmasks are not implemented.
- No window rules, swallow or scratchpad yet.

## Keybindings (Super = Win key)

The defaults follow NIRUPI DWM. Only `Super+P` differs: it opens the launcher
directly (NIRUPI opens dmenu there) because NIRUWM has no dmenu integration yet.

| Keys | Action |
| --- | --- |
| `Super+Enter` | terminal (`kitty`) |
| `Super+Shift+Enter` | second terminal (`terminal_alt`) |
| `Super+B` | browser (`nirupi-browser` if installed, else `browser`) |
| `Super+P` | launcher (`rofi -show drun`) |
| `Super+Shift+P` | NIRUPI unified launcher (`nirupi-launch.sh`) |
| `Super+W` | window switcher |
| `Super+F` … `Super+T` etc. | see layouts below |
| `Super+1` … `Super+9` | show workspace |
| `Super+Shift+1` … `Super+Shift+9` | move focused window |
| `Super+0` / `Super+Shift+0` | last workspace / move there |
| `Super+Tab` / `Super+Shift+Tab` | next / previous workspace |
| `Super+J` / `Super+K` | focus next / previous window |
| `Super+Left/Right/Up/Down` | focus previous / next |
| `Super+Shift+Left/Right/Up/Down` | rotate the focused window in the stack |
| `Super+Ctrl+Left/Right/Up/Down` | move a floating window |
| `Super+Ctrl+Shift+arrows` | resize a floating window |
| `Super+T` | tile (right stack) |
| `Super+Y` | left stack |
| `Super+M` | monocle |
| `Super+C` | centred master |
| `Super+U` | centred floating master |
| `Super+F` | floating layout |
| `Super+Ctrl+E` | fibonacci |
| `Super+Ctrl+,` / `Super+Ctrl+.` | previous / next layout |
| `Super+H` / `Super+L` | shrink / grow master |
| `Super+I` / `Super+D` | more / fewer master windows |
| `Super+Shift+Space` | toggle focused window floating |
| `Super+Shift+E` | fullscreen |
| `Super+Q` | close focused window |
| `Super+Shift+Q` | NIRUPI power menu |
| `Super+Ctrl+Shift+Q` | quit to SDDM |
| `Super+R` | reload configuration and theme |
| `Super+G` | toggle gaps |
| `Super+Escape` | lock (`nirupi-lock.sh` or `slock`) |
| `Super+V` | clipboard menu |
| `Super+Print` / `Print` | screenshot |
| `Super+Shift+R` | NIRUPI reload |

NIRUPI helper bindings (`Super+Shift+M`, `Super+Alt+K`, `Super+Ctrl+T`,
`Super+S`, `Super+Shift+C`, `Super+Shift+D`, `Super+Shift+L`, `Super+Shift+T`,
`Super+N`, `Super+Shift+Z`, and more) run the matching `nirupi-*.sh` when it
exists and do nothing otherwise.

### Changing bindings

Add a `[[keys]]` table to `config.toml`. Entries replace a default with the
same keys:

```toml
[[keys]]
keys = "super+ctrl+Return"
action = "start alacritty"

[[keys]]
keys = "super+shift+c"
action = "start google-chrome-stable"
```

Actions: `terminal`, `terminal_alt`, `browser`, `launcher`,
`launcher_unified`, `window_switcher`, `start CMD`, `shell CMD`, `tag N`,
`movetotag N`, `toggletag N`, `togglemovetotag N`, `layout NAME`,
`layout_next`, `layout_prev`, `focus_next`, `focus_prev`, `move_next`,
`move_prev`, `move_float left|right|up|down`, `toggle_float`,
`toggle_fullscreen`, `close`, `quit`, `reload`, `toggle_gaps`, `swap_next`,
`swap_prev`, `theme NAME`.

Limits: `Super+Ctrl+N` and `Super+Ctrl+Shift+N` ("toggle tag") are accepted
but 0.2.0 shows one workspace at a time.

## Install

```sh
git clone https://github.com/nirucon/niruwm
cd niruwm
./install.sh --register-session
```

That is all: the installer downloads static binaries (NIRUWM, NIRUBAR,
NIRUCTL), verifies their SHA256 against the release checksums and installs
them under `~/.local/bin`. `--register-session` adds
`/usr/share/xsessions/niruwm.desktop` with sudo.

Then log out of NIRUPI DWM and pick **NIRUWM** in SDDM.

To build yourself instead (needs Rust 1.75+):

```sh
./install.sh --build --register-session
```

Other commands:

```sh
./install.sh --check      # verify binaries and session entry; no changes
./install.sh --uninstall  # remove NIRUWM files and the SDDM entry
```

`--uninstall` never touches NIRUPI: not `dwm`, not `dwm-status.sh`, not
`nirupi-session`, not `~/.xinitrc`, not the NIRUPI manifests.

## Switch back and forth

The SDDM entry is additive. NIRUPI DWM stays in the list:

| What | Where |
| --- | --- |
| NIRUPI DWM session | `/usr/share/xsessions/nirupi-dwm.desktop` → `nirupi-session` |
| NIRUWM session | `/usr/share/xsessions/niruwm.desktop` → `niruwm-session` |

Both launch the same X session environment (D-Bus, PipeWire on Void, GTK/Qt
theme, cursor, `setxkbmap`). NIRUWM reuses `nirupi-lock.sh`,
`nirupi-battery.sh` and `nirupi-autostart.sh` when present, so the lock screen
and battery guard behave as you are used to. Logging out of NIRUWM removes its
bar and helpers; nothing from NIRUPI is stopped or modified.

## Configuration

`~/.config/niruwm/config.toml` (shared by NIRUWM and NIRUBAR):

```toml
theme = "niru-noir"
gap = 8
border_width = 2
bar_height = 28
master_ratio = 0.55
terminal = ["kitty"]
launcher = ["rofi", "-show", "drun"]

[bar]
enabled = true
title = false      # set true to show the focused window title
cell_width = 30
```

The English default plus an existing `~/.config/niru/config.toml` from the
0.1 alpha is still read when the new path does not exist.

Session options (`~/.config/niruwm/session.conf`), all optional:

```sh
KEYBOARD_LAYOUT=se
# KEYBOARD_VARIANT=nodeadkeys
START_DUNST=1
START_PICOM=1
START_IDLE=1
```

## Testing

The session smoke test is the gate for a release. It starts a bare X server,
launches the session, opens a client, switches workspaces, quits and verifies
that nothing is left running:

```sh
sh tests/session-smoke.sh /path/to/bin "$HOME"
```

CI runs it on Arch, Debian and Void containers, and also renders the bar in
Xvfb to catch text that silently stops drawing.

## Build requirements (only for `--build`)

Rust 1.75 or newer, a C linker, `fontconfig` and a monospace TrueType font
(JetBrainsMono Nerd Font preferred, DejaVu Sans Mono works).

```sh
# Arch
sudo pacman -S --needed rust base-devel fontconfig ttf-dejavu kitty rofi
# Debian
sudo apt install cargo rustc build-essential fontconfig fonts-dejavu-core kitty rofi
# Void
sudo xbps-install -S rust cargo base-devel fontconfig dejavu-fonts-ttf kitty rofi
```

## License

MIT. Copyright 2026 Nicklas Rudolfsson.
