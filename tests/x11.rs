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

/// The lock key is a manual lock; `niructl lock` from a script is silent
/// unless it says `--manual`. A stand-in NIRULOCK records NIRULOCK_REASON.
#[test]
#[ignore = "requires isolated X server and xdotool"]
fn lock_reasons_reach_the_locker() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("niruwm-lock-reason-{}", std::process::id()));
    let bin = dir.join("home/.local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(dir.join("config/niruwm")).unwrap();
    std::fs::write(
        dir.join("config/niruwm/config.toml"),
        "[bar]\nenabled = false\n",
    )
    .unwrap();
    let log = dir.join("reasons");
    std::fs::write(
        bin.join("nirulock"),
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"${{NIRULOCK_REASON:-none}}\" \"$*\" >> '{}'\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("nirulock"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let reasons = || std::fs::read_to_string(&log).unwrap_or_default();
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let _wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("HOME", dir.join("home"))
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("PATH", &path)
            .env_remove("NIRULOCK_REASON")
            .spawn()
            .unwrap(),
    );
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    assert!(Command::new("xdotool")
        .args(["key", "super+Escape"])
        .status()
        .unwrap()
        .success());
    wait(|| reasons().lines().count() == 1);
    // The reason travels in the environment: an older NIRULOCK rejects
    // unknown arguments, so none are passed.
    assert_eq!(reasons(), "manual \n");
    // The stand-in never publishes readiness, so each niructl call ends with
    // an error after its wait; only the recorded reason matters here.
    for (args, want) in [
        (&["lock"][..], "script"),
        (&["lock", "--silent"][..], "script"),
        (&["lock", "--manual"][..], "manual"),
        (&["lock", "--reason", "session"][..], "session"),
    ] {
        let before = reasons().lines().count();
        let _ = Command::new(env!("CARGO_BIN_EXE_niructl"))
            .args(args)
            .env("HOME", dir.join("home"))
            .env("PATH", &path)
            .env_remove("NIRULOCK_REASON")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        let lines = reasons();
        assert_eq!(lines.lines().count(), before + 1, "{args:?}");
        assert_eq!(
            lines.lines().last(),
            Some(format!("{want} ").as_str()),
            "{args:?}"
        );
    }
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
    run(&["key", "--clearmodifiers", "super+alt+period"]);
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

    // A named window goes to a slot whatever has the focus (command 18):
    // `b` keeps the focus, `a` disappears into slot 3 and stays there when
    // it is sent again.
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![b]);
    message(&c, root, root, "_NIRUWM_COMMAND", [18, 3, a, 0, 0]);
    wait(|| !mapped(&c, a));
    wait(|| text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("3\thidden\t{a}")));
    assert_eq!(
        values(&c, root, "_NET_ACTIVE_WINDOW"),
        vec![b],
        "the focus stays"
    );
    assert_eq!(values(&c, root, "_NET_CLIENT_LIST"), vec![b]);
    message(&c, root, root, "_NIRUWM_COMMAND", [18, 3, a, 0, 0]);
    thread::sleep(Duration::from_millis(150));
    assert!(text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("3\thidden\t{a}")));
    // An unknown window id changes nothing.
    message(
        &c,
        root,
        root,
        "_NIRUWM_COMMAND",
        [18, 4, 0x7fff_fff0, 0, 0],
    );
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 3, 1, 0, 0]);
    wait(|| mapped(&c, a));
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![a]);
    message(&c, root, root, "_NIRUWM_COMMAND", [17, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 2);
    assert!(!text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("\t{a}\t")));

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

#[test]
#[ignore = "requires isolated X server and xdotool"]
fn cheat_sheet_keys_open_the_launcher_and_run_bound_actions_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("niruwm-keysheet-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n\n[[keys]]\nkeys = \"super+x\"\naction = \"layout monocle\"\ndescription = \"My monocle\"\n",
    )
    .unwrap();
    let log = dir.join("launcher.log");
    let stub = dir.join("bin/nirulauncher");
    std::fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$LAUNCHER_LOG\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", dir.join("bin").display());
    let _wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("PATH", &path)
            .env("LAUNCHER_LOG", &log)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    wait(|| !text(&c, root, "_NIRUWM_LAYOUT").is_empty());
    let niructl = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_niructl"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("PATH", &path)
            .args(args)
            .output()
            .unwrap()
    };
    let launched = || std::fs::read_to_string(&log).unwrap_or_default();

    // The new default keys start the launcher views.
    assert!(Command::new("xdotool")
        .args(["key", "--clearmodifiers", "super+slash"])
        .status()
        .unwrap()
        .success());
    wait(|| launched().contains("--keys"));
    assert!(Command::new("xdotool")
        .args(["key", "--clearmodifiers", "super+period"])
        .status()
        .unwrap()
        .success());
    wait(|| launched().contains("--emoji"));

    // Enter in the sheet: run what a key is bound to.
    let out = niructl(&["keys", "run", "super+x"]);
    assert!(out.status.success(), "{out:?}");
    wait(|| text(&c, root, "_NIRUWM_LAYOUT").starts_with("monocle"));
    let out = niructl(&["keys", "run", "super+t"]);
    assert!(out.status.success(), "{out:?}");
    wait(|| text(&c, root, "_NIRUWM_LAYOUT").starts_with("tile"));

    // A key that is not bound is refused by niructl, and the manager ignores
    // a raw command 19 for it.
    assert!(!niructl(&["keys", "run", "super+z"]).status.success());
    message(&c, root, root, "_NIRUWM_COMMAND", [19, 1 << 6, 0x7a, 0, 0]);

    // While a lock screen holds the keyboard nothing runs through 19.
    message(&c, root, root, "_NIRUWM_LOCK", [1, 0, 0, 0, 0]);
    message(&c, root, root, "_NIRUWM_COMMAND", [19, 1 << 6, 0x78, 0, 0]);
    thread::sleep(Duration::from_millis(300));
    assert!(text(&c, root, "_NIRUWM_LAYOUT").starts_with("tile"));
    message(&c, root, root, "_NIRUWM_LOCK", [0, 0, 0, 0, 0]);
    message(&c, root, root, "_NIRUWM_COMMAND", [19, 1 << 6, 0x78, 0, 0]);
    wait(|| text(&c, root, "_NIRUWM_LAYOUT").starts_with("monocle"));

    // The sheet carries the user's words and marks the entry as config.
    let out = niructl(&["keys", "--json"]);
    assert!(out.status.success());
    let sheet: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mine = sheet["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["keys"] == "super+x")
        .unwrap();
    assert_eq!(mine["description"], "My monocle");
    assert_eq!(mine["source"], "config");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore]
