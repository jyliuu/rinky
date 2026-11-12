//! Settings screen.
//!
//! One screen with pages. The main page is ONE vertically scrolling list ([`rows`]: a
//! swipe jumps half a page, a long swipe a whole page, one GL16 repaint) with sections:
//! Reading (font, page turn wave, status bars, default layout, full refresh), Gestures,
//! Control center, Home & library (navigation bar, KOReader import, library folders),
//! Keyboard, Plugins (every plugin with its switch and its settings pages), Device
//! (light, Wi-Fi, sleep, screensaver, orientation, night mode, language, device info),
//! Diagnostics, About (update) and "Exit to Kindle" at the bottom. Every row has an icon
//! and shows its current value on the right, switches sit on the right edge. Sub-pages
//! have the app bar with a back button: choice lists, Light sliders, Wi-Fi (signal
//! bars, locks, scan, join with a password, saved networks), ordered lists (move up and
//! down, on/off), device info cards and Update. Device services come from
//! [`AppCtx::services`]; a service the device does not have shows "not available".
//! Settings changes go through [`AppCtx::settings_update`]: the app stores them, saves
//! `settings.toml` and applies the rotation.
//!
//! # Adding a page from another module
//!
//! A page that lives in its own file (`settings/<name>.rs`) implements [`PageModule`]
//! and is listed in [`PAGE_MODULES`]: one [`PageInfo`] line names its key, section,
//! icon and row title, and `make` builds it. Entries whose module is not written yet
//! have `make: None` and show "Coming soon". To wire a module:
//!
//! 1. add `pub mod <name>;` next to the other `pub mod` lines below;
//! 2. in its [`PAGE_MODULES`] line replace `make: None` with
//!    `make: Some(|| Box::new(<name>::Page::default()))` (any constructor works).
//!
//! The page shows the rows [`PageModule::rows`] returns (the same [`rows::Row`]s as
//! every other page: headers, notes, switches, choices, ordered items); a tap on a row
//! calls [`PageModule::action`] with the row's action, and the page is built again
//! when the reply asks for it. [`edit_settings`] changes a setting.
//!
//! A swipe down from the top edge opens the quick light panel ([`quick_light`]) on every
//! screen: the app pushes [`crate::router::Route::QuickLight`].

pub mod advanced;
pub mod battery;
pub mod control_buttons;
pub mod diagnostics;
#[cfg(test)]
pub mod fakes;
pub mod keyboard_layouts;
pub mod order;
pub mod fonts;
pub mod page;
pub mod popup;
pub mod gestures;
pub mod quick_light;
pub mod rows;
pub mod status_bars;
pub mod worker;

#[cfg(test)]
mod tests;

/// Main-list section titles, in order.
pub const SECTIONS: [&str; 9] =
    ["Reading", "Gestures", "Control center", "Home & library", "Keyboard", "Plugins", "Device", "Diagnostics", "About"];

/// What a [`PageModule`] wants after an action.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reply {
    /// Text for the status line.
    pub status: Option<String>,
    /// Build the page again (new rows).
    pub rebuild: bool,
    /// A screen change (e.g. [`Transition::Pop`]); `None` stays.
    pub transition: Option<Transition>,
}

impl Reply {
    /// Build the page again and show `status`.
    pub fn rebuild(status: impl Into<String>) -> Self {
        Reply { status: Some(status.into()), rebuild: true, transition: None }
    }
}

/// A settings page in its own module, listed in [`PAGE_MODULES`].
pub trait PageModule {
    /// App bar title; `None` uses the [`PageInfo::title`].
    fn title(&self) -> Option<String> {
        None
    }

    /// Value shown on the right of the page's row in the main list.
    fn summary(&self, _settings: &Settings) -> String {
        String::new()
    }

    /// The rows of the page.
    fn rows(&mut self, cx: &mut AppCtx) -> Vec<Row>;

    /// A tap on a row with `action` (toggles add `:on`/`:off`, ordered rows `:up`,
    /// `:down`, `:on`, `:off`).
    fn action(&mut self, action: &str, cx: &mut AppCtx) -> Reply;

    /// Back was pressed: true when the module went back inside itself (a sub-page of
    /// its own) and the page must be built again; false leaves the page.
    fn back(&mut self) -> bool {
        false
    }
}

/// One page module in [`PAGE_MODULES`].
#[derive(Clone, Copy)]
pub struct PageInfo {
    /// Key: `page:mod:<key>` opens it.
    pub key: &'static str,
    /// Main-list section ([`SECTIONS`]).
    pub section: &'static str,
    /// Row icon.
    pub icon: Icon,
    /// Row title and default page title.
    pub title: &'static str,
    /// Builds the page; `None`: not wired yet ("Coming soon").
    pub make: Option<fn() -> Box<dyn PageModule>>,
}

/// Pages that live in their own module (see the module docs). One line per page.
pub const PAGE_MODULES: &[PageInfo] = &[
    PageInfo { key: "fonts", section: "Reading", icon: Icon::Font, title: "Font", make: None },
    PageInfo { key: "status_bars", section: "Reading", icon: Icon::ListView, title: "Status bars", make: None },
    PageInfo { key: "gestures", section: "Gestures", icon: Icon::Book, title: "Reader gestures", make: None },
    PageInfo { key: "control_center", section: "Control center", icon: Icon::Grid, title: "Control center buttons", make: Some(|| Box::new(control_buttons::Page)) },
    PageInfo { key: "keyboard", section: "Keyboard", icon: Icon::Globe, title: "Keyboard layouts", make: Some(|| Box::new(keyboard_layouts::Page)) },
    PageInfo { key: "battery", section: "Device", icon: Icon::Battery, title: "Battery", make: Some(|| Box::new(battery::Page)) },
    PageInfo { key: "diagnostics", section: "Diagnostics", icon: Icon::Info, title: "Diagnostics", make: Some(|| Box::new(diagnostics::Page::default())) },
];

/// The [`PAGE_MODULES`] entry with `key`.
pub fn page_info(key: &str) -> Option<&'static PageInfo> {
    PAGE_MODULES.iter().find(|p| p.key == key)
}

/// Changes the settings the app saves after this event.
pub fn edit_settings(cx: &mut AppCtx, f: impl FnOnce(&mut Settings)) {
    let mut s = cx.settings_update.take().unwrap_or_else(|| cx.settings.clone());
    f(&mut s);
    *cx.settings_update = Some(s);
}

/// The settings as they are after this event's changes.
pub fn current_settings<'a>(cx: &'a AppCtx) -> &'a Settings {
    cx.settings_update.as_ref().unwrap_or(cx.settings)
}

/// The name of an export format in the settings list.
fn export_format_name(f: ExportFormat) -> &'static str {
    match f {
        ExportFormat::Markdown => "Markdown",
        ExportFormat::Clippings => "Kindle clippings",
        ExportFormat::Json => "JSON",
    }
}

/// Where "Export all highlights" writes in format `f`.
fn export_subtitle(f: ExportFormat) -> &'static str {
    match f {
        ExportFormat::Markdown => "One Markdown file per book, in rinky/exports",
        ExportFormat::Clippings => "All books in rinky/exports/My Clippings.txt",
        ExportFormat::Json => "One JSON file per book, in rinky/exports",
    }
}

/// Rows of the KOReader import page (import, library, highlight export and its format).
pub(crate) fn koreader_rows(format: ExportFormat) -> Vec<Row> {
    let mut rows = vec![
        Row::header("From KOReader"),
        Row::item("Import from KOReader", "ko-import").icon(Icon::Download).subtitle("Positions, highlights, notes, status, statistics"),
        Row::note("KOReader's files are only read, never changed."),
        Row::header("Library"),
        Row::item("Rescan the library", "lib-rescan").icon(Icon::Sync).subtitle("Read the book folders again"),
        Row::item("Browse files", "lib-files").icon(Icon::Folder).subtitle("Open a book from any folder"),
        Row::note("These run on the library screen; it opens when you tap."),
        Row::header("Highlights"),
        Row::item("Export all highlights", "export-all").icon(Icon::Download).subtitle(export_subtitle(format)),
        Row::header("Export format"),
    ];
    rows.extend(ExportFormat::ALL.iter().map(|f| Row::choice(export_format_name(*f), *f == format, format!("exportfmt:{}", f.name()))));
    rows.push(Row::note("Export highlights in the reader menu and the book menu use it too."));
    rows
}

/// A module page that is not wired yet.
struct ComingSoon;

impl PageModule for ComingSoon {
    fn summary(&self, _s: &Settings) -> String {
        "Coming soon".into()
    }

    fn rows(&mut self, _cx: &mut AppCtx) -> Vec<Row> {
        vec![Row::note("Coming soon")]
    }

    fn action(&mut self, _action: &str, _cx: &mut AppCtx) -> Reply {
        Reply::default()
    }
}

/// Builds the module of `info` (or the "Coming soon" page).
fn make_module(info: &PageInfo) -> Box<dyn PageModule> {
    match info.make {
        Some(f) => f(),
        None => Box::new(ComingSoon),
    }
}

/// Value of the Reader gestures row: how many gestures have an action.
fn gestures_summary(map: &ink_store::prefs::GestureMap) -> String {
    let set = ink_store::prefs::GESTURES.iter().filter(|(g, _)| map.action(g) != "none").count();
    format!("{set} of {} set", ink_store::prefs::GESTURES.len())
}

