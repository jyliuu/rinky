//! A single-line text field with a cursor, placeholder and password masking, edited by
//! [`crate::keyboard::KeyAction`]s from the on-screen keyboard.

use crate::draw::{fill_round_rect, stroke_round_rect};
use crate::gesture::Gesture;
use crate::keyboard::KeyAction;
use crate::widget::{EventResult, PaintCx, Widget};
use crate::UiEvent;
use ink_device::{Canvas, Rect, RefreshClass};
use ink_text::Style;
use unicode_segmentation::UnicodeSegmentation;

/// What [`TextInput::apply`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputResult {
    /// Nothing changed (a keyboard-internal key such as shift).
    Unchanged,
    /// Text or cursor changed.
    Edited,
    /// Enter was pressed.
    Submitted,
    /// The keyboard's hide key was pressed.
    Closed,
}

/// Inner padding of the field.
const PAD: i32 = 20;

/// The text field.
#[derive(Clone, Debug, PartialEq)]
pub struct TextInput {
    /// Content.
    pub text: String,
    /// Cursor as a byte offset into `text` (on a grapheme boundary).
    pub cursor: usize,
    /// Shown in gray when the text is empty.
    pub placeholder: String,
    /// Has the keyboard focus (cursor shown, thicker frame).
    pub focused: bool,
    /// Show bullets instead of the text.
    pub password: bool,
    /// Text size.
    pub px: f32,
    /// Action reported as `focus:<action>` when tapped.
    pub action: String,
    scroll: f32,
    /// (byte offset, x from the text start) of every grapheme boundary, from the last paint.
    stops: Vec<(usize, f32)>,
}

impl TextInput {
    /// An empty field.
    pub fn new(placeholder: impl Into<String>, action: impl Into<String>) -> Self {
        TextInput {
            text: String::new(),
            cursor: 0,
            placeholder: placeholder.into(),
            focused: false,
            password: false,
            px: 40.0,
            action: action.into(),
            scroll: 0.0,
            stops: Vec::new(),
        }
    }

    /// Replaces the text and puts the cursor at its end.
    pub fn set_text(&mut self, s: &str) {
        self.text = s.to_string();
        self.cursor = self.text.len();
    }

    /// Inserts at the cursor.
    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Start of the grapheme before the cursor.
    fn prev_grapheme(&self) -> Option<usize> {
        self.text[..self.cursor].grapheme_indices(true).next_back().map(|(i, _)| i)
    }

    /// Deletes the grapheme before the cursor.
    pub fn backspace(&mut self) -> bool {
        let Some(i) = self.prev_grapheme() else { return false };
        self.text.replace_range(i..self.cursor, "");
        self.cursor = i;
        true
    }

    /// Moves the cursor one grapheme left.
    pub fn left(&mut self) -> bool {
        let Some(i) = self.prev_grapheme() else { return false };
        self.cursor = i;
        true
    }

    /// Moves the cursor one grapheme right.
    pub fn right(&mut self) -> bool {
        match self.text[self.cursor..].graphemes(true).next() {
            Some(g) => {
                self.cursor += g.len();
                true
            }
            None => false,
        }
    }

    /// Applies a key.
    pub fn apply(&mut self, a: &KeyAction) -> InputResult {
        let edited = |b: bool| if b { InputResult::Edited } else { InputResult::Unchanged };
        match a {
            KeyAction::Char(s) => {
                self.insert(s);
                InputResult::Edited
            }
            KeyAction::Space => {
                self.insert(" ");
                InputResult::Edited
            }
            KeyAction::Backspace => edited(self.backspace()),
            KeyAction::Left => edited(self.left()),
            KeyAction::Right => edited(self.right()),
            KeyAction::Enter => InputResult::Submitted,
            KeyAction::Close => InputResult::Closed,
            KeyAction::Shift | KeyAction::Layout(_) | KeyAction::NextLanguage => InputResult::Unchanged,
        }
    }

    /// What is drawn: the text, or one bullet per grapheme for passwords. Returns the
    /// shown string and a map from text byte offsets to shown byte offsets.
    fn shown(&self) -> (String, Vec<(usize, usize)>) {
        let mut shown = String::new();
        let mut map = vec![(0, 0)];
        for (i, g) in self.text.grapheme_indices(true) {
            shown.push_str(if self.password { "\u{2022}" } else { g });
            map.push((i + g.len(), shown.len()));
        }
        (shown, map)
    }

    fn inner(b: Rect) -> Rect {
        Rect::new(b.x + PAD, b.y + 6, (b.w as i32 - 2 * PAD).max(1) as u32, b.h.saturating_sub(12))
    }
}

