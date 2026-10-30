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

/// Tiling state for one workspace. Ratios and master counts are deliberately
/// not global: changing one workspace must leave every other workspace alone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkspaceState {
    pub layout: crate::layout::Layout,
    pub master_ratio: f32,
    pub nmaster: usize,
    pub scroll_columns: crate::layout::ScrollColumns,
    pub scroll_start: usize,
}

impl WorkspaceState {
    pub fn default_for(master_ratio: f32) -> Self {
        Self {
            layout: crate::layout::Layout::Tile,
            master_ratio,
            nmaster: 1,
            scroll_columns: crate::layout::ScrollColumns::Two,
            scroll_start: 0,
        }
    }
}

/// One physical screen.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    /// The RandR output name, e.g. `eDP-1` or `HDMI-1`.
    pub name: String,
    /// Where it sits in the combined coordinate space.
    pub rect: Rect,
    /// Names of its workspaces, in order.
    pub workspaces: Vec<String>,
    /// Which of `workspaces` is shown.
    pub active: usize,
    /// Tiling state for every workspace on this screen, in `workspaces` order.
    /// Each workspace owns its layout, master ratio, master count and Scroll
    /// viewport.
    pub states: Vec<WorkspaceState>,
    /// Default ratio for workspaces added later.
    default_master_ratio: f32,
}

impl Monitor {
    fn new(name: String, rect: Rect, count: usize, default_master_ratio: f32) -> Self {
        let workspaces: Vec<String> = (1..=count).map(|n| n.to_string()).collect();
        Self {
            name,
            rect,
            workspaces: workspaces.clone(),
            active: 0,
            states: vec![WorkspaceState::default_for(default_master_ratio); workspaces.len()],
            default_master_ratio,
        }
    }

    fn state(&self) -> WorkspaceState {
        self.states
            .get(self.active)
            .copied()
            .unwrap_or_else(|| WorkspaceState::default_for(self.default_master_ratio))
    }

    fn state_mut(&mut self) -> &mut WorkspaceState {
        self.ensure_state();
        let active = self.active.min(self.states.len().saturating_sub(1));
        &mut self.states[active]
    }

    /// The layout this screen's active workspace uses.
    pub fn layout(&self) -> crate::layout::Layout {
        self.state().layout
    }

    /// Give the active workspace another layout, leaving the others alone.
    pub fn set_layout(&mut self, layout: crate::layout::Layout) {
        self.state_mut().layout = layout;
    }

    pub fn default_ratio(&self) -> f32 {
        self.default_master_ratio
    }

    pub fn master_ratio(&self) -> f32 {
        self.state().master_ratio
    }

    pub fn nmaster(&self) -> usize {
        self.state().nmaster
    }

    pub fn set_master_ratio(&mut self, ratio: f32) {
        let ratio = (ratio.clamp(0.1, 0.9) * 100.0).round() / 100.0;
        self.state_mut().master_ratio = ratio;
    }

    pub fn change_master(&mut self, delta: i32) {
        let state = self.state_mut();
        state.nmaster = (state.nmaster as i32 + delta).max(1) as usize;
    }

    fn ensure_state(&mut self) {
        let count = self.workspaces.len().max(1);
        let default = self.default_master_ratio;
        self.states
            .resize_with(count, || WorkspaceState::default_for(default));
    }

    pub fn scroll_columns(&self) -> crate::layout::ScrollColumns {
        self.state().scroll_columns
    }

    pub fn set_scroll_columns(&mut self, mode: crate::layout::ScrollColumns) {
        self.state_mut().scroll_columns = mode;
    }

    /// Keep the focused column visible while moving the viewport as little as
    /// possible. Returns the start and mode for this frame.
    pub fn reveal_scroll(
        &mut self,
        focus: Option<usize>,
        n: usize,
        effective_columns: usize,
    ) -> (usize, crate::layout::ScrollColumns) {
        self.ensure_state();
        let state = self.state_mut();
        let mode = state.scroll_columns;
        let columns = effective_columns.clamp(1, 2).min(n.max(1));
        let max_start = n.saturating_sub(columns);
        let mut start = state.scroll_start.min(max_start);
        if let Some(focus) = focus {
            if focus < start {
                start = focus;
            } else if focus >= start.saturating_add(columns) {
                start = focus.saturating_add(1).saturating_sub(columns);
            }
        }
        start = start.min(max_start);
        state.scroll_start = start;
        (start, mode)
    }

