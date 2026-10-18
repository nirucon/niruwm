#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i16,
    pub y: i16,
    pub w: u16,
    pub h: u16,
}

/// Distribute remainder pixels, never silently lose the bottom edge.
pub fn tile(area: Rect, n: usize, ratio: f32, gap: u16, monocle: bool) -> Vec<Rect> {
    if n == 0 {
        return vec![];
    }
    let g = if n == 1 || monocle {
        0
    } else {
        gap.min(area.w / 8)
            .min((u32::from(area.h) / (n as u32 * 2).max(1)) as u16)
    };
    let x = area.x + i16::try_from(g).unwrap_or(0);
    let y = area.y + i16::try_from(g).unwrap_or(0);
    let w = area.w.saturating_sub(g * 2).max(1);
    let h = area.h.saturating_sub(g * 2).max(1);
    if n == 1 || monocle {
        return vec![Rect { x, y, w, h }; n];
    }
    tile_with_master(area, n, 1, ratio, gap)
}

/// Tile with `nmaster` windows in the master column (dwm's `nmaster`).
pub fn tile_with_master(area: Rect, n: usize, nmaster: usize, ratio: f32, gap: u16) -> Vec<Rect> {
    if n == 0 {
        return vec![];
    }
    if n == 1 || nmaster == 0 || nmaster >= n {
        return tile(area, n, ratio, gap, false);
    }
    let g = gap
        .min(area.w / 8)
        .min((u32::from(area.h) / (n as u32 * 2).max(1)) as u16);
    let x = area.x + i16::try_from(g).unwrap_or(0);
    let y = area.y + i16::try_from(g).unwrap_or(0);
    let w = area.w.saturating_sub(g * 2).max(1);
    let h = area.h.saturating_sub(g * 2).max(1);
    let mw = ((w.saturating_sub(g)) as f32 * ratio) as u16;
    let mut out = Vec::with_capacity(n);
    let count = nmaster as u32;
    let usable = u32::from(h).saturating_sub(u32::from(g) * (count - 1));
    for i in 0..count {
        let top = usable * i / count + u32::from(g) * i;
        let bottom = usable * (i + 1) / count;
        out.push(Rect {
            x,
            y: y + top as i16,
            w: mw.max(1),
            h: (bottom - usable * i / count).max(1) as u16,
        });
    }
    let scount = (n - nmaster) as u32;
    let susable = u32::from(h).saturating_sub(u32::from(g) * (scount - 1));
    for i in 0..scount {
        let top = susable * i / scount + u32::from(g) * i;
        let bottom = susable * (i + 1) / scount;
        out.push(Rect {
            x: x + mw as i16 + g as i16,
            y: y + top as i16,
            w: w.saturating_sub(mw + g).max(1),
            h: (bottom - susable * i / scount).max(1) as u16,
        });
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Layout {
    #[default]
    Tile,
    Left,
    Monocle,
    Centered,
    /// Centered master with floating side windows (dwm's centeredfloatingmaster).
    CenteredFloat,
    Fibonacci,
    Floating,
}

impl Layout {
    /// Short name used in the config and on the command line.
    #[allow(dead_code)] // printed by niructl and used by the WM
    pub fn name(self) -> &'static str {
        match self {
            Layout::Tile => "tile",
            Layout::Left => "left",
            Layout::Monocle => "monocle",
            Layout::Centered => "centered",
            Layout::CenteredFloat => "centeredfloat",
            Layout::Fibonacci => "fibonacci",
            Layout::Floating => "floating",
        }
    }

    /// One line of explanation, shown by `niructl layouts`.
    #[allow(dead_code)]
    pub fn describe(self) -> &'static str {
        match self {
            Layout::Tile => "master left, stack right (the default)",
            Layout::Left => "master right, stack left",
            Layout::Monocle => "one window fills the screen, the rest behind it",
            Layout::Centered => "master in the middle, stack either side",
            Layout::CenteredFloat => "master centred at 70% height, the rest floating",
            Layout::Fibonacci => "spiral split, each window half of the rest",
            Layout::Floating => "nothing is tiled, windows keep their own size",
        }
    }

    pub fn all() -> &'static [Layout] {
        &[
            Layout::Tile,
            Layout::Left,
            Layout::Monocle,
            Layout::Centered,
            Layout::CenteredFloat,
            Layout::Fibonacci,
            Layout::Floating,
        ]
    }

    pub fn next(self) -> Self {
        let all = Self::all();
        let i = all.iter().position(|l| *l == self).unwrap_or(0);
        all[(i + 1) % all.len()]
    }

    pub fn prev(self) -> Self {
        let all = Self::all();
        let i = all.iter().position(|l| *l == self).unwrap_or(0);
        all[(i + all.len() - 1) % all.len()]
    }

    #[allow(dead_code)] // kept for single-window callers and tests
    pub fn arrange(self, area: Rect, n: usize, ratio: f32, gap: u16) -> Vec<Rect> {
        self.arrange_master(area, n, 1, ratio, gap)
    }

    pub fn arrange_master(
        self,
        area: Rect,
        n: usize,
        nmaster: usize,
        ratio: f32,
        gap: u16,
    ) -> Vec<Rect> {
        match self {
            Self::Monocle => tile(area, n, ratio, gap, true),
            Self::Left => tile_with_master(area, n, nmaster, ratio, gap)
                .into_iter()
                .map(|r| Rect {
                    x: area.x
                        + (i32::from(area.w) - i32::from(r.x - area.x) - i32::from(r.w)) as i16,
                    ..r
                })
                .collect(),
            Self::Centered if n > 2 => {
                let g = gap.min(area.w / 10).min(area.h / (n as u16 * 2));
                let inner = Rect {
                    x: area.x + g as i16,
                    y: area.y + g as i16,
                    w: area.w - g * 2,
                    h: area.h - g * 2,
                };
                let mw = (f32::from(inner.w.saturating_sub(g * 2)) * ratio) as u16;
                let side = (inner.w - mw - g * 2) / 2;
                let mut out = vec![Rect {
                    x: inner.x + side as i16 + g as i16,
                    y: inner.y,
                    w: mw,
                    h: inner.h,
                }];
                let left = (n - 1) / 2;
                let right = n - 1 - left;
                for (count, x, w) in [
                    (left, inner.x, side),
                    (
                        right,
                        inner.x + side as i16 + g as i16 + mw as i16 + g as i16,
                        inner.w - side - mw - g * 2,
                    ),
                ] {
                    for i in 0..count {
                        let usable =
                            usize::from(inner.h).saturating_sub(usize::from(g) * (count - 1));
                        let y = usable * i / count + usize::from(g) * i;
                        let h = usable * (i + 1) / count - usable * i / count;
                        out.push(Rect {
                            x,
                            y: inner.y + y as i16,
                            w: w.max(1),
                            h: h.max(1) as u16,
                        });
                    }
                }
                out
            }
            Self::CenteredFloat if n > 2 => {
                // Master centred at 70% height, side windows floating above it.
                let g = gap.min(area.w / 10).min(area.h / 10);
                let inner = Rect {
                    x: area.x + g as i16,
                    y: area.y + g as i16,
                    w: area.w - g * 2,
                    h: area.h - g * 2,
                };
                let mh = (f32::from(inner.h) * 0.7) as u16;
                let mw = (f32::from(inner.w) * 0.5) as u16;
                let mx = inner.x + (inner.w as i16 - mw as i16) / 2;
                let my = inner.y + (inner.h as i16 - mh as i16) / 2;
                let mut out = vec![Rect {
                    x: mx,
                    y: my,
                    w: mw,
                    h: mh,
                }];
                let side_w = (inner.w - mw) / 2 - g;
                let rows = ((n - 1) as f32 / 2.0).ceil() as usize;
                let row_h = inner.h / rows.max(1) as u16;
                for i in 0..n - 1 {
                    let left = i % 2 == 0;
                    let row = i / 2;
                    out.push(Rect {
                        x: if left {
                            inner.x + (i as u16 % 2) as i16 * 6
                        } else {
                            inner.x + (inner.w - side_w) as i16
                        },
                        y: inner.y + (row * usize::from(row_h)) as i16 + (i % 2) as i16 * 4,
                        w: side_w.max(1),
                        h: row_h.saturating_sub(g).max(1),
                    });
                }
                out
            }
            Self::Fibonacci if n > 1 => {
                let g = gap.min(area.w / 10).min(area.h / 10);
                let mut r = Rect {
                    x: area.x + g as i16,
                    y: area.y + g as i16,
                    w: area.w - g * 2,
                    h: area.h - g * 2,
                };
                let mut out = vec![];
                for i in 0..n - 1 {
                    let mut first = r;
                    if i % 2 == 0 && r.w > g + 2 {
                        let w = (r.w - g) / 2;
                        first.w = w;
                        r.x += w as i16 + g as i16;
                        r.w -= w + g;
                    } else if r.h > g + 2 {
                        let h = (r.h - g) / 2;
                        first.h = h;
                        r.y += h as i16 + g as i16;
                        r.h -= h + g;
                    }
                    out.push(first);
                }
                out.push(r);
                out
            }
            _ => tile_with_master(area, n, nmaster, ratio, gap),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 28,
            w: 1920,
            h: 1052,
        }
    }

    #[test]
    fn remainder() {
        let v = tile(area(), 4, 0.55, 8, false);
        assert_eq!(v[3].y as i32 + v[3].h as i32, 1072);
        assert_eq!(v.len(), 4);
    }

    #[test]
    fn lone() {
        let r = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 100,
        };
        assert_eq!(tile(r, 1, 0.55, 8, false), vec![r]);
    }

    #[test]
    fn empty() {
        assert!(tile(
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1
            },
            0,
            0.5,
            8,
            false
        )
        .is_empty());
    }

    #[test]
    fn layouts_stay_in_bounds() {
        for layout in Layout::all() {
            if *layout == Layout::Floating {
                continue;
            }
            for n in 1..12 {
                let boxes = layout.arrange(area(), n, 0.55, 8);
                assert_eq!(boxes.len(), n, "{layout:?} n={n}");
                for r in boxes {
                    assert!(r.w > 0 && r.h > 0, "{layout:?} empty box");
                    assert!(r.x >= 0 && r.y >= 28, "{layout:?} out of bounds");
                    assert!(i32::from(r.x) + i32::from(r.w) <= 1920);
                    assert!(i32::from(r.y) + i32::from(r.h) <= 1080);
                }
            }
        }
    }

    #[test]
    fn nmaster_splits_master_column() {
        let boxes = tile_with_master(area(), 4, 2, 0.55, 8);
        assert_eq!(boxes.len(), 4);
        // Two full-height windows on the left, two stacked on the right.
        assert_eq!(boxes[0].h, boxes[1].h.min(boxes[1].h));
        assert!(boxes[2].x > boxes[0].x);
    }

    #[test]
    fn layout_cycles_all() {
        let mut l = Layout::Tile;
        for _ in 0..Layout::all().len() {
            l = l.next();
        }
        assert_eq!(l, Layout::Tile);
        assert_eq!(Layout::Tile.prev(), Layout::Floating);
    }
}
