#!/bin/sh
# Offline CLI/provider regression. Never touches the real microphone.
set -eu
BIN=${1:?usage: audio-microphone.sh /absolute/path/to/niructl}
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT INT TERM
export HOME=$T/home XDG_STATE_HOME=$T/state
export AUDIO_FIXTURE=$T PATH=$T/bin:$PATH
unset DISPLAY
mkdir -p "$T/home" "$T/bin"
printf '0.45\n' > "$T/level"
printf '1\n' > "$T/muted"
printf 'ready\n' > "$T/state"
: > "$T/actions"
cat > "$T/bin/wpctl" <<'EOF'
#!/bin/sh
set -eu
case "$(cat "$AUDIO_FIXTURE/state")" in
  failed) exit 1 ;;
  slow) sleep 10; exit 1 ;;
esac
case "$1" in
  status)
    printf 'Audio\n ├─ Sources:\n'
    if [ "$(cat "$AUDIO_FIXTURE/state")" != no-input ]; then
      printf ' │  *   53. Test microphone [vol: 0.45]\n'
    fi
    printf 'Video\n ├─ Sources:\n │  *  86. Webcam (V4L2)\n'
    ;;
  get-volume)
    [ "$2" = 53 ] || exit 1
    printf 'Volume: %s' "$(cat "$AUDIO_FIXTURE/level")"
    [ "$(cat "$AUDIO_FIXTURE/muted")" != 1 ] || printf ' [MUTED]'
    printf '\n'
    ;;
  set-volume)
    [ "$2" = --limit ] && [ "$3" = 1.0 ] && [ "$4" = 53 ] || exit 1
    printf 'gain %s\n' "$5" >> "$AUDIO_FIXTURE/actions"
    ;;
  set-mute)
    [ "$2" = 53 ] || exit 1
    printf 'mute %s\n' "$3" >> "$AUDIO_FIXTURE/actions"
    ;;
  set-default) printf 'select %s\n' "$2" >> "$AUDIO_FIXTURE/actions" ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$T/bin/wpctl"

"$BIN" audio mic-status > "$T/status"
for expected in state=ready id=53 'name=Test microphone' percent=45 muted=true; do
  grep -qxF "$expected" "$T/status"
done
[ ! -s "$T/actions" ] || { echo 'FAIL status changed audio'; exit 1; }
if "$BIN" audio source 86 > /dev/null 2>&1; then
  echo 'FAIL camera accepted as microphone'; exit 1
fi
[ ! -s "$T/actions" ] || { echo 'FAIL invalid source changed defaults'; exit 1; }

"$BIN" audio mic-level up
"$BIN" audio mic-level down
"$BIN" audio mic-level set 70
grep -qxF 'gain 5%+' "$T/actions"
grep -qxF 'gain 5%-' "$T/actions"
grep -qxF 'gain 70%' "$T/actions"
! grep -q '^mute' "$T/actions" || { echo 'FAIL gain unmuted input'; exit 1; }
if "$BIN" audio mic-level set 101 > /dev/null 2>&1; then
  echo 'FAIL accepted input gain over 100%'; exit 1
fi
"$BIN" audio mic-mute on
grep -qxF 'mute 1' "$T/actions"

printf 'no-input\n' > "$T/state"
"$BIN" audio mic-status | grep -qxF state=no-input
if "$BIN" audio mic-mute > /dev/null 2>&1; then
  echo 'FAIL no-input mute pretended success'; exit 1
fi
printf 'failed\n' > "$T/state"
"$BIN" audio mic-status | grep -qxF state=unavailable
printf 'slow\n' > "$T/state"
start=$(date +%s)
"$BIN" audio mic-status | grep -qxF state=unavailable
elapsed=$(( $(date +%s) - start ))
[ "$elapsed" -lt 6 ] || { echo 'FAIL provider deadline exceeded'; exit 1; }
echo 'PASS: read-only mic state, no-input/video filtering, bounded failures and gain preserves mute'
