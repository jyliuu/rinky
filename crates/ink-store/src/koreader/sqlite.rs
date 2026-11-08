//! Read-only readers for KOReader's `statistics.sqlite3` and `vocabulary_builder.sqlite3`.
//!
//! KOReader keeps these databases in WAL mode on some devices (the header bytes 18/19
//! are 2). Opening such a file in place, even read-only, can create `-wal`/`-shm` files
//! next to it, and `immutable=1` would ignore data still in the WAL. So the database
//! file and its `-wal` / `-journal` files are copied into a private scratch directory,
//! the copy is opened (SQLite replays the WAL into the copy) and removed afterwards.
//! KOReader's files are only ever opened for reading by `std::fs::copy`.
//!
//! Columns are found by inspecting the schema, so every schema version KOReader has
//! used loads: statistics with `page_stat_data` (2020-10 and later) or the older
//! `page_stat` table (`period` column), vocabulary with `title_id` + `title` (2022-06
//! and later) or the older `book_title` column.

use crate::{Result, StoreError};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Row};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// One book of KOReader's statistics (`book` table).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KoStatsBook {
    /// Row id (`page_stat_data.id_book` refers to it).
    pub id: i64,
    /// Title.
    pub title: String,
    /// Authors (KOReader separates several with newlines).
    pub authors: String,
    /// Partial MD5 of the book file.
    pub md5: Option<String>,
    /// Page count at the last open.
    pub pages: Option<i64>,
    /// Last open, seconds since the epoch.
    pub last_open: Option<i64>,
    /// Notes count.
    pub notes: Option<i64>,
    /// Highlights count.
    pub highlights: Option<i64>,
    /// Series.
    pub series: Option<String>,
    /// Language.
    pub language: Option<String>,
    /// Total reading time in seconds.
    pub total_read_time: Option<i64>,
    /// Pages read.
    pub total_read_pages: Option<i64>,
}

/// One row of KOReader's reading statistics (`page_stat_data`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KoPageStat {
    /// Book md5 (empty if the book row has none).
    pub md5: String,
    /// Book row id.
    pub book_id: i64,
    /// Page number (1-based, in the book's page count `total_pages` at that time).
    pub page: u32,
    /// Start time, seconds since the epoch.
    pub start_time: u64,
    /// Seconds on the page.
    pub duration: u32,
    /// Pages in the book at that time.
    pub total_pages: u32,
}

/// KOReader's whole statistics database.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KoStatistics {
    /// `PRAGMA user_version` (20221111 for current KOReader).
    pub schema_version: i64,
    /// Books.
    pub books: Vec<KoStatsBook>,
    /// Page reading records, oldest first.
    pub pages: Vec<KoPageStat>,
}

impl KoStatistics {
    /// The book with partial MD5 `md5`.
    pub fn book_by_md5(&self, md5: &str) -> Option<&KoStatsBook> {
        self.books.iter().find(|b| b.md5.as_deref() == Some(md5))
    }

    /// Total seconds read, over all books.
    pub fn total_seconds(&self) -> u64 {
        self.pages.iter().map(|p| p.duration as u64).sum()
    }
}

/// One word of KOReader's vocabulary builder.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KoVocabWord {
    /// The word.
    pub word: String,
    /// Title of the book it was looked up in.
    pub title: Option<String>,
    /// When it was added, seconds since the epoch.
    pub create_time: i64,
    /// Last review, seconds since the epoch.
    pub review_time: Option<i64>,
    /// Next review due, seconds since the epoch.
    pub due_time: i64,
    /// Number of reviews.
    pub review_count: i64,
    /// Correct answers in a row.
    pub streak_count: i64,
    /// Text before the word.
    pub prev_context: Option<String>,
    /// Text after the word.
    pub next_context: Option<String>,
    /// The highlighted sentence, if the lookup came from a highlight.
    pub highlight: Option<String>,
}

static SNAP: AtomicU64 = AtomicU64::new(0);

