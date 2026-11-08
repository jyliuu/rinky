//! rinky's reading statistics: `/mnt/us/rinky/statistics.sqlite3`.
//!
//! Tables:
//! - `book` and `page_stat_data`: the same columns as KOReader's statistics plugin, so
//!   KOReader data imports as a plain copy ([`StatsDb::import_koreader`]) and queries
//!   written for KOReader's database work here.
//! - `session`: one row per reading session (book, start, end, seconds, pages, source
//!   `rinky` or `koreader`). Page records belong to the session whose time range
//!   holds their start time.
//!
//! The database uses `journal_mode=TRUNCATE` (the Kindle's `/mnt/us` cannot memory-map
//! a WAL index; KOReader makes the same choice) and a busy timeout. Every write is one
//! `BEGIN IMMEDIATE` transaction (a deferred one that reads first cannot wait for another
//! writer: SQLite fails it at once with "database is locked"), and the connections of one
//! process that write to the same file take turns in arrival order, so a writer cannot
//! be starved by others that write back to back (SQLite's busy handler only polls).
//! Page records can be
//! written one by one ([`StatsDb::record_page`]) or in batches ([`StatsDb::record_pages`],
//! one transaction). Each transaction syncs to disk: on firmware 5.19.5 `/mnt/us` is a
//! FUSE file system (`fuse.fsp`) and one transaction took about 70 ms in the emulator, so
//! callers should keep page records in memory and write them in batches (every few
//! pages, on suspend and on close), as KOReader's statistics plugin does. Times are
//! seconds since the Unix epoch and are passed in by the caller.

use crate::koreader::KoStatistics;
use crate::{Result, StoreError};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

/// How long one write waits for another process's write (SQLite busy timeout).
pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A first-come, first-served queue of the writers of one database file in this process
/// (a ticket lock). Unlike SQLite's busy handler, which sleeps and polls, a writer that
/// arrives first writes first.
#[derive(Debug, Default)]
struct WriteQueue {
    /// (next ticket, ticket being served).
    state: Mutex<(u64, u64)>,
    turn: Condvar,
}

/// A writer's turn; the next writer goes when it is dropped (also on panic).
struct Turn(Arc<WriteQueue>);

fn lock_state(q: &WriteQueue) -> MutexGuard<'_, (u64, u64)> {
    q.state.lock().unwrap_or_else(|e| e.into_inner())
}

impl WriteQueue {
    /// The queue of the database at `path` (one per file in this process).
    fn for_path(path: &Path) -> Arc<WriteQueue> {
        static QUEUES: OnceLock<Mutex<HashMap<PathBuf, Arc<WriteQueue>>>> = OnceLock::new();
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let mut all = QUEUES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        all.entry(key).or_default().clone()
    }

    fn wait_turn(self: &Arc<Self>) -> Turn {
        let mut st = lock_state(self);
        let ticket = st.0;
        st.0 += 1;
        while st.1 != ticket {
            st = self.turn.wait(st).unwrap_or_else(|e| e.into_inner());
        }
        Turn(self.clone())
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        lock_state(&self.0).1 += 1;
        self.0.turn.notify_all();
    }
}

/// Our schema version (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 1;

/// Gap that ends a session when sessions are rebuilt from page records (KOReader data).
pub const SESSION_GAP_SECS: u64 = 600;

