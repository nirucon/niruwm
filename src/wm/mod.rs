mod clients;
mod events;
mod keys;

/// Exposed for `--doctor`, which reports which keys the layout can deliver.
pub fn keysym_for(name: &str) -> Option<u32> {
    keys::keysym_named(name)
}
mod randr;

use crate::bindings::Binding;
use crate::config::{Config, Result, Theme};
use crate::layout::{Layout, Rect};
use crate::monitor::{Monitor, MonitorConfig};
use crate::wallpaper::Wallpaper;
use std::{
    collections::{HashMap, HashSet},
    process::{Child, Command},
};
use x11rb::{
    connection::Connection,
    protocol::{xproto::*, Event},
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};

#[derive(Clone)]
pub(crate) struct Client {
    win: Window,
    /// Which screen the window belongs to.
    monitor: usize,
    tag: u32,
    floating: bool,
    fullscreen: bool,
    geom: Rect,
    /// Set for windows that asked for WM_TAKE_FOCUS; focus is then delivered
    /// as a message rather than with SetInputFocus.
    take_focus: bool,
    /// The process that asked to be opened, for the session file.
    ///
    /// X11 has no "what program is this window"; `_NET_WM_PID` is set by
    /// toolkits and is what every desktop reads to answer the question.
    /// Without it, a saved session could not say what to start again.
    pid: Option<u32>,
}

pub(crate) struct Wm {
    pub(crate) c: RustConnection,
    pub(crate) root: Window,
    pub(crate) screen_w: u16,
    pub(crate) screen_h: u16,
    pub(crate) atoms: HashMap<&'static str, Atom>,
    pub(crate) clients: Vec<Client>,
    pub(crate) focus: Option<Window>,
    pub(crate) tag: u32,
    pub(crate) cfg: Config,
    pub(crate) theme: Theme,
    pub(crate) layout: Layout,
    /// One bar per monitor, in monitor order.
    pub(crate) bars: Vec<Child>,
    pub(crate) hidden: HashSet<Window>,
    pub(crate) pending_unmaps: HashMap<Window, u32>,
    /// Resolved key bindings. Built once and rebuilt on MappingNotify or
    /// reload, so a key press costs a lookup, never an X round trip.
    pub(crate) bindings: Vec<Binding>,
    /// Wallpaper owner. Only active when `[wallpaper] enabled = true`.
    pub(crate) wallpaper: Wallpaper,
    /// Connected screens. A window belongs to the screen it was opened on.
    pub(crate) monitors: Vec<Monitor>,
    /// True when more than one screen is active.
    pub(crate) multi_monitor: bool,
    /// Which screen has the input focus.
    pub(crate) focused_monitor: usize,
    pub(crate) monitor_cfg: MonitorConfig,
    /// Scratchpad window and whether it is currently shown.
    pub(crate) scratchpad: Option<Window>,
    pub(crate) scratchpad_shown: bool,
    /// True while a lock screen owns the keyboard. The WM releases its key
    /// grabs so every key reaches the lock, then takes them back.
    pub(crate) keys_released: bool,
    /// When the bar was last started, to pace restarts.
    pub(crate) last_bar_start: std::time::Instant,
}

impl Wm {
    pub(crate) fn atom(&self, n: &str) -> Atom {
        self.atoms[n]
    }

    pub(crate) fn prop(&self, w: Window, n: &str, t: Atom, v: &[u32]) -> Result<()> {
        self.c
            .change_property32(PropMode::REPLACE, w, self.atom(n), t, v)?;
        Ok(())
    }

