//! `niructl debug windows`: what the window manager tracks about every
//! window, so a window that does not show up can be explained at once.
//!
//! The manager answers `_NIRUWM_COMMAND` 24 with this text in
//! `_NIRUWM_DEBUG_RESULT` on the caller's window.

use super::Wm;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::*;

/// One managed window, as the report shows it.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Row {
    pub win: u32,
    pub class: String,
    pub monitor: usize,
    pub tag: u32,
    /// The X server's view: mapped (viewable), unmapped or gone.
    pub x_state: &'static str,
    pub parent: Option<u32>,
    pub pid: Option<u32>,
    pub marks: Vec<&'static str>,
}

/// Everything the report shows.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Report {
    pub version: String,
    pub focus: Option<u32>,
    /// Seconds since the last tray click (`_NIRU_TRAY_ACTIVATE`).
    pub tray_click_age: Option<u64>,
    pub rows: Vec<Row>,
    /// Ids recorded as withdrawn, hidden or kept that belong to no managed
    /// window. Always empty unless something is wrong.
    pub stale: Vec<(u32, &'static str)>,
    /// Unmanaged windows kept off the screen although their client mapped
    /// them (`Wm::dismissed`), with the X server's view.
    pub dismissed: Vec<(u32, &'static str)>,
    /// Windows waiting for a tray to take them, with seconds waited.
    pub tray_pending: Vec<(u32, u64)>,
}

fn hex(w: u32) -> String {
    format!("{w:#x}")
}

pub(crate) fn render(r: &Report) -> String {
    let mut out = format!("NIRUWM {}\n", r.version);
    out.push_str(&format!(
        "focus: {}\n",
        r.focus.map(hex).unwrap_or_else(|| "none".into())
    ));
    out.push_str(&format!(
        "last tray click: {}\n",
        r.tray_click_age
            .map(|s| format!("{s} s ago"))
            .unwrap_or_else(|| "never".into())
    ));
    out.push_str(&format!("managed windows: {}\n", r.rows.len()));
    for row in &r.rows {
        let class = if row.class.is_empty() {
            "-".to_string()
        } else {
            row.class.clone()
        };
        out.push_str(&format!(
            "  {} {} screen {} workspace {} x:{} parent {} pid {}{}{}\n",
            hex(row.win),
            class,
            row.monitor,
            row.tag + 1,
            row.x_state,
            row.parent.map(hex).unwrap_or_else(|| "-".into()),
            row.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            if row.marks.is_empty() { "" } else { " " },
            row.marks.join(" "),
        ));
    }
    let list = |title: &str, items: Vec<String>, out: &mut String| {
        if items.is_empty() {
            out.push_str(&format!("{title}: none\n"));
        } else {
            out.push_str(&format!("{title}: {}\n", items.join(", ")));
        }
    };
    list(
        "kept off the screen (not managed)",
        r.dismissed
            .iter()
            .map(|(w, state)| format!("{} x:{state}", hex(*w)))
            .collect(),
        &mut out,
    );
    list(
        "waiting for a tray",
        r.tray_pending
            .iter()
            .map(|(w, secs)| format!("{} {secs} s", hex(*w)))
            .collect(),
        &mut out,
    );
    list(
        "stale records (a bug if not none)",
        r.stale
            .iter()
            .map(|(w, set)| format!("{} in {set}", hex(*w)))
            .collect(),
        &mut out,
    );
    out
}

impl Wm {
    fn x_state(&self, w: Window) -> &'static str {
        match self
            .c
            .get_window_attributes(w)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
        {
            Some(a) if a.map_state == MapState::VIEWABLE => "mapped",
            Some(a) if a.map_state == MapState::UNVIEWABLE => "unviewable",
            Some(_) => "unmapped",
            None => "gone",
        }
    }

    pub(crate) fn debug_windows(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let tray_click_age = self
            .values(self.root, "_NIRU_TRAY_ACTIVATE")
            .first()
            .copied()
            .filter(|s| *s != 0)
            .map(|s| now.saturating_sub(u64::from(s)));
        let rows = self
            .clients
            .iter()
            .map(|c| {
                let mut marks = Vec::new();
                let flags = [
                    (c.floating, "floating"),
                    (c.fullscreen, "fullscreen"),
                    (c.sticky, "sticky"),
                    (c.orphan, "orphan-popup"),
                    (self.withdrawn.contains(&c.win), "withdrawn"),
                    (self.hidden.contains(&c.win), "hidden"),
                    (self.kept.contains(&c.win), "kept-mapped"),
                    (c.scratch.is_some(), "scratchpad"),
                    (c.swallowed_by.is_some(), "swallowed"),
                    (c.pip.is_some(), "pip"),
                    (c.urgent, "urgent"),
                ];
                for (on, name) in flags {
                    if on {
                        marks.push(name);
                    }
                }
                Row {
                    win: c.win,
                    class: c
                        .class
                        .split('\0')
                        .find(|p| !p.is_empty())
                        .unwrap_or("")
                        .to_string(),
                    monitor: c.monitor,
                    tag: c.tag,
                    x_state: self.x_state(c.win),
                    parent: c.parent,
                    pid: c.pid,
                    marks,
                }
            })
            .collect();
        let managed = |w: &Window| self.clients.iter().any(|c| c.win == *w);
        let mut stale: Vec<(u32, &'static str)> = Vec::new();
        for (set, name) in [
            (&self.withdrawn, "withdrawn"),
            (&self.hidden, "hidden"),
            (&self.kept, "kept"),
        ] {
            stale.extend(set.iter().filter(|w| !managed(w)).map(|w| (*w, name)));
        }
        stale.sort_unstable();
        let mut dismissed: Vec<(u32, &'static str)> = self
            .dismissed
            .iter()
            .map(|w| (*w, self.x_state(*w)))
            .collect();
        dismissed.sort_unstable();
        let mut tray_pending: Vec<(u32, u64)> = self
            .tray_pending
            .iter()
            .map(|(w, since)| (*w, since.elapsed().as_secs()))
            .collect();
        tray_pending.sort_unstable();
        let _ = self.c.flush();
        render(&Report {
            version: env!("CARGO_PKG_VERSION").to_string(),
            focus: self.focus,
            tray_click_age,
            rows,
            stale,
            dismissed,
            tray_pending,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_names_every_mark_and_every_leftover() {
        let text = render(&Report {
            version: "0.37.17".into(),
            focus: Some(0x2000017),
            tray_click_age: Some(3),
            rows: vec![Row {
                win: 0x2000017,
                class: "nextcloud".into(),
                monitor: 0,
                tag: 0,
                x_state: "mapped",
                parent: Some(0x2000019),
                pid: Some(42),
                marks: vec!["floating", "orphan-popup"],
            }],
            stale: vec![(0x2000019, "withdrawn")],
            dismissed: vec![(0x200001c, "unmapped")],
            tray_pending: vec![(0x3000001, 1)],
        });
        assert!(text.starts_with("NIRUWM 0.37.17\nfocus: 0x2000017\nlast tray click: 3 s ago\n"));
        assert!(text.contains(
            "  0x2000017 nextcloud screen 0 workspace 1 x:mapped parent 0x2000019 pid 42 floating orphan-popup\n"
        ));
        assert!(text.contains("kept off the screen (not managed): 0x200001c x:unmapped\n"));
        assert!(text.contains("waiting for a tray: 0x3000001 1 s\n"));
        assert!(text.contains("stale records (a bug if not none): 0x2000019 in withdrawn\n"));
    }

    #[test]
    fn an_empty_report_says_none() {
        let text = render(&Report {
            version: "0.37.17".into(),
            ..Report::default()
        });
        assert!(text.contains("focus: none\nlast tray click: never\nmanaged windows: 0\n"));
        assert!(text.contains("stale records (a bug if not none): none\n"));
    }
}
