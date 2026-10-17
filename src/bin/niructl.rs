#[path = "../config.rs"]
mod config;
use config::{Config, Result, Theme};
use x11rb::{connection::Connection, protocol::xproto::*, wrapper::ConnectionExt as _};
fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let usage =
        "Usage: niructl status | tag 1..9 | layout tile|left|monocle|centered|fibonacci|floating | reload | theme NAME | quit";
    let Some(cmd) = args.first() else {
        return Err(usage.into());
    };
    let (c, s) = x11rb::connect(None)?;
    let root = c.setup().roots[s].root;
    let atom = |n: &str| -> Result<Atom> { Ok(c.intern_atom(false, n.as_bytes())?.reply()?.atom) };
    if cmd == "status" {
        let a = atom("_NET_CURRENT_DESKTOP")?;
        let p = c
            .get_property(false, root, a, AtomEnum::CARDINAL, 0, 1)?
            .reply()?;
        let tag = p.value32().and_then(|mut v| v.next()).unwrap_or(0);
        println!(
            "tag={} theme={}",
            tag + 1,
            Theme::load(&Config::load()?.theme)?.name
        );
        return Ok(());
    }
    let (action, value) = match cmd.as_str() {
        "tag" => {
            let n: u32 = args.get(1).ok_or(usage)?.parse()?;
            if !(1..=9).contains(&n) {
                return Err(usage.into());
            }
            (1, n - 1)
        }
        "layout" => (
            2,
            match args.get(1).map(String::as_str) {
                Some("tile") => 0,
                Some("monocle") => 1,
                Some("left") => 2,
                Some("centered") => 3,
                Some("fibonacci") => 4,
                Some("floating") => 5,
                _ => return Err(usage.into()),
            },
        ),
        "reload" => (3, 0),
        "quit" => (4, 0),
        "theme" => {
            let name = args.get(1).ok_or(usage)?;
            Theme::load(name)?;
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
                .ok_or("Invalid config")?
                .insert("theme".into(), toml::Value::String(name.clone()));
            let temp = dir.join(format!("config.toml.{}.tmp", std::process::id()));
            std::fs::write(&temp, toml::to_string_pretty(&value)?)?;
            std::fs::rename(temp, path)?;
            (3, 0)
        }
        _ => return Err(usage.into()),
    };
    c.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT,
        ClientMessageEvent::new(32, root, atom("_NIRUWM_COMMAND")?, [action, value, 0, 0, 0]),
    )?;
    c.change_property32(
        PropMode::REPLACE,
        root,
        atom("_NIRU_CONFIG_SERIAL")?,
        AtomEnum::CARDINAL,
        &[std::process::id()],
    )?;
    c.flush()?;
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("niructl: {e}");
        std::process::exit(1)
    }
}