    pub(crate) fn new() -> Result<Self> {
        let cfg = Config::load()?;
        let theme = Theme::load(&cfg.theme)?;
        let wallpaper = Wallpaper::new(&cfg.wallpaper);
        let monitor_cfg = MonitorConfig {
            workspaces: cfg.monitors.workspaces,
            named: cfg
                .monitors
                .profiles
                .iter()
                .map(|p| (p.name.clone(), p.workspaces.clone()))
                .collect(),
            primary: cfg
                .monitors
                .profiles
                .iter()
                .find(|p| p.primary)
                .map(|p| p.name.clone()),
        };
        let (c, s) = x11rb::connect(None)?;
        let root = c.setup().roots[s].root;
        let screen_w = c.setup().roots[s].width_in_pixels;
        let screen_h = c.setup().roots[s].height_in_pixels;

        // SubstructureRedirect is the standard "no other WM" lock. It fails
        // cleanly when another window manager owns the display, so the session
        // returns to SDDM instead of drawing over another desktop.
        c.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::SUBSTRUCTURE_REDIRECT
                    | EventMask::SUBSTRUCTURE_NOTIFY
                    | EventMask::PROPERTY_CHANGE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::KEY_PRESS
                    | EventMask::BUTTON_PRESS
                    | EventMask::POINTER_MOTION
                    | EventMask::ENTER_WINDOW
                    | EventMask::FOCUS_CHANGE,
            ),
        )?
        .check()
        .map_err(|e| {
            eprintln!("niruwm: cannot take over the display: {e:?}");
            "Another window manager is already running on this display"
        })?;

        let mut atoms = HashMap::new();
        for n in [
            "_NET_SUPPORTED",
            "_NET_SUPPORTING_WM_CHECK",
            "_NET_WM_NAME",
            "UTF8_STRING",
            "WM_NAME",
            "_NET_CLIENT_LIST",
            "_NET_CLIENT_LIST_STACKING",
            "_NET_ACTIVE_WINDOW",
            "_NET_CURRENT_DESKTOP",
            "_NET_NUMBER_OF_DESKTOPS",
            "_NET_DESKTOP_NAMES",
            "_NET_WM_DESKTOP",
            "_NET_WM_WINDOW_TYPE",
            "_NET_WM_WINDOW_TYPE_DOCK",
            "_NET_WM_WINDOW_TYPE_DIALOG",
            "_NET_WM_WINDOW_TYPE_UTILITY",
            "_NET_WM_WINDOW_TYPE_SPLASH",
            "_NET_WM_STATE",
            "_NET_WM_STATE_FULLSCREEN",
            "_NET_WM_STATE_HIDDEN",
            "_NET_WM_STATE_ABOVE",
            "_NET_WM_STATE_STICKY",
            "_NET_WM_STRUT_PARTIAL",
            "_NET_WM_STRUT",
            "_NET_WM_PID",
            "WM_PROTOCOLS",
            "WM_DELETE_WINDOW",
            "WM_STATE",
            "WM_TAKE_FOCUS",
            "_NIRUWM_COMMAND",
            "_NIRUWM_BAR_CONTROL",
            "_NIRUWM_BAR_RESTART_SERIAL",
            "_NIRUWM_QUIT",
            "_NIRUWM_LOCK",
        ] {
            atoms.insert(n, c.intern_atom(false, n.as_bytes())?.reply()?.atom);
        }

        let mut wm = Self {
            c,
            root,
            screen_w,
            screen_h,
            atoms,
            clients: vec![],
            focus: None,
            tag: 0,
            cfg,
            theme,
            layout: Layout::Tile,
            bars: Vec::new(),
            hidden: HashSet::new(),
            pending_unmaps: HashMap::new(),
            bindings: Vec::new(),
            wallpaper,
            monitors: Vec::new(),
            multi_monitor: false,
            focused_monitor: 0,
            monitor_cfg,
            scratchpad: None,
            scratchpad_shown: false,
            keys_released: false,
            last_bar_start: std::time::Instant::now(),
        };
        let (monitors, multi) = randr::read(&wm.c, root, screen_w, screen_h, &wm.monitor_cfg);
        wm.focused_monitor = crate::monitor::primary(&monitors, &wm.monitor_cfg);
        wm.monitors = monitors;
        wm.multi_monitor = multi;
        wm.announce()?;
        wm.resolve_bindings()?;
        wm.grab()?;
        let existing = wm.c.query_tree(root)?.reply()?.children;
        let viewable: Vec<Window> = existing
            .into_iter()
            .filter(|w| {
                wm.c.get_window_attributes(*w)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map(|a| a.map_state == MapState::VIEWABLE && !a.override_redirect)
                    .unwrap_or(false)
            })
            .collect();
        for w in viewable {
            wm.manage(w)?;
        }
        wm.arrange()?;
        wm.start_bar();
        // Set the wallpaper once the WM owns the display; a failure here must
        // not stop the session, so it is reported and ignored.
        if let Err(e) = wm.wallpaper.apply() {
            eprintln!("niruwm: wallpaper: {e}");
        }
        // Bring back what was open last time, when the user asked for it. A
        // failure here costs comfort, never the session.
        if wm.cfg.restore_session {
            if let Err(e) = wm.restore_session() {
                eprintln!("niruwm: could not restore the session: {e}");
            }
        }
        Ok(wm)
    }

    /// Advertise EWMH support. Panels (NIRUBAR, polybar), pagers and
    /// `wmctrl` all decide what to show from `_NET_SUPPORTED`.
    fn announce(&self) -> Result<()> {
        let check = self.c.generate_id()?;
        self.c.create_window(
            0,
            check,
            self.root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            // No event mask is needed: XSendEvent with an empty mask delivers
            // to the client that created the window (this WM), exactly once.
            &CreateWindowAux::new(),
        )?;
        self.prop(
            self.root,
            "_NET_SUPPORTING_WM_CHECK",
            AtomEnum::WINDOW.into(),
            &[check],
        )?;
        self.prop(
            check,
            "_NET_SUPPORTING_WM_CHECK",
            AtomEnum::WINDOW.into(),
            &[check],
        )?;
        self.prop(
            check,
            "_NIRUWM_BAR_CONTROL",
            AtomEnum::CARDINAL.into(),
            &[1],
        )?;
        self.c.change_property8(
            PropMode::REPLACE,
            check,
            self.atom("_NET_WM_NAME"),
            self.atom("UTF8_STRING"),
            format!("NIRUWM {}", env!("CARGO_PKG_VERSION")).as_bytes(),
        )?;
        let supported: Vec<u32> = self
            .atoms
            .iter()
            .filter(|(n, _)| n.starts_with("_NET_") && **n != "_NET_SUPPORTED")
            .map(|(_, a)| *a)
            .collect();
        self.prop(
            self.root,
            "_NET_SUPPORTED",
            AtomEnum::ATOM.into(),
            &supported,
        )?;
        self.prop(
            self.root,
            "_NET_NUMBER_OF_DESKTOPS",
            AtomEnum::CARDINAL.into(),
            &[9],
        )?;
        self.c.change_property8(
            PropMode::REPLACE,
            self.root,
            self.atom("_NET_DESKTOP_NAMES"),
            self.atom("UTF8_STRING"),
            b"1\x002\x003\x004\x005\x006\x007\x008\x009\x00",
        )?;
        Ok(())
    }

    /// Re-read the screens after a hotplug or resolution change.
    ///
    /// Windows on a screen that disappeared move to the first remaining one,
    /// and each surviving screen keeps its active workspace.
    /// Restart the bars after a monitor change.
    ///
    /// A bar is tied to one screen's geometry, so a hotplug needs new bars
    /// rather than moved ones: the width and the strut both change.
    pub(crate) fn restart_bars_if_owning(&mut self) {
        self.start_bar();
    }

    pub(crate) fn refresh_monitors(&mut self) -> Result<()> {
        let (monitors, multi) = randr::refresh(
            &self.c,
            self.root,
            self.screen_w,
            self.screen_h,
            &self.monitor_cfg,
            &self.monitors,
        );
        let count = monitors.len().max(1);
        for c in self.clients.iter_mut() {
            if c.monitor >= count {
                c.monitor = count - 1;
                if let Some(m) = monitors.get(c.monitor) {
                    c.tag = m.active as u32;
                }
            } else if let Some(m) = monitors.get(c.monitor) {
                // The screen may now have fewer workspaces than before.
                if (c.tag as usize) >= m.workspaces.len() {
                    c.tag = m.active as u32;
                }
            }
        }
        if self.focused_monitor >= count {
            self.focused_monitor = count - 1;
        }
        self.monitors = monitors;
        self.multi_monitor = multi;
        self.restart_bars_if_owning();
        // Focus followed the pointer at login; after a hotplug a window is
        // more useful than an empty screen.
        let active = self
            .monitors
            .get(self.focused_monitor)
            .map(|m| m.active as u32)
            .unwrap_or(0);
        let target = self
            .clients
            .iter()
            .find(|c| c.monitor == self.focused_monitor && c.tag == active);
        if let Some(c) = target {
            let w = c.win;
            self.set_focus(w)?;
        }
        self.arrange()
    }

    /// The WM alone owns bars in session and standalone launches.
    fn start_bar(&mut self) {
        self.stop_bars();
        self.last_bar_start = std::time::Instant::now();
        if !self.cfg.bar.enabled {
            return;
        }
        // One bar per monitor. Each is told where it belongs, so it covers that
        // screen only and its workspace cells describe that screen.
        for m in &self.monitors {
            let geometry = format!("{},{},{}", m.rect.x, m.rect.y, m.rect.w);
            match Command::new("nirubar")
                .arg("--monitor")
                .arg(geometry)
                .spawn()
            {
                Ok(child) => self.bars.push(child),
                Err(e) => eprintln!("niruwm: cannot start nirubar: {e}"),
            }
        }
    }

    /// Stop every bar and wait for it, so none survives a reload or logout.
    pub(crate) fn stop_bars(&mut self) {
        for mut b in self.bars.drain(..) {
            let _ = b.kill();
            let _ = b.wait();
        }
    }

    /// Start a bar again after it died on its own.
    ///
    /// A bar is not optional furniture: without it the workspaces are invisible
    /// and the session looks broken. It died for some reason, so the restart is
    /// paced: at most one attempt every ten seconds, and the attempts are
    /// logged, so a bar that cannot start cannot turn into a loop.
    pub(crate) fn restart_bars(&mut self) {
        if !self.cfg.bar.enabled {
            return;
        }
        let now = std::time::Instant::now();
        if now.duration_since(self.last_bar_start).as_secs() < 10 {
            return;
        }
        let mut dead = self.bars.is_empty();
        for child in &mut self.bars {
            if let Ok(Some(status)) = child.try_wait() {
                eprintln!("niruwm: the bar exited ({status}); starting it again");
                dead = true;
            }
        }
        if !dead {
            return;
        }
        self.last_bar_start = now;
        self.stop_bars();
        self.start_bar();
    }

    /// Reload configuration, theme and bindings. A broken file keeps the
    /// working runtime instead of leaving a half-applied state.
    pub(crate) fn reload(&mut self) -> Result<()> {
        let cfg = match Config::load() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("niruwm: reload rejected: {e}");
                return Ok(());
            }
        };
        let theme = match Theme::load(&cfg.theme) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("niruwm: reload rejected: {e}");
                return Ok(());
            }
        };
        let old_height = self.cfg.bar_height;
        let old_enabled = self.cfg.bar.enabled;
        self.cfg = cfg;
        self.theme = theme;
        self.resolve_bindings()?;
        self.grab()?;
        if self.cfg.bar_height != old_height || self.cfg.bar.enabled != old_enabled {
            self.start_bar();
        }
        self.update_borders()?;
        Ok(())
    }

    pub(crate) fn start(&mut self, program: &[String]) {
        self.spawn_owned(program);
    }

    pub(crate) fn spawn_owned(&mut self, program: &[String]) {
        if let Some((cmd, args)) = program.split_first() {
            match Command::new(cmd).args(args).spawn() {
                Ok(mut child) => {
                    // Reap in a thread: a long-running app must not become a
                    // zombie, and the event loop must not block on it.
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                Err(e) => {
                    eprintln!("niruwm: cannot start {cmd}: {e}");
                    Self::missing_program(cmd, &e);
                }
            }
        }
    }

    /// Tell the person when a binding failed, not only the log.
    ///
    /// A keyboard shortcut that does nothing looks like a broken window
    /// manager. The name of the missing program, in a notification, is the
    /// difference between "this desktop is broken" and "install kitty".
    fn missing_program(cmd: &str, e: &std::io::Error) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return;
        }
        // The command name comes from the config; strip anything that could
        // end the shell string early.
        let cmd: String = cmd
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || "-_./".contains(*c))
            .collect();
        let body = format!("{cmd} is not installed; the key needs it");
        Self::spawn_detached(&[
            "sh".into(),
            "-c".into(),
            format!("notify-send -a NIRUWM 'Missing program' '{body}' 2>/dev/null || true"),
        ]);
    }

    /// Start a detached program without waiting for it at all.
    pub(crate) fn spawn_detached(program: &[String]) {
        if let Some((cmd, args)) = program.split_first() {
            if let Err(e) = Command::new(cmd).args(args).spawn() {
                eprintln!("niruwm: cannot start {cmd}: {e}");
            }
        }
    }
}

impl Drop for Wm {
    fn drop(&mut self) {
        self.stop_bars();
    }
}
