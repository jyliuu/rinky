//! Widget gallery (`rinky demo`): every `ink-ui` widget, the on-screen keyboard,
//! gesture feedback, night mode and rotation, for screenshots in the simulator, headless
//! mode and on the Kindle.
//!
//! - Top bar: rotate (logical top-left corner, so scripts know where it is in every
//!   orientation), night mode, title, exit (top-right).
//! - Tabs: Controls (label, buttons, toggles, sliders, progress bar, icons), Lists (menu,
//!   paged list), Text (scroll view with shaping, fallback fonts, synthetic styles,
//!   hyphenated paragraphs, the fonts found), Input (text fields and the keyboard),
//!   Gestures (a pad that marks every recognised gesture), Design (the design system of
//!   `docs/DESIGN.md`: app bar, button kinds, segmented control, chips, thin and thick
//!   progress with chapter marks, cards with covers and a progress ring, bottom
//!   navigation).
//! - Buttons open a dialog, a toast, a popup and a bottom sheet ("Aa" panel), or push
//!   the settings screen.
//! - A status line shows the last action or gesture. Every action is logged as
//!   `demo action: NAME`; after each layout the panel position of every button is logged
//!   as `demo button NAME panel X Y`, so emulator scripts can tap them in any rotation.
//! - Power key, the exit button or the end of a script quits.

mod widgets;

#[cfg(test)]
mod tests;

use crate::router::{AppCtx, AppScreen, Route, ScreenRegistry, Transition};
use ink_device::{Canvas, Key, Point, Rect, RefreshClass, Rotation, Size};
use ink_text::registry::{scan_fonts, summary, KINDLE_FONT_DIRS};
use ink_text::Style;
use ink_ui::controls::Align;
use ink_ui::input::InputResult;
use ink_ui::keyboard::{language_name, KeyAction};
use ink_ui::widget::split_cols;
use ink_ui::bars::{APP_BAR_H, NAV_BAR_H};
use ink_ui::{
    AppBar, BarAction, Card, CardKind, Chips, Cover, NavBar, NavItem, RingProgress, Segmented,
};
use ink_ui::{
    BottomSheet, Button, Damage, Dialog, EventResult, Gesture, Icon, IconView, Keyboard, Label, List, ListItem, Menu,
    Popup, ProgressBar, Scrim, ScrollView, Slider, Tabs, TextBlock, TextInput, Theme, Toast, Toggle, UiEvent, WidgetId,
    WidgetTree,
};
use std::path::PathBuf;
use std::time::Duration;
pub use widgets::{describe, GesturePad, ShapingSample};

/// Registers the gallery for [`Route::Demo`].
pub fn register(reg: &mut ScreenRegistry) {
    reg.register("demo", |_| Ok(Box::new(DemoScreen::default())));
}

/// Tab titles.
pub const TABS: [&str; 6] = ["Controls", "Lists", "Text", "Input", "Gestures", "Design"];
/// Hyphenated sample paragraphs of the Text tab (language, text).
const PARAGRAPHS: [(&str, &str); 4] = [
    ("en-US", "English: Typographical hyphenation divides unmanageable words at syllable boundaries, which keeps justified lines on narrow e-ink screens evenly spaced."),
    ("da", "Dansk: Orddeling gør det muligt at fordele mellemrummene jævnt, når teksten er lige margen på en smal skærm med elektronisk papir."),
    ("de", "Deutsch: Die Silbentrennung verbessert den Blocksatz auf schmalen Bildschirmen erheblich, besonders bei zusammengesetzten Substantiven."),
    ("fr", "Français: La césure automatique améliore considérablement la justification du texte sur les écrans étroits des liseuses électroniques."),
];
const TOP_H: u32 = 110;
const TABS_H: u32 = 100;
const STATUS_H: u32 = 80;
const MARGIN: i32 = 24;

/// Font directories for fallback faces: `RINKY_FONT_DIRS` (colon-separated), else
/// the Kindle's (`/mnt/us/fonts`, `/usr/java/lib/fonts`) plus the repository's test font
/// directory when it exists (host builds).
pub fn font_dirs() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("RINKY_FONT_DIRS") {
        return v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from).collect();
    }
    #[allow(unused_mut)]
    let mut dirs: Vec<PathBuf> = KINDLE_FONT_DIRS.iter().map(PathBuf::from).collect();
    // Host builds only: the Kindle binary carries no path of the build machine.
    #[cfg(not(target_arch = "arm"))]
    {
        let test = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../ink-text/tests/data"));
        if test.is_dir() {
            dirs.push(test);
        }
    }
    dirs
}

