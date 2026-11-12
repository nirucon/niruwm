//! `niructl config`: what differs from the defaults, resetting one section
//! with a preview, and the line diff the history and reset commands print.
//!
//! The defaults are the shipped template. NIRUX installs the one it used for
//! the first `config.toml` as `~/.local/share/nirux/config.example.toml`; a
//! system without NIRUX uses the copy compiled into NIRUWM.

use std::path::PathBuf;
use toml::Value;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const EMBEDDED: &str = include_str!("../config.example.toml");

/// Keys NIRUX fills in for this machine when it writes the first file. A
/// reset keeps them and the diff does not count them as your changes.
const MACHINE_KEYS: &[&str] = &["wallpaper.dir", "wallpaper.theme_dir"];

fn nirux_template() -> PathBuf {
    let data = std::env::var_os("NIRUX_PREFIX")
        .map(|p| PathBuf::from(p).join("share"))
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(PathBuf::from))
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        });
    data.join("nirux").join("config.example.toml")
}

/// The template text and where it came from.
pub fn template() -> (String, String) {
    let path = nirux_template();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if text.parse::<Value>().is_ok() {
            return (text, path.display().to_string());
        }
    }
    (EMBEDDED.to_string(), "the defaults built into NIRUWM".into())
}

/// Top-level names a reset accepts: every section and key of the template
/// plus the ones the user has.
pub fn section_names(user: &str, template: &str) -> Vec<String> {
    let mut names = Vec::new();
    for text in [template, user] {
        if let Ok(doc) = text.parse::<toml_edit::DocumentMut>() {
            for (k, _) in doc.iter() {
                if !names.iter().any(|n| n == k) {
                    names.push(k.to_string());
                }
            }
        }
    }
    names
}

