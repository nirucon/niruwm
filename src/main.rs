mod bindings;
mod config;
mod doctor;
mod layout;
mod monitor;
mod wallpaper;
mod wm;
use config::{Config, Theme};
use wm::Wm;
fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg == "--version" {
        println!("NIRUWM {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if arg == "--check-config" {
        match Config::load().and_then(|c| Theme::load(&c.theme)) {
            Ok(t) => {
                println!("Configuration OK · {}", t.name);
                return;
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1)
            }
        }
    }
    if arg == "--dump-config" {
        match config::dump() {
            Ok(text) => print!("{text}"),
            Err(e) => {
                eprintln!("niruwm: {e}");
                std::process::exit(1)
            }
        }
        return;
    }
    if arg == "--doctor" {
        if let Err(e) = doctor::run() {
            eprintln!("niruwm: {e}");
            std::process::exit(1)
        }
        return;
    }
    if !arg.is_empty() {
        eprintln!("Usage: niruwm [--version|--check-config|--doctor|--dump-config]");
        std::process::exit(2)
    }
    if let Err(e) = Wm::new().and_then(|mut w| w.run()) {
        eprintln!("niruwm: {e}");
        std::process::exit(1)
    }
}
