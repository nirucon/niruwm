//! Studio (DAW) mode: `niructl studio on|off|toggle|status`.
//!
//! One switch for recording sessions. While it is on:
//! - the screen saver, idle lock and display-off wait (NIRUIDLE honours the
//!   session-scoped root property `_NIRU_IDLE_INHIBIT`),
//! - Do Not Disturb is on (no popups, no notification sounds),
//! - the compositor (picom) of this display is stopped,
//! - the power profile is the configured one (`performance`).
//!
//! Turning it off puts back exactly what it changed, item by item, and leaves
//! alone anything the user changed in the meantime.
//!
//! All state lives on the X root window, never in a file: `_NIRUWM_STUDIO`
//! ("on"/"off", published "off" by NIRUWM 0.20 so its presence means the
//! session supports studio mode) and `_NIRUWM_STUDIO_SAVED` (what was
//! changed and how to undo it). Root properties survive a window manager
//! crash and restart, and vanish with the X server, so the mode is per
//! session and the idle inhibition can never outlive a logout. The session
//! script runs `niructl studio off --session-end` on logout to put DND and the
//! power profile back.

use crate::config::{Config, Result, StudioConfig};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode, Window};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

pub const SWITCH: &str = "_NIRUWM_STUDIO";
pub const SAVED: &str = "_NIRUWM_STUDIO_SAVED";
/// Session-scoped idle inhibition read by NIRUIDLE 0.20; the value names who
/// holds it.
pub const IDLE_INHIBIT: &str = "_NIRU_IDLE_INHIBIT";
/// Published by NIRUIDLE 0.20: the features of the running idle daemon.
pub const IDLE_FEATURES: &str = "_NIRUIDLE_FEATURES";
const HOLDER: &str = "studio";

/// What studio mode changed, and what to put back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Saved {
    /// Turned on because a DAW window appeared (`[studio] auto_enable`).
    pub auto: bool,
    /// The idle inhibition property was set by studio mode.
    pub idle: bool,
    /// DND was off and studio mode turned it on.
    pub dnd: bool,
    /// picom was running and studio mode stopped it; its command line.
    pub picom: Option<Vec<String>>,
    /// `(previous, selected)` power profiles, when studio mode changed it.
    pub power: Option<(String, String)>,
    /// Short notes about items that were skipped, for `status`.
    pub notes: Vec<String>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string()
}

impl Saved {
    /// `key=value` lines; the command line is hex so any byte survives.
    pub fn format(&self) -> String {
        let mut out = String::from("version=1\n");
        out.push_str(&format!("auto={}\n", u8::from(self.auto)));
        out.push_str(&format!("idle={}\n", u8::from(self.idle)));
        out.push_str(&format!("dnd={}\n", u8::from(self.dnd)));
        if let Some(argv) = &self.picom {
            out.push_str(&format!("picom={}\n", hex(argv.join("\0").as_bytes())));
        }
        if let Some((before, now)) = &self.power {
            out.push_str(&format!(
                "power_prev={}\npower_set={}\n",
                clean(before),
                clean(now)
            ));
        }
        for note in &self.notes {
            out.push_str(&format!("note={}\n", clean(note)));
        }
        out
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut saved = Saved::default();
        let mut version = false;
        let mut prev = None;
        let mut set = None;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "version" => version = value == "1",
                "auto" => saved.auto = value == "1",
                "idle" => saved.idle = value == "1",
                "dnd" => saved.dnd = value == "1",
                "picom" => {
                    let bytes = unhex(value)?;
                    let text = String::from_utf8(bytes).ok()?;
                    let argv: Vec<String> = text.split('\0').map(str::to_string).collect();
                    if argv.first().is_some_and(|a| !a.is_empty()) {
                        saved.picom = Some(argv);
                    }
                }
                "power_prev" => prev = Some(value.to_string()),
                "power_set" => set = Some(value.to_string()),
                "note" => saved.notes.push(value.to_string()),
                _ => {}
            }
        }
        if let (Some(prev), Some(set)) = (prev, set) {
            if !prev.is_empty() && !set.is_empty() {
                saved.power = Some((prev, set));
            }
        }
        version.then_some(saved)
    }
}

