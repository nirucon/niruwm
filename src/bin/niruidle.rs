//! niruidle: the NIRU idle ladder.
//!
//! Watches how long the screen has been idle and runs, one after another:
//!
//!   1. niruscreensaver (or a blank screen when it is not installed)
//!   2. the screen locker (nirulock, NIRULOCK, or slock)
//!   3. display off
//!
//! Idle time is the time since the last real input: keys, buttons, pointer
//! motion and touch from any device, read as raw XInput 2 events on the root
//! window. A program cannot reset it: `XResetScreenSaver` and
//! `ForceScreenSaver(Reset)`, which browsers and games call to keep a screen
//! awake, only reset the X server's own clock (MIT-SCREEN-SAVER). Programs
//! that want the screen to stay on ask over D-Bus (`org.freedesktop.ScreenSaver`
//! Inhibit), which is shown in `niructl idle status`. Without XInput 2 the
//! MIT-SCREEN-SAVER idle time is used as before.
//!
//! The times are read from `~/.config/niruui/screensaver.toml`, the same file
//! NIRUSCREENSAVER uses, so the settings describe the whole sequence in one
//! place. This exists so the ladder works on a clean install with no
//! `xidlehook`, `xprintidle` or shell glue.

#[path = "../stdout.rs"]
#[macro_use]
mod stdout;
use std::{
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::{Connection, RequestConnection as _},
    protocol::{
        dpms::ConnectionExt as _, screensaver::ConnectionExt as _, xinput,
        xinput::ConnectionExt as _, xproto::*, Event,
    },
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

#[allow(dead_code)]
#[path = "../bindings.rs"]
mod bindings;
#[allow(dead_code)] // shared with niructl/niruwm; only the [idle] reader is used here
#[path = "../config.rs"]
mod config;
#[path = "../idle_bus.rs"]
mod idle_bus;
#[allow(dead_code)]
#[path = "../layout.rs"]
mod layout;
#[path = "../proximity.rs"]
mod proximity;

/// How often the idle time is checked. Five seconds is precise enough for a
/// fifteen minute timer and costs nothing.
const POLL: Duration = Duration::from_secs(5);

/// At most one log line per this long for `SimulateUserActivity` calls; some
/// programs send one every thirty seconds.
const ACTIVITY_LOG_EVERY: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy)]
struct Settings {
    /// Minutes of idle before the screensaver.
    screen: u64,
    /// Minutes after the screensaver before locking.
    lock_after: u64,
    /// Minutes after locking before the display is turned off.
    dpms_after: u64,
    /// Deprecated compatibility input; never used to terminate a locker.
    lock_timeout: u64,
    /// `[idle] honor_app_inhibit`: programs may pause the ladder over D-Bus.
    honor_apps: bool,
}

/// How often the proximity lock asks the phone. Each check pages it once.
const PROXIMITY_POLL: Duration = Duration::from_secs(10);

/// `[idle] proximity_device` (empty: off) and `proximity_lock_after` seconds.
fn load_proximity() -> (String, u64) {
    let idle = shared_idle_settings().unwrap_or_default().safe();
    (idle.proximity_device, u64::from(idle.proximity_lock_after))
}

/// Check the configured phone in the background, so a five second page never
/// holds up the idle ladder. The results arrive on the returned channel.
fn start_proximity(
    device: std::sync::Arc<std::sync::Mutex<String>>,
) -> std::sync::mpsc::Receiver<proximity::Presence> {
    let (tx, rx) = std::sync::mpsc::channel();
    // Tests (tests/idle-proximity.sh) stand in for the phone with a file that
    // says near, away or unknown; it is read every second instead of paging.
    let fake = std::env::var_os("NIRUIDLE_TEST_PROXIMITY").map(std::path::PathBuf::from);
    thread::spawn(move || loop {
        let address = device.lock().map(|d| d.clone()).unwrap_or_default();
        if address.is_empty() {
            thread::sleep(POLL);
            continue;
        }
        let presence = match &fake {
            None => proximity::probe(&address),
            Some(path) => match std::fs::read_to_string(path).unwrap_or_default().trim() {
                "near" => proximity::Presence::Near,
                "away" => proximity::Presence::Away,
                _ => proximity::Presence::Unknown("test".into()),
            },
        };
        if tx.send(presence).is_err() {
            return;
        }
        thread::sleep(if fake.is_some() {
            Duration::from_secs(1)
        } else {
            PROXIMITY_POLL
        });
    });
    rx
}

fn config_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    base.join("niruui/screensaver.toml")
}

