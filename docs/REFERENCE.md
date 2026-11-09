# NIRUWM reference

Detailed reference for NIRUWM's configuration and the `niructl` control
command. The [README](../README.md) has the overview; key bindings are in
[KEYBINDINGS.md](KEYBINDINGS.md) and display profiles in
[display-profiles.md](display-profiles.md).

## niructl at a glance

Run `niructl` without arguments to print the full usage line. The most used commands:

```sh
niructl reload                 # reload WM, bar and theme
niructl restart-bar
niructl bar top|bottom|hide|show|toggle|cycle|status
niructl theme niru-nord
niructl themes                 # catalogue, origin and active marker
niructl theme-catalog          # the same catalogue as JSON for GUIs
niructl theme-import ~/theme.toml --id my-theme --wallpaper ~/wall.png
niructl theme-hide niru-satie  # hide a bundled theme
niructl theme-show niru-satie
niructl theme-remove my-theme  # user themes only
niructl layout show|save|NAME|columns 1|2|toggle
niructl timing show
niructl timing set screensaver_timeout 20
niructl storage review
niructl input status
niructl input set-keyboard se
niructl input set-pointer natural on
niructl task list
niructl agent --dry-run "explain why the microphone is silent"
niructl windows rescue --dry-run
niructl urgent [--json]        # windows that asked for attention, newest last
niructl urgent focus           # go to the newest one (Super+Shift+U)
niructl swallow on|off|toggle|status
niructl studio on|off|toggle|status   # studio (DAW) mode for this session
```

`niructl urgent` reads `_NIRUWM_URGENT` (a WINDOW list on the root window,
oldest request first); a window in a scratchpad is listed as `scratchpad`.
`niructl swallow` changes window swallowing for the running session only and
reports the state the window manager publishes in `_NIRUWM_SWALLOW`;
`[swallow] enabled` in config.toml is the setting for every login. A window
manager started again after a crash keeps the session's switch.

`niructl studio on` (studio or DAW mode, `[studio]` in config.toml, action
`studio_toggle`) applies, item by item and as configured: a session-scoped
idle inhibition (`_NIRU_IDLE_INHIBIT` on the root window, which NIRUIDLE 0.20
honours; the screen saver, the idle lock and display-off wait), DND through
`niructl dnd`'s provider, stopping this display's picom (its command line is
kept), and `powerprofile set performance`. What it changed is kept in
`_NIRUWM_STUDIO_SAVED`, the switch in `_NIRUWM_STUDIO`; both live on the root
window, so a window manager restarted after a crash keeps the mode and the X
server's end takes it away: the idle lock can never stay off after a logout.
`niructl studio off` puts back only what it changed and only where nothing
else changed it since: DND it turned on goes off, picom starts again with the
same command line unless one runs already, the previous power profile returns
unless you picked another one meanwhile. The keep-awake switch
(`niructl idle`) is separate and never touched. The session runs
`niructl studio off --session-end` at logout. With `[studio] auto_enable =
true` the first window of a `daw_classes` class (REAPER by default) turns the
mode on and closing the last one turns it off, unless you turned it on by
hand. `~/.config/picom.conf` is never edited.

`niruthemeselector` is the graphical front end for themes: it previews
wallpaper and palette, applies through the same path and manages user imports
and hidden bundled themes. NIRUWM's theme change still owns the terminal hook,
wallpaper, bar and running-session propagation.

## Idle, screensaver and lock timing

Screensaver and lock timing live in the shared config under `[idle]`:

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
behaviour (`niructl lock --reason suspend`). Every lock tells NIRULOCK why it
was started (`NIRULOCK_REASON`: `manual` for `Super+Escape`, `idle` for
NIRUIDLE, `suspend` before sleep, `script` for `niructl lock --silent` or a
non-interactive `niructl lock`), and NIRULOCK's pause music plays only for a
manual lock by default. A legacy
`~/.config/niruui/screensaver.toml` is still read for any value `[idle]` does
not set, so an older installation keeps its timing. Notifications use
NIRUNOTIFY when installed, with Dunst fallback.

## Session

