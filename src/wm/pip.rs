//! Picture-in-picture (NIRUWM 0.32): a window made small, floating and on
//! every workspace of its screen, in a corner, above the other windows.
//!
//! Super+Alt+P turns it on for the focused window and, pressed again, puts
//! the window back as it was (tiled or floating, with its old size) on the
//! workspace that is showing. Super+Ctrl+P moves it to the next corner. A
//! browser's own picture-in-picture window (the PiP button on YouTube in
//! Helium, Chromium, Brave or Firefox) becomes one by itself when `[pip]
//! auto` is on.
//!
//! The state lives on the window too, in `_NIRUWM_PIP`, so a restart in
//! place or after a crash finds the window still in its corner and still
//! knows how to put it back.

use super::*;

/// How a window was before it became picture-in-picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pip {
    pub(crate) was_floating: bool,
    pub(crate) was_sticky: bool,
    /// The outer rectangle it had.
    pub(crate) geom: Rect,
    /// Index into `config::PIP_CORNERS`.
    pub(crate) corner: u8,
}

impl Pip {
    /// The `_NIRUWM_PIP` property value.
    pub(crate) fn to_property(self) -> [u32; 7] {
        [
            u32::from(self.was_floating),
            u32::from(self.was_sticky),
            self.geom.x as i32 as u32,
            self.geom.y as i32 as u32,
            u32::from(self.geom.w),
            u32::from(self.geom.h),
            u32::from(self.corner),
        ]
    }

    pub(crate) fn from_property(v: &[u32]) -> Option<Self> {
        if v.len() < 7 {
            return None;
        }
        let corner = u8::try_from(v[6]).ok().filter(|c| usize::from(*c) < 4)?;
        Some(Self {
            was_floating: v[0] != 0,
            was_sticky: v[1] != 0,
            geom: Rect {
                x: i16::try_from(v[2] as i32).ok()?,
                y: i16::try_from(v[3] as i32).ok()?,
                w: u16::try_from(v[4]).ok().filter(|w| *w > 0)?,
                h: u16::try_from(v[5]).ok().filter(|h| *h > 0)?,
            },
            corner,
        })
    }
}

/// The index of a corner name, bottom-right when it is unknown.
pub(crate) fn corner_index(name: &str) -> u8 {
    crate::config::PIP_CORNERS
        .iter()
        .position(|c| *c == name)
        .unwrap_or(0) as u8
}

/// The outer rectangle of a picture-in-picture window: `size` percent of the
/// screen's width, the height from the window's shape (`aspect` is its inner
/// width and height; 16:9 when it has none), `margin` pixels from the two
/// edges of `corner` inside `area` (the screen without the bar).
pub(crate) fn place(
    area: Rect,
    corner: u8,
    size: u16,
    aspect: (u16, u16),
    margin: u16,
    border: u16,
) -> Rect {
    let (aw, ah) = if aspect.0 >= 32 && aspect.1 >= 18 {
        aspect
    } else {
        (16, 9)
    };
    let margin = margin.min(area.w / 8).min(area.h / 8);
    let max_w = area.w.saturating_sub(margin * 2).max(1);
    let max_h = (u32::from(area.h) * 6 / 10).max(1);
    let mut inner_w = (u32::from(area.w) * u32::from(size.clamp(10, 60)) / 100).max(1);
    let mut inner_h = (inner_w * u32::from(ah) / u32::from(aw)).max(1);
    // A tall window (a portrait video) is limited by the height instead.
    if inner_h > max_h {
        inner_h = max_h;
        inner_w = (inner_h * u32::from(aw) / u32::from(ah)).max(1);
    }
    let w = (inner_w + u32::from(border) * 2).min(u32::from(max_w)) as u16;
    let h = (inner_h + u32::from(border) * 2)
        .min(u32::from(area.h.saturating_sub(margin * 2).max(1))) as u16;
    let left = area.x + margin as i16;
    let top = area.y + margin as i16;
    let right = area.x + area.w as i16 - margin as i16 - w as i16;
    let bottom = area.y + area.h as i16 - margin as i16 - h as i16;
    let (x, y) = match corner % 4 {
        0 => (right, bottom),
        1 => (left, bottom),
        2 => (left, top),
        _ => (right, top),
    };
    Rect { x, y, w, h }
}

