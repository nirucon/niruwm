use super::*;

impl Wm {
    pub(crate) fn values(&self, w: Window, n: &str) -> Vec<u32> {
        self.c
            .get_property(false, w, self.atom(n), AtomEnum::ANY, 0, 64)
            .ok()
            .and_then(|r| r.reply().ok())
            .and_then(|r| r.value32().map(|v| v.collect()))
            .unwrap_or_default()
    }

    fn has_protocol(&self, w: Window, protocol: &str) -> bool {
        self.values(w, "WM_PROTOCOLS")
            .contains(&self.atom(protocol))
    }

    fn window_types(&self, w: Window) -> Vec<u32> {
        self.values(w, "_NET_WM_WINDOW_TYPE")
    }

    pub(crate) fn manage(&mut self, w: Window) -> Result<()> {
        if self.clients.iter().any(|c| c.win == w) {
            return Ok(());
        }
        let a = self.c.get_window_attributes(w)?.reply()?;
        // Override-redirect (menus, tooltips, splash screens) stay unmanaged.
        if a.override_redirect {
            return Ok(());
        }

        let types = self.window_types(w);
        let is_dock = types.contains(&self.atom("_NET_WM_WINDOW_TYPE_DOCK"));
        if is_dock {
            // Panels are mapped as found and never tiled. NIRUBAR reserves
            // space via its strut, which `work_area()` reads.
            self.c.map_window(w)?;
            self.c.flush()?;
            return self.arrange();
        }
        if types.contains(&self.atom("_NET_WM_STATE")) {
            // Malformed hint; treat as normal.
        }

        let g = self.c.get_geometry(w)?.reply()?;
        let transient = self
            .c
            .get_property(false, w, AtomEnum::WM_TRANSIENT_FOR, AtomEnum::WINDOW, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut v| v.next())
            .filter(|w| *w != 0);

        // Dialogs, utilities, splash screens and transients float above the
        // tile layout, like dwm's `isfloating` rules.
        let floating = transient.is_some()
            || types.iter().any(|t| {
                [
                    self.atom("_NET_WM_WINDOW_TYPE_DIALOG"),
                    self.atom("_NET_WM_WINDOW_TYPE_UTILITY"),
                    self.atom("_NET_WM_WINDOW_TYPE_SPLASH"),
                ]
                .contains(t)
            });

        // Follow the client's own desktop when it asks for one (session
        // restore, `wmctrl -i -t`, applications started with a tag override).
        let requested_tag = self.values(w, "_NET_WM_DESKTOP").first().copied();
        let tag = match requested_tag {
            Some(t) if t < 9 => t,
            _ => self.tag,
        };

        let border = self.cfg.border_width;
        // Border width is applied here once; `arrange()` only changes geometry.
        self.c.configure_window(
            w,
            &ConfigureWindowAux::new().border_width(u32::from(border)),
        )?;
        self.c.change_window_attributes(
            w,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::ENTER_WINDOW | EventMask::PROPERTY_CHANGE | EventMask::FOCUS_CHANGE,
            ),
        )?;
        self.c.change_save_set(SetMode::INSERT, w)?;

        let take_focus = self.has_protocol(w, "WM_TAKE_FOCUS");
        self.clients.push(Client {
            win: w,
            tag,
            floating,
            fullscreen: false,
            geom: Rect {
                x: g.x,
                y: g.y,
                w: g.width,
                h: g.height,
            },
            take_focus,
        });

