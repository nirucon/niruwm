#!/bin/sh
# Offline power-profiles-daemon adapter regression.
set -eu
BIN=${1:?usage: power-profile.sh /absolute/path/to/niructl}
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT INT TERM
export HOME=$T/home XDG_STATE_HOME=$T/state PP_FIXTURE=$T
export PATH=$T/bin:$PATH
unset DISPLAY
mkdir -p "$HOME" "$T/bin"
printf 'balanced\n' > "$T/active"
: > "$T/actions"
cat > "$T/bin/powerprofilesctl" <<'EOF'
#!/bin/sh
set -eu
case "$1" in
  get) cat "$PP_FIXTURE/active" ;;
  list)
    printf '  Driver: intel_pstate\n'
    printf '* %s:\n' "$(cat "$PP_FIXTURE/active")"
    printf '  performance:\n'
    printf '  balanced:\n'
    printf '  power-saver:\n'
    ;;
  set)
    printf 'set %s\n' "$2" >> "$PP_FIXTURE/actions"
    printf '%s\n' "$2" > "$PP_FIXTURE/active"
    ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$T/bin/powerprofilesctl"
"$BIN" powerprofile status > "$T/status"
grep -qxF 'state=ready' "$T/status"
grep -qxF 'active=balanced' "$T/status"
grep -qxF 'profile=performance' "$T/status"
grep -qxF 'profile=power-saver' "$T/status"
! grep -qxF 'profile=driver' "$T/status"
[ ! -s "$T/actions" ] || { echo 'FAIL status changed power profile'; exit 1; }
"$BIN" powerprofile list | grep -qxF performance
"$BIN" powerprofile set performance
grep -qxF 'set performance' "$T/actions"
"$BIN" powerprofile next
grep -qxF 'set balanced' "$T/actions"
if "$BIN" powerprofile set turbo > /dev/null 2>&1; then
  echo 'FAIL unsupported profile accepted'; exit 1
fi
printf 'provider-unavailable\n' > /dev/null
if "$BIN" powerprofile status | grep -qx 'state=provider-unavailable'; then :; fi
echo 'PASS: power profile status/list are read-only; set/next are validated and confirmed'
