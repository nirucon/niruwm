//! nirugestured: touchpad swipe gestures for NIRUX.
//!
//! Three fingers left or right switch to the previous or next workspace (of
//! ten, wrapping); four fingers up open the overview (`nirulauncher
//! --overview`). libinput recognises the gestures (`libinput debug-events`),
//! exactly as before, so thresholds and palm handling are libinput's.
//!
//! The Python version ran `libinput debug-events` on every input device for
//! the whole session, also on desktops without a touchpad, and read every
//! keyboard and mouse event line. This one only watches touchpads: it finds
//! them in udev's data (or by their capabilities), runs libinput on exactly
//! those devices, and otherwise sleeps on inotify for `/dev/input` until a
//! touchpad appears. Workspaces are switched with the EWMH client message
//! xdotool sent, without starting xdotool.

#[path = "../stdout.rs"]
#[macro_use]
mod stdout;

use std::io::Read as _;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, Window,
};
use x11rb::rust_connection::RustConnection;

const DESKTOPS: i64 = 10;
/// udev applies permissions just after a node appears: look again then.
const SETTLE: Duration = Duration::from_millis(800);

fn input_dir() -> PathBuf {
    std::env::var_os("NIRUGESTURED_INPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/dev/input"))
}

fn sys_input() -> PathBuf {
    std::env::var_os("NIRUGESTURED_SYS_INPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/class/input"))
}

/// A bit of a sysfs capability bitmap (`"3 0 1f"`: hex words, highest first).
fn has_bit(map: &str, bit: usize) -> bool {
    let words: Vec<u64> = map
        .split_whitespace()
        .rev()
        .filter_map(|word| u64::from_str_radix(word, 16).ok())
        .collect();
    let bits = u64::BITS as usize;
    words
        .get(bit / bits)
        .is_some_and(|word| word & (1u64 << (bit % bits)) != 0)
}

const ABS_MT_POSITION_X: usize = 0x35;
const BTN_TOOL_FINGER: usize = 0x145;
const INPUT_PROP_DIRECT: usize = 0x01;

/// Whether `eventN` is a touchpad: udev's `ID_INPUT_TOUCHPAD`, as libinput
/// reads it, or a multitouch pointer that is not a touchscreen.
fn is_touchpad(name: &str) -> bool {
    let device = sys_input().join(name).join("device");
    let read = |file: &str| std::fs::read_to_string(device.join(file)).unwrap_or_default();
    let dev = std::fs::read_to_string(sys_input().join(name).join("dev")).unwrap_or_default();
    if let Some((major, minor)) = dev.trim().split_once(':') {
        let udev = PathBuf::from("/run/udev/data").join(format!("c{major}:{minor}"));
        if let Ok(data) = std::fs::read_to_string(udev) {
            if data.lines().any(|line| line == "E:ID_INPUT_TOUCHPAD=1") {
                return true;
            }
            if data.lines().any(|line| line.starts_with("E:ID_INPUT")) {
                // udev classified it, and not as a touchpad.
                return false;
            }
        }
    }
    has_bit(&read("capabilities/abs"), ABS_MT_POSITION_X)
        && has_bit(&read("capabilities/key"), BTN_TOOL_FINGER)
        && !has_bit(&read("properties"), INPUT_PROP_DIRECT)
}

fn readable(path: &Path) -> bool {
    let Ok(text) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: a valid C string.
    unsafe { libc::access(text.as_ptr(), libc::R_OK) == 0 }
}

/// The touchpads, and those of them this user may read.
fn touchpads() -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut found: Vec<PathBuf> = std::fs::read_dir(input_dir())
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_str()?.to_string();
                    (name.starts_with("event") && is_touchpad(&name)).then(|| entry.path())
                })
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    let usable = found
        .iter()
        .filter(|path| readable(path))
        .cloned()
        .collect();
    (found, usable)
}

/// The finger count at the end of a `GESTURE_SWIPE_BEGIN` line (3 or 4).
fn fingers(line: &str) -> u32 {
    let line = line.trim_end();
    let mut chars = line.chars().rev();
    match (chars.next(), chars.next()) {
        (Some(digit @ ('3' | '4')), before)
            if before.map_or(true, |c| !(c.is_alphanumeric() || c == '_')) =>
        {
            digit.to_digit(10).unwrap_or(0)
        }
        _ => 0,
    }
}

fn decimal(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    digits.split_once('.').is_some_and(|(whole, part)| {
        !whole.is_empty()
            && !part.is_empty()
            && whole.bytes().all(|b| b.is_ascii_digit())
            && part.bytes().all(|b| b.is_ascii_digit())
    })
}

