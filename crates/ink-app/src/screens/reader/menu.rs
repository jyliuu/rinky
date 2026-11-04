//! Reader menu: a tap in the middle of the page shows a top bar and a bottom bar.
//!
//! - Top: a status strip (clock, battery) over an [`AppBar`]: back to the library, the
//!   book title with the chapter under it, icon buttons Contents, Aa, Light, Bookmark,
//!   Search, More. A marked page shows the bookmark icon on a black disc.
//! - Bottom: the [`Scrubber`]: its upper half is one row with "Page 3 of 208 · 12%" on
//!   the left and "12 pages left in chapter" on the right (a "Go back to p. N" button at its
//!   right end after a jump: search result, Contents, Go to, scrubber, link, footnote), its lower half the slider (a page slider with chapter ticks on the track that
//!   shows the chapter and page under the finger while dragging), and under it the dock:
//!   an icon over a label for each plugin entry (when there are any).
//!
//! A tap outside the bars closes the menu. Actions: `back`, `goto`, `aa`, `light`,
//! `bookmark`, `search`, `more`, `page:N` (1-based), `return`, `plugin-menu:I`.
//! [`build`] needs only [`MenuInfo`]; [`build_with`] also takes [`MenuExtra`] (chapter
//! ticks, the return page), which the reader fills with
//! `ReaderScreen::menu_overlay`.

use super::overlay::{Bar, Overlay, OverlayKind, PageScrim, Rule};
use crate::router::{AppCtx, Route, Transition};
use ink_device::{Rect, RefreshClass, Size};
use ink_doc::Position;
use ink_text::Style;
use ink_ui::bars::APP_BAR_H;
use ink_ui::draw::{fill_round_rect, stroke_round_rect, Path};
use ink_ui::theme::space;
use ink_ui::widget::split_cols;
use ink_ui::{AppBar, BarAction, Button, EventResult, Icon, Menu, PaintCx, Slider, TouchKind, UiEvent, Widget};

/// Height of the top bar (the app bar alone: the menu shows no status bar; the page's
/// status bar is under it).
pub const TOP_H: u32 = APP_BAR_H;
/// Height of the bottom bar (without the plugin row).
pub const BOTTOM_H: u32 = 240;
/// Height of the dock of plugin entries (when there are any).
pub const PLUGIN_ROW_H: u32 = 104;
/// Height of the progress row (text and the "Back to" pill): the scrubber's upper half.
const ROW_H: u32 = 104;
/// Height of the scrubber (progress row, controls half).
const SCRUB_H: u32 = 2 * ROW_H;

/// What the bars show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuInfo {
    /// Book title.
    pub title: String,
    /// 0-based page.
    pub page: usize,
    /// Page count; `complete` is false while pagination runs.
    pub pages: usize,
    /// Pagination finished.
    pub complete: bool,
    /// Estimated page count while pagination runs; shown as "~N" instead of `pages`.
    pub estimate: Option<usize>,
    /// Progress 0..=1.
    pub percent: f32,
    /// Title of the chapter the page is in.
    pub chapter: Option<String>,
    /// The page has a bookmark.
    pub bookmarked: bool,
}

/// The page total as text: `208` when pagination is complete, `~8370` while the total is
/// an estimate, `40+` while pagination runs and there is no estimate.
pub fn total_text(pages: usize, complete: bool, estimate: Option<usize>) -> String {
    match (complete, estimate.filter(|&e| e > pages)) {
        (true, _) => pages.to_string(),
        (false, Some(e)) => format!("~{e}"),
        (false, None) => format!("{pages}+"),
    }
}

impl MenuInfo {
    /// `Page 3 of 208 · 12%` (`of ~8370` while pagination runs).
    pub fn progress_text(&self) -> String {
        format!("Page {} of {} · {}", self.page + 1, total_text(self.pages, self.complete, self.estimate), super::footer::percent_text(self.percent))
    }
}

/// Start of a chapter, for the scrubber's ticks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChapterMark {
    /// Chapter title.
    pub title: String,
    /// 0-based first page.
    pub page: usize,
}

/// What the bars show beyond [`MenuInfo`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuExtra {
    /// Chapter starts (top TOC level), sorted by page.
    pub chapters: Vec<ChapterMark>,
    /// 0-based page the last jump started from ("Go back to p. N"): a search result,
    /// Contents, Go to, scrubber, bookmark, link or footnote; when it differs from the page.
    pub return_to: Option<usize>,
}

/// Name, action and icon of the top bar's buttons, left to right.
pub const TOOLS: [(&str, &str, Icon); 6] = [
    ("Contents", "goto", Icon::Toc),
    ("Aa", "aa", Icon::Font),
    ("Light", "light", Icon::Light),
    ("Bookmark", "bookmark", Icon::Bookmark),
    ("Search", "search", Icon::Search),
    ("More", "more", Icon::More),
];

/// Entries of the More menu (label, action).
pub const MORE: [(&str, &str); 5] = [
    ("Go to page or chapter", "goto"),
    ("Settings", "settings"),
    ("Reading statistics", "stats"),
    ("Vocabulary", "vocab"),
    ("Close book", "back"),
];

/// Builds the menu overlay for a screen of `size` without chapter ticks or return page.
pub fn build(info: &MenuInfo, plugins: &[String], size: Size) -> Overlay {
    build_with(info, &MenuExtra::default(), plugins, size)
}

