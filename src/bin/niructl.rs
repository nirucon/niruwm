//! niructl — small control channel for NIRUWM and NIRUBAR.
//!
//! Communication is a client message on the root window. It is deliberately
//! the same mechanism `xdotool` and EWMH clients use, so no daemon, socket or
//! dependency is required.
#[path = "../bindings.rs"]
#[allow(dead_code)] // niructl only reads the shared config shape.
mod bindings;
#[path = "../config.rs"]
#[allow(dead_code)] // shared with NIRUWM; theme management consumes part of it
mod config;
#[path = "../layout.rs"]
#[allow(dead_code)] // niructl prints the layout names
mod layout;
#[path = "../monitor.rs"]
#[allow(dead_code)]
mod monitor;
#[path = "../session_state.rs"]
#[allow(dead_code)] // niructl reads the list, it never writes one
mod session_state;
#[path = "../wallpaper.rs"]
#[allow(dead_code)] // status resolves assets without applying a wallpaper.
mod wallpaper;

use chrono::{Local, TimeZone, Timelike};
use config::{Config, Result, Theme};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use x11rb::{
    connection::{Connection, RequestConnection as _},
    protocol::{randr::ConnectionExt as _, xproto::*},
    wrapper::ConnectionExt as _,
};

const USAGE: &str = "Usage: niructl status | tag N | movetotag N | layout NAME | layouts | \
theme NAME | themes | theme-catalog | theme-import | theme-remove | theme-hide | theme-show | keys | monitors | reload | restart-bar | quit | save | save-session | \
clear-session | brightness up|down|set N|status | nightlight on|off|toggle|status|temperature K|apply|watch | \
compositor on|off|toggle|status | \
timing show|set KEY MINUTES | \
wallpaper list|status|apply [--json]|set PATH|mode VALUE|rotation MINUTES|scale VALUE|follow-theme on|off | \
         output list|set NAME KEY VALUE|reset NAME | \
autostart | recipe list|run NAME | mode focus|presentation|stop|status [MINUTES] | \
display list [--json]|save NAME|save-json NAME|preview NAME [SECONDS]|preview-json [SECONDS]|apply NAME|keep|revert|status [--json]|delete NAME|rename OLD NEW|startup show|set NAME|off|apply|rotate OUTPUT normal|left|right|inverted|auto | \
layout show|save | \
powerprofile status|list|set NAME|next | \
lock | screensaver | logout | suspend | reboot | shutdown | dnd on|off|toggle|status | \
audio outputs|sources|output ID|next|source ID|next|mute|mic-status|mic-mute [on|off|toggle]|mic-level up|down|set N|volume up|down | \
bluetooth status|list|power on|off|toggle|connect MAC|disconnect MAC | \
explain [--launcher] | idle on|off|toggle|status [MINUTES]";
const CMD_ATOM: &str = "_NIRUWM_COMMAND";
const CONFIG_SERIAL: &str = "_NIRU_CONFIG_SERIAL";

fn which(name: &str) -> bool {
    std::env::var("PATH")
        .map(|p| {
            p.split(':')
                .filter(|d| !d.is_empty())
                .any(|d| std::path::Path::new(d).join(name).is_file())
        })
        .unwrap_or(false)
}

fn command(program: &str, args: &[String]) -> Result<std::process::Output> {
    let out = std::process::Command::new(program).args(args).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "{program} failed{}",
            if err.is_empty() {
                String::new()
            } else {
                format!(": {err}")
            }
        )
        .into());
    }
    Ok(out)
}

fn backlight_percent(device: &str) -> Option<u32> {
    let mut args = vec!["-m".to_string()];
    if !device.is_empty() {
        args.push("-d".into());
        args.push(device.to_string());
    }
    let out = std::process::Command::new("brightnessctl")
        .args(&args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split(',').nth(3)?.trim_end_matches('%').parse().ok()
}

fn notify(summary: &str, body: &str, value: Option<u32>) {
    // Prefer the suite's coalesced hardware OSD when its daemon is running.
    // The CLI exits nonzero without a socket, so a Dunst session falls back
    // to the ordinary notification path.
    if let Some(osd) = program("nirunotify") {
        let channel = summary.to_lowercase().replace(' ', "-");
        if std::process::Command::new(osd)
            .args(["--osd", &channel, body])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return;
        }
    }
    let mut args = vec![
        "-a".to_string(),
        "NIRU".to_string(),
        "-r".to_string(),
        "9201".to_string(),
        "-t".to_string(),
        "1200".to_string(),
    ];
    if let Some(value) = value {
        args.push("-h".to_string());
        args.push(format!("int:value:{value}"));
    }
    args.push(summary.to_string());
    args.push(body.to_string());
    let _ = std::process::Command::new("notify-send").args(args).spawn();
}

fn brightness(args: &[String]) -> Result<()> {
    if !which("brightnessctl") {
        return Err("brightnessctl is not installed".into());
    }
    let cfg = Config::load().unwrap_or_default();
    let device = cfg.brightness.device.clone();
    let step = cfg.brightness.step.clamp(1, 50);
    let action = args.first().map(String::as_str).unwrap_or("status");
    let set = |value: &str| -> Result<()> {
        let mut full = vec!["-q".to_string()];
        if !device.is_empty() {
            full.push("-d".into());
            full.push(device.clone());
        }
        full.push("set".into());
        full.push(value.into());
        command("brightnessctl", &full)?;
        Ok(())
    };
    match action {
        "up" => set(&format!("{step}%+"))?,
        "down" => set(&format!("{step}%-"))?,
        "set" => {
            let value: u32 = args
                .get(1)
                .ok_or("brightness set needs a percentage")?
                .parse()
                .map_err(|_| "brightness set: not a number")?;
            if value > 100 {
                return Err("brightness set: use 0..100".into());
            }
            set(&format!("{value}%"))?;
        }
        "status" => {
            let percent = backlight_percent(&device).ok_or("no backlight device is available")?;
            println!("{percent}%");
            return Ok(());
        }
        _ => return Err("brightness needs up, down, set N or status".into()),
    }
    let percent = backlight_percent(&device);
    notify(
        "Brightness",
        &format!(
            "{}%",
            percent
                .map(|p| p.to_string())
                .unwrap_or_else(|| "--".into())
        ),
        percent,
    );
    Ok(())
}

fn session_user() -> Option<String> {
    std::env::var("USER").ok().filter(|user| !user.is_empty())
}

fn compositor_running() -> bool {
    let Some(user) = session_user() else {
        return false;
    };
    std::process::Command::new("pgrep")
        .args(["-u", &user, "-x", "picom"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn compositor(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("status");
    let quiet = args.iter().any(|arg| arg == "--quiet");
    let running = compositor_running();
    match action {
        "status" => println!("{}", if running { "on" } else { "off" }),
        "on" => {
            if running {
                println!("on");
                return Ok(());
            }
            if !which("picom") {
                return Err("picom is not installed".into());
            }
            let mut child = std::process::Command::new("picom")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("cannot start picom: {e}"))?;
            for _ in 0..40 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if compositor_running() {
                    if !quiet {
                        notify("Compositor", "on", None);
                    }
                    println!("on");
                    return Ok(());
                }
                if child.try_wait().ok().flatten().is_some() {
                    return Err("picom exited during startup; check its config/backend".into());
                }
            }
            return Err("picom did not stay running".into());
        }
        "off" => {
            if running {
                if let Some(user) = session_user() {
                    let _ = std::process::Command::new("pkill")
                        .args(["-u", &user, "-x", "picom"])
                        .status();
                }
                if !quiet {
                    notify("Compositor", "off", None);
                }
            }
            println!("off");
        }
        "toggle" => {
            if running {
                compositor(&["off".to_string(), "--quiet".to_string()])
            } else {
                compositor(&["on".to_string()])
            }?;
        }
        _ => return Err("compositor needs on, off, toggle or status".into()),
    }
    Ok(())
}

fn gamma_signature() -> Option<String> {
    let out = std::process::Command::new("xrandr")
        .arg("--verbose")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("Gamma:"))
        .collect();
    (!lines.is_empty()).then(|| lines.join("|"))
}

fn nightlight_state() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/state")
        });
    base.join("niruwm/nightlight")
}

fn clock_minutes(value: &str) -> Option<u32> {
    let (hour, minute) = value.trim().split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

fn fixed_schedule_active(now: u32, on: u32, off: u32) -> bool {
    if on < off {
        now >= on && now < off
    } else {
        now >= on || now < off
    }
}

fn solar_temperature(location: &str, day: u32, night: u32) -> Option<u32> {
    let out = std::process::Command::new("gammastep")
        .args(["-l", location, "-t", &format!("{day}:{night}"), "-p"])
        .output()
        .ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for line in text.lines() {
        if let Some(value) = line.split("Color temperature:").nth(1) {
            return value
                .trim()
                .trim_end_matches('K')
                .trim()
                .parse::<u32>()
                .ok();
        }
    }
    None
}

fn nightlight(args: &[String]) -> Result<()> {
    if !which("gammastep") {
        return Err("gammastep is not installed".into());
    }
    let cfg = Config::load()?;
    let nl = &cfg.nightlight;
    let (latitude, longitude) = if nl.latitude != 0.0 || nl.longitude != 0.0 {
        (nl.latitude, nl.longitude)
    } else {
        (cfg.weather_latitude, cfg.weather_longitude)
    };
    if latitude == 0.0 && longitude == 0.0 {
        return Err(
            "set [nightlight] latitude/longitude in config.toml (or weather coordinates)".into(),
        );
    }
    let location = format!("{latitude}:{longitude}");
    let state = nightlight_state();
    let apply = |kelvin: u32| -> Result<()> {
        let before = gamma_signature();
        command(
            "gammastep",
            &[
                "-m".into(),
                "randr".into(),
                // gammastep needs the location to apply a manual temperature:
                // without it, it refuses with a GeoClue2 error. `-t K:K` forces
                // the same temperature day and night, which is what a manual
                // setting means; with `-O K` gammastep recomputes from the sun
                // and silently ignores the requested value during the day
                // (measured on a QXL display: gamma stayed at 1.0:1.0:1.0).
                "-l".into(),
                location.clone(),
                "-t".into(),
                format!("{kelvin}:{kelvin}"),
                "-o".into(),
            ],
        )?;
        if let (Some(before), Some(after)) = (before, gamma_signature()) {
            if before == after {
                let _ = std::fs::remove_file(&state);
                notify(
                    "Night light",
                    "this display did not accept the gamma change",
                    None,
                );
                return Err("the display did not accept the XRandR gamma change".into());
            }
        }
        if let Some(parent) = state.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&state, kelvin.to_string())?;
        notify("Night light", &format!("{kelvin} K"), None);
        Ok(())
    };
    let off = || -> Result<()> {
        command("gammastep", &["-x".into()])?;
        let _ = std::fs::remove_file(&state);
        notify("Night light", "off", None);
        Ok(())
    };
    let reconcile = || -> Result<()> {
        let clock = Local::now();
        let now = clock.hour() * 60 + clock.minute();
        let desired = match nl.mode.as_str() {
            "fixed" => {
                let on = clock_minutes(&nl.on).ok_or("nightlight.on must be HH:MM")?;
                let off = clock_minutes(&nl.off).ok_or("nightlight.off must be HH:MM")?;
                fixed_schedule_active(now, on, off).then_some(nl.temperature)
            }
            "solar" => Some(
                solar_temperature(&location, nl.day_temperature, nl.temperature)
                    .ok_or("gammastep could not compute the solar temperature")?,
            ),
            _ => None,
        };
        match desired {
            Some(kelvin) => {
                let current = std::fs::read_to_string(&state).ok();
                if current.as_deref() != Some(&kelvin.to_string()) {
                    apply(kelvin)?;
                }
            }
            None if state.exists() => off()?,
            _ => {}
        }
        Ok(())
    };
    match args.first().map(String::as_str).unwrap_or("status") {
        "on" => apply(nl.temperature)?,
        "off" => off()?,
        "toggle" => {
            if state.exists() {
                off()?
            } else {
                apply(nl.temperature)?
            }
        }
        "temperature" => {
            let kelvin: u32 = args
                .get(1)
                .ok_or("nightlight temperature needs Kelvin")?
                .parse()
                .map_err(|_| "nightlight temperature: not a number")?;
            if !(1000..=10000).contains(&kelvin) {
                return Err("temperature must be 1000..10000 K".into());
            }
            apply(kelvin)?;
        }
        "apply" => reconcile()?,
        "watch" => {
            if nl.mode == "manual" {
                println!("nightlight mode is manual; nothing to watch");
                return Ok(());
            }
            loop {
                if let Err(e) = reconcile() {
                    eprintln!("niructl: nightlight: {e}");
                }
                std::thread::sleep(Duration::from_secs(30));
            }
        }
        "status" => {
            let current = std::fs::read_to_string(&state).unwrap_or_else(|_| "off".into());
            println!("mode: {}", nl.mode);
            match nl.mode.as_str() {
                "fixed" => println!("schedule: {} - {}", nl.on, nl.off),
                "solar" => println!(
                    "solar: {location} ({} K day, {} K night)",
                    nl.day_temperature, nl.temperature
                ),
                _ => {}
            }
            println!("current: {current}");
        }
        _ => {
            return Err(
                "nightlight needs on, off, toggle, status, temperature K, apply or watch".into(),
            )
        }
    }
    Ok(())
}

/// Find a program in PATH or the selected NIRU prefix.
fn program(name: &str) -> Option<String> {
    if which(name) {
        return Some(name.to_string());
    }
    let local = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join(".local/bin")
        .join(name);
    local.is_file().then(|| local.to_string_lossy().to_string())
}

fn spawn_action(program: &str, args: &[&str]) -> Result<()> {
    std::process::Command::new(program)
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot start {program}: {e}").into())
}

/// A login/power action with an explicit backend and no shell.
fn login_action(action: &str) -> Result<()> {
    if action == "logout" && which("loginctl") {
        let mut args: Vec<String> = Vec::new();
        if let Ok(session) = std::env::var("XDG_SESSION_ID") {
            if !session.is_empty() {
                args.push("terminate-session".into());
                args.push(session);
            }
        }
        if args.is_empty() {
            let user = std::env::var("USER").map_err(|_| "USER is not set")?;
            args.push("terminate-user".into());
            args.push(user);
        }
        command("loginctl", &args)?;
        return Ok(());
    }
    let (method, verb) = match action {
        "shutdown" => ("PowerOff", "poweroff"),
        "reboot" => ("Reboot", "reboot"),
        "suspend" => ("Suspend", "suspend"),
        "logout" => return Err("no logout backend: install loginctl (logind/elogind)".into()),
        _ => return Err(format!("unknown login action: {action}").into()),
    };
    // systemd's loginctl has session commands, but NOT elogind's power verbs.
    // Both services implement the same login1 D-Bus methods. Request normal
    // interactive authorization and honor inhibitors; never retry a rejected
    // request through another backend (or bypass it with sudo/force).
    if which("dbus-send") {
        command(
            "dbus-send",
            &[
                "--system".into(),
                "--print-reply".into(),
                "--reply-timeout=120000".into(),
                "--dest=org.freedesktop.login1".into(),
                "/org/freedesktop/login1".into(),
                format!("org.freedesktop.login1.Manager.{method}"),
                "boolean:true".into(),
            ],
        )?;
        return Ok(());
    }
    if which("systemctl") {
        command("systemctl", &[verb.to_string()])?;
        return Ok(());
    }
    if which("loginctl") {
        let help = command("loginctl", &["--help".into()])?;
        if String::from_utf8_lossy(&help.stdout)
            .lines()
            .any(|line| line.split_whitespace().next() == Some(verb))
        {
            command("loginctl", &[verb.to_string()])?;
            return Ok(());
        }
    }
    Err(format!("no power backend for {action}: install dbus-send and logind/elogind").into())
}

/// How long the lock screen stays visible before the machine suspends.
const SUSPEND_DELAY: Duration = Duration::from_secs(3);

fn power_action(action: &str) -> Result<()> {
    let result = (|| -> Result<()> {
        if action == "suspend" {
            // A second request while the first one waits is coalesced, not
            // queued: holding the advisory lock is the whole mechanism.
            let _suspend_guard = match acquire_suspend_lock() {
                Ok(Some(guard)) => Some(guard),
                Ok(None) => return Ok(()),
                Err(e) => {
                    eprintln!("niructl: cannot create the suspend lock: {e}");
                    None
                }
            };
            let (c, root, lock_window) = ensure_lock_ready()?;
            std::thread::sleep(SUSPEND_DELAY);
            // Unlocking or a locker crash during the delay cancels this
            // request. A quick unlock-and-relock creates a new window, so it
            // cannot revive the old request either.
            if !lock_still_held(ready_lock(&c, root), lock_window) {
                return Err("cancelled: the screen was unlocked before suspend".into());
            }
        }
        login_action(action)
    })();
    if let Err(e) = &result {
        let text = e.to_string();
        let body = if let Some(reason) = text.strip_prefix("cancelled: ") {
            format!("Suspend cancelled: {reason}")
        } else {
            format!("{action} failed: {text}")
        };
        notify("Power", &body, None);
    }
    result
}

fn desktop(action: &str, args: &[String]) -> Result<()> {
    match action {
        "lock" => lock_action(),
        "screensaver" => {
            let saver = program("niruscreensaver").ok_or("niruscreensaver is not installed")?;
            spawn_action(&saver, &["--preview"])
        }
        "suspend" | "logout" | "reboot" | "shutdown" => power_action(action),
        "dnd" => dnd(args),
        _ => Err(USAGE.into()),
    }
}

fn idle_path() -> std::path::PathBuf {
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

/// Read the explicit idle-inhibition state. `Some(0)` never expires.
fn idle_until() -> Option<u64> {
    let text = std::fs::read_to_string(idle_path()).ok()?;
    let until = text.trim().parse::<u64>().ok()?;
    if until == 0 || until > now_secs() {
        Some(until)
    } else {
        None
    }
}

fn set_idle(until: Option<u64>) -> Result<()> {
    let path = idle_path();
    match until {
        Some(value) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, format!("{value}\n"))?;
        }
        None => match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        },
    }
    Ok(())
}

fn xset(args: &[&str]) {
    let _ = std::process::Command::new("xset")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn idle(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("status");
    let minutes = args.get(1).map(|v| v.parse::<u64>());
    let minutes = match minutes {
        Some(Ok(0)) => return Err("idle minutes must be at least 1".into()),
        Some(Ok(n)) if n > 24 * 60 => return Err("idle minutes must be at most 1440".into()),
        Some(Ok(n)) => Some(n),
        Some(Err(_)) => return Err("idle minutes: not a number".into()),
        None => None,
    };
    match action {
        "on" => {
            let until = minutes.map(|m| now_secs() + m * 60).unwrap_or(0);
            set_idle(Some(until))?;
            xset(&["s", "off", "-dpms"]);
            println!(
                "idle on{}",
                minutes.map(|m| format!(" for {m} min")).unwrap_or_default()
            );
        }
        "off" => {
            set_idle(None)?;
            xset(&["s", "default", "+dpms"]);
            println!("idle off");
        }
        "toggle" => {
            if idle_until().is_some() {
                set_idle(None)?;
                xset(&["s", "default", "+dpms"]);
                println!("idle off");
            } else {
                let until = minutes.map(|m| now_secs() + m * 60).unwrap_or(0);
                set_idle(Some(until))?;
                xset(&["s", "off", "-dpms"]);
                println!(
                    "idle on{}",
                    minutes.map(|m| format!(" for {m} min")).unwrap_or_default()
                );
            }
        }
        "status" => match idle_until() {
            Some(0) => println!("idle on (no expiry)"),
            Some(until) => {
                let left = until.saturating_sub(now_secs());
                println!("idle on, {} min remaining", left.div_ceil(60));
            }
            None => println!("idle off"),
        },
        _ => return Err("idle needs on, off, toggle or status".into()),
    }
    Ok(())
}

/// The effective idle, lock and display-off timers.
fn timing(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str).unwrap_or("show") {
        "show" => {
            let cfg = Config::load()?;
            println!("screensaver_timeout={}", cfg.idle.screensaver_timeout);
            println!("lock_after_screensaver={}", cfg.idle.lock_after_screensaver);
            println!("dpms_off_after_lock={}", cfg.idle.dpms_off_after_lock);
            Ok(())
        }
        "set" => {
            let key = args
                .get(1)
                .map(String::as_str)
                .ok_or("timing set needs a key and minutes")?;
            let raw = args
                .get(2)
                .map(String::as_str)
                .ok_or("timing set needs a key and minutes")?;
            let minutes: u32 = raw
                .parse()
                .map_err(|_| "timing minutes: not a number".to_string())?;
            let min = match key {
                "screensaver_timeout" => 1,
                "lock_after_screensaver" | "dpms_off_after_lock" => 0,
                _ => {
                    return Err("timing key must be screensaver_timeout, \
                                lock_after_screensaver or dpms_off_after_lock"
                        .into())
                }
            };
            if !(min..=crate::config::IdleConfig::MAX_MINUTES).contains(&minutes) {
                return Err(format!(
                    "{key} must be between {min} and {} minutes",
                    crate::config::IdleConfig::MAX_MINUTES
                )
                .into());
            }
            write_timing(key, minutes)?;
            println!("{key}={minutes}");
            Ok(())
        }
        _ => Err("timing needs show or set KEY MINUTES".into()),
    }
}

