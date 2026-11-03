//! The book details page as a card: big cover, title, author, progress bar and percent,
//! facts; under it Read / Continue as the filled button, the reading status as chips
//! and the book's collections as chips.

use super::browse::{badge_w, book_state, chip_w, paint_badge, paint_chip, percent_text, Badge};
use super::model::status_label;
use super::view::{human_size, HEADER_H, MARGIN, TAB_H};
use super::{Hit, LibraryScreen, STATUSES};
use ink_device::Rect;
use ink_store::{BookEntry, ReadingStatus};
use ink_text::Style;
use ink_ui::draw::stroke_round_rect;
use ink_ui::theme::space;
use ink_ui::{paint_button, ButtonKind, Icon, PaintCx, ProgressBar, Widget};
use std::path::Path;

/// Height of the details card.
pub const CARD_H: i32 = 600;

/// Caption of the main button: "Read", "Continue reading" or "Read again".
pub fn read_label(b: &BookEntry) -> &'static str {
    match book_state(b) {
        Ok(v) if v > 0.0 => "Continue reading",
        Err(Badge::Finished) => "Read again",
        _ => "Read",
    }
}

impl LibraryScreen {
    pub(super) fn draw_details(&mut self, p: &mut PaintCx, w: i32, h: i32, path: &Path) {
        let t = p.theme.clone();
        let Some(b) = self.lib.as_ref().and_then(|l| l.index.get(path)).cloned() else {
            p.text_centered(Rect::new(MARGIN, 400, (w - 2 * MARGIN) as u32, 80), "This book is gone", t.body.px, Style::Italic, t.muted);
            return;
        };
        let cw = (w - 2 * MARGIN) as u32;
        let card = Rect::new(MARGIN, HEADER_H + space(3), cw, CARD_H as u32);
        stroke_round_rect(p.canvas, card, t.radius_lg, t.border_px as f32, t.fg);
        let pad = space(3);
        let ch = card.h as i32 - 2 * pad;
        let cover = Rect::new(card.x + pad, card.y + pad, (ch * 2 / 3) as u32, ch as u32);
        self.paint_book_cover(p, cover, &b);
        let tx = cover.right() + space(4);
        let tw = (card.right() - pad - tx).max(10) as u32;
        let mut y = card.y + pad;
        let lh = (t.heading.px * 1.25) as i32;
        for l in ink_ui::dialog::wrap_lines(p, &b.title, t.heading.px, t.heading.style, tw as f32).iter().take(3) {
            p.text_left(Rect::new(tx, y, tw, lh as u32), l, t.heading.px, t.heading.style, t.fg);
            y += lh;
        }
        let authors = if b.authors.is_empty() { "Unknown author".to_string() } else { b.authors.join(", ") };
        p.text_left(Rect::new(tx, y + 4, tw, 56), &authors, t.body.px, Style::Regular, t.muted);
        y += 64;
        if let Some(s) = &b.series {
            let text = match b.series_index {
                Some(i) => format!("{s} #{i}"),
                None => s.clone(),
            };
            p.text_left(Rect::new(tx, y, tw, 44), &text, t.caption.px, Style::Italic, t.muted);
            y += 44;
        }
        // Progress: a thick bar with the percent, the status as its caption.
        y += space(2);
        let pc = b.percent.unwrap_or(0.0);
        let (value, caption) = match book_state(&b) {
            Ok(v) => (
                v,
                match b.pages {
                    Some(n) if n > 0 => format!("{} · page {} of {n}", status_label(b.status), ((v * n as f32).round() as u32).max(1)),
                    _ => status_label(b.status).to_string(),
                },
            ),
            Err(Badge::Finished) => (1.0, "Finished".to_string()),
            Err(Badge::New) => (pc, "Unread".to_string()),
            Err(Badge::Locked) => (pc, "Cannot be opened".to_string()),
        };
        if !b.unsupported && b.status != ReadingStatus::New && !super::browse::shows_progress(&b) {
            // Opened but still at 0 %: the status in words, no bar.
            p.text_left(Rect::new(tx, y, tw, 64), &format!("{} · not started", status_label(b.status)), t.caption.px, Style::Regular, t.muted);
            y += 64 + space(2);
        } else if b.unsupported || !super::browse::shows_progress(&b) {
            // No progress for a book that cannot be opened, nor for one marked unread:
            // the badge says why.
            let badge = if b.unsupported { Badge::Locked } else { Badge::New };
            let bh = 48;
            let bw = badge_w(p, badge, bh);
            paint_badge(p, Rect::new(tx, y + 8, bw, bh), badge);
            y += bh as i32 + space(4);
        } else {
            ProgressBar::new(value).with_label(percent_text(value)).with_caption(caption).paint(p, Rect::new(tx - 4, y, tw + 4, 96));
            y += 96 + space(2);
        }
        let mut facts: Vec<String> = vec![format!("{} · {}", b.format.to_uppercase(), human_size(b.size))];
        if let Some(n) = b.pages {
            facts.push(format!("{n} pages"));
        }
        if let Some(l) = &b.language {
            facts.push(format!("Language: {l}"));
        }
        if let Some(pb) = &b.publisher {
            facts.push(format!("Publisher: {pb}"));
        }
        let bottom = card.bottom() - pad;
        for f in facts {
            if y + 44 > bottom {
                break;
            }
            p.text_left(Rect::new(tx, y, tw, 44), &f, t.caption.px, Style::Regular, t.muted);
            y += 44;
        }
        if b.unsupported {
            // The refusal text from ink-formats (DRM scheme, KFX), wrapped under the facts.
            let text = self.refusal.clone().unwrap_or_else(|| "This book cannot be opened (DRM or unsupported format).".into());
            for l in ink_ui::dialog::wrap_lines(p, &text, t.caption.px, Style::Italic, tw as f32).iter().take(4) {
                if y + 44 > bottom {
                    break;
                }
                p.text_left(Rect::new(tx, y, tw, 44), l, t.caption.px, Style::Italic, t.fg);
                y += 44;
            }
        }

        // Actions: Read / Continue (filled), Delete, Plugins.
        let bh = t.control_h();
        let mut y = card.bottom() + space(4);
        let side = 300;
        let n_side = if self.plugin_actions.is_empty() { 1 } else { 2 };
        let main_w = cw as i32 - n_side * (side + space(2));
        let read = Rect::new(MARGIN, y, main_w as u32, bh);
        paint_button(p, read, read_label(&b), Some(Icon::Book), ButtonKind::Filled, true, false);
        self.hits.push((read, Hit::Read(b.path.clone())));
        let mut x = read.right() + space(2);
        if !self.plugin_actions.is_empty() {
            let r = Rect::new(x, y, side as u32, bh);
            paint_button(p, r, "Plugins", Some(Icon::More), ButtonKind::Outlined, true, false);
            self.hits.push((r, Hit::PluginMenu(b.path.clone())));
            x = r.right() + space(2);
        }
        let del = Rect::new(x, y, side as u32, bh);
        paint_button(p, del, "Delete", Some(Icon::Trash), ButtonKind::Outlined, true, false);
        self.hits.push((del, Hit::Delete(b.path.clone())));
        y += bh as i32 + space(4);

        // Reading status chips.
        p.text_left(Rect::new(MARGIN, y, cw, 48), "Reading status", t.caption.px, Style::Bold, t.fg);
        y += 48 + space(1);
        let mut x = MARGIN;
        for (i, s) in STATUSES.iter().enumerate() {
            let label = status_label(*s);
            let on = b.status == *s;
            let r = Rect::new(x, y, chip_w(p, label, on), super::browse::CHIP_H);
            paint_chip(p, r, label, None, on);
            self.hits.push((r, Hit::SetStatus(b.path.clone(), i)));
            x = r.right() + space(2);
        }
        y += super::browse::CHIP_H as i32 + space(4);

        // Collections: the book's collections (on) and "Add to collection".
        p.text_left(Rect::new(MARGIN, y, cw, 48), "Collections", t.caption.px, Style::Bold, t.fg);
        y += 48 + space(1);
        let mut chips: Vec<(String, Option<Icon>, bool)> = b.collections.iter().map(|c| (c.clone(), None, true)).collect();
        chips.push(("Add to collection".to_string(), Some(Icon::Plus), false));
        let limit = h - TAB_H - space(2);
        let mut x = MARGIN;
        for (label, icon, on) in chips {
            let cwid = chip_w(p, &label, icon.is_some() || on).min(cw);
            if x > MARGIN && x + cwid as i32 > MARGIN + cw as i32 {
                x = MARGIN;
                y += super::browse::CHIP_H as i32 + space(2);
            }
            let r = Rect::new(x, y, cwid, super::browse::CHIP_H);
            if r.bottom() > limit {
                break;
            }
            paint_chip(p, r, &label, icon, on);
            self.hits.push((r, Hit::CollectionMenu(b.path.clone())));
            x = r.right() + space(2);
        }
        y += super::browse::CHIP_H as i32 + space(3);

        // The file, last.
        let lines = ink_ui::dialog::wrap_lines(p, &b.path.display().to_string(), t.caption.px * 0.9, Style::Regular, cw as f32);
        for (i, l) in lines.iter().take(2).enumerate() {
            if y + (i as i32 + 1) * 40 > limit {
                break;
            }
            p.text_left(Rect::new(MARGIN, y + i as i32 * 40, cw, 40), l, t.caption.px * 0.9, Style::Regular, t.muted);
        }
    }
}

/// Status of index `i` in the details chips.
pub fn status_at(i: usize) -> Option<ReadingStatus> {
    STATUSES.get(i).copied()
}