/// Builds the menu overlay for a screen of `size`; `plugins` are the labels of plugin
/// menu entries (a row at the top of the bottom bar, at most [`PLUGIN_PILLS`] pills, the
/// last one `plugin-more` when there are more; actions `plugin-menu:I`).
pub fn build_with(info: &MenuInfo, extra: &MenuExtra, plugins: &[String], size: Size) -> Overlay {
    let mut o = Overlay::new(OverlayKind::Menu);
    let w = size.w;
    o.tree.add(None, Box::new(PageScrim { action: "close".into() }), Rect::from_size(size));
    // Top: the app bar, over the page's status bar.
    let top = o.tree.add(None, Box::new(Bar::new(Rule::None)), Rect::new(0, 0, w, TOP_H));
    let mut bar = AppBar::new(info.title.clone()).with_back();
    if let Some(c) = info.chapter.as_ref().filter(|c| !c.is_empty()) {
        bar = bar.with_subtitle(c.clone());
    }
    for (_, action, icon) in TOOLS {
        bar = bar.action(BarAction::icon(icon, action));
    }
    let bar_r = Rect::new(0, 0, w, APP_BAR_H);
    o.buttons.push(("back".into(), AppBar::back_rect(bar_r)));
    let rects = bar.action_rects(bar_r);
    for ((_, action, _), r) in TOOLS.iter().zip(&rects) {
        if r.w > 0 {
            o.buttons.push((action.to_string(), *r));
        }
    }
    let bar_id = o.tree.add(Some(top), Box::new(bar), bar_r);
    if info.bookmarked {
        if let Some(r) = TOOLS.iter().position(|t| t.1 == "bookmark").map(|i| rects[i]).filter(|r| r.w > 0) {
            o.ids.insert("marked", o.tree.add(Some(bar_id), Box::new(Marked), r));
        }
    }
    // Bottom: progress row, scrubber, then the dock of plugin entries.
    let extra_h = if plugins.is_empty() { 0 } else { PLUGIN_ROW_H };
    let bot_r = Rect::new(0, (size.h - BOTTOM_H - extra_h) as i32, w, BOTTOM_H + extra_h);
    let bot = o.tree.add(None, Box::new(Bar::new(Rule::Top)), bot_r);
    let row_y = bot_r.y + space(2);
    let pill_w = 400u32;
    let mut scrub = Scrubber::new(info, extra.chapters.clone());
    scrub.progress = info.progress_text();
    if extra.return_to.is_some() {
        scrub.reserve = pill_w + 16;
    }
    let scrub_r = Rect::new(space(3), row_y, w.saturating_sub(48), SCRUB_H);
    let slider = o.tree.add(Some(bot), Box::new(scrub), scrub_r);
    o.ids.insert("slider", slider);
    if let Some(p) = extra.return_to {
        // A child of the scrubber: it repaints with it (the drag pill clears the row).
        let r = Rect::new((w - pill_w) as i32 - space(3), row_y, pill_w, ROW_H);
        o.button(Some(slider), Button::new(format!("Go back to p. {}", p + 1), "return").with_icon(Icon::Back), r);
    }
    if !plugins.is_empty() {
        // At most PLUGIN_PILLS entries; with more, the last one opens the rest
        // (`plugin-more`, [`build_plugin_more`]), so no entry is dropped.
        let mut pills: Vec<(String, String)> = plugins.iter().enumerate().map(|(i, l)| (l.clone(), format!("plugin-menu:{i}"))).collect();
        if pills.len() > PLUGIN_PILLS {
            pills.truncate(PLUGIN_PILLS - 1);
            pills.push((PLUGIN_MORE_LABEL.to_string(), PLUGIN_MORE.to_string()));
        }
        let row = Rect::new(space(3), scrub_r.bottom() + space(1), w.saturating_sub(48), PLUGIN_ROW_H - space(1) as u32);
        for (r, (label, action)) in split_cols(row, pills.len() as u32, 8).into_iter().zip(pills) {
            o.buttons.push((action.clone(), r));
            o.tree.add(Some(bot), Box::new(PluginPill { label, action, pressed: false, changed: false }), r);
        }
    }
    o
}

/// The icon of a dock entry, picked from its label.
pub fn dock_icon(label: &str) -> Icon {
    let l = label.to_lowercase();
    let has = |ks: &[&str]| ks.iter().any(|k| l.contains(k));
    if label == PLUGIN_MORE_LABEL {
        Icon::More
    } else if has(&["dict", "look", "define"]) {
        Icon::Book
    } else if has(&["wiki", "translat"]) {
        Icon::Globe
    } else if has(&["insightful", "ai"]) {
        Icon::Sparkle
    } else if has(&["sync"]) {
        Icon::Sync
    } else if has(&["calibre", "librar", "opds"]) {
        Icon::Library
    } else if has(&["ssh", "terminal"]) {
        Icon::Terminal
    } else if has(&["usb"]) {
        Icon::Usb
    } else {
        Icon::Star
    }
}

/// Label size of a dock entry.
const DOCK_PX: f32 = 28.0;

/// The label of a dock entry `room` px wide: whole, or ellipsized.
pub fn dock_caption(cx: &mut PaintCx, label: &str, room: f32) -> String {
    if cx.text_width(Style::Regular, DOCK_PX, label) <= room {
        label.to_string()
    } else {
        cx.ellipsize(label, DOCK_PX, Style::Regular, room)
    }
}

/// A plugin entry of the menu's dock: an icon over a short label, no outline. Reports
/// `action` on a tap; pressed (a finger rests on it, from [`TouchKind::Press`] on):
/// inverted (DU).
pub struct PluginPill {
    /// The plugin's label.
    pub label: String,
    /// Reported on a tap (`plugin-menu:I`).
    pub action: String,
    /// A finger rests on it.
    pub pressed: bool,
    /// The last event changed the look (pressed state).
    changed: bool,
}

