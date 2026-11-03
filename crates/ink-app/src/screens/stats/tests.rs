//! Statistics and vocabulary screens on the headless device (1272x1696).
//! `INK_STATS_OUT=DIR` writes the screenshots.

use crate::app::App;
use crate::router::Route;
use ink_device::headless::{self, HeadlessOptions, HeadlessProbe};
use ink_device::script::parse;
use ink_stats::{Lookup, VocabDb};
use ink_store::stats::{PageRecord, StatsBook};
use ink_store::{Paths, Settings, StatsDb};
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-stats-screen-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn out_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os("INK_STATS_OUT").map(|d| PathBuf::from(d).join(name))
}

fn now() -> u64 {
    ink_stats::unix_now()
}

/// Two books read today, yesterday and the day before.
fn seed_stats(paths: &Paths) {
    let mut db = StatsDb::open(&paths.statistics()).unwrap();
    let t = now();
    for (n, (md5, title)) in [("a", "Romeo and Juliet"), ("b", "Pride and Prejudice")].iter().enumerate() {
        let book = StatsBook { md5: md5.to_string(), title: title.to_string(), authors: "x".into(), pages: Some(200), ..Default::default() };
        let id = db.upsert_book(&book, t).unwrap();
        for day in 0..3u64 {
            let start = t - day * 86_400 - 3600 * (n as u64 + 1);
            let s = db.start_session(id, start).unwrap();
            let recs: Vec<PageRecord> =
                (0..10).map(|i| PageRecord { page: (day * 10 + i) as u32 + 1, total_pages: 200, start_time: start + i * 60, duration: 60 }).collect();
            db.record_pages(s, &recs).unwrap();
            db.end_session(s, start + 600).unwrap();
        }
    }
}

fn seed_words(paths: &Paths) {
    let db = VocabDb::open(&ink_stats::vocabulary_path(paths)).unwrap();
    let t = now();
    let l = |w: &str| Lookup {
        word: w.into(),
        title: Some("Romeo and Juliet".into()),
        prev_context: Some("Two households, both".into()),
        next_context: Some("in dignity".into()),
        ..Default::default()
    };
    db.record_lookup(&l("alike"), t - 100).unwrap();
    db.record_lookup(&Lookup { definition: Some("a long-standing resentment".into()), ..l("grudge") }, t - 50).unwrap();
}

fn run(name: &str, paths: &Paths, script: &str, start: Route) -> HeadlessProbe {
    let (device, probe) = headless::open(HeadlessOptions { script: parse(script).unwrap(), out_dir: out_dir(name), ..Default::default() });
    let mut app = App::new(device, Settings::default(), paths.clone());
    app.run(start).expect("app run");
    probe
}

fn dark_pixels(probe: &HeadlessProbe) -> usize {
    probe.panel().pixels.iter().filter(|v| **v < 64).count()
}

// Tap targets (see mod.rs / vocab.rs): app bar action ("Words", "Practice (N)"), Books
// segment, the flashcard (flip) and the answer buttons.
const RIGHT_BUTTON: &str = "1182 68";
const BOOKS_TAB: &str = "1038 212";
const SHOW: &str = "636 860";
const KNOW: &str = "946 1580";
const DONT_KNOW: &str = "326 1580";

