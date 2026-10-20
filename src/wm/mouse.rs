use super::*;

const MOVE_SNAP: i16 = 4;
const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 60;

impl Wm {
    /// Grab Super+Button1 (move) and Super+Button3 (resize) on a client.
    pub(crate) fn grab_mouse_buttons(&self, w: Window) -> Result<()> {
        for button in [ButtonIndex::M1, ButtonIndex::M3] {
            for extra in [0u16, 2, 16, 18] {
                let mods = ModMask::M4 | ModMask::from(extra);
                self.c.grab_button(
                    false,
                    w,
                    EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                    x11rb::NONE,
                    x11rb::NONE,
                    button,
                    mods,
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn mouse_button(&mut self, e: ButtonPressEvent) -> Result<()> {
        if !e.state.contains(ModMask::M4) || (e.detail != 1 && e.detail != 3) {
            return Ok(());
        }
        let win = if self.clients.iter().any(|c| c.win == e.event) {
            e.event
        } else {
            e.child
        };
        let Some(index) = self.clients.iter().position(|c| c.win == win) else {
            return Ok(());
        };
        if self.clients[index].fullscreen {
            return Ok(());
        }
        self.set_focus(win)?;
        self.drag_mouse(index, e.root_x, e.root_y, e.detail == 3)
    }

    fn drag_mouse(&mut self, index: usize, start_x: i16, start_y: i16, resize: bool) -> Result<()> {
        let win = self.clients[index].win;
        let start = self.clients[index].geom;
        let monitor = self.clients[index].monitor;
        let bounds = self
            .monitors
            .get(monitor)
            .map(|m| m.rect)
            .unwrap_or_else(|| self.work_area());
        let left = (i32::from(start_x) - i32::from(start.x)).abs()
            < (i32::from(start.x) + i32::from(start.w) - i32::from(start_x)).abs();
        let top = (i32::from(start_y) - i32::from(start.y)).abs()
            < (i32::from(start.y) + i32::from(start.h) - i32::from(start_y)).abs();

        let grab = self
            .c
            .grab_pointer(
                false,
                self.root,
                EventMask::BUTTON_RELEASE | EventMask::POINTER_MOTION,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
                x11rb::NONE,
                x11rb::NONE,
                x11rb::CURRENT_TIME,
            )?
            .reply()?;
        if grab.status != GrabStatus::SUCCESS {
            return Ok(());
        }

        let mut floating = self.clients[index].floating;
        let dragged: Result<()> = (|| {
            loop {
                let event = self.c.wait_for_event()?;
                match event {
                    Event::MotionNotify(m) => {
                        let dx = i32::from(m.root_x) - i32::from(start_x);
                        let dy = i32::from(m.root_y) - i32::from(start_y);
                        if !floating
                            && (dx.abs() > i32::from(MOVE_SNAP) || dy.abs() > i32::from(MOVE_SNAP))
                        {
                            floating = true;
                            if let Some(c) = self.clients.get_mut(index) {
                                c.floating = true;
                            }
                        }
                        if !floating {
                            continue;
                        }
                        let current = if resize {
                            resize_geometry(start, dx, dy, left, top, bounds)
                        } else {
                            move_geometry(start, dx, dy, bounds)
                        };
                        if let Some(c) = self.clients.get_mut(index) {
                            c.geom = current;
                        }
                        self.c.configure_window(
                            win,
                            &ConfigureWindowAux::new()
                                .x(i32::from(current.x))
                                .y(i32::from(current.y))
                                .width(u32::from(current.w))
                                .height(u32::from(current.h)),
                        )?;
                        self.c.flush()?;
                    }
                    Event::ButtonRelease(_) => break,
                    Event::ConfigureRequest(e) => self.configure_request(e)?,
                    Event::MapRequest(e) => self.manage(e.window)?,
                    Event::Expose(_) => {}
                    _ => {}
                }
            }
            Ok(())
        })();
        self.c.ungrab_pointer(x11rb::CURRENT_TIME)?;
        self.arrange()?;
        dragged
    }
}

fn move_geometry(start: Rect, dx: i32, dy: i32, bounds: Rect) -> Rect {
    let mut x = i32::from(start.x) + dx;
    let mut y = i32::from(start.y) + dy;
    x = x.clamp(
        i32::from(bounds.x),
        i32::from(bounds.x) + i32::from(bounds.w) - i32::from(start.w),
    );
    y = y.clamp(
        i32::from(bounds.y),
        i32::from(bounds.y) + i32::from(bounds.h) - i32::from(start.h),
    );
    Rect {
        x: x as i16,
        y: y as i16,
        ..start
    }
}

fn resize_geometry(start: Rect, dx: i32, dy: i32, left: bool, top: bool, bounds: Rect) -> Rect {
    let mut x = i32::from(start.x);
    let mut y = i32::from(start.y);
    let mut w = i32::from(start.w);
    let mut h = i32::from(start.h);
    if left {
        x += dx;
        w -= dx;
    } else {
        w += dx;
    }
    if top {
        y += dy;
        h -= dy;
    } else {
        h += dy;
    }

    let right = i32::from(bounds.x) + i32::from(bounds.w);
    let bottom = i32::from(bounds.y) + i32::from(bounds.h);
    if x < i32::from(bounds.x) {
        w -= i32::from(bounds.x) - x;
        x = i32::from(bounds.x);
    }
    if y < i32::from(bounds.y) {
        h -= i32::from(bounds.y) - y;
        y = i32::from(bounds.y);
    }
    if x + w > right {
        w = right - x;
    }
    if y + h > bottom {
        h = bottom - y;
    }
    if w < i32::from(MIN_WIDTH) {
        if left {
            x -= i32::from(MIN_WIDTH) - w;
        }
        w = i32::from(MIN_WIDTH);
    }
    if h < i32::from(MIN_HEIGHT) {
        if top {
            y -= i32::from(MIN_HEIGHT) - h;
        }
        h = i32::from(MIN_HEIGHT);
    }
    x = x.max(i32::from(bounds.x));
    y = y.max(i32::from(bounds.y));
    w = w.min(right - x);
    h = h.min(bottom - y);
    Rect {
        x: x as i16,
        y: y as i16,
        w: w as u16,
        h: h as u16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: Rect = Rect {
        x: 0,
        y: 0,
        w: 1000,
        h: 800,
    };

    #[test]
    fn moving_clamps_to_the_monitor() {
        let start = Rect {
            x: 10,
            y: 10,
            w: 200,
            h: 100,
        };
        let moved = move_geometry(start, 2000, -100, BOUNDS);
        assert_eq!(moved.x, 800);
        assert_eq!(moved.y, 0);
    }

    #[test]
    fn resizing_from_the_top_left_keeps_the_opposite_corner() {
        let start = Rect {
            x: 100,
            y: 100,
            w: 300,
            h: 200,
        };
        let resized = resize_geometry(start, -50, -40, true, true, BOUNDS);
        assert_eq!(resized.x, 50);
        assert_eq!(resized.y, 60);
        assert_eq!(resized.w, 350);
        assert_eq!(resized.h, 240);
    }

    #[test]
    fn resizing_respects_the_minimum_size() {
        let start = Rect {
            x: 100,
            y: 100,
            w: 200,
            h: 200,
        };
        let resized = resize_geometry(start, -500, -500, true, true, BOUNDS);
        assert!(resized.w >= MIN_WIDTH);
        assert!(resized.h >= MIN_HEIGHT);
    }
}