impl Widget for PluginPill {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme;
        let (ink, back) = if self.pressed { (t.bg, t.fg) } else { (t.fg, t.bg) };
        fill_round_rect(cx.canvas, b, 16.0, back);
        let icon_r = Rect::new(b.x, b.y + 4, b.w, 48);
        cx.icon(dock_icon(&self.label), icon_r, 44, ink);
        let text = dock_caption(cx, &self.label, b.w as f32 - 16.0);
        let label_r = Rect::new(b.x, b.y + 52, b.w, b.h.saturating_sub(56));
        cx.text_centered(label_r, &text, DOCK_PX, Style::Regular, ink);
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match ev {
            UiEvent::Touch(t) => {
                // No feedback on touch-down: the touch may still become a swipe.
                let now = t.press_on(self.pressed, b.contains(t.pos));
                if now != self.pressed {
                    self.pressed = now;
                    self.changed = true;
                    EventResult::Handled
                } else if t.kind == TouchKind::Down {
                    // Capture the contact (for its Press) without a repaint.
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            UiEvent::Gesture(ink_ui::Gesture::Tap { .. }) => {
                self.changed |= std::mem::take(&mut self.pressed);
                EventResult::Action(self.action.clone())
            }
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        if self.pressed {
            RefreshClass::Du
        } else {
            RefreshClass::Gl16
        }
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        // An event that changed nothing (a touch-down, a quick tap) refreshes nothing.
        if std::mem::take(&mut self.changed) {
            None
        } else {
            Some(Vec::new())
        }
    }
}

/// The More menu: a [`Menu`] under the More button with [`MORE`]'s entries.
pub fn build_more(size: Size) -> Overlay {
    let mut o = Overlay::new(OverlayKind::Menu);
    o.tree.add(None, Box::new(PageScrim { action: "close".into() }), Rect::from_size(size));
    let menu = Menu::new(MORE.iter().map(|(l, a)| (l.to_string(), a.to_string())).collect());
    let mw = 600u32.min(size.w.saturating_sub(48));
    let r = Rect::new(size.w as i32 - mw as i32 - space(3), space(3), mw, menu.height());
    for (i, (_, a)) in MORE.iter().enumerate() {
        o.buttons.push((a.to_string(), Rect::new(r.x, r.y + (i as u32 * menu.row_h) as i32, r.w, menu.row_h)));
    }
    o.ids.insert("more", o.tree.add(None, Box::new(menu), r));
    o
}

/// Most plugin pills in the menu's bottom bar.
pub const PLUGIN_PILLS: usize = 4;
/// Action of the last pill when there are more plugin entries than [`PLUGIN_PILLS`].
pub const PLUGIN_MORE: &str = "plugin-more";
/// Caption of that pill.
const PLUGIN_MORE_LABEL: &str = "More";

/// The plugin entries without a pill (`plugins[PLUGIN_PILLS - 1..]`) as a [`Menu`]
/// standing on the plugin row, right-aligned; actions `plugin-menu:I` with I the index in
/// `plugins`. `plugins` are all the entries, as given to [`build_with`].
pub fn build_plugin_more(plugins: &[String], size: Size) -> Overlay {
    let mut o = Overlay::new(OverlayKind::Menu);
    o.tree.add(None, Box::new(PageScrim { action: "close".into() }), Rect::from_size(size));
    let first = (PLUGIN_PILLS - 1).min(plugins.len());
    let items: Vec<(String, String)> = plugins.iter().enumerate().skip(first).map(|(i, l)| (l.clone(), format!("plugin-menu:{i}"))).collect();
    let menu = Menu::new(items.clone());
    let mw = 600u32.min(size.w.saturating_sub(48));
    let bottom = size.h.saturating_sub(BOTTOM_H + PLUGIN_ROW_H) as i32;
    let r = Rect::new(size.w as i32 - mw as i32 - space(3), (bottom - menu.height() as i32 - space(1)).max(space(3)), mw, menu.height());
    for (i, (_, a)) in items.iter().enumerate() {
        o.buttons.push((a.clone(), Rect::new(r.x, r.y + (i as u32 * menu.row_h) as i32, r.w, menu.row_h)));
    }
    o.ids.insert("plugin-more", o.tree.add(None, Box::new(menu), r));
    o
}

/// The marked bookmark button: the icon on a black disc (drawn over the app bar's
/// button; taps go through to the bar).
struct Marked;

impl Widget for Marked {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let (x, y) = ink_ui::draw::center(b);
        let d = b.w.min(b.h);
        Path::new().circle(x, y, d as f32 / 2.0).fill(cx.canvas, cx.theme.fg);
        let bg = cx.theme.bg;
        cx.icon(Icon::Bookmark, b, d / 2, bg);
    }

    fn opaque(&self) -> bool {
        false
    }
}

/// Size of the progress row's page text.
const PROGRESS_PX: f32 = 36.0;

/// A page slider with chapter ticks. Geometry and events are [`Slider`]'s (minus button,
/// track, plus button in the lower half; `page:N` on release or on a step). The upper
/// half is the progress row: [`Scrubber::progress`] (bold) on the left and the pages left
/// in the chapter (muted caption) on the right, on one line; the right text is cut with
/// "…" when both do not fit. While the finger is down a black pill with the chapter and
/// page under the finger replaces the row (DU, pure black and white).
pub struct Scrubber {
    slider: Slider,
    chapters: Vec<ChapterMark>,
    pages: usize,
    complete: bool,
    estimate: Option<usize>,
    page: usize,
    dragging: bool,
    /// `Page 3 of 208 · 12%`, the left side of the progress row.
    pub progress: String,
    /// Pixels kept free at the right end of the progress row (the "Back to" pill).
    pub reserve: u32,
}

impl Scrubber {
    /// A scrubber at `info.page`; `chapters` sorted by page.
    pub fn new(info: &MenuInfo, chapters: Vec<ChapterMark>) -> Self {
        let max = info.pages.max(1) as f32;
        let slider = Slider::new("", 1.0, max, 1.0, (info.page + 1) as f32, "page");
        Scrubber { slider, chapters, pages: info.pages.max(1), complete: info.complete, estimate: info.estimate, page: info.page, dragging: false, progress: String::new(), reserve: 0 }
    }

