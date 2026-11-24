//! Programs that start with the session and come back after sleep.
//!
//! - The user's XDG autostart entries (`~/.config/autostart/*.desktop`, what
//!   Nextcloud, niruprodrive and other tray programs write) start at login,
//!   next to NIRUWM's own `[[autostart.apps]]`. System entries in
//!   `/etc/xdg/autostart` are not run: NIRUWM's session starts its own
//!   helpers (polkit, notifications, keyring) and the system list is mostly
//!   other desktops' daemons.
//! - Before sleep, `niructl autostart snapshot` writes down which of these
//!   programs run. After the wake-up `niructl autostart revive` starts the
//!   ones that are gone: a program cannot be quit by hand while the machine
//!   sleeps, so one missing afterwards died with the suspend.

use std::path::{Path, PathBuf};

/// One program that starts with the session.
#[derive(Clone, Debug, PartialEq)]
pub struct Start {
    /// A name for messages: the entry's Name, or the program.
    pub name: String,
    /// Argument list (field codes removed), never through a shell.
    pub command: Vec<String>,
}

impl Start {
    /// The program name a running process of it carries.
    pub fn program(&self) -> String {
        self.command
            .first()
            .map(|p| {
                Path::new(p)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
}

/// Split an Exec line as the desktop entry specification says: quoted
/// arguments, backslash escapes inside quotes, field codes dropped.
pub fn split_exec(exec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter()
        .filter_map(|a| {
            if a.len() == 2 && a.starts_with('%') {
                return None; // %f %F %u %U %i %c %k ...
            }
            Some(a.replace("%%", "%"))
        })
        .filter(|a| !a.is_empty())
        .collect()
}

fn in_list(list: &str, desktop: &str) -> bool {
    list.split(';')
        .any(|d| !d.is_empty() && desktop.split(':').any(|own| own.eq_ignore_ascii_case(d)))
}

fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).is_file();
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(program).is_file()))
        .unwrap_or(false)
}

/// The entry in `text`, when it is one this session starts.
pub fn parse_entry(text: &str, desktop: &str, file: &str) -> Option<Start> {
    let mut in_main = false;
    let (mut name, mut exec, mut kind) = (None, None, None);
    let (mut only, mut not, mut try_exec) = (None, None, None);
    let mut off = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "Type" => kind = Some(v.to_string()),
            "Name" => name = Some(v.to_string()),
            "Exec" => exec = Some(v.to_string()),
            "TryExec" => try_exec = Some(v.to_string()),
            "OnlyShowIn" => only = Some(v.to_string()),
            "NotShowIn" => not = Some(v.to_string()),
            "Hidden" => off |= v.eq_ignore_ascii_case("true"),
            "X-GNOME-Autostart-enabled" => off |= v.eq_ignore_ascii_case("false"),
            _ => {}
        }
    }
    if off || kind.as_deref().is_some_and(|k| k != "Application") {
        return None;
    }
    if only.as_deref().is_some_and(|l| !in_list(l, desktop)) {
        return None;
    }
    if not.as_deref().is_some_and(|l| in_list(l, desktop)) {
        return None;
    }
    if try_exec.as_deref().is_some_and(|t| !on_path(t)) {
        return None;
    }
    let command = split_exec(exec.as_deref()?);
    command.first()?;
    Some(Start {
        name: name.unwrap_or_else(|| file.trim_end_matches(".desktop").to_string()),
        command,
    })
}

/// The user's XDG autostart entries that this session starts, by file name.
pub fn user_entries() -> Vec<Start> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "NIRUWM".into());
    let dir = config_home().join("autostart");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
        .collect();
    files.sort();
    files
        .iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(p).ok()?;
            let file = p.file_name()?.to_string_lossy().into_owned();
            parse_entry(&text, &desktop, &file)
        })
        .collect()
}

