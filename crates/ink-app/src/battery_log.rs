//! Battery history for the battery page: what the battery used per hour awake and
//! asleep, since the last charge, and how long the charge left may last.
//!
//! One sample per line in `battery.csv` in the home folder
//! ([`ink_store::Paths::battery_log`]): `unix seconds,percent,charging,awake` (0/1). The
//! `awake` flag tells what the device did until the next sample: a sample is written
//! when the device goes to sleep (`0`) and when it wakes (`1`), and every
//! [`BatteryLog::EVERY`] while awake. Samples older than [`KEEP`] are dropped when the
//! file is rewritten.

use ink_device::BatteryState;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How far back samples are kept and rates are taken.
pub const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);
/// A gap longer than this between two samples is not counted (clock change, rinky not
/// running).
const MAX_GAP: u64 = 3 * 24 * 3600;
/// A rise of this many points between two samples is a charge (smaller ones are the
/// reading wobbling).
const CHARGED: u8 = 3;
/// The file is rewritten without old samples when it grows past this many lines.
const MAX_LINES: usize = 4000;

/// One battery sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    /// Wall-clock time, Unix seconds.
    pub t: u64,
    /// Charge in percent.
    pub percent: u8,
    /// Charging at that time.
    pub charging: bool,
    /// The device stayed awake until the next sample (false: it went to sleep).
    pub awake: bool,
}

impl Sample {
    fn line(&self) -> String {
        format!("{},{},{},{}\n", self.t, self.percent, self.charging as u8, self.awake as u8)
    }

    fn parse(line: &str) -> Option<Sample> {
        let mut f = line.trim().split(',');
        let t = f.next()?.parse().ok()?;
        let percent = f.next()?.parse::<u8>().ok().filter(|p| *p <= 100)?;
        let charging = f.next()? == "1";
        let awake = f.next()? == "1";
        Some(Sample { t, percent, charging, awake })
    }
}

/// Reads the samples in `path`, oldest first (none when it is missing; bad lines skipped).
pub fn load(path: &Path) -> Vec<Sample> {
    std::fs::read_to_string(path).map(|s| s.lines().filter_map(Sample::parse).collect()).unwrap_or_default()
}

/// Writes samples to the log: on sleep and wake, and at most every [`Self::EVERY`] while
/// awake.
#[derive(Debug)]
pub struct BatteryLog {
    path: PathBuf,
    last: Option<Instant>,
}

impl BatteryLog {
    /// Longest time between two samples while awake.
    pub const EVERY: Duration = Duration::from_secs(10 * 60);

    /// A log written to `path`.
    pub fn new(path: PathBuf) -> Self {
        BatteryLog { path, last: None }
    }

    /// True when a periodic sample is due.
    pub fn due(&self) -> bool {
        self.last.is_none_or(|t| t.elapsed() >= Self::EVERY)
    }

    /// Appends a sample of `state` at Unix time `now`; `awake` false when the device goes
    /// to sleep now.
    pub fn note(&mut self, now: u64, state: BatteryState, awake: bool) {
        self.last = Some(Instant::now());
        let s = Sample { t: now, percent: state.percent, charging: state.charging, awake };
        if let Err(e) = self.append(s, now) {
            log::warn!("battery log: {}: {e}", self.path.display());
        }
    }

    fn append(&self, s: Sample, now: u64) -> std::io::Result<()> {
        let lines = std::fs::read_to_string(&self.path).map(|t| t.lines().count()).unwrap_or(0);
        if lines >= MAX_LINES {
            let keep: String = load(&self.path).iter().filter(|x| now.saturating_sub(x.t) < KEEP.as_secs()).map(Sample::line).collect();
            let tmp = self.path.with_extension("csv.tmp");
            std::fs::write(&tmp, keep)?;
            std::fs::rename(&tmp, &self.path)?;
        }
        std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?.write_all(s.line().as_bytes())
    }
}

/// Percent used over time.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Use {
    /// Percent used.
    pub percent: f32,
    /// Over this many hours.
    pub hours: f32,
}

impl Use {
    fn add(&mut self, percent: f32, hours: f32) {
        self.percent += percent;
        self.hours += hours;
    }

    /// Percent per hour, once there is enough to tell (half an hour and 2 %).
    pub fn per_hour(&self) -> Option<f32> {
        (self.hours >= 0.5 && self.percent >= 2.0).then(|| self.percent / self.hours)
    }
}

/// What the battery page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// Use while awake (screen on, reading) in the last [`KEEP`].
    pub awake: Use,
    /// Use while asleep in the last [`KEEP`].
    pub asleep: Use,
    /// Use since the last charge (awake and asleep).
    pub since_charge: Use,
}

impl Stats {
    /// From `samples` (oldest first) at Unix time `now`.
    pub fn from_samples(samples: &[Sample], now: u64) -> Stats {
        let mut st = Stats::default();
        let recent: Vec<&Sample> = samples.iter().filter(|s| now.saturating_sub(s.t) < KEEP.as_secs()).collect();
        for w in recent.windows(2) {
            let (a, b) = (w[0], w[1]);
            let dt = b.t.saturating_sub(a.t);
            if a.charging || b.charging || dt == 0 || dt > MAX_GAP || b.percent >= a.percent + CHARGED {
                // A charge (or a gap) ends the run since the last charge.
                st.since_charge = Use::default();
                continue;
            }
            // The reading wobbles by a point or two: a small rise counts as nothing used.
            let (used, hours) = (a.percent.saturating_sub(b.percent) as f32, dt as f32 / 3600.0);
            let part = if a.awake { &mut st.awake } else { &mut st.asleep };
            part.add(used, hours);
            st.since_charge.add(used, hours);
        }
        st
    }

