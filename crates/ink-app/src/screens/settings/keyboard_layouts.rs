//! Settings page "Keyboard layouts": which on-screen keyboard layouts the globe key
//! cycles (`settings.ui.keyboard_layouts`, names from
//! [`ink_store::prefs::KEYBOARD_LAYOUTS`]) and their order; the first is the default.
//! Screens build their keyboard with [`keyboard`].

use super::order;
use super::rows::Row;
use super::{current_settings, PageModule, Reply};
use crate::router::AppCtx;
use ink_store::prefs::KEYBOARD_LAYOUTS;
use ink_store::Settings;

/// Row action prefix.
const PREFIX: &str = "kbd";
/// At least one layout stays on.
const MIN: usize = 1;

/// The enabled layouts of `s` that are known (empty list: English).
pub fn layouts(s: &Settings) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for l in &s.ui.keyboard_layouts {
        if KEYBOARD_LAYOUTS.iter().any(|(n, _)| n == l) && !v.contains(l) {
            v.push(l.clone());
        }
    }
    if v.is_empty() {
        v.push("en".into());
    }
    v
}

/// An on-screen keyboard with the layouts of `s` (first one shown, the globe key cycles).
pub fn keyboard(s: &Settings) -> ink_ui::Keyboard {
    ink_ui::Keyboard::with_language_list(&layouts(s))
}

/// The page.
#[derive(Default)]
pub struct Page;

impl PageModule for Page {
    fn summary(&self, s: &Settings) -> String {
        layouts(s).iter().map(|l| l.to_uppercase()).collect::<Vec<_>>().join(", ")
    }

    fn rows(&mut self, cx: &mut AppCtx) -> Vec<Row> {
        let chosen = layouts(current_settings(cx));
        let mut rows = vec![Row::header("Layouts"), order::order_note(MIN, KEYBOARD_LAYOUTS.len())];
        rows.extend(order::order_rows(KEYBOARD_LAYOUTS, &chosen, PREFIX));
        rows.push(Row::note("The first layout is the default; the globe key switches."));
        rows
    }

    fn action(&mut self, action: &str, cx: &mut AppCtx) -> Reply {
        let chosen = layouts(current_settings(cx));
        let range = (MIN, KEYBOARD_LAYOUTS.len());
        order::page_action(action, PREFIX, KEYBOARD_LAYOUTS, chosen, range, cx, |s, v| s.ui.keyboard_layouts = v).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_and_duplicate_layouts_are_skipped() {
        let mut s = Settings::default();
        s.ui.keyboard_layouts = vec!["xx".into(), "da".into(), "da".into(), "de".into()];
        assert_eq!(layouts(&s), ["da", "de"]);
        s.ui.keyboard_layouts.clear();
        assert_eq!(layouts(&s), ["en"]);
    }
}
