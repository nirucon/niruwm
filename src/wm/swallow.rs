//! Window swallowing, as in dwm's swallow patch.
//!
//! When a window maps whose process descends from a terminal window's
//! process (`mpv film.mkv`, `zathura doc.pdf`, `feh .` typed in kitty), the
//! new window takes the terminal's place: the same screen and workspace, the
//! same position in the tiling order, and the terminal's rectangle when the
//! terminal floats. The terminal is hidden and is not part of any workspace,
//! window list or focus order until that window closes; then it comes back
//! where the window was and gets the focus again.
//!
//! The process is found with `_NET_WM_PID`, or with the X-Resource
//! extension's client PID for programs that do not set it (plain Xt and Xaw
//! programs such as xclock). The parent chain is read from `/proc`.
//!
//! Never swallowed: dialogs and other transient or utility windows,
//! override-redirect windows (never managed), scratchpads, windows a rule or
//! `_NET_WM_DESKTOP` sends to a workspace, windows restored by the session,
//! terminals themselves, and the classes in `[swallow] exclude`. A terminal
//! that is a scratchpad, sticky, or already swallowed is never taken.
//!
//! A restart after a crash: the X server maps every window of the old
//! manager's save-set when it dies, so a swallowed terminal never stays
//! invisible. The terminal carries `_NIRUWM_SWALLOWED_BY` (the window in its
//! place), and the new manager hides it again when that window is still
//! there; otherwise the terminal is an ordinary window.
use super::*;

/// The parent of a process, from `/proc/PID/stat`.
pub(crate) fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name is in parentheses and may itself contain spaces or
    // parentheses; the fixed fields follow the last ')'.
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// The ancestors of `pid`, nearest first (not `pid` itself), ending before
/// init. Bounded, so a broken table cannot loop.
pub(crate) fn ancestors(pid: u32, parent: impl Fn(u32) -> Option<u32>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut current = pid;
    for _ in 0..64 {
        match parent(current) {
            Some(p) if p > 1 && p != current && p != pid && !out.contains(&p) => {
                out.push(p);
                current = p;
            }
            _ => break,
        }
    }
    out
}

/// The terminal to swallow: the nearest ancestor of `pid` that owns one of
/// `terminals` (window, pid).
pub(crate) fn nearest_terminal(
    pid: u32,
    terminals: &[(Window, u32)],
    parent: impl Fn(u32) -> Option<u32>,
) -> Option<Window> {
    ancestors(pid, parent)
        .into_iter()
        .find_map(|a| terminals.iter().find(|(_, p)| *p == a).map(|(w, _)| *w))
}

