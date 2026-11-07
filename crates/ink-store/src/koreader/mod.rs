//! Read-only import from KOReader (phase 4).
//!
//! Sources, never modified (files are only opened for reading; databases are read from
//! a private copy, see [`sqlite`]):
//! - `<book>.sdr/metadata.<ext>.lua`: a Lua table literal (`return { ... }`) with
//!   `last_xpointer` (crengine xpointer) or `last_page`, `percent_finished`,
//!   `annotations` (highlights, notes, bookmarks with `pos0`/`pos1`/`text`/`note`/`datetime`;
//!   older files: `bookmarks` + `highlight`), `summary.status`, `summary.rating`,
//!   `summary.note`, `doc_props`, `partial_md5_checksum`. Parsed with [`crate::lua`], which
//!   never runs Lua.
//! - `koreader/settings/statistics.sqlite3`: tables `book` and `page_stat_data`.
//! - `koreader/settings/vocabulary_builder.sqlite3`: tables `vocabulary` and `title`.
//!
//! The sidecar is found the way KOReader's `DocSettings:open` does it: the book folder
//! (`<path without last suffix>.sdr/metadata.<suffix>.lua`, its `.old` backup, the legacy
//! `<name>.lua`), then, given KOReader's data directory, `docsettings/<path>.sdr`,
//! `hashdocsettings/<md5[0..2]>/<md5>.sdr` and `history/[dir] name.lua`. Of the existing
//! candidates the most recently modified is read first; one that does not parse or is an
//! empty table is skipped (KOReader deletes those; we leave them alone).

pub mod sqlite;

pub use sqlite::{
    read_statistics, read_statistics_db, read_vocabulary, KoPageStat, KoStatistics, KoStatsBook, KoVocabWord,
};

use crate::lua::{self, Table, Value};
use crate::sidecar::{BookState, Bookmark, Highlight, Quad, ReadingStatus};
use crate::{os_bytes, os_from_bytes, with_suffix, Result, StoreError};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// KOReader's data directory (`/mnt/us/koreader` on a Kindle).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KoreaderDirs {
    /// The data directory.
    pub root: PathBuf,
}

impl KoreaderDirs {
    /// The Kindle location, `/mnt/us/koreader`.
    pub fn kindle() -> Self {
        KoreaderDirs { root: ink_platform::Platform::Kindle.koreader_dir() }
    }

    /// KOReader on this e-reader (`/mnt/us/koreader`, `/mnt/onboard/.adds/koreader`).
    pub fn device() -> Self {
        KoreaderDirs { root: ink_platform::current().koreader_dir() }
    }

    /// `settings/`.
    pub fn settings_dir(&self) -> PathBuf {
        self.root.join("settings")
    }

    /// `settings/statistics.sqlite3`.
    pub fn statistics(&self) -> PathBuf {
        self.settings_dir().join("statistics.sqlite3")
    }

    /// `settings/vocabulary_builder.sqlite3`.
    pub fn vocabulary(&self) -> PathBuf {
        self.settings_dir().join("vocabulary_builder.sqlite3")
    }

    /// `docsettings/` (the "dir" sidecar location).
    pub fn docsettings(&self) -> PathBuf {
        self.root.join("docsettings")
    }

    /// `hashdocsettings/` (the "hash" sidecar location).
    pub fn hashdocsettings(&self) -> PathBuf {
        self.root.join("hashdocsettings")
    }

    /// `history/` (legacy sidecars).
    pub fn history(&self) -> PathBuf {
        self.root.join("history")
    }
}

/// Kind of a KOReader annotation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KoAnnotationKind {
    /// A highlight without a note.
    #[default]
    Highlight,
    /// A highlight with a note.
    Note,
    /// A page bookmark (no `pos0`).
    Bookmark,
}

/// A box of a fixed-layout highlight (`pboxes`), in page points (KOReader's native page
/// coordinates: origin top left, zoom 1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KoBox {
    /// Left.
    pub x: f64,
    /// Top.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