/// `:0`, `:0.0` and `unix:0` are the same display for this purpose.
pub fn same_display(a: &str, b: &str) -> bool {
    fn norm(d: &str) -> String {
        let d = d.trim();
        let (host, rest) = d.rsplit_once(':').unwrap_or(("", d));
        let host = if host == "unix" { "" } else { host };
        let number = rest.split('.').next().unwrap_or(rest);
        format!("{host}:{number}")
    }
    !a.trim().is_empty() && norm(a) == norm(b)
}

fn proc_uid(dir: &Path) -> Option<u32> {
    let status = std::fs::read_to_string(dir.join("status")).ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn proc_display(dir: &Path) -> Option<String> {
    let env = std::fs::read(dir.join("environ")).ok()?;
    env.split(|b| *b == 0).find_map(|entry| {
        entry
            .strip_prefix(b"DISPLAY=")
            .map(|v| String::from_utf8_lossy(v).to_string())
    })
}

/// picom processes of this user on this display, oldest first, with their
/// command lines. Another X session's compositor is never touched.
pub fn session_picoms(display: &str) -> Vec<(u32, Vec<String>)> {
    let uid = unsafe { libc::getuid() };
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let dir = entry.path();
        let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default();
        if comm.trim() != "picom" || proc_uid(&dir) != Some(uid) {
            continue;
        }
        if !proc_display(&dir).is_some_and(|d| same_display(&d, display)) {
            continue;
        }
        let cmdline = std::fs::read(dir.join("cmdline")).unwrap_or_default();
        let argv: Vec<String> = cmdline
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).to_string())
            .collect();
        if argv.is_empty() {
            continue;
        }
        found.push((pid, argv));
    }
    found.sort_by_key(|(pid, _)| *pid);
    found
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
        && std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| {
                s.rsplit_once(')')
                    .map(|(_, r)| r.trim_start().starts_with('Z'))
            })
            != Some(true)
}

/// Stop picom processes: TERM, then KILL after three seconds.
fn stop_picoms(pids: &[u32]) -> bool {
    for &pid in pids {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while pids.iter().any(|&p| alive(p)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    for &pid in pids {
        if alive(pid) {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
    std::thread::sleep(Duration::from_millis(50));
    !pids.iter().any(|&p| alive(p))
}

/// Start picom again with the command line it had.
fn start_picom(argv: &[String], display: &str) -> std::result::Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", argv[0]))?;
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(50));
        if !session_picoms(display).is_empty() {
            // Reap it later from init; this process exits soon.
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            // `picom -b` forks and the parent exits 0.
            if !status.success() {
                return Err(format!("picom exited: {status}"));
            }
        }
    }
    if session_picoms(display).is_empty() {
        Err("picom did not start again".into())
    } else {
        Ok(())
    }
}

pub struct Session<'a> {
    pub c: &'a RustConnection,
    pub root: Window,
    pub display: String,
}

impl Session<'_> {
    fn atom(&self, name: &str) -> Result<u32> {
        Ok(self.c.intern_atom(false, name.as_bytes())?.reply()?.atom)
    }

    pub fn get(&self, name: &str) -> Result<Option<String>> {
        let atom = self.atom(name)?;
        let reply = self
            .c
            .get_property(false, self.root, atom, AtomEnum::ANY, 0, 16384)?
            .reply()?;
        if reply.type_ == 0 {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&reply.value).to_string()))
    }

    pub fn set(&self, name: &str, value: &str) -> Result<()> {
        let atom = self.atom(name)?;
        let utf8 = self.atom("UTF8_STRING")?;
        self.c
            .change_property8(PropMode::REPLACE, self.root, atom, utf8, value.as_bytes())?;
        self.sync()
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let atom = self.atom(name)?;
        self.c.delete_property(self.root, atom)?;
        self.sync()
    }

    /// Wait until the server has applied every request: `flush` only hands
    /// them to the socket, and a reader that asks right after this process
    /// exits (the launcher, a script, a test) could still see the old value.
    fn sync(&self) -> Result<()> {
        self.c.get_input_focus()?.reply()?;
        Ok(())
    }

    /// `Some(true/false)` from `_NIRUWM_STUDIO`, `None` when the running
    /// window manager does not publish it (older than 0.20).
    pub fn switch(&self) -> Result<Option<bool>> {
        Ok(self.get(SWITCH)?.map(|t| t.trim() == "on"))
    }

    fn saved(&self) -> Result<Option<Saved>> {
        Ok(self.get(SAVED)?.and_then(|t| Saved::parse(&t)))
    }

    fn save(&self, saved: &Saved) -> Result<()> {
        self.set(SAVED, &saved.format())
    }
}

