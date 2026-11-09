use super::*;
use crate::bindings::{Action, LayoutId};
use std::os::fd::AsRawFd;

/// How often the wallpaper timer is checked while idle.
const ROTATION_TICK: i32 = 1000;

/// Block until `fd` is readable or the timeout expires. Returns true on data.
fn wait_readable(fd: std::os::fd::RawFd, timeout_ms: i32) -> bool {
    let mut pfd = libc_pollfd {
        fd,
        events: 1, // POLLIN
        revents: 0,
    };
    // SAFETY: one valid pollfd for the duration of the call.
    let n = unsafe { poll(&mut pfd, 1, timeout_ms) };
    n > 0
}

/// Minimal `poll(2)` binding: adding the `libc` crate for one call is not
/// worth it, and this is the only place the WM needs a syscall.
#[repr(C)]
struct libc_pollfd {
    fd: std::os::fd::RawFd,
    events: i16,
    revents: i16,
}

extern "C" {
    fn poll(fds: *mut libc_pollfd, nfds: u64, timeout: i32) -> i32;
}

impl Wm {
    /// Once a minute: keep the session log under its size cap.
    fn cap_session_log(&mut self) {
        let now = std::time::Instant::now();
        if self
            .log_checked
            .is_some_and(|at| now.duration_since(at).as_secs() < 60)
        {
            return;
        }
        self.log_checked = Some(now);
        let log = crate::logcap::session_log();
        if !crate::logcap::stderr_is(&log) {
            return;
        }
        match crate::logcap::rotate_in_place(&log, crate::logcap::MAX_BYTES, crate::logcap::KEEP) {
            Ok(true) => eprintln!(
                "niruwm: session log rotated (over {} MB; older lines in session.log.1)",
                crate::logcap::MAX_BYTES / 1024 / 1024
            ),
            Ok(false) => {}
            Err(e) => eprintln!("niruwm: could not rotate the session log: {e}"),
        }
    }

    pub(crate) fn run(&mut self) -> Result<()> {
        loop {
            self.reap_children();
            // Drain everything already queued first: a burst of events (a tag
            // switch unmap/map sequence) must be handled before sleeping.
            while let Some(event) = self.c.poll_for_event()? {
                // A bug in one event handler must not end the session. The
                // panic message is in the session log; the window manager
                // carries on. Many in a row mean the state is broken: leave
                // with an error, and niruwm-session starts a fresh manager
                // that keeps the open windows.
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.handle(event)));
                match outcome {
                    Ok(keep_going) => {
                        if !keep_going? {
                            return Ok(());
                        }
                    }
                    Err(_) => {
                        let now = std::time::Instant::now();
                        self.panics
                            .retain(|at| now.duration_since(*at).as_secs() < 60);
                        self.panics.push(now);
                        eprintln!(
                            "niruwm: internal error while handling an event ({} in the last minute); continuing",
                            self.panics.len()
                        );
                        if self.panics.len() >= 5 {
                            return Err("five internal errors within a minute".into());
                        }
                    }
                }
            }
            self.c.flush()?;

