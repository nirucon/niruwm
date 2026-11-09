//! Key binding table shared by NIRUWM and its configuration.
//!
//! Bindings are data, not code: the defaults mirror the NIRU desktop layout
//! so the same muscle memory works in both sessions, and a `[[keys]]` table in
//! `config.toml` can add or replace any of them without recompiling.

use serde::Deserialize;
use std::collections::HashMap;

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
    /// The focused NIRU system-action catalogue.
    LauncherActions,
    /// Type a phrase and open it in a web search engine.
    LauncherSearch,
    /// Open the local clipboard history.
    LauncherClipboard,
    /// The keybinding cheat sheet (`nirulauncher --keys`).
    LauncherKeys,
    /// The emoji and symbol picker (`nirulauncher --emoji`).
    LauncherEmoji,
    /// File search (`nirulauncher --files`).
    LauncherFiles,
    /// Lock the screen with the first available locker.
    Lock,
    /// Lock, wait for verified readiness, then suspend the machine.
    Suspend,
    WindowSwitcher,
    Start(String),
    /// Run a shell command (used by configured pipelines and user bindings).
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
    /// Set the focused Scroll workspace to one or two visible columns.
    SetScrollColumns(crate::layout::ScrollColumns),
    ToggleScrollColumns,
    LayoutNext,
    LayoutPrev,
    IncMaster,
    DecMaster,
    IncMasterRatio,
    DecMasterRatio,
    ToggleFloat,
    ToggleFullscreen,
    ToggleSticky,
    BarTop,
    BarBottom,
    BarHide,
    BarShow,
    BarToggle,
    BarCycle,
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
    /// Show or hide the default scratchpad (Super+Shift+S).
    Scratchpad,
    /// Show or hide numbered scratchpad 1-9 (Super+Alt+N).
    ScratchpadSlot(u8),
    /// Send the focused window to scratchpad N, or back when it is already
    /// there (Super+Alt+Shift+N).
    ScratchpadSend(u8),
    /// Turn the focused scratchpad back into a normal window.
    Unscratch,
    /// A hardware media key: volume, mute or playback.
    MediaKey(String),
    /// Change laptop backlight brightness by a percentage step.
    BrightnessStep(i16),
    /// Toggle the configured manual night light.
    NightlightToggle,
    /// Pause or resume notifications through the active provider.
    DndToggle,
    /// Keep the screen awake until toggled off.
    IdleToggle,
    SwapPrev,
    /// Go to the window that asked for attention most recently: its screen
    /// and workspace, or its scratchpad (Super+Shift+U).
    FocusUrgent,
    /// Window swallowing on or off for this session.
    SwallowToggle,
    /// Studio (DAW) mode on or off for this session (`niructl studio`).
    StudioToggle,
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
    Scroll,
}

