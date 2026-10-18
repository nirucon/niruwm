//! Which bindings a keyboard can actually deliver.
//!
//! A binding names a keysym, and the keymap maps keysyms to keycodes. A keysym
//! the keyboard does not produce can never be pressed, and a binding for it is
//! silently dead. `niruwm --doctor` reports those, which is the difference
//! between "the media keys are broken" and "this keyboard has no mute key".

use crate::{bindings, config::Config};
use x11rb::{connection::Connection, protocol::xproto::*, rust_connection::RustConnection};

/// Resolve the configured bindings against the live keymap.
///
/// Returns the number of bindings the keyboard can deliver, and the key names
/// it cannot. `None` when there is no display to ask.
pub fn resolve(cfg: &Config) -> Option<(usize, Vec<String>)> {
    let (c, _) = x11rb::connect(None).ok()?;
    Some(resolve_with(&c, cfg))
}

fn resolve_with(c: &RustConnection, cfg: &Config) -> (usize, Vec<String>) {
    let setup = c.setup();
    let count = setup.max_keycode - setup.min_keycode + 1;
    let Ok(cookie) = c.get_keyboard_mapping(setup.min_keycode, count) else {
        return (0, Vec::new());
    };
    let Ok(map) = cookie.reply() else {
        return (0, Vec::new());
    };
    let per = map.keysyms_per_keycode as usize;

    let mut present = 0;
    let mut missing = Vec::new();
    for raw in cfg.effective_keys() {
        let Ok((_, key)) = bindings::parse_keys(&raw.keys) else {
            continue;
        };
        let Some(sym) = crate::wm::keysym_for(&key) else {
            missing.push(raw.keys.clone());
            continue;
        };
        // Every level of every key counts: a media key sits on the first level,
        // but a shifted character does not.
        let found = map.keysyms.chunks(per).any(|levels| levels.contains(&sym));
        if found {
            present += 1;
        } else {
            missing.push(raw.keys.clone());
        }
    }
    (present, missing)
}
