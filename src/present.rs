//! Presenting: `niructl present on [--pid PID] | off | status`.
//!
//! While a presentation runs (NIRUPRES calls `present on --pid <own pid>`):
//! - Do Not Disturb is on, and `off` puts back the state it found,
//! - the root property `_NIRUWM_PRESENTING` (CARDINAL 1) is set; NIRUBAR
//!   0.27 hides the modules in `[bar] presenting_hide` while it is 1.
//!
//! What `on` found and which process it watches live in a small private file
//! in the runtime directory, `$XDG_RUNTIME_DIR/niruwm/presenting-<display>`.
//! The window manager reads it about once a second: when the watched process
//! has ended (a crash, a kill), it runs `niructl present off --if-pid PID`, so
//! a presentation that dies never leaves DND on or the bar trimmed. Root
//! property and file both outlive a restart in place of the window manager.
//!
//! Windows that set `_NIRUWM_KEEP_MAPPED` (CARDINAL 1) stay mapped outside
//! every screen while their workspace is hidden, so a screen capture of them
//! keeps getting frames; see `Wm::arrange`.

use crate::config::Result;
use std::path::PathBuf;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode, Window};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// Root property: CARDINAL 1 while presenting.
pub const PROPERTY: &str = "_NIRUWM_PRESENTING";
/// Client property: CARDINAL 1 keeps the window mapped while it is hidden.
pub const KEEP_MAPPED: &str = "_NIRUWM_KEEP_MAPPED";

/// What `on` changed and what it watches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// The process whose end turns presenting off.
    pub pid: Option<u32>,
    /// Its start time (field 22 of `/proc/PID/stat`), so a reused PID is not
    /// taken for the presenter.
    pub pid_start: Option<u64>,
    /// DND as `on` found it: `Some(false)` means `off` turns DND off again.
    /// `None`: unknown (no notification provider), left alone.
    pub dnd_before: Option<bool>,
    /// Seconds since the epoch when presenting started.
    pub since: u64,
}

impl State {
    pub fn format(&self) -> String {
        let mut out = String::from("version=1\n");
        if let Some(pid) = self.pid {
            out.push_str(&format!("pid={pid}\n"));
        }
        if let Some(start) = self.pid_start {
            out.push_str(&format!("pid_start={start}\n"));
        }
        if let Some(dnd) = self.dnd_before {
            out.push_str(&format!("dnd_before={}\n", if dnd { "on" } else { "off" }));
        }
        out.push_str(&format!("since={}\n", self.since));
        out
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut state = State::default();
        let mut version = false;
        for line in text.lines() {
            let Some((key, value)) = line.trim().split_once('=') else {
                continue;
            };
            match key {
                "version" => version = value == "1",
                "pid" => state.pid = value.parse().ok().filter(|p| *p > 0),
                "pid_start" => state.pid_start = value.parse().ok(),
                "dnd_before" => {
                    state.dnd_before = match value {
                        "on" => Some(true),
                        "off" => Some(false),
                        _ => None,
                    }
                }
                "since" => state.since = value.parse().unwrap_or(0),
                _ => {}
            }
        }
        version.then_some(state)
    }
}

/// `:0`, `:0.0` and `unix:0` are one display: the file name uses `0`.
pub fn display_key(display: &str) -> String {
    let d = display.trim();
    let after = d.rsplit_once(':').map(|(_, n)| n).unwrap_or(d);
    let number = after.split('.').next().unwrap_or(after);
    let key: String = number
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if key.is_empty() {
        "default".into()
    } else {
        key
    }
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache")
                })
        })
        .join("niruwm")
}

/// `$XDG_RUNTIME_DIR/niruwm/presenting-<display>`.
pub fn path(display: &str) -> PathBuf {
    runtime_dir().join(format!("presenting-{}", display_key(display)))
}

pub fn read(display: &str) -> Option<State> {
    State::parse(&std::fs::read_to_string(path(display)).ok()?)
}

pub fn write(display: &str, state: &State) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let path = path(display);
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let tmp = path.with_extension("new");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(state.format().as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&tmp, &path)
}

pub fn remove(display: &str) {
    let _ = std::fs::remove_file(path(display));
}

/// `(state letter, start time)` from the text of `/proc/PID/stat`. The
/// command name may contain spaces and parentheses, so fields are counted
/// after the last `)`.
pub fn parse_stat(text: &str) -> Option<(char, u64)> {
    let rest = &text[text.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After the name: field 3 (state) is index 0, field 22 (starttime) is 19.
    let state = fields.first()?.chars().next()?;
    let start = fields.get(19)?.parse().ok()?;
    Some((state, start))
}

/// The start time of a live process; `None` when it is gone or a zombie.
pub fn process_start(pid: u32) -> Option<u64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (state, start) = parse_stat(&text)?;
    (state != 'Z' && state != 'X').then_some(start)
}

