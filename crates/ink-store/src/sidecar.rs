//! Per-book state next to the book:
//! `<dir>/<name without extension>.sdr/rinky.<extension>.json`.
//!
//! The `.sdr` directory is the one KOReader uses (`DocSettings:getSidecarDir`), so both
//! readers keep their files side by side: KOReader's `metadata.epub.lua`, ours
//! `rinky.epub.json`. We never write KOReader's files. As in KOReader, the file is
//! named after the book's extension (case kept, `_` for none), because two formats of
//! one title in one folder (`Title.epub` and `Title.pdf`, as Calibre stores them) share
//! `Title.sdr`. File name bytes are kept as they are (Linux names need not be UTF-8).
//!
//! An `rinky.json` from phase 0 and the first wave A build (one file per `.sdr`) is
//! still read when the book has no file of its own yet, but only if it belongs to this
//! book: its `md5` equals the book's partial MD5, or it has no `md5` and no other book
//! file (an extension the library lists) in the folder maps to the same `.sdr`. It is
//! never written, moved or deleted.
//!
//! Format (schema [`BookState::VERSION`]): JSON with the fields of [`BookState`]. Unknown
//! keys are kept ([`BookState::extra`]) so an older rinky does not drop fields a newer
//! one wrote. Positions are `ink_doc::Position` strings: crengine xpointers
//! (`/body/DocFragment[3]/body/p[2]/text().42`) for reflowable books, `page:N` or
//! `page:N;x0,y0,x1,y1;...` (0-based page, page points) for fixed-layout books.
//!
//! Durability: [`BookState::save`] writes through [`crate::save_with_backup`] (temp file,
//! fsync, rename; `rinky.<ext>.json.old` refreshed at most once a minute). It never
//! writes bytes that [`BookState::load`] would refuse: non-finite numbers are dropped and
//! the result is parsed before it is written.
//! [`BookState::load`] never fails on bad content: a file that does not parse is moved to
//! `rinky.<ext>.json.corrupt-<time>-<n>` and the backup, then the old shared
//! `rinky.json`, then the prototype's `BOOK.rinky` position, then the defaults
//! are used.

use crate::{load_recovering, os_bytes, os_from_bytes, parse_json, save_with_backup, LoadSource, Recovered, Result, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The old file name inside the `.sdr` directory (phase 0 and the first wave A build):
/// one file for every book with the same stem. Only read, as a fallback (see the module
/// documentation); [`sidecar_path`] gives the current name.
pub const SIDECAR_FILE: &str = "rinky.json";

/// Splits a file name at its last `.` into stem and extension (bytes kept). A name
/// without a `.` after its first byte (`README`, `.hidden`) has no extension.
fn split_name(name: &OsStr) -> (OsString, Option<OsString>) {
    let b = os_bytes(name);
    match b.iter().rposition(|&c| c == b'.') {
        Some(i) if i > 0 => (os_from_bytes(b[..i].to_vec()), Some(os_from_bytes(b[i + 1..].to_vec()))),
        _ => (name.to_os_string(), None),
    }
}

/// Sidecar directory of a book: `/mnt/us/Books/juliet.epub` -> `/mnt/us/Books/juliet.sdr`.
/// The last extension of the file name is dropped; a name without one (or a dot file)
/// keeps its whole name. The name's bytes are kept (no UTF-8 needed).
pub fn sidecar_dir(book: &Path) -> PathBuf {
    let name = book.file_name().unwrap_or(OsStr::new("book"));
    let (mut stem, _) = split_name(name);
    stem.push(".sdr");
    book.with_file_name(stem)
}

/// Our file name inside the `.sdr` directory: `rinky.<extension>.json`, extension
/// case kept, `_` when the name has none (`rinky._.json`), like KOReader's
/// `metadata.<extension>.lua`.
pub fn sidecar_file_name(book: &Path) -> OsString {
    let ext = book.file_name().and_then(|n| split_name(n).1).filter(|e| !e.is_empty()).unwrap_or_else(|| OsString::from("_"));
    let mut name = OsString::from("rinky.");
    name.push(ext);
    name.push(".json");
    name
}

/// Path of our sidecar file for `book`: `<stem>.sdr/rinky.<extension>.json`.
pub fn sidecar_path(book: &Path) -> PathBuf {
    sidecar_dir(book).join(sidecar_file_name(book))
}

/// Path of the old shared sidecar file (`<stem>.sdr/rinky.json`), read only as a
/// fallback.
pub fn legacy_sidecar_path(book: &Path) -> PathBuf {
    sidecar_dir(book).join(SIDECAR_FILE)
}

/// Reading status, as KOReader's `summary.status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReadingStatus {
    /// Not opened yet.
    #[default]
    New,
    /// Being read.
    Reading,
    /// Finished.
    Complete,
    /// Put aside ("on hold" in KOReader's menus).
    Abandoned,
}

impl ReadingStatus {
    /// KOReader's `summary.status` value (`reading`, `complete`, `abandoned`); unknown
    /// values give `None`.
    pub fn from_koreader(s: &str) -> Option<ReadingStatus> {
        match s {
            "reading" => Some(ReadingStatus::Reading),
            "complete" | "finished" => Some(ReadingStatus::Complete),
            "abandoned" | "on_hold" | "on hold" => Some(ReadingStatus::Abandoned),
            "new" | "unread" => Some(ReadingStatus::New),
            _ => None,
        }
    }

    /// The lowercase name used in files.
    pub fn as_str(self) -> &'static str {
        match self {
            ReadingStatus::New => "new",
            ReadingStatus::Reading => "reading",
            ReadingStatus::Complete => "complete",
            ReadingStatus::Abandoned => "abandoned",
        }
    }
}

/// Reads a status leniently: KOReader's and our names map as in
/// [`ReadingStatus::from_koreader`]; anything else (a value from a newer version, a
/// wrong type) gives `New` instead of making the whole file unreadable.
pub(crate) fn lenient_status<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<ReadingStatus, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(v.as_str().and_then(ReadingStatus::from_koreader).unwrap_or_default())
}

