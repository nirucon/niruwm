//! `org.freedesktop.ScreenSaver` on the session bus, for NIRUIDLE.
//!
//! Browsers (Chromium, Helium, Firefox), mpv, VLC and video call clients ask
//! the desktop not to start the screensaver while they play a video or hold a
//! call. They do that over D-Bus: `Inhibit(application, reason)` returns a
//! cookie and `UnInhibit(cookie)` ends the request. NIRUIDLE owns the name
//! when nobody else does and keeps a table of the requests; while it is not
//! empty the idle ladder (screensaver, lock, display off) is paused, like
//! `niructl idle on`.
//!
//! A program that crashes never calls `UnInhibit`. Every request remembers
//! the caller's unique bus name, and the bus says when that name goes away
//! (`NameOwnerChanged`), so a crashed program's requests end with it.
//!
//! This is a small, synchronous D-Bus client: one Unix socket, EXTERNAL
//! authentication, and only the message types the service needs. It runs on
//! its own thread that sleeps in a blocking read, so it costs no CPU while
//! nothing happens and adds no dependency to the static binaries. When there
//! is no session bus the thread says so once and ends; NIRUIDLE then works
//! exactly as before.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

pub const SERVICE: &str = "org.freedesktop.ScreenSaver";
const INTERFACE: &str = "org.freedesktop.ScreenSaver";
const PATHS: [&str; 2] = ["/org/freedesktop/ScreenSaver", "/ScreenSaver"];
const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";

/// At most this many requests in all, and per caller. Real programs hold one
/// or two; the limit only keeps a misbehaving client from growing the table.
const MAX_TOTAL: usize = 256;
const MAX_PER_OWNER: usize = 16;
/// Longest application name or reason kept (characters).
const MAX_TEXT: usize = 80;
/// A message larger than this is read and dropped, never kept in memory.
const MAX_MESSAGE: usize = 1 << 20;

/// One request to keep the screen awake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inhibitor {
    pub cookie: u32,
    /// The caller's unique bus name (`:1.42`).
    pub owner: String,
    pub app: String,
    pub reason: String,
}

/// The current requests, by cookie and by owner.
#[derive(Debug, Default)]
pub struct Inhibitors {
    next: u32,
    list: Vec<Inhibitor>,
}

/// Printable, single-line, bounded text for the log and the root property.
pub fn clean_text(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TEXT)
        .collect();
    cleaned.trim().to_string()
}

