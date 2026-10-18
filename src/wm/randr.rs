//! Reading the physical screens from RandR.
//!
//! Only the monitor list is taken from RandR; the rest of the window manager
//! works in plain screen coordinates that the monitor list maps back to a
//! screen. That keeps the tiling code free of protocol details.

use crate::{
    layout::Rect,
    monitor::{self, Monitor, MonitorConfig},
};
use x11rb::{protocol::randr::ConnectionExt as _, rust_connection::RustConnection};

/// Query the connected outputs and build the monitor list.
///
/// A server without RandR, or one that reports no active outputs, falls back to
/// a single monitor covering the whole screen: the window manager keeps working
/// instead of refusing to start.
pub fn read(
    c: &RustConnection,
    root: x11rb::protocol::xproto::Window,
    screen_w: u16,
    screen_h: u16,
    cfg: &MonitorConfig,
) -> (Vec<Monitor>, bool) {
    let outputs = match read_randr(c, root) {
        Some(o) if !o.is_empty() => o,
        _ => Vec::new(),
    };
    if outputs.is_empty() {
        let (m, many) = monitor::build(
            vec![(
                "screen".into(),
                Rect {
                    x: 0,
                    y: 0,
                    w: screen_w,
                    h: screen_h,
                },
                true,
            )],
            cfg,
        );
        return (m, many);
    }
    monitor::build(outputs, cfg)
}

fn read_randr(
    c: &RustConnection,
    root: x11rb::protocol::xproto::Window,
) -> Option<Vec<(String, Rect, bool)>> {
    let resources = c
        .randr_get_screen_resources_current(root)
        .ok()?
        .reply()
        .ok()?;
    let mut out = Vec::new();
    for output in resources.outputs {
        let info = c
            .randr_get_output_info(output, resources.config_timestamp)
            .ok()?
            .reply()
            .ok()?;
        // Only outputs with a current mode are usable.
        if info.connection != x11rb::protocol::randr::Connection::CONNECTED || info.crtc == 0 {
            continue;
        }
        let name = String::from_utf8_lossy(&info.name).to_string();
        let crtc = c
            .randr_get_crtc_info(info.crtc, resources.config_timestamp)
            .ok()?
            .reply()
            .ok()?;
        if crtc.width == 0 || crtc.height == 0 {
            continue;
        }
        out.push((
            name,
            Rect {
                x: crtc.x,
                y: crtc.y,
                w: crtc.width,
                h: crtc.height,
            },
            true,
        ));
    }
    Some(out)
}

/// Rebuild the monitor list, keeping each screen's active workspace where the
/// screen still exists. Hotplugging a cable must not send anyone back to
/// workspace 1 on the screen that stayed connected.
pub fn refresh(
    c: &RustConnection,
    root: x11rb::protocol::xproto::Window,
    screen_w: u16,
    screen_h: u16,
    cfg: &MonitorConfig,
    previous: &[Monitor],
) -> (Vec<Monitor>, bool) {
    let (mut monitors, many) = read(c, root, screen_w, screen_h, cfg);
    for m in monitors.iter_mut() {
        if let Some(old) = previous.iter().find(|p| p.name == m.name) {
            // Keep the workspace if it still exists on this screen.
            if old.active < m.workspaces.len() {
                m.active = old.active;
            }
        }
    }
    (monitors, many)
}
