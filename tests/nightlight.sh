#!/bin/sh
# Night light must actually change the display, and must restore it.
#
# The bug this pins: `niructl nightlight on` passed `gammastep -O K`, which
# recomputes the temperature from the sun and silently ignores the requested
# value during the day. On a QXL display the gamma stayed at 1.0:1.0:1.0 while
# the command reported success. A real display change needs `-t K:K`, which also
# means the location has to be supplied or gammastep refuses with a GeoClue2
# error.
#
# Needs an X server with RandR gamma support. Without one the check is reported
# as not-run rather than passed.
set -eu
BIN=${1:?usage: nightlight.sh BINDIR}

command -v xrandr >/dev/null 2>&1 || { printf 'nightlight: not-run (no xrandr)\n'; exit 0; }
[ -n "${DISPLAY:-}" ] || { printf 'nightlight: not-run (no DISPLAY)\n'; exit 0; }
command -v gammastep >/dev/null 2>&1 || { printf 'nightlight: not-run (gammastep absent)\n'; exit 0; }

gamma_now() {
    xrandr --verbose 2>/dev/null | sed -n 's/^[[:space:]]*Gamma: *//p' | head -n1
}

output=$(xrandr 2>/dev/null | sed -n 's/^\([A-Za-z0-9-]*\) connected.*/\1/p' | head -n1)
[ -n "$output" ] || { printf 'nightlight: not-run (no connected output)\n'; exit 0; }

TEST=$(mktemp -d)
trap 'rm -rf "$TEST"; xrandr --output "$output" --gamma 1.0:1.0:1.0 >/dev/null 2>&1 || :' EXIT HUP INT TERM
export HOME="$TEST/home" XDG_CONFIG_HOME="$TEST/home/.config"
mkdir -p "$XDG_CONFIG_HOME/niruwm"
cat > "$XDG_CONFIG_HOME/niruwm/config.toml" <<'EOF'
[nightlight]
latitude = 58.22
longitude = 11.93
temperature = 3400
EOF

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

xrandr --output "$output" --gamma 1.0:1.0:1.0 >/dev/null 2>&1 || {
    printf 'nightlight: not-run (display refuses a gamma change)\n'; exit 0;
}
baseline=$(gamma_now)

"$BIN/niructl" nightlight on >/dev/null 2>&1 ||
    fail 'nightlight on reported an error'
sleep 1
after=$(gamma_now)
[ "$after" != "$baseline" ] ||
    fail "nightlight on did not change the display gamma (still $after); \
gammastep needs -t K:K and -l, not -O"

status=$("$BIN/niructl" nightlight status 2>&1 || true)
printf '%s\n' "$status" | grep -q 'current: 3400' ||
    fail "status does not report the applied temperature: $status"

"$BIN/niructl" nightlight off >/dev/null 2>&1 || fail 'nightlight off reported an error'
sleep 1
restored=$(gamma_now)
[ "$restored" = "$baseline" ] ||
    fail "nightlight off did not restore the gamma ($restored, expected $baseline)"

printf 'nightlight: ok\n'
