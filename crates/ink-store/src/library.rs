//! The library index: one JSON file listing known books (`/mnt/us/rinky/library.json`).
//!
//! It holds per book: path, partial MD5, metadata (title, authors, series, language,
//! publisher, page count), the cover thumbnail path in the cache, collections, last
//! open, progress, status and rating. Collections are also listed on their own so empty
//! ones survive. The file is compact JSON (10k books load in well under a second on the
//! device; see `library_10k_benchmark` and the selftest) and is written with a `.old`
//! backup; a corrupt file is moved aside and the backup used.
//!
//! The scanner only lists files (path, size, mtime); metadata and covers are extracted
//! by the library screen through `ink-formats`/`ink-mupdf` and stored with
//! [`LibraryIndex::set_metadata`]. [`LibraryIndex::needs_metadata`] lists the books whose
//! file changed since. A folder the scanner cannot read (I/O error, permissions, a
//! scan root that is gone while the user store is remounted) keeps its books and their
//! collections; only books whose file is really gone are dropped.
//!
//! Paths are stored as JSON strings; a path that is not UTF-8 (Linux file names are
//! bytes) is stored as `{"hex": "<bytes in hex>"}` so one such book cannot make the
//! index unsaveable.

use crate::sidecar::{BookState, ReadingStatus};
use crate::{load_recovering, parse_json, save_with_backup, LoadSource, Result, StoreError};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

/// File extensions the scanner lists (lowercase, without dot).
pub const BOOK_EXTENSIONS: &[&str] =
    &["epub", "pdf", "cbz", "fb2", "fb3", "mobi", "azw3", "azw", "prc", "txt", "html", "htm", "kfx"];

fn is_false(b: &bool) -> bool {
    !*b
}

/// (De)serializes a path as a JSON string, or as `{"hex": "..."}` when it is not UTF-8.
mod path_json {
    use crate::{os_bytes, os_from_bytes};
    use serde::de::{self, MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};
    use std::fmt;
    use std::path::{Path, PathBuf};

    pub fn serialize<S: Serializer>(p: &Path, s: S) -> Result<S::Ok, S::Error> {
        match p.to_str() {
            Some(t) => s.serialize_str(t),
            None => {
                let hex: String = os_bytes(p.as_os_str()).iter().map(|b| format!("{b:02x}")).collect();
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("hex", &hex)?;
                m.end()
            }
        }
    }

    struct PathVisitor;

