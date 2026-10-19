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
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
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

#[test]
#[ignore = "requires isolated X server and xdotool"]
fn partial_key_overrides_preserve_defaults_and_helpers_are_reaped() {
    let dir = std::env::temp_dir().join(format!("niruwm-bindings-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[[keys]]\nkeys = \"super+x\"\naction = \"tag 2\"\n[[keys]]\nkeys = \"super+z\"\naction = \"shell true\"\n[[keys]]\nkeys = \"super+p\"\naction = \"unbind\"\n").unwrap();
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let mut wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .spawn()
            .unwrap(),
    );
    wait(|| {
        values(&c, root, "_NET_SUPPORTING_WM_CHECK")
            .first()
            .is_some_and(|owner| {
                c.get_window_attributes(*owner)
                    .ok()
                    .and_then(|r| r.reply().ok())
                    .is_some()
            })
    });
    let key = |key: &str| {
        assert!(Command::new("xdotool")
            .args(["key", key])
            .status()
            .unwrap()
            .success())
    };
    key("super+3");
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![2]);
    key("super+x");
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![1]);
    let setup = c.setup();
    let mapping = c
        .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)
        .unwrap()
        .reply()
        .unwrap();
    let p = mapping
        .keysyms
        .chunks(mapping.keysyms_per_keycode as usize)
        .position(|symbols| symbols.contains(&0x70))
        .unwrap() as u8
        + setup.min_keycode;
    // An unbound key is genuinely available to another client, not a grabbed no-op.
    c.grab_key(
        false,
        root,
        ModMask::M4,
        p,
        GrabMode::ASYNC,
        GrabMode::ASYNC,
    )
    .unwrap()
    .check()
    .unwrap();
    c.ungrab_key(p, root, ModMask::M4).unwrap();
    for _ in 0..8 {
        key("super+z");
    }
    let children = format!("/proc/{0}/task/{0}/children", wm.0.id());
    wait(|| {
        std::fs::read_to_string(&children)
            .unwrap()
            .trim()
            .is_empty()
    });
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn reload_and_theme_acknowledge_applied_state() {
    let dir = std::env::temp_dir().join(format!("niruwm-ack-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    let config = dir.join("niruwm/config.toml");
    std::fs::write(&config, "theme = \"niru-noir\"\n[bar]\nenabled = false\n").unwrap();
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let mut wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("XDG_STATE_HOME", &dir)
            .env("PATH", "/nonexistent")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    // A property from the previous test survives its window manager. Wait for
    // a check window that actually exists before sending events to the root.
    wait(|| {
        values(&c, root, "_NET_SUPPORTING_WM_CHECK")
            .first()
            .is_some_and(|owner| {
                c.get_window_attributes(*owner)
                    .ok()
                    .and_then(|r| r.reply().ok())
                    .is_some()
            })
    });
    let theme = || {
        c.get_property(
            false,
            root,
            atom(&c, "_NIRUWM_THEME"),
            AtomEnum::ANY,
            0,
            128,
        )
        .unwrap()
        .reply()
        .unwrap()
        .value
    };
    wait(|| theme() == b"niru-noir");
    assert_eq!(values(&c, root, "_NIRUWM_ACK_CONTROL"), vec![1]);

    // A control reload carries a serial and is only acknowledged after the
    // configuration was actually applied.
    message(&c, root, root, "_NIRUWM_COMMAND", [4, 4242, 0, 0, 0]);
    wait(|| values(&c, root, "_NIRUWM_CONFIG_ACK") == vec![4242]);

    let niructl = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_niructl"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("XDG_STATE_HOME", &dir)
            .env("PATH", "/nonexistent")
            .args(args)
            .output()
            .unwrap()
    };
    let out = niructl(&["theme", "niru-nord"]);
    assert!(
        out.status.success(),
        "theme failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    wait(|| theme() == b"niru-nord");

    // The old implementation slept 30 ms and reported success even when the
    // window manager rejected the file. This must now fail observably while
    // the working theme stays in effect.
    std::fs::write(&config, "theme = \"niru-nord\"\n[bar\n").unwrap();
    let out = niructl(&["reload"]);
    assert!(!out.status.success(), "invalid config reported success");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("rejected"),
        "unexpected error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ack = values(&c, root, "_NIRUWM_CONFIG_ACK");
    assert!(ack.first().is_some_and(|v| v & 0x8000_0000 != 0));
    assert_eq!(theme(), b"niru-nord");

    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}
