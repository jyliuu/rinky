//! The library data behind the screen: `library.json` (ink-store's [`LibraryIndex`]), the
//! scan of the book folders, metadata and cover extraction (ink-formats `read_info`,
//! MuPDF for PDF/CBZ/XPS), queries (sort, filter, search, collection), reading status,
//! delete and the KOReader import. Nothing here draws; everything is host-testable.
//!
//! Errors while saving `library.json` or a cover are logged and the screen goes on:
//! the library must never end the app because the user store is full or read-only.

use super::covers;
use ink_doc::Provider;
use ink_store::koreader::{self, KoreaderDirs};
use ink_store::library::{BookMeta, SortKey};
use ink_store::{BookEntry, BookState, LibraryIndex, Paths, ReadingStatus};
use std::collections::VecDeque;
use std::sync::mpsc;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Status filter of the library view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusFilter {
    /// Every book.
    All,
    /// Never opened.
    Unread,
    /// Being read (also "on hold").
    Reading,
    /// Finished.
    Finished,
}

impl StatusFilter {
    /// Every filter in menu order.
    pub const ALL: [StatusFilter; 4] = [StatusFilter::All, StatusFilter::Unread, StatusFilter::Reading, StatusFilter::Finished];

    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            StatusFilter::All => "All books",
            StatusFilter::Unread => "Unread",
            StatusFilter::Reading => "Reading",
            StatusFilter::Finished => "Finished",
        }
    }

    /// The same rule as the cover badge ([`super::browse::shows_progress`]): a book
    /// opened but still at 0 % is "Unread" (status `Reading`, no progress bar), not
    /// "Reading".
    fn matches(self, b: &BookEntry) -> bool {
        let opened = matches!(b.status, ReadingStatus::Reading | ReadingStatus::Abandoned);
        match self {
            StatusFilter::All => true,
            StatusFilter::Unread => b.status == ReadingStatus::New || (opened && !super::browse::shows_progress(b)),
            StatusFilter::Reading => opened && super::browse::shows_progress(b),
            StatusFilter::Finished => b.status == ReadingStatus::Complete,
        }
    }
}

/// Sort orders offered in the menu.
pub const SORTS: [(SortKey, &str); 4] =
    [(SortKey::Recent, "Recent"), (SortKey::Title, "Title"), (SortKey::Author, "Author"), (SortKey::Added, "Date added")];

/// Label of a sort key.
pub fn sort_label(k: SortKey) -> &'static str {
    SORTS.iter().find(|(s, _)| *s == k).map(|(_, l)| *l).unwrap_or("Recent")
}

/// What the library view shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    /// Order.
    pub sort: SortKey,
    /// Reading status filter.
    pub status: StatusFilter,
    /// Format filter (lowercase extension), `None` = every format.
    pub format: Option<String>,
    /// Search text (title, author, series), empty = none.
    pub search: String,
    /// Only books of this collection.
    pub collection: Option<String>,
}

impl Default for Query {
    fn default() -> Self {
        Query { sort: SortKey::Recent, status: StatusFilter::All, format: None, search: String::new(), collection: None }
    }
}

/// Human label of a reading status.
pub fn status_label(s: ReadingStatus) -> &'static str {
    match s {
        ReadingStatus::New => "Unread",
        ReadingStatus::Reading => "Reading",
        ReadingStatus::Complete => "Finished",
        ReadingStatus::Abandoned => "On hold",
    }
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    ink_store::now_secs()
}

/// Book formats MuPDF opens (metadata and the cover come from the first page).
fn is_fixed_layout(path: &Path) -> bool {
    ink_mupdf::MupdfProvider.can_open(path)
}

/// The library: index plus the work still to do on it.
pub struct Library {
    /// The index (books, collections).
    pub index: LibraryIndex,
    paths: Paths,
    dirs: Vec<PathBuf>,
    pending: VecDeque<PathBuf>,
    unsaved: bool,
    /// Folders the last scan could not read (shown on the import/info page).
    pub unreadable: Vec<PathBuf>,
    /// Metadata read on a background thread ([`Library::start_background`]).
    worker: Option<MetaWorker>,
}

impl Library {
    /// Loads `library.json` (a broken file gives an empty index, logged) and scans `dirs`.
    pub fn open(paths: &Paths, dirs: &[PathBuf]) -> Library {
        let index = match LibraryIndex::load(&paths.library()) {
            Ok(i) => i,
            Err(e) => {
                log::warn!("library: cannot load {}: {e}", paths.library().display());
                LibraryIndex::default()
            }
        };
        let mut lib = Library { index, paths: paths.clone(), dirs: dirs.to_vec(), pending: VecDeque::new(), unsaved: false, unreadable: Vec::new(), worker: None };
        lib.rescan();
        lib
    }

