//! Automatic display arrangement for `niructl display`.
//!
//! Everything here is pure: no X connection, no files, no clock. The watcher
//! in niructl takes a snapshot of the connected screens, asks this module what
//! the layout should be, applies it and records what it saw. Keeping the
//! decisions pure is what makes them testable without a monitor on the desk.
//!
//! Vocabulary:
//! - A *screen* is a connected output that has modes.
//! - Its *identity* is make, product code, model and serial from the EDID.
//! - Its *key* is the identity as text, or `connector:NAME` without an EDID.
//! - The *effective set* is every usable screen, minus the built-in panel
//!   while the lid is closed.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// Hardware identity of a screen, read from its EDID.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Identity {
    /// Three-letter PNP manufacturer ID, e.g. `DEL`.
    pub manufacturer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

impl Identity {
    /// Stable text form, used as the key in the layout memory.
    pub fn key(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.manufacturer,
            self.product_code
                .map(|code| format!("{code:04x}"))
                .unwrap_or_default(),
            self.model.as_deref().unwrap_or(""),
            self.serial.as_deref().unwrap_or("")
        )
    }

    /// Human-readable form for messages: model and the end of the serial.
    pub fn describe(&self) -> String {
        let name = match &self.model {
            Some(model) if !model.is_empty() => format!("{} {model}", self.manufacturer),
            _ => match self.product_code {
                Some(code) => format!("{} {code:04X}", self.manufacturer),
                None => self.manufacturer.clone(),
            },
        };
        match &self.serial {
            Some(serial) if serial.len() > 4 => {
                format!("{name} (serial …{})", &serial[serial.len() - 4..])
            }
            Some(serial) if !serial.is_empty() => format!("{name} (serial {serial})"),
            _ => name,
        }
    }

    /// Whether a saved identity describes a screen that is connected now.
    ///
    /// Make must agree. Product code and model must agree when both sides
    /// have them. A saved serial needs the same serial; a saved identity
    /// without a serial matches any serial, so a profile saved from a screen
    /// with an unreadable serial still finds it.
    pub fn matches(&self, seen: &Identity) -> bool {
        fn agree<T: PartialEq>(a: &Option<T>, b: &Option<T>) -> bool {
            match (a, b) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
        }
        self.manufacturer == seen.manufacturer
            && agree(&self.product_code, &seen.product_code)
            && agree(&self.model, &seen.model)
            && match (&self.serial, &seen.serial) {
                (Some(saved), Some(now)) => saved == now,
                (Some(_), None) => false,
                (None, _) => true,
            }
    }
}

/// Built-in panels are what a closed lid hides.
pub fn is_internal(connector: &str) -> bool {
    let upper = connector.to_ascii_uppercase();
    upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mode {
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub rates: Vec<f64>,
}

/// What a screen currently shows, if it is on.
#[derive(Clone, Debug, PartialEq)]
pub struct Current {
    pub mode: String,
    pub rate: Option<f64>,
    pub rotation: String,
    pub x: i32,
    pub y: i32,
    pub primary: bool,
}

/// One connected output as the watcher sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    pub connector: String,
    pub identity: Option<Identity>,
    pub modes: Vec<Mode>,
    pub preferred: Option<String>,
    pub current: Option<Current>,
}

impl Screen {
    pub fn key(&self) -> String {
        match &self.identity {
            Some(identity) => identity.key(),
            None => format!("connector:{}", self.connector),
        }
    }
    pub fn internal(&self) -> bool {
        is_internal(&self.connector)
    }
    pub fn usable(&self) -> bool {
        !self.modes.is_empty()
    }
    fn mode(&self, name: &str) -> Option<&Mode> {
        self.modes.iter().find(|mode| mode.name == name)
    }
    fn preferred_mode(&self) -> Option<&Mode> {
        self.preferred
            .as_deref()
            .and_then(|name| self.mode(name))
            .or_else(|| self.modes.first())
    }
}

/// A screen's size on the desktop for a mode and rotation.
pub fn rotated_size(mode: &Mode, rotation: &str) -> (i32, i32) {
    if matches!(rotation, "left" | "right") {
        (mode.height, mode.width)
    } else {
        (mode.width, mode.height)
    }
}

/// The screens that count for matching: usable, and not the built-in panel
/// while the lid is closed.
pub fn effective(screens: &[Screen], lid_closed: bool) -> Vec<&Screen> {
    screens
        .iter()
        .filter(|screen| screen.usable() && !(lid_closed && screen.internal()))
        .collect()
}

/// The effective set as sorted keys: what "the same setup again" means.
pub fn set_signature(screens: &[Screen], lid_closed: bool) -> Vec<String> {
    let mut keys: Vec<String> = effective(screens, lid_closed)
        .iter()
        .map(|screen| screen.key())
        .collect();
    keys.sort();
    keys
}

/// One screen a profile asks for: the connector it was saved on and, for
/// profiles saved by 0.17 or later, the hardware identity.
#[derive(Clone, Debug)]
pub struct Wanted<'a> {
    pub connector: &'a str,
    pub identity: Option<&'a Identity>,
}

