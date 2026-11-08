//! The keybinding cheat sheet's source of truth.
//!
//! One list, built from the live NIRUWM binding table plus the keys other
//! programs grab themselves and register in `~/.local/share/niru/keys.d`.
//! `niructl keys --json` prints it for the launcher's cheat sheet, and
//! `niructl keys --markdown` prints the tables of `docs/KEYBINDINGS.md`, so
//! the sheet and the documentation can never disagree.

use crate::bindings::{self, RawBinding};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The schema version of `niructl keys --json`.
pub const SCHEMA: u32 = 1;

/// One key in the sheet.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Entry {
    /// The key as written in the config (`super+shift+Return`).
    pub keys: String,
    /// The key as people read it (`Super+Shift+Enter`).
    pub label: String,
    /// X modifier mask and keysym, for matching a pressed key. 0 when the
    /// key name has no known keysym.
    pub mods: u32,
    pub keysym: u32,
    /// NIRUWM action (`layout tile`); `None` for a key another program owns.
    pub action: Option<String>,
    /// For a registered app key: the argv the cheat sheet runs on Enter.
    pub run: Option<Vec<String>>,
    pub description: String,
    pub group: String,
    /// `niruwm` (a default), `config` (from config.toml) or `app`.
    pub source: String,
    /// The program that registered the key (`keys.d/<app>.toml`).
    pub app: Option<String>,
    /// Other entries on the same key, as `who: description`.
    pub conflicts: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Sheet {
    pub version: u32,
    pub entries: Vec<Entry>,
    /// keys.d files that could not be read, in words.
    pub problems: Vec<String>,
}

/// `keys.d/<app>.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppFile {
    /// Display name; the file name when missing.
    app: Option<String>,
    /// Group for every key of the file; `Apps` when missing.
    group: Option<String>,
    #[serde(default)]
    keys: Vec<AppKey>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppKey {
    keys: String,
    description: String,
    #[serde(default)]
    run: Option<Vec<String>>,
    #[serde(default)]
    group: Option<String>,
}

/// `$XDG_DATA_HOME/niru/keys.d`, or `~/.local/share/niru/keys.d`.
pub fn keys_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        })
        .join("niru/keys.d")
}

fn wm_entry(raw: &RawBinding, defaults: &[RawBinding]) -> Option<Entry> {
    if raw.action.trim() == "unbind" {
        return None;
    }
    let (mods, keysym) = match bindings::parse_keys(&raw.keys) {
        Ok((mods, key)) => (mods, bindings::keysym_named(&key).unwrap_or(0)),
        Err(_) => (0, 0),
    };
    let (group, text) = bindings::describe_action(&raw.action);
    let is_default = defaults
        .iter()
        .any(|d| d.keys == raw.keys && d.action == raw.action);
    Some(Entry {
        keys: raw.keys.clone(),
        label: bindings::key_label(&raw.keys),
        mods,
        keysym,
        action: Some(raw.action.clone()),
        run: None,
        description: raw
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string)
            .unwrap_or(text),
        group: group.to_string(),
        source: if is_default && raw.description.is_none() {
            "niruwm".into()
        } else {
            "config".into()
        },
        app: None,
        conflicts: Vec::new(),
    })
}

/// Read every `*.toml` in `dir`, sorted by name. Unreadable files become
/// problems; they never hide the rest of the sheet.
fn app_entries(dir: &Path, problems: &mut Vec<String>) -> Vec<Entry> {
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(read) => read
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect(),
        Err(_) => return Vec::new(),
    };
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) if text.len() <= 256 * 1024 => text,
            Ok(_) => {
                problems.push(format!("{}: larger than 256 KiB", path.display()));
                continue;
            }
            Err(e) => {
                problems.push(format!("{}: {e}", path.display()));
                continue;
            }
        };
        let file: AppFile = match toml::from_str(&text) {
            Ok(file) => file,
            Err(e) => {
                problems.push(format!(
                    "{}: {}",
                    path.display(),
                    e.to_string().lines().next().unwrap_or("invalid TOML")
                ));
                continue;
            }
        };
        let app = file
            .app
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .unwrap_or(&name)
            .to_string();
        for key in file.keys {
            let (mods, keysym) = match bindings::parse_keys(&key.keys) {
                Ok((mods, k)) => (mods, bindings::keysym_named(&k).unwrap_or(0)),
                Err(e) => {
                    problems.push(format!("{}: {:?}: {e}", path.display(), key.keys));
                    continue;
                }
            };
            let run = key
                .run
                .filter(|argv| !argv.is_empty() && !argv[0].is_empty());
            out.push(Entry {
                keys: key.keys.clone(),
                label: bindings::key_label(&key.keys),
                mods,
                keysym,
                action: None,
                run,
                description: key.description.trim().to_string(),
                group: key
                    .group
                    .or_else(|| file.group.clone())
                    .unwrap_or_else(|| "Apps".into()),
                source: "app".into(),
                app: Some(app.clone()),
                conflicts: Vec::new(),
            });
        }
    }
    out
}