    /// 0-based page under the knob.
    pub fn value_page(&self) -> usize {
        (self.slider.value.round() as usize).saturating_sub(1)
    }

    /// Index of the chapter `page` is in.
    pub fn chapter_index(&self, page: usize) -> Option<usize> {
        self.chapters.iter().rposition(|c| c.page <= page)
    }

    /// `Chapter 7 · p. 120 of 208` for the page under the finger.
    pub fn bubble_text(&self) -> String {
        let p = self.value_page();
        let page = format!("p. {} of {}", p + 1, total_text(self.pages, self.complete, self.estimate));
        match self.chapter_index(p).map(|i| self.chapters[i].title.as_str()).filter(|t| !t.is_empty()) {
            Some(t) => format!("{t} · {page}"),
            None => page,
        }
    }

    /// `12 pages left in chapter` (`Last page of chapter`), when the chapters are known.
    pub fn left_text(&self) -> Option<String> {
        let i = self.chapter_index(self.page)?;
        let end = self.chapters.get(i + 1).map_or(self.pages, |c| c.page);
        match end.saturating_sub(self.page + 1) {
            0 => Some("Last page of chapter".into()),
            1 => Some("1 page left in chapter".into()),
            n => Some(format!("{n} pages left in chapter")),
        }
    }

    fn fraction(&self, page: usize) -> f32 {
        if self.pages > 1 {
            (page as f32 / (self.pages - 1) as f32).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    fn hit_track(b: Rect) -> Rect {
        let (_, track, _) = Slider::parts(b);
        Rect::new(track.x - 30, track.y, track.w + 60, track.h)
    }
}

impl Widget for Scrubber {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.clone();
        let (fg, bg) = (t.fg, t.bg);
        let (minus, track, plus) = Slider::parts(b);
        let ty = track.y as f32 + track.h as f32 / 2.0;
        let kx = track.x as f32 + self.fraction(self.value_page()) * track.w as f32;
        // Caption half.
        let top = Rect::new(b.x, b.y, b.w, b.h / 2);
        if self.dragging {
            let text = cx.ellipsize(&self.bubble_text(), t.label.px, Style::Bold, (b.w as f32 - 96.0).max(0.0));
            let pw = (cx.text_width(Style::Bold, t.label.px, &text) as u32 + 2 * space(3) as u32).min(b.w);
            let ph = 80u32.min(top.h);
            let x = (kx as i32 - pw as i32 / 2).clamp(b.x, b.right() - pw as i32);
            let pill = Rect::new(x, top.y + (top.h as i32 - ph as i32) / 2, pw, ph);
            fill_round_rect(cx.canvas, pill, ph as f32 / 2.0, fg);
            cx.text_centered(pill, &text, t.label.px, Style::Bold, bg);
        } else {
            let inner = Rect::new(top.x + 8, top.y, top.w.saturating_sub(16 + self.reserve), top.h);
            cx.text_left(inner, &self.progress, PROGRESS_PX, Style::Bold, fg);
            if let Some(left) = self.left_text() {
                let used = cx.text_width(Style::Bold, PROGRESS_PX, &self.progress) + 32.0;
                let room = (inner.w as f32 - used).max(0.0);
                let text = cx.ellipsize(&left, t.caption.px, t.caption.style, room);
                let tw = cx.text_width(t.caption.style, t.caption.px, &text).ceil() as u32;
                let r = Rect::new(inner.right() - tw as i32, inner.y, tw, inner.h);
                cx.text_left(r, &text, t.caption.px, t.caption.style, t.muted);
            }
        }
        // Page step buttons.
        for (r, icon) in [(minus, Icon::Back), (plus, Icon::Forward)] {
            stroke_round_rect(cx.canvas, r, r.w as f32 / 2.0, t.border_px as f32, fg);
            cx.icon(icon, r, r.w / 2, fg);
        }
        // Track, fill, chapter ticks (white notches in the fill, black after it), knob.
        let th = 12u32;
        let tr = Rect::new(track.x, (ty - th as f32 / 2.0) as i32, track.w, th);
        fill_round_rect(cx.canvas, tr, th as f32 / 2.0, t.track());
        fill_round_rect(cx.canvas, Rect::new(tr.x, tr.y, (kx - track.x as f32).max(0.0) as u32, th), th as f32 / 2.0, fg);
        let mut last_x = i32::MIN;
        for c in &self.chapters {
            let x = track.x + (self.fraction(c.page) * track.w as f32) as i32;
            if c.page == 0 || x.saturating_sub(last_x) < 12 {
                continue;
            }
            last_x = x;
            let col = if (x as f32) < kx - 2.0 { bg } else { fg };
            cx.canvas.fill_rect(x - 2, (ty - 16.0) as i32, 4, 32, col);
        }
        let kr = (track.h as f32 * 0.28).min(26.0);
        Path::new().circle(kx, ty, kr).fill(cx.canvas, fg);
        Path::new().circle(kx, ty, kr - 5.0).fill(cx.canvas, if self.dragging { fg } else { bg });
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let r = self.slider.event(ev, b);
        if let UiEvent::Touch(t) = ev {
            match t.kind {
                TouchKind::Down if r != EventResult::Ignored && Scrubber::hit_track(b).contains(t.pos) => self.dragging = true,
                TouchKind::Up | TouchKind::Cancel => self.dragging = false,
                _ => {}
            }
        }
        r
    }

    fn refresh_class(&self) -> RefreshClass {
        if self.dragging {
            RefreshClass::Du
        } else {
            RefreshClass::Gl16
        }
    }
}

impl super::ReaderScreen {
    /// Chapter starts and the return page for the menu.
    pub(super) fn menu_extra(&mut self) -> MenuExtra {
        let page = self.page;
        let toc = self.toc().to_vec();
        let Some(doc) = self.doc.as_deref() else { return MenuExtra::default() };
        let top = toc.iter().map(|e| e.level).min().unwrap_or(0);
        let mut chapters: Vec<ChapterMark> = toc
            .iter()
            .filter(|e| e.level == top)
            .filter_map(|e| Some(ChapterMark { title: e.title.clone(), page: e.page.or_else(|| doc.page_of_known(&e.position))? }))
            .collect();
        chapters.sort_by_key(|c| c.page);
        // "Go back" after any jump (search, Contents, Go to, scrubber, link, footnote).
        let return_to = self.return_page().filter(|&p| p != page);
        MenuExtra { chapters, return_to }
    }

