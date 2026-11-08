//! "Aa" panel: a bottom sheet with three tabs.
//!
//! - Text: presets (Compact, Standard, Large, Largest), font size (slider with a live
//!   preview), the font family list (built-in font plus the installed families from
//!   [`crate::fonts`], each name drawn in its own font), justify and hyphenation.
//! - Layout: line spacing, side, top and bottom margins as sliders over a live preview.
//!   Side margins are measured from the screen edges; the top and bottom margins from
//!   the inner edge of their status bar (from the screen edge when that bar is off). The
//!   margins never move the bars ([`super::footer::text_layout`]).
//! - Page: page turn wave on/off (the panel's hardware sweep), "Status bars...", full
//!   refresh interval, night mode.
//!
//! Every numeric value is a slider row: caption and value, a minus button, the track and
//! a plus button (104 px buttons). While a finger drags a track only the value and the
//! preview block are drawn again (DU); the book is laid out again once, 150 ms after the
//! last change ([`DEBOUNCE`]; [`relayout_timeout`] and `ReaderScreen::aa_flush`), never
//! per drag frame. Text/Layout values are saved for this book (`BookState.settings` in
//! the sidecar); "Use for all books" also writes them to `settings.toml` as the
//! defaults. Page-tab values are app-wide (`apply_global`).
//!
//! The top row shows "Back to p. N" when the reader came to this page by a link
//! ([`ReaderScreen::link_return_page`], [`ReaderScreen::go_back_from_link`]).
//!
//! The controls share one [`Shown`] record: a control updates it from its own action and
//! every control reads it before painting, so after a preset (one action that changes
//! several values) the whole sheet shows the new values when the reader repaints it.
//!
//! Actions: `aa-size:PX`, `aa-spacing:F`, `aa-margin:PX`, `aa-mtop:PX`, `aa-mbottom:PX`,
//! `aa-justify:on|off`, `aa-hyph:on|off`, `aa-publisher:on|off` (the book's CSS),
//! `aa-pspace:EM` (space after paragraphs; Layout tab), `aa-indent:EM` (first-line indent,
//! negative: the book's own; Layout tab), `aa-weight:N` (darker text 0..=2; Text tab),
//! `aa-preset:I`, `aa-font:NAME` (empty: the
//! default family), `aa-anim:on|off`, `aa-night:on|off`, `aa-flash:N`, `aa-statusbars`,
//! `aa-linkback`, `aa-global`, `close`. A tab tap reports `aa` (the reader builds the
//! sheet again; the tab is remembered here).

use super::footer;
#[cfg(test)] use super::footer::Edge;
use super::stored_layout;
use super::overlay::{Overlay, OverlayKind};
use super::ReaderScreen;
use crate::fonts::{self, BUILTIN};
use crate::router::{AppCtx, Route, Transition};
use ink_device::{Point, Rect, RefreshClass, Size};
use ink_doc::LayoutSettings;
use ink_store::settings::{INDENT_BOOK, WEIGHT_MAX};
use ink_store::sidecar::BookSettings;
use ink_store::Settings;
use ink_text::Style;
use ink_ui::draw::{fill_round_rect, stroke_round_rect, Path};
use ink_ui::{BottomSheet, Button, EventResult, Gesture, Icon, Label, PaintCx, Segmented, Tabs, Toggle, TouchKind, UiEvent, Widget};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Font size range in pixels (about 6 to 17 pt on the PW12's 300 dpi).
pub const FONT_PX: (f32, f32, f32) = (24.0, 72.0, 2.0);
/// Line spacing range.
pub const SPACING: (f32, f32, f32) = (1.0, 2.2, 0.1);
/// Side margin range in pixels (from the screen edges; the text column keeps at least
/// [`footer::MIN_TEXT_PX`]).
pub const MARGIN_PX: (f32, f32, f32) = (0.0, 450.0, 10.0);
/// Top margin range in pixels, below the top status bar.
pub const MARGIN_TOP_PX: (f32, f32, f32) = (0.0, 450.0, 10.0);
/// Bottom margin range in pixels, above the bottom status bar.
pub const MARGIN_BOTTOM_PX: (f32, f32, f32) = (0.0, 450.0, 10.0);
/// Paragraph spacing choices (extra em after a paragraph).
pub const PARA_SPACINGS: [(f32, &str); 3] = [(0.0, "None"), (0.5, "0.5 em"), (1.0, "1 em")];
/// First-line indent choices in em; a negative value keeps the book's own indent.
pub const INDENTS: [(f32, &str); 5] = [(INDENT_BOOK, "Book"), (0.0, "None"), (1.0, "1 em"), (1.5, "1.5 em"), (2.0, "2 em")];
/// Text weight choices (extra stroke width of the drawn text).
pub const WEIGHTS: [(u8, &str); 3] = [(0, "Normal"), (1, "Darker"), (2, "Darkest")];
/// Sheet height (portrait).
/// Portrait sheet height: the Layout tab (preview, four sliders, two option rows) and
/// the "Use for all books" button fit in one full-width column.
pub const SHEET_H: u32 = 1456;
/// Tab titles.
pub const TABS: [&str; 3] = ["Text", "Layout", "Page"];
/// Full refresh choices (`reader.flash_every`; 0 = never).
pub const FLASHES: [(u32, &str); 4] = [(0, "Never"), (4, "4 pages"), (8, "8 pages"), (16, "16 pages")];
/// Time from the last layout change to the relayout.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// A preset: font size, line spacing, side margins, justify, hyphenate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    /// Caption.
    pub name: &'static str,
    /// Font size in pixels.
    pub font_px: f32,
    /// Line spacing.
    pub spacing: f32,
    /// Side margins in pixels.
    pub margin: f32,
    /// Justified.
    pub justify: bool,
    /// Hyphenated.
    pub hyph: bool,
}

/// The presets row.
pub const PRESETS: [Preset; 4] = [
    Preset { name: "Compact", font_px: 32.0, spacing: 1.2, margin: 40.0, justify: true, hyph: true },
    Preset { name: "Standard", font_px: 40.0, spacing: 1.4, margin: 70.0, justify: true, hyph: false },
    Preset { name: "Large", font_px: 52.0, spacing: 1.5, margin: 90.0, justify: true, hyph: true },
    Preset { name: "Largest", font_px: 68.0, spacing: 1.8, margin: 50.0, justify: false, hyph: false },
];

thread_local! {
    /// The tab shown when the sheet is built (kept across tab switches and reopenings).
    static TAB: Cell<usize> = const { Cell::new(0) };
    /// When the debounced relayout is due (`None`: nothing pending).
    static RELAYOUT_AT: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// The tab the next [`build`] shows.
pub fn current_tab() -> usize {
    TAB.with(Cell::get)
}

/// Asks for a relayout [`DEBOUNCE`] from now (a later change moves it again).
pub fn schedule_relayout() {
    RELAYOUT_AT.with(|c| c.set(Some(Instant::now() + DEBOUNCE)));
}

/// Time until the pending relayout (`None`: nothing pending). The reader merges it into
/// its `timeout`, so the event loop wakes up for it.
pub fn relayout_timeout() -> Option<Duration> {
    RELAYOUT_AT.with(Cell::get).map(|t| t.saturating_duration_since(Instant::now()))
}

/// Takes the pending relayout when it is due, or at once with `now`.
fn take_relayout(now: bool) -> bool {
    RELAYOUT_AT.with(|c| match c.get() {
        Some(t) if now || Instant::now() >= t => {
            c.set(None);
            true
        }
        _ => false,
    })
}

/// Values for the Page tab and the font list, which the layout does not hold.
#[derive(Clone, Debug, PartialEq)]
pub struct AaExtras {
    /// Font families of the list (the built-in one first); empty: built-in only.
    pub families: Vec<String>,
    /// Page turn wave (`reader.page_animation`: `on` / `off`).
    pub wave: bool,
    /// The panel can sweep a page turn (the toggle is shown).
    pub wave_supported: bool,
    /// Night mode.
    pub night: bool,
    /// Full refresh every N page turns (0 = never).
    pub flash_every: u32,
    /// 1-based page a followed link came from ("Back to p. N"), if any.
    pub link_return: Option<usize>,
}

impl Default for AaExtras {
    fn default() -> Self {
        AaExtras {
            families: Vec::new(),
            wave: true,
            wave_supported: true,
            night: false,
            flash_every: 8,
            link_return: None,
        }
    }
}

impl AaExtras {
    /// Night mode, refresh interval and page turn wave from `settings`.
    pub fn from_settings(settings: &Settings) -> Self {
        AaExtras {
            night: settings.device.night_mode,
            flash_every: settings.reader.flash_every,
            wave: ink_store::prefs::page_animation_on(&settings.reader.page_animation),
            ..AaExtras::default()
        }
    }
}

/// The values the sheet shows, shared by its controls.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    /// Font size.
    pub font_px: f32,
    /// Line spacing.
    pub spacing: f32,
    /// Side margins.
    pub margin: f32,
    /// Top margin.
    pub margin_top: f32,
    /// Bottom margin.
    pub margin_bottom: f32,
    /// Justified.
    pub justify: bool,
    /// Hyphenated.
    pub hyph: bool,
    /// Extra space after paragraphs in em.
    pub para_space: f32,
    /// First-line indent in em ([`INDENT_BOOK`]: the book's).
    pub indent: f32,
    /// Text weight level ([`WEIGHTS`]).
    pub weight: u8,
    /// The book's own CSS is used.
    pub publisher: bool,
    /// Font family (`None`: built-in).
    pub font: Option<String>,
    /// Page tab values.
    pub extras: AaExtras,
}

