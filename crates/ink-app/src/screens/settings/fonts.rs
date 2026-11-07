//! Settings page "Fonts": the reading font (default for books without their own choice,
//! `reader.font_family`) and the interface font (`ui.font_family`, the whole UI).
//!
//! A segmented control switches between the two pickers; under it a sample block shows
//! a sentence in the selected family, then the family list: the built-in Noto Serif and
//! every installed family ([`crate::fonts`]; for the interface, the built-in Noto Sans
//! and only families with bold and italic faces), each name drawn in its own font, a
//! check mark on the selected one. With more families than rows the last row turns the page.
//!
//! Actions: `fonts-tab:I` (0 reading, 1 interface), `font-reading:NAME`,
//! `font-ui:NAME`, `fonts-more`. The page is self-contained: the settings screen calls
//! [`build`] for its content area and gives every action to [`action`] first.

use crate::fonts::{self, BUILTIN};
use crate::router::AppCtx;
use ink_device::Rect;
use ink_store::Settings;
use ink_text::Style;
use ink_ui::draw::stroke_round_rect;
use ink_ui::theme::space;
use ink_ui::{EventResult, Gesture, Icon, PaintCx, Segmented, UiEvent, Widget, WidgetTree};
use std::cell::Cell;

/// Page name (`page:fonts`).
pub const PAGE: &str = "fonts";
/// Page title.
pub const TITLE: &str = "Fonts";
/// The sample sentence.
pub const SAMPLE: &str = "Sphinx of black quartz, judge my vow. 0123456789";
/// Height of a family row.
pub const ROW_H: u32 = 120;
/// Height of the sample block.
pub const SAMPLE_H: u32 = 220;

thread_local! {
    /// The picker shown (0 reading, 1 interface).
    static TAB: Cell<usize> = const { Cell::new(0) };
    /// The list page shown.
    static LIST_PAGE: Cell<usize> = const { Cell::new(0) };
}

/// The settings the page shows (a change not yet taken by the app counts).
fn current<'a>(cx: &'a AppCtx) -> &'a Settings {
    cx.settings_update.as_ref().unwrap_or(cx.settings)
}

/// The family picked for reading (`reader.font_family`, resolved like the reader does).
pub fn reading_family(s: &Settings) -> String {
    fonts::reading_default(s).unwrap_or_else(|| BUILTIN.to_string())
}

/// The family picked for the interface (`ui.font_family`; empty: built-in).
pub fn ui_family(s: &Settings) -> String {
    match s.ui.font_family.trim() {
        "" => fonts::UI_BUILTIN.to_string(),
        f => f.to_string(),
    }
}

/// Value for a settings row: "Bookerly · UI Noto Sans".
pub fn summary(s: &Settings) -> String {
    format!("{} · UI {}", reading_family(s), ui_family(s))
}

/// Applies a page action; `Some(status text)` when it was one (the screen then builds
/// the page again), `None` otherwise.
pub fn action(a: &str, cx: &mut AppCtx) -> Option<String> {
    let (verb, arg) = a.split_once(':').unwrap_or((a, ""));
    let edit = |cx: &mut AppCtx, f: &dyn Fn(&mut Settings)| {
        let mut s = cx.settings_update.take().unwrap_or_else(|| cx.settings.clone());
        f(&mut s);
        *cx.settings_update = Some(s);
    };
    match verb {
        "fonts-tab" => {
            TAB.with(|t| t.set(arg.parse::<usize>().unwrap_or(0).min(1)));
            LIST_PAGE.with(|p| p.set(0));
            log::info!("fonts page: {} picker", if TAB.with(Cell::get) == 0 { "reading" } else { "interface" });
            Some(String::new())
        }
        "fonts-more" => {
            LIST_PAGE.with(|p| p.set(p.get() + 1));
            log::info!("fonts page: list page {}", LIST_PAGE.with(Cell::get) + 1);
            Some(String::new())
        }
        "font-reading" => {
            let name = arg.to_string();
            edit(cx, &|s| s.reader.font_family = name.clone());
            log::info!("fonts page: reading font '{name}' (books without their own font)");
            Some(format!("Reading font: {name}. Books with their own font in Aa keep it."))
        }
        "font-ui" => {
            let name = if arg.eq_ignore_ascii_case(fonts::UI_BUILTIN) { String::new() } else { arg.to_string() };
            edit(cx, &|s| s.ui.font_family = name.clone());
            log::info!("fonts page: interface font '{arg}'");
            Some(format!("Interface font: {arg} (loading…)"))
        }
        _ => None,
    }
}

/// Adds the page's widgets for the content area `area`.
pub fn build(tree: &mut WidgetTree, cx: &AppCtx, area: Rect) {
    let s = current(cx);
    let tab = TAB.with(Cell::get);
    let x = area.x + space(4);
    let w = area.w.saturating_sub(2 * space(4) as u32);
    let mut seg = Segmented::new(&["Reading font", "Interface font"], "fonts-tab");
    seg.selected = tab;
    let mut y = area.y + space(2);
    tree.add(None, Box::new(seg), Rect::new(x, y, w, 104));
    y += 104 + space(3);
    let selected = if tab == 0 { reading_family(s) } else { ui_family(s) };
    tree.add(None, Box::new(Sample { family: selected.clone() }), Rect::new(x, y, w, SAMPLE_H));
    y += (SAMPLE_H as i32) + space(3);
    let h = (area.bottom() - y).max(ROW_H as i32) as u32;
    // The interface list: families with a bold and an italic face, so dictionary
    // entries show their emphasis in it.
    let names = if tab == 0 { fonts::picker_names() } else { fonts::ui_picker_names(&s.ui.font_family) };
    let prefix = if tab == 0 { "font-reading" } else { "font-ui" };
    tree.add(None, Box::new(FamilyList { names, selected, prefix, page: LIST_PAGE.with(Cell::get) }), Rect::new(x, y, w, h));
}