impl Inhibitors {
    /// Add a request and return its cookie (never 0).
    pub fn add(&mut self, owner: &str, app: &str, reason: &str) -> Result<u32, &'static str> {
        if self.list.len() >= MAX_TOTAL {
            return Err("too many inhibitors");
        }
        if self.list.iter().filter(|i| i.owner == owner).count() >= MAX_PER_OWNER {
            return Err("too many inhibitors from this connection");
        }
        let mut cookie = self.next;
        loop {
            cookie = cookie.wrapping_add(1);
            if cookie != 0 && !self.list.iter().any(|i| i.cookie == cookie) {
                break;
            }
        }
        self.next = cookie;
        let app = clean_text(app);
        self.list.push(Inhibitor {
            cookie,
            owner: owner.to_string(),
            app: if app.is_empty() {
                owner.to_string()
            } else {
                app
            },
            reason: clean_text(reason),
        });
        Ok(cookie)
    }

    /// End a request. Only the connection that made it can end it, so one
    /// program cannot cancel another's by guessing a cookie.
    pub fn remove(&mut self, cookie: u32, owner: &str) -> Option<Inhibitor> {
        let at = self
            .list
            .iter()
            .position(|i| i.cookie == cookie && i.owner == owner)?;
        Some(self.list.remove(at))
    }

    /// Every request of a connection that left the bus.
    pub fn drop_owner(&mut self, owner: &str) -> Vec<Inhibitor> {
        let (gone, kept) = std::mem::take(&mut self.list)
            .into_iter()
            .partition(|i| i.owner == owner);
        self.list = kept;
        gone
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// One `app<TAB>reason` line per request, for `_NIRUIDLE_APP_INHIBIT`.
    pub fn summary(&self) -> String {
        self.list
            .iter()
            .map(|i| format!("{}\t{}", i.app, i.reason))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// What the bus thread and the idle loop share.
#[derive(Debug, Default)]
pub struct Shared {
    pub inhibitors: Inhibitors,
    /// Bumped on every change of `inhibitors`.
    pub generation: u64,
    /// Whether the screensaver or the lock is up (`GetActive`).
    pub saver_active: bool,
    /// Set by `SimulateUserActivity`; the idle loop resets the idle clock.
    pub activity: bool,
    /// Whether this process owns the service name.
    pub owned: bool,
}

pub type Handle = Arc<Mutex<Shared>>;

/// Start the service on its own thread. Returns at once; problems are
/// logged, and without a session bus nothing else happens.
pub fn start() -> Handle {
    let shared: Handle = Arc::default();
    let thread_shared = Arc::clone(&shared);
    let started = std::thread::Builder::new()
        .name("screensaver-bus".into())
        .spawn(move || {
            if let Err(e) = serve(&thread_shared) {
                eprintln!("niruidle: app inhibit service stopped: {e}");
            }
            let mut s = lock(&thread_shared);
            s.owned = false;
            if !s.inhibitors.is_empty() {
                s.inhibitors = Inhibitors::default();
                s.generation += 1;
            }
        });
    if let Err(e) = started {
        eprintln!("niruidle: cannot start the app inhibit service: {e}");
    }
    shared
}

/// The lock, also after a panic on the other thread (the data stays valid:
/// every change is a single assignment or Vec operation).
pub fn lock(shared: &Handle) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

// ── Addresses and authentication ───────────────────────────────────────────

/// Decode the `%xx` escapes of a D-Bus address value.
fn unescape(value: &str) -> Option<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// A socket this client can connect to.
#[derive(Debug, PartialEq, Eq)]
pub enum Socket {
    Path(Vec<u8>),
    Abstract(Vec<u8>),
}

/// The Unix sockets of a bus address, in order (`;` separates choices).
pub fn parse_address(address: &str) -> Vec<Socket> {
    let mut found = Vec::new();
    for entry in address.split(';') {
        let Some(params) = entry.strip_prefix("unix:") else {
            continue;
        };
        for param in params.split(',') {
            let Some((key, value)) = param.split_once('=') else {
                continue;
            };
            let Some(value) = unescape(value) else {
                continue;
            };
            match key {
                "path" => found.push(Socket::Path(value)),
                "abstract" => found.push(Socket::Abstract(value)),
                _ => {}
            }
        }
    }
    found
}

fn connect(address: &str) -> std::io::Result<UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    let mut last = std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "no unix socket in DBUS_SESSION_BUS_ADDRESS",
    );
    for socket in parse_address(address) {
        let attempt = match &socket {
            Socket::Path(path) => UnixStream::connect(std::ffi::OsStr::from_bytes(path)),
            Socket::Abstract(name) => {
                use std::os::linux::net::SocketAddrExt;
                std::os::unix::net::SocketAddr::from_abstract_name(name)
                    .and_then(|addr| UnixStream::connect_addr(&addr))
            }
        };
        match attempt {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn read_line(stream: &mut UnixStream) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        stream.read_exact(&mut byte)?;
        line.push(byte[0]);
        if line.len() > 512 {
            return Err(std::io::Error::other("authentication line too long"));
        }
    }
    line.truncate(line.len() - 2);
    Ok(String::from_utf8_lossy(&line).into_owned())
}

fn authenticate(stream: &mut UnixStream) -> std::io::Result<()> {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() }.to_string();
    let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
    stream.write_all(b"\0")?;
    stream.write_all(format!("AUTH EXTERNAL {hex}\r\n").as_bytes())?;
    let reply = read_line(stream)?;
    if !reply.starts_with("OK ") {
        return Err(std::io::Error::other(format!(
            "the bus refused authentication: {reply}"
        )));
    }
    stream.write_all(b"BEGIN\r\n")
}

// ── Messages ────────────────────────────────────────────────────────────────

const METHOD_CALL: u8 = 1;
const METHOD_RETURN: u8 = 2;
const ERROR: u8 = 3;
const SIGNAL: u8 = 4;
const NO_REPLY_EXPECTED: u8 = 1;

/// A received message: the header fields the service looks at and the body.
#[derive(Debug, Default, Clone)]
pub struct Message {
    pub kind: u8,
    pub flags: u8,
    pub serial: u32,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub reply_serial: Option<u32>,
    pub sender: Option<String>,
    pub signature: String,
    big_endian: bool,
    /// The body, and its offset in the whole message (alignment is counted
    /// from the start of the message).
    body: Vec<u8>,
    body_offset: usize,
}

/// Reads values with D-Bus alignment from a byte slice.
struct Reader<'a> {
    data: &'a [u8],
    /// Offset of `data[0]` in the message, for alignment.
    base: usize,
    pos: usize,
    big_endian: bool,
}

