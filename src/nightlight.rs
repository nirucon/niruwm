//! The gradual night-light curve (NIRUWM 0.30), kept free of I/O so it can be
//! tested with any date, location and clock.
//!
//! Times are minutes after local midnight of the evening's own day, so a
//! morning time on the next day is above 1440. Clock times in the settings
//! that are before noon belong to the night after midnight: `deepest_at =
//! "00:30"` is half an hour after midnight, not the morning before.

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, Offset, TimeZone, Timelike};

/// Minutes in a day.
const DAY: f64 = 1440.0;
/// The night temperature is reached no sooner than this after the start.
const MIN_GLIDE: f64 = 60.0;
/// How long the morning glide back to the day temperature takes.
const MORNING: f64 = 30.0;

/// What `[nightlight]` asks of the gradual mode.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradual {
    pub start_temperature: u32,
    pub night_temperature: u32,
    pub day_temperature: u32,
    /// Minutes after midnight.
    pub not_before: u32,
    pub deepest_at: u32,
    pub off: u32,
}

/// One night, in minutes after midnight of the evening's day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Evening {
    /// The day the evening belongs to.
    pub date: NaiveDate,
    /// Local sunset that day, if the sun sets.
    pub sunset: Option<f64>,
    /// When the night light starts, at the start temperature.
    pub start: f64,
    /// When the night temperature is reached.
    pub deepest: f64,
    /// When the morning glide begins and ends (back at the day temperature).
    pub morning: f64,
    pub end: f64,
}

/// Where `now` is in the gradual schedule.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    /// The night `now` is in, or the next one during the day.
    pub evening: Evening,
    /// Kelvin the schedule wants; `None` during the day.
    pub kelvin: Option<u32>,
}

impl Position {
    pub fn night(&self) -> bool {
        self.kelvin.is_some()
    }
}

