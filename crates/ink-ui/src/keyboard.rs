//! On-screen keyboard (the stock `kb` process is gone once the framework stops).
//!
//! - Layouts: English QWERTY, Danish (QWERTY + å æ ø), German (QWERTZ + ü ö ä ß), French
//!   (AZERTY), Spanish (QWERTY + ñ), Swedish (QWERTY + å ö ä), Norwegian (QWERTY + å ø æ),
//!   two symbol pages. The globe key cycles the enabled languages
//!   ([`Keyboard::with_language_list`], e.g. the `ui.keyboard_layouts` setting; the
//!   first is the default); with one language it does nothing.
//! - Size (as KOReader on the PW12): full screen width, [`ROW_H`] px rows, [`GAP`] px
//!   between keys, [`PAD`] px padding; [`Keyboard::bottom_rect`] places it.
//! - Long-press alternates (accents, quotes, dashes): hold a key, a row of alternates
//!   opens over the row above it, slide to one and lift.
//! - Press feedback: the pressed key gets a thick border ([`PRESS_BORDER`]) and only its
//!   face is refreshed, with GL16 (gray-preserving). Inverting the whole key was slower
//!   to show: every pixel of the face had to go black and back. An optional preview
//!   bubble ([`Keyboard::preview`]).
//! - Repeat on hold: backspace, space and the cursor keys repeat while held.
//! - Shift is one-shot; holding shift locks capitals.
//! - It works per finger ([`crate::Touch`] notifications), so two thumbs can type at once
//!   and a touch-down and touch-up in the same frame (PW12 short taps) still types.
//!
//! The widget queues [`KeyAction`]s and reports the action `keyboard`; the screen takes
//! them with [`Keyboard::take_actions`] and applies them, e.g. to a
//! [`crate::TextInput`] with [`crate::TextInput::apply`].

use crate::draw::{fill_round_rect, stroke_round_rect};
use crate::gesture::{Touch, TouchKind};
use crate::icon::Icon;
use crate::widget::{EventResult, PaintCx, Widget};
use crate::UiEvent;
use ink_device::{Point, Rect, RefreshClass, Size};
use ink_text::Style;

/// Border of a pressed key (and of shift while it is on), px, drawn inside the face.
pub const PRESS_BORDER: f32 = 6.0;

/// What a key does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Types this text.
    Char(String),
    /// Deletes before the cursor.
    Backspace,
    /// Enter / submit.
    Enter,
    /// Shift (next letter upper case).
    Shift,
    /// Switches to another layout by name.
    Layout(String),
    /// Hides the keyboard.
    Close,
    /// Space.
    Space,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// The next keyboard language (globe key).
    NextLanguage,
}

impl KeyAction {
    fn repeats(&self) -> bool {
        matches!(self, KeyAction::Backspace | KeyAction::Space | KeyAction::Left | KeyAction::Right)
    }
}

/// One key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeySpec {
    /// Caption (special keys: a plain word, see `icon`).
    pub label: String,
    /// Action.
    pub action: KeyAction,
    /// Width in half-key units (2 = a letter key).
    pub width: u32,
    /// Long-press alternates (accents).
    pub alternates: Vec<String>,
    /// Drawn instead of the caption.
    pub icon: Option<Icon>,
}

impl KeySpec {
    fn ch(c: &str, alternates: &[&str]) -> Self {
        KeySpec {
            label: c.into(),
            action: KeyAction::Char(c.into()),
            width: 2,
            alternates: alternates.iter().map(|s| s.to_string()).collect(),
            icon: None,
        }
    }

    fn special(label: &str, action: KeyAction, width: u32, icon: Option<Icon>) -> Self {
        KeySpec { label: label.into(), action, width, alternates: Vec::new(), icon }
    }
}

/// Rows of keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardLayout {
    /// Name (`en`, `da`, `de`, `fr`, `symbols`, `symbols2`).
    pub name: String,
    /// Rows, top to bottom. Widths are in half-key units.
    pub rows: Vec<Vec<KeySpec>>,
}

/// Languages with a letter layout, in globe-key order.
pub const LANGUAGES: [&str; 7] = ["en", "da", "de", "fr", "es", "sv", "nb"];

/// Display name of a keyboard language (shown on the space bar).
pub fn language_name(lang: &str) -> &'static str {
    match lang {
        "da" => "Dansk",
        "de" => "Deutsch",
        "fr" => "Français",
        "es" => "Español",
        "sv" => "Svenska",
        "nb" => "Norsk",
        _ => "English",
    }
}

fn alternates(lang: &str, key: &str) -> &'static [&'static str] {
    match (lang, key) {
        ("da", "a") => &["å", "æ", "à", "á", "â", "ä"],
        ("da", "e") => &["é", "è", "ê", "ë"],
        ("da", "o") => &["ø", "ö", "ó", "ô"],
        ("da", "u") => &["ü", "ú"],
        ("de", "a") => &["ä", "à", "á", "â"],
        ("de", "o") => &["ö", "ó", "ô"],
        ("de", "u") => &["ü", "ú", "ù", "û"],
        ("de", "s") => &["ß"],
        ("de", "e") => &["é", "è", "ê"],
        ("fr", "e") => &["é", "è", "ê", "ë"],
        ("fr", "a") => &["à", "â", "æ"],
        ("fr", "c") => &["ç"],
        ("fr", "u") => &["ù", "û", "ü"],
        ("fr", "i") => &["î", "ï"],
        ("fr", "o") => &["ô", "œ"],
        ("fr", "y") => &["ÿ"],
        ("es", "a") => &["á", "à", "ä", "â"],
        ("es", "e") => &["é", "è", "ë", "ê"],
        ("es", "i") => &["í", "ì", "ï"],
        ("es", "o") => &["ó", "ò", "ö", "º"],
        ("es", "u") => &["ú", "ü", "ù"],
        ("es", "n") => &["ñ"],
        ("es", "c") => &["ç"],
        ("es", "?") => &["¿"],
        ("es", "!") => &["¡"],
        ("es", ".") => &["¿", "¡", "…"],
        ("sv", "a") => &["à", "á", "æ"],
        ("sv", "e") => &["é", "è", "ë"],
        ("sv", "o") => &["ø", "ó", "ô"],
        ("sv", "u") => &["ü", "ú"],
        ("sv", "ö") => &["ø"],
        ("sv", "ä") => &["æ"],
        ("nb", "a") => &["ä", "à", "á"],
        ("nb", "e") => &["é", "è", "ê"],
        ("nb", "o") => &["ö", "ó", "ô"],
        ("nb", "u") => &["ü", "ú"],
        ("nb", "ø") => &["ö"],
        ("nb", "æ") => &["ä"],
        (_, "a") => &["à", "á", "â", "ä", "æ", "ã", "å", "ā"],
        (_, "c") => &["ç", "ć", "č"],
        (_, "e") => &["è", "é", "ê", "ë", "ē", "ę"],
        (_, "i") => &["ì", "í", "î", "ï"],
        (_, "n") => &["ñ", "ń"],
        (_, "o") => &["ò", "ó", "ô", "ö", "õ", "ø", "œ"],
        (_, "s") => &["ß", "ś", "š"],
        (_, "u") => &["ù", "ú", "û", "ü", "ū"],
        (_, "y") => &["ý", "ÿ"],
        (_, "z") => &["ž", "ź", "ż"],
        (_, "l") => &["ł"],
        (_, "-") => &["–", "—", "·"],
        (_, ".") => &["…"],
        (_, "'") => &["’", "‘", "‚"],
        (_, "\"") => &["“", "”", "„", "«", "»"],
        (_, "?") => &["¿"],
        (_, "!") => &["¡"],
        (_, "€") => &["$", "£", "¥", "¢"],
        (_, "%") => &["‰"],
        (_, "0") => &["°"],
        (_, "1") => &["¹", "½"],
        (_, "2") => &["²"],
        (_, "3") => &["³"],
        _ => &[],
    }
}

