#!/bin/sh
# Restart in place: `niructl restart-wm` replaces the running NIRUWM with the
# installed one inside the same X session, keeping every window.
#
# Usage: sh tests/restart-in-place.sh BINDIR NIRUBAR [ROUNDS]
#   BINDIR   release build directory (niruwm, niructl, niruidle)
#   NIRUBAR  a nirubar binary for the bars
#   ROUNDS   how many restarts in a row (default 5)
#   NIRUNOTIFY=PATH (optional) also runs the notification daemon, changed
#            in generation B like niruidle.
#
# The test builds two NIRUX-style generations (A and B) under a throwaway
# HOME, with `current` and ~/.local/bin symlinks like NIRUX installs them. B
# differs from A in niruwm, niructl and niruidle (a byte appended to each), so
# a restart after switching `current` must run the new manager and replace
# the helpers whose program changed, and keep those that did not.
#
# Two emulated screens on one Xvfb server (NIRU_RANDR_EMU), windows on
# several workspaces of both, a floating window, a scratchpad (hidden and
# shown) and a swallowed terminal. Every round compares each window's
# workspace, map state and geometry, the tiling order, the focus, both
# screens' workspaces and layouts, and counts MapNotify events for windows
# that must stay hidden (a flash of every hidden window is the failure this
# guards against).
#
# Needs: Xvfb, xdotool, xprop, xwininfo, xterm, xeyes, xev, dbus-daemon.
# Never touches DISPLAY :0 or the real HOME.
set -eu

