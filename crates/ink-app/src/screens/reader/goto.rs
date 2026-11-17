//! "Go to" panel: a full-screen page with an app bar ("Go to", the book and page, a close
//! button), tabs Contents / Bookmarks / Highlights and a percent slider at the bottom.
//!
//! - Contents: the TOC as a tree. An entry with sub-entries has a chevron that folds
//!   them in or out; collapse and expand buttons in the app bar fold one level at a
//!   time. The current chapter (or its folded parent) is marked and selected.
//!
//! The lists have a scroll bar (no page footer): a swipe moves a screenful, a tap or a
//! drag on the bar jumps there.
//! - Bookmarks: chapter or label, creation time, page.
//! - Highlights: the quoted text, chapter and date, page (needs [`GotoExtra`]).
//!
//! Actions: `toc:I` (TOC entry I), `fold:I` (fold entry I in or out), `toc-collapse`,
//! `toc-expand` (one level), `bm:I` (bookmark I), `hl:I` (highlight I), `pct:N`
//! (percent N), `gtab:T` (tab), `close`.

use super::overlay::{Bar, Overlay, OverlayKind, Rule};
use ink_device::{Rect, Size};
use ink_doc::Position;
use ink_text::Style;
use ink_ui::bars::APP_BAR_H;
use ink_ui::theme::space;
use ink_ui::{AppBar, BarAction, Icon, Label, List, ListItem, Slider, Tabs};

/// Tab titles.
pub const TABS: [&str; 3] = ["Contents", "Bookmarks", "Highlights"];
/// Height of a Contents row.
pub const ROW_H: u32 = 132;
/// Height of a Bookmarks or Highlights row.
pub const NOTE_ROW_H: u32 = 150;
const TABS_H: u32 = 104;
const SLIDER_H: u32 = 200;

/// One row of the Contents tab.
#[derive(Clone, Debug, PartialEq)]
pub struct TocRow {
    /// Title.
    pub title: String,
    /// Nesting level (0 = top).
    pub level: u8,
    /// 0-based page for the current layout, if known.
    pub page: Option<usize>,
}

/// One row of the Bookmarks tab.
#[derive(Clone, Debug, PartialEq)]
pub struct BookmarkRow {
    /// Chapter or label.
    pub title: String,
    /// Creation time or excerpt.
    pub subtitle: String,
    /// 0-based page for the current layout, if known.
    pub page: Option<usize>,
}

/// One row of the Highlights tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HighlightRow {
    /// Highlighted text.
    pub text: String,
    /// Chapter and date.
    pub subtitle: String,
    /// 0-based page for the current layout, if known.
    pub page: Option<usize>,
}

/// What the panel shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GotoInfo {
    /// Selected tab (0 = Contents, 1 = Bookmarks, 2 = Highlights).
    pub tab: usize,
    /// TOC rows.
    pub toc: Vec<TocRow>,
    /// Index of the current chapter in `toc`.
    pub current: Option<usize>,
    /// Which TOC entries show their sub-entries, by index (missing entries are open).
    pub open: Vec<bool>,
    /// TOC entry the list pages to (default: the current chapter).
    pub anchor: Option<usize>,
    /// Bookmark rows.
    pub bookmarks: Vec<BookmarkRow>,
    /// Progress 0..=1 of the current page.
    pub percent: f32,
}

/// What the panel shows beyond [`GotoInfo`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GotoExtra {
    /// Book title.
    pub title: String,
    /// 0-based current page.
    pub page: usize,
    /// Page count (0 = unknown: no progress per chapter).
    pub pages: usize,
    /// Estimated page count while pagination runs (shown as [`super::footer::about`]); `None` when the count
    /// is exact.
    pub estimate: Option<usize>,
    /// Highlight rows.
    pub highlights: Vec<HighlightRow>,
}

fn page_text(p: Option<usize>) -> String {
    p.map(|p| format!("p. {}", p + 1)).unwrap_or_default()
}

/// True when TOC entry `i` has sub-entries.
pub fn has_children(toc: &[TocRow], i: usize) -> bool {
    toc.get(i + 1).is_some_and(|n| n.level > toc[i].level)
}

fn is_open(open: &[bool], i: usize) -> bool {
    open.get(i).copied().unwrap_or(true)
}

/// Indices of the TOC entries shown: every entry whose parents are all open.
pub fn visible(toc: &[TocRow], open: &[bool]) -> Vec<usize> {
    let mut shown = Vec::with_capacity(toc.len());
    let mut hide_below: Option<u8> = None;
    for (i, e) in toc.iter().enumerate() {
        match hide_below {
            Some(l) if e.level > l => continue,
            _ => hide_below = None,
        }
        shown.push(i);
        if has_children(toc, i) && !is_open(open, i) {
            hide_below = Some(e.level);
        }
    }
    shown
}

