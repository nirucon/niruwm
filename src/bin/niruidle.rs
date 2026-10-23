//! niruidle: the NIRU idle ladder.
//!
//! Watches how long the screen has been idle and runs, one after another:
//!
//!   1. niruscreensaver (or a blank screen when it is not installed)
//!   2. the screen locker (nirulock, NIRULOCK, or slock)
//!   3. display off
//!
//! Idle time comes from the X server's MIT-SCREEN-SAVER extension, so user
//! activity is reported by X itself and no input device is watched.
//!
//! The times are read from `~/.config/niruui/screensaver.toml`, the same file
//! NIRUSCREENSAVER uses, so the settings describe the whole sequence in one
//! place. This exists so the ladder works on a clean install with no
//! `xidlehook`, `xprintidle` or shell glue.

use std::{
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::{Connection, RequestConnection as _},
    protocol::{screensaver::ConnectionExt as _, xproto::*},
};

/// How often the idle time is checked. Five seconds is precise enough for a
/// fifteen minute timer and costs nothing.
const POLL: Duration = Duration::from_secs(5);

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
    };
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
    if let Some(n) = read_u64(&v, "idle_minutes") {
        s.screen = n;
    }
    if let Some(n) = read_u64(&v, "lock_after_minutes") {
        s.lock_after = n;
    }
    if let Some(n) = read_u64(&v, "dpms_off_minutes") {
        s.dpms_after = n;
    }
    if let Some(n) = read_u64(&v, "lock_timeout_minutes") {
        s.lock_timeout = n;
    }
    s
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

fn xset_saver(force: &str, dpms: &str) {
    let _ = Command::new("xset")
        .args(["s", force, dpms])
        .stderr(Stdio::null())
        .status();
}

fn idle_seconds(conn: &x11rb::rust_connection::RustConnection, root: Window) -> Option<u64> {
    let info = conn.screensaver_query_info(root).ok()?.reply().ok()?;
    Some(u64::from(info.ms_since_user_input) / 1000)
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg == "--version" {
        println!("NIRUIDLE {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let settings = load_settings();
    if arg == "--config" {
        println!(
            "screensaver after {} min\nlock {} min later\ndisplay off {} min after locking\nlegacy lock_timeout_minutes: {} (ignored; never terminate a locker)",
            settings.screen, settings.lock_after, settings.dpms_after, settings.lock_timeout
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

    let screen_at = settings.screen.saturating_mul(60);
    let lock_at = settings
        .screen
        .saturating_add(settings.lock_after)
        .saturating_mul(60);
    let dpms_at = settings
        .screen
        .saturating_add(settings.lock_after)
        .saturating_add(settings.dpms_after)
        .saturating_mul(60);

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

    loop {
        reap(&mut saver_child);
        if let Some(status) = reap(&mut locker_child) {
            if status.success() {
                let _ = conn.force_screen_saver(ScreenSaver::RESET);
                let _ = conn.flush();
            } else {
                eprintln!("niruidle: locker exited unsuccessfully: {status}");
                retry_lock_at = Instant::now() + POLL;
            }
        }
        // Explicit inhibition state, never a guessed process id. While it is
        // active, reset the X idle clock so no saver or locker fires, and undo
        // any display-off state niruidle itself set.
        if inhibit_active_from(&inhibit, now_secs()) {
            if !inhibited {
                eprintln!("niruidle: idle inhibition on");
                stop_saver(&mut saver_child);
                if display_off {
                    xset_dpms("on");
                    display_off = false;
                }
                inhibited = true;
            }
            let _ = conn.force_screen_saver(ScreenSaver::RESET);
            let _ = conn.flush();
            thread::sleep(POLL);
            continue;
        }
        if inhibited {
            eprintln!("niruidle: idle inhibition off");
            xset_saver("default", "+dpms");
            inhibited = false;
        }

        let idle = idle_seconds(&conn, root).unwrap_or(0);
        let locked = locker_child.is_some();

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
            && !locked
            && settings.screen > 0
            && idle >= screen_at
            && (settings.lock_after == 0 || idle < lock_at)
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
        if !locked && settings.lock_after > 0 && idle >= lock_at && Instant::now() >= retry_lock_at
        {
            stop_saver(&mut saver_child);
            if let Some(l) = &locker {
                eprintln!("niruidle: idle {idle}s: lock");
                locker_child = spawn(l);
                retry_lock_at = Instant::now() + POLL;
            }
        }

        // 3. Display off, only while locked: an unlocked machine going dark
        //    would be a surprise.
        if !display_off && settings.dpms_after > 0 && locker_child.is_some() && idle >= dpms_at {
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
