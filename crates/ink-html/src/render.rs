//! Draws a page of a chapter layout into a canvas (page content only).

use crate::fonts::FontBook;
use crate::layout::{ChapterLayout, Draw, RectRun};
use crate::paginate::PageSpan;
use ink_device::Canvas;
use std::sync::Arc;

/// Loads, scales and dithers the image `src` to `w` x `h` (None = draw a frame).
pub type ImageFn<'a> = dyn FnMut(&str, u32, u32) -> Option<Arc<Canvas>> + 'a;

/// Coordinates beyond this many px from the page cannot be visible; items there are
/// skipped (hostile CSS lengths), so float-to-int casts stay far from the i32 limits.
const FAR: f32 = 1.0e7;

fn sane(v: f32) -> bool {
    v.is_finite() && v.abs() < FAR
}

fn rect(canvas: &mut Canvas, r: &RectRun, dy: f32, clip_top: f32, clip_bottom: f32) {
    let top = (r.y + dy).max(clip_top);
    let bottom = (r.y + r.h + dy).min(clip_bottom);
    if bottom <= top || !sane(top) || !sane(bottom) || !sane(r.x) || !sane(r.x + r.w) {
        return;
    }
    let x0 = r.x.round() as i32;
    let x1 = (r.x + r.w).round().max(r.x.round() + 1.0) as i32;
    let y0 = top.round() as i32;
    let y1 = bottom.round().max(top.round() + 1.0) as i32;
    canvas.fill_rect(x0, y0, x1 - x0, y1 - y0, r.gray);
}

/// Draws page `span` of `lay` with the page top at `margin_top`.
pub fn draw_page(canvas: &mut Canvas, fonts: &mut FontBook, lay: &ChapterLayout, span: &PageSpan, margin_top: f32, images: &mut ImageFn) {
    canvas.clear(255);
    let dy = margin_top - span.y0;
    let clip_top = margin_top - 0.5;
    let clip_bottom = span.y1 + dy + 0.5;
    for d in &lay.decor {
        if d.y + d.h + dy >= clip_top && d.y + dy <= clip_bottom {
            rect(canvas, d, dy, clip_top, clip_bottom);
        }
    }
    let slices = &lay.slices[span.first as usize..span.end as usize];
    let (Some(first), Some(last)) = (slices.first(), slices.last()) else { return };
    for it in &lay.items[first.first_item as usize..last.end_item as usize] {
        match it {
            Draw::Text(t) => {
                let base = t.baseline + dy;
                let visible = sane(base) && sane(t.x) && sane(t.x + t.deco_w) && t.px > 0.0 && t.px < 4096.0;
                if !visible {
                    continue;
                }
                if t.letter_spacing == 0.0 {
                    fonts.draw(canvas, t.face, t.px, t.x, base, &t.text);
                } else {
                    let mut x = t.x;
                    let mut buf = [0u8; 4];
                    for c in t.text.chars() {
                        x = fonts.draw(canvas, t.face, t.px, x, base, c.encode_utf8(&mut buf)) + t.letter_spacing;
                    }
                }
                // Underline below the baseline, line-through above it.
                let thick = (t.px / 16.0).max(1.0).round() as i32;
                for (on, below) in [(t.underline, 0.12), (t.strike, -0.28)] {
                    if on {
                        let y = base + t.px * below;
                        canvas.fill_rect(t.x.round() as i32, y.round() as i32, t.deco_w.round() as i32, thick, 0);
                    }
                }
            }
            Draw::Image(i) => {
                let visible = sane(i.x) && sane(i.y + dy) && i.w.is_finite() && i.w < FAR && i.h.is_finite() && i.h < FAR;
                if !visible {
                    continue;
                }
                let (w, h) = (i.w.round().max(1.0) as u32, i.h.round().max(1.0) as u32);
                let x = i.x.round() as i32;
                let y = (i.y + dy).round() as i32;
                match images(&i.src, w, h) {
                    Some(img) => canvas.blit(&img, x, y),
                    None => canvas.stroke_rect(ink_device::Rect::new(x, y, w, h), 2, 128),
                }
            }
            Draw::Rect(r) => rect(canvas, r, dy, clip_top, clip_bottom),
        }
    }
}