/// Write one `[idle]` value without disturbing comments or unrelated keys.
fn write_timing(key: &str, minutes: u32) -> Result<()> {
    let path = config::directory().join("config.toml");
    let original = fs::read_to_string(&path).unwrap_or_default();
    let rendered = updated_timing(&original, key, minutes)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !original.is_empty() {
        let _ = fs::write(path.with_extension("toml.bak"), &original);
    }
    let temp = path.with_file_name(format!(".config.toml.{}", std::process::id()));
    fs::write(&temp, rendered)?;
    if let Ok(metadata) = fs::metadata(&path) {
        let _ = fs::set_permissions(&temp, metadata.permissions());
    }
    fs::rename(&temp, &path)?;
    Ok(())
}

/// Apply one timing value to the file text and validate the whole result
/// before anything is written.
fn updated_timing(text: &str, key: &str, minutes: u32) -> Result<String> {
    let mut document: toml_edit::DocumentMut = text.parse()?;
    let has_table = document
        .get("idle")
        .map(|item| item.is_table())
        .unwrap_or(false);
    if !has_table {
        if document.get("idle").is_some() {
            return Err("idle must be a [idle] table".into());
        }
        document["idle"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    document["idle"][key] = toml_edit::value(i64::from(minutes));
    let rendered = document.to_string();
    // Validate the whole file before replacing the working one. A typo in an
    // unrelated section must not turn this into a partial write.
    let candidate: Config = toml::from_str(&rendered)?;
    candidate.validate()?;
    Ok(rendered)
}

/// Lock and, for NIRULOCK, wait for verified readiness before returning.
///
/// `slock` has no readiness contract, so it is still usable interactively but
/// cannot be used as proof that a suspend is safe.
fn lock_action() -> Result<()> {
    let (c, s) = x11rb::connect(None)?;
    let root = c.setup().roots[s].root;
    if ready_lock(&c, root).is_some() {
        return Ok(());
    }
    if let Some(locker) = program("nirulock") {
        spawn_action(&locker, &[])?;
        wait_ready_lock(&c, root)?;
        return Ok(());
    }
    if let Some(locker) = program("slock") {
        return spawn_action(&locker, &[]);
    }
    Err("no screen locker found: install nirulock or slock".into())
}

/// The lock window that currently owns the readiness marker, if any.
///
/// A killed locker can leave the property on the root window. Its X window
/// disappears with the connection, so the window is part of the check.
fn ready_lock(c: &x11rb::rust_connection::RustConnection, root: Window) -> Option<Window> {
    let atom = c
        .intern_atom(false, b"_NIRU_LOCK_READY")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let window = c
        .get_property(false, root, atom, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()?;
    let attributes = c.get_window_attributes(window).ok()?.reply().ok()?;
    (attributes.map_state == MapState::VIEWABLE).then_some(window)
}

fn wait_ready_lock(c: &x11rb::rust_connection::RustConnection, root: Window) -> Result<Window> {
    for _ in 0..250 {
        if let Some(window) = ready_lock(c, root) {
            return Ok(window);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err("screen locker did not publish readiness; not suspending".into())
}

/// The verified lock window a suspend has to keep seeing.
///
/// The second window is the locker's X window; unlocking destroys it, so a
/// comparison after the safety delay also catches an unlock and a quick
/// relock that happened in between.
fn ensure_lock_ready() -> Result<(x11rb::rust_connection::RustConnection, Window, Window)> {
    let (c, s) = x11rb::connect(None)?;
    let root = c.setup().roots[s].root;
    if let Some(window) = ready_lock(&c, root) {
        return Ok((c, root, window));
    }
    let locker = program("nirulock")
        .ok_or("cannot lock before suspend: install nirulock (slock cannot confirm readiness)")?;
    spawn_action(&locker, &[])?;
    let window = wait_ready_lock(&c, root)?;
    Ok((c, root, window))
}

/// One suspend request at a time. The file is only a handle: the kernel
/// releases the advisory lock when this process exits, so a crash cannot leave
/// a permanent "busy" marker behind.
fn acquire_suspend_lock() -> std::io::Result<Option<fs::File>> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache")
        });
    fs::create_dir_all(&base)?;
    acquire_lock_at(&base.join("niruwm-suspend.lock"))
}

/// Take a non-blocking exclusive advisory lock. A second open file description
/// fails at once, which is what makes repeated suspend presses coalesce.
fn acquire_lock_at(path: &Path) -> std::io::Result<Option<fs::File>> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    use std::os::unix::io::AsRawFd;
    let acquired = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    Ok(acquired.then_some(file))
}

/// The lock is still safe to suspend under only while the same lock window is
/// mapped. A different window means the screen was unlocked and relocked.
fn lock_still_held(ready: Option<Window>, expected: Window) -> bool {
    ready == Some(expected)
}

/// Do-not-disturb through the active provider's documented control.
///
/// The native notifier will take this over when it owns the bus; until then
/// Dunst's own control interface is the honest provider.
fn dnd(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("status");
    if !matches!(action, "on" | "off" | "toggle" | "status") {
        return Err("dnd needs on, off, toggle or status".into());
    }

    // Prefer the running native provider. Its CLI is a socket client, so a
    // missing daemon fails quickly and Dunst remains the fallback.
    if let Some(notifier) = program("nirunotify") {
        if let Ok(out) = command(&notifier, &["--dnd".into(), action.into()]) {
            let text = String::from_utf8_lossy(&out.stdout);
            let text = text.trim();
            if !text.is_empty() {
                println!("{text}");
            }
            return Ok(());
        }
    }

    if !which("dunstctl") {
        return Err(
            "DND is unavailable: neither a running nirunotify nor dunstctl is available".into(),
        );
    }
    let paused = || -> Result<bool> {
        let out = command("dunstctl", &["is-paused".into()])?;
        Ok(String::from_utf8_lossy(&out.stdout).trim() == "true")
    };
    match action {
        "on" => {
            command("dunstctl", &["set-paused".into(), "true".into()])?;
            println!("dnd on");
        }
        "off" => {
            command("dunstctl", &["set-paused".into(), "false".into()])?;
            println!("dnd off");
        }
        "toggle" => {
            let next = if paused()? { "false" } else { "true" };
            command("dunstctl", &["set-paused".into(), next.into()])?;
            println!("dnd {}", if next == "true" { "on" } else { "off" });
        }
        "status" => println!("dnd {}", if paused()? { "on" } else { "off" }),
        _ => unreachable!(),
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AudioNode {
    id: u32,
    name: String,
    default: bool,
}

/// Parse the Sinks/Sources part of `wpctl status` without depending on the
/// tree drawing characters. IDs, not names, are used for selection because
/// two identical devices can coexist.
fn audio_nodes_from(text: &str, wanted: &str) -> Vec<AudioNode> {
    let mut domain = "";
    let mut section = "";
    let mut nodes = Vec::new();
    for line in text.lines() {
        let clean = line.trim_matches(|c: char| c.is_whitespace() || "│├└─".contains(c));
        if clean == "Audio" || clean == "Video" {
            domain = clean;
            section = "";
            continue;
        }
        if clean == "Sinks:" {
            section = "outputs";
            continue;
        }
        if clean == "Sources:" {
            section = "sources";
            continue;
        }
        if clean.ends_with(':') || clean == "Filters:" || clean == "Streams:" {
            section = "";
            continue;
        }
        if domain != "Audio" || section != wanted {
            continue;
        }
        let default = clean.starts_with('*');
        let clean = clean.trim_start_matches('*').trim();
        let Some((id, rest)) = clean.split_once(". ") else {
            continue;
        };
        let Ok(id) = id.trim().parse::<u32>() else {
            continue;
        };
        let name = rest.split(" [").next().unwrap_or(rest).trim();
        if !name.is_empty() {
            nodes.push(AudioNode {
                id,
                name: name.to_string(),
                default,
            });
        }
    }
    nodes
}

/// Audio queries must not hold the launcher or a status worker indefinitely.
fn provider_query(program: &str, args: &[String]) -> Result<String> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .env("NO_COLOR", "1")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("audio stdout unavailable")?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(65_537).read_to_end(&mut bytes);
        let _ = sender.send((result, bytes));
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            // Stop descendants too: they can otherwise keep stdout open after
            // their leader exits, defeating the query deadline.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let response = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    if response.is_err() {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.wait();
    }
    reader.join().map_err(|_| "audio reader stopped")?;
    let (result, bytes) = response.map_err(|_| "provider timed out")?;
    result?;
    let status = status.ok_or("provider timed out")?;
    if !status.success() {
        return Err("provider rejected the request or is unavailable".into());
    }
    if bytes.len() > 65_536 {
        return Err("audio provider output is too large".into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn audio_nodes(kind: &str) -> Result<Vec<AudioNode>> {
    let wpctl = program("wpctl").ok_or("audio control needs wpctl (WirePlumber)")?;
    let text = provider_query(&wpctl, &["status".into()])?;
    Ok(audio_nodes_from(&text, kind))
}

#[derive(Debug)]
struct MicrophoneReading {
    id: u32,
    name: String,
    percent: u32,
    muted: bool,
}

fn microphone_reading() -> Result<Option<MicrophoneReading>> {
    let sources = audio_nodes("sources")?;
    if sources.is_empty() {
        return Ok(None);
    }
    let node = sources
        .into_iter()
        .find(|node| node.default)
        .ok_or("no default audio input is selected")?;
    let wpctl = program("wpctl").ok_or("audio control needs wpctl (WirePlumber)")?;
    let text = provider_query(&wpctl, &["get-volume".into(), node.id.to_string()])?;
    let value = text
        .strip_prefix("Volume:")
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= 10_000.0)
        .ok_or("audio input level is invalid")?;
    Ok(Some(MicrophoneReading {
        id: node.id,
        name: node.name.replace(['\t', '\r', '\n'], " "),
        percent: (value * 100.0).round() as u32,
        muted: text.contains("[MUTED]"),
    }))
}

fn print_microphone(reading: &MicrophoneReading) {
    println!(
        "state=ready\nid={}\nname={}\npercent={}\nmuted={}",
        reading.id, reading.name, reading.percent, reading.muted
    );
}

fn microphone_status() {
    if program("wpctl").is_none() {
        println!("state=provider-unavailable\nreason=wpctl (WirePlumber) is not installed");
        return;
    }
    match microphone_reading() {
        Ok(Some(reading)) => print_microphone(&reading),
        Ok(None) => println!("state=no-input\nreason=no audio input is available"),
        Err(error) => println!("state=unavailable\nreason={error}"),
    }
}

fn audio_default(kind: &str) -> Result<Option<u32>> {
    Ok(audio_nodes(kind)?
        .into_iter()
        .find(|node| node.default)
        .map(|node| node.id))
}

fn audio_set_default(kind: &str, id: u32) -> Result<()> {
    let label = kind.trim_end_matches('s');
    if !audio_nodes(kind)?.iter().any(|node| node.id == id) {
        return Err(format!("audio {label} {id} is not an available audio device").into());
    }
    let wpctl = program("wpctl").ok_or("audio control needs wpctl (WirePlumber)")?;
    command(&wpctl, &["set-default".into(), id.to_string()])?;
    std::thread::sleep(Duration::from_millis(150));
    if audio_default(kind)? != Some(id) {
        return Err(format!(
            "audio {label} {id} did not become the default; the device may be unavailable"
        )
        .into());
    }
    println!("default {label}: {id}");
    Ok(())
}

fn audio_next(kind: &str) -> Result<()> {
    let label = kind.trim_end_matches('s');
    let nodes = audio_nodes(kind)?;
    if nodes.is_empty() {
        return Err(format!("no audio {label} is available").into());
    }
    if nodes.len() == 1 {
        println!("only one audio {label} is available");
        return Ok(());
    }
    let current = nodes.iter().position(|node| node.default).unwrap_or(0);
    for step in 1..nodes.len() {
        let id = nodes[(current + step) % nodes.len()].id;
        let wpctl = program("wpctl").ok_or("audio control needs wpctl (WirePlumber)")?;
        command(&wpctl, &["set-default".into(), id.to_string()])?;
        std::thread::sleep(Duration::from_millis(150));
        if audio_default(kind)? == Some(id) {
            println!("default {label}: {id}");
            return Ok(());
        }
    }
    Err(format!("no other audio {label} could become the default").into())
}

fn audio(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("outputs");
    let wpctl = || program("wpctl").ok_or("audio control needs wpctl (WirePlumber)");
    match action {
        "outputs" | "sources" => {
            let kind = action;
            for node in audio_nodes(kind)? {
                println!(
                    "{}{}\t{}",
                    if node.default { "*" } else { "" },
                    node.id,
                    node.name
                );
            }
        }
        "mic-status" => microphone_status(),
        "mic-level" => {
            // Adjust input gain only: changing gain must never unmute a mic.
            let direction = args.get(1).map(String::as_str).ok_or(USAGE)?;
            let value = match direction {
                "up" => "5%+".to_string(),
                "down" => "5%-".to_string(),
                "set" => {
                    let percent = args
                        .get(2)
                        .ok_or("mic-level set needs a percentage")?
                        .parse::<u32>()
                        .map_err(|_| "microphone level must be an integer 0..100")?;
                    if percent > 100 {
                        return Err("microphone level must be between 0 and 100".into());
                    }
                    format!("{percent}%")
                }
                _ => return Err("mic-level needs up, down or set N".into()),
            };
            let reading = microphone_reading()?.ok_or("no audio input is available")?;
            provider_query(
                &wpctl()?,
                &[
                    "set-volume".into(),
                    "--limit".into(),
                    "1.0".into(),
                    reading.id.to_string(),
                    value,
                ],
            )?;
        }
        "output" | "source" => {
            let kind = if action == "output" {
                "outputs"
            } else {
                "sources"
            };
            let value = args.get(1).ok_or(USAGE)?;
            if value == "next" {
                audio_next(kind)?;
            } else {
                let id = value
                    .parse::<u32>()
                    .map_err(|_| "audio id must be a number or next")?;
                audio_set_default(kind, id)?;
            }
        }
        "mic-mute" => {
            let value = match args.get(1).map(String::as_str).unwrap_or("toggle") {
                "on" => "1",
                "off" => "0",
                "toggle" => "toggle",
                _ => return Err("mic-mute needs on, off or toggle".into()),
            };
            let reading = microphone_reading()?.ok_or("no audio input is available")?;
            provider_query(
                &wpctl()?,
                &["set-mute".into(), reading.id.to_string(), value.into()],
            )?;
        }
        "mute" => {
            command(
                &wpctl()?,
                &[
                    "set-mute".into(),
                    "@DEFAULT_AUDIO_SINK@".into(),
                    "toggle".into(),
                ],
            )?;
        }
        "volume" => {
            let direction = args.get(1).map(String::as_str).ok_or(USAGE)?;
            let arg = match direction {
                "up" => "5%+",
                "down" => "5%-",
                _ => return Err("audio volume needs up or down".into()),
            };
            command(
                &wpctl()?,
                &[
                    "set-volume".into(),
                    "@DEFAULT_AUDIO_SINK@".into(),
                    arg.into(),
                ],
            )?;
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BluetoothDevice {
    mac: String,
    name: String,
    connected: bool,
    battery: Option<u32>,
}

#[derive(Clone, Debug)]
struct BluetoothState {
    adapter: Option<String>,
    powered: bool,
    devices: Vec<BluetoothDevice>,
}

fn bluetoothctl(args: &[&str]) -> Result<String> {
    let program =
        program("bluetoothctl").ok_or("Bluetooth control needs bluetoothctl (bluez-utils)")?;
    provider_query(
        &program,
        &args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>(),
    )
}

fn parse_bluetooth_show(text: &str) -> (Option<String>, bool) {
    let mut adapter = None;
    let mut powered = false;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Controller ") {
            adapter = rest.split_whitespace().next().map(str::to_string);
        }
        if line.trim() == "Powered: yes" {
            powered = true;
        }
    }
    (adapter, powered)
}

fn parse_bluetooth_devices(text: &str) -> Vec<(String, String)> {
    let mut devices = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("Device ") else {
            continue;
        };
        let mut words = rest.splitn(2, ' ');
        let Some(mac) = words.next().filter(|mac| !mac.is_empty()) else {
            continue;
        };
        let name = words.next().unwrap_or(mac).trim();
        devices.push((mac.to_string(), name.to_string()));
    }
    devices
}

fn parse_bluetooth_info(mac: String, name: String, text: &str) -> BluetoothDevice {
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.trim_start().strip_prefix(key))
            .map(str::trim)
    };
    let connected = field("Connected:").is_some_and(|value| value == "yes");
    let battery = text.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix("Battery Percentage:")?;
        rest.split_once('(')
            .and_then(|(_, value)| value.trim_end_matches(')').trim().parse::<u32>().ok())
            .or_else(|| {
                rest.split_whitespace()
                    .next()
                    .and_then(|hex| u32::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
            })
    });
    BluetoothDevice {
        mac,
        name,
        connected,
        battery,
    }
}

fn bluetooth_state() -> Result<BluetoothState> {
    let show = bluetoothctl(&["show"])?;
    let (adapter, powered) = parse_bluetooth_show(&show);
    if adapter.is_none() {
        return Ok(BluetoothState {
            adapter: None,
            powered: false,
            devices: Vec::new(),
        });
    }
    let mut devices = parse_bluetooth_devices(&bluetoothctl(&["devices", "Paired"])?);
    if devices.is_empty() {
        devices = parse_bluetooth_devices(&bluetoothctl(&["devices"])?);
    }
    let mut paired = Vec::new();
    for (mac, name) in devices.into_iter().take(32) {
        let info = match bluetoothctl(&["info", &mac]) {
            Ok(info) => info,
            Err(_) => continue,
        };
        if !info.lines().any(|line| line.trim() == "Paired: yes") {
            continue;
        }
        paired.push(parse_bluetooth_info(mac, name, &info));
    }
    Ok(BluetoothState {
        adapter,
        powered,
        devices: paired,
    })
}

fn bluetooth_status() {
    if program("bluetoothctl").is_none() {
        println!("state=provider-unavailable\nreason=bluetoothctl (bluez-utils) is not installed");
        return;
    }
    match bluetooth_state() {
        Ok(state) => {
            println!(
                "state=ready\nadapter={}\npowered={}",
                state.adapter.unwrap_or_default(),
                state.powered
            );
            for device in state.devices {
                println!(
                    "device={}\t{}\t{}\t{}",
                    device.mac,
                    device.name.replace(['\t', '\r', '\n'], " "),
                    device.connected,
                    device.battery.map(|b| b.to_string()).unwrap_or_default()
                );
            }
        }
        Err(error) => println!("state=unavailable\nreason={error}"),
    }
}

