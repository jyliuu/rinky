//! Frontlight warmth ("amber") schedule: the logic is here, host-tested; the Kindle
//! backend reads and writes powerd's own schedule properties and can apply the
//! schedule itself while the framework is stopped.
//!
//! powerd on 5.19.5 (`lipc-probe -v com.lab126.powerd`, emulator):
//! `amberSchedEnable` (0/1), `amberSchedMode` (`Manual`), `amberSchedStartTime`
//! (`20:0`), `amberSchedEndTime` (`8:0`), `schedAmberLevel` (10), `currentAmberLevel`
//! (0..24), `flIntensity` (0..`flMaxIntensity` = 24).

use std::time::Duration;

/// A time of day in minutes after midnight (0..1440).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DayTime(pub u16);

impl DayTime {
    /// `h:m`.
    pub const fn hm(h: u16, m: u16) -> Self {
        DayTime((h % 24) * 60 + m % 60)
    }

    /// Hours.
    pub fn hour(self) -> u16 {
        self.0 / 60
    }

    /// Minutes.
    pub fn minute(self) -> u16 {
        self.0 % 60
    }

    /// Parses powerd's `H:M` (`20:0`, `08:30`).
    pub fn parse(s: &str) -> Option<DayTime> {
        let (h, m) = s.trim().split_once(':')?;
        let (h, m): (u16, u16) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
        (h < 24 && m < 60).then(|| DayTime::hm(h, m))
    }

    /// powerd's format (`20:0`).
    pub fn to_powerd(self) -> String {
        format!("{}:{}", self.hour(), self.minute())
    }

    /// Minutes from `self` forward to `later`, wrapping past midnight.
    pub fn minutes_until(self, later: DayTime) -> u16 {
        (later.0 + 1440 - self.0) % 1440
    }
}

impl std::fmt::Display for DayTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}", self.hour(), self.minute())
    }
}

/// powerd's schedule mode.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AmberMode {
    /// Custom start and end times (`Manual`).
    #[default]
    Manual,
    /// Any other mode string powerd reports (e.g. a sunset-based mode).
    Other(String),
}

impl AmberMode {
    /// From powerd's string.
    pub fn parse(s: &str) -> AmberMode {
        match s.trim() {
            "Manual" | "" => AmberMode::Manual,
            o => AmberMode::Other(o.to_string()),
        }
    }

    /// powerd's string.
    pub fn as_powerd(&self) -> &str {
        match self {
            AmberMode::Manual => "Manual",
            AmberMode::Other(s) => s,
        }
    }
}

/// A warmth schedule: `level` between `start` and `end` (wrapping past midnight), the
/// user's own warmth otherwise, with linear ramps of `ramp` at both ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmberSchedule {
    /// The schedule is active.
    pub enabled: bool,
    /// powerd's mode.
    pub mode: AmberMode,
    /// Start of the warm period.
    pub start: DayTime,
    /// End of the warm period.
    pub end: DayTime,
    /// Warmth during the period (0..=24).
    pub level: u32,
    /// Transition length at the start and at the end (0 = switch at once).
    pub ramp: Duration,
}

impl Default for AmberSchedule {
    fn default() -> Self {
        AmberSchedule {
            enabled: false,
            mode: AmberMode::Manual,
            start: DayTime::hm(20, 0),
            end: DayTime::hm(8, 0),
            level: 10,
            ramp: Duration::from_secs(30 * 60),
        }
    }
}

impl AmberSchedule {
    /// True when `t` lies in `[start, end)`, wrapping past midnight. A schedule whose
    /// start equals its end is never active.
    pub fn in_window(&self, t: DayTime) -> bool {
        let len = self.start.minutes_until(self.end);
        len > 0 && self.start.minutes_until(t) < len
    }

    /// Warmth at time `t` for a user whose own warmth is `base`.
    pub fn warmth_at(&self, t: DayTime, base: u32) -> u32 {
        if !self.enabled || !self.in_window(t) {
            return base;
        }
        let len = self.start.minutes_until(self.end) as u64;
        let ramp = (self.ramp.as_secs() / 60).min(len / 2);
        let since_start = self.start.minutes_until(t) as u64;
        let to_end = len - since_start; // >= 1 inside the window
        let blend = |num: u64| -> u32 {
            let (b, l) = (base as i64, self.level as i64);
            (b + (l - b) * num as i64 / ramp as i64) as u32
        };
        if ramp > 0 && since_start < ramp {
            blend(since_start)
        } else if ramp > 0 && to_end <= ramp {
            blend(to_end.saturating_sub(1))
        } else {
            self.level
        }
    }

    /// Minutes until the warmth may change next (for timers): 1 inside a ramp, else
    /// until the window starts or ends.
    pub fn next_change_in(&self, t: DayTime) -> Duration {
        if !self.enabled {
            return Duration::from_secs(3600);
        }
        let ramp = (self.ramp.as_secs() / 60) as u16;
        let since_start = self.start.minutes_until(t);
        let to_end = t.minutes_until(self.end);
        let m = if self.in_window(t) {
            if since_start < ramp || to_end <= ramp {
                1
            } else {
                to_end - ramp
            }
        } else {
            t.minutes_until(self.start)
        };
        Duration::from_secs(m.max(1) as u64 * 60)
    }
}

