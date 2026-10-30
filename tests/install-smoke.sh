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
cp "$SOURCE/session/niruclipboard" "$fixture/release/niruclipboard"
cp "$SOURCE/session/nirugestured" "$fixture/release/nirugestured"
cp "$SOURCE/nirubar-version" "$fixture/release/nirubar-version"
(cd "$fixture/release" && sha256sum niruwm niructl niruidle nirubar niruwm-session niruclipboard nirugestured nirubar-version > SHA256SUMS)
NIRUWM_BASE_URL="file://$fixture/release" sh "$SOURCE/install.sh" --prefix "$fixture/prebuilt"
sh "$SOURCE/install.sh" --check --prefix "$fixture/prebuilt"
test "$("$fixture/prebuilt/bin/niruwm-session" --version)" = "$(sh "$SOURCE/session/niruwm-session" --version)"
test "$("$fixture/prebuilt/bin/nirubar" --version)" = "NIRUBAR $(cat "$SOURCE/nirubar-version")"

# An older release has no nirubar-version asset. Its tag metadata, not this
# checkout's nirubar-version, defines the expected bundled bar.
mkdir -p "$fixture/oldrelease" "$fixture/oldmeta"
for binary in niruwm niructl niruidle; do cp "$SOURCE/target/release/$binary" "$fixture/oldrelease/$binary"; done
cp "$SOURCE/session/niruwm-session" "$fixture/oldrelease/niruwm-session"
cp "$SOURCE/session/niruclipboard" "$fixture/oldrelease/niruclipboard"
cp "$SOURCE/session/nirugestured" "$fixture/oldrelease/nirugestured"
cat > "$fixture/oldrelease/nirubar" <<'EOF'
#!/bin/sh
[ "$1" = --version ] && printf 'NIRUBAR 0.18.6\n'
exit 0
EOF
chmod +x "$fixture/oldrelease/nirubar"
printf '0.18.6\n' > "$fixture/oldmeta/nirubar-version"
(cd "$fixture/oldrelease" && sha256sum niruwm niructl niruidle nirubar niruwm-session niruclipboard nirugestured > SHA256SUMS)
version=$(awk -F '"' '/^version = / {print $2; exit}' "$SOURCE/Cargo.toml")
NIRUWM_VERSION="$version" NIRUWM_BASE_URL="file://$fixture/oldrelease" \
  NIRUWM_METADATA_BASE_URL="file://$fixture/oldmeta" \
  sh "$SOURCE/install.sh" --prefix "$fixture/oldprebuilt"
test "$("$fixture/oldprebuilt/bin/nirubar" --version)" = 'NIRUBAR 0.18.6'
rm "$fixture/source/bin/niruwm"
ln -s "$fixture/share/nirux/current/bin/niruwm" "$fixture/source/bin/niruwm"
if sh "$SOURCE/install.sh" --build --prefix "$fixture/source" > "$fixture/refusal.log" 2>&1; then
  printf '%s\n' 'FAIL: standalone installer replaced a NIRUX-owned link' >&2
  exit 1
fi
test -L "$fixture/source/bin/niruwm"
printf '%s\n' 'PASS: independent build, old-release bar metadata, separately versioned prebuilt pair, pinned session and NIRUX ownership refusal'
