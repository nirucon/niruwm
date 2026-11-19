//! Is a Bluetooth device (a phone) within reach?
//!
//! The check opens an L2CAP connection to the device's SDP channel (PSM 1)
//! and closes it again. The phone answers the page when it is in range with
//! Bluetooth on; no pairing prompt appears and no root rights are needed,
//! unlike `l2ping`. A refused connection still proves the phone answered.

use std::time::{Duration, Instant};

/// What one check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Presence {
    /// The device answered.
    Near,
    /// The device did not answer within the page timeout.
    Away,
    /// The check itself could not run (no adapter, adapter off, no kernel
    /// support). Never a reason to lock.
    Unknown(String),
}

/// How long one check may take. The kernel's own page timeout is about
/// five seconds; this only guards against a stuck socket.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(9);

#[repr(C)]
struct SockaddrL2 {
    l2_family: libc::sa_family_t,
    l2_psm: u16,
    l2_bdaddr: [u8; 6],
    l2_cid: u16,
    l2_bdaddr_type: u8,
}

const BTPROTO_L2CAP: libc::c_int = 0;
const SDP_PSM: u16 = 1;

/// `AA:BB:CC:DD:EE:FF` as the little-endian bytes the kernel expects.
pub fn parse_address(text: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        if part.len() != 2 {
            return None;
        }
        out[5 - i] = u8::from_str_radix(part, 16).ok()?;
    }
    Some(out)
}

/// Which answer an `errno` from the connection attempt stands for.
pub fn classify(errno: i32) -> Presence {
    match errno {
        0 | libc::ECONNREFUSED | libc::EACCES | libc::ECONNRESET | libc::EPERM => Presence::Near,
        // The page went out and nobody answered.
        libc::EHOSTDOWN | libc::ETIMEDOUT | libc::ECONNABORTED => Presence::Away,
        // No adapter, or Bluetooth switched off: the page never went out.
        libc::EHOSTUNREACH => Presence::Unknown("Bluetooth is off or missing".into()),
        other => Presence::Unknown(std::io::Error::from_raw_os_error(other).to_string()),
    }
}

/// Check once whether `address` answers.
pub fn probe(address: &str) -> Presence {
    let Some(bdaddr) = parse_address(address) else {
        return Presence::Unknown(format!("{address:?} is not a Bluetooth address"));
    };
    // SAFETY: plain socket calls on a descriptor owned by this function and
    // closed on every path; the address struct matches `struct sockaddr_l2`.
    unsafe {
        let fd = libc::socket(
            libc::AF_BLUETOOTH,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            BTPROTO_L2CAP,
        );
        if fd < 0 {
            return Presence::Unknown(std::io::Error::last_os_error().to_string());
        }
        let addr = SockaddrL2 {
            l2_family: libc::AF_BLUETOOTH as libc::sa_family_t,
            l2_psm: SDP_PSM.to_le(),
            l2_bdaddr: bdaddr,
            l2_cid: 0,
            l2_bdaddr_type: 0,
        };
        let rc = libc::connect(
            fd,
            &addr as *const SockaddrL2 as *const libc::sockaddr,
            std::mem::size_of::<SockaddrL2>() as libc::socklen_t,
        );
        let result = if rc == 0 {
            Presence::Near
        } else {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno == libc::EINPROGRESS {
                wait_connected(fd)
            } else {
                classify(errno)
            }
        };
        libc::close(fd);
        result
    }
}

/// Wait for a non-blocking connect to finish and read its outcome.
unsafe fn wait_connected(fd: libc::c_int) -> Presence {
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Presence::Away;
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let n = libc::poll(&mut pfd, 1, left.as_millis().min(i32::MAX as u128) as i32);
        if n < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if errno == libc::EINTR {
                continue;
            }
            return Presence::Unknown(std::io::Error::from_raw_os_error(errno).to_string());
        }
        if n == 0 {
            return Presence::Away;
        }
        let mut err: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        if libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            &mut err as *mut libc::c_int as *mut libc::c_void,
            &mut len,
        ) < 0
        {
            return Presence::Unknown(std::io::Error::last_os_error().to_string());
        }
        return classify(err);
    }
}

/// The proximity lock's memory between checks.
#[derive(Debug, Default)]
pub struct Tracker {
    /// The phone was seen while the session was unlocked. Only then may its
    /// absence lock: a login or an unlock without the phone never starts a
    /// lock loop.
    armed: bool,
    last_near: Option<Instant>,
    misses: u32,
    last: Option<Presence>,
}

/// What the idle daemon should do after a check.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// The phone has been away long enough: lock.
    Lock,
    /// The phone came back while the session is locked: light the screen so
    /// the password prompt is ready. It never unlocks.
    Wake,
}

