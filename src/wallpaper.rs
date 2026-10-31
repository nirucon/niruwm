//! Wallpaper handling.
//!
//! Picks a picture and sets it through the first available X11 tool. Rotation
//! is driven by the WM's main loop, not a separate daemon, so starting or
//! stopping a session is enough to start and stop rotation.
//!
//! NIRUWM only owns the wallpaper when `[wallpaper] enabled = true`. When it
//! is off, another program may own the root background; running both would
//! make two programs fight over it.
//!
//! Every screen is set separately: the common settings apply to every screen
//! that has no `[[wallpaper.outputs]]` entry, and each picture is scaled to
//! that screen's own geometry. One picture is never stretched across the whole
//! desktop.

use crate::config::{Result, WallpaperConfig};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// One connected screen and where it sits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// RandR output name, e.g. `HDMI-1`.
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Output {
    /// `WxH+X+Y`, the geometry form the background tools understand.
    pub fn geometry(&self) -> String {
        format!("{}x{}+{}+{}", self.width, self.height, self.x, self.y)
    }
}

/// Look up a program in PATH without spawning anything.
fn which(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    path.split(':')
        .filter(|p| !p.is_empty())
        .any(|dir| Path::new(dir).join(name).is_file())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Feh,
    Hsetroot,
    Nitrogen,
    Xwallpaper,
    Xsetroot,
}

impl Tool {
    fn binary(self) -> &'static str {
        match self {
            Tool::Feh => "feh",
            Tool::Hsetroot => "hsetroot",
            Tool::Nitrogen => "nitrogen",
            Tool::Xwallpaper => "xwallpaper",
            Tool::Xsetroot => "xsetroot",
        }
    }

    /// Arguments for one image, scaled to one screen.
    ///
    /// `geometry` is `WxH+X+Y`. Every tool here places the picture on the
    /// screen it belongs to; `scale` is the NIRUX name (`fill`, `fit`,
    /// `stretch`, `center`).
    fn image_args(self, scale: &str, geometry: &str, path: &Path) -> Vec<String> {
        let g = geometry.to_string();
        let p = path.display().to_string();
        let text = |args: &[&str], flags: bool| -> Vec<String> {
            let mut out: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            if flags || self == Tool::Feh {
                out.push("--no-fehbg".into());
            }
            // feh has no per-output selector. Its geometry-like argument is
            // interpreted as another image path, so pass only the image.
            if self != Tool::Feh {
                out.push(g.clone());
            }
            out.push(p.clone());
            out
        };
        match (self, scale) {
            // feh: one image per screen; `--no-fehbg` keeps the user's own
            // ~/.fehbg script from being overwritten on every refresh.
            (Tool::Feh, "fill") => text(&["--bg-fill"], true),
            (Tool::Feh, "fit") => text(&["--bg-max"], true),
            (Tool::Feh, "stretch") => text(&["--bg-scale"], true),
            (Tool::Feh, _) => text(&["--bg-center"], true),
            (Tool::Hsetroot, "fill") => text(&["-fill"], false),
            (Tool::Hsetroot, "fit") => text(&["-full"], false),
            (Tool::Hsetroot, "stretch") => text(&["-stretch"], false),
            (Tool::Hsetroot, _) => text(&["-center"], false),
            (Tool::Nitrogen, "fill") => text(&["--set-zoom-fill"], false),
            (Tool::Nitrogen, "fit") => text(&["--set-zoom"], false),
            (Tool::Nitrogen, "stretch") => text(&["--set-auto"], false),
            (Tool::Nitrogen, _) => text(&["--set-centered"], false),
            (Tool::Xwallpaper, "fill") => text(&["--zoom"], false),
            (Tool::Xwallpaper, "fit") => text(&["--maximize"], false),
            (Tool::Xwallpaper, "stretch") => text(&["--stretch"], false),
            (Tool::Xwallpaper, _) => text(&["--center"], false),
            (Tool::Xsetroot, _) => Vec::new(),
        }
    }

    /// Arguments that paint one screen in a solid colour.
    fn colour_args(self, _geometry: &str, pixel: &str) -> Vec<String> {
        // Each tool has its own way to paint one screen with one colour.
        // `xsetroot` takes only a colour under `-solid` and rejects anything
        // else, so it is asked for the colour alone.
        match self {
            // feh has no colour-only mode: it needs at least one image, and
            // with none it scans the directory it was started in and fails.
            // A one-pixel image of the colour is the smallest honest input;
            // feh stretches it to the screen in `--bg-fill` mode.
            Tool::Feh => vec![
                "--no-fehbg".into(),
                "--image-bg".into(),
                pixel.into(),
                "--bg-fill".into(),
                solid_image(pixel).display().to_string(),
            ],
            Tool::Hsetroot => vec!["-solid".into(), pixel.into()],
            Tool::Nitrogen => vec![
                "--set-color".into(),
                pixel.into(),
                "--head".into(),
                "0".into(),
            ],
            Tool::Xwallpaper => vec!["--solid".into(), pixel.into()],
            Tool::Xsetroot => vec!["-solid".into(), pixel.into()],
        }
    }

    fn is_image_capable(self) -> bool {
        self != Tool::Xsetroot
    }
}

