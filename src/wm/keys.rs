use super::*;
use crate::bindings::{self, Action, Binding, LayoutId, RawBinding};

impl Wm {
    /// Resolve the configured bindings against the current keyboard mapping.
    ///
    /// Keys are stored as X keysym names, so a Swedish and a US layout share
    /// one binding table: `super+2` is the same physical key in both.
    pub(crate) fn resolve_bindings(&mut self) -> Result<()> {
        let setup = self.c.setup();
        let count = setup.max_keycode - setup.min_keycode + 1;
        let map = self
            .c
            .get_keyboard_mapping(setup.min_keycode, count)?
            .reply()?;
        let per = map.keysyms_per_keycode as usize;

        // Build keysym -> keycode using every level of every key.
        let mut by_sym: HashMap<u32, u8> = HashMap::new();
        for (i, syms) in map.keysyms.chunks(per).enumerate() {
            let code = (setup.min_keycode as usize + i) as u8;
            for s in syms {
                if *s != 0 {
                    by_sym.entry(*s).or_insert(code);
                }
            }
        }

        let raw: Vec<RawBinding> = if self.cfg.keys.is_empty() {
            Action::defaults()
        } else {
            self.cfg.keys.clone()
        };

        let mut list = Vec::with_capacity(raw.len());
        for r in raw {
            let (mods, key) = match bindings::parse_keys(&r.keys) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("niruwm: binding {:?}: {e}", r.keys);
                    continue;
                }
            };
            let action = match bindings::parse_action(&r.action) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("niruwm: binding {:?}: {e}", r.action);
                    continue;
                }
            };
            let Some(sym) = keysym_named(&key) else {
                eprintln!("niruwm: unknown keysym {key:?}");
                continue;
            };
            let Some(code) = by_sym.get(&sym).copied() else {
                // Not on this keyboard layout; skip instead of failing the WM.
                // Media keys are worth naming: when they are missing the user
                // presses them and nothing happens, with nothing to explain it.
                if key.starts_with("XF86") {
                    eprintln!("niruwm: {key} is not on this keyboard layout");
                }
                continue;
            };
            list.push(Binding { mods, action, code });
        }
        self.bindings = list;
        Ok(())
    }

    /// Grab every resolved binding, with Lock and NumLock tolerances.
    pub(crate) fn grab(&self) -> Result<()> {
        self.c.ungrab_key(0, self.root, ModMask::ANY)?;
        for b in &self.bindings {
            for extra in [0u16, 2, 16, 18] {
                let mods = ModMask::from(u16::try_from(b.mods | u32::from(extra)).unwrap_or(0));
                self.c.grab_key(
                    false,
                    self.root,
                    mods,
                    b.code,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn key(&mut self, e: KeyPressEvent) -> Result<bool> {
        // Compare only the modifier bits a binding can use. Lock (Caps) and
        // Mod2 (NumLock) change the state value but must not break matching;
        // mouse-button bits are irrelevant for a key press.
        const MEANINGFUL: u32 = 0x1 | 0x4 | 0x8 | 0x40;
        let state = u32::from(e.state.bits()) & MEANINGFUL;
        let index = self
            .bindings
            .iter()
            .position(|b| b.code == e.detail && (b.mods & MEANINGFUL) == state);
        let Some(b) = index.map(|i| self.bindings[i].clone()) else {
            // A grabbed key that matches no binding (layout changed under us).
            return Ok(true);
        };
        self.dispatch(b.action)
    }

    pub(super) fn dispatch(&mut self, action: Action) -> Result<bool> {
        let focused = self.focus;
        match action {
            Action::Terminal => {
                let cmd = self.cfg.terminal.clone();
                self.start(&cmd);
            }
            Action::TerminalAlt => {
                let cmd = if self.cfg.terminal_alt.is_empty() {
                    self.cfg.terminal.clone()
                } else {
                    self.cfg.terminal_alt.clone()
                };
                self.start(&cmd);
            }
            Action::Browser => {
                let cmd = if self.browser_present() {
                    vec!["nirupi-browser".into()]
                } else {
                    self.cfg.browser.clone()
                };
                self.start(&cmd);
            }
            Action::Launcher => {
                let cmd = self.cfg.launcher.clone();
                self.start(&cmd);
            }
            Action::LauncherUnified => {
                // Only call the NIRUPI launcher when it is actually there;
                // otherwise fall back to NIRUWM's own launcher so the binding
                // is never dead on a clean install.
                if self.helper_present("nirupi-launch.sh") {
                    self.shell("$HOME/.local/bin/nirupi-launch.sh");
                } else {
                    let cmd = self.cfg.launcher.clone();
                    self.start(&cmd);
                }
            }
            Action::Lock => self.lock_screen(),
            Action::Scratchpad => self.toggle_scratchpad()?,
            Action::MediaKey(name) => {
                // `media_key` knows which command each key needs. The previous
                // version guessed a program from the key name first and dropped
                // keys it did not recognise, so a correct binding could still
                // do nothing.
                self.media_key(&name);
            }
            Action::WindowSwitcher => {
                if self.helper_present("nirupi-window.sh") {
                    self.shell("$HOME/.local/bin/nirupi-window.sh");
                } else if self.command_present("rofi") {
                    self.start(&["rofi".into(), "-show".into(), "window".into()]);
                }
            }
            Action::Start(cmd) => {
                let args: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
                self.start(&args);
            }
            Action::Shell(cmd) => self.shell(&cmd),
            Action::FocusNext => self.focus_next(1)?,
            Action::FocusPrev => self.focus_next(-1)?,
            Action::FocusTag(n) => self.view_tag(tag_index(n))?,
            Action::NextTag(d) => self.next_tag(d)?,
            Action::ToggleTag(n) => self.toggle_tag(tag_index(n))?,
            Action::MoveToTag(n) => self.move_to_tag(tag_index(n), false)?,
            Action::ToggleMoveToTag(n) => self.move_to_tag(tag_index(n), true)?,
            Action::SetLayout(l) => {
                self.layout = match l {
                    LayoutId::Tile => Layout::Tile,
                    LayoutId::Left => Layout::Left,
                    LayoutId::Monocle => Layout::Monocle,
                    LayoutId::Centered => Layout::Centered,
                    LayoutId::CenteredFloat => Layout::CenteredFloat,
                    LayoutId::Floating => Layout::Floating,
                    LayoutId::Fibonacci => Layout::Fibonacci,
                };
            }
            Action::LayoutNext => self.layout = self.layout.next(),
            Action::LayoutPrev => self.layout = self.layout.prev(),
            // Master count is per screen: changing it on one monitor must not
            // rearrange the other.
            Action::IncMaster => return self.change_master(1).map(|_| true),
            Action::DecMaster => return self.change_master(-1).map(|_| true),
            Action::IncMasterRatio => {
                self.cfg.master_ratio = (self.cfg.master_ratio + 0.05).min(0.9)
            }
            Action::DecMasterRatio => {
                self.cfg.master_ratio = (self.cfg.master_ratio - 0.05).max(0.1)
            }
            Action::ToggleFloat => {
                if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == focused) {
                    c.floating = !c.floating;
                }
            }
            Action::ToggleFullscreen => {
                if let Some(w) = focused {
                    return self.toggle_fullscreen(w).map(|_| true);
                }
            }
            Action::Close => {
                if let Some(w) = focused {
                    self.close(w)?;
                }
            }
            Action::Quit => return Ok(false),
            Action::Reload => self.reload()?,
            Action::Theme(name) => {
                let mut cfg = self.cfg.clone();
                cfg.theme = name;
                if let Ok(t) = Theme::load(&cfg.theme) {
                    self.cfg = cfg;
                    self.theme = t;
                    self.update_borders()?;
                }
            }
            Action::ToggleGaps => {
                self.cfg.gap = if self.cfg.gap == 0 { 8 } else { 0 };
            }
            Action::MoveFloat(dx, dy) => {
                if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == focused) {
                    c.floating = true;
                    c.geom.x = c.geom.x.saturating_add(dx);
                    c.geom.y = c.geom.y.saturating_add(dy);
                }
            }
            Action::ResizeFloat(dx, dy) => {
                if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == focused) {
                    c.floating = true;
                    c.geom.w = (c.geom.w as i32 + dx as i32).clamp(80, 10000) as u16;
                    c.geom.h = (c.geom.h as i32 + dy as i32).clamp(60, 10000) as u16;
                }
            }
            Action::SwapNext => self.swap(1)?,
            Action::SwapPrev => self.swap(-1)?,
            Action::FocusMonitor(d) => self.focus_monitor(d)?,
            Action::TagMonitor(d) => self.move_to_monitor(d)?,
        }
        self.arrange()?;
        Ok(true)
    }

    /// Run a shell command detached from the WM process.
    ///
    /// `sh -c` is used so a command may use `$HOME`, pipes or `&&` as written
    /// in NIRUPI's helper scripts. The child is not waited for: a launcher
    /// that stays open must not block the event loop.
    fn shell(&self, cmd: &str) {
        Self::spawn_detached(&["sh".to_string(), "-c".to_string(), cmd.to_string()]);
    }

    /// Run one media key.
    ///
    /// NIRUWM does the work itself instead of shelling out to a helper, so the
    /// keys work with nothing installed beyond `wpctl` or `playerctl`.
    fn media_key(&mut self, name: &str) {
        let cmd: Option<(&str, Vec<String>)> = match name {
            // `sh -c SCRIPT sh OP`: the operation must be `$1` inside the
            // script. Appending it to the script text made `op` empty and the
            // whole case statement was skipped, which is why the key showed a
            // notification and changed nothing.
            "XF86AudioRaiseVolume" => Some((
                "sh",
                vec!["-c".into(), VOLUME_SCRIPT.into(), "sh".into(), "up".into()],
            )),
            "XF86AudioLowerVolume" => Some((
                "sh",
                vec![
                    "-c".into(),
                    VOLUME_SCRIPT.into(),
                    "sh".into(),
                    "down".into(),
                ],
            )),
            "XF86AudioMute" => Some((
                "sh",
                vec![
                    "-c".into(),
                    VOLUME_SCRIPT.into(),
                    "sh".into(),
                    "mute".into(),
                ],
            )),
            "XF86AudioPlay" | "XF86AudioPause" => Some(("playerctl", vec!["play-pause".into()])),
            "XF86AudioNext" => Some(("playerctl", vec!["next".into()])),
            "XF86AudioPrev" => Some(("playerctl", vec!["previous".into()])),
            _ => None,
        };
        if let Some((program, args)) = cmd {
            let mut full = vec![program.to_string()];
            full.extend(args);
            Self::spawn_detached(&full);
        }
    }

    /// Show or hide the scratchpad.
    ///
    /// The scratchpad is a single floating window, kept alive between uses and
    /// placed over the focused screen. Starting it the first time runs the
    /// configured command; afterwards the same window is shown and hidden, which
    /// is what makes it feel instant.
    pub(crate) fn toggle_scratchpad(&mut self) -> Result<()> {
        if let Some(w) = self.scratchpad {
            if self.clients.iter().any(|c| c.win == w) {
                if self.scratchpad_shown {
                    let tag = self.active_tag();
                    let monitor = self.focused_monitor;
                    if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                        c.floating = false;
                        c.tag = tag;
                        c.monitor = monitor;
                    }
                    self.hidden.insert(w);
                    *self.pending_unmaps.entry(w).or_default() += 1;
                    self.c.unmap_window(w)?;
                    self.scratchpad_shown = false;
                } else {
                    let screen = self
                        .monitors
                        .get(self.focused_monitor)
                        .map(|m| m.rect)
                        .unwrap_or_else(|| self.work_area());
                    let w_px = (f32::from(screen.w) * self.cfg.scratchpad.width) as u16;
                    let h_px = (f32::from(screen.h) * self.cfg.scratchpad.height) as u16;
                    let geom = Rect {
                        x: screen.x + ((screen.w - w_px) / 2) as i16,
                        y: screen.y + ((screen.h - h_px) / 2) as i16,
                        w: w_px,
                        h: h_px,
                    };
                    let tag = self.active_tag();
                    let monitor = self.focused_monitor;
                    if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                        c.floating = true;
                        c.geom = geom;
                        c.tag = tag;
                        c.monitor = monitor;
                    }
                    self.hidden.remove(&w);
                    self.c.map_window(w)?;
                    self.set_focus(w)?;
                    self.scratchpad_shown = true;
                }
                return self.arrange();
            }
            // The window was closed by the application.
            self.scratchpad = None;
            self.scratchpad_shown = false;
        }
        // First use: start the command. The window becomes the scratchpad when
        // it appears, because `manage` checks for it below.
        let cmd = self.cfg.scratchpad.command.clone();
        self.start(&cmd);
        Ok(())
    }

    /// Lock the screen. Preference order: nirulock, NIRUPI's wrapper, slock.
    ///
    /// Never fails silently: if nothing is installed the user gets a message
    /// instead of a key that appears broken.
    fn lock_screen(&mut self) {
        // Absolute paths first. A session started by a display manager does
        // not always have `~/.local/bin` in PATH, and a bare name then fell
        // through to slock, which does not speak the NIRU lock protocol.
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = [
            format!("{home}/.local/bin/nirulock"),
            format!("{home}/.local/bin/nirupi-lock.sh"),
            "slock".to_string(),
        ];
        for candidate in candidates {
            let present = if candidate.contains('/') {
                std::path::Path::new(&candidate).is_file()
            } else {
                self.command_present(&candidate)
            };
            if present {
                self.start(std::slice::from_ref(&candidate));
                return;
            }
        }
        self.shell(
            "notify-send -a NIRUWM 'Lock' 'No screen locker found: install nirulock, nirupi-lock or slock' 2>/dev/null || true",
        );
        eprintln!("niruwm: no screen locker found (nirulock, nirupi-lock.sh, slock)");
    }

    fn browser_present(&self) -> bool {
        self.helper_present("nirupi-browser")
    }

    fn helper_present(&self, name: &str) -> bool {
        let path = std::path::Path::new(&std::env::var("HOME").unwrap_or_default())
            .join(".local/bin")
            .join(name);
        path.is_file()
    }

    fn command_present(&self, name: &str) -> bool {
        which(name)
    }
}

