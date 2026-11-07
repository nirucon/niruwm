//! A size cap for the session log.
//!
//! `niruwm-session` sends its own output, the window manager's and that of
//! every program started in the session to `~/.local/state/niruwm/session.log`.
//! Chatty programs (Discord, Electron apps) filled it to 81 MB on one laptop
//! because it was never rotated.
//!
//! The session script rotates the file when a session starts. During a
//! session every program holds the file open for appending, so it cannot be
//! renamed away; the window manager instead copies the newest part to
//! `session.log.1` and empties the file in place. Every writer opened it with
//! O_APPEND, so each simply continues at the new end. Lines written during
//! the copy itself can be lost; that is the price of not touching any other
//! program's file descriptors.
use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// Largest size the log may reach before it is rotated.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// How many old copies are kept: session.log.1 (newest) to session.log.3.
pub const KEEP: u32 = 3;

/// The session log `niruwm-session` writes.
pub fn session_log() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("niruwm/session.log")
}

/// Whether this process's stderr is `path` (so a test or a terminal run never
/// rotates a file it does not write).
pub fn stderr_is(path: &Path) -> bool {
    match (fs::read_link("/proc/self/fd/2"), fs::canonicalize(path)) {
        (Ok(fd), Ok(real)) => fd == real,
        _ => false,
    }
}

fn numbered(path: &Path, n: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Rotate `path` when it is larger than `max`: older copies move up one
/// number (the oldest falls off), the newest `max` bytes become `path.1`, and
/// `path` is emptied in place. Returns whether it rotated.
pub fn rotate_in_place(path: &Path, max: u64, keep: u32) -> io::Result<bool> {
    let len = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if len <= max || keep == 0 {
        return Ok(false);
    }
    for n in (1..keep).rev() {
        let from = numbered(path, n);
        if from.exists() {
            fs::rename(&from, numbered(path, n + 1))?;
        }
    }
    let mut src = fs::File::open(path)?;
    src.seek(SeekFrom::Start(len.saturating_sub(max)))?;
    let mut tail = Vec::with_capacity(max.min(len) as usize);
    src.take(max).read_to_end(&mut tail)?;
    let mut copy = fs::File::create(numbered(path, 1))?;
    copy.write_all(&tail)?;
    fs::OpenOptions::new().write(true).open(path)?.set_len(0)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_log_is_rotated_with_a_few_old_copies_and_appending_continues() {
        let dir = std::env::temp_dir().join(format!("niruwm-logcap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("session.log");
        // A writer like every program in the session: append mode.
        let mut writer = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap();
        writer.write_all(&[b'a'; 100]).unwrap();
        assert!(!rotate_in_place(&log, 128, 3).unwrap(), "small log stays");
        for round in 0..5u8 {
            writer.write_all(&[b'b' + round; 200]).unwrap();
            assert!(rotate_in_place(&log, 128, 3).unwrap());
            assert_eq!(fs::metadata(&log).unwrap().len(), 0);
            // The newest copy holds the newest bytes, capped at the limit.
            let copy = fs::read(numbered(&log, 1)).unwrap();
            assert_eq!(copy.len(), 128);
            assert!(copy.iter().all(|b| *b == b'b' + round));
        }
        assert!(numbered(&log, 3).exists());
        assert!(!numbered(&log, 4).exists(), "only three old copies");
        // The open writer goes on at the start of the emptied file.
        writer.write_all(b"after\n").unwrap();
        assert_eq!(fs::read(&log).unwrap(), b"after\n");
        assert!(!rotate_in_place(&dir.join("missing.log"), 1, 3).unwrap());
        fs::remove_dir_all(dir).unwrap();
    }
}
