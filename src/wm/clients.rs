use super::*;

/// Whether a client takes a tile on screen `monitor` showing workspace
/// `active`. Floating, fullscreen and withdrawn windows do not, and neither
/// does any scratchpad, shown or hidden: a scratchpad is never a workspace
/// member, so showing or hiding one cannot move or resize a tiled window.
pub(crate) fn tiles(c: &Client, monitor: usize, active: u32, withdrawn: &HashSet<Window>) -> bool {
    c.monitor == monitor
        && (c.tag == active || c.sticky)
        && !c.floating
        && !c.fullscreen
        && c.scratch.is_none()
        && c.swallowed_by.is_none()
        && !withdrawn.contains(&c.win)
}

fn suppress_transient(withdrawn: &HashSet<Window>, parent: Option<Window>) -> bool {
    parent.is_some_and(|parent| withdrawn.contains(&parent))
}

impl Wm {
    pub(crate) fn values(&self, w: Window, n: &str) -> Vec<u32> {
        self.c
            .get_property(false, w, self.atom(n), AtomEnum::ANY, 0, 64)
            .ok()
            .and_then(|r| r.reply().ok())
            .and_then(|r| r.value32().map(|v| v.collect()))
            .unwrap_or_default()
    }

    fn size_hints(&self, w: Window) -> SizeHints {
        let Ok(cookie) = self.c.get_property(
            false,
            w,
            AtomEnum::WM_NORMAL_HINTS,
            AtomEnum::WM_SIZE_HINTS,
            0,
            18,
        ) else {
            return SizeHints::default();
        };
        let Ok(reply) = cookie.reply() else {
            return SizeHints::default();
        };
        match x11rb::properties::WmSizeHints::from_reply(&reply) {
            Ok(Some(hints)) => SizeHints::from_wm(&hints),
            _ => SizeHints::default(),
        }
    }

    fn has_protocol(&self, w: Window, protocol: &str) -> bool {
        self.values(w, "WM_PROTOCOLS")
            .contains(&self.atom(protocol))
    }

    fn window_types(&self, w: Window) -> Vec<u32> {
        self.values(w, "_NET_WM_WINDOW_TYPE")
    }

    fn tray_activation_recent(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        self.values(self.root, "_NIRU_TRAY_ACTIVATE")
            .first()
            .copied()
            .map(|stamp| u64::from(stamp) != 0 && now.saturating_sub(u64::from(stamp)) <= 5)
            .unwrap_or(false)
    }

