use super::*;
impl Wm {
    pub(crate) fn run(&mut self) -> Result<()> {
        loop {
            let e = self.c.wait_for_event()?;
            let result: Result<bool> = (|| {
                match e {
                    Event::MapRequest(e) => self.manage(e.window)?,
                    Event::DestroyNotify(e) => {
                        self.clients.retain(|c| c.win != e.window);
                        self.hidden.remove(&e.window);
                        self.pending_unmaps.remove(&e.window);
                        self.arrange()?;
                    }
                    Event::UnmapNotify(e) => {
                        if let Some(count) = self.pending_unmaps.get_mut(&e.window) {
                            *count -= 1;
                            if *count == 0 {
                                self.pending_unmaps.remove(&e.window);
                            }
                        } else {
                            self.clients.retain(|c| c.win != e.window);
                            self.hidden.remove(&e.window);
                            self.arrange()?;
                        }
                    }
                    Event::ConfigureRequest(e) => {
                        if let Some(c) = self.clients.iter_mut().find(|c| c.win == e.window) {
                            if c.floating {
                                let aux = ConfigureWindowAux::from_configure_request(&e);
                                self.c.configure_window(e.window, &aux)?;
                                let g = self.c.get_geometry(e.window)?.reply()?;
                                c.geom = Rect {
                                    x: g.x,
                                    y: g.y,
                                    w: g.width,
                                    h: g.height,
                                };
                            } else {
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
                        } else {
                            self.c.configure_window(
                                e.window,
                                &ConfigureWindowAux::from_configure_request(&e),
                            )?;
                        }
                    }
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
                    Event::MappingNotify(_) => self.grab()?,
                    Event::ConfigureNotify(e) if e.window == self.root => self.arrange()?,
                    Event::ClientMessage(e) => {
                        let d = e.data.as_data32();
                        if e.type_ == self.atom("_NIRUWM_COMMAND") {
                            match d[0] {
                                1 if d[1] < 9 => self.tag = d[1],
                                2 => {
                                    self.layout = match d[1] {
                                        1 => Layout::Monocle,
                                        2 => Layout::Left,
                                        3 => Layout::Centered,
                                        4 => Layout::Fibonacci,
                                        5 => Layout::Floating,
                                        _ => Layout::Tile,
                                    }
                                }
                                3 => match Config::load()
                                    .and_then(|c| Theme::load(&c.theme).map(|t| (c, t)))
                                {
                                    Ok((c, t)) => {
                                        self.cfg = c;
                                        self.theme = t;
                                        if let Some(w) = self.focus {
                                            self.set_focus(w)?;
                                        }
                                    }
                                    Err(e) => eprintln!("Reload rejected: {e}"),
                                },
                                4 => return Ok(false),
                                _ => (),
                            }
                            self.arrange()?;
                        } else if e.type_ == self.atom("_NET_CURRENT_DESKTOP") && d[0] < 9 {
                            self.tag = d[0];
                            self.arrange()?;
                        } else if e.type_ == self.atom("_NET_ACTIVE_WINDOW") {
                            if let Some(c) = self.clients.iter().find(|c| c.win == e.window) {
                                self.tag = c.tag;
                                self.arrange()?;
                                self.set_focus(e.window)?;
                            }
                        } else if e.type_ == self.atom("_NET_WM_STATE")
                            && (d[1] == self.atom("_NET_WM_STATE_FULLSCREEN")
                                || d[2] == self.atom("_NET_WM_STATE_FULLSCREEN"))
                        {
                            if let Some(c) = self.clients.iter_mut().find(|c| c.win == e.window) {
                                c.fullscreen = match d[0] {
                                    0 => false,
                                    1 => true,
                                    2 => !c.fullscreen,
                                    _ => c.fullscreen,
                                };
                                let (w, f) = (c.win, c.fullscreen);
                                self.prop(
                                    w,
                                    "_NET_WM_STATE",
                                    AtomEnum::ATOM.into(),
                                    &if f {
                                        vec![self.atom("_NET_WM_STATE_FULLSCREEN")]
                                    } else {
                                        vec![]
                                    },
                                )?;
                                self.arrange()?;
                            }
                        }
                    }
                    Event::Error(e) => eprintln!("X11: {e:?}"),
                    _ => (),
                }
                self.c.flush()?;
                Ok(true)
            })();
            match result {
                Ok(false) => break,
                Ok(true) => (),
                Err(e) => eprintln!("Event: {e}"),
            }
        }
        for c in &self.clients {
            self.c.map_window(c.win)?;
        }
        if let Some(b) = &mut self.bar {
            let _ = b.kill();
            let _ = b.wait();
        }
        self.c.flush()?;
        Ok(())
    }
}
