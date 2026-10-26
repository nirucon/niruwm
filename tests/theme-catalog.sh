#!/bin/sh
# Theme catalogue, import, hide/show and removal without a display.
set -eu
bin=${1:-target/release/niructl}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export HOME="$tmp" XDG_CONFIG_HOME="$tmp/.config" XDG_PICTURES_DIR="$tmp/Pictures"
mkdir -p "$HOME/.config/niruwm/themes" "$HOME/Pictures/Wallpapers/NIRU"
cat > "$tmp/custom.toml" <<'THEME'
name = "Custom Test"
background = "101820"
foreground = "e0f0ff"
muted = "90a0b0"
border = "203040"
focus = "40a0ff"
THEME
base64 -d > "$tmp/wall.png" <<'PNG'
iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=
PNG
fail() { printf 'theme-catalog: %s\n' "$*" >&2; exit 1; }

"$bin" theme-catalog | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d["active"]=="niru-noir"; assert len([t for t in d["themes"] if t["builtin"]])==10'
"$bin" theme-import "$tmp/custom.toml" --id custom-test --wallpaper "$tmp/wall.png"
"$bin" theme-catalog | python3 -c 'import json,sys; d=json.load(sys.stdin); e=[t for t in d["themes"] if t["id"]=="custom-test"][0]; assert e["valid"] and e["wallpaper_available"] and not e["builtin"]'
"$bin" theme-hide niru-satie
"$bin" theme-catalog | python3 -c 'import json,sys; d=json.load(sys.stdin); assert "niru-satie" in d["hidden"]'
if "$bin" theme-hide niru-noir >/dev/null 2>&1; then fail 'hid the active theme'; fi
"$bin" theme-show niru-satie
"$bin" theme-catalog | python3 -c 'import json,sys; d=json.load(sys.stdin); assert "niru-satie" not in d["hidden"]'
"$bin" theme-remove custom-test
[ ! -e "$HOME/.config/niruwm/themes/custom-test.toml" ] || fail 'user theme remained'
[ ! -e "$HOME/Pictures/Wallpapers/NIRU/nirux-wallpaper-custom-test-4k.png" ] || fail 'imported wallpaper remained'
"$bin" theme-catalog | python3 -c 'import json,sys; d=json.load(sys.stdin); assert all(t["id"]!="custom-test" for t in d["themes"])'
printf 'theme-catalog: ok\n'
