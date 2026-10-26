//! Configuration shared by NIRUWM and NIRUBAR.
//!
//! The file lives in `~/.config/niruwm/config.toml` (or `$XDG_CONFIG_HOME`).
//! Both programs read it; unknown keys are ignored so that installing NIRUBAR
//! before or after NIRUWM never rejects the other program's settings.
//!
//! The old 0.1 path `~/.config/niru/config.toml` is still read when the new
//! file does not exist, so an existing alpha configuration keeps working.
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn parse_clock(s: &str) -> Option<(u32, u32)> {
    let (hour, minute) = s.trim().split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub theme: String,
    pub gap: u16,
    pub border_width: u16,
    pub bar_height: u16,
    pub master_ratio: f32,
    pub terminal: Vec<String>,
    /// Optional second terminal (`Super+Shift+Enter`).
    pub terminal_alt: Vec<String>,
    pub browser: Vec<String>,
    pub launcher: Vec<String>,
    pub font_path: Option<String>,
    pub font_size: f32,
    pub bar: Bar,
    /// Optional bindings. When empty, the built-in table is used. Entries
    /// replace a default binding with the same keys.
    pub keys: Vec<crate::bindings::RawBinding>,
    pub wallpaper: WallpaperConfig,
    /// Optional applications to open on named workspaces after login.
    pub autostart: AutostartConfig,
    /// Launcher-invoked workspace recipes. Empty means no recipes.
    pub recipes: Vec<Recipe>,
    pub monitors: MonitorsConfig,
    pub scratchpad: ScratchpadConfig,
    /// Window rules, applied when a window is first managed.
    pub rules: Vec<Rule>,
    /// Manual night-light settings shared by `niructl` and the WM.
    pub nightlight: NightlightConfig,
    /// Screensaver and lock timing. Read by NIRUIDLE, accepted here so the
    /// shared configuration file validates as a whole.
    #[serde(default)]
    pub idle: IdleConfig,
    pub brightness: BrightnessConfig,
    /// Settings NIRUBAR reads from the same file. It refuses unknown fields as
    /// strictly as this program does, so every setting either program knows
    /// has to be accepted here, or the shared file is a config the other one
    /// refuses: 0.14.0 shipped a bar setting this program did not know, and
    /// the whole session failed to start. `{bar}` marks fields that belong to
    /// the bar alone; the window manager ignores them.
    pub weather_latitude: f64,
    pub weather_longitude: f64,
    /// Start the programs that were open when the last session ended.
    /// Read by NIRUWM only; NIRUBAR accepts it because the file is shared.
    pub restore_session: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScratchpadConfig {
    /// Command that provides the scratchpad window.
    pub command: Vec<String>,
    /// Size as a share of the screen.
    pub width: f32,
    pub height: f32,
}

impl ScratchpadConfig {
    /// True when a window with this `WM_CLASS` is the scratchpad.
    ///
    /// Only the value of `--class` (or `-class`) counts. A command without one
    /// never matches, so ordinary windows of the same program stay tiled.
    pub fn matches_class(&self, wm_class: &str) -> bool {
        let mut args = self.command.iter().peekable();
        while let Some(arg) = args.next() {
            let is_class_flag = matches!(
                arg.as_str(),
                "--class" | "-class" | "--app-id" | "-name" | "--name"
            );
            if is_class_flag {
                if let Some(name) = args.peek() {
                    let lower = wm_class.to_ascii_lowercase();
                    let needle = name.to_ascii_lowercase();
                    if !needle.is_empty() && lower.contains(&needle) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

impl Default for ScratchpadConfig {
    fn default() -> Self {
        Self {
            command: vec!["kitty".into(), "--class".into(), "scratchpad".into()],
            width: 0.6,
            height: 0.6,
        }
    }
}

/// One window rule.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Substring matched against the window class or instance name.
    pub class: String,
    /// Workspace index (0-based) to open on. Omitted means "current".
    #[allow(dead_code)] // read by niruwm, not by niructl
    pub workspace: Option<u32>,
    /// Open floating instead of tiled.
    #[allow(dead_code)]
    pub floating: Option<bool>,
    /// Center a floating window on its monitor when it first maps.
    #[allow(dead_code)]
    pub center: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NightlightConfig {
    /// Degrees, north positive. 0,0 means "not configured" for solar use.
    pub latitude: f64,
    /// Degrees, east positive.
    pub longitude: f64,
    /// Kelvin applied when night light is enabled.
    pub temperature: u32,
    /// `manual`, `fixed` or `solar`.
    pub mode: String,
    /// Fixed schedule start, local time `HH:MM`.
    pub on: String,
    /// Fixed schedule end, local time `HH:MM`.
    pub off: String,
    /// Neutral daytime temperature for solar mode.
    pub day_temperature: u32,
}

/// Screensaver and lock timing, stored in `[idle]`.
///
/// NIRUIDLE owns the timers; NIRUWM only accepts the section so the shared
/// file stays valid. The names in the file are `screensaver_timeout` and
/// `lock_after_screensaver`; the older `*_minutes` spellings are accepted as
/// aliases so an existing file keeps working.
#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdleConfig {
    /// Minutes of inactivity before the screensaver starts.
    #[serde(alias = "screensaver_timeout_minutes", alias = "idle_minutes")]
    pub screensaver_timeout: u32,
    /// Minutes the screensaver stays up before the session locks. 0 locks at
    /// the same moment as the screensaver.
    #[serde(alias = "lock_after_screensaver_minutes", alias = "lock_after_minutes")]
    pub lock_after_screensaver: u32,
    /// Minutes after locking before the display is switched off.
    #[serde(alias = "dpms_off_minutes")]
    pub dpms_off_after_lock: u32,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            screensaver_timeout: 15,
            lock_after_screensaver: 15,
            dpms_off_after_lock: 15,
        }
    }
}

impl IdleConfig {
    /// Largest accepted value. A day is already an unreachable idle screen;
    /// anything longer is a typo, and a wrapped or huge timer would leave the
    /// session either unlocked forever or locked immediately.
    pub const MAX_MINUTES: u32 = 24 * 60;

    /// Read `[idle]` from the shared config file, clamped to safe values.
    /// Returns `None` when the file is absent, unreadable or has no section,
    /// so a broken file never changes the timers.
    #[allow(dead_code)] // called by niruidle; the other binaries accept the section
    pub fn defaults_from_shared_file() -> Option<Self> {
        let text = std::fs::read_to_string(directory().join("config.toml")).ok()?;
        let value: toml::Value = text.parse().ok()?;
        let idle = value.get("idle")?;
        let parsed: Self = idle.clone().try_into().ok()?;
        Some(parsed.safe())
    }

    /// The configured values with out-of-range entries replaced by defaults,
    /// so one bad number cannot leave the screen unlocked indefinitely.
    #[allow(dead_code)] // used by niruidle
    pub fn safe(&self) -> Self {
        Self {
            screensaver_timeout: clamp_minutes(self.screensaver_timeout, 15),
            lock_after_screensaver: self.lock_after_screensaver.min(Self::MAX_MINUTES),
            dpms_off_after_lock: self.dpms_off_after_lock.min(Self::MAX_MINUTES),
        }
    }

    /// Validate after parsing. Only the screensaver timeout must be positive:
    /// a zero would mean "never", which is the one value that silently leaves
    /// a machine unlocked.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.screensaver_timeout == 0 {
            return Err("idle.screensaver_timeout must be at least 1 minute".into());
        }
        if self.screensaver_timeout > Self::MAX_MINUTES {
            return Err(format!(
                "idle.screensaver_timeout must be at most {} minutes",
                Self::MAX_MINUTES
            ));
        }
        if self.lock_after_screensaver > Self::MAX_MINUTES {
            return Err(format!(
                "idle.lock_after_screensaver must be at most {} minutes",
                Self::MAX_MINUTES
            ));
        }
        Ok(())
    }
}

#[allow(dead_code)] // used by niruidle's safe()
fn clamp_minutes(value: u32, fallback: u32) -> u32 {
    if (1..=IdleConfig::MAX_MINUTES).contains(&value) {
        value
    } else {
        fallback
    }
}

impl Default for NightlightConfig {
    fn default() -> Self {
        Self {
            latitude: 0.0,
            longitude: 0.0,
            temperature: 3400,
            mode: "manual".into(),
            on: "20:00".into(),
            off: "07:00".into(),
            day_temperature: 6500,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrightnessConfig {
    /// brightnessctl device name or wildcard. Empty uses the default.
    pub device: String,
    /// Percentage step for `niructl brightness up/down`.
    pub step: u32,
}

impl Default for BrightnessConfig {
    fn default() -> Self {
        Self {
            device: String::new(),
            step: 5,
        }
    }
}

/// Every layout name a workspace can start with.
pub const VALID_LAYOUTS: [&str; 7] = [
    "tile",
    "left",
    "monocle",
    "centered",
    "centeredfloat",
    "floating",
    "fibonacci",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MonitorsConfig {
    /// Workspaces per screen when a screen has no profile below.
    pub workspaces: usize,
    /// Per-screen overrides, matched by RandR output name.
    pub profiles: Vec<MonitorProfile>,
    /// Which layout each workspace starts with, in workspace order. An empty
    /// entry keeps the default (tile). The list may be shorter than the
    /// workspace count; those workspaces stay tiled.
    #[serde(default)]
    pub layouts: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorProfile {
    /// RandR output name, for example `eDP-1` or `HDMI-1`.
    pub name: String,
    /// Workspace labels this screen shows instead of 1..10.
    pub workspaces: Vec<String>,
    /// When true, this screen receives focus at login and opens new windows
    /// when the pointer is outside every screen.
    #[allow(dead_code)] // read by niruwm, not by niructl
    pub primary: bool,
    /// This screen's workspace layouts, in workspace order. Overrides the
    /// common `monitors.layouts` for the workspaces it lists.
    #[serde(default)]
    pub layouts: Vec<String>,
}

impl Default for MonitorsConfig {
    fn default() -> Self {
        Self {
            workspaces: 10,
            profiles: Vec::new(),
            layouts: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WallpaperConfig {
    /// NIRUWM sets and rotates the wallpaper only when this is true.
    /// Leave it false when another program should keep managing it.
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
    /// How the picture fills the screen: `fill` (crop), `fit` (letterbox),
    /// `stretch` or `center` (no scaling).
    pub scale: String,
    /// A specific list of files to rotate through. Empty means every image in
    /// `dir`. Paths may use ~.
    pub files: Vec<String>,
    /// Pick a new picture every time the session starts, even in `fixed` mode.
    pub shuffle_on_start: bool,
    /// When the theme changes, use that theme's wallpaper from `theme_dir`.
    /// Custom `single` wallpapers are never overridden.
    pub follow_theme: bool,
    /// Directory holding NIRUX-installed theme wallpapers. A custom theme
    /// `foo` may provide `nirux-wallpaper-foo-4k.png` or `-8k.png`.
    pub theme_dir: String,
    /// Pictures for one named screen. A screen without an entry inherits the
    /// common settings above. Entries survive a screen being unplugged.
    pub outputs: Vec<OutputWallpaper>,
}

impl Default for WallpaperConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: "~/Pictures/Wallpapers".into(),
            mode: "random".into(),
            file: String::new(),
            rotation_minutes: 10,
            scale: "fill".into(),
            files: Vec::new(),
            shuffle_on_start: true,
            follow_theme: true,
            theme_dir: "~/Pictures/Wallpapers/NIRU".into(),
            outputs: Vec::new(),
        }
    }
}

/// One screen's own picture, overriding the common wallpaper settings.
///
/// The screen is named by its RandR output (`eDP-1`, `DP-2`), never by its
/// position in a list, so a cable moved to another port is a different screen
/// and keeps the old entry for when it returns to the original port.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OutputWallpaper {
    /// RandR output name, matched exactly.
    pub output: String,
    /// `inherit` (the default), `theme` (always this theme's own picture),
    /// or `single` (this file, and only this file).
    pub mode: String,
    /// Image used by `mode = "single"`; empty means "pick from `dir`".
    pub file: String,
    /// `fill`, `fit`, `stretch` or `center`; empty inherits the common scale.
    pub scale: String,
}

impl Default for OutputWallpaper {
    fn default() -> Self {
        Self {
            output: String::new(),
            mode: "inherit".into(),
            file: String::new(),
            scale: String::new(),
        }
    }
}

impl OutputWallpaper {
    /// True when this entry changes nothing and may be removed.
    pub fn is_inherit(&self) -> bool {
        self.mode != "theme" && self.file.trim().is_empty()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutostartConfig {
    /// Run the entries at login. Empty `apps` means nothing starts.
    pub enabled: bool,
    /// Seconds to wait after each workspace group before switching to the next.
    pub settle_seconds: u32,
    /// How long to wait for a launched program's window before moving on.
    pub window_timeout_ms: u64,
    /// Workspace to return to when the sequence finishes. 0 leaves the last one.
    pub return_workspace: u32,
    /// Applications to start, grouped by 1-based workspace.
    pub apps: Vec<AutostartApp>,
}

impl Default for AutostartConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            settle_seconds: 2,
            window_timeout_ms: 5000,
            return_workspace: 1,
            apps: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutostartApp {
    /// 1-based workspace number.
    pub workspace: u32,
    /// Optional substring matched against WM_CLASS instance/class. Without it,
    /// autostart keeps the legacy fixed spacing and does not wait for a window.
    #[allow(dead_code)] // read by niructl, not by the WM binary
    #[serde(default)]
    pub class: Option<String>,
    /// Argument list, never passed through a shell.
    pub command: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub name: String,
    #[serde(default)]
    pub apps: Vec<AutostartApp>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bar {
    /// `true` renders the bar as a dock window (NIRUWM only). Ignored by
    /// standalone use.
    pub enabled: bool,
    /// Show a short hint of the focused window's title between the tags and
    /// the clock. `false` keeps the bar minimal: tags and clock only.
    /// Written as `title` in older configs; `show_title` is preferred.
    #[serde(alias = "title")]
    pub show_title: bool,
    /// Maximum number of characters kept from the window title.
    pub title_max: usize,
    /// Width reserved per workspace cell, in pixels.
    pub cell_width: u16,
    /// Separator drawn between the status groups.
    pub separator: String,
    /// Modules to show, in order: workspaces, network, battery, clock.
    pub modules: Vec<String>,
    /// Visibility of the optional microphone module. Read by NIRUBAR.
    pub microphone_visibility: String,
    /// Workspace labels shown instead of 1..10.
    #[serde(default = "default_tags")]
    #[allow(dead_code)] // read by nirubar; accepted here so the shared file loads
    pub tags: Vec<String>,
    /// Preset for the workspace labels: 1 = numbers, 2 = icons, 3 = text.
    /// When set, it selects the labels and `tags` is ignored; an absent or
    /// unknown value keeps `tags`, which defaults to numbers.
    #[serde(default)]
    #[allow(dead_code)] // read by nirubar
    pub workspace_layout: Option<u32>,
    /// Show the date, the time, or both.
    pub show_date: bool,
    pub show_time: bool,
    pub date_format: String,
    pub time_format: String,
    /// Open a detail panel when a module is clicked.
    pub popups: bool,
    /// Characters kept from the media title.
    pub media_width: usize,
    /// Minutes between package update checks.
    pub update_check_minutes: u32,
    /// Let the bar own a system tray.
    pub tray: bool,
    /// Seconds before an open panel closes itself. 0 keeps it until dismissed.
    pub popup_timeout_seconds: u32,
    /// Battery segment in the bar. Read by NIRUBAR; declared here so the
    /// window manager accepts the shared file.
    pub battery: bool,
    /// Network icon in the bar. Read by NIRUBAR.
    pub network: bool,
    /// Nerd Font glyphs in the bar. Read by NIRUBAR.
    pub icons: bool,
    /// Seconds before the focused window's title is shown. Read by NIRUBAR.
    pub title_delay_seconds: u32,
    /// Width of the `pulse` graph in the bar's panel. Read by NIRUBAR.
    pub graph_width: usize,
    /// Draw the bar see-through through the compositor. Read by NIRUBAR.
    pub transparent: bool,
    /// Bar opacity in percent when `transparent` is true. Read by NIRUBAR.
    pub opacity: u32,
    /// Bar settings a newer NIRUBAR could add. Accepted, so updating one
    /// program never stops the other from starting.
    #[serde(flatten)]
    #[allow(dead_code)] // deliberately unread; accepting it is the point
    pub extra: std::collections::HashMap<String, toml::Value>,
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
            launcher: vec!["nirulauncher".into(), "--apps".into()],
            font_path: None,
            font_size: 15.0,
            bar: Bar::default(),
            keys: Vec::new(),
            wallpaper: WallpaperConfig::default(),
            autostart: AutostartConfig::default(),
            recipes: Vec::new(),
            monitors: MonitorsConfig::default(),
            nightlight: NightlightConfig::default(),
            idle: IdleConfig::default(),
            brightness: BrightnessConfig::default(),
            scratchpad: ScratchpadConfig::default(),
            rules: Vec::new(),
            weather_latitude: 0.0,
            weather_longitude: 0.0,
            restore_session: false,
        }
    }
}

/// The default workspace labels: workspace 1 shows "1", and so on.
fn default_tags() -> Vec<String> {
    (1..=10).map(|n| n.to_string()).collect()
}

/// Workspace label presets, selected by `[bar] workspace_layout`.
///
/// Only the presentation changes; the workspace count and the Super+1..0
/// shortcuts are the same in every preset. Private-use Nerd Font glyphs render
/// as text when the font lacks them, which is why the icon preset is not the
/// default.
#[allow(dead_code)] // the table lives in nirubar; accepted here for validation
pub const WORKSPACE_LAYOUTS: u32 = 3;

/// Labels for a preset. `1` is numbers, `2` icons, `3` text.
#[allow(dead_code)]
pub fn workspace_layout_tags(layout: u32) -> Vec<String> {
    match layout {
        2 => [
            "\u{f0ac}", // globe: web
            "\u{f121}", // code: agent
            "\u{f086}", // comments: agent
            "\u{e795}", // terminal
            "\u{f075}", // comment: chat
            "\u{f0e0}", // envelope: mail
            "\u{f07b}", // folder
            "\u{f086}", // comments: chat
            "\u{f0e0}", // envelope: mail
            "\u{f07b}", // folder
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        3 => [
            "web", "ai", "ai", "ai", "term", "chat", "mail", "folder", "pchat", "term",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        _ => default_tags(),
    }
}

impl Bar {
    /// Labels to render. `workspace_layout` wins when it names a known preset;
    /// otherwise the explicit `tags` list is used, which defaults to numbers.
    #[allow(dead_code)] // read by nirubar
    pub fn effective_tags(&self) -> Vec<String> {
        match self.workspace_layout {
            Some(n) if (1..=WORKSPACE_LAYOUTS).contains(&n) => workspace_layout_tags(n),
            _ => self.tags.clone(),
        }
    }
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            enabled: true,
            show_title: false,
            title_max: 60,
            cell_width: 30,
            separator: "|".into(),
            modules: vec![
                "workspaces".into(),
                "layout".into(),
                "media".into(),
                "volume".into(),
                "brightness".into(),
                "display".into(),
                "network".into(),
                "mode".into(),
                "battery".into(),
                "niruxupdate".into(),
                "clock".into(),
            ],
            tags: default_tags(),
            workspace_layout: None,
            microphone_visibility: "always".into(),
            show_date: true,
            show_time: true,
            date_format: "%Y-%m-%d".into(),
            time_format: "%H:%M".into(),
            popups: true,
            media_width: 32,
            update_check_minutes: 30,
            tray: true,
            popup_timeout_seconds: 4,
            battery: true,
            network: true,
            icons: true,
            title_delay_seconds: 0,
            graph_width: 24,
            transparent: true,
            opacity: 94,
            extra: std::collections::HashMap::new(),
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
        let text = match fs::read_to_string(primary) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match fs::read_to_string(legacy_path()) {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        let value = toml::Value::Table(Default::default());
                        let mut c: Self = value.clone().try_into()?;
                        apply_appearance_defaults(&mut c, &value);
                        c.validate()?;
                        return Ok(c);
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Err(e) => return Err(e.into()),
        };
        let value: toml::Value = text.parse()?;
        let mut c: Self = value.clone().try_into()?;
        apply_appearance_defaults(&mut c, &value);
        c.validate()?;
        // A missing section silently selects the built-in default. For
        // `[wallpaper]` that default is "disabled", which stops the theme
        // wallpaper from ever being set. Say so instead of doing nothing.
        if value.get("wallpaper").is_none() {
            eprintln!(
                "niruwm: no [wallpaper] section in {}; wallpapers are disabled. \
                 Add it with `niructl theme <name>` or `nirux repair-wallpaper`.",
                directory().join("config.toml").display()
            );
        }
        Ok(c)
    }

    /// Final binding table: the defaults with any user entry replacing the
    /// binding that uses the same keys.
    #[allow(dead_code)] // used by the WM binary, shared with niructl
    pub fn effective_keys(&self) -> Vec<crate::bindings::RawBinding> {
        use crate::bindings::{parse_keys, RawBinding};
        // Overrides replace keys, not every other shortcut for the same action.
        // Use action="unbind" to explicitly release an unwanted default key.
        let mut table = crate::bindings::Action::defaults();
        for user in &self.keys {
            table.retain(|b| {
                !(b.keys.eq_ignore_ascii_case(&user.keys)
                    || matches!(
                        (parse_keys(&b.keys), parse_keys(&user.keys)),
                        (Ok((m1, k1)), Ok((m2, k2))) if m1 == m2 && k1 == k2
                    ))
            });
            table.push(RawBinding {
                keys: user.keys.clone(),
                action: user.action.clone(),
            });
        }
        table
    }

    #[allow(dead_code)] // niructl validates a candidate before writing it
    pub fn validate(&self) -> Result<()> {
        use std::fmt::Write as _;

        self.idle
            .validate()
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

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
        if !matches!(self.bar.microphone_visibility.as_str(), "always" | "muted") {
            return Err("bar.microphone_visibility must be always or muted".into());
        }
        if self.bar.title_max > 512 {
            return Err("bar.title_max must be at most 512".into());
        }
        if self.bar.media_width > 200 {
            return Err("bar.media_width must be at most 200".into());
        }
        if !["random", "fixed", "single"].contains(&self.wallpaper.mode.as_str()) {
            return Err("wallpaper.mode must be random, fixed or single".into());
        }
        let mut outputs = std::collections::HashSet::new();
        for o in &self.wallpaper.outputs {
            if o.output.trim().is_empty()
                || !o
                    .output
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err("wallpaper.outputs entries need a RandR output name".into());
            }
            if !outputs.insert(o.output.as_str()) {
                return Err(format!("wallpaper.outputs names {} twice", o.output).into());
            }
            if !["inherit", "theme", "single"].contains(&o.mode.as_str()) {
                return Err(format!(
                    "wallpaper.outputs {} mode must be inherit, theme or single",
                    o.output
                )
                .into());
            }
            if !o.scale.is_empty()
                && !["fill", "fit", "stretch", "center"].contains(&o.scale.as_str())
            {
                return Err(format!(
                    "wallpaper.outputs {} scale must be empty, fill, fit, stretch or center",
                    o.output
                )
                .into());
            }
        }
        if self.monitors.workspaces == 0 || self.monitors.workspaces > 32 {
            return Err("monitors.workspaces must be between 1 and 32".into());
        }
        for p in &self.monitors.profiles {
            if p.name.is_empty() || p.workspaces.is_empty() {
                return Err("monitors.profiles entries need a name and workspaces".into());
            }
        }
        if let Some(bad) = self
            .monitors
            .layouts
            .iter()
            .find(|l| !VALID_LAYOUTS.contains(&l.as_str()))
        {
            return Err(format!("monitors.layouts: unknown layout {bad}").into());
        }
        for p in &self.monitors.profiles {
            if let Some(bad) = p
                .layouts
                .iter()
                .find(|l| !VALID_LAYOUTS.contains(&l.as_str()))
            {
                return Err(format!("monitors.profiles {}: unknown layout {bad}", p.name).into());
            }
        }
        if !(-90.0..=90.0).contains(&self.nightlight.latitude)
            || !(-180.0..=180.0).contains(&self.nightlight.longitude)
            || !(1000..=10000).contains(&self.nightlight.temperature)
            || !(1000..=10000).contains(&self.nightlight.day_temperature)
        {
            return Err("nightlight latitude/longitude/temperature out of range".into());
        }
        if !matches!(self.nightlight.mode.as_str(), "manual" | "fixed" | "solar") {
            return Err("nightlight.mode must be manual, fixed or solar".into());
        }
        if parse_clock(&self.nightlight.on).is_none() || parse_clock(&self.nightlight.off).is_none()
        {
            return Err("nightlight.on/off must be HH:MM".into());
        }
        if self.brightness.step == 0 || self.brightness.step > 50 {
            return Err("brightness.step must be between 1 and 50".into());
        }
        if !(0.1..=0.95).contains(&self.scratchpad.width)
            || !(0.1..=0.95).contains(&self.scratchpad.height)
        {
            return Err("scratchpad.width and height must be between 0.1 and 0.95".into());
        }
        if self.scratchpad.command.is_empty() {
            return Err("scratchpad.command must not be empty".into());
        }
        for r in &self.rules {
            if r.class.is_empty() {
                return Err("rules entries need a class".into());
            }
        }
        // A scratchpad without a class would either never appear or, worse,
        // capture every window of its program.
        let has_class = self.scratchpad.matches_class("probe");
        let names_a_class = self.scratchpad.command.iter().any(|a| {
            matches!(
                a.as_str(),
                "--class" | "-class" | "--app-id" | "-name" | "--name"
            )
        });
        if !has_class && !names_a_class {
            return Err(
                "scratchpad.command needs a window class, for example [\"kitty\", \"--class\", \"scratchpad\"]"
                    .into(),
            );
        }
        if !["fill", "fit", "stretch", "center"].contains(&self.wallpaper.scale.as_str()) {
            return Err("wallpaper.scale must be fill, fit, stretch or center".into());
        }
        if self.wallpaper.rotation_minutes > 100_000 {
            return Err("wallpaper.rotation_minutes is too large".into());
        }
        if self.wallpaper.follow_theme && self.wallpaper.theme_dir.trim().is_empty() {
            return Err("wallpaper.theme_dir must not be empty when follow_theme is true".into());
        }
        if self.autostart.settle_seconds > 60 {
            return Err("autostart.settle_seconds must be at most 60".into());
        }
        if self.autostart.return_workspace > 10 {
            return Err("autostart.return_workspace must be between 0 and 10".into());
        }
        if !(500..=30_000).contains(&self.autostart.window_timeout_ms) {
            return Err("autostart.window_timeout_ms must be between 500 and 30000".into());
        }
        for recipe in &self.recipes {
            if recipe.name.trim().is_empty() {
                return Err("recipe name must not be empty".into());
            }
            for app in &recipe.apps {
                if app.workspace == 0 || app.workspace > 10 {
                    return Err("recipe workspace must be between 1 and 10".into());
                }
                if app.command.is_empty() {
                    return Err("recipe command must not be empty".into());
                }
            }
        }
        for app in &self.autostart.apps {
            if !(1..=10).contains(&app.workspace) {
                return Err("autostart.apps workspace must be between 1 and 10".into());
            }
            if app.command.is_empty() {
                return Err("autostart.apps command must not be empty".into());
            }
        }
        if self.bar.tags.len() > 10 {
            return Err("bar.tags has more than ten entries".into());
        }
        for m in &self.bar.modules {
            // The bar owns this list; the window manager only has to accept
            // it. Keeping its own copy meant every new bar module, `pulse` in
            // 0.14.0, was a config the window manager refused, and a session
            // that did not start. Anything a bar module resembles passes.
            if m.is_empty() || !m.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                return Err(format!("unknown bar module {m:?}").into());
            }
        }
        for (name, format) in [
            ("date_format", &self.bar.date_format),
            ("time_format", &self.bar.time_format),
        ] {
            // Match NIRUBAR: token parsing alone accepts parse-only
            // directives such as %#z. Exercise formatting through a fallible
            // sink so both programs reject the same clock formats.
            if format.is_empty()
                || write!(
                    &mut String::new(),
                    "{}",
                    chrono::Local::now().format(format)
                )
                .is_err()
            {
                return Err(format!("bar.{name} must be a valid nonempty strftime format").into());
            }
        }
        for binding in &self.keys {
            crate::bindings::parse_keys(&binding.keys)?;
            crate::bindings::parse_action(&binding.action)?;
        }
        Ok(())
    }
}

/// Read the small, versioned appearance contract. Invalid or unsupported
/// files are ignored with one diagnostic: the current theme keeps working.
fn appearance_value() -> Option<toml::Value> {
    let path = directory().join("appearance.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("niruwm: {}: {e}", path.display());
            return None;
        }
    };
    let value: toml::Value = match text.parse() {
        Ok(value) => value,
        Err(e) => {
            eprintln!("niruwm: {}: {e}", path.display());
            return None;
        }
    };
    if value.get("schema").and_then(|v| v.as_integer()) != Some(1) {
        eprintln!("niruwm: {}: unsupported appearance schema", path.display());
        return None;
    }
    if let Some(colors) = value.get("colors") {
        let Some(colors) = colors.as_table() else {
            eprintln!("niruwm: {}: colors must be a table", path.display());
            return None;
        };
        for colour in colors.values() {
            let Some(text) = colour.as_str() else {
                eprintln!("niruwm: {}: colours must be strings", path.display());
                return None;
            };
            if Theme::pixel(text).is_err() {
                eprintln!("niruwm: {}: invalid colour {text:?}", path.display());
                return None;
            }
        }
    }
    if let Some(size) = value.get("font").and_then(|font| font.get("size")) {
        if !size
            .as_float()
            .is_some_and(|size| size.is_finite() && size > 0.0)
        {
            eprintln!("niruwm: {}: font.size must be positive", path.display());
            return None;
        }
    }
    if let Some(motion) = value.get("motion").and_then(|motion| motion.get("reduced")) {
        if motion.as_bool().is_none() {
            eprintln!("niruwm: {}: motion.reduced must be boolean", path.display());
            return None;
        }
    }
    Some(value)
}

fn appearance_colour(value: &toml::Value, key: &str) -> Option<String> {
    value.get("colors")?.get(key)?.as_str().map(str::to_string)
}

fn appearance_font_size(value: &toml::Value) -> Option<f32> {
    value.get("font")?.get("size")?.as_float().map(|v| v as f32)
}

fn appearance_font_path(value: &toml::Value) -> Option<String> {
    value.get("font")?.get("path")?.as_str().map(str::to_string)
}

fn appearance_spacing(value: &toml::Value, key: &str) -> Option<u16> {
    value
        .get("spacing")?
        .get(key)?
        .as_integer()
        .and_then(|n| u16::try_from(n).ok())
}

fn apply_appearance_defaults(cfg: &mut Config, file: &toml::Value) {
    let Some(appearance) = appearance_value() else {
        return;
    };
    if file.get("font_size").is_none() {
        if let Some(size) = appearance_font_size(&appearance) {
            cfg.font_size = size;
        }
    }
    if file.get("font_path").is_none() {
        if let Some(path) = appearance_font_path(&appearance) {
            cfg.font_path = Some(path);
        }
    }
    if file.get("gap").is_none() {
        if let Some(gap) = appearance_spacing(&appearance, "gap") {
            cfg.gap = gap;
        }
    }
    if file.get("border_width").is_none() {
        if let Some(width) = appearance
            .get("focus")
            .and_then(|focus| focus.get("width"))
            .and_then(|width| width.as_integer())
            .and_then(|width| u16::try_from(width).ok())
            .or_else(|| appearance_spacing(&appearance, "border_width"))
        {
            cfg.border_width = width;
        }
    }
    if file.get("bar_height").is_none() {
        if let Some(height) = appearance_spacing(&appearance, "bar_height") {
            cfg.bar_height = height;
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
        // Built-in themes are compiled in, so the four that ship always work
        // even before the installer has copied anything. A file of the same
        // name in ~/.config/niruwm/themes takes precedence, so a user can
        // tweak a built-in one.
        let user_file = directory().join("themes").join(format!("{name}.toml"));
        let text = match (name, user_file.exists()) {
            ("niru-noir", false) => include_str!("../themes/niru-noir.toml").to_string(),
            ("niru-nord", false) => include_str!("../themes/niru-nord.toml").to_string(),
            ("niru-satie", false) => include_str!("../themes/niru-satie.toml").to_string(),
            ("niru-carl-larsson", false) => {
                include_str!("../themes/niru-carl-larsson.toml").to_string()
            }
            ("niru-forest-vigil", false) => {
                include_str!("../themes/niru-forest-vigil.toml").to_string()
            }
            ("niru-blood-of-urdr", false) => {
                include_str!("../themes/niru-blood-of-urdr.toml").to_string()
            }
            ("niru-root-and-stone", false) => {
                include_str!("../themes/niru-root-and-stone.toml").to_string()
            }
            ("niru-blue-horizon", false) => {
                include_str!("../themes/niru-blue-horizon.toml").to_string()
            }
            ("niru-the-norns", false) => include_str!("../themes/niru-the-norns.toml").to_string(),
            ("niru-winter-nights", false) => {
                include_str!("../themes/niru-winter-nights.toml").to_string()
            }
            _ => fs::read_to_string(&user_file)?,
        };
        let mut theme: Self = toml::from_str(&text)?;
        if !user_file.exists() {
            if let Some(appearance) = appearance_value() {
                for (field, key) in [
                    (&mut theme.background, "background"),
                    (&mut theme.foreground, "foreground"),
                    (&mut theme.muted, "muted"),
                    (&mut theme.border, "border"),
                    (&mut theme.focus, "focus"),
                ] {
                    if let Some(colour) = appearance_colour(&appearance, key) {
                        *field = colour;
                    }
                }
            }
        }
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

    /// The themes that ship with the programs.
    #[allow(dead_code)] // listed by niructl
    pub fn builtin() -> &'static [&'static str] {
        &[
            "niru-noir",
            "niru-nord",
            "niru-satie",
            "niru-carl-larsson",
            "niru-forest-vigil",
            "niru-blood-of-urdr",
            "niru-root-and-stone",
            "niru-blue-horizon",
            "niru-the-norns",
            "niru-winter-nights",
        ]
    }

    pub fn pixel(s: &str) -> Result<u32> {
        if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Theme colours must be six hex digits".into());
        }
        Ok(u32::from_str_radix(s, 16)?)
    }
}

/// Write the effective configuration to stdout.
///
/// People ask "what is actually in effect?" when a value comes from the
/// built-in defaults rather than from their file. This prints the merged result
/// with a comment for the built-in themes and the current theme name.
#[allow(dead_code)] // used by niruwm --dump-config
/// Replace the shared configuration with the shipped example, keeping a
/// backup of the previous file. This removes obsolete alpha keys as well as
/// user overrides; callers must make that explicit to the user.
pub fn reset_to_example() -> Result<(PathBuf, Option<PathBuf>)> {
    let path = directory().join("config.toml");
    let original = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let parent = path.parent().ok_or("configuration directory is missing")?;
    fs::create_dir_all(parent)?;
    let backup = if original.is_some() {
        let backup = PathBuf::from(format!("{}.niruwm-backup", path.display()));
        fs::write(&backup, original.as_deref().unwrap_or(""))?;
        Some(backup)
    } else {
        None
    };
    let temp = parent.join(format!(".config.toml.{}", std::process::id()));
    fs::write(&temp, include_str!("../config.example.toml"))?;
    fs::rename(&temp, &path)?;
    Ok((path, backup))
}

#[allow(dead_code)] // used by niruwm --dump-config
pub fn dump() -> Result<String> {
    let c = Config::load()?;
    let mut out = String::from("# NIRUWM effective configuration\n");
    out.push_str("# Every value below is what the programs are using now.\n");
    out.push_str("# Reload after editing the real file with: niructl reload\n\n");
    out.push_str(&format!("theme = {:?}\n", c.theme));
    out.push_str(&format!("gap = {}\n", c.gap));
    out.push_str(&format!("border_width = {}\n", c.border_width));
    out.push_str(&format!("bar_height = {}\n", c.bar_height));
    out.push_str(&format!("master_ratio = {}\n", c.master_ratio));
    out.push_str(&format!("terminal = {:?}\n", c.terminal));
    out.push_str(&format!("browser = {:?}\n", c.browser));
    out.push_str(&format!("launcher = {:?}\n", c.launcher));
    out.push_str(&format!("font_size = {}\n\n", c.font_size));
    out.push_str("[monitors]\n");
    out.push_str(&format!("workspaces = {}\n", c.monitors.workspaces));
    for p in &c.monitors.profiles {
        out.push_str(&format!(
            "# profile {}: {:?} primary={}\n",
            p.name, p.workspaces, p.primary
        ));
    }
    out.push_str("\n[wallpaper]\n");
    out.push_str(&format!("enabled = {}\n", c.wallpaper.enabled));
    out.push_str(&format!("dir = {:?}\n", c.wallpaper.dir));
    out.push_str(&format!("mode = {:?}\n", c.wallpaper.mode));
    out.push_str(&format!("scale = {:?}\n", c.wallpaper.scale));
    out.push_str(&format!("follow_theme = {}\n", c.wallpaper.follow_theme));
    out.push_str(&format!("theme_dir = {:?}\n", c.wallpaper.theme_dir));
    out.push_str(&format!(
        "rotation_minutes = {}\n",
        c.wallpaper.rotation_minutes
    ));
    for o in &c.wallpaper.outputs {
        out.push_str(&format!(
            "# output {}: mode={} file={:?} scale={:?}\n",
            o.output, o.mode, o.file, o.scale
        ));
    }
    out.push_str("\n[autostart]\n");
    out.push_str(&format!("enabled = {}\n", c.autostart.enabled));
    out.push_str(&format!(
        "settle_seconds = {}\n",
        c.autostart.settle_seconds
    ));
    out.push_str(&format!(
        "return_workspace = {}\n",
        c.autostart.return_workspace
    ));
    for app in &c.autostart.apps {
        out.push_str(&format!(
            "# workspace {}: {:?}\n",
            app.workspace, app.command
        ));
    }
    out.push_str("\n[nightlight]\n");
    out.push_str(&format!("latitude = {}\n", c.nightlight.latitude));
    out.push_str(&format!("longitude = {}\n", c.nightlight.longitude));
    out.push_str(&format!("temperature = {}\n", c.nightlight.temperature));
    out.push_str(&format!("mode = {:?}\n", c.nightlight.mode));
    out.push_str(&format!("on = {:?}\n", c.nightlight.on));
    out.push_str(&format!("off = {:?}\n", c.nightlight.off));
    out.push_str(&format!(
        "day_temperature = {}\n",
        c.nightlight.day_temperature
    ));
    out.push_str("\n[brightness]\n");
    out.push_str(&format!("device = {:?}\n", c.brightness.device));
    out.push_str(&format!("step = {}\n", c.brightness.step));
    out.push_str("\n[bar]\n");
    out.push_str(&format!("modules = {:?}\n", c.bar.modules));
    out.push_str(&format!("tags = {:?}\n", c.bar.tags));
    out.push_str(&format!("date_format = {:?}\n", c.bar.date_format));
    out.push_str(&format!("time_format = {:?}\n", c.bar.time_format));
    out.push_str(&format!("icons = {}\n", c.bar.icons));
    out.push_str(&format!("popups = {}\n", c.bar.popups));
    out.push_str(&format!(
        "popup_timeout_seconds = {}\n\n",
        c.bar.popup_timeout_seconds
    ));
    out.push_str(&format!(
        "# {} key bindings in effect\n",
        c.effective_keys().len()
    ));
    for k in c.effective_keys() {
        out.push_str(&format!(
            "[[keys]]\nkeys = {:?}\naction = {:?}\n\n",
            k.keys, k.action
        ));
    }
    Ok(out)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ThemePreferences {
    #[serde(default)]
    hidden: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThemeEntry {
    pub id: String,
    pub name: String,
    pub builtin: bool,
    pub overridden: bool,
    pub hidden: bool,
    pub valid: bool,
    pub error: Option<String>,
    pub colors: Option<Theme>,
    pub wallpaper: Option<String>,
    pub wallpaper_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ThemeCatalog {
    pub schema: u32,
    pub active: String,
    pub themes: Vec<ThemeEntry>,
    pub hidden: Vec<String>,
    pub appearance_overrides: bool,
    pub follow_theme: bool,
    pub theme_dir: String,
}

fn theme_preferences_path() -> PathBuf {
    directory().join("theme-preferences.toml")
}

fn read_theme_preferences() -> ThemePreferences {
    fs::read_to_string(theme_preferences_path())
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_theme_preferences(prefs: &ThemePreferences) -> Result<()> {
    let path = theme_preferences_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("new");
    fs::write(&temp, toml::to_string_pretty(prefs)?)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        return home.join(rest);
    }
    PathBuf::from(path)
}

fn theme_wallpaper(id: &str, theme_dir: &Path) -> Option<PathBuf> {
    let short = id.strip_prefix("niru-").unwrap_or(id);
    [
        format!("nirux-wallpaper-{short}-4k.png"),
        format!("nirux-wallpaper-{short}-8k.png"),
        format!("nirux-wallpaper-{short}.png"),
    ]
    .into_iter()
    .map(|name| theme_dir.join(name))
    .find(|path| path.is_file())
}

fn appearance_has_overrides() -> bool {
    appearance_value()
        .as_ref()
        .and_then(|value| value.get("colors"))
        .and_then(|colors| colors.as_table())
        .map(|table| {
            table.keys().any(|key| {
                matches!(
                    key.as_str(),
                    "background" | "foreground" | "muted" | "border" | "focus" | "accent"
                )
            })
        })
        .unwrap_or(false)
}

fn user_theme_ids() -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(directory().join("themes"))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                return None;
            }
            path.file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
                .filter(|id| !id.is_empty())
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn all_theme_ids() -> Vec<String> {
    let mut ids: Vec<String> = Theme::builtin()
        .iter()
        .map(|id| (*id).to_string())
        .collect();
    for id in user_theme_ids() {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// The complete effective theme catalogue, including hidden and invalid user
/// files. A malformed user file is an entry with an error, not a failed list.
pub fn theme_catalog() -> ThemeCatalog {
    let prefs = read_theme_preferences();
    let cfg = Config::load().unwrap_or_default();
    let theme_dir = expand_home(&cfg.wallpaper.theme_dir);
    let mut themes = Vec::new();
    for id in all_theme_ids() {
        let builtin = Theme::builtin().contains(&id.as_str());
        let user_file = directory().join("themes").join(format!("{id}.toml"));
        let overridden = user_file.is_file() && builtin;
        let hidden = prefs.hidden.iter().any(|name| name == &id);
        let wallpaper = theme_wallpaper(&id, &theme_dir);
        match Theme::load(&id) {
            Ok(colors) => themes.push(ThemeEntry {
                name: colors.name.clone(),
                id,
                builtin,
                overridden,
                hidden,
                valid: true,
                error: None,
                colors: Some(colors),
                wallpaper_available: wallpaper.is_some(),
                wallpaper: wallpaper.map(|path| path.display().to_string()),
            }),
            Err(error) => themes.push(ThemeEntry {
                name: id.clone(),
                id,
                builtin,
                overridden,
                hidden,
                valid: false,
                error: Some(error.to_string()),
                colors: None,
                wallpaper_available: wallpaper.is_some(),
                wallpaper: wallpaper.map(|path| path.display().to_string()),
            }),
        }
    }
    ThemeCatalog {
        schema: 1,
        active: cfg.theme,
        themes,
        hidden: prefs.hidden,
        appearance_overrides: appearance_has_overrides(),
        follow_theme: cfg.wallpaper.follow_theme,
        theme_dir: cfg.wallpaper.theme_dir,
    }
}

fn valid_theme_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn theme_lock(dir: &Path) -> Result<fs::File> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    fs::create_dir_all(dir)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(".operation.lock"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("another theme operation is running".into());
    }
    Ok(file)
}

/// Stage every write before committing any destination. Backup files stay beside
/// their destinations so rollback uses atomic renames even across filesystems.
fn theme_transaction(changes: Vec<(PathBuf, Option<Vec<u8>>)>) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    struct Prepared {
        target: PathBuf,
        staged: PathBuf,
        backup: PathBuf,
        had: bool,
        write: bool,
    }
    struct Cleanup(Vec<Prepared>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for item in &self.0 {
                let _ = fs::remove_file(&item.staged);
                let _ = fs::remove_file(&item.backup);
            }
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let mut prepared = Cleanup(Vec::new());
    for (target, data) in changes {
        let metadata = match fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_file() => Some(meta),
            Ok(_) => return Err("theme destination must be a regular file, not a symlink".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let parent = target.parent().ok_or("missing theme directory")?;
        let index = prepared.0.len();
        let staged = parent.join(format!(".niru-theme-{nonce}-{index}.new"));
        let backup = parent.join(format!(".niru-theme-{nonce}-{index}.bak"));
        prepared.0.push(Prepared {
            target,
            staged,
            backup,
            had: metadata.is_some(),
            write: data.is_some(),
        });
        let item = prepared.0.last().unwrap();
        if let Some(data) = data {
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&item.staged)?;
            file.write_all(&data)?;
            if let Some(meta) = &metadata {
                file.set_permissions(meta.permissions())?;
            }
            file.sync_all()?;
        }
        if metadata.is_some() {
            fs::copy(&item.target, &item.backup)?;
        }
    }
    for (index, item) in prepared.0.iter().enumerate() {
        let result = if item.write {
            fs::rename(&item.staged, &item.target)
        } else if item.had {
            fs::remove_file(&item.target)
        } else {
            Ok(())
        };
        if let Err(error) = result {
            let mut rollback_failed = false;
            for old in prepared.0[..index].iter().rev() {
                if old.had {
                    rollback_failed |= fs::rename(&old.backup, &old.target).is_err();
                } else {
                    rollback_failed |= fs::remove_file(&old.target).is_err();
                }
            }
            if rollback_failed {
                // Retain backups for manual recovery rather than deleting them.
                std::mem::forget(prepared);
                return Err(
                    format!("theme commit failed ({error}); recovery backups retained").into(),
                );
            }
            return Err(error.into());
        }
    }
    Ok(())
}

fn validate_theme_png(bytes: &[u8]) -> Result<()> {
    if bytes.len() > 256 * 1024 * 1024 {
        return Err("theme PNG is too large".into());
    }
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info()?;
    let info = reader.info();
    if info.width == 0
        || info.height == 0
        || u64::from(info.width) * u64::from(info.height) > 80_000_000
    {
        return Err("theme PNG dimensions are unsupported".into());
    }
    let mut pixels = vec![0; reader.output_buffer_size()];
    reader.next_frame(&mut pixels)?;
    Ok(())
}

/// Import a validated theme file. Built-in ids are protected unless the user
/// already has an override. An optional PNG wallpaper can be copied into the
/// configured theme directory and is recorded for safe removal.
pub fn theme_import(
    source: &Path,
    id: Option<&str>,
    wallpaper: Option<&Path>,
    replace: bool,
) -> Result<String> {
    let text = fs::read_to_string(source)?;
    let theme: Theme = toml::from_str(&text)?;
    for colour in [
        &theme.background,
        &theme.foreground,
        &theme.muted,
        &theme.border,
        &theme.focus,
    ] {
        Theme::pixel(colour)?;
    }
    let id = id
        .map(str::to_string)
        .or_else(|| {
            source
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string())
        })
        .ok_or("the theme file needs a name")?;
    if !valid_theme_id(&id) {
        return Err("theme id must use letters, digits, - and _".into());
    }
    let dir = directory().join("themes");
    let _guard = theme_lock(&dir)?;
    let target = dir.join(format!("{id}.toml"));
    let builtin = Theme::builtin().contains(&id.as_str());
    if builtin && !target.is_file() {
        return Err(format!("{id} is a built-in theme; choose another id").into());
    }
    if target.is_file() && !replace {
        return Err(format!("a user theme named {id} already exists").into());
    }
    let mut changes = Vec::new();
    if let Some(wallpaper) = wallpaper {
        let bytes = fs::read(wallpaper)?;
        validate_theme_png(&bytes)?;
        let cfg = Config::load().unwrap_or_default();
        let theme_dir = expand_home(&cfg.wallpaper.theme_dir);
        fs::create_dir_all(&theme_dir)?;
        let short = id.strip_prefix("niru-").unwrap_or(&id);
        let name = format!("nirux-wallpaper-{short}-4k.png");
        let target_wallpaper = theme_dir.join(&name);
        if target_wallpaper.exists() && !replace {
            return Err(format!("wallpaper {name} already exists").into());
        }
        // Replace only bytes this theme already owns. A user's manual edit or a
        // legacy file without a matching sidecar is never overwritten.
        if target_wallpaper.exists() {
            use sha2::{Digest, Sha256};
            let owned = fs::read_to_string(dir.join(format!("{id}.assets")))
                .ok()
                .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
                .filter(|value| value.get("wallpaper").and_then(|v| v.as_str()) == Some(&name))
                .and_then(|value| {
                    value
                        .get("sha256")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                })
                .and_then(|expected| {
                    fs::read(&target_wallpaper)
                        .ok()
                        .map(|current| format!("{:x}", Sha256::digest(&current)) == expected)
                })
                .unwrap_or(false);
            if !owned {
                return Err(format!(
                    "existing wallpaper {name} is not owned by this theme; choose another id"
                )
                .into());
            }
        }
        // Never replace a wallpaper recorded by another theme, even with --replace.
        for entry in fs::read_dir(&dir)?.flatten() {
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("assets")
                || entry.path() == dir.join(format!("{id}.assets"))
            {
                continue;
            }
            if fs::read_to_string(entry.path())
                .ok()
                .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
                .and_then(|value| {
                    value
                        .get("wallpaper")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                })
                .as_deref()
                == Some(&name)
            {
                return Err("wallpaper is shared with another theme; choose another id".into());
            }
        }
        let assets = dir.join(format!("{id}.assets"));
        use sha2::{Digest, Sha256};
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let assets_text = format!("schema = 1\nwallpaper = \"{name}\"\nsha256 = \"{hash}\"\n");
        changes.push((target_wallpaper, Some(bytes)));
        changes.push((assets, Some(assets_text.into_bytes())));
    }
    // Publish the catalogue entry last, after its assets are ready.
    changes.push((target, Some(text.into_bytes())));
    theme_transaction(changes)?;
    Ok(id)
}

pub fn theme_remove(id: &str) -> Result<()> {
    if !valid_theme_id(id) {
        return Err("invalid theme id".into());
    }
    let _guard = theme_lock(&directory().join("themes"))?;
    if Theme::builtin().contains(&id) {
        let user_file = directory().join("themes").join(format!("{id}.toml"));
        if !user_file.is_file() {
            return Err(format!("{id} is a bundled theme; hide it instead").into());
        }
    }
    if Config::load().map(|cfg| cfg.theme == id).unwrap_or(false) {
        return Err("choose another theme before removing the active one".into());
    }
    let user_file = directory().join("themes").join(format!("{id}.toml"));
    if !user_file.is_file() {
        return Err(format!("no user theme named {id}").into());
    }
    let assets = directory().join("themes").join(format!("{id}.assets"));
    // Remove only an image this theme imported, still matching its recorded
    // hash, not a symlink, and not referenced by another theme. Legacy,
    // externally modified and shared images are retained.
    let owned_wallpaper = fs::read_to_string(&assets)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .and_then(|value| {
            let name = value.get("wallpaper")?.as_str()?;
            let expected = value.get("sha256")?.as_str()?;
            let path = Path::new(name);
            if path.components().count() != 1 || name == "." || name == ".." {
                return None;
            }
            let cfg = Config::load().unwrap_or_default();
            let theme_dir = expand_home(&cfg.wallpaper.theme_dir);
            let target = theme_dir.join(name);
            let meta = fs::symlink_metadata(&target).ok()?;
            if !meta.file_type().is_file() {
                return None;
            }
            use sha2::{Digest, Sha256};
            let actual = format!("{:x}", Sha256::digest(fs::read(&target).ok()?));
            if actual != expected {
                return None;
            }
            let shared = fs::read_dir(directory().join("themes"))
                .ok()?
                .flatten()
                .filter(|entry| entry.path() != assets)
                .filter(|entry| {
                    entry.path().extension().and_then(|ext| ext.to_str()) == Some("assets")
                })
                .any(|entry| {
                    fs::read_to_string(entry.path())
                        .ok()
                        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
                        .and_then(|value| {
                            value
                                .get("wallpaper")
                                .and_then(|v| v.as_str())
                                .map(str::to_owned)
                        })
                        .as_deref()
                        == Some(name)
                });
            (!shared).then_some(target)
        });
    let mut changes = vec![(user_file, None), (assets, None)];
    if let Some(path) = owned_wallpaper {
        changes.push((path, None));
    }
    let prefs_path = theme_preferences_path();
    if prefs_path.exists() {
        let mut doc: toml_edit::DocumentMut = fs::read_to_string(&prefs_path)?.parse()?;
        if let Some(hidden) = doc.get("hidden").and_then(|item| item.as_array()) {
            let mut next = toml_edit::Array::new();
            for value in hidden.iter().filter(|value| value.as_str() != Some(id)) {
                next.push(value.clone());
            }
            doc["hidden"] = toml_edit::value(next);
        }
        changes.push((prefs_path, Some(doc.to_string().into_bytes())));
    }
    theme_transaction(changes)?;
    Ok(())
}

/// Hide a bundled theme without deleting package/generation files. At least
/// one visible theme and a non-active target are required.
pub fn theme_hide(id: &str, hide: bool) -> Result<()> {
    if !all_theme_ids().iter().any(|name| name == id) {
        return Err(format!("unknown theme: {id}").into());
    }
    let mut prefs = read_theme_preferences();
    if hide {
        if Config::load().map(|cfg| cfg.theme == id).unwrap_or(false) {
            return Err("apply another theme before hiding the active one".into());
        }
        let visible = all_theme_ids()
            .iter()
            .filter(|name| !prefs.hidden.iter().any(|hidden| hidden == *name))
            .count();
        if visible <= 1 {
            return Err("at least one theme must remain visible".into());
        }
        if !prefs.hidden.iter().any(|name| name == id) {
            prefs.hidden.push(id.to_string());
        }
    } else {
        prefs.hidden.retain(|name| name != id);
    }
    write_theme_preferences(&prefs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_png_and_failed_staging_do_not_replace_existing_theme() {
        assert!(validate_theme_png(b"\x89PNG\r\n\x1a\ntruncated").is_err());
        let dir = std::env::temp_dir().join(format!("niru-theme-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let theme = dir.join("theme.toml");
        fs::write(&theme, b"original").unwrap();
        let blocked = dir.join("directory");
        fs::create_dir(&blocked).unwrap();
        assert!(theme_transaction(vec![
            (theme.clone(), Some(b"replacement".to_vec())),
            (blocked, Some(b"invalid".to_vec()))
        ])
        .is_err());
        assert_eq!(fs::read(&theme).unwrap(), b"original");
        assert!(fs::read_dir(&dir).unwrap().flatten().all(|entry| !entry
            .file_name()
            .to_string_lossy()
            .starts_with(".niru-theme-")));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_symlink_is_not_a_theme_transaction_destination() {
        let dir = std::env::temp_dir().join(format!("niru-theme-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let own = dir.join("own.png");
        let link = dir.join("theme.png");
        fs::write(&own, b"personal image").unwrap();
        std::os::unix::fs::symlink(&own, &link).unwrap();
        assert!(theme_transaction(vec![(link, Some(b"replacement".to_vec()))]).is_err());
        assert_eq!(fs::read(&own).unwrap(), b"personal image");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn defaults_are_minimal() {
        let c = Config::default();
        assert!(c.bar.enabled);
        assert!(!c.bar.show_title, "the bar defaults to tags and clock only");
        assert_eq!(c.bar.cell_width, 30);
    }

    #[test]
    fn tenth_workspace_autostart_values_are_valid() {
        let mut c = Config::default();
        c.autostart.return_workspace = 10;
        c.validate()
            .expect("workspace 10 is valid for return_workspace");
        c.autostart.return_workspace = 11;
        assert!(c.validate().is_err());
    }

    #[test]
    fn only_the_scratchpad_class_matches() {
        let cfg = ScratchpadConfig::default();
        // A kitty marked with --class scratchpad is the scratchpad.
        assert!(cfg.matches_class("scratchpad Scratchpad"));
        // A plain kitty is not.
        assert!(!cfg.matches_class("kitty Kitty"));
        assert!(!cfg.matches_class("org.gnome.Terminal"));
    }

    #[test]
    fn a_command_without_a_class_never_matches() {
        let cfg = ScratchpadConfig {
            command: vec!["kitty".into()],
            ..Default::default()
        };
        assert!(!cfg.matches_class("kitty Kitty"));
    }

    #[test]
    fn every_module_named_in_the_error_is_accepted() {
        // The error message lists the valid modules. If the list and the
        // message disagree, a valid module is rejected with a message that
        // names it as valid, which is exactly what happened with `updates`.
        for module in [
            "workspaces",
            "network",
            "battery",
            "clock",
            "volume",
            "media",
            "updates",
        ] {
            let mut c = Config::default();
            c.bar.modules = vec![module.to_string()];
            c.validate()
                .unwrap_or_else(|e| panic!("{module} should be accepted: {e}"));
        }
    }

    #[test]
    fn only_shaped_module_names_are_accepted() {
        // The bar decides which names exist; the window manager only refuses
        // something that cannot be a module name at all.
        let mut c = Config::default();
        c.bar.modules = vec!["nonsense_future_module".to_string()];
        assert!(c.validate().is_ok());
        c.bar.modules = vec!["/etc/passwd".to_string()];
        assert!(c.validate().is_err());
        c.bar.modules = vec![String::new()];
        assert!(c.validate().is_err());
    }

    #[test]
    fn a_key_in_the_file_replaces_the_default_once() {
        let c = Config {
            keys: vec![crate::bindings::RawBinding {
                keys: "super+Tab".into(),
                action: "shell echo hi".into(),
            }],
            ..Config::default()
        };
        let table = c.effective_keys();
        let tabs = table.iter().filter(|b| b.keys == "super+Tab").count();
        assert_eq!(tabs, 1, "one key, one binding");
        let tab = table.iter().find(|b| b.keys == "super+Tab").unwrap();
        assert_eq!(tab.action, "shell echo hi");
        // The pairing key is untouched: only what the file names changes.
        assert!(table.iter().any(|b| b.keys == "super+shift+Tab"));
    }

    #[test]
    fn adding_an_action_does_not_remove_its_other_shortcuts() {
        let c = Config {
            keys: vec![crate::bindings::RawBinding {
                keys: "super+x".into(),
                action: "focus_next".into(),
            }],
            ..Config::default()
        };
        let table = c.effective_keys();
        for key in ["super+x", "super+j", "super+Right", "super+Down"] {
            assert!(
                table
                    .iter()
                    .any(|b| b.keys == key && b.action == "focus_next"),
                "missing {key}"
            );
        }
    }

    #[test]
    fn accepts_the_full_bar_section_from_the_example() {
        // Parse the shipped example file: if the bar gains a key here first,
        // this test fails instead of leaving NIRUWM unable to start.
        let example = include_str!("../config.example.toml");
        toml::from_str::<Config>(example).expect("config.example.toml must parse in NIRUWM");
    }

    #[test]
    fn accepts_every_bar_key() {
        // The bar's own settings must parse here, or `niruwm --check-config`
        // rejects a file the bar just wrote. This drifted once: battery,
        // network and icons were added to NIRUBAR only.
        let shared = r#"
[bar]
enabled = true
title = false
title_max = 60
cell_width = 30
separator = "|"
battery = true
network = true
icons = true
"#;
        let c: Config = toml::from_str(shared).expect("every bar key must parse");
        assert!(c.bar.battery && c.bar.network && c.bar.icons);
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
        assert!(c.bar.show_title);
        assert_eq!(c.bar.cell_width, 34);
        assert_eq!(c.terminal, vec!["kitty"]);
    }

    #[test]
    fn nightlight_schedule_and_brightness_validate() {
        let c: Config = toml::from_str(
            "[nightlight]\nmode = \"fixed\"\non = \"20:30\"\noff = \"07:15\"\n[brightness]\nstep = 7\ndevice = \"intel_backlight\"\n",
        )
        .unwrap();
        assert!(c.validate().is_ok());
        assert_eq!(c.brightness.step, 7);
        assert_eq!(c.nightlight.mode, "fixed");
        let bad: Config = toml::from_str("[nightlight]\nmode = \"sometimes\"\n").unwrap();
        assert!(bad.validate().is_err());
        let bad: Config = toml::from_str("[brightness]\nstep = 0\n").unwrap();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn partial_config_falls_back_to_defaults() {
        let c: Config = toml::from_str("[bar]\ntitle = true\n").unwrap();
        assert!(c.bar.show_title);
        assert_eq!(c.bar.cell_width, 30);
        assert_eq!(c.gap, 8);
    }

    #[test]
    fn all_builtin_themes_load() {
        for name in Theme::builtin() {
            let t = Theme::load(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!t.name.is_empty());
        }
    }

    #[test]
    fn themes_differ_in_background() {
        let mut seen = std::collections::HashSet::new();
        for name in Theme::builtin() {
            let t = Theme::load(name).unwrap();
            assert!(
                seen.insert(t.background.clone()),
                "{name} reuses another theme's background"
            );
        }
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

    #[test]
    fn invalid_clock_formats_are_rejected_like_nirubar() {
        for bad in ["%", "%Q", "%#", "%#z", ""] {
            let mut c = Config::default();
            c.bar.time_format = bad.into();
            assert!(c.validate().is_err(), "accepted time format {bad:?}");
            c.bar.time_format = "%H:%M".into();
            c.bar.date_format = bad.into();
            assert!(c.validate().is_err(), "accepted date format {bad:?}");
        }
    }

    #[test]
    fn idle_times_parse_and_use_documented_names() {
        let c: Config =
            toml::from_str("[idle]\nscreensaver_timeout = 5\nlock_after_screensaver = 2\n")
                .expect("[idle] must parse");
        assert_eq!(c.idle.screensaver_timeout, 5);
        assert_eq!(c.idle.lock_after_screensaver, 2);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn idle_defaults_apply_when_the_section_is_missing() {
        let c: Config = toml::from_str("theme = \"niru-noir\"\n").unwrap();
        assert_eq!(c.idle.screensaver_timeout, 15);
        assert_eq!(c.idle.lock_after_screensaver, 15);
        assert!(c.validate().is_ok(), "an absent [idle] must stay valid");
    }

    #[test]
    fn old_idle_spellings_are_still_accepted() {
        let c: Config =
            toml::from_str("[idle]\nidle_minutes = 7\nlock_after_minutes = 3\n").unwrap();
        assert_eq!(c.idle.screensaver_timeout, 7);
        assert_eq!(c.idle.lock_after_screensaver, 3);
    }

    #[test]
    fn a_zero_screensaver_timeout_is_rejected_not_silently_unlocked() {
        let mut c = Config::default();
        c.idle.screensaver_timeout = 0;
        assert!(c.validate().is_err(), "zero must not mean 'never'");
    }

    #[test]
    fn absurd_idle_values_are_rejected_or_clamped() {
        let mut c = Config::default();
        c.idle.screensaver_timeout = IdleConfig::MAX_MINUTES + 1;
        assert!(c.validate().is_err());

        // The runtime clamp never produces a zero or an out-of-range timer.
        let already = IdleConfig {
            screensaver_timeout: 0,
            lock_after_screensaver: 99_999,
            dpms_off_after_lock: 0,
        };
        let safe = already.safe();
        assert_eq!(safe.screensaver_timeout, 15, "bad timeout falls back");
        assert_eq!(safe.lock_after_screensaver, IdleConfig::MAX_MINUTES);
        assert_eq!(safe.dpms_off_after_lock, 0, "0 keeps the display on");
    }

    #[test]
    fn idle_section_is_accepted_by_the_full_shared_file() {
        // NIRUWM must accept the section even though NIRUIDLE owns it.
        let mut c = Config::default();
        c.idle.screensaver_timeout = 3;
        assert!(c.validate().is_ok());
    }
}