/// Window titles browsers give their picture-in-picture window. Chromium's
/// (also Helium, Brave, Chrome, Vivaldi, Edge) is "Picture in picture",
/// translated; Firefox's is "Picture-in-Picture". Compared without case,
/// with hyphens as spaces.
const PIP_TITLES: &[&str] = &[
    "picture in picture",
    "bild i bild",
    "bild im bild",
    "billede i billede",
    "bilde i bilde",
    "kuva kuvassa",
    "image dans l'image",
    "imagen en imagen",
    "picture in picture (pip)",
];

/// Browsers whose always-on-top, every-workspace window is their
/// picture-in-picture window (any part of WM_CLASS, any case, substring).
const BROWSERS: &[&str] = &[
    "chromium",
    "chrome",
    "helium",
    "brave",
    "vivaldi",
    "microsoft-edge",
    "opera",
    "firefox",
    "librewolf",
    "floorp",
    "waterfox",
    "thorium",
    "yandex",
];

/// Browser classes too short to match as part of a word ("zen" is in
/// "zenity").
const BROWSERS_EXACT: &[&str] = &["zen", "zen-browser", "zen-alpha", "zen-beta"];

/// What a newly mapped window says about itself, for the PiP test.
pub(crate) struct Hints<'a> {
    pub(crate) title: &'a str,
    pub(crate) class: &'a str,
    pub(crate) role: &'a str,
    /// `_NET_WM_STATE_ABOVE` before it mapped.
    pub(crate) above: bool,
    /// `_NET_WM_STATE_STICKY`, or `_NET_WM_DESKTOP` 0xFFFFFFFF.
    pub(crate) all_desktops: bool,
    pub(crate) dialog: bool,
}

/// Whether a new window is a browser's picture-in-picture window.
///
/// Chromium names the window in the browser's language, so the structure
/// decides first: Chromium maps it always on top and on every workspace
/// (`kFloatingWindow`, `visible_on_all_workspaces`), which no ordinary
/// browser window is. The title and Firefox's window role are the fallback.
pub(crate) fn is_browser_pip(h: &Hints) -> bool {
    if h.dialog {
        return false;
    }
    let class = h.class.to_ascii_lowercase();
    let browser = class
        .split('\0')
        .any(|part| BROWSERS.iter().any(|b| part.contains(b)) || BROWSERS_EXACT.contains(&part));
    if !browser {
        return false;
    }
    if h.above && h.all_desktops {
        return true;
    }
    if h.role.eq_ignore_ascii_case("PictureInPicture") {
        return true;
    }
    let title = h.title.trim().to_lowercase().replace('-', " ");
    PIP_TITLES.contains(&title.as_str())
}