/// A private copy of a database, deleted on drop.
struct Snapshot {
    dir: PathBuf,
    db: PathBuf,
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Copies `db` (and its `-wal` / `-journal`) into a new directory under `scratch`
/// (default: the system temp dir).
fn snapshot(db: &Path, scratch: Option<&Path>) -> Result<Snapshot> {
    if !db.is_file() {
        return Err(StoreError::NotFound(db.display().to_string()));
    }
    let base = scratch.map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
    let n = SNAP.fetch_add(1, Ordering::Relaxed);
    let dir = base.join(format!("koreader-db-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let snap = Snapshot { db: dir.join("copy.sqlite3"), dir };
    std::fs::copy(db, &snap.db)?;
    for suffix in ["-wal", "-journal"] {
        let src = crate::with_suffix(db, suffix);
        if src.is_file() {
            std::fs::copy(&src, crate::with_suffix(&snap.db, suffix))?;
        }
    }
    Ok(snap)
}

fn open_copy(snap: &Snapshot) -> Result<Connection> {
    // Read-write on the copy, so SQLite can replay a WAL or roll back a hot journal.
    let conn = Connection::open_with_flags(&snap.db, OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    // Fails early with "file is not a database" for garbage.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
    Ok(conn)
}

fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", [name], |r| r.get::<_, i64>(0))? > 0)
}

fn columns(conn: &Connection, table: &str) -> Result<HashSet<String>> {
    // `table` is one of this module's constants, never input.
    crate::query_all(conn, &format!("PRAGMA table_info(\"{table}\")"), [], |r| r.get::<_, String>(1))
}

/// `t.col`, or `NULL` when the column does not exist.
fn col(cols: &HashSet<String>, t: &str, c: &str) -> String {
    if cols.contains(c) {
        format!("{t}.\"{c}\"")
    } else {
        "NULL".into()
    }
}

/// [`col`] of each of `names`, separated by commas.
fn col_list(cols: &HashSet<String>, t: &str, names: &[&str]) -> String {
    names.iter().map(|c| col(cols, t, c)).collect::<Vec<_>>().join(", ")
}

/// Integer from any SQLite type (KOReader's columns are not strictly typed).
fn opt_i64(r: &Row, i: usize) -> rusqlite::Result<Option<i64>> {
    Ok(match r.get_ref(i)? {
        ValueRef::Null => None,
        ValueRef::Integer(v) => Some(v),
        ValueRef::Real(v) if v.is_finite() => Some(v as i64),
        ValueRef::Real(_) => None,
        ValueRef::Text(t) => std::str::from_utf8(t).ok().and_then(|s| s.trim().parse::<f64>().ok()).map(|v| v as i64),
        ValueRef::Blob(_) => None,
    })
}

/// Text from any SQLite type (lossy UTF-8).
fn opt_text(r: &Row, i: usize) -> rusqlite::Result<Option<String>> {
    Ok(match r.get_ref(i)? {
        ValueRef::Null => None,
        ValueRef::Integer(v) => Some(v.to_string()),
        ValueRef::Real(v) => Some(v.to_string()),
        ValueRef::Text(t) | ValueRef::Blob(t) => Some(String::from_utf8_lossy(t).into_owned()),
    })
}

fn clamp_u32(v: Option<i64>) -> u32 {
    v.unwrap_or(0).clamp(0, u32::MAX as i64) as u32
}

/// Reads `statistics.sqlite3` (books and page records) from a private copy made under
/// `scratch` (default: the system temp dir; on the Kindle pass `Paths::tmp()`, since
/// `/tmp` is a small RAM disk).
pub fn read_statistics_db(db: &Path, scratch: Option<&Path>) -> Result<KoStatistics> {
    let snap = snapshot(db, scratch)?;
    let conn = open_copy(&snap)?;
    let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let mut out = KoStatistics { schema_version, ..Default::default() };
    if !has_table(&conn, "book")? {
        return Err(StoreError::Format(format!("{}: no `book` table, not a KOReader statistics database", db.display())));
    }
    let bc = columns(&conn, "book")?;
    let fields = ["title", "authors", "md5", "pages", "last_open", "notes", "highlights", "series", "language", "total_read_time", "total_read_pages"];
    let sql = format!("SELECT b.id, {} FROM book b ORDER BY b.id", col_list(&bc, "b", &fields));
    out.books = crate::query_all(&conn, &sql, [], |r| {
        Ok(KoStatsBook {
            id: r.get(0)?,
            title: opt_text(r, 1)?.unwrap_or_default(),
            authors: opt_text(r, 2)?.unwrap_or_default(),
            md5: opt_text(r, 3)?.filter(|s| !s.is_empty()),
            pages: opt_i64(r, 4)?,
            last_open: opt_i64(r, 5)?,
            notes: opt_i64(r, 6)?,
            highlights: opt_i64(r, 7)?,
            series: opt_text(r, 8)?.filter(|s| !s.is_empty() && s != "N/A"),
            language: opt_text(r, 9)?.filter(|s| !s.is_empty() && s != "N/A"),
            total_read_time: opt_i64(r, 10)?,
            total_read_pages: opt_i64(r, 11)?,
        })
    })?;

    // Before 2020-10: `page_stat(id_book, page, start_time, period)`, no page count.
    let table = if has_table(&conn, "page_stat_data")? {
        "page_stat_data"
    } else if has_table(&conn, "page_stat")? {
        "page_stat"
    } else {
        return Ok(out);
    };
    let pc = columns(&conn, table)?;
    let dur = if pc.contains("duration") { "p.duration" } else { "p.period" };
    let total = if pc.contains("total_pages") { "p.total_pages" } else { "b.pages" };
    let sql = format!(
        "SELECT p.id_book, b.md5, p.page, p.start_time, {dur}, {total} FROM {table} p \
         LEFT JOIN book b ON b.id = p.id_book ORDER BY p.start_time, p.id_book, p.page"
    );
    out.pages = crate::query_all(&conn, &sql, [], |r| {
        Ok(KoPageStat {
            book_id: opt_i64(r, 0)?.unwrap_or(0),
            md5: opt_text(r, 1)?.unwrap_or_default(),
            page: clamp_u32(opt_i64(r, 2)?),
            start_time: opt_i64(r, 3)?.unwrap_or(0).max(0) as u64,
            duration: clamp_u32(opt_i64(r, 4)?),
            total_pages: clamp_u32(opt_i64(r, 5)?),
        })
    })?;
    Ok(out)
}

/// Reads the page records of `statistics.sqlite3` (see [`read_statistics_db`]).
pub fn read_statistics(db: &Path) -> Result<Vec<KoPageStat>> {
    Ok(read_statistics_db(db, None)?.pages)
}

/// Reads `vocabulary_builder.sqlite3` from a private copy (see [`read_statistics_db`]
/// for `scratch`). Words are ordered by creation time.
pub fn read_vocabulary(db: &Path, scratch: Option<&Path>) -> Result<Vec<KoVocabWord>> {
    let snap = snapshot(db, scratch)?;
    let conn = open_copy(&snap)?;
    if !has_table(&conn, "vocabulary")? {
        return Err(StoreError::Format(format!("{}: no `vocabulary` table, not a KOReader vocabulary database", db.display())));
    }
    let vc = columns(&conn, "vocabulary")?;
    let title = if vc.contains("title_id") && has_table(&conn, "title")? {
        "(SELECT t.name FROM title t WHERE t.id = v.title_id)".to_string()
    } else {
        col(&vc, "v", "book_title")
    };
    let fields = ["create_time", "review_time", "due_time", "review_count", "streak_count", "prev_context", "next_context", "highlight"];
    let sql = format!("SELECT v.word, {title}, {} FROM vocabulary v ORDER BY {}, v.word", col_list(&vc, "v", &fields), col(&vc, "v", "create_time"));
    crate::query_all(&conn, &sql, [], |r| {
        Ok(KoVocabWord {
            word: opt_text(r, 0)?.unwrap_or_default(),
            title: opt_text(r, 1)?,
            create_time: opt_i64(r, 2)?.unwrap_or(0),
            review_time: opt_i64(r, 3)?,
            due_time: opt_i64(r, 4)?.unwrap_or(0),
            review_count: opt_i64(r, 5)?.unwrap_or(0),
            streak_count: opt_i64(r, 6)?.unwrap_or(0),
            prev_context: opt_text(r, 7)?,
            next_context: opt_text(r, 8)?,
            highlight: opt_text(r, 9)?,
        })
    })
}

/// Schema statements copied from KOReader (`plugins/statistics.koplugin/main.lua`,
/// `plugins/vocabbuilder.koplugin/db.lua`), for test fixtures and the selftest.
pub mod schema {
    /// KOReader's `book` table, the same in every statistics schema (and in ours,
    /// `crate::stats`, so its rows copy over as they are).
    macro_rules! book_table {
        () => {
            r#"
        CREATE TABLE IF NOT EXISTS book
            (
                id integer PRIMARY KEY autoincrement,
                title text,
                authors text,
                notes      integer,
                last_open  integer,
                highlights integer,
                pages      integer,
                series text,
                language text,
                md5 text,
                total_read_time  integer,
                total_read_pages integer
            );
"#
        };
    }
    pub(crate) use book_table;

    /// Statistics, schema 20221111.
    pub const STATISTICS: &str = concat!(
        book_table!(),
        r#"
        CREATE UNIQUE INDEX IF NOT EXISTS book_title_authors_md5 ON book(title, authors, md5);
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
        CREATE TABLE IF NOT EXISTS numbers
            (
                number INTEGER PRIMARY KEY
            );
        WITH RECURSIVE counter AS
            (
                SELECT 1 as N UNION ALL
                SELECT N + 1 FROM counter WHERE N < 1000
            )
            INSERT INTO numbers SELECT N AS number FROM counter;
        CREATE VIEW IF NOT EXISTS page_stat AS
            SELECT id_book, first_page + idx - 1 AS page, start_time, duration / (last_page - first_page + 1) AS duration
            FROM (
                SELECT id_book, page, total_pages, pages, start_time, duration,
                    ((page - 1) * pages) / total_pages + 1 AS first_page,
                    max(((page - 1) * pages) / total_pages + 1, (page * pages) / total_pages) AS last_page,
                    idx
                FROM page_stat_data
                JOIN book ON book.id = id_book
                JOIN (SELECT number as idx FROM numbers) AS N ON idx <= (last_page - first_page + 1)
            );
        PRAGMA user_version=20221111;
    "#
    );

    /// Statistics before 2020-10 (`page_stat` table with `period`).
    pub const STATISTICS_2019: &str = concat!(
        book_table!(),
        r#"
        CREATE TABLE IF NOT EXISTS page_stat
            (
                id_book integer,
                page integer NOT NULL,
                start_time integer NOT NULL,
                period integer NOT NULL,
                UNIQUE (page, start_time),
                FOREIGN KEY(id_book) REFERENCES book(id)
            );
        CREATE TABLE IF NOT EXISTS info
            (
                version integer
            );
    "#
    );

    /// Vocabulary builder, schema 20240905.
    pub const VOCABULARY: &str = r#"
        CREATE TABLE IF NOT EXISTS vocabulary (
            word          TEXT NOT NULL UNIQUE,
            title_id      INTEGER,
            create_time   INTEGER NOT NULL,
            review_time   INTEGER,
            due_time      INTEGER NOT NULL,
            review_count  INTEGER NOT NULL DEFAULT 0,
            prev_context  TEXT,
            next_context  TEXT,
            streak_count  INTEGER NOT NULL DEFAULT 0,
            highlight     TEXT,
            PRIMARY KEY(word)
        );
        CREATE TABLE IF NOT EXISTS title (
            id            INTEGER NOT NULL UNIQUE,
            name          TEXT UNIQUE,
            filter        INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY(id)
        );
        CREATE INDEX IF NOT EXISTS due_time_index ON vocabulary(due_time);
        CREATE INDEX IF NOT EXISTS title_name_index ON title(name);
        PRAGMA user_version=20240905;
    "#;

    /// Vocabulary builder before 2022-06 (`book_title` column).
    pub const VOCABULARY_2022: &str = r#"
        CREATE TABLE IF NOT EXISTS vocabulary (
            word        TEXT NOT NULL UNIQUE,
            book_title  TEXT DEFAULT '',
            create_time INTEGER NOT NULL,
            review_time INTEGER,
            due_time    INTEGER NOT NULL,
            review_count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(word)
        );
    "#;
}

/// Writes small KOReader-format databases (current and old schemas) for tests and the
/// selftest. Returns (statistics, vocabulary).
pub fn write_sample_databases(dir: &Path, old_schema: bool, wal: bool) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(dir)?;
    let stats = dir.join("statistics.sqlite3");
    let vocab = dir.join("vocabulary_builder.sqlite3");
    for p in [&stats, &vocab] {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(crate::with_suffix(p, suffix));
        }
    }
    // In WAL mode keep every row in the `-wal` file (no automatic checkpoint, none on
    // close), as KOReader's files on a device can be.
    let open = |p: &Path| -> Result<Connection> {
        let c = Connection::open(p)?;
        if wal {
            c.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
            c.query_row("PRAGMA wal_autocheckpoint=0", [], |_| Ok(()))?;
            c.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)?;
        }
        Ok(c)
    };
    let c = open(&stats)?;
    if old_schema {
        c.execute_batch(schema::STATISTICS_2019)?;
        c.execute_batch(
            "INSERT INTO book (id, title, authors, pages, md5, last_open, total_read_time, total_read_pages)
                 VALUES (1, 'Old Book', 'Old Author', 100, 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1500000000, 90, 3);
             INSERT INTO page_stat VALUES (1, 1, 1500000000, 30), (1, 2, 1500000030, 30), (1, 3, 1500000060, 30);",
        )?;
    } else {
        c.execute_batch(schema::STATISTICS)?;
        c.execute_batch(
            "INSERT INTO book (id, title, authors, notes, last_open, highlights, pages, series, language, md5, total_read_time, total_read_pages)
                 VALUES (1, 'Romeo and Juliet', 'William Shakespeare', 1, 1760000600, 2, 180, 'N/A', 'en', 'cf7ce9b8d4e135c8ac340f18d1e58c85', 95, 3),
                        (2, 'Leaves of Grass', 'Walt Whitman', 0, 1760100000, 0, 400, 'N/A', 'en', '59d481d168cca6267322f150c5f6a2a3', 40, 2);
             INSERT INTO page_stat_data VALUES
                 (1, 1, 1760000000, 30, 180), (1, 2, 1760000030, 45, 180), (1, 3, 1760000075, 20, 180),
                 (2, 10, 1760100000, 25, 400), (2, 11, 1760100025, 15, 400);",
        )?;
    }
    drop(c);
    let c = open(&vocab)?;
    if old_schema {
        c.execute_batch(schema::VOCABULARY_2022)?;
        c.execute_batch("INSERT INTO vocabulary VALUES ('vex', 'Old Book', 1500000000, NULL, 1500003600, 0);")?;
    } else {
        c.execute_batch(schema::VOCABULARY)?;
        c.execute_batch(
            "INSERT INTO title (id, name) VALUES (1, 'Romeo and Juliet');
             INSERT INTO vocabulary (word, title_id, create_time, review_time, due_time, review_count, prev_context, next_context, streak_count, highlight)
                 VALUES ('dignity', 1, 1760000100, NULL, 1760000400, 0, 'Two households, both alike in ', ', In fair Verona', 0, NULL),
                        ('mutiny', 1, 1760000200, 1760086400, 1760259200, 2, 'From ancient grudge break to new ', ', Where civil blood', 2, 'break to new mutiny');",
        )?;
    }
    drop(c);
    Ok((stats, vocab))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn statistics_current_schema() {
        let tmp = TempDir::new("ko-stats");
        let (db, _) = write_sample_databases(&tmp.path().join("ko"), false, false).unwrap();
        let before = tree(db.parent().unwrap());
        let s = read_statistics_db(&db, Some(&tmp.path().join("scratch"))).unwrap();
        assert_eq!(s.schema_version, 20221111);
        assert_eq!(s.books.len(), 2);
        let b = s.book_by_md5("cf7ce9b8d4e135c8ac340f18d1e58c85").unwrap();
        assert_eq!((b.title.as_str(), b.authors.as_str(), b.pages, b.series.as_deref()), ("Romeo and Juliet", "William Shakespeare", Some(180), None));
        assert_eq!(s.pages.len(), 5);
        assert_eq!(s.pages[0], KoPageStat { md5: "cf7ce9b8d4e135c8ac340f18d1e58c85".into(), book_id: 1, page: 1, start_time: 1760000000, duration: 30, total_pages: 180 });
        assert_eq!(s.total_seconds(), 135);
        assert_eq!(read_statistics(&db).unwrap().len(), 5);
        // Nothing changed next to KOReader's file, and the scratch copy is gone.
        assert_eq!(tree(db.parent().unwrap()), before);
        assert!(std::fs::read_dir(tmp.path().join("scratch")).unwrap().next().is_none());
    }

