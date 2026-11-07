//! Real X11 integration. Run in an isolated server: xvfb-run cargo test --test x11 -- --ignored
use std::{
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection, protocol::xproto::*, rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};
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
fn text(c: &RustConnection, w: Window, n: &str) -> String {
    c.get_property(false, w, atom(c, n), AtomEnum::ANY, 0, 1024)
        .unwrap()
        .reply()
        .unwrap()
        .value
        .into_iter()
        .map(char::from)
        .collect::<String>()
        .trim()
        .to_string()
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
fn hiding_bar_sends_term_before_forced_cleanup() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("niruwm-bar-term-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = true\n[wallpaper]\nenabled = false\n",
    )
    .unwrap();
    let bar = dir.join("bin/nirubar");
    std::fs::write(&bar, "#!/bin/sh\ntrap 'printf term > \"$BAR_TERM_TEST\"; exit 0' TERM\nprintf started > \"$BAR_START_TEST\"\nwhile :; do sleep 0.05; done\n").unwrap();
    std::fs::set_permissions(&bar, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = dir.join("started");
    let stopped = dir.join("stopped");
    let _wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", dir.join("bin").display()),
            )
            .env("BAR_START_TEST", &started)
            .env("BAR_TERM_TEST", &stopped)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (c, screen) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[screen].root;
    wait(|| started.is_file());
    message(&c, root, root, "_NIRUWM_COMMAND", [12, 3, 0, 0, 0]);
    wait(|| stopped.is_file());
    assert_eq!(std::fs::read_to_string(&stopped).unwrap(), "term");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn unchanged_desktop_properties_do_not_emit_repeated_notifications() {
    let dir = std::env::temp_dir().join(format!("niruwm-property-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("PATH", "/nonexistent")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    c.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .unwrap()
    .check()
    .unwrap();
    wait(|| {
        values(&c, root, "_NET_SUPPORTING_WM_CHECK")
            .first()
            .copied()
            .is_some_and(|window| {
                c.get_window_attributes(window)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .is_some()
                    && values(&c, window, "_NET_SUPPORTING_WM_CHECK") == vec![window]
            })
    });
    wait(|| !values(&c, root, "_NET_NUMBER_OF_DESKTOPS").is_empty());
    thread::sleep(Duration::from_millis(200));
    while c.poll_for_event().unwrap().is_some() {}
    let monitored: Vec<Atom> = [
        "_NET_CURRENT_DESKTOP",
        "_NET_NUMBER_OF_DESKTOPS",
        "_NET_CLIENT_LIST",
        "_NET_CLIENT_LIST_STACKING",
        "_NET_DESKTOP_NAMES",
        "_NIRUWM_MONITORS",
    ]
    .iter()
    .map(|name| atom(&c, name))
    .collect();
    for _ in 0..3 {
        message(&c, root, root, "_NIRUWM_COMMAND", [1, 0, 0, 0, 0]);
    }
    thread::sleep(Duration::from_millis(300));
    while let Some(event) = c.poll_for_event().unwrap() {
        if let x11rb::protocol::Event::PropertyNotify(event) = event {
            assert!(
                !monitored.contains(&event.atom),
                "unchanged desktop property was republished"
            );
        }
    }
    message(&c, root, root, "_NIRUWM_COMMAND", [1, 1, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![1]);
    let window = c.generate_id().unwrap();
    c.create_window(
        0,
        window,
        root,
        20,
        20,
        200,
        150,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    c.map_window(window).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![window]);
    while c.poll_for_event().unwrap().is_some() {}
    let list_atom = atom(&c, "_NET_CLIENT_LIST");
    message(&c, root, window, "_NET_WM_DESKTOP", [0, 0, 0, 0, 0]);
    wait(|| values(&c, window, "_NET_WM_DESKTOP") == vec![0]);
    let mut saw_list_notification = false;
    wait(|| {
        while let Some(event) = c.poll_for_event().unwrap() {
            if matches!(event, x11rb::protocol::Event::PropertyNotify(e) if e.window == root && e.atom == list_atom)
            {
                saw_list_notification = true;
            }
        }
        saw_list_notification
    });
    c.destroy_window(window).unwrap();
    c.flush().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
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
    // A previous WM may have left a root property pointing at a destroyed
    // window. Wait for the current WM's live self-referential check window,
    // otherwise the duplicate process can win the startup race and hang.
    wait(|| {
        values(&c, root, "_NET_SUPPORTING_WM_CHECK")
            .first()
            .copied()
            .is_some_and(|window| {
                c.get_window_attributes(window)
                    .ok()
                    .and_then(|cookie| cookie.reply().ok())
                    .is_some()
                    && values(&c, window, "_NET_SUPPORTING_WM_CHECK") == vec![window]
            })
    });
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
    // The window sits below whatever the top bar reserves. A test PATH has no
    // bar, so the reservation is the configured bar height, never a whole
    // screen: a second screen's bar must not push its windows to the bottom.
    assert!(g.y > 0 && g.y <= 64, "tiled window started at {}", g.y);
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
    // The property is published before the geometry request is flushed. Wait
    // for the applied rectangle, not only for the state atom.
    wait(|| {
        let g = c.get_geometry(windows[0]).unwrap().reply().unwrap();
        g.y == 0 && g.width == c.setup().roots[s].width_in_pixels
    });
    let g = c.get_geometry(windows[0]).unwrap().reply().unwrap();
    assert_eq!(g.y, 0);
    assert_eq!(g.width, c.setup().roots[s].width_in_pixels);
    let dialog = c.generate_id().unwrap();
    c.create_window(
        0,
        dialog,
        root,
        100,
        100,
        400,
        300,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    let transient = atom(&c, "WM_TRANSIENT_FOR");
    c.change_property32(
        PropMode::REPLACE,
        dialog,
        transient,
        AtomEnum::WINDOW,
        &[windows[0]],
    )
    .unwrap();
    c.map_window(dialog).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 4);
    let tree = c.query_tree(root).unwrap().reply().unwrap();
    let dialog_at = tree.children.iter().position(|w| *w == dialog).unwrap();
    let parent_at = tree.children.iter().position(|w| *w == windows[0]).unwrap();
    assert!(
        dialog_at > parent_at,
        "transient dialog was stacked below its fullscreen parent"
    );
    let stacking = values(&c, root, "_NET_CLIENT_LIST_STACKING");
    let dialog_at = stacking.iter().position(|w| *w == dialog).unwrap();
    let parent_at = stacking.iter().position(|w| *w == windows[0]).unwrap();
    assert!(
        dialog_at > parent_at,
        "_NET_CLIENT_LIST_STACKING did not match the X stacking order"
    );
    c.destroy_window(dialog).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 3);
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

#[test]
#[ignore = "requires isolated X server and xterm"]
fn session_restore_matches_windows_and_keeps_saved_focus() {
    let dir = std::env::temp_dir().join(format!("niruwm-restore-{}", std::process::id()));
    let state = dir.join("state");
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::create_dir_all(state.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "theme = \"niru-noir\"\nrestore_session = true\n[bar]\nenabled = false\n",
    )
    .unwrap();
    // Two independent commands with different placements. The second one is
    // floating on workspace 1 and had focus; the first is tiled on workspace 3.
    let session = "xterm\u{1f}-title\u{1f}rest-two\u{1f}-e\u{1f}sleep\u{1f}3\t2\t0\t0\t1\n\
         xterm\u{1f}-title\u{1f}rest-zero\u{1f}-e\u{1f}sleep\u{1f}3\t0\t0\t1\t0\n";
    std::fs::write(state.join("niruwm/session.tsv"), session).unwrap();

    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let mut wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("XDG_STATE_HOME", &state)
            .env("PATH", "/usr/bin:/bin")
            .stdout(Stdio::null())
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
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 2);
    // Restore waits for the mapped windows, then shows the saved focus.
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![2]);
    let clients = values(&c, root, "_NET_CLIENT_LIST");
    let desktop = |w: Window| values(&c, w, "_NET_WM_DESKTOP").first().copied();
    let focused = clients
        .iter()
        .copied()
        .find(|w| desktop(*w) == Some(2))
        .expect("the focused saved window on workspace 3");
    let floating = clients
        .iter()
        .copied()
        .find(|w| desktop(*w) == Some(0))
        .expect("the tiled saved window on workspace 1");
    assert_eq!(values(&c, root, "_NET_ACTIVE_WINDOW"), vec![focused]);
    let focused_width = c.get_geometry(focused).unwrap().reply().unwrap().width;
    let floating_width = c.get_geometry(floating).unwrap().reply().unwrap().width;
    let screen_width = c.setup().roots[s].width_in_pixels;
    assert_eq!(focused_width, screen_width, "focused window was not tiled");
    assert!(
        floating_width < screen_width / 2,
        "saved floating state was not restored (width {floating_width})"
    );
    // Stop the test applications before the next test starts its own manager.
    for window in [focused, floating] {
        if let Some(pid) = values(&c, window, "_NET_WM_PID").first() {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }

    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn bar_bottom_hides_and_returns_without_breaking_work_area() {
    let dir = std::env::temp_dir().join(format!("niruwm-bar-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = true\nposition = \"bottom\"\n",
    )
    .unwrap();
    let _wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("PATH", "/nonexistent")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let screen_h = c.setup().roots[s].height_in_pixels;
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
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
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![w]);
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_niructl"))
            .env("XDG_CONFIG_HOME", &dir)
            .args(args)
            .output()
            .unwrap()
    };
    let bottom = c.get_geometry(w).unwrap().reply().unwrap();
    assert!(bottom.y < 20, "bottom bar window started at {}", bottom.y);
    assert!(
        bottom.y + i16::try_from(bottom.height).unwrap() < i16::try_from(screen_h).unwrap() - 20,
        "bottom bar did not reserve space"
    );
    assert!(run(&["bar", "hide"]).status.success());
    wait(|| text(&c, root, "_NIRUWM_BAR_STATE") == "hidden");
    // Bar state is published before the asynchronous geometry requests have
    // necessarily reached this independent X connection.
    wait(|| c.get_geometry(w).unwrap().reply().unwrap().height > bottom.height);
    let hidden = c.get_geometry(w).unwrap().reply().unwrap();
    assert!(
        hidden.height > bottom.height,
        "hidden bar did not free space"
    );
    assert!(run(&["bar", "show"]).status.success());
    wait(|| text(&c, root, "_NIRUWM_BAR_STATE") == "bottom");
    wait(|| c.get_geometry(w).unwrap().reply().unwrap().height < hidden.height);
    let shown = c.get_geometry(w).unwrap().reply().unwrap();
    assert!(
        shown.height < hidden.height,
        "shown bar did not reserve space"
    );
    assert!(run(&["bar", "top"]).status.success());
    wait(|| text(&c, root, "_NIRUWM_BAR_STATE") == "top");
    wait(|| c.get_geometry(w).unwrap().reply().unwrap().y > bottom.y);
    let top = c.get_geometry(w).unwrap().reply().unwrap();
    assert!(top.y > bottom.y, "top bar did not move the work area down");
}

#[test]
#[ignore = "requires isolated X server and xdotool"]
fn super_drag_tiles_a_window_on_the_target_monitor() {
    let dir = std::env::temp_dir().join(format!("niruwm-drag-test-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n",
    )
    .unwrap();

    // Get the real root geometry first, then describe two monitors that cover
    // it. Xvfb has one output, so the acceptance seam supplies RandR geometry.
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let screen_w = c.setup().roots[s].width_in_pixels;
    let screen_h = c.setup().roots[s].height_in_pixels;
    let left_w = screen_w / 2;
    let right_w = screen_w.saturating_sub(left_w);
    let layouts = dir.join("monitors.txt");
    std::fs::write(
        &layouts,
        format!("LEFT {left_w}x{screen_h}+0+0\nRIGHT {right_w}x{screen_h}+{left_w}+0\n"),
    )
    .unwrap();

    let mut _wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("NIRU_RANDR_EMU", &layouts)
            .env("PATH", "/nonexistent")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait(|| {
        let monitors = text(&c, root, "_NIRUWM_MONITORS");
        monitors.contains("LEFT") && monitors.contains("RIGHT")
    });

    let run = |args: &[&str]| {
        assert!(
            Command::new("xdotool")
                .args(args)
                .status()
                .unwrap()
                .success(),
            "xdotool {args:?} failed"
        );
    };
    let pause = || thread::sleep(Duration::from_millis(80));
    let create = |name: u32| {
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
        c.flush().unwrap();
        assert!(name > 0);
        w
    };

    // A window opened under the pointer on the left screen.
    run(&["mousemove", "--sync", "100", "100"]);
    pause();
    let left = create(1);
    wait(|| values(&c, root, "_NET_CLIENT_LIST").contains(&left));
    wait(|| {
        let g = c.get_geometry(left).unwrap().reply().unwrap();
        g.width > left_w / 2 && g.height > screen_h / 2
    });

    // A second window on the right screen, so the drop has an existing tile
    // to join rather than an empty workspace.
    run(&[
        "mousemove",
        "--sync",
        &(left_w + right_w / 2).to_string(),
        "100",
    ]);
    pause();
    let right = create(2);
    wait(|| values(&c, root, "_NET_CLIENT_LIST").contains(&right));
    wait(|| values(&c, right, "_NET_WM_DESKTOP") == vec![10]);

    let before = c.get_geometry(left).unwrap().reply().unwrap();
    let start_x = before.x + i16::try_from(before.width / 2).unwrap();
    let start_y = before.y + i16::try_from(before.height / 2).unwrap();
    let target_x = left_w as i16 + i16::try_from(right_w / 4).unwrap();
    run(&[
        "mousemove",
        "--sync",
        &start_x.to_string(),
        &start_y.to_string(),
    ]);
    pause();
    run(&["keydown", "Super"]);
    pause();
    run(&["mousedown", "1"]);
    pause();
    // Cross the boundary with real motion events; xdotool emits ButtonPress,
    // MotionNotify and ButtonRelease exactly like a physical mouse.
    run(&[
        "mousemove",
        "--sync",
        &(start_x + 80).to_string(),
        &start_y.to_string(),
    ]);
    pause();
    let during_small = c.get_geometry(left).unwrap().reply().unwrap();
    let outer = |g: &x11rb::protocol::xproto::GetGeometryReply| {
        (g.width + g.border_width * 2, g.height + g.border_width * 2)
    };
    assert_eq!(
        outer(&during_small),
        outer(&before),
        "starting a drag restored the window's pre-tiling size"
    );
    run(&[
        "mousemove",
        "--sync",
        &target_x.to_string(),
        &start_y.to_string(),
    ]);
    pause();
    let during = c.get_geometry(left).unwrap().reply().unwrap();
    assert!(
        during.x > i16::try_from(left_w / 2).unwrap(),
        "the window did not follow the pointer across the boundary"
    );
    // Mapping an unrelated window delivers a MapRequest to the manager while
    // the pointer is still grabbed. arrange() must not clamp the dragged
    // window back to the source monitor when that happens.
    let extra = create(3);
    wait(|| values(&c, root, "_NET_CLIENT_LIST").contains(&extra));
    let after_map = c.get_geometry(left).unwrap().reply().unwrap();
    assert_eq!(
        (after_map.x, after_map.y, outer(&after_map)),
        (during.x, during.y, outer(&during)),
        "an unrelated MapRequest moved the window during the drag"
    );
    run(&["mouseup", "1"]);
    run(&["keyup", "Super"]);

    wait(|| values(&c, left, "_NET_WM_DESKTOP") == vec![10]);
    wait(|| values(&c, extra, "_NET_WM_DESKTOP") == vec![10]);
    let after = c.get_geometry(left).unwrap().reply().unwrap();
    assert!(
        after.x >= i16::try_from(left_w).unwrap(),
        "dragged window stayed on the left screen: x={} left_width={left_w}",
        after.x
    );
    assert!(
        after.width < right_w / 2,
        "dragged window did not join the target stack: width={} target_width={right_w}",
        after.width
    );
    assert!(
        after.height > screen_h / 4,
        "dragged window was not tiled to the target height: height={}",
        after.height
    );
    assert!(values(&c, right, "_NET_WM_DESKTOP") == vec![10]);

    // Destroy this test's clients before stopping the manager so the next
    // ignored test starts with an empty root window.
    for window in [left, right, extra] {
        c.destroy_window(window).unwrap();
    }
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST").is_empty());

    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| _wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn a_client_that_withdrew_itself_stays_hidden() {
    let dir = std::env::temp_dir().join(format!("niruwm-withdrawn-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n",
    )
    .unwrap();
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
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![w]);
    wait(|| c.get_geometry(w).unwrap().reply().unwrap().width > 320);

    // The client hides itself, as a tray application does.
    c.unmap_window(w).unwrap();
    c.flush().unwrap();
    thread::sleep(Duration::from_millis(200));
    message(&c, root, root, "_NET_CURRENT_DESKTOP", [1, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![1]);
    message(&c, root, root, "_NET_CURRENT_DESKTOP", [0, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![0]);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        c.get_window_attributes(w)
            .unwrap()
            .reply()
            .unwrap()
            .map_state,
        MapState::UNMAPPED,
        "a withdrawn client was remapped by a workspace round trip"
    );

    c.destroy_window(w).unwrap();
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server and xdotool"]
fn niructl_tag_and_root_pointer_motion_follow_the_focused_screen() {
    let dir = std::env::temp_dir().join(format!("niruwm-local-tag-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n",
    )
    .unwrap();
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let screen_w = c.setup().roots[s].width_in_pixels;
    let screen_h = c.setup().roots[s].height_in_pixels;
    let left_w = screen_w / 2;
    let right_w = screen_w.saturating_sub(left_w);
    let layouts = dir.join("monitors.txt");
    std::fs::write(
        &layouts,
        format!("LEFT {left_w}x{screen_h}+0+0\nRIGHT {right_w}x{screen_h}+{left_w}+0\n"),
    )
    .unwrap();
    let mut wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("NIRU_RANDR_EMU", &layouts)
            .env("PATH", "/nonexistent")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait(|| {
        let monitors = text(&c, root, "_NIRUWM_MONITORS");
        monitors.contains("LEFT") && monitors.contains("RIGHT")
    });
    let run = |args: &[&str]| {
        assert!(Command::new("xdotool")
            .args(args)
            .status()
            .unwrap()
            .success())
    };

    // Focus the right screen, then ask for local workspace 2.
    run(&["key", "--clearmodifiers", "super+period"]);
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![10]);
    let output = Command::new(env!("CARGO_BIN_EXE_niructl"))
        .env("XDG_CONFIG_HOME", &dir)
        .args(["tag", "2"])
        .output()
        .unwrap();
    assert!(output.status.success(), "niructl tag failed: {output:?}");
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![11]);
    let monitors = text(&c, root, "_NIRUWM_MONITORS");
    let active = |monitors: &str, name: &str| {
        monitors
            .lines()
            .find(|line| line.starts_with(name))
            .and_then(|line| line.split('\t').nth(6))
            .map(str::to_string)
    };
    assert_eq!(active(&monitors, "LEFT"), Some("0".into()));
    assert_eq!(active(&monitors, "RIGHT"), Some("1".into()));

    // Focus the left screen, then move the pointer to the right screen while
    // both desktops are empty. No EnterNotify crosses that boundary, so root
    // motion must update the focused screen before Super+3.
    run(&["key", "--clearmodifiers", "super+comma"]);
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![0]);
    run(&["mousemove", "--sync", "100", "300"]);
    thread::sleep(Duration::from_millis(100));
    let right_x = (left_w + 100).to_string();
    run(&["mousemove", "--sync", &right_x, "300"]);
    thread::sleep(Duration::from_millis(200));
    run(&["key", "--clearmodifiers", "super+3"]);
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![12]);
    let monitors = text(&c, root, "_NIRUWM_MONITORS");
    assert_eq!(active(&monitors, "LEFT"), Some("0".into()));
    assert_eq!(active(&monitors, "RIGHT"), Some("2".into()));

    // `movetotag` is the same local command: the focused window joins local
    // workspace 4 on the focused (right) screen. Its global desktop is 13.
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
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST").contains(&w));
    let output = Command::new(env!("CARGO_BIN_EXE_niructl"))
        .env("XDG_CONFIG_HOME", &dir)
        .args(["movetotag", "4"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "niructl movetotag failed: {output:?}"
    );
    wait(|| values(&c, w, "_NET_WM_DESKTOP") == vec![13]);
    c.destroy_window(w).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST").is_empty());

    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn docks_stay_above_normal_clients_after_fullscreen() {
    let dir = std::env::temp_dir().join(format!("niruwm-dock-stack-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n",
    )
    .unwrap();
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

    let dock = c.generate_id().unwrap();
    c.create_window(
        0,
        dock,
        root,
        0,
        0,
        640,
        28,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    c.change_property32(
        PropMode::REPLACE,
        dock,
        atom(&c, "_NET_WM_WINDOW_TYPE"),
        AtomEnum::ATOM,
        &[atom(&c, "_NET_WM_WINDOW_TYPE_DOCK")],
    )
    .unwrap();
    c.map_window(dock).unwrap();
    c.flush().unwrap();
    wait(|| {
        c.get_window_attributes(dock)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|attrs| attrs.map_state == MapState::VIEWABLE)
    });

    let client = c.generate_id().unwrap();
    c.create_window(
        0,
        client,
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
    c.map_window(client).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![client]);

    let stacking = || {
        c.query_tree(root)
            .unwrap()
            .reply()
            .unwrap()
            .children
            .iter()
            .copied()
            .position(|w| w == dock)
            .zip(
                c.query_tree(root)
                    .unwrap()
                    .reply()
                    .unwrap()
                    .children
                    .iter()
                    .copied()
                    .position(|w| w == client),
            )
            .unwrap()
    };

    // A fullscreen client may cover the panel.
    let full = atom(&c, "_NET_WM_STATE_FULLSCREEN");
    message(&c, root, client, "_NET_WM_STATE", [1, full, 0, 0, 0]);
    wait(|| {
        let g = c.get_geometry(client).unwrap().reply().unwrap();
        g.y == 0 && g.width == c.setup().roots[s].width_in_pixels
    });
    let (dock_at, client_at) = stacking();
    assert!(
        client_at > dock_at,
        "fullscreen client was not above the dock"
    );

    // Leaving fullscreen must put the panel back above ordinary clients.
    message(&c, root, client, "_NET_WM_STATE", [0, full, 0, 0, 0]);
    wait(|| !values(&c, client, "_NET_WM_STATE").contains(&full));
    wait(|| {
        let (dock_at, client_at) = stacking();
        dock_at > client_at
    });

    c.destroy_window(client).unwrap();
    c.destroy_window(dock).unwrap();
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn utility_and_popup_windows_do_not_steal_focus() {
    let dir = std::env::temp_dir().join(format!("niruwm-focus-steal-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n",
    )
    .unwrap();
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

    let create = |class: &str, kind: &str| {
        let w = c.generate_id().unwrap();
        c.create_window(
            0,
            w,
            root,
            40,
            60,
            320,
            240,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )
        .unwrap();
        c.change_property8(
            PropMode::REPLACE,
            w,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            format!("{class}\0{class}\0").as_bytes(),
        )
        .unwrap();
        c.change_property32(
            PropMode::REPLACE,
            w,
            atom(&c, "_NET_WM_WINDOW_TYPE"),
            AtomEnum::ATOM,
            &[atom(&c, kind)],
        )
        .unwrap();
        c.map_window(w).unwrap();
        c.flush().unwrap();
        w
    };

    let normal = create("Steam", "_NET_WM_WINDOW_TYPE_NORMAL");
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![normal]);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![normal]);

    let utility = create("Steam", "_NET_WM_WINDOW_TYPE_UTILITY");
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 2);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        values(&c, root, "_NET_ACTIVE_WINDOW"),
        vec![normal],
        "a utility helper stole focus on map"
    );

    let popup = create("Steam", "_NET_WM_WINDOW_TYPE_POPUP_MENU");
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 3);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        values(&c, root, "_NET_ACTIVE_WINDOW"),
        vec![normal],
        "a managed popup stole focus on map"
    );

    c.destroy_window(popup).unwrap();
    c.destroy_window(utility).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![normal]);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![normal]);
    c.destroy_window(normal).unwrap();
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn input_only_helpers_are_not_managed_or_focused() {
    let dir = std::env::temp_dir().join(format!("niruwm-input-only-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n",
    )
    .unwrap();
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

    let normal = c.generate_id().unwrap();
    c.create_window(
        0,
        normal,
        root,
        40,
        60,
        320,
        240,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    c.map_window(normal).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![normal]);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![normal]);

    let helper = c.generate_id().unwrap();
    c.create_window(
        0,
        helper,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    c.map_window(helper).unwrap();
    c.flush().unwrap();
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        values(&c, root, "_NET_CLIENT_LIST"),
        vec![normal],
        "an input-only helper entered the client list"
    );
    assert_eq!(
        values(&c, root, "_NET_ACTIVE_WINDOW"),
        vec![normal],
        "an input-only helper took focus"
    );

    c.destroy_window(helper).unwrap();
    c.destroy_window(normal).unwrap();
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

fn create(c: &RustConnection, root: Window, class: &[u8]) -> Window {
    let w = c.generate_id().unwrap();
    c.create_window(
        0,
        w,
        root,
        10,
        10,
        300,
        200,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    c.change_property8(
        PropMode::REPLACE,
        w,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        class,
    )
    .unwrap();
    c.map_window(w).unwrap();
    c.flush().unwrap();
    w
}

fn geometry(c: &RustConnection, w: Window) -> (i16, i16, u16, u16) {
    let g = c.get_geometry(w).unwrap().reply().unwrap();
    (g.x, g.y, g.width, g.height)
}

fn mapped(c: &RustConnection, w: Window) -> bool {
    c.get_window_attributes(w)
        .unwrap()
        .reply()
        .unwrap()
        .map_state
        == MapState::VIEWABLE
}

#[test]
#[ignore = "requires isolated X server"]
fn scratchpads_never_resize_the_workspace_and_return_focus() {
    let dir = std::env::temp_dir().join(format!("niruwm-scratch-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n\
         [scratchpad]\ncommand = [\"xterm\", \"-class\", \"scratchpad\"]\n\
         [[scratchpad.slot]]\nnumber = 2\ncommand = [\"xterm\", \"-class\", \"scratch-two\"]\nwidth = 0.4\n",
    )
    .unwrap();
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
    let screen_w = c.setup().roots[s].width_in_pixels;
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());

    // One ordinary window: it tiles at full width.
    let a = create(&c, root, b"term\0Term\0");
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![a]);
    wait(|| geometry(&c, a).2 > screen_w - 40);
    let full = geometry(&c, a);

    // The default scratchpad appears floating, focused, and changes nothing.
    let s0 = create(&c, root, b"scratchpad\0scratchpad\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![s0]);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        geometry(&c, a),
        full,
        "showing the scratchpad moved the window"
    );
    assert_eq!(
        values(&c, root, "_NET_CLIENT_LIST"),
        vec![a],
        "a scratchpad is not a workspace window"
    );
    assert!(text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("0\tshown\t{s0}")));

    // Hide: the 0.17.3 bug made the hidden scratchpad a tile, halving `a`.
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 0, 0, 0, 0]);
    wait(|| !mapped(&c, s0));
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        geometry(&c, a),
        full,
        "hiding the scratchpad resized the window"
    );
    assert_eq!(
        values(&c, root, "_NET_ACTIVE_WINDOW"),
        vec![a],
        "focus returns"
    );
    assert!(text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("0\thidden\t{s0}")));

    // Show again, then switch workspace: the scratchpad follows, `a` stays.
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 0, 0, 0, 0]);
    wait(|| mapped(&c, s0));
    assert_eq!(geometry(&c, a), full);
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 1, 0, 0, 0]);
    wait(|| !mapped(&c, a));
    assert!(mapped(&c, s0), "a shown scratchpad follows the workspace");
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 0, 2, 0, 0]);
    wait(|| !mapped(&c, s0));
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 0, 0, 0, 0]);
    wait(|| mapped(&c, a));
    assert_eq!(geometry(&c, a), full);

    // Send `b` to slot 2: it hides and `a` gets the full width back.
    let b = create(&c, root, b"other\0Other\0");
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![a, b]);
    wait(|| geometry(&c, a).2 < full.2);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![b]);
    message(&c, root, root, "_NIRUWM_COMMAND", [16, 2, 0, 0, 0]);
    wait(|| !mapped(&c, b));
    wait(|| geometry(&c, a) == full);
    assert_eq!(values(&c, root, "_NET_CLIENT_LIST"), vec![a]);
    // Only one scratchpad is shown at a time.
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 0, 1, 0, 0]);
    wait(|| mapped(&c, s0));
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 2, 0, 0, 0]);
    wait(|| mapped(&c, b));
    wait(|| !mapped(&c, s0));
    assert_eq!(geometry(&c, a), full);
    // Slot 2's own width (0.4 of the screen) applies to a sent window too.
    let gb = geometry(&c, b);
    assert!(
        (i32::from(gb.2) - i32::from(screen_w) * 4 / 10).abs() < 8,
        "{gb:?}"
    );

    // Sending it to the same slot again makes it a normal tiled window.
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![b]);
    message(&c, root, root, "_NIRUWM_COMMAND", [16, 2, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![a, b]);
    wait(|| geometry(&c, a).2 < full.2);
    assert!(!text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("\t{b}\t")));

    // Closing a scratchpad frees its slot.
    c.destroy_window(s0).unwrap();
    c.flush().unwrap();
    wait(|| !text(&c, root, "_NIRUWM_SCRATCHPADS").starts_with("0\t"));
    // A new window of the class takes the free slot again.
    let s1 = create(&c, root, b"scratchpad\0scratchpad\0");
    wait(|| text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("0\tshown\t{s1}")));

    c.destroy_window(s1).unwrap();
    c.destroy_window(b).unwrap();
    c.destroy_window(a).unwrap();
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