/// One KOReader annotation (highlight, note or bookmark).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KoAnnotation {
    /// Highlight, note or bookmark.
    pub kind: KoAnnotationKind,
    /// Start: the xpointer (`pos0`) for reflowable books; for fixed-layout books the
    /// `ink_doc` string of the first box (`page:N;x0,y0,x1,y1`, N 0-based). For a
    /// bookmark: its position.
    pub pos0: String,
    /// End (`pos1`, or the last box); empty for bookmarks.
    pub pos1: String,
    /// Fixed-layout page, 1-based as KOReader stores it.
    pub page: Option<u32>,
    /// Boxes of a fixed-layout highlight.
    pub boxes: Vec<KoBox>,
    /// Highlighted text (for bookmarks: KOReader's snippet).
    pub text: String,
    /// Note.
    pub note: Option<String>,
    /// Drawer style (`lighten`, `underscore`, `strikeout`, `invert`).
    pub drawer: Option<String>,
    /// Color name.
    pub color: Option<String>,
    /// `datetime` as stored (`YYYY-MM-DD HH:MM:SS`).
    pub datetime: String,
    /// `datetime_updated`.
    pub datetime_updated: Option<String>,
    /// Chapter title.
    pub chapter: Option<String>,
    /// Page number KOReader showed (`pageno`), informational.
    pub pageno: Option<u32>,
}

/// What KOReader stored for one book.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KoBookMetadata {
    /// Source file.
    pub path: PathBuf,
    /// `last_xpointer` (reflowable books).
    pub last_xpointer: Option<String>,
    /// `last_page` (fixed-layout books, 1-based).
    pub last_page: Option<u32>,
    /// `percent_finished` (0.0..=1.0).
    pub percent_finished: Option<f64>,
    /// `summary.status` mapped to ours (no status: `Reading` if a position is stored,
    /// else `New`, as KOReader's book list does).
    pub status: ReadingStatus,
    /// `summary.status` as stored.
    pub status_raw: Option<String>,
    /// `summary.rating` (1..=5).
    pub rating: Option<u8>,
    /// `summary.note` (review).
    pub review: Option<String>,
    /// `summary.modified` (date of the last status change).
    pub status_modified: Option<String>,
    /// `partial_md5_checksum`.
    pub md5: Option<String>,
    /// `doc_props.title`.
    pub title: Option<String>,
    /// `doc_props.authors` (KOReader separates several with newlines).
    pub authors: Vec<String>,
    /// `doc_props.language`.
    pub language: Option<String>,
    /// `doc_props.series`.
    pub series: Option<String>,
    /// `doc_props.series_index`.
    pub series_index: Option<f64>,
    /// `doc_pages`.
    pub doc_pages: Option<u32>,
    /// `annotations` (or the older `highlight` + `bookmarks` tables).
    pub annotations: Vec<KoAnnotation>,
}

impl KoBookMetadata {
    /// Highlights and notes.
    pub fn highlights(&self) -> impl Iterator<Item = &KoAnnotation> {
        self.annotations.iter().filter(|a| a.kind != KoAnnotationKind::Bookmark)
    }

    /// Page bookmarks.
    pub fn bookmarks(&self) -> impl Iterator<Item = &KoAnnotation> {
        self.annotations.iter().filter(|a| a.kind == KoAnnotationKind::Bookmark)
    }

    /// The reading position as an `ink_doc::Position` string.
    pub fn position(&self) -> Option<String> {
        match (&self.last_xpointer, self.last_page) {
            (Some(x), _) if !x.is_empty() => Some(x.clone()),
            (_, Some(p)) => Some(format!("page:{}", p.saturating_sub(1))),
            _ => None,
        }
    }

    /// Our sidecar state for this book (nothing is written).
    pub fn to_book_state(&self) -> BookState {
        let mut s = BookState::default();
        merge_into(&mut s, self, true);
        s
    }
}

/// What [`merge_into`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    /// The reading position was taken over.
    pub position: bool,
    /// Highlights and notes added.
    pub highlights: usize,
    /// Bookmarks added.
    pub bookmarks: usize,
}