impl LayoutId {
    /// The layout a name refers to, if any.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "tile" => Some(LayoutId::Tile),
            "left" => Some(LayoutId::Left),
            "monocle" => Some(LayoutId::Monocle),
            "centered" => Some(LayoutId::Centered),
            "centeredfloat" => Some(LayoutId::CenteredFloat),
            "floating" => Some(LayoutId::Floating),
            "fibonacci" => Some(LayoutId::Fibonacci),
            "scroll" => Some(LayoutId::Scroll),
            _ => None,
        }
    }

    /// The value the control channel carries for this layout.
    #[allow(dead_code)] // used by niructl, which shares this file
    pub fn code(self) -> u32 {
        match self {
            LayoutId::Tile => 0,
            LayoutId::Monocle => 1,
            LayoutId::Left => 2,
            LayoutId::Centered => 3,
            LayoutId::Fibonacci => 4,
            LayoutId::Floating => 5,
            LayoutId::CenteredFloat => 6,
            LayoutId::Scroll => 7,
        }
    }

    /// The layout itself.
    pub fn layout(self) -> crate::layout::Layout {
        match self {
            LayoutId::Tile => crate::layout::Layout::Tile,
            LayoutId::Left => crate::layout::Layout::Left,
            LayoutId::Monocle => crate::layout::Layout::Monocle,
            LayoutId::Centered => crate::layout::Layout::Centered,
            LayoutId::CenteredFloat => crate::layout::Layout::CenteredFloat,
            LayoutId::Floating => crate::layout::Layout::Floating,
            LayoutId::Fibonacci => crate::layout::Layout::Fibonacci,
            LayoutId::Scroll => crate::layout::Layout::Scroll,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Binding {
    /// Modifier mask, built from `mod` strings.
    pub mods: u32,

    /// The X keysym the binding names, so a request to run "the binding on
    /// Super+T" can find it without the keyboard mapping.
    pub sym: u32,

    /// The keycode this symbol currently maps to.
    pub code: u8,
    pub action: Action,
}

/// Raw form in the TOML file.
#[derive(Clone, Debug, Deserialize)]
pub struct RawBinding {
    pub keys: String,
    pub action: String,
    /// What the binding does, in words, for the cheat sheet. Optional; the
    /// built-in description of the action is used without it.
    #[serde(default)]
    pub description: Option<String>,
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
    /// Default bindings, all backed by the suite itself.
    ///
    pub fn defaults() -> Vec<RawBinding> {
        let mut b: Vec<RawBinding> = Vec::new();
        let mut add = |keys: &str, action: &str| {
            b.push(RawBinding {
                keys: keys.into(),
                action: action.into(),
                description: None,
            })
        };

        // ── Terminals, browser, launchers ───────────────────────────────────
        add("super+Return", "terminal");
        add("super+shift+Return", "terminal_alt");
        add("super+b", "browser");
        // Prefer the native launcher; the configured app launcher is fallback.
        add("super+p", "launcher");
        add("super+shift+p", "launcher_unified");
        // Practical system actions: maintenance, diagnostics, contexts and
        // the optional OpenCode handoff. Super+A is free by default.
        add("super+a", "launcher_actions");
        // Type a phrase, Enter opens it in the instant-search engine.
        add("super+s", "launcher_search");
        add("super+v", "launcher_clipboard");
        // The keybinding cheat sheet (Super+K is focus_prev) and the emoji
        // and symbol picker.
        add("super+slash", "launcher_keys");
        // The same sheet on a key every layout has: on Swedish, German and
        // other layouts "/" is Shift+7, so Super+/ is the workspace 7 key and
        // that one wins (see resolve_codes).
        add("super+F1", "launcher_keys");
        add("super+period", "launcher_emoji");
        // File search: Super+O as in "open"; free by default.
        add("super+o", "launcher_files");
        add("super+shift+b", "launcher_unified");
        // The suite launcher is also available from the old menu key.
        add("super+shift+m", "launcher_unified");
        add("super+w", "window_switcher");
        add("alt+Tab", "window_switcher");
        add("super+grave", "window_switcher");
        add("super+shift+f", "start pcmanfm");
        add("super+Escape", "lock");
        // Lock, wait for verified readiness, then suspend. The shared
        // niructl path owns the readiness check and the delay.
        add("super+shift+Escape", "suspend");
        // The old reload key is a native reload.
        add("super+shift+r", "reload");
        add("super+n", "nightlight_toggle");
        add("super+shift+d", "dnd_toggle");
        add("super+shift+c", "idle_toggle");
        add("super+shift+w", "save_session");
        add("super+g", "toggle_gaps");
        add("super+Print", "shell nirushot --full");
        add("Print", "shell nirushot --region");
        add("super+shift+Print", "shell nirushot --window");
        // Colour picker: a frozen frame with a loupe; the colour is copied.
        add("super+ctrl+c", "shell nirushot pick");
        // Text from a screen region (OCR, NIRUSHOT 0.7); the text is copied.
        add("super+ctrl+o", "shell nirushot ocr");

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
        add("super+ctrl+s", "layout scroll");
        add("super+ctrl+shift+s", "scroll_columns toggle");
        add("super+ctrl+comma", "layout_prev");
        add("super+ctrl+period", "layout_next");
        add("super+shift+space", "toggle_float");
        add("super+shift+e", "toggle_fullscreen");
        add("super+alt+s", "toggle_sticky");
        add("super+alt+b", "bar_cycle");
        add("super+shift+s", "scratchpad");
        // Numbered scratchpads: Super+Alt+N shows or hides slot N,
        // Super+Alt+Shift+N sends the focused window there (or back).
        for n in 1..=9 {
            add(&format!("super+alt+{n}"), &format!("scratchpad {n}"));
            add(
                &format!("super+alt+shift+{n}"),
                &format!("scratchpad_send {n}"),
            );
        }
        add("super+h", "dec_master_ratio");
        add("super+l", "inc_master_ratio");
        add("super+i", "inc_master");
        add("super+d", "dec_master");
        add("super+shift+j", "swap_next");
        add("super+shift+k", "swap_prev");
        // Super+U is the centred floating layout; Shift goes to the window
        // that asked for attention (dwm marks it urgent).
        add("super+shift+u", "focus_urgent");

        // ── Focus ───────────────────────────────────────────────────────────
        // Multi-monitor bindings are inert until
        // the second-screen milestone but stay bound so muscle memory holds.
        // Super+. is the emoji picker since 0.18.0. With two screens
        // Super+, reaches the other one either way (it wraps around);
        // Super+Alt+, and Super+Alt+. keep both directions.
        add("super+comma", "focusmon -1");
        add("super+alt+comma", "focusmon -1");
        add("super+alt+period", "focusmon 1");
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
        add("super+ctrl+0", "toggletag 10");
        add("super+ctrl+shift+0", "togglemovetotag 10");

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

/// A scratchpad slot: 1-9, the digit in Super+Alt+N.
fn slot_number(verb: &str, text: &str) -> Result<u8, String> {
    match text.trim().parse::<u8>() {
        Ok(n) if (1..=9).contains(&n) => Ok(n),
        _ => Err(format!("{verb}: slot must be 1 to 9, got {text:?}")),
    }
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
        "launcher_actions" => Action::LauncherActions,
        "launcher_search" => Action::LauncherSearch,
        "launcher_clipboard" => Action::LauncherClipboard,
        "launcher_keys" => Action::LauncherKeys,
        "launcher_emoji" => Action::LauncherEmoji,
        "launcher_files" => Action::LauncherFiles,
        "save_session" => Action::SaveSession,
        "clear_session" => Action::ClearSession,
        "window_switcher" => Action::WindowSwitcher,
        // Lock with the best available locker: NIRUWM's own nirulock when
        // installed, then slock.
        "lock" => Action::Lock,
        // Lock, verify, wait three seconds, verify again, then suspend.
        "suspend" => Action::Suspend,
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
            "scroll" => LayoutId::Scroll,
            other => return Err(format!("unknown layout: {other}")),
        }),
        "scroll_columns" => match arg()? {
            "1" => Action::SetScrollColumns(crate::layout::ScrollColumns::One),
            "2" => Action::SetScrollColumns(crate::layout::ScrollColumns::Two),
            "toggle" => Action::ToggleScrollColumns,
            other => {
                return Err(format!(
                    "scroll_columns must be 1, 2 or toggle, got {other}"
                ))
            }
        },
        "layout_next" => Action::LayoutNext,
        "layout_prev" => Action::LayoutPrev,
        "inc_master" => Action::IncMaster,
        "dec_master" => Action::DecMaster,
        "inc_master_ratio" => Action::IncMasterRatio,
        "dec_master_ratio" => Action::DecMasterRatio,
        "toggle_float" => Action::ToggleFloat,
        "toggle_fullscreen" => Action::ToggleFullscreen,
        "toggle_sticky" => Action::ToggleSticky,
        "bar_top" => Action::BarTop,
        "bar_bottom" => Action::BarBottom,
        "bar_hide" => Action::BarHide,
        "bar_show" => Action::BarShow,
        "bar_toggle" => Action::BarToggle,
        "bar_cycle" => Action::BarCycle,
        "close" => Action::Close,
        "quit" => Action::Quit,
        "reload" => Action::Reload,
        "theme" => Action::Theme(arg()?.to_string()),
        "toggle_gaps" => Action::ToggleGaps,
        "focusmon" => Action::FocusMonitor(arg()?.parse().unwrap_or(1)),
        "tagmon" => Action::TagMonitor(arg()?.parse().unwrap_or(1)),
        "scratchpad" if rest.is_empty() => Action::Scratchpad,
        "scratchpad" => Action::ScratchpadSlot(slot_number(verb, rest)?),
        "scratchpad_send" => Action::ScratchpadSend(slot_number(verb, arg()?)?),
        "unscratch" => Action::Unscratch,
        // Hardware keys. The X names are what `keysym_named` resolves.
        "media_key" => Action::MediaKey(arg()?.to_string()),
        "brightness_step" => Action::BrightnessStep(
            arg()?
                .parse::<i16>()
                .map_err(|_| "brightness_step: not a number".to_string())?,
        ),
        "nightlight_toggle" => Action::NightlightToggle,
        "dnd_toggle" => Action::DndToggle,
        "idle_toggle" => Action::IdleToggle,
        "focus_urgent" => Action::FocusUrgent,
        "swallow_toggle" => Action::SwallowToggle,
        "studio_toggle" => Action::StudioToggle,
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

/// The X keysym for a key name from [`parse_keys`].
#[allow(dead_code)] // used by niructl, which shares this file
pub fn keysym_named(name: &str) -> Option<u32> {
    let v = match name {
        "Return" => 0xff0d,
        "Tab" => 0xff09,
        "Escape" => 0xff1b,
        "space" => 0x0020,
        "grave" => 0x0060,
        "minus" => 0x002d,
        "equal" => 0x003d,
        "comma" => 0x002c,
        "period" => 0x002e,
        "slash" => 0x002f,
        "semicolon" => 0x003b,
        "apostrophe" => 0x0027,
        "bracketleft" => 0x005b,
        "bracketright" => 0x005d,
        "backslash" => 0x005c,
        "Left" => 0xff51,
        "Up" => 0xff52,
        "Right" => 0xff53,
        "Down" => 0xff54,
        "Print" => 0xff61,
        "BackSpace" => 0xff08,
        "Delete" => 0xffff,
        // Hardware keys, from XF86keysym.h. Without these the names resolved
        // to None and every media key was skipped when the table was built,
        // so the keys did nothing at all.
        "XF86AudioMute" => 0x1008ff12,
        "XF86AudioLowerVolume" => 0x1008ff11,
        "XF86AudioRaiseVolume" => 0x1008ff13,
        "XF86AudioPlay" => 0x1008ff14,
        "XF86AudioStop" => 0x1008ff15,
        "XF86AudioPrev" => 0x1008ff16,
        "XF86AudioNext" => 0x1008ff17,
        "XF86AudioPause" => 0x1008ff31,
        "XF86AudioMicMute" => 0x1008ffb2,
        "XF86MonBrightnessUp" => 0x1008ff02,
        "XF86MonBrightnessDown" => 0x1008ff03,
        "XF86Display" => 0x1008ff59,
        _ => {
            if name.chars().count() == 1 {
                let c = name.chars().next()?;
                // X keysyms for printable Latin-1 characters match the code point.
                u32::from(c)
            } else {
                // F1..F35 are contiguous from 0xffbe.
                let n: u32 = name.strip_prefix('F')?.parse().ok()?;
                if !(1..=35).contains(&n) {
                    return None;
                }
                0xffbe + (n - 1)
            }
        }
    };
    Some(v)
}

/// The cheat-sheet groups, in the order the sheet and KEYBINDINGS.md list
/// them.
#[allow(dead_code)] // used by niructl, which shares this file
pub const GROUPS: &[&str] = &[
    "Apps and launchers",
    "Session",
    "Layouts",
    "Scratchpads",
    "Focus and windows",
    "Workspaces",
    "Floating windows",
    "Screenshots and color",
    "Hardware keys",
    "Other",
];

/// Descriptions of the shell commands the default table binds.
#[allow(dead_code)] // used by niructl, which shares this file
fn describe_shell(command: &str) -> Option<(&'static str, &'static str)> {
    Some(match command.trim() {
        "nirushot --full" => (
            "Screenshots and color",
            "Screenshot of the whole screen, then review",
        ),
        "nirushot --region" => (
            "Screenshots and color",
            "Screenshot of a region, then review",
        ),
        "nirushot --window" => (
            "Screenshots and color",
            "Screenshot of the active window, then review",
        ),
        "nirushot pick" => (
            "Screenshots and color",
            "Pick a color from the screen and copy it",
        ),
        "nirushot ocr" => (
            "Screenshots and color",
            "Copy the text in a screen region (OCR)",
        ),
        _ => return None,
    })
}

#[allow(dead_code)] // used by niructl, which shares this file
fn layout_words(id: LayoutId) -> &'static str {
    match id {
        LayoutId::Tile => "Tile",
        LayoutId::Left => "Left stack",
        LayoutId::Monocle => "Monocle (one window at a time)",
        LayoutId::Centered => "Centered master",
        LayoutId::CenteredFloat => "Centered floating master",
        LayoutId::Floating => "Floating",
        LayoutId::Fibonacci => "Fibonacci spiral",
        LayoutId::Scroll => "Scroll",
    }
}

/// The cheat-sheet group and an English description of a binding's action.
///
/// Every action has one, so a sheet built from the live table never shows a
/// bare action name. An unparsable action says so instead of failing.
#[allow(dead_code)] // used by niructl, which shares this file
pub fn describe_action(spec: &str) -> (&'static str, String) {
    let action = match parse_action(spec) {
        Ok(action) => action,
        Err(e) => return ("Other", format!("Invalid action: {e}")),
    };
    let s = |g: &'static str, t: &str| (g, t.to_string());
    let apps = "Apps and launchers";
    let session = "Session";
    let layouts = "Layouts";
    let scratch = "Scratchpads";
    let focus = "Focus and windows";
    let tags = "Workspaces";
    let float = "Floating windows";
    let hardware = "Hardware keys";
    match action {
        Action::Unbind => s("Other", "Not bound (released default)"),
        Action::Terminal => s(apps, "Open a terminal"),
        Action::TerminalAlt => s(apps, "Open the alternative terminal"),
        Action::Browser => s(apps, "Open the web browser"),
        Action::Launcher => s(apps, "Application launcher"),
        Action::LauncherUnified => s(
            apps,
            "Unified launcher: apps, windows, actions and settings",
        ),
        Action::LauncherActions => s(
            apps,
            "NIRU Actions: maintenance, diagnostics, contexts and OpenCode",
        ),
        Action::LauncherSearch => s(apps, "Web search: type a phrase, Enter opens it"),
        Action::LauncherClipboard => s(apps, "Clipboard history"),
        Action::LauncherKeys => s(apps, "Keybinding cheat sheet (this list)"),
        Action::LauncherEmoji => s(apps, "Emoji and symbol picker"),
        Action::LauncherFiles => s(apps, "Find files in the home folder"),
        Action::WindowSwitcher => s(apps, "Window switcher"),
        Action::Start(cmd) => {
            if cmd.trim() == "pcmanfm" {
                s(apps, "Open the file manager (pcmanfm)")
            } else {
                (apps, format!("Start {}", cmd.trim()))
            }
        }
        Action::Shell(cmd) => match describe_shell(&cmd) {
            Some((group, text)) => s(group, text),
            None => (apps, format!("Run: {}", cmd.trim())),
        },
        Action::Lock => s(session, "Lock the screen"),
        Action::Suspend => s(session, "Lock, then suspend the computer"),
        Action::Quit => s(session, "Log out (end the session)"),
        Action::Reload => s(session, "Reload the configuration"),
        Action::SaveSession => s(session, "Save the open windows as the session"),
        Action::ClearSession => s(session, "Forget the saved session"),
        Action::Theme(name) => (session, format!("Switch to the {name} theme")),
        Action::NightlightToggle => s(session, "Night light on or off"),
        Action::DndToggle => s(session, "Do Not Disturb on or off"),
        Action::IdleToggle => s(session, "Keep the screen awake on or off"),
        Action::SwallowToggle => s(
            session,
            "Window swallowing on or off (a program started from a terminal takes its place)",
        ),
        Action::StudioToggle => s(
            session,
            "Studio mode on or off (no screen saver, lock or popups; compositor off; performance)",
        ),
        Action::SetLayout(id) => (layouts, format!("Layout: {}", layout_words(id))),
        Action::SetScrollColumns(c) => (
            layouts,
            format!(
                "Scroll layout: {} visible column{}",
                c.count(),
                if c.count() == 1 { "" } else { "s" }
            ),
        ),
        Action::ToggleScrollColumns => s(layouts, "Scroll layout: one or two visible columns"),
        Action::LayoutNext => s(layouts, "Next layout"),
        Action::LayoutPrev => s(layouts, "Previous layout"),
        Action::IncMaster => s(layouts, "One more window in the master area"),
        Action::DecMaster => s(layouts, "One window fewer in the master area"),
        Action::IncMasterRatio => s(layouts, "Wider master area"),
        Action::DecMasterRatio => s(layouts, "Narrower master area"),
        Action::ToggleGaps => s(layouts, "Gaps between windows on or off"),
        Action::BarTop => s(layouts, "Bar at the top"),
        Action::BarBottom => s(layouts, "Bar at the bottom"),
        Action::BarHide => s(layouts, "Hide the bar"),
        Action::BarShow => s(layouts, "Show the bar"),
        Action::BarToggle => s(layouts, "Show or hide the bar"),
        Action::BarCycle => s(layouts, "Bar: top, bottom, hidden"),
        Action::Scratchpad => s(scratch, "Show or hide the default scratchpad"),
        Action::ScratchpadSlot(n) => (scratch, format!("Show or hide scratchpad {n}")),
        Action::ScratchpadSend(n) => (
            scratch,
            format!("Send the window to scratchpad {n}, or take it back"),
        ),
        Action::Unscratch => s(scratch, "Make the scratchpad a normal window"),
        Action::FocusNext => s(focus, "Focus the next window"),
        Action::FocusPrev => s(focus, "Focus the previous window"),
        Action::SwapNext => s(focus, "Move the window down the stack"),
        Action::SwapPrev => s(focus, "Move the window up the stack"),
        Action::ToggleFloat => s(focus, "Floating on or off"),
        Action::ToggleFullscreen => s(focus, "Fullscreen on or off"),
        Action::ToggleSticky => s(focus, "Sticky (on every workspace) on or off"),
        Action::Close => s(focus, "Close the window"),
        Action::FocusUrgent => s(focus, "Go to the window that asked for attention (urgent)"),
        Action::FocusMonitor(d) => (
            focus,
            format!(
                "Focus the {} screen",
                if d < 0 { "previous" } else { "next" }
            ),
        ),
        Action::TagMonitor(d) => (
            focus,
            format!(
                "Move the window to the {} screen",
                if d < 0 { "previous" } else { "next" }
            ),
        ),
        Action::FocusTag(n) => (tags, format!("Go to workspace {n}")),
        Action::NextTag(d) => (
            tags,
            format!("{} workspace", if d < 0 { "Previous" } else { "Next" }),
        ),
        Action::ToggleTag(n) => (tags, format!("Show workspace {n} too (toggle)")),
        Action::MoveToTag(n) => (tags, format!("Move the window to workspace {n}")),
        Action::ToggleMoveToTag(n) => (
            tags,
            format!("Put the window on workspace {n} too (toggle)"),
        ),
        Action::MoveFloat(dx, dy) => (float, format!("Move a floating window {}", arrow(dx, dy))),
        Action::ResizeFloat(dx, dy) => (
            float,
            format!(
                "{} a floating window {}",
                if dx > 0 || dy > 0 { "Grow" } else { "Shrink" },
                if dx != 0 {
                    "horizontally"
                } else {
                    "vertically"
                }
            ),
        ),
        Action::MediaKey(name) => (
            hardware,
            match name.as_str() {
                "XF86AudioRaiseVolume" => "Volume up".to_string(),
                "XF86AudioLowerVolume" => "Volume down".to_string(),
                "XF86AudioMute" => "Mute or unmute".to_string(),
                "XF86AudioPlay" => "Play or pause".to_string(),
                "XF86AudioPause" => "Pause".to_string(),
                "XF86AudioNext" => "Next track".to_string(),
                "XF86AudioPrev" => "Previous track".to_string(),
                "XF86AudioStop" => "Stop playback".to_string(),
                "XF86AudioMicMute" => "Mute or unmute the microphone".to_string(),
                other => format!("Media key {other}"),
            },
        ),
        Action::BrightnessStep(step) => (
            hardware,
            format!(
                "Screen brightness {} ({} %)",
                if step >= 0 { "up" } else { "down" },
                step.unsigned_abs()
            ),
        ),
    }
}

#[allow(dead_code)] // used by niructl, which shares this file
fn arrow(dx: i16, dy: i16) -> &'static str {
    match (dx.signum(), dy.signum()) {
        (-1, 0) => "left",
        (1, 0) => "right",
        (0, -1) => "up",
        (0, 1) => "down",
        _ => "diagonally",
    }
}

/// `super+shift+Return` as people write it: `Super+Shift+Enter`.
#[allow(dead_code)] // used by niructl, which shares this file
pub fn key_label(spec: &str) -> String {
    let Ok((mods, key)) = parse_keys(spec) else {
        return spec.to_string();
    };
    let mut out = String::new();
    for (bit, name) in [
        (1 << 6, "Super"),
        (1 << 2, "Ctrl"),
        (1 << 3, "Alt"),
        (1 << 5, "Mod3"),
        (1, "Shift"),
    ] {
        if mods & bit != 0 {
            out.push_str(name);
            out.push('+');
        }
    }
    let key = match key.as_str() {
        "Return" => "Enter".to_string(),
        "space" => "Space".to_string(),
        "grave" => "`".to_string(),
        "comma" => ",".to_string(),
        "period" => ".".to_string(),
        "slash" => "/".to_string(),
        "minus" => "-".to_string(),
        "equal" => "=".to_string(),
        k if k.chars().count() == 1 => k.to_uppercase(),
        k => k.to_string(),
    };
    out.push_str(&key);
    out
}

#[allow(dead_code)] // used by niruwm, not by niructl, which shares this file
/// Map each keysym to the key that types it, preferring the lowest shift
/// level: `7` is level 0 of its key and `slash` level 1 of the same key on a
/// Swedish layout. `rows` is one keysym list per keycode, from `min_keycode`.
pub fn keysym_codes(min_keycode: u8, rows: &[&[u32]]) -> HashMap<u32, (u8, usize)> {
    let mut by_sym: HashMap<u32, (u8, usize)> = HashMap::new();
    for (i, syms) in rows.iter().enumerate() {
        let code = (min_keycode as usize + i) as u8;
        for (level, s) in syms.iter().enumerate() {
            if *s == 0 {
                continue;
            }
            match by_sym.get(s) {
                Some(&(_, l)) if l <= level => {}
                _ => {
                    by_sym.insert(*s, (code, level));
                }
            }
        }
    }
    by_sym
}

#[allow(dead_code)] // used by niruwm, not by niructl, which shares this file
/// Bindings are grabbed by keycode, so two keysyms on one key (`7` and
/// `slash` on a Swedish layout) with the same modifiers are one physical
/// combination. Keep the binding whose keysym the key types without Shift or
/// AltGr; on a tie the earlier one. Returns the kept indexes and, for each
/// dropped one, `(dropped, kept)`.
pub fn shadowed(resolved: &[(u8, u32, usize)]) -> (Vec<usize>, Vec<(usize, usize)>) {
    let mut keep: Vec<usize> = Vec::new();
    let mut dropped = Vec::new();
    for (i, &(code, mods, level)) in resolved.iter().enumerate() {
        match keep
            .iter()
            .position(|&k| resolved[k].0 == code && resolved[k].1 == mods)
        {
            None => keep.push(i),
            Some(pos) => {
                let other = keep[pos];
                if level < resolved[other].2 {
                    dropped.push((other, i));
                    keep[pos] = i;
                } else {
                    dropped.push((i, other));
                }
            }
        }
    }
    keep.sort_unstable();
    (keep, dropped)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_shifted_keysym_never_shadows_the_key_itself() {
        // Swedish: keycode 16 types 7, Shift+7 types slash; keycode 61 types
        // minus. US: keycode 16 types 7/ampersand, 61 slash/question.
        let se: [&[u32]; 2] = [&[0x37, 0x2f, 0x37, 0x2f, 0x7b], &[0x2d, 0x5f]];
        let codes = keysym_codes(16, &se);
        assert_eq!(codes[&0x37], (16, 0));
        assert_eq!(codes[&0x2f], (16, 1));
        // super+slash (launcher_keys) comes first in the defaults, super+7
        // later; on Swedish the key must stay workspace 7.
        let super_ = 1 << 6;
        let resolved = [(16u8, super_, 1usize), (16, super_, 0), (16, super_ | 1, 0)];
        let (keep, dropped) = shadowed(&resolved);
        assert_eq!(keep, vec![1, 2]);
        assert_eq!(dropped, vec![(0, 1)]);
        // Same level: the earlier binding wins, as before.
        let (keep, dropped) = shadowed(&[(20, super_, 0), (20, super_, 0)]);
        assert_eq!(keep, vec![0]);
        assert_eq!(dropped, vec![(1, 0)]);
        // A keysym found on two keys uses the one that types it unshifted.
        let rows: [&[u32]; 3] = [&[0x31, 0x2f], &[0, 0], &[0x2f, 0x3f]];
        assert_eq!(keysym_codes(10, &rows)[&0x2f], (12, 0));
    }

    #[test]
    fn the_cheat_sheet_has_a_layout_independent_key() {
        let d = Action::defaults();
        let find = |k: &str| d.iter().find(|b| b.keys == k).map(|b| b.action.as_str());
        assert_eq!(find("super+slash"), Some("launcher_keys"));
        assert_eq!(find("super+F1"), Some("launcher_keys"));
    }

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
    fn tenth_workspace_has_all_modifiers() {
        let defaults = Action::defaults();
        for (keys, action) in [
            ("super+0", "tag 10"),
            ("super+shift+0", "movetotag 10"),
            ("super+ctrl+0", "toggletag 10"),
            ("super+ctrl+shift+0", "togglemovetotag 10"),
        ] {
            assert!(
                defaults
                    .iter()
                    .any(|raw| raw.keys == keys && raw.action == action),
                "missing default binding: {keys} -> {action}"
            );
        }
    }

    #[test]
    fn numbered_scratchpads_have_default_keys() {
        let defaults = Action::defaults();
        for n in 1..=9 {
            let show = defaults
                .iter()
                .find(|raw| raw.keys == format!("super+alt+{n}"))
                .expect("Super+Alt+N is bound");
            assert_eq!(parse_action(&show.action), Ok(Action::ScratchpadSlot(n)));
            let send = defaults
                .iter()
                .find(|raw| raw.keys == format!("super+alt+shift+{n}"))
                .expect("Super+Alt+Shift+N is bound");
            assert_eq!(parse_action(&send.action), Ok(Action::ScratchpadSend(n)));
        }
        assert_eq!(parse_action("scratchpad"), Ok(Action::Scratchpad));
        assert_eq!(parse_action("unscratch"), Ok(Action::Unscratch));
        assert!(parse_action("scratchpad 0").is_err());
        assert!(parse_action("scratchpad_send 10").is_err());
        assert!(parse_action("scratchpad_send").is_err());
    }

    #[test]
    fn every_default_has_a_description_and_a_known_group() {
        for raw in Action::defaults() {
            let (group, text) = describe_action(&raw.action);
            assert!(GROUPS.contains(&group), "{}: group {group}", raw.action);
            assert!(
                !text.is_empty() && !text.starts_with("Invalid"),
                "{}",
                raw.action
            );
            assert!(!text.contains('_'), "{}: {text}", raw.action);
        }
    }

    #[test]
    fn new_keys_are_bound_and_super_period_moved() {
        let d = Action::defaults();
        let find = |k: &str| d.iter().find(|r| r.keys == k).map(|r| r.action.as_str());
        assert_eq!(find("super+slash"), Some("launcher_keys"));
        assert_eq!(find("super+period"), Some("launcher_emoji"));
        assert_eq!(find("super+ctrl+c"), Some("shell nirushot pick"));
        assert_eq!(find("super+ctrl+o"), Some("shell nirushot ocr"));
        assert_eq!(find("super+alt+period"), Some("focusmon 1"));
        assert_eq!(find("super+comma"), Some("focusmon -1"));
        assert_eq!(parse_action("launcher_keys"), Ok(Action::LauncherKeys));
        assert_eq!(parse_action("launcher_emoji"), Ok(Action::LauncherEmoji));
        assert_eq!(find("super+o"), Some("launcher_files"));
        assert_eq!(parse_action("launcher_files"), Ok(Action::LauncherFiles));
    }

    #[test]
    fn key_labels_read_like_the_keyboard() {
        assert_eq!(key_label("super+shift+Return"), "Super+Shift+Enter");
        assert_eq!(key_label("super+slash"), "Super+/");
        assert_eq!(key_label("super+ctrl+c"), "Super+Ctrl+C");
        assert_eq!(key_label("XF86AudioMute"), "XF86AudioMute");
        assert_eq!(key_label("alt+Tab"), "Alt+Tab");
    }

    #[test]
    fn reject_unknown_modifier() {
        assert!(parse_keys("hyper+x").is_err());
    }
}