#[derive(Default)]
struct Ids {
    status: Option<WidgetId>,
    sample: Option<WidgetId>,
    progress: Option<WidgetId>,
    night: Option<WidgetId>,
    input: Option<WidgetId>,
    password: Option<WidgetId>,
    typed: Option<WidgetId>,
    keyboard: Option<WidgetId>,
    pad: Option<WidgetId>,
}

/// The gallery.
pub struct DemoScreen {
    tree: WidgetTree,
    theme: Theme,
    tab: usize,
    ids: Ids,
    overlays: Vec<WidgetId>,
    /// Logical rectangle of every button-like widget by action (for tests and the log).
    buttons: Vec<(String, Rect)>,
    last: String,
    /// Every action handled, in order (for tests).
    pub actions: Vec<String>,
    /// Every gesture seen, in order (for tests).
    pub gestures: Vec<Gesture>,
    texts: [String; 2],
    focus: usize,
    keyboard_visible: bool,
    keyboard_language: String,
    font_lines: Vec<String>,
    sample_px: f32,
    progress: f32,
    preview: bool,
    /// Measured heights of the Text tab's paragraphs at the current width.
    text_heights: Vec<u32>,
    size: Size,
}

impl Default for DemoScreen {
    fn default() -> Self {
        DemoScreen {
            tree: WidgetTree::new(),
            theme: Theme::day(),
            tab: 0,
            ids: Ids::default(),
            overlays: Vec::new(),
            buttons: Vec::new(),
            last: "Tap something".into(),
            actions: Vec::new(),
            gestures: Vec::new(),
            texts: [String::new(), String::new()],
            focus: 0,
            keyboard_visible: true,
            keyboard_language: "en".into(),
            font_lines: Vec::new(),
            sample_px: 40.0,
            progress: 0.4,
            preview: true,
            text_heights: Vec::new(),
            size: Size::new(1272, 1696),
        }
    }
}

/// Stacks rectangles down a column.
struct Col {
    r: Rect,
    y: i32,
    gap: i32,
}

impl Col {
    fn new(r: Rect) -> Self {
        Col { r, y: r.y, gap: 18 }
    }

    fn take(&mut self, h: u32) -> Rect {
        let out = Rect::new(self.r.x, self.y, self.r.w, h);
        self.y += h as i32 + self.gap;
        out
    }

    fn rest(&self) -> Rect {
        Rect::new(self.r.x, self.y, self.r.w, (self.r.bottom() - self.y).max(0) as u32)
    }
}

/// Rows of `n` cells at least `min_w` wide, `h` tall.
fn flow(col: &mut Col, n: usize, min_w: u32, h: u32) -> Vec<Rect> {
    let per_row = ((col.r.w + 16) / (min_w + 16)).clamp(1, n.max(1) as u32);
    let mut out = Vec::new();
    for chunk in (0..n).collect::<Vec<_>>().chunks(per_row as usize) {
        let row = col.take(h);
        out.extend(split_cols(row, per_row, 16).into_iter().take(chunk.len()));
    }
    out
}

impl DemoScreen {
    /// The logical rectangle of the button with this action, if shown.
    pub fn button(&self, action: &str) -> Option<Rect> {
        self.buttons.iter().find(|(a, _)| a == action).map(|(_, r)| *r)
    }

    /// The last action or gesture shown in the status line.
    pub fn status(&self) -> &str {
        &self.last
    }

    /// The selected tab.
    pub fn tab(&self) -> usize {
        self.tab
    }

    /// Text of the two input fields.
    pub fn texts(&self) -> &[String; 2] {
        &self.texts
    }

    /// Logical rectangle of a keyboard key by label, when the keyboard is shown.
    pub fn key(&mut self, label: &str) -> Option<Rect> {
        let id = self.ids.keyboard?;
        let b = self.tree.bounds(id)?;
        let kb = self.tree.get_mut::<Keyboard>(id)?;
        let area = kb.keys_area(b);
        kb.layout.key_rects(area).into_iter().find(|(_, k)| k.label == label).map(|(r, _)| r)
    }

    fn landscape(&self) -> bool {
        self.size.w > self.size.h
    }

    fn keyboard_rect(&self) -> Rect {
        Keyboard::bottom_rect(self.size)
    }

