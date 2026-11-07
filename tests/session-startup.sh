#!/bin/sh
# Run as an ordinary user, including inside a minimal Void container. No X
# server needed: substitute the WM and optional helpers, exercise real startup.
set -eu
SOURCE=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TEST=$(mktemp -d)
trap 'rm -rf "$TEST"' EXIT HUP INT TERM
export HOME="$TEST/home" XDG_CONFIG_HOME="$TEST/home/.config"
export XDG_CACHE_HOME="$TEST/home/.cache" XDG_STATE_HOME="$TEST/home/.local/state"
export DISPLAY=:99 DBUS_SESSION_BUS_ADDRESS=test-session-bus
unset XDG_RUNTIME_DIR
mkdir -p "$TEST/bin" "$XDG_CONFIG_HOME/niruwm"
cp "${1:-$SOURCE/session/niruwm-session}" "$TEST/bin/niruwm-session"
cat > "$TEST/bin/niruwm" <<'EOF'
#!/bin/sh
printf '%s\n' "$XDG_RUNTIME_DIR" > "$HOME/wm-runtime"
exit "${TEST_WM_EXIT:-0}"
EOF
cat > "$TEST/bin/noop" <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$TEST/bin/pgrep" <<'EOF'
#!/bin/sh
exit 1
EOF
chmod +x "$TEST/bin/"*
for tool in pipewire wireplumber pipewire-pulse sleep pkill xsetroot setxkbmap xrdb dbus-update-activation-environment; do
  ln -s noop "$TEST/bin/$tool"
done
cat > "$XDG_CONFIG_HOME/niruwm/session.conf" <<'EOF'
START_PICOM=0
START_IDLE=0
START_SLEEP_LOCK=0
NOTIFICATION_PROVIDER=external
EOF
export PATH="$TEST/bin:/usr/bin:/bin"
"$TEST/bin/niruwm-session"
[ "$(cat "$HOME/wm-runtime")" = "$XDG_CACHE_HOME/niruwm/runtime" ]
[ "$(stat -c '%u:%a' "$XDG_CACHE_HOME/niruwm/runtime")" = "$(id -u):700" ]
grep -q 'session startup' "$XDG_STATE_HOME/niruwm/session.log"
printf 'PASS: no login runtime directory still reaches WM\n'

mkdir -m 700 "$HOME/provided-runtime"
XDG_RUNTIME_DIR="$HOME/provided-runtime" "$TEST/bin/niruwm-session"
[ "$(cat "$HOME/wm-runtime")" = "$HOME/provided-runtime" ]
printf 'PASS: valid login runtime directory is preserved\n'

status=0
TEST_WM_EXIT=42 "$TEST/bin/niruwm-session" || status=$?
[ "$status" = 42 ]
grep -q 'session exited with status 42' "$XDG_STATE_HOME/niruwm/session.log"
printf 'PASS: WM failure status is preserved and logged\n'

# Errors in optional X resources must not tear down the desktop.
printf 'invalid resource\n' > "$HOME/.Xresources"
rm "$TEST/bin/xrdb"
ln -s pgrep "$TEST/bin/xrdb"
"$TEST/bin/niruwm-session"
grep -q 'could not load optional .Xresources' "$XDG_STATE_HOME/niruwm/session.log"
printf 'PASS: optional xrdb failure does not terminate session\n'

# A session killed without its cleanup leaves niruidle reparented to init.
# The next session must stop the one for this display, or two ladders compete
# and lock the screen again right after an unlock.
cat > "$TEST/bin/pgrep" <<'EOF'
#!/bin/sh
case "$*" in
  *"-x niruidle"*) if [ -f "$HOME/stale-pid" ]; then cat "$HOME/stale-pid"; exit 0; fi ;;
esac
exit 1
EOF
chmod +x "$TEST/bin/pgrep"
/bin/sleep 60 & STALE=$!
printf '%s\n' "$STALE" > "$HOME/stale-pid"
"$TEST/bin/niruwm-session"
if kill -0 "$STALE" 2>/dev/null; then kill "$STALE" 2>/dev/null || :; echo "FAIL stale niruidle survived"; exit 1; fi
grep -q 'stopping stale niruidle' "$XDG_STATE_HOME/niruwm/session.log"
printf 'PASS: stale idle daemon from an earlier session is stopped\n'
rm -f "$HOME/stale-pid"
cat > "$TEST/bin/pgrep" <<'EOF'
#!/bin/sh
exit 1
EOF
chmod +x "$TEST/bin/pgrep"

