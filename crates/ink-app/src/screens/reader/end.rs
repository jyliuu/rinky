//! The end of a book: a card over the last page when the reader turns past it.
//!
//! The card shows the title and author, "Mark as finished" (the sidecar's reading
//! status), a 1-5 star rating (the sidecar's `rating`, KOReader's `summary.rating`),
//! "Open next" with the next book of the series or else the unfinished book read most
//! recently, and "Back to book" / "Back to library". It is an [`OverlayKind::End`]
//! built like the other reader overlays: a [`PageScrim`] (a tap beside the card goes
//! back to the page) under an [`ink_ui::Card`] frame with buttons on it.
//!
//! Refresh: the card appears with one GC16 (it covers most of the page). A tap on a
//! star or on "Mark as finished" rebuilds the card; only the pixels that changed are
//! refreshed, without a flash. Closing puts the page back from the page buffer.
//!
//! Actions: `end-finished` (toggle), `end-star:N`, `end-next`, `end-library`, `close`.

use super::overlay::{Overlay, OverlayKind, PageScrim};
use super::ReaderScreen;
use crate::router::{AppCtx, Route, Transition};
use ink_device::{Rect, RefreshClass, Size};
use ink_store::{BookState, LibraryIndex, ReadingStatus};
use ink_text::Style;
use ink_ui::theme::space;
use ink_ui::{Button, Card, Icon, Label};
use std::path::{Path, PathBuf};

/// Size of the title lines.
pub const TITLE_PX: f32 = 52.0;
/// At most this many title lines.
pub const TITLE_LINES: usize = 3;
/// Widest card.
const CARD_MAX_W: u32 = 1000;
const PAD: i32 = space(5);
const BUTTON_H: u32 = 108;
const STAR: u32 = 108;
const STAR_GAP: u32 = space(3) as u32;
const CAPTION_H: u32 = 56;
const TITLE_H: u32 = 70;
/// The "Open next" card: title and reason, plus a progress bar for a book started.
const NEXT_H: u32 = 150;
const NEXT_BAR_H: u32 = 60;

fn next_height(next: &NextBook) -> u32 {
    NEXT_H + if next.progress.is_some() { NEXT_BAR_H } else { 0 }
}

/// The book the card suggests reading next.
#[derive(Clone, Debug, PartialEq)]
pub struct NextBook {
    /// Its file.
    pub path: PathBuf,
    /// Its title.
    pub title: String,
    /// Why it is suggested ("Next in Animals · book 2", "Continue reading · 40%").
    pub why: String,
    /// Its reading progress, for a book already started.
    pub progress: Option<f32>,
}

/// What the card shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EndInfo {
    /// The title, already wrapped (at most [`TITLE_LINES`]).
    pub title: Vec<String>,
    /// Authors, comma separated (may be empty).
    pub author: String,
    /// The sidecar says the book is finished.
    pub finished: bool,
    /// The rating 1..=5, if any.
    pub rating: Option<u8>,
    /// The suggestion for "Open next".
    pub next: Option<NextBook>,
}

/// Width of the card on a screen of `size`.
pub fn card_width(size: Size) -> u32 {
    size.w.saturating_sub(2 * space(6) as u32).min(CARD_MAX_W)
}

/// Width available to the title lines on a screen of `size`.
pub fn title_width(size: Size) -> f32 {
    card_width(size).saturating_sub(2 * PAD as u32) as f32
}

fn index_text(i: f32) -> String {
    if i.fract() == 0.0 {
        format!("{}", i as i64)
    } else {
        format!("{i}")
    }
}