fn units(row: &[KeySpec]) -> u32 {
    row.iter().map(|k| k.width).sum()
}

/// Letter keys for the space-separated captions in `row`, with `lang`'s alternates.
fn keys(lang: &str, row: &str) -> Vec<KeySpec> {
    row.split(' ').map(|k| KeySpec::ch(k, alternates(lang, k))).collect()
}

/// With fewer than two languages the globe key has nothing to switch: it goes and the
/// space bar takes its width.
fn fit_languages(mut layout: KeyboardLayout, languages: usize) -> KeyboardLayout {
    if languages >= 2 {
        return layout;
    }
    for row in &mut layout.rows {
        let Some(g) = row.iter().position(|k| k.action == KeyAction::NextLanguage) else { continue };
        let w = row.remove(g).width;
        if let Some(space) = row.iter_mut().find(|k| k.action == KeyAction::Space) {
            space.width += w;
        }
    }
    layout
}

impl KeyboardLayout {
    /// English QWERTY (the phase 0 name).
    pub fn qwerty_en() -> Self {
        Self::letters("en").expect("en layout")
    }

    /// The letter layout of a language (`en`, `da`, `de`, `fr`, `es`, `sv`, `nb`).
    pub fn letters(lang: &str) -> Option<Self> {
        let rows: [&str; 3] = match lang {
            "en" => ["q w e r t y u i o p", "a s d f g h j k l", "z x c v b n m"],
            "da" => ["q w e r t y u i o p å", "a s d f g h j k l æ ø", "z x c v b n m"],
            "de" => ["q w e r t z u i o p ü", "a s d f g h j k l ö ä", "y x c v b n m ß"],
            "fr" => ["a z e r t y u i o p", "q s d f g h j k l m", "w x c v b n '"],
            "es" => ["q w e r t y u i o p", "a s d f g h j k l ñ", "z x c v b n m"],
            "sv" => ["q w e r t y u i o p å", "a s d f g h j k l ö ä", "z x c v b n m"],
            "nb" => ["q w e r t y u i o p å", "a s d f g h j k l ø æ", "z x c v b n m"],
            _ => return None,
        };
        let (r1, r2, letters3) = (keys(lang, rows[0]), keys(lang, rows[1]), keys(lang, rows[2]));
        let total = units(&r1).max(units(&r2)).max(units(&letters3) + 6);
        let side = (total - units(&letters3)) / 2;
        let mut r3 = vec![KeySpec::special("shift", KeyAction::Shift, side, Some(Icon::Shift))];
        r3.extend(letters3);
        r3.push(KeySpec::special("del", KeyAction::Backspace, total - units(&r3), Some(Icon::Backspace)));
        let mut bottom = Self::bottom_row("?123", "symbols", ".", total);
        // The "." key keeps the language's punctuation alternates (Spanish ¿ ¡).
        if let Some(k) = bottom.iter_mut().find(|k| k.label == ".") {
            k.alternates = alternates(lang, ".").iter().map(|s| s.to_string()).collect();
        }
        Some(KeyboardLayout { name: lang.into(), rows: vec![r1, r2, r3, bottom] })
    }

    fn bottom_row(switch_label: &str, switch_to: &str, extra: &str, total: u32) -> Vec<KeySpec> {
        let mut r = vec![
            KeySpec::special(switch_label, KeyAction::Layout(switch_to.into()), 3, None),
            KeySpec::special("globe", KeyAction::NextLanguage, 2, Some(Icon::Globe)),
            KeySpec::special("space", KeyAction::Space, 0, None),
            KeySpec::ch(extra, alternates("", extra)),
            KeySpec::special("enter", KeyAction::Enter, 3, Some(Icon::Enter)),
            KeySpec::special("hide", KeyAction::Close, 2, Some(Icon::KeyboardHide)),
        ];
        r[2].width = total.saturating_sub(units(&r)).max(4);
        r
    }

    /// Digits and punctuation.
    pub fn symbols() -> Self {
        Self::symbol_page("symbols", ["1 2 3 4 5 6 7 8 9 0", "@ # € % & * - + ( )", ". , ? ! ' \" :"], ("=\\<", "symbols2"), "/")
    }

    /// More symbols.
    pub fn symbols2() -> Self {
        Self::symbol_page("symbols2", ["~ ` | • ° ± ÷ × ¶ §", "£ ¥ $ ¢ ^ _ = { } \\", "< > [ ] ; © ®"], ("?123", "symbols"), "…")
    }

