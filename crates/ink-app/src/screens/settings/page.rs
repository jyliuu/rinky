//! The settings page: the one full-screen page every settings page is built from.
//!
//! A page is an [`AppBar`] with a back button (and optional bar actions) at the top, a
//! [`SettingsList`] of [`Row`]s (headers, notes, switches, choices, sliders, links to
//! sub-pages, actions) in [`content_rect`], and a status line at the bottom. The
//! settings screen builds every row page with [`add_bar`], [`add_list`] and
//! [`add_status`]: the built-in pages, the [`super::PageModule`] pages and the plugin
//! pages (a plugin's settings menu shown as a page, see
//! [`super::SettingsScreen`], `Page::Plugin`). Only pages that are not lists keep their own
//! widgets under the same bar and status line (the Wi-Fi password keyboard, the device
//! info cards, the update card, the status bar preview, the font previews).
//!
//! The page is shown whole with one REAGL update (no GC16 flash,
//! [`ink_ui::refresh::Change::LargePanel`]); a change of one row repaints that row only.

use super::rows::{Row, SettingsList};
use ink_device::{Rect, Size};
use ink_ui::bars::APP_BAR_H;
use ink_ui::theme::space;
use ink_ui::{AppBar, BarAction, Label, WidgetId, WidgetTree};

/// Height of the status line at the bottom of every page.
pub const STATUS_H: u32 = 88;

/// The area of a page's list: under the app bar, above the status line.
pub fn content_rect(size: Size) -> Rect {
    let top = APP_BAR_H as i32 + space(1);
    Rect::new(0, top, size.w, (size.h as i32 - top - STATUS_H as i32).max(200) as u32)
}

/// The app bar of every page.
pub fn app_bar_rect(size: Size) -> Rect {
    Rect::new(0, 0, size.w, APP_BAR_H)
}

/// The status line under the list.
pub fn status_rect(size: Size) -> Rect {
    let content = content_rect(size);
    Rect::new(space(4), content.bottom() + space(1), size.w - 2 * space(4) as u32, STATUS_H - space(2) as u32)
}

/// Adds the app bar: `title`, a back button, `actions` on the right.
pub fn add_bar(tree: &mut WidgetTree, size: Size, title: impl Into<String>, actions: Vec<BarAction>) -> WidgetId {
    let mut bar = AppBar::new(title).with_back();
    for a in actions {
        bar = bar.action(a);
    }
    tree.add(None, Box::new(bar), app_bar_rect(size))
}

/// Adds the list of `rows` in `area`, scrolled to `scroll` (snapped to a row top) and
/// then as little as needed to show the row with action `reveal`.
pub fn add_list(tree: &mut WidgetTree, rows: Vec<Row>, area: Rect, scroll: i32, reveal: Option<&str>) -> WidgetId {
    let mut list = SettingsList::new(rows);
    list.scroll = list.snap(area, scroll, false);
    if let Some(a) = reveal {
        list.reveal(area, a);
    }
    tree.add(None, Box::new(list), area)
}

/// Adds the status line with `text`.
pub fn add_status(tree: &mut WidgetTree, size: Size, text: &str) -> WidgetId {
    let mut status = Label::left(text.to_string(), 32.0);
    status.centered = false;
    tree.add(None, Box::new(status), status_rect(size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_parts_stack_without_overlap_in_both_orientations() {
        for size in [Size::new(1272, 1696), Size::new(1696, 1272)] {
            let (bar, list, status) = (app_bar_rect(size), content_rect(size), status_rect(size));
            assert!(bar.bottom() <= list.y && list.bottom() <= status.y && status.bottom() <= size.h as i32, "{size:?}");
            assert!(status.h >= 64 && list.h >= 1000, "{size:?}");
        }
    }
}
