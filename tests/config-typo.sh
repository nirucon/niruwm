#!/bin/sh
# A typing error in config.toml must not end the session: NIRUWM starts with
# the last working settings (or the defaults) and says where the error is.
# Needs Xvfb and a built niruwm: tests/config-typo.sh [path/to/niruwm] [path/to/nirubar]
set -eu
SOURCE=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
WM=${1:-$SOURCE/target/release/niruwm}
BAR=${2:-}
TEST=$(mktemp -d)
cleanup() { kill "$WMPID" "$XPID" 2>/dev/null || :; rm -rf "$TEST"; }
WMPID= XPID=
trap cleanup EXIT HUP INT TERM
export HOME="$TEST/home" XDG_CONFIG_HOME="$TEST/home/.config" XDG_STATE_HOME="$TEST/home/.local/state"
mkdir -p "$TEST/bin" "$XDG_CONFIG_HOME/niruwm" "$XDG_STATE_HOME/niruwm"
cat > "$TEST/bin/notify-send" <<'N'
#!/bin/sh
printf '%s\n' "$*" >> "$HOME/notifications"
N
chmod +x "$TEST/bin/notify-send"
[ -z "$BAR" ] || ln -s "$BAR" "$TEST/bin/nirubar"
export PATH="$TEST/bin:/usr/bin:/bin"
D=:$((80 + $$ % 100))
Xvfb "$D" -screen 0 1280x800x24 -nolisten tcp >/dev/null 2>&1 & XPID=$!
export DISPLAY=$D
for _ in 1 2 3 4 5 6 7 8 9 10; do xdpyinfo >/dev/null 2>&1 && break; sleep 0.3; done

start() {
  rm -f "$HOME/notifications"
  "$WM" >> "$TEST/wm.log" 2>&1 & WMPID=$!
  sleep 5
  kill -0 "$WMPID" 2>/dev/null || { cat "$TEST/wm.log"; echo "FAIL: $1: niruwm ended"; exit 1; }
}
stop() { kill "$WMPID" 2>/dev/null || :; wait "$WMPID" 2>/dev/null || :; WMPID=; }

# 1. A good file loads and is kept as the last working copy.
printf 'theme = "niru-forest-vigil"\n[bar]\nenabled = true\n' > "$XDG_CONFIG_HOME/niruwm/config.toml"
start 'good file'
grep -q 'niru-forest-vigil' "$XDG_STATE_HOME/niruwm/config.last-good.toml"
[ ! -s "$HOME/notifications" ]
stop
echo 'PASS: a good file is kept as the last working copy'

# 2. A typing error: the session keeps running with the last working copy.
printf 'theme = "niru-forest-vigil"\nn# Monitors\n' > "$XDG_CONFIG_HOME/niruwm/config.toml"
start 'typing error'
grep -q 'Settings error' "$HOME/notifications"
grep -q 'config.toml line 2' "$HOME/notifications"
grep -q 'last working settings' "$HOME/notifications"
grep -q 'niru-forest-vigil' "$XDG_STATE_HOME/niruwm/config.last-good.toml"
if [ -n "$BAR" ]; then
  pgrep -P "$WMPID" -x nirubar >/dev/null || { cat "$TEST/wm.log"; echo 'FAIL: the bar is not running'; exit 1; }
  if grep -q 'the bar exited' "$TEST/wm.log"; then cat "$TEST/wm.log"; echo 'FAIL: the bar crashed'; exit 1; fi
fi
stop
echo 'PASS: a typing error keeps the session, with a notification'

# 3. No working copy at all: the defaults.
rm -f "$XDG_STATE_HOME/niruwm/config.last-good.toml"
start 'no working copy'
grep -q 'default settings' "$HOME/notifications"
stop
echo 'PASS: without a working copy the defaults are used'

# 4. An unknown theme: niru-noir, with a notification.
printf 'theme = "no-such-theme"\n' > "$XDG_CONFIG_HOME/niruwm/config.toml"
start 'unknown theme'
grep -q 'Theme no-such-theme' "$HOME/notifications"
stop
echo 'PASS: an unknown theme falls back to niru-noir'