    /// A page of symbol keys: `rows` of keys, the third between a key that switches to
    /// the other page (`(caption, layout)`) and backspace, then the bottom row with
    /// `extra` next to the space bar.
    fn symbol_page(name: &str, rows: [&str; 3], other: (&str, &str), extra: &str) -> Self {
        let mut r3 = vec![KeySpec::special(other.0, KeyAction::Layout(other.1.into()), 3, None)];
        r3.extend(keys("", rows[2]));
        r3.push(KeySpec::special("del", KeyAction::Backspace, 3, Some(Icon::Backspace)));
        KeyboardLayout { name: name.into(), rows: vec![keys("", rows[0]), keys("", rows[1]), r3, Self::bottom_row("abc", "letters", extra, 20)] }
    }

    /// A layout by name (`letters` is not a layout: the keyboard maps it to its language).
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "symbols" => Some(Self::symbols()),
            "symbols2" => Some(Self::symbols2()),
            lang => Self::letters(lang),
        }
    }

    /// Key faces inside `area`: rows of equal height, keys centred per row, [`GAP`] px
    /// between neighbouring keys and rows. The faces of a full row span `area.w`.
    pub fn key_rects(&self, area: Rect) -> Vec<(Rect, &KeySpec)> {
        let total = self.rows.iter().map(|r| units(r)).max().unwrap_or(1).max(1) as f32;
        let n = self.rows.len().max(1) as f32;
        let gap = GAP as f32;
        let unit = (area.w as f32 + gap) / total;
        let pitch = (area.h as f32 + gap) / n;
        let mut out = Vec::new();
        for (ri, row) in self.rows.iter().enumerate() {
            let y0 = area.y + (ri as f32 * pitch).round() as i32;
            let y1 = area.y + ((ri as f32 + 1.0) * pitch).round() as i32 - GAP as i32;
            let offset = (total - units(row) as f32) / 2.0 * unit;
            let mut cum = 0u32;
            for k in row {
                let x0 = area.x + (offset + cum as f32 * unit).round() as i32;
                cum += k.width;
                let x1 = area.x + (offset + cum as f32 * unit).round() as i32 - GAP as i32;
                out.push((Rect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32), k));
            }
        }
        out
    }

    /// The key at `p` inside `area`.
    pub fn key_at(&self, area: Rect, p: Point) -> Option<&KeySpec> {
        self.index_at(area, p).map(|i| self.key_rects(area)[i].1)
    }

    /// Index of the key at `p`: a point in a gap or in the outer padding goes to the
    /// nearest key within `GAP / 2 + PAD` px, so no tap between keys is lost.
    fn index_at(&self, area: Rect, p: Point) -> Option<usize> {
        let reach = (GAP / 2 + PAD) as i32;
        let dist = |r: &Rect| {
            let dx = (r.x - p.x).max(p.x - (r.right() - 1)).max(0);
            let dy = (r.y - p.y).max(p.y - (r.bottom() - 1)).max(0);
            dx.max(dy)
        };
        self.key_rects(area).iter().enumerate().map(|(i, (r, _))| (dist(r), i)).filter(|(d, _)| *d <= reach).min().map(|(_, i)| i)
    }
}

/// Height of one key row in px (KOReader: 64 * 300 / 160 on the 300 dpi PW12).
pub const ROW_H: u32 = 120;
/// Space between neighbouring keys and rows, in px.
pub const GAP: u32 = 4;
/// Space between the widget edge and the outer keys, in px.
pub const PAD: u32 = 4;
/// Px at the bottom of the screen left free below the keyboard.
pub const BOTTOM_MARGIN: u32 = 8;
/// Letter caption size in px (KOReader: 22 pt scaled, about 41 px).
const LABEL_PX: f32 = 44.0;

/// `field` moved up so that it ends `gap` px above the top of `keyboard` when the
/// keyboard would cover it; unchanged when it is above already. Text fields of sheets,
/// dialogs and chat views use it so the field being typed in stays in sight (the plugin
/// chat view's input bar is lifted onto the keyboard's top edge this way).
pub fn lift_above(field: Rect, keyboard: Rect, gap: u32) -> Rect {
    let limit = keyboard.y - gap as i32;
    if keyboard.is_empty() || field.bottom() <= limit {
        return field;
    }
    Rect::new(field.x, limit - field.h as i32, field.w, field.h)
}

#[derive(Clone, Debug, PartialEq)]
struct Press {
    slot: u8,
    key: usize,
    repeated: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct AltPopup {
    slot: u8,
    options: Vec<String>,
    selected: usize,
    rect: Rect,
}

/// The keyboard widget.
#[derive(Clone, Debug)]
pub struct Keyboard {
    /// Current layout.
    pub layout: KeyboardLayout,
    /// Shift is on (one-shot).
    pub shift: bool,
    /// Capitals locked (hold shift).
    pub caps_lock: bool,
    /// Languages the globe key cycles through.
    pub languages: Vec<String>,
    /// Index into `languages`.
    pub language: usize,
    /// Show a key preview bubble over the row above the pressed letter (off by
    /// default, as in KOReader: the pressed key's border is the feedback).
    pub preview: bool,
    pressed: Vec<Press>,
    popup: Option<AltPopup>,
    queue: Vec<KeyAction>,
    changed: Vec<Rect>,
    full: bool,
    bounds: Rect,
}

impl Keyboard {
    /// English keyboard (globe key cycles English, Danish, German, French).
    pub fn new() -> Self {
        Self::with_languages(&LANGUAGES)
    }

    /// A keyboard for `languages` (unknown ones are skipped), starting with the first.
    pub fn with_languages(languages: &[&str]) -> Self {
        let mut langs: Vec<String> = languages.iter().filter(|l| KeyboardLayout::letters(l).is_some()).map(|l| l.to_string()).collect();
        if langs.is_empty() {
            langs.push("en".into());
        }
        let layout = fit_languages(KeyboardLayout::letters(&langs[0]).expect("known language"), langs.len());
        Keyboard {
            layout,
            shift: false,
            caps_lock: false,
            languages: langs,
            language: 0,
            preview: false,
            pressed: Vec::new(),
            popup: None,
            queue: Vec::new(),
            changed: Vec::new(),
            full: false,
            bounds: Rect::default(),
        }
    }

    /// A keyboard for the language codes in `langs` (e.g. the `ui.keyboard_layouts`
    /// setting): unknown names are skipped, an empty list gives English; the first is
    /// the default and the globe key cycles them in order.
    pub fn with_language_list(langs: &[String]) -> Self {
        let v: Vec<&str> = langs.iter().map(String::as_str).collect();
        Self::with_languages(&v)
    }

    /// The current language code.
    pub fn current_language(&self) -> &str {
        &self.languages[self.language]
    }

