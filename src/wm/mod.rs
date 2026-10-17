mod events;
mod keys;
use crate::config::{Config, Result, Theme};
use crate::layout::{Layout, Rect};
use std::{
    collections::HashMap,
    process::{Child, Command},
};
use x11rb::{
    connection::Connection,
    protocol::{xproto::*, Event},
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};
#[derive(Clone)]
struct Client {
    win: Window,
    tag: u32,
    floating: bool,
    fullscreen: bool,
    geom: Rect,
}
pub(crate) struct Wm {
    c: RustConnection,
    root: Window,
    atoms: HashMap<&'static str, Atom>,
    clients: Vec<Client>,
    focus: Option<Window>,
    tag: u32,
    cfg: Config,
    theme: Theme,
    layout: Layout,
    bar: Option<Child>,
    hidden: std::collections::HashSet<Window>,
    pending_unmaps: HashMap<Window, u32>,
}
impl Wm {
    fn atom(&self, n: &str) -> Atom {
        self.atoms[n]
    }
    fn prop(&self, w: Window, n: &str, t: Atom, v: &[u32]) -> Result<()> {
        self.c
            .change_property32(PropMode::REPLACE, w, self.atom(n), t, v)?;
        Ok(())
    }
    pub(crate) fn new() -> Result<Self> {
        let cfg = Config::load()?;
        let theme = Theme::load(&cfg.theme)?;
        let (c, s) = x11rb::connect(None)?;
        let root = c.setup().roots[s].root;
        c.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(
                EventMask::SUBSTRUCTURE_REDIRECT
                    | EventMask::SUBSTRUCTURE_NOTIFY
                    | EventMask::PROPERTY_CHANGE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::KEY_PRESS,
            ),
        )?
        .check()
        .map_err(|_| "Another window manager is running; choose a separate X session")?;
        let mut atoms = HashMap::new();
        for n in [
            "_NET_SUPPORTED",
            "_NET_SUPPORTING_WM_CHECK",
            "_NET_WM_NAME",
            "UTF8_STRING",
            "_NET_CLIENT_LIST",
            "_NET_ACTIVE_WINDOW",
            "_NET_CURRENT_DESKTOP",
            "_NET_NUMBER_OF_DESKTOPS",
            "_NET_DESKTOP_NAMES",
            "_NET_WM_DESKTOP",
            "_NET_WM_WINDOW_TYPE",
            "_NET_WM_WINDOW_TYPE_DOCK",
            "_NET_WM_WINDOW_TYPE_DIALOG",
            "_NET_WM_STATE",
            "_NET_WM_STATE_FULLSCREEN",
            "WM_PROTOCOLS",
            "WM_DELETE_WINDOW",
            "WM_STATE",
            "WM_TAKE_FOCUS",
            "_NIRUWM_COMMAND",
        ] {
            atoms.insert(n, c.intern_atom(false, n.as_bytes())?.reply()?.atom);
        }
        let mut wm = Self {
            c,
            root,
            atoms,
            clients: vec![],
            focus: None,
            tag: 0,
            cfg,
            theme,
            layout: Layout::Tile,
            bar: None,
            hidden: Default::default(),
            pending_unmaps: HashMap::new(),
        };
        let check = wm.c.generate_id()?;
        wm.c.create_window(
            0,
            check,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )?;
        wm.prop(
            root,
            "_NET_SUPPORTING_WM_CHECK",
            AtomEnum::WINDOW.into(),
            &[check],
        )?;
        wm.prop(
            check,
            "_NET_SUPPORTING_WM_CHECK",
            AtomEnum::WINDOW.into(),
            &[check],
        )?;
        wm.c.change_property8(
            PropMode::REPLACE,
            check,
            wm.atom("_NET_WM_NAME"),
            wm.atom("UTF8_STRING"),
            b"NIRUWM",
        )?;
        wm.prop(
            root,
            "_NET_SUPPORTED",
            AtomEnum::ATOM.into(),
            &wm.atoms
                .iter()
                .filter(|(n, _)| n.starts_with("_NET_"))
                .map(|(_, a)| *a)
                .collect::<Vec<_>>(),
        )?;
        wm.prop(
            root,
            "_NET_NUMBER_OF_DESKTOPS",
            AtomEnum::CARDINAL.into(),
            &[9],
        )?;
        wm.c.change_property8(
            PropMode::REPLACE,
            root,
            wm.atom("_NET_DESKTOP_NAMES"),
            wm.atom("UTF8_STRING"),
            b"1\x002\x003\x004\x005\x006\x007\x008\x009\x00",
        )?;
        wm.grab()?;
        let children = wm.c.query_tree(root)?.reply()?.children;
        for w in children {
            let attr = wm.c.get_window_attributes(w)?.reply();
            if let Ok(a) = attr {
                if a.map_state == MapState::VIEWABLE && !a.override_redirect {
                    wm.manage(w)?;
                }
            }
        }
        wm.arrange()?;
        wm.bar = Command::new("nirubar").spawn().ok();
        Ok(wm)
    }
    fn grab(&self) -> Result<()> {
        self.c.ungrab_key(0, self.root, ModMask::ANY)?;
        let setup = self.c.setup();
        let map = self
            .c
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)?
            .reply()?;
        for (i, syms) in map
            .keysyms
            .chunks(map.keysyms_per_keycode as usize)
            .enumerate()
        {
            if syms.iter().any(|s| {
                matches!(
                    *s,
                    0x31..=0x39
                        | 0xff0d
                        | 0xff09
                        | 0x6a
                        | 0x6b
                        | 0x71
                        | 0x74
                        | 0x6d
                        | 0x66
                        | 0x70
                        | 0x68
                        | 0x6c
                        | 0x72
                        | 0x20
                        | 0x79
                        | 0x63
                        | 0x65
                        | 0x7a
                        | 0x73
                )
            }) {
                for extra in [0, 2, 16, 18] {
                    for action in [0, 1, 4, 5] {
                        self.c.grab_key(
                            false,
                            self.root,
                            (u16::from(ModMask::M4) | extra | action).into(),
                            (i + setup.min_keycode as usize) as u8,
                            GrabMode::ASYNC,
                            GrabMode::ASYNC,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
    fn values(&self, w: Window, n: &str) -> Vec<u32> {
        self.c
            .get_property(false, w, self.atom(n), AtomEnum::ANY, 0, 64)
            .ok()
            .and_then(|r| r.reply().ok())
            .and_then(|r| r.value32().map(|v| v.collect()))
            .unwrap_or_default()
    }
    fn manage(&mut self, w: Window) -> Result<()> {
        if self.clients.iter().any(|c| c.win == w) {
            return Ok(());
        }
        let a = self.c.get_window_attributes(w)?.reply()?;
        if a.override_redirect {
            return Ok(());
        }
        if self
            .values(w, "_NET_WM_WINDOW_TYPE")
            .contains(&self.atom("_NET_WM_WINDOW_TYPE_DOCK"))
        {
            self.c.map_window(w)?;
            return Ok(());
        }
        let g = self.c.get_geometry(w)?.reply()?;
        let transient = self
            .c
            .get_property(false, w, AtomEnum::WM_TRANSIENT_FOR, AtomEnum::WINDOW, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut v| v.next());
        let floating = transient.is_some()
            || self
                .values(w, "_NET_WM_WINDOW_TYPE")
                .contains(&self.atom("_NET_WM_WINDOW_TYPE_DIALOG"));
        self.c.change_window_attributes(
            w,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::ENTER_WINDOW | EventMask::PROPERTY_CHANGE),
        )?;
        self.clients.push(Client {
            win: w,
            tag: self.tag,
            floating,
            fullscreen: false,
            geom: Rect {
                x: g.x,
                y: g.y,
                w: g.width,
                h: g.height,
            },
        });
        self.prop(w, "WM_STATE", self.atom("WM_STATE"), &[1, 0])?;
        self.prop(w, "_NET_WM_DESKTOP", AtomEnum::CARDINAL.into(), &[self.tag])?;
        self.c.map_window(w)?;
        self.set_focus(w)?;
        self.arrange()
    }
    fn set_focus(&mut self, w: Window) -> Result<()> {
        self.focus = Some(w);
        self.c
            .set_input_focus(InputFocus::POINTER_ROOT, w, x11rb::CURRENT_TIME)?;
        if self
            .values(w, "WM_PROTOCOLS")
            .contains(&self.atom("WM_TAKE_FOCUS"))
        {
            self.c.send_event(
                false,
                w,
                EventMask::NO_EVENT,
                ClientMessageEvent::new(
                    32,
                    w,
                    self.atom("WM_PROTOCOLS"),
                    [self.atom("WM_TAKE_FOCUS"), x11rb::CURRENT_TIME, 0, 0, 0],
                ),
            )?;
        }
        self.prop(
            self.root,
            "_NET_ACTIVE_WINDOW",
            AtomEnum::WINDOW.into(),
            &[w],
        )?;
        for c in &self.clients {
            let color = if c.win == w {
                &self.theme.focus
            } else {
                &self.theme.border
            };
            self.c.change_window_attributes(
                c.win,
                &ChangeWindowAttributesAux::new().border_pixel(Theme::pixel(color)?),
            )?;
        }
        Ok(())
    }
    fn arrange(&mut self) -> Result<()> {
        let screen = self.c.get_geometry(self.root)?.reply()?;
        let area = Rect {
            x: 0,
            y: self.cfg.bar_height as i16,
            w: screen.width,
            h: screen.height.saturating_sub(self.cfg.bar_height),
        };
        let n = self
            .clients
            .iter()
            .filter(|c| c.tag == self.tag && !c.floating && !c.fullscreen)
            .count();
        let boxes = self
            .layout
            .arrange(area, n, self.cfg.master_ratio, self.cfg.gap);
        let mut idx = 0;
        for c in &self.clients {
            if c.tag != self.tag {
                if !self.hidden.contains(&c.win) {
                    self.hidden.insert(c.win);
                    *self.pending_unmaps.entry(c.win).or_default() += 1;
                    self.c.unmap_window(c.win)?;
                }
                continue;
            }
            if self.hidden.remove(&c.win) {
                self.c.map_window(c.win)?;
            }
            let r = if c.fullscreen {
                Rect {
                    x: 0,
                    y: 0,
                    w: screen.width,
                    h: screen.height,
                }
            } else if c.floating || self.layout == Layout::Floating {
                c.geom
            } else {
                let r = boxes[idx];
                idx += 1;
                r
            };
            let b = if c.fullscreen || (!c.floating && (n == 1 || self.layout == Layout::Monocle)) {
                0
            } else {
                self.cfg.border_width
            };
            self.c.configure_window(
                c.win,
                &ConfigureWindowAux::new()
                    .x(i32::from(r.x))
                    .y(i32::from(r.y))
                    .width(u32::from(r.w.saturating_sub(b * 2).max(1)))
                    .height(u32::from(r.h.saturating_sub(b * 2).max(1)))
                    .border_width(u32::from(b)),
            )?;
            if c.fullscreen {
                self.c.configure_window(
                    c.win,
                    &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
                )?;
            }
        }
        let visible = self
            .clients
            .iter()
            .filter(|c| c.tag == self.tag)
            .map(|c| c.win)
            .collect::<Vec<_>>();
        if !self.focus.map(|w| visible.contains(&w)).unwrap_or(false) {
            if let Some(w) = visible.last() {
                self.set_focus(*w)?;
            } else {
                self.focus = None;
                self.c
                    .set_input_focus(InputFocus::POINTER_ROOT, self.root, x11rb::CURRENT_TIME)?;
                self.prop(
                    self.root,
                    "_NET_ACTIVE_WINDOW",
                    AtomEnum::WINDOW.into(),
                    &[],
                )?;
            }
        }
        self.prop(
            self.root,
            "_NET_CURRENT_DESKTOP",
            AtomEnum::CARDINAL.into(),
            &[self.tag],
        )?;
        self.prop(
            self.root,
            "_NET_CLIENT_LIST",
            AtomEnum::WINDOW.into(),
            &self.clients.iter().map(|c| c.win).collect::<Vec<_>>(),
        )?;
        self.c.flush()?;
        Ok(())
    }
    fn spawn(args: &[String]) {
        if let Some(a) = args.first() {
            match Command::new(a).args(&args[1..]).spawn() {
                Ok(mut child) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                Err(e) => eprintln!("spawn {a}: {e}"),
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
