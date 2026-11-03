//! Fills `ROOT/statistics.sqlite3` and `ROOT/vocabulary.sqlite3` with sample reading
//! (three books over the last three weeks) and looked-up words, for simulator
//! screenshots of the Statistics and Vocabulary screens:
//!
//! ```text
//! cargo run -p ink-stats --example seed -- "$TMPDIR/rinky-sim"
//! ```

use ink_stats::{Lookup, VocabDb};
use ink_store::stats::{PageRecord, StatsBook, StatsDb};
use ink_store::Paths;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args().nth(1).ok_or("usage: seed ROOT")?;
    let paths = Paths { root: root.into() };
    let mut db = StatsDb::open(&paths.statistics())?;
    let now = ink_stats::unix_now();
    let books = [
        ("11111111111111111111111111111111", "Romeo and Juliet", "William Shakespeare", 180u32),
        ("22222222222222222222222222222222", "Pride and Prejudice", "Jane Austen", 420),
        ("33333333333333333333333333333333", "Moby-Dick", "Herman Melville", 720),
    ];
    let mut page = [0u32; 3];
    for day in (0..21u64).rev() {
        // A gap on days 5 and 12, so the streak is 5 days and the calendar has holes.
        if day == 5 || day == 12 {
            continue;
        }
        let b = (day % 3) as usize;
        let (md5, title, author, pages) = books[b];
        let id = db.upsert_book(&StatsBook { md5: md5.into(), title: title.into(), authors: author.into(), pages: Some(pages), ..Default::default() }, now)?;
        let start = now - day * 86_400 - 2 * 3600;
        let n = 6 + (day * 7 % 25) as u32;
        let s = db.start_session(id, start)?;
        let recs: Vec<PageRecord> = (0..n)
            .map(|i| {
                page[b] += 1;
                PageRecord { page: page[b], total_pages: pages, start_time: start + i as u64 * 75, duration: 40 + (i * 13 % 70) }
            })
            .collect();
        db.record_pages(s, &recs)?;
        db.end_session(s, start + n as u64 * 75)?;
    }
    let vocab = VocabDb::open(&ink_stats::vocabulary_path(&paths))?;
    let words = [
        ("alike", "Two households, both", "in dignity,", None),
        ("grudge", "From ancient", "break to new mutiny,", Some("a persistent feeling of ill will or resentment")),
        ("mutiny", "From ancient grudge break to new", "where civil blood", None),
        ("loins", "From forth the fatal", "of these two foes", None),
        ("star-cross'd", "A pair of", "lovers take their life;", Some("ill-fated by a malign star")),
        ("piteous", "Whose misadventured", "overthrows", None),
        ("entail", "it would", "upon her", Some("settle the inheritance of property over several generations")),
        ("civility", "with great", "", None),
        ("leviathan", "the great", "of the deep", Some("a sea monster; anything very large")),
        ("harpooneer", "the", "was a dark", None),
    ];
    for (i, (w, prev, next, def)) in words.iter().enumerate() {
        let title = if i < 6 { "Romeo and Juliet" } else if i < 8 { "Pride and Prejudice" } else { "Moby-Dick" };
        let l = Lookup {
            word: w.to_string(),
            title: Some(title.into()),
            prev_context: Some(prev.to_string()),
            next_context: Some(next.to_string()),
            definition: def.map(String::from),
            ..Default::default()
        };
        vocab.record_lookup(&l, now - (words.len() - i) as u64 * 3600)?;
    }
    vocab.record_lookup(&Lookup { word: "grudge".into(), ..Default::default() }, now)?;
    println!("seeded {} and {}", paths.statistics().display(), ink_stats::vocabulary_path(&paths).display());
    Ok(())
}
