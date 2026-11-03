//! Card dialog for plugin `Dialog` requests that use the card body convention
//! (the dictionary popup): a big title, an optional muted italic subtitle line, the body,
//! an optional muted footer line, a round close button in the corner and pill buttons.
//!
//! Body convention (plain text, so other hosts still show something readable):
//! - first line `_text_` (Markdown italic): subtitle (pronunciation, part of speech);
//! - last line starting with `— ` (em dash, space): footer (source dictionary);
//! - body lines are paragraphs; a paragraph starting with a number (`1. `) wraps with a
//!   hanging indent under its text, one wrapped in `_…_` is drawn in italics (examples);
//! - a button with the action `close` becomes the close button in the corner when there
//!   are other buttons;
//! - a footer starting with `N/M · ` (`— 2/3 · Dictionary`) marks page N of M: a swipe
//!   left on the card reports `next`, a swipe right `prev` (the plugin sends the other
//!   page as a new dialog with the same id). A paged card keeps one height for every
//!   page (the subtitle row and [`MAX_LINES`] body lines are always reserved), so a new
//!   page is drawn in the same rectangle with one partial update.
//!
//! A dialog without a subtitle or footer line stays an ink-ui [`ink_ui::Dialog`] (its
//! geometry and look are unchanged).

use ink_device::{Rect, Size};
use ink_ui::draw::{fill_round_rect, stroke_round_rect};
use ink_ui::theme::space;
use ink_ui::{paint_button, ButtonKind, Direction, EventResult, Gesture, Icon, PaintCx, UiEvent, Widget};

/// Inner padding.
const PAD: i32 = 40;
/// Title row height (title role, 56 px).
const TITLE_H: i32 = 88;
/// Subtitle row height.
const SUB_H: i32 = 52;
/// Height of one body line.
const LINE_H: i32 = 54;
/// Footer row height.
const FOOT_H: i32 = 52;
/// Button height.
const BUTTON_H: u32 = 104;
/// Body lines shown at most.
const MAX_LINES: usize = 8;

/// One wrapped body line.
#[derive(Clone, Debug, PartialEq)]
struct Line {
    /// Sense number drawn left of the text (first line of a numbered paragraph).
    marker: String,
    /// Indent of the text (numbered paragraphs).
    indent: i32,
    text: String,
    italic: bool,
}

/// A card dialog (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct CardDialog {
    /// Title (headword).
    pub title: String,
    /// Muted italic line under the title.
    pub subtitle: Option<String>,
    /// Body text.
    pub body: String,
    /// Muted line under the body.
    pub footer: Option<String>,
    /// Action of the close button in the corner.
    pub close: Option<String>,
    /// Other `(label, action)` buttons; the last is filled when there are several.
    pub buttons: Vec<(String, String)>,
    /// `(page, pages)` from a `N/M · ` footer (1-based page): swipes report `next`/`prev`.
    pub pages: Option<(usize, usize)>,
    /// Put in front of every action the card reports (the reader uses `pcard:` so the
    /// card's `close` is not the reader's own `close`).
    pub prefix: String,
    /// Wrapped body lines (set by [`CardDialog::frame`]).
    lines: Vec<Line>,
    /// The card changed since its last paint (new card, new page); a tap on its empty
    /// parts reports no damage.
    changed: bool,
}

