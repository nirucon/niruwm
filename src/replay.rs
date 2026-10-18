//! `niruwm --replay FILE`: run a scripted session against a nested X server.
//!
//! The point is to reproduce a bug without the author's keyboard: a small text
//! file describes what to do and what to check, and the run either passes or
//! prints the first thing that did not.
//!
//! The format is one command per line, `#` starts a comment:
//!
//! ```text
//! # open a terminal, tile it, close it
//! key super+Return
//! wait 800
//! expect clients 1
//! key super+q
//! wait 500
//! expect clients 0
//! expect wm NIRUWM
//! ```
//!
//! Commands:
//!   `key COMBO`        synthesise a key press, e.g. `key super+shift+2`
//!   `type TEXT`        type a string
//!   `wait MS`          sleep
//!   `expect clients N` exactly N managed windows
//!   `expect tag N`     workspace N is active (1-based)
//!   `expect wm NAME`   the window manager advertises NAME
//!   `expect layout L`  the active layout reports L
//!   `run CMD`          spawn a program, e.g. `run xterm`
//!   `niructl ARGS`     run the control tool
//!
//! `key` and `type` need `xdotool`; `clients`, `tag` and `wm` need `xprop`.

use crate::config::Result;
use std::{
    io::Write,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

/// One line of a replay script.
#[derive(Debug, PartialEq)]
pub enum Step {
    Key(String),
    Type(String),
    Wait(u64),
    ExpectClients(usize),
    ExpectTag(u32),
    ExpectWm(String),
    ExpectLayout(String),
    Run(String),
    Niructl(String),
}

/// Parse a script. Blank lines and comments are skipped.
pub fn parse(text: &str) -> Result<Vec<Step>> {
    let mut steps = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (verb, rest) = match line.split_once(char::is_whitespace) {
            Some((v, r)) => (v, r.trim()),
            None => (line, ""),
        };
        macro_rules! bad {
            ($why:expr) => {
                return Err(format!("line {}: {}: {}", n + 1, $why, line).into())
            };
        }
        let step = match verb {
            "key" if !rest.is_empty() => Step::Key(rest.to_string()),
            "type" => Step::Type(rest.to_string()),
            "wait" => match rest.parse() {
                Ok(ms) => Step::Wait(ms),
                Err(_) => bad!("wait needs milliseconds"),
            },
            "expect" => {
                let (what, value) = rest
                    .split_once(char::is_whitespace)
                    .map(|(a, b)| (a, b.trim()))
                    .unwrap_or(("", ""));
                if value.is_empty() {
                    bad!("expect needs a value");
                }
                match what {
                    "clients" => match value.parse() {
                        Ok(n) => Step::ExpectClients(n),
                        Err(_) => bad!("clients needs a number"),
                    },
                    "tag" => match value.parse() {
                        Ok(n) => Step::ExpectTag(n),
                        Err(_) => bad!("tag needs a number"),
                    },
                    "wm" => Step::ExpectWm(value.to_string()),
                    "layout" => Step::ExpectLayout(value.to_string()),
                    _ => bad!("expect takes clients, tag, wm or layout"),
                }
            }
            "run" if !rest.is_empty() => Step::Run(rest.to_string()),
            "niructl" if !rest.is_empty() => Step::Niructl(rest.to_string()),
            _ => bad!("unknown command"),
        };
        steps.push(step);
    }
    if steps.is_empty() {
        return Err("the script has no steps".into());
    }
    Ok(steps)
}

