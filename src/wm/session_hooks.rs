//! NIRUX hooks the window manager runs on its own clock, without a thread:
//!
//! - `nirux whats-new --notify`, once, about 8 s after a fresh session start
//!   (not after a restart in place or a crash), when nirunotify is up;
//! - `nirux resume-check --notify`, 3 s after the machine woke from sleep,
//!   at most once a minute, also while the screen is locked.
//!
//! Sleep is noticed without logind: CLOCK_BOOTTIME counts suspended time and
//! CLOCK_MONOTONIC does not, so their difference only grows across a suspend.
//! One pair of clock reads per event-loop tick (once a second) is all it costs.
//! Without `nirux` in PATH nothing is started and nothing is said.

use std::time::{Duration, Instant};

/// Delay after a fresh session start before `nirux whats-new --notify`.
pub(crate) const WHATS_NEW_DELAY: Duration = Duration::from_secs(8);
/// A growth of BOOTTIME minus MONOTONIC above this means the machine slept.
pub(crate) const SLEEP_JUMP: Duration = Duration::from_secs(5);
/// Delay after the wake-up is noticed before `nirux resume-check --notify`.
pub(crate) const RESUME_DELAY: Duration = Duration::from_secs(3);
/// At most one resume check per this much awake time.
pub(crate) const RESUME_DEBOUNCE: Duration = Duration::from_secs(60);

/// True when the suspended time grew by more than [`SLEEP_JUMP`] between two
/// readings of BOOTTIME minus MONOTONIC.
pub(crate) fn slept(previous: Duration, current: Duration) -> bool {
    current.saturating_sub(previous) > SLEEP_JUMP
}

/// The state behind both hooks. Times are monotonic instants (awake time).
#[derive(Debug, Default)]
pub(crate) struct SessionHooks {
    /// When to run `nirux whats-new --notify`; `None` once done or never.
    whats_new_at: Option<Instant>,
    /// The last BOOTTIME minus MONOTONIC reading.
    last_offset: Option<Duration>,
    /// When to run the pending resume check.
    resume_at: Option<Instant>,
    /// When the last resume check was started.
    last_resume: Option<Instant>,
}

/// What a tick asks the window manager to start.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Hook {
    WhatsNew,
    ResumeCheck,
}

impl SessionHooks {
    /// `fresh` is a real session start, not a restart in place or a crash.
    pub(crate) fn new(fresh: bool, now: Instant) -> Self {
        Self {
            whats_new_at: fresh.then(|| now + WHATS_NEW_DELAY),
            ..Self::default()
        }
    }

    /// One tick: `offset` is BOOTTIME minus MONOTONIC now (`None` when the
    /// clocks cannot be read). Returns the hooks that are due.
    pub(crate) fn tick(&mut self, now: Instant, offset: Option<Duration>) -> Vec<Hook> {
        let mut due = Vec::new();
        if self.whats_new_at.is_some_and(|at| now >= at) {
            self.whats_new_at = None;
            due.push(Hook::WhatsNew);
        }
        if let Some(offset) = offset {
            if let Some(previous) = self.last_offset {
                if slept(previous, offset) && self.resume_at.is_none() {
                    self.resume_at = Some(now + RESUME_DELAY);
                }
            }
            self.last_offset = Some(offset);
        }
        if self.resume_at.is_some_and(|at| now >= at) {
            self.resume_at = None;
            let recent = self
                .last_resume
                .is_some_and(|last| now.duration_since(last) < RESUME_DEBOUNCE);
            if !recent {
                self.last_resume = Some(now);
                due.push(Hook::ResumeCheck);
            }
        }
        due
    }
}

/// CLOCK_BOOTTIME minus CLOCK_MONOTONIC: the time spent suspended since boot.
pub(crate) fn suspended_offset() -> Option<Duration> {
    fn read(clock: libc::clockid_t) -> Option<Duration> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a valid, writable timespec for the call.
        if unsafe { libc::clock_gettime(clock, &mut ts) } != 0 {
            return None;
        }
        Some(Duration::new(
            u64::try_from(ts.tv_sec).ok()?,
            u32::try_from(ts.tv_nsec).ok()?,
        ))
    }
    let boot = read(libc::CLOCK_BOOTTIME)?;
    let mono = read(libc::CLOCK_MONOTONIC)?;
    Some(boot.saturating_sub(mono))
}