/// A window with a WM_CLASS, a transient parent, a window type and states,
/// all set before it maps, the way a toolkit's dialog arrives.
fn create_dialog(
    c: &RustConnection,
    root: Window,
    class: &[u8],
    parent: Window,
    modal: bool,
) -> Window {
    let w = c.generate_id().unwrap();
    c.create_window(
        0,
        w,
        root,
        40,
        40,
        200,
        120,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    c.change_property8(
        PropMode::REPLACE,
        w,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        class,
    )
    .unwrap();
    if parent != 0 {
        c.change_property32(
            PropMode::REPLACE,
            w,
            AtomEnum::WM_TRANSIENT_FOR,
            AtomEnum::WINDOW,
            &[parent],
        )
        .unwrap();
    }
    c.change_property32(
        PropMode::REPLACE,
        w,
        atom(c, "_NET_WM_WINDOW_TYPE"),
        AtomEnum::ATOM,
        &[atom(c, "_NET_WM_WINDOW_TYPE_DIALOG")],
    )
    .unwrap();
    if modal {
        c.change_property32(
            PropMode::REPLACE,
            w,
            atom(c, "_NET_WM_STATE"),
            AtomEnum::ATOM,
            &[atom(c, "_NET_WM_STATE_MODAL")],
        )
        .unwrap();
    }
    c.map_window(w).unwrap();
    c.flush().unwrap();
    w
}

/// Top-level windows from bottom to top.
fn stacking(c: &RustConnection, root: Window) -> Vec<Window> {
    c.query_tree(root).unwrap().reply().unwrap().children
}

fn above(c: &RustConnection, root: Window, upper: Window, lower: Window) -> bool {
    let order = stacking(c, root);
    let at = |w| order.iter().position(|x| *x == w);
    matches!((at(upper), at(lower)), (Some(u), Some(l)) if u > l)
}

fn warp(c: &RustConnection, root: Window, x: i16, y: i16) {
    c.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, x, y).unwrap();
    c.flush().unwrap();
}