/// Reads an optional value leniently: a value of the wrong type (`"columns": "2"`,
/// `"rtl": 1`, a number out of range) reads as unset instead of making the whole file
/// unreadable. For keys added after files were written, which may hold anything a
/// screen or another tool put under that name while it was an unknown key.
pub(crate) fn lenient<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned>(d: D) -> std::result::Result<Option<T>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(serde_json::from_value(v).ok())
}

/// A rectangle on a fixed-layout page in page points (MuPDF page space: origin at the
/// top left, y down). Same fields and JSON as `ink_doc::Quad`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quad {
    /// Left.
    pub x0: f32,
    /// Top.
    pub y0: f32,
    /// Right.
    pub x1: f32,
    /// Bottom.
    pub y1: f32,
}

/// A highlight, with or without a note.
///
/// Reflowable books: `start`/`end` are xpointers and `page`/`quads` are unset.
/// Fixed-layout books: `page` is the 0-based page, `quads` the highlighted boxes, and
/// `start`/`end` the `page:N;x0,y0,x1,y1` strings of the first and last box.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Highlight {
    /// Start position (`ink_doc::Position` string form).
    pub start: String,
    /// End position.
    pub end: String,
    /// Highlighted text.
    pub text: String,
    /// Attached note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Style name (`lighten`, `underscore`, `strikeout`, `invert`).
    pub style: String,
    /// Color name (`gray`, `yellow`, ...); `None` = the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Chapter title at the highlight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapter: Option<String>,
    /// Creation time, `YYYY-MM-DD HH:MM:SS` like KOReader.
    pub created: String,
    /// Last edit time (text or note), same format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// 0-based page (fixed-layout books).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// Boxes on `page` (fixed-layout books).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub quads: Vec<Quad>,
}

impl Quad {
    /// True if every coordinate is a finite number (JSON cannot store NaN or infinity).
    pub fn is_finite(&self) -> bool {
        self.x0.is_finite() && self.y0.is_finite() && self.x1.is_finite() && self.y1.is_finite()
    }
}

impl Highlight {
    /// True if a note is attached.
    pub fn has_note(&self) -> bool {
        self.note.as_deref().is_some_and(|n| !n.is_empty())
    }
}

/// A bookmark.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    /// Position string.
    pub position: String,
    /// Label shown in the list.
    pub label: String,
    /// Creation time.
    pub created: String,
    /// Note attached to the bookmark.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Chapter title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chapter: Option<String>,
    /// 0-based page (fixed-layout books).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
}

/// Per-book overrides of the reading settings; `None` = use the global setting.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookSettings {
    /// Body font size in pixels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_px: Option<f32>,
    /// Font family name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    /// Line spacing factor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_spacing: Option<f32>,
    /// Side margins in pixels (from the screen edges).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_x: Option<f32>,
    /// Top margin in pixels (below the top status bar).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_top: Option<f32>,
    /// Bottom margin in pixels (above the bottom status bar).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_bottom: Option<f32>,
    /// How the top and bottom margins are measured
    /// ([`crate::settings::MARGIN_VERSION`]; absent: 1). See
    /// [`BookSettings::migrate_margins`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub margin_version: Option<u32>,
    /// Extra space after paragraphs in em.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paragraph_spacing: Option<f32>,
    /// First-line indent in em; negative: the book's own indent
    /// ([`crate::settings::INDENT_BOOK`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paragraph_indent: Option<f32>,
    /// Darker text, 0..=[`crate::settings::WEIGHT_MAX`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<u8>,
    /// Justify text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justify: Option<bool>,
    /// Hyphenate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hyphenation: Option<bool>,
    /// Hyphenation language override (`en-us`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Use the book's own CSS ("publisher styles").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher_styles: Option<bool>,
    /// Show annotations (the book's `<ruby>` readings and plugins' annotations, above or
    /// below the text).
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub annotations: Option<bool>,
    /// Use the book's embedded fonts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedded_fonts: Option<bool>,
    /// Rotation in quarter turns clockwise (0..=3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u8>,
    /// Night mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub night_mode: Option<bool>,
    /// Flash every N page turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flash_every: Option<u32>,
    /// Fixed layout: zoom mode (`page`, `width`, `height`, `content`) or a factor. The
    /// reader writes the fit, `page` or `width`, and ignores other values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zoom: Option<String>,
    /// Fixed layout: margins cropped away (left, top, right, bottom) as page fractions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crop: Option<[f32; 4]>,
    /// Fixed layout: white margins cropped automatically (detected on each page).
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub crop_auto: Option<bool>,
    /// Fixed layout: text columns read one after the other (1 = off).
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub columns: Option<u8>,
    /// Fixed layout: contrast factor (1 = none).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f32>,
    /// Fixed layout: two pages side by side, `auto` (comics in landscape), `always` or
    /// `never`.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub spreads: Option<String>,
    /// Fixed layout: pages run right to left (manga).
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub rtl: Option<bool>,
    /// Reading progress in the bottom margin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer: Option<crate::settings::FooterStyle>,
    /// Title in the top margin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<crate::settings::HeaderStyle>,
    /// Other keys (screens add their own without a schema change).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Everything we remember about one book.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookState {
    /// Format version of this file.
    pub version: u32,
    /// KOReader partial MD5 of the book file.
    pub md5: Option<String>,
    /// Last reading position (`ink_doc::Position` string form).
    pub position: Option<String>,
    /// Page shown at the last save (0-based, current layout).
    pub page: Option<u32>,
    /// Page count at the last save.
    pub pages: Option<u32>,
    /// Progress 0.0..=1.0 at the last save.
    pub percent: Option<f32>,
    /// Reading status.
    #[serde(deserialize_with = "lenient_status")]
    pub status: ReadingStatus,
    /// Rating 1..=5.
    pub rating: Option<u8>,
    /// Review / book note (KOReader `summary.note`).
    pub review: Option<String>,
    /// Highlights and notes.
    pub highlights: Vec<Highlight>,
    /// Bookmarks.
    pub bookmarks: Vec<Bookmark>,
    /// Seconds since the Unix epoch of the last open.
    pub last_opened: Option<u64>,
    /// Per-book reading settings.
    pub settings: BookSettings,
    /// Data of plugins, by plugin name (each plugin owns its value).
    pub plugins: Map<String, Value>,
    /// Where imported data came from (`koreader:<metadata file>`), if any.
    pub imported_from: Option<String>,
    /// Keys this version does not know, kept as they were.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Which file [`BookState::load_report`] used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateSource {
    /// `rinky.<ext>.json`, or a complete temporary copy of it newer than the backup
    /// whose rename had not happened yet ([`crate::LoadSource::Temp`]).
    Sidecar,
    /// `rinky.<ext>.json.old` (the sidecar was missing or corrupt).
    Backup,
    /// The old shared `rinky.json` (or its `.old`) of phase 0, which belongs to this
    /// book (see the module documentation).
    SharedSidecar,
    /// The prototype's `BOOK.rinky` position file.
    Legacy,
    /// Nothing usable: defaults.
    Default,
}

