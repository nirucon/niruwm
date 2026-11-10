//! Restart in place: replace the running window manager with the installed
//! binary without ending the session.
//!
//! `niructl restart-wm` asks for it (`_NIRUWM_COMMAND` 22). The manager
//! writes what only it knows (each window's screen, workspace, floating and
//! fullscreen state, the order of the tiles, every workspace's layout, the
//! focus and the shown scratchpad) to a small private file, takes the windows
//! it had hidden out of its save-set so the X server does not map them when
//! the connection closes (that would flash every hidden window on the
//! screen), and exits with `RESTART_STATUS`. `niruwm-session` sees that
//! status, restarts the session helpers whose binary changed and runs the
//! installed `niruwm` again, which adopts the windows and reads the file.
//!
//! A manager is only offered this when the session script says it can take
//! the status (`NIRUWM_SESSION_RESTART=1`). Older session scripts treat any
//! unknown status as a crash, which also keeps the windows, but restarts no
//! helpers and counts towards the crash limit.

use super::{Client, Home, Wm};
use crate::layout::{Layout, Rect, ScrollColumns};
use crate::monitor::WorkspaceState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

/// The exit status that means "run me again in this session".
pub(crate) const RESTART_STATUS: i32 = 75;

/// A state file older than this is from an earlier, abandoned restart.
const MAX_AGE_SECS: u64 = 300;

const SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedClient {
    pub win: u32,
    pub monitor: String,
    pub monitor_index: usize,
    pub tag: u32,
    pub floating: bool,
    pub fullscreen: bool,
    pub sticky: bool,
    /// Outer rectangle: x, y, w, h.
    pub geom: [i32; 4],
    /// Mapped when the state was written. Hidden windows stay unmapped
    /// across the restart and are adopted from this list.
    pub mapped: bool,
    #[serde(default)]
    pub scratch: Option<u8>,
    #[serde(default)]
    pub scratch_floating: bool,
    #[serde(default)]
    pub home: Option<(String, u32, String)>,
    #[serde(default)]
    pub urgent: bool,
    /// A swallowed terminal: the window in its place.
    #[serde(default)]
    pub swallowed_by: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedWorkspace {
    pub layout: String,
    pub master_ratio: f32,
    pub nmaster: usize,
    pub scroll_columns: u32,
    pub scroll_start: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedMonitor {
    pub name: String,
    pub active: usize,
    pub states: Vec<SavedWorkspace>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RestartState {
    pub schema: u32,
    pub display: String,
    pub saved_at: u64,
    pub version: String,
    pub clients: Vec<SavedClient>,
    pub monitors: Vec<SavedMonitor>,
    pub focused_monitor: String,
    pub focus: Option<u32>,
    pub scratch_shown: Option<u32>,
    pub scratch_return: Option<u32>,
    pub bar_visible: bool,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn display_name() -> String {
    std::env::var("DISPLAY").unwrap_or_default()
}

/// `$XDG_RUNTIME_DIR/niruwm/restart-<display>.json`: private, per display,
/// gone at reboot.
pub(crate) fn path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache")
                })
        });
    let display: String = display_name()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    base.join("niruwm").join(format!("restart-{display}.json"))
}

fn workspace_to_saved(s: &WorkspaceState) -> SavedWorkspace {
    SavedWorkspace {
        layout: s.layout.name().to_string(),
        master_ratio: s.master_ratio,
        nmaster: s.nmaster,
        scroll_columns: s.scroll_columns.count() as u32,
        scroll_start: s.scroll_start,
    }
}

fn saved_to_workspace(s: &SavedWorkspace, fallback: WorkspaceState) -> WorkspaceState {
    WorkspaceState {
        layout: Layout::all()
            .iter()
            .copied()
            .find(|l| l.name() == s.layout)
            .unwrap_or(fallback.layout),
        master_ratio: if s.master_ratio.is_finite() && s.master_ratio > 0.0 && s.master_ratio < 1.0
        {
            s.master_ratio
        } else {
            fallback.master_ratio
        },
        nmaster: s.nmaster.min(64),
        scroll_columns: ScrollColumns::from_count(s.scroll_columns)
            .unwrap_or(fallback.scroll_columns),
        scroll_start: s.scroll_start,
    }
}