    /// Switches to a language's letter layout.
    pub fn set_language(&mut self, lang: &str) -> bool {
        match (self.languages.iter().position(|l| l == lang), KeyboardLayout::letters(lang)) {
            (Some(i), Some(layout)) => {
                self.language = i;
                self.layout = fit_languages(layout, self.languages.len());
                self.full = true;
                true
            }
            _ => false,
        }
    }

    /// Takes the queued key actions (characters already cased).
    pub fn take_actions(&mut self) -> Vec<KeyAction> {
        std::mem::take(&mut self.queue)
    }

    /// Action for a reported `key:<label>` name (the phase 0 tap API).
    pub fn action_for(&self, action: &str) -> Option<KeyAction> {
        let label = action.strip_prefix("key:")?;
        let k = self.layout.rows.iter().flatten().find(|k| k.label == label)?;
        Some(match &k.action {
            KeyAction::Char(c) => KeyAction::Char(self.cased(c)),
            a => a.clone(),
        })
    }

    fn upper(&self) -> bool {
        self.shift || self.caps_lock
    }

    fn cased(&self, s: &str) -> String {
        if self.upper() {
            s.to_uppercase()
        } else {
            s.to_string()
        }
    }

    /// Height of a keyboard with `rows` key rows: `rows * ROW_H + 2 * PAD`.
    pub fn preferred_height(rows: usize) -> u32 {
        rows as u32 * ROW_H + 2 * PAD
    }

    /// Where every screen puts the keyboard: full width at the bottom of a `screen`,
    /// [`Keyboard::preferred_height`] of the 4-row layouts tall, ending
    /// [`BOTTOM_MARGIN`] px above the bottom edge.
    pub fn bottom_rect(screen: Size) -> Rect {
        let h = Self::preferred_height(4).min(screen.h.saturating_sub(BOTTOM_MARGIN));
        Rect::new(0, screen.h.saturating_sub(BOTTOM_MARGIN + h) as i32, screen.w, h)
    }

    /// The key area inside the widget bounds: the bounds less [`PAD`] on every side.
    pub fn keys_area(&self, b: Rect) -> Rect {
        crate::draw::inset(b, PAD as i32)
    }

    /// A box one key row tall over the row above `key` (over the key's own row for the
    /// top row), `w` wide from `left`, moved inside `b`.
    fn above(&self, b: Rect, key: Rect, left: i32, w: u32) -> Rect {
        let w = w.min(b.w);
        let x = left.clamp(b.x, (b.right() - w as i32).max(b.x));
        let up = key.y - (key.h + GAP) as i32;
        let y = if up >= b.y { up } else { key.y };
        Rect::new(x, y, w, key.h)
    }

    /// Rectangle of the preview bubble for a key.
    fn bubble(&self, b: Rect, key: Rect) -> Rect {
        let w = key.w + key.w / 2;
        self.above(b, key, key.x + key.w as i32 / 2 - w as i32 / 2, w)
    }

    fn key_rect(&self, b: Rect, i: usize) -> Option<(Rect, KeySpec)> {
        self.layout.key_rects(self.keys_area(b)).get(i).map(|(r, k)| (*r, (*k).clone()))
    }

    /// Queues the face of key `i` (and its bubble) for a partial refresh.
    fn mark_key(&mut self, b: Rect, i: usize) {
        if let Some((r, k)) = self.key_rect(b, i) {
            self.changed.push(r);
            if self.preview && matches!(k.action, KeyAction::Char(_)) {
                let br = self.bubble(b, r);
                self.changed.push(br);
            }
        }
    }

    fn switch_layout(&mut self, name: &str) {
        let name = if name == "letters" { self.current_language().to_string() } else { name.to_string() };
        if let Some(l) = KeyboardLayout::by_name(&name) {
            self.layout = fit_languages(l, self.languages.len());
            self.full = true;
        }
    }

    /// Runs a key's action on release (or on a repeat).
    fn commit(&mut self, k: &KeySpec) {
        match &k.action {
            KeyAction::Shift => {
                if self.caps_lock {
                    self.caps_lock = false;
                    self.shift = false;
                } else {
                    self.shift = !self.shift;
                }
                self.full = true;
            }
            KeyAction::Layout(name) => self.switch_layout(&name.clone()),
            KeyAction::NextLanguage if self.languages.len() < 2 => {}
            KeyAction::NextLanguage => {
                self.language = (self.language + 1) % self.languages.len();
                let lang = self.current_language().to_string();
                self.switch_layout(&lang);
                self.queue.push(KeyAction::NextLanguage);
            }
            KeyAction::Char(c) => {
                self.queue.push(KeyAction::Char(self.cased(c)));
                if self.shift && !self.caps_lock {
                    self.shift = false;
                    self.full = true;
                }
            }
            a => self.queue.push(a.clone()),
        }
    }

