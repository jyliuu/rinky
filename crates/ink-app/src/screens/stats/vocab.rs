//! The Vocabulary screen: looked-up words (newest first, in pages) and flashcards for the
//! words that are due ("Know" / "Don't know", schedule in `ink_stats::vocab`). A card
//! shows the word large; a tap on it flips it to the meaning and the context (only the
//! card is refreshed).

use super::{header, now_and_offset, pager, HEADER_H, MARGIN, PAGER_H};
use crate::screens::common::{input, paint_cx, pill, Input};
use crate::router::{AppCtx, AppScreen, Transition};
use ink_device::{Key, Rect, RefreshClass, Size};
use ink_stats::{VocabDb, VocabWord};
use ink_text::Style;
use ink_ui::draw::{fill_round_rect, stroke_round_rect};
use ink_ui::theme::space;
use ink_ui::dialog::wrap_lines;
use ink_ui::{Damage, PaintCx, ProgressBar, UiEvent, Widget};
use std::path::PathBuf;

/// Height of one word row.
const ROW_H: i32 = 160;
/// Top of the flashcard.
const CARD_Y: i32 = HEADER_H + 88;
/// Height of the answer buttons.
const ANSWER_H: i32 = 120;
/// Words per practice round at most.
pub const ROUND: usize = 50;

#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Back,
    Practice,
    PrevPage,
    NextPage,
    Show,
    Know(bool),
}

/// A practice round.
#[derive(Clone, Debug, Default, PartialEq)]
struct Round {
    queue: Vec<VocabWord>,
    /// Cards answered "know".
    done: usize,
    /// Size of the round at the start.
    total: usize,
    shown: bool,
}

/// The Vocabulary screen.
#[derive(Default)]
pub struct VocabScreen {
    path: PathBuf,
    /// The open database (opened on first use once the file exists; kept, so a page
    /// flip or an answer costs no open).
    db: Option<VocabDb>,
    words: Vec<VocabWord>,
    count: usize,
    due: usize,
    page: usize,
    round: Option<Round>,
    error: Option<String>,
    hits: Vec<(Rect, Hit)>,
    dirty: Option<RefreshClass>,
    /// Only this part changed (a card flip).
    dirty_rect: Option<Rect>,
}

/// The flashcard rectangle on a `w` x `h` screen.
fn card_rect(w: i32, h: i32) -> Rect {
    Rect::new(MARGIN, CARD_Y, (w - 2 * MARGIN) as u32, (h - CARD_Y - ANSWER_H - 2 * space(4)) as u32)
}

/// The answer buttons: Don't know, Know.
fn answer_rects(w: i32, h: i32) -> (Rect, Rect) {
    let y = h - ANSWER_H - space(4);
    let bw = (w - 2 * MARGIN - space(4)) / 2;
    (Rect::new(MARGIN, y, bw as u32, ANSWER_H as u32), Rect::new(MARGIN + bw + space(4), y, bw as u32, ANSWER_H as u32))
}

/// `…before word after…` from a word's context.
pub fn context_line(w: &VocabWord) -> String {
    let prev = w.prev_context.as_deref().unwrap_or("").trim();
    let next = w.next_context.as_deref().unwrap_or("").trim();
    if prev.is_empty() && next.is_empty() {
        return String::new();
    }
    let before = if prev.is_empty() { String::new() } else { format!("…{prev} ") };
    let after = if next.is_empty() { String::new() } else { format!(" {next}…") };
    format!("{before}{}{after}", w.word)
}

impl VocabScreen {
    fn rows_per_page(h: i32) -> usize {
        ((h - HEADER_H - space(2) - PAGER_H - 2 * space(3)) / ROW_H).max(1) as usize
    }

    /// Runs `f` with the database, opening it first if needed; `None` when it cannot be
    /// opened.
    fn with_db<R>(&mut self, f: impl FnOnce(&mut Self, &VocabDb) -> R) -> Option<R> {
        let db = match self.db.take() {
            Some(db) => db,
            None => match VocabDb::open_or_recover(&self.path) {
                Ok((db, _)) => db,
                Err(e) => {
                    log::warn!("vocabulary: {}: {e}", self.path.display());
                    return None;
                }
            },
        };
        let r = f(self, &db);
        self.db = Some(db);
        Some(r)
    }

