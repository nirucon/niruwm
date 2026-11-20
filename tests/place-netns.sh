#!/bin/sh
# Places end to end in a private network namespace: two fake routers, the
# watcher sees the default route move between them, enters and leaves the
# saved places, and waits before leaving when no network is known.
# Needs root (ip netns); nothing outside the namespace is touched.
#
#   sudo tests/place-netns.sh NIRUCTL
set -eu
BIN=${1:?usage: place-netns.sh NIRUCTL}
NS=niru-place-test-$$
T=$(mktemp -d)
cleanup() {
  [ -z "${WATCH:-}" ] || kill "$WATCH" 2>/dev/null || :
  # Whatever still runs inside the namespace (the watcher) goes with it.
  for pid in $(ip netns pids "$NS" 2>/dev/null); do kill "$pid" 2>/dev/null || :; done
  ip netns del "$NS" 2>/dev/null || :
  rm -rf "$T"
}
trap cleanup EXIT INT TERM
fail() { echo "FAIL: $*"; cat "$T/watch.log" 2>/dev/null; exit 1; }
ip netns add "$NS"
run() { ip netns exec "$NS" env XDG_CONFIG_HOME="$T/c" XDG_STATE_HOME="$T/s" HOME="$T" PATH="$T/bin:$PATH" "$@"; }
mkdir -p "$T/bin" "$T/c/niruwm"
# Actions are recorded, not done: no notifier, no display, a fake nirunotify.
cat > "$T/bin/nirunotify" <<'FAKE'
#!/bin/sh
case "$2" in status) cat "$HOME/dnd" 2>/dev/null || echo "dnd off" ;; *) echo "dnd $2" > "$HOME/dnd" ;; esac
FAKE
printf '#!/bin/sh\nexit 0\n' > "$T/bin/notify-send"
chmod +x "$T/bin/nirunotify" "$T/bin/notify-send"
run ip link set lo up
run ip link add d0 type dummy
run ip link add d1 type dummy
run ip link set d0 up
run ip link set d1 up
run ip addr add 10.10.0.2/24 dev d0
run ip addr add 10.20.0.2/24 dev d1
run ip neigh add 10.10.0.1 lladdr 02:00:00:00:10:01 dev d0 nud permanent
run ip neigh add 10.20.0.1 lladdr 02:00:00:00:20:01 dev d1 nud permanent
route() { run ip route replace default via "$1" dev "$2"; }
current() { sed -n 's/^current = "\(.*\)"/\1/p' "$T/s/niruwm/place.toml" 2>/dev/null; }
wait_for() {
  i=0
  until [ "$(current)" = "$1" ]; do
    i=$((i + 1)); [ "$i" -gt "${2:-40}" ] && fail "expected place '$1', have '$(current)'"
    sleep 0.25
  done
}
route 10.10.0.1 d0
run "$BIN" place save Office >/dev/null
run "$BIN" place set Office dnd on >/dev/null
route 10.20.0.1 d1
run "$BIN" place save Home >/dev/null
printf '[[place]]\nname = "Office"\ngateway = "10.10.0.1"\ngateway_mac = "02:00:00:00:10:01"\ndnd = true\non_enter = ["touch $HOME/entered-$NIRUX_PLACE"]\non_leave = ["touch $HOME/left-$NIRUX_PLACE"]\n\n[[place]]\nname = "Home"\ngateway = "10.20.0.1"\ngateway_mac = "02:00:00:00:20:01"\n' > "$T/c/niruwm/places.toml"
run "$BIN" place watch >"$T/watch.log" 2>&1 &
WATCH=$!
wait_for Home
route 10.10.0.1 d0
wait_for Office
sleep 0.5
[ -f "$T/entered-Office" ] || fail "on_enter did not run"
[ "$(cat "$T/dnd")" = "dnd on" ] || fail "dnd was not turned on"
# A router with the same address but another MAC is not the office.
run ip neigh replace 10.10.0.1 lladdr 02:00:00:00:99:99 dev d0 nud permanent
route 10.20.0.1 d1
wait_for Home
[ -f "$T/left-Office" ] || fail "on_leave did not run"
[ "$(cat "$T/dnd")" = "dnd off" ] || fail "dnd was not put back"
route 10.10.0.1 d0
sleep 3
[ "$(current)" = "Home" ] || [ "$(current)" = "" ] || fail "an unknown router counted as a place: $(current)"
[ "$(current)" = "Home" ] || fail "left at once instead of waiting"
wait_for "" 120
echo "PASS place-netns"