    impl<'de> Visitor<'de> for PathVisitor {
        type Value = PathBuf;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a path string or {\"hex\": \"...\"}")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<PathBuf, E> {
            Ok(PathBuf::from(v))
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<PathBuf, A::Error> {
            let mut hex: Option<String> = None;
            while let Some(k) = map.next_key::<String>()? {
                if k == "hex" {
                    hex = Some(map.next_value()?);
                } else {
                    map.next_value::<de::IgnoredAny>()?;
                }
            }
            let hex = hex.ok_or_else(|| de::Error::missing_field("hex"))?;
            if hex.len() % 2 != 0 || !hex.is_ascii() {
                return Err(de::Error::custom("bad hex path"));
            }
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
                .collect::<Result<Vec<u8>, _>>()
                .map_err(|_| de::Error::custom("bad hex path"))?;
            Ok(PathBuf::from(os_from_bytes(bytes)))
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<PathBuf, D::Error> {
        d.deserialize_any(PathVisitor)
    }

    /// The same for `Option<PathBuf>`.
    pub mod option {
        use serde::{Deserialize, Deserializer, Serializer};
        use std::path::PathBuf;

        pub fn serialize<S: Serializer>(p: &Option<PathBuf>, s: S) -> Result<S::Ok, S::Error> {
            match p {
                Some(p) => super::serialize(p, s),
                None => s.serialize_none(),
            }
        }

        #[derive(Deserialize)]
        struct Wrap(#[serde(deserialize_with = "super::deserialize")] PathBuf);

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<PathBuf>, D::Error> {
            Ok(Option::<Wrap>::deserialize(d)?.map(|w| w.0))
        }
    }
}

/// One book in the index.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookEntry {
    /// Absolute path.
    #[serde(with = "path_json")]
    pub path: PathBuf,
    /// KOReader partial MD5, once computed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
    /// Title (file name until metadata is read).
    pub title: String,
    /// Authors.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    /// Series name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    /// Position in the series.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_index: Option<f32>,
    /// Language (BCP 47, from the OPF or FB2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Publisher.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
    /// Lowercase extension.
    pub format: String,
    /// File size in bytes.
    pub size: u64,
    /// Modification time, seconds since the epoch.
    pub mtime: u64,
    /// File mtime when metadata and cover were last extracted; 0 = never.
    #[serde(skip_serializing_if = "is_zero")]
    pub meta_mtime: u64,
    /// Cover thumbnail in the cache (see [`crate::Paths::cover`]), once extracted.
    #[serde(skip_serializing_if = "Option::is_none", with = "path_json::option")]
    pub cover: Option<PathBuf>,
    /// Page count (fixed layout) or last known page count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    /// When the book was first seen, seconds since the epoch.
    pub added: u64,
    /// Last open, seconds since the epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_opened: Option<u64>,
    /// Progress 0.0..=1.0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f32>,
    /// Reading status.
    #[serde(deserialize_with = "crate::sidecar::lenient_status")]
    pub status: ReadingStatus,
    /// Rating 1..=5.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    /// Collection names.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<String>,
    /// DRM or unsupported format detected (listed, refused on open).
    #[serde(skip_serializing_if = "is_false")]
    pub unsupported: bool,
    /// Lowercase title, authors and series for [`BookEntry::matches`]; not saved.
    /// Built when the index is loaded and when a book is added or gets metadata. After
    /// editing those fields directly, call [`BookEntry::refresh_search_key`]; until then
    /// an edit that changes a length is noticed and the key is not used.
    #[serde(skip)]
    pub search_key: SearchKey,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// The lowercase text a library search looks in, with the lengths of the fields it
/// was made from. It never makes two entries unequal.
#[derive(Clone, Debug, Default)]
pub struct SearchKey {
    /// [`SourceLens`] of the fields when `text` was made; `None` = never made.
    made_from: Option<SourceLens>,
    /// Lowercase title, each author and the series, separated by NUL.
    text: String,
}

/// Byte lengths of title, authors and series: a check that costs a few additions per
/// book (comparing or hashing the text would cost about as much as lowercasing it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceLens {
    title: usize,
    authors: usize,
    author_bytes: usize,
    series: Option<usize>,
}

impl SourceLens {
    fn of(b: &BookEntry) -> Self {
        SourceLens { title: b.title.len(), authors: b.authors.len(), author_bytes: b.authors.iter().map(String::len).sum(), series: b.series.as_ref().map(String::len) }
    }
}

impl PartialEq for SearchKey {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl BookEntry {
    /// Rebuilds [`BookEntry::search_key`] from the title, authors and series.
    pub fn refresh_search_key(&mut self) {
        let mut text = self.title.to_lowercase();
        for a in &self.authors {
            text.push('\0');
            text.push_str(&a.to_lowercase());
        }
        if let Some(s) = &self.series {
            text.push('\0');
            text.push_str(&s.to_lowercase());
        }
        self.search_key = SearchKey { made_from: Some(SourceLens::of(self)), text };
    }

    /// True when the title, an author or the series contains `query`, which must
    /// already be lowercase. Uses the cached key when it is current, else lowercases
    /// the fields on the spot (same answer, slower).
    pub fn matches(&self, query: &str) -> bool {
        let key = &self.search_key;
        if key.made_from.is_some_and(|l| l == SourceLens::of(self)) && !query.contains('\0') {
            return key.text.contains(query);
        }
        self.title.to_lowercase().contains(query)
            || self.authors.iter().any(|a| a.to_lowercase().contains(query))
            || self.series.as_deref().is_some_and(|s| s.to_lowercase().contains(query))
    }
}

/// A named collection.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Collection {
    /// Name (unique).
    pub name: String,
    /// Creation time, seconds since the epoch.
    pub created: u64,
}

/// Metadata extracted from a book file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BookMeta {
    /// Title.
    pub title: Option<String>,
    /// Authors.
    pub authors: Vec<String>,
    /// Series.
    pub series: Option<String>,
    /// Position in the series.
    pub series_index: Option<f32>,
    /// Language.
    pub language: Option<String>,
    /// Publisher.
    pub publisher: Option<String>,
    /// Page count.
    pub pages: Option<u32>,
    /// Cover thumbnail written to the cache.
    pub cover: Option<PathBuf>,
    /// The format is refused (DRM, KFX).
    pub unsupported: bool,
}

/// Sort orders for [`LibraryIndex::sorted`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    /// Last opened, newest first; never-opened books last.
    Recent,
    /// Title, A-Z (case-insensitive).
    Title,
    /// First author, A-Z, then title.
    Author,
    /// Date added, newest first.
    Added,
    /// Progress, most read first.
    Progress,
    /// Series name, then position in the series.
    Series,
}

/// The whole index.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryIndex {
    /// Format version.
    pub version: u32,
    /// Books, in no particular order.
    pub books: Vec<BookEntry>,
    /// Collections, in display order.
    pub collections: Vec<Collection>,
    #[serde(skip)]
    lookup: PathLookup,
}

/// Where each book is in [`LibraryIndex::books`] by path, so [`LibraryIndex::get`] is not a
/// scan of every book (the library screen looks a book up several times while it takes
/// its metadata, for every book). `books` is public, so the map is checked on each use:
/// it is rebuilt when the list moved or changed length, a hit must name a book with that
/// path, and a path the map does not have is looked for by a scan.
#[derive(Default)]
struct PathLookup(Mutex<Option<Positions>>);

/// Path hash to the first book with it, for `books` at `shape` (address and length).
struct Positions {
    shape: (usize, usize),
    map: HashMap<u64, usize>,
}

impl PathLookup {
    fn hash(path: &Path) -> u64 {
        BuildHasherDefault::<DefaultHasher>::default().hash_one(path)
    }