pub(crate) fn write(state: &RestartState) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let tmp = path.with_extension("json.new");
    let text = serde_json::to_vec(state).map_err(std::io::Error::other)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(&text)?;
    file.sync_all()?;
    std::fs::rename(&tmp, &path)
}

/// The state a restart left for this display, if it is recent and ours.
pub(crate) fn read() -> Option<RestartState> {
    let path = path();
    let text = std::fs::read(&path).ok()?;
    let state: RestartState = match serde_json::from_slice(&text) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("niruwm: restart state unreadable ({e}); adopting windows without it");
            let _ = std::fs::remove_file(&path);
            return None;
        }
    };
    usable(&state, &display_name(), now_secs()).then_some(state)
}

fn usable(state: &RestartState, display: &str, now: u64) -> bool {
    state.schema == SCHEMA
        && state.display == display
        && now.saturating_sub(state.saved_at) <= MAX_AGE_SECS
}

pub(crate) fn remove() {
    let _ = std::fs::remove_file(path());
}

/// Whether this manager may offer an in-place restart: only under a session
/// script that knows `RESTART_STATUS`.
pub(crate) fn session_supports_restart() -> bool {
    std::env::var_os("NIRUWM_SESSION_RESTART").is_some_and(|v| v == "1")
}

impl Wm {
    /// Not on the screen now, and must not appear during a restart: hidden by
    /// this manager (another workspace, a hidden scratchpad), withdrawn by
    /// its client, or a terminal a window has swallowed.
    fn kept_unmapped(&self, c: &Client) -> bool {
        self.hidden.contains(&c.win) || self.withdrawn.contains(&c.win) || c.swallowed_by.is_some()
    }

    /// Capture the state for the next manager. Pure: no X requests.
    pub(crate) fn restart_state(&self) -> RestartState {
        let clients = self
            .clients
            .iter()
            .map(|c| SavedClient {
                win: c.win,
                monitor: self
                    .monitors
                    .get(c.monitor)
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| c.monitor_name.clone()),
                monitor_index: c.monitor,
                tag: c.tag,
                floating: c.floating,
                fullscreen: c.fullscreen,
                sticky: c.sticky,
                geom: [
                    i32::from(c.geom.x),
                    i32::from(c.geom.y),
                    i32::from(c.geom.w),
                    i32::from(c.geom.h),
                ],
                mapped: !self.kept_unmapped(c),
                scratch: c.scratch,
                scratch_floating: c.scratch_floating,
                home: c
                    .home
                    .as_ref()
                    .map(|h| (h.output.clone(), h.tag, h.moved_to.clone())),
                urgent: c.urgent,
                swallowed_by: c.swallowed_by,
            })
            .collect();
        let monitors = self
            .monitors
            .iter()
            .map(|m| SavedMonitor {
                name: m.name.clone(),
                active: m.active,
                states: m.states.iter().map(workspace_to_saved).collect(),
            })
            .collect();
        RestartState {
            schema: SCHEMA,
            display: display_name(),
            saved_at: now_secs(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            clients,
            monitors,
            focused_monitor: self
                .monitors
                .get(self.focused_monitor)
                .map(|m| m.name.clone())
                .unwrap_or_default(),
            focus: self.focus,
            scratch_shown: self.scratch_shown,
            scratch_return: self.scratch_return,
            bar_visible: self.bar_visible,
        }
    }

    /// Prepare to hand the session to a new manager. Returns false (and
    /// changes nothing) when the restart must not happen now.
    pub(crate) fn prepare_restart(&mut self) -> bool {
        if self.keys_released {
            eprintln!("niruwm: restart refused: the screen is locked");
            return false;
        }
        let state = self.restart_state();
        if let Err(e) = write(&state) {
            eprintln!("niruwm: restart refused: cannot save the window state: {e}");
            return false;
        }
        // A window this manager hid (another workspace, a hidden
        // scratchpad, a swallowed terminal) or that its client withdrew must
        // stay unmapped when the connection closes. The save-set would map
        // all of them for a moment otherwise.
        for c in &self.clients {
            if self.kept_unmapped(c) {
                let _ = self.c.change_save_set(SetMode::DELETE, c.win);
            }
        }
        let _ = self.c.flush();
        eprintln!(
            "niruwm: restart in place requested; {} windows saved",
            state.clients.len()
        );
        self.restart_requested = true;
        true
    }

