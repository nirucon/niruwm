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

        // Build keysym -> keycode using every level of every key, preferring
        // the key that types the keysym without Shift or AltGr.
        let rows: Vec<&[u32]> = map.keysyms.chunks(per.max(1)).collect();
        let by_sym = bindings::keysym_codes(setup.min_keycode, &rows);

        let raw: Vec<RawBinding> = self.cfg.effective_keys();

        let mut list = Vec::with_capacity(raw.len());
        let mut names = Vec::with_capacity(raw.len());
        let mut levels = Vec::with_capacity(raw.len());
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
            let Some((code, level)) = by_sym.get(&sym).copied() else {
                // Not on this keyboard layout; skip instead of failing the WM.
                // Media keys are worth naming: when they are missing the user
                // presses them and nothing happens, with nothing to explain it.
                if key.starts_with("XF86") {
                    eprintln!("niruwm: {key} is not on this keyboard layout");
                }
                continue;
            };
            list.push(Binding {
                mods,
                sym,
                action,
                code,
            });
            names.push(r.keys);
            levels.push(level);
        }
        // Two keysyms on one key with the same modifiers are one grab. The
        // binding for what the key types unshifted wins, so on a Swedish
        // layout Super+7 stays workspace 7 rather than becoming Super+/.
        let resolved: Vec<(u8, u32, usize)> = list
            .iter()
            .zip(&levels)
            .map(|(b, &l)| (b.code, b.mods, l))
            .collect();
        let (keep, dropped) = bindings::shadowed(&resolved);
        for (d, k) in dropped {
            eprintln!(
                "niruwm: {} is the same key as {} on this keyboard layout; {} is used",
                names[d], names[k], names[k]
            );
        }
        let list: Vec<Binding> = keep.into_iter().map(|i| list[i].clone()).collect();
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
        if matches!(b.action, Action::Quit) {
            // The session log must say why a session ended.
            eprintln!(
                "niruwm: quit requested with a key binding (keycode {}, modifiers {:#x}); the session ends",
                e.detail, state
            );
        }
        self.dispatch(b.action)
    }

    /// The action bound to `mods` + `sym`, to run as if the key were
    /// pressed (`_NIRUWM_COMMAND` 19, from the cheat sheet's Enter). Only a
    /// binding in the live table can run this way, so the channel cannot
    /// start anything the user has not bound, and nothing runs while a lock
    /// screen holds the keyboard.
    pub(super) fn bound_action(&self, mods: u32, sym: u32) -> Option<Action> {
        const MEANINGFUL: u32 = 0x1 | 0x4 | 0x8 | 0x20 | 0x40;
        if self.keys_released {
            return None;
        }
        let action = self
            .bindings
            .iter()
            .find(|b| b.sym == sym && (b.mods & MEANINGFUL) == (mods & MEANINGFUL))
            .map(|b| b.action.clone())?;
        if matches!(action, Action::Quit) {
            eprintln!(
                "niruwm: quit requested through _NIRUWM_COMMAND 19 (a bound key run from the cheat sheet); the session ends"
            );
        }
        Some(action)
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
            Action::LauncherClipboard => {
                if self.command_present("nirulauncher") {
                    self.start(&["nirulauncher".into(), "--clipboard".into()]);
                }
            }
            Action::LauncherKeys => {
                if self.command_present("nirulauncher") {
                    self.start(&["nirulauncher".into(), "--keys".into()]);
                }
            }
            Action::LauncherEmoji => {
                if self.command_present("nirulauncher") {
                    self.start(&["nirulauncher".into(), "--emoji".into()]);
                }
            }
            Action::LauncherManual => {
                if self.command_present("nirulauncher") {
                    self.start(&["nirulauncher".into(), "--manual".into()]);
                }
            }
            Action::LauncherFiles => {
                if self.command_present("nirulauncher") {
                    self.start(&["nirulauncher".into(), "--files".into()]);
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
            Action::Scratchpad => self.scratchpad(0, super::ScratchOp::Toggle)?,
            Action::ScratchpadSlot(n) => self.scratchpad(n, super::ScratchOp::Toggle)?,
            Action::ScratchpadSend(n) => self.send_to_scratch(n)?,
            Action::Unscratch => {
                if let Some(w) = focused {
                    self.unscratch(w)?;
                }
            }
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
            Action::StudioToggle => {
                self.spawn_helper(
                    "studio",
                    &["niructl".into(), "studio".into(), "toggle".into()],
                );
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
            Action::SetScrollColumns(mode) => {
                if self.set_scroll_columns(Some(mode)) {
                    self.publish_layout()?;
                }
            }
            Action::ToggleScrollColumns => {
                if self.set_scroll_columns(None) {
                    self.publish_layout()?;
                }
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
            // Master count and ratio belong to the focused workspace: changing
            // one workspace must not rearrange any other workspace or screen.
            Action::IncMaster => return self.change_master(1).map(|_| true),
            Action::DecMaster => return self.change_master(-1).map(|_| true),
            Action::IncMasterRatio => self.change_master_ratio(0.05)?,
            Action::DecMasterRatio => self.change_master_ratio(-0.05)?,
            Action::ToggleFloat => {
                // A scratchpad always floats; "un-floating" one makes it a
                // normal tiled window on the current workspace.
                if focused.is_some_and(|w| self.is_scratch(w)) {
                    if let Some(w) = focused {
                        self.unscratch(w)?;
                    }
                    if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == focused) {
                        c.floating = false;
                    }
                } else if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == focused) {
                    c.floating = !c.floating;
                }
            }
            Action::ToggleSticky => {
                if let Some(focused) = self.focus.filter(|w| !self.is_scratch(*w)) {
                    self.toggle_sticky(focused)?;
                }
            }
            Action::BarTop => self.set_bar_position("top"),
            Action::BarBottom => self.set_bar_position("bottom"),
            Action::BarHide => self.set_bar_visible(false),
            Action::BarShow => self.set_bar_visible(true),
            Action::BarToggle => {
                if self.bar_active() {
                    self.set_bar_visible(false);
                } else {
                    self.set_bar_visible(true);
                }
            }
            Action::BarCycle => self.cycle_bar(),
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
            Action::FocusUrgent => self.focus_urgent()?,
            Action::SwallowToggle => {
                let on = !self.swallow_enabled;
                self.set_swallow(on);
            }
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
            // whether it is shown or hidden when the session ends. That holds
            // for windows sent to a slot as well.
            if self.scratch_owner(c.win).is_some() {
                continue;
            }
            // A dialog is not a program to start again; its application is.
            if c.parent.is_some() {
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
            if self
                .cfg
                .scratchpad
                .commands()
                .any(|command| command == e.program.as_slice())
            {
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
    ///
    /// A key binding is a manual lock, so NIRULOCK may play its pause music.
    /// The reason goes in `NIRULOCK_REASON` rather than `--reason`: an older
    /// NIRULOCK rejects unknown arguments and would not lock at all, while it
    /// simply ignores the variable (and slock does too).
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
                self.spawn_owned_env(
                    std::slice::from_ref(&candidate),
                    &[(LOCK_REASON_ENV, "manual")],
                );
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

/// Tells NIRULOCK (0.25+) why it was started; see its `--reason`.
pub(crate) const LOCK_REASON_ENV: &str = "NIRULOCK_REASON";

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
/// They change the volume and then show the result, because a key that changes
/// something invisible is not feedback. The suite OSD (`nirunotify --osd`)
/// shows the level and, in the same call, plays the matching NIRUX feedback
/// sound: a tick whose pitch follows the new level, a softer bump when the key
/// cannot go further (0 % or 100 %), and a two-note mute/unmute. NIRUNOTIFY
/// decides whether a sound is on and limits a held key, so this script never
/// starts a sound player itself. Without NIRUNOTIFY, `notify-send` reaches
/// whatever daemon runs (or is silent), and one bubble is replaced by the next
/// through a stable id per operation.
///
/// Raising stops at 100 %; a sink that is already above it (set elsewhere)
/// comes back to 100 %. wpctl versions without `-l` raise without a limit.
/// The mute sound plays just before the sink is muted, so it can be heard.
const VOLUME_SCRIPT: &str = r#"op="$1"
sink=@DEFAULT_AUDIO_SINK@
level() { awk '{printf "%d", $2 * 100 + 0.5}'; }
before=$(wpctl get-volume "$sink" 2>/dev/null || :)
was=$(printf '%s\n' "$before" | level)
have_osd=0
command -v nirunotify >/dev/null 2>&1 && have_osd=1
case "$op" in
  up)   wpctl set-mute "$sink" 0 2>/dev/null
        wpctl set-volume -l 1.0 "$sink" 5%+ 2>/dev/null || wpctl set-volume "$sink" 5%+ 2>/dev/null ||
          amixer -q set Master 5%+ unmute 2>/dev/null ;;
  down) wpctl set-volume "$sink" 5%- 2>/dev/null || amixer -q set Master 5%- 2>/dev/null ;;
  mute)
    case "$before" in
      *MUTED*) : ;;
      *) if [ "$have_osd" = 1 ]; then
           nirunotify --sound mute "${was:-0}" unmuted >/dev/null 2>&1 && sleep 0.18 2>/dev/null
         fi ;;
    esac
    wpctl set-mute "$sink" toggle 2>/dev/null || amixer -q set Master toggle 2>/dev/null ;;
