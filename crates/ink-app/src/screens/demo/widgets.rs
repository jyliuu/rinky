//! Widgets that exist only in the gallery: the gesture pad and the shaping samples.

use ink_device::{Point, Rect, RefreshClass};
use ink_text::{ShapeOptions, Style};
use ink_ui::draw::{stroke_round_rect, Path};
use ink_ui::{Corner, EventResult, Gesture, PaintCx, TouchKind, UiEvent, Widget};

/// Short text for a gesture (status line and pad log).
pub fn describe(g: &Gesture) -> String {
    match *g {
        Gesture::Tap { pos } => format!("tap {},{}", pos.x, pos.y),
        Gesture::DoubleTap { pos } => format!("double tap {},{}", pos.x, pos.y),
        Gesture::LongPress { pos, held } => format!("long press {},{} {} ms", pos.x, pos.y, held.as_millis()),
        Gesture::Swipe { from, to, dir } => format!("swipe {dir:?} {},{} -> {},{}", from.x, from.y, to.x, to.y),
        Gesture::TwoFingerTap { pos } => format!("two-finger tap {},{}", pos.x, pos.y),
        Gesture::TwoFingerSwipe { from, to, dir } => format!("two-finger swipe {dir:?} {},{} -> {},{}", from.x, from.y, to.x, to.y),
        Gesture::Pinch { center, scale_milli } => {
            let kind = if scale_milli > 1000 { "spread" } else { "pinch" };
            format!("{kind} x{:.2} at {},{}", scale_milli as f32 / 1000.0, center.x, center.y)
        }
        Gesture::CornerHold { corner, .. } => format!("corner hold {corner:?}"),
    }
}

/// Records gestures and live touches and draws a mark for each.
#[derive(Debug, Default)]
pub struct GesturePad {
    /// Recent gestures, newest last.
    pub gestures: Vec<Gesture>,
    /// Fingers down now.
    touches: Vec<(u8, Point)>,
}

impl GesturePad {
    /// Keeps the last 8 gestures.
    pub fn record(&mut self, g: Gesture) {
        self.gestures.push(g);
        if self.gestures.len() > 8 {
            self.gestures.remove(0);
        }
    }
}

fn clamp_in(b: Rect, p: Point) -> (f32, f32) {
    ((p.x.clamp(b.x + 20, b.right() - 20)) as f32, (p.y.clamp(b.y + 20, b.bottom() - 20)) as f32)
}

impl Widget for GesturePad {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let (fg, muted) = (cx.theme.fg, cx.theme.muted);
        stroke_round_rect(cx.canvas, b, cx.theme.radius, 2.0, muted);
        let small = cx.theme.small_px;
        let hint = Rect::new(b.x + 20, b.y + 10, b.w.saturating_sub(40), 50);
        cx.text_left(hint, "Tap, double tap, hold, swipe, two-finger tap, pinch, hold a corner", small, Style::Italic, muted);
        let n = self.gestures.len();
        for (i, g) in self.gestures.iter().enumerate() {
            // Older marks are lighter.
            let v = if i + 1 == n { fg } else { muted };
            let w = 6.0;
            match *g {
                Gesture::Tap { pos } => {
                    let (x, y) = clamp_in(b, pos);
                    Path::new().circle(x, y, 14.0).fill(cx.canvas, v);
                }
                Gesture::DoubleTap { pos } => {
                    let (x, y) = clamp_in(b, pos);
                    Path::new().ring(x, y, 24.0, w).ring(x, y, 40.0, w).fill(cx.canvas, v);
                }
                Gesture::LongPress { pos, .. } => {
                    let (x, y) = clamp_in(b, pos);
                    Path::new().rect(x - 22.0, y - 22.0, 44.0, 44.0).fill(cx.canvas, v);
                }
                Gesture::Swipe { from, to, .. } | Gesture::TwoFingerSwipe { from, to, .. } => {
                    let (a, c) = (clamp_in(b, from), clamp_in(b, to));
                    let (dx, dy) = (c.0 - a.0, c.1 - a.1);
                    let len = (dx * dx + dy * dy).sqrt().max(1.0);
                    let (ux, uy) = (dx / len, dy / len);
                    let mut p = Path::new();
                    p.line(a.0, a.1, c.0, c.1, w);
                    p.polygon(&[(c.0 + ux * 10.0, c.1 + uy * 10.0), (c.0 - ux * 30.0 - uy * 20.0, c.1 - uy * 30.0 + ux * 20.0), (c.0 - ux * 30.0 + uy * 20.0, c.1 - uy * 30.0 - ux * 20.0)]);
                    p.fill(cx.canvas, v);
                }
                Gesture::TwoFingerTap { pos } => {
                    let (x, y) = clamp_in(b, pos);
                    Path::new().circle(x - 30.0, y, 14.0).circle(x + 30.0, y, 14.0).fill(cx.canvas, v);
                }
                Gesture::Pinch { center, scale_milli } => {
                    let (x, y) = clamp_in(b, center);
                    let r = 30.0 + (scale_milli as f32 / 1000.0).min(4.0) * 30.0;
                    Path::new().ring(x, y, r, w).ring(x, y, 20.0, w).fill(cx.canvas, v);
                }
                Gesture::CornerHold { corner, .. } => {
                    let (x0, y0, sx, sy) = match corner {
                        Corner::TopLeft => (b.x, b.y, 1, 1),
                        Corner::TopRight => (b.right(), b.y, -1, 1),
                        Corner::BottomLeft => (b.x, b.bottom(), 1, -1),
                        Corner::BottomRight => (b.right(), b.bottom(), -1, -1),
                    };
                    let (x0, y0) = (x0 as f32, y0 as f32);
                    Path::new().polygon(&[(x0, y0), (x0 + sx as f32 * 120.0, y0), (x0, y0 + sy as f32 * 120.0)]).fill(cx.canvas, v);
                }
            }
        }
        for (_, p) in &self.touches {
            let (x, y) = clamp_in(b, *p);
            Path::new().ring(x, y, 50.0, 4.0).fill(cx.canvas, fg);
        }
        // Log of the last gestures, newest at the bottom.
        let lines: Vec<String> = self.gestures.iter().rev().take(5).rev().map(describe).collect();
        let mut y = b.bottom() - 20 - lines.len() as i32 * (small * 1.3) as i32;
        for (i, l) in lines.iter().enumerate() {
            let style = if i + 1 == lines.len() { Style::Bold } else { Style::Regular };
            // Right of the bottom-left corner mark.
            let r = Rect::new(b.x + 150, y, b.w.saturating_sub(174), (small * 1.3) as u32);
            cx.text_left(r, l, small, style, fg);
            y += (small * 1.3) as i32;
        }
    }

    fn event(&mut self, ev: &UiEvent, _b: Rect) -> EventResult {
        match ev {
            UiEvent::Touch(t) => {
                match t.kind {
                    TouchKind::Down => self.touches.push((t.slot, t.pos)),
                    TouchKind::Up | TouchKind::Cancel => self.touches.retain(|(s, _)| *s != t.slot),
                    // Nothing to redraw (a repaint per move is slow on the device).
                    TouchKind::Press | TouchKind::Move | TouchKind::Hold => return EventResult::Ignored,
                }
                EventResult::Handled
            }
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        RefreshClass::Du
    }
}

