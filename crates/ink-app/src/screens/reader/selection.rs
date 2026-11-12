//! Text selection by long press and drag, and highlights.
//!
//! A finger held on text (the first `Hold` notification, after [`ink_ui::LONG_PRESS`]) selects the whole word
//! there ([`word_hit`]); moving the finger extends the selection word by word; lifting
//! it opens a card with the quote and icon buttons: Highlight, Note, Insightful (when
//! that plugin is loaded; it runs the plugin's selection action, as its plugin button
//! did), the other plugin actions and Search that fit, More (a page inside the card
//! listing the rest) and Close. With `gestures.long_press = dictionary` a one-word
//! selection is also looked up at once: the dictionary card is a sibling of the
//! selection card in the same overlay tree, over one page scrim, so a tap on the page
//! closes both. The selection is shown inverted.
//! A long press where no word is found still turns the page (phase 0 behaviour).
//!
//! A passage that goes on over the page: the finger drags off the text (past the last
//! line, into the bottom band or the bottom-right corner, [`edge_zone`]) and rests there
//! for [`EDGE_DWELL`]; the view moves on half a page ([`Bridge`]: the lower half of the
//! page at the top, the upper half of the next below it; the reader's page-turn refresh)
//! and the selection continues at the next page's first word, now in the middle of the
//! screen with the text before it in sight. Another rest goes on to the next page whole
//! (the selection runs to the last line the bridge showed), and so on. The top band, the
//! top-left corner or above the first line goes back half a page the same way. After a
//! move the finger must leave the zone before another one. When the selection's card
//! closes, the reader shows the page the selection ended on. The selection is kept as document positions
//! from the press's word to the dragged end ([`span_select`]: every page's part through
//! `Document::select`, joined, and set as the document's selection for plugins); its
//! card, Note, Search, plugins and the saved highlight get the whole text, and the
//! highlight (one start/end range) is drawn on each page through `range_rects`.
//!
//! Highlights are saved in the sidecar (`BookState.highlights`, style `lighten`). They
//! are drawn as a gray underlay on pages whose rectangles the reader knows: those made in
//! this session, in the current layout. Drawing saved highlights after a reopen needs a
//! `Document` call that maps a position range to rectangles.

use super::overlay::{keyword_icon, Overlay, OverlayKind};
use ink_device::{Canvas, Point, Rect, Size};
use ink_doc::Selection;
use ink_text::Style;
use ink_ui::dialog::wrap_lines;
use ink_ui::controls::paint_round_button;
use ink_ui::draw::{fill_round_rect, stroke_round_rect, Path};
use ink_ui::widget::split_cols;
use ink_ui::{paint_button, ButtonKind, EventResult, Gesture, Icon, PaintCx, Popup, UiEvent, Widget};

/// Smallest rectangle holding all of `rects` (`None` when empty).
pub fn bounds(rects: &[Rect]) -> Option<Rect> {
    let mut it = rects.iter();
    let first = *it.next()?;
    Some(it.fold(first, |a, r| a.union(r)))
}

/// Inverts `rects` (the selection look).
pub fn invert(canvas: &mut Canvas, rects: &[Rect]) {
    for r in rects {
        canvas.invert_rect(*r);
    }
}

/// Marks a search hit: a 3 px frame around each rectangle (the text stays readable).
pub fn frame(canvas: &mut Canvas, rects: &[Rect]) {
    const T: i32 = 3;
    for r in rects {
        canvas.stroke_rect(Rect::new(r.x - T - 1, r.y - 1, r.w + 2 * (T as u32 + 1), r.h + 2), T, 0);
    }
}

/// Draws a `lighten` highlight: white turns light gray, black stays black.
pub fn lighten(canvas: &mut Canvas, rects: &[Rect]) {
    for r in rects {
        let r = r.intersect(&canvas.bounds());
        for y in r.y..r.bottom() {
            let row = y as usize * canvas.width;
            for x in r.x..r.right() {
                let p = &mut canvas.pixels[row + x as usize];
                *p = (*p as u16 * 3 / 4) as u8;
            }
        }
    }
}

/// A character that can be part of a word: letters, digits, apostrophes and hyphens.
fn word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '\'' | '\u{2019}' | '-' | '\u{2010}' | '\u{2011}')
}

/// The one character of a probe's text (`None` for none or several, e.g. a trimmed space).
fn single(text: &str) -> Option<char> {
    let mut it = text.chars();
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

/// Farthest a scan looks past a character's edge for the next one.
const SCAN_PX: i32 = 24;
/// Scan step while a probe still returns the same character.
const SCAN_STEP: i32 = 3;
/// An x past the right end of any line: a probe there returns the line's last character.
const LINE_END_X: i32 = 1 << 20;

/// A hyphen, as a probe returns it.
fn hyphen(text: &str) -> bool {
    matches!(single(text), Some('-' | '\u{2010}' | '\u{2011}' | '\u{ad}'))
}

/// The word characters next to `from` on its line, nearest first: to the right
/// (`forward`) or the left, up to a non-word character, a gap or the line's end.
fn scan_line(probe: &mut dyn FnMut(Point) -> Option<(String, Rect)>, from: Rect, forward: bool) -> Vec<(char, Rect)> {
    let y = from.y + from.h as i32 / 2;
    let mut out: Vec<(char, Rect)> = Vec::new();
    let mut cur = from;
    // `r` is the character right after (before) `cur` on the line, with no gap between.
    let adjacent = |r: Rect, cur: Rect| r.y == from.y && if forward { r.right() > cur.right() && r.x <= cur.right() + 2 } else { r.x < cur.x && r.right() >= cur.x - 2 };
    while out.len() < 64 {
        let mut x = if forward { cur.right() } else { cur.x - 1 };
        let mut next = None;
        while if forward { x <= cur.right() + SCAN_PX } else { x >= (cur.x - SCAN_PX).max(0) } {
            match probe(Point::new(x, y)) {
                Some((_, r)) if r == cur => x += if forward { SCAN_STEP } else { -SCAN_STEP },
                Some((t, r)) if adjacent(r, cur) => {
                    next = single(&t).filter(|c| word_char(*c)).map(|c| (c, r));
                    break;
                }
                _ => break,
            }
        }
        let Some((c, r)) = next else { break };
        out.push((c, r));
        cur = r;
    }
    // Trim apostrophes and hyphens at the far end.
    while out.last().is_some_and(|(c, _)| !c.is_alphanumeric()) {
        out.pop();
    }
    out
}

/// The hyphen that ends `last`'s line right after it, if any: a hyphen of the book
/// ("well-" | "known") or the one the layout added ("in-" | "finities"), which a
/// document's selection leaves out of the text (so the probe returns no text).
fn hyphen_after(probe: &mut dyn FnMut(Point) -> Option<(String, Rect)>, last: Rect) -> Option<Rect> {
    let (t, r) = probe(Point::new(LINE_END_X, last.y + last.h as i32 / 2))?;
    let next_to = r.y == last.y && r.x >= last.right() - 2 && r.x <= last.right() + 2;
    (next_to && (hyphen(&t) || t.is_empty())).then_some(r)
}

/// Finds the word around `start` with `probe`, which returns the character a point
/// lands on (its text and rectangle; `Document::select` of one point). Returns the
/// rectangles of the word's first and last character.
///
/// The scan does not depend on how the document maps a point to a character (the
/// nearest character boundary, as ink-html does, or the character containing the
/// point): it steps from the current character's edge, a few pixels at a time, until a
/// probe returns a different character, and stops at a non-word character, a gap
/// between characters (a space between text runs) or another line. Punctuation at the
/// ends (quotes, commas, a trailing apostrophe or hyphen) is trimmed. A press on a space
/// or a punctuation mark takes the word within 30 px.
///
/// A word hyphenated at the end of a line goes on at the start of the next: pressed on
/// either part, the word is both (its first and last character are then on two lines).
pub fn word_span(start: Point, probe: &mut dyn FnMut(Point) -> Option<(String, Rect)>) -> Option<(Rect, Rect)> {
    let mut r0 = None;
    for dx in [0, -10, 10, -20, 20, -30, 30] {
        if let Some((t, r)) = probe(Point::new(start.x + dx, start.y)) {
            if single(&t).is_some_and(char::is_alphanumeric) {
                r0 = Some(r);
                break;
            }
        }
    }
    let r0 = r0?;
    let mut first = scan_line(probe, r0, false).last().map_or(r0, |(_, r)| *r);
    let mut last = scan_line(probe, r0, true).last().map_or(r0, |(_, r)| *r);
    let h = r0.h as i32;
    // The word ends its line with a hyphen: it goes on at the next line's start.
    if hyphen_after(probe, last).is_some() {
        let below = probe(Point::new(0, last.bottom() + h / 2));
        if let Some((_, r)) = below.filter(|(t, r)| r.y > last.y && r.y <= last.bottom() + h / 2 && single(t).is_some_and(char::is_alphanumeric)) {
            last = scan_line(probe, r, true).last().map_or(r, |(_, r)| *r);
        }
    }
    // The word starts its line and the line before ends with a hyphen: it began there.
    let starts_line = probe(Point::new(0, first.y + h / 2)).is_some_and(|(_, r)| r == first);
    if starts_line {
        let above = probe(Point::new(LINE_END_X, first.y - h / 2));
        if let Some((_, r)) = above.filter(|(t, r)| r.y < first.y && r.bottom() + h / 2 >= first.y && (hyphen(t) || t.is_empty())) {
            if let Some((_, w)) = scan_line(probe, r, false).last() {
                first = *w;
            }
        }
    }
    Some((first, last))
}

/// A word found by [`word_hit`].
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    /// The word as selected (the document's selection is set to it).
    pub sel: Selection,
    /// A point on its first character.
    pub first: Point,
    /// A point on its last character.
    pub last: Point,
}

