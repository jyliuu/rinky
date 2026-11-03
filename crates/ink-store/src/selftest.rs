//! Store checks that run anywhere (host, emulator, Kindle) without test files: used by
//! `rinky selftest` and by `examples/store_selftest.rs`, the binary run on the
//! emulator for evidence. Everything is written under [`SelftestOptions::scratch`]
//! (pass a directory on `/mnt/us` to exercise the user store) and removed afterwards. Nothing
//! outside the scratch directory is written; KOReader's real data is never touched.

use crate::koreader::{self, sqlite};
use crate::library::{synthetic_index, LibraryIndex, SortKey};
use crate::sidecar::{sidecar_path, BookState, Highlight, Quad, ReadingStatus, StateSource};
use crate::stats::{PageRecord, StatsBook, StatsDb};
use crate::{lua, md5, Paths, PluginStore, Settings};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// One check's result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    /// Name (`store.<area>`).
    pub name: String,
    /// Passed.
    pub ok: bool,
    /// What was measured, or the failure.
    pub detail: String,
    /// Run time in milliseconds.
    pub millis: u128,
}

impl std::fmt::Display for Check {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {} ({} ms)", if self.ok { "PASS" } else { "FAIL" }, self.name, self.detail, self.millis)
    }
}

/// What to check.
#[derive(Clone, Debug, Default)]
pub struct SelftestOptions {
    /// Scratch directory (created; removed afterwards).
    pub scratch: PathBuf,
    /// Books to hash (printed, not checked).
    pub books: Vec<PathBuf>,
    /// KOReader's `test/` directory (`leaves.epub`, `tall.pdf`): checks the spec hashes.
    pub koreader_test_dir: Option<PathBuf>,
    /// Number of books in the library benchmark (default 10 000).
    pub library_books: Option<usize>,
    /// Keep the scratch directory (to look at the files written).
    pub keep: bool,
}

const ROLLING: &[u8] = include_bytes!("../tests/fixtures/metadata.rolling.epub.lua");
const PAGING: &[u8] = include_bytes!("../tests/fixtures/metadata.paging.pdf.lua");
const LEGACY: &[u8] = include_bytes!("../tests/fixtures/metadata.legacy.epub.lua");
const NASTY: &[u8] = include_bytes!("../tests/fixtures/nasty.lua");

type R = Result<String, String>;

fn ensure(cond: bool, msg: impl FnOnce() -> String) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(msg())
    }
}

