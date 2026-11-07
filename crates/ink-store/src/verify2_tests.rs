//! Adversarial tests from the wave A verifier, cycle 2. Each test is one attack on the
//! store after the cycle 1 fixes. A test marked `#[ignore = "verifier: ..."]` reproduces
//! a defect that is still open; run it with
//! `cargo test -p ink-store --release verifier2_ -- --ignored --nocapture`.
//! Tests marked `#[ignore = "measurement: ..."]` only print numbers (for the evidence);
//! tests/e2e/store-verify2.sh runs them on ARM under a time limit.

use crate::koreader;
use crate::lua;
use std::path::Path;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------
// KOReader import: time on large inputs that pass the size limits
// ---------------------------------------------------------------------------------------

/// Phases of one KOReader import of `src`: Lua parse, metadata extraction, merge into
/// our state. Returns (parse, metadata, merge, highlights + bookmarks imported).
fn import_phases(src: &[u8], paging: bool) -> (Duration, Duration, Duration, usize) {
    let t = Instant::now();
    let table = lua::parse_table(src).unwrap();
    let parse = t.elapsed();
    let t = Instant::now();
    let m = koreader::metadata_from_table(Path::new("/x/metadata.epub.lua"), &table, Some(paging));
    let meta = t.elapsed();
    let t = Instant::now();
    let s = m.to_book_state();
    let merge = t.elapsed();
    (parse, meta, merge, s.highlights.len() + s.bookmarks.len())
}

/// Modern KOReader format (2024+): `annotations` with `n` distinct highlights.
fn modern_highlights(n: usize) -> Vec<u8> {
    let mut s = String::from("return { annotations = {\n");
    for i in 0..n {
        s.push_str(&format!("{{pos0=\"/body/p[{i}]/text().0\",pos1=\"/body/p[{i}]/text().9\"}},\n"));
    }
    s.push_str("} }\n");
    s.into_bytes()
}

/// Modern KOReader format: `annotations` with `n` distinct page bookmarks (no `pos0`).
fn modern_bookmarks(n: usize) -> Vec<u8> {
    let mut s = String::from("return { annotations = {\n");
    for i in 0..n {
        s.push_str(&format!("{{page=\"/body/p[{i}]\"}},\n"));
    }
    s.push_str("} }\n");
    s.into_bytes()
}

/// KOReader before 2024: `n` highlighted bookmarks and `n` highlights, all with the same
/// `datetime` and distinct positions (every bookmark/highlight comparison runs in full).
fn legacy_pairs(n: usize) -> Vec<u8> {
    let mut s = String::from("return { bookmarks = {\n");
    for i in 0..n {
        s.push_str(&format!("{{datetime=\"d\",highlighted=true,notes=\"t\",pos0=\"/p[{i}]\",pos1=\"/q[{i}]\",page=\"/p[{i}]\"}},\n"));
    }
    s.push_str("}, highlight = {\n");
    for i in 0..n {
        s.push_str(&format!("[{}]={{{{datetime=\"d\",pos0=\"/p[{}]\",pos1=\"/q[{}]\"}}}},\n", i + 1, n - 1 - i, n - 1 - i));
    }
    s.push_str("} }\n");
    s.into_bytes()
}

