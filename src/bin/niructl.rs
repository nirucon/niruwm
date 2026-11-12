//! niructl — small control channel for NIRUWM and NIRUBAR.
//!
//! Communication is a client message on the root window. It is deliberately
//! the same mechanism `xdotool` and EWMH clients use, so no daemon, socket or
//! dependency is required.
#[path = "../stdout.rs"]
#[macro_use]
mod stdout;
#[path = "../bindings.rs"]
#[allow(dead_code)] // niructl only reads the shared config shape.
mod bindings;
#[path = "../config.rs"]
#[allow(dead_code)] // shared with NIRUWM; theme management consumes part of it
mod config;
#[path = "../config_tools.rs"]
mod config_tools;
#[path = "../display_auto.rs"]
mod display_auto;
#[path = "../keysheet.rs"]
mod keysheet;
#[path = "../layout.rs"]
#[allow(dead_code)] // niructl prints the layout names
mod layout;
#[path = "../monitor.rs"]
#[allow(dead_code)]
mod monitor;
#[path = "../session_state.rs"]
#[allow(dead_code)] // niructl reads the list, it never writes one
mod session_state;
#[path = "../studio.rs"]
mod studio;
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

const USAGE: &str = "Usage: niructl status | tag N | movetotag N | \
theme NAME | themes | theme-catalog | theme-import | theme-remove | theme-hide | theme-show | keys | monitors | reload | restart-bar | restart-wm [--check] | bar top|bottom|hide|show|toggle|cycle|status | quit | save | save-session | \
layout NAME|columns 1|2|toggle | layouts | \
clear-session | brightness up|down|set N|status | nightlight on|off|toggle|status|temperature K|apply|watch | \
compositor on|off|toggle|status | \
timing show|set KEY MINUTES | \
wallpaper list|status|apply [--json]|set PATH|mode VALUE|rotation MINUTES|scale VALUE|follow-theme on|off|style art|signature|toggle | \
         output list|set NAME KEY VALUE|reset NAME | \
autostart | recipe list|run NAME | mode focus|presentation|stop|status [MINUTES] | \
display list [--json]|save NAME|save-json NAME|preview NAME [SECONDS]|preview-json [SECONDS]|apply NAME|keep|revert|status [--json]|delete NAME|rename OLD NEW|startup show|set NAME|off|apply|rotate OUTPUT normal|left|right|inverted|auto|automatic on|off|status [--json]|run|doctor [--json]|watch [--once] [--quiet] | \
action list [--json]|save NAME -- CMD [ARG...]|run NAME|delete NAME|rename OLD NEW|pin NAME|unpin NAME | \
layout show|save | \
scratchpad [N] [toggle|show|hide]|send N|unscratch|list [--json] | \
urgent [list|--json|focus] | swallow on|off|toggle|status | studio on|off|toggle|status | \
powerprofile status|list|set NAME|next | \
lock [--manual|--silent|--reason NAME] | screensaver | logout | suspend | reboot | shutdown | update-shutdown | update-reboot | dnd on|off|toggle|status | \
audio status [--json]|outputs|sources [--json]|output ID|next|source ID|next|input ID|next|mute|mic-status [--json]|mic-mute [on|off|toggle]|mic-level up|down|set N|mic-test [SECONDS] [--json] [--keep]|volume up|down | \
bluetooth status|list|power on|off|toggle|connect MAC|disconnect MAC | \
explain [--launcher] | idle on|off|toggle|status [MINUTES] | \
storage review [--json]|clean cache --yes | \
agent [--dry-run] TASK | task list [--json]|clear | \
windows rescue [--dry-run] | \
battery status [--json]|apply|full|limit | \
config history|diff [N|--all]|undo [STEPS] [--yes]|restore N [--yes]|reset SECTION|KEY|all [--yes] | \
input status [--json]|apply|set-keyboard LAYOUT [VARIANT]|set-repeat DELAY RATE|set-pointer natural|tap on|off|toggle natural|tap|set-speed VALUE|cycle-speed|reset";
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
    notify_with_sound(summary, body, value, None);
}

/// Like `notify`, and the suite OSD also plays SOUND (a NIRUX feedback event
/// and the new level) in the same call. NIRUNOTIFY decides whether the
/// event is switched on in `~/.config/nirux/sounds.toml`.
fn notify_with_sound(summary: &str, body: &str, value: Option<u32>, sound: Option<(&str, u32)>) {
    // Prefer the suite's coalesced hardware OSD when its daemon is running.
    // The CLI exits nonzero without a socket, so a Dunst session falls back
    // to the ordinary notification path.
    if let Some(osd) = program("nirunotify") {
        let channel = summary.to_lowercase().replace(' ', "-");
        let mut args = vec!["--osd".to_string(), channel, body.to_string()];
        if let Some((event, level)) = sound {
            args.extend(["--sound".to_string(), event.to_string(), level.to_string()]);
        }
        if std::process::Command::new(osd)
            .args(&args)
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
    let sound = match action {
        "up" => Some("brightness-up"),
        "down" => Some("brightness-down"),
        _ => None,
    };
    notify_with_sound(
        "Brightness",
        &format!(
            "{}%",
            percent
                .map(|p| p.to_string())
                .unwrap_or_else(|| "--".into())
        ),
        percent,
        sound.map(|event| (event, percent.unwrap_or(0))),
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
    // 0.25.2: without a location, use Stockholm's, which the example file
    // ships with, so the default schedule works before anyone sets theirs.
    let (latitude, longitude) = if latitude == 0.0 && longitude == 0.0 {
        (59.33, 18.07)
    } else {
        (latitude, longitude)
    };
    let location = format!("{latitude}:{longitude}");
    let state = nightlight_state();
    // `loud` shows a notification; the schedule changes the temperature in
    // small steps at dusk and dawn and stays quiet.
    let apply = |kelvin: u32, loud: bool| -> Result<()> {
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
        if loud {
            notify("Night light", &format!("{kelvin} K"), None);
        }
        Ok(())
    };
    let off = |loud: bool| -> Result<()> {
        command("gammastep", &["-x".into()])?;
        let _ = std::fs::remove_file(&state);
        if loud {
            notify("Night light", "off", None);
        }
        Ok(())
    };
    // What the schedule wants now, and whether that is the night phase.
    let scheduled = || -> Result<(Option<u32>, bool)> {
        let clock = Local::now();
        let now = clock.hour() * 60 + clock.minute();
        Ok(match nl.mode.as_str() {
            "fixed" => {
                let on = clock_minutes(&nl.on).ok_or("nightlight.on must be HH:MM")?;
                let off = clock_minutes(&nl.off).ok_or("nightlight.off must be HH:MM")?;
                let night = fixed_schedule_active(now, on, off);
                (night.then_some(nl.temperature), night)
            }
            "solar" => {
                let kelvin = solar_temperature(&location, nl.day_temperature, nl.temperature)
                    .ok_or("gammastep could not compute the solar temperature")?;
                (
                    Some(kelvin),
                    kelvin < (nl.day_temperature + nl.temperature) / 2,
                )
            }
            _ => (None, false),
        })
    };
    // 0.25.2: Super+N (on, off, toggle) wins over the schedule until the
    // schedule changes phase, at the next sunset or sunrise (or on/off time).
    let override_file = state.with_file_name("nightlight-override");
    let remember = |choice: &str| -> Result<()> {
        if nl.mode == "manual" {
            return Ok(());
        }
        let (_, night) = scheduled()?;
        if let Some(parent) = override_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            &override_file,
            format!("{choice} {}", if night { "night" } else { "day" }),
        )?;
        Ok(())
    };
    let reconcile = || -> Result<()> {
        let (desired, night) = scheduled()?;
        let phase = if night { "night" } else { "day" };
        let manual = std::fs::read_to_string(&override_file).ok();
        let desired = match manual
            .as_deref()
            .map(str::split_whitespace)
            .map(|mut w| (w.next(), w.next()))
        {
            Some((Some(choice), Some(p))) if p == phase => {
                if choice == "on" {
                    Some(nl.temperature)
                } else {
                    None
                }
            }
            Some(_) => {
                let _ = std::fs::remove_file(&override_file);
                desired
            }
            None => desired,
        };
        match desired {
            Some(kelvin) => {
                let current = std::fs::read_to_string(&state).ok();
                if current.as_deref() != Some(&kelvin.to_string()) {
                    apply(kelvin, false)?;
                }
            }
            None if state.exists() => off(false)?,
            _ => {}
        }
        Ok(())
    };
    match args.first().map(String::as_str).unwrap_or("status") {
        "on" => {
            apply(nl.temperature, true)?;
            remember("on")?;
        }
        "off" => {
            off(true)?;
            remember("off")?;
        }
        "toggle" => {
            // In solar mode the day temperature is also "applied", so "on"
            // means the night temperature is what the screen shows.
            let shown = std::fs::read_to_string(&state).ok();
            if shown.as_deref() == Some(&nl.temperature.to_string())
                || (nl.mode == "manual" && state.exists())
            {
                off(true)?;
                remember("off")?;
            } else {
                apply(nl.temperature, true)?;
                remember("on")?;
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
            apply(kelvin, true)?;
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

/// Start NIRULOCK with its lock reason. The reason goes in `NIRULOCK_REASON`,
/// not `--reason`: NIRULOCK before 0.25 rejects unknown arguments and would
/// not lock at all, but ignores the variable.
fn spawn_locker(program: &str, reason: &str) -> Result<()> {
    std::process::Command::new(program)
        .env("NIRULOCK_REASON", reason)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("cannot start {program}: {e}").into())
}

/// Open the system update action in a real terminal. Package managers need a
/// TTY for sudo and for conffile prompts, so this intentionally does not run in
/// the WM process or through a hidden pkexec helper.
fn update_terminal(action: &str, extra: &[String]) -> Result<()> {
    let nirux = program("nirux").ok_or("nirux is not installed")?;
    let title = if action == "update-reboot" {
        "NIRUX Update and Restart"
    } else {
        "NIRUX Update and Shut Down"
    };
    let mut args: Vec<String> = vec![action.to_string()];
    args.extend(extra.iter().cloned());
    let terminals: [(&str, &str, &str); 5] = [
        ("kitty", "--title", "--hold"),
        ("foot", "-T", "--hold"),
        ("alacritty", "-t", "--hold"),
        ("x-terminal-emulator", "-T", "-hold"),
        ("xterm", "-T", "-hold"),
    ];
    for (name, title_flag, hold_flag) in terminals {
        let Some(path) = program(name) else {
            continue;
        };
        let mut process = std::process::Command::new(path);
        if name == "kitty" {
            process
                .arg(title_flag)
                .arg(title)
                .arg(hold_flag)
                .arg(&nirux)
                .args(&args);
        } else {
            process
                .arg(title_flag)
                .arg(title)
                .arg(hold_flag)
                .arg("-e")
                .arg(&nirux)
                .args(&args);
        }
        process
            .spawn()
            .map_err(|e| format!("cannot start {name}: {e}"))?;
        return Ok(());
    }
    Err("no terminal found for the update action (install kitty or xterm)".into())
}

fn update_power_action(action: &str, args: &[String]) -> Result<()> {
    if !matches!(action, "update-shutdown" | "update-reboot") {
        return Err("unknown update action".into());
    }
    update_terminal(action, args)
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
        "lock" => {
            let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
            lock_action(lock_reason(args, interactive))
        }
        "screensaver" => {
            let saver = program("niruscreensaver").ok_or("niruscreensaver is not installed")?;
            // No argument: the screensaver itself, over every monitor.
            // `--preview` (NIRUSCREENSAVER 0.13+) covers one monitor only.
            spawn_action(&saver, &[])
        }
        "suspend" | "logout" | "reboot" | "shutdown" => power_action(action),
        "update-shutdown" | "update-reboot" => update_power_action(action, args),
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

/// Why `niructl lock` locks, for NIRULOCK's pause music (only `manual`
/// plays by default).
const LOCK_REASONS: [&str; 5] = ["manual", "idle", "suspend", "script", "session"];

/// The reason from `niructl lock [--manual | --silent | --reason NAME]`.
///
/// Without an option the lock is manual when typed in a terminal and
/// `script` otherwise (an SSH command, cron, another program), so no
/// automated lock starts music by accident. A wrong option never prevents
/// the lock: it is reported and the lock is silent.
fn lock_reason(args: &[String], interactive: bool) -> &'static str {
    let mut reason = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let named = match arg.as_str() {
            "--manual" => Some("manual"),
            "--silent" => Some("script"),
            "--reason" => rest.next().map(String::as_str),
            other => other.strip_prefix("--reason="),
        };
        match named.and_then(|n| LOCK_REASONS.into_iter().find(|r| *r == n)) {
            Some(r) => reason = Some(r),
            None => {
                eprintln!(
                    "niructl: lock: ignoring {arg:?} (use --manual, --silent or --reason {}); locking silently",
                    LOCK_REASONS.join("|")
                );
                return "script";
            }
        }
    }
    reason.unwrap_or(if interactive { "manual" } else { "script" })
}

/// Lock and, for NIRULOCK, wait for verified readiness before returning.
///
/// `slock` has no readiness contract, so it is still usable interactively but
/// cannot be used as proof that a suspend is safe.
fn lock_action(reason: &str) -> Result<()> {
    let (c, s) = x11rb::connect(None)?;
    let root = c.setup().roots[s].root;
    if ready_lock(&c, root).is_some() {
        return Ok(());
    }
    if let Some(locker) = program("nirulock") {
        stop_screensavers(&c);
        spawn_locker(&locker, reason)?;
        wait_ready_lock(&c, root)?;
        return Ok(());
    }
    if let Some(locker) = program("slock") {
        return spawn_action(&locker, &[]);
    }
    Err("no screen locker found: install nirulock or slock".into())
}

/// Close a running NIRUSCREENSAVER on this display before locking.
///
/// The screensaver holds the pointer grab, so NIRULOCK could not take it and
/// gave up: a manual lock, or the lock before a suspend, failed while the
/// screensaver was up. Resetting the X idle clock first keeps NIRUIDLE from
/// starting the screensaver again before the lock screen is ready.
fn stop_screensavers(c: &impl Connection) {
    let _ = c.force_screen_saver(ScreenSaver::RESET);
    let _ = c.flush();
    let pids = screensaver_pids(std::env::var("DISPLAY").ok().as_deref());
    if pids.is_empty() {
        return;
    }
    for pid in &pids {
        unsafe { libc::kill(*pid, libc::SIGTERM) };
    }
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline
        && pids
            .iter()
            .any(|pid| Path::new(&format!("/proc/{pid}")).exists() && !zombie(*pid))
    {
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn zombie(pid: i32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, rest)| rest.starts_with('Z'))
        })
        .unwrap_or(true)
}

/// This user's NIRUSCREENSAVER processes on DISPLAY (all of them when the
/// display is unknown), read from /proc.
fn screensaver_pids(display: Option<&str>) -> Vec<i32> {
    let uid = unsafe { libc::getuid() };
    let me = std::process::id() as i32;
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == me {
            continue;
        }
        let dir = entry.path();
        if fs::read_to_string(dir.join("comm"))
            .map(|c| c.trim() != "niruscreensaver")
            .unwrap_or(true)
        {
            continue;
        }
        let owner = fs::read_to_string(dir.join("status"))
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|l| l.strip_prefix("Uid:"))
                    .and_then(|v| v.split_whitespace().next()?.parse::<u32>().ok())
            });
        if owner != Some(uid) {
            continue;
        }
        if let Some(display) = display {
            let same = fs::read(dir.join("environ"))
                .map(|env| {
                    env.split(|b| *b == 0)
                        .any(|kv| kv == format!("DISPLAY={display}").as_bytes())
                })
                .unwrap_or(false);
            if !same {
                continue;
            }
        }
        pids.push(pid);
    }
    pids
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
    stop_screensavers(&c);
    spawn_locker(&locker, "suspend")?;
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

/// Parse the Streams section of `wpctl status`. Stream names identify active
/// applications; IDs remain the authority for any future per-stream action.
fn audio_streams_from(text: &str) -> Vec<AudioNode> {
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
        if clean == "Streams:" {
            section = "streams";
            continue;
        }
        if clean == "Sinks:" || clean == "Sources:" || clean == "Filters:" || clean.ends_with(':') {
            section = "";
            continue;
        }
        if domain != "Audio" || section != "streams" {
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

#[derive(Clone, Debug)]
struct AudioReading {
    id: u32,
    name: String,
    percent: u32,
    muted: bool,
}

fn node_reading(kind: &str) -> Result<Option<AudioReading>> {
    let nodes = audio_nodes(kind)?;
    if nodes.is_empty() {
        return Ok(None);
    }
    let node = nodes
        .into_iter()
        .find(|node| node.default)
        .ok_or("no default audio device is selected")?;
    let wpctl = program("wpctl").ok_or("audio control needs wpctl (WirePlumber)")?;
    let text = provider_query(&wpctl, &["get-volume".into(), node.id.to_string()])?;
    let value = text
        .strip_prefix("Volume:")
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= 10_000.0)
        .ok_or("audio level is invalid")?;
    Ok(Some(AudioReading {
        id: node.id,
        name: node.name.replace(['\t', '\r', '\n'], " "),
        percent: (value * 100.0).round() as u32,
        muted: text.contains("[MUTED]"),
    }))
}

fn microphone_reading() -> Result<Option<AudioReading>> {
    node_reading("sources")
}

fn print_microphone(reading: &AudioReading, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "schema": 1,
                "state": "ready",
                "id": reading.id,
                "name": reading.name,
                "percent": reading.percent,
                "muted": reading.muted
            })
        );
    } else {
        println!(
            "state=ready\nid={}\nname={}\npercent={}\nmuted={}",
            reading.id, reading.name, reading.percent, reading.muted
        );
    }
}

fn microphone_status(json: bool) {
    if program("wpctl").is_none() {
        if json {
            println!(
                "{}",
                serde_json::json!({"schema": 1, "state": "provider-unavailable", "reason": "wpctl (WirePlumber) is not installed"})
            );
        } else {
            println!("state=provider-unavailable\nreason=wpctl (WirePlumber) is not installed");
        }
        return;
    }
    match microphone_reading() {
        Ok(Some(reading)) => print_microphone(&reading, json),
        Ok(None) if json => println!(
            "{}",
            serde_json::json!({"schema": 1, "state": "no-input", "reason": "no audio input is available"})
        ),
        Ok(None) => println!("state=no-input\nreason=no audio input is available"),
        Err(error) if json => println!(
            "{}",
            serde_json::json!({"schema": 1, "state": "unavailable", "reason": error.to_string()})
        ),
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

fn audio_json_nodes(nodes: &[AudioNode]) -> Vec<serde_json::Value> {
    nodes
        .iter()
        .map(|node| {
            serde_json::json!({
                "id": node.id,
                "name": node.name,
                "default": node.default
            })
        })
        .collect()
}

fn audio_reading_json(reading: &AudioReading) -> serde_json::Value {
    serde_json::json!({
        "id": reading.id,
        "name": reading.name,
        "percent": reading.percent,
        "muted": reading.muted
    })
}

fn audio_status_json() -> Result<()> {
    if program("wpctl").is_none() {
        println!(
            "{}",
            serde_json::json!({"schema": 1, "available": false, "reason": "wpctl (WirePlumber) is not installed"})
        );
        return Ok(());
    }
    let outputs = audio_nodes("outputs")?;
    let sources = audio_nodes("sources")?;
    let streams = audio_streams_from(&provider_query(
        &program("wpctl").ok_or("audio control needs wpctl (WirePlumber)")?,
        &["status".into()],
    )?);
    let output = node_reading("outputs")?;
    let input = node_reading("sources")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema": 1,
            "available": true,
            "outputs": audio_json_nodes(&outputs),
            "sources": audio_json_nodes(&sources),
            "streams": audio_json_nodes(&streams),
            "output": output.as_ref().map(audio_reading_json),
            "input": input.as_ref().map(audio_reading_json)
        }))?
    );
    Ok(())
}

fn audio_json_nodes_command(kind: &str) -> Result<()> {
    let nodes = audio_nodes(kind)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&audio_json_nodes(&nodes))?
    );
    Ok(())
}

fn wav_analysis(bytes: &[u8]) -> Result<(f64, f64, bool, &'static str)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("recording did not produce a WAV file".into());
    }
    let mut offset = 12usize;
    let mut data: Option<&[u8]> = None;
    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start.saturating_add(size).min(bytes.len());
        if id == b"data" {
            data = Some(&bytes[start..end]);
            break;
        }
        offset = start.saturating_add(size + (size & 1));
    }
    let data = data.ok_or("recording WAV has no data chunk")?;
    if data.len() < 2 {
        return Err("recording is empty".into());
    }
    let mut peak = 0i32;
    let mut sum = 0f64;
    let mut count = 0u64;
    for chunk in data.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
        peak = peak.max(sample.abs());
        sum += (sample as f64) * (sample as f64);
        count += 1;
    }
    if count == 0 {
        return Err("recording is empty".into());
    }
    let peak_ratio = peak as f64 / 32768.0;
    let rms_ratio = (sum / count as f64).sqrt() / 32768.0;
    let dbfs = |ratio: f64| {
        if ratio > 0.0 {
            20.0 * ratio.log10()
        } else {
            -120.0
        }
    };
    let peak_db = dbfs(peak_ratio).max(-120.0);
    let rms_db = dbfs(rms_ratio).max(-120.0);
    let clipping = peak_ratio >= 0.999;
    let verdict = if clipping || peak_db > -1.0 {
        "too hot"
    } else if peak_db < -40.0 {
        "too quiet"
    } else {
        "good"
    };
    Ok((peak_db, rms_db, clipping, verdict))
}