/// Merges KOReader's data into `state`: highlights and bookmarks are added unless an
/// equal one exists (same start and end, or same bookmark position); the position,
/// progress, status, rating and review are taken over when `state` has none or when
/// `prefer_koreader` is set.
pub fn merge_into(state: &mut BookState, ko: &KoBookMetadata, prefer_koreader: bool) -> ImportSummary {
    let mut sum = ImportSummary::default();
    if let Some(p) = ko.position().filter(|_| prefer_koreader || state.position.is_none()) {
        state.position = Some(p);
        state.page = ko.last_page.map(|p| p.saturating_sub(1)).or(state.page);
        sum.position = true;
    }
    if prefer_koreader || state.percent.is_none() {
        state.percent = ko.percent_finished.map(|p| p.clamp(0.0, 1.0) as f32).or(state.percent);
    }
    if (prefer_koreader || state.status == ReadingStatus::New) && ko.status != ReadingStatus::New {
        state.status = ko.status;
    }
    if prefer_koreader || state.rating.is_none() {
        state.rating = ko.rating.or(state.rating);
    }
    if prefer_koreader || state.review.is_none() {
        state.review = ko.review.clone().or(state.review.take());
    }
    if state.md5.is_none() {
        state.md5 = ko.md5.clone();
    }
    if ko.doc_pages.is_some() && state.pages.is_none() {
        state.pages = ko.doc_pages;
    }
    for a in &ko.annotations {
        let page = a.page.map(|p| p.saturating_sub(1));
        match a.kind {
            KoAnnotationKind::Bookmark => {
                let added = state.add_bookmark(Bookmark {
                    position: a.pos0.clone(),
                    label: a.text.clone(),
                    created: a.datetime.clone(),
                    note: a.note.clone(),
                    chapter: a.chapter.clone(),
                    page,
                });
                sum.bookmarks += added as usize;
            }
            KoAnnotationKind::Highlight | KoAnnotationKind::Note => {
                if state.highlights.iter().any(|h| h.start == a.pos0 && h.end == a.pos1 && h.page == page) {
                    continue;
                }
                state.highlights.push(Highlight {
                    start: a.pos0.clone(),
                    end: a.pos1.clone(),
                    text: a.text.clone(),
                    note: a.note.clone(),
                    style: a.drawer.clone().unwrap_or_else(|| "lighten".into()),
                    color: a.color.clone(),
                    chapter: a.chapter.clone(),
                    created: a.datetime.clone(),
                    updated: a.datetime_updated.clone(),
                    page,
                    quads: a.boxes.iter().map(box_quad).collect(),
                });
                sum.highlights += 1;
            }
        }
    }
    if state.imported_from.is_none() {
        state.imported_from = Some(format!("koreader:{}", ko.path.display()));
    }
    sum
}

/// A box as a quad; boxes are only built from finite numbers (see `boxes`), and the
/// `f32` conversion of a huge finite number is checked again by [`Quad::is_finite`].
fn box_quad(b: &KoBox) -> Quad {
    Quad { x0: b.x as f32, y0: b.y as f32, x1: (b.x + b.w) as f32, y1: (b.y + b.h) as f32 }
}

/// KOReader's sidecar directory in the book folder: the whole path up to its last `.`
/// plus `.sdr` (`doc_path:match("(.*)%.")`); a path without a dot keeps its full name.
/// Path bytes are kept (KOReader works on bytes; names need not be UTF-8).
pub fn koreader_sidecar_dir(book: &Path) -> PathBuf {
    let b = os_bytes(book.as_os_str());
    let mut base = match b.iter().rposition(|&c| c == b'.') {
        Some(i) => b[..i].to_vec(),
        None => b.to_vec(),
    };
    base.extend_from_slice(b".sdr");
    PathBuf::from(os_from_bytes(base))
}

/// `metadata.<suffix>.lua`, the suffix being the bytes after the last `.` of the path,
/// case kept; `_` when there is none (`DocSettings.getSidecarFilename`).
pub fn sidecar_filename(book: &Path) -> OsString {
    let b = os_bytes(book.as_os_str());
    let suffix = match b.iter().rposition(|&c| c == b'.') {
        Some(i) if i + 1 < b.len() => &b[i + 1..],
        _ => b"_",
    };
    let mut name = b"metadata.".to_vec();
    name.extend_from_slice(suffix);
    name.extend_from_slice(b".lua");
    os_from_bytes(name)
}

/// Path of KOReader's metadata file for `book` in the book folder:
/// `<book without suffix>.sdr/metadata.<suffix>.lua`.
pub fn metadata_path(book: &Path) -> PathBuf {
    koreader_sidecar_dir(book).join(sidecar_filename(book))
}