    fn status_rect(&self) -> Rect {
        if self.tab == 3 && self.keyboard_visible {
            let k = self.keyboard_rect();
            Rect::new(0, k.y - STATUS_H as i32, self.size.w, STATUS_H)
        } else {
            Rect::new(0, (self.size.h - STATUS_H) as i32, self.size.w, STATUS_H)
        }
    }

    fn content_rect(&self) -> Rect {
        let top = (TOP_H + TABS_H + 20) as i32;
        let bottom = self.status_rect().y - 10;
        Rect::new(MARGIN, top, self.size.w - 2 * MARGIN as u32, (bottom - top).max(0) as u32)
    }

    fn add_button(&mut self, b: Button, r: Rect) -> WidgetId {
        self.buttons.push((b.action.clone(), r));
        self.tree.add(None, Box::new(b), r)
    }

    /// Builds the whole screen for the current size, tab and theme.
    fn rebuild(&mut self) {
        self.tree = WidgetTree::new();
        self.ids = Ids::default();
        self.overlays.clear();
        self.buttons.clear();
        let w = self.size.w;
        // Top bar.
        self.add_button(Button::new("", "rotate").with_icon(Icon::Rotate), Rect::new(6, 6, 100, 98));
        self.add_button(Button::new("", "night").with_icon(Icon::Night), Rect::new(116, 6, 100, 98));
        self.tree.add(None, Box::new(Label::left("rinky gallery", 46.0).styled(Style::Bold)), Rect::new(230, 0, w.saturating_sub(350), TOP_H));
        self.add_button(Button::new("", "exit").with_icon(Icon::Close), Rect::new(w as i32 - 106, 6, 100, 98));
        let mut tabs = Tabs::new(&TABS, "tab");
        tabs.selected = self.tab;
        let tabs_r = Rect::new(0, TOP_H as i32, w, TABS_H);
        for (i, r) in tabs.cells(tabs_r).into_iter().enumerate() {
            self.buttons.push((format!("tab:{i}"), r));
        }
        self.tree.add(None, Box::new(tabs), tabs_r);
        match self.tab {
            0 => self.build_controls(),
            1 => self.build_lists(),
            2 => self.build_text(),
            3 => self.build_input(),
            4 => self.build_gestures(),
            _ => self.build_design(),
        }
        let status = Label::left(self.last.clone(), self.theme.small_px * 1.1).styled(Style::Italic);
        let sr = self.status_rect();
        self.ids.status = Some(self.tree.add(None, Box::new(status), sr));
        // The keyboard goes last: it is the top layer of the Input tab.
        if self.tab == 3 && self.keyboard_visible {
            let mut kb = Keyboard::new();
            kb.set_language(&self.keyboard_language);
            kb.preview = self.preview;
            self.ids.keyboard = Some(self.tree.add(None, Box::new(kb), self.keyboard_rect()));
        }
        let size = self.size;
        self.tree.invalidate(Rect::from_size(size), RefreshClass::Gc16);
    }

    fn build_controls(&mut self) {
        let c = self.content_rect();
        let (left, right) = if self.landscape() {
            let cols = split_cols(c, 2, 40);
            (cols[0], Some(cols[1]))
        } else {
            (c, None)
        };
        let mut col = Col::new(left);
        let sample = Label::left(format!("Label · AV To · fi fl · {} px", self.sample_px as u32), self.sample_px);
        self.ids.sample = Some(self.tree.add(None, Box::new(sample), col.take(60)));
        // Dialog at (200, 320) and Settings at (600, 320) in portrait, where the phase 0
        // gallery had them (tests/sim/demo.txt, tests/screens.rs).
        let buttons = [("Dialog", "dialog"), ("Toast", "toast"), ("Settings", "settings"), ("Popup", "popup"), ("Sheet", "sheet")];
        let rects = flow(&mut col, buttons.len(), 210, 100);
        for ((label, action), r) in buttons.iter().zip(rects) {
            self.add_button(Button::new(*label, *action), r);
        }
        let toggles = flow(&mut col, 2, 460, 90);
        self.ids.night = Some(self.tree.add(None, Box::new(Toggle::new("Night mode", self.theme.night, "night")), toggles[0]));
        self.tree.add(None, Box::new(Toggle::new("Key preview", self.preview, "preview")), toggles[1]);
        let size = Slider::new("Font size", 20.0, 60.0, 2.0, self.sample_px, "size");
        self.tree.add(None, Box::new(size), col.take(160));
        let prog = Slider::new("Reading progress", 0.0, 100.0, 5.0, self.progress * 100.0, "progress");
        self.tree.add(None, Box::new(prog), col.take(160));
        let mut bar = ProgressBar::new(self.progress);
        bar.marks = vec![0.12, 0.3, 0.55, 0.8];
        bar.label = Some(format!("{}%", (self.progress * 100.0).round() as u32));
        self.ids.progress = Some(self.tree.add(None, Box::new(bar), col.take(60)));
        let icons_area = right.unwrap_or_else(|| col.rest());
        let cell = 116u32;
        let per_row = (icons_area.w / cell).max(1);
        for (i, icon) in Icon::ALL.iter().enumerate() {
            let (r, k) = (i as u32 / per_row, i as u32 % per_row);
            let rect = Rect::new(icons_area.x + (k * cell) as i32, icons_area.y + (r * cell) as i32, cell - 8, cell - 8);
            if rect.bottom() > icons_area.bottom() {
                break;
            }
            let action = format!("icon:{icon:?}");
            self.buttons.push((action.clone(), rect));
            self.tree.add(None, Box::new(IconView::new(*icon, action)), rect);
        }
    }