/// A metadata file exactly as KOReader's `dump.lua` writes it (4-space indentation,
/// `["key"] = value,` lines, the fields of a real EPUB highlight; see
/// tests/fixtures/metadata.rolling.epub.lua) with `n` distinct highlights.
fn koreader_dump(n: usize) -> Vec<u8> {
    let mut s = String::from("-- we can read Lua syntax here!\nreturn {\n    [\"annotations\"] = {\n");
    for i in 0..n {
        let (frag, para) = (i / 40 + 1, i % 40 + 1);
        s.push_str(&format!(
            "        [{}] = {{\n            [\"chapter\"] = \"Chapter {frag}\",\n            [\"color\"] = \"yellow\",\n            [\"datetime\"] = \"2025-03-04 21:15:03\",\n            [\"drawer\"] = \"lighten\",\n            [\"page\"] = \"/body/DocFragment[{frag}]/body/div/p[{para}]/text().0\",\n            [\"pageno\"] = {},\n            [\"pos0\"] = \"/body/DocFragment[{frag}]/body/div/p[{para}]/text().0\",\n            [\"pos1\"] = \"/body/DocFragment[{frag}]/body/div/p[{para}]/text().57\",\n            [\"text\"] = \"A highlighted sentence of ordinary length in the book.\",\n        }},\n",
            i + 1,
            i / 3 + 1
        ));
    }
    s.push_str("    },\n    [\"doc_pages\"] = 9000,\n    [\"last_xpointer\"] = \"/body/DocFragment[3]/body/div/p[2]/text().0\",\n    [\"partial_md5_checksum\"] = \"cf7ce9b8d4e135c8ac340f18d1e58c85\",\n    [\"percent_finished\"] = 0.25,\n    [\"summary\"] = {\n        [\"status\"] = \"reading\",\n    },\n}\n");
    s.into_bytes()
}