    /// The position of the first book with `path`.
    fn find(&self, books: &[BookEntry], path: &Path) -> Option<usize> {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let shape = (books.as_ptr() as usize, books.len());
        let map = match &mut *slot {
            Some(p) if p.shape == shape => &p.map,
            slot => {
                let map = books.iter().enumerate().rev().map(|(i, b)| (Self::hash(&b.path), i)).collect();
                &slot.insert(Positions { shape, map }).map
            }
        };
        match map.get(&Self::hash(path)) {
            Some(&i) if books[i].path == path => Some(i),
            hit => {
                if hit.is_some() {
                    // Books were replaced or reordered in place: build the map again next time.
                    *slot = None;
                }
                books.iter().position(|b| b.path == path)
            }
        }
    }
}

impl Clone for PathLookup {
    fn clone(&self) -> Self {
        PathLookup::default()
    }
}

impl PartialEq for PathLookup {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for PathLookup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PathLookup")
    }
}

fn mtime_secs(m: &std::fs::Metadata) -> u64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0)
}

impl LibraryIndex {
    /// Current format version. 1: phase 0. 2: metadata, covers, collections list, rating.
    pub const VERSION: u32 = 2;

    /// Loads `path`; a missing file gives an empty index. A corrupt file is moved aside
    /// and its `.old` backup is used; [`StoreError::Format`] only if neither parses.
    pub fn load(path: &Path) -> Result<Self> {
        let r = load_recovering(path, parse_json::<LibraryIndex>)?;
        match (r.value, r.source) {
            (Some(mut v), _) => {
                v.refresh_search_keys();
                Ok(v)
            }
            (None, LoadSource::Missing) if r.errors.is_empty() => Ok(LibraryIndex::default()),
            _ => Err(StoreError::Format(r.errors.join("; "))),
        }
    }

