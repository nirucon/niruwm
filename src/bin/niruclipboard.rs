//! niruclipboard: NIRU clipboard history, bounded, local-only and explicit.
//!
//! `niruclipboard daemon` keeps the history in
//! `$XDG_STATE_HOME/niru/clipboard.json` (`~/.local/state/niru/clipboard.json`):
//! a JSON list of `{"id": <ns>, "text": ..., "time": <s>}`, newest first, at
//! most 200 entries of at most 64 KiB, without anything that looks like a
//! secret. NIRULAUNCHER reads the same file and copies an entry back with
//! `niruclipboard copy ID`.
//!
//! The Python version asked `xclip -o` every 0.75 s. This one asks X to say
//! when the CLIPBOARD owner changes (XFixes selection notify) and reads the
//! new text then, so an idle session costs nothing. `copy` serves the text
//! itself from a small background process until another program takes the
//! clipboard, the way `xclip -i` does.

#[path = "../stdout.rs"]
#[macro_use]
mod stdout;

use std::collections::VecDeque;
use std::io::Write as _;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use x11rb::connection::Connection;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, Property,
    SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

const MAX_ENTRIES: usize = 200;
const MAX_BYTES: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(2);

// ── history file ────────────────────────────────────────────────────────────

fn state_path() -> PathBuf {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(base) => PathBuf::from(base),
        None => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"),
    };
    base.join("niru").join("clipboard.json")
}

type Entry = Map<String, Value>;

fn read_entries() -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(state_path()) else {
        return Vec::new();
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            Value::Object(entry) if entry.contains_key("text") => Some(entry),
            _ => None,
        })
        .take(MAX_ENTRIES)
        .collect()
}

/// The layout Python's `json.dump` writes: `", "` between items and
/// `": "` after keys, non-ASCII text as it is.
struct PythonStyle;

impl serde_json::ser::Formatter for PythonStyle {
    fn begin_array_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }
    fn begin_object_key<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> std::io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }
    fn begin_object_value<W: ?Sized + std::io::Write>(
        &mut self,
        writer: &mut W,
    ) -> std::io::Result<()> {
        writer.write_all(b": ")
    }
}

fn encode(entries: &[Entry]) -> Vec<u8> {
    use serde::Serialize as _;
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, PythonStyle);
    let list = &entries[..entries.len().min(MAX_ENTRIES)];
    let _ = list.serialize(&mut serializer);
    out
}

/// Replace the file atomically, readable only by its owner.
fn write_entries(entries: &[Entry]) -> std::io::Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.{}", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        // An old temporary file keeps its mode; make sure.
        file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(&encode(entries))?;
        file.flush()?;
        std::fs::rename(&temp, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

// ── filters ─────────────────────────────────────────────────────────────────

/// Python's `str.split()` whitespace: Unicode whitespace plus the ASCII
/// separators 0x1c..0x1f.
fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// The old filter, `(?i)(password|passwd|secret|api[_-]?key|token|
/// BEGIN [A-Z ]*PRIVATE KEY|authorization:|bearer\s+[A-Za-z0-9._~-]+)`.
fn sensitive(text: &str) -> bool {
    let lower = text.to_lowercase();
    if [
        "password",
        "passwd",
        "secret",
        "apikey",
        "api_key",
        "api-key",
        "token",
        "authorization:",
    ]
    .iter()
    .any(|word| lower.contains(word))
    {
        return true;
    }
    for (at, _) in lower.match_indices("begin ") {
        let rest = &lower[at + "begin ".len()..];
        let run = rest
            .find(|c: char| !(c.is_ascii_lowercase() || c == ' '))
            .unwrap_or(rest.len());
        if rest[..run].contains("private key") {
            return true;
        }
    }
    for (at, _) in lower.match_indices("bearer") {
        let rest = &lower[at + "bearer".len()..];
        let token = rest.trim_start_matches(python_space);
        if token.len() < rest.len()
            && token
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || "._~-".contains(c))
        {
            return true;
        }
    }
    false
}