/// Whether the process `state` watches has ended. `start_now` is what
/// `process_start` reports for it now. Without a watched process nothing
/// ever ends.
pub fn watched_gone(state: &State, start_now: Option<u64>) -> bool {
    match (state.pid, start_now) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(_), Some(now)) => state.pid_start.is_some_and(|then| then != now),
    }
}

/// The state `on` writes. A second `on` while presenting keeps what the
/// first one found (DND was turned on by us, so "on" now says nothing about
/// the user's choice) and only takes over the new watched process.
pub fn plan_on(
    existing: Option<&State>,
    dnd_now: Option<bool>,
    pid: Option<u32>,
    pid_start: Option<u64>,
    now: u64,
) -> State {
    match existing {
        Some(old) => State {
            pid,
            pid_start,
            dnd_before: old.dnd_before,
            since: old.since,
        },
        None => State {
            pid,
            pid_start,
            dnd_before: dnd_now,
            since: now,
        },
    }
}

/// Whether `off` turns DND off again: only when `on` found it off.
pub fn restore_dnd_off(state: Option<&State>) -> bool {
    state.and_then(|s| s.dnd_before) == Some(false)
}

/// The value of the root property: presenting while the first item is 1.
pub fn parse_property(values: &[u32]) -> bool {
    values.first() == Some(&1)
}

