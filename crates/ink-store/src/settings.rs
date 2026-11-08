//! Global settings: `/mnt/us/rinky/settings.toml`.
//!
//! Unknown keys are ignored and missing keys take defaults, so older and newer files
//! both load. Each settings screen owns its own section. [`Settings::plugins`] holds a
//! TOML table per plugin for options the user may edit by hand (read with
//! [`Settings::plugin_value`]); a plugin's own run-time settings live in
//! `plugins/<name>/settings.json` through [`crate::PluginStore`].
//!
//! [`Settings::load`] is strict (a hand-edited file with a syntax error is reported, not
//! replaced); [`Settings::load_or_recover`] moves a bad file aside and falls back to the
//! `.old` backup or the defaults. [`Settings::save`] keeps `settings.toml.old`.

use crate::{load_recovering, save_with_backup, LoadSource, Result, StoreError};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Largest side, top and bottom margin in pixels ([`ReaderSettings::clamp_margins`]).
pub const MARGIN_MAX_PX: f32 = 450.0;
/// Current [`ReaderSettings::margin_version`]. 1 (or no key): the top and bottom margins
/// were measured from the screen edge with the status bar inside them. 2: they are
/// measured from the inner edge of the bar ([`crate::prefs::StatusBar::room`]).
pub const MARGIN_VERSION: u32 = 2;

/// Reading defaults for new books.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReaderSettings {
    /// Body font size in pixels.
    pub font_px: f32,
    /// Line spacing factor.
    pub line_spacing: f32,
    /// Side margins in pixels, from the screen edges (0..=[`MARGIN_MAX_PX`]).
    pub margin_x: f32,
    /// Top margin in pixels, below the top status bar (from the screen edge when the
    /// bar is off; 0..=[`MARGIN_MAX_PX`]).
    pub margin_top: f32,
    /// Bottom margin in pixels, above the bottom status bar (0..=[`MARGIN_MAX_PX`]).
    pub margin_bottom: f32,
    /// How the top and bottom margins are measured ([`MARGIN_VERSION`]); a file
    /// without the key is converted once on load ([`ReaderSettings::migrate_margins`]).
    pub margin_version: u32,
    /// Justify text.
    pub justify: bool,
    /// Flash (GC16) every N page turns; 0 = never.
    pub flash_every: u32,
    /// Old bottom-margin style (`footer = "full"`); mapped once onto
    /// [`Settings::status_bars`] by [`Settings::migrate_status_bars`], not drawn any more.
    pub footer: FooterStyle,
    /// Old top-margin style (`header = "none"`); mapped like `footer`.
    pub header: HeaderStyle,
    /// Page-turn animation: `on` or `off` (older `slide`, `curl`, `fade` read as `on`
    /// and are rewritten as `on` by [`Settings::migrate`]; see
    /// [`crate::prefs::page_animation_on`]).
    pub page_animation: String,
    /// Reading font family (empty: the default, Bookerly when the Kindle has it, else
    /// the built-in font).
    pub font_family: String,
    /// Extra space after every paragraph in em (0 = the book's own spacing; the Aa
    /// sheet offers 0, 0.5 and 1).
    pub paragraph_spacing: f32,
    /// First-line indent of paragraphs in em; a negative value ([`INDENT_BOOK`]) keeps
    /// the book's own indent (the sheet offers book, 0, 1, 1.5 and 2).
    pub paragraph_indent: f32,
    /// Darker text for e-ink: 0 normal, 1 and 2 widen every stroke ([`WEIGHT_MAX`]).
    pub font_weight: u8,
    /// Hyphenate words (new books; a book's own choice wins).
    pub hyphenate: bool,
    /// Use the book's own CSS (new books; a book's own choice wins). Off: the reader's
    /// plain style and the settings above only.
    pub publisher_styles: bool,
}

/// [`ReaderSettings::paragraph_indent`] value for "use the book's indent".
pub const INDENT_BOOK: f32 = -1.0;
/// Largest [`ReaderSettings::font_weight`].
pub const WEIGHT_MAX: u8 = 2;

/// What the reader draws in the bottom margin. Stored as a lowercase string; an unknown
/// string reads as the default, so older and newer files both load.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum FooterStyle {
    /// Nothing.
    None,
    /// `3 / 208`, centred.
    Pages,
    /// A thin progress bar across the text width.
    Bar,
    /// The thin bar with a tick at every chapter start.
    Chapters,
    /// The bar with chapter ticks, then `12 min left in chapter · 42%`.
    #[default]
    Full,
}

/// What the reader draws in the top margin. Stored like [`FooterStyle`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum HeaderStyle {
    /// Nothing.
    #[default]
    None,
    /// The book title.
    Book,
    /// The title of the current chapter.
    Chapter,
}