/// The book to suggest after `current`: the book after it in its series (`series`:
/// name and position; from the library index, else from the book itself), the lowest
/// position above this one that is not finished; else the unfinished book opened most
/// recently. Books the reader cannot open (DRM, KFX) are never suggested.
pub fn pick_next(index: &LibraryIndex, current: &Path, series: Option<(&str, f32)>) -> Option<NextBook> {
    let usable = |b: &&ink_store::BookEntry| b.path != current && !b.unsupported && b.status != ReadingStatus::Complete;
    if let Some((name, at)) = series {
        let key = name.trim().to_lowercase();
        let next = index
            .books
            .iter()
            .filter(usable)
            .filter(|b| b.series.as_deref().is_some_and(|s| s.trim().to_lowercase() == key))
            .filter(|b| b.series_index.is_some_and(|i| i > at))
            .min_by(|a, b| a.series_index.unwrap_or(0.0).total_cmp(&b.series_index.unwrap_or(0.0)));
        if let Some(b) = next {
            let why = match b.series_index {
                Some(i) => format!("Next in {} · book {}", name.trim(), index_text(i)),
                None => format!("Next in {}", name.trim()),
            };
            return Some(NextBook { path: b.path.clone(), title: b.title.clone(), why, progress: b.percent.filter(|p| *p > 0.0) });
        }
    }
    let recent = index.recent(usize::MAX).into_iter().filter(usable).find(|b| b.status != ReadingStatus::Abandoned)?;
    let why = match recent.percent.filter(|p| *p > 0.0) {
        Some(p) => format!("Continue reading · {}%", (p.clamp(0.0, 1.0) * 100.0 + 1e-3).floor()),
        None => "Continue reading".to_string(),
    };
    Some(NextBook { path: recent.path.clone(), title: recent.title.clone(), why, progress: recent.percent.filter(|p| *p > 0.0) })
}

/// Height of the card for `info`.
fn card_height(info: &EndInfo) -> u32 {
    let title = TITLE_H * info.title.len().max(1) as u32;
    let author = if info.author.is_empty() { 0 } else { CAPTION_H };
    let next = info.next.as_ref().map_or(0, |n| CAPTION_H + next_height(n) + space(4) as u32);
    2 * PAD as u32 + CAPTION_H + title + author + space(3) as u32 + BUTTON_H + space(4) as u32 + CAPTION_H + STAR + space(5) as u32 + next + BUTTON_H
}

/// What fits on a screen `h` pixels high: on a short screen (a 6" panel turned to
/// landscape) the suggestion loses its progress bar, then the title its last lines, then
/// the card its suggestion, so that the buttons stay on the screen.
fn fit(info: &EndInfo, h: u32) -> EndInfo {
    let mut info = info.clone();
    while card_height(&info) > h {
        if let Some(n) = info.next.as_mut().filter(|n| n.progress.is_some()) {
            n.progress = None;
        } else if info.title.len() > 1 {
            info.title.pop();
            if let Some(last) = info.title.last_mut() {
                last.push('…');
            }
        } else if info.next.is_some() {
            info.next = None;
        } else {
            break;
        }
    }
    info
}