# An installed NIRUDICTATE extra is started with the session (it decides
# itself whether autostart is on) and is a tracked helper like the others.
cat > "$TEST/bin/nirudictate" <<'EOF2'
#!/bin/sh
printf '%s\n' "$*" > "$HOME/nirudictate-args"
EOF2
cp "$TEST/bin/niruwm" "$TEST/niruwm.plain"
cat > "$TEST/bin/niruwm" <<'EOF2'
#!/bin/sh
i=0
while [ ! -f "$HOME/nirudictate-args" ] && [ "$i" -lt 100 ]; do /bin/sleep 0.05; i=$((i + 1)); done
exit 0
EOF2
chmod +x "$TEST/bin/nirudictate" "$TEST/bin/niruwm"
rm -f "$HOME/nirudictate-args"
"$TEST/bin/niruwm-session"
[ "$(cat "$HOME/nirudictate-args" 2>/dev/null)" = autostart ] || { echo "FAIL nirudictate was not started"; exit 1; }
printf 'PASS: an installed NIRUDICTATE is started with the session\n'
rm -f "$TEST/bin/nirudictate" "$HOME/nirudictate-args"
cp "$TEST/niruwm.plain" "$TEST/bin/niruwm"

# A crashed window manager is started again; the session (and every open
# program) goes on. The restarted manager is told so it does not start the
# saved session a second time.
cat > "$TEST/bin/xprop" <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$TEST/bin/niruwm" <<'EOF'
#!/bin/sh
n=$(cat "$HOME/wm-runs" 2>/dev/null || echo 0)
n=$((n + 1))
printf '%s\n' "$n" > "$HOME/wm-runs"
printf '%s\n' "${NIRUWM_RESTARTED:-}" >> "$HOME/wm-restarted"
[ "$n" -ge 3 ] && exit 0
kill -SEGV $$
EOF
chmod +x "$TEST/bin/xprop" "$TEST/bin/niruwm"
rm -f "$HOME/wm-runs" "$HOME/wm-restarted"
"$TEST/bin/niruwm-session"
[ "$(cat "$HOME/wm-runs")" = 3 ] || { echo "FAIL crashed WM was not restarted ($(cat "$HOME/wm-runs"))"; exit 1; }
[ "$(sed -n 1p "$HOME/wm-restarted")" = "" ] && [ "$(sed -n 3p "$HOME/wm-restarted")" = 1 ] ||
  { echo "FAIL NIRUWM_RESTARTED not passed"; exit 1; }
grep -q 'niruwm stopped with status 139 (crash 1 of at most 3' "$XDG_STATE_HOME/niruwm/session.log"
printf 'PASS: a crashed WM is restarted and the session continues\n'

# A manager that crashes again and again is given up on.
cat > "$TEST/bin/niruwm" <<'EOF'
#!/bin/sh
n=$(cat "$HOME/wm-runs" 2>/dev/null || echo 0)
printf '%s\n' "$((n + 1))" > "$HOME/wm-runs"
exit 101
EOF
chmod +x "$TEST/bin/niruwm"
rm -f "$HOME/wm-runs"
status=0
"$TEST/bin/niruwm-session" || status=$?
[ "$status" = 101 ] && [ "$(cat "$HOME/wm-runs")" = 4 ] ||
  { echo "FAIL crash loop: status $status runs $(cat "$HOME/wm-runs")"; exit 1; }
grep -q 'stopped with status 101 4 times within 60s; ending the session' "$XDG_STATE_HOME/niruwm/session.log"
printf 'PASS: a crash loop ends the session after the limit\n'

# A deliberate stop (TERM) is not a crash.
cat > "$TEST/bin/niruwm" <<'EOF'
#!/bin/sh
n=$(cat "$HOME/wm-runs" 2>/dev/null || echo 0)
printf '%s\n' "$((n + 1))" > "$HOME/wm-runs"
kill -TERM $$
EOF
chmod +x "$TEST/bin/niruwm"
rm -f "$HOME/wm-runs"
status=0
"$TEST/bin/niruwm-session" || status=$?
[ "$(cat "$HOME/wm-runs")" = 1 ] || { echo "FAIL TERM restarted the WM"; exit 1; }
printf 'PASS: a WM stopped with TERM is not restarted\n'
rm -f "$TEST/bin/xprop"

# The session log is rotated with a size cap and three older copies.
cp "$TEST/niruwm.plain" "$TEST/bin/niruwm"
L="$XDG_STATE_HOME/niruwm/session.log"
for round in 1 2 3 4; do
  head -c 9000000 /dev/zero | tr '\0' "$round" > "$L"
  "$TEST/bin/niruwm-session"
  [ "$(head -c 1 "$L.1")" = "$round" ] || { echo "FAIL rotation round $round"; exit 1; }
  [ "$(wc -c < "$L.1")" -le 8388608 ] || { echo "FAIL old copy not capped"; exit 1; }
  [ "$(wc -c < "$L")" -lt 100000 ] || { echo "FAIL log not emptied"; exit 1; }
done
[ -f "$L.3" ] && [ ! -f "$L.4" ] || { echo "FAIL expected exactly three old copies"; exit 1; }
[ "$(head -c 1 "$L.3")" = 2 ] || { echo "FAIL old copies out of order"; exit 1; }
printf 'PASS: session log is rotated and capped with three old copies\n'