fn accepted(text: &str) -> bool {
    !text.is_empty() && !text.contains('\0') && text.len() <= MAX_BYTES && !sensitive(text)
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or_default()
}

/// Put `text` first, without an older copy of it.
fn add_text(mut entries: Vec<Entry>, text: &str) -> Vec<Entry> {
    if !accepted(text) {
        return entries;
    }
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    entries.retain(|entry| entry.get("text").and_then(Value::as_str) != Some(text.as_str()));
    let id = now_ns();
    let mut entry = Map::new();
    entry.insert("id".into(), Value::from(id));
    entry.insert("text".into(), Value::from(text));
    entry.insert("time".into(), Value::from(id / 1_000_000_000));
    entries.insert(0, entry);
    entries.truncate(MAX_ENTRIES);
    entries
}

fn preview(text: &str, width: usize) -> String {
    let line = text
        .split(python_space)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if line.chars().count() > width {
        format!("{}…", line.chars().take(width).collect::<String>())
    } else {
        line
    }
}

// ── X selections ────────────────────────────────────────────────────────────

struct Atoms {
    clipboard: Atom,
    utf8: Atom,
    targets: Atom,
    text: Atom,
    plain: Atom,
    plain_utf8: Atom,
    incr: Atom,
    timestamp: Atom,
    property: Atom,
}

