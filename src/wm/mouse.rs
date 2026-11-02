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
        let source_monitor = self.clients[index].monitor;
        let resize_bounds = self
            .monitors
            .get(source_monitor)
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
        let border = self.cfg.border_width;
        let size_hints = self.clients[index].size_hints;
        let mut release_at = None;
        let mut cancelled = false;
        let dragged: Result<()> = (|| {
            loop {
                let event = self.c.wait_for_event()?;
                match event {
                    Event::MotionNotify(m) => {
                        // The client list can change while the pointer is grabbed.
                        // Always revalidate by the stable X window id; never keep
                        // using the initial vector index after an unmap/destroy.
                        let Some(index) = self.clients.iter().position(|c| c.win == win) else {
                            cancelled = true;
                            break;
                        };
                        if self.hidden.contains(&win) {
                            cancelled = true;
                            break;
                        }
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
                            resize_geometry(start, dx, dy, left, top, resize_bounds, size_hints)
                        } else {
                            // A move may cross monitor boundaries. Do not clamp
                            // to the source monitor; constrain only at drop.
                            move_geometry(start, dx, dy)
                        };
                        if let Some(c) = self.clients.get_mut(index) {
                            c.geom = current;
                        }
                        self.c.configure_window(
                            win,
                            &ConfigureWindowAux::new()
                                .x(i32::from(current.x))
                                .y(i32::from(current.y))
                                .width(u32::from(current.w.saturating_sub(border * 2)))
                                .height(u32::from(current.h.saturating_sub(border * 2))),
                        )?;
                        self.c.flush()?;
                    }
                    Event::ButtonRelease(e) => {
                        release_at = Some((e.root_x, e.root_y));
                        break;
                    }
                    Event::RandrScreenChangeNotify(_) => {
                        // A hotplug can invalidate the cached monitor rectangles.
                        // End this drag cleanly instead of clamping with stale data.
                        self.handle(Event::RandrScreenChangeNotify(match event {
                            Event::RandrScreenChangeNotify(e) => e,
                            _ => unreachable!(),
                        }))?;
                        cancelled = true;
                        break;
                    }
                    other => {
                        // Lifecycle, focus, configure, map, RandR and key events
                        // must reach the normal dispatcher even while a drag owns
                        // the pointer. Dropping them here was the stale-client bug.
                        let keep_going = self.handle(other)?;
                        if !keep_going {
                            cancelled = true;
                            break;
                        }
                        if self.hidden.contains(&win) || !self.clients.iter().any(|c| c.win == win)
                        {
                            cancelled = true;
                            break;
                        }
                    }
                }
            }
            Ok(())
        })();
        let _ = self.c.ungrab_pointer(x11rb::CURRENT_TIME);
        if !cancelled {
            if let Some((x, y)) = release_at {
                if !resize {
                    self.drop_to_monitor(win, x, y)?;
                }
            }
        }
        self.arrange()?;
        dragged
    }

    fn drop_to_monitor(&mut self, win: Window, x: i16, y: i16) -> Result<()> {
        let Some(index) = self.clients.iter().position(|c| c.win == win) else {
            return Ok(());
        };
        if !self.clients[index].floating {
            return Ok(());
        }
        let Some(target) = target_monitor(&self.monitors, i32::from(x), i32::from(y)) else {
            return Ok(());
        };
        let target_rect = self.monitor_work_area(target);
        let geometry = clamp_drop(self.clients[index].geom, target_rect);
        let source = self.clients[index].monitor;
        let target_tag = self.monitors[target].active as u32;
        let target_name = self.monitors[target].name.clone();
        if let Some(c) = self.clients.get_mut(index) {
            c.geom = geometry;
            if target != source {
                c.monitor = target;
                c.monitor_name = target_name;
                c.tag = target_tag;
                self.focused_monitor = target;
            }
        }
        self.arrange()?;
        self.set_focus(win)
    }
}

/// Pick the monitor under the pointer, then the nearest monitor for a gap.
fn target_monitor(monitors: &[Monitor], x: i32, y: i32) -> Option<usize> {
    if monitors.is_empty() {
        return None;
    }
    if let Some(index) = monitors.iter().position(|m| {
        let r = m.rect;
        x >= i32::from(r.x)
            && y >= i32::from(r.y)
            && x < i32::from(r.x) + i32::from(r.w)
            && y < i32::from(r.y) + i32::from(r.h)
    }) {
        return Some(index);
    }
    monitors
        .iter()
        .enumerate()
        .map(|(index, m)| {
            let r = m.rect;
            let right = i32::from(r.x) + i32::from(r.w);
            let bottom = i32::from(r.y) + i32::from(r.h);
            let dx = if x < i32::from(r.x) {
                i32::from(r.x) - x
            } else if x >= right {
                x - right + 1
            } else {
                0
            };
            let dy = if y < i32::from(r.y) {
                i32::from(r.y) - y
            } else if y >= bottom {
                y - bottom + 1
            } else {
                0
            };
            (
                i64::from(dx) * i64::from(dx) + i64::from(dy) * i64::from(dy),
                index,
            )
        })
        .min()
        .map(|(_, index)| index)
}