/// Pair the screens a profile wants with connected screens.
///
/// Rules, in order:
/// 1. An entry with an identity takes a screen with that identity. When
///    several screens qualify (identical monitors without distinct serials),
///    the one on the connector the profile saved wins; the rest are paired
///    in profile order with the remaining candidates in connector-name order.
/// 2. An entry without an identity (profiles saved before identities) takes
///    the screen on its connector, exactly as before.
///
/// Returns the connector for every entry, or the indices left unmatched.
pub fn assign(
    wanted: &[Wanted],
    screens: &[&Screen],
    taken: &mut HashSet<String>,
) -> Result<Vec<String>, Vec<usize>> {
    let mut result: Vec<Option<String>> = vec![None; wanted.len()];
    // Identity entries whose saved connector holds a matching screen first,
    // so a screen that did not move keeps its place.
    for (index, entry) in wanted.iter().enumerate() {
        let Some(identity) = entry.identity else {
            continue;
        };
        if let Some(screen) = screens.iter().find(|screen| {
            screen.connector == entry.connector
                && !taken.contains(&screen.connector)
                && screen
                    .identity
                    .as_ref()
                    .is_some_and(|seen| identity.matches(seen))
        }) {
            taken.insert(screen.connector.clone());
            result[index] = Some(screen.connector.clone());
        }
    }
    for (index, entry) in wanted.iter().enumerate() {
        if result[index].is_some() {
            continue;
        }
        let Some(identity) = entry.identity else {
            continue;
        };
        let mut candidates: Vec<&&Screen> = screens
            .iter()
            .filter(|screen| {
                !taken.contains(&screen.connector)
                    && screen
                        .identity
                        .as_ref()
                        .is_some_and(|seen| identity.matches(seen))
            })
            .collect();
        candidates.sort_by(|a, b| a.connector.cmp(&b.connector));
        if let Some(screen) = candidates.first() {
            taken.insert(screen.connector.clone());
            result[index] = Some(screen.connector.clone());
        }
    }
    for (index, entry) in wanted.iter().enumerate() {
        if result[index].is_some() || entry.identity.is_some() {
            continue;
        }
        if let Some(screen) = screens.iter().find(|screen| {
            screen.connector == entry.connector && !taken.contains(&screen.connector)
        }) {
            taken.insert(screen.connector.clone());
            result[index] = Some(screen.connector.clone());
        }
    }
    let missing: Vec<usize> = result
        .iter()
        .enumerate()
        .filter(|(_, connector)| connector.is_none())
        .map(|(index, _)| index)
        .collect();
    if missing.is_empty() {
        Ok(result.into_iter().flatten().collect())
    } else {
        Err(missing)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Fit {
    /// Every screen the profile wants is here and nothing else is.
    Exact,
    /// Every wanted screen is here, plus screens the profile does not know.
    Partial { extra: Vec<String> },
    /// Some wanted screens are not connected (or hidden by the lid).
    Missing { entries: Vec<usize> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileMatch {
    pub fit: Fit,
    /// Connector for every enabled entry, in profile order (when not missing).
    pub on: Vec<String>,
    /// Connectors the profile turns off explicitly.
    pub off: Vec<String>,
    /// How many entries were matched by hardware identity.
    pub by_identity: usize,
}

/// Match a profile against the effective set.
pub fn match_profile(
    on: &[Wanted],
    off: &[Wanted],
    screens: &[Screen],
    lid_closed: bool,
) -> ProfileMatch {
    let effective = effective(screens, lid_closed);
    let mut taken = HashSet::new();
    let by_identity = on.iter().filter(|entry| entry.identity.is_some()).count();
    let on_connectors = match assign(on, &effective, &mut taken) {
        Ok(connectors) => connectors,
        Err(entries) => {
            return ProfileMatch {
                fit: Fit::Missing { entries },
                on: Vec::new(),
                off: Vec::new(),
                by_identity,
            }
        }
    };
    // Explicitly disabled screens are optional: a profile saved with the
    // laptop panel off still fits when the panel is hidden by the lid.
    let mut off_connectors = Vec::new();
    for entry in off {
        if let Ok(found) = assign(std::slice::from_ref(entry), &effective, &mut taken) {
            off_connectors.extend(found);
        }
    }
    let extra: Vec<String> = effective
        .iter()
        .filter(|screen| !taken.contains(&screen.connector))
        .map(|screen| screen.connector.clone())
        .collect();
    ProfileMatch {
        fit: if extra.is_empty() {
            Fit::Exact
        } else {
            Fit::Partial { extra }
        },
        on: on_connectors,
        off: off_connectors,
        by_identity,
    }
}

/// A profile as the chooser sees it.
pub struct Candidate<'a> {
    pub name: &'a str,
    /// `auto = false` keeps a profile out of automatic switching.
    pub auto: bool,
    pub matched: &'a ProfileMatch,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    Profile(String),
    /// Several profiles fit equally well and nothing says which one to use.
    Ambiguous(Vec<String>),
    None,
}

/// Choose the profile for the effective set.
///
/// Only exact fits take part, and among them the user's own choices come
/// first: the login profile (at login), then the profile last used with these
/// same screens. Without either, more entries matched by identity rank higher
/// than connector-only entries; a remaining tie is ambiguous and the caller
/// keeps a usable layout instead of guessing.
pub fn choose(candidates: &[Candidate], preferred: Option<&str>, last: Option<&str>) -> Choice {
    let exact: Vec<&Candidate> = candidates
        .iter()
        .filter(|candidate| candidate.auto && candidate.matched.fit == Fit::Exact)
        .collect();
    for hint in [preferred, last].into_iter().flatten() {
        if exact.iter().any(|candidate| candidate.name == hint) {
            return Choice::Profile(hint.to_string());
        }
    }
    let Some(best) = exact.iter().map(|c| c.matched.by_identity).max() else {
        return Choice::None;
    };
    let top: Vec<&str> = exact
        .iter()
        .filter(|candidate| candidate.matched.by_identity == best)
        .map(|candidate| candidate.name)
        .collect();
    if top.len() == 1 {
        return Choice::Profile(top[0].to_string());
    }
    Choice::Ambiguous(top.iter().map(|name| name.to_string()).collect())
}

/// One screen of a proposed layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub connector: String,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    pub rotation: String,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub primary: bool,
}

/// What the watcher remembers about one screen: its last mode, rate and
/// rotation, so a screen that returns without a profile keeps them.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ScreenMemory {
    pub key: String,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    pub rotation: String,
    #[serde(default)]
    pub used: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct OutputMemory {
    pub key: String,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    pub rotation: String,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub primary: bool,
}

/// The last layout seen for one effective set.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LayoutMemory {
    pub set: Vec<String>,
    /// The profile that was current, if the layout was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default)]
    pub outputs: Vec<OutputMemory>,
    #[serde(default)]
    pub used: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Memory {
    #[serde(default)]
    pub screen: Vec<ScreenMemory>,
    #[serde(default)]
    pub layout: Vec<LayoutMemory>,
}

const MAX_SCREENS: usize = 64;
const MAX_LAYOUTS: usize = 32;

impl Memory {
    pub fn screen(&self, key: &str) -> Option<&ScreenMemory> {
        self.screen.iter().find(|screen| screen.key == key)
    }
    pub fn layout(&self, set: &[String]) -> Option<&LayoutMemory> {
        self.layout.iter().find(|layout| layout.set == set)
    }

    /// Record the layout the screens show now. Returns whether anything
    /// changed, so the caller writes the file only then.
    pub fn record(
        &mut self,
        screens: &[Screen],
        lid_closed: bool,
        profile: Option<&str>,
        now: u64,
    ) -> bool {
        let set = set_signature(screens, lid_closed);
        let active: Vec<(&Screen, &Current)> = effective(screens, lid_closed)
            .into_iter()
            .filter_map(|screen| screen.current.as_ref().map(|current| (screen, current)))
            .collect();
        if active.is_empty() || set.is_empty() {
            return false;
        }
        let before = self.clone();
        for (screen, current) in &active {
            let entry = ScreenMemory {
                key: screen.key(),
                mode: current.mode.clone(),
                rate: current.rate,
                rotation: current.rotation.clone(),
                used: now,
            };
            match self.screen.iter_mut().find(|known| known.key == entry.key) {
                Some(known) => {
                    if known.mode != entry.mode
                        || known.rate != entry.rate
                        || known.rotation != entry.rotation
                    {
                        *known = entry;
                    }
                }
                None => self.screen.push(entry),
            }
        }
        let mut outputs: Vec<OutputMemory> = active
            .iter()
            .map(|(screen, current)| OutputMemory {
                key: screen.key(),
                mode: current.mode.clone(),
                rate: current.rate,
                rotation: current.rotation.clone(),
                x: current.x,
                y: current.y,
                primary: current.primary,
            })
            .collect();
        outputs.sort_by(|a, b| (a.y, a.x, &a.key).cmp(&(b.y, b.x, &b.key)));
        let layout = LayoutMemory {
            set: set.clone(),
            profile: profile.map(str::to_string),
            outputs,
            used: now,
        };
        match self.layout.iter_mut().find(|known| known.set == set) {
            Some(known) => {
                if known.outputs != layout.outputs || known.profile != layout.profile {
                    *known = layout;
                }
            }
            None => self.layout.push(layout),
        }
        // Keep the newest entries only. `used` changes only together with
        // the content, so an unchanged layout never rewrites the file.
        self.screen.sort_by_key(|a| std::cmp::Reverse(a.used));
        self.screen.truncate(MAX_SCREENS);
        self.layout.sort_by_key(|a| std::cmp::Reverse(a.used));
        self.layout.truncate(MAX_LAYOUTS);
        *self != before
    }

    /// The remembered layout for the effective set, mapped back onto the
    /// connectors the screens use now.
    pub fn restore(&self, screens: &[Screen], lid_closed: bool) -> Option<Vec<Placement>> {
        let set = set_signature(screens, lid_closed);
        let layout = self.layout(&set)?;
        let effective = effective(screens, lid_closed);
        let mut taken = HashSet::new();
        let mut placements = Vec::new();
        for output in &layout.outputs {
            // Identical keys (twin monitors without serials) are handed out
            // in connector-name order, the same rule `assign` uses.
            let mut candidates: Vec<&&Screen> = effective
                .iter()
                .filter(|screen| screen.key() == output.key && !taken.contains(&screen.connector))
                .collect();
            candidates.sort_by(|a, b| a.connector.cmp(&b.connector));
            let screen = candidates.first()?;
            taken.insert(screen.connector.clone());
            let mode = screen
                .mode(&output.mode)
                .or_else(|| screen.preferred_mode())?;
            let rate = output
                .rate
                .filter(|rate| mode.rates.iter().any(|known| (known - rate).abs() < 0.01));
            placements.push(Placement {
                connector: screen.connector.clone(),
                mode: mode.name.clone(),
                rate,
                rotation: output.rotation.clone(),
                x: output.x,
                y: output.y,
                primary: output.primary,
            });
        }
        (!placements.is_empty()).then_some(placements)
    }
}

/// Keep the usable part of the current layout and add new screens to the
/// right, touching the rightmost screen at its top edge.
///
/// - Screens that disappeared or are hidden by the closed lid are dropped.
/// - A new screen uses the mode, rate and rotation remembered for it, else
///   its preferred mode, upright.
/// - If nothing would be lit, the built-in panel (or the first screen) is
///   turned on, even with the lid closed: the session never goes black.
/// - The primary screen stays primary while it is shown; otherwise the
///   built-in panel, else the leftmost screen, becomes primary.
/// - The result starts at 0,0.
pub fn extend(screens: &[Screen], lid_closed: bool, memory: &Memory) -> Vec<Placement> {
    let effective = effective(screens, lid_closed);
    let mut placements: Vec<Placement> = Vec::new();
    let mut extents: Vec<(i32, i32, i32, i32)> = Vec::new();
    for screen in &effective {
        let Some(current) = &screen.current else {
            continue;
        };
        let Some(mode) = screen.mode(&current.mode) else {
            continue;
        };
        let (width, height) = rotated_size(mode, &current.rotation);
        extents.push((current.x, current.y, width, height));
        placements.push(Placement {
            connector: screen.connector.clone(),
            mode: current.mode.clone(),
            rate: current.rate,
            rotation: current.rotation.clone(),
            x: current.x,
            y: current.y,
            primary: current.primary,
        });
    }
    let mut new_screens: Vec<&&Screen> = effective
        .iter()
        .filter(|screen| !placements.iter().any(|p| p.connector == screen.connector))
        .collect();
    new_screens.sort_by(|a, b| a.connector.cmp(&b.connector));
    if placements.is_empty() && new_screens.is_empty() {
        // Nothing usable in the effective set: light the built-in panel even
        // with the lid closed, or the first usable screen.
        let fallback = screens
            .iter()
            .filter(|screen| screen.usable())
            .min_by_key(|screen| (!screen.internal(), screen.connector.clone()));
        if let Some(screen) = fallback {
            if let Some(placement) = place_new(screen, memory, 0, 0) {
                placements.push(placement);
            }
        }
        return finish(placements);
    }
    for screen in new_screens {
        let (x, y) = extents
            .iter()
            .max_by_key(|(x, _, width, _)| x + width)
            .map(|(x, y, width, _)| (x + width, *y))
            .unwrap_or((0, 0));
        if let Some(placement) = place_new(screen, memory, x, y) {
            if let Some(mode) = screen.mode(&placement.mode) {
                let (width, height) = rotated_size(mode, &placement.rotation);
                extents.push((x, y, width, height));
            }
            placements.push(placement);
        }
    }
    finish(placements)
}

fn place_new(screen: &Screen, memory: &Memory, x: i32, y: i32) -> Option<Placement> {
    let remembered = memory.screen(&screen.key());
    let mode = remembered
        .and_then(|known| screen.mode(&known.mode))
        .or_else(|| screen.preferred_mode())?;
    let rate = remembered
        .filter(|known| known.mode == mode.name)
        .and_then(|known| known.rate)
        .filter(|rate| mode.rates.iter().any(|known| (known - rate).abs() < 0.01));
    let rotation = remembered
        .map(|known| known.rotation.clone())
        .filter(|rotation| matches!(rotation.as_str(), "normal" | "left" | "right" | "inverted"))
        .unwrap_or_else(|| "normal".into());
    Some(Placement {
        connector: screen.connector.clone(),
        mode: mode.name.clone(),
        rate,
        rotation,
        x,
        y,
        primary: false,
    })
}

fn finish(mut placements: Vec<Placement>) -> Vec<Placement> {
    if placements.is_empty() {
        return placements;
    }
    let min_x = placements.iter().map(|p| p.x).min().unwrap_or(0);
    let min_y = placements.iter().map(|p| p.y).min().unwrap_or(0);
    for placement in placements.iter_mut() {
        placement.x -= min_x;
        placement.y -= min_y;
    }
    let primaries = placements.iter().filter(|p| p.primary).count();
    if primaries != 1 {
        for placement in placements.iter_mut() {
            placement.primary = false;
        }
        let index = placements
            .iter()
            .enumerate()
            .min_by_key(|(_, p)| (!is_internal(&p.connector), p.x, p.y))
            .map(|(index, _)| index)
            .unwrap_or(0);
        placements[index].primary = true;
    }
    placements.sort_by(|a, b| a.connector.cmp(&b.connector));
    placements
}

/// Compare what was asked for with what the screens report.
///
/// Returns the connectors that differ. A missing rate in the request means
/// "whatever the driver chose" and is not compared.
pub fn verify(requested: &[Placement], screens: &[Screen]) -> Vec<String> {
    let mut wrong = Vec::new();
    for placement in requested {
        let Some(screen) = screens.iter().find(|s| s.connector == placement.connector) else {
            wrong.push(placement.connector.clone());
            continue;
        };
        let ok = screen.current.as_ref().is_some_and(|current| {
            current.mode == placement.mode
                && current.rotation == placement.rotation
                && current.x == placement.x
                && current.y == placement.y
                && match (placement.rate, current.rate) {
                    (Some(want), Some(have)) => (want - have).abs() < 0.05,
                    (Some(_), None) => false,
                    (None, _) => true,
                }
        });
        if !ok {
            wrong.push(placement.connector.clone());
        }
    }
    for screen in screens {
        if screen.current.is_some() && !requested.iter().any(|p| p.connector == screen.connector) {
            wrong.push(screen.connector.clone());
        }
    }
    wrong.sort();
    wrong.dedup();
    wrong
}

/// A gentler version of a layout for screens that refused it: step 1 drops
/// the refresh rate (the driver picks), step 2 also uses the preferred mode.
/// Positions are kept, so neighbours do not jump. None when nothing is left
/// to relax.
pub fn degrade(
    requested: &[Placement],
    failed: &[String],
    screens: &[Screen],
    step: u32,
) -> Option<Vec<Placement>> {
    if step == 0 || step > 2 || failed.is_empty() {
        return None;
    }
    let mut changed = false;
    let relaxed: Vec<Placement> = requested
        .iter()
        .map(|placement| {
            let mut placement = placement.clone();
            if failed.contains(&placement.connector) {
                if placement.rate.is_some() {
                    placement.rate = None;
                    changed = true;
                }
                if step == 2 {
                    if let Some(preferred) = screens
                        .iter()
                        .find(|s| s.connector == placement.connector)
                        .and_then(|s| s.preferred_mode())
                    {
                        if preferred.name != placement.mode {
                            placement.mode = preferred.name.clone();
                            changed = true;
                        }
                    }
                }
            }
            placement
        })
        .collect();
    changed.then_some(relaxed)
}

/// The retry schedule for a layout that had to be relaxed: 10 s, then
/// doubling up to five minutes. One schedule exists at a time; a new screen
/// set or a manual change clears it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Retry {
    pub attempt: u32,
    pub next: Option<u64>,
}

impl Retry {
    pub const FIRST: u64 = 10;
    pub const LIMIT: u64 = 300;

    pub fn failed(&mut self, now: u64) -> u64 {
        let delay = Self::FIRST
            .saturating_mul(1u64 << self.attempt.min(16))
            .min(Self::LIMIT);
        self.attempt = self.attempt.saturating_add(1);
        self.next = Some(now + delay);
        delay
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn due(&self, now: u64) -> bool {
        self.next.is_some_and(|next| now >= next)
    }
    pub fn seconds_left(&self, now: u64) -> Option<u64> {
        self.next.map(|next| next.saturating_sub(now))
    }
}

/// Describe the screens in a set for a message, e.g. "eDP-1 + DELL U2723QE".
pub fn describe_set(screens: &[Screen], lid_closed: bool) -> String {
    let mut names: BTreeMap<String, usize> = BTreeMap::new();
    for screen in effective(screens, lid_closed) {
        let name = if screen.internal() {
            "built-in screen".to_string()
        } else {
            screen
                .identity
                .as_ref()
                .and_then(|identity| identity.model.clone())
                .filter(|model| !model.is_empty())
                .unwrap_or_else(|| screen.connector.clone())
        };
        *names.entry(name).or_default() += 1;
    }
    names
        .into_iter()
        .map(|(name, count)| {
            if count > 1 {
                format!("{count}× {name}")
            } else {
                name
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(model: &str, serial: Option<&str>) -> Identity {
        Identity {
            manufacturer: "DEL".into(),
            product_code: Some(0x4321),
            model: Some(model.into()),
            serial: serial.map(str::to_string),
        }
    }

    fn mode(name: &str, rates: &[f64]) -> Mode {
        let (w, h) = name.split_once('x').unwrap();
        Mode {
            name: name.into(),
            width: w.parse().unwrap(),
            height: h.parse().unwrap(),
            rates: rates.to_vec(),
        }
    }

    fn screen(connector: &str, identity: Option<Identity>, current: Option<(i32, i32)>) -> Screen {
        Screen {
            connector: connector.into(),
            identity,
            modes: vec![
                mode("2560x1440", &[59.95, 74.97]),
                mode("1920x1080", &[60.0]),
            ],
            preferred: Some("2560x1440".into()),
            current: current.map(|(x, y)| Current {
                mode: "2560x1440".into(),
                rate: Some(59.95),
                rotation: "normal".into(),
                x,
                y,
                primary: x == 0 && y == 0,
            }),
        }
    }

    fn laptop(current: bool) -> Screen {
        Screen {
            connector: "eDP-1".into(),
            identity: Some(Identity {
                manufacturer: "BOE".into(),
                product_code: Some(0x0abc),
                model: None,
                serial: None,
            }),
            modes: vec![mode("1920x1080", &[60.0])],
            preferred: Some("1920x1080".into()),
            current: current.then(|| Current {
                mode: "1920x1080".into(),
                rate: Some(60.0),
                rotation: "normal".into(),
                x: 0,
                y: 0,
                primary: true,
            }),
        }
    }

    #[test]
    fn identity_matching_needs_the_saved_serial_but_tolerates_a_missing_saved_one() {
        let saved = ident("U2723QE", Some("ABC123"));
        assert!(saved.matches(&ident("U2723QE", Some("ABC123"))));
        assert!(!saved.matches(&ident("U2723QE", Some("XYZ999"))));
        assert!(!saved.matches(&ident("U2723QE", None)));
        assert!(ident("U2723QE", None).matches(&ident("U2723QE", Some("ABC123"))));
        assert!(!ident("U2723QE", None).matches(&ident("P2422H", None)));
        assert_eq!(saved.describe(), "DEL U2723QE (serial …C123)");
    }

    #[test]
    fn a_profile_follows_its_screens_across_a_connector_swap() {
        let a = ident("U2723QE", Some("A1"));
        let b = ident("U2723QE", Some("B2"));
        // Saved with A on DP-1 and B on DP-2; now the cables are swapped.
        let screens = vec![
            screen("DP-1", Some(b.clone()), Some((0, 0))),
            screen("DP-2", Some(a.clone()), Some((2560, 0))),
        ];
        let on = [
            Wanted {
                connector: "DP-1",
                identity: Some(&a),
            },
            Wanted {
                connector: "DP-2",
                identity: Some(&b),
            },
        ];
        let m = match_profile(&on, &[], &screens, false);
        assert_eq!(m.fit, Fit::Exact);
        assert_eq!(m.on, vec!["DP-2".to_string(), "DP-1".to_string()]);
        assert_eq!(m.by_identity, 2);
    }

    #[test]
    fn identical_screens_without_serials_keep_their_connector_then_go_in_name_order() {
        let twin = ident("P2422H", None);
        let screens = vec![
            screen("DP-3", Some(twin.clone()), None),
            screen("DP-1", Some(twin.clone()), None),
        ];
        // The profile saved them on DP-1 and DP-9: DP-1 stays, the second
        // entry takes the remaining twin.
        let on = [
            Wanted {
                connector: "DP-9",
                identity: Some(&twin),
            },
            Wanted {
                connector: "DP-1",
                identity: Some(&twin),
            },
        ];
        let m = match_profile(&on, &[], &screens, false);
        assert_eq!(m.on, vec!["DP-3".to_string(), "DP-1".to_string()]);
        // Without a saved connector match, the pairing is by name order.
        let on = [
            Wanted {
                connector: "X",
                identity: Some(&twin),
            },
            Wanted {
                connector: "Y",
                identity: Some(&twin),
            },
        ];
        let m = match_profile(&on, &[], &screens, false);
        assert_eq!(m.on, vec!["DP-1".to_string(), "DP-3".to_string()]);
    }

    #[test]
    fn legacy_entries_match_by_connector_exactly_as_before() {
        let screens = vec![screen("HDMI-1", Some(ident("U2723QE", None)), None)];
        let on = [Wanted {
            connector: "HDMI-1",
            identity: None,
        }];
        assert_eq!(match_profile(&on, &[], &screens, false).fit, Fit::Exact);
        let on = [Wanted {
            connector: "HDMI-2",
            identity: None,
        }];
        assert_eq!(
            match_profile(&on, &[], &screens, false).fit,
            Fit::Missing { entries: vec![0] }
        );
    }

    #[test]
    fn the_closed_lid_hides_the_panel_and_explicit_off_screens_are_optional() {
        let monitor = ident("U2723QE", Some("A1"));
        let screens = vec![laptop(true), screen("HDMI-1", Some(monitor.clone()), None)];
        let panel = laptop(true).identity.unwrap();
        let desk_on = [Wanted {
            connector: "HDMI-1",
            identity: Some(&monitor),
        }];
        let desk_off = [Wanted {
            connector: "eDP-1",
            identity: Some(&panel),
        }];
        // Lid open: the panel is extra unless the profile turns it off.
        assert!(matches!(
            match_profile(&desk_on, &[], &screens, false).fit,
            Fit::Partial { .. }
        ));
        let m = match_profile(&desk_on, &desk_off, &screens, false);
        assert_eq!(m.fit, Fit::Exact);
        assert_eq!(m.off, vec!["eDP-1".to_string()]);
        // Lid closed: the panel does not count, with or without the off list.
        assert_eq!(match_profile(&desk_on, &[], &screens, true).fit, Fit::Exact);
        assert_eq!(
            match_profile(&desk_on, &desk_off, &screens, true).fit,
            Fit::Exact
        );
        // A profile that needs the panel cannot fit while the lid is closed.
        let both = [
            Wanted {
                connector: "eDP-1",
                identity: Some(&panel),
            },
            Wanted {
                connector: "HDMI-1",
                identity: Some(&monitor),
            },
        ];
        assert!(matches!(
            match_profile(&both, &[], &screens, true).fit,
            Fit::Missing { .. }
        ));
    }

    fn exact(by_identity: usize) -> ProfileMatch {
        ProfileMatch {
            fit: Fit::Exact,
            on: vec![],
            off: vec![],
            by_identity,
        }
    }

    #[test]
    fn choosing_prefers_identity_then_login_then_last_use_and_never_guesses() {
        let a = exact(2);
        let b = exact(2);
        let legacy = exact(0);
        let partial = ProfileMatch {
            fit: Fit::Partial {
                extra: vec!["x".into()],
            },
            on: vec![],
            off: vec![],
            by_identity: 3,
        };
        let only = [Candidate {
            name: "Desk",
            auto: true,
            matched: &a,
        }];
        assert_eq!(choose(&only, None, None), Choice::Profile("Desk".into()));
        let with_legacy = [
            Candidate {
                name: "Old",
                auto: true,
                matched: &legacy,
            },
            Candidate {
                name: "Desk",
                auto: true,
                matched: &a,
            },
            Candidate {
                name: "Wide",
                auto: true,
                matched: &partial,
            },
        ];
        assert_eq!(
            choose(&with_legacy, None, None),
            Choice::Profile("Desk".into())
        );
        // The user's own choices beat the identity ranking: the login
        // profile at login, then the profile last used with these screens.
        assert_eq!(
            choose(&with_legacy, Some("Old"), None),
            Choice::Profile("Old".into())
        );
        assert_eq!(
            choose(&with_legacy, None, Some("Old")),
            Choice::Profile("Old".into())
        );
        // A hint that does not fit exactly is ignored.
        assert_eq!(
            choose(&with_legacy, Some("Wide"), None),
            Choice::Profile("Desk".into())
        );
        let tie = [
            Candidate {
                name: "Desk",
                auto: true,
                matched: &a,
            },
            Candidate {
                name: "Talk",
                auto: true,
                matched: &b,
            },
        ];
        assert_eq!(
            choose(&tie, None, None),
            Choice::Ambiguous(vec!["Desk".into(), "Talk".into()])
        );
        assert_eq!(
            choose(&tie, Some("Talk"), None),
            Choice::Profile("Talk".into())
        );
        assert_eq!(
            choose(&tie, None, Some("Talk")),
            Choice::Profile("Talk".into())
        );
        assert_eq!(
            choose(&tie, Some("Nope"), Some("Desk")),
            Choice::Profile("Desk".into())
        );
        let opted_out = [Candidate {
            name: "Desk",
            auto: false,
            matched: &a,
        }];
        assert_eq!(choose(&opted_out, None, None), Choice::None);
    }

    #[test]
    fn extend_keeps_the_layout_and_adds_a_new_screen_to_the_right_with_its_memory() {
        let mut memory = Memory::default();
        let monitor = ident("U2723QE", Some("A1"));
        memory.screen.push(ScreenMemory {
            key: monitor.key(),
            mode: "1920x1080".into(),
            rate: Some(60.0),
            rotation: "inverted".into(),
            used: 1,
        });
        let screens = vec![laptop(true), screen("HDMI-1", Some(monitor), None)];
        let layout = extend(&screens, false, &memory);
        assert_eq!(layout.len(), 2);
        let hdmi = layout.iter().find(|p| p.connector == "HDMI-1").unwrap();
        assert_eq!((hdmi.x, hdmi.y), (1920, 0));
        assert_eq!(hdmi.mode, "1920x1080");
        assert_eq!(hdmi.rotation, "inverted");
        assert_eq!(hdmi.rate, Some(60.0));
        assert!(
            layout
                .iter()
                .find(|p| p.connector == "eDP-1")
                .unwrap()
                .primary
        );
    }

    #[test]
    fn extend_drops_the_panel_under_a_closed_lid_but_never_goes_black() {
        let screens = vec![
            laptop(true),
            screen("HDMI-1", Some(ident("U", None)), Some((1920, 0))),
        ];
        let layout = extend(&screens, true, &Memory::default());
        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].connector, "HDMI-1");
        assert_eq!((layout[0].x, layout[0].y), (0, 0));
        assert!(layout[0].primary);
        // The external screen goes away with the lid still closed: the
        // panel comes back on rather than leaving nothing lit.
        let alone = vec![laptop(false)];
        let layout = extend(&alone, true, &Memory::default());
        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].connector, "eDP-1");
    }

    #[test]
    fn extend_with_the_upper_screen_inverted_keeps_it() {
        // othala: DisplayPort-5 above, inverted; DisplayPort-4 below.
        let mut upper = screen("DisplayPort-5", Some(ident("S27", Some("U"))), Some((0, 0)));
        upper.current.as_mut().unwrap().rotation = "inverted".into();
        upper.current.as_mut().unwrap().primary = false;
        let mut lower = screen(
            "DisplayPort-4",
            Some(ident("S27", Some("L"))),
            Some((0, 1440)),
        );
        lower.current.as_mut().unwrap().primary = true;
        let layout = extend(&[lower, upper], false, &Memory::default());
        let up = layout
            .iter()
            .find(|p| p.connector == "DisplayPort-5")
            .unwrap();
        assert_eq!((up.x, up.y, up.rotation.as_str()), (0, 0, "inverted"));
        let down = layout
            .iter()
            .find(|p| p.connector == "DisplayPort-4")
            .unwrap();
        assert_eq!((down.x, down.y, down.primary), (0, 1440, true));
    }

    #[test]
    fn memory_restores_the_last_layout_for_the_same_screens() {
        let a = ident("U2723QE", Some("A1"));
        let mut screens = vec![
            laptop(true),
            screen("HDMI-1", Some(a.clone()), Some((1920, 0))),
        ];
        screens[1].current.as_mut().unwrap().rotation = "left".into();
        let mut memory = Memory::default();
        assert!(memory.record(&screens, false, None, 5));
        assert!(
            !memory.record(&screens, false, None, 6),
            "an unchanged layout is not rewritten"
        );
        // Later the monitor comes back on another connector, switched off.
        let later = vec![laptop(true), screen("DP-2", Some(a), None)];
        let restored = memory.restore(&later, false).unwrap();
        let dp = restored.iter().find(|p| p.connector == "DP-2").unwrap();
        assert_eq!((dp.x, dp.rotation.as_str()), (1920, "left"));
        // A different set has no memory.
        assert!(memory.restore(&[laptop(true)], false).is_none());
    }

    #[test]
    fn verify_reports_screens_that_did_not_take_the_layout() {
        let screens = vec![
            screen("DP-1", None, Some((0, 0))),
            screen("DP-2", None, Some((2560, 0))),
        ];
        let ok = vec![
            Placement {
                connector: "DP-1".into(),
                mode: "2560x1440".into(),
                rate: Some(59.95),
                rotation: "normal".into(),
                x: 0,
                y: 0,
                primary: true,
            },
            Placement {
                connector: "DP-2".into(),
                mode: "2560x1440".into(),
                rate: None,
                rotation: "normal".into(),
                x: 2560,
                y: 0,
                primary: false,
            },
        ];
        assert!(verify(&ok, &screens).is_empty());
        let mut wrong = ok.clone();
        wrong[1].rate = Some(74.97);
        assert_eq!(verify(&wrong, &screens), vec!["DP-2".to_string()]);
        assert_eq!(
            verify(&ok[..1], &screens),
            vec!["DP-2".to_string()],
            "an extra lit screen"
        );
    }

    #[test]
    fn degrade_drops_the_rate_then_uses_the_preferred_mode() {
        let screens = vec![screen("DP-1", None, None)];
        let want = vec![Placement {
            connector: "DP-1".into(),
            mode: "1920x1080".into(),
            rate: Some(60.0),
            rotation: "normal".into(),
            x: 0,
            y: 0,
            primary: true,
        }];
        let failed = vec!["DP-1".to_string()];
        let first = degrade(&want, &failed, &screens, 1).unwrap();
        assert_eq!((first[0].rate, first[0].mode.as_str()), (None, "1920x1080"));
        let second = degrade(&want, &failed, &screens, 2).unwrap();
        assert_eq!(
            (second[0].rate, second[0].mode.as_str()),
            (None, "2560x1440")
        );
        assert!(degrade(&want, &failed, &screens, 3).is_none());
        assert!(degrade(&want, &[], &screens, 1).is_none());
    }

    #[test]
    fn the_retry_schedule_backs_off_to_five_minutes() {
        let mut retry = Retry::default();
        let delays: Vec<u64> = (0..7).map(|_| retry.failed(100)).collect();
        assert_eq!(delays, vec![10, 20, 40, 80, 160, 300, 300]);
        assert!(retry.due(400));
        assert!(!retry.due(399));
        retry.clear();
        assert!(!retry.due(10_000));
        assert_eq!(retry, Retry::default());
    }

    #[test]
    fn set_signatures_ignore_order_and_the_hidden_panel() {
        let a = screen("DP-1", Some(ident("A", None)), None);
        let b = screen("DP-2", Some(ident("B", None)), None);
        let s1 = set_signature(&[a.clone(), b.clone(), laptop(true)], true);
        let s2 = set_signature(&[laptop(false), b, a], true);
        assert_eq!(s1, s2);
        assert_eq!(s1.len(), 2);
    }
}