impl CardDialog {
    /// A card for a dialog request, or `None` when the body does not use the card
    /// convention.
    pub fn parse(title: &str, body: &str, buttons: &[(String, String)]) -> Option<Self> {
        let mut lines: Vec<&str> = body.lines().collect();
        let subtitle = match lines.first() {
            Some(l) if l.len() > 2 && l.starts_with('_') && l.ends_with('_') => {
                let s = l[1..l.len() - 1].trim().to_string();
                lines.remove(0);
                Some(s)
            }
            _ => None,
        };
        let footer = match lines.last() {
            Some(l) if l.starts_with("— ") => {
                let s = l["— ".len()..].trim().to_string();
                lines.pop();
                Some(s)
            }
            _ => None,
        };
        if subtitle.is_none() && footer.is_none() {
            return None;
        }
        let mut buttons = buttons.to_vec();
        let close = match buttons.iter().position(|(_, a)| a == "close") {
            Some(i) if buttons.len() > 1 => Some(buttons.remove(i).1),
            _ => None,
        };
        let pages = footer.as_deref().and_then(page_of);
        Some(CardDialog {
            title: title.to_string(),
            subtitle,
            body: lines.join("\n").trim().to_string(),
            footer,
            close,
            buttons,
            pages,
            prefix: String::new(),
            lines: Vec::new(),
            changed: true,
        })
    }

    /// Every action the card reports by a tap with its rectangle in the card `b`
    /// (prefixed): the close button, then the pill buttons.
    pub fn action_rects(&self, b: Rect) -> Vec<(String, Rect)> {
        let mut v: Vec<(String, Rect)> = self.close.iter().map(|a| (format!("{}{a}", self.prefix), Self::close_rect(b))).collect();
        v.extend(self.button_rects(b).into_iter().zip(&self.buttons).map(|(r, (_, a))| (format!("{}{a}", self.prefix), r)));
        v
    }

    fn report(&self, action: &str) -> EventResult {
        EventResult::Action(format!("{}{action}", self.prefix))
    }

    /// Height of the card for its wrapped lines.
    fn height(&self) -> u32 {
        let mut h = PAD + TITLE_H + space(2);
        // A paged card has one height for all its pages.
        let paged = self.pages.is_some();
        if self.subtitle.is_some() || paged {
            h += SUB_H;
        }
        h += if paged { MAX_LINES as i32 } else { self.lines.len() as i32 } * LINE_H;
        if self.footer.is_some() {
            h += space(1) + FOOT_H;
        }
        if !self.buttons.is_empty() {
            h += space(3) + BUTTON_H as i32;
        }
        (h + PAD) as u32
    }

    /// Wraps the body for the screen width and returns the card rectangle: full width
    /// less the screen margins, standing `space(1)` (8 px) above the bottom edge, over the status bar (a higher card cut through the footer text).
    pub fn frame(&mut self, cx: &mut PaintCx, screen: Size) -> Rect {
        let w = screen.w.saturating_sub(2 * space(4) as u32);
        let (px, regular) = (cx.theme.body.px, cx.theme.body.style);
        let max = (w as i32 - 2 * PAD) as f32;
        let mut lines: Vec<Line> = Vec::new();
        for para in self.body.split('\n').map(str::trim).filter(|p| !p.is_empty()) {
            let (para, italic) = match para.strip_prefix('_').and_then(|p| p.strip_suffix('_')) {
                Some(p) if !p.is_empty() => (p, true),
                _ => (para, false),
            };
            let style = if italic { ink_text::Style::Italic } else { regular };
            // `12. text`: the number hangs left of the wrapped text.
            let digits = para.chars().take_while(|c| c.is_ascii_digit()).count();
            let (marker, text) = match para[digits..].strip_prefix(". ") {
                Some(rest) if digits > 0 => (para[..digits + 1].to_string(), rest),
                _ => (String::new(), para),
            };
            let indent = if marker.is_empty() { 0 } else { cx.text_width(regular, px, "00. ").ceil() as i32 };
            for (i, l) in ink_ui::dialog::wrap_lines(cx, text, px, style, max - indent as f32).into_iter().enumerate() {
                lines.push(Line { marker: if i == 0 { marker.clone() } else { String::new() }, indent, text: l, italic });
            }
        }
        if lines.len() > MAX_LINES {
            lines.truncate(MAX_LINES);
            let last = &mut lines[MAX_LINES - 1];
            last.text = format!("{}…", last.text.trim_end_matches(['…', ' ']));
        }
        self.lines = lines;
        self.changed = true;
        let h = self.height().min(screen.h);
        let y = (screen.h - h) as i32 - space(1);
        Rect::new(space(4), y.max(0), w, h)
    }

