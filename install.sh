#!/bin/sh
# NIRUWM installer — offline-first, never touches a NIRUPI installation.
#
#   ./install.sh                  install prebuilt binaries (downloads release)
#   ./install.sh --build          build from this checkout (needs Rust)
#   ./install.sh --register-session   add the SDDM entry (needs sudo)
#   ./install.sh --uninstall      remove everything NIRUWM installed
#   ./install.sh --check          verify the installed session without changes
#
# Everything NIRUWM owns uses the "niruwm" prefix and is recorded in
# $XDG_STATE_HOME/niruwm/manifest.tsv. Files owned by NIRUPI (dwm,
# dwm-status.sh, nirupi-session, ~/.xinitrc, the nirupi manifest) are never
# read for writing and never removed.
set -eu

VERSION="0.2.0"
REPO="${NIRUWM_REPO:-nirucon/niruwm}"
PREFIX="${NIRUWM_PREFIX:-$HOME/.local}"
BINDIR="$PREFIX/bin"
CONFDIR="${XDG_CONFIG_HOME:-$HOME/.config}/niruwm"
STATEDIR="${XDG_STATE_HOME:-$HOME/.local/state}/niruwm"
MANIFEST="$STATEDIR/manifest.tsv"
SESSION_DIR="${NIRUWM_SESSION_DIR:-/usr/share/xsessions}"
SESSION_FILE="$SESSION_DIR/niruwm.desktop"

# Paths NIRUPI owns that a careless installer would overwrite. Listed one per
# line because $HOME may contain spaces.
nirupi_owned() {
  printf '%s\n' \
    "/usr/local/bin/nirupi-session" \
    "$SESSION_DIR/nirupi-dwm.desktop" \
    "$HOME/.xinitrc" \
    "$HOME/.local/bin/dwm" \
    "$HOME/.local/bin/dwm-status.sh"
}

usage() {
  cat <<'EOF'
Usage: ./install.sh [options]
  (no options)         install the released binaries
  --build              build from this checkout and install the result
  --register-session   also add NIRUWM to the SDDM session list (sudo)
  --uninstall          remove installed NIRUWM files and the session entry
  --check              verify the installation; make no changes
  --prefix DIR         install under DIR (default ~/.local)
EOF
}

