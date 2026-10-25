//! Monitors: where the physical screens are and which windows belong to them.
//!
//! The layout code works in screen coordinates; this module says which parts
//! of that screen are real. Every window belongs to one monitor and keeps its
//! own workspace set there, the way dwm does.
//!
//! The default is automatic: each connected screen is one monitor and gets the
//! ten workspaces. `[monitors]` in the config can name screens and give each
//! its own number of workspaces, which is what a fixed desk setup wants.

use crate::layout::Rect;
use std::collections::HashMap;

/// One physical screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor {
    /// The RandR output name, e.g. `eDP-1` or `HDMI-1`.
    pub name: String,
    /// Where it sits in the combined coordinate space.
    pub rect: Rect,
    /// Names of its workspaces, in order.
    pub workspaces: Vec<String>,
    /// Which of `workspaces` is shown.
    pub active: usize,
    /// Windows in the master column on this screen (dwm's `nmaster`).
    /// Per screen, so raising it on one monitor does not change the other.
    pub nmaster: usize,
    /// The layout of every workspace on this screen, in `workspaces` order.
    /// Each workspace keeps its own layout; the first one is used until a
    /// workspace is given another.
    pub layouts: Vec<crate::layout::Layout>,
}

impl Monitor {
    fn new(name: String, rect: Rect, count: usize) -> Self {
        let workspaces: Vec<String> = (1..=count).map(|n| n.to_string()).collect();
        Self {
            name,
            rect,
            workspaces: workspaces.clone(),
            active: 0,
            nmaster: 1,
            layouts: vec![crate::layout::Layout::Tile; workspaces.len()],
        }
    }

    /// The layout this screen's active workspace uses.
    pub fn layout(&self) -> crate::layout::Layout {
        self.layouts
            .get(self.active)
            .copied()
            .unwrap_or(crate::layout::Layout::Tile)
    }

    /// Give the active workspace another layout, leaving the others alone.
    pub fn set_layout(&mut self, layout: crate::layout::Layout) {
        let count = self.workspaces.len().max(1);
        self.layouts.resize(count, crate::layout::Layout::Tile);
        if self.active < self.layouts.len() {
            self.layouts[self.active] = layout;
        }
    }

    /// The active workspace name.
    #[allow(dead_code)] // used by --dump-config and the bar
    pub fn active_name(&self) -> &str {
        self.workspaces
            .get(self.active)
            .map(String::as_str)
            .unwrap_or("1")
    }

    /// True when a point is inside this monitor.
    pub fn contains(&self, x: i16, y: i16) -> bool {
        x >= self.rect.x
            && y >= self.rect.y
            && x < self.rect.x + self.rect.w as i16
            && y < self.rect.y + self.rect.h as i16
    }

    /// The monitor's state as it is stored in the config: its name and its
    /// workspace labels. Used by `niruwm --dump-config`.
    #[allow(dead_code)] // used by --dump-config
    pub fn summary(&self) -> String {
        format!("{}:{}", self.name, self.workspaces.join(","))
    }
}

/// Optional per-screen settings from `[monitors]`.
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorConfig {
    /// Number of workspaces a screen gets when it has no entry below.
    pub workspaces: usize,
    /// Per-screen overrides, keyed by output name.
    pub named: HashMap<String, Vec<String>>,
    /// Output preferred for focus at login, if named in the config.
    pub primary: Option<String>,
    /// Workspace layouts every screen starts with, in workspace order.
    pub layouts: Vec<crate::layout::Layout>,
    /// Per-screen workspace layouts, keyed by output name.
    pub screen_layouts: HashMap<String, Vec<crate::layout::Layout>>,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            // Ten workspaces per screen, like dwm with an extra tag
            // learned about monitors.
            workspaces: 10,
            named: HashMap::new(),
            primary: None,
            layouts: Vec::new(),
            screen_layouts: HashMap::new(),
        }
    }
}

impl MonitorConfig {
    pub fn workspaces_for(&self, name: &str) -> Option<&Vec<String>> {
        self.named.get(name)
    }
}