    fn touch(&mut self, t: &Touch, b: Rect) -> EventResult {
        let area = self.keys_area(b);
        match t.kind {
            // Keys show feedback on touch-down already (typing needs it).
            TouchKind::Press => EventResult::Ignored,
            TouchKind::Down => {
                let Some(i) = self.layout.index_at(area, t.pos) else { return EventResult::Handled };
                self.pressed.retain(|p| p.slot != t.slot);
                self.pressed.push(Press { slot: t.slot, key: i, repeated: false });
                self.mark_key(b, i);
                EventResult::Handled
            }
            TouchKind::Move => {
                if let Some(p) = self.popup.as_mut().filter(|p| p.slot == t.slot) {
                    let cell = p.rect.w / p.options.len().max(1) as u32;
                    let sel = (((t.pos.x - p.rect.x).max(0) as u32 / cell.max(1)) as usize).min(p.options.len() - 1);
                    if sel != p.selected {
                        p.selected = sel;
                        let r = p.rect;
                        self.changed.push(r);
                        return EventResult::Handled;
                    }
                    return EventResult::Ignored;
                }
                let Some(pi) = self.pressed.iter().position(|p| p.slot == t.slot) else { return EventResult::Ignored };
                match self.layout.index_at(area, t.pos) {
                    Some(i) if i != self.pressed[pi].key && !self.pressed[pi].repeated => {
                        let old = self.pressed[pi].key;
                        self.pressed[pi].key = i;
                        self.mark_key(b, old);
                        self.mark_key(b, i);
                        EventResult::Handled
                    }
                    _ => EventResult::Ignored,
                }
            }
            TouchKind::Hold => {
                let Some(pi) = self.pressed.iter().position(|p| p.slot == t.slot) else { return EventResult::Ignored };
                if self.popup.is_some() {
                    return EventResult::Ignored;
                }
                let Some((r, k)) = self.key_rect(b, self.pressed[pi].key) else { return EventResult::Ignored };
                match &k.action {
                    KeyAction::Char(c) if !k.alternates.is_empty() && !self.pressed[pi].repeated => {
                        let mut options = vec![self.cased(c)];
                        options.extend(k.alternates.iter().map(|a| self.cased(a)));
                        let cell = r.w.max(100);
                        let w = cell * options.len() as u32;
                        // The first option (the base letter) sits over the pressed key.
                        let rect = self.above(b, r, r.x + r.w as i32 / 2 - cell as i32 / 2, w);
                        // The preview bubble goes; the popup replaces it.
                        self.mark_key(b, self.pressed[pi].key);
                        self.changed.push(rect);
                        self.pressed[pi].repeated = true;
                        self.popup = Some(AltPopup { slot: t.slot, options, selected: 0, rect });
                        EventResult::Handled
                    }
                    KeyAction::Shift if !self.pressed[pi].repeated => {
                        self.caps_lock = true;
                        self.shift = true;
                        self.pressed[pi].repeated = true;
                        self.full = true;
                        EventResult::Handled
                    }
                    a if a.repeats() => {
                        self.pressed[pi].repeated = true;
                        let k = k.clone();
                        self.commit(&k);
                        EventResult::Action("keyboard".into())
                    }
                    _ => EventResult::Ignored,
                }
            }
            TouchKind::Cancel => {
                // The contact ended without a lift (rotation, lost release): forget the
                // press and the alternates popup; type nothing.
                let mut handled = false;
                if let Some(p) = self.popup.take_if(|p| p.slot == t.slot) {
                    self.changed.push(p.rect);
                    handled = true;
                }
                if let Some(pi) = self.pressed.iter().position(|p| p.slot == t.slot) {
                    let press = self.pressed.remove(pi);
                    self.mark_key(b, press.key);
                    handled = true;
                }
                if handled {
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            TouchKind::Up => {
                if let Some(p) = self.popup.take_if(|p| p.slot == t.slot) {
                    self.changed.push(p.rect);
                    if let Some(pi) = self.pressed.iter().position(|p| p.slot == t.slot) {
                        let press = self.pressed.remove(pi);
                        self.mark_key(b, press.key);
                    }
                    self.queue.push(KeyAction::Char(p.options[p.selected].clone()));
                    if self.shift && !self.caps_lock {
                        self.shift = false;
                        self.full = true;
                    }
                    return EventResult::Action("keyboard".into());
                }
                let Some(pi) = self.pressed.iter().position(|p| p.slot == t.slot) else { return EventResult::Ignored };
                let press = self.pressed.remove(pi);
                self.mark_key(b, press.key);
                if !press.repeated {
                    if let Some((_, k)) = self.key_rect(b, press.key) {
                        self.commit(&k);
                    }
                }
                if self.queue.is_empty() {
                    EventResult::Handled
                } else {
                    EventResult::Action("keyboard".into())
                }
            }
        }
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for Keyboard {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        self.bounds = b;
        let (fg, bg) = (cx.theme.fg, cx.theme.bg);
        // Flat board: white, a rule on top, keys with a thin border (KOReader style).
        cx.canvas.fill_rect(b.x, b.y, b.w as i32, b.h as i32, bg);
        cx.canvas.fill_rect(b.x, b.y, b.w as i32, 2, fg);
        let rects = self.layout.key_rects(self.keys_area(b));
        let radius = 6.0;
        for (i, &(face, k)) in rects.iter().enumerate() {
            let special = !matches!(k.action, KeyAction::Char(_));
            let down = self.pressed.iter().any(|p| p.key == i) || (k.action == KeyAction::Shift && self.upper());
            // Every face is white (special keys too), as the board under it already is; a
            // pressed key (and shift while on) only gets a thicker border, inside its
            // face. Nothing else in the face changes, so the refresh changes few pixels.
            stroke_round_rect(cx.canvas, face, radius, if down { PRESS_BORDER } else { 2.0 }, fg);
            let icon = match (&k.action, k.icon) {
                (KeyAction::Shift, _) if self.caps_lock => Some(Icon::ShiftLocked),
                (_, i) => i,
            };
            if let Some(icon) = icon {
                let side = (face.h.min(face.w) as f32 * 0.45) as u32;
                cx.icon(icon, face, side, fg);
            } else if k.action == KeyAction::Space {
                // The language name only tells something when the globe can change it.
                if self.languages.len() < 2 {
                    continue;
                }
                let name = language_name(self.current_language());
                cx.text_centered(face, name, LABEL_PX * 0.7, Style::Italic, cx.theme.muted);
            } else {
                let label = if matches!(k.action, KeyAction::Char(_)) { self.cased(&k.label) } else { k.label.clone() };
                let size = if special { LABEL_PX * 0.75 } else { LABEL_PX };
                cx.text_centered(face, &label, size, Style::Regular, fg);
                if !k.alternates.is_empty() {
                    // A small dot marks keys with alternates.
                    crate::draw::Path::new().circle(face.right() as f32 - 14.0, face.y as f32 + 14.0, 3.5).fill(cx.canvas, cx.theme.muted);
                }
            }
        }
        // Key preview bubbles (off by default).
        if self.preview {
            for p in &self.pressed {
                if self.popup.as_ref().is_some_and(|pp| pp.slot == p.slot) {
                    continue;
                }
                let Some(&(r, k)) = rects.get(p.key) else { continue };
                if let KeyAction::Char(c) = &k.action {
                    let br = self.bubble(b, r);
                    fill_round_rect(cx.canvas, br, radius, bg);
                    stroke_round_rect(cx.canvas, br, radius, 3.0, fg);
                    let label = self.cased(c);
                    cx.text_centered(br, &label, LABEL_PX * 1.4, Style::Bold, fg);
                }
            }
        }
        // Alternates popup.
        if let Some(p) = &self.popup {
            fill_round_rect(cx.canvas, p.rect, radius, bg);
            stroke_round_rect(cx.canvas, p.rect, radius, 3.0, fg);
            let cell = p.rect.w / p.options.len().max(1) as u32;
            for (i, o) in p.options.iter().enumerate() {
                let c = Rect::new(p.rect.x + (i as u32 * cell) as i32, p.rect.y, cell, p.rect.h);
                let sel = i == p.selected;
                if sel {
                    fill_round_rect(cx.canvas, crate::draw::inset(c, 6), radius, fg);
                }
                cx.text_centered(c, o, LABEL_PX * 1.1, Style::Regular, if sel { bg } else { fg });
            }
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match ev {
            UiEvent::Touch(t) => self.touch(t, b),
            // Gestures are made of touches the keyboard already handled.
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        // Every key shows gray (antialiased captions, the alternates
        // dot): GL16 keeps it, DU would turn it black or white. Only the changed key
        // faces are refreshed, so it stays a small partial update.
        RefreshClass::Gl16
    }

    fn damage(&mut self, _b: Rect) -> Option<Vec<Rect>> {
        let changed = std::mem::take(&mut self.changed);
        if std::mem::take(&mut self.full) {
            return None;
        }
        Some(changed)
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn lift_above_moves_a_covered_field_onto_the_keyboard() {
        let kb = Keyboard::bottom_rect(Size::new(1272, 1696));
        let low = Rect::new(0, 1560, 1272, 136);
        let lifted = lift_above(low, kb, 0);
        assert_eq!((lifted.x, lifted.w, lifted.h, lifted.bottom()), (0, 1272, 136, kb.y));
        assert_eq!(lift_above(low, kb, 16).bottom(), kb.y - 16);
        let high = Rect::new(32, 200, 400, 100);
        assert_eq!(lift_above(high, kb, 16), high, "a field above the keyboard stays");
        assert_eq!(lift_above(low, Rect::default(), 0), low, "no keyboard");
    }
    use super::*;
    use crate::theme::Theme;
    use crate::widget::WidgetTree;
    use ink_device::Canvas;
    use ink_text::Fonts;
    use std::time::Duration;

    const AREA: Rect = Rect::new(0, 1000, 1272, 680);

    fn t(slot: u8, kind: TouchKind, p: Point) -> UiEvent {
        UiEvent::Touch(Touch { slot, kind, pos: p, start: p, held: Duration::ZERO, moved: false })
    }

    fn center_of(kb: &Keyboard, label: &str) -> Point {
        let (r, _) = kb.layout.key_rects(kb.keys_area(AREA)).into_iter().find(|(_, k)| k.label == label).unwrap_or_else(|| panic!("{label}"));
        Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2)
    }

    fn press(kb: &mut Keyboard, label: &str) -> Vec<KeyAction> {
        let p = center_of(kb, label);
        kb.event(&t(0, TouchKind::Down, p), AREA);
        kb.event(&t(0, TouchKind::Up, p), AREA);
        kb.take_actions()
    }

    fn chars(a: &[KeyAction]) -> String {
        a.iter()
            .map(|a| match a {
                KeyAction::Char(c) => c.clone(),
                KeyAction::Space => " ".into(),
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn hit_testing_and_actions() {
        let area = Rect::new(0, 1000, 1272, 600);
        let l = KeyboardLayout::qwerty_en();
        let rects = l.key_rects(area);
        assert_eq!(rects.len(), 10 + 9 + 9 + 6);
        for (r, _) in &rects {
            assert!(area.intersect(r) == *r, "{r:?} inside the area");
        }
        let (q_rect, _) = rects[0];
        let q = l.key_at(area, Point::new(q_rect.x + 5, q_rect.y + 5)).unwrap();
        assert_eq!(q.label, "q");
        let mut kb = Keyboard::new();
        assert_eq!(kb.action_for("key:q"), Some(KeyAction::Char("q".into())));
        kb.shift = true;
        assert_eq!(kb.action_for("key:q"), Some(KeyAction::Char("Q".into())));
        assert_eq!(kb.action_for("nope"), None);
        let a = l.rows[1][0].clone();
        assert!(a.alternates.contains(&"å".to_string()));
    }

    #[test]
    fn layouts_for_every_language() {
        for lang in LANGUAGES {
            let l = KeyboardLayout::letters(lang).unwrap();
            // Every row fills the same width (in key units) and the bottom row has space.
            let widths: Vec<u32> = l.rows.iter().map(|r| units(r)).collect();
            let total = *widths.iter().max().unwrap();
            assert!(widths[2] == total && widths[3] == total, "{lang}: {widths:?}");
            assert!(l.rows[3].iter().any(|k| k.action == KeyAction::Space));
        }
        let letters = |lang: &str| -> String {
            KeyboardLayout::letters(lang).unwrap().rows.iter().flatten().filter_map(|k| match &k.action {
                KeyAction::Char(c) => Some(c.clone()),
                _ => None,
            }).collect()
        };
        assert!(letters("da").contains("på") && letters("da").contains("læø"));
        assert!(letters("de").starts_with("qwertzuiopü") && letters("de").contains('ß'));
        assert!(letters("fr").starts_with("azertyuiop"));
        assert!(letters("es").contains("klñ"));
        assert!(letters("sv").contains("på") && letters("sv").contains("löä"));
        assert!(letters("nb").contains("på") && letters("nb").contains("løæ"));
        // Only the given languages are cycled; one language: the globe does nothing.
        let mut kb = Keyboard::with_language_list(&["sv".into(), "xx".into(), "es".into()]);
        assert_eq!(kb.languages, vec!["sv".to_string(), "es".to_string()]);
        assert_eq!(press(&mut kb, "globe"), vec![KeyAction::NextLanguage]);
        assert_eq!(kb.layout.name, "es");
        press(&mut kb, "globe");
        assert_eq!(kb.layout.name, "sv");
        // One language: no globe key on any layout; the space bar takes its width.
        let mut one = Keyboard::with_language_list(&[]);
        assert_eq!(one.layout.name, "en");
        let space_w = |kb: &Keyboard| kb.layout.rows[3].iter().find(|k| k.action == KeyAction::Space).unwrap().width;
        assert_eq!(space_w(&one), space_w(&Keyboard::new()) + 2);
        for _ in 0..3 {
            assert!(one.layout.rows.iter().flatten().all(|k| k.action != KeyAction::NextLanguage), "{}", one.layout.name);
            let switch = one.layout.rows[3][0].label.clone();
            press(&mut one, &switch);
        }
        assert!(KeyboardLayout::letters("xx").is_none());
        assert_eq!(KeyboardLayout::by_name("symbols2").unwrap().name, "symbols2");
    }

    #[test]
    fn typing_shift_layouts_and_languages() {
        let mut kb = Keyboard::new();
        assert_eq!(chars(&press(&mut kb, "h")), "h");
        press(&mut kb, "shift");
        assert!(kb.shift);
        assert_eq!(chars(&press(&mut kb, "i")), "I");
        assert!(!kb.shift, "one-shot");
        assert_eq!(press(&mut kb, "space"), vec![KeyAction::Space]);
        assert_eq!(press(&mut kb, "del"), vec![KeyAction::Backspace]);
        assert_eq!(press(&mut kb, "enter"), vec![KeyAction::Enter]);
        // Symbols and back.
        press(&mut kb, "?123");
        assert_eq!(kb.layout.name, "symbols");
        assert_eq!(chars(&press(&mut kb, "7")), "7");
        press(&mut kb, "=\\<");
        assert_eq!(kb.layout.name, "symbols2");
        assert_eq!(chars(&press(&mut kb, "§")), "§");
        press(&mut kb, "abc");
        assert_eq!(kb.layout.name, "en");
        // The globe key cycles en -> da -> de -> fr -> es -> sv -> nb -> en.
        assert_eq!(press(&mut kb, "globe"), vec![KeyAction::NextLanguage]);
        assert_eq!((kb.current_language(), kb.layout.name.as_str()), ("da", "da"));
        assert_eq!(chars(&press(&mut kb, "ø")), "ø");
        press(&mut kb, "globe");
        assert_eq!(kb.layout.name, "de");
        press(&mut kb, "?123");
        press(&mut kb, "abc");
        assert_eq!(kb.layout.name, "de", "abc returns to the current language");
        for _ in 0..5 {
            press(&mut kb, "globe");
        }
        assert_eq!(kb.layout.name, "en");
        assert!(kb.set_language("fr") && !kb.set_language("xx"));
    }

    #[test]
    fn long_press_alternates() {
        let mut kb = Keyboard::new();
        let e = center_of(&kb, "e");
        kb.event(&t(0, TouchKind::Down, e), AREA);
        assert_eq!(kb.event(&t(0, TouchKind::Hold, e), AREA), EventResult::Handled);
        let popup = kb.popup.clone().expect("popup open");
        assert_eq!(popup.options[0], "e");
        assert!(popup.options.contains(&"é".to_string()));
        assert!(popup.rect.contains(e), "top row: the popup covers the key's own row");
        // Slide to the second option (è) and lift.
        let cell = popup.rect.w / popup.options.len() as u32;
        let p = Point::new(popup.rect.x + cell as i32 + 5, popup.rect.y + 10);
        assert_eq!(kb.event(&t(0, TouchKind::Move, p), AREA), EventResult::Handled);
        assert_eq!(kb.event(&t(0, TouchKind::Up, p), AREA), EventResult::Action("keyboard".into()));
        assert_eq!(kb.take_actions(), vec![KeyAction::Char(popup.options[1].clone())]);
        assert!(kb.popup.is_none());
        // Shifted alternates are capitals; lifting without moving types the base letter.
        kb.shift = true;
        let a = center_of(&kb, "a");
        kb.event(&t(0, TouchKind::Down, a), AREA);
        kb.event(&t(0, TouchKind::Hold, a), AREA);
        assert!(kb.popup.as_ref().unwrap().options.contains(&"Å".to_string()));
        kb.event(&t(0, TouchKind::Up, a), AREA);
        assert_eq!(kb.take_actions(), vec![KeyAction::Char("A".into())]);
        assert!(!kb.shift);
        // Holding shift locks capitals; tapping it again unlocks.
        let s = center_of(&kb, "shift");
        kb.event(&t(0, TouchKind::Down, s), AREA);
        kb.event(&t(0, TouchKind::Hold, s), AREA);
        kb.event(&t(0, TouchKind::Up, s), AREA);
        assert!(kb.caps_lock);
        assert_eq!(chars(&press(&mut kb, "b")), "B");
        assert_eq!(chars(&press(&mut kb, "c")), "C");
        press(&mut kb, "shift");
        assert!(!kb.caps_lock && !kb.shift);
    }

    #[test]
    fn repeat_on_hold() {
        let mut kb = Keyboard::new();
        let del = center_of(&kb, "del");
        kb.event(&t(0, TouchKind::Down, del), AREA);
        for _ in 0..4 {
            assert_eq!(kb.event(&t(0, TouchKind::Hold, del), AREA), EventResult::Action("keyboard".into()));
        }
        assert_eq!(kb.take_actions(), vec![KeyAction::Backspace; 4]);
        // The release adds nothing after repeats.
        assert_eq!(kb.event(&t(0, TouchKind::Up, del), AREA), EventResult::Handled);
        assert!(kb.take_actions().is_empty());
        // Letters without alternates do not repeat.
        let q = center_of(&kb, "q");
        kb.event(&t(0, TouchKind::Down, q), AREA);
        assert_eq!(kb.event(&t(0, TouchKind::Hold, q), AREA), EventResult::Ignored);
        kb.event(&t(0, TouchKind::Up, q), AREA);
        assert_eq!(chars(&kb.take_actions()), "q");
    }

    #[test]
    fn cancel_types_nothing() {
        let mut kb = Keyboard::new();
        // A pressed letter: Cancel forgets it, no key.
        let q = center_of(&kb, "q");
        kb.event(&t(0, TouchKind::Down, q), AREA);
        assert_eq!(kb.event(&t(0, TouchKind::Cancel, q), AREA), EventResult::Handled);
        assert!(kb.take_actions().is_empty() && kb.pressed.is_empty());
        // An open alternates popup closes without typing.
        let a = center_of(&kb, "a");
        kb.event(&t(0, TouchKind::Down, a), AREA);
        kb.event(&t(0, TouchKind::Hold, a), AREA);
        assert!(kb.popup.is_some());
        assert_eq!(kb.event(&t(0, TouchKind::Cancel, a), AREA), EventResult::Handled);
        assert!(kb.popup.is_none() && kb.take_actions().is_empty());
        // A cancel for a slot with no press is not taken.
        assert_eq!(kb.event(&t(4, TouchKind::Cancel, a), AREA), EventResult::Ignored);
        // Typing works afterwards.
        assert_eq!(chars(&press(&mut kb, "b")), "b");
    }

    #[test]
    fn two_thumbs_overlap_and_sliding_corrects_the_key() {
        let mut kb = Keyboard::new();
        let (h, i) = (center_of(&kb, "h"), center_of(&kb, "i"));
        kb.event(&t(0, TouchKind::Down, h), AREA);
        kb.event(&t(1, TouchKind::Down, i), AREA);
        kb.event(&t(0, TouchKind::Up, h), AREA);
        kb.event(&t(1, TouchKind::Up, i), AREA);
        assert_eq!(chars(&kb.take_actions()), "hi");
        // Down on g, slide to h, lift: h.
        let g = center_of(&kb, "g");
        kb.event(&t(0, TouchKind::Down, g), AREA);
        kb.event(&t(0, TouchKind::Move, h), AREA);
        kb.event(&t(0, TouchKind::Up, h), AREA);
        assert_eq!(chars(&kb.take_actions()), "h");
    }

    #[test]
    fn press_damage_is_the_key_face_only() {
        let theme = Theme::day();
        let mut fonts = Fonts::load();
        let mut canvas = Canvas::new(1272, 1696);
        let mut tree = WidgetTree::new();
        let id = tree.add(None, Box::new(Keyboard::new()), AREA);
        tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        let kb = tree.get_mut::<Keyboard>(id).unwrap();
        let k = center_of(kb, "k");
        tree.dispatch(&t(0, TouchKind::Down, k));
        let dmg = tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        // Only the pressed key's face, with the gray-preserving GL16.
        let kb = tree.get_mut::<Keyboard>(id).unwrap();
        let (face, _) = kb.layout.key_rects(kb.keys_area(AREA)).into_iter().find(|(_, k)| k.label == "k").unwrap();
        assert_eq!(dmg.iter().map(|d| (d.rect, d.class)).collect::<Vec<_>>(), vec![(face, RefreshClass::Gl16)]);
        // The key gets a thick border; its inside stays white apart from the caption.
        let mid = face.y + face.h as i32 / 2;
        let border = (face.x..face.x + PRESS_BORDER as i32).all(|x| canvas.get(x, mid).unwrap() < 64);
        assert!(border, "a {PRESS_BORDER} px border at the left edge");
        let inner = crate::draw::inset(face, PRESS_BORDER as i32 + 2);
        let dark = (inner.y..inner.bottom()).flat_map(|y| (inner.x..inner.right()).map(move |x| (x, y))).filter(|(x, y)| canvas.get(*x, *y).unwrap() < 64).count();
        assert!((dark as u32) < inner.w * inner.h / 4, "not inverted: {dark}");
        // Release: the key face is refreshed again and typing reports an action.
        assert_eq!(tree.dispatch(&t(0, TouchKind::Up, k)).map(|r| r.1), Some(EventResult::Action("keyboard".into())));
        let dmg = tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        assert!(!dmg.is_empty());
        // A special key (backspace) comes back with GL16, only its rectangle.
        let kb = tree.get_mut::<Keyboard>(id).unwrap();
        let d = center_of(kb, "del");
        tree.dispatch(&t(0, TouchKind::Down, d));
        tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        tree.dispatch(&t(0, TouchKind::Up, d));
        let dmg = tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        assert!(dmg.iter().all(|x| x.class == RefreshClass::Gl16 && x.rect.w < 300), "{dmg:?}");
        // Shift changes every caption: a full repaint.
        let kb = tree.get_mut::<Keyboard>(id).unwrap();
        let s = center_of(kb, "shift");
        tree.dispatch(&t(0, TouchKind::Down, s));
        tree.dispatch(&t(0, TouchKind::Up, s));
        let dmg = tree.paint(&mut PaintCx::new(&mut canvas, &mut fonts, &theme));
        assert_eq!(dmg.len(), 1);
        assert_eq!((dmg[0].rect, dmg[0].class), (AREA, RefreshClass::Gl16), "gray key faces need GL16");
    }

    #[test]
    fn full_width_rows_with_gaps() {
        let b = Keyboard::bottom_rect(Size::new(1272, 1696));
        assert_eq!(b, Rect::new(0, 1696 - 8 - 488, 1272, 488));
        for lang in ["en", "da"] {
            let l = KeyboardLayout::letters(lang).unwrap();
            let area = Keyboard::new().keys_area(b);
            let rects = l.key_rects(area);
            // Row 1 spans the full width less the padding; neighbours are GAP apart.
            let row1: Vec<Rect> = rects.iter().take(l.rows[0].len()).map(|(r, _)| *r).collect();
            assert_eq!((row1[0].x, row1.last().unwrap().right()), (PAD as i32, 1272 - PAD as i32), "{lang}");
            for w in row1.windows(2) {
                assert_eq!(w[1].x - w[0].right(), GAP as i32, "{lang}");
            }
            // Rows are about ROW_H apart and the faces stay inside the area.
            let ys: Vec<i32> = l.rows.iter().scan(0, |i, r| { let y = rects[*i].0.y; *i += r.len(); Some(y) }).collect();
            for w in ys.windows(2) {
                assert!((w[1] - w[0] - ROW_H as i32).abs() <= 1, "{ys:?}");
            }
            assert!(rects.iter().all(|(r, _)| area.intersect(r) == *r && r.h >= ROW_H - GAP - 1));
        }
    }

    #[test]
    fn taps_in_gaps_and_padding_hit_a_key() {
        let b = Keyboard::bottom_rect(Size::new(1272, 1696));
        let kb = Keyboard::new();
        let area = kb.keys_area(b);
        let rects = kb.layout.key_rects(area);
        let (q, w) = (rects[0].0, rects[1].0);
        // Between q and w, left of q in the padding, above q at the board edge.
        assert_eq!(kb.layout.key_at(area, Point::new(q.right() + 1, q.y + 20)).unwrap().label, "q");
        assert_eq!(kb.layout.key_at(area, Point::new(w.x - 1, q.y + 20)).unwrap().label, "w");
        assert_eq!(kb.layout.key_at(area, Point::new(b.x, q.y + 20)).unwrap().label, "q");
        assert_eq!(kb.layout.key_at(area, Point::new(q.x + 5, b.y)).unwrap().label, "q");
        // Far from any key (the empty ends of the short middle row): nothing.
        let a = rects[10].0;
        assert!(kb.layout.key_at(area, Point::new(a.x - 30, a.y + 20)).is_none());
    }
}