    /// Saves atomically as compact JSON (with a `.old` backup at most once a minute).
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec(&VersionedRef { version: self.version.max(Self::VERSION), books: &self.books, collections: &self.collections })
            .map_err(|e| StoreError::Format(e.to_string()))?;
        save_with_backup(path, &json, |b| parse_json::<LibraryIndex>(b).is_ok())
    }

    /// Rebuilds the search key of every book (after editing titles, authors or series
    /// through `books` directly; [`LibraryIndex::search`] is right without it, only
    /// slower for the edited books).
    pub fn refresh_search_keys(&mut self) {
        for b in &mut self.books {
            b.refresh_search_key();
        }
    }

    /// Finds a book by path.
    pub fn get(&self, path: &Path) -> Option<&BookEntry> {
        self.lookup.find(&self.books, path).map(|i| &self.books[i])
    }

    /// Finds a book by path, mutably.
    pub fn get_mut(&mut self, path: &Path) -> Option<&mut BookEntry> {
        self.lookup.find(&self.books, path).map(|i| &mut self.books[i])
    }

    /// Finds a book by partial MD5.
    pub fn by_md5(&self, md5: &str) -> Option<&BookEntry> {
        self.books.iter().find(|b| b.md5.as_deref() == Some(md5))
    }

    /// Scans `dirs` recursively (skipping `.sdr` and hidden directories), adds new books,
    /// updates size/mtime of known ones and drops entries whose file is gone.
    /// `now` is the current time in seconds (for `added`). Returns (added, removed).
    /// Linear in the number of files; files are not opened or hashed. See
    /// [`LibraryIndex::scan_report`] for the folders that could not be read.
    pub fn scan<P: AsRef<Path>>(&mut self, dirs: &[P], now: u64) -> (usize, usize) {
        let r = self.scan_report(dirs, now);
        (r.added, r.removed)
    }

    /// [`LibraryIndex::scan`], also reporting what could not be read. Books under a
    /// folder that could not be listed (any error; for a scan root also "not found", as
    /// while the user store is remounted after USB drive mode) and books whose file
    /// could not be examined are kept as they are, with their collections. Books that
    /// are in no scanned folder are dropped. Symbolic links to book files are followed;
    /// links to folders are not.
    pub fn scan_report<P: AsRef<Path>>(&mut self, dirs: &[P], now: u64) -> ScanReport {
        let mut w = Walk::default();
        for d in dirs {
            walk(d.as_ref(), 0, &mut w);
        }
        let found = std::mem::take(&mut w.found);
        let present: HashSet<&Path> = found.iter().map(|(p, _)| p.as_path()).collect();
        let before = self.books.len();
        self.books.retain(|b| {
            let p = b.path.as_path();
            present.contains(p) || w.keep_files.contains(p) || w.keep_dirs.iter().any(|d| p.starts_with(d))
        });
        let removed = before - self.books.len();
        let mut index: HashMap<PathBuf, usize> = self.books.iter().enumerate().map(|(i, b)| (b.path.clone(), i)).collect();
        let mut added = 0;
        for (path, meta) in found {
            match index.get(&path) {
                Some(&i) => {
                    let b = &mut self.books[i];
                    b.size = meta.len();
                    b.mtime = mtime_secs(&meta);
                }
                None => {
                    added += 1;
                    let format = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
                    let title = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    index.insert(path.clone(), self.books.len());
                    let mut entry = BookEntry {
                        title,
                        unsupported: format == "kfx",
                        format,
                        size: meta.len(),
                        mtime: mtime_secs(&meta),
                        added: now,
                        path,
                        ..Default::default()
                    };
                    entry.refresh_search_key();
                    self.books.push(entry);
                }
            }
        }
        ScanReport { added, removed, unreadable: w.keep_dirs.into_iter().chain(w.keep_files).collect(), errors: w.errors }
    }

    /// Books whose metadata was never extracted or whose file changed since.
    pub fn needs_metadata(&self) -> Vec<&Path> {
        self.books.iter().filter(|b| b.meta_mtime == 0 || b.meta_mtime != b.mtime).map(|b| b.path.as_path()).collect()
    }

    /// Stores extracted metadata for `path` (fields that are `None` keep their value).
    /// Returns false if the book is not in the index.
    pub fn set_metadata(&mut self, path: &Path, meta: BookMeta) -> bool {
        let Some(b) = self.get_mut(path) else { return false };
        if let Some(t) = meta.title.filter(|t| !t.trim().is_empty()) {
            b.title = t;
        }
        if !meta.authors.is_empty() {
            b.authors = meta.authors;
        }
        b.series = meta.series.or(b.series.take());
        b.series_index = meta.series_index.or(b.series_index);
        b.language = meta.language.or(b.language.take());
        b.publisher = meta.publisher.or(b.publisher.take());
        b.pages = meta.pages.or(b.pages);
        b.cover = meta.cover.or(b.cover.take());
        b.unsupported = meta.unsupported;
        b.meta_mtime = b.mtime.max(1);
        b.refresh_search_key();
        true
    }

    /// Records that `path` was opened at `now`; a book first opened becomes `Reading`.
    pub fn record_open(&mut self, path: &Path, now: u64) -> bool {
        let Some(b) = self.get_mut(path) else { return false };
        b.last_opened = Some(now);
        if b.status == ReadingStatus::New {
            b.status = ReadingStatus::Reading;
        }
        true
    }

    /// Copies progress, status, rating, last open and MD5 from a book's sidecar state.
    /// An MD5 that is not 32 hex digits (it comes from files) is ignored.
    pub fn update_from_state(&mut self, path: &Path, s: &BookState) -> bool {
        let Some(b) = self.get_mut(path) else { return false };
        b.percent = s.percent.filter(|p| p.is_finite()).or(b.percent);
        b.status = s.status;
        b.rating = s.rating.or(b.rating);
        b.last_opened = s.last_opened.or(b.last_opened);
        if let Some(m) = s.md5.as_deref().and_then(crate::md5::normalize_md5) {
            b.md5 = Some(m);
        }
        if s.pages.is_some() {
            b.pages = s.pages;
        }
        true
    }

    /// The `limit` most recently opened books, newest first.
    pub fn recent(&self, limit: usize) -> Vec<&BookEntry> {
        let mut v: Vec<&BookEntry> = self.books.iter().filter(|b| b.last_opened.is_some()).collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.last_opened));
        v.truncate(limit);
        v
    }

    /// All books in `key` order.
    pub fn sorted(&self, key: SortKey) -> Vec<&BookEntry> {
        let mut v: Vec<&BookEntry> = self.books.iter().collect();
        let lower = |s: &str| s.to_lowercase();
        match key {
            SortKey::Recent => v.sort_by(|a, b| b.last_opened.cmp(&a.last_opened).then_with(|| b.added.cmp(&a.added))),
            SortKey::Title => v.sort_by_cached_key(|b| lower(&b.title)),
            SortKey::Author => v.sort_by_cached_key(|b| (lower(b.authors.first().map(String::as_str).unwrap_or("\u{10FFFF}")), lower(&b.title))),
            SortKey::Added => v.sort_by_key(|b| std::cmp::Reverse(b.added)),
            SortKey::Progress => v.sort_by(|a, b| b.percent.unwrap_or(0.0).total_cmp(&a.percent.unwrap_or(0.0))),
            SortKey::Series => {
                // Books without a series go last.
                let mut keyed: Vec<_> = v.into_iter().map(|b| (b.series.as_deref().map(lower), b.series_index.unwrap_or(0.0), b)).collect();
                keyed.sort_by(|a, b| (a.0.is_none(), &a.0).cmp(&(b.0.is_none(), &b.0)).then(a.1.total_cmp(&b.1)));
                return keyed.into_iter().map(|k| k.2).collect();
            }
        }
        v
    }

    /// Books whose title, author or series contains `query` (case-insensitive).
    /// Each book's lowercase text is cached ([`BookEntry::search_key`]), so a query
    /// lowercases only itself.
    pub fn search(&self, query: &str) -> Vec<&BookEntry> {
        let q = query.to_lowercase();
        self.books.iter().filter(|b| b.matches(&q)).collect()
    }

    /// Books with status `status`.
    pub fn with_status(&self, status: ReadingStatus) -> Vec<&BookEntry> {
        self.books.iter().filter(|b| b.status == status).collect()
    }

    /// Creates an empty collection. Errors if the name is empty or taken.
    pub fn create_collection(&mut self, name: &str, now: u64) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::Invalid("empty collection name".into()));
        }
        if self.collections.iter().any(|c| c.name == name) {
            return Err(StoreError::Invalid(format!("collection {name:?} exists")));
        }
        self.collections.push(Collection { name: name.to_string(), created: now });
        Ok(())
    }

    /// Renames a collection and updates its books.
    pub fn rename_collection(&mut self, old: &str, new: &str) -> Result<()> {
        let new = new.trim();
        if new.is_empty() {
            return Err(StoreError::Invalid("empty collection name".into()));
        }
        if old != new && self.collections.iter().any(|c| c.name == new) {
            return Err(StoreError::Invalid(format!("collection {new:?} exists")));
        }
        let c = self.collections.iter_mut().find(|c| c.name == old).ok_or_else(|| StoreError::NotFound(format!("collection {old:?}")))?;
        c.name = new.to_string();
        for n in self.books.iter_mut().flat_map(|b| &mut b.collections).filter(|n| *n == old) {
            *n = new.to_string();
        }
        Ok(())
    }

    /// Deletes a collection (the books stay). Returns whether it existed.
    pub fn delete_collection(&mut self, name: &str) -> bool {
        let n = self.collections.len();
        self.collections.retain(|c| c.name != name);
        for b in &mut self.books {
            b.collections.retain(|c| c != name);
        }
        n != self.collections.len()
    }

    /// Adds a book to a collection, creating the collection if needed.
    pub fn add_to_collection(&mut self, path: &Path, name: &str, now: u64) -> Result<()> {
        if !self.collections.iter().any(|c| c.name == name) {
            self.create_collection(name, now)?;
        }
        let b = self.get_mut(path).ok_or_else(|| StoreError::NotFound(path.display().to_string()))?;
        if !b.collections.iter().any(|c| c == name) {
            b.collections.push(name.to_string());
        }
        Ok(())
    }

    /// Removes a book from a collection. Returns whether it was in it.
    pub fn remove_from_collection(&mut self, path: &Path, name: &str) -> bool {
        let Some(b) = self.get_mut(path) else { return false };
        let n = b.collections.len();
        b.collections.retain(|c| c != name);
        n != b.collections.len()
    }

    /// Books in collection `name`.
    pub fn collection_books(&self, name: &str) -> Vec<&BookEntry> {
        self.books.iter().filter(|b| b.collections.iter().any(|c| c == name)).collect()
    }

    /// Removes a book from the index (the file is not touched). Returns the entry.
    pub fn remove(&mut self, path: &Path) -> Option<BookEntry> {
        let i = self.lookup.find(&self.books, path)?;
        Some(self.books.swap_remove(i))
    }
}

