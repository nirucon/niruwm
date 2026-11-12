# NIRUWM key bindings

Every default binding of NIRUWM. All of them can be changed or removed in
`~/.config/niruwm/config.toml` (see [Changing a binding](#changing-a-binding)).

Press **Super+/** for the cheat sheet: it shows the bindings in effect,
including your own changes and the keys other programs register, and it can
run any of them. `niructl keys --json` prints the same list for scripts.

## Default bindings

The tables below are generated from `src/bindings.rs` with
`niructl keys --markdown --defaults`; a test fails when they are out of date.
Run `niructl keys --update-doc docs/KEYBINDINGS.md` after changing a default.

<!-- BEGIN GENERATED: niructl keys --markdown --defaults -->

### Apps and launchers

| Keys | Action |
|---|---|
| `Super+Enter` | Open a terminal |
| `Super+Shift+Enter` | Open the alternative terminal |
| `Super+B` | Open the web browser |
| `Super+P` | Application launcher |
| `Super+Shift+P` / `Super+Shift+B` / `Super+Shift+M` | Unified launcher: apps, windows, actions and settings |
| `Super+A` | NIRU Actions: maintenance, diagnostics, contexts and OpenCode |
| `Super+S` | Web search: type a phrase, Enter opens it |
| `Super+V` | Clipboard history |
| `Super+/` / `Super+F1` | Keybinding cheat sheet (this list) |
| `Super+Shift+F1` | The NIRUX manual (search and read) |
| `Super+.` | Emoji and symbol picker |
| `Super+O` | Find files in the home folder |
| `Ctrl+Alt+W` | Jump to a visible window by its label |
| `Super+Shift+N` | Quick note to the inbox (also to Discord or a device) |
| `Super+W` / `Alt+Tab` / `` Super+` `` | Window switcher |
| `Super+Shift+F` | Open the file manager (pcmanfm) |

### Session

| Keys | Action |
|---|---|
| `Super+Escape` | Lock the screen |
| `Super+Shift+Escape` | Lock, then suspend the computer |
| `Super+Shift+R` / `Super+R` / `Super+Ctrl+Shift+R` | Reload the configuration |
| `Super+N` | Night light on or off |
| `Super+Shift+D` | Do Not Disturb on or off |
| `Super+Shift+C` | Keep the screen awake on or off |
| `Super+Shift+W` | Save the open windows as the session |
| `Super+Ctrl+Shift+Q` | Log out (end the session) |

### Layouts

| Keys | Action |
|---|---|
| `Super+G` | Gaps between windows on or off |
| `Super+T` | Layout: Tile |
| `Super+Y` | Layout: Left stack |
| `Super+M` | Layout: Monocle (one window at a time) |
| `Super+C` | Layout: Centered master |
| `Super+U` | Layout: Centered floating master |
| `Super+F` | Layout: Floating |
| `Super+Ctrl+E` | Layout: Fibonacci spiral |
| `Super+Ctrl+S` | Layout: Scroll |
| `Super+Ctrl+Shift+S` | Scroll layout: one or two visible columns |
| `Super+Ctrl+,` | Previous layout |
| `Super+Ctrl+.` | Next layout |
| `Super+Alt+B` | Bar: top, bottom, hidden |
| `Super+H` | Narrower master area |
| `Super+L` | Wider master area |
| `Super+I` | One more window in the master area |
| `Super+D` | One window fewer in the master area |

### Scratchpads

| Keys | Action |
|---|---|
| `Super+Shift+S` | Show or hide the default scratchpad |
| `Super+Alt+1…9` | Show or hide scratchpad 1…9 |
| `Super+Alt+Shift+1…9` | Send the window to scratchpad 1…9, or take it back |

### Focus and windows

| Keys | Action |
|---|---|
| `Super+Shift+Space` | Floating on or off |
| `Super+Shift+E` | Fullscreen on or off |
| `Super+Alt+S` | Sticky (on every workspace) on or off |
| `Super+Shift+J` / `Super+Shift+Right` / `Super+Shift+Down` | Move the window down the stack |
| `Super+Shift+K` / `Super+Shift+Left` / `Super+Shift+Up` | Move the window up the stack |
| `Super+Shift+U` | Go to the window that asked for attention (urgent) |
| `Super+,` / `Super+Alt+,` | Focus the previous screen |
| `Super+Alt+.` | Focus the next screen |
| `Super+Shift+,` | Move the window to the previous screen |
| `Super+Shift+.` | Move the window to the next screen |
| `Super+J` / `Super+Right` / `Super+Down` | Focus the next window |
| `Super+K` / `Super+Left` / `Super+Up` | Focus the previous window |
| `Super+Q` | Close the window |

### Workspaces

| Keys | Action |
|---|---|
| `Super+E` | Overview of every workspace, with thumbnails |
| `Super+Tab` | Next workspace |
| `Super+Shift+Tab` | Previous workspace |
| `Super+1…9` / `Super+0` | Go to workspace 1…9 / 10 |
| `Super+Shift+1…9` / `Super+Shift+0` | Move the window to workspace 1…9 / 10 |
| `Super+Ctrl+1…9` / `Super+Ctrl+0` | Go to workspace 1…9 / 10 |
| `Super+Ctrl+Shift+1…9` / `Super+Ctrl+Shift+0` | Move the window to workspace 1…9 / 10 and follow it |

### Floating windows

| Keys | Action |
|---|---|
| `Super+Ctrl+Left` | Move a floating window left |
| `Super+Ctrl+Right` | Move a floating window right |
| `Super+Ctrl+Up` | Move a floating window up |
| `Super+Ctrl+Down` | Move a floating window down |
| `Super+Ctrl+Shift+Left` | Shrink a floating window horizontally |
| `Super+Ctrl+Shift+Right` | Grow a floating window horizontally |
| `Super+Ctrl+Shift+Up` | Shrink a floating window vertically |
| `Super+Ctrl+Shift+Down` | Grow a floating window vertically |

### Screenshots and color

| Keys | Action |
|---|---|
| `Super+Print` | Screenshot of the whole screen, then review |
| `Print` | Screenshot of a region, then review |
| `Super+Shift+Print` | Screenshot of the active window, then review |
| `Super+Ctrl+C` | Pick a color from the screen and copy it |
| `Super+Ctrl+O` | Copy the text in a screen region (OCR) |

### Hardware keys

| Keys | Action |
|---|---|
| `XF86AudioRaiseVolume` | Volume up |
| `XF86AudioLowerVolume` | Volume down |
| `XF86AudioMute` | Mute or unmute |
| `XF86AudioPlay` | Play or pause |
| `XF86AudioPause` | Pause |
| `XF86AudioNext` | Next track |
| `XF86AudioPrev` | Previous track |
| `XF86MonBrightnessUp` | Screen brightness up (5 %) |
| `XF86MonBrightnessDown` | Screen brightness down (5 %) |

<!-- END GENERATED -->

## The cheat sheet (Super+/)

`Super+/` opens the launcher's keybinding view (`nirulauncher --keys`):

- Every key in effect, grouped (apps, session, layouts, scratchpads, focus,
  workspaces, floating windows, screenshots and color, hardware keys, then the
  keys of other programs).
- Type words (`scratchpad`, `screen`) or keys (`super+t`, `ctrl c`) to search.
- **Enter** runs the selected binding, exactly as if the key were pressed.
- **Ctrl+R**, then press any key combination: the sheet shows what it does, or
  that it is free.
- A key that two programs claim is marked **conflict** and names the other
  program.

The list comes from `niructl keys --json`. A `[[keys]]` entry in
`config.toml` may carry its own words:

```toml
[[keys]]
keys = "super+shift+f"
action = "start thunar"
description = "Open Thunar"
```

## Keys of other programs (keys.d)

Programs that grab keys themselves (NIRUDICTATE, for example) register them in
`~/.local/share/niru/keys.d/<app>.toml` (`$XDG_DATA_HOME/niru/keys.d`), so the
cheat sheet lists them and flags clashes with NIRUWM:

```toml
app = "NIRUDICTATE"          # name shown in the sheet (default: the file name)
group = "Dictation"          # group for every key below (default: Apps)

[[keys]]
keys = "ctrl+alt+space"      # same syntax as [[keys]] in config.toml
description = "Dictate: tap to start and stop, hold to talk"
run = ["nirudictate", "toggle"]   # optional: what Enter in the sheet runs

[[keys]]
keys = "shift+alt+space"
description = "Dictate to the opencode agent"
run = ["nirudictate", "agent", "toggle"]
```

- `keys` and `description` are required; `run` (an argv list, never a shell
  line) and a per-key `group` are optional.
- A program should write its file when it starts and when its keys change,
  so the sheet follows its configuration. A file that cannot be read is
  reported by `niructl keys --json` (`problems`) and skipped.
- NIRUWM does not grab these keys; the program does.

## Scratchpads

A scratchpad is a floating window you show and hide with one key. It is never a
member of a workspace: showing or hiding one never moves or resizes any other
window, and a hidden one is left out of tiling, focus cycling, window lists,
workspace indicators and the saved session.

- **One at a time.** Showing a scratchpad hides the one that was shown. The key
  of the shown one hides it, and focus goes back to the window you were using.
- **It follows you.** A shown scratchpad stays on top when you switch workspace,
  and moves along when you go to the other screen with `Super+,` / `Super+Alt+.` or
  a workspace key there. Moving the mouse alone does not drag it along.
- **It stays until you hide it.** Moving the mouse onto another window gives
  that window the keyboard focus, but the scratchpad stays visible on top. Set
  `hide_on_focus_loss = true` in `[scratchpad]` to hide it instead whenever
  another window gets the focus (default: `false`).
- **Its dialogs belong to it.** A dialog the scratchpad's program opens (Open,
  Save, a confirmation) stays above the scratchpad, is hidden and shown with
  it and follows it to another workspace or screen. That includes a file
  chooser from xdg-desktop-portal, which names no parent window: a dialog
  that opens while the scratchpad has the focus is treated as its own. A
  dialog never takes a scratchpad slot itself, even when it has the
  scratchpad's window class.
- **Any window can be one.** `Super+Alt+Shift+N` hides the focused window in
  slot N. If slot N already holds a window, that window comes back to the
  current workspace as a normal window (they swap places). Pressing
  `Super+Alt+Shift+N` on the window in slot N, `Super+Shift+Space`,
  `Super+Shift+1…0` or `niructl scratchpad unscratch` makes it a normal window
  again.
- **Closing frees the slot.** The next press of a slot with a configured command
  starts it again; an empty slot without one shows a short hint.
- Numbered slots may have their own command (recognised by its window class) and
  size in `[[scratchpad.slot]]`; see [config.example.toml](../config.example.toml).
- `niructl scratchpad [N] [toggle|show|hide]`, `niructl scratchpad send N`,
  `niructl scratchpad unscratch` and `niructl scratchpad list [--json]` do the
  same from scripts. `niructl scratchpad send N --window ID` sends one named
  window instead of the focused one (NIRUDICTATE's agent mode keeps opencode in
  slot 9 this way); a window already in slot N stays there. NIRUBAR's `scratchpad` module lists them and shows or hides
  them with a click; it reads the `_NIRUWM_SCRATCHPADS` root property (one line
  per slot: `slot<TAB>shown|hidden|empty<TAB>window<TAB>class`, slot 0 is the
  default).

## Volume and brightness keys

The volume keys stop at 100 % and show the level through `nirunotify --osd`.
The same call plays the NIRUX feedback sound: a soft tick whose pitch follows
the new level, a duller bump at 0 % or 100 %, and a two-note mute/unmute (the
mute sound plays just before the sink is muted). Brightness keys can tick too
(off by default). Switch them with `nirux sounds`; NIRUNOTIFY 0.4 or newer
plays them.

## Mouse

| Input | Action |
|---|---|
| `Super`+left-drag | Move window; a tiled window becomes floating and can cross monitor boundaries |
| `Super`+right-drag | Resize from the nearest corner |
| Click on title/bar module | Normal application interaction; no `Super` required |

## Changing a binding

```toml
[[keys]]
keys = "super+shift+f"
action = "start thunar"

[[keys]]
keys = "super+g"
action = "unbind"
```

An entry with the same keys replaces the default; a new keys entry adds a
binding; `action = "unbind"` removes a default. Modifiers are `super`, `shift`,
`ctrl` and `alt`. The full list of actions is in
[config.example.toml](../config.example.toml).