#[test]
fn statistics_overview_books_then_vocabulary_flashcards() {
    let root = tmp("flow");
    let paths = Paths { root: root.clone() };
    seed_stats(&paths);
    seed_words(&paths);
    let script = format!(
        "shot 01-overview\ntap {BOOKS_TAB}\nshot 02-books\ntap {RIGHT_BUTTON}\nshot 03-words\ntap {RIGHT_BUTTON}\nshot 04-card\n\
         tap {SHOW}\nshot 05-shown\ntap {KNOW}\nshot 06-next\ntap {DONT_KNOW}\nshot 07-again\nkey back\nshot 08-list\nkey back\nshot 09-back\n"
    );
    let probe = run("flow", &paths, &script, Route::Statistics);
    // One update per step, but "Don't know" puts the same card face down again: no
    // pixel changes, so that frame sends nothing (app::present_over).
    assert!(probe.updates().len() >= 8, "{:?}", probe.updates());
    assert!(dark_pixels(&probe) > 5_000);
    let db = VocabDb::open(&ink_stats::vocabulary_path(&paths)).unwrap();
    let alike = db.get("alike").unwrap().unwrap();
    assert_eq!((alike.streak_count, alike.review_count), (1, 1), "alike was due first and answered Know");
    assert!(alike.due_time > now() + 80_000);
    let grudge = db.get("grudge").unwrap().unwrap();
    assert_eq!((grudge.streak_count, grudge.review_count), (0, 1), "grudge answered Don't know");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn empty_statistics_and_vocabulary_open_without_creating_files() {
    let root = tmp("empty");
    let paths = Paths { root: root.clone() };
    let script = format!("shot 01-overview\ntap {BOOKS_TAB}\nshot 02-books\ntap {RIGHT_BUTTON}\nshot 03-words\nkey back\nkey back\n");
    let probe = run("empty", &paths, &script, Route::Statistics);
    assert!(probe.updates().len() >= 3);
    assert!(!paths.statistics().exists());
    assert!(!ink_stats::vocabulary_path(&paths).exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn library_header_opens_statistics() {
    let root = tmp("library");
    let paths = Paths { root: root.clone() };
    seed_stats(&paths);
    let books = root.join("books");
    std::fs::create_dir_all(&books).unwrap();
    let settings = Settings { library_dirs: vec![books.clone()], ..Default::default() };
    // Stats icon in the Home app bar (stats, more from the right; no gear while Settings
    // is in the nav bar): centre (1098, 68).
    let (device, probe) = headless::open(HeadlessOptions {
        script: parse("wait 200\ntap 1098 68\nshot stats\n").unwrap(),
        out_dir: out_dir("library"),
        ..Default::default()
    });
    let mut app = App::new(device, settings, paths.clone());
    app.run(Route::Library).expect("app run");
    // The statistics screen shows "Romeo and Juliet"'s time in its tiles: its title row
    // is the header text "Reading statistics"; check the full-screen GC16 it starts with.
    let full = probe.updates().iter().filter(|u| u.rect.w == 1272 && u.rect.h == 1696).count();
    assert!(full >= 2, "library and statistics full-screen updates: {:?}", probe.updates());
    let _ = std::fs::remove_dir_all(root);
}

/// The compiled-in `stats` plugin writes the file the Statistics screen reads.
#[test]
fn stats_plugin_writes_paths_statistics() {
    let root = tmp("plugin");
    let paths = Paths { root: root.clone() };
    let book = root.join("juliet.epub");
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../../testdata/juliet.epub"), &book).unwrap();
    let mut doc = crate::screens::reader::open_book(&crate::app::providers(), &Settings::default(), &book, 1272, 1696).unwrap();
    let mut host = crate::plugin_host::PluginHost::load(crate::plugins::compiled_in(), &Settings::default(), &paths);
    host.book_open(doc.as_mut(), &book);
    host.page_change(doc.as_mut(), 0);
    host.book_close(doc.as_mut());
    let md5 = ink_store::partial_md5(&book).unwrap();
    let db = StatsDb::open(&paths.statistics()).unwrap();
    let t = db.book_totals(&md5).unwrap().expect("the plugin added the book to paths.statistics()");
    assert!(t.title.contains("Romeo"), "{t:?}");
    // The plugin's data folder (plugins/stats/data, no plugin.toml) is not taken for a
    // broken Lua plugin at the next start.
    let again = crate::plugin_host::PluginHost::load(crate::plugins::compiled_in(), &Settings::default(), &paths);
    assert!(again.manager.errors.is_empty(), "{:?}", again.manager.errors);
    assert_eq!(again.plugins().len(), crate::plugins::compiled_in().len());
    let _ = std::fs::remove_dir_all(root);
}