fn a_shifted_symbol_never_takes_over_the_workspace_key() {
    // Swedish layout: the 7 key types 7, and "/" with Shift. Super+/ was
    // grabbed on that key without Shift and turned Super+7 into the cheat
    // sheet. The workspace key must win (Super+F1 is the sheet's key on
    // every layout; that default is checked by a unit test).
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("niruwm-se-keys-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n",
    )
    .unwrap();
    let log = dir.join("launcher.log");
    let stub = dir.join("bin/nirulauncher");
    std::fs::write(
        &stub,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$LAUNCHER_LOG\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let min = c.setup().min_keycode;
    let count = c.setup().max_keycode - min + 1;
    let map = c.get_keyboard_mapping(min, count).unwrap().reply().unwrap();
    let per = map.keysyms_per_keycode as usize;
    let row = |code: u8| map.keysyms[(code - min) as usize * per..][..per].to_vec();
    // The key that types 7 now, and one with "/" that must not be used.
    let seven = (min..=c.setup().max_keycode)
        .find(|&k| row(k).first() == Some(&0x37))
        .unwrap();
    let slash = (min..=c.setup().max_keycode).find(|&k| row(k).contains(&0x2f));
    let saved: Vec<(u8, Vec<u32>)> = std::iter::once(seven)
        .chain(slash)
        .map(|k| (k, row(k)))
        .collect();
    // Put the keyboard back even when an assertion fails, so the next test
    // sees the normal map.
    struct Restore(Vec<(u8, Vec<u32>)>, usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            match x11rb::connect(None) {
                Ok((c, _)) => {
                    for (code, syms) in &self.0 {
                        let r = c
                            .change_keyboard_mapping(1, *code, self.1 as u8, syms)
                            .map(|cookie| cookie.check());
                        eprintln!("se-keys: restore keycode {code}: {r:?}");
                    }
                    let _ = c.flush();
                }
                Err(e) => eprintln!("se-keys: restore: cannot connect: {e}"),
            }
        }
    }
    let _restore = Restore(saved, per);
    let set = |code: u8, syms: &[u32]| {
        let mut full = syms.to_vec();
        full.resize(per, 0);
        c.change_keyboard_mapping(1, code, per as u8, &full)
            .unwrap()
            .check()
            .unwrap();
    };
    set(seven, &[0x37, 0x2f, 0x37, 0x2f]);
    if let Some(k) = slash.filter(|&k| k != seven) {
        set(k, &[0x2d, 0x5f, 0x2d, 0x5f]);
    }
    c.flush().unwrap();
    let probe = || {
        let m = c.get_keyboard_mapping(min, count).unwrap().reply().unwrap();
        m.keysyms[(seven - min) as usize * per..][..per].to_vec()
    };
    eprintln!(
        "se-keys: per={per} seven={seven} slash={slash:?} row={:x?}",
        probe()
    );

    let path = format!("{}:/usr/bin:/bin", dir.join("bin").display());
    let wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("PATH", &path)
            .env("LAUNCHER_LOG", &log)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(dir.join("wm.log")).unwrap())
            .spawn()
            .unwrap(),
    );
    wait(|| !text(&c, root, "_NIRUWM_LAYOUT").is_empty());
    let launched = || std::fs::read_to_string(&log).unwrap_or_default();
    let key = |k: &str| {
        assert!(Command::new("xdotool")
            .args(["key", "--clearmodifiers", k])
            .status()
            .unwrap()
            .success());
    };

    let until = |what: &str, f: &dyn Fn() -> bool| {
        let end = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(
                Instant::now() < end,
                "{what}: desktop {:?}, launcher {:?}, key row {:x?}, F1 row {:x?}, niruwm: {}",
                values(&c, root, "_NET_CURRENT_DESKTOP"),
                launched(),
                probe(),
                {
                    let m = c.get_keyboard_mapping(min, count).unwrap().reply().unwrap();
                    (min..=c.setup().max_keycode)
                        .filter(|&k| m.keysyms[(k - min) as usize * per..][..per].contains(&0xffbe))
                        .collect::<Vec<u8>>()
                },
                std::fs::read_to_string(dir.join("wm.log")).unwrap_or_default()
            );
            thread::sleep(Duration::from_millis(25));
        }
    };
    key("super+7");
    until("Super+7 goes to workspace 7", &|| {
        values(&c, root, "_NET_CURRENT_DESKTOP") == vec![6]
    });
    thread::sleep(Duration::from_millis(300));
    assert!(
        !launched().contains("--keys"),
        "Super+7 opened the cheat sheet: {}",
        launched()
    );
    key("super+1");
    until("Super+1 goes back", &|| {
        values(&c, root, "_NET_CURRENT_DESKTOP") == vec![0]
    });

    drop(wm);
    std::fs::remove_dir_all(dir).unwrap();
}

/// A window with a WM_CLASS and the `_NET_WM_PID` of a given process, mapped.
fn create_for_pid(c: &RustConnection, root: Window, class: &[u8], pid: u32) -> Window {
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
    c.change_property32(
        PropMode::REPLACE,
        w,
        atom(c, "_NET_WM_PID"),
        AtomEnum::CARDINAL,
        &[pid],
    )
    .unwrap();
    c.map_window(w).unwrap();
    c.flush().unwrap();
    w
}