/// Build the monitor list from RandR outputs.
///
/// `outputs` is `(name, rect, connected)` in the order the server reports
/// them. Disconnected outputs are skipped: their geometry is stale.
///
/// Returns the monitors and whether more than one screen is active.
pub fn build(mut outputs: Vec<(String, Rect, bool)>, cfg: &MonitorConfig) -> (Vec<Monitor>, bool) {
    outputs.retain(|(_, _, connected)| *connected);
    if outputs.is_empty() {
        return (Vec::new(), false);
    }
    // Left-to-right, top-to-bottom, so `Super+,` and `Super+.` move in the
    // order people see.
    outputs.sort_by_key(|(_, r, _)| (r.y, r.x));
    let count = cfg.workspaces.max(1);
    let monitors = outputs
        .into_iter()
        .map(|(name, rect, _)| {
            // The screen's own workspace list, or the common count.
            let mut monitor = match cfg.workspaces_for(&name) {
                Some(list) if !list.is_empty() => {
                    let mut monitor = Monitor::new(name.clone(), rect, list.len());
                    monitor.workspaces = list.clone();
                    monitor
                }
                _ => Monitor::new(name.clone(), rect, count),
            };
            // The common list is a default, not a snapshot of one screen: a
            // screen with its own list keeps it, and an absent common list
            // leaves every workspace tiled.
            for (i, layout) in cfg.layouts.iter().enumerate() {
                if i < monitor.layouts.len() {
                    monitor.layouts[i] = *layout;
                }
            }
            if let Some(list) = cfg.screen_layouts.get(&name) {
                for (i, layout) in list.iter().enumerate() {
                    if i < monitor.layouts.len() {
                        monitor.layouts[i] = *layout;
                    }
                }
            }
            monitor
        })
        .collect::<Vec<_>>();
    let many = monitors.len() > 1;
    (monitors, many)
}

/// First global EWMH desktop index belonging to monitor `index`.
pub fn offset(monitors: &[Monitor], index: usize) -> u32 {
    monitors
        .iter()
        .take(index)
        .map(|m| m.workspaces.len() as u32)
        .sum()
}

/// The focused global desktop number, including the monitor's offset.
pub fn global_active(monitors: &[Monitor], index: usize) -> u32 {
    offset(monitors, index) + monitors.get(index).map(|m| m.active as u32).unwrap_or(0)
}

/// Resolve a global desktop index to `(monitor, local workspace)`.
pub fn find_global(monitors: &[Monitor], global: u32) -> Option<(usize, usize)> {
    let mut start = 0u32;
    for (index, monitor) in monitors.iter().enumerate() {
        let end = start + monitor.workspaces.len() as u32;
        if global >= start && global < end {
            return Some((index, (global - start) as usize));
        }
        start = end;
    }
    None
}

/// Index of the screen that should hold focus at login: the configured primary
/// when it is connected, otherwise the first screen.
pub fn primary(monitors: &[Monitor], cfg: &MonitorConfig) -> usize {
    cfg.primary
        .as_ref()
        .and_then(|name| monitors.iter().position(|m| &m.name == name))
        .unwrap_or(0)
}