impl Atoms {
    fn new(c: &RustConnection) -> Result<Self, Box<dyn std::error::Error>> {
        let get = |name: &str| -> Result<Atom, Box<dyn std::error::Error>> {
            Ok(c.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        Ok(Self {
            clipboard: get("CLIPBOARD")?,
            utf8: get("UTF8_STRING")?,
            targets: get("TARGETS")?,
            text: get("TEXT")?,
            plain: get("text/plain")?,
            plain_utf8: get("text/plain;charset=utf-8")?,
            incr: get("INCR")?,
            timestamp: get("TIMESTAMP")?,
            property: get("_NIRUCLIPBOARD")?,
        })
    }
}

fn window(
    c: &RustConnection,
    screen: usize,
    mask: EventMask,
) -> Result<Window, Box<dyn std::error::Error>> {
    let root = c.setup().roots[screen].root;
    let win = c.generate_id()?;
    c.create_window(
        0,
        win,
        root,
        -10,
        -10,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new().event_mask(mask),
    )?;
    Ok(win)
}

/// Wait for the next event, at most until `deadline`.
fn next_event(c: &RustConnection, deadline: Instant) -> Option<Event> {
    loop {
        if let Ok(Some(event)) = c.poll_for_event() {
            return Some(event);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let mut fd = libc::pollfd {
            fd: c.stream().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut fd, 1, ms) } < 0
            && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            return None;
        }
    }
}

/// Read the clipboard as text, like `xclip -selection clipboard -o`:
/// UTF8_STRING, then STRING, following INCR transfers. Events that are not
/// part of the transfer are kept in `pending`.
fn read_clipboard(
    c: &RustConnection,
    win: Window,
    atoms: &Atoms,
    pending: &mut VecDeque<Event>,
) -> Option<String> {
    for target in [atoms.utf8, AtomEnum::STRING.into()] {
        if let Some(bytes) = convert(c, win, atoms, target, pending)? {
            return Some(String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    None
}

/// `Some(None)`: the owner refused this target. `None`: no answer at all.
fn convert(
    c: &RustConnection,
    win: Window,
    atoms: &Atoms,
    target: Atom,
    pending: &mut VecDeque<Event>,
) -> Option<Option<Vec<u8>>> {
    c.delete_property(win, atoms.property).ok()?;
    c.convert_selection(
        win,
        atoms.clipboard,
        target,
        atoms.property,
        x11rb::CURRENT_TIME,
    )
    .ok()?;
    c.flush().ok()?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let event = next_event(c, deadline)?;
        match event {
            Event::SelectionNotify(e) if e.requestor == win && e.selection == atoms.clipboard => {
                if e.property == x11rb::NONE {
                    return Some(None);
                }
                break;
            }
            other => pending.push_back(other),
        }
    }
    let reply = c
        .get_property(true, win, atoms.property, AtomEnum::ANY, 0, u32::MAX / 4)
        .ok()?
        .reply()
        .ok()?;
    if reply.type_ != atoms.incr {
        return Some(Some(reply.value));
    }
    // INCR: the owner sends chunks, each announced by a new value of the
    // property, and an empty one at the end. A transfer larger than the
    // history accepts is still drained so the owner is not left waiting.
    let mut data = Vec::new();
    let mut too_large = false;
    let deadline = Instant::now() + TIMEOUT * 2;
    loop {
        let event = next_event(c, deadline)?;
        match event {
            Event::PropertyNotify(e)
                if e.window == win
                    && e.atom == atoms.property
                    && e.state == Property::NEW_VALUE =>
            {
                let chunk = c
                    .get_property(true, win, atoms.property, AtomEnum::ANY, 0, u32::MAX / 4)
                    .ok()?
                    .reply()
                    .ok()?;
                c.flush().ok()?;
                if chunk.value.is_empty() {
                    return Some((!too_large).then_some(data));
                }
                if data.len() + chunk.value.len() > MAX_BYTES * 4 {
                    too_large = true;
                    data.clear();
                } else if !too_large {
                    data.extend_from_slice(&chunk.value);
                }
            }
            Event::PropertyNotify(_) => {}
            other => pending.push_back(other),
        }
    }
}

fn daemon() -> i32 {
    let Ok((c, screen)) = x11rb::connect(None) else {
        eprintln!("niruclipboard: cannot open the X display");
        return 1;
    };
    let run = || -> Result<(), Box<dyn std::error::Error>> {
        let atoms = Atoms::new(&c)?;
        let win = window(&c, screen, EventMask::PROPERTY_CHANGE)?;
        c.xfixes_query_version(5, 0)?.reply()?;
        c.xfixes_select_selection_input(
            win,
            atoms.clipboard,
            SelectionEventMask::SET_SELECTION_OWNER
                | SelectionEventMask::SELECTION_WINDOW_DESTROY
                | SelectionEventMask::SELECTION_CLIENT_CLOSE,
        )?;
        c.flush()?;
        let mut pending = VecDeque::new();
        // What is on the clipboard at start is not a new copy.
        let mut last = read_clipboard(&c, win, &atoms, &mut pending);
        loop {
            let event = match pending.pop_front() {
                Some(event) => event,
                None => c.wait_for_event()?,
            };
            let Event::XfixesSelectionNotify(e) = event else {
                continue;
            };
            if e.selection != atoms.clipboard || e.owner == x11rb::NONE {
                continue;
            }
            let Some(current) = read_clipboard(&c, win, &atoms, &mut pending) else {
                continue;
            };
            if last.as_deref() == Some(current.as_str()) {
                continue;
            }
            // Read the file again: `niruclipboard clear` may have run.
            let entries = add_text(read_entries(), &current);
            last = Some(current);
            if let Err(error) = write_entries(&entries) {
                eprintln!("niruclipboard: cannot save the history: {error}");
            }
        }
    };
    match run() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("niruclipboard: {error}");
            1
        }
    }
}

/// Answer one paste request for `text`.
fn serve(c: &RustConnection, atoms: &Atoms, request: &SelectionRequestEvent, text: &[u8]) {
    let property = if request.property == x11rb::NONE {
        request.target
    } else {
        request.property
    };
    let text_targets = [
        atoms.utf8,
        AtomEnum::STRING.into(),
        atoms.text,
        atoms.plain,
        atoms.plain_utf8,
    ];
    let answered = if request.target == atoms.targets {
        let mut list = vec![atoms.targets, atoms.timestamp];
        list.extend(text_targets);
        c.change_property32(
            PropMode::REPLACE,
            request.requestor,
            property,
            AtomEnum::ATOM,
            &list,
        )
        .is_ok()
    } else if text_targets.contains(&request.target) {
        let kind = if request.target == atoms.text || request.target == atoms.plain {
            atoms.utf8
        } else {
            request.target
        };
        c.change_property8(PropMode::REPLACE, request.requestor, property, kind, text)
            .is_ok()
    } else {
        false
    };
    let notify = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: request.time,
        requestor: request.requestor,
        selection: request.selection,
        target: request.target,
        property: if answered { property } else { x11rb::NONE },
    };
    let _ = c.send_event(false, request.requestor, EventMask::NO_EVENT, notify);
    let _ = c.flush();
}