die() { printf 'niruwm install: %s\n' "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

# Refuse anything that would touch a NIRUPI-owned path.
guard_nirupi() {
  [ -z "$1" ] && return 0
  # No pipeline: `die` inside a subshell would only exit the subshell.
  saved_ifs=$IFS
  IFS='
'
  for p in $(nirupi_owned); do
    [ "$1" = "$p" ] && die "refusing to modify NIRUPI-owned path: $1"
  done
  IFS=$saved_ifs
}

record() {
  entry_path="$1"
  entry_hash="$2"
  mkdir -p "$STATEDIR"
  # Keep one entry per path so re-installing never grows the manifest.
  # Names are prefixed because POSIX sh has no local variables: a plain
  # `tmp` here would clobber the caller's download directory.
  manifest_tmp="$STATEDIR/.manifest.$$"
  if [ -f "$MANIFEST" ]; then
    awk -F '\t' -v p="$entry_path" '$1 != p' "$MANIFEST" > "$manifest_tmp" || true
  fi
  printf '%s\t%s\n' "$entry_path" "$entry_hash" >> "$manifest_tmp"
  mv -f "$manifest_tmp" "$MANIFEST"
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# Install one file atomically, keeping a timestamped backup of an old copy.
put() {
  src="$1"; dest="$2"; mode="${3:-0755}"
  guard_nirupi "$dest"
  mkdir -p "$(dirname "$dest")"
  install -m "$mode" "$src" "$dest.niruwm-new"
  mv -f "$dest.niruwm-new" "$dest"
  record "$dest" "$(sha256 "$dest")"
}

need_cmd() { command -v "$1" >/dev/null 2>&1; }

fetch() {
  url="$1"; out="$2"
  if need_cmd curl; then
    curl -fsSL --retry 3 -o "$out" "$url"
  elif need_cmd wget; then
    wget -qO "$out" "$url"
  else
    die "curl or wget is required to download $url"
  fi
}

install_release() {
  tag="${NIRUWM_VERSION:-$VERSION}"
  base="${NIRUWM_BASE_URL:-https://github.com/$REPO/releases/download/v$tag}"
  dl_dir="$(mktemp -d)"
  trap 'rm -rf "$dl_dir"' EXIT HUP INT TERM
  say "Downloading NIRUWM $tag for x86_64 ..."
  for b in niruwm nirubar niructl; do
    fetch "$base/$b" "$dl_dir/$b" || die "cannot download $base/$b"
  done
  fetch "$base/SHA256SUMS" "$dl_dir/SHA256SUMS" || die "cannot download checksums"
  for b in niruwm nirubar niructl; do
    # Single quotes: $2 belongs to awk, not to the shell.
    expected="$(awk -v n="$b" '$2 == n {print $1}' "$dl_dir/SHA256SUMS")"
    [ -n "$expected" ] || die "no checksum for $b"
    actual="$(sha256 "$dl_dir/$b")"
    [ "$expected" = "$actual" ] || die "checksum mismatch for $b"
    chmod 0755 "$dl_dir/$b"
  done
  verify_versions "$dl_dir"
  for b in niruwm nirubar niructl; do
    put "$dl_dir/$b" "$BINDIR/$b"
  done
  put "$(dirname "$0")/session/niruwm-session" "$BINDIR/niruwm-session" 0755
  install_config
  say "Installed NIRUWM $tag in $BINDIR"
}

# Both binaries ship together. A mismatch means the release was built against
# the wrong NIRUBAR branch, which has happened; refuse rather than mix versions.
verify_versions() {
  dir="$1"
  wm="$("$dir/niruwm" --version 2>/dev/null | awk '{print $2}')"
  bar="$("$dir/nirubar" --version 2>/dev/null | awk '{print $2}')"
  say "NIRUWM $wm, NIRUBAR $bar"
  [ -n "$wm" ] && [ -n "$bar" ] || die "cannot read binary versions"
  [ "$wm" = "$bar" ] || die "version mismatch: niruwm $wm / nirubar $bar — refusing to install a mixed pair"
}

build_local() {
  need_cmd cargo || die "cargo is required for --build (or drop --build to use released binaries)"
  say "Building from source ..."
  ( cd "$(dirname "$0")" && cargo build --release --locked )
  for b in niruwm nirubar niructl; do
    [ -f "target/release/$b" ] || die "build did not produce target/release/$b"
    put "target/release/$b" "$BINDIR/$b"
  done
  verify_versions "$(dirname "$0")/target/release"
  put "session/niruwm-session" "$BINDIR/niruwm-session" 0755
  install_config
  say "Installed from source in $BINDIR"
}

# Create missing files only. An existing configuration is never rewritten, so
# installing NIRUBAR and NIRUWM in either order cannot clobber the other.
install_config() {
  mkdir -p "$CONFDIR/themes"
  if [ ! -e "$CONFDIR/config.toml" ]; then
    install -m 0644 "$(dirname "$0")/config.example.toml" "$CONFDIR/config.toml"
    say "Created $CONFDIR/config.toml"
  else
    say "Keeping existing $CONFDIR/config.toml"
  fi
  for t in "$(dirname "$0")"/themes/*.toml; do
    [ -f "$t" ] || continue
    dest="$CONFDIR/themes/$(basename "$t")"
    [ -e "$dest" ] || install -m 0644 "$t" "$dest"
  done
}

register_session() {
  # Never write a root-owned session entry by accident: SDDM would start a
  # session that cannot read the user's config, and the binaries would live in
  # /root. This happened once when the installer was run under `sudo` instead
  # of being allowed to ask for sudo itself.
  [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ] &&
    die "run ./install.sh as $SUDO_USER, not under sudo"
  case "$BINDIR" in
    /root/*) die "refusing to register a session pointing into /root" ;;
  esac
  tmp="$(mktemp)"
  cat > "$tmp" <<EOF
[Desktop Entry]
Name=NIRUWM
Comment=NIRU Noir X11 session
Exec="$BINDIR/niruwm-session"
Type=Application
DesktopNames=NIRUWM
EOF
  say "Adding $SESSION_FILE (sudo):"
  sudo install -m 0644 "$tmp" "$SESSION_FILE"
  rm -f "$tmp"
  say "NIRUWM is now selectable in SDDM. NIRUPI DWM remains unchanged."
}

uninstall() {
  [ -f "$MANIFEST" ] || { say "Nothing to remove (no manifest)."; return 0; }
  while IFS="$(printf '\t')" read -r path _hash; do
    [ -n "$path" ] || continue
    case "$path" in
      */niruwm*|*/nirubar|*/niructl) ;;
      *) say "Skipping unexpected manifest entry: $path"; continue ;;
    esac
    [ -e "$path" ] && rm -f "$path" && say "Removed $path"
  done < "$MANIFEST"
  if [ -e "$SESSION_FILE" ]; then
    grep -q "niruwm-session" "$SESSION_FILE" 2>/dev/null &&
      sudo rm -f "$SESSION_FILE" && say "Removed $SESSION_FILE"
  fi
  rm -f "$MANIFEST"
  say "Configuration kept in $CONFDIR (delete it manually if you want a clean slate)."
  say "NIRUPI files were not touched."
}

check() {
  ok=0
  for b in niruwm nirubar niructl niruwm-session; do
    if [ -x "$BINDIR/$b" ]; then say "OK   $BINDIR/$b"; else say "FAIL missing $BINDIR/$b"; ok=1; fi
  done
  if [ -e "$SESSION_FILE" ]; then say "OK   $SESSION_FILE"; else say "INFO no SDDM entry (run --register-session)"; fi
  saved_ifs=$IFS
  IFS='
'
  for p in $(nirupi_owned); do
    [ -e "$p" ] && say "OK   untouched NIRUPI path present: $p"
  done
  IFS=$saved_ifs
  "$BINDIR/niruwm" --check-config || ok=1
  "$BINDIR/nirubar" --check-config || ok=1
  return "$ok"
}

MODE=install
REGISTER=0
while [ $# -gt 0 ]; do
  case "$1" in
    --build) MODE=build ;;
    --register-session) REGISTER=1 ;;
    --uninstall) MODE=uninstall ;;
    --check) MODE=check ;;
    --prefix) shift; PREFIX="${1:?--prefix needs a directory}"; BINDIR="$PREFIX/bin" ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
  shift
done

cd "$(dirname "$0")"
case "$MODE" in
  install) install_release ;;
  build) build_local ;;
  uninstall) uninstall; exit 0 ;;
  check) check; exit $? ;;
esac
[ "$REGISTER" = 1 ] && register_session
say "Done. Log out of NIRUPI DWM and choose NIRUWM in SDDM."