/// What [`BookState::load_report`] found.
#[derive(Clone, Debug)]
pub struct LoadReport {
    /// The state.
    pub state: BookState,
    /// Where it came from.
    pub source: StateSource,
    /// Corrupt files moved aside (new names).
    pub set_aside: Vec<PathBuf>,
    /// Parse errors of those files.
    pub errors: Vec<String>,
}

/// True if `key` names a typed field of `T` rather than a key its flattened `extra`
/// keeps: a key in `extra` with such a name would be written twice (serde refuses
/// duplicate fields), so `save` drops it. Asked of `T` itself: the key alone, set to
/// `null`, is read into a field or refused by one, or lands in `extra`.
fn is_field<T: serde::de::DeserializeOwned>(key: &str, extra: impl Fn(&T) -> &Map<String, Value>) -> bool {
    let probe = Value::Object(Map::from_iter([(key.to_string(), Value::Null)]));
    serde_json::from_value::<T>(probe).map_or(true, |t| !extra(&t).contains_key(key))
}

fn finite(v: Option<f32>) -> Option<f32> {
    v.filter(|x| x.is_finite())
}

/// Deepest nesting of arrays and objects [`BookState::sanitized`] keeps in a plugin value
/// or an unknown key. serde_json reads at most 128 levels in all, and the sidecar puts
/// a plugin value three levels deep (`{"plugins": {"<plugin>": {"<key>": ...}}}`).
pub const MAX_VALUE_DEPTH: usize = 100;

/// Nesting depth of `v`: 0 for a scalar, 1 for a flat array or object. Walks with its
/// own stack, so a value of any depth is measured without overflowing the thread's.
pub fn value_depth(v: &Value) -> usize {
    let mut max = 0;
    let mut stack = vec![(v, 0)];
    while let Some((v, d)) = stack.pop() {
        match v {
            Value::Array(a) => stack.extend(a.iter().map(|c| (c, d + 1))),
            Value::Object(o) => stack.extend(o.values().map(|c| (c, d + 1))),
            _ => continue,
        }
        max = max.max(d + 1);
    }
    max
}

/// The old shared `rinky.json` (or its `.old`) if it parses and belongs to `book`.
/// Never moved aside or written: it may belong to another format of the same title.
fn shared_sidecar(book: &Path) -> Option<BookState> {
    let file = legacy_sidecar_path(book);
    let state = [file.clone(), crate::backup_path(&file)].iter().find_map(|f| std::fs::read(f).ok().and_then(|b| parse_json::<BookState>(&b).ok()))?;
    let belongs = match state.md5.as_deref() {
        Some(m) => crate::partial_md5(book).is_ok_and(|own| own == m),
        None => !has_sibling_with_same_sdr(book),
    };
    belongs.then_some(state)
}

/// True if another book file (an extension in [`crate::library::BOOK_EXTENSIONS`]) in
/// `book`'s folder maps to the same `.sdr` directory. A folder that cannot be listed
/// counts as having one (the old file is then not adopted).
fn has_sibling_with_same_sdr(book: &Path) -> bool {
    let Some(name) = book.file_name() else { return true };
    let dir = match book.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    let sdr = sidecar_dir(book);
    let Ok(rd) = std::fs::read_dir(dir) else { return true };
    rd.flatten().any(|e| {
        let n = e.file_name();
        n != name && crate::library::is_book_name(Path::new(&n)) && sidecar_dir(&dir.join(&n)) == sdr
    })
}

impl BookSettings {
    /// Converts version 1 top and bottom margin overrides (measured from the screen edge,
    /// the bar inside them) once to be measured from the bars `bars` (the global ones),
    /// so the text stays where it was, and sets [`BookSettings::margin_version`]. Call
    /// after loading, before the settings are used or saved. Returns true when it ran.
    pub fn migrate_margins(&mut self, bars: &crate::prefs::StatusBars) -> bool {
        use crate::settings::{margin_from_bar, MARGIN_VERSION};
        if self.margin_version.unwrap_or(1) >= MARGIN_VERSION {
            return false;
        }
        self.margin_top = self.margin_top.map(|v| margin_from_bar(v, bars.top.room()));
        self.margin_bottom = self.margin_bottom.map(|v| margin_from_bar(v, bars.bottom.room()));
        self.margin_version = Some(MARGIN_VERSION);
        true
    }
}

impl BookState {
    /// Current format version. 1: phase 0. 2: page, pages, review, settings, highlight
    /// color/chapter/updated/page/quads, bookmark note/chapter/page, imported_from.
    pub const VERSION: u32 = 2;

    /// Loads the sidecar of `book` (see [`BookState::load_report`]).
    pub fn load(book: &Path) -> Result<BookState> {
        Ok(Self::load_report(book)?.state)
    }

