#!/bin/sh
# The 0.11.0 configuration contract.
#
# These settings are read by three programs from one file, so the test checks
# both that the values take effect and that a bad value never stops a program.
set -eu
BIN=${1:?usage: suite-config.sh BINDIR}
[ -x "$BIN/niruwm" ] || { printf 'missing niruwm in %s\n' "$BIN" >&2; exit 1; }
[ -x "$BIN/nirubar" ] || { printf 'missing nirubar in %s\n' "$BIN" >&2; exit 1; }

TEST=$(mktemp -d)
trap 'rm -rf "$TEST"' EXIT HUP INT TERM
export HOME="$TEST/home" XDG_CONFIG_HOME="$TEST/home/.config"
unset DISPLAY
mkdir -p "$XDG_CONFIG_HOME/niruwm"
CONF="$XDG_CONFIG_HOME/niruwm/config.toml"

fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
check_ok() {
  "$BIN/niruwm" --check-config >/dev/null 2>&1 ||
    fail "niruwm rejected: $1"
  "$BIN/nirubar" --check-config >/dev/null 2>&1 ||
    fail "nirubar rejected: $1"
}

# 1. The documented names are accepted by both programs.
cat > "$CONF" <<'EOF'
theme = "niru-noir"
[bar]
enabled = true
workspace_layout = 3
transparent = true
opacity = 80
[idle]
screensaver_timeout = 5
lock_after_screensaver = 2
dpms_off_after_lock = 0
EOF
check_ok "the documented 0.11.0 settings"

# 2. An absent [idle] keeps the section optional.
printf 'theme = "niru-noir"\n' > "$CONF"
check_ok "a config without [idle]"

# 3. An out-of-range NUMBER is a value error and falls back safely; a wrong
#    TYPE is a typo and must be reported rather than guessed.
for bad in \
  '[bar]\nworkspace_layout = 99\n' \
  '[bar]\nopacity = 500\n'
do
  printf "$bad" > "$CONF"
  check_ok "an out-of-range value ($bad)"
done

printf '[bar]\ntransparent = "maybe"\n' > "$CONF"
if "$BIN/niruwm" --check-config >/dev/null 2>&1; then
  fail "a wrong type was accepted silently"
fi

# 4. A zero screensaver timeout is refused: it would mean "never".
printf '[idle]\nscreensaver_timeout = 0\n' > "$CONF"
if "$BIN/niruwm" --check-config >/dev/null 2>&1; then
  fail "screensaver_timeout = 0 was accepted; the screen would never lock"
fi

# 5. An absurd timer is refused rather than wrapping.
printf '[idle]\nscreensaver_timeout = 99999\n' > "$CONF"
if "$BIN/niruwm" --check-config >/dev/null 2>&1; then
  fail "screensaver_timeout above one day was accepted"
fi

# 6. The shared file is the source of truth: [idle] must parse without the
#    legacy screensaver.toml, and the legacy file must not override it.
printf '[idle]\nscreensaver_timeout = 7\nlock_after_screensaver = 3\n' > "$CONF"
mkdir -p "$XDG_CONFIG_HOME/niruui"
printf 'idle_minutes = 60\nlock_after_minutes = 60\n' > "$XDG_CONFIG_HOME/niruui/screensaver.toml"
if [ -x "$BIN/niruidle" ]; then
  # --status needs an X connection; Xvfb is present in CI, and a display in the
  # environment is used when one exists. Without either, the check is skipped
  # rather than reported as a pass.
  display=${NIRU_TEST_DISPLAY:-}
  if [ -z "$display" ] && command -v Xvfb >/dev/null 2>&1; then
    Xvfb :97 -screen 0 800x600x24 -nolisten tcp >/dev/null 2>&1 &
    XVFB=$!
    display=:97
    sleep 1
  fi
  if [ -n "$display" ]; then
    out=$(DISPLAY="$display" timeout 5 "$BIN/niruidle" --status 2>&1 || true)
    case "$out" in
      *"screen after 7 min"*"lock 3 min later"*) : ;;
      *"cannot connect"*|*"MIT-SCREEN-SAVER"*|*"X server"*)
        # No usable idle source here: record it instead of claiming a pass.
        printf 'suite config: niruidle --status not-run (%s)\n' \
          "$(printf '%s' "$out" | head -1)"
        ;;
      *) fail "niruidle did not use [idle]: $out" ;;
    esac
    [ -z "${XVFB:-}" ] || kill "$XVFB" 2>/dev/null || :
  else
    printf 'suite config: niruidle --status not-run (no X display)\n'
  fi
fi

printf 'suite config: ok\n'