/// Whether a process of this user runs `program` (its argv[0], its binary
/// or its short name).
pub fn running(program: &str) -> bool {
    if program.is_empty() {
        return false;
    }
    let uid = unsafe { libc::getuid() };
    let short: String = program.chars().take(15).collect();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return false;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        use std::os::unix::fs::MetadataExt;
        if e.metadata().map(|m| m.uid()).ok() != Some(uid) {
            continue;
        }
        let base = format!("/proc/{pid}");
        if let Ok(cmd) = std::fs::read(format!("{base}/cmdline")) {
            if let Some(argv0) = cmd.split(|b| *b == 0).next() {
                let argv0 = String::from_utf8_lossy(argv0);
                let b = argv0.rsplit('/').next().unwrap_or("");
                // A script run as "sh /path/prog" or "python3 /path/prog".
                let second = cmd
                    .split(|b| *b == 0)
                    .nth(1)
                    .map(|a| {
                        String::from_utf8_lossy(a)
                            .rsplit('/')
                            .next()
                            .unwrap_or("")
                            .to_string()
                    })
                    .unwrap_or_default();
                if b == program || second == program {
                    return true;
                }
            }
        }
        if let Ok(exe) = std::fs::read_link(format!("{base}/exe")) {
            if exe
                .file_name()
                .is_some_and(|n| n.to_string_lossy() == program)
            {
                return true;
            }
        }
        if std::fs::read_to_string(format!("{base}/comm")).is_ok_and(|c| c.trim_end() == short) {
            return true;
        }
    }
    false
}

/// Start `s` on its own (its own session, in the home folder), not waiting.
pub fn spawn(s: &Start) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut c = std::process::Command::new(&s.command[0]);
    c.args(&s.command[1..])
        .stdin(std::process::Stdio::null())
        .current_dir(std::env::var_os("HOME").unwrap_or_else(|| "/".into()));
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn().map(|_| ())
}

/// Where the list of running programs is kept across a suspend.
pub fn snapshot_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let display: String = display
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    dir.join(format!("niruwm-autostart-{display}.before-sleep"))
}

/// One line per program: name TAB the arguments joined by US (0x1f).
pub fn write_snapshot(running: &[Start]) -> std::io::Result<()> {
    let mut text = String::new();
    for s in running {
        text.push_str(&s.name.replace(['\t', '\n'], " "));
        text.push('\t');
        text.push_str(&s.command.join("\u{1f}").replace('\n', " "));
        text.push('\n');
    }
    let path = snapshot_path();
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

pub fn read_snapshot(text: &str) -> Vec<Start> {
    text.lines()
        .filter_map(|l| {
            let (name, cmd) = l.split_once('\t')?;
            let command: Vec<String> = cmd.split('\u{1f}').map(str::to_string).collect();
            command.first().filter(|p| !p.is_empty())?;
            Some(Start {
                name: name.to_string(),
                command,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_lines_are_split_and_field_codes_dropped() {
        assert_eq!(
            split_exec(r#"/usr/bin/nextcloud --background %U"#),
            vec!["/usr/bin/nextcloud", "--background"]
        );
        assert_eq!(
            split_exec(r#""/opt/My App/run" --name "a \"b\"" 100%%"#),
            vec!["/opt/My App/run", "--name", "a \"b\"", "100%"]
        );
    }

    #[test]
    fn hidden_disabled_and_other_desktops_entries_are_skipped() {
        let base = "[Desktop Entry]\nType=Application\nName=Cloud\nExec=cloud --tray\n";
        let s = parse_entry(base, "NIRUWM", "cloud.desktop").unwrap();
        assert_eq!(s.name, "Cloud");
        assert_eq!(s.program(), "cloud");
        for extra in [
            "Hidden=true\n",
            "X-GNOME-Autostart-enabled=false\n",
            "OnlyShowIn=GNOME;KDE;\n",
            "NotShowIn=NIRUWM;\n",
            "TryExec=/nonexistent/cloud\n",
        ] {
            let text = format!("{base}{extra}");
            assert!(
                parse_entry(&text, "NIRUWM", "cloud.desktop").is_none(),
                "{extra}"
            );
        }
        let only = format!("{base}OnlyShowIn=XFCE;NIRUWM;\n");
        assert!(parse_entry(&only, "NIRUWM", "cloud.desktop").is_some());
        // Keys of another group do not count.
        let action = format!("{base}[Desktop Action x]\nHidden=true\n");
        assert!(parse_entry(&action, "NIRUWM", "cloud.desktop").is_some());
    }

    #[test]
    fn the_snapshot_keeps_names_and_arguments() {
        let s = vec![Start {
            name: "Proton Mail Bridge".into(),
            command: vec!["/usr/bin/bridge".into(), "--no-window".into(), "a b".into()],
        }];
        let mut text = String::new();
        for x in &s {
            text.push_str(&format!("{}\t{}\n", x.name, x.command.join("\u{1f}")));
        }
        assert_eq!(read_snapshot(&text), s);
    }

    #[test]
    fn this_test_process_is_found_running() {
        let me = std::env::current_exe().unwrap();
        let name = me.file_name().unwrap().to_string_lossy().into_owned();
        assert!(running(&name));
        assert!(!running("no-such-program-niruwm"));
    }
}