    /// Sets the folders scanned by the next [`Library::rescan`].
    pub fn set_dirs(&mut self, dirs: &[PathBuf]) {
        self.dirs = dirs.to_vec();
    }

    /// Scans the folders again (new, changed and removed files) and queues metadata work.
    pub fn rescan(&mut self) {
        let t = Instant::now();
        let r = self.index.scan_report(&self.dirs, now());
        for e in &r.errors {
            log::warn!("library scan: {e}");
        }
        self.unreadable = r.unreadable;
        log::info!("library scan: {} books, {} added, {} removed in {:?}", self.index.books.len(), r.added, r.removed, t.elapsed());
        self.pending = self.index.needs_metadata().into_iter().map(Path::to_path_buf).collect();
        self.unsaved |= r.added > 0 || r.removed > 0;
    }

    /// Books still waiting for metadata.
    pub fn pending(&self) -> usize {
        self.pending.len() + self.worker.as_ref().map_or(0, |w| w.left)
    }

    /// Extracts metadata and covers until `budget` is used up or nothing is left.
    /// Returns how many books were done. Saves the index when the queue empties.
    pub fn extract_some(&mut self, budget: Duration) -> usize {
        let t = Instant::now();
        let mut n = 0;
        while let Some(p) = self.pending.pop_front() {
            self.extract(&p);
            n += 1;
            if t.elapsed() >= budget {
                break;
            }
        }
        if n > 0 {
            self.unsaved = true;
        }
        if self.pending.is_empty() {
            self.save();
        }
        n
    }

    /// Reads one book's metadata, cover and sidecar state into the index.
    pub fn extract(&mut self, path: &Path) {
        let Some(entry) = self.index.get(path) else { return };
        let (md5, meta) = read_meta(&self.paths, path, entry.md5.clone());
        self.apply_meta(path, md5, meta);
    }

    /// Puts metadata read by [`read_meta`] into the index (a book removed meanwhile is
    /// skipped).
    fn apply_meta(&mut self, path: &Path, md5: Option<String>, meta: BookMeta) {
        if self.index.get(path).is_none() {
            return;
        }
        self.index.set_metadata(path, meta);
        if let Some(b) = self.index.get_mut(path) {
            if b.md5.is_none() {
                b.md5 = md5;
            }
        }
        self.refresh_state(path);
    }