fn read_u64(value: &toml::Value, key: &str) -> Option<u64> {
    value
        .get(key)?
        .as_integer()
        .filter(|n| *n >= 0)
        .map(|n| n as u64)
}

fn load_settings() -> Settings {
    let mut s = Settings {
        screen: 15,
        lock_after: 15,
        dpms_after: 15,
        lock_timeout: 0,
        honor_apps: true,
    };
    // The shared NIRU config owns these values. The old
    // ~/.config/niruui/screensaver.toml is read only for values the shared
    // file does not set, so an older installation keeps its timings without
    // silently overriding the documented `[idle]` section.
    let shared = shared_idle_settings();
    if let Some(idle) = &shared {
        s.screen = u64::from(idle.screensaver_timeout);
        s.lock_after = u64::from(idle.lock_after_screensaver);
        s.dpms_after = u64::from(idle.dpms_off_after_lock);
        s.honor_apps = idle.honor_app_inhibit;
    }
    let Ok(text) = std::fs::read_to_string(config_path()) else {
        return s;
    };
    let Ok(v) = text.parse::<toml::Value>() else {
        eprintln!(
            "niruidle: {} is not valid TOML; using defaults",
            config_path().display()
        );
        return s;
    };
    // The shared file wins: only fill a value the shared file did not set.
    if shared.is_none() {
        if let Some(n) = read_u64(&v, "idle_minutes") {
            s.screen = n;
        }
        if let Some(n) = read_u64(&v, "lock_after_minutes") {
            s.lock_after = n;
        }
        if let Some(n) = read_u64(&v, "dpms_off_minutes") {
            s.dpms_after = n;
        }
    }
    if let Some(n) = read_u64(&v, "lock_timeout_minutes") {
        s.lock_timeout = n;
    }
    s
}

/// The shared config's modification time, used to reload timers in place.
fn shared_config_stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(crate::config::directory().join("config.toml"))
        .ok()?
        .modified()
        .ok()
}

/// The `[idle]` section of the shared config, already clamped to safe values.
/// A broken shared file must not stop the idle daemon: the defaults already in
/// `Settings` stay in place.
fn shared_idle_settings() -> Option<crate::config::IdleConfig> {
    let found = crate::config::IdleConfig::defaults_from_shared_file();
    if found.is_none() {
        eprintln!("niruidle: no [idle] section in the shared config; using defaults");
    }
    found
}

/// First program in PATH that exists.
fn first_available(candidates: &[String]) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let executable = |path: &std::path::Path| {
        path.metadata()
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };
    candidates
        .iter()
        .find(|c| {
            if c.contains('/') {
                executable(std::path::Path::new(c))
            } else {
                std::env::var("PATH")
                    .map(|p| {
                        p.split(':')
                            .any(|d| executable(&std::path::Path::new(d).join(c)))
                    })
                    .unwrap_or(false)
            }
        })
        .cloned()
}

fn lockers() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    // Honour the session's selected prefix in PATH, then the ordinary user
    // installation, before trying a compatible X11 fallback locker.
    vec![
        "nirulock".into(),
        format!("{home}/.local/bin/nirulock"),
        "slock".into(),
    ]
}

/// Retain ownership so completed children are reaped and PID reuse is harmless.
fn spawn(program: &str) -> Option<Child> {
    Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| eprintln!("niruidle: cannot start {program}: {e}"))
        .ok()
}

/// The idle lock: NIRULOCK must not start its pause music for it. The reason
/// is passed in the environment, which an older NIRULOCK and slock ignore,
/// instead of `--reason`, which an older NIRULOCK would refuse.
fn spawn_locker(program: &str) -> Option<Child> {
    Command::new(program)
        .env("NIRULOCK_REASON", "idle")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| eprintln!("niruidle: cannot start {program}: {e}"))
        .ok()
}

fn stop_saver(child: &mut Option<Child>) {
    if let Some(mut child) = child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn reap(child: &mut Option<Child>) -> Option<std::process::ExitStatus> {
    let status = child.as_mut()?.try_wait().ok()??;
    *child = None;
    Some(status)
}

/// The lock window named by `_NIRU_LOCK_READY`, if it is still alive.
///
/// NIRULOCK may be started by NIRUWM's shortcut or by another session helper,
/// not only by this daemon. Treating only our own child as "locked" made the
/// idle ladder try to cover an already locked screen every five seconds, which
/// spawned short-lived helpers and, on some sessions, left visible zombies
/// between reaps. The root property is the suite-wide readiness contract.
fn lock_ready_window(c: &RustConnection, root: Window, atom: Atom) -> Option<Window> {
    let reply = c
        .get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    let window = reply.value32()?.next()?;
    if window == 0 {
        return None;
    }
    c.get_window_attributes(window).ok()?.reply().ok()?;
    Some(window)
}

fn xset_dpms(force: &str) {
    let _ = Command::new("xset")
        .args(["dpms", "force", force])
        .stderr(Stdio::null())
        .status();
}

fn inhibit_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/state")
        });
    base.join("niruui/inhibit")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Explicit expiry in the shared state file; `0` means until turned off.