    fn build_lists(&mut self) {
        let c = self.content_rect();
        let menu = Menu::new(vec![
            ("Go to page".into(), "menu:goto".into()),
            ("Search in book".into(), "menu:search".into()),
            ("Bookmarks".into(), "menu:bookmarks".into()),
        ]);
        let (menu_r, list_r) = if self.landscape() {
            let cols = split_cols(c, 2, 40);
            (Rect::new(cols[0].x, cols[0].y, cols[0].w, menu.height()), cols[1])
        } else {
            let mut col = Col::new(c);
            (col.take(menu.height()), col.rest())
        };
        self.tree.add(None, Box::new(menu), menu_r);
        let authors = ["Jane Austen", "Herman Melville", "Mary Shelley", "Leo Tolstoy", "H. C. Andersen"];
        let items: Vec<ListItem> = (1..=40)
            .map(|i| {
                let mut it = ListItem::new(format!("Book {i}"), format!("list:{i}"))
                    .subtitle(authors[i % authors.len()])
                    .detail(format!("{}.{} MB", i % 7 + 1, i % 10));
                if i % 4 == 1 {
                    it = it.icon(Icon::Bookmark);
                }
                if i % 3 == 0 {
                    it = it.checked(true);
                }
                it
            })
            .collect();
        self.tree.add(None, Box::new(List::new(items, 120)), list_r);
    }

    fn build_text(&mut self) {
        let c = self.content_rect();
        let mut v = ScrollView::new();
        let head = |t: &str| Box::new(Label::left(t, 40.0).styled(Style::Bold));
        v.push(head("Shaping (rustybuzz)"), 64);
        let sample = ShapingSample::gallery(40.0);
        let h = sample.height();
        v.push(Box::new(sample), h);
        v.push(head("Hyphenation, justified"), 64);
        for (i, (lang, text)) in PARAGRAPHS.iter().enumerate() {
            let h = self.text_heights.get(i).copied().unwrap_or(270);
            v.push(Box::new(TextBlock::new(*text, 38.0).hyphenate(lang).aligned(Align::Justify)), h);
        }
        v.push(head("Fonts found"), 64);
        let fonts = if self.font_lines.is_empty() { "No font directories scanned yet.".to_string() } else { self.font_lines.join("\n") };
        v.push(Box::new(TextBlock::new(fonts, 26.0)), 420);
        self.tree.add(None, Box::new(v), c);
    }

    fn build_input(&mut self) {
        let mut col = Col::new(self.content_rect());
        let mut input = TextInput::new("Type here", "input");
        input.set_text(&self.texts[0]);
        input.focused = self.focus == 0;
        let r = col.take(110);
        self.buttons.push(("focus:input".into(), r));
        self.ids.input = Some(self.tree.add(None, Box::new(input), r));
        let mut pw = TextInput::new("Password", "password");
        pw.password = true;
        pw.set_text(&self.texts[1]);
        pw.focused = self.focus == 1;
        let r = col.take(110);
        self.buttons.push(("focus:password".into(), r));
        self.ids.password = Some(self.tree.add(None, Box::new(pw), r));
        let typed = format!("Keyboard: {} · hold a key for accents", language_name(&self.keyboard_language));
        self.ids.typed = Some(self.tree.add(None, Box::new(Label::left(typed, 30.0)), col.take(60)));
    }

