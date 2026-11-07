//! The `stats` plugin: records reading time per page from the reader hooks.
//!
//! Rules (KOReader's statistics plugin defaults):
//! - a page shown for less than [`MIN_PAGE_SECS`] is not recorded (paging through);
//! - a page shown longer than [`MAX_PAGE_SECS`] counts as [`MAX_PAGE_SECS`] (the reader
//!   put the device down);
//! - page numbers in the database are 1-based (`page_stat_data`, as in KOReader); the
//!   hooks give 0-based pages.
//!
//! One session runs from `on_book_open` (or `on_resume`) to `on_close` (or
//! `on_suspend`), so time asleep is never counted. Records are kept in memory and sent to
//! a worker thread every [`FLUSH_EVERY`] pages, on suspend and on close; the worker owns
//! the [`StatsDb`] connection, so SQLite writes (about 70 ms each on the Kindle's
//! `/mnt/us`) stay off the UI thread. Suspend and close wait for the worker (at most
//! [`END_WAIT`]) so the records are on disk before the device sleeps or the app exits.
//! A book without a partial MD5 (`book_id()` is `None`) is not recorded.

use ink_plugin::{HostCtx, Permission, Plugin, PluginManifest, Result};
use ink_store::stats::{PageRecord, StatsBook, StatsDb};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// Plugin name.
pub const NAME: &str = "stats";
/// Shorter page views are not recorded (seconds).
pub const MIN_PAGE_SECS: u64 = 5;
/// Longer page views are cut to this (seconds).
pub const MAX_PAGE_SECS: u64 = 120;
/// Buffered page records that trigger a write.
pub const FLUSH_EVERY: usize = 5;
/// How long suspend and close wait for the worker.
pub const END_WAIT: Duration = Duration::from_secs(3);

/// A wall clock in seconds since the epoch (injected in tests).
pub type ClockFn = Box<dyn Fn() -> u64>;

enum Cmd {
    /// Upsert the book and start a session.
    Open { book: StatsBook, now: u64 },
    /// Write page records of the current session.
    Pages(Vec<PageRecord>),
    /// End the current session; answer on the sender when done.
    End { now: u64, done: Sender<()> },
}

/// The worker thread's side: owns the database and the current session id.
struct Worker {
    path: PathBuf,
    db: Option<StatsDb>,
    session: Option<i64>,
}

impl Worker {
    fn db(&mut self) -> Option<&mut StatsDb> {
        if self.db.is_none() {
            match StatsDb::open_or_recover(&self.path) {
                Ok((db, moved)) => {
                    if let Some(m) = moved {
                        log::warn!("stats: damaged database moved to {}", m.display());
                    }
                    self.db = Some(db);
                }
                Err(e) => log::warn!("stats: cannot open {}: {e}", self.path.display()),
            }
        }
        self.db.as_mut()
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        for cmd in rx {
            match cmd {
                Cmd::Open { book, now } => {
                    self.session = None;
                    let Some(db) = self.db() else { continue };
                    match db.upsert_book(&book, now).and_then(|id| db.start_session(id, now)) {
                        Ok(s) => self.session = Some(s),
                        Err(e) => log::warn!("stats: cannot start a session: {e}"),
                    }
                }
                Cmd::Pages(recs) => {
                    let Some(s) = self.session else { continue };
                    let Some(db) = self.db() else { continue };
                    if let Err(e) = db.record_pages(s, &recs) {
                        log::warn!("stats: cannot write {} page records: {e}", recs.len());
                    }
                }
                Cmd::End { now, done } => {
                    if let Some(s) = self.session.take() {
                        if let Some(db) = self.db() {
                            if let Err(e) = db.end_session(s, now) {
                                log::warn!("stats: cannot end session {s}: {e}");
                            }
                        }
                    }
                    let _ = done.send(());
                }
            }
        }
    }
}

/// The book being read.
struct Current {
    book: StatsBook,
    total_pages: u32,
    /// 0-based page on screen and when it appeared; `None` while suspended.
    page: Option<(usize, u64)>,
    buffer: Vec<PageRecord>,
}