/// Own the clipboard with `text` in a background process, like `xclip -i`.
/// Returns once that process holds the selection.
fn set_clipboard(text: &str) -> bool {
    let mut fds = [0i32; 2];
    // SAFETY: plain pipe and fork; the child only uses its own X connection.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return false;
    }
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return false;
    }
    if pid > 0 {
        unsafe { libc::close(fds[1]) };
        let mut fd = libc::pollfd {
            fd: fds[0],
            events: libc::POLLIN,
            revents: 0,
        };
        let mut byte = [0u8; 1];
        let ok = unsafe { libc::poll(&mut fd, 1, TIMEOUT.as_millis() as i32) } > 0
            && unsafe { libc::read(fds[0], byte.as_mut_ptr() as *mut _, 1) } == 1
            && byte[0] == b'1';
        unsafe { libc::close(fds[0]) };
        return ok;
    }
    // The server process: away from the caller's session and terminal.
    unsafe {
        libc::close(fds[0]);
        libc::setsid();
        let null = libc::open(b"/dev/null\0".as_ptr() as *const libc::c_char, libc::O_RDWR);
        if null >= 0 {
            libc::dup2(null, 0);
            libc::dup2(null, 1);
            libc::dup2(null, 2);
        }
    }
    let report = |ok: bool| unsafe {
        let byte = [if ok { b'1' } else { b'0' }];
        libc::write(fds[1], byte.as_ptr() as *const _, 1);
        libc::close(fds[1]);
    };
    let Ok((c, screen)) = x11rb::connect(None) else {
        report(false);
        unsafe { libc::_exit(1) };
    };
    let held = (|| -> Result<Atoms, Box<dyn std::error::Error>> {
        let atoms = Atoms::new(&c)?;
        let win = window(&c, screen, EventMask::NO_EVENT)?;
        c.set_selection_owner(win, atoms.clipboard, x11rb::CURRENT_TIME)?;
        if c.get_selection_owner(atoms.clipboard)?.reply()?.owner != win {
            return Err("the clipboard was not taken".into());
        }
        Ok(atoms)
    })();
    let Ok(atoms) = held else {
        report(false);
        unsafe { libc::_exit(1) };
    };
    report(true);
    let bytes = text.as_bytes();
    while let Ok(event) = c.wait_for_event() {
        match event {
            Event::SelectionRequest(request) => serve(&c, &atoms, &request, bytes),
            Event::SelectionClear(_) => break,
            _ => {}
        }
    }
    unsafe { libc::_exit(0) }
}

// ── command line ────────────────────────────────────────────────────────────

fn usage() {
    println!("Usage: niruclipboard [daemon|list|copy ID|clear|--version]");
    println!("History is local-only, filtered for obvious secrets, and capped at 200 entries.");
}

fn stamp(seconds: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(seconds, 0)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn show(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => "None".into(),
        Some(other) => other.to_string(),
    }
}

