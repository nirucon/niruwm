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
    pub(crate) fn run(&mut self) -> Result<()> {
        loop {
            self.reap_children();
            // Drain everything already queued first: a burst of events (a tag
            // switch unmap/map sequence) must be handled before sleeping.
            while let Some(event) = self.c.poll_for_event()? {
                if !self.handle(event)? {
                    return Ok(());
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
            // The same tick notices a bar that died. It is cheap: one
            // try_wait per child, once a second, and nothing else.
            self.restart_bars();
            // A restored program may open fewer windows than it did when the
            // session was saved. Stop waiting after the deadline.
            self.finish_restore_if_due()?;
        }
    }

    /// Handle one event. Returns `false` when the WM should exit.
    fn handle(&mut self, event: Event) -> Result<bool> {
        let outcome: Result<bool> = (|| {
            match event {
                Event::MapRequest(e) => self.manage(e.window)?,
                Event::DestroyNotify(e) => {
                    self.unmanage(e.window);
                    self.arrange()?;
                }
                Event::UnmapNotify(e) => {
                    // Ignore the UnmapNotify that our own tag hiding
                    // produces; a client that unmaps itself is withdrawn.
                    if let Some(count) = self.pending_unmaps.get_mut(&e.window) {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            self.pending_unmaps.remove(&e.window);
                        }
                    } else {
                        self.unmanage(e.window);
                        self.arrange()?;
                    }
                }
                Event::ConfigureRequest(e) => self.configure_request(e)?,
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
                        if monitor != self.focused_monitor {
                            self.focused_monitor = monitor;
                        }
                        if tag == self.active_tag() {
                            self.set_focus(e.event)?;
                        }
                    }
                }
                Event::ButtonPress(e) => self.mouse_button(e)?,
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
                Event::RandrScreenChangeNotify(_) => {
                    self.refresh_monitors()?;
                }
                Event::ClientMessage(e) => {
                    if !self.client_message(e)? {
                        return Ok(false);
                    }
                }
                Event::Error(e) => eprintln!("niruwm: X11 error: {e:?}"),
                _ => (),
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
        if c.floating {
            self.c
                .configure_window(e.window, &ConfigureWindowAux::from_configure_request(&e))?;
            let g = self.c.get_geometry(e.window)?.reply()?;
            let geom = Rect {
                x: g.x,
                y: g.y,
                w: g.width,
                h: g.height,
            };
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
                3 => Some(Action::SetLayout(match d[1] {
                    1 => LayoutId::Monocle,
                    2 => LayoutId::Left,
                    3 => LayoutId::Centered,
                    4 => LayoutId::Fibonacci,
                    5 => LayoutId::Floating,
                    6 => LayoutId::CenteredFloat,
                    _ => LayoutId::Tile,
                })),
                4 => {
                    // d[1] is the serial `niructl` is waiting for. A key
                    // binding sends zero and needs no acknowledgement.
                    let applied = self.reload()?;
                    if d[1] != 0 {
                        reload_ack = Some((d[1], applied));
                    }
                    None
                }
                5 => Some(Action::Quit),
                6 => Some(Action::SaveSession),
                7 => Some(Action::ClearSession),
                _ => None,
            };
            if let Some(action) = action {
                if !self.dispatch(action)? {
                    return Ok(false);
                }
            }
            self.arrange()?;
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
            } else {
                self.keys_released = false;
                self.grab()?;
            }
            self.c.flush()?;
        } else if e.type_ == self.atom("_NET_CURRENT_DESKTOP") {
            // From an external pager: a zero-based global desktop.
            self.view_global(d[0])?;
        } else if e.type_ == self.atom("_NET_ACTIVE_WINDOW") {
            if let Some(c) = self.clients.iter().find(|c| c.win == e.window) {
                let (monitor, tag) = (c.monitor, c.tag);
                self.focused_monitor = monitor;
                self.view_tag(tag)?;
                self.set_focus(e.window)?;
            }
        } else if e.type_ == self.atom("_NET_WM_STATE") {
            self.handle_wm_state(e)?;
        }
        Ok(true)
    }

    fn handle_wm_state(&mut self, e: ClientMessageEvent) -> Result<()> {
        let d = e.data.as_data32();
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
        // A bound local, not a `&[full]` inside the `if`: rustc 1.75, the
        // declared baseline, rejects the latter.
        let states: [u32; 1] = [full];
        let state: &[u32] = if fullscreen { &states } else { &[] };
        self.prop(e.window, "_NET_WM_STATE", AtomEnum::ATOM.into(), state)?;
        self.arrange()
    }
}
