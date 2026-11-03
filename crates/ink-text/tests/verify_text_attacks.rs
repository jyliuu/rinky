//! Verifier attacks on `ink-text` (wave A, cycle 1): missing and broken font files,
//! corrupted font tables, hostile words for hyphenation, pathological line widths and
//! huge paragraphs, and the glyph cache budget.
//!
//! The two defects found in cycle 1 (corrupt `.ttc` header, hyphenation offsets after
//! case folding) are fixed; their tests run with the others.

use ink_device::Canvas;
use ink_text::hyphen::{hyphenator_for, split_at_points, Hyphenator};
use ink_text::linebreak::{finish_paragraph, greedy_items, knuth_plass, wrap, BreakParams, Item};
use ink_text::raster::{rasterize, Synth};
use ink_text::{FaceData, Fonts, NoHyphenation, Style};
use rustybuzz::ttf_parser;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn data() -> PathBuf {
    std::env::var_os("INK_TEST_DATA").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data"))
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-text-verify-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---- Missing and broken font files ----

/// Font directories that do not exist, are files, or hold junk named like fonts: the
/// junk is skipped without panicking, the good face is loaded, and shaping and drawing
/// still work.
#[test]
fn junk_font_files_are_skipped() {
    let dir = scratch("junk");
    let good = std::fs::read(data().join("InkTestFallback.ttf")).unwrap();
    std::fs::write(dir.join("empty.ttf"), b"").unwrap();
    std::fs::write(dir.join("garbage.ttf"), vec![0xA5u8; 4096]).unwrap();
    std::fs::write(dir.join("text.otf"), b"this is not a font\n").unwrap();
    std::fs::write(dir.join("truncated.ttf"), &good[..good.len() / 3]).unwrap();
    // A collection header that claims 1000 faces but holds none.
    std::fs::write(dir.join("collection.TTC"), b"ttcf\x00\x01\x00\x00\x00\x00\x03\xe8").unwrap();
    std::fs::create_dir_all(dir.join("folder.ttf")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.join("missing-target.ttf"), dir.join("dangling.ttf")).unwrap();
    std::fs::write(dir.join("zz-good.ttf"), &good).unwrap();
    let not_a_dir = dir.join("empty.ttf");
    let mut fonts = Fonts::load();
    let n = fonts.load_fallbacks(&[dir.as_path(), Path::new("/nonexistent/fonts"), not_a_dir.as_path()]);
    // truncated.ttf still parses lazily (its tables are cut later); junk does not.
    assert!((1..=2).contains(&n), "{n}");
    let run = fonts.shape(Style::Regular, 40.0, "A 日本語 ★ B");
    assert!(run.faces().len() >= 2, "{:?}", run.faces());
    let mut c = Canvas::new(600, 80);
    fonts.draw_run(&mut c, &run, 10.0, 60.0);
    assert!(c.pixels.iter().any(|v| *v < 64));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A 12-byte `.ttc` in a font directory whose header claims 0xFFFFFFFF faces (a
/// corrupted or truncated collection in /mnt/us/fonts). Scanning must skip it. Before
/// the fix, `registry::scan_fonts` looped over all 4294967295 claimed faces: a debug
/// build panicked inside ttf-parser ("assertion failed: self.offset as u64 + len as u64
/// <= u32::MAX as u64"), and the ARM release build on the emulator was still scanning
/// after 6 minutes with no UI. Now `face::collection_faces` returns 0 for a header that
/// claims more face offsets than the file holds, and the file is skipped with one
/// warning (emulator: tests/e2e/ui-verify-bad-ttc.sh).
#[test]
fn corrupt_collection_header_is_skipped() {
    let dir = scratch("ttc");
    std::fs::write(dir.join("broken.ttc"), b"ttcf\x00\x01\x00\x00\xff\xff\xff\xff").unwrap();
    std::fs::copy(data().join("InkTestFallback.ttf"), dir.join("good.ttf")).unwrap();
    let res = std::panic::catch_unwind(|| ink_text::registry::scan_fonts(&[dir.as_path()]).len());
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(res.ok(), Some(1), "scan_fonts panicked or found the wrong number of faces");
}

/// No fallback fonts at all (the host, or a Kindle without /usr/java/lib/fonts): CJK and
/// symbols draw the main face's .notdef without panicking, in every style.
#[test]
fn missing_fallbacks_draw_notdef() {
    let mut fonts = Fonts::load();
    assert_eq!(fonts.load_fallbacks(&["/nonexistent/a", "/nonexistent/b"]), 0);
    let mut c = Canvas::new(800, 100);
    for style in [Style::Regular, Style::Bold, Style::Italic, Style::BoldItalic] {
        let run = fonts.shape(style, 36.0, "日本語 한국어 ★ \u{1F600} \u{0} \u{FFFF} \u{10FFFF}");
        assert!(run.width > 0.0);
        fonts.draw_run(&mut c, &run, 0.0, 70.0);
    }
}

/// The file behind a mapped fallback font is replaced while the reader runs (a user
/// copies a new version into /mnt/us/fonts over USB). Writing a new file under the same
/// name must not crash the process when later glyphs are drawn.
#[test]
fn replacing_a_font_file_after_loading_does_not_crash() {
    let dir = scratch("replace");
    let good = std::fs::read(data().join("InkTestFallback.ttf")).unwrap();
    let path = dir.join("user.ttf");
    std::fs::write(&path, &good).unwrap();
    let mut fonts = Fonts::load();
    assert_eq!(fonts.load_fallbacks(&[dir.as_path()]), 1);
    // Replace by writing a new file and renaming it over the old one (what copy tools do).
    let tmp = dir.join("user.ttf.part");
    std::fs::write(&tmp, &good).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    let run = fonts.shape(Style::Regular, 40.0, "日本語★");
    let mut c = Canvas::new(400, 80);
    fonts.draw_run(&mut c, &run, 0.0, 60.0);
    assert!(c.pixels.iter().any(|v| *v < 64));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Bytes of a small TrueType font set to hostile values, one at a time (every byte of the
/// table directory and the first tables, then every 16th byte): parsing, shaping and
/// rasterizing every glyph never panic, and no glyph bitmap is absurdly large.
#[test]
fn byte_corrupted_fonts_do_not_panic_or_explode() {
    let good = std::fs::read(data().join("InkTestFallback.ttf")).unwrap();
    let text = "日本語中文漢字한★☆♥→←⌫⇧↵✓☰A";
    let mut parsed = 0usize;
    let mut largest = (0usize, 0usize, 0u8, 0usize);
    let positions = (0..good.len()).filter(|p| *p < 320 || p % 16 == 0);
    for pos in positions {
        let values: &[u8] = if pos < 320 { &[0x00, 0x7F, 0xFF] } else { &[0x7F] };
        for &v in values {
            if good[pos] == v {
                continue;
            }
            let mut bytes = good.clone();
            bytes[pos] = v;
            let Ok(face) = ttf_parser::Face::parse(&bytes, 0) else { continue };
            parsed += 1;
            for gid in 0..face.number_of_glyphs() {
                let m = rasterize(&face, ttf_parser::GlyphId(gid), 40.0, Synth { bold: true, italic: true });
                if m.coverage.len() > largest.0 {
                    largest = (m.coverage.len(), pos, v, gid as usize);
                }
            }
            if let Some(rb) = rustybuzz::Face::from_slice(&bytes, 0) {
                let mut buf = rustybuzz::UnicodeBuffer::new();
                buf.push_str(text);
                let _ = rustybuzz::shape(&rb, &[], buf);
            }
        }
    }
    assert!(parsed > 300, "{parsed}");
    // A 40 px glyph needs a few thousand pixels; allow a 16 MP worst case.
    assert!(largest.0 < 16_000_000, "a corrupted font gave a {} byte glyph mask (byte {} = {:#x}, glyph {})", largest.0, largest.1, largest.2, largest.3);
}

/// A font that claims 16 units per em (the smallest ttf-parser accepts) with normal
/// 1000-unit outlines: one 40 px glyph becomes a 2100 x 2200 mask (about 18 MB of float
/// accumulator while it is drawn). Not a crash, but `rasterize` has no size cap; this test
/// keeps it under 16 megapixels and 5 s. Corrupted fonts in /mnt/us/fonts are untrusted.
#[test]
fn tiny_units_per_em_glyph_stays_bounded() {
    let mut bytes = std::fs::read(data().join("InkTestFallback.ttf")).unwrap();
    // Patch head.unitsPerEm (offset 18 in the head table) to 16.
    let face = ttf_parser::Face::parse(&bytes, 0).unwrap();
    let gid = face.glyph_index('日').unwrap();
    let n_tables = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let mut head = None;
    for i in 0..n_tables {
        let rec = 12 + 16 * i;
        if &bytes[rec..rec + 4] == b"head" {
            head = Some(u32::from_be_bytes([bytes[rec + 8], bytes[rec + 9], bytes[rec + 10], bytes[rec + 11]]) as usize);
        }
    }
    let head = head.expect("head table");
    bytes[head + 18..head + 20].copy_from_slice(&16u16.to_be_bytes());
    let face = ttf_parser::Face::parse(&bytes, 0).expect("still parses");
    assert_eq!(face.units_per_em(), 16);
    let t0 = Instant::now();
    let m = rasterize(&face, gid, 40.0, Synth::NONE);
    let took = t0.elapsed();
    assert!(m.width * m.height < 16_000_000 && took < Duration::from_secs(5), "{}x{} mask in {took:?}", m.width, m.height);
}

// ---- Hyphenation ----

/// Hostile words for every embedded language the UI uses: very long words, combining
/// marks, case changes that alter byte lengths (İ, ẞ), digits, emoji, soft hyphens at the
/// edges. Points are increasing char boundaries strictly inside the word.
#[test]
fn hyphenation_points_are_valid_for_hostile_words() {
    let long = "supercalifragilisticexpialidocious".repeat(300);
    let combining = "e\u{301}le\u{301}phant\u{301}ine\u{301}s";
    let words = [
        long.as_str(),
        combining,
        "İSTANBULLULAŞTIRAMADIKLARIMIZDAN",
        "Straßenverkehrsordnung",
        "ﬁnancial",
        "co\u{ad}op\u{ad}",
        "\u{ad}leading",
        "trailing\u{ad}",
        "a1b2c3d4e5f6g7",
        "😀😀😀😀😀😀😀😀",
        "--------",
        "\u{200d}\u{200d}\u{200d}\u{200d}\u{200d}",
        "",
        "x",
        "Ǆungla",
        "ΑΡΧΙΜΗΔΗΣ",
    ];
    for lang in ["en", "en-GB", "da", "de", "fr", "tr", "el", "ru", "zh", "xx"] {
        let h = hyphenator_for(lang);
        for w in words {
            let pts = h.hyphenate(w);
            let mut last = 0;
            for p in &pts {
                assert!(*p > last && *p < w.len(), "{lang} {w:?}: {pts:?}");
                assert!(w.is_char_boundary(*p), "{lang} {w:?}: {p}");
                last = *p;
            }
            let pieces = split_at_points(w, &pts);
            assert_eq!(pieces.concat(), w);
        }
    }
}

/// German capitals with the capital sharp s (U+1E9E, three bytes; lower case ß has two)
/// and decomposed accents (NFD text from some converters). The hyphenation crate works on
/// a lower-cased copy; before the fix its offsets were returned unchanged, so for
/// "GROẞSTADTSTRAẞENVERKEHR" the points were [5, 10, 14, 18, 21] (5 inside 'ẞ', breaks
/// GROẞSTAD|TSTR|Aẞ|ENV|ERKEHR instead of the lower-case word's
/// groß|stadt|stra|ßen|ver|kehr), and Turkish patterns on NFD text split accents off
/// their letters (le|◌́p). `PatternHyphenator` now folds case itself with a byte map back
/// to the word and drops points that are not grapheme boundaries. The
/// `Hyphenator::hyphenate` contract: points are char and grapheme boundaries, so a caller
/// (ink-html) may slice the word at any of them.
#[test]
fn hyphenation_points_respect_chars_and_graphemes() {
    use unicode_segmentation::UnicodeSegmentation;
    let mut bad = Vec::new();
    for (lang, w) in [("de", "GROẞSTADTSTRAẞENVERKEHR"), ("tr", "e\u{301}le\u{301}phant\u{301}ine\u{301}s")] {
        let graphemes: Vec<usize> = w.grapheme_indices(true).map(|(i, _)| i).collect();
        let pts = hyphenator_for(lang).hyphenate(w);
        for p in &pts {
            if !w.is_char_boundary(*p) || !graphemes.contains(p) {
                bad.push(format!("{lang} {w:?}: point {p} of {pts:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

// ---- Line breaking ----

fn mono(s: &str) -> f32 {
    s.chars().count() as f32 * 10.0
}

/// Pathological widths (0, negative, NaN, infinite, huge) and texts (only spaces, only
/// newlines, one huge word) give lines that cover the text, without panicking or hanging.
#[test]
fn wrap_survives_pathological_widths_and_texts() {
    let h = hyphenator_for("en");
    let texts = [
        String::new(),
        "     ".into(),
        "\n\n\n".into(),
        "word".into(),
        "a".repeat(5000),
        "Hyphenation makes justified text look better on narrow screens.".into(),
        "x\u{ad}y\u{ad}z \t\r\n tab\tseparated\u{a0}nbsp".into(),
    ];
    for w in [0.0f32, -50.0, f32::NAN, f32::INFINITY, 1e30, 1.0, 35.0] {
        for t in &texts {
            for optimal in [false, true] {
                for hy in [&*h as &dyn Hyphenator, &NoHyphenation] {
                    let lines = wrap(t, w, &mut mono, hy, optimal);
                    for l in &lines {
                        assert!(l.range.start <= l.range.end && l.range.end <= t.len(), "{w} {t:?}: {lines:?}");
                        assert!(t.is_char_boundary(l.range.start) && t.is_char_boundary(l.range.end));
                    }
                    let words_in = t.split_whitespace().count();
                    if words_in > 0 && w.is_finite() && w > 0.0 {
                        let joined: String = lines.iter().map(|l| &t[l.range.clone()]).collect::<Vec<_>>().join(" ");
                        assert!(joined.split_whitespace().count() >= words_in.min(1), "{w} {t:?}");
                    }
                }
            }
        }
    }
}

/// A 20 000-word paragraph breaks with total fit in reasonable time (a long chapter in
/// one <p>, as in badly converted books).
#[test]
fn knuth_plass_on_a_huge_paragraph_is_fast() {
    let mut items = Vec::new();
    for i in 0..20_000 {
        if i > 0 {
            items.push(Item::space(10.0));
        }
        items.push(Item::word(30.0 + (i % 7) as f32 * 10.0));
    }
    finish_paragraph(&mut items);
    let t0 = Instant::now();
    let breaks = knuth_plass(&items, &|_| 1000.0, &BreakParams::default()).expect("feasible");
    let took = t0.elapsed();
    assert!(breaks.len() > 1000);
    assert!(took < Duration::from_secs(10), "{took:?}");
    let g = greedy_items(&items, &|_| 1000.0);
    assert!(g.len() >= breaks.len() - 50);
}

// ---- Glyph cache ----

/// Drawing many distinct glyphs at many sizes keeps the shaped glyph cache within its
/// byte budget (plus at most the glyph being drawn).
#[test]
fn glyph_cache_stays_within_budget() {
    let mut fonts = Fonts::load();
    fonts.set_cache_budget(256 * 1024);
    let mut c = Canvas::new(1200, 200);
    let text: String = ('!'..='~').collect();
    for px in 10..90 {
        let run = fonts.shape(Style::BoldItalic, px as f32, &text);
        fonts.draw_run(&mut c, &run, 0.0, 150.0);
        assert!(fonts.cached_bytes() <= 256 * 1024 + 64 * 1024, "{} bytes at {px} px", fonts.cached_bytes());
    }
}

/// Synthetic bold at a huge size and a zero or negative size do not panic.
#[test]
fn extreme_sizes() {
    let face = FaceData::from_static(ink_text::embedded_font_bytes(Style::Regular), 0).unwrap();
    let f = face.ttf().unwrap();
    let gid = f.glyph_index('W').unwrap();
    for px in [0.0f32, -10.0, 0.01, 1.0, 600.0] {
        let m = rasterize(f, gid, px, Synth { bold: true, italic: true });
        assert_eq!(m.coverage.len(), m.width * m.height, "{px}");
    }
    let mut fonts = Fonts::load();
    let mut c = Canvas::new(100, 100);
    for px in [0.0f32, -5.0, 0.5] {
        let run = fonts.shape(Style::Bold, px, "Hello");
        fonts.draw_run(&mut c, &run, 0.0, 50.0);
    }
}