impl Wm {
    fn title_of(&self, w: Window) -> String {
        for (name, ty) in [("_NET_WM_NAME", "UTF8_STRING"), ("WM_NAME", "STRING")] {
            let value = self
                .c
                .get_property(false, w, self.atom(name), self.atom(ty), 0, 256)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| String::from_utf8_lossy(&reply.value).into_owned())
                .unwrap_or_default();
            if !value.is_empty() {
                return value;
            }
        }
        String::new()
    }

    fn role_of(&self, w: Window) -> String {
        self.c
            .get_property(
                false,
                w,
                self.atom("WM_WINDOW_ROLE"),
                AtomEnum::STRING,
                0,
                64,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| {
                String::from_utf8_lossy(&reply.value)
                    .trim_end_matches('\0')
                    .to_string()
            })
            .unwrap_or_default()
    }

    /// Whether `w`, about to be managed, is a browser picture-in-picture
    /// window that `[pip] auto` should put in the corner.
    pub(crate) fn wants_auto_pip(&self, w: Window, class: &str, dialog: bool) -> bool {
        if !self.cfg.pip.auto {
            return false;
        }
        // Chromium-based browsers give their PiP window no WM_CLASS at all
        // (measured with Helium 0.x on X11), so the program behind the window
        // names it instead: the class of another window of the same process,
        // and the process's own name.
        let mut class = class.to_string();
        if class.split('\0').all(str::is_empty) {
            if let Some(pid) = self.window_pid(w) {
                if let Some(other) = self
                    .clients
                    .iter()
                    .find(|c| c.win != w && c.pid == Some(pid) && !c.class.is_empty())
                {
                    class.push_str(&other.class);
                }
                for file in ["comm", "exe"] {
                    let path = format!("/proc/{pid}/{file}");
                    let name = if file == "exe" {
                        std::fs::read_link(&path)
                            .ok()
                            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    } else {
                        std::fs::read_to_string(&path).ok()
                    };
                    if let Some(name) = name {
                        class.push_str(name.trim());
                        class.push('\0');
                    }
                }
            }
        }
        let class = class.as_str();
        let state = self.values(w, "_NET_WM_STATE");
        let desktop = self.values(w, "_NET_WM_DESKTOP").first().copied();
        let title = self.title_of(w);
        let role = self.role_of(w);
        is_browser_pip(&Hints {
            title: &title,
            class,
            role: &role,
            above: state.contains(&self.atom("_NET_WM_STATE_ABOVE")),
            all_desktops: state.contains(&self.atom("_NET_WM_STATE_STICKY"))
                || desktop == Some(0xFFFF_FFFF),
            dialog,
        })
    }

    /// The PiP state a window carried over a restart, if any.
    pub(crate) fn adopted_pip(&self, w: Window) -> Option<Pip> {
        Pip::from_property(&self.values(w, "_NIRUWM_PIP"))
    }

    fn publish_pip(&self, w: Window, pip: Option<Pip>) -> Result<()> {
        match pip {
            Some(p) => self.prop(
                w,
                "_NIRUWM_PIP",
                AtomEnum::CARDINAL.into(),
                &p.to_property(),
            ),
            None => {
                self.c.delete_property(w, self.atom("_NIRUWM_PIP"))?;
                Ok(())
            }
        }
    }

    /// The corner rectangle for `w` on its screen.
    fn pip_rect(&self, w: Window, corner: u8) -> Option<Rect> {
        let c = self.clients.iter().find(|c| c.win == w)?;
        let border = self.cfg.border_width;
        let inner = (
            c.geom.w.saturating_sub(border * 2),
            c.geom.h.saturating_sub(border * 2),
        );
        let area = self.monitor_work_area(c.monitor);
        Some(place(
            area,
            corner,
            self.cfg.pip.size,
            inner,
            self.cfg.gap.max(8),
            border,
        ))
    }

    /// Make `w` picture-in-picture in the configured corner. Used by the key
    /// and by `[pip] auto` while the window is being managed.
    pub(crate) fn pip_on(&mut self, w: Window) -> Result<()> {
        let corner = corner_index(&self.cfg.pip.corner);
        let Some(rect) = self.pip_rect(w, corner) else {
            return Ok(());
        };
        let Some(c) = self.clients.iter_mut().find(|c| c.win == w) else {
            return Ok(());
        };
        if c.pip.is_some() || c.fullscreen || c.scratch.is_some() {
            return Ok(());
        }
        let pip = Pip {
            was_floating: c.floating,
            was_sticky: c.sticky,
            geom: c.geom,
            corner,
        };
        c.pip = Some(pip);
        c.floating = true;
        c.sticky = true;
        c.geom = rect;
        self.publish_pip(w, Some(pip))?;
        eprintln!("niruwm: window {w:#x} is picture-in-picture");
        Ok(())
    }

    /// Super+Alt+P: picture-in-picture on, or back as it was.
    pub(crate) fn pip_toggle(&mut self, w: Window) -> Result<()> {
        let pip = self.clients.iter().find(|c| c.win == w).and_then(|c| c.pip);
        match pip {
            None => self.pip_on(w)?,
            Some(p) => {
                let active = self
                    .clients
                    .iter()
                    .find(|c| c.win == w)
                    .and_then(|c| self.monitors.get(c.monitor))
                    .map(|m| m.active as u32);
                if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                    c.pip = None;
                    c.floating = p.was_floating;
                    c.sticky = p.was_sticky;
                    c.geom = p.geom;
                    // Back on the workspace that is showing, not the one it
                    // came from: the user is looking at this one.
                    if let Some(active) = active {
                        c.tag = active;
                    }
                }
                self.publish_pip(w, None)?;
                if let Some(global) = self
                    .clients
                    .iter()
                    .find(|c| c.win == w)
                    .map(|c| crate::monitor::offset(&self.monitors, c.monitor) + c.tag)
                {
                    self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[global])?;
                }
            }
        }
        self.arrange()
    }

    /// Super+Ctrl+P: the focused PiP window, or else the newest one on the
    /// focused screen, to the next corner.
    pub(crate) fn pip_next_corner(&mut self) -> Result<()> {
        let focused = self
            .focus
            .filter(|f| self.clients.iter().any(|c| c.win == *f && c.pip.is_some()));
        let target = focused.or_else(|| {
            self.clients
                .iter()
                .rev()
                .find(|c| c.pip.is_some() && c.monitor == self.focused_monitor)
                .map(|c| c.win)
        });
        let Some(w) = target else {
            return Ok(());
        };
        let Some(mut pip) = self.clients.iter().find(|c| c.win == w).and_then(|c| c.pip) else {
            return Ok(());
        };
        pip.corner = (pip.corner + 1) % 4;
        let Some(rect) = self.pip_rect(w, pip.corner) else {
            return Ok(());
        };
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.pip = Some(pip);
            c.geom = rect;
        }
        self.publish_pip(w, Some(pip))?;
        self.arrange()
    }

    /// A ConfigureRequest from a PiP window: the browser sizes its window to
    /// the video. The shape it asks for is kept, the size and the corner are
    /// the window manager's. Returns false when `w` is not a PiP window.
    pub(crate) fn pip_configure(&mut self, w: Window, asked: Option<(u16, u16)>) -> Result<bool> {
        let Some(c) = self.clients.iter().find(|c| c.win == w) else {
            return Ok(false);
        };
        let Some(pip) = c.pip else {
            return Ok(false);
        };
        let border = self.cfg.border_width;
        let shape = asked.unwrap_or((
            c.geom.w.saturating_sub(border * 2),
            c.geom.h.saturating_sub(border * 2),
        ));
        let area = self.monitor_work_area(c.monitor);
        let rect = place(
            area,
            pip.corner,
            self.cfg.pip.size,
            shape,
            self.cfg.gap.max(8),
            border,
        );
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.geom = rect;
        }
        self.c.configure_window(
            w,
            &ConfigureWindowAux::new()
                .x(i32::from(rect.x))
                .y(i32::from(rect.y))
                .width(u32::from(rect.w.saturating_sub(border * 2).max(1)))
                .height(u32::from(rect.h.saturating_sub(border * 2).max(1))),
        )?;
        Ok(true)
    }

    /// A PiP window that is no longer on its screen (moved to another one,
    /// or the screen changed size) goes back to its corner there.
    pub(crate) fn settle_pip(&mut self) {
        let moved: Vec<(Window, u8)> = self
            .clients
            .iter()
            .filter_map(|c| {
                let pip = c.pip?;
                let area = self.monitor_work_area(c.monitor);
                let cx = i32::from(c.geom.x) + i32::from(c.geom.w) / 2;
                let cy = i32::from(c.geom.y) + i32::from(c.geom.h) / 2;
                let inside = cx >= i32::from(area.x)
                    && cx < i32::from(area.x) + i32::from(area.w)
                    && cy >= i32::from(area.y)
                    && cy < i32::from(area.y) + i32::from(area.h);
                (!inside && Some(c.win) != self.dragging).then_some((c.win, pip.corner))
            })
            .collect();
        for (w, corner) in moved {
            if let Some(rect) = self.pip_rect(w, corner) {
                if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                    c.geom = rect;
                }
            }
        }
    }

    /// Switching workspaces never leaves the focus on a PiP window: it is
    /// there to be watched, not typed into.
    pub(crate) fn drop_pip_focus(&mut self) {
        if self
            .focus
            .is_some_and(|f| self.clients.iter().any(|c| c.win == f && c.pip.is_some()))
        {
            self.focus = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0,
        y: 28,
        w: 2560,
        h: 1412,
    };

    #[test]
    fn the_corner_keeps_the_shape_and_the_margin() {
        // 25% of 2560 is 640 wide; 16:9 is 360 high; border 2 each side.
        let r = place(AREA, 0, 25, (1280, 720), 8, 2);
        assert_eq!((r.w, r.h), (644, 364));
        assert_eq!((r.x, r.y), (2560 - 8 - 644, 28 + 1412 - 8 - 364));
        let r = place(AREA, 2, 25, (1280, 720), 8, 2);
        assert_eq!((r.x, r.y), (8, 36));
        let r = place(AREA, 1, 25, (1280, 720), 8, 2);
        assert_eq!((r.x, r.y), (8, 28 + 1412 - 8 - 364));
        let r = place(AREA, 3, 25, (1280, 720), 8, 2);
        assert_eq!((r.x, r.y), (2560 - 8 - 644, 36));
    }

    #[test]
    fn a_window_without_a_shape_is_16_by_9_and_a_tall_one_fits() {
        let r = place(AREA, 0, 25, (1, 1), 8, 0);
        assert_eq!((r.w, r.h), (640, 360));
        // A portrait video at 60% would be far too tall: the height decides.
        let r = place(AREA, 0, 60, (720, 1280), 8, 0);
        assert!(r.h <= 1412 * 6 / 10, "{r:?}");
        assert!(r.w < r.h);
    }

    #[test]
    fn the_property_round_trips() {
        let p = Pip {
            was_floating: false,
            was_sticky: true,
            geom: Rect {
                x: -40,
                y: 12,
                w: 800,
                h: 600,
            },
            corner: 3,
        };
        assert_eq!(Pip::from_property(&p.to_property()), Some(p));
        assert_eq!(Pip::from_property(&[1, 0, 0]), None);
        assert_eq!(Pip::from_property(&[1, 0, 0, 0, 10, 10, 9]), None);
    }

    fn hints<'a>(title: &'a str, class: &'a str) -> Hints<'a> {
        Hints {
            title,
            class,
            role: "",
            above: false,
            all_desktops: false,
            dialog: false,
        }
    }

    #[test]
    fn browser_pip_windows_are_found_in_any_language() {
        // Chromium and Helium: always on top and on every desktop.
        let h = Hints {
            above: true,
            all_desktops: true,
            ..hints("Bild-i-bild", "helium\0Helium\0")
        };
        assert!(is_browser_pip(&h));
        let h = Hints {
            above: true,
            all_desktops: true,
            ..hints("", "chromium\0Chromium\0")
        };
        assert!(is_browser_pip(&h));
        // No WM_CLASS (Helium's PiP window): the process name stands in.
        let h = Hints {
            above: true,
            all_desktops: true,
            ..hints("Picture in picture", "\0helium\0helium\0")
        };
        assert!(is_browser_pip(&h));
        // Titles alone, as a fallback.
        assert!(is_browser_pip(&hints(
            "Picture in picture",
            "brave-browser\0Brave-browser\0"
        )));
        assert!(is_browser_pip(&hints(
            "Picture-in-Picture",
            "Toolkit\0firefox\0"
        )));
        assert!(is_browser_pip(&hints(
            "Bild-i-bild",
            "navigator\0firefox\0"
        )));
        let h = Hints {
            role: "PictureInPicture",
            ..hints("Video", "Toolkit\0firefox\0")
        };
        assert!(is_browser_pip(&h));
    }

    #[test]
    fn ordinary_windows_are_not_pip() {
        assert!(!is_browser_pip(&hints(
            "YouTube - Helium",
            "helium\0Helium\0"
        )));
        // Not a browser: a terminal titled like one, an always-on-top tool.
        assert!(!is_browser_pip(&hints(
            "Picture in picture",
            "kitty\0kitty\0"
        )));
        let h = Hints {
            above: true,
            all_desktops: true,
            ..hints("", "nirupres\0nirupres\0")
        };
        assert!(!is_browser_pip(&h));
        // A browser dialog is never one.
        let h = Hints {
            dialog: true,
            above: true,
            all_desktops: true,
            ..hints("Picture in picture", "chromium\0Chromium\0")
        };
        assert!(!is_browser_pip(&h));
    }
}
