//! Search in the book: a full-screen page with an app bar (title, close), the text
//! field with a Find button, the on-screen keyboard and the results as cards: chapter
//! and page on top, the text around the match below with the query in bold and
//! underlined. Results are paged (no scrolling). After a result is chosen a small bar at
//! the bottom of the page steps through the hits (previous, `3 / 27`, next, results,
//! close); taps elsewhere on the page keep turning pages.
//!
//! Actions: `keyboard` (keys typed), `focus:query` (field tapped), `search-go`,
//! `hit:I`, `hit-prev`, `hit-next`, `hits-list`, `close`.

use super::overlay::{Bar, Overlay, OverlayKind, Rule};
use ink_device::{Rect, Size};
use ink_text::Style;
use ink_ui::bars::APP_BAR_H;
use ink_ui::draw::stroke_round_rect;
use ink_ui::widget::split_cols;
use ink_ui::{paint_button, AppBar, BarAction, Button, ButtonKind, EventResult, Gesture, Icon, Keyboard, Label, PaintCx, TextInput, UiEvent, Widget};

/// Most hits asked from the document.
pub const MAX_HITS: usize = 500;
/// Height of the hit bar.
pub const HIT_BAR_H: u32 = 120;
const FIELD_H: u32 = 110;
/// Height of a result card.
pub const CARD_H: u32 = 134;
/// Gap between result cards.
pub const CARD_GAP: u32 = 12;
/// Height of the results' page footer.
const FOOTER_H: u32 = 112;

/// A search result row.
#[derive(Clone, Debug, PartialEq)]
pub struct HitRow {
    /// Text around the match.
    pub excerpt: String,
    /// 0-based page, if known.
    pub page: Option<usize>,
}

/// A hit's excerpt for the list: starts at a word boundary before the match ("..."
/// when text was cut), whitespace collapsed. `matched` is the byte range of the match.
pub fn excerpt(text: &str, matched: std::ops::Range<usize>) -> String {
    let start = matched.start.min(text.len());
    let start = if text.is_char_boundary(start) { start } else { 0 };
    let before = &text[..start];
    let (cut, rest) = match before.find(char::is_whitespace) {
        // The first word may be cut: start after it (unless the match follows at once).
        Some(i) if start > 0 => (true, &text[i..]),
        _ => (false, text),
    };
    let body = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    if cut {
        format!("...{body}")
    } else {
        body
    }
}

/// Byte ranges of `needle` in `hay`, ignoring case (empty needle: none).
pub fn find_all(hay: &str, needle: &str) -> Vec<std::ops::Range<usize>> {
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let n: Vec<char> = needle.trim().chars().map(fold).collect();
    if n.is_empty() {
        return Vec::new();
    }
    let h: Vec<(usize, char)> = hay.char_indices().map(|(i, c)| (i, fold(c))).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + n.len() <= h.len() {
        if h[i..i + n.len()].iter().map(|(_, c)| *c).eq(n.iter().copied()) {
            let end = h.get(i + n.len()).map_or(hay.len(), |(b, _)| *b);
            out.push(h[i].0..end);
            i += n.len();
        } else {
            i += 1;
        }
    }
    out
}

/// `text` cut into runs of (piece, is_match), each piece one word or space-free part of
/// a word, so lines can break between pieces that start with a space.
fn runs(text: &str, query: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut at = 0;
    let mut push = |s: &str, m: bool| {
        // Split at spaces: a piece keeps its leading space.
        let mut start = 0;
        for (i, c) in s.char_indices() {
            if c == ' ' && i > start {
                out.push((s[start..i].to_string(), m));
                start = i;
            }
        }
        if start < s.len() {
            out.push((s[start..].to_string(), m));
        }
    };
    for r in find_all(text, query) {
        push(&text[at..r.start], false);
        push(&text[r.clone()], true);
        at = r.end;
    }
    push(&text[at..], false);
    out
}

/// Search result cards, paged. A tap on a card reports `hit:I`; the footer arrows page.
pub struct HitCards {
    /// Rows.
    pub hits: Vec<HitRow>,
    /// Chapter title of each row (missing or empty: none shown).
    pub chapters: Vec<String>,
    /// The query, set bold in the excerpts.
    pub query: String,
    /// First row shown.
    pub first: usize,
}

impl HitCards {
    /// Cards per page in `b`.
    pub fn per_page(b: Rect) -> usize {
        ((b.h.saturating_sub(FOOTER_H) + CARD_GAP) / (CARD_H + CARD_GAP)).max(1) as usize
    }