    /// Apply the screens' workspaces and layouts from a restart, before the
    /// windows are adopted, so adopting never shows the wrong workspace.
    pub(crate) fn apply_restart_monitors(&mut self, state: &RestartState) {
        for saved in &state.monitors {
            let Some(m) = self.monitors.iter_mut().find(|m| m.name == saved.name) else {
                continue;
            };
            for (i, s) in saved.states.iter().enumerate() {
                if let Some(slot) = m.states.get_mut(i) {
                    *slot = saved_to_workspace(s, *slot);
                }
            }
            if saved.active < m.workspaces.len() {
                m.active = saved.active;
            }
        }
        if let Some(i) = self
            .monitors
            .iter()
            .position(|m| m.name == state.focused_monitor)
        {
            self.focused_monitor = i;
        }
        self.layout = self.layout();
        self.bar_visible = state.bar_visible;
    }

    /// Windows to adopt after a restart: what the X server shows, plus the
    /// hidden windows the previous manager left unmapped, in the saved order.
    pub(crate) fn restart_candidates(
        &self,
        state: Option<&RestartState>,
        viewable: Vec<Window>,
    ) -> Vec<Window> {
        let Some(state) = state else {
            return viewable;
        };
        let mut out = Vec::new();
        for saved in &state.clients {
            if viewable.contains(&saved.win) {
                out.push(saved.win);
                continue;
            }
            if saved.mapped {
                continue; // gone, or unmapped by its client meanwhile
            }
            let alive = self
                .c
                .get_window_attributes(saved.win)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some_and(|a| !a.override_redirect && a.map_state == MapState::UNMAPPED);
            if alive {
                out.push(saved.win);
            }
        }
        for w in viewable {
            if !out.contains(&w) {
                out.push(w);
            }
        }
        out
    }

    /// Put an adopted window back exactly as the previous manager had it.
    /// Returns whether the window should stay unmapped (it was hidden).
    pub(crate) fn apply_restart_client(&mut self, w: Window, saved: &SavedClient) -> bool {
        let monitor = self
            .monitors
            .iter()
            .position(|m| m.name == saved.monitor)
            .unwrap_or_else(|| {
                saved
                    .monitor_index
                    .min(self.monitors.len().saturating_sub(1))
            });
        let monitor_name = self
            .monitors
            .get(monitor)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        let workspaces = self
            .monitors
            .get(monitor)
            .map(|m| m.workspaces.len() as u32)
            .unwrap_or(9)
            .max(1);
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.monitor = monitor;
            c.monitor_name = monitor_name;
            c.tag = saved.tag.min(workspaces - 1);
            c.floating = saved.floating;
            c.fullscreen = saved.fullscreen;
            c.sticky = saved.sticky;
            c.scratch_floating = saved.scratch_floating;
            c.urgent = saved.urgent;
            // Linked at once, so no arrange during adoption shows the
            // terminal; relink_swallowed() confirms or undoes it.
            if c.swallowed_by.is_none() {
                c.swallowed_by = saved.swallowed_by;
            }
            c.home = saved.home.as_ref().map(|(output, tag, moved_to)| Home {
                output: output.clone(),
                tag: *tag,
                moved_to: moved_to.clone(),
            });
            if saved.floating || saved.scratch.is_some() {
                c.geom = rect(saved.geom, c.geom);
            }
        }
        if saved.mapped && saved.scratch.is_some() {
            // The scratchpad that was up stays up: marking it shown before
            // the first arrange keeps it from being hidden and shown again.
            self.scratch_shown = Some(w);
        }
        !saved.mapped
    }

    /// After every window is adopted: the tiling order, the focus and the
    /// scratchpad that was shown.
    pub(crate) fn finish_restart(&mut self, state: &RestartState) -> super::Result<()> {
        let order: HashMap<u32, usize> = state
            .clients
            .iter()
            .enumerate()
            .map(|(i, c)| (c.win, i))
            .collect();
        self.clients
            .sort_by_key(|c: &Client| order.get(&c.win).copied().unwrap_or(usize::MAX));
        let known = |w: &Option<u32>, wm: &Wm| w.filter(|w| wm.clients.iter().any(|c| c.win == *w));
        if let Some(s) = known(&state.scratch_shown, self).filter(|s| {
            self.clients
                .iter()
                .any(|c| c.win == *s && c.scratch.is_some())
        }) {
            self.show_scratch(s)?;
        }
        self.scratch_return = known(&state.scratch_return, self);
        // `announce()` published the layout before the screens were restored.
        self.publish_layout_readonly()?;
        if let Some(f) = known(&state.focus, self) {
            self.focus = Some(f);
            self.focus_dirty = true;
        }
        eprintln!(
            "niruwm: restarted in place from NIRUWM {}; {} of {} windows adopted",
            state.version,
            self.clients
                .iter()
                .filter(|c| order.contains_key(&c.win))
                .count(),
            state.clients.len()
        );
        Ok(())
    }
}