impl Word {
    /// True when this word comes before `other` in reading order (an earlier line, or
    /// the same line further left).
    pub fn before(&self, other: &Word) -> bool {
        let dy = self.first.y - other.first.y;
        if dy.abs() > 8 {
            dy < 0
        } else {
            self.first.x < other.first.x
        }
    }
}

/// The whole word under `pos` on `page` ([`word_span`] with `Document::select` probes),
/// selected in the document. `None` when there is no word near `pos`.
pub fn word_hit(doc: &mut dyn ink_doc::Document, page: usize, pos: Point) -> Option<Word> {
    let t0 = std::time::Instant::now();
    let mut probes = 0u32;
    let span = {
        let mut probe = |p: Point| {
            probes += 1;
            let s = doc.select(page, p, p).ok()??;
            Some((s.text, *s.rects.first()?))
        };
        word_span(pos, &mut probe)
    };
    let (a, b) = span?;
    // x + 1: the left part of a character maps to it under both point rules.
    let first = Point::new(a.x + 1, a.y + a.h as i32 / 2);
    let last = Point::new(b.x + 1, b.y + b.h as i32 / 2);
    let sel = doc.select(page, first, last).ok()??;
    log::debug!("reader: word {:?} at {},{} in {} probes, {} ms", sel.text, pos.x, pos.y, probes + 1, t0.elapsed().as_millis());
    Some(Word { sel, first, last })
}

/// The whole word under `pos` on `page` (see [`word_hit`]).
pub fn word_at(doc: &mut dyn ink_doc::Document, page: usize, pos: Point) -> Option<Selection> {
    word_hit(doc, page, pos).map(|w| w.sel)
}

/// Where the moving end of a selection is on the page shown (the other end is the word
/// the long press selected, on its own page).
#[derive(Clone, Debug, PartialEq)]
pub enum SpanEnd {
    /// A word on the page shown.
    Word(Word),
    /// The page's first character.
    PageStart,
    /// The page's last character.
    PageEnd,
}

/// A point that `Document::select` maps to a page's first character (the nearest text
/// to the top-left corner).
pub fn page_start() -> Point {
    Point::new(0, 0)
}

/// A point that `Document::select` maps to the last character of a page on a `size`
/// screen (the nearest text to the bottom-right corner).
pub fn page_end(size: Size) -> Point {
    Point::new(size.w as i32 - 1, size.h as i32 - 1)
}

/// Most pages one selection spans (the drag stops turning pages there).
pub const MAX_SPAN_PAGES: usize = 8;

/// A selection that may span pages.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// The whole selection: `start`/`end` are document positions (the first page's start,
    /// the last page's end), `text` is all of it, `page`/`rects` are the part on the page
    /// shown (empty rects when nothing of it is there).
    pub sel: Selection,
    /// Every page of the selection in reading order with its rectangles there.
    pub pages: Vec<(usize, Vec<Rect>)>,
}

/// The selection from `anchor` (a word on `anchor_page`) to `end` on `page` (the page
/// shown), across pages: on the earlier page from its end of the selection to the page
/// end, every page between whole, on the later page from the page start. Each page's
/// part comes from `Document::select`; the pieces are joined with a line break, and the
/// document's selection is set to the whole (`Document::set_selection`), which plugins
/// read. `None` when no page has text in the range.
pub fn span_select(doc: &mut dyn ink_doc::Document, size: Size, anchor_page: usize, anchor: &Word, page: usize, end: &SpanEnd) -> Option<Span> {
    let (s, e) = (page_start(), page_end(size));
    let (end_first, end_last) = match end {
        SpanEnd::Word(w) => (w.first, w.last),
        SpanEnd::PageStart => (s, s),
        SpanEnd::PageEnd => (e, e),
    };
    let segs: Vec<(usize, Point, Point)> = if page == anchor_page {
        let (from, to) = match end {
            SpanEnd::Word(w) if w.before(anchor) => (w.first, anchor.last),
            SpanEnd::Word(w) => (anchor.first, w.last),
            SpanEnd::PageStart => (s, anchor.last),
            SpanEnd::PageEnd => (anchor.first, e),
        };
        vec![(page, from, to)]
    } else {
        // (page, point) of the earlier and the later end.
        let ((p0, from), (p1, to)) = if page > anchor_page { ((anchor_page, anchor.first), (page, end_last)) } else { ((page, end_first), (anchor_page, anchor.last)) };
        let mut v = vec![(p0, from, e)];
        v.extend((p0 + 1..p1).map(|p| (p, s, e)));
        v.push((p1, s, to));
        v
    };
    let parts: Vec<Selection> = segs.into_iter().filter_map(|(p, from, to)| doc.select(p, from, to).ok().flatten()).collect();
    let (first, last) = (parts.first()?, parts.last()?);
    let text = parts.iter().map(|p| p.text.as_str()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n");
    let rects = parts.iter().find(|p| p.page == page).map(|p| p.rects.clone()).unwrap_or_default();
    let sel = Selection { start: first.start.clone(), end: last.end.clone(), text, page, rects };
    let pages = parts.iter().map(|p| (p.page, p.rects.clone())).collect();
    if parts.len() > 1 {
        doc.set_selection(sel.clone());
    }
    Some(Span { sel, pages })
}

/// The top of a page's first line and the bottom of its last (`None` without text).
pub fn text_box(doc: &mut dyn ink_doc::Document, page: usize, size: Size) -> Option<(i32, i32)> {
    let (s, e) = (page_start(), page_end(size));
    let top = doc.select(page, s, s).ok()??.rects.first()?.y;
    let bottom = doc.select(page, e, e).ok()??.rects.last()?.bottom();
    doc.clear_selection();
    Some((top, bottom))
}

/// Height of the bands at the top and bottom screen edge that turn the page while a
/// selection is dragged there.
pub const EDGE_PX: i32 = 48;
/// A finger this far past the first or last text line is "off the text".
pub const PAST_TEXT_PX: i32 = 16;
/// Size of the corner squares (bottom right: next page, top left: previous) that also
/// turn the page.
pub const EDGE_CORNER_PX: i32 = 120;

/// The page-turn zone a dragged selection end is in: `1` (next page) past the last text
/// line, in the bottom band or the bottom-right corner; `-1` (previous page) above the
/// first line, in the top band or the top-left corner; `0` elsewhere. `text`: the top of
/// the page's first line and the bottom of its last ([`text_box`]).
pub fn edge_zone(p: Point, size: Size, text: Option<(i32, i32)>) -> i8 {
    let (w, h) = (size.w as i32, size.h as i32);
    let below = text.is_some_and(|(_, b)| p.y > b + PAST_TEXT_PX);
    let above = text.is_some_and(|(t, _)| p.y < t - PAST_TEXT_PX);
    if p.y >= h - EDGE_PX || below || (p.x >= w - EDGE_CORNER_PX && p.y >= h - EDGE_CORNER_PX) {
        1
    } else if p.y < EDGE_PX || above || (p.x < EDGE_CORNER_PX && p.y < EDGE_CORNER_PX) {
        -1
    } else {
        0
    }
}

/// How long a dragged selection end rests in a page-turn zone ([`edge_zone`]) before
/// the page turns.
pub const EDGE_DWELL: std::time::Duration = std::time::Duration::from_millis(600);

/// Where a selection continues after a drag turned to `page`: its first word (`forward`)
/// or its last word; the page start or end when no word is found there.
pub fn edge_word(doc: &mut dyn ink_doc::Document, page: usize, size: Size, forward: bool) -> SpanEnd {
    let at = if forward { page_start() } else { page_end(size) };
    let ch = doc.select(page, at, at).ok().flatten().and_then(|s| if forward { s.rects.first().copied() } else { s.rects.last().copied() });
    let word = ch.and_then(|r| word_hit(doc, page, Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2)));
    word.map_or(if forward { SpanEnd::PageStart } else { SpanEnd::PageEnd }, SpanEnd::Word)
}