fn bluetooth(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("status");
    match action {
        "status" => bluetooth_status(),
        "list" => {
            for device in bluetooth_state()?.devices {
                println!(
                    "{}\t{}\t{}\t{}",
                    device.mac,
                    device.name,
                    device.connected,
                    device.battery.map(|b| b.to_string()).unwrap_or_default()
                );
            }
        }
        "power" => {
            let state = bluetooth_state()?;
            if state.adapter.is_none() {
                return Err("no Bluetooth adapter is available".into());
            }
            let target = match args.get(1).map(String::as_str).unwrap_or("toggle") {
                "on" => true,
                "off" => false,
                "toggle" => !state.powered,
                _ => return Err("bluetooth power needs on, off or toggle".into()),
            };
            if target != state.powered {
                bluetoothctl(&["power", if target { "on" } else { "off" }])?;
            }
            println!("bluetooth power {}", if target { "on" } else { "off" });
        }
        "connect" | "disconnect" => {
            let mac = args.get(1).ok_or("bluetooth needs a device MAC")?;
            if !mac.contains(':') {
                return Err("bluetooth device must be a MAC address".into());
            }
            let state = bluetooth_state()?;
            if !state.devices.iter().any(|device| device.mac == *mac) {
                return Err(format!("Bluetooth device {mac} is not paired").into());
            }
            bluetoothctl(&[action, mac])?;
            std::thread::sleep(Duration::from_millis(300));
            let connected = bluetooth_state()?
                .devices
                .iter()
                .any(|device| device.mac == *mac && device.connected);
            if action == "connect" && !connected {
                return Err(format!("Bluetooth device {mac} did not report connected").into());
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct DisplayOutput {
    name: String,
    #[serde(default = "display_preferred_mode")]
    mode: String,
    /// Refresh rate in Hz. Older profiles have none, and then xrandr's own
    /// choice for the mode is kept instead of silently changing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate: Option<f64>,
    #[serde(default = "display_zero_position")]
    position: String,
    #[serde(default = "display_normal_rotation")]
    rotation: String,
    #[serde(default)]
    primary: bool,
}

fn display_preferred_mode() -> String {
    "preferred".into()
}
const DISPLAY_ROTATE_USAGE: &str =
    "display rotate needs an output and normal|left|right|inverted: niructl display rotate HDMI-1 left";

/// The width a screen takes on the desktop at the given mode, before rotation.
/// The width and height a mode gives a screen, before any rotation.
fn display_mode_size(mode: &str, state: &DisplayOutputState) -> Option<(i32, i32)> {
    if let Some((width, rest)) = mode.split_once('x') {
        if let (Ok(width), Some(height)) = (
            width.parse::<i32>(),
            rest.split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|h| h.parse::<i32>().ok()),
        ) {
            if width > 0 && height > 0 {
                return Some((width, height));
            }
        }
    }
    // "preferred" resolved earlier; anything left is a mode named like the
    // current geometry, which the unparsed line carries.
    let line = state.line.as_deref()?;
    let geometry = line.split_whitespace().find(|word| {
        word.split_once('x')
            .is_some_and(|(w, _)| w.parse::<i32>().is_ok())
    })?;
    let (width, rest) = geometry.split_once('x')?;
    let height = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|h| h.parse::<i32>().ok())?;
    Some((width.parse().ok()?, height))
}

fn display_rotate(name: &str, rotation: &str) -> Result<()> {
    if !matches!(rotation, "normal" | "left" | "right" | "inverted") {
        return Err(DISPLAY_ROTATE_USAGE.into());
    }
    let outputs = display_outputs()?;
    let Some(state) = outputs.iter().find(|state| state.name == name) else {
        return Err(format!("no display output named {name}").into());
    };
    if state.current.is_none() {
        return Err(format!("{name} is not connected; connect it and try again").into());
    }
    let previous = display_current_layout(&outputs);
    let mut layout = previous.clone();
    let Some(output) = layout.iter_mut().find(|output| output.name == name) else {
        return Err(format!("{name} is not active").into());
    };
    output.rotation = rotation.to_string();
    let profile = DisplayProfile {
        name: format!("rotate {name}"),
        outputs: layout,
    };
    if let Err(error) = display_apply(&outputs, &profile) {
        let _ = display_restore(&previous);
        return Err(error);
    }
    display_notify("Display rotated", &format!("{name}: {rotation}"));
    println!("rotated {name} to {rotation}");
    Ok(())
}
fn display_zero_position() -> String {
    "0x0".into()
}
fn display_normal_rotation() -> String {
    "normal".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct DisplayProfile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    outputs: Vec<DisplayOutput>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct DisplayFile {
    #[serde(default)]
    profile: Vec<DisplayProfile>,
    /// Opt-in profile applied once at login by niruwm-session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    startup_profile: Option<String>,
}

#[derive(Clone, Debug)]
struct DisplayMode {
    mode: String,
    rates: Vec<f64>,
    preferred: bool,
}

#[derive(Clone, Debug)]
struct DisplayOutputState {
    name: String,
    connected: bool,
    modes: Vec<DisplayMode>,
    /// The mode xrandr marks preferred, when it names one.
    preferred: Option<String>,
    current: Option<DisplayOutput>,
    /// The unparsed xrandr line, for the physical size after the mode name.
    line: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DisplayPreview {
    name: String,
    token: String,
    expiry: u64,
    previous: Vec<DisplayOutput>,
}

fn display_profiles_path() -> PathBuf {
    config::directory().join("displays.toml")
}
fn display_preview_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
    base.join("niruwm/display-preview.toml")
}

fn display_transaction_lock() -> Result<std::fs::File> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let path = display_preview_path().with_extension("lock");
    std::fs::create_dir_all(path.parent().ok_or("missing display state directory")?)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err("could not lock display transaction".into());
    }
    Ok(file)
}

fn display_outputs() -> Result<Vec<DisplayOutputState>> {
    let out = command("xrandr", &["--query".into()])?;
    Ok(display_outputs_from(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse `xrandr --query`. Separate from the query so a fixture can exercise
/// the rotation, preferred mode and switched-off screen rules without X.
fn display_outputs_from(text: &str) -> Vec<DisplayOutputState> {
    let mut outputs: Vec<DisplayOutputState> = Vec::new();
    for line in text.lines() {
        if !line.starts_with(|c: char| !c.is_whitespace()) {
            if let Some(output) = outputs.last_mut() {
                let mut words = line.split_whitespace();
                if let Some(mode) = words.next() {
                    if !mode.is_empty() && mode.contains('x') {
                        let rate_words: Vec<&str> = words.collect();
                        let rates: Vec<f64> = rate_words
                            .iter()
                            .filter_map(|word| {
                                word.trim_end_matches(['*', '+']).parse::<f64>().ok()
                            })
                            .collect();
                        // `+` marks the preferred rate; `*` marks the one in
                        // use. The current mode's geometry is on the output
                        // line above, while its rate is on this line.
                        let preferred = rate_words.iter().any(|word| word.contains('+'));
                        let current_rate = rate_words
                            .iter()
                            .find(|word| word.contains('*'))
                            .and_then(|word| word.trim_end_matches(['*', '+']).parse::<f64>().ok());
                        if preferred && output.preferred.is_none() {
                            output.preferred = Some(mode.to_string());
                        }
                        if let (Some(rate), Some(current)) = (current_rate, output.current.as_mut())
                        {
                            current.mode = mode.to_string();
                            current.rate = Some(rate);
                        }
                        output.modes.push(DisplayMode {
                            mode: mode.to_string(),
                            rates,
                            preferred,
                        });
                    }
                }
            }
            continue;
        }
        let mut words = line.split_whitespace();
        let Some(name) = words.next() else { continue };
        let connected = line.contains(" connected");
        let disconnected = line.contains(" disconnected");
        if !connected && !disconnected {
            continue;
        }
        let geometry = line
            .split_whitespace()
            .find(|word| word.contains('x') && word.contains('+') && !word.starts_with('('));
        let primary = line.contains(" primary ");
        // The current rotation follows the geometry, for example
        // `DP-2 connected 2560x1440+2560+0 inverted (normal left inverted ...)`.
        // The words inside the parentheses are the rotations the output
        // supports, in that order, so they can never say what it is set to.
        let rotation = line
            .split('(')
            .next()
            .unwrap_or(line)
            .split_whitespace()
            .find(|word| matches!(*word, "normal" | "left" | "inverted" | "right"))
            .unwrap_or("normal")
            .to_string();
        let current = geometry.map(|geometry| {
            let mode = geometry.split('+').next().unwrap_or(geometry);
            DisplayOutput {
                name: name.to_string(),
                mode: mode.to_string(),
                rate: None,
                position: geometry
                    .split_once('+')
                    .and_then(|(_, rest)| rest.split_once('+'))
                    .map(|(x, y)| format!("{x}x{y}"))
                    .unwrap_or_else(display_zero_position),
                rotation,
                primary,
            }
        });
        outputs.push(DisplayOutputState {
            name: name.to_string(),
            connected,
            modes: Vec::new(),
            preferred: None,
            current,
            line: Some(line.to_string()),
        });
    }
    outputs
}

fn display_resolve(
    outputs: &[DisplayOutputState],
    output: &DisplayOutput,
) -> Result<DisplayOutput> {
    let state = outputs
        .iter()
        .find(|state| state.name == output.name)
        .ok_or_else(|| format!("display output {} is not connected", output.name))?;
    // A connected output with no current mode is simply switched off, as a
    // freshly plugged screen is. Applying a profile must still turn it on.
    if !state.connected || state.modes.is_empty() {
        return Err(format!("display output {} is not connected", output.name).into());
    }
    let mode = if output.mode == "preferred" || output.mode.is_empty() {
        state
            .preferred
            .clone()
            .or_else(|| state.current.as_ref().map(|current| current.mode.clone()))
            .or_else(|| state.modes.first().map(|mode| mode.mode.clone()))
            .ok_or_else(|| format!("display output {} has no modes", output.name))?
    } else if state.modes.iter().any(|mode| mode.mode == output.mode) {
        output.mode.clone()
    } else {
        return Err(format!(
            "display mode {} is not available on {}",
            output.mode, output.name
        )
        .into());
    };
    let available = state
        .modes
        .iter()
        .find(|available| available.mode == mode)
        .map(|available| available.rates.as_slice())
        .unwrap_or(&[]);
    let rate = match output.rate {
        Some(rate) => {
            if !available.is_empty()
                && !available
                    .iter()
                    .any(|available| (available - rate).abs() < 0.01)
            {
                return Err(format!(
                    "display rate {rate} is not available on {} mode {mode}",
                    output.name
                )
                .into());
            }
            Some(rate)
        }
        None => None,
    };
    if !matches!(
        output.rotation.as_str(),
        "normal" | "left" | "inverted" | "right"
    ) {
        return Err(format!("invalid rotation {} for {}", output.rotation, output.name).into());
    }
    let position = output.position.trim_start_matches('+');
    let Some((x, y)) = position.split_once('x') else {
        return Err(format!("display position {} must be WxH", output.position).into());
    };
    if x.parse::<i32>().is_err() || y.parse::<i32>().is_err() {
        return Err(format!("display position {} must be WxH", output.position).into());
    }
    Ok(DisplayOutput {
        mode,
        rate,
        position: format!("{x}x{y}"),
        ..output.clone()
    })
}

fn display_apply(outputs: &[DisplayOutputState], profile: &DisplayProfile) -> Result<()> {
    if profile.outputs.is_empty() {
        return Err("display profile has no outputs".into());
    }
    let mut args = Vec::new();
    let mut names = std::collections::HashSet::new();
    for output in &profile.outputs {
        let resolved = display_resolve(outputs, output)?;
        if !names.insert(resolved.name.clone()) {
            return Err(format!("display output {} is listed twice", resolved.name).into());
        }
        args.extend([
            "--output".to_string(),
            resolved.name.clone(),
            "--mode".to_string(),
            resolved.mode,
            "--pos".to_string(),
            resolved.position,
            "--rotate".to_string(),
            resolved.rotation,
        ]);
        if let Some(rate) = resolved.rate {
            args.push("--rate".to_string());
            args.push(format!("{rate:.2}"));
        }
        if resolved.primary {
            args.push("--primary".into());
        }
    }
    for state in outputs {
        if state.connected && state.current.is_some() && !names.contains(&state.name) {
            args.extend([
                "--output".to_string(),
                state.name.clone(),
                "--off".to_string(),
            ]);
        }
    }
    let mut bounded = vec!["5".to_string(), "xrandr".to_string()];
    bounded.extend(args);
    command("timeout", &bounded)?;
    Ok(())
}

fn display_current_layout(outputs: &[DisplayOutputState]) -> Vec<DisplayOutput> {
    outputs
        .iter()
        .filter_map(|state| state.current.clone())
        .collect()
}

fn display_read_file() -> Result<DisplayFile> {
    let path = display_profiles_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(toml::from_str(&text)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DisplayFile::default()),
        Err(e) => Err(e.into()),
    }
}

fn display_write_file(file: &DisplayFile) -> Result<()> {
    let path = display_profiles_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("new");
    std::fs::write(&temp, toml::to_string_pretty(file)?)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

fn display_preview_read() -> Option<DisplayPreview> {
    toml::from_str(&std::fs::read_to_string(display_preview_path()).ok()?).ok()
}

fn display_preview_write(state: &DisplayPreview) -> Result<()> {
    let path = display_preview_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("new");
    std::fs::write(&temp, toml::to_string(state)?)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

fn display_restore(previous: &[DisplayOutput]) -> Result<()> {
    let outputs = display_outputs()?;
    let profile = DisplayProfile {
        name: "previous".into(),
        outputs: previous.to_vec(),
    };
    display_apply(&outputs, &profile)
}

fn display_notify(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "NIRUX", "-t", "6000", summary, body])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn display_startup(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("show");
    match action {
        "show" => match display_read_file()?.startup_profile {
            Some(name) => println!("{name}"),
            None => println!("off"),
        },
        "set" => {
            let name = args
                .get(1)
                .ok_or("display startup set needs a profile name")?;
            let mut file = display_read_file()?;
            if !file.profile.iter().any(|profile| profile.name == *name) {
                return Err(format!("unknown display profile: {name}").into());
            }
            file.startup_profile = Some(name.clone());
            display_write_file(&file)?;
            println!("startup display profile set to {name}");
        }
        "off" | "none" => {
            let mut file = display_read_file()?;
            file.startup_profile = None;
            display_write_file(&file)?;
            println!("startup display profile disabled");
        }
        "apply" => {
            let Some(name) = display_read_file()?.startup_profile else {
                return Ok(());
            };
            let profile = display_read_file()?
                .profile
                .into_iter()
                .find(|profile| profile.name == name)
                .ok_or_else(|| format!("startup display profile not found: {name}"))?;
            let outputs = display_outputs()?;
            let previous = display_current_layout(&outputs);
            if let Err(error) = display_apply(&outputs, &profile) {
                if !previous.is_empty() {
                    let _ = display_restore(&previous);
                }
                return Err(error);
            }
            println!("applied startup display profile {name}");
        }
        _ => return Err("display startup needs show, set NAME, off or apply".into()),
    }
    Ok(())
}

fn display_position_pair(position: &str) -> Option<(i32, i32)> {
    let position = position.trim_start_matches('+');
    let (x, y) = position.split_once('x')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

fn parse_edid(bytes: &[u8]) -> Option<serde_json::Value> {
    if bytes.len() < 128 || bytes.get(0..8)? != [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00] {
        return None;
    }
    let manufacturer_id = (u16::from(bytes[8]) << 8) | u16::from(bytes[9]);
    let manufacturer: String = [10u16, 5u16, 0u16]
        .iter()
        .map(|shift| char::from(b'A' - 1 + (((manufacturer_id >> *shift) & 0x1f) as u8)))
        .collect();
    let product_code = u16::from_le_bytes([bytes[10], bytes[11]]);
    let serial_number = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    let week = bytes[16];
    let year = 1990 + u16::from(bytes[17]);
    let mut model = None;
    let mut serial_text = None;
    for block in bytes.get(54..)?.chunks_exact(18) {
        if block[0] != 0 || block[1] != 0 || block[2] != 0 {
            continue;
        }
        let text = String::from_utf8_lossy(&block[5..18]);
        let text = text.trim_matches(|c: char| c == '\n' || c == '\r' || c == ' ' || c == '\0');
        if text.is_empty() {
            continue;
        }
        match block[3] {
            0xfc if model.is_none() => model = Some(text.to_string()),
            0xff if serial_text.is_none() => serial_text = Some(text.to_string()),
            _ => {}
        }
    }
    Some(serde_json::json!({
        "manufacturer": manufacturer,
        "model": model,
        "product_code": product_code,
        "serial": serial_text.or_else(|| (serial_number != 0).then(|| serial_number.to_string())),
        "week": week,
        "year": year,
    }))
}

fn display_edid_identities_from(text: &str) -> HashMap<String, serde_json::Value> {
    let mut identities = HashMap::new();
    let mut current: Option<(String, Vec<u8>)> = None;
    let mut collecting = false;
    for line in text.lines() {
        if !line.starts_with(|c: char| c.is_whitespace()) {
            if let Some((name, edid)) = current.take() {
                if let Some(identity) = parse_edid(&edid) {
                    identities.insert(name, identity);
                }
            }
            collecting = false;
            if line.contains(" connected") || line.contains(" disconnected") {
                if let Some(name) = line.split_whitespace().next() {
                    current = Some((name.to_string(), Vec::new()));
                }
            }
            continue;
        }
        let Some((_, edid)) = current.as_mut() else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed == "EDID:" {
            collecting = true;
            continue;
        }
        if !collecting {
            continue;
        }
        let hex: String = trimmed.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if hex.len() < 32 || hex.len() % 2 != 0 {
            collecting = false;
            continue;
        }
        for pair in hex.as_bytes().chunks_exact(2) {
            if let Ok(value) = u8::from_str_radix(std::str::from_utf8(pair).unwrap_or(""), 16) {
                edid.push(value);
            }
        }
    }
    if let Some((name, edid)) = current {
        if let Some(identity) = parse_edid(&edid) {
            identities.insert(name, identity);
        }
    }
    identities
}

fn display_edid_identities() -> HashMap<String, serde_json::Value> {
    let Ok(out) = std::process::Command::new("xrandr")
        .args(["--verbose"])
        .output()
    else {
        return HashMap::new();
    };
    if !out.status.success() {
        return HashMap::new();
    }
    display_edid_identities_from(&String::from_utf8_lossy(&out.stdout))
}

fn display_output_json(
    state: &DisplayOutputState,
    identities: &HashMap<String, serde_json::Value>,
) -> serde_json::Value {
    let current = state.current.as_ref();
    let (x, y) = current
        .and_then(|current| display_position_pair(&current.position))
        .unwrap_or((0, 0));
    let (mut width, mut height) = current
        .and_then(|current| display_mode_size(&current.mode, state))
        .unwrap_or((0, 0));
    if current
        .map(|current| matches!(current.rotation.as_str(), "left" | "right"))
        .unwrap_or(false)
    {
        std::mem::swap(&mut width, &mut height);
    }
    serde_json::json!({
        "name": state.name,
        "identity": identities.get(&state.name),
        "connected": state.connected,
        "active": current.is_some(),
        "primary": current.map(|current| current.primary).unwrap_or(false),
        "geometry": current.map(|_| serde_json::json!({
            "x": x,
            "y": y,
            "width": width,
            "height": height,
        })),
        "current": current.map(|current| serde_json::json!({
            "mode": current.mode,
            "rate": current.rate,
            "position": current.position,
            "rotation": current.rotation,
            "primary": current.primary,
        })),
        "modes": state.modes.iter().map(|mode| serde_json::json!({
            "mode": mode.mode,
            "rates": mode.rates,
            "preferred": mode.preferred,
        })).collect::<Vec<_>>(),
    })
}

fn display_profile_json(profile: &DisplayProfile) -> serde_json::Value {
    serde_json::json!({
        "name": profile.name,
        "outputs": profile.outputs.iter().map(|output| serde_json::json!({
            "name": output.name,
            "mode": output.mode,
            "rate": output.rate,
            "position": output.position,
            "rotation": output.rotation,
            "primary": output.primary,
        })).collect::<Vec<_>>(),
    })
}

fn display_status_json() -> Result<()> {
    let outputs = display_outputs()?;
    let identities = display_edid_identities();
    let file = display_read_file()?;
    let preview = display_preview_read().map(|preview| {
        serde_json::json!({
            "name": preview.name,
            "token": preview.token,
            "seconds_left": preview.expiry.saturating_sub(now_secs()),
        })
    });
    let value = serde_json::json!({
        "schema": 1,
        "outputs": outputs.iter().map(|output| display_output_json(output, &identities)).collect::<Vec<_>>(),
        "profiles": file.profile.iter().map(display_profile_json).collect::<Vec<_>>(),
        "startup_profile": file.startup_profile,
        "preview": preview,
    });
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn display_list_json() -> Result<()> {
    let file = display_read_file()?;
    let value = serde_json::json!({
        "schema": 1,
        "profiles": file.profile.iter().map(display_profile_json).collect::<Vec<_>>(),
    });
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn display_read_profile_from_stdin() -> Result<DisplayProfile> {
    const LIMIT: u64 = 256 * 1024;
    let mut text = String::new();
    std::io::stdin()
        .take(LIMIT)
        .read_to_string(&mut text)
        .map_err(|e| format!("cannot read the display profile from stdin: {e}"))?;
    if text.len() as u64 >= LIMIT {
        return Err("display profile JSON is too large".into());
    }
    let mut profile: DisplayProfile =
        serde_json::from_str(&text).map_err(|e| format!("invalid display profile JSON: {e}"))?;
    if profile.name.trim().is_empty() {
        profile.name = "draft".into();
    }
    if profile.outputs.is_empty() {
        return Err("display profile has no outputs".into());
    }
    Ok(profile)
}

fn display_preview_apply(profile: &DisplayProfile, seconds: u64) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let outputs = display_outputs()?;
    // A second preview must undo to the layout before the first preview, not
    // to the already-previewed layout. Keep the original recovery snapshot.
    let previous = display_preview_read()
        .map(|preview| preview.previous)
        .filter(|outputs| !outputs.is_empty())
        .unwrap_or_else(|| display_current_layout(&outputs));
    if previous.is_empty() {
        return Err("no active display layout to revert to".into());
    }
    let seconds = seconds.clamp(5, 120);
    let token = format!("{}-{}", now_secs(), std::process::id());
    let expiry = now_secs() + seconds;
    if let Err(error) = display_preview_write(&DisplayPreview {
        name: profile.name.clone(),
        token: token.clone(),
        expiry,
        previous: previous.clone(),
    }) {
        return Err(format!("could not save the preview transaction: {error}").into());
    }
    display_notify(
        "Display preview",
        &format!(
            "{}: keep within {seconds}s or the previous layout returns",
            profile.name
        ),
    );
    let ready = display_preview_path().with_file_name(format!("display-{token}.ready"));
    match std::process::Command::new(exe)
        .args(["display", "expire", &token])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(mut watchdog) => {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while !ready.is_file() && std::time::Instant::now() < deadline {
                if watchdog.try_wait()?.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            if !ready.is_file() {
                let _ = watchdog.kill();
                let _ = watchdog.wait();
                let _ = std::fs::remove_file(display_preview_path());
                return Err("display recovery did not become ready; layout unchanged".into());
            }
            let _ = std::fs::remove_file(&ready);
            if let Err(error) = display_apply(&outputs, profile) {
                let _ = display_restore(&previous);
                let _ = std::fs::remove_file(display_preview_path());
                return Err(error);
            }
            println!("preview-token: {token}");
            println!("previewing display profile {} for {seconds}s", profile.name);
            Ok(())
        }
        Err(error) => {
            let _ = display_restore(&previous);
            let _ = std::fs::remove_file(display_preview_path());
            Err(format!("cannot start the display preview watchdog: {error}").into())
        }
    }
}

fn display_save_profile(profile: &DisplayProfile, name: &str) -> Result<()> {
    if name.is_empty() || name.chars().any(|c| c.is_control()) {
        return Err("invalid display profile name".into());
    }
    let mut file = display_read_file()?;
    let mut saved = profile.clone();
    saved.name = name.to_string();
    file.profile.retain(|profile| profile.name != name);
    file.profile.push(saved);
    display_write_file(&file)?;
    println!("saved display profile {name}");
    Ok(())
}

fn display(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("list");
    let _guard = if matches!(action, "list" | "status" | "expire") {
        None
    } else {
        Some(display_transaction_lock()?)
    };
    match action {
        "list" => {
            if args.iter().any(|arg| arg == "--json") {
                return display_list_json();
            }
            for profile in display_read_file()?.profile {
                println!(
                    "{}\t{}\tdisplay profile",
                    profile.name,
                    profile.outputs.len()
                );
            }
        }
        "save" => {
            let name = args.get(1).ok_or("display save needs a name")?;
            if name.is_empty() || name.chars().any(|c| c.is_control()) {
                return Err("invalid display profile name".into());
            }
            let mut file = display_read_file()?;
            let outputs = display_outputs()?;
            let layout = display_current_layout(&outputs);
            if layout.is_empty() {
                return Err("no active display outputs to save".into());
            }
            file.profile.retain(|profile| profile.name != *name);
            file.profile.push(DisplayProfile {
                name: name.clone(),
                outputs: layout,
            });
            display_write_file(&file)?;
            println!("saved display profile {name}");
        }
        "save-json" => {
            let profile = display_read_profile_from_stdin()?;
            let name = args
                .get(1)
                .map(String::as_str)
                .unwrap_or(profile.name.as_str());
            display_save_profile(&profile, name)?;
        }
        "startup" => {
            display_startup(&args[1..])?;
        }
        "delete" => {
            let name = args.get(1).ok_or("display delete needs a profile name")?;
            let mut file = display_read_file()?;
            let before = file.profile.len();
            file.profile.retain(|profile| profile.name != *name);
            if file.profile.len() == before {
                return Err(format!("unknown display profile: {name}").into());
            }
            display_write_file(&file)?;
            println!("deleted display profile {name}");
        }
        "rename" => {
            let old = args.get(1).ok_or("display rename needs an old name")?;
            let new = args.get(2).ok_or("display rename needs a new name")?;
            if new.is_empty() || new.chars().any(|c| c.is_control()) {
                return Err("invalid display profile name".into());
            }
            let mut file = display_read_file()?;
            if file.profile.iter().any(|profile| profile.name == *new) {
                return Err(format!("display profile already exists: {new}").into());
            }
            let profile = file
                .profile
                .iter_mut()
                .find(|profile| profile.name == *old)
                .ok_or_else(|| format!("unknown display profile: {old}"))?;
            profile.name = new.to_string();
            display_write_file(&file)?;
            println!("renamed display profile {old} to {new}");
        }
        "apply" | "preview" => {
            let name = args.get(1).ok_or("display command needs a profile name")?;
            let profile = display_read_file()?
                .profile
                .into_iter()
                .find(|p| p.name == *name)
                .ok_or_else(|| format!("unknown display profile: {name}"))?;
            let outputs = display_outputs()?;
            if action == "apply" {
                let previous = display_current_layout(&outputs);
                if let Err(error) = display_apply(&outputs, &profile) {
                    if !previous.is_empty() {
                        let _ = display_restore(&previous);
                    }
                    return Err(error);
                }
                let _ = std::fs::remove_file(display_preview_path());
                println!("applied display profile {name}");
                return Ok(());
            }
            let seconds = args
                .get(2)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(15);
            display_preview_apply(&profile, seconds)?;
        }
        "preview-json" => {
            let profile = display_read_profile_from_stdin()?;
            let seconds = args
                .get(1)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(15);
            display_preview_apply(&profile, seconds)?;
        }
        "keep" => {
            if let Some(state) = display_preview_read() {
                if args.get(1).map(String::as_str) != Some(state.token.as_str()) {
                    return Err(
                        "stale or missing display transaction token; refresh the view".into(),
                    );
                }
                let _ = std::fs::remove_file(display_preview_path());
                display_notify("Display layout kept", "Preview confirmed");
                println!("display preview kept");
            } else {
                println!("no display preview active");
            }
        }
        "revert" => {
            let Some(state) = display_preview_read() else {
                println!("no display preview active");
                return Ok(());
            };
            if args.get(1).map(String::as_str) != Some(state.token.as_str()) {
                return Err("stale or missing display transaction token; refresh the view".into());
            }
            display_restore(&state.previous)?;
            let _ = std::fs::remove_file(display_preview_path());
            display_notify("Display layout reverted", "Previous layout restored");
            println!("display preview reverted");
        }
        "expire" => {
            let Some(token) = args.get(1) else {
                return Err("display expire needs a token".into());
            };
            let Some(state) = display_preview_read() else {
                return Ok(());
            };
            if state.token != *token {
                return Ok(());
            }
            let ready = display_preview_path().with_file_name(format!("display-{token}.ready"));
            std::fs::write(&ready, b"ready")?;
            let wait = state.expiry.saturating_sub(now_secs());
            if wait > 0 {
                std::thread::sleep(Duration::from_secs(wait));
            }
            let _guard = display_transaction_lock()?;
            let _ = std::fs::remove_file(&ready);
            let Some(state) = display_preview_read() else {
                return Ok(());
            };
            if state.token != *token {
                return Ok(());
            }
            display_restore(&state.previous)?;
            let _ = std::fs::remove_file(display_preview_path());
            display_notify("Display preview expired", "Previous layout restored");
        }
        "rotate" => {
            // One screen turned without building a profile: the common case
            // for a display mounted upside down or standing on its side.
            let Some(name) = args.get(1) else {
                return Err(DISPLAY_ROTATE_USAGE.into());
            };
            let Some(rotation) = args.get(2) else {
                return Err(DISPLAY_ROTATE_USAGE.into());
            };
            display_rotate(name, rotation)?;
        }
        "auto" => {
            // Lay the connected screens out side by side in xrandr's own
            // order, each at its preferred mode and upright. This is the
            // starting point a profile is usually saved from.
            let outputs = display_outputs()?;
            let mut position = 0i32;
            let mut layout = Vec::new();
            // A connected screen may be switched off, as one just plugged in
            // is. It is part of the arrangement, not skipped.
            for state in outputs.iter().filter(|state| state.connected) {
                if state.modes.is_empty() {
                    continue;
                }
                let mut output = state.current.clone().unwrap_or(DisplayOutput {
                    name: state.name.clone(),
                    mode: "preferred".into(),
                    rate: None,
                    position: display_zero_position(),
                    rotation: display_normal_rotation(),
                    primary: false,
                });
                output.mode = "preferred".into();
                output.rate = None;
                output.rotation = "normal".into();
                output.position = format!("{position}x0");
                // Place the next screen past this one's real preferred size.
                let resolved = display_resolve(&outputs, &output)?;
                position += display_mode_size(&resolved.mode, state)
                    .map(|(width, _)| width)
                    .unwrap_or(0);
                layout.push(output);
            }
            if layout.is_empty() {
                return Err("no connected display output to arrange".into());
            }
            let profile = DisplayProfile {
                name: "auto".into(),
                outputs: layout,
            };
            let previous = display_current_layout(&outputs);
            if let Err(error) = display_apply(&outputs, &profile) {
                if !previous.is_empty() {
                    let _ = display_restore(&previous);
                }
                return Err(error);
            }
            let mut summary = String::new();
            for output in &profile.outputs {
                if !summary.is_empty() {
                    summary.push_str(", ");
                }
                summary.push_str(&format!("{} {}", output.name, output.position));
            }
            display_notify("Displays arranged", &summary);
            println!("displays arranged side by side: {summary}");
        }
        "status" => {
            if args.iter().any(|arg| arg == "--json") {
                return display_status_json();
            }
            match display_preview_read() {
                Some(state) if state.expiry > now_secs() => {
                    println!(
                        "display preview {} remaining {}s",
                        state.name,
                        state.expiry - now_secs()
                    );
                }
                _ => println!("display preview off"),
            }
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PowerProfileState {
    active: String,
    profiles: Vec<String>,
    source: String,
}

fn powerprofilesctl(args: &[&str]) -> Result<String> {
    let program = program("powerprofilesctl")
        .ok_or("power profiles need powerprofilesctl (power-profiles-daemon)")?;
    provider_query(
        &program,
        &args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>(),
    )
}

fn power_source() -> String {
    let base = std::path::Path::new("/sys/class/power_supply");
    let Ok(entries) = std::fs::read_dir(base) else {
        return "unknown".into();
    };
    let mut battery = false;
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        let kind = kind.trim();
        if kind == "Mains" || kind == "USB" {
            if std::fs::read_to_string(path.join("online"))
                .map(|v| v.trim() == "1")
                .unwrap_or(false)
            {
                return "ac".into();
            }
        } else if kind == "Battery" {
            battery = true;
        }
    }
    if battery {
        "battery".into()
    } else {
        "unknown".into()
    }
}

fn power_profile_state() -> Result<PowerProfileState> {
    let active = powerprofilesctl(&["get"])?.trim().to_string();
    if active.is_empty() {
        return Err("powerprofilesctl returned no active profile".into());
    }
    let list = powerprofilesctl(&["list"])?;
    let mut profiles = Vec::new();
    for line in list.lines() {
        let clean = line
            .trim()
            .trim_start_matches('*')
            .trim()
            .trim_end_matches(':');
        if !clean.is_empty()
            && clean
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-' || c == '_')
            && !profiles.iter().any(|profile| profile == clean)
        {
            profiles.push(clean.to_string());
        }
    }
    if profiles.is_empty() {
        return Err("powerprofilesctl reported no available profiles".into());
    }
    Ok(PowerProfileState {
        active,
        profiles,
        source: power_source(),
    })
}

fn powerprofile_status() {
    if program("powerprofilesctl").is_none() {
        println!("state=provider-unavailable\nreason=powerprofilesctl (power-profiles-daemon) is not installed");
        return;
    }
    match power_profile_state() {
        Ok(state) => {
            println!(
                "state=ready\nprovider=power-profiles-daemon\nactive={}\npower={}",
                state.active, state.source
            );
            for profile in state.profiles {
                println!("profile={profile}");
            }
        }
        Err(error) => println!("state=unavailable\nreason={error}"),
    }
}

fn powerprofile(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("status");
    match action {
        "status" => powerprofile_status(),
        "list" => {
            for profile in power_profile_state()?.profiles {
                println!("{profile}");
            }
        }
        "set" | "next" => {
            let state = power_profile_state()?;
            let target = if action == "next" {
                let index = state
                    .profiles
                    .iter()
                    .position(|profile| profile == &state.active)
                    .unwrap_or(0);
                state.profiles[(index + 1) % state.profiles.len()].clone()
            } else {
                args.get(1)
                    .ok_or("powerprofile set needs a profile name")?
                    .to_string()
            };
            if !state.profiles.iter().any(|profile| profile == &target) {
                return Err(
                    format!("power profile {target} is not supported by this provider").into(),
                );
            }
            if target != state.active {
                powerprofilesctl(&["set", &target])?;
                std::thread::sleep(Duration::from_millis(250));
                let after = power_profile_state()?;
                if after.active != target {
                    return Err(format!("power profile {target} did not become active").into());
                }
            }
            println!("power profile {target}");
        }
        _ => return Err(USAGE.into()),
    }
    Ok(())
}

fn monitors() -> Result<()> {
    // Read RandR directly so this works without querying the WM.
    let (c, s) = x11rb::connect(None)?;
    let root = c.setup().roots[s].root;
    c.extension_information(x11rb::protocol::randr::X11_EXTENSION_NAME)
        .ok()
        .flatten()
        .ok_or("this X server has no RandR extension")?;
    let resources = c.randr_get_screen_resources_current(root)?.reply()?;
    let mut found = 0;
    println!("NIRUWM monitors:");
    for output in resources.outputs {
        let info = c
            .randr_get_output_info(output, resources.config_timestamp)?
            .reply()?;
        if info.connection != x11rb::protocol::randr::Connection::CONNECTED {
            continue;
        }
        let name = String::from_utf8_lossy(&info.name).to_string();
        if info.crtc == 0 {
            println!("  {name:<12} connected, no mode");
            continue;
        }
        let crtc = c
            .randr_get_crtc_info(info.crtc, resources.config_timestamp)?
            .reply()?;
        found += 1;
        println!(
            "  {name:<12} {}x{}+{}+{}",
            crtc.width, crtc.height, crtc.x, crtc.y
        );
    }
    if found == 0 {
        println!("  (no active output; the whole screen is one monitor)");
    }
    println!();
    println!("Each screen gets its own workspaces. Override one in [monitors].");
    Ok(())
}

fn wallpaper_config_path() -> PathBuf {
    config::directory().join("config.toml")
}

fn wallpaper_expand(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        return PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest);
    }
    PathBuf::from(path)
}

fn wallpaper_is_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "bmp")
    )
}

fn wallpaper_friendly_name(base: &str) -> String {
    let named = match base {
        "nirux-wallpaper-noir" => Some("Niru Noir"),
        "nirux-wallpaper-nord" => Some("Niru Nord"),
        "nirux-wallpaper-satie" => Some("Niru Satie"),
        "nirux-wallpaper-carl-larsson" => Some("Niru Carl Larsson"),
        "nirux-wallpaper-forest-vigil" => Some("Forest Vigil"),
        "nirux-wallpaper-blood-of-urdr" => Some("Blood of Urdr"),
        "nirux-wallpaper-root-and-stone" => Some("Root and Stone"),
        "nirux-wallpaper-blue-horizon" => Some("Blue Horizon"),
        "nirux-wallpaper-the-norns" => Some("The Norns"),
        "nirux-wallpaper-winter-nights" => Some("Winter Nights"),
        "nirux-wallpaper-noir-havamal-19" => Some("Håvamål 19"),
        "nirux-wallpaper-noir-rooted-in-less" => Some("Rooted in Less"),
        "nirux-wallpaper-noir-space-to-think-silhouette" => Some("Space to Think Silhouette"),
        _ => None,
    };
    if let Some(name) = named {
        return name.into();
    }
    let stem = base.strip_prefix("nirux-wallpaper-").unwrap_or(base);
    stem.split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn wallpaper_collect(
    dir: &Path,
    theme_dir: &Path,
    seen: &mut std::collections::HashSet<PathBuf>,
    entries: &mut std::collections::BTreeMap<String, (PathBuf, u8)>,
) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for file in read_dir.flatten() {
        let path = file.path();
        if !path.is_file() || !wallpaper_is_image(&path) {
            continue;
        }
        if path.starts_with(theme_dir) {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(wallpaper_friendly_name)
            .unwrap_or_else(|| "Wallpaper".into());
        entries.entry(name).or_insert((canonical, 0));
        if entries.len() >= 200 {
            return;
        }
    }
}

fn wallpaper_list() -> Result<()> {
    let cfg = Config::load()?;
    let theme_dir = wallpaper_expand(&cfg.wallpaper.theme_dir);
    let dir = wallpaper_expand(&cfg.wallpaper.dir);
    let mut seen = std::collections::HashSet::new();
    let mut themes: std::collections::BTreeMap<String, (PathBuf, u8)> =
        std::collections::BTreeMap::new();
    if let Ok(read_dir) = fs::read_dir(&theme_dir) {
        for file in read_dir.flatten() {
            let path = file.path();
            if !path.is_file() || !wallpaper_is_image(&path) {
                continue;
            }
            let Ok(canonical) = path.canonicalize() else {
                continue;
            };
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let (base, score) = if let Some(base) = stem.strip_suffix("-4k") {
                (base, 2)
            } else if let Some(base) = stem.strip_suffix("-8k") {
                (base, 1)
            } else {
                (stem, 0)
            };
            let entry = themes
                .entry(wallpaper_friendly_name(base))
                .or_insert((canonical.clone(), 0));
            if score > entry.1 {
                *entry = (canonical, score);
            }
        }
    }
    for (name, (path, _)) in themes {
        seen.insert(path.clone());
        println!("{name}\t{}", path.display());
    }
    let mut user = std::collections::BTreeMap::new();
    wallpaper_collect(&dir, &theme_dir, &mut seen, &mut user);
    for file in &cfg.wallpaper.files {
        let path = wallpaper_expand(file);
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !canonical.is_file()
            || !wallpaper_is_image(&canonical)
            || !seen.insert(canonical.clone())
        {
            continue;
        }
        let name = canonical
            .file_stem()
            .and_then(|s| s.to_str())
            .map(wallpaper_friendly_name)
            .unwrap_or_else(|| "Wallpaper".into());
        user.entry(name).or_insert((canonical, 0));
    }
    for (name, (path, _)) in user {
        println!("{name}\t{}", path.display());
    }
    Ok(())
}

fn wallpaper_status() -> Result<()> {
    let cfg = Config::load()?;
    let wallpaper = &cfg.wallpaper;
    println!("enabled={}", wallpaper.enabled);
    println!("mode={}", wallpaper.mode);
    println!("rotation_minutes={}", wallpaper.rotation_minutes);
    println!("scale={}", wallpaper.scale);
    println!("follow_theme={}", wallpaper.follow_theme);
    println!("file={}", wallpaper.file);
    println!("dir={}", wallpaper.dir);
    println!("theme_dir={}", wallpaper.theme_dir);
    println!("theme={}", cfg.theme);
    let resolved = wallpaper::Wallpaper::new(wallpaper, &cfg.theme).theme_image();
    println!(
        "theme_image={}",
        resolved
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );
    let following = if !wallpaper.enabled {
        "disabled"
    } else if !wallpaper.follow_theme {
        "follow-theme-off"
    } else if wallpaper.mode == "single" {
        "single-file-override (use: niructl wallpaper follow-theme on)"
    } else if resolved.is_none() {
        "theme-image-missing"
    } else {
        "enabled"
    };
    println!("theme_following={following}");
    // The plan per screen: what the configuration says each screen shows. It
    // is not proof that a running window manager rendered it.
    let mut planned = wallpaper::Wallpaper::new(wallpaper, &cfg.theme);
    if let Ok((c, s)) = x11rb::connect(None) {
        let root = c.setup().roots[s].root;
        if let Ok(outputs) = randr_outputs(&c, root) {
            planned.set_outputs(outputs);
        }
    }
    for plan in planned.plan() {
        println!(
            "output={}\tsource={}\tmode={}\tscale={}\timage={}",
            plan.output, plan.source, plan.kind, plan.scale, plan.path
        );
    }
    Ok(())
}

/// The rendered state the running window manager published, if it can.
fn wallpaper_rendered() -> Option<Vec<serde_json::Value>> {
    let (c, s) = x11rb::connect(None).ok()?;
    let root = c.setup().roots[s].root;
    let atom = c
        .intern_atom(false, b"_NIRUWM_WALLPAPER_STATE")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let reply = c
        .get_property(false, root, atom, AtomEnum::ANY, 0, 1 << 16)
        .ok()?
        .reply()
        .ok()?;
    let text = String::from_utf8_lossy(&reply.value).to_string();
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    // A session without wallpaper ownership publishes an empty list.
    value.get("outputs")?.as_array().cloned()
}

/// Apply one request as a single transaction, then report what the running
/// window manager rendered. Prints JSON, for the GUI and for scripts.
fn wallpaper_apply(args: &[String]) -> Result<()> {
    let mut settings: Vec<(String, String)> = Vec::new();
    let mut output: Option<(String, String, Option<String>)> = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args.get(index + 1).cloned();
        match flag {
            "--json" => {
                index += 1;
                continue;
            }
            "--image" | "--scale" | "--rotation" | "--follow-theme" => {
                let key = flag.trim_start_matches("--").replace('-', "_");
                let value = value.ok_or(format!("{flag} needs a value"))?;
                settings.push((key, value));
                index += 2;
            }
            "--output" | "--output-image" | "--output-scale" | "--output-inherit" => {
                let output_name = value.ok_or(format!("{flag} needs an output name"))?;
                match flag {
                    "--output-inherit" => {
                        output = Some((output_name, "inherit".into(), None));
                        index += 2;
                    }
                    _ => {
                        let setting = args
                            .get(index + 2)
                            .cloned()
                            .ok_or(format!("{flag} needs a value after the output name"))?;
                        let key = match flag {
                            "--output" => "image",
                            _ => "scale",
                        };
                        output = Some((output_name, key.into(), Some(setting)));
                        index += 3;
                    }
                }
            }
            other => return Err(format!("unknown wallpaper apply option: {other}").into()),
        }
    }
    if settings.is_empty() && output.is_none() {
        return Err("wallpaper apply needs at least one setting".into());
    }
    for (key, value) in &settings {
        match key.as_str() {
            "scale" if !["fill", "fit", "stretch", "center"].contains(&value.as_str()) => {
                return Err("wallpaper scale must be fill, fit, stretch or center".into())
            }
            "rotation" if value.parse::<u32>().is_err() => {
                return Err("rotation must be minutes".into())
            }
            "rotation" if value.parse::<u32>().unwrap_or(0) > 100_000 => {
                return Err("rotation is too large".into())
            }
            "follow_theme" if !["on", "off"].contains(&value.as_str()) => {
                return Err("follow-theme must be on or off".into())
            }
            _ => {}
        }
    }
    let output_clone = output.clone();
    let settings_clone = settings.clone();
    let count = settings.len() + usize::from(output.is_some());
    wallpaper_transaction(
        move |document| {
            let mut change = OutputChange::default();
            for (key, value) in &settings_clone {
                change
                    .actions
                    .extend(apply_common_setting(document, key, value));
            }
            if let Some((name, key, value)) = &output_clone {
                let entry = set_output_entry(document, name, key, value.as_deref());
                change.actions.extend(entry.actions);
                if let Some(e) = entry.error {
                    change.fail(e);
                }
            }
            change
        },
        false,
    )
    .map_err(|e| e.to_string())?;
    // Re-read the state now that the change is acknowledged.
    let status_cfg = Config::load()?;
    let mut planned = wallpaper::Wallpaper::new(&status_cfg.wallpaper, &status_cfg.theme);
    if let Ok((c, s)) = x11rb::connect(None) {
        let root = c.setup().roots[s].root;
        if let Ok(outputs) = randr_outputs(&c, root) {
            planned.set_outputs(outputs);
        }
    }
    let plan = planned.plan();
    let rendered = wallpaper_rendered();
    println!(
        "{}",
        serde_json::json!({
            "applied": true,
            "settings": count,
            "status": wallpaper_json(&status_cfg, &planned),
            "planned": plan.iter().map(|p| serde_json::json!({
                "output": p.output,
                "source": p.source,
                "mode": p.kind,
                "scale": p.scale,
                "image": p.path,
            })).collect::<Vec<_>>(),
            "rendered": rendered,
        })
    );
    Ok(())
}

/// The state as JSON, so the GUI reads one structured document instead of
/// parsing the human-oriented status lines.
fn wallpaper_json(cfg: &Config, planned: &wallpaper::Wallpaper) -> serde_json::Value {
    let w = &cfg.wallpaper;
    let theme_image = planned.theme_image();
    serde_json::json!({
        "theme": cfg.theme,
        "enabled": w.enabled,
        "mode": w.mode,
        "rotation_minutes": w.rotation_minutes,
        "scale": w.scale,
        "follow_theme": w.follow_theme,
        "file": w.file,
        "dir": w.dir,
        "theme_dir": w.theme_dir,
        "theme_image": theme_image.map(|p| p.display().to_string()).unwrap_or_default(),
        "outputs": w.outputs.iter().map(|o| serde_json::json!({
            "output": o.output,
            "mode": o.mode,
            "file": o.file,
            "scale": o.scale,
        })).collect::<Vec<_>>(),
    })
}

fn wallpaper_status_json() -> Result<()> {
    let cfg = Config::load()?;
    let mut planned = wallpaper::Wallpaper::new(&cfg.wallpaper, &cfg.theme);
    if let Ok((c, s)) = x11rb::connect(None) {
        let root = c.setup().roots[s].root;
        if let Ok(outputs) = randr_outputs(&c, root) {
            planned.set_outputs(outputs);
        }
    }
    let mut value = wallpaper_json(&cfg, &planned);
    value["planned"] = serde_json::Value::Array(
        planned
            .plan()
            .iter()
            .map(|p| {
                serde_json::json!({
                    "output": p.output,
                    "source": p.source,
                    "mode": p.kind,
                    "scale": p.scale,
                    "image": p.path,
                })
            })
            .collect(),
    );
    value["rendered"] = wallpaper_rendered()
        .map(serde_json::Value::Array)
        .unwrap_or(serde_json::Value::Null);
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// The value changes one `wallpaper apply` request makes, as printed lines.
fn apply_common_setting(
    document: &mut toml_edit::DocumentMut,
    key: &str,
    value: &str,
) -> Vec<String> {
    if !document
        .get("wallpaper")
        .map(|w| w.is_table())
        .unwrap_or(false)
    {
        let mut table = toml_edit::Table::new();
        table.set_implicit(false);
        document["wallpaper"] = toml_edit::Item::Table(table);
    }
    match key {
        "image" => match wallpaper_expand(value).canonicalize() {
            Ok(path) if path.is_file() && wallpaper_is_image(&path) => {
                document["wallpaper"]["enabled"] = toml_edit::value(true);
                document["wallpaper"]["mode"] = toml_edit::value("single");
                document["wallpaper"]["file"] = toml_edit::value(path.display().to_string());
                vec![format!("common picture {}", path.display())]
            }
            _ => vec!["the chosen image is no longer readable; nothing was changed".into()],
        },
        "scale" => {
            document["wallpaper"]["enabled"] = toml_edit::value(true);
            document["wallpaper"]["scale"] = toml_edit::value(value);
            vec![format!("common scale {value}")]
        }
        "rotation" => {
            document["wallpaper"]["enabled"] = toml_edit::value(true);
            document["wallpaper"]["rotation_minutes"] =
                toml_edit::value(value.parse::<i64>().unwrap_or(0));
            // Leaving single mode is what makes an interval take effect.
            if document["wallpaper"]["mode"].as_str() == Some("single")
                && !document["wallpaper"]["file"]
                    .as_str()
                    .unwrap_or("")
                    .is_empty()
            {
                document["wallpaper"]["mode"] = toml_edit::value("fixed");
            }
            vec![format!("rotation {value} minutes")]
        }
        "follow_theme" => {
            let on = value == "on";
            document["wallpaper"]["follow_theme"] = toml_edit::value(on);
            if on {
                document["wallpaper"]["enabled"] = toml_edit::value(true);
                if document["wallpaper"]["mode"].as_str() == Some("single") {
                    document["wallpaper"]["mode"] = toml_edit::value("fixed");
                }
            }
            vec![format!("follow the theme: {value}")]
        }
        _ => vec![],
    }
}

fn wallpaper_document() -> Result<toml_edit::DocumentMut> {
    let path = wallpaper_config_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut document = text.parse::<toml_edit::DocumentMut>()?;
    if !document.contains_key("wallpaper") {
        let mut table = toml_edit::Table::new();
        table.set_implicit(false);
        document["wallpaper"] = toml_edit::Item::Table(table);
    }
    Ok(document)
}

fn write_wallpaper_document(document: &toml_edit::DocumentMut) -> Result<()> {
    let path = wallpaper_config_path();
    let dir = path.parent().ok_or("configuration directory is missing")?;
    fs::create_dir_all(dir)?;
    let mode = fs::metadata(&path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o644);
    let temp = dir.join(format!(".config.toml.{}", std::process::id()));
    fs::write(&temp, document.to_string())?;
    fs::set_permissions(&temp, std::fs::Permissions::from_mode(mode))?;
    fs::rename(temp, path)?;
    Ok(())
}

/// Outcomes of a per-output change, as a person would read them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct OutputChange {
    actions: Vec<String>,
    error: Option<String>,
}

impl OutputChange {
    fn push(&mut self, action: impl Into<String>) {
        self.actions.push(action.into());
    }

    fn fail(&mut self, error: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(error.into());
        }
    }
}

/// Apply one `niructl wallpaper output` request to a parsed document.
///
/// `key` is `image` (a path), `scale` (fill/fit/stretch/center), `mode`
/// (`theme` to follow the theme on this screen, `image` for its own
/// picture) or `inherit`. `value` is unused by `inherit`.
fn set_output_entry(
    document: &mut toml_edit::DocumentMut,
    output: &str,
    key: &str,
    value: Option<&str>,
) -> OutputChange {
    let mut change = OutputChange::default();
    if output.is_empty()
        || !output
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        change.fail("output must be a RandR name such as HDMI-1");
        return change;
    }
    if !document
        .get("wallpaper")
        .map(|w| w.is_table())
        .unwrap_or(false)
    {
        let mut table = toml_edit::Table::new();
        table.set_implicit(false);
        document["wallpaper"] = toml_edit::Item::Table(table);
    }
    let array = match document["wallpaper"]
        .as_table_mut()
        .and_then(|t| t.get_mut("outputs"))
    {
        Some(item) => match item.as_array_of_tables_mut() {
            Some(array) => array,
            None => {
                change.fail("wallpaper.outputs is not a list of tables");
                return change;
            }
        },
        None => {
            let mut array = toml_edit::ArrayOfTables::new();
            array.push(toml_edit::Table::new());
            document["wallpaper"]["outputs"] = toml_edit::Item::ArrayOfTables(array);
            document["wallpaper"]["outputs"]
                .as_array_of_tables_mut()
                .expect("just inserted")
        }
    };
    // A table with no output name cannot describe a screen; it is dropped so
    // the file stays loadable.
    let stale: Vec<usize> = array
        .iter()
        .enumerate()
        .filter(|(_, t)| t.get("output").and_then(|v| v.as_str()).is_none())
        .map(|(i, _)| i)
        .collect();
    for index in stale.into_iter().rev() {
        array.remove(index);
    }
    let index = array
        .iter()
        .position(|t| t.get("output").and_then(|v| v.as_str()) == Some(output));
    let index = match index {
        Some(i) => i,
        None => {
            let mut table = toml_edit::Table::new();
            table["output"] = toml_edit::value(output);
            table["mode"] = toml_edit::value("inherit");
            table["file"] = toml_edit::value("");
            table["scale"] = toml_edit::value("");
            array.push(table);
            array.len() - 1
        }
    };
    let table = array
        .get_mut(index)
        .expect("the entry exists or was just pushed");
    match key {
        "image" => {
            let Some(raw) = value else {
                change.fail("output image needs a path");
                return change;
            };
            let path = match wallpaper_expand(raw).canonicalize() {
                Ok(path) if path.is_file() && wallpaper_is_image(&path) => path,
                Ok(_) => {
                    change.fail(format!("not a supported image: {raw}"));
                    return change;
                }
                Err(e) => {
                    change.fail(format!("wallpaper not found: {raw}: {e}"));
                    return change;
                }
            };
            if !table.contains_key("output") {
                table["output"] = toml_edit::value(output);
            }
            table["mode"] = toml_edit::value("single");
            table["file"] = toml_edit::value(path.display().to_string());
            change.push(format!("{output} uses {}", path.display()));
        }
        "scale" => {
            let Some(scale) = value else {
                change.fail("output scale needs a value");
                return change;
            };
            if !["fill", "fit", "stretch", "center"].contains(&scale) {
                change.fail("output scale must be fill, fit, stretch or center");
                return change;
            }
            table["scale"] = toml_edit::value(scale);
            change.push(format!("{output} scale {scale}"));
        }
        "mode" => {
            let Some(mode) = value else {
                change.fail("output mode needs a value");
                return change;
            };
            if !["theme", "image"].contains(&mode) {
                change.fail("output mode must be theme or image");
                return change;
            }
            table["mode"] = toml_edit::value(if mode == "theme" { "theme" } else { "single" });
            if mode == "theme" {
                table["file"] = toml_edit::value("");
                change.push(format!("{output} follows the theme"));
            } else {
                change.push(format!("{output} uses its own picture"));
            }
        }
        "inherit" => {
            table["mode"] = toml_edit::value("inherit");
            table["file"] = toml_edit::value("");
            table["scale"] = toml_edit::value("");
            change.push(format!("{output} follows the common settings"));
        }
        _ => change.fail("output key must be image, scale, mode or inherit"),
    }
    document["wallpaper"]["enabled"] = toml_edit::value(true);
    change
}

/// Drop an output entry entirely. Used by reset so no stale file is kept.
fn reset_output_entry(document: &mut toml_edit::DocumentMut, output: &str) -> OutputChange {
    let mut change = OutputChange::default();
    let before = document["wallpaper"]
        .as_table()
        .and_then(|t| t.get("outputs"))
        .and_then(|i| i.as_array_of_tables())
        .map(|a| a.len())
        .unwrap_or(0);
    let mut remaining = 0;
    if let Some(table) = document["wallpaper"].as_table_mut() {
        if let Some(array) = table
            .get_mut("outputs")
            .and_then(|i| i.as_array_of_tables_mut())
        {
            array.retain(|t| t.get("output").and_then(|v| v.as_str()) != Some(output));
            remaining = array.len();
        }
        // An empty list would read back as an entry without an output, so
        // the key goes too; the documented header stays in the file text.
        if remaining == 0 {
            table.remove("outputs");
        }
    }
    if remaining == before {
        change.push(format!("{output} already follows the common settings"));
    } else {
        change.push(format!("{output} follows the common settings"));
    }
    change
}

/// Run a set of document changes as one transaction: parse, change, validate,
/// write atomically, ask the window manager to reload and wait for its answer.
///
/// A file that does not parse is never rewritten. If the reload is rejected,
/// the previous file is put back, but only when nobody edited it meanwhile.
fn wallpaper_transaction<F>(mut change: F, json: bool) -> Result<()>
where
    F: FnMut(&mut toml_edit::DocumentMut) -> OutputChange,
{
    let path = wallpaper_config_path();
    let original = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let mut document: toml_edit::DocumentMut = if original.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        original
            .parse()
            .map_err(|e| format!("refusing to rewrite invalid {}: {e}", path.display()))?
    };
    let outcome = change(&mut document);
    if let Some(error) = &outcome.error {
        return Err(error.clone().into());
    }
    // Value-level validation before anything is written: a value the window
    // manager would reject must never reach the file.
    let candidate: config::Config = document
        .to_string()
        .parse::<toml::Value>()?
        .try_into()
        .map_err(|e: toml::de::Error| {
            format!("refusing to write an invalid wallpaper setting: {e}")
        })?;
    if let Err(e) = candidate.validate() {
        return Err(format!("refusing to write an invalid wallpaper setting: {e}").into());
    }
    let rendered = fill_wallpaper_header(&original, &document.to_string());
    if rendered != original {
        write_wallpaper_text(&path, &rendered)?;
    }
    let result = reload_wallpaper_and_wait();
    if let Err(error) = &result {
        // Put the previous file back only if it is still the text we wrote.
        let now = fs::read_to_string(&path).unwrap_or_default();
        if now == rendered {
            if original.trim().is_empty() {
                let _ = fs::remove_file(&path);
            } else {
                let _ = write_wallpaper_text(&path, &original);
            }
        }
        let mut out = String::from("the window manager did not accept the change: ");
        out.push_str(&error.to_string());
        if now == rendered {
            out.push_str(" (the previous settings were restored)");
        } else {
            out.push_str(" (the file was edited meanwhile and was left as it is)");
        }
        return Err(out.into());
    }
    if json {
        println!("{}", serde_json::json!({ "changes": outcome.actions }));
        return Ok(());
    }
    for action in outcome.actions {
        println!("{action}");
    }
    Ok(())
}

