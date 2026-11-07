//! Scratchpads: floating windows that are shown and hidden with one key.
//!
//! Slot 0 is the default scratchpad (Super+Shift+S, `[scratchpad] command`).
//! Slots 1-9 are the numbered ones (Super+Alt+1..9). A slot holds at most one
//! window: one started from the slot's configured command and recognised by
//! its window class, or any window sent there with Super+Alt+Shift+N.
//!
//! A scratchpad is never a member of a workspace. It is always floating, it is
//! never tiled, it is left out of focus cycling while hidden, out of
//! `_NET_CLIENT_LIST` (so workspace indicators and window lists ignore it) and
//! out of the saved session. At most one scratchpad is shown at a time; it is
//! drawn above the workspace on the focused screen and stays there across
//! workspace switches until it is hidden.
//!
//! 0.17.3 and earlier hid the scratchpad by making it a tiled member of the
//! active workspace: the hidden window still took a tile, and the window under
//! it shrank from full to half width. Nothing in this file changes any other
//! window's geometry; arrange() simply does not see scratchpads as tiles.
use super::*;

/// What a show/hide request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScratchOp {
    Toggle,
    Show,
    Hide,
}

impl ScratchOp {
    /// The value the control channel carries (`_NIRUWM_COMMAND` 15, data[2]).
    pub(crate) fn from_code(code: u32) -> Self {
        match code {
            1 => ScratchOp::Show,
            2 => ScratchOp::Hide,
            _ => ScratchOp::Toggle,
        }
    }
}

/// The outcome of a show/hide request, decided without touching X.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Show this window (hiding whichever other scratchpad is shown).
    Show(Window),
    /// Hide the shown scratchpad.
    Hide,
    /// The slot is empty but has a command: start it.
    Launch,
    /// The slot is empty and has nothing to start.
    Empty,
    /// Nothing to do (hide asked for a slot that is not shown).
    Nothing,
}

/// Decide what a press of a scratchpad key does.
///
/// `window` is the slot's window, `shown` the scratchpad shown right now.
pub(crate) fn plan_toggle(
    window: Option<Window>,
    shown: Option<Window>,
    has_command: bool,
    op: ScratchOp,
) -> Plan {
    match window {
        Some(w) if shown == Some(w) => match op {
            ScratchOp::Show => Plan::Nothing,
            _ => Plan::Hide,
        },
        Some(w) => match op {
            ScratchOp::Hide => Plan::Nothing,
            _ => Plan::Show(w),
        },
        None if op == ScratchOp::Hide => Plan::Nothing,
        None if has_command => Plan::Launch,
        None => Plan::Empty,
    }
}

/// What sending the focused window to a slot does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Send {
    /// The window already is that slot's scratchpad: it goes back to the
    /// current workspace as a normal window. Sending again undoes a send.
    Unscratch,
    /// The window becomes the slot's (hidden) scratchpad. A previous occupant
    /// returns to the current workspace as a normal window.
    Take { evict: Option<Window> },
}

pub(crate) fn plan_send(
    window: Window,
    window_slot: Option<u8>,
    occupant: Option<Window>,
    slot: u8,
) -> Send {
    if window_slot == Some(slot) {
        return Send::Unscratch;
    }
    Send::Take {
        evict: occupant.filter(|o| *o != window),
    }
}

/// The rectangle a scratchpad of `share` (width, height) gets on a screen.
pub(crate) fn centred(screen: Rect, share: (f32, f32)) -> Rect {
    let w = ((f32::from(screen.w) * share.0) as u16).clamp(1, screen.w.max(1));
    let h = ((f32::from(screen.h) * share.1) as u16).clamp(1, screen.h.max(1));
    Rect {
        x: screen.x + ((screen.w - w) / 2) as i16,
        y: screen.y + ((screen.h - h) / 2) as i16,
        w,
        h,
    }
}