/// `nirux` from PATH, if there is one.
pub(crate) fn find_nirux() -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join("nirux"))
        .find(|path| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(path)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

impl Hook {
    /// The `nirux` arguments for this hook.
    pub(crate) fn args(&self) -> &'static [&'static str] {
        match self {
            Hook::WhatsNew => &["whats-new", "--notify"],
            Hook::ResumeCheck => &["resume-check", "--notify"],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_jump_above_five_seconds_is_sleep() {
        assert!(!slept(s(100), s(100)));
        assert!(!slept(s(100), s(105)));
        assert!(slept(s(100), s(106)));
        assert!(slept(s(0), s(3600)));
        // The offset never shrinks; if it did, it is not sleep.
        assert!(!slept(s(100), s(10)));
    }

    #[test]
    fn whats_new_runs_once_after_a_fresh_start_only() {
        let t0 = Instant::now();
        let mut fresh = SessionHooks::new(true, t0);
        assert!(fresh.tick(t0 + s(7), Some(s(0))).is_empty());
        assert_eq!(fresh.tick(t0 + s(8), Some(s(0))), vec![Hook::WhatsNew]);
        assert!(fresh.tick(t0 + s(9), Some(s(0))).is_empty());
        assert!(fresh.tick(t0 + s(600), Some(s(0))).is_empty());

        let mut restarted = SessionHooks::new(false, t0);
        for n in 0..30 {
            assert!(restarted.tick(t0 + s(n), Some(s(0))).is_empty());
        }
    }

    #[test]
    fn resume_check_follows_a_jump_after_three_seconds() {
        let t0 = Instant::now();
        let mut h = SessionHooks::new(false, t0);
        assert!(h.tick(t0, Some(s(10))).is_empty());
        // Woke up: an hour was spent suspended.
        assert!(h.tick(t0 + s(1), Some(s(3610))).is_empty());
        assert!(h.tick(t0 + s(3), Some(s(3610))).is_empty());
        assert_eq!(h.tick(t0 + s(4), Some(s(3610))), vec![Hook::ResumeCheck]);
        assert!(h.tick(t0 + s(5), Some(s(3610))).is_empty());
    }

    #[test]
    fn resume_checks_are_at_most_one_a_minute() {
        let t0 = Instant::now();
        let mut h = SessionHooks::new(false, t0);
        h.tick(t0, Some(s(0)));
        h.tick(t0 + s(1), Some(s(100)));
        assert_eq!(h.tick(t0 + s(4), Some(s(100))), vec![Hook::ResumeCheck]);
        // A second sleep 20 s later: debounced.
        h.tick(t0 + s(20), Some(s(200)));
        assert!(h.tick(t0 + s(23), Some(s(200))).is_empty());
        assert!(h.tick(t0 + s(30), Some(s(200))).is_empty());
        // A third, well over a minute after the first check: runs again.
        h.tick(t0 + s(70), Some(s(300)));
        assert_eq!(h.tick(t0 + s(73), Some(s(300))), vec![Hook::ResumeCheck]);
    }

    #[test]
    fn small_drift_and_unreadable_clocks_do_nothing() {
        let t0 = Instant::now();
        let mut h = SessionHooks::new(false, t0);
        for n in 0..100 {
            // A few milliseconds of drift per tick never adds up to a jump.
            let drift = Duration::from_millis(n * 3);
            assert!(h.tick(t0 + s(n), Some(drift)).is_empty());
        }
        let mut blind = SessionHooks::new(false, t0);
        for n in 0..100 {
            assert!(blind.tick(t0 + s(n), None).is_empty());
        }
    }

    #[test]
    fn both_hooks_can_be_due_on_the_same_tick() {
        let t0 = Instant::now();
        let mut h = SessionHooks::new(true, t0);
        h.tick(t0, Some(s(0)));
        h.tick(t0 + s(5), Some(s(30)));
        assert_eq!(
            h.tick(t0 + s(8), Some(s(30))),
            vec![Hook::WhatsNew, Hook::ResumeCheck]
        );
    }

    #[test]
    fn the_offset_reads_on_linux() {
        assert!(suspended_offset().is_some());
    }
}
