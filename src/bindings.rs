//! Key binding table shared by NIRUWM and its configuration.
//!
//! Bindings are data, not code: the defaults mirror NIRUPI's `dwm/config.h`
//! so the same muscle memory works in both sessions, and a `[[keys]]` table in
//! `config.toml` can add or replace any of them without recompiling.

use serde::Deserialize;

/// What a binding does. Parsed once at load time; the WM matches on this.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Explicitly release a default shortcut rather than grabbing a no-op.
    Unbind,
    Terminal,
    TerminalAlt,
    Browser,
    Launcher,
    LauncherUnified,
    /// Lock the screen with the first available locker.
    Lock,
    WindowSwitcher,
    Start(String),
    /// Run a shell command (used by NIRUPI helper scripts and pipelines).
    Shell(String),
    FocusNext,
    FocusPrev,
    FocusTag(u32),
    /// Cycle workspaces: 1 = next, -1 = previous.
    NextTag(i32),
    ToggleTag(u32),
    MoveToTag(u32),
    ToggleMoveToTag(u32),
    SetLayout(LayoutId),
    LayoutNext,
    LayoutPrev,
    IncMaster,
    DecMaster,
    IncMasterRatio,
    DecMasterRatio,
    ToggleFloat,
    ToggleFullscreen,
    Close,
    Quit,
    Reload,
    /// Write the open windows to the session file.
    SaveSession,
    /// Forget the saved session; the next login starts empty.
    ClearSession,
    Theme(String),
    ToggleGaps,
    /// Nudge or resize the focused floating window by (dx, dy).
    MoveFloat(i16, i16),
    ResizeFloat(i16, i16),
    /// Reserved for the multi-monitor milestone; accepted from config so a
    /// bindings file written today keeps working when it lands.
    FocusMonitor(i32),
    TagMonitor(i32),
    /// Swap the focused window with the previous/next window in the stack.
    SwapNext,
    /// Show or hide the scratchpad window.
    Scratchpad,
    /// A hardware media key: volume, mute or playback.
    MediaKey(String),
    /// Change laptop backlight brightness by a percentage step.
    BrightnessStep(i16),
    /// Toggle the configured manual night light.
    NightlightToggle,
    SwapPrev,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutId {
    Tile,
    Left,
    Monocle,
    Centered,
    CenteredFloat,
    Floating,
    Fibonacci,
}

#[derive(Clone, Debug)]
pub struct Binding {
    /// Modifier mask, built from `mod` strings.
    pub mods: u32,

    /// The keycode this symbol currently maps to.
    pub code: u8,
    pub action: Action,
}

/// Raw form in the TOML file.
#[derive(Clone, Debug, Deserialize)]
pub struct RawBinding {
    pub keys: String,
    pub action: String,
}

/// Parsed modifiers.
pub fn parse_mods(spec: &str) -> Result<u32, String> {
    let mut mask = 0u32;
    for part in spec.split('+') {
        let t = part.trim();
        if t.is_empty() {
            continue;
        }
        mask |= match t.to_ascii_lowercase().as_str() {
            "super" | "mod4" => 1 << 6,
            "shift" => 1,
            "ctrl" | "control" => 1 << 2,
            "alt" | "mod1" => 1 << 3,
            "mod3" => 1 << 5,
            other => return Err(format!("unknown modifier: {other}")),
        };
    }
    Ok(mask)
}

/// Split `super+shift+2` into `(mask, "2")`.
pub fn parse_keys(spec: &str) -> Result<(u32, String), String> {
    let spec = spec.trim();
    let (mods, key) = match spec.rfind('+') {
        Some(i) => (&spec[..i], &spec[i + 1..]),
        None => ("", spec),
    };
    let key = expand_key(key)?;
    Ok((parse_mods(mods)?, key))
}

/// Human-friendly key names to X keysym names.
fn expand_key(key: &str) -> Result<String, String> {
    let k = key.trim();
    if k.is_empty() {
        return Err("empty key".into());
    }
    let named = match k.to_ascii_lowercase().as_str() {
        "enter" | "return" => "Return",
        "space" => "space",
        "tab" => "Tab",
        "escape" | "esc" => "Escape",
        "backtick" | "grave" => "grave",
        "print" | "printscreen" => "Print",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "comma" => "comma",
        "period" => "period",
        "slash" => "slash",
        "minus" => "minus",
        "equal" => "equal",
        _ => {
            // Single character or an X keysym name such as F5 or XF86AudioMute.
            if k.chars().count() == 1 {
                return Ok(k.to_string());
            }
            if k.starts_with('F') && k[1..].chars().all(|c| c.is_ascii_digit()) {
                return Ok(k.to_string());
            }
            if k.starts_with("XF86") {
                return Ok(k.to_string());
            }
            return Err(format!("unknown key: {k}"));
        }
    };
    Ok(named.to_string())
}

