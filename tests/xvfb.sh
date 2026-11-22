#!/bin/sh
# End-to-end checks of the session helpers in a private Xvfb:
#   niruclipboard: history from real X clipboard owners (xclip), the secret
#     and size filters, INCR transfers, `list`, `copy ID`, `clear`, the
#     Python-era file format, and an idle daemon that costs nothing.
#   nirugestured: no libinput without a touchpad, a touchpad appearing and
#     going, three-finger workspace swipes under NIRUWM and the four-finger
#     overview, with a fake libinput and fixture devices.
# Usage: tests/xvfb.sh TARGET_DIR   (niruwm, niruclipboard, nirugestured)
#        XVFB_DISPLAY=:61 chooses the display. Needs Xvfb, xclip, xprop.
set -eu
DIR=$(cd "${1:?usage: tests/xvfb.sh TARGET_DIR}" && pwd)
T=$(mktemp -d)
PIDS=""
cleanup() {
  for p in $PIDS; do kill "$p" 2>/dev/null || :; done
  sleep 0.2
  rm -rf "$T"
}
trap cleanup EXIT INT TERM
fail() {
  echo "FAIL: $*" >&2
  for f in "$T"/*.log; do [ -f "$f" ] && { echo "--- $f"; tail -n 20 "$f"; }; done >&2
  exit 1
}
D=${XVFB_DISPLAY:-:$((70 + $$ % 20))}
Xvfb "$D" -screen 0 1280x800x24 -nolisten tcp >/dev/null 2>&1 &
PIDS="$PIDS $!"
export DISPLAY=$D HOME=$T/home XDG_STATE_HOME=$T/state XDG_CONFIG_HOME=$T/config
mkdir -p "$HOME" "$XDG_STATE_HOME/niru" "$XDG_CONFIG_HOME" "$T/bin"
HIST=$XDG_STATE_HOME/niru/clipboard.json
sleep 1

until_true() { # TRIES COMMAND...
  n=$1; shift
  i=0
  while ! "$@"; do
    i=$((i + 1)); [ "$i" -lt "$n" ] || return 1
    sleep 0.05
  done
}
has() { grep -qF -- "$1" "$HIST" 2>/dev/null; }
copy() { printf '%s' "$1" | xclip -selection clipboard -i; }

# ── niruclipboard ──────────────────────────────────────────────────────────
# A history written by the Python version opens unchanged.
printf '%s' '[{"id": 1700000000000000001, "text": "from python", "time": 1700000000}]' > "$HIST"
copy "already there"
"$DIR/niruclipboard" daemon > "$T/clipboard.log" 2>&1 &
C=$!; PIDS="$PIDS $C"
sleep 0.5
kill -0 "$C" || fail "the daemon did not start"
! has "already there" || fail "the text present at start was recorded"
copy "hello one"
until_true 60 has '"hello one"' || fail "a copy was not recorded"
has '"from python"' || fail "the old entry was lost"
[ "$(stat -c %a "$HIST")" = 600 ] || fail "history mode $(stat -c %a "$HIST")"
grep -q '^\[{"id": [0-9]*, "text": "hello one", "time": [0-9]*}, {"id": 1700000000000000001' "$HIST" ||
  fail "history layout: $(head -c 200 "$HIST")"
copy "my password is hunter2"
copy "line one
line two"
until_true 60 has 'line one\nline two' || fail "a multi-line copy was not recorded"
! has hunter2 || fail "a secret was recorded"
# A transfer larger than the history accepts (INCR) is skipped; the daemon
# keeps working.
head -c 300000 /dev/zero | tr '\0' 'x' | xclip -selection clipboard -i
sleep 1
copy "after the big one"
until_true 60 has '"after the big one"' || fail "the daemon stopped after a large copy"
! grep -q xxxxxxxxxx "$HIST" || fail "a large copy was recorded"
copy "hello one"
sleep 0.6
[ "$(grep -o '"hello one"' "$HIST" | wc -l)" = 1 ] || fail "a repeated copy was duplicated"
"$DIR/niruclipboard" list > "$T/list"
head -1 "$T/list" | grep -qE '^[0-9]+	[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}	hello one$' ||
  fail "list: $(head -1 "$T/list")"
grep -q '	line one line two$' "$T/list" || fail "list preview"
id=$(grep 'from python' "$T/list" | cut -f1)
"$DIR/niruclipboard" copy "$id" || fail "copy $id"
[ "$(xclip -selection clipboard -o)" = "from python" ] || fail "copy did not take the clipboard"
until_true 60 sh -c "head -c 60 '$HIST' | grep -q 'from python'" || fail "the copied entry did not move first"
"$DIR/niruclipboard" copy 42 2>/dev/null && fail "copy of a missing id succeeded"
rc=0; "$DIR/niruclipboard" copy abc 2>/dev/null || rc=$?
[ "$rc" = 2 ] || fail "copy of a non-number exited $rc"
rc=0; "$DIR/niruclipboard" bogus > /dev/null || rc=$?
[ "$rc" = 2 ] || fail "usage exit status $rc"
[ "$("$DIR/niruclipboard" clear)" = "Clipboard history cleared" ] || fail "clear"
[ "$(cat "$HIST")" = "[]" ] || fail "clear left $(cat "$HIST")"
copy "after clear"
until_true 60 has '"after clear"' || fail "no copy after clear"
[ "$(grep -o '"id"' "$HIST" | wc -l)" = 1 ] || fail "clear came back: $(cat "$HIST")"
cpu() { awk '{print $14+$15}' "/proc/$1/stat"; }
a=$(cpu "$C"); sleep 5; b=$(cpu "$C")
[ $((b - a)) -le 1 ] || fail "the idle daemon used $((b - a)) ticks in 5 s"
rss=$(awk '/VmRSS/ {print $2}' "/proc/$C/status")
[ "$rss" -lt 8000 ] || fail "daemon memory $rss kB"
echo "niruclipboard ok: idle $((b - a)) ticks in 5 s, ${rss} kB"
kill "$C"

# ── nirugestured ───────────────────────────────────────────────────────────
"$DIR/niruwm" > "$T/niruwm.log" 2>&1 &
W=$!; PIDS="$PIDS $W"
desktop() { xprop -root _NET_CURRENT_DESKTOP 2>/dev/null | awk '{print $3}'; }
until_true 100 sh -c '[ -n "$(xprop -root _NET_CURRENT_DESKTOP 2>/dev/null | awk "{print \$3}")" ]' ||
  fail "NIRUWM did not publish _NET_CURRENT_DESKTOP"
mkdir -p "$T/dev" "$T/sys/event5/device/capabilities"
mkfifo "$T/swipes"
cat > "$T/bin/libinput" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$GESTURE_CALLS"
exec 3<> "$GESTURE_FIFO"
while IFS= read -r line <&3; do printf '%s\n' "$line"; done
EOF
cat > "$T/bin/nirulauncher" <<'EOF'
#!/bin/sh
printf 'launcher %s\n' "$*" >> "$GESTURE_CALLS"
EOF
chmod +x "$T/bin/libinput" "$T/bin/nirulauncher"
export NIRUGESTURED_INPUT_DIR=$T/dev NIRUGESTURED_SYS_INPUT=$T/sys
export NIRUGESTURED_LIBINPUT=$T/bin/libinput GESTURE_CALLS=$T/calls GESTURE_FIFO=$T/swipes
export PATH="$T/bin:$PATH"
: > "$T/calls"
"$DIR/nirugestured" daemon > "$T/gestures.log" 2>&1 &
G=$!; PIDS="$PIDS $G"
until_true 60 grep -q 'no touchpad' "$T/gestures.log" || fail "no 'no touchpad' notice"
sleep 0.5
[ ! -s "$T/calls" ] || fail "libinput ran without a touchpad"
# A touchpad appears: multitouch, finger tool, not a touchscreen.
echo 2e0800000000003 > "$T/sys/event5/device/capabilities/abs"
echo "e520 10000 0 0 0 0" > "$T/sys/event5/device/capabilities/key"
echo 5 > "$T/sys/event5/device/properties"
: > "$T/dev/event5"
until_true 60 grep -qF "debug-events --device $T/dev/event5" "$T/calls" || fail "libinput did not start for the touchpad"
swipe() { # FINGERS DX DY
  {
    printf ' event5   GESTURE_SWIPE_BEGIN     +1.000s\t%s\n' "$1"
    printf ' event5   GESTURE_SWIPE_UPDATE    +1.010s\t%s %s/ %s ( 1.00/ 1.00 unaccelerated)\n' "$1" "$2" "$3"
    printf ' event5   GESTURE_SWIPE_END       +1.100s\t%s\n' "$1"
  } > "$T/swipes"
}
start=$(desktop)
swipe 3 2.50 0.10
until_true 60 sh -c "[ \"\$(xprop -root _NET_CURRENT_DESKTOP | awk '{print \$3}')\" = $(( (start + 1) % 10 )) ]" ||
  fail "three fingers right did not switch to the next workspace ($(desktop))"
swipe 3 -2.50 0.10
until_true 60 sh -c "[ \"\$(xprop -root _NET_CURRENT_DESKTOP | awk '{print \$3}')\" = $start ]" ||
  fail "three fingers left did not go back ($(desktop))"
swipe 3 -2.50 0.10
until_true 60 sh -c "[ \"\$(xprop -root _NET_CURRENT_DESKTOP | awk '{print \$3}')\" = $(( (start + 9) % 10 )) ]" ||
  fail "three fingers left did not wrap ($(desktop))"
swipe 3 0.20 0.10
swipe 4 0.10 -1.50
until_true 60 grep -qx 'launcher --overview' "$T/calls" || fail "four fingers up did not open the overview"
[ "$(desktop)" = $(( (start + 9) % 10 )) ] || fail "a short swipe switched workspaces"
a=$(cpu "$G"); sleep 3; b=$(cpu "$G")
[ $((b - a)) -le 1 ] || fail "idle nirugestured used $((b - a)) ticks"
grss=$(awk '/VmRSS/ {print $2}' "/proc/$G/status")
rm "$T/dev/event5"
until_true 100 sh -c "grep -c 'no touchpad' '$T/gestures.log' | grep -qx 2" || fail "removal not noticed"
echo "nirugestured ok: swipes, overview, hotplug; ${grss} kB"
echo "PASS: niruclipboard and nirugestured end to end"
