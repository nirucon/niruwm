use super::*;
use crate::bindings::{self, Action, Binding, RawBinding};

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

        let raw: Vec<RawBinding> = self.cfg.effective_keys();

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
            if action == Action::Unbind {
                continue;
            }
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
        const MEANINGFUL: u32 = 0x1 | 0x4 | 0x8 | 0x20 | 0x40;
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
            Action::Unbind => {}
            Action::Terminal => self.start_terminal(self.cfg.terminal.clone()),
            Action::TerminalAlt => {
                let cmd = if self.cfg.terminal_alt.is_empty() {
                    self.cfg.terminal.clone()
                } else {
                    self.cfg.terminal_alt.clone()
                };
                self.start_terminal(cmd);
            }
            Action::Browser => {
                let cmd = self.cfg.browser.clone();
                self.start(&cmd);
            }
            Action::Launcher => {
                if self.command_present("nirulauncher") {
                    self.start(&["nirulauncher".into(), "--apps".into()]);
                } else {
                    let cmd = self.cfg.launcher.clone();
                    self.start(&cmd);
                }
            }
            Action::LauncherSearch => {
                if self.command_present("nirulauncher") {
                    self.start(&["nirulauncher".into(), "--search".into()]);
                } else {
                    let cmd = self.cfg.launcher.clone();
                    self.start(&cmd);
                }
            }
            Action::LauncherUnified => {
                if self.command_present("nirulauncher") {
                    self.start(&["nirulauncher".into(), "--actions".into()]);
                } else {
                    let cmd = self.cfg.launcher.clone();
                    self.start(&cmd);
                }
            }
            Action::LauncherActions => {
                if self.command_present("nirulauncher") {
                    self.start(&["nirulauncher".into(), "--system-actions".into()]);
                } else {
                    let cmd = self.cfg.launcher.clone();
                    self.start(&cmd);
                }
            }
            Action::Lock => self.lock_screen(),
            Action::Suspend => {
                // The helper waits for NIRULOCK's readiness and the three
                // second safety window, so the WM event loop never blocks.
                self.spawn_helper("suspend", &["niructl".into(), "suspend".into()]);
            }
            Action::Scratchpad => self.toggle_scratchpad()?,
            Action::BrightnessStep(step) => {
                let direction = if step >= 0 { "up" } else { "down" };
                self.spawn_helper(
                    "brightness",
                    &["niructl".into(), "brightness".into(), direction.into()],
                );
            }
            Action::NightlightToggle => {
                self.spawn_helper(
                    "nightlight",
                    &["niructl".into(), "nightlight".into(), "toggle".into()],
                );
            }
            Action::DndToggle => {
                self.spawn_helper("dnd", &["niructl".into(), "dnd".into(), "toggle".into()]);
            }
            Action::IdleToggle => {
                self.spawn_helper("idle", &["niructl".into(), "idle".into(), "toggle".into()]);
            }
            Action::MediaKey(name) => {
                // `media_key` knows which command each key needs. The previous
                // version guessed a program from the key name first and dropped
                // keys it did not recognise, so a correct binding could still
                // do nothing.
                self.media_key(&name);
            }
            Action::WindowSwitcher => {
                if self.command_present("nirulauncher") {
                    self.start(&["nirulauncher".into(), "--windows".into()]);
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
            Action::NextTag(d) => {
                self.next_tag(d)?;
                // One line per step: when a replay goes the wrong way (a
                // modifier that did not arrive, say), this is the evidence.
                eprintln!(
                    "niruwm: tag {} after {}",
                    self.active_tag() + 1,
                    if d > 0 { "next" } else { "previous" }
                );
            }
            Action::ToggleTag(n) => self.toggle_tag(tag_index(n))?,
            Action::MoveToTag(n) => self.move_to_tag(tag_index(n), false)?,
            Action::ToggleMoveToTag(n) => self.move_to_tag(tag_index(n), true)?,
            // A layout belongs to one workspace: the one shown on the
            // focused screen. Changing it there leaves every other workspace,
            // on every screen, exactly as it was.
            Action::SetLayout(l) => {
                self.set_layout(l.layout());
                self.publish_layout()?;
            }
            Action::LayoutNext => {
                let next = self.layout().next();
                self.set_layout(next);
                self.publish_layout()?;
            }
            Action::LayoutPrev => {
                let prev = self.layout().prev();
                self.set_layout(prev);
                self.publish_layout()?;
            }
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
            Action::Quit => {
                // The session ends: what was open is what should come back.
                if let Err(e) = self.save_session() {
                    eprintln!("niruwm: could not save the session: {e}");
                }
                return Ok(false);
            }
            Action::Reload => {
                self.reload()?;
            }
            Action::SaveSession => {
                if let Err(e) = self.save_session() {
                    eprintln!("niruwm: could not save the session: {e}");
                }
            }
            Action::ClearSession => {
                if let Err(e) = crate::session_state::clear() {
                    eprintln!("niruwm: could not clear the session: {e}");
                } else {
                    eprintln!("niruwm: saved session cleared");
                }
            }
            Action::Theme(name) => {
                let mut cfg = self.cfg.clone();
                cfg.theme = name;
                if let Ok(t) = Theme::load(&cfg.theme) {
                    self.cfg = cfg;
                    self.theme = t;
                    self.publish_theme()?;
                    self.update_borders()?;
                    if let Err(e) = self.wallpaper.set_theme(&self.cfg.theme) {
                        eprintln!("niruwm: wallpaper theme: {e}");
                    }
                    let terminal_theme = self.cfg.theme.clone();
                    self.run_terminal_theme_hook(&terminal_theme);
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
    /// `sh -c` is used so a command may use `$HOME`, pipes or `&&`. The child
    /// is not waited for: a launcher that stays open must not block the event
    /// loop.
    fn shell(&mut self, cmd: &str) {
        self.spawn_detached(&["sh".to_string(), "-c".to_string(), cmd.to_string()]);
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
            self.spawn_helper(&format!("media:{name}"), &full);
        }
    }

    /// Write the open windows to the session file.
    ///
    /// One entry per window, in the order they were mapped, so the next login
    /// starts them in the order they appear: a browser before the editor that
    /// the editor remembered being next to.
    pub(crate) fn save_session(&self) -> Result<()> {
        let mut entries = Vec::new();
        for c in &self.clients {
            // A scratchpad is part of the window manager, not a session,
            // whether it is shown or hidden when the session ends.
            if self.scratchpad == Some(c.win) {
                continue;
            }
            let Some(pid) = c.pid else { continue };
            let Some(program) = command_line(pid) else {
                continue;
            };
            entries.push(crate::session_state::Entry {
                program,
                tag: c.tag,
                monitor: c.monitor,
                floating: c.floating,
                focused: self.focus == Some(c.win),
            });
        }
        crate::session_state::save(&entries)?;
        eprintln!("niruwm: session saved: {} window(s)", entries.len());
        Ok(())
    }

    /// Start the programs a saved session describes.
    ///
    /// Every entry becomes an ordinary window: it is tiled on its workspace
    /// like anything else, and if the program is gone the user gets one
    /// notification, not a session that fails to start.
    pub(crate) fn restore_session(&mut self) -> Result<()> {
        let entries = crate::session_state::load();
        if entries.is_empty() {
            return Ok(());
        }
        let mut valid = Vec::new();
        let mut missing = Vec::new();
        for e in crate::session_state::ordered(&entries) {
            // Old session files could contain the scratchpad even though it is
            // not part of the desktop the user arranged.
            if e.program == self.cfg.scratchpad.command {
                continue;
            }
            let Some(program) = e.program.first() else {
                continue;
            };
            let present = if program.contains('/') {
                std::path::Path::new(program).is_file()
            } else {
                which(program)
            };
            if !present {
                if !missing.contains(program) {
                    missing.push(program.clone());
                }
                continue;
            }
            // The file was written on the screens and workspace counts of the
            // previous session. Clamp it to what exists now instead of asking
            // a manager to show workspace 8 on a screen that has four.
            let screen = e.monitor.min(self.monitors.len().saturating_sub(1));
            let max_tag = self
                .monitors
                .get(screen)
                .map(|m| m.workspaces.len().saturating_sub(1) as u32)
                .unwrap_or(8);
            valid.push(crate::session_state::Entry {
                tag: e.tag.min(max_tag),
                monitor: screen,
                ..e
            });
        }

        // One process can own several saved windows. Start each distinct
        // command once and let the mapped windows consume their own records.
        let mut pending = Vec::new();
        let mut remaining = 0;
        for (command, windows) in crate::session_state::launches(&valid) {
            let Some(pid) = self.spawn_owned_pid(&command) else {
                continue;
            };
            remaining += windows.len();
            pending.push(super::PendingRestore {
                command,
                pid: Some(pid),
                windows: windows
                    .into_iter()
                    .map(|e| super::PendingWindow {
                        tag: e.tag,
                        monitor: e.monitor,
                        floating: e.floating,
                        focused: e.focused,
                    })
                    .collect(),
            });
        }
        if !pending.is_empty() {
            self.pending_restore = pending;
            self.restore_remaining = remaining;
            // Programs started by a session can take a while to map. Stop
            // waiting after ten seconds so a saved window the program does not
            // recreate cannot own a later, unrelated window.
            self.restore_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
        }

        if !missing.is_empty() {
            eprintln!(
                "niruwm: session: {} program(s) are not installed: {}",
                missing.len(),
                missing.join(", ")
            );
            let list = missing.join(", ");
            self.spawn_detached(&[
                "notify-send".into(),
                "-a".into(),
                "NIRUWM".into(),
                "Session partly restored".into(),
                format!("Not installed: {list}"),
            ]);
        }
        Ok(())
    }

    /// Take the saved placement for a window that belongs to a restore launch.
    ///
    /// Matching uses `_NET_WM_PID` first. A program that forks or execs reports
    /// a different pid, so its command line is the second chance. Only a single
    /// pending launch with no pid information at all falls back to order.
    pub(crate) fn take_restored(
        &mut self,
        pid: Option<u32>,
        scratchpad: bool,
    ) -> Option<super::PendingWindow> {
        self.restore_deadline?;
        let index = self
            .pending_restore
            .iter()
            .position(|r| pid.is_some() && r.pid == pid)
            .or_else(|| {
                let command = pid.and_then(command_line)?;
                self.pending_restore
                    .iter()
                    .position(|r| same_command(&r.command, &command))
            })
            // Some X clients still do not publish `_NET_WM_PID`. During the
            // bounded restore window, order is then the only signal we have;
            // the oldest record is still better than dropping the placement.
            .or_else(|| pid.is_none().then_some(0))?;
        let entry = self.pending_restore[index].windows.pop_front()?;
        self.restore_remaining = self.restore_remaining.saturating_sub(1);
        if self.pending_restore[index].windows.is_empty() {
            self.pending_restore.remove(index);
        }
        // An old session may still name the scratchpad. Consume its record but
        // let `manage` treat the window as the scratchpad it is.
        if scratchpad {
            return None;
        }
        Some(entry)
    }

    /// Finish restore when every saved window has mapped, or when the deadline
    /// passes for programs that open fewer windows than they did last time.
    pub(crate) fn finish_restore_if_due(&mut self) -> Result<()> {
        if let Some(deadline) = self.restore_deadline {
            if std::time::Instant::now() >= deadline {
                self.finish_restore()?;
            }
        }
        Ok(())
    }

    /// Stop waiting for more restored windows and show the saved focus.
    ///
    /// Restoring does not switch workspaces while programs start. When they
    /// have all mapped, the screen and workspace that held focus are shown
    /// again, so the session opens where the user left it.
    pub(crate) fn finish_restore(&mut self) -> Result<()> {
        if self.restore_deadline.take().is_none() {
            return Ok(());
        }
        self.pending_restore.clear();
        self.restore_remaining = 0;
        let Some((window, monitor, tag)) = self.restore_focus.take() else {
            return Ok(());
        };
        if !self.clients.iter().any(|c| c.win == window) {
            return Ok(());
        }
        let monitor = monitor.min(self.monitors.len().saturating_sub(1));
        self.focused_monitor = monitor;
        if let Some(m) = self.monitors.get_mut(monitor) {
            m.active = (tag as usize).min(m.workspaces.len().saturating_sub(1));
        }
        self.arrange()?;
        if self.clients.iter().any(|c| c.win == window) {
            self.set_focus(window)?;
        }
        Ok(())
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

    /// Start the configured terminal, or the first one that is installed.
    ///
    /// The default is kitty, which a minimal machine often does not have. A
    /// terminal is the way out of every other problem, so it must open when
    /// any of the usual ones exists; the notification from `start` still names
    /// what was tried if none does.
    fn start_terminal(&mut self, cmd: Vec<String>) {
        let first = cmd.first().cloned().unwrap_or_default();
        if self.command_present(&first) {
            self.start(&cmd);
            return;
        }
        for alternative in ["kitty", "alacritty", "foot", "xterm"] {
            if self.command_present(alternative) {
                eprintln!("niruwm: {first} is not installed, starting {alternative}");
                self.start(&[alternative.to_string()]);
                return;
            }
        }
        self.start(&cmd);
    }

    /// Lock the screen. Preference order: nirulock, then slock.
    ///
    /// Never fails silently: if nothing is installed the user gets a message
    /// instead of a key that appears broken.
    fn lock_screen(&mut self) {
        // Absolute paths first. A session started by a display manager does
        // not always have `~/.local/bin` in PATH, and a bare name then fell
        // through to slock, which does not speak the NIRU lock protocol.
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = vec![format!("{home}/.local/bin/nirulock"), "slock".to_string()];
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
        let body = "No screen locker found: install nirulock or slock";
        self.spawn_detached(&[
            "notify-send".into(),
            "-a".into(),
            "NIRUWM".into(),
            "Lock".into(),
            body.into(),
        ]);
        eprintln!("niruwm: {body}");
    }

    fn command_present(&self, name: &str) -> bool {
        which(name)
    }
}

/// Map a binding's tag number to a zero-based workspace index.
///
/// Users write `1`..`10`; `10` is the `0` key, which dwm binds to the last tag.
pub(crate) fn tag_index(n: u32) -> u32 {
    match n {
        0 => 0,
        1..=10 => n - 1,
        _ => 9,
    }
}

/// Volume key commands.
///
/// They change the volume and then show the result, because a key that only
/// changes something you cannot see is not feedback. `notify-send` reaches
/// whatever notification daemon is running, and silence is acceptable when
/// none is.
/// Volume key commands.
///
/// They change the volume and then show the result, because a key that changes
/// something invisible is not feedback. `notify-send` reaches whatever
/// notification daemon runs, or is silent when none is.
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
# Prefer the suite OSD; a missing daemon falls back to notify-send.
if command -v nirunotify >/dev/null 2>&1 && nirunotify --osd volume "$body" >/dev/null 2>&1; then
  exit 0
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

/// Compare a saved command with a running process command line.
///
/// A saved `kitty` and an exec'd `/usr/bin/kitty` are the same program. The
/// arguments must still match exactly.
fn same_command(saved: &[String], running: &[String]) -> bool {
    fn base(s: &str) -> &str {
        s.rsplit('/').next().unwrap_or(s)
    }
    if saved == running {
        return true;
    }
    saved.len() == running.len()
        && !saved.is_empty()
        && base(&saved[0]) == base(&running[0])
        && saved[1..] == running[1..]
}

/// The full command line of a process, from `/proc`.
///
/// The session has to start the exact program that was running: a browser
/// started with a profile argument must come back with it. `/proc/PID/cmdline`
/// is NUL separated and keeps the arguments as they were.
pub(super) fn command_line(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let parts: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).to_string())
        .collect();
    if parts.is_empty() {
        return None;
    }
    // A program started through a shell wrapper reports the wrapper. Names
    // that cannot be restarted usefully are skipped rather than saved.
    let first = parts[0].rsplit('/').next().unwrap_or("");
    if matches!(
        first,
        "sh" | "bash" | "zsh" | "fish" | "dash" | "python" | "python3" | "perl"
    ) {
        return None;
    }
    Some(parts)
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
        assert_eq!(tag_index(10), 9, "the 0 key selects workspace 10");
    }
}