/// The text lines of `page` as `(top, bottom)` bands, top to bottom (overlapping rects
/// merged). Clears the document's selection.
pub fn line_bands(doc: &mut dyn ink_doc::Document, page: usize, size: Size) -> Vec<(i32, i32)> {
    let (s, e) = (page_start(), page_end(size));
    let mut rects = doc.select(page, s, e).ok().flatten().map(|s| s.rects).unwrap_or_default();
    doc.clear_selection();
    rects.sort_by_key(|r| r.y);
    let mut bands: Vec<(i32, i32)> = Vec::new();
    for r in rects {
        match bands.last_mut() {
            Some(b) if r.y < b.1 => b.1 = b.1.max(r.bottom()),
            _ => bands.push((r.y, r.bottom())),
        }
    }
    bands
}

/// Half a page on: the lower half of page `page`'s text moved up to the text top, and
/// the upper half of the next page below it, each cut between lines. A selection drag
/// held at the bottom edge goes page, bridge, next page, ... (back the same way), half
/// a page at a time, so the place it continues from stays in the middle of the screen
/// with the text before it in sight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bridge {
    /// The upper page (the next one is below it).
    pub page: usize,
    /// Screen y of the text top (where the upper page's part starts).
    pub top: i32,
    /// Upper page y of the first line shown (the cut).
    pub cut: i32,
    /// Upper page y of the bottom of its last line.
    pub end: i32,
    /// Screen y where the lower page's part starts.
    pub split: i32,
    /// Lower page y of its text top.
    pub top2: i32,
    /// Lower page y of the end of its part (the bottom of its last line shown).
    pub cut2: i32,
    /// The lower of the two pages' last line bottoms: the text area ends there.
    pub bottom: i32,
}

impl Bridge {
    /// The bridge from `page` to `page + 1`; `None` when either has no text lines or
    /// `page` has one line (nothing to cut).
    pub fn new(doc: &mut dyn ink_doc::Document, page: usize, size: Size) -> Option<Bridge> {
        let a = line_bands(doc, page, size);
        let b = line_bands(doc, page + 1, size);
        let (&(top, _), &(_, a_end)) = (a.first()?, a.last()?);
        let (&(top2, _), &(_, b_end)) = (b.first()?, b.last()?);
        // The first line from the middle of the text on (never the first line).
        let mid = (top + a_end) / 2;
        let k = a.iter().position(|l| l.0 >= mid).filter(|k| *k > 0)?;
        let cut = a[k].0;
        let gap = (a[k].0 - a[k - 1].1).max(0);
        let split = top + (a_end - cut) + gap;
        // Lines of the next page that fit above the lower of the two text ends.
        let bottom = a_end.max(b_end);
        let cut2 = b.iter().take_while(|l| split + (l.1 - top2) <= bottom).last().map_or(top2, |l| l.1);
        Some(Bridge { page, top, cut, end: a_end, split, top2, cut2, bottom })
    }

    /// The page and page point under screen point `p`.
    pub fn to_page(&self, p: Point) -> (usize, Point) {
        if p.y < self.split {
            (self.page, Point::new(p.x, p.y - self.top + self.cut))
        } else {
            (self.page + 1, Point::new(p.x, p.y - self.split + self.top2))
        }
    }

    /// Screen rectangles of page rectangles (`pages`: page and its rects): the parts in
    /// the bridge, moved to where they are shown.
    pub fn to_screen(&self, pages: &[(usize, Vec<Rect>)]) -> Vec<Rect> {
        let mut out = Vec::new();
        for (page, rects) in pages {
            for r in rects {
                if *page == self.page && r.y >= self.cut - 1 {
                    out.push(Rect::new(r.x, r.y - self.cut + self.top, r.w, r.h));
                } else if *page == self.page + 1 && r.bottom() <= self.cut2 + 1 {
                    out.push(Rect::new(r.x, r.y - self.top2 + self.split, r.w, r.h));
                }
            }
        }
        out
    }

    /// The top of the first line shown and the bottom of the last (as [`text_box`]).
    pub fn text_box(&self) -> (i32, i32) {
        (self.top, self.split + (self.cut2 - self.top2))
    }

    /// Draws the bridge on `out` from the two pages drawn whole (`upper`, `lower`, the
    /// size of `out`): the lower page's margins, the text area white, the upper page's
    /// lines from the cut, then the lower page's lines that fit (whole lines only).
    pub fn compose(&self, upper: &Canvas, lower: &Canvas, out: &mut Canvas) {
        let (w, h) = (out.width, out.height as i32);
        out.pixels.copy_from_slice(&lower.pixels);
        let band_top = self.top.min(self.top2).clamp(0, h);
        let band_end = self.bottom.clamp(0, h);
        let rows = |out: &mut Canvas, src: &Canvas, from: i32, to: i32, at: i32| {
            for y in from.max(0)..to.min(h) {
                let dst = y - from + at;
                if (0..h).contains(&dst) {
                    let (d, s) = (dst as usize * w, y as usize * w);
                    out.pixels[d..d + w].copy_from_slice(&src.pixels[s..s + w]);
                }
            }
        };
        for y in band_top..band_end {
            let i = y as usize * w;
            out.pixels[i..i + w].fill(255);
        }
        rows(out, upper, self.cut, self.end, self.top);
        rows(out, lower, self.top2, self.cut2, self.split);
    }
}

/// The first (`first`) or last word of the line of `page` around page y `y`; the page
/// start or end when no word is found there.
pub fn line_word(doc: &mut dyn ink_doc::Document, page: usize, size: Size, y: i32, first: bool) -> SpanEnd {
    let fallback = if first { SpanEnd::PageStart } else { SpanEnd::PageEnd };
    // The line's rectangle from the whole page's selection (one rect per line).
    let all = doc.select(page, page_start(), page_end(size)).ok().flatten().map(|s| s.rects).unwrap_or_default();
    doc.clear_selection();
    let on = |r: &&Rect| r.y <= y && y < r.bottom();
    let line = if first { all.iter().find(on) } else { all.iter().rev().find(on) }.copied();
    let Some(r) = line else { return fallback };
    let x = if first { r.x + 2 } else { r.right() - 3 };
    word_hit(doc, page, Point::new(x, r.y + r.h as i32 / 2)).map_or(fallback, SpanEnd::Word)
}

/// Text shown at the top of the selection popup (at most `max` characters).
pub fn quote(sel: &Selection, max: usize) -> String {
    super::note_excerpt(&sel.text, max)
}

/// Widest selection card.
const CARD_W: u32 = 1100;
/// Height of a tool button (icon over caption).
const TOOL_H: u32 = 136;
/// Narrowest tool button.
const TOOL_MIN_W: u32 = 150;
/// Height of a pill in the "More" page.
const PILL_H: u32 = 96;
/// Gap between buttons.
const GAP: u32 = 16;
/// Inner padding of the card.
const PAD: i32 = 24;
/// Offset of the solid shadow.
const SHADOW: u32 = 8;
/// Line height of the quote.
const QUOTE_LH: u32 = 47;
/// Most rows of plugin pills on the "More" page.
const MORE_ROWS: usize = 3;

/// One tool of the selection card.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    /// Caption under the icon.
    pub label: String,
    /// Icon (`None`: the highlighter mark drawn by the card).
    pub icon: Option<Icon>,
    /// Reported on a tap (`sel-more` opens the "More" page inside the card).
    pub action: String,
}

