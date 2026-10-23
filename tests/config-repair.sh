#!/bin/sh
# A truncated config must be repaired by `niructl theme`, never preserved.
#
# This is the regression that broke theme wallpapers on Arch: the user's
# config.toml had been reduced to a single `theme = "..."` line. Because
# [wallpaper] was gone, NIRUWM silently used its built-in `enabled = false`
# default, so the wallpaper was never set and never followed the theme, while
# the bar still changed colour. `niructl theme` wrote the file back in the same
# broken shape, so the state survived every theme switch.
#
# Runs without an X server on purpose: repairing the file must not depend on a
# display, because the same command is used from a TTY and over ssh.
set -eu
BIN=${1:?usage: config-repair.sh BINDIR}
SOURCE=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

TEST=$(mktemp -d)
trap 'rm -rf "$TEST"' EXIT HUP INT TERM
export HOME="$TEST/home" XDG_CONFIG_HOME="$TEST/home/.config"
unset DISPLAY XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS
mkdir -p "$XDG_CONFIG_HOME/niruwm"
# Reproduce the installed layout exactly: the binary lives in
# <state>/generations/ID/bin while config.example.toml sits in <state>. A
# repair that cannot find the example still works, but it silently loses all
# the comments that document the options, so this must be tested.
STATE="$TEST/share/nirux"
GEN="$STATE/generations/0.10.121-test"
mkdir -p "$GEN/bin" "$STATE"
cp "$BIN/niructl" "$GEN/bin/niructl" ||
  { printf 'missing niructl in %s\n' "$BIN" >&2; exit 1; }
cp "$SOURCE/config.example.toml" "$STATE/config.example.toml" ||
  { printf 'missing config.example.toml in %s\n' "$SOURCE" >&2; exit 1; }
NIRUCTL="$GEN/bin/niructl"
CONFIG="$XDG_CONFIG_HOME/niruwm/config.toml"

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# 1. The exact broken shape from Arch.
printf 'theme = "niru-satie"\n' > "$CONFIG"
# Apply is expected to fail without a display; only the repair matters here.
"$NIRUCTL" theme niru-nord >"$TEST/out" 2>&1 || true

[ "$(wc -l < "$CONFIG")" -gt 20 ] ||
  fail "truncated config was not repaired: $(wc -c < "$CONFIG") bytes"
for section in wallpaper autostart bar nightlight brightness monitors; do
  grep -q "^\[$section\]" "$CONFIG" || fail "repair did not add [$section]"
done
# The shipped example carries the documentation; a repair must keep it.
grep -q '^#' "$CONFIG" || fail "repair lost every comment from config.example.toml"
[ "$(grep -c '^#' "$CONFIG")" -gt 100 ] ||
  fail "repair kept only $(grep -c '^#' "$CONFIG") comments; the example was not found"
# A repaired wallpaper section must be enabled, or theme wallpapers stay off.
awk '/^\[wallpaper\]/{p=1} /^\[/ && !/^\[wallpaper\]/{p=0} p' "$CONFIG" |
  grep -qx 'enabled = true' || fail "repaired [wallpaper] is not enabled"
awk '/^\[wallpaper\]/{p=1} /^\[/ && !/^\[wallpaper\]/{p=0} p' "$CONFIG" |
  grep -qx 'follow_theme = true' || fail "repaired [wallpaper] does not follow the theme"
grep -qx 'theme = "niru-nord"' "$CONFIG" || fail "requested theme was not written"
[ -f "$XDG_CONFIG_HOME/niruwm/config.toml.bak" ] ||
  fail "no backup of the previous config was kept"

# 2. A complete config is left untouched and user choices survive.
cp "$SOURCE/config.example.toml" "$CONFIG"
before_lines=$(wc -l < "$CONFIG")
"$NIRUCTL" theme niru-satie >"$TEST/out2" 2>&1 || true
grep -q '^enabled = true$' "$CONFIG" || fail "explicit [wallpaper] enabled was lost"
grep -qx 'theme = "niru-satie"' "$CONFIG" || fail "theme was not written"
[ "$(wc -l < "$CONFIG")" -ge "$before_lines" ] ||
  fail "config lost lines during write"

# 3. Invalid TOML is refused, never overwritten.
printf 'theme = "x"\nthis is [not toml\n' > "$CONFIG"
broken=$(cksum < "$CONFIG")
if "$NIRUCTL" theme niru-nord >"$TEST/out3" 2>&1; then
  fail "invalid TOML was accepted"
fi
[ "$(cksum < "$CONFIG")" = "$broken" ] ||
  fail "invalid config was overwritten instead of refused"
grep -q 'refusing to rewrite invalid' "$TEST/out3" ||
  fail "no explanation was printed for an invalid config"

printf 'config repair: ok\n'