    /// Starts reading the metadata and covers of every pending book on a background
    /// thread (nice 10 on Linux): the screen paints at once and stays responsive.
    /// [`Library::take_background`] puts the results into the index. A rescan that
    /// queues more books while it runs gets them read by the next start.
    pub fn start_background(&mut self) {
        if self.worker.is_some() || self.pending.is_empty() {
            return;
        }
        // Only books that still need it (a rescan during the last run queued the books
        // that thread was reading).
        let jobs: Vec<(PathBuf, Option<String>)> = self
            .pending
            .drain(..)
            .filter_map(|p| {
                let e = self.index.get(&p)?;
                (e.meta_mtime == 0 || e.meta_mtime != e.mtime).then(|| (p.clone(), e.md5.clone()))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let back: Vec<PathBuf> = jobs.iter().map(|(p, _)| p.clone()).collect();
        let left = jobs.len();
        let (tx, rx) = mpsc::channel();
        let paths = self.paths.clone();
        let spawned = std::thread::Builder::new().name("library-meta".into()).spawn(move || {
            ink_html::paginate::lower_priority();
            let t = Instant::now();
            let n = jobs.len();
            for (p, md5) in jobs {
                let (md5, meta) = read_meta(&paths, &p, md5);
                if tx.send((p, md5, meta)).is_err() {
                    return;
                }
            }
            log::info!("library: metadata of {n} books read in {:?} (background)", t.elapsed());
        });
        match spawned {
            Ok(_) => self.worker = Some(MetaWorker { rx, left }),
            Err(e) => {
                log::warn!("library: no metadata thread ({e}); reading between events");
                self.pending.extend(back);
            }
        }
    }

    /// Puts the books the background thread finished into the index. Returns how many.
    /// Saves the index when nothing is left.
    pub fn take_background(&mut self) -> usize {
        let Some(w) = self.worker.as_mut() else { return 0 };
        let mut done = Vec::new();
        let mut ended = false;
        loop {
            match w.rx.try_recv() {
                Ok(r) => done.push(r),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    ended = true;
                    break;
                }
            }
        }
        w.left = w.left.saturating_sub(done.len());
        if ended || w.left == 0 {
            if w.left > 0 {
                log::warn!("library: metadata thread stopped with {} books left", w.left);
            }
            self.worker = None;
        }
        let n = done.len();
        for (p, md5, meta) in done {
            self.apply_meta(&p, md5, meta);
        }
        if n > 0 {
            self.unsaved = true;
        }
        if self.worker.is_none() && self.pending.is_empty() {
            self.save();
        }
        n
    }
}

/// Metadata of one book read off the UI thread: (book, partial MD5, metadata).
type MetaResult = (PathBuf, Option<String>, BookMeta);

/// The background metadata reader of [`Library::start_background`].
struct MetaWorker {
    rx: mpsc::Receiver<MetaResult>,
    /// Books not received yet.
    left: usize,
}

/// Reads one book's partial MD5 (unless known), metadata and cover (written to the
/// cover cache). Touches no index: runs on the metadata thread too.
fn read_meta(paths: &Paths, path: &Path, md5: Option<String>) -> (Option<String>, BookMeta) {
    let mut md5 = md5;
    if md5.is_none() {
        md5 = ink_store::partial_md5(path).ok();
    }
    let key = md5.clone().unwrap_or_else(|| path.display().to_string());
    let cover_path = paths.cover(&key);
    let mut meta = BookMeta::default();
    if is_fixed_layout(path) {
        match ink_mupdf::MupdfDocument::open(path, ink_mupdf::FixedOptions::default()) {
            Ok(doc) => {
                use ink_doc::Document as _;
                let m = doc.metadata();
                meta.title = Some(m.title.clone()).filter(|t| !t.trim().is_empty());
                meta.authors = m.authors.clone();
                meta.pages = u32::try_from(doc.page_count().pages).ok();
                match doc.thumbnail(covers::THUMB_W, covers::THUMB_H) {
                    Ok(c) => meta.cover = covers::save(&c, &cover_path),
                    Err(e) => log::info!("library: no cover for {}: {e}", path.display()),
                }
            }
            Err(e) => log::warn!("library: cannot read {}: {e}", path.display()),
        }
    } else {
        match ink_formats::read_info(path) {
            Ok(info) => {
                let m = &info.metadata;
                meta.title = Some(m.title.clone()).filter(|t| !t.trim().is_empty());
                meta.authors = m.authors.clone();
                meta.language = m.language.clone();
                meta.publisher = m.publisher.clone();
                if let Some((s, i)) = &m.series {
                    meta.series = Some(s.clone());
                    meta.series_index = Some(*i);
                }
                meta.unsupported = info.refusal.is_some();
                if let Some(c) = &info.cover {
                    match covers::from_bytes(&c.bytes) {
                        Some(img) => meta.cover = covers::save(&img, &cover_path),
                        None => log::info!("library: cover of {} not decoded ({})", path.display(), c.media_type),
                    }
                }
            }
            Err(e) => log::warn!("library: no metadata for {}: {e}", path.display()),
        }
    }
    (md5, meta)
}

impl Library {
    /// Copies progress and status from the book's sidecar into the index (after the
    /// reader closed it, or on the first scan).
    pub fn refresh_state(&mut self, path: &Path) {
        let Ok(state) = BookState::load(path) else { return };
        let before = self.index.get(path).map(|b| (b.status, b.last_opened));
        self.index.update_from_state(path, &state);
        // The reader saves positions but no status: a book that was opened stays
        // "Reading" instead of falling back to the sidecar's default "New".
        if let (Some((old, opened)), Some(b)) = (before, self.index.get_mut(path)) {
            if b.status == ReadingStatus::New && (old != ReadingStatus::New || opened.is_some() || state.position.is_some()) {
                b.status = if old == ReadingStatus::New { ReadingStatus::Reading } else { old };
            }
        }
        self.unsaved = true;
    }

    /// Marks a book opened now (before the reader is pushed).
    pub fn record_open(&mut self, path: &Path) {
        if self.index.record_open(path, now()) {
            self.unsaved = true;
            self.save();
        }
    }