/// Icons of plugin selection actions by keywords of their label ([`keyword_icon`]).
const PLUGIN_ICONS: [(&[&str], Icon); 5] = [
    (&["dict", "look", "define", "word"], Icon::Book),
    (&["translat"], Icon::Globe),
    (&["note", "comment", "ask", "chat"], Icon::Chat),
    (&["share", "send", "export"], Icon::Forward),
    (&["copy"], Icon::ListView),
];

/// Every tool of the card in order: Highlight (Remove highlight when the selection is a
/// saved highlight, `highlighted`), Note, Insightful (`plugin-sel:I` with
/// `insightful = Some(I)`, the index of Insightful's selection action in `plugins`), the
/// other plugin actions (`plugin-sel:I`), Search. Close and More are added by
/// [`layout_tools`].
pub fn all_tools(plugins: &[String], insightful: Option<usize>, highlighted: bool) -> Vec<Tool> {
    let first = if highlighted {
        Tool { label: "Remove highlight".into(), icon: Some(Icon::Close), action: "sel-unhighlight".into() }
    } else {
        Tool { label: "Highlight".into(), icon: None, action: "sel-highlight".into() }
    };
    let mut v = vec![
        first,
        Tool { label: "Note".into(), icon: Some(Icon::Chat), action: "sel-note".into() },
    ];
    if let Some(i) = insightful.filter(|i| *i < plugins.len()) {
        v.push(Tool { label: "Insightful".into(), icon: Some(Icon::Sparkle), action: format!("plugin-sel:{i}") });
    }
    v.extend(plugins.iter().enumerate().filter(|(i, _)| Some(*i) != insightful).map(|(i, l)| Tool { label: l.clone(), icon: Some(keyword_icon(l, &PLUGIN_ICONS)), action: format!("plugin-sel:{i}") }));
    v.push(Tool { label: "Search".into(), icon: Some(Icon::Search), action: "sel-search".into() });
    v
}

/// The card's main row for a card `w` wide and the tools listed after "More": as many of
/// [`all_tools`] as fit, "More" when some do not, Close.
pub fn layout_tools(plugins: &[String], insightful: Option<usize>, highlighted: bool, w: u32) -> (Vec<Tool>, Vec<Tool>) {
    let inner = w.saturating_sub(SHADOW + 2 * PAD as u32);
    let slots = ((inner + GAP) / (TOOL_MIN_W + GAP)).max(4) as usize;
    let mut all = all_tools(plugins, insightful, highlighted);
    // One slot is Close; when not everything fits, one more is More.
    let room = slots.saturating_sub(1);
    let rest = if all.len() <= room { Vec::new() } else { all.split_off(room.saturating_sub(1)) };
    if !rest.is_empty() {
        all.push(Tool { label: "More".into(), icon: Some(Icon::More), action: "sel-more".into() });
    }
    all.push(Tool { label: "Close".into(), icon: Some(Icon::Close), action: "sel-cancel".into() });
    (all, rest)
}

/// The selection card: the quoted text with a black bar on its left, a row of icon
/// buttons, and (after "More") a page listing every plugin action as pills.
pub struct SelectionCard {
    /// Quoted text.
    pub quote: String,
    /// Main row.
    pub tools: Vec<Tool>,
    /// The tools listed on the "More" page.
    pub rest: Vec<Tool>,
    /// The "More" page is shown.
    pub more: bool,
    /// First plugin shown on the "More" page.
    pub first: usize,
}

impl SelectionCard {
    /// Card height for a quote of `quote_chars` characters and `rest` tools on the "More"
    /// page when the main row is `tools` (the quote gets one to three lines, estimated
    /// from its length: building needs no fonts).
    pub fn height(tools: &[Tool], quote_chars: usize, rest: usize, w: u32) -> u32 {
        let per_line = (w.saturating_sub(2 * PAD as u32 + 40) / 18).max(10) as usize;
        let lines = quote_chars.div_ceil(per_line).clamp(1, 3) as u32;
        let base = 2 * PAD as u32 + lines * QUOTE_LH + GAP + TOOL_H + SHADOW;
        if !tools.iter().any(|t| t.action == "sel-more") {
            return base;
        }
        let rows = rest.div_ceil(2).min(MORE_ROWS) as u32;
        let more = 2 * PAD as u32 + PILL_H + rows * (PILL_H + GAP) + SHADOW;
        base.max(more)
    }

    /// The card without its shadow.
    pub fn body(b: Rect) -> Rect {
        Rect::new(b.x, b.y, b.w.saturating_sub(SHADOW), b.h.saturating_sub(SHADOW))
    }

    /// The "More" page: (back button, pills with their index in `rest`, next-page pill).
    pub fn more_rects(&self, b: Rect) -> (Rect, Vec<(usize, Rect)>, Option<Rect>) {
        let body = SelectionCard::body(b);
        let back = Rect::new(body.x + PAD, body.y + PAD, PILL_H, PILL_H);
        let top = back.bottom() + GAP as i32;
        let rows = ((body.bottom() - PAD - top + GAP as i32) / (PILL_H + GAP) as i32).max(1) as usize;
        let per_page = rows * 2;
        let left = self.rest.len().saturating_sub(self.first);
        let next = left > per_page;
        let shown = if next { per_page - 1 } else { left.min(per_page) };
        let w = body.w.saturating_sub(2 * PAD as u32);
        let mut pills = Vec::new();
        let mut cells = (0..per_page).map(|k| {
            let row = Rect::new(body.x + PAD, top + (k / 2) as i32 * (PILL_H + GAP) as i32, w, PILL_H);
            split_cols(row, 2, GAP)[k % 2]
        });
        for i in 0..shown {
            pills.push((self.first + i, cells.next().unwrap_or_default()));
        }
        let next = next.then(|| cells.next().unwrap_or_default());
        (back, pills, next)
    }

    fn paint_tool(cx: &mut PaintCx, r: Rect, t: &Tool) {
        let th = cx.theme;
        let (fg, cap) = (th.fg, th.caption);
        let side = 56;
        let ir = Rect::new(r.x + (r.w as i32 - side) / 2, r.y + 16, side as u32, side as u32);
        match t.icon {
            Some(i) => cx.icon(i, ir, side as u32, fg),
            None => highlighter(cx, ir, fg),
        }
        let label = cx.ellipsize(&t.label, cap.px, cap.style, r.w.saturating_sub(8) as f32);
        cx.text_centered(Rect::new(r.x, ir.bottom() + 6, r.w, (r.bottom() - ir.bottom() - 10).max(0) as u32), &label, cap.px, cap.style, fg);
    }
}

/// Tool rectangles along the bottom of the card `b` (same layout as the paint).
fn tool_rects(b: Rect, n: usize) -> Vec<Rect> {
    let body = SelectionCard::body(b);
    let row = Rect::new(body.x + PAD, body.bottom() - PAD - TOOL_H as i32, body.w.saturating_sub(2 * PAD as u32), TOOL_H);
    split_cols(row, n.max(1) as u32, GAP)
}

/// A highlighter mark: a slanted pen tip over a thick bar.
fn highlighter(cx: &mut PaintCx, r: Rect, v: u8) {
    let (x, y, s) = (r.x as f32, r.y as f32, r.w as f32);
    let w = (s / 12.0).max(3.0);
    Path::new()
        .polyline(&[(x + s * 0.30, y + s * 0.62), (x + s * 0.62, y + s * 0.08), (x + s * 0.86, y + s * 0.22), (x + s * 0.54, y + s * 0.76), (x + s * 0.30, y + s * 0.62)], w)
        .fill(cx.canvas, v);
    Path::new().polygon(&[(x + s * 0.30, y + s * 0.62), (x + s * 0.54, y + s * 0.76), (x + s * 0.22, y + s * 0.80)]).fill(cx.canvas, v);
    Path::new().rect(x + s * 0.08, y + s * 0.86, s * 0.84, s * 0.12).fill(cx.canvas, v);
}

