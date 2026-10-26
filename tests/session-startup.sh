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