/// `open` with one entry per TOC entry (missing ones open).
fn filled(toc: &[TocRow], open: &[bool]) -> Vec<bool> {
    (0..toc.len()).map(|i| is_open(open, i)).collect()
}

/// Shows one more level: opens every shown entry whose sub-entries are hidden. Returns
/// false when everything is open already.
pub fn expand_level(toc: &[TocRow], open: &mut Vec<bool>) -> bool {
    *open = filled(toc, open);
    let closed: Vec<usize> = visible(toc, open).into_iter().filter(|&i| has_children(toc, i) && !open[i]).collect();
    closed.iter().for_each(|&i| open[i] = true);
    !closed.is_empty()
}

/// Hides one level: closes the deepest open entries that are shown. Returns false when
/// only the top level is shown.
pub fn collapse_level(toc: &[TocRow], open: &mut Vec<bool>) -> bool {
    *open = filled(toc, open);
    let shown: Vec<usize> = visible(toc, open).into_iter().filter(|&i| has_children(toc, i) && open[i]).collect();
    let Some(deepest) = shown.iter().map(|&i| toc[i].level).max() else { return false };
    shown.iter().filter(|&&i| toc[i].level == deepest).for_each(|&i| open[i] = false);
    true
}

/// Opens the parents of entry `i` so it is shown.
pub fn reveal(toc: &[TocRow], open: &mut Vec<bool>, i: usize) {
    *open = filled(toc, open);
    let Some(mut level) = toc.get(i).map(|e| e.level) else { return };
    for j in (0..i).rev() {
        if toc[j].level < level {
            open[j] = true;
            level = toc[j].level;
        }
    }
}

/// Entry `i`, or its innermost shown parent when it is folded away.
fn shown_for(toc: &[TocRow], shown: &[usize], i: usize) -> Option<usize> {
    let mut level = toc.get(i)?.level;
    let mut best = shown.binary_search(&i).ok();
    if best.is_none() {
        for j in (0..i).rev() {
            if toc[j].level < level {
                if let Ok(p) = shown.binary_search(&j) {
                    best = Some(p);
                    break;
                }
                level = toc[j].level;
            }
        }
    }
    best
}

/// Builds the panel for a screen of `size` without highlights or chapter progress.
pub fn build(info: &GotoInfo, size: Size) -> Overlay {
    build_with(info, &GotoExtra::default(), size)
}

/// The app bar's second line: book, page of total (`about N` while the total is an estimate), percent.
fn subtitle(pct: f32, extra: &GotoExtra) -> String {
    let total = match extra.estimate {
        Some(e) => super::footer::about(e),
        None => extra.pages.to_string(),
    };
    match (extra.title.is_empty(), extra.pages) {
        (_, 0) if extra.title.is_empty() => format!("{pct}% read"),
        (true, _) => format!("Page {} of {total} · {pct}%", extra.page + 1),
        (false, 0) => format!("{} · {pct}%", extra.title),
        (false, _) => format!("{} · page {} of {total} · {pct}%", extra.title, extra.page + 1),
    }
}