impl Widget for SelectionCard {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme;
        // A solid offset shadow (no blur), then the card.
        fill_round_rect(cx.canvas, Rect::new(b.x + SHADOW as i32, b.y + SHADOW as i32, b.w.saturating_sub(SHADOW), b.h.saturating_sub(SHADOW)), t.radius_lg, t.subtle);
        let body = SelectionCard::body(b);
        fill_round_rect(cx.canvas, body, t.radius_lg, t.bg);
        stroke_round_rect(cx.canvas, body, t.radius_lg, t.border_px as f32, t.fg);
        let fg = t.fg;
        if self.more {
            let (back, pills, next) = self.more_rects(b);
            paint_round_button(cx, back, Icon::Back);
            let title = Rect::new(back.right() + PAD, back.y, (body.right() - PAD - back.right() - PAD).max(0) as u32, back.h);
            cx.text_left(title, "More actions", t.heading.px, t.heading.style, fg);
            for (i, r) in pills {
                if let Some(t) = self.rest.get(i) {
                    paint_button(cx, r, &t.label, t.icon, ButtonKind::Outlined, true, false);
                }
            }
            if let Some(r) = next {
                paint_button(cx, r, "Next", Some(Icon::Forward), ButtonKind::Text, true, false);
            }
            return;
        }
        // The quote: a black bar on the left, italic text, at most the lines that fit.
        let rects = tool_rects(b, self.tools.len());
        let top = body.y + PAD;
        let bottom = rects.first().map_or(body.bottom(), |r| r.y) - GAP as i32;
        let px = QUOTE_LH as f32 / 1.3;
        let text_x = body.x + PAD + 6 + 20;
        let max = (body.right() - PAD - text_x).max(0) as f32;
        let mut lines = wrap_lines(cx, &self.quote, px, Style::Italic, max);
        let fit = (((bottom - top) as f32) / (px * 1.3)).floor().max(1.0) as usize;
        if lines.len() > fit {
            lines.truncate(fit);
            if let Some(l) = lines.last_mut() {
                *l = cx.ellipsize(&format!("{l}\u{2026}"), px, Style::Italic, max);
            }
        }
        let used = (lines.len() as f32 * px * 1.3) as i32;
        cx.canvas.fill_rect(body.x + PAD, top + 4, 6, (used - 4).max(6), fg);
        let clip = Rect::new(text_x, top, max as u32, (bottom - top).max(0) as u32);
        let mut y = top as f32 + px;
        for l in &lines {
            cx.draw_text_clipped(Style::Italic, px, text_x as f32, y, l, fg, clip);
            y += px * 1.3;
        }
        for (r, tool) in rects.iter().zip(&self.tools) {
            SelectionCard::paint_tool(cx, *r, tool);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let UiEvent::Gesture(Gesture::Tap { pos }) = ev else {
            return match ev {
                UiEvent::Gesture(_) | UiEvent::Touch(_) => EventResult::Handled,
                _ => EventResult::Ignored,
            };
        };
        if self.more {
            let (back, pills, next) = self.more_rects(b);
            if back.contains(*pos) {
                self.more = false;
                self.first = 0;
            } else if let Some(t) = pills.iter().find(|(_, r)| r.contains(*pos)).and_then(|(i, _)| self.rest.get(*i)) {
                return EventResult::Action(t.action.clone());
            } else if next.is_some_and(|r| r.contains(*pos)) {
                let n = pills.len();
                self.first = if self.first + n >= self.rest.len() { 0 } else { self.first + n };
            }
            return EventResult::Handled;
        }
        match tool_rects(b, self.tools.len()).iter().zip(&self.tools).find(|(r, _)| r.contains(*pos)) {
            Some((_, t)) if t.action == "sel-more" => {
                self.more = true;
                self.first = 0;
                EventResult::Handled
            }
            Some((_, t)) => EventResult::Action(t.action.clone()),
            None => EventResult::Handled,
        }
    }
}

/// The popup for a finished selection on a `size` screen: a card next to the selection
/// with the quote and icon buttons (Highlight, Note, Insightful, plugin actions, Search,
/// More, Close) over a page scrim (`sel-cancel` on a tap on the page). `plugins`: labels
/// of all plugin selection actions (`plugin-sel:I`); `insightful`: the index of
/// Insightful's among them (its button takes the third place); tools that do not fit the
/// row are listed after "More". `above`: the card stays above this y (the top of the
/// dictionary card added next to it).
pub fn popup(sel: &Selection, plugins: &[String], insightful: Option<usize>, highlighted: bool, size: Size, above: Option<i32>) -> Overlay {
    let mut o = Overlay::over_page(OverlayKind::Selection, size, "sel-cancel");
    let anchor = bounds(&sel.rects).unwrap_or_default();
    let w = size.w.saturating_sub(48).min(CARD_W);
    let (tools, rest) = layout_tools(plugins, insightful, highlighted, w);
    let quote = quote(sel, 300);
    let h = SelectionCard::height(&tools, quote.chars().count() + 2, rest.len(), w);
    let room = Size::new(size.w, above.map_or(size.h, |y| (y - GAP as i32).clamp(0, size.h as i32) as u32));
    let mut frame = Popup::frame(anchor, Size::new(w, h), room);
    if let Some(y) = above {
        // Below the word there was no room above the card's top: keep clear of it.
        frame.y = frame.y.min(y - GAP as i32 - h as i32).max(0);
    }
    // Keep a margin to the screen edges.
    frame.x = frame.x.clamp(24.min(size.w.saturating_sub(w) as i32 / 2), (size.w.saturating_sub(w + 24) as i32).max(0));
    for (r, t) in tool_rects(frame, tools.len()).into_iter().zip(&tools) {
        o.buttons.push((t.action.clone(), r));
    }
    let card = SelectionCard { quote, tools, rest, more: false, first: 0 };
    o.ids.insert("card", o.tree.add(None, Box::new(card), frame));
    o
}