fn inhibit_active_from(path: &std::path::Path, now: u64) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(until) = text.trim().parse::<u64>() else {
        return false;
    };
    until == 0 || until > now
}

/// Session-scoped inhibition: `_NIRU_IDLE_INHIBIT` on the root window with
/// any text (the holder, e.g. `studio`). Unlike the state file it vanishes
/// with the X server, so it can never outlive the session that set it.
fn session_inhibit_active(conn: &RustConnection, root: Window, atom: Atom) -> bool {
    atom != 0
        && conn
            .get_property(false, root, atom, AtomEnum::ANY, 0, 16)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|reply| session_inhibit_value(&reply.value))
}

fn session_inhibit_value(value: &[u8]) -> bool {
    !String::from_utf8_lossy(value).trim().is_empty()
}

/// Write (or with no text, remove) the root property that lists the programs
/// holding a request, one `app<TAB>reason` line each.
fn publish_app_inhibitors(conn: &RustConnection, root: Window, atom: Atom, utf8: Atom, text: &str) {
    if atom == 0 {
        return;
    }
    if text.is_empty() {
        let _ = conn.delete_property(root, atom);
    } else if utf8 != 0 {
        let _ = conn.change_property8(PropMode::REPLACE, root, atom, utf8, text.as_bytes());
    }
    let _ = conn.flush();
}

/// Keep the X server's own blanking and DPMS timers at zero.
///
/// NIRUIDLE owns the ladder (screensaver, lock, display off). The server's
/// defaults (blank and DPMS off after 600 s) turned the display off after ten
/// minutes, before the screensaver and long before the lock, so a user coming
/// back found a dark but unlocked screen. `niructl idle off` and other tools
/// may restore `xset s default`, so this is checked every tick; it costs two
/// round trips and changes nothing when the timers are already zero. DPMS
/// stays enabled, so `dpms force off` after the lock still works.
fn keep_x_timers_off(conn: &RustConnection) {
    if let Some(saver) = conn.get_screen_saver().ok().and_then(|c| c.reply().ok()) {
        if saver.timeout != 0 {
            let _ = conn.set_screen_saver(
                0,
                saver.interval as i16,
                Blanking::from(u8::from(saver.prefer_blanking)),
                Exposures::from(u8::from(saver.allow_exposures)),
            );
        }
    }
    if let Some(t) = conn.dpms_get_timeouts().ok().and_then(|c| c.reply().ok()) {
        if t.standby_timeout != 0 || t.suspend_timeout != 0 || t.off_timeout != 0 {
            let _ = conn.dpms_set_timeouts(0, 0, 0);
        }
    }
    let _ = conn.flush();
}

fn xset_saver(force: &str, dpms: &str) {
    let _ = Command::new("xset")
        .args(["s", force, dpms])
        .stderr(Stdio::null())
        .status();
}

/// How long the input thread sleeps after an event before it drains the
/// queue: a moving mouse sends up to a thousand events a second, and the
/// clock only needs to know that one came.
const INPUT_BATCH: Duration = Duration::from_millis(250);

/// The time of the last real input, kept by a thread on its own X connection.
///
/// Raw XInput 2 events reach the root window whatever holds a grab (the
/// screensaver and the locker grab the keyboard and pointer), and only input
/// devices produce them, the XTEST devices included, so `xdotool` and remote
/// desktop input still count as activity.
struct InputClock {
    start: Instant,
    /// Milliseconds after `start` of the last input or [`InputClock::touch`].
    last_ms: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
}