/// The sample sentence in the selected family, with the family name above it.
struct Sample {
    family: String,
}

impl Widget for Sample {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let fg = cx.theme.fg;
        stroke_round_rect(cx.canvas, b, cx.theme.radius_lg, cx.theme.border_px as f32, fg);
        let inner = Rect::new(b.x + 32, b.y + 16, b.w.saturating_sub(64), b.h.saturating_sub(32));
        let cap = Rect::new(inner.x, inner.y, inner.w, 44);
        cx.text_left(cap, &self.family, cx.theme.small_px, Style::Bold, cx.theme.muted);
        // Greedy wrap of the sample at the body size.
        let px = 48.0;
        let line_h = (px * 1.35) as u32;
        let space_w = fonts::family_text_width(cx.fonts, &self.family, px, " ").max(px * 0.25);
        let mut y = cap.bottom() + 8;
        let mut x = inner.x as f32;
        for word in SAMPLE.split_whitespace() {
            let ww = fonts::family_text_width(cx.fonts, &self.family, px, word);
            if x > inner.x as f32 && x + ww > inner.right() as f32 {
                x = inner.x as f32;
                y += line_h as i32;
            }
            if y + line_h as i32 > inner.bottom() + 8 {
                break;
            }
            let row = Rect::new(inner.x, y, inner.w, line_h);
            fonts::draw_family_text(cx, &self.family, px, x, row, word, fg, inner);
            x += ww + space_w;
        }
    }
}

/// The family list: one row per family, its name in its own font, a check on the
/// selected one; the last row turns the page when the list is longer than the area.
struct FamilyList {
    names: Vec<String>,
    selected: String,
    prefix: &'static str,
    page: usize,
}

/// What a list row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Name(usize),
    More(usize, usize),
}

impl FamilyList {
    fn rows(&self, b: Rect) -> Vec<(Rect, Item)> {
        let per = ((b.h / ROW_H) as usize).max(1);
        let paged = self.names.len() > per;
        let per_page = if paged { (per - 1).max(1) } else { per };
        let pages = self.names.len().div_ceil(per_page).max(1);
        let page = self.page % pages;
        let at = |n: usize| Rect::new(b.x, b.y + (n as u32 * ROW_H) as i32, b.w, ROW_H);
        let mut out: Vec<(Rect, Item)> = (page * per_page..((page + 1) * per_page).min(self.names.len())).enumerate().map(|(n, i)| (at(n), Item::Name(i))).collect();
        if paged {
            out.push((at(out.len()), Item::More(page + 1, pages)));
        }
        out
    }
}

impl Widget for FamilyList {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let fg = cx.theme.fg;
        for (r, item) in self.rows(b) {
            match item {
                Item::Name(i) => {
                    let name = &self.names[i];
                    let text_r = Rect::new(r.x + 16, r.y, r.w.saturating_sub(16 + 120), r.h);
                    fonts::draw_family_text(cx, name, 48.0, text_r.x as f32, text_r, name, fg, text_r);
                    if name.eq_ignore_ascii_case(&self.selected) {
                        let c = Rect::new(r.right() - 96, r.y + (r.h as i32 - 64) / 2, 64, 64);
                        cx.icon(Icon::Check, c, 56, fg);
                    }
                }
                Item::More(p, n) => {
                    cx.text_left(Rect::new(r.x + 16, r.y, r.w.saturating_sub(32), r.h), &format!("More fonts ({p}/{n})"), cx.theme.font_px, Style::Bold, fg);
                }
            }
            // Divider: light, rule_px thick (DESIGN.md).
            let rule = cx.theme.rule_px;
            cx.canvas.fill_rect(r.x, r.bottom() - rule, r.w as i32, rule, cx.theme.light);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let UiEvent::Gesture(Gesture::Tap { pos }) = ev else {
            return if matches!(ev, UiEvent::Gesture(_)) { EventResult::Handled } else { EventResult::Ignored };
        };
        for (r, item) in self.rows(b) {
            if r.contains(*pos) {
                return match item {
                    Item::Name(i) => EventResult::Action(format!("{}:{}", self.prefix, self.names[i])),
                    Item::More(..) => EventResult::Action("fonts-more".into()),
                };
            }
        }
        EventResult::Handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_pages_and_names() {
        let list = FamilyList { names: (0..7).map(|i| format!("F{i}")).collect(), selected: "F1".into(), prefix: "font-ui", page: 1 };
        // 4 rows fit: 3 names and a "More" row per page.
        let rows = list.rows(Rect::new(0, 0, 1000, 4 * ROW_H));
        assert_eq!(rows.iter().map(|r| r.1).collect::<Vec<_>>(), vec![Item::Name(3), Item::Name(4), Item::Name(5), Item::More(2, 3)]);
        let s = Settings::default();
        assert_eq!(ui_family(&s), fonts::UI_BUILTIN);
        assert!(summary(&s).contains("UI Noto Sans"));
    }
}