fn microphone_test(args: &[String]) -> Result<()> {
    let json = args.iter().any(|arg| arg == "--json");
    let keep = args.iter().any(|arg| arg == "--keep");
    let seconds = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .and_then(|arg| arg.parse::<u64>().ok())
        .unwrap_or(5)
        .clamp(1, 15);
    let path = std::env::temp_dir().join(format!(
        "niructl-mic-test-{}-{}.wav",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default()
    ));
    // Prefer PipeWire's recorder: arecord talks to the raw ALSA device and
    // bypasses the calibrated WirePlumber source volume, producing a
    // misleading clipping verdict. pw-record measures the effective route.
    let (program_name, arguments) = if let Some(pw_record) = program("pw-record") {
        (
            pw_record,
            vec![
                "--rate".into(),
                "48000".into(),
                "--channels".into(),
                "1".into(),
                "--format".into(),
                "s16".into(),
                path.to_string_lossy().into_owned(),
            ],
        )
    } else if let Some(arecord) = program("arecord") {
        (
            arecord,
            vec![
                "-q".into(),
                "-f".into(),
                "S16_LE".into(),
                "-r".into(),
                "48000".into(),
                "-c".into(),
                "1".into(),
                "-d".into(),
                seconds.to_string(),
                path.to_string_lossy().into_owned(),
            ],
        )
    } else {
        return Err("microphone test needs pw-record or arecord".into());
    };
    let mut child = std::process::Command::new(program_name)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if child.try_wait()?.is_none() {
        std::thread::sleep(Duration::from_secs(seconds));
        if child.try_wait()?.is_none() {
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if child.try_wait()?.is_none() {
            let _ = child.kill();
        }
    }
    let _ = child.wait();
    let bytes = fs::read(&path)?;
    let (peak, rms, clipping, verdict) = wav_analysis(&bytes)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "schema": 1,
                "seconds": seconds,
                "peak_dbfs": (peak * 10.0).round() / 10.0,
                "rms_dbfs": (rms * 10.0).round() / 10.0,
                "clipping": clipping,
                "verdict": verdict,
                "file": if keep { Some(path.to_string_lossy().into_owned()) } else { None }
            })
        );
    } else {
        println!(
            "Peak: {peak:.1} dBFS · RMS: {rms:.1} dBFS · {} · {verdict}",
            if clipping { "CLIPPING" } else { "no clipping" }
        );
    }
    if !keep {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn audio(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("outputs");
    let wpctl = || program("wpctl").ok_or("audio control needs wpctl (WirePlumber)");
    let json = args.iter().any(|arg| arg == "--json");
    match action {
        "status" if json => audio_status_json()?,
        "status" => {
            for node in audio_nodes("outputs")? {
                println!(
                    "output {}\t{}{}",
                    node.id,
                    node.name,
                    if node.default { " · default" } else { "" }
                );
            }
            for node in audio_nodes("sources")? {
                println!(
                    "input {}\t{}{}",
                    node.id,
                    node.name,
                    if node.default { " · default" } else { "" }
                );
            }
            match microphone_reading() {
                Ok(Some(reading)) => print_microphone(&reading, false),
                Ok(None) => println!("state=no-input"),
                Err(error) => println!("state=unavailable\nreason={error}"),
            }
        }
        "mic-test" => microphone_test(&args[1..])?,
        "outputs" | "sources" if json => audio_json_nodes_command(action)?,
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
        "mic-status" => microphone_status(json),
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
        "output" | "source" | "input" => {
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
    /// Hardware identity from the EDID, saved by 0.17 and later. A profile
    /// entry with an identity follows the screen to whatever connector it is
    /// on; one without (older profiles) matches its connector, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<display_auto::Identity>,
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
        ..Default::default()
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DisplayProfile {
    #[serde(default)]
    name: String,
    #[serde(default)]
    outputs: Vec<DisplayOutput>,
    /// Connected screens the profile deliberately leaves off, such as the
    /// laptop panel in a desk profile. Recorded when the profile is saved so
    /// automatic matching can tell "off on purpose" from "unknown screen".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    off: Vec<DisplayOffOutput>,
    /// `auto = false` keeps the profile out of automatic switching.
    #[serde(default = "display_true", skip_serializing_if = "display_is_true")]
    auto: bool,
}

impl Default for DisplayProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            outputs: Vec::new(),
            off: Vec::new(),
            auto: true,
        }
    }
}

fn display_true() -> bool {
    true
}
#[allow(clippy::trivially_copy_pass_by_ref)]
fn display_is_true(value: &bool) -> bool {
    *value
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct DisplayOffOutput {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<display_auto::Identity>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct DisplayFile {
    /// Switch profiles automatically when screens or the lid change. Off
    /// unless the user turns it on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    automatic: bool,
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
                identity: None,
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
    // A screen that was unplugged can keep its CRTC (a "ghost" area); turn
    // off everything lit that the profile does not list, connected or not.
    for state in outputs {
        if state.current.is_some() && !names.contains(&state.name) {
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
    // A screen unplugged since the snapshot cannot come back; restore the
    // others rather than failing and leaving the risky layout in place.
    let still_here: Vec<DisplayOutput> = previous
        .iter()
        .filter(|output| {
            outputs.iter().any(|state| {
                state.name == output.name && state.connected && !state.modes.is_empty()
            })
        })
        .cloned()
        .collect();
    if still_here.is_empty() {
        return Err("none of the previous screens is connected".into());
    }
    let mut profile = DisplayProfile {
        name: "previous".into(),
        outputs: still_here,
        ..Default::default()
    };
    if !profile.outputs.iter().any(|output| output.primary) {
        profile.outputs[0].primary = true;
    }
    display_apply(&outputs, &profile)
}

fn display_notify(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "NIRUX", "-t", "6000", summary, body])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn display_startup_apply_profile() -> Result<Option<String>> {
    let Some(name) = display_read_file()?.startup_profile else {
        return Ok(None);
    };
    let profile = display_read_file()?
        .profile
        .into_iter()
        .find(|profile| profile.name == name)
        .ok_or_else(|| format!("startup display profile not found: {name}"))?;
    let snapshot = DisplaySnapshot::read()?;
    let profile = display_resolve_profile(&profile, &snapshot)?;
    let outputs = snapshot.outputs;
    let previous = display_current_layout(&outputs);
    if let Err(error) = display_apply(&outputs, &profile) {
        if !previous.is_empty() {
            let _ = display_restore(&previous);
        }
        return Err(error);
    }
    Ok(Some(name))
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
            if let Some(name) = display_startup_apply_profile()? {
                println!("applied startup display profile {name}");
            }
        }
        _ => return Err("display startup needs show, set NAME, off or apply".into()),
    }
    Ok(())
}

fn display_output_signature(outputs: &[DisplayOutputState]) -> Vec<(String, bool)> {
    let mut signature: Vec<(String, bool)> = outputs
        .iter()
        .filter(|output| output.connected)
        .map(|output| (output.name.clone(), output.current.is_some()))
        .collect();
    signature.sort();
    signature
}

/// Only the set of connected outputs. A manual `xrandr --output ... --off`
/// changes the enabled state but not this set, so it must not re-apply a
/// startup profile and undo the user's choice.
fn display_connected_signature(outputs: &[DisplayOutputState]) -> Vec<String> {
    let mut signature: Vec<String> = outputs
        .iter()
        .filter(|output| output.connected)
        .map(|output| output.name.clone())
        .collect();
    signature.sort();
    signature
}

fn profile_outputs_connected(outputs: &[DisplayOutputState], profile: &DisplayProfile) -> bool {
    if profile.outputs.is_empty() {
        return false;
    }
    let identities = display_identity_map();
    let screens = display_screens(outputs, &identities);
    let (on, off) = display_wanted(profile);
    !matches!(
        display_auto::match_profile(&on, &off, &screens, false).fit,
        display_auto::Fit::Missing { .. }
    )
}

fn display_watch_pid_path() -> PathBuf {
    use sha2::{Digest, Sha256};
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let key = format!("{:x}", Sha256::digest(display.as_bytes()));
    state_dir().join(format!("display-watch-{}.pid", &key[..16]))
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[derive(Debug)]
struct WatchPidFile {
    path: PathBuf,
    pid: u32,
}

impl WatchPidFile {
    fn acquire(path: PathBuf, pid: u32) -> Result<Option<Self>> {
        Self::acquire_with(path, pid, process_alive)
    }

    fn acquire_with<F>(path: PathBuf, pid: u32, alive: F) -> Result<Option<Self>>
    where
        F: Fn(u32) -> bool,
    {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        for _ in 0..4 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    writeln!(file, "{pid}")?;
                    file.sync_all().ok();
                    return Ok(Some(Self { path, pid }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = fs::read_to_string(&path)
                        .ok()
                        .and_then(|text| text.trim().parse::<u32>().ok());
                    if let Some(existing) = existing {
                        if alive(existing) {
                            return Ok(None);
                        }
                    }
                    match fs::remove_file(&path) {
                        Ok(()) => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err("could not acquire the display watch PID file".into())
    }
}

impl Drop for WatchPidFile {
    fn drop(&mut self) {
        let current = fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok());
        if current == Some(self.pid) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn display_watch_reconcile(quiet: bool, allow_startup: bool) -> Result<()> {
    let _guard = display_transaction_lock()?;
    let outputs = display_outputs()?;
    let file = display_read_file()?;
    let mut startup_error = None;
    let startup_ready = allow_startup
        && match file.startup_profile.as_deref() {
            Some(name) => match file.profile.iter().find(|profile| profile.name == name) {
                Some(profile) if profile_outputs_connected(&outputs, profile) => true,
                Some(_) => false,
                None => {
                    startup_error = Some(format!("startup display profile not found: {name}"));
                    false
                }
            },
            None => false,
        };
    if startup_ready {
        if let Some(name) = display_startup_apply_profile()? {
            let body = format!("Startup display profile {name} applied.");
            if !quiet {
                println!("display watch: {body}");
            }
            display_notify("Display layout changed", &body);
            return Ok(());
        }
    }
    let (c, screen) = x11rb::connect(None)?;
    let root = c.setup().roots[screen].root;
    let moved = windows_rescue_run(&c, root, false, quiet)?;
    let body = if let Some(error) = startup_error {
        format!("{error}; moved {moved} window(s) to the active display.")
    } else if !allow_startup {
        format!("Manual display change kept; moved {moved} window(s) to the active display.")
    } else if moved == 0 {
        "No startup profile applied; all managed windows are already on a connected display.".into()
    } else {
        format!("No startup profile applied; moved {moved} window(s) to the active display.")
    };
    if !quiet {
        println!("display watch: {body}");
    }
    display_notify("Display layout changed", &body);
    Ok(())
}

fn display_watch(args: &[String]) -> Result<()> {
    use std::os::fd::AsRawFd;
    let once = args.iter().any(|arg| arg == "--once");
    let quiet = args.iter().any(|arg| arg == "--quiet");
    if once {
        let outputs = display_outputs().map_err(|_| "cannot read the display output state")?;
        if !quiet {
            for (name, enabled) in display_output_signature(&outputs) {
                println!("{name}\t{}", if enabled { "enabled" } else { "disabled" });
            }
        }
        return Ok(());
    }
    let _guard = match WatchPidFile::acquire(display_watch_pid_path(), std::process::id())? {
        Some(guard) => guard,
        None => {
            if !quiet {
                println!("display watch is already running for this display");
            }
            return Ok(());
        }
    };
    // RandR tells us when screens, outputs or CRTCs change; nothing is
    // polled. Only the lid has no event the session can read without root,
    // so it is read once a second, and only while it can matter.
    let (c, screen) = x11rb::connect(None)?;
    let root = c.setup().roots[screen].root;
    c.randr_query_version(1, 5)?.reply()?;
    c.randr_select_input(
        root,
        x11rb::protocol::randr::NotifyMask::SCREEN_CHANGE
            | x11rb::protocol::randr::NotifyMask::OUTPUT_CHANGE
            | x11rb::protocol::randr::NotifyMask::CRTC_CHANGE,
    )?;
    c.flush()?;
    let fd = c.stream().as_raw_fd();
    let uevents = DrmUevents::open();
    if !quiet {
        println!("display watch: waiting for RandR events");
    }
    const SETTLE: Duration = Duration::from_millis(700);
    let mut seen = DisplaySeen::read();
    // The last lid state seen while it mattered, to lock on open -> closed.
    let mut lid_was = seen.lid;
    let mut pending: Option<Instant> = None;
    let mut deferred = false;
    let mut retry_next: Option<u64> = None;
    if display_read_file()
        .map(|file| file.automatic)
        .unwrap_or(false)
    {
        match display_auto_reconcile(DisplayTrigger::Start, quiet) {
            Ok(done) => deferred = !done,
            Err(error) => eprintln!("niructl: display watch: {error}"),
        }
        seen = DisplaySeen::read();
        retry_next = display_retry_next();
    }
    loop {
        let now = Instant::now();
        let mut timeout: Option<Duration> = pending.map(|at| at.saturating_duration_since(now));
        let mut tighten = |limit: Duration| {
            timeout = Some(timeout.map_or(limit, |current| current.min(limit)));
        };
        if deferred || seen.lid_matters {
            tighten(Duration::from_secs(1));
        }
        if let Some(next) = retry_next {
            tighten(Duration::from_secs(next.saturating_sub(now_secs()).max(1)));
        }
        let mut events = false;
        while let Some(event) = c.poll_for_event()? {
            events |= matches!(
                event,
                x11rb::protocol::Event::RandrScreenChangeNotify(_)
                    | x11rb::protocol::Event::RandrNotify(_)
            );
        }
        if !events {
            let mut pfds = [
                libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: uevents.as_ref().map_or(-1, |u| u.fd),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let millis = timeout.map_or(-1, |t| t.as_millis().min(i32::MAX as u128) as i32);
            unsafe { libc::poll(pfds.as_mut_ptr(), 2, millis) };
            if uevents.as_ref().is_some_and(|u| u.drain()) {
                events = true;
            }
            while let Some(event) = c.poll_for_event()? {
                events |= matches!(
                    event,
                    x11rb::protocol::Event::RandrScreenChangeNotify(_)
                        | x11rb::protocol::Event::RandrNotify(_)
                );
            }
        }
        if events {
            pending = Some(Instant::now() + SETTLE);
        }
        let lid_now = if seen.lid_matters {
            display_lid_closed()
        } else {
            None
        };
        if lid_closed_now(lid_was, lid_now) {
            lock_on_docked_lid_close();
        }
        lid_was = lid_now;
        if seen.lid_matters && pending.is_none() && display_lid_closed() != seen.lid {
            pending = Some(Instant::now() + Duration::from_millis(300));
        }
        if deferred && pending.is_none() && display_preview_read().is_none() {
            pending = Some(Instant::now());
        }
        // The file is read only when something is due, not on every lid poll.
        let due = pending.is_some_and(|at| Instant::now() >= at)
            || (pending.is_none() && retry_next.is_some_and(|next| now_secs() >= next));
        if !due {
            continue;
        }
        let automatic = display_read_file()
            .map(|file| file.automatic)
            .unwrap_or(false);
        if automatic && pending.is_none() && retry_next.is_some_and(|next| now_secs() >= next) {
            if let Err(error) = display_auto_reconcile(DisplayTrigger::Retry, quiet) {
                eprintln!("niructl: display watch: {error}");
            }
            seen = DisplaySeen::read();
            retry_next = display_retry_next();
            continue;
        }
        if !pending.is_some_and(|at| Instant::now() >= at) {
            continue;
        }
        pending = None;
        let now_seen = DisplaySeen::read();
        if display_preview_read().is_some() {
            // Never act during a preview; look again when it has ended.
            deferred = true;
            continue;
        }
        deferred = false;
        if now_seen.set != seen.set {
            if automatic {
                match display_auto_reconcile(DisplayTrigger::Screens, quiet) {
                    Ok(done) => deferred = !done,
                    Err(error) => eprintln!("niructl: display watch: {error}"),
                }
            } else if now_seen.legacy != seen.legacy {
                let allow_startup = now_seen.connected != seen.connected;
                if let Err(error) = display_watch_reconcile(quiet, allow_startup) {
                    eprintln!("niructl: display watch: {error}");
                }
            }
        } else if now_seen.layout != seen.layout {
            if automatic {
                if let Err(error) = display_auto_observe() {
                    eprintln!("niructl: display watch: {error}");
                }
            } else if now_seen.legacy != seen.legacy {
                // A manual change keeps the user's choice; it only re-homes
                // windows, exactly as before the automation existed.
                let allow_startup = now_seen.connected != seen.connected;
                if let Err(error) = display_watch_reconcile(quiet, allow_startup) {
                    eprintln!("niructl: display watch: {error}");
                }
            }
            if !automatic {
                let _ = display_auto_observe();
            }
        }
        // Record the settled state, so our own change does not look like a
        // new one when its RandR events arrive.
        seen = DisplaySeen::read();
        retry_next = if automatic {
            display_retry_next()
        } else {
            None
        };
    }
}

/// Kernel uevents for the drm subsystem. A real hotplug sends one; X reads
/// it through udev and sends RandR events, but not every server or driver
/// reprobes by itself (amdgpu reports a lost screen only on the next query).
/// Listening here makes the watcher independent of that. None when the
/// socket cannot be opened; RandR events still work then.
struct DrmUevents {
    fd: libc::c_int,
}

impl DrmUevents {
    fn open() -> Option<Self> {
        const NETLINK_KOBJECT_UEVENT: libc::c_int = 15;
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                NETLINK_KOBJECT_UEVENT,
            )
        };
        if fd < 0 {
            return None;
        }
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        addr.nl_groups = 1; // kernel broadcasts
        let bound = unsafe {
            libc::bind(
                fd,
                &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if bound != 0 {
            unsafe { libc::close(fd) };
            return None;
        }
        Some(Self { fd })
    }

    /// Read every queued message; true when one was a drm change.
    fn drain(&self) -> bool {
        let mut found = false;
        let mut buffer = [0u8; 8192];
        loop {
            let read = unsafe {
                libc::recv(
                    self.fd,
                    buffer.as_mut_ptr() as *mut libc::c_void,
                    buffer.len(),
                    0,
                )
            };
            if read <= 0 {
                return found;
            }
            found |= drm_uevent_is_hotplug(&buffer[..read as usize]);
        }
    }
}

impl Drop for DrmUevents {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

/// A kernel uevent is NUL-separated `KEY=value` text after an
/// `action@devpath` header. Any `change` of a drm card counts: real
/// hotplugs carry `HOTPLUG=1`, `udevadm trigger` sends a bare change.
fn drm_uevent_is_hotplug(message: &[u8]) -> bool {
    let mut subsystem = false;
    let mut change = false;
    for field in message.split(|byte| *byte == 0) {
        match field {
            b"SUBSYSTEM=drm" => subsystem = true,
            b"ACTION=change" => change = true,
            _ => {}
        }
    }
    subsystem && change
}

/// What the watcher compares between two events.
struct DisplaySeen {
    set: Vec<String>,
    layout: Vec<String>,
    legacy: Vec<(String, bool)>,
    connected: Vec<String>,
    lid: Option<bool>,
    lid_matters: bool,
}

impl DisplaySeen {
    fn read() -> Self {
        match DisplaySnapshot::read() {
            Ok(snapshot) => Self {
                set: snapshot.set(),
                layout: snapshot.layout_signature(),
                legacy: display_output_signature(&snapshot.outputs),
                connected: display_connected_signature(&snapshot.outputs),
                lid: snapshot.lid,
                lid_matters: snapshot.lid_matters(),
            },
            Err(error) => {
                eprintln!("niructl: display watch: {error}");
                Self {
                    set: Vec::new(),
                    layout: Vec::new(),
                    legacy: Vec::new(),
                    connected: Vec::new(),
                    lid: None,
                    lid_matters: false,
                }
            }
        }
    }
}

fn display_retry_next() -> Option<u64> {
    let state: DisplayAutoState = display_read_toml(&display_auto_state_path());
    state.retry.next
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
    let snapshot = DisplaySnapshot::read()?;
    let outputs = &snapshot.outputs;
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
        "outputs": outputs.iter().map(|output| {
            let mut value = display_output_json(output, &identities);
            if let Some(screen) = snapshot.screens.iter().find(|screen| screen.connector == output.name) {
                value["key"] = serde_json::Value::String(screen.key());
            }
            value["internal"] = serde_json::Value::Bool(display_auto::is_internal(&output.name));
            value
        }).collect::<Vec<_>>(),
        "profiles": file.profile.iter().map(|profile| display_profile_status_json(profile, &snapshot)).collect::<Vec<_>>(),
        "startup_profile": file.startup_profile,
        "preview": preview,
        "automatic": display_auto_json(&file, &snapshot),
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

fn display_delete_profile(file: &mut DisplayFile, name: &str) -> Result<()> {
    let before = file.profile.len();
    file.profile.retain(|profile| profile.name != name);
    if file.profile.len() == before {
        return Err(format!("unknown display profile: {name}").into());
    }
    if file.startup_profile.as_deref() == Some(name) {
        file.startup_profile = None;
    }
    Ok(())
}

fn display_rename_profile(file: &mut DisplayFile, old: &str, new: &str) -> Result<()> {
    if new.is_empty() || new.chars().any(|c| c.is_control()) {
        return Err("invalid display profile name".into());
    }
    if old != new && file.profile.iter().any(|profile| profile.name == new) {
        return Err(format!("display profile already exists: {new}").into());
    }
    let profile = file
        .profile
        .iter_mut()
        .find(|profile| profile.name == old)
        .ok_or_else(|| format!("unknown display profile: {old}"))?;
    profile.name = new.to_string();
    if file.startup_profile.as_deref() == Some(old) {
        file.startup_profile = Some(new.to_string());
    }
    Ok(())
}

fn display_save_profile(profile: &DisplayProfile, name: &str) -> Result<()> {
    if name.is_empty() || name.chars().any(|c| c.is_control()) {
        return Err("invalid display profile name".into());
    }
    let mut file = display_read_file()?;
    let mut saved = profile.clone();
    saved.name = name.to_string();
    let previous = file
        .profile
        .iter()
        .find(|profile| profile.name == name)
        .cloned();
    let saved = display_profile_for_saving(saved, previous.as_ref());
    // Keep the profile's place in the list when it is updated.
    match file.profile.iter().position(|profile| profile.name == name) {
        Some(index) => file.profile[index] = saved,
        None => file.profile.push(saved),
    }
    display_write_file(&file)?;
    println!("saved display profile {name}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Hardware identities, automatic switching, verification and doctor.
//
// The decisions live in `display_auto` (pure, unit-tested); this part reads
// the screens, applies layouts through the same xrandr path as profiles and
// keeps two small state files next to the preview transaction:
// `display-memory.toml` (last layout per screen set, last mode per screen)
// and `display-auto.toml` (what the automation did last, a pending retry).
// ---------------------------------------------------------------------------

/// Test seam: a file holding `open` or `closed` replaces the ACPI lid state.
/// Documented in docs/niructl-display.md; nothing in a session sets it.
const DISPLAY_LID_OVERRIDE: &str = "NIRUX_DISPLAY_LID_FILE";

fn display_identity_map() -> HashMap<String, display_auto::Identity> {
    display_edid_identities()
        .into_iter()
        .filter_map(|(name, value)| {
            serde_json::from_value::<display_auto::Identity>(value)
                .ok()
                .map(|identity| (name, identity))
        })
        .collect()
}

fn display_screens(
    outputs: &[DisplayOutputState],
    identities: &HashMap<String, display_auto::Identity>,
) -> Vec<display_auto::Screen> {
    outputs
        .iter()
        .filter(|state| state.connected)
        .map(|state| display_auto::Screen {
            connector: state.name.clone(),
            identity: identities.get(&state.name).cloned(),
            modes: state
                .modes
                .iter()
                .filter_map(|mode| {
                    let (width, height) = display_mode_size(&mode.mode, state)?;
                    Some(display_auto::Mode {
                        name: mode.mode.clone(),
                        width,
                        height,
                        rates: mode.rates.clone(),
                    })
                })
                .collect(),
            preferred: state.preferred.clone(),
            current: state.current.as_ref().map(|current| {
                let (x, y) = display_position_pair(&current.position).unwrap_or((0, 0));
                display_auto::Current {
                    mode: current.mode.clone(),
                    rate: current.rate,
                    rotation: current.rotation.clone(),
                    x,
                    y,
                    primary: current.primary,
                }
            }),
        })
        .collect()
}

/// True on the open -> closed edge only: a lid that was already closed when
/// watching started, or that stays closed, does not lock again.
fn lid_closed_now(was: Option<bool>, now: Option<bool>) -> bool {
    was == Some(false) && now == Some(true)
}

/// Closing the lid while other screens keep the machine awake (docked:
/// logind ignores the lid then) locks the session, unless
/// `[idle] lock_on_lid_close = false`. Undocked, logind suspends and the
/// sleep-lock helper locks first, so this only covers the docked case.
fn lock_on_docked_lid_close() {
    let wanted = Config::load()
        .map(|cfg| cfg.idle.lock_on_lid_close)
        .unwrap_or(true);
    if !wanted {
        return;
    }
    eprintln!("niructl: display watch: lid closed while docked; locking");
    if let Err(error) = lock_action("suspend") {
        eprintln!("niructl: display watch: lock on lid close failed: {error}");
    }
}

/// The ACPI lid switch, when the machine has one.
fn display_lid_closed() -> Option<bool> {
    let text = if let Some(path) = std::env::var_os(DISPLAY_LID_OVERRIDE) {
        fs::read_to_string(path).ok()?
    } else {
        let dir = fs::read_dir("/proc/acpi/button/lid").ok()?;
        let mut found = None;
        for entry in dir.flatten() {
            if let Ok(text) = fs::read_to_string(entry.path().join("state")) {
                found = Some(text);
                break;
            }
        }
        found?
    };
    if text.contains("closed") {
        Some(true)
    } else if text.contains("open") {
        Some(false)
    } else {
        None
    }
}

/// One consistent reading of everything the automation looks at.
struct DisplaySnapshot {
    outputs: Vec<DisplayOutputState>,
    identities: HashMap<String, display_auto::Identity>,
    screens: Vec<display_auto::Screen>,
    lid: Option<bool>,
}

impl DisplaySnapshot {
    fn read() -> Result<Self> {
        let outputs = display_outputs()?;
        let identities = display_identity_map();
        let screens = display_screens(&outputs, &identities);
        Ok(Self {
            outputs,
            identities,
            screens,
            lid: display_lid_closed(),
        })
    }
    fn lid_closed(&self) -> bool {
        self.lid == Some(true)
    }
    fn set(&self) -> Vec<String> {
        display_auto::set_signature(&self.screens, self.lid_closed())
    }
    /// Every lit screen with its placement: what "the layout changed" means.
    fn layout_signature(&self) -> Vec<String> {
        let mut lit: Vec<String> = self
            .outputs
            .iter()
            .filter_map(|state| {
                let current = state.current.as_ref()?;
                Some(format!(
                    "{} {} {:?} {} {} {}",
                    state.name,
                    current.mode,
                    current.rate,
                    current.position,
                    current.rotation,
                    current.primary
                ))
            })
            .collect();
        lit.sort();
        lit
    }
    /// Outputs that lost their screen but still hold a CRTC: an area of the
    /// desktop nobody can see.
    fn ghosts(&self) -> Vec<String> {
        self.outputs
            .iter()
            .filter(|state| !state.connected && state.current.is_some())
            .map(|state| state.name.clone())
            .collect()
    }
    fn has_internal(&self) -> bool {
        self.screens.iter().any(|screen| screen.internal())
    }
    /// The lid only matters with a panel and another screen to show, or
    /// while the panel is off; otherwise logind suspends or nothing changes.
    fn lid_matters(&self) -> bool {
        self.lid.is_some()
            && self.has_internal()
            && (self
                .screens
                .iter()
                .any(|screen| !screen.internal() && screen.usable())
                || self
                    .screens
                    .iter()
                    .any(|screen| screen.internal() && screen.current.is_none()))
    }
}

fn display_wanted(
    profile: &DisplayProfile,
) -> (Vec<display_auto::Wanted<'_>>, Vec<display_auto::Wanted<'_>>) {
    let on = profile
        .outputs
        .iter()
        .map(|output| display_auto::Wanted {
            connector: &output.name,
            identity: output.identity.as_ref(),
        })
        .collect();
    let off = profile
        .off
        .iter()
        .map(|output| display_auto::Wanted {
            connector: &output.name,
            identity: output.identity.as_ref(),
        })
        .collect();
    (on, off)
}

fn display_entry_label(output: &DisplayOutput) -> String {
    match &output.identity {
        Some(identity) if display_auto::is_internal(&output.name) => {
            format!("the built-in screen ({})", identity.describe())
        }
        Some(identity) => identity.describe(),
        None => output.name.clone(),
    }
}

fn display_match(
    profile: &DisplayProfile,
    snapshot: &DisplaySnapshot,
    lid_closed: bool,
) -> display_auto::ProfileMatch {
    let (on, off) = display_wanted(profile);
    display_auto::match_profile(&on, &off, &snapshot.screens, lid_closed)
}

/// Why a profile does not fit the screens that are connected now.
fn display_mismatch_reason(
    profile: &DisplayProfile,
    matched: &display_auto::ProfileMatch,
    snapshot: &DisplaySnapshot,
) -> Option<String> {
    match &matched.fit {
        display_auto::Fit::Exact => None,
        display_auto::Fit::Partial { extra } => Some(format!(
            "also connected: {}",
            extra
                .iter()
                .map(|connector| {
                    snapshot
                        .screens
                        .iter()
                        .find(|screen| &screen.connector == connector)
                        .and_then(|screen| screen.identity.as_ref())
                        .map(|identity| format!("{connector} ({})", identity.describe()))
                        .unwrap_or_else(|| connector.clone())
                })
                .collect::<Vec<_>>()
                .join(", ")
        )),
        display_auto::Fit::Missing { entries } => {
            let needs: Vec<String> = entries
                .iter()
                .filter_map(|index| profile.outputs.get(*index))
                .map(|output| {
                    if display_auto::is_internal(&output.name) && snapshot.lid_closed() {
                        "the built-in screen (the lid is closed)".to_string()
                    } else {
                        display_entry_label(output)
                    }
                })
                .collect();
            Some(format!("needs {}", needs.join(" and ")))
        }
    }
}

/// The profile with every entry moved to the connector its screen is on now.
/// The lid is ignored: a profile can always be loaded and applied by hand.
fn display_resolve_profile(
    profile: &DisplayProfile,
    snapshot: &DisplaySnapshot,
) -> Result<DisplayProfile> {
    let matched = display_match(profile, snapshot, false);
    if let display_auto::Fit::Missing { .. } = matched.fit {
        let reason = display_mismatch_reason(profile, &matched, snapshot).unwrap_or_default();
        return Err(format!("display profile {} does not fit: {reason}", profile.name).into());
    }
    let mut resolved = profile.clone();
    for (output, connector) in resolved.outputs.iter_mut().zip(matched.on.iter()) {
        output.name = connector.clone();
    }
    Ok(resolved)
}

/// Turn a resolved profile into concrete placements ("preferred" becomes the
/// screen's preferred mode), for verification and the memory.
///
/// A mode or rate the screen does not offer is kept as asked: applying it
/// fails, and the automation then relaxes it instead of dropping the screen.
fn display_placements(
    profile: &DisplayProfile,
    snapshot: &DisplaySnapshot,
) -> Vec<display_auto::Placement> {
    profile
        .outputs
        .iter()
        .filter_map(|output| {
            let state = snapshot
                .outputs
                .iter()
                .find(|state| state.name == output.name)?;
            let mode = if output.mode == "preferred" || output.mode.is_empty() {
                state
                    .preferred
                    .clone()
                    .or_else(|| state.modes.first().map(|mode| mode.mode.clone()))?
            } else {
                output.mode.clone()
            };
            let (x, y) = display_position_pair(&output.position)?;
            Some(display_auto::Placement {
                connector: output.name.clone(),
                mode,
                rate: output.rate,
                rotation: output.rotation.clone(),
                x,
                y,
                primary: output.primary,
            })
        })
        .collect()
}

fn display_profile_from_placements(
    name: &str,
    placements: &[display_auto::Placement],
    identities: &HashMap<String, display_auto::Identity>,
) -> DisplayProfile {
    DisplayProfile {
        name: name.to_string(),
        outputs: placements
            .iter()
            .map(|placement| DisplayOutput {
                name: placement.connector.clone(),
                mode: placement.mode.clone(),
                rate: placement.rate,
                position: format!("{}x{}", placement.x, placement.y),
                rotation: placement.rotation.clone(),
                primary: placement.primary,
                identity: identities.get(&placement.connector).cloned(),
            })
            .collect(),
        ..Default::default()
    }
}

/// The profile whose layout the screens show right now, if any.
fn display_current_profile(file: &DisplayFile, snapshot: &DisplaySnapshot) -> Option<String> {
    file.profile.iter().find_map(|profile| {
        let resolved = display_resolve_profile(profile, snapshot).ok()?;
        let placements = display_placements(&resolved, snapshot);
        (placements.len() == resolved.outputs.len()
            && display_auto::verify(&placements, &snapshot.screens).is_empty())
        .then(|| profile.name.clone())
    })
}

/// Attach identities and the deliberately-off screens before a profile is
/// written. Re-saving an older profile is what upgrades it.
fn display_profile_for_saving(
    mut profile: DisplayProfile,
    previous: Option<&DisplayProfile>,
) -> DisplayProfile {
    let identities = display_identity_map();
    let outputs = display_outputs().unwrap_or_default();
    for output in profile.outputs.iter_mut() {
        if let Some(identity) = identities.get(&output.name) {
            output.identity = Some(identity.clone());
        }
    }
    profile.off = outputs
        .iter()
        .filter(|state| state.connected && !state.modes.is_empty())
        .filter(|state| {
            !profile
                .outputs
                .iter()
                .any(|output| output.name == state.name)
        })
        .map(|state| DisplayOffOutput {
            name: state.name.clone(),
            identity: identities.get(&state.name).cloned(),
        })
        .collect();
    if let Some(previous) = previous {
        profile.auto = previous.auto;
    }
    profile
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DisplayAutoEvent {
    time: u64,
    /// profile, remembered, extended, lid, ambiguous, retry, relaxed, failed
    action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
    detail: String,
    ok: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DisplayAutoState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last: Option<DisplayAutoEvent>,
    #[serde(default)]
    retry: display_auto::Retry,
    /// The full layout a retry tries again, for the set it was made for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retry_target: Vec<display_auto::Placement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retry_set: Vec<String>,
    /// Set while the layout came from the memory or the fallback rather than
    /// a profile: the editor offers to save it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    automatic_layout: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ambiguous: Vec<String>,
}

fn display_auto_state_path() -> PathBuf {
    display_preview_path().with_file_name("display-auto.toml")
}
fn display_memory_path() -> PathBuf {
    display_preview_path().with_file_name("display-memory.toml")
}

fn display_read_toml<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

fn display_write_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("new");
    fs::write(&temp, toml::to_string(value)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

/// Apply placements, read the result back and relax what the screens
/// refused. Returns the placements that are in effect and whether they are
/// the ones asked for.
fn display_apply_verified(
    wanted: &[display_auto::Placement],
    snapshot: &DisplaySnapshot,
) -> Result<(Vec<display_auto::Placement>, bool, DisplaySnapshot)> {
    let mut attempt = wanted.to_vec();
    let mut step = 0;
    loop {
        let profile = display_profile_from_placements("automatic", &attempt, &snapshot.identities);
        let applied = display_apply(&snapshot.outputs, &profile);
        let after = DisplaySnapshot::read()?;
        let mut wrong = display_auto::verify(&attempt, &after.screens);
        wrong.extend(after.ghosts());
        if applied.is_ok() && wrong.is_empty() {
            return Ok((attempt, step == 0, after));
        }
        step += 1;
        let failed = if wrong.is_empty() {
            attempt.iter().map(|p| p.connector.clone()).collect()
        } else {
            wrong
        };
        match display_auto::degrade(wanted, &failed, &after.screens, step) {
            Some(relaxed) => attempt = relaxed,
            None => {
                // Nothing left to relax. Never leave every screen dark.
                if after.screens.iter().all(|screen| screen.current.is_none()) {
                    let rescue = display_auto::extend(
                        &after.screens,
                        false,
                        &display_auto::Memory::default(),
                    );
                    let profile =
                        display_profile_from_placements("rescue", &rescue, &after.identities);
                    let _ = display_apply(&after.outputs, &profile);
                    let after = DisplaySnapshot::read()?;
                    return Ok((rescue, false, after));
                }
                return Ok((attempt, false, after));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DisplayTrigger {
    /// The watcher started, or the automation was just turned on.
    Start,
    /// The set of screens changed (hotplug, or the lid hid or showed the panel).
    Screens,
    /// A relaxed layout is due for another try.
    Retry,
}

fn display_notify_auto(summary: &str, body: &str) {
    display_notify(summary, body);
}

/// Decide and apply the layout for the screens connected now. Holds the
/// display transaction lock; never runs while a preview is pending.
/// Returns false when it deferred because of a preview.
fn display_auto_reconcile(trigger: DisplayTrigger, quiet: bool) -> Result<bool> {
    let _guard = display_transaction_lock()?;
    if display_preview_read().is_some() {
        return Ok(false);
    }
    let file = display_read_file()?;
    let snapshot = DisplaySnapshot::read()?;
    let lid_closed = snapshot.lid_closed();
    let set = snapshot.set();
    let mut memory: display_auto::Memory = display_read_toml(&display_memory_path());
    let mut state: DisplayAutoState = display_read_toml(&display_auto_state_path());
    let now = now_secs();
    let log = |text: &str| {
        if !quiet {
            println!("display auto: {text}");
        }
    };

    if trigger == DisplayTrigger::Retry {
        if state.retry_set != set || state.retry_target.is_empty() {
            state.retry.clear();
            state.retry_target.clear();
            state.retry_set.clear();
            display_write_toml(&display_auto_state_path(), &state)?;
            return Ok(true);
        }
        let target = state.retry_target.clone();
        let (_, exact, after) = display_apply_verified(&target, &snapshot)?;
        if exact {
            log("the saved settings were accepted on retry");
            state.retry.clear();
            state.retry_target.clear();
            state.retry_set.clear();
            state.last = Some(DisplayAutoEvent {
                time: now,
                action: "retry".into(),
                profile: display_current_profile(&file, &after),
                detail: "Saved settings accepted on retry.".into(),
                ok: true,
            });
        } else {
            let delay = state.retry.failed(now);
            log(&format!("still relaxed; next try in {delay}s"));
        }
        let current = display_current_profile(&file, &after);
        if memory.record(&after.screens, after.lid_closed(), current.as_deref(), now) {
            display_write_toml(&display_memory_path(), &memory)?;
        }
        display_write_toml(&display_auto_state_path(), &state)?;
        return Ok(true);
    }

    // A new set clears an old retry: it was for other screens.
    if state.retry_set != set {
        state.retry.clear();
        state.retry_target.clear();
        state.retry_set.clear();
    }
    let matches: Vec<(usize, display_auto::ProfileMatch)> = file
        .profile
        .iter()
        .enumerate()
        .map(|(index, profile)| (index, display_match(profile, &snapshot, lid_closed)))
        .collect();
    let candidates: Vec<display_auto::Candidate> = matches
        .iter()
        .map(|(index, matched)| display_auto::Candidate {
            name: &file.profile[*index].name,
            auto: file.profile[*index].auto,
            matched,
        })
        .collect();
    let preferred = if trigger == DisplayTrigger::Start {
        file.startup_profile.as_deref()
    } else {
        None
    };
    let last = memory
        .layout(&set)
        .and_then(|layout| layout.profile.as_deref());
    let choice = display_auto::choose(&candidates, preferred, last);
    let description = display_auto::describe_set(&snapshot.screens, lid_closed);
    let (target, action, profile_name, summary, body): (Vec<display_auto::Placement>, &str, Option<String>, String, String) =
        match &choice {
            display_auto::Choice::Profile(name) => {
                let index = file.profile.iter().position(|p| &p.name == name).unwrap_or(0);
                let (_, matched) = &matches[index];
                let mut resolved = file.profile[index].clone();
                for (output, connector) in resolved.outputs.iter_mut().zip(matched.on.iter()) {
                    output.name = connector.clone();
                }
                (
                    display_placements(&resolved, &snapshot),
                    "profile",
                    Some(name.clone()),
                    format!("Display profile: {name}"),
                    format!("Applied automatically for {description}."),
                )
            }
            display_auto::Choice::Ambiguous(names) => {
                let fallback = memory
                    .restore(&snapshot.screens, lid_closed)
                    .unwrap_or_else(|| display_auto::extend(&snapshot.screens, lid_closed, &memory));
                (
                    fallback,
                    "ambiguous",
                    None,
                    "Display profiles: choose one".to_string(),
                    format!(
                        "{} fit {description}. Kept a usable layout; pick one in NIRU Display.",
                        names.join(", ")
                    ),
                )
            }
            display_auto::Choice::None if display_auto::effective(&snapshot.screens, lid_closed).is_empty() => (
                // The lid is closed and nothing else is connected: the panel
                // stays (or comes back) on so the session is never dark.
                display_auto::extend(&snapshot.screens, lid_closed, &memory),
                "fallback",
                None,
                "Display layout".to_string(),
                "No other screen is connected: the built-in screen stays on.".to_string(),
            ),
            display_auto::Choice::None => match memory.restore(&snapshot.screens, lid_closed) {
                Some(remembered) => (
                    remembered,
                    "remembered",
                    None,
                    "Display layout restored".to_string(),
                    format!("Last layout for {description}."),
                ),
                None => (
                    display_auto::extend(&snapshot.screens, lid_closed, &memory),
                    "extended",
                    None,
                    "Display layout".to_string(),
                    format!("{description}: new screens placed to the right. Save it as a profile in NIRU Display."),
                ),
            },
        };
    state.ambiguous = match &choice {
        display_auto::Choice::Ambiguous(names) => names.clone(),
        _ => Vec::new(),
    };
    if target.is_empty() {
        display_write_toml(&display_auto_state_path(), &state)?;
        return Ok(true);
    }
    let already =
        display_auto::verify(&target, &snapshot.screens).is_empty() && snapshot.ghosts().is_empty();
    let snapshot_lit: Vec<String> = snapshot
        .screens
        .iter()
        .filter(|screen| screen.current.is_some())
        .map(|screen| screen.connector.clone())
        .collect();
    // "Extended" without a new screen (the lid hid the panel, a screen
    // left) only kept the layout; say so.
    let lit_before = |connector: &str| snapshot_lit.iter().any(|lit| lit == connector);
    let added = target
        .iter()
        .any(|placement| !lit_before(&placement.connector));
    let body = if action == "extended" && (already || !added) {
        format!("Kept the layout for {description}.")
    } else {
        body
    };

    let (in_effect, exact, after) = if already {
        (target.clone(), true, snapshot)
    } else {
        display_apply_verified(&target, &snapshot)?
    };
    let lid_note = if lid_closed
        && after.has_internal()
        && after
            .screens
            .iter()
            .any(|screen| screen.internal() && screen.current.is_none())
    {
        " The lid is closed: built-in screen off."
    } else {
        ""
    };
    if !exact {
        state.retry_target = target.clone();
        state.retry_set = set.clone();
        let delay = state.retry.failed(now);
        display_notify_auto(
            "Display settings relaxed",
            &format!("A screen refused its saved mode or rate; using a safe one and trying again in {delay}s."),
        );
        log(&format!("relaxed {} ; retry in {delay}s", action));
    }
    if !already {
        let mut text = body.clone();
        text.push_str(lid_note);
        display_notify_auto(&summary, &text);
        log(&format!("{summary}: {text}"));
    } else {
        log(&format!("{action}: already in effect"));
    }
    state.automatic_layout = match action {
        "profile" | "fallback" => None,
        _ => Some(description.clone()),
    };
    if !already || state.last.is_none() || trigger == DisplayTrigger::Screens {
        state.last = Some(DisplayAutoEvent {
            time: now,
            action: if exact {
                action.to_string()
            } else {
                "relaxed".into()
            },
            profile: profile_name,
            detail: if already {
                format!("{body} (already in effect)")
            } else {
                body
            },
            ok: exact,
        });
    }
    let _ = in_effect;
    let current = display_current_profile(&file, &after);
    if memory.record(&after.screens, after.lid_closed(), current.as_deref(), now) {
        display_write_toml(&display_memory_path(), &memory)?;
    }
    display_write_toml(&display_auto_state_path(), &state)?;
    if let Ok((c, screen)) = x11rb::connect(None) {
        let root = c.setup().roots[screen].root;
        let _ = windows_rescue_run(&c, root, false, true);
    }
    Ok(true)
}

/// Remember a layout the user arranged by hand, without acting on it.
fn display_auto_observe() -> Result<()> {
    let _guard = display_transaction_lock()?;
    if display_preview_read().is_some() {
        return Ok(());
    }
    let file = display_read_file()?;
    let snapshot = DisplaySnapshot::read()?;
    let current = display_current_profile(&file, &snapshot);
    let mut memory: display_auto::Memory = display_read_toml(&display_memory_path());
    if memory.record(
        &snapshot.screens,
        snapshot.lid_closed(),
        current.as_deref(),
        now_secs(),
    ) {
        display_write_toml(&display_memory_path(), &memory)?;
    }
    let mut state: DisplayAutoState = display_read_toml(&display_auto_state_path());
    let mut changed = false;
    if current.is_some() && state.automatic_layout.is_some() {
        state.automatic_layout = None;
        changed = true;
    }
    // The user changed the layout: a pending retry would undo that.
    if state.retry.next.is_some() {
        state.retry.clear();
        state.retry_target.clear();
        state.retry_set.clear();
        changed = true;
    }
    if changed {
        display_write_toml(&display_auto_state_path(), &state)?;
    }
    Ok(())
}

fn display_auto_json(file: &DisplayFile, snapshot: &DisplaySnapshot) -> serde_json::Value {
    let state: DisplayAutoState = display_read_toml(&display_auto_state_path());
    let current = display_current_profile(file, snapshot);
    serde_json::json!({
        "enabled": file.automatic,
        "lid": snapshot.lid.map(|closed| if closed { "closed" } else { "open" }),
        "last": state.last.as_ref().map(|event| serde_json::json!({
            "time": event.time,
            "action": event.action,
            "profile": event.profile,
            "detail": event.detail,
            "ok": event.ok,
        })),
        "retry_in": state.retry.seconds_left(now_secs()),
        "ambiguous": state.ambiguous,
        "automatic_layout": if current.is_none() { state.automatic_layout.clone() } else { None },
        "current_profile": current,
    })
}

fn display_profile_status_json(
    profile: &DisplayProfile,
    snapshot: &DisplaySnapshot,
) -> serde_json::Value {
    let lid_closed = snapshot.lid_closed();
    let matched = display_match(profile, snapshot, lid_closed);
    let loadable = display_match(profile, snapshot, false);
    let fit = match &matched.fit {
        display_auto::Fit::Exact => "exact",
        display_auto::Fit::Partial { .. } => "partial",
        display_auto::Fit::Missing { .. } => "missing",
    };
    let names: Vec<String> = if loadable.on.len() == profile.outputs.len() {
        loadable.on.clone()
    } else {
        profile
            .outputs
            .iter()
            .map(|output| output.name.clone())
            .collect()
    };
    serde_json::json!({
        "name": profile.name,
        "auto": profile.auto,
        "fit": fit,
        "reason": display_mismatch_reason(profile, &matched, snapshot),
        // Every screen it names is connected (the lid aside): it can be
        // loaded and applied by hand.
        "loadable": !matches!(loadable.fit, display_auto::Fit::Missing { .. }),
        "outputs": profile.outputs.iter().zip(names.iter()).map(|(output, name)| serde_json::json!({
            "name": name,
            "saved_name": output.name,
            "label": display_entry_label(output),
            "identity": output.identity,
            "mode": output.mode,
            "rate": output.rate,
            "position": output.position,
            "rotation": output.rotation,
            "primary": output.primary,
        })).collect::<Vec<_>>(),
        "off": profile.off.iter().map(|output| output.name.clone()).collect::<Vec<_>>(),
    })
}

fn display_doctor(json: bool) -> Result<()> {
    let file = display_read_file()?;
    let snapshot = DisplaySnapshot::read()?;
    let lid_closed = snapshot.lid_closed();
    let state: DisplayAutoState = display_read_toml(&display_auto_state_path());
    let mut problems: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    // Screens that cannot be told apart.
    let mut seen: HashMap<String, Vec<String>> = HashMap::new();
    for screen in &snapshot.screens {
        if let Some(identity) = &screen.identity {
            seen.entry(identity.key())
                .or_default()
                .push(screen.connector.clone());
        } else if screen.usable() {
            notes.push(format!(
                "{}: no EDID; profiles match it by connector only",
                screen.connector
            ));
        }
    }
    for (key, connectors) in &seen {
        if connectors.len() > 1 {
            notes.push(format!(
                "{} report the same identity ({key}); they are told apart by connector name",
                connectors.join(" and ")
            ));
        }
    }
    let mut profiles = Vec::new();
    for profile in &file.profile {
        let mut issues = Vec::new();
        let mut names = HashSet::new();
        for output in &profile.outputs {
            if !names.insert(output.name.clone()) {
                issues.push(format!("{} is listed twice", output.name));
            }
            if !matches!(
                output.rotation.as_str(),
                "normal" | "left" | "right" | "inverted"
            ) {
                issues.push(format!(
                    "{}: invalid rotation {}",
                    output.name, output.rotation
                ));
            }
            if display_position_pair(&output.position).is_none() {
                issues.push(format!(
                    "{}: invalid position {}",
                    output.name, output.position
                ));
            }
            if output.mode != "preferred" && !output.mode.contains('x') {
                issues.push(format!("{}: invalid mode {}", output.name, output.mode));
            }
        }
        if profile.outputs.is_empty() {
            issues.push("no screens: it can never be applied".into());
        }
        if profile
            .outputs
            .iter()
            .filter(|output| output.primary)
            .count()
            > 1
        {
            issues.push("more than one primary screen".into());
        }
        let matched = display_match(profile, &snapshot, lid_closed);
        let reason = display_mismatch_reason(profile, &matched, &snapshot);
        let legacy = profile
            .outputs
            .iter()
            .any(|output| output.identity.is_none());
        if legacy {
            notes.push(format!(
                "profile {}: saved without screen identities; it follows connectors. Save it again to upgrade.",
                profile.name
            ));
        }
        for issue in &issues {
            problems.push(format!("profile {}: {issue}", profile.name));
        }
        profiles.push(serde_json::json!({
            "name": profile.name,
            "fit": match matched.fit { display_auto::Fit::Exact => "exact", display_auto::Fit::Partial{..} => "partial", display_auto::Fit::Missing{..} => "missing" },
            "reason": reason,
            "auto": profile.auto,
            "identities": !legacy,
            "issues": issues,
        }));
    }
    if let Some(name) = &file.startup_profile {
        match file.profile.iter().find(|profile| &profile.name == name) {
            None => problems.push(format!("startup profile {name} does not exist")),
            Some(profile) => {
                if let Err(error) = display_resolve_profile(profile, &snapshot) {
                    notes.push(format!("startup profile: {error}"));
                }
            }
        }
    }
    let lit = snapshot
        .screens
        .iter()
        .filter(|screen| screen.current.is_some())
        .count();
    if lit == 0 {
        problems.push("no connected screen is lit".into());
    }
    for state_output in &snapshot.outputs {
        if !state_output.connected && state_output.current.is_some() {
            problems.push(format!(
                "{} is disconnected but still holds a screen area; apply a profile or run niructl display auto",
                state_output.name
            ));
        }
    }
    let current = display_current_profile(&file, &snapshot);
    if json {
        let value = serde_json::json!({
            "schema": 1,
            "automatic": file.automatic,
            "lid": snapshot.lid.map(|closed| if closed { "closed" } else { "open" }),
            "screens": snapshot.screens.iter().map(|screen| serde_json::json!({
                "connector": screen.connector,
                "key": screen.key(),
                "identity": screen.identity,
                "internal": screen.internal(),
                "lit": screen.current.is_some(),
            })).collect::<Vec<_>>(),
            "current_profile": current,
            "profiles": profiles,
            "problems": problems,
            "notes": notes,
            "last": state.last.as_ref().map(|event| serde_json::json!({
                "time": event.time, "action": event.action, "profile": event.profile, "detail": event.detail, "ok": event.ok,
            })),
            "retry_in": state.retry.seconds_left(now_secs()),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    println!(
        "automatic switching: {}",
        if file.automatic {
            "on"
        } else {
            "off (niructl display automatic on)"
        }
    );
    if let Some(closed) = snapshot.lid {
        println!("lid: {}", if closed { "closed" } else { "open" });
    }
    println!("screens:");
    for screen in &snapshot.screens {
        println!(
            "  {:<14} {:<4} {}",
            screen.connector,
            if screen.current.is_some() {
                "on"
            } else {
                "off"
            },
            screen
                .identity
                .as_ref()
                .map(|identity| identity.describe())
                .unwrap_or_else(|| "no EDID".into())
        );
    }
    println!(
        "current layout: {}",
        current
            .as_deref()
            .map(|name| format!("profile {name}"))
            .unwrap_or_else(|| "not a saved profile".into())
    );
    println!("profiles:");
    for profile in &profiles {
        let name = profile["name"].as_str().unwrap_or("");
        let fit = profile["fit"].as_str().unwrap_or("");
        let reason = profile["reason"]
            .as_str()
            .map(|r| format!(" — {r}"))
            .unwrap_or_default();
        let auto = if profile["auto"].as_bool() == Some(false) {
            " [not automatic]"
        } else {
            ""
        };
        println!("  {name}: {fit}{reason}{auto}");
    }
    if let Some(event) = &state.last {
        let when = Local
            .timestamp_opt(event.time as i64, 0)
            .single()
            .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        println!(
            "last automatic action: {when} {}{} — {}",
            event.action,
            event
                .profile
                .as_deref()
                .map(|p| format!(" {p}"))
                .unwrap_or_default(),
            event.detail
        );
    }
    if let Some(seconds) = state.retry.seconds_left(now_secs()) {
        println!("retrying the saved settings in {seconds}s");
    }
    for note in &notes {
        println!("note: {note}");
    }
    for problem in &problems {
        println!("problem: {problem}");
    }
    if problems.is_empty() {
        println!("no problems found");
    }
    Ok(())
}

fn display_auto_command(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("status");
    match action {
        "on" | "off" => {
            {
                let _guard = display_transaction_lock()?;
                let mut file = display_read_file()?;
                file.automatic = action == "on";
                display_write_file(&file)?;
            }
            println!(
                "automatic display switching {}",
                if action == "on" { "on" } else { "off" }
            );
            if action == "on" {
                // Act on the screens connected now, as a hotplug would.
                if !display_auto_reconcile(DisplayTrigger::Start, true)? {
                    println!("a display preview is pending; the automation acts after it ends");
                }
            }
        }
        "status" => {
            let file = display_read_file()?;
            let snapshot = DisplaySnapshot::read()?;
            let json = args.iter().any(|arg| arg == "--json");
            let value = display_auto_json(&file, &snapshot);
            if json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!("automatic: {}", if file.automatic { "on" } else { "off" });
                if let Some(lid) = value["lid"].as_str() {
                    println!("lid: {lid}");
                }
                if let Some(detail) = value["last"]["detail"].as_str() {
                    println!(
                        "last: {} — {detail}",
                        value["last"]["action"].as_str().unwrap_or("")
                    );
                }
            }
        }
        "run" | "now" => {
            if !display_auto_reconcile(DisplayTrigger::Screens, false)? {
                println!("a display preview is pending; nothing changed");
            }
        }
        _ => return Err("display auto needs on, off, status [--json] or run".into()),
    }
    Ok(())
}

fn display(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("list");
    // `watch` owns the lock only around each transition. Taking it for the
    // whole watch loop would make the watcher block against itself when it
    // reconciles the first output change.
    let _guard = if matches!(
        action,
        "list" | "status" | "expire" | "watch" | "automatic" | "doctor"
    ) {
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
            let layout: Vec<DisplayOutput> = display_current_layout(&outputs)
                .into_iter()
                .filter(|output| {
                    outputs
                        .iter()
                        .any(|state| state.name == output.name && state.connected)
                })
                .collect();
            if layout.is_empty() {
                return Err("no active display outputs to save".into());
            }
            let previous = file
                .profile
                .iter()
                .find(|profile| profile.name == *name)
                .cloned();
            let profile = display_profile_for_saving(
                DisplayProfile {
                    name: name.clone(),
                    outputs: layout,
                    ..Default::default()
                },
                previous.as_ref(),
            );
            file.profile.retain(|profile| profile.name != *name);
            file.profile.push(profile);
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
            display_delete_profile(&mut file, name)?;
            display_write_file(&file)?;
            println!("deleted display profile {name}");
        }
        "rename" => {
            let old = args.get(1).ok_or("display rename needs an old name")?;
            let new = args.get(2).ok_or("display rename needs a new name")?;
            let mut file = display_read_file()?;
            display_rename_profile(&mut file, old, new)?;
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
            let snapshot = DisplaySnapshot::read()?;
            let profile = display_resolve_profile(&profile, &snapshot)?;
            let outputs = snapshot.outputs;
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
            let restored = display_restore(&state.previous);
            // The transaction ends either way; a stale one would block the
            // automation and every later preview.
            let _ = std::fs::remove_file(display_preview_path());
            restored?;
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
            let restored = display_restore(&state.previous);
            let _ = std::fs::remove_file(display_preview_path());
            restored?;
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
        "watch" => display_watch(&args[1..])?,
        "automatic" => display_auto_command(&args[1..])?,
        "doctor" => display_doctor(args.iter().any(|arg| arg == "--json"))?,
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
                    identity: None,
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
                ..Default::default()
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
    println!("theme_style={}", wallpaper.theme_style);
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
        "theme_style": w.theme_style,
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
/// `niructl keys`: the effective bindings as `keys<TAB>action` lines (the
/// format NIRULAUNCHER 0.4 reads), `--json` for the cheat sheet, `--markdown`
/// for the documentation, `run KEYS` to run what a key is bound to and
/// `--update-doc FILE` to rewrite the generated part of KEYBINDINGS.md.
fn keys_command(args: &[String]) -> Result<()> {
    let defaults_only = args.iter().any(|a| a == "--defaults");
    let defaults = bindings::Action::defaults();
    let effective = if defaults_only {
        defaults.clone()
    } else {
        Config::load()?.effective_keys()
    };
    let dir = (!defaults_only).then(keysheet::keys_dir);
    match args.first().map(String::as_str) {
        None | Some("--defaults") => {
            for binding in effective {
                if binding.action == "unbind" {
                    continue;
                }
                println!("{}\t{}", binding.keys, binding.action);
            }
            Ok(())
        }
        Some("--json") => {
            let sheet = keysheet::build(&effective, &defaults, dir.as_deref());
            println!("{}", serde_json::to_string(&sheet)?);
            Ok(())
        }
        Some("--markdown") => {
            let sheet = keysheet::build(&effective, &defaults, dir.as_deref());
            print!("{}", keysheet::markdown(&sheet.entries));
            Ok(())
        }
        Some("--update-doc") => {
            let path = args
                .get(1)
                .filter(|p| !p.starts_with("--"))
                .ok_or("keys --update-doc needs the path of KEYBINDINGS.md")?;
            let text = fs::read_to_string(path)?;
            let start = text
                .find(keysheet::BEGIN)
                .ok_or("the file has no BEGIN GENERATED marker")?
                + keysheet::BEGIN.len();
            let end = text
                .find(keysheet::END)
                .ok_or("the file has no END GENERATED marker")?;
            let sheet = keysheet::build(&defaults, &defaults, None);
            let updated = format!(
                "{}\n\n{}{}",
                &text[..start],
                keysheet::markdown(&sheet.entries),
                &text[end..]
            );
            fs::write(path, updated)?;
            Ok(())
        }
        Some("run") => {
            let spec = args.get(1).ok_or("keys run needs a key, like super+t")?;
            let (mods, key) = bindings::parse_keys(spec)?;
            let sym = bindings::keysym_named(&key).ok_or_else(|| format!("unknown key {key}"))?;
            let cfg = Config::load()?;
            let bound = cfg.effective_keys().into_iter().any(|b| {
                b.action != "unbind"
                    && bindings::parse_keys(&b.keys)
                        .is_ok_and(|(m, k)| m == mods && bindings::keysym_named(&k) == Some(sym))
            });
            if !bound {
                return Err(format!("{spec} is not bound in NIRUWM").into());
            }
            let (c, root) = connect_for_change()?;
            let atom = c.intern_atom(false, CMD_ATOM.as_bytes())?.reply()?.atom;
            send(&c, root, atom, [19, mods, sym, 0, 0])?;
            c.flush()?;
            Ok(())
        }
        Some(other) => Err(format!(
            "keys takes --json, --markdown, --defaults, run KEYS or --update-doc FILE, not {other}"
        )
        .into()),
    }
}

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
        "style" => {
            // `art`, `signature` or `toggle`: the picture a theme shows.
            // A config without the key (every config before 0.24) is `art`;
            // indexing a missing key would panic, so read it with get().
            let current = document
                .get("wallpaper")
                .and_then(|w| w.get("theme_style"))
                .and_then(|v| v.as_str())
                .unwrap_or("art")
                .to_string();
            let value = match args.get(1).map(String::as_str).ok_or(USAGE)? {
                "toggle" if current == "signature" => "art",
                "toggle" => "signature",
                v @ ("art" | "signature") => v,
                _ => return Err("wallpaper style must be art, signature or toggle".into()),
            };
            document["wallpaper"]["theme_style"] = toml_edit::value(value);
            println!("theme_style={value}");
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

fn autostart_suppressed(restore_session: bool, saved_session: bool) -> bool {
    // A saved session is only a reason to skip the deterministic autostart
    // sequence when restoration is actually enabled. With restore_session=false
    // the saved layout is not replayed, so the user's declared startup app list
    // must still run.
    restore_session && saved_session
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
    if autostart_suppressed(cfg.restore_session, !session_state::load().is_empty()) {
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

/// Studio mode's effects through niructl's own DND and power helpers.
struct StudioEffects;

impl studio::Effects for StudioEffects {
    fn dnd_is_on(&mut self) -> Option<bool> {
        dnd_is_on()
    }
    fn dnd_apply(&mut self, on: bool) -> Result<()> {
        dnd_apply(on)
    }
    fn power(&mut self) -> std::result::Result<(String, Vec<String>), String> {
        if program("powerprofilesctl").is_none() {
            return Err("powerprofilesctl (power-profiles-daemon) is not installed".into());
        }
        power_profile_state()
            .map(|state| (state.active, state.profiles))
            .map_err(|e| e.to_string())
    }
    fn set_power(&mut self, profile: &str) -> Result<()> {
        powerprofilesctl(&["set", profile])?;
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(125));
            if power_profile_state().is_ok_and(|s| s.active == profile) {
                return Ok(());
            }
        }
        Err(format!("power profile {profile} did not become active").into())
    }
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TaskEntry {
    id: String,
    label: String,
    state: String,
    updated: u64,
    detail: String,
}

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("niruwm")
}

fn tasks_path() -> PathBuf {
    state_dir().join("tasks.json")
}

fn read_tasks() -> Vec<TaskEntry> {
    fs::read_to_string(tasks_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_tasks(tasks: &[TaskEntry]) -> Result<()> {
    let path = tasks_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("new");
    fs::write(&temp, serde_json::to_vec_pretty(tasks)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn update_task(id: &str, state: &str, detail: &str) -> Result<()> {
    let mut tasks = read_tasks();
    if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
        task.state = state.into();
        task.updated = now_secs();
        if !detail.is_empty() {
            task.detail = detail.into();
        }
    }
    tasks.sort_by_key(|task| std::cmp::Reverse(task.updated));
    tasks.truncate(20);
    write_tasks(&tasks)
}

fn add_task(label: &str) -> Result<String> {
    let id = format!("{}-{}", now_secs(), std::process::id());
    let mut tasks = read_tasks();
    tasks.push(TaskEntry {
        id: id.clone(),
        label: label.chars().take(72).collect(),
        state: "running".into(),
        updated: now_secs(),
        detail: String::new(),
    });
    tasks.sort_by_key(|task| std::cmp::Reverse(task.updated));
    tasks.truncate(20);
    write_tasks(&tasks)?;
    Ok(id)
}

fn task(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str).unwrap_or("list") {
        "list" => {
            let tasks = read_tasks();
            if args.iter().any(|arg| arg == "--json") {
                println!("{}", serde_json::to_string_pretty(&tasks)?);
                return Ok(());
            }
            if tasks.is_empty() {
                println!("No NIRU tasks recorded.");
                return Ok(());
            }
            for task in tasks {
                let age = now_secs().saturating_sub(task.updated);
                println!(
                    "{}\t{}\t{}s ago\t{}",
                    task.state, task.label, age, task.detail
                );
            }
        }
        "clear" => {
            let mut tasks = read_tasks();
            tasks.retain(|task| task.state == "running" || task.state == "waiting");
            write_tasks(&tasks)?;
            println!("Cleared finished tasks.");
        }
        other => return Err(format!("task needs list or clear, not {other}").into()),
    }
    Ok(())
}

const ACTIONS_VERSION: u32 = 1;
const MAX_ACTIONS: usize = 200;
const MAX_ACTION_NAME_CHARS: usize = 80;
const MAX_ACTION_ARGS: usize = 32;
const MAX_ACTION_ARG_BYTES: usize = 8 * 1024;
const MAX_ACTION_TOTAL_BYTES: usize = 64 * 1024;
const MAX_ACTIONS_FILE_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SavedAction {
    name: String,
    command: Vec<String>,
    #[serde(default)]
    favorite: bool,
    #[serde(default)]
    created: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ActionsFile {
    version: u32,
    #[serde(default, rename = "action")]
    actions: Vec<SavedAction>,
}

impl Default for ActionsFile {
    fn default() -> Self {
        Self {
            version: ACTIONS_VERSION,
            actions: Vec::new(),
        }
    }
}

fn actions_path() -> PathBuf {
    config::directory().join("actions.toml")
}

fn validate_action_name(name: &str) -> Result<()> {
    let length = name.chars().count();
    if length == 0 || length > MAX_ACTION_NAME_CHARS || name.trim().is_empty() {
        return Err(format!("action name must be 1..{MAX_ACTION_NAME_CHARS} characters").into());
    }
    if name.chars().any(|c| c == '\0' || c.is_control()) {
        return Err("action name must not contain NUL or control characters".into());
    }
    Ok(())
}

fn validate_action_command(command: &[String]) -> Result<()> {
    if command.is_empty() || command.len() > MAX_ACTION_ARGS {
        return Err(format!("action command must have 1..{MAX_ACTION_ARGS} arguments").into());
    }
    let mut total = 0usize;
    for arg in command {
        if arg.is_empty() {
            return Err("action command arguments must not be empty".into());
        }
        if arg.contains('\0') {
            return Err("action command arguments must not contain NUL".into());
        }
        if arg.len() > MAX_ACTION_ARG_BYTES {
            return Err(format!(
                "action command argument is longer than {MAX_ACTION_ARG_BYTES} bytes"
            )
            .into());
        }
        total = total.saturating_add(arg.len());
    }
    if total > MAX_ACTION_TOTAL_BYTES {
        return Err(
            format!("action command exceeds the {MAX_ACTION_TOTAL_BYTES}-byte limit").into(),
        );
    }
    Ok(())
}

fn validate_saved_action(action: &SavedAction) -> Result<()> {
    validate_action_name(&action.name)?;
    validate_action_command(&action.command)?;
    Ok(())
}

fn validate_actions_file(file: &ActionsFile) -> Result<()> {
    if file.version != ACTIONS_VERSION {
        return Err(format!("unsupported actions file version: {}", file.version).into());
    }
    if file.actions.len() > MAX_ACTIONS {
        return Err(format!("at most {MAX_ACTIONS} actions are allowed").into());
    }
    let mut names = HashSet::new();
    let mut total = 0usize;
    for action in &file.actions {
        validate_saved_action(action)?;
        if !names.insert(action.name.clone()) {
            return Err(format!("duplicate action name: {}", action.name).into());
        }
        total = total.saturating_add(action.name.len());
        total = total.saturating_add(action.command.iter().map(String::len).sum::<usize>());
    }
    if total > MAX_ACTION_TOTAL_BYTES {
        return Err(format!("actions exceed the {MAX_ACTION_TOTAL_BYTES}-byte limit").into());
    }
    Ok(())
}

fn read_actions_from(path: &Path) -> Result<ActionsFile> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ActionsFile::default())
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_ACTIONS_FILE_BYTES {
        return Err(format!("actions file is larger than {MAX_ACTIONS_FILE_BYTES} bytes").into());
    }
    let text = fs::read_to_string(path)?;
    if text.len() as u64 > MAX_ACTIONS_FILE_BYTES {
        return Err(format!("actions file is larger than {MAX_ACTIONS_FILE_BYTES} bytes").into());
    }
    let file: ActionsFile = toml::from_str(&text)?;
    validate_actions_file(&file)?;
    Ok(file)
}

fn read_actions() -> Result<ActionsFile> {
    read_actions_from(&actions_path())
}

fn write_actions_to(path: &Path, file: &ActionsFile) -> Result<()> {
    validate_actions_file(file)?;
    let text = toml::to_string_pretty(file)?;
    if text.len() as u64 > MAX_ACTIONS_FILE_BYTES {
        return Err(format!("actions file would exceed {MAX_ACTIONS_FILE_BYTES} bytes").into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("actions.toml");
    let temp = path.with_file_name(format!(".{name}.{}.new", std::process::id()));
    if let Err(error) = fs::write(&temp, text) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
    Ok(())
}

fn write_actions(file: &ActionsFile) -> Result<()> {
    write_actions_to(&actions_path(), file)
}

fn action_save(
    file: &mut ActionsFile,
    name: &str,
    command: Vec<String>,
    created: u64,
) -> Result<()> {
    validate_action_name(name)?;
    validate_action_command(&command)?;
    if file.actions.len() >= MAX_ACTIONS {
        return Err(format!("at most {MAX_ACTIONS} actions are allowed").into());
    }
    if file.actions.iter().any(|action| action.name == name) {
        return Err(format!("action already exists: {name}").into());
    }
    file.actions.push(SavedAction {
        name: name.to_string(),
        command,
        favorite: false,
        created,
    });
    validate_actions_file(file)?;
    Ok(())
}

fn action_rename(file: &mut ActionsFile, old: &str, new: &str) -> Result<()> {
    validate_action_name(new)?;
    if old != new && file.actions.iter().any(|action| action.name == new) {
        return Err(format!("action already exists: {new}").into());
    }
    let action = file
        .actions
        .iter_mut()
        .find(|action| action.name == old)
        .ok_or_else(|| format!("unknown action: {old}"))?;
    action.name = new.to_string();
    validate_actions_file(file)?;
    Ok(())
}

fn action_delete(file: &mut ActionsFile, name: &str) -> Result<()> {
    let before = file.actions.len();
    file.actions.retain(|action| action.name != name);
    if file.actions.len() == before {
        return Err(format!("unknown action: {name}").into());
    }
    Ok(())
}

fn action_set_favorite(file: &mut ActionsFile, name: &str, favorite: bool) -> Result<()> {
    let action = file
        .actions
        .iter_mut()
        .find(|action| action.name == name)
        .ok_or_else(|| format!("unknown action: {name}"))?;
    action.favorite = favorite;
    Ok(())
}

fn actions_json_value(file: &ActionsFile) -> serde_json::Value {
    serde_json::Value::Array(
        file.actions
            .iter()
            .map(|action| {
                serde_json::json!({
                    "name": action.name,
                    "command": action.command,
                    "favorite": action.favorite,
                })
            })
            .collect(),
    )
}

fn actions(args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("list");
    match action {
        "list" => {
            let file = read_actions()?;
            if args.iter().any(|arg| arg == "--json") {
                println!("{}", serde_json::to_string(&actions_json_value(&file))?);
            } else {
                for action in file.actions {
                    println!("{}", action.name);
                }
            }
        }
        "save" => {
            let name = args.get(1).ok_or("action save needs a name")?;
            if args.get(2).map(String::as_str) != Some("--") {
                return Err("action save needs -- before the command".into());
            }
            let command = args[3..].to_vec();
            if command.is_empty() {
                return Err("action save needs a command after --".into());
            }
            let mut file = read_actions()?;
            action_save(&mut file, name, command, now_secs())?;
            write_actions(&file)?;
            println!("saved action {name}");
        }
        "run" => {
            use std::os::unix::process::CommandExt;
            let name = args.get(1).ok_or("action run needs a name")?;
            let file = read_actions()?;
            let action = file
                .actions
                .iter()
                .find(|action| action.name == *name)
                .ok_or_else(|| format!("unknown action: {name}"))?;
            validate_saved_action(action)?;
            std::process::Command::new(&action.command[0])
                .args(&action.command[1..])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .map_err(|error| format!("cannot run action {name}: {error}"))?;
            println!("started action {name}");
        }
        "delete" => {
            let name = args.get(1).ok_or("action delete needs a name")?;
            let mut file = read_actions()?;
            action_delete(&mut file, name)?;
            write_actions(&file)?;
            println!("deleted action {name}");
        }
        "rename" => {
            let old = args.get(1).ok_or("action rename needs an old name")?;
            let new = args.get(2).ok_or("action rename needs a new name")?;
            let mut file = read_actions()?;
            action_rename(&mut file, old, new)?;
            write_actions(&file)?;
            println!("renamed action {old} to {new}");
        }
        "pin" | "unpin" => {
            let name = args
                .get(1)
                .ok_or_else(|| format!("action {action} needs a name"))?;
            let favorite = action == "pin";
            let mut file = read_actions()?;
            action_set_favorite(&mut file, name, favorite)?;
            write_actions(&file)?;
            println!(
                "{} action {name}",
                if favorite { "pinned" } else { "unpinned" }
            );
        }
        other => {
            return Err(format!(
                "action needs list, save, run, delete, rename, pin or unpin, not {other}"
            )
            .into())
        }
    }
    Ok(())
}

fn human_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < units.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, units[unit])
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

fn scan_size(path: &Path) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    let mut stack = vec![path.to_path_buf()];
    let mut visited = 0usize;
    while let Some(next) = stack.pop() {
        if visited > 250_000 {
            break;
        }
        let Ok(meta) = fs::symlink_metadata(&next) else {
            continue;
        };
        visited += 1;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(&next) {
                for entry in entries.flatten() {
                    stack.push(entry.path());
                }
            }
        } else if meta.is_file() {
            bytes = bytes.saturating_add(meta.len());
            files = files.saturating_add(1);
        }
    }
    (bytes, files)
}

fn old_downloads(path: &Path, days: u64) -> (u64, u64) {
    let now = now_secs();
    let mut bytes = 0u64;
    let mut count = 0u64;
    let Ok(entries) = fs::read_dir(path) else {
        return (0, 0);
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(now);
        if now.saturating_sub(modified) >= days * 86_400 {
            bytes = bytes.saturating_add(meta.len());
            count += 1;
        }
    }
    (bytes, count)
}

fn storage(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str).unwrap_or("review") {
        "review" => storage_review(args.iter().any(|arg| arg == "--json")),
        "clean" => {
            let what = args.get(1).map(String::as_str).unwrap_or("");
            if what != "cache" || !args.iter().any(|arg| arg == "--yes") {
                return Err("storage clean needs: cache --yes".into());
            }
            let base = std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache")
                })
                .join("nirubar");
            let (before, _) = scan_size(&base);
            if base.is_dir() {
                for entry in fs::read_dir(&base)?.flatten() {
                    let path = entry.path();
                    if entry
                        .file_type()
                        .map(|kind| kind.is_symlink())
                        .unwrap_or(true)
                    {
                        let _ = fs::remove_file(&path);
                    } else if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                        fs::remove_dir_all(&path)?;
                    } else {
                        fs::remove_file(&path)?;
                    }
                }
            }
            let (after, _) = scan_size(&base);
            println!(
                "Removed NIRUBAR cache: {} (was {}).",
                human_bytes(after),
                human_bytes(before)
            );
            Ok(())
        }
        other => Err(format!("storage needs review or clean, not {other}").into()),
    }
}

fn storage_review(json: bool) -> Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"));
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let (cache_bytes, cache_files) = scan_size(&cache);
    let (bar_bytes, _) = scan_size(&cache.join("nirubar"));
    let (trash_bytes, trash_files) = scan_size(&data.join("Trash"));
    let (download_bytes, download_count) = old_downloads(&home.join("Downloads"), 90);
    let generation_root = data.join("nirux/generations");
    let mut generation_count = 0usize;
    let mut old_generations = 0usize;
    if let Ok(entries) = fs::read_dir(&generation_root) {
        let current = fs::read_link(data.join("nirux/current")).ok();
        let previous = fs::read_link(data.join("nirux/previous")).ok();
        for entry in entries.flatten() {
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            generation_count += 1;
            let path = entry.path();
            if current.as_ref() != Some(&path) && previous.as_ref() != Some(&path) {
                old_generations += 1;
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "cache_bytes": cache_bytes,
                "cache_files": cache_files,
                "nirubar_cache_bytes": bar_bytes,
                "trash_bytes": trash_bytes,
                "trash_files": trash_files,
                "old_download_bytes": download_bytes,
                "old_download_count": download_count,
                "generation_count": generation_count,
                "old_generations": old_generations,
            })
        );
        return Ok(());
    }
    println!("NIRU storage review (nothing has been removed)");
    println!(
        "  Cache:                    {} in {} files",
        human_bytes(cache_bytes),
        cache_files
    );
    println!("    NIRUBAR cache:          {}", human_bytes(bar_bytes));
    println!(
        "  Trash:                    {} in {} files",
        human_bytes(trash_bytes),
        trash_files
    );
    println!(
        "  Downloads older than 90d: {} in {} files",
        human_bytes(download_bytes),
        download_count
    );
    println!(
        "  NIRUX generations:        {} total, {} older than current/previous",
        generation_count, old_generations
    );
    println!("Review Downloads and generations before deleting anything personal.");
    Ok(())
}

fn agent(args: &[String]) -> Result<()> {
    if !which("opencode") {
        return Err(
            "OpenCode is not installed; enable it with NIRUX or install it separately".into(),
        );
    }
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let task = args
        .iter()
        .filter(|arg| arg.as_str() != "--dry-run")
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let interactive = task.trim().is_empty();
    let dir = state_dir().join("agent");
    fs::create_dir_all(&dir)?;
    let context_path = dir.join("context.md");
    let version = std::process::Command::new("nirux")
        .arg("--version")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|| "NIRUX version unavailable".into());
    let context = format!(
        "# NIRU desktop assistance context\n\n\
         Component: {}\n\
         Desktop: NIRUWM / X11\n\
         Task scope: user described a desktop task.\n\n\
         Start by inspecting current state with `nirux status`, `nirux doctor`, `niructl explain`, and the relevant `niructl` command.\n\
         Do not print credentials or private file contents. Do not make destructive changes without explicit confirmation. Prefer reversible changes and report what changed.\n",
        version
    );
    fs::write(&context_path, context)?;
    let prompt = if interactive {
        format!(
            "You are helping on the NIRUX minimal X11 desktop. Read {} and ask me what I want to do. Use the existing NIRUX tools and ask before destructive changes. Do not expose credentials, private messages, or personal files.",
            context_path.display()
        )
    } else {
        format!(
            "You are helping on the NIRUX minimal X11 desktop. Read {}. Task: {}. Use the existing NIRUX tools and ask before destructive changes. Do not expose credentials, private messages, or personal files.",
            context_path.display(),
            task.trim()
        )
    };
    if dry_run {
        println!("context: {}", context_path.display());
        println!("prompt: {prompt}");
        println!("command: opencode --prompt <prompt>");
        return Ok(());
    }
    let id = add_task("OpenCode desktop task")?;
    if interactive {
        println!("Starting OpenCode in interactive handoff mode.");
    } else {
        println!("Starting OpenCode for: {}", task.trim());
    }
    println!("Context: {}", context_path.display());
    let status = std::process::Command::new("opencode")
        .arg("--prompt")
        .arg(&prompt)
        .status();
    match status {
        Ok(status) if status.success() => {
            update_task(&id, "completed", "OpenCode exited successfully")?
        }
        Ok(status) => update_task(&id, "failed", &format!("OpenCode exited with {status}"))?,
        Err(error) => {
            update_task(&id, "failed", &format!("cannot start OpenCode: {error}"))?;
            return Err(format!("cannot start OpenCode: {error}").into());
        }
    }
    Ok(())
}

fn windows_rescue(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    args: &[String],
) -> Result<()> {
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let moved = windows_rescue_run(c, root, dry_run, false)?;
    if moved == 0 {
        println!("All managed windows are on a connected display.");
    }
    Ok(())
}

fn windows_rescue_run(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    dry_run: bool,
    quiet: bool,
) -> Result<usize> {
    let atom =
        |name: &str| -> Result<Atom> { Ok(c.intern_atom(false, name.as_bytes())?.reply()?.atom) };
    let client_list = atom("_NET_CLIENT_LIST")?;
    let Some(property) = c
        .get_property(false, root, client_list, AtomEnum::WINDOW, 0, 4096)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return Err("cannot read the window list".into());
    };
    let windows = property
        .value32()
        .map(|values| values.collect::<Vec<Window>>())
        .unwrap_or_default();
    let pointer = c
        .query_pointer(root)?
        .reply()
        .map(|reply| (reply.root_x, reply.root_y))
        .unwrap_or((0, 0));
    let target = monitor_rect(c, root, pointer.0, pointer.1)?;
    let mut moved = 0usize;
    for window in windows {
        let Ok(attributes) = c.get_window_attributes(window) else {
            continue;
        };
        let Ok(attributes) = attributes.reply() else {
            continue;
        };
        if attributes.override_redirect || attributes.map_state == MapState::UNMAPPED {
            continue;
        }
        let Ok(geometry) = c.get_geometry(window) else {
            continue;
        };
        let Ok(geometry) = geometry.reply() else {
            continue;
        };
        let frame = c
            .get_property(
                false,
                window,
                atom("_NET_FRAME_EXTENTS")?,
                AtomEnum::CARDINAL,
                0,
                4,
            )
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .and_then(|reply| reply.value32().map(|values| values.collect::<Vec<u32>>()))
            .unwrap_or_else(|| vec![0, 0, 0, 0]);
        let left = i32::try_from(*frame.first().unwrap_or(&0)).unwrap_or(0);
        let right = i32::try_from(*frame.get(1).unwrap_or(&0)).unwrap_or(0);
        let top = i32::try_from(*frame.get(2).unwrap_or(&0)).unwrap_or(0);
        let bottom = i32::try_from(*frame.get(3).unwrap_or(&0)).unwrap_or(0);
        let x = i32::from(geometry.x) - left;
        let y = i32::from(geometry.y) - top;
        let width = i32::from(geometry.width) + left + right;
        let height = i32::from(geometry.height) + top + bottom;
        if monitors_intersect(c, root, x, y, width, height) {
            continue;
        }
        let name = c
            .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 128)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| String::from_utf8_lossy(&reply.value).to_string())
            .unwrap_or_else(|| format!("0x{window:x}"));
        if !quiet {
            println!(
                "{}: {} at {},{}x{},{}",
                if dry_run { "would move" } else { "moving" },
                name.trim(),
                x,
                y,
                width,
                height
            );
        }
        if !dry_run {
            let x = target.x.saturating_add(40);
            let y = target.y.saturating_add(40);
            let data = [
                u32::try_from(10).unwrap_or(10),
                x.max(0) as u32,
                y.max(0) as u32,
                geometry.width as u32,
                geometry.height as u32,
            ];
            let event = ClientMessageEvent::new(32, window, atom("_NET_MOVERESIZE_WINDOW")?, data);
            c.send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            )?;
        }
        moved += 1;
    }
    c.flush()?;
    Ok(moved)
}

fn monitor_rect(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    x: i16,
    y: i16,
) -> Result<Rectangle> {
    let resources = c.randr_get_screen_resources_current(root)?.reply()?;
    let mut fallback = Rectangle {
        x: 0,
        y: 0,
        width: c
            .setup()
            .roots
            .last()
            .map(|screen| screen.width_in_pixels)
            .unwrap_or(1920),
        height: c
            .setup()
            .roots
            .last()
            .map(|screen| screen.height_in_pixels)
            .unwrap_or(1080),
    };
    for output in resources.outputs {
        let Ok(info) = c.randr_get_output_info(output, resources.config_timestamp) else {
            continue;
        };
        let Ok(info) = info.reply() else { continue };
        if info.connection != x11rb::protocol::randr::Connection::CONNECTED || info.crtc == 0 {
            continue;
        }
        let Ok(crtc) = c.randr_get_crtc_info(info.crtc, resources.config_timestamp) else {
            continue;
        };
        let Ok(crtc) = crtc.reply() else { continue };
        if crtc.width == 0 || crtc.height == 0 {
            continue;
        }
        let rect = Rectangle {
            x: crtc.x,
            y: crtc.y,
            width: crtc.width,
            height: crtc.height,
        };
        fallback = rect;
        if i32::from(x) >= i32::from(rect.x)
            && i32::from(x) < i32::from(rect.x) + i32::from(rect.width)
            && i32::from(y) >= i32::from(rect.y)
            && i32::from(y) < i32::from(rect.y) + i32::from(rect.height)
        {
            return Ok(rect);
        }
    }
    Ok(fallback)
}

fn monitors_intersect(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> bool {
    let Ok(resources) = c.randr_get_screen_resources_current(root) else {
        return true;
    };
    let Ok(resources) = resources.reply() else {
        return true;
    };
    for output in resources.outputs {
        let Ok(info) = c.randr_get_output_info(output, resources.config_timestamp) else {
            continue;
        };
        let Ok(info) = info.reply() else { continue };
        if info.connection != x11rb::protocol::randr::Connection::CONNECTED || info.crtc == 0 {
            continue;
        }
        let Ok(crtc) = c.randr_get_crtc_info(info.crtc, resources.config_timestamp) else {
            continue;
        };
        let Ok(crtc) = crtc.reply() else { continue };
        let right = i32::from(crtc.x) + i32::from(crtc.width);
        let bottom = i32::from(crtc.y) + i32::from(crtc.height);
        if x < right
            && i32::from(crtc.x) < x.saturating_add(width)
            && y < bottom
            && i32::from(crtc.y) < y.saturating_add(height)
        {
            return true;
        }
    }
    false
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct InputConfig {
    #[serde(default)]
    keyboard: KeyboardInput,
    #[serde(default)]
    pointer: PointerInput,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct KeyboardInput {
    #[serde(default)]
    layout: String,
    #[serde(default)]
    variant: String,
    #[serde(default)]
    repeat_delay: Option<u16>,
    #[serde(default)]
    repeat_rate: Option<u16>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct PointerInput {
    #[serde(default)]
    natural_scroll: Option<bool>,
    #[serde(default)]
    tap: Option<bool>,
    #[serde(default)]
    speed: Option<f32>,
}

fn input_path() -> PathBuf {
    config::directory().join("input.toml")
}

fn read_input_config() -> InputConfig {
    fs::read_to_string(input_path())
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_input_config(config: &InputConfig) -> Result<()> {
    let path = input_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("new");
    fs::write(&temp, toml::to_string_pretty(config)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn input_apply_keyboard(config: &KeyboardInput) -> Result<()> {
    if !config.layout.trim().is_empty() {
        let mut args = vec!["-layout".to_string(), config.layout.trim().to_string()];
        if !config.variant.trim().is_empty() {
            args.push("-variant".to_string());
            args.push(config.variant.trim().to_string());
        }
        let out = std::process::Command::new("setxkbmap")
            .args(&args)
            .output()
            .map_err(|e| format!("cannot run setxkbmap: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr)
                .trim()
                .to_string()
                .into());
        }
    }
    if let (Some(delay), Some(rate)) = (config.repeat_delay, config.repeat_rate) {
        if delay == 0 || rate == 0 {
            return Ok(());
        }
        let out = std::process::Command::new("xset")
            .args(["r", "rate", &delay.to_string(), &rate.to_string()])
            .output()
            .map_err(|e| format!("cannot run xset: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr)
                .trim()
                .to_string()
                .into());
        }
    }
    Ok(())
}

fn xinput_devices() -> Result<Vec<(String, String)>> {
    let out = std::process::Command::new("xinput")
        .args(["list", "--short"])
        .output()
        .map_err(|e| format!("xinput is not available: {e}"))?;
    if !out.status.success() {
        return Err("xinput could not list pointer devices".into());
    }
    let mut devices = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if !line.contains("[slave") || !line.contains("pointer") || !line.contains("id=") {
            continue;
        }
        let Some(id) = line
            .split("id=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
        else {
            continue;
        };
        let name = line
            .split("id=")
            .next()
            .unwrap_or("")
            .trim()
            .trim_start_matches('↳')
            .trim()
            .to_string();
        if name.contains("Virtual core") || name.contains("XTEST") {
            continue;
        }
        devices.push((id.to_string(), name));
    }
    Ok(devices)
}

fn xinput_properties(id: &str) -> Vec<String> {
    let Ok(out) = std::process::Command::new("xinput")
        .args(["list-props", id])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split('(').next())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn xinput_set(id: &str, property: &str, value: &str) -> Result<()> {
    let out = std::process::Command::new("xinput")
        .args(["set-prop", id, property, value])
        .output()
        .map_err(|e| format!("cannot run xinput: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr)
            .trim()
            .to_string()
            .into());
    }
    Ok(())
}

fn input_apply_pointer(config: &PointerInput) -> Result<usize> {
    let devices = xinput_devices()?;
    let mut changes = 0;
    for (id, _) in devices {
        let properties = xinput_properties(&id);
        let find = |names: &[&str]| {
            names
                .iter()
                .find(|name| properties.iter().any(|candidate| candidate == *name))
                .map(|name| (*name).to_string())
        };
        if let Some(value) = config.natural_scroll {
            if let Some(property) = find(&[
                "libinput Natural Scrolling Enabled",
                "Natural Scrolling Enabled",
            ]) {
                xinput_set(&id, &property, if value { "1" } else { "0" })?;
                changes += 1;
            }
        }
        if let Some(value) = config.tap {
            if let Some(property) = find(&["libinput Tapping Enabled", "Tapping Enabled"]) {
                xinput_set(&id, &property, if value { "1" } else { "0" })?;
                changes += 1;
            }
        }
        if let Some(value) = config.speed {
            if let Some(property) = find(&["libinput Accel Speed", "Accel Speed"]) {
                xinput_set(&id, &property, &format!("{value:.2}"))?;
                changes += 1;
            }
        }
    }
    Ok(changes)
}

fn input_apply(config: &InputConfig, quiet: bool) -> Result<()> {
    input_apply_keyboard(&config.keyboard)?;
    let pointer_requested = config.pointer.natural_scroll.is_some()
        || config.pointer.tap.is_some()
        || config.pointer.speed.is_some();
    if pointer_requested {
        match input_apply_pointer(&config.pointer) {
            Ok(changes) => {
                if !quiet {
                    println!("Applied {changes} pointer property change(s).");
                }
            }
            Err(error) => {
                if !quiet {
                    println!("Pointer settings not applied: {error}");
                }
            }
        }
    }
    Ok(())
}

fn input_capabilities() -> (bool, bool, bool) {
    let Ok(devices) = xinput_devices() else {
        return (false, false, false);
    };
    let mut natural = false;
    let mut tap = false;
    let mut speed = false;
    for (id, _) in devices {
        let properties = xinput_properties(&id);
        natural |= properties.iter().any(|property| {
            matches!(
                property.as_str(),
                "libinput Natural Scrolling Enabled" | "Natural Scrolling Enabled"
            )
        });
        tap |= properties.iter().any(|property| {
            matches!(
                property.as_str(),
                "libinput Tapping Enabled" | "Tapping Enabled"
            )
        });
        speed |= properties
            .iter()
            .any(|property| matches!(property.as_str(), "libinput Accel Speed" | "Accel Speed"));
    }
    (natural, tap, speed)
}

fn input(args: &[String]) -> Result<()> {
    let command = args.first().map(String::as_str).unwrap_or("status");
    let mut config = read_input_config();
    match command {
        "status" => {
            if args.iter().any(|arg| arg == "--json") {
                let (natural_supported, tap_supported, speed_supported) = input_capabilities();
                println!(
                    "{}",
                    serde_json::json!({
                        "keyboard": {
                            "layout": config.keyboard.layout,
                            "variant": config.keyboard.variant,
                            "repeat_delay": config.keyboard.repeat_delay,
                            "repeat_rate": config.keyboard.repeat_rate,
                        },
                        "pointer": {
                            "natural_scroll": config.pointer.natural_scroll,
                            "tap": config.pointer.tap,
                            "speed": config.pointer.speed,
                        },
                        "xinput_available": which("xinput"),
                        "pointer_support": {
                            "natural_scroll": natural_supported,
                            "tap": tap_supported,
                            "speed": speed_supported,
                        },
                    })
                );
                return Ok(());
            }
            println!(
                "keyboard layout={} variant={} repeat={}ms/{}",
                if config.keyboard.layout.is_empty() {
                    "system".to_string()
                } else {
                    config.keyboard.layout.clone()
                },
                if config.keyboard.variant.is_empty() {
                    "-".to_string()
                } else {
                    config.keyboard.variant.clone()
                },
                config
                    .keyboard
                    .repeat_delay
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "system".into()),
                config
                    .keyboard
                    .repeat_rate
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "system".into())
            );
            println!(
                "pointer natural_scroll={} tap={} speed={}",
                option_label(config.pointer.natural_scroll),
                option_label(config.pointer.tap),
                config
                    .pointer
                    .speed
                    .map(|value| format!("{value:.2}"))
                    .unwrap_or_else(|| "unchanged".into())
            );
            println!(
                "xinput={}",
                if which("xinput") { "available" } else { "missing" }
            );
        }
        "apply" => input_apply(&config, args.iter().any(|arg| arg == "--quiet"))?,
        "set-keyboard" => {
            let layout = args.get(1).ok_or("set-keyboard needs a layout")?;
            if !layout
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            {
                return Err("invalid keyboard layout".into());
            }
            config.keyboard.layout = layout.to_string();
            config.keyboard.variant = args.get(2).cloned().unwrap_or_default();
            write_input_config(&config)?;
            input_apply_keyboard(&config.keyboard)?;
            println!("Keyboard layout set to {}.", layout);
        }
        "set-repeat" => {
            let delay = args.get(1).ok_or("set-repeat needs a delay")?.parse::<u16>()?;
            let rate = args.get(2).ok_or("set-repeat needs a rate")?.parse::<u16>()?;
            if !(100..=2000).contains(&delay) || !(10..=200).contains(&rate) {
                return Err("repeat delay must be 100..2000 and rate 10..200".into());
            }
            config.keyboard.repeat_delay = Some(delay);
            config.keyboard.repeat_rate = Some(rate);
            write_input_config(&config)?;
            input_apply_keyboard(&config.keyboard)?;
            println!("Keyboard repeat set to {delay} ms / {rate} per second.");
        }
        "set-pointer" => {
            let which = args.get(1).ok_or("set-pointer needs natural or tap")?;
            let value = args.get(2).ok_or("set-pointer needs on or off")?;
            let value = match value.as_str() {
                "on" => true,
                "off" => false,
                _ => return Err("pointer setting must be on or off".into()),
            };
            match which.as_str() {
                "natural" => config.pointer.natural_scroll = Some(value),
                "tap" => config.pointer.tap = Some(value),
                _ => return Err("set-pointer needs natural or tap".into()),
            }
            write_input_config(&config)?;
            let changed = input_apply_pointer(&config.pointer)?;
            if changed == 0 {
                return Err("no pointer device exposes that property".into());
            }
            println!("Pointer {which} set to {}.", if value { "on" } else { "off" });
        }
        "toggle" => {
            let which = args.get(1).ok_or("toggle needs natural or tap")?;
            match which.as_str() {
                "natural" => {
                    config.pointer.natural_scroll = Some(!config.pointer.natural_scroll.unwrap_or(false))
                }
                "tap" => config.pointer.tap = Some(!config.pointer.tap.unwrap_or(false)),
                _ => return Err("toggle needs natural or tap".into()),
            }
            write_input_config(&config)?;
            let changed = input_apply_pointer(&config.pointer)?;
            if changed == 0 {
                return Err("no pointer device exposes that property".into());
            }
            println!("Toggled pointer {which}.");
        }
        "set-speed" => {
            let value = args
                .get(1)
                .ok_or("set-speed needs a value")?
                .parse::<f32>()?;
            if !(-1.0..=1.0).contains(&value) {
                return Err("pointer speed must be between -1 and 1".into());
            }
            config.pointer.speed = Some(value);
            write_input_config(&config)?;
            let changed = input_apply_pointer(&config.pointer)?;
            if changed == 0 {
                return Err("no pointer device exposes a speed property".into());
            }
            println!("Pointer speed set to {value:.2}.");
        }
        "cycle-speed" => {
            let current = config.pointer.speed.unwrap_or(0.0);
            let mut next = current + 0.2;
            if next > 0.61 {
                next = -0.6;
            }
            config.pointer.speed = Some(next);
            write_input_config(&config)?;
            let changed = input_apply_pointer(&config.pointer)?;
            if changed == 0 {
                return Err("no pointer device exposes a speed property".into());
            }
            println!("Pointer speed set to {next:.2}.");
        }
        "reset" => {
            let _ = fs::remove_file(input_path());
            println!("Input settings removed; the system defaults remain active.");
        }
        other => return Err(format!("input needs status, apply, set-keyboard, set-repeat, set-pointer, toggle, set-speed, cycle-speed or reset, not {other}").into()),
    }
    Ok(())
}

fn option_label(value: Option<bool>) -> String {
    match value {
        Some(true) => "on".into(),
        Some(false) => "off".into(),
        None => "unchanged".into(),
    }
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
    if cmd == "update-shutdown" || cmd == "update-reboot" {
        return update_power_action(cmd, &std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "brightness" {
        return brightness(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "battery" {
        return battery_command(&std::env::args().skip(2).collect::<Vec<_>>());
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
    if cmd == "storage" {
        return storage(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "agent" {
        return agent(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "task" {
        return task(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "action" {
        return actions(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "input" {
        return input(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if matches!(
        cmd,
        "lock" | "screensaver" | "logout" | "suspend" | "reboot" | "shutdown" | "dnd"
    ) {
        return desktop(cmd, &std::env::args().skip(2).collect::<Vec<_>>());
    }
    if cmd == "keys" {
        return keys_command(&std::env::args().skip(2).collect::<Vec<_>>());
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
            Some("columns") => {
                let requested = rest.get(1).map(String::as_str).unwrap_or("");
                let (c, root) = connect_for_change()?;
                let atom = |n: &str| -> Result<Atom> {
                    Ok(c.intern_atom(false, n.as_bytes())?.reply()?.atom)
                };
                let current =
                    property_string(&c, root, atom("_NIRUWM_LAYOUT")?)?.unwrap_or_default();
                if !current.starts_with("scroll") {
                    return Err("layout columns applies only while Scroll is active".into());
                }
                let (value, want) = match requested {
                    "1" => (1, "scroll 1".to_string()),
                    "2" => (2, "scroll 2".to_string()),
                    "toggle" => (
                        0,
                        if current.trim() == "scroll 1" {
                            "scroll 2".to_string()
                        } else {
                            "scroll 1".to_string()
                        },
                    ),
                    other => {
                        return Err(
                            format!("layout columns must be 1, 2 or toggle, got {other}").into(),
                        )
                    }
                };
                send(&c, root, atom(CMD_ATOM)?, [11, value, 0, 0, 0])?;
                let deadline = std::time::Instant::now() + Duration::from_millis(500);
                loop {
                    let shown =
                        property_string(&c, root, atom("_NIRUWM_LAYOUT")?)?.unwrap_or_default();
                    if shown.trim() == want {
                        return Ok(());
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(
                            "the window manager did not apply the Scroll column mode".into()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
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
                    if shown.split_whitespace().next() == Some(name) {
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
            ("scroll", "Super+Ctrl+S"),
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
        println!("Scroll columns: Super+Ctrl+Shift+S or niructl layout columns 1|2|toggle.");
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
            | "battery"
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
            | "update-shutdown"
            | "update-reboot"
            | "dnd"
            | "audio"
            | "bluetooth"
            | "display"
            | "powerprofile"
            | "mode"
            | "idle"
            | "timing"
            | "monitors"
            | "storage"
            | "agent"
            | "task"
            | "action"
            | "input"
    ) {
        return offline(cmd);
    }
    if cmd == "config" {
        return config_command(&args[1..]);
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

    if cmd == "windows" {
        return windows_rescue(&c, root, &std::env::args().skip(2).collect::<Vec<_>>());
    }

    if cmd == "bar" {
        return bar_command(&c, root, &atom, &args[1..]);
    }

    if cmd == "scratchpad" {
        return scratchpad_command(&c, root, &atom, &args[1..]);
    }

    if cmd == "urgent" {
        return urgent_command(&c, root, &atom, &args[1..]);
    }

    if cmd == "swallow" {
        return swallow_command(&c, root, &atom, &args[1..]);
    }

    if cmd == "studio" {
        return studio::command(&c, root, &args[1..], &mut StudioEffects);
    }

    if cmd == "restart-wm" {
        return restart_wm(&c, root, &args[1..]);
    }

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
        // Local on the focused screen, matching Super+N. Actions 1 and 2 are
        // the global-desktop protocol used by autostart, `_NET_WM_DESKTOP`
        // and NIRUBAR, so they keep their existing meaning.
        "tag" => (13, parse_tag(&args)?),
        "movetotag" => (14, parse_tag(&args)?),
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
                Some("scroll") => 7,
                Some(other) => {
                    return Err(format!(
                        "layout {other}: use show, save, columns, or one of tile, left, monocle, centered, centeredfloat, fibonacci, floating, scroll"
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

/// Exit status of `niructl restart-wm` when the running manager or session
/// cannot restart in place, so `nirux update` can tell "log in again" from a
/// failure.
const RESTART_UNSUPPORTED: i32 = 3;

/// The running manager's name ("NIRUWM 0.23.0") and its process id, from
/// its check window (`_NET_WM_PID`; 0 for a manager that does not say).
fn running_wm(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
) -> Option<(Window, String, u32)> {
    let check = property_u32(c, root, atom_named(c, "_NET_SUPPORTING_WM_CHECK").ok()?)?;
    // A stale property can name a window that is already gone.
    c.get_window_attributes(check).ok()?.reply().ok()?;
    let pid = property_u32(c, check, atom_named(c, "_NET_WM_PID").ok()?).unwrap_or(0);
    let name = c
        .get_property(
            false,
            check,
            atom_named(c, "_NET_WM_NAME").ok()?,
            atom_named(c, "UTF8_STRING").ok()?,
            0,
            64,
        )
        .ok()?
        .reply()
        .ok()
        .map(|r| String::from_utf8_lossy(&r.value).to_string())
        .unwrap_or_default();
    Some((check, name, pid))
}

/// `niructl restart-wm [--check]`: run the installed NIRUWM in this session.
///
/// The manager saves its window state and exits with the restart status;
/// niruwm-session restarts the session helpers whose binary changed and starts
/// the installed manager, which adopts every window where it was. While the
/// screen is locked nothing is restarted: the request waits in the background
/// until the lock is gone (`--after-unlock`), so a locked screen stays locked.
fn restart_wm(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    args: &[String],
) -> Result<()> {
    let check_only = args.iter().any(|a| a == "--check");
    let after_unlock = args.iter().any(|a| a == "--after-unlock");
    let quiet = args.iter().any(|a| a == "--quiet");
    if let Some(bad) = args
        .iter()
        .find(|a| !matches!(a.as_str(), "--check" | "--after-unlock" | "--quiet"))
    {
        return Err(format!("restart-wm: unknown option {bad}; use --check").into());
    }
    let Some((check, name, old_pid)) = running_wm(c, root) else {
        eprintln!("niructl: restart-wm: no window manager is running on this display");
        std::process::exit(RESTART_UNSUPPORTED);
    };
    if property_u32(c, check, atom_named(c, "_NIRUWM_RESTART_CONTROL")?) != Some(1) {
        // A NIRUWM that knows the command but runs outside niruwm-session
        // 0.23.0 or later (an older session script, or started by hand) has
        // nothing that would start it again.
        let knows = name
            .strip_prefix("NIRUWM ")
            .and_then(|v| {
                let mut parts = v.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
                Some((parts.next()?, parts.next().unwrap_or(0)))
            })
            .is_some_and(|(major, minor)| major > 0 || minor >= 23);
        let why = if knows {
            "its session script is older than NIRUWM 0.23.0, or it was not started by niruwm-session"
        } else {
            "it is older than NIRUWM 0.23.0"
        };
        let who = if name.is_empty() {
            "the running window manager".to_string()
        } else {
            name
        };
        eprintln!(
            "niructl: {who} cannot restart in place ({why}); log out and back in once to activate the update"
        );
        std::process::exit(RESTART_UNSUPPORTED);
    }
    if check_only {
        if !quiet {
            println!("{name}: can restart in place");
        }
        return Ok(());
    }
    if ready_lock(c, root).is_some() {
        if after_unlock {
            // Wait for the unlock; a session that ends meanwhile ends this.
            let deadline = Instant::now() + Duration::from_secs(24 * 3600);
            while ready_lock(c, root).is_some() {
                if Instant::now() >= deadline || running_wm(c, root).map(|w| w.2) != Some(old_pid) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_secs(2));
            }
            // Give the lock screen time to hand the keyboard back.
            std::thread::sleep(Duration::from_secs(2));
        } else {
            use std::os::unix::process::CommandExt as _;
            let me = std::env::current_exe()?;
            let mut waiter = std::process::Command::new(me);
            // Its own session: closing the terminal that ran `nirux update`
            // must not cancel the restart that waits for the unlock.
            // SAFETY: setsid is async-signal-safe and takes no arguments.
            unsafe {
                waiter.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
            waiter
                .args(["restart-wm", "--after-unlock"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            println!("The screen is locked: NIRUWM restarts after it is unlocked.");
            return Ok(());
        }
    }
    let serial = command_serial() & 0x7fff_ffff;
    let ack = atom_named(c, "_NIRUWM_RESTART_ACK")?;
    send(c, root, atom_named(c, CMD_ATOM)?, [22, serial, 0, 0, 0])?;
    c.flush()?;
    let mut answer = None;
    for _ in 0..250 {
        match property_u32(c, root, ack) {
            Some(v) if v == serial => {
                answer = Some(true);
                break;
            }
            Some(v) if v == serial | 0x8000_0000 => {
                answer = Some(false);
                break;
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    match answer {
        Some(true) => {}
        Some(false) => {
            // The screen was locked again between the unlock and the request
            // (a quick relock, or the idle lock). Keep waiting for the next
            // unlock instead of dropping the update until the next login.
            if after_unlock && ready_lock(c, root).is_some() {
                return restart_wm(c, root, args);
            }
            return Err("the window manager refused to restart (the screen is locked, or its state could not be saved; see the session log)".into());
        }
        None => return Err("the window manager did not answer the restart request".into()),
    }
    // The new manager is a new process with its own check window.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some((_, new_name, pid)) = running_wm(c, root) {
            if pid != old_pid && pid != 0 {
                if !quiet {
                    println!(
                        "Restarted in place: {} -> {new_name}",
                        if name.is_empty() { "?" } else { &name }
                    );
                }
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("the window manager stopped but no new one has started yet; see ~/.local/state/niruwm/session.log".into())
}

/// One line of `_NIRUWM_SCRATCHPADS`: slot (0 default, 1-9), state, window, class.
fn scratchpad_rows(text: &str) -> Vec<(u8, String, u32, String)> {
    text.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let slot = f.next()?.parse().ok()?;
            let state = f.next()?.to_string();
            let window = f.next()?.parse().ok()?;
            let class = f.next().unwrap_or("").to_string();
            Some((slot, state, window, class))
        })
        .collect()
}

/// `niructl scratchpad`: show, hide, send and list scratchpads.
///
///   scratchpad [N] [toggle|show|hide]   N = 1-9, none = the default (Super+Shift+S)
///   scratchpad send N                   the focused window to slot N (again: back)
///   scratchpad send N --window ID       that window to slot N, whatever has the focus
///   scratchpad unscratch                the focused scratchpad becomes a normal window
///   scratchpad list [--json]
fn scratchpad_command(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    args: &[String],
) -> Result<()> {
    const HELP: &str = "Usage: niructl scratchpad [N] [toggle|show|hide] | send N [--window ID] | unscratch | list [--json]   (N = 1-9; no N = the default scratchpad)";
    let slot_arg = |text: &str| -> Result<u32> {
        match text.parse::<u32>() {
            Ok(n) if (1..=9).contains(&n) => Ok(n),
            _ => Err(format!("scratchpad: slot must be 1 to 9, got {text:?}\n{HELP}").into()),
        }
    };
    // 0 or "default" names the Super+Shift+S scratchpad for show and hide.
    let show_slot = |text: &str| -> Result<u32> {
        if matches!(text, "0" | "default") {
            Ok(0)
        } else {
            slot_arg(text)
        }
    };
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let data = match words.as_slice() {
        ["list"] | ["list", "--json"] | ["status"] => {
            let reply = c
                .get_property(
                    false,
                    root,
                    atom("_NIRUWM_SCRATCHPADS")?,
                    atom("UTF8_STRING")?,
                    0,
                    4096,
                )?
                .reply()?;
            let rows = scratchpad_rows(&String::from_utf8_lossy(&reply.value));
            let title = |w: u32| -> String {
                if w == 0 {
                    return String::new();
                }
                atom("_NET_WM_NAME")
                    .ok()
                    .and_then(|name| {
                        c.get_property(false, w, name, atom("UTF8_STRING").ok()?, 0, 256)
                            .ok()
                    })
                    .and_then(|cookie| cookie.reply().ok())
                    .map(|r| String::from_utf8_lossy(&r.value).to_string())
                    .filter(|t| !t.is_empty())
                    .or_else(|| {
                        c.get_property(false, w, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256)
                            .ok()
                            .and_then(|cookie| cookie.reply().ok())
                            .map(|r| String::from_utf8_lossy(&r.value).to_string())
                    })
                    .unwrap_or_default()
            };
            let key = |slot: u8| -> String {
                if slot == 0 {
                    "Super+Shift+S".into()
                } else {
                    format!("Super+Alt+{slot}")
                }
            };
            if words.contains(&"--json") {
                let list: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|(slot, state, window, class)| {
                        serde_json::json!({
                            "slot": slot,
                            "key": key(*slot),
                            "state": state,
                            "window": window,
                            "class": class,
                            "title": title(*window),
                        })
                    })
                    .collect();
                println!("{}", serde_json::json!({"schema": 1, "scratchpads": list}));
            } else if rows.is_empty() {
                println!("no scratchpads (Super+Shift+S opens the default one)");
            } else {
                for (slot, state, window, class) in &rows {
                    let name = if *slot == 0 {
                        "default".to_string()
                    } else {
                        slot.to_string()
                    };
                    let what = if *window == 0 {
                        "not started".to_string()
                    } else {
                        let t = title(*window);
                        if t.is_empty() {
                            class.clone()
                        } else {
                            t
                        }
                    };
                    println!("{name:<8} {:<14} {state:<7} {what}", key(*slot));
                }
            }
            return Ok(());
        }
        ["send", n] => [16, slot_arg(n)?, 0, 0, 0],
        ["send", n, "--window", id] | ["send", "--window", id, n] => {
            let parsed = match id.strip_prefix("0x").or_else(|| id.strip_prefix("0X")) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => id.parse::<u32>().ok(),
            };
            match parsed {
                Some(w) if w != 0 => [18, slot_arg(n)?, w, 0, 0],
                _ => return Err(format!("scratchpad: not a window id: {id:?}\n{HELP}").into()),
            }
        }
        ["unscratch"] => [17, 0, 0, 0, 0],
        [] => [15, 0, 0, 0, 0],
        [op @ ("toggle" | "show" | "hide")] => [15, 0, op_code(op), 0, 0],
        [n] => [15, show_slot(n)?, 0, 0, 0],
        [n, op @ ("toggle" | "show" | "hide")] => [15, show_slot(n)?, op_code(op), 0, 0],
        _ => return Err(HELP.into()),
    };
    send(c, root, atom(CMD_ATOM)?, data)?;
    c.flush()?;
    Ok(())
}

/// `niructl urgent`: the windows that asked for attention (newest last), or
/// go to the newest one, like Super+Shift+U.
fn urgent_command(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    args: &[String],
) -> Result<()> {
    const HELP: &str = "Usage: niructl urgent [list|--json|focus]";
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    // A manager from before 0.19.0 never publishes the property at all.
    let supported = {
        let supported = atom("_NET_SUPPORTED")?;
        let wanted = atom("_NET_WM_STATE_DEMANDS_ATTENTION")?;
        c.get_property(false, root, supported, AtomEnum::ATOM, 0, 1024)?
            .reply()?
            .value32()
            .is_some_and(|mut v| v.any(|a| a == wanted))
    };
    if !supported {
        return Err(
            "the running window manager does not track urgent windows; log out/in to activate NIRUWM 0.19"
                .into(),
        );
    }
    match words.as_slice() {
        ["focus"] => {
            send(c, root, atom(CMD_ATOM)?, [20, 0, 0, 0, 0])?;
            c.flush()?;
            Ok(())
        }
        [] | ["list"] | ["status"] | ["--json"] | ["list", "--json"] => {
            let windows: Vec<u32> = c
                .get_property(
                    false,
                    root,
                    atom("_NIRUWM_URGENT")?,
                    AtomEnum::WINDOW,
                    0,
                    1024,
                )?
                .reply()?
                .value32()
                .map(|v| v.collect())
                .unwrap_or_default();
            let desktop_atom = atom("_NET_WM_DESKTOP")?;
            let name_atom = atom("_NET_WM_NAME")?;
            let rows: Vec<(u32, Option<u32>, String, String)> = windows
                .iter()
                .map(|&w| {
                    let desktop = property_u32(c, w, desktop_atom);
                    let class = c
                        .get_property(false, w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
                        .ok()
                        .and_then(|cookie| cookie.reply().ok())
                        .map(|r| {
                            String::from_utf8_lossy(&r.value)
                                .split('\0')
                                .rfind(|p| !p.is_empty())
                                .unwrap_or("")
                                .to_string()
                        })
                        .unwrap_or_default();
                    let title = property_string(c, w, name_atom)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    (w, desktop, class, title)
                })
                .collect();
            // 0xFFFFFFFF is a scratchpad; the rest are global desktops.
            let place = |desktop: Option<u32>| -> String {
                match desktop {
                    Some(0xFFFF_FFFF) => "scratchpad".into(),
                    Some(d) => format!("workspace {}", d + 1),
                    None => "unknown".into(),
                }
            };
            if words.contains(&"--json") {
                let list: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|(w, desktop, class, title)| {
                        serde_json::json!({
                            "window": format!("{w:#x}"),
                            "desktop": desktop.filter(|d| *d != 0xFFFF_FFFF),
                            "scratchpad": *desktop == Some(0xFFFF_FFFF),
                            "class": class,
                            "title": title,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({ "urgent": list }))?
                );
            } else if rows.is_empty() {
                println!("no window asks for attention");
            } else {
                for (w, desktop, class, title) in &rows {
                    println!("{w:#x}\t{}\t{class}\t{title}", place(*desktop));
                }
            }
            Ok(())
        }
        _ => Err(HELP.into()),
    }
}

/// `niructl swallow`: window swallowing for the running session. The file
/// setting is `[swallow] enabled`; this switch lasts until the next login.
fn swallow_command(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    args: &[String],
) -> Result<()> {
    const HELP: &str = "Usage: niructl swallow on|off|toggle|status";
    let state = || -> Result<String> {
        let text = property_string(c, root, atom("_NIRUWM_SWALLOW")?)?.unwrap_or_default();
        if text.is_empty() {
            return Err(
                "the running window manager cannot swallow windows; log out/in to activate NIRUWM 0.19"
                    .into(),
            );
        }
        Ok(text)
    };
    let mode = args.first().map(String::as_str).unwrap_or("status");
    let code = match mode {
        "status" => {
            println!("swallow {}", state()?);
            return Ok(());
        }
        "toggle" => 0,
        "on" => 1,
        "off" => 2,
        _ => return Err(HELP.into()),
    };
    let before = state()?;
    let expected = match code {
        1 => "on".to_string(),
        2 => "off".to_string(),
        _ => if before == "on" { "off" } else { "on" }.to_string(),
    };
    send(c, root, atom(CMD_ATOM)?, [21, code, 0, 0, 0])?;
    c.flush()?;
    for _ in 0..100 {
        if state()? == expected {
            println!("swallow {expected}");
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err("the window manager did not confirm the swallow change".into())
}

fn op_code(op: &str) -> u32 {
    match op {
        "show" => 1,
        "hide" => 2,
        _ => 0,
    }
}

fn bar_state_from(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
) -> Result<String> {
    let reply = c
        .get_property(
            false,
            root,
            atom("_NIRUWM_BAR_STATE")?,
            atom("UTF8_STRING")?,
            0,
            32,
        )?
        .reply()?;
    let state = String::from_utf8_lossy(&reply.value).trim().to_string();
    if state.is_empty() {
        return Err("the running window manager does not expose bar state; log out/in to activate this NIRUWM".into());
    }
    Ok(state)
}

fn bar_command(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: &dyn Fn(&str) -> Result<Atom>,
    args: &[String],
) -> Result<()> {
    let mode = args.first().map(String::as_str).unwrap_or("status");
    if mode == "status" {
        println!("{}", bar_state_from(c, root, atom)?);
        return Ok(());
    }
    let before = bar_state_from(c, root, atom).unwrap_or_else(|_| "unknown".into());
    let code = match mode {
        "top" => 1,
        "bottom" => 2,
        "hide" => 3,
        "show" => 4,
        "toggle" => 5,
        "cycle" => 6,
        _ => {
            return Err("bar needs top, bottom, hide, show, toggle, cycle or status".into());
        }
    };
    let expected = match mode {
        "top" => vec!["top"],
        "bottom" => vec!["bottom"],
        "hide" => vec!["hidden", "disabled"],
        "show" => vec!["top", "bottom"],
        "toggle" => {
            if matches!(before.as_str(), "hidden" | "disabled") {
                vec!["top", "bottom"]
            } else {
                vec!["hidden", "disabled"]
            }
        }
        "cycle" => match before.as_str() {
            "top" => vec!["bottom"],
            "bottom" => vec!["hidden", "disabled"],
            _ => vec!["top"],
        },
        _ => unreachable!(),
    };
    send(c, root, atom(CMD_ATOM)?, [12, code, 0, 0, 0])?;
    c.flush()?;
    for _ in 0..200 {
        if let Ok(state) = bar_state_from(c, root, atom) {
            if expected.contains(&state.as_str()) {
                println!("{state}");
                return Ok(());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err("window manager did not apply the bar command".into())
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

fn property_string(
    c: &x11rb::rust_connection::RustConnection,
    w: Window,
    atom: Atom,
) -> Result<Option<String>> {
    let Some(reply) = c
        .get_property(false, w, atom, AtomEnum::ANY, 0, 64)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
    else {
        return Ok(None);
    };
    if reply.value.is_empty() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&reply.value).trim().to_string();
    Ok((!text.is_empty()).then_some(text))
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
    // `niructl keys | head` ends quietly instead of panicking.
    stdout::exit_on_broken_pipe();
    if let Err(e) = run() {
        eprintln!("niructl: {e}");
        std::process::exit(1)
    }
}

#[cfg(test)]
mod display_parse_tests {
    use super::{
        display_connected_signature, display_delete_profile, display_output_signature,
        display_outputs_from, display_rename_profile, display_resolve, profile_outputs_connected,
        DisplayFile, DisplayOutput, DisplayProfile, WatchPidFile,
    };

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

    fn display_file_with_startup() -> DisplayFile {
        DisplayFile {
            profile: vec![
                DisplayProfile {
                    name: "Desk".into(),
                    outputs: Vec::new(),
                    ..Default::default()
                },
                DisplayProfile {
                    name: "Other".into(),
                    outputs: Vec::new(),
                    ..Default::default()
                },
            ],
            startup_profile: Some("Desk".into()),
            automatic: false,
        }
    }

    #[test]
    fn renaming_a_startup_profile_moves_the_reference() {
        let mut file = display_file_with_startup();
        display_rename_profile(&mut file, "Desk", "Office").unwrap();
        assert_eq!(file.startup_profile.as_deref(), Some("Office"));
    }

    #[test]
    fn deleting_a_startup_profile_clears_the_reference() {
        let mut file = display_file_with_startup();
        display_delete_profile(&mut file, "Desk").unwrap();
        assert_eq!(file.startup_profile, None);
        assert_eq!(file.profile.len(), 1);
    }

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
            identity: None,
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
            identity: None,
        };
        let resolved = display_resolve(&outputs, &profile).expect("an off screen is placeable");
        assert_eq!(resolved.mode, "1920x1200");
    }

    fn wanted_output(name: &str) -> DisplayOutput {
        DisplayOutput {
            name: name.into(),
            mode: "preferred".into(),
            rate: None,
            position: "0x0".into(),
            rotation: "normal".into(),
            primary: false,
            identity: None,
        }
    }

    #[test]
    fn the_output_signature_tracks_connected_names_and_enabled_state() {
        let outputs = display_outputs_from(SAMPLE);
        let signature = display_output_signature(&outputs);
        assert_eq!(
            signature,
            vec![
                ("DisplayPort-4".into(), true),
                ("DisplayPort-5".into(), true),
                ("DisplayPort-6".into(), false),
            ]
        );
        // Re-reading the same state, as an ordinary pointer or window event
        // would, must not look like a display transition.
        assert_eq!(
            display_output_signature(&display_outputs_from(SAMPLE)),
            signature
        );

        let mut enabled = outputs.clone();
        enabled
            .iter_mut()
            .find(|output| output.name == "DisplayPort-6")
            .unwrap()
            .current = Some(wanted_output("DisplayPort-6"));
        assert_ne!(display_output_signature(&enabled), signature);

        let mut removed = outputs;
        removed
            .iter_mut()
            .find(|output| output.name == "DisplayPort-5")
            .unwrap()
            .connected = false;
        removed
            .iter_mut()
            .find(|output| output.name == "DisplayPort-4")
            .unwrap()
            .current = None;
        let changed = display_output_signature(&removed);
        assert_eq!(
            changed,
            vec![
                ("DisplayPort-4".into(), false),
                ("DisplayPort-6".into(), false)
            ]
        );
    }

    #[test]
    fn a_manual_enabled_change_does_not_change_the_connected_set() {
        let outputs = display_outputs_from(SAMPLE);
        let connected = display_connected_signature(&outputs);
        let mut manual = outputs.clone();
        manual
            .iter_mut()
            .find(|output| output.name == "DisplayPort-4")
            .unwrap()
            .current = None;
        assert_ne!(
            display_output_signature(&manual),
            display_output_signature(&outputs)
        );
        assert_eq!(display_connected_signature(&manual), connected);
    }

    #[test]
    fn a_startup_profile_matches_only_when_every_output_is_connected() {
        let outputs = display_outputs_from(SAMPLE);
        let profile = DisplayProfile {
            name: "Dock".into(),
            outputs: vec![
                wanted_output("DisplayPort-4"),
                wanted_output("DisplayPort-6"),
            ],
            ..Default::default()
        };
        // DisplayPort-6 is connected but switched off; that is still matched.
        assert!(profile_outputs_connected(&outputs, &profile));
        let missing = DisplayProfile {
            name: "Missing".into(),
            outputs: vec![
                wanted_output("DisplayPort-4"),
                wanted_output("DisplayPort-3"),
            ],
            ..Default::default()
        };
        assert!(!profile_outputs_connected(&outputs, &missing));
        let empty = DisplayProfile {
            name: "Empty".into(),
            outputs: Vec::new(),
            ..Default::default()
        };
        assert!(!profile_outputs_connected(&outputs, &empty));
    }

    #[test]
    fn a_stale_watch_pid_file_is_reclaimed() {
        let dir = std::env::temp_dir().join(format!("niructl-watch-{}-stale", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("display-watch.pid");
        std::fs::write(&path, "999999\n").unwrap();
        let guard = WatchPidFile::acquire_with(path.clone(), 4242, |pid| pid == 4242)
            .unwrap()
            .expect("a stale PID must be reclaimed");
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "4242");
        assert!(
            WatchPidFile::acquire_with(path.clone(), 5500, |pid| pid == 4242)
                .unwrap()
                .is_none(),
            "a live watcher must block a duplicate"
        );
        drop(guard);
        assert!(!path.exists(), "the owner removes its PID file");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_invalid_watch_pid_file_is_reclaimed() {
        let dir =
            std::env::temp_dir().join(format!("niructl-watch-{}-invalid", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("display-watch.pid");
        std::fs::write(&path, "not-a-pid\n").unwrap();
        let guard = WatchPidFile::acquire_with(path.clone(), 4243, |_| true)
            .unwrap()
            .expect("an unreadable PID must be reclaimed");
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "4243");
        drop(guard);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn autostart_decision_respects_restore_session() {
        assert!(!autostart_suppressed(false, true));
        assert!(!autostart_suppressed(false, false));
        assert!(autostart_suppressed(true, true));
        assert!(!autostart_suppressed(true, false));
    }

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
    fn parses_audio_streams() {
        let text = "Audio\n ├─ Streams:\n │      91. Firefox [vol: 0.50]\n │      92. Discord [vol: 1.00]\nVideo\n";
        assert_eq!(
            audio_streams_from(text),
            vec![
                AudioNode {
                    id: 91,
                    name: "Firefox".into(),
                    default: false
                },
                AudioNode {
                    id: 92,
                    name: "Discord".into(),
                    default: false
                }
            ]
        );
    }

    #[test]
    fn analyzes_recorded_wav_levels() {
        let samples = [0i16, 1000, -2000, 32767];
        let data_len = (samples.len() * 2) as u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&48000u32.to_le_bytes());
        wav.extend_from_slice(&96000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for sample in samples {
            wav.extend_from_slice(&sample.to_le_bytes());
        }
        let (peak, rms, clipping, verdict) = wav_analysis(&wav).unwrap();
        assert!(peak > -1.0 && rms > -20.0);
        assert!(clipping);
        assert_eq!(verdict, "too hot");
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
        wait_for("niruscreensaver \n");
        assert!(!calls_now().contains("--preview"));
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
    fn docked_lid_locks_on_the_closing_edge_only() {
        assert!(super::lid_closed_now(Some(false), Some(true)));
        assert!(
            !super::lid_closed_now(Some(true), Some(true)),
            "stays closed"
        );
        assert!(
            !super::lid_closed_now(None, Some(true)),
            "closed at start or undocked"
        );
        assert!(!super::lid_closed_now(Some(true), Some(false)), "opened");
        assert!(
            !super::lid_closed_now(Some(false), None),
            "undocked: logind suspends"
        );
    }

    #[test]
    fn screensaver_pids_ignores_other_displays_and_this_process() {
        // No NIRUSCREENSAVER runs on a display that cannot exist, and the
        // scan never returns this test process.
        assert!(super::screensaver_pids(Some(":nirux-test-no-display")).is_empty());
        let me = std::process::id() as i32;
        assert!(!super::screensaver_pids(None).contains(&me));
        assert!(super::zombie(i32::MAX));
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

#[cfg(test)]
mod action_tests {
    use super::{human_bytes, scan_size, InputConfig, KeyboardInput, PointerInput};

    fn lock_args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn a_lock_typed_in_a_terminal_is_manual_and_a_scripted_one_is_silent() {
        assert_eq!(super::lock_reason(&[], true), "manual");
        assert_eq!(super::lock_reason(&[], false), "script");
    }

    #[test]
    fn lock_options_name_the_reason() {
        use super::lock_reason;
        assert_eq!(lock_reason(&lock_args(&["--manual"]), false), "manual");
        assert_eq!(lock_reason(&lock_args(&["--silent"]), true), "script");
        assert_eq!(lock_reason(&lock_args(&["--reason", "idle"]), true), "idle");
        assert_eq!(
            lock_reason(&lock_args(&["--reason=suspend"]), true),
            "suspend"
        );
        assert_eq!(
            lock_reason(&lock_args(&["--manual", "--silent"]), true),
            "script"
        );
    }

    #[test]
    fn a_wrong_lock_option_still_locks_silently() {
        use super::lock_reason;
        assert_eq!(
            lock_reason(&lock_args(&["--reason", "party"]), true),
            "script"
        );
        assert_eq!(lock_reason(&lock_args(&["--reason"]), true), "script");
        assert_eq!(lock_reason(&lock_args(&["--loud"]), true), "script");
    }
    use std::fs;

    #[test]
    fn storage_scan_counts_files_without_following_symlinks() {
        let dir = std::env::temp_dir().join(format!("niructl-storage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("a"), b"1234").unwrap();
        fs::write(dir.join("nested/b"), b"12").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("a"), dir.join("link")).unwrap();
        let (bytes, files) = scan_size(&dir);
        assert_eq!(bytes, 6);
        assert_eq!(files, 2);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn input_config_round_trips_and_defaults() {
        let config = InputConfig {
            keyboard: KeyboardInput {
                layout: "se".into(),
                variant: "nodeadkeys".into(),
                repeat_delay: Some(300),
                repeat_rate: Some(30),
            },
            pointer: PointerInput {
                natural_scroll: Some(true),
                tap: Some(false),
                speed: Some(0.2),
            },
        };
        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: InputConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed.keyboard.layout, "se");
        assert_eq!(parsed.keyboard.repeat_delay, Some(300));
        assert_eq!(parsed.pointer.natural_scroll, Some(true));
        let empty: InputConfig = toml::from_str("").unwrap();
        assert_eq!(empty.keyboard.repeat_delay, None);
        assert_eq!(empty.pointer.tap, None);
    }

    #[test]
    fn byte_units_are_human_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
    }
}

#[cfg(test)]
mod saved_action_tests {
    use super::{
        action_delete, action_rename, action_save, action_set_favorite, actions_json_value,
        read_actions_from, validate_action_command, validate_action_name, validate_actions_file,
        write_actions_to, ActionsFile, SavedAction,
    };
    use std::fs;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("niructl-actions-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn action_names_and_commands_are_bounded() {
        assert!(validate_action_name("").is_err());
        assert!(validate_action_name("   ").is_err());
        assert!(validate_action_name(&"x".repeat(81)).is_err());
        assert!(validate_action_name("bad\0name").is_err());
        assert!(validate_action_name("bad\nname").is_err());
        assert!(validate_action_name("Edit notes").is_ok());
        assert!(validate_action_command(&[]).is_err());
        assert!(validate_action_command(&["".into()]).is_err());
        assert!(validate_action_command(&["bad\0arg".into()]).is_err());
        let too_many = vec!["arg".to_string(); 33];
        assert!(validate_action_command(&too_many).is_err());
        assert!(validate_action_command(&["kitty".into(), "-e".into()]).is_ok());
    }

    #[test]
    fn actions_round_trip_atomically_and_reject_duplicates() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("actions.toml");
        let mut file = ActionsFile::default();
        action_save(
            &mut file,
            "Edit",
            vec!["kitty".into(), "-e".into(), "vim".into()],
            123,
        )
        .unwrap();
        assert!(action_save(&mut file, "Edit", vec!["nvim".into()], 124).is_err());
        action_set_favorite(&mut file, "Edit", true).unwrap();
        write_actions_to(&path, &file).unwrap();
        let loaded = read_actions_from(&path).unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.actions.len(), 1);
        assert_eq!(loaded.actions[0].name, "Edit");
        assert_eq!(
            loaded.actions[0].command,
            vec!["kitty".to_string(), "-e".into(), "vim".into()]
        );
        assert!(loaded.actions[0].favorite);
        assert_eq!(loaded.actions[0].created, 123);
        // Only the owned file remains; no temporary file is left behind.
        let names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["actions.toml".to_string()]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_entries_never_replace_the_stored_file() {
        let dir = temp_dir("validate-before-replace");
        let path = dir.join("actions.toml");
        let mut file = ActionsFile::default();
        action_save(&mut file, "One", vec!["true".into()], 1).unwrap();
        write_actions_to(&path, &file).unwrap();
        let before = fs::read_to_string(&path).unwrap();

        let duplicate = SavedAction {
            name: "One".into(),
            command: vec!["false".into()],
            favorite: false,
            created: 2,
        };
        file.actions.push(duplicate);
        assert!(validate_actions_file(&file).is_err());
        assert!(write_actions_to(&path, &file).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_delete_and_pin_have_exact_idempotent_semantics() {
        let mut file = ActionsFile::default();
        action_save(&mut file, "One", vec!["one".into()], 1).unwrap();
        action_save(&mut file, "Two", vec!["two".into()], 2).unwrap();
        action_set_favorite(&mut file, "One", true).unwrap();
        action_set_favorite(&mut file, "One", true).unwrap();
        action_rename(&mut file, "One", "Renamed").unwrap();
        assert!(file.actions[0].favorite, "rename keeps the favorite flag");
        assert!(action_rename(&mut file, "Renamed", "Two").is_err());
        assert!(action_rename(&mut file, "Missing", "Other").is_err());
        action_set_favorite(&mut file, "Two", false).unwrap();
        action_set_favorite(&mut file, "Two", false).unwrap();
        action_delete(&mut file, "Renamed").unwrap();
        assert!(action_delete(&mut file, "Renamed").is_err());
        assert_eq!(file.actions.len(), 1);
        assert_eq!(file.actions[0].name, "Two");
    }

    #[test]
    fn json_listing_is_an_array_with_launcher_fields_only() {
        let mut file = ActionsFile::default();
        action_save(&mut file, "Edit", vec!["kitty".into(), "-e".into()], 1).unwrap();
        action_save(&mut file, "Files", vec!["thunar".into()], 2).unwrap();
        action_set_favorite(&mut file, "Files", true).unwrap();
        let value = actions_json_value(&file);
        let array = value.as_array().expect("a JSON array");
        assert_eq!(array.len(), 2);
        assert_eq!(array[0]["name"], "Edit");
        assert_eq!(array[0]["command"][0], "kitty");
        assert_eq!(array[0]["favorite"], false);
        assert_eq!(array[1]["name"], "Files");
        assert_eq!(array[1]["favorite"], true);
        for item in array {
            let mut keys: Vec<&str> = item
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, vec!["command", "favorite", "name"]);
            assert!(item["name"].is_string());
            assert!(item["command"].is_array());
            assert!(item["favorite"].is_boolean());
        }
    }
}

#[cfg(test)]
mod display_uevent_tests {
    use super::drm_uevent_is_hotplug;

    #[test]
    fn only_drm_changes_count() {
        let hotplug = b"change@/devices/pci0000:00/0000:00:02.0/drm/card1\0ACTION=change\0DEVPATH=/devices/pci0000:00/0000:00:02.0/drm/card1\0SUBSYSTEM=drm\0HOTPLUG=1\0DEVNAME=dri/card1\0SEQNUM=4242\0";
        assert!(drm_uevent_is_hotplug(hotplug));
        let usb = b"add@/devices/usb1/1-1\0ACTION=add\0SUBSYSTEM=usb\0SEQNUM=1\0";
        assert!(!drm_uevent_is_hotplug(usb));
        let drm_add = b"add@/devices/drm/card1/card1-DP-1\0ACTION=add\0SUBSYSTEM=drm\0";
        assert!(!drm_uevent_is_hotplug(drm_add));
    }
}

// ── niructl config: history, undo, diff and reset ──────────────────────────

fn config_file() -> std::path::PathBuf {
    config::directory().join("config.toml")
}

fn history_label(path: &std::path::Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let digits: String = stem
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(14)
        .collect();
    if digits.len() == 14 {
        format!(
            "{}-{}-{} {}:{}:{}",
            &digits[0..4],
            &digits[4..6],
            &digits[6..8],
            &digits[8..10],
            &digits[10..12],
            &digits[12..14]
        )
    } else {
        stem.to_string()
    }
}

fn stdin_is_terminal() -> bool {
    unsafe { libc::isatty(0) == 1 }
}

/// Show what `new` changes, ask (or require --yes when nobody can answer),
/// keep the current file in the history, write, and reload the session.
fn config_apply(current: &str, new: &str, what: &str, yes: bool) -> Result<()> {
    if let Err(e) = Config::from_text(new) {
        return Err(format!(
            "{what} would not load ({}); nothing was changed",
            config::short_error(&e.to_string())
        )
        .into());
    }
    let diff = config_tools::unified_diff(
        current,
        new,
        "config.toml now",
        &format!("config.toml after {what}"),
    );
    if diff.is_empty() {
        println!("Nothing to change: config.toml already matches.");
        return Ok(());
    }
    print!("{diff}");
    if !yes {
        if !stdin_is_terminal() {
            println!("\nThis is a preview. Run the same command with --yes to apply it.");
            return Ok(());
        }
        print!("\nApply {what}? [y/N] ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "j" | "J" | "ja") {
            println!("Nothing was changed.");
            return Ok(());
        }
    }
    config::history_record(current);
    let path = config_file();
    let dir = path.parent().ok_or("configuration directory is missing")?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".config.toml.{}", std::process::id()));
    fs::write(&tmp, new)?;
    fs::rename(&tmp, &path)?;
    // It loads (checked above), so it is a working version too: a second
    // undo then steps back from here even before the session reloads.
    config::history_record(new);
    println!("Applied {what}. The previous version is in the history: niructl config undo");
    let reloaded = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            std::process::Command::new(exe)
                .arg("reload")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .ok()
        })
        .is_some_and(|s| s.success());
    if reloaded {
        println!("The session reloaded the settings.");
    } else {
        println!("No running NIRUWM answered; the settings apply at the next start.");
    }
    Ok(())
}

fn config_command(args: &[String]) -> Result<()> {
    const HELP: &str = "Usage: niructl config history | diff [N|--all] | undo [STEPS] [--yes] | restore N [--yes] | reset SECTION|KEY|all [--yes]";
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let rest: Vec<&str> = args
        .iter()
        .skip(1)
        .map(String::as_str)
        .filter(|a| *a != "--yes" && *a != "-y")
        .collect();
    let current = match fs::read_to_string(config_file()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let entries = config::history_entries();
    let texts: Vec<String> = entries
        .iter()
        .map(|p| fs::read_to_string(p).unwrap_or_default())
        .collect();
    let position = texts.iter().position(|t| *t == current);
    let pick = |n: usize| -> Result<(String, String)> {
        texts
            .get(n)
            .cloned()
            .map(|t| (t, history_label(&entries[n])))
            .ok_or_else(|| {
                format!(
                    "there is no version {n}; `niructl config history` lists {} version(s)",
                    texts.len()
                )
                .into()
            })
    };
    match args.first().map(String::as_str) {
        Some("history") => {
            if entries.is_empty() {
                println!("No versions kept yet. NIRUWM keeps one each time config.toml loads.");
                return Ok(());
            }
            println!("Working versions of config.toml, newest first (* = the file now):\n");
            for (i, path) in entries.iter().enumerate() {
                let summary = match texts.get(i + 1) {
                    Some(older) => {
                        let names = config_tools::changed_sections(older, &texts[i]);
                        if names.is_empty() {
                            "only comments or layout".to_string()
                        } else {
                            names.join(", ")
                        }
                    }
                    None => "oldest kept".to_string(),
                };
                let mark = if Some(i) == position { "*" } else { " " };
                println!("{mark}{i:>3}  {}  {summary}", history_label(path));
            }
            if position.is_none() {
                println!(
                    "\nThe file now is not in the list: it has not loaded since it was edited."
                );
            }
            println!("\nniructl config diff N     what changed since version N");
            println!("niructl config restore N  go back to version N");
            println!("niructl config undo       go back one version");
            println!(
                "Kept in {} (the last {}).",
                config::history_dir().display(),
                config::HISTORY_KEEP
            );
            Ok(())
        }
        Some("diff") => {
            if let Some(n) = rest.first().and_then(|a| a.parse::<usize>().ok()) {
                let (old, label) = pick(n)?;
                let d = config_tools::unified_diff(
                    &old,
                    &current,
                    &format!("version {n} ({label})"),
                    "config.toml now",
                );
                if d.is_empty() {
                    println!("config.toml is the same as version {n}.");
                } else {
                    print!("{d}");
                }
                return Ok(());
            }
            let all = rest.contains(&"--all");
            let (template, source) = config_tools::template();
            let d = config_tools::differences(&current, &template)?;
            println!("Your settings compared with the defaults ({source}):\n");
            if d.is_empty() {
                println!("Nothing differs.");
            }
            for (title, lines) in [
                ("Changed", &d.changed),
                ("Added", &d.added),
                ("Removed from the defaults", &d.removed),
            ] {
                if !lines.is_empty() {
                    println!("{title}:");
                    for l in lines {
                        println!("  {l}");
                    }
                    println!();
                }
            }
            if all && !d.unset.is_empty() {
                println!("Not in your file (the built-in default applies):");
                for l in &d.unset {
                    println!("  {l}");
                }
                println!();
            } else if !d.unset.is_empty() {
                println!(
                    "{} default key(s) are not in your file; --all lists them.",
                    d.unset.len()
                );
            }
            println!("Reset a part with: niructl config reset SECTION   (for example bar)");
            Ok(())
        }
        Some("undo") => {
            let steps = match rest.first() {
                Some(a) => a.parse::<usize>().map_err(|_| HELP)?.max(1),
                None => 1,
            };
            let target = position.map_or(steps - 1, |p| p + steps);
            let (old, label) = pick(target).map_err(|_| -> Box<dyn std::error::Error> {
                "no older version is kept; `niructl config history` lists what there is".into()
            })?;
            config_apply(
                &current,
                &old,
                &format!("undo to version {target} ({label})"),
                yes,
            )
        }
        Some("restore") => {
            let n = rest
                .first()
                .and_then(|a| a.parse::<usize>().ok())
                .ok_or(HELP)?;
            let (old, label) = pick(n)?;
            config_apply(
                &current,
                &old,
                &format!("restore of version {n} ({label})"),
                yes,
            )
        }
        Some("reset") => {
            let (template, source) = config_tools::template();
            let Some(name) = rest.first() else {
                return Err(format!(
                    "name what to reset: {} or all",
                    config_tools::section_names(&current, &template).join(", ")
                )
                .into());
            };
            let new = config_tools::reset_text(&current, &template, name).map_err(|e| {
                format!(
                    "{e}. Sections: {} or all",
                    config_tools::section_names(&current, &template).join(", ")
                )
            })?;
            println!("Defaults from {source}.\n");
            config_apply(&current, &new, &format!("reset of {name}"), yes)
        }
        _ => Err(HELP.into()),
    }
}

// ── niructl battery: the charge limit ([battery] charge_limit) ─────────────

struct BatteryInfo {
    name: String,
    dir: std::path::PathBuf,
    capacity: Option<u32>,
    status: String,
    limit: Option<u32>,
    writable: bool,
}

fn battery_read(path: &std::path::Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn battery_writable(path: &std::path::Path) -> bool {
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: access() only reads the NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

fn batteries() -> Vec<BatteryInfo> {
    let root = std::path::Path::new(
        &std::env::var("NIRUCTL_POWER_SUPPLY").unwrap_or_else(|_| "/sys/class/power_supply".into()),
    )
    .to_path_buf();
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&root) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("BAT") {
            continue;
        }
        let dir = entry.path();
        let end = dir.join("charge_control_end_threshold");
        out.push(BatteryInfo {
            capacity: battery_read(&dir.join("capacity")).and_then(|v| v.parse().ok()),
            status: battery_read(&dir.join("status")).unwrap_or_default(),
            limit: battery_read(&end).and_then(|v| v.parse().ok()),
            writable: end.exists() && battery_writable(&end),
            name,
            dir,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// "Charge to full once" lasts until the next login: the marker lives in the
/// runtime directory, which the system clears when the session ends.
fn battery_full_marker() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("niructl-{}", unsafe { libc::getuid() }))
        });
    dir.join("nirux-battery-full")
}

fn battery_apply(target: u32) -> Result<Vec<String>> {
    let mut done = Vec::new();
    let all = batteries();
    if all.is_empty() {
        return Err("no battery".into());
    }
    for b in &all {
        let end = b.dir.join("charge_control_end_threshold");
        if !end.exists() {
            continue;
        }
        if !b.writable {
            return Err(format!(
                "{}: the charge limit is not writable for this user; `nirux update` installs the permission rule (log in again afterwards)",
                b.name
            )
            .into());
        }
        let start = b.dir.join("charge_control_start_threshold");
        let start_now = battery_read(&start).and_then(|v| v.parse::<u32>().ok());
        let lowering = match b.limit {
            Some(now) => target < now,
            None => true,
        };
        let fix_start =
            start.exists() && start_now.is_some_and(|v| v >= target) && battery_writable(&start);
        // Firmware rejects an end below the start, and some a start above the end.
        if lowering && fix_start {
            fs::write(&start, (target.saturating_sub(5)).to_string())?;
        }
        fs::write(&end, target.to_string())?;
        if !lowering && fix_start {
            fs::write(&start, (target.saturating_sub(5)).to_string())?;
        }
        done.push(format!("{}: charges to {target} %", b.name));
    }
    if done.is_empty() {
        return Err("this battery has no charge limit in Linux (no charge_control_end_threshold); many HP models lack it".into());
    }
    Ok(done)
}

fn battery_command(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    let marker = battery_full_marker();
    let configured = Config::load().map(|c| c.battery.charge_limit).unwrap_or(80);
    match sub {
        "status" => {
            let all = batteries();
            let full = marker.exists();
            if args.iter().any(|a| a == "--json") {
                let list: Vec<serde_json::Value> = all
                    .iter()
                    .map(|b| {
                        serde_json::json!({
                            "name": b.name, "capacity": b.capacity, "status": b.status,
                            "limit": b.limit, "supported": b.limit.is_some(), "writable": b.writable,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({"configured": configured, "full_once": full, "batteries": list})
                );
                return Ok(());
            }
            if all.is_empty() {
                println!("no battery");
                return Ok(());
            }
            for b in &all {
                let level = b
                    .capacity
                    .map(|c| format!("{c} %"))
                    .unwrap_or_else(|| "?".into());
                let limit = match (b.limit, b.writable) {
                    (None, _) => "no charge limit in Linux on this model".to_string(),
                    (Some(l), true) => format!("charges to {l} %"),
                    (Some(l), false) => format!(
                        "charges to {l} % (not writable: run `nirux update`, then log in again)"
                    ),
                };
                println!("{}: {level} {} · {limit}", b.name, b.status.to_lowercase());
            }
            println!(
                "configured: {configured} %{}",
                if full {
                    " · charging to 100 % once, until the next login (niructl battery limit ends it)"
                } else {
                    ""
                }
            );
            Ok(())
        }
        "apply" => {
            let target = if marker.exists() { 100 } else { configured };
            match battery_apply(target) {
                Ok(lines) => {
                    for l in lines {
                        println!("{l}");
                    }
                    Ok(())
                }
                // Desktops and unsupported laptops are not an error at login.
                Err(e) if batteries().iter().all(|b| b.limit.is_none()) => {
                    println!("{e}");
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        "full" => {
            if let Some(dir) = marker.parent() {
                fs::create_dir_all(dir)?;
            }
            for l in battery_apply(100)? {
                println!("{l} (once; back to {configured} % at the next login, or with niructl battery limit)");
            }
            fs::write(&marker, "1")?;
            Ok(())
        }
        "limit" => {
            let _ = fs::remove_file(&marker);
            for l in battery_apply(configured)? {
                println!("{l}");
            }
            Ok(())
        }
        _ => Err("Usage: niructl battery status [--json] | apply | full | limit".into()),
    }
}

#[cfg(test)]
mod battery_tests {
    use super::*;

    #[test]
    fn the_limit_is_written_and_full_once_is_temporary() {
        let base = std::env::temp_dir().join(format!("niructl-battery-{}", std::process::id()));
        let bat = base.join("BAT0");
        fs::create_dir_all(&bat).unwrap();
        fs::write(bat.join("charge_control_end_threshold"), "100\n").unwrap();
        fs::write(bat.join("charge_control_start_threshold"), "95\n").unwrap();
        fs::write(bat.join("capacity"), "64\n").unwrap();
        fs::create_dir_all(base.join("AC")).unwrap();
        std::env::set_var("NIRUCTL_POWER_SUPPLY", &base);
        let lines = battery_apply(80).unwrap();
        assert_eq!(lines, vec!["BAT0: charges to 80 %"]);
        assert_eq!(
            fs::read_to_string(bat.join("charge_control_end_threshold")).unwrap(),
            "80"
        );
        assert_eq!(
            fs::read_to_string(bat.join("charge_control_start_threshold")).unwrap(),
            "75"
        );
        battery_apply(100).unwrap();
        assert_eq!(
            fs::read_to_string(bat.join("charge_control_end_threshold")).unwrap(),
            "100"
        );
        fs::remove_file(bat.join("charge_control_end_threshold")).unwrap();
        fs::remove_file(bat.join("charge_control_start_threshold")).unwrap();
        assert!(battery_apply(80)
            .unwrap_err()
            .to_string()
            .contains("no charge limit"));
        std::env::remove_var("NIRUCTL_POWER_SUPPLY");
        let _ = fs::remove_dir_all(&base);
    }
}