/// Applies an [`AmberSchedule`] over time: returns a new warmth only when it changes.
#[derive(Clone, Debug, Default)]
pub struct AmberScheduler {
    last: Option<u32>,
}

impl AmberScheduler {
    /// The warmth to set now, or `None` when it is unchanged since the last call.
    pub fn tick(&mut self, sched: &AmberSchedule, t: DayTime, base: u32) -> Option<u32> {
        let w = sched.warmth_at(t, base);
        if self.last == Some(w) {
            None
        } else {
            self.last = Some(w);
            Some(w)
        }
    }

    /// Forgets the last value (after the user set the warmth by hand).
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(start: (u16, u16), end: (u16, u16), level: u32, ramp_min: u64) -> AmberSchedule {
        AmberSchedule {
            enabled: true,
            mode: AmberMode::Manual,
            start: DayTime::hm(start.0, start.1),
            end: DayTime::hm(end.0, end.1),
            level,
            ramp: Duration::from_secs(ramp_min * 60),
        }
    }

    #[test]
    fn powerd_time_format() {
        assert_eq!(DayTime::parse("20:0"), Some(DayTime::hm(20, 0)));
        assert_eq!(DayTime::parse("8:05"), Some(DayTime::hm(8, 5)));
        assert_eq!(DayTime::parse("24:00"), None);
        assert_eq!(DayTime::parse("x"), None);
        assert_eq!(DayTime::hm(20, 0).to_powerd(), "20:0");
        assert_eq!(DayTime::hm(7, 3).to_string(), "07:03");
        assert_eq!(AmberMode::parse("Manual"), AmberMode::Manual);
        assert_eq!(AmberMode::parse("Sunset").as_powerd(), "Sunset");
    }

    #[test]
    fn window_wraps_past_midnight() {
        let s = sched((20, 0), (8, 0), 12, 0);
        assert!(s.in_window(DayTime::hm(20, 0)));
        assert!(s.in_window(DayTime::hm(23, 59)));
        assert!(s.in_window(DayTime::hm(0, 0)));
        assert!(s.in_window(DayTime::hm(7, 59)));
        assert!(!s.in_window(DayTime::hm(8, 0)));
        assert!(!s.in_window(DayTime::hm(12, 0)));
        assert_eq!(s.warmth_at(DayTime::hm(2, 0), 3), 12);
        assert_eq!(s.warmth_at(DayTime::hm(12, 0), 3), 3);
        let day = sched((9, 0), (17, 0), 5, 0);
        assert!(day.in_window(DayTime::hm(12, 0)) && !day.in_window(DayTime::hm(18, 0)));
        let empty = sched((9, 0), (9, 0), 5, 0);
        assert!(!empty.in_window(DayTime::hm(9, 0)));
        let off = AmberSchedule { enabled: false, ..s };
        assert_eq!(off.warmth_at(DayTime::hm(2, 0), 3), 3);
    }

    #[test]
    fn ramps_blend_linearly_at_both_ends() {
        let s = sched((20, 0), (8, 0), 20, 30);
        assert_eq!(s.warmth_at(DayTime::hm(20, 0), 0), 0);
        assert_eq!(s.warmth_at(DayTime::hm(20, 15), 0), 10);
        assert_eq!(s.warmth_at(DayTime::hm(20, 30), 0), 20);
        assert_eq!(s.warmth_at(DayTime::hm(3, 0), 0), 20);
        assert_eq!(s.warmth_at(DayTime::hm(7, 30), 0), 19);
        assert_eq!(s.warmth_at(DayTime::hm(7, 59), 0), 0);
        assert_eq!(s.warmth_at(DayTime::hm(8, 0), 0), 0);
        // A ramp longer than half the window is capped.
        // 10-minute window: the 60-minute ramp becomes 5 minutes (uncapped, 10:04 would be 0).
        let short = sched((10, 0), (10, 10), 10, 60);
        assert_eq!(short.warmth_at(DayTime::hm(10, 4), 0), 8);
        assert_eq!(short.warmth_at(DayTime::hm(10, 9), 0), 0);
        // Cooler than the user's own warmth also works (level below base).
        let cool = sched((20, 0), (8, 0), 0, 30);
        assert_eq!(cool.warmth_at(DayTime::hm(20, 15), 20), 10);
    }

    #[test]
    fn scheduler_reports_only_changes_and_next_change() {
        let s = sched((20, 0), (8, 0), 20, 0);
        let mut a = AmberScheduler::default();
        assert_eq!(a.tick(&s, DayTime::hm(19, 0), 4), Some(4));
        assert_eq!(a.tick(&s, DayTime::hm(19, 30), 4), None);
        assert_eq!(a.tick(&s, DayTime::hm(20, 0), 4), Some(20));
        a.reset();
        assert_eq!(a.tick(&s, DayTime::hm(21, 0), 4), Some(20));
        assert_eq!(s.next_change_in(DayTime::hm(19, 0)), Duration::from_secs(3600));
        assert_eq!(s.next_change_in(DayTime::hm(7, 0)), Duration::from_secs(3600));
        let r = sched((20, 0), (8, 0), 20, 30);
        assert_eq!(r.next_change_in(DayTime::hm(20, 10)), Duration::from_secs(60));
    }
}
