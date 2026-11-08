//! Dictionary lookups for the vocabulary builder (`vocabulary.sqlite3`).
//!
//! Every lookup from the reader's dictionary is one row in `lookup`: the word as
//! selected, the form the dictionary found, the book (KOReader partial MD5 and title),
//! the time and some text around it. [`VocabDb::words`] groups the rows by word. The
//! vocabulary builder (phase 10) may add its own tables (review times, flashcards) in
//! the same file; this module only creates and fills `lookup`.
//!
//! Added by the wave C `dict` agent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection};

use crate::{Paths, Result, StoreError};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
    CREATE TABLE IF NOT EXISTS lookup (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        word TEXT NOT NULL,
        form TEXT NOT NULL DEFAULT '',
        book_md5 TEXT,
        book_title TEXT,
        time INTEGER NOT NULL,
        found INTEGER NOT NULL DEFAULT 1,
        context TEXT
    );
    CREATE INDEX IF NOT EXISTS lookup_word ON lookup(word);
    CREATE INDEX IF NOT EXISTS lookup_time ON lookup(time);
"#;

/// Where the database lives: `<root>/vocabulary.sqlite3`.
pub fn default_path(paths: &Paths) -> PathBuf {
    paths.root.join("vocabulary.sqlite3")
}

/// One lookup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lookup {
    /// The word as selected (cleaned: punctuation removed, case kept).
    pub word: String,
    /// The dictionary form that was found (`run` for `running`); empty when not found.
    pub form: String,
    /// KOReader partial MD5 of the book.
    pub book_md5: Option<String>,
    /// Book title.
    pub book_title: Option<String>,
    /// Seconds since the epoch.
    pub time: u64,
    /// A dictionary had an entry.
    pub found: bool,
    /// Text around the word (a sentence or so).
    pub context: Option<String>,
}

/// One word with its lookups combined.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VocabWord {
    /// The word, lowercase (the found form when there is one).
    pub word: String,
    /// Number of lookups.
    pub count: u64,
    /// First lookup, seconds since the epoch.
    pub first_time: u64,
    /// Last lookup, seconds since the epoch.
    pub last_time: u64,
    /// Title of the book of the last lookup.
    pub last_title: Option<String>,
}

/// The lookup database.
pub struct VocabDb {
    conn: Connection,
}

