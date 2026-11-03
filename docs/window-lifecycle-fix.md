# Window lifecycle / bar reliability fix (0.16.162)

This is the implementation record for
[`agent-window-lifecycle-oneshot.md`](agent-window-lifecycle-oneshot.md).

## Root causes found

1. **Panels were not restacked after fullscreen churn.** NIRUWM raised
   fullscreen and floating clients, but never raised the managed dock window
   again when the fullscreen client returned to a normal rectangle. The bar
   could then be covered by a Steam/RetroArch window and stop receiving
   workspace clicks.
2. **Destroyed windows could remain in the request path.** Requests already in
   flight against a just-destroyed XID produced `BadWindow` errors. The WM only
   logged them, so one stale reference could keep producing errors on every
   arrange/border/property pass without repairing the window list.
3. **Helper windows took focus.** Every new non-restored client on the active
   workspace received focus, including utility palettes, notifications, menu
   popups, toolbars and other Steam helpers. That produced focus/border
   flicker while Steam opened its window family.
4. **Input-only helpers were managed like normal windows.** They cannot be
   tiled, bordered or focused, and doing so created avoidable protocol errors.
5. **UnmapNotify attribute races aborted the event.** A window destroyed
   between the notification and the attribute reply was not removed.

## Fix

- Track dock windows separately and restack them above normal/floating
  clients whenever no fullscreen client is visible. A fullscreen client may
  cover the bar; leaving fullscreen raises the panel again.
- On an X11 `Window` error for a tracked window, remove it through the normal
  idempotent unmanage path and arrange once. Keep a bounded set of recently
  removed XIDs so in-flight errors from those exact windows are treated as
  expected cleanup, not logged as new failures.
- Ignore input-only windows (map them, do not manage/focus them).
- Map desktop windows below everything and leave them unmanaged.
- Float the full EWMH helper family and only focus normal windows and real
  dialogs on map. Utilities, notifications, popups, menus and toolbars wait
  for an explicit click or `_NET_ACTIVE_WINDOW` request.
- Prefer focusable clients when choosing a fallback focus after a window dies.
- Treat a vanished `GetWindowAttributes` reply in UnmapNotify as a destroyed
  window and unmanage it immediately.

No Steam/RetroArch class names are hardcoded. The rules are generic EWMH/X11
semantics and apply to any toolkit with the same window family.

## Tests added

All are real-X11 tests in `tests/x11.rs`, run on an isolated server:

```sh
xvfb-run -a cargo test --locked --test x11 -- --ignored --test-threads=1
```

- `docks_stay_above_normal_clients_after_fullscreen`
- `utility_and_popup_windows_do_not_steal_focus`
- `input_only_helpers_are_not_managed_or_focused`

They join the existing lifecycle, drag, focus, restore, key-binding,
theme-reload and bar-work-area tests. The release workflow also runs its
normal session/config/audio/display smoke tests.

## Additional live checks

- Real NIRUBAR 0.18.76 + NIRUWM 0.16.162 on an isolated X server: workspace
  cells clicked before the stress, then after eight rounds of five client
  windows, fullscreen enter/exit and forced destroys. Every click changed the
  desktop and every return click restored it.
- Real RetroArch 1.20 and Steam desktop mode on the isolated display, with the
  new WM and real NIRUBAR: workspace clicks still worked after opening and
  toggling RetroArch fullscreen and after Steam opened its window.
- NIRUSHOT 0.4.10 produced a 1920x1200 PNG with RetroArch open. The old
  `/usr/local/bin/nirushot` 0.4.8 that shadowed the NIRUX-managed binary on
  the target was renamed out of the way.

## Version matrix

| Component | Version | Note |
|---|---|---|
| NIRUWM | 0.16.162 | fixed |
| NIRUBAR | 0.18.76 | unchanged; compatible pin |
| NIRUX | 0.12.195 | pins NIRUWM 0.16.162 |
| NIRUSHOT | 0.4.10 | NIRUX-managed; old system binary removed on target |

NIRUWM release: `v0.16.162`. NIRUX release: `v0.12.195`.

## Remaining acceptance gate

The physical othala session was locked when the release was deployed, so its
running WM could not be replaced without the owner logging in. The new
generation is installed; log out/in (or reboot) and then verify on the real
seat:

- Steam desktop mode and Big Picture through `gamescope`, opening and closing
  repeatedly, with NIRUBAR workspace clicks before/during/after.
- RetroArch menu -> content -> menu -> fullscreen exit, 20 cycles, including
  touchpad/mouse clicks and workspace switching afterwards.
- Screenshot, notification, audio, theme and lock smoke checks.

The isolated tests above are not a claim of physical-session acceptance.
Previous NIRUX generations remain available through `nirux rollback`.