esac
after=$(wpctl get-volume "$sink" 2>/dev/null || :)
v=$(printf '%s\n' "$after" | level)
[ -n "$v" ] || v=$(amixer get Master 2>/dev/null | sed -n 's/.*\[\([0-9]*\)%\].*/\1/p' | head -1)
sound=
case "$after" in
  *MUTED*) body="Muted" ;;
  *) body="${v}%"
     case "$op" in
       up) if [ "${was:-0}" -ge 100 ] 2>/dev/null && [ "${v:-0}" -ge 100 ] 2>/dev/null; then sound=volume-limit; else sound=volume-up; fi ;;
       down) if [ "${was:-1}" -le 0 ] 2>/dev/null && [ "${v:-1}" -le 0 ] 2>/dev/null; then sound=volume-limit; else sound=volume-down; fi ;;
       mute) sound=unmute ;;
     esac ;;
esac
# Prefer the suite OSD; a missing daemon falls back to notify-send.
if [ "$have_osd" = 1 ]; then
  if [ -n "$sound" ]; then
    nirunotify --osd volume "$body" --sound "$sound" "${v:-0}" unmuted >/dev/null 2>&1 && exit 0
  else
    nirunotify --osd volume "$body" >/dev/null 2>&1 && exit 0
  fi
fi
# A stable id per operation, so repeated presses replace the same bubble.
case "$op" in up) id=9101 ;; down) id=9102 ;; *) id=9103 ;; esac
notify-send -a NIRU -r "$id" -h int:value:"${v:-0}" -t 1200 "Volume" "$body" 2>/dev/null
exit 0"#;