    /// Reads the counts and the current page of words.
    fn load(&mut self, now: u64, h: i32) {
        self.error = None;
        if !self.path.exists() {
            (self.words, self.count, self.due) = (Vec::new(), 0, 0);
            return;
        }
        let per = Self::rows_per_page(h);
        let r = self.with_db(|s, db| -> ink_stats::rusqlite::Result<()> {
            s.count = db.count()?;
            s.due = db.due_count(now)?;
            let pages = s.count.div_ceil(per).max(1);
            s.page = s.page.min(pages - 1);
            s.words = db.list(s.page * per, per)?;
            Ok(())
        });
        match r {
            None => self.error = Some("Cannot open the vocabulary database".into()),
            Some(Err(e)) => self.error = Some(format!("Cannot read the vocabulary: {e}")),
            Some(Ok(())) => {}
        }
    }

    fn start_round(&mut self, now: u64) {
        let Some(due) = self.with_db(|_, db| db.due(now, ROUND)) else { return };
        match due {
            Ok(queue) if !queue.is_empty() => {
                let total = queue.len();
                self.round = Some(Round { queue, done: 0, total, shown: false });
            }
            Ok(_) => {}
            Err(e) => self.error = Some(format!("Cannot read the vocabulary: {e}")),
        }
    }

    /// Answers the card on top: "know" writes the review and takes the card out of the
    /// round; "don't know" writes the review and puts the card at the end.
    fn answer(&mut self, know: bool, now: u64) {
        let Some(card) = self.round.as_mut().filter(|r| !r.queue.is_empty()).map(|r| r.queue.remove(0)) else { return };
        if let Some(Err(e)) = self.with_db(|_, db| db.answer(&card.word, know, now)) {
            log::warn!("vocabulary: answer {}: {e}", card.word);
        }
        let Some(round) = self.round.as_mut() else { return };
        if know {
            round.done += 1;
        } else {
            round.queue.push(card);
        }
        round.shown = false;
    }

    fn act(&mut self, hit: Hit, now: u64, size: Size) -> Transition {
        let h = size.h as i32;
        match hit {
            Hit::Back if self.round.is_some() => {
                self.round = None;
                self.load(now, h);
            }
            Hit::Back => return Transition::Pop,
            Hit::Practice => self.start_round(now),
            Hit::PrevPage => {
                self.page = self.page.saturating_sub(1);
                self.load(now, h);
            }
            Hit::NextPage => {
                self.page += 1;
                self.load(now, h);
            }
            Hit::Show => {
                // Flip the card (both ways); only the card is refreshed.
                if let Some(r) = self.round.as_mut() {
                    r.shown = !r.shown;
                    self.dirty_rect = Some(card_rect(size.w as i32, h));
                }
                self.dirty = Some(RefreshClass::Gl16);
                return Transition::None;
            }
            Hit::Know(k) => self.answer(k, now),
        }
        self.dirty = Some(RefreshClass::Gl16);
        self.dirty_rect = None;
        Transition::None
    }

    fn draw(&mut self, p: &mut PaintCx) {
        let (w, h) = (p.canvas.width as i32, p.canvas.height as i32);
        self.hits.clear();
        p.canvas.clear(p.theme.bg);
        if self.round.is_some() {
            self.draw_card(p, w, h);
        } else {
            self.draw_list(p, w, h);
        }
    }

