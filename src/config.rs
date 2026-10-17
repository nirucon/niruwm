//! Configuration shared by NIRUWM and NIRUBAR.
//!
//! The file lives in `~/.config/niruwm/config.toml` (or `$XDG_CONFIG_HOME`).
//! Both programs read it; unknown keys are ignored so that installing NIRUBAR
//! before or after NIRUWM never rejects the other program's settings.
//!
//! The old 0.1 path `~/.config/niru/config.toml` is still read when the new
//! file does not exist, so an existing alpha configuration keeps working.
use serde::Deserialize;
use std::{error::Error, fs, path::PathBuf};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: String,
    pub gap: u16,
    pub border_width: u16,
    pub bar_height: u16,
    pub master_ratio: f32,
    pub terminal: Vec<String>,
    /// Optional second terminal (`Super+Shift+Enter`), NIRUPI uses alacritty.
    pub terminal_alt: Vec<String>,
    pub browser: Vec<String>,
    pub launcher: Vec<String>,
    pub font_path: Option<String>,
    pub font_size: f32,
    pub bar: Bar,
    /// Optional bindings. When empty, the built-in NIRUPI-compatible table is
    /// used. Entries replace a default binding with the same keys.
    pub keys: Vec<crate::bindings::RawBinding>,
    pub wallpaper: WallpaperConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WallpaperConfig {
    /// NIRUWM sets and rotates the wallpaper only when this is true.
    /// Leave it false when NIRUPI's wallrotate.sh should keep managing it.
    pub enabled: bool,
    /// Directory to pick pictures from.
    pub dir: String,
    /// `random` (new picture each rotation), `fixed` (one picture, kept),
    /// or `single` (always `file`).
    pub mode: String,
    /// Used by `mode = "single"`.
    pub file: String,
    /// Minutes between changes. 0 disables rotation.
    pub rotation_minutes: u32,
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: "~/Pictures/Wallpapers".into(),
            mode: "random".into(),
            file: String::new(),
            rotation_minutes: 10,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bar {
    /// `true` renders the bar as a dock window (NIRUWM only). Ignored by
    /// standalone use.
    pub enabled: bool,
    /// Show a short hint of the focused window's title between the tags and
    /// the clock. `false` keeps the bar minimal: tags and clock only.
    pub title: bool,
    /// Maximum number of characters kept from the window title.
    pub title_max: usize,
    /// Width reserved per workspace cell, in pixels.
    pub cell_width: u16,
    /// Separator drawn between the status groups.
    pub separator: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "niru-noir".into(),
            gap: 8,
            border_width: 2,
            bar_height: 28,
            master_ratio: 0.55,
            terminal: vec!["kitty".into()],
            terminal_alt: Vec::new(),
            browser: vec!["brave".into()],
            launcher: vec!["rofi".into(), "-show".into(), "drun".into()],
            font_path: None,
            font_size: 15.0,
            bar: Bar::default(),
            keys: Vec::new(),
            wallpaper: WallpaperConfig::default(),
        }
    }
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            enabled: true,
            title: false,
            title_max: 60,
            cell_width: 30,
            separator: "|".into(),
        }
    }
}

/// The directory that holds `config.toml` and `themes/`.
pub fn directory() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("niruwm")
}

fn legacy_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("niru")
        .join("config.toml")
}

impl Config {
    pub fn load() -> Result<Self> {
        let primary = directory().join("config.toml");
        let text = match fs::read_to_string(&primary) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match fs::read_to_string(legacy_path()) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(Self::default())
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) => return Err(e.into()),
        };
        let c: Self = toml::from_str(&text)?;
        c.validate()?;
        Ok(c)
    }

    /// Final binding table: the defaults with any user entry replacing the
    /// binding that uses the same keys.
    #[allow(dead_code)] // used by the WM binary, shared with niructl
    pub fn effective_keys(&self) -> Vec<crate::bindings::RawBinding> {
        use crate::bindings::{parse_keys, RawBinding};
        let mut table = crate::bindings::Action::defaults();
        for user in &self.keys {
            match parse_keys(&user.keys) {
                Ok((mods, key)) => {
                    let same =
                        |k: &str| matches!(parse_keys(k), Ok((m, kk)) if m == mods && kk == key);
                    table.retain(|b| !same(&b.keys));
                    table.push(RawBinding {
                        keys: user.keys.clone(),
                        action: user.action.clone(),
                    });
                }
                Err(e) => eprintln!("niruwm: ignoring binding {:?}: {e}", user.keys),
            }
        }
        table
    }

    fn validate(&self) -> Result<()> {
        if !(8.0..=40.0).contains(&self.font_size)
            || !(0.1..=0.9).contains(&self.master_ratio)
            || self.gap > 100
            || self.border_width > 20
            || !(18..=100).contains(&self.bar_height)
            || self.terminal.is_empty()
            || self.launcher.is_empty()
        {
            return Err("Invalid geometry or empty command".into());
        }
        if self.bar.cell_width < 12 || self.bar.cell_width > 200 {
            return Err("bar.cell_width must be between 12 and 200".into());
        }
        if self.bar.title_max > 512 {
            return Err("bar.title_max must be at most 512".into());
        }
        if !["random", "fixed", "single"].contains(&self.wallpaper.mode.as_str()) {
            return Err("wallpaper.mode must be random, fixed or single".into());
        }
        if self.wallpaper.rotation_minutes > 100_000 {
            return Err("wallpaper.rotation_minutes is too large".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub name: String,
    pub background: String,
    pub foreground: String,
    pub muted: String,
    pub border: String,
    pub focus: String,
}

impl Theme {
    pub fn load(name: &str) -> Result<Self> {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("Invalid theme name".into());
        }
        let text = if name == "niru-noir" {
            include_str!("../themes/niru-noir.toml").to_string()
        } else {
            fs::read_to_string(directory().join("themes").join(format!("{name}.toml")))?
        };
        let theme: Self = toml::from_str(&text)?;
        for s in [
            &theme.background,
            &theme.foreground,
            &theme.muted,
            &theme.border,
            &theme.focus,
        ] {
            Self::pixel(s)?;
        }
        Ok(theme)
    }

    pub fn pixel(s: &str) -> Result<u32> {
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Theme colours must be six hex digits".into());
        }
        Ok(u32::from_str_radix(s, 16)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_minimal() {
        let c = Config::default();
        assert!(c.bar.enabled);
        assert!(!c.bar.title, "the bar defaults to tags and clock only");
        assert_eq!(c.bar.cell_width, 30);
    }

    #[test]
    fn accepts_the_bar_config_unchanged() {
        // NIRUBAR and NIRUWM share one file: a key the bar writes must not
        // stop the window manager from starting.
        let shared = r#"
theme = "niru-noir"
terminal = ["kitty"]

[bar]
title = true
cell_width = 34
"#;
        let c: Config = toml::from_str(shared).expect("shared config must parse");
        assert!(c.bar.title);
        assert_eq!(c.bar.cell_width, 34);
        assert_eq!(c.terminal, vec!["kitty"]);
    }

    #[test]
    fn partial_config_falls_back_to_defaults() {
        let c: Config = toml::from_str("[bar]\ntitle = true\n").unwrap();
        assert!(c.bar.title);
        assert_eq!(c.bar.cell_width, 30);
        assert_eq!(c.gap, 8);
    }

    #[test]
    fn reject_bad_theme_name() {
        assert!(Theme::load("../bad").is_err());
    }

    #[test]
    fn reject_bad_cell_width() {
        let mut c = Config::default();
        c.bar.cell_width = 4;
        assert!(c.validate().is_err());
    }
}