    /// Cards shown in `b`: (row index, rectangle).
    pub fn card_rects(&self, b: Rect) -> Vec<(usize, Rect)> {
        let n = HitCards::per_page(b);
        (self.first..self.hits.len().min(self.first + n))
            .enumerate()
            .map(|(k, i)| (i, Rect::new(b.x + 24, b.y + k as i32 * (CARD_H + CARD_GAP) as i32, b.w.saturating_sub(48), CARD_H)))
            .collect()
    }

    /// (previous, next) page buttons; `None` when there is one page.
    pub fn footer_rects(&self, b: Rect) -> Option<(Rect, Rect)> {
        if self.hits.len() <= HitCards::per_page(b) {
            return None;
        }
        let y = b.bottom() - FOOTER_H as i32 + 8;
        Some((Rect::new(b.x + 24, y, 160, 96), Rect::new(b.right() - 24 - 160, y, 160, 96)))
    }

    fn paint_card(&self, cx: &mut PaintCx, r: Rect, i: usize) {
        let t = cx.theme;
        let (fg, cap) = (t.fg, t.caption);
        stroke_round_rect(cx.canvas, r, t.radius_lg, t.border_px as f32, fg);
        let inner = Rect::new(r.x + 24, r.y + 12, r.w.saturating_sub(48), r.h.saturating_sub(24));
        let h = &self.hits[i];
        let page = h.page.map(|p| format!("Page {}", p + 1)).unwrap_or_default();
        let head = Rect::new(inner.x, inner.y, inner.w, 34);
        let pw = if page.is_empty() { 0.0 } else { cx.text_width(Style::Bold, cap.px, &page) };
        let base = cx.baseline_in(head, cap.px, Style::Regular);
        if !page.is_empty() {
            cx.draw_text_clipped(Style::Bold, cap.px, head.right() as f32 - pw, base, &page, fg, head);
        }
        let chapter = self.chapters.get(i).map(String::as_str).unwrap_or("");
        if !chapter.is_empty() {
            let max = (head.w as f32 - pw - 24.0).max(0.0);
            let c = cx.ellipsize(chapter, cap.px, Style::Italic, max);
            cx.draw_text_clipped(Style::Italic, cap.px, head.x as f32, base, &c, t.muted, head);
        }
        // Two lines of excerpt, the query bold with a black underline.
        let px = 32.0;
        let lh = 38.0;
        let clip = Rect::new(inner.x, head.bottom(), inner.w, (inner.bottom() - head.bottom()).max(0) as u32);
        let max_x = clip.right() as f32;
        let mut x = clip.x as f32;
        let mut base = clip.y as f32 + px;
        let mut line = 0;
        // The document cuts excerpts mid-word: show that the text goes on.
        let text = if h.excerpt.ends_with(char::is_alphanumeric) { format!("{}\u{2026}", h.excerpt) } else { h.excerpt.clone() };
        for (piece, m) in runs(&text, &self.query) {
            let style = if m { Style::Bold } else { Style::Regular };
            let w = cx.text_width(style, px, &piece);
            if x + w > max_x && x > clip.x as f32 {
                line += 1;
                if line == 2 {
                    break;
                }
                x = clip.x as f32;
                base += lh;
            }
            let piece = if x == clip.x as f32 { piece.trim_start().to_string() } else { piece };
            let w = cx.text_width(style, px, &piece);
            let drawn = if line == 1 && x + w > max_x { cx.ellipsize(&piece, px, style, (max_x - x).max(0.0)) } else { piece };
            // Returns the pen position after the text.
            let dw = cx.draw_text_clipped(style, px, x, base, &drawn, fg, clip) - x;
            if m {
                let lead = drawn.len() - drawn.trim_start().len();
                let lw = if lead > 0 { cx.text_width(style, px, &drawn[..lead]) } else { 0.0 };
                cx.canvas.fill_rect((x + lw) as i32, base as i32 + 5, (dw - lw).max(0.0) as i32, 4, fg);
            }
            x += dw;
        }
    }
}

