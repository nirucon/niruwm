#[derive(Clone, Copy, Debug, PartialEq)]
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
    let mw = ((w.saturating_sub(g)) as f32 * ratio) as u16;
    let mut out = vec![Rect {
        x,
        y,
        w: mw.max(1),
        h,
    }];
    let count = (n - 1) as u32;
    let usable = u32::from(h).saturating_sub(u32::from(g) * (count - 1));
    for i in 0..count {
        let top = usable * i / count + u32::from(g) * i;
        let bottom = usable * (i + 1) / count;
        out.push(Rect {
            x: x + mw as i16 + g as i16,
            y: y + top as i16,
            w: w.saturating_sub(mw + g).max(1),
            h: (bottom - usable * i / count).max(1) as u16,
        });
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remainder() {
        let r = Rect {
            x: 0,
            y: 28,
            w: 1920,
            h: 1052,
        };
        let v = tile(r, 4, 0.55, 8, false);
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
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Layout {
    #[default]
    Tile,
    Left,
    Monocle,
    Centered,
    Fibonacci,
    Floating,
}
impl Layout {
    pub fn arrange(self, area: Rect, n: usize, ratio: f32, gap: u16) -> Vec<Rect> {
        match self {
            Self::Monocle => tile(area, n, ratio, gap, true),
            Self::Left => tile(area, n, ratio, gap, false)
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
            _ => tile(area, n, ratio, gap, false),
        }
    }
}
#[cfg(test)]
mod more_tests {
    use super::*;
    #[test]
    fn layouts_stay_in_bounds() {
        let area = Rect {
            x: 0,
            y: 28,
            w: 1920,
            h: 1052,
        };
        for layout in [
            Layout::Tile,
            Layout::Left,
            Layout::Monocle,
            Layout::Centered,
            Layout::Fibonacci,
        ] {
            for n in 1..12 {
                let boxes = layout.arrange(area, n, 0.55, 8);
                assert_eq!(boxes.len(), n);
                for r in boxes {
                    assert!(r.w > 0 && r.h > 0);
                    assert!(r.x >= 0 && r.y >= 28);
                    assert!(i32::from(r.x) + i32::from(r.w) <= 1920);
                    assert!(i32::from(r.y) + i32::from(r.h) <= 1080);
                }
            }
        }
    }
}