/// The note editor for a selection on a `size` screen: an app bar ("Add note", Save,
/// Close), the quote, the note field and the keyboard. Actions: `note-save`, `close`,
/// `keyboard` (keys typed; Enter saves), `focus:note`.
pub fn note_editor(quote: &str, size: Size) -> Overlay {
    use super::overlay::{Bar, Rule};
    use ink_ui::bars::APP_BAR_H;
    use ink_ui::{AppBar, BarAction, Keyboard, Label, TextInput};
    let mut o = Overlay::new(OverlayKind::Note);
    let root = o.tree.add(None, Box::new(Bar::new(Rule::None)), Rect::from_size(size));
    let w = size.w;
    let bar = AppBar::new("Add note").action(BarAction::text("Save", "note-save")).action(BarAction::icon(Icon::Close, "close"));
    o.app_bar(Some(root), bar, Rect::new(0, 0, w, APP_BAR_H));
    let quote_r = Rect::new(32, APP_BAR_H as i32 + 24, w.saturating_sub(64), 120);
    o.tree.add(Some(root), Box::new(Label::left(format!("\u{201c}{quote}\u{201d}"), 34.0).styled(Style::Italic)), quote_r);
    let field_r = Rect::new(32, quote_r.bottom() + 16, w.saturating_sub(64), 110);
    let mut field = TextInput::new("Type a note", "focus:note");
    field.focused = true;
    o.buttons.push(("focus:note".into(), field_r));
    o.ids.insert("field", o.tree.add(Some(root), Box::new(field), field_r));
    o.ids.insert("keyboard", o.tree.add(None, Box::new(Keyboard::new()), super::search::keyboard_rect(size)));
    o
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(bounds(&[]), None);
        assert_eq!(bounds(&[Rect::new(10, 10, 5, 5), Rect::new(0, 20, 5, 5)]), Some(Rect::new(0, 10, 15, 15)));
        let mut c = Canvas::new(4, 1);
        c.pixels = vec![255, 0, 255, 100];
        lighten(&mut c, &[Rect::new(0, 0, 3, 1)]);
        assert_eq!(c.pixels, vec![191, 0, 191, 100]);
        invert(&mut c, &[Rect::new(3, 0, 1, 1)]);
        assert_eq!(c.pixels[3], 155);
    }

    /// A line of text at y 300..350 starting at x 100, every character 20 px wide.
    /// `midpoint`: a point maps to the first character whose middle is right of it (as
    /// ink-html does); otherwise to the character containing it.
    fn line_probe(text: &'static str, midpoint: bool) -> impl FnMut(Point) -> Option<(String, Rect)> {
        let chars: Vec<char> = text.chars().collect();
        move |p: Point| {
            if !(300..350).contains(&p.y) {
                return None;
            }
            let n = chars.len() as i32;
            let i = if midpoint {
                (0..n).find(|&i| p.x < 100 + i * 20 + 10).unwrap_or(n - 1)
            } else {
                ((p.x - 100) / 20).clamp(0, n - 1)
            };
            let c = chars[i as usize];
            Some((c.to_string().trim().to_string(), Rect::new(100 + i * 20, 300, 20, 50)))
        }
    }

    /// The word [`word_span`] finds when pressing character `at` (pressed at `dx` px into it).
    fn word(text: &'static str, at: i32, dx: i32, midpoint: bool) -> String {
        let mut probe = line_probe(text, midpoint);
        let (a, b) = word_span(Point::new(100 + at * 20 + dx, 320), &mut probe).unwrap();
        let chars: Vec<char> = text.chars().collect();
        chars[((a.x - 100) / 20) as usize..=((b.x - 100) / 20) as usize].iter().collect()
    }

    #[test]
    fn words_are_whole_and_trimmed_under_both_point_rules() {
        let t = "He said: \u{201c}households,\u{201d} world's dogs' end.";
        for midpoint in [true, false] {
            for dx in [2, 10, 18] {
                // Middle of "households" (the old scan gave "seholds,").
                assert_eq!(word(t, 14, dx, midpoint), "households", "mid {midpoint} dx {dx}");
                // First and last character of the word.
                assert_eq!(word(t, 10, dx, midpoint), "households");
                assert_eq!(word(t, 19, dx, midpoint), "households");
                assert_eq!(word(t, 26, dx, midpoint), "world's");
                assert_eq!(word(t, 33, dx, midpoint), "dogs", "trailing apostrophe trimmed");
                // The last word of the line, pressed on its last letter.
                assert_eq!(word(t, 39, dx, midpoint), "end", "mid {midpoint} dx {dx}");
                assert_eq!(word(t, 0, dx, midpoint), "He");
            }
            // A press on the space after "said:" takes a word nearby.
            assert!(["said", "households"].contains(&word(t, 8, 10, midpoint).as_str()));
        }
    }

    /// Lines of text 50 px high from y 300, characters 20 px wide from x 100; a point
    /// maps to the nearest character boundary of the nearest line (as ink-html does).
    /// `~` is a hyphen the layout added, which the document's text leaves out.
    fn lines_probe(lines: &'static [&'static str]) -> impl FnMut(Point) -> Option<(String, Rect)> {
        move |p: Point| {
            let l = ((p.y - 300).max(0) / 50).min(lines.len() as i32 - 1);
            let chars: Vec<char> = lines[l as usize].chars().collect();
            let n = chars.len() as i32;
            let i = (0..n).find(|&i| p.x < 100 + i * 20 + 10).unwrap_or(n - 1);
            let c = chars[i as usize];
            let t = if c == '~' { String::new() } else { c.to_string().trim().to_string() };
            Some((t, Rect::new(100 + i * 20, 300 + l * 50, 20, 50)))
        }
    }

    /// The word [`word_span`] finds when pressing line `l`, character `at`, as
    /// "first line part|second line part".
    fn split_word(lines: &'static [&'static str], l: i32, at: i32) -> String {
        let mut probe = lines_probe(lines);
        let (a, b) = word_span(Point::new(100 + at * 20 + 5, 300 + l * 50 + 20), &mut probe).unwrap();
        let (la, ca) = ((a.y - 300) / 50, (a.x - 100) / 20);
        let (lb, cb) = ((b.y - 300) / 50, (b.x - 100) / 20);
        let line = |l: i32| lines[l as usize].chars().collect::<Vec<char>>();
        if la == lb {
            line(la)[ca as usize..=cb as usize].iter().collect()
        } else {
            let (x, y) = (line(la), line(lb));
            format!("{}|{}", x[ca as usize..].iter().collect::<String>(), y[..=cb as usize].iter().collect::<String>())
        }
    }

    #[test]
    fn a_word_hyphenated_at_a_line_end_is_selected_whole() {
        // The layout's hyphen: pressed on either part, both parts.
        let lines: &[&str] = &["Two of the in~", "finities here", "now."];
        assert_eq!(split_word(lines, 0, 12), "in~|finities");
        assert_eq!(split_word(lines, 0, 11), "in~|finities");
        assert_eq!(split_word(lines, 1, 0), "in~|finities");
        assert_eq!(split_word(lines, 1, 5), "in~|finities");
        // Other words on those lines stay on their line.
        assert_eq!(split_word(lines, 0, 5), "of");
        assert_eq!(split_word(lines, 1, 10), "here");
        assert_eq!(split_word(lines, 2, 1), "now");
        // A hyphen of the book at a line end joins the same way.
        let lines: &[&str] = &["a well-", "known one"];
        assert_eq!(split_word(lines, 0, 3), "well-|known");
        assert_eq!(split_word(lines, 1, 2), "well-|known");
        // A line that ends in a word does not join the next line's first word.
        let lines: &[&str] = &["the end", "of it"];
        assert_eq!(split_word(lines, 0, 5), "end");
        assert_eq!(split_word(lines, 1, 0), "of");
        // A dash after a space is not part of the word.
        let lines: &[&str] = &["the end -", "of it"];
        assert_eq!(split_word(lines, 0, 5), "end");
        assert_eq!(split_word(lines, 1, 0), "of");
    }

    /// Gives every ideograph a reading wider than the ideograph.
    struct WideReadings;

    impl ink_doc::TextAnnotator for WideReadings {
        fn id(&self) -> &str {
            "wide"
        }
        fn version(&self) -> u64 {
            1
        }
        fn wants(&self, _: &str, _lang: Option<&str>) -> bool {
            true
        }
        fn annotate(&self, p: &str, _: Option<&str>) -> Vec<ink_doc::Annotation> {
            p.char_indices()
                .filter(|&(_, c)| ('\u{4e00}'..='\u{9fff}').contains(&c))
                .map(|(i, c)| ink_doc::Annotation::over(i..i + c.len_utf8(), "aaaaaaaaaaaaaa"))
                .collect()
        }
    }

    /// A long press on a character selects the same characters, and the same text,
    /// whatever ruby the layout draws: none, the book's, or wide readings of an annotator
    /// (which move the characters apart). Compared by character, not by pixel.
    #[test]
    fn a_long_press_selects_the_same_characters_with_and_without_readings() {
        use ink_doc::Document;
        let book = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ink-html/tests/fixtures/ruby.epub");
        let mut words = Vec::new();
        for (ruby, annotate) in [(false, false), (true, false), (true, true)] {
            let mut d = ink_formats::open(&book).unwrap();
            let mut s = ink_doc::LayoutSettings::for_screen(1272, 1696);
            s.ruby = ruby;
            if annotate {
                s.annotators = vec![ink_doc::AnnotatorHandle::new(std::sync::Arc::new(WideReadings))];
            }
            d.set_layout(&s).unwrap();
            let (l, span) = d.page_layout(0).unwrap();
            let dy = s.margin_top - span.y0;
            // The centre of 依 (third poem: its book ruby is hidden, so the annotator's
            // readings go there).
            let at = l.items.iter().find_map(|i| match i {
                ink_html::layout::Draw::Text(t) if t.node != u32::MAX && t.text.contains('依') => {
                    let n = t.text.chars().count() as f32;
                    let k = t.text.chars().position(|c| c == '依').unwrap() as f32;
                    let cell = (t.width - t.letter_spacing * n) / n;
                    Some(Point::new((t.x + cell * (k + 0.5)) as i32, ((t.line_top + t.line_bottom) / 2.0 + dy) as i32))
                }
                _ => None,
            });
            let w = word_at(&mut d, 0, at.unwrap()).unwrap();
            words.push(w.text);
        }
        assert_eq!(words[0], "白日依山尽", "the run up to the comma");
        assert_eq!(words[1], words[0], "with the book's ruby");
        assert_eq!(words[2], words[0], "with wide readings");
    }

    /// In a book laid out by ink-html, a long press on either part of a word the layout
    /// hyphenated selects the whole word.
    #[test]
    fn a_long_press_selects_a_hyphenated_word_in_a_book() {
        use ink_doc::Document;
        let book = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../ink-html/tests/fixtures/lang.epub");
        let mut d = ink_formats::open(&book).unwrap();
        let mut s = ink_doc::LayoutSettings::for_screen(1272, 1696);
        s.hyphenate = true;
        d.set_layout(&s).unwrap();
        assert!(d.wait_paginated(std::time::Duration::from_secs(300)));
        let mut checked = 0;
        for p in 0..d.page_count().pages {
            let (l, span) = d.page_layout(p).unwrap();
            let a = l.slices[span.first as usize].first_item as usize;
            let b = l.slices[span.end as usize - 1].end_item as usize;
            let runs: Vec<_> = l.items[a..b].iter().filter_map(|i| if let ink_html::layout::Draw::Text(t) = i { Some(t.clone()) } else { None }).collect();
            let dy = 70.0 - span.y0;
            for w in runs.windows(2) {
                let (a, b) = (&w[0], &w[1]);
                // Runs are stretches of a line: the word is the last of one and the first of the next.
                let head = a.text.trim_end_matches('-').rsplit(' ').next().unwrap_or_default();
                let tail = b.text.split(' ').next().unwrap_or_default().trim_end_matches(|c: char| c.is_ascii_punctuation());
                let word = format!("{head}{tail}");
                // Words of letters (the book also has pages of noise).
                if !a.hyphenated || head.len() < 3 || tail.is_empty() || b.line_top <= a.line_top || !word.chars().all(|c| c.is_ascii_alphabetic()) {
                    continue;
                }
                let (ya, yb) = (((a.line_top + a.line_bottom) / 2.0 + dy) as i32, ((b.line_top + b.line_bottom) / 2.0 + dy) as i32);
                // The hyphen ends the line: a press just left of it is on the word's last letter.
                let hyphen = d.select(p, Point::new(LINE_END_X, ya), Point::new(LINE_END_X, ya)).unwrap().unwrap().rects[0];
                let first = Point::new(hyphen.x - 6, ya);
                let second = Point::new((b.x + 4.0) as i32, yb);
                for at in [first, second] {
                    let sel = word_at(&mut d, p, at).unwrap();
                    assert_eq!(sel.text, word, "page {p}: {:?} | {:?}", a.text, b.text);
                }
                checked += 1;
            }
            if checked >= 5 {
                break;
            }
        }
        assert!(checked >= 3, "{checked} hyphenated words in the book");
    }

    /// A paged text: page `p` has lines at y 100 + 60 l (50 px high), characters 20 px
    /// wide from x 100; a point maps to the character containing it (clamped to the
    /// page's text). Positions are `flow:page.line.col`.
    pub(in crate::screens::reader) struct PagedDoc {
        pages: Vec<Vec<&'static str>>,
        meta: ink_doc::Metadata,
        layout: ink_doc::LayoutSettings,
        selection: Option<Selection>,
        /// The last [`ink_doc::Document::set_read_ahead`] call, shared with the test.
        pub read_ahead: std::rc::Rc<std::cell::Cell<Option<bool>>>,
    }

    impl PagedDoc {
        fn new(pages: Vec<Vec<&'static str>>) -> Self {
            PagedDoc {
                pages,
                meta: Default::default(),
                layout: ink_doc::LayoutSettings::for_screen(1272, 1696),
                selection: None,
                read_ahead: Default::default(),
            }
        }
        fn at(&self, page: usize, p: Point) -> (usize, usize) {
            let lines = &self.pages[page];
            let l = ((p.y - 100).max(0) / 60).min(lines.len() as i32 - 1) as usize;
            let c = ((p.x - 100).max(0) / 20).min(lines[l].chars().count() as i32 - 1) as usize;
            (l, c)
        }
        fn pos(page: usize, l: usize, c: usize) -> ink_doc::Position {
            format!("flow:{page}.{l}.{c}").parse().unwrap()
        }
    }

    impl ink_doc::Document for PagedDoc {
        fn metadata(&self) -> &ink_doc::Metadata {
            &self.meta
        }
        fn kind(&self) -> ink_doc::DocKind {
            ink_doc::DocKind::Reflowable
        }
        fn set_layout(&mut self, _: &ink_doc::LayoutSettings) -> ink_doc::Result<()> {
            Ok(())
        }
        fn layout(&self) -> &ink_doc::LayoutSettings {
            &self.layout
        }
        fn page_count(&self) -> ink_doc::PageCount {
            ink_doc::PageCount { pages: self.pages.len(), complete: true }
        }
        fn render_page(&mut self, _: usize, _: &mut Canvas) -> ink_doc::Result<()> {
            Ok(())
        }
        fn position_of_page(&self, page: usize) -> ink_doc::Result<ink_doc::Position> {
            Ok(Self::pos(page, 0, 0))
        }
        fn page_of(&self, _: &ink_doc::Position) -> ink_doc::Result<usize> {
            Ok(0)
        }
        fn current_page(&self) -> usize {
            0
        }
        fn set_current_page(&mut self, _: usize) {}
        fn select(&mut self, page: usize, from: Point, to: Point) -> ink_doc::Result<Option<Selection>> {
            let (a, b) = (self.at(page, from), self.at(page, to));
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            let lines = &self.pages[page];
            let mut text = Vec::new();
            let mut rects = Vec::new();
            for (l, line) in lines.iter().enumerate().take(b.0 + 1).skip(a.0) {
                let c0 = if l == a.0 { a.1 } else { 0 };
                let c1 = if l == b.0 { b.1 + 1 } else { line.chars().count() };
                text.push(line.chars().skip(c0).take(c1 - c0).collect::<String>());
                rects.push(Rect::new(100 + 20 * c0 as i32, 100 + 60 * l as i32, 20 * (c1 - c0) as u32, 50));
            }
            let text = text.join("\n").trim().to_string();
            let sel = Selection { start: Self::pos(page, a.0, a.1), end: Self::pos(page, b.0, b.1 + 1), text, page, rects };
            self.selection = Some(sel.clone());
            Ok(Some(sel))
        }
        fn selection(&self) -> Option<&Selection> {
            self.selection.as_ref()
        }
        fn clear_selection(&mut self) {
            self.selection = None;
        }
        fn set_selection(&mut self, sel: Selection) {
            self.selection = Some(sel);
        }
        fn set_read_ahead(&mut self, on: bool) {
            self.read_ahead.set(Some(on));
        }
    }

    pub(in crate::screens::reader) fn three_pages() -> PagedDoc {
        PagedDoc::new(vec![
            vec!["Two households, both", "alike in dignity, In"],
            vec!["fair Verona, where we", "lay our scene, From"],
            vec!["ancient grudge break", "to new mutiny."],
        ])
    }

    /// The word at line `l`, column `c` of `page`.
    fn word_on(doc: &mut PagedDoc, page: usize, l: i32, c: i32) -> Word {
        word_hit(doc, page, Point::new(100 + 20 * c + 5, 100 + 60 * l + 20)).unwrap()
    }

    const SIZE: Size = Size::new(1272, 1696);

    #[test]
    fn a_selection_continues_on_the_next_page() {
        let mut doc = three_pages();
        // "alike" on page 0, line 1.
        let anchor = word_on(&mut doc, 0, 1, 2);
        assert_eq!(anchor.sel.text, "alike");
        // Turned to page 1: first its first word, then "where" on line 0.
        let first = edge_word(&mut doc, 1, SIZE, true);
        let SpanEnd::Word(w) = &first else { panic!("{first:?}") };
        assert_eq!(w.sel.text, "fair");
        let s = span_select(&mut doc, SIZE, 0, &anchor, 1, &first).unwrap();
        assert_eq!(s.sel.text, "alike in dignity, In\nfair");
        let end = word_on(&mut doc, 1, 0, 14);
        let s = span_select(&mut doc, SIZE, 0, &anchor, 1, &SpanEnd::Word(end)).unwrap();
        assert_eq!(s.sel.text, "alike in dignity, In\nfair Verona, where");
        // Document positions: from "alike" on page 0 to the end of "where" on page 1.
        assert_eq!(s.sel.start.to_string(), "flow:0.1.0");
        assert_eq!(s.sel.end.to_string(), "flow:1.0.18");
        // The part on the page shown carries the rectangles; both pages are listed.
        assert_eq!(s.sel.page, 1);
        assert_eq!(s.sel.rects, vec![Rect::new(100, 100, 360, 50)]);
        assert_eq!(s.pages.iter().map(|(p, _)| *p).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(s.pages[0].1, vec![Rect::new(100, 160, 400, 50)]);
        // The document's selection (what plugins read) is the whole of it.
        assert_eq!(ink_doc::Document::selection(&doc), Some(&s.sel));
        // Two turns: the page between is taken whole.
        let end = word_on(&mut doc, 2, 0, 1);
        let s = span_select(&mut doc, SIZE, 0, &anchor, 2, &SpanEnd::Word(end)).unwrap();
        assert_eq!(s.sel.text, "alike in dignity, In\nfair Verona, where we\nlay our scene, From\nancient");
        assert_eq!(s.pages.len(), 3);
    }

    #[test]
    fn a_selection_continues_on_the_previous_page() {
        let mut doc = three_pages();
        // "scene" on page 1, dragged back to page 0, where it starts at the last word.
        let anchor = word_on(&mut doc, 1, 1, 8);
        assert_eq!(anchor.sel.text, "scene");
        let last = edge_word(&mut doc, 0, SIZE, false);
        let SpanEnd::Word(w) = &last else { panic!("{last:?}") };
        assert_eq!(w.sel.text, "In");
        let s = span_select(&mut doc, SIZE, 1, &anchor, 0, &last).unwrap();
        assert_eq!(s.sel.text, "In\nfair Verona, where we\nlay our scene");
        let end = word_on(&mut doc, 0, 1, 10);
        let s = span_select(&mut doc, SIZE, 1, &anchor, 0, &SpanEnd::Word(end)).unwrap();
        assert_eq!(s.sel.text, "dignity, In\nfair Verona, where we\nlay our scene");
        assert_eq!(s.sel.start.to_string(), "flow:0.1.9");
        assert_eq!(s.sel.end.to_string(), "flow:1.1.13");
        assert_eq!((s.sel.page, s.pages[0].0, s.pages[1].0), (0, 0, 1));
        // Back on the anchor's page: a one-page selection again; the page edges as ends.
        let s = span_select(&mut doc, SIZE, 1, &anchor, 1, &SpanEnd::PageEnd).unwrap();
        assert_eq!((s.sel.text.as_str(), s.pages.len()), ("scene, From", 1));
        let s = span_select(&mut doc, SIZE, 1, &anchor, 1, &SpanEnd::PageStart).unwrap();
        assert_eq!(s.sel.text, "fair Verona, where we\nlay our scene");
    }

    #[test]
    fn turn_zones_are_off_the_text_and_at_the_edges() {
        let text = Some((100, 1500));
        // In the text: none.
        assert_eq!(edge_zone(Point::new(600, 800), SIZE, text), 0);
        assert_eq!(edge_zone(Point::new(600, 1510), SIZE, text), 0);
        // Past the last line, the bottom band and the bottom-right corner: next page.
        assert_eq!(edge_zone(Point::new(600, 1530), SIZE, text), 1);
        assert_eq!(edge_zone(Point::new(600, 1690), SIZE, Some((100, 1680))), 1);
        assert_eq!(edge_zone(Point::new(1200, 1600), SIZE, Some((100, 1680))), 1);
        // Above the first line, the top band and the top-left corner: previous page.
        assert_eq!(edge_zone(Point::new(600, 80), SIZE, text), -1);
        assert_eq!(edge_zone(Point::new(600, 10), SIZE, Some((0, 1680))), -1);
        assert_eq!(edge_zone(Point::new(50, 100), SIZE, Some((0, 1680))), -1);
        // The text box of the paged document.
        let mut doc = three_pages();
        assert_eq!(text_box(&mut doc, 0, SIZE), Some((100, 210)));
        assert!(ink_doc::Document::selection(&doc).is_none());
    }

    fn sel(text: &str) -> Selection {
        let p: ink_doc::Position = "page:0".parse().unwrap();
        Selection { start: p.clone(), end: p, page: 0, text: text.into(), rects: vec![Rect::new(100, 300, 400, 50)] }
    }

    fn tap(o: &mut Overlay, r: Rect) -> Option<String> {
        let pos = ink_device::Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2);
        match o.tree.dispatch(&UiEvent::Gesture(Gesture::Tap { pos }))?.1 {
            EventResult::Action(a) => Some(a),
            _ => None,
        }
    }

    #[test]
    fn card_row_fits_and_more_lists_every_plugin() {
        let size = Size::new(1272, 1696);
        // One plugin: Highlight, Note, the plugin, Search, Close (no More, no Copy).
        let mut o = popup(&sel("Romeo"), &["Look up".into()], None, false, size, None);
        let acts: Vec<&str> = o.buttons.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(acts, ["sel-highlight", "sel-note", "plugin-sel:0", "sel-search", "sel-cancel"]);
        for (a, r) in o.buttons.clone() {
            assert!(r.w >= 96 && r.h >= 96, "{a} {r:?}");
            assert_eq!(tap(&mut o, r).as_deref(), Some(a.as_str()));
        }
        // Eight plugins: two inline, More opens a page with the other six and Search (paged).
        let plugins: Vec<String> = (0..8).map(|i| format!("Action {i}")).collect();
        let mut o = popup(&sel("Romeo"), &plugins, None, false, size, None);
        let acts: Vec<String> = o.buttons.iter().map(|(a, _)| a.clone()).collect();
        assert_eq!(acts, ["sel-highlight", "sel-note", "plugin-sel:0", "plugin-sel:1", "sel-more", "sel-cancel"]);
        let more = o.button_rect("sel-more").unwrap();
        assert_eq!(tap(&mut o, more), None);
        let id = o.ids["card"];
        let b = o.tree.bounds(id).unwrap();
        assert!(b.bottom() <= size.h as i32);
        let mut seen = Vec::new();
        for _ in 0..4 {
            let card = o.tree.get_mut::<SelectionCard>(id).unwrap();
            assert!(card.more);
            let (_, pills, next) = card.more_rects(b);
            for (i, r) in &pills {
                assert!(r.bottom() <= b.bottom() && r.h >= 96);
                let want = if *i < 6 { format!("plugin-sel:{}", i + 2) } else { "sel-search".into() };
                assert_eq!(tap(&mut o, *r), Some(want));
                seen.push(*i);
            }
            match next {
                Some(r) => assert_eq!(tap(&mut o, r), None),
                None => break,
            }
        }
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen, (0..7).collect::<Vec<_>>());
        // Back returns to the row.
        let back = o.tree.get_mut::<SelectionCard>(id).unwrap().more_rects(b).0;
        tap(&mut o, back);
        assert!(!o.tree.get_mut::<SelectionCard>(id).unwrap().more);
        // Landscape and a narrow screen keep at least four tools of 96 px.
        for size in [Size::new(1696, 1272), Size::new(600, 800)] {
            let o = popup(&sel("x"), &plugins, None, false, size, None);
            assert!(o.buttons.iter().all(|(_, r)| r.w >= 96 && r.right() <= size.w as i32), "{size:?} {:?}", o.buttons);
        }
    }

    #[test]
    fn insightful_takes_the_third_place_and_there_is_no_copy() {
        let size = Size::new(1272, 1696);
        let plugins: Vec<String> = vec!["Look up".into(), "Insightful".into()];
        let mut o = popup(&sel("Romeo"), &plugins, Some(1), false, size, None);
        let acts: Vec<&str> = o.buttons.iter().map(|(a, _)| a.as_str()).collect();
        // Insightful's own plugin button is not listed twice.
        assert_eq!(acts, ["sel-highlight", "sel-note", "plugin-sel:1", "plugin-sel:0", "sel-search", "sel-cancel"]);
        let id = o.ids["card"];
        let labels: Vec<String> = o.tree.get_mut::<SelectionCard>(id).unwrap().tools.iter().map(|t| t.label.clone()).collect();
        assert_eq!(labels, ["Highlight", "Note", "Insightful", "Look up", "Search", "Close"]);
        assert!(!labels.iter().any(|l| l == "Copy"));
        let r = o.button_rect("plugin-sel:1").unwrap();
        assert_eq!(tap(&mut o, r).as_deref(), Some("plugin-sel:1"));
        // Not loaded: no Insightful button.
        assert!(!all_tools(&plugins[..1], None, false).iter().any(|t| t.label == "Insightful"));
        // Kept above a card whose top is at y 900, even for a word under it.
        for y in [400, 1200] {
            let mut s = sel("Romeo");
            s.rects = vec![Rect::new(100, y, 400, 50)];
            let o = popup(&s, &plugins, Some(1), false, size, Some(900));
            let b = o.tree.bounds(o.ids["card"]).unwrap();
            assert!(b.bottom() <= 900 && b.y >= 0, "word at {y}: {b:?}");
        }
    }
}
