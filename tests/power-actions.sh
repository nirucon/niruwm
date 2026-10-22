#!/bin/sh
# Never calls a real power backend: each niructl process has a private PATH.
set -eu
BIN=${1:?usage: power-actions.sh /absolute/path/to/niructl}
BIN=$(CDPATH='' cd -- "$(dirname "$BIN")" && pwd)/$(basename "$BIN")
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
mkdir -p "$T/bin" "$T/home"
export HOME=$T/home POWER_CALLS=$T/calls XDG_SESSION_ID=42 USER=nirutest
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
cat > "$T/bin/dbus-send" <<'EOF'
#!/bin/sh
printf 'dbus-send %s\n' "$*" >> "$POWER_CALLS"
if [ "${DENY_POWER:-0}" = 1 ]; then
  printf 'org.freedesktop.DBus.Error.AccessDenied: denied or inhibited\n' >&2
  exit 1
fi
EOF
cat > "$T/bin/systemctl" <<'EOF'
#!/bin/sh
printf 'systemctl %s\n' "$*" >> "$POWER_CALLS"
EOF
cat > "$T/bin/loginctl" <<'EOF'
#!/bin/sh
printf 'loginctl %s\n' "$*" >> "$POWER_CALLS"
if [ "${1:-}" = --help ]; then
  printf 'Session Commands:\n  terminate-session ID\n'
  [ "${ELOGIND:-0}" != 1 ] || printf '  poweroff\n  reboot\n  suspend\n'
  exit 0
fi
case "${1:-}" in
  terminate-session|terminate-user) exit 0 ;;
esac
if [ "${ELOGIND:-0}" != 1 ]; then
  printf "Unknown command verb '%s'.\n" "$1" >&2
  exit 1
fi
EOF
chmod +x "$T/bin/"*

# Both systemd (Debian/Arch) and elogind (Void) implement the login1 API.
for provider in systemd elogind; do
  : > "$POWER_CALLS"
  ELOGIND=0
  [ "$provider" != elogind ] || ELOGIND=1
  export ELOGIND
  for action in shutdown reboot; do
    env PATH="$T/bin" "$BIN" "$action"
  done
  grep -qxF 'dbus-send --system --print-reply --reply-timeout=120000 --dest=org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager.PowerOff boolean:true' "$POWER_CALLS" || fail "$provider PowerOff request"
  grep -qF 'org.freedesktop.login1.Manager.Reboot boolean:true' "$POWER_CALLS" || fail "$provider Reboot request"
  [ "$(wc -l < "$POWER_CALLS")" = 2 ] || fail 'unexpected CLI power fallback'

  # Authorization/inhibitor errors must not be retried or bypassed.
  : > "$POWER_CALLS"
  if env PATH="$T/bin" DENY_POWER=1 "$BIN" shutdown > "$T/out" 2> "$T/error"; then
    fail 'denied shutdown reported success'
  fi
  grep -q 'AccessDenied' "$T/error" || fail 'backend error was lost'
  [ "$(wc -l < "$POWER_CALLS")" = 1 ] || fail 'denied shutdown retried through another backend'
done

# Logout remains a session operation, never a machine-wide power request.
: > "$POWER_CALLS"
env PATH="$T/bin" "$BIN" logout
grep -qxF 'loginctl terminate-session 42' "$POWER_CALLS" || fail 'wrong logout target'
[ "$(wc -l < "$POWER_CALLS")" = 1 ] || fail 'logout made extra backend requests'

# Suspend retains the lock-readiness guard, even when the power API is present.
: > "$POWER_CALLS"
if env PATH="$T/bin" DISPLAY= "$BIN" suspend > "$T/out" 2> "$T/error"; then
  fail 'suspend proceeded without a verified lock'
fi
[ ! -s "$POWER_CALLS" ] || fail 'suspend requested power before locking'

# Minimal systemd setup without dbus-send uses systemctl, NOT loginctl.
rm "$T/bin/dbus-send"
: > "$POWER_CALLS"
env PATH="$T/bin" "$BIN" shutdown
env PATH="$T/bin" "$BIN" reboot
grep -qxF 'systemctl poweroff' "$POWER_CALLS" || fail 'missing systemctl poweroff fallback'
grep -qxF 'systemctl reboot' "$POWER_CALLS" || fail 'missing systemctl reboot fallback'
[ "$(wc -l < "$POWER_CALLS")" = 2 ] || fail 'systemd fallback called loginctl'

# A loginctl-only elogind setup is accepted only if it advertises power verbs.
rm "$T/bin/systemctl"
: > "$POWER_CALLS"
env PATH="$T/bin" ELOGIND=1 "$BIN" shutdown
grep -qxF 'loginctl --help' "$POWER_CALLS" || fail 'elogind capability not checked'
grep -qxF 'loginctl poweroff' "$POWER_CALLS" || fail 'elogind fallback missing'
: > "$POWER_CALLS"
if env PATH="$T/bin" ELOGIND=0 "$BIN" shutdown > "$T/out" 2> "$T/error"; then
  fail 'systemd loginctl-only setup accepted an unsupported power verb'
fi
grep -q 'no power backend' "$T/error" || fail 'missing actionable backend error'
[ "$(wc -l < "$POWER_CALLS")" = 1 ] || fail 'unsupported loginctl power verb attempted'
printf 'PASS: systemd/elogind login1 power API, safe CLI fallbacks, no retry on rejection, logout and suspend lock guard\n'