/// The names of a style stored as a lowercase string: `ALL`, `as_str`, `parse`, and the
/// `String` conversions serde uses (an unknown name reads as the default).
macro_rules! named_style {
    ($ty:ident { $($variant:ident = $name:literal),+ $(,)? }) => {
        impl $ty {
            /// Every style, in the order a settings control lists them.
            pub const ALL: [$ty; [$($name),+].len()] = [$($ty::$variant),+];

            /// The stored name.
            pub fn as_str(self) -> &'static str {
                match self {
                    $($ty::$variant => $name),+
                }
            }

            /// The style named `s`, if it is one.
            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|f| f.as_str() == s)
            }
        }

        impl From<String> for $ty {
            fn from(s: String) -> Self {
                Self::parse(&s).unwrap_or_default()
            }
        }

        impl From<$ty> for String {
            fn from(f: $ty) -> Self {
                f.as_str().to_string()
            }
        }
    };
}

named_style!(FooterStyle { None = "none", Pages = "pages", Bar = "bar", Chapters = "chapters", Full = "full" });
named_style!(HeaderStyle { None = "none", Book = "book", Chapter = "chapter" });

impl Default for ReaderSettings {
    fn default() -> Self {
        ReaderSettings {
            font_px: 40.0,
            line_spacing: 1.4,
            margin_x: 70.0,
            // With the default bars the text starts about 70 px from the top and ends
            // about 110 px from the bottom, as before the margins were measured from them.
            margin_top: 20.0,
            margin_bottom: 30.0,
            margin_version: MARGIN_VERSION,
            justify: true,
            flash_every: 8,
            footer: FooterStyle::default(),
            header: HeaderStyle::default(),
            page_animation: "on".into(),
            font_family: String::new(),
            paragraph_spacing: 0.0,
            paragraph_indent: INDENT_BOOK,
            font_weight: 0,
            hyphenate: false,
            publisher_styles: true,
        }
    }
}

/// Device and UI settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceSettings {
    /// Touch mapping options (`swap,mirror_x,mirror_y`), like `RINKY_TOUCH`.
    pub touch: String,
    /// Night mode.
    pub night_mode: bool,
    /// Rotation in quarter turns clockwise (0..=3).
    pub rotation: u8,
    /// Minutes of inactivity before sleep; 0 = never.
    pub sleep_minutes: u32,
    /// Screensaver: `page` (current page), `cover`, `blank` or `image:PATH`, as
    /// `ink_device::screensaver::Screensaver::from_setting` reads it.
    pub screensaver: String,
    /// Wi-Fi was on when rinky last ran (turned on, or connected, and not turned off
    /// by the user since): Wi-Fi is turned on again at start-up and after a wake.
    pub wifi_was_on: bool,
}

impl Default for DeviceSettings {
    fn default() -> Self {
        DeviceSettings {
            touch: String::new(),
            night_mode: false,
            rotation: 0,
            sleep_minutes: 10,
            screensaver: DEFAULT_SCREENSAVER.into(),
            wifi_was_on: false,
        }
    }
}

/// All settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Directories scanned for books.
    pub library_dirs: Vec<PathBuf>,
    /// UI language (`en`, `da`).
    pub language: String,
    /// File format of highlight exports: `markdown`, `clippings` (Kindle's
    /// `My Clippings.txt`) or `json`; see [`Settings::export_format`].
    pub export_format: String,
    /// Reading defaults.
    pub reader: ReaderSettings,
    /// Device options.
    pub device: DeviceSettings,
    /// UI font, keyboard layouts, navigation bar, control-center buttons.
    pub ui: crate::prefs::UiSettings,
    /// Reader gesture map (`[gestures]`, trigger = action).
    pub gestures: crate::prefs::GestureMap,
    /// Reader status bars (`[status_bars.top]`, `[status_bars.bottom]`).
    pub status_bars: crate::prefs::StatusBars,
    /// Per-plugin values, by plugin name.
    pub plugins: toml::Table,
}

impl Settings {
    /// [`Settings::export_format`] as a format; an unknown name gives Markdown.
    pub fn export_format(&self) -> crate::export::ExportFormat {
        crate::export::ExportFormat::from_name(&self.export_format).unwrap_or_default()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            library_dirs: ink_platform::current().default_library_dirs(),
            language: "en".into(),
            export_format: crate::export::ExportFormat::default().name().into(),
            reader: ReaderSettings::default(),
            device: DeviceSettings::default(),
            ui: crate::prefs::UiSettings::default(),
            gestures: crate::prefs::GestureMap::default(),
            status_bars: crate::prefs::StatusBars::default(),
            plugins: toml::Table::new(),
        }
    }
}

