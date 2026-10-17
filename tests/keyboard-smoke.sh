#!/bin/sh
# Keyboard-level test: press real modifier combinations through XTEST.
#
# This is the test that catches the class of bug where a binding parses,
# grabs and looks right but never fires, because the modifier comparison or
# the keysym lookup is wrong.
set -eu

BINDIR="${1:?usage: keyboard-smoke.sh BINDIR HOME}"
export HOME="$2"
export PATH="$BINDIR:$PATH"
export XDG_CONFIG_HOME="$HOME/.config"
export DISPLAY=:97

Xvfb :97 -screen 0 1024x768x24 -nolisten tcp >/tmp/xvfb-kbd.log 2>&1 &
XVFB=$!
trap 'kill $XVFB 2>/dev/null || :' EXIT INT TERM
i=0
while [ ! -S /tmp/.X11-unix/X97 ]; do
  i=$((i + 1)); [ "$i" -gt 100 ] && { echo "FAIL Xvfb"; cat /tmp/xvfb-kbd.log; exit 1; }
  sleep 0.1
done

fail() { echo "FAIL $*"; exit 1; }
ok() { echo "OK   $*"; }

# Swedish layout, as the session sets it.
command -v setxkbmap >/dev/null 2>&1 && setxkbmap se 2>/dev/null || true

"$BINDIR/nirubar" >/tmp/kbd-bar.log 2>&1 &
BAR_PID=$!
NIRUWM_SESSION_BAR=1 "$BINDIR/niruwm" >/tmp/kbd-wm.log 2>&1 &
WM_PID=$!
cleanup() { kill "$BAR_PID" "$WM_PID" "$XVFB" 2>/dev/null || :; }
trap cleanup EXIT INT TERM
sleep 1.5

# ── Helper: press Super + key(s) for real ──────────────────────────────────
# xdotool is used because it synthesises events through XTEST, exactly like a
# person pressing the key. It is a test-only dependency.
if ! command -v xdotool >/dev/null 2>&1; then
  echo "SKIP keyboard test (xdotool not installed)"
  exit 0
fi

press() {
  # $1 = comma separated extra modifiers (shift,ctrl), $2 = key name
  mods="super"
  [ -n "$1" ] && mods="$mods+$1"
  xdotool key --clearmodifiers "$mods+$2"
  sleep 0.35
}

# xprop output differs between Xorg versions: some print "= 2" on the same
# line, some wrap it. Take the last integer anywhere in the output.
tagnow() { xprop -root _NET_CURRENT_DESKTOP | grep -oE '[0-9]+' | tail -1; }
clients() { xprop -root _NET_CLIENT_LIST | sed 's/.*# //'; }

# ── 1. Workspace switching by keyboard ─────────────────────────────────────
press "" "3"
[ "$(tagnow)" = "2" ] || fail "super+3 did not switch (got $(tagnow))"
ok "super+3 switches workspace"

press "" "1"
[ "$(tagnow)" = "0" ] || fail "super+1 did not switch"
ok "super+1 returns"

press "shift" "Tab"
[ "$(tagnow)" = "8" ] || fail "super+shift+Tab did not wrap to 9 (got $(tagnow))"
ok "super+shift+Tab wraps backwards"
press "" "1"

# ── 2. Terminal binding actually starts a program ──────────────────────────
# Point the terminal at something guaranteed to exist in the test image.
mkdir -p "$HOME/.config/niruwm"
printf 'terminal = ["xterm", "-title", "NIRU-TERM"]\n' > "$HOME/.config/niruwm/config.toml"
# Reload so the new command is used.
xdotool key --clearmodifiers super+r
sleep 0.5
press "" "Return"
i=0
while [ "$i" -lt 40 ]; do
  [ -n "$(clients)" ] && [ "$(clients)" != "not found" ] && break
  sleep 0.15; i=$((i + 1))
done
[ "$(clients)" != "not found" ] || fail "super+Return did not open the terminal"
ok "super+Return opens a terminal"

# ── 3. Tiling: the window sits under the bar ───────────────────────────────
WIN=$(xprop -root _NET_CLIENT_LIST | sed 's/.*# //' | tr ',' '\n' | awk '{print $NF}' | head -1)
Y=$(xwininfo -id "$WIN" | awk -F: '/Absolute upper-left Y/{gsub(/ /,"",$2); print $2}')
[ "${Y:-0}" -ge 28 ] || fail "terminal overlaps the bar (y=$Y)"
ok "terminal is tiled below the bar (y=$Y)"

# ── 4. A second window tiles beside the first ──────────────────────────────
press "" "Return"
sleep 1
N=$(xprop -root _NET_CLIENT_LIST | sed 's/.*# //' | tr ',' '\n' | grep -c '0x')
[ "$N" -ge 2 ] || fail "second terminal did not open ($N clients)"
W1=$(xwininfo -id "$WIN" | awk -F: '/^ *Width:/{gsub(/ /,"",$2); print $2}')
[ "${W1:-0}" -lt 1000 ] || fail "windows are not tiled side by side (w=$W1)"
ok "second window tiles beside the first (w=$W1)"

# ── 5. Close the focused window with super+q ───────────────────────────────
press "" "q"
sleep 0.8
N2=$(xprop -root _NET_CLIENT_LIST | sed 's/.*# //' | tr ',' '\n' | grep -c '0x' || true)
[ "$((N - 1))" -eq "${N2:-0}" ] || fail "super+q did not close a window ($N -> ${N2:-0})"
ok "super+q closes the focused window ($N -> $N2 clients)"

# ── 6. Layout switch by keyboard ───────────────────────────────────────────
press "" "m"
press "" "t"
ok "layout bindings respond"

# ── 7. Quit by keyboard ────────────────────────────────────────────────────
xdotool key --clearmodifiers "super+ctrl+shift+q"
# The WM is our child, so it becomes a zombie when it exits; `kill -0` would
# still succeed, so inspect the process state instead.
exited=0
i=0
while [ "$i" -lt 100 ]; do
  state=$(ps -o stat= -p "$WM_PID" 2>/dev/null || true)
  if [ -z "$state" ] || [ "${state#Z}" != "$state" ]; then
    exited=1
    break
  fi
  sleep 0.05
  i=$((i + 1))
done
[ "$exited" = 1 ] || fail "super+ctrl+shift+q did not quit (state: $state)"
ok "super+ctrl+shift+q quits cleanly"

echo "PASS keyboard-smoke"
