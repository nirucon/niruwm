use super::*;
impl Wm {
    pub(super) fn key(&mut self, e: KeyPressEvent) -> Result<bool> {
        let map = self.c.get_keyboard_mapping(e.detail, 1)?.reply()?;
        let s = map.keysyms[0];
        let shift = e.state.contains(KeyButMask::SHIFT);
        let ctrl = e.state.contains(KeyButMask::CONTROL);
        match s {
            0x31..=0x39 => {
                let tag = s - 0x31;
                if shift {
                    if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == self.focus) {
                        c.tag = tag;
                        let w = c.win;
                        self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[tag])?;
                    }
                } else {
                    self.tag = tag;
                }
            }
            0xff0d => Self::spawn(&self.cfg.terminal),
            0x70 => Self::spawn(&self.cfg.launcher),
            0x71 if shift && ctrl => return Ok(false),
            0x71 => {
                if let Some(w) = self.focus {
                    if self
                        .values(w, "WM_PROTOCOLS")
                        .contains(&self.atom("WM_DELETE_WINDOW"))
                    {
                        self.c.send_event(
                            false,
                            w,
                            EventMask::NO_EVENT,
                            ClientMessageEvent::new(
                                32,
                                w,
                                self.atom("WM_PROTOCOLS"),
                                [self.atom("WM_DELETE_WINDOW"), x11rb::CURRENT_TIME, 0, 0, 0],
                            ),
                        )?;
                    } else {
                        self.c.kill_client(w)?;
                    }
                }
            }
            0x6a | 0x6b | 0xff09 => {
                let v = self
                    .clients
                    .iter()
                    .filter(|c| c.tag == self.tag)
                    .map(|c| c.win)
                    .collect::<Vec<_>>();
                if !v.is_empty() {
                    let i = v.iter().position(|w| Some(*w) == self.focus).unwrap_or(0);
                    let j = if s == 0x6b {
                        (i + v.len() - 1) % v.len()
                    } else {
                        (i + 1) % v.len()
                    };
                    self.set_focus(v[j])?;
                    self.c.configure_window(
                        v[j],
                        &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                    )?;
                }
            }
            0x6d => self.layout = Layout::Monocle,
            0x74 => self.layout = Layout::Tile,
            0x79 => self.layout = Layout::Left,
            0x63 => self.layout = Layout::Centered,
            0x66 => self.layout = Layout::Floating,
            0x65 if ctrl => self.layout = Layout::Fibonacci,
            0x7a if shift => {
                if let Some(i) = self.clients.iter().position(|c| Some(c.win) == self.focus) {
                    if let Some(master) = self
                        .clients
                        .iter()
                        .position(|c| c.tag == self.tag && !c.floating)
                    {
                        self.clients.swap(i, master);
                    }
                }
            }
            0x68 => self.cfg.master_ratio = (self.cfg.master_ratio - 0.05).max(0.1),
            0x6c => self.cfg.master_ratio = (self.cfg.master_ratio + 0.05).min(0.9),
            0x20 if shift => {
                if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == self.focus) {
                    c.floating = !c.floating;
                }
            }
            0x65 if shift => {
                if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == self.focus) {
                    c.fullscreen = !c.fullscreen;
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
                }
            }
            0x72 => match Config::load().and_then(|c| Theme::load(&c.theme).map(|t| (c, t))) {
                Ok((c, t)) => {
                    self.cfg = c;
                    self.theme = t;
                    self.grab()?;
                    if let Some(w) = self.focus {
                        self.set_focus(w)?;
                    }
                }
                Err(e) => eprintln!("Reload rejected: {e}"),
            },
            _ => (),
        }
        self.arrange()?;
        Ok(true)
    }
}