/// The main-list rows of the module pages in `section`, without values.
fn module_nav_rows(section: &str) -> impl Iterator<Item = (&'static PageInfo, Row)> + '_ {
    PAGE_MODULES.iter().filter(move |p| p.section == section).map(|p| (p, Row::nav(p.icon, p.title, format!("page:mod:{}", p.key))))
}

/// The main-list rows of the module pages in `section`, with their values.
fn module_rows(section: &str, s: &Settings) -> Vec<Row> {
    module_nav_rows(section)
        .map(|(p, row)| {
            // fonts and status_bars are Page variants (not PageModule): their own summaries.
            let value = match p.key {
                "fonts" => fonts::summary(s),
                "status_bars" => status_bars::summary(&s.status_bars).into(),
                "gestures" => gestures_summary(&s.gestures),
                _ => make_module(p).summary(s),
            };
            row.value(value)
        })
        .collect()
}

use crate::screens::common::paint_cx;
use crate::router::{AppCtx, AppScreen, ScreenRegistry, Transition};
use crate::plugin_host::{self, hooks, PluginEntry};
use crate::update;
use ink_device::{Device, Key, NetEvent, NetStatus, PowerEvent, Rect, RefreshClass, Rotation, Size, WifiNetwork};
use ink_store::export::ExportFormat;
use ink_store::settings::{INDENT_BOOK, WEIGHT_MAX};
use ink_store::{Paths, Settings};
use ink_ui::bars::APP_BAR_H;
use ink_ui::input::InputResult;
use ink_ui::theme::space;
use ink_ui::{
    AppBar, BarAction, Button, Card, CardKind, Damage, Dialog, EventResult, Icon, KeyAction, Keyboard, Label,
    Scrim, TextBlock, TextInput, UiEvent, WidgetId, WidgetTree,
};
use ink_plugin::UiRequest;
use quick_light::QuickLight;
use rows::{Lead, Row, SettingsList, Trail};
use ink_ui::refresh::Change;
use std::time::Duration;
use worker::{Job, Poll, Reporter};

thread_local! {
    /// Page the next [`crate::router::Route::Settings`] opens on (set by [`open_at`];
    /// screens are built on the UI thread).
    static START_PAGE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Makes the next settings screen open on the page named `name` ([`Page::from_name`],
/// e.g. `status_bars`) instead of the main list. The caller then pushes
/// [`crate::router::Route::Settings`].
pub fn open_at(name: &str) {
    START_PAGE.with(|p| *p.borrow_mut() = Some(name.to_string()));
}

/// Registers settings for [`crate::router::Route::Settings`].
pub fn register(reg: &mut ScreenRegistry) {
    reg.register("settings", |_| {
        let mut screen = SettingsScreen::default();
        let start = START_PAGE.with(|p| p.borrow_mut().take());
        if let Some(page) = start.as_deref().and_then(Page::from_name) {
            log::info!("settings: start on page {page:?}");
            screen.module = match &page {
                Page::Module(key) => page_info(key).map(make_module),
                _ => None,
            };
            screen.start = Some(page.clone());
            screen.page = page;
        }
        Ok(Box::new(screen))
    });
    reg.register("quicklight", |_| Ok(Box::new(quick_light::ControlCenterScreen::default())));
}

pub use page::{app_bar_rect, content_rect, STATUS_H};

/// Value of the Notifications row.
fn notifications_summary(cx: &AppCtx) -> String {
    match (cx.plugins.notifications().len(), cx.plugins.unread_notifications()) {
        (0, _) => "None".to_string(),
        (n, 0) => format!("{n}, all read"),
        (_, u) => format!("{u} unread"),
    }
}

/// The page turn wave setting (`reader.page_animation`, `on` / `off`): the panel sweeps
/// a page turn's one update across the screen.
pub fn page_wave(s: &Settings) -> bool {
    ink_store::prefs::page_animation_on(&s.reader.page_animation)
}

/// Stores the page turn wave setting.
pub fn set_page_wave(s: &mut Settings, on: bool) {
    s.reader.page_animation = if on { "on" } else { "off" }.into();
}

/// UI languages (`language`), as `(code, name)`.
pub const LANGUAGES: [(&str, &str); 2] = [("en", "English"), ("da", "Dansk")];

fn language_name(code: &str) -> String {
    LANGUAGES.iter().find(|(c, _)| *c == code).map_or_else(|| code.to_string(), |(_, n)| n.to_string())
}

/// Text sizes on the default layout page.
pub const FONT_SIZES: [f32; 6] = [32.0, 36.0, 40.0, 44.0, 48.0, 56.0];
/// Line spacings on the default layout page.
pub const LINE_SPACINGS: [f32; 4] = [1.2, 1.4, 1.6, 1.8];
/// Side margins on the default layout page, `(px, name)`.
pub const MARGINS: [(f32, &str); 3] = [(40.0, "Narrow"), (70.0, "Medium"), (110.0, "Wide")];

/// Paragraph spacings on the default layout page, `(em, name)`.
pub const PARA_SPACINGS: [(f32, &str); 3] = [(0.0, "None"), (0.5, "Half a line"), (1.0, "One line")];
/// First-line indents on the default layout page, `(em, name)`; negative: the book's.
pub const INDENTS: [(f32, &str); 5] = [(-1.0, "Book's own"), (0.0, "None"), (1.0, "1 em"), (1.5, "1.5 em"), (2.0, "2 em")];
/// Text weights on the default layout page, `(level, name)`.
pub const WEIGHTS: [(u8, &str); 3] = [(0, "Normal"), (1, "Darker"), (2, "Darkest")];

/// The library folders offered: the chosen ones, then the folders in `root` (the
/// user storage)
/// (hidden ones and rinky's own skipped), at most 40.
pub fn folder_choices(chosen: &[std::path::PathBuf], root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = chosen.to_vec();
    if let Ok(rd) = std::fs::read_dir(root) {
        let mut found: Vec<std::path::PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .filter(|p| p.file_name().is_some_and(|n| !n.to_string_lossy().starts_with('.') && n != "rinky" && n != "system"))
            .collect();
        found.sort();
        for p in found {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out.truncate(40);
    out
}

/// Fewest items in the navigation bar.
pub const NAV_MIN: usize = 2;
/// Most items in the navigation bar.
pub const NAV_MAX: usize = 5;

/// Full refresh choices in pages (0 = never), `reader.flash_every`.
pub const REFRESH_CHOICES: [u32; 6] = [1, 4, 8, 16, 32, 0];

/// Name of a full refresh choice (`Every 8 pages`, `Never`).
pub fn refresh_name(n: u32) -> String {
    match n {
        0 => "Never".into(),
        1 => "Every page".into(),
        n => format!("Every {n} pages"),
    }
}

/// A plugin and whether the user wants it on (`plugins/disabled.txt`).
pub type PluginChoice = (ink_plugin::PluginInfo, bool);

/// How a plugin's running state differs from the user's choice `wanted` (`None`: it
/// runs as chosen).
fn plugin_change(p: &ink_plugin::PluginInfo, wanted: bool) -> Option<String> {
    match (p.enabled, wanted) {
        (true, true) | (false, false) => None,
        (true, false) => Some("off at the next start".to_string()),
        (false, true) if p.errors > 0 => Some(format!("stopped after {} errors", p.errors)),
        (false, true) => Some("on at the next start".to_string()),
    }
}

/// Second line of a plugin row: kind, state and description. `wanted` is the user's
/// choice in `plugins/disabled.txt` (applied at the next start).
pub fn plugin_detail(p: &ink_plugin::PluginInfo, wanted: bool) -> String {
    let state = plugin_change(p, wanted).unwrap_or_else(|| if wanted { "on" } else { "off" }.to_string());
    let mut s = format!("{} · {state}", p.kind);
    if !p.manifest.description.is_empty() {
        s.push_str(" · ");
        s.push_str(&p.manifest.description);
    }
    s
}

/// Subtitle of a plugin's switch in the main list: the description, after the state only
/// when the switch does not show it ("off at the next start", "stopped after 2 errors"),
/// and "Lua plugin" for a user plugin. The implementation kind of a built-in plugin
/// ("rust") and a plain "on"/"off" are left out: the switch shows them.
pub fn plugin_summary(p: &ink_plugin::PluginInfo, wanted: bool) -> String {
    let state = plugin_change(p, wanted);
    let kind = (p.kind == "lua").then(|| "Lua plugin".to_string());
    let desc = Some(p.manifest.description.clone()).filter(|d| !d.is_empty());
    let parts: Vec<String> = [state, kind, desc].into_iter().flatten().collect();
    parts.join(" · ")
}

/// Auto-sleep choices in minutes (0 = never).
pub const SLEEP_CHOICES: [u32; 7] = [0, 2, 5, 10, 15, 30, 60];

/// The screensaver setting (`page`, `cover`, `blank`, `image:PATH`), `page` when empty.
/// Stored in `settings.device.screensaver`; files with the wave B
/// `[plugins.settings-ui] screensaver` are moved there by `Settings::migrate` at load.
pub fn screensaver_setting(s: &Settings) -> String {
    match s.device.screensaver.trim() {
        "" => ink_store::settings::DEFAULT_SCREENSAVER.into(),
        v => v.to_string(),
    }
}

fn set_screensaver_setting(s: &mut Settings, value: &str) {
    s.device.screensaver = value.to_string();
}

/// The auto-sleep timeout for `minutes` (0 = never).
pub fn auto_sleep(minutes: u32) -> Option<Duration> {
    (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60))
}

/// Gives the device the saved settings: night mode (when on), auto-sleep timeout and
/// screensaver. Called once at start; a device without a service is left alone.
pub fn apply_device_settings(dev: &mut Device, s: &Settings, paths: &Paths) {
    if s.device.night_mode && !dev.screen.night_mode() {
        if let Err(e) = dev.screen.set_night_mode(true) {
            log::debug!("settings: night mode: {e}");
        }
    }
    if let Err(e) = dev.power.set_auto_sleep(auto_sleep(s.device.sleep_minutes)) {
        log::debug!("settings: auto-sleep: {e}");
    }
    if let Err(e) = dev.power.set_screensaver(crate::sleep_cover::screensaver(&screensaver_setting(s), paths)) {
        log::debug!("settings: screensaver: {e}");
    }
}

/// Name of a rotation in the Display page.
pub fn rotation_name(r: Rotation) -> &'static str {
    match r {
        Rotation::Portrait => "Portrait",
        Rotation::Clockwise => "Landscape, turned right",
        Rotation::UpsideDown => "Portrait, upside down",
        Rotation::CounterClockwise => "Landscape, turned left",
    }
}

fn screensaver_name(v: &str) -> String {
    match v {
        "page" => "Current page".into(),
        "cover" => "Book cover".into(),
        "blank" => "Blank screen".into(),
        other => match other.strip_prefix("image:") {
            Some(p) => std::path::Path::new(p).file_name().map_or(p.to_string(), |n| n.to_string_lossy().into_owned()),
            None => other.to_string(),
        },
    }
}

fn sleep_short(minutes: u32) -> String {
    match minutes {
        0 => "Never".into(),
        1 => "1 minute".into(),
        m => format!("{m} minutes"),
    }
}

fn sleep_name(minutes: u32) -> String {
    match minutes {
        0 => "Never".into(),
        1 => "After 1 minute".into(),
        m => format!("After {m} minutes"),
    }
}

/// A network's second line in the Wi-Fi list.
pub fn network_detail(n: &WifiNetwork) -> String {
    let mut parts = vec![format!("signal {}/5", n.signal)];
    parts.push(if n.secured { "secured".into() } else { "open".into() });
    if n.known {
        parts.push("saved".into());
    }
    parts.join(" · ")
}

/// Text for the result of an update check.
pub fn update_check_text(result: &update::Result<(update::Release, bool)>) -> String {
    match result {
        Ok((r, true)) => format!("Version {} is available (installed: {})", r.version, update::current_version()),
        Ok((r, false)) => format!("Up to date (installed {}, latest {})", update::current_version(), r.version),
        Err(e) => format!("Update check failed: {e}"),
    }
}

/// Height of a slider on the Light page.
pub const LIGHT_SLIDER_H: u32 = 200;
/// Top of the password label on the join page.
pub const JOIN_TOP: i32 = APP_BAR_H as i32 + 32;
/// Height of a device info card.
pub const DEVICE_CARD_H: u32 = 256;
/// Height of the date and time card.
pub const CLOCK_CARD_H: u32 = 168;
/// Height of the card on the Update page.
pub const UPDATE_CARD_H: u32 = 360;
/// After an update was installed: how long "Restarting rinky…" shows before rinky exits
/// and the launcher starts the new version.
const RESTART_DELAY: Duration = Duration::from_millis(500);

/// The last `max` characters of a path, with a leading ellipsis when cut
/// (card bodies clip at the card edge; the end of a path is the useful part).
pub fn path_tail(path: &str, max: usize) -> String {
    let n = path.chars().count();
    if n <= max {
        return path.to_string();
    }
    let tail: String = path.chars().skip(n - (max - 1)).collect();
    format!("…{tail}")
}

/// The Join button on the join page.
pub fn join_button_rect(size: Size) -> Rect {
    let w = 320;
    Rect::new(size.w as i32 - space(4) - w, JOIN_TOP + 220, w as u32, 104)
}

/// Rectangles of the device info cards (one column in portrait, two in landscape)
/// and of the button under them.
pub fn device_layout(content: Rect, heights: Vec<u32>) -> (Vec<Rect>, Rect) {
    let cols = if content.w > 1400 { 2 } else { 1 };
    let gap = space(3);
    let x0 = content.x + space(4);
    let w = (content.w as i32 - 2 * space(4) - gap * (cols - 1)) / cols;
    let mut ys = vec![content.y + space(2); cols as usize];
    let mut out = Vec::new();
    for (i, h) in heights.into_iter().enumerate() {
        let c = i % cols as usize;
        out.push(Rect::new(x0 + c as i32 * (w + gap), ys[c], w as u32, h));
        ys[c] += h as i32 + gap;
    }
    let y = ys.into_iter().max().unwrap_or(content.y);
    (out, Rect::new(x0, y, (content.w as i32 - 2 * space(4)) as u32, 104))
}

/// Values shown on the main page.
#[derive(Clone, Debug, Default)]
pub struct MainValues {
    /// Full refresh.
    pub refresh: String,
    /// Orientation.
    pub rotation: String,
    /// Night mode on.
    pub night: bool,
    /// Page turn wave on.
    pub wave: bool,
    /// The panel cannot sweep a page turn: no "Page turn wave" toggle.
    pub wave_hidden: bool,
    /// Default layout (text size, spacing).
    pub layout: String,
    /// Navigation bar items.
    pub nav: String,
    /// Library folders.
    pub folders: String,
    /// Brightness and warmth.
    pub light: String,
    /// Wi-Fi network or state.
    pub wifi: String,
    /// Airplane mode on.
    pub airplane: bool,
    /// Sleep timeout.
    pub sleep: String,
    /// Screensaver.
    pub screensaver: String,
    /// UI language.
    pub language: String,
    /// Battery or model.
    pub device: String,
    /// Rows of the Plugins section (see [`plugin_section`]).
    pub plugins: Vec<Row>,
    /// Notifications.
    pub notifications: String,
    /// Installed version.
    pub version: String,
    /// Ask before "Exit to Kindle".
    pub confirm_exit: bool,
    /// Module rows per section (see [`module_rows`]); empty: the registry's titles
    /// without values.
    pub modules: Vec<(&'static str, Vec<Row>)>,
}

/// The rows of `section`'s module pages in `v` (or from the registry).
fn section_modules(v: &MainValues, section: &str) -> Vec<Row> {
    match v.modules.iter().find(|(s, _)| *s == section) {
        Some((_, rows)) => rows.clone(),
        None => module_nav_rows(section).map(|(_, row)| row).collect(),
    }
}

/// The main page: one scrolling list with the sections in [`SECTIONS`] order and
/// "Exit to Kindle" at the bottom.
pub fn main_rows(v: &MainValues) -> Vec<Row> {
    let m = |s: &str| section_modules(v, s);
    let mut rows = vec![Row::header("Reading")];
    rows.extend(m("Reading").into_iter().take(1));
    if !v.wave_hidden {
        rows.push(Row::toggle("Page turn wave", v.wave, "anim").icon(Icon::Book).subtitle("The screen sweeps the new page in"));
    }
    rows.extend(m("Reading").into_iter().skip(1));
    rows.push(Row::nav(Icon::Font, "Default layout", "page:layout").value(&v.layout));
    rows.push(Row::nav(Icon::Sync, "Full refresh", "page:refresh").value(&v.refresh));
    rows.push(Row::header("Gestures"));
    rows.extend(m("Gestures"));
    rows.push(Row::header("Control center"));
    rows.extend(m("Control center"));
    rows.push(Row::header("Home & library"));
    rows.push(Row::nav(Icon::Menu, "Navigation bar", "page:nav").value(&v.nav));
    rows.push(Row::nav(Icon::Folder, "Library folders", "page:folders").value(&v.folders));
    rows.push(Row::nav(Icon::Download, "KOReader import", "page:koreader"));
    rows.extend(m("Home & library"));
    rows.push(Row::header("Keyboard"));
    rows.extend(m("Keyboard"));
    rows.push(Row::header("Plugins"));
    rows.extend(v.plugins.iter().cloned());
    rows.push(Row::nav(Icon::Chat, "Notifications", "page:notifications").value(&v.notifications));
    rows.extend(m("Plugins"));
    rows.push(Row::header("Device"));
    rows.extend([
        Row::nav(Icon::Light, "Light", "page:light").value(&v.light),
        Row::nav(Icon::Wifi, "Wi-Fi", "page:wifi").value(&v.wifi),
        Row::toggle("Airplane mode", v.airplane, "airplane").icon(Icon::Globe),
        Row::nav(Icon::Clock, "Sleep", "page:sleep").value(&v.sleep),
        Row::nav(Icon::Star, "Screensaver", "page:screensaver").value(&v.screensaver),
        Row::nav(Icon::Rotate, "Orientation", "page:display").value(&v.rotation),
        Row::toggle("Night mode", v.night, "night").icon(Icon::Night),
        Row::nav(Icon::Chat, "Language", "page:language").value(&v.language),
        Row::nav(Icon::Info, "Device info", "page:device").value(&v.device),
    ]);
    rows.extend(m("Device"));
    rows.push(Row::header("Diagnostics"));
    rows.extend(m("Diagnostics"));
    rows.push(Row::header("About"));
    rows.push(Row::nav(Icon::Download, "Update", "page:update").value(&v.version));
    rows.extend(m("About"));
    rows.push(Row::toggle("Ask before exit", v.confirm_exit, "confirm-exit").icon(Icon::Info));
    let device = ink_platform::current().device_name();
    rows.push(
        Row::item(format!("Exit to {device}"), "exit").icon(Icon::Home).subtitle(format!("Close rinky, start the {device} home screen")),
    );
    rows
}

/// The Light page's rows: brightness and warmth sliders (actions `bright:N`, `warm:N`,
/// written on release) and a note.
pub fn light_rows(q: &QuickLight) -> Vec<Row> {
    let mut rows = vec![Row::header("Frontlight")];
    for s in [&q.brightness, &q.warmth].into_iter().flatten() {
        // Same values, the page's own action names.
        let action = if s.action == quick_light::BRIGHTNESS { "bright" } else { "warm" };
        rows.push(Row::slider(s.label.clone(), s.min, s.max, s.step, s.value, action));
    }
    rows.push(Row::note(if rows.len() > 1 {
        "Swipe down from the top edge of any screen for the control center."
    } else {
        "The frontlight is not available on this device."
    }));
    rows
}

/// The Plugins section of the main list: every plugin with its switch (`(info,
/// wanted)`), followed by its settings pages (`(label, plugin)`, action `ppage:<index>`).
pub fn plugin_section(pages: &[(String, String)], plugins: &[PluginChoice]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (p, wanted) in plugins {
        let name = &p.manifest.name;
        let row = Row::toggle(plugin_title(name), *wanted, format!("ptoggle:{name}")).icon(Icon::Grid);
        let summary = plugin_summary(p, *wanted);
        rows.push(if summary.is_empty() { row } else { row.subtitle(summary) });
        for (i, (label, _)) in pages.iter().enumerate().filter(|(_, (_, pl))| pl == name) {
            rows.push(Row::nav(Icon::Settings, label.clone(), format!("ppage:{i}")));
        }
    }
    // Pages of plugins that are not listed (should not happen; kept reachable).
    for (i, (label, plugin)) in pages.iter().enumerate() {
        if !plugins.iter().any(|(p, _)| &p.manifest.name == plugin) {
            rows.push(Row::nav(Icon::Settings, label.clone(), format!("ppage:{i}")).value(plugin.clone()));
        }
    }
    if plugins.is_empty() {
        rows.push(Row::note("No plugins installed"));
    } else {
        rows.push(Row::note("Switching a plugin takes effect at the next start."));
    }
    rows
}

/// Display name of a plugin (`insightful` -> `Insightful`, `kosync` -> `Sync`).
pub fn plugin_title(name: &str) -> String {
    match name {
        "kosync" => "Sync (KOSync)".into(),
        "opds" => "OPDS catalogs".into(),
        "stats" => "Statistics".into(),
        n => {
            let mut c = n.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        }
    }
}

/// The Wi-Fi page: switches, saved networks, then the networks found (signal bars, a
/// lock on secured ones, a check on the connected one).
pub fn wifi_rows(state: &NetStatus, airplane: bool, networks: &[WifiNetwork]) -> Vec<Row> {
    let line = wifi_line(state);
    let mut rows = vec![
        Row::header("Connection"),
        Row::toggle("Wi-Fi", state.radio_on(), "wifi").icon(Icon::Wifi).subtitle(line),
        Row::toggle("Airplane mode", airplane, "airplane").icon(Icon::Globe),
        Row::nav(Icon::Bookmark, "Saved networks", "page:saved"),
        Row::header("Networks"),
    ];
    if networks.is_empty() {
        rows.push(Row::note(if state.radio_on() { "No networks listed. Tap Scan." } else { "Turn Wi-Fi on to see networks." }));
    }
    let joining = match state {
        NetStatus::Connecting { essid } => essid.as_deref(),
        _ => None,
    };
    for (i, n) in networks.iter().enumerate() {
        let connected = state.connected_to() == Some(n.essid.as_str());
        let mut row = Row::choice(n.essid.clone(), connected, format!("net:{i}")).lead(Lead::Signal(n.signal)).lock(n.secured);
        if connected {
            row = row.subtitle("Connected");
        } else if joining == Some(n.essid.as_str()) {
            row = row.subtitle("Connecting…");
        } else if n.known {
            row = row.subtitle("Saved");
        }
        rows.push(row);
    }
    rows
}

/// What the network is doing, in one line (the Wi-Fi row, the Wi-Fi page).
pub fn wifi_line(state: &NetStatus) -> String {
    match state {
        NetStatus::Connected { essid, signal: Some(s) } => format!("Connected to {essid} (signal {s}/5)"),
        NetStatus::Connected { essid, signal: None } => format!("Connected to {essid}"),
        NetStatus::Connecting { essid: Some(e) } => format!("Connecting to {e}…"),
        NetStatus::Connecting { essid: None } => "Looking for saved networks…".into(),
        NetStatus::NoNetwork => "Not connected".into(),
        NetStatus::Off => "Off".into(),
    }
}

/// The Plugins page: plugin settings pages (`(label, plugin)`), the plugins with their
/// on/off switch (`(info, wanted)`), and the notifications row.
pub fn plugin_rows(pages: &[(String, String)], plugins: &[PluginChoice], notifications: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    if !pages.is_empty() {
        rows.push(Row::header("Plugin settings"));
        rows.extend(pages.iter().enumerate().map(|(i, (label, plugin))| Row::nav(Icon::Settings, label.clone(), format!("ppage:{i}")).value(plugin.clone())));
    }
    rows.push(Row::header("Installed plugins"));
    for (p, wanted) in plugins {
        let name = &p.manifest.name;
        rows.push(Row::toggle(format!("{name} {}", p.manifest.version), *wanted, format!("ptoggle:{name}")).subtitle(plugin_detail(p, *wanted)));
    }
    if plugins.is_empty() {
        rows.push(Row::note("No plugins installed"));
    } else {
        rows.push(Row::note("Turning a plugin on or off takes effect at the next start."));
    }
    rows.push(Row::header("Messages"));
    rows.push(Row::nav(Icon::Chat, "Notifications", "page:notifications").value(notifications));
    rows
}

/// A page of the settings screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Page {
    /// The list of pages.
    #[default]
    Main,
    /// Brightness and warmth.
    Light,
    /// Auto-sleep timeout.
    Sleep,
    /// Screensaver choice.
    Screensaver,
    /// Wi-Fi switch and networks.
    Wifi,
    /// Password entry for a network.
    Join(String),
    /// Saved Wi-Fi networks.
    Saved,
    /// Orientation.
    Display,
    /// Default text size, line spacing, margins and justification.
    Layout,
    /// Bottom navigation bar items of the home and library screens.
    Nav,
    /// Folders scanned for books.
    Folders,
    /// KOReader import and library rescan.
    Koreader,
    /// UI language.
    Language,
    /// A page from [`PAGE_MODULES`] (its key).
    Module(String),
    /// Full refresh every N pages.
    Refresh,
    /// Model, battery, date and time.
    Device,
    /// In-app update.
    Update,
    /// Plugin settings pages, plugins on/off, notifications.
    Plugins,
    /// Plugin notifications.
    Notifications,
    /// Reading and interface fonts ([`fonts`], aa builder).
    Fonts,
    /// Reader status bars.
    StatusBars(status_bars::View),
    /// Reader gestures list (`None`) or the action picker of one trigger.
    Gestures(Option<String>),
    /// A plugin's settings page `(plugin, page id)`: the plugin's settings menu shown
    /// as rows of a full page ([`PluginPage`]).
    Plugin(String, String),
}

impl Page {
    fn title(&self) -> String {
        match self {
            Page::Main => "Settings".into(),
            Page::Light => "Light".into(),
            Page::Sleep => "Sleep".into(),
            Page::Screensaver => "Screensaver".into(),
            Page::Wifi => "Wi-Fi".into(),
            Page::Join(essid) => format!("Join {essid}"),
            Page::Saved => "Saved networks".into(),
            Page::Display => "Orientation".into(),
            Page::Layout => "Default layout".into(),
            Page::Nav => "Navigation bar".into(),
            Page::Folders => "Library folders".into(),
            Page::Koreader => "KOReader import".into(),
            Page::Language => "Language".into(),
            Page::Module(key) => page_info(key).map_or_else(|| key.clone(), |p| p.title.to_string()),
            Page::Refresh => "Full refresh".into(),
            Page::Device => "Device info".into(),
            Page::Update => "Update".into(),
            Page::Plugins => "Plugins".into(),
            Page::Notifications => "Notifications".into(),
            Page::Fonts => fonts::TITLE.into(),
            Page::StatusBars(v) => status_bars::title(v),
            Page::Gestures(p) => gestures::title(p.as_deref()),
            Page::Plugin(plugin, _) => plugin_title(plugin),
        }
    }

    fn parent(&self) -> Option<Page> {
        match self {
            Page::Main => None,
            Page::Join(_) | Page::Saved => Some(Page::Wifi),
            Page::Notifications => Some(Page::Plugins),
            Page::StatusBars(v) => Some(status_bars::parent(v).map_or(Page::Main, Page::StatusBars)),
            Page::Gestures(Some(_)) => Some(Page::Gestures(None)),
            _ => Some(Page::Main),
        }
    }

    fn from_name(name: &str) -> Option<Page> {
        Some(match name {
            "main" => Page::Main,
            "light" => Page::Light,
            "sleep" => Page::Sleep,
            "screensaver" => Page::Screensaver,
            "wifi" => Page::Wifi,
            "saved" => Page::Saved,
            "display" => Page::Display,
            "layout" => Page::Layout,
            "nav" => Page::Nav,
            "folders" => Page::Folders,
            "koreader" => Page::Koreader,
            "language" => Page::Language,
            "refresh" => Page::Refresh,
            "device" => Page::Device,
            "update" => Page::Update,
            "plugins" => Page::Plugins,
            "notifications" => Page::Notifications,
            // aa: the Fonts page is a Page variant; its registry row ("page:mod:fonts") opens it.
            fonts::PAGE | "mod:fonts" => Page::Fonts,
            // statusbar: the Status bars page is a Page variant; its registry row opens it.
            "status_bars" | "mod:status_bars" => Page::StatusBars(status_bars::View::default()),
            // gestures: the Gestures page is a Page variant; its registry row opens it.
            "gestures" | "mod:gestures" => Page::Gestures(None),
            // ctrlpanel: the password page of one network (the control center's Wi-Fi picker).
            other if other.starts_with("join:") && other.len() > "join:".len() => Page::Join(other["join:".len()..].to_string()),
            other => match other.strip_prefix("mod:") {
                Some(key) if page_info(key).is_some() => Page::Module(key.to_string()),
                _ => return None,
            },
        })
    }
}

/// A plugin menu: request id, title, `(label, action)` items.
pub type PluginMenu = (u32, String, Vec<(String, String)>);

/// A plugin settings page ([`Page::Plugin`]). Opening it calls the plugin's
/// `on_action("settings:<id>")` with the plugin's menus claimed
/// ([`crate::plugin_host::PluginHost::claim_menus`]): the menu the plugin answers with
/// is not drawn as a popup over the settings but shown as this page's rows
/// ([`popup::menu_rows`]: switches, choices, values, notes). A tap answers the menu with
/// the item's action; the plugin's next menu (its new state) replaces the rows, and a
/// label change repaints one row. Back answers `back` when the menu has a back item (the
/// plugin's previous menu shows on the same page), else cancels it and leaves the page.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginPage {
    /// The settings page entry.
    pub entry: PluginEntry,
    /// The menu shown: request id, title, `(label, action)` items.
    pub menu: Option<PluginMenu>,
    /// The menu waits for an answer (a tap answered it and no new menu came: its rows
    /// stay, Back does not cancel it again).
    pub open: bool,
}

impl PluginPage {
    /// App bar title: the menu's title, else the page's.
    pub fn title(&self) -> String {
        match &self.menu {
            Some((_, t, _)) if !t.is_empty() => t.clone(),
            _ => self.entry.label.clone(),
        }
    }

    /// True when the menu has a `back` item (Back answers it).
    pub fn has_back(&self) -> bool {
        self.menu.as_ref().is_some_and(|(_, _, items)| items.iter().any(|(_, a)| a == "back"))
    }

    /// The page's rows: the menu's items without the back item (the app bar's back
    /// button does it).
    pub fn rows(&self) -> Vec<Row> {
        match &self.menu {
            Some((_, _, items)) => {
                let rows: Vec<Row> = popup::menu_rows(items).into_iter().zip(items).filter(|(_, (_, a))| a != "back").map(|(r, _)| r).collect();
                if rows.is_empty() {
                    vec![Row::note("This plugin has no settings to show.")]
                } else {
                    rows
                }
            }
            None => vec![Row::note(format!("No settings from {} yet.", plugin_title(&self.entry.plugin)))],
        }
    }
}

/// What the open confirmation dialog does on OK.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Confirm {
    Exit,
    Forget(String),
}

/// Result of an update job.
enum UpdateJob {
    Check(update::Result<(update::Release, bool)>),
    Install(update::Release, update::Result<String>),
}

/// The settings screen.
#[derive(Default)]
pub struct SettingsScreen {
    tree: WidgetTree,
    page: Page,
    size: Size,
    /// Rebuild the page at the next paint with this class.
    rebuild: Option<RefreshClass>,
    status: Option<WidgetId>,
    status_text: String,
    /// The status text last written to the log.
    logged_status: String,
    dialog: Option<([WidgetId; 2], Confirm)>,
    /// The next paint repaints widgets that did not change: cut its damage to the changed
    /// pixels (a closed dialog shows the list under it again).
    cut_damage: bool,
    keyboard: Option<WidgetId>,
    input: Option<WidgetId>,
    networks: Vec<WifiNetwork>,
    /// A join started here runs in the background (the network's events end it).
    pending_join: Option<String>,
    /// A scan started here runs in the background.
    scanning: bool,
    release: Option<update::Release>,
    /// An update was installed and the launcher runs it when rinky exits: the next timer
    /// ends rinky (after the status line said so). The installed version.
    restarting: Option<String>,
    /// Update check or install running on a worker thread.
    job: Option<Job<UpdateJob>>,
    /// Plugin settings pages listed on the Plugins page (`ppage:<index>`).
    plugin_pages: Vec<PluginEntry>,
    /// The page's list of rows.
    list: Option<WidgetId>,
    /// The scroll offset to show after a rebuild of the same page.
    keep_scroll: i32,
    /// The pages left for a sub-page, outermost first: each with its scroll offset and
    /// the action of the row that opened the sub-page. Back (or a change that returns to
    /// one of them) shows it at that offset again, with that row on screen.
    trail: Vec<(Page, i32, Option<String>)>,
    /// The scroll offset of the next page built after [`SettingsScreen::go`].
    next_scroll: i32,
    /// A row (by action) to bring on screen at the next build.
    reveal: Option<String>,
    /// The action being handled (the row that may open a sub-page).
    last_action: Option<String>,
    /// The module of a [`Page::Module`] page.
    module: Option<Box<dyn PageModule>>,
    /// Folders listed on the library folders page (`dir:<index>`).
    folders: Vec<std::path::PathBuf>,
    /// The page and size the tree was last built for (a GL16 rebuild of the same page
    /// refreshes only the pixel rows that changed, e.g. one toggled row).
    built: Option<(Page, Size)>,
    /// The page the screen was opened on by [`open_at`]; Back from it leaves the screen
    /// (to the reader, the control center's screen below) instead of going to the main list.
    start: Option<Page>,
    /// The plugin settings page shown ([`Page::Plugin`]).
    plugin_page: Option<PluginPage>,
}

/// The rows of the Default layout page for the reader defaults `r`.
fn layout_rows(r: &ink_store::settings::ReaderSettings) -> Vec<Row> {
    let mut rows = vec![Row::header("Text size")];
    rows.extend(FONT_SIZES.iter().map(|px| Row::choice(format!("{px} px"), (r.font_px - px).abs() < 0.5, format!("font-px:{px}"))));
    rows.push(Row::header("Line spacing"));
    rows.extend(LINE_SPACINGS.iter().map(|ls| Row::choice(format!("{ls:.1}"), (r.line_spacing - ls).abs() < 0.05, format!("spacing:{ls}"))));
    rows.push(Row::header("Side margins"));
    rows.extend(MARGINS.iter().map(|(px, name)| Row::choice(*name, (r.margin_x - px).abs() < 0.5, format!("margin:{px}"))));
    rows.push(Row::header("Space between paragraphs"));
    rows.extend(PARA_SPACINGS.iter().map(|(em, name)| Row::choice(*name, (r.paragraph_spacing - em).abs() < 0.01, format!("pspace:{em}"))));
    rows.push(Row::header("First-line indent"));
    rows.extend(INDENTS.iter().map(|(em, name)| Row::choice(*name, (r.paragraph_indent - em).abs() < 0.01, format!("indent:{em}"))));
    rows.push(Row::header("Text weight"));
    rows.extend(WEIGHTS.iter().map(|(lv, name)| Row::choice(*name, r.font_weight == *lv, format!("weight:{lv}"))));
    rows.push(Row::header("Text"));
    rows.push(Row::toggle("Justify", r.justify, "justify").icon(Icon::ListView));
    rows.push(Row::toggle("Hyphenate", r.hyphenate, "hyph").icon(Icon::ListView));
    rows.push(Row::toggle("Use the book's own styles", r.publisher_styles, "publisher").icon(Icon::Book));
    rows.push(Row::toggle("Ruby annotations", r.ruby, "ruby").icon(Icon::Book));
    rows.push(Row::note("Used for books opened from now on; Aa in a book changes that book."));
    rows
}

impl SettingsScreen {
    /// The page shown.
    pub fn page(&self) -> &Page {
        &self.page
    }

    fn go(&mut self, page: Page, cx: &mut AppCtx) {
        if page == Page::Wifi {
            self.networks = cx.services.wifi.scan_list().unwrap_or_default();
        }
        if matches!(self.page, Page::Plugin(..)) && page != self.page {
            self.leave_plugin_page(cx);
        }
        log::info!("settings: open page {page:?} (from {:?})", self.page);
        // Remember where this page was, so going back to it shows the place the user left.
        let from = self.page.clone();
        let scroll = self.list.and_then(|id| self.tree.get_mut::<SettingsList>(id)).map_or(self.next_scroll, |l| l.scroll);
        self.trail.retain(|(p, _, _)| *p != from);
        self.trail.push((from, scroll, self.last_action.take()));
        match self.trail.iter().position(|(p, _, _)| *p == page) {
            Some(i) => {
                let (_, scroll, opener) = self.trail[i].clone();
                self.trail.truncate(i);
                log::info!("settings: back to {page:?} at offset {scroll} (row {opener:?})");
                (self.next_scroll, self.reveal) = (scroll, opener);
            }
            None => {
                // A new page starts at the top; only its ancestors stay remembered.
                let mut up = Vec::new();
                let mut p = page.parent();
                while let Some(q) = p {
                    p = q.parent();
                    up.push(q);
                }
                self.trail.retain(|(p, _, _)| up.contains(p));
                (self.next_scroll, self.reveal) = (0, None);
            }
        }
        self.module = match &page {
            Page::Module(key) => page_info(key).map(make_module),
            _ => None,
        };
        if page == Page::Folders {
            self.folders = folder_choices(&current_settings(cx).library_dirs, std::path::Path::new(ink_platform::current().user_root()));
        }
        self.page = page;
        self.list = None;
        self.status_text.clear();
        self.rebuild = Some(RefreshClass::Gl16);
    }

    /// [`Self::go`] with the status line kept (the result of what led there).
    fn go_keep_status(&mut self, page: Page, cx: &mut AppCtx) {
        let text = std::mem::take(&mut self.status_text);
        self.go(page, cx);
        self.status_text = text;
    }

    fn set_status(&mut self, text: impl Into<String>) {
        self.set_status_class(text, RefreshClass::Gl16);
    }

    /// Builds the page again (the next paint) with `text` in the status line.
    fn rebuild_with(&mut self, text: impl Into<String>) {
        self.status_text = text.into();
        self.rebuild = Some(RefreshClass::Gl16);
    }

    fn set_status_class(&mut self, text: impl Into<String>, class: RefreshClass) {
        self.status_text = text.into();
        self.log_status();
        if let Some(id) = self.status {
            if let Some(l) = self.tree.get_mut::<Label>(id) {
                l.text = self.status_text.clone();
            }
            self.tree.mark_dirty(id, class);
        }
    }

    fn log_status(&mut self) {
        if !self.status_text.is_empty() && self.status_text != self.logged_status {
            log::info!("settings status: {}", self.status_text);
            self.logged_status = self.status_text.clone();
        }
    }

    /// Shows a status line at once (before a call that blocks).
    fn say_now(&mut self, text: &str, cx: &mut AppCtx) -> anyhow::Result<()> {
        self.set_status(text);
        for d in self.tree.paint(&mut paint_cx(cx)) {
            cx.show(d.rect, d.class)?;
        }
        Ok(())
    }

    /// Adds a list of `rows` in the content area, at the scroll offset the previous
    /// list of this page had (kept across a rebuild, reset by [`SettingsScreen::go`]).
    fn add_rows(&mut self, rows: Vec<Row>, area: Rect) {
        let reveal = self.reveal.take();
        self.list = Some(page::add_list(&mut self.tree, rows, area, self.keep_scroll, reveal.as_deref()));
    }

    fn build(&mut self, cx: &mut AppCtx, class: RefreshClass) {
        let size = cx.screen.size();
        self.size = size;
        self.log_status();
        // The offset the list had, for a rebuild of the same page; after a page change
        // the offset that page had when it was left (0 for a new page).
        self.keep_scroll = self.list.and_then(|id| self.tree.get_mut::<SettingsList>(id)).map_or(self.next_scroll, |l| l.scroll);
        let started = std::time::Instant::now();
        self.tree = WidgetTree::new();
        (self.status, self.dialog, self.keyboard, self.input, self.list) = (None, None, None, None, None);
        cx.canvas.clear(cx.theme.bg);
        let title = match &self.plugin_page {
            Some(p) if matches!(self.page, Page::Plugin(..)) => p.title(),
            _ => self.module.as_ref().and_then(|m| m.title()).unwrap_or_else(|| self.page.title()),
        };
        let actions = match self.page {
            Page::Wifi if cx.services.wifi.state().is_ok() => vec![BarAction::text("Scan", "scan")],
            Page::Device => vec![BarAction::icon(Icon::Sync, "page:device")],
            _ => Vec::new(),
        };
        page::add_bar(&mut self.tree, size, title, actions);
        let content = content_rect(size);
        match self.page.clone() {
            Page::Main => self.build_main(cx, content),
            Page::Light => self.build_light(cx, content),
            Page::Sleep => self.build_sleep(cx, content),
            Page::Screensaver => self.build_screensaver(cx, content),
            Page::Wifi => self.build_wifi(cx, content),
            Page::Join(essid) => {
                self.build_join(&essid, size, keyboard_layouts::keyboard(current_settings(cx)));
                self.tree.invalidate(Rect::from_size(size), class);
                return;
            }
            Page::Saved => self.build_saved(cx, content),
            Page::Display => self.build_display(cx, content),
            Page::Layout => self.build_layout(cx, content),
            Page::Nav => self.build_nav(cx, content),
            Page::Folders => self.build_folders(cx, content),
            Page::Koreader => self.build_koreader(cx, content),
            Page::Language => self.build_language(cx, content),
            Page::Module(_) => self.build_module(cx, content),
            Page::Refresh => self.build_refresh(cx, content),
            Page::Device => self.build_device(cx, content),
            Page::Update => self.build_update(content),
            Page::Plugins => self.build_plugins(cx, content),
            Page::Notifications => self.build_notifications(cx, content),
            Page::Fonts => fonts::build(&mut self.tree, cx, content),
            Page::StatusBars(v) => self.build_status_bars(cx, content, &v),
            Page::Gestures(p) => {
                let plugins = crate::plugin_entries::gesture_choices(cx.plugins);
                self.add_rows(gestures::rows(&current_settings(cx).gestures, p.as_deref(), &plugins), content)
            }
            Page::Plugin(..) => {
                let rows = self.plugin_page.as_ref().map_or_else(|| vec![Row::note("Not available")], PluginPage::rows);
                self.add_rows(rows, content);
            }
        }
        self.status = Some(page::add_status(&mut self.tree, size, &self.status_text));
        self.tree.invalidate(Rect::from_size(size), class);
        log::info!("settings: page {:?} built in {:?} ({class:?})", self.page, started.elapsed());
    }

    fn build_main(&mut self, cx: &mut AppCtx, content: Rect) {
        let s = &cx.services;
        let light = match (s.light.brightness(), s.light.warmth()) {
            (Ok(b), Ok(w)) => format!("{b} · warmth {w}"),
            (Ok(b), Err(_)) => format!("{b}"),
            _ => "Not available".into(),
        };
        let airplane = s.wifi.airplane_mode().unwrap_or(false);
        let wifi = match s.wifi.status() {
            _ if airplane => "Airplane mode".into(),
            Ok(NetStatus::Connected { essid, .. }) => essid,
            Ok(NetStatus::Connecting { essid: Some(e) }) => format!("Connecting to {e}…"),
            Ok(NetStatus::Connecting { essid: None }) => "Connecting…".into(),
            Ok(NetStatus::NoNetwork) => "Not connected".into(),
            Ok(NetStatus::Off) => "Off".into(),
            Err(_) => "Not available".into(),
        };
        let device = match s.battery.state() {
            Ok(b) => format!("Battery {}%{}", b.percent, if b.charging { ", charging" } else { "" }),
            Err(_) => s.info.model.clone(),
        };
        let (pages, plugins) = self.plugin_lists(cx);
        let st = current_settings(cx);
        let r = &st.reader;
        let v = MainValues {
            refresh: refresh_name(r.flash_every),
            rotation: rotation_name(crate::app::settings_rotation(st)).into(),
            night: st.device.night_mode,
            wave: page_wave(st),
            wave_hidden: !cx.services.info.page_wave,
            layout: format!("{} px · {:.1}", r.font_px.round(), r.line_spacing),
            nav: crate::plugin_entries::nav_chosen(st, cx.plugins).iter().map(|n| crate::plugin_entries::nav_label(cx.plugins, n)).collect::<Vec<_>>().join(", "),
            folders: match st.library_dirs.len() {
                1 => "1 folder".into(),
                n => format!("{n} folders"),
            },
            light,
            wifi,
            airplane,
            sleep: sleep_short(st.device.sleep_minutes),
            screensaver: screensaver_name(&screensaver_setting(st)),
            language: language_name(&st.language),
            device,
            plugins: plugin_section(&pages, &plugins),
            notifications: notifications_summary(cx),
            version: update::current_version().to_string(),
            confirm_exit: st.ui.confirm_exit,
            modules: SECTIONS.iter().map(|sec| (*sec, module_rows(sec, st))).collect(),
        };
        self.add_rows(main_rows(&v), content);
    }

    /// The Light page: a slider row for the brightness and one for the warmth.
    fn build_light(&mut self, cx: &mut AppCtx, content: Rect) {
        self.add_rows(light_rows(&QuickLight::read(&*cx.services.light)), content);
    }

    fn build_sleep(&mut self, cx: &mut AppCtx, content: Rect) {
        let now = cx.settings.device.sleep_minutes;
        let mut rows = vec![Row::header("Go to sleep when not used")];
        rows.extend(SLEEP_CHOICES.iter().map(|m| Row::choice(sleep_name(*m), *m == now, format!("sleep:{m}"))));
        self.add_rows(rows, content);
    }

    fn build_screensaver(&mut self, cx: &mut AppCtx, content: Rect) {
        let now = screensaver_setting(cx.settings);
        let mut values: Vec<String> = ["page", "cover", "blank"].iter().map(|s| s.to_string()).collect();
        let dir = cx.paths.root.join("screensavers");
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut pngs: Vec<String> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")))
                .map(|p| format!("image:{}", p.display()))
                .collect();
            pngs.sort();
            values.extend(pngs.into_iter().take(50));
        }
        let mut rows = vec![Row::header(format!("Shown while the {} sleeps", ink_platform::current().device_name()))];
        rows.extend(values.iter().map(|v| Row::choice(screensaver_name(v), *v == now, format!("saver:{v}"))));
        rows.push(Row::note(format!("PNG pictures in {} are listed too.", dir.display())));
        self.add_rows(rows, content);
    }

    fn build_wifi(&mut self, cx: &mut AppCtx, content: Rect) {
        let wifi = &mut cx.services.wifi;
        let state = match wifi.status() {
            Ok(s) => s,
            Err(e) => {
                let text = format!("Wi-Fi is not available: {e}");
                let r = Rect::new(content.x + space(4), content.y + space(2), content.w - 2 * space(4) as u32, 200);
                self.tree.add(None, Box::new(TextBlock::new(text, 36.0)), r);
                return;
            }
        };
        let airplane = wifi.airplane_mode().unwrap_or(false);
        self.add_rows(wifi_rows(&state, airplane, &self.networks), content);
    }

    fn build_join(&mut self, essid: &str, size: Size, keyboard: Keyboard) {
        let w = size.w;
        let bar = AppBar::new(format!("Join {essid}")).with_back();
        self.tree.add(None, Box::new(bar), app_bar_rect(size));
        let mut input = TextInput::new("Password", "password");
        input.password = true;
        input.focused = true;
        let x = space(4);
        let iw = w - 2 * space(4) as u32;
        let mut label = Label::left(format!("Password for {essid}"), 36.0);
        label.centered = false;
        self.tree.add(None, Box::new(label), Rect::new(x, JOIN_TOP, iw, 70));
        self.input = Some(self.tree.add(None, Box::new(input), Rect::new(x, JOIN_TOP + 80, iw, 110)));
        let join = join_button_rect(size);
        self.tree.add(None, Box::new(Button::filled("Join", "join")), join);
        let mut status = Label::left(self.status_text.clone(), 32.0);
        status.centered = false;
        self.status = Some(self.tree.add(None, Box::new(status), Rect::new(x, join.y, (join.x - space(3) - x).max(0) as u32, join.h)));
        self.keyboard = Some(self.tree.add(None, Box::new(keyboard), Keyboard::bottom_rect(size)));
    }

    fn build_saved(&mut self, cx: &mut AppCtx, content: Rect) {
        let rows = match cx.services.wifi.profiles() {
            Ok(p) if !p.is_empty() => {
                let mut rows = vec![Row::header("Tap a network to forget it")];
                rows.extend(p.iter().map(|p| {
                    Row::item(p.essid.clone(), format!("forget:{}", p.essid))
                        .icon(Icon::Wifi)
                        .subtitle(if p.secured { "Secured" } else { "Open" })
                        .lock(p.secured)
                        .trail(Trail::Icon(Icon::Trash))
                }));
                rows
            }
            Ok(_) => vec![Row::note("No saved networks")],
            Err(e) => vec![Row::note(format!("Saved networks are not available: {e}"))],
        };
        self.add_rows(rows, content);
    }

    fn build_display(&mut self, cx: &mut AppCtx, content: Rect) {
        let now = crate::app::settings_rotation(cx.settings);
        let mut rows = vec![Row::header("Screen orientation")];
        rows.extend(Rotation::ALL.iter().enumerate().map(|(i, r)| {
            let icon = if matches!(r, Rotation::Portrait | Rotation::UpsideDown) { Icon::Book } else { Icon::Rotate };
            Row::choice(rotation_name(*r), *r == now, format!("rot:{i}")).icon(icon)
        }));
        self.add_rows(rows, content);
    }

    fn build_layout(&mut self, cx: &mut AppCtx, content: Rect) {
        let rows = layout_rows(&current_settings(cx).reader);
        self.add_rows(rows, content);
    }

    fn build_nav(&mut self, cx: &mut AppCtx, content: Rect) {
        let chosen = crate::plugin_entries::nav_chosen(current_settings(cx), cx.plugins);
        let extra = crate::plugin_entries::nav_choices(cx.plugins);
        let list = crate::plugin_entries::merged(ink_store::prefs::NAV_ITEMS, &extra);
        let mut rows = vec![order::order_note(NAV_MIN, NAV_MAX), Row::header("Bottom bar, left to right")];
        rows.extend(order::order_rows(&list, &chosen, "nav"));
        self.add_rows(rows, content);
    }

    fn build_folders(&mut self, cx: &mut AppCtx, content: Rect) {
        let chosen = &current_settings(cx).library_dirs;
        let mut rows = vec![Row::header("Folders with books")];
        rows.extend(self.folders.iter().enumerate().map(|(i, p)| {
            Row::toggle(path_tail(&p.display().to_string(), 48), chosen.contains(p), format!("dir:{i}")).icon(Icon::Folder)
        }));
        if self.folders.is_empty() {
            rows.push(Row::note("No folders"));
        }
        rows.push(Row::note("The library reads the folders again when you go back to it."));
        self.add_rows(rows, content);
    }

    fn build_koreader(&mut self, cx: &mut AppCtx, content: Rect) {
        let rows = koreader_rows(current_settings(cx).export_format());
        self.add_rows(rows, content);
    }

    fn build_language(&mut self, cx: &mut AppCtx, content: Rect) {
        let now = current_settings(cx).language.clone();
        let mut rows = vec![Row::header("Language of the screens")];
        rows.extend(LANGUAGES.iter().map(|(code, name)| Row::choice(*name, now == *code, format!("lang:{code}"))));
        rows.push(Row::note("Screens are in English where a translation is missing."));
        self.add_rows(rows, content);
    }

    fn build_module(&mut self, cx: &mut AppCtx, content: Rect) {
        let rows = match self.module.as_mut() {
            Some(m) => m.rows(cx),
            None => vec![Row::note("Coming soon")],
        };
        self.add_rows(rows, content);
    }

    fn build_refresh(&mut self, cx: &mut AppCtx, content: Rect) {
        let now = cx.settings.reader.flash_every;
        let mut rows = vec![Row::header("Clear ghosting with a full refresh")];
        rows.extend(REFRESH_CHOICES.iter().map(|n| Row::choice(refresh_name(*n), *n == now, format!("refresh:{n}"))));
        // App::take_settings_update gives the scheduler the new value at once.
        rows.push(Row::note("Applies at once, also to the open book."));
        self.add_rows(rows, content);
    }

    fn build_device(&mut self, cx: &mut AppCtx, content: Rect) {
        let s = &cx.services;
        let device = Card::new(s.info.model.clone(), "")
            .subtitle(if s.info.firmware.is_empty() { format!("Backend: {}", s.info.backend) } else { s.info.firmware.clone() })
            .body(format!("rinky {}", update::current_version()));
        let mut cards = vec![(device, DEVICE_CARD_H)];
        let battery = match s.battery.info() {
            Ok(b) => {
                let mut sub = if b.charging {
                    "Charging".to_string()
                } else if b.status.is_empty() {
                    "On battery".to_string()
                } else {
                    b.status.clone()
                };
                if let Ok(c) = s.usb.cable() {
                    sub.push_str(&format!(" · USB {c:?}"));
                }
                let mut details = Vec::new();
                if let Some(t) = b.temperature_c {
                    details.push(format!("{t:.0} °C"));
                }
                if let Some(v) = b.voltage_mv {
                    details.push(format!("{v} mV"));
                }
                // A tap opens the Battery page (use per hour, time left).
                let mut c = Card::new(format!("Battery {}%", b.percent), "page:mod:battery").subtitle(sub).progress(b.percent as f32 / 100.0);
                if !details.is_empty() {
                    c = c.body(details.join(" · "));
                }
                c
            }
            Err(e) => Card::new("Battery", "").subtitle(e.to_string()),
        };
        cards.push((battery, DEVICE_CARD_H));
        let clock = Card::new(s.clock.local_now().format(), "").subtitle("Date and time").kind(CardKind::Filled);
        cards.push((clock, CLOCK_CARD_H));
        let files = Card::new("Files", "").subtitle("Settings file").body(path_tail(&cx.paths.settings().display().to_string(), 60));
        cards.push((files, DEVICE_CARD_H));
        let (rects, button) = device_layout(content, cards.iter().map(|(_, h)| *h).collect());
        for ((c, _), r) in cards.into_iter().zip(rects) {
            self.tree.add(None, Box::new(c), r);
        }
        self.tree.add(None, Box::new(Button::filled("Set time (NTP)", "ntp")), button);
    }

    /// Reads the plugin settings pages (kept for `ppage:<index>`) as `(label, plugin)`
    /// and every plugin with the user's choice (`(info, wanted)`).
    fn plugin_lists(&mut self, cx: &mut AppCtx) -> (Vec<(String, String)>, Vec<PluginChoice>) {
        self.plugin_pages = hooks::settings_pages(cx);
        let off = plugin_host::user_disabled(cx.paths);
        let pages = self.plugin_pages.iter().map(|e| (e.label.clone(), e.plugin.clone())).collect();
        let plugins = cx.plugins.plugins().into_iter().map(|p| { let wanted = !off.contains(&p.manifest.name); (p, wanted) }).collect();
        (pages, plugins)
    }

    fn build_plugins(&mut self, cx: &mut AppCtx, content: Rect) {
        let (pages, plugins) = self.plugin_lists(cx);
        let rows = plugin_rows(&pages, &plugins, &notifications_summary(cx));
        self.add_rows(rows, content);
    }

    fn build_notifications(&mut self, cx: &mut AppCtx, content: Rect) {
        let notes = cx.plugins.notifications();
        let rows = if notes.is_empty() {
            vec![Row::note("No notifications")]
        } else {
            // Newest first; unread ones with a star.
            notes
                .iter()
                .rev()
                .map(|n| {
                    let lead = if n.read { Lead::Icon(Icon::Chat) } else { Lead::Icon(Icon::Star) };
                    Row::item(n.text.clone(), "").lead(lead).subtitle(n.plugin.clone())
                })
                .collect()
        };
        self.add_rows(rows, content);
        cx.plugins.mark_notifications_read();
    }

    fn build_update(&mut self, content: Rect) {
        let x = content.x + space(4);
        let w = content.w - 2 * space(4) as u32;
        let card = Card::new(format!("rinky {}", update::current_version()), "").subtitle("Installed version").body(
            "The check asks the release server for a newer package. Install downloads it, verifies its SHA-256, installs it and restarts rinky into it. Wi-Fi must be on.",
        );
        let y = content.y + space(2);
        self.tree.add(None, Box::new(card), Rect::new(x, y, w, UPDATE_CARD_H));
        let y = y + UPDATE_CARD_H as i32 + space(4);
        match &self.release {
            Some(r) => {
                self.tree.add(None, Box::new(Button::new("Check again", "update-check")), Rect::new(x, y, w, 104));
                let label = format!("Install {} and restart", r.version);
                self.tree.add(None, Box::new(Button::filled(label, "update-install")), Rect::new(x, y + 104 + space(3), w, 104));
            }
            None => {
                self.tree.add(None, Box::new(Button::filled("Check for update", "update-check")), Rect::new(x, y, w, 104));
            }
        }
    }

    /// Opens the settings page of plugin entry `e` ([`PluginPage`]).
    fn open_plugin_page(&mut self, e: PluginEntry, cx: &mut AppCtx) {
        self.go(Page::Plugin(e.plugin.clone(), e.id.clone()), cx);
        cx.plugins.claim_menus(Some(&e.plugin));
        self.plugin_page = Some(PluginPage { entry: e.clone(), menu: None, open: false });
        if !hooks::run_entry(cx, &e, None, None) {
            self.status_text = format!("{} did not open its page", plugin_title(&e.plugin));
        }
        if !self.take_plugin_menus(cx) {
            log::info!("settings: {} answered settings:{} without a menu", e.plugin, e.id);
        }
    }

    /// Takes the plugin page's new menus and label changes; true when a new menu came
    /// (the page is built again; only the rows that changed are refreshed).
    fn take_plugin_menus(&mut self, cx: &mut AppCtx) -> bool {
        let Some(p) = self.plugin_page.as_mut() else { return false };
        let mut new_menu = false;
        let mut changed = Vec::new();
        for req in cx.plugins.take_claimed_menus() {
            match req {
                UiRequest::Menu { id, title, items } => {
                    log::info!("settings: plugin page {}: menu {id} {title:?} ({} items)", p.entry.plugin, items.len());
                    p.menu = Some((id, title, items));
                    p.open = true;
                    new_menu = true;
                    changed.clear();
                }
                UiRequest::MenuLabel { id, action, label } => {
                    let Some((mid, _, items)) = p.menu.as_mut() else { continue };
                    if *mid != id {
                        continue;
                    }
                    if let Some(i) = items.iter().position(|(_, a)| *a == action) {
                        items[i].0 = label;
                        changed.push(i);
                    }
                }
                _ => {}
            }
        }
        if new_menu {
            self.rebuild = Some(RefreshClass::Gl16);
        } else if let (Some((_, _, items)), Some(list)) = (p.menu.as_ref(), self.list) {
            let choices = items.iter().any(|(l, _)| ink_ui::dialog::split_check(l).0);
            let rows: Vec<(usize, Row)> = changed.into_iter().map(|i| (i, popup::menu_row(i, &items[i].0, &items[i].1, choices))).collect();
            if let Some(l) = self.tree.get_mut::<SettingsList>(list) {
                let mut any = false;
                for (i, row) in rows {
                    // The rows of the page are the menu's rows without the back item.
                    if let Some(k) = l.rows.iter().position(|r| popup::menu_index(&r.action) == Some(i)) {
                        any |= l.replace_row(k, row);
                    }
                }
                if any {
                    self.tree.mark_changed(list, RefreshClass::Gl16);
                }
            }
        }
        new_menu
    }

    /// A row of the plugin page (`menu:<i>`): answers the menu with the item's action.
    fn plugin_pick(&mut self, a: &str, cx: &mut AppCtx) {
        let Some(p) = self.plugin_page.as_ref() else { return };
        let Some((id, _, items)) = p.menu.as_ref() else { return };
        let Some(pick) = popup::menu_pick(a, items) else { return };
        let (plugin, id) = (p.entry.plugin.clone(), *id);
        log::info!("settings: plugin page {plugin}: {pick}");
        if let Some(p) = self.plugin_page.as_mut() {
            p.open = false;
        }
        if !cx.plugins.answer(&plugin, id, &pick, None) {
            self.set_status(format!("{} failed", plugin_title(&plugin)));
        }
        self.take_plugin_menus(cx);
    }

    /// Back on a plugin page: answers `back` (the menu has a back item) or cancels the
    /// menu. True when the plugin showed another menu, which stays on the page.
    fn plugin_back(&mut self, cx: &mut AppCtx) -> bool {
        let Some(p) = self.plugin_page.as_mut() else { return false };
        let Some((id, _, _)) = p.menu.as_ref().filter(|_| p.open) else { return false };
        let back = p.has_back();
        let (plugin, id) = (p.entry.plugin.clone(), *id);
        cx.plugins.answer(&plugin, id, if back { "back" } else { "" }, None);
        p.menu = None;
        p.open = false;
        // Another menu after a cancel stays only when it is a different one (no trap).
        if self.take_plugin_menus(cx) {
            let again = self.plugin_page.as_ref().and_then(|p| p.menu.as_ref()).is_some_and(|(nid, _, _)| back || *nid != id);
            if again {
                log::info!("settings: plugin page {plugin}: back to the plugin's previous menu");
                return true;
            }
            if let Some(p) = self.plugin_page.as_mut() {
                p.menu = None;
            }
        }
        false
    }

    /// Leaves the plugin page: cancels its menu and gives the plugin's menus back to the
    /// overlay.
    fn leave_plugin_page(&mut self, cx: &mut AppCtx) {
        let Some(p) = self.plugin_page.take() else { return };
        if let Some((id, _, _)) = p.menu.filter(|_| p.open) {
            cx.plugins.answer(&p.entry.plugin, id, "", None);
        }
        if cx.plugins.claimed_menus() == Some(p.entry.plugin.as_str()) {
            cx.plugins.claim_menus(None);
        }
    }

    fn confirm(&mut self, what: Confirm) {
        let (title, body, ok) = match &what {
            Confirm::Exit => {
                let device = ink_platform::current().device_name();
                (format!("Exit to {device}?"), format!("rinky closes and the {device} home screen starts."), "Exit")
            }
            Confirm::Forget(e) => {
                (format!("Forget {e}?"), format!("The saved password is removed from the {}.", ink_platform::current().device_name()), "Forget")
            }
        };
        let scrim = self.tree.add(None, Box::new(Scrim::new("")), Rect::from_size(self.size));
        let dialog = Dialog { title, body, buttons: vec![("Cancel".into(), "dlg-cancel".into()), (ok.into(), "dlg-ok".into())] };
        let id = self.tree.add(None, Box::new(dialog), Dialog::frame(self.size));
        self.tree.mark_dirty(id, RefreshClass::Gl16);
        self.dialog = Some(([scrim, id], what));
    }

    /// Removes the confirmation dialog and its scrim without building the page again:
    /// the tree repaints the area the dialog covered (one update of the dialog's rect).
    fn close_dialog(&mut self) {
        if let Some(([scrim, id], _)) = self.dialog.take() {
            self.tree.remove(id);
            self.tree.remove(scrim);
            self.cut_damage = true;
        }
    }

    fn close_overlay(&mut self) {
        self.rebuild = Some(RefreshClass::Gl16);
    }

    fn back(&mut self, cx: &mut AppCtx) -> Transition {
        if self.dialog.is_some() {
            self.close_dialog();
            return Transition::None;
        }
        if matches!(self.page, Page::Plugin(..)) && self.plugin_back(cx) {
            return Transition::None;
        }
        if self.module.as_mut().is_some_and(|m| m.back()) {
            log::info!("settings: back inside page {:?}", self.page);
            self.rebuild = Some(RefreshClass::Gl16);
            return Transition::None;
        }
        // A page opened directly by `open_at` goes back to the screen that opened it.
        if self.trail.is_empty() && self.start.as_ref() == Some(&self.page) {
            log::info!("settings: back from the direct start page {:?} to the screen below", self.page);
            return Transition::Pop;
        }
        // Back to the page this one was opened from (Notifications opened from the main
        // list goes back there, not to Plugins), else to the parent page.
        match self.trail.last().map(|(p, _, _)| p.clone()).or_else(|| self.page.parent()) {
            Some(p) => {
                self.go(p, cx);
                Transition::None
            }
            None => Transition::Pop,
        }
    }

    /// Joins `essid`: in the background when the device can (the page then follows the
    /// network's events), else waiting here. False when it failed at once.
    fn connect(&mut self, essid: &str, password: Option<&str>, cx: &mut AppCtx) -> anyhow::Result<bool> {
        self.say_now(&format!("Connecting to {essid}…"), cx)?;
        match cx.services.wifi.connect_in_background(essid, password) {
            Ok(true) => {
                self.pending_join = Some(essid.to_string());
                self.status_text = format!("Connecting to {essid}…");
                Ok(true)
            }
            Ok(false) => {
                self.status_text = format!("Connected to {essid}");
                Ok(true)
            }
            Err(e) => {
                self.set_status(format!("Cannot connect to {essid}: {e}"));
                Ok(false)
            }
        }
    }

    /// The network worker's news: the Wi-Fi rows show it, a join or scan started here
    /// ends.
    fn network_event(&mut self, e: &NetEvent, cx: &mut AppCtx) {
        let shows_wifi = matches!(self.page, Page::Main | Page::Wifi);
        match e {
            NetEvent::Status(st) => {
                if let (Some(j), Some(c)) = (&self.pending_join, st.connected_to()) {
                    if j == c {
                        self.status_text = format!("Connected to {c}");
                        self.pending_join = None;
                    }
                }
                if shows_wifi {
                    self.rebuild = Some(RefreshClass::Gl16);
                }
            }
            NetEvent::JoinFailed(essid) if self.pending_join.as_deref() == Some(essid.as_str()) => {
                self.pending_join = None;
                let text = format!("Couldn't connect to {essid}");
                let secured = self.networks.iter().any(|n| n.essid == *essid && n.secured);
                if secured && self.page == Page::Wifi {
                    // The saved password may be wrong (a failed join keeps the profile):
                    // ask for it again instead of retrying the saved one forever.
                    self.go(Page::Join(essid.clone()), cx);
                }
                self.set_status(text);
            }
            NetEvent::Scanned(r) => {
                if self.page == Page::Wifi {
                    self.networks = cx.services.wifi.scan_list().unwrap_or_default();
                    self.rebuild = Some(RefreshClass::Gl16);
                }
                if std::mem::take(&mut self.scanning) {
                    self.status_text = match r {
                        Ok(n) => format!("{n} network{} found", if *n == 1 { "" } else { "s" }),
                        Err(e) => format!("Scan failed: {e}"),
                    };
                    self.log_status();
                }
            }
            _ => {}
        }
    }

    fn action(&mut self, a: &str, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        log::info!("settings action: {a}");
        self.last_action = Some(a.to_string());
        if let Some(text) = fonts::action(a, cx) {
            // aa: the Fonts page's own actions.
            self.rebuild_with(text);
            return Ok(Transition::None);
        }
        if let Some(text) = quick_light::apply(a, cx.services.light) {
            self.set_status(text);
            return Ok(Transition::None);
        }
        if self.status_bars_action(a, cx) {
            return Ok(Transition::None);
        }
        if let Some((page, status)) = gestures::dispatch(a, cx) {
            self.go(Page::Gestures(page), cx);
            self.status_text = status;
            return Ok(Transition::None);
        }
        let (verb, arg) = a.split_once(':').unwrap_or((a, ""));
        if verb == popup::MENU_ROW && matches!(self.page, Page::Plugin(..)) {
            self.plugin_pick(a, cx);
            return Ok(Transition::None);
        }
        if !matches!(verb, "back" | "page" | "dlg-cancel" | "dlg-ok") {
            if let Some(m) = self.module.as_mut() {
                let reply = m.action(a, cx);
                log::info!("settings: page {:?} answered {reply:?}", self.page);
                if let Some(text) = reply.status {
                    self.status_text = text;
                    self.log_status();
                }
                if reply.rebuild {
                    self.rebuild = Some(RefreshClass::Gl16);
                } else {
                    let text = self.status_text.clone();
                    self.set_status(text);
                }
                return Ok(reply.transition.unwrap_or(Transition::None));
            }
        }
        match verb {
            "back" => return Ok(self.back(cx)),
            "page" => {
                if let Some(p) = Page::from_name(arg) {
                    self.go(p, cx);
                }
            }
            "exit" if current_settings(cx).ui.confirm_exit => self.confirm(Confirm::Exit),
            "exit" => {
                log::info!("settings: exit to Kindle (no confirmation)");
                return Ok(Transition::Exit);
            }
            "confirm-exit" => {
                let on = arg == "on";
                edit_settings(cx, |s| s.ui.confirm_exit = on);
                self.set_status(if on { "Exit asks first" } else { "Exit does not ask" });
            }
            "dlg-cancel" => self.close_dialog(),
            "dlg-ok" => {
                let Some((_, what)) = self.dialog.take() else { return Ok(Transition::None) };
                self.close_overlay();
                return self.confirmed(what, cx);
            }
            "sleep" => {
                let Ok(m) = arg.parse::<u32>() else { return Ok(Transition::None) };
                edit_settings(cx, |s| s.device.sleep_minutes = m);
                let r = cx.services.power.set_auto_sleep(auto_sleep(m));
                self.rebuild_with(match r {
                    Ok(()) => format!("Sleep: {}", sleep_name(m).to_lowercase()),
                    Err(e) => format!("Saved; the device says: {e}"),
                });
            }
            "saver" => {
                edit_settings(cx, |s| set_screensaver_setting(s, arg));
                let r = cx.services.power.set_screensaver(crate::sleep_cover::screensaver(arg, cx.paths));
                self.rebuild_with(match r {
                    Ok(()) => format!("Screensaver: {}", screensaver_name(arg)),
                    Err(e) => format!("Saved; the device says: {e}"),
                });
            }
            "wifi" | "airplane" => {
                let on = arg == "on";
                let r = if verb == "wifi" { cx.services.wifi.set_enabled(on) } else { cx.services.wifi.set_airplane_mode(on) };
                // The same page again (main or Wi-Fi), with the new state.
                if self.page == Page::Wifi {
                    self.networks = cx.services.wifi.scan_list().unwrap_or_default();
                }
                self.rebuild_with(match r {
                    Ok(()) => String::new(),
                    Err(e) => format!("{verb}: {e}"),
                });
            }
            "scan" => {
                self.say_now("Scanning…", cx)?;
                match cx.services.wifi.scan_in_background() {
                    // The page follows when the scan ends (NetEvent::Scanned).
                    Ok(true) => self.scanning = true,
                    Ok(false) => {
                        self.go(Page::Wifi, cx);
                        self.networks = cx.services.wifi.scan_list().unwrap_or_default();
                        let n = self.networks.len();
                        self.status_text = format!("{n} network{} found", if n == 1 { "" } else { "s" });
                    }
                    Err(e) => {
                        self.go(Page::Wifi, cx);
                        self.status_text = format!("Scan failed: {e}");
                    }
                }
            }
            "net" => {
                let Some(n) = arg.parse::<usize>().ok().and_then(|i| self.networks.get(i)).cloned() else {
                    return Ok(Transition::None);
                };
                if n.secured && !n.known {
                    self.go(Page::Join(n.essid), cx);
                } else if self.connect(&n.essid, None, cx)? {
                    self.go_keep_status(Page::Wifi, cx);
                } else if n.secured {
                    // The saved password may be wrong (a failed join keeps the profile):
                    // ask for it again instead of retrying the saved one forever.
                    self.go_keep_status(Page::Join(n.essid), cx);
                }
            }
            "join" => self.join(cx)?,
            "forget" => self.confirm(Confirm::Forget(arg.to_string())),
            "rot" => {
                let Ok(r) = arg.parse::<u8>() else { return Ok(Transition::None) };
                edit_settings(cx, |s| s.device.rotation = r % 4);
                // The app applies the rotation after this event; the canvas size change
                // lays the page out again (on_resize). Same size: rebuild for the check.
                self.rebuild_with(format!("Orientation: {}", rotation_name(Rotation::ALL[(r % 4) as usize]).to_lowercase()));
            }
            "night" => {
                let on = arg == "on";
                edit_settings(cx, |s| s.device.night_mode = on);
                if let Err(e) = cx.screen.set_night_mode(on) {
                    self.status_text = format!("Night mode: {e}");
                }
                self.rebuild = Some(RefreshClass::Gc16);
            }
            "anim" => {
                let on = arg == "on";
                edit_settings(cx, |s| set_page_wave(s, on));
                self.set_status(if on { "Page turn wave on" } else { "Page turn wave off" });
            }
            "font-px" | "spacing" | "margin" | "pspace" | "indent" | "weight" => {
                let Ok(v) = arg.parse::<f32>() else { return Ok(Transition::None) };
                edit_settings(cx, |s| {
                    let r = &mut s.reader;
                    match verb {
                        "font-px" => r.font_px = v,
                        "spacing" => r.line_spacing = v,
                        "margin" => r.margin_x = v,
                        "pspace" => r.paragraph_spacing = v.clamp(0.0, 3.0),
                        "indent" => r.paragraph_indent = if v < 0.0 { INDENT_BOOK } else { v.min(6.0) },
                        _ => r.font_weight = (v.max(0.0) as u8).min(WEIGHT_MAX),
                    }
                });
                self.rebuild_with(match verb {
                    "font-px" => format!("Default text size {v} px"),
                    "spacing" => format!("Default line spacing {v:.1}"),
                    "margin" => format!("Default side margins {v} px"),
                    "pspace" => format!("Default paragraph spacing {v} em"),
                    "indent" if v < 0.0 => "Default indent: the book's own".to_string(),
                    "indent" => format!("Default first-line indent {v} em"),
                    _ => format!("Default text weight {}", WEIGHTS.iter().find(|(l, _)| *l as f32 == v).map_or("", |w| w.1).to_lowercase()),
                });
            }
            "hyph" | "publisher" | "ruby" => {
                let on = arg == "on";
                edit_settings(cx, |s| match verb {
                    "hyph" => s.reader.hyphenate = on,
                    "ruby" => s.reader.ruby = on,
                    _ => s.reader.publisher_styles = on,
                });
                self.set_status(match (verb, on) {
                    ("hyph", true) => "Hyphenation on",
                    ("hyph", false) => "Hyphenation off",
                    ("ruby", true) => "Ruby annotations shown",
                    ("ruby", false) => "Ruby annotations hidden",
                    (_, true) => "The book's own styles are used",
                    _ => "The book's own styles are ignored",
                });
            }
            "justify" => {
                let on = arg == "on";
                edit_settings(cx, |s| s.reader.justify = on);
                self.set_status(if on { "Justified text" } else { "Ragged right text" });
            }
            "nav" => {
                let items = crate::plugin_entries::nav_chosen(current_settings(cx), cx.plugins);
                let extra = crate::plugin_entries::nav_choices(cx.plugins);
                let list = crate::plugin_entries::merged(ink_store::prefs::NAV_ITEMS, &extra);
                let Some(reply) = order::page_action(a, "nav", &list, items, (NAV_MIN, NAV_MAX), cx, |s, v| s.ui.nav_items = v) else { return Ok(Transition::None) };
                self.status_text = reply.status.unwrap_or_default();
                // The switch flipped in the list even when refused: build it again.
                self.rebuild = Some(RefreshClass::Gl16);
            }
            "dir" => {
                let Some((i, on)) = arg.rsplit_once(':') else { return Ok(Transition::None) };
                let Some(dir) = i.parse::<usize>().ok().and_then(|i| self.folders.get(i)).cloned() else {
                    return Ok(Transition::None);
                };
                let mut dirs = current_settings(cx).library_dirs.clone();
                if on == "on" && !dirs.contains(&dir) {
                    dirs.push(dir.clone());
                } else if on == "off" {
                    if dirs.len() <= 1 {
                        self.rebuild_with("At least one folder must stay on");
                        return Ok(Transition::None);
                    }
                    dirs.retain(|d| *d != dir);
                }
                log::info!("settings: library folders {dirs:?}");
                edit_settings(cx, |s| s.library_dirs = dirs);
                self.set_status(format!("{} {}", dir.display(), if on == "on" { "added" } else { "removed" }));
            }
            "export-all" => {
                let format = current_settings(cx).export_format();
                let text = match crate::export::load_index(cx.paths) {
                    Ok(index) => crate::export::export_everything(cx.paths, &index, format),
                    Err(e) => {
                        log::warn!("settings: export all: {e}");
                        format!("Export failed: {e}")
                    }
                };
                crate::export::toast(cx.plugins, text);
            }
            "ko-import" | "lib-rescan" | "lib-files" => {
                let req = match verb {
                    "ko-import" => crate::screens::library::Request::KoreaderImport,
                    "lib-rescan" => crate::screens::library::Request::Rescan,
                    _ => crate::screens::library::Request::Files,
                };
                crate::screens::library::request(req);
                log::info!("settings: library request {req:?}; back to the screen below");
                return Ok(Transition::Pop);
            }
            "exportfmt" => {
                let Some(f) = ExportFormat::from_name(arg) else { return Ok(Transition::None) };
                edit_settings(cx, |s| s.export_format = f.name().to_string());
                self.rebuild_with(format!("Export format: {}", export_format_name(f)));
            }
            "lang" => {
                if !LANGUAGES.iter().any(|(c, _)| *c == arg) {
                    return Ok(Transition::None);
                }
                edit_settings(cx, |s| s.language = arg.to_string());
                self.rebuild_with(format!("Language: {}", language_name(arg)));
            }
            "refresh" => {
                let Ok(n) = arg.parse::<u32>() else { return Ok(Transition::None) };
                edit_settings(cx, |s| s.reader.flash_every = n);
                self.rebuild_with(format!("Full refresh: {}; used from the next start", refresh_name(n).to_lowercase()));
            }
            "ntp" => {
                self.say_now("Asking the time server…", cx)?;
                let r = cx.services.clock.sync_ntp(None);
                self.go(Page::Device, cx);
                self.status_text = match r {
                    Ok(s) => format!("Time set from {} (offset {} ms)", s.server, s.offset_ms),
                    Err(e) => format!("Time sync failed: {e}"),
                };
            }
            "update-check" => {
                let client = ink_net::UreqClient::new();
                let cfg = update::UpdateConfig::from_settings(cx.settings, cx.paths);
                self.start_job(Job::spawn("Checking for an update", move |r: &Reporter<UpdateJob>| {
                    r.progress("asking the release server");
                    UpdateJob::Check(update::check(&client, &cfg))
                }));
            }
            // No question: the button says what happens.
            "update-install" => self.start_install(cx),
            "ppage" => {
                let Some(e) = arg.parse::<usize>().ok().and_then(|i| self.plugin_pages.get(i)).cloned() else {
                    return Ok(Transition::None);
                };
                log::info!("settings: plugin page {}/{}", e.plugin, e.id);
                self.open_plugin_page(e, cx);
            }
            "ptoggle" => {
                let Some((name, on)) = arg.rsplit_once(':') else { return Ok(Transition::None) };
                let on = on == "on";
                let r = plugin_host::set_user_enabled(cx.paths, name, on);
                self.rebuild_with(match r {
                    Ok(()) => format!("{name} turned {}; restart rinky to apply", if on { "on" } else { "off" }),
                    Err(e) => format!("Cannot change {name}: {e}"),
                });
            }
            _ => {}
        }
        Ok(Transition::None)
    }

    fn confirmed(&mut self, what: Confirm, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        match what {
            Confirm::Exit => {
                log::info!("settings: exit to Kindle");
                return Ok(Transition::Exit);
            }
            Confirm::Forget(essid) => {
                self.status_text = match cx.services.wifi.delete_profile(&essid) {
                    Ok(()) => format!("Forgot {essid}"),
                    Err(e) => format!("Cannot forget {essid}: {e}"),
                };
            }
        }
        Ok(Transition::None)
    }

    fn join(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        let Page::Join(essid) = self.page.clone() else { return Ok(()) };
        let password = self.input.and_then(|id| self.tree.get_mut::<TextInput>(id)).map(|t| t.text.clone()).unwrap_or_default();
        if password.is_empty() {
            self.set_status("Type the password first");
            return Ok(());
        }
        if self.connect(&essid, Some(&password), cx)? {
            self.go_keep_status(Page::Wifi, cx);
        }
        Ok(())
    }

    /// Downloads, checks and installs the release found by the check; rinky restarts
    /// into it when that worked ([`update::request_restart`]).
    fn start_install(&mut self, cx: &mut AppCtx) {
        let Some(rel) = self.release.clone() else { return };
        let client = ink_net::UreqClient::new();
        let cfg = update::UpdateConfig::from_settings(cx.settings, cx.paths);
        let label = format!("Installing {}", rel.version);
        self.start_job(Job::spawn(label, move |r: &Reporter<UpdateJob>| {
            r.progress("downloading, checking and installing");
            let out = update::install(&client, &cfg, &rel);
            UpdateJob::Install(rel, out)
        }));
    }

    fn start_job(&mut self, job: Job<UpdateJob>) {
        self.job = Some(job);
        self.poll_job();
    }

    /// Takes the job's progress or result into the status line.
    fn poll_job(&mut self) {
        let Some(job) = self.job.as_mut() else { return };
        match job.poll() {
            Poll::Running => {
                let text = job.status();
                if text != self.status_text {
                    // Not logged: the elapsed seconds change every second.
                    self.logged_status = text.clone();
                    self.set_status_class(text, RefreshClass::Du);
                }
            }
            Poll::Lost => {
                let label = job.label().to_string();
                self.job = None;
                self.set_status(format!("{label} stopped with an internal error"));
            }
            Poll::Done(out) => {
                self.job = None;
                match out {
                    UpdateJob::Check(r) => {
                        self.status_text = update_check_text(&r);
                        self.release = match r {
                            Ok((rel, true)) => Some(rel),
                            _ => None,
                        };
                    }
                    UpdateJob::Install(rel, r) => {
                        self.status_text = match r {
                            Ok(_) if update::request_restart() => {
                                self.restarting = Some(rel.version.to_string());
                                format!("Installed {}. Restarting rinky…", rel.version)
                            }
                            Ok(_) => format!("Installed {}. Exit and start rinky again to use it.", rel.version),
                            Err(e) => format!("Install failed: {e}"),
                        };
                        self.release = None;
                    }
                }
                self.rebuild = Some(RefreshClass::Gl16);
            }
        }
    }

    /// Feeds keyboard actions into the password field.
    fn keyboard_actions(&mut self, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        let (Some(kid), Some(fid)) = (self.keyboard, self.input) else { return Ok(Transition::None) };
        let actions: Vec<KeyAction> = self.tree.get_mut::<Keyboard>(kid).map(|k| k.take_actions()).unwrap_or_default();
        for a in actions {
            let r = self.tree.get_mut::<TextInput>(fid).map(|t| t.apply(&a));
            match r {
                Some(InputResult::Edited) => self.tree.mark_dirty(fid, RefreshClass::Du),
                Some(InputResult::Submitted) => {
                    self.join(cx)?;
                    break;
                }
                Some(InputResult::Closed) => return Ok(self.back(cx)),
                _ => {}
            }
        }
        Ok(Transition::None)
    }
}