    /// Loads the sidecar of `book`: `rinky.<ext>.json`, else its `.old`, else the old
    /// shared `rinky.json` if it belongs to this book, else the prototype's
    /// `BOOK.rinky` position, else the defaults. Corrupt files of our own name are
    /// moved aside and listed in the report. Only I/O errors (not bad content) are
    /// errors: also a `.sdr` that is a file, or a sidecar that is a directory.
    pub fn load_report(book: &Path) -> Result<LoadReport> {
        // Every file but the prototype's is in the `.sdr` folder. A book no reader has saved
        // has none, and its files are not looked for one by one (each look is a round trip
        // on the Kindle's FUSE store).
        let sdr = !std::fs::metadata(sidecar_dir(book)).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
        let r = if sdr { load_recovering(&sidecar_path(book), parse_json::<BookState>)? } else { Recovered::default() };
        let (state, source) = match (r.value, r.source) {
            (Some(s), LoadSource::Primary | LoadSource::Temp) => (s, StateSource::Sidecar),
            (Some(s), _) => (s, StateSource::Backup),
            (None, _) => match sdr.then(|| shared_sidecar(book)).flatten() {
                Some(s) => (s, StateSource::SharedSidecar),
                None => match legacy_position(book) {
                    Some(p) => (BookState { position: Some(p), ..BookState::default() }, StateSource::Legacy),
                    None => (BookState::default(), StateSource::Default),
                },
            },
        };
        Ok(LoadReport { state, source, set_aside: r.set_aside, errors: r.errors })
    }

    /// The copy [`BookState::save`] writes: the current version, no non-finite numbers
    /// (JSON has none: serde_json writes `null`, which a plain `f32` refuses), so quads
    /// with one are dropped, a crop with one is unset and optional floats become unset;
    /// no key in `extra` or `settings.extra` that names a typed field (it would be
    /// written twice, and serde refuses duplicate fields); and no plugin value or unknown
    /// key nested deeper than [`MAX_VALUE_DEPTH`] (serde_json would not read it back).
    pub fn sanitized(&self) -> BookState {
        let mut s = self.clone();
        s.version = Self::VERSION;
        s.percent = finite(s.percent);
        let shallow = |_: &String, v: &mut Value| value_depth(v) <= MAX_VALUE_DEPTH;
        s.extra.retain(|k, v| !is_field::<BookState>(k, |s| &s.extra) && shallow(k, v));
        s.plugins.retain(|name, p| match p.as_object_mut() {
            Some(values) => {
                values.retain(shallow);
                true
            }
            None => shallow(name, p),
        });
        for h in &mut s.highlights {
            h.quads.retain(Quad::is_finite);
        }
        let st = &mut s.settings;
        st.extra.retain(|k, v| !is_field::<BookSettings>(k, |s| &s.extra) && shallow(k, v));
        if st.crop.is_some_and(|c| c.iter().any(|v| !v.is_finite())) {
            st.crop = None;
        }
        for f in [&mut st.font_px, &mut st.line_spacing, &mut st.margin_x, &mut st.margin_top, &mut st.margin_bottom, &mut st.contrast] {
            *f = finite(*f);
        }
        for f in [&mut st.margin_x, &mut st.margin_top, &mut st.margin_bottom] {
            *f = f.map(|v| v.clamp(0.0, crate::settings::MARGIN_MAX_PX));
        }
        st.paragraph_spacing = finite(st.paragraph_spacing).map(|v| v.clamp(0.0, 3.0));
        st.paragraph_indent = finite(st.paragraph_indent).map(|v| if v < 0.0 { crate::settings::INDENT_BOOK } else { v.min(6.0) });
        st.font_weight = st.font_weight.map(|v| v.min(crate::settings::WEIGHT_MAX));
        s
    }

    /// Writes the sidecar (temp file, fsync, rename; backup at most once a minute),
    /// creating the `.sdr` directory. Writes [`BookState::sanitized`] and checks that the
    /// bytes load before writing them; [`StoreError::Invalid`] (nothing written) if not.
    pub fn save(&self, book: &Path) -> Result<()> {
        let s = self.sanitized();
        let json = serde_json::to_vec_pretty(&s).map_err(|e| StoreError::Format(e.to_string()))?;
        parse_json::<BookState>(&json).map_err(|e| StoreError::Invalid(format!("sidecar for {} would not load again: {e}", book.display())))?;
        save_with_backup(&sidecar_path(book), &json, |b| parse_json::<BookState>(b).is_ok())
    }

    /// Adds a highlight; one with the same start and end replaces the old one.
    pub fn add_highlight(&mut self, h: Highlight) {
        match self.highlights.iter_mut().find(|x| x.start == h.start && x.end == h.end && x.page == h.page) {
            Some(old) => *old = h,
            None => self.highlights.push(h),
        }
    }

    /// Removes the highlight at `start`..`end`; returns whether one was removed.
    pub fn remove_highlight(&mut self, start: &str, end: &str) -> bool {
        let n = self.highlights.len();
        self.highlights.retain(|h| !(h.start == start && h.end == end));
        n != self.highlights.len()
    }

    /// Highlights that carry a note.
    pub fn notes(&self) -> impl Iterator<Item = &Highlight> {
        self.highlights.iter().filter(|h| h.has_note())
    }

    /// Adds a bookmark unless one exists at the same position; returns whether it was added.
    pub fn add_bookmark(&mut self, b: Bookmark) -> bool {
        if self.bookmarks.iter().any(|x| x.position == b.position) {
            return false;
        }
        self.bookmarks.push(b);
        true
    }

    /// Removes the bookmark at `position`; returns whether one was removed.
    pub fn remove_bookmark(&mut self, position: &str) -> bool {
        let n = self.bookmarks.len();
        self.bookmarks.retain(|b| b.position != position);
        n != self.bookmarks.len()
    }

    /// Per-book value `key` of plugin `plugin`.
    pub fn plugin_get(&self, plugin: &str, key: &str) -> Option<&Value> {
        self.plugins.get(plugin)?.as_object()?.get(key)
    }

    /// Sets per-book value `key` of plugin `plugin` (`null` removes it). The reader owns
    /// the `BookState` of the open book and saves it; plugins write through that copy so
    /// their values and the reading position are saved together.
    pub fn plugin_set(&mut self, plugin: &str, key: &str, value: Value) {
        let entry = self.plugins.entry(plugin.to_string()).or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        let obj = entry.as_object_mut().expect("object");
        if value.is_null() {
            obj.remove(key);
        } else {
            obj.insert(key.to_string(), value);
        }
    }
}

