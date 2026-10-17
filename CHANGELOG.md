# Changelog

## 0.2.0 — first laptop test build

The first version intended to be started from SDDM, used for a normal session
and logged out of, on a real machine.

### Added

- `session/niruwm-session`: one entry point for SDDM, `startx` and manual
  starts. Sets up D-Bus, PipeWire (Void), GTK/Qt theming, cursor, Xresources
  and the Swedish keyboard layout; reuses NIRUPI's lock, battery and
  autostart helpers when they exist and works without NIRUPI otherwise.
- `install.sh`: downloads static binaries with SHA256 verification, or builds
  locally with `--build`; `--register-session`, `--check` and `--uninstall`.
  Every installed path is recorded in a manifest and NIRUPI-owned paths are
  refused by name.
- `tests/session-smoke.sh`: end-to-end X11 test (session, bar, strut, a tiled
  client, workspace switching, clean logout, no leftover processes). Runs on
  Arch, Debian and Void in CI.
- Static `x86_64-unknown-linux-musl` release artifacts, so installing needs no
  Rust, compiler or `-dev` packages.

### Fixed

- **Bar text was clipped.** Glyph rasterisation applied the outline's pixel
  offset twice, so almost every glyph fell outside its box and the bar looked
  empty. Found by rendering the bar in Xvfb and inspecting the pixels.
- **The bar flickered.** It cleared the window and painted text in several
  `put_image` calls. It now composes each frame on a pixmap and presents it
  with a single `CopyArea`.
- **Bar clicks and `niructl` were unreliable.** Client messages must be sent
  with `SubstructureRedirectMask | SubstructureNotifyMask` on the root.
- **The bar survived logout.** `exec` replaced the session shell, so its EXIT
  trap never ran. The session now waits for the WM, stops the bar, then exits.
- **Two bars were started** in sessions: the session script and the WM both
  launched `nirubar`. The WM now only starts one when no session owns it.
- Focus is restored to a visible window when the focused one is closed or
  moved to another tag, instead of being lost.
- Keyboard mapping is cached and refreshed on `MappingNotify` instead of being
  read on every key press.
- Configuration is shared through `~/.config/niruwm/config.toml` with a `[bar]`
  section, so NIRUWM and NIRUBAR can be installed in either order. The 0.1
  path is still read.

### Changed

- `niructl` gained `movetotag`, `layout`, `quit` and persists `theme`.
- Keybindings follow NIRUPI DWM more closely: `Super+Tab` cycles workspaces,
  `Super+J/K` cycles windows, `Super+T/Y/M/C/F` set layouts,
  `Super+Ctrl+E` fibonacci, `Super+Shift+E` fullscreen.

### Known limits

- One screen. Multi-monitor geometry is modelled but not wired to RandR.
- X11 only. No swallow, scratchpad, sticky windows, rules or mouse resize.
- The bar shows tags, Swedish date and `hh:mm` only.

## 0.1.0-alpha.1

First independently implemented Rust/X11 alpha. Niru Noir is the default.
