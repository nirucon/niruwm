//! Wallpaper handling.
//!
//! Picks a picture from a directory and sets it through the first available
//! X11 tool. Rotation is driven by the WM's main loop, not a separate daemon,
//! so starting or stopping a session is enough to start or stop rotation.
//!
//! NIRUWM only owns the wallpaper when `[wallpaper] enabled = true`. When it
//! is off, NIRUPI's `wallrotate.sh` keeps doing whatever it does; running both
//! would make two programs fight over the root background.

use crate::config::Result;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Wallpaper tools in order of preference. `xsetroot` is the fallback: it
/// cannot draw an image, but it still sets the Niru Noir background.
/// Look up a program in PATH without spawning anything.
fn which(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(':')
        .filter(|p| !p.is_empty())
        .any(|dir| Path::new(dir).join(name).is_file())
}

const SETTERS: &[(&str, &[&str])] = &[
    ("feh", &["--bg-fill"]),
    ("hsetroot", &["-fill"]),
    ("nitrogen", &["--set-scaled"]),
    ("xwallpaper", &["--zoom"]),
];

pub struct Wallpaper {
    dir: PathBuf,
    mode: Mode,
    image: Option<PathBuf>,
    last_change: Instant,
    rotation: Duration,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Pick a different picture from the directory on every rotation.
    Random,
    /// Always use `file`.
    Single,
    /// Pick one picture and keep it.
    Fixed,
}

impl Wallpaper {
    pub fn new(cfg: &crate::config::WallpaperConfig) -> Self {
        let dir = expand_home(&cfg.dir);
        let mode = match cfg.mode.as_str() {
            "single" => Mode::Single,
            "fixed" => Mode::Fixed,
            _ => Mode::Random,
        };
        let image = if mode == Mode::Single && !cfg.file.is_empty() {
            Some(expand_home(&cfg.file))
        } else {
            None
        };
        Self {
            dir,
            mode,
            image,
            last_change: Instant::now(),
            rotation: Duration::from_secs(u64::from(cfg.rotation_minutes) * 60),
            enabled: cfg.enabled,
        }
    }

    /// Apply the wallpaper now and return which file was used.
    pub fn apply(&mut self) -> Result<Option<PathBuf>> {
        if !self.enabled {
            return Ok(None);
        }
        let chosen = match self.mode {
            Mode::Single => self.image.clone(),
            Mode::Fixed => match self.image.clone() {
                Some(p) => Some(p),
                None => {
                    let p = self.pick();
                    self.image = p.clone();
                    p
                }
            },
            Mode::Random => self.pick(),
        };
        if let Some(path) = &chosen {
            set_image(path);
        }
        self.last_change = Instant::now();
        Ok(chosen)
    }

    /// Called from the event loop; rotates when the interval has passed.
    ///
    /// Returns `true` when the wallpaper changed, so the caller can log it.
    pub fn tick(&mut self) -> bool {
        if !self.enabled || self.mode == Mode::Single || self.rotation.is_zero() {
            return false;
        }
        if self.last_change.elapsed() < self.rotation {
            return false;
        }
        // Fixed mode keeps its picture; only the timer is reset.
        if self.mode == Mode::Fixed && self.image.is_some() {
            self.last_change = Instant::now();
            return false;
        }
        self.image = None;
        let _ = self.apply();
        true
    }

    fn pick(&self) -> Option<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(&self.dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_image(p))
            .collect();
        if files.is_empty() {
            return None;
        }
        files.sort();
        // Deterministic pseudo-random choice: the process id and the current
        // picture count change between rotations, which is enough variety
        // without pulling in a random number generator.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as usize)
            .unwrap_or(0)
            ^ std::process::id() as usize;
        Some(files[seed % files.len()].clone())
    }
}

fn is_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "bmp")
    )
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(p)
}

/// Set an image with the first available tool, falling back to a solid colour.
fn set_image(path: &Path) {
    for (tool, args) in SETTERS {
        if !which(tool) {
            continue;
        }
        let mut cmd = vec![tool.to_string()];
        cmd.extend(args.iter().map(|s| s.to_string()));
        cmd.push(path.display().to_string());
        if std::process::Command::new(&cmd[0])
            .args(&cmd[1..])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return;
        }
    }
    // No image tool: keep a dark root rather than leave the previous picture.
    if which("xsetroot") {
        let _ = std::process::Command::new("xsetroot")
            .args(["-solid", "#0f0f10"])
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn tmpdir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("niruwm-wp-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn picks_only_images() {
        let dir = tmpdir("images");
        File::create(dir.join("a.png")).unwrap();
        File::create(dir.join("b.jpg")).unwrap();
        File::create(dir.join("notes.txt")).unwrap();
        let w = Wallpaper {
            dir: dir.clone(),
            mode: Mode::Random,
            image: None,
            last_change: Instant::now(),
            rotation: Duration::from_secs(600),
            enabled: true,
        };
        let picked = w.pick().expect("at least one image");
        assert!(picked.extension().is_some());
        assert_ne!(picked.extension().unwrap(), "txt");
    }

    #[test]
    fn empty_directory_yields_nothing() {
        let dir = tmpdir("empty");
        let w = Wallpaper {
            dir,
            mode: Mode::Random,
            image: None,
            last_change: Instant::now(),
            rotation: Duration::from_secs(600),
            enabled: true,
        };
        assert_eq!(w.pick(), None);
    }

    #[test]
    fn disabled_does_nothing() {
        let dir = tmpdir("off");
        File::create(dir.join("a.png")).unwrap();
        let mut w = Wallpaper {
            dir,
            mode: Mode::Random,
            image: None,
            last_change: Instant::now(),
            rotation: Duration::from_secs(600),
            enabled: false,
        };
        assert_eq!(w.apply().unwrap(), None);
        assert!(!w.tick());
    }

    #[test]
    fn rotation_respects_the_interval() {
        let dir = tmpdir("rotate");
        File::create(dir.join("a.png")).unwrap();
        let mut w = Wallpaper {
            dir,
            mode: Mode::Random,
            image: None,
            last_change: Instant::now(),
            rotation: Duration::from_secs(600),
            enabled: true,
        };
        assert!(!w.tick(), "must not rotate before the interval");
    }

    #[test]
    fn expand_home_prefix() {
        std::env::set_var("HOME", "/home/test");
        assert_eq!(
            expand_home("~/Pictures"),
            PathBuf::from("/home/test/Pictures")
        );
        assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
    }
}