/// A stored indent: negative is [`INDENT_BOOK`], else 0..=6 em.
fn indent_value(v: f32) -> f32 {
    if v < 0.0 {
        INDENT_BOOK
    } else {
        v.min(6.0)
    }
}

fn weight_value(v: f32) -> u8 {
    (v.max(0.0) as u8).min(WEIGHT_MAX)
}

/// `Some(name)` for an installed family, `None` for empty or the built-in name.
fn family_value(name: &str) -> Option<String> {
    let n = name.trim();
    (!n.is_empty() && !n.eq_ignore_ascii_case(BUILTIN)).then(|| n.to_string())
}

impl Shown {
    /// From the shown layout and the page values.
    pub fn new(l: &LayoutSettings, extras: &AaExtras) -> Self {
        Shown {
            font_px: l.font_px,
            spacing: l.line_spacing,
            margin: l.margin_left,
            margin_top: l.margin_top,
            margin_bottom: l.margin_bottom,
            justify: l.justify,
            hyph: l.hyphenate,
            para_space: l.paragraph_spacing,
            indent: l.paragraph_indent.unwrap_or(INDENT_BOOK),
            weight: l.embolden,
            publisher: l.publisher_styles,
            font: l.font_family.as_deref().and_then(family_value),
            extras: extras.clone(),
        }
    }

    /// Records the effect of an Aa action.
    pub fn apply(&mut self, action: &str) {
        let Some((name, value)) = action.split_once(':') else { return };
        let num = value.parse::<f32>().ok().filter(|v| v.is_finite());
        let on = value == "on";
        if let Some(i) = SLIDERS.iter().position(|s| s.action == name) {
            if let Some(v) = num {
                self.set(i, v.clamp(SLIDERS[i].min, SLIDERS[i].max));
            }
            return;
        }
        match (name, num) {
            ("aa-justify", _) => self.justify = on,
            ("aa-hyph", _) => self.hyph = on,
            ("aa-publisher", _) => self.publisher = on,
            ("aa-pspace", Some(v)) => self.para_space = v.clamp(0.0, 3.0),
            ("aa-indent", Some(v)) => self.indent = indent_value(v),
            ("aa-weight", Some(v)) => self.weight = weight_value(v),
            ("aa-preset", Some(v)) => {
                if let Some(p) = PRESETS.get(v as usize) {
                    (self.font_px, self.spacing, self.margin, self.justify, self.hyph) = (p.font_px, p.spacing, p.margin, p.justify, p.hyph);
                }
            }
            ("aa-font", _) => self.font = family_value(value),
            ("aa-anim", _) => self.extras.wave = on,
            ("aa-night", _) => self.extras.night = on,
            ("aa-flash", Some(v)) => self.extras.flash_every = v as u32,
            _ => {}
        }
    }

    /// The preset these values match, if any.
    pub fn preset(&self) -> Option<usize> {
        let near = |a: f32, b: f32| (a - b).abs() < 0.01;
        PRESETS.iter().position(|p| near(p.font_px, self.font_px) && near(p.spacing, self.spacing) && near(p.margin, self.margin) && p.justify == self.justify && p.hyph == self.hyph)
    }

    /// Value of slider `i` of [`SLIDERS`].
    pub fn get(&self, i: usize) -> f32 {
        match i {
            0 => self.font_px,
            1 => self.spacing,
            2 => self.margin,
            3 => self.margin_top,
            _ => self.margin_bottom,
        }
    }

    fn set(&mut self, i: usize, v: f32) {
        match i {
            0 => self.font_px = v,
            1 => self.spacing = v,
            2 => self.margin = v,
            3 => self.margin_top = v,
            _ => self.margin_bottom = v,
        }
    }
}

/// One numeric layout value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SliderSpec {
    /// Overlay id key.
    pub key: &'static str,
    /// Caption.
    pub label: &'static str,
    /// Action prefix.
    pub action: &'static str,
    /// Smallest value.
    pub min: f32,
    /// Largest value.
    pub max: f32,
    /// Step of the buttons and the snapping.
    pub step: f32,
    /// Unit after the value (`px` or empty).
    pub unit: &'static str,
}

const fn spec(key: &'static str, label: &'static str, action: &'static str, r: (f32, f32, f32), unit: &'static str) -> SliderSpec {
    SliderSpec { key, label, action, min: r.0, max: r.1, step: r.2, unit }
}

/// The numeric values, in [`Shown::get`] order.
pub const SLIDERS: [SliderSpec; 5] = [
    spec("size", "Font size", "aa-size", FONT_PX, "px"),
    spec("spacing", "Line spacing", "aa-spacing", SPACING, ""),
    spec("margin", "Side margins", "aa-margin", MARGIN_PX, "px"),
    spec("mtop", "Top margin", "aa-mtop", MARGIN_TOP_PX, "px"),
    spec("mbottom", "Bottom margin", "aa-mbottom", MARGIN_BOTTOM_PX, "px"),
];

impl SliderSpec {
    /// `v` clamped and snapped to a step.
    pub fn snap(&self, v: f32) -> f32 {
        let v = v.clamp(self.min, self.max);
        (self.min + ((v - self.min) / self.step).round() * self.step).clamp(self.min, self.max)
    }

    /// The value as the action writes it (no decimals for whole steps).
    pub fn value_text(&self, v: f32) -> String {
        if self.step.fract() == 0.0 {
            format!("{}", v.round() as i64)
        } else {
            format!("{v:.1}")
        }
    }

    /// The value as the row shows it (with its unit).
    pub fn shown_text(&self, v: f32) -> String {
        match self.unit {
            "" => self.value_text(v),
            u => format!("{} {u}", self.value_text(v)),
        }
    }
}

/// The layout for a book: the global reader settings with the book's overrides on top,
/// the top and bottom margins placed against the status bars ([`footer::text_layout`]).
/// The font family is the book's own choice, else [`fonts::reading_default`] (the
/// user's default, else Bookerly when installed, else the built-in font).
pub fn book_layout(settings: &Settings, book: &BookSettings, w: u32, h: u32) -> LayoutSettings {
    footer::text_layout(stored_book_layout(settings, book, w, h), &settings.status_bars)
}

/// [`book_layout`] with the margins as stored (what the sheet shows and saves).
pub fn stored_book_layout(settings: &Settings, book: &BookSettings, w: u32, h: u32) -> LayoutSettings {
    let mut l = stored_layout(settings, w, h);
    if let Some(v) = book.font_px {
        l.font_px = v;
    }
    if let Some(v) = book.line_spacing {
        l.line_spacing = v;
    }
    if let Some(v) = book.margin_x {
        l.margin_left = v;
        l.margin_right = v;
    }
    if let Some(v) = book.margin_top {
        l.margin_top = v;
    }
    if let Some(v) = book.margin_bottom {
        l.margin_bottom = v;
    }
    if let Some(v) = book.justify {
        l.justify = v;
    }
    if let Some(v) = book.hyphenation {
        l.hyphenate = v;
    }
    if let Some(v) = book.publisher_styles {
        l.publisher_styles = v;
    }
    if let Some(v) = book.paragraph_spacing {
        l.paragraph_spacing = v;
    }
    if let Some(v) = book.paragraph_indent {
        l.paragraph_indent = super::indent_em(v);
    }
    if let Some(v) = book.font_weight {
        l.embolden = v.min(WEIGHT_MAX);
    }
    l.font_family = match book.font_family.as_deref() {
        Some(f) => family_value(f),
        None => {
            // First run without a font cache: give the scan a moment so the first book
            // opens in Bookerly instead of being laid out twice.
            fonts::wait_first_scan(Duration::from_secs(3));
            fonts::reading_default(settings)
        }
    };
    l
}