    /// The menu with chapter ticks and the return page.
    pub(super) fn menu_overlay(&mut self, plugins: &[String], size: Size) -> Overlay {
        let info = self.menu_info();
        let extra = self.menu_extra();
        build_with(&info, &extra, plugins, size)
    }

    /// The menu's and Go to panel's own actions. `Some` when handled here; `None` hands
    /// the action on to `ReaderScreen::action`.
    pub(super) fn menu_action(&mut self, a: &str, cx: &mut AppCtx) -> anyhow::Result<Option<Transition>> {
        let size = cx.screen.size();
        match a {
            "light" => {
                self.close_overlay();
                return Ok(Some(crate::screens::settings::quick_light::open_control_center(cx)));
            }
            "settings" | "stats" | "vocab" => {
                self.close_overlay();
                let r = match a {
                    "settings" => Route::Settings,
                    "stats" => Route::Statistics,
                    _ => Route::Vocabulary,
                };
                return Ok(Some(Transition::Push(r)));
            }
            "more" => self.open_overlay(build_more(size), cx),
            PLUGIN_MORE => {
                let labels: Vec<String> = super::plugin_menu(cx.plugins).into_iter().map(|m| m.label).collect();
                self.open_overlay(build_plugin_more(&labels, size), cx);
            }
            "return" => {
                self.close_overlay();
                self.go_back();
            }
            "goto" => {
                let o = self.goto_overlay(0, size);
                self.open_overlay(o, cx);
            }
            _ if a.starts_with("gtab:") => {
                let o = self.goto_overlay(a[5..].parse().unwrap_or(0), size);
                self.open_overlay(o, cx);
            }
            "export" => {
                let text = self.export_highlights(cx.paths);
                crate::export::toast(cx.plugins, text);
            }
            _ if a.starts_with("hl:") => {
                let pos = a[3..].parse::<usize>().ok().and_then(|i| self.highlight_targets().into_iter().nth(i));
                if let Some(pos) = pos {
                    self.close_overlay();
                    self.go_to_position(&pos);
                }
            }
            _ => return Ok(None),
        }
        Ok(Some(Transition::None))
    }

    /// Writes this book's highlights, notes and bookmarks to `exports/<title>.md` and
    /// returns the toast text. The open document gives the page numbers and percents.
    fn export_highlights(&mut self, paths: &ink_store::Paths) -> String {
        let (title, authors, total) = match self.doc.as_deref() {
            Some(d) => (d.metadata().title.clone(), d.metadata().authors.clone(), d.page_count().pages as u32),
            None => (String::new(), Vec::new(), 0),
        };
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let entry = ink_store::BookEntry { path: self.path.clone(), title: if title.is_empty() { stem } else { title }, authors, ..Default::default() };
        let doc = self.doc.as_deref();
        let page_of = |s: &str| -> Option<u32> {
            let pos = s.parse::<Position>().ok()?;
            doc?.page_of(&pos).ok().map(|p| p as u32)
        };
        crate::export::export_one(paths, &entry, &ink_store::export::Locator { page_of: &page_of, total: (total > 0).then_some(total) })
    }