fn who(e: &Entry) -> String {
    match &e.app {
        Some(app) => app.clone(),
        None if e.source == "config" => "NIRUWM (config.toml)".into(),
        None => "NIRUWM".into(),
    }
}

/// The sheet: NIRUWM's effective table, then registered app keys, with
/// every key that two entries claim marked on both.
pub fn build(effective: &[RawBinding], defaults: &[RawBinding], dir: Option<&Path>) -> Sheet {
    let mut problems = Vec::new();
    let mut entries: Vec<Entry> = effective
        .iter()
        .filter_map(|raw| wm_entry(raw, defaults))
        .collect();
    if let Some(dir) = dir {
        entries.extend(app_entries(dir, &mut problems));
    }
    let same = |a: &Entry, b: &Entry| {
        if a.keysym != 0 && b.keysym != 0 {
            a.mods == b.mods && a.keysym == b.keysym
        } else {
            a.keys.eq_ignore_ascii_case(&b.keys)
        }
    };
    let snapshot = entries.clone();
    for (i, entry) in entries.iter_mut().enumerate() {
        entry.conflicts = snapshot
            .iter()
            .enumerate()
            .filter(|(j, other)| *j != i && same(entry, other))
            .map(|(_, other)| format!("{}: {}", who(other), other.description))
            .collect();
    }
    Sheet {
        version: SCHEMA,
        entries,
        problems,
    }
}

/// Group order for the sheet and the documentation; app groups follow.
pub fn group_rank(group: &str) -> usize {
    bindings::GROUPS
        .iter()
        .position(|g| *g == group)
        .unwrap_or(bindings::GROUPS.len())
}

/// A numbered family member: `super+shift+3` with `movetotag 3` is
/// ("super+shift+", 3) and its description with the number as `N`.
fn family(entry: &Entry) -> Option<(String, u32, String)> {
    let action = entry.action.as_deref()?;
    let (verb, arg) = action.split_once(' ')?;
    let n: u32 = arg.trim().parse().ok()?;
    if !matches!(
        verb,
        "tag" | "movetotag" | "toggletag" | "togglemovetotag" | "scratchpad" | "scratchpad_send"
    ) {
        return None;
    }
    let last = entry.keys.chars().last()?;
    let digit = last.to_digit(10)?;
    if digit != n % 10 {
        return None;
    }
    let prefix = entry.keys[..entry.keys.len() - 1].to_string();
    let template = entry.description.replacen(&n.to_string(), "N", 1);
    Some((prefix, n, template))
}

fn code(label: &str) -> String {
    if label.contains('`') {
        format!("`` {label} ``")
    } else {
        format!("`{label}`")
    }
}

/// The Markdown tables of `docs/KEYBINDINGS.md`: one table per group, keys
/// with the same description on one row, numbered families collapsed.
pub fn markdown(entries: &[Entry]) -> String {
    let mut groups: Vec<&str> = Vec::new();
    for e in entries {
        if !groups.contains(&e.group.as_str()) {
            groups.push(&e.group);
        }
    }
    groups.sort_by_key(|g| group_rank(g));
    let mut out = String::new();
    for group in groups {
        out.push_str(&format!("### {group}\n\n| Keys | Action |\n|---|---|\n"));
        // (keys cell, description) in table order.
        let mut rows: Vec<(Vec<String>, String)> = Vec::new();
        let mut families: Vec<(String, String, Vec<u32>)> = Vec::new();
        let mut row_of_family: Vec<(usize, usize)> = Vec::new();
        for e in entries.iter().filter(|e| e.group == group) {
            if let Some((prefix, n, template)) = family(e) {
                if let Some(fi) = families
                    .iter()
                    .position(|(p, t, _)| *p == prefix && *t == template)
                {
                    families[fi].2.push(n);
                    continue;
                }
                families.push((prefix, template.clone(), vec![n]));
                rows.push((Vec::new(), template));
                row_of_family.push((rows.len() - 1, families.len() - 1));
                continue;
            }
            let mut label = e.label.clone();
            if !e.conflicts.is_empty() {
                label.push_str(" (conflict)");
            }
            if let Some(row) = rows
                .iter_mut()
                .find(|(keys, d)| !keys.is_empty() && *d == e.description)
            {
                row.0.push(label);
            } else {
                rows.push((vec![label], e.description.clone()));
            }
        }
        for (row, fi) in row_of_family {
            let (prefix, template, numbers) = &families[fi];
            let label = |n: u32| bindings::key_label(&format!("{prefix}{}", n % 10));
            let ones: Vec<u32> = numbers.iter().copied().filter(|n| *n < 10).collect();
            let mut keys = Vec::new();
            let mut range = String::new();
            if ones.len() > 2 {
                let (lo, hi) = (ones[0], ones[ones.len() - 1]);
                let first = label(lo);
                keys.push(format!("{}…{hi}", first));
                range = format!("{lo}…{hi}");
            } else {
                for n in &ones {
                    keys.push(label(*n));
                }
            }
            if numbers.contains(&10) {
                keys.push(label(10));
                range = if range.is_empty() {
                    "10".into()
                } else {
                    format!("{range} / 10")
                };
            }
            let text = if range.is_empty() {
                template.replace('N', &ones.first().copied().unwrap_or(0).to_string())
            } else {
                template.replace('N', &range)
            };
            rows[row] = (keys, text);
        }
        for (keys, text) in rows {
            let cell: Vec<String> = keys.iter().map(|k| code(k)).collect();
            out.push_str(&format!("| {} | {} |\n", cell.join(" / "), text));
        }
        out.push('\n');
    }
    out
}