    /// The Design tab: one of each design-system widget (`docs/DESIGN.md`).
    fn build_design(&mut self) {
        let c = self.content_rect();
        let (left, right) = if self.landscape() {
            let cols = split_cols(c, 2, 40);
            (cols[0], Some(cols[1]))
        } else {
            (c, None)
        };
        let mut col = Col::new(left);
        let bar = AppBar::new("Book details")
            .with_subtitle("App bar: back, title, actions")
            .with_back()
            .action(BarAction::icon(Icon::Search, "d:search"))
            .action(BarAction::icon(Icon::More, "d:more"));
        self.tree.add(None, Box::new(bar), col.take(APP_BAR_H));
        let h = self.theme.control_h();
        let kinds = [
            Button::filled("Read", "d:read").with_icon(Icon::Book),
            Button::new("Details", "d:details"),
            Button::text("Cancel", "d:cancel"),
            Button::new("Delete", "d:delete").disabled(),
        ];
        let rects = flow(&mut col, kinds.len(), 270, h);
        for (b, r) in kinds.into_iter().zip(rects) {
            self.add_button(b, r);
        }
        let seg = Segmented::new(&["List", "Grid", "Covers"], "d:view").with_selected(1);
        self.tree.add(None, Box::new(seg), col.take(h));
        let chips = Chips::new(&["Unread", "Reading", "Finished", "EPUB", "PDF"], "d:chip").with_selected(1).with_selected(3);
        // One row of chips in portrait; the narrower landscape column wraps to two.
        let rows = if self.landscape() { 2 } else { 1 };
        self.tree.add(None, Box::new(chips), col.take(rows * Chips::CHIP_H + (rows - 1) * 16));
        let marks = vec![0.08, 0.22, 0.41, 0.6, 0.83];
        let thin = ProgressBar::thin(0.42).with_marks(marks.clone()).with_caption("Act II · Scene 3 · thin bar with chapter ticks").with_label("42%");
        self.tree.add(None, Box::new(thin), col.take(88));
        let thick = ProgressBar::new(0.68).with_marks(marks).with_label("68%");
        self.tree.add(None, Box::new(thick), col.take(56));
        let mut rcol = match right {
            Some(r) => Col::new(r),
            None => Col::new(col.rest()),
        };
        let card_h = 300;
        let cards: Vec<Rect> = if self.landscape() {
            vec![rcol.take(card_h), rcol.take(card_h)]
        } else {
            split_cols(rcol.take(card_h), 2, 24)
        };
        let book = Card::new("Romeo and Juliet", "d:card")
            .subtitle("William Shakespeare")
            .cover(Cover::placeholder("Romeo and Juliet"))
            .progress(0.42);
        let card = self.tree.add(None, Box::new(book), cards[0]);
        let cover = Card::cover_rect(cards[0]);
        let side = (cover.w * 2 / 3).max(64);
        self.tree.add(Some(card), Box::new(RingProgress::new(0.42)), RingProgress::badge_rect(cover, side));
        let note = Card::new("Filled card", "d:card2").kind(CardKind::Filled).body("Cards group one object: a book, a statistic, a setting.");
        self.tree.add(None, Box::new(note), cards[1]);
        let nav = NavBar::new(vec![
            NavItem::new(Icon::Home, "Home", "d:nav:home"),
            NavItem::new(Icon::Library, "Library", "d:nav:library"),
            NavItem::new(Icon::Chart, "Stats", "d:nav:stats"),
            NavItem::new(Icon::Settings, "Settings", "d:nav:settings"),
        ])
        .with_selected(1);
        let rest = rcol.rest();
        let nav_r = Rect::new(rest.x, rest.bottom() - NAV_BAR_H as i32, rest.w, NAV_BAR_H.min(rest.h));
        self.tree.add(None, Box::new(nav), nav_r);
    }

    fn build_gestures(&mut self) {
        let mut pad = GesturePad::default();
        for g in self.gestures.iter().rev().take(8).rev() {
            pad.record(*g);
        }
        self.ids.pad = Some(self.tree.add(None, Box::new(pad), self.content_rect()));
    }

    /// Measures the Text tab's paragraphs for the current width (needs the fonts, so it
    /// runs before a rebuild where a context is at hand).
    fn measure(&mut self, fonts: &mut ink_text::Fonts) {
        let w = self.content_rect().w.saturating_sub(ink_ui::list::SCROLLBAR_W + 12) as f32;
        self.text_heights =
            PARAGRAPHS.iter().map(|(lang, text)| TextBlock::new(*text, 38.0).hyphenate(lang).height(fonts, w)).collect();
    }