fn main() {
    stdout::exit_on_broken_pipe();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let first = args.first().map(String::as_str);
    let code = match first {
        None | Some("daemon") => daemon(),
        Some("--version" | "version") => {
            println!("NIRUCLIPBOARD {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("list") => {
            for entry in read_entries() {
                let time = entry.get("time").and_then(Value::as_i64).unwrap_or(0);
                let text = entry.get("text").and_then(Value::as_str).unwrap_or("");
                println!(
                    "{}\t{}\t{}",
                    show(entry.get("id")),
                    stamp(time),
                    preview(text, 80)
                );
            }
            0
        }
        Some("copy") if args.len() == 2 => {
            match args[1].trim().trim_start_matches('+').parse::<i64>() {
                Err(_) => {
                    eprintln!("niruclipboard: ID must be an integer");
                    2
                }
                Ok(wanted) => {
                    let entry = read_entries()
                        .into_iter()
                        .find(|entry| entry.get("id").and_then(Value::as_i64) == Some(wanted));
                    match entry {
                        None => {
                            eprintln!("niruclipboard: no such history entry");
                            1
                        }
                        Some(entry) => {
                            let text = entry.get("text").and_then(Value::as_str).unwrap_or("");
                            if set_clipboard(text) {
                                0
                            } else {
                                1
                            }
                        }
                    }
                }
            }
        }
        Some("clear") => match write_entries(&[]) {
            Ok(()) => {
                println!("Clipboard history cleared");
                0
            }
            Err(error) => {
                eprintln!("niruclipboard: {error}");
                1
            }
        },
        _ => {
            usage();
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_secret_filter_matches_the_old_pattern() {
        for text in [
            "my Password is x",
            "PASSWD",
            "top secret",
            "API_KEY=1",
            "apikey",
            "api-key",
            "a token",
            "-----BEGIN RSA PRIVATE KEY-----",
            "begin openssh private key",
            "Authorization: Basic x",
            "Bearer abc.def",
            "bearer\t\tX",
        ] {
            assert!(sensitive(text), "{text}");
        }
        for text in [
            "hello world",
            "BEGIN PUBLIC KEY",
            "BEGIN RSA-PRIVATE KEY",
            "bearer",
            "bearer  !",
            "bearerX",
            "api key",
            "authorization",
        ] {
            assert!(!sensitive(text), "{text}");
        }
    }

    #[test]
    fn accepted_text_is_bounded_and_normalised() {
        assert!(!accepted(""));
        assert!(!accepted("a\0b"));
        assert!(accepted(&"x".repeat(MAX_BYTES)));
        assert!(!accepted(&"x".repeat(MAX_BYTES + 1)));
        let entries = add_text(Vec::new(), "one\r\ntwo\rthree");
        assert_eq!(entries[0]["text"], "one\ntwo\nthree");
        let entries = add_text(entries, "second");
        let entries = add_text(entries, "one\ntwo\nthree");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["text"], "one\ntwo\nthree");
        assert_eq!(entries[1]["text"], "second");
        let mut many = Vec::new();
        for i in 0..250 {
            many = add_text(many, &format!("entry {i}"));
        }
        assert_eq!(many.len(), MAX_ENTRIES);
        assert_eq!(many[0]["text"], "entry 249");
    }

    #[test]
    fn the_file_has_the_python_layout() {
        let mut entry = Map::new();
        entry.insert("id".into(), Value::from(1_700_000_000_123_456_789_i64));
        entry.insert("text".into(), Value::from("hé \"q\"\n"));
        entry.insert("time".into(), Value::from(1_700_000_000));
        let text = String::from_utf8(encode(&[entry.clone(), entry])).unwrap();
        let one = r#"{"id": 1700000000123456789, "text": "hé \"q\"\n", "time": 1700000000}"#;
        assert_eq!(text, format!("[{one}, {one}]"));
        assert_eq!(String::from_utf8(encode(&[])).unwrap(), "[]");
    }

    #[test]
    fn previews_join_whitespace_and_cut() {
        assert_eq!(preview("  a\n\tb\u{1f}c  ", 80), "a b c");
        assert_eq!(preview(&"é".repeat(81), 80), format!("{}…", "é".repeat(80)));
        assert_eq!(preview(&"é".repeat(80), 80), "é".repeat(80));
    }
}