fn inline(v: &Value) -> String {
    match v {
        Value::String(s) => format!("{s:?}"),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(inline).collect::<Vec<_>>().join(", ")
        ),
        Value::Table(t) => format!(
            "{{ {} }}",
            t.iter()
                .map(|(k, v)| format!("{k} = {}", inline(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

fn is_table_array(v: &Value) -> bool {
    matches!(v, Value::Array(a) if !a.is_empty() && a.iter().all(|x| x.is_table()))
}

/// One line per difference from the template. `changed` lines are values you
/// set differently, `added` are keys and entries the template does not have,
/// `unset` are template keys missing from your file (the built-in default
/// applies to those, which may not be the template's value).
#[derive(Default, Debug)]
pub struct Differences {
    pub changed: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub unset: Vec<String>,
}

impl Differences {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.added.is_empty() && self.removed.is_empty()
    }
}

pub fn differences(user: &str, template: &str) -> Result<Differences> {
    let user: Value = user.parse()?;
    let template: Value = template.parse()?;
    let mut out = Differences::default();
    walk(&user, &template, "", &mut out);
    Ok(out)
}

fn walk(user: &Value, template: &Value, path: &str, out: &mut Differences) {
    let join = |k: &str| {
        if path.is_empty() {
            k.to_string()
        } else {
            format!("{path}.{k}")
        }
    };
    match (user, template) {
        (Value::Table(u), Value::Table(t)) => {
            for (k, uv) in u {
                let p = join(k);
                if MACHINE_KEYS.contains(&p.as_str()) {
                    continue;
                }
                match t.get(k) {
                    Some(tv) => walk(uv, tv, &p, out),
                    None if uv.is_table() => {
                        out.added.push(format!("[{p}] (a section the template does not have)"))
                    }
                    None if is_table_array(uv) => {
                        for e in uv.as_array().into_iter().flatten() {
                            out.added.push(format!("[[{p}]] {}", inline(e)));
                        }
                    }
                    None => out.added.push(format!("{p} = {}", inline(uv))),
                }
            }
            for (k, tv) in t {
                let p = join(k);
                if !u.contains_key(k) && !MACHINE_KEYS.contains(&p.as_str()) {
                    if tv.is_table() {
                        out.unset.push(format!("[{p}]"));
                    } else if is_table_array(tv) {
                        out.removed.push(format!("[[{p}]] all {} entries", tv.as_array().map_or(0, |a| a.len())));
                    } else {
                        out.unset.push(format!("{p} (template: {})", inline(tv)));
                    }
                }
            }
        }
        (Value::Array(u), Value::Array(t)) if is_table_array(user) || is_table_array(template) => {
            for e in u.iter().filter(|e| !t.contains(e)) {
                out.added.push(format!("[[{path}]] {}", inline(e)));
            }
            for e in t.iter().filter(|e| !u.contains(e)) {
                out.removed.push(format!("[[{path}]] {}", inline(e)));
            }
        }
        (u, t) if u != t => out
            .changed
            .push(format!("{path} = {}    (default {})", inline(u), inline(t))),
        _ => {}
    }
}

/// The names of the top-level sections and keys that differ between two
/// versions, for the history list.
pub fn changed_sections(older: &str, newer: &str) -> Vec<String> {
    let (Ok(a), Ok(b)) = (older.parse::<Value>(), newer.parse::<Value>()) else {
        return vec!["(not valid TOML)".into()];
    };
    let empty = toml::map::Map::new();
    let a = a.as_table().unwrap_or(&empty);
    let b = b.as_table().unwrap_or(&empty);
    let mut names: Vec<String> = Vec::new();
    for (k, v) in b {
        if a.get(k) != Some(v) {
            names.push(k.clone());
        }
    }
    for k in a.keys() {
        if !b.contains_key(k) {
            names.push(k.clone());
        }
    }
    names
}

fn get_path<'a>(doc: &'a toml_edit::Table, path: &[&str]) -> Option<&'a toml_edit::Item> {
    let (first, rest) = path.split_first()?;
    let item = doc.get(first)?;
    if rest.is_empty() {
        Some(item)
    } else {
        get_path(item.as_table()?, rest)
    }
}

/// `user` with `name` (a section like `bar`, a key like `theme`, or a
/// dotted key like `bar.modules`) replaced by the template's, comments
/// included, or removed when the template has none. `all` gives the
/// template itself. Machine keys (the picture folders) are kept.
pub fn reset_text(user: &str, template: &str, name: &str) -> Result<String> {
    let tdoc: toml_edit::DocumentMut = template.parse()?;
    // A file that no longer parses can still be reset as a whole.
    let mut doc: toml_edit::DocumentMut = match user.parse() {
        Ok(doc) => doc,
        Err(_) if name == "all" => tdoc.clone(),
        Err(e) => {
            return Err(format!(
                "config.toml does not parse ({}); fix it, `niructl config undo` or reset all",
                e.to_string().lines().next().unwrap_or("")
            )
            .into())
        }
    };
    let machine: Vec<(String, toml_edit::Item)> = MACHINE_KEYS
        .iter()
        .filter_map(|k| {
            let parts: Vec<&str> = k.split('.').collect();
            get_path(doc.as_table(), &parts).map(|i| (k.to_string(), i.clone()))
        })
        .collect();
    if name == "all" {
        doc = tdoc;
    } else {
        let parts: Vec<&str> = name.split('.').collect();
        let (last, parents) = parts.split_last().ok_or("name a section to reset")?;
        let mut table = doc.as_table_mut();
        for p in parents {
            table = table
                .get_mut(p)
                .and_then(|i| i.as_table_mut())
                .ok_or_else(|| format!("{name}: your file has no [{p}]"))?;
        }
        match get_path(tdoc.as_table(), &parts) {
            Some(item) => {
                let mut item = item.clone();
                // A section keeps its place in your file instead of moving to
                // where it sits in the template.
                if let (Some(new), Some(old)) = (
                    item.as_table_mut(),
                    table.get(last).and_then(|i| i.as_table()),
                ) {
                    if let Some(pos) = old.position() {
                        new.set_position(pos);
                    }
                }
                table.insert(last, item);
            }
            None => {
                if table.remove(last).is_none() {
                    return Err(format!("{name}: neither your file nor the template has it").into());
                }
            }
        }
    }
    for (k, item) in machine {
        let parts: Vec<&str> = k.split('.').collect();
        // Only a reset that replaced that section can have changed them.
        let touched = name == "all" || name == parts[0] || name == k;
        if !touched {
            continue;
        }
        if let Some(t) = doc.get_mut(parts[0]).and_then(|i| i.as_table_mut()) {
            // Assign the value, so the key keeps the template's comment.
            match (t.get_mut(parts[1]), item.as_value()) {
                (Some(slot), Some(value)) => {
                    if let Some(old) = slot.as_value_mut() {
                        let decor = old.decor().clone();
                        *old = value.clone();
                        *old.decor_mut() = decor;
                    }
                }
                (None, _) => {}
                (Some(slot), None) => *slot = item,
            }
        }
    }
    Ok(doc.to_string())
}

/// A unified line diff with three lines of context, or an empty string.
pub fn unified_diff(a: &str, b: &str, label_a: &str, label_b: &str) -> String {
    let a: Vec<&str> = a.lines().collect();
    let b: Vec<&str> = b.lines().collect();
    // Longest common subsequence table, suffix form.
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![0u32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[at(i, j)] = if a[i] == b[j] {
                lcs[at(i + 1, j + 1)] + 1
            } else {
                lcs[at(i + 1, j)].max(lcs[at(i, j + 1)])
            };
        }
    }
    // Edit script: (' ', a index, b index), ('-', ..), ('+', ..).
    let mut ops: Vec<(char, usize, usize)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', i, j));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[at(i + 1, j)] >= lcs[at(i, j + 1)]) {
            ops.push(('-', i, j));
            i += 1;
        } else {
            ops.push(('+', i, j));
            j += 1;
        }
    }
    if ops.iter().all(|o| o.0 == ' ') {
        return String::new();
    }
    let mut out = format!("--- {label_a}\n+++ {label_b}\n");
    let ctx = 3;
    let mut k = 0;
    while k < ops.len() {
        if ops[k].0 == ' ' {
            k += 1;
            continue;
        }
        let start = k.saturating_sub(ctx);
        let mut end = k;
        // Extend the hunk while changes are closer than two contexts apart.
        let mut last_change = k;
        while end < ops.len() {
            if ops[end].0 != ' ' {
                last_change = end;
            } else if end - last_change > 2 * ctx {
                break;
            }
            end += 1;
        }
        let end = (last_change + ctx + 1).min(ops.len());
        let a_start = ops[start].1;
        let b_start = ops[start].2;
        let a_len = ops[start..end].iter().filter(|o| o.0 != '+').count();
        let b_len = ops[start..end].iter().filter(|o| o.0 != '-').count();
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            a_start + 1,
            a_len,
            b_start + 1,
            b_len
        ));
        for &(op, ai, bj) in &ops[start..end] {
            let line = if op == '+' { b[bj] } else { a[ai] };
            out.push(op);
            out.push_str(line);
            out.push('\n');
        }
        k = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "theme = \"niru-noir\"\n\n# The bar.\n[bar]\n# Modules.\nmodules = [\"clock\"]\ntitle = false\n\n[wallpaper]\ndir = \"~/Pictures/Wallpapers\"\nmode = \"fixed\"\n";

    #[test]
    fn a_reset_section_gets_the_template_comments_and_leaves_the_rest() {
        let user = "theme = \"niru-nord\"\n[bar]\nmodules = [\"workspaces\"]\ntitle = true\n[wallpaper]\ndir = \"~/Bilder\"\nmode = \"single\"\n";
        let out = reset_text(user, T, "bar").unwrap();
        assert!(out.contains("# Modules.\nmodules = [\"clock\"]"), "{out}");
        assert!(out.contains("theme = \"niru-nord\""));
        assert!(out.contains("mode = \"single\""));
        let out = reset_text(user, T, "wallpaper").unwrap();
        assert!(out.contains("dir = \"~/Bilder\""), "the picture folder is kept: {out}");
        assert!(out.contains("mode = \"fixed\""));
        let commented = "[wallpaper]\n# The folder.\ndir = \"~/Bilder\"\n[bar]\ntitle = true\n";
        let out = reset_text(commented, T, "bar").unwrap();
        assert!(out.contains("# The folder.\ndir = \"~/Bilder\""), "{out}");
        let out = reset_text(user, T, "bar.modules").unwrap();
        assert!(out.contains("modules = [\"clock\"]") && out.contains("title = true"), "{out}");
    }

    #[test]
    fn a_key_the_template_lacks_is_removed() {
        let user = "[[keys]]\nkeys = \"super+x\"\naction = \"kill\"\n[bar]\ntitle = true\n";
        let out = reset_text(user, T, "keys").unwrap();
        assert!(!out.contains("super+x"), "{out}");
        assert!(reset_text(user, T, "nonsense").is_err());
    }

    #[test]
    fn differences_name_changed_and_added_values() {
        let user = "theme = \"niru-nord\"\n[bar]\nmodules = [\"clock\"]\n[wallpaper]\ndir = \"~/Bilder\"\nmode = \"fixed\"\n[[keys]]\nkeys = \"super+x\"\naction = \"kill\"\n";
        let d = differences(user, T).unwrap();
        assert_eq!(d.changed, vec!["theme = \"niru-nord\"    (default \"niru-noir\")"]);
        assert_eq!(d.added, vec!["[[keys]] { action = \"kill\", keys = \"super+x\" }"]);
        assert_eq!(d.unset, vec!["bar.title (template: false)"]);
        assert!(differences(T, T).unwrap().is_empty());
    }

    #[test]
    fn the_diff_shows_only_the_changed_region() {
        let a = (1..=20).map(|n| format!("l{n}\n")).collect::<String>();
        let b = a.replace("l10\n", "ten\n");
        let d = unified_diff(&a, &b, "a", "b");
        assert!(d.contains("@@ -7,7 +7,7 @@"), "{d}");
        assert!(d.contains("-l10\n+ten\n"));
        assert!(!d.contains("l2\n"));
        assert_eq!(unified_diff(&a, &a, "a", "b"), "");
    }

    #[test]
    fn the_shipped_template_resets_every_section_to_a_valid_file() {
        for name in section_names("", EMBEDDED) {
            let out = reset_text(EMBEDDED, EMBEDDED, &name).unwrap();
            assert!(out.parse::<Value>().is_ok(), "{name}");
        }
    }
}