    fn set_status(&mut self, text: String) {
        self.last = text;
        if let Some(id) = self.ids.status {
            if let Some(l) = self.tree.get_mut::<Label>(id) {
                l.text = self.last.clone();
            }
            self.tree.mark_dirty(id, RefreshClass::Gl16);
        }
    }

    fn close_overlays(&mut self) {
        while let Some(id) = self.overlays.pop() {
            self.tree.remove(id);
        }
    }

    fn open_dialog(&mut self) {
        let screen = Rect::from_size(self.size);
        self.overlays.push(self.tree.add(None, Box::new(Scrim::new("")), screen));
        let d = Dialog {
            title: "Delete book?".into(),
            body: "Dialogs are modal layers: the scrim under this one takes every tap outside it.".into(),
            buttons: vec![("Cancel".into(), "dialog:cancel".into()), ("Delete".into(), "dialog:ok".into())],
        };
        self.overlays.push(self.tree.add(None, Box::new(d), Dialog::frame(self.size)));
    }

    fn open_popup(&mut self) {
        let anchor = self.button("popup").unwrap_or_default();
        let screen = Rect::from_size(self.size);
        self.overlays.push(self.tree.add(None, Box::new(Scrim::new("dismiss")), screen));
        let p = Popup {
            title: "Footnote 1".into(),
            text: "Popups show footnotes, dictionary entries and selection tools next to what was tapped. Tap outside to close.".into(),
            buttons: vec![("Close".into(), "dismiss".into())],
            ..Default::default()
        };
        let size = Size::new((self.size.w - 48).min(900), 420);
        self.overlays.push(self.tree.add(None, Box::new(p), Popup::frame(anchor, size, self.size)));
    }

    fn open_sheet(&mut self) {
        let screen = Rect::from_size(self.size);
        self.overlays.push(self.tree.add(None, Box::new(Scrim::new("dismiss")), screen));
        let h = if self.landscape() { self.size.h * 3 / 5 } else { self.size.h * 9 / 20 };
        let frame = BottomSheet::frame(self.size, h);
        let sheet = self.tree.add(None, Box::new(BottomSheet { title: "Aa".into(), taken: false }), frame);
        self.overlays.push(sheet);
        let mut col = Col::new(BottomSheet::content(frame));
        self.tree.add(Some(sheet), Box::new(Tabs::new(&["Font", "Layout", "Themes"], "aa-tab")), col.take(90));
        self.tree.add(Some(sheet), Box::new(Slider::new("Size", 1.0, 14.0, 1.0, 6.0, "aa-size")), col.take(150));
        self.tree.add(Some(sheet), Box::new(Slider::new("Line spacing", 1.0, 2.0, 0.1, 1.3, "aa-spacing")), col.take(150));
        self.tree.add(Some(sheet), Box::new(Toggle::new("Bold", false, "aa-bold")), col.take(90));
    }

    fn set_night(&mut self, on: bool, cx: &mut AppCtx) -> anyhow::Result<()> {
        self.theme = if on { Theme::night() } else { Theme::day() };
        if let Some(id) = self.ids.night {
            if let Some(t) = self.tree.get_mut::<Toggle>(id) {
                t.on = on;
            }
        }
        if self.theme.apply(cx.screen)? {
            log::info!("demo night mode {}", if on { "on" } else { "off" });
            let size = self.size;
            self.tree.invalidate(Rect::from_size(size), RefreshClass::Gc16);
        }
        Ok(())
    }

    fn rotate(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        let r = cx.screen.rotation().next();
        self.set_rotation(r, cx)
    }

    fn set_rotation(&mut self, r: Rotation, cx: &mut AppCtx) -> anyhow::Result<()> {
        cx.screen.set_rotation(r)?;
        self.size = cx.screen.size();
        *cx.canvas = Canvas::new(self.size.w as usize, self.size.h as usize);
        self.measure(cx.fonts);
        self.rebuild();
        self.log_layout(cx);
        Ok(())
    }

