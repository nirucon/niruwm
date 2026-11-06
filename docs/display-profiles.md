# Display profiles and automatic switching

`niructl display` owns the screen arrangement in a NIRU session. NIRU Display
(`nirudisplay`) is the graphical editor for the same profiles. Both go through
the same commands, so the window and the terminal can never disagree.

This page is the reference. The short version is in the README.

## Profiles

A profile is a saved arrangement: every lit screen with its mode, refresh
rate, position, rotation and which one is primary.

```sh
niructl display save Desk            # the layout as it is now
niructl display list                 # saved profiles
niructl display preview Desk 15      # try it; keep within 15 s or it reverts
niructl display keep TOKEN           # confirm the preview (the token is printed)
niructl display apply Desk           # switch without a countdown
niructl display rename Desk Office
niructl display delete Office
niructl display rotate DisplayPort-5 inverted
niructl display auto                 # side by side at preferred modes
```

Profiles live in `~/.config/niruwm/displays.toml`. `displays.example.toml` in
the NIRUX source shows the format.

### Screens are matched by identity

Since 0.17, saving a profile records each screen's identity from its EDID:
make, product code, model and serial number. A profile then follows its
screens, not the connector names:

- If `DP-1` and `DP-2` swap, after a reboot, a driver update or a cable moved
  in a dock, each screen still gets its own settings.
- A screen plugged into another port of the dock is still recognised.

The details:

- **Identical screens.** Identical monitors without distinct serial numbers
  are told apart by connector. The screen on the connector the profile saved
  keeps that place, and the rest are paired in connector-name order. This is
  deterministic, so the same cabling always gives the same result.
- **Screens without an EDID** (some adapters, projectors and forced outputs)
  are matched by connector, as before.
- **Older profiles.** Profiles saved before 0.17 have no identities and keep
  matching by connector, unchanged. Saving such a profile again (`niructl
  display save NAME`, or *Save as…* in NIRU Display) upgrades it in place. Its
  name, its place in the list and *Use at login* are kept.

### Screens a profile leaves off

When a profile is saved, the connected screens it does not light are
recorded as deliberately off (`[[profile.off]]`). A *Docked* profile saved
with the laptop panel off therefore still fits when the lid is open. It turns
the panel off, instead of being mistaken for a layout that lacks a screen.

### Keeping a profile out of automatic switching

`auto = false` keeps a profile out of automatic switching; it can still be
applied by hand. NIRU Display marks such a profile *manual*.

## Automatic switching

Automatic switching is off by default. Turn it on and off with:

```sh
niructl display automatic on      # also acts on the screens connected now
niructl display automatic off
niructl display automatic status [--json]
niructl display automatic run     # decide again now, as a hotplug would
```

NIRU Display has the same switch, *Automatic*, in its profile header.

With automatic switching on, the session's display watcher acts whenever the
set of connected screens changes, or the lid hides or shows the built-in
panel. The watcher is `niructl display watch`, started by `niruwm-session`.
It picks the layout in this order:

1. **A profile that fits exactly.** Every screen it names is connected, and
   no other screen is.
   - At login, the *Use at login* profile wins if it fits.
   - After that, the profile last used with these same screens wins.
   - Otherwise a profile matched by identity beats one matched only by
     connector names.
   - If several profiles still tie, nothing is guessed. The current layout is
     kept usable, and a notification asks you to pick one. The one you pick is
     remembered for next time.
2. **The last layout these screens had.** The watcher remembers the
   arrangement each set of screens last had, whether it was a profile or
   arranged by hand. Docking, undocking or opening the lid brings it back.
3. **Extend.** Keep the current layout and place each new screen to the
   right of the rightmost one, touching its edge. A screen the watcher has
   seen before gets its remembered mode, rate and rotation; a new one gets
   its preferred mode, upright. NIRU Display shows this as *Automatic layout
   — save as profile?*.

Every automatic change shows a short notification naming the profile or
layout.

The watcher never acts while a preview is pending; it decides as soon as the
preview is kept, reverted or runs out. A profile you pick by hand stays until
the next hotplug or lid change.

### The lid

On a laptop, closing the lid while another screen is connected turns the
built-in panel off. Opening it brings the panel back, through the same
choice as above, so a *Desk* profile with the panel on returns.

