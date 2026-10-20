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

use chrono::{Local, Timelike};
use config::{Config, Result, Theme};
use std::time::Duration;
use x11rb::{
    connection::{Connection, RequestConnection as _},
    protocol::{randr::ConnectionExt as _, xproto::*},
    wrapper::ConnectionExt as _,
};

const USAGE: &str = "Usage: niructl status | tag N | movetotag N | layout NAME | layouts | \
theme NAME | themes | monitors | reload | restart-bar | quit | save | save-session | \
clear-session | brightness up|down|set N|status | nightlight on|off|toggle|status|temperature K|apply|watch | \
lock | screensaver | logout | suspend | reboot | shutdown | dnd on|off|toggle|status | \
idle on|off|toggle|status [MINUTES]";
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
    if which("loginctl") {
        let mut args: Vec<String> = Vec::new();
        match action {
            "logout" => {
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
            }
            "suspend" | "reboot" | "shutdown" => args.push(action.into()),
            _ => return Err(format!("unknown login action: {action}").into()),
        }
        command("loginctl", &args)?;
        return Ok(());
    }
    if action != "logout" && which("systemctl") {
        command("systemctl", &[action.to_string()])?;
        return Ok(());
    }
    Err(format!("no login backend for {action}: install logind/elogind or systemd").into())
}

fn power_action(action: &str) -> Result<()> {
    if action == "suspend" {
        ensure_lock_ready()?;
    }
    if let Err(e) = login_action(action) {
        let body = format!("{action} failed: {e}");
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "NIRU", "-u", "critical", "Power", &body])
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
    if !which("dunstctl") {
        return Err(
            "DND is unavailable: no supported notification provider (dunstctl missing)".into(),
        );
    }
    let action = args.first().map(String::as_str).unwrap_or("status");
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
        _ => return Err("dnd needs on, off, toggle or status".into()),
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
    if cmd == "nightlight" {
        return nightlight(&std::env::args().skip(2).collect::<Vec<_>>());
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
            | "lock"
            | "screensaver"
            | "logout"
            | "suspend"
            | "reboot"
            | "shutdown"
            | "dnd"
            | "idle"
    ) {
        return offline(cmd);
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
        _ => return Err(USAGE.into()),
    };

    if cmd == "quit" {
        // Exit the WM cleanly: the session script then tears down helpers.
        send(&c, root, atom(CMD_ATOM)?, [action, value, 0, 0, 0])?;
    } else if cmd == "reload" || cmd == "theme" {
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
    if !(1..=9).contains(&n) {
        return Err("tag must be 1..9".into());
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
        wait_for("loginctl reboot");
        desktop("logout", &[]).unwrap();
        wait_for("loginctl terminate-session 42");
        dnd(&["toggle".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&dnd_state).unwrap().trim(), "true");
        dnd(&["toggle".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&dnd_state).unwrap().trim(), "false");
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
