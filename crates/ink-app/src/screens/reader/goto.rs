//! "Go to" panel: a full-screen page with an app bar ("Go to", the book and page, a close
//! button), tabs Contents / Bookmarks / Highlights and a percent slider at the bottom.
//!
//! - Contents: the TOC nested by indent, the current chapter marked and selected, and
//!   with [`GotoExtra`] a progress bar per chapter (read, partly read, unread).
//! - Bookmarks: chapter or label, creation time, page.
//! - Highlights: the quoted text, chapter and date, page (needs [`GotoExtra`]).
//!
//! Actions: `toc:I` (TOC entry I), `bm:I` (bookmark I), `hl:I` (highlight I), `pct:N`
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

/// Reading progress of every TOC entry: a chapter runs from its page to the page of the
/// next entry at its level or above (or the end of the book). `None` when a page is
/// unknown.
pub fn chapter_progress(toc: &[TocRow], page: usize, pages: usize) -> Vec<Option<f32>> {
    (0..toc.len())
        .map(|i| {
            let start = toc[i].page?;
            let end = toc[i + 1..].iter().find(|e| e.level <= toc[i].level).and_then(|e| e.page).unwrap_or(pages).max(start + 1);
            if page < start {
                Some(0.0)
            } else {
                Some(((page + 1 - start) as f32 / (end - start) as f32).min(1.0))
            }
        })
        .collect()
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
    let mut bar = AppBar::new("Go to").with_subtitle(sub).action(BarAction::icon(Icon::Close, "close"));
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
            let progress = if extra.pages > 0 { chapter_progress(&info.toc, extra.page, extra.pages) } else { vec![None; info.toc.len()] };
            let items = info
                .toc
                .iter()
                .zip(progress)
                .enumerate()
                .map(|(i, (e, p))| {
                    let indent = "      ".repeat(e.level.min(4) as usize);
                    let mut item = ListItem::new(format!("{indent}{}", e.title), format!("toc:{i}")).detail(page_text(e.page));
                    if let Some(p) = p {
                        item = item.progress(p);
                    }
                    if info.current == Some(i) {
                        item = item.checked(true);
                    }
                    item
                })
                .collect();
            let mut l = List::new(items, ROW_H);
            l.selected = info.current;
            l
        }
        1 => {
            let items = info
                .bookmarks
                .iter()
                .enumerate()
                .map(|(i, b)| ListItem::new(b.title.clone(), format!("bm:{i}")).subtitle(b.subtitle.clone()).detail(page_text(b.page)).icon(Icon::Bookmark))
                .collect();
            List::new(items, NOTE_ROW_H)
        }
        _ => {
            let items = extra
                .highlights
                .iter()
                .enumerate()
                .map(|(i, h)| ListItem::new(format!("\u{201c}{}\u{201d}", h.text), format!("hl:{i}")).subtitle(h.subtitle.clone()).detail(page_text(h.page)))
                .collect();
            List::new(items, NOTE_ROW_H)
        }
    };
    if let Some(c) = list.selected {
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

    /// The Go to panel on `tab` with chapter progress and highlights.
    pub(super) fn goto_overlay(&mut self, tab: usize, size: Size) -> Overlay {
        let info = self.goto_info(tab);
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
        assert!(l.items[1].title.starts_with("      Chapter 1"));
        assert_eq!(l.items[2].detail.as_deref(), Some("p. 7"));
        assert!(l.items[2].progress.is_none());
        assert!(o.button_rect("gtab:2").is_some() && o.button_rect("close").is_some());
        assert!(o.button_rect("close").unwrap().w >= 96);
    }

    #[test]
    fn chapter_progress_runs_to_the_next_entry_at_the_same_level() {
        let toc = vec![
            TocRow { title: "I".into(), level: 0, page: Some(0) },
            TocRow { title: "I.1".into(), level: 1, page: Some(2) },
            TocRow { title: "II".into(), level: 0, page: Some(10) },
            TocRow { title: "?".into(), level: 0, page: None },
        ];
        let p = chapter_progress(&toc, 4, 20);
        assert_eq!(p[0], Some(0.5));
        // I.1 runs to II (page 10): pages 2..=4 of 2..10.
        assert_eq!(p[1], Some(3.0 / 8.0));
        assert_eq!(p[2], Some(0.0));
        assert_eq!(p[3], None);
        assert_eq!(chapter_progress(&toc, 19, 20)[2], Some(1.0));
    }

    #[test]
    fn extra_adds_progress_and_highlights() {
        let extra = GotoExtra {
            title: "Book".into(),
            page: 10,
            pages: 200,
            highlights: vec![HighlightRow { text: "words".into(), subtitle: "Act I · 2026-10-01".into(), page: Some(4) }],
            ..Default::default()
        };
        let mut o = build_with(&info(10), &extra, Size::new(1272, 1696));
        let l = o.tree.get_mut::<List>(o.ids["list"]).unwrap();
        assert_eq!(l.items[0].progress, Some(1.0));
        assert_eq!(l.items[9].progress, Some(0.0));
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