/// The prototype's position file `BOOK.epub.rinky` (`chapter block word`), as a
/// `flow:` position string.
pub fn legacy_position(book: &Path) -> Option<String> {
    let text = std::fs::read_to_string(crate::with_suffix(book, ".rinky")).ok()?;
    let v: Vec<u32> = text.split_whitespace().filter_map(|t| t.parse().ok()).collect();
    match v[..] {
        [chapter, block, word] => Some(format!("flow:{chapter}.{block}.{word}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn sidecar_paths_match_koreader() {
        assert_eq!(sidecar_dir(Path::new("/mnt/us/Books/juliet.epub")), PathBuf::from("/mnt/us/Books/juliet.sdr"));
        assert_eq!(sidecar_dir(Path::new("/b/a.fb2.zip")), PathBuf::from("/b/a.fb2.sdr"));
        assert_eq!(sidecar_dir(Path::new("/b/.hidden")), PathBuf::from("/b/.hidden.sdr"));
        assert_eq!(sidecar_path(Path::new("x.epub")), PathBuf::from("x.sdr/rinky.epub.json"));
        // One file per extension, case kept, `_` for none (KOReader's metadata.<ext>.lua).
        assert_eq!(sidecar_path(Path::new("/b/T.pdf")), PathBuf::from("/b/T.sdr/rinky.pdf.json"));
        assert_eq!(sidecar_path(Path::new("/b/T.EPUB")), PathBuf::from("/b/T.sdr/rinky.EPUB.json"));
        assert_eq!(sidecar_path(Path::new("/b/a.fb2.zip")), PathBuf::from("/b/a.fb2.sdr/rinky.zip.json"));
        assert_eq!(sidecar_path(Path::new("/b/README")), PathBuf::from("/b/README.sdr/rinky._.json"));
        assert_eq!(sidecar_path(Path::new("/b/.hidden")), PathBuf::from("/b/.hidden.sdr/rinky._.json"));
        assert_eq!(sidecar_path(Path::new("/b/name.")), PathBuf::from("/b/name.sdr/rinky._.json"));
        assert_eq!(legacy_sidecar_path(Path::new("/b/T.pdf")), PathBuf::from("/b/T.sdr/rinky.json"));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_names_keep_their_bytes() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let p = |b: &[u8]| PathBuf::from(OsStr::from_bytes(b));
        assert_eq!(sidecar_dir(&p(b"/b/caf\xe9.epub")), p(b"/b/caf\xe9.sdr"));
        assert_eq!(sidecar_path(&p(b"/b/x.\xe9pub")), p(b"/b/x.sdr/rinky.\xe9pub.json"));
        assert_ne!(sidecar_dir(&p(b"/b/caf\xe9.epub")), sidecar_dir(&p(b"/b/na\xefve.epub")));
    }

    #[test]
    fn typed_fields_are_told_from_extra_keys() {
        let keys = |v: serde_json::Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        let full_settings = BookSettings {
            font_px: Some(1.0),
            font_family: Some("f".into()),
            line_spacing: Some(1.0),
            margin_x: Some(1.0),
            margin_top: Some(1.0),
            margin_bottom: Some(1.0),
            margin_version: Some(2),
            paragraph_spacing: Some(0.5),
            paragraph_indent: Some(1.5),
            font_weight: Some(1),
            justify: Some(true),
            hyphenation: Some(true),
            language: Some("en".into()),
            publisher_styles: Some(true),
            annotations: Some(false),
            embedded_fonts: Some(true),
            rotation: Some(1),
            night_mode: Some(true),
            flash_every: Some(1),
            zoom: Some("page".into()),
            crop: Some([0.0; 4]),
            crop_auto: Some(true),
            columns: Some(2),
            contrast: Some(1.0),
            spreads: Some("auto".into()),
            rtl: Some(true),
            footer: Some(crate::settings::FooterStyle::Pages),
            header: Some(crate::settings::HeaderStyle::Chapter),
            extra: Map::new(),
        };
        assert!(keys(serde_json::to_value(&full_settings).unwrap()).iter().all(|k| is_field::<BookSettings>(k, |s| &s.extra)));
        let mut full = BookState {
            version: 2,
            md5: Some("m".into()),
            position: Some("p".into()),
            page: Some(1),
            pages: Some(2),
            percent: Some(0.5),
            rating: Some(3),
            review: Some("r".into()),
            last_opened: Some(1),
            imported_from: Some("i".into()),
            settings: full_settings,
            ..Default::default()
        };
        full.plugins.insert("p".into(), serde_json::json!(1));
        full.highlights.push(Highlight::default());
        full.bookmarks.push(Bookmark::default());
        assert!(keys(serde_json::to_value(&full).unwrap()).iter().all(|k| is_field::<BookState>(k, |s| &s.extra)));
        for (key, of_state, of_settings) in [("font_px", false, true), ("version", true, false), ("highlight", false, false), ("", false, false)] {
            assert_eq!((is_field::<BookState>(key, |s| &s.extra), is_field::<BookSettings>(key, |s| &s.extra)), (of_state, of_settings), "{key}");
        }
    }

    #[test]
    fn old_margin_overrides_convert_once_and_survive_a_save() {
        let dir = TempDir::new("sidecar-margins");
        let book = dir.path().join("m.epub");
        std::fs::create_dir_all(sidecar_dir(&book)).unwrap();
        // Written before the margins were measured from the bars: no margin_version.
        std::fs::write(sidecar_path(&book), r#"{"settings":{"margin_x":70.0,"margin_top":70.0,"margin_bottom":110.0}}"#).unwrap();
        let bars = crate::prefs::StatusBars::default();
        assert_eq!((bars.top.room(), bars.bottom.room()), (53.0, 81.0));
        let mut s = BookState::load(&book).unwrap();
        assert_eq!(s.settings.margin_version, None);
        assert!(s.settings.migrate_margins(&bars));
        assert_eq!((s.settings.margin_x, s.settings.margin_top, s.settings.margin_bottom, s.settings.margin_version), (Some(70.0), Some(17.0), Some(29.0), Some(2)));
        s.save(&book).unwrap();
        let mut back = BookState::load(&book).unwrap();
        assert!(!back.settings.migrate_margins(&bars), "converted only once");
        assert_eq!((back.settings.margin_top, back.settings.margin_bottom), (Some(17.0), Some(29.0)));
        // Never below 0; absent overrides stay absent; a bar that is off takes no room.
        let mut off = crate::prefs::StatusBars::default();
        off.bottom.enabled = false;
        let mut b = BookSettings { margin_top: Some(30.0), ..BookSettings::default() };
        assert!(b.migrate_margins(&off));
        assert_eq!((b.margin_top, b.margin_bottom), (Some(0.0), None));
        let mut c = BookSettings { margin_bottom: Some(110.0), ..BookSettings::default() };
        c.migrate_margins(&off);
        assert_eq!(c.margin_bottom, Some(110.0));
    }

    #[test]
    fn save_never_writes_what_load_refuses() {
        let dir = TempDir::new("sidecar-sanitize");
        let book = dir.path().join("s.pdf");
        let mut s = BookState { position: Some("page:3".into()), percent: Some(f32::NAN), ..Default::default() };
        s.add_highlight(Highlight {
            start: "a".into(),
            end: "b".into(),
            page: Some(3),
            quads: vec![Quad { x0: 1.0, y0: 2.0, x1: 3.0, y1: 4.0 }, Quad { x0: f32::NAN, y0: 0.0, x1: f32::INFINITY, y1: 1.0 }],
            ..Default::default()
        });
        s.settings.crop = Some([f32::NAN, f32::NEG_INFINITY, 0.1, 0.1]);
        s.settings.font_px = Some(f32::INFINITY);
        s.settings.line_spacing = Some(1.2);
        s.settings.extra.insert("font_px".into(), serde_json::json!("big"));
        s.settings.extra.insert("page_curl".into(), serde_json::json!(2));
        s.extra.insert("highlights".into(), serde_json::json!("not a list"));
        s.extra.insert("future".into(), serde_json::json!({"x": 1}));
        // A value as deep as serde_json reads back in the file stays; one level more goes.
        let nested = |depth: usize| (0..depth).fold(serde_json::json!(1), |v, _| serde_json::json!([v]));
        assert_eq!((value_depth(&nested(0)), value_depth(&nested(3)), value_depth(&serde_json::json!({"a": [1, {}]}))), (0, 3, 3));
        s.plugin_set("p", "ok", nested(MAX_VALUE_DEPTH));
        s.plugin_set("p", "deep", nested(MAX_VALUE_DEPTH + 1));
        s.save(&book).unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert_eq!((r.source, r.set_aside.len()), (StateSource::Sidecar, 0), "{:?}", r.errors);
        let back = r.state;
        assert_eq!(back.highlights[0].quads, [Quad { x0: 1.0, y0: 2.0, x1: 3.0, y1: 4.0 }]);
        assert_eq!((back.percent, back.settings.crop, back.settings.font_px, back.settings.line_spacing), (None, None, None, Some(1.2)));
        assert_eq!(back.settings.extra.get("page_curl"), Some(&serde_json::json!(2)));
        assert!(!back.settings.extra.contains_key("font_px"));
        assert_eq!(back.extra.get("future"), Some(&serde_json::json!({"x": 1})));
        assert!(!back.extra.contains_key("highlights"));
        assert!(back.plugin_get("p", "ok").is_some() && back.plugin_get("p", "deep").is_none());
        assert_eq!(back, s.sanitized());
    }

    #[test]
    fn two_formats_of_one_title_and_the_old_shared_file() {
        let dir = TempDir::new("sidecar-formats");
        let epub = dir.path().join("T.epub");
        let pdf = dir.path().join("T.pdf");
        std::fs::write(&epub, b"epub").unwrap();
        // Phase 0 wrote one shared rinky.json without md5. While only the EPUB is
        // there, it is the EPUB's.
        std::fs::create_dir_all(sidecar_dir(&epub)).unwrap();
        std::fs::write(legacy_sidecar_path(&epub), br#"{"version":1,"position":"flow:1.2.3"}"#).unwrap();
        let r = BookState::load_report(&epub).unwrap();
        assert_eq!((r.source, r.state.position.as_deref()), (StateSource::SharedSidecar, Some("flow:1.2.3")));
        // With a second format beside it, nobody can tell whose it is: not adopted.
        std::fs::write(&pdf, b"pdf").unwrap();
        assert_eq!(BookState::load_report(&epub).unwrap().source, StateSource::Default);
        assert_eq!(BookState::load_report(&pdf).unwrap().source, StateSource::Default);
        // With an md5 it belongs to the book with that hash only.
        let md5 = crate::partial_md5(&pdf).unwrap();
        std::fs::write(legacy_sidecar_path(&epub), format!(r#"{{"version":1,"md5":"{md5}","position":"page:4"}}"#)).unwrap();
        assert_eq!(BookState::load_report(&epub).unwrap().source, StateSource::Default);
        let r = BookState::load_report(&pdf).unwrap();
        assert_eq!((r.source, r.state.position.as_deref()), (StateSource::SharedSidecar, Some("page:4")));
        // Saving writes the book's own file; the shared file is left as it was.
        r.state.save(&pdf).unwrap();
        BookState { position: Some("flow:9.0.0".into()), ..Default::default() }.save(&epub).unwrap();
        assert_eq!(BookState::load(&pdf).unwrap().position.as_deref(), Some("page:4"));
        assert_eq!(BookState::load(&epub).unwrap().position.as_deref(), Some("flow:9.0.0"));
        assert!(std::fs::read_to_string(legacy_sidecar_path(&epub)).unwrap().contains("page:4"));
        let mut names: Vec<String> = std::fs::read_dir(sidecar_dir(&epub)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["rinky.epub.json", "rinky.json", "rinky.pdf.json"]);
    }

    /// Without a `.sdr` folder only the prototype's file is read; a `.sdr` that is a file
    /// is still an error, and an empty folder gives the defaults.
    #[test]
    fn a_missing_or_odd_sdr_folder() {
        let dir = TempDir::new("sidecar-sdr");
        let book = dir.path().join("s.epub");
        std::fs::write(dir.path().join("s.epub.rinky"), "1 2 3").unwrap();
        assert_eq!(BookState::load_report(&book).unwrap().source, StateSource::Legacy);
        std::fs::write(sidecar_dir(&book), b"not a folder").unwrap();
        assert!(BookState::load(&book).is_err());
        std::fs::remove_file(sidecar_dir(&book)).unwrap();
        std::fs::remove_file(dir.path().join("s.epub.rinky")).unwrap();
        std::fs::create_dir(sidecar_dir(&book)).unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert_eq!((r.state, r.source), (BookState::default(), StateSource::Default));
    }

    #[test]
    fn save_load_and_legacy_fallback() {
        let dir = TempDir::new("sidecar");
        let book = dir.path().join("b.epub");
        std::fs::write(&book, b"x").unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert_eq!((r.state, r.source), (BookState::default(), StateSource::Default));
        std::fs::write(dir.path().join("b.epub.rinky"), "3 10 5\n").unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert_eq!(r.state.position.as_deref(), Some("flow:3.10.5"));
        assert_eq!(r.source, StateSource::Legacy);
        let mut s = BookState { position: Some("flow:1.2.3".into()), status: ReadingStatus::Reading, ..Default::default() };
        s.plugins.insert("stats".into(), serde_json::json!({"seconds": 12}));
        s.save(&book).unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert_eq!(r.source, StateSource::Sidecar);
        let back = r.state;
        assert_eq!(back.position.as_deref(), Some("flow:1.2.3"));
        assert_eq!(back.version, BookState::VERSION);
        assert_eq!(back.plugins["stats"]["seconds"], 12);
    }

    #[test]
    fn full_schema_round_trip() {
        let dir = TempDir::new("sidecar-full");
        let book = dir.path().join("Some Book.pdf");
        let mut s = BookState {
            md5: Some("41cce710f34e5ec21315e19c99821415".into()),
            position: Some("page:3".into()),
            page: Some(3),
            pages: Some(10),
            percent: Some(0.4),
            status: ReadingStatus::Complete,
            rating: Some(4),
            review: Some("Good.".into()),
            last_opened: Some(1_760_000_000),
            ..Default::default()
        };
        s.add_highlight(Highlight {
            start: "/body/DocFragment[2]/body/p[3]/text().4".into(),
            end: "/body/DocFragment[2]/body/p[3]/text().20".into(),
            text: "some words".into(),
            note: Some("a note".into()),
            style: "lighten".into(),
            color: Some("yellow".into()),
            chapter: Some("Chapter 1".into()),
            created: "2026-10-24 12:00:00".into(),
            ..Default::default()
        });
        s.add_highlight(Highlight {
            start: "page:2;10,20,110,32".into(),
            end: "page:2;10,40,60,52".into(),
            text: "pdf words".into(),
            style: "underscore".into(),
            page: Some(2),
            quads: vec![Quad { x0: 10.0, y0: 20.0, x1: 110.0, y1: 32.0 }, Quad { x0: 10.0, y0: 40.0, x1: 60.0, y1: 52.0 }],
            ..Default::default()
        });
        // Same range again: replaced, not duplicated.
        s.add_highlight(Highlight { start: "page:2;10,20,110,32".into(), end: "page:2;10,40,60,52".into(), page: Some(2), text: "pdf words!".into(), ..Default::default() });
        assert_eq!(s.highlights.len(), 2);
        assert!(s.add_bookmark(Bookmark { position: "page:5".into(), label: "p. 6".into(), note: Some("here".into()), page: Some(5), ..Default::default() }));
        assert!(!s.add_bookmark(Bookmark { position: "page:5".into(), ..Default::default() }));
        s.settings.font_px = Some(36.0);
        s.settings.crop = Some([0.05, 0.1, 0.05, 0.1]);
        s.settings.extra.insert("page_curl".into(), serde_json::json!(2));
        s.plugin_set("insightful", "chats", serde_json::json!(["c1"]));
        s.plugin_set("stats", "seconds", serde_json::json!(5));
        s.plugin_set("stats", "seconds", Value::Null);
        s.save(&book).unwrap();
        let back = BookState::load(&book).unwrap();
        assert_eq!(back.version, 2);
        let mut expect = s.clone();
        expect.version = 2;
        assert_eq!(back, expect);
        assert_eq!(back.notes().count(), 1);
        assert_eq!(back.plugin_get("insightful", "chats").unwrap()[0], "c1");
        assert!(back.plugin_get("stats", "seconds").is_none());
        let mut edited = back.clone();
        assert!(edited.remove_highlight("page:2;10,20,110,32", "page:2;10,40,60,52"));
        assert!(!edited.remove_highlight("page:2;10,20,110,32", "page:2;10,40,60,52"));
        assert!(edited.remove_bookmark("page:5"));
        // The JSON is readable and names the schema version.
        let text = std::fs::read_to_string(sidecar_path(&book)).unwrap();
        assert!(text.contains("\"version\": 2"), "{text}");
    }

    #[test]
    fn version_1_files_and_unknown_keys() {
        let dir = TempDir::new("sidecar-v1");
        let book = dir.path().join("old.epub");
        std::fs::create_dir_all(sidecar_dir(&book)).unwrap();
        // A phase 0 file plus a key from a hypothetical newer version.
        std::fs::write(
            sidecar_path(&book),
            r#"{"version":1,"md5":null,"position":"flow:0.1.2","percent":null,"status":"reading","rating":null,
               "highlights":[{"start":"a","end":"b","text":"t","style":"","created":""}],"bookmarks":[],
               "last_opened":5,"plugins":{},"future_field":{"x":1}}"#,
        )
        .unwrap();
        let s = BookState::load(&book).unwrap();
        assert_eq!((s.version, s.position.as_deref(), s.status), (1, Some("flow:0.1.2"), ReadingStatus::Reading));
        assert_eq!(s.highlights[0].text, "t");
        assert_eq!(s.extra["future_field"]["x"], 1);
        s.save(&book).unwrap();
        let again = BookState::load(&book).unwrap();
        assert_eq!(again.version, 2);
        assert_eq!(again.extra["future_field"]["x"], 1, "unknown keys survive a save");
        // A status value this version does not know is read as `new`, not as corruption.
        std::fs::write(sidecar_path(&book), r#"{"version":3,"position":"flow:0.1.2","status":"wishlist"}"#).unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert_eq!((r.source, r.state.status, r.state.position.as_deref()), (StateSource::Sidecar, ReadingStatus::New, Some("flow:0.1.2")));
        assert!(r.set_aside.is_empty());
    }

    #[test]
    fn corruption_is_moved_aside_and_backup_used() {
        let dir = TempDir::new("sidecar-corrupt");
        let book = dir.path().join("c.epub");
        let s1 = BookState { position: Some("flow:1.0.0".into()), ..Default::default() };
        s1.save(&book).unwrap();
        let s2 = BookState { position: Some("flow:2.0.0".into()), ..Default::default() };
        s2.save(&book).unwrap();
        // Power loss can leave a zero-length or truncated file.
        std::fs::write(sidecar_path(&book), b"{\"version\": 2, \"posi").unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert_eq!(r.source, StateSource::Backup);
        assert_eq!(r.state.position.as_deref(), Some("flow:1.0.0"));
        assert_eq!(r.set_aside.len(), 1);
        assert_eq!(std::fs::read(&r.set_aside[0]).unwrap(), b"{\"version\": 2, \"posi");
        // `load` (what the reader calls) gives the same state, never an error.
        assert_eq!(BookState::load(&book).unwrap().position.as_deref(), Some("flow:1.0.0"));
        // Both files corrupt: defaults, both kept aside.
        std::fs::write(sidecar_path(&book), b"").unwrap();
        std::fs::write(crate::backup_path(&sidecar_path(&book)), b"[").unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert_eq!(r.source, StateSource::Default);
        assert_eq!(r.errors.len(), 2);
        let aside = std::fs::read_dir(sidecar_dir(&book)).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".corrupt-")).count();
        assert_eq!(aside, 3, "first corrupt file, then the empty file and the bad backup");
    }

    /// The fixed-layout view (fit, crop, columns, contrast, spreads, right to left)
    /// survives a save, a file without it reads as unset, and an older file that kept
    /// `columns` as an unknown key now reads it as the typed value.
    #[test]
    fn fixed_layout_view_round_trip() {
        let dir = TempDir::new("sidecar-fixed");
        let book = dir.path().join("scan.pdf");
        let mut s = BookState::default();
        let st = &mut s.settings;
        (st.zoom, st.crop_auto, st.columns, st.contrast, st.spreads, st.rtl) =
            (Some("width".into()), Some(true), Some(2), Some(1.5), Some("never".into()), Some(true));
        s.save(&book).unwrap();
        let back = BookState::load(&book).unwrap().settings;
        assert_eq!(
            (back.zoom.as_deref(), back.crop_auto, back.columns, back.contrast, back.spreads.as_deref(), back.rtl),
            (Some("width"), Some(true), Some(2), Some(1.5), Some("never"), Some(true))
        );
        assert!(back.extra.is_empty(), "typed, not unknown keys: {:?}", back.extra);
        let text = std::fs::read_to_string(sidecar_path(&book)).unwrap();
        assert!(text.contains(r#""crop_auto": true"#) && text.contains(r#""rtl": true"#), "{text}");
        // Unset values are not written at all (an EPUB's sidecar stays as it was).
        BookState::default().save(&book).unwrap();
        let text = std::fs::read_to_string(sidecar_path(&book)).unwrap();
        assert!(!text.contains("columns") && !text.contains("spreads"), "{text}");
        std::fs::write(sidecar_path(&book), r#"{"version":2,"settings":{"columns":2,"contrast":1.25}}"#).unwrap();
        let old = BookState::load(&book).unwrap().settings;
        assert_eq!((old.columns, old.contrast, old.rtl), (Some(2), Some(1.25), None));
        // Something else kept under one of the new names while it was an unknown key
        // reads as unset; the rest of the file (the position) still loads.
        let odd = r#"{"version":2,"position":"page:7","settings":{"columns":"two","rtl":1,"spreads":true,"crop_auto":null,"font_px":30}}"#;
        std::fs::write(sidecar_path(&book), odd).unwrap();
        let r = BookState::load_report(&book).unwrap();
        assert!(r.set_aside.is_empty(), "{:?}", r.set_aside);
        let st = r.state;
        assert_eq!(st.position.as_deref(), Some("page:7"));
        assert_eq!((st.settings.columns, st.settings.rtl, st.settings.spreads, st.settings.crop_auto, st.settings.font_px), (None, None, None, None, Some(30.0)));
    }

    /// A book's own "show annotations" choice survives a save; unset is not written, and a
    /// value of another type reads as unset.
    #[test]
    fn annotations_choice_round_trip() {
        let dir = TempDir::new("sidecar-ruby");
        let book = dir.path().join("poems.epub");
        let mut s = BookState::default();
        s.settings.annotations = Some(false);
        s.save(&book).unwrap();
        assert_eq!(BookState::load(&book).unwrap().settings.annotations, Some(false));
        assert!(std::fs::read_to_string(sidecar_path(&book)).unwrap().contains(r#""annotations": false"#));
        BookState::default().save(&book).unwrap();
        assert!(!std::fs::read_to_string(sidecar_path(&book)).unwrap().contains("annotations"));
        std::fs::write(sidecar_path(&book), r#"{"version":2,"settings":{"annotations":"off","font_px":30}}"#).unwrap();
        let st = BookState::load(&book).unwrap().settings;
        assert_eq!((st.annotations, st.font_px), (None, Some(30.0)));
    }

    #[test]
    fn koreader_status_names() {
        assert_eq!(ReadingStatus::from_koreader("complete"), Some(ReadingStatus::Complete));
        assert_eq!(ReadingStatus::from_koreader("abandoned"), Some(ReadingStatus::Abandoned));
        assert_eq!(ReadingStatus::from_koreader("reading"), Some(ReadingStatus::Reading));
        assert_eq!(ReadingStatus::from_koreader("tbr"), None);
        assert_eq!(ReadingStatus::Abandoned.as_str(), "abandoned");
    }
}