/// DND through niructl's own helpers (the ones `niructl dnd` uses); tests
/// replace it.
pub trait Dnd {
    fn is_on(&mut self) -> Option<bool>;
    fn apply(&mut self, on: bool) -> Result<()>;
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn atom(c: &RustConnection, name: &str) -> Result<u32> {
    Ok(c.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

/// Whether the root property says presenting.
pub fn is_on(c: &RustConnection, root: Window) -> Result<bool> {
    let reply = c
        .get_property(false, root, atom(c, PROPERTY)?, AtomEnum::CARDINAL, 0, 1)?
        .reply()?;
    let values: Vec<u32> = reply.value32().map(|v| v.collect()).unwrap_or_default();
    Ok(parse_property(&values))
}

fn set_property(c: &RustConnection, root: Window, on: bool) -> Result<()> {
    let a = atom(c, PROPERTY)?;
    if on {
        c.change_property32(PropMode::REPLACE, root, a, AtomEnum::CARDINAL, &[1])?;
    } else {
        c.delete_property(root, a)?;
    }
    // Applied before this process exits: a reader right after us must see it.
    c.get_input_focus()?.reply()?;
    Ok(())
}

/// Serialise `on` and `off` of one display (the window manager's `off` after
/// a crash racing a new `on`).
fn lock(display: &str) -> Option<std::fs::File> {
    use std::os::unix::fs::DirBuilderExt;
    use std::os::unix::io::AsRawFd;
    let dir = runtime_dir();
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(dir.join(format!("presenting-{}.lock", display_key(display))))
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Some(file);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

pub const HELP: &str = "Usage: niructl present on [--pid PID] | off | status";

/// `niructl present ...`.
pub fn command(c: &RustConnection, root: Window, args: &[String], dnd: &mut dyn Dnd) -> Result<()> {
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let quiet = args.iter().any(|a| a == "--quiet");
    let value_of = |flag: &str| -> Result<Option<u32>> {
        match args.iter().position(|a| a == flag) {
            None => Ok(None),
            Some(i) => args
                .get(i + 1)
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|p| *p > 0)
                .map(Some)
                .ok_or_else(|| format!("{flag} needs a process id\n{HELP}").into()),
        }
    };
    let mut words = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if a == "--pid" || a == "--if-pid" {
            skip = true;
            continue;
        }
        if !a.starts_with("--") {
            words.push(a.as_str());
        }
    }
    let action = words.first().copied().unwrap_or("status");
    if words.len() > 1 {
        return Err(HELP.into());
    }
    match action {
        "status" => {
            println!("{}", if is_on(c, root)? { "on" } else { "off" });
            Ok(())
        }
        "on" => {
            let pid = value_of("--pid")?;
            let pid_start = match pid {
                Some(p) => {
                    Some(process_start(p).ok_or_else(|| format!("process {p} is not running"))?)
                }
                None => None,
            };
            let _guard = lock(&display);
            // A file without the root property is from an earlier X session.
            let existing = read(&display).filter(|_| is_on(c, root).unwrap_or(false));
            let dnd_now = if existing.is_some() {
                None // kept from the first `on`, see plan_on
            } else {
                dnd.is_on()
            };
            let state = plan_on(existing.as_ref(), dnd_now, pid, pid_start, now_secs());
            write(&display, &state)?;
            if let Err(e) = dnd.apply(true) {
                eprintln!("niructl: present: Do Not Disturb is unavailable: {e}");
            }
            set_property(c, root, true)?;
            if !quiet {
                println!("presenting on");
            }
            Ok(())
        }
        "off" => {
            let only_for = value_of("--if-pid")?;
            let _guard = lock(&display);
            let state = read(&display);
            if let Some(p) = only_for {
                // The window manager's crash cleanup: a newer `on` for another
                // process owns presenting now.
                if state.as_ref().and_then(|s| s.pid) != Some(p) {
                    return Ok(());
                }
            }
            if restore_dnd_off(state.as_ref()) {
                if let Err(e) = dnd.apply(false) {
                    eprintln!("niructl: present: could not turn Do Not Disturb off: {e}");
                }
            }
            remove(&display);
            // At logout the X server may already be gone; the file is what
            // matters then.
            let _ = set_property(c, root, false);
            if !quiet {
                println!("presenting off");
            }
            Ok(())
        }
        _ => Err(HELP.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_round_trips() {
        let s = State {
            pid: Some(4242),
            pid_start: Some(987654),
            dnd_before: Some(false),
            since: 1_800_000_000,
        };
        assert_eq!(State::parse(&s.format()), Some(s));
        let bare = State::default();
        assert_eq!(State::parse(&bare.format()), Some(bare));
        assert_eq!(State::parse("pid=1\n"), None, "no version line");
        assert_eq!(State::parse("version=2\npid=1\n"), None);
    }

    #[test]
    fn displays_share_one_key() {
        assert_eq!(display_key(":0"), "0");
        assert_eq!(display_key(":0.0"), "0");
        assert_eq!(display_key("unix:0"), "0");
        assert_eq!(display_key(":57"), "57");
        assert_eq!(display_key(""), "default");
    }

    #[test]
    fn stat_is_read_after_the_command_name() {
        let line = "1234 (Web Content (x)) S 1 1234 1234 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 7 0 555123 1000 100";
        assert_eq!(parse_stat(line), Some(('S', 555123)));
        assert_eq!(parse_stat("garbage"), None);
        assert_eq!(parse_stat("1 (x) Z"), None, "too short");
    }

    #[test]
    fn this_process_is_alive_and_its_start_is_stable() {
        let me = std::process::id();
        let start = process_start(me).expect("own /proc entry");
        assert_eq!(process_start(me), Some(start));
        let state = State {
            pid: Some(me),
            pid_start: Some(start),
            ..State::default()
        };
        assert!(!watched_gone(&state, process_start(me)));
    }

    #[test]
    fn a_watched_process_is_gone_when_missing_or_reused() {
        let s = State {
            pid: Some(10),
            pid_start: Some(500),
            ..State::default()
        };
        assert!(!watched_gone(&s, Some(500)));
        assert!(watched_gone(&s, None), "process ended");
        assert!(
            watched_gone(&s, Some(900)),
            "the PID belongs to another process now"
        );
        let unwatched = State::default();
        assert!(
            !watched_gone(&unwatched, None),
            "manual on never ends by itself"
        );
    }

    #[test]
    fn on_remembers_the_first_dnd_state_and_off_restores_only_what_it_changed() {
        let first = plan_on(None, Some(false), Some(10), Some(1), 100);
        assert_eq!(first.dnd_before, Some(false));
        assert!(restore_dnd_off(Some(&first)));
        // A second `on` (DND is on now, because of us) keeps "was off" and
        // takes over the new process.
        let second = plan_on(Some(&first), Some(true), Some(20), Some(2), 200);
        assert_eq!(second.dnd_before, Some(false));
        assert_eq!((second.pid, second.since), (Some(20), 100));
        // DND was already on: off leaves it on.
        let was_on = plan_on(None, Some(true), None, None, 100);
        assert!(!restore_dnd_off(Some(&was_on)));
        // No provider: nothing to restore; no state: nothing either.
        assert!(!restore_dnd_off(Some(&plan_on(None, None, None, None, 1))));
        assert!(!restore_dnd_off(None));
    }

    #[test]
    fn the_property_is_a_cardinal_one() {
        assert!(parse_property(&[1]));
        assert!(!parse_property(&[0]));
        assert!(!parse_property(&[]));
        assert!(!parse_property(&[2]));
    }
}
