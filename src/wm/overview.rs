//! The workspace overview (Super+E) and `niructl do`.
//!
//! When a screen leaves a workspace, before its windows are unmapped, the
//! screen is scaled down on the server with an XRender transform onto a small
//! pixmap (longest side at most `THUMB_MAX` pixels). The composite is queued
//! ahead of the unmaps, so it sees the old workspace; the one small GetImage
//! that reads it back runs after the switch has been sent, so the switch
//! itself only pays for queuing a few requests.
//!
//! Thumbnails are binary PPM (`P6`) files `mon<index>-tag<N>.ppm` in
//! `$XDG_RUNTIME_DIR/niruwm/overview/` (0700, files 0600), next to
//! `state.json`, which `nirulauncher --overview` reads. Without
//! XDG_RUNTIME_DIR nothing is captured or written. Nothing is captured while a
//! lock screen holds the keyboard, and the thumbnails are deleted the moment
//! the screen locks, so screen content never lingers behind a lock.

use super::*;
use crate::bindings::{DoError, DoRequest};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use x11rb::protocol::render::{self, ConnectionExt as _};

/// The longest side of a thumbnail, in pixels.
pub(crate) const THUMB_MAX: u32 = 384;

/// What the overview needs between captures.
#[derive(Default)]
pub(crate) struct Overview {
    /// Per screen: (output name, workspace shown) at the last arrange.
    shown: Vec<(String, usize)>,
    /// XRender is set up (or known to be missing: `broken`).
    render: Option<RenderSetup>,
    broken: bool,
    /// Composited but not yet read back.
    pending: Vec<Pending>,
    /// `NIRUWM_OVERVIEW_TIMING=1` logs what each capture costs.
    timing: Option<bool>,
}

#[derive(Clone, Copy)]
struct RenderSetup {
    /// The root window as a source, including its children.
    source: render::Picture,
    format: render::Pictformat,
    depth: u8,
    bits_per_pixel: u8,
    masks: (u32, u32, u32),
    lsb_first: bool,
}

struct Pending {
    pixmap: Pixmap,
    picture: render::Picture,
    width: u16,
    height: u16,
    file: PathBuf,
}

/// `$XDG_RUNTIME_DIR/niruwm/overview`, or `None` without a runtime directory.
pub(crate) fn overview_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    Some(PathBuf::from(base).join("niruwm").join("overview"))
}

