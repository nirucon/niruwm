//! Remember the windows of a session and bring them back.
//!
//! The point of ten workspaces is that they accumulate: the browser on one,
//! the editor on another, a terminal you left in the third. Closing the
//! session throws that away, and rebuilding it by hand every morning is the
//! cost that makes a tiling desktop feel like work.
//!
//! What is saved is deliberately small: which program, on which workspace, on
//! which screen, and whether it floated. The program's own memory covers the
//! rest (a browser reopens its tabs, an editor its file list); a window manager
//! guessing at window titles would get that wrong more often than right.
//!
//! Saving happens when the session ends and on demand. Restoring happens at
//! start. Neither ever fails the session: a missing file means an empty
//! session, an unreadable line is skipped, and a program that will not start
//! is reported and stepped over.

use crate::config::Result;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// One program to start again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The command, already split into arguments as it was started.
    pub program: Vec<String>,
    /// Workspace index, 0-based.
    pub tag: u32,
    /// Screen index, 0-based.
    pub monitor: usize,
    pub floating: bool,
    /// This window had the input focus when the session was saved.
    pub focused: bool,
}

impl Entry {
    /// The line written to the file. Tabs separate fields, so a program with
    /// spaces in its path survives.
    pub fn to_line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.program.join("\u{1f}"),
            self.tag,
            self.monitor,
            if self.floating { 1 } else { 0 },
            if self.focused { 1 } else { 0 }
        )
    }

    /// Read one line back. A malformed line is `None`, not an error: one bad
    /// entry must not cost the others.
    pub fn from_line(line: &str) -> Option<Self> {
        let mut fields = line.split('\t');
        let program: Vec<String> = fields
            .next()?
            .split('\u{1f}')
            .map(str::to_string)
            .filter(|p| !p.is_empty())
            .collect();
        if program.is_empty() {
            return None;
        }
        let tag: u32 = fields.next()?.parse().ok()?;
        let monitor: usize = fields.next()?.parse().ok()?;
        let floating = fields.next().map(|f| f == "1").unwrap_or(false);
        // The focus flag was added after the first session files. A missing
        // field means "unknown", not "focus this window".
        let focused = fields.next().map(|f| f == "1").unwrap_or(false);
        Some(Self {
            program,
            tag: tag.min(8),
            monitor: monitor.min(15),
            floating,
            focused,
        })
    }
}

/// Where the saved session lives.
pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
    base.join("niruwm").join("session.tsv")
}

/// Write the entries, oldest first.
pub fn save(entries: &[Entry]) -> Result<()> {
    let path = path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut text = String::from("# niruwm session: program, workspace, screen, floating\n");
    for e in entries {
        text.push_str(&e.to_line());
        text.push('\n');
    }
    // Write beside and rename, so a crash mid-write never leaves half a file.
    let tmp = path.with_extension("tsv.new");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Read the entries. A missing file is an empty session.
pub fn load() -> Vec<Entry> {
    let Ok(text) = fs::read_to_string(path()) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(Entry::from_line)
        .collect()
}

/// Remove the file, for "start empty".
pub fn clear() -> Result<()> {
    match fs::remove_file(path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Start order: workspace by workspace, screen by screen, so programs that
/// remember their tiling neighbours find them where they were.
pub fn ordered(entries: &[Entry]) -> Vec<Entry> {
    let mut by_key: BTreeMap<(usize, u32), Vec<Entry>> = BTreeMap::new();
    for e in entries {
        by_key
            .entry((e.monitor, e.tag))
            .or_default()
            .push(e.clone());
    }
    by_key.into_values().flatten().collect()
}

/// Group entries by the command that creates them.
///
/// A browser or terminal can own several windows from one process. Starting
/// the same command once per window would either create duplicates or ignore
/// the saved workspace for all but the first window.
pub fn launches(entries: &[Entry]) -> Vec<(Vec<String>, Vec<Entry>)> {
    let mut launches: Vec<(Vec<String>, Vec<Entry>)> = Vec::new();
    for e in ordered(entries) {
        if let Some((_, windows)) = launches
            .iter_mut()
            .find(|(command, _)| *command == e.program)
        {
            windows.push(e);
        } else {
            launches.push((e.program.clone(), vec![e]));
        }
    }
    launches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_survives_a_round_trip() {
        let e = Entry {
            program: vec!["kitty".into(), "--class".into(), "a b".into()],
            tag: 3,
            monitor: 1,
            floating: true,
            focused: true,
        };
        assert_eq!(Entry::from_line(&e.to_line()), Some(e));
    }

    #[test]
    fn a_broken_line_is_skipped_not_fatal() {
        assert_eq!(Entry::from_line(""), None);
        assert_eq!(Entry::from_line("kitty"), None);
        assert_eq!(Entry::from_line("\t1\t0\t0"), None);
        // Out-of-range numbers are clamped, not dropped: a session saved on
        // three screens still starts on one.
        let e = Entry::from_line("kitty\t99\t99\t0").expect("still readable");
        assert_eq!(e.tag, 8);
        assert_eq!(e.monitor, 15);
    }

    #[test]
    fn start_order_is_per_screen_then_workspace() {
        let mk = |program: &str, tag: u32, monitor: usize| Entry {
            program: vec![program.into()],
            tag,
            monitor,
            floating: false,
            focused: false,
        };
        let entries = vec![mk("b", 2, 0), mk("a", 1, 0), mk("c", 0, 1), mk("d", 1, 0)];
        let order: Vec<String> = ordered(&entries)
            .into_iter()
            .map(|e| e.program[0].clone())
            .collect();
        assert_eq!(order, vec!["a", "d", "b", "c"]);
    }

    #[test]
    fn shared_commands_start_once_and_keep_each_window() {
        let mk = |program: &str, tag: u32| Entry {
            program: vec![program.into()],
            tag,
            monitor: 0,
            floating: false,
            focused: false,
        };
        let entries = vec![mk("browser", 1), mk("browser", 3), mk("editor", 0)];
        let launches = launches(&entries);
        assert_eq!(launches.len(), 2);
        let browser = launches
            .iter()
            .find(|(command, _)| command == &["browser".to_string()])
            .expect("browser launch");
        assert_eq!(browser.1.len(), 2);
        let editor = launches
            .iter()
            .find(|(command, _)| command == &["editor".to_string()])
            .expect("editor launch");
        assert_eq!(editor.1.len(), 1);
    }
}