/// Applies one Aa action to `book` (the book's overrides, starting from the shown
/// layout `l`). Returns false for an action that is not a book layout setting (the
/// Page-tab actions: see [`apply_global`]).
pub fn apply(action: &str, l: &LayoutSettings, book: &mut BookSettings) -> bool {
    let Some((name, value)) = action.split_once(':') else { return false };
    let num = value.parse::<f32>().ok().filter(|v| v.is_finite());
    let on = value == "on";
    // The text column keeps at least MIN_TEXT_PX; the top and bottom margins are cut at
    // layout time when the text height would drop below it (footer::text_layout).
    let side = |v: f32| v.clamp(MARGIN_PX.0, MARGIN_PX.1).min(((l.width as f32 - footer::MIN_TEXT_PX) / 2.0).max(0.0));
    let vertical = |v: f32, r: (f32, f32, f32)| v.clamp(r.0, r.1);
    match (name, num) {
        ("aa-size", Some(v)) => book.font_px = Some(v.clamp(FONT_PX.0, FONT_PX.1)),
        ("aa-spacing", Some(v)) => book.line_spacing = Some(v.clamp(SPACING.0, SPACING.1)),
        ("aa-margin", Some(v)) => book.margin_x = Some(side(v)),
        ("aa-mtop", Some(v)) => book.margin_top = Some(vertical(v, MARGIN_TOP_PX)),
        ("aa-mbottom", Some(v)) => book.margin_bottom = Some(vertical(v, MARGIN_BOTTOM_PX)),
        ("aa-justify", _) => book.justify = Some(on),
        ("aa-hyph", _) => book.hyphenation = Some(on),
        ("aa-publisher", _) => book.publisher_styles = Some(on),
        ("aa-pspace", Some(v)) => book.paragraph_spacing = Some(v.clamp(0.0, 3.0)),
        ("aa-indent", Some(v)) => book.paragraph_indent = Some(indent_value(v)),
        ("aa-weight", Some(v)) => book.font_weight = Some(weight_value(v)),
        ("aa-preset", Some(v)) => {
            let Some(p) = PRESETS.get(v as usize) else { return false };
            book.font_px = Some(p.font_px);
            book.line_spacing = Some(p.spacing);
            book.margin_x = Some(side(p.margin));
            book.justify = Some(p.justify);
            book.hyphenation = Some(p.hyph);
        }
        ("aa-font", _) => book.font_family = (!value.trim().is_empty()).then(|| value.trim().to_string()),
        _ => return false,
    }
    true
}

/// Applies a Page-tab action that is an app-wide setting: `aa-night:on|off`
/// (`device.night_mode`), `aa-flash:N` (`reader.flash_every`) and `aa-anim:on|off`
/// (`reader.page_animation`, the page turn wave). Returns false for anything else.
pub fn apply_global(action: &str, settings: &mut Settings) -> bool {
    let Some((name, value)) = action.split_once(':') else { return false };
    match name {
        "aa-night" => settings.device.night_mode = value == "on",
        "aa-anim" if value == "on" || value == "off" => settings.reader.page_animation = value.into(),
        "aa-flash" => match value.parse::<u32>() {
            Ok(n) => settings.reader.flash_every = n,
            Err(_) => return false,
        },
        _ => return false,
    }
    true
}

/// Copies the shown layout (stored margins: [`stored_book_layout`]) into the global
/// reader settings (the defaults for books without their own values); the font family
/// only when the book has its own choice.
pub fn make_global(settings: &mut Settings, l: &LayoutSettings, book: &BookSettings) {
    let r = &mut settings.reader;
    r.font_px = l.font_px;
    r.line_spacing = l.line_spacing;
    r.margin_x = l.margin_left;
    r.margin_top = l.margin_top;
    r.margin_bottom = l.margin_bottom;
    r.justify = l.justify;
    r.hyphenate = l.hyphenate;
    r.publisher_styles = l.publisher_styles;
    r.paragraph_spacing = l.paragraph_spacing;
    r.paragraph_indent = l.paragraph_indent.unwrap_or(INDENT_BOOK);
    r.font_weight = l.embolden;
    if let Some(f) = &book.font_family {
        r.font_family = f.clone();
    }
}

impl ReaderScreen {
    /// Opens the Aa sheet (reader menu "Aa", the swipe-up-from-bottom gesture).
    pub fn open_aa(&mut self, cx: &mut AppCtx) {
        if self.doc.is_none() {
            log::info!("aa: no book open, no sheet");
            return;
        }
        let settings = cx.settings_update.as_ref().unwrap_or(cx.settings);
        let mut extras = AaExtras::from_settings(settings);
        extras.night = cx.screen.night_mode();
        extras.wave_supported = cx.services.info.page_wave;
        extras.families = fonts::picker_names();
        extras.link_return = self.link_return_page().map(|p| p + 1);
        let size = cx.screen.size();
        // The values the book will have (a relayout may still be pending), as stored.
        let layout = stored_book_layout(settings, &self.book_settings, size.w, size.h);
        log::info!(
            "aa: sheet tab {} ({} px, spacing {}, margins {}/{}/{}, font {:?}, {} families, link return {:?})",
            TABS[current_tab().min(TABS.len() - 1)],
            layout.font_px,
            layout.line_spacing,
            layout.margin_left,
            layout.margin_top,
            layout.margin_bottom,
            layout.font_family,
            extras.families.len(),
            extras.link_return
        );
        let o = build_with(&layout, &extras, size);
        self.open_overlay(o, cx);
    }

    /// Handles `aa` and the `aa-*` actions; `None` when `a` is not one of them.
    pub(super) fn aa_action(&mut self, a: &str, cx: &mut AppCtx) -> anyhow::Result<Option<Transition>> {
        match a {
            "aa" => self.open_aa(cx),
            "aa-linkback" => {
                self.close_overlay();
                let ok = self.go_back_from_link();
                log::info!("aa: back to the link's page: {}", if ok { "done" } else { "no link return" });
                self.rerender = true;
            }
            "aa-statusbars" => {
                log::info!("aa: open the status bar settings");
                self.close_overlay();
                crate::screens::settings::open_at("status_bars");
                return Ok(Some(Transition::Push(Route::Settings)));
            }
            "aa-global" => {
                self.aa_flush_now(cx)?;
                let size = cx.screen.size();
                let mut global = cx.settings_update.take().unwrap_or_else(|| cx.settings.clone());
                let l = stored_book_layout(&global, &self.book_settings, size.w, size.h);
                make_global(&mut global, &l, &self.book_settings);
                *cx.settings_update = Some(global);
                log::info!("aa: defaults handed to the app for settings.toml ({} px, spacing {}, font {:?})", l.font_px, l.line_spacing, l.font_family);
                if let Some(o) = self.overlay.as_mut() {
                    if let Some(&id) = o.ids.get("global") {
                        if let Some(b) = o.tree.get_mut::<Button>(id) {
                            b.label = "Saved for all books".into();
                        }
                        o.tree.mark_dirty(id, RefreshClass::Gl16);
                    }
                }
            }
            _ if a.starts_with("aa-night:") || a.starts_with("aa-flash:") || a.starts_with("aa-anim:") => {
                let mut g = cx.settings_update.take().unwrap_or_else(|| cx.settings.clone());
                let changed = apply_global(a, &mut g);
                let night = g.device.night_mode;
                *cx.settings_update = Some(g);
                if changed && a.starts_with("aa-night:") && cx.screen.night_mode() != night {
                    log::info!("aa: night mode {}", if night { "on" } else { "off" });
                    if let Err(e) = cx.screen.set_night_mode(night) {
                        log::warn!("aa: night mode: {e}");
                    }
                    // The whole screen inverts: draw the page and the sheet again with GC16.
                    self.rerender = true;
                    self.dirty = Some(RefreshClass::Gc16);
                }
            }
            _ if a.starts_with("aa-") => {
                let size = cx.screen.size();
                let current = stored_book_layout(cx.settings, &self.book_settings, size.w, size.h);
                if apply(a, &current, &mut self.book_settings) {
                    // Laid out once, DEBOUNCE after the last change (aa_flush).
                    schedule_relayout();
                    log::info!("aa: {a} for this book, relayout in {} ms", DEBOUNCE.as_millis());
                } else {
                    log::info!("aa: '{a}' changes nothing");
                }
            }
            _ => return Ok(None),
        }
        Ok(Some(Transition::None))
    }

    /// Lays the book out again when a debounced Aa change is due, or at once when the
    /// sheet is no longer open. Called after every event the reader handles.
    pub(super) fn aa_flush(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        let sheet_open = self.overlay.as_ref().is_some_and(|o| o.kind == OverlayKind::Aa);
        if take_relayout(!sheet_open) {
            self.aa_relayout(cx)?;
        }
        Ok(())
    }