/// Builds the card for a screen of `size`.
pub fn build(info: &EndInfo, size: Size) -> Overlay {
    let info = &fit(info, size.h);
    let mut o = Overlay::new(OverlayKind::End);
    o.tree.add(None, Box::new(PageScrim { action: "close".into() }), Rect::from_size(size));
    let w = card_width(size);
    let h = card_height(info).min(size.h);
    let frame = Rect::new(((size.w - w) / 2) as i32, (size.h.saturating_sub(h) / 2) as i32, w, h);
    // The frame: taps on its empty parts are taken (they do not close the card).
    let root = o.tree.add(None, Box::new(Card::new("", "")), frame);
    o.ids.insert("card", root);
    let inner_w = w.saturating_sub(2 * PAD as u32);
    let x = frame.x + PAD;
    let mut y = frame.y + PAD;
    let row = |y: i32, h: u32| Rect::new(x, y, inner_w, h);
    o.tree.add(Some(root), Box::new(Label::new("The end", 36.0).styled(Style::Italic)), row(y, CAPTION_H));
    y += CAPTION_H as i32;
    for line in &info.title {
        o.tree.add(Some(root), Box::new(Label::new(line.clone(), TITLE_PX).styled(Style::Bold)), row(y, TITLE_H));
        y += TITLE_H as i32;
    }
    if info.title.is_empty() {
        y += TITLE_H as i32;
    }
    if !info.author.is_empty() {
        o.tree.add(Some(root), Box::new(Label::new(info.author.clone(), 36.0)), row(y, CAPTION_H));
        y += CAPTION_H as i32;
    }
    y += space(3);
    // "Mark as finished"; once finished, a filled "Finished" (a tap undoes it).
    let fin = if info.finished { Button::filled("Finished", "end-finished") } else { Button::new("Mark as finished", "end-finished") }.with_icon(Icon::Check);
    let fw = inner_w.min(560);
    let id = o.button(Some(root), fin, Rect::new(frame.x + (w - fw) as i32 / 2, y, fw, BUTTON_H));
    o.ids.insert("finished", id);
    y += (BUTTON_H + space(4) as u32) as i32;
    o.tree.add(Some(root), Box::new(Label::new("Your rating", 32.0)), row(y, CAPTION_H));
    y += CAPTION_H as i32;
    // Five stars: the ones up to the rating are filled.
    let stars_w = 5 * STAR + 4 * STAR_GAP;
    let sx = frame.x + (w - stars_w) as i32 / 2;
    let rating = info.rating.unwrap_or(0);
    for n in 1..=5u8 {
        let action = format!("end-star:{n}");
        let b = if n <= rating { Button::filled("", action) } else { Button::new("", action) }.with_icon(Icon::Star);
        let r = Rect::new(sx + ((n as u32 - 1) * (STAR + STAR_GAP)) as i32, y, STAR, STAR);
        o.button(Some(root), b, r);
    }
    y += (STAR + space(5) as u32) as i32;
    if let Some(next) = &info.next {
        o.tree.add(Some(root), Box::new(Label::left("Open next", 32.0)), row(y, CAPTION_H));
        y += CAPTION_H as i32;
        let mut card = Card::new(next.title.clone(), "end-next").subtitle(next.why.clone());
        if let Some(p) = next.progress {
            card = card.progress(p);
        }
        let r = row(y, next_height(next));
        o.buttons.push(("end-next".into(), r));
        o.tree.add(Some(root), Box::new(card), r);
        y += (r.h + space(4) as u32) as i32;
    }
    let cols = ink_ui::widget::split_cols(row(y, BUTTON_H), 2, space(3) as u32);
    o.button(Some(root), Button::new("Back to book", "close"), cols[0]);
    o.button(Some(root), Button::filled("Back to library", "end-library"), cols[1]);
    o
}

impl ReaderScreen {
    /// True when a turn forward from the page shown would go past the end of the book:
    /// the last page, with the page count complete.
    pub(super) fn at_book_end(&self) -> bool {
        self.doc.as_deref().is_some_and(|d| {
            let c = d.page_count();
            c.complete && c.pages > 0 && self.page + 1 >= c.pages
        })
    }

    /// Reads what the end card shows: title and author from the book, status and rating
    /// from the sidecar, the suggestion from the library index.
    fn end_info(&self, cx: &mut AppCtx) -> EndInfo {
        let size = cx.screen.size();
        let meta = self.doc.as_deref().map(|d| d.metadata().clone()).unwrap_or_default();
        let title = if meta.title.trim().is_empty() { self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default() } else { meta.title.trim().to_string() };
        // Measured in the regular face; the bold title is a little wider.
        let lines = super::wrap_text(cx.fonts, &title, TITLE_PX, title_width(size) * 0.9, TITLE_LINES);
        let state = BookState::load(&self.path).unwrap_or_default();
        let index = LibraryIndex::load(&cx.paths.library()).unwrap_or_default();
        let entry = index.get(&self.path);
        let series = entry.and_then(|b| Some((b.series.clone()?, b.series_index.unwrap_or(0.0)))).or(meta.series.clone());
        let next = pick_next(&index, &self.path, series.as_ref().map(|(s, i)| (s.as_str(), *i)));
        EndInfo { title: lines, author: meta.authors.join(", "), finished: state.status == ReadingStatus::Complete, rating: state.rating.filter(|r| (1..=5).contains(r)), next }
    }