BINDIR=$(CDPATH='' cd -- "${1:?usage: restart-in-place.sh BINDIR NIRUBAR [ROUNDS]}" && pwd)
NIRUBAR=${2:?usage: restart-in-place.sh BINDIR NIRUBAR [ROUNDS]}
ROUNDS=${3:-5}
SRC=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
DISP=${RESTART_TEST_DISPLAY:-:97}
NUM=${DISP#:}

T=$(mktemp -d "${TMPDIR:-/tmp}/niruwm-restart.XXXXXX")
H="$T/home"
mkdir -p "$H/.local/bin" "$H/.local/share/nirux/generations" "$H/.config/niruwm" "$T/run"
chmod 700 "$T/run"

fail() { echo "FAIL $*"; [ ! -f "$T/before" ] || { echo "--- before ---"; cat "$T/before"; }; [ ! -f "$T/after" ] || { echo "--- after ---"; cat "$T/after"; }; echo "--- session.out ---"; tail -n 20 "$T/session.out" 2>/dev/null || :; echo "--- session.log (tail) ---"; tail -n 60 "$H/.local/state/niruwm/session.log" 2>/dev/null || :; exit 1; }
ok() { echo "OK   $*"; }

# ── Two generations ─────────────────────────────────────────────────────────
for g in A B; do
  d="$H/.local/share/nirux/generations/$g/bin"
  mkdir -p "$d"
  for b in niruwm niructl niruidle; do cp "$BINDIR/$b" "$d/$b"; done
  cp "$NIRUBAR" "$d/nirubar"
  cp "$SRC/session/niruwm-session" "$SRC/session/niruclipboard" "$d/"
  chmod 755 "$d"/*
done
if [ -n "${NIRUNOTIFY:-}" ]; then
  for g in A B; do cp "$NIRUNOTIFY" "$H/.local/share/nirux/generations/$g/bin/nirunotify"; done
  ln -s "$H/.local/share/nirux/current/bin/nirunotify" "$H/.local/bin/nirunotify"
  printf 'B' >> "$H/.local/share/nirux/generations/B/bin/nirunotify"
fi
for b in niruwm niructl niruidle; do printf 'B' >> "$H/.local/share/nirux/generations/B/bin/$b"; done
ln -s generations/A "$H/.local/share/nirux/current"
for b in niruwm niructl niruidle nirubar niruwm-session niruclipboard; do
  ln -s "$H/.local/share/nirux/current/bin/$b" "$H/.local/bin/$b"
done
# A stand-in gammastep: the night-light watcher (solar mode, the default)
# runs without touching any real display's gamma.
printf '#!/bin/sh\nexit 0\n' > "$H/.local/bin/gammastep"
chmod 755 "$H/.local/bin/gammastep"
switch_to() {
  ln -sfn "generations/$1" "$H/.local/share/nirux/current.new"
  mv -T "$H/.local/share/nirux/current.new" "$H/.local/share/nirux/current"
}

cat > "$H/.config/niruwm/session.conf" <<'EOF'
START_PICOM=0
START_SLEEP_LOCK=0
START_GESTURES=0
WELCOME_NOTIFICATION=off
EOF
[ -n "${NIRUNOTIFY:-}" ] || echo NOTIFICATION_PROVIDER=external >> "$H/.config/niruwm/session.conf"
cat > "$H/.config/niruwm/config.toml" <<'EOF'
terminal = ["xterm"]
restore_session = false
[swallow]
enabled = true
EOF
printf 'LEFT 1280x800+0+0\nRIGHT 1280x800+1280+0\n' > "$T/screens"

Xvfb "$DISP" -screen 0 2560x800x24 -nolisten tcp >"$T/xvfb.log" 2>&1 &
XVFB=$!
SESSION=""
XEV=""
BUS_PID=""
cleanup() {
  [ -z "$XEV" ] || kill "$XEV" 2>/dev/null || :
  [ -z "$SESSION" ] || kill "$SESSION" 2>/dev/null || :
  sleep 0.5
  kill "$XVFB" 2>/dev/null || :
  [ -z "$BUS_PID" ] || kill "$BUS_PID" 2>/dev/null || :
  # Programs started for the test run under the throwaway HOME.
  pkill -f "$T" 2>/dev/null || :
  rm -rf "$T"
}
trap cleanup EXIT INT TERM
i=0
while [ ! -S "/tmp/.X11-unix/X$NUM" ]; do
  i=$((i + 1)); [ "$i" -gt 100 ] && fail "Xvfb did not start"
  sleep 0.1
done

# One private session bus for everything the test starts, stopped at the
# end. Without an address, every D-Bus client (notify-send from niructl)
# would autolaunch a bus of its own that outlives the test.
BUS_INFO=$(env -i HOME="$H" PATH=/usr/bin:/bin dbus-daemon --session --fork --print-address=1 --print-pid=1) ||
  fail "cannot start a private D-Bus session bus"
BUS=$(printf '%s\n' "$BUS_INFO" | sed -n 1p)
BUS_PID=$(printf '%s\n' "$BUS_INFO" | sed -n 2p)
run() {
  env -i HOME="$H" DISPLAY="$DISP" PATH="$H/.local/bin:/usr/bin:/bin" \
    XDG_RUNTIME_DIR="$T/run" NIRU_RANDR_EMU="$T/screens" LANG=C.UTF-8 \
    DBUS_SESSION_BUS_ADDRESS="$BUS" "$@"
}
ctl() { run "$H/.local/bin/niructl" "$@"; }
prop() { run xprop -root "$1" 2>/dev/null | sed 's/^[^=]*= *//'; }

run "$H/.local/bin/niruwm-session" >"$T/session.out" 2>&1 &
SESSION=$!
i=0
until prop _NET_SUPPORTING_WM_CHECK | grep -q 0x; do
  i=$((i + 1)); [ "$i" -gt 100 ] && fail "NIRUWM did not start"
  kill -0 "$SESSION" 2>/dev/null || fail "session exited"
  sleep 0.1
done
sleep 1
ok "session started"

# ── Windows ─────────────────────────────────────────────────────────────────
pointer() { run xdotool mousemove "$1" "$2"; sleep 0.2; }
open_term() {
  run xterm -class "$1" -title "$1" -e sh -c 'sleep 100000' >/dev/null 2>&1 &
  i=0
  until run xdotool search --class "^$1\$" >/dev/null 2>&1; do
    i=$((i + 1)); [ "$i" -gt 50 ] && fail "$1 did not map"
    sleep 0.1
  done
  sleep 0.3
}
win() { run xdotool search --class "^$1\$" | head -n 1; }

pointer 600 400
ctl tag 1; open_term L1a; open_term L1b; open_term L1c
ctl tag 2; open_term L2a
ctl tag 3; open_term L3a; open_term L3b
ctl layout monocle
pointer 1900 400
ctl tag 1; open_term R1a; open_term R1b
ctl tag 4; open_term R4a
ctl layout scroll
# A floating window on R4.
open_term R4f
run xdotool key --clearmodifiers super+shift+space
sleep 0.3
# A swallowed terminal: xeyes started from inside xterm SW.
ctl tag 5
run xterm -class SW -title SW -e sh -c 'sleep 1; exec xeyes' >/dev/null 2>&1 &
sleep 3
run xdotool search --class '^SW$' >/dev/null || fail "SW terminal missing"
grep -q "swallowed terminal" "$H/.local/state/niruwm/session.log" || fail "xeyes did not swallow its terminal"
# Scratchpads: one hidden in slot 2, one shown in slot 3.
open_term S2
run xdotool windowactivate "$(win S2)"
sleep 0.3
ctl scratchpad send 2
sleep 0.3
open_term S3
run xdotool windowactivate "$(win S3)"
sleep 0.3
ctl scratchpad send 3
sleep 0.3
ctl scratchpad 2 hide >/dev/null 2>&1 || :
ctl scratchpad 3 show >/dev/null 2>&1 || :
sleep 0.3
# Back to the left screen, workspace 3 (monocle), with L3b focused.
pointer 600 400
ctl tag 3
run xdotool windowactivate "$(win L3b)" 2>/dev/null || :
sleep 0.5
ok "windows open on both screens"

# One line per managed window: class, desktop, map state, geometry.
snapshot() {
  for class in L1a L1b L1c L2a L3a L3b R1a R1b R4a R4f SW XEyes S2 S3; do
    id=$(win "$class")
    desk=$(run xprop -id "$id" _NET_WM_DESKTOP 2>/dev/null | sed 's/.*= //')
    info=$(run xwininfo -id "$id" 2>/dev/null)
    state=$(printf '%s\n' "$info" | awk -F: '/Map State/{gsub(/ /,"",$2); print $2}')
    geo=$(printf '%s\n' "$info" | awk '/-geometry/{print $2}')
    # A hidden window's geometry is set again when it is shown (its border
    # follows the windows next to it), so only visible ones are compared.
    [ "$state" = IsViewable ] || geo=hidden
    echo "$class desk=$desk $state $geo"
  done
  echo "order $(prop _NET_CLIENT_LIST | tr -d ' ')"
  echo "active $(prop _NET_ACTIVE_WINDOW)"
  echo "desktop $(prop _NET_CURRENT_DESKTOP)"
  echo "layout $(prop _NIRUWM_LAYOUT)"
  echo "monitors $(prop _NIRUWM_MONITORS)"
  echo "scratch $(prop _NIRUWM_SCRATCHPADS | tr '\n' ' ')"
}
BEFORE=$(snapshot)
printf '%s\n' "$BEFORE" > "$T/before"
printf '%s\n' "$BEFORE" | grep -q "^S2 .*IsUnMapped" || fail "S2 should start hidden"
printf '%s\n' "$BEFORE" | grep -q "^S3 .*IsViewable" || fail "S3 should start shown"
printf '%s\n' "$BEFORE" | grep -q "^SW .*IsUnMapped" || fail "SW should start swallowed"
HIDDEN=$(printf '%s\n' "$BEFORE" | awk '$3 == "IsUnMapped" {print $1}' | tr '\n' ' ')
echo "    hidden before restart: $HIDDEN"

# Processes of this test only (the real session runs the same programs):
# matched by the throwaway HOME in their environment.
mine() {
  for pid in $(pgrep -u "$(id -u)" "$@" 2>/dev/null); do
    tr '\0' '\n' < "/proc/$pid/environ" 2>/dev/null | grep -qxF "HOME=$H" && echo "$pid"
  done
}
nightlight_watchers() { mine -f "niructl nightlight watch" | grep -c . || :; }
[ "$(nightlight_watchers)" -eq 1 ] || fail "one night-light watcher at login, found $(nightlight_watchers)"
gen=A
r=1
while [ "$r" -le "$ROUNDS" ]; do
  if [ "$r" -eq 2 ]; then
    # A helper that should run but does not (here: the night-light watcher,
    # stopped; on a real host the config changed from manual to solar after
    # the login) is started by the restart.
    for pid in $(mine -f "niructl nightlight watch"); do kill "$pid"; done
    sleep 0.3
    [ "$(nightlight_watchers)" -eq 0 ] || fail "could not stop the night-light watcher"
  fi
  if [ "$gen" = A ]; then gen=B; else gen=A; fi
  switch_to "$gen"
  OLD_WM=$(mine -x niruwm)
  OLD_IDLE=$(mine -x niruidle)
  OLD_CLIP=$(mine -f "niruclipboard daemon")
  OLD_NOTIFY=$(mine -x nirunotify)
  # Every MapNotify on the root during the restart.
  run xev -root -event substructure > "$T/xev.$r" 2>&1 &
  XEV=$!
  sleep 0.3
  out=$(ctl restart-wm) || fail "round $r: restart-wm failed: $out"
  echo "    round $r: $out"
  sleep 2
  kill "$XEV" 2>/dev/null || :; XEV=""
  NEW_WM=$(mine -x niruwm)
  [ -n "$NEW_WM" ] && [ "$NEW_WM" != "$OLD_WM" ] || fail "round $r: the manager process was not replaced"
  exe=$(readlink -f "/proc/$NEW_WM/exe")
  case "$exe" in */generations/$gen/bin/niruwm) ;; *) fail "round $r: running $exe, not generation $gen" ;; esac
  AFTER=$(snapshot)
  if [ "$AFTER" != "$BEFORE" ]; then
    printf '%s\n' "$AFTER" > "$T/after"
    diff "$T/before" "$T/after" || :
    fail "round $r: window state changed"
  fi
  # No hidden window may have been mapped.
  for class in $HIDDEN; do
    id=$(win "$class")
    [ -n "$id" ] || continue
    dec=$(printf '%d' "$id")
    hex=$(printf '0x%x' "$dec")
    if grep -A2 '^MapNotify' "$T/xev.$r" | grep -q "window $hex,"; then
      fail "round $r: hidden window $class was mapped during the restart"
    fi
  done
  NEW_IDLE=$(mine -x niruidle)
  NEW_CLIP=$(mine -f "niruclipboard daemon")
  if [ -n "${NIRUNOTIFY:-}" ]; then
    NEW_NOTIFY=$(mine -x nirunotify)
    [ -n "$NEW_NOTIFY" ] && [ "$NEW_NOTIFY" != "$OLD_NOTIFY" ] || fail "round $r: nirunotify changed but was not replaced"
    [ "$(mine -x nirunotify | wc -l)" -eq 1 ] || fail "round $r: more than one nirunotify"
  fi
  [ -n "$NEW_IDLE" ] && [ "$NEW_IDLE" != "$OLD_IDLE" ] || fail "round $r: niruidle changed but was not replaced"
  [ -z "$OLD_CLIP" ] || [ "$NEW_CLIP" = "$OLD_CLIP" ] || fail "round $r: niruclipboard did not change but was restarted"
  [ "$(nightlight_watchers)" -eq 1 ] || fail "round $r: $(nightlight_watchers) night-light watchers instead of 1"
  if [ "$r" -eq 2 ]; then
    grep -q "started the missing helper: niructl nightlight watch" "$H/.local/state/niruwm/session.log" ||
      fail "round 2: the missing night-light watcher was not started"
  fi
  [ "$(mine -x niruidle | wc -l)" -eq 1 ] || fail "round $r: more than one niruidle: $(ps -o pid,ppid,stat,lstart,args -p "$(mine -x niruidle | tr "\n" "," | sed "s/,$//")")"
  bars=$(run xwininfo -root -tree | grep -c NIRUBAR || :)
  [ "$bars" -eq 2 ] || fail "round $r: $bars bars instead of 2"
  ok "round $r: generation $gen running, every window kept, hidden windows never mapped, helpers replaced only when changed"
  r=$((r + 1))