/// Write the wallpaper file atomically, keeping its mode.
fn write_wallpaper_text(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().ok_or("configuration directory is missing")?;
    fs::create_dir_all(dir)?;
    let mode = fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
        .unwrap_or(0o644);
    let temp = dir.join(format!(".config.toml.{}", std::process::id()));
    fs::write(&temp, text)?;
    fs::set_permissions(&temp, std::fs::Permissions::from_mode(mode))?;
    fs::rename(temp, path)?;
    Ok(())
}

/// Keep the documented `[wallpaper]` example when a setting creates the
/// section in a file that never had one. Comments in an existing section are
/// always kept, because the edit is made in place.
fn fill_wallpaper_header(original: &str, rendered: &str) -> String {
    if original.contains("[[wallpaper.outputs]]") || !rendered.contains("[[wallpaper.outputs]]") {
        return rendered.to_string();
    }
    if original.contains("outputs") {
        return rendered.to_string();
    }
    let block = "\n# Pictures for one named screen. A screen without an entry here\n\
                 # uses the common settings above. Entries are kept when the screen\n\
                 # is unplugged. mode is `theme` or `single`; an empty scale means\n\
                 # the common scale.\n";
    if let Some(pos) = rendered.find("[[wallpaper.outputs]]") {
        let mut out = String::with_capacity(rendered.len() + block.len());
        out.push_str(&rendered[..pos]);
        out.push_str(block);
        out.push_str(&rendered[pos..]);
        return out;
    }
    rendered.to_string()
}

