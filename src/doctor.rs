//! `niruwm --doctor`: report what this machine has and what a binding needs.
//!
//! A window manager that silently ignores `Super+Enter` because no terminal is
//! installed is confusing. This lists the programs the default bindings use,
//! says whether each is present, and names the package on Arch, Debian and
//! Void. It changes nothing.

use crate::config::{Config, Result};
use crate::keyboard;
use std::path::Path;

/// A program some binding or part of the session expects.
struct Need {
    name: &'static str,
    why: &'static str,
    required: bool,
    arch: &'static str,
    debian: &'static str,
    void: &'static str,
}

const NEEDS: &[Need] = &[
    Need {
        name: "kitty",
        why: "Super+Enter terminal",
        required: true,
        arch: "kitty",
        debian: "kitty",
        void: "kitty",
    },
    Need {
        name: "setxkbmap",
        why: "keyboard layout",
        required: true,
        arch: "xorg-setxkbmap",
        debian: "x11-xkb-utils",
        void: "setxkbmap",
    },
    Need {
        name: "xsetroot",
        why: "root background",
        required: false,
        arch: "xorg-xsetroot",
        debian: "x11-xserver-utils",
        void: "xsetroot",
    },
    Need {
        name: "xrdb",
        why: "Xresources for application colours",
        required: false,
        arch: "xorg-xrdb",
        debian: "x11-xserver-utils",
        void: "xrdb",
    },
    Need {
        name: "dunst",
        why: "notifications",
        required: false,
        arch: "dunst",
        debian: "dunst",
        void: "dunst",
    },
    Need {
        name: "picom",
        why: "compositor (shadows, transparency)",
        required: false,
        arch: "picom",
        debian: "picom",
        void: "picom",
    },
    Need {
        name: "maim",
        why: "maim fallback for screenshots",
        required: false,
        arch: "maim",
        debian: "maim",
        void: "maim",
    },
    Need {
        name: "feh",
        why: "wallpaper",
        required: false,
        arch: "feh",
        debian: "feh",
        void: "feh",
    },
    Need {
        name: "nirulock",
        why: "Super+Escape lock",
        required: false,
        arch: "nirulock (build from source)",
        debian: "nirulock (build from source)",
        void: "nirulock (build from source)",
    },
    Need {
        name: "xidlehook",
        why: "fallback idle timer; niruidle replaces it",
        required: false,
        arch: "xidlehook (optional)",
        debian: "xidlehook (optional)",
        void: "xidlehook (optional)",
    },
];

/// Which locker `niruidle` will start, in the order it looks.
///
/// Absolute paths first: a session started by a display manager does not
/// always have `~/.local/bin` in PATH, and a bare name then falls through to
/// `slock`, which does not speak the NIRU lock protocol.
fn lock_screen(home: &Path) -> String {
    let candidates = [
        home.join(".local/bin/nirulock"),
        Path::new("/usr/bin/slock").to_path_buf(),
        Path::new("/usr/local/bin/slock").to_path_buf(),
        Path::new("/usr/bin/swaylock").to_path_buf(),
    ];
    for c in candidates {
        if c.is_file() {
            return c.display().to_string();
        }
    }
    "none found: install nirulock".to_string()
}

/// Which distribution this looks like, chosen by its package manager.
fn distro() -> &'static str {
    if Path::new("/etc/arch-release").exists() || Path::new("/usr/bin/pacman").exists() {
        "arch"
    } else if Path::new("/etc/debian_version").exists() {
        "debian"
    } else if Path::new("/etc/void-release").exists() || Path::new("/usr/bin/xbps-install").exists()
    {
        "void"
    } else {
        "unknown"
    }
}

/// Look for a program in PATH, and also in `~/.local/bin`.
///
/// The session script prepends `~/.local/bin`, but `--doctor` is often run
/// from a shell that does not: without this fallback it reports programs as
/// missing when they are installed exactly where NIRU puts them.
fn in_path(name: &str) -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    let local_bin = Path::new(&home).join(".local/bin").join(name);
    if local_bin.is_file() {
        return true;
    }
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(':').filter(|p| !p.is_empty()).any(|dir| {
        let p = Path::new(dir).join(name);
        p.is_file()
    })
}

pub fn run() -> Result<()> {
    let cfg = Config::load()?;
    let home = std::env::var("HOME").unwrap_or_default();
    let os = distro();
    println!("NIRUWM {}", env!("CARGO_PKG_VERSION"));
    println!("distribution: {os}");
    println!(
        "session: {}",
        if std::env::var_os("DISPLAY").is_some() {
            "X11 running"
        } else {
            "no DISPLAY (start from SDDM or startx)"
        }
    );
    println!();

    // Configuration.
    let path = crate::config::directory().join("config.toml");
    println!(
        "config: {} {}",
        path.display(),
        if path.exists() {
            "(found)"
        } else {
            "(defaults)"
        }
    );
    println!("theme:  {} ({})", cfg.theme, cfg.terminal.join(" "));
    println!(
        "bindings: {} defined{}",
        cfg.effective_keys().len(),
        if cfg.keys.is_empty() {
            " (built-in)"
        } else {
            " (built-in plus your overrides)"
        }
    );

    // Which bindings this keyboard can actually deliver.
    //
    // A key that is bound but absent from the layout does nothing, silently.
    // That is how the media keys looked broken while the code was fine: the
    // binding existed but the keycode was never found.
    match keyboard::resolve(&cfg) {
        Some((present, missing)) => {
            println!("keyboard: {present} bindings usable on this layout");
            for name in &missing {
                println!("  not on this keyboard: {name}");
            }
        }
        None => println!("keyboard: no DISPLAY, cannot read the keymap"),
    }
    println!();

    // Programs.
    let mut missing_required = 0;
    let mut optional: Vec<&Need> = Vec::new();
    println!("programs:");
    for need in NEEDS {
        let present = in_path(need.name);
        let mark = if present { "ok     " } else { "missing" };
        if !present && need.required {
            missing_required += 1;
        }
        println!("  {mark}  {:<16} {}", need.name, need.why);
        if !present {
            optional.push(need);
        }
    }
    println!();

    if !optional.is_empty() {
        let pkg = |n: &Need| match os {
            "arch" => n.arch,
            "debian" => n.debian,
            "void" => n.void,
            _ => n.arch,
        };
        println!("to install the missing ones:");
        for need in &optional {
            println!("  {:<16} {}", need.name, pkg(need));
        }
        println!();
    }

    // Which lock screen the idle timer will start. A wrong choice here is
    // invisible until the screen locks and the password will not work, so it
    // is worth reporting next to everything else.
    println!();
    println!("lock screen: {}", lock_screen(Path::new(&home)));

    if missing_required > 0 {
        println!();
        println!(
            "{missing_required} required program(s) missing: the window manager runs, but some default bindings will do nothing."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distro_is_known_or_unknown() {
        let d = distro();
        assert!(
            ["arch", "debian", "void", "unknown"].contains(&d),
            "unexpected distro label: {d}"
        );
    }

    #[test]
    fn every_need_has_a_package_per_distro() {
        for n in NEEDS {
            assert!(!n.arch.is_empty(), "{}", n.name);
            assert!(!n.debian.is_empty(), "{}", n.name);
            assert!(!n.void.is_empty(), "{}", n.name);
        }
    }

    #[test]
    fn sh_is_found_in_path() {
        // The test environment always has a shell somewhere in PATH.
        assert!(in_path("sh"));
    }
}