done

# ── A broken new manager: the session goes back to the previous one ───────
C="$H/.local/share/nirux/generations/C/bin"
mkdir -p "$C"
cp -P "$H/.local/share/nirux/generations/$gen/bin/"* "$C/"
printf '#!/bin/sh\necho "broken niruwm" >&2\nexit 1\n' > "$C/niruwm"
chmod 755 "$C/niruwm"
switch_to C
out=$(ctl restart-wm) || fail "restart into a broken manager: $out"
sleep 2
grep -q "running the previous one again" "$H/.local/state/niruwm/session.log" || fail "no fallback to the previous manager"
exe=$(readlink -f "/proc/$(mine -x niruwm)/exe")
case "$exe" in */generations/$gen/bin/niruwm) ;; *) fail "fallback runs $exe" ;; esac
AFTER=$(snapshot)
[ "$AFTER" = "$BEFORE" ] || { printf '%s\n' "$AFTER" > "$T/after"; diff "$T/before" "$T/after" || :; fail "state changed across the fallback"; }
ok "a broken new manager falls back to the previous one, windows kept"
switch_to "$gen"

# ── A locked screen stays locked: the restart waits for the unlock ─────────
OLD_WM=$(mine -x niruwm)
LOCKWIN=$(win L1a)
# NIRULOCK's readiness contract: _NIRU_LOCK_READY (type WINDOW) names its
# mapped window. xprop cannot write a WINDOW property; libX11 can.
run python3 -c '
import ctypes, sys
x = ctypes.CDLL("libX11.so.6")
x.XOpenDisplay.restype = ctypes.c_void_p
x.XInternAtom.restype = ctypes.c_ulong
x.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
x.XDefaultRootWindow.restype = ctypes.c_ulong
x.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
x.XChangeProperty.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong,
                              ctypes.c_int, ctypes.c_int, ctypes.c_void_p, ctypes.c_int]
