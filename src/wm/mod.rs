mod clients;
mod events;
mod keys;

use crate::bindings::Binding;
use crate::config::{Config, Result, Theme};
use crate::layout::{Layout, Rect};
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
    tag: u32,
    floating: bool,
    fullscreen: bool,
    geom: Rect,
    /// Set for windows that asked for WM_TAKE_FOCUS; focus is then delivered
    /// as a message rather than with SetInputFocus.
    take_focus: bool,
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
    pub(crate) bar: Option<Child>,
    pub(crate) hidden: HashSet<Window>,
    pub(crate) pending_unmaps: HashMap<Window, u32>,
    /// Resolved key bindings. Built once and rebuilt on MappingNotify or
    /// reload, so a key press costs a lookup, never an X round trip.
    pub(crate) bindings: Vec<Binding>,
    /// Windows in the master column (dwm's nmaster).
    pub(crate) nmaster: usize,
    /// Wallpaper owner. Only active when `[wallpaper] enabled = true`.
    pub(crate) wallpaper: Wallpaper,
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
        .map_err(|_| "Another window manager is already running on this display")?;

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
            "_NIRUWM_QUIT",
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
            bar: None,
            hidden: HashSet::new(),
            pending_unmaps: HashMap::new(),
            bindings: Vec::new(),
            nmaster: 1,
            wallpaper,
        };
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

    /// Start the bar from PATH unless a session script already did.
    ///
    /// `niruwm-session` starts the bar before the WM so the strut exists
    /// before the first window is tiled, and sets `NIRUWM_SESSION_BAR`.
    /// Running `niruwm` directly (nested tests, tty start) still gets a bar.
    fn start_bar(&mut self) {
        if !self.cfg.bar.enabled {
            return;
        }
        if std::env::var_os("NIRUWM_SESSION_BAR").is_some() {
            return;
        }
        match Command::new("nirubar").spawn() {
            Ok(child) => self.bar = Some(child),
            Err(e) => eprintln!("niruwm: cannot start nirubar: {e}"),
        }
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
        self.cfg = cfg;
        self.theme = theme;
        self.resolve_bindings()?;
        self.grab()?;
        if self.cfg.bar_height != old_height {
            if let Some(mut b) = self.bar.take() {
                let _ = b.kill();
                let _ = b.wait();
            }
            std::env::remove_var("NIRUWM_SESSION_BAR");
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
                Err(e) => eprintln!("niruwm: cannot start {cmd}: {e}"),
            }
        }
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
        if let Some(b) = &mut self.bar {
            let _ = b.kill();
            let _ = b.wait();
        }
    }
}