/// Every place KOReader may keep metadata for `book`, in KOReader's priority order.
/// `ko` enables the locations under KOReader's data directory; `md5` (the book's
/// partial MD5) enables the hash location.
pub fn metadata_candidates(book: &Path, ko: Option<&KoreaderDirs>, md5: Option<&str>) -> Vec<PathBuf> {
    let name = sidecar_filename(book);
    let doc_dir = koreader_sidecar_dir(book);
    let doc_file = doc_dir.join(&name);
    let mut v = vec![doc_file.clone(), with_suffix(&doc_file, ".old")];
    if let Some(base) = book.file_name() {
        let mut legacy = base.to_os_string();
        legacy.push(".lua");
        v.push(doc_dir.join(legacy));
    }
    if let Some(ko) = ko {
        // KOReader: DOCSETTINGS_DIR .. "<absolute path without suffix>.sdr".
        let sdr = koreader_sidecar_dir(book);
        let b = os_bytes(sdr.as_os_str());
        let rel = os_from_bytes(b[b.iter().take_while(|&&c| c == b'/').count()..].to_vec());
        let dir_file = ko.docsettings().join(rel).join(&name);
        v.push(dir_file.clone());
        v.push(with_suffix(&dir_file, ".old"));
        // Only a real book id names a directory (it comes from a file).
        if let Some(md5) = md5.and_then(crate::md5::normalize_md5) {
            let hash_file = ko.hashdocsettings().join(&md5[..2]).join(format!("{md5}.sdr")).join(&name);
            v.push(hash_file.clone());
            v.push(with_suffix(&hash_file, ".old"));
        }
        let hist = ko.history().join(history_name(book));
        v.push(hist.clone());
        v.push(with_suffix(&hist, ".old"));
    }
    v.push(with_suffix(book, ".kpdfview.lua"));
    v
}

/// Legacy history file name: `/mnt/us/Books/x.epub` -> `[#mnt#us#Books#] x.epub.lua`
/// (bytes kept).
pub fn history_name(book: &Path) -> OsString {
    let b = os_bytes(book.as_os_str());
    let mut out = b"[".to_vec();
    match b.iter().rposition(|&c| c == b'/') {
        Some(i) => {
            out.extend_from_slice(&b[..=i]);
            out.extend_from_slice(b"] ");
            out.extend_from_slice(&b[i + 1..]);
        }
        None => out.extend_from_slice(b),
    }
    for c in &mut out {
        if *c == b'/' {
            *c = b'#';
        }
    }
    out.extend_from_slice(b".lua");
    os_from_bytes(out)
}

/// Existing candidates, most recently modified first (priority order breaks ties; a
/// `.old` backup never sorts ahead of its primary file), as `DocSettings` orders them.
pub fn existing_candidates(candidates: &[PathBuf]) -> Vec<PathBuf> {
    let mut found: Vec<(PathBuf, SystemTime, usize)> = Vec::new();
    let mut prev_exists = false;
    for (prio, p) in candidates.iter().enumerate() {
        match std::fs::metadata(p) {
            Ok(m) if m.is_file() => {
                let mut mtime = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                if p.to_string_lossy().ends_with(".old") && prev_exists {
                    if let Some(last) = found.last_mut() {
                        if last.1 < mtime {
                            last.1 = mtime;
                        }
                        mtime = last.1;
                    }
                }
                found.push((p.clone(), mtime, prio));
                prev_exists = true;
            }
            _ => prev_exists = false,
        }
    }
    found.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
    found.into_iter().map(|(p, _, _)| p).collect()
}

/// Reads KOReader's metadata for `book` from the book folder (see
/// [`read_book_metadata_in`] for KOReader's other locations).
pub fn read_book_metadata(book: &Path) -> Result<KoBookMetadata> {
    read_book_metadata_in(book, None, None)
}