fn clamp_drop(rect: Rect, bounds: Rect) -> Rect {
    let mut x = i32::from(rect.x);
    let mut y = i32::from(rect.y);
    let bw = i32::from(bounds.w);
    let bh = i32::from(bounds.h);
    let w = i32::from(rect.w);
    let h = i32::from(rect.h);
    x = if w >= bw {
        i32::from(bounds.x)
    } else {
        x.clamp(i32::from(bounds.x), i32::from(bounds.x) + bw - w)
    };
    y = if h >= bh {
        i32::from(bounds.y)
    } else {
        y.clamp(i32::from(bounds.y), i32::from(bounds.y) + bh - h)
    };
    Rect {
        x: x as i16,
        y: y as i16,
        ..rect
    }
}

fn move_geometry(start: Rect, dx: i32, dy: i32) -> Rect {
    Rect {
        x: (i32::from(start.x) + dx) as i16,
        y: (i32::from(start.y) + dy) as i16,
        ..start
    }
}

fn resize_geometry(
    start: Rect,
    dx: i32,
    dy: i32,
    left: bool,
    top: bool,
    bounds: Rect,
    hints: SizeHints,
) -> Rect {
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
    let min_w = i32::from(
        hints
            .min
            .map(|(w, _)| w.max(MIN_WIDTH))
            .unwrap_or(MIN_WIDTH),
    );
    let min_h = i32::from(
        hints
            .min
            .map(|(_, h)| h.max(MIN_HEIGHT))
            .unwrap_or(MIN_HEIGHT),
    );
    if w < min_w {
        if left {
            x -= min_w - w;
        }
        w = min_w;
    }
    if h < min_h {
        if top {
            y -= min_h - h;
        }
        h = min_h;
    }
    if let Some((max_w, max_h)) = hints.max {
        w = w.min(i32::from(max_w));
        h = h.min(i32::from(max_h));
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
    fn moving_follows_root_coordinates_across_monitors() {
        let start = Rect {
            x: 10,
            y: 10,
            w: 200,
            h: 100,
        };
        let moved = move_geometry(start, 2000, -100);
        assert_eq!(moved.x, 2010);
        assert_eq!(moved.y, -90);
    }

    #[test]
    fn resizing_from_the_top_left_keeps_the_opposite_corner() {
        let start = Rect {
            x: 100,
            y: 100,
            w: 300,
            h: 200,
        };
        let resized = resize_geometry(start, -50, -40, true, true, BOUNDS, SizeHints::default());
        assert_eq!(resized.x, 50);
        assert_eq!(resized.y, 60);
        assert_eq!(resized.w, 350);
        assert_eq!(resized.h, 240);
    }

    #[test]
    fn client_minimum_size_wins_over_the_wm_default() {
        let start = Rect {
            x: 100,
            y: 100,
            w: 700,
            h: 700,
        };
        let hints = SizeHints {
            min: Some((543, 427)),
            max: None,
        };
        let resized = resize_geometry(start, 600, 600, true, true, BOUNDS, hints);
        assert_eq!((resized.w, resized.h), (543, 427));
    }

    #[test]
    fn client_maximum_size_is_respected() {
        let start = Rect {
            x: 100,
            y: 100,
            w: 300,
            h: 200,
        };
        let hints = SizeHints {
            min: None,
            max: Some((400, 300)),
        };
        let resized = resize_geometry(start, 500, 500, false, false, BOUNDS, hints);
        assert_eq!((resized.w, resized.h), (400, 300));
    }

    #[test]
    fn target_monitor_uses_pointer_then_nearest_gap() {
        let monitors = vec![
            Monitor::for_test(
                "left",
                Rect {
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                },
            ),
            Monitor::for_test(
                "right",
                Rect {
                    x: 1920,
                    y: 0,
                    w: 1920,
                    h: 1080,
                },
            ),
        ];
        assert_eq!(target_monitor(&monitors, 10, 10), Some(0));
        assert_eq!(target_monitor(&monitors, 2000, 10), Some(1));
        assert_eq!(target_monitor(&monitors, 1919, 10), Some(0));
    }

    #[test]
    fn target_monitor_handles_negative_coordinates_and_vertical_stacks() {
        let monitors = vec![
            Monitor::for_test(
                "top",
                Rect {
                    x: -1920,
                    y: -1080,
                    w: 1920,
                    h: 1080,
                },
            ),
            Monitor::for_test(
                "bottom",
                Rect {
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                },
            ),
        ];
        assert_eq!(target_monitor(&monitors, -100, -100), Some(0));
        assert_eq!(target_monitor(&monitors, 100, 100), Some(1));
        // A gap between screens falls back to the nearest edge.
        assert_eq!(target_monitor(&monitors, 0, -1), Some(0));
    }

    #[test]
    fn drop_clamps_oversized_windows_to_the_target_origin() {
        let target = Rect {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let dropped = clamp_drop(
            Rect {
                x: 4000,
                y: -100,
                w: 200,
                h: 100,
            },
            target,
        );
        assert_eq!((dropped.x, dropped.y), (3640, 0));
        let oversized = clamp_drop(
            Rect {
                x: 0,
                y: 0,
                w: 2500,
                h: 1200,
            },
            target,
        );
        assert_eq!(
            (oversized.x, oversized.y, oversized.w, oversized.h),
            (1920, 0, 2500, 1200)
        );
    }

    #[test]
    fn resizing_respects_the_minimum_size() {
        let start = Rect {
            x: 100,
            y: 100,
            w: 200,
            h: 200,
        };
        let resized = resize_geometry(start, -500, -500, true, true, BOUNDS, SizeHints::default());
        assert!(resized.w >= MIN_WIDTH);
        assert!(resized.h >= MIN_HEIGHT);
    }
}