/// Map a binding's tag number to a zero-based workspace index.
///
/// Users write `1`..`9` for the nine workspaces; `10` is the `0` key, which
/// dwm binds to the last tag.
pub(crate) fn tag_index(n: u32) -> u32 {
    match n {
        0 => 0,
        1..=9 => n - 1,
        _ => 8,
    }
}

/// Volume key commands.
///
/// They change the volume and then show the result, because a key that only
/// changes something you cannot see is not feedback. `notify-send` reaches
/// whatever notification daemon is running (dunst, NIRUPI's, or the desktop's),
/// and silence is acceptable when none is.
/// Volume key commands.
///
/// They change the volume and then show the result, because a key that changes
/// something invisible is not feedback. `notify-send` reaches whatever
/// notification daemon runs (dunst, NIRUPI's, or none, in which case the call
/// is silent).
///
/// One notification is replaced by the next instead of stacking: pressing the
/// key five times shows one bubble with the final level, not five. That is what
/// `-r` (replace id) does, and the id is stable per operation.
const VOLUME_SCRIPT: &str = r#"op="$1"
case "$op" in
  up)   wpctl set-mute @DEFAULT_AUDIO_SINK@ 0 2>/dev/null; wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+ 2>/dev/null || amixer -q set Master 5%+ unmute 2>/dev/null ;;
  down) wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%- 2>/dev/null || amixer -q set Master 5%- 2>/dev/null ;;
  mute) wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle 2>/dev/null || amixer -q set Master toggle 2>/dev/null ;;