/// Look up a program in PATH without spawning anything.
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
pub use crate::bindings::keysym_named;

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

    /// Run the volume script against a fake wpctl (a sink in a file) and a
    /// fake nirunotify that records its arguments.
    fn volume_key(op: &str, volume: &str, muted: bool) -> (String, Vec<String>) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "niruwm-volume-{}-{op}-{volume}-{muted}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sink = dir.join("sink");
        std::fs::write(&sink, format!("{volume} {}\n", if muted { 1 } else { 0 })).unwrap();
        let wpctl = r#"#!/bin/sh
read v m < "$SINK"
case "$1" in
  get-volume) if [ "$m" = 1 ]; then echo "Volume: $v [MUTED]"; else echo "Volume: $v"; fi ;;
  set-mute) case "$3" in toggle) m=$((1 - m)) ;; *) m=$3 ;; esac ;;
  set-volume)
    [ "$2" = -l ] && { limit=$3; shift 2; }
    v=$(awk -v v="$v" -v s="$3" -v l="${limit:-9}" 'BEGIN {
      d = substr(s, 1, length(s) - 2) / 100; if (s ~ /-$/) d = -d
      v += d; if (v < 0) v = 0; if (v > l) v = l; printf "%.2f", v }') ;;
esac
echo "$v $m" > "$SINK"
"#;
        let notify = "#!/bin/sh\necho \"$*\" >> \"$CALLS\"\n";
        for (name, body) in [("wpctl", wpctl), ("nirunotify", notify)] {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let status = std::process::Command::new("sh")
            .args(["-c", VOLUME_SCRIPT, "sh", op])
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .env("SINK", &sink)
            .env("CALLS", dir.join("calls"))
            .status()
            .unwrap();
        assert!(status.success());
        let state = std::fs::read_to_string(&sink).unwrap().trim().to_string();
        let calls = std::fs::read_to_string(dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        (state, calls)
    }

    #[test]
    fn volume_keys_show_the_level_and_play_the_matching_sound() {
        let (state, calls) = volume_key("up", "0.40", false);
        assert_eq!(state, "0.45 0");
        assert_eq!(calls, ["--osd volume 45% --sound volume-up 45 unmuted"]);
        let (_, calls) = volume_key("down", "0.40", false);
        assert_eq!(calls, ["--osd volume 35% --sound volume-down 35 unmuted"]);
    }

    #[test]
    fn volume_keys_bump_at_the_ends() {
        let (state, calls) = volume_key("up", "0.98", false);
        assert_eq!(state, "1.00 0", "raising stops at 100 %");
        assert_eq!(calls, ["--osd volume 100% --sound volume-up 100 unmuted"]);
        let (state, calls) = volume_key("up", "1.00", false);
        assert_eq!(state, "1.00 0");
        assert_eq!(
            calls,
            ["--osd volume 100% --sound volume-limit 100 unmuted"]
        );
        let (_, calls) = volume_key("down", "0.00", false);
        assert_eq!(calls, ["--osd volume 0% --sound volume-limit 0 unmuted"]);
    }

    #[test]
    fn mute_sounds_before_muting_and_unmute_after() {
        let (state, calls) = volume_key("mute", "0.50", false);
        assert_eq!(state, "0.50 1");
        assert_eq!(calls, ["--sound mute 50 unmuted", "--osd volume Muted"]);
        let (state, calls) = volume_key("mute", "0.50", true);
        assert_eq!(state, "0.50 0");
        assert_eq!(calls, ["--osd volume 50% --sound unmute 50 unmuted"]);
        // Lowering a muted sink changes the level silently.
        let (state, calls) = volume_key("down", "0.50", true);
        assert_eq!(state, "0.45 1");
        assert_eq!(calls, ["--osd volume Muted"]);
        // Raising unmutes, so the tick is heard.
        let (state, calls) = volume_key("up", "0.50", true);
        assert_eq!(state, "0.55 0");
        assert_eq!(calls, ["--osd volume 55% --sound volume-up 55 unmuted"]);
    }
}