#[derive(Serialize)]
struct VersionedRef<'a> {
    version: u32,
    books: &'a [BookEntry],
    collections: &'a [Collection],
}

/// What [`LibraryIndex::scan_report`] did.
#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    /// Books added.
    pub added: usize,
    /// Books dropped (file gone).
    pub removed: usize,
    /// Folders and files that could not be read; the books in or under them were kept.
    pub unreadable: Vec<PathBuf>,
    /// The errors, one line each.
    pub errors: Vec<String>,
}

#[derive(Default)]
struct Walk {
    found: Vec<(PathBuf, std::fs::Metadata)>,
    keep_dirs: Vec<PathBuf>,
    keep_files: HashSet<PathBuf>,
    errors: Vec<String>,
}

/// True if `path` has one of [`BOOK_EXTENSIONS`] (any case).
pub(crate) fn is_book_name(path: &Path) -> bool {
    let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
    BOOK_EXTENSIONS.contains(&ext.as_str())
}

fn walk(dir: &Path, depth: usize, w: &mut Walk) {
    if depth > 8 {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        // A subfolder listed a moment ago and gone now: its books are gone.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && depth > 0 => return,
        Err(e) => {
            w.errors.push(format!("{}: {e}", dir.display()));
            w.keep_dirs.push(dir.to_path_buf());
            return;
        }
    };
    for entry in rd {
        let e = match entry {
            Ok(e) => e,
            Err(err) => {
                // The listing broke off: whatever was not listed is kept.
                w.errors.push(format!("{}: {err}", dir.display()));
                w.keep_dirs.push(dir.to_path_buf());
                break;
            }
        };
        let path = e.path();
        let meta = match e.file_type() {
            // Follow links to files only (links to folders could loop).
            Ok(t) if t.is_symlink() => match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => Ok(m),
                Ok(_) => continue,
                Err(err) => Err(err),
            },
            // The listing tells folders and files apart: no `stat` for a folder or for a
            // file that is not a book (covers and metadata.opf in a Calibre library).
            Ok(t) if t.is_dir() => {
                let name = e.file_name();
                let name_b = crate::os_bytes(&name);
                if !name_b.starts_with(b".") && !name_b.ends_with(b".sdr") {
                    walk(&path, depth + 1, w);
                }
                continue;
            }
            Ok(t) if t.is_file() && !is_book_name(&path) => continue,
            Ok(_) => e.metadata(),
            Err(err) => Err(err),
        };
        let meta = match meta {
            Ok(m) => m,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                w.errors.push(format!("{}: {err}", path.display()));
                // Anything that is not a book file could be a folder of books: keep
                // anything under it.
                if is_book_name(&path) {
                    w.keep_files.insert(path);
                } else {
                    w.keep_dirs.push(path);
                }
                continue;
            }
        };
        if is_book_name(&path) {
            w.found.push((path, meta));
        }
    }
}

