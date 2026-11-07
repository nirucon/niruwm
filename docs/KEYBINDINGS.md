# NIRUWM key bindings

Every default binding of NIRUWM, as defined in `src/bindings.rs`. All of them
can be changed or removed in `~/.config/niruwm/config.toml` (see
[Configuration](../README.md#configuration)). The launcher's key-binding view
always shows the effective bindings, including your own overrides.

## Applications and session

| Keys | Action |
|---|---|
| `Super+Enter` | Terminal |
| `Super+Shift+Enter` | Alternative terminal |
| `Super+B` | Browser |
| `Super+P` | Application launcher |
| `Super+A` | NIRU Actions: maintenance, diagnostics, contexts and optional OpenCode |
| `Super+Shift+P` | Unified launcher: applications, windows, actions and settings |
| `Super+Shift+B` / `Super+Shift+M` | Unified launcher (alternative keys) |
| `Super+S` | Web search: type a phrase, Enter opens the engine |
| `Super+V` | Clipboard history |
| `Super+W` / `Alt+Tab` | Window switcher |
| `Super+grave` | Window switcher with the workspace-grouped overview |
| `Super+Shift+F` | File manager (`pcmanfm`) |
| `Super+Escape` | Lock |
| `Super+Shift+Escape` | Lock, wait three seconds, then suspend |
| `Super+R` / `Super+Shift+R` / `Super+Ctrl+Shift+R` | Reload configuration |
| `Super+Shift+D` | Toggle Do Not Disturb |
| `Super+Shift+C` | Toggle idle inhibition |
| `Super+Shift+W` | Save session |
| `Super+G` | Toggle gaps |
| `Super+N` | Toggle night light |
| `Super+Q` | Close window |
| `Super+Ctrl+Shift+Q` | Log out |

## Layouts

| Keys | Action |
|---|---|
| `Super+T` | Tile |
| `Super+Y` | Left stack |
| `Super+M` | Monocle |
| `Super+C` | Centered |
| `Super+U` | Centered floating master |
| `Super+F` | Floating layout |
| `Super+Ctrl+E` | Fibonacci |
| `Super+Ctrl+S` | Scroll |
| `Super+Ctrl+Shift+S` | Scroll: toggle one/two visible columns |
| `Super+Ctrl+,` / `Super+Ctrl+.` | Previous / next layout |
| `Super+Shift+Space` | Toggle floating |
| `Super+Shift+E` | Toggle fullscreen |
| `Super+Alt+S` | Toggle sticky (visible on every workspace of its monitor) |
| `Super+Alt+B` | Cycle bar: top, bottom, hidden |
| `Super+H` / `Super+L` | Decrease / increase master ratio |
| `Super+I` / `Super+D` | Increase / decrease master count |
| `Super+Shift+J` / `Super+Shift+K` | Swap with next / previous window |

## Scratchpads

| Keys | Action |
|---|---|
| `Super+Shift+S` | Show / hide the default scratchpad (`[scratchpad] command`, started on first use) |
| `Super+Alt+1…9` | Show / hide scratchpad 1…9 |
| `Super+Alt+Shift+1…9` | Send the focused window to scratchpad 1…9; on a window already there, take it back |
| `Super+Shift+Space` | On a scratchpad: make it a normal tiled window again |

A scratchpad is a floating window you show and hide with one key. It is never a
member of a workspace: showing or hiding one never moves or resizes any other
window, and a hidden one is left out of tiling, focus cycling, window lists,
workspace indicators and the saved session.

- **One at a time.** Showing a scratchpad hides the one that was shown. The key
  of the shown one hides it, and focus goes back to the window you were using.
- **It follows you.** A shown scratchpad stays on top when you switch workspace,
  and moves along when you go to the other screen with `Super+,` / `Super+.` or
  a workspace key there. Moving the mouse alone does not drag it along.
- **It stays until you hide it.** Moving the mouse onto another window gives
  that window the keyboard focus, but the scratchpad stays visible on top. Set
  `hide_on_focus_loss = true` in `[scratchpad]` to hide it instead whenever
  another window gets the focus (default: `false`).
- **Its dialogs belong to it.** A dialog the scratchpad's program opens (Open,
  Save, a confirmation) stays above the scratchpad, is hidden and shown with
  it and follows it to another workspace or screen. A dialog never takes a
  scratchpad slot itself, even when it has the scratchpad's window class.
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
  same from scripts. NIRUBAR's `scratchpad` module lists them and shows or hides
  them with a click; it reads the `_NIRUWM_SCRATCHPADS` root property (one line
  per slot: `slot<TAB>shown|hidden|empty<TAB>window<TAB>class`, slot 0 is the
  default).

## Focus and windows

| Keys | Action |
|---|---|
| `Super+J` / `Super+K` | Next / previous window |
| `Super+Right` or `Super+Down` | Next window |
| `Super+Left` or `Super+Up` | Previous window |
| `Super+Shift+Right` or `Super+Shift+Down` | Move window next |
| `Super+Shift+Left` or `Super+Shift+Up` | Move window previous |
| `Super+,` / `Super+.` | Previous / next monitor |
| `Super+Shift+,` / `Super+Shift+.` | Move window to previous / next monitor |

## Workspaces

| Keys | Action |
|---|---|
| `Super+1…9` | Select workspace 1…9 |
| `Super+0` | Select workspace 10 |
| `Super+Shift+1…9` / `Super+Shift+0` | Move window to workspace 1…9 / 10 |
| `Super+Ctrl+1…9` / `Super+Ctrl+0` | Toggle workspace 1…9 / 10 on the current view |
| `Super+Ctrl+Shift+1…9` / `Super+Ctrl+Shift+0` | Toggle window on workspace 1…9 / 10 and move it there |
| `Super+Tab` / `Super+Shift+Tab` | Next / previous workspace |

## Floating windows

| Keys | Action |
|---|---|
| `Super+Ctrl+Left/Right/Up/Down` | Move floating window |
| `Super+Ctrl+Shift+Left/Right` | Resize floating window horizontally (20 px) |
| `Super+Ctrl+Shift+Up/Down` | Resize floating window vertically (20 px) |

## Screenshots and hardware keys

| Keys | Action |
|---|---|
| `Super+Print` | Full-screen capture and review (`nirushot --full`) |
| `Print` | Region capture and review (`nirushot --region`) |
| `Super+Shift+Print` | Active-window capture and review (`nirushot --window`) |
| `XF86AudioRaiseVolume` / `XF86AudioLowerVolume` | Volume up / down |
| `XF86AudioMute` | Mute |
| `XF86AudioPlay` / `XF86AudioPause` | Play / pause |
| `XF86AudioNext` / `XF86AudioPrev` | Next / previous track |
| `XF86MonBrightnessUp` / `XF86MonBrightnessDown` | Brightness up / down (5 % steps) |

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