/// Reads KOReader's metadata for `book`, looking in the book folder and, with `ko`, in
/// KOReader's data directory (`md5` enables the hash location). Candidates are tried
/// most recent first; [`StoreError::NotFound`] if none exists, the last parse error if
/// none parses.
pub fn read_book_metadata_in(book: &Path, ko: Option<&KoreaderDirs>, md5: Option<&str>) -> Result<KoBookMetadata> {
    let candidates = existing_candidates(&metadata_candidates(book, ko, md5));
    if candidates.is_empty() {
        return Err(StoreError::NotFound(metadata_path(book).display().to_string()));
    }
    let paging = is_paging_format(book);
    let mut last_err = None;
    for c in candidates {
        match read_metadata_file(&c, Some(paging)) {
            Ok(m) => return Ok(m),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| StoreError::NotFound(book.display().to_string())))
}

/// True for formats KOReader opens with a paging (fixed-layout) engine.
pub fn is_paging_format(book: &Path) -> bool {
    let ext = book.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "pdf" | "cbz" | "cbt" | "cb7" | "cbr" | "djvu" | "djv" | "xps" | "oxps" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "tif" | "tiff"
    )
}

/// Parses one KOReader metadata file. `paging` selects the annotation set when the file
/// holds both formats (`None`: guess from the stored annotations). Errors: the file is
/// unreadable, is not a Lua table literal, or is an empty table.
pub fn read_metadata_file(path: &Path, paging: Option<bool>) -> Result<KoBookMetadata> {
    // Read at most one byte more than the parser accepts (a huge file is refused
    // without being read into memory).
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(lua::MAX_INPUT as u64 + 1).read_to_end(&mut bytes)?;
    let t = lua::parse_table(&bytes).map_err(|e| StoreError::Format(format!("{}: {e}", path.display())))?;
    if t.is_empty() {
        return Err(StoreError::Format(format!("{}: empty table", path.display())));
    }
    Ok(metadata_from_table(path, &t, paging))
}

/// Builds [`KoBookMetadata`] from a parsed metadata table.
pub fn metadata_from_table(path: &Path, t: &Table, paging: Option<bool>) -> KoBookMetadata {
    let summary = t.table("summary");
    let props = t.table("doc_props");
    let status_raw = summary.and_then(|s| s.str("status")).map(str::to_string);
    let last_xpointer = t.str("last_xpointer").map(str::to_string);
    let last_page = t.int("last_page").and_then(|p| u32::try_from(p).ok());
    let percent_finished = t.num("percent_finished").filter(|p| p.is_finite());
    let status = status_raw.as_deref().and_then(ReadingStatus::from_koreader).unwrap_or(
        if last_xpointer.is_some() || last_page.is_some() || percent_finished.is_some() {
            ReadingStatus::Reading
        } else {
            ReadingStatus::New
        },
    );
    let text = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let paging = paging.unwrap_or_else(|| guess_paging(t, last_page.is_some()));
    KoBookMetadata {
        path: path.to_path_buf(),
        last_xpointer,
        last_page,
        percent_finished,
        status,
        status_raw,
        rating: summary.and_then(|s| s.int("rating")).filter(|r| (1..=5).contains(r)).map(|r| r as u8),
        review: text(summary.and_then(|s| s.str("note"))),
        status_modified: text(summary.and_then(|s| s.str("modified"))),
        // Only a real book id: it ends up in file names (covers, hash locations).
        md5: t.str("partial_md5_checksum").and_then(crate::md5::normalize_md5),
        title: text(props.and_then(|p| p.str("title"))),
        authors: props
            .and_then(|p| p.str("authors"))
            .map(|a| a.split('\n').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
            .unwrap_or_default(),
        language: text(props.and_then(|p| p.str("language"))),
        series: text(props.and_then(|p| p.str("series"))),
        series_index: props.and_then(|p| p.num("series_index")),
        doc_pages: t.int("doc_pages").and_then(|p| u32::try_from(p).ok()),
        annotations: annotations(t, paging),
    }
}

/// Paging if the stored annotations say so (numeric `page`), else if `last_page` is set.
fn guess_paging(t: &Table, has_last_page: bool) -> bool {
    for key in ["annotations", "bookmarks"] {
        if let Some(first) = t.table(key).and_then(|a| a.seq().first().and_then(|v| v.as_table())) {
            match first.get("page") {
                Some(Value::Int(_) | Value::Num(_)) => return true,
                Some(Value::Str(_)) => return false,
                _ => {}
            }
        }
    }
    has_last_page
}

/// True if the annotation list stores pages the way `paging` expects (numbers for paging,
/// xpointer strings for rolling). Empty lists match.
fn list_matches(list: &Table, paging: bool) -> bool {
    match list.seq().first().and_then(|v| v.as_table()).and_then(|a| a.get("page")) {
        Some(Value::Int(_) | Value::Num(_)) => paging,
        Some(Value::Str(_)) => !paging,
        _ => true,
    }
}

fn annotations(t: &Table, paging: bool) -> Vec<KoAnnotation> {
    let alt = if paging { "annotations_paging" } else { "annotations_rolling" };
    if let Some(list) = t.table("annotations") {
        let chosen = if !list.is_empty() && !list_matches(list, paging) { t.table(alt) } else { Some(list) };
        return chosen.map(|l| l.seq().into_iter().filter_map(|v| v.as_table()).filter_map(|a| annotation(a, paging)).collect()).unwrap_or_default();
    }
    // Older files: `bookmarks` + `highlight` (or the per-engine copies).
    let suffix = if paging { "_paging" } else { "_rolling" };
    let (bookmarks, highlights) = match t.table("bookmarks") {
        Some(b) if list_matches(b, paging) => (Some(b), t.table("highlight")),
        _ => (t.table(&format!("bookmarks{suffix}")), t.table(&format!("highlight{suffix}"))),
    };
    legacy_annotations(bookmarks, highlights, paging, t.get("highlights_imported").is_none())
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.filter(|s| !s.is_empty()).map(str::to_string)
}

fn page_number(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_i64).and_then(|p| u32::try_from(p).ok())
}

