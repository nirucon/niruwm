#!/bin/sh
# niructl config: every working version is kept, a section reset reloads the
# running session, and undo brings the earlier file back exactly.
# Needs Xvfb and built binaries:
#   tests/config-history.sh [niruwm] [niructl] [nirubar]
set -eu
SOURCE=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
WM=${1:-$SOURCE/target/release/niruwm}
CTL=${2:-$SOURCE/target/release/niructl}
BAR=${3:-}
TEST=$(mktemp -d)
cleanup() {
  [ -z "$WMPID" ] || { pkill -P "$WMPID" 2>/dev/null; kill "$WMPID" 2>/dev/null; } || :
  [ -z "$XPID" ] || kill "$XPID" 2>/dev/null || :
  rm -rf "$TEST"
}
WMPID= XPID=
trap cleanup EXIT HUP INT TERM
fail() { cat "$TEST/wm.log" 2>/dev/null; echo "FAIL: $*"; exit 1; }
export HOME="$TEST/home" XDG_CONFIG_HOME="$TEST/home/.config" \
  XDG_STATE_HOME="$TEST/home/.local/state" XDG_DATA_HOME="$TEST/home/.local/share"
CFG=$XDG_CONFIG_HOME/niruwm/config.toml
HIST=$XDG_STATE_HOME/niruwm/config-history
mkdir -p "$TEST/bin" "$XDG_CONFIG_HOME/niruwm"
printf '#!/bin/sh\nexit 0\n' > "$TEST/bin/notify-send"
chmod +x "$TEST/bin/notify-send"
ln -s "$CTL" "$TEST/bin/niructl"
[ -z "$BAR" ] || ln -s "$BAR" "$TEST/bin/nirubar"
export PATH="$TEST/bin:/usr/bin:/bin"
D=:$((80 + $$ % 100))
Xvfb "$D" -screen 0 1280x800x24 -nolisten tcp >/dev/null 2>&1 & XPID=$!
export DISPLAY=$D
for _ in 1 2 3 4 5 6 7 8 9 10; do xdpyinfo >/dev/null 2>&1 && break; sleep 0.3; done

cat > "$CFG" <<'C'
theme = "niru-forest-vigil"

# My own bar.
[bar]
enabled = true
modules = ["workspaces", "clock"]
C
cp "$CFG" "$TEST/original.toml"
"$WM" >> "$TEST/wm.log" 2>&1 & WMPID=$!
sleep 4
kill -0 "$WMPID" 2>/dev/null || fail 'niruwm ended'
[ "$(ls "$HIST" | wc -l)" = 1 ] || fail 'the first load is not in the history'
echo 'PASS: a working file is kept in the history'

out=$(niructl config reset bar --yes)
printf '%s\n' "$out" | grep -q 'The session reloaded' || { echo "$out"; fail 'reset did not reload'; }
grep -q '"recording"' "$CFG" || fail 'the bar did not get the default modules'
grep -q 'niru-forest-vigil' "$CFG" || fail 'the reset changed the theme'
grep -q '^# Modules to show' "$CFG" || fail 'the template comments are missing'
sleep 1
[ "$(ls "$HIST" | wc -l)" = 2 ] || fail 'the reset version is not in the history'
if grep -q 'reload rejected' "$TEST/wm.log"; then fail 'the session rejected the reset file'; fi
if [ -n "$BAR" ]; then pgrep -P "$WMPID" -x nirubar >/dev/null || fail 'the bar is not running'; fi
echo 'PASS: reset bar keeps the rest, has the comments and reloads the session'

niructl config undo --yes | grep -q 'The session reloaded' || fail 'undo did not reload'
cmp -s "$CFG" "$TEST/original.toml" || fail 'undo did not bring the file back exactly'
echo 'PASS: undo restores the earlier file exactly'

niructl config undo --yes >/dev/null 2>&1 && fail 'undo past the oldest version succeeded'
niructl config restore 0 --yes >/dev/null
grep -q '"recording"' "$CFG" || fail 'restore 0 did not return to the newest version'
echo 'PASS: restore 0 returns to the newest version'

if [ -n "$BAR" ]; then
  cp "$TEST/original.toml" "$CFG"
  nirubar --reset-config | grep -q 'The session reloaded' || fail 'nirubar --reset-config did not use niructl'
  grep -q '"recording"' "$CFG" || fail 'nirubar --reset-config did not reset'
  echo 'PASS: nirubar --reset-config resets through niructl'
fi