/// The `_NIRUWM_SCRATCHPADS` text: one line per slot that has a window or a
/// command, `slot<TAB>state<TAB>window<TAB>class`. `slot` is 0 for the
/// default scratchpad; `state` is shown, hidden or empty (configured, not
/// started; window 0). Bars read titles from the window when they need them.
pub(crate) fn describe(slots: &[(u8, Option<Window>, String)], shown: Option<Window>) -> String {
    let mut out = String::new();
    for (slot, window, class) in slots {
        // A tab or newline in a class would break the line format.
        let class: String = class
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect();
        let class = class.trim();
        match window {
            Some(w) => {
                let state = if shown == Some(*w) { "shown" } else { "hidden" };
                out.push_str(&format!("{slot}\t{state}\t{w}\t{class}\n"));
            }
            None => out.push_str(&format!("{slot}\tempty\t0\t{class}\n")),
        }
    }
    out
}

/// The scratchpad a window belongs to: the window itself when it is one, or
/// the scratchpad its chain of transient parents leads to (a file dialog of a
/// scratchpad, a dialog of that dialog). `link` returns, for a managed window,
/// its slot and its transient parent. The walk is bounded, so a transient
/// cycle a broken client sets up cannot hang the window manager.
pub(crate) fn owner_of(
    w: Window,
    link: impl Fn(Window) -> Option<(Option<u8>, Option<Window>)>,
) -> Option<Window> {
    let mut current = w;
    for _ in 0..8 {
        let (slot, parent) = link(current)?;
        if slot.is_some() {
            return Some(current);
        }
        current = parent?;
    }
    None
}

/// Notices focus that flips between windows far faster than a person can
/// move: two clients (or a client and the window manager) answering each
/// other's focus changes. 0.17.4 met one between a scratchpad and its Qt file
/// dialog: about 9,000 focus changes a second, a flickering screen and a
/// desktop that no longer reacted.
#[derive(Debug, Default)]
pub(crate) struct FocusRate {
    window_start: Option<std::time::Instant>,
    changes: u32,
}

impl FocusRate {
    /// Focus changes per second above which pointer focus pauses.
    pub(crate) const LIMIT: u32 = 60;

    /// Count one focus change at `now`. True when the rate is over the limit.
    pub(crate) fn record(&mut self, now: std::time::Instant) -> bool {
        match self.window_start {
            Some(start) if now.duration_since(start) < std::time::Duration::from_secs(1) => {
                self.changes += 1;
            }
            _ => {
                self.window_start = Some(now);
                self.changes = 1;
            }
        }
        self.changes > Self::LIMIT
    }
}

impl Wm {
    /// The scratchpad `w` belongs to: itself, or the scratchpad it is a
    /// (possibly nested) transient of.
    pub(crate) fn scratch_owner(&self, w: Window) -> Option<Window> {
        owner_of(w, |x| {
            self.clients
                .iter()
                .find(|c| c.win == x)
                .map(|c| (c.scratch, c.owner_link()))
        })
    }

    /// The dialogs (transients) of the shown scratchpad, parents first.
    pub(crate) fn scratch_children(&self, owner: Window) -> Vec<Window> {
        self.clients
            .iter()
            .filter(|c| c.scratch.is_none() && c.owner_link().is_some())
            .filter(|c| self.scratch_owner(c.win) == Some(owner))
            .map(|c| c.win)
            .collect()
    }

    /// The window held by `slot`, if any.
    pub(crate) fn scratch_window(&self, slot: u8) -> Option<Window> {
        self.clients
            .iter()
            .find(|c| c.scratch == Some(slot))
            .map(|c| c.win)
    }

    pub(crate) fn is_scratch(&self, w: Window) -> bool {
        self.clients
            .iter()
            .any(|c| c.win == w && c.scratch.is_some())
    }

    /// A scratchpad that is not shown, or a dialog of one: it takes part in
    /// nothing.
    pub(crate) fn scratch_hidden(&self, c: &Client) -> bool {
        match self.scratch_owner(c.win) {
            Some(owner) => self.scratch_shown != Some(owner),
            None => false,
        }
    }

