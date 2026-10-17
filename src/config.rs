use serde::Deserialize;
use std::{error::Error, fs, path::PathBuf};
pub type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub gap: u16,
    pub border_width: u16,
    pub bar_height: u16,
    pub master_ratio: f32,
    pub terminal: Vec<String>,
    pub launcher: Vec<String>,
    pub theme: String,
    pub font_path: Option<String>,
    pub font_size: f32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            gap: 8,
            border_width: 2,
            bar_height: 28,
            master_ratio: 0.55,
            terminal: vec!["kitty".into()],
            launcher: vec!["rofi".into(), "-show".into(), "drun".into()],
            theme: "niru-noir".into(),
            font_path: None,
            font_size: 15.0,
        }
    }
}
pub fn directory() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("niru")
}
impl Config {
    pub fn load() -> Result<Self> {
        let path = directory().join("config.toml");
        let c: Self = match fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        if !(8.0..=40.0).contains(&c.font_size)
            || !(0.1..=0.9).contains(&c.master_ratio)
            || c.gap > 100
            || c.border_width > 20
            || !(18..=100).contains(&c.bar_height)
            || c.terminal.is_empty()
            || c.launcher.is_empty()
        {
            return Err("Invalid geometry or empty command".into());
        }
        Ok(c)
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
    fn default_theme() {
        let t = Theme::load("niru-noir").unwrap();
        assert_eq!(Theme::pixel(&t.background).unwrap(), 0x0f0f10);
    }
    #[test]
    fn reject_path() {
        assert!(Theme::load("../bad").is_err());
    }
    #[test]
    fn reject_colour() {
        assert!(Theme::pixel("ffffff00").is_err());
    }
}