impl Tracker {
    /// Record one check at `now` and decide.
    pub fn update(
        &mut self,
        presence: Presence,
        now: Instant,
        locked: bool,
        lock_after: Duration,
    ) -> Action {
        let came_back = presence == Presence::Near && self.last == Some(Presence::Away);
        let action = match &presence {
            Presence::Near => {
                self.last_near = Some(now);
                self.misses = 0;
                if !locked {
                    self.armed = true;
                }
                if locked && came_back {
                    Action::Wake
                } else {
                    Action::Nothing
                }
            }
            Presence::Away => {
                self.misses = self.misses.saturating_add(1);
                let away_long = self
                    .last_near
                    .map(|t| now.saturating_duration_since(t) >= lock_after)
                    .unwrap_or(false);
                if !locked && self.armed && away_long && self.misses >= 2 {
                    self.armed = false;
                    Action::Lock
                } else {
                    Action::Nothing
                }
            }
            Presence::Unknown(_) => Action::Nothing,
        };
        if locked {
            // Any lock, ours or not, needs a fresh sighting after the unlock.
            self.armed = false;
        }
        self.last = Some(presence);
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_secs(30);

    #[test]
    fn addresses_are_reversed_for_the_kernel() {
        assert_eq!(
            parse_address("AA:BB:CC:DD:EE:0f"),
            Some([0x0f, 0xee, 0xdd, 0xcc, 0xbb, 0xaa])
        );
        assert_eq!(parse_address("AA:BB:CC:DD:EE"), None);
        assert_eq!(parse_address("AA:BB:CC:DD:EE:GG"), None);
        assert_eq!(parse_address("AAA:BB:CC:DD:EE:FF"), None);
    }

    #[test]
    fn a_refusal_is_an_answer_and_no_adapter_is_unknown() {
        assert_eq!(classify(0), Presence::Near);
        assert_eq!(classify(libc::ECONNREFUSED), Presence::Near);
        assert_eq!(classify(libc::EHOSTDOWN), Presence::Away);
        assert_eq!(classify(libc::ETIMEDOUT), Presence::Away);
        assert!(matches!(classify(libc::ENODEV), Presence::Unknown(_)));
        assert!(matches!(classify(libc::EHOSTUNREACH), Presence::Unknown(_)));
        assert!(matches!(
            classify(libc::EADDRNOTAVAIL),
            Presence::Unknown(_)
        ));
    }

    #[test]
    fn locks_only_after_the_phone_was_seen_and_stayed_away() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        // Never seen: absence alone never locks.
        assert_eq!(t.update(Presence::Away, t0, false, LIMIT), Action::Nothing);
        assert_eq!(
            t.update(Presence::Away, t0 + Duration::from_secs(60), false, LIMIT),
            Action::Nothing
        );
        // Seen, then away: lock once the limit has passed.
        let s = t0 + Duration::from_secs(100);
        assert_eq!(t.update(Presence::Near, s, false, LIMIT), Action::Nothing);
        assert_eq!(
            t.update(Presence::Away, s + Duration::from_secs(10), false, LIMIT),
            Action::Nothing
        );
        assert_eq!(
            t.update(Presence::Away, s + Duration::from_secs(20), false, LIMIT),
            Action::Nothing
        );
        assert_eq!(
            t.update(Presence::Away, s + Duration::from_secs(31), false, LIMIT),
            Action::Lock
        );
    }

    #[test]
    fn one_missed_answer_is_not_enough() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.update(Presence::Near, t0, false, LIMIT);
        // A single miss long after the last sighting (a stalled check).
        assert_eq!(
            t.update(Presence::Away, t0 + Duration::from_secs(40), false, LIMIT),
            Action::Nothing
        );
        assert_eq!(
            t.update(Presence::Away, t0 + Duration::from_secs(50), false, LIMIT),
            Action::Lock
        );
    }

    #[test]
    fn an_unlock_without_the_phone_does_not_lock_again() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.update(Presence::Near, t0, false, LIMIT);
        t.update(Presence::Away, t0 + Duration::from_secs(20), false, LIMIT);
        assert_eq!(
            t.update(Presence::Away, t0 + Duration::from_secs(40), false, LIMIT),
            Action::Lock
        );
        // Locked, still away; then the owner unlocks with the phone elsewhere.
        t.update(Presence::Away, t0 + Duration::from_secs(50), true, LIMIT);
        for s in [60, 120, 600] {
            assert_eq!(
                t.update(Presence::Away, t0 + Duration::from_secs(s), false, LIMIT),
                Action::Nothing
            );
        }
    }

    #[test]
    fn the_phone_coming_back_wakes_but_an_unknown_does_nothing() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.update(Presence::Near, t0, false, LIMIT);
        t.update(Presence::Away, t0 + Duration::from_secs(10), true, LIMIT);
        assert_eq!(
            t.update(Presence::Near, t0 + Duration::from_secs(20), true, LIMIT),
            Action::Wake
        );
        // Still near: no repeated wake.
        assert_eq!(
            t.update(Presence::Near, t0 + Duration::from_secs(30), true, LIMIT),
            Action::Nothing
        );
        let mut u = Tracker::default();
        u.update(Presence::Near, t0, false, LIMIT);
        for s in [40, 80, 120] {
            assert_eq!(
                u.update(
                    Presence::Unknown("adapter off".into()),
                    t0 + Duration::from_secs(s),
                    false,
                    LIMIT
                ),
                Action::Nothing
            );
        }
    }
}
