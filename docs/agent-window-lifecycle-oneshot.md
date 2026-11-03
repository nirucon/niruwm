# One-shot agent assignment: window lifecycle, focus and bar reliability

## Goal and authority

Complete a root-cause fix for NIRUWM window/frame flicker and loss of NIRUBAR
workspace-click functionality after Steam, RetroArch and other transient or
fullscreen windows. Keep the NIRUX suite installable and version-consistent.
Work end to end: investigate, reproduce, implement, test, release and verify
the suite's pinned installation path. Do not stop at a diagnosis or workaround.

This document is an implementation assignment, not proof that the cause or fix
has already been established. Do not delegate without separate authorization.
Use the owner's separately supplied target/access details; no credentials,
game content or personal desktop screenshots belong in commits or issues.

You may change `nirucon/niruwm`, `nirucon/nirubar` when evidence requires it,
and `nirucon/nirux` for integration and release pins. Preserve unrelated work.
Do not restart the user's WM, log out, reboot or kill user applications without
explicit session-disruption approval. Isolated X servers are safe defaults.
If live verification is blocked, finish independent work and clearly identify
the remaining acceptance gate; never report complete hardware acceptance.

## Reported symptoms and reference evidence

- Steam desktop mode opens main, chat, webhelper, popup and overlay windows.
  Borders flash, focus seems unstable and windows sometimes jump.
- RetroArch/fullscreen entry and exit can produce similar symptoms.
- Afterward, workspace buttons in NIRUBAR sometimes stop responding to clicks.
- A previous inspection of the accumulated session log counted 169,406
  `niruwm: X11 error` records, including `error_kind: Window`, error code 3,
  repeated `bad_value` window IDs and requests `ConfigureWindow`,
  `SetInputFocus`, `ChangeProperty`, `ChangeWindowAttributes`.
- Reference versions at observation: NIRUWM 0.16.161, NIRUBAR 0.18.76;
  Debian 13 amd64, X11, AMD RADV/Mesa, 1920x1200 at 60 Hz,
  picom 12.5 using xrender with shadows and inactive opacity.
- Reference log: `${XDG_STATE_HOME:-$HOME/.local/state}/niruwm/session.log`.
  Accumulated errors alone do NOT establish current frequency or causation.

Possible mechanisms to distinguish, not assumptions to encode:

1. Dead clients/focus references surviving destroy/unmap races.
2. Legitimate asynchronous BadWindow races, versus persistent stale state.
3. Configure/map/unmap/focus feedback loops with Steam's helper windows.
4. Incorrect handling of transients, override-redirect, input-only, dock,
   notification or unmapped/non-viewable windows.
5. Pointer/button grabs, stale drag state, fullscreen stacking over the bar,
   or bar hit-testing/IPC failures independent of WM client cleanup.
6. Picom shadow/opacity repainting amplifying an underlying focus loop.

Existing mitigations include floating app rules, picom shadow exclusions and
gamescope wrapping Steam. They are NOT an acceptance substitute. Earlier
statements that gamescope guarantees prevention were too strong: test it.

## 1. Inventory and controlled reproduction

- Read repository instructions and current upstream code, tests and release
  workflows. Record exact source commits and installed binary versions.
- Inspect live window properties and tree, `_NET_CLIENT_LIST`,
  `_NET_ACTIVE_WINDOW`, mapping/viewability, WM_STATE, transient/type hints,
  stacking and bar/input geometry. Resolve each relevant XID at failure time.
- Collect bounded timestamped event/error traces and process/CPU/log-growth
  measurements. Avoid full desktop captures containing personal information.
- Reproduce first in Xephyr/Xvfb using a synthetic X11 client, not game files.
  Drive rapid create/map/configure/unmap/remap/destroy, independent client
  disconnects, popup families, fullscreen toggles and workspace switches.
- Test picom on/off separately and physical pointer versus synthetic clicks.
  Compare keyboard workspace switching with actual bar-button clicks.
- Establish a failing regression on the baseline before claiming causality.

Useful starting points (verify against current upstream):

- `src/wm/events.rs`: event loop, Error, DestroyNotify, UnmapNotify,
  ConfigureRequest, focus/enter events and client messages.
- `src/wm/clients.rs`: manage/unmanage, set_focus/focus_client, border updates,
  arrange/stacking and desktop state publication.
- `src/wm/mouse.rs`: grabs and drag cleanup.
- `src/wm/mod.rs`: event subscriptions, bar ownership and restart paths.
- `tests/x11.rs`, `tests/replay/`, `.github/workflows/ci.yml`.
- NIRUBAR's workspace hit-testing, event delivery and WM command path.

## 2. Implement a principled lifecycle fix

- Make cleanup idempotent across duplicate destroy/unmap and client exits.
  Remove stale client, focus, hidden/withdrawn, fullscreen, drag/grab,
  scratchpad and stacking references where applicable; publish consistent
  EWMH/ICCCM state afterward.
- Preserve legitimate minimize/tray-hide/remap and workspace-hide behavior.
  Do not treat all UnmapNotify as destruction or drop hidden live clients.
- Focus only appropriate live/viewable clients, honoring input hints and
  WM_TAKE_FOCUS. Provide a safe fallback when the target vanishes mid-operation.
- Understand x11rb asynchronous request/error semantics. An X11 error's
  `bad_value` is NOT universally a client XID; interpret opcode/error context
  before removing anything. Avoid synchronous round trips in every hot path.
