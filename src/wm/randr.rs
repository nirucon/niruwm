//! Reading the physical screens from RandR.
//!
//! Only the monitor list is taken from RandR; the rest of the window manager
//! works in plain screen coordinates that the monitor list maps back to a
//! screen. That keeps the tiling code free of protocol details.

use crate::{
    layout::Rect,
    monitor::{self, Monitor, MonitorConfig},
    wallpaper::Output as WallpaperOutput,
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

/// The connected outputs with their real geometry, in the server's order.
///
/// The wallpaper needs the output names and where each one sits; the monitor
/// list sorts them for tiling, which is not the same thing.
pub fn outputs(c: &RustConnection, root: x11rb::protocol::xproto::Window) -> Vec<WallpaperOutput> {
    if let Some(emulated) = emulated_outputs() {
        return emulated;
    }
    let Some(list) = read_randr(c, root) else {
        return Vec::new();
    };
    list.into_iter()
        .map(|(name, rect, _)| WallpaperOutput {
            name,
            x: i32::from(rect.x),
            y: i32::from(rect.y),
            width: u32::from(rect.w),
            height: u32::from(rect.h),
        })
        .collect()
}

/// Screens from a layout file, for the automated acceptance run.
///
/// Read only when `NIRU_RANDR_EMU` names an existing file, so an ordinary
/// session never consults it. The format is one `NAME WxH+X+Y` per line.
fn emulated_outputs() -> Option<Vec<WallpaperOutput>> {
    let path = std::env::var_os("NIRU_RANDR_EMU")?;
    let text = std::fs::read_to_string(path).ok()?;
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((name, geometry)) = line.trim().split_once(' ') else {
            continue;
        };
        let Some((size, rest)) = geometry.split_once('+') else {
            continue;
        };
        let Some((w, h)) = size.split_once('x') else {
            continue;
        };
        let Some((x, y)) = rest.split_once('+') else {
            continue;
        };
        let (Ok(w), Ok(h), Ok(x), Ok(y)) =
            (w.parse::<u32>(), h.parse::<u32>(), x.parse::<i32>(), y.parse::<i32>())
        else {
            continue;
        };
        out.push(WallpaperOutput {
            name: name.to_string(),
            x,
            y,
            width: w,
            height: h,
        });
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