The watcher never leaves every screen dark. If the last external screen goes
away while the lid is closed, the panel is turned back on.

Suspend is not affected; logind decides it as before:

- With a screen connected, the default `HandleLidSwitchDocked=ignore` keeps
  the machine awake with the lid closed.
- Without one, `HandleLidSwitch=suspend` suspends it.
- The lock-before-suspend of `niruwm-session` is unchanged.

The lid state comes from `/proc/acpi/button/lid/*/state`. It is read once a
second, and only while it can matter: a panel plus another screen, or the
panel off. Machines without an ACPI lid have no lid handling.

### Verified apply, relaxing and retrying

After every automatic change the watcher reads the screens back and compares
them with what it asked for. If a screen refuses its saved mode or rate, the
watcher relaxes the layout in two steps:

1. Let the driver choose the rate.
2. Use the screen's preferred mode.

Neighbouring screens keep their positions. The full saved settings are tried
again after 10 s, then 20, 40, up to every 5 minutes. Only one apply runs at
a time. The saved profile itself is never changed. A new screen set, or a
layout you change by hand, ends the retries.

A screen that was unplugged but still holds part of the desktop (a "ghost"
area) is turned off with the next change.

### Events, not polling

The watcher sleeps until something changes. It wakes on:

- RandR screen, output and CRTC events from the X server;
- kernel `drm` uevents. Some drivers, such as amdgpu, report a lost screen
  to X only on the next query, so the watcher queries once after a uevent;
- the lid, while it matters.

An idle session costs no wakeups. The 0.16 watcher ran `xrandr` every two
seconds.

## Doctor

```sh
niructl display doctor [--json]
```

The doctor shows:

- whether automatic switching is on, and the lid state;
- every connected screen with its identity;
- which profile is current;
- for each profile: whether it fits now, and why not (`needs HP E27q G5
  (serial …2MRJ)`, `also connected: HDMI-1`, `the built-in screen (the lid is
  closed)`);
- profiles saved without identities;
- screens that cannot be told apart;
- a startup profile that does not exist or does not fit;
- ghost areas;
- the last automatic action and any pending retry.

`niructl display status --json` carries the same information for NIRU
Display (`schema` 1; the new fields are additions).

## Workspaces when a screen comes back

Each screen has its own workspaces. When a screen goes away, its windows wait
on the same workspace number of a remaining screen. When it comes back:

- it shows the workspace it showed before;
- each workspace keeps its layout and ratios;
- its windows return to it, unless you moved them meanwhile.

This follows the RandR output name for the length of the session, so a
screen that returns on another connector starts fresh.

## Files

| Path | Purpose |
|---|---|
| `~/.config/niruwm/displays.toml` | Profiles, `automatic`, `startup_profile` |
| `~/.local/state/niruwm/display-memory.toml` | Last layout per screen set, last mode per screen |
| `~/.local/state/niruwm/display-auto.toml` | Last automatic action, pending retry |
| `~/.local/state/niruwm/display-preview.toml` | A pending preview transaction |

The state files can be deleted at any time; they only hold memory.

## Limits

- **Fractional per-screen scaling, VRR and HDR.** These are not offered: X11
  has no sound per-screen fractional scaling, and VRR and HDR are driver
  properties outside the profile model.
- **Automatic rotation from an accelerometer** (2-in-1 laptops) is not
  handled.
- **Profiles are X11/RandR profiles.** They are not used by Wayland
  compositors.

## Testing without hardware

`NIRUX_DISPLAY_LID_FILE=/path/to/file` makes the watcher and `niructl
display` read `open` or `closed` from that file instead of the ACPI lid. It
is a test seam; nothing in a session sets it.

Hotplugs can be simulated as root:

- amdgpu: `echo 0 > /sys/kernel/debug/dri/N/DP-5/trigger_hotplug` unplugs,
  and `echo 1` plugs back in. This is the driver's own path: EDID read and
  uevent.
- Other drivers: write `on`, `off` or `detect` to
  `/sys/class/drm/cardN-CONN/status`, then `udevadm trigger --action=change
  --subsystem-match=drm`. A sysfs write alone sends no uevent.
  `/sys/kernel/debug/dri/N/CONN/edid_override` gives a forced screen an
  identity on i915. Write `reset` without a newline to clear it.