    fn draw_list(&mut self, p: &mut PaintCx, w: i32, h: i32) {
        let practice = format!("Practice ({})", self.due);
        let right = (self.due > 0).then_some((practice.as_str(), Hit::Practice));
        header(p, "Vocabulary", right, Hit::Back, &mut self.hits);
        let t = p.theme.clone();
        let tw = (w - 2 * MARGIN) as u32;
        let top = HEADER_H + space(2);
        if let Some(e) = self.error.clone() {
            p.text_left(Rect::new(MARGIN, top, tw, 60), &e, t.caption.px, t.caption.style, t.fg);
            return;
        }
        if self.words.is_empty() {
            let r = Rect::new(MARGIN, top + 60, tw, 80);
            p.text_left(r, "No words yet. Tap Add to vocabulary in the dictionary popup.", t.body.px, t.body.style, t.muted);
            return;
        }
        for (i, wd) in self.words.iter().enumerate() {
            let y = top + i as i32 * ROW_H;
            if i > 0 {
                p.canvas.fill_rect(MARGIN, y - t.rule_px / 2, w - 2 * MARGIN, t.rule_px, t.light);
            }
            // Word (bold) with the lookup count and the review streak on the right.
            let count = (wd.lookup_count > 1).then(|| format!("×{}", wd.lookup_count));
            let streak = (wd.streak_count > 0).then(|| format!("known {}×", wd.streak_count));
            let right = count.into_iter().chain(streak).collect::<Vec<_>>().join(" · ");
            let rw = if right.is_empty() { 0.0 } else { p.text_width(t.caption.style, t.caption.px, &right) + space(3) as f32 };
            let word = p.ellipsize(&wd.word, t.body.px, Style::Bold, tw as f32 - rw);
            p.text_left(Rect::new(MARGIN, y + 12, tw, 56), &word, t.body.px, Style::Bold, t.fg);
            if !right.is_empty() {
                let base = p.baseline_in(Rect::new(0, y + 12, 10, 56), t.caption.px, t.caption.style);
                let x = (w - MARGIN) as f32 - p.text_width(t.caption.style, t.caption.px, &right);
                p.draw_text(t.caption.style, t.caption.px, x, base, &right, t.muted);
            }
            let detail = wd.definition.clone().unwrap_or_else(|| context_line(wd));
            let detail = p.ellipsize(&detail, t.caption.px, t.caption.style, tw as f32);
            p.text_left(Rect::new(MARGIN, y + 70, tw, 40), &detail, t.caption.px, t.caption.style, t.fg);
            let book = p.ellipsize(wd.title.as_deref().unwrap_or(""), t.caption.px, Style::Italic, tw as f32);
            p.text_left(Rect::new(MARGIN, y + 112, tw, 40), &book, t.caption.px, Style::Italic, t.muted);
        }
        let per = Self::rows_per_page(h);
        let pages = self.count.div_ceil(per).max(1);
        let extra = format!(" · {} words", self.count);
        pager(p, self.page, pages, &extra, Hit::PrevPage, Hit::NextPage, &mut self.hits);
    }

    fn draw_card(&mut self, p: &mut PaintCx, w: i32, h: i32) {
        let Some(round) = self.round.clone() else { return };
        let done = round.done.min(round.total);
        header(p, "Practice", None, Hit::Back, &mut self.hits);
        let t = p.theme.clone();
        let tw = (w - 2 * MARGIN) as u32;
        // Round progress under the header.
        let pr = Rect::new(MARGIN, HEADER_H + space(3), tw, 48);
        ProgressBar::thin(done as f32 / round.total.max(1) as f32).with_label(format!("{done} / {}", round.total)).paint(p, pr);
        let Some(card) = round.queue.first() else {
            let y = h / 3;
            p.text_centered(Rect::new(MARGIN, y, tw, 100), "All done", t.title.px * 1.3, Style::Bold, t.fg);
            let msg = format!("{} words reviewed. The next ones are due later.", round.total);
            p.text_centered(Rect::new(MARGIN, y + 120, tw, 60), &msg, t.label.px, t.label.style, t.muted);
            let r = Rect::new(w / 2 - 220, y + 240, 440, 104);
            pill(p, r, "Back to the list", None, true);
            self.hits.push((r, Hit::Back));
            return;
        };
        let cr = card_rect(w, h);
        self.paint_card(p, cr, card, round.shown);
        self.hits.push((cr, Hit::Show));
        let (no, yes) = answer_rects(w, h);
        pill(p, no, "Don't know", None, false);
        pill(p, yes, "Know", None, true);
        self.hits.push((no, Hit::Know(false)));
        self.hits.push((yes, Hit::Know(true)));
    }