impl Widget for HitCards {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        for (i, r) in self.card_rects(b) {
            self.paint_card(cx, r, i);
        }
        if let Some((prev, next)) = self.footer_rects(b) {
            let n = HitCards::per_page(b);
            let pages = self.hits.len().div_ceil(n);
            let enabled = (self.first > 0, self.first + n < self.hits.len());
            paint_button(cx, prev, "", Some(Icon::Back), ButtonKind::Outlined, enabled.0, false);
            paint_button(cx, next, "", Some(Icon::Forward), ButtonKind::Outlined, enabled.1, false);
            let mid = Rect::new(prev.right(), prev.y, (next.x - prev.right()).max(0) as u32, prev.h);
            let (px, style, fg) = (cx.theme.caption.px, cx.theme.caption.style, cx.theme.fg);
            cx.text_centered(mid, &format!("{} / {}", self.first / n + 1, pages), px, style, fg);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let UiEvent::Gesture(Gesture::Tap { pos }) = ev else {
            return match ev {
                UiEvent::Gesture(_) => EventResult::Handled,
                _ => EventResult::Ignored,
            };
        };
        if let Some((i, _)) = self.card_rects(b).into_iter().find(|(_, r)| r.contains(*pos)) {
            return EventResult::Action(format!("hit:{i}"));
        }
        let n = HitCards::per_page(b);
        if let Some((prev, next)) = self.footer_rects(b) {
            if prev.contains(*pos) && self.first > 0 {
                self.first = self.first.saturating_sub(n);
            } else if next.contains(*pos) && self.first + n < self.hits.len() {
                self.first += n;
            }
        }
        EventResult::Handled
    }
}

/// Rectangle of the keyboard on a `size` screen (the shared size:
/// [`Keyboard::bottom_rect`]).
pub fn keyboard_rect(size: Size) -> Rect {
    Keyboard::bottom_rect(size)
}

/// The search page. `hits` is `None` before the first search; `keyboard` shows the
/// keyboard (and the results above it).
pub fn build(query: &str, hits: Option<&[HitRow]>, keyboard: bool, size: Size) -> Overlay {
    build_with_chapters(query, hits, &[], keyboard, size)
}

/// [`build`] with the chapter title of each hit (`chapters[i]` for `hits[i]`; missing or
/// empty titles are not shown).
pub fn build_with_chapters(query: &str, hits: Option<&[HitRow]>, chapters: &[String], keyboard: bool, size: Size) -> Overlay {
    let mut o = Overlay::new(OverlayKind::Search);
    let w = size.w;
    let root = o.tree.add(None, Box::new(Bar::new(Rule::None)), Rect::from_size(size));
    let bar = AppBar::new("Search in book").action(BarAction::icon(Icon::Close, "close"));
    let bar_r = Rect::new(0, 0, w, APP_BAR_H);
    for (r, a) in bar.action_rects(bar_r).into_iter().zip(&bar.actions) {
        o.buttons.push((a.action.clone(), r));
    }
    o.tree.add(Some(root), Box::new(bar), bar_r);
    let field_r = Rect::new(32, APP_BAR_H as i32 + 24, w.saturating_sub(64 + 236), FIELD_H);
    let mut field = TextInput::new("Words to find", "query");
    field.set_text(query);
    field.focused = keyboard;
    o.buttons.push(("focus:query".into(), field_r));
    o.ids.insert("field", o.tree.add(Some(root), Box::new(field), field_r));
    o.button(Some(root), Button::filled("Find", "search-go").with_icon(Icon::Search), Rect::new(field_r.right() + 16, field_r.y + 3, 220, 104));
    let top = field_r.bottom() + 24;
    let bottom = if keyboard { keyboard_rect(size).y - 10 } else { size.h as i32 - 16 };
    let area = Rect::new(0, top, w, (bottom - top).max(0) as u32);
    let caption = |cx_text: String| Label::left(cx_text, 30.0).styled(Style::Bold);
    match hits {
        Some([]) => {
            o.tree.add(Some(root), Box::new(Label::left(format!("No matches for \u{201c}{}\u{201d}", query.trim()), 36.0).styled(Style::Italic)), Rect::new(32, top, w.saturating_sub(64), 100));
        }
        Some(hits) => {
            let n = if hits.len() >= MAX_HITS { format!("First {} matches", hits.len()) } else if hits.len() == 1 { "1 match".into() } else { format!("{} matches", hits.len()) };
            o.tree.add(Some(root), Box::new(caption(n)), Rect::new(32, top, w.saturating_sub(64), 56));
            let list_r = Rect::new(0, top + 64, w, area.h.saturating_sub(64));
            if list_r.h >= CARD_H + FOOTER_H {
                let cards = HitCards { hits: hits.to_vec(), chapters: chapters.to_vec(), query: query.to_string(), first: 0 };
                o.ids.insert("list", o.tree.add(Some(root), Box::new(cards), list_r));
            }
        }
        None => {}
    }
    if keyboard {
        o.ids.insert("keyboard", o.tree.add(None, Box::new(Keyboard::new()), keyboard_rect(size)));
    }
    o
}