impl Action {
    /// Default bindings, matching NIRUPI DWM where an equivalent exists.
    ///
    /// Commands that NIRUPI provides are called through their helper script
    /// when present; the WM falls back to the plain binary so NIRUWM works
    /// with no NIRUPI installed at all.
    pub fn defaults() -> Vec<RawBinding> {
        let mut b: Vec<RawBinding> = Vec::new();
        let mut add = |keys: &str, action: &str| {
            b.push(RawBinding {
                keys: keys.into(),
                action: action.into(),
            })
        };

        // ── Terminals, browser, launchers ───────────────────────────────────
        add("super+Return", "terminal");
        add("super+shift+Return", "terminal_alt");
        add("super+b", "browser");
        // Super+P is NIRUWM's own launcher: rofi -show drun, no NIRUPI needed.
        // Super+Shift+P uses NIRUPI's combined launcher when installed.
        add("super+p", "launcher");
        add("super+shift+p", "launcher_unified");
        add("super+w", "window_switcher");
        add("super+shift+f", "start pcmanfm");
        add("super+shift+m", "shell nirupi-menu.sh");
        add("super+alt+k", "shell dwm-keybindings.sh -i");
        add("super+shift+h", "shell nirupi-keys.sh");
        add("super+Escape", "lock");
        add("super+shift+Escape", "shell sleep-suspend.sh");
        add("super+shift+q", "shell nirupi-power.sh");
        add("super+ctrl+t", "shell nirupi theme menu");
        add("super+shift+r", "shell nirupi-reload.sh");
        add("super+n", "nightlight_toggle");
        add("super+shift+c", "shell nirupi-caffeine.sh");
        add("super+shift+d", "shell nirupi-focus.sh");
        add("super+shift+l", "shell nirupi-keyboard.sh");
        add("super+shift+t", "shell nirupi-touchpad.sh");
        add("super+v", "shell clip-menu.sh");
        // Super+S belongs to NIRUPI (its ssh helper) and stays that way for
        // compatibility; the session is saved with Super+Shift+W instead.
        add("super+s", "shell nirupi-ssh.sh");
        add("super+shift+w", "save_session");
        add("super+g", "toggle_gaps");
        add("super+ctrl+shift+b", "shell nirupi-barmode.sh");
        add("super+shift+Print", "shell flameshot gui");
        add(
            "super+Print",
            "shell maim \"$HOME/Pictures/screenshot-$(date +%F_%H-%M-%S).png\"",
        );
        add(
            "Print",
            "shell maim -s \"$HOME/Pictures/screenshot-$(date +%F_%H-%M-%S).png\"",
        );

        // ── Hardware media keys ──────────────────────────────────────────
        // Bound without a modifier, as they are on a laptop keyboard. They
        // only fire when the key exists, so a keyboard without them is
        // unaffected. These went missing in a merge and did nothing.
        add("XF86AudioRaiseVolume", "media_key XF86AudioRaiseVolume");
        add("XF86AudioLowerVolume", "media_key XF86AudioLowerVolume");
        add("XF86AudioMute", "media_key XF86AudioMute");
        add("XF86AudioPlay", "media_key XF86AudioPlay");
        add("XF86AudioPause", "media_key XF86AudioPause");
        add("XF86AudioNext", "media_key XF86AudioNext");
        add("XF86AudioPrev", "media_key XF86AudioPrev");
        add("XF86MonBrightnessUp", "brightness_step 5");
        add("XF86MonBrightnessDown", "brightness_step -5");

        // ── Layouts ─────────────────────────────────────────────────────────
        add("super+t", "layout tile");
        add("super+y", "layout left");
        add("super+m", "layout monocle");
        add("super+c", "layout centered");
        add("super+u", "layout centeredfloat");
        add("super+f", "layout floating");
        add("super+ctrl+e", "layout fibonacci");
        add("super+ctrl+comma", "layout_prev");
        add("super+ctrl+period", "layout_next");
        add("super+shift+space", "toggle_float");
        add("super+shift+e", "toggle_fullscreen");
        add("super+shift+z", "shell nirupi-record.sh");
        add("super+shift+s", "scratchpad");
        add("super+h", "dec_master_ratio");
        add("super+l", "inc_master_ratio");
        add("super+i", "inc_master");
        add("super+d", "dec_master");
        add("super+shift+j", "swap_next");
        add("super+shift+k", "swap_prev");

        // ── Focus ───────────────────────────────────────────────────────────
        // Multi-monitor bindings exist in NIRUPI DWM; they are inert until
        // the second-screen milestone but stay bound so muscle memory holds.
        add("super+comma", "focusmon -1");
        add("super+period", "focusmon 1");
        add("super+shift+comma", "tagmon -1");
        add("super+shift+period", "tagmon 1");
        add("super+j", "focus_next");
        add("super+k", "focus_prev");
        add("super+Right", "focus_next");
        add("super+Down", "focus_next");
        add("super+Left", "focus_prev");
        add("super+Up", "focus_prev");
        add("super+shift+Right", "move_next"); // dwm rotatestack
        add("super+shift+Down", "move_next");
        add("super+shift+Left", "move_prev");
        add("super+shift+Up", "move_prev");

        // ── Floating movement ───────────────────────────────────────────────
        add("super+ctrl+Left", "move_float left");
        add("super+ctrl+Right", "move_float right");
        add("super+ctrl+Up", "move_float up");
        add("super+ctrl+Down", "move_float down");
        add("super+ctrl+shift+Left", "resize_float -20 0");
        add("super+ctrl+shift+Right", "resize_float 20 0");
        add("super+ctrl+shift+Up", "resize_float 0 -20");
        add("super+ctrl+shift+Down", "resize_float 0 20");

        // ── Tags ────────────────────────────────────────────────────────────
        add("super+Tab", "tag_next");
        add("super+shift+Tab", "tag_prev");
        for n in 1..=9 {
            add(&format!("super+{n}"), &format!("tag {n}"));
            add(&format!("super+shift+{n}"), &format!("movetotag {n}"));
            add(&format!("super+ctrl+{n}"), &format!("toggletag {n}"));
            add(
                &format!("super+ctrl+shift+{n}"),
                &format!("togglemovetotag {n}"),
            );
        }
        add("super+0", "tag 10");
        add("super+shift+0", "movetotag 10");

        // ── Windows and session ─────────────────────────────────────────────
        add("super+q", "close");
        add("super+ctrl+shift+q", "quit");
        add("super+r", "reload");
        add("super+ctrl+shift+r", "reload");
        b
    }
}

