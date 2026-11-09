//! Urgency: windows that ask for attention.
//!
//! A window asks with the ICCCM urgency bit in `WM_HINTS` (a terminal bell, a
//! chat message) or with `_NET_WM_STATE_DEMANDS_ATTENTION`. NIRUWM marks it
//! the way dwm does: the window that has the focus is never marked (the
//! request is answered at once and its hints are cleared), and a marked
//! window loses the mark, and its hints, as soon as it gets the focus.
//!
//! The marked windows are published on the root window in `_NIRUWM_URGENT`
//! (WINDOW list, oldest request first), so each NIRUBAR colours the
//! workspace cell of its own screen that holds one; a scratchpad has
//! `_NET_WM_DESKTOP` 0xFFFFFFFF and is shown by the bar's scratchpad mark.
//! Super+Shift+U (`focus_urgent`, `niructl urgent focus`) goes to the most
//! recent one: its screen and workspace, or its scratchpad.
use super::*;

impl Wm {
    /// Whether the window's own hints ask for attention.
    pub(crate) fn wants_attention(&self, w: Window) -> bool {
        self.hint_urgent(w)
            || self
                .values(w, "_NET_WM_STATE")
                .contains(&self.atom("_NET_WM_STATE_DEMANDS_ATTENTION"))
    }

    fn hint_urgent(&self, w: Window) -> bool {
        x11rb::properties::WmHints::get(&self.c, w)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .flatten()
            .is_some_and(|hints| hints.urgent)
    }

    /// Read a window's attention hints again (`WM_HINTS` or `_NET_WM_STATE`
    /// changed) and mark or unmark it.
    pub(crate) fn note_attention(&mut self, w: Window) -> Result<()> {
        if !self.clients.iter().any(|c| c.win == w) {
            return Ok(());
        }
        let mut wants = self.wants_attention(w);
        if wants && self.focus == Some(w) {
            // The user is already looking at it.
            self.clear_attention_hints(w)?;
            wants = false;
        }
        self.mark_urgent(w, wants)
    }

    /// The window got the focus: it no longer needs attention.
    pub(crate) fn clear_urgent_on_focus(&mut self, w: Window) -> Result<()> {
        if !self.clients.iter().any(|c| c.win == w && c.urgent) {
            return Ok(());
        }
        self.clear_attention_hints(w)?;
        self.mark_urgent(w, false)
    }

    /// Clear the urgency bit and `_NET_WM_STATE_DEMANDS_ATTENTION`, as dwm
    /// does when it focuses an urgent window, so the client and pagers agree.
    fn clear_attention_hints(&mut self, w: Window) -> Result<()> {
        if let Some(mut hints) = x11rb::properties::WmHints::get(&self.c, w)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .flatten()
        {
            if hints.urgent {
                hints.urgent = false;
                hints.set(&self.c, w)?;
            }
        }
        let attention = self.atom("_NET_WM_STATE_DEMANDS_ATTENTION");
        let state = self.values(w, "_NET_WM_STATE");
        if state.contains(&attention) {
            let state = super::events::next_state_atoms(&state, attention, false);
            self.prop(w, "_NET_WM_STATE", AtomEnum::ATOM.into(), &state)?;
        }
        Ok(())
    }

    /// Set the mark itself; publish and recolour only on a change.
    fn mark_urgent(&mut self, w: Window, urgent: bool) -> Result<()> {
        let serial = self.urgent_counter + 1;
        let Some(c) = self.clients.iter_mut().find(|c| c.win == w) else {
            return Ok(());
        };
        if c.urgent == urgent {
            return Ok(());
        }
        c.urgent = urgent;
        if urgent {
            c.urgent_serial = serial;
            self.urgent_counter = serial;
        }
        self.update_borders()?;
        self.publish_urgent()?;
        self.c.flush()?;
        Ok(())
    }

    /// The marked windows, oldest request first. A window that withdrew
    /// itself or a swallowed terminal cannot be shown, so neither is listed.
    pub(crate) fn urgent_windows(&self) -> Vec<Window> {
        let mut marked: Vec<(u64, Window)> = self
            .clients
            .iter()
            .filter(|c| c.urgent && c.swallowed_by.is_none() && !self.withdrawn.contains(&c.win))
            .map(|c| (c.urgent_serial, c.win))
            .collect();
        marked.sort_unstable();
        marked.into_iter().map(|(_, w)| w).collect()
    }

    pub(crate) fn publish_urgent(&mut self) -> Result<()> {
        let list = self.urgent_windows();
        self.publish_root32("_NIRUWM_URGENT", AtomEnum::WINDOW.into(), &list)
    }

    /// Super+Shift+U: go to the window that asked for attention most
    /// recently.
    pub(crate) fn focus_urgent(&mut self) -> Result<()> {
        match self.urgent_windows().last().copied() {
            Some(w) => self.activate(w),
            None => Ok(()),
        }
    }

    /// Show a window wherever it is and focus it: its screen and workspace,
    /// or its scratchpad on the focused screen. `_NET_ACTIVE_WINDOW` from a
    /// pager or a notification and the attention key both land here.
    pub(crate) fn activate(&mut self, w: Window) -> Result<()> {
        // A swallowed terminal is shown through the window in its place.
        let w = self.swallower_of(w).unwrap_or(w);
        // Activating a scratchpad (a window switcher, a notification) shows
        // it where the user is instead of switching workspace.
        if self.is_scratch(w) {
            if self.scratch_shown != Some(w) {
                self.show_scratch(w)?;
            }
            return self.arrange();
        }
        // A dialog of a hidden scratchpad brings the scratchpad up with it.
        if let Some(owner) = self.scratch_owner(w) {
            if self.scratch_shown != Some(owner) {
                self.show_scratch(owner)?;
            }
            self.focus = Some(w);
            self.focus_dirty = true;
            return self.arrange();
        }
        if let Some(c) = self.clients.iter().find(|c| c.win == w) {
            let (monitor, tag) = (c.monitor, c.tag);
            self.focused_monitor = monitor;
            if let Some(m) = self.monitors.get_mut(monitor) {
                m.active = tag as usize;
            }
            // A Scroll column may be intentionally unmapped. Set the
            // requested focus before arranging so the viewport reveals it,
            // then focus the now-mapped window.
            self.focus = Some(w);
            self.publish_layout_readonly()?;
            self.arrange()?;
            if !self.hidden.contains(&w) {
                self.set_focus(w)?;
            }
        }
        Ok(())
    }
}