    /// One flashcard: the word large on the front; the meaning and the context on the
    /// back.
    fn paint_card(&self, p: &mut PaintCx, cr: Rect, card: &VocabWord, shown: bool) {
        let t = p.theme.clone();
        fill_round_rect(p.canvas, cr, t.radius_lg, t.bg);
        stroke_round_rect(p.canvas, cr, t.radius_lg, t.border_px as f32, t.fg);
        let iw = cr.w - 2 * space(6) as u32;
        let x = cr.x + space(6);
        let hint = |p: &mut PaintCx, text: &str| {
            let r = Rect::new(cr.x, cr.bottom() - space(4) - 48, cr.w, 48);
            p.text_centered(r, text, t.caption.px, t.caption.style, t.muted);
        };
        if !shown {
            let big = 96.0;
            let word = p.ellipsize(&card.word, big, Style::Bold, iw as f32);
            let y = cr.y + cr.h as i32 * 2 / 5 - 70;
            p.text_centered(Rect::new(x, y, iw, 140), &word, big, Style::Bold, t.fg);
            if let Some(b) = &card.title {
                let b = p.ellipsize(b, t.label.px, Style::Italic, iw as f32);
                p.text_centered(Rect::new(x, y + 150, iw, 52), &b, t.label.px, Style::Italic, t.muted);
            }
            hint(p, "Tap the card to see the meaning");
            return;
        }
        // Back: the word as a heading, a light rule, the meaning, the context.
        let mut y = cr.y + space(5);
        let word = p.ellipsize(&card.word, t.title.px, Style::Bold, iw as f32);
        p.text_left(Rect::new(x, y, iw, 80), &word, t.title.px, Style::Bold, t.fg);
        y += 80 + space(2);
        p.canvas.fill_rect(x, y, iw as i32, t.rule_px, t.light);
        y += space(4);
        if let Some(d) = &card.definition {
            for line in wrap_lines(p, &one_paragraph(d), t.body.px, t.body.style, iw as f32).into_iter().take(6) {
                p.text_left(Rect::new(x, y, iw, 56), &line, t.body.px, t.body.style, t.fg);
                y += 56;
            }
            y += space(4);
        }
        let ctx = context_line(card);
        if !ctx.is_empty() {
            for line in wrap_lines(p, &one_paragraph(&ctx), t.label.px, Style::Italic, iw as f32).into_iter().take(4) {
                p.text_left(Rect::new(x, y, iw, 52), &line, t.label.px, Style::Italic, t.muted);
                y += 52;
            }
        }
        if ctx.is_empty() && card.definition.is_none() {
            p.text_left(Rect::new(x, y, iw, 52), "No meaning or context saved", t.label.px, t.label.style, t.muted);
        }
        if let Some(b) = &card.title {
            let b = p.ellipsize(b, t.caption.px, Style::Italic, iw as f32);
            p.text_left(Rect::new(x, cr.bottom() - space(4) - 48 - 56, iw, 48), &b, t.caption.px, Style::Italic, t.muted);
        }
        hint(p, "Tap the card to turn it back");
    }
}

/// `text` with every run of whitespace (newlines too) as one space: the card shows the
/// meaning and the context as one flowing paragraph each, as the card's own wrap did
/// before it used [`wrap_lines`], which starts a new line at every `\n`.
fn one_paragraph(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl AppScreen for VocabScreen {
    fn name(&self) -> &'static str {
        "vocabulary"
    }

    fn on_enter(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        self.path = ink_stats::vocabulary_path(cx.paths);
        let (now, _) = now_and_offset(cx);
        self.load(now, cx.screen.size().h as i32);
        self.dirty = Some(RefreshClass::Gc16);
        self.dirty_rect = None;
        Ok(())
    }

    fn handle(&mut self, ev: &UiEvent, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        let size = cx.screen.size();
        let (now, _) = now_and_offset(cx);
        // Pages turn on the list only, not during a practice round.
        let list = self.round.is_none();
        let hit = match input(ev, &self.hits) {
            Some(Input::Quit) => return Ok(Transition::Exit),
            Some(Input::Key(Key::Back)) => Hit::Back,
            Some(Input::Key(Key::PageNext)) if list => Hit::NextPage,
            Some(Input::Key(Key::PagePrev)) if list => Hit::PrevPage,
            Some(Input::Swipe(step)) if list => if step > 0 { Hit::NextPage } else { Hit::PrevPage },
            Some(Input::Hit(hit)) => hit,
            _ => return Ok(Transition::None),
        };
        Ok(self.act(hit, now, size))
    }

    fn paint(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        let Some(class) = self.dirty.take() else { return Ok(Vec::new()) };
        let mut p = paint_cx(cx);
        self.draw(&mut p);
        let rect = self.dirty_rect.take().unwrap_or_else(|| cx.canvas.bounds());
        Ok(vec![Damage { rect, class }])
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn card_text_flows_as_one_paragraph() {
        assert_eq!(one_paragraph("a long-standing\n\n resentment\t"), "a long-standing resentment");
    }

    #[test]
    fn context_lines() {
        let mut w = VocabWord { word: "alike".into(), ..Default::default() };
        assert_eq!(context_line(&w), "");
        w.prev_context = Some("both ".into());
        assert_eq!(context_line(&w), "…both alike");
        w.next_context = Some("in dignity".into());
        assert_eq!(context_line(&w), "…both alike in dignity…");
        w.prev_context = None;
        assert_eq!(context_line(&w), "alike in dignity…");
    }
}
