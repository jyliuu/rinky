//! The numbers on the Statistics screen, computed from [`StatsDb`] rows.
//!
//! Days are local days: a day index is the number of days since 1970-01-01 in a time
//! zone `utc_offset` seconds east of UTC. Weeks start on Monday.

use ink_store::stats::{BookTotals, DayTotal, StatsDb};
use std::collections::BTreeMap;

/// Seconds per day.
pub const DAY: i64 = 86_400;

/// Local day index of `unix` seconds.
pub fn day_index(unix: u64, utc_offset: i64) -> i64 {
    (unix as i64 + utc_offset).div_euclid(DAY)
}

/// Day index of a civil date (proleptic Gregorian; Howard Hinnant's algorithm).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date `(year, month, day)` of a day index.
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400 + if m <= 2 { 1 } else { 0 }) as i32;
    (y, m, d)
}

/// 0 = Monday ... 6 = Sunday.
pub fn weekday(day: i64) -> u32 {
    (day + 3).rem_euclid(7) as u32
}

/// Day index of a `YYYY-MM-DD` string (as [`DayTotal::day`]).
pub fn parse_day(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let y = it.next()?.parse().ok()?;
    let m = it.next()?.parse().ok()?;
    let d = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// Days in a month.
pub fn month_len(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (days_from_civil(ny, nm, 1) - days_from_civil(y, m, 1)) as u32
}

/// Reading of one day.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Day {
    /// Seconds read.
    pub seconds: u64,
    /// Page records (page views of at least the minimum time).
    pub pages: u64,
}

/// Current streak: consecutive days with reading, ending today, or ending yesterday when
/// nothing was read yet today.
pub fn streak(days: &BTreeMap<i64, Day>, today: i64) -> u32 {
    let read = |d: i64| days.get(&d).is_some_and(|x| x.seconds > 0);
    let mut d = if read(today) { today } else { today - 1 };
    let mut n = 0;
    while read(d) {
        n += 1;
        d -= 1;
    }
    n
}

/// Longest run of consecutive reading days.
pub fn best_streak(days: &BTreeMap<i64, Day>) -> u32 {
    let mut best = 0;
    let mut run = 0;
    let mut prev: Option<i64> = None;
    for (&d, x) in days {
        if x.seconds == 0 {
            continue;
        }
        run = if prev == Some(d - 1) { run + 1 } else { 1 };
        best = best.max(run);
        prev = Some(d);
    }
    best
}

/// Pages per hour, or `None` without reading time.
pub fn pages_per_hour(pages: u64, seconds: u64) -> Option<f64> {
    (seconds > 0).then(|| pages as f64 * 3600.0 / seconds as f64)
}

/// `1 h 05 min`, `12 min`, `40 s`.
pub fn format_duration(secs: u64) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    if h > 0 {
        format!("{h} h {m:02} min")
    } else if m > 0 {
        format!("{m} min")
    } else {
        format!("{secs} s")
    }
}

/// One cell of a month calendar (Monday-first rows of 7); `None` pads before day 1 and
/// after the last day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Day of the month.
    pub day: u32,
    /// Day index.
    pub index: i64,
    /// Reading that day.
    pub read: Day,
}

/// The calendar of month `m` of year `y`: rows of seven cells, Monday first.
pub fn month_grid(y: i32, m: u32, days: &BTreeMap<i64, Day>) -> Vec<[Option<Cell>; 7]> {
    let first = days_from_civil(y, m, 1);
    let len = month_len(y, m) as i64;
    let lead = weekday(first) as i64;
    let rows = (lead + len + 6) / 7;
    (0..rows)
        .map(|r| {
            std::array::from_fn(|c| {
                let i = r * 7 + c as i64 - lead;
                (0..len).contains(&i).then(|| Cell { day: i as u32 + 1, index: first + i, read: days.get(&(first + i)).copied().unwrap_or_default() })
            })
        })
        .collect()
}

/// Everything the Statistics screen shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overview {
    /// Today's day index.
    pub today: i64,
    /// Reading per day (days with reading only).
    pub days: BTreeMap<i64, Day>,
    /// Today.
    pub today_read: Day,
    /// Monday to today.
    pub week: Day,
    /// The last 30 days including today.
    pub month: Day,
    /// Current streak in days.
    pub streak: u32,
    /// Longest streak in days.
    pub best_streak: u32,
    /// All reading time.
    pub total_seconds: u64,
    /// Distinct pages read over all books.
    pub total_pages: u64,
    /// Per book, most recently read first (books with reading time only).
    pub books: Vec<BookTotals>,
}

impl Overview {
    /// Pages per hour over all books.
    pub fn pages_per_hour(&self) -> Option<f64> {
        pages_per_hour(self.total_pages, self.total_seconds)
    }
}

/// Builds the overview from day totals and book totals.
pub fn overview(daily: &[DayTotal], books: Vec<BookTotals>, now: u64, utc_offset: i64) -> Overview {
    let today = day_index(now, utc_offset);
    let days: BTreeMap<i64, Day> =
        daily.iter().filter_map(|d| Some((parse_day(&d.day)?, Day { seconds: d.seconds, pages: d.pages }))).collect();
    let sum = |from: i64| {
        days.range(from..=today).fold(Day::default(), |a, (_, d)| Day { seconds: a.seconds + d.seconds, pages: a.pages + d.pages })
    };
    let books: Vec<BookTotals> = books.into_iter().filter(|b| b.read_time > 0).collect();
    Overview {
        today,
        today_read: days.get(&today).copied().unwrap_or_default(),
        week: sum(today - weekday(today) as i64),
        month: sum(today - 29),
        streak: streak(&days, today),
        best_streak: best_streak(&days),
        total_seconds: books.iter().map(|b| b.read_time).sum(),
        total_pages: books.iter().map(|b| b.pages_read).sum(),
        books,
        days,
    }
}

