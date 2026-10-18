//! niruidle: the NIRU idle ladder.
//!
//! Watches how long the screen has been idle and runs, one after another:
//!
//!   1. niruscreensaver (or a blank screen when it is not installed)
//!   2. the screen locker (nirulock, NIRUPI's wrapper, or slock)
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
    process::{Command, Stdio},
    thread,
    time::Duration,
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
    s
}

/// First program in PATH that exists.
fn first_available(candidates: &[String]) -> Option<String> {
    candidates
        .iter()
        .find(|c| {
            if c.contains('/') {
                std::path::Path::new(c).is_file()
            } else {
                std::env::var("PATH")
                    .map(|p| {
                        p.split(':')
                            .any(|d| std::path::Path::new(d).join(c).is_file())
                    })
                    .unwrap_or(false)
            }
        })
        .cloned()
}

fn lockers() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    vec![
        "nirulock".into(),
        format!("{home}/.local/bin/nirupi-lock.sh"),
        "slock".into(),
        "swaylock".into(),
    ]
}

/// Start a program and return its pid, discarding its output.
fn spawn(program: &str) -> Option<u32> {
    Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
        .map(|c| c.id())
}

/// Run a program and wait for it: the locker blocks until the password is
/// accepted, which is exactly what the ladder needs.
fn run_blocking(program: &str) -> bool {
    Command::new(program)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn xset_dpms(force: &str) {
    let _ = Command::new("xset")
        .args(["dpms", "force", force])
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
            "screensaver after {} min\nlock {} min later\ndisplay off {} min after locking",
            settings.screen, settings.lock_after, settings.dpms_after
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

    let screen_at = settings.screen * 60;
    let lock_at = (settings.screen + settings.lock_after) * 60;
    let dpms_at = (settings.screen + settings.lock_after + settings.dpms_after) * 60;

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

    let mut saver_pid: Option<u32> = None;
    let mut locked = false;
    let mut display_off = false;

    loop {
        let idle = idle_seconds(&conn, root).unwrap_or(0);

        // Any activity below the first step resets the whole ladder.
        if idle < screen_at {
            if let Some(pid) = saver_pid.take() {
                let _ = Command::new("kill").arg(pid.to_string()).status();
            }
            if display_off {
                xset_dpms("on");
                display_off = false;
            }
            locked = false;
        }

        // 1. Screensaver.
        if saver_pid.is_none() && !locked && settings.screen > 0 && idle >= screen_at {
            match &saver {
                Some(s) => {
                    eprintln!("niruidle: idle {idle}s: screensaver");
                    saver_pid = spawn(s);
                }
                None => {
                    eprintln!("niruidle: idle {idle}s: blank");
                    xset_dpms("off");
                    saver_pid = Some(0);
                }
            }
        }

        // 2. Lock. Run it in the foreground: this process waits until the
        //    password is accepted, then the idle counter is already low again.
        if !locked && settings.lock_after > 0 && idle >= lock_at {
            if let Some(pid) = saver_pid.take() {
                if pid != 0 {
                    let _ = Command::new("kill").arg(pid.to_string()).status();
                    // Give it a moment to release the screen.
                    thread::sleep(Duration::from_millis(300));
                }
            }
            if let Some(l) = &locker {
                eprintln!("niruidle: idle {idle}s: lock");
                run_blocking(l);
            }
            locked = true;
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
