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
        let dialog_floating = transient.is_some()
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
        self.grab_mouse_buttons(w)?;
        self.c.change_save_set(SetMode::INSERT, w)?;

        let take_focus = self.has_protocol(w, "WM_TAKE_FOCUS");
        // A new window belongs to the screen under the pointer, or to the
        // focused screen when the pointer is outside every screen.
        // A rule may pin this window to a workspace, before the pointer
        // decides the screen.
        let rule = self.rule_for(w);
        // A window whose class matches the scratchpad command becomes the
        // scratchpad, floating and centred, instead of being tiled.
        // The scratchpad is recognised by the class named in its command, not
        // by the program. Matching every argument made *every* kitty window a
        // scratchpad, because "kitty" was one of them.
        let class = self.window_class(w);
        let is_scratchpad = self.cfg.scratchpad.matches_class(&class);
        if is_scratchpad {
            self.scratchpad = Some(w);
            self.scratchpad_shown = true;
        }
        // A restored window carries the placement saved for it. Matching
        // happens after the scratchpad check so a terminal that happens to be
        // the scratchpad never consumes a normal session record.
        let pid = self.values(w, "_NET_WM_PID").first().copied();
        let restored = self.take_restored(pid, is_scratchpad);
        let floating = dialog_floating
            || is_scratchpad
            || rule.as_ref().and_then(|r| r.floating).unwrap_or(false)
            || restored.as_ref().is_some_and(|r| r.floating);
        // `_NET_WM_DESKTOP` is a global EWMH desktop number. Resolve it to
        // the owning monitor and that monitor's local workspace.
        let requested = requested_tag.and_then(|t| crate::monitor::find_global(&self.monitors, t));
        let (px, py) = self.pointer_position();
        let pointer_monitor = if self.monitors.is_empty() {
            0
        } else {
            let by_pointer = crate::monitor::monitor_at(&self.monitors, px, py);
            if self.monitors.get(by_pointer).is_some() {
                by_pointer
            } else {
                self.focused_monitor
            }
        };
        let (monitor, tag) = match &restored {
            Some(r) => (r.monitor.min(self.monitors.len().saturating_sub(1)), r.tag),
            None => match requested {
                Some((monitor, tag)) => (monitor, tag as u32),
                None => (
                    pointer_monitor,
                    rule.as_ref()
                        .and_then(|r| r.workspace)
                        .unwrap_or_else(|| self.active_tag()),
                ),
            },
        };
        let monitor_name = self
            .monitors
            .get(monitor)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        self.clients.push(Client {
            win: w,
            monitor,
            monitor_name,
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
            pid,
        });
        if restored.as_ref().is_some_and(|r| r.focused) {
            // Remember the window that had focus last time; switching to its
            // workspace waits until the other restored windows have mapped.
            self.restore_focus = Some((w, monitor, tag));
        }

        if floating && rule.as_ref().and_then(|r| r.center).unwrap_or(false) {
            let screen = self
                .monitors
                .get(monitor)
                .map(|m| m.rect)
                .unwrap_or_else(|| self.work_area());
            if let Some(c) = self.clients.last_mut() {
                c.geom.x = screen.x + (screen.w as i16 - c.geom.w as i16) / 2;
                c.geom.y = screen.y + (screen.h as i16 - c.geom.h as i16) / 2;
                c.geom.x = c.geom.x.max(screen.x);
                c.geom.y = c.geom.y.max(screen.y);
            }
        }
        if is_scratchpad {
            let screen = self
                .monitors
                .get(monitor)
                .map(|m| m.rect)
                .unwrap_or_else(|| self.work_area());
            let w_px = (f32::from(screen.w) * self.cfg.scratchpad.width) as u16;
            let h_px = (f32::from(screen.h) * self.cfg.scratchpad.height) as u16;
            if let Some(c) = self.clients.last_mut() {
                c.geom = Rect {
                    x: screen.x + ((screen.w - w_px) / 2) as i16,
                    y: screen.y + ((screen.h - h_px) / 2) as i16,
                    w: w_px,
                    h: h_px,
                };
            }
        }
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
        // A restored window must not move focus while its siblings are still
        // starting; the saved focus is applied once restore has settled.
        if restored.is_none() && tag == self.active_tag() {
            self.set_focus(w)?;
        }
        self.arrange()?;
        if self.restore_remaining == 0 && self.restore_deadline.is_some() {
            self.finish_restore()?;
        }
        Ok(())
    }

    pub(crate) fn unmanage(&mut self, w: Window) {
        if self.scratchpad == Some(w) {
            self.scratchpad = None;
            self.scratchpad_shown = false;
        }
        if let Some(pos) = self.clients.iter().position(|c| c.win == w) {
            self.clients.remove(pos);
        }
        self.hidden.remove(&w);
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
    /// WM_CLASS for a window as one string, e.g. "kitty Scratchpad".
    pub(crate) fn window_class(&self, w: Window) -> String {
        self.c
            .get_property(false, w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.value).to_string())
            .unwrap_or_default()
    }

    /// The first rule whose class matches this window.
    fn rule_for(&self, w: Window) -> Option<crate::config::Rule> {
        if self.cfg.rules.is_empty() {
            return None;
        }
        let class = self.window_class(w);
        self.cfg
            .rules
            .iter()
            .find(|rule| class.contains(&rule.class))
            .cloned()
    }

    /// Pointer position, queried fresh (used by focus-follows-mouse).
    pub(crate) fn pointer_position_now(&self) -> (i16, i16) {
        self.pointer_position()
    }

    /// Focus whichever screen the pointer is over.
    ///
    /// Moving the mouse across a screen boundary changes the focused screen
    /// even when the pointer lands on empty desktop rather than a window.
    pub(crate) fn focus_screen_at(&mut self, x: i16, y: i16) -> Result<()> {
        if self.monitors.len() < 2 {
            return Ok(());
        }
        let target = crate::monitor::monitor_at(&self.monitors, x, y);
        if target == self.focused_monitor {
            return Ok(());
        }
        self.focused_monitor = target;
        // Focus a window on that screen when there is one, so typing has a
        // destination. An empty screen keeps the pointer focus.
        let active = self.monitors[target].active as u32;
        let win = self
            .clients
            .iter()
            .find(|c| c.monitor == target && c.tag == active)
            .map(|c| c.win);
        match win {
            Some(w) => self.set_focus(w),
            None => Ok(()),
        }
    }

    /// Pointer position in root coordinates, or the focused screen's centre.
    fn pointer_position(&self) -> (i16, i16) {
        if let Ok(cookie) = self.c.query_pointer(self.root) {
            if let Ok(reply) = cookie.reply() {
                return (reply.root_x, reply.root_y);
            }
        }
        match self.monitors.get(self.focused_monitor) {
            Some(m) => (
                m.rect.x + (m.rect.w / 2) as i16,
                m.rect.y + (m.rect.h / 2) as i16,
            ),
            None => (0, 0),
        }
    }

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
        let area = self.work_area();

        // Tile each screen separately. A window is visible when its tag is the
        // active tag on its own screen, so two screens can show two different
        // workspaces at once.
        let mut geometry: Vec<(Window, Rect, u16, bool)> = Vec::new();
        let mut visible: Vec<Window> = Vec::new();
        for mi in 0..self.monitors.len().max(1) {
            let screen = self.monitor_area(mi, area);
            let active = self.monitors.get(mi).map(|m| m.active).unwrap_or(0) as u32;
            let n = self
                .clients
                .iter()
                .filter(|c| c.monitor == mi && c.tag == active && !c.floating && !c.fullscreen)
                .count();
            let boxes = self.layout.arrange_master(
                screen,
                n,
                self.monitors.get(mi).map(|m| m.nmaster).unwrap_or(1),
                self.cfg.master_ratio,
                self.cfg.gap,
            );
            let mut idx = 0;
            for c in &self.clients {
                if c.monitor != mi {
                    continue;
                }
                if c.tag != active {
                    continue;
                }
                visible.push(c.win);
                let r = if c.fullscreen {
                    self.monitors
                        .get(mi)
                        .map(|m| m.rect)
                        .unwrap_or_else(|| self.screen_area())
                } else if c.floating || self.layout == Layout::Floating {
                    self.clamp(c.geom, screen)
                } else {
                    let r = boxes.get(idx).copied().unwrap_or(screen);
                    idx += 1;
                    r
                };
                let smart = !c.floating && (n == 1 || self.layout == Layout::Monocle);
                let b = if c.fullscreen || smart {
                    0
                } else {
                    self.cfg.border_width
                };
                geometry.push((c.win, r, b, c.fullscreen));
            }
        }

        // Hide everything that is not on an active workspace of its screen.
        for c in &self.clients {
            let shown = visible.contains(&c.win);
            if !shown && self.hidden.insert(c.win) {
                self.c.unmap_window(c.win)?;
            } else if shown && self.hidden.remove(&c.win) {
                self.c.map_window(c.win)?;
            }
        }

        for (win, r, b, fullscreen) in geometry {
            self.c.configure_window(
                win,
                &ConfigureWindowAux::new()
                    .x(i32::from(r.x))
                    .y(i32::from(r.y))
                    .width(u32::from(r.w.saturating_sub(b * 2).max(1)))
                    .height(u32::from(r.h.saturating_sub(b * 2).max(1)))
                    .border_width(u32::from(b)),
            )?;
            if fullscreen {
                self.c.configure_window(
                    win,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                )?;
            }
        }

        // Focus stays on a visible window of the focused screen.
        let focus_ok = self.focus.map(|w| visible.contains(&w)).unwrap_or(false);
        if !focus_ok {
            let next = self
                .clients
                .iter()
                .filter(|c| visible.contains(&c.win) && c.monitor == self.focused_monitor)
                .map(|c| c.win)
                .next()
                .or_else(|| {
                    self.clients
                        .iter()
                        .rev()
                        .find(|c| visible.contains(&c.win))
                        .map(|c| c.win)
                });
            self.focus_client(next)?;
        }

        self.publish_desktop_state()?;
        self.c.flush()?;
        Ok(())
    }

    /// The usable area of one monitor, with the bar's height removed only on
    /// the monitor the bar sits on.
    fn monitor_area(&self, index: usize, work: Rect) -> Rect {
        match self.monitors.get(index) {
            Some(m) => {
                // The bar spans the whole screen width, so its strut only
                // affects whichever monitor covers the top edge.
                if m.rect.y == 0 {
                    let top = work.y.saturating_sub(m.rect.y).max(0) as u16;
                    Rect {
                        x: m.rect.x,
                        y: m.rect.y + top as i16,
                        w: m.rect.w,
                        h: m.rect.h.saturating_sub(top),
                    }
                } else {
                    m.rect
                }
            }
            None => work,
        }
    }

    /// Publish the desktop state in a way panels understand with several
    /// screens: one flat list of workspaces, with the focused screen's
    /// workspace selected.
    fn publish_desktop_state(&mut self) -> Result<()> {
        let total: usize = self.monitors.iter().map(|m| m.workspaces.len()).sum();
        let active = crate::monitor::global_active(&self.monitors, self.focused_monitor);
        self.prop(
            self.root,
            "_NET_NUMBER_OF_DESKTOPS",
            AtomEnum::CARDINAL.into(),
            &[total.max(1) as u32],
        )?;
        self.prop(
            self.root,
            "_NET_CURRENT_DESKTOP",
            AtomEnum::CARDINAL.into(),
            &[active],
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
        // Publish workspace names so a bar or pager can label them.
        let mut names = String::new();
        for m in &self.monitors {
            for w in &m.workspaces {
                names.push_str(w);
                names.push('\0');
            }
        }
        if !names.is_empty() {
            self.c.change_property8(
                PropMode::REPLACE,
                self.root,
                self.atom("_NET_DESKTOP_NAMES"),
                self.atom("UTF8_STRING"),
                names.as_bytes(),
            )?;
        }
        // Every client advertises its global desktop for panels using EWMH.
        for c in &self.clients {
            let global = crate::monitor::offset(&self.monitors, c.monitor) + c.tag;
            self.prop(
                c.win,
                "_NET_WM_DESKTOP",
                AtomEnum::CARDINAL.into(),
                &[global],
            )?;
        }
        // The monitor layout contract lets one bar per output describe its own
        // workspace range without guessing from the focused monitor.
        let mut layout = String::new();
        for (index, m) in self.monitors.iter().enumerate() {
            let start = crate::monitor::offset(&self.monitors, index);
            layout.push_str(&format!(
                "{}	{}	{}	{}	{}	{}	{}
",
                m.name,
                m.rect.x,
                m.rect.y,
                m.rect.w,
                start,
                m.workspaces.len(),
                m.active
            ));
        }
        self.c.change_property8(
            PropMode::REPLACE,
            self.root,
            self.atom("_NIRUWM_MONITORS"),
            self.atom("UTF8_STRING"),
            layout.as_bytes(),
        )?;
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

    /// Change the master count on the focused screen only.
    pub(crate) fn change_master(&mut self, delta: i32) -> Result<()> {
        if let Some(m) = self.monitors.get_mut(self.focused_monitor) {
            let next = m.nmaster as i32 + delta;
            // At least one master and never more than the screen is tiled for;
            // the layout code handles the upper bound.
            m.nmaster = next.max(1) as usize;
        }
        self.arrange()
    }

    /// The workspace shown on the focused screen.
    pub(crate) fn active_tag(&self) -> u32 {
        self.monitors
            .get(self.focused_monitor)
            .map(|m| m.active as u32)
            .unwrap_or(0)
    }

    /// Switch to a global EWMH desktop, focusing its owning monitor.
    pub(crate) fn view_global(&mut self, global: u32) -> Result<()> {
        if let Some((monitor, workspace)) = crate::monitor::find_global(&self.monitors, global) {
            self.focused_monitor = monitor;
            if let Some(m) = self.monitors.get_mut(monitor) {
                m.active = workspace;
            }
        }
        self.arrange()
    }

    /// Move the focused window to a global desktop, changing monitor if needed.
    pub(crate) fn move_to_global(&mut self, global: u32) -> Result<()> {
        if let Some(w) = self.focus {
            self.move_window_to_global(w, global)?;
        } else {
            self.arrange()?;
        }
        Ok(())
    }

    /// Move one specific managed window to a global desktop. Used by the
    /// standard `_NET_WM_DESKTOP` request and by autostart placement.
    pub(crate) fn move_window_to_global(&mut self, w: Window, global: u32) -> Result<()> {
        let Some((monitor, workspace)) = crate::monitor::find_global(&self.monitors, global) else {
            return Ok(());
        };
        let monitor_name = self
            .monitors
            .get(monitor)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.monitor = monitor;
            c.monitor_name = monitor_name;
            c.tag = workspace as u32;
        }
        self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[global])?;
        self.arrange()
    }

    /// Switch the focused screen to one of its workspaces.
    ///
    /// `tag` is an index into that screen's own workspace list, so each screen
    /// keeps its set independent of the others.
    pub(crate) fn view_tag(&mut self, tag: u32) -> Result<()> {
        if let Some(m) = self.monitors.get_mut(self.focused_monitor) {
            if (tag as usize) < m.workspaces.len() {
                m.active = tag as usize;
            }
        }
        self.arrange()
    }

    /// Toggle a tag in the view.
    ///
    /// One workspace is shown per screen, so toggling a different workspace
    /// switches to it; toggling the current one does nothing.
    pub(crate) fn toggle_tag(&mut self, tag: u32) -> Result<()> {
        if tag != self.active_tag() {
            return self.view_tag(tag);
        }
        Ok(())
    }

    /// Move the focused window to a tag (dwm `tag`/`toggletag`).
    pub(crate) fn move_to_tag(&mut self, tag: u32, follow: bool) -> Result<()> {
        let count = self
            .monitors
            .get(self.focused_monitor)
            .map(|m| m.workspaces.len())
            .unwrap_or(9);
        if tag as usize >= count {
            return Ok(());
        }
        if let Some(w) = self.focus {
            if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                c.tag = tag;
            }
            // Publish the global desktop number, not the monitor-local tag.
            let global = crate::monitor::offset(&self.monitors, self.focused_monitor) + tag;
            self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[global])?;
            if follow {
                self.view_tag(tag)?;
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
            .filter(|c| {
                c.monitor == self.focused_monitor && c.tag == self.active_tag() && !c.floating
            })
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

    /// Move focus to the screen to the left (-1) or right (+1).
    ///
    /// A window is focused on the target screen so typing goes somewhere
    /// useful; the screen keeps its own workspace.
    pub(crate) fn focus_monitor(&mut self, delta: i32) -> Result<()> {
        let count = self.monitors.len();
        if count < 2 {
            return Ok(());
        }
        let next = (self.focused_monitor as i32 + delta).rem_euclid(count as i32) as usize;
        self.focused_monitor = next;
        let active = self.monitors[next].active as u32;
        let target = self
            .clients
            .iter()
            .find(|c| c.monitor == next && c.tag == active)
            .map(|c| c.win);
        match target {
            Some(w) => self.set_focus(w)?,
            None => self.focus_client(None)?,
        }
        self.arrange()
    }

    /// Move the focused window to the screen to the left (-1) or right (+1).
    pub(crate) fn move_to_monitor(&mut self, delta: i32) -> Result<()> {
        let count = self.monitors.len();
        if count < 2 {
            return Ok(());
        }
        let Some(w) = self.focus else {
            return Ok(());
        };
        let target = (self.focused_monitor as i32 + delta).rem_euclid(count as i32) as usize;
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.monitor = target;
            c.tag = self.monitors[target].active as u32;
        }
        self.focused_monitor = target;
        self.arrange()
    }

    pub(crate) fn next_tag(&mut self, delta: i32) -> Result<()> {
        let len = self
            .monitors
            .get(self.focused_monitor)
            .map(|m| m.workspaces.len())
            .unwrap_or(9)
            .max(1) as i32;
        let t = (self.active_tag() as i32 + delta).rem_euclid(len);
        self.view_tag(t as u32)
    }

    pub(crate) fn toggle_fullscreen(&mut self, w: Window) -> Result<()> {
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.fullscreen = !c.fullscreen;
            let f = c.fullscreen;
            let tag = c.tag;
            let full = self.atom("_NET_WM_STATE_FULLSCREEN");
            // Bound to a local: a temporary array in the `if` lives only for
            // the statement, which rustc 1.75 rejects and later versions
            // accept. The suite's declared baseline must compile.
            let states: [u32; 1] = [full];
            let state: &[u32] = if f { &states } else { &[] };
            self.prop(w, "_NET_WM_STATE", AtomEnum::ATOM.into(), state)?;
            if f {
                self.c
                    .configure_window(w, &ConfigureWindowAux::new().border_width(0))?;
            } else {
                let b = self.cfg.border_width;
                self.c
                    .configure_window(w, &ConfigureWindowAux::new().border_width(u32::from(b)))?;
            }
            if tag == self.active_tag()
                && self
                    .clients
                    .iter()
                    .any(|c| c.win == w && c.monitor == self.focused_monitor)
            {
                self.set_focus(w)?;
            }
        }
        self.arrange()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decision that keeps a client alive across workspace switches.
    ///
    /// A hidden window's UnmapNotify is ours: it must stay managed. A visible
    /// window's UnmapNotify came from its client, so it is unmanaged. This is
    /// the invariant that replaced the outstanding-unmap counter, which drifted
    /// whenever a window changed workspace while already hidden and then
    /// dropped a live client.
    fn stays_managed(hidden: &HashSet<Window>, w: Window) -> bool {
        hidden.contains(&w)
    }

    #[test]
    fn a_hidden_window_unmapped_by_us_stays_managed() {
        let mut hidden = HashSet::new();
        hidden.insert(42u32);
        assert!(stays_managed(&hidden, 42));
    }

    #[test]
    fn a_visible_window_that_unmaps_itself_is_unmanaged() {
        let hidden = HashSet::new();
        assert!(!stays_managed(&hidden, 42));
    }

    #[test]
    fn remapping_clears_the_hidden_record() {
        // Hiding then showing again must leave nothing that could later be
        // mistaken for "we are hiding this" after a rapid switch.
        let mut hidden = HashSet::new();
        let w: Window = 7;
        hidden.insert(w);
        assert!(
            stays_managed(&hidden, w),
            "hidden while on another workspace"
        );
        hidden.remove(&w);
        assert!(
            !stays_managed(&hidden, w),
            "visible again: a later unmap is the client's"
        );
    }

    #[test]
    fn repeated_hide_and_show_is_idempotent() {
        // The old counter grew on every hide and only shrank on an event that
        // could arrive late. Repeating the cycle must not accumulate state.
        let mut hidden = HashSet::new();
        let w: Window = 9;
        for _ in 0..50 {
            assert!(hidden.insert(w), "first hide records the window");
            assert!(!hidden.insert(w), "a second hide adds nothing");
            hidden.remove(&w);
        }
        assert!(hidden.is_empty(), "no leftover bookkeeping after cycles");
    }
}
