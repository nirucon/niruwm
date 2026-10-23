//! niructl — small control channel for NIRUWM and NIRUBAR.
//!
//! Communication is a client message on the root window. It is deliberately
//! the same mechanism `xdotool` and EWMH clients use, so no daemon, socket or
//! dependency is required.
#[path = "../bindings.rs"]
#[allow(dead_code)] // niructl only reads the shared config shape.
mod bindings;
#[path = "../config.rs"]
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

use chrono::{Local, TimeZone, Timelike};
use config::{Config, Result, Theme};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
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
theme NAME | themes | monitors | reload | restart-bar | quit | save | save-session | \
clear-session | brightness up|down|set N|status | nightlight on|off|toggle|status|temperature K|apply|watch | \
compositor on|off|toggle|status | \
wallpaper list|status|set PATH|mode VALUE|rotation MINUTES|scale VALUE|follow-theme on|off | \
autostart | recipe list|run NAME | mode focus|presentation|stop|status [MINUTES] | \
display list|save NAME|preview NAME|apply NAME|keep|revert|status | \
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
                "-l".into(),
                location.clone(),
                "-O".into(),
                kelvin.to_string(),
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

fn power_action(action: &str) -> Result<()> {
    if action == "suspend" {
        ensure_lock_ready()?;
    }
    if let Err(e) = login_action(action) {
        let body = format!("{action} failed: {e}");
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "NIRU", "-t", "8000", "Power", &body])
            .status();
        return Err(e);
    }
    Ok(())
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