    /// Runs a pending relayout now.
    fn aa_flush_now(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        if take_relayout(true) {
            self.aa_relayout(cx)?;
        }
        Ok(())
    }

    fn aa_relayout(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        let t = Instant::now();
        self.relayout(cx, RefreshClass::Gl16)?;
        let pages = self.doc.as_deref().map_or(0, |d| d.page_count().pages);
        if let Some(l) = self.doc.as_deref().map(|d| d.layout().clone()) {
            log::info!(
                "aa: relayout in {:?}: {} px, spacing {}, margins {}/{}/{}, justify {}, hyphenate {}, font {:?}; page {} of {}",
                t.elapsed(),
                l.font_px,
                l.line_spacing,
                l.margin_left,
                l.margin_top,
                l.margin_bottom,
                l.justify,
                l.hyphenate,
                l.font_family,
                self.page + 1,
                pages
            );
        }
        self.save_book_settings();
        Ok(())
    }

    fn save_book_settings(&self) {
        self.update_sidecar("book settings", |state| state.settings = self.book_settings.clone());
    }
}

/// What a [`Control`] edits (how it reads [`Shown`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Hyph,
    Publisher,
    ParaSpace,
    Indent,
    Weight,
    Justify,
    Preset,
    Night,
    Wave,
    Flash,
}

/// The ink-ui control inside a [`Control`].
enum Inner {
    Seg(Segmented),
    Toggle(Toggle),
}

impl Inner {
    fn widget(&mut self) -> &mut dyn Widget {
        match self {
            Inner::Seg(w) => w,
            Inner::Toggle(w) => w,
        }
    }

    /// Selects option `i` (segmented, single chips).
    fn select(&mut self, i: Option<usize>) {
        match self {
            Inner::Seg(s) => s.selected = i.unwrap_or(usize::MAX),
            Inner::Toggle(_) => {}
        }
    }
}

/// An ink-ui control bound to one [`Field`] of the shared [`Shown`]: it shows the
/// shared value, maps an option index to its action (`values[i]`) and records the
/// action in the shared values.
struct Control {
    field: Field,
    inner: Inner,
    /// Action per option index (segmented, chips); empty: the control's own action.
    values: Vec<String>,
    shown: Rc<RefCell<Shown>>,
}

impl Control {
    fn new(field: Field, inner: Inner, values: Vec<String>, shown: &Rc<RefCell<Shown>>) -> Self {
        let mut c = Control { field, inner, values, shown: shown.clone() };
        c.sync();
        c
    }

    fn sync(&mut self) {
        let s = self.shown.borrow();
        let e = &s.extras;
        if let Inner::Toggle(t) = &mut self.inner {
            t.on = match self.field {
                Field::Night => e.night,
                Field::Wave => e.wave,
                Field::Hyph => s.hyph,
                Field::Publisher => s.publisher,
                _ => s.justify,
            };
            return;
        }
        let sel = match self.field {
            Field::Preset => s.preset(),
            Field::Flash => FLASHES.iter().position(|(n, _)| *n == e.flash_every),
            Field::ParaSpace => PARA_SPACINGS.iter().position(|(v, _)| (*v - s.para_space).abs() < 0.01),
            Field::Indent => INDENTS.iter().position(|(v, _)| (*v - s.indent).abs() < 0.01),
            Field::Weight => WEIGHTS.iter().position(|(v, _)| *v == s.weight),
            _ => None,
        };
        self.inner.select(sel);
    }
}

impl Widget for Control {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        self.sync();
        self.inner.widget().paint(cx, b);
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match self.inner.widget().event(ev, b) {
            EventResult::Action(a) => {
                let a = match a.split(':').nth(1).and_then(|i| i.parse::<usize>().ok()).and_then(|i| self.values.get(i)) {
                    Some(v) if !self.values.is_empty() => v.clone(),
                    _ => a,
                };
                self.shown.borrow_mut().apply(&a);
                EventResult::Action(a)
            }
            r => r,
        }
    }

    fn damage(&mut self, b: Rect) -> Option<Vec<Rect>> {
        self.inner.widget().damage(b)
    }
}

/// The tab row: a tap on another tab remembers it and reports `aa` (build again).
struct TabRow(Tabs);

impl Widget for TabRow {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        self.0.paint(cx, b);
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match self.0.event(ev, b) {
            EventResult::Action(_) => {
                TAB.with(|t| t.set(self.0.selected));
                log::info!("aa: tab {}", TABS[self.0.selected.min(TABS.len() - 1)]);
                EventResult::Action("aa".into())
            }
            EventResult::Ignored if matches!(ev, UiEvent::Gesture(_)) => EventResult::Handled,
            r => r,
        }
    }
}

/// The preview's paragraphs: the first is short so that the paragraph space and the
/// indent show in a block of three lines.
pub const PREVIEW_PARAS: [&str; 2] = [
    "Reading should feel effortless.",
    "Choose the size, spacing and margins that suit your eyes. The book is laid out again when you let go of the slider.",
];

/// Height of the preview block.
pub const PREVIEW_H: u32 = 212;
/// Height of a slider row (its buttons are this square).
pub const ROW_H: u32 = 104;
const GAP: u32 = 16;
const CAPTION_H: u32 = 44;
const CONTROL_H: u32 = 104;

/// Sliders over a live preview: the composite keeps the value, the row and the preview
/// in one widget, so a drag repaints only them (DU) and reports one action on release.
struct SliderPanel {
    /// Indices into [`SLIDERS`].
    which: Vec<usize>,
    shown: Rc<RefCell<Shown>>,
    /// Screen width (the preview scales the side margins to its own width).
    screen_w: u32,
    /// The row being dragged and its value before the drag.
    drag: Option<(usize, f32)>,
    /// Rows changed since the last paint (positions in `which`).
    changed: Vec<usize>,
    /// A drag just ended: the next update is a REAGL settle of what DU showed (no flash).
    settle: bool,
    /// Shaped preview words for (family, size, face).
    cache: Option<PreviewCache>,
    /// Time of the last paint (logged on release).
    last_paint: Duration,
}

/// The preview's words shaped once per family and size: a spacing or margin drag
/// shapes nothing, a size drag shapes again only when the value steps.
struct PreviewCache {
    family: Option<String>,
    px: f32,
    face: Option<ink_text::FaceId>,
    /// Words of each paragraph.
    words: Vec<Vec<ink_text::TextRun>>,
    space: f32,
    ascent: f32,
    descent: f32,
}

impl SliderPanel {
    fn new(which: Vec<usize>, shown: &Rc<RefCell<Shown>>, screen_w: u32) -> Self {
        SliderPanel { which, shown: shown.clone(), screen_w, drag: None, changed: Vec::new(), settle: false, cache: None, last_paint: Duration::ZERO }
    }

    /// Height for `n` rows under a preview of `preview_h`.
    fn height(n: usize, preview_h: u32) -> u32 {
        preview_h + GAP + n as u32 * (ROW_H + GAP) - GAP
    }

    /// (preview, rows) inside `b`; side by side when `b` is wide (landscape).
    fn areas(b: Rect, n: usize) -> (Rect, Vec<Rect>) {
        if b.w >= 1400 {
            let pw = b.w * 2 / 5;
            let preview = Rect::new(b.x, b.y, pw, b.h.min(n as u32 * (ROW_H + GAP)).max(PREVIEW_H));
            let x = b.x + (pw + 32) as i32;
            let w = b.w.saturating_sub(pw + 32);
            return (preview, (0..n).map(|i| Rect::new(x, b.y + (i as u32 * (ROW_H + GAP)) as i32, w, ROW_H)).collect());
        }
        // The preview takes what the rows leave.
        let ph = b.h.saturating_sub(n as u32 * (ROW_H + GAP)).max(PREVIEW_H.min(b.h));
        let preview = Rect::new(b.x, b.y, b.w, ph);
        let y0 = b.y + (ph + GAP) as i32;
        (preview, (0..n).map(|i| Rect::new(b.x, y0 + (i as u32 * (ROW_H + GAP)) as i32, b.w, ROW_H)).collect())
    }

    /// (caption, minus, track, plus) of one row.
    fn parts(row: Rect) -> (Rect, Rect, Rect, Rect) {
        let label_w = (row.w * 9 / 25).min(420);
        let label = Rect::new(row.x + 8, row.y, label_w.saturating_sub(8), row.h);
        let minus = Rect::new(row.x + label_w as i32 + 8, row.y, ROW_H, ROW_H);
        let plus = Rect::new(row.right() - ROW_H as i32, row.y, ROW_H, ROW_H);
        let track = Rect::new(minus.right() + 24, row.y, (plus.x - minus.right() - 48).max(1) as u32, row.h);
        (label, minus, track, plus)
    }

