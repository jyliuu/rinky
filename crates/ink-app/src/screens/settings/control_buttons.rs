//! Settings page "Control center buttons": which buttons the control center shows
//! (`settings.ui.control_buttons`, names from [`ink_store::prefs::CONTROL_BUTTONS`]) and
//! in which order, at most [`quick_light::MAX_BUTTONS`] (two rows of tiles), and the
//! buttons' long-press options ([`advanced::settings_rows`]: the Rotate angle as choices,
//! links to the Wi-Fi, status bars and full refresh pages).

use super::advanced;
use super::order;
use super::quick_light;
use super::rows::Row;
use super::{current_settings, edit_settings, PageModule, Reply};
use crate::plugin_entries;
use crate::plugin_host::PluginHost;
use crate::router::AppCtx;
use ink_store::prefs::CONTROL_BUTTONS;
use ink_store::Settings;

/// Row action prefix.
const PREFIX: &str = "ccbtn";
/// At least one button stays on.
const MIN: usize = 1;

/// The page's rows for settings `s`: the buttons in order, then their long-press options.
pub fn rows_for(s: &Settings, host: &PluginHost) -> Vec<Row> {
    let chosen = quick_light::buttons(s, host);
    let extra = plugin_entries::control_choices(host);
    let list = plugin_entries::merged(CONTROL_BUTTONS, &extra);
    let mut rows = vec![Row::header("Shown in the control center"), order::order_note(MIN, quick_light::MAX_BUTTONS)];
    rows.extend(order::order_rows(&list, &chosen, PREFIX));
    rows.push(Row::note("Swipe down from the top edge to open the control center."));
    rows.extend(advanced::settings_rows(s));
    rows
}

/// The page.
#[derive(Default)]
pub struct Page;

impl PageModule for Page {
    fn summary(&self, s: &Settings) -> String {
        match quick_light::buttons_unchecked(s).len() {
            1 => "1 button".into(),
            n => format!("{n} buttons"),
        }
    }

    fn rows(&mut self, cx: &mut AppCtx) -> Vec<Row> {
        rows_for(current_settings(cx), cx.plugins)
    }

    fn action(&mut self, action: &str, cx: &mut AppCtx) -> Reply {
        if let Some(text) = advanced::settings_action(action, cx) {
            return Reply::rebuild(text);
        }
        let Some((name, op)) = action.strip_prefix(PREFIX).and_then(|a| a.strip_prefix(':')).and_then(order::split) else {
            return Reply::default();
        };
        let mut chosen = quick_light::buttons(current_settings(cx), cx.plugins);
        let extra = plugin_entries::control_choices(cx.plugins);
        let list = plugin_entries::merged(CONTROL_BUTTONS, &extra);
        match order::apply(&mut chosen, &list, name, op, MIN, quick_light::MAX_BUTTONS) {
            Ok(text) => {
                log::info!("settings: control center buttons {name} {op} -> {chosen:?}");
                edit_settings(cx, |s| s.ui.control_buttons = chosen);
                Reply::rebuild(text)
            }
            Err(text) => {
                log::info!("settings: control center buttons {name} {op} refused: {text}");
                Reply { status: Some(text), ..Reply::default() }
            }
        }
    }
}