/// Largest `n` for which `make(n)` stays within the parser's limits.
fn largest_accepted(make: impl Fn(usize) -> Vec<u8>) -> usize {
    let (mut lo, mut hi) = (1usize, 2_000_000usize);
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        let src = make(mid);
        let values_ok = src.len() <= lua::MAX_INPUT && lua::parse_table(&src).is_ok();
        if values_ok {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Times an import at `n` and `2n` (fastest of 3 runs each, so a busy machine does not
/// fake a ratio) and returns the ratio of the times (2 for linear work, 4 for quadratic
/// work), printing the phases.
fn scaling(name: &str, make: &dyn Fn(usize) -> Vec<u8>, n: usize, paging: bool) -> f64 {
    let mut totals = Vec::new();
    for k in [n, 2 * n] {
        let src = make(k);
        let mut best = Duration::MAX;
        for _ in 0..3 {
            let (p, m, g, count) = import_phases(&src, paging);
            assert_eq!(count, k, "{name}: every item imported");
            eprintln!(
                "verifier2 {name}: n={k} ({} KiB): parse {} ms, metadata {} ms, merge {} ms",
                src.len() / 1024,
                p.as_millis(),
                m.as_millis(),
                g.as_millis()
            );
            best = best.min(p + m + g);
        }
        totals.push(best.as_secs_f64());
    }
    totals[1] / totals[0]
}

/// A KOReader metadata file with many highlights (all within MAX_INPUT and MAX_VALUES)
/// must import in time linear in its size: doubling the highlights may at most about
/// double the time (a timing ratio: run on an idle machine).
#[test]
#[ignore = "slow: run with --ignored before a release"]
fn verifier2_koreader_import_highlights_scale_linearly() {
    let r = scaling("modern highlights", &modern_highlights, 20_000, false);
    assert!(r < 3.0, "doubling the highlights multiplied the import time by {r:.2}");
}

/// The same for page bookmarks.
#[test]
#[ignore = "slow: run with --ignored before a release"]
fn verifier2_koreader_import_bookmarks_scale_linearly() {
    let r = scaling("modern bookmarks", &modern_bookmarks, 20_000, false);
    assert!(r < 3.0, "doubling the bookmarks multiplied the import time by {r:.2}");
}

/// The same for the pre-2024 format (bookmarks matched to highlights).
#[test]
#[ignore = "slow: run with --ignored before a release"]
fn verifier2_koreader_legacy_import_scales_linearly() {
    let r = scaling("legacy pairs", &legacy_pairs, 5_000, false);
    assert!(r < 3.0, "doubling the legacy bookmarks and highlights multiplied the import time by {r:.2}");
}

/// Absolute numbers at the largest file of one shape that the parser accepts (printed
/// for the evidence; run alone in release with a time limit).
fn limits_timing(name: &str, make: &dyn Fn(usize) -> Vec<u8>) {
    let t = Instant::now();
    let n = largest_accepted(make);
    let src = make(n);
    eprintln!("verifier2 limits {name}: n={n} ({} KiB) is the largest accepted input (search {} ms); importing", src.len() / 1024, t.elapsed().as_millis());
    let t = Instant::now();
    let (p, m, g, count) = import_phases(&src, false);
    eprintln!(
        "verifier2 limits {name}: n={n} ({} KiB, {count} imported): parse {} ms, metadata {} ms, merge {} ms, total {} s",
        src.len() / 1024,
        p.as_millis(),
        m.as_millis(),
        g.as_millis(),
        t.elapsed().as_secs()
    );
}

#[test]
#[ignore = "measurement: import time of the largest accepted file of compact highlights (minutes when quadratic)"]
fn verifier2_limits_timing_modern_highlights() {
    limits_timing("modern highlights", &modern_highlights);
}

#[test]
#[ignore = "measurement: import time of the largest accepted file of compact bookmarks (minutes when quadratic)"]
fn verifier2_limits_timing_modern_bookmarks() {
    limits_timing("modern bookmarks", &modern_bookmarks);
}

#[test]
#[ignore = "measurement: import time of the largest accepted file of legacy bookmark/highlight pairs (minutes when quadratic)"]
fn verifier2_limits_timing_legacy_pairs() {
    limits_timing("legacy pairs", &legacy_pairs);
}

/// What a real file costs: the largest file in KOReader's own `dump.lua` layout under
/// MAX_INPUT, imported through the public path (`koreader::import_book`: partial MD5,
/// parse, merge, save of our sidecar), then our sidecar loaded and saved again (what
/// the reader does on open and on every page).
#[test]
#[ignore = "measurement: full import_book of the largest dump.lua-formatted metadata file, plus sidecar load and save times"]
fn verifier2_limits_timing_koreader_dump_format() {
    let n = largest_accepted(koreader_dump);
    let src = koreader_dump(n);
    let dir = crate::testutil::TempDir::new("v2-dump");
    let book = dir.path().join("big.epub");
    std::fs::write(&book, b"not really an epub").unwrap();
    std::fs::create_dir_all(koreader::koreader_sidecar_dir(&book)).unwrap();
    std::fs::write(koreader::metadata_path(&book), &src).unwrap();
    let (p, m, g, count) = import_phases(&src, false);
    eprintln!(
        "verifier2 limits dump format: n={n} ({} KiB): parse {} ms, metadata {} ms, merge {} ms ({count} imported)",
        src.len() / 1024,
        p.as_millis(),
        m.as_millis(),
        g.as_millis()
    );
    let t = Instant::now();
    let sum = koreader::import_book(&book, None, true).unwrap().unwrap();
    let import_ms = t.elapsed().as_millis();
    let json = std::fs::metadata(crate::sidecar::sidecar_path(&book)).unwrap().len();
    let t = Instant::now();
    let s = crate::sidecar::BookState::load(&book).unwrap();
    let load_ms = t.elapsed().as_millis();
    let t = Instant::now();
    s.save(&book).unwrap();
    let save_ms = t.elapsed().as_millis();
    assert_eq!(sum.highlights, n);
    eprintln!(
        "verifier2 limits dump format: import_book {import_ms} ms ({} highlights), sidecar {} KiB: load {load_ms} ms, save {save_ms} ms",
        sum.highlights,
        json / 1024
    );
}

// ---------------------------------------------------------------------------------------
// Values JSON or TOML cannot read back (nesting deeper than the parser's limit)
// ---------------------------------------------------------------------------------------

/// `{"prev": {"prev": ... "first message"}}`, `depth` levels: a reply chain as a plugin
/// may store it. serde_json writes any depth but reads back at most 128 levels.
fn deep_object(depth: usize) -> serde_json::Value {
    deep_value("objects", "prev", depth, serde_json::json!("first message"))
}

/// `depth` nested objects (`{key: ...}`) or arrays (`[...]`) around `leaf`, built
/// without `json!` (which re-serializes its argument recursively).
fn deep_value(shape: &str, key: &str, depth: usize, leaf: serde_json::Value) -> serde_json::Value {
    let mut v = leaf;
    for _ in 0..depth {
        v = if shape == "objects" {
            let mut m = serde_json::Map::new();
            m.insert(key.to_string(), v);
            serde_json::Value::Object(m)
        } else {
            serde_json::Value::Array(vec![v])
        };
    }
    v
}

/// A plugin value nested deeper than serde_json reads back must never reach the disk:
/// whatever `save` does with it, the file on disk always loads afterwards, nothing is
/// moved aside and the last good state is kept.
#[test]
fn verifier2_sidecar_deep_plugin_value_never_makes_the_file_unloadable() {
    use crate::sidecar::{sidecar_path, BookState, StateSource};
    let dir = crate::testutil::TempDir::new("v2-deep");
    let book = dir.path().join("d.epub");
    BookState { position: Some("flow:1.0.0".into()), ..Default::default() }.save(&book).unwrap();
    let mut s = BookState::load(&book).unwrap();
    s.plugin_set("chat", "thread", deep_object(200));
    s.position = Some("flow:9.0.0".into());
    let saved = s.save(&book);
    let r = BookState::load_report(&book).unwrap();
    assert_eq!((r.source, r.set_aside.len()), (StateSource::Sidecar, 0), "save gave {saved:?}; {:?}", r.errors);
    assert!(serde_json::from_slice::<serde_json::Value>(&std::fs::read(sidecar_path(&book)).unwrap()).is_ok());
    let pos = r.state.position.as_deref();
    assert!(pos == Some("flow:1.0.0") || pos == Some("flow:9.0.0"), "{pos:?}");
}

/// One plugin value that is too deep must not stop the book's reading position from
/// being saved. `BookState::save` used to refuse the whole state with `Invalid`
/// (nothing written), so after one such `plugin_set` every later save of that book
/// failed. Now `sanitized` drops the value, like non-finite numbers.
#[test]
fn verifier2_sidecar_deep_plugin_value_does_not_block_position_saves() {
    use crate::sidecar::BookState;
    let dir = crate::testutil::TempDir::new("v2-deep-pos");
    let book = dir.path().join("d.epub");
    let mut s = BookState { position: Some("flow:1.0.0".into()), ..Default::default() };
    s.save(&book).unwrap();
    s.plugin_set("chat", "thread", deep_object(200));
    for page in 2..5 {
        s.position = Some(format!("flow:{page}.0.0"));
        let r = s.save(&book);
        assert!(r.is_ok(), "page {page}: save refused: {r:?}");
    }
    assert_eq!(BookState::load(&book).unwrap().position.as_deref(), Some("flow:4.0.0"));
}

/// The same value through `PluginStore::set`: it writes `settings.json` without reading
/// the bytes back (unlike `BookState::save` since cycle 1). If the deep value reaches the
/// disk, the next `open` finds a file that does not parse, moves it aside and uses the
/// `.old` backup: settings written after the backup are lost silently. Expected: `set`
/// refuses the value (nothing written) or the value loads again; either way every other
/// setting survives a reopen.
#[test]
#[ignore = "verifier: PluginStore::set writes a value serde_json cannot read back; on the next open the whole settings.json is set aside and later settings are lost"]
fn verifier2_plugin_settings_deep_value_never_loses_other_settings() {
    use crate::plugin::PluginStore;
    use crate::Paths;
    use serde_json::json;
    let dir = crate::testutil::TempDir::new("v2-plugin-deep");
    let paths = Paths { root: dir.path().to_path_buf() };
    {
        let mut st = PluginStore::open(&paths, "chat", None).unwrap();
        st.set("a", json!(1)).unwrap();
        let deep = st.set("thread", deep_object(200));
        eprintln!("verifier2 plugin deep: set returned {deep:?}");
        st.set("b", json!(2)).unwrap();
    }
    let st = PluginStore::open(&paths, "chat", None).unwrap();
    let names: Vec<String> = std::fs::read_dir(paths.plugin_dir("chat")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!((st.get("a"), st.get("b")), (Some(json!(1)), Some(json!(2))), "files now: {names:?}");
    assert!(!names.iter().any(|n| n.contains(".corrupt-")), "a file was set aside: {names:?}");
}

/// A deep value as the very first plugin setting (no `.old` backup exists yet): if it is
/// written, the reopened store is empty.
#[test]
#[ignore = "verifier: PluginStore::set writes a value serde_json cannot read back; with no backup yet the reopened plugin settings are empty"]
fn verifier2_plugin_settings_first_deep_value_keeps_the_file_loadable() {
    use crate::plugin::PluginStore;
    use crate::Paths;
    use serde_json::json;
    let dir = crate::testutil::TempDir::new("v2-plugin-deep-first");
    let paths = Paths { root: dir.path().to_path_buf() };
    {
        let mut st = PluginStore::open(&paths, "chat", None).unwrap();
        let _ = st.set("thread", deep_object(200));
        st.set("lang", json!("da")).unwrap();
    }
    let st = PluginStore::open(&paths, "chat", None).unwrap();
    assert_eq!(st.get("lang"), Some(json!("da")));
}

/// Sets `[plugins.deep]` to a value of `depth` nested objects or arrays, saves and loads
/// strictly. Returns (set_plugin_value ok, save ok, strict load ok, load error).
fn settings_deep_round_trip(f: &Path, shape: &str, depth: usize) -> (bool, bool, bool, String) {
    use crate::settings::Settings;
    let v = deep_value(shape, "k", depth, serde_json::json!(1));
    let _ = std::fs::remove_file(f);
    let _ = std::fs::remove_file(crate::backup_path(f));
    let mut s = Settings { language: "da".into(), ..Default::default() };
    let set = s.set_plugin_value("deep", &v).is_ok();
    let saved = set && s.save(f).is_ok();
    let loaded = Settings::load(f);
    let ok = loaded.as_ref().is_ok_and(|b| b.language == "da");
    (set, saved, ok, loaded.err().map(|e| e.to_string()).unwrap_or_default())
}

/// The global settings: a plugin's `[plugins.<name>]` table nested deeper than the TOML
/// reader accepts. After `set_plugin_value` and `save`, the strict `Settings::load`
/// (what ink-app calls at start) must still read the file, or `set_plugin_value`/`save`
/// must refuse the value. Prints the smallest depth that breaks it.
#[test]
#[ignore = "verifier: Settings::save writes a [plugins.<name>] value nested deeper than the TOML reader accepts; strict Settings::load then fails on the whole settings.toml"]
fn verifier2_settings_deep_plugin_value_keeps_settings_loadable() {
    let dir = crate::testutil::TempDir::new("v2-settings-deep");
    let f = dir.path().join("settings.toml");
    let mut broken = Vec::new();
    for shape in ["objects", "arrays"] {
        // Smallest depth in 1..=200 that is written but does not load (if any). Much deeper
        // values overflow the stack inside serde/toml serialization (minor: the plugin
        // host must bound plugin values).
        let bad = |d: usize| {
            let (set, saved, ok, _) = settings_deep_round_trip(&f, shape, d);
            set && saved && !ok
        };
        if bad(200) {
            let (mut lo, mut hi) = (1usize, 200usize);
            while lo + 1 < hi {
                let mid = (lo + hi) / 2;
                if bad(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let (_, _, _, err) = settings_deep_round_trip(&f, shape, hi);
            eprintln!("verifier2 settings deep {shape}: depth {lo} loads, depth {hi} is saved but strict Settings::load fails: {err}");
            broken.push(format!("{shape} at depth {hi}: {err}"));
        } else {
            eprintln!("verifier2 settings deep {shape}: depth 200 is refused or loads again");
        }
    }
    assert!(broken.is_empty(), "settings saved that do not load: {broken:?}");
}

/// The rename gap of the Kindle user store (the file is missing while a new version is
/// renamed over it), for `settings.toml`: `Settings::load_or_recover` waits for the save
/// and returns the new settings, not the `.old` backup and not the defaults. The strict
/// `Settings::load` (what `ink-app` calls today) does not wait: it returns the defaults in
/// the gap (printed; a minor finding, `load_or_recover` is the documented fix).
#[test]
fn verifier2_settings_load_or_recover_waits_for_a_save_in_progress() {
    use crate::settings::Settings;
    let dir = crate::testutil::TempDir::new("v2-settings-gap");
    let f = dir.path().join("settings.toml");
    let mut s = Settings { language: "da".into(), ..Default::default() };
    s.save(&f).unwrap();
    s.language = "de".into();
    s.save(&f).unwrap();
    assert!(crate::backup_path(&f).exists());
    let mut tmp = f.clone().into_os_string();
    tmp.push(".tmp.4242.0");
    let tmp = std::path::PathBuf::from(tmp);
    // Strict load in the gap.
    std::fs::rename(&f, &tmp).unwrap();
    let strict = Settings::load(&f).unwrap();
    eprintln!("verifier2 settings gap: strict Settings::load returned language {:?} (saved: \"de\")", strict.language);
    // load_or_recover in the gap, the writer finishes its rename 30 ms later.
    let t = std::thread::spawn({
        let (f, tmp) = (f.clone(), tmp.clone());
        move || {
            std::thread::sleep(Duration::from_millis(30));
            std::fs::rename(&tmp, &f).unwrap();
        }
    });
    let (back, warning) = Settings::load_or_recover(&f).unwrap();
    t.join().unwrap();
    assert_eq!((back.language.as_str(), warning), ("de", None));
}

// ---------------------------------------------------------------------------------------
// Sidecar save cost with many highlights (the reader saves after every page)
// ---------------------------------------------------------------------------------------

/// ms per `BookState::save` and `BookState::load` for books with many highlights (the
/// fields of a real EPUB highlight). Since cycle 1 each save clones the state, writes the
/// JSON and parses it back before writing; once a minute it also reads and parses the old
/// file for the `.old` backup.
#[test]
#[ignore = "measurement: sidecar save/load time with 1k, 5k and 18k highlights"]
fn verifier2_sidecar_save_time_with_many_highlights() {
    use crate::sidecar::{BookState, Highlight};
    let dir = crate::testutil::TempDir::new("v2-save-time");
    for n in [1_000usize, 5_000, 18_000] {
        let book = dir.path().join(format!("b{n}.epub"));
        let mut s = BookState { position: Some("/body/DocFragment[3]/body/div/p[2]/text().0".into()), ..Default::default() };
        for i in 0..n {
            let (frag, para) = (i / 40 + 1, i % 40 + 1);
            s.highlights.push(Highlight {
                start: format!("/body/DocFragment[{frag}]/body/div/p[{para}]/text().0"),
                end: format!("/body/DocFragment[{frag}]/body/div/p[{para}]/text().57"),
                text: "A highlighted sentence of ordinary length in the book.".into(),
                style: "lighten".into(),
                color: Some("yellow".into()),
                chapter: Some(format!("Chapter {frag}")),
                created: "2025-03-04 21:15:03".into(),
                ..Default::default()
            });
        }
        s.save(&book).unwrap();
        let mut times = Vec::new();
        for page in 0..5 {
            s.position = Some(format!("/body/DocFragment[3]/body/div/p[{page}]/text().0"));
            let t = Instant::now();
            s.save(&book).unwrap();
            times.push(t.elapsed().as_millis());
        }
        let t = Instant::now();
        let back = BookState::load(&book).unwrap();
        let load_ms = t.elapsed().as_millis();
        assert_eq!(back.highlights.len(), n);
        let size = std::fs::metadata(crate::sidecar::sidecar_path(&book)).unwrap().len();
        eprintln!("verifier2 sidecar {n} highlights: {} KiB, save ms {times:?}, load {load_ms} ms", size / 1024);
    }
}

