#!/bin/sh
# Real startx + Xvfb, run as an unprivileged user without a login runtime.
set -eu
SOURCE=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
BIN=${1:?usage: wallpaper-session.sh BINDIR WALLPAPERDIR}
ASSETS=${2:?missing wallpaper directory}
[ -x "$BIN/nirubar" ] || { printf 'missing pinned nirubar in %s\n' "$BIN" >&2; exit 1; }
TEST=$(mktemp -d)
export HOME="$TEST/home" XDG_CONFIG_HOME="$TEST/home/.config"
export XDG_CACHE_HOME="$TEST/home/.cache" XDG_STATE_HOME="$TEST/home/.local/state"
export DISPLAY=:96 PATH="$BIN:$PATH"
unset XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS
mkdir -p "$HOME/.local/bin" "$XDG_CONFIG_HOME/niruwm" "$HOME/Bilder med blanksteg/Wallpapers/NIRU"
# Record the selected file while still drawing it with the real setter.
FEH_REAL=$(command -v feh)
export FEH_REAL
cat > "$HOME/.local/bin/feh" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" > "$HOME/last-wallpaper-args"
[ ! -f "$HOME/fail-setter" ] || exit 9
exec "$FEH_REAL" "$@"
EOF
chmod +x "$HOME/.local/bin/feh"
cp "$SOURCE/session/niruwm-session" "$HOME/.local/bin/"
chmod +x "$HOME/.local/bin/niruwm-session"
ln -s "$BIN/niruwm" "$HOME/.local/bin/niruwm"
cp -R "$SOURCE/themes" "$XDG_CONFIG_HOME/niruwm/"
cp "$ASSETS/nirux-wallpaper-noir-4k.png" "$ASSETS/nirux-wallpaper-nord-4k.png" "$HOME/Bilder med blanksteg/Wallpapers/NIRU/"
printf 'XDG_PICTURES_DIR="$HOME/Bilder med blanksteg"\n' > "$XDG_CONFIG_HOME/user-dirs.dirs"
cat > "$XDG_CONFIG_HOME/niruwm/config.toml" <<'EOF'
theme = "niru-noir"
[wallpaper]
enabled = true
mode = "fixed"
follow_theme = true
# Deliberately stale English path: runtime must discover the XDG assets.
theme_dir = "~/Pictures/Wallpapers/NIRU"
EOF
cat > "$XDG_CONFIG_HOME/niruwm/session.conf" <<'EOF'
START_PICOM=0
START_IDLE=0
START_SLEEP_LOCK=0
NOTIFICATION_PROVIDER=external
WELCOME_NOTIFICATION=off
EOF
printf '#!/bin/sh\nexec "$HOME/.local/bin/niruwm-session"\n' > "$HOME/.xinitrc"
chmod +x "$HOME/.xinitrc"
cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    cat "$HOME/startx.log" 2>/dev/null || :
    cat "$XDG_STATE_HOME/niruwm/session.log" 2>/dev/null || :
  fi
  niructl quit >/dev/null 2>&1 || :
  [ -z "${START:-}" ] || kill "$START" 2>/dev/null || :
  rm -rf "$TEST"
}
trap cleanup EXIT HUP INT TERM
startx -- "$(command -v Xvfb)" :96 -screen 0 1024x768x24 -nolisten tcp -noreset > "$HOME/startx.log" 2>&1 &
START=$!
i=0
while ! xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -q 'window id'; do
  i=$((i + 1))
  if [ "$i" -ge 200 ]; then
    cat "$HOME/startx.log"
    [ ! -f "$XDG_STATE_HOME/niruwm/session.log" ] || cat "$XDG_STATE_HOME/niruwm/session.log"
    exit 1
  fi
  sleep 0.1
done
pixmap() { xprop -root _XROOTPMAP_ID; }
i=0
while ! pixmap | grep -q 'pixmap id'; do
  i=$((i + 1)); [ "$i" -lt 200 ] || exit 1
  sleep 0.1
done
before=$(pixmap)
printf '%s\n' "$before" | grep -q 'pixmap id'
i=0
while ! xwininfo -root -tree | grep -q NIRUBAR; do
  i=$((i + 1)); [ "$i" -lt 200 ] || exit 1
  sleep 0.1
done
niructl theme niru-nord
after=$(pixmap)
[ "$before" != "$after" ]
niructl wallpaper status | grep -q '/Bilder med blanksteg/Wallpapers/NIRU/nirux-wallpaper-nord-4k.png'
printf 'PASS: startx without login runtime; theme wallpaper follows localized XDG path\n'

niructl wallpaper set "$HOME/Bilder med blanksteg/Wallpapers/NIRU/nirux-wallpaper-noir-4k.png"
niructl theme niru-noir
# Reload may allocate a new pixmap even for the same file. Assert the actual
# image passed to the real setter, rather than comparing resource IDs here.
grep -Fqx "$HOME/Bilder med blanksteg/Wallpapers/NIRU/nirux-wallpaper-noir-4k.png" "$HOME/last-wallpaper-args"
niructl wallpaper status | grep -qx 'mode=single'
niructl wallpaper follow-theme on
niructl wallpaper status | grep -qx 'mode=fixed'
niructl theme niru-nord
pixmap | grep -q 'pixmap id'
grep -Fqx "$HOME/Bilder med blanksteg/Wallpapers/NIRU/nirux-wallpaper-nord-4k.png" "$HOME/last-wallpaper-args"
niructl wallpaper status | grep -qx 'theme_following=enabled'
xwininfo -root -tree | grep -q NIRUBAR
printf 'PASS: explicit manual override is preserved; follow-theme on restores theme following\n'

# A broken/missing setter must leave an actionable error, not silently replace
# the requested image with solid black and report success.
for tool in hsetroot nitrogen xwallpaper; do
  printf '#!/bin/sh\nexit 9\n' > "$HOME/.local/bin/$tool"
  chmod +x "$HOME/.local/bin/$tool"
done
: > "$HOME/fail-setter"
before=$(pixmap)
niructl theme niru-noir
[ "$before" = "$(pixmap)" ]
grep -q 'could not set wallpaper.*feh:.*9' "$XDG_STATE_HOME/niruwm/session.log"
printf 'PASS: image setter failure is logged and does not erase the existing wallpaper\n'
niructl quit
wait "$START"
START=
printf 'PASS: startx session exits cleanly\n'