/// The monitor a point belongs to, or the first one.
pub fn monitor_at(monitors: &[Monitor], x: i16, y: i16) -> usize {
    monitors.iter().position(|m| m.contains(x, y)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i16, y: i16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn a_laptop_gets_one_monitor_with_ten_workspaces() {
        let (m, many) = build(
            vec![("eDP-1".into(), rect(0, 0, 1920, 1080), true)],
            &MonitorConfig::default(),
        );
        assert_eq!(m.len(), 1);
        assert!(!many);
        assert_eq!(m[0].workspaces.len(), 10);
        assert_eq!(m[0].active_name(), "1");
    }

    #[test]
    fn two_screens_each_get_their_own_workspaces() {
        let (m, many) = build(
            vec![
                ("eDP-1".into(), rect(0, 0, 1920, 1080), true),
                ("HDMI-1".into(), rect(1920, 0, 2560, 1440), true),
            ],
            &MonitorConfig::default(),
        );
        assert_eq!(m.len(), 2);
        assert!(many);
        // Sorted left to right.
        assert_eq!(m[0].name, "eDP-1");
        assert_eq!(m[1].name, "HDMI-1");
        // Each has its own active workspace, independent of the other.
        assert_eq!(m[0].active, 0);
        assert_eq!(m[1].active, 0);
    }

    #[test]
    fn disconnected_outputs_are_ignored() {
        let (m, _) = build(
            vec![
                ("eDP-1".into(), rect(0, 0, 1920, 1080), true),
                ("HDMI-1".into(), rect(1920, 0, 2560, 1440), false),
            ],
            &MonitorConfig::default(),
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "eDP-1");
    }

    #[test]
    fn named_workspaces_override_the_default() {
        let mut cfg = MonitorConfig::default();
        cfg.named.insert(
            "HDMI-1".into(),
            vec!["web".into(), "code".into(), "chat".into()],
        );
        let (m, _) = build(
            vec![
                ("eDP-1".into(), rect(0, 0, 1920, 1080), true),
                ("HDMI-1".into(), rect(1920, 0, 2560, 1440), true),
            ],
            &cfg,
        );
        assert_eq!(m[0].workspaces.len(), 10);
        assert_eq!(m[1].workspaces, vec!["web", "code", "chat"]);
    }

    #[test]
    fn a_point_maps_to_the_monitor_under_it() {
        let (m, _) = build(
            vec![
                ("eDP-1".into(), rect(0, 0, 1920, 1080), true),
                ("HDMI-1".into(), rect(1920, 0, 2560, 1440), true),
            ],
            &MonitorConfig::default(),
        );
        assert_eq!(monitor_at(&m, 100, 100), 0);
        assert_eq!(monitor_at(&m, 2000, 100), 1);
        // Outside every screen falls back to the first.
        assert_eq!(monitor_at(&m, 5000, 5000), 0);
    }

    #[test]
    fn screens_are_ordered_left_to_right() {
        let (m, _) = build(
            vec![
                ("HDMI-1".into(), rect(1920, 0, 2560, 1440), true),
                ("eDP-1".into(), rect(0, 0, 1920, 1080), true),
            ],
            &MonitorConfig::default(),
        );
        assert_eq!(m[0].name, "eDP-1");
        assert_eq!(m[1].name, "HDMI-1");
    }

    #[test]
    fn global_desktops_map_to_monitor_local_workspaces() {
        let (m, _) = build(
            vec![
                ("eDP-1".into(), rect(0, 0, 1000, 800), true),
                ("HDMI-1".into(), rect(1000, 0, 1000, 800), true),
            ],
            &MonitorConfig::default(),
        );
        assert_eq!(offset(&m, 0), 0);
        assert_eq!(offset(&m, 1), 10);
        assert_eq!(global_active(&m, 1), 10);
        assert_eq!(find_global(&m, 0), Some((0, 0)));
        assert_eq!(find_global(&m, 9), Some((0, 9)));
        assert_eq!(find_global(&m, 10), Some((1, 0)));
        assert_eq!(find_global(&m, 19), Some((1, 9)));
        assert_eq!(find_global(&m, 20), None);
    }

    #[test]
    fn primary_screen_follows_the_config() {
        let cfg = MonitorConfig {
            primary: Some("HDMI-1".into()),
            ..Default::default()
        };
        let (m, _) = build(
            vec![
                ("eDP-1".into(), rect(0, 0, 1920, 1080), true),
                ("HDMI-1".into(), rect(1920, 0, 2560, 1440), true),
            ],
            &cfg,
        );
        assert_eq!(primary(&m, &cfg), 1);
    }

    #[test]
    fn primary_falls_back_when_not_connected() {
        let cfg = MonitorConfig {
            primary: Some("DP-9".into()),
            ..Default::default()
        };
        let (m, _) = build(vec![("eDP-1".into(), rect(0, 0, 1920, 1080), true)], &cfg);
        assert_eq!(primary(&m, &cfg), 0);
    }

    #[test]
    fn no_outputs_means_no_monitors() {
        let (m, many) = build(vec![], &MonitorConfig::default());
        assert!(m.is_empty());
        assert!(!many);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use crate::layout::{Layout, Rect};

    fn two() -> Vec<Monitor> {
        let (m, _) = build(
            vec![
                (
                    "eDP-1".into(),
                    Rect {
                        x: 0,
                        y: 0,
                        w: 1000,
                        h: 800,
                    },
                    true,
                ),
                (
                    "HDMI-1".into(),
                    Rect {
                        x: 1000,
                        y: 0,
                        w: 1000,
                        h: 800,
                    },
                    true,
                ),
            ],
            &MonitorConfig::default(),
        );
        m
    }

    #[test]
    fn each_screen_tiles_inside_itself() {
        let m = two();
        for (i, monitor) in m.iter().enumerate() {
            let boxes = Layout::Tile.arrange_master(monitor.rect, 2, 1, 0.5, 8);
            for b in boxes {
                assert!(
                    b.x as i32 >= monitor.rect.x as i32
                        && (b.x as i32 + b.w as i32)
                            <= (monitor.rect.x as i32 + monitor.rect.w as i32),
                    "screen {i} box {b:?} escapes {:?}",
                    monitor.rect
                );
            }
        }
    }

    #[test]
    fn master_count_is_independent_per_screen() {
        let mut m = two();
        m[0].nmaster = 3;
        assert_eq!(m[0].nmaster, 3);
        assert_eq!(m[1].nmaster, 1, "the other screen keeps its own count");
    }

    #[test]
    fn workspaces_are_independent_per_screen() {
        let mut m = two();
        m[0].active = 3;
        m[1].active = 7;
        assert_eq!(m[0].active, 3);
        assert_eq!(m[1].active, 7);
        assert_ne!(m[0].active_name(), m[1].active_name());
    }

    #[test]
    fn a_window_follows_the_screen_under_the_pointer() {
        let m = two();
        // Pointer at x=1500 is on the second screen.
        assert_eq!(monitor_at(&m, 1500, 400), 1);
        // Pointer at x=200 is on the first.
        assert_eq!(monitor_at(&m, 200, 400), 0);
    }
}
