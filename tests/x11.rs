//! Real X11 integration. Run in an isolated server: xvfb-run cargo test --test x11 -- --ignored
use std::{
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use x11rb::{connection::Connection, protocol::xproto::*, rust_connection::RustConnection};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn atom(c: &RustConnection, n: &str) -> Atom {
    c.intern_atom(false, n.as_bytes())
        .unwrap()
        .reply()
        .unwrap()
        .atom
}
fn values(c: &RustConnection, w: Window, n: &str) -> Vec<u32> {
    c.get_property(false, w, atom(c, n), AtomEnum::ANY, 0, 1024)
        .unwrap()
        .reply()
        .unwrap()
        .value32()
        .map(|v| v.collect())
        .unwrap_or_default()
}
fn wait(mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(Instant::now() < end, "X11 condition timed out");
        thread::sleep(Duration::from_millis(20));
    }
}
fn message(c: &RustConnection, root: Window, w: Window, name: &str, data: [u32; 5]) {
    c.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ClientMessageEvent::new(32, w, atom(c, name), data),
    )
    .unwrap();
    c.flush().unwrap();
}
#[test]
#[ignore = "requires isolated X server"]
fn window_lifecycle_tags_fullscreen_and_ownership() {
    let dir = std::env::temp_dir().join(format!("niruwm-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("PATH", "/nonexistent")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    let duplicate = Command::new(env!("CARGO_BIN_EXE_niruwm"))
        .env("XDG_CONFIG_HOME", &dir)
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("Another window manager"));
    let mut windows = vec![];
    for _ in 0..3 {
        let w = c.generate_id().unwrap();
        c.create_window(
            0,
            w,
            root,
            20,
            40,
            320,
            240,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )
        .unwrap();
        c.map_window(w).unwrap();
        windows.push(w);
    }
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 3);
    let g = c.get_geometry(windows[0]).unwrap().reply().unwrap();
    assert_eq!(g.y, 36);
    assert!(g.width > 320);
    message(&c, root, root, "_NET_CURRENT_DESKTOP", [1, 0, 0, 0, 0]);
    wait(|| {
        c.get_window_attributes(windows[0])
            .unwrap()
            .reply()
            .unwrap()
            .map_state
            == MapState::UNMAPPED
    });
    // Bursty tag switches previously exposed delayed UnmapNotify bookkeeping errors.
    for i in 0..50 {
        message(&c, root, root, "_NET_CURRENT_DESKTOP", [i % 2, 0, 0, 0, 0]);
    }
    message(&c, root, root, "_NET_CURRENT_DESKTOP", [0, 0, 0, 0, 0]);
    wait(|| {
        values(&c, root, "_NET_CURRENT_DESKTOP") == vec![0]
            && c.get_window_attributes(windows[0])
                .unwrap()
                .reply()
                .unwrap()
                .map_state
                == MapState::VIEWABLE
    });
    assert_eq!(values(&c, root, "_NET_CLIENT_LIST").len(), 3);
    let full = atom(&c, "_NET_WM_STATE_FULLSCREEN");
    message(&c, root, windows[0], "_NET_WM_STATE", [1, full, 0, 0, 0]);
    wait(|| values(&c, windows[0], "_NET_WM_STATE").contains(&full));
    let g = c.get_geometry(windows[0]).unwrap().reply().unwrap();
    assert_eq!(g.y, 0);
    assert_eq!(g.width, c.setup().roots[s].width_in_pixels);
    message(&c, root, windows[0], "_NET_WM_STATE", [0, full, 0, 0, 0]);
    wait(|| !values(&c, windows[0], "_NET_WM_STATE").contains(&full));
    c.destroy_window(windows[1]).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 2);
    message(&c, root, root, "_NET_CURRENT_DESKTOP", [1, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![1]);
    message(&c, root, root, "_NIRUWM_COMMAND", [4, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    assert_eq!(
        c.get_window_attributes(windows[0])
            .unwrap()
            .reply()
            .unwrap()
            .map_state,
        MapState::VIEWABLE
    );
    std::fs::remove_dir_all(dir).unwrap();
}