    /// Start positions of the saved highlights in the Go to panel's order.
    pub(super) fn highlight_targets(&mut self) -> Vec<Position> {
        self.highlight_rows().into_iter().map(|(_, p)| p).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> MenuInfo {
        MenuInfo { title: "Romeo and Juliet".into(), page: 40, pages: 208, complete: true, percent: 0.19, chapter: Some("Act II".into()), ..Default::default() }
    }

    fn chapters() -> Vec<ChapterMark> {
        [(0, "Prologue"), (10, "Act I"), (38, "Act II"), (90, "Act III")].iter().map(|(p, t)| ChapterMark { title: t.to_string(), page: *p }).collect()
    }

    #[test]
    fn dock_captions_stay_inside_the_button_and_icons_follow_labels() {
        let theme = ink_ui::Theme::day();
        let mut fonts = ink_text::Fonts::load();
        let mut canvas = ink_device::Canvas::new(1272, 200);
        let mut cx = PaintCx::new(&mut canvas, &mut fonts, &theme);
        for label in ["Dictionary", "Sync reading progress", "A very long plugin entry label that cannot fit"] {
            let text = dock_caption(&mut cx, label, 300.0);
            assert!(cx.text_width(Style::Regular, DOCK_PX, &text) <= 300.0, "{label}: {text:?}");
        }
        assert_eq!(dock_caption(&mut cx, "Dictionary", 300.0), "Dictionary");
        assert_eq!(dock_icon("Insightful"), Icon::Sparkle);
        assert_eq!(dock_icon("Wikipedia"), Icon::Globe);
        assert_eq!(dock_icon("Dictionary"), Icon::Book);
    }

    #[test]
    fn texts() {
        let mut i = MenuInfo { page: 2, pages: 208, complete: true, percent: 0.123, ..Default::default() };
        assert_eq!(i.progress_text(), "Page 3 of 208 · 12%");
        i.complete = false;
        assert_eq!(i.progress_text(), "Page 3 of 208+ · 12%");
        i.estimate = Some(8370);
        assert_eq!(i.progress_text(), "Page 3 of ~8370 · 12%");
        i.complete = true;
        assert_eq!(i.progress_text(), "Page 3 of 208 · 12%", "an exact count wins over a stale estimate");
    }

    #[test]
    fn scrubber_bubble_shows_the_estimate_while_counting() {
        let i = MenuInfo { page: 2, pages: 208, complete: false, estimate: Some(900), ..Default::default() };
        let s = Scrubber::new(&i, Vec::new());
        assert_eq!(s.bubble_text(), "p. 3 of ~900");
    }

    #[test]
    fn layout_has_every_tool_and_the_slider() {
        let o = build(&MenuInfo { pages: 10, ..Default::default() }, &["Ask AI".to_string()], Size::new(1272, 1696));
        for (_, a, _) in TOOLS {
            let r = o.button_rect(a).unwrap_or_else(|| panic!("{a}"));
            assert!(r.w >= 96 && r.h >= 96, "{a} {r:?}");
            assert!(r.y >= 0 && r.bottom() <= TOP_H as i32, "{a} {r:?}");
        }
        let back = o.button_rect("back").unwrap();
        assert!(back.x < 100 && back.w >= 96);
        assert!(o.ids.contains_key("slider"));
        assert!(o.button_rect("plugin-menu:0").unwrap().y >= (1696 - BOTTOM_H - PLUGIN_ROW_H) as i32);
        assert!(o.button_rect("return").is_none());
        assert!(!o.ids.contains_key("marked"));
    }

    #[test]
    fn plugin_entries_beyond_the_pills_stay_reachable() {
        let size = Size::new(1272, 1696);
        let labels: Vec<String> = ["Dictionary", "Wikipedia", "Insightful", "Sync progress", "Connect to calibre"].iter().map(|s| s.to_string()).collect();
        // Four entries: four pills, no More.
        let o = build(&MenuInfo { pages: 10, ..Default::default() }, &labels[..4], size);
        assert!(o.button_rect("plugin-menu:3").is_some() && o.button_rect(PLUGIN_MORE).is_none());
        // Five: three entries and More; the More menu lists the other two with their indices.
        let o = build(&MenuInfo { pages: 10, ..Default::default() }, &labels, size);
        assert!(o.button_rect("plugin-menu:2").is_some() && o.button_rect("plugin-menu:3").is_none());
        let more = o.button_rect(PLUGIN_MORE).expect("More entry");
        assert!(more.w >= 96 && more.h >= 96);
        let m = build_plugin_more(&labels, size);
        for a in ["plugin-menu:3", "plugin-menu:4"] {
            let r = m.button_rect(a).unwrap_or_else(|| panic!("{a}"));
            assert!(r.h >= 96 && r.bottom() <= more.y, "{a} {r:?} above the pill row");
        }
        assert!(m.button_rect("plugin-menu:2").is_none());
    }

    #[test]
    fn return_pill_and_marked_bookmark() {
        let mut i = info();
        i.bookmarked = true;
        let extra = MenuExtra { chapters: chapters(), return_to: Some(11) };
        let o = build_with(&i, &extra, &[], Size::new(1272, 1696));
        let r = o.button_rect("return").unwrap();
        assert!(r.h >= 96 && r.y >= (1696 - BOTTOM_H) as i32);
        // The quiet spot the reader tests touch stays inert.
        assert!(!r.contains(ink_device::Point::new(600, 1400)));
        assert!(o.ids.contains_key("marked"));
    }

    #[test]
    fn scrubber_texts_follow_the_chapters() {
        let mut s = Scrubber::new(&info(), chapters());
        assert_eq!(s.chapter_index(40), Some(2));
        assert_eq!(s.left_text().as_deref(), Some("49 pages left in chapter"));
        assert_eq!(s.bubble_text(), "Act II · p. 41 of 208");
        s.slider.value = 100.0;
        assert_eq!(s.bubble_text(), "Act III · p. 100 of 208");
        let none = Scrubber::new(&info(), Vec::new());
        assert_eq!(none.left_text(), None);
        assert_eq!(none.bubble_text(), "p. 41 of 208");
    }

    #[test]
    fn more_menu_rows_are_touch_sized() {
        let o = build_more(Size::new(1272, 1696));
        for (_, a) in MORE {
            let r = o.button_rect(a).unwrap();
            assert!(r.h >= 96 && r.right() <= 1272);
        }
    }

}

/// The menu and Go to panel as the reader shows them once `menu_action` is wired in:
/// actions go to `menu_action` first, then to
/// `ReaderScreen::action`. With `RINKY_OUT=DIR` the tests write PNGs to `DIR/<test>/`.
#[cfg(test)]
mod wired {
    use super::super::overlay::OverlayKind;
    use super::super::tests::Harness;
    use super::*;
    use crate::app::providers;
    use ink_device::{InputEvent, Timestamp, TouchContact, TouchFrame, TouchPhase};

