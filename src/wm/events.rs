use super::*;
use crate::bindings::{Action, LayoutId};

impl Wm {
    pub(crate) fn run(&mut self) -> Result<()> {
        loop {
            let event = self.c.wait_for_event()?;
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
                    Event::EnterNotify(e) => {
                        if self
                            .clients
                            .iter()
                            .any(|c| c.win == e.event && c.tag == self.tag)
                        {
                            self.set_focus(e.event)?;
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
                    Event::PropertyNotify(e) if e.window == self.root => {
                        if e.atom == self.atom("_NIRUWM_QUIT") {
                            return Ok(false);
                        }
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
                Ok(false) => break,
                Ok(true) => (),
                Err(e) => eprintln!("niruwm: {e}"),
            }
        }
        Ok(())
    }

    fn configure_request(&mut self, e: ConfigureRequestEvent) -> Result<()> {
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
            // The control channel speaks the same actions as the key table,
            // so `niructl` and a binding can never drift apart.
            // The protocol carries zero-based indices (as EWMH does), while
            // the binding table uses the numbers a user types. Convert here
            // so both paths land on the same workspace.
            let action = match d[0] {
                1 if d[1] < 9 => Some(Action::FocusTag(d[1] + 1)),
                2 if d[1] < 9 => Some(Action::MoveToTag(d[1] + 1)),
                3 => Some(Action::SetLayout(match d[1] {
                    1 => LayoutId::Monocle,
                    2 => LayoutId::Left,
                    3 => LayoutId::Centered,
                    4 => LayoutId::Fibonacci,
                    5 => LayoutId::Floating,
                    _ => LayoutId::Tile,
                })),
                4 => Some(Action::Reload),
                5 => Some(Action::Quit),
                _ => None,
            };
            if let Some(action) = action {
                if !self.dispatch(action)? {
                    return Ok(false);
                }
            }
            self.arrange()?;
        } else if e.type_ == self.atom("_NET_CURRENT_DESKTOP") && d[0] < 9 {
            // From NIRUBAR's clickable workspaces or an external pager.
            self.view_tag(d[0])?;
        } else if e.type_ == self.atom("_NET_ACTIVE_WINDOW") {
            if d[0] < 9 {
                if let Some(c) = self.clients.iter().find(|c| c.win == e.window) {
                    let tag = c.tag;
                    self.view_tag(tag)?;
                    self.set_focus(e.window)?;
                }
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
        let state: &[u32] = if fullscreen { &[full] } else { &[] };
        self.prop(e.window, "_NET_WM_STATE", AtomEnum::ATOM.into(), state)?;
        self.arrange()
    }
}