/// The first `dx/ dy` pair of a `GESTURE_SWIPE_UPDATE` line (the
/// accelerated deltas), like `(-?\d+\.\d+)\s*/\s*(-?\d+\.\d+)`.
fn deltas(line: &str) -> Option<(f64, f64)> {
    let token = |c: char| c.is_ascii_digit() || c == '.' || c == '-';
    for (at, _) in line.match_indices('/') {
        let left = line[..at].trim_end();
        let start = left
            .rfind(|c: char| !token(c))
            .map(|i| i + left[i..].chars().next().map_or(1, char::len_utf8))
            .unwrap_or(0);
        let right = line[at + 1..].trim_start();
        let end = right.find(|c: char| !token(c)).unwrap_or(right.len());
        // The regex finds the longest valid suffix on the left.
        let mut x = &left[start..];
        while !x.is_empty() && !decimal(x) {
            x = &x[1..];
        }
        let mut y = &right[..end];
        while !y.is_empty() && !decimal(y) {
            y = &y[..y.len() - 1];
        }
        if decimal(x) && decimal(y) {
            return Some((x.parse().ok()?, y.parse().ok()?));
        }
    }
    None
}

#[derive(Debug, PartialEq)]
enum Action {
    Overview,
    Desktop(i64),
}

/// What a finished swipe asks for.
fn decide(fingers: u32, dx: f64, dy: f64) -> Option<Action> {
    if fingers == 4 && dy < -0.45 {
        Some(Action::Overview)
    } else if fingers == 3 && dx.abs() > 0.45_f64.max(dy.abs()) {
        Some(Action::Desktop(if dx > 0.0 { 1 } else { -1 }))
    } else {
        None
    }
}

/// The swipe being made, fed line by line.
#[derive(Default)]
struct Swipe {
    fingers: u32,
    dx: f64,
    dy: f64,
}

impl Swipe {
    fn feed(&mut self, line: &str) -> Option<Option<Action>> {
        if line.contains("GESTURE_SWIPE_BEGIN") {
            self.fingers = fingers(line);
            self.dx = 0.0;
            self.dy = 0.0;
        } else if line.contains("GESTURE_SWIPE_UPDATE") {
            if let Some((dx, dy)) = deltas(line) {
                self.dx = dx;
                self.dy = dy;
            }
        } else if line.contains("GESTURE_SWIPE_END") {
            let action = decide(self.fingers, self.dx, self.dy);
            self.fingers = 0;
            return Some(action);
        }
        None
    }
}

struct Desktop {
    x: Option<(RustConnection, Window)>,
}

impl Desktop {
    fn connection(&mut self) -> Option<&(RustConnection, Window)> {
        if self.x.is_none() {
            let (c, screen) = x11rb::connect(None).ok()?;
            let root = c.setup().roots.get(screen)?.root;
            self.x = Some((c, root));
        }
        self.x.as_ref()
    }

    /// `xdotool get_desktop` and `xdotool set_desktop N`, wrapping at ten.
    fn step(&mut self, delta: i64) {
        let done = (|| -> Option<()> {
            let (c, root) = self.connection()?;
            let atom = c
                .intern_atom(false, b"_NET_CURRENT_DESKTOP")
                .ok()?
                .reply()
                .ok()?
                .atom;
            let current = c
                .get_property(false, *root, atom, AtomEnum::CARDINAL, 0, 1)
                .ok()?
                .reply()
                .ok()?
                .value32()?
                .next()?;
            let next = (i64::from(current) + delta).rem_euclid(DESKTOPS) as u32;
            let event = ClientMessageEvent::new(32, *root, atom, [next, 0, 0, 0, 0]);
            c.send_event(
                false,
                *root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )
            .ok()?;
            c.flush().ok()
        })();
        if done.is_none() {
            // The display went away or never answered: connect next time.
            self.x = None;
        }
    }
}

/// inotify on the input directory: nodes appearing, going, or changing
/// permissions.
struct Watch {
    fd: i32,
}

impl Watch {
    fn new(dir: &Path) -> Option<Self> {
        // SAFETY: plain inotify calls on a valid C string.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        let path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
        let wd = unsafe {
            libc::inotify_add_watch(
                fd,
                path.as_ptr(),
                libc::IN_CREATE | libc::IN_DELETE | libc::IN_ATTRIB | libc::IN_MOVED_TO,
            )
        };
        if wd < 0 {
            unsafe { libc::close(fd) };
            return None;
        }
        Some(Self { fd })
    }