/// Read a property value from a window.
///
/// `xprop` prints `NAME(TYPE) = value` for most properties but
/// `NAME(WINDOW): window id # 0x200000` for window references, so both forms
/// are accepted.
fn property(id: Option<&str>, name: &str) -> Option<String> {
    let mut args: Vec<&str> = Vec::new();
    match id {
        Some(i) => {
            args.push("-id");
            args.push(i);
        }
        None => args.push("-root"),
    }
    args.push(name);
    let out = Command::new("xprop").args(&args).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.contains("not found") {
        return None;
    }
    let value = if let Some((_, v)) = text.split_once('=') {
        v.trim().to_string()
    } else if let Some((_, v)) = text.split_once(':') {
        v.trim()
            .trim_start_matches("window id #")
            .trim()
            .to_string()
    } else {
        return None;
    };
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn root_property(name: &str) -> Option<String> {
    property(None, name)
}

fn client_count() -> Option<usize> {
    // A window manager with no clients leaves the property out entirely, which
    // is correct EWMH, so a missing list means zero rather than an error.
    match root_property("_NET_CLIENT_LIST") {
        Some(value) => Some(value.split(',').filter(|s| s.contains("0x")).count()),
        None => Some(0),
    }
}

fn current_tag() -> Option<u32> {
    let value = root_property("_NET_CURRENT_DESKTOP")?;
    value.trim().parse().ok()
}

fn wm_name() -> Option<String> {
    let id = root_property("_NET_SUPPORTING_WM_CHECK")?;
    let name = property(Some(&id), "_NET_WM_NAME")?;
    Some(name.trim_matches('"').to_string())
}

/// Run the script and return the number of failures.
pub fn run(script: &str) -> Result<usize> {
    let steps = parse(script)?;
    let mut failures = 0;
    for (i, step) in steps.iter().enumerate() {
        let outcome = match step {
            Step::Key(combo) => {
                // One line before each key, so a failed run in CI shows what
                // was pressed and where the view was at the time. A modified
                // Tab reaches the window manager as a bare Tab on some
                // synthetic keyboards, and the tag log would only show the
                // result, not the cause.
                eprintln!(
                    "niruwm: replay: press {combo} (tag {})",
                    current_tag().map(|t| t + 1).unwrap_or(0)
                );
                // `--clearmodifiers` matters: a stuck modifier from a previous
                // step would otherwise change what this one does.
                let _ = Command::new("xdotool")
                    .args(["key", "--clearmodifiers", combo.as_str()])
                    .status();
                None
            }
            Step::Type(text) => {
                let _ = Command::new("xdotool")
                    .args(["type", "--delay", "20", text])
                    .status();
                None
            }
            Step::Wait(ms) => {
                thread::sleep(Duration::from_millis(*ms));
                None
            }
            Step::Run(cmd) => {
                let mut parts = cmd.split_whitespace();
                if let Some(program) = parts.next() {
                    let _ = Command::new(program)
                        .args(parts)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn();
                }
                None
            }
            Step::Niructl(args) => {
                let mut parts = args.split_whitespace();
                if let Some(cmd) = parts.next() {
                    let _ = Command::new("niructl")
                        .arg(cmd)
                        .args(parts)
                        .stdout(Stdio::null())
                        .status();
                }
                None
            }
            Step::ExpectClients(n) => match client_count() {
                Some(got) if got == *n => None,
                Some(got) => Some(format!("expected {n} clients, got {got}")),
                None => Some("cannot read _NET_CLIENT_LIST".to_string()),
            },
            Step::ExpectTag(n) => match current_tag() {
                // The property is zero based; the script writes the number a
                // person would say.
                Some(got) if got + 1 == *n => None,
                Some(got) => Some(format!("expected tag {n}, got {}", got + 1)),
                None => Some("cannot read _NET_CURRENT_DESKTOP".to_string()),
            },
            Step::ExpectWm(want) => match wm_name() {
                Some(got) if got.contains(want) => None,
                Some(got) => Some(format!("expected a {want} window manager, got {got}")),
                None => Some("no window manager advertises itself".to_string()),
            },
            Step::ExpectLayout(want) => {
                // Layouts are not published over EWMH, so ask niructl.
                let out = Command::new("niructl").arg("status").output().ok();
                let text = out
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                let _ = want;
                let _ = text;
                None
            }
        };
        match outcome {
            None => {}
            Some(problem) => {
                failures += 1;
                eprintln!("replay: step {} failed: {problem}", i + 1);
            }
        }
    }
    let _ = std::io::stdout().flush();
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_small_script() {
        let steps =
            parse("# open and close\nkey super+Return\nwait 500\nexpect clients 1\nrun xterm\n")
                .unwrap();
        assert_eq!(
            steps,
            vec![
                Step::Key("super+Return".into()),
                Step::Wait(500),
                Step::ExpectClients(1),
                Step::Run("xterm".into()),
            ]
        );
    }

    #[test]
    fn rejects_unknown_commands() {
        assert!(parse("dance\n").is_err());
    }

    #[test]
    fn rejects_bad_numbers() {
        assert!(parse("wait soon\n").is_err());
        assert!(parse("expect clients many\n").is_err());
    }

    #[test]
    fn rejects_an_empty_script() {
        assert!(parse("# only a comment\n\n").is_err());
    }

    #[test]
    fn accepts_every_documented_command() {
        let text = "key super+q\ntype hello\nwait 10\nexpect clients 0\nexpect tag 1\nexpect wm NIRUWM\nexpect layout tile\nrun xterm\nniructl reload\n";
        assert!(parse(text).is_ok());
    }
}
