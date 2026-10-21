#!/bin/sh
# Offline BlueZ CLI regression with a fake bluetoothctl.
set -eu
BIN=${1:?usage: bluetooth.sh /absolute/path/to/niructl}
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT INT TERM
export HOME=$T/home XDG_STATE_HOME=$T/state BT_FIXTURE=$T
export PATH=$T/bin:$PATH
unset DISPLAY
mkdir -p "$HOME" "$T/bin"
printf 'yes\n' > "$T/powered"
printf 'yes\n' > "$T/connected"
: > "$T/actions"
cat > "$T/bin/bluetoothctl" <<'EOF'
#!/bin/sh
set -eu
case "$1" in
  show)
    [ "$(cat "$BT_FIXTURE/adapter" 2>/dev/null || echo yes)" = no ] && exit 0
    printf 'Controller 00:11:22:33:44:55 (public)\n'
    printf '\tPowered: %s\n' "$(case "$(cat "$BT_FIXTURE/powered")" in yes) echo yes;; *) echo no;; esac)"
    ;;
  devices)
    [ "$(cat "$BT_FIXTURE/adapter" 2>/dev/null || echo yes)" = no ] && exit 0
    printf 'Device AA:BB:CC:DD:EE:01 Headphones\n'
    printf 'Device AA:BB:CC:DD:EE:02 Keyboard\n'
    ;;
  info)
    case "$2" in
      AA:BB:CC:DD:EE:01)
        printf 'Device AA:BB:CC:DD:EE:01\n\tName: Headphones\n\tPaired: yes\n\tConnected: %s\n\tBattery Percentage: 0x4b (75)\n' "$(cat "$BT_FIXTURE/connected")"
        ;;
      AA:BB:CC:DD:EE:02)
        printf 'Device AA:BB:CC:DD:EE:02\n\tName: Keyboard\n\tPaired: yes\n\tConnected: no\n'
        ;;
      *) exit 1 ;;
    esac
    ;;
  power)
    printf 'power %s\n' "$2" >> "$BT_FIXTURE/actions"
    printf '%s\n' "$2" > "$BT_FIXTURE/powered"
    ;;
  connect)
    [ "$2" = AA:BB:CC:DD:EE:01 ] || exit 1
    printf 'connect %s\n' "$2" >> "$BT_FIXTURE/actions"
    printf 'yes\n' > "$BT_FIXTURE/connected"
    ;;
  disconnect)
    [ "$2" = AA:BB:CC:DD:EE:01 ] || exit 1
    printf 'disconnect %s\n' "$2" >> "$BT_FIXTURE/actions"
    printf 'no\n' > "$BT_FIXTURE/connected"
    ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$T/bin/bluetoothctl"

"$BIN" bluetooth status > "$T/status"
grep -qxF 'state=ready' "$T/status"
grep -qxF 'powered=true' "$T/status"
grep -q '^device=AA:BB:CC:DD:EE:01	Headphones	true	75$' "$T/status"
grep -q '^device=AA:BB:CC:DD:EE:02	Keyboard	false	$' "$T/status"
[ ! -s "$T/actions" ] || { echo 'FAIL status changed Bluetooth'; exit 1; }

"$BIN" bluetooth list | grep -q '^AA:BB:CC:DD:EE:02	Keyboard	false	$'
[ ! -s "$T/actions" ] || { echo 'FAIL list changed Bluetooth'; exit 1; }

"$BIN" bluetooth disconnect AA:BB:CC:DD:EE:01
grep -qxF 'disconnect AA:BB:CC:DD:EE:01' "$T/actions"
"$BIN" bluetooth connect AA:BB:CC:DD:EE:01
grep -qxF 'connect AA:BB:CC:DD:EE:01' "$T/actions"
"$BIN" bluetooth power off
grep -qxF 'power off' "$T/actions"
"$BIN" bluetooth power off
[ "$(grep -c 'power off' "$T/actions")" = 1 ] || { echo 'FAIL redundant power command'; exit 1; }

if "$BIN" bluetooth connect AA:BB:CC:DD:EE:03 > /dev/null 2>&1; then
  echo 'FAIL unpaired device was accepted'; exit 1
fi
if "$BIN" bluetooth connect not-a-mac > /dev/null 2>&1; then
  echo 'FAIL invalid MAC was accepted'; exit 1
fi

printf 'no\n' > "$T/adapter"
"$BIN" bluetooth status | grep -qxF 'adapter='
"$BIN" bluetooth power on > /dev/null 2>&1 && { echo 'FAIL power without adapter'; exit 1; }
echo 'PASS: Bluetooth status/list are read-only; power/connect/disconnect are explicit and validated'