const SCHEMA: &str = concat!(
    crate::koreader::sqlite::schema::book_table!(),
    r#"
    CREATE UNIQUE INDEX IF NOT EXISTS book_title_authors_md5 ON book(title, authors, md5);
    CREATE INDEX IF NOT EXISTS book_md5 ON book(md5);
    CREATE TABLE IF NOT EXISTS page_stat_data
        (
            id_book     integer,
            page        integer NOT NULL DEFAULT 0,
            start_time  integer NOT NULL DEFAULT 0,
            duration    integer NOT NULL DEFAULT 0,
            total_pages integer NOT NULL DEFAULT 0,
            UNIQUE (id_book, page, start_time),
            FOREIGN KEY(id_book) REFERENCES book(id)
        );
    CREATE INDEX IF NOT EXISTS page_stat_data_start_time ON page_stat_data(start_time);
    CREATE TABLE IF NOT EXISTS session
        (
            id         integer PRIMARY KEY autoincrement,
            id_book    integer NOT NULL REFERENCES book(id),
            start_time integer NOT NULL,
            end_time   integer NOT NULL,
            duration   integer NOT NULL DEFAULT 0,
            pages      integer NOT NULL DEFAULT 0,
            source     text NOT NULL DEFAULT 'rinky'
        );
    CREATE INDEX IF NOT EXISTS session_book_start ON session(id_book, start_time);
"#
);

/// A book as the statistics know it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsBook {
    /// Partial MD5 (the key).
    pub md5: String,
    /// Title.
    pub title: String,
    /// Authors (newline-separated, as KOReader stores them).
    pub authors: String,
    /// Page count in the current layout.
    pub pages: Option<u32>,
    /// Series.
    pub series: Option<String>,
    /// Language.
    pub language: Option<String>,
}

/// One page record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageRecord {
    /// Page (1-based, as KOReader).
    pub page: u32,
    /// Pages in the book in the layout used.
    pub total_pages: u32,
    /// When the page was shown.
    pub start_time: u64,
    /// Seconds on the page.
    pub duration: u32,
}

/// A reading session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    /// Row id.
    pub id: i64,
    /// Book md5.
    pub md5: String,
    /// Start.
    pub start_time: u64,
    /// End (last page start + duration).
    pub end_time: u64,
    /// Seconds read (sum of page durations).
    pub duration: u64,
    /// Distinct pages read.
    pub pages: u64,
    /// `rinky` or `koreader`.
    pub source: String,
}

/// Totals for one book.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookTotals {
    /// Book md5.
    pub md5: String,
    /// Title.
    pub title: String,
    /// Authors.
    pub authors: String,
    /// Page count.
    pub pages: Option<u32>,
    /// Seconds read.
    pub read_time: u64,
    /// Distinct pages read.
    pub pages_read: u64,
    /// Number of sessions.
    pub sessions: u64,
    /// First page record.
    pub first_read: Option<u64>,
    /// Last page record end.
    pub last_read: Option<u64>,
}

/// Reading on one calendar day.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DayTotal {
    /// `YYYY-MM-DD` in the caller's time zone.
    pub day: String,
    /// Seconds read.
    pub seconds: u64,
    /// Page records (page turns).
    pub pages: u64,
    /// Distinct books.
    pub books: u64,
}

/// What [`StatsDb::import_koreader`] added.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportCounts {
    /// Books matched or created.
    pub books: usize,
    /// New page records (already present ones are skipped).
    pub pages: usize,
    /// Sessions rebuilt from KOReader's records.
    pub sessions: usize,
}

/// The statistics database.
#[derive(Debug)]
pub struct StatsDb {
    conn: Connection,
    queue: Arc<WriteQueue>,
}

fn u(v: i64) -> u64 {
    v.max(0) as u64
}

fn u32_of(v: i64) -> u32 {
    v.clamp(0, u32::MAX as i64) as u32
}

