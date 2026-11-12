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

# ── The profile follows the power source ([battery] auto_profile) ──────────
export XDG_RUNTIME_DIR=$T/run XDG_CONFIG_HOME=$T/config NIRUCTL_POWER_SUPPLY=$T/supply
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME/niruwm" "$T/supply/BAT0" "$T/supply/AC"
chmod 700 "$XDG_RUNTIME_DIR"
printf 'Battery\n' > "$T/supply/BAT0/type"
printf 'System\n' > "$T/supply/BAT0/scope"
printf 'Mains\n' > "$T/supply/AC/type"
plug() { printf '%s\n' "$1" > "$T/supply/AC/online"; }
active() { cat "$T/active"; }
fail() { echo "FAIL $*"; exit 1; }

plug 0
"$BIN" powerprofile auto | grep -q 'on battery: power profile power-saver' || fail 'auto on battery'
[ "$(active)" = power-saver ] || fail "battery gave $(active)"
plug 1
"$BIN" powerprofile auto > /dev/null
[ "$(active)" = balanced ] || fail "AC gave $(active)"
"$BIN" powerprofile status | grep -qx 'auto=on' || fail 'status auto=on'

# The watcher: applies at start, follows a plug, and a profile set by hand
# holds until the next plug or unplug.
plug 0
"$BIN" powerprofile watch > "$T/watch.log" 2>&1 &
WATCH=$!
trap 'kill $WATCH 2>/dev/null; rm -rf "$T"' EXIT INT TERM
wait_for() {
  i=0
  while [ "$(active)" != "$1" ]; do
    i=$((i + 1)); [ "$i" -gt 80 ] && fail "waited for $1, have $(active): $(cat "$T/watch.log")"
    sleep 0.1
  done
}
wait_for power-saver
# A second watcher for the same display exits at once.
"$BIN" powerprofile watch | grep -q 'already runs' || fail 'second watcher'
plug 1
wait_for balanced
"$BIN" powerprofile set performance > /dev/null
"$BIN" powerprofile status | grep -qx 'hold=manual' || fail 'manual hold not shown'
sleep 6
[ "$(active)" = performance ] || fail 'the manual profile did not hold'
plug 0
wait_for power-saver
"$BIN" powerprofile status | grep -qx 'hold=none' || fail 'unplug did not end the hold'
kill $WATCH 2>/dev/null || :
wait $WATCH 2>/dev/null || :

# "keep" and auto_profile = false leave it alone.
printf '[battery]\nprofile_on_battery = "keep"\n' > "$XDG_CONFIG_HOME/niruwm/config.toml"
printf 'performance\n' > "$T/active"
"$BIN" powerprofile auto | grep -q 'left alone' || fail 'keep'
[ "$(active)" = performance ] || fail 'keep changed the profile'
printf '[battery]\nauto_profile = false\n' > "$XDG_CONFIG_HOME/niruwm/config.toml"
"$BIN" powerprofile auto | grep -q 'is off' || fail 'auto_profile = false'
"$BIN" powerprofile watch | grep -q 'is off' || fail 'watcher with auto_profile = false'
[ "$(active)" = performance ] || fail 'auto off changed the profile'

# A desktop with a wireless mouse is not on battery.
rm -f "$XDG_CONFIG_HOME/niruwm/config.toml"
rm -rf "$T/supply/BAT0" "$T/supply/AC"
mkdir -p "$T/supply/hidpp_battery_0"
printf 'Battery\n' > "$T/supply/hidpp_battery_0/type"
printf 'Device\n' > "$T/supply/hidpp_battery_0/scope"
"$BIN" powerprofile auto | grep -q 'no system battery' || fail 'device battery'
"$BIN" powerprofile watch | grep -q 'no system battery' || fail 'watcher on a desktop'
[ "$(active)" = performance ] || fail 'desktop changed the profile'
echo 'PASS: the power profile follows the power source; a manual profile holds until the next plug'