/// Builds the panel for a screen of `size`.
pub fn build_with(info: &GotoInfo, extra: &GotoExtra, size: Size) -> Overlay {
    let mut o = Overlay::new(OverlayKind::Goto);
    let root = o.tree.add(None, Box::new(Bar::new(Rule::None)), Rect::from_size(size));
    let w = size.w;
    // Rounded down like the footer's percent (`footer::percent_text`).
    let pct = (info.percent.clamp(0.0, 1.0) * 100.0 + 1e-3).floor();
    let sub = subtitle(pct, extra);
    let tab = info.tab.min(TABS.len() - 1);
    let mut bar = AppBar::new("Go to").with_subtitle(sub);
    // A nested Contents list folds one level in or out from the bar, left of close.
    if tab == 0 && info.toc.iter().any(|e| e.level > 0) {
        bar = bar.action(BarAction::icon(Icon::UnfoldLess, "toc-collapse")).action(BarAction::icon(Icon::UnfoldMore, "toc-expand"));
    }
    bar = bar.action(BarAction::icon(Icon::Close, "close"));
    // The Highlights tab can write its book's highlights and notes to a Markdown file.
    if tab == 2 && !extra.highlights.is_empty() {
        bar = bar.action(BarAction::text("Export", "export"));
    }
    o.app_bar(Some(root), bar, Rect::new(0, 0, w, APP_BAR_H));
    let mut tabs = Tabs::new(&TABS, "gtab");
    tabs.selected = tab;
    let tabs_r = Rect::new(0, APP_BAR_H as i32, w, TABS_H);
    for (i, r) in tabs.cells(tabs_r).into_iter().enumerate() {
        o.buttons.push((format!("gtab:{i}"), r));
    }
    o.tree.add(Some(root), Box::new(tabs), tabs_r);
    let list_top = tabs_r.bottom() + space(1);
    let slider_r = Rect::new(space(3), (size.h - SLIDER_H - 16) as i32, w.saturating_sub(48), SLIDER_H);
    let list_r = Rect::new(0, list_top, w, (slider_r.y - space(2) - list_top).max(ROW_H as i32) as u32);
    let mut list = match tab {
        0 => {
            let shown = visible(&info.toc, &info.open);
            // The current chapter, or its parent when it is folded away, is checked.
            let mark = info.current.and_then(|c| shown_for(&info.toc, &shown, c));
            let items = shown
                .iter()
                .enumerate()
                .map(|(k, &i)| {
                    let e = &info.toc[i];
                    let item = ListItem::new(e.title.clone(), format!("toc:{i}")).detail(page_text(e.page)).level(e.level);
                    let item = if has_children(&info.toc, i) { item.fold(is_open(&info.open, i), format!("fold:{i}")) } else { item };
                    ListItem { checked: (mark == Some(k)).then_some(true), ..item }
                })
                .collect();
            let mut l = List::new(items, ROW_H).with_scroll_bar();
            l.selected = mark;
            if let Some(a) = info.anchor.and_then(|a| shown_for(&info.toc, &shown, a)) {
                l.page = a / l.per_page(list_r);
            }
            l
        }
        1 => {
            let items = info
                .bookmarks
                .iter()
                .enumerate()
                .map(|(i, b)| ListItem::new(b.title.clone(), format!("bm:{i}")).subtitle(b.subtitle.clone()).detail(page_text(b.page)).icon(Icon::Bookmark))
                .collect();
            List::new(items, NOTE_ROW_H).with_scroll_bar()
        }
        _ => {
            let items = extra
                .highlights
                .iter()
                .enumerate()
                .map(|(i, h)| ListItem::new(format!("\u{201c}{}\u{201d}", h.text), format!("hl:{i}")).subtitle(h.subtitle.clone()).detail(page_text(h.page)))
                .collect();
            List::new(items, NOTE_ROW_H).with_scroll_bar()
        }
    };
    if let (Some(c), None) = (list.selected, info.anchor) {
        list.page = c / list.per_page(list_r);
    }
    if list.items.is_empty() {
        let (icon, text) = match tab {
            0 => (Icon::Toc, "This book has no table of contents"),
            1 => (Icon::Bookmark, "No bookmarks yet: tap the bookmark in the menu"),
            _ => (Icon::Book, "No highlights yet: hold a word and drag to select"),
        };
        let ir = Rect::new((w / 2) as i32 - 48, list_top + space(6), 96, 96);
        o.tree.add(Some(root), Box::new(ink_ui::IconView::new(icon, "")), ir);
        o.tree.add(Some(root), Box::new(Label::new(text, 36.0).styled(Style::Italic)), Rect::new(space(3), ir.bottom() + space(2), w.saturating_sub(48), 80));
    } else {
        o.ids.insert("list", o.tree.add(Some(root), Box::new(list), list_r));
    }
    o.ids.insert("percent", o.tree.add(Some(root), Box::new(Slider::new("Go to percent", 0.0, 100.0, 1.0, pct, "pct")), slider_r));
    o
}

impl super::ReaderScreen {
    /// Folds the Contents tab: `fold:I` opens or closes entry I, `toc-collapse` and
    /// `toc-expand` one level. Returns the panel to show, `None` for other actions and
    /// when nothing changes.
    pub(super) fn fold_action(&mut self, a: &str, size: Size) -> Option<Overlay> {
        let toc = self.goto_info(0).toc;
        let anchor = if let Some(i) = a.strip_prefix("fold:") {
            let i = i.parse::<usize>().ok().filter(|i| *i < self.toc_open.len())?;
            self.toc_open[i] = !self.toc_open[i];
            i
        } else {
            // The first entry on the list's page stays in view (or its parent).
            let first = self.overlay.as_mut().and_then(|o| {
                let id = *o.ids.get("list")?;
                let b = o.tree.bounds(id)?;
                let l = o.tree.get_mut::<List>(id)?;
                l.items.get(l.first(b))?.action.strip_prefix("toc:")?.parse().ok()
            });
            let changed = match a {
                "toc-collapse" => collapse_level(&toc, &mut self.toc_open),
                "toc-expand" => expand_level(&toc, &mut self.toc_open),
                _ => return None,
            };
            // Nothing left to fold that way: the panel stays as it is.
            if !changed {
                return None;
            }
            first.unwrap_or(0)
        };
        Some(self.goto_overlay_at(0, Some(anchor), size))
    }