    fn value_at(spec: &SliderSpec, track: Rect, p: Point) -> f32 {
        let f = (p.x - track.x) as f32 / track.w.max(1) as f32;
        spec.snap(spec.min + f.clamp(0.0, 1.0) * (spec.max - spec.min))
    }

    fn report(&self, k: usize) -> EventResult {
        let spec = &SLIDERS[self.which[k]];
        let v = self.shown.borrow().get(self.which[k]);
        EventResult::Action(format!("{}:{}", spec.action, spec.value_text(v)))
    }

    fn set(&mut self, k: usize, v: f32) -> bool {
        let i = self.which[k];
        let mut s = self.shown.borrow_mut();
        if (s.get(i) - v).abs() < 1e-4 {
            return false;
        }
        s.set(i, v);
        drop(s);
        self.mark(k);
        true
    }

    /// Row `k` is drawn again at the next paint.
    fn mark(&mut self, k: usize) {
        if !self.changed.contains(&k) {
            self.changed.push(k);
        }
    }

    fn paint_row(&self, cx: &mut PaintCx, k: usize, row: Rect) {
        let spec = &SLIDERS[self.which[k]];
        let v = self.shown.borrow().get(self.which[k]);
        let (fg, bg) = (cx.theme.fg, cx.theme.bg);
        let (label, minus, track, plus) = Self::parts(row);
        // Caption above, value below (bold), in the label column.
        let top = Rect::new(label.x, label.y + 4, label.w, label.h / 2 - 4);
        let bottom = Rect::new(label.x, label.y + label.h as i32 / 2, label.w, label.h / 2 - 4);
        cx.text_left(top, spec.label, cx.theme.small_px, Style::Regular, fg);
        cx.text_left(bottom, &spec.shown_text(v), cx.theme.font_px, Style::Bold, fg);
        for (r, icon) in [(minus, Icon::Minus), (plus, Icon::Plus)] {
            stroke_round_rect(cx.canvas, r, r.w as f32 / 2.0, cx.theme.border_px as f32, fg);
            cx.icon(icon, r, r.w / 2, fg);
        }
        let ty = track.y as f32 + track.h as f32 / 2.0;
        let f = if spec.max > spec.min { (v - spec.min) / (spec.max - spec.min) } else { 0.0 };
        let kx = track.x as f32 + f * track.w as f32;
        let th = 12.0;
        let dragging = self.drag.is_some_and(|(d, _)| d == k);
        // While dragging (DU) the track is drawn black only: no gray under DU.
        let track_v = if dragging { bg } else { cx.theme.track() };
        fill_round_rect(cx.canvas, Rect::new(track.x, (ty - th / 2.0) as i32, track.w, th as u32), th / 2.0, track_v);
        if dragging {
            stroke_round_rect(cx.canvas, Rect::new(track.x, (ty - th / 2.0) as i32, track.w, th as u32), th / 2.0, 2.0, fg);
        }
        fill_round_rect(cx.canvas, Rect::new(track.x, (ty - th / 2.0) as i32, (kx - track.x as f32).max(0.0) as u32, th as u32), th / 2.0, fg);
        Path::new().circle(kx, ty, 26.0).fill(cx.canvas, fg);
        Path::new().circle(kx, ty, 21.0).fill(cx.canvas, if dragging { fg } else { bg });
    }
}

/// The shaped preview words for the shown family and size (from `cache` when it is for
/// them).
fn preview_words<'a>(cx: &mut PaintCx, s: &Shown, cache: &'a mut Option<PreviewCache>) -> &'a PreviewCache {
    let family = s.font.as_deref().unwrap_or(BUILTIN);
    let face = fonts::preview_face(cx.fonts, family);
    let fresh = cache.as_ref().is_some_and(|c| c.family == s.font && c.px == s.font_px && c.face == face);
    if !fresh {
        let px = s.font_px;
        let shape = |cx: &mut PaintCx, w: &str| match face {
            Some(id) => cx.fonts.shape_in(id, px, w),
            None => cx.fonts.shape(Style::Regular, px, w),
        };
        let words: Vec<Vec<ink_text::TextRun>> = PREVIEW_PARAS.iter().map(|p| p.split_whitespace().map(|w| shape(cx, w)).collect()).collect();
        let space = shape(cx, " ").width.max(px * 0.25);
        let (ascent, descent) = match face {
            Some(id) => cx.fonts.face_vmetrics(id, px),
            None => cx.fonts.vmetrics(Style::Regular, px),
        };
        *cache = Some(PreviewCache { family: s.font.clone(), px, face, words, space, ascent, descent });
    }
    cache.as_ref().expect("filled above")
}

/// Draws the live preview: two paragraphs at the shown size, spacing, family, justify,
/// paragraph space, first-line indent and text weight, the side margins and the top margin scaled to
/// the block's width as the reader places them; black on white with a black frame
/// (reads under DU). Only whole lines are drawn.
fn paint_preview(cx: &mut PaintCx, r: Rect, s: &Shown, screen_w: u32, cache: &mut Option<PreviewCache>) {
    let (fg, bg) = (cx.theme.fg, cx.theme.bg);
    fill_round_rect(cx.canvas, r, cx.theme.radius_lg, bg);
    stroke_round_rect(cx.canvas, r, cx.theme.radius_lg, cx.theme.border_px as f32, fg);
    let scale = r.w as f32 / screen_w.max(1) as f32;
    let pad = cx.theme.border_px + 10;
    let side = ((screen_w as f32 - footer::MIN_TEXT_PX) / 2.0).max(0.0);
    let inset = ((s.margin.min(side) * scale) as i32).max(pad + 6);
    let top = ((s.margin_top * scale) as i32).max(pad);
    let inner = Rect::new(r.x + inset, r.y + top, r.w.saturating_sub(2 * inset as u32), r.h.saturating_sub((top + pad / 2) as u32));
    if inner.w < 40 || inner.h < 20 {
        return;
    }
    let clip = cx.clip().map_or(inner, |c| c.intersect(&inner));
    let c = preview_words(cx, s, cache);
    let px = s.font_px;
    let line_h = (px * s.spacing).max(px);
    // As dark as the page: the reader widens the strokes by the same pixels.
    let extra = ink_text::raster::weight_px(s.weight, px);
    // "Book's own" indent shows as 1 em.
    let indent = if s.indent < 0.0 { 1.0 } else { s.indent } * px;
    let mut y = inner.y as f32;
    'paras: for (n, words) in c.words.iter().enumerate() {
        if n > 0 {
            y += px * s.para_space;
        }
        let mut i = 0;
        let mut first = true;
        while i < words.len() {
            if y + line_h > inner.bottom() as f32 + 1.0 {
                break 'paras;
            }
            let off = if first { indent } else { 0.0 };
            let avail = (inner.w as f32 - off).max(px);
            // Greedy line.
            let mut j = i;
            let mut w = 0.0;
            while j < words.len() && (j == i || w + c.space + words[j].width <= avail) {
                w += if j == i { words[j].width } else { c.space + words[j].width };
                j += 1;
            }
            let gaps = (j - i).saturating_sub(1) as f32;
            let gap = if s.justify && j < words.len() && gaps > 0.0 { c.space + (avail - w) / gaps } else { c.space };
            let baseline = y + (line_h - (c.ascent - c.descent)) / 2.0 + c.ascent;
            let mut x = inner.x as f32 + off;
            for run in &words[i..j] {
                cx.fonts.draw_run_clipped_extra(cx.canvas, run, x, baseline, fg, clip, extra);
                x += run.width + gap;
            }
            y += line_h;
            i = j;
            first = false;
        }
    }
}