impl Widget for TextInput {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let (fg, bg) = (cx.theme.fg, cx.theme.bg);
        fill_round_rect(cx.canvas, b, cx.theme.radius, bg);
        stroke_round_rect(cx.canvas, b, cx.theme.radius, if self.focused { 5.0 } else { 2.0 }, fg);
        let inner = TextInput::inner(b);
        let (shown, map) = self.shown();
        let run = cx.fonts.shape(Style::Regular, self.px, &shown);
        self.stops = map.iter().map(|(t, s)| (*t, run.x_at(*s))).collect();
        let cursor_x = self.stops.iter().find(|(t, _)| *t == self.cursor).map(|(_, x)| *x).unwrap_or(run.width);
        // Keep the cursor visible.
        let room = inner.w as f32 - 8.0;
        if cursor_x - self.scroll > room {
            self.scroll = cursor_x - room;
        } else if cursor_x < self.scroll {
            self.scroll = (cursor_x - room / 3.0).max(0.0);
        }
        if run.width <= room {
            self.scroll = 0.0;
        }
        // Draw through a scratch canvas so long text is clipped to the field.
        let mut scratch = Canvas::new(inner.w as usize, inner.h as usize);
        scratch.clear(bg);
        let (asc, desc) = cx.fonts.vmetrics(Style::Regular, self.px);
        let baseline = (inner.h as f32 - (asc - desc)) / 2.0 + asc;
        if self.text.is_empty() {
            let muted = cx.theme.muted;
            let ph = cx.fonts.shape(Style::Italic, self.px, &self.placeholder);
            cx.fonts.draw_run_value(&mut scratch, &ph, 0.0, baseline, muted);
        } else {
            cx.fonts.draw_run_value(&mut scratch, &run, -self.scroll, baseline, fg);
        }
        if self.focused {
            let x = (cursor_x - self.scroll).round() as i32;
            let h = (self.px * 1.2) as i32;
            scratch.fill_rect(x, (baseline - asc).round() as i32 - 2, 3, h.min(inner.h as i32), fg);
        }
        cx.canvas.blit(&scratch, inner.x, inner.y);
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match ev {
            UiEvent::Gesture(Gesture::Tap { pos }) => {
                let x = (pos.x - TextInput::inner(b).x) as f32 + self.scroll;
                if let Some((t, _)) = self.stops.iter().min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs())) {
                    self.cursor = *t;
                }
                self.focused = true;
                EventResult::Action(format!("focus:{}", self.action))
            }
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        RefreshClass::Du
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use ink_device::Point;
    use ink_text::Fonts;

    #[test]
    fn editing_with_keys() {
        let mut t = TextInput::new("Search", "q");
        for a in [KeyAction::Char("H".into()), KeyAction::Char("é".into()), KeyAction::Space, KeyAction::Char("日".into())] {
            assert_eq!(t.apply(&a), InputResult::Edited);
        }
        assert_eq!(t.text, "Hé 日");
        assert_eq!(t.apply(&KeyAction::Left), InputResult::Edited);
        assert_eq!(t.apply(&KeyAction::Left), InputResult::Edited);
        assert_eq!(t.apply(&KeyAction::Backspace), InputResult::Edited);
        assert_eq!(t.text, "H 日");
        assert_eq!(t.apply(&KeyAction::Right), InputResult::Edited);
        t.insert("x");
        assert_eq!(t.text, "H x日");
        assert_eq!(t.apply(&KeyAction::Shift), InputResult::Unchanged);
        assert_eq!(t.apply(&KeyAction::Enter), InputResult::Submitted);
        assert_eq!(t.apply(&KeyAction::Close), InputResult::Closed);
        // Combining sequences are deleted as one grapheme.
        t.set_text("e\u{301}");
        assert!(t.backspace());
        assert_eq!(t.text, "");
        assert!(!t.backspace());
        assert!(!t.left() && !t.right());
    }

    #[test]
    fn paint_tap_cursor_and_password() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(800, 200);
        let b = Rect::new(10, 10, 700, 100);
        let mut t = TextInput::new("Password", "pw");
        t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme), b);
        assert!(canvas.pixels.iter().any(|v| *v > 64 && *v < 200), "gray placeholder");
        t.set_text("abcdef");
        t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme), b);
        // Tap near the start puts the cursor there.
        let r = t.event(&UiEvent::Gesture(Gesture::Tap { pos: Point::new(b.x + PAD + 2, 50) }), b);
        assert_eq!(r, EventResult::Action("focus:pw".into()));
        assert!(t.focused && t.cursor == 0);
        t.password = true;
        let (shown, _) = t.shown();
        assert_eq!(shown, "\u{2022}".repeat(6));
        // A long text scrolls so the cursor stays inside the field.
        t.password = false;
        t.set_text(&"word ".repeat(40));
        t.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme), b);
        assert!(t.scroll > 0.0);
    }
}