/// Default [`DeviceSettings::screensaver`].
pub const DEFAULT_SCREENSAVER: &str = "page";

/// Where the wave B settings screen kept the screensaver before
/// [`DeviceSettings::screensaver`] existed: `[plugins.settings-ui] screensaver = "..."`.
pub const LEGACY_UI_TABLE: &str = "settings-ui";
/// Plugin table where the settings screen kept `page_animation` before
/// [`ReaderSettings::page_animation`] existed.
pub const LEGACY_READER_TABLE: &str = "ui-reader";

fn parse_settings(b: &[u8]) -> std::result::Result<Settings, String> {
    let text = std::str::from_utf8(b).map_err(|e| e.to_string())?;
    let table: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let reader = table.get("reader").and_then(|r| r.as_table());
    let has = |k: &str| reader.is_some_and(|r| r.contains_key(k));
    // `[status_bars]` defaults when absent, so its absence is only visible here: which
    // of the old `footer` and `header` keys the file has.
    let legacy = (!table.contains_key("status_bars")).then(|| (has("footer"), has("header")));
    // Same for the margin version; only the margin keys the file has are converted
    // (an absent key takes the new default, which needs no conversion).
    let margin_version = reader.and_then(|r| r.get("margin_version")).and_then(|v| v.as_integer()).unwrap_or(1);
    let old_margins = (margin_version < i64::from(MARGIN_VERSION)).then(|| (has("margin_top"), has("margin_bottom")));
    // `version` defaults to the current one when absent, so read the stored one here.
    let bars_version = table.get("status_bars").and_then(|t| t.get("version")).and_then(|v| v.as_integer()).unwrap_or(0);
    let mut s: Settings = table.try_into().map_err(|e: toml::de::Error| e.to_string())?;
    s.migrate();
    match legacy {
        Some((footer, header)) => {
            s.migrate_status_bars(footer, header);
        }
        None if bars_version < i64::from(crate::prefs::STATUS_BARS_VERSION) => {
            s.repair_status_bars_v1();
        }
        None => {}
    }
    s.status_bars.version = crate::prefs::STATUS_BARS_VERSION;
    if let Some((top, bottom)) = old_margins {
        let bars = s.status_bars.clone();
        s.reader.migrate_margins(&bars, top, bottom);
    }
    s.reader.margin_version = MARGIN_VERSION;
    s.reader.clamp_margins();
    Ok(s)
}

/// `old` (a version 1 margin: from the screen edge, the bar inside it) measured from the
/// inner edge of a bar that takes `room` pixels: `max(0, old - room)`.
pub fn margin_from_bar(old: f32, room: f32) -> f32 {
    (old - room).max(0.0)
}

impl ReaderSettings {
    /// Converts the version 1 top margin (when `top`) and bottom margin (when `bottom`)
    /// to be measured from the bars `bars` ([`margin_from_bar`]), so the text stays
    /// where it was. Does not set [`ReaderSettings::margin_version`].
    pub fn migrate_margins(&mut self, bars: &crate::prefs::StatusBars, top: bool, bottom: bool) {
        if top {
            self.margin_top = margin_from_bar(self.margin_top, bars.top.room());
        }
        if bottom {
            self.margin_bottom = margin_from_bar(self.margin_bottom, bars.bottom.room());
        }
    }

    /// Puts the margins into 0..=[`MARGIN_MAX_PX`] (called by every load); a value that is
    /// not a number takes its default. Returns true when one changed.
    pub fn clamp_margins(&mut self) -> bool {
        let d = ReaderSettings::default();
        let before = (self.margin_x, self.margin_top, self.margin_bottom);
        for (v, def) in [(&mut self.margin_x, d.margin_x), (&mut self.margin_top, d.margin_top), (&mut self.margin_bottom, d.margin_bottom)] {
            *v = if v.is_finite() { v.clamp(0.0, MARGIN_MAX_PX) } else { def };
        }
        before != (self.margin_x, self.margin_top, self.margin_bottom)
    }
}