    fn close_rect(b: Rect) -> Rect {
        Rect::new(b.right() - space(3) - 104, b.y + space(3), 104, 104)
    }

    fn button_rects(&self, b: Rect) -> Vec<Rect> {
        let n = self.buttons.len() as i32;
        if n == 0 {
            return Vec::new();
        }
        let gap = space(2);
        let w = ((b.w as i32 - 2 * PAD - gap * (n - 1)) / n).max(1);
        let y = b.bottom() - PAD - BUTTON_H as i32;
        (0..n).map(|i| Rect::new(b.x + PAD + i * (w + gap), y, w as u32, BUTTON_H)).collect()
    }
}

impl Widget for CardDialog {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = cx.theme.clone();
        fill_round_rect(cx.canvas, b, t.radius_lg, t.bg);
        stroke_round_rect(cx.canvas, b, t.radius_lg, t.border_px as f32, t.fg);
        let x = b.x + PAD;
        let close = self.close.as_ref().map(|_| Self::close_rect(b));
        let tw = close.map_or(b.right() - PAD, |c| c.x - space(2)) - x;
        let mut y = b.y + PAD;
        let title = cx.ellipsize(&self.title, t.title.px, t.title.style, tw as f32);
        cx.text_left(Rect::new(x, y, tw.max(1) as u32, TITLE_H as u32), &title, t.title.px, t.title.style, t.fg);
        if let Some(c) = close {
            // Outlined round icon button.
            paint_button(cx, c, "", Some(Icon::Close), ButtonKind::Outlined, true, false);
        }
        y += TITLE_H;
        let w = (b.w as i32 - 2 * PAD).max(1) as u32;
        if let Some(s) = &self.subtitle {
            let s = cx.ellipsize(s, t.label.px, ink_text::Style::Italic, w as f32);
            cx.text_left(Rect::new(x, y, w, SUB_H as u32), &s, t.label.px, ink_text::Style::Italic, t.muted);
            y += SUB_H;
        } else if self.pages.is_some() {
            y += SUB_H;
        }
        y += space(2);
        for line in self.lines.clone() {
            if !line.marker.is_empty() {
                cx.text_left(Rect::new(x, y, line.indent.max(1) as u32, LINE_H as u32), &line.marker, t.body.px, ink_text::Style::Bold, t.fg);
            }
            let (style, value) = if line.italic { (ink_text::Style::Italic, t.muted) } else { (t.body.style, t.fg) };
            cx.text_left(Rect::new(x + line.indent, y, (w as i32 - line.indent).max(1) as u32, LINE_H as u32), &line.text, t.body.px, style, value);
            y += LINE_H;
        }
        if self.pages.is_some() {
            // The footer and buttons stay in place on every page.
            y = b.y + PAD + TITLE_H + SUB_H + space(2) + MAX_LINES as i32 * LINE_H;
        }
        if let Some(f) = &self.footer {
            y += space(1);
            let f = cx.ellipsize(f, t.caption.px, t.caption.style, w as f32);
            cx.text_left(Rect::new(x, y, w, FOOT_H as u32), &f, t.caption.px, t.caption.style, t.muted);
        }
        let n = self.buttons.len();
        for (i, (r, (label, _))) in self.button_rects(b).into_iter().zip(self.buttons.clone()).enumerate() {
            let kind = if n > 1 && i + 1 == n { ButtonKind::Filled } else { ButtonKind::Outlined };
            paint_button(cx, r, &label, None, kind, true, false);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match ev {
            UiEvent::Gesture(Gesture::Tap { pos }) => {
                if let Some(a) = &self.close {
                    if Self::close_rect(b).contains(*pos) {
                        return self.report(a);
                    }
                }
                for (r, (_, action)) in self.button_rects(b).into_iter().zip(&self.buttons) {
                    if r.contains(*pos) {
                        return self.report(action);
                    }
                }
                EventResult::Handled
            }
            // A paged card: swipe left for the next page, right for the previous one.
            UiEvent::Gesture(Gesture::Swipe { dir: Direction::Left, .. }) if self.pages.is_some() => self.report("next"),
            UiEvent::Gesture(Gesture::Swipe { dir: Direction::Right, .. }) if self.pages.is_some() => self.report("prev"),
            UiEvent::Gesture(_) => EventResult::Handled,
            // Per-contact notifications change nothing on the card (it works on
            // gestures); taking them would repaint it on every touch.
            _ => EventResult::Ignored,
        }
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        // A tap on the card's empty parts changed nothing: no refresh.
        if std::mem::take(&mut self.changed) {
            None
        } else {
            Some(Vec::new())
        }
    }
}