    /// Sets the reading status in the index and in the book's sidecar.
    pub fn set_status(&mut self, path: &Path, status: ReadingStatus) {
        if let Some(b) = self.index.get_mut(path) {
            b.status = status;
            if status == ReadingStatus::Complete {
                b.percent.get_or_insert(1.0);
            }
        }
        match BookState::load(path) {
            Ok(mut s) => {
                s.status = status;
                if let Err(e) = s.save(path) {
                    log::warn!("library: cannot save status of {}: {e}", path.display());
                }
            }
            Err(e) => log::warn!("library: cannot load state of {}: {e}", path.display()),
        }
        self.unsaved = true;
        self.save();
    }

    /// Saves `library.json` if something changed (errors are logged).
    pub fn save(&mut self) {
        if !self.unsaved {
            return;
        }
        let path = self.paths.library();
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        match self.index.save(&path) {
            Ok(()) => self.unsaved = false,
            Err(e) => log::warn!("library: cannot save {}: {e}", path.display()),
        }
    }

    /// The books `q` selects, in its order.
    pub fn query(&self, q: &Query) -> Vec<&BookEntry> {
        let s = q.search.trim().to_lowercase();
        self.index
            .sorted(q.sort)
            .into_iter()
            .filter(|b| q.status.matches(b))
            .filter(|b| q.format.as_deref().is_none_or(|f| b.format == f))
            .filter(|b| q.collection.as_deref().is_none_or(|c| b.collections.iter().any(|n| n == c)))
            // The lowercase text of each book is cached in the index (search_key).
            .filter(|b| s.is_empty() || b.matches(&s))
            .collect()
    }

    /// The book being read: the most recently opened one that is not finished.
    pub fn current(&self) -> Option<&BookEntry> {
        self.index.recent(1).into_iter().next().filter(|b| b.status != ReadingStatus::Complete)
    }

    /// Formats present in the library, sorted.
    pub fn formats(&self) -> Vec<String> {
        let mut v: Vec<String> = self.index.books.iter().map(|b| b.format.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Deletes a book: the file, our sidecar (and its backup), the cover thumbnail and
    /// the `.sdr` folder only when that leaves it empty (KOReader's files in it are
    /// never touched), then the index entry.
    pub fn delete_book(&mut self, path: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let sc = ink_store::sidecar::sidecar_path(path);
        for f in [sc.clone(), ink_store::backup_path(&sc)] {
            if let Err(e) = std::fs::remove_file(&f) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::warn!("library: cannot remove {}: {e}", f.display());
                }
            }
        }
        if let Some(dir) = sc.parent() {
            // Fails (and keeps the folder) when anything else is in it.
            let _ = std::fs::remove_dir(dir);
        }
        if let Some(cover) = self.index.get(path).and_then(|b| b.cover.clone()) {
            if cover.starts_with(self.paths.covers()) {
                let _ = std::fs::remove_file(cover);
            }
        }
        self.pending.retain(|p| p != path);
        self.index.remove(path);
        self.unsaved = true;
        self.save();
        Ok(())
    }

    /// Creates a collection.
    pub fn create_collection(&mut self, name: &str) -> Result<(), String> {
        self.index.create_collection(name, now()).map_err(|e| e.to_string())?;
        self.unsaved = true;
        self.save();
        Ok(())
    }

    /// Renames a collection.
    pub fn rename_collection(&mut self, old: &str, new: &str) -> Result<(), String> {
        self.index.rename_collection(old, new.trim()).map_err(|e| e.to_string())?;
        self.unsaved = true;
        self.save();
        Ok(())
    }

    /// Deletes a collection (its books stay).
    pub fn delete_collection(&mut self, name: &str) {
        if self.index.delete_collection(name) {
            self.unsaved = true;
            self.save();
        }
    }

    /// Adds `path` to `name` or removes it when it is already there. Returns whether it
    /// is in the collection afterwards.
    pub fn toggle_collection(&mut self, path: &Path, name: &str) -> bool {
        let inside = self.index.get(path).is_some_and(|b| b.collections.iter().any(|c| c == name));
        let now_in = if inside {
            self.index.remove_from_collection(path, name);
            false
        } else {
            match self.index.add_to_collection(path, name, now()) {
                Ok(()) => true,
                Err(e) => {
                    log::warn!("library: {e}");
                    false
                }
            }
        };
        self.unsaved = true;
        self.save();
        now_in
    }

    /// Imports KOReader's data (positions, highlights, bookmarks, status, rating) for
    /// every book into our sidecars, and KOReader's reading statistics into
    /// `statistics.sqlite3`. KOReader's files are only read.
    pub fn import_koreader(&mut self, ko: &KoreaderDirs) -> ImportResult {
        self.import_koreader_with(ko, &mut |_, _| {})
    }