/// `HH:MM` to minutes after midnight.
pub fn clock_minutes(value: &str) -> Option<u32> {
    let (hour, minute) = value.trim().split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

/// Minutes (possibly above a day) as `HH:MM`.
pub fn clock(minutes: f64) -> String {
    let total = (minutes.round() as i64).rem_euclid(1440);
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// An evening setting as minutes after the evening's midnight: before noon
/// means after midnight.
fn evening_clock(minutes: u32) -> f64 {
    let m = f64::from(minutes);
    if m < 720.0 {
        m + DAY
    } else {
        m
    }
}

/// The night that starts on `date`. `sunset` is that day's local sunset and
/// `sunrise` the next day's local sunrise, in minutes after their own
/// midnight; `None` (polar day or night) falls back to `not_before` / `off`.
pub fn evening(g: &Gradual, date: NaiveDate, sunset: Option<f64>, sunrise: Option<f64>) -> Evening {
    let floor = evening_clock(g.not_before);
    let start = match sunset {
        // A sunset after noon is that evening; anything else is not a usable
        // evening sunset (polar edge cases), so use the floor alone.
        Some(set) if (720.0..DAY).contains(&set) => set.max(floor),
        _ => floor,
    };
    let deepest = evening_clock(g.deepest_at).max(start + MIN_GLIDE);
    let off = f64::from(g.off) + DAY;
    let end = match sunrise {
        Some(rise) if (0.0..DAY).contains(&rise) => (rise + DAY).min(off),
        _ => off,
    };
    // A night shorter than the morning glide still ends on time.
    let end = end.max(start);
    let morning = (end - MORNING).max(start);
    Evening {
        date,
        sunset,
        start,
        deepest,
        morning,
        end,
    }
}

fn lerp(from: u32, to: u32, fraction: f64) -> f64 {
    let fraction = fraction.clamp(0.0, 1.0);
    f64::from(from) + (f64::from(to) - f64::from(from)) * fraction
}

/// Kelvin at `t` (minutes after the evening's midnight); `None` outside the
/// night. The evening glide (falling) and the morning glide (rising) are each
/// continuous and the higher of the two is shown, so where a very short night
/// makes them overlap the screen still never jumps.
pub fn kelvin_at(g: &Gradual, e: &Evening, t: f64) -> Option<u32> {
    if t < e.start || t >= e.end {
        return None;
    }
    let evening = if e.deepest > e.start {
        lerp(
            g.start_temperature,
            g.night_temperature,
            (t - e.start) / (e.deepest - e.start),
        )
    } else {
        f64::from(g.night_temperature)
    };
    let morning = if t >= e.morning && e.end > e.morning {
        lerp(
            g.night_temperature,
            g.day_temperature,
            (t - e.morning) / (e.end - e.morning),
        )
    } else {
        f64::from(g.night_temperature)
    };
    Some(evening.max(morning).round() as u32)
}

/// Where `now` falls, given a function that returns (sunrise, sunset) for a
/// date in local minutes after midnight.
pub fn position(
    g: &Gradual,
    now: NaiveDateTime,
    sun: impl Fn(NaiveDate) -> (Option<f64>, Option<f64>),
) -> Position {
    let today = now.date();
    let t = f64::from(now.hour() * 60 + now.minute()) + f64::from(now.second()) / 60.0;
    let night_of = |date: NaiveDate| {
        let (_, set) = sun(date);
        let (rise, _) = sun(date + Duration::days(1));
        evening(g, date, set, rise)
    };
    // Last night still running in the morning?
    let yesterday = night_of(today - Duration::days(1));
    if let Some(kelvin) = kelvin_at(g, &yesterday, t + DAY) {
        return Position {
            evening: yesterday,
            kelvin: Some(kelvin),
        };
    }
    let tonight = night_of(today);
    Position {
        evening: tonight,
        kelvin: kelvin_at(g, &tonight, t),
    }
}

/// Local sunrise and sunset on `date`, in minutes after local midnight, by the
/// simplified NOAA algorithm (a couple of minutes off between the tropics and
/// the polar circles). `None` where the sun does not rise or set that day.
pub fn sun_times(latitude: f64, longitude: f64, date: NaiveDate) -> (Option<f64>, Option<f64>) {
    let offset = date
        .and_hms_opt(12, 0, 0)
        .and_then(|noon| Local.from_local_datetime(&noon).earliest())
        .map(|t| f64::from(t.offset().fix().local_minus_utc()) / 60.0)
        .unwrap_or(0.0);
    match solar_events(latitude, longitude, f64::from(date.ordinal()), offset) {
        Some((rise, set)) => (Some(rise), Some(set)),
        None => (None, None),
    }
}

/// Sunrise and sunset in local minutes for day-of-year `day` at a UTC offset
/// of `offset` minutes.
pub fn solar_events(latitude: f64, longitude: f64, day: f64, offset: f64) -> Option<(f64, f64)> {
    let gamma = 2.0 * std::f64::consts::PI / 365.0 * (day - 1.0);
    let eqtime = 229.18
        * (0.000075 + 0.001868 * gamma.cos()
            - 0.032077 * gamma.sin()
            - 0.014615 * (2.0 * gamma).cos()
            - 0.040849 * (2.0 * gamma).sin());
    let decl = 0.006918 - 0.399912 * gamma.cos() + 0.070257 * gamma.sin()
        - 0.006758 * (2.0 * gamma).cos()
        + 0.000907 * (2.0 * gamma).sin()
        - 0.002697 * (3.0 * gamma).cos()
        + 0.00148 * (3.0 * gamma).sin();
    let lat = latitude.to_radians();
    let zenith = 90.833_f64.to_radians();
    let cos_ha = (zenith.cos() / (lat.cos() * decl.cos())) - lat.tan() * decl.tan();
    if !(-1.0..=1.0).contains(&cos_ha) {
        return None;
    }
    let ha = cos_ha.acos().to_degrees();
    let noon = 720.0 - 4.0 * longitude - eqtime;
    Some((noon - 4.0 * ha + offset, noon + 4.0 * ha + offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Gradual {
        Gradual {
            start_temperature: 4500,
            night_temperature: 2700,
            day_temperature: 6500,
            not_before: 19 * 60,
            deepest_at: 22 * 60,
            off: 7 * 60,
        }
    }

    fn at(date: &str, time: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S").unwrap()
    }

    /// A fixed sun: the same rise and set every day.
    fn sun(rise: &str, set: &str) -> impl Fn(NaiveDate) -> (Option<f64>, Option<f64>) {
        let rise = f64::from(clock_minutes(rise).unwrap());
        let set = f64::from(clock_minutes(set).unwrap());
        move |_| (Some(rise), Some(set))
    }

    fn kelvin(
        g: &Gradual,
        when: NaiveDateTime,
        s: &impl Fn(NaiveDate) -> (Option<f64>, Option<f64>),
    ) -> Option<u32> {
        position(g, when, s).kelvin
    }

    #[test]
    fn winter_evening_starts_at_not_before_and_glides_to_the_night() {
        // Stockholm in December: sunset 14:50, sunrise 08:40.
        let g = settings();
        let s = sun("08:40", "14:50");
        assert_eq!(kelvin(&g, at("2026-12-10", "18:59:00"), &s), None);
        assert_eq!(kelvin(&g, at("2026-12-10", "19:00:00"), &s), Some(4500));
        // Halfway between 19:00 and 22:00.
        assert_eq!(kelvin(&g, at("2026-12-10", "20:30:00"), &s), Some(3600));
        assert_eq!(kelvin(&g, at("2026-12-10", "22:00:00"), &s), Some(2700));
        assert_eq!(kelvin(&g, at("2026-12-11", "03:00:00"), &s), Some(2700));
        // Sunrise is after 07:00, so the morning ends at off: 06:30-07:00.
        assert_eq!(kelvin(&g, at("2026-12-11", "06:30:00"), &s), Some(2700));
        assert_eq!(kelvin(&g, at("2026-12-11", "06:45:00"), &s), Some(4600));
        assert_eq!(kelvin(&g, at("2026-12-11", "07:00:00"), &s), None);
        let p = position(&g, at("2026-12-11", "02:00:00"), &s);
        assert_eq!(
            p.evening.date,
            NaiveDate::from_ymd_opt(2026, 12, 10).unwrap()
        );
        assert_eq!(clock(p.evening.start), "19:00");
        assert_eq!(clock(p.evening.deepest), "22:00");
        assert_eq!(clock(p.evening.end), "07:00");
    }

    #[test]
    fn steps_every_30_seconds_are_small() {
        let g = settings();
        let s = sun("08:40", "14:50");
        let mut t = at("2026-12-10", "19:00:00");
        let mut last = kelvin(&g, t, &s).unwrap();
        while t < at("2026-12-11", "07:00:00") {
            t += Duration::seconds(30);
            let Some(k) = kelvin(&g, t, &s) else { break };
            assert!(last.abs_diff(k) <= 70, "{last} -> {k} at {t}");
            last = k;
        }
        assert_eq!(t, at("2026-12-11", "07:00:00"));
    }

    #[test]
    fn late_sunset_starts_at_the_sun_and_clamps_deepest_to_one_hour() {
        // Stockholm in June: sunset 22:05, sunrise 03:35.
        let g = settings();
        let s = sun("03:35", "22:05");
        assert_eq!(kelvin(&g, at("2026-06-20", "22:00:00"), &s), None);
        assert_eq!(kelvin(&g, at("2026-06-20", "22:05:00"), &s), Some(4500));
        let p = position(&g, at("2026-06-20", "22:35:00"), &s);
        assert_eq!(clock(p.evening.deepest), "23:05");
        assert_eq!(p.kelvin, Some(3600));
        assert_eq!(kelvin(&g, at("2026-06-20", "23:05:00"), &s), Some(2700));
        // Sunrise before off: the morning ends at sunrise.
        assert_eq!(clock(p.evening.end), "03:35");
        assert_eq!(kelvin(&g, at("2026-06-21", "03:05:00"), &s), Some(2700));
        assert_eq!(kelvin(&g, at("2026-06-21", "03:35:00"), &s), None);
    }

    #[test]
    fn times_after_midnight_are_handled() {
        let mut g = settings();
        g.not_before = 23 * 60;
        g.deepest_at = 30; // 00:30
        let s = sun("08:40", "14:50");
        assert_eq!(kelvin(&g, at("2026-12-10", "23:00:00"), &s), Some(4500));
        // 23:00 to 00:30 is 90 minutes; 23:45 is the middle.
        assert_eq!(kelvin(&g, at("2026-12-10", "23:45:00"), &s), Some(3600));
        assert_eq!(kelvin(&g, at("2026-12-11", "00:30:00"), &s), Some(2700));
        // A sunset after midnight is not an evening; a deepest time that
        // would be before the start is clamped to one hour after it.
        g.not_before = 23 * 60 + 30;
        g.deepest_at = 23 * 60 + 45;
        let p = position(&g, at("2026-12-11", "00:00:00"), &s);
        assert_eq!(clock(p.evening.deepest), "00:30");
        assert_eq!(p.kelvin, Some(3600));
    }

    #[test]
    fn morning_ends_at_the_earlier_of_sunrise_and_off() {
        let g = settings();
        let e = evening(
            &g,
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
            Some(1050.0),
            Some(375.0),
        );
        assert_eq!(clock(e.end), "06:15");
        assert_eq!(clock(e.morning), "05:45");
        let e = evening(
            &g,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            Some(900.0),
            Some(510.0),
        );
        assert_eq!(clock(e.end), "07:00");
        // Polar night and day: no sun events, the settings decide.
        let e = evening(&g, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), None, None);
        assert_eq!(
            (clock(e.start), clock(e.end)),
            ("19:00".into(), "07:00".into())
        );
    }

    #[test]
    fn a_night_shorter_than_the_glides_never_jumps() {
        // Far north in early summer: sunset 23:50, sunrise 01:10.
        let g = settings();
        let s = sun("01:10", "23:50");
        let mut t = at("2026-06-01", "23:50:00");
        let mut last = kelvin(&g, t, &s).unwrap();
        assert_eq!(last, 4500);
        loop {
            t += Duration::seconds(30);
            let Some(k) = kelvin(&g, t, &s) else { break };
            assert!(last.abs_diff(k) <= 200, "{last} -> {k} at {t}");
            last = k;
        }
        assert_eq!(t, at("2026-06-02", "01:10:00"));
        assert!(last > 6000, "the morning glide ends near the day: {last}");
    }

    #[test]
    fn super_n_override_phase_changes_only_at_start_and_end() {
        // A Super+N choice holds while the phase it was made in lasts; the
        // phase is night from the start to the morning end, day otherwise.
        let g = settings();
        let s = sun("08:40", "14:50");
        let night = |when| position(&g, at("2026-12-10", when), &s).night();
        let morning = |when| position(&g, at("2026-12-11", when), &s).night();
        assert!(!night("18:59:59"));
        assert!(night("19:00:00"));
        assert!(night("23:59:59"));
        assert!(morning("00:00:00"));
        assert!(morning("06:59:30"));
        assert!(!morning("07:00:00"));
        assert!(!morning("18:00:00"));
    }

    #[test]
    fn noaa_sun_times_for_stockholm() {
        // 2026-12-10, CET (+60 min): sunrise about 08:40, sunset about 14:48.
        let (rise, set) = solar_events(59.33, 18.07, 344.0, 60.0).unwrap();
        assert!((rise - 520.0).abs() < 10.0, "sunrise {}", clock(rise));
        assert!((set - 888.0).abs() < 10.0, "sunset {}", clock(set));
        // Midsummer at Longyearbyen: no sunset.
        assert!(solar_events(78.22, 15.65, 172.0, 120.0).is_none());
    }
}