Session options live in `~/.config/niruwm/session.conf`, a shell file sourced
by `niruwm-session`: `KEYBOARD_LAYOUT`, `KEYBOARD_VARIANT`,
`NOTIFICATION_PROVIDER` (`auto`, `native`, `dunst` or `external`),
`WELCOME_NOTIFICATION` (`always`, `once` or `off`), `START_DUNST`,
`START_PICOM`, `START_IDLE` and `START_SLEEP_LOCK` (`1` or `0`). Restart the
session after changing it.

For a TTY login, run `startx "$HOME/.local/bin/niruwm-session"` (or plain
`startx` when `.xinitrc` selects NIRUWM). The session keeps a valid login-provided
`XDG_RUNTIME_DIR`; without one it uses a private, mode-700 directory under
`${XDG_CACHE_HOME:-$HOME/.cache}/niruwm/runtime` before starting D-Bus/audio.
Startup errors and WM errors are recorded in
`${XDG_STATE_HOME:-$HOME/.local/state}/niruwm/session.log`.


## Theme wallpapers

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

## Layouts per workspace

A layout belongs to one workspace on one screen. Changing it there leaves
every other workspace, on every screen, exactly as it was, and every workspace
starts tiled until you change it:

```sh
niructl layout show          # the layouts stored in the config
niructl layout monocle       # this workspace only
niructl layout save          # write the current ones to the file
```

`Super+T/Y/M/C/U/F` set the focused workspace's layout; `Super+Ctrl+S` selects
Scroll and `Super+Ctrl+period` / `comma` cycle through all eight. The choice is
written to `[monitors] layouts` at once, so it survives a crash as well as a
logout, and it comes back after the next login. A screen can override the common
list in its own `[[monitors.profiles]]` entry with `layouts = [...]`.

Master ratio (`Super+H/L`) and master count (`Super+I/D`) are also per
workspace. They are stored beside the layout as `master_ratios` and `nmasters`,
so changing one workspace never resizes another. The top-level `master_ratio`
is the default for new workspaces, not a live global override.

Scroll keeps one window per column. Two adjacent columns are visible by default;
focus reveals the minimum needed viewport, so `A B -> B C -> C D`. A single
window fills the work area. Use `niructl layout columns 1|2|toggle` to change the
mode; the layout name is persisted, the column mode is session state and returns
to two columns after a restart. Off-viewport columns remain managed and appear
in the window switcher, but are not treated as lost windows by rescue/display
watch. If the work area is narrower than 480 px, two-column mode uses a
one-column geometry fallback. Hidden columns stay in the window switcher and
`_NET_CLIENT_LIST`; activating one reveals its column.

## More than one screen

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

### Turning a screen and saving a layout

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
`~/.config/niruwm/displays.toml`;
[`displays.example.toml`](https://github.com/nirucon/nirux/blob/main/displays.example.toml)
in the NIRUX repository shows the file format, and the launcher lists the same profiles under
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

A missing profile or output leaves the current connected screens usable and
logs why the profile was skipped.

Since 0.17 a saved profile also records which physical screen each entry is
(make, model and serial from the EDID). It therefore follows its screens when
connector names such as `DP-1` and `DP-2` swap or a dock port changes.
Profiles saved earlier keep matching by connector; saving one again upgrades
it.

Automatic switching is off by default. When it is on, the session's display
watcher applies the right layout whenever screens are plugged in or out or
the laptop lid closes:

- a profile that fits the connected screens exactly;
- otherwise the layout those screens had last time;
- otherwise the current layout with the new screens added to the right.

A closed lid turns the built-in screen off while another screen is
connected, and the session is never left dark. Every change shows a short
notification. The watcher waits for RandR and kernel events instead of
polling.

```sh
niructl display automatic on|off|status [--json]|run
niructl display doctor [--json]   # what fits, what is missing and why
```

See [docs/display-profiles.md](docs/display-profiles.md) for the matching
rules, the lid, verification and retries, the state files and the limits.

## Audio, Bluetooth, power and modes

`niructl` provides provider-neutral controls for audio, microphone, Bluetooth,
display profiles, power profiles, DND, diagnostics, work modes and workspace
recipes:

```sh
niructl audio status --json
niructl audio outputs --json
niructl audio sources --json
niructl audio mic-status --json
niructl audio mic-level set 30
niructl audio mic-test 5 --json
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

## Compositor, rules and themes

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