/// The switch a crashed manager left on the root window.
pub(crate) fn published_switch(wm: &Wm) -> Option<bool> {
    let reply =
        wm.c.get_property(
            false,
            wm.root,
            wm.atom("_NIRUWM_SWALLOW"),
            AtomEnum::ANY,
            0,
            16,
        )
        .ok()?
        .reply()
        .ok()?;
    match String::from_utf8_lossy(&reply.value).trim() {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

impl Wm {
    /// The process behind a window: `_NET_WM_PID`, else the X-Resource
    /// client PID (local clients only, which is every terminal child).
    pub(crate) fn window_pid(&self, w: Window) -> Option<u32> {
        if let Some(pid) = self
            .values(w, "_NET_WM_PID")
            .first()
            .copied()
            .filter(|p| *p > 0)
        {
            return Some(pid);
        }
        use x11rb::protocol::res::{ClientIdMask, ClientIdSpec, ConnectionExt as _};
        let spec = ClientIdSpec {
            client: w,
            mask: ClientIdMask::LOCAL_CLIENT_PID,
        };
        let reply = self.c.res_query_client_ids(&[spec]).ok()?.reply().ok()?;
        reply
            .ids
            .iter()
            .find(|id| id.spec.mask.contains(ClientIdMask::LOCAL_CLIENT_PID))
            .and_then(|id| id.value.first().copied())
            .filter(|p| *p > 0)
    }

    /// The window that swallowed terminal `w`, if it is swallowed.
    pub(crate) fn swallower_of(&self, w: Window) -> Option<Window> {
        self.clients
            .iter()
            .find(|c| c.win == w)
            .and_then(|c| c.swallowed_by)
    }

    /// The terminal a new window of `class` and process `pid` takes the place
    /// of, when swallowing is on and the window may swallow.
    pub(crate) fn swallow_target(&self, class: &str, pid: Option<u32>) -> Option<Window> {
        if !self.swallow_enabled {
            return None;
        }
        let cfg = &self.cfg.swallow;
        if cfg.is_terminal(class) || cfg.is_excluded(class) {
            return None;
        }
        let pid = pid?;
        let terminals: Vec<(Window, u32)> = self
            .clients
            .iter()
            .filter(|c| {
                c.swallowed_by.is_none()
                    && c.scratch.is_none()
                    && c.parent.is_none()
                    && !c.sticky
                    && !self.withdrawn.contains(&c.win)
                    && self.scratch_owner(c.win).is_none()
                    && cfg.is_terminal(&c.class)
            })
            .filter_map(|c| Some((c.win, c.pid.or_else(|| self.window_pid(c.win))?)))
            .collect();
        if terminals.is_empty() {
            return None;
        }
        nearest_terminal(pid, &terminals, parent_pid)
    }

    /// Hide terminal `t` behind the new window `w`, which was just inserted
    /// in the terminal's place.
    pub(crate) fn swallow(&mut self, w: Window, t: Window) {
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == t) {
            c.swallowed_by = Some(w);
        }
        if self.focus == Some(t) {
            self.focus = None;
        }
        let atom = self.atom("_NIRUWM_SWALLOWED_BY");
        let _ = self
            .c
            .change_property32(PropMode::REPLACE, t, atom, AtomEnum::WINDOW, &[w]);
    }

    /// Bring a swallowed terminal back. It is placed directly after the window
    /// that swallowed it, on that window's screen and workspace.
    fn release(&mut self, t: Window) {
        let Some(index) = self.clients.iter().position(|c| c.win == t) else {
            return;
        };
        let Some(owner) = self.clients[index].swallowed_by.take() else {
            return;
        };
        let atom = self.atom("_NIRUWM_SWALLOWED_BY");
        let _ = self.c.delete_property(t, atom);
        if let Some(at) = self.clients.iter().position(|c| c.win == owner) {
            let terminal = self.clients.remove(index);
            let at = if index < at { at - 1 } else { at };
            self.clients
                .insert((at + 1).min(self.clients.len()), terminal);
        }
    }

    /// The window `w` is going away: the terminal it swallowed takes its
    /// place in the client list (call before removing `w`). Returns the
    /// terminal.
    pub(crate) fn unswallow_for_closed(&mut self, w: Window) -> Option<Window> {
        let t = self
            .clients
            .iter()
            .find(|c| c.swallowed_by == Some(w))
            .map(|c| c.win)?;
        let place = self
            .clients
            .iter()
            .find(|c| c.win == w)
            .map(|c| (c.monitor, c.monitor_name.clone(), c.tag));
        self.release(t);
        if let (Some((monitor, name, tag)), Some(c)) =
            (place, self.clients.iter_mut().find(|c| c.win == t))
        {
            c.monitor = monitor;
            c.monitor_name = name;
            c.tag = tag;
        }
        eprintln!("niruwm: window {w:#x} closed; terminal {t:#x} is back");
        Some(t)
    }

    /// A swallowed terminal goes wherever the window in its place goes, so
    /// it comes back there and `_NET_WM_DESKTOP` (which a restarted manager
    /// reads) stays true. Runs at the start of every arrange().
    pub(crate) fn settle_swallowed(&mut self) {
        let pairs: Vec<(usize, Window)> = self
            .clients
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.swallowed_by.map(|s| (i, s)))
            .collect();
        for (index, owner) in pairs {
            let place = self
                .clients
                .iter()
                .find(|c| c.win == owner)
                .map(|c| (c.monitor, c.monitor_name.clone(), c.tag));
            match place {
                Some((monitor, name, tag)) => {
                    let c = &mut self.clients[index];
                    c.monitor = monitor;
                    c.monitor_name = name;
                    c.tag = tag;
                }
                // The window is gone without an unmanage (should not
                // happen): never leave the terminal hidden.
                None => {
                    let t = self.clients[index].win;
                    self.release(t);
                }
            }
        }
    }

    /// After a restart: hide again the terminals whose window is still there.
    pub(crate) fn relink_swallowed(&mut self) {
        for (t, owner) in std::mem::take(&mut self.swallow_adopted) {
            // A restart in place links the terminal while adopting it, so it
            // is never shown in between; that link is confirmed here.
            let both = self.clients.iter().any(|c| c.win == owner)
                && self
                    .clients
                    .iter()
                    .any(|c| c.win == t && c.swallowed_by.map_or(true, |o| o == owner));
            if both && owner != t && self.swallow_enabled {
                self.swallow(owner, t);
                eprintln!("niruwm: terminal {t:#x} stays swallowed by {owner:#x}");
            } else {
                if let Some(c) = self
                    .clients
                    .iter_mut()
                    .find(|c| c.win == t && c.swallowed_by == Some(owner))
                {
                    c.swallowed_by = None;
                }
                let atom = self.atom("_NIRUWM_SWALLOWED_BY");
                let _ = self.c.delete_property(t, atom);
            }
        }
    }

    /// Turn swallowing on or off at run time. Off brings every swallowed
    /// terminal back at once, next to the window that had its place.
    pub(crate) fn set_swallow(&mut self, on: bool) {
        self.swallow_enabled = on;
        if !on {
            let swallowed: Vec<Window> = self
                .clients
                .iter()
                .filter(|c| c.swallowed_by.is_some())
                .map(|c| c.win)
                .collect();
            self.settle_swallowed();
            for t in swallowed {
                self.release(t);
            }
        }
        self.publish_swallow();
    }

    /// `_NIRUWM_SWALLOW` on the root: "on" or "off", for `niructl swallow
    /// status` and for a manager started again after a crash.
    pub(crate) fn publish_swallow(&self) {
        let text = if self.swallow_enabled { "on" } else { "off" };
        let _ = self.c.change_property8(
            PropMode::REPLACE,
            self.root,
            self.atom("_NIRUWM_SWALLOW"),
            self.atom("UTF8_STRING"),
            text.as_bytes(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(pairs: &[(u32, u32)]) -> impl Fn(u32) -> Option<u32> + '_ {
        move |pid| pairs.iter().find(|(c, _)| *c == pid).map(|(_, p)| *p)
    }

    #[test]
    fn the_parent_chain_stops_before_init_and_never_loops() {
        let parents = [(40, 30), (30, 20), (20, 1)];
        assert_eq!(ancestors(40, table(&parents)), vec![30, 20]);
        let cycle = [(5, 6), (6, 5)];
        assert_eq!(ancestors(5, table(&cycle)), vec![6]);
        assert!(ancestors(9, table(&parents)).is_empty());
    }

    #[test]
    fn the_nearest_terminal_swallows() {
        // kitty (10) -> fish (11) -> kitty (12) -> fish (13) -> mpv (14)
        let parents = [(14, 13), (13, 12), (12, 11), (11, 10), (10, 1)];
        let terminals = [(0xa, 10), (0xb, 12)];
        assert_eq!(nearest_terminal(14, &terminals, table(&parents)), Some(0xb));
        // A process outside every terminal swallows nothing.
        assert_eq!(nearest_terminal(99, &terminals, table(&parents)), None);
        // The terminal's own process is not its own child: a second window
        // of the terminal itself never swallows the first.
        assert_eq!(nearest_terminal(10, &terminals, table(&parents)), None);
    }

    #[test]
    fn a_command_with_parentheses_still_has_its_parent_read() {
        // /proc/self/stat is always there; its parent is this test runner's.
        let me = std::process::id();
        let parent = parent_pid(me).expect("own parent");
        assert!(parent > 0);
        assert_eq!(
            ancestors(me, parent_pid).first().copied(),
            Some(parent).filter(|p| *p > 1)
        );
    }

    #[test]
    fn swallow_classes_match_either_part_in_any_case() {
        let cfg = crate::config::SwallowConfig::default();
        assert!(cfg.enabled, "on when [swallow] is absent");
        assert!(cfg.is_terminal("xterm\0XTerm\0"));
        assert!(cfg.is_terminal("kitty\0kitty\0"));
        assert!(cfg.is_terminal("Alacritty\0Alacritty\0"));
        assert!(!cfg.is_terminal("mpv\0mpv\0"));
        assert!(cfg.is_excluded("Navigator\0firefox\0"));
        assert!(cfg.is_excluded("xev\0Xev\0"));
        assert!(!cfg.is_excluded("mpv\0mpv\0"));
        let own = crate::config::SwallowConfig {
            terminals: vec!["myterm".into()],
            exclude: vec!["MPV".into()],
            ..Default::default()
        };
        assert!(own.is_terminal("myterm\0MyTerm\0"));
        assert!(
            !own.is_terminal("kitty\0kitty\0"),
            "a list replaces the defaults"
        );
        assert!(own.is_excluded("mpv\0mpv\0"));
    }
}
