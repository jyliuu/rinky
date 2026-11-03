//! Gestures page of the settings screen: the list of reader gestures with their current
//! action, and one picker page per gesture. The picker lists the built-in actions, then
//! under "Plugins" what the enabled plugins offer
//! ([`crate::plugin_entries::gesture_choices`]: their actions and reader menu items).
//!
//! Row actions: `gest:<trigger>` opens the picker of `trigger`, `gset:<trigger>:<action>`
//! stores `action` for `trigger` (the action may itself contain `:`, as in
//! `plugin:<plugin>:<item>`), `gest-reset` puts back [`ink_store::prefs::DEFAULT_GESTURES`].
//! Everything here is pure except [`dispatch`], which reads and writes
//! [`AppCtx::settings_update`].

use super::rows::{Row, Trail};
use crate::router::AppCtx;
use ink_store::prefs::{label, GestureMap, ACTIONS, GESTURES, LONG_PRESS_ACTIONS};
use ink_store::Settings;

/// What a gesture action asks the settings screen to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Show the list (`None`) or the picker of a trigger (`Some(trigger)`).
    Open(Option<String>),
    /// The settings changed: show the list with this status line.
    Saved(String),
}

/// The section header of `trigger`.
fn section(trigger: &str) -> &'static str {
    if trigger.starts_with("two_finger") || trigger.starts_with("pinch") {
        "Two fingers"
    } else if trigger.contains("_from_") {
        "Edge swipes"
    } else if trigger.starts_with("swipe") {
        "Swipes"
    } else {
        "Taps"
    }
}

/// The label of a gesture trigger (the trigger name when unknown).
pub fn gesture_label(trigger: &str) -> &str {
    label(GESTURES, trigger)
}

/// The label of `action`: for `plugin:<plugin>:<item>` its label in `plugins` (the
/// `(saved id, label)` list of [`crate::plugin_entries::gesture_choices`]) or
/// `<item> (<plugin>, off)` when no enabled plugin offers it; else its label in
/// [`LONG_PRESS_ACTIONS`] or [`ACTIONS`], else the name itself.
pub fn action_label(action: &str, plugins: &[(String, String)]) -> String {
    if let Some((plugin, item)) = action.strip_prefix("plugin:").and_then(|r| r.split_once(':')) {
        return plugins.iter().find(|(n, _)| n == action).map_or_else(|| format!("{item} ({plugin}, off)"), |(_, l)| l.clone());
    }
    LONG_PRESS_ACTIONS.iter().chain(ACTIONS).find(|(n, _)| *n == action).map_or(action, |(_, l)| *l).to_string()
}

/// The actions offered for `trigger`: `long_press` lists [`LONG_PRESS_ACTIONS`] first.
pub fn choices(trigger: &str) -> Vec<(&'static str, &'static str)> {
    let extra: &[(&str, &str)] = if trigger == "long_press" { LONG_PRESS_ACTIONS } else { &[] };
    extra.iter().chain(ACTIONS).copied().collect()
}

/// The page title: "Gestures", or the label of the gesture being picked.
pub fn title(picking: Option<&str>) -> String {
    picking.map_or_else(|| "Gestures".to_string(), |t| gesture_label(t).to_string())
}

/// The rows of the list page (`picking` = `None`) or of the picker of a trigger.
/// `plugins` is what the enabled plugins offer, as `(saved id, label)`.
pub fn rows(map: &GestureMap, picking: Option<&str>, plugins: &[(String, String)]) -> Vec<Row> {
    if let Some(trigger) = picking {
        let now = map.action(trigger);
        let mut rows = vec![Row::header("Action")];
        let list = choices(trigger);
        if !list.iter().any(|(n, _)| *n == now) && !plugins.iter().any(|(n, _)| n == now) {
            // An action of a plugin that is off, or an unknown one, stored in the file:
            // shown checked on top.
            rows.push(Row::choice(action_label(now, plugins), true, format!("gset:{trigger}:{now}")));
        }
        rows.extend(list.iter().map(|(n, l)| Row::choice(*l, *n == now, format!("gset:{trigger}:{n}"))));
        if !plugins.is_empty() {
            rows.push(Row::header("Plugins"));
            rows.extend(plugins.iter().map(|(n, l)| Row::choice(l.as_str(), n == now, format!("gset:{trigger}:{n}"))));
        }
        return rows;
    }
    let mut rows = Vec::new();
    let mut last = "";
    for (trigger, l) in GESTURES {
        let s = section(trigger);
        if s != last {
            rows.push(Row::header(s));
            last = s;
        }
        rows.push(Row::item(*l, format!("gest:{trigger}")).trail(Trail::Chevron).value(action_label(map.action(trigger), plugins)));
    }
    rows.push(Row::header("Defaults"));
    rows.push(Row::item("Reset to defaults", "gest-reset"));
    rows.push(Row::note(
        "Corners are squares of 1/8 of the screen width. Edge swipes start within 1/20 of the screen of an edge.",
    ));
    rows
}