    /// Show, hide or start a scratchpad slot (0 = default, 1-9 numbered).
    pub(crate) fn scratchpad(&mut self, slot: u8, op: ScratchOp) -> Result<()> {
        if slot > 9 {
            return Ok(());
        }
        let window = self.scratch_window(slot);
        let has_command = self.cfg.scratchpad.command_for(slot).is_some();
        match plan_toggle(window, self.scratch_shown, has_command, op) {
            Plan::Show(w) => self.show_scratch(w)?,
            Plan::Hide => self.hide_scratch()?,
            Plan::Launch => {
                // A second press while the program is still starting must not
                // start a second copy; the first window takes the slot.
                let recent = self.scratch_launching[slot as usize]
                    .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(5));
                if !recent {
                    if let Some(cmd) = self.cfg.scratchpad.command_for(slot) {
                        let cmd = cmd.to_vec();
                        self.scratch_launching[slot as usize] = Some(std::time::Instant::now());
                        self.start(&cmd);
                    }
                }
            }
            Plan::Empty => {
                self.spawn_detached(&[
                    "notify-send".into(),
                    "-a".into(),
                    "NIRUWM".into(),
                    format!("Scratchpad {slot} is empty"),
                    format!("Super+Alt+Shift+{slot} sends the focused window there."),
                ]);
            }
            Plan::Nothing => {}
        }
        self.arrange()
    }

    /// Show one scratchpad on the focused screen, above its workspace.
    pub(crate) fn show_scratch(&mut self, w: Window) -> Result<()> {
        self.quiet_crossings();
        // The focus to return to is the one the user had before any
        // scratchpad came up; switching from one scratchpad to another keeps it.
        if let Some(f) = self.focus.filter(|f| !self.is_scratch(*f)) {
            self.scratch_return = Some(f);
        }
        self.scratch_shown = Some(w);
        self.place_scratch(w, true);
        self.focus = Some(w);
        self.focus_dirty = true;
        Ok(())
    }

    /// Hide the shown scratchpad and give focus back.
    pub(crate) fn hide_scratch(&mut self) -> Result<()> {
        let Some(w) = self.scratch_shown.take() else {
            return Ok(());
        };
        self.quiet_crossings();
        let back = self.scratch_return.take();
        if self.focus == Some(w) || self.focus.is_none() {
            // The window the user worked in before, when it still exists. If it
            // is not visible (another workspace), arrange() falls back.
            self.focus = back.filter(|b| {
                self.clients
                    .iter()
                    .any(|c| c.win == *b && c.scratch.is_none())
            });
            self.focus_dirty = self.focus.is_some();
        }
        Ok(())
    }

    /// Ignore the pointer crossings our own map/unmap is about to cause.
    fn quiet_crossings(&mut self) {
        self.scratch_enter_quiet =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(300));
    }

    /// Put a scratchpad on the focused screen's shown workspace. `recentre`
    /// gives it the slot's size in the middle of that screen; otherwise it
    /// keeps the place the user dragged it to unless it changes screen.
    fn place_scratch(&mut self, w: Window, recentre: bool) {
        let monitor = self
            .focused_monitor
            .min(self.monitors.len().saturating_sub(1));
        let screen = self
            .monitors
            .get(monitor)
            .map(|m| m.rect)
            .unwrap_or_else(|| self.work_area());
        let tag = self
            .monitors
            .get(monitor)
            .map(|m| m.active as u32)
            .unwrap_or(0);
        let name = self.monitor_name(monitor);
        let Some(c) = self.clients.iter_mut().find(|c| c.win == w) else {
            return;
        };
        let share = self.cfg.scratchpad.size_for(c.scratch.unwrap_or(0));
        if recentre || c.monitor != monitor {
            c.geom = centred(screen, share);
        }
        c.monitor = monitor;
        c.monitor_name = name;
        c.tag = tag;
        c.floating = true;
    }

    /// The shown scratchpad moves to the focused screen. Called where the user
    /// deliberately changes screen or workspace (keys, bar, `niructl`), not
    /// on pointer motion, so moving the mouse never drags it along.
    pub(crate) fn follow_scratch(&mut self) {
        if let Some(w) = self.scratch_shown {
            if !self.clients.iter().any(|c| c.win == w) {
                self.scratch_shown = None;
                return;
            }
            self.place_scratch(w, false);
        }
    }

    /// Keep the shown scratchpad on whichever workspace its screen shows, and
    /// always floating. Runs at the start of every arrange().
    pub(crate) fn settle_scratch(&mut self) {
        let Some(w) = self.scratch_shown else { return };
        let Some(index) = self.clients.iter().position(|c| c.win == w) else {
            self.scratch_shown = None;
            return;
        };
        let monitor = self.clients[index].monitor;
        if let Some(m) = self.monitors.get(monitor) {
            self.clients[index].tag = m.active as u32;
        }
        self.clients[index].floating = true;
        // Its dialogs go wherever it goes: same screen, same workspace,
        // floating. One that would now sit on another screen is centred over
        // the scratchpad again.
        let (tag, name, owner_geom) = (
            self.clients[index].tag,
            self.clients[index].monitor_name.clone(),
            self.clients[index].geom,
        );
        let screen = self.monitors.get(monitor).map(|m| m.rect);
        for child in self.scratch_children(w) {
            let Some(c) = self.clients.iter_mut().find(|c| c.win == child) else {
                continue;
            };
            c.monitor = monitor;
            c.monitor_name = name.clone();
            c.tag = tag;
            c.floating = true;
            c.fullscreen = false;
            if let Some(screen) = screen {
                let cx = i32::from(c.geom.x) + i32::from(c.geom.w) / 2;
                let cy = i32::from(c.geom.y) + i32::from(c.geom.h) / 2;
                let inside = cx >= i32::from(screen.x)
                    && cx < i32::from(screen.x) + i32::from(screen.w)
                    && cy >= i32::from(screen.y)
                    && cy < i32::from(screen.y) + i32::from(screen.h);
                if !inside {
                    c.geom.x = owner_geom.x + (owner_geom.w as i16 - c.geom.w as i16) / 2;
                    c.geom.y = owner_geom.y + (owner_geom.h as i16 - c.geom.h as i16) / 2;
                }
            }
        }
    }

    /// Remember a window's slot on the window itself, so a window manager
    /// started again in the same X session (after a crash) finds its
    /// scratchpads instead of turning them into ordinary windows.
    pub(crate) fn mark_scratch(&mut self, w: Window, slot: Option<u8>) {
        let atom = self.atom("_NIRUWM_SCRATCH_SLOT");
        let _ = match slot {
            Some(slot) => self
                .c
                .change_property32(
                    PropMode::REPLACE,
                    w,
                    atom,
                    AtomEnum::CARDINAL,
                    &[u32::from(slot)],
                )
                .map(|_| ()),
            None => self.c.delete_property(w, atom).map(|_| ()),
        };
    }

    /// `hide_on_focus_loss`: the focus moved to a window outside the shown
    /// scratchpad (and outside its dialogs), so it hides. The window that
    /// has the focus now keeps it.
    pub(crate) fn hide_scratch_after_focus_loss(&mut self) -> Result<()> {
        let Some(shown) = self.scratch_shown else {
            return Ok(());
        };
        let Some(focus) = self.focus else {
            return Ok(());
        };
        if self.scratch_owner(focus) == Some(shown) {
            return Ok(());
        }
        self.quiet_crossings();
        self.scratch_shown = None;
        self.scratch_return = None;
        self.arrange()
    }

    /// Send the focused window to a numbered slot, or back from it.
    pub(crate) fn send_to_scratch(&mut self, slot: u8) -> Result<()> {
        if !(1..=9).contains(&slot) {
            return Ok(());
        }
        let Some(w) = self.focus else { return Ok(()) };
        let Some(current) = self.clients.iter().find(|c| c.win == w).map(|c| c.scratch) else {
            return Ok(());
        };
        match plan_send(w, current, self.scratch_window(slot), slot) {
            Send::Unscratch => self.unscratch(w)?,
            Send::Take { evict } => {
                if let Some(old) = evict {
                    self.unscratch(old)?;
                }
                let was_shown = self.scratch_shown == Some(w);
                if was_shown {
                    self.hide_scratch()?;
                } else {
                    self.quiet_crossings();
                    self.focus = None;
                }
                let fullscreen = self.clients.iter().any(|c| c.win == w && c.fullscreen);
                if fullscreen {
                    self.toggle_fullscreen(w)?;
                }
                if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                    if c.scratch.is_none() {
                        c.scratch_floating = c.floating;
                    }
                    c.scratch = Some(slot);
                    c.floating = true;
                    c.sticky = false;
                }
                self.mark_scratch(w, Some(slot));
            }
        }
        self.arrange()
    }

    /// Make a scratchpad an ordinary window on the focused screen's workspace.
    pub(crate) fn unscratch(&mut self, w: Window) -> Result<()> {
        let monitor = self
            .focused_monitor
            .min(self.monitors.len().saturating_sub(1));
        let tag = self
            .monitors
            .get(monitor)
            .map(|m| m.active as u32)
            .unwrap_or(0);
        let name = self.monitor_name(monitor);
        let was_shown = self.scratch_shown == Some(w);
        let Some(c) = self.clients.iter_mut().find(|c| c.win == w) else {
            return Ok(());
        };
        if c.scratch.is_none() {
            return Ok(());
        }
        c.scratch = None;
        c.floating = c.scratch_floating;
        c.monitor = monitor;
        c.monitor_name = name;
        c.tag = tag;
        self.mark_scratch(w, None);
        if was_shown {
            self.scratch_shown = None;
            self.scratch_return = None;
        }
        Ok(())
    }

    /// Make a newly managed window the scratchpad of `slot` and show it.
    pub(crate) fn adopt_scratch(&mut self, w: Window, slot: u8) {
        self.scratch_launching[slot as usize] = None;
        if self.scratch_shown.is_some() {
            // The scratchpad shown so far is about to be hidden.
            self.quiet_crossings();
        }
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.scratch = Some(slot);
            c.scratch_floating = false;
            c.floating = true;
        }
        self.mark_scratch(w, Some(slot));
        if let Some(f) = self.focus.filter(|f| *f != w && !self.is_scratch(*f)) {
            self.scratch_return = Some(f);
        }
        self.scratch_shown = Some(w);
        self.place_scratch(w, true);
    }

    /// Publish which slots exist for bars, only when it changed.
    pub(crate) fn publish_scratchpads(&mut self) -> Result<()> {
        let mut slots: Vec<(u8, Option<Window>, String)> = Vec::new();
        for slot in 0..=9u8 {
            let window = self.clients.iter().find(|c| c.scratch == Some(slot));
            if let Some(c) = window {
                slots.push((slot, Some(c.win), c.class.replace('\0', " ")));
            } else if slot > 0 {
                // A configured slot that has not been started yet: the bar
                // can offer to start it, named by the class it will have.
                if let Some(command) = self.cfg.scratchpad.command_for(slot) {
                    let class = crate::config::scratchpad_class(command).unwrap_or_default();
                    slots.push((slot, None, class));
                }
            }
        }
        let text = describe(&slots, self.scratch_shown);
        let ty = self.atom("UTF8_STRING");
        self.publish_root8("_NIRUWM_SCRATCHPADS", ty, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shown_scratchpad_hides_and_another_one_replaces_it() {
        // Pressing the key of the shown one hides it.
        assert_eq!(
            plan_toggle(Some(5), Some(5), true, ScratchOp::Toggle),
            Plan::Hide
        );
        // Another slot's key shows that one (show_scratch hides the first).
        assert_eq!(
            plan_toggle(Some(6), Some(5), true, ScratchOp::Toggle),
            Plan::Show(6)
        );
        assert_eq!(
            plan_toggle(Some(6), None, false, ScratchOp::Toggle),
            Plan::Show(6)
        );
        // Explicit show/hide from the bar or niructl are idempotent.
        assert_eq!(
            plan_toggle(Some(5), Some(5), true, ScratchOp::Show),
            Plan::Nothing
        );
        assert_eq!(
            plan_toggle(Some(6), Some(5), true, ScratchOp::Hide),
            Plan::Nothing
        );
    }

    #[test]
    fn an_empty_slot_starts_its_command_or_explains_itself() {
        assert_eq!(
            plan_toggle(None, None, true, ScratchOp::Toggle),
            Plan::Launch
        );
        assert_eq!(
            plan_toggle(None, Some(5), true, ScratchOp::Show),
            Plan::Launch
        );
        assert_eq!(
            plan_toggle(None, None, false, ScratchOp::Toggle),
            Plan::Empty
        );
        assert_eq!(
            plan_toggle(None, None, true, ScratchOp::Hide),
            Plan::Nothing
        );
    }

    #[test]
    fn sending_swaps_out_the_old_window_and_sending_again_undoes_it() {
        assert_eq!(plan_send(7, None, None, 3), Send::Take { evict: None });
        // The previous occupant returns to the workspace as a normal window.
        assert_eq!(
            plan_send(7, None, Some(9), 3),
            Send::Take { evict: Some(9) }
        );
        // A scratchpad sent to another slot just moves there.
        assert_eq!(plan_send(7, Some(1), None, 3), Send::Take { evict: None });
        // The same slot again: back to normal tiling.
        assert_eq!(plan_send(7, Some(3), Some(7), 3), Send::Unscratch);
    }

    #[test]
    fn dialogs_belong_to_the_scratchpad_their_parents_lead_to() {
        use std::collections::HashMap;
        // 1 is scratchpad slot 3; 2 is its file dialog; 3 a dialog of that
        // dialog; 4 an ordinary window; 5 a dialog of 4; 6 and 7 a cycle.
        let links: HashMap<Window, (Option<u8>, Option<Window>)> = [
            (1, (Some(3), None)),
            (2, (None, Some(1))),
            (3, (None, Some(2))),
            (4, (None, None)),
            (5, (None, Some(4))),
            (6, (None, Some(7))),
            (7, (None, Some(6))),
            (8, (None, Some(99))),
        ]
        .into_iter()
        .collect();
        let owner = |w| owner_of(w, |x| links.get(&x).copied());
        assert_eq!(owner(1), Some(1));
        assert_eq!(owner(2), Some(1));
        assert_eq!(owner(3), Some(1));
        assert_eq!(owner(4), None);
        assert_eq!(owner(5), None);
        // A transient cycle or an unmanaged parent ends the walk.
        assert_eq!(owner(6), None);
        assert_eq!(owner(8), None);
    }

    #[test]
    fn focus_flipping_thousands_of_times_a_second_is_noticed() {
        let start = std::time::Instant::now();
        let mut rate = FocusRate::default();
        // A person: a few changes a second never trip it.
        for i in 0..10u64 {
            assert!(!rate.record(start + std::time::Duration::from_millis(i * 300)));
        }
        // A loop: 61 changes within a second do.
        let mut rate = FocusRate::default();
        let tripped = (0..=FocusRate::LIMIT as u64)
            .map(|i| rate.record(start + std::time::Duration::from_micros(i * 100)))
            .last()
            .unwrap();
        assert!(tripped);
        // A second later the count starts over.
        assert!(!rate.record(start + std::time::Duration::from_secs(2)));
    }

    #[test]
    fn scratchpads_are_centred_with_their_share_of_the_screen() {
        let screen = Rect {
            x: 2560,
            y: 0,
            w: 2560,
            h: 1440,
        };
        let r = centred(screen, (0.6, 0.6));
        assert_eq!((r.w, r.h), (1536, 864));
        assert_eq!((r.x, r.y), (2560 + 512, 288));
    }

    #[test]
    fn the_bar_state_lists_slots_and_which_one_is_shown() {
        let text = describe(
            &[
                (0, Some(11), "kitty\tscratchpad".into()),
                (1, None, "scratch-btop".into()),
                (3, Some(12), "firefox".into()),
            ],
            Some(12),
        );
        assert_eq!(
            text,
            "0\thidden\t11\tkitty scratchpad\n1\tempty\t0\tscratch-btop\n3\tshown\t12\tfirefox\n"
        );
    }
}
