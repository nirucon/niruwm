# NIRUWM

**Part of [NIRUX - A minimal X11 desktop by Nicklas Rudolfsson](https://github.com/nirucon/nirux).**

**Early beta.** A small Rust/X11 tiling window manager and session layer for the
NIRUX X11 desktop, built in the spirit of [suckless dwm](https://dwm.suckless.org/),
which the author used for many years.

It keeps what makes dwm pleasant: tags instead of virtual desktops, a single
plain-text configuration, keybindings as the primary interface, and a
codebase small enough to read. It adds what a daily machine needs: per-monitor
workspaces, seven layouts, a bar, a launcher, notifications, a lock screen and
the `niructl`/`niruidle` tools.

## Features

- Seven layouts: tile, left, monocle, centered, centered-floating, fibonacci and
  floating.
- Ten workspaces per monitor, selected with `Super+1`…`Super+0`.
- A theme without a wallpaper paints its own background colour, so switching
  theme never leaves the previous picture on screen.
- Tiling, floating, fullscreen, scratchpad, master ratio, master count, gaps and
  window swapping.
- dwm-style mouse move/resize with **Super+left-drag** and **Super+right-drag**.
- Session save/restore, autostart, workspace recipes and focus/presentation modes
  through `niructl`.
- Native notification, DND, audio, microphone, Bluetooth, display-profile, power-
  profile, brightness, night-light and screenshot integration.
- Shared NIRU themes, font metrics and appearance contract.
- `niructl`: provider-neutral controls, diagnostics, work modes and recipes.
- `niruidle`: idle, lock and DPMS timing without a separate idle daemon. The
  whole ladder is one sequence with one timer per step: active ->
  `screensaver_timeout` -> screensaver -> `lock_after_screensaver` -> locked,
  configured in the shared `[idle]` section.

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
| Super+S | Web search: type a phrase, Enter opens the engine |
| Super+Shift+M | Unified launcher |
| Super+W | Window switcher |
| Super+Shift+F | File manager |
| Super+Escape | Lock |
| Super+Shift+Escape | Lock, wait three seconds, then suspend |
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
active -> screensaver -> locked has a single timer on each step. `niructl timing
show` prints the effective values and `niructl timing set KEY MINUTES` changes
one of them in place (comments and unrelated keys are preserved); the running
`niruidle` reloads the file within a few seconds.

`Super+Shift+Escape` is the explicit suspend shortcut. It starts NIRULOCK,
waits for the verified `_NIRU_LOCK_READY` contract, keeps the lock screen
visible for three seconds, verifies that the same lock window is still there
and only then requests suspend through the login1 API. Repeated presses are
coalesced, and unlocking during the delay cancels the request. The existing
lid/sleep inhibitor path in the session keeps its own lock-before-sleep
behaviour. A legacy
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

`niructl wallpaper status-json` reports the same settings as one JSON document
together with `planned` (what each screen will show) and `rendered` (what the
running session actually set, or `null` when no window manager is running).
NIRUWM publishes the rendered result on `_NIRUWM_WALLPAPER_STATE`, so a
configuration is never reported as a displayed picture.

### Layouts per workspace

A layout belongs to one workspace on one screen. Changing it there leaves
every other workspace, on every screen, exactly as it was, and every workspace
starts tiled until you change it:

```sh
niructl layout show          # the layouts stored in the config
niructl layout monocle       # this workspace only
niructl layout save          # write the current ones to the file
```

`Super+T/Y/M/C/U/F` set the focused workspace's layout and
`Super+Ctrl+period` / `comma` cycle through them. The choice is written to
`[monitors] layouts` at once, so it survives a crash as well as a logout, and
it comes back after the next login. A screen can override the common list in
its own `[[monitors.profiles]]` entry with `layouts = [...]`.

### More than one screen

Every connected screen is set separately, at its own geometry (for example
`1080x1920+1600+0` for a portrait screen to the right of a laptop panel). The
common settings apply to every screen that has no entry of its own:

```sh
niructl wallpaper output list                  # screens with source, mode and scale
niructl wallpaper output set DP-1 image PATH   # this screen's own picture
niructl wallpaper output set DP-1 scale fit
niructl wallpaper output set DP-1 mode theme   # follow the theme on this screen
niructl wallpaper output reset DP-1            # inherit the common settings
```

`[[wallpaper.outputs]]` entries are keyed by RandR output name, kept while a
screen is unplugged and reused when it returns to the same port. A change is
written with comments, permissions and unrelated keys preserved, then checked
against the configuration rules; a rejected reload restores the previous file
unless someone edited it meanwhile. The setter is `feh` (without writing
`~/.fehbg`), then `hsetroot`, `nitrogen` or `xwallpaper`; a screen whose theme
has no picture is painted with the theme's background colour.

#### Turning a screen and saving a layout

A screen mounted upside down or standing on its side is turned on its own:

```sh
niructl display rotate DisplayPort-5 inverted   # normal, left, right, inverted
```

Once the screens look right, the arrangement is saved as a profile. A profile
stores every active screen with its mode, position, rotation and which one is
primary, so a laptop-and-desk setup can be switched in one command:

```sh
niructl display save Desk                     # the layout as it is now
niructl display list                          # saved profiles
niructl display preview Desk 15               # try it; keeps or reverts after 15s
niructl display keep                          # confirm during a preview
niructl display apply Desk                    # switch without a countdown
niructl display auto                          # side by side, preferred modes
```

A failed switch puts the previous layout back. Profiles live in
`~/.config/niruwm/displays.toml`; `displays.example.toml` in the source tree
shows the file format, and the launcher lists the same profiles under
Displays. Each output can record a refresh rate, and old profiles without one
keep xrandr's choice for the selected mode.

An opt-in startup profile is applied once by `niruwm-session` before the WM
reads the monitor layout:

```sh
niructl display startup show
niructl display startup set Desk
niructl display startup off
niructl display startup apply   # what the session runs at login
```

There is no automatic switching by default. A missing profile or output leaves
the current connected screens usable and logs why the profile was skipped.

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
