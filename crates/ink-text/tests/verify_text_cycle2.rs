//! Verifier attacks on `ink-text` (wave A, cycle 2), aimed at the cycle-2 changes: the
//! hyphenation byte map through case folding and the grapheme filter, soft hyphens,
//! `face::collection_faces` with real multi-face collections, and clipped run drawing.

use ink_device::{Canvas, Rect};
use ink_text::face::{collection_faces, MAX_COLLECTION_FACES};
use ink_text::hyphen::{hyphenator_for, soft_hyphens};
use ink_text::{FaceData, Fonts, Style};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

fn data() -> PathBuf {
    std::env::var_os("INK_TEST_DATA").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data"))
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-text-verify2-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The trait contract from `hyphen.rs`: increasing, strictly inside, char and extended
/// grapheme boundaries.
fn check_points(lang: &str, w: &str, pts: &[usize]) -> Result<(), String> {
    let g: Vec<usize> = w.grapheme_indices(true).map(|(i, _)| i).collect();
    let mut last = 0;
    for p in pts {
        if *p <= last || *p >= w.len() || !w.is_char_boundary(*p) || g.binary_search(p).is_err() {
            return Err(format!("{lang} {w:?}: bad point {p} in {pts:?}"));
        }
        last = *p;
    }
    Ok(())
}

// ---- Hyphenation ----

/// Soft hyphens next to combining marks, ZWJ and other format characters: every point is
/// still a grapheme boundary (U+00AD is a grapheme Control, so a break follows it).
#[test]
fn soft_hyphen_points_keep_the_contract() {
    let words = [
        "co\u{ad}\u{301}operation",
        "e\u{301}\u{ad}le\u{301}\u{ad}phant",
        "a\u{ad}\u{200d}b\u{ad}\u{200c}c\u{ad}d",
        "👨\u{ad}\u{200d}👩",
        "\u{ad}\u{ad}\u{ad}",
        "x\u{ad}",
        "Stra\u{ad}ẞen\u{ad}bahn",
    ];
    for w in words {
        check_points("shy", w, &soft_hyphens(w)).unwrap();
        for lang in ["en", "de", "da", "fr"] {
            check_points(lang, w, &hyphenator_for(lang).hyphenate(w)).unwrap();
        }
    }
}

/// A small deterministic generator (no extra dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[(self.next() as usize) % xs.len()]
    }
}

/// The lower case `PatternHyphenator` looks up (char by char; 'İ' becomes 'i').
fn simple_lower(w: &str) -> String {
    w.chars().map(|c| if c == 'İ' { "i".to_string() } else { c.to_lowercase().collect() }).collect()
}

/// 3000 random words per language from letters whose lower case has another byte length
/// (ẞ, Kelvin K, Ohm Ω, Angstrom Å, Ⱥ, Ⱦ, İ), plain letters, accents (precomposed and
/// combining), apostrophes and hyphens. Every point keeps the contract, and the points
/// of a word fall at the same letters as the points of its lower case.
#[test]
fn random_words_keep_the_contract_and_ignore_case() {
    let pool: &[&str] = &[
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "k", "l", "m", "n", "o", "p", "r", "s", "t", "u", "v", "z", "A", "E", "I",
        "N", "O", "R", "S", "T", "ẞ", "ß", "\u{212a}", "\u{2126}", "\u{212b}", "Ⱥ", "Ⱦ", "İ", "ı", "æ", "ø", "å", "Æ", "Ø", "Å",
        "ä", "ö", "ü", "Ä", "Ö", "Ü", "é", "è", "ç", "É", "e\u{301}", "o\u{308}", "'", "’", "-",
    ];
    let mut rng = Lcg(0x1234_5678);
    let mut bad = Vec::new();
    let mut points = 0;
    for lang in ["en", "de", "da", "fr", "tr", "nb", "sv"] {
        let h = hyphenator_for(lang);
        for _ in 0..3000 {
            let n = 4 + (rng.next() % 18) as usize;
            let w: String = (0..n).map(|_| *rng.pick(pool)).collect();
            let pts = h.hyphenate(&w);
            points += pts.len();
            if let Err(e) = check_points(lang, &w, &pts) {
                bad.push(e);
                continue;
            }
            // Same letters as the lower-case word (both measured in chars).
            let lw = simple_lower(&w);
            if lw.chars().count() == w.chars().count() {
                let chars_at = |s: &str, ps: &[usize]| ps.iter().map(|p| s[..*p].chars().count()).collect::<Vec<_>>();
                let (a, b) = (chars_at(&w, &pts), chars_at(&lw, &h.hyphenate(&lw)));
                if a != b {
                    bad.push(format!("{lang} {w:?}: points at chars {a:?}, lower case {lw:?} at {b:?}"));
                }
            }
        }
    }
    assert!(points > 5000, "the words were hyphenated at all: {points} points");
    assert!(bad.is_empty(), "{} bad of 21000:\n{}", bad.len(), bad[..bad.len().min(20)].join("\n"));
}