/// What studio mode needs from the rest of the suite; the real one runs
/// niructl's own helpers, tests can replace it.
pub trait Effects {
    fn dnd_is_on(&mut self) -> Option<bool>;
    fn dnd_apply(&mut self, on: bool) -> Result<()>;
    /// `(active, available)` power profiles, or why there are none.
    fn power(&mut self) -> std::result::Result<(String, Vec<String>), String>;
    fn set_power(&mut self, profile: &str) -> Result<()>;
    /// The profile to put back when studio mode ends, given the one it
    /// found. With `[battery] auto_profile` that is the profile of the power
    /// source now (it may have been unplugged meanwhile), unless a profile
    /// chosen by hand holds.
    fn restore_profile(&mut self, before: &str) -> String {
        before.to_string()
    }
}

/// Serialise studio commands of this display (a key pressed twice, a DAW
/// window opening while the launcher toggles).
fn lock(display: &str) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let name: String = display
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let path = dir.join(format!("niruwm-studio-{name}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some(file);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub struct Options {
    pub auto: bool,
    pub session_end: bool,
}

/// Turn studio mode on. Returns the lines to print.
pub fn on(
    s: &Session,
    cfg: &StudioConfig,
    fx: &mut dyn Effects,
    opt: &Options,
) -> Result<Vec<String>> {
    let _guard = lock(&s.display);
    if s.switch()?.is_none() {
        return Err(
            "the running window manager has no studio mode; log out/in to activate NIRUWM 0.20"
                .into(),
        );
    }
    if opt.auto && !cfg.auto_enable {
        return Ok(vec![
            "studio auto-enable is off ([studio] auto_enable)".into()
        ]);
    }
    if s.switch()? == Some(true) {
        if let Some(mut saved) = s.saved()? {
            // Asked for by hand: it now stays on when the DAW closes.
            if saved.auto && !opt.auto {
                saved.auto = false;
                s.save(&saved)?;
            }
        }
        return Ok(vec!["studio on".into()]);
    }
    let mut saved = Saved {
        auto: opt.auto,
        ..Saved::default()
    };
    let mut lines = Vec::new();
    // Each step is recorded at once, so an interrupted command can still be
    // undone by `studio off`.
    if cfg.idle {
        s.set(IDLE_INHIBIT, HOLDER)?;
        saved.idle = true;
        s.save(&saved)?;
        if s.get(IDLE_FEATURES)?.is_none() {
            saved.notes.push(
                "NIRUIDLE 0.20 is not running in this session; the idle lock may still start until you log out and in".into(),
            );
        }
    }
    if cfg.dnd {
        match fx.dnd_is_on() {
            Some(false) => match fx.dnd_apply(true) {
                Ok(()) => saved.dnd = true,
                Err(e) => saved.notes.push(format!("DND: {e}")),
            },
            Some(true) => saved.notes.push("DND was already on; left on".into()),
            None => saved
                .notes
                .push("DND: no notification daemon answered".into()),
        }
        s.save(&saved)?;
    }
    if let Some(target) = cfg.power_profile() {
        match fx.power() {
            Ok((active, _)) if active == target => {
                saved
                    .notes
                    .push(format!("power profile was already {target}"));
            }
            Ok((active, profiles)) if profiles.iter().any(|p| p == target) => {
                match fx.set_power(target) {
                    Ok(()) => saved.power = Some((active, target.to_string())),
                    Err(e) => saved.notes.push(format!("power profile: {e}")),
                }
            }
            Ok(_) => saved
                .notes
                .push(format!("power profile {target} is not offered here")),
            Err(e) => saved.notes.push(format!("power profile: {e}")),
        }
        s.save(&saved)?;
    }
    if cfg.compositor {
        let running = session_picoms(&s.display);
        if let Some((_, argv)) = running.first() {
            saved.picom = Some(argv.clone());
            s.save(&saved)?;
            let pids: Vec<u32> = running.iter().map(|(p, _)| *p).collect();
            if !stop_picoms(&pids) {
                saved.notes.push("picom did not stop".into());
            }
        } else {
            saved.notes.push("compositor was not running".into());
        }
        s.save(&saved)?;
    }
    s.set(SWITCH, "on")?;
    lines.push("studio on".into());
    lines.extend(describe(&saved));
    Ok(lines)
}

/// Turn studio mode off and put back what it changed.
pub fn off(s: &Session, fx: &mut dyn Effects, opt: &Options) -> Result<Vec<String>> {
    let _guard = lock(&s.display);
    let switch = s.switch()?;
    let saved = s.saved()?;
    if opt.auto && !saved.as_ref().is_some_and(|saved| saved.auto) {
        // Turned on by hand: closing the DAW leaves it on.
        return Ok(vec![format!(
            "studio {}",
            if switch == Some(true) { "on" } else { "off" }
        )]);
    }
    let Some(saved) = saved else {
        if switch == Some(true) {
            s.set(SWITCH, "off")?;
        }
        if s.get(IDLE_INHIBIT)?.as_deref() == Some(HOLDER) {
            s.delete(IDLE_INHIBIT)?;
        }
        return Ok(vec!["studio off".into()]);
    };
    let mut lines = vec!["studio off".to_string()];
    // In reverse order of `on`.
    if let Some(argv) = &saved.picom {
        if opt.session_end {
            // The session stops the compositor itself on logout.
        } else if !session_picoms(&s.display).is_empty() {
            lines.push("compositor: already running again; left as it is".into());
        } else {
            match start_picom(argv, &s.display) {
                Ok(()) => lines.push("compositor: started again".into()),
                Err(e) => lines.push(format!("compositor: {e}")),
            }
        }
    }
    if let Some((before, set)) = &saved.power {
        let back = fx.restore_profile(before);
        match fx.power() {
            Ok((active, _)) if active == *set && back == *set => {
                lines.push(format!("power profile: {back} (follows the power source)"))
            }
            Ok((active, _)) if active == *set => match fx.set_power(&back) {
                Ok(()) if back == *before => lines.push(format!("power profile: {before} again")),
                Ok(()) => lines.push(format!("power profile: {back} (follows the power source)")),
                Err(e) => lines.push(format!("power profile: {e}")),
            },
            Ok((active, _)) => lines.push(format!(
                "power profile: changed to {active} meanwhile; left as it is"
            )),
            Err(e) => lines.push(format!("power profile: {e}")),
        }
    }
    if saved.dnd {
        match fx.dnd_is_on() {
            Some(true) => match fx.dnd_apply(false) {
                Ok(()) => lines.push("DND: off again".into()),
                Err(e) => lines.push(format!("DND: {e}")),
            },
            Some(false) => lines.push("DND: already off".into()),
            None => lines.push("DND: no notification daemon answered".into()),
        }
    }
    if saved.idle && s.get(IDLE_INHIBIT)?.as_deref() == Some(HOLDER) {
        s.delete(IDLE_INHIBIT)?;
        lines.push("idle: screen saver and lock active again".into());
    }
    s.delete(SAVED)?;
    s.set(SWITCH, "off")?;
    Ok(lines)
}

/// Human lines about what studio mode holds.
pub fn describe(saved: &Saved) -> Vec<String> {
    let mut lines = Vec::new();
    if saved.idle {
        lines.push("idle: screen saver, lock and display-off paused".into());
    }
    if saved.dnd {
        lines.push("DND: on (was off)".into());
    }
    if let Some((before, now)) = &saved.power {
        lines.push(format!("power profile: {now} (was {before})"));
    }
    if let Some(argv) = &saved.picom {
        lines.push(format!("compositor: stopped ({})", argv.join(" ")));
    }
    if saved.auto {
        lines.push("turned on by a DAW window; off when the last one closes".into());
    }
    for note in &saved.notes {
        lines.push(format!("note: {note}"));
    }
    lines
}

pub fn status(s: &Session) -> Result<Vec<String>> {
    match s.switch()? {
        None => Err(
            "the running window manager has no studio mode; log out/in to activate NIRUWM 0.20"
                .into(),
        ),
        Some(false) => Ok(vec!["studio off".into()]),
        Some(true) => {
            let mut lines = vec!["studio on".to_string()];
            if let Some(saved) = s.saved()? {
                lines.extend(describe(&saved));
            }
            Ok(lines)
        }
    }
}

/// `niructl studio …`
pub fn command(
    c: &RustConnection,
    root: Window,
    args: &[String],
    fx: &mut dyn Effects,
) -> Result<()> {
    const HELP: &str = "Usage: niructl studio on|off|toggle|status [--auto] [--quiet]";
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let s = Session { c, root, display };
    let flag = |f: &str| args.iter().any(|a| a == f);
    let opt = Options {
        auto: flag("--auto"),
        session_end: flag("--session-end"),
    };
    let quiet = flag("--quiet");
    let action = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(String::as_str)
        .unwrap_or("status");
    let cfg = || Config::load().map(|c| c.studio).unwrap_or_default();
    let lines = match action {
        "status" => status(&s)?,
        "on" => on(&s, &cfg(), fx, &opt)?,
        "off" => off(&s, fx, &opt)?,
        "toggle" => {
            if s.switch()? == Some(true) {
                off(&s, fx, &opt)?
            } else {
                on(&s, &cfg(), fx, &opt)?
            }
        }
        _ => return Err(HELP.into()),
    };
    if quiet {
        if let Some(first) = lines.first() {
            println!("{first}");
        }
    } else {
        for line in lines {
            println!("{line}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_round_trips_any_command_line() {
        let saved = Saved {
            auto: true,
            idle: true,
            dnd: true,
            picom: Some(vec![
                "/usr/bin/picom".into(),
                "--config".into(),
                "/home/n/my picom=1.conf".into(),
            ]),
            power: Some(("balanced".into(), "performance".into())),
            notes: vec!["a note".into()],
        };
        assert_eq!(Saved::parse(&saved.format()), Some(saved));
        let empty = Saved::default();
        assert_eq!(Saved::parse(&empty.format()), Some(empty));
        assert_eq!(Saved::parse("auto=1\n"), None, "no version");
        assert_eq!(Saved::parse("version=1\npicom=zz\n"), None, "bad hex");
    }

    #[test]
    fn displays_compare_by_number() {
        assert!(same_display(":0", ":0.0"));
        assert!(same_display(":171", ":171"));
        assert!(!same_display(":0", ":171"));
        assert!(!same_display(":1", ":10"));
        assert!(!same_display("", ":0"));
        assert!(same_display("unix:3", ":3"));
    }

    #[test]
    fn studio_defaults_are_conservative() {
        let d = StudioConfig::default();
        assert!(d.idle && d.dnd && d.compositor);
        assert_eq!(d.power_profile(), Some("performance"));
        assert!(!d.auto_enable, "a DAW window never turns it on by default");
        assert!(d.is_daw("reaper\0REAPER\0"));
        assert!(!d.is_daw("kitty\0kitty\0"));
        let keep = StudioConfig {
            power_profile: "keep".into(),
            ..d.clone()
        };
        assert_eq!(keep.power_profile(), None);
        let none = StudioConfig {
            daw_classes: Vec::new(),
            ..d
        };
        assert!(!none.is_daw("reaper\0REAPER\0"));
    }
}