/// The boxes of a `pboxes` list; a box with a missing or non-finite number (LuaJIT's
/// `dump.lua` writes `inf` and `nan`) or one that is not finite as `f32` is left out.
fn boxes(t: Option<&Table>) -> Vec<KoBox> {
    t.map(|t| {
        t.seq()
            .into_iter()
            .filter_map(|b| b.as_table())
            .filter_map(|b| Some(KoBox { x: b.num("x")?, y: b.num("y")?, w: b.num("w")?, h: b.num("h")? }))
            .filter(|b| box_quad(b).is_finite())
            .collect()
    })
    .unwrap_or_default()
}

/// `page:N;x0,y0,x1,y1` (N 0-based) for a fixed-layout position.
fn page_position(page: u32, quad: Option<Quad>) -> String {
    let n = page.saturating_sub(1);
    match quad {
        Some(q) => format!("page:{n};{},{},{},{}", q.x0, q.y0, q.x1, q.y1),
        None => format!("page:{n}"),
    }
}

fn point_quad(p: Option<&Table>) -> Option<Quad> {
    let p = p?;
    let (x, y) = (p.num("x")? as f32, p.num("y")? as f32);
    Some(Quad { x0: x, y0: y, x1: x, y1: y }).filter(Quad::is_finite)
}

/// One entry of the `annotations` list.
fn annotation(a: &Table, paging: bool) -> Option<KoAnnotation> {
    let note = non_empty(a.str("note"));
    let mut out = KoAnnotation {
        text: a.str("text").unwrap_or_default().to_string(),
        note,
        drawer: non_empty(a.str("drawer")),
        color: non_empty(a.str("color")),
        datetime: a.str("datetime").unwrap_or_default().to_string(),
        datetime_updated: non_empty(a.str("datetime_updated")),
        chapter: non_empty(a.str("chapter")),
        pageno: page_number(a.get("pageno")),
        ..Default::default()
    };
    let has_pos = a.get("pos0").is_some();
    if paging {
        let page = page_number(a.get("page")).or_else(|| a.table("pos0").and_then(|p| page_number(p.get("page"))))?;
        out.page = Some(page);
        out.boxes = boxes(a.table("pboxes"));
        if has_pos {
            let first = out.boxes.first().map(box_quad).or_else(|| point_quad(a.table("pos0")));
            let last = out.boxes.last().map(box_quad).or_else(|| point_quad(a.table("pos1")));
            out.pos0 = page_position(page, first);
            out.pos1 = page_position(page, last);
        } else {
            out.pos0 = page_position(page, None);
        }
    } else if has_pos {
        out.pos0 = a.str("pos0")?.to_string();
        out.pos1 = a.str("pos1").unwrap_or(&out.pos0).to_string();
    } else {
        out.pos0 = a.str("page")?.to_string();
    }
    out.kind = match (has_pos, out.note.is_some()) {
        (false, _) => KoAnnotationKind::Bookmark,
        (true, true) => KoAnnotationKind::Note,
        (true, false) => KoAnnotationKind::Highlight,
    };
    Some(out)
}