/// `(page, pages)` of a footer starting with `N/M · ` (1 <= N <= M, M >= 2).
fn page_of(footer: &str) -> Option<(usize, usize)> {
    let (nm, _) = footer.split_once(" · ")?;
    let (n, m) = nm.split_once('/')?;
    let (n, m) = (n.trim().parse::<usize>().ok()?, m.trim().parse::<usize>().ok()?);
    (m >= 2 && (1..=m).contains(&n)).then_some((n, m))
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn card_convention() {
        let b = |l: &str, a: &str| (l.to_string(), a.to_string());
        let buttons = vec![b("Close", "close"), b("Add", "add"), b("Full view", "full")];
        let c = CardDialog::parse("word", "_noun_\na unit of language\n— Sample", &buttons).unwrap();
        assert_eq!((c.subtitle.as_deref(), c.body.as_str(), c.footer.as_deref()), (Some("noun"), "a unit of language", Some("Sample")));
        assert_eq!((c.close.as_deref(), c.buttons.len()), (Some("close"), 2));
        assert!(CardDialog::parse("T", "plain body", &buttons).is_none());
        // A lone Close stays a button.
        let c = CardDialog::parse("w", "text\n— D", &[b("Close", "close")]).unwrap();
        assert!(c.close.is_none() && c.buttons.len() == 1);
        // Paragraphs stay separate lines.
        let c = CardDialog::parse("w", "_noun_\n1. one\n_an example_\n2. two\n— D", &buttons).unwrap();
        assert_eq!(c.body, "1. one\n_an example_\n2. two");
        assert_eq!(c.pages, None);
        // A `N/M · ` footer pages the card.
        let c = CardDialog::parse("w", "text\n— 2/3 · Dict B · +1 more", &buttons).unwrap();
        assert_eq!((c.pages, c.footer.as_deref()), (Some((2, 3)), Some("2/3 · Dict B · +1 more")));
        assert_eq!(page_of("1/1 · D"), None);
        assert_eq!(page_of("4/3 · D"), None);
        assert_eq!(page_of("Searched 2 dictionaries"), None);
    }

    #[test]
    fn swipes_page_a_paged_card_and_actions_carry_the_prefix() {
        use ink_device::Point;
        let b = |l: &str, a: &str| (l.to_string(), a.to_string());
        let buttons = vec![b("Close", "close"), b("Add", "add")];
        let mut c = CardDialog::parse("w", "text\n— 1/2 · A", &buttons).unwrap();
        c.prefix = "pcard:".into();
        let r = Rect::new(32, 900, 1208, 788);
        let swipe = |dir| UiEvent::Gesture(Gesture::Swipe { from: Point::new(600, 1000), to: Point::new(300, 1000), dir });
        assert_eq!(c.event(&swipe(Direction::Left), r), EventResult::Action("pcard:next".into()));
        assert_eq!(c.event(&swipe(Direction::Right), r), EventResult::Action("pcard:prev".into()));
        assert_eq!(c.event(&swipe(Direction::Up), r), EventResult::Handled);
        let acts: Vec<String> = c.action_rects(r).into_iter().map(|(a, _)| a).collect();
        assert_eq!(acts, ["pcard:close", "pcard:add"]);
        // An unpaged card swallows swipes and reports nothing.
        let mut one = CardDialog::parse("w", "text\n— A", &buttons).unwrap();
        assert_eq!(one.event(&swipe(Direction::Left), r), EventResult::Handled);
    }

    /// The Wikipedia plugin's cards (summary, paged, missing) are cards of this host:
    /// subtitle from the description, footer, close in the corner, pages from `N/M ·`,
    /// and the body fits the card (at most 8 wrapped lines, ellipsis when cut).
    #[test]
    fn wikipedia_cards_parse_and_fit() {
        use ink_text::Fonts;
        use ink_device::Canvas;
        use ink_ui::Theme;
        let b = |l: &str, a: &str| (l.to_string(), a.to_string());
        let buttons = vec![b("Close", "close"), b("Save", "save"), b("Full view", "full")];
        let long = "The mutiny on the Bounty occurred in the south Pacific Ocean on 28 April 1789 aboard HMS Bounty. Disaffected crewmen, led by acting-Lieutenant Fletcher Christian, seized control of the ship from their captain, Lieutenant William Bligh, and set him and eighteen loyalists adrift in the ship's launch. ".repeat(3);
        let sum = wikipedia::api::Summary { title: "Mutiny on the Bounty".into(), description: Some("1789 insurrection".into()), extract: long, url: None, kind: "standard".into() };
        let (title, body) = wikipedia::summary_card(&sum, "en", 1, 1);
        let c = CardDialog::parse(&title, &body, &buttons).expect("a card");
        assert_eq!((c.subtitle.as_deref(), c.footer.as_deref(), c.close.as_deref(), c.pages), (Some("1789 insurrection"), Some("Wikipedia · en"), Some("close"), None));
        assert_eq!(c.buttons.len(), 2);
        // Several languages: a paged card.
        let (title, body) = wikipedia::summary_card(&sum, "de", 2, 3);
        let c = CardDialog::parse(&title, &body, &buttons).unwrap();
        assert_eq!((c.pages, c.footer.as_deref()), (Some((2, 3)), Some("2/3 · Wikipedia · de")));
        // Missing page and error cards: footer only, no subtitle.
        let (title, body) = wikipedia::missing_card("Mutinyy", "en", 1, 1);
        let m = CardDialog::parse(&title, &body, &[b("Close", "close")]).unwrap();
        assert_eq!((m.subtitle, m.footer.as_deref()), (None, Some("Wikipedia · en")));
        let err = format!("{}\n— {}", wikipedia::net_message("offline: Wi-Fi is off"), wikipedia::footer("en", 1, 1));
        let e = CardDialog::parse("Paris", &err, &[b("Close", "close"), b("Retry", "retry")]).unwrap();
        assert!(e.body.starts_with("Turn on Wi-Fi") && e.close.as_deref() == Some("close") && e.buttons.len() == 1);
        // Wrapping: the long extract fills the card but never more than its 8 lines.
        let (theme, mut canvas, mut fonts) = (Theme::day(), Canvas::new(1264, 1680), Fonts::load());
        let (title, body) = wikipedia::summary_card(&sum, "en", 1, 1);
        let mut c = CardDialog::parse(&title, &body, &buttons).unwrap();
        let r = c.frame(&mut PaintCx::new(&mut canvas, &mut fonts, &theme), Size { w: 1264, h: 1680 });
        assert!(c.lines.len() <= MAX_LINES && c.lines.len() >= 5, "{} lines", c.lines.len());
        assert!(r.h < 1680 && r.y > 0, "{r:?}");
    }
}