esac
v=$(wpctl get-volume @DEFAULT_AUDIO_SINK@ 2>/dev/null | awk '{printf "%d", $2*100}')
[ -n "$v" ] || v=$(amixer get Master 2>/dev/null | sed -n 's/.*\[\([0-9]*\)%\].*/\1/p' | head -1)
if wpctl get-volume @DEFAULT_AUDIO_SINK@ 2>/dev/null | grep -q MUTED; then
  body="Muted"
else
  body="${v}%"
fi
# A stable id per operation, so repeated presses replace the same bubble.
case "$op" in up) id=9101 ;; down) id=9102 ;; *) id=9103 ;; esac
notify-send -a NIRU -r "$id" -h int:value:"${v:-0}" -t 1200 "Volume" "$body" 2>/dev/null
exit 0"#;

/// Look up a program in PATH without spawning anything./// Look up a program in PATH without spawning anything.
pub(crate) fn which(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(':')
        .filter(|p| !p.is_empty())
        .any(|dir| std::path::Path::new(dir).join(name).is_file())
}

/// X keysym values for named keys. Only names that cannot be derived from the
/// keysym table are listed; printable characters use their Latin-1 value.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keysyms_for_common_keys() {
        assert_eq!(keysym_named("Return"), Some(0xff0d));
        assert_eq!(keysym_named("space"), Some(0x20));
        assert_eq!(keysym_named("2"), Some(0x32));
        assert_eq!(keysym_named("F5"), Some(0xffbe + 4));
        assert_eq!(keysym_named("Print"), Some(0xff61));
        assert_eq!(keysym_named("nonsense"), None);
    }

    #[test]
    fn every_media_key_has_a_keysym() {
        // The default table binds these by name. A missing keysym silently
        // dropped the binding, which is how the media keys once did nothing.
        for name in [
            "XF86AudioMute",
            "XF86AudioLowerVolume",
            "XF86AudioRaiseVolume",
            "XF86AudioPlay",
            "XF86AudioPause",
            "XF86AudioNext",
            "XF86AudioPrev",
        ] {
            assert!(keysym_named(name).is_some(), "{name} has no keysym");
        }
    }

    #[test]
    fn tag_numbers_are_one_based() {
        assert_eq!(tag_index(1), 0);
        assert_eq!(tag_index(3), 2);
        assert_eq!(tag_index(9), 8);
        assert_eq!(tag_index(10), 8, "the 0 key selects the last workspace");
    }
}