/// The X connection a change needs, or the offline answer for commands that
/// only read files.
fn connect_for_change() -> Result<(x11rb::rust_connection::RustConnection, Window)> {
    let (c, s) = x11rb::connect(None)?;
    let root = c.setup().roots[s].root;
    Ok((c, root))
}

/// Ask the running window manager to reload and wait for its acknowledgement.
fn reload_wallpaper_and_wait() -> Result<()> {
    let (c, root) = connect_for_change()?;
    let atom = |n: &str| -> Result<Atom> { Ok(c.intern_atom(false, n.as_bytes())?.reply()?.atom) };
    let serial = command_serial();
    send(&c, root, atom(CMD_ATOM)?, [4, serial, 0, 0, 0])?;
    c.change_property32(
        PropMode::REPLACE,
        root,
        atom(CONFIG_SERIAL)?,
        AtomEnum::CARDINAL,
        &[serial],
    )?;
    c.flush()?;
    if property_u32(&c, root, atom("_NIRUWM_ACK_CONTROL")?) == Some(1) {
        wait_reload_ack(&c, root, &atom, serial)?;
    } else {
        std::thread::sleep(Duration::from_millis(30));
        eprintln!("niructl: the running window manager cannot confirm reload");
    }
    Ok(())
}

/// `niructl wallpaper output ...`: one screen's picture, scale or reset.
fn wallpaper_output(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).ok_or(USAGE)?;
    match sub {
        "list" => wallpaper_output_list(),
        "set" => {
            let output = args.get(1).ok_or(USAGE)?.clone();
            let key = args.get(2).map(String::as_str).ok_or(USAGE)?;
            let value = args.get(3).cloned();
            wallpaper_transaction(
                |document| set_output_entry(document, &output, key, value.as_deref()),
                false,
            )
        }
        "reset" => {
            let output = args.get(1).ok_or(USAGE)?.clone();
            wallpaper_transaction(|document| reset_output_entry(document, &output), false)
        }
        _ => Err(USAGE.into()),
    }
}

