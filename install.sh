#!/bin/sh
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
case "${1:-}" in ""|--register-session) ;; *) echo "Usage: ./install.sh [--register-session]" >&2; exit 2;; esac
command -v cargo >/dev/null || { echo "Install Rust/Cargo and a C linker first (see README)." >&2; exit 1; }
cargo build --release --locked
bindir="${NIRU_PREFIX:-$HOME/.local}/bin"
configdir="${XDG_CONFIG_HOME:-$HOME/.config}/niru"
mkdir -p "$bindir" "$configdir/themes"
stamp=$(date +%Y%m%d-%H%M%S)
for binary in niruwm niructl; do
 if [ -f "$bindir/$binary" ]; then cp -p "$bindir/$binary" "$bindir/$binary.backup-$stamp"; fi
 install -m 755 "target/release/$binary" "$bindir/$binary.new"
 mv "$bindir/$binary.new" "$bindir/$binary"
done
if [ ! -e "$configdir/config.toml" ]; then install -m 644 config.example.toml "$configdir/config.toml"; fi
for theme in themes/*.toml; do
 target="$configdir/themes/$(basename "$theme")"
 if [ ! -e "$target" ]; then install -m 644 "$theme" "$target"; fi
done
install -m 755 session/niruwm-session "$bindir/niruwm-session"
if [ "${1:-}" = --register-session ]; then
 sessionfile=$(mktemp)
 trap 'rm -f "$sessionfile"' EXIT HUP INT TERM
 case "$bindir" in *'"'*|*'%'*|*'\'*) echo "Unsupported characters in home path" >&2; exit 1;; esac
 printf '[Desktop Entry]\nName=NIRUWM (alpha)\nComment=NIRU Noir X11 desktop\nExec="%s/niruwm-session"\nType=Application\nDesktopNames=NIRUWM\n' "$bindir" > "$sessionfile"
 sudo install -m 644 "$sessionfile" /usr/share/xsessions/niruwm.desktop
fi
echo "Installed 0.1.0-alpha.1 in $bindir. Existing configuration preserved."