/// Lock before suspending. Requires NIRULOCK's readiness contract; a bare
/// `slock` cannot be verified and is therefore not accepted for suspend.
fn ensure_lock_ready() -> Result<()> {
    let (c, s) = x11rb::connect(None)?;
    let root = c.setup().roots[s].root;
    if ready_lock(&c, root).is_some() {
        return Ok(());
    }
    let locker = program("nirulock")
        .ok_or("cannot lock before suspend: install nirulock (slock cannot confirm readiness)")?;
    spawn_action(&locker, &[])?;
    wait_ready_lock(&c, root)?;
    Ok(())
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct DisplayOutput {
    name: String,
    #[serde(default = "display_preferred_mode")]
    mode: String,
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
fn display_zero_position() -> String {
    "0x0".into()
}
fn display_normal_rotation() -> String {
    "normal".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct DisplayProfile {
    name: String,
    #[serde(default)]
    outputs: Vec<DisplayOutput>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct DisplayFile {
    #[serde(default)]
    profile: Vec<DisplayProfile>,
}

#[derive(Clone, Debug)]
struct DisplayOutputState {
    name: String,
    connected: bool,
    modes: Vec<String>,
    current: Option<DisplayOutput>,
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

fn display_outputs() -> Result<Vec<DisplayOutputState>> {
    let out = command("xrandr", &["--query".into()])?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut outputs: Vec<DisplayOutputState> = Vec::new();
    for line in text.lines() {
        if !line.starts_with(|c: char| !c.is_whitespace()) {
            if let Some(output) = outputs.last_mut() {
                if let Some(mode) = line.split_whitespace().next() {
                    if !mode.is_empty() && mode.contains('x') {
                        output.modes.push(mode.to_string());
                    }
                }
                if line.split_whitespace().any(|word| word.contains('*')) {
                    if let Some(mode) = line.split_whitespace().next() {
                        if let Some(current) = output.current.as_mut() {
                            current.mode = mode.to_string();
                        }
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
        let rotation = line
            .split('(')
            .nth(1)
            .and_then(|inside| {
                inside
                    .split_whitespace()
                    .find(|word| matches!(*word, "normal" | "left" | "inverted" | "right"))
            })
            .unwrap_or("normal")
            .to_string();
        let current = geometry.map(|geometry| {
            let mode = geometry.split('+').next().unwrap_or(geometry);
            DisplayOutput {
                name: name.to_string(),
                mode: mode.to_string(),
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
            current,
        });
    }
    Ok(outputs)
}

fn display_resolve(
    outputs: &[DisplayOutputState],
    output: &DisplayOutput,
) -> Result<DisplayOutput> {
    let state = outputs
        .iter()
        .find(|state| state.name == output.name)
        .ok_or_else(|| format!("display output {} is not connected", output.name))?;
    if !state.connected || state.current.is_none() && state.modes.is_empty() {
        return Err(format!("display output {} is not connected", output.name).into());
    }
    let mode = if output.mode == "preferred" || output.mode.is_empty() {
        state
            .modes
            .first()
            .cloned()
            .ok_or_else(|| format!("display output {} has no modes", output.name))?
    } else if state.modes.iter().any(|mode| mode == &output.mode) {
        output.mode.clone()
    } else {
        return Err(format!(
            "display mode {} is not available on {}",
            output.mode, output.name
        )
        .into());
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
    command("xrandr", &args)?;
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

fn display(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("list");
    match action {
        "list" => {
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
            let previous = display_current_layout(&outputs);
            if previous.is_empty() {
                return Err("no active display layout to revert to".into());
            }
            if let Err(error) = display_apply(&outputs, &profile) {
                let _ = display_restore(&previous);
                return Err(error);
            }
            let seconds = args
                .get(2)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(15)
                .clamp(5, 120);
            let token = format!("{}-{}", now_secs(), std::process::id());
            let expiry = now_secs() + seconds;
            display_preview_write(&DisplayPreview {
                name: name.clone(),
                token: token.clone(),
                expiry,
                previous,
            })?;
            display_notify(
                "Display preview",
                &format!("{name}: keep within {seconds}s or the previous layout returns"),
            );
            let exe = std::env::current_exe()?;
            let _ = std::process::Command::new(exe)
                .args(["display", "expire", &token])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            println!("previewing display profile {name} for {seconds}s");
        }
        "keep" => {
            if display_preview_read().is_none() {
                println!("no display preview active");
            } else {
                let _ = std::fs::remove_file(display_preview_path());
                display_notify("Display layout kept", "Preview confirmed");
                println!("display preview kept");
            }
        }
        "revert" => {
            let Some(state) = display_preview_read() else {
                println!("no display preview active");
                return Ok(());
            };
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
            let wait = state.expiry.saturating_sub(now_secs());
            if wait > 0 {
                std::thread::sleep(Duration::from_secs(wait));
            }
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
        "status" => match display_preview_read() {
            Some(state) if state.expiry > now_secs() => {
                println!(
                    "display preview {} remaining {}s",
                    state.name,
                    state.expiry - now_secs()
                );
            }
            _ => println!("display preview off"),
        },
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
    Ok(())
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
    if cmd == "nightlight" {
        return nightlight(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "compositor" {
        return compositor(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "idle" {
        return idle(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if matches!(
        cmd,
        "lock" | "screensaver" | "logout" | "suspend" | "reboot" | "shutdown" | "dnd"
    ) {
        return desktop(cmd, &std::env::args().skip(2).collect::<Vec<_>>());
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
    if cmd == "monitors" {
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
        return Ok(());
    }
    if cmd == "themes" {
        let dir = config::directory().join("themes");
        println!("NIRUWM themes:");
        for name in config::Theme::builtin() {
            let t = Theme::load(name)?;
            println!("  {:<18} {}", name, t.name);
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut extra: Vec<String> = entries
                .flatten()
                .filter_map(|e| {
                    let p = e.path();
                    if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                        p.file_stem().map(|s| s.to_string_lossy().to_string())
                    } else {
                        None
                    }
                })
                .filter(|n| !config::Theme::builtin().contains(&n.as_str()))
                .collect();
            extra.sort();
            if !extra.is_empty() {
                println!();
                println!("Your own themes in {}:", dir.display());
                for name in extra {
                    println!("  {name}");
                }
            }
        }
        println!();
        println!("Select with: niructl theme NAME   (or set theme in config.toml)");
        return Ok(());
    }
    Ok(())
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
            | "layouts"
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
    ) {
        return offline(cmd);
    }
    if cmd == "wallpaper" {
        let sub = args.get(1).map(String::as_str).unwrap_or("");
        if sub == "list" || sub == "status" {
            return wallpaper(sub);
        }
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
        println!(
            "wm={} tag={} theme={}",
            wm,
            tag + 1,
            Theme::load(&Config::load()?.theme)?.name
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
                _ => return Err(USAGE.into()),
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
            let name = args.get(1).ok_or(USAGE)?;
            Theme::load(name)?;
            write_theme(name)?;
            (4, 0)
        }
        "wallpaper" => {
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

/// Persist a theme choice without destroying the rest of the configuration.
fn write_theme(name: &str) -> Result<()> {
    let dir = config::directory();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("config.toml");
    let mut value = match std::fs::read_to_string(&path) {
        Ok(s) => s.parse::<toml::Value>()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            toml::Value::Table(Default::default())
        }
        Err(e) => return Err(e.into()),
    };
    value
        .as_table_mut()
        .ok_or("Invalid config: expected a table")?
        .insert("theme".into(), toml::Value::String(name.into()));
    let temp = dir.join(format!(".config.toml.{}", std::process::id()));
    std::fs::write(&temp, toml::to_string_pretty(&value)?)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("niructl: {e}");
        std::process::exit(1)
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
}
