use super::*;

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
                        self.refresh_keymap()?;
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
            match d[0] {
                1 if d[1] < 9 => self.tag = d[1],
                2 => {
                    // movetotag: move the focused window to tag d[1] and
                    // follow it, like dwm's `tag`.
                    if let Some(w) = self.focus {
                        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                            c.tag = d[1];
                        }
                        self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[d[1]])?;
                        self.tag = d[1];
                    }
                }
                3 => {
                    self.layout = match d[1] {
                        1 => Layout::Monocle,
                        2 => Layout::Left,
                        3 => Layout::Centered,
                        4 => Layout::Fibonacci,
                        5 => Layout::Floating,
                        _ => Layout::Tile,
                    }
                }
                4 => {
                    if let Ok(c) = Config::load() {
                        if let Ok(t) = Theme::load(&c.theme) {
                            let old_height = self.cfg.bar_height;
                            self.cfg = c;
                            self.theme = t;
                            self.grab()?;
                            if self.cfg.bar_height != old_height {
                                if let Some(mut b) = self.bar.take() {
                                    let _ = b.kill();
                                    let _ = b.wait();
                                }
                                self.start_bar();
                            }
                        }
                    }
                }
                5 => return Ok(false),
                _ => (),
            }
            self.arrange()?;
        } else if e.type_ == self.atom("_NET_CURRENT_DESKTOP") && d[0] < 9 {
            // From NIRUBAR's clickable workspaces or an external pager.
            self.tag = d[0];
            self.arrange()?;
        } else if e.type_ == self.atom("_NET_ACTIVE_WINDOW") {
            if d[0] < 9 {
                if let Some(c) = self.clients.iter().find(|c| c.win == e.window) {
                    self.tag = c.tag;
                    self.arrange()?;
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
