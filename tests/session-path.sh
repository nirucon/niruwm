#!/bin/sh
# PATH is set once per directory by niruwm-session, also after many restarts
# in place (each one runs the script again in the same environment).
#
# Usage: sh tests/session-path.sh
set -eu
SRC=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
FUNC=$(awk '/^dedupe_path\(\) \{/{f=1} f{print} f&&/^\}/{exit}' "$SRC/session/niruwm-session")
[ -n "$FUNC" ] || { echo "FAIL dedupe_path not found in niruwm-session"; exit 1; }
OLD_IFS=$IFS
eval "$FUNC"

check() {
  got=$(dedupe_path "$1")
  [ "$got" = "$2" ] || { echo "FAIL dedupe_path '$1': got '$got', want '$2'"; exit 1; }
  echo "OK   '$1' -> '$2'"
}
check "/a:/b:/c" "/a:/b:/c"
check "/s:/h/.local/bin:/s:/h/.local/bin:/usr/bin:/bin" "/s:/h/.local/bin:/usr/bin:/bin"
check "/a::/b:" "/a:/b"
check "/usr/bin:/bin:/usr/bin" "/usr/bin:/bin"
check "/with space:/b:/with space" "/with space:/b"
check "/glob*:/b" "/glob*:/b"
check "" ""

# Thirty restarts in place: the same two directories are put first each time.
P="/usr/local/bin:/usr/bin:/bin"
i=0
while [ "$i" -lt 30 ]; do
  P=$(dedupe_path "/gen/bin:/home/u/.local/bin:$P")
  i=$((i + 1))
done
check "$P" "/gen/bin:/home/u/.local/bin:/usr/local/bin:/usr/bin:/bin"

# The function leaves globbing and IFS as they were.
case "$-" in *f*) echo "FAIL set -f leaked"; exit 1 ;; esac
[ "$IFS" = "$OLD_IFS" ] || { echo "FAIL IFS changed"; exit 1; }

# The lines of the session script itself, with a PATH full of repeats.
out=$(env -i HOME=/nonexistent SELF_DIR=/self PATH="/usr/bin:/self:/bin:/usr/bin" sh -c '
  sed -n "/^dedupe_path() {/,/^export PATH/p" "$1" > "$2"
  . "$2"
  printf "%s\n" "$PATH"
' sh "$SRC/session/niruwm-session" "${TMPDIR:-/tmp}/session-path.$$")
rm -f "${TMPDIR:-/tmp}/session-path.$$"
case "$out" in
  "/self:/nonexistent/.local/bin:/usr/bin:/bin") echo "OK   session PATH: $out" ;;
  *) echo "FAIL session PATH: $out"; exit 1 ;;
esac
echo "PASS session PATH"