impl Widget for SliderPanel {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let t = Instant::now();
        let (preview, rows) = Self::areas(b, self.which.len());
        {
            let shown = self.shown.clone();
            let s = shown.borrow();
            paint_preview(cx, preview, &s, self.screen_w, &mut self.cache);
        }
        for (k, row) in rows.into_iter().enumerate() {
            self.paint_row(cx, k, row);
        }
        self.last_paint = t.elapsed();
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let (_, rows) = Self::areas(b, self.which.len());
        match ev {
            UiEvent::Touch(t) => match t.kind {
                TouchKind::Down => {
                    for (k, row) in rows.iter().enumerate() {
                        let (_, _, track, _) = Self::parts(*row);
                        let hit = Rect::new(track.x - 30, track.y, track.w + 60, track.h);
                        if hit.contains(t.pos) {
                            let spec = SLIDERS[self.which[k]];
                            self.drag = Some((k, self.shown.borrow().get(self.which[k])));
                            self.set(k, Self::value_at(&spec, track, t.pos));
                            self.mark(k);
                            return EventResult::Handled;
                        }
                    }
                    EventResult::Ignored
                }
                TouchKind::Move | TouchKind::Hold => match self.drag {
                    Some((k, _)) => {
                        let (_, _, track, _) = Self::parts(rows[k]);
                        if self.set(k, Self::value_at(&SLIDERS[self.which[k]], track, t.pos)) {
                            EventResult::Handled
                        } else {
                            EventResult::Ignored
                        }
                    }
                    None => EventResult::Ignored,
                },
                TouchKind::Up => match self.drag.take() {
                    Some((k, _)) => {
                        let (_, _, track, _) = Self::parts(rows[k]);
                        self.set(k, Self::value_at(&SLIDERS[self.which[k]], track, t.pos));
                        self.mark(k);
                        self.settle = true;
                        log::info!("aa: {} drag ended, last panel paint {:?}", SLIDERS[self.which[k]].label, self.last_paint);
                        self.report(k)
                    }
                    None => EventResult::Ignored,
                },
                // A slider drag follows the finger from touch-down; Press changes nothing.
                TouchKind::Press => EventResult::Ignored,
                // The drag ended without a lift (rotation): back to the value before it.
                TouchKind::Cancel => match self.drag.take() {
                    Some((k, before)) => {
                        self.set(k, before);
                        EventResult::Handled
                    }
                    None => EventResult::Ignored,
                },
            },
            UiEvent::Gesture(Gesture::Tap { pos }) => {
                for (k, row) in rows.iter().enumerate() {
                    let (_, minus, _, plus) = Self::parts(*row);
                    if minus.contains(*pos) || plus.contains(*pos) {
                        let spec = SLIDERS[self.which[k]];
                        let d = if minus.contains(*pos) { -spec.step } else { spec.step };
                        let v = spec.snap(self.shown.borrow().get(self.which[k]) + d);
                        self.set(k, v);
                        return self.report(k);
                    }
                }
                EventResult::Handled
            }
            UiEvent::Gesture(_) => EventResult::Handled,
            _ => EventResult::Ignored,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        if self.drag.is_some() {
            RefreshClass::Du
        } else if self.settle {
            RefreshClass::Reagl
        } else {
            RefreshClass::Gl16
        }
    }

    fn damage(&mut self, b: Rect) -> Option<Vec<Rect>> {
        self.settle = false;
        if self.changed.is_empty() {
            return None;
        }
        let (preview, rows) = Self::areas(b, self.which.len());
        let mut out = vec![preview];
        out.extend(std::mem::take(&mut self.changed).into_iter().filter_map(|k| rows.get(k).copied()));
        Some(out)
    }
}

/// The font family list: a paged grid of pills, each family name drawn in its own font;
/// the shown family is black with white text. With more families than cells the last
/// cell turns the page.
struct FamilyGrid {
    names: Vec<String>,
    shown: Rc<RefCell<Shown>>,
    page: usize,
}

/// A cell of the [`FamilyGrid`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cell2 {
    Name(usize),
    More,
}

impl FamilyGrid {
    fn cols(b: Rect) -> u32 {
        if b.w >= 1100 {
            3
        } else {
            2
        }
    }

    /// (cells in `b`, names per page, pages): with more names than cells the last cell
    /// is the "More" cell.
    fn paging(&self, b: Rect) -> (usize, usize, usize) {
        let per = (Self::cols(b) * ((b.h + GAP) / (CONTROL_H + GAP)).max(1)) as usize;
        let per_page = if self.names.len() > per { per - 1 } else { per };
        (per, per_page, self.names.len().div_ceil(per_page.max(1)).max(1))
    }

    /// The cells of the current page.
    fn cells(&self, b: Rect) -> Vec<(Rect, Cell2)> {
        let cols = Self::cols(b);
        let (per, per_page, pages) = self.paging(b);
        let paged = per_page < per;
        let page = self.page % pages;
        let w = (b.w - (cols - 1) * GAP) / cols;
        let at = |n: usize| {
            let (c, r) = (n as u32 % cols, n as u32 / cols);
            Rect::new(b.x + (c * (w + GAP)) as i32, b.y + (r * (CONTROL_H + GAP)) as i32, w, CONTROL_H)
        };
        let mut out: Vec<(Rect, Cell2)> =
            (page * per_page..((page + 1) * per_page).min(self.names.len())).enumerate().map(|(n, i)| (at(n), Cell2::Name(i))).collect();
        if paged {
            out.push((at(per - 1), Cell2::More));
        }
        out
    }

    fn pages(&self, b: Rect) -> usize {
        self.paging(b).2
    }

    fn selected(&self) -> String {
        self.shown.borrow().font.clone().unwrap_or_else(|| BUILTIN.to_string())
    }
}

impl Widget for FamilyGrid {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let (fg, bg) = (cx.theme.fg, cx.theme.bg);
        let sel = self.selected();
        let pages = self.pages(b);
        for (r, cell) in self.cells(b) {
            let rad = cx.theme.radius;
            match cell {
                Cell2::Name(i) => {
                    let name = &self.names[i];
                    let on = name.eq_ignore_ascii_case(&sel);
                    if on {
                        fill_round_rect(cx.canvas, r, rad, fg);
                    } else {
                        stroke_round_rect(cx.canvas, r, rad, cx.theme.border_px as f32, fg);
                    }
                    let px = cx.theme.label.px;
                    let inner = Rect::new(r.x + 16, r.y, r.w.saturating_sub(32), r.h);
                    let w = fonts::family_text_width(cx.fonts, name, px, name).min(inner.w as f32);
                    let x = inner.x as f32 + (inner.w as f32 - w) / 2.0;
                    fonts::draw_family_text(cx, name, px, x, inner, name, if on { bg } else { fg }, inner);
                }
                Cell2::More => {
                    stroke_round_rect(cx.canvas, r, rad, cx.theme.border_px as f32, fg);
                    let text = format!("More ({}/{})", self.page % pages + 1, pages);
                    cx.text_centered(r, &text, cx.theme.small_px, Style::Bold, fg);
                }
            }
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let UiEvent::Gesture(g) = ev else { return EventResult::Ignored };
        let Gesture::Tap { pos } = g else { return EventResult::Handled };
        for (r, cell) in self.cells(b) {
            if !r.contains(*pos) {
                continue;
            }
            return match cell {
                Cell2::Name(i) => {
                    let a = format!("aa-font:{}", self.names[i]);
                    self.shown.borrow_mut().apply(&a);
                    EventResult::Action(a)
                }
                Cell2::More => {
                    self.page = (self.page + 1) % self.pages(b);
                    log::info!("aa: font list page {}", self.page + 1);
                    EventResult::Handled
                }
            };
        }
        EventResult::Handled
    }
}

/// One row of the sheet: an optional caption above one or more widgets side by side.
struct Row {
    caption: Option<&'static str>,
    h: u32,
    widgets: Vec<(Option<&'static str>, Box<dyn Widget>)>,
    /// In the narrow columns of the landscape sheet the widgets go one under the other
    /// (side by side they would not fit their captions).
    stack_narrow: bool,
}

impl Row {
    fn one(key: &'static str, caption: Option<&'static str>, h: u32, widget: Box<dyn Widget>) -> Self {
        Row { caption, h, widgets: vec![(Some(key), widget)], stack_narrow: false }
    }