fn scratch_wm(dir: &std::path::Path, extra: &str) -> Process {
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        format!(
            "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n\
             [scratchpad]\ncommand = [\"xterm\", \"-class\", \"scratchpad\"]\n{extra}"
        ),
    )
    .unwrap();
    Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", dir)
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("PATH", "/nonexistent")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

#[test]
#[ignore = "requires isolated X server"]
fn scratchpad_dialogs_stay_above_it_follow_it_and_never_loop_focus() {
    let dir = std::env::temp_dir().join(format!("niruwm-scratch-dialog-{}", std::process::id()));
    let mut wm = scratch_wm(&dir, "");
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let (sw, sh) = (
        c.setup().roots[s].width_in_pixels as i16,
        c.setup().roots[s].height_in_pixels as i16,
    );
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    let a = create(&c, root, b"term\0Term\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![a]);
    let sp = create(&c, root, b"scratchpad\0scratchpad\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![sp]);

    // Behave like Qt: whenever the scratchpad itself gets the focus while its
    // modal dialog is open, ask for the dialog to be activated again. 0.17.4
    // and this client then took turns about 9,000 times a second.
    let dialog = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let echo = {
        let (dialog, stop) = (dialog.clone(), stop.clone());
        thread::spawn(move || {
            let (q, qs) = x11rb::connect(None).unwrap();
            let qroot = q.setup().roots[qs].root;
            q.change_window_attributes(
                sp,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::FOCUS_CHANGE),
            )
            .unwrap();
            q.flush().unwrap();
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                match q.poll_for_event().unwrap() {
                    Some(x11rb::protocol::Event::FocusIn(_)) => {
                        let d = dialog.load(std::sync::atomic::Ordering::Relaxed);
                        if d != 0 {
                            message(&q, qroot, d, "_NET_ACTIVE_WINDOW", [2, 0, 0, 0, 0]);
                        }
                    }
                    Some(_) => {}
                    None => thread::sleep(Duration::from_millis(1)),
                }
            }
        })
    };

    // The dialog has the scratchpad's class, as Qt dialogs do: it must not
    // become a scratchpad itself, nor join the workspace's window list.
    let d = create_dialog(&c, root, b"scratchpad\0scratchpad\0", sp, true);
    dialog.store(d, std::sync::atomic::Ordering::Relaxed);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![d]);
    wait(|| above(&c, root, d, sp));
    assert_eq!(values(&c, root, "_NET_CLIENT_LIST"), vec![a]);
    assert!(!text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("\t{d}\t")));

    // Count focus changes while the pointer crosses the scratchpad, the
    // dialog and back.
    let watcher = x11rb::connect(None).unwrap().0;
    watcher
        .change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .unwrap();
    watcher.flush().unwrap();
    let active = atom(&watcher, "_NET_ACTIVE_WINDOW");
    let gs = geometry(&c, sp);
    let gd = geometry(&c, d);
    for _ in 0..5 {
        warp(&c, root, gs.0 + 5, gs.1 + 5);
        thread::sleep(Duration::from_millis(60));
        warp(&c, root, gd.0 + gd.2 as i16 / 2, gd.1 + gd.3 as i16 / 2);
        thread::sleep(Duration::from_millis(60));
    }
    thread::sleep(Duration::from_millis(500));
    let mut changes = 0;
    while let Some(event) = watcher.poll_for_event().unwrap() {
        if let x11rb::protocol::Event::PropertyNotify(p) = event {
            if p.atom == active {
                changes += 1;
            }
        }
    }
    assert!(changes < 40, "focus loop: {changes} focus changes");
    assert!(
        above(&c, root, d, sp),
        "the dialog went under its scratchpad"
    );
    // The pointer over the scratchpad focuses its modal dialog.
    assert_eq!(values(&c, root, "_NET_ACTIVE_WINDOW"), vec![d]);

    // Hidden and shown with its scratchpad, above it again.
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 0, 0, 0, 0]);
    wait(|| !mapped(&c, sp) && !mapped(&c, d));
    assert_eq!(values(&c, root, "_NET_CLIENT_LIST"), vec![a]);
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 0, 0, 0, 0]);
    wait(|| mapped(&c, sp) && mapped(&c, d));
    wait(|| above(&c, root, d, sp));
    // It follows the scratchpad to another workspace.
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 2, 0, 0, 0]);
    wait(|| !mapped(&c, a));
    assert!(mapped(&c, sp) && mapped(&c, d));
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 0, 0, 0, 0]);
    wait(|| mapped(&c, a));

    // The pointer leaving onto another window gives that window the focus,
    // but the scratchpad and its dialog stay on top (hide_on_focus_loss is
    // off by default).
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    echo.join().unwrap();
    c.destroy_window(d).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![sp]);

    // A dialog that names no parent (the portal's file chooser) opening
    // while the scratchpad has the focus belongs to it as well: above it,
    // hidden and shown with it.
    let g = create_dialog(
        &c,
        root,
        b"xdg-desktop-portal-gtk\0Xdg-desktop-portal-gtk\0",
        0,
        true,
    );
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![g]);
    wait(|| above(&c, root, g, sp));
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 0, 0, 0, 0]);
    wait(|| !mapped(&c, sp) && !mapped(&c, g));
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 0, 0, 0, 0]);
    wait(|| mapped(&c, sp) && mapped(&c, g));
    wait(|| above(&c, root, g, sp));
    c.destroy_window(g).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![sp]);
    thread::sleep(Duration::from_millis(400));
    warp(&c, root, sw / 2, sh / 2);
    thread::sleep(Duration::from_millis(100));
    warp(&c, root, 4, sh - 4);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![a]);
    thread::sleep(Duration::from_millis(150));
    assert!(mapped(&c, sp), "the scratchpad hid when the pointer left");
    assert!(
        above(&c, root, sp, a),
        "a window went over the shown scratchpad"
    );

    c.destroy_window(sp).unwrap();
    c.destroy_window(a).unwrap();
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn hide_on_focus_loss_hides_the_scratchpad_when_another_window_takes_focus() {
    let dir = std::env::temp_dir().join(format!("niruwm-scratch-hide-{}", std::process::id()));
    let mut wm = scratch_wm(&dir, "hide_on_focus_loss = true\n");
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let (sw, sh) = (
        c.setup().roots[s].width_in_pixels as i16,
        c.setup().roots[s].height_in_pixels as i16,
    );
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    let a = create(&c, root, b"term\0Term\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![a]);
    warp(&c, root, sw / 2, sh / 2);
    let sp = create(&c, root, b"scratchpad\0scratchpad\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![sp]);
    // A dialog of the scratchpad is not a focus loss.
    let d = create_dialog(&c, root, b"dlg\0Dlg\0", sp, false);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![d]);
    thread::sleep(Duration::from_millis(400));
    assert!(mapped(&c, sp));
    c.destroy_window(d).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![sp]);
    // The pointer moving onto the window outside it is.
    warp(&c, root, 4, sh - 4);
    wait(|| !mapped(&c, sp));
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![a]);
    assert!(text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("0\thidden\t{sp}")));
    c.destroy_window(sp).unwrap();
    c.destroy_window(a).unwrap();
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn a_manager_started_again_after_a_crash_keeps_workspaces_and_scratchpads() {
    let dir = std::env::temp_dir().join(format!("niruwm-restart-{}", std::process::id()));
    let mut first = scratch_wm(&dir, "");
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    let a = create(&c, root, b"one\0One\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![a]);
    let b = create(&c, root, b"two\0Two\0");
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![a, b]);
    // b goes to workspace 3, then a third window to scratchpad slot 4.
    message(&c, root, b, "_NET_WM_DESKTOP", [2, 0, 0, 0, 0]);
    wait(|| !mapped(&c, b));
    let k = create(&c, root, b"three\0Three\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![k]);
    message(&c, root, root, "_NIRUWM_COMMAND", [16, 4, 0, 0, 0]);
    wait(|| !mapped(&c, k));
    let sp = create(&c, root, b"scratchpad\0scratchpad\0");
    wait(|| text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("0\tshown\t{sp}")));

    // The manager dies without cleaning up, as a crash would.
    let _ = first.0.kill();
    let _ = first.0.wait();
    let mut second = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("NIRUWM_RESTARTED", "1")
            .env("PATH", "/nonexistent")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait(|| {
        let state = text(&c, root, "_NIRUWM_SCRATCHPADS");
        state.contains(&format!("4\thidden\t{k}")) && state.contains(&format!("\t{sp}\t"))
    });
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 2);
    let mut list = values(&c, root, "_NET_CLIENT_LIST");
    list.sort_unstable();
    let mut expected = vec![a, b];
    expected.sort_unstable();
    assert_eq!(list, expected, "scratchpads stay out of the window list");
    wait(|| mapped(&c, a) && !mapped(&c, b) && !mapped(&c, k));
    assert_eq!(
        values(&c, b, "_NET_WM_DESKTOP"),
        vec![2],
        "b kept its workspace"
    );
    for w in [a, b, k, sp] {
        c.destroy_window(w).unwrap();
    }
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| second.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}