// ---- Font collections ----

/// A `.ttc` holding `copies` copies of `font`, each with its own table directory
/// (table offsets rewritten); the header claims `claimed` faces and lists `offsets`
/// extra bogus offsets after the real ones.
fn make_ttc(font: &[u8], copies: usize, claimed: u32, bogus: &[u32]) -> Vec<u8> {
    let entries = copies + bogus.len();
    let header = 12 + 4 * entries;
    let base0 = header.div_ceil(4) * 4;
    let stride = font.len().div_ceil(4) * 4;
    let mut out = b"ttcf\x00\x01\x00\x00".to_vec();
    out.extend(claimed.to_be_bytes());
    for i in 0..copies {
        out.extend(((base0 + i * stride) as u32).to_be_bytes());
    }
    for b in bogus {
        out.extend(b.to_be_bytes());
    }
    out.resize(base0, 0);
    let tables = u16::from_be_bytes([font[4], font[5]]) as usize;
    for i in 0..copies {
        let base = base0 + i * stride;
        let mut f = font.to_vec();
        for t in 0..tables {
            let at = 12 + 16 * t + 8;
            let off = u32::from_be_bytes(f[at..at + 4].try_into().unwrap());
            f[at..at + 4].copy_from_slice(&(off + base as u32).to_be_bytes());
        }
        out.extend(&f);
        out.resize(base + stride, 0);
    }
    out
}

