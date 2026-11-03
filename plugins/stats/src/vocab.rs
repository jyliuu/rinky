//! Looked-up words and flashcard reviews: `<root>/vocabulary.sqlite3`.
//!
//! Table `vocabulary` (the contract with the dictionary plugin). The first columns are those of KOReader's vocabulary
//! builder (`word`, `create_time`, `review_time`, `due_time`, `review_count`,
//! `streak_count`, `prev_context`, `next_context`), with the book title inline instead of
//! KOReader's `title` table, so `ink_store::koreader::KoVocabWord` maps one to one. Extra
//! columns: `book_md5`, `lookup_count`, `definition`.
//!
//! Writers: the dictionary plugin calls [`VocabDb::record_lookup`] after a lookup (or runs
//! the same `INSERT ... ON CONFLICT` itself). The Vocabulary screen reads the list and
//! writes reviews ([`VocabDb::answer`]). Times are seconds since the epoch.

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// Our schema version (`PRAGMA user_version`).
pub const SCHEMA_VERSION: i64 = 1;

/// The table. `CREATE ... IF NOT EXISTS`, so any writer may run it.
pub const SCHEMA: &str = r#"
    CREATE TABLE IF NOT EXISTS vocabulary
        (
            word         TEXT NOT NULL PRIMARY KEY,
            title        TEXT,
            book_md5     TEXT,
            create_time  INTEGER NOT NULL,
            review_time  INTEGER,
            due_time     INTEGER NOT NULL,
            review_count INTEGER NOT NULL DEFAULT 0,
            streak_count INTEGER NOT NULL DEFAULT 0,
            prev_context TEXT,
            next_context TEXT,
            lookup_count INTEGER NOT NULL DEFAULT 1,
            definition   TEXT
        );
    CREATE INDEX IF NOT EXISTS vocabulary_due ON vocabulary(due_time);
    CREATE INDEX IF NOT EXISTS vocabulary_create ON vocabulary(create_time);
"#;

/// Review intervals after a "know" answer, by streak (1st, 2nd, ... correct answer in a
/// row); the last one repeats.
pub const INTERVALS: [u64; 6] = [86_400, 3 * 86_400, 7 * 86_400, 14 * 86_400, 30 * 86_400, 90 * 86_400];

/// A lookup to record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lookup {
    /// The word as looked up (trimmed; empty words are ignored).
    pub word: String,
    /// Book title.
    pub title: Option<String>,
    /// Book partial MD5.
    pub book_md5: Option<String>,
    /// Text before the word (a few words).
    pub prev_context: Option<String>,
    /// Text after the word.
    pub next_context: Option<String>,
    /// A short definition (first sense), if the dictionary has one.
    pub definition: Option<String>,
}

/// One word.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VocabWord {
    /// The word.
    pub word: String,
    /// Book title.
    pub title: Option<String>,
    /// Book partial MD5.
    pub book_md5: Option<String>,
    /// First lookup.
    pub create_time: u64,
    /// Last review.
    pub review_time: Option<u64>,
    /// Next review.
    pub due_time: u64,
    /// Reviews so far.
    pub review_count: u32,
    /// "Know" answers in a row.
    pub streak_count: u32,
    /// Text before the word.
    pub prev_context: Option<String>,
    /// Text after the word.
    pub next_context: Option<String>,
    /// Number of lookups.
    pub lookup_count: u32,
    /// Short definition.
    pub definition: Option<String>,
}

/// The vocabulary database.
pub struct VocabDb {
    conn: Connection,
}

const COLS: &str = "word, title, book_md5, create_time, review_time, due_time, review_count, streak_count, prev_context, next_context, lookup_count, definition";

fn row(r: &rusqlite::Row) -> rusqlite::Result<VocabWord> {
    let u = |v: i64| v.max(0) as u64;
    let n = |v: i64| v.clamp(0, u32::MAX as i64) as u32;
    Ok(VocabWord {
        word: r.get(0)?,
        title: r.get(1)?,
        book_md5: r.get(2)?,
        create_time: u(r.get(3)?),
        review_time: r.get::<_, Option<i64>>(4)?.map(u),
        due_time: u(r.get(5)?),
        review_count: n(r.get(6)?),
        streak_count: n(r.get(7)?),
        prev_context: r.get(8)?,
        next_context: r.get(9)?,
        lookup_count: n(r.get(10)?),
        definition: r.get(11)?,
    })
}