pub const BEGIN: &str = "<!-- BEGIN GENERATED: niructl keys --markdown --defaults -->";
pub const END: &str = "<!-- END GENERATED -->";

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(keys: &str, action: &str) -> RawBinding {
        RawBinding {
            keys: keys.into(),
            action: action.into(),
            description: None,
        }
    }

    #[test]
    fn every_default_is_in_the_sheet_with_words() {
        let d = bindings::Action::defaults();
        let sheet = build(&d, &d, None);
        assert_eq!(sheet.entries.len(), d.len());
        for e in &sheet.entries {
            assert_eq!(e.source, "niruwm");
            assert!(e.keysym != 0, "{} has no keysym", e.keys);
            assert!(e.conflicts.is_empty(), "{}: {:?}", e.keys, e.conflicts);
        }
    }

    #[test]
    fn config_entries_are_marked_and_keep_their_description() {
        let d = bindings::Action::defaults();
        let mut user = raw("super+x", "focus_next");
        user.description = Some("My own words".into());
        let sheet = build(&[raw("super+t", "layout tile"), user], &d, None);
        assert_eq!(sheet.entries[0].source, "niruwm");
        assert_eq!(sheet.entries[1].source, "config");
        assert_eq!(sheet.entries[1].description, "My own words");
        assert_eq!(sheet.entries[1].label, "Super+X");
    }

    #[test]
    fn app_keys_are_read_and_conflicts_flagged_on_both_sides() {
        let dir = std::env::temp_dir().join(format!("niru-keysd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("nirudictate.toml"),
            "app = \"NIRUDICTATE\"\ngroup = \"Dictation\"\n\n[[keys]]\nkeys = \"ctrl+alt+space\"\ndescription = \"Dictate\"\nrun = [\"nirudictate\", \"toggle\"]\n\n[[keys]]\nkeys = \"super+t\"\ndescription = \"Clash\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("broken.toml"), "keys = 3").unwrap();
        std::fs::write(dir.join("ignored.txt"), "not toml").unwrap();
        let d = bindings::Action::defaults();
        let sheet = build(&[raw("super+t", "layout tile")], &d, Some(&dir));
        assert_eq!(sheet.entries.len(), 3);
        let dictate = &sheet.entries[1];
        assert_eq!(dictate.app.as_deref(), Some("NIRUDICTATE"));
        assert_eq!(dictate.group, "Dictation");
        assert_eq!(dictate.label, "Ctrl+Alt+Space");
        assert_eq!(
            dictate.run.as_deref(),
            Some(&["nirudictate".to_string(), "toggle".to_string()][..])
        );
        assert!(dictate.conflicts.is_empty());
        assert_eq!(sheet.entries[0].conflicts, vec!["NIRUDICTATE: Clash"]);
        assert_eq!(sheet.entries[2].conflicts, vec!["NIRUWM: Layout: Tile"]);
        assert_eq!(sheet.problems.len(), 1, "{:?}", sheet.problems);
        assert!(sheet.problems[0].contains("broken.toml"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn markdown_collapses_families_and_shared_descriptions() {
        let d = bindings::Action::defaults();
        let md = markdown(&build(&d, &d, None).entries);
        assert!(
            md.contains("| `Super+1…9` / `Super+0` | Go to workspace 1…9 / 10 |"),
            "{md}"
        );
        assert!(
            md.contains("| `Super+Alt+1…9` | Show or hide scratchpad 1…9 |"),
            "{md}"
        );
        assert!(md.contains("`Super+J` / `Super+Right` / `Super+Down` | Focus the next window"));
        assert!(md.contains("| `Super+/` | Keybinding cheat sheet (this list) |"));
        assert!(md.contains("| `Super+.` | Emoji and symbol picker |"));
        assert!(md.contains("| `Super+Ctrl+C` | Pick a color from the screen and copy it |"));
        assert!(md.find("### Apps and launchers").unwrap() < md.find("### Layouts").unwrap());
    }

    #[test]
    fn the_keybindings_document_matches_the_table() {
        let d = bindings::Action::defaults();
        let md = markdown(&build(&d, &d, None).entries);
        let doc = include_str!("../docs/KEYBINDINGS.md");
        let start = doc.find(BEGIN).expect("BEGIN marker") + BEGIN.len();
        let end = doc.find(END).expect("END marker");
        assert_eq!(
            doc[start..end].trim(),
            md.trim(),
            "docs/KEYBINDINGS.md is stale: run `niructl keys --update-doc docs/KEYBINDINGS.md`"
        );
    }
}