/// Builds a synthetic index of `n` books (benchmarks and the selftest).
pub fn synthetic_index(n: usize) -> LibraryIndex {
    let mut lib = LibraryIndex { version: LibraryIndex::VERSION, ..Default::default() };
    for c in 0..20 {
        lib.collections.push(Collection { name: format!("Collection {c}"), created: 1_700_000_000 + c });
    }
    for i in 0..n {
        let md5 = format!("{:032x}", (i as u128).wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835));
        lib.books.push(BookEntry {
            path: PathBuf::from(format!("/mnt/us/Books/Author {}/Title of the book number {i} - A long subtitle.epub", i % 500)),
            cover: Some(PathBuf::from(format!("/mnt/us/rinky/cache/covers/{}/{md5}.png", &md5[..2]))),
            md5: Some(md5),
            title: format!("Title of the book number {i}: A long subtitle"),
            authors: vec![format!("Author {}", i % 500), "Second Author".into()],
            series: (i % 3 == 0).then(|| format!("Series {}", i % 50)),
            series_index: (i % 3 == 0).then_some((i % 12) as f32),
            language: Some("en".into()),
            publisher: Some("Standard Ebooks".into()),
            format: "epub".into(),
            size: 300_000 + i as u64,
            mtime: 1_700_000_000 + i as u64,
            meta_mtime: 1_700_000_000 + i as u64,
            pages: Some(300),
            added: 1_700_000_000 + i as u64,
            last_opened: (i % 4 == 0).then_some(1_760_000_000 + i as u64),
            percent: (i % 4 == 0).then_some((i % 100) as f32 / 100.0),
            status: if i % 4 == 0 { ReadingStatus::Reading } else { ReadingStatus::New },
            rating: (i % 7 == 0).then_some(4),
            collections: if i % 5 == 0 { vec![format!("Collection {}", i % 20)] } else { Vec::new() },
            unsupported: false,
            search_key: SearchKey::default(),
        });
    }
    lib
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::time::Instant;

    #[test]
    fn scan_add_update_remove_and_round_trip() {
        let tmp = TempDir::new("lib");
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("sub/x.sdr")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join("a.epub"), b"1").unwrap();
        std::fs::write(dir.join("sub/b.PDF"), b"22").unwrap();
        std::fs::write(dir.join("sub/x.sdr/c.epub"), b"3").unwrap();
        std::fs::write(dir.join(".hidden/d.epub"), b"3").unwrap();
        std::fs::write(dir.join("notes.doc"), b"4").unwrap();
        std::fs::write(dir.join("k.kfx"), b"5").unwrap();
        let mut lib = LibraryIndex::default();
        assert_eq!(lib.scan(&[dir], 100), (3, 0));
        let b = lib.get(&dir.join("sub/b.PDF")).unwrap();
        assert_eq!((b.format.as_str(), b.size, b.title.as_str(), b.added), ("pdf", 2, "b", 100));
        assert!(lib.get(&dir.join("k.kfx")).unwrap().unsupported);
        assert_eq!(lib.scan(&[dir], 150), (0, 0), "rescan adds nothing");
        std::fs::remove_file(dir.join("a.epub")).unwrap();
        assert_eq!(lib.scan(&[dir], 200), (0, 1));
        let file = dir.join("library.json");
        lib.save(&file).unwrap();
        let mut expect = lib.clone();
        expect.version = LibraryIndex::VERSION;
        assert_eq!(LibraryIndex::load(&file).unwrap(), expect);
        assert_eq!(LibraryIndex::load(&dir.join("none.json")).unwrap(), LibraryIndex::default());
    }

    /// `get` finds books by path however `books` was changed since the last lookup: in
    /// place (reordered, a path edited), grown, shrunk or replaced; the first of equal
    /// paths wins; `Path` equality (`a//b` is `a/b`) as before.
    #[test]
    fn lookups_follow_every_change_to_the_books() {
        let book = |p: &str| BookEntry { path: PathBuf::from(p), title: p.into(), ..Default::default() };
        let mut lib = LibraryIndex { books: (0..50).map(|i| book(&format!("/b/{i}.epub"))).collect(), ..Default::default() };
        let title = |lib: &LibraryIndex, p: &str| lib.get(Path::new(p)).map(|b| b.title.clone());
        assert_eq!(title(&lib, "/b/7.epub").as_deref(), Some("/b/7.epub"));
        assert_eq!(title(&lib, "/b//7.epub").as_deref(), Some("/b/7.epub"));
        assert_eq!(title(&lib, "/b/50.epub"), None);
        lib.books.swap(3, 40);
        assert_eq!(lib.get(Path::new("/b/3.epub")).unwrap().path, Path::new("/b/3.epub"));
        assert_eq!(lib.get(Path::new("/b/40.epub")).unwrap().path, Path::new("/b/40.epub"));
        lib.books[10].path = PathBuf::from("/b/renamed.epub");
        assert_eq!(title(&lib, "/b/renamed.epub").as_deref(), Some("/b/10.epub"));
        assert_eq!(title(&lib, "/b/10.epub"), None);
        lib.books.push(BookEntry { title: "second".into(), ..book("/b/1.epub") });
        lib.books.push(book("/b/new.epub"));
        assert_eq!(title(&lib, "/b/1.epub").as_deref(), Some("/b/1.epub"));
        assert!(lib.get(Path::new("/b/new.epub")).is_some());
        assert_eq!(lib.remove(Path::new("/b/1.epub")).unwrap().title, "/b/1.epub");
        assert_eq!(title(&lib, "/b/1.epub").as_deref(), Some("second"));
        lib.get_mut(Path::new("/b/2.epub")).unwrap().path = PathBuf::from("/b/two.epub");
        assert!(lib.get(Path::new("/b/2.epub")).is_none() && lib.get(Path::new("/b/two.epub")).is_some());
        lib.books = vec![book("/c/x.epub")];
        assert!(lib.get(Path::new("/b/7.epub")).is_none() && lib.get(Path::new("/c/x.epub")).is_some());
    }

    #[test]
    fn metadata_progress_and_queries() {
        let tmp = TempDir::new("lib-meta");
        let dir = tmp.path();
        for name in ["one.epub", "two.epub", "three.pdf"] {
            std::fs::write(dir.join(name), name).unwrap();
        }
        let mut lib = LibraryIndex::default();
        lib.scan(&[dir], 10);
        assert_eq!(lib.needs_metadata().len(), 3);
        let one = dir.join("one.epub");
        assert!(lib.set_metadata(
            &one,
            BookMeta {
                title: Some("Zebra Tales".into()),
                authors: vec!["Ann Author".into()],
                series: Some("Animals".into()),
                series_index: Some(2.0),
                cover: Some(PathBuf::from("/c/ab/x.png")),
                ..Default::default()
            }
        ));
        assert_eq!(lib.needs_metadata().len(), 2);
        assert!(lib.record_open(&one, 500));
        assert_eq!(lib.get(&one).unwrap().status, ReadingStatus::Reading);
        let md5 = "cf7ce9b8d4e135c8ac340f18d1e58c85";
        let s = BookState { percent: Some(0.5), status: ReadingStatus::Reading, rating: Some(5), last_opened: Some(600), md5: Some(md5.to_uppercase()), ..Default::default() };
        assert!(lib.update_from_state(&one, &s));
        let e = lib.get(&one).unwrap();
        assert_eq!((e.percent, e.rating, e.last_opened, e.md5.as_deref()), (Some(0.5), Some(5), Some(600), Some(md5)));
        assert_eq!(lib.by_md5(md5).unwrap().path, one);
        // An id that is not 32 hex digits (from a file) is not taken over.
        assert!(lib.update_from_state(&one, &BookState { md5: Some("../../x".into()), ..s.clone() }));
        assert_eq!(lib.get(&one).unwrap().md5.as_deref(), Some(md5));
        assert_eq!(lib.recent(5).len(), 1);
        assert_eq!(lib.sorted(SortKey::Title).last().unwrap().title, "Zebra Tales");
        assert_eq!(lib.sorted(SortKey::Author)[0].title, "Zebra Tales");
        assert_eq!(lib.sorted(SortKey::Series)[0].title, "Zebra Tales");
        assert_eq!(lib.sorted(SortKey::Progress)[0].title, "Zebra Tales");
        assert_eq!(lib.search("zebra").len(), 1);
        assert_eq!(lib.search("ANN").len(), 1);
        assert_eq!(lib.with_status(ReadingStatus::New).len(), 2);
        // Collections.
        lib.add_to_collection(&one, "Favourites", 700).unwrap();
        lib.add_to_collection(&dir.join("three.pdf"), "Favourites", 700).unwrap();
        lib.create_collection("Empty", 700).unwrap();
        assert!(lib.create_collection("Empty", 700).is_err());
        assert!(lib.create_collection("  ", 700).is_err());
        assert_eq!(lib.collection_books("Favourites").len(), 2);
        lib.rename_collection("Favourites", "Best").unwrap();
        assert!(lib.rename_collection("Best", "Empty").is_err());
        assert_eq!(lib.collection_books("Best").len(), 2);
        assert!(lib.remove_from_collection(&one, "Best"));
        assert_eq!(lib.collection_books("Best").len(), 1);
        assert!(lib.delete_collection("Best"));
        assert!(lib.collection_books("Best").is_empty());
        assert_eq!(lib.collections.len(), 1);
        assert!(lib.add_to_collection(&dir.join("missing.epub"), "Empty", 1).is_err());
        assert_eq!(lib.remove(&one).unwrap().title, "Zebra Tales");
        assert_eq!(lib.books.len(), 2);
    }

    #[test]
    fn non_utf8_paths_round_trip() {
        let tmp = TempDir::new("lib-hex");
        let file = tmp.path().join("library.json");
        let mut lib = LibraryIndex::default();
        #[cfg(unix)]
        let odd = {
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(std::ffi::OsStr::from_bytes(b"/mnt/us/Books/caf\xe9.epub"))
        };
        #[cfg(not(unix))]
        let odd = PathBuf::from("/mnt/us/Books/odd.epub");
        lib.books.push(BookEntry { path: odd.clone(), cover: Some(odd.with_extension("png")), title: "odd".into(), ..Default::default() });
        lib.books.push(BookEntry { path: PathBuf::from("/mnt/us/Books/ok.epub"), title: "ok".into(), ..Default::default() });
        lib.save(&file).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(r#""path":"/mnt/us/Books/ok.epub""#), "UTF-8 paths stay plain strings: {text}");
        #[cfg(unix)]
        assert!(text.contains(r#""path":{"hex":"2f6d6e742f75732f426f6f6b732f636166e92e65707562"}"#), "{text}");
        let back = LibraryIndex::load(&file).unwrap();
        assert_eq!(back.get(&odd).unwrap().cover, Some(odd.with_extension("png")));
        assert_eq!(back.books.len(), 2);
        // Bad hex: the file does not parse (moved aside, no backup yet: an error).
        std::fs::write(&file, r#"{"version":2,"books":[{"path":{"hex":"zz"}}]}"#).unwrap();
        assert!(LibraryIndex::load(&file).is_err());
    }

    #[test]
    fn corrupt_index_uses_backup() {
        let tmp = TempDir::new("lib-corrupt");
        let file = tmp.path().join("library.json");
        let a = synthetic_index(3);
        a.save(&file).unwrap();
        synthetic_index(4).save(&file).unwrap();
        std::fs::write(&file, b"{\"version\":2,\"books\":[{").unwrap();
        assert_eq!(LibraryIndex::load(&file).unwrap().books.len(), 3);
        std::fs::write(&file, b"nope").unwrap();
        std::fs::write(crate::backup_path(&file), b"nope").unwrap();
        assert!(matches!(LibraryIndex::load(&file), Err(StoreError::Format(_))));
    }

    #[test]
    fn search_keys_follow_edits_and_never_change_equality() {
        let mut lib = LibraryIndex::default();
        lib.books.push(BookEntry { path: "/b/a.epub".into(), title: "Über Alles".into(), authors: vec!["Ann Zebra".into()], series: Some("Saga".into()), ..Default::default() });
        // Not built yet: lowercases on the spot.
        assert_eq!(lib.search("über").len(), 1);
        let plain = lib.clone();
        lib.refresh_search_keys();
        assert_eq!(lib, plain, "the key is not part of equality");
        for q in ["ÜBER", "zebra", "saga", "ann z", ""] {
            assert_eq!(lib.search(q).len(), 1, "{q}");
        }
        // No match across fields, even though the key holds them together.
        assert!(lib.search("alles\0ann").is_empty() && lib.search("allesann").is_empty());
        // A direct edit that changes a length makes the key stale; search notices.
        lib.books[0].title = "Different".into();
        assert!(lib.search("über").is_empty());
        assert_eq!(lib.search("differ").len(), 1);
        lib.books[0].series = None;
        assert!(lib.search("saga").is_empty());
        // set_metadata rebuilds it.
        let p = lib.books[0].path.clone();
        lib.set_metadata(&p, BookMeta { title: Some("New Title".into()), ..Default::default() });
        assert_eq!(lib.books[0].search_key.text, "new title\0ann zebra");
        assert_eq!(lib.search("new t").len(), 1);
    }

    #[test]
    #[ignore = "measurement: time of LibraryIndex::search over 10k books, per query"]
    fn library_10k_search_time() {
        let mut lib = synthetic_index(10_000);
        lib.refresh_search_keys();
        for q in ["number 9999", "zzz", "author 1", "e"] {
            let n = lib.search(q).len();
            let reps = 50u32;
            let t = Instant::now();
            for _ in 0..reps {
                std::hint::black_box(lib.search(std::hint::black_box(q)));
            }
            eprintln!("search {q:?} over 10k: {n} hits, {:.2} ms per query", t.elapsed().as_secs_f64() * 1000.0 / reps as f64);
        }
    }

    /// 10k books: save and load times (debug build here; the release ARM number is in
    /// the selftest output on the emulator).
    #[test]
    fn library_10k_benchmark() {
        let tmp = TempDir::new("lib-10k");
        let file = tmp.path().join("library.json");
        let lib = synthetic_index(10_000);
        let t = Instant::now();
        lib.save(&file).unwrap();
        let save_ms = t.elapsed().as_millis();
        let size = std::fs::metadata(&file).unwrap().len();
        let t = Instant::now();
        let back = LibraryIndex::load(&file).unwrap();
        let load_ms = t.elapsed().as_millis();
        assert_eq!(back.books.len(), 10_000);
        assert_eq!(back, lib);
        let t = Instant::now();
        let recent = back.recent(20).len();
        let sorted = back.sorted(SortKey::Title).len();
        let found = back.search("number 9999").len();
        let query_ms = t.elapsed().as_millis();
        assert_eq!((recent, sorted, found), (20, 10_000, 1));
        eprintln!("library 10k: {size} bytes, save {save_ms} ms, load {load_ms} ms, queries {query_ms} ms");
        // Loose bound for unoptimized builds on a shared machine; catches O(n^2) mistakes.
        assert!(load_ms < 5_000, "load took {load_ms} ms");
        // Scanning 10k files is linear too (the old scan was quadratic).
        let mut big = lib.clone();
        let t = Instant::now();
        let (_, removed) = big.scan(&[tmp.path().join("no-such-dir")], 1);
        assert_eq!(removed, 10_000);
        assert!(t.elapsed().as_millis() < 2_000);
    }
}