impl Settings {
    /// Moves values from older layouts into their fields (called by every load):
    /// `[plugins.settings-ui] screensaver` becomes `device.screensaver` and
    /// `[plugins.ui-reader] page_animation` becomes `reader.page_animation` (the old key
    /// wins: only a binary without the field writes it) and the old key is removed, with
    /// the table when it is then empty. A `page_animation` other than `on`/`off` (the
    /// older `slide`, `curl`, `fade`) becomes `on`. Returns true when something moved.
    pub fn migrate(&mut self) -> bool {
        let mut moved = false;
        type Field = fn(&mut Settings) -> &mut String;
        let moves: [(&str, &str, Field); 2] =
            [(LEGACY_UI_TABLE, "screensaver", |s| &mut s.device.screensaver), (LEGACY_READER_TABLE, "page_animation", |s| &mut s.reader.page_animation)];
        for (table, key, field) in moves {
            let Some(t) = self.plugins.get_mut(table).and_then(|v| v.as_table_mut()) else { continue };
            let Some(old) = t.remove(key) else { continue };
            if t.is_empty() {
                self.plugins.remove(table);
            }
            if let Some(v) = old.as_str().filter(|v| !v.is_empty()) {
                *field(self) = v.to_string();
            }
            moved = true;
        }
        let anim = self.reader.page_animation.as_str();
        if anim != "on" && anim != "off" {
            self.reader.page_animation = if crate::prefs::page_animation_on(anim) { "on" } else { "off" }.into();
            moved = true;
        }
        moved
    }

    /// Maps the old `[reader] footer` / `header` styles onto [`Settings::status_bars`]
    /// (done by every load of a file without a `[status_bars]` table; only the keys the
    /// file has are mapped, so a file with neither keeps the default bars):
    /// footer `none` = bottom bar off, `pages` = `page_of_total` centred, `bar` = thin
    /// progress only, `chapters` = progress with chapter ticks only, `full` = the default
    /// bottom bar; header `none` / `chapter` = the default top bar (clock, chapter title,
    /// battery; on: the old header had no clock or battery to keep), `book` = the same
    /// with the book title centred. Returns true when a bar changed.
    pub fn migrate_status_bars(&mut self, footer: bool, header: bool) -> bool {
        use crate::prefs::{StatusBar, StatusBars};
        let before = self.status_bars.clone();
        let one = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        if footer {
            let bottom = &mut self.status_bars.bottom;
            *bottom = match self.reader.footer {
                FooterStyle::None => StatusBar { enabled: false, ..StatusBars::default().bottom },
                FooterStyle::Pages => StatusBar { center: one(&["page_of_total"]), ..StatusBar::default() },
                FooterStyle::Bar => StatusBar { progress_bar: "thin".into(), ..StatusBar::default() },
                FooterStyle::Chapters => StatusBar { progress_bar: "chapters".into(), ..StatusBar::default() },
                FooterStyle::Full => StatusBars::default().bottom,
            };
        }
        if header {
            let top = &mut self.status_bars.top;
            *top = match self.reader.header {
                HeaderStyle::None | HeaderStyle::Chapter => StatusBars::default().top,
                HeaderStyle::Book => StatusBar { center: one(&["book_title"]), ..StatusBars::default().top },
            };
        }
        self.status_bars != before
    }

    /// Repairs a `[status_bars]` table written by the first mapping (no `version` key),
    /// which turned the top bar off for `header = "none"`: a top bar that is exactly that
    /// (the default top bar, off) is turned on. Everything else is kept as the user set
    /// it. Returns true when the top bar was turned on.
    pub fn repair_status_bars_v1(&mut self) -> bool {
        let off = crate::prefs::StatusBar { enabled: false, ..crate::prefs::StatusBars::default().top };
        let fix = self.status_bars.top == off;
        if fix {
            self.status_bars.top.enabled = true;
        }
        fix
    }

    /// Loads `path`; a missing file gives the defaults; a file that does not parse is an
    /// error ([`StoreError::Format`]) and is left in place.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(b) => parse_settings(&b).map_err(|e| StoreError::Format(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Loads `path`, never failing on bad content: a file that does not parse is moved to
    /// `settings.toml.corrupt-<time>-<n>` and `settings.toml.old` or the defaults are used.
    /// Returns the settings and a warning to show when something was recovered.
    pub fn load_or_recover(path: &Path) -> Result<(Self, Option<String>)> {
        let r = load_recovering(path, parse_settings)?;
        let warning = (!r.errors.is_empty()).then(|| {
            let kept: Vec<String> = r.set_aside.iter().map(|p| p.display().to_string()).collect();
            format!("settings recovered from {:?}; bad file kept as {}: {}", r.source, kept.join(", "), r.errors.join("; "))
        });
        debug_assert!(r.value.is_some() || r.source == LoadSource::Missing);
        Ok((r.value.unwrap_or_default(), warning))
    }