/// Accept `left|right|up|down` or an explicit `dx dy` pair.
fn direction(spec: &str, name: &str) -> Result<(i16, i16), String> {
    const STEP: i16 = 20;
    match spec.trim() {
        "left" => return Ok((-STEP, 0)),
        "right" => return Ok((STEP, 0)),
        "up" => return Ok((0, -STEP)),
        "down" => return Ok((0, STEP)),
        _ => {}
    }
    let mut it = spec.split_whitespace().map(|n| n.parse::<i16>());
    let dx = it
        .next()
        .and_then(|r| r.ok())
        .ok_or(format!("{name}: left|right|up|down or dx dy"))?;
    let dy = it
        .next()
        .and_then(|r| r.ok())
        .ok_or(format!("{name}: left|right|up|down or dx dy"))?;
    Ok((dx, dy))
}

/// A few actions are aliases that keep the table readable.
pub fn parse_action(spec: &str) -> Result<Action, String> {
    let spec = spec.trim();
    let (verb, rest) = match spec.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim()),
        None => (spec, ""),
    };
    let arg = || -> Result<&str, String> {
        if rest.is_empty() {
            Err(format!("{verb} needs an argument"))
        } else {
            Ok(rest)
        }
    };
    let num = || -> Result<u32, String> {
        arg()?
            .parse::<u32>()
            .map_err(|_| format!("{verb}: not a number"))
    };
    Ok(match verb {
        "unbind" => Action::Unbind,
        "terminal" => Action::Terminal,
        "terminal_alt" => Action::TerminalAlt,
        "browser" => Action::Browser,
        "launcher" => Action::Launcher,
        "launcher_unified" => Action::LauncherUnified,
        "save_session" => Action::SaveSession,
        "clear_session" => Action::ClearSession,
        "window_switcher" => Action::WindowSwitcher,
        // Lock with the best available locker: NIRUWM's own nirulock when
        // installed, then NIRUPI's wrapper, then slock.
        "lock" => Action::Lock,
        "start" => Action::Start(arg()?.to_string()),
        "shell" => Action::Shell(arg()?.to_string()),
        "focus_next" => Action::FocusNext,
        "focus_prev" => Action::FocusPrev,
        "tag" => Action::FocusTag(num()?),
        "toggletag" => Action::ToggleTag(num()?),
        "movetotag" => Action::MoveToTag(num()?),
        "togglemovetotag" => Action::ToggleMoveToTag(num()?),
        "tag_next" => Action::NextTag(1),
        "tag_prev" => Action::NextTag(-1),
        "layout" => Action::SetLayout(match arg()? {
            "tile" => LayoutId::Tile,
            "left" => LayoutId::Left,
            "monocle" => LayoutId::Monocle,
            "centered" => LayoutId::Centered,
            "centeredfloat" => LayoutId::CenteredFloat,
            "floating" => LayoutId::Floating,
            "fibonacci" => LayoutId::Fibonacci,
            other => return Err(format!("unknown layout: {other}")),
        }),
        "layout_next" => Action::LayoutNext,
        "layout_prev" => Action::LayoutPrev,
        "inc_master" => Action::IncMaster,
        "dec_master" => Action::DecMaster,
        "inc_master_ratio" => Action::IncMasterRatio,
        "dec_master_ratio" => Action::DecMasterRatio,
        "toggle_float" => Action::ToggleFloat,
        "toggle_fullscreen" => Action::ToggleFullscreen,
        "close" => Action::Close,
        "quit" => Action::Quit,
        "reload" => Action::Reload,
        "theme" => Action::Theme(arg()?.to_string()),
        "toggle_gaps" => Action::ToggleGaps,
        "focusmon" => Action::FocusMonitor(arg()?.parse().unwrap_or(1)),
        "tagmon" => Action::TagMonitor(arg()?.parse().unwrap_or(1)),
        "scratchpad" => Action::Scratchpad,
        // Hardware keys. The X names are what `keysym_named` resolves.
        "media_key" => Action::MediaKey(arg()?.to_string()),
        "brightness_step" => Action::BrightnessStep(
            arg()?
                .parse::<i16>()
                .map_err(|_| "brightness_step: not a number".to_string())?,
        ),
        "nightlight_toggle" => Action::NightlightToggle,
        "swap_next" | "move_next" => Action::SwapNext,
        "swap_prev" | "move_prev" => Action::SwapPrev,
        "move_float" => {
            let (dx, dy) = direction(arg()?, "move_float")?;
            Action::MoveFloat(dx, dy)
        }
        "resize_float" => {
            let (dx, dy) = direction(arg()?, "resize_float")?;
            Action::ResizeFloat(dx, dy)
        }
        other => return Err(format!("unknown action: {other}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_super_shift_2() {
        let (mods, key) = parse_keys("super+shift+2").unwrap();
        assert_eq!(mods, (1 << 6) | 1);
        assert_eq!(key, "2");
    }

    #[test]
    fn parse_ctrl_comma() {
        let (mods, key) = parse_keys("super+ctrl+comma").unwrap();
        assert_eq!(mods, (1 << 6) | (1 << 2));
        assert_eq!(key, "comma");
    }

    #[test]
    fn defaults_parse() {
        for raw in Action::defaults() {
            parse_keys(&raw.keys).unwrap_or_else(|e| panic!("{}: {e}", raw.keys));
            parse_action(&raw.action).unwrap_or_else(|e| panic!("{}: {e}", raw.action));
        }
    }

    #[test]
    fn no_duplicate_default_bindings() {
        let mut seen = std::collections::HashSet::new();
        for raw in Action::defaults() {
            let (mods, key) = parse_keys(&raw.keys).unwrap();
            assert!(
                seen.insert((mods, key.clone())),
                "duplicate binding: {}",
                raw.keys
            );
        }
    }

    #[test]
    fn reject_unknown_modifier() {
        assert!(parse_keys("hyper+x").is_err());
    }
}
