#!/bin/sh
# Exercise actual component builds/download checks inside an isolated home.
set -eu
SOURCE=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
BAR=${1:?pass the absolute path of the pinned nirubar binary}
# Preserve the real build toolchain before isolating the install user's home.
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
export HOME="$fixture/home" XDG_CONFIG_HOME="$fixture/config" XDG_STATE_HOME="$fixture/state"
export NIRUWM_SESSION_DIR="$fixture/xsessions"
mkdir -p "$HOME" "$fixture/release"
sh "$SOURCE/install.sh" --build --prefix "$fixture/source"
sh "$SOURCE/install.sh" --check --prefix "$fixture/source"
test ! -e "$fixture/source/bin/nirubar"
for binary in niruwm niructl niruidle; do cp "$SOURCE/target/release/$binary" "$fixture/release/$binary"; done
cp "$BAR" "$fixture/release/nirubar"
cp "$SOURCE/session/niruwm-session" "$fixture/release/niruwm-session"
(cd "$fixture/release" && sha256sum niruwm niructl niruidle nirubar niruwm-session > SHA256SUMS)
NIRUWM_BASE_URL="file://$fixture/release" sh "$SOURCE/install.sh" --prefix "$fixture/prebuilt"
sh "$SOURCE/install.sh" --check --prefix "$fixture/prebuilt"
test "$("$fixture/prebuilt/bin/niruwm-session" --version)" = "$(sh "$SOURCE/session/niruwm-session" --version)"
test "$("$fixture/prebuilt/bin/nirubar" --version)" = "NIRUBAR $(cat "$SOURCE/nirubar-version")"
rm "$fixture/source/bin/niruwm"
ln -s "$fixture/share/nirux/current/bin/niruwm" "$fixture/source/bin/niruwm"
if sh "$SOURCE/install.sh" --build --prefix "$fixture/source" > "$fixture/refusal.log" 2>&1; then
  printf '%s\n' 'FAIL: standalone installer replaced a NIRUX-owned link' >&2
  exit 1
fi
test -L "$fixture/source/bin/niruwm"
printf '%s\n' 'PASS: independent build, separately versioned prebuilt pair, pinned session and NIRUX ownership refusal'