fn nonempty(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

impl VocabDb {
    /// Opens (creating if needed) the database at `path`.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
    }

    /// [`VocabDb::open`], but a file that is not a database or is damaged is moved aside
    /// to `<name>.corrupt-<secs>-<n>` (with its `-journal`) and a new database is started;
    /// the new name is returned. Other errors (locked, read-only) are returned.
    pub fn open_or_recover(path: &Path) -> rusqlite::Result<(Self, Option<std::path::PathBuf>)> {
        match Self::open(path) {
            Ok(db) => Ok((db, None)),
            Err(e) if ink_store::is_damaged_sqlite(&e) => {
                let Some(moved) = ink_store::set_aside_database(path) else { return Err(e) };
                log::warn!("vocabulary: {} is damaged ({e}); kept as {}, starting a new one", path.display(), moved.display());
                Ok((Self::open(path)?, Some(moved)))
            }
            Err(e) => Err(e),
        }
    }

    /// An in-memory database (tests).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        // No WAL: the Kindle's /mnt/us cannot memory-map the WAL index (as statistics).
        conn.query_row("PRAGMA journal_mode=TRUNCATE", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        // Write only when the table is missing or older: an open costs no synced write.
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        // vocabulary.sqlite3 holds two tables (ink-store `lookup`, ink-stats `vocabulary`)
        // that share user_version: create ours when it is missing, whatever the version.
        let ours: i64 =
            conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name='vocabulary'", [], |r| r.get(0))?;
        if v < SCHEMA_VERSION || ours == 0 {
            conn.execute_batch(&format!("BEGIN IMMEDIATE; {SCHEMA} PRAGMA user_version={SCHEMA_VERSION}; COMMIT;"))?;
        }
        Ok(VocabDb { conn })
    }

    /// The connection, for queries this API does not cover.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Records a lookup at `now`: a new word is due at once; a known word counts one more
    /// lookup and takes the new book, context and definition where given. Returns false
    /// for an empty word.
    pub fn record_lookup(&self, l: &Lookup, now: u64) -> rusqlite::Result<bool> {
        let word = l.word.trim();
        if word.is_empty() {
            return Ok(false);
        }
        self.conn.execute(
            "INSERT INTO vocabulary (word, title, book_md5, create_time, due_time, prev_context, next_context, definition)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7)
             ON CONFLICT(word) DO UPDATE SET lookup_count = lookup_count + 1,
                 title = coalesce(excluded.title, title), book_md5 = coalesce(excluded.book_md5, book_md5),
                 prev_context = coalesce(excluded.prev_context, prev_context),
                 next_context = coalesce(excluded.next_context, next_context),
                 definition = coalesce(excluded.definition, definition)",
            params![
                word,
                nonempty(&l.title),
                nonempty(&l.book_md5),
                now as i64,
                nonempty(&l.prev_context),
                nonempty(&l.next_context),
                nonempty(&l.definition)
            ],
        )?;
        Ok(true)
    }

    /// Number of words.
    pub fn count(&self) -> rusqlite::Result<usize> {
        self.conn.query_row("SELECT count(*) FROM vocabulary", [], |r| r.get::<_, i64>(0)).map(|n| n as usize)
    }

    /// Number of words due at `now`.
    pub fn due_count(&self, now: u64) -> rusqlite::Result<usize> {
        self.conn.query_row("SELECT count(*) FROM vocabulary WHERE due_time <= ?1", [now as i64], |r| r.get::<_, i64>(0)).map(|n| n as usize)
    }

    /// Words, newest lookup first.
    pub fn list(&self, offset: usize, limit: usize) -> rusqlite::Result<Vec<VocabWord>> {
        let mut st = self.conn.prepare_cached(&format!("SELECT {COLS} FROM vocabulary ORDER BY create_time DESC, word LIMIT ?1 OFFSET ?2"))?;
        let rows = st.query_map(params![limit as i64, offset as i64], row)?;
        rows.collect()
    }

    /// Words due at `now`, longest overdue first.
    pub fn due(&self, now: u64, limit: usize) -> rusqlite::Result<Vec<VocabWord>> {
        let mut st = self.conn.prepare_cached(&format!("SELECT {COLS} FROM vocabulary WHERE due_time <= ?1 ORDER BY due_time, create_time LIMIT ?2"))?;
        let rows = st.query_map(params![now as i64, limit as i64], row)?;
        rows.collect()
    }

    /// One word.
    pub fn get(&self, word: &str) -> rusqlite::Result<Option<VocabWord>> {
        self.conn.query_row(&format!("SELECT {COLS} FROM vocabulary WHERE word = ?1"), [word], row).optional()
    }

    /// A flashcard answer at `now`: "know" raises the streak and schedules the next review
    /// by [`INTERVALS`]; "don't know" resets the streak and makes the word due at once.
    pub fn answer(&self, word: &str, know: bool, now: u64) -> rusqlite::Result<Option<VocabWord>> {
        let Some(w) = self.get(word)? else { return Ok(None) };
        let streak = if know { w.streak_count + 1 } else { 0 };
        let due = if know { now + INTERVALS[(streak as usize - 1).min(INTERVALS.len() - 1)] } else { now };
        self.conn.execute(
            "UPDATE vocabulary SET review_time = ?2, due_time = ?3, review_count = review_count + 1, streak_count = ?4 WHERE word = ?1",
            params![word, now as i64, due as i64, streak as i64],
        )?;
        self.get(word)
    }

    /// Copies KOReader's vocabulary-builder words in (`ink_store::koreader::read_vocabulary`)
    /// in one transaction. Words already present are kept as they are; returns how many
    /// were added.
    pub fn import_koreader(&mut self, words: &[ink_store::koreader::KoVocabWord]) -> rusqlite::Result<usize> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut added = 0;
        {
            let mut ins = tx.prepare(
                "INSERT INTO vocabulary (word, title, create_time, review_time, due_time, review_count, streak_count, prev_context, next_context)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(word) DO NOTHING",
            )?;
            for w in words {
                let word = w.word.trim();
                if word.is_empty() {
                    continue;
                }
                added += ins.execute(params![
                    word,
                    nonempty(&w.title),
                    w.create_time,
                    w.review_time,
                    w.due_time,
                    w.review_count,
                    w.streak_count,
                    nonempty(&w.prev_context),
                    nonempty(&w.next_context)
                ])?;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// Removes a word; true when it existed.
    pub fn remove(&self, word: &str) -> rusqlite::Result<bool> {
        Ok(self.conn.execute("DELETE FROM vocabulary WHERE word = ?1", [word])? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(word: &str, title: &str) -> Lookup {
        Lookup { word: word.into(), title: Some(title.into()), prev_context: Some("Two households, both".into()), next_context: Some("in dignity".into()), ..Default::default() }
    }

    #[test]
    fn lookups_merge_by_word() {
        let db = VocabDb::open_in_memory().unwrap();
        assert!(db.record_lookup(&look("alike", "Romeo and Juliet"), 100).unwrap());
        assert!(db.record_lookup(&look("grudge", "Romeo and Juliet"), 200).unwrap());
        assert!(!db.record_lookup(&look("  ", "x"), 300).unwrap());
        let again = Lookup { word: " alike ".into(), definition: Some("similar".into()), ..Default::default() };
        assert!(db.record_lookup(&again, 400).unwrap());
        assert_eq!(db.count().unwrap(), 2);
        let w = db.get("alike").unwrap().unwrap();
        assert_eq!(w.lookup_count, 2);
        assert_eq!(w.create_time, 100);
        assert_eq!(w.title.as_deref(), Some("Romeo and Juliet"));
        assert_eq!(w.prev_context.as_deref(), Some("Two households, both"));
        assert_eq!(w.definition.as_deref(), Some("similar"));
        let list: Vec<String> = db.list(0, 10).unwrap().into_iter().map(|w| w.word).collect();
        assert_eq!(list, vec!["grudge", "alike"]);
        assert_eq!(db.list(1, 10).unwrap().len(), 1);
    }

    #[test]
    fn flashcard_schedule() {
        let db = VocabDb::open_in_memory().unwrap();
        db.record_lookup(&look("alike", "R"), 100).unwrap();
        db.record_lookup(&look("grudge", "R"), 200).unwrap();
        assert_eq!(db.due_count(1000).unwrap(), 2);
        assert_eq!(db.due(1000, 10).unwrap()[0].word, "alike");
        let w = db.answer("alike", true, 1000).unwrap().unwrap();
        assert_eq!((w.streak_count, w.review_count, w.due_time, w.review_time), (1, 1, 1000 + INTERVALS[0], Some(1000)));
        let w = db.answer("alike", true, 2000).unwrap().unwrap();
        assert_eq!((w.streak_count, w.due_time), (2, 2000 + INTERVALS[1]));
        let w = db.answer("alike", false, 3000).unwrap().unwrap();
        assert_eq!((w.streak_count, w.review_count, w.due_time), (0, 3, 3000));
        for i in 0..10 {
            db.answer("grudge", true, 5000 + i).unwrap();
        }
        let g = db.get("grudge").unwrap().unwrap();
        assert_eq!(g.due_time, 5009 + INTERVALS[5]);
        assert_eq!(db.due_count(3000).unwrap(), 1);
        assert!(db.answer("missing", true, 1).unwrap().is_none());
        assert!(db.remove("alike").unwrap());
        assert!(!db.remove("alike").unwrap());
    }

    #[test]
    fn koreader_import_keeps_existing_words() {
        use ink_store::koreader::KoVocabWord;
        let mut db = VocabDb::open_in_memory().unwrap();
        db.record_lookup(&look("alike", "R"), 100).unwrap();
        db.answer("alike", true, 200).unwrap();
        let ko = |w: &str| KoVocabWord {
            word: w.into(),
            title: Some("Moby-Dick".into()),
            create_time: 50,
            review_time: Some(60),
            due_time: 70,
            review_count: 2,
            streak_count: 1,
            prev_context: Some("the great".into()),
            next_context: Some("".into()),
            highlight: None,
        };
        assert_eq!(db.import_koreader(&[ko("alike"), ko("leviathan"), ko(" ")]).unwrap(), 1);
        assert_eq!(db.import_koreader(&[ko("leviathan")]).unwrap(), 0, "a second import adds nothing");
        let l = db.get("leviathan").unwrap().unwrap();
        assert_eq!((l.create_time, l.review_time, l.due_time, l.review_count, l.streak_count), (50, Some(60), 70, 2, 1));
        assert_eq!(l.title.as_deref(), Some("Moby-Dick"));
        assert_eq!(l.next_context, None);
        assert_eq!(db.get("alike").unwrap().unwrap().streak_count, 1, "our review kept");
    }

    #[test]
    fn damaged_file_is_set_aside_and_a_new_database_starts() {
        let dir = std::env::temp_dir().join(format!("ink-stats-vocab-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("vocabulary.sqlite3");
        std::fs::write(&p, vec![0x41u8; 5000]).unwrap(); // not a database
        assert!(VocabDb::open(&p).is_err());
        let (db, moved) = VocabDb::open_or_recover(&p).unwrap();
        let moved = moved.expect("the bad file was moved");
        assert!(moved.to_string_lossy().contains(".corrupt-"));
        assert_eq!(std::fs::read(&moved).unwrap().len(), 5000, "the bad bytes are kept");
        assert!(db.record_lookup(&look("alike", "R"), 1).unwrap());
        assert_eq!(db.count().unwrap(), 1);
        drop(db);
        let (db, moved) = VocabDb::open_or_recover(&p).unwrap();
        assert!(moved.is_none() && db.count().unwrap() == 1, "a good file is left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_database_reopens() {
        let dir = std::env::temp_dir().join(format!("ink-stats-vocab-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("vocabulary.sqlite3");
        VocabDb::open(&p).unwrap().record_lookup(&look("alike", "R"), 1).unwrap();
        let db = VocabDb::open(&p).unwrap();
        assert_eq!(db.count().unwrap(), 1);
        let v: i64 = db.connection().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
