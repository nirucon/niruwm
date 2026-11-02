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

    pub(super) fn mouse_button(&mut self, e: ButtonPressEvent) -> Result<bool> {
        if !e.state.contains(ModMask::M4) || (e.detail != 1 && e.detail != 3) {
            return Ok(true);
        }
        let win = if self.clients.iter().any(|c| c.win == e.event) {
            e.event
        } else {
            e.child
        };
        let Some(index) = self.clients.iter().position(|c| c.win == win) else {
            return Ok(true);
        };
        if self.clients[index].fullscreen {
            return Ok(true);
        }
        self.set_focus(win)?;
        self.drag_mouse(index, e.root_x, e.root_y, e.detail == 3, e.detail)
    }

    fn drag_mouse(
        &mut self,
        index: usize,
        start_x: i16,
        start_y: i16,
        resize: bool,
        button: u8,
    ) -> Result<bool> {
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
            return Ok(true);
        }

        let started_floating = self.clients[index].floating;
        let mut floating = started_floating;
        let border = self.cfg.border_width;
        let size_hints = self.clients[index].size_hints;
        let mut release_at = None;
        let mut cancelled = false;
        let mut keep_going = true;
        self.dragging = Some(win);
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
                        // Only the button that started the drag ends it. A
                        // second button press/release must not drop the window
                        // while the user is still holding the first one.
                        if e.detail == button {
                            release_at = Some((e.root_x, e.root_y));
                            break;
                        }
                    }
                    Event::RandrScreenChangeNotify(_) => {
                        // A hotplug can invalidate the cached monitor rectangles.
                        // End this drag cleanly instead of clamping with stale data.
                        keep_going = self.handle(Event::RandrScreenChangeNotify(match event {
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
                        keep_going = self.handle(other)?;
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
        // Clear the guard before arranging or dropping: from here on the
        // window is an ordinary client again and may be tiled on its target.
        self.dragging = None;
        let _ = self.c.ungrab_pointer(x11rb::CURRENT_TIME);
        if !cancelled {
            if let Some((x, y)) = release_at {
                if !resize {
                    self.drop_to_monitor(win, x, y, started_floating)?;
                }
            }
        }
        if keep_going {
            self.arrange()?;
            if self.clients.iter().any(|c| c.win == win) && !self.hidden.contains(&win) {
                self.set_focus(win)?;
            }
        }
        dragged?;
        Ok(keep_going)
    }

    fn drop_to_monitor(
        &mut self,
        win: Window,
        x: i16,
        y: i16,
        started_floating: bool,
    ) -> Result<()> {
        let Some(index) = self.clients.iter().position(|c| c.win == win) else {
            return Ok(());
        };
        if !self.clients[index].floating {
            return Ok(());
        }
        // Dwm picks the monitor from the window rectangle, not the pointer.
        // That keeps a large window on the screen it visibly belongs to while
        // the cursor happens to be near an edge.
        let Some(target) = target_monitor_for_rect(
            self.clients[index].geom,
            &self.monitors,
            i32::from(x),
            i32::from(y),
        ) else {
            return Ok(());
        };
        let target_rect = self.monitor_work_area(target);
        let geometry = clamp_drop(self.clients[index].geom, target_rect);
        let source = self.clients[index].monitor;
        let target_tag = self.monitors[target].active as u32;
        let target_name = self.monitors[target].name.clone();
        let target_layout = self.monitors[target].layout();
        let tile_on_target = should_tile_drop(started_floating, source, target, target_layout);
        {
            let c = &mut self.clients[index];
            c.geom = geometry;
            if target != source {
                c.monitor = target;
                c.monitor_name = target_name;
                c.tag = target_tag;
            }
            // A window that was tiled in its source workspace becomes part of
            // the target workspace's tiling layout, not an arbitrary floating
            // rectangle left where the pointer happened to stop.
            c.floating = !tile_on_target;
        }
        self.focused_monitor = target;
        if target != source {
            // Client order determines master/stack position in Tile, Left,
            // Centered, Fibonacci and Scroll. Join the target workspace where
            // a newly opened window would, instead of keeping an order that
            // only made sense on the old screen.
            self.reposition_dropped_client(win, target, target_tag);
        }
        Ok(())
    }

    fn reposition_dropped_client(&mut self, win: Window, target: usize, target_tag: u32) {
        let Some(index) = self.clients.iter().position(|c| c.win == win) else {
            return;
        };
        let client = self.clients.remove(index);
        let insert_at = self
            .clients
            .iter()
            .rposition(|c| c.monitor == target && (c.tag == target_tag || c.sticky))
            .map(|i| i + 1)
            .unwrap_or(self.clients.len());
        self.clients.insert(insert_at, client);
    }
}

/// A cross-monitor drop from a tiled source is re-tiled when the target
/// workspace's active layout is a tiling layout. Float preservation is
/// deliberate for windows that were already floating and for `Floating`.
fn should_tile_drop(
    started_floating: bool,
    source: usize,
    target: usize,
    target_layout: crate::layout::Layout,
) -> bool {
    !started_floating && source != target && target_layout != crate::layout::Layout::Floating
}

/// Pick the target monitor by the largest overlap with the dragged window.
///
/// The pointer decides ties and is the fallback when the window is entirely
/// outside every screen (for example while it is being dragged through a gap).
/// This mirrors dwm's `recttomon`, which is why a large window changes screen
/// when it actually crosses, not when the cursor grazes the edge.
fn target_monitor_for_rect(rect: Rect, monitors: &[Monitor], x: i32, y: i32) -> Option<usize> {
    if monitors.is_empty() {
        return None;
    }
    let pointer = target_monitor(monitors, x, y);
    let mut best: Option<(i64, usize)> = None;
    for (index, monitor) in monitors.iter().enumerate() {
        let area = intersection_area(rect, monitor.rect);
        if area > 0 && best.map_or(true, |(best_area, _)| area > best_area) {
            best = Some((area, index));
        }
    }
    let Some((best_area, best_index)) = best else {
        return pointer;
    };
    if let Some(pointer_index) = pointer {
        if intersection_area(rect, monitors[pointer_index].rect) == best_area {
            return Some(pointer_index);
        }
    }
    Some(best_index)
}

/// Area shared by two rectangles, in square pixels.
fn intersection_area(a: Rect, b: Rect) -> i64 {
    let ax = i64::from(a.x);
    let ay = i64::from(a.y);
    let aw = i64::from(a.w);
    let ah = i64::from(a.h);
    let bx = i64::from(b.x);
    let by = i64::from(b.y);
    let bw = i64::from(b.w);
    let bh = i64::from(b.h);
    let width = (ax + aw).min(bx + bw) - ax.max(bx);
    let height = (ay + ah).min(by + bh) - ay.max(by);
    if width <= 0 || height <= 0 {
        0
    } else {
        width * height
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

    fn two_monitors() -> Vec<Monitor> {
        vec![
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
        ]
    }

    #[test]
    fn target_monitor_follows_the_window_not_the_pointer() {
        let monitors = two_monitors();
        // Most of the window is still on the left, even though the cursor has
        // crossed the boundary. Dwm keeps it on the screen it visibly occupies.
        let mostly_left = Rect {
            x: 900,
            y: 100,
            w: 700,
            h: 500,
        };
        assert_eq!(
            target_monitor_for_rect(mostly_left, &monitors, 2000, 100),
            Some(0)
        );
        // The same is true in the other direction: pointer back on the left,
        // but most of the window already on the right.
        let mostly_right = Rect {
            x: 1600,
            y: 100,
            w: 700,
            h: 500,
        };
        assert_eq!(
            target_monitor_for_rect(mostly_right, &monitors, 100, 100),
            Some(1)
        );
    }

    #[test]
    fn target_monitor_uses_the_pointer_to_break_an_even_split() {
        let monitors = two_monitors();
        // Exactly half on each screen. The screen under the cursor is the one
        // the user is indicating, so it wins the tie.
        let split = Rect {
            x: 1000,
            y: 100,
            w: 1840,
            h: 500,
        };
        assert_eq!(
            target_monitor_for_rect(split, &monitors, 2000, 100),
            Some(1)
        );
        assert_eq!(
            target_monitor_for_rect(split, &monitors, 1000, 100),
            Some(0)
        );
    }

    #[test]
    fn target_monitor_falls_back_to_the_pointer_outside_every_screen() {
        let monitors = two_monitors();
        let off_screen = Rect {
            x: 5000,
            y: 5000,
            w: 200,
            h: 100,
        };
        assert_eq!(
            target_monitor_for_rect(off_screen, &monitors, 10, 10),
            Some(0)
        );
        assert_eq!(
            target_monitor_for_rect(off_screen, &monitors, 2000, 10),
            Some(1)
        );
    }

    #[test]
    fn intersection_area_handles_negative_coordinates() {
        let a = Rect {
            x: -500,
            y: -500,
            w: 1000,
            h: 1000,
        };
        let b = Rect {
            x: -200,
            y: -200,
            w: 400,
            h: 400,
        };
        assert_eq!(intersection_area(a, b), 160_000);
        assert_eq!(
            intersection_area(
                a,
                Rect {
                    x: 600,
                    y: 600,
                    w: 100,
                    h: 100,
                }
            ),
            0
        );
    }

    #[test]
    fn cross_monitor_tiled_drop_uses_the_target_layout() {
        use crate::layout::Layout;
        assert!(should_tile_drop(false, 0, 1, Layout::Tile));
        assert!(should_tile_drop(false, 0, 1, Layout::Monocle));
        assert!(!should_tile_drop(false, 0, 0, Layout::Tile));
        assert!(!should_tile_drop(true, 0, 1, Layout::Tile));
        assert!(!should_tile_drop(false, 0, 1, Layout::Floating));
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