#[test]
#[ignore = "requires isolated X server"]
fn a_program_started_from_a_terminal_swallows_it_and_gives_it_back() {
    let dir = std::env::temp_dir().join(format!("niruwm-swallow-{}", std::process::id()));
    let mut first = scratch_wm(&dir, "");
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    wait(|| text(&c, root, "_NIRUWM_SWALLOW") == "on");
    // The "terminal" belongs to this process; the "program" to a child of it.
    let other = create(&c, root, b"other\0Other\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![other]);
    let term = create_for_pid(&c, root, b"xterm\0XTerm\0", std::process::id());
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![term]);
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![other, term]);
    let term_place = geometry(&c, term);
    let child = Process(Command::new("sleep").arg("60").spawn().unwrap());
    let gui = create_for_pid(&c, root, b"mpv\0mpv\0", child.0.id());
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![other, gui]);
    wait(|| mapped(&c, gui) && !mapped(&c, term));
    assert_eq!(values(&c, root, "_NET_ACTIVE_WINDOW"), vec![gui]);
    assert_eq!(
        geometry(&c, gui),
        term_place,
        "the program takes the terminal's tile"
    );
    assert_eq!(values(&c, term, "_NIRUWM_SWALLOWED_BY"), vec![gui]);
    // A terminal opened from the terminal is not swallowed.
    let child_term = Process(Command::new("sleep").arg("60").spawn().unwrap());
    let nested = create_for_pid(&c, root, b"xterm\0XTerm\0", child_term.0.id());
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![other, gui, nested]);
    c.destroy_window(nested).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![other, gui]);

    // The manager dies while the terminal is swallowed. The X server maps the
    // terminal again; the new manager hides it again behind the program.
    let _ = first.0.kill();
    let _ = first.0.wait();
    wait(|| mapped(&c, term));
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
        let mut list = values(&c, root, "_NET_CLIENT_LIST");
        list.sort_unstable();
        let mut want = vec![other, gui];
        want.sort_unstable();
        list == want
    });
    wait(|| mapped(&c, gui) && !mapped(&c, term));

    // Off brings the terminal back at once; on again swallows the next one.
    message(&c, root, root, "_NIRUWM_COMMAND", [21, 2, 0, 0, 0]);
    wait(|| text(&c, root, "_NIRUWM_SWALLOW") == "off");
    wait(|| mapped(&c, term) && mapped(&c, gui));
    assert!(values(&c, term, "_NIRUWM_SWALLOWED_BY").is_empty());
    c.destroy_window(gui).unwrap();
    c.flush().unwrap();
    wait(|| !values(&c, root, "_NET_CLIENT_LIST").contains(&gui));
    message(&c, root, root, "_NIRUWM_COMMAND", [21, 0, 0, 0, 0]);
    wait(|| text(&c, root, "_NIRUWM_SWALLOW") == "on");
    message(&c, root, term, "_NET_ACTIVE_WINDOW", [2, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![term]);
    let again = create_for_pid(&c, root, b"feh\0feh\0", child.0.id());
    wait(|| mapped(&c, again) && !mapped(&c, term));
    // Activating the hidden terminal shows the window in its place.
    message(&c, root, term, "_NET_ACTIVE_WINDOW", [2, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![again]);
    // The program closes: the terminal is back in its place with the focus.
    let again_place = geometry(&c, again);
    c.destroy_window(again).unwrap();
    c.flush().unwrap();
    wait(|| mapped(&c, term));
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![term]);
    assert!(values(&c, root, "_NET_CLIENT_LIST").contains(&term));
    assert_eq!(
        geometry(&c, term),
        again_place,
        "the terminal takes the tile back"
    );

    // A program without `_NET_WM_PID` (plain Xt and Xaw ones) is found
    // through the X-Resource client PID: here this test process, whose
    // parent plays the terminal.
    let outer = create_for_pid(
        &c,
        root,
        b"xterm\0XTerm\0",
        std::os::unix::process::parent_id(),
    );
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![outer]);
    let plain = create(&c, root, b"xclock\0XClock\0");
    wait(|| mapped(&c, plain) && !mapped(&c, outer));
    assert_eq!(values(&c, outer, "_NIRUWM_SWALLOWED_BY"), vec![plain]);
    c.destroy_window(plain).unwrap();
    c.flush().unwrap();
    wait(|| mapped(&c, outer));

    for w in [other, term, outer] {
        c.destroy_window(w).unwrap();
    }
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| second.0.try_wait().unwrap().is_some());
    drop(child);
    drop(child_term);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn urgent_windows_are_marked_and_the_attention_key_goes_to_them() {
    let dir = std::env::temp_dir().join(format!("niruwm-urgent-{}", std::process::id()));
    let mut wm = scratch_wm(&dir, "");
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    let urgent_hint = |w: Window, on: bool| {
        let mut hints = x11rb::properties::WmHints::get(&c, w)
            .unwrap()
            .reply()
            .unwrap()
            .unwrap_or_default();
        hints.urgent = on;
        hints.set(&c, w).unwrap();
        c.flush().unwrap();
    };
    let hint_is_urgent = |w: Window| {
        x11rb::properties::WmHints::get(&c, w)
            .unwrap()
            .reply()
            .unwrap()
            .is_some_and(|h| h.urgent)
    };
    let a = create(&c, root, b"one\0One\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![a]);
    let b = create(&c, root, b"two\0Two\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![b]);
    // b goes to workspace 3 and asks for attention there.
    message(&c, root, b, "_NET_WM_DESKTOP", [2, 0, 0, 0, 0]);
    wait(|| !mapped(&c, b));
    urgent_hint(b, true);
    wait(|| values(&c, root, "_NIRUWM_URGENT") == vec![b]);
    // The focused window is never marked: its request is answered at once.
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![a]);
    urgent_hint(a, true);
    wait(|| !hint_is_urgent(a));
    assert_eq!(values(&c, root, "_NIRUWM_URGENT"), vec![b]);
    // _NET_WM_STATE_DEMANDS_ATTENTION marks an unfocused window as well.
    let k = create(&c, root, b"three\0Three\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![k]);
    let attention = atom(&c, "_NET_WM_STATE_DEMANDS_ATTENTION");
    message(&c, root, a, "_NET_WM_STATE", [1, attention, 0, 1, 0]);
    wait(|| values(&c, root, "_NIRUWM_URGENT") == vec![b, a]);
    assert!(values(&c, a, "_NET_WM_STATE").contains(&attention));
    // The key goes to the newest request first, then to the older one on
    // another workspace, clearing each window's hints.
    message(&c, root, root, "_NIRUWM_COMMAND", [20, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![a]);
    wait(|| values(&c, root, "_NIRUWM_URGENT") == vec![b]);
    assert!(!values(&c, a, "_NET_WM_STATE").contains(&attention));
    message(&c, root, root, "_NIRUWM_COMMAND", [20, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![b]);
    wait(|| values(&c, root, "_NIRUWM_URGENT").is_empty());
    assert_eq!(values(&c, root, "_NET_CURRENT_DESKTOP"), vec![2]);
    assert!(!hint_is_urgent(b), "the urgency hint is cleared on focus");
    // A client that clears its own hint is unmarked too.
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![0]);
    urgent_hint(b, true);
    wait(|| values(&c, root, "_NIRUWM_URGENT") == vec![b]);
    urgent_hint(b, false);
    wait(|| values(&c, root, "_NIRUWM_URGENT").is_empty());

    for w in [a, b, k] {
        c.destroy_window(w).unwrap();
    }
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn focusing_a_tile_keeps_floating_windows_and_dialogs_above_it() {
    // A tile that took the focus (here under the pointer) used to go to the
    // very top, over a floating window spanning two tiles (NIRUSHOT's review
    // window) and over another window's Open/Save dialog: half under one
    // tile, half over the other.
    let dir = std::env::temp_dir().join(format!("niruwm-tile-stack-{}", std::process::id()));
    let mut wm = scratch_wm(&dir, "[[rules]]\nclass = \"floaty\"\nfloating = true\n");
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    let a = create(&c, root, b"tile-a\0Tile\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![a]);
    let b = create(&c, root, b"tile-b\0Tile\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![b]);
    let f = create(&c, root, b"floaty\0Floaty\0");
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![f]);
    let d = create_dialog(&c, root, b"tile-a\0Tile\0", a, false);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![d]);
    wait(|| above(&c, root, f, a) && above(&c, root, f, b) && above(&c, root, d, a));

    // A point near the bottom of a tile, away from the floating window and
    // the dialog in the top-left area.
    let spot = |w| {
        let g = geometry(&c, w);
        (g.0 + g.2 as i16 / 2, g.1 + g.3 as i16 - 12)
    };
    for _ in 0..3 {
        // b first: the pointer may already be inside a, and moving within a
        // window is no crossing. A tile with an open dialog may hand the
        // focus to that dialog; either way the tile itself must stay below.
        for t in [b, a] {
            let (x, y) = spot(t);
            warp(&c, root, x, y);
            wait(|| {
                let active = values(&c, root, "_NET_ACTIVE_WINDOW");
                active == vec![t] || (t == a && active == vec![d])
            });
            thread::sleep(Duration::from_millis(80));
            assert!(
                above(&c, root, f, t),
                "the focused tile went over the floating window"
            );
            assert!(above(&c, root, d, t), "the focused tile went over a dialog");
        }
    }
    // The tile's own dialog stays above it.
    assert!(above(&c, root, d, a));

    for w in [d, f, b, a] {
        c.destroy_window(w).unwrap();
    }
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

/// Stubs for studio mode: a notification daemon and power-profiles-daemon
/// that keep their state in files, and a "picom" (a copy of `sleep`, so the
/// process is really called picom and can be started again from its command
/// line). Nothing reaches the real session.
fn studio_stubs(dir: &std::path::Path) -> std::path::PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let write = |name: &str, body: &str| {
        use std::os::unix::fs::PermissionsExt;
        let path = bin.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    // Every niructl run, the window manager's included, is logged.
    write(
        "niructl",
        &format!(
            "#!/bin/sh\necho \"start $*\" >> {dir}/niructl.log\n{exe} \"$@\"\nrc=$?\necho \"end $* rc=$rc\" >> {dir}/niructl.log\nexit $rc\n",
            dir = dir.display(),
            exe = env!("CARGO_BIN_EXE_niructl")
        ),
    );
    write(
        "nirunotify",
        &format!(
            "#!/bin/sh\nPATH=/usr/bin:/bin\nS={dir}/dnd\necho \"nirunotify $*\" >> {dir}/calls\n\
             [ \"$1\" = --dnd ] || exit 1\n\
             case \"$2\" in on|off) echo \"$2\" > \"$S\";; status) echo \"dnd $(cat \"$S\")\";; *) exit 1;; esac\n",
            dir = dir.display()
        ),
    );
    write(
        "powerprofilesctl",
        &format!(
            "#!/bin/sh\nPATH=/usr/bin:/bin\nS={dir}/power\necho \"powerprofilesctl $*\" >> {dir}/calls\n\
             case \"$1\" in get) cat \"$S\";; list) a=$(cat \"$S\"); for p in performance balanced power-saver; do \
             if [ \"$p\" = \"$a\" ]; then echo \"* $p:\"; else echo \"  $p:\"; fi; done;; \
             set) echo \"$2\" > \"$S\";; *) exit 1;; esac\n",
            dir = dir.display()
        ),
    );
    // This test program itself, called picom: `sleep` can be a coreutils
    // multi-call binary that refuses another name.
    std::fs::copy(std::env::current_exe().unwrap(), bin.join("picom")).unwrap();
    bin
}

/// Arguments that make a copy of this test program named `picom` wait like
/// a compositor (see `picom_stand_in`).
const PICOM_ARGS: [&str; 4] = ["picom_stand_in", "--ignored", "--exact", "--quiet"];

#[test]
#[ignore = "a stand-in process for the studio test; returns at once otherwise"]
fn picom_stand_in() {
    let me = std::env::args().next().unwrap_or_default();
    if me.ends_with("/picom") {
        thread::sleep(Duration::from_secs(3600));
    }
}

/// "picom" processes on this test's display, with their command lines.
fn test_picoms() -> Vec<(u32, String)> {
    let display = std::env::var("DISPLAY").unwrap();
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
        let zombie = stat
            .rsplit_once(')')
            .is_some_and(|(_, r)| r.trim_start().starts_with('Z'));
        let env = std::fs::read(entry.path().join("environ")).unwrap_or_default();
        let ours = env
            .split(|b| *b == 0)
            .any(|e| e == format!("DISPLAY={display}").as_bytes());
        if comm.trim() == "picom" && ours && !zombie {
            let cmd = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
            out.push((pid, String::from_utf8_lossy(&cmd).replace('\0', " ")));
        }
    }
    out
}

fn studio_ctl(dir: &std::path::Path, bin: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(bin.join("niructl"))
        .args(args)
        .env("PATH", bin)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_RUNTIME_DIR", dir)
        // No battery unless the test makes one: the result must not depend
        // on the machine that runs it ([battery] auto_profile).
        .env("NIRUCTL_POWER_SUPPLY", dir.join("power_supply"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "niructl {args:?}: {text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    text
}

fn studio_wm(dir: &std::path::Path, bin: &std::path::Path, restarted: bool) -> Process {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_niruwm"));
    cmd.env("XDG_CONFIG_HOME", dir)
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_RUNTIME_DIR", dir)
        .env("NIRUCTL_POWER_SUPPLY", dir.join("power_supply"))
        .env("HOME", dir)
        .env("PATH", bin)
        .stdout(Stdio::null());
    if restarted {
        cmd.env("NIRUWM_RESTARTED", "1");
    }
    Process(cmd.spawn().unwrap())
}

#[test]
#[ignore = "requires isolated X server"]
fn studio_mode_pauses_and_restores_exactly_and_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("niruwm-studio-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bin = studio_stubs(&dir);
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name))
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    let set =
        |name: &str, value: &str| std::fs::write(dir.join(name), format!("{value}\n")).unwrap();
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n\
         [studio]\nauto_enable = true\ndaw_classes = [\"REAPER\"]\n",
    )
    .unwrap();
    let mut first = studio_wm(&dir, &bin, false);
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    // NIRUWM 0.20 says the session has studio mode.
    wait(|| text(&c, root, "_NIRUWM_STUDIO") == "off");
    assert_eq!(
        studio_ctl(&dir, &bin, &["studio", "status"]),
        "studio off\n"
    );

    // Before: DND off, balanced, picom running with its own arguments.
    set("dnd", "off");
    set("power", "balanced");
    let picom = Process(
        Command::new(bin.join("picom"))
            .args(PICOM_ARGS)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait(|| test_picoms().len() == 1);
    let picom_cmd = test_picoms()[0].1.clone();

    let out = studio_ctl(&dir, &bin, &["studio", "on"]);
    assert!(out.starts_with("studio on\n"), "{out}");
    assert_eq!(text(&c, root, "_NIRUWM_STUDIO"), "on");
    assert_eq!(text(&c, root, "_NIRU_IDLE_INHIBIT"), "studio");
    assert_eq!(read("dnd"), "on", "{out}");
    assert_eq!(read("power"), "performance", "{out} {}", read("calls"));
    wait(|| test_picoms().is_empty());
    drop(picom);

    // The manager crashes and is started again: studio mode stays on.
    let _ = first.0.kill();
    let _ = first.0.wait();
    let mut second = studio_wm(&dir, &bin, true);
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    thread::sleep(Duration::from_millis(300));
    assert_eq!(text(&c, root, "_NIRUWM_STUDIO"), "on");
    let status = studio_ctl(&dir, &bin, &["studio", "status"]);
    assert!(
        status.starts_with("studio on\n") && status.contains("was balanced"),
        "{status}"
    );

    // Off puts back exactly what was there.
    let out = studio_ctl(&dir, &bin, &["studio", "toggle"]);
    assert!(out.starts_with("studio off\n"), "{out}");
    assert_eq!(text(&c, root, "_NIRUWM_STUDIO"), "off");
    assert_eq!(text(&c, root, "_NIRU_IDLE_INHIBIT"), "");
    assert_eq!(text(&c, root, "_NIRUWM_STUDIO_SAVED"), "");
    assert_eq!(read("dnd"), "off");
    assert_eq!(read("power"), "balanced");
    wait(|| test_picoms().len() == 1);
    assert_eq!(
        test_picoms()[0].1,
        picom_cmd,
        "picom runs again as it ran before"
    );
    let restarted_picom = test_picoms()[0].0;

    // Already DND, already performance, no compositor: nothing to undo.
    unsafe_kill(restarted_picom);
    wait(|| test_picoms().is_empty());
    set("dnd", "on");
    set("power", "performance");
    let _ = std::fs::remove_file(dir.join("calls"));
    studio_ctl(&dir, &bin, &["studio", "on"]);
    studio_ctl(&dir, &bin, &["studio", "off"]);
    assert_eq!(read("dnd"), "on", "DND that was on stays on");
    assert_eq!(read("power"), "performance");
    assert!(
        test_picoms().is_empty(),
        "no compositor is started that was not running"
    );
    assert!(!read("calls").contains("--dnd off") && !read("calls").contains("set "));

    // A change made while it is on is left alone.
    set("dnd", "off");
    set("power", "balanced");
    studio_ctl(&dir, &bin, &["studio", "on"]);
    set("power", "power-saver");
    set("dnd", "off");
    let out = studio_ctl(&dir, &bin, &["studio", "off"]);
    assert_eq!(read("power"), "power-saver", "{out}");
    assert_eq!(read("dnd"), "off");

    // [battery] auto_profile (on by default): unplugged, studio mode off
    // gives the battery's profile, not the one it found on AC.
    let supply = dir.join("power_supply");
    for (name, files) in [
        ("BAT0", &[("type", "Battery"), ("scope", "System")][..]),
        ("AC", &[("type", "Mains"), ("online", "0")]),
    ] {
        std::fs::create_dir_all(supply.join(name)).unwrap();
        for (k, v) in files {
            std::fs::write(supply.join(name).join(k), format!("{v}\n")).unwrap();
        }
    }
    set("power", "balanced");
    studio_ctl(&dir, &bin, &["studio", "on"]);
    assert_eq!(read("power"), "performance");
    let out = studio_ctl(&dir, &bin, &["studio", "off"]);
    assert_eq!(read("power"), "power-saver", "{out}");
    assert!(out.contains("follows the power source"), "{out}");
    // A profile chosen by hand holds until the next plug or unplug.
    studio_ctl(&dir, &bin, &["powerprofile", "set", "balanced"]);
    studio_ctl(&dir, &bin, &["studio", "on"]);
    let out = studio_ctl(&dir, &bin, &["studio", "off"]);
    assert_eq!(read("power"), "balanced", "{out}");
    // While studio mode is on, `powerprofile auto` leaves it alone.
    studio_ctl(&dir, &bin, &["studio", "on"]);
    let out = studio_ctl(&dir, &bin, &["powerprofile", "auto"]);
    assert!(out.contains("studio mode holds"), "{out}");
    assert_eq!(read("power"), "performance");
    studio_ctl(&dir, &bin, &["studio", "off"]);
    assert_eq!(read("power"), "power-saver");
    std::fs::remove_dir_all(&supply).unwrap();
    set("power", "power-saver");

    // A DAW window turns it on and closing the last one turns it off, after
    // a grace period: a splash screen closing before the main window opens
    // must not flip the mode off and on.
    let _ = std::fs::remove_file(dir.join("calls"));
    let splash = create(&c, root, b"reaper\0REAPER\0");
    wait(|| text(&c, root, "_NIRUWM_STUDIO") == "on");
    c.destroy_window(splash).unwrap();
    c.flush().unwrap();
    thread::sleep(Duration::from_millis(600));
    let daw = create(&c, root, b"reaper\0REAPER\0");
    let plugin = create(&c, root, b"reaper\0REAPER\0");
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 2);
    thread::sleep(Duration::from_millis(9500));
    assert_eq!(
        text(&c, root, "_NIRUWM_STUDIO"),
        "on",
        "no flip for a splash"
    );
    assert_eq!(
        read("calls").matches("--dnd on").count(),
        1,
        "calls {:?} saved {:?} log {}",
        read("calls"),
        text(&c, root, "_NIRUWM_STUDIO_SAVED"),
        read("niructl.log")
    );
    c.destroy_window(daw).unwrap();
    c.flush().unwrap();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        text(&c, root, "_NIRUWM_STUDIO"),
        "on",
        "one DAW window is still open"
    );
    c.destroy_window(plugin).unwrap();
    c.flush().unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    while text(&c, root, "_NIRUWM_STUDIO") != "off" {
        assert!(
            Instant::now() < end,
            "auto off: calls {:?} saved {:?}",
            read("calls"),
            text(&c, root, "_NIRUWM_STUDIO_SAVED")
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(read("power"), "power-saver");
    // Turned on by hand, it stays on when the DAW closes.
    studio_ctl(&dir, &bin, &["studio", "on"]);
    let daw = create(&c, root, b"reaper\0REAPER\0");
    wait(|| values(&c, root, "_NET_CLIENT_LIST").contains(&daw));
    c.destroy_window(daw).unwrap();
    c.flush().unwrap();
    thread::sleep(Duration::from_millis(10_000));
    assert_eq!(text(&c, root, "_NIRUWM_STUDIO"), "on");
    // The session end restores DND and power without starting picom.
    studio_ctl(&dir, &bin, &["studio", "off", "--session-end"]);
    assert_eq!(text(&c, root, "_NIRUWM_STUDIO"), "off");
    assert_eq!(read("power"), "power-saver");

    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| second.0.try_wait().unwrap().is_some());
    for (pid, _) in test_picoms() {
        unsafe_kill(pid);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

fn unsafe_kill(pid: u32) {
    let _ = Command::new("kill").arg(pid.to_string()).status();
}

#[test]
#[ignore = "requires isolated X server"]
fn niruidle_honours_the_session_inhibition() {
    let dir = std::env::temp_dir().join(format!("niruwm-idle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = std::fs::File::create(dir.join("log")).unwrap();
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let mut idle = Process(
        Command::new(env!("CARGO_BIN_EXE_niruidle"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("HOME", &dir)
            .env("PATH", "/nonexistent")
            // Never the session bus of whoever runs the tests.
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    wait(|| {
        text(&c, root, "_NIRUIDLE_FEATURES")
            .split_whitespace()
            .any(|f| f == "session-inhibit")
    });
    c.change_property8(
        PropMode::REPLACE,
        root,
        atom(&c, "_NIRU_IDLE_INHIBIT"),
        atom(&c, "UTF8_STRING"),
        b"studio",
    )
    .unwrap();
    c.flush().unwrap();
    let logged = |needle: &str| {
        std::fs::read_to_string(dir.join("log"))
            .unwrap_or_default()
            .contains(needle)
    };
    let end = Instant::now() + Duration::from_secs(12);
    while !logged("idle inhibition on (session)") {
        assert!(Instant::now() < end, "niruidle did not see the inhibition");
        thread::sleep(Duration::from_millis(100));
    }
    c.delete_property(root, atom(&c, "_NIRU_IDLE_INHIBIT"))
        .unwrap();
    c.flush().unwrap();
    let end = Instant::now() + Duration::from_secs(12);
    while !logged("idle inhibition off") {
        assert!(Instant::now() < end, "niruidle did not see the end");
        thread::sleep(Duration::from_millis(100));
    }
    let _ = idle.0.kill();
    let _ = idle.0.wait();
    std::fs::remove_dir_all(dir).unwrap();
}

/// Width, height and RGB bytes of a binary PPM.
fn read_ppm(path: &std::path::Path) -> (usize, usize, Vec<u8>) {
    let bytes = std::fs::read(path).unwrap();
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..i]).to_string());
    }
    assert_eq!(fields[0], "P6");
    let (w, h): (usize, usize) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    let data = bytes[i + 1..].to_vec();
    assert_eq!(data.len(), w * h * 3);
    (w, h, data)
}

#[test]
#[ignore = "requires isolated X server"]
fn niructl_do_switches_workspaces_goes_to_windows_and_captures_thumbnails() {
    let dir = std::env::temp_dir().join(format!("niruwm-do-{}", std::process::id()));
    let run_dir = dir.join("run");
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::create_dir_all(&run_dir).unwrap();
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
            .env("XDG_RUNTIME_DIR", &run_dir)
            .env("NIRU_RANDR_EMU", &layouts)
            .env("NIRUWM_OVERVIEW_TIMING", "1")
            .env("PATH", "/nonexistent")
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait(|| {
        let monitors = text(&c, root, "_NIRUWM_MONITORS");
        monitors.contains("LEFT") && monitors.contains("RIGHT")
    });
    let ctl = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_niructl"))
            .env("XDG_CONFIG_HOME", &dir)
            .arg("do")
            .args(args)
            .output()
            .unwrap()
    };
    // The pointer on the left screen, which has the focus: a window that
    // unmaps must not hand the focus to the screen under the pointer.
    warp(&c, root, 100, 300);
    assert!(ctl(&["goto", "0", "1"]).status.success());
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![0]);

    // A red window on workspace 1 of the left screen.
    let red = c.generate_id().unwrap();
    c.create_window(
        0,
        red,
        root,
        20,
        40,
        320,
        240,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().background_pixel(0xff0000),
    )
    .unwrap();
    c.map_window(red).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST").contains(&red));
    thread::sleep(Duration::from_millis(300));

    // `niructl do tag 2` is Super+2: workspace 2 on the focused screen.
    let out = ctl(&["tag", "2"]);
    assert!(out.status.success(), "niructl do tag 2: {out:?}");
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![1]);

    // Leaving workspace 1 left a thumbnail of the left screen, red where the
    // window was, right side up and in the right colours.
    let thumbs = run_dir.join("niruwm/overview");
    let thumb = thumbs.join("mon0-tag1.ppm");
    wait(|| thumb.exists());
    let (w, h, data) = read_ppm(&thumb);
    assert!(w.max(h) <= 384 && w > 0 && h > 0, "{w}x{h}");
    let scale = f64::from(left_w) / w as f64;
    let px = |x: f64, y: f64| {
        let (x, y) = ((x / scale) as usize, (y / scale) as usize);
        let i = (y * w + x) * 3;
        (data[i], data[i + 1], data[i + 2])
    };
    // The managed window is tiled over the whole left screen: its middle is
    // red, and the thumbnail has the screen's aspect ratio.
    assert_eq!(
        px(f64::from(left_w) / 2.0, f64::from(screen_h) / 2.0),
        (255, 0, 0)
    );
    let aspect = f64::from(left_w) / f64::from(screen_h);
    assert!((w as f64 / h as f64 - aspect).abs() < 0.05);
    let mode = std::fs::metadata(&thumb).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(mode.mode() & 0o777, 0o600);

    // Programs and quit are refused with status 2, unknown actions 1.
    for refused in [&["shell", "xterm"][..], &["start", "xterm"], &["quit"]] {
        let out = ctl(refused);
        assert_eq!(out.status.code(), Some(2), "{refused:?}: {out:?}");
    }
    assert_eq!(ctl(&["frobnicate"]).status.code(), Some(1));
    assert!(
        wm.0.try_wait().unwrap().is_none(),
        "quit must not reach the WM"
    );

    // A blue window on workspace 1 of the right screen.
    warp(&c, root, i16::try_from(left_w).unwrap() + 100, 300);
    assert!(ctl(&["goto", "1", "1"]).status.success());
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![10]);
    let blue = c.generate_id().unwrap();
    c.create_window(
        0,
        blue,
        root,
        i16::try_from(left_w).unwrap() + 20,
        40,
        320,
        240,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().background_pixel(0x0000ff),
    )
    .unwrap();
    c.map_window(blue).unwrap();
    c.flush().unwrap();
    wait(|| values(&c, root, "_NET_CLIENT_LIST").contains(&blue));
    assert_eq!(
        values(&c, blue, "_NET_WM_DESKTOP"),
        vec![10],
        "blue window placement"
    );
    thread::sleep(Duration::from_millis(300));

    // goto: screen 1 (RIGHT), workspace 3 = global desktop 12. The right
    // screen's thumbnail shows the right screen, not the left one.
    assert!(ctl(&["goto", "1", "3"]).status.success());
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![12]);
    let right_thumb = thumbs.join("mon1-tag1.ppm");
    wait(|| right_thumb.exists());
    let (rw, rh, rdata) = read_ppm(&right_thumb);
    let middle = ((rh / 2) * rw + rw / 2) * 3;
    assert_eq!(&rdata[middle..middle + 3], &[0, 0, 255]);
    // goto with a window: back to the red one, focused.
    warp(&c, root, 100, 300);
    let id = format!("{red:#x}");
    assert!(ctl(&["goto", "0", "1", &id]).status.success());
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![0]);
    wait(|| values(&c, root, "_NET_ACTIVE_WINDOW") == vec![red]);
    assert_eq!(ctl(&["goto", "5", "1"]).status.code(), Some(1));

    // The overview action writes state.json (nirulauncher is not on PATH).
    assert!(ctl(&["overview"]).status.success());
    let state_path = thumbs.join("state.json");
    wait(|| state_path.exists());
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(state["version"], 1);
    let left = &state["monitors"][0];
    assert_eq!(left["name"], "LEFT");
    assert_eq!(left["focused"], true);
    assert_eq!(left["current_tag"], 1);
    assert_eq!(left["tags"][0]["tag"], 1);
    assert_eq!(left["tags"][0]["thumbnail"], "mon0-tag1.ppm");
    assert_eq!(left["tags"][0]["windows"][0]["id"], id.as_str());
    assert_eq!(left["tags"][0]["windows"][0]["focused"], true);
    assert_eq!(state["monitors"][1]["current_tag"], 3);

    // A lock deletes every thumbnail and refuses `do`.
    message(&c, root, root, "_NIRUWM_LOCK", [1, 0, 0, 0, 0]);
    wait(|| !thumb.exists() && !state_path.exists());
    assert_eq!(ctl(&["tag", "3"]).status.code(), Some(2));
    assert_eq!(values(&c, root, "_NET_CURRENT_DESKTOP"), vec![0]);
    message(&c, root, root, "_NIRUWM_LOCK", [0, 0, 0, 0, 0]);
    thread::sleep(Duration::from_millis(200));
    assert!(ctl(&["tag", "3"]).status.success());
    wait(|| values(&c, root, "_NET_CURRENT_DESKTOP") == vec![2]);

    // `--list --json` describes the accepted actions.
    let out = ctl(&["--list", "--json"]);
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["name"] == "tag" && a["arg"] == "N"));

    c.destroy_window(red).unwrap();
    c.destroy_window(blue).unwrap();
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