impl<'a> Reader<'a> {
    fn align(&mut self, n: usize) -> Option<()> {
        let absolute = self.base + self.pos;
        let pad = (n - absolute % n) % n;
        if self.pos + pad > self.data.len() {
            return None;
        }
        self.pos += pad;
        Some(())
    }
    fn byte(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
    fn u32(&mut self) -> Option<u32> {
        self.align(4)?;
        let raw: [u8; 4] = self.data.get(self.pos..self.pos + 4)?.try_into().ok()?;
        self.pos += 4;
        Some(if self.big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        })
    }
    fn bytes_nul(&mut self, len: usize) -> Option<String> {
        let raw = self.data.get(self.pos..self.pos.checked_add(len)?)?;
        if self.data.get(self.pos + len) != Some(&0) {
            return None;
        }
        self.pos += len + 1;
        String::from_utf8(raw.to_vec()).ok()
    }
    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        self.bytes_nul(len)
    }
    fn signature(&mut self) -> Option<String> {
        let len = self.byte()? as usize;
        self.bytes_nul(len)
    }
}

impl Message {
    /// The body's leading values, when its signature starts with `want`
    /// (only `s` and `u` are needed). Extra trailing arguments are ignored.
    pub fn args(&self, want: &str) -> Option<Vec<Arg>> {
        if !self.signature.starts_with(want) {
            return None;
        }
        let mut r = Reader {
            data: &self.body,
            base: self.body_offset,
            pos: 0,
            big_endian: self.big_endian,
        };
        want.chars()
            .map(|c| match c {
                's' | 'o' => r.string().map(Arg::Str),
                'u' | 'b' => r.u32().map(Arg::U32),
                _ => None,
            })
            .collect()
    }

    fn strings<const N: usize>(&self) -> Option<[String; N]> {
        let args = self.args(&"s".repeat(N))?;
        let v: Vec<String> = args
            .into_iter()
            .map(|a| match a {
                Arg::Str(s) => s,
                Arg::U32(_) => String::new(),
            })
            .collect();
        v.try_into().ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    Str(String),
    U32(u32),
}

/// The length of the whole message from its first 16 bytes.
fn message_length(fixed: &[u8; 16]) -> Option<usize> {
    let big = match fixed[0] {
        b'l' => false,
        b'B' => true,
        _ => return None,
    };
    let get = |at: usize| {
        let raw: [u8; 4] = fixed[at..at + 4].try_into().unwrap();
        if big {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        }
    };
    let body = get(4) as usize;
    let fields = get(12) as usize;
    let header = (16usize.checked_add(fields)?).checked_add(7)? / 8 * 8;
    header.checked_add(body)
}

/// Parse one complete message (header and body).
pub fn parse(data: &[u8]) -> Option<Message> {
    let big_endian = match *data.first()? {
        b'l' => false,
        b'B' => true,
        _ => return None,
    };
    let mut r = Reader {
        data,
        base: 0,
        pos: 1,
        big_endian,
    };
    let mut m = Message {
        kind: r.byte()?,
        flags: r.byte()?,
        big_endian,
        ..Message::default()
    };
    let _version = r.byte()?;
    let body_len = r.u32()? as usize;
    m.serial = r.u32()?;
    let fields_len = r.u32()? as usize;
    let fields_end = r.pos.checked_add(fields_len)?;
    if fields_end > data.len() {
        return None;
    }
    while r.pos < fields_end {
        r.align(8)?;
        if r.pos >= fields_end {
            break;
        }
        let code = r.byte()?;
        let sig = r.signature()?;
        match (code, sig.as_str()) {
            (1, "o") => m.path = Some(r.string()?),
            (2, "s") => m.interface = Some(r.string()?),
            (3, "s") => m.member = Some(r.string()?),
            (4, "s") => m.error_name = Some(r.string()?),
            (5, "u") => m.reply_serial = Some(r.u32()?),
            (6, "s") | (7, "s") => {
                let value = r.string()?;
                if code == 7 {
                    m.sender = Some(value);
                }
            }
            (8, "g") => m.signature = r.signature()?,
            (_, "u") => {
                r.u32()?;
            }
            (_, "s") | (_, "o") => {
                r.string()?;
            }
            (_, "g") => {
                r.signature()?;
            }
            // A field of a type this client does not know: its length is
            // unknown, so the rest of the header cannot be read.
            _ => return None,
        }
    }
    r.pos = fields_end;
    r.align(8)?;
    let body_start = r.pos;
    let body = data.get(body_start..body_start.checked_add(body_len)?)?;
    m.body = body.to_vec();
    m.body_offset = body_start;
    Some(m)
}

/// Builds a little-endian message.
struct Builder {
    buf: Vec<u8>,
}

impl Builder {
    fn pad(&mut self, n: usize) {
        while self.buf.len() % n != 0 {
            self.buf.push(0);
        }
    }
    fn u32(&mut self, v: u32) {
        self.pad(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }
    fn signature(&mut self, s: &str) {
        self.buf.push(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }
}

/// What a message to send carries.
#[derive(Default)]
pub struct Outgoing<'a> {
    pub kind: u8,
    pub flags: u8,
    pub path: Option<&'a str>,
    pub interface: Option<&'a str>,
    pub member: Option<&'a str>,
    pub error_name: Option<&'a str>,
    pub reply_serial: Option<u32>,
    pub destination: Option<&'a str>,
    pub args: &'a [Arg],
    /// `b` instead of `u` for a `U32` argument.
    pub boolean: bool,
}

pub fn build(serial: u32, out: &Outgoing<'_>) -> Vec<u8> {
    // The body first: alignment in the body is relative to the message
    // start, and the header always ends on an 8-byte boundary, so building
    // it from offset 0 gives the same padding.
    let mut body = Builder { buf: Vec::new() };
    let mut signature = String::new();
    for arg in out.args {
        match arg {
            Arg::Str(s) => {
                signature.push('s');
                body.string(s);
            }
            Arg::U32(v) => {
                signature.push(if out.boolean { 'b' } else { 'u' });
                body.u32(*v);
            }
        }
    }
    let mut h = Builder { buf: Vec::new() };
    h.buf.extend_from_slice(&[b'l', out.kind, out.flags, 1]);
    h.u32(body.buf.len() as u32);
    h.u32(serial);
    h.u32(0); // fields length, filled in below
    let fields_start = h.buf.len();
    fn field(h: &mut Builder, code: u8, sig: &str, value: &str) {
        h.pad(8);
        h.buf.push(code);
        h.signature(sig);
        if sig == "g" {
            h.signature(value);
        } else {
            h.string(value);
        }
    }
    if let Some(v) = out.path {
        field(&mut h, 1, "o", v);
    }
    if let Some(v) = out.interface {
        field(&mut h, 2, "s", v);
    }
    if let Some(v) = out.member {
        field(&mut h, 3, "s", v);
    }
    if let Some(v) = out.error_name {
        field(&mut h, 4, "s", v);
    }
    if let Some(v) = out.reply_serial {
        h.pad(8);
        h.buf.push(5);
        h.signature("u");
        h.u32(v);
    }
    if let Some(v) = out.destination {
        field(&mut h, 6, "s", v);
    }
    if !signature.is_empty() {
        field(&mut h, 8, "g", &signature);
    }
    let fields_len = (h.buf.len() - fields_start) as u32;
    h.buf[12..16].copy_from_slice(&fields_len.to_le_bytes());
    h.pad(8);
    h.buf.extend_from_slice(&body.buf);
    h.buf
}

// ── The connection ─────────────────────────────────────────────────────────

struct Bus {
    stream: UnixStream,
    serial: u32,
}

impl Bus {
    fn send(&mut self, out: &Outgoing<'_>) -> std::io::Result<u32> {
        self.serial = self.serial.wrapping_add(1).max(1);
        let serial = self.serial;
        self.stream.write_all(&build(serial, out))?;
        Ok(serial)
    }