    /// Saves atomically (with a `.old` backup at most once a minute).
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| StoreError::Format(e.to_string()))?;
        save_with_backup(path, text.as_bytes(), |b| parse_settings(b).is_ok())
    }

    /// The `[plugins.<name>]` table, if present.
    pub fn plugin_table(&self, name: &str) -> Option<&toml::Table> {
        self.plugins.get(name)?.as_table()
    }

    /// The `[plugins.<name>]` table as `T` (`None` if absent; an error if it does not
    /// match `T`).
    pub fn plugin_value<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        match self.plugins.get(name) {
            None => Ok(None),
            Some(v) => v.clone().try_into().map(Some).map_err(|e| StoreError::Format(format!("plugins.{name}: {e}"))),
        }
    }

    /// Stores `value` as the `[plugins.<name>]` table. TOML has no `null` and no integers
    /// above `i64::MAX`, and the TOML reader limits nesting; such values are refused with
    /// [`StoreError::Invalid`] (fields that are `None` are left out), so `settings.toml`
    /// always loads again.
    pub fn set_plugin_value<T: Serialize>(&mut self, name: &str, value: &T) -> Result<()> {
        crate::plugin::validate_name(name)?;
        let invalid = |e: &dyn std::fmt::Display| StoreError::Invalid(format!("plugins.{name}: {e}"));
        let v = toml::Value::try_from(value).map_err(|e| invalid(&e))?;
        // The table as `save` writes it must read back.
        let probe = toml::Table::from_iter([("plugins".to_string(), toml::Value::Table(toml::Table::from_iter([(name.to_string(), v.clone())])))]);
        let text = toml::to_string_pretty(&probe).map_err(|e| invalid(&e))?;
        toml::from_str::<toml::Table>(&text).map_err(|e| invalid(&e))?;
        self.plugins.insert(name.to_string(), v);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_margins_convert_once_to_be_measured_from_the_bars() {
        // The device file (top 70, bottom 110, footer "full", header "none"): with the
        // default bars (rooms 53 and 81) the text stays where it was.
        let text = "[reader]\nmargin_x = 70.0\nmargin_top = 70.0\nmargin_bottom = 110.0\nfooter = \"full\"\nheader = \"none\"\n";
        let s = parse_settings(text.as_bytes()).unwrap();
        assert_eq!((s.reader.margin_x, s.reader.margin_top, s.reader.margin_bottom, s.reader.margin_version), (70.0, 17.0, 29.0, MARGIN_VERSION));
        // Saved with the key: the next load does not convert again.
        let out = toml::to_string_pretty(&s).unwrap();
        assert!(out.contains("margin_version = 2"), "{out}");
        assert_eq!(parse_settings(out.as_bytes()).unwrap().reader, s.reader);
        // Keys the file does not have take the new defaults unchanged.
        let d = parse_settings(b"[reader]\nmargin_top = 70.0\n").unwrap();
        assert_eq!((d.reader.margin_top, d.reader.margin_bottom), (17.0, ReaderSettings::default().margin_bottom));
        assert_eq!(parse_settings(b"language = \"en\"\n").unwrap().reader, ReaderSettings::default());
        // A bar that is off took no room: that margin stays.
        let off = parse_settings(b"[reader]\nmargin_top = 70.0\nmargin_bottom = 110.0\n[status_bars.bottom]\nenabled = false\n").unwrap();
        assert_eq!((off.reader.margin_top, off.reader.margin_bottom), (17.0, 110.0));
    }

    #[test]
    fn typography_keys_default_when_missing_and_round_trip() {
        // A file from before the keys existed loads with today's behaviour.
        let old = parse_settings(b"[reader]\nfont_px = 44.0\njustify = false\n").unwrap();
        let d = ReaderSettings::default();
        assert_eq!((old.reader.paragraph_spacing, old.reader.paragraph_indent, old.reader.font_weight), (0.0, INDENT_BOOK, 0));
        assert!(!old.reader.hyphenate && old.reader.publisher_styles);
        assert_eq!(old.reader.font_px, 44.0);
        assert_eq!(parse_settings(b"").unwrap().reader, d);
        // Set values survive a save and load.
        let mut s = Settings::default();
        s.reader.paragraph_spacing = 0.5;
        s.reader.paragraph_indent = 1.5;
        s.reader.font_weight = 2;
        s.reader.hyphenate = true;
        s.reader.publisher_styles = false;
        let out = toml::to_string_pretty(&s).unwrap();
        assert_eq!(parse_settings(out.as_bytes()).unwrap().reader, s.reader);
    }

    #[test]
    fn export_format_defaults_to_markdown_and_round_trips() {
        use crate::export::ExportFormat;
        assert_eq!(parse_settings(b"").unwrap().export_format(), ExportFormat::Markdown);
        assert_eq!(parse_settings(b"export_format = \"nonsense\"\n").unwrap().export_format(), ExportFormat::Markdown);
        let s = Settings { export_format: "clippings".into(), ..Default::default() };
        let out = toml::to_string_pretty(&s).unwrap();
        assert_eq!(parse_settings(out.as_bytes()).unwrap().export_format(), ExportFormat::Clippings);
    }

    #[test]
    fn margins_load_into_their_range() {
        let s = parse_settings(b"[reader]\nmargin_version = 2\nmargin_x = 900.0\nmargin_top = -5.0\nmargin_bottom = 450.0\n").unwrap();
        assert_eq!((s.reader.margin_x, s.reader.margin_top, s.reader.margin_bottom), (MARGIN_MAX_PX, 0.0, 450.0));
        let mut r = ReaderSettings { margin_x: f32::NAN, ..ReaderSettings::default() };
        assert!(r.clamp_margins());
        assert_eq!(r.margin_x, ReaderSettings::default().margin_x);
        assert!(!r.clamp_margins());
    }

    #[test]
    fn defaults_partial_files_and_round_trip() {
        let s: Settings = toml::from_str("language = \"da\"\n[reader]\nfont_px = 30.0\nunknown = 1\n").unwrap();
        assert_eq!(s.language, "da");
        assert_eq!(s.reader.font_px, 30.0);
        assert_eq!(s.reader.flash_every, 8);
        assert_eq!(s.device.sleep_minutes, 10);
        let dir = std::env::temp_dir().join(format!("ink-settings-{}", std::process::id()));
        let file = dir.join("settings.toml");
        let mut t = Settings::default();
        t.plugins.insert("kosync".into(), toml::Value::Table(toml::Table::from_iter([("server".to_string(), toml::Value::from("https://x"))])));
        t.save(&file).unwrap();
        assert_eq!(Settings::load(&file).unwrap(), t);
        std::fs::write(&file, "language = [").unwrap();
        assert!(Settings::load(&file).is_err());
        assert!(file.exists(), "strict load leaves the file alone");
        assert_eq!(Settings::load(&dir.join("missing.toml")).unwrap(), Settings::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rotate_angle_round_trips_defaults_and_ignores_unknown_values() {
        assert_eq!(Settings::default().ui.rotate_degrees(), 90);
        let s: Settings = toml::from_str("[ui]\nconfirm_exit = false\n").unwrap();
        assert_eq!(s.ui.rotate_angle, 90, "a file without the key gets the default");
        let s: Settings = toml::from_str("[ui]\nrotate_angle = 45\n").unwrap();
        assert_eq!(s.ui.rotate_degrees(), 90, "an unknown angle reads as the default");
        let dir = std::env::temp_dir().join(format!("ink-settings-angle-{}", std::process::id()));
        let file = dir.join("settings.toml");
        let mut t = Settings::default();
        t.ui.rotate_angle = 270;
        t.save(&file).unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains("rotate_angle = 270"));
        assert_eq!(Settings::load(&file).unwrap().ui.rotate_degrees(), 270);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn footer_and_header_names_round_trip_and_unknown_names_read_as_default() {
        assert_eq!(Settings::default().reader.footer, FooterStyle::Full);
        assert_eq!(Settings::default().reader.header, HeaderStyle::None);
        let s: Settings = toml::from_str("[reader]\nfooter = \"chapters\"\nheader = \"chapter\"\n").unwrap();
        assert_eq!((s.reader.footer, s.reader.header), (FooterStyle::Chapters, HeaderStyle::Chapter));
        let text = toml::to_string(&s).unwrap();
        assert!(text.contains("footer = \"chapters\"") && text.contains("header = \"chapter\""), "{text}");
        let s: Settings = toml::from_str("[reader]\nfooter = \"sparkles\"\nheader = \"x\"\nfont_px = 30.0\n").unwrap();
        assert_eq!((s.reader.footer, s.reader.header, s.reader.font_px), (FooterStyle::Full, HeaderStyle::None, 30.0));
        for f in FooterStyle::ALL {
            assert_eq!(FooterStyle::parse(f.as_str()), Some(f));
        }
        for h in HeaderStyle::ALL {
            assert_eq!(HeaderStyle::parse(h.as_str()), Some(h));
        }
        let b: crate::sidecar::BookSettings = serde_json::from_str(r#"{"footer":"bar","header":"book"}"#).unwrap();
        assert_eq!((b.footer, b.header), (Some(FooterStyle::Bar), Some(HeaderStyle::Book)));
        assert!(b.extra.is_empty());
        assert_eq!(serde_json::to_string(&crate::sidecar::BookSettings::default()).unwrap(), "{}");
    }

    #[test]
    fn recover_and_plugin_values() {
        let dir = crate::testutil::TempDir::new("settings-recover");
        let file = dir.path().join("settings.toml");
        let mut s = Settings { language: "da".into(), ..Default::default() };
        s.save(&file).unwrap();
        s.language = "en".into();
        s.save(&file).unwrap();
        std::fs::write(&file, "language = [").unwrap();
        let (back, warning) = Settings::load_or_recover(&file).unwrap();
        assert_eq!(back.language, "da", "the .old backup is used");
        assert!(warning.unwrap().contains("corrupt-"));
        let (none, w) = Settings::load_or_recover(&dir.path().join("nothing.toml")).unwrap();
        assert_eq!((none, w), (Settings::default(), None));

        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Kosync {
            server: String,
            port: u16,
            user: Option<String>,
        }
        let mut s = Settings::default();
        assert_eq!(s.plugin_value::<Kosync>("kosync").unwrap(), None);
        let k = Kosync { server: "https://sync.example".into(), port: 443, user: None };
        s.set_plugin_value("kosync", &k).unwrap();
        assert_eq!(s.plugin_value::<Kosync>("kosync").unwrap(), Some(k));
        assert_eq!(s.plugin_table("kosync").unwrap()["port"].as_integer(), Some(443));
        assert!(s.set_plugin_value("../evil", &1).is_err());
        assert!(s.set_plugin_value("big", &serde_json::json!({ "n": u64::MAX })).is_err());
        // Nested deeper than the TOML reader reads back: refused, nothing stored.
        let deep = (0..100).fold(serde_json::json!(1), |v, _| serde_json::json!({ "k": v }));
        assert!(matches!(s.set_plugin_value("deep", &deep), Err(StoreError::Invalid(_))));
        assert!(s.plugin_table("deep").is_none());
        assert!(s.plugin_value::<u32>("kosync").is_err());
        s.save(&file).unwrap();
        assert_eq!(Settings::load(&file).unwrap(), s);
    }

    #[test]
    fn page_animation_defaults_and_migrates_from_the_ui_reader_table() {
        assert_eq!(Settings::default().reader.page_animation, "on");
        let mut s: Settings = toml::from_str("[plugins.ui-reader]\npage_animation = \"curl\"\n").unwrap();
        assert!(s.migrate());
        assert_eq!(s.reader.page_animation, "on");
        assert!(!s.plugins.contains_key(LEGACY_READER_TABLE));
        assert!(!s.migrate());
    }

    #[test]
    fn screensaver_field_defaults_and_migrates_from_the_settings_ui_table() {
        assert_eq!(Settings::default().device.screensaver, "page");
        let s: Settings = toml::from_str("[device]\nsleep_minutes = 5\n").unwrap();
        assert_eq!(s.device.screensaver, "page", "files without the field get the default");

        let dir = crate::testutil::TempDir::new("settings-saver");
        let file = dir.path().join("settings.toml");
        // A file written by the wave B settings screen (screensaver in a plugin table).
        std::fs::write(&file, "[device]\nsleep_minutes = 5\n\n[plugins.settings-ui]\nscreensaver = \"blank\"\n\n[plugins.kosync]\nport = 1\n").unwrap();
        let s = Settings::load(&file).unwrap();
        assert_eq!(s.device.screensaver, "blank");
        assert!(!s.plugins.contains_key(LEGACY_UI_TABLE), "the emptied table is removed");
        assert!(s.plugins.contains_key("kosync"));
        let (r, w) = Settings::load_or_recover(&file).unwrap();
        assert_eq!((r.device.screensaver.as_str(), w), ("blank", None));
        // Saved again: the new field is written, the old key is gone.
        s.save(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("screensaver = \"blank\"") && !text.contains("settings-ui"), "{text}");
        assert_eq!(Settings::load(&file).unwrap(), s);

        // Other keys in the old table stay; an empty value keeps the field.
        let mut t: Settings = toml::from_str("[plugins.settings-ui]\nscreensaver = \"\"\nother = 1\n").unwrap();
        assert!(t.migrate());
        assert_eq!(t.device.screensaver, "page");
        assert_eq!(t.plugin_table(LEGACY_UI_TABLE).unwrap().len(), 1);
        assert!(!t.migrate(), "nothing left to move");
    }

    #[test]
    fn old_footer_and_header_map_to_status_bars_once() {
        let s = parse_settings(b"[reader]\nfooter = \"pages\"\nheader = \"chapter\"\n").unwrap();
        assert_eq!(s.status_bars.bottom.center, vec!["page_of_total".to_string()]);
        assert_eq!((s.status_bars.bottom.progress_bar.as_str(), s.status_bars.top.center[0].as_str()), ("none", "chapter_title"));
        let s = parse_settings(b"[reader]\nfooter = \"none\"\n").unwrap();
        assert!(!s.status_bars.bottom.enabled && s.status_bars.top.enabled, "only the keys present are mapped");
        let s = parse_settings(b"[reader]\nfooter = \"bar\"\n[status_bars.bottom]\nright = [\"clock\"]\n").unwrap();
        assert_eq!(s.status_bars.bottom.right, vec!["clock".to_string()], "a [status_bars] table wins");
        assert_eq!(parse_settings(b"language = \"da\"\n").unwrap().status_bars, crate::prefs::StatusBars::default());
        let s = parse_settings(b"[reader]\nheader = \"book\"\n").unwrap();
        assert!(s.status_bars.top.enabled && s.status_bars.top.center == vec!["book_title".to_string()]);
    }

    #[test]
    fn status_bars_written_by_the_first_mapping_get_the_top_bar_once() {
        // The device file of 2026-11-01 after the first mapping and the user's own edits
        // (battery added bottom left, text size 22): no `version` key, top bar off.
        let text = "[reader]\nfooter = \"full\"\nheader = \"none\"\npage_animation = \"off\"\n\n[status_bars.top]\nenabled = false\nleft = [\"clock\"]\ncenter = [\"chapter_title\"]\nright = [\"battery\"]\nprogress_bar = \"none\"\nfont_px = 26.0\nseparator = \" · \"\n\n[status_bars.bottom]\nenabled = true\nleft = [\"time_left_chapter\", \"battery\"]\ncenter = []\nright = [\"percent\"]\nprogress_bar = \"chapters\"\nfont_px = 22.0\nseparator = \" · \"\n";
        let s = parse_settings(text.as_bytes()).unwrap();
        let bars = crate::prefs::StatusBars::default();
        assert_eq!(s.status_bars.top, bars.top, "top bar on with the defaults");
        assert_eq!(s.status_bars.bottom.left, vec!["time_left_chapter".to_string(), "battery".to_string()]);
        assert_eq!(s.status_bars.bottom.font_px, 22.0, "the user's bottom bar is kept");
        assert_eq!(s.status_bars.version, 2);
        // Saved with version = 2: the user may now turn the top bar off and it stays off.
        let mut off = s.clone();
        off.status_bars.top.enabled = false;
        let again = parse_settings(toml::to_string_pretty(&off).unwrap().as_bytes()).unwrap();
        assert!(!again.status_bars.top.enabled, "repaired once only");
        // A version-1 top bar the user changed is left alone.
        let changed = text.replace("right = [\"battery\"]", "right = [\"wifi\"]");
        assert!(!parse_settings(changed.as_bytes()).unwrap().status_bars.top.enabled);
    }

    #[test]
    fn device_file_of_2026_11_01_loads_with_new_defaults() {
        // /mnt/us/rinky/settings.toml from the user's Kindle (no secrets in it).
        let text = "library_dirs = [\n    \"/mnt/us/Books\",\n    \"/mnt/us/documents\",\n]\nlanguage = \"en\"\n\n[reader]\nfont_px = 40.0\nline_spacing = 1.4\nmargin_x = 70.0\nmargin_top = 70.0\nmargin_bottom = 110.0\njustify = true\nflash_every = 8\nfooter = \"full\"\nheader = \"none\"\npage_animation = \"off\"\n\n[device]\ntouch = \"\"\nnight_mode = false\nrotation = 0\nsleep_minutes = 10\nscreensaver = \"cover\"\n\n[plugins]\n";
        let s = parse_settings(text.as_bytes()).unwrap();
        assert_eq!(s.reader.page_animation, "off");
        assert_eq!(s.device.screensaver, "cover");
        assert_eq!(s.gestures.action("swipe_up_from_bottom"), "reading_settings");
        assert_eq!(s.ui, crate::prefs::UiSettings::default());
        // footer = "full" is the default bottom bar; header = "none" gives the default top
        // bar, on: clock left, chapter title centre, battery right.
        let bars = crate::prefs::StatusBars::default();
        assert_eq!(s.status_bars, bars);
        assert!(s.status_bars.top.enabled);
        assert_eq!((s.status_bars.top.left[0].as_str(), s.status_bars.top.center[0].as_str(), s.status_bars.top.right[0].as_str()), ("clock", "chapter_title", "battery"));
        let text_out = toml::to_string_pretty(&s).unwrap();
        assert!(text_out.contains("[status_bars]\nversion = 2"), "{text_out}");
        let again = parse_settings(toml::to_string_pretty(&s).unwrap().as_bytes()).unwrap();
        assert_eq!(again, s);
        for (old, new) in [("slide", "on"), ("curl", "on"), ("fade", "on"), ("off", "off"), ("on", "on")] {
            let s = parse_settings(format!("[reader]\npage_animation = \"{old}\"\n").as_bytes()).unwrap();
            assert_eq!(s.reader.page_animation, new, "{old}");
        }
        let g = parse_settings(b"[gestures]\npinch_in = \"zoom_out\"\n").unwrap();
        assert_eq!((g.gestures.action("pinch_in"), g.gestures.action("pinch_out")), ("zoom_out", "font_bigger"));
    }
}