            // Nothing queued. Sleep until X has something for us, but never
            // longer than the wallpaper check interval, so rotation happens
            // without a second thread. `poll(2)` blocks in the kernel; this
            // does not spin.
            wait_readable(self.c.stream().as_raw_fd(), ROTATION_TICK);
            if self.wallpaper.tick() {
                eprintln!("niruwm: wallpaper changed");
            }
            self.cap_session_log();
            // The same tick notices a bar that died. It is cheap: one
            // try_wait per child, once a second, and nothing else.
            self.restart_bars();
            // A restored program may open fewer windows than it did when the
            // session was saved. Stop waiting after the deadline.
            self.finish_restore_if_due()?;
            self.studio_tick();
        }
    }

    /// Handle one event. Returns `false` when the WM should exit.
    pub(crate) fn handle(&mut self, event: Event) -> Result<bool> {
        let outcome: Result<bool> = (|| {
            match event {
                Event::MapRequest(e) => self.manage(e.window)?,
                Event::DestroyNotify(e) => {
                    // Helpers create and destroy small unmanaged windows all
                    // the time. Only a window the manager knew about changes
                    // the layout; re-arranging for the others restacked
                    // floating windows and scratchpads over open bar panels.
                    let known = self.clients.iter().any(|c| c.win == e.window)
                        || self.docks.contains(&e.window)
                        || self.withdrawn.contains(&e.window)
                        || self.hidden.contains(&e.window);
                    self.withdrawn.remove(&e.window);
                    self.unmanage(e.window);
                    if known {
                        self.arrange()?;
                    }
                }
                Event::UnmapNotify(e) => {
                    // Only a window this WM does not manage, or is not
                    // currently hiding, can withdraw itself.
                    //
                    // Our own tag hiding produces an UnmapNotify as well, and
                    // X does not guarantee one event per UnmapWindow request:
                    // a long burst of workspace switches delivers far fewer
                    // notifications than requests (measured: 78 unmap requests,
                    // 25 notifications). Any bookkeeping that counts events
                    // therefore drifts, and once it drifts the next notification
                    // is read as a client withdrawal and the live client is
                    // dropped from _NET_CLIENT_LIST while its process keeps
                    // running.
                    //
                    // A managed client that is visible must not be unmapped by
                    // its client without us hearing a MapRequest first, so a
                    // notification for such a window is discarded. A window
                    // that is gone from the client list is unknown and is
                    // dropped as before.
                    if self.hidden.contains(&e.window) {
                        // expected: we unmapped it
                    } else if self.clients.iter().any(|c| c.win == e.window) {
                        // A visible managed client that is now unmapped withdrew
                        // itself (a tray hide or a minimized main window). Record
                        // that fact; a stale notification from our own hide has
                        // already been remapped and is still VIEWABLE.
                        match self
                            .c
                            .get_window_attributes(e.window)
                            .ok()
                            .and_then(|cookie| cookie.reply().ok())
                        {
                            Some(attributes) if attributes.map_state == MapState::UNMAPPED => {
                                self.withdrawn.insert(e.window);
                            }
                            Some(_) => {}
                            None => {
                                // Destroyed between the UnmapNotify and the
                                // reply. Remove it now; DestroyNotify may
                                // already have been consumed by the race.
                                self.unmanage(e.window);
                                self.arrange()?;
                            }
                        }
                    } else {
                        self.unmanage(e.window);
                        self.arrange()?;
                    }
                }
                Event::ConfigureRequest(e) => self.configure_request(e)?,
                // A window asks for attention, or stops asking.
                Event::PropertyNotify(e) if e.atom == u32::from(AtomEnum::WM_HINTS) => {
                    self.note_attention(e.window)?;
                }
                // Moving between two empty desktops stays inside the root
                // window, so no EnterNotify arrives at the boundary. Follow
                // root-level motion as well, otherwise keys keep going to the
                // screen the pointer left.
                Event::MotionNotify(e) if e.event == self.root => {
                    self.focus_screen_at(e.root_x, e.root_y)?;
                }
                // Pointer enters empty desktop of a screen: focus that screen.
                Event::EnterNotify(e) if e.event == self.root => {
                    let (px, py) = self.pointer_position_now();
                    self.focus_screen_at(px, py)?;
                }
                Event::EnterNotify(e) => {
                    // Pointer enters a window: focus it and the screen it is on.
                    if let Some(c) = self.clients.iter().find(|c| c.win == e.event) {
                        let (monitor, tag) = (c.monitor, c.tag);
                        // Moving onto another screen makes that screen the
                        // focused one. Without this the keyboard stayed on the
                        // screen the pointer left, which is the opposite of
                        // focus-follows-mouse.
                        // Showing or hiding a scratchpad uncovers or covers
                        // the window under the pointer. That crossing is the
                        // window manager's doing, not the user's, and must not
                        // take the focus from the window it is given back to.
                        let ours = self
                            .scratch_enter_quiet
                            .is_some_and(|until| std::time::Instant::now() < until);
                        // After a focus loop was noticed the pointer stops
                        // moving the focus for a moment, which breaks it.
                        let paused = self
                            .pointer_focus_paused
                            .is_some_and(|until| std::time::Instant::now() < until);
                        if ours || paused {
                            return Ok(true);
                        }
                        if monitor != self.focused_monitor {
                            self.focused_monitor = monitor;
                        }
                        if tag == self.active_tag() {
                            let target = self.modal_for(e.event).unwrap_or(e.event);
                            self.set_focus(target)?;
                        }
                    }
                }
                Event::ButtonPress(e) => {
                    if !self.mouse_button(e)? {
                        return Ok(false);
                    }
                }
                Event::KeyPress(e) => return self.key(e),
                Event::MappingNotify(_) => {
                    self.resolve_bindings()?;
                    self.grab()?;
                }
                Event::ConfigureNotify(e) if e.window == self.root => {
                    let g = self.c.get_geometry(self.root)?.reply()?;
                    self.screen_w = g.width;
                    self.screen_h = g.height;
                    self.arrange()?;
                }
                // A cable was plugged in or out, or a resolution changed.
                // The root geometry is the authoritative new size; refresh
                // would otherwise rebuild the monitors from the old width.
                Event::RandrScreenChangeNotify(_) => {
                    let g = self.c.get_geometry(self.root)?.reply()?;
                    self.screen_w = g.width;
                    self.screen_h = g.height;
                    self.refresh_monitors()?;
                }
                Event::ClientMessage(e) => {
                    if !self.client_message(e)? {
                        return Ok(false);
                    }
                }
                Event::Error(e) => match self.repair_x11_error(&e) {
                    true => {}
                    false => eprintln!("niruwm: X11 error: {e:?}"),
                },
                _ => (),
            }
            if std::mem::take(&mut self.scratch_hide_pending) {
                self.hide_scratch_after_focus_loss()?;
            }
            self.c.flush()?;
            Ok(true)
        })();
        match outcome {
            Ok(keep_going) => Ok(keep_going),
            Err(e) => {
                eprintln!("niruwm: {e}");
                Ok(true)
            }
        }
    }

    pub(super) fn configure_request(&mut self, e: ConfigureRequestEvent) -> Result<()> {
        let Some(c) = self.clients.iter().find(|c| c.win == e.window) else {
            // Unmanaged (override-redirect or a panel): honour it as-is.
            self.c
                .configure_window(e.window, &ConfigureWindowAux::from_configure_request(&e))?;
            return Ok(());
        };
        if self.dragging == Some(e.window) {
            // The WM owns the geometry for the duration of a Super+drag. A
            // toolkit's own ConfigureRequest must not overwrite the rectangle
            // the pointer is currently moving.
            return Ok(());
        }
        if c.floating {
            let hints = c.size_hints;
            let border = self.cfg.border_width;
            self.c
                .configure_window(e.window, &ConfigureWindowAux::from_configure_request(&e))?;
            let g = self.c.get_geometry(e.window)?.reply()?;
            let geom = hints.clamp(Rect {
                x: g.x,
                y: g.y,
                w: g.width.saturating_add(border * 2),
                h: g.height.saturating_add(border * 2),
            });
            if geom.w != g.width.saturating_add(border * 2)
                || geom.h != g.height.saturating_add(border * 2)
            {
                self.c.configure_window(
                    e.window,
                    &ConfigureWindowAux::new()
                        .x(i32::from(geom.x))
                        .y(i32::from(geom.y))
                        .width(u32::from(geom.w.saturating_sub(border * 2)))
                        .height(u32::from(geom.h.saturating_sub(border * 2))),
                )?;
            }
            if let Some(c) = self.clients.iter_mut().find(|c| c.win == e.window) {
                c.geom = geom;
            }
        } else {
            // Tiled clients are told the real geometry so they do not
            // redraw at a size they never got.
            let g = self.c.get_geometry(e.window)?.reply()?;
            self.c.send_event(
                false,
                e.window,
                EventMask::STRUCTURE_NOTIFY,
                ConfigureNotifyEvent {
                    response_type: CONFIGURE_NOTIFY_EVENT,
                    sequence: 0,
                    event: e.window,
                    window: e.window,
                    above_sibling: 0,
                    x: g.x,
                    y: g.y,
                    width: g.width,
                    height: g.height,
                    border_width: g.border_width,
                    override_redirect: false,
                },
            )?;
        }
        Ok(())
    }

    /// Returns `false` when the WM should exit.
    fn client_message(&mut self, e: ClientMessageEvent) -> Result<bool> {
        let d = e.data.as_data32();
        if e.type_ == self.atom("_NET_WM_DESKTOP") {
            self.move_window_to_global(e.window, d[0])?;
            return Ok(true);
        }
        if e.type_ == self.atom("_NIRUWM_COMMAND") {
            if d[0] == 8 {
                self.start_bar();
                self.c.change_property32(
                    PropMode::REPLACE,
                    self.root,
                    self.atom("_NIRUWM_BAR_RESTART_SERIAL"),
                    AtomEnum::CARDINAL,
                    &[d[1]],
                )?;
                self.c.flush()?;
                return Ok(true);
            }
            // The control channel speaks the same actions as the key table,
            // so `niructl` and a binding can never drift apart.
            // The protocol carries zero-based indices (as EWMH does), while
            // the binding table uses the numbers a user types. Convert here
            // so both paths land on the same workspace.
            let mut reload_ack = None;
            let mut layout_ack = None;
            let action = match d[0] {
                // The control channel carries zero-based *global* desktops.
                1 => {
                    self.view_global(d[1])?;
                    None
                }
                2 => {
                    self.move_to_global(d[1])?;
                    None
                }
                9 => {
                    self.view_global(d[1])?;
                    None
                }
                // Local workspace commands from `niructl tag`/`movetotag`.
                // Action 1 and 9 stay global for autostart and NIRUBAR, but a
                // user-facing `tag N` must mean workspace N on the focused
                // screen, exactly like Super+N.
                13 => {
                    self.view_tag(d[1])?;
                    None
                }
                14 => {
                    self.move_to_tag(d[1], false)?;
                    None
                }
                3 => Some(Action::SetLayout(
                    [
                        LayoutId::Tile,
                        LayoutId::Monocle,
                        LayoutId::Left,
                        LayoutId::Centered,
                        LayoutId::Fibonacci,
                        LayoutId::Floating,
                        LayoutId::CenteredFloat,
                        LayoutId::Scroll,
                    ]
                    .get(d[1] as usize)
                    .copied()
                    .unwrap_or(LayoutId::Tile),
                )),
                11 => {
                    if d[1] == 0 {
                        Some(Action::ToggleScrollColumns)
                    } else {
                        crate::layout::ScrollColumns::from_count(d[1]).map(Action::SetScrollColumns)
                    }
                }
                4 => {
                    // d[1] is the serial `niructl` is waiting for. A key
                    // binding sends zero and needs no acknowledgement.
                    let applied = self.reload()?;
                    if d[1] != 0 {
                        reload_ack = Some((d[1], applied));
                    }
                    None
                }
                // Scratchpads, for `niructl scratchpad` and NIRUBAR:
                // 15 show/hide slot d[1] (0 default, 1-9) with d[2] 0 toggle,
                // 1 show, 2 hide; 16 send the focused window to slot d[1];
                // 17 make the focused scratchpad a normal window again;
                // 18 send window d[2] to slot d[1] (whatever has the focus).
                15 => {
                    if let Ok(slot) = u8::try_from(d[1]) {
                        self.scratchpad(slot, super::ScratchOp::from_code(d[2]))?;
                    }
                    None
                }
                16 => u8::try_from(d[1])
                    .ok()
                    .filter(|n| (1..=9).contains(n))
                    .map(Action::ScratchpadSend),
                17 => Some(Action::Unscratch),
                // 20: go to the window that asked for attention most recently.
                20 => Some(Action::FocusUrgent),
                // 21: window swallowing, d[1] 0 toggle, 1 on, 2 off.
                21 => {
                    let on = match d[1] {
                        1 => true,
                        2 => false,
                        _ => !self.swallow_enabled,
                    };
                    self.set_swallow(on);
                    eprintln!(
                        "niruwm: window swallowing {}",
                        if on { "on" } else { "off" }
                    );
                    None
                }
                // 19: run the binding on modifiers d[1] + keysym d[2].
                19 => self.bound_action(d[1], d[2]),
                18 => {
                    if let Ok(slot) = u8::try_from(d[1]) {
                        self.send_named_to_scratch(d[2], slot)?;
                    }
                    None
                }
                5 => {
                    eprintln!(
                        "niruwm: quit requested through _NIRUWM_COMMAND (niructl quit) by window {}; the session ends",
                        e.window
                    );
                    Some(Action::Quit)
                }
                6 => Some(Action::SaveSession),
                7 => Some(Action::ClearSession),
                12 => Some(match d[1] {
                    1 => Action::BarTop,
                    2 => Action::BarBottom,
                    3 => Action::BarHide,
                    4 => Action::BarShow,
                    5 => Action::BarToggle,
                    _ => Action::BarCycle,
                }),
                10 => {
                    // Write every workspace's layout to the config, then
                    // acknowledge so `niructl layout save` reports the truth.
                    // A serial of zero does not mean "no acknowledgement"
                    // here: this command has no key binding, so every call
                    // states one and waits for the answer.
                    let saved = self.remember_layouts();
                    layout_ack = Some((d[1], saved.is_ok()));
                    None
                }
                _ => None,
            };
            if let Some(action) = action {
                let keep_going = self.dispatch(action)?;
                if !keep_going {
                    return Ok(false);
                }
            }
            self.arrange()?;
            if let Some((serial, saved)) = layout_ack {
                self.c.change_property32(
                    PropMode::REPLACE,
                    self.root,
                    self.atom("_NIRUWM_LAYOUT_SAVE_ACK"),
                    AtomEnum::CARDINAL,
                    &[if saved { serial } else { serial | 0x8000_0000 }],
                )?;
                self.c.flush()?;
            }
            if let Some((serial, applied)) = reload_ack {
                // The high bit marks a rejected reload, so the caller gets a
                // completion signal either way and never treats "rejected" as
                // success after an arbitrary delay.
                let value = if applied {
                    serial
                } else {
                    serial | 0x8000_0000
                };
                self.prop(
                    self.root,
                    "_NIRUWM_CONFIG_ACK",
                    AtomEnum::CARDINAL.into(),
                    &[value],
                )?;
                self.c.flush()?;
            }
        } else if e.type_ == self.atom("_NIRUWM_LOCK") {
            // A lock screen has taken the keyboard and asks for the key grabs
            // to be released. d[0] = 1 means "locked", 0 means "unlocked".
            if d[0] == 1 {
                self.keys_released = true;
                // Every modifier, every key: a grab left behind here would be
                // answered by the window manager instead of the lock screen
                // after the unlock, which looks like a dead keyboard.
                for extra in [0u16, 8, 16, 24] {
                    self.c.ungrab_key(0, self.root, ModMask::from(extra))?;
                }
                // A Super+drag owns an active pointer grab. The locker cannot
                // ungrab another client's grab, so cancel the drag and release
                // ours before it retries the lock grab.
                if self.dragging.take().is_some() {
                    let _ = self.c.ungrab_pointer(x11rb::CURRENT_TIME);
                    eprintln!("niruwm: cancelled drag for lock takeover");
                }
            } else {
                self.keys_released = false;
                self.grab()?;
            }
            self.c.flush()?;
        } else if e.type_ == self.atom("_NET_CURRENT_DESKTOP") {
            // From an external pager: a zero-based global desktop.
            self.view_global(d[0])?;
        } else if e.type_ == self.atom("_NET_ACTIVE_WINDOW") {
            // NIRUWM honours every activation request (a pager, a window
            // switcher, a notification): it switches to the window's screen
            // and workspace, or shows its scratchpad. See `activate`.
            if self.clients.iter().any(|c| c.win == e.window) {
                self.activate(e.window)?;
            }
        } else if e.type_ == self.atom("_NET_WM_STATE") {
            self.handle_wm_state(e)?;
        }
        Ok(true)
    }

    fn handle_wm_state(&mut self, e: ClientMessageEvent) -> Result<()> {
        let d = e.data.as_data32();
        let attention = self.atom("_NET_WM_STATE_DEMANDS_ATTENTION");
        if (d[1] == attention || d[2] == attention)
            && self.clients.iter().any(|c| c.win == e.window)
        {
            // EWMH: the window manager keeps `_NET_WM_STATE`; the mark itself
            // follows from it (and from WM_HINTS) in `note_attention`.
            let existing = self.values(e.window, "_NET_WM_STATE");
            let has = existing.contains(&attention);
            let wanted = match d[0] {
                0 => false,
                1 => true,
                _ => !has,
            };
            if wanted != has {
                let state = next_state_atoms(&existing, attention, wanted);
                self.prop(e.window, "_NET_WM_STATE", AtomEnum::ATOM.into(), &state)?;
            }
            self.note_attention(e.window)?;
        }
        let is_fullscreen = d[1] == self.atom("_NET_WM_STATE_FULLSCREEN")
            || d[2] == self.atom("_NET_WM_STATE_FULLSCREEN");
        if !is_fullscreen {
            return Ok(());
        }
        let Some(c) = self.clients.iter_mut().find(|c| c.win == e.window) else {
            return Ok(());
        };
        let fullscreen = match d[0] {
            0 => false,
            1 => true,
            _ => !c.fullscreen,
        };
        c.fullscreen = fullscreen;
        let full = self.atom("_NET_WM_STATE_FULLSCREEN");
        let existing = self.values(e.window, "_NET_WM_STATE");
        let state = next_state_atoms(&existing, full, fullscreen);
        self.prop(e.window, "_NET_WM_STATE", AtomEnum::ATOM.into(), &state)?;
        self.arrange()
    }
}

/// Add or remove one state atom (fullscreen, demands-attention) without
/// erasing unrelated EWMH state atoms.
pub(super) fn next_state_atoms(existing: &[u32], atom: u32, enabled: bool) -> Vec<u32> {
    let mut state: Vec<u32> = existing.iter().copied().filter(|a| *a != atom).collect();
    if enabled {
        state.push(atom);
    }
    state
}

#[cfg(test)]
mod state_tests {
    use super::next_state_atoms;

    #[test]
    fn fullscreen_transition_preserves_unrelated_state_atoms() {
        let states = next_state_atoms(&[7, 11, 13], 11, true);
        assert_eq!(states, vec![7, 13, 11]);
        assert_eq!(next_state_atoms(&states, 11, false), vec![7, 13]);
        assert_eq!(next_state_atoms(&states, 11, true), vec![7, 13, 11]);
    }
}