/// Runs a gesture row action on `s`; `plugins` gives the labels of plugin actions for the
/// status line. `None`: not a gesture action (or an unknown trigger or action, which is
/// ignored).
pub fn action(a: &str, s: &mut Settings, plugins: &[(String, String)]) -> Option<Step> {
    if a == "gest-reset" {
        s.gestures = GestureMap::default();
        log::info!("settings gestures: reset to defaults");
        return Some(Step::Saved("Gestures reset to defaults".into()));
    }
    if let Some(trigger) = a.strip_prefix("gest:") {
        GESTURES.iter().find(|(g, _)| *g == trigger)?;
        log::info!("settings gestures: open {trigger}");
        return Some(Step::Open(Some(trigger.to_string())));
    }
    let (trigger, act) = a.strip_prefix("gset:")?.split_once(':')?;
    GESTURES.iter().find(|(g, _)| *g == trigger)?;
    if !act.starts_with("plugin:") && !choices(trigger).iter().any(|(n, _)| *n == act) {
        return None;
    }
    s.gestures.set(trigger, act);
    log::info!("settings gestures: {trigger} -> {act}");
    Some(Step::Saved(format!("{}: {}", gesture_label(trigger), action_label(act, plugins))))
}

/// Runs a gesture row action against the newest settings and stores a change in
/// [`AppCtx::settings_update`]. Returns the gesture page to show (`None` = the list) and
/// the status line, or `None` when `a` is not a gesture action.
pub fn dispatch(a: &str, cx: &mut AppCtx) -> Option<(Option<String>, String)> {
    if !a.starts_with("gest") && !a.starts_with("gset:") {
        return None;
    }
    let mut s = cx.settings_update.as_ref().unwrap_or(cx.settings).clone();
    let plugins = crate::plugin_entries::gesture_choices(cx.plugins);
    Some(match action(a, &mut s, &plugins)? {
        Step::Open(p) => (p, String::new()),
        Step::Saved(text) => {
            *cx.settings_update = Some(s);
            (None, text)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gset_saves_and_reset_restores() {
        let mut s = Settings::default();
        let action = |a: &str, s: &mut Settings| action(a, s, &[]);
        assert_eq!(action("gset:tap_left:next_page", &mut s), Some(Step::Saved("Tap left side: Next page".into())));
        assert_eq!(s.gestures.action("tap_left"), "next_page");
        assert_eq!(action("gset:tap_left:bogus", &mut s), None);
        assert_eq!(action("gest:tap_left", &mut s), Some(Step::Open(Some("tap_left".into()))));
        assert_eq!(action("gest-reset", &mut s), Some(Step::Saved("Gestures reset to defaults".into())));
        assert_eq!(s.gestures, GestureMap::default());
        assert_eq!(action("anim:slide", &mut s), None);
    }

    #[test]
    fn long_press_picker_lists_its_actions_first() {
        let map = GestureMap::default();
        let r = rows(&map, Some("long_press"), &[]);
        assert_eq!(r[1].action, "gset:long_press:dictionary");
        assert_eq!(r[2].action, "gset:long_press:select");
        assert_eq!(r.len(), 1 + LONG_PRESS_ACTIONS.len() + ACTIONS.len());
        assert_eq!(action_label("plugin:p:Do it", &[]), "Do it (p, off)");
    }

    #[test]
    fn plugin_actions_are_offered_and_labelled() {
        let plugins = vec![("plugin:ai:ask".to_string(), "AI: Ask".to_string())];
        let mut s = Settings::default();
        let r = rows(&s.gestures, Some("two_finger_tap"), &plugins);
        assert_eq!(r.len(), 1 + ACTIONS.len() + 2);
        assert_eq!(r[r.len() - 2].title, "Plugins");
        assert_eq!(r.last().map(|r| r.action.as_str()), Some("gset:two_finger_tap:plugin:ai:ask"));
        assert_eq!(action("gset:two_finger_tap:plugin:ai:ask", &mut s, &plugins), Some(Step::Saved("Two-finger tap: AI: Ask".into())));
        assert_eq!(s.gestures.action("two_finger_tap"), "plugin:ai:ask");
        // Checked in the Plugins section, not repeated on top.
        let r = rows(&s.gestures, Some("two_finger_tap"), &plugins);
        assert_eq!(r.len(), 1 + ACTIONS.len() + 2);
        assert!(r.last().is_some_and(|r| r.trail == Trail::Check(true)));
        let list = rows(&s.gestures, None, &plugins);
        assert!(list.iter().any(|r| r.action == "gest:two_finger_tap" && r.value.as_deref() == Some("AI: Ask")));
        // The plugin turned off: the stored action stays, shown on top.
        let r = rows(&s.gestures, Some("two_finger_tap"), &[]);
        assert_eq!((r.len(), r[1].title.as_str()), (2 + ACTIONS.len(), "ask (ai, off)"));
    }
}
