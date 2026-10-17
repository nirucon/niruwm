use super::*;

/// Keysym constants (X11/keysymdef.h and XF86keysym.h).
mod sym {
    pub const RETURN: u32 = 0xff0d;
    pub const SPACE: u32 = 0x0020;
    pub const TAB: u32 = 0xff09;
    pub const ESCAPE: u32 = 0xff1b;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const PRINT: u32 = 0xff61;
}

impl Wm {
    /// Cache every keycode's first keysym. Swedish and US keysyms live on the
    /// same hardware keycode, so one lookup covers both layouts.
    pub(crate) fn refresh_keymap(&mut self) -> Result<()> {
        let setup = self.c.setup();
        let count = setup.max_keycode - setup.min_keycode + 1;
        let map = self
            .c
            .get_keyboard_mapping(setup.min_keycode, count)?
            .reply()?;
        let per = map.keysyms_per_keycode as usize;
        self.keys.clear();
        for (i, syms) in map.keysyms.chunks(per).enumerate() {
            if let Some(first) = syms.first() {
                if *first != 0 {
                    self.keys
                        .insert((setup.min_keycode as usize + i) as u8, *first);
                }
            }
            // Some actions are bound to the shift level of a key (e.g. the
            // Swedish letters never need it; the arrow/print keys are the
            // only multi-level ones here). Keep the secondary level too when
            // no first-level binding exists.
            if syms.len() > 1 {
                self.keys
                    .entry((setup.min_keycode as usize + i) as u8)
                    .or_insert(syms[1]);
            }
        }
        Ok(())
    }

    /// Grab Super plus every binding key, on all modifier combinations that
    /// include Super (Lock and NumLock in either state).
    pub(crate) fn grab(&self) -> Result<()> {
        self.c.ungrab_key(0, self.root, ModMask::ANY)?;
        let setup = self.c.setup();
        let count = setup.max_keycode - setup.min_keycode + 1;
        let map = self
            .c
            .get_keyboard_mapping(setup.min_keycode, count)?
            .reply()?;
        let per = map.keysyms_per_keycode as usize;
        let wanted = |syms: &[u32]| {
            syms.iter().any(|s| {
                matches!(
                    *s,
                    sym::RETURN
                        | sym::SPACE
                        | sym::TAB
                        | sym::ESCAPE
                        | sym::LEFT
                        | sym::UP
                        | sym::RIGHT
                        | sym::DOWN
                        | sym::PRINT
                        // 1..9
                        | 0x31..=0x39
                ) || (0x61..=0x7a).contains(s)
            })
        };
        for (i, syms) in map.keysyms.chunks(per).enumerate() {
            if !wanted(syms) {
                continue;
            }
            let code = (setup.min_keycode as usize + i) as u8;
            for extra in [0u16, 2, 16, 18] {
                for action in [0u16, 1, 4, 5] {
                    let mods = ModMask::M4 | extra | action;
                    self.c.grab_key(
                        false,
                        self.root,
                        mods,
                        code,
                        GrabMode::ASYNC,
                        GrabMode::ASYNC,
                    )?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn key(&mut self, e: KeyPressEvent) -> Result<bool> {
        let Some(&sym) = self.keys.get(&e.detail) else {
            return Ok(true);
        };
        let shift = e.state.contains(KeyButMask::SHIFT);
        let ctrl = e.state.contains(KeyButMask::CONTROL);

        match sym {
            0x31..=0x39 => {
                let tag = sym - 0x31;
                if shift {
                    if let Some(w) = self.focus {
                        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                            c.tag = tag;
                        }
                        self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[tag])?;
                    }
                } else {
                    self.tag = tag;
                }
            }
            sym::RETURN => {
                let cmd = self.cfg.terminal.clone();
                self.start(&cmd);
            }
            0x70 /* p */ => {
                let cmd = self.cfg.launcher.clone();
                self.start(&cmd);
            }
            0x71 /* q */ if shift && ctrl => return Ok(false),
            0x71 /* q */ => {
                if let Some(w) = self.focus {
                    self.close(w)?;
                }
            }
            0x6a /* j */ | 0x6b /* k */ | sym::TAB => {
                // dwm: Super+Tab cycles tags, Super+J/K cycles windows.
                if sym == sym::TAB {
                    return self.next_tag(if shift { -1 } else { 1 }).map(|_| true);
                }
                let delta = if sym == 0x6b { -1 } else { 1 };
                self.focus_next(delta)?;
            }
            sym::LEFT => {
                if ctrl {
                    if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == self.focus) {
                        c.geom.x -= 20;
                        c.floating = true;
                    }
                } else {
                    self.focus_next(-1)?;
                }
            }
            sym::RIGHT => {
                if ctrl {
                    if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == self.focus) {
                        c.geom.x += 20;
                        c.floating = true;
                    }
                } else {
                    self.focus_next(1)?;
                }
            }
            sym::UP => {
                if ctrl {
                    if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == self.focus) {
                        c.geom.y -= 20;
                        c.floating = true;
                    }
                } else {
                    self.focus_next(-1)?;
                }
            }
            sym::DOWN => {
                if ctrl {
                    if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == self.focus) {
                        c.geom.y += 20;
                        c.floating = true;
                    }
                } else {
                    self.focus_next(1)?;
                }
            }
            0x6d /* m */ => self.layout = Layout::Monocle,
            0x74 /* t */ => self.layout = Layout::Tile,
            0x79 /* y */ => self.layout = Layout::Left,
            0x63 /* c */ => self.layout = Layout::Centered,
            0x66 /* f */ => self.layout = Layout::Floating,
            0x65 /* e */ if ctrl => self.layout = Layout::Fibonacci,
            0x65 /* e */ if shift => {
                if let Some(w) = self.focus {
                    return self.toggle_fullscreen(w).map(|_| true);
                }
            }
            0x68 /* h */ => {
                self.cfg.master_ratio = (self.cfg.master_ratio - 0.05).max(0.1);
            }
            0x6c /* l */ => {
                self.cfg.master_ratio = (self.cfg.master_ratio + 0.05).min(0.9);
            }
            sym::SPACE if shift => {
                if let Some(c) = self.clients.iter_mut().find(|c| Some(c.win) == self.focus) {
                    c.floating = !c.floating;
                }
            }
            0x72 /* r */ => {
                if let Ok(c) = Config::load() {
                    if let Ok(t) = Theme::load(&c.theme) {
                        let old_gap = self.cfg.gap;
                        let old_bar = self.cfg.bar_height;
                        self.cfg = c;
                        self.theme = t;
                        self.grab()?;
                        // Restarting the bar is required when its height
                        // changes; otherwise it keeps its old strut.
                        if self.cfg.bar_height != old_bar || self.cfg.bar.enabled {
                            if let Some(mut b) = self.bar.take() {
                                let _ = b.kill();
                                let _ = b.wait();
                            }
                            self.start_bar();
                        }
                        let _ = old_gap;
                    }
                }
            }
            sym::PRINT => {
                let shot = vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "mkdir -p \"$HOME/Pictures\" && maim -s \"$HOME/Pictures/screenshot-$(date +%s).png\"".to_string(),
                ];
                self.start(&shot);
            }
            _ => (),
        }
        self.arrange()?;
        Ok(true)
    }
}
