#!/bin/sh
# Programs pause the idle ladder through org.freedesktop.ScreenSaver.
#
# Usage: sh tests/idle-app-inhibit.sh BINDIR
#   BINDIR  release build directory (niruidle, niructl)
#
# Runs NIRUIDLE on a private Xvfb server and a private session bus, with a
# one-minute screensaver and stand-ins for niruscreensaver and nirulock that
# only record that they ran. Checks:
#   - NIRUIDLE owns the name and answers Introspect and GetActive;
#   - an Inhibit holds the screensaver back past its timeout, and
#     `niructl idle status` lists the program;
#   - a holder killed with SIGKILL (no UnInhibit) ends its request, and the
#     screensaver comes one timeout later (the timer restarted from zero);
#   - UnInhibit ends a request; a stranger's UnInhibit does not;
#   - an Inhibit while the screensaver is up takes it down;
#   - [idle] honor_app_inhibit = false ignores requests (reloaded live);
#   - a name owned by another program is not taken, and is taken once free;
#   - without a session bus NIRUIDLE keeps running.
#
# Needs: Xvfb, dbus-daemon, python3 with GObject introspection (Gio), gdbus.
# Takes about five minutes. Never touches DISPLAY :0 or the real HOME.
set -eu

BINDIR=$(CDPATH='' cd -- "${1:?usage: idle-app-inhibit.sh BINDIR}" && pwd)
DISP=${IDLE_TEST_DISPLAY:-:96}
NUM=${DISP#:}

T=$(mktemp -d "${TMPDIR:-/tmp}/niruidle-apps.XXXXXX")
H="$T/home"
mkdir -p "$H/.config/niruwm" "$T/bin" "$T/run"
chmod 700 "$T/run"
EVENTS="$T/events"
: > "$EVENTS"

XVFB=""
BUS_PID=""
IDLE=""
HOLDERS=""
fail() {
  echo "FAIL $*"
  echo "--- niruidle log ---"; cat "$T/idle.log" 2>/dev/null || :
  echo "--- events ---"; cat "$EVENTS" 2>/dev/null || :
  exit 1
}
ok() { echo "OK   $*"; }
cleanup() {
  for p in $HOLDERS $IDLE; do kill "$p" 2>/dev/null || :; done
  sleep 0.3
  pkill -f "$T" 2>/dev/null || :
  [ -z "$XVFB" ] || kill "$XVFB" 2>/dev/null || :
  [ -z "$BUS_PID" ] || kill "$BUS_PID" 2>/dev/null || :
  rm -rf "$T"
}
trap cleanup EXIT INT TERM

# Stand-ins: they record when they start, nothing else.
cat > "$T/bin/niruscreensaver" <<EOF
#!/bin/sh
echo "saver \$(date +%s) \$\$" >> "$EVENTS"
exec sleep 100000
EOF
cat > "$T/bin/nirulock" <<EOF
#!/bin/sh
echo "lock \$(date +%s)" >> "$EVENTS"
exec sleep 100000
EOF
cp "$BINDIR/niruidle" "$BINDIR/niructl" "$T/bin/"
chmod 755 "$T/bin"/*

write_config() {
  cat > "$H/.config/niruwm/config.toml" <<EOF
[idle]
screensaver_timeout = 1
lock_after_screensaver = 1
dpms_off_after_lock = 0
honor_app_inhibit = $1
EOF
}
write_config true

Xvfb "$DISP" -screen 0 1024x768x24 -nolisten tcp >"$T/xvfb.log" 2>&1 &
XVFB=$!
i=0
while [ ! -S "/tmp/.X11-unix/X$NUM" ]; do
  i=$((i + 1)); [ "$i" -gt 100 ] && fail "Xvfb did not start"
  sleep 0.1
done
start_bus() {
  info=$(env -i HOME="$H" PATH=/usr/bin:/bin dbus-daemon --session --fork --print-address=1 --print-pid=1) ||
    fail "cannot start a private session bus"
  BUS=$(printf '%s\n' "$info" | sed -n 1p)
  BUS_PID=$(printf '%s\n' "$info" | sed -n 2p)
}
start_bus
run() {
  env -i HOME="$H" DISPLAY="$DISP" PATH="$T/bin:/usr/bin:/bin" XDG_RUNTIME_DIR="$T/run" \
    LANG=C.UTF-8 DBUS_SESSION_BUS_ADDRESS="$BUS" "$@"
}
# In the background with $! naming the program itself (exec all the way
# down), so a kill reaches it and not a subshell.
spawn() {
  (exec env -i HOME="$H" DISPLAY="$DISP" PATH="$T/bin:/usr/bin:/bin" XDG_RUNTIME_DIR="$T/run" \
    LANG=C.UTF-8 DBUS_SESSION_BUS_ADDRESS="$BUS" "$@") &
}
start_idle() {
  spawn "$T/bin/niruidle" >>"$T/idle.log" 2>&1
  IDLE=$!
}
wait_log() {
  wl_i=0
  until grep -qF -- "$1" "$T/idle.log"; do
    wl_i=$((wl_i + 1)); [ "$wl_i" -gt "${2:-100}" ] && fail "no '$1' in the niruidle log"
    sleep 0.1
  done
}
stop_idle() {
  kill "$IDLE" 2>/dev/null || :
  wait "$IDLE" 2>/dev/null || :
  IDLE=""
}
count() { grep -c "^$1 " "$EVENTS" || :; }
# A program that holds a request: Inhibit, then wait. With a FIFO it calls
# UnInhibit when a line arrives on it.
cat > "$T/holder.py" <<'EOF'
import sys
from gi.repository import Gio, GLib
path, app, reason = sys.argv[1], sys.argv[2], sys.argv[3]
ctl = sys.argv[4] if len(sys.argv) > 4 else None
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
def call(method, args, out):
    return bus.call_sync("org.freedesktop.ScreenSaver", path, "org.freedesktop.ScreenSaver",
                         method, args, GLib.VariantType(out) if out else None,
                         Gio.DBusCallFlags.NONE, 5000, None)
cookie = call("Inhibit", GLib.Variant("(ss)", (app, reason)), "(u)").unpack()[0]
import os
print(cookie, flush=True)
print("pid", os.getpid(), flush=True)
if ctl:
    with open(ctl) as f:
        for line in f:
            if line.strip() == "un":
                call("UnInhibit", GLib.Variant("(u)", (cookie,)), None)
                print("uninhibited", flush=True)
GLib.MainLoop().run()
EOF
HOLDN=0
hold() {
  HOLDN=$((HOLDN + 1))
  HOLDER_OUT="$T/holder.out.$HOLDN"
  spawn python3 "$T/holder.py" "$@" > "$HOLDER_OUT" 2>&1
  # The program itself, not the shell that started it.
  hd_i=0
  until HOLDER=$(sed -n 's/^pid //p' "$HOLDER_OUT") && [ -n "$HOLDER" ]; do
    hd_i=$((hd_i + 1)); [ "$hd_i" -gt 100 ] && fail "the holder did not start: $(cat "$HOLDER_OUT")"
    sleep 0.1
  done
  HOLDERS="$HOLDERS $HOLDER"
}

# ── The name, Introspect, GetActive ─────────────────────────────────────────
start_idle
wait_log "owns org.freedesktop.ScreenSaver"
ok "niruidle owns org.freedesktop.ScreenSaver"
run gdbus introspect --session --dest org.freedesktop.ScreenSaver --object-path /org/freedesktop/ScreenSaver |
  grep -q "Inhibit" || fail "Introspect does not list Inhibit"
active=$(run gdbus call --session --dest org.freedesktop.ScreenSaver --object-path /ScreenSaver --method org.freedesktop.ScreenSaver.GetActive)
[ "$active" = "(false,)" ] || fail "GetActive: $active"
ok "Introspect and GetActive answer ($active)"
# A short-lived caller (gdbus exits after the call): its request ends at once.
run gdbus call --session --dest org.freedesktop.ScreenSaver --object-path /org/freedesktop/ScreenSaver \
  --method org.freedesktop.ScreenSaver.Inhibit gdbus-test "one shot" >/dev/null
wait_log "app inhibit off: gdbus-test (the program left)" 50
ok "a caller that leaves the bus takes its request with it"

# ── A held request pauses the ladder past its timeout ──────────────────────
hold /org/freedesktop/ScreenSaver Helium "Playing video"
H1=$HOLDER
wait_log "app inhibit on: Helium (Playing video)"
wait_log "idle inhibition on (1 program request)" 80
status=$(run "$T/bin/niructl" idle status)
printf '%s\n' "$status" | sed -n 1p | grep -qx "idle off" || fail "first status line changed: $status"
printf '%s\n' "$status" | grep -qx "app inhibit: Helium (Playing video)" || fail "idle status does not list the program: $status"
ok "niructl idle status lists it: $(printf '%s' "$status" | tr '\n' '|')"
sleep 75
[ "$(count saver)" -eq 0 ] || fail "the screensaver started while a program held a request"
ok "no screensaver after 75 s idle with a request held"

# ── SIGKILL: the request ends with the program, the timer starts over ──────
kill -9 "$H1"
t0=$(date +%s)
wait_log "app inhibit off: Helium (the program left)" 50
wait_log "idle inhibition off" 80
i=0
until [ "$(count saver)" -ge 1 ]; do
  i=$((i + 1)); [ "$i" -gt 100 ] && fail "no screensaver within 100 s after the holder was killed"
  sleep 1
done
t1=$(awk '/^saver /{print $2; exit}' "$EVENTS")
dt=$((t1 - t0))
[ "$dt" -ge 50 ] || fail "the screensaver came $dt s after the request ended; the timer did not start over"
ok "holder killed: request gone, screensaver $dt s later"
sleep 6
active=$(run gdbus call --session --dest org.freedesktop.ScreenSaver --object-path /ScreenSaver --method org.freedesktop.ScreenSaver.GetActive)
[ "$active" = "(true,)" ] || fail "GetActive while the screensaver is up: $active"
ok "GetActive is true while the screensaver is up"

# ── An Inhibit while the screensaver is up takes it down ───────────────────
mkfifo "$T/ctl"
hold /ScreenSaver vlc "Playing some media." "$T/ctl"
H2=$HOLDER
exec 7>"$T/ctl"
wait_log "app inhibit on: vlc (Playing some media.)"
SAVER_PID=$(awk '/^saver /{p=$3} END{print p}' "$EVENTS")
i=0
while kill -0 "$SAVER_PID" 2>/dev/null; do
  i=$((i + 1)); [ "$i" -gt 80 ] && fail "the screensaver stayed up after an Inhibit"
  sleep 0.1
done
ok "an Inhibit takes the screensaver down (KDE path /ScreenSaver)"
# A stranger cannot end it with the right cookie.
cookie=$(sed -n 1p "$HOLDER_OUT")
run gdbus call --session --dest org.freedesktop.ScreenSaver --object-path /ScreenSaver \
  --method org.freedesktop.ScreenSaver.UnInhibit "$cookie" >/dev/null
sleep 6
! grep -qF "app inhibit off: vlc" "$T/idle.log" || fail "another connection ended vlc's request"
ok "another connection's UnInhibit with the same cookie changes nothing"
echo un >&7
wait_log "app inhibit off: vlc" 50
wait_log "idle inhibition off" 80
ok "UnInhibit ends the request"
exec 7>&-
kill "$H2" 2>/dev/null || :

# ── honor_app_inhibit = false ───────────────────────────────────────────────
saver_before=$(count saver)
write_config false
wait_log "requests from programs are ignored" 100
hold /org/freedesktop/ScreenSaver mpv "video"
wait_log "app inhibit on: mpv (video)"
t0=$(date +%s)
i=0
until [ "$(count saver)" -gt "$saver_before" ]; do
  i=$((i + 1)); [ "$i" -gt 90 ] && fail "with honor_app_inhibit = false a request still held the screensaver back"
  sleep 1
done
ok "honor_app_inhibit = false: screensaver $(( $(date +%s) - t0 )) s later despite a request"
status=$(run "$T/bin/niructl" idle status)
! printf '%s\n' "$status" | grep -q "app inhibit" || fail "idle status lists an ignored request: $status"
ok "an ignored request is not listed by niructl idle status"
stop_idle
for p in $HOLDERS; do kill "$p" 2>/dev/null || :; done
HOLDERS=""
write_config true

# ── Another owner keeps the name; NIRUIDLE takes it once it is free ────────
kill "$BUS_PID"
start_bus
cat > "$T/owner.py" <<'EOF'
from gi.repository import Gio, GLib
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
r = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                  "RequestName", GLib.Variant("(su)", ("org.freedesktop.ScreenSaver", 4)),
                  GLib.VariantType("(u)"), Gio.DBusCallFlags.NONE, 5000, None)
print(r.unpack()[0], flush=True)
GLib.MainLoop().run()
EOF
spawn python3 "$T/owner.py" > "$T/owner.out" 2>&1
OWNER=$!
HOLDERS="$OWNER"
i=0
until grep -qx 1 "$T/owner.out" 2>/dev/null; do
  i=$((i + 1)); [ "$i" -gt 50 ] && fail "the stand-in owner did not get the name"
  sleep 0.1
done
: > "$T/idle.log"
start_idle
wait_log "is owned by" 50
grep -q "is owned by :.*python" "$T/idle.log" || fail "the owner is not named: $(cat "$T/idle.log")"
ok "$(grep -o 'org.freedesktop.ScreenSaver is owned by [^;]*' "$T/idle.log")"
kill "$OWNER"
wait_log "was released; niruidle owns it now" 50
ok "the name is taken once the other owner leaves"
stop_idle

# ── No session bus: NIRUIDLE keeps working ──────────────────────────────────
: > "$T/idle.log"
(exec env -i HOME="$H" DISPLAY="$DISP" PATH="$T/bin:/usr/bin:/bin" XDG_RUNTIME_DIR="$T/run" \
  "$T/bin/niruidle" >>"$T/idle.log" 2>&1) &
IDLE=$!
wait_log "no session bus" 50
sleep 6
kill -0 "$IDLE" 2>/dev/null || fail "niruidle stopped without a session bus"
ok "without a session bus niruidle runs on ($(grep -c . "$T/idle.log") log lines)"

echo "PASS idle app inhibit"