/// The screens RandR reports now, with the effective settings per screen.
fn wallpaper_output_list() -> Result<()> {
    let cfg = Config::load()?;
    let (c, s) = x11rb::connect(None)?;
    let root = c.setup().roots[s].root;
    let wall = wallpaper::Wallpaper::new(&cfg.wallpaper, &cfg.theme);
    let mut out = Vec::new();
    for output in randr_outputs(&c, root)? {
        let entry = cfg
            .wallpaper
            .outputs
            .iter()
            .find(|o| o.output == output.name);
        let scale = entry
            .map(|o| o.scale.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| cfg.wallpaper.scale.clone());
        out.push(serde_json::json!({
            "output": output.name,
            "x": output.x,
            "y": output.y,
            "width": output.width,
            "height": output.height,
            "source": if entry.is_some() { "override" } else { "inherit" },
            "mode": entry.map(|o| o.mode.clone()).unwrap_or_else(|| "inherit".into()),
            "file": entry.map(|o| o.file.clone()).unwrap_or_default(),
            "scale": scale,
        }));
    }
    let _ = wall;
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

fn randr_outputs(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
) -> Result<Vec<wallpaper::Output>> {
    // The automated acceptance run supplies a layout; an ordinary session
    // never sets the variable.
    if let Some(path) = std::env::var_os("NIRU_RANDR_EMU") {
        if let Ok(text) = fs::read_to_string(path) {
            let mut out = Vec::new();
            for line in text.lines() {
                let Some((name, rest)) = line.trim().split_once(' ') else {
                    continue;
                };
                let Some((size, pos)) = rest.split_once('+') else {
                    continue;
                };
                let (Some((w, h)), Some((x, y))) = (size.split_once('x'), pos.split_once('+'))
                else {
                    continue;
                };
                let (Ok(w), Ok(h), Ok(x), Ok(y)) = (
                    w.parse::<u32>(),
                    h.parse::<u32>(),
                    x.parse::<i32>(),
                    y.parse::<i32>(),
                ) else {
                    continue;
                };
                out.push(wallpaper::Output {
                    name: name.to_string(),
                    x,
                    y,
                    width: w,
                    height: h,
                });
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
    }
    let resources = c.randr_get_screen_resources_current(root)?.reply()?;
    let mut out = Vec::new();
    for output in resources.outputs {
        let info = c
            .randr_get_output_info(output, resources.config_timestamp)?
            .reply()?;
        if info.connection != x11rb::protocol::randr::Connection::CONNECTED || info.crtc == 0 {
            continue;
        }
        let crtc = c
            .randr_get_crtc_info(info.crtc, resources.config_timestamp)?
            .reply()?;
        if crtc.width == 0 || crtc.height == 0 {
            continue;
        }
        out.push(wallpaper::Output {
            name: String::from_utf8_lossy(&info.name).to_string(),
            x: i32::from(crtc.x),
            y: i32::from(crtc.y),
            width: u32::from(crtc.width),
            height: u32::from(crtc.height),
        });
    }
    Ok(out)
}

fn wallpaper_update(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).ok_or(USAGE)?;
    let mut document = wallpaper_document()?;
    match sub {
        "set" => {
            let raw = args.get(1).ok_or(USAGE)?;
            let path = wallpaper_expand(raw)
                .canonicalize()
                .map_err(|e| format!("wallpaper not found: {raw}: {e}"))?;
            if !path.is_file() || !wallpaper_is_image(&path) {
                return Err(format!("not a supported image: {raw}").into());
            }
            document["wallpaper"]["enabled"] = toml_edit::value(true);
            document["wallpaper"]["mode"] = toml_edit::value("single");
            document["wallpaper"]["file"] = toml_edit::value(path.display().to_string());
        }
        "mode" => {
            let value = args.get(1).map(String::as_str).ok_or(USAGE)?;
            if !["random", "fixed", "single"].contains(&value) {
                return Err("wallpaper mode must be random, fixed or single".into());
            }
            document["wallpaper"]["enabled"] = toml_edit::value(true);
            document["wallpaper"]["mode"] = toml_edit::value(value);
        }
        "rotation" => {
            let value = args
                .get(1)
                .ok_or(USAGE)?
                .parse::<u32>()
                .map_err(|_| "rotation must be minutes")?;
            if value > 100_000 {
                return Err("rotation is too large".into());
            }
            document["wallpaper"]["enabled"] = toml_edit::value(true);
            document["wallpaper"]["rotation_minutes"] = toml_edit::value(i64::from(value));
        }
        "scale" => {
            let value = args.get(1).map(String::as_str).ok_or(USAGE)?;
            if !["fill", "fit", "stretch", "center"].contains(&value) {
                return Err("wallpaper scale must be fill, fit, stretch or center".into());
            }
            document["wallpaper"]["enabled"] = toml_edit::value(true);
            document["wallpaper"]["scale"] = toml_edit::value(value);
        }
        "follow-theme" => {
            let value = args.get(1).map(String::as_str).ok_or(USAGE)?;
            let enabled = match value {
                "on" => true,
                "off" => false,
                _ => return Err("follow-theme must be on or off".into()),
            };
            document["wallpaper"]["follow_theme"] = toml_edit::value(enabled);
            // Explicitly enabling theme following must actually enable it.
            // A previous manual selection sets mode=single, which otherwise
            // silently overrides follow_theme=true forever. Keep its file so
            // the user can return to it with `wallpaper mode single`.
            if enabled {
                document["wallpaper"]["enabled"] = toml_edit::value(true);
                if document["wallpaper"]["mode"].as_str() == Some("single") {
                    document["wallpaper"]["mode"] = toml_edit::value("fixed");
                }
            }
        }
        _ => return Err(USAGE.into()),
    }
    write_wallpaper_document(&document)?;
    Ok(())
}

fn wallpaper(cmd: &str) -> Result<()> {
    match cmd {
        "list" => wallpaper_list(),
        "status" => wallpaper_status(),
        _ => Err(USAGE.into()),
    }
}

fn current_tag(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
) -> Option<u32> {
    property_u32(c, root, atom("_NET_CURRENT_DESKTOP").ok()?)
}

fn switch_tag(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    workspace: u32,
) -> Result<()> {
    if !valid_workspace(workspace) {
        return Err("autostart workspace must be between 1 and 10".into());
    }
    let tag = workspace - 1;
    send(c, root, atom("_NIRUWM_COMMAND")?, [1, tag, 0, 0, 0])?;
    for _ in 0..50 {
        if current_tag(c, root, atom) == Some(tag) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("window manager did not switch to workspace {workspace}").into())
}

fn client_windows(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
) -> Vec<Window> {
    c.get_property(
        false,
        root,
        atom("_NET_CLIENT_LIST").unwrap_or(0),
        AtomEnum::WINDOW,
        0,
        4096,
    )
    .ok()
    .and_then(|cookie| cookie.reply().ok())
    .and_then(|reply| reply.value32().map(|values| values.collect()))
    .unwrap_or_default()
}

fn window_class(c: &x11rb::rust_connection::RustConnection, win: Window) -> Option<String> {
    let reply = c
        .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    let bytes = reply.value;
    let mut parts = bytes.split(|b| *b == 0).filter(|part| !part.is_empty());
    let instance = String::from_utf8_lossy(parts.next()?).to_ascii_lowercase();
    let class = parts
        .next()
        .map(|part| String::from_utf8_lossy(part).to_ascii_lowercase())
        .unwrap_or_default();
    Some(format!("{instance} {class}"))
}

fn move_window_to_workspace(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    win: Window,
    workspace: u32,
) -> Result<()> {
    let desktop = workspace.saturating_sub(1);
    let event = ClientMessageEvent::new(32, win, atom("_NET_WM_DESKTOP")?, [desktop, 2, 0, 0, 0]);
    c.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        event,
    )?;
    c.flush()?;
    for _ in 0..100 {
        if property_u32(c, win, atom("_NET_WM_DESKTOP")?) == Some(desktop) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("window did not move to workspace {workspace}").into())
}

#[derive(Clone, Debug)]
struct Explanation {
    level: &'static str,
    name: String,
    detail: String,
    action: String,
}

fn running_wm_name(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
) -> String {
    let Some(owner) = property_u32(c, root, atom("_NET_SUPPORTING_WM_CHECK").unwrap_or(0)) else {
        return "none".into();
    };
    let Ok(name_atom) = atom("_NET_WM_NAME") else {
        return "unknown".into();
    };
    c.get_property(
        false,
        owner,
        name_atom,
        atom("UTF8_STRING").unwrap_or(0),
        0,
        128,
    )
    .ok()
    .and_then(|cookie| cookie.reply().ok())
    .map(|reply| String::from_utf8_lossy(&reply.value).to_string())
    .unwrap_or_else(|| "unknown".into())
}

fn installed_program_version(name: &str) -> Option<String> {
    let program = program(name)?;
    let out = command(&program, &["--version".into()]).ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
}

fn explanation_action(token: &str) -> String {
    token.to_string()
}

fn explain(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    launcher: bool,
) -> Result<()> {
    let mut rows = Vec::new();
    let cfg = Config::load()?;
    let running = running_wm_name(c, root, atom);
    let installed = installed_program_version("niruwm").unwrap_or_else(|| "unknown".into());
    if running != format!("NIRUWM {installed}") {
        rows.push(Explanation {
            level: "WARN",
            name: "Session version".into(),
            detail: format!(
                "running {running}; installed NIRUWM {installed}; log out/in to activate"
            ),
            action: explanation_action("doctor"),
        });
    } else {
        rows.push(Explanation {
            level: "OK",
            name: "Session version".into(),
            detail: format!("{running} is current"),
            action: String::new(),
        });
    }

    let native_notify = program("nirunotify")
        .map(|program| command(&program, &["--dnd".into(), "status".into()]).is_ok())
        .unwrap_or(false);
    if native_notify {
        rows.push(Explanation {
            level: "OK",
            name: "Notifications".into(),
            detail: "NIRUNOTIFY owns the session bus".into(),
            action: String::new(),
        });
    } else if which("dunstctl") {
        rows.push(Explanation {
            level: "OK",
            name: "Notifications".into(),
            detail: "Dunst is the active fallback".into(),
            action: String::new(),
        });
    } else {
        rows.push(Explanation {
            level: "WARN",
            name: "Notifications".into(),
            detail: "no native notifier or Dunst responds".into(),
            action: explanation_action("doctor"),
        });
    }

    match audio_nodes("outputs") {
        Ok(nodes) if nodes.is_empty() => rows.push(Explanation {
            level: "WARN",
            name: "Audio".into(),
            detail: "no audio output is available".into(),
            action: explanation_action("audio"),
        }),
        Ok(_) => rows.push(Explanation {
            level: "OK",
            name: "Audio".into(),
            detail: "default output is available".into(),
            action: String::new(),
        }),
        Err(e) => rows.push(Explanation {
            level: "WARN",
            name: "Audio".into(),
            detail: e.to_string(),
            action: explanation_action("doctor"),
        }),
    }

    if backlight_percent(&cfg.brightness.device).is_none() {
        rows.push(Explanation {
            level: "WARN",
            name: "Brightness".into(),
            detail: "no backlight device was found".into(),
            action: explanation_action("doctor"),
        });
    }

    let inhibitor = ["systemd-inhibit", "elogind-inhibit"]
        .iter()
        .find_map(|tool| {
            which(tool).then(|| {
                command(tool, &["--list".into()])
                    .map(|out| String::from_utf8_lossy(&out.stdout).contains("NIRU"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if !inhibitor {
        rows.push(Explanation {
            level: "WARN",
            name: "Sleep protection".into(),
            detail: "the lock-before-suspend inhibitor is not held".into(),
            action: explanation_action("doctor"),
        });
    }

    if let Ok(out) = command("xrandr", &["--query".into()]) {
        let connected = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| line.contains(" connected"))
            .count();
        if connected == 0 {
            rows.push(Explanation {
                level: "WARN",
                name: "Displays".into(),
                detail: "no connected output was reported".into(),
                action: explanation_action("doctor"),
            });
        }
    } else {
        rows.push(Explanation {
            level: "WARN",
            name: "Displays".into(),
            detail: "xrandr is unavailable".into(),
            action: explanation_action("doctor"),
        });
    }

    if cfg.weather_latitude == 0.0 && cfg.weather_longitude == 0.0 {
        rows.push(Explanation {
            level: "WARN",
            name: "Weather".into(),
            detail: "latitude/longitude are 0,0; weather stays unavailable".into(),
            action: explanation_action("doctor"),
        });
    }

    if launcher {
        let mut emitted = 0;
        for row in &rows {
            if row.level == "OK" {
                continue;
            }
            println!(
                "{}\t{}\t{}\t{}",
                row.level, row.name, row.detail, row.action
            );
            emitted += 1;
        }
        if emitted == 0 {
            println!("OK\tCore services\tall checked services respond\t\t");
        }
    } else {
        for row in &rows {
            println!(
                "{:<4} {:<18} {}{}",
                row.level,
                row.name,
                row.detail,
                if row.action.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", row.action)
                }
            );
        }
    }
    Ok(())
}

fn valid_workspace(workspace: u32) -> bool {
    (1..=10).contains(&workspace)
}

fn launch_sequence(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    apps: &[config::AutostartApp],
    settle_seconds: u32,
    window_timeout_ms: u64,
    return_to: Option<u32>,
) -> Result<()> {
    let mut apps = apps.to_vec();
    apps.sort_by_key(|app| app.workspace);
    let mut index = 0;
    let mut last_workspace = 0u32;
    let mut user_moved = false;
    while index < apps.len() && !user_moved {
        let workspace = apps[index].workspace;
        if !valid_workspace(workspace) {
            eprintln!("niructl: autostart workspace {workspace} is outside 1..10");
            while index < apps.len() && apps[index].workspace == workspace {
                index += 1;
            }
            continue;
        }
        switch_tag(c, root, atom, workspace)?;
        if current_tag(c, root, atom) != Some(workspace - 1) {
            user_moved = true;
            break;
        }
        last_workspace = workspace;
        while index < apps.len() && apps[index].workspace == workspace {
            let app = &apps[index];
            let command = &app.command;
            if let Some((program, arguments)) = command.split_first() {
                let before: HashSet<Window> = client_windows(c, root, atom).into_iter().collect();
                match std::process::Command::new(program).args(arguments).spawn() {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("niructl: autostart {program}: {e}");
                        index += 1;
                        continue;
                    }
                }
                if let Some(wanted) = app.class.as_deref().filter(|wanted| !wanted.is_empty()) {
                    let wanted = wanted.to_ascii_lowercase();
                    let deadline = Instant::now() + Duration::from_millis(window_timeout_ms);
                    let mut placed = false;
                    while Instant::now() < deadline {
                        if current_tag(c, root, atom) != Some(workspace - 1) {
                            user_moved = true;
                            break;
                        }
                        for win in client_windows(c, root, atom) {
                            if before.contains(&win) {
                                continue;
                            }
                            if !window_class(c, win)
                                .map(|class| class.contains(&wanted))
                                .unwrap_or(false)
                            {
                                continue;
                            }
                            move_window_to_workspace(c, root, atom, win, workspace)?;
                            placed = true;
                            break;
                        }
                        if placed {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    if !placed && !user_moved {
                        eprintln!(
                            "niructl: autostart {program}: no window matching {wanted:?} within {window_timeout_ms} ms"
                        );
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
            index += 1;
        }
        if user_moved {
            break;
        }
        if settle_seconds > 0 {
            std::thread::sleep(Duration::from_secs(u64::from(settle_seconds)));
        }
    }

    if !user_moved {
        if let Some(return_to) = return_to.filter(|workspace| *workspace > 0) {
            if current_tag(c, root, atom) == Some(last_workspace.saturating_sub(1)) {
                switch_tag(c, root, atom, return_to)?;
            }
        }
    }
    Ok(())
}

fn autostart(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
) -> Result<()> {
    let cfg = Config::load()?;
    if !cfg.autostart.enabled || cfg.autostart.apps.is_empty() {
        return Ok(());
    }
    if cfg.restore_session || !session_state::load().is_empty() {
        return Ok(());
    }
    launch_sequence(
        c,
        root,
        atom,
        &cfg.autostart.apps,
        cfg.autostart.settle_seconds,
        cfg.autostart.window_timeout_ms,
        (cfg.autostart.return_workspace > 0).then_some(cfg.autostart.return_workspace),
    )
}

fn recipe(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    args: &[String],
) -> Result<()> {
    let cfg = Config::load()?;
    let action = args.first().map(String::as_str).unwrap_or("list");
    match action {
        "list" => {
            for recipe in &cfg.recipes {
                println!("{}\t{}\tworkspace recipe", recipe.name, recipe.apps.len());
            }
        }
        "run" => {
            let name = args.get(1).ok_or("recipe run needs a name")?;
            let recipe = cfg
                .recipes
                .iter()
                .find(|recipe| recipe.name == *name)
                .ok_or_else(|| format!("unknown recipe: {name}"))?;
            let return_to = current_tag(c, root, atom).map(|tag| tag + 1);
            launch_sequence(
                c,
                root,
                atom,
                &recipe.apps,
                0,
                cfg.autostart.window_timeout_ms,
                return_to,
            )?;
        }
        _ => return Err("recipe needs list or run NAME".into()),
    }
    Ok(())
}

fn dnd_is_on() -> Option<bool> {
    if let Some(notifier) = program("nirunotify") {
        if let Ok(out) = command(&notifier, &["--dnd".into(), "status".into()]) {
            let text = String::from_utf8_lossy(&out.stdout);
            return Some(text.trim().ends_with("on"));
        }
    }
    if which("dunstctl") {
        let out = command("dunstctl", &["is-paused".into()]).ok()?;
        return Some(String::from_utf8_lossy(&out.stdout).trim() == "true");
    }
    None
}

fn dnd_apply(on: bool) -> Result<()> {
    if let Some(notifier) = program("nirunotify") {
        if command(
            &notifier,
            &["--dnd".into(), if on { "on" } else { "off" }.into()],
        )
        .is_ok()
        {
            return Ok(());
        }
    }
    if which("dunstctl") {
        command(
            "dunstctl",
            &[
                "set-paused".into(),
                if on { "true" } else { "false" }.into(),
            ],
        )?;
        return Ok(());
    }
    Err("no supported notification provider".into())
}

fn mode_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
    base.join("niruwm/mode")
}

#[derive(Clone, Debug)]
struct ModeState {
    name: String,
    expiry: u64,
    token: String,
    dnd_initial_off: bool,
    idle_set_until: Option<u64>,
}

fn read_mode() -> Option<ModeState> {
    let text = std::fs::read_to_string(mode_path()).ok()?;
    let mut name = None;
    let mut expiry = None;
    let mut token = None;
    let mut dnd_initial_off = None;
    let mut idle_set_until = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "name" => name = Some(value.to_string()),
            "expiry" => expiry = value.parse::<u64>().ok(),
            "token" => token = Some(value.to_string()),
            "dnd_initial_off" => dnd_initial_off = Some(value == "1"),
            "idle_set_until" => idle_set_until = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    Some(ModeState {
        name: name?,
        expiry: expiry?,
        token: token?,
        dnd_initial_off: dnd_initial_off?,
        idle_set_until,
    })
}

fn write_mode(state: &ModeState) -> Result<()> {
    let path = mode_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("new");
    std::fs::write(
        &temp,
        format!(
            "name={}\nexpiry={}\ntoken={}\ndnd_initial_off={}\nidle_set_until={}\n",
            state.name,
            state.expiry,
            state.token,
            u8::from(state.dnd_initial_off),
            state
                .idle_set_until
                .map(|v| v.to_string())
                .unwrap_or_default()
        ),
    )?;
    std::fs::rename(temp, path)?;
    Ok(())
}

fn mode_notify(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "NIRUX", "-t", "6000", summary, body])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn mode_stop(quiet: bool) -> Result<()> {
    let Some(state) = read_mode() else {
        if !quiet {
            println!("no active work mode");
        }
        return Ok(());
    };
    if state.dnd_initial_off {
        let _ = dnd_apply(false);
    }
    if let Some(until) = state.idle_set_until {
        if idle_until() == Some(until) {
            set_idle(None)?;
            xset(&["s", "default", "+dpms"]);
        }
    }
    let _ = std::fs::remove_file(mode_path());
    if !quiet {
        println!("{} mode stopped", state.name);
        mode_notify("Work mode ended", &format!("{} restored", state.name));
    }
    Ok(())
}

fn mode(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("status");
    match action {
        "focus" | "presentation" => {
            let default_minutes = if action == "focus" { 25 } else { 60 };
            let minutes = match args.get(1) {
                Some(value) => value
                    .parse::<u64>()
                    .map_err(|_| "mode minutes must be a number")?
                    .clamp(1, 480),
                None => default_minutes,
            };
            mode_stop(true)?;
            let expiry = now_secs() + minutes * 60;
            let dnd_initial_off = dnd_is_on() != Some(true);
            if dnd_initial_off {
                dnd_apply(true)?;
            }
            let idle_set_until = if idle_until().is_none() {
                let until = now_secs() + minutes * 60;
                set_idle(Some(until))?;
                xset(&["s", "off", "-dpms"]);
                Some(until)
            } else {
                None
            };
            let token = format!("{}-{}", now_secs(), std::process::id());
            write_mode(&ModeState {
                name: action.into(),
                expiry,
                token: token.clone(),
                dnd_initial_off,
                idle_set_until,
            })?;
            println!(
                "{} mode for {minutes} min, until {}",
                action,
                Local
                    .timestamp_opt(expiry as i64, 0)
                    .single()
                    .map(|t| t.format("%H:%M").to_string())
                    .unwrap_or_else(|| "?".into())
            );
            mode_notify(
                if action == "focus" {
                    "Focus mode"
                } else {
                    "Presentation mode"
                },
                &format!("DND and keep-awake for {minutes} min"),
            );
            let exe = std::env::current_exe()?;
            let _ = std::process::Command::new(exe)
                .args(["mode", "expire", &token])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        "stop" => {
            mode_stop(args.iter().any(|arg| arg == "--quiet"))?;
        }
        "status" => match read_mode() {
            Some(state) if state.expiry > now_secs() => {
                let left = (state.expiry - now_secs()).div_ceil(60);
                println!("{} mode, {left} min remaining", state.name);
            }
            Some(_) => {
                mode_stop(true)?;
                println!("mode off");
            }
            None => println!("mode off"),
        },
        "expire" => {
            let token = args.get(1).ok_or("mode expire needs a token")?;
            let Some(state) = read_mode() else {
                return Ok(());
            };
            if state.token != *token {
                return Ok(());
            }
            let wait = state.expiry.saturating_sub(now_secs());
            if wait > 0 {
                std::thread::sleep(Duration::from_secs(wait));
            }
            let Some(state) = read_mode() else {
                return Ok(());
            };
            if state.token == *token {
                mode_stop(false)?;
            }
        }
        _ => return Err("mode needs focus, presentation, stop or status".into()),
    }
    Ok(())
}

fn expand_user_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        return PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest);
    }
    PathBuf::from(path)
}

fn offline(cmd: &str) -> Result<()> {
    // The session file is a plain list, so these work without a window
    // manager too: useful from the console before starting a session.
    if cmd == "save-session" || cmd == "clear-session" {
        return session_from_file(cmd);
    }
    if cmd == "brightness" {
        return brightness(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "audio" {
        return audio(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "bluetooth" {
        return bluetooth(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "display" {
        return display(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "powerprofile" {
        return powerprofile(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "mode" {
        return mode(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "monitors" {
        // A read-only report that needs no window manager, and must not stop
        // a script when there is no display at all.
        if std::env::var_os("DISPLAY").is_none() {
            println!("No display is set; start a session to list screens with niructl monitors.");
            return Ok(());
        }
        return monitors();
    }
    if cmd == "nightlight" {
        return nightlight(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "compositor" {
        return compositor(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "idle" {
        return idle(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "timing" {
        return timing(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if matches!(
        cmd,
        "lock" | "screensaver" | "logout" | "suspend" | "reboot" | "shutdown" | "dnd"
    ) {
        return desktop(cmd, &std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "keys" {
        let cfg = Config::load()?;
        for binding in cfg.effective_keys() {
            if binding.action == "unbind" {
                continue;
            }
            println!("{}\t{}", binding.keys, binding.action);
        }
        return Ok(());
    }
    if cmd == "layout" {
        // `show` reports the stored layouts; there is nothing to ask a
        // display-less client. `save` asks the running manager, which is the
        // only process that knows each workspace's current layout. Any other
        // word is a layout name and is handled below, like `niructl layout
        // monocle`.
        let rest: Vec<String> = std::env::args().skip(2).collect();
        match rest.first().map(String::as_str) {
            None | Some("show") => return layout_show(),
            Some("save") => return layout_save(),
            // Any other word is a layout name. It goes through the same
            // command as Super+T and friends, so the two can never diverge.
            Some(name) => {
                let layout = bindings::LayoutId::from_name(name)
                    .ok_or_else(|| format!("unknown layout {name}; see niructl layouts"))?;
                let (c, root) = connect_for_change()?;
                let atom = |n: &str| -> Result<Atom> {
                    Ok(c.intern_atom(false, n.as_bytes())?.reply()?.atom)
                };
                send(&c, root, atom(CMD_ATOM)?, [3, layout.code(), 0, 0, 0])?;
                // Report the layout the manager now shows, once it has applied
                // it; the published property is the confirmation.
                let deadline = std::time::Instant::now() + Duration::from_millis(500);
                loop {
                    let shown = c
                        .get_property(
                            false,
                            root,
                            atom("_NIRUWM_LAYOUT")?,
                            atom("UTF8_STRING")?,
                            0,
                            64,
                        )
                        .ok()
                        .and_then(|r| r.reply().ok())
                        .map(|r| String::from_utf8_lossy(&r.value).to_string())
                        .unwrap_or_default();
                    if shown == name {
                        return Ok(());
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(format!("the window manager did not switch to {name}").into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
    if cmd == "layouts" {
        // Print the layouts in the order Super+Ctrl+period cycles them, with
        // the key that selects each one directly.
        let keys = [
            ("tile", "Super+T"),
            ("left", "Super+Y"),
            ("monocle", "Super+M"),
            ("centered", "Super+C"),
            ("centeredfloat", "Super+U"),
            ("fibonacci", "Super+Ctrl+E"),
            ("floating", "Super+F"),
        ];
        println!("NIRUWM layouts, in cycling order:");
        for l in layout::Layout::all() {
            let key = keys
                .iter()
                .find(|(n, _)| *n == l.name())
                .map(|(_, k)| *k)
                .unwrap_or("-");
            println!("  {:<14} {:<12} {}", l.name(), key, l.describe());
        }
        println!();
        println!("Change with the key above, or: niructl layout NAME");
        println!("Cycle with Super+Ctrl+period and Super+Ctrl+comma.");
        return Ok(());
    }
    if cmd == "theme-catalog" {
        let catalog = config::theme_catalog();
        println!("{}", serde_json::to_string_pretty(&catalog)?);
        return Ok(());
    }
    if cmd == "theme-import" {
        let rest = std::env::args().skip(2).collect::<Vec<_>>();
        let mut source = None;
        let mut id = None;
        let mut wallpaper = None;
        let mut replace = false;
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--id" => {
                    i += 1;
                    id = rest.get(i).cloned();
                    if id.is_none() {
                        return Err("theme-import --id needs a name".into());
                    }
                }
                "--wallpaper" => {
                    i += 1;
                    wallpaper = rest.get(i).cloned();
                    if wallpaper.is_none() {
                        return Err("theme-import --wallpaper needs a file".into());
                    }
                }
                "--replace" => replace = true,
                value if source.is_none() => source = Some(value.to_string()),
                other => return Err(format!("unknown theme-import argument: {other}").into()),
            }
            i += 1;
        }
        let source = source.ok_or("theme-import needs a theme file")?;
        let source = expand_user_path(&source);
        let wallpaper = wallpaper.as_deref().map(expand_user_path);
        let imported = config::theme_import(&source, id.as_deref(), wallpaper.as_deref(), replace)?;
        println!("imported theme {imported}");
        return Ok(());
    }
    if cmd == "theme-remove" {
        let name = std::env::args()
            .nth(2)
            .ok_or("theme-remove needs a theme name")?;
        config::theme_remove(&name)?;
        println!("removed theme {name}");
        return Ok(());
    }
    if cmd == "theme-hide" || cmd == "theme-show" {
        let name = std::env::args()
            .nth(2)
            .ok_or("theme-hide/show needs a theme name")?;
        config::theme_hide(&name, cmd == "theme-hide")?;
        println!(
            "{} theme {name}",
            if cmd == "theme-hide" { "hid" } else { "shown" }
        );
        return Ok(());
    }
    if cmd == "themes" {
        let catalog = config::theme_catalog();
        println!("NIRUWM themes:");
        for theme in &catalog.themes {
            let active = if theme.id == catalog.active { "*" } else { " " };
            let hidden = if theme.hidden { " hidden" } else { "" };
            let origin = if theme.overridden {
                "built-in override"
            } else if theme.builtin {
                "built-in"
            } else {
                "user"
            };
            let wallpaper = if theme.wallpaper_available {
                ""
            } else {
                " no wallpaper"
            };
            match &theme.colors {
                Some(colors) => println!(
                    "{active} {:<18} {:<20} {:<16}{hidden}{wallpaper}",
                    theme.id, colors.name, origin
                ),
                None => println!(
                    "{active} {:<18} {:<20} {:<16} invalid: {}{hidden}{wallpaper}",
                    theme.id,
                    theme.name,
                    origin,
                    theme.error.as_deref().unwrap_or("unknown error")
                ),
            }
        }
        println!();
        println!("Apply with: niructl theme NAME");
        println!("Inspect with: niructl theme-catalog --json");
        return Ok(());
    }
    Ok(())
}

/// The layouts stored in the shared configuration.
fn layout_show() -> Result<()> {
    let c = Config::load()?;
    let dir = config::directory().join("config.toml");
    println!("config: {}", dir.display());
    println!(
        "common: {}",
        if c.monitors.layouts.is_empty() {
            "(every workspace tiled)".to_string()
        } else {
            c.monitors.layouts.join(", ")
        }
    );
    for p in &c.monitors.profiles {
        if !p.layouts.is_empty() {
            println!("{}: {}", p.name, p.layouts.join(", "));
        }
    }
    println!();
    println!("Each workspace keeps its own layout. Change the focused one with");
    println!("Super+T/Y/M/C/U/F, then save with: niructl layout save");
    Ok(())
}

/// Ask the running manager to write the layouts it is using.
fn layout_save() -> Result<()> {
    let (c, root) = connect_for_change()?;
    let atom = |n: &str| -> Result<Atom> { Ok(c.intern_atom(false, n.as_bytes())?.reply()?.atom) };
    let serial = command_serial();
    send(&c, root, atom(CMD_ATOM)?, [10, serial, 0, 0, 0])?;
    c.flush()?;
    // The manager acknowledges, so the command reports what happened instead
    // of assuming it worked.
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    loop {
        if property_u32(&c, root, atom("_NIRUWM_LAYOUT_SAVE_ACK")?) == Some(serial) {
            println!("saved the current workspace layouts");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("the running NIRUWM did not save the layouts".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(cmd) = args.first() else {
        return Err(USAGE.into());
    };
    // Listing themes and layouts reads only local files, so it works without
    // a display: people often run it over ssh or before X is up.
    if matches!(
        cmd.as_str(),
        "themes"
            | "theme-catalog"
            | "theme-import"
            | "theme-remove"
            | "theme-hide"
            | "theme-show"
            | "layouts"
            | "keys"
            | "layout"
            | "brightness"
            | "nightlight"
            | "compositor"
            | "lock"
            | "screensaver"
            | "logout"
            | "suspend"
            | "reboot"
            | "shutdown"
            | "dnd"
            | "audio"
            | "bluetooth"
            | "display"
            | "powerprofile"
            | "mode"
            | "idle"
            | "timing"
            | "monitors"
    ) {
        return offline(cmd);
    }
    if cmd == "wallpaper" {
        let sub = args.get(1).map(String::as_str).unwrap_or("");
        if sub == "list" || sub == "status" {
            return wallpaper(sub);
        }
        if sub == "status-json" {
            return wallpaper_status_json();
        }
        if sub == "output" {
            return wallpaper_output(&args[2..]);
        }
    }
    // Repair the configuration before touching X: a truncated file must be
    // fixed even when the theme cannot be applied (no display, WM not
    // running), otherwise the broken state survives until an unrelated
    // reinstall happens to run.
    if cmd == "theme" {
        let name = args.get(1).ok_or(USAGE)?;
        Theme::load(name)?;
        write_theme(name)?;
    }
    let (c, s) = x11rb::connect(None)?;
    let root = c.setup().roots[s].root;
    let atom = |n: &str| -> Result<Atom> { Ok(c.intern_atom(false, n.as_bytes())?.reply()?.atom) };

    if cmd == "restart-bar" {
        let owner = property_u32(&c, root, atom("_NET_SUPPORTING_WM_CHECK")?)
            .ok_or("no window manager is running")?;
        if property_u32(&c, owner, atom("_NIRUWM_BAR_CONTROL")?) != Some(1) {
            return Err(
                "this running WM cannot restart bars; log out/in to activate the updated session"
                    .into(),
            );
        }
        let serial = std::process::id();
        send(&c, root, atom(CMD_ATOM)?, [8, serial, 0, 0, 0])?;
        c.flush()?;
        for _ in 0..100 {
            if property_u32(&c, root, atom("_NIRUWM_BAR_RESTART_SERIAL")?) == Some(serial) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        return Err("window manager did not acknowledge bar restart".into());
    }

    if cmd == "status" {
        let tag = property_u32(&c, root, atom("_NET_CURRENT_DESKTOP")?).unwrap_or(0);
        let check = property_u32(&c, root, atom("_NET_SUPPORTING_WM_CHECK")?);
        let wm = check
            .and_then(|w| {
                c.get_property(
                    false,
                    w,
                    atom("_NET_WM_NAME").ok()?,
                    atom("UTF8_STRING").ok()?,
                    0,
                    128,
                )
                .ok()?
                .reply()
                .ok()
            })
            .map(|r| String::from_utf8_lossy(&r.value).to_string())
            .unwrap_or_else(|| "none".into());
        let layout = c
            .get_property(
                false,
                root,
                atom("_NIRUWM_LAYOUT")?,
                atom("UTF8_STRING")?,
                0,
                64,
            )
            .ok()
            .and_then(|r| r.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.value).trim().to_string())
            .unwrap_or_else(|| "tile".into());
        println!(
            "wm={} tag={} theme={} layout={}",
            wm,
            tag + 1,
            Theme::load(&Config::load()?.theme)?.name,
            layout
        );
        println!(
            "brightness={}",
            backlight_percent(&Config::load()?.brightness.device)
                .map(|p| format!("{p}%"))
                .unwrap_or_else(|| "unavailable".into())
        );
        println!(
            "nightlight={}",
            if nightlight_state().exists() {
                "on"
            } else {
                "off"
            }
        );
        return Ok(());
    }

    if cmd == "explain" {
        return explain(&c, root, &atom, args.iter().any(|arg| arg == "--launcher"));
    }
    if cmd == "recipe" {
        return recipe(&c, root, &atom, &args[1..]);
    }
    if cmd == "autostart" {
        return autostart(&c, root, &atom);
    }

    let (action, value) = match cmd.as_str() {
        "tag" => (1, parse_tag(&args)?),
        "movetotag" => (2, parse_tag(&args)?),
        // `niructl layout show|save` is handled before the display is opened;
        // this is the one-word `niructl layout NAME` form that sets a layout.
        "layout" => (
            3,
            match args.get(1).map(String::as_str) {
                Some("tile") => 0,
                Some("monocle") => 1,
                Some("left") => 2,
                Some("centered") => 3,
                Some("centeredfloat") => 6,
                Some("fibonacci") => 4,
                Some("floating") => 5,
                Some(other) => {
                    return Err(format!(
                        "layout {other}: use show, save, or one of tile, left, monocle, centered, centeredfloat, fibonacci, floating"
                    )
                    .into())
                }
                None => return Err("layout needs a name; see niructl layouts".into()),
            },
        ),
        "reload" => (4, 0),
        // Save now: the same action as Super+Shift+W and the session end.
        "save" => (6, 0),
        "quit" => (5, 0),
        // Remember what is open, or start clean next time. Sent to the window
        // manager so the session file describes the real screens and tags.
        "save-session" => (6, 0),
        "clear-session" => (7, 0),
        "theme" => {
            // Already validated and written above, before the X connection.
            let _ = args.get(1).ok_or(USAGE)?;
            (4, 0)
        }
        "wallpaper" => {
            if args.get(1).map(String::as_str) == Some("apply") {
                return wallpaper_apply(&args[2..]).map_err(|e| e.to_string().into());
            }
            wallpaper_update(&args[1..])?;
            (4, 0)
        }
        _ => return Err(USAGE.into()),
    };

    if cmd == "quit" {
        // Exit the WM cleanly: the session script then tears down helpers.
        send(&c, root, atom(CMD_ATOM)?, [action, value, 0, 0, 0])?;
    } else if cmd == "reload" || cmd == "theme" || cmd == "wallpaper" {
        // One serial for both the property the bar watches and the
        // acknowledgement the window manager publishes. A fixed sleep could
        // report success before the configuration was even parsed.
        let serial = command_serial();
        send(&c, root, atom(CMD_ATOM)?, [action, serial, 0, 0, 0])?;
        c.change_property32(
            PropMode::REPLACE,
            root,
            atom(CONFIG_SERIAL)?,
            AtomEnum::CARDINAL,
            &[serial],
        )?;
        c.flush()?;
        if property_u32(&c, root, atom("_NIRUWM_ACK_CONTROL")?) == Some(1) {
            wait_reload_ack(&c, root, &atom, serial)?;
            if cmd == "theme" {
                verify_theme(&c, root, &atom, args.get(1).ok_or(USAGE)?)?;
            }
        } else {
            // A manager from before the acknowledgement property still
            // accepts the command. Keep the old short pause for that running
            // session and say clearly that it is not a confirmed result.
            std::thread::sleep(std::time::Duration::from_millis(30));
            eprintln!(
                "niructl: the running window manager cannot confirm reload; \
                 log out/in to activate NIRUWM 0.16.9"
            );
        }
        return Ok(());
    } else {
        send(&c, root, atom(CMD_ATOM)?, [action, value, 0, 0, 0])?;
    }
    c.flush()?;

    // Wait until the window manager has applied the command. Without this a
    // script that runs `niructl tag 3` and then reads the property races the
    // WM; with it, `niructl` is synchronous like `wmctrl -s`.
    if action != 5 {
        wait_applied(&c, root, &atom, action, value)?;
    }
    Ok(())
}

/// A non-zero, 31-bit serial so the high bit stays free for "rejected".
fn command_serial() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u32)
        .unwrap_or(0);
    let serial = (nanos ^ std::process::id()) & 0x7fff_ffff;
    serial.max(1)
}

/// Wait for the window manager to publish the result of this exact reload.
///
/// `_NIRUWM_CONFIG_ACK` carries the serial on success and the same serial with
/// the high bit set when the configuration was rejected and the previous state
/// is still active.
fn wait_reload_ack(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    serial: u32,
) -> Result<()> {
    let ack = atom("_NIRUWM_CONFIG_ACK")?;
    for _ in 0..200 {
        if let Some(value) = property_u32(c, root, ack) {
            if value & 0x7fff_ffff == serial {
                if value & 0x8000_0000 != 0 {
                    return Err(
                        "window manager rejected the configuration; the previous state is still active"
                            .into(),
                    );
                }
                return Ok(());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err("window manager did not acknowledge the reload".into())
}

/// Confirm that the theme in effect is the one that was requested.
fn verify_theme(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    want: &str,
) -> Result<()> {
    let reply = c
        .get_property(
            false,
            root,
            atom("_NIRUWM_THEME")?,
            atom("UTF8_STRING")?,
            0,
            128,
        )?
        .reply()?;
    let got = String::from_utf8_lossy(&reply.value).to_string();
    if got != want {
        return Err(format!("window manager reports theme {got}, expected {want}").into());
    }
    Ok(())
}

/// Poll the property the command changes until it reflects the request.
fn wait_applied(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    action: u32,
    value: u32,
) -> Result<()> {
    let check: Option<(Atom, u32)> = match action {
        1 => Some((atom("_NET_CURRENT_DESKTOP")?, value)),
        _ => None,
    };
    let Some((prop, want)) = check else {
        // Layout has no single readable property. Reload and theme no longer
        // take this path: both wait for `_NIRUWM_CONFIG_ACK`.
        std::thread::sleep(std::time::Duration::from_millis(30));
        return Ok(());
    };
    std::thread::sleep(std::time::Duration::from_millis(5));
    for _ in 0..100 {
        if property_u32(c, root, prop) == Some(want) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err("window manager did not apply the command".into())
}

/// Read the saved session, or forget it, without a window manager.
///
/// Saving needs the window manager (it knows screens and workspaces), so this
/// only reports and clears: the honest half of the pair when there is nothing
/// running to describe.
fn session_from_file(cmd: &str) -> Result<()> {
    let path = crate::session_state::path();
    if cmd == "clear-session" {
        crate::session_state::clear()?;
        println!("saved session cleared: {}", path.display());
        return Ok(());
    }
    let entries = crate::session_state::load();
    if entries.is_empty() {
        println!("no saved session ({})", path.display());
        return Ok(());
    }
    println!("{} window(s) saved in {}", entries.len(), path.display());
    for e in entries {
        println!(
            "  screen {}  workspace {}  {}{}",
            e.monitor + 1,
            e.tag + 1,
            e.program.join(" "),
            if e.floating { "  (floating)" } else { "" }
        );
    }
    Ok(())
}

fn parse_tag(args: &[String]) -> Result<u32> {
    let n: u32 = args.get(1).ok_or(USAGE)?.parse()?;
    if !(1..=10).contains(&n) {
        return Err("tag must be 1..10".into());
    }
    Ok(n - 1)
}

/// Send a command to the window manager.
///
/// The message goes to NIRUWM's own check window (advertised through
/// `_NET_SUPPORTING_WM_CHECK`) with `NoEvent`. Sending to the root with
/// SUBSTRUCTURE_REDIRECT is the EWMH convention, but the manager selects many
/// masks on the root and X may deliver one message once per matching mask;
/// a duplicate `quit` left the process stuck in its event loop. Addressing the
/// manager directly is unambiguous: one message, one recipient.
///
/// When there is no window manager (a bar on its own, a session still
/// starting), the message goes to the root window instead. The bar listens
/// there for the two commands that concern it, `reload` and `theme`, and
/// ignores the rest. Without this a bar without a window manager could not be
/// told to reload, and said "no window manager is running" for a command it
/// was the one to carry out.
fn send(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: Atom,
    data: [u32; 5],
) -> Result<()> {
    let check = property_u32(c, root, atom_named(c, "_NET_SUPPORTING_WM_CHECK")?);
    let target = check.unwrap_or(root);
    c.send_event(
        false,
        target,
        EventMask::NO_EVENT,
        ClientMessageEvent::new(32, target, atom, data),
    )?;
    // Round-trip before returning. Without this the process could exit with
    // the request still buffered in the X connection, and the command was
    // silently lost — about one login in five lost `quit`.
    c.sync()?;
    Ok(())
}

fn atom_named(c: &x11rb::rust_connection::RustConnection, name: &str) -> Result<Atom> {
    Ok(c.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

fn property_u32(c: &x11rb::rust_connection::RustConnection, w: Window, atom: Atom) -> Option<u32> {
    c.get_property(false, w, atom, AtomEnum::ANY, 0, 1)
        .ok()?
        .reply()
        .ok()?
        .value32()?
        .next()
}

/// Top-level sections every config must keep. A file written before a section
/// existed, or truncated by an editor, would otherwise stay incomplete: the
/// built-in defaults then silently apply, and `[wallpaper] enabled` falls back
/// to false so theme wallpapers stop working with no message anywhere.
///
/// `[[keys]]` is intentionally absent: it is an array of tables that defaults
/// to the standard bindings, so an empty or missing list is valid.
const REQUIRED_SECTIONS: &[&str] = &[
    "wallpaper",
    "autostart",
    "bar",
    "nightlight",
    "brightness",
    "monitors",
    "idle",
];

/// Persist a theme choice without destroying the rest of the configuration.
///
/// The theme is the only value changed. Missing sections are added from the
/// shipped example, so a short or truncated file is repaired instead of being
/// preserved in its broken form, and invalid TOML is refused rather than
/// overwritten. A backup of the previous file is kept next to it.
fn write_theme(name: &str) -> Result<()> {
    let dir = config::directory();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    let original = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };

    // Neither the file nor its sections may be born invalid: refuse to replace
    // something a person wrote, because the error still describes their text.
    let mut value: toml::Value = if original.trim().is_empty() {
        toml::Value::Table(Default::default())
    } else {
        original
            .parse()
            .map_err(|e| format!("refusing to rewrite invalid {}: {e}", path.display()))?
    };
    if value.as_table().is_none() {
        return Err(format!("refusing to rewrite {}: expected a table", path.display()).into());
    }

    let missing = ensure_sections(&mut value);

    // A complete file is edited in place: re-serialising it would drop every
    // comment, and those comments are the documentation of the options.
    // A repaired file starts from the example so it keeps that documentation
    // too, with the user's own values laid over it.
    let rendered = if missing.is_empty() && !original.trim().is_empty() {
        replace_theme_line(&original, name)?
    } else {
        value
            .as_table_mut()
            .ok_or("Invalid config: expected a table")?
            .insert("theme".into(), toml::Value::String(name.into()));
        match example_text() {
            Some(example) => merge_into_example(&example, &value),
            None => toml::to_string_pretty(&value)?,
        }
    };
    if rendered == original {
        return Ok(());
    }
    if !original.is_empty() {
        let backup = dir.join("config.toml.bak");
        let _ = std::fs::write(&backup, &original);
    }
    let temp = dir.join(format!(".config.toml.{}", std::process::id()));
    std::fs::write(&temp, &rendered)?;
    std::fs::rename(&temp, &path)?;
    if !missing.is_empty() {
        eprintln!(
            "niructl: repaired {} (added {})",
            path.display(),
            missing.join(", ")
        );
    }
    Ok(())
}

/// Write the user's values into the shipped example, which carries the comments
/// that document every option. Values absent from the example are appended at
/// the end of their section.
fn merge_into_example(example: &str, value: &toml::Value) -> String {
    let mut out = String::with_capacity(example.len());
    let mut section = String::new();
    let mut seen: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();

    for line in example.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = body.trim_start();
        if let Some(rest) = trimmed.strip_prefix('[') {
            if let Some(name) = rest.strip_suffix(']') {
                if !name.starts_with('[') {
                    // Leaving a section: append its extra keys before the new one.
                    if !section.is_empty() {
                        append_extra_keys(&mut out, &section, value, &seen);
                    }
                    section = name.to_string();
                }
            }
        }
        // A commented-out key documents a default; never resurrect it.
        let key = if !trimmed.starts_with('#') && !trimmed.starts_with('[') {
            trimmed
                .split('=')
                .next()
                .map(str::trim)
                .filter(|k| !k.is_empty())
        } else {
            None
        };
        // Keep a trailing ` # comment` on a rewritten line so the example's
        // annotation stays attached to its value. Only " #" counts, so a '#'
        // inside a quoted value is not mistaken for a comment.
        let tail = body
            .find(" #")
            .map(|i| format!(" {}", body[i + 1..].trim()))
            .unwrap_or_default();
        match key {
            Some(k) if section.is_empty() => {
                if let Some(v) = value.get(k) {
                    out.push_str(&format!("{k} = {}{tail}\n", inline_value(v)));
                    seen.insert((section.clone(), k.to_string()));
                    continue;
                }
            }
            Some(k) => {
                if let Some(s) = value.get(&section).and_then(|s| s.get(k)) {
                    out.push_str(&format!("{k} = {}{tail}\n", inline_value(s)));
                    seen.insert((section.clone(), k.to_string()));
                    continue;
                }
            }
            None => {}
        }
        out.push_str(line);
    }
    if !section.is_empty() {
        append_extra_keys(&mut out, &section, value, &seen);
    }

    out
}

/// Append keys of `section` that the example does not mention.
fn append_extra_keys(
    out: &mut String,
    section: &str,
    value: &toml::Value,
    seen: &std::collections::BTreeSet<(String, String)>,
) {
    let Some(table) = value.get(section).and_then(|s| s.as_table()) else {
        return;
    };
    for (key, v) in table {
        if seen.contains(&(section.to_string(), key.clone())) {
            continue;
        }
        out.push_str(&format!("{key} = {}\n", inline_value(v)));
    }
}

/// Render a scalar or array on one line, the way the example writes them.
fn inline_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("{:?}", s),
        toml::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(inline_value).collect();
            format!("[{}]", parts.join(", "))
        }
        other => other.to_string(),
    }
}

/// Set the top-level `theme` line, keeping every other line and comment as it
/// was. A missing line is added at the top, where it reads best.
fn replace_theme_line(text: &str, name: &str) -> Result<String> {
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    let line = format!("theme = \"{escaped}\"");
    let mut out = String::with_capacity(text.len() + line.len());
    let mut replaced = false;
    let mut in_section = false;
    // split_inclusive keeps the file's own line endings, including whether it
    // ends with a newline, so an untouched line stays byte-for-byte identical.
    for original in text.split_inclusive('\n') {
        let body = original.strip_suffix('\n').unwrap_or(original);
        let trimmed = body.trim_start();
        if trimmed.starts_with('[') {
            in_section = true;
        }
        // Only the top-level key counts: [wallpaper] has no `theme`, but a
        // future section must not be mistaken for the theme line.
        if !in_section
            && !replaced
            && !trimmed.starts_with('#')
            && trimmed.split('=').next().map(str::trim) == Some("theme")
        {
            out.push_str(&line);
            if original.ends_with('\n') {
                out.push('\n');
            }
            replaced = true;
            continue;
        }
        out.push_str(original);
    }
    if !replaced {
        let mut prefixed = String::with_capacity(out.len() + line.len() + 1);
        prefixed.push_str(&line);
        prefixed.push('\n');
        prefixed.push_str(&out);
        return Ok(prefixed);
    }
    Ok(out)
}

/// Add every required top-level section that is absent, taking its defaults
/// from the example configuration shipped with this release. Returns the
/// section names that were added.
fn ensure_sections(value: &mut toml::Value) -> Vec<String> {
    let mut added = Vec::new();
    let Some(table) = value.as_table_mut() else {
        return added;
    };
    for name in REQUIRED_SECTIONS {
        if table.contains_key(*name) {
            continue;
        }
        // `[wallpaper]` must never inherit the "off" default: NIRUX installs
        // the theme wallpapers and expects them to follow the theme.
        let mut section = example_section(name).unwrap_or_default();
        if *name == "wallpaper" {
            section.insert("enabled".into(), toml::Value::Boolean(true));
            section
                .entry("follow_theme")
                .or_insert(toml::Value::Boolean(true));
        }
        table.insert((*name).into(), toml::Value::Table(section));
        added.push(format!("[{name}]"));
    }
    added
}

/// Read one section from `config.example.toml`, which sits next to the binary
/// in an installed generation and in the source tree during development.
fn example_section(name: &str) -> Option<toml::value::Table> {
    let text = example_text()?;
    let parsed: toml::Value = text.parse().ok()?;
    parsed.get(name).and_then(|v| v.as_table()).cloned()
}

/// The shipped `config.example.toml`, read from beside the binary in an
/// installed generation or from the source tree during development.
fn example_text() -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(exe) = exe.canonicalize() {
            // <prefix>/share/nirux/generations/ID/bin/niructl
            //   -> <prefix>/share/nirux/generations/ID/config.example.toml
            //   -> <prefix>/share/nirux/config.example.toml  (NIRUX install)
            if let Some(dir) = exe.parent().and_then(|p| p.parent()) {
                candidates.push(dir.join("config.example.toml"));
                if let Some(state) = dir.parent().and_then(|p| p.parent()) {
                    candidates.push(state.join("config.example.toml"));
                }
            }
        }
        // A standalone install keeps the example beside the binary's parent.
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("config.example.toml"));
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join("config.example.toml"));
            }
        }
    }
    candidates.push(config::directory().join("config.example.toml"));
    candidates.push(std::path::PathBuf::from("config.example.toml"));
    candidates
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("niructl: {e}");
        std::process::exit(1)
    }
}

#[cfg(test)]
mod display_parse_tests {
    use super::{display_outputs_from, display_resolve, DisplayOutput};

    const SAMPLE: &str = "Screen 0: minimum 320 x 200, current 5120 x 1440, maximum 16384 x 16384
DisplayPort-3 disconnected primary (normal left inverted right x axis y axis)
DisplayPort-4 connected 2560x1440+0+0 (normal left inverted right x axis y axis) 597mm x 336mm
   2560x1440     59.95*+  74.97
   1920x1080     60.00
DisplayPort-5 connected 2560x1440+2560+0 inverted (normal left inverted right x axis y axis) 597mm x 336mm
   2560x1440     59.95*+
DisplayPort-6 connected (normal left inverted right x axis y axis)
   1920x1200     59.88+
";

    #[test]
    fn the_current_rotation_is_read_not_the_supported_list() {
        let outputs = display_outputs_from(SAMPLE);
        let five = outputs.iter().find(|o| o.name == "DisplayPort-5").unwrap();
        assert_eq!(five.current.as_ref().unwrap().rotation, "inverted");
        let four = outputs.iter().find(|o| o.name == "DisplayPort-4").unwrap();
        assert_eq!(four.current.as_ref().unwrap().rotation, "normal");
    }

    #[test]
    fn the_preferred_mode_is_the_star_plus_entry() {
        let outputs = display_outputs_from(SAMPLE);
        let four = outputs.iter().find(|o| o.name == "DisplayPort-4").unwrap();
        assert_eq!(four.preferred.as_deref(), Some("2560x1440"));
        let six = outputs.iter().find(|o| o.name == "DisplayPort-6").unwrap();
        assert_eq!(six.preferred.as_deref(), Some("1920x1200"));
    }

    #[test]
    fn the_current_refresh_rate_is_read_from_the_mode_line() {
        let outputs = display_outputs_from(SAMPLE);
        let four = outputs.iter().find(|o| o.name == "DisplayPort-4").unwrap();
        assert!((four.current.as_ref().unwrap().rate.unwrap() - 59.95).abs() < 0.001);
        let mode = four
            .modes
            .iter()
            .find(|mode| mode.mode == "2560x1440")
            .unwrap();
        assert!(mode.rates.iter().any(|rate| (rate - 74.97).abs() < 0.001));
        assert!(mode.preferred);
    }

    #[test]
    fn a_saved_rate_is_validated_and_old_profiles_still_apply() {
        let outputs = display_outputs_from(SAMPLE);
        let with_rate = DisplayOutput {
            name: "DisplayPort-4".into(),
            mode: "2560x1440".into(),
            rate: Some(74.97),
            position: "0x0".into(),
            rotation: "normal".into(),
            primary: true,
        };
        let resolved = display_resolve(&outputs, &with_rate).unwrap();
        assert_eq!(resolved.rate, Some(74.97));
        let bad = DisplayOutput {
            rate: Some(60.0),
            ..with_rate.clone()
        };
        assert!(display_resolve(&outputs, &bad).is_err());
        let old = DisplayOutput {
            rate: None,
            ..with_rate
        };
        assert_eq!(display_resolve(&outputs, &old).unwrap().rate, None);
    }

    #[test]
    fn the_startup_profile_is_optional_and_round_trips() {
        let text = "startup_profile = \"Desk\"\n\n[[profile]]\nname = \"Desk\"\n[[profile.outputs]]\nname = \"eDP-1\"\nmode = \"preferred\"\nposition = \"0x0\"\nrotation = \"normal\"\nprimary = true\n";
        let file: super::DisplayFile = toml::from_str(text).unwrap();
        assert_eq!(file.startup_profile.as_deref(), Some("Desk"));
        assert_eq!(file.profile[0].name, "Desk");
        let rendered = toml::to_string(&file).unwrap();
        assert!(rendered.contains("startup_profile = \"Desk\""));
        let empty: super::DisplayFile = toml::from_str("[[profile]]\nname = \"Old\"\n").unwrap();
        assert_eq!(empty.startup_profile, None);
    }

    #[test]
    fn edid_identity_is_decoded_without_x() {
        let mut edid = vec![0u8; 128];
        edid[0..8].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
        // DEL: D=4, E=5, L=12.
        edid[8] = 0x10;
        edid[9] = 0xac;
        edid[10] = 0x34;
        edid[11] = 0x12;
        edid[12..16].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        edid[16] = 7;
        edid[17] = 30;
        edid[54..58].copy_from_slice(&[0, 0, 0, 0xfc]);
        let model = b"Test Monitor\n   ";
        edid[59..59 + model.len()].copy_from_slice(model);
        let identity = super::parse_edid(&edid).expect("valid EDID");
        assert_eq!(identity["manufacturer"], "DEL");
        assert_eq!(identity["model"], "Test Monitor");
        assert_eq!(identity["year"], 2020);
        assert_eq!(identity["product_code"], 0x1234);
    }

    #[test]
    fn a_connected_but_switched_off_screen_can_be_placed() {
        let outputs = display_outputs_from(SAMPLE);
        let profile = DisplayOutput {
            name: "DisplayPort-6".into(),
            mode: "preferred".into(),
            rate: None,
            position: "5120x0".into(),
            rotation: "normal".into(),
            primary: false,
        };
        let resolved = display_resolve(&outputs, &profile).expect("an off screen is placeable");
        assert_eq!(resolved.mode, "1920x1200");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tenth_workspace_is_accepted_for_autostart() {
        assert!(valid_workspace(1));
        assert!(valid_workspace(10));
        assert!(!valid_workspace(0));
        assert!(!valid_workspace(11));
    }

    #[test]
    fn truncated_config_regains_every_required_section() {
        // The exact shape that broke Arch: one line, no sections at all.
        let mut value: toml::Value = "theme = \"niru-satie\"\n".parse().unwrap();
        let added = ensure_sections(&mut value);
        assert!(
            added.contains(&"[wallpaper]".to_string()),
            "added: {added:?}"
        );
        let table = value.as_table().unwrap();
        for name in REQUIRED_SECTIONS {
            assert!(
                table.get(*name).and_then(|v| v.as_table()).is_some(),
                "missing [{name}] after repair"
            );
        }
    }

    #[test]
    fn repaired_wallpaper_section_is_enabled_and_follows_the_theme() {
        // The built-in default is disabled, so a repaired config must be
        // explicit: otherwise theme wallpapers stay off after the repair.
        let mut value: toml::Value = "theme = \"niru-noir\"\n".parse().unwrap();
        ensure_sections(&mut value);
        let wallpaper = value["wallpaper"].as_table().unwrap();
        assert_eq!(wallpaper.get("enabled"), Some(&toml::Value::Boolean(true)));
        assert_eq!(
            wallpaper.get("follow_theme"),
            Some(&toml::Value::Boolean(true))
        );
        // The user's theme survives the repair.
        assert_eq!(value["theme"].as_str(), Some("niru-noir"));
    }

    #[test]
    fn complete_sections_are_left_alone() {
        let mut value: toml::Value = r#"
theme = "niru-noir"
[wallpaper]
enabled = false
follow_theme = false
[autostart]
enabled = true
[bar]
enabled = true
[nightlight]
enabled = false
[brightness]
enabled = false
[monitors]
[idle]
screensaver_timeout = 15
[keys]
"#
        .parse()
        .unwrap();
        let added = ensure_sections(&mut value);
        assert!(
            added.is_empty(),
            "should not touch a complete file: {added:?}"
        );
        // An explicit user choice of "off" is respected, never flipped to on.
        assert_eq!(
            value["wallpaper"].get("enabled"),
            Some(&toml::Value::Boolean(false))
        );
    }

    #[test]
    fn theme_line_is_rewritten_without_touching_comments() {
        let text = "# my config\ntheme = \"niru-noir\"\n\n[wallpaper]\n# keep me\nenabled = true\ntheme_dir = \"~/Pictures/Wallpapers/NIRU\"\n";
        let out = replace_theme_line(text, "niru-nord").unwrap();
        assert!(out.contains("theme = \"niru-nord\""));
        assert!(out.contains("# my config"), "comments must survive");
        assert!(out.contains("# keep me"));
        assert!(out.contains("theme_dir = \"~/Pictures/Wallpapers/NIRU\""));
        // Everything except the theme line is byte-for-byte identical.
        let expect = text.replace("theme = \"niru-noir\"", "theme = \"niru-nord\"");
        assert_eq!(out, expect);
    }

    #[test]
    fn missing_theme_line_is_added_at_the_top() {
        let out = replace_theme_line("[wallpaper]\nenabled = true\n", "niru-noir").unwrap();
        assert_eq!(out, "theme = \"niru-noir\"\n[wallpaper]\nenabled = true\n");
    }

    #[test]
    fn section_keys_are_not_mistaken_for_the_theme() {
        // `theme_dir` inside [wallpaper] is a different key and must be kept.
        let text = "theme = \"a\"\n[wallpaper]\ntheme_dir = \"~/x\"\n";
        let out = replace_theme_line(text, "b").unwrap();
        assert!(out.starts_with("theme = \"b\"\n"));
        assert!(out.contains("theme_dir = \"~/x\""));
    }

    use super::*;
    use std::{os::unix::fs::PermissionsExt, thread, time::Duration};

    fn script(dir: &std::path::Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut mode = std::fs::metadata(&path).unwrap().permissions();
        mode.set_mode(0o755);
        std::fs::set_permissions(&path, mode).unwrap();
    }

    #[test]
    fn parses_audio_nodes_and_defaults() {
        let text = "Audio\n ├─ Devices:\n │      42. Built-in Audio [alsa]\n ├─ Sinks:\n │  *   52. Speakers [vol: 0.50]\n │      57. HDMI / DisplayPort [vol: 1.00]\n ├─ Sources:\n │  *   53. Microphone [vol: 1.00]\n ├─ Filters:\nVideo\n ├─ Sources:\n │  *   86. Webcam (V4L2)\n";
        let outputs = audio_nodes_from(text, "outputs");
        assert_eq!(
            outputs,
            vec![
                AudioNode {
                    id: 52,
                    name: "Speakers".into(),
                    default: true
                },
                AudioNode {
                    id: 57,
                    name: "HDMI / DisplayPort".into(),
                    default: false
                },
            ]
        );
        let sources = audio_nodes_from(text, "sources");
        assert_eq!(
            sources,
            vec![AudioNode {
                id: 53,
                name: "Microphone".into(),
                default: true
            }]
        );
    }

    #[test]
    fn fixed_schedule_handles_daytime_and_midnight_wrap() {
        assert!(fixed_schedule_active(12 * 60, 8 * 60, 18 * 60));
        assert!(!fixed_schedule_active(7 * 60, 8 * 60, 18 * 60));
        assert!(fixed_schedule_active(23 * 60, 20 * 60, 7 * 60));
        assert!(fixed_schedule_active(2 * 60, 20 * 60, 7 * 60));
        assert!(!fixed_schedule_active(12 * 60, 20 * 60, 7 * 60));
        assert_eq!(clock_minutes("20:30"), Some(20 * 60 + 30));
        assert_eq!(clock_minutes("24:00"), None);
    }

    #[test]
    fn desktop_actions_use_explicit_backends() {
        let dir = std::env::temp_dir().join(format!("niructl-actions-{}", std::process::id()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let calls = dir.join("calls");
        let dnd_state = dir.join("dnd");
        std::fs::write(&dnd_state, "false\n").unwrap();
        let log = |name: &str| format!("printf '%s %s\\n' '{name}' \"$*\" >> \"$CALLS\"\n");
        script(&bin, "niruscreensaver", &log("niruscreensaver"));
        script(&bin, "loginctl", &log("loginctl"));
        script(&bin, "dbus-send", &log("dbus-send"));
        script(
            &bin,
            "dunstctl",
            "printf 'dunstctl %s\\n' \"$*\" >> \"$CALLS\"\ncase \"$1\" in\nis-paused) cat \"$DND_STATE\" ;;\nset-paused) printf '%s\\n' \"$2\" > \"$DND_STATE\" ;;\nesac",
        );
        std::env::set_var("HOME", &dir);
        std::env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.display()));
        std::env::set_var("CALLS", &calls);
        std::env::set_var("DND_STATE", &dnd_state);
        std::env::set_var("XDG_SESSION_ID", "42");
        std::env::set_var("XDG_STATE_HOME", &dir);
        let calls_now = || std::fs::read_to_string(&calls).unwrap_or_default();
        let wait_for = |what: &str| {
            for _ in 0..100 {
                if calls_now().contains(what) {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("{what} was not called; calls: {}", calls_now());
        };

        desktop("screensaver", &[]).unwrap();
        wait_for("niruscreensaver --preview");
        login_action("reboot").unwrap();
        wait_for("org.freedesktop.login1.Manager.Reboot boolean:true");
        desktop("shutdown", &[]).unwrap();
        wait_for("org.freedesktop.login1.Manager.PowerOff boolean:true");
        login_action("suspend").unwrap();
        wait_for("org.freedesktop.login1.Manager.Suspend boolean:true");
        desktop("logout", &[]).unwrap();
        wait_for("loginctl terminate-session 42");
        dnd(&["toggle".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&dnd_state).unwrap().trim(), "true");
        dnd(&["toggle".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&dnd_state).unwrap().trim(), "false");

        // When the native service is present, DND must not shell out to Dunst.
        let native_state = dir.join("native-dnd");
        std::fs::write(&native_state, "off\n").unwrap();
        std::fs::write(
            bin.join("nirunotify"),
            "#!/bin/sh\nprintf 'nirunotify %s\\n' \"$*\" >> \"$CALLS\"\ncase \"$2\" in\non|off) printf '%s\\n' \"$2\" > \"$NATIVE_STATE\" ;;\ntoggle) if [ \"$(cat \"$NATIVE_STATE\")\" = on ]; then printf 'off\\n' > \"$NATIVE_STATE\"; else printf 'on\\n' > \"$NATIVE_STATE\"; fi ;;\nesac\n[ \"$2\" = status ] && printf 'dnd %s\\n' \"$(cat \"$NATIVE_STATE\")\"\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(
            bin.join("nirunotify"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        std::env::set_var("NATIVE_STATE", &native_state);
        dnd(&["toggle".to_string()]).unwrap();
        wait_for("nirunotify --dnd toggle");
        assert_eq!(std::fs::read_to_string(&native_state).unwrap().trim(), "on");
        dnd(&["status".to_string()]).unwrap();
        wait_for("nirunotify --dnd status");
        idle(&["on".to_string(), "1".to_string()]).unwrap();
        assert!(idle_until().is_some());
        idle(&["off".to_string()]).unwrap();
        assert!(idle_until().is_none());
        let path = idle_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "1\n").unwrap();
        assert!(idle_until().is_none(), "expired inhibition stayed active");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn timing_update_preserves_comments_order_and_unrelated_keys() {
        let text = "# my config\ntheme = \"niru-nord\"\n\n[idle]\n# keep this comment\nscreensaver_timeout = 15\nlock_after_screensaver = 15\ndpms_off_after_lock = 15\n";
        let out = super::updated_timing(text, "screensaver_timeout", 5).unwrap();
        assert!(out.contains("# my config"));
        assert!(out.contains("# keep this comment"));
        assert!(out.contains("theme = \"niru-nord\""));
        assert!(out.contains("screensaver_timeout = 5"));
        assert!(out.contains("lock_after_screensaver = 15"));
    }

    #[test]
    fn timing_update_rejects_an_unlocked_screen_and_unknown_keys() {
        let text = "[idle]\nscreensaver_timeout = 15\n";
        assert!(super::updated_timing(text, "screensaver_timeout", 0).is_err());
        assert!(super::updated_timing(text, "bogus", 5).is_err());
    }

    #[test]
    fn a_second_suspend_request_is_coalesced() {
        let dir = std::env::temp_dir().join(format!("niructl-suspend-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("suspend.lock");
        let first = super::acquire_lock_at(&path).unwrap();
        assert!(first.is_some(), "the first request takes the lock");
        assert!(
            super::acquire_lock_at(&path).unwrap().is_none(),
            "a second request must not queue"
        );
        drop(first);
        assert!(
            super::acquire_lock_at(&path).unwrap().is_some(),
            "the lock is released when the request finishes"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn suspend_is_cancelled_when_the_lock_window_changes() {
        assert!(super::lock_still_held(Some(42), 42));
        assert!(!super::lock_still_held(Some(43), 42));
        assert!(!super::lock_still_held(None, 42));
    }
}