/// Lines that show what shaping does: kerning, ligatures, fallback faces and synthetic
/// styles, each drawn with the shaped text path.
#[derive(Debug)]
pub struct ShapingSample {
    /// (caption, text, style, options)
    pub lines: Vec<(String, String, Style, ShapeOptions)>,
    /// Text size.
    pub px: f32,
}

impl ShapingSample {
    /// The gallery's samples.
    pub fn gallery(px: f32) -> Self {
        let off = |k: bool, l: bool| ShapeOptions { kerning: k, ligatures: l, language: None };
        let on = ShapeOptions::default();
        let s = |c: &str, t: &str, st: Style, o: &ShapeOptions| (c.to_string(), t.to_string(), st, o.clone());
        ShapingSample {
            px,
            lines: vec![
                s("kerning off", "AVATAR WAVE To Ty", Style::Regular, &off(false, true)),
                s("kerning on", "AVATAR WAVE To Ty", Style::Regular, &on),
                s("ligatures off", "fi fl ffi office", Style::Italic, &off(true, false)),
                s("ligatures on", "fi fl ffi office", Style::Italic, &on),
                s("fallback", "日本語 中文 한국어 ★☆♥ →← ✓", Style::Regular, &on),
                s("synthetic bold", "日本語 ★ Bold", Style::Bold, &on),
                s("synthetic italic", "日本語 ★ Italic", Style::Italic, &on),
                s("bold italic", "Bold Italic 日本 ★", Style::BoldItalic, &on),
            ],
        }
    }

    /// Height needed.
    pub fn height(&self) -> u32 {
        (self.lines.len() as f32 * self.px * 1.6) as u32 + 10
    }
}

impl Widget for ShapingSample {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let (fg, muted) = (cx.theme.fg, cx.theme.muted);
        let small = cx.theme.small_px * 0.9;
        let cap_w = (b.w as f32 * 0.3).min(300.0);
        let mut baseline = b.y as f32 + self.px * 1.2;
        for (caption, text, style, opts) in &self.lines {
            // Inset: italic glyphs overhang to the left.
            cx.draw_text_clipped(Style::Italic, small, b.x as f32 + 6.0, baseline, caption, muted, b);
            let run = cx.fonts.shape_with(*style, self.px, text, opts);
            cx.fonts.draw_run_clipped(cx.canvas, &run, b.x as f32 + cap_w, baseline, fg, b);
            baseline += self.px * 1.6;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_device::Canvas;
    use ink_text::Fonts;
    use ink_ui::{Direction, Theme};
    use std::time::Duration;

    #[test]
    fn pad_draws_every_gesture_kind() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(1000, 1000);
        let mut pad = GesturePad::default();
        let p = Point::new(500, 500);
        for g in [
            Gesture::Tap { pos: p },
            Gesture::DoubleTap { pos: p },
            Gesture::LongPress { pos: p, held: Duration::from_millis(900) },
            Gesture::Swipe { from: p, to: Point::new(900, 500), dir: Direction::Right },
            Gesture::TwoFingerTap { pos: p },
            Gesture::Pinch { center: p, scale_milli: 2000 },
            Gesture::CornerHold { corner: Corner::TopLeft, pos: Point::new(5, 5) },
            Gesture::Pinch { center: p, scale_milli: 400 },
            Gesture::Tap { pos: p },
        ] {
            pad.record(g);
            assert!(!describe(&g).is_empty());
        }
        assert_eq!(pad.gestures.len(), 8);
        pad.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme), Rect::new(0, 0, 1000, 1000));
        assert!(canvas.pixels.iter().filter(|v| **v < 64).count() > 5000);
        assert_eq!(describe(&Gesture::Pinch { center: p, scale_milli: 500 }), "pinch x0.50 at 500,500");
    }

    #[test]
    fn shaping_sample_paints() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(1200, 800);
        let mut s = ShapingSample::gallery(40.0);
        assert!(s.height() > 400);
        s.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme), Rect::new(0, 0, 1200, 800));
        assert!(canvas.pixels.iter().filter(|v| **v < 64).count() > 5000);
    }
}