    /// Saved highlights with their start positions, sorted by page (the panel's order).
    pub(super) fn highlight_rows(&mut self) -> Vec<(HighlightRow, Position)> {
        let Some(doc) = self.doc.as_deref() else { return Vec::new() };
        let saved = ink_store::BookState::load(&self.path).map(|s| s.highlights).unwrap_or_default();
        let mut rows: Vec<(HighlightRow, Position)> = saved
            .iter()
            .filter_map(|h| {
                let pos = h.start.parse::<Position>().ok()?;
                let page = doc.page_of(&pos).ok().or(h.page.map(|p| p as usize));
                let date = h.created.get(..10).unwrap_or(&h.created);
                let subtitle = match h.chapter.as_deref().filter(|c| !c.is_empty()) {
                    Some(c) => format!("{c} · {date}"),
                    None => date.to_string(),
                };
                let text: String = h.text.split_whitespace().collect::<Vec<_>>().join(" ");
                let text = if text.chars().count() > 120 { format!("{}…", text.chars().take(119).collect::<String>()) } else { text };
                Some((HighlightRow { text, subtitle, page }, pos))
            })
            .collect();
        rows.sort_by_key(|(r, _)| r.page.unwrap_or(usize::MAX));
        rows
    }

    /// The Go to panel on `tab` with highlights, its Contents opened down to the current
    /// chapter.
    pub(super) fn goto_overlay(&mut self, tab: usize, size: Size) -> Overlay {
        self.goto_overlay_at(tab, None, size)
    }