    /// Runs action `a` as the wired reader does; returns the transition.
    fn act(h: &mut Harness, a: &str) -> Transition {
        let p = providers();
        let mut cx = AppCtx {
            canvas: &mut h.canvas,
            screen: h.dev.screen.as_mut(),
            fonts: &mut h.fonts,
            theme: &h.theme,
            refresh: &mut h.refresh,
            plugins: &mut h.plugins,
            settings: &h.settings,
            paths: &h.paths,
            providers: &p,
            services: crate::router::Services {
                info: &h.dev.info,
                light: h.dev.light.as_mut(),
                battery: h.dev.battery.as_mut(),
                power: h.dev.power.as_mut(),
                wifi: h.dev.wifi.as_mut(),
                usb: h.dev.usb.as_mut(),
                clock: h.dev.clock.as_mut(),
            },
            settings_update: &mut h.settings_update,
        };
        h.reader.actions.push(a.to_string());
        let t = match h.reader.menu_action(a, &mut cx).unwrap() {
            Some(t) => t,
            None => h.reader.action(a, &mut cx).unwrap(),
        };
        h.paint();
        t
    }

    fn open_menu(h: &mut Harness) {
        let o = h.reader.menu_overlay(&[], ink_device::Size::new(1272, 1696));
        h.reader.overlay = Some(o);
        h.reader.dirty = Some(RefreshClass::Gl16);
        h.paint();
    }

    fn touch(h: &mut Harness, x: i32, y: i32, phase: TouchPhase) {
        h.clock_ms += 120;
        let contacts = vec![TouchContact { slot: 0, x, y, phase }];
        h.feed(InputEvent::Touch(TouchFrame { time: Timestamp::from_millis(h.clock_ms), contacts }));
    }

    #[test]
    fn scrub_jump_then_back_to_where_you_were() {
        let mut h = Harness::new("wired-menu", "juliet.epub");
        h.reader.go_to_page(40);
        h.paint();
        open_menu(&mut h);
        h.shot("05-reader-menu");
        let extra = h.reader.menu_extra();
        assert!(extra.chapters.len() > 2, "{extra:?}");
        assert_eq!(extra.return_to, None);
        // Drag on the track: the pill shows chapter and page; the menu stays until the lift.
        let o = h.reader.overlay().unwrap();
        let b = o.tree.bounds(o.ids["slider"]).unwrap();
        let (_, track, _) = Slider::parts(b);
        let y = track.y + track.h as i32 / 2;
        touch(&mut h, track.x + track.w as i32 / 3, y, TouchPhase::Down);
        touch(&mut h, track.x + track.w as i32 * 2 / 3, y, TouchPhase::Move);
        h.shot("05b-reader-scrub");
        assert_eq!(h.kind(), Some(OverlayKind::Menu));
        let target = {
            let o = h.reader.overlay.as_mut().unwrap();
            let id = o.ids["slider"];
            o.tree.get_mut::<Scrubber>(id).unwrap().value_page()
        };
        touch(&mut h, track.x + track.w as i32 * 2 / 3, y, TouchPhase::Up);
        let a = h.reader.actions().last().unwrap().clone();
        assert_eq!(a, format!("page:{}", target + 1));
        // The harness runs mod.rs's action; do what the wired reader does for it.
        h.reader.go_to_page(40);
        h.reader.back.clear();
        assert_eq!(act(&mut h, &a), Transition::None);
        assert_eq!(h.reader.page(), target);
        // A scrubber jump has a way back in the menu ("Go back to p. 41"), but it is not
        // a link: the back gesture and the page pill leave it alone.
        open_menu(&mut h);
        assert_eq!(h.reader.menu_extra().return_to, Some(40));
        h.shot("05d-reader-menu-go-back");
        assert!(h.reader.overlay().unwrap().button_rect("return").is_some());
        assert_eq!(h.reader.link_return_page(), None);
        assert!(!h.reader.go_back_from_link(), "the back gesture does not go back from a scrub");
        act(&mut h, "return");
        assert_eq!(h.reader.page(), 40);
        assert!(h.reader.back.is_empty());
        // A plain in-book link (e.g. a contents page) keeps the way back for the Aa
        // sheet and the menu, but shows no pill on the page.
        let target_pos = h.reader.doc.as_deref().unwrap().position_of_page(target).unwrap();
        h.reader.jump(&target_pos, false);
        assert_eq!(h.reader.link_return_page(), Some(40));
        assert!(!h.reader.pill_shown());
        open_menu(&mut h);
        assert_eq!(h.reader.menu_extra().return_to, Some(40));
        act(&mut h, "close");
        h.reader.back.clear();
        // After a footnote jump the pill is there and returns.
        h.reader.go_to_page(40);
        h.reader.jump(&target_pos, true);
        assert!(h.reader.pill_shown());
        open_menu(&mut h);
        assert_eq!(h.reader.menu_extra().return_to, Some(40));
        h.shot("05c-reader-menu-return");
        assert!(h.reader.overlay().unwrap().button_rect("return").is_some());
        act(&mut h, "return");
        assert_eq!(h.reader.page(), 40);
        assert_eq!(h.kind(), None);
        // A page jump after a footnote: the way back goes to the footnote's page first,
        // then to where the footnote was followed. The pill is gone with the jump.
        h.reader.jump(&target_pos, true);
        act(&mut h, "page:42");
        assert!(!h.reader.pill_shown());
        assert_eq!(h.reader.back.len(), 2);
        assert!(h.reader.go_back());
        assert_eq!(h.reader.page(), target);
        assert!(h.reader.go_back());
        assert_eq!(h.reader.page(), 40);
        assert!(!h.reader.go_back(), "nowhere left");
    }