fn set_keep_mapped(c: &RustConnection, w: Window, on: bool) {
    let a = atom(c, "_NIRUWM_KEEP_MAPPED");
    if on {
        c.change_property32(PropMode::REPLACE, w, a, AtomEnum::CARDINAL, &[1])
            .unwrap();
    } else {
        c.delete_property(w, a).unwrap();
    }
    c.flush().unwrap();
}

/// Entirely right of the screen: the parked place of a kept-mapped window.
fn parked(c: &RustConnection, w: Window, screen_w: u16) -> bool {
    let reply = c
        .translate_coordinates(w, c.setup().roots[0].root, 0, 0)
        .unwrap()
        .reply()
        .unwrap();
    i32::from(reply.dst_x) >= i32::from(screen_w)
}

#[test]
#[ignore = "requires isolated X server"]
fn keep_mapped_windows_stay_mapped_outside_the_screen_while_hidden() {
    let dir = std::env::temp_dir().join(format!("niruwm-keep-{}", std::process::id()));
    let mut wm = scratch_wm(&dir, "");
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    let screen_w = c.setup().roots[s].width_in_pixels;
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());

    // The audience window asks before it maps; a plain window beside it.
    let audience = c.generate_id().unwrap();
    c.create_window(
        0,
        audience,
        root,
        10,
        10,
        640,
        360,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    c.change_property8(
        PropMode::REPLACE,
        audience,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"nirupres-audience\0NIRUPRES\0",
    )
    .unwrap();
    set_keep_mapped(&c, audience, true);
    c.map_window(audience).unwrap();
    c.flush().unwrap();
    let plain = create(&c, root, b"plain\0Plain\0");
    wait(|| values(&c, root, "_NET_CLIENT_LIST") == vec![audience, plain]);
    wait(|| mapped(&c, audience) && mapped(&c, plain));
    let shown = geometry(&c, audience);
    assert!(!parked(&c, audience, screen_w));

    // Workspace 2: the plain window unmaps, the audience window stays mapped
    // outside the screen at its own size, and has no focus.
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 1, 0, 0, 0]);
    wait(|| !mapped(&c, plain));
    wait(|| parked(&c, audience, screen_w));
    assert!(mapped(&c, audience), "a kept window is never unmapped");
    let g = geometry(&c, audience);
    assert_eq!((g.2, g.3), (shown.2, shown.3), "parked at its own size");
    assert_ne!(values(&c, root, "_NET_ACTIVE_WINDOW"), vec![audience]);
    let focus = c.get_input_focus().unwrap().reply().unwrap().focus;
    assert_ne!(focus, audience, "keyboard focus left the hidden window");
    // Still a window of workspace 1 for everyone else.
    assert_eq!(values(&c, audience, "_NET_WM_DESKTOP"), vec![0]);

    // Back on workspace 1 it returns to its place in the layout.
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 0, 0, 0, 0]);
    wait(|| mapped(&c, plain));
    wait(|| geometry(&c, audience) == shown);

    // The property removed while hidden: unmapped then. Set again while
    // hidden: mapped again, outside the screen.
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 1, 0, 0, 0]);
    wait(|| parked(&c, audience, screen_w));
    set_keep_mapped(&c, audience, false);
    wait(|| !mapped(&c, audience));
    set_keep_mapped(&c, audience, true);
    wait(|| mapped(&c, audience));
    assert!(parked(&c, audience, screen_w), "mapped outside the screen");
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 0, 0, 0, 0]);
    wait(|| geometry(&c, audience) == shown);

    // A hidden scratchpad with the property stays mapped too.
    message(&c, root, root, "_NIRUWM_COMMAND", [18, 2, audience, 0, 0]);
    wait(|| text(&c, root, "_NIRUWM_SCRATCHPADS").contains(&format!("2\thidden\t{audience}")));
    wait(|| parked(&c, audience, screen_w));
    assert!(mapped(&c, audience));
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 2, 0, 0, 0]);
    wait(|| !parked(&c, audience, screen_w));
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 2, 0, 0, 0]);
    wait(|| parked(&c, audience, screen_w));
    message(&c, root, root, "_NIRUWM_COMMAND", [15, 2, 1, 0, 0]);
    wait(|| !parked(&c, audience, screen_w));
    message(&c, root, root, "_NIRUWM_COMMAND", [17, 0, 0, 0, 0]);
    wait(|| values(&c, root, "_NET_CLIENT_LIST").len() == 2);

    // Its client withdraws it while it is parked: it is gone from the
    // workspace and does not come back by itself.
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 1, 0, 0, 0]);
    wait(|| parked(&c, audience, screen_w));
    c.unmap_window(audience).unwrap();
    c.flush().unwrap();
    wait(|| !mapped(&c, audience));
    message(&c, root, root, "_NIRUWM_COMMAND", [13, 0, 0, 0, 0]);
    wait(|| mapped(&c, plain));
    thread::sleep(Duration::from_millis(200));
    assert!(!mapped(&c, audience), "a withdrawn window stays withdrawn");
    // Mapping it again brings it back.
    c.map_window(audience).unwrap();
    c.flush().unwrap();
    wait(|| mapped(&c, audience) && !parked(&c, audience, screen_w));

    c.destroy_window(audience).unwrap();
    c.destroy_window(plain).unwrap();
    c.flush().unwrap();
    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires isolated X server"]
