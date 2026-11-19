#!/bin/sh
# The proximity lock: a phone that leaves locks the session.
#
# Usage: sh tests/idle-proximity.sh BINDIR
#   BINDIR  release build directory (niruidle)
#
# Runs NIRUIDLE on a private Xvfb server with a stand-in nirulock that only
# records that it ran. NIRUIDLE_TEST_PROXIMITY replaces the Bluetooth page
# with a file saying near, away or unknown. Checks:
#   - a phone never seen does not lock;
#   - near, then away for proximity_lock_after seconds, locks once, even
#     while `niructl idle off` style inhibition is not involved and the
#     ordinary idle timers are far away;
#   - "unknown" (Bluetooth switched off) never locks;
#   - after the lock ends without the phone, nothing locks again until the
#     phone has been seen;
#   - an empty proximity_device (reloaded live) turns the lock off.
# Takes about two minutes. Never touches DISPLAY :0 or the real HOME.
set -eu

BINDIR=$(CDPATH='' cd -- "${1:?usage: idle-proximity.sh BINDIR}" && pwd)
DISP=${IDLE_TEST_DISPLAY:-:95}
NUM=${DISP#:}

T=$(mktemp -d "${TMPDIR:-/tmp}/niruidle-prox.XXXXXX")
H="$T/home"
mkdir -p "$H/.config/niruwm" "$T/bin" "$T/run"
chmod 700 "$T/run"
EVENTS="$T/events"
PHONE="$T/phone"
: > "$EVENTS"
echo away > "$PHONE"

XVFB=""
IDLE=""
fail() {
  echo "FAIL $*"
  echo "--- niruidle log ---"; cat "$T/idle.log" 2>/dev/null || :
  echo "--- events ---"; cat "$EVENTS" 2>/dev/null || :
  exit 1
}
ok() { echo "OK   $*"; }
cleanup() {
  [ -z "$IDLE" ] || kill "$IDLE" 2>/dev/null || :
  sleep 0.3
  pkill -f "$T" 2>/dev/null || :
  [ -z "$XVFB" ] || kill "$XVFB" 2>/dev/null || :
  rm -rf "$T"
}
trap cleanup EXIT INT TERM

# The stand-in locker records its start and stays until unlock is written.
cat > "$T/bin/nirulock" <<EOF
#!/bin/sh
echo "lock \$(date +%s)" >> "$EVENTS"
while [ ! -f "$T/unlock" ]; do sleep 0.2; done
rm -f "$T/unlock"
echo "unlock \$(date +%s)" >> "$EVENTS"
EOF
cat > "$T/bin/niruscreensaver" <<'EOF'
#!/bin/sh
exec sleep 100000
EOF
cp "$BINDIR/niruidle" "$T/bin/"
chmod 755 "$T/bin"/*

write_config() {
  cat > "$H/.config/niruwm/config.toml" <<EOF
[idle]
screensaver_timeout = 60
lock_after_screensaver = 60
dpms_off_after_lock = 0
proximity_device = "$1"
proximity_lock_after = 10
EOF
}
write_config "AA:BB:CC:DD:EE:FF"

Xvfb "$DISP" -screen 0 800x600x24 -nolisten tcp >"$T/xvfb.log" 2>&1 &
XVFB=$!
i=0
while [ ! -S "/tmp/.X11-unix/X$NUM" ]; do
  i=$((i + 1)); [ "$i" -gt 100 ] && fail "Xvfb did not start"
  sleep 0.1
done
(exec env -i HOME="$H" DISPLAY="$DISP" PATH="$T/bin:/usr/bin:/bin" XDG_RUNTIME_DIR="$T/run" \
  LANG=C.UTF-8 NIRUIDLE_TEST_PROXIMITY="$PHONE" "$T/bin/niruidle") >>"$T/idle.log" 2>&1 &
IDLE=$!
count() { grep -c "^$1 " "$EVENTS" || :; }
wait_count() { # KIND N SECONDS
  wc_i=0
  until [ "$(count "$1")" -ge "$2" ]; do
    wc_i=$((wc_i + 1)); [ "$wc_i" -gt $(($3 * 5)) ] && fail "no $1 #$2 within $3 s"
    sleep 0.2
  done
}

grep -q "proximity lock: AA:BB:CC:DD:EE:FF" "$T/idle.log" 2>/dev/null || sleep 1
grep -q "proximity lock: AA:BB:CC:DD:EE:FF" "$T/idle.log" || fail "the device was not read"
sleep 16
[ "$(count lock)" = 0 ] || fail "a phone never seen locked the session"
ok "a phone never seen does not lock"

echo near > "$PHONE"; sleep 7
echo away > "$PHONE"
wait_count lock 1 25
ok "near, then away: locked"
grep -q "phone out of reach for 10s: lock" "$T/idle.log" || fail "no lock reason in the log"

echo near > "$PHONE"; sleep 7
grep -q "phone back" "$T/idle.log" || fail "the phone coming back was not noticed"
ok "the phone coming back is noticed while locked"
echo away > "$PHONE"; sleep 2
touch "$T/unlock"; wait_count unlock 1 5
sleep 16
[ "$(count lock)" = 1 ] || fail "an unlock without the phone locked again"
ok "an unlock without the phone does not start a lock loop"

echo near > "$PHONE"; sleep 7
echo unknown > "$PHONE"; sleep 16
[ "$(count lock)" = 1 ] || fail "Bluetooth off (unknown) locked the session"
ok "Bluetooth off never locks"

write_config ""
sleep 6
grep -q "proximity lock: off" "$T/idle.log" || fail "turning it off was not reloaded"
echo near > "$PHONE"; sleep 7; echo away > "$PHONE"; sleep 16
[ "$(count lock)" = 1 ] || fail "locked after proximity_device was cleared"
ok "an empty proximity_device turns it off live"
echo "PASS idle-proximity"