fn e<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Runs every check; returns the results in order.
pub fn run(opts: &SelftestOptions) -> Vec<Check> {
    let scratch = opts.scratch.clone();
    let _ = std::fs::remove_dir_all(&scratch);
    let mut out = Vec::new();
    let mut check = |name: &str, f: &dyn Fn() -> R| {
        let t = Instant::now();
        let r = f();
        let millis = t.elapsed().as_millis();
        let (ok, detail) = match r {
            Ok(d) => (true, d),
            Err(d) => (false, d),
        };
        out.push(Check { name: name.into(), ok, detail, millis });
    };
    let dir = |sub: &str| -> Result<PathBuf, String> {
        let d = scratch.join(sub);
        std::fs::create_dir_all(&d).map_err(|err| format!("{}: {err}", d.display()))?;
        Ok(d)
    };

    check("store.scratch", &|| {
        let d = dir("probe")?;
        Ok(format!("{} ({})", d.display(), fs_type(&d)))
    });

    check("store.md5.koreader_vectors", &|| {
        let d = dir("md5")?;
        let n = md5::vectors::check(Some(&d))?;
        Ok(format!("{n} sizes 0..5000000 equal KOReader util.partialMD5 (LuaJIT), in memory and through files"))
    });

    if let Some(kt) = &opts.koreader_test_dir {
        check("store.md5.koreader_spec", &|| {
            let mut v = Vec::new();
            for (name, want) in md5::vectors::SPEC_FILES {
                let got = md5::partial_md5(&kt.join(name)).map_err(|err| format!("{name}: {err}"))?;
                ensure(got == *want, || format!("{name}: {got}, KOReader spec {want}"))?;
                v.push(format!("{name} {got}"));
            }
            Ok(v.join(", "))
        });
    }
    // Books given on the command line: their partial MD5, compared with the checksum in
    // KOReader's sidecar next to the book when there is one (read only).
    for b in &opts.books {
        check("store.md5.book", &|| {
            let h = md5::partial_md5(b).map_err(|err| format!("{}: {err}", b.display()))?;
            match koreader::read_book_metadata(b) {
                Ok(m) => match m.md5 {
                    Some(ko) if ko == h => Ok(format!("{} {h} = KOReader partial_md5_checksum", b.display())),
                    Some(ko) => Err(format!("{} {h}, KOReader's sidecar says {ko} ({})", b.display(), m.path.display())),
                    None => Ok(format!("{} {h} (KOReader sidecar without checksum)", b.display())),
                },
                Err(_) => Ok(format!("{} {h} (no KOReader sidecar)", b.display())),
            }
        });
    }

    check("store.atomic_write", &|| {
        let d = dir("atomic")?;
        let f = d.join("x.json");
        for i in 0..20 {
            crate::write_atomic(&f, format!("{{\"i\":{i}}}").as_bytes()).map_err(e)?;
        }
        let left: Vec<String> = std::fs::read_dir(&d).map_err(e)?.map(|x| x.map(|x| x.file_name().to_string_lossy().into_owned()).unwrap_or_default()).collect();
        ensure(std::fs::read(&f).map_err(e)? == b"{\"i\":19}" && left == ["x.json"], || format!("left {left:?}"))?;
        Ok("20 rename-over-existing writes, no temp files left".into())
    });

    check("store.sidecar_recovery", &|| {
        let d = dir("books")?;
        let book = d.join("Juliet.epub");
        std::fs::write(&book, b"book").map_err(e)?;
        let mut s = BookState { position: Some("/body/DocFragment[3]/body/p[2]/text().42".into()), status: ReadingStatus::Reading, rating: Some(4), ..Default::default() };
        s.add_highlight(Highlight { start: "/body/DocFragment[3]/body/p[2]/text().0".into(), end: "/body/DocFragment[3]/body/p[2]/text().9".into(), text: "two words".into(), note: Some("n".into()), ..Default::default() });
        s.add_highlight(Highlight { start: "page:1;1,2,3,4".into(), end: "page:1;1,2,3,4".into(), page: Some(1), quads: vec![Quad { x0: 1.0, y0: 2.0, x1: 3.0, y1: 4.0 }], ..Default::default() });
        s.settings.font_px = Some(38.0);
        s.plugin_set("stats", "seconds", serde_json::json!(12));
        s.save(&book).map_err(e)?;
        // What a page turn costs: one save of a sidecar with highlights.
        let t = Instant::now();
        for i in 0..10 {
            let si = BookState { position: Some(format!("/body/DocFragment[4]/body/p[{i}]/text().0")), ..s.clone() };
            si.save(&book).map_err(e)?;
        }
        let per_save = t.elapsed().as_millis() / 10;
        let s2 = BookState { position: Some("/body/DocFragment[4]/body/p[1]/text().0".into()), ..s.clone() };
        s2.save(&book).map_err(e)?;
        let back = BookState::load(&book).map_err(e)?;
        ensure(back.position == s2.position && back.version == BookState::VERSION && back.highlights.len() == 2, || format!("read back {back:?}"))?;
        // Truncated file (power loss): moved aside, backup used.
        std::fs::write(sidecar_path(&book), b"{\"version\":2,\"posi").map_err(e)?;
        let r = BookState::load_report(&book).map_err(e)?;
        ensure(r.source == StateSource::Backup && r.state.position == s.position && r.set_aside.len() == 1, || format!("recovery {:?} {:?}", r.source, r.state.position))?;
        Ok(format!(
            "v{} saved ({per_save} ms per save), reloaded; corrupt file kept as {}, backup used",
            BookState::VERSION,
            r.set_aside[0].file_name().unwrap_or_default().to_string_lossy()
        ))
    });

    check("store.library_benchmark", &|| {
        let d = dir("library")?;
        let n = opts.library_books.unwrap_or(10_000);
        let lib = synthetic_index(n);
        let f = d.join("library.json");
        let t = Instant::now();
        lib.save(&f).map_err(e)?;
        let save = t.elapsed().as_millis();
        let size = std::fs::metadata(&f).map_err(e)?.len();
        let t = Instant::now();
        let back = LibraryIndex::load(&f).map_err(e)?;
        let load = t.elapsed().as_millis();
        let t = Instant::now();
        let recent = back.recent(20).len();
        let sorted = back.sorted(SortKey::Title).len();
        let query = t.elapsed().as_millis();
        ensure(back.books.len() == n && recent == 20.min(n) && sorted == n, || "wrong counts".into())?;
        ensure(back == lib, || "round trip differs".into())?;
        Ok(format!("{n} books, {size} bytes: save {save} ms, load {load} ms, recent+sort {query} ms"))
    });

    check("store.library_scan", &|| {
        let d = dir("scan")?;
        for i in 0..50 {
            std::fs::write(d.join(format!("book{i}.epub")), b"x").map_err(e)?;
        }
        std::fs::create_dir_all(d.join("book0.sdr")).map_err(e)?;
        let mut lib = LibraryIndex::default();
        let (added, removed) = lib.scan(&[&d], 1);
        lib.add_to_collection(&d.join("book3.epub"), "Favourites", 2).map_err(e)?;
        ensure((added, removed) == (50, 0) && lib.collection_books("Favourites").len() == 1, || format!("scan {added}/{removed}"))?;
        Ok(format!("{added} files indexed, collection created"))
    });

    check("store.settings", &|| {
        let d = dir("settings")?;
        let paths = Paths { root: d.clone() };
        let mut s = Settings::default();
        s.set_plugin_value("kosync", &serde_json::json!({"server": "https://sync.example", "port": 443})).map_err(e)?;
        s.save(&paths.settings()).map_err(e)?;
        ensure(Settings::load(&paths.settings()).map_err(e)? == s, || "settings differ".into())?;
        let mut state = BookState::default();
        {
            let mut ps = PluginStore::open(&paths, "insightful", Some(&mut state)).map_err(e)?;
            ps.set("model", serde_json::json!("gpt")).map_err(e)?;
            ps.book_set("chats", serde_json::json!(["c1"])).map_err(e)?;
            ensure(ps.data_dir().is_dir(), || "no data dir".into())?;
        }
        let ps = PluginStore::open(&paths, "insightful", None).map_err(e)?;
        ensure(ps.get("model") == Some(serde_json::json!("gpt")), || "plugin setting lost".into())?;
        ensure(state.plugin_get("insightful", "chats").is_some(), || "per-book value lost".into())?;
        ensure(PluginStore::open(&paths, "../x", None).is_err(), || "bad name accepted".into())?;
        Ok("settings.toml, plugins/insightful/settings.json, per-book data, data dir".into())
    });

    check("store.lua_parser", &|| {
        let t = lua::parse_table(NASTY).map_err(e)?;
        ensure(t.str("nul_digit") == Some("a\u{0}1b") && t.str("cjk") == Some("中文 text"), || "string escapes".into())?;
        for bad in ["os.execute('reboot')", "return { a = io.open('/etc/passwd') }", "return {", "return { [nil] = 1 }"] {
            ensure(lua::parse(bad.as_bytes()).is_err(), || format!("accepted {bad:?}"))?;
        }
        let deep = "{".repeat(100_000);
        ensure(lua::parse(deep.as_bytes()).is_err(), || "depth limit".into())?;
        Ok("KOReader dump.lua fixture parsed; code and 100000-deep nesting refused".into())
    });

    check("store.koreader_metadata", &|| {
        let d = dir("ko-books")?;
        let mut v = Vec::new();
        for (name, bytes, highlights, bookmarks) in [("juliet.epub", ROLLING, 2, 2), ("scan.pdf", PAGING, 2, 1), ("old.epub", LEGACY, 3, 1)] {
            let book = d.join(name);
            std::fs::write(&book, name).map_err(e)?;
            let mp = koreader::metadata_path(&book);
            std::fs::create_dir_all(mp.parent().unwrap_or(&d)).map_err(e)?;
            std::fs::write(&mp, bytes).map_err(e)?;
            let m = koreader::read_book_metadata(&book).map_err(e)?;
            let (h, b) = (m.highlights().count(), m.bookmarks().count());
            ensure((h, b) == (highlights, bookmarks), || format!("{name}: {h} highlights, {b} bookmarks"))?;
            let sum = koreader::import_book(&book, None, false).map_err(e)?.ok_or("nothing imported")?;
            ensure(std::fs::read(&mp).map_err(e)? == bytes, || format!("{name}: KOReader file changed"))?;
            let st = BookState::load(&book).map_err(e)?;
            ensure(st.position == m.position() && st.highlights.len() == h, || format!("{name}: import {st:?}"))?;
            v.push(format!("{name} {}h/{}b pos {}", sum.highlights, sum.bookmarks, m.position().unwrap_or_default()));
        }
        Ok(v.join("; "))
    });

    check("store.koreader_sqlite", &|| {
        let d = dir("ko-db")?;
        let (st, vo) = sqlite::write_sample_databases(&d.join("settings"), false, true).map_err(e)?;
        let snapshot = |p: &Path| -> Result<Vec<(String, Vec<u8>)>, String> {
            let mut v = Vec::new();
            for x in std::fs::read_dir(p).map_err(e)? {
                let x = x.map_err(e)?;
                v.push((x.file_name().to_string_lossy().into_owned(), std::fs::read(x.path()).map_err(e)?));
            }
            v.sort();
            Ok(v)
        };
        let before = snapshot(&d.join("settings"))?;
        let s = sqlite::read_statistics_db(&st, Some(&d)).map_err(e)?;
        let w = sqlite::read_vocabulary(&vo, Some(&d)).map_err(e)?;
        ensure(s.books.len() == 2 && s.pages.len() == 5 && w.len() == 2, || format!("{} books {} pages {} words", s.books.len(), s.pages.len(), w.len()))?;
        ensure(snapshot(&d.join("settings"))? == before, || "KOReader database files changed".into())?;
        let (old_st, old_vo) = sqlite::write_sample_databases(&d.join("old"), true, false).map_err(e)?;
        let s_old = sqlite::read_statistics_db(&old_st, Some(&d)).map_err(e)?;
        let w_old = sqlite::read_vocabulary(&old_vo, Some(&d)).map_err(e)?;
        ensure(s_old.pages.len() == 3 && w_old.len() == 1, || "old schemas".into())?;
        Ok(format!("WAL statistics: {} books {} pages {} s; vocabulary {} words; files unchanged; 2019/2022 schemas read", s.books.len(), s.pages.len(), s.total_seconds(), w.len()))
    });

    check("store.reading_stats", &|| {
        let d = dir("stats")?;
        let mut db = StatsDb::open(&d.join("statistics.sqlite3")).map_err(e)?;
        let book = db.upsert_book(&StatsBook { md5: "cf7ce9b8d4e135c8ac340f18d1e58c85".into(), title: "Romeo and Juliet".into(), pages: Some(180), ..Default::default() }, 1_760_200_000).map_err(e)?;
        let t = Instant::now();
        let s = db.start_session(book, 1_760_200_000).map_err(e)?;
        for i in 0..50u32 {
            db.record_page(s, PageRecord { page: 10 + i, total_pages: 180, start_time: 1_760_200_000 + i as u64 * 30, duration: 30 }).map_err(e)?;
        }
        let per_page = t.elapsed().as_millis() / 50;
        let done = db.end_session(s, 1_760_201_600).map_err(e)?;
        ensure(done.duration == 1500 && done.pages == 50, || format!("session {done:?}"))?;
        let (ko, _) = sqlite::write_sample_databases(&d.join("ko"), false, false).map_err(e)?;
        let c = db.import_koreader(&sqlite::read_statistics_db(&ko, Some(&d)).map_err(e)?).map_err(e)?;
        let tot = db.book_totals("cf7ce9b8d4e135c8ac340f18d1e58c85").map_err(e)?.ok_or("no totals")?;
        ensure(tot.read_time == 1500 + 95 && tot.sessions == 2, || format!("totals {tot:?}"))?;
        Ok(format!("50 page records ({per_page} ms each), session {} s; KOReader import {} pages {} sessions; book total {} s", done.duration, c.pages, c.sessions, tot.read_time))
    });

    if !opts.keep {
        let _ = std::fs::remove_dir_all(&scratch);
    }
    out
}

/// The file system type of `p` from `/proc/mounts` (Linux), else `?`.
fn fs_type(p: &Path) -> String {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else { return "?".into() };
    let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let mut best = ("?".to_string(), 0usize);
    for line in mounts.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() >= 3 && p.starts_with(f[1]) && f[1].len() >= best.1 {
            best = (format!("{} on {}", f[2], f[1]), f[1].len());
        }
    }
    best.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_checks_pass_on_the_host() {
        let tmp = crate::testutil::TempDir::new("selftest");
        let kt = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../koreader/test"));
        let opts = SelftestOptions {
            scratch: tmp.path().join("run"),
            koreader_test_dir: kt.join("leaves.epub").exists().then_some(kt),
            library_books: Some(2_000),
            ..Default::default()
        };
        let checks = run(&opts);
        for c in &checks {
            eprintln!("{c}");
        }
        assert!(checks.len() >= 11);
        assert!(checks.iter().all(|c| c.ok), "failures: {:?}", checks.iter().filter(|c| !c.ok).collect::<Vec<_>>());
        assert!(!opts.scratch.exists(), "scratch removed");
    }
}