fn present_remembers_dnd_and_ends_when_the_watched_process_ends() {
    let dir = std::env::temp_dir().join(format!("niruwm-present-{}", std::process::id()));
    let bin = dir.join("bin");
    let runtime = dir.join("run");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    // A notification provider that keeps DND in a file.
    let dnd = dir.join("dnd");
    std::fs::write(&dnd, "off").unwrap();
    std::fs::write(
        bin.join("nirunotify"),
        format!(
            "#!/bin/sh\nf='{}'\ncase \"$2\" in\n  on|off) printf %s \"$2\" > \"$f\" ;;\n  status) echo \"dnd $(cat \"$f\")\" ;;\nesac\n",
            dnd.display()
        ),
    )
    .unwrap();
    let niructl = std::path::PathBuf::from(env!("CARGO_BIN_EXE_niructl"));
    std::os::unix::fs::symlink(&niructl, bin.join("niructl")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        bin.join("nirunotify"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    std::fs::create_dir_all(dir.join("niruwm")).unwrap();
    std::fs::write(
        dir.join("niruwm/config.toml"),
        "[bar]\nenabled = false\n[wallpaper]\nenabled = false\n",
    )
    .unwrap();
    let mut wm = Process(
        Command::new(env!("CARGO_BIN_EXE_niruwm"))
            .env("XDG_CONFIG_HOME", &dir)
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("HOME", &dir)
            .env("PATH", &path)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (c, s) = x11rb::connect(None).unwrap();
    let root = c.setup().roots[s].root;
    wait(|| !values(&c, root, "_NET_SUPPORTING_WM_CHECK").is_empty());
    let ctl = |args: &[&str]| -> String {
        let out = Command::new(&niructl)
            .args(args)
            .env("XDG_CONFIG_HOME", &dir)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("HOME", &dir)
            .env("PATH", &path)
            .output()
            .unwrap();
        assert!(out.status.success(), "niructl {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let dnd_now = || std::fs::read_to_string(&dnd).unwrap();

    assert_eq!(ctl(&["present", "status"]), "off");
    // By hand: DND on while presenting, off again after.
    ctl(&["present", "on"]);
    assert_eq!(ctl(&["present", "status"]), "on");
    assert_eq!(values(&c, root, "_NIRUWM_PRESENTING"), vec![1]);
    assert_eq!(dnd_now(), "on");
    ctl(&["present", "off"]);
    assert_eq!(ctl(&["present", "status"]), "off");
    assert!(values(&c, root, "_NIRUWM_PRESENTING").is_empty());
    assert_eq!(dnd_now(), "off");

    // DND that was on stays on.
    std::fs::write(&dnd, "on").unwrap();
    ctl(&["present", "on"]);
    ctl(&["present", "off"]);
    assert_eq!(dnd_now(), "on");
    std::fs::write(&dnd, "off").unwrap();

    // A process that is not running is refused.
    let out = Command::new(&niructl)
        .args(["present", "on", "--pid", "999999999"])
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(!out.status.success());

    // Watched: ends by itself shortly after the process ends.
    let mut sleeper = Command::new("sleep").arg("60").spawn().unwrap();
    let pid = sleeper.id().to_string();
    ctl(&["present", "on", "--pid", &pid]);
    assert_eq!(dnd_now(), "on");
    thread::sleep(Duration::from_millis(2500));
    assert_eq!(ctl(&["present", "status"]), "on", "still running: still on");
    sleeper.kill().unwrap();
    sleeper.wait().unwrap();
    let end = Instant::now() + Duration::from_secs(8);
    while ctl(&["present", "status"]) != "off" {
        assert!(
            Instant::now() < end,
            "presenting did not end with its process"
        );
        thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(dnd_now(), "off", "DND restored");
    let leftovers: Vec<String> = std::fs::read_dir(runtime.join("niruwm"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("presenting-") && !n.ends_with(".lock"))
        .collect();
    assert!(leftovers.is_empty(), "state file removed: {leftovers:?}");

    message(&c, root, root, "_NIRUWM_COMMAND", [5, 0, 0, 0, 0]);
    wait(|| wm.0.try_wait().unwrap().is_some());
    std::fs::remove_dir_all(dir).unwrap();
}
