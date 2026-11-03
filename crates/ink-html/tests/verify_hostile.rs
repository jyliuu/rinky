//! Verifier attacks (cycle 1) on the phase 2 engine: hostile markup and CSS, huge
//! inputs, images, positions, relayout, the page-map cache and the background
//! paginator.
//!
//! Heavy or crash-prone cases run in a child process: this test binary runs itself
//! again with `INK_VERIFY_CHILD=<case>` (see [`child_runner`]), so a stack overflow,
//! an abort or a hang shows up as one failed test instead of killing the whole run.
//! Children print `RESULT key=value` lines (time, peak heap) that the parent reads.
//! Attacks that fail are kept with `#[ignore = "verifier: ..."]`.

mod common;

use ink_device::{Canvas, Point};
use ink_doc::{Document, LayoutSettings, MemoryLoader, Metadata, Position, XPointer};
use ink_html::layout::{Draw, Params};
use ink_html::{HtmlChapter, HtmlDocument};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------
// Heap counter: current and peak bytes (for the memory cases, run in children).

struct Counting;
static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn track_add(n: usize) {
    let now = CUR.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

fn track_sub(n: usize) {
    CUR.fetch_sub(n, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            track_add(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            track_add(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        track_sub(l.size());
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                track_add(new - l.size());
            } else {
                track_sub(l.size() - new);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Resets the peak to the current heap size and returns that size.
fn heap_mark() -> usize {
    let c = CUR.load(Ordering::Relaxed);
    PEAK.store(c, Ordering::Relaxed);
    c
}

/// Peak heap growth in MB since `mark`.
fn peak_mb_since(mark: usize) -> f64 {
    PEAK.load(Ordering::Relaxed).saturating_sub(mark) as f64 / (1024.0 * 1024.0)
}

// ---------------------------------------------------------------------------------
// Child processes.

const CHILD_ENV: &str = "INK_VERIFY_CHILD";

struct Outcome {
    /// Exit status 0.
    ok: bool,
    timed_out: bool,
    secs: f64,
    out: String,
    results: HashMap<String, String>,
}

impl Outcome {
    fn num(&self, k: &str) -> f64 {
        self.results.get(k).and_then(|v| v.parse().ok()).unwrap_or(f64::NAN)
    }
    fn tail(&self) -> String {
        let lines: Vec<&str> = self.out.lines().collect();
        lines[lines.len().saturating_sub(25)..].join("\n")
    }
}

fn child(case: &str, timeout: Duration) -> Outcome {
    let exe = std::env::current_exe().unwrap();
    let log = std::env::temp_dir().join(format!("ink-verify-{}-{case}.log", std::process::id()));
    let f = std::fs::File::create(&log).unwrap();
    let f2 = f.try_clone().unwrap();
    let t = Instant::now();
    let mut c = std::process::Command::new(exe)
        .args(["child_runner", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, case)
        .stdout(f)
        .stderr(f2)
        .spawn()
        .unwrap();
    let mut timed_out = false;
    let status = loop {
        if let Some(st) = c.try_wait().unwrap() {
            break Some(st);
        }
        if t.elapsed() > timeout {
            let _ = c.kill();
            let _ = c.wait();
            timed_out = true;
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let secs = t.elapsed().as_secs_f64();
    let out = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    let mut results = HashMap::new();
    for l in out.lines() {
        if let Some(rest) = l.strip_prefix("RESULT ") {
            if let Some((k, v)) = rest.split_once('=') {
                results.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    let ok = status.is_some_and(|s| s.success()) && out.contains("CASE DONE");
    eprintln!("child {case}: ok={ok} timed_out={timed_out} {secs:.1}s {results:?}");
    Outcome { ok, timed_out, secs, out, results }
}

fn result(k: &str, v: impl std::fmt::Display) {
    // Leading newline: the test harness prints "test child_runner ... " without one.
    println!("\nRESULT {k}={v}");
    let _ = std::io::stdout().flush();
}

/// Runs `f` on a thread with an 8 MB stack (the size of a Linux main thread).
fn on_8mb_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().stack_size(8 << 20).spawn(f).unwrap().join().unwrap();
}

#[test]
fn child_runner() {
    let Ok(case) = std::env::var(CHILD_ENV) else { return };
    match case.as_str() {
        "nest_xml_100k" => on_8mb_stack(|| {
            let n = 100_000;
            let html = format!("<html><body>{}deep text{}</body></html>", "<div>".repeat(n), "</div>".repeat(n));
            open_render_all(html, 50);
        }),
        "nest_soup_100k" => on_8mb_stack(|| {
            let html = format!("<body>{}soup text", "<div><b><i>".repeat(34_000));
            open_render_all(html, 50);
        }),
        "nest_inline_100k" => on_8mb_stack(|| {
            let n = 100_000;
            let html = format!("<html><body><p>{}inline text{}</p></body></html>", "<span>".repeat(n), "</span>".repeat(n));
            open_render_all(html, 50);
        }),
        "css_media_nest_50k" => on_8mb_stack(|| {
            let n = 50_000;
            let css = format!("{}p {{ color: red }}{}", "@media screen { ".repeat(n), "}".repeat(n));
            open_render_all(chapter_with_style(&css, "<p>media text</p>"), 5);
        }),
        "css_not_nest_50k" => on_8mb_stack(|| {
            let n = 50_000;
            let css = format!("p{}.x{} {{ color: red }}", ":not(".repeat(n), ")".repeat(n));
            open_render_all(chapter_with_style(&css, "<p>not text</p>"), 5);
        }),
        "css_selector_chain_100k" => on_8mb_stack(|| {
            let n = 100_000;
            let css = format!("p{} {{ color: red }}", " > p".repeat(n));
            open_render_all(chapter_with_style(&css, "<p>chain text</p>"), 5);
        }),
        "css_descendant_backtracking" => on_8mb_stack(|| {
            // 30 nested divs; a 10-part descendant selector whose leftmost part never
            // matches makes a naive matcher try every combination of ancestors.
            let css = format!(".nomatch{} {{ color: red }}", " div".repeat(9));
            let body = format!("{}<p>backtrack text</p>{}", "<div>".repeat(30), "</div>".repeat(30));
            let t = Instant::now();
            open_render_all(chapter_with_style(&css, &body), 5);
            result("ms", t.elapsed().as_millis());
        }),
        "css_many_rules_20k" => on_8mb_stack(|| {
            let mut css = String::new();
            for i in 0..20_000 {
                css.push_str(&format!("div.c{i} p.k{i} {{ margin-left: {}px; font-size: {}% }}\n", i % 50, 80 + i % 60));
            }
            let body: String = (0..2000).map(|i| format!("<div class=\"c{i}\"><p class=\"k{i}\">para {i} with some words</p></div>\n")).collect();
            let t = Instant::now();
            open_render_all(chapter_with_style(&css, &body), 3);
            result("ms", t.elapsed().as_millis());
        }),
        "css_huge_lengths" => on_8mb_stack(|| {
            let css = "\
                p.a { margin-top: 1e39px; margin-bottom: 1e39px }\n\
                p.b { padding: 1e39px; border: 1e39px solid black }\n\
                p.c { line-height: 1e39; text-indent: -1e39px }\n\
                p.d { width: 1e39px; height: 1e39px; border: 3px solid }\n\
                div.e { margin-left: -1e39px; margin-right: -1e39px; border: 2px solid }\n\
                p.f { font-size: 1e39px; letter-spacing: 1e39px; word-spacing: -1e39px }\n\
                p.g { margin-left: 1e39px; text-indent: 1e39px }\n\
                p.h { line-height: 0; font-size: 0 }\n\
                img.i { width: 1e39px; height: 1e39px }\n\
                p.j { margin-top: -1e39px; padding-left: -50px }\n";
            let body = "<p>plain before</p><p class=\"a\">margins</p><p class=\"b\">padding and border</p>\
                <p class=\"c\">line height</p><p class=\"d\">box</p><div class=\"e\"><p>negative</p></div>\
                <p class=\"f\">font size</p><p class=\"g\">far right</p><p class=\"h\">zero</p>\
                <p><img class=\"i\" src=\"pic.png\" alt=\"\"/></p><p class=\"j\">negative top</p><p>plain after</p>";
            let mut res = MemoryLoader::default();
            res.0.insert("pic.png".into(), png_gray(40, 30));
            let mut d = doc_with(vec![("a.xhtml", chapter_with_style(css, body))], res);
            d.set_layout(&LayoutSettings::for_screen(600, 800)).unwrap();
            assert!(d.wait_paginated(Duration::from_secs(120)));
            let n = d.page_count().pages;
            result("pages", n);
            assert!(n < 1000, "{n} pages");
            let mut c = Canvas::new(600, 800);
            for p in 0..n {
                d.render_page(p, &mut c).unwrap();
                let pos = d.position_of_page(p).unwrap();
                d.page_of(&pos).unwrap();
            }
            let all = all_text(&d);
            for w in ["before", "after", "negative", "zero"] {
                assert!(all.contains(w), "{w} missing: {all}");
            }
        }),
        "para_5mb_words" => on_8mb_stack(|| {
            let words = ["lorem", "ipsum", "dolor", "sit", "amet,", "consectetur", "adipiscing", "elit."];
            let mut text = String::new();
            let mut i = 0;
            while text.len() < 5 << 20 {
                text.push_str(words[i % words.len()]);
                text.push(' ');
                i += 1;
            }
            let html = format!("<html><body><p>{text}</p></body></html>");
            drop(text);
            measure_open(html);
        }),
        "word_unbroken_256k" => on_8mb_stack(|| measure_open(format!("<html><body><p>{}</p></body></html>", "a".repeat(256 << 10)))),
        "word_unbroken_1m" => on_8mb_stack(|| measure_open(format!("<html><body><p>{}</p></body></html>", "a".repeat(1 << 20)))),
        "spine_5000" => on_8mb_stack(|| spine_case(5000)),
        "spine_1250" => on_8mb_stack(|| spine_case(1250)),
        "image_10k_gray" => on_8mb_stack(|| big_image_case(false)),
        "image_10k_rgb" => on_8mb_stack(|| big_image_case(true)),
        "svg_use_bomb" => on_8mb_stack(|| {
            // 9 levels of 10 <use> each: 10^9 rectangles when expanded naively.
            let mut svg = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"100\" height=\"100\"><defs><rect id=\"l0\" width=\"1\" height=\"1\"/>");
            for l in 1..10 {
                svg.push_str(&format!("<g id=\"l{l}\">"));
                for _ in 0..10 {
                    svg.push_str(&format!("<use xlink:href=\"#l{}\"/>", l - 1));
                }
                svg.push_str("</g>");
            }
            svg.push_str("</defs><use xlink:href=\"#l9\"/></svg>");
            let t = Instant::now();
            let _ = ink_html::images::intrinsic_size(svg.as_bytes());
            let _ = ink_html::images::decode_scaled(svg.as_bytes(), 100, 100, None);
            result("ms", t.elapsed().as_millis());
        }),
        "nav_nest_100k" => on_8mb_stack(|| {
            let n = 100_000;
            let navx = format!(
                "<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\"><body><nav epub:type=\"toc\">{}{}</nav></body></html>",
                "<ol><li><a href=\"a.xhtml\">x</a>".repeat(n),
                "</li></ol>".repeat(n)
            );
            let nav = ink_html::nav::parse_nav(&navx, "OPS/nav.xhtml");
            result("entries", nav.toc.len());
        }),
        other if other.starts_with("probe:") => {
            // probe:<media|not|div>:<depth>:<stack MB> (used to find crash thresholds).
            let parts: Vec<&str> = other.split(':').collect();
            let (kind, n, mb): (String, usize, usize) = (parts[1].into(), parts[2].parse().unwrap(), parts[3].parse().unwrap());
            std::thread::Builder::new()
                .stack_size(mb << 20)
                .spawn(move || {
                    let html = match kind.as_str() {
                        "media" => chapter_with_style(&format!("{}p {{ color: red }}{}", "@media screen { ".repeat(n), "}".repeat(n)), "<p>t</p>"),
                        "not" => chapter_with_style(&format!("p{}.x{} {{ color: red }}", ":not(".repeat(n), ")".repeat(n)), "<p>t</p>"),
                        _ => format!("<html><body>{}t{}</body></html>", "<div>".repeat(n), "</div>".repeat(n)),
                    };
                    open_render_all(html, 2);
                })
                .unwrap()
                .join()
                .unwrap();
        }
        other => panic!("unknown case {other}"),
    }
    println!("CASE DONE");
}

// ---------------------------------------------------------------------------------
// Helpers.

fn doc_with(chapters: Vec<(&str, String)>, res: MemoryLoader) -> HtmlDocument {
    let chapters = chapters.into_iter().map(|(p, h)| HtmlChapter { path: p.into(), html: h }).collect();
    HtmlDocument::new(Metadata::default(), chapters, Box::new(res)).unwrap()
}

fn chapter_with_style(css: &str, body: &str) -> String {
    format!("<?xml version=\"1.0\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><head><style>{css}</style></head><body>{body}</body></html>")
}

/// Text of a page from its layout (runs joined with spaces).
fn page_text(d: &HtmlDocument, page: usize) -> String {
    let (l, span) = d.page_layout(page).unwrap();
    let a = l.slices[span.first as usize].first_item as usize;
    let b = l.slices[span.end as usize - 1].end_item as usize;
    l.items[a..b]
        .iter()
        .filter_map(|i| match i {
            Draw::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn all_text(d: &HtmlDocument) -> String {
    (0..d.page_count().pages).map(|p| page_text(d, p)).collect::<Vec<_>>().join(" ")
}

/// Opens one chapter, lays it out, renders up to `max_pages` pages and checks that
/// positions round-trip.
fn open_render_all(html: String, max_pages: usize) {
    let mut d = doc_with(vec![("a.xhtml", html)], MemoryLoader::default());
    d.set_layout(&LayoutSettings::for_screen(600, 800)).unwrap();
    assert!(d.wait_paginated(Duration::from_secs(600)));
    let n = d.page_count().pages;
    result("pages", n);
    let mut c = Canvas::new(600, 800);
    for p in 0..n.min(max_pages) {
        d.render_page(p, &mut c).unwrap();
        let pos = d.position_of_page(p).unwrap();
        let s = pos.to_string();
        let back: Position = s.parse().unwrap();
        assert_eq!(d.page_of(&back).unwrap(), p, "{s}");
    }
    let _ = d.search("text", 5).unwrap();
}

/// Opens a one-chapter book and reports time to the first page and the peak heap.
fn measure_open(html: String) {
    let mark = heap_mark();
    let t = Instant::now();
    let mut d = doc_with(vec![("a.xhtml", html)], MemoryLoader::default());
    d.set_layout(&LayoutSettings::for_screen(1272, 1696)).unwrap();
    let mut c = Canvas::new(1272, 1696);
    d.render_page(0, &mut c).unwrap();
    result("first_page_ms", t.elapsed().as_millis());
    let n = d.page_count().pages;
    result("pages", n);
    let last = n - 1;
    d.render_page(last, &mut c).unwrap();
    let pos = d.position_of_page(last).unwrap();
    assert_eq!(d.page_of(&pos).unwrap(), last);
    result("total_ms", t.elapsed().as_millis());
    result("peak_mb", format!("{:.1}", peak_mb_since(mark)));
}

fn spine_case(n: usize) {
    let t = Instant::now();
    let chapters: Vec<(String, String)> =
        (0..n).map(|i| (format!("c{i}.xhtml"), format!("<html><body><h1>Chapter {i}</h1><p>Text of chapter {i}, short.</p></body></html>"))).collect();
    let chapters = chapters.iter().map(|(p, h)| (p.as_str(), h.clone())).collect();
    let mut d = doc_with(chapters, MemoryLoader::default());
    d.set_layout(&LayoutSettings::for_screen(1272, 1696)).unwrap();
    result("first_page_ms", t.elapsed().as_millis());
    assert!(d.wait_paginated(Duration::from_secs(600)));
    let pages = d.page_count();
    result("paginated_ms", t.elapsed().as_millis());
    assert!(pages.complete && pages.pages >= n, "{pages:?}");
    let t2 = Instant::now();
    for p in (0..pages.pages).step_by(pages.pages / 50) {
        let pos = d.position_of_page(p).unwrap();
        assert_eq!(d.page_of(&pos).unwrap(), p);
    }
    let toc = d.toc();
    assert_eq!(toc.len(), n);
    assert!(toc.iter().all(|e| e.page.is_some()));
    let hits = d.search("chapter", 10_000).unwrap();
    assert!(hits.len() >= n);
    d.set_current_page(pages.pages - 1);
    let mut c = Canvas::new(1272, 1696);
    d.render_page(pages.pages - 1, &mut c).unwrap();
    result("queries_ms", t2.elapsed().as_millis());
}

fn png_bytes(w: u32, h: u32, color: png::ColorType, row: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(color);
        e.set_depth(png::BitDepth::Eight);
        e.set_compression(png::Compression::Fast);
        let mut wr = e.write_header().unwrap();
        let mut sw = wr.stream_writer().unwrap();
        for _ in 0..h {
            sw.write_all(row).unwrap();
        }
        sw.finish().unwrap();
    }
    out
}

fn png_gray(w: u32, h: u32) -> Vec<u8> {
    let row: Vec<u8> = (0..w).map(|x| (x * 255 / w.max(1)) as u8).collect();
    png_bytes(w, h, png::ColorType::Grayscale, &row)
}

fn big_image_case(rgb: bool) {
    let (w, h) = (10_000u32, 10_000u32);
    let bytes = if rgb {
        let row: Vec<u8> = (0..w * 3).map(|i| (i % 251) as u8).collect();
        png_bytes(w, h, png::ColorType::Rgb, &row)
    } else {
        png_gray(w, h)
    };
    result("png_kb", bytes.len() / 1024);
    let mut res = MemoryLoader::default();
    res.0.insert("big.png".into(), bytes);
    let html = "<html><body><p>Before the image.</p><p><img src=\"big.png\" alt=\"big\"/></p><p>After.</p></body></html>".to_string();
    let mut d = doc_with(vec![("a.xhtml", html)], res);
    d.set_layout(&LayoutSettings::for_screen(1272, 1696)).unwrap();
    assert!(d.wait_paginated(Duration::from_secs(300)));
    let n = d.page_count().pages;
    let mut c = Canvas::new(1272, 1696);
    let mark = heap_mark();
    let t = Instant::now();
    for p in 0..n {
        d.render_page(p, &mut c).unwrap();
    }
    result("render_ms", t.elapsed().as_millis());
    result("peak_mb", format!("{:.1}", peak_mb_since(mark)));
    // Something was drawn for the image (not only the frame of a missing image).
    let dark = c.pixels.iter().filter(|&&v| v < 200).count();
    result("dark_px", dark);
}

// ---------------------------------------------------------------------------------
// Crash, hang and memory cases (children).

#[test]
fn deep_xml_nesting_100k() {
    let o = child("nest_xml_100k", Duration::from_secs(300));
    assert!(o.ok, "{}", o.tail());
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn deep_soup_nesting_100k() {
    let o = child("nest_soup_100k", Duration::from_secs(300));
    assert!(o.ok, "{}", o.tail());
}

#[test]
fn deep_inline_nesting_100k() {
    let o = child("nest_inline_100k", Duration::from_secs(300));
    assert!(o.ok, "{}", o.tail());
}

#[test]
fn css_nested_media_50k() {
    let o = child("css_media_nest_50k", Duration::from_secs(120));
    assert!(o.ok, "{}", o.tail());
}

#[test]
fn css_nested_not_50k() {
    let o = child("css_not_nest_50k", Duration::from_secs(120));
    assert!(o.ok, "{}", o.tail());
}

#[test]
fn css_selector_chain_100k() {
    let o = child("css_selector_chain_100k", Duration::from_secs(120));
    assert!(o.ok, "{}", o.tail());
}

#[test]
fn css_descendant_selector_backtracking() {
    let o = child("css_descendant_backtracking", Duration::from_secs(60));
    assert!(o.ok, "timed out={} after {:.0}s\n{}", o.timed_out, o.secs, o.tail());
}

#[test]
fn css_twenty_thousand_rules() {
    let o = child("css_many_rules_20k", Duration::from_secs(300));
    assert!(o.ok, "{}", o.tail());
}

#[test]
fn css_huge_and_infinite_lengths() {
    let o = child("css_huge_lengths", Duration::from_secs(120));
    assert!(o.ok, "{}", o.tail());
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn single_paragraph_of_5mb() {
    let o = child("para_5mb_words", Duration::from_secs(600));
    assert!(o.ok, "timed out={} after {:.0}s\n{}", o.timed_out, o.secs, o.tail());
    // Per-char vectors of the paragraph (text, CharSrc, char_at, boxes) must stay well
    // below the ~410 MB free on the Kindle.
    let peak = o.num("peak_mb");
    assert!(peak < 128.0, "a 5 MB paragraph took {peak} MB of heap ({:?})", o.results);
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn unbroken_word_time_is_not_quadratic() {
    // A word with no break opportunity (a long URL, a row of symbols) is cut line by
    // line. 4x the text should take about 4x the time, not 16x.
    let a = child("word_unbroken_256k", Duration::from_secs(300));
    assert!(a.ok, "{}", a.tail());
    let b = child("word_unbroken_1m", Duration::from_secs(600));
    assert!(b.ok, "1 MB word: timed out={} after {:.0}s (256 KB took {:.0} ms)\n{}", b.timed_out, b.secs, a.num("total_ms"), b.tail());
    let ratio = b.num("total_ms") / a.num("total_ms").max(1.0);
    assert!(ratio < 8.0, "256 KB: {} ms, 1 MB: {} ms (x{ratio:.1})", a.num("total_ms"), b.num("total_ms"));
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn spine_of_5000_items() {
    let a = child("spine_1250", Duration::from_secs(600));
    assert!(a.ok, "{}", a.tail());
    let b = child("spine_5000", Duration::from_secs(900));
    assert!(b.ok, "timed out={} after {:.0}s\n{}", b.timed_out, b.secs, b.tail());
    // Pagination scales linearly. The queries (toc, positions, search) grow about
    // quadratically (known_page / first_page walk the spine for each entry), about 1 s
    // for 5000 items in a debug build: noted, not a failure at this size.
    let ratio_pag = b.num("paginated_ms") / a.num("paginated_ms").max(1.0);
    assert!(ratio_pag < 8.0, "1250 vs 5000 items: pagination x{ratio_pag:.1}: {:?} {:?}", a.results, b.results);
    assert!(b.num("queries_ms") < 30_000.0, "{:?}", b.results);
}

/// About 410 MB are free on the Kindle with the framework stopped; one image page
/// must not use a large part of it.
const IMAGE_PEAK_LIMIT_MB: f64 = 96.0;

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn image_10000x10000_gray_memory() {
    let o = child("image_10k_gray", Duration::from_secs(600));
    assert!(o.ok, "{}", o.tail());
    let peak = o.num("peak_mb");
    assert!(peak < IMAGE_PEAK_LIMIT_MB, "rendering a 10000x10000 gray PNG page took {peak} MB of heap");
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn image_10000x10000_rgb_memory() {
    let o = child("image_10k_rgb", Duration::from_secs(600));
    assert!(o.ok, "{}", o.tail());
    let peak = o.num("peak_mb");
    assert!(peak < IMAGE_PEAK_LIMIT_MB, "rendering a 10000x10000 RGB PNG page took {peak} MB of heap");
}

#[test]
fn svg_use_expansion_bomb() {
    let o = child("svg_use_bomb", Duration::from_secs(60));
    assert!(o.ok, "timed out={} after {:.0}s\n{}", o.timed_out, o.secs, o.tail());
}

// ---------------------------------------------------------------------------------
// In-process attacks.

fn open_fixture(name: &str) -> HtmlDocument {
    ink_formats::open(&common::book_path(name)).unwrap()
}

fn settle(d: &mut HtmlDocument, s: &LayoutSettings) {
    d.set_layout(s).unwrap();
    assert!(d.wait_paginated(Duration::from_secs(300)));
}

/// Writes a minimal EPUB 3 (one spine item per chapter) and returns its path.
fn write_epub(dir: &Path, name: &str, chapters: &[(&str, String)], files: &[(&str, Vec<u8>)]) -> PathBuf {
    write_epub_lang(dir, name, "en", chapters, files)
}

fn write_epub_lang(dir: &Path, name: &str, lang: &str, chapters: &[(&str, String)], files: &[(&str, Vec<u8>)]) -> PathBuf {
    use zip::write::SimpleFileOptions;
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(format!("{name}.epub"));
    let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    z.start_file("mimetype", stored).unwrap();
    z.write_all(b"application/epub+zip").unwrap();
    z.start_file("META-INF/container.xml", deflated).unwrap();
    z.write_all(
        br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
    )
    .unwrap();
    let mut manifest = String::new();
    let mut spine = String::new();
    for (i, (p, _)) in chapters.iter().enumerate() {
        manifest.push_str(&format!("<item id=\"c{i}\" href=\"{p}\" media-type=\"application/xhtml+xml\"/>"));
        spine.push_str(&format!("<itemref idref=\"c{i}\"/>"));
    }
    for (i, (p, _)) in files.iter().enumerate() {
        manifest.push_str(&format!("<item id=\"f{i}\" href=\"{p}\" media-type=\"image/png\"/>"));
    }
    let opf = format!(
        "<?xml version=\"1.0\"?><package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"uid\">\
         <metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:identifier id=\"uid\">urn:uuid:verify-{name}</dc:identifier>\
         <dc:title>{name}</dc:title><dc:language>{lang}</dc:language></metadata><manifest>{manifest}</manifest><spine>{spine}</spine></package>"
    );
    z.start_file("OEBPS/content.opf", deflated).unwrap();
    z.write_all(opf.as_bytes()).unwrap();
    for (p, html) in chapters {
        z.start_file(format!("OEBPS/{p}"), deflated).unwrap();
        z.write_all(html.as_bytes()).unwrap();
    }
    for (p, bytes) in files {
        z.start_file(format!("OEBPS/{p}"), stored).unwrap();
        z.write_all(bytes).unwrap();
    }
    z.finish().unwrap();
    path
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-verify-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Chapter index and DOM position of an xpointer.
fn resolve_x(d: &HtmlDocument, pos: &Position) -> (usize, ink_html::xpointer::DomPos) {
    let Position::XPointer(x) = pos else { panic!("not an xpointer: {pos}") };
    let c = d.book().chapter_of_fragment(x.fragment().unwrap()).unwrap();
    let ch = d.book().chapter(c);
    (c, ink_html::xpointer::resolve(&ch.dom, x).unwrap_or_else(|| panic!("{x} does not resolve")))
}

/// True when page `p` draws the character at text position `pos`.
fn page_shows(d: &HtmlDocument, p: usize, pos: &Position) -> bool {
    let (_, dp) = resolve_x(d, pos);
    let ink_html::xpointer::DomPos::Text { node, offset } = dp else { return true };
    let (l, span) = d.page_layout(p).unwrap();
    let a = l.slices[span.first as usize].first_item as usize;
    let b = l.slices[span.end as usize - 1].end_item as usize;
    l.items[a..b].iter().any(|i| matches!(i, Draw::Text(t) if t.node == node && t.start <= offset && offset < t.start + t.len.max(1)))
}

#[test]
fn xpointer_garbage_strings_do_not_panic() {
    let mut d = open_fixture("notes");
    settle(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let long = format!("/body/DocFragment[2]/body{}", "/div[1]".repeat(100_000));
    let cases = [
        "/body/DocFragment[0]/body/p",
        "/body/DocFragment[4294967295]/body/p[1]/text().1",
        "/body/DocFragment[2]/body/p[4294967295]/text()[4294967295].4294967295",
        "/body/DocFragment[2]/body/p[0]",
        "/body/DocFragment[2]/body/p[1].4294967295",
        "/body/DocFragment[2]/body/1/1/1.99999",
        "/body/DocFragment[2]/head/title",
        "/body/DocFragment[2]/html/body/p",
        "/body/DocFragment[2]",
        "/body/DocFragment[2].99999",
        "/DocFragment[2]/body/p",
        "/body/p[1]/text().5",
        "/body/DocFragment[2]/body/svg:svg/svg:rect",
        "/body/DocFragment[2]/body/p[1]/text()[0].0",
        long.as_str(),
    ];
    for s in cases {
        let Ok(pos) = s.parse::<Position>() else { continue };
        let _ = d.page_of(&pos);
        let _ = d.read_around(&pos, 200);
        let _ = d.location(&pos);
        let _ = d.links(&pos);
        let _ = d.note(&pos);
        let _ = d.set_anchor(&pos);
    }
}

#[test]
fn xpointer_to_a_removed_node_lands_in_its_chapter() {
    // Positions saved for an older edition of the book (a node that no longer exists,
    // an offset past the end of a text) must still open in the right chapter.
    let mut d = open_fixture("juliet");
    settle(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let n = d.page_count().pages;
    for p in [5usize, 60, 120, n - 2] {
        let real = d.position_of_page(p).unwrap();
        let Position::XPointer(x) = &real else { unreachable!() };
        let frag = x.fragment().unwrap();
        let first = d.page_of(&Position::XPointer(XPointer::fragment_point(frag))).unwrap();
        let s = x.to_string();
        let body_at = s.find("/body/").unwrap() + 1;
        let body_at = s[body_at..].find("/body").map(|i| body_at + i).unwrap();
        let removed = [
            format!("{}/div[999]/p[3]/text().42", &s[..body_at + 5]),
            format!("{}/p[777]", &s[..body_at + 5]),
            format!("{}/text().999999", s.split("/text()").next().unwrap()),
            format!("{}/span[40]/text()[9].3", s.split("/text()").next().unwrap()),
        ];
        let mut next_first = n;
        for q in first..n {
            let Position::XPointer(y) = d.position_of_page(q).unwrap() else { unreachable!() };
            if y.fragment() != Some(frag) {
                next_first = q;
                break;
            }
        }
        for r in &removed {
            let pos: Position = r.parse().unwrap_or_else(|e| panic!("{r}: {e:?}"));
            let q = d.page_of(&pos).unwrap_or_else(|e| panic!("{r}: {e}"));
            assert!(q >= first && q < next_first, "{r} -> page {q}, chapter pages {first}..{next_first}");
            assert!(d.read_around(&pos, 100).is_ok(), "{r}");
        }
    }
}

#[test]
fn positions_are_exact_across_font_sizes() {
    // The page holding a saved word position must show that very character, at every
    // font size and screen size.
    let mut d = open_fixture("juliet");
    let mut s = LayoutSettings::for_screen(1272, 1696);
    settle(&mut d, &s);
    let hits = d.search("the", 100_000).unwrap();
    assert!(hits.len() > 500, "{} hits", hits.len());
    let sample: Vec<Position> = hits.iter().step_by(hits.len() / 40).map(|h| h.position.clone()).collect();
    let mut bad = Vec::new();
    for (px, w, h) in [(40.0, 1272, 1696), (26.0, 1272, 1696), (52.0, 1272, 1696), (40.0, 1696, 1272), (30.0, 600, 800)] {
        s.font_px = px;
        s.width = w;
        s.height = h;
        settle(&mut d, &s);
        for pos in &sample {
            let p = d.page_of(pos).unwrap();
            if !page_shows(&d, p, pos) {
                bad.push(format!("{px}px {w}x{h}: {pos} -> page {p}"));
            }
            // The page's start position maps back to the page (was a separate test in
            // document.rs, positions_are_stable_across_font_sizes).
            let start = d.position_of_page(p).unwrap();
            assert_eq!(d.page_of(&start).unwrap(), p, "{px}px {w}x{h}: start of page {p}");
        }
    }
    assert!(bad.is_empty(), "{} of {} positions not on their page:\n{}", bad.len(), sample.len() * 5, bad.join("\n"));
}

#[test]
fn relayout_there_and_back_returns_to_the_same_page() {
    relayout_there_and_back(&[100]);
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn relayout_there_and_back_returns_to_the_same_page_from_three_starts() {
    relayout_there_and_back(&[40, 100, 160]);
}

fn relayout_there_and_back(starts: &[usize]) {
    let mut d = open_fixture("juliet");
    let base = LayoutSettings::for_screen(1272, 1696);
    settle(&mut d, &base);
    let mut report = Vec::new();
    for &start in starts {
        settle(&mut d, &base);
        d.set_current_page(start);
        for _ in 0..3 {
            for px in [26.0, 52.0, 40.0] {
                let mut s = base.clone();
                s.font_px = px;
                d.set_layout(&s).unwrap();
            }
        }
        let after_font = d.current_page();
        settle(&mut d, &base);
        d.set_current_page(start);
        for _ in 0..3 {
            let mut s = base.clone();
            (s.width, s.height) = (1696, 1272);
            d.set_layout(&s).unwrap();
            d.set_layout(&base).unwrap();
        }
        let after_rotation = d.current_page();
        report.push((start, after_font, after_rotation));
    }
    assert!(report.iter().all(|&(a, b, c)| a == b && a == c), "(start, after 3 font cycles 40-26-52-40, after 3 rotations): {report:?}");
}

/// Page counts per chapter in the page map against a fresh layout of the chapter.
fn check_map_against_layout(d: &mut HtmlDocument, s: &LayoutSettings, label: &str) -> Vec<String> {
    let params = Params::new(s);
    let n = d.page_count().pages;
    let mut errors = Vec::new();
    let mut p = 0;
    while p < n {
        let (lay, span) = match d.page_layout(p) {
            Ok(x) => x,
            Err(e) => {
                errors.push(format!("{label}: page {p}: {e}"));
                p += 1;
                continue;
            }
        };
        if span.first != 0 {
            errors.push(format!("{label}: page {p} does not start a chapter in the layout (slice {})", span.first));
        }
        let real = ink_html::paginate::paginate(&lay, params.content_h);
        let mut q = p + 1;
        while q < n && d.page_layout(q).map(|(_, sp)| sp.first != 0).unwrap_or(true) {
            q += 1;
        }
        if q - p != real.len() {
            errors.push(format!("{label}: chapter starting at page {p}: {} pages in the page map, {} in its layout", q - p, real.len()));
        }
        // Every page start of the map is a page start of the layout.
        let pos = d.position_of_page(p).unwrap();
        let (c, _) = resolve_x(d, &pos);
        let ch = d.book().chapter(c);
        let frag = d.book().chapters[c].fragment.unwrap_or(1);
        for (k, sp) in real.iter().enumerate().take(q - p) {
            let mapped = d.position_of_page(p + k).unwrap().to_string();
            let node = ch.dom.preorder.get(sp.key.0 as usize).copied();
            let dp = match node {
                Some(nd) if ch.dom.text(nd).is_some() => ink_html::xpointer::DomPos::Text { node: nd, offset: sp.key.1 },
                Some(nd) => ink_html::xpointer::DomPos::Element { node: nd, point: Some(0) },
                None => continue,
            };
            let laid = ink_html::xpointer::to_xpointer(&ch.dom, frag, d.book().fragments, dp).to_string();
            if laid != mapped {
                errors.push(format!("{label}: page {}: map starts at {mapped}, layout at {laid}", p + k));
            }
        }
        p = q;
    }
    let mut c = Canvas::new(s.width as usize, s.height as usize);
    for p in 0..n {
        if let Err(e) = d.render_page(p, &mut c) {
            errors.push(format!("{label}: render page {p}: {e}"));
        }
    }
    errors
}

#[test]
fn background_page_map_matches_foreground_layout() {
    background_page_map(&["css-selectors", "css-text", "css-box", "fonts", "images", "tables-lists", "notes", "lang", "pagelist", "malformed", "ws"]);
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn background_page_map_matches_foreground_layout_juliet() {
    background_page_map(&["juliet"]);
}

fn background_page_map(names: &[&str]) {
    // The EPUB loader cannot be shared with the background thread, so the page map
    // there is made with image sizes prefilled from a text scan. It must agree with the
    // foreground layout of every chapter.
    let s = LayoutSettings::for_screen(1272, 1696);
    let mut errors = Vec::new();
    for &name in names {
        let mut d = open_fixture(name);
        d.set_layout(&s).unwrap();
        assert!(d.wait_paginated(Duration::from_secs(300)));
        errors.extend(check_map_against_layout(&mut d, &s, name));
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn image_after_gt_in_attribute_keeps_page_map_in_sync() {
    // `prefill_image_sizes` takes the tag up to the first '>', so `alt="x > y"` hides
    // the src. The background thread then lays the chapter out without the image.
    let dir = temp_dir("gt-alt");
    let tall = png_gray(100, 1400);
    let page = |t: &str| format!("<?xml version=\"1.0\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><body>{t}</body></html>");
    let chapters = [
        ("one.xhtml", page("<h1>One</h1><p>First chapter.</p>")),
        ("two.xhtml", page("<h1>Two</h1><p>Before.</p><p><img alt=\"x &gt; y\" src=\"tall.png\"/></p><p>Text after the tall image.</p>")),
        ("three.xhtml", page("<h1>Three</h1><p>Third chapter.</p>")),
    ];
    // Also with a literal '>' (allowed in XML attribute values).
    let mut chapters = chapters.to_vec();
    chapters[1].1 = chapters[1].1.replace("&gt;", ">");
    let path = write_epub(&dir, "gt-alt", &chapters, &[("tall.png", tall)]);
    let s = LayoutSettings::for_screen(1272, 1696);
    let mut d = ink_formats::open(&path).unwrap();
    d.set_layout(&s).unwrap();
    assert!(d.wait_paginated(Duration::from_secs(60)));
    let errors = check_map_against_layout(&mut d, &s, "gt-alt");
    let all = all_text(&d);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(errors.is_empty() && all.contains("tall image"), "{}\ntext: {all}", errors.join("\n"));
}

fn three_chapter_doc() -> HtmlDocument {
    let para = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore. ".repeat(6);
    let ch = |i: usize| format!("<html><body><h1>Chapter {i}</h1>{}<p>End of chapter {i}.</p></body></html>", format!("<p>{para}</p>").repeat(12));
    doc_with(vec![("a.xhtml", ch(1)), ("b.xhtml", ch(2)), ("c.xhtml", ch(3))], MemoryLoader::default())
}

#[test]
fn garbage_page_map_cache_does_not_panic() {
    let dir = temp_dir("garbage-cache");
    let s = LayoutSettings::for_screen(1272, 1696);
    let file = ink_html::paginate::cache_file(&dir, "garbage", &s);
    let v = ink_html::paginate::ENGINE_VERSION;
    std::fs::write(&file, format!("rinky-pagemap {v} 3\n10 0.0 4294967295.4294967295 7.1\n0\n99 5.5 3.1 0.0 0.0\n")).unwrap();
    let mut d = three_chapter_doc();
    d.set_page_cache(&dir, "garbage");
    d.set_layout(&s).unwrap();
    let n = d.page_count().pages;
    let mut c = Canvas::new(1272, 1696);
    for p in 0..n + 2 {
        let _ = d.render_page(p, &mut c);
        if let Ok(pos) = d.position_of_page(p) {
            let _ = d.page_of(&pos);
            let _ = d.location(&pos);
        }
    }
    let _ = d.toc();
    let _ = d.search("chapter", 50);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stale_page_map_cache_loses_no_text() {
    // A cache written by an engine whose layout differs (ENGINE_VERSION not bumped, a
    // system font replaced under the same name): one page per chapter.
    let dir = temp_dir("stale-cache");
    let s = LayoutSettings::for_screen(1272, 1696);
    let file = ink_html::paginate::cache_file(&dir, "stale", &s);
    let v = ink_html::paginate::ENGINE_VERSION;
    std::fs::write(&file, format!("rinky-pagemap {v} 3\n5000 0.0\n5000 0.0\n5000 0.0\n")).unwrap();
    let mut d = three_chapter_doc();
    d.set_page_cache(&dir, "stale");
    d.set_layout(&s).unwrap();
    // Fixed (html agent): the chapter in view is checked against a fresh layout, the
    // stale cache is dropped and the book is paginated again in the background, so the
    // count is complete only after waiting (a reader treats an incomplete count as an
    // unknown total).
    assert!(!d.cache_hit(), "a stale cache must not be used");
    assert!(d.wait_paginated(Duration::from_secs(60)));
    let all = all_text(&d);
    let rewritten = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    for i in 1..=3 {
        assert!(all.contains(&format!("chapter {i}.")), "'End of chapter {i}.' unreachable ({} pages)", d.page_count().pages);
    }
    assert!(!rewritten.contains("\n5000 0.0\n5000 0.0"), "the stale cache file was kept: {rewritten}");
}

/// A cache whose first chapter is right but whose later chapters are stale: the chapter
/// in view passes the check, the later ones are corrected when the reader reaches them.
#[test]
fn stale_later_chapter_is_corrected_when_reached() {
    let dir = temp_dir("stale-later");
    let s = LayoutSettings::for_screen(1272, 1696);
    let mut fresh = three_chapter_doc();
    settle(&mut fresh, &s);
    let first: Vec<String> = (0..fresh.page_count().pages)
        .map(|p| fresh.position_of_page(p).unwrap())
        .filter(|p| matches!(p, Position::XPointer(x) if x.fragment() == Some(1)))
        .map(|p| {
            let (_, dp) = resolve_x(&fresh, &p);
            let ch = fresh.book().chapter(0);
            let key = ink_html::xpointer::order_key(&ch.dom, dp);
            format!("{}.{}", key.0, key.1)
        })
        .collect();
    assert!(first.len() > 1, "{first:?}");
    let file = ink_html::paginate::cache_file(&dir, "later", &s);
    let v = ink_html::paginate::ENGINE_VERSION;
    std::fs::write(&file, format!("rinky-pagemap {v} 3\n5000 {}\n5000 0.0\n5000 0.0\n", first.join(" "))).unwrap();
    let mut d = three_chapter_doc();
    d.set_page_cache(&dir, "later");
    d.set_layout(&s).unwrap();
    assert!(d.cache_hit(), "chapter 1 of the cache is right");
    // Turn pages like a reader: the count is read again after every page.
    let mut all = String::new();
    let mut p = 0;
    while p < d.page_count().pages {
        all.push_str(&page_text(&d, p));
        all.push(' ');
        d.set_current_page(p);
        p += 1;
    }
    let _ = std::fs::remove_dir_all(&dir);
    for i in 1..=3 {
        assert!(all.contains(&format!("chapter {i}.")), "'End of chapter {i}.' unreachable ({} pages)", d.page_count().pages);
    }
    assert_eq!(d.page_count(), fresh.page_count());
    assert!(!file.exists(), "the stale cache file must be deleted");
}

#[test]
fn cache_key_covers_every_setting() {
    let base = LayoutSettings::for_screen(1272, 1696);
    let dir = Path::new("/tmp/x");
    let f0 = ink_html::paginate::cache_file(dir, "id", &base);
    let mut variants: Vec<LayoutSettings> = Vec::new();
    let mut v = base.clone();
    v.width += 1;
    variants.push(v.clone());
    v = base.clone();
    v.height += 1;
    variants.push(v.clone());
    for f in 0..4 {
        v = base.clone();
        match f {
            0 => v.margin_left += 1.0,
            1 => v.margin_right += 1.0,
            2 => v.margin_top += 1.0,
            _ => v.margin_bottom += 1.0,
        }
        variants.push(v.clone());
    }
    v = base.clone();
    v.font_px += 0.5;
    variants.push(v.clone());
    v = base.clone();
    v.line_spacing += 0.05;
    variants.push(v.clone());
    v = base.clone();
    v.justify = !v.justify;
    variants.push(v.clone());
    v = base.clone();
    v.hyphenate = !v.hyphenate;
    variants.push(v.clone());
    v = base.clone();
    v.publisher_styles = !v.publisher_styles;
    variants.push(v.clone());
    v = base.clone();
    v.font_family = Some("Bookerly".into());
    variants.push(v.clone());
    for v in variants {
        assert_ne!(ink_html::paginate::cache_file(dir, "id", &v), f0, "{v:?}");
    }
}

#[test]
fn page_numbers_near_usize_max() {
    let mut d = three_chapter_doc();
    settle(&mut d, &LayoutSettings::for_screen(1272, 1696));
    let n = d.page_count().pages;
    let mut c = Canvas::new(1272, 1696);
    assert!(d.render_page(usize::MAX, &mut c).is_err());
    assert!(d.position_of_page(usize::MAX).is_err());
    assert!(d.page_layout(usize::MAX).is_err());
    d.set_current_page(usize::MAX);
    assert_eq!(d.current_page(), n - 1);
    let pos = d.position_of_page(0).unwrap();
    assert!(!d.read_around(&pos, usize::MAX).unwrap().is_empty());
    assert!(!d.search("lorem", usize::MAX).unwrap().is_empty());
    let _ = d.select(0, Point { x: i32::MIN, y: i32::MIN }, Point { x: i32::MAX, y: i32::MAX }).unwrap();
    let _ = d.select(n - 1, Point { x: -50, y: 100_000 }, Point { x: 5, y: -3 }).unwrap();
    assert!(d.select(usize::MAX, Point { x: 0, y: 0 }, Point { x: 1, y: 1 }).is_err());
}

/// Deterministic byte mutations of `src`.
fn mutations(src: &[u8], seed: u64) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for cut in [0usize, 1, 8, 16, 24, 33, 64, 100, 300, src.len() / 3, src.len() / 2, src.len().saturating_sub(1)] {
        out.push(src[..cut.min(src.len())].to_vec());
    }
    let mut x = seed | 1;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for k in 0..60 {
        let mut m = src.to_vec();
        let flips = 1 + (k % 6);
        for _ in 0..flips {
            // Mostly in the header, where sizes and chunk lengths live.
            let range = if k % 2 == 0 { m.len().min(200) } else { m.len() };
            let i = (next() as usize) % range.max(1);
            m[i] = (next() & 0xff) as u8;
        }
        out.push(m);
    }
    // Header size fields set to huge values (PNG IHDR width/height).
    if src.len() > 24 && &src[1..4] == b"PNG" {
        for (w, h) in [(0xffff_fff0u32, 1u32), (1, 0xffff_fff0), (60_000, 60_000), (0, 5)] {
            let mut m = src.to_vec();
            m[16..20].copy_from_slice(&w.to_be_bytes());
            m[20..24].copy_from_slice(&h.to_be_bytes());
            out.push(m);
        }
    }
    out
}

#[test]
fn corrupt_images_do_not_panic() {
    corrupt_images(8);
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn corrupt_images_do_not_panic_every_mutation() {
    corrupt_images(1);
}

/// Decodes every `step`-th mutation of every image of the images fixture.
fn corrupt_images(step: usize) {
    let zip = zip::ZipArchive::new(std::fs::File::open(common::book_path("images")).unwrap()).unwrap();
    let names: Vec<String> = zip.file_names().filter_map(|r| r.ok()).map(|s| s.to_string()).collect();
    let loader = ink_formats::epub::ZipLoader::new(zip);
    use ink_doc::ResourceLoader;
    let mut panics = Vec::new();
    let mut tried = 0;
    for name in names.iter().filter(|n| [".png", ".jpg", ".jpeg", ".gif", ".webp", ".svg"].iter().any(|e| n.to_ascii_lowercase().ends_with(e))) {
        let bytes = loader.read(name).unwrap();
        for (k, m) in mutations(&bytes, name.len() as u64 * 7919).into_iter().enumerate().step_by(step) {
            tried += 1;
            let r = std::panic::catch_unwind(|| {
                let _ = ink_html::images::intrinsic_size(&m);
                let _ = ink_html::images::decode_scaled(&m, 300, 200, None);
                let _ = ink_html::images::decode_into_box(&m, 300, 200, "xMidYMid meet");
            });
            if r.is_err() {
                panics.push(format!("{name} mutation {k}"));
            }
        }
    }
    assert!(tried > 100 / step, "{tried} images tried: {names:?}");
    assert!(panics.is_empty(), "{} panics: {panics:?}", panics.len());
}

#[test]
fn odd_chapter_content_opens() {
    odd_chapter_content(10);
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn odd_chapter_content_opens_full_size() {
    odd_chapter_content(1);
}

/// Odd chapters; the repeat counts of the big cases are divided by `div`.
fn odd_chapter_content(div: usize) {
    let png = png_gray(20, 20);
    let utf16: String = "<html><body><p>utf sixteen</p></body></html>".encode_utf16().flat_map(|u| u.to_le_bytes()).map(|b| b as char).collect();
    let cases: Vec<(&str, String)> = vec![
        ("binary", String::from_utf8_lossy(&png).into_owned()),
        ("utf16-as-latin1", format!("\u{ff}\u{fe}{utf16}")),
        ("nul", "<html><body><p>nul\0char\0</p></body></html>".into()),
        ("wrong-namespace", "<html xmlns=\"urn:wrong\"><body><p>wrong ns</p></body></html>".into()),
        ("prefixed", "<x:html xmlns:x=\"http://www.w3.org/1999/xhtml\"><x:body><x:p>prefixed</x:p></x:body></x:html>".into()),
        ("svg-root", "<svg xmlns=\"http://www.w3.org/2000/svg\"><text>svg</text></svg>".into()),
        (
            "entity-bomb",
            "<?xml version=\"1.0\"?><!DOCTYPE html [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;\"><!ENTITY c \"&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;\">]><html><body><p>&c;&c;&c; bomb</p></body></html>".into(),
        ),
        ("head-only", "<html><head><title>t</title></head></html>".into()),
        ("two-bodies", "<html><body><p>one</p></body><body><p>two</p></body></html>".into()),
        ("stray-ends", "</p></div></body></html><p>stray</p>".into()),
        ("cdata-only", "<![CDATA[<p>cdata</p>]]>".into()),
        ("comment-unclosed", "<html><body><p>before</p><!-- never closed <p>after</p>".into()),
        ("many-attrs", format!("<html><body><p {}>attrs</p></body></html>", (0..20_000 / div).map(|i| format!("a{i}=\"{i}\"")).collect::<Vec<_>>().join(" "))),
        ("huge-attr", format!("<html><body><p title=\"{}\">big attr</p></body></html>", "x".repeat((2 << 20) / div))),
        ("table-colspan", "<html><body><table><tr><td colspan=\"4294967295\" rowspan=\"99999\">a</td><td colspan=\"-1\">b</td></tr></table></body></html>".into()),
        ("nested-tables", format!("<html><body>{}cell{}</body></html>", "<table><tr><td>".repeat(300 / div), "</td></tr></table>".repeat(300 / div))),
        ("wide-table", format!("<html><body><table>{}</table></body></html>", format!("<tr>{}</tr>", "<td>w</td>".repeat(400 / div)).repeat(40))),
        ("pre-tabs", format!("<html><body><pre>{}</pre></body></html>", "\t".repeat(100_000 / div / div))),
        ("br-flood", format!("<html><body><p>{}</p></body></html>", "<br/>".repeat(20_000 / div))),
        ("img-no-src", "<html><body><p><img/><img src=\"\"/><img src=\"data:image/png;base64,AAAA\"/><image/></p></body></html>".into()),
        ("svg-empty", "<html><body><svg xmlns=\"http://www.w3.org/2000/svg\" width=\"0\" height=\"0\" viewBox=\"0 0 0 0\"/><svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"-1 -1 -5 nan\"/></body></html>".into()),
    ];
    let mut failures = Vec::new();
    for (name, html) in cases {
        let r = std::panic::catch_unwind(move || {
            let mut res = MemoryLoader::default();
            res.0.insert("pic.png".into(), png_gray(20, 20));
            let chapters = vec![HtmlChapter { path: "a.xhtml".into(), html }, HtmlChapter { path: "b.xhtml".into(), html: "<html><body><p>next</p></body></html>".into() }];
            let Ok(mut d) = HtmlDocument::new(Metadata::default(), chapters, Box::new(res)) else { return };
            if d.set_layout(&LayoutSettings::for_screen(600, 800)).is_err() {
                return;
            }
            d.wait_paginated(Duration::from_secs(60));
            let n = d.page_count().pages;
            let mut c = Canvas::new(600, 800);
            for p in 0..n.min(30) {
                d.render_page(p, &mut c).unwrap();
                let pos = d.position_of_page(p).unwrap();
                assert_eq!(d.page_of(&pos).unwrap(), p, "{pos}");
            }
            let _ = d.search("a", 10);
            let _ = d.toc();
        });
        if let Err(e) = r {
            let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            failures.push(format!("{name}: {msg}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn rapid_relayout_while_paginating() {
    // Font size changes faster than the background thread finishes: abandoned workers
    // keep running and may write cache files; the final map must equal a clean one.
    let dir = temp_dir("rapid");
    let book = common::book_path("juliet");
    let id = ink_store::partial_md5(&book).unwrap();
    let mut s = LayoutSettings::for_screen(1272, 1696);
    let mut d = open_fixture("juliet");
    d.set_page_cache(&dir, &id);
    let mut c = Canvas::new(1272, 1696);
    for i in 0..18 {
        s.font_px = [30.0, 40.0, 50.0][i % 3];
        d.set_layout(&s).unwrap();
        let p = d.current_page();
        d.render_page(p, &mut c).unwrap();
        d.set_current_page(p + 7);
    }
    s.font_px = 40.0;
    d.set_layout(&s).unwrap();
    assert!(d.wait_paginated(Duration::from_secs(300)));
    let got = d.page_count();
    // Let abandoned workers finish writing.
    std::thread::sleep(Duration::from_secs(3));
    let mut clean = open_fixture("juliet");
    settle(&mut clean, &s);
    let want = clean.page_count();
    assert_eq!(got, want);
    for p in (0..want.pages).step_by(9) {
        assert_eq!(d.position_of_page(p).unwrap(), clean.position_of_page(p).unwrap(), "page {p}");
    }
    // The cache files written along the way agree with clean layouts.
    for px in [30.0, 40.0, 50.0] {
        s.font_px = px;
        let mut cached = open_fixture("juliet");
        cached.set_page_cache(&dir, &id);
        cached.set_layout(&s).unwrap();
        if !cached.cache_hit() {
            continue;
        }
        let mut fresh = open_fixture("juliet");
        settle(&mut fresh, &s);
        assert_eq!(cached.page_count(), fresh.page_count(), "{px}px");
        for p in (0..fresh.page_count().pages).step_by(13) {
            assert_eq!(cached.position_of_page(p).unwrap(), fresh.position_of_page(p).unwrap(), "{px}px page {p}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn secs(f: impl FnOnce()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64()
}

/// Time of building the DOM of one chapter.
fn dom_build_secs(html: String) -> f64 {
    let book = ink_html::book::Book::new(Metadata::default(), vec![("a.xhtml".into(), html)], &MemoryLoader::default());
    secs(|| {
        let _ = book.chapter(0);
    })
}

#[test]
fn many_paragraphs_build() {
    let body: String = (0..16000).map(|i| format!("<p>Paragraph {i}.</p>\n")).collect();
    let book = ink_html::book::Book::new(Metadata::default(), vec![("a.xhtml".into(), format!("<html><body>{body}</body></html>"))], &MemoryLoader::default());
    let _ = book.chapter(0);
}

#[test]
#[ignore = "slow: wall-clock ratio, fails on a loaded machine; run with --ignored on an idle one"]
fn many_paragraphs_build_in_linear_time() {
    // `<p>..</p>\n` repeated: every newline between blocks is an inline run that
    // cleanup_block removes with a full `retain` over the children.
    let body = |n: usize| (0..n).map(|i| format!("<p>Paragraph {i}.</p>\n")).collect::<String>();
    let a = dom_build_secs(format!("<html><body>{}</body></html>", body(4000)));
    let b = dom_build_secs(format!("<html><body>{}</body></html>", body(16000)));
    let ratio = b / a.max(1e-4);
    assert!(ratio < 8.0, "4000 paragraphs: {a:.3}s, 16000: {b:.3}s (x{ratio:.1})");
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn many_script_tags_parse_in_linear_time() {
    let doc = |n: usize| format!("<body><p>soup{}", "<style>p{color:red}</style><p>text <br>".repeat(n));
    let a = secs(|| {
        let _ = ink_html::parse::expand_self_closing(&doc(2000));
    });
    let b = secs(|| {
        let _ = ink_html::parse::expand_self_closing(&doc(8000));
    });
    let ratio = b / a.max(1e-4);
    assert!(ratio < 8.0, "2000 tags: {a:.3}s, 8000: {b:.3}s (x{ratio:.1})");
}

/// A document of `n` headings with a TOC of `n` entries, laid out.
fn toc_doc(n: usize) -> impl ink_doc::Document {
    let body: String = (0..n).map(|i| format!("<h2 id=\"e{i}\">Entry {i}</h2><p>Text {i}.</p>")).collect();
    let mut d = doc_with(
        vec![("a.xhtml", format!("<html><body>{body}</body></html>")), ("b.xhtml", "<html><body><p>end</p></body></html>".into())],
        MemoryLoader::default(),
    );
    let toc: Vec<ink_html::NavPoint> = (0..n).map(|i| ink_html::NavPoint { title: format!("Entry {i}"), href: format!("a.xhtml#e{i}"), level: 0 }).collect();
    d.set_navigation(ink_html::Navigation { toc, page_list: Vec::new() });
    settle(&mut d, &LayoutSettings::for_screen(1272, 1696));
    d
}

#[test]
fn toc_with_thousands_of_entries() {
    // Functional part (no clock): 1500 entries all come back, in order.
    let d = toc_doc(1500);
    let t = d.toc();
    assert_eq!(t.len(), 1500);
    assert_eq!(t[0].title, "Entry 0");
    assert_eq!(t[1499].title, "Entry 1499");
}

#[test]
#[ignore = "slow: wall-clock ratio, fails on a loaded machine; run with --ignored on an idle one"]
fn toc_with_thousands_of_entries_time_ratio() {
    let toc_secs = |n: usize| {
        let d = toc_doc(n);
        secs(|| {
            let t = d.toc();
            assert_eq!(t.len(), n);
        })
    };
    let a = toc_secs(1500);
    let b = toc_secs(6000);
    let ratio = b / a.max(1e-4);
    assert!(ratio < 8.0, "1500 entries: {a:.3}s, 6000: {b:.3}s (x{ratio:.1})");
}

#[test]
fn hyphenation_language_from_opf_only() {
    use ink_text::Hyphenator;
    // No lang attribute anywhere: the language comes from dc:language alone.
    let word = "Donaudampfschifffahrtsgesellschaftskapitänsmützenabzeichen";
    let body = format!("<?xml version=\"1.0\"?><html xmlns=\"http://www.w3.org/1999/xhtml\"><body><p>{}</p></body></html>", format!("Ein {word} und noch ein {word} hier. ").repeat(8));
    let dir = temp_dir("opf-lang");
    let mut found = HashMap::new();
    for lang in ["de", "en"] {
        let path = write_epub_lang(&dir, &format!("opf-{lang}"), lang, &[("a.xhtml", body.clone())], &[]);
        let mut d = ink_formats::open(&path).unwrap();
        let mut s = LayoutSettings::for_screen(700, 1696);
        s.hyphenate = true;
        settle(&mut d, &s);
        let (l, _) = d.page_layout(0).unwrap();
        let pieces: Vec<String> = l
            .items
            .iter()
            .filter_map(|i| match i {
                Draw::Text(t) if t.text.ends_with('-') && word.starts_with(t.text.trim_end_matches('-')) => Some(t.text.trim_end_matches('-').to_string()),
                _ => None,
            })
            .collect();
        let h = ink_html::hyph::for_lang(lang).unwrap();
        let points: Vec<usize> = h.hyphenate(word);
        assert!(!pieces.is_empty(), "{lang}: no hyphenated piece of {word}");
        for p in &pieces {
            assert!(points.contains(&p.len()), "{lang}: '{p}-' is not a {lang} hyphenation point ({points:?})");
        }
        found.insert(lang, points);
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert_ne!(found["de"], found["en"], "the word must hyphenate differently in de and en for this test to mean anything");
}

#[test]
fn list_counter_limits() {
    let html = "<html><body><ol start=\"-9223372036854775808\" reversed=\"\"><li>a</li><li>b</li></ol><ol><li value=\"9223372036854775807\">c</li><li>d</li></ol></body></html>";
    let mut d = doc_with(vec![("a.xhtml", html.to_string())], MemoryLoader::default());
    settle(&mut d, &LayoutSettings::for_screen(600, 800));
    let mut c = Canvas::new(600, 800);
    d.render_page(0, &mut c).unwrap();
}

#[test]
fn nav_document_nested_100k() {
    let o = child("nav_nest_100k", Duration::from_secs(120));
    assert!(o.ok, "{}", o.tail());
}

#[test]
fn nav_document_nested_300_levels() {
    // Levels are u8: a TOC nested deeper than 255 levels must not overflow.
    let n = 300;
    let navx = format!(
        "<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\"><body><nav epub:type=\"toc\">{}{}</nav></body></html>",
        "<ol><li><a href=\"a.xhtml\">x</a>".repeat(n),
        "</li></ol>".repeat(n)
    );
    let nav = ink_html::nav::parse_nav(&navx, "OPS/nav.xhtml");
    assert_eq!(nav.toc.len(), n);
}

#[test]
fn unbroken_word_is_cut_to_the_text_width() {
    // Fixed (html agent): a word wider than the line at the start of a line used to be
    // drawn whole, past the right edge. It is cut (or hyphenated) line by line now;
    // `white-space: nowrap` text still overflows on purpose.
    let s = LayoutSettings::for_screen(600, 800);
    let right = s.width as f32 - s.margin_right + 1.0;
    for body in [format!("<p>{}</p>", "a".repeat(5000)), format!("<p>Go to https://example.org/{}.html now</p>", "x".repeat(400))] {
        let mut d = doc_with(vec![("a.xhtml", format!("<html><body>{body}</body></html>"))], MemoryLoader::default());
        settle(&mut d, &s);
        let mut chars = 0;
        for p in 0..d.page_count().pages {
            let (l, span) = d.page_layout(p).unwrap();
            let a = l.slices[span.first as usize].first_item as usize;
            let b = l.slices[span.end as usize - 1].end_item as usize;
            for i in &l.items[a..b] {
                if let Draw::Text(t) = i {
                    assert!(t.x + t.width <= right, "page {p}: run '{}...' ends at {} > {right}", &t.text[..t.text.len().min(20)], t.x + t.width);
                    chars += t.text.trim_end_matches('-').len();
                }
            }
        }
        assert!(chars >= 5000.min(body.len() - 20), "{chars} chars drawn");
    }
}
