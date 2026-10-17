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

use config::{Config, Result, Theme};
use x11rb::{connection::Connection, protocol::xproto::*, wrapper::ConnectionExt as _};

const USAGE: &str =
    "Usage: niructl status | tag N | movetotag N | layout NAME | reload | theme NAME | quit";
const CMD_ATOM: &str = "_NIRUWM_COMMAND";
const CONFIG_SERIAL: &str = "_NIRU_CONFIG_SERIAL";

fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(cmd) = args.first() else {
        return Err(USAGE.into());
    };
    let (c, s) = x11rb::connect(None)?;
    let root = c.setup().roots[s].root;
    let atom = |n: &str| -> Result<Atom> { Ok(c.intern_atom(false, n.as_bytes())?.reply()?.atom) };

    if cmd == "status" {
        let tag = property_u32(&c, root, atom("_NET_CURRENT_DESKTOP")?).unwrap_or(0);
        let wm = property_u32(&c, root, atom("_NET_SUPPORTING_WM_CHECK")?).is_some();
        println!(
            "wm={} tag={} theme={}",
            if wm { "niruwm" } else { "none" },
            tag + 1,
            Theme::load(&Config::load()?.theme)?.name
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
                Some("fibonacci") => 4,
                Some("floating") => 5,
                _ => return Err(USAGE.into()),
            },
        ),
        "reload" => (4, 0),
        "quit" => (5, 0),
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
        // Bump the serial so BOTH the WM and the bar reload their settings.
        send(&c, root, atom(CMD_ATOM)?, [action, value, 0, 0, 0])?;
        let serial = atom(CONFIG_SERIAL)?;
        c.change_property32(
            PropMode::REPLACE,
            root,
            serial,
            AtomEnum::CARDINAL,
            &[std::process::id()],
        )?;
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
        // Layout, reload and theme have no single readable property; a short
        // pause is enough for them to take effect.
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

fn parse_tag(args: &[String]) -> Result<u32> {
    let n: u32 = args.get(1).ok_or(USAGE)?.parse()?;
    if !(1..=9).contains(&n) {
        return Err("tag must be 1..9".into());
    }
    Ok(n - 1)
}

fn send(
    c: &x11rb::rust_connection::RustConnection,
    root: Window,
    atom: Atom,
    data: [u32; 5],
) -> Result<()> {
    // EWMH: clients send ClientMessage to the root window with
    // SubstructureRedirectMask | SubstructureNotifyMask, the manager selects
    // SubstructureRedirect on the root, and X delivers it once.
    c.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ClientMessageEvent::new(32, root, atom, data),
    )?;
    Ok(())
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