/// KOReader before 2024: `bookmarks` (list; `highlighted` ones are highlights, `notes` is
/// the highlighted text and `text` the user's note or an automatic label) plus
/// `highlight` (by page: style and, before 2014, highlights missing from `bookmarks`).
fn legacy_annotations(bookmarks: Option<&Table>, highlights: Option<&Table>, paging: bool, import_old: bool) -> Vec<KoAnnotation> {
    // Highlights by page, with their table (for the drawer and pboxes).
    let mut hls: Vec<(i64, &Table)> = Vec::new();
    if let Some(h) = highlights {
        for (k, v) in &h.entries {
            let page = match k {
                lua::Key::Int(i) => *i,
                _ => 0,
            };
            if let Some(list) = v.as_table() {
                for hl in list.seq().into_iter().filter_map(|x| x.as_table()) {
                    hls.push((page, hl));
                }
            }
        }
    }
    let same = |hl: &Table, bm: &Table| {
        let dt_differs = matches!((hl.str("datetime"), bm.str("datetime")), (Some(a), Some(b)) if a != b);
        if dt_differs {
            return false;
        }
        if paging {
            let num = |t: &Table, p: &str, c: &str| t.table(p).and_then(|x| x.num(c));
            ["x", "y"].iter().all(|c| num(hl, "pos0", c) == num(bm, "pos0", c) && num(hl, "pos1", c) == num(bm, "pos1", c))
        } else {
            hl.str("pos0") == bm.str("pos0") && hl.str("pos1") == bm.str("pos1")
        }
    };
    let mut items: Vec<(Table, Option<&Table>)> = Vec::new();
    let mut used = vec![false; hls.len()];
    if let Some(b) = bookmarks {
        for bm in b.seq().into_iter().filter_map(|x| x.as_table()) {
            let mut hl = None;
            if bm.bool("highlighted") == Some(true) {
                if let Some(i) = hls.iter().position(|(_, h)| same(h, bm)) {
                    used[i] = true;
                    hl = Some(hls[i].1);
                }
            }
            items.push((bm.clone(), hl));
        }
    }
    if import_old {
        // Before 2014 highlights were not added to the bookmarks.
        for (i, (page, h)) in hls.iter().enumerate() {
            // KOReader skips old highlights without `pos0` too.
            if used[i] || h.get("pos0").is_none() || items.iter().any(|(bm, _)| bm.bool("highlighted") == Some(true) && same(h, bm)) {
                continue;
            }
            let mut bm = Table::default();
            let set = |bm: &mut Table, k: &str, v: Option<&Value>| {
                if let Some(v) = v {
                    bm.entries.push((lua::Key::Str(k.into()), v.clone()));
                }
            };
            set(&mut bm, "datetime", h.get("datetime"));
            set(&mut bm, "notes", h.get("text"));
            set(&mut bm, "pos0", h.get("pos0"));
            set(&mut bm, "pos1", h.get("pos1"));
            set(&mut bm, "chapter", h.get("chapter"));
            bm.entries.push((lua::Key::Str("highlighted".into()), Value::Bool(true)));
            let page_v = if paging { Value::Int(*page) } else { h.get("pos0").cloned().unwrap_or(Value::Nil) };
            bm.entries.push((lua::Key::Str("page".into()), page_v));
            items.push((bm, Some(h)));
        }
    }
    let mut out = Vec::new();
    for (bm, hl) in items {
        let highlighted = bm.bool("highlighted") == Some(true);
        let notes = bm.str("notes").unwrap_or_default().to_string();
        let datetime = bm.str("datetime").unwrap_or_default().to_string();
        let mut note = non_empty(bm.str("text"));
        if note.as_deref().is_some_and(|n| is_auto_text(n, &notes, &datetime)) {
            note = None;
        }
        let mut a = KoAnnotation {
            text: notes,
            note,
            datetime,
            chapter: non_empty(bm.str("chapter")),
            drawer: hl.and_then(|h| non_empty(h.str("drawer"))),
            color: hl.and_then(|h| non_empty(h.str("color"))),
            ..Default::default()
        };
        if paging {
            let Some(page) = page_number(bm.get("page")) else { continue };
            a.page = Some(page);
            a.boxes = boxes(hl.and_then(|h| h.table("pboxes")));
            if highlighted {
                a.pos0 = page_position(page, a.boxes.first().map(box_quad).or_else(|| point_quad(bm.table("pos0"))));
                a.pos1 = page_position(page, a.boxes.last().map(box_quad).or_else(|| point_quad(bm.table("pos1"))));
            } else {
                a.pos0 = page_position(page, None);
            }
        } else if highlighted {
            let Some(p0) = bm.str("pos0").or(bm.str("page")) else { continue };
            a.pos0 = p0.to_string();
            a.pos1 = bm.str("pos1").unwrap_or(p0).to_string();
        } else {
            let Some(p) = bm.str("page") else { continue };
            a.pos0 = p.to_string();
        }
        a.kind = match (highlighted, a.note.is_some()) {
            (false, _) => KoAnnotationKind::Bookmark,
            (true, true) => KoAnnotationKind::Note,
            (true, false) => KoAnnotationKind::Highlight,
        };
        out.push(a);
    }
    out
}