/// Reads the overview from the database (all days).
pub fn load(db: &StatsDb, now: u64, utc_offset: i64) -> ink_store::Result<Overview> {
    let daily = db.daily(0, now + DAY as u64, utc_offset)?;
    Ok(overview(&daily, db.all_books()?, now, utc_offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_store::stats::{PageRecord, StatsBook};

    #[test]
    fn civil_round_trip_and_weekdays() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(weekday(0), 3); // Thursday
        let d = days_from_civil(2026, 10, 31);
        assert_eq!(civil_from_days(d), (2026, 10, 31));
        assert_eq!(weekday(d), 5); // Saturday
        assert_eq!(parse_day("2026-10-31"), Some(d));
        assert_eq!(parse_day("2026-13-01"), None);
        assert_eq!(parse_day("x"), None);
        assert_eq!(month_len(2024, 2), 29);
        assert_eq!(month_len(2026, 2), 28);
        assert_eq!(month_len(2026, 12), 31);
        for z in [-800_000, -1, 0, 59, 60, 365, 20_000, 1_000_000] {
            let (y, m, dd) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, dd), z);
        }
        // Local days: 23:30 UTC is the next day at UTC+1.
        let t = (d * DAY + 23 * 3600 + 1800) as u64;
        assert_eq!(day_index(t, 0), d);
        assert_eq!(day_index(t, 3600), d + 1);
    }

    fn days(list: &[(i64, u64)]) -> BTreeMap<i64, Day> {
        list.iter().map(|&(d, s)| (d, Day { seconds: s, pages: 1 })).collect()
    }

    #[test]
    fn streaks() {
        let m = days(&[(10, 60), (11, 60), (12, 60), (14, 60), (15, 60)]);
        assert_eq!(streak(&m, 15), 2);
        assert_eq!(streak(&m, 16), 2); // nothing yet today: counts up to yesterday
        assert_eq!(streak(&m, 17), 0);
        assert_eq!(streak(&m, 12), 3);
        assert_eq!(best_streak(&m), 3);
        assert_eq!(best_streak(&BTreeMap::new()), 0);
    }

    #[test]
    fn calendar_grid() {
        // October 2026 starts on a Thursday and has 31 days: 5 rows.
        let first = days_from_civil(2026, 10, 1);
        let g = month_grid(2026, 10, &days(&[(first + 4, 600)]));
        assert_eq!(g.len(), 5);
        assert!(g[0][..3].iter().all(Option::is_none));
        assert_eq!(g[0][3].unwrap().day, 1);
        assert_eq!(g[0][3].unwrap().index, first);
        assert_eq!(g[1][0].unwrap().day, 5);
        assert_eq!(g[1][0].unwrap().read.seconds, 600);
        assert_eq!(g[4][5].unwrap().day, 31);
        assert!(g[4][6].is_none());
        // February 2021 starts on a Monday and has 28 days: exactly 4 rows.
        assert_eq!(month_grid(2021, 2, &BTreeMap::new()).len(), 4);
    }

    #[test]
    fn formatting() {
        assert_eq!(format_duration(40), "40 s");
        assert_eq!(format_duration(12 * 60 + 5), "12 min");
        assert_eq!(format_duration(3600 + 5 * 60), "1 h 05 min");
        assert_eq!(pages_per_hour(30, 1800), Some(60.0));
        assert_eq!(pages_per_hour(3, 0), None);
    }

    #[test]
    fn overview_from_database() {
        let mut db = StatsDb::open_in_memory().unwrap();
        let today = days_from_civil(2026, 10, 31); // Saturday
        let at = |d: i64, h: i64| (d * DAY + h * 3600) as u64;
        let id = db.upsert_book(&StatsBook { md5: "a".into(), title: "A".into(), ..Default::default() }, at(today, 0)).unwrap();
        // Read on Friday, Saturday (this week), and last Sunday (previous week).
        for (n, d) in [(today - 6, 10), (today - 1, 12), (today, 9)].iter().enumerate() {
            let s = db.start_session(id, at(d.0, d.1)).unwrap();
            let recs: Vec<PageRecord> = (0..3)
                .map(|i| PageRecord { page: (n * 3 + i) as u32 + 1, total_pages: 100, start_time: at(d.0, d.1) + i as u64 * 60, duration: 60 })
                .collect();
            db.record_pages(s, &recs).unwrap();
            db.end_session(s, at(d.0, d.1) + 180).unwrap();
        }
        let o = load(&db, at(today, 20), 0).unwrap();
        assert_eq!(o.today, today);
        assert_eq!(o.today_read, Day { seconds: 180, pages: 3 });
        assert_eq!(o.week, Day { seconds: 360, pages: 6 }); // Monday..Saturday
        assert_eq!(o.month.seconds, 540);
        assert_eq!(o.streak, 2);
        assert_eq!(o.best_streak, 2);
        assert_eq!(o.total_seconds, 540);
        assert_eq!(o.total_pages, 9);
        assert_eq!(o.pages_per_hour(), Some(60.0));
        assert_eq!(o.books.len(), 1);
        assert_eq!(o.days.len(), 3);
    }
}