/// A 1x1 PNG in `colour` (`#rrggbb`), written under the state directory and
/// reused for every screen. Only an image-capable tool needs it; the others
/// take the colour directly.
fn solid_image(colour: &str) -> PathBuf {
    let hex: String = colour.trim_start_matches('#').chars().take(6).collect();
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
    let (r, g, b) = (byte(0), byte(2), byte(4));
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("niruwm");
    let path = dir.join(format!("solid-{r:02x}{g:02x}{b:02x}.png"));
    if path.is_file() {
        return path;
    }
    // RGBA, one pixel: signature, IHDR, IDAT (zlib-stored), IEND.
    let mut png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        png.extend_from_slice(&body);
        png.extend_from_slice(&crc32(&body).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit RGBA, default filters
    chunk(b"IHDR", &ihdr);
    let mut raw = vec![0u8, r, g, b, 0xff];
    // zlib: no compression, stored block.
    let mut zlib = vec![
        0x78,
        0x01,
        0x01,
        raw.len() as u8,
        0x00,
        !(raw.len() as u8),
        0xff,
    ];
    zlib.append(&mut raw);
    zlib.extend_from_slice(&adler32(&zlib[4..]).to_be_bytes());
    chunk(b"IDAT", &zlib);
    chunk(b"IEND", &[]);
    let _ = std::fs::create_dir_all(&dir);
    let temp = dir.join(format!(".solid-{}.{}.png", std::process::id(), r));
    if std::fs::write(&temp, &png).is_ok() {
        let _ = std::fs::rename(&temp, &path);
    }
    path
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in data {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Tools in order of preference; the first that works wins.
const TOOLS: &[Tool] = &[Tool::Feh, Tool::Hsetroot, Tool::Nitrogen, Tool::Xwallpaper];

pub struct Wallpaper {
    cfg: WallpaperConfig,
    dir: PathBuf,
    mode: Mode,
    image: Option<PathBuf>,
    last_change: Instant,
    rotation: Duration,
    /// Explicit file list; empty means "every image in dir".
    files: Vec<PathBuf>,
    /// Current theme name, used when theme following is enabled.
    theme: String,
    /// Whether a theme change should apply that theme's default wallpaper.
    follow_theme: bool,
    /// Directory holding the NIRUX theme wallpapers.
    theme_dir: PathBuf,
    /// Screens to set: the last list the window manager read from RandR.
    outputs: Vec<Output>,
    /// One picture this process itself chose for the common picture, so
    /// rotation keeps showing it between ticks.
    rotation_image: Option<PathBuf>,
    /// A fixed-mode picture chosen on the first apply, kept for the session.
    fixed_image: Option<PathBuf>,
    /// How each screen ended up, in the order it was set.
    last: Vec<OutputState>,
    /// True once the screens were applied, so status can tell whether the
    /// running window manager has rendered anything at all.
    rendered: bool,
}

/// What one screen shows, and where that choice came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputPlan {
    pub output: String,
    /// `theme`, `image`, `colour` or `disabled`.
    pub kind: String,
    pub path: String,
    pub scale: String,
    /// `inherit` or `override`.
    pub source: String,
}

/// What actually reached one screen in the running session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputState {
    pub output: String,
    /// `theme`, `image`, `colour`, `failed` or `off`.
    pub kind: String,
    pub path: String,
    pub scale: String,
    pub source: String,
    /// Why the screen shows what it shows, when that needs saying.
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Pick a different picture from the directory on every rotation.
    Random,
    /// Pick one picture and keep it.
    Fixed,
    /// Always use `file`.
    Single,
}

impl Wallpaper {
    pub fn new(cfg: &WallpaperConfig, theme: &str) -> Self {
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
            cfg: cfg.clone(),
            dir: expand_home(&cfg.dir),
            mode,
            image,
            last_change: Instant::now(),
            rotation: Duration::from_secs(u64::from(cfg.rotation_minutes) * 60),
            files: cfg.files.iter().map(|f| expand_home(f)).collect(),
            theme: theme.to_string(),
            follow_theme: cfg.follow_theme,
            theme_dir: expand_home(&cfg.theme_dir),
            outputs: Vec::new(),
            rotation_image: None,
            fixed_image: None,
            last: Vec::new(),
            rendered: false,
        }
    }

    /// Tell the wallpaper which screens exist. Called after every RandR read.
    pub fn set_outputs(&mut self, outputs: Vec<Output>) {
        self.outputs = outputs;
    }

    #[allow(dead_code)] // used by tests and by niructl through the shared module
    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    /// What the running session rendered, per screen. Empty until the first
    /// apply, so configuration alone is never reported as a rendered result.
    #[allow(dead_code)] // read by niructl once the WM publishes its state
    pub fn rendered(&self) -> &[OutputState] {
        &self.last
    }

    #[allow(dead_code)] // read by niructl once the WM publishes its state
    pub fn has_rendered(&self) -> bool {
        self.rendered
    }

    fn entry(&self, output: &str) -> Option<&crate::config::OutputWallpaper> {
        self.cfg
            .outputs
            .iter()
            .find(|o| o.output == output)
            .filter(|o| !o.is_inherit() || !o.scale.trim().is_empty())
    }

    pub fn effective_scale(&self, output: &str) -> String {
        self.entry(output)
            .map(|o| o.scale.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.cfg.scale.clone())
    }

    /// True when this screen shows whatever theme is selected.
    fn theme_enabled(&self, output: &str) -> bool {
        if !self.cfg.enabled || !self.cfg.follow_theme {
            return false;
        }
        match self.entry(output) {
            Some(o) => match o.mode.as_str() {
                "single" => false,
                "theme" => true,
                // An explicit picture on this screen wins; an entry that only
                // changes the scale still follows the theme.
                _ => o.file.trim().is_empty() && self.mode != Mode::Single,
            },
            None => self.mode != Mode::Single,
        }
    }

    /// The common picture, as this process knows it. In `single` mode it is
    /// the configured file; in `fixed` mode a file chosen once for the
    /// session; in `random` mode the picture rotation is showing.
    fn common_image(&self) -> Option<PathBuf> {
        match self.mode {
            Mode::Single => self.image.clone(),
            Mode::Fixed => self.fixed_image.clone().or_else(|| self.image.clone()),
            Mode::Random => self.rotation_image.clone(),
        }
    }

    /// The picture one screen shows: its own file when it has one, otherwise
    /// the theme picture when it follows the theme, otherwise the common
    /// picture. `None` means the theme has no picture and its colour is used.
    pub fn plan_for(&self, output: &str) -> (String, Option<PathBuf>) {
        if !self.cfg.enabled {
            return ("disabled".into(), None);
        }
        let entry = self.entry(output);
        if let Some(o) = entry {
            if !o.file.trim().is_empty() {
                return ("image".into(), Some(expand_home(&o.file)));
            }
            if o.mode == "theme" {
                return match self.theme_image() {
                    Some(path) => ("theme".into(), Some(path)),
                    None => ("colour".into(), None),
                };
            }
            if o.mode == "single" && self.mode != Mode::Single {
                return ("image".into(), self.pick());
            }
        }
        if self.theme_enabled(output) {
            return match self.theme_image() {
                Some(path) => ("theme".into(), Some(path)),
                None => ("colour".into(), None),
            };
        }
        match self.common_image() {
            Some(path) => ("image".into(), Some(path)),
            None => ("colour".into(), None),
        }
    }

    /// The plan for every screen, in screen order. Used by status and tests.
    #[allow(dead_code)] // used by tests and by niructl through the shared module
    pub fn plan(&self) -> Vec<OutputPlan> {
        self.outputs
            .iter()
            .map(|out| {
                let (kind, path) = self.plan_for(&out.name);
                OutputPlan {
                    output: out.name.clone(),
                    kind,
                    path: path
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(String::new),
                    scale: self.effective_scale(&out.name),
                    source: if self.entry(&out.name).is_some() {
                        "override".into()
                    } else {
                        "inherit".into()
                    },
                }
            })
            .collect()
    }

    /// Apply the current theme's wallpaper on the screens that follow it.
    ///
    /// Screens with an explicit picture keep it. Returns the theme picture
    /// when there is one, `None` otherwise.
    pub fn apply_theme(&mut self) -> Result<Option<PathBuf>> {
        if !self.cfg.enabled || !self.cfg.follow_theme || self.mode == Mode::Single {
            return Ok(None);
        }
        let path = self.theme_image();
        if path.is_none() {
            eprintln!(
                "niruwm: no wallpaper image for theme {}; using the theme background colour. \
                 Install the wallpaper set with `nirux wallpapers download`, or pick one in the launcher.",
                self.theme
            );
            // A session picture chosen earlier belongs to the theme it was
            // chosen for. Keeping it here would repaint the previous theme's
            // picture while the colour was supposed to take over.
            self.fixed_image = None;
            self.rotation_image = None;
        } else if self.mode == Mode::Fixed {
            self.fixed_image = path.clone();
        }
        self.last_change = Instant::now();
        // Screens that keep their own picture are re-set here too, so a theme
        // change never disturbs them and a reconnect picks them up again.
        self.apply_all()?;
        Ok(path)
    }

    /// Update the theme and apply what follows it.
    pub fn set_theme(&mut self, theme: &str) -> Result<Option<PathBuf>> {
        self.theme = theme.to_string();
        self.apply_theme()
    }

    /// First theme picture found for the current theme, in the configured
    /// directory, the XDG Pictures directory, then the English default.
    pub fn theme_image(&self) -> Option<PathBuf> {
        let slug = self.theme.strip_prefix("niru-").unwrap_or(&self.theme);
        if slug.is_empty() {
            return None;
        }
        let mut roots = vec![self.theme_dir.clone()];
        // Older installations may have written the English ~/Pictures default
        // while the user's XDG Pictures directory is localized (Bilder, Kuvat,
        // ...). Follow the actual user-dirs.dirs before giving up.
        if let Some(pictures) = configured_pictures_dir() {
            roots.push(pictures.join("Wallpapers/NIRU"));
        }
        roots.push(expand_home("~/Pictures/Wallpapers/NIRU"));
        roots.dedup();
        for root in roots {
            for suffix in ["4k.png", "8k.png", "png", "jpg", "jpeg", "webp", "bmp"] {
                let candidate = root.join(format!("nirux-wallpaper-{slug}-{suffix}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// Apply the wallpaper now and return the picture at least one screen
    /// shows. Screens that follow the theme keep the theme picture.
    pub fn apply(&mut self) -> Result<Option<PathBuf>> {
        if !self.cfg.enabled {
            return Ok(None);
        }
        if self.mode == Mode::Single {
            self.image = if self.cfg.file.is_empty() {
                None
            } else {
                Some(expand_home(&self.cfg.file))
            };
        }
        // Following the theme decides the picture even when the theme has
        // none: its colour is then the answer. Falling through to the
        // session's own picture would show the wallpaper of a previous theme.
        if self.follow_theme && self.mode != Mode::Single {
            return self.apply_theme();
        }
        match self.mode {
            Mode::Single => {}
            Mode::Fixed => {
                if self.fixed_image.is_none() {
                    self.fixed_image = self.image.clone().or_else(|| self.pick());
                }
            }
            Mode::Random => {
                if self.rotation_image.is_none() {
                    self.rotation_image = self.pick();
                }
            }
        }
        self.last_change = Instant::now();
        self.apply_all()
    }

    /// Choose a new common picture and set every screen.
    fn rotate(&mut self) -> Result<()> {
        self.fixed_image = None;
        self.rotation_image = self.pick();
        self.last_change = Instant::now();
        self.apply_all()?;
        Ok(())
    }

    /// Called from the event loop; rotates when the interval has passed.
    ///
    /// Returns `true` when the wallpaper changed, so the caller can log it.
    pub fn tick(&mut self) -> bool {
        if !self.cfg.enabled || self.mode == Mode::Single || self.rotation.is_zero() {
            return false;
        }
        if self.last_change.elapsed() < self.rotation {
            return false;
        }
        // Fixed mode keeps its picture; only the timer is reset.
        if self.mode == Mode::Fixed {
            self.last_change = Instant::now();
            return false;
        }
        let _ = self.rotate();
        true
    }

    fn pick(&self) -> Option<PathBuf> {
        // An explicit list wins over the directory.
        if !self.files.is_empty() {
            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as usize)
                .unwrap_or(0);
            return Some(self.files[seed % self.files.len()].clone());
        }
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
        // Deterministic pseudo-random choice: the time and the process id
        // change between rotations, which is enough variety without pulling in
        // a random number generator.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as usize)
            .unwrap_or(0)
            ^ std::process::id() as usize;
        Some(files[seed % files.len()].clone())
    }

    fn theme_colour(&self) -> String {
        let colour = crate::config::Theme::load(&self.theme)
            .map(|t| t.background)
            .unwrap_or_else(|_| "0f0f10".to_string());
        format!("#{colour}")
    }

    /// Set every screen. One screen that cannot be set is reported but does
    /// not stop the others, because a hotplugged screen must never leave the
    /// rest of the desktop without a background.
    pub fn apply_all(&mut self) -> Result<Option<PathBuf>> {
        self.last.clear();
        self.rendered = true;
        if !self.cfg.enabled {
            return Ok(None);
        }
        if self.outputs.is_empty() {
            // Without RandR information the common picture still goes to the
            // whole screen the old way.
            let (kind, path) = self.plan_for("");
            if let Some(path) = &path {
                set_image(path, &self.cfg.scale, &[])?;
                return Ok(Some(path.clone()));
            }
            debug_assert_eq!(kind, "colour");
            return Ok(None);
        }
        let colour = self.theme_colour();
        let mut used = None;
        let mut failures: Vec<String> = Vec::new();
        for out in self.outputs.clone() {
            let (kind, path) = self.plan_for(&out.name);
            let scale = self.effective_scale(&out.name);
            let mut state = OutputState {
                output: out.name.clone(),
                kind: kind.clone(),
                path: path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                scale,
                source: if self.entry(&out.name).is_some() {
                    "override".into()
                } else {
                    "inherit".into()
                },
                note: String::new(),
            };
            let set = match &path {
                Some(path) => set_image(path, &state.scale, std::slice::from_ref(&out)),
                None => set_colour(&colour, &out),
            };
            match set {
                Ok(()) => {
                    if state.kind == "colour" {
                        state.path = colour.clone();
                    }
                    used = path.or(used);
                }
                Err(e) => {
                    state.kind = "failed".into();
                    state.note = e.to_string();
                    failures.push(format!("{}: {e}", out.name));
                }
            }
            self.last.push(state);
        }
        if failures.len() == self.outputs.len() && !self.outputs.is_empty() {
            return Err(format!("could not set the wallpaper: {}", failures.join("; ")).into());
        }
        for failure in failures {
            eprintln!("niruwm: wallpaper: {failure}");
        }
        Ok(used)
    }
}

/// Set one picture on zero or more screens.
///
/// An empty `outputs` list means "the whole screen", which is the only option
/// when RandR reported nothing.
fn set_image(path: &Path, scale: &str, outputs: &[Output]) -> Result<()> {
    let mut failures = Vec::new();
    for tool in TOOLS.iter().copied().filter(|t| t.is_image_capable()) {
        if !which(tool.binary()) {
            continue;
        }
        let geometry = outputs
            .first()
            .map(Output::geometry)
            .unwrap_or_else(|| "0x0+0+0".into());
        let args = tool.image_args(scale, &geometry, path);
        eprintln!("niruwm: wallpaper: {} {}", tool.binary(), args.join(" "));
        match std::process::Command::new(tool.binary())
            .args(&args)
            .status()
        {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => failures.push(format!("{}: {status}", tool.binary())),
            Err(e) => failures.push(format!("{}: {e}", tool.binary())),
        }
    }
    if failures.is_empty() {
        return Err(
            "no wallpaper image setter available; install feh, hsetroot, nitrogen or xwallpaper"
                .into(),
        );
    }
    Err(format!(
        "could not set wallpaper {}: {}",
        path.display(),
        failures.join("; ")
    )
    .into())
}

/// Paint the root window with the theme's own background colour.
///
/// Used when a screen follows a theme that has no wallpaper image. Leaving the
/// previous theme's picture on screen would silently show the wrong theme, so
/// the fallback is a solid colour that belongs to the theme that is actually
/// selected. The colour comes from the theme file, never a hardcoded value:
/// Satie is light and the others are dark.
fn set_colour(colour: &str, output: &Output) -> Result<()> {
    let mut failures = Vec::new();
    let mut tools = TOOLS.to_vec();
    // xsetroot can always paint a colour, so it is the last resort rather than
    // a separate path.
    tools.push(Tool::Xsetroot);
    for tool in tools {
        if !which(tool.binary()) {
            continue;
        }
        let args = tool.colour_args(&output.geometry(), colour);
        match std::process::Command::new(tool.binary())
            .args(&args)
            .status()
        {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => failures.push(format!("{}: {status}", tool.binary())),
            Err(e) => failures.push(format!("{}: {e}", tool.binary())),
        }
    }
    if failures.is_empty() {
        eprintln!("niruwm: no tool can paint a solid background ({colour})");
        return Ok(());
    }
    Err(format!(
        "could not paint the {colour} background: {}",
        failures.join("; ")
    )
    .into())
}

fn configured_pictures_dir() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        });
    let text = fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    for line in text.lines() {
        let Some(value) = line.strip_prefix("XDG_PICTURES_DIR=") else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        if value.is_empty() || value == "$HOME/Desktop" {
            return None;
        }
        let home = std::env::var("HOME").unwrap_or_default();
        return Some(PathBuf::from(value.replace("$HOME", &home)));
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OutputWallpaper;
    use std::fs::File;

    fn tmpdir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("niruwm-wp-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn config(dir: &Path) -> WallpaperConfig {
        WallpaperConfig {
            enabled: true,
            dir: dir.display().to_string(),
            theme_dir: dir.display().to_string(),
            ..WallpaperConfig::default()
        }
    }

    /// Each tool must be given only arguments it accepts. `xsetroot -solid`
    /// rejects anything after the colour, which left a theme without a
    /// wallpaper unpainted and only a failing command in the session log.
    #[test]
    fn a_colour_is_painted_with_arguments_each_tool_accepts() {
        assert_eq!(
            Tool::Xsetroot.colour_args("1920x1080+0+0", "#141414"),
            vec!["-solid", "#141414"]
        );
        assert_eq!(
            Tool::Feh.colour_args("1920x1080+0+0", "#141414"),
            vec![
                "--no-fehbg".to_string(),
                "--image-bg".to_string(),
                "#141414".to_string(),
                "--bg-fill".to_string(),
                solid_image("#141414").display().to_string()
            ]
        );
        for tool in TOOLS {
            let args = tool.colour_args("1920x1080+0+0", "#141414");
            assert!(
                args.iter().any(|a| a == "#141414"),
                "{} does not receive the colour: {args:?}",
                tool.binary()
            );
        }
    }

    #[test]
    fn theme_wallpaper_uses_the_theme_slug() {
        let dir = tmpdir("slug");
        File::create(dir.join("nirux-wallpaper-nord-4k.png")).unwrap();
        File::create(dir.join("nirux-wallpaper-nord-8k.png")).unwrap();
        let mut cfg = config(&dir);
        cfg.theme_dir = dir.display().to_string();
        let w = Wallpaper::new(&cfg, "niru-nord");
        assert_eq!(
            w.theme_image(),
            Some(dir.join("nirux-wallpaper-nord-4k.png"))
        );
        let mut w = Wallpaper::new(&cfg, "niru-mytheme");
        File::create(dir.join("nirux-wallpaper-mytheme-4k.png")).unwrap();
        assert_eq!(
            w.theme_image(),
            Some(dir.join("nirux-wallpaper-mytheme-4k.png"))
        );
        let mut cfg = config(&dir);
        cfg.theme_dir = dir.display().to_string();
        w = Wallpaper::new(&cfg, "niru-test-missing-wallpaper");
        assert_eq!(w.theme_image(), None);
        fs::remove_dir_all(&dir).ok();
    }

    /// A theme that ships no wallpaper must show its colour, not the picture
    /// the previous theme chose. `fixed` mode caches that picture for the
    /// session, and the cache belongs to the theme it was chosen for.
    #[test]
    fn a_theme_without_a_picture_forgets_the_previous_picture() {
        let dir = tmpdir("no-picture");
        File::create(dir.join("nirux-wallpaper-nord-4k.png")).unwrap();
        let mut cfg = config(&dir);
        cfg.mode = "fixed".into();
        let mut w = Wallpaper::new(&cfg, "niru-nord");
        // No display here, so `apply_theme` stops at the setter; the picture it
        // would use is what this test is about.
        let _ = w.set_theme("niru-nord");
        assert_eq!(
            w.common_image(),
            Some(dir.join("nirux-wallpaper-nord-4k.png")),
            "the theme with a picture is shown"
        );
        // A full apply must keep following the theme instead of picking a
        // session picture; the colour is then what the screen shows.
        let _ = w.set_theme("niru-test-missing-wallpaper");
        assert_eq!(w.common_image(), None, "apply picked a picture again");
        assert_eq!(
            w.common_image(),
            None,
            "the old theme's picture must not stand in for the colour"
        );
        assert_eq!(
            w.plan_for("eDP-1").0,
            "colour",
            "the screen paints the theme colour"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_screen_entry_overrides_only_that_screen() {
        let dir = tmpdir("outputs");
        File::create(dir.join("nirux-wallpaper-nord-4k.png")).unwrap();
        let mut cfg = config(&dir);
        cfg.outputs = vec![OutputWallpaper {
            output: "DP-1".into(),
            mode: "single".into(),
            file: dir.join("mine.png").display().to_string(),
            scale: "fit".into(),
        }];
        let w = Wallpaper::new(&cfg, "niru-nord");
        assert!(!w.theme_enabled("DP-1"));
        assert!(w.theme_enabled("eDP-1"));
        assert_eq!(w.effective_scale("DP-1"), "fit");
        assert_eq!(w.effective_scale("eDP-1"), "fill");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn theme_override_keeps_the_theme_on_one_screen() {
        let dir = tmpdir("theme-output");
        let mut cfg = config(&dir);
        cfg.mode = "single".into();
        cfg.file = "/tmp/own.png".into();
        cfg.outputs = vec![OutputWallpaper {
            output: "HDMI-1".into(),
            mode: "theme".into(),
            file: String::new(),
            scale: String::new(),
        }];
        let w = Wallpaper::new(&cfg, "niru-nord");
        assert!(
            w.theme_enabled("HDMI-1"),
            "theme override follows the theme"
        );
        assert!(!w.theme_enabled("eDP-1"), "the common single image wins");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn duplicate_or_unknown_output_settings_are_rejected() {
        let mut cfg = WallpaperConfig {
            outputs: vec![
                OutputWallpaper {
                    output: "DP-1".into(),
                    mode: "bogus".into(),
                    ..OutputWallpaper::default()
                },
                OutputWallpaper {
                    output: "DP-1".into(),
                    ..OutputWallpaper::default()
                },
            ],
            ..WallpaperConfig::default()
        };
        assert!(crate::config::Config {
            wallpaper: cfg.clone(),
            ..Default::default()
        }
        .validate()
        .is_err());
        cfg.outputs = vec![OutputWallpaper {
            output: "eDP/1".into(),
            ..OutputWallpaper::default()
        }];
        assert!(crate::config::Config {
            wallpaper: cfg,
            ..Default::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn every_tool_gets_this_screens_geometry() {
        let out = Output {
            name: "DP-1".into(),
            x: 1920,
            y: -40,
            width: 1080,
            height: 1920,
        };
        let path = Path::new("/w/a b.png");
        for tool in TOOLS {
            for scale in ["fill", "fit", "stretch", "center"] {
                let args = tool.image_args(scale, &out.geometry(), path);
                if *tool == Tool::Feh {
                    assert!(
                        !args.iter().any(|a| a == "1080x1920+1920+-40"),
                        "feh treats geometry as an image path"
                    );
                } else {
                    assert!(
                        args.iter().any(|a| a == "1080x1920+1920+-40"),
                        "{tool:?} {scale}"
                    );
                }
                assert!(args.iter().any(|a| a == "/w/a b.png"));
            }
        }
    }

    #[test]
    fn an_empty_or_inherit_entry_is_not_an_override() {
        assert!(OutputWallpaper::default().is_inherit());
        let scaled = OutputWallpaper {
            scale: "fit".into(),
            ..OutputWallpaper::default()
        };
        assert!(scaled.is_inherit());
        let theme = OutputWallpaper {
            mode: "theme".into(),
            ..OutputWallpaper::default()
        };
        assert!(!theme.is_inherit());
    }

    #[test]
    fn a_reload_keeps_each_screens_own_picture() {
        // A reload with no fake setter is checked through the plan: the own
        // picture wins on its screen and the theme on the other one.
        let cfg = WallpaperConfig {
            enabled: true,
            follow_theme: true,
            outputs: vec![OutputWallpaper {
                output: "DP-1".into(),
                mode: "single".into(),
                file: "/w/own.png".into(),
                scale: String::new(),
            }],
            ..WallpaperConfig::default()
        };
        let mut w = Wallpaper::new(&cfg, "niru-test-missing-wallpaper");
        w.set_outputs(vec![
            Output {
                name: "eDP-1".into(),
                x: 0,
                y: 0,
                width: 1600,
                height: 900,
            },
            Output {
                name: "DP-1".into(),
                x: 1600,
                y: 0,
                width: 1080,
                height: 1920,
            },
        ]);
        // Missing theme pictures fall back to the theme colour, which makes
        // the apply harmless without a background tool.
        let plan = w.plan();
        assert_eq!(plan[0].kind, "colour");
        assert_eq!(plan[1].kind, "image");
        assert!(plan[1].path.ends_with("own.png"));
    }

    #[test]
    fn a_screen_that_reappears_keeps_its_picture_between_changes() {
        let cfg = WallpaperConfig {
            enabled: true,
            outputs: vec![OutputWallpaper {
                output: "HDMI-1".into(),
                mode: "single".into(),
                file: "/w/a.png".into(),
                scale: String::new(),
            }],
            ..WallpaperConfig::default()
        };
        let mut w = Wallpaper::new(&cfg, "niru-nord");
        // Before the screen exists the remembered list is empty; when it
        // appears it is planned from its saved entry.
        w.set_outputs(vec![Output {
            name: "HDMI-1".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        }]);
        let plan = w.plan();
        assert_eq!(plan[0].kind, "image");
        assert!(plan[0].path.ends_with("a.png"));
    }
}