    /// Logs where each button and keyboard key is on the panel (for emulator scripts).
    fn log_layout(&mut self, cx: &AppCtx) {
        let (rot, panel) = (cx.screen.rotation(), cx.screen.panel_size());
        log::info!("demo layout {rot:?} {}x{} tab {}", self.size.w, self.size.h, TABS[self.tab]);
        for (a, r) in &self.buttons {
            if a.starts_with("icon:") {
                continue;
            }
            let p = rot.to_panel(Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2), panel);
            log::info!("demo button {a} panel {} {}", p.x, p.y);
        }
        if let Some(id) = self.ids.keyboard {
            let b = self.tree.bounds(id).unwrap_or_default();
            if let Some(kb) = self.tree.get_mut::<Keyboard>(id) {
                let area = kb.keys_area(b);
                for (r, k) in kb.layout.key_rects(area) {
                    let p = rot.to_panel(Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2), panel);
                    log::info!("demo key {} panel {} {}", k.label, p.x, p.y);
                }
            }
        }
    }

    fn keyboard_actions(&mut self) {
        let Some(kid) = self.ids.keyboard else { return };
        let actions = self.tree.get_mut::<Keyboard>(kid).map(|k| k.take_actions()).unwrap_or_default();
        let field = if self.focus == 0 { self.ids.input } else { self.ids.password };
        for a in actions {
            if a == KeyAction::NextLanguage {
                if let Some(k) = self.tree.get_mut::<Keyboard>(kid) {
                    self.keyboard_language = k.current_language().to_string();
                }
                let text = format!("Keyboard: {} · hold a key for accents", language_name(&self.keyboard_language));
                if let Some(id) = self.ids.typed {
                    if let Some(l) = self.tree.get_mut::<Label>(id) {
                        l.text = text;
                    }
                    self.tree.mark_dirty(id, RefreshClass::Gl16);
                }
                log::info!("demo keyboard language {}", self.keyboard_language);
                self.set_status(format!("keyboard language {}", self.keyboard_language));
                continue;
            }
            let Some(fid) = field else { continue };
            let result = self.tree.get_mut::<TextInput>(fid).map(|t| (t.apply(&a), t.text.clone()));
            match result {
                Some((InputResult::Edited, text)) => {
                    self.texts[self.focus] = text;
                    self.tree.mark_dirty(fid, RefreshClass::Du);
                    let shown = if self.focus == 1 { "•".repeat(self.texts[1].chars().count()) } else { self.texts[0].clone() };
                    log::info!("demo text {}: {shown}", if self.focus == 1 { "password" } else { "input" });
                    self.set_status(format!("typed: {shown}"));
                }
                Some((InputResult::Submitted, text)) => self.set_status(format!("submitted: {text}")),
                Some((InputResult::Closed, _)) => {
                    self.keyboard_visible = false;
                    self.rebuild();
                    self.set_status("keyboard hidden; tap a field to show it".into());
                }
                _ => {}
            }
        }
    }

    fn action(&mut self, a: &str, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        log::info!("demo action: {a}");
        self.actions.push(a.to_string());
        if self.actions.len() > 200 {
            self.actions.remove(0);
        }
        match a {
            "exit" => return Ok(Transition::Exit),
            "settings" => return Ok(Transition::Push(Route::Settings)),
            "rotate" => self.rotate(cx)?,
            "night" => {
                let on = !self.theme.night;
                self.set_night(on, cx)?;
            }
            "night:on" | "night:off" => self.set_night(a == "night:on", cx)?,
            "dialog" => self.open_dialog(),
            "toast" => {
                let t = Toast::new("This is a toast; it goes away by itself");
                let ttl = t.timeout;
                let frame = Toast::frame(self.size);
                self.tree.add_timed(Box::new(t), frame, ttl);
            }
            "popup" => self.open_popup(),
            "sheet" => self.open_sheet(),
            "dismiss" | "close" | "dialog:cancel" | "dialog:ok" => {
                self.close_overlays();
                // Large overlays leave ghosts after partial updates: flash their area.
                cx.refresh.request_ui_flash();
            }
            "keyboard" => {
                let lang = self.keyboard_language.clone();
                self.keyboard_actions();
                if lang != self.keyboard_language {
                    self.log_layout(cx);
                }
            }
            "preview:on" | "preview:off" => self.preview = a == "preview:on",
            _ if a.starts_with("tab:") => {
                if let Ok(i) = a[4..].parse::<usize>() {
                    self.tab = i.min(TABS.len() - 1);
                    if self.tab == 3 {
                        self.keyboard_visible = true;
                    }
                    self.rebuild();
                    self.log_layout(cx);
                }
            }
            _ if a.starts_with("size:") => {
                if let Ok(v) = a[5..].parse::<f32>() {
                    self.sample_px = v;
                    if let Some(id) = self.ids.sample {
                        if let Some(l) = self.tree.get_mut::<Label>(id) {
                            l.px = v;
                            l.text = format!("Label · AV To · fi fl · {} px", v as u32);
                        }
                        self.tree.mark_dirty(id, RefreshClass::Gl16);
                    }
                }
            }
            _ if a.starts_with("progress:") => {
                if let Ok(v) = a[9..].parse::<f32>() {
                    self.progress = v / 100.0;
                    if let Some(id) = self.ids.progress {
                        if let Some(p) = self.tree.get_mut::<ProgressBar>(id) {
                            p.value = v / 100.0;
                            p.label = Some(format!("{}%", v as u32));
                        }
                        self.tree.mark_dirty(id, RefreshClass::Gl16);
                    }
                }
            }
            _ if a.starts_with("focus:") => {
                self.focus = if a == "focus:password" { 1 } else { 0 };
                let was_visible = self.keyboard_visible;
                self.keyboard_visible = true;
                if !was_visible {
                    self.rebuild();
                } else {
                    for (i, id) in [self.ids.input, self.ids.password].into_iter().enumerate() {
                        if let Some(id) = id {
                            if let Some(t) = self.tree.get_mut::<TextInput>(id) {
                                t.focused = i == self.focus;
                            }
                            self.tree.mark_dirty(id, RefreshClass::Du);
                        }
                    }
                }
            }
            _ => {}
        }
        if a != "keyboard" {
            self.set_status(format!("action: {a}"));
        }
        Ok(Transition::None)
    }
}