    fn call_bus(&mut self, member: &str, args: &[Arg]) -> std::io::Result<u32> {
        self.send(&Outgoing {
            kind: METHOD_CALL,
            path: Some(BUS_PATH),
            interface: Some(BUS),
            member: Some(member),
            destination: Some(BUS),
            args,
            ..Outgoing::default()
        })
    }

    /// Read one message. `None` for one that was too large or malformed
    /// (it is consumed either way, so the stream stays in step).
    fn receive(&mut self) -> std::io::Result<Option<Message>> {
        let mut fixed = [0u8; 16];
        self.stream.read_exact(&mut fixed)?;
        let total = message_length(&fixed)
            .ok_or_else(|| std::io::Error::other("the bus sent an invalid message"))?;
        if total < 16 {
            return Err(std::io::Error::other("the bus sent an invalid message"));
        }
        if total > MAX_MESSAGE {
            let mut rest = (&mut self.stream).take((total - 16) as u64);
            std::io::copy(&mut rest, &mut std::io::sink())?;
            return Ok(None);
        }
        let mut data = vec![0u8; total];
        data[..16].copy_from_slice(&fixed);
        self.stream.read_exact(&mut data[16..])?;
        Ok(parse(&data))
    }

    /// Call a bus method and wait for its reply. Only used while setting up,
    /// before anything else can be addressed to this connection; other
    /// messages that arrive meanwhile are handed to `other`.
    fn call_wait(
        &mut self,
        member: &str,
        args: &[Arg],
        other: &mut dyn FnMut(&mut Bus, Message) -> std::io::Result<()>,
    ) -> std::io::Result<Message> {
        let serial = self.call_bus(member, args)?;
        loop {
            let Some(m) = self.receive()? else { continue };
            if m.reply_serial == Some(serial) && (m.kind == METHOD_RETURN || m.kind == ERROR) {
                return Ok(m);
            }
            other(self, m)?;
        }
    }
}

fn first_u32(m: &Message) -> Option<u32> {
    match m.args("u")?.first()? {
        Arg::U32(v) => Some(*v),
        Arg::Str(_) => None,
    }
}

fn first_string(m: &Message) -> Option<String> {
    let [s] = m.strings::<1>()?;
    Some(s)
}

/// The program behind a unique bus name, for the log.
fn describe_owner(bus: &mut Bus, owner: &str) -> std::io::Result<String> {
    let pid = bus.call_wait(
        "GetConnectionUnixProcessID",
        &[Arg::Str(owner.to_string())],
        &mut |_, _| Ok(()),
    )?;
    Ok(match first_u32(&pid) {
        Some(pid) => {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            format!("{owner}, {} pid {pid}", comm.trim())
        }
        None => owner.to_string(),
    })
}

/// Ask for the name without taking it from another owner and without
/// queueing for it. Returns whether this connection is now the owner.
fn request_name(bus: &mut Bus, shared: &Handle) -> std::io::Result<bool> {
    const DO_NOT_QUEUE: u32 = 4;
    // Whatever arrives before the reply is handled normally, so no caller
    // is left without an answer.
    let reply = bus.call_wait(
        "RequestName",
        &[Arg::Str(SERVICE.into()), Arg::U32(DO_NOT_QUEUE)],
        &mut |b, m| handle(b, shared, m),
    )?;
    if reply.kind == ERROR {
        return Err(std::io::Error::other(format!(
            "RequestName failed: {}",
            reply.error_name.unwrap_or_default()
        )));
    }
    // 1: primary owner; 4: already the owner.
    Ok(matches!(first_u32(&reply), Some(1) | Some(4)))
}

fn serve(shared: &Handle) -> std::io::Result<()> {
    let Some(address) = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        .ok()
        .filter(|a| !a.is_empty())
    else {
        eprintln!("niruidle: no session bus; app inhibit requests are not available");
        return Ok(());
    };
    let mut stream = connect(&address).map_err(|e| {
        std::io::Error::new(e.kind(), format!("cannot connect to the session bus: {e}"))
    })?;
    authenticate(&mut stream)?;
    let mut bus = Bus { stream, serial: 0 };
    let hello = bus.call_wait("Hello", &[], &mut |_, _| Ok(()))?;
    if hello.kind == ERROR {
        return Err(std::io::Error::other("the session bus refused Hello"));
    }
    // Every connection that leaves the bus, so a crashed program's requests
    // end with it; the bus filters, so nothing else wakes this thread.
    bus.call_wait(
        "AddMatch",
        &[Arg::Str(
            "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged'"
                .into(),
        )],
        &mut |_, _| Ok(()),
    )?;
    if request_name(&mut bus, shared)? {
        lock(shared).owned = true;
        eprintln!("niruidle: owns {SERVICE}; programs can ask the screensaver to wait");
    } else {
        let owner = bus.call_wait("GetNameOwner", &[Arg::Str(SERVICE.into())], &mut |_, _| {
            Ok(())
        })?;
        let who = match first_string(&owner) {
            Some(unique) => describe_owner(&mut bus, &unique)?,
            None => "another program".into(),
        };
        eprintln!(
            "niruidle: {SERVICE} is owned by {who}; app inhibit requests go there, not to niruidle"
        );
    }
    loop {
        let Some(m) = bus.receive()? else { continue };
        handle(&mut bus, shared, m)?;
    }
}

fn handle(bus: &mut Bus, shared: &Handle, m: Message) -> std::io::Result<()> {
    match m.kind {
        SIGNAL => {
            if m.member.as_deref() == Some("NameOwnerChanged") && m.sender.as_deref() == Some(BUS) {
                if let Some([name, _old, new]) = m.strings::<3>() {
                    if new.is_empty() && name.starts_with(':') {
                        let gone = {
                            let mut s = lock(shared);
                            let gone = s.inhibitors.drop_owner(&name);
                            if !gone.is_empty() {
                                s.generation += 1;
                            }
                            gone
                        };
                        for i in gone {
                            eprintln!("niruidle: app inhibit off: {} (the program left)", i.app);
                        }
                    } else if new.is_empty() && name == SERVICE && !lock(shared).owned {
                        // The other owner left: take the name now.
                        if request_name(bus, shared)? {
                            lock(shared).owned = true;
                            eprintln!("niruidle: {SERVICE} was released; niruidle owns it now");
                        }
                    }
                }
            } else if m.member.as_deref() == Some("NameLost") {
                if let Some(name) = first_string(&m) {
                    if name == SERVICE {
                        lock(shared).owned = false;
                        eprintln!("niruidle: lost {SERVICE}");
                    }
                }
            }
            Ok(())
        }
        METHOD_CALL => method_call(bus, shared, &m),
        _ => Ok(()),
    }
}

const INTROSPECT: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.freedesktop.ScreenSaver">
    <method name="Inhibit"><arg name="application_name" type="s" direction="in"/><arg name="reason_for_inhibit" type="s" direction="in"/><arg name="cookie" type="u" direction="out"/></method>
    <method name="UnInhibit"><arg name="cookie" type="u" direction="in"/></method>
    <method name="GetActive"><arg type="b" direction="out"/></method>
    <method name="SimulateUserActivity"/>
  </interface>
  <interface name="org.freedesktop.DBus.Introspectable">
    <method name="Introspect"><arg name="data" type="s" direction="out"/></method>
  </interface>
  <interface name="org.freedesktop.DBus.Peer">
    <method name="Ping"/>
  </interface>
</node>
"#;

/// What a method call asks for, decided without the connection so it can be
/// tested on its own.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Values(Vec<Arg>, bool),
    Error(&'static str, String),
}

/// Answer one method call against the shared state.
pub fn answer(shared: &Handle, m: &Message) -> Reply {
    let sender = m.sender.clone().unwrap_or_default();
    let member = m.member.as_deref().unwrap_or("");
    let interface = m.interface.as_deref();
    let path = m.path.as_deref().unwrap_or("");
    match (interface, member) {
        (Some("org.freedesktop.DBus.Peer"), "Ping") => return Reply::Values(vec![], false),
        (Some("org.freedesktop.DBus.Introspectable"), "Introspect") => {
            let xml = if PATHS.contains(&path) {
                INTROSPECT.to_string()
            } else if path == "/" {
                "<node><node name=\"ScreenSaver\"/><node name=\"org\"/></node>".into()
            } else if path == "/org" {
                "<node><node name=\"freedesktop\"/></node>".into()
            } else if path == "/org/freedesktop" {
                "<node><node name=\"ScreenSaver\"/></node>".into()
            } else {
                "<node/>".into()
            };
            return Reply::Values(vec![Arg::Str(xml)], false);
        }
        _ => {}
    }
    if !PATHS.contains(&path) || interface.is_some_and(|i| i != INTERFACE) {
        return Reply::Error(
            "org.freedesktop.DBus.Error.UnknownMethod",
            format!("no method {member} at {path}"),
        );
    }
    match member {
        "Inhibit" => {
            let Some([app, reason]) = m.strings::<2>() else {
                return Reply::Error(
                    "org.freedesktop.DBus.Error.InvalidArgs",
                    "Inhibit takes (application, reason)".into(),
                );
            };
            let mut s = lock(shared);
            match s.inhibitors.add(&sender, &app, &reason) {
                Ok(cookie) => {
                    s.generation += 1;
                    let i = s.inhibitors.list.last().cloned();
                    drop(s);
                    if let Some(i) = i {
                        eprintln!("niruidle: app inhibit on: {} ({})", i.app, i.reason);
                    }
                    Reply::Values(vec![Arg::U32(cookie)], false)
                }
                Err(e) => Reply::Error("org.freedesktop.DBus.Error.LimitsExceeded", e.into()),
            }
        }
        "UnInhibit" => {
            let Some(cookie) = first_u32(m) else {
                return Reply::Error(
                    "org.freedesktop.DBus.Error.InvalidArgs",
                    "UnInhibit takes a cookie".into(),
                );
            };
            let removed = {
                let mut s = lock(shared);
                let removed = s.inhibitors.remove(cookie, &sender);
                if removed.is_some() {
                    s.generation += 1;
                }
                removed
            };
            if let Some(i) = removed {
                eprintln!("niruidle: app inhibit off: {}", i.app);
            }
            // An unknown cookie is not an error: the request is gone either way.
            Reply::Values(vec![], false)
        }
        "GetActive" => {
            let active = lock(shared).saver_active;
            Reply::Values(vec![Arg::U32(u32::from(active))], true)
        }
        "SimulateUserActivity" => {
            lock(shared).activity = true;
            Reply::Values(vec![], false)
        }
        _ => Reply::Error(
            "org.freedesktop.DBus.Error.UnknownMethod",
            format!("{INTERFACE} has no method {member}"),
        ),
    }
}

fn method_call(bus: &mut Bus, shared: &Handle, m: &Message) -> std::io::Result<()> {
    let reply = answer(shared, m);
    if m.flags & NO_REPLY_EXPECTED != 0 {
        return Ok(());
    }
    let Some(sender) = m.sender.as_deref() else {
        return Ok(());
    };
    match reply {
        Reply::Values(args, boolean) => {
            bus.send(&Outgoing {
                kind: METHOD_RETURN,
                reply_serial: Some(m.serial),
                destination: Some(sender),
                args: &args,
                boolean,
                ..Outgoing::default()
            })?;
        }
        Reply::Error(name, text) => {
            bus.send(&Outgoing {
                kind: ERROR,
                error_name: Some(name),
                reply_serial: Some(m.serial),
                destination: Some(sender),
                args: &[Arg::Str(text)],
                ..Outgoing::default()
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_are_unique_nonzero_and_owned() {
        let mut t = Inhibitors::default();
        let a = t.add(":1.10", "Helium", "Playing video").unwrap();
        let b = t.add(":1.11", "mpv", "video").unwrap();
        assert_ne!(a, 0);
        assert_ne!(a, b);
        assert_eq!(t.len(), 2);
        // Another connection cannot end someone else's request.
        assert!(t.remove(a, ":1.11").is_none());
        assert_eq!(t.remove(a, ":1.10").unwrap().app, "Helium");
        assert!(t.remove(a, ":1.10").is_none(), "a cookie ends once");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn a_crashed_program_takes_its_requests_with_it() {
        let mut t = Inhibitors::default();
        t.add(":1.10", "Helium", "call").unwrap();
        t.add(":1.10", "Helium", "video").unwrap();
        t.add(":1.12", "vlc", "playing").unwrap();
        let gone = t.drop_owner(":1.10");
        assert_eq!(gone.len(), 2);
        assert_eq!(t.len(), 1);
        assert_eq!(t.summary(), "vlc\tplaying");
        assert!(t.drop_owner(":1.99").is_empty());
        t.drop_owner(":1.12");
        assert!(t.is_empty());
    }

    #[test]
    fn the_table_is_bounded_and_texts_are_clean() {
        let mut t = Inhibitors::default();
        for _ in 0..MAX_PER_OWNER {
            t.add(":1.5", "app", "r").unwrap();
        }
        assert!(t.add(":1.5", "app", "r").is_err());
        assert!(t.add(":1.6", "app", "r").is_ok());
        let long = "x".repeat(500);
        t.add(":1.7", &format!("bad\napp\t{long}"), "").unwrap();
        let last = t.list.last().unwrap();
        assert!(!last.app.contains('\n') && !last.app.contains('\t'));
        assert_eq!(last.app.chars().count(), MAX_TEXT);
        // A blank name falls back to the bus name.
        t.add(":1.8", "  ", "").unwrap();
        assert_eq!(t.list.last().unwrap().app, ":1.8");
    }

    #[test]
    fn the_cookie_counter_skips_zero_and_taken_cookies() {
        let mut t = Inhibitors {
            next: u32::MAX - 1,
            list: Vec::new(),
        };
        assert_eq!(t.add(":1.1", "a", "").unwrap(), u32::MAX);
        assert_eq!(t.add(":1.1", "b", "").unwrap(), 1);
        t.next = 0;
        assert_eq!(t.add(":1.1", "c", "").unwrap(), 2, "1 is still in use");
    }

    #[test]
    fn addresses_cover_paths_abstract_sockets_and_escapes() {
        assert_eq!(
            parse_address("unix:path=/run/user/1000/bus"),
            vec![Socket::Path(b"/run/user/1000/bus".to_vec())]
        );
        assert_eq!(
            parse_address("unix:abstract=/tmp/dbus-AbC,guid=0123"),
            vec![Socket::Abstract(b"/tmp/dbus-AbC".to_vec())]
        );
        assert_eq!(
            parse_address("tcp:host=x,port=1;unix:path=/tmp/a%20b"),
            vec![Socket::Path(b"/tmp/a b".to_vec())]
        );
        assert!(parse_address("").is_empty());
        assert!(parse_address("unix:path=%zz").is_empty());
    }

    fn call(sender: &str, path: &str, member: &str, args: &[Arg]) -> Message {
        let bytes = build(
            7,
            &Outgoing {
                kind: METHOD_CALL,
                path: Some(path),
                interface: Some(INTERFACE),
                member: Some(member),
                destination: Some(SERVICE),
                args,
                ..Outgoing::default()
            },
        );
        let mut m = parse(&bytes).expect("a built message parses");
        // The bus adds the sender.
        m.sender = Some(sender.to_string());
        m
    }

    #[test]
    fn built_messages_parse_back() {
        let bytes = build(
            42,
            &Outgoing {
                kind: METHOD_CALL,
                path: Some("/ScreenSaver"),
                interface: Some(INTERFACE),
                member: Some("Inhibit"),
                destination: Some(SERVICE),
                args: &[Arg::Str("mpv".into()), Arg::Str("video".into())],
                ..Outgoing::default()
            },
        );
        let mut fixed = [0u8; 16];
        fixed.copy_from_slice(&bytes[..16]);
        assert_eq!(message_length(&fixed), Some(bytes.len()));
        let m = parse(&bytes).unwrap();
        assert_eq!(m.serial, 42);
        assert_eq!(m.kind, METHOD_CALL);
        assert_eq!(m.path.as_deref(), Some("/ScreenSaver"));
        assert_eq!(m.member.as_deref(), Some("Inhibit"));
        assert_eq!(m.signature, "ss");
        assert_eq!(
            m.args("ss").unwrap(),
            vec![Arg::Str("mpv".into()), Arg::Str("video".into())]
        );
        assert!(m.args("u").is_none());
        // Truncated input is refused, never read past its end.
        for cut in 0..bytes.len() {
            let _ = parse(&bytes[..cut]);
        }
    }

    #[test]
    fn big_endian_messages_are_read() {
        // A signal with body (s) "hi" from a big-endian sender, built by hand.
        let mut b: Vec<u8> = vec![b'B', SIGNAL, 0, 1];
        b.extend_from_slice(&7u32.to_be_bytes()); // body length
        b.extend_from_slice(&9u32.to_be_bytes()); // serial
        let fields: Vec<u8> = {
            let mut f = vec![3, 1, b's', 0];
            f.extend_from_slice(&4u32.to_be_bytes());
            f.extend_from_slice(b"Ping\0");
            f.extend_from_slice(&[0, 0, 0]); // pad to 8
            f.extend_from_slice(&[8, 1, b'g', 0, 1, b's', 0]);
            f
        };
        b.extend_from_slice(&(fields.len() as u32).to_be_bytes());
        b.extend_from_slice(&fields);
        while b.len() % 8 != 0 {
            b.push(0);
        }
        b.extend_from_slice(&2u32.to_be_bytes());
        b.extend_from_slice(b"hi\0");
        let m = parse(&b).unwrap();
        assert_eq!(m.serial, 9);
        assert_eq!(m.member.as_deref(), Some("Ping"));
        assert_eq!(first_string(&m).as_deref(), Some("hi"));
    }

    #[test]
    fn inhibit_and_uninhibit_through_the_service() {
        let shared: Handle = Arc::default();
        let reply = answer(
            &shared,
            &call(
                ":1.20",
                "/org/freedesktop/ScreenSaver",
                "Inhibit",
                &[Arg::Str("Helium".into()), Arg::Str("Playing video".into())],
            ),
        );
        let Reply::Values(values, false) = reply else {
            panic!("Inhibit must return a cookie: {reply:?}")
        };
        let [Arg::U32(cookie)] = values.as_slice() else {
            panic!("one cookie")
        };
        assert_eq!(lock(&shared).inhibitors.len(), 1);
        assert_eq!(lock(&shared).generation, 1);
        // The KDE path works too, and a stranger's UnInhibit changes nothing.
        answer(
            &shared,
            &call(":1.21", "/ScreenSaver", "UnInhibit", &[Arg::U32(*cookie)]),
        );
        assert_eq!(lock(&shared).inhibitors.len(), 1);
        answer(
            &shared,
            &call(":1.20", "/ScreenSaver", "UnInhibit", &[Arg::U32(*cookie)]),
        );
        assert!(lock(&shared).inhibitors.is_empty());
        assert_eq!(lock(&shared).generation, 2);
    }

    #[test]
    fn get_active_simulate_activity_and_unknown_methods() {
        let shared: Handle = Arc::default();
        lock(&shared).saver_active = true;
        assert_eq!(
            answer(
                &shared,
                &call(":1.3", "/org/freedesktop/ScreenSaver", "GetActive", &[])
            ),
            Reply::Values(vec![Arg::U32(1)], true)
        );
        answer(
            &shared,
            &call(":1.3", "/ScreenSaver", "SimulateUserActivity", &[]),
        );
        assert!(lock(&shared).activity);
        assert!(matches!(
            answer(&shared, &call(":1.3", "/ScreenSaver", "Lock", &[])),
            Reply::Error("org.freedesktop.DBus.Error.UnknownMethod", _)
        ));
        assert!(matches!(
            answer(&shared, &call(":1.3", "/elsewhere", "Inhibit", &[])),
            Reply::Error(..)
        ));
        assert!(matches!(
            answer(&shared, &call(":1.3", "/ScreenSaver", "Inhibit", &[])),
            Reply::Error("org.freedesktop.DBus.Error.InvalidArgs", _)
        ));
    }
}