impl StatsDb {
    /// Opens (creating if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        crate::create_parent_dir(path)?;
        let conn = Connection::open(path)?;
        Self::init(conn, WriteQueue::for_path(path))
    }

    /// [`StatsDb::open`], but a file that is not a database or is damaged (SQLite's
    /// `NOTADB`, `CORRUPT`) is moved aside to `<name>.corrupt-<secs>-<n>` (with its
    /// `-journal`) and a new database is started; the new name is returned. Other errors
    /// (locked, read-only, a newer schema) are returned and nothing is moved.
    pub fn open_or_recover(path: &Path) -> Result<(Self, Option<PathBuf>)> {
        crate::open_db_or_recover(path, Self::open)
    }

    /// An in-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, Arc::default())
    }

    fn init(conn: Connection, queue: Arc<WriteQueue>) -> Result<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        let mut db = StatsDb { conn, queue };
        let _turn = db.queue.wait_turn();
        db.conn.query_row("PRAGMA journal_mode=TRUNCATE", [], |_| Ok(()))?;
        db.conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
        let tx = db.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let v: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            return Err(StoreError::Format(format!("statistics schema {v} is newer than {SCHEMA_VERSION}")));
        }
        if v < SCHEMA_VERSION {
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION}"))?;
        }
        tx.commit()?;
        drop(_turn);
        Ok(db)
    }

    /// The underlying connection, for queries this API does not cover.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// This process's turn to write to the file, then a write transaction that takes
    /// the write lock at `BEGIN` (`BEGIN IMMEDIATE`). A deferred transaction that reads
    /// first and then writes cannot wait for another connection's write: SQLite returns
    /// "database is locked" at once instead of calling the busy handler (to avoid a
    /// deadlock), and the records would be lost. `BEGIN IMMEDIATE` waits up to the busy
    /// timeout for other processes. Keep the turn until the transaction has ended.
    fn write_tx(&mut self) -> Result<(Turn, rusqlite::Transaction<'_>)> {
        let turn = self.queue.wait_turn();
        Ok((turn, self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?))
    }

    /// Finds the book by md5 (or creates it) and refreshes its title, authors, page
    /// count and `last_open`. Returns its row id. One write transaction, so two
    /// connections cannot add the same book twice.
    pub fn upsert_book(&mut self, b: &StatsBook, now: u64) -> Result<i64> {
        let (_turn, tx) = self.write_tx()?;
        let id: Option<i64> = tx.query_row("SELECT id FROM book WHERE md5 = ?1 ORDER BY id LIMIT 1", [&b.md5], |r| r.get(0)).optional()?;
        let pages = b.pages.map(i64::from);
        let id = match id {
            Some(id) => {
                tx.execute(
                    "UPDATE book SET title = ?2, authors = ?3, pages = coalesce(?4, pages), series = coalesce(?5, series),
                         language = coalesce(?6, language), last_open = ?7 WHERE id = ?1",
                    params![id, b.title, b.authors, pages, b.series, b.language, now as i64],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO book (title, authors, notes, last_open, highlights, pages, series, language, md5, total_read_time, total_read_pages)
                     VALUES (?1, ?2, 0, ?3, 0, ?4, ?5, ?6, ?7, 0, 0)",
                    params![b.title, b.authors, now as i64, pages, b.series, b.language, b.md5],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.commit()?;
        Ok(id)
    }

    /// Starts a session for book row `book_id` at `start`.
    pub fn start_session(&mut self, book_id: i64, start: u64) -> Result<i64> {
        let _turn = self.queue.wait_turn();
        self.conn.execute(
            "INSERT INTO session (id_book, start_time, end_time, duration, pages, source) VALUES (?1, ?2, ?2, 0, 0, 'rinky')",
            params![book_id, start as i64],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Records one page of session `session` (see [`StatsDb::record_pages`]).
    pub fn record_page(&mut self, session: i64, rec: PageRecord) -> Result<()> {
        self.record_pages(session, &[rec])
    }

    /// Records pages of session `session` in one transaction. A record with the same
    /// book, page and start time replaces the old one. The session's end, seconds and
    /// page count are updated, so a crash loses at most the unwritten records.
    pub fn record_pages(&mut self, session: i64, recs: &[PageRecord]) -> Result<()> {
        let (_turn, tx) = self.write_tx()?;
        let (book, start, mut end): (i64, i64, i64) = tx
            .query_row("SELECT id_book, start_time, end_time FROM session WHERE id = ?1", [session], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("session {session}")))?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO page_stat_data (id_book, page, start_time, duration, total_pages) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id_book, page, start_time) DO UPDATE SET duration = excluded.duration, total_pages = excluded.total_pages",
            )?;
            for r in recs {
                ins.execute(params![book, r.page, r.start_time as i64, r.duration, r.total_pages])?;
                end = end.max(r.start_time as i64 + r.duration as i64);
            }
        }
        let (dur, pages): (i64, i64) = tx.query_row(
            "SELECT coalesce(sum(duration), 0), count(DISTINCT page) FROM page_stat_data WHERE id_book = ?1 AND start_time >= ?2 AND start_time <= ?3",
            params![book, start, end],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute("UPDATE session SET end_time = ?2, duration = ?3, pages = ?4 WHERE id = ?1", params![session, end, dur, pages])?;
        tx.commit()?;
        Ok(())
    }

    /// Ends session `session` at `end` (or at its last page if later), updates the
    /// book's totals and returns the session. A session without pages is deleted
    /// (returned with `duration == 0`).
    pub fn end_session(&mut self, session: i64, end: u64) -> Result<Session> {
        let (_turn, tx) = self.write_tx()?;
        let (book, cur_end, pages): (i64, i64, i64) = tx
            .query_row("SELECT id_book, end_time, pages FROM session WHERE id = ?1", [session], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("session {session}")))?;
        tx.execute("UPDATE session SET end_time = ?2 WHERE id = ?1", params![session, cur_end.max(end as i64)])?;
        let s = session_by_id(&tx, session)?;
        if pages == 0 {
            tx.execute("DELETE FROM session WHERE id = ?1", [session])?;
        }
        update_book_totals(&tx, book)?;
        tx.commit()?;
        Ok(s)
    }

    /// Totals for the book with `md5`, if known.
    pub fn book_totals(&self, md5: &str) -> Result<Option<BookTotals>> {
        Ok(self.totals(Some(md5))?.into_iter().next())
    }

    /// Totals for every book, most recently read first.
    pub fn all_books(&self) -> Result<Vec<BookTotals>> {
        self.totals(None)
    }

    fn totals(&self, md5: Option<&str>) -> Result<Vec<BookTotals>> {
        let mut st = self.conn.prepare_cached(
            "SELECT b.md5, b.title, b.authors, b.pages,
                    (SELECT coalesce(sum(duration), 0) FROM page_stat_data p WHERE p.id_book = b.id),
                    (SELECT count(DISTINCT page) FROM page_stat_data p WHERE p.id_book = b.id),
                    (SELECT count(*) FROM session s WHERE s.id_book = b.id),
                    (SELECT min(start_time) FROM page_stat_data p WHERE p.id_book = b.id),
                    (SELECT max(start_time + duration) FROM page_stat_data p WHERE p.id_book = b.id)
             FROM book b WHERE (?1 IS NULL OR b.md5 = ?1)
             ORDER BY 9 DESC NULLS LAST, b.id",
        )?;
        let rows = st.query_map([md5], |r| {
            Ok(BookTotals {
                md5: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                authors: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                pages: r.get::<_, Option<i64>>(3)?.map(u32_of),
                read_time: u(r.get(4)?),
                pages_read: u(r.get(5)?),
                sessions: u(r.get(6)?),
                first_read: r.get::<_, Option<i64>>(7)?.map(u),
                last_read: r.get::<_, Option<i64>>(8)?.map(u),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Sessions, newest first; of one book when `md5` is given.
    pub fn sessions(&self, md5: Option<&str>, limit: usize) -> Result<Vec<Session>> {
        let mut st = self.conn.prepare_cached(
            "SELECT s.id, b.md5, s.start_time, s.end_time, s.duration, s.pages, s.source FROM session s JOIN book b ON b.id = s.id_book
             WHERE (?1 IS NULL OR b.md5 = ?1) ORDER BY s.start_time DESC, s.id DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![md5, limit as i64], row_session)?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Page records of the book with `md5`, oldest first.
    pub fn page_stats(&self, md5: &str) -> Result<Vec<PageRecord>> {
        let mut st = self.conn.prepare_cached(
            "SELECT p.page, p.total_pages, p.start_time, p.duration FROM page_stat_data p JOIN book b ON b.id = p.id_book
             WHERE b.md5 = ?1 ORDER BY p.start_time, p.page",
        )?;
        let rows = st.query_map([md5], |r| {
            Ok(PageRecord {
                page: u32_of(r.get(0)?),
                total_pages: u32_of(r.get(1)?),
                start_time: u(r.get(2)?),
                duration: u32_of(r.get(3)?),
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Reading per day for page records starting in `from..to`, with days counted in a
    /// time zone `utc_offset` seconds east of UTC.
    pub fn daily(&self, from: u64, to: u64, utc_offset: i64) -> Result<Vec<DayTotal>> {
        let mut st = self.conn.prepare_cached(
            "SELECT date(start_time + ?3, 'unixepoch') AS day, sum(duration), count(*), count(DISTINCT id_book)
             FROM page_stat_data WHERE start_time >= ?1 AND start_time < ?2 GROUP BY day ORDER BY day",
        )?;
        let rows = st.query_map(params![from as i64, to as i64, utc_offset], |r| {
            Ok(DayTotal { day: r.get(0)?, seconds: u(r.get(1)?), pages: u(r.get(2)?), books: u(r.get(3)?) })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Copies KOReader's statistics in: books are matched by md5 (created if missing),
    /// page records already present are skipped, and the KOReader sessions of each book
    /// are rebuilt from records not covered by an rinky session (a gap of more than
    /// [`SESSION_GAP_SECS`] starts a new one). Running it twice adds nothing.
    pub fn import_koreader(&mut self, ko: &KoStatistics) -> Result<ImportCounts> {
        let mut counts = ImportCounts::default();
        let (_turn, tx) = self.write_tx()?;
        let mut ids: HashMap<i64, i64> = HashMap::new();
        for b in &ko.books {
            let found: Option<i64> = match &b.md5 {
                Some(md5) => tx.query_row("SELECT id FROM book WHERE md5 = ?1 ORDER BY id LIMIT 1", [md5], |r| r.get(0)).optional()?,
                None => tx
                    .query_row("SELECT id FROM book WHERE md5 IS NULL AND title = ?1 AND authors = ?2 LIMIT 1", [&b.title, &b.authors], |r| r.get(0))
                    .optional()?,
            };
            let id = match found {
                Some(id) => {
                    tx.execute(
                        "UPDATE book SET pages = coalesce(pages, ?2), series = coalesce(series, ?3), language = coalesce(language, ?4),
                             last_open = max(coalesce(last_open, 0), coalesce(?5, 0)) WHERE id = ?1",
                        params![id, b.pages, b.series, b.language, b.last_open],
                    )?;
                    id
                }
                None => {
                    tx.execute(
                        "INSERT INTO book (title, authors, notes, last_open, highlights, pages, series, language, md5, total_read_time, total_read_pages)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, 0)",
                        params![b.title, b.authors, b.notes, b.last_open, b.highlights, b.pages, b.series, b.language, b.md5],
                    )?;
                    tx.last_insert_rowid()
                }
            };
            ids.insert(b.id, id);
            counts.books += 1;
        }
        {
            let mut ins = tx.prepare_cached("INSERT OR IGNORE INTO page_stat_data (id_book, page, start_time, duration, total_pages) VALUES (?1, ?2, ?3, ?4, ?5)")?;
            for p in &ko.pages {
                let Some(&book) = ids.get(&p.book_id) else { continue };
                counts.pages += ins.execute(params![book, p.page, p.start_time as i64, p.duration, p.total_pages])?;
            }
        }
        let mut books: Vec<i64> = ids.values().copied().collect();
        books.sort_unstable();
        books.dedup();
        for book in books {
            counts.sessions += rebuild_koreader_sessions(&tx, book)?;
            update_book_totals(&tx, book)?;
        }
        tx.commit()?;
        Ok(counts)
    }
}

fn row_session(r: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: r.get(0)?,
        md5: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
        start_time: u(r.get(2)?),
        end_time: u(r.get(3)?),
        duration: u(r.get(4)?),
        pages: u(r.get(5)?),
        source: r.get(6)?,
    })
}

fn session_by_id(conn: &Connection, id: i64) -> Result<Session> {
    Ok(conn.query_row(
        "SELECT s.id, b.md5, s.start_time, s.end_time, s.duration, s.pages, s.source FROM session s JOIN book b ON b.id = s.id_book WHERE s.id = ?1",
        [id],
        row_session,
    )?)
}

fn update_book_totals(conn: &Connection, book: i64) -> Result<()> {
    conn.execute(
        "UPDATE book SET
             total_read_time = (SELECT coalesce(sum(duration), 0) FROM page_stat_data WHERE id_book = ?1),
             total_read_pages = (SELECT count(DISTINCT page) FROM page_stat_data WHERE id_book = ?1)
         WHERE id = ?1",
        [book],
    )?;
    Ok(())
}

/// `spans` (inclusive `(start, end)`, any order, may overlap) sorted by start with the
/// overlapping ones joined, so [`covers`] can binary search them.
fn merge_spans(mut spans: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    spans.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::with_capacity(spans.len());
    for (a, b) in spans {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// True when `t` lies in one of `merged` (from [`merge_spans`]); O(log n).
fn covers(merged: &[(i64, i64)], t: i64) -> bool {
    let i = merged.partition_point(|&(a, _)| a <= t);
    i > 0 && t <= merged[i - 1].1
}

/// Replaces the `koreader` sessions of `book` with sessions built from its page records
/// that no rinky session covers. Returns the number of sessions.
fn rebuild_koreader_sessions(conn: &Connection, book: i64) -> Result<usize> {
    conn.execute("DELETE FROM session WHERE id_book = ?1 AND source = 'koreader'", [book])?;
    // Sorted and joined once, so each page record is checked in O(log sessions) rather
    // than against every session (an imported history has thousands of each).
    let ours: Vec<(i64, i64)> = conn
        .prepare("SELECT start_time, end_time FROM session WHERE id_book = ?1")?
        .query_map([book], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let ours = merge_spans(ours);
    let rows: Vec<(i64, i64, i64)> = conn
        .prepare("SELECT start_time, duration, page FROM page_stat_data WHERE id_book = ?1 ORDER BY start_time, page")?
        .query_map([book], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    let mut sessions: Vec<(i64, i64, i64, std::collections::HashSet<i64>)> = Vec::new();
    for (start, dur, page) in rows {
        if covers(&ours, start) {
            continue;
        }
        match sessions.last_mut() {
            Some(s) if start <= s.1 + SESSION_GAP_SECS as i64 => {
                s.1 = s.1.max(start + dur);
                s.2 += dur;
                s.3.insert(page);
            }
            _ => sessions.push((start, start + dur, dur, std::collections::HashSet::from([page]))),
        }
    }
    let mut ins = conn.prepare("INSERT INTO session (id_book, start_time, end_time, duration, pages, source) VALUES (?1, ?2, ?3, ?4, ?5, 'koreader')")?;
    for (start, end, dur, pages) in &sessions {
        ins.execute(params![book, start, end, dur, pages.len() as i64])?;
    }
    Ok(sessions.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::koreader::sqlite::{read_statistics_db, write_sample_databases};
    use crate::testutil::TempDir;

    fn juliet() -> StatsBook {
        StatsBook { md5: "cf7ce9b8d4e135c8ac340f18d1e58c85".into(), title: "Romeo and Juliet".into(), authors: "William Shakespeare".into(), pages: Some(180), ..Default::default() }
    }

    #[test]
    fn covered_times_match_a_linear_scan() {
        // Unsorted, overlapping, touching, nested and single-instant spans.
        let spans = vec![(50, 60), (10, 20), (15, 30), (31, 31), (100, 200), (120, 130), (30, 30), (70, 70), (-5, 0)];
        let merged = merge_spans(spans.clone());
        assert!(merged.windows(2).all(|w| w[0].1 < w[1].0), "{merged:?}");
        for t in -10..=210 {
            let linear = spans.iter().any(|&(a, b)| t >= a && t <= b);
            assert_eq!(covers(&merged, t), linear, "t = {t}");
        }
        assert!(!covers(&merge_spans(Vec::new()), 0));
    }

    #[test]
    fn sessions_pages_and_totals() {
        let tmp = TempDir::new("stats");
        let path = tmp.path().join("rinky/statistics.sqlite3");
        let mut db = StatsDb::open(&path).unwrap();
        let book = db.upsert_book(&juliet(), 1_000).unwrap();
        assert_eq!(db.upsert_book(&juliet(), 1_001).unwrap(), book, "same md5, same row");
        let s = db.start_session(book, 1_000).unwrap();
        db.record_page(s, PageRecord { page: 1, total_pages: 180, start_time: 1_000, duration: 30 }).unwrap();
        db.record_pages(
            s,
            &[
                PageRecord { page: 2, total_pages: 180, start_time: 1_030, duration: 45 },
                PageRecord { page: 3, total_pages: 180, start_time: 1_075, duration: 20 },
                // Same page and start again: replaces, not duplicates.
                PageRecord { page: 3, total_pages: 180, start_time: 1_075, duration: 25 },
            ],
        )
        .unwrap();
        let done = db.end_session(s, 1_200).unwrap();
        assert_eq!((done.start_time, done.end_time, done.duration, done.pages, done.source.as_str()), (1_000, 1_200, 100, 3, "rinky"));
        // A second session the next day, re-reading page 3.
        let s2 = db.start_session(book, 90_000).unwrap();
        db.record_page(s2, PageRecord { page: 3, total_pages: 180, start_time: 90_000, duration: 10 }).unwrap();
        db.record_page(s2, PageRecord { page: 4, total_pages: 180, start_time: 90_010, duration: 50 }).unwrap();
        db.end_session(s2, 90_000).unwrap();
        // An empty session disappears.
        let s3 = db.start_session(book, 95_000).unwrap();
        assert_eq!(db.end_session(s3, 95_100).unwrap().duration, 0);
        let t = db.book_totals(&juliet().md5).unwrap().unwrap();
        assert_eq!((t.read_time, t.pages_read, t.sessions, t.first_read, t.last_read, t.pages), (160, 4, 2, Some(1_000), Some(90_060), Some(180)));
        let sessions = db.sessions(Some(&juliet().md5), 10).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!((sessions[0].start_time, sessions[0].end_time, sessions[0].duration, sessions[0].pages), (90_000, 90_060, 60, 2));
        assert_eq!(db.page_stats(&juliet().md5).unwrap().len(), 5);
        let days = db.daily(0, 200_000, 0).unwrap();
        assert_eq!(days, [
            DayTotal { day: "1970-01-01".into(), seconds: 100, pages: 3, books: 1 },
            DayTotal { day: "1970-01-02".into(), seconds: 60, pages: 2, books: 1 },
        ]);
        // A time zone shifts the day boundary.
        assert_eq!(db.daily(0, 200_000, -3_600).unwrap()[0].day, "1969-12-31");
        // KOReader-compatible totals in the book table.
        let (trt, trp): (i64, i64) = db.connection().query_row("SELECT total_read_time, total_read_pages FROM book WHERE id = ?1", [book], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((trt, trp), (160, 4));
        assert!(db.record_page(9999, PageRecord::default()).is_err());
        drop(db);
        // Reopen: data persists, schema version set.
        let db = StatsDb::open(&path).unwrap();
        assert_eq!(db.all_books().unwrap().len(), 1);
        let v: i64 = db.connection().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let mode: String = db.connection().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, "truncate");
    }

    #[test]
    fn koreader_import_is_idempotent() {
        let tmp = TempDir::new("stats-import");
        let (kodb, _) = write_sample_databases(&tmp.path().join("ko"), false, true).unwrap();
        let ko = read_statistics_db(&kodb, None).unwrap();
        let mut db = StatsDb::open_in_memory().unwrap();
        // rinky already read Juliet in a session that overlaps nothing of KOReader's.
        let book = db.upsert_book(&juliet(), 1_760_200_000).unwrap();
        let s = db.start_session(book, 1_760_200_000).unwrap();
        db.record_page(s, PageRecord { page: 50, total_pages: 180, start_time: 1_760_200_000, duration: 40 }).unwrap();
        db.end_session(s, 1_760_200_100).unwrap();
        let c = db.import_koreader(&ko).unwrap();
        assert_eq!(c, ImportCounts { books: 2, pages: 5, sessions: 2 });
        assert_eq!(db.import_koreader(&ko).unwrap(), ImportCounts { books: 2, pages: 0, sessions: 2 }, "nothing new the second time");
        let j = db.book_totals(&juliet().md5).unwrap().unwrap();
        assert_eq!((j.read_time, j.pages_read, j.sessions), (95 + 40, 4, 2));
        let all = db.all_books().unwrap();
        assert_eq!(all.len(), 2, "matched by md5, not duplicated");
        let leaves = db.book_totals("59d481d168cca6267322f150c5f6a2a3").unwrap().unwrap();
        assert_eq!((leaves.title.as_str(), leaves.read_time, leaves.sessions), ("Leaves of Grass", 40, 1));
        let ks = db.sessions(Some(&juliet().md5), 10).unwrap();
        assert_eq!(ks.iter().map(|s| s.source.as_str()).collect::<Vec<_>>(), ["rinky", "koreader"]);
        assert_eq!((ks[1].start_time, ks[1].end_time, ks[1].duration, ks[1].pages), (1_760_000_000, 1_760_000_095, 95, 3));
    }

    #[test]
    fn newer_schema_is_refused() {
        let tmp = TempDir::new("stats-new");
        let p = tmp.path().join("s.sqlite3");
        let c = Connection::open(&p).unwrap();
        c.execute_batch("PRAGMA user_version=99").unwrap();
        drop(c);
        assert!(matches!(StatsDb::open(&p), Err(StoreError::Format(_))));
        // A newer schema is not damage: open_or_recover refuses it too and moves nothing.
        assert!(matches!(StatsDb::open_or_recover(&p), Err(StoreError::Format(_))));
        assert!(p.exists());
    }

    #[test]
    fn damaged_database_is_moved_aside() {
        let tmp = TempDir::new("stats-damaged");
        let p = tmp.path().join("statistics.sqlite3");
        std::fs::write(&p, b"this is not a database, but long enough to have a header....................................................................").unwrap();
        assert!(StatsDb::open(&p).is_err());
        let (mut db, moved) = StatsDb::open_or_recover(&p).unwrap();
        let moved = moved.unwrap();
        assert!(moved.to_string_lossy().contains(".corrupt-"));
        assert!(std::fs::read(&moved).unwrap().starts_with(b"this is not a database"));
        db.upsert_book(&juliet(), 1).unwrap();
        drop(db);
        // A good database opens as it is.
        let (_, moved) = StatsDb::open_or_recover(&p).unwrap();
        assert_eq!(moved, None);
    }
}