impl AppScreen for DemoScreen {
    fn name(&self) -> &'static str {
        "demo"
    }

    fn on_enter(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        if cx.fonts.face_count() <= 3 {
            let dirs = font_dirs();
            let entries = scan_fonts(&dirs);
            self.font_lines = summary(&dirs, &entries);
            for l in &self.font_lines {
                log::info!("fonts {l}");
            }
            let n = cx.fonts.load_fallbacks(&dirs);
            log::info!("fonts: {n} fallback faces loaded");
            self.font_lines.push(format!("{n} fallback faces in use after Noto Serif."));
        }
        self.size = cx.screen.size();
        if cx.canvas.size() != self.size {
            *cx.canvas = Canvas::new(self.size.w as usize, self.size.h as usize);
        }
        self.theme.night = cx.screen.night_mode();
        self.measure(cx.fonts);
        self.rebuild();
        self.log_layout(cx);
        Ok(())
    }

    fn on_exit(&mut self, cx: &mut AppCtx) {
        // Leave the panel upright and in day mode for whatever runs next.
        if cx.screen.rotation() != Rotation::Portrait {
            if let Err(e) = cx.screen.set_rotation(Rotation::Portrait) {
                log::warn!("demo: {e}");
            }
            let s = cx.screen.size();
            *cx.canvas = Canvas::new(s.w as usize, s.h as usize);
        }
        if let Err(e) = Theme::day().apply(cx.screen) {
            log::warn!("demo: {e}");
        }
    }

    fn handle(&mut self, ev: &UiEvent, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        match ev {
            UiEvent::Quit => return Ok(Transition::Exit),
            UiEvent::Key(k) if k.pressed && k.key == Key::Power => return Ok(Transition::Exit),
            UiEvent::Timer(_) => {
                self.tree.expire();
                return Ok(Transition::None);
            }
            UiEvent::Gesture(g) => {
                self.gestures.push(*g);
                if self.gestures.len() > 200 {
                    self.gestures.remove(0);
                }
                if let Some(id) = self.ids.pad {
                    if let Some(p) = self.tree.get_mut::<GesturePad>(id) {
                        p.record(*g);
                    }
                    self.tree.mark_dirty(id, RefreshClass::Du);
                }
            }
            _ => {}
        }
        // A toast that timed out while other input came in.
        self.tree.expire();
        match (self.tree.dispatch(ev), ev) {
            (Some((_, EventResult::Action(a))), _) => self.action(&a, cx),
            // Gestures that did nothing else show in the status line (not while typing).
            (_, UiEvent::Gesture(g)) if self.tab != 3 => {
                self.set_status(format!("gesture: {}", describe(g)));
                Ok(Transition::None)
            }
            _ => Ok(Transition::None),
        }
    }

    fn paint(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        // The app loop coalesces and applies the UI flash policy (app::present).
        Ok(self.tree.paint(&mut ink_ui::PaintCx::new(cx.canvas, cx.fonts, &self.theme)))
    }

    fn timeout(&self) -> Option<Duration> {
        self.tree.next_expiry()
    }
}
