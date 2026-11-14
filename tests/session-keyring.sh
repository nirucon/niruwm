#!/bin/sh
# niruwm-session hands the session bus to a keyring daemon that PAM started,
# and starts nothing when PAM started none.
#
# Usage: sh tests/session-keyring.sh
set -eu
SRC=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
FUNC=$(awk '/^connect_keyring\(\) \{/{f=1} f{print} f&&/^\}/{exit}' "$SRC/session/niruwm-session")
[ -n "$FUNC" ] || { echo "FAIL connect_keyring not found in niruwm-session"; exit 1; }
grep -qx 'connect_keyring' "$SRC/session/niruwm-session" || { echo "FAIL connect_keyring is never called"; exit 1; }

T=$(mktemp -d "${TMPDIR:-/tmp}/session-keyring.XXXXXX")
trap 'rm -rf "$T"' EXIT INT TERM
mkdir -p "$T/bin"
cat > "$T/bin/gnome-keyring-daemon" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$KEYRING_LOG"
printf 'GNOME_KEYRING_CONTROL=/run/user/1000/keyring\nSSH_AUTH_SOCK=/should/not/leak\nEVIL=$(touch /tmp/x)\n'
EOF
cat > "$T/bin/pgrep" <<'EOF'
#!/bin/sh
[ "${KEYRING_RUNNING:-0}" = 1 ]
EOF
chmod +x "$T/bin/gnome-keyring-daemon" "$T/bin/pgrep"

run() {
  env -i PATH="$T/bin:/usr/bin:/bin" KEYRING_LOG="$T/log" KEYRING_RUNNING="$1" sh -c '
    eval "$1"
    connect_keyring
    printf "control=%s ssh=%s evil=%s\n" "${GNOME_KEYRING_CONTROL:-}" "${SSH_AUTH_SOCK:-}" "${EVIL:-}"
  ' sh "$FUNC"
}

: > "$T/log"
out=$(run 1)
[ "$out" = "control=/run/user/1000/keyring ssh= evil=" ] || { echo "FAIL running daemon: $out"; exit 1; }
grep -qx -- '--start --components=secrets' "$T/log" || { echo "FAIL daemon not asked to start secrets: $(cat "$T/log")"; exit 1; }
echo "OK   PAM-started daemon gets the session bus, only GNOME_KEYRING_CONTROL is exported"

: > "$T/log"
out=$(run 0)
[ "$out" = "control= ssh= evil=" ] || { echo "FAIL no daemon: $out"; exit 1; }
[ ! -s "$T/log" ] || { echo "FAIL a daemon was started without PAM: $(cat "$T/log")"; exit 1; }
echo "OK   no daemon is started when PAM started none"

rm -f "$T/bin/gnome-keyring-daemon"
out=$(run 1)
[ "$out" = "control= ssh= evil=" ] || { echo "FAIL without gnome-keyring: $out"; exit 1; }
echo "OK   machines without gnome-keyring are left alone"
echo "PASS session keyring"