impl AppScreen for SettingsScreen {
    fn name(&self) -> &'static str {
        "settings"
    }

    fn on_exit(&mut self, cx: &mut AppCtx) {
        self.leave_plugin_page(cx);
    }

    fn on_enter(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        // Show the whole screen: nothing of the previous screen may stay on the panel.
        // REAGL drives every pixel without the GC16 flash.
        let class = cx.refresh.class_for(Change::LargePanel);
        self.build(cx, class);
        self.rebuild = None;
        Ok(())
    }

    fn handle(&mut self, ev: &UiEvent, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        if matches!(ev, UiEvent::Quit) {
            return Ok(Transition::Exit);
        }
        if let UiEvent::Power(PowerEvent::Network(e)) = ev {
            self.network_event(e, cx);
            return Ok(Transition::None);
        }
        // The status line showed "Restarting rinky…": the launcher starts the new version.
        if let (Some(v), UiEvent::Timer(_)) = (&self.restarting, ev) {
            crate::logging::set_exit_reason(format!("update {v} installed: restarting into it"), true);
            return Ok(Transition::Exit);
        }
        if self.job.is_some() {
            self.poll_job();
        }
        if let Some(job) = &self.job {
            // Taps and keys wait until the job ends (an install must not be left half
            // way by a page change); the status line says so.
            let pressed = match ev {
                UiEvent::Key(k) => k.pressed,
                UiEvent::Gesture(_) => true,
                _ => false,
            };
            if pressed {
                let text = format!("Please wait: {}", job.status());
                self.set_status(text);
            }
            return Ok(Transition::None);
        }
        if let UiEvent::Key(k) = ev {
            if k.pressed && matches!(k.key, Key::Power | Key::Back) {
                return Ok(self.back(cx));
            }
            if k.pressed && matches!(k.key, Key::PageNext | Key::PagePrev) && self.dialog.is_none() {
                let content = content_rect(self.size);
                let d = if k.key == Key::PageNext { 1 } else { -1 } * content.h as i32 * 3 / 4;
                if let Some(id) = self.list {
                    if self.tree.get_mut::<SettingsList>(id).is_some_and(|l| l.scroll_by(content, d)) {
                        self.tree.mark_dirty(id, RefreshClass::Du);
                    }
                }
                return Ok(Transition::None);
            }
        }
        let t = match self.tree.dispatch(ev) {
            Some((_, EventResult::Action(a))) => {
                let t = self.action(&a, cx);
                // The row that opened a page is recorded by `go` during this action only.
                self.last_action = None;
                t?
            }
            _ => Transition::None,
        };
        if t != Transition::None {
            return Ok(t);
        }
        self.keyboard_actions(cx)
    }

    fn paint(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        // A plugin page's menu may come later (after a network answer).
        if matches!(self.page, Page::Plugin(..)) {
            self.take_plugin_menus(cx);
        }
        let mut before = None;
        if std::mem::take(&mut self.cut_damage) && self.rebuild.is_none() {
            before = Some(cx.canvas.clone());
        }
        if let Some(class) = self.rebuild.take() {
            let key = (self.page.clone(), cx.screen.size());
            // A GL16 rebuild of the page on screen: keep the old pixels to compare.
            if class == RefreshClass::Gl16 && self.built.as_ref() == Some(&key) {
                before = Some(cx.canvas.clone());
            }
            self.build(cx, class);
            self.built = Some(key);
        }
        let damage = self.tree.paint(&mut paint_cx(cx));
        let damage = match before {
            Some(old) => ink_ui::refresh::changed_bands(&old, cx.canvas, damage),
            None => damage,
        };
        // A whole page (another page, a tab, a dialog over the list) is REAGL: the
        // settings never flash on their own.
        let class = cx.refresh.class_for(Change::LargePanel);
        Ok(ink_ui::refresh::no_flash(damage, cx.canvas.bounds(), class))
    }

    fn timeout(&self) -> Option<Duration> {
        if self.restarting.is_some() {
            return Some(RESTART_DELAY);
        }
        self.job.is_some().then_some(worker::POLL)
    }
}