x.XCloseDisplay.argtypes = [ctypes.c_void_p]
d = x.XOpenDisplay(None)
value = (ctypes.c_long * 1)(int(sys.argv[1]))
x.XChangeProperty(d, x.XDefaultRootWindow(d), x.XInternAtom(d, b"_NIRU_LOCK_READY", 0),
                  33, 32, 0, value, 1)
x.XCloseDisplay(d)
' "$(printf '%d' "$LOCKWIN")"
run xprop -root _NIRU_LOCK_READY | grep -q window || fail "could not fake the lock readiness property"
pointer 600 400
ctl tag 1
sleep 0.3
out=$(ctl restart-wm) || fail "restart-wm while locked failed"
case "$out" in *locked*) ok "locked: $out" ;; *) fail "locked restart did not defer: $out" ;; esac
sleep 3
[ "$(mine -x niruwm)" = "$OLD_WM" ] || fail "the manager restarted while locked"
run xprop -root -remove _NIRU_LOCK_READY
i=0
while [ "$(mine -x niruwm)" = "$OLD_WM" ]; do
  i=$((i + 1)); [ "$i" -gt 100 ] && fail "the deferred restart did not happen after the unlock"
  sleep 0.1
done
ok "deferred restart ran after the unlock"
sleep 2
ctl tag 3
sleep 0.3
run xdotool windowactivate "$(win L3b)" 2>/dev/null || :
sleep 0.5
AFTER=$(snapshot)
[ "$AFTER" = "$BEFORE" ] || { printf '%s\n' "$AFTER" > "$T/after"; diff "$T/before" "$T/after" || :; fail "state changed across the deferred restart"; }
ok "state kept across the deferred restart"