    #[test]
    fn wal_databases_are_read_completely_and_left_alone() {
        let tmp = TempDir::new("ko-wal");
        let (db, vocab) = write_sample_databases(&tmp.path().join("ko"), false, true).unwrap();
        assert!(crate::with_suffix(&db, "-wal").exists(), "the sample keeps its data in the WAL");
        let mut header = [0u8; 20];
        use std::io::Read;
        std::fs::File::open(&db).unwrap().read_exact(&mut header).unwrap();
        assert_eq!(&header[18..20], &[2, 2], "WAL mode header, like the real KOReader files");
        let before = tree(db.parent().unwrap());
        let s = read_statistics_db(&db, Some(tmp.path())).unwrap();
        assert_eq!((s.books.len(), s.pages.len()), (2, 5), "rows that live only in the WAL are read");
        let w = read_vocabulary(&vocab, Some(tmp.path())).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(tree(db.parent().unwrap()), before, "no -shm/-wal created or changed next to KOReader's files");
    }

    #[test]
    fn vocabulary_current_schema() {
        let tmp = TempDir::new("ko-vocab");
        let (_, db) = write_sample_databases(tmp.path(), false, false).unwrap();
        let w = read_vocabulary(&db, None).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].word, "dignity");
        assert_eq!(w[0].title.as_deref(), Some("Romeo and Juliet"));
        assert_eq!(w[0].prev_context.as_deref(), Some("Two households, both alike in "));
        assert_eq!((w[1].word.as_str(), w[1].review_count, w[1].streak_count, w[1].review_time), ("mutiny", 2, 2, Some(1760086400)));
        assert_eq!(w[1].highlight.as_deref(), Some("break to new mutiny"));
    }

    #[test]
    fn old_schemas() {
        let tmp = TempDir::new("ko-old");
        let (db, vocab) = write_sample_databases(tmp.path(), true, false).unwrap();
        let s = read_statistics_db(&db, None).unwrap();
        assert_eq!(s.schema_version, 0);
        assert_eq!(s.pages.len(), 3);
        assert_eq!((s.pages[2].page, s.pages[2].duration, s.pages[2].total_pages), (3, 30, 100), "period column, total from book.pages");
        let w = read_vocabulary(&vocab, None).unwrap();
        assert_eq!((w[0].word.as_str(), w[0].title.as_deref(), w[0].streak_count, w[0].highlight.as_deref()), ("vex", Some("Old Book"), 0, None));
    }

    #[test]
    fn errors() {
        let tmp = TempDir::new("ko-err");
        assert!(matches!(read_statistics_db(&tmp.path().join("none.sqlite3"), None), Err(StoreError::NotFound(_))));
        let junk = tmp.path().join("junk.sqlite3");
        std::fs::write(&junk, b"this is not a database at all, just text....................................................................").unwrap();
        assert!(read_statistics_db(&junk, None).is_err());
        assert!(read_vocabulary(&junk, None).is_err());
        let (_, vocab) = write_sample_databases(&tmp.path().join("x"), false, false).unwrap();
        assert!(matches!(read_statistics_db(&vocab, None), Err(StoreError::Format(_))), "a vocabulary db is not a statistics db");
    }

    /// The user's real databases from a device backup (outside git); skipped when absent.
    /// Prints counts only.
    #[test]
    fn real_databases_if_available() {
        let dir = std::env::var("KOREADER_SAMPLE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../inkreader-wt/.cache/store-ko/koreader")));
        let stats = dir.join("settings/statistics.sqlite3");
        let vocab = dir.join("settings/vocabulary_builder.sqlite3");
        if !stats.exists() || !vocab.exists() {
            eprintln!("skipped: no sample databases in {}", dir.display());
            return;
        }
        let before = (std::fs::read(&stats).unwrap(), std::fs::read(&vocab).unwrap());
        let s = read_statistics_db(&stats, None).unwrap();
        let w = read_vocabulary(&vocab, None).unwrap();
        assert!(s.schema_version >= 20201010);
        assert!(!s.books.is_empty());
        assert!(s.pages.iter().all(|p| p.book_id > 0));
        eprintln!("real statistics: schema {}, {} books, {} page records, {} s; vocabulary: {} words", s.schema_version, s.books.len(), s.pages.len(), s.total_seconds(), w.len());
        assert_eq!(before, (std::fs::read(&stats).unwrap(), std::fs::read(&vocab).unwrap()));
    }
}