    /// The Go to panel on `tab`. With `anchor` (after a fold) the Contents list keeps
    /// that entry in view and the folds as they are; without, the current chapter is
    /// opened and shown.
    pub(super) fn goto_overlay_at(&mut self, tab: usize, anchor: Option<usize>, size: Size) -> Overlay {
        let mut info = self.goto_info(tab);
        match (anchor, info.current) {
            (Some(a), _) => info.anchor = Some(a),
            (None, Some(c)) => {
                reveal(&info.toc, &mut self.toc_open, c);
                info.open = self.toc_open.clone();
            }
            (None, None) => {}
        }
        let highlights = self.highlight_rows().into_iter().map(|(r, _)| r).collect();
        let (title, pages, estimate) = self.doc.as_deref().map(|d| (d.metadata().title.clone(), d.page_count().pages, super::footer::estimate(d))).unwrap_or_default();
        build_with(&info, &GotoExtra { title, page: self.page, pages, estimate, highlights }, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(n: usize) -> GotoInfo {
        GotoInfo {
            toc: (0..n).map(|i| TocRow { title: format!("Chapter {i}"), level: (i % 2) as u8, page: Some(i * 3) }).collect(),
            current: Some(n.saturating_sub(2)),
            ..Default::default()
        }
    }

    #[test]
    fn current_chapter_page_is_shown() {
        let mut o = build(&info(40), Size::new(1272, 1696));
        let id = o.ids["list"];
        let b = o.tree.bounds(id).unwrap();
        let l = o.tree.get_mut::<List>(id).unwrap();
        assert_eq!(l.selected, Some(38));
        assert_eq!(l.page, 38 / l.per_page(b));
        assert_eq!((l.items[1].title.as_str(), l.items[1].level), ("Chapter 1", 1));
        assert_eq!(l.items[2].detail.as_deref(), Some("p. 7"));
        assert!(l.items.iter().all(|i| i.progress.is_none()), "no progress bars");
        // Every top-level entry has a sub-entry: a chevron, open by default.
        assert_eq!(l.items[0].fold.as_ref().map(|f| (f.open, f.action.as_str())), Some((true, "fold:0")));
        assert!(l.items[1].fold.is_none());
        assert!(o.button_rect("gtab:2").is_some() && o.button_rect("close").is_some());
        assert!(o.button_rect("close").unwrap().w >= 96);
    }

    fn tree() -> Vec<TocRow> {
        let row = |t: &str, level| TocRow { title: t.into(), level, page: None };
        vec![row("I", 0), row("I.1", 1), row("I.1.a", 2), row("I.2", 1), row("II", 0), row("II.1", 1), row("III", 0)]
    }

    #[test]
    fn folding_hides_sub_entries_one_level_at_a_time() {
        let toc = tree();
        assert_eq!(visible(&toc, &[]), (0..7).collect::<Vec<_>>(), "missing entries are open");
        let mut open = vec![false; 7];
        assert_eq!(visible(&toc, &open), vec![0, 4, 6]);
        assert!(!collapse_level(&toc, &mut open), "only the top level shown");
        assert!(expand_level(&toc, &mut open));
        assert_eq!(visible(&toc, &open), vec![0, 1, 3, 4, 5, 6]);
        assert!(expand_level(&toc, &mut open));
        assert_eq!(visible(&toc, &open), (0..7).collect::<Vec<_>>());
        assert!(!expand_level(&toc, &mut open), "everything open");
        // Collapse closes the deepest open level first.
        assert!(collapse_level(&toc, &mut open));
        assert_eq!(visible(&toc, &open), vec![0, 1, 3, 4, 5, 6]);
        assert!(collapse_level(&toc, &mut open));
        assert_eq!(visible(&toc, &open), vec![0, 4, 6]);
        // One entry opened by hand, then revealing a deep entry opens its parents.
        open[4] = true;
        assert_eq!(visible(&toc, &open), vec![0, 4, 5, 6]);
        reveal(&toc, &mut open, 2);
        assert_eq!(visible(&toc, &open), (0..7).collect::<Vec<_>>());
    }

    #[test]
    fn folded_current_chapter_marks_its_parent_and_buttons_follow_the_state() {
        let size = Size::new(1272, 1696);
        let info = GotoInfo { toc: tree(), current: Some(2), open: vec![false; 7], ..Default::default() };
        let mut o = build(&info, size);
        let (c, e, x) = (o.button_rect("toc-collapse").unwrap(), o.button_rect("toc-expand").unwrap(), o.button_rect("close").unwrap());
        assert!(c.y < APP_BAR_H as i32 && c.right() <= e.x && e.right() <= x.x, "in the app bar, left of close: {c:?} {e:?} {x:?}");
        assert!(c.w >= 96 && e.w >= 96);
        let l = o.tree.get_mut::<List>(o.ids["list"]).unwrap();
        let titles: Vec<&str> = l.items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["I", "II", "III"]);
        assert_eq!(l.selected, Some(0), "I holds the current chapter");
        assert_eq!(l.items[0].checked, Some(true));
        assert_eq!(l.items[2].fold, None, "III has no sub-entries");
        // A flat contents list has no Collapse / Expand.
        let flat = GotoInfo { toc: tree().into_iter().filter(|e| e.level == 0).collect(), ..Default::default() };
        assert!(build(&flat, size).button_rect("toc-expand").is_none());
    }

    #[test]
    fn extra_adds_highlights() {
        let extra = GotoExtra {
            title: "Book".into(),
            page: 10,
            pages: 200,
            highlights: vec![HighlightRow { text: "words".into(), subtitle: "Act I · 2026-10-01".into(), page: Some(4) }],
            ..Default::default()
        };
        let mut o = build_with(&GotoInfo { tab: 2, ..info(3) }, &extra, Size::new(1272, 1696));
        let l = o.tree.get_mut::<List>(o.ids["list"]).unwrap();
        assert_eq!(l.items[0].title, "\u{201c}words\u{201d}");
        assert_eq!(l.items[0].action, "hl:0");
    }

    #[test]
    fn subtitle_shows_the_estimate_while_counting() {
        let mut e = GotoExtra { title: "Book".into(), page: 4, pages: 200, ..Default::default() };
        assert_eq!(subtitle(12.0, &e), "Book · page 5 of 200 · 12%");
        e.estimate = Some(8370);
        assert_eq!(subtitle(12.0, &e), "Book · page 5 of about 8370 · 12%");
        e.title.clear();
        assert_eq!(subtitle(12.0, &e), "Page 5 of about 8370 · 12%");
    }

    #[test]
    fn empty_lists_get_a_label() {
        let o = build(&GotoInfo::default(), Size::new(1272, 1696));
        assert!(!o.ids.contains_key("list"));
        let o = build(&GotoInfo { tab: 1, ..info(3) }, Size::new(1272, 1696));
        assert!(!o.ids.contains_key("list"));
        let o = build(&GotoInfo { tab: 2, ..info(3) }, Size::new(1272, 1696));
        assert!(!o.ids.contains_key("list"));
    }
}
