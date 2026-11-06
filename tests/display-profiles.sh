#!/bin/sh
# Real RandR profile lifecycle on a disposable Xvfb screen.
set -eu
BIN=${1:?usage: display-profiles.sh /absolute/path/to/niructl}
T=$(mktemp -d)
XVFB=""
cleanup() { [ -z "$XVFB" ] || kill "$XVFB" 2>/dev/null || :; rm -rf "$T"; }
trap cleanup EXIT INT TERM
export HOME=$T/home XDG_CONFIG_HOME=$T/home/.config XDG_STATE_HOME=$T/state DISPLAY=:83
mkdir -p "$XDG_CONFIG_HOME/niruwm"
Xvfb :83 -screen 0 1280x800x24 -nolisten tcp > "$T/xvfb.log" 2>&1 &
XVFB=$!
i=0
while [ ! -S /tmp/.X11-unix/X83 ]; do
  i=$((i + 1)); [ "$i" -lt 100 ] || { cat "$T/xvfb.log"; exit 1; }
  sleep 0.05
done
"$BIN" display save Test
"$BIN" display list | grep -q '^Test	1	display profile$'
"$BIN" display apply Test
TOKEN=$("$BIN" display preview Test 5 | sed -n 's/^preview-token: //p')
"$BIN" display status | grep -q '^display preview Test remaining '
"$BIN" display keep "$TOKEN"
"$BIN" display status | grep -qx 'display preview off'
"$BIN" display revert | grep -qx 'no display preview active'
grep -q '^\[\[profile\]\]' "$XDG_CONFIG_HOME/niruwm/displays.toml"
grep -q '^name = "Test"' "$XDG_CONFIG_HOME/niruwm/displays.toml"
echo 'PASS: save/list/apply/preview/status/keep/revert with validated RandR state'

# Automatic switching: off by default, on and off by command, acting on the
# connected screens at once, and reported by status and doctor.
"$BIN" display automatic status | grep -q '^automatic: off$'
"$BIN" display automatic on | grep -q 'automatic display switching on'
grep -q '^automatic = true$' "$XDG_CONFIG_HOME/niruwm/displays.toml"
"$BIN" display status --json | grep -q '"enabled": true'
"$BIN" display automatic status --json | grep -q '"action"'
"$BIN" display doctor | grep -q '^automatic switching: on$'
"$BIN" display doctor --json | grep -q '"schema": 1'
"$BIN" display automatic off | grep -q 'automatic display switching off'
if grep -q '^automatic' "$XDG_CONFIG_HOME/niruwm/displays.toml"; then
  echo "automatic = false must not be written" >&2; exit 1
fi
# A profile saved now records the screens it leaves off (none here) and keeps
# its place and "Use at login" when it is saved again.
"$BIN" display startup set Test
"$BIN" display save Test
"$BIN" display startup show | grep -q '^Test$'
echo "display automation PASS"