/// Real collections: two faces are found and load; a header claiming a third face whose
/// offset points past the end gives the two good faces; a collection truncated at every
/// length never panics in scanning, opening or shaping. The daily run cuts at every 97th
/// length (and the header lengths below 128); the ignored run at every 7th.
#[test]
fn real_collections_and_every_truncation() {
    real_collections(97);
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn real_collections_and_every_7th_truncation() {
    real_collections(7);
}

fn real_collections(step: usize) {
    let font = std::fs::read(data().join("InkTestFallback.ttf")).unwrap();
    let dir = scratch(&format!("ttc-{step}"));
    let two = make_ttc(&font, 2, 2, &[]);
    assert_eq!(collection_faces(&two), 2);
    std::fs::write(dir.join("two.ttc"), &two).unwrap();
    assert_eq!(FaceData::open_all(&dir.join("two.ttc")).unwrap().len(), 2);
    assert_eq!(ink_text::registry::scan_fonts(&[dir.as_path()]).len(), 2);
    // A third offset past the end of the file.
    let three = make_ttc(&font, 2, 3, &[0x7fff_fff0]);
    assert_eq!(collection_faces(&three), 3);
    let d3 = scratch(&format!("ttc3-{step}"));
    std::fs::write(d3.join("three.ttc"), &three).unwrap();
    assert_eq!(FaceData::open_all(&d3.join("three.ttc")).unwrap().len(), 2, "the bad face is skipped, the good ones stay");
    assert_eq!(ink_text::registry::scan_fonts(&[d3.as_path()]).len(), 2);
    // Every truncation (each in its own file: files are mapped once per path).
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut panics = Vec::new();
    let dt = scratch(&format!("ttc-cut-{step}"));
    let mut lens: Vec<usize> = (0..128.min(two.len())).chain((0..two.len()).step_by(step)).chain([two.len() - 1]).collect();
    lens.sort_unstable();
    lens.dedup();
    for len in lens {
        let p = dt.join(format!("cut-{len:05}.ttc"));
        std::fs::write(&p, &two[..len]).unwrap();
        let r = std::panic::catch_unwind(|| {
            let _ = collection_faces(&two[..len]);
            let _ = FaceData::open_all(&p);
        });
        if r.is_err() {
            panics.push(len);
        }
    }
    let scan = std::panic::catch_unwind(|| ink_text::registry::scan_fonts(&[dt.as_path()]).len());
    let shaped = std::panic::catch_unwind(|| {
        let mut fonts = Fonts::load();
        fonts.load_fallbacks(&[dt.as_path()]);
        let run = fonts.shape(Style::Regular, 40.0, "A 日本語 ★ B ∑");
        let mut c = Canvas::new(400, 80);
        fonts.draw_run(&mut c, &run, 5.0, 60.0);
    });
    std::panic::set_hook(quiet);
    for d in [&dir, &d3, &dt] {
        let _ = std::fs::remove_dir_all(d);
    }
    assert!(panics.is_empty(), "open_all panicked for truncations at {panics:?}");
    assert!(scan.is_ok(), "scan_fonts panicked on truncated collections");
    assert!(shaped.is_ok(), "loading or shaping with truncated collections panicked");
}

/// A collection whose header claims 100000 faces and holds that many offsets (all
/// pointing at one font) is capped at MAX_COLLECTION_FACES and scanned quickly.
#[test]
fn huge_valid_collection_is_capped() {
    let font = std::fs::read(data().join("InkTestFallback.ttf")).unwrap();
    let one = make_ttc(&font, 1, 1, &[]);
    let base = u32::from_be_bytes(one[12..16].try_into().unwrap());
    // Same font data after a header with 100000 offsets, all pointing at it.
    let n = 100_000usize;
    let header = 12 + 4 * n;
    let shift = (header.div_ceil(4) * 4) as u32 - base;
    let mut ttc = b"ttcf\x00\x01\x00\x00".to_vec();
    ttc.extend((n as u32).to_be_bytes());
    for _ in 0..n {
        ttc.extend((base + shift).to_be_bytes());
    }
    ttc.resize((base + shift) as usize, 0);
    let tables = u16::from_be_bytes([font[4], font[5]]) as usize;
    let mut f = font.clone();
    for t in 0..tables {
        let at = 12 + 16 * t + 8;
        let off = u32::from_be_bytes(f[at..at + 4].try_into().unwrap());
        f[at..at + 4].copy_from_slice(&(off + base + shift).to_be_bytes());
    }
    ttc.extend(&f);
    assert_eq!(collection_faces(&ttc), MAX_COLLECTION_FACES);
    let dir = scratch("ttc-huge");
    std::fs::write(dir.join("huge.ttc"), &ttc).unwrap();
    let t0 = Instant::now();
    let found = ink_text::registry::scan_fonts(&[dir.as_path()]).len();
    let took = t0.elapsed();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(found, MAX_COLLECTION_FACES as usize);
    assert!(took < Duration::from_secs(30), "{took:?}");
}

// ---- Clipped runs ----

/// `draw_run_clipped` with clips partly or wholly off the canvas, empty clips and large
/// (but finite) positions: no panic, and nothing outside `clip ∩ canvas` changes.
#[test]
fn clipped_runs_with_hostile_clips_and_positions() {
    let mut f = Fonts::load();
    let run = f.shape(Style::BoldItalic, 60.0, "Wj ÅÉ fi 日本 ★");
    let clips = [
        Rect::new(-100, -100, 150, 150),
        Rect::new(350, 50, 500, 500),
        Rect::new(0, 0, 0, 0),
        Rect::new(10, 10, 1, 1),
        Rect::new(-1_000_000, -1_000_000, 2_000_000, 2_000_000),
        Rect::new(400, 100, 10, 10),
    ];
    for clip in clips {
        for (x, base) in [(5.0f32, 60.0f32), (-30.0, 20.0), (380.0, 98.0), (1e9, 1e9), (-1e9, -1e9), (f32::NAN, f32::NAN)] {
            let mut c = Canvas::new(400, 100);
            c.clear(200);
            f.draw_run_clipped(&mut c, &run, x, base, 0, clip);
            let inside = clip.intersect(&c.bounds());
            let outside = (0..100)
                .flat_map(|y| (0..400).map(move |x| (x, y)))
                .filter(|(px, py)| !inside.contains(ink_device::Point::new(*px, *py)) && c.get(*px, *py) != Some(200))
                .count();
            assert_eq!(outside, 0, "clip {clip:?} at ({x}, {base})");
        }
    }
}