fn ensure_dir(dir: &std::path::Path) -> std::io::Result<()> {
    if !dir.is_dir() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    if let Some(parent) = dir.parent() {
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// The thumbnail file name of a screen and a one-based workspace number.
pub(crate) fn thumb_name(monitor: usize, tag: u32) -> String {
    format!("mon{monitor}-tag{tag}.ppm")
}

/// Thumbnail size for a screen: the longest side `THUMB_MAX`, never larger
/// than the screen itself, never zero.
pub(crate) fn thumb_size(w: u16, h: u16) -> (u16, u16) {
    let (w, h) = (u32::from(w.max(1)), u32::from(h.max(1)));
    let long = w.max(h);
    if long <= THUMB_MAX {
        return (w as u16, h as u16);
    }
    let tw = (w * THUMB_MAX + long / 2) / long;
    let th = (h * THUMB_MAX + long / 2) / long;
    (tw.max(1) as u16, th.max(1) as u16)
}

/// 16.16 fixed point, as XRender transforms want it.
fn fixed(v: f64) -> render::Fixed {
    (v * 65536.0).round() as render::Fixed
}

/// Shift and width of a channel mask, to scale any visual to 8 bits.
fn channel(mask: u32) -> (u32, u32) {
    if mask == 0 {
        return (0, 0);
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    (shift, bits)
}

fn to8(value: u32, (shift, bits): (u32, u32)) -> u8 {
    if bits == 0 {
        return 0;
    }
    let v = (value >> shift) & ((1u32 << bits) - 1);
    if bits >= 8 {
        (v >> (bits - 8)) as u8
    } else {
        ((v * 255) / ((1u32 << bits) - 1)) as u8
    }
}

/// A ZPixmap image (rows padded to 32 bits) as binary PPM.
pub(crate) fn ppm_from_zpixmap(
    data: &[u8],
    width: u16,
    height: u16,
    bits_per_pixel: u8,
    masks: (u32, u32, u32),
    lsb_first: bool,
) -> Option<Vec<u8>> {
    let bytes_pp = match bits_per_pixel {
        32 => 4usize,
        24 => 3,
        16 => 2,
        _ => return None,
    };
    let (w, h) = (usize::from(width), usize::from(height));
    let stride = (w * bytes_pp).div_ceil(4) * 4;
    if data.len() < stride * h {
        return None;
    }
    let (r, g, b) = (channel(masks.0), channel(masks.1), channel(masks.2));
    let header = format!("P6\n{w} {h}\n255\n");
    let mut out = Vec::with_capacity(header.len() + w * h * 3);
    out.extend_from_slice(header.as_bytes());
    for row in 0..h {
        let line = &data[row * stride..row * stride + w * bytes_pp];
        for px in line.chunks_exact(bytes_pp) {
            let mut v = 0u32;
            if lsb_first {
                for (i, byte) in px.iter().enumerate() {
                    v |= u32::from(*byte) << (8 * i);
                }
            } else {
                for byte in px {
                    v = (v << 8) | u32::from(*byte);
                }
            }
            out.extend_from_slice(&[to8(v, r), to8(v, g), to8(v, b)]);
        }
    }
    Some(out)
}

/// Write `bytes` to `path` atomically (temporary file, then rename), 0600.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
    }
    std::fs::rename(&tmp, path)
}

/// Delete every thumbnail and the state file (the screen is locking).
pub(crate) fn delete_thumbnails() {
    let Some(dir) = overview_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if (name.starts_with("mon") && name.contains(".ppm")) || name.starts_with("state.json") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Wm {
    fn overview_timing(&mut self) -> bool {
        *self.overview.timing.get_or_insert_with(|| {
            std::env::var_os("NIRUWM_OVERVIEW_TIMING").is_some_and(|v| v == "1")
        })
    }

    /// Set up XRender once: the root as a source picture and the formats.
    fn overview_render(&mut self) -> Option<RenderSetup> {
        if self.overview.broken {
            return None;
        }
        if let Some(r) = self.overview.render {
            return Some(r);
        }
        let setup = self.render_setup();
        match setup {
            Some(r) => self.overview.render = Some(r),
            None => {
                self.overview.broken = true;
                eprintln!("niruwm: overview thumbnails off: the X server has no usable XRender");
            }
        }
        setup
    }

    fn render_setup(&self) -> Option<RenderSetup> {
        let c = &self.c;
        c.render_query_version(0, 11).ok()?.reply().ok()?;
        let screen = c.setup().roots.iter().find(|s| s.root == self.root)?;
        let visual_id = screen.root_visual;
        let depth = screen.root_depth;
        let visual = screen
            .allowed_depths
            .iter()
            .flat_map(|d| d.visuals.iter())
            .find(|v| v.visual_id == visual_id)?;
        let bits_per_pixel = c
            .setup()
            .pixmap_formats
            .iter()
            .find(|f| f.depth == depth)?
            .bits_per_pixel;
        let formats = c.render_query_pict_formats().ok()?.reply().ok()?;
        let format = formats
            .screens
            .iter()
            .flat_map(|s| s.depths.iter())
            .flat_map(|d| d.visuals.iter())
            .find(|v| v.visual == visual_id)?
            .format;
        let source = c.generate_id().ok()?;
        c.render_create_picture(
            source,
            self.root,
            format,
            &render::CreatePictureAux::new().subwindowmode(SubwindowMode::INCLUDE_INFERIORS),
        )
        .ok()?;
        // Bilinear: about 0.3 ms for a 2560x1440 screen in Xvfb on othala.
        c.render_set_picture_filter(source, b"bilinear", &[]).ok()?;
        Some(RenderSetup {
            source,
            format,
            depth,
            bits_per_pixel,
            masks: (visual.red_mask, visual.green_mask, visual.blue_mask),
            lsb_first: c.setup().image_byte_order == ImageOrder::LSB_FIRST,
        })
    }

    /// Queue a scaled copy of screen `mi` as workspace `tag` (zero-based).
    fn overview_capture(&mut self, mi: usize, tag: usize) {
        if self.keys_released {
            return;
        }
        let Some(dir) = overview_dir() else { return };
        let Some(rect) = self.monitors.get(mi).map(|m| m.rect) else {
            return;
        };
        if rect.w == 0 || rect.h == 0 {
            return;
        }
        let Some(r) = self.overview_render() else {
            return;
        };
        if ensure_dir(&dir).is_err() {
            return;
        }
        let (tw, th) = thumb_size(rect.w, rect.h);
        let timing = self.overview_timing();
        let c = &self.c;
        // Timing only: wait for the server before and after, so the log
        // shows what the capture itself costs the server.
        if timing {
            let _ = c.get_input_focus().map(|c| c.reply());
        }
        let started = std::time::Instant::now();
        let queued = (|| -> std::result::Result<Pending, Box<dyn std::error::Error>> {
            let pixmap = c.generate_id()?;
            c.create_pixmap(r.depth, pixmap, self.root, tw, th)?;
            let picture = c.generate_id()?;
            c.render_create_picture(picture, pixmap, r.format, &Default::default())?;
            let sx = f64::from(rect.w) / f64::from(tw);
            let sy = f64::from(rect.h) / f64::from(th);
            // Destination to source: scale, then move to the screen.
            c.render_set_picture_transform(
                r.source,
                render::Transform {
                    matrix11: fixed(sx),
                    matrix12: 0,
                    matrix13: fixed(f64::from(rect.x)),
                    matrix21: 0,
                    matrix22: fixed(sy),
                    matrix23: fixed(f64::from(rect.y)),
                    matrix31: 0,
                    matrix32: 0,
                    matrix33: fixed(1.0),
                },
            )?;
            c.render_composite(
                render::PictOp::SRC,
                r.source,
                x11rb::NONE,
                picture,
                0,
                0,
                0,
                0,
                0,
                0,
                tw,
                th,
            )?;
            Ok(Pending {
                pixmap,
                picture,
                width: tw,
                height: th,
                file: dir.join(thumb_name(mi, tag as u32 + 1)),
            })
        })();
        if timing {
            let _ = c.get_input_focus().map(|c| c.reply());
            eprintln!(
                "niruwm: overview timing: capture {tw}x{th} {} us",
                started.elapsed().as_micros()
            );
        }
        match queued {
            Ok(p) => self.overview.pending.push(p),
            Err(e) => eprintln!("niruwm: overview capture: {e}"),
        }
    }

    /// Before an arrange: capture each screen that is about to leave the
    /// workspace it showed last time.
    /// Returns when the timing log is on and a workspace changed.
    pub(crate) fn overview_before_arrange(&mut self) -> Option<std::time::Instant> {
        let now: Vec<(String, usize)> = self
            .monitors
            .iter()
            .map(|m| (m.name.clone(), m.active))
            .collect();
        if now == self.overview.shown {
            return None;
        }
        let started = std::time::Instant::now();
        let before = std::mem::take(&mut self.overview.shown);
        for (mi, (name, active)) in now.iter().enumerate() {
            if let Some((old_name, old_active)) = before.get(mi) {
                if old_name == name && old_active != active {
                    self.overview_capture(mi, *old_active);
                }
            }
        }
        self.overview.shown = now;
        self.overview_timing().then_some(started)
    }

    /// `NIRUWM_OVERVIEW_TIMING=1`: how long the switch took until the X
    /// server had done all of it, capture included.
    pub(crate) fn overview_log_switch(&mut self, started: std::time::Instant) {
        let queued = self.overview.pending.len();
        let _ = self.c.get_input_focus().map(|c| c.reply());
        eprintln!(
            "niruwm: overview timing: workspace switch {} us with {queued} capture(s)",
            started.elapsed().as_micros()
        );
    }

    /// After an arrange: read back what was captured and write the files.
    pub(crate) fn overview_after_arrange(&mut self) {
        if self.overview.pending.is_empty() {
            return;
        }
        let timing = self.overview_timing();
        let started = std::time::Instant::now();
        let Some(r) = self.overview.render else {
            return;
        };
        let mut readback = std::time::Duration::ZERO;
        for p in std::mem::take(&mut self.overview.pending) {
            let asked = std::time::Instant::now();
            let image = self
                .c
                .get_image(ImageFormat::Z_PIXMAP, p.pixmap, 0, 0, p.width, p.height, !0)
                .ok()
                .and_then(|cookie| cookie.reply().ok());
            readback += asked.elapsed();
            let _ = self.c.render_free_picture(p.picture);
            let _ = self.c.free_pixmap(p.pixmap);
            // A lock that arrived meanwhile wins: nothing is written.
            if self.keys_released {
                continue;
            }
            let Some(image) = image else { continue };
            let Some(ppm) = ppm_from_zpixmap(
                &image.data,
                p.width,
                p.height,
                r.bits_per_pixel,
                r.masks,
                r.lsb_first,
            ) else {
                continue;
            };
            if let Err(e) = write_private(&p.file, &ppm) {
                eprintln!("niruwm: overview thumbnail {}: {e}", p.file.display());
            }
        }
        if timing {
            eprintln!(
                "niruwm: overview timing: readback {} us, readback and write {} us",
                readback.as_micros(),
                started.elapsed().as_micros()
            );
        }
    }

    /// The screen locked: no thumbnail may outlive this moment.
    pub(crate) fn overview_locked(&mut self) {
        for p in std::mem::take(&mut self.overview.pending) {
            let _ = self.c.render_free_picture(p.picture);
            let _ = self.c.free_pixmap(p.pixmap);
        }
        delete_thumbnails();
    }

    fn window_title(&self, w: Window) -> String {
        for (name, ty) in [("_NET_WM_NAME", "UTF8_STRING"), ("WM_NAME", "STRING")] {
            let reply = self
                .c
                .get_property(false, w, self.atom(name), self.atom(ty), 0, 256)
                .ok()
                .and_then(|c| c.reply().ok());
            if let Some(r) = reply {
                if !r.value.is_empty() {
                    return String::from_utf8_lossy(&r.value).to_string();
                }
            }
        }
        String::new()
    }

    /// The overview state as `state.json` describes it (version 1).
    pub(crate) fn overview_state(&self, dir: &std::path::Path) -> serde_json::Value {
        let mut monitors = Vec::new();
        for (mi, m) in self.monitors.iter().enumerate() {
            let current = m.active as u32;
            let mut tags = Vec::new();
            for (ti, name) in m.workspaces.iter().enumerate() {
                let t = ti as u32;
                let windows: Vec<serde_json::Value> = self
                    .clients
                    .iter()
                    .filter(|c| c.monitor == mi)
                    .filter(|c| self.scratch_owner(c.win).is_none() && c.swallowed_by.is_none())
                    // Sticky windows are on every workspace; list them once,
                    // on the one shown.
                    .filter(|c| if c.sticky { t == current } else { c.tag == t })
                    .map(|c| {
                        let class = c
                            .class
                            .split('\0')
                            .filter(|p| !p.is_empty())
                            .nth(1)
                            .or_else(|| c.class.split('\0').next())
                            .unwrap_or("")
                            .to_string();
                        serde_json::json!({
                            "id": format!("{:#x}", c.win),
                            "class": class,
                            "title": self.window_title(c.win),
                            "focused": self.focus == Some(c.win),
                        })
                    })
                    .collect();
                if windows.is_empty() && t != current {
                    continue;
                }
                let file = thumb_name(mi, t + 1);
                let path = dir.join(&file);
                let captured = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                tags.push(serde_json::json!({
                    "tag": t + 1,
                    "name": name,
                    "thumbnail": if captured.is_some() { file } else { String::new() },
                    "captured": captured.unwrap_or(0),
                    "windows": windows,
                }));
            }
            monitors.push(serde_json::json!({
                "index": mi,
                "name": m.name,
                "x": m.rect.x,
                "y": m.rect.y,
                "width": m.rect.w,
                "height": m.rect.h,
                "focused": mi == self.focused_monitor,
                "current_tag": current + 1,
                "tags": tags,
            }));
        }
        serde_json::json!({"version": 1, "generated": unix_now(), "monitors": monitors})
    }

    /// Super+E: refresh the shown workspaces, write the state, open the
    /// overview.
    pub(crate) fn overview(&mut self) {
        if self.keys_released {
            return;
        }
        if let Some(dir) = overview_dir() {
            for mi in 0..self.monitors.len() {
                let active = self.monitors[mi].active;
                self.overview_capture(mi, active);
            }
            self.overview_after_arrange();
            if ensure_dir(&dir).is_ok() {
                let state = self.overview_state(&dir);
                let text = serde_json::to_vec_pretty(&state).unwrap_or_default();
                if let Err(e) = write_private(&dir.join("state.json"), &text) {
                    eprintln!("niruwm: overview state: {e}");
                }
            }
        }
        if super::keys::which("nirulauncher") {
            self.start(&["nirulauncher".into(), "--overview".into()]);
        } else {
            eprintln!("niruwm: the overview needs nirulauncher");
        }
    }

    /// `niructl do goto MONITOR TAG [WINDOW]`.
    fn goto(
        &mut self,
        monitor: usize,
        tag: u32,
        window: Option<u32>,
    ) -> std::result::Result<(), String> {
        let Some(m) = self.monitors.get_mut(monitor) else {
            return Err(format!("there is no screen {monitor}"));
        };
        let index = tag as usize - 1;
        if index >= m.workspaces.len() {
            return Err(format!("screen {monitor} has no workspace {tag}"));
        }
        m.active = index;
        self.focused_monitor = monitor;
        self.follow_scratch();
        let run = |wm: &mut Self| -> Result<()> {
            let here = window.and_then(|w| {
                wm.clients
                    .iter()
                    .find(|c| c.win == w)
                    .map(|c| c.monitor == monitor && (c.tag == index as u32 || c.sticky))
            });
            match (window, here) {
                (Some(w), Some(false)) => return wm.activate(w),
                (Some(w), Some(true)) => {
                    wm.focus = Some(w);
                    wm.publish_layout_readonly()?;
                    wm.arrange()?;
                    if !wm.hidden.contains(&w) {
                        wm.set_focus(w)?;
                    }
                    return Ok(());
                }
                _ => {}
            }
            let target = wm.scratch_shown.or_else(|| {
                wm.clients
                    .iter()
                    .find(|c| {
                        c.monitor == monitor
                            && c.tag == index as u32
                            && c.swallowed_by.is_none()
                            && wm.scratch_owner(c.win).is_none()
                    })
                    .map(|c| c.win)
            });
            wm.publish_layout_readonly()?;
            wm.arrange()?;
            match target {
                Some(w) => wm.set_focus(w),
                None => wm.focus_client(None),
            }
        };
        run(self).map_err(|e| e.to_string())
    }

    /// `_NIRUWM_COMMAND` 23 from `niructl do`: the request text is in the
    /// `_NIRUWM_DO` property of the caller's window `requester`; the answer
    /// goes to its `_NIRUWM_DO_RESULT` ("ok", "refused: …" or "error: …").
    pub(crate) fn do_request(&mut self, requester: Window) -> Result<bool> {
        let text = self
            .c
            .get_property(
                true,
                requester,
                self.atom("_NIRUWM_DO"),
                AtomEnum::ANY,
                0,
                1024,
            )
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.value).to_string());
        let answer = |wm: &Self, value: &str| {
            let _ = wm.c.change_property8(
                PropMode::REPLACE,
                requester,
                wm.atom("_NIRUWM_DO_RESULT"),
                wm.atom("UTF8_STRING"),
                value.as_bytes(),
            );
            let _ = wm.c.flush();
        };
        let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
            answer(self, "error: no request text");
            return Ok(true);
        };
        if self.keys_released {
            answer(self, "refused: the screen is locked");
            return Ok(true);
        }
        match crate::bindings::parse_do(&text) {
            Err(DoError::Refused(m)) => {
                answer(self, &format!("refused: {m}"));
                Ok(true)
            }
            Err(DoError::Invalid(m)) => {
                answer(self, &format!("error: {m}"));
                Ok(true)
            }
            Ok(DoRequest::Goto {
                monitor,
                tag,
                window,
            }) => {
                match self.goto(monitor, tag, window) {
                    Ok(()) => answer(self, "ok"),
                    Err(e) => answer(self, &format!("error: {e}")),
                }
                Ok(true)
            }
            Ok(DoRequest::Run(action)) => {
                eprintln!("niruwm: niructl do {}", text.trim());
                let keep = self.dispatch(action)?;
                answer(self, "ok");
                Ok(keep)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_keep_the_aspect_and_fit_384() {
        assert_eq!(thumb_size(2560, 1440), (384, 216));
        assert_eq!(thumb_size(1440, 2560), (216, 384));
        assert_eq!(thumb_size(1280, 800), (384, 240));
        assert_eq!(thumb_size(300, 200), (300, 200));
        assert_eq!(thumb_size(0, 0), (1, 1));
        assert_eq!(thumb_name(1, 10), "mon1-tag10.ppm");
    }

    #[test]
    fn zpixmap_bgrx_becomes_rgb_ppm() {
        // Two pixels, one row: red then blue, 32 bpp little endian (BGRX).
        let data = [0x00, 0x00, 0xff, 0x00, 0xff, 0x00, 0x00, 0x00];
        let ppm = ppm_from_zpixmap(&data, 2, 1, 32, (0xff0000, 0x00ff00, 0x0000ff), true).unwrap();
        assert_eq!(&ppm[..11], b"P6\n2 1\n255\n");
        assert_eq!(&ppm[11..], &[0xff, 0, 0, 0, 0, 0xff]);
        // Rows are padded to 32 bits: a 1x2 image at 24 bpp has 4-byte rows.
        let data = [0x10, 0x20, 0x30, 0, 0x40, 0x50, 0x60, 0];
        let ppm = ppm_from_zpixmap(&data, 1, 2, 24, (0xff0000, 0x00ff00, 0x0000ff), true).unwrap();
        assert_eq!(&ppm[11..], &[0x30, 0x20, 0x10, 0x60, 0x50, 0x40]);
        // 16 bpp RGB565.
        let data = [0x00, 0xf8, 0, 0];
        let ppm = ppm_from_zpixmap(&data, 1, 1, 16, (0xf800, 0x07e0, 0x001f), true).unwrap();
        assert_eq!(&ppm[11..], &[0xff, 0, 0]);
        assert!(ppm_from_zpixmap(&data, 4, 4, 32, (0, 0, 0), true).is_none());
    }
}