    /// Percent per hour for the time left: since the last charge when that tells
    /// enough, else the last week's mix of awake and asleep.
    pub fn per_hour(&self) -> Option<f32> {
        self.since_charge.per_hour().or_else(|| {
            let all = Use { percent: self.awake.percent + self.asleep.percent, hours: self.awake.hours + self.asleep.hours };
            all.per_hour()
        })
    }

    /// Hours `percent` lasts at `per_hour`.
    pub fn hours_left(percent: u8, per_hour: Option<f32>) -> Option<f32> {
        per_hour.filter(|r| *r > 0.0).map(|r| percent as f32 / r)
    }
}

/// `12 h`, `3 days 4 h`, `45 min`.
pub fn hours_text(hours: f32) -> String {
    let mins = (hours * 60.0).round() as u64;
    match (mins / (24 * 60), mins / 60 % 24, mins % 60) {
        (0, 0, m) => format!("{m} min"),
        (0, h, _) => format!("{h} h"),
        (1, h, _) => format!("1 day {h} h"),
        (d, h, _) => format!("{d} days {h} h"),
    }
}

/// `1.5 % per hour`, `0.2 % per hour`.
pub fn rate_text(per_hour: f32) -> String {
    if per_hour >= 10.0 {
        format!("{per_hour:.0} % per hour")
    } else {
        format!("{per_hour:.1} % per hour")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(t: u64, percent: u8, charging: bool, awake: bool) -> Sample {
        Sample { t, percent, charging, awake }
    }

    const H: u64 = 3600;

    #[test]
    fn lines_round_trip() {
        let a = s(1_700_000_000, 81, false, true);
        assert_eq!(Sample::parse(&a.line()), Some(a));
        assert_eq!(Sample::parse("x,1,0,1"), None);
        assert_eq!(Sample::parse("1,101,0,1"), None);
    }

    #[test]
    fn awake_and_asleep_use_is_split_and_a_charge_starts_over() {
        let t0 = 1_700_000_000;
        let samples = [
            s(t0, 90, true, true),        // charging
            s(t0 + H, 100, false, true),  // off the charger: the run starts
            s(t0 + 3 * H, 90, false, false), // 2 h awake: 10 %, then sleep
            s(t0 + 13 * H, 85, false, true), // 10 h asleep: 5 %
            s(t0 + 14 * H, 80, false, true), // 1 h awake: 5 %
        ];
        let st = Stats::from_samples(&samples, t0 + 14 * H);
        assert_eq!(st.awake, Use { percent: 15.0, hours: 3.0 });
        assert_eq!(st.asleep, Use { percent: 5.0, hours: 10.0 });
        assert_eq!(st.since_charge, Use { percent: 20.0, hours: 13.0 });
        assert_eq!(st.awake.per_hour(), Some(5.0));
        assert_eq!(st.asleep.per_hour(), Some(0.5));
        let left = Stats::hours_left(80, st.per_hour()).unwrap();
        assert!((left - 52.0).abs() < 0.01, "{left}");
        // Charged again: the run since the charge is empty, the week's rates stay.
        let mut more = samples.to_vec();
        more.push(s(t0 + 15 * H, 80, true, true));
        more.push(s(t0 + 16 * H, 100, false, true));
        let st = Stats::from_samples(&more, t0 + 16 * H);
        assert_eq!(st.since_charge, Use::default());
        assert_eq!(st.per_hour(), Some(20.0 / 13.0));
    }

    #[test]
    fn a_small_rise_is_not_a_charge() {
        let t0 = 1_700_000_000;
        let samples = [s(t0, 80, false, true), s(t0 + H, 76, false, true), s(t0 + 2 * H, 77, false, true), s(t0 + 3 * H, 75, false, true)];
        let st = Stats::from_samples(&samples, t0 + 3 * H);
        assert_eq!(st.since_charge, Use { percent: 6.0, hours: 3.0 });
        let charged = [s(t0, 70, false, true), s(t0 + H, 75, false, true)];
        assert_eq!(Stats::from_samples(&charged, t0 + H).since_charge, Use::default());
    }

    #[test]
    fn too_little_data_gives_no_rate_and_old_samples_are_ignored() {
        let t0 = 1_700_000_000;
        assert_eq!(Stats::from_samples(&[s(t0, 80, false, true), s(t0 + 600, 79, false, true)], t0 + 600).per_hour(), None);
        let old = [s(t0, 100, false, true), s(t0 + 5 * H, 50, false, true)];
        assert_eq!(Stats::from_samples(&old, t0 + KEEP.as_secs() + 6 * H), Stats::default());
    }

    #[test]
    fn texts() {
        assert_eq!(hours_text(0.75), "45 min");
        assert_eq!(hours_text(12.2), "12 h");
        assert_eq!(hours_text(30.0), "1 day 6 h");
        assert_eq!(hours_text(77.0), "3 days 5 h");
        assert_eq!(rate_text(1.25), "1.2 % per hour");
        assert_eq!(rate_text(12.4), "12 % per hour");
    }

    #[test]
    fn the_log_appends_samples() {
        let dir = std::env::temp_dir().join(format!("rinky-battery-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("battery.csv");
        let mut log = BatteryLog::new(path.clone());
        assert!(log.due());
        log.note(100, BatteryState { percent: 50, charging: false }, true);
        assert!(!log.due());
        log.note(200, BatteryState { percent: 49, charging: false }, false);
        assert_eq!(load(&path), vec![s(100, 50, false, true), s(200, 49, false, false)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