    /// Opens the end card with one GC16 (the page with the card in a single update).
    pub(super) fn open_end(&mut self, cx: &mut AppCtx) {
        let info = self.end_info(cx);
        log::info!("reader: end of the book (finished {}, rating {:?}, next {:?})", info.finished, info.rating, info.next.as_ref().map(|n| &n.path));
        let o = build(&info, cx.screen.size());
        self.end = Some(info);
        self.open_overlay(o, cx);
        self.dirty = Some(RefreshClass::Gc16);
    }

    /// The card again after a change (only the changed pixels are refreshed).
    fn rebuild_end(&mut self, cx: &mut AppCtx) {
        let Some(info) = self.end.as_ref() else { return };
        let o = build(info, cx.screen.size());
        self.open_overlay(o, cx);
    }

    /// Acts on an `end-` action of the card (`name`, `arg` split at the colon).
    pub(super) fn end_action(&mut self, name: &str, arg: &str, cx: &mut AppCtx) -> Option<Transition> {
        match name {
            "end-finished" => {
                let finished = !self.end.as_ref().is_some_and(|e| e.finished);
                let status = if finished { ReadingStatus::Complete } else { ReadingStatus::Reading };
                if self.update_sidecar("status", |s| s.status = status) {
                    log::info!("reader: book marked {status:?}");
                    if let Some(e) = self.end.as_mut() {
                        e.finished = finished;
                    }
                    self.rebuild_end(cx);
                }
            }
            "end-star" => {
                let n = arg.parse::<u8>().ok().filter(|n| (1..=5).contains(n))?;
                if self.update_sidecar("rating", |s| s.rating = Some(n)) {
                    log::info!("reader: book rated {n}");
                    if let Some(e) = self.end.as_mut() {
                        e.rating = Some(n);
                    }
                    self.rebuild_end(cx);
                }
            }
            "end-next" => {
                let next = self.end.as_ref().and_then(|e| e.next.clone())?;
                log::info!("reader: open next book {}", next.path.display());
                crate::screens::library::note_opened(&next.path);
                return Some(Transition::Replace(Route::Reader(next.path)));
            }
            "end-library" => return Some(Transition::Pop),
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_store::BookEntry;

    fn book(path: &str, title: &str, series: Option<(&str, f32)>) -> BookEntry {
        BookEntry {
            path: PathBuf::from(path),
            title: title.into(),
            series: series.map(|(s, _)| s.to_string()),
            series_index: series.map(|(_, i)| i),
            format: "epub".into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_next_book_of_the_series_comes_first() {
        let mut index = LibraryIndex::default();
        index.books.push(book("/b/one.epub", "One", Some(("Animals", 1.0))));
        index.books.push(book("/b/three.epub", "Three", Some(("Animals", 3.0))));
        index.books.push(book("/b/two.epub", "Two", Some(("animals ", 2.0))));
        let mut other = book("/b/other.epub", "Other", None);
        other.last_opened = Some(100);
        index.books.push(other);
        let n = pick_next(&index, Path::new("/b/one.epub"), Some(("Animals", 1.0))).unwrap();
        assert_eq!((n.title.as_str(), n.why.as_str()), ("Two", "Next in Animals · book 2"));
        // A finished next volume is skipped.
        index.books[2].status = ReadingStatus::Complete;
        let n = pick_next(&index, Path::new("/b/one.epub"), Some(("Animals", 1.0))).unwrap();
        assert_eq!(n.title, "Three");
        // The last of the series: the unfinished book read most recently.
        let n = pick_next(&index, Path::new("/b/three.epub"), Some(("Animals", 3.0))).unwrap();
        assert_eq!((n.title.as_str(), n.why.as_str()), ("Other", "Continue reading"));
    }

    #[test]
    fn otherwise_the_most_recent_unfinished_book() {
        let mut index = LibraryIndex::default();
        let mut add = |p: &str, opened: Option<u64>, status: ReadingStatus, percent: Option<f32>, unsupported: bool| {
            let mut b = book(p, p, None);
            (b.last_opened, b.status, b.percent, b.unsupported) = (opened, status, percent, unsupported);
            index.books.push(b);
        };
        add("cur", Some(900), ReadingStatus::Reading, Some(1.0), false);
        add("done", Some(800), ReadingStatus::Complete, Some(1.0), false);
        add("drm", Some(700), ReadingStatus::Reading, None, true);
        add("aside", Some(650), ReadingStatus::Abandoned, Some(0.2), false);
        add("older", Some(600), ReadingStatus::Reading, Some(0.4), false);
        add("never", None, ReadingStatus::New, None, false);
        let n = pick_next(&index, Path::new("cur"), None).unwrap();
        assert_eq!((n.title.as_str(), n.why.as_str(), n.progress), ("older", "Continue reading · 40%", Some(0.4)));
        index.books.retain(|b| b.title != "older");
        assert_eq!(pick_next(&index, Path::new("cur"), None), None, "never opened books are not suggested");
    }

    #[test]
    fn the_card_has_every_action_and_fits_the_screen() {
        let info = EndInfo {
            title: vec!["Romeo and Juliet".into()],
            author: "William Shakespeare".into(),
            finished: false,
            rating: Some(3),
            next: Some(NextBook { path: "/b/n.epub".into(), title: "Hamlet".into(), why: "Continue reading · 12%".into(), progress: Some(0.12) }),
        };
        for size in [Size::new(1272, 1696), Size::new(1696, 1272), Size::new(1072, 1448)] {
            let o = build(&info, size);
            let card = o.tree.bounds(o.ids["card"]).unwrap();
            assert!(card.y >= 0 && card.bottom() <= size.h as i32, "{size:?}: {card:?}");
            for a in ["end-finished", "end-star:1", "end-star:5", "end-next", "end-library", "close"] {
                let r = o.button_rect(a).unwrap_or_else(|| panic!("{a}"));
                assert!(card.contains(ink_device::Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2)), "{a} inside the card on {size:?}");
                assert!(r.h >= 96, "{a} is a touch target");
            }
        }
        // A three-line title on a 6" panel turned to landscape: still every button on it.
        let long = EndInfo { title: vec!["A title".into(), "of three".into(), "lines".into()], ..info.clone() };
        for size in [Size::new(1448, 1072), Size::new(1024, 758)] {
            let o = build(&long, size);
            let card = o.tree.bounds(o.ids["card"]).unwrap();
            for a in ["end-finished", "end-star:1", "end-star:5", "end-library", "close"] {
                let r = o.button_rect(a).unwrap_or_else(|| panic!("{a}"));
                assert!(r.y >= card.y && r.bottom() <= card.bottom() && card.bottom() <= size.h as i32, "{a} on {size:?}: {r:?} in {card:?}");
            }
            if let Some(r) = o.button_rect("end-next") {
                assert!(r.bottom() <= card.bottom(), "end-next on {size:?}");
            }
        }
        assert_eq!(fit(&long, 1072).next.unwrap().progress, None, "the progress bar goes first");
        assert_eq!(fit(&long, 1696), long, "nothing is left out where it fits");
        let o = build(&EndInfo { next: None, ..info }, Size::new(1272, 1696));
        assert!(o.button_rect("end-next").is_none());
    }
}