    /// [`Library::import_koreader`], calling `progress(done, total)` before each book and
    /// `progress(total, total)` before the statistics.
    pub fn import_koreader_with(&mut self, ko: &KoreaderDirs, progress: &mut dyn FnMut(usize, usize)) -> ImportResult {
        let mut r = ImportResult::default();
        let books: Vec<PathBuf> = self.index.books.iter().map(|b| b.path.clone()).collect();
        for (i, p) in books.iter().enumerate() {
            progress(i, books.len());
            match koreader::import_book(p, Some(ko), false) {
                Ok(Some(sum)) => {
                    r.books += 1;
                    r.positions += usize::from(sum.position);
                    r.highlights += sum.highlights;
                    r.bookmarks += sum.bookmarks;
                    self.refresh_state(p);
                }
                Ok(None) => {}
                Err(e) => r.errors.push(format!("{}: {e}", p.display())),
            }
        }
        progress(books.len(), books.len());
        let db = ko.statistics();
        if db.exists() {
            let tmp = self.paths.tmp();
            let _ = std::fs::create_dir_all(&tmp);
            match koreader::read_statistics_db(&db, Some(&tmp)) {
                Ok(stats) => match ink_store::StatsDb::open(&self.paths.statistics()).and_then(|mut s| s.import_koreader(&stats)) {
                    Ok(c) => r.statistics = Some(format!("{} books, {} page records, {} sessions", c.books, c.pages, c.sessions)),
                    Err(e) => r.errors.push(format!("statistics: {e}")),
                },
                Err(e) => r.errors.push(format!("{}: {e}", db.display())),
            }
        }
        self.unsaved = true;
        self.save();
        r
    }
}

/// What [`Library::import_koreader`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportResult {
    /// Books KOReader had data for.
    pub books: usize,
    /// Reading positions taken over.
    pub positions: usize,
    /// Highlights and notes added.
    pub highlights: usize,
    /// Bookmarks added.
    pub bookmarks: usize,
    /// Statistics import counts, when KOReader had a statistics database.
    pub statistics: Option<String>,
    /// One line per failure.
    pub errors: Vec<String>,
}

impl ImportResult {
    /// Lines for the import page.
    pub fn lines(&self) -> Vec<String> {
        let mut v = vec![
            format!("Books with KOReader data: {}", self.books),
            format!("Reading positions: {}", self.positions),
            format!("Highlights and notes: {}", self.highlights),
            format!("Bookmarks: {}", self.bookmarks),
        ];
        v.push(match &self.statistics {
            Some(s) => format!("Statistics: {s}"),
            None => "Statistics: no KOReader database".into(),
        });
        if !self.errors.is_empty() {
            v.push(format!("Errors: {}", self.errors.len()));
            v.extend(self.errors.iter().take(3).cloned());
        }
        v
    }
}

/// View preferences kept in `library-view.json` next to `library.json`: sort, filters
/// and list/grid. Search text and collection are not kept.
pub fn prefs_path(paths: &Paths) -> PathBuf {
    paths.root.join("library-view.json")
}

/// Reads the view preferences into `q`; returns whether the grid was on. A missing or
/// broken file keeps the defaults.
pub fn load_prefs(path: &Path, q: &mut Query) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        log::warn!("library: ignoring broken {}", path.display());
        return false;
    };
    if let Some(s) = v.get("sort").and_then(|s| s.as_str()) {
        if let Some((k, _)) = SORTS.iter().find(|(_, l)| *l == s) {
            q.sort = *k;
        }
    }
    if let Some(s) = v.get("status").and_then(|s| s.as_str()) {
        if let Some(f) = StatusFilter::ALL.iter().find(|f| f.label() == s) {
            q.status = *f;
        }
    }
    q.format = v.get("format").and_then(|s| s.as_str()).filter(|s| !s.is_empty() && s.len() <= 16).map(str::to_string);
    v.get("grid").and_then(|g| g.as_bool()).unwrap_or(false)
}

/// Writes the view preferences (errors are logged).
pub fn save_prefs(path: &Path, q: &Query, grid: bool) {
    let v = serde_json::json!({
        "sort": sort_label(q.sort),
        "status": q.status.label(),
        "format": q.format.clone().unwrap_or_default(),
        "grid": grid,
    });
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Err(e) = ink_store::write_atomic(path, v.to_string().as_bytes()) {
        log::warn!("library: cannot save {}: {e}", path.display());
    }
}