impl InputClock {
    /// Start the thread. `None` when the X server has no XInput 2.1 or the
    /// selection fails; the caller then uses the MIT-SCREEN-SAVER idle time.
    fn start() -> Option<Self> {
        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen_num].root;
        conn.extension_information(xinput::X11_EXTENSION_NAME)
            .ok()
            .flatten()?;
        let version = conn.xinput_xi_query_version(2, 2).ok()?.reply().ok()?;
        if (version.major_version, version.minor_version) < (2, 1) {
            return None;
        }
        let mask = xinput::XIEventMask::RAW_KEY_PRESS
            | xinput::XIEventMask::RAW_BUTTON_PRESS
            | xinput::XIEventMask::RAW_MOTION
            | xinput::XIEventMask::RAW_TOUCH_BEGIN;
        conn.xinput_xi_select_events(
            root,
            &[xinput::EventMask {
                deviceid: xinput::Device::ALL_MASTER.into(),
                mask: vec![mask],
            }],
        )
        .ok()?
        .check()
        .ok()?;
        let clock = InputClock {
            start: Instant::now(),
            last_ms: Arc::new(AtomicU64::new(0)),
            alive: Arc::new(AtomicBool::new(true)),
        };
        let (start, last, alive) = (clock.start, clock.last_ms.clone(), clock.alive.clone());
        let spawned = thread::Builder::new().name("input-clock".into()).spawn(move || {
            let is_input = |e: &Event| {
                matches!(
                    e,
                    Event::XinputRawKeyPress(_)
                        | Event::XinputRawButtonPress(_)
                        | Event::XinputRawMotion(_)
                        | Event::XinputRawTouchBegin(_)
                )
            };
            loop {
                match conn.wait_for_event() {
                    Ok(e) if is_input(&e) => {
                        last.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                        thread::sleep(INPUT_BATCH);
                        while let Ok(Some(e)) = conn.poll_for_event() {
                            if is_input(&e) {
                                last.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("niruidle: input clock lost its X connection ({e}); using the X idle time");
                        alive.store(false, Ordering::Relaxed);
                        return;
                    }
                }
            }
        });
        spawned.ok()?;
        Some(clock)
    }

    /// Count now as activity: niruidle's own resets (an inhibition, an unlock,
    /// a program's `SimulateUserActivity`) restart this clock as well.
    fn touch(&self) {
        self.last_ms
            .store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// Seconds since the last input, or `None` once the thread has stopped.
    fn idle(&self) -> Option<u64> {
        if !self.alive.load(Ordering::Relaxed) {
            return None;
        }
        let now = self.start.elapsed().as_millis() as u64;
        Some(now.saturating_sub(self.last_ms.load(Ordering::Relaxed)) / 1000)
    }
}

/// The idle time the ladder runs on. The X server's clock counts input
/// before niruidle started, so the larger of the two is right: after start
/// they agree until a program resets the X clock, which is then ignored.
fn ladder_idle(input: Option<u64>, x: Option<u64>) -> u64 {
    match input {
        Some(i) => i.max(x.unwrap_or(0)),
        None => x.unwrap_or(0),
    }
}

/// Seconds since the last input, ignoring the X clock reset that
/// `niructl lock` makes before it starts the locker: one poll of grace when
/// the X clock was just reset without input, so niruidle neither starts the
/// screensaver over a lock in progress nor a second locker. Only one: a
/// program that resets the X clock every few seconds cannot hold the ladder.
fn just_reset(input_idle: u64, x_idle: Option<u64>) -> bool {
    x_idle.is_some_and(|x| x < POLL.as_secs() + 1 && input_idle > x + POLL.as_secs())
}

fn idle_seconds(conn: &x11rb::rust_connection::RustConnection, root: Window) -> Option<u64> {
    let info = conn.screensaver_query_info(root).ok()?.reply().ok()?;
    Some(u64::from(info.ms_since_user_input) / 1000)
}

/// Seconds of idle at which the lock step is due.
fn lock_seconds(s: &Settings) -> u64 {
    s.screen.saturating_add(s.lock_after).saturating_mul(60)
}

/// Whether the screensaver step should be up at `idle` seconds.
///
/// It runs from `screensaver_timeout` until the lock. With
/// `lock_after_screensaver = 0` the lock comes at the same moment, so there is
/// no screensaver period at all: the lock screen is what covers the display.
/// Without a locker the screensaver stays, so the screen is still covered.
fn wants_saver(s: &Settings, idle: u64, locked: bool, has_locker: bool) -> bool {
    !locked
        && s.screen > 0
        && idle >= s.screen.saturating_mul(60)
        && (!has_locker || idle < lock_seconds(s))
}

/// Whether the lock step is due. `lock_after_screensaver = 0` locks at
/// `screensaver_timeout`, as documented; it never means "do not lock".
fn wants_lock(s: &Settings, idle: u64, locked: bool, has_locker: bool) -> bool {
    !locked && has_locker && s.screen > 0 && idle >= lock_seconds(s)
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg == "--version" {
        println!("NIRUIDLE {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let mut settings = load_settings();
    if arg == "--config" {
        println!(
            "screensaver after {} min\nlock {} min later\ndisplay off {} min after locking\nprograms may pause the ladder: {}\nlegacy lock_timeout_minutes: {} (ignored; never terminate a locker)",
            settings.screen,
            settings.lock_after,
            settings.dpms_after,
            if settings.honor_apps { "yes" } else { "no" },
            settings.lock_timeout
        );
        return;
    }

    let (conn, screen_num) = match x11rb::connect(None) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("niruidle: cannot connect to X11: {e}");
            std::process::exit(1)
        }
    };
    let root = conn.setup().roots[screen_num].root;
    if conn
        .extension_information(x11rb::protocol::screensaver::X11_EXTENSION_NAME)
        .ok()
        .flatten()
        .is_none()
    {
        eprintln!("niruidle: the X server has no MIT-SCREEN-SAVER extension");
        std::process::exit(1)
    }

    let locker = first_available(&lockers());
    let saver = first_available(&["niruscreensaver".to_string()]);

    let mut screen_at = settings.screen.saturating_mul(60);
    let mut dpms_at = settings
        .screen
        .saturating_add(settings.lock_after)
        .saturating_add(settings.dpms_after)
        .saturating_mul(60);
    let mut config_stamp = shared_config_stamp();
    let lock_ready = conn
        .intern_atom(false, b"_NIRU_LOCK_READY")
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .map(|reply| reply.atom)
        .unwrap_or(0);

    eprintln!(
        "niruidle: screen after {} min, lock {} min later, display off {} min after that{}{}",
        settings.screen,
        settings.lock_after,
        settings.dpms_after,
        if saver.is_none() {
            " (no niruscreensaver: blanking)"
        } else {
            ""
        },
        if locker.is_none() {
            " (no locker found)"
        } else {
            ""
        },
    );

    let mut saver_child = None;
    let mut locker_child = None;
    let mut blanked = false;
    let mut display_off = false;
    let mut retry_lock_at = Instant::now();
    let inhibit = inhibit_path();
    let mut inhibited = false;
    // Whether the state file (`niructl idle`, which also turned the X saver
    // and DPMS off) took part, so its end restores them; the session
    // property alone changed neither.
    let mut file_inhibited = false;
    let intern = |name: &[u8]| {
        conn.intern_atom(false, name)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| reply.atom)
            .unwrap_or(0)
    };
    let session_inhibit = intern(b"_NIRU_IDLE_INHIBIT");
    // Tell `niructl studio` that this daemon honours the session property.
    let features = intern(b"_NIRUIDLE_FEATURES");
    let utf8 = intern(b"UTF8_STRING");
    if features != 0 && utf8 != 0 {
        let _ = conn.change_property8(
            PropMode::REPLACE,
            root,
            features,
            utf8,
            b"session-inhibit app-inhibit",
        );
        let _ = conn.flush();
    }
    let mut external_locked = false;
    // Programs that asked the screensaver to wait (org.freedesktop.ScreenSaver).
    // `_NIRUIDLE_APP_INHIBIT` on the root lists them for `niructl idle status`;
    // like the session property it vanishes with the X server.
    let app_inhibit_atom = intern(b"_NIRUIDLE_APP_INHIBIT");
    if app_inhibit_atom != 0 {
        let _ = conn.delete_property(root, app_inhibit_atom);
    }
    let mut bus = settings.honor_apps.then(idle_bus::start);
    if !settings.honor_apps {
        eprintln!("niruidle: [idle] honor_app_inhibit = false: requests from programs are ignored");
    }
    let mut published = (0u64, settings.honor_apps);
    let (prox_device, mut prox_after) = load_proximity();
    if !prox_device.is_empty() {
        eprintln!("niruidle: proximity lock: {prox_device}, locks {prox_after}s after it leaves");
    }
    let prox_shared = std::sync::Arc::new(std::sync::Mutex::new(prox_device));
    let clock = InputClock::start();
    if clock.is_some() {
        eprintln!("niruidle: idle time from real input (XInput 2); programs cannot reset it");
    } else {
        eprintln!("niruidle: no XInput 2.1: idle time from the X server, which programs can reset");
    }
    let mut graced = false;
    let mut pointer_at: Option<(i16, i16)> = None;
    let mut last_activity_log: Option<Instant> = None;
    let prox_results = start_proximity(prox_shared.clone());
    let mut prox_tracker = proximity::Tracker::default();
    let mut prox_last: Option<proximity::Presence> = None;

    loop {
        // `niructl timing set` writes the shared file. Pick the new values up
        // without a logout, without a signal and without polling the content:
        // one metadata stat per five-second tick is effectively free.
        let stamp = shared_config_stamp();
        if stamp != config_stamp {
            settings = load_settings();
            screen_at = settings.screen.saturating_mul(60);
            dpms_at = settings
                .screen
                .saturating_add(settings.lock_after)
                .saturating_add(settings.dpms_after)
                .saturating_mul(60);
            config_stamp = stamp;
            eprintln!(
                "niruidle: timing reloaded: screen after {} min, lock {} min later, display off {} min after that{}",
                settings.screen,
                settings.lock_after,
                settings.dpms_after,
                if settings.honor_apps { "" } else { "; requests from programs are ignored" }
            );
            if settings.honor_apps && bus.is_none() {
                bus = Some(idle_bus::start());
            }
            let (device, after) = load_proximity();
            prox_after = after;
            if let Ok(mut current) = prox_shared.lock() {
                if *current != device {
                    eprintln!(
                        "niruidle: proximity lock: {}",
                        if device.is_empty() {
                            "off".to_string()
                        } else {
                            format!("{device}, locks {after}s after it leaves")
                        }
                    );
                    *current = device;
                    prox_tracker = proximity::Tracker::default();
                    prox_last = None;
                }
            }
        }
        // Requests from programs, as one snapshot per tick: the bus thread
        // answers calls at once, the ladder looks at the table here.
        let mut apps: Option<(usize, String)> = None;
        if let Some(handle) = &bus {
            let mut shared = idle_bus::lock(handle);
            shared.saver_active = saver_child.is_some() || blanked || locker_child.is_some();
            if std::mem::take(&mut shared.activity) {
                let _ = conn.force_screen_saver(ScreenSaver::RESET);
                if let Some(c) = &clock {
                    c.touch();
                }
                if last_activity_log.map_or(true, |t: Instant| t.elapsed() >= ACTIVITY_LOG_EVERY) {
                    eprintln!("niruidle: a program reported user activity (SimulateUserActivity)");
                    last_activity_log = Some(Instant::now());
                }
            }
            if settings.honor_apps && !shared.inhibitors.is_empty() {
                apps = Some((shared.inhibitors.len(), shared.inhibitors.summary()));
            }
            if (shared.generation, settings.honor_apps) != published {
                published = (shared.generation, settings.honor_apps);
                let text = apps.as_ref().map(|(_, t)| t.as_str()).unwrap_or("");
                drop(shared);
                publish_app_inhibitors(&conn, root, app_inhibit_atom, utf8, text);
            }
        }
        // The proximity lock comes before every inhibition: a video playing or
        // `niructl idle off` keeps the screen awake for the person in front
        // of it, not for an empty chair. It also runs before the locker is
        // reaped, so checks made while the session was locked count as
        // locked even when the unlock happened since.
        while let Ok(presence) = prox_results.try_recv() {
            if prox_last.as_ref() != Some(&presence) {
                eprintln!(
                    "niruidle: proximity: {}",
                    match &presence {
                        proximity::Presence::Near => "phone in reach".to_string(),
                        proximity::Presence::Away => "phone out of reach".to_string(),
                        proximity::Presence::Unknown(why) => format!("cannot check ({why})"),
                    }
                );
                prox_last = Some(presence.clone());
            }
            let locked_now =
                locker_child.is_some() || lock_ready_window(&conn, root, lock_ready).is_some();
            match prox_tracker.update(
                presence,
                Instant::now(),
                locked_now,
                Duration::from_secs(prox_after),
            ) {
                proximity::Action::Lock => {
                    if let Some(l) = &locker {
                        stop_saver(&mut saver_child);
                        eprintln!("niruidle: phone out of reach for {prox_after}s: lock");
                        locker_child = spawn_locker(l);
                        retry_lock_at = Instant::now() + POLL;
                    }
                }
                proximity::Action::Wake => {
                    eprintln!("niruidle: phone back: screen on for the password prompt");
                    if display_off {
                        xset_dpms("on");
                        display_off = false;
                    }
                    let _ = conn.force_screen_saver(ScreenSaver::RESET);
                    let _ = conn.flush();
                    if let Some(c) = &clock {
                        c.touch();
                    }
                }
                proximity::Action::Nothing => {}
            }
        }
        keep_x_timers_off(&conn);
        reap(&mut saver_child);
        if let Some(status) = reap(&mut locker_child) {
            if status.success() {
                let _ = conn.force_screen_saver(ScreenSaver::RESET);
                let _ = conn.flush();
                if let Some(c) = &clock {
                    c.touch();
                }
            } else {
                eprintln!("niruidle: locker exited unsuccessfully: {status}");
                retry_lock_at = Instant::now() + POLL;
            }
        }
        // Explicit inhibition state, never a guessed process id. While it is
        // active, reset the X idle clock so no saver or locker fires, and undo
        // any display-off state niruidle itself set.
        let by_file = inhibit_active_from(&inhibit, now_secs());
        let by_session = !by_file && session_inhibit_active(&conn, root, session_inhibit);
        if by_file || by_session || apps.is_some() {
            file_inhibited |= by_file;
            if !inhibited {
                let source = if by_file {
                    String::new()
                } else if by_session {
                    " (session)".to_string()
                } else {
                    let n = apps.as_ref().map(|(n, _)| *n).unwrap_or(0);
                    format!(" ({n} program request{})", if n == 1 { "" } else { "s" })
                };
                eprintln!("niruidle: idle inhibition on{source}");
                stop_saver(&mut saver_child);
                // A program's request does not wake a display that went dark
                // behind the lock: a video starting in a background tab must
                // not light up a locked screen.
                let locked_now =
                    locker_child.is_some() || lock_ready_window(&conn, root, lock_ready).is_some();
                if display_off && (by_file || by_session || !locked_now) {
                    xset_dpms("on");
                    display_off = false;
                }
                inhibited = true;
            }
            let _ = conn.force_screen_saver(ScreenSaver::RESET);
            let _ = conn.flush();
            if let Some(c) = &clock {
                c.touch();
            }
            thread::sleep(POLL);
            continue;
        }
        if inhibited {
            eprintln!("niruidle: idle inhibition off");
            if file_inhibited {
                xset_saver("default", "+dpms");
            }
            inhibited = false;
            file_inhibited = false;
        }

        // XTEST pointer motion (xdotool, remote desktops) makes no raw
        // motion event on every server, so a pointer that moved since the
        // last poll counts as input too. A program cannot fake this without
        // moving the pointer, which is input.
        if let Some(p) = conn.query_pointer(root).ok().and_then(|c| c.reply().ok()) {
            let at = (p.root_x, p.root_y);
            if pointer_at.is_some_and(|last| last != at) {
                if let Some(c) = &clock {
                    c.touch();
                }
            }
            pointer_at = Some(at);
        }
        let x_idle = idle_seconds(&conn, root);
        let input_idle = clock.as_ref().and_then(InputClock::idle);
        let idle = ladder_idle(input_idle, x_idle);
        // One poll of grace after an X clock reset without input (see
        // `just_reset`), then the ladder goes on whatever resets the X clock.
        let hold = !graced && input_idle.is_some() && just_reset(idle, x_idle);
        graced = hold;
        if hold && idle >= screen_at.max(1) {
            thread::sleep(POLL);
            continue;
        }
        let external_lock = lock_ready_window(&conn, root, lock_ready);
        if external_lock.is_some() != external_locked {
            external_locked = external_lock.is_some();
            if external_locked {
                eprintln!("niruidle: external lock active; pausing the idle ladder");
            } else {
                eprintln!("niruidle: external lock cleared");
            }
        }
        if external_locked {
            stop_saver(&mut saver_child);
        }
        let locked = locker_child.is_some() || external_locked;

        // Any activity below the first step resets the whole ladder.
        if idle < screen_at.max(1) {
            stop_saver(&mut saver_child);
            blanked = false;
            if display_off {
                xset_dpms("on");
                display_off = false;
            }
        }

        // 1. Screensaver.
        if saver_child.is_none()
            && !blanked
            && wants_saver(&settings, idle, locked, locker.is_some())
        {
            match &saver {
                Some(s) => {
                    eprintln!("niruidle: idle {idle}s: screensaver");
                    saver_child = spawn(s);
                }
                None => {
                    eprintln!("niruidle: idle {idle}s: blank");
                    xset_dpms("off");
                    blanked = true;
                    display_off = true;
                }
            }
        }

        // Track a nonblocking child, never a timeout wrapper. Input activity
        // cannot mark a still-running locker as unlocked or spawn another one.
        if wants_lock(&settings, idle, locked, locker.is_some()) && Instant::now() >= retry_lock_at
        {
            stop_saver(&mut saver_child);
            if let Some(l) = &locker {
                eprintln!("niruidle: idle {idle}s: lock");
                locker_child = spawn_locker(l);
                retry_lock_at = Instant::now() + POLL;
            }
        }

        // 3. Display off, only while locked: an unlocked machine going dark
        //    would be a surprise.
        if !display_off && settings.dpms_after > 0 && locked && idle >= dpms_at {
            eprintln!("niruidle: idle {idle}s: display off");
            xset_dpms("off");
            display_off = true;
        }

        thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_resetting_the_x_clock_does_not_reset_the_ladder() {
        // A browser called XResetScreenSaver: X says 3 s, real input 400 s ago.
        assert_eq!(ladder_idle(Some(400), Some(3)), 400);
        // Input before niruidle started is only known to the X server.
        assert_eq!(ladder_idle(Some(10), Some(900)), 900);
        // Without XInput 2 the X clock is all there is.
        assert_eq!(ladder_idle(None, Some(42)), 42);
        assert_eq!(ladder_idle(None, None), 0);
    }

    #[test]
    fn only_a_fresh_x_reset_without_input_gets_grace() {
        assert!(just_reset(400, Some(2)));
        // Real input: both clocks agree.
        assert!(!just_reset(2, Some(2)));
        // An older reset is not fresh.
        assert!(!just_reset(400, Some(30)));
        assert!(!just_reset(400, None));
    }

    fn timing(screen: u64, lock_after: u64) -> Settings {
        Settings {
            screen,
            lock_after,
            dpms_after: 15,
            lock_timeout: 0,
            honor_apps: true,
        }
    }

    #[test]
    fn lock_after_zero_locks_together_with_the_screensaver() {
        let s = timing(5, 0);
        // Before the timeout: nothing.
        assert!(!wants_saver(&s, 299, false, true));
        assert!(!wants_lock(&s, 299, false, true));
        // At the timeout: the lock, and no separate screensaver.
        assert!(wants_lock(&s, 300, false, true));
        assert!(!wants_saver(&s, 300, false, true));
        // Once locked, neither step starts again.
        assert!(!wants_lock(&s, 900, true, true));
        assert!(!wants_saver(&s, 900, true, true));
    }

    #[test]
    fn lock_after_minutes_shows_the_screensaver_first() {
        let s = timing(5, 2);
        assert!(wants_saver(&s, 300, false, true));
        assert!(!wants_lock(&s, 300, false, true));
        assert!(wants_saver(&s, 419, false, true));
        assert!(wants_lock(&s, 420, false, true));
        assert!(!wants_saver(&s, 420, false, true));
    }

    #[test]
    fn without_a_locker_the_screensaver_stays_up() {
        let s = timing(5, 0);
        assert!(!wants_lock(&s, 300, false, false));
        assert!(wants_saver(&s, 300, false, false));
        assert!(wants_saver(&s, 3600, false, false));
    }

    #[test]
    fn the_idle_lock_tells_nirulock_it_is_not_manual() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("niruidle-reason-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("reason");
        let locker = dir.join("nirulock");
        std::fs::write(
            &locker,
            format!(
                "#!/bin/sh\nprintf '%s' \"$NIRULOCK_REASON\" > '{}'\n",
                out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&locker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = spawn_locker(locker.to_str().unwrap()).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "idle");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saver_is_waited_and_cannot_leave_a_zombie() {
        let mut child = Some(Command::new("sleep").arg("60").spawn().unwrap());
        let pid = child.as_ref().unwrap().id();
        stop_saver(&mut child);
        assert!(child.is_none());
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        stop_saver(&mut child); // No fake PID/sentinel is ever sent a signal.
    }

    #[test]
    fn only_explicit_unexpired_inhibition_is_active() {
        let dir = std::env::temp_dir().join(format!("niruidle-inhibit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inhibit");
        assert!(!inhibit_active_from(&path, 100));
        std::fs::write(&path, "0\n").unwrap();
        assert!(inhibit_active_from(&path, 100));
        std::fs::write(&path, "200\n").unwrap();
        assert!(inhibit_active_from(&path, 100));
        assert!(!inhibit_active_from(&path, 200));
        std::fs::write(&path, "garbage\n").unwrap();
        assert!(!inhibit_active_from(&path, 100));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn any_holder_text_is_a_session_inhibition() {
        assert!(session_inhibit_value(b"studio"));
        assert!(!session_inhibit_value(b""));
        assert!(!session_inhibit_value(b"  "));
    }

    #[test]
    fn completed_child_status_is_reaped_once() {
        let mut child = Some(Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap());
        let end = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Some(status) = reap(&mut child) {
                break status;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(status.code(), Some(7));
        assert!(child.is_none());
        assert!(reap(&mut child).is_none());
    }
}