    fn orphan_transient(&self, parent: Option<Window>) -> bool {
        let Some(parent) = parent else { return false };
        if self.clients.iter().any(|client| client.win == parent) {
            return false;
        }
        self.c
            .get_window_attributes(parent)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|attrs| attrs.map_state != MapState::VIEWABLE)
            .unwrap_or(true)
    }

    pub(crate) fn manage(&mut self, w: Window) -> Result<()> {
        if let Some(client) = self.clients.iter().find(|c| c.win == w) {
            if suppress_transient(&self.withdrawn, client.parent)
                || (self.orphan_transient(client.parent) && !self.tray_activation_recent())
            {
                let _ = self.c.unmap_window(w);
                self.c.flush()?;
                return Ok(());
            }
            self.withdrawn.remove(&w);
            self.hidden.remove(&w);
            self.c.map_window(w)?;
            self.c.flush()?;
            return self.arrange();
        }
        let a = self.c.get_window_attributes(w)?.reply()?;
        // Override-redirect (menus, tooltips, splash screens) stay unmanaged.
        if a.override_redirect {
            return Ok(());
        }
        // An input-only helper has no visible surface and cannot be tiled or
        // focused. Mapping it and walking away is the correct ICCCM handling.
        if a.class == WindowClass::INPUT_ONLY {
            self.c.map_window(w)?;
            self.c.flush()?;
            return Ok(());
        }

        let types = self.window_types(w);
        // A desktop window belongs below everything and never takes focus.
        if types.contains(&self.atom("_NET_WM_WINDOW_TYPE_DESKTOP")) {
            self.c.map_window(w)?;
            self.c
                .configure_window(w, &ConfigureWindowAux::new().stack_mode(StackMode::BELOW))?;
            self.c.flush()?;
            return Ok(());
        }
        let is_dock = types.contains(&self.atom("_NET_WM_WINDOW_TYPE_DOCK"));
        if is_dock {
            // Panels are mapped as found and never tiled. NIRUBAR reserves
            // space via its strut, which `work_area()` reads.
            self.docks.insert(w);
            self.c.map_window(w)?;
            self.c.flush()?;
            return self.arrange();
        }
        if types.contains(&self.atom("_NET_WM_STATE")) {
            // Malformed hint; treat as normal.
        }

        let g = self.c.get_geometry(w)?.reply()?;
        let transient = self
            .c
            .get_property(false, w, AtomEnum::WM_TRANSIENT_FOR, AtomEnum::WINDOW, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut v| v.next())
            .filter(|w| *w != 0);

        self.withdrawn.remove(&w);
        let orphan = self.orphan_transient(transient);
        // A dialog belongs to its parent's workspace. Suppress it only when the
        // parent withdrew itself (a tray hide or a closed main window). A parent
        // that is merely hidden by the WM on another workspace still allows a
        // new dialog, which is placed with that parent below.
        if suppress_transient(&self.withdrawn, transient)
            || (orphan && !self.tray_activation_recent())
        {
            let _ = self.c.unmap_window(w);
            self.c.flush()?;
            return Ok(());
        }

        // Dialogs, utilities, splash screens, notifications and menu/popup
        // helpers float above the tile layout. The popup family is normally
        // override-redirect, but Steam and some toolkits create managed
        // variants; they still must not join the tiling layout.
        let floating_types = [
            "_NET_WM_WINDOW_TYPE_DIALOG",
            "_NET_WM_WINDOW_TYPE_UTILITY",
            "_NET_WM_WINDOW_TYPE_SPLASH",
            "_NET_WM_WINDOW_TYPE_NOTIFICATION",
            "_NET_WM_WINDOW_TYPE_POPUP_MENU",
            "_NET_WM_WINDOW_TYPE_DROPDOWN_MENU",
            "_NET_WM_WINDOW_TYPE_TOOLTIP",
            "_NET_WM_WINDOW_TYPE_COMBO",
            "_NET_WM_WINDOW_TYPE_DND",
            "_NET_WM_WINDOW_TYPE_TOOLBAR",
            "_NET_WM_WINDOW_TYPE_MENU",
        ];
        let dialog_floating = transient.is_some()
            || types
                .iter()
                .any(|t| floating_types.iter().any(|name| self.atom(name) == *t));
        // Only normal windows and real dialogs take focus when they map.
        // Palettes, notifications, popups and toolbars wait for a click.
        let no_focus_types = [
            "_NET_WM_WINDOW_TYPE_UTILITY",
            "_NET_WM_WINDOW_TYPE_SPLASH",
            "_NET_WM_WINDOW_TYPE_NOTIFICATION",
            "_NET_WM_WINDOW_TYPE_POPUP_MENU",
            "_NET_WM_WINDOW_TYPE_DROPDOWN_MENU",
            "_NET_WM_WINDOW_TYPE_TOOLTIP",
            "_NET_WM_WINDOW_TYPE_COMBO",
            "_NET_WM_WINDOW_TYPE_DND",
            "_NET_WM_WINDOW_TYPE_TOOLBAR",
            "_NET_WM_WINDOW_TYPE_MENU",
        ];
        let focus_on_map = !types
            .iter()
            .any(|t| no_focus_types.iter().any(|name| self.atom(name) == *t));

        // Follow the client's own desktop when it asks for one (session
        // restore, `wmctrl -i -t`, applications started with a tag override).
        let requested_tag = self.values(w, "_NET_WM_DESKTOP").first().copied();

        let border = self.cfg.border_width;
        // Border width is applied here once; `arrange()` only changes geometry.
        self.c.configure_window(
            w,
            &ConfigureWindowAux::new().border_width(u32::from(border)),
        )?;
        self.c.change_window_attributes(
            w,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::ENTER_WINDOW | EventMask::PROPERTY_CHANGE | EventMask::FOCUS_CHANGE,
            ),
        )?;
        self.grab_mouse_buttons(w)?;
        self.c.change_save_set(SetMode::INSERT, w)?;

        let take_focus = self.has_protocol(w, "WM_TAKE_FOCUS");
        // A new window belongs to the screen under the pointer, or to the
        // focused screen when the pointer is outside every screen.
        // A rule may pin this window to a workspace, before the pointer
        // decides the screen.
        let rule = self.rule_for(w);
        // A window whose class matches the scratchpad command becomes the
        // scratchpad, floating and centred, instead of being tiled.
        // The scratchpad is recognised by the class named in its command, not
        // by the program. Matching every argument made *every* kitty window a
        // scratchpad, because "kitty" was one of them.
        let class = self.window_class(w);
        let suite_settings = class.split('\0').any(|part| {
            matches!(
                part.to_ascii_lowercase().as_str(),
                "niruthemeselector" | "niruwallpaper" | "nirupres"
            )
        });
        // The launcher marks the agent terminal as a suite overlay so the TUI
        // opens centred and floating even on a fresh configuration.
        let agent_window = class
            .split('\0')
            .any(|part| part.eq_ignore_ascii_case("niru-agent"));
        // A window of a scratchpad's class takes that slot when the slot is
        // free. A second window of the same class is an ordinary window.
        //
        // A dialog never takes a slot: Qt gives a file dialog the class of its
        // application, so nirunote's Open dialog used to become the nirunote
        // scratchpad itself whenever that slot was free.
        //
        // A window that was a scratchpad before the window manager started
        // again (after a crash) says so in `_NIRUWM_SCRATCH_SLOT`.
        let marked_slot = self
            .values(w, "_NIRUWM_SCRATCH_SLOT")
            .first()
            .and_then(|slot| u8::try_from(*slot).ok())
            .filter(|slot| *slot <= 9);
        let scratch_slot = if transient.is_some() {
            None
        } else {
            marked_slot
                .or_else(|| self.cfg.scratchpad.slot_for_class(&class))
                .filter(|slot| self.scratch_window(*slot).is_none())
        };
        let is_scratchpad = scratch_slot.is_some();
        // A restored window carries the placement saved for it. Matching
        // happens after the scratchpad check so a terminal that happens to be
        // the scratchpad never consumes a normal session record.
        let pid = self.values(w, "_NET_WM_PID").first().copied();
        let restored = self.take_restored(pid, is_scratchpad);
        let sticky = rule.as_ref().and_then(|r| r.sticky).unwrap_or(false);
        let floating = dialog_floating
            || suite_settings
            || agent_window
            || is_scratchpad
            || sticky
            || rule.as_ref().and_then(|r| r.floating).unwrap_or(false)
            || restored.as_ref().is_some_and(|r| r.floating);
        // `_NET_WM_DESKTOP` is a global EWMH desktop number. Resolve it to
        // the owning monitor and that monitor's local workspace.
        let requested = requested_tag.and_then(|t| crate::monitor::find_global(&self.monitors, t));
        let (px, py) = self.pointer_position();
        let pointer_monitor = if self.monitors.is_empty() {
            0
        } else {
            let by_pointer = crate::monitor::monitor_at(&self.monitors, px, py);
            if self.monitors.get(by_pointer).is_some() {
                by_pointer
            } else {
                self.focused_monitor
            }
        };
        let parent_place = transient.and_then(|parent| {
            self.clients
                .iter()
                .find(|c| c.win == parent)
                .map(|c| (c.monitor, c.tag))
        });
        let (monitor, tag) = match &restored {
            Some(r) => (r.monitor.min(self.monitors.len().saturating_sub(1)), r.tag),
            None => match parent_place {
                Some((monitor, tag)) => (monitor, tag),
                None if transient.is_some() => {
                    // A tray dialog is often transient for an unmanaged 1x1
                    // helper that asks for desktop 0. Keep it on the workspace
                    // the user is actually looking at instead of hiding it.
                    let monitor = pointer_monitor;
                    let tag = self
                        .monitors
                        .get(monitor)
                        .map(|m| m.active as u32)
                        .unwrap_or_else(|| self.active_tag());
                    (monitor, tag)
                }
                None => match requested {
                    Some((monitor, tag)) => (monitor, tag as u32),
                    None => {
                        // Suite settings belong to the screen the user is working on.
                        // The pointer is the active screen even when the desktop has no
                        // focused client, and the window must use that screen's tag.
                        let monitor = pointer_monitor;
                        let tag = rule
                            .as_ref()
                            .and_then(|r| r.workspace)
                            .or_else(|| self.monitors.get(monitor).map(|m| m.active as u32))
                            .unwrap_or_else(|| self.active_tag());
                        (monitor, tag)
                    }
                },
            },
        };
        // A program started from a terminal takes the terminal's place.
        // Only an ordinary top-level window that nothing else places swallows:
        // not a dialog or helper, not a scratchpad, not a restored window, not
        // one a rule or `_NET_WM_DESKTOP` sends to a workspace.
        let may_swallow = !self.adopting
            && restored.is_none()
            && transient.is_none()
            && !dialog_floating
            && focus_on_map
            && !is_scratchpad
            && !suite_settings
            && !agent_window
            && requested.is_none()
            && !sticky
            && rule
                .as_ref()
                .map_or(true, |r| r.workspace.is_none() && r.sticky != Some(true));
        let swallowed = if may_swallow {
            self.swallow_target(&class, self.window_pid(w))
                .and_then(|t| {
                    let index = self.clients.iter().position(|c| c.win == t)?;
                    let c = &self.clients[index];
                    Some((t, index, c.monitor, c.tag, c.floating, c.geom))
                })
        } else {
            None
        };
        let (monitor, tag) = swallowed
            .map(|(_, _, monitor, tag, _, _)| (monitor, tag))
            .unwrap_or((monitor, tag));
        let floating = floating || swallowed.is_some_and(|(_, _, _, _, f, _)| f);
        let monitor_name = self
            .monitors
            .get(monitor)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        // A dialog that names no parent but opens while the shown scratchpad
        // (or one of its dialogs) has the focus is the scratchpad's: the
        // portal's file chooser for a program in the scratchpad. Without
        // this it opened below the scratchpad, which stays on top.
        let is_dialog = types.contains(&self.atom("_NET_WM_WINDOW_TYPE_DIALOG"))
            || self
                .values(w, "_NET_WM_STATE")
                .contains(&self.atom("_NET_WM_STATE_MODAL"));
        let guest_of = if transient.is_none() && is_dialog && !is_scratchpad && !self.adopting {
            self.scratch_shown.filter(|s| {
                self.focus
                    .is_some_and(|f| self.scratch_owner(f) == Some(*s))
            })
        } else {
            None
        };
        let size_hints = self.size_hints(w);
        // Toolkits may establish fullscreen before the first MapWindow. The
        // initial state must have the same meaning as a later ClientMessage;
        // otherwise the client is tiled into the bar's work area while still
        // claiming to be fullscreen.
        let initial_fullscreen = self
            .values(w, "_NET_WM_STATE")
            .contains(&self.atom("_NET_WM_STATE_FULLSCREEN"));
        let client = Client {
            win: w,
            monitor,
            monitor_name,
            tag,
            floating,
            fullscreen: initial_fullscreen,
            geom: Rect {
                x: g.x,
                y: g.y,
                w: g.width.saturating_add(border * 2),
                h: g.height.saturating_add(border * 2),
            },
            take_focus,
            pid,
            parent: transient,
            focus_on_map,
            orphan,
            sticky,
            size_hints,
            home: None,
            class: class.clone(),
            scratch: None,
            scratch_floating: false,
            guest_of,
            urgent: false,
            urgent_serial: 0,
            swallowed_by: None,
        };
        let mut client = client;
        if let Some((_, _, _, _, true, geom)) = swallowed {
            // A floating terminal hands over its rectangle.
            client.geom = geom;
        }
        if let Some(opacity) = rule.as_ref().and_then(|r| r.opacity) {
            let value = (u64::from(opacity.clamp(1, 100)) * u64::from(u32::MAX) / 100) as u32;
            self.prop(
                w,
                "_NET_WM_WINDOW_OPACITY",
                AtomEnum::CARDINAL.into(),
                &[value],
            )?;
        }
        let scroll = self
            .monitors
            .get(monitor)
            .and_then(|m| m.states.get(tag as usize))
            .is_some_and(|state| state.layout == Layout::Scroll);
        let insert_at = if let Some((_, index, _, _, _, _)) = swallowed {
            // The terminal's place in the tiling order; the terminal stays
            // right after it, hidden.
            index
        } else if scroll && !floating {
            self.focus
                .and_then(|focused| {
                    self.clients.iter().position(|c| {
                        c.win == focused
                            && c.monitor == monitor
                            && c.tag == tag
                            && !c.floating
                            && !c.fullscreen
                    })
                })
                .map(|index| index + 1)
                .unwrap_or(self.clients.len())
        } else {
            self.clients.len()
        };
        self.clients.insert(insert_at, client);
        if let Some((t, ..)) = swallowed {
            self.swallow(w, t);
            eprintln!(
                "niruwm: window {w:#x} ({}) swallowed terminal {t:#x}",
                class.replace('\0', " ").trim()
            );
        }
        // After a restart, a terminal that was swallowed says by which window.
        if self.adopting {
            if let Some(owner) = self
                .values(w, "_NIRUWM_SWALLOWED_BY")
                .first()
                .copied()
                .filter(|o| *o != 0)
            {
                self.swallow_adopted.push((w, owner));
            }
        }
        if restored.as_ref().is_some_and(|r| r.focused) {
            // Remember the window that had focus last time; switching to its
            // workspace waits until the other restored windows have mapped.
            self.restore_focus = Some((w, monitor, tag));
        }

        if floating
            && (suite_settings
                || agent_window
                || rule.as_ref().and_then(|r| r.center).unwrap_or(false))
        {
            let screen = self.monitor_area(monitor, self.work_area());
            if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                c.geom.w = c.geom.w.min(screen.w.saturating_sub(border * 2).max(1));
                c.geom.h = c.geom.h.min(screen.h.saturating_sub(border * 2).max(1));
                c.geom.x = screen.x + (screen.w as i16 - c.geom.w as i16 - (border * 2) as i16) / 2;
                c.geom.y = screen.y + (screen.h as i16 - c.geom.h as i16 - (border * 2) as i16) / 2;
                c.geom.x = c.geom.x.max(screen.x);
                c.geom.y = c.geom.y.max(screen.y);
            }
        }
        let adopted_hidden = self.adopting && marked_slot.is_some() && is_scratchpad;
        if let Some(slot) = scratch_slot {
            if adopted_hidden {
                // Found again after a restart: back in its slot, hidden, as
                // the user most likely had it.
                if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                    c.scratch = Some(slot);
                    c.floating = true;
                }
            } else {
                // Shown at once on the focused screen, above its workspace,
                // and hiding any other scratchpad. It never joins the
                // workspace.
                self.adopt_scratch(w, slot);
            }
        }
        // A dialog of a hidden scratchpad would open hidden with it. A dialog
        // asks for attention, so its scratchpad comes up with it.
        let owner = if transient.is_some() || guest_of.is_some() {
            self.scratch_owner(w)
        } else {
            None
        };
        if let Some(owner) = owner {
            if self.scratch_shown != Some(owner) && focus_on_map && !self.adopting {
                self.show_scratch(owner)?;
            }
            // It follows the scratchpad: same screen and workspace, centred
            // over it when the client did not place it inside.
            let parent_geom = self
                .clients
                .iter()
                .find(|c| c.win == owner)
                .map(|c| (c.geom, c.monitor, c.tag));
            if let (Some((pg, pm, pt)), Some(c)) =
                (parent_geom, self.clients.iter_mut().find(|c| c.win == w))
            {
                c.monitor = pm;
                c.tag = pt;
                c.floating = true;
                let inside = c.geom.x >= pg.x
                    && c.geom.y >= pg.y
                    && i32::from(c.geom.x) + i32::from(c.geom.w)
                        <= i32::from(pg.x) + i32::from(pg.w)
                    && i32::from(c.geom.y) + i32::from(c.geom.h)
                        <= i32::from(pg.y) + i32::from(pg.h);
                if !inside && c.geom.w > 1 && c.geom.h > 1 {
                    c.geom.x = pg.x + (pg.w as i16 - c.geom.w as i16) / 2;
                    c.geom.y = pg.y + (pg.h as i16 - c.geom.h as i16) / 2;
                }
            }
        }
        self.prop(w, "WM_STATE", self.atom("WM_STATE"), &[1, 0])?;
        self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[tag])?;
        // Keep floating windows centred if the client asked for a 1x1 size
        // (common for dialogs that size themselves after mapping).
        if floating && !is_scratchpad && swallowed.is_none() && (g.width <= 1 || g.height <= 1) {
            let area = self.work_area();
            if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                c.geom = Rect {
                    x: area.x + (area.w as i16 - 480) / 2,
                    y: area.y + (area.h as i16 - 320) / 2,
                    w: 480,
                    h: 320,
                };
            }
        }
        self.c.map_window(w)?;
        // A restored window must not move focus while its siblings are still
        // starting; the saved focus is applied once restore has settled.
        if suite_settings {
            self.focused_monitor = monitor;
        }
        if is_scratchpad && !adopted_hidden {
            self.focus = Some(w);
            self.focus_dirty = true;
        } else if owner.is_some() && focus_on_map && !self.adopting {
            // Focused once arrange() has mapped and stacked it above its
            // scratchpad.
            self.focus = Some(w);
            self.focus_dirty = true;
        } else if !is_scratchpad && restored.is_none() && focus_on_map && tag == self.active_tag() {
            self.set_focus(w)?;
        }
        self.arrange()?;
        self.studio_window_added(w, &class);
        // A window may ask for attention before it maps (or, adopted after a
        // restart, still carry the request).
        self.note_attention(w)?;
        if self.restore_remaining == 0 && self.restore_deadline.is_some() {
            self.finish_restore()?;
        }
        Ok(())
    }

    /// Toggle a focused floating window between one workspace and all
    /// workspaces on its monitor. Sticky always implies floating.
    pub(crate) fn toggle_sticky(&mut self, w: Window) -> Result<()> {
        if let Some(client) = self.clients.iter_mut().find(|c| c.win == w) {
            client.sticky = !client.sticky;
            if client.sticky {
                client.floating = true;
            }
        }
        self.arrange()
    }

    pub(crate) fn unmanage(&mut self, w: Window) {
        self.studio_window_removed(w);
        self.withdrawn.remove(&w);
        self.hidden.remove(&w);
        self.docks.remove(&w);
        self.remember_gone(w);
        if self.dragging == Some(w) {
            self.dragging = None;
        }
        if self.restore_focus.map(|(window, _, _)| window) == Some(w) {
            self.restore_focus = None;
        }
        if self.scratch_return == Some(w) {
            self.scratch_return = None;
        }
        // A window that swallowed a terminal gives the terminal its place back.
        let terminal_back = self.unswallow_for_closed(w);
        let urgent_gone = self.clients.iter().any(|c| c.win == w && c.urgent);
        // Closing a scratchpad frees its slot: the slot is a property of the
        // client, so removing the client below is all it takes. Focus goes
        // back to where it was before the scratchpad was shown.
        let scratch_back = if self.scratch_shown == Some(w) {
            self.scratch_shown = None;
            self.scratch_return.take()
        } else {
            None
        };
        let neighbor = if self.focus == Some(w) {
            self.clients
                .iter()
                .find(|c| c.win == w)
                .and_then(|removed| {
                    let monitor = removed.monitor;
                    let tag = removed.tag;
                    if removed.floating
                        || removed.fullscreen
                        || self
                            .monitors
                            .get(monitor)
                            .and_then(|m| m.states.get(tag as usize))
                            .map_or(true, |state| state.layout != Layout::Scroll)
                    {
                        return None;
                    }
                    let tiled: Vec<Window> = self
                        .clients
                        .iter()
                        .filter(|c| {
                            c.monitor == monitor
                                && c.tag == tag
                                && !c.floating
                                && !c.fullscreen
                                && c.swallowed_by.is_none()
                        })
                        .map(|c| c.win)
                        .collect();
                    let index = tiled.iter().position(|candidate| *candidate == w)?;
                    tiled
                        .get(index + 1)
                        .or_else(|| index.checked_sub(1).and_then(|i| tiled.get(i)))
                        .copied()
                })
        } else {
            None
        };
        // A closed dialog gives the focus back to the window it belongs to.
        let parent_back = if self.focus == Some(w) {
            self.clients
                .iter()
                .find(|c| c.win == w)
                .and_then(|c| c.owner_link())
                .filter(|p| self.clients.iter().any(|c| c.win == *p) && !self.hidden.contains(p))
        } else {
            None
        };
        if let Some(pos) = self.clients.iter().position(|c| c.win == w) {
            self.clients.remove(pos);
        }
        self.hidden.remove(&w);
        if self.focus == Some(w) || (terminal_back.is_some() && self.focus.is_none()) {
            // The terminal it swallowed comes back with the focus.
            let next = terminal_back.or(scratch_back).or(parent_back).or(neighbor);
            self.focus = next;
            self.focus_dirty = next.is_some();
        }
        if urgent_gone {
            let _ = self.publish_urgent();
        }
    }

    /// Remove a managed window after the X server has already destroyed it.
    ///
    /// DestroyNotify normally handles this. Errors are the backstop for the
    /// race where a request was already in flight: they identify the dead XID,
    /// stop the WM from retrying the same window on every arrange, and let the
    /// surviving clients keep their state. Unknown errors are left to the
    /// caller's log instead of being mistaken for client XIDs.
    pub(crate) fn repair_x11_error(&mut self, error: &x11rb::x11_utils::X11Error) -> bool {
        if error.error_kind != ErrorKind::Window {
            return false;
        }
        if self.recently_gone.contains(&error.bad_value) {
            return true;
        }
        if !matches!(
            error.request_name,
            Some(
                "ConfigureWindow"
                    | "ChangeProperty"
                    | "SetInputFocus"
                    | "ChangeWindowAttributes"
                    | "SendEvent"
                    | "MapWindow"
                    | "UnmapWindow"
            )
        ) {
            return false;
        }
        let window = error.bad_value;
        let tracked = self.clients.iter().any(|client| client.win == window)
            || self.docks.contains(&window)
            || self.hidden.contains(&window)
            || self.withdrawn.contains(&window)
            || self.scratch_shown == Some(window)
            || self.focus == Some(window)
            || self.dragging == Some(window);
        if !tracked {
            return false;
        }
        self.unmanage(window);
        if self.focus == Some(window) {
            self.focus = None;
            self.focus_dirty = true;
        }
        eprintln!(
            "niruwm: dropped stale window {window:#x} after {}",
            error.request_name.unwrap_or("X request")
        );
        if let Err(e) = self.arrange() {
            eprintln!("niruwm: could not repair after stale window: {e}");
        }
        true
    }

    fn remember_gone(&mut self, w: Window) {
        if self.recently_gone.contains(&w) {
            return;
        }
        self.recently_gone.push_back(w);
        while self.recently_gone.len() > 128 {
            self.recently_gone.pop_front();
        }
    }

    /// Focus a client, with fallbacks that survive the common race where the
    /// window disappears between the event and the call.
    pub(crate) fn set_focus(&mut self, w: Window) -> Result<()> {
        // A swallowed terminal is hidden; the window in its place has the focus.
        let w = self.swallower_of(w).unwrap_or(w);
        if !self.clients.iter().any(|c| c.win == w) {
            return Ok(());
        }
        let take_focus = self
            .clients
            .iter()
            .find(|c| c.win == w)
            .map(|c| c.take_focus)
            .unwrap_or(false);
        let previous = self.focus;
        // Never raise an unmanaged window; the WM owns stacking for tiles.
        // The window goes to the top together with its own dialogs, which
        // stay above it.
        //
        // While a scratchpad is shown and stays on top, every other window is
        // raised to just below it, never over it: raising a window over the
        // scratchpad for a moment under the pointer is a crossing into that
        // window, which takes the focus, and then the scratchpad goes up
        // again, which is a crossing back. That loop ran on othala between a
        // terminal and the nirunote scratchpad.
        //
        // A tiled window stays in the tiled layer. Raising it to the very top
        // put it over floating windows: a NIRUSHOT review window or an
        // Open/Save dialog of another window that overlapped two tiles ended
        // up half under the tile the pointer crossed into and half over the
        // other one. The tile goes directly below the lowest floating window
        // instead, and only its own dialogs go to the top.
        let dialogs = self.visible_descendants(w);
        let ceiling = self.scratch_ceiling(w);
        let tiled = self
            .clients
            .iter()
            .find(|c| c.win == w)
            .is_some_and(|c| !c.floating && !c.fullscreen);
        if tiled {
            let floor = self.lowest_floating(w, &dialogs)?;
            self.stack_group(&[w], floor.or(ceiling))?;
            self.stack_group(&dialogs, ceiling)?;
        } else {
            let mut group = vec![w];
            group.extend(dialogs);
            self.stack_group(&group, ceiling)?;
        }
        if take_focus {
            // ICCCM: the client asked for focus messages and may keep
            // input focus itself (e.g. Java, some input methods).
            self.c.send_event(
                false,
                w,
                EventMask::NO_EVENT,
                ClientMessageEvent::new(
                    32,
                    w,
                    self.atom("WM_PROTOCOLS"),
                    [self.atom("WM_TAKE_FOCUS"), x11rb::CURRENT_TIME, 0, 0, 0],
                ),
            )?;
        }
        // RevertToParent rather than PointerRoot: when the focused window is
        // destroyed X returns focus to its parent (the root) instead of
        // leaving it dangling, and NIRUWM repairs it in `arrange()`.
        self.c
            .set_input_focus(InputFocus::POINTER_ROOT, w, x11rb::CURRENT_TIME)?;
        self.focus = Some(w);
        self.prop(
            self.root,
            "_NET_ACTIVE_WINDOW",
            AtomEnum::WINDOW.into(),
            &[w],
        )?;
        self.clear_urgent_on_focus(w)?;
        self.update_borders()?;
        if previous != Some(w) {
            self.note_focus_change(w);
        }
        Ok(())
    }

    /// Count a focus change: pause pointer focus when it is a loop, and note
    /// a focus loss of the shown scratchpad for `hide_on_focus_loss`.
    fn note_focus_change(&mut self, w: Window) {
        let now = std::time::Instant::now();
        if self.focus_rate.record(now) {
            self.pointer_focus_paused = Some(now + std::time::Duration::from_secs(1));
            let report = self.focus_loop_reported.map_or(true, |at| {
                now.duration_since(at) > std::time::Duration::from_secs(30)
            });
            if report {
                self.focus_loop_reported = Some(now);
                eprintln!(
                    "niruwm: the focus changed more than {} times in a second (last: window {w}, class {:?}); pointer focus pauses for a second",
                    super::scratch::FocusRate::LIMIT,
                    self.clients
                        .iter()
                        .find(|c| c.win == w)
                        .map(|c| c.class.replace('\0', " "))
                        .unwrap_or_default()
                );
            }
        }
        if self.cfg.scratchpad.hide_on_focus_loss {
            if let Some(shown) = self.scratch_shown {
                if self.scratch_owner(w) != Some(shown) {
                    self.scratch_hide_pending = true;
                }
            }
        }
    }

    /// Raising the focused window must not cover what belongs above it: its
    /// own dialogs, and the shown scratchpad with its dialogs, which stays on
    /// top until it is hidden. 0.17.4 raised the focused window alone, so a
    /// dialog went under its scratchpad, and moving the mouse onto a window
    /// outside the scratchpad put that window over it (as if it had hidden).
    /// The lowest window of the shown scratchpad's group (the scratchpad
    /// itself) when `w` must stay below it, i.e. when `w` is not part of it
    /// and the scratchpad stays on top (`hide_on_focus_loss` off).
    fn scratch_ceiling(&self, w: Window) -> Option<Window> {
        let shown = self.scratch_shown?;
        if self.cfg.scratchpad.hide_on_focus_loss
            || self.hidden.contains(&shown)
            || self.scratch_owner(w) == Some(shown)
        {
            return None;
        }
        Some(shown)
    }

    /// The bottom-most managed floating or fullscreen window in the real X
    /// stacking order, leaving out `w` and `skip` (its own dialogs). A tile
    /// that takes the focus goes directly below it, so every floating window
    /// stays above the tiles, as `arrange()` stacks them. Unmapped windows
    /// count too: stacking below a sibling is valid whether it is mapped or
    /// not, and the next `arrange()` orders them again when they are shown.
    fn lowest_floating(&self, w: Window, skip: &[Window]) -> Result<Option<Window>> {
        let children = self.c.query_tree(self.root)?.reply()?.children;
        Ok(children.into_iter().find(|&child| {
            child != w
                && !skip.contains(&child)
                && self
                    .clients
                    .iter()
                    .any(|c| c.win == child && (c.floating || c.fullscreen))
        }))
    }

    /// Put `group` on top of the stack in this order (the last one topmost)
    /// without ever stacking an earlier window over a later one, not even for
    /// a moment.
    ///
    /// Raising a window and then its dialog one after the other covered the
    /// dialog briefly. Under the pointer that is a crossing into the parent,
    /// which focused the parent, which raised it over the dialog again: the
    /// 0.17.4 flicker. The topmost window goes up first and each one before
    /// it is placed directly below the next.
    pub(crate) fn raise_group(&self, group: &[Window]) -> Result<()> {
        self.stack_group(group, None)
    }

    /// `raise_group`, or with `ceiling` the group goes directly below that
    /// window instead of to the very top.
    pub(crate) fn stack_group(&self, group: &[Window], ceiling: Option<Window>) -> Result<()> {
        let Some((&top, rest)) = group.split_last() else {
            return Ok(());
        };
        match ceiling {
            Some(ceiling) if ceiling != top => self.c.configure_window(
                top,
                &ConfigureWindowAux::new()
                    .sibling(ceiling)
                    .stack_mode(StackMode::BELOW),
            )?,
            _ => self
                .c
                .configure_window(top, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE))?,
        };
        let mut upper = top;
        for &w in rest.iter().rev() {
            self.c.configure_window(
                w,
                &ConfigureWindowAux::new()
                    .sibling(upper)
                    .stack_mode(StackMode::BELOW),
            )?;
            upper = w;
        }
        Ok(())
    }

    /// The mapped transients of `w` (dialogs, their dialogs), in the order
    /// they were opened, which is the order they stack in.
    pub(crate) fn visible_descendants(&self, w: Window) -> Vec<Window> {
        self.clients
            .iter()
            .filter(|c| c.win != w && c.owner_link().is_some())
            .filter(|c| !self.hidden.contains(&c.win) && !self.withdrawn.contains(&c.win))
            .filter(|c| self.descends_from(c.win, w))
            .map(|c| c.win)
            .collect()
    }

    /// Whether `w` is a transient of `ancestor`, directly or through others.
    pub(crate) fn descends_from(&self, w: Window, ancestor: Window) -> bool {
        let mut current = w;
        for _ in 0..8 {
            let Some(parent) = self
                .clients
                .iter()
                .find(|c| c.win == current)
                .and_then(|c| c.owner_link())
            else {
                return false;
            };
            if parent == ancestor {
                return true;
            }
            current = parent;
        }
        false
    }

    /// A modal dialog open over `w`. The pointer entering `w` focuses that
    /// dialog instead: a toolkit hands the focus back to its modal dialog at
    /// once anyway, and answering each other is how a focus loop starts.
    pub(crate) fn modal_for(&self, w: Window) -> Option<Window> {
        let modal = self.atom("_NET_WM_STATE_MODAL");
        self.visible_descendants(w)
            .into_iter()
            .rev()
            .find(|d| self.values(*d, "_NET_WM_STATE").contains(&modal))
    }

    fn focus_client(&mut self, w: Option<Window>) -> Result<()> {
        match w {
            Some(w) => self.set_focus(w),
            None => {
                self.focus = None;
                self.c
                    .set_input_focus(InputFocus::POINTER_ROOT, self.root, x11rb::CURRENT_TIME)?;
                self.prop(
                    self.root,
                    "_NET_ACTIVE_WINDOW",
                    AtomEnum::WINDOW.into(),
                    &[],
                )?;
                self.update_borders()
            }
        }
    }

    pub(crate) fn update_borders(&self) -> Result<()> {
        // Looked up only while a window asks for attention: it may read
        // appearance.toml.
        let urgent = self
            .clients
            .iter()
            .any(|c| c.urgent)
            .then(|| self.theme.urgent_pixel(&self.cfg.theme));
        for c in &self.clients {
            if c.fullscreen || c.swallowed_by.is_some() {
                continue;
            }
            let colour = if Some(c.win) == self.focus {
                Theme::pixel(&self.theme.focus)?
            } else if let (true, Some(pixel)) = (c.urgent, urgent) {
                pixel
            } else {
                Theme::pixel(&self.theme.border)?
            };
            self.c.change_window_attributes(
                c.win,
                &ChangeWindowAttributesAux::new().border_pixel(colour),
            )?;
        }
        Ok(())
    }

    /// Area available to tiled clients: the screen minus the bar's strut.
    /// WM_CLASS for a window as one string, e.g. "kitty Scratchpad".
    pub(crate) fn window_class(&self, w: Window) -> String {
        self.c
            .get_property(false, w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.value).to_string())
            .unwrap_or_default()
    }

    /// The first rule whose class matches this window.
    fn rule_for(&self, w: Window) -> Option<crate::config::Rule> {
        if self.cfg.rules.is_empty() {
            return None;
        }
        let class = self.window_class(w);
        self.cfg
            .rules
            .iter()
            .find(|rule| class.contains(&rule.class))
            .cloned()
    }

    /// Pointer position, queried fresh (used by focus-follows-mouse).
    pub(crate) fn pointer_position_now(&self) -> (i16, i16) {
        self.pointer_position()
    }

    /// Focus whichever screen the pointer is over.
    ///
    /// Moving the mouse across a screen boundary changes the focused screen
    /// even when the pointer lands on empty desktop rather than a window.
    pub(crate) fn focus_screen_at(&mut self, x: i16, y: i16) -> Result<()> {
        if self.monitors.len() < 2 {
            return Ok(());
        }
        let target = crate::monitor::monitor_at(&self.monitors, x, y);
        if target == self.focused_monitor {
            return Ok(());
        }
        self.focused_monitor = target;
        // Focus a window on that screen when there is one, so typing has a
        // destination. An empty screen keeps the pointer focus.
        let active = self.monitors[target].active as u32;
        let win = self
            .clients
            .iter()
            .find(|c| {
                c.monitor == target
                    && c.tag == active
                    && c.swallowed_by.is_none()
                    && self.scratch_owner(c.win).is_none()
            })
            .map(|c| c.win);
        match win {
            Some(w) => self.set_focus(w),
            None => Ok(()),
        }
    }

    /// Pointer position in root coordinates, or the focused screen's centre.
    fn pointer_position(&self) -> (i16, i16) {
        if let Ok(cookie) = self.c.query_pointer(self.root) {
            if let Ok(reply) = cookie.reply() {
                return (reply.root_x, reply.root_y);
            }
        }
        match self.monitors.get(self.focused_monitor) {
            Some(m) => (
                m.rect.x + (m.rect.w / 2) as i16,
                m.rect.y + (m.rect.h / 2) as i16,
            ),
            None => (0, 0),
        }
    }

    fn screen_area(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.screen_w,
            h: self.screen_h,
        }
    }

    pub(crate) fn work_area(&self) -> Rect {
        let mut top = self.bar_reserved_height();
        let mut bottom = self.bar_reserved_bottom();
        let children = match self
            .c
            .query_tree(self.root)
            .ok()
            .and_then(|c| c.reply().ok())
        {
            Some(r) => r.children,
            None => return self.screen_area(),
        };
        for w in children {
            let attrs = match self.c.get_window_attributes(w) {
                Ok(cookie) => match cookie.reply() {
                    Ok(a) => a,
                    Err(_) => continue,
                },
                Err(_) => continue,
            };
            if attrs.map_state != MapState::VIEWABLE {
                continue;
            }
            // Both forms are read: the partial one, when a bar publishes it,
            // and the plain one, which only says how much to reserve.
            let read = |atom: Atom, len: usize| -> Option<Vec<u32>> {
                self.c
                    .get_property(false, w, atom, AtomEnum::CARDINAL, 0, len as u32)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .and_then(|r| r.value32().map(|v| v.collect::<Vec<_>>()))
                    .filter(|v| v.len() >= len)
            };
            if let Some(strut) = read(self.atom("_NET_WM_STRUT_PARTIAL"), 12) {
                top = top.max(strut[2].min(u32::from(self.screen_h)) as u16);
                bottom = bottom.max(strut[3].min(u32::from(self.screen_h)) as u16);
            } else if let Some(strut) = read(self.atom("_NET_WM_STRUT"), 4) {
                top = top.max(strut[2].min(u32::from(self.screen_h)) as u16);
                bottom = bottom.max(strut[3].min(u32::from(self.screen_h)) as u16);
            }
        }
        let top = top.min(self.screen_h);
        let bottom = bottom.min(self.screen_h.saturating_sub(top));
        Rect {
            x: 0,
            y: top as i16,
            w: self.screen_w,
            h: self.screen_h.saturating_sub(top).saturating_sub(bottom),
        }
    }

    /// The bar height this session reserves at the top of every screen.
    ///
    /// The configured height, or a little more with a title, so a window never
    /// appears behind the bar in the short time before it maps.
    fn bar_reserved_height(&self) -> u16 {
        if self.bar_active() && !self.bar_bottom() {
            self.cfg.bar_height
        } else {
            0
        }
    }

    fn bar_reserved_bottom(&self) -> u16 {
        if self.bar_active() && self.bar_bottom() {
            self.cfg.bar_height
        } else {
            0
        }
    }

    pub(crate) fn arrange(&mut self) -> Result<()> {
        let area = self.work_area();
        self.settle_scratch();
        self.settle_swallowed();

        // Tile each screen separately. A window is visible when its tag is the
        // active tag on its own screen, so two screens can show two different
        // workspaces at once.
        let mut geometry: Vec<(usize, Window, Rect, u16, bool)> = Vec::new();
        let mut visible: Vec<Window> = Vec::new();
        for mi in 0..self.monitors.len().max(1) {
            let screen = self.monitor_area(mi, area);
            let active = self.monitors.get(mi).map(|m| m.active).unwrap_or(0) as u32;
            // The layout belongs to this workspace on this screen, so two
            // screens and ten workspaces never share one.
            let layout = self
                .monitors
                .get(mi)
                .map(Monitor::layout)
                .unwrap_or(Layout::Tile);
            let tiled: Vec<Window> = self
                .clients
                .iter()
                .filter(|c| tiles(c, mi, active, &self.withdrawn))
                .filter(|c| self.scratch_owner(c.win).is_none())
                .map(|c| c.win)
                .collect();
            let n = tiled.len();
            let fullscreen_active = self.clients.iter().any(|c| {
                c.monitor == mi
                    && c.tag == active
                    && c.fullscreen
                    && c.scratch.is_none()
                    && c.swallowed_by.is_none()
                    && !self.withdrawn.contains(&c.win)
            });
            let (scroll_start, scroll_columns) = if layout == Layout::Scroll {
                let effective = if self
                    .monitors
                    .get(mi)
                    .map(Monitor::scroll_columns)
                    .unwrap_or(crate::layout::ScrollColumns::Two)
                    == crate::layout::ScrollColumns::Two
                    && screen.w < crate::layout::SCROLL_MIN_COLUMN_WIDTH.saturating_mul(2)
                {
                    1
                } else {
                    self.monitors
                        .get(mi)
                        .map(Monitor::scroll_columns)
                        .unwrap_or(crate::layout::ScrollColumns::Two)
                        .count()
                };
                let focus = self.focus.and_then(|w| {
                    tiled
                        .iter()
                        .position(|candidate| *candidate == w)
                        .or_else(|| {
                            self.clients
                                .iter()
                                .find(|c| c.win == w)
                                .and_then(|c| c.parent)
                                .and_then(|parent| {
                                    tiled.iter().position(|candidate| *candidate == parent)
                                })
                        })
                });
                let monitor = self.monitors.get_mut(mi).unwrap();
                let (start, mode) = monitor.reveal_scroll(focus, n, effective);
                (start, mode.count())
            } else {
                (0, 1)
            };
            let boxes = if layout == Layout::Scroll {
                crate::layout::scroll(screen, n, scroll_start, scroll_columns, self.cfg.gap)
            } else {
                layout.arrange_master(
                    screen,
                    n,
                    self.monitors.get(mi).map(|m| m.nmaster()).unwrap_or(1),
                    self.monitors
                        .get(mi)
                        .map(|m| m.master_ratio())
                        .unwrap_or(self.cfg.master_ratio),
                    self.cfg.gap,
                )
            };
            let mut idx = 0;
            for (client_index, c) in self.clients.iter().enumerate() {
                if c.monitor != mi {
                    continue;
                }
                if c.tag != active && !c.sticky {
                    continue;
                }
                // A client that withdrew itself (for example to a tray) is
                // not a member of this workspace until it maps again. Skip it
                // entirely so it consumes neither a tile box nor visible state.
                if self.withdrawn.contains(&c.win) {
                    continue;
                }
                // A swallowed terminal is not on any workspace until the
                // window in its place closes.
                if c.swallowed_by.is_some() {
                    continue;
                }
                // A hidden scratchpad is not on any workspace: no tile, no
                // geometry, not visible. The shown one floats above.
                // Its dialogs share its fate.
                let owner = self.scratch_owner(c.win);
                if owner.is_some_and(|o| self.scratch_shown != Some(o)) {
                    continue;
                }
                let shown = if owner.is_some() || c.sticky {
                    true
                } else if layout == Layout::Scroll {
                    if fullscreen_active {
                        c.fullscreen
                    } else if c.floating || c.fullscreen {
                        true
                    } else {
                        let at = tiled.iter().position(|candidate| *candidate == c.win);
                        at.is_some_and(|at| {
                            at >= scroll_start && at < scroll_start + scroll_columns
                        })
                    }
                } else {
                    true
                };
                if shown {
                    visible.push(c.win);
                }
                let r = if c.fullscreen {
                    self.monitors
                        .get(mi)
                        .map(|m| m.rect)
                        .unwrap_or_else(|| self.screen_area())
                } else if Some(c.win) == self.dragging {
                    // The drag loop already positioned this window from root
                    // pointer coordinates. Clamping it to the source monitor
                    // here would make it jump back whenever any unrelated
                    // event triggers arrange() while the button is held.
                    c.geom
                } else if c.floating || layout == Layout::Floating {
                    self.clamp(c.size_hints.clamp(c.geom), screen)
                } else {
                    let r = boxes.get(idx).copied().unwrap_or(screen);
                    idx += 1;
                    r
                };
                let smart = !c.floating
                    && (n == 1
                        || layout == Layout::Monocle
                        || (layout == Layout::Scroll && scroll_columns == 1));
                let b = if c.fullscreen || smart {
                    0
                } else {
                    self.cfg.border_width
                };
                geometry.push((client_index, c.win, r, b, c.fullscreen));
            }
        }

        // Keep `Client.geom` in step with what arrange actually configured.
        // Mouse move/resize starts from this rectangle; a stale pre-tiling
        // size made the first drag jump back to the window's original size.
        // Fullscreen geometry is deliberately not written back: toggling it
        // off must restore the window's previous floating/tiled rectangle.
        for (index, _, r, _, fullscreen) in &geometry {
            if !fullscreen {
                self.clients[*index].geom = *r;
            }
        }

        // Hide everything that is not on an active workspace of its screen.
        // Orphan tray popups are one-shot: once the user leaves their
        // workspace they must not be remapped on a later visit.
        let recent_tray = self.tray_activation_recent();
        let mut dismiss_orphans = Vec::new();
        let mut unmap = Vec::new();
        for c in &self.clients {
            let shown = visible.contains(&c.win);
            if !shown && c.orphan && !recent_tray {
                dismiss_orphans.push(c.win);
            }
            if !shown && self.withdrawn.contains(&c.win) {
                // Already unmapped by its client. Do not record it as hidden
                // by the WM; a later MapRequest is what brings it back.
                continue;
            }
            if !shown && self.hidden.insert(c.win) {
                unmap.push(c.win);
            } else if shown && self.hidden.remove(&c.win) {
                self.c.map_window(c.win)?;
            }
        }
        // Map first, then unmap: replacing one scratchpad with another (or
        // one workspace with the next) then never uncovers the window below
        // for a moment, which would hand it the focus through EnterNotify.
        for w in unmap {
            self.c.unmap_window(w)?;
        }
        for w in dismiss_orphans {
            self.unmanage(w);
        }

        for (_, win, r, b, fullscreen) in geometry {
            self.c.configure_window(
                win,
                &ConfigureWindowAux::new()
                    .x(i32::from(r.x))
                    .y(i32::from(r.y))
                    .width(u32::from(r.w.saturating_sub(b * 2).max(1)))
                    .height(u32::from(r.h.saturating_sub(b * 2).max(1)))
                    .border_width(u32::from(b)),
            )?;
            if fullscreen {
                self.c.configure_window(
                    win,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                )?;
            }
        }

        // Floating windows above the tiles, in the order they were opened
        // (so a dialog stays over its parent), as one group: raising them one
        // by one briefly covered a dialog with its parent.
        let floating: Vec<Window> = self
            .clients
            .iter()
            .filter(|c| c.floating && visible.contains(&c.win))
            .filter(|c| {
                self.scratch_shown.is_none() || self.scratch_owner(c.win) != self.scratch_shown
            })
            .map(|c| c.win)
            .collect();
        // The shown scratchpad is above everything on its workspace,
        // including other floating and fullscreen windows. Its dialogs go
        // above it, in the order they were opened, so a file dialog is never
        // covered by the scratchpad it belongs to. It goes up first and the
        // floating windows are placed below it, so none of them is ever
        // above it for a moment (a crossing under the pointer).
        let shown = self.scratch_shown.filter(|w| visible.contains(w));
        if let Some(w) = shown {
            let mut group = vec![w];
            group.extend(
                self.scratch_children(w)
                    .into_iter()
                    .filter(|child| visible.contains(child)),
            );
            self.raise_group(&group)?;
        }
        self.stack_group(&floating, shown)?;

        // Panels belong above ordinary and floating clients. A visible
        // fullscreen client is the only exception; once it leaves fullscreen
        // the next arrange raises the panel again. Without this, a Steam or
        // RetroArch window that was raised over the bar can keep intercepting
        // workspace clicks after it returns to a normal rectangle.
        let fullscreen_visible = self
            .clients
            .iter()
            .any(|c| visible.contains(&c.win) && c.fullscreen);
        if !fullscreen_visible {
            let docks: Vec<Window> = self.docks.iter().copied().collect();
            for dock in docks {
                self.c.configure_window(
                    dock,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                )?;
            }
        }

        // Focus stays on a visible window of the focused screen.
        let focus_ok = self.focus.map(|w| visible.contains(&w)).unwrap_or(false);
        if !focus_ok {
            let next = self
                .clients
                .iter()
                .filter(|c| {
                    visible.contains(&c.win) && c.monitor == self.focused_monitor && c.focus_on_map
                })
                .map(|c| c.win)
                .next()
                .or_else(|| {
                    self.clients
                        .iter()
                        .filter(|c| visible.contains(&c.win) && c.monitor == self.focused_monitor)
                        .map(|c| c.win)
                        .next()
                })
                .or_else(|| {
                    self.clients
                        .iter()
                        .rev()
                        .find(|c| visible.contains(&c.win) && c.focus_on_map)
                        .map(|c| c.win)
                })
                .or_else(|| {
                    self.clients
                        .iter()
                        .rev()
                        .find(|c| visible.contains(&c.win))
                        .map(|c| c.win)
                });
            self.focus_client(next)?;
        } else if self.focus_dirty {
            if let Some(w) = self.focus.filter(|w| visible.contains(w)) {
                self.set_focus(w)?;
            }
            self.focus_dirty = false;
        } else {
            self.focus_dirty = false;
        }

        self.publish_desktop_state()?;
        self.c.flush()?;
        Ok(())
    }

    /// RandR output name for a monitor index. Every path that moves a client
    /// must keep this beside the numeric index so hotplug can re-home it.
    pub(crate) fn monitor_name(&self, index: usize) -> String {
        self.monitors
            .get(index)
            .map(|m| m.name.clone())
            .unwrap_or_default()
    }

    /// The usable area of one monitor, with the bar's height removed only on
    /// the monitor the bar sits on.
    pub(crate) fn monitor_work_area(&self, index: usize) -> Rect {
        self.monitor_area(index, self.work_area())
    }

    fn monitor_area(&self, index: usize, work: Rect) -> Rect {
        match self.monitors.get(index) {
            Some(m) => {
                // Each screen starts below whatever its own bar reserves. A
                // bar's strut is measured from the top of the desktop, so the
                // part that falls on this screen is the strut minus the
                // distance from the desktop's top to this screen's top. A bar
                // on a screen lower down therefore never eats that screen.
                let top = work
                    .y
                    .saturating_sub(m.rect.y)
                    .max(0)
                    .max(self.bar_reserved_height() as i16) as u16;
                let top = top.min(m.rect.h.saturating_sub(1));
                let bottom = (i32::from(m.rect.y) + i32::from(m.rect.h)
                    - i32::from(work.y)
                    - i32::from(work.h))
                .max(0)
                .max(i32::from(self.bar_reserved_bottom())) as u16;
                let bottom = bottom.min(m.rect.h.saturating_sub(top).saturating_sub(1));
                Rect {
                    x: m.rect.x,
                    y: m.rect.y + top as i16,
                    w: m.rect.w,
                    h: m.rect.h.saturating_sub(top).saturating_sub(bottom),
                }
            }
            None => work,
        }
    }

    /// Publish the desktop state in a way panels understand with several
    /// screens: one flat list of workspaces, with the focused screen's
    /// workspace selected.
    pub(crate) fn publish_root32(
        &mut self,
        name: &'static str,
        ty: Atom,
        values: &[u32],
    ) -> Result<()> {
        let bytes: Vec<u8> = values
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        if self.published_root.get(name) == Some(&bytes) {
            return Ok(());
        }
        self.prop(self.root, name, ty, values)?;
        self.published_root.insert(name, bytes);
        Ok(())
    }

    pub(crate) fn publish_root8(
        &mut self,
        name: &'static str,
        ty: Atom,
        value: &[u8],
    ) -> Result<()> {
        if self.published_root.get(name).map(Vec::as_slice) == Some(value) {
            return Ok(());
        }
        self.c
            .change_property8(PropMode::REPLACE, self.root, self.atom(name), ty, value)?;
        self.published_root.insert(name, value.to_vec());
        Ok(())
    }

    fn publish_desktop_state(&mut self) -> Result<()> {
        let total: usize = self.monitors.iter().map(|m| m.workspaces.len()).sum();
        let active = crate::monitor::global_active(&self.monitors, self.focused_monitor);
        self.publish_root32(
            "_NET_NUMBER_OF_DESKTOPS",
            AtomEnum::CARDINAL.into(),
            &[total.max(1) as u32],
        )?;
        self.publish_root32("_NET_CURRENT_DESKTOP", AtomEnum::CARDINAL.into(), &[active])?;
        // Scratchpads are not workspace members, so window lists and workspace
        // indicators never count them; `_NIRUWM_SCRATCHPADS` describes them.
        // A swallowed terminal is hidden behind its window and listed nowhere.
        let list: Vec<Window> = self
            .clients
            .iter()
            .filter(|c| self.scratch_owner(c.win).is_none() && c.swallowed_by.is_none())
            .map(|c| c.win)
            .collect();
        let placements: Vec<(Window, u32)> = self
            .clients
            .iter()
            .filter(|c| self.scratch_owner(c.win).is_none() && c.swallowed_by.is_none())
            .map(|client| {
                (
                    client.win,
                    crate::monitor::offset(&self.monitors, client.monitor) + client.tag,
                )
            })
            .collect();
        if self.published_client_desktops.as_ref() != Some(&placements) {
            // Notify workspace bars even if only a window's desktop changed.
            self.published_root.remove("_NET_CLIENT_LIST");
            self.published_client_desktops = Some(placements);
        }
        let stacking: Vec<Window> = self
            .c
            .query_tree(self.root)?
            .reply()?
            .children
            .into_iter()
            .filter(|w| list.contains(w))
            .collect();
        self.publish_root32(
            "_NET_CLIENT_LIST_STACKING",
            AtomEnum::WINDOW.into(),
            &stacking,
        )?;
        // Publish workspace names so a bar or pager can label them.
        let mut names = String::new();
        for m in &self.monitors {
            for w in &m.workspaces {
                names.push_str(w);
                names.push('\0');
            }
        }
        if !names.is_empty() {
            self.publish_root8(
                "_NET_DESKTOP_NAMES",
                self.atom("UTF8_STRING"),
                names.as_bytes(),
            )?;
        }
        // Every client advertises its global desktop for panels using EWMH.
        for c in &self.clients {
            // EWMH: 0xFFFFFFFF is "on all desktops", which is how a
            // scratchpad behaves while it is shown.
            let global = if self.scratch_owner(c.win).is_some() {
                0xFFFF_FFFF
            } else {
                crate::monitor::offset(&self.monitors, c.monitor) + c.tag
            };
            self.prop(
                c.win,
                "_NET_WM_DESKTOP",
                AtomEnum::CARDINAL.into(),
                &[global],
            )?;
        }
        // Send the root notification after the client properties so a bar
        // observing it sees the updated desktop assignments immediately.
        self.publish_root32("_NET_CLIENT_LIST", AtomEnum::WINDOW.into(), &list)?;
        self.publish_scratchpads()?;
        self.publish_urgent()?;
        // The monitor layout contract lets one bar per output describe its own
        // workspace range without guessing from the focused monitor.
        //
        // The eighth field is the layout of the workspace each screen shows, so
        // a bar can show and change its own screen's layout. Bars start in the
        // moment the manager has no screens yet, so the failure is not fatal.
        let mut layout = String::new();
        for (index, m) in self.monitors.iter().enumerate() {
            let start = crate::monitor::offset(&self.monitors, index);
            layout.push_str(&format!(
                "{}	{}	{}	{}	{}	{}	{}	{}
",
                m.name,
                m.rect.x,
                m.rect.y,
                m.rect.w,
                start,
                m.workspaces.len(),
                m.active,
                m.layout_label()
            ));
        }
        if layout.is_empty() {
            return Ok(());
        }
        self.publish_root8(
            "_NIRUWM_MONITORS",
            self.atom("UTF8_STRING"),
            layout.as_bytes(),
        )?;
        Ok(())
    }

    /// Keep a floating rectangle inside the usable area.
    fn clamp(&self, r: Rect, area: Rect) -> Rect {
        let w = r.w.min(area.w).max(60);
        let h = r.h.min(area.h).max(40);
        let max_x = area.x + (area.w.saturating_sub(w)) as i16;
        let max_y = area.y + (area.h.saturating_sub(h)) as i16;
        Rect {
            x: r.x.clamp(area.x, max_x),
            y: r.y.clamp(area.y, max_y),
            w,
            h,
        }
    }

    /// Change the master ratio on the focused workspace only.
    pub(crate) fn change_master_ratio(&mut self, delta: f32) -> Result<()> {
        if let Some(m) = self.monitors.get_mut(self.focused_monitor) {
            m.set_master_ratio(m.master_ratio() + delta);
        }
        self.publish_layout()?;
        self.arrange()?;
        Ok(())
    }

    /// Change the master count on the focused workspace only.
    pub(crate) fn change_master(&mut self, delta: i32) -> Result<()> {
        if let Some(m) = self.monitors.get_mut(self.focused_monitor) {
            // At least one master and never more than the workspace is tiled
            // for; the layout code handles the upper bound.
            m.change_master(delta);
        }
        self.publish_layout()?;
        self.arrange()
    }

    /// The workspace shown on the focused screen.
    pub(crate) fn active_tag(&self) -> u32 {
        self.monitors
            .get(self.focused_monitor)
            .map(|m| m.active as u32)
            .unwrap_or(0)
    }

    /// Switch to a global EWMH desktop, focusing its owning monitor.
    pub(crate) fn view_global(&mut self, global: u32) -> Result<()> {
        if let Some((monitor, workspace)) = crate::monitor::find_global(&self.monitors, global) {
            self.focused_monitor = monitor;
            if let Some(m) = self.monitors.get_mut(monitor) {
                m.active = workspace;
            }
        }
        self.follow_scratch();
        self.publish_layout_readonly()?;
        self.arrange()
    }

    /// Move the focused window to a global desktop, changing monitor if needed.
    pub(crate) fn move_to_global(&mut self, global: u32) -> Result<()> {
        if let Some(w) = self.focus {
            self.move_window_to_global(w, global)?;
        } else {
            self.arrange()?;
        }
        Ok(())
    }

    /// Move one specific managed window to a global desktop. Used by the
    /// standard `_NET_WM_DESKTOP` request and by autostart placement.
    pub(crate) fn move_window_to_global(&mut self, w: Window, global: u32) -> Result<()> {
        let Some((monitor, workspace)) = crate::monitor::find_global(&self.monitors, global) else {
            return Ok(());
        };
        let monitor_name = self
            .monitors
            .get(monitor)
            .map(|m| m.name.clone())
            .unwrap_or_default();
        self.unscratch(w)?;
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.monitor = monitor;
            c.monitor_name = monitor_name;
            c.tag = workspace as u32;
        }
        self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[global])?;
        self.arrange()
    }

    /// Switch the focused screen to one of its workspaces.
    ///
    /// `tag` is an index into that screen's own workspace list, so each screen
    /// keeps its set independent of the others.
    pub(crate) fn view_tag(&mut self, tag: u32) -> Result<()> {
        if let Some(m) = self.monitors.get_mut(self.focused_monitor) {
            if (tag as usize) < m.workspaces.len() {
                m.active = tag as usize;
            }
        }
        self.follow_scratch();
        // The published layout follows the focused workspace, so tools and
        // scripts read the layout the user is looking at.
        self.publish_layout_readonly()?;
        self.arrange()
    }

    /// Toggle a tag in the view.
    ///
    /// One workspace is shown per screen, so toggling a different workspace
    /// switches to it; toggling the current one does nothing.
    pub(crate) fn toggle_tag(&mut self, tag: u32) -> Result<()> {
        if tag != self.active_tag() {
            return self.view_tag(tag);
        }
        Ok(())
    }

    /// Move the focused window to a tag (dwm `tag`/`toggletag`).
    pub(crate) fn move_to_tag(&mut self, tag: u32, follow: bool) -> Result<()> {
        let count = self
            .monitors
            .get(self.focused_monitor)
            .map(|m| m.workspaces.len())
            .unwrap_or(9);
        if tag as usize >= count {
            return Ok(());
        }
        if let Some(w) = self.focus {
            // Moving a scratchpad to a workspace makes it a normal window there.
            self.unscratch(w)?;
            if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
                c.tag = tag;
            }
            // Publish the global desktop number, not the monitor-local tag.
            let global = crate::monitor::offset(&self.monitors, self.focused_monitor) + tag;
            self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[global])?;
            if follow {
                self.view_tag(tag)?;
            }
        }
        self.arrange()
    }

    /// Swap the focused window with its neighbour in the tiling order.
    pub(crate) fn swap(&mut self, delta: isize) -> Result<()> {
        let Some(w) = self.focus else { return Ok(()) };
        let visible: Vec<Window> = self
            .clients
            .iter()
            .filter(|c| {
                c.monitor == self.focused_monitor
                    && c.tag == self.active_tag()
                    && !c.floating
                    && c.swallowed_by.is_none()
            })
            .map(|c| c.win)
            .collect();
        if visible.len() < 2 {
            return Ok(());
        }
        let Some(i) = visible.iter().position(|x| *x == w) else {
            return Ok(());
        };
        let scroll = self
            .monitors
            .get(self.focused_monitor)
            .is_some_and(|m| m.layout() == Layout::Scroll);
        let j = if scroll {
            (i as isize + delta).clamp(0, visible.len() as isize - 1) as usize
        } else {
            (i as isize + delta).rem_euclid(visible.len() as isize) as usize
        };
        let a = visible[i];
        let b = visible[j];
        let ia = self.clients.iter().position(|c| c.win == a);
        let ib = self.clients.iter().position(|c| c.win == b);
        if let (Some(ia), Some(ib)) = (ia, ib) {
            self.clients.swap(ia, ib);
        }
        self.arrange()
    }

    /// Close the focused window politely, then forcibly.
    pub(crate) fn close(&mut self, w: Window) -> Result<()> {
        if self.has_protocol(w, "WM_DELETE_WINDOW") {
            self.c.send_event(
                false,
                w,
                EventMask::NO_EVENT,
                ClientMessageEvent::new(
                    32,
                    w,
                    self.atom("WM_PROTOCOLS"),
                    [self.atom("WM_DELETE_WINDOW"), x11rb::CURRENT_TIME, 0, 0, 0],
                ),
            )?;
        } else {
            self.c.kill_client(w)?;
        }
        Ok(())
    }

    pub(crate) fn focus_next(&mut self, delta: isize) -> Result<()> {
        // Cycle through the clients on the focused screen's active workspace.
        //
        // This used to read `self.tag`, a field left over from before
        // per-monitor workspaces. It was never assigned, so it stayed 0 and
        // focus only cycled on workspace 1; every other workspace appeared
        // stuck on one window. `active_tag` is the value the layout uses.
        let tag = self.active_tag();
        let monitor = self.focused_monitor;
        let visible: Vec<Window> = self
            .clients
            .iter()
            .filter(|c| {
                c.monitor == monitor
                    && c.tag == tag
                    && c.swallowed_by.is_none()
                    && !self.scratch_hidden(c)
            })
            .map(|c| c.win)
            .collect();
        if visible.is_empty() {
            return Ok(());
        }
        if self
            .monitors
            .get(monitor)
            .is_some_and(|m| m.layout() == Layout::Scroll)
        {
            if self
                .clients
                .iter()
                .any(|c| Some(c.win) == self.focus && c.fullscreen)
            {
                return Ok(());
            }
            let tiled: Vec<Window> = visible
                .iter()
                .copied()
                .filter(|w| {
                    self.clients
                        .iter()
                        .any(|c| c.win == *w && !c.floating && !c.fullscreen)
                })
                .collect();
            if tiled.is_empty() {
                return Ok(());
            }
            let i = tiled.iter().position(|w| Some(*w) == self.focus);
            let j = match i {
                Some(i) => (i as isize + delta).clamp(0, tiled.len() as isize - 1) as usize,
                None if delta > 0 => 0,
                None => tiled.len() - 1,
            };
            self.focus = Some(tiled[j]);
            self.arrange()?;
            return self.set_focus(tiled[j]);
        }
        let i = visible
            .iter()
            .position(|w| Some(*w) == self.focus)
            .unwrap_or(0) as isize;
        let len = visible.len() as isize;
        let j = (i + delta).rem_euclid(len) as usize;
        self.set_focus(visible[j])
    }

    /// Move focus to the screen to the left (-1) or right (+1).
    ///
    /// A window is focused on the target screen so typing goes somewhere
    /// useful; the screen keeps its own workspace.
    pub(crate) fn focus_monitor(&mut self, delta: i32) -> Result<()> {
        let count = self.monitors.len();
        if count < 2 {
            return Ok(());
        }
        let next = (self.focused_monitor as i32 + delta).rem_euclid(count as i32) as usize;
        self.focused_monitor = next;
        let active = self.monitors[next].active as u32;
        // A shown scratchpad comes along and keeps the keyboard.
        self.follow_scratch();
        let target = self.scratch_shown.or_else(|| {
            self.clients
                .iter()
                .find(|c| {
                    c.monitor == next
                        && c.tag == active
                        && c.swallowed_by.is_none()
                        && self.scratch_owner(c.win).is_none()
                })
                .map(|c| c.win)
        });
        match target {
            Some(w) => self.set_focus(w)?,
            None => self.focus_client(None)?,
        }
        self.arrange()
    }

    /// Move the focused window to the screen to the left (-1) or right (+1).
    pub(crate) fn move_to_monitor(&mut self, delta: i32) -> Result<()> {
        let count = self.monitors.len();
        if count < 2 {
            return Ok(());
        }
        let Some(w) = self.focus else {
            return Ok(());
        };
        let target = (self.focused_monitor as i32 + delta).rem_euclid(count as i32) as usize;
        let monitor_name = self.monitor_name(target);
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.monitor = target;
            c.monitor_name = monitor_name;
            c.tag = self.monitors[target].active as u32;
        }
        self.focused_monitor = target;
        self.follow_scratch();
        self.arrange()
    }

    pub(crate) fn next_tag(&mut self, delta: i32) -> Result<()> {
        let len = self
            .monitors
            .get(self.focused_monitor)
            .map(|m| m.workspaces.len())
            .unwrap_or(9)
            .max(1) as i32;
        let t = (self.active_tag() as i32 + delta).rem_euclid(len);
        self.view_tag(t as u32)
    }

    pub(crate) fn toggle_fullscreen(&mut self, w: Window) -> Result<()> {
        if let Some(c) = self.clients.iter_mut().find(|c| c.win == w) {
            c.fullscreen = !c.fullscreen;
            let f = c.fullscreen;
            let tag = c.tag;
            let full = self.atom("_NET_WM_STATE_FULLSCREEN");
            // Bound to a local: a temporary array in the `if` lives only for
            // the statement, which rustc 1.75 rejects and later versions
            // accept. The suite's declared baseline must compile.
            let states: [u32; 1] = [full];
            let state: &[u32] = if f { &states } else { &[] };
            self.prop(w, "_NET_WM_STATE", AtomEnum::ATOM.into(), state)?;
            if f {
                self.c
                    .configure_window(w, &ConfigureWindowAux::new().border_width(0))?;
            } else {
                let b = self.cfg.border_width;
                self.c
                    .configure_window(w, &ConfigureWindowAux::new().border_width(u32::from(b)))?;
            }
            if tag == self.active_tag()
                && self
                    .clients
                    .iter()
                    .any(|c| c.win == w && c.monitor == self.focused_monitor)
            {
                self.set_focus(w)?;
            }
        }
        self.arrange()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decision that keeps a client alive across workspace switches.
    ///
    /// A hidden window's UnmapNotify is ours: it must stay managed. A visible
    /// window's UnmapNotify came from its client, so it is unmanaged. This is
    /// the invariant that replaced the outstanding-unmap counter, which drifted
    /// whenever a window changed workspace while already hidden and then
    /// dropped a live client.
    fn stays_managed(hidden: &HashSet<Window>, w: Window) -> bool {
        hidden.contains(&w)
    }

    #[test]
    fn a_hidden_window_unmapped_by_us_stays_managed() {
        let mut hidden = HashSet::new();
        hidden.insert(42u32);
        assert!(stays_managed(&hidden, 42));
    }

    #[test]
    fn a_visible_window_that_unmaps_itself_is_unmanaged() {
        let hidden = HashSet::new();
        assert!(!stays_managed(&hidden, 42));
    }

    fn client(win: Window, tag: u32) -> Client {
        Client {
            win,
            monitor: 0,
            tag,
            floating: false,
            fullscreen: false,
            geom: Rect {
                x: 0,
                y: 0,
                w: 100,
                h: 100,
            },
            take_focus: false,
            monitor_name: "DP-1".into(),
            pid: None,
            parent: None,
            focus_on_map: true,
            orphan: false,
            sticky: false,
            home: None,
            size_hints: SizeHints::default(),
            class: String::new(),
            scratch: None,
            scratch_floating: false,
            guest_of: None,
            urgent: false,
            urgent_serial: 0,
            swallowed_by: None,
        }
    }

    #[test]
    fn a_scratchpad_never_takes_a_tile() {
        // 0.17.3: hiding the scratchpad made it a tiled member of the active
        // workspace, so the full-width window under it became half width.
        let withdrawn = HashSet::new();
        let normal = client(1, 0);
        let mut hidden = client(2, 0);
        hidden.scratch = Some(0);
        let mut sent = client(3, 0);
        sent.scratch = Some(4);
        let tiled: Vec<Window> = [&normal, &hidden, &sent]
            .into_iter()
            .filter(|c| tiles(c, 0, 0, &withdrawn))
            .map(|c| c.win)
            .collect();
        assert_eq!(tiled, vec![1], "only the ordinary window is tiled");
        // A full-width single tile stays a single tile.
        let screen = Rect {
            x: 0,
            y: 28,
            w: 2560,
            h: 1412,
        };
        let boxes = Layout::Tile.arrange_master(screen, tiled.len(), 1, 0.55, 8);
        assert_eq!(boxes.len(), 1);
        assert!(boxes[0].w > 2500, "the window keeps the full width");
    }

    #[test]
    fn floating_fullscreen_and_withdrawn_windows_do_not_tile() {
        let mut withdrawn = HashSet::new();
        let mut floating = client(1, 0);
        floating.floating = true;
        let mut full = client(2, 0);
        full.fullscreen = true;
        let gone = client(3, 0);
        withdrawn.insert(3);
        let other = client(4, 1);
        for c in [&floating, &full, &gone, &other] {
            assert!(
                !tiles(c, 0, 0, &withdrawn),
                "window {} must not tile",
                c.win
            );
        }
        assert!(tiles(&other, 0, 1, &withdrawn));
    }

    #[test]
    fn a_dialog_is_suppressed_only_for_a_withdrawn_parent() {
        let mut withdrawn = HashSet::new();
        withdrawn.insert(42u32);
        assert!(suppress_transient(&withdrawn, Some(42)));
        assert!(!suppress_transient(&withdrawn, Some(43)));
        assert!(!suppress_transient(&withdrawn, None));
    }

    #[test]
    fn remapping_clears_the_hidden_record() {
        // Hiding then showing again must leave nothing that could later be
        // mistaken for "we are hiding this" after a rapid switch.
        let mut hidden = HashSet::new();
        let w: Window = 7;
        hidden.insert(w);
        assert!(
            stays_managed(&hidden, w),
            "hidden while on another workspace"
        );
        hidden.remove(&w);
        assert!(
            !stays_managed(&hidden, w),
            "visible again: a later unmap is the client's"
        );
    }

    #[test]
    fn repeated_hide_and_show_is_idempotent() {
        // The old counter grew on every hide and only shrank on an event that
        // could arrive late. Repeating the cycle must not accumulate state.
        let mut hidden = HashSet::new();
        let w: Window = 9;
        for _ in 0..50 {
            assert!(hidden.insert(w), "first hide records the window");
            assert!(!hidden.insert(w), "a second hide adds nothing");
            hidden.remove(&w);
        }
        assert!(hidden.is_empty(), "no leftover bookkeeping after cycles");
    }
}