    /// Height with the widgets side by side (or `stacked`: one under the other).
    fn total(&self, stacked: bool) -> u32 {
        let n = if stacked { self.widgets.len() as u32 } else { 1 };
        n * self.h + (n - 1) * GAP + if self.caption.is_some() { CAPTION_H } else { 0 }
    }
}

fn seg(options: &[&str]) -> Inner {
    Inner::Seg(Segmented::new(options, "seg"))
}

/// The rows of tab `tab`.
fn rows(tab: usize, shown: &Rc<RefCell<Shown>>, size: Size) -> Vec<Row> {
    let screen_w = size.w;
    let s = shown.borrow().clone();
    let ctl = |f, inner, values: Vec<String>| -> Box<dyn Widget> { Box::new(Control::new(f, inner, values, shown)) };
    let toggle = |f, label: &str, on: bool, action: &str| ctl(f, Inner::Toggle(Toggle::new(label, on, action)), Vec::new());
    // A segmented choice from a (value, caption) table; option I reports `action:value`.
    fn choice<T: std::fmt::Display>(table: &[(T, &str)], action: &str) -> (Inner, Vec<String>) {
        let captions: Vec<&str> = table.iter().map(|x| x.1).collect();
        (seg(&captions), table.iter().map(|x| format!("{action}:{}", x.0)).collect())
    }
    let pick = |f, (inner, values): (Inner, Vec<String>)| ctl(f, inner, values);
    match tab {
        0 => {
            let preset_names: Vec<&str> = PRESETS.iter().map(|p| p.name).collect();
            let mut families = s.extras.families.clone();
            if families.is_empty() {
                families.push(BUILTIN.to_string());
            }
            vec![
                Row::one("preset", None, CONTROL_H, ctl(Field::Preset, seg(&preset_names), (0..PRESETS.len()).map(|i| format!("aa-preset:{i}")).collect())),
                Row::one("size", None, SliderPanel::height(1, PREVIEW_H), Box::new(SliderPanel::new(vec![0], shown, screen_w))),
                Row::one("font", Some("Font"), 2 * CONTROL_H + GAP, Box::new(FamilyGrid { names: families, shown: shown.clone(), page: 0 })),
                Row {
                    caption: None,
                    h: CONTROL_H,
                    widgets: vec![
                        (Some("justify"), toggle(Field::Justify, "Justify", s.justify, "aa-justify")),
                        (Some("hyph"), toggle(Field::Hyph, "Hyphenate", s.hyph, "aa-hyph")),
                    ],
                    stack_narrow: false,
                },
                Row {
                    caption: None,
                    h: CONTROL_H,
                    stack_narrow: true,
                    widgets: vec![
                        (Some("publisher"), toggle(Field::Publisher, "Book's styles", s.publisher, "aa-publisher")),
                        (Some("weight"), pick(Field::Weight, choice(&WEIGHTS, "aa-weight"))),
                    ],
                },
            ]
        }
        1 => vec![
            Row::one("sliders", None, SliderPanel::height(4, PREVIEW_H), Box::new(SliderPanel::new(vec![1, 2, 3, 4], shown, screen_w))),
            Row::one("pspace", Some("Space between paragraphs"), CONTROL_H, pick(Field::ParaSpace, choice(&PARA_SPACINGS, "aa-pspace"))),
            Row::one("indent", Some("First-line indent"), CONTROL_H, pick(Field::Indent, choice(&INDENTS, "aa-indent"))),
        ],
        _ => [
            s.extras.wave_supported.then(|| Row::one("wave", None, CONTROL_H, toggle(Field::Wave, "Page turn wave", s.extras.wave, "aa-anim"))),
            Some(Row::one("statusbars", None, CONTROL_H, Box::new(Button::new("Status bars…", "aa-statusbars")))),
            Some(Row::one("flash", Some("Full refresh every"), CONTROL_H, pick(Field::Flash, choice(&FLASHES, "aa-flash")))),
            Some(Row::one("night", None, CONTROL_H, toggle(Field::Night, "Night mode", s.extras.night, "aa-night"))),
        ]
        .into_iter()
        .flatten()
        .collect(),
    }
}

/// Builds the sheet for the shown layout on a screen of `size` (Page tab values at
/// their defaults; see [`build_with`]).
pub fn build(l: &LayoutSettings, size: Size) -> Overlay {
    build_with(l, &AaExtras::default(), size)
}

/// Builds the sheet with the Page tab values and font list in `extras`, on the tab last
/// chosen.
pub fn build_with(l: &LayoutSettings, extras: &AaExtras, size: Size) -> Overlay {
    build_tab(l, extras, current_tab(), size)
}

/// The (minus, track, plus) rectangles of slider `key` ([`SLIDERS`]) in a built sheet,
/// for tests and scripts.
pub fn slider_parts(o: &Overlay, key: &str) -> Option<(Rect, Rect, Rect)> {
    let i = SLIDERS.iter().position(|s| s.key == key)?;
    let (panel_key, which): (&str, &[usize]) = if i == 0 { ("size", &[0]) } else { ("sliders", &[1, 2, 3, 4]) };
    let b = o.tree.bounds(*o.ids.get(panel_key)?)?;
    let k = which.iter().position(|w| *w == i)?;
    let (_, rows) = SliderPanel::areas(b, which.len());
    let (_, minus, track, plus) = SliderPanel::parts(rows[k]);
    Some((minus, track, plus))
}

/// The live preview's rectangle in a built sheet (the Text tab's size panel, else the
/// Layout tab's sliders), for tests and scripts.
pub fn preview_rect(o: &Overlay) -> Option<Rect> {
    let (key, n) = if o.ids.contains_key("size") { ("size", 1) } else { ("sliders", 4) };
    let b = o.tree.bounds(*o.ids.get(key)?)?;
    Some(SliderPanel::areas(b, n).0)
}

/// Builds the sheet on tab `tab` (0 Text, 1 Layout, 2 Page).
pub fn build_tab(l: &LayoutSettings, extras: &AaExtras, tab: usize, size: Size) -> Overlay {
    let tab = tab.min(TABS.len() - 1);
    let mut o = Overlay::over_page(OverlayKind::Aa, size, "close");
    let h = SHEET_H.min(size.h.saturating_sub(160));
    let frame = BottomSheet::frame(size, h);
    let sheet = o.tree.add(None, Box::new(BottomSheet { title: "Reading settings".into(), taken: false }), frame);
    let done = Rect::new(frame.right() - 204, frame.y + 26, 180, 96);
    o.button(Some(sheet), Button::new("Done", "close"), done);
    if let Some(p) = extras.link_return {
        // Left of the centred title (the right side holds Done).
        let r = Rect::new(frame.x + 24, done.y, 320, 96);
        let id = o.button(Some(sheet), Button::new(format!("Back to p. {p}"), "aa-linkback"), r);
        o.ids.insert("linkback", id);
    }
    let c = BottomSheet::content(frame);
    let mut tabs = Tabs::new(&TABS, "aa-tab");
    tabs.selected = tab;
    let tabs_r = Rect::new(c.x, c.y - 8, c.w, CONTROL_H);
    for (i, r) in tabs.cells(tabs_r).into_iter().enumerate() {
        o.buttons.push((format!("aa-tab:{i}"), r));
    }
    o.tree.add(Some(sheet), Box::new(TabRow(tabs)), tabs_r);
    let shown = Rc::new(RefCell::new(Shown::new(l, extras)));
    let mut rows = rows(tab, &shown, size);
    // "Use for all books" under the Text and Layout values (Page values are global).
    let global = tab < 2;
    let body = Rect::new(c.x, tabs_r.bottom() + 24, c.w, (c.bottom() - tabs_r.bottom() - 24).max(0) as u32);
    let global_h = if global { CONTROL_H + GAP } else { 0 };
    let area = Rect::new(body.x, body.y, body.w, body.h.saturating_sub(global_h));
    // One column; two when the rows do not fit (landscape).
    let total: u32 = rows.iter().map(|r| r.total(false) + GAP).sum();
    let cols = if total > area.h + GAP && area.w >= 1000 { 2 } else { 1 };
    let col_w = (area.w - (cols - 1) * 48) / cols;
    let per_col = total.div_ceil(cols);
    let (mut col, mut y, mut used) = (0u32, area.y, 0u32);
    for row in rows.drain(..) {
        let stacked = cols == 2 && row.stack_narrow;
        let t = row.total(stacked) + GAP;
        let cap_h = if row.caption.is_some() { CAPTION_H } else { 0 };
        if cols == 2 && col == 0 && used > 0 && used + t / 2 > per_col {
            col = 1;
            y = area.y;
        }
        used += t;
        let x = area.x + (col * (col_w + 48)) as i32;
        if let Some(cap) = row.caption {
            o.tree.add(Some(sheet), Box::new(Label::left(cap, 32.0).styled(Style::Bold)), Rect::new(x, y, col_w, CAPTION_H - 8));
            y += CAPTION_H as i32;
        }
        let n = if stacked { 1 } else { row.widgets.len() as u32 };
        let w = (col_w - (n - 1) * 48) / n;
        let h = row.h;
        for (k, (key, widget)) in row.widgets.into_iter().enumerate() {
            let k = k as u32;
            let r = if stacked { Rect::new(x, y + (k * (h + GAP)) as i32, w, h) } else { Rect::new(x + (k * (w + 48)) as i32, y, w, h) };
            let id = o.tree.add(Some(sheet), widget, r);
            if let Some(key) = key {
                o.ids.insert(key, id);
            }
        }
        y += (t - cap_h) as i32;
    }
    if global {
        let w = 520.min(body.w);
        let r = Rect::new(body.x + (body.w - w) as i32 / 2, body.bottom() - CONTROL_H as i32, w, CONTROL_H);
        let id = o.button(Some(sheet), Button::new("Use for all books", "aa-global"), r);
        o.ids.insert("global", id);
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_and_actions() {
        let s = Settings::default();
        let mut b = BookSettings::default();
        let base = book_layout(&s, &b, 1272, 1696);
        assert_eq!(base, super::super::layout_for(&s, 1272, 1696));
        assert_eq!(stored_book_layout(&s, &b, 1272, 1696), super::super::stored_layout(&s, 1272, 1696));
        for a in ["aa-size:50", "aa-spacing:1.8", "aa-margin:500", "aa-mtop:90", "aa-mbottom:1000", "aa-justify:off", "aa-hyph:on"] {
            assert!(apply(a, &base, &mut b), "{a}");
        }
        assert!(!apply("close", &base, &mut b));
        for a in ["aa-footer:bar", "aa-anim:on", "aa-night:on", "aa-flash:4", "aa-preset:9"] {
            assert!(!apply(a, &base, &mut b), "{a}");
        }
        // Clamped to 0..=450; the side margin also to (1272 - 300) / 2 = 486.
        let l = stored_book_layout(&s, &b, 1272, 1696);
        assert_eq!((l.font_px, l.line_spacing, l.margin_left, l.margin_top, l.margin_bottom), (50.0, 1.8, 450.0, 90.0, 450.0));
        // Laid out: the top and bottom margins start at the inner edges of the bars.
        let laid = book_layout(&s, &b, 1272, 1696);
        let (top, bottom) = (footer::bar_reserve(&s.status_bars, Edge::Top), footer::bar_reserve(&s.status_bars, Edge::Bottom));
        assert!(top > 0.0 && bottom > 0.0);
        assert_eq!((laid.margin_left, laid.margin_top, laid.margin_bottom), (450.0, top + 90.0, bottom + 450.0));
        // A narrow screen keeps a 300 px text column.
        assert!(apply("aa-margin:450", &LayoutSettings::for_screen(800, 1000), &mut b));
        assert_eq!(b.margin_x, Some(250.0));
        b.margin_x = Some(450.0);
        assert!(!l.justify && l.hyphenate);
        assert!(apply("aa-font:Literata", &base, &mut b));
        assert_eq!(book_layout(&s, &b, 1272, 1696).font_family.as_deref(), Some("Literata"));
        assert!(apply(&format!("aa-font:{BUILTIN}"), &base, &mut b));
        assert_eq!(book_layout(&s, &b, 1272, 1696).font_family, None, "the built-in font is the engine default");
        let mut g = Settings::default();
        make_global(&mut g, &l, &b);
        assert_eq!((g.reader.font_px, g.reader.margin_top, g.reader.margin_bottom, g.reader.font_family.as_str()), (50.0, 90.0, 450.0, BUILTIN));
        assert!(apply_global("aa-anim:off", &mut g) && g.reader.page_animation == "off");
        assert!(!apply_global("aa-anim:slide", &mut g) && !apply_global("aa-flash:x", &mut g));
        // Slider values snap to their steps and print without stray decimals.
        assert_eq!(SLIDERS[1].value_text(SLIDERS[1].snap(1.43)), "1.4");
        assert_eq!(SLIDERS[0].shown_text(SLIDERS[0].snap(47.0)), "48 px");
    }

    #[test]
    fn typography_actions_reach_the_layout_and_the_defaults() {
        let s = Settings::default();
        let mut b = BookSettings::default();
        let base = book_layout(&s, &b, 1272, 1696);
        // Defaults: the engine sees today's behaviour.
        assert_eq!((base.paragraph_spacing, base.paragraph_indent, base.embolden, base.hyphenate, base.publisher_styles), (0.0, None, 0, false, true));
        for a in ["aa-pspace:0.5", "aa-indent:1.5", "aa-weight:2", "aa-publisher:off", "aa-hyph:on"] {
            assert!(apply(a, &base, &mut b), "{a}");
        }
        let l = book_layout(&s, &b, 1272, 1696);
        assert_eq!((l.paragraph_spacing, l.paragraph_indent, l.embolden, l.hyphenate, l.publisher_styles), (0.5, Some(1.5), 2, true, false));
        // Out of range values are clamped; a negative indent means the book's own.
        assert!(apply("aa-weight:9", &base, &mut b) && apply("aa-pspace:7", &base, &mut b) && apply("aa-indent:-3", &base, &mut b));
        let l2 = book_layout(&s, &b, 1272, 1696);
        assert_eq!((l2.embolden, l2.paragraph_spacing, l2.paragraph_indent), (WEIGHT_MAX, 3.0, None));
        // The sheet shows the same values and records the same actions.
        let mut shown = Shown::new(&l, &AaExtras::default());
        assert_eq!((shown.para_space, shown.indent, shown.weight, shown.publisher), (0.5, 1.5, 2, false));
        shown.apply("aa-indent:-1");
        shown.apply("aa-weight:1");
        shown.apply("aa-publisher:on");
        assert_eq!((shown.indent, shown.weight, shown.publisher), (INDENT_BOOK, 1, true));
        // "Use for all books" writes them to settings.toml's reader section.
        let mut g = Settings::default();
        make_global(&mut g, &l, &b);
        let r = &g.reader;
        assert_eq!((r.paragraph_spacing, r.paragraph_indent, r.font_weight, r.hyphenate, r.publisher_styles), (0.5, 1.5, 2, true, false));
        // A book without its own values takes the global ones.
        let layout = book_layout(&g, &BookSettings::default(), 1272, 1696);
        assert_eq!((layout.paragraph_spacing, layout.paragraph_indent, layout.embolden), (0.5, Some(1.5), 2));
        // The sheet's chips carry the actions the book layout reads.
        let o = build_tab(&LayoutSettings { paragraph_indent: Some(1.5), ..LayoutSettings::for_screen(1272, 1696) }, &AaExtras::default(), 1, Size::new(1272, 1696));
        assert!(o.ids.contains_key("pspace") && o.ids.contains_key("indent"));
    }

    #[test]
    fn sheet_fits_and_targets_are_large() {
        let extras = AaExtras { families: (0..9).map(|i| format!("Family {i}")).collect(), link_return: Some(12), ..AaExtras::default() };
        for size in [Size::new(1272, 1696), Size::new(1696, 1272)] {
            for tab in 0..3 {
                let o = build_tab(&LayoutSettings::for_screen(size.w, size.h), &extras, tab, size);
                let keys: &[&str] = match tab {
                    0 => &["preset", "size", "font", "justify", "hyph", "publisher", "weight", "global"],
                    1 => &["sliders", "pspace", "indent", "global"],
                    _ => &["statusbars", "flash", "night"],
                };
                let mut seen: Vec<Rect> = Vec::new();
                for key in keys {
                    let r = o.tree.bounds(o.ids[key]).unwrap();
                    assert!(r.bottom() <= size.h as i32 && r.right() <= size.w as i32 && r.x >= 0, "tab {tab} {key} {r:?} in {size:?}");
                    assert!(r.h >= 96, "tab {tab} {key} {r:?}");
                    assert!(seen.iter().all(|s| s.intersect(&r).is_empty()), "tab {tab} {key} {r:?} overlaps");
                    seen.push(r);
                }
                for key in ["size", "spacing", "mbottom"] {
                    if let Some((m, _, p)) = slider_parts(&o, key) {
                        assert!(m.w >= 96 && m.h >= 96 && p.w >= 96 && p.bottom() <= size.h as i32, "{key} {m:?} {p:?}");
                    }
                }
                let back = o.button_rect("aa-linkback").unwrap();
                assert!(back.right() < size.w as i32 / 2 - 220 && back.h >= 96, "{back:?} clear of the title");
            }
        }
    }

    /// A drag on a slider reports the preview and the dragged row as damage, each once,
    /// also on the lift that moved the value again.
    #[test]
    fn a_slider_drag_reports_its_row_once() {
        let l = LayoutSettings::for_screen(1272, 1696);
        let shown = Rc::new(RefCell::new(Shown::new(&l, &AaExtras::default())));
        let mut p = SliderPanel::new(vec![1, 2], &shown, 1272);
        let b = Rect::new(0, 0, 1200, SliderPanel::height(2, PREVIEW_H));
        let (preview, rows) = SliderPanel::areas(b, 2);
        let (_, _, track, _) = SliderPanel::parts(rows[1]);
        let y = track.y + track.h as i32 / 2;
        let touch = |kind, x: i32| UiEvent::Touch(ink_ui::Touch { slot: 0, kind, pos: Point::new(x, y), start: Point::new(track.x + 10, y), held: Duration::ZERO, moved: true });
        p.event(&touch(TouchKind::Down, track.x + 10), b);
        assert_eq!(p.damage(b), Some(vec![preview, rows[1]]));
        p.event(&touch(TouchKind::Move, track.x + track.w as i32 / 3), b);
        p.event(&touch(TouchKind::Up, track.x + track.w as i32 / 2), b);
        assert_eq!(p.damage(b), Some(vec![preview, rows[1]]));
        assert_eq!(p.damage(b), None);
    }
}