/// KOReader's old automatic bookmark text (`isBookmarkAutoText`): empty, equal to the
/// highlighted text, or `Page <n> <text> @ <datetime>` (any language: the check is on
/// the text and the datetime).
fn is_auto_text(text: &str, notes: &str, datetime: &str) -> bool {
    text.is_empty() || text == notes || (!datetime.is_empty() && text.ends_with(&format!(" @ {datetime}")) && text.contains(notes))
}

/// Reads KOReader's metadata for `book` (book folder and, with `ko`, KOReader's data
/// directory), merges it into our sidecar and saves our sidecar. KOReader's files are
/// only read. Returns `None` when KOReader has nothing for this book.
pub fn import_book(book: &Path, ko: Option<&KoreaderDirs>, prefer_koreader: bool) -> Result<Option<ImportSummary>> {
    let md5 = crate::partial_md5(book).ok();
    let meta = match read_book_metadata_in(book, ko, md5.as_deref()) {
        Ok(m) => m,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut state = BookState::load(book)?;
    // The hash of the file as it is now beats KOReader's stored checksum.
    if state.md5.is_none() {
        state.md5 = md5;
    }
    let sum = merge_into(&mut state, &meta, prefer_koreader);
    state.save(book)?;
    Ok(Some(sum))
}

/// What [`sync_book`] did for one book.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncResult {
    /// KOReader's metadata file was newer than our sidecar (or ours had no position), so
    /// its position, progress, status, rating and review were taken over.
    pub koreader_newer: bool,
    /// What was merged.
    pub summary: ImportSummary,
    /// KOReader's metadata file.
    pub source: PathBuf,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Mirrors KOReader's data for `book` into our sidecar. Highlights, notes and bookmarks
/// are merged (ours are kept, equal ones are not added twice). The position, progress,
/// status, rating and review come from KOReader when its metadata file is newer than our
/// sidecar or ours has no position; otherwise ours stay (the newer reader wins). With
/// `dry_run` nothing is written. `None` when KOReader has nothing for this book.
pub fn sync_book(book: &Path, ko: Option<&KoreaderDirs>, dry_run: bool) -> Result<Option<SyncResult>> {
    let md5 = crate::partial_md5(book).ok();
    let meta = match read_book_metadata_in(book, ko, md5.as_deref()) {
        Ok(m) => m,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut state = BookState::load(book)?;
    if state.md5.is_none() {
        state.md5 = md5;
    }
    let ours = mtime(&crate::sidecar::sidecar_path(book));
    let koreader_newer = state.position.is_none()
        || match (mtime(&meta.path), ours) {
            (Some(k), Some(o)) => k > o,
            (_, None) => true,
            (None, Some(_)) => false,
        };
    let summary = merge_into(&mut state, &meta, koreader_newer);
    if !dry_run {
        state.save(book)?;
    }
    Ok(Some(SyncResult { koreader_newer, summary, source: meta.path.clone() }))
}

#[cfg(test)]
mod tests;