    fn drain(&self) {
        let mut buffer = [0u8; 4096];
        // SAFETY: reading into a local buffer from a nonblocking fd.
        while unsafe { libc::read(self.fd, buffer.as_mut_ptr() as *mut _, buffer.len()) } > 0 {}
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

fn libinput_command() -> String {
    std::env::var("NIRUGESTURED_LIBINPUT").unwrap_or_else(|_| "libinput".into())
}

fn spawn_libinput(devices: &[PathBuf]) -> Option<(Child, ChildStdout)> {
    let mut command = Command::new(libinput_command());
    // `--device A B ...`: libinput's path backend on exactly these nodes.
    command.arg("debug-events").arg("--device").args(devices);
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let fd = stdout.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    Some((child, stdout))
}

fn stop(running: &mut Option<(Child, ChildStdout)>) {
    if let Some((mut child, _)) = running.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn run() -> i32 {
    let dir = input_dir();
    let watch = Watch::new(&dir);
    let mut desktop = Desktop { x: None };
    let mut launched: Vec<Child> = Vec::new();
    let mut running: Option<(Child, ChildStdout)> = None;
    let mut devices: Vec<PathBuf> = Vec::new();
    let mut rescan_at = Some(Instant::now());
    let mut reported = String::new();
    let mut swipe = Swipe::default();
    let mut line = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if rescan_at.is_some_and(|at| at <= Instant::now()) {
            rescan_at = None;
            let (found, usable) = touchpads();
            let state = if found.is_empty() {
                "no touchpad: waiting for one".to_string()
            } else if usable.is_empty() {
                "a touchpad was found but cannot be read (input group or uaccess)".to_string()
            } else {
                format!("watching {} touchpad(s)", usable.len())
            };
            if state != reported {
                eprintln!("nirugestured: {state}");
                reported = state;
            }
            if usable != devices || (running.is_none() && !usable.is_empty()) {
                stop(&mut running);
                devices = usable;
                if !devices.is_empty() {
                    running = spawn_libinput(&devices);
                    if running.is_none() {
                        eprintln!("nirugestured: cannot start libinput debug-events");
                    }
                }
                swipe = Swipe::default();
                line.clear();
            }
        }
        let mut fds = Vec::with_capacity(2);
        if let Some(watch) = &watch {
            fds.push(libc::pollfd {
                fd: watch.fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let watch_slots = fds.len();
        if let Some((_, stdout)) = &running {
            fds.push(libc::pollfd {
                fd: stdout.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let timeout = rescan_at
            .map(|at| {
                at.saturating_duration_since(Instant::now())
                    .as_millis()
                    .clamp(1, i32::MAX as u128) as i32
            })
            .unwrap_or(-1);
        if fds.is_empty() && timeout < 0 {
            // Neither inotify nor a touchpad: nothing can ever happen.
            eprintln!("nirugestured: cannot watch {}", dir.display());
            return 1;
        }
        // SAFETY: valid pollfd entries.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return 1;
        }
        if watch_slots == 1 && fds[0].revents != 0 {
            if let Some(watch) = &watch {
                watch.drain();
            }
            rescan_at.get_or_insert_with(|| Instant::now() + SETTLE);
        }
        launched.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        let Some(slot) = fds.get(watch_slots) else {
            continue;
        };
        if slot.revents == 0 {
            continue;
        }
        let mut ended = false;
        if let Some((_, stdout)) = &mut running {
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => {
                        ended = true;
                        break;
                    }
                    Ok(n) => {
                        for &byte in &chunk[..n] {
                            if byte != b'\n' {
                                if line.len() < 4096 {
                                    line.push(byte);
                                }
                                continue;
                            }
                            let text = String::from_utf8_lossy(&line).into_owned();
                            line.clear();
                            match swipe.feed(&text) {
                                Some(Some(Action::Overview)) => {
                                    if let Ok(child) = Command::new("nirulauncher")
                                        .arg("--overview")
                                        .stdin(Stdio::null())
                                        .stdout(Stdio::null())
                                        .stderr(Stdio::null())
                                        .spawn()
                                    {
                                        launched.push(child);
                                    }
                                }
                                Some(Some(Action::Desktop(delta))) => desktop.step(delta),
                                _ => {}
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        ended = true;
                        break;
                    }
                }
            }
        }
        if ended {
            if let Some((mut child, _)) = running.take() {
                let _ = child.kill();
                let status = child.wait();
                eprintln!(
                    "nirugestured: libinput stopped ({}); waiting for a device change",
                    status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|e| e.to_string())
                );
            }
            // Started again by the next change under /dev/input, not in a
            // loop: a libinput that cannot open the device would only fail
            // again.
            devices.clear();
            if watch.is_none() {
                return 1;
            }
        }
    }
}

fn main() {
    stdout::exit_on_broken_pipe();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "version") => {
            println!("NIRUGESTURED {}", env!("CARGO_PKG_VERSION"));
        }
        None | Some("daemon") => std::process::exit(run()),
        Some(_) => {
            eprintln!("Usage: nirugestured [daemon|--version]");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libinput_lines_become_actions() {
        let mut swipe = Swipe::default();
        let lines = [
            " event7   GESTURE_SWIPE_BEGIN     +2.522s\t3",
            " event7   GESTURE_SWIPE_UPDATE    +2.537s\t3 -2.25/ 0.98 (-6.47/ 2.82 unaccelerated)",
            " event7   GESTURE_SWIPE_UPDATE    +2.550s\t3 -1.10/ 0.20 (-3.00/ 0.50 unaccelerated)",
        ];
        for line in lines {
            assert_eq!(swipe.feed(line), None);
        }
        assert_eq!(
            swipe.feed(" event7   GESTURE_SWIPE_END       +2.676s\t3"),
            Some(Some(Action::Desktop(-1)))
        );
        swipe.feed(" event7   GESTURE_SWIPE_BEGIN     +3.000s\t4");
        swipe.feed(
            " event7   GESTURE_SWIPE_UPDATE    +3.010s\t4  0.10/-0.90 ( 0.20/-2.00 unaccelerated)",
        );
        assert_eq!(
            swipe.feed(" event7   GESTURE_SWIPE_END       +3.100s\t4"),
            Some(Some(Action::Overview))
        );
        // A slow end, too few fingers, or a vertical three-finger swipe.
        swipe.feed(" event7   GESTURE_SWIPE_BEGIN     +4.000s\t3");
        swipe.feed(
            " event7   GESTURE_SWIPE_UPDATE    +4.010s\t3  0.30/ 0.00 ( 0.20/ 0.00 unaccelerated)",
        );
        assert_eq!(
            swipe.feed(" event7   GESTURE_SWIPE_END       +4.100s\t3"),
            Some(None)
        );
        swipe.feed(" event7   GESTURE_SWIPE_BEGIN     +5.000s\t2");
        swipe.feed(
            " event7   GESTURE_SWIPE_UPDATE    +5.010s\t2  3.00/ 0.00 ( 2.00/ 0.00 unaccelerated)",
        );
        assert_eq!(
            swipe.feed(" event7   GESTURE_SWIPE_END +5.1s\t2"),
            Some(None)
        );
        assert_eq!(decide(3, 0.5, -0.8), None);
        assert_eq!(decide(3, 0.9, -0.8), Some(Action::Desktop(1)));
        assert_eq!(
            swipe.feed(" event2   KEYBOARD_KEY  +6.0s *** (-1) pressed"),
            None
        );
    }

    #[test]
    fn parsing_matches_the_old_patterns() {
        assert_eq!(fingers("GESTURE_SWIPE_BEGIN +1s 3"), 3);
        assert_eq!(fingers("GESTURE_SWIPE_BEGIN +1s\t4  "), 4);
        assert_eq!(fingers("GESTURE_SWIPE_BEGIN +1s 13"), 0);
        assert_eq!(fingers("GESTURE_SWIPE_BEGIN +1s 5"), 0);
        assert_eq!(deltas("x 1.50/-2.25 y"), Some((1.5, -2.25)));
        assert_eq!(deltas("x -0.00 /  3.10 (1.0/2.0)"), Some((-0.0, 3.1)));
        assert_eq!(deltas("+2.5s 3 1/2"), None);
        assert_eq!(deltas("no deltas"), None);
    }

    #[test]
    fn capability_bitmaps_read_like_the_kernel_writes_them() {
        // `abs` of a touchpad: ABS_X, ABS_Y, ABS_PRESSURE, MT slots/positions.
        assert!(has_bit("2e0800000000003", ABS_MT_POSITION_X));
        assert!(!has_bit("3", ABS_MT_POSITION_X));
        // `key` words, highest first: BTN_TOOL_FINGER is bit 0x145 (word 5).
        assert!(has_bit("e520 10000 0 0 0 0", BTN_TOOL_FINGER));
        assert!(!has_bit("e500 10000 0 0 0 0", BTN_TOOL_FINGER));
        assert!(has_bit("2", INPUT_PROP_DIRECT));
        assert!(!has_bit("5", INPUT_PROP_DIRECT));
    }
}