# ── A manager without the session contract says so ─────────────────────────
run sh -c "$H/.local/bin/niructl restart-wm --check" >/dev/null 2>&1 || fail "--check refused a capable manager"
ok "restart-wm --check accepts the running manager"

ctl quit
i=0
while kill -0 "$SESSION" 2>/dev/null && [ "$(ps -o stat= -p "$SESSION" | cut -c1)" != Z ]; do
  i=$((i + 1)); [ "$i" -gt 100 ] && fail "session did not end after quit"
  sleep 0.1
done
SESSION=""
sleep 0.5
leftover=$( { mine -x niruwm; mine -x nirubar; mine -x niruidle; mine -x niructl; mine -f niruclipboard; } | tr '\n' ' ')
[ -z "${leftover% }" ] || fail "helpers survived the logout: $(ps -o pid,args -p "$(echo $leftover | tr ' ' ',')")"
ok "logout after $ROUNDS restarts stops every helper"

# An old-style session (no restart contract): the manager does not offer it.
run "$BINDIR/niruwm" >"$T/plain.log" 2>&1 &
PLAIN=$!
i=0
until prop _NET_SUPPORTING_WM_CHECK | grep -q 0x; do
  i=$((i + 1)); [ "$i" -gt 50 ] && fail "plain niruwm did not start"
  sleep 0.1
done
sleep 0.5
status=0
msg=$(ctl restart-wm 2>&1) || status=$?
kill "$PLAIN" 2>/dev/null || :
[ "$status" = 3 ] || fail "unsupported restart should exit 3, got $status ($msg)"
case "$msg" in *"log out and back in"*) ok "without the session contract: $msg" ;; *) fail "unclear fallback: $msg" ;; esac
echo "PASS restart in place"