/// The `stats` plugin.
pub struct StatsPlugin {
    manifest: PluginManifest,
    clock: ClockFn,
    db_path: Option<PathBuf>,
    tx: Option<Sender<Cmd>>,
    cur: Option<Current>,
}

impl Default for StatsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl StatsPlugin {
    /// The plugin as compiled into the app: wall clock, database next to the plugin
    /// folder (`<root>/statistics.sqlite3`, found from the data directory).
    pub fn new() -> Self {
        let mut manifest = PluginManifest::new(NAME, env!("CARGO_PKG_VERSION"), &[Permission::Document, Permission::Storage]);
        manifest.description = "Reading statistics: time per page, book and day".into();
        StatsPlugin { manifest, clock: Box::new(crate::unix_now), db_path: None, tx: None, cur: None }
    }

    /// A plugin writing to `db_path` with `clock` (tests).
    pub fn with(db_path: PathBuf, clock: ClockFn) -> Self {
        StatsPlugin { db_path: Some(db_path), clock, ..Self::new() }
    }

    fn send(&mut self, cx: &mut HostCtx, cmd: Cmd) {
        if self.tx.is_none() {
            let path = match &self.db_path {
                Some(p) => p.clone(),
                None => match cx.storage() {
                    Ok(s) => crate::statistics_path_for_data_dir(&s.data_dir()),
                    Err(e) => {
                        log::warn!("stats: {e}");
                        return;
                    }
                },
            };
            log::info!("stats: database {}", path.display());
            let (tx, rx) = channel();
            let worker = Worker { path, db: None, session: None };
            if let Err(e) = std::thread::Builder::new().name("stats".into()).spawn(move || worker.run(rx)) {
                log::warn!("stats: cannot start the worker: {e}");
                return;
            }
            self.tx = Some(tx);
        }
        if let Some(tx) = &self.tx {
            if tx.send(cmd).is_err() {
                log::warn!("stats: the worker stopped");
                self.tx = None;
            }
        }
    }

    /// Ends the timing of the page on screen at `now` and buffers its record.
    fn close_page(&mut self, now: u64) {
        let Some(cur) = self.cur.as_mut() else { return };
        if let Some((page, start)) = cur.page.take() {
            let secs = now.saturating_sub(start);
            if secs >= MIN_PAGE_SECS {
                cur.buffer.push(PageRecord {
                    page: page as u32 + 1,
                    total_pages: cur.total_pages,
                    start_time: start,
                    duration: secs.min(MAX_PAGE_SECS) as u32,
                });
            }
        }
    }

    fn flush(&mut self, cx: &mut HostCtx) {
        let Some(cur) = self.cur.as_mut() else { return };
        if cur.buffer.is_empty() {
            return;
        }
        let recs = std::mem::take(&mut cur.buffer);
        self.send(cx, Cmd::Pages(recs));
    }

    /// Writes what is buffered, ends the session and waits for the worker.
    fn end(&mut self, cx: &mut HostCtx, now: u64) {
        self.close_page(now);
        self.flush(cx);
        if self.tx.is_none() {
            return;
        }
        let (done, wait) = channel();
        self.send(cx, Cmd::End { now, done });
        if wait.recv_timeout(END_WAIT).is_err() {
            log::warn!("stats: the worker did not finish within {END_WAIT:?}");
        }
    }

    fn start(&mut self, cx: &mut HostCtx, now: u64) {
        let Some(cur) = self.cur.as_ref() else { return };
        let book = cur.book.clone();
        self.send(cx, Cmd::Open { book, now });
    }
}

