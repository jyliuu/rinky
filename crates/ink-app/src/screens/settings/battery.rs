//! Settings page "Battery": the charge now, what the battery used per hour awake and
//! asleep and since the last charge, and how long the charge left may last, from the
//! samples of [`crate::battery_log`]. Opened from the main list (Device), from the
//! battery card of Device info and from the control center's Battery button.

use super::rows::Row;
use super::{PageModule, Reply};
use crate::battery_log::{self, hours_text, Rate, Sample, Stats, Use};
use crate::router::AppCtx;
use ink_store::Settings;

/// The page.
#[derive(Default)]
pub struct Page;

/// `5 % in 3 h`, or "Not enough data yet".
fn used_text(u: &Use) -> String {
    if u.hours < 0.1 {
        return "Not enough data yet".into();
    }
    format!("{:.0} % in {}", u.percent, hours_text(u.hours))
}

/// A row with `title` on the left and `value` on the right (not tappable).
fn value_row(title: &str, value: impl Into<String>) -> Row {
    Row::item(title, "").value(value)
}

/// The rows for the charge `percent` (`None`: unknown), `charging`, and the stats.
pub fn rows_for(percent: Option<u8>, charging: bool, st: &Stats) -> Vec<Row> {
    let mut rows = vec![Row::header("Now")];
    rows.push(value_row(
        "Charge",
        match percent {
            Some(p) if charging => format!("{p} %, charging"),
            Some(p) => format!("{p} %"),
            None => "Unknown".into(),
        },
    ));
    let none = "Not enough data yet";
    rows.push(Row::header("Estimated time left"));
    let left = |rate: Option<Rate>| match (percent, charging) {
        (_, true) => "Charging".to_string(),
        (Some(p), false) => rate.and_then(|r| r.left_text(p)).unwrap_or_else(|| none.to_string()),
        (None, false) => none.to_string(),
    };
    rows.push(value_row("As you use it", left(st.per_hour())));
    rows.push(value_row("Reading all the time", left(st.awake.per_hour())));
    rows.push(value_row("Asleep all the time", left(st.asleep.per_hour())));
    rows.push(Row::header("Use per hour (last 7 days)"));
    let rate = |u: &Use| u.per_hour().map_or_else(|| none.to_string(), |r| r.text());
    rows.push(value_row("Awake", rate(&st.awake)).subtitle(used_text(&st.awake)));
    rows.push(value_row("Asleep", rate(&st.asleep)).subtitle(used_text(&st.asleep)));
    rows.push(Row::header("Since the last charge"));
    rows.push(value_row("Used", used_text(&st.since_charge)));
    rows.push(value_row("Per hour", rate(&st.since_charge)));
    rows.push(Row::note("Noted while rinky runs: as the charge drops, at each sleep and wake."));
    rows
}

impl PageModule for Page {
    fn summary(&self, _s: &Settings) -> String {
        String::new()
    }

    fn rows(&mut self, cx: &mut AppCtx) -> Vec<Row> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let b = cx.services.battery.state().ok();
        // The charge now ends the last stretch, which the log has not noted yet.
        let mut samples = battery_log::load(&cx.paths.battery_log());
        samples.extend(b.map(|b| Sample { t: now, percent: b.percent, charging: b.charging, awake: true }));
        let st = Stats::from_samples(&samples, now);
        rows_for(b.map(|b| b.percent), b.is_some_and(|b| b.charging), &st)
    }

    fn action(&mut self, _action: &str, _cx: &mut AppCtx) -> Reply {
        Reply::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(rows: &[Row]) -> Vec<(String, String)> {
        rows.iter().filter_map(|r| Some((r.title.clone(), r.value.clone()?))).collect()
    }

    #[test]
    fn rows_show_rates_and_time_left() {
        let st = Stats {
            awake: Use { percent: 15.0, hours: 3.0 },
            asleep: Use { percent: 5.0, hours: 10.0 },
            since_charge: Use { percent: 20.0, hours: 13.0 },
        };
        let v = values(&rows_for(Some(80), false, &st));
        let get = |t: &str| v.iter().find(|(k, _)| k == t).map(|(_, v)| v.as_str());
        assert_eq!(get("Charge"), Some("80 %"));
        assert_eq!(get("As you use it"), Some("2 days 4 h"));
        assert_eq!(get("Reading all the time"), Some("16 h"));
        assert_eq!(get("Asleep all the time"), Some("6 days 16 h"));
        assert_eq!(get("Awake"), Some("5.0 % per hour"));
        assert_eq!(get("Used"), Some("20 % in 13 h"));
    }

    #[test]
    fn rows_with_no_point_used_show_a_bound() {
        let st = Stats { asleep: Use { percent: 0.0, hours: 8.0 }, since_charge: Use { percent: 0.0, hours: 8.0 }, ..Default::default() };
        let v = values(&rows_for(Some(80), false, &st));
        let get = |t: &str| v.iter().find(|(k, _)| k == t).map(|(_, v)| v.as_str());
        assert_eq!(get("Asleep"), Some("under 0.1 % per hour"));
        assert_eq!(get("Asleep all the time"), Some("over 26 days 16 h"));
        assert_eq!(get("As you use it"), Some("over 26 days 16 h"));
        assert_eq!(get("Awake"), Some("Not enough data yet"));
    }

    #[test]
    fn rows_without_data_say_so() {
        let v = values(&rows_for(Some(50), true, &Stats::default()));
        assert!(v.contains(&("Charge".into(), "50 %, charging".into())));
        assert!(v.contains(&("As you use it".into(), "Charging".into())));
        assert!(v.contains(&("Awake".into(), "Not enough data yet".into())));
    }
}