impl VocabDb {
    /// Opens (creating if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        crate::create_parent_dir(path)?;
        Self::init(Connection::open(path)?)
    }

    /// [`VocabDb::open`], but a file that is not a database or is damaged is moved aside
    /// to `<name>.corrupt-<secs>-<n>` (with its `-journal`) and a new database is started;
    /// the new name is returned. Other errors (locked, a newer schema) are returned and
    /// nothing is moved.
    pub fn open_or_recover(path: &Path) -> Result<(Self, Option<PathBuf>)> {
        crate::open_db_or_recover(path, Self::open)
    }

    /// An in-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.query_row("PRAGMA journal_mode=TRUNCATE", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            return Err(StoreError::Format(format!("vocabulary schema {v} is newer than {SCHEMA_VERSION}")));
        }
        // vocabulary.sqlite3 holds two tables (ink-store `lookup`, ink-stats `vocabulary`)
        // that share user_version: create ours when it is missing, whatever the version.
        let ours: i64 =
            conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='lookup'", [], |r| r.get(0))?;
        if v < SCHEMA_VERSION || ours == 0 {
            conn.execute_batch(&format!("BEGIN IMMEDIATE; {SCHEMA} PRAGMA user_version={SCHEMA_VERSION}; COMMIT;"))?;
        }
        Ok(VocabDb { conn })
    }

    /// The underlying connection, for queries this API does not cover.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Adds one lookup and returns its row id.
    pub fn record(&mut self, l: &Lookup) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO lookup (word, form, book_md5, book_title, time, found, context) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![l.word, l.form, l.book_md5, l.book_title, l.time as i64, l.found as i64, l.context],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The newest `limit` lookups, newest first.
    pub fn lookups(&self, limit: usize) -> Result<Vec<Lookup>> {
        let sql = "SELECT word, form, book_md5, book_title, time, found, context FROM lookup ORDER BY time DESC, id DESC LIMIT ?1";
        crate::query_all(&self.conn, sql, [limit as i64], |r| {
            Ok(Lookup { word: r.get(0)?, form: r.get(1)?, book_md5: r.get(2)?, book_title: r.get(3)?, time: r.get::<_, i64>(4)?.max(0) as u64, found: r.get::<_, i64>(5)? != 0, context: r.get(6)? })
        })
    }

    /// Words that were found, grouped by lowercase form, most recent first.
    pub fn words(&self) -> Result<Vec<VocabWord>> {
        let sql = "WITH k AS (SELECT lower(CASE WHEN form <> '' THEN form ELSE word END) AS w, time, id, book_title FROM lookup WHERE found = 1)
             SELECT w, count(*), min(time), max(time),
                    (SELECT book_title FROM k k2 WHERE k2.w = k.w ORDER BY time DESC, id DESC LIMIT 1)
             FROM k GROUP BY w ORDER BY max(time) DESC, w";
        crate::query_all(&self.conn, sql, [], |r| Ok(VocabWord { word: r.get(0)?, count: r.get::<_, i64>(1)? as u64, first_time: r.get::<_, i64>(2)?.max(0) as u64, last_time: r.get::<_, i64>(3)?.max(0) as u64, last_title: r.get(4)? }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(word: &str, form: &str, title: &str, time: u64) -> Lookup {
        Lookup { word: word.into(), form: form.into(), book_md5: Some("abc".into()), book_title: Some(title.into()), time, found: !form.is_empty(), context: Some("ctx".into()) }
    }

    #[test]
    fn record_list_and_group() {
        let mut db = VocabDb::open_in_memory().unwrap();
        db.record(&l("Running", "run", "Book A", 100)).unwrap();
        db.record(&l("runs", "run", "Book B", 200)).unwrap();
        db.record(&l("dignity", "dignity", "Book A", 150)).unwrap();
        db.record(&l("xyzzy", "", "Book A", 300)).unwrap();
        let all = db.lookups(10).unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].word, "xyzzy");
        assert!(!all[0].found);
        assert_eq!(all[3], l("Running", "run", "Book A", 100));
        let w = db.words().unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0], VocabWord { word: "run".into(), count: 2, first_time: 100, last_time: 200, last_title: Some("Book B".into()) });
        assert_eq!(w[1].word, "dignity");
    }

    #[test]
    fn file_reopens_and_refuses_newer_schema() {
        let dir = std::env::temp_dir().join(format!("ink-store-vocab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("sub/vocabulary.sqlite3");
        VocabDb::open(&p).unwrap().record(&l("a", "a", "T", 1)).unwrap();
        assert_eq!(VocabDb::open(&p).unwrap().lookups(5).unwrap().len(), 1);
        VocabDb::open(&p).unwrap().connection().execute_batch("PRAGMA user_version=9").unwrap();
        assert!(VocabDb::open(&p).is_err());
        assert_eq!(default_path(&Paths::kindle()), PathBuf::from("/mnt/us/rinky/vocabulary.sqlite3"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn damaged_file_is_set_aside_and_a_new_database_starts() {
        let dir = std::env::temp_dir().join(format!("ink-store-vocab-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("vocabulary.sqlite3");
        std::fs::write(&p, vec![0x41u8; 5000]).unwrap();
        assert!(VocabDb::open(&p).is_err());
        let (mut db, moved) = VocabDb::open_or_recover(&p).unwrap();
        let moved = moved.expect("the bad file was moved");
        assert!(moved.to_string_lossy().contains(".corrupt-"));
        assert_eq!(std::fs::read(&moved).unwrap().len(), 5000);
        db.record(&l("a", "a", "T", 1)).unwrap();
        assert_eq!(db.lookups(5).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