impl Plugin for StatsPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn on_book_open(&mut self, cx: &mut HostCtx) -> Result<()> {
        let now = (self.clock)();
        if self.cur.is_some() {
            self.end(cx, now);
            self.cur = None;
        }
        let doc = cx.document()?;
        let Some(md5) = doc.book_id() else {
            log::info!("stats: book without an id, not recorded");
            return Ok(());
        };
        let m = doc.metadata();
        let total = doc.page_count().unwrap_or(0) as u32;
        let book = StatsBook {
            md5,
            title: m.title,
            authors: m.authors.join("\n"),
            pages: (total > 0).then_some(total),
            series: m.series.map(|(s, _)| s),
            language: m.language,
        };
        self.cur = Some(Current { book, total_pages: total, page: None, buffer: Vec::new() });
        self.start(cx, now);
        Ok(())
    }

    fn on_page_change(&mut self, cx: &mut HostCtx, page: usize) -> Result<()> {
        if self.cur.as_ref().is_none_or(|c| c.page.is_some_and(|(p, _)| p == page)) {
            return Ok(());
        }
        let now = (self.clock)();
        let total = cx.document().ok().and_then(|d| d.page_count()).map(|n| n as u32);
        self.close_page(now);
        let cur = self.cur.as_mut().expect("checked above");
        if let Some(t) = total.filter(|t| *t > 0) {
            cur.total_pages = t;
        }
        cur.page = Some((page, now));
        if cur.buffer.len() >= FLUSH_EVERY {
            self.flush(cx);
        }
        Ok(())
    }

    fn on_suspend(&mut self, cx: &mut HostCtx) -> Result<()> {
        if self.cur.is_some() {
            let now = (self.clock)();
            self.end(cx, now);
        }
        Ok(())
    }

    fn on_resume(&mut self, cx: &mut HostCtx) -> Result<()> {
        if self.cur.is_none() {
            return Ok(());
        }
        let now = (self.clock)();
        let page = cx.document().ok().and_then(|d| d.current_page());
        self.start(cx, now);
        if let Some(cur) = self.cur.as_mut() {
            cur.page = page.map(|p| (p, now));
        }
        Ok(())
    }

    fn on_close(&mut self, cx: &mut HostCtx) -> Result<()> {
        if self.cur.is_some() {
            let now = (self.clock)();
            self.end(cx, now);
            self.cur = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_doc::Metadata;
    use ink_plugin::testing::FakeBook;
    use ink_plugin::{MemoryStorage, UiQueue};
    use std::cell::Cell;
    use std::rc::Rc;

    struct Env {
        ui: UiQueue,
        store: MemoryStorage,
        book: FakeBook,
        plugin: StatsPlugin,
        t: Rc<Cell<u64>>,
        db: PathBuf,
    }

    const MD5: &str = "0123456789abcdef0123456789abcdef";

    impl Env {
        fn new(test: &str, id: Option<&str>) -> Self {
            let dir = std::env::temp_dir().join(format!("ink-stats-rec-{}-{test}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let db = dir.join("statistics.sqlite3");
            let t = Rc::new(Cell::new(1_700_000_000));
            let t2 = t.clone();
            let plugin = StatsPlugin::with(db.clone(), Box::new(move || t2.get()));
            let metadata = Metadata { title: "Romeo and Juliet".into(), authors: vec!["William Shakespeare".into()], ..Metadata::default() };
            let book = FakeBook { metadata, id: id.map(String::from), page: Some(0), pages: Some(100), ..FakeBook::default() };
            Env { ui: UiQueue::default(), store: MemoryStorage::default(), book, plugin, t, db }
        }
        fn call(&mut self, f: impl FnOnce(&mut StatsPlugin, &mut HostCtx) -> Result<()>) {
            let perms = self.plugin.manifest().permissions.clone();
            let mut cx = HostCtx::new(&mut self.ui, &mut self.store, &perms).with_doc(Some(&mut self.book));
            f(&mut self.plugin, &mut cx).unwrap();
        }
        fn page(&mut self, p: usize, wait: u64) {
            self.book.page = Some(p);
            self.call(|pl, cx| pl.on_page_change(cx, p));
            self.t.set(self.t.get() + wait);
        }
    }

    #[test]
    fn records_pages_sessions_and_caps() {
        let mut e = Env::new("basic", Some(MD5));
        e.call(|p, cx| p.on_book_open(cx));
        e.page(0, 30); // recorded: 30 s
        e.page(0, 0); // same page again: ignored (repaint)
        e.page(1, 2); // too short: not recorded
        e.page(2, 600); // capped at 120 s
        e.page(3, 10);
        e.call(|p, cx| p.on_close(cx));
        let db = StatsDb::open(&e.db).unwrap();
        let recs = db.page_stats(MD5).unwrap();
        let pages: Vec<(u32, u32, u32)> = recs.iter().map(|r| (r.page, r.duration, r.total_pages)).collect();
        assert_eq!(pages, vec![(1, 30, 100), (3, 120, 100), (4, 10, 100)]);
        let t = db.book_totals(MD5).unwrap().unwrap();
        assert_eq!(t.title, "Romeo and Juliet");
        assert_eq!(t.authors, "William Shakespeare");
        assert_eq!(t.read_time, 160);
        assert_eq!(t.pages_read, 3);
        assert_eq!(t.sessions, 1);
    }

    #[test]
    fn suspend_ends_the_session_and_sleep_is_not_counted() {
        let mut e = Env::new("suspend", Some(MD5));
        e.call(|p, cx| p.on_book_open(cx));
        e.page(5, 20);
        e.call(|p, cx| p.on_suspend(cx));
        e.t.set(e.t.get() + 3600); // asleep for an hour
        e.call(|p, cx| p.on_resume(cx));
        e.t.set(e.t.get() + 40);
        e.page(6, 15);
        e.call(|p, cx| p.on_close(cx));
        let db = StatsDb::open(&e.db).unwrap();
        let recs: Vec<(u32, u32)> = db.page_stats(MD5).unwrap().iter().map(|r| (r.page, r.duration)).collect();
        assert_eq!(recs, vec![(6, 20), (6, 40), (7, 15)]);
        let sessions = db.sessions(Some(MD5), 10).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions.iter().map(|s| s.duration).sum::<u64>(), 75);
    }

    #[test]
    fn flushes_in_batches_before_close() {
        let mut e = Env::new("batch", Some(MD5));
        e.call(|p, cx| p.on_book_open(cx));
        for p in 0..=FLUSH_EVERY {
            e.page(p, 10);
        }
        e.page(FLUSH_EVERY + 1, 10);
        // The worker writes asynchronously; wait a little for the batch.
        let mut n = 0;
        for _ in 0..100 {
            n = StatsDb::open(&e.db).map(|db| db.page_stats(MD5).unwrap().len()).unwrap_or(0);
            if n >= FLUSH_EVERY {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(n >= FLUSH_EVERY, "batch not written before close: {n}");
        e.call(|p, cx| p.on_close(cx));
        assert_eq!(StatsDb::open(&e.db).unwrap().page_stats(MD5).unwrap().len(), FLUSH_EVERY + 2);
    }

    #[test]
    fn book_without_id_is_not_recorded() {
        let mut e = Env::new("noid", None);
        e.call(|p, cx| p.on_book_open(cx));
        e.page(0, 30);
        e.page(1, 30);
        e.call(|p, cx| p.on_close(cx));
        assert!(!e.db.exists());
    }

    #[test]
    fn opening_another_book_ends_the_first() {
        let mut e = Env::new("two", Some(MD5));
        e.call(|p, cx| p.on_book_open(cx));
        e.page(0, 30);
        e.book.id = Some("fedcba9876543210fedcba9876543210".into());
        e.call(|p, cx| p.on_book_open(cx));
        e.page(0, 50);
        e.call(|p, cx| p.on_close(cx));
        let db = StatsDb::open(&e.db).unwrap();
        assert_eq!(db.book_totals(MD5).unwrap().unwrap().read_time, 30);
        assert_eq!(db.book_totals("fedcba9876543210fedcba9876543210").unwrap().unwrap().read_time, 50);
    }
}