        self.prop(w, "WM_STATE", self.atom("WM_STATE"), &[1, 0])?;
        self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[tag])?;
        // Keep floating windows centred if the client asked for a 1x1 size
        // (common for dialogs that size themselves after mapping).
        if floating && (g.width <= 1 || g.height <= 1) {
            let area = self.work_area();
            if let Some(c) = self.clients.last_mut() {
                c.geom = Rect {
                    x: area.x + (area.w as i16 - 480) / 2,
                    y: area.y + (area.h as i16 - 320) / 2,
                    w: 480,
                    h: 320,
                };
            }
        }
        self.c.map_window(w)?;
        if tag == self.tag {
            self.set_focus(w)?;
        }
        self.arrange()
    }

    pub(crate) fn unmanage(&mut self, w: Window) {
        if let Some(pos) = self.clients.iter().position(|c| c.win == w) {
            self.clients.remove(pos);
        }
        self.hidden.remove(&w);
        self.pending_unmaps.remove(&w);
        if self.focus == Some(w) {
            self.focus = None;
        }
    }

    /// Focus a client, with fallbacks that survive the common race where the
    /// window disappears between the event and the call.
    pub(crate) fn set_focus(&mut self, w: Window) -> Result<()> {
        if !self.clients.iter().any(|c| c.win == w) {
            return Ok(());
        }
        let take_focus = self
            .clients
            .iter()
            .find(|c| c.win == w)
            .map(|c| c.take_focus)
            .unwrap_or(false);
        // Never raise an unmanaged window; the WM owns stacking for tiles.
        self.c
            .configure_window(w, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))?;
        if take_focus {
            // ICCCM: the client asked for focus messages and may keep
            // input focus itself (e.g. Java, some input methods).
            self.c.send_event(
                false,
                w,
                EventMask::NO_EVENT,
                ClientMessageEvent::new(
                    32,
                    w,
                    self.atom("WM_PROTOCOLS"),
                    [self.atom("WM_TAKE_FOCUS"), x11rb::CURRENT_TIME, 0, 0, 0],
                ),
            )?;
        }
        // RevertToParent rather than PointerRoot: when the focused window is
        // destroyed X returns focus to its parent (the root) instead of
        // leaving it dangling, and NIRUWM repairs it in `arrange()`.
        self.c
            .set_input_focus(InputFocus::POINTER_ROOT, w, x11rb::CURRENT_TIME)?;
        self.focus = Some(w);
        self.prop(
            self.root,
            "_NET_ACTIVE_WINDOW",
            AtomEnum::WINDOW.into(),
            &[w],
        )?;
        self.update_borders()?;
        Ok(())
    }

    fn focus_client(&mut self, w: Option<Window>) -> Result<()> {
        match w {
            Some(w) => self.set_focus(w),
            None => {
                self.focus = None;
                self.c
                    .set_input_focus(InputFocus::POINTER_ROOT, self.root, x11rb::CURRENT_TIME)?;
                self.prop(
                    self.root,
                    "_NET_ACTIVE_WINDOW",
                    AtomEnum::WINDOW.into(),
                    &[],
                )?;
                self.update_borders()
            }
        }
    }

    pub(crate) fn update_borders(&self) -> Result<()> {
        if !self.cfg.bar.enabled {
            return Ok(());
        }
        for c in &self.clients {
            if c.fullscreen {
                continue;
            }
            let colour = if Some(c.win) == self.focus {
                Theme::pixel(&self.theme.focus)?
            } else {
                Theme::pixel(&self.theme.border)?
            };
            self.c.change_window_attributes(
                c.win,
                &ChangeWindowAttributesAux::new().border_pixel(colour),
            )?;
        }
        Ok(())
    }

    /// Area available to tiled clients: the screen minus the bar's strut.
    fn screen_area(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.screen_w,
            h: self.screen_h,
        }
    }

    pub(crate) fn work_area(&self) -> Rect {
        let mut top = 0u16;
        let children = match self
            .c
            .query_tree(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            Some(r) => r.children,
            None => return self.screen_area(),
        };
        for w in children {
            let attrs = match self.c.get_window_attributes(w) {
                Ok(cookie) => match cookie.reply() {
                    Ok(a) => a,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            if attrs.map_state != MapState::VIEWABLE {
                continue;
            }
            let strut = match self
                .c
                .get_property(
                    false,
                    w,
                    self.atom("_NET_WM_STRUT_PARTIAL"),
                    AtomEnum::CARDINAL,
                    0,
                    12,
                )
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| r.value32().map(|v| v.collect::<Vec<_>>()))
            {
                Some(v) if v.len() >= 12 => v,
                _ => continue,
            };
            top = top.max(strut[2].min(u32::from(self.screen_h)) as u16);
        }
        // Without a strut, reserve the configured bar height so windows never
        // hide behind the bar during the short window before it maps.
        if top == 0 && self.cfg.bar.enabled {
            top = self.cfg.bar_height;
        }
        Rect {
            x: 0,
            y: top as i16,
            w: self.screen_w,
            h: self.screen_h.saturating_sub(top),
        }
    }

    pub(crate) fn arrange(&mut self) -> Result<()> {
        let screen = self.work_area();
        let n = self
            .clients
            .iter()
            .filter(|c| c.tag == self.tag && !c.floating && !c.fullscreen)
            .count();
        let boxes = self.layout.arrange_master(
            screen,
            n,
            self.nmaster,
            self.cfg.master_ratio,
            self.cfg.gap,
        );

        let mut idx = 0;
        for c in &self.clients {
            if c.tag != self.tag {
                if self.hidden.insert(c.win) {
                    *self.pending_unmaps.entry(c.win).or_default() += 1;
                    self.c.unmap_window(c.win)?;
                }
                continue;
            }
            if self.hidden.remove(&c.win) {
                self.c.map_window(c.win)?;
            }
            let r = if c.fullscreen {
                Rect {
                    x: 0,
                    y: 0,
                    w: self.screen_w,
                    h: self.screen_h,
                }
            } else if c.floating || self.layout == Layout::Floating {
                // Keep floating windows inside the work area, as dwm clamps.
                self.clamp(c.geom, screen)
            } else {
                let r = boxes[idx];
                idx += 1;
                r
            };
            // Smart borders, like dwm: no border on a lone tile or in monocle,
            // and never on a fullscreen window. Floating windows keep theirs.
            let smart = !c.floating && (n == 1 || self.layout == Layout::Monocle);
            let b = if c.fullscreen || smart {
                0
            } else {
                self.cfg.border_width
            };
            self.c.configure_window(
                c.win,
                &ConfigureWindowAux::new()
                    .x(i32::from(r.x))
                    .y(i32::from(r.y))
                    .width(u32::from(r.w.saturating_sub(b * 2).max(1)))
                    .height(u32::from(r.h.saturating_sub(b * 2).max(1)))
                    .border_width(u32::from(b)),
            )?;
            if c.fullscreen {
                self.c.configure_window(
                    c.win,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                )?;
            }
        }

        // Keep focus on a visible client. If the focused window was moved to
        // another tag or closed, focus the most recently used visible one.
        let visible: Vec<Window> = self
            .clients
            .iter()
            .filter(|c| c.tag == self.tag)
            .map(|c| c.win)
            .collect();
        let focus_valid = self.focus.map(|w| visible.contains(&w)).unwrap_or(false);
        if !focus_valid {
            let next = visible.last().copied();
            self.focus_client(next)?;
        }

        self.prop(
            self.root,
            "_NET_CURRENT_DESKTOP",
            AtomEnum::CARDINAL.into(),
            &[self.tag],
        )?;
        let list: Vec<Window> = self.clients.iter().map(|c| c.win).collect();
        self.prop(
            self.root,
            "_NET_CLIENT_LIST",
            AtomEnum::WINDOW.into(),
            &list,
        )?;
        self.prop(
            self.root,
            "_NET_CLIENT_LIST_STACKING",
            AtomEnum::WINDOW.into(),
            &list,
        )?;
        self.c.flush()?;
        Ok(())
    }

    /// Keep a floating rectangle inside the usable area.
    fn clamp(&self, r: Rect, area: Rect) -> Rect {
        let w = r.w.min(area.w).max(60);
        let h = r.h.min(area.h).max(40);
        let max_x = area.x + (area.w.saturating_sub(w)) as i16;
        let max_y = area.y + (area.h.saturating_sub(h)) as i16;
        Rect {
            x: r.x.clamp(area.x, max_x),
            y: r.y.clamp(area.y, max_y),
            w,
            h,
        }
    }

    /// Show a tag (dwm `view`, with `0` meaning the bitmask form).
    pub(crate) fn view_tag(&mut self, tag: u32) -> Result<()> {
        if tag < 9 {
            self.tag = tag;
        }
        self.arrange()
    }

    /// Toggle a tag in the view (dwm `view` with `toggleview`).
    pub(crate) fn toggle_tag(&mut self, tag: u32) -> Result<()> {
        // Single-tag model: toggling the visible tag returns to tag 1 rather
        // than showing an empty view. Keeps 0.2.0 predictable.
        if tag < 9 && tag != self.tag {
            self.tag = tag;
        }
        self.arrange()
    }

    /// Move the focused window to a tag (dwm `tag`/`toggletag`).
    pub(crate) fn move_to_tag(&mut self, tag: u32, follow: bool) -> Result<()> {
        if tag >= 9 {
            return Ok(());
        }
        if let Some(w) = self.focus {
            if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                c.tag = tag;
            }
            self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[tag])?;
            if follow {
                self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[tag])?;
            }
        }
        self.arrange()
    }

    /// Swap the focused window with its neighbour in the tiling order.
    pub(crate) fn swap(&mut self, delta: isize) -> Result<()> {
        let Some(w) = self.focus else { return Ok(()) };
        let visible: Vec<Window> = self
            .clients
            .iter()
            .filter(|c| c.tag == self.tag && !c.floating)
            .map(|c| c.win)
            .collect();
        if visible.len() < 2 {
            return Ok(());
        }
        let Some(i) = visible.iter().position(|x| *x == w) else {
            return Ok(());
        };
        let j = (i as isize + delta).rem_euclid(visible.len() as isize) as usize;
        let a = visible[i];
        let b = visible[j];
        let ia = self.clients.iter().position(|c| c.win == a);
        let ib = self.clients.iter().position(|c| c.win == b);
        if let (Some(ia), Some(ib)) = (ia, ib) {
            self.clients.swap(ia, ib);
        }
        self.arrange()
    }

    /// Close the focused window politely, then forcibly.
    pub(crate) fn close(&mut self, w: Window) -> Result<()> {
        if self.has_protocol(w, "WM_DELETE_WINDOW") {
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
        Ok(())
    }

    pub(crate) fn focus_next(&mut self, delta: isize) -> Result<()> {
        let visible: Vec<Window> = self
            .clients
            .iter()
            .filter(|c| c.tag == self.tag)
            .map(|c| c.win)
            .collect();
        if visible.is_empty() {
            return Ok(());
        }
        let i = visible
            .iter()
            .position(|w| Some(*w) == self.focus)
            .unwrap_or(0) as isize;
        let len = visible.len() as isize;
        let j = (i + delta).rem_euclid(len) as usize;
        self.set_focus(visible[j])
    }

    pub(crate) fn next_tag(&mut self, delta: i32) -> Result<()> {
        let t = (self.tag as i32 + delta).rem_euclid(9);
        self.tag = t as u32;
        self.arrange()
    }

    pub(crate) fn toggle_fullscreen(&mut self, w: Window) -> Result<()> {
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.fullscreen = !c.fullscreen;
            let f = c.fullscreen;
            let tag = c.tag;
            let full = self.atom("_NET_WM_STATE_FULLSCREEN");
            let state: &[u32] = if f { &[full] } else { &[] };
            self.prop(w, "_NET_WM_STATE", AtomEnum::ATOM.into(), state)?;
            if f {
                self.c
                    .configure_window(w, &ConfigureWindowAux::new().border_width(0))?;
            } else {
                let b = self.cfg.border_width;
                self.c
                    .configure_window(w, &ConfigureWindowAux::new().border_width(u32::from(b)))?;
            }
            if tag == self.tag {
                self.set_focus(w)?;
            }
        }
        self.arrange()
    }
}