- Handle expected BadWindow races locally and boundedly without hiding real
  protocol/connection failures. Removing logging alone is not a fix.
- Avoid recursive arrange/focus storms, excessive border writes, and raising
  every floating window on unrelated events. Measure before/after.
- Prove bar clicks reach the correct target and result in workspace changes;
  distinguish input grabs/occlusion from bar rendering and IPC defects.
- Fix generic behavior instead of hardcoding Steam/RetroArch names. Keep dock,
  fullscreen, transient and override-redirect semantics correct.
- Do not introduce repeated touchpad resets, broad process killing or a
  wineserver shutdown into generic window-management cleanup.

## 3. Automated acceptance and regression tests

Add deterministic real-X11 tests with bounded waits and guaranteed cleanup.
Run them as a non-root disposable user, on an isolated display. Include:

1. Hundreds of transient/helper lifecycle cycles with workspace churn and
   fullscreen enter/exit; destroy focused/unfocused/hidden windows and close
   an entire client connection with children still mapped.
2. Destroy between request and processing, duplicate notifications, and
   hide/remap/minimize/tray cycles. No disappearing live clients.
3. `_NET_CLIENT_LIST` has no destroyed XIDs and `_NET_ACTIVE_WINDOW` is valid
   or cleared. Focus and borders settle, CPU/log growth return to baseline.
4. Real NIRUBAR on the same display: click workspace buttons before, during
   and after the lifecycle stress. Verify active tag and visible client set,
   not merely that a click was sent or a process is alive.
5. Input works after fullscreen exit: bar clicks, application clicks,
   keyboard focus and mouse drag. No lingering grabs/occluding overlays.
6. Multi-monitor/hotplug where supported, sticky/floating/tiled clients,
   scratchpads, dialogs and session restore; no unrelated layout regressions.
7. Picom on/off comparison where available; compositor absence remains valid.

Run the existing relevant cargo/unit/X11, replay, session/config, installer
and release-source checks. Respect the declared Rust baseline, lockfile and
supported architecture/distro CI matrix. Do not silently raise MSRV.

## 4. Real-session validation

Only on an explicitly authorized desktop, after unsaved-work approval:

- Steam desktop main/chat/friends popups, then Big Picture both directly and
  via gamescope; open/close repeatedly and switch workspaces using NIRUBAR.
- RetroArch menu -> content -> menu -> close content -> exit; windowed and
  fullscreen. Existing legally owned content is optional, not bundled.
- Check border/focus stability, pointer/touchpad clicks, workspace switching,
  fullscreen exit, and normal apps after at least 20 cycles.
- Validate audio, theme switching, notifications, clipboard, screenshot,
  lock/screensaver and power controls as suite smoke checks. Do not alter
  idle/suspend policy or relax security to pass tests.
- Capture fresh bounded log intervals and compare baseline/patched behavior.
  A handful of handled destruction races can be legitimate; sustained stale
  XID error storms or dead workspace buttons fail acceptance.

## 5. Coordinated versions, release and installation

Component versions need compatible pins, NOT identical version numbers.

1. Fetch current versions first; do not blindly reuse the reference numbers.
   Bump only changed components following each repo's release conventions.
2. NIRUWM: align Cargo.toml/Cargo.lock, binary --version, release tag/workflow,
   source archives and generated assets. Update `nirubar-version` if required
   by a new bar dependency. Run release-source validation.
3. NIRUBAR: if changed, release and verify its assets first, then pin the
   compatible released version in NIRUWM/NIRUX. If unchanged, verify the
   existing pin and document why no release was needed.
4. NIRUX: inspect `manifest`, installer/updater and generation ownership.
   Pin the actual released WM/bar versions and any hashes/asset metadata.
   Bump the NIRUX suite version according to its conventions. Preserve other
   suite pins unless a real compatibility change requires updating them.
5. Release in dependency order. Check GitHub CI and artifact availability;
   do not publish suite pins pointing to nonexistent/failed release assets.
6. Test a fresh NIRUX install and an update from the prior release in isolated
   targets. Verify symlinks, generation manifest, PATH and installed --version
   results. Ensure the updater does not replace the fix with an older binary.
7. Preserve user config/theme hooks, launchers, profiles and permissions.
   Document necessary migration without overwriting custom configs.
8. For owner-approved live deployment use NIRUX's normal generation/update
   mechanism; keep the prior working generation and documented rollback.
   A temporary standalone binary is not a completed suite deployment.

Do not patch unrelated suite components solely to bump versions. If another
component really needs changing, explain the interface/dependency involved
and extend the compatibility matrix and tests accordingly.

## 6. Deliverables and completion gates

- Root cause supported by baseline reproduction and a passing regression.
- Reviewed, scoped patches and tests in the relevant repositories.
- Documented version/commit/pin matrix for WM, bar, NIRUX and unchanged suite
  components used in smoke tests; working release artifacts and CI links.
- Fresh install/update verification, and authorized live-session results.
- Recovery/rollback instructions and explicit remaining hardware limitations.
- A concise final report distinguishing proven causes, mitigations, tests
  actually run, and anything still unverified. No claims of "fully fixed"
  based only on float rules, suppressed logs or gamescope.

Suggested invocation:

> Execute docs/agent-window-lifecycle-oneshot.md end to end. Fix the underlying
> lifecycle/focus/bar-input defect and coordinate verified releases and NIRUX
> pins. Authorized targets, access and permitted disruption: [supplied here].