    #[test]
    fn light_more_and_bookmark_mark() {
        let mut h = Harness::new("wired-tools", "juliet.epub");
        open_menu(&mut h);
        assert_eq!(act(&mut h, "light"), Transition::Push(Route::QuickLight));
        assert_eq!(h.kind(), None);
        open_menu(&mut h);
        act(&mut h, "more");
        assert!(h.reader.overlay().unwrap().ids.contains_key("more"));
        h.shot("05d-reader-more");
        assert_eq!(act(&mut h, "stats"), Transition::Push(Route::Statistics));
        open_menu(&mut h);
        act(&mut h, "bookmark");
        open_menu(&mut h);
        assert!(h.reader.overlay().unwrap().ids.contains_key("marked"));
        h.shot("05e-reader-menu-marked");
    }

    #[test]
    fn goto_tabs_with_progress_and_highlights() {
        let mut h = Harness::new("wired-goto", "juliet.epub");
        // A saved highlight on page 31.
        let start = h.reader.doc.as_deref().unwrap().position_of_page(30).unwrap();
        let mut state = ink_store::BookState::load(&h.reader.path).unwrap_or_default();
        state.add_highlight(ink_store::sidecar::Highlight {
            start: start.to_string(),
            end: start.to_string(),
            text: "But, soft! what light through yonder window breaks?".into(),
            style: "lighten".into(),
            chapter: Some("Act II".into()),
            created: "2026-10-30 21:04:11".into(),
            ..Default::default()
        });
        state.save(&h.reader.path).unwrap();
        h.reader.go_to_page(100);
        h.paint();
        open_menu(&mut h);
        act(&mut h, "goto");
        assert_eq!(h.kind(), Some(OverlayKind::Goto));
        h.shot("07-goto-panel");
        {
            let o = h.reader.overlay.as_mut().unwrap();
            let id = o.ids["list"];
            let l = o.tree.get_mut::<ink_ui::List>(id).unwrap();
            assert_eq!(l.items[0].progress, Some(1.0), "{:?}", l.items[0]);
            let cur = l.selected.unwrap();
            let p = l.items[cur].progress.unwrap();
            assert!(p > 0.0 && p <= 1.0, "{p}");
        }
        act(&mut h, "gtab:2");
        h.shot("07c-goto-highlights");
        let o = h.reader.overlay().unwrap();
        assert!(o.ids.contains_key("list"));
        act(&mut h, "hl:0");
        assert_eq!(h.reader.page(), 30);
        // Go back from the highlight to where the Go to panel was opened.
        assert_eq!(h.reader.menu_extra().return_to, Some(100), "a highlight jump has a way back");
        assert_eq!(h.reader.link_return_page(), None, "but it is not a link");
        open_menu(&mut h);
        act(&mut h, "return");
        assert_eq!(h.reader.page(), 100);
    }

    #[test]
    fn export_action_on_the_highlights_tab_writes_markdown_and_toasts() {
        let mut h = Harness::new("wired-export", "juliet.epub");
        // No highlights: the tab has no Export button.
        open_menu(&mut h);
        act(&mut h, "gtab:2");
        assert!(h.reader.overlay().unwrap().button_rect("export").is_none());
        act(&mut h, "close");
        let start = h.reader.doc.as_deref().unwrap().position_of_page(30).unwrap();
        let mut state = ink_store::BookState::load(&h.reader.path).unwrap_or_default();
        state.add_highlight(ink_store::sidecar::Highlight {
            start: start.to_string(),
            end: start.to_string(),
            text: "But, soft! what light through yonder window breaks?".into(),
            note: Some("balcony".into()),
            chapter: Some("Act II".into()),
            created: "2026-10-30 21:04:11".into(),
            ..Default::default()
        });
        state.save(&h.reader.path).unwrap();
        open_menu(&mut h);
        act(&mut h, "gtab:2");
        assert!(h.reader.overlay().unwrap().button_rect("export").is_some());
        let toasts = |h: &Harness| h.plugins.ui.pending() + h.plugins.ui.layer_names().iter().filter(|n| **n == "toast").count();
        let before = toasts(&h);
        act(&mut h, "export");
        assert_eq!(toasts(&h), before + 1, "one toast queued or shown");
        let files: Vec<_> = std::fs::read_dir(h.paths.exports()).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let md = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(md.contains("> But, soft! what light through yonder window breaks?"), "{md}");
        assert!(md.contains("**Note:** balcony") && md.contains("## Act II") && md.contains("p. 31"), "{md}");
    }

    #[test]
    fn night_mode_menu_and_goto() {
        let mut h = Harness::new("wired-night", "juliet.epub");
        h.dev.screen.set_night_mode(true).unwrap();
        h.reader.go_to_page(100);
        h.reader.back.clear();
        h.reader.dirty = Some(RefreshClass::Gc16);
        h.paint();
        h.reader.go_to_page(40);
        let to = h.reader.doc.as_deref().unwrap().position_of_page(100).unwrap();
        h.reader.jump(&to, true);
        open_menu(&mut h);
        h.shot("05n-reader-menu-night");
        // The rule under the app bar is black ink: white on the night panel.
        let p = h.probe.panel();
        assert_eq!(p.get(636, TOP_H as i32 - 2), Some(255));
        act(&mut h, "goto");
        h.shot("07n-goto-panel-night");
    }
}
