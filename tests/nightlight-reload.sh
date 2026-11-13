#!/bin/sh
# The night-light watcher follows edits to config.toml without a new login
# (NIRUWM 0.30.0): a new temperature applies within seconds, a broken file
# keeps the last good settings, manual mode leaves the gamma alone, and a
# switch from manual back to a schedule works. The gradual mode is checked
# at fixed times through NIRUCTL_NIGHTLIGHT_NOW.
#
# Needs an X server with RandR gamma support. Without one the check is reported
# as not-run rather than passed.
set -eu
BIN=${1:?usage: nightlight-reload.sh BINDIR}

command -v xrandr >/dev/null 2>&1 || { printf 'nightlight-reload: not-run (no xrandr)\n'; exit 0; }
[ -n "${DISPLAY:-}" ] || { printf 'nightlight-reload: not-run (no DISPLAY)\n'; exit 0; }
command -v gammastep >/dev/null 2>&1 || { printf 'nightlight-reload: not-run (gammastep absent)\n'; exit 0; }
output=$(xrandr 2>/dev/null | sed -n 's/^\([A-Za-z0-9-]*\) connected.*/\1/p' | head -n1)
[ -n "$output" ] || { printf 'nightlight-reload: not-run (no connected output)\n'; exit 0; }

TEST=$(mktemp -d)
WATCH=
cleanup() {
    [ -z "$WATCH" ] || kill "$WATCH" 2>/dev/null || :
    rm -rf "$TEST"
    xrandr --output "$output" --gamma 1.0:1.0:1.0 >/dev/null 2>&1 || :
}
trap cleanup EXIT HUP INT TERM
export HOME="$TEST/home" XDG_CONFIG_HOME="$TEST/home/.config" XDG_STATE_HOME="$TEST/home/.local/state"
# The expected sunset and sunrise times are Stockholm local time.
export TZ=Europe/Stockholm
CONFIG="$XDG_CONFIG_HOME/niruwm/config.toml"
STATE="$XDG_STATE_HOME/niruwm/nightlight"
LOG="$TEST/watch.log"
mkdir -p "$XDG_CONFIG_HOME/niruwm"

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    [ ! -f "$LOG" ] || sed 's/^/  log: /' "$LOG" >&2
    exit 1
}

# on = off means the fixed schedule is on all day, whatever the time is.
write() {
    printf '[nightlight]\nlatitude = 59.33\nlongitude = 18.07\nmode = "%s"\ntemperature = %s\non = "12:00"\noff = "12:00"\n' "$1" "$2" > "$CONFIG.new"
    mv "$CONFIG.new" "$CONFIG"
}
shown() { cat "$STATE" 2>/dev/null || printf 'off'; }
# Wait up to 12 s (the watcher checks every 5 s) for the state to become $1.
wait_for() {
    i=0
    while [ "$i" -lt 24 ]; do
        [ "$(shown)" = "$1" ] && return 0
        sleep 0.5
        i=$((i + 1))
    done
    return 1
}

# Xvfb applies gammastep only once a gamma ramp has been set.
xrandr --output "$output" --gamma 1.0:1.0:1.0 >/dev/null 2>&1 || {
    printf 'nightlight-reload: not-run (display refuses a gamma change)\n'; exit 0;
}

write fixed 3000
"$BIN/niructl" nightlight watch >"$LOG" 2>&1 &
WATCH=$!
wait_for 3000 || fail "the watcher did not apply 3000 K at start (shows $(shown))"

write fixed 4000
wait_for 4000 || fail "an edited temperature was not applied within seconds (shows $(shown))"

printf '[nightlight]\nmode = "sometimes"\n' > "$CONFIG"
sleep 7
[ "$(shown)" = 4000 ] || fail "a broken config changed the night light (shows $(shown))"
grep -q 'keeping the last good settings' "$LOG" || fail "a broken config was not logged"

write manual 2000
sleep 7
[ "$(shown)" = 4000 ] || fail "manual mode touched the gamma (shows $(shown))"
kill -0 "$WATCH" 2>/dev/null || fail "the watcher stopped in manual mode"

write fixed 5000
wait_for 5000 || fail "manual -> fixed did not apply without a new login (shows $(shown))"
kill "$WATCH" 2>/dev/null || :
wait "$WATCH" 2>/dev/null || :
WATCH=

# Gradual at fixed times, Stockholm in December (sunset about 14:50).
printf '[nightlight]\nlatitude = 59.33\nlongitude = 18.07\n' > "$CONFIG"
status_at() { NIRUCTL_NIGHTLIGHT_NOW="$1" "$BIN/niructl" nightlight status 2>&1; }
s=$(status_at "2026-12-10 20:30")
printf '%s\n' "$s" | grep -q '^mode: gradual$' || fail "gradual is not the default: $s"
printf '%s\n' "$s" | grep -q 'start 19:00 .*at 4500 K' || fail "winter start is not 19:00: $s"
printf '%s\n' "$s" | grep -q '^deepest: 22:00 at 2700 K' || fail "deepest is not 22:00: $s"
printf '%s\n' "$s" | grep -q '^morning: 06:30 - 07:00' || fail "winter morning does not end at 07:00: $s"
printf '%s\n' "$s" | grep -q '^scheduled: 3600 K (night)' || fail "20:30 is not halfway: $s"
s=$(status_at "2026-06-20 12:00")
printf '%s\n' "$s" | grep -q '^scheduled: 6500 K (day)' || fail "midsummer noon is not day: $s"
printf '%s\n' "$s" | grep -Eq 'start 22:[0-9][0-9] \(sunset' || fail "midsummer start is not at sunset: $s"
NIRUCTL_NIGHTLIGHT_NOW="2026-12-10 21:00" "$BIN/niructl" nightlight apply >/dev/null 2>&1 ||
    fail "nightlight apply failed in gradual mode"
[ "$(shown)" = 3300 ] || fail "gradual apply at 21:00 shows $(shown), expected 3300"
# Super+N during the glide switches off, and the override is reported.
NIRUCTL_NIGHTLIGHT_NOW="2026-12-10 21:00" "$BIN/niructl" nightlight toggle >/dev/null 2>&1 ||
    fail "toggle failed in gradual mode"
[ "$(shown)" = off ] || fail "toggle during the glide did not switch off (shows $(shown))"
s=$(status_at "2026-12-10 21:30")
printf '%s\n' "$s" | grep -q '^override: off' || fail "the override is not shown: $s"
NIRUCTL_NIGHTLIGHT_NOW="2026-12-10 21:30" "$BIN/niructl" nightlight apply >/dev/null 2>&1 || :
[ "$(shown)" = off ] || fail "the override did not hold during the night (shows $(shown))"
# The next morning the override from the night is over.
NIRUCTL_NIGHTLIGHT_NOW="2026-12-11 12:00" "$BIN/niructl" nightlight apply >/dev/null 2>&1 || :
[ "$(shown)" = 6500 ] || fail "the day did not end the override (shows $(shown))"

printf 'nightlight-reload: ok\n'
