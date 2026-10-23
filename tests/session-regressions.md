# Session and wallpaper regression checks — 2026-10-23

## Reproduced failure

The released 0.16.75 session script was run through `session-startup.sh` as
UID 1000 in `voidlinux/voidlinux:latest`, without `XDG_RUNTIME_DIR` or a login
runtime directory. It terminated before invoking the WM:

```text
mkdir: cannot create directory '/run/user': Permission denied
```

The existing root-run session tests and pre-created runtime directories masked
this failure. The fixed script establishes a private runtime directory before
D-Bus/audio, logs startup from the beginning, and preserves nonzero WM exit
status. Optional `.Xresources` failures no longer end the session.

## Verified locally

Both new scripts passed as UID/GID 1000 in the existing `debtest`, `archtest`
and `voidtest` containers (Debian bookworm, Arch and Void). The X tests used
current source builds of NIRUWM/niructl with the release musl target and pinned
NIRUBAR 0.18.35, real startx/xinit, Xvfb and feh.

- No login runtime: reaches the WM with a user-owned mode-700 directory.
- Valid login runtime: preserved.
- WM exit 42: returned and recorded in the session log.
- Failed optional xrdb: WM still starts.
- A real WM and bar start through `.xinitrc`, and quit cleanly.
- Theme changes replace the root pixmap using images found in an XDG Pictures
  path containing spaces, despite a stale English `theme_dir` in the config.
- Manual single-file selection remains an override across theme changes.
- Explicit `wallpaper follow-theme on` exits that override, retains the manual
  file in the config and applies the selected theme wallpaper.
- Failed setters leave the previous wallpaper intact and log the tool errors.

The distro CI matrix now runs these regressions as a disposable unprivileged
account, in addition to the existing session/keyboard checks.
`wallpaper-session.sh` takes a directory
containing `nirux-wallpaper-noir-4k.png` and `nirux-wallpaper-nord-4k.png`.

```sh
sh tests/session-startup.sh
sh tests/wallpaper-session.sh /path/to/binaries /path/to/nirux/wallpaper
```

## Still requiring the reported VMs

No SSH access to the reported Void/Arch VMs was available for these checks.
The actual Arch config has not been obtained: a single-file override is a
reproduced defect in the explicit re-enable command, not a confirmed diagnosis
of that VM. Inspect `niructl wallpaper status`, the running WM version and the
session log on that VM after deploying the fixes. Also confirm a physical/VM
TTY login on Void and continued LightDM/SDDM logins; Xvfb does not exercise
their PAM/login setup or GPU drivers.

These changes are local source changes, not a new published suite release.