fn rect(g: [i32; 4], fallback: Rect) -> Rect {
    let (Ok(x), Ok(y), Ok(w), Ok(h)) = (
        i16::try_from(g[0]),
        i16::try_from(g[1]),
        u16::try_from(g[2]),
        u16::try_from(g[3]),
    ) else {
        return fallback;
    };
    if w == 0 || h == 0 {
        return fallback;
    }
    Rect { x, y, w, h }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RestartState {
        RestartState {
            schema: SCHEMA,
            display: ":3".into(),
            saved_at: 1000,
            version: "0.23.0".into(),
            clients: vec![SavedClient {
                win: 0x400001,
                monitor: "HDMI-1".into(),
                monitor_index: 1,
                tag: 4,
                floating: true,
                fullscreen: false,
                sticky: false,
                geom: [10, 20, 300, 200],
                mapped: false,
                scratch: Some(2),
                scratch_floating: false,
                home: None,
                urgent: true,
                swallowed_by: None,
            }],
            monitors: vec![SavedMonitor {
                name: "HDMI-1".into(),
                active: 4,
                states: vec![SavedWorkspace {
                    layout: "scroll".into(),
                    master_ratio: 0.6,
                    nmaster: 2,
                    scroll_columns: 1,
                    scroll_start: 3,
                }],
            }],
            focused_monitor: "HDMI-1".into(),
            focus: Some(0x400001),
            scratch_shown: None,
            scratch_return: None,
            bar_visible: false,
        }
    }

    #[test]
    fn state_round_trips_through_json() {
        let state = sample();
        let text = serde_json::to_string(&state).unwrap();
        let back: RestartState = serde_json::from_str(&text).unwrap();
        assert_eq!(back, state);
    }

    #[test]
    fn only_a_fresh_state_for_this_display_is_used() {
        let state = sample();
        assert!(usable(&state, ":3", 1000 + MAX_AGE_SECS));
        assert!(!usable(&state, ":3", 1001 + MAX_AGE_SECS));
        assert!(!usable(&state, ":0", 1000));
        let mut old = state.clone();
        old.schema = 0;
        assert!(!usable(&old, ":3", 1000));
    }

    #[test]
    fn workspace_state_survives_and_bad_values_fall_back() {
        let fallback = WorkspaceState::default_for(0.55);
        let saved = workspace_to_saved(&WorkspaceState {
            layout: Layout::Scroll,
            master_ratio: 0.7,
            nmaster: 3,
            scroll_columns: ScrollColumns::One,
            scroll_start: 2,
        });
        let back = saved_to_workspace(&saved, fallback);
        assert_eq!(back.layout, Layout::Scroll);
        assert_eq!(back.nmaster, 3);
        assert_eq!(back.scroll_columns, ScrollColumns::One);
        assert_eq!(back.scroll_start, 2);
        let broken = SavedWorkspace {
            layout: "nonsense".into(),
            master_ratio: f32::NAN,
            nmaster: 1,
            scroll_columns: 7,
            scroll_start: 0,
        };
        let back = saved_to_workspace(&broken, fallback);
        assert_eq!(back.layout, fallback.layout);
        assert_eq!(back.master_ratio, 0.55);
        assert_eq!(back.scroll_columns, fallback.scroll_columns);
    }

    #[test]
    fn a_broken_rectangle_keeps_the_current_one() {
        let now = Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        };
        assert_eq!(rect([0, 0, 0, 10], now), now);
        assert_eq!(rect([70000, 0, 10, 10], now), now);
        assert_eq!(
            rect([5, 6, 7, 8], now),
            Rect {
                x: 5,
                y: 6,
                w: 7,
                h: 8
            }
        );
    }

    #[test]
    fn the_file_is_per_display_and_private_by_location() {
        let p = path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("restart-") && name.ends_with(".json"));
        assert!(!name.contains(':'));
    }
}
