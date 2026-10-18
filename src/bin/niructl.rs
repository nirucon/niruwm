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

use config::{Config, Result, Theme};
use x11rb::{
    connection::{Connection, RequestConnection as _},
    protocol::{randr::ConnectionExt as _, xproto::*},
    wrapper::ConnectionExt as _,
};

const USAGE: &str = "Usage: niructl status | tag N | movetotag N | layout NAME | layouts | \
theme NAME | themes | monitors | reload | quit";
const CMD_ATOM: &str = "_NIRUWM_COMMAND";
const CONFIG_SERIAL: &str = "_NIRU_CONFIG_SERIAL";

fn offline(cmd: &str) -> Result<()> {
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
    if cmd == "themes" || cmd == "layouts" {
        return offline(cmd);
    }
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