/// The bar that steps through hits: `index` of `total` is shown.
pub fn hit_bar(index: usize, total: usize, size: Size) -> Overlay {
    let mut o = Overlay::new(OverlayKind::Hits);
    let r = Rect::new(0, (size.h - HIT_BAR_H) as i32, size.w, HIT_BAR_H);
    let bar = o.tree.add(None, Box::new(Bar::new(Rule::Top)), r);
    let inner = Rect::new(16, r.y + 12, size.w.saturating_sub(32), HIT_BAR_H - 20);
    let cells = split_cols(inner, 5, 12);
    o.button(Some(bar), Button::text("", "hit-prev").with_icon(Icon::Back), cells[0]);
    o.tree.add(Some(bar), Box::new(Label::new(format!("{} / {}", index + 1, total), 36.0).styled(Style::Bold)), cells[1]);
    o.button(Some(bar), Button::text("", "hit-next").with_icon(Icon::Forward), cells[2]);
    o.button(Some(bar), Button::text("", "hits-list").with_icon(Icon::ListView), cells[3]);
    o.button(Some(bar), Button::text("", "close").with_icon(Icon::Close), cells[4]);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpts() {
        assert_eq!(excerpt("ter LADY and Nurse  here", 13..18), "...LADY and Nurse here");
        assert_eq!(excerpt("Nurse here", 0..5), "Nurse here");
        assert_eq!(excerpt("abcNurse", 3..8), "abcNurse");
    }

    #[test]
    fn layouts() {
        let size = Size::new(1272, 1696);
        let o = build("", None, true, size);
        assert!(o.ids.contains_key("keyboard") && o.ids.contains_key("field") && !o.ids.contains_key("list"));
        let hits: Vec<HitRow> = (0..30).map(|i| HitRow { excerpt: format!("hit {i}"), page: Some(i) }).collect();
        let o = build("x", Some(&hits), false, size);
        assert!(o.ids.contains_key("list") && !o.ids.contains_key("keyboard"));
        let o = build("x", Some(&[]), false, size);
        assert!(!o.ids.contains_key("list"));
        let b = hit_bar(2, 9, size);
        for a in ["hit-prev", "hit-next", "hits-list", "close"] {
            assert!(b.button_rect(a).unwrap().y >= (size.h - HIT_BAR_H) as i32, "{a}");
        }
    }

    #[test]
    fn query_matches_ignore_case() {
        assert_eq!(find_all("Nurse, NURSE and nurses", "nurse"), vec![0..5, 7..12, 17..22]);
        assert_eq!(find_all("abc", " "), Vec::<std::ops::Range<usize>>::new());
        assert_eq!(find_all("Ärger ärger", "ÄR"), vec![0..3, 7..10]);
        let r = runs("the Nurse here", "nurse");
        assert_eq!(r, vec![("the".into(), false), (" ".into(), false), ("Nurse".into(), true), (" here".into(), false)]);
    }

    #[test]
    fn cards_page_and_report_hits() {
        let size = Size::new(1272, 1696);
        let hits: Vec<HitRow> = (0..30).map(|i| HitRow { excerpt: format!("hit {i}"), page: Some(i) }).collect();
        let mut o = build_with_chapters("hit", Some(&hits), &["Act I".into()], false, size);
        let id = o.ids["list"];
        let lb = o.tree.bounds(id).unwrap();
        let tap = |o: &mut Overlay, x: i32, y: i32| match o.tree.dispatch(&UiEvent::Gesture(Gesture::Tap { pos: ink_device::Point::new(x, y) })) {
            Some((_, EventResult::Action(a))) => Some(a),
            _ => None,
        };
        // The second card is where the second list row used to be.
        assert_eq!(tap(&mut o, 400, lb.y + 150).as_deref(), Some("hit:1"));
        let per = HitCards::per_page(lb);
        assert!(per >= 5, "{per}");
        let (prev, next) = o.tree.get_mut::<HitCards>(id).unwrap().footer_rects(lb).unwrap();
        assert!(next.bottom() <= size.h as i32 && prev.h >= 96);
        assert_eq!(tap(&mut o, next.x + 10, next.y + 10), None);
        assert_eq!(tap(&mut o, 400, lb.y + 20), Some(format!("hit:{per}")));
        assert_eq!(tap(&mut o, prev.x + 10, prev.y + 10), None);
        assert_eq!(tap(&mut o, 400, lb.y + 20).as_deref(), Some("hit:0"));
        // The app bar's close button is recorded.
        let c = o.button_rect("close").unwrap();
        assert!(c.w >= 96 && c.y < APP_BAR_H as i32);
    }
}