    pub fn layout_label(&self) -> String {
        if self.layout() == crate::layout::Layout::Scroll {
            format!("scroll {}", self.scroll_columns().count())
        } else {
            self.layout().name().to_string()
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
    /// Default master ratio for new workspaces.
    pub master_ratio: f32,
    /// Common master ratios, in workspace order.
    pub master_ratios: Vec<f32>,
    /// Common master counts, in workspace order.
    pub nmasters: Vec<usize>,
    /// Per-screen master ratios, keyed by output name.
    pub screen_master_ratios: HashMap<String, Vec<f32>>,
    /// Per-screen master counts, keyed by output name.
    pub screen_nmasters: HashMap<String, Vec<usize>>,
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
            master_ratio: 0.55,
            master_ratios: Vec::new(),
            nmasters: Vec::new(),
            screen_master_ratios: HashMap::new(),
            screen_nmasters: HashMap::new(),
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
                    let mut monitor =
                        Monitor::new(name.clone(), rect, list.len(), cfg.master_ratio);
                    monitor.workspaces = list.clone();
                    monitor
                }
                _ => Monitor::new(name.clone(), rect, count, cfg.master_ratio),
            };
            // The common list is a default, not a snapshot of one screen: a
            // screen with its own list keeps it, and an absent common list
            // leaves every workspace tiled.
            for (i, layout) in cfg.layouts.iter().enumerate() {
                if i < monitor.states.len() {
                    monitor.states[i].layout = *layout;
                }
            }
            for (i, ratio) in cfg.master_ratios.iter().enumerate() {
                if i < monitor.states.len() {
                    monitor.states[i].master_ratio = ratio.clamp(0.1, 0.9);
                }
            }
            for (i, count) in cfg.nmasters.iter().enumerate() {
                if i < monitor.states.len() {
                    monitor.states[i].nmaster = (*count).max(1);
                }
            }
            if let Some(list) = cfg.screen_layouts.get(&name) {
                for (i, layout) in list.iter().enumerate() {
                    if i < monitor.states.len() {
                        monitor.states[i].layout = *layout;
                    }
                }
            }
            if let Some(list) = cfg.screen_master_ratios.get(&name) {
                for (i, ratio) in list.iter().enumerate() {
                    if i < monitor.states.len() {
                        monitor.states[i].master_ratio = ratio.clamp(0.1, 0.9);
                    }
                }
            }
            if let Some(list) = cfg.screen_nmasters.get(&name) {
                for (i, count) in list.iter().enumerate() {
                    if i < monitor.states.len() {
                        monitor.states[i].nmaster = (*count).max(1);
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
    fn master_count_is_independent_per_workspace_and_screen() {
        let mut m = two();
        m[0].change_master(2);
        assert_eq!(m[0].nmaster(), 3);
        assert_eq!(m[1].nmaster(), 1, "the other screen keeps its own count");
        m[0].active = 1;
        assert_eq!(m[0].nmaster(), 1, "the other workspace keeps its own count");
        m[0].set_master_ratio(0.7);
        assert_eq!(m[0].master_ratio(), 0.7);
        m[0].active = 0;
        assert_eq!(m[0].master_ratio(), 0.55);
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
    fn scroll_reveals_the_focused_column_with_minimal_movement() {
        let mut m = two();
        m[0].set_layout(Layout::Scroll);
        assert_eq!(m[0].layout_label(), "scroll 2");
        assert_eq!(
            m[0].reveal_scroll(Some(2), 4, 2),
            (1, crate::layout::ScrollColumns::Two)
        );
        assert_eq!(
            m[0].reveal_scroll(Some(1), 4, 2),
            (1, crate::layout::ScrollColumns::Two)
        );
        assert_eq!(
            m[0].reveal_scroll(Some(0), 4, 2),
            (0, crate::layout::ScrollColumns::Two)
        );
        assert_eq!(
            m[0].reveal_scroll(Some(3), 4, 2),
            (2, crate::layout::ScrollColumns::Two)
        );
        m[0].set_scroll_columns(crate::layout::ScrollColumns::One);
        assert_eq!(m[0].layout_label(), "scroll 1");
        assert_eq!(
            m[0].reveal_scroll(Some(3), 4, 1),
            (3, crate::layout::ScrollColumns::One)
        );
        m[0].set_scroll_columns(crate::layout::ScrollColumns::Two);
        assert_eq!(
            m[0].reveal_scroll(Some(1), 1, 2),
            (0, crate::layout::ScrollColumns::Two)
        );
    }

    #[test]
    fn scroll_mode_is_per_workspace() {
        let mut m = two();
        m[0].set_layout(Layout::Scroll);
        m[0].set_scroll_columns(crate::layout::ScrollColumns::One);
        m[0].active = 1;
        assert_eq!(m[0].scroll_columns(), crate::layout::ScrollColumns::Two);
        m[0].active = 0;
        assert_eq!(m[0].scroll_columns(), crate::layout::ScrollColumns::One);
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
