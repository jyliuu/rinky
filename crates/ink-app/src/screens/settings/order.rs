//! Ordered choices: a list of known names (`(name, label)`, e.g.
//! `ink_store::prefs::NAV_ITEMS`) of which some are on, in an order the user picks.
//! The page shows the chosen items first, in their order, each with "move up",
//! "move down" and a switch ([`super::rows::Trail::Order`]), then the other items
//! switched off. Used by the navigation bar page; the control-center buttons and the
//! keyboard layouts pages can use it too.

use super::rows::{Row, Trail};
use super::{edit_settings, Reply};
use crate::router::AppCtx;
use ink_store::Settings;
use ink_ui::Icon;

/// Rows for `list` with `chosen` on and in order. Row actions are `<prefix>:<name>`;
/// the list reports `<prefix>:<name>:up|down|on|off`.
pub fn order_rows(list: &[(&str, &str)], chosen: &[String], prefix: &str) -> Vec<Row> {
    let n = chosen.len();
    let mut rows = Vec::new();
    for (i, name) in chosen.iter().enumerate() {
        let label = ink_store::prefs::label(list, name);
        let sub = if i == 0 { "1 · first".to_string() } else { format!("{}", i + 1) };
        rows.push(Row::item(label, format!("{prefix}:{name}")).subtitle(sub).trail(Trail::Order { on: true, up: i > 0, down: i + 1 < n }));
    }
    for (name, label) in list.iter().filter(|(name, _)| !chosen.iter().any(|c| c == name)) {
        rows.push(Row::item(*label, format!("{prefix}:{name}")).trail(Trail::Order { on: false, up: false, down: false }));
    }
    rows
}

/// A heading row for an ordered page with `min`..=`max` items.
pub fn order_note(min: usize, max: usize) -> Row {
    Row::note(format!("Choose {min} to {max}. The arrows change the order.")).lead(super::rows::Lead::Icon(Icon::Sort))
}

/// Applies `op` (`up`, `down`, `on`, `off`) to `name` in `chosen`, keeping between `min`
/// and `max` items. Returns the status text, or why nothing changed.
pub fn apply(chosen: &mut Vec<String>, list: &[(&str, &str)], name: &str, op: &str, min: usize, max: usize) -> Result<String, String> {
    if !list.iter().any(|(n, _)| *n == name) {
        return Err(format!("unknown item {name}"));
    }
    let label = ink_store::prefs::label(list, name);
    let pos = chosen.iter().position(|c| c == name);
    match (op, pos) {
        ("up", Some(i)) if i > 0 => {
            chosen.swap(i, i - 1);
            Ok(format!("{label} moved up"))
        }
        ("down", Some(i)) if i + 1 < chosen.len() => {
            chosen.swap(i, i + 1);
            Ok(format!("{label} moved down"))
        }
        ("on", None) if chosen.len() >= max => Err(format!("At most {max}: turn another one off first")),
        ("on", None) => {
            chosen.push(name.to_string());
            Ok(format!("{label} added"))
        }
        ("off", Some(_)) if chosen.len() <= min => Err(format!("At least {min} must stay on")),
        ("off", Some(i)) => {
            chosen.remove(i);
            Ok(format!("{label} removed"))
        }
        _ => Err(format!("{label}: nothing to change")),
    }
}

/// Splits a reported `<prefix>:<name>:<op>` action (prefix already removed: `<name>:<op>`).
pub fn split(arg: &str) -> Option<(&str, &str)> {
    arg.rsplit_once(':')
}

/// A page module's reply to a reported `<prefix>:<name>:<op>` action: [`apply`] on
/// `chosen` (between `min` and `max` of `list`), the new order saved with `store`; a
/// refused change only says why. `None`: not an action of `prefix`.
pub fn page_action(
    action: &str,
    prefix: &str,
    list: &[(&str, &str)],
    mut chosen: Vec<String>,
    (min, max): (usize, usize),
    cx: &mut AppCtx,
    store: impl FnOnce(&mut Settings, Vec<String>),
) -> Option<Reply> {
    let (name, op) = action.strip_prefix(prefix)?.strip_prefix(':').and_then(split)?;
    Some(match apply(&mut chosen, list, name, op, min, max) {
        Ok(text) => {
            log::info!("settings: {prefix} {name} {op} -> {chosen:?}");
            edit_settings(cx, |s| store(s, chosen));
            Reply::rebuild(text)
        }
        Err(text) => {
            log::info!("settings: {prefix} {name} {op} refused: {text}");
            Reply { status: Some(text), ..Reply::default() }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_store::prefs::NAV_ITEMS;

    #[test]
    fn apply_moves_adds_and_keeps_the_limits() {
        let mut v: Vec<String> = ["home", "library"].iter().map(|s| s.to_string()).collect();
        assert!(apply(&mut v, NAV_ITEMS, "home", "off", 2, 5).is_err(), "at least 2");
        assert!(apply(&mut v, NAV_ITEMS, "stats", "on", 2, 5).is_ok());
        assert!(apply(&mut v, NAV_ITEMS, "stats", "up", 2, 5).is_ok());
        assert_eq!(v, ["home", "stats", "library"]);
        assert!(apply(&mut v, NAV_ITEMS, "home", "up", 2, 5).is_err());
        assert!(apply(&mut v, NAV_ITEMS, "nope", "on", 2, 5).is_err());
        let rows = order_rows(NAV_ITEMS, &v, "nav");
        assert_eq!(rows.len(), NAV_ITEMS.len());
        assert_eq!(rows[0].trail, Trail::Order { on: true, up: false, down: true });
        assert_eq!(rows[3].trail, Trail::Order { on: false, up: false, down: false });
    }
}
