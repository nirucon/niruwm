#!/bin/sh
# End-to-end session smoke test.
#
# Verifies the contract the user cares about: a bar and window manager start
# on a bare X server, apps open, workspaces switch, and the session exits
# cleanly leaving no helper process behind.
#
# Runs on Debian/Arch/Void containers. Needs: xvfb, x11-utils, x11-apps
# (xterm is used as the "terminal app" because it has no GPU requirements).
set -eu

BINDIR="${1:?usage: session-smoke.sh BINDIR HOME}"
export HOME="$2"
export PATH="$BINDIR:$PATH"
export XDG_CONFIG_HOME="$HOME/.config"
export DISPLAY=:98

Xvfb :98 -screen 0 1280x800x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
XVFB=$!
cleanup() { kill "$XVFB" 2>/dev/null || :; }
trap cleanup EXIT INT TERM
i=0
while [ ! -S /tmp/.X11-unix/X98 ]; do
  i=$((i + 1)); [ "$i" -gt 100 ] && { echo "FAIL Xvfb did not start"; cat /tmp/xvfb.log; exit 1; }
  sleep 0.1
done

fail() { echo "FAIL $*"; exit 1; }
ok() { echo "OK   $*"; }

# ── 1. Session script starts the WM and the bar ─────────────────────────────
"$BINDIR/niruwm-session" >/tmp/session.log 2>&1 &
SESSION=$!
trap 'kill $SESSION 2>/dev/null || :; kill $XVFB 2>/dev/null || :' EXIT INT TERM
sleep 2

kill -0 "$SESSION" 2>/dev/null || { echo "--- session.log ---"; cat /tmp/session.log; fail "session exited"; }
ok "session process running"

# ── 2. EWMH ownership: NIRUWM is the window manager ─────────────────────────
WMCHECK=$(xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | awk '{print $NF}')
[ -n "$WMCHECK" ] || fail "_NET_SUPPORTING_WM_CHECK missing"
NAME=$(xprop -id "$WMCHECK" _NET_WM_NAME 2>/dev/null | sed 's/.*= //')
echo "    WM: $NAME"
case "$NAME" in *NIRUWM*) ok "NIRUWM owns the display" ;; *) fail "wrong WM: $NAME" ;; esac

# ── 3. The bar is a mapped dock with a strut ────────────────────────────────
BARID=$(xwininfo -root -tree | awk '/NIRUBAR/{print $1; exit}')
[ -n "$BARID" ] || fail "NIRUBAR window missing"
STRUT=$(xprop -id "$BARID" _NET_WM_STRUT 2>/dev/null)
echo "    $STRUT"
case "$STRUT" in *"= 0, 0, 28, 0"*) ok "bar reserves 28px at the top" ;; *) fail "unexpected strut: $STRUT" ;; esac

# ── 4. A client can open and is tiled inside the work area ─────────────────
xterm -hold -title NIRU-TEST >/dev/null 2>&1 &
# shellcheck disable=SC2034
XTERM=$!
sleep 1.5
CLIENTS=$(xprop -root _NET_CLIENT_LIST 2>/dev/null | sed 's/.*= //')
echo "    clients: $CLIENTS"
[ "$CLIENTS" != "not found" ] || fail "_NET_CLIENT_LIST missing"
case "$CLIENTS" in *0x*) ok "client is managed" ;; *) fail "no managed clients" ;; esac

# Read the window geometry by id (name lookup differs between Xorg tools).
# Match the client from _NET_CLIENT_LIST (xterm titles vary by build).
WINTITLE=$(xprop -root _NET_CLIENT_LIST | sed 's/.*# //' | tr ',' '\n' | awk '{print $NF}' | head -1)
[ -n "$WINTITLE" ] || fail "xterm did not map"
GEOM_INFO=$(xwininfo -id "$WINTITLE")
Y=$(printf '%s\n' "$GEOM_INFO" | awk -F: '/Absolute upper-left Y/{gsub(/ /,"",$2); print $2}')
W=$(printf '%s\n' "$GEOM_INFO" | awk -F: '/^ *Width:/{gsub(/ /,"",$2); print $2}')
H=$(printf '%s\n' "$GEOM_INFO" | awk -F: '/^ *Height:/{gsub(/ /,"",$2); print $2}')
echo "    geometry (y w h): ${Y:-?} ${W:-?} ${H:-?}"
[ -n "${Y:-}" ] || fail "could not read window geometry"
[ "$Y" -ge 28 ] || fail "client overlaps the bar (y=$Y)"
[ "${H:-0}" -le 772 ] || fail "client is taller than the work area (h=$H)"

# ── 5. Workspace switching via the control channel ─────────────────────────
niructl tag 3 || fail "niructl tag failed"
sleep 0.4
TAG=$(xprop -root _NET_CURRENT_DESKTOP 2>/dev/null | awk '{print $NF}')
[ "$TAG" = "2" ] || fail "tag switch not applied (got $TAG)"
ok "niructl switched to tag 3"

niructl tag 1
sleep 0.4
TAG=$(xprop -root _NET_CURRENT_DESKTOP 2>/dev/null | awk '{print $NF}')
[ "$TAG" = "0" ] || fail "return to tag 1 failed"
ok "returned to tag 1"

# ── 6. Clean logout ────────────────────────────────────────────────────────
niructl quit || fail "niructl quit failed"

# Wait for the process to disappear. `kill -0` is not enough: the session is a
# child of this script, so once it exits it stays a zombie (state Z) until
# reaped, and `kill -0` still succeeds against a zombie.
exited=0
i=0
while [ "$i" -lt 100 ]; do
  state=$(ps -o stat= -p "$SESSION" 2>/dev/null || true)
  if [ -z "$state" ] || [ "${state#Z}" != "$state" ]; then
    exited=1
    break
  fi
  sleep 0.05
  i=$((i + 1))
done
if [ "$exited" != 1 ]; then
  echo "--- diagnostic: what is still running ---"
  ps -o pid,ppid,stat,wchan:25,cmd -e 2>/dev/null | grep -E "niruwm|nirubar" | grep -v grep || echo "(no niruwm/nirubar processes)"
  echo "--- session pid $SESSION ---"
  ps -o pid,ppid,stat,wchan:25,cmd -p "$SESSION" 2>/dev/null || echo "(gone)"
  echo "--- session children ---"
  ps --ppid "$SESSION" -o pid,stat,cmd 2>/dev/null || echo "(none)"
  echo "--- session log ---"
  cat /tmp/session.log 2>/dev/null | tail -20 || true
  fail "session did not exit after quit (state: $state)"
fi
ok "session exited on quit"

# No leftover bar process.
if pgrep -f "$BINDIR/nirubar" >/dev/null 2>&1; then fail "nirubar left running"; fi
ok "no leftover processes"

echo "PASS session-smoke"
