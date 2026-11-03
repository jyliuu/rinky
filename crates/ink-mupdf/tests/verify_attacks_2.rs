//! Verifier attacks (cycle 2) on ink-mupdf: column detection on pages with little ink,
//! TOC and link targets past the last page, CBZ entries that are not images, and public
//! helpers called with out-of-range screens.
//!
//! Every fixture is generated at run time, so the test binary also runs on the Kindle
//! emulator (`tests/e2e/mupdf-verify2-attacks.sh` runs each test alone with a timeout).
//! Hang tests run the code on a helper thread and fail after a time limit; a hung thread
//! keeps spinning until the test process exits, so run them one per process
//! (`--exact NAME`).
//!
//! Tests marked `#[ignore = "verifier: ..."]` fail today; each one is an open finding.
//! `#[ignore = "verifier (minor, ...)"]` marks a minor finding.
//! Run every finding: `cargo test -p ink-mupdf --test verify_attacks_2 -- --include-ignored`.

use ink_device::Canvas;
use ink_doc::{Document, LayoutSettings, LinkTarget, Position};
use ink_mupdf::analyze::{column_cuts, Gray};
use ink_mupdf::testgen::{self, PdfPage};
use ink_mupdf::{FixedOptions, MupdfDocument};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

// --- Helpers -----------------------------------------------------------------------------

/// Slower targets (the ARM emulator) get longer time limits.
fn slow() -> u64 {
    if cfg!(target_arch = "arm") {
        20
    } else {
        1
    }
}

/// Writes `bytes` to a per-process temp file (temporary name, then rename).
fn write(name: &str, bytes: &[u8]) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("ink-mupdf-verify2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let tmp = dir.join(format!(".{name}.{}", N.fetch_add(1, Ordering::SeqCst)));
    std::fs::write(&tmp, bytes).unwrap();
    std::fs::rename(&tmp, &p).unwrap();
    p
}

/// Runs `f` on a thread with an 8 MiB stack (like the reader's main thread) and fails the
/// test when it panics or does not return within `secs` seconds (times `slow()`).
fn within<T: Send + 'static>(secs: u64, what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let _ = tx.send(f());
        })
        .unwrap();
    let limit = secs * slow();
    match rx.recv_timeout(Duration::from_secs(limit)) {
        Ok(v) => v,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{what}: no result after {limit} s (hang)"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what}: panicked (see above)"),
    }
}

/// Runs every case on its own thread at once and waits up to `secs` seconds (times
/// `slow()`) in total. Returns (case, result) with `None` for a case that did not return
/// (hung) or panicked. Hung threads keep spinning until the process exits.
type Case = (String, Box<dyn FnOnce() -> String + Send>);
fn survey(secs: u64, cases: Vec<Case>) -> Vec<(String, Option<String>)> {
    let mut rxs = Vec::new();
    for (name, f) in cases {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .stack_size(8 << 20)
            .spawn(move || {
                let _ = tx.send(f());
            })
            .unwrap();
        rxs.push((name, rx));
    }
    let deadline = Instant::now() + Duration::from_secs(secs * slow());
    rxs.into_iter()
        .map(|(name, rx)| {
            let left = deadline.saturating_duration_since(Instant::now());
            (name, rx.recv_timeout(left).ok())
        })
        .collect()
}

/// Prints a survey and fails when a case hung.
fn assert_all_returned(what: &str, results: &[(String, Option<String>)]) {
    for (name, r) in results {
        eprintln!("{what}: {name}: {}", r.as_deref().unwrap_or("NO RESULT (hang)"));
    }
    let hung: Vec<&str> = results.iter().filter(|r| r.1.is_none()).map(|r| r.0.as_str()).collect();
    assert!(hung.is_empty(), "{what}: no result (hang) for {hung:?}");
}

fn layout(d: &mut MupdfDocument, w: u32, h: u32) {
    d.set_layout(&LayoutSettings::for_screen(w, h)).unwrap();
}

fn opts(s: &str) -> FixedOptions {
    FixedOptions::parse(s).unwrap()
}

/// A white `w` x `h` gray image with black boxes `(x0, y0, x1, y1)`.
fn gray(w: usize, h: usize, boxes: &[(usize, usize, usize, usize)]) -> Gray {
    let mut px = vec![255u8; w * h];
    for &(x0, y0, x1, y1) in boxes {
        for y in y0..y1 {
            px[y * w + x0..y * w + x1].fill(0);
        }
    }
    Gray { w, h, px }
}

/// A PDF from raw object bodies (object `i + 1` is `objs[i]`) with a classic xref table.
fn raw_pdf(objs: &[String]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offs = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    let mut x = format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1);
    for o in &offs {
        let _ = writeln!(x, "{o:010} 00000 n ");
    }
    let _ = write!(x, "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1);
    out.extend_from_slice(x.as_bytes());
    out
}

fn stream(content: &str) -> String {
    format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len())
}

// --- 1. Column detection on pages with little ink ----------------------------------------
//
// `analyze::column_cuts` scans `x` from `lo` to `hi` for runs of low-ink pixel columns
// (at most `tol` ink pixels, `tol` = 1 % of the ink box height, at least 1). Inside a run
// the inner loop stops at `x1 - 1` (the last column of the ink box) without moving `x`.
// When `hi` is `x1 - 1` (an ink box 2 to 4 px wide for 2 columns, up to 8 px for 4) and
// that last column is "low ink", the outer loop visits `x1 - 1` forever.

/// Pure function: ink only in rows 0 and 299 at x = 48..52 (two short dashes, one above the
/// other). Every ink column holds 2 ink pixels; `tol` is 3, so they all count as gap.
#[test]
fn column_cuts_returns_for_a_narrow_sparse_ink_box() {
    let g = gray(100, 300, &[(48, 0, 52, 1), (48, 299, 52, 300)]);
    let cuts = within(5, "column_cuts on a 4 px wide ink box", move || column_cuts(&g, (48, 0, 52, 300), (0, 100), 2));
    eprintln!("cuts {cuts:?}");
    assert_eq!(cuts.len(), 1);
}

/// Pure function: a blank analysis image only 4 px wide (2 columns) or 8 px wide
/// (4 columns): every column is gap.
#[test]
fn column_cuts_returns_for_a_narrow_blank_image() {
    for (w, n) in [(4usize, 2usize), (8, 4)] {
        let g = gray(w, 600, &[]);
        let cuts = within(5, &format!("column_cuts on a blank {w} x 600 image, {n} columns"), move || {
            column_cuts(&g, (0, 0, w, 600), (0, w), n)
        });
        eprintln!("{w} px, {n} columns: cuts {cuts:?}");
        assert_eq!(cuts.len(), n - 1);
    }
}

/// Control: the same box with dense ink (a solid 4 px bar) is not "gap" and returns.
#[test]
fn column_cuts_returns_for_a_narrow_dense_ink_box() {
    let g = gray(100, 300, &[(48, 0, 52, 300)]);
    let cuts = within(5, "column_cuts on a solid 4 px bar", move || column_cuts(&g, (48, 0, 52, 300), (0, 100), 2));
    assert_eq!(cuts.len(), 1);
}

/// A comic page (CBZ) that is white except two 4 px dashes, one near the top and one near
/// the bottom, at the same x. With `columns=2`, opening the book analyses the first pages
/// for the tile plan and never returns.
#[test]
fn columns_mode_opens_a_cbz_page_with_two_small_marks() {
    let (w, h) = (400usize, 600usize);
    let mut px = vec![255u8; w * h];
    for y in [10, 11, 588, 589] {
        px[y * w + 198..y * w + 202].fill(0);
    }
    let path = write("two-marks.cbz", &testgen::write_zip(&[("001.png", testgen::png_gray(w, h, &px))]));
    let line = within(20, "open a 2-mark CBZ page with columns=2", move || {
        let t = Instant::now();
        let mut d = MupdfDocument::open(&path, opts("columns=2")).unwrap();
        layout(&mut d, 1272, 1696);
        let mut c = Canvas::new(1272, 1696);
        d.render_page(0, &mut c).unwrap();
        format!("opened and rendered in {:?}, plan {:?}, columns {:?}", t.elapsed(), d.plan(), d.columns(0))
    });
    eprintln!("{line}");
}

/// A letter PDF page that is white except two dust specks (4 x 1 px in the 600 px analysis
/// image, 5.3 x 1.3 pt), 300 px apart, at the same x: a blank scanned page. Checked with
/// `columns=2` and with `crop=1,columns=2,fit=width` (the two-column paper setting).
#[test]
fn columns_mode_opens_a_pdf_page_with_two_specks() {
    // Analysis scale for a letter page: 600 / 792 px per point = 25/33.
    let pt = |px: f32| px * 33.0 / 25.0;
    let mut p = PdfPage::letter();
    p.rect(pt(200.0), pt(100.0), pt(4.0), pt(1.0), 0.0);
    p.rect(pt(200.0), pt(400.0), pt(4.0), pt(1.0), 0.0);
    let path = write("two-specks.pdf", &testgen::write_pdf(&[p], &[], &[]));
    let cases: Vec<Case> = ["columns=2", "crop=1,columns=2,fit=width", "columns=3", "columns=4", "crop=1,columns=4"]
        .into_iter()
        .map(|o| {
            let path = path.clone();
            let f: Box<dyn FnOnce() -> String + Send> = Box::new(move || {
                let t = Instant::now();
                let mut d = MupdfDocument::open(&path, opts(o)).unwrap();
                layout(&mut d, 1272, 1696);
                let mut c = Canvas::new(1272, 1696);
                d.render_page(0, &mut c).unwrap();
                format!("opened and rendered in {:?}, plan {:?}, columns {:?}", t.elapsed(), d.plan(), d.columns(0))
            });
            (format!("'{o}'"), f)
        })
        .collect();
    assert_all_returned("2-speck letter page", &survey(20, cases));
}

/// The plan samples pages 0, 1 and pages/2. A speck page later in the book (page 4 of 5)
/// opens fine and hangs when the reader turns to it, asks for its position or searches.
#[test]
fn columns_mode_turns_to_a_later_page_with_two_specks() {
    let pt = |px: f32| px * 33.0 / 25.0;
    let mut pages = Vec::new();
    for i in 0..5 {
        let mut p = PdfPage::letter();
        if i == 4 {
            p.rect(pt(200.0), pt(100.0), pt(4.0), pt(1.0), 0.0);
            p.rect(pt(200.0), pt(400.0), pt(4.0), pt(1.0), 0.0);
        } else {
            p.text(72.0, 72.0, 11.0, testgen::Font::Times, &testgen::filler(i, 40));
            p.text(330.0, 72.0, 11.0, testgen::Font::Times, &testgen::filler(i + 9, 40));
        }
        pages.push(p);
    }
    let path = write("later-specks.pdf", &testgen::write_pdf(&pages, &[], &[]));
    let (tx, rx) = mpsc::channel();
    let report = std::thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let mut d = MupdfDocument::open(&path, opts("columns=2")).unwrap();
            layout(&mut d, 1272, 1696);
            let _ = tx.send(format!("opened: plan {:?}, {} screens", d.plan(), d.page_count().pages));
            let last = d.page_count().pages - 1;
            let mut c = Canvas::new(1272, 1696);
            d.render_page(last, &mut c).unwrap();
            let _ = tx.send(format!("rendered screen {last}"));
        })
        .unwrap();
    let limit = Duration::from_secs(20 * slow());
    let opened = rx.recv_timeout(limit).expect("open did not return");
    eprintln!("{opened}");
    let turned = rx.recv_timeout(limit);
    assert!(turned.is_ok(), "turning to the speck page (page 4) did not return within {limit:?}");
    drop(report);
}

/// Odd page shapes give a narrow analysis image: a blank 4 x 600 pt page (2 columns) and a
/// blank 612 x 100000 pt page (600 px tall, 4 px wide).
#[test]
fn columns_mode_opens_a_blank_tall_narrow_page() {
    let mut cases: Vec<Case> = Vec::new();
    for (w, h, o) in [
        (4.0f32, 600.0f32, "columns=2"),
        (612.0, 100000.0, "columns=2"),
        (8.0, 600.0, "columns=4"),
        (612.0, 46000.0, "columns=4"),
        (4.0, 600.0, "crop=1,columns=2,fit=width"),
    ] {
        let path = write(&format!("narrow-{w}x{h}.pdf"), &testgen::write_pdf(&[PdfPage::new(w, h)], &[], &[]));
        let f: Box<dyn FnOnce() -> String + Send> = Box::new(move || {
            let mut d = MupdfDocument::open(&path, opts(o)).unwrap();
            layout(&mut d, 1272, 1696);
            format!("bound {:?}, plan {:?}", d.page_bounds(0), d.plan())
        });
        cases.push((format!("blank {w} x {h} pt, '{o}'"), f));
    }
    assert_all_returned("blank narrow page", &survey(20, cases));
}

/// Controls that must keep working: the same speck page without columns, with crop only,
/// and with columns on a normal two-column page.
#[test]
fn speck_page_without_columns_still_opens() {
    let pt = |px: f32| px * 33.0 / 25.0;
    let mut p = PdfPage::letter();
    p.rect(pt(200.0), pt(100.0), pt(4.0), pt(1.0), 0.0);
    p.rect(pt(200.0), pt(400.0), pt(4.0), pt(1.0), 0.0);
    let path = write("two-specks-control.pdf", &testgen::write_pdf(&[p], &[], &[]));
    for o in ["", "crop=1", "crop=1,fit=width", "zoom=2"] {
        let path = path.clone();
        let line = within(20, &format!("speck page with '{o}'"), move || {
            let mut d = MupdfDocument::open(&path, opts(o)).unwrap();
            layout(&mut d, 1272, 1696);
            let mut c = Canvas::new(1272, 1696);
            d.render_page(0, &mut c).unwrap();
            format!("'{o}': plan {:?}, content {:?}", d.plan(), d.content_box(0))
        });
        eprintln!("{line}");
    }
}

// --- 2. TOC and link targets past the last page ------------------------------------------

/// A one-page PDF whose outline lists `bad` (outline item bodies without the tree keys)
/// and then a good entry "Good" that points at page 0. Its page has three links that point
/// past the end: an explicit destination with integer page 999999 (without and with
/// coordinates) and a URI action `#page=2147483647`.
fn pdf_with_targets_past_the_end(name: &str, bad: &[&str]) -> PathBuf {
    let content = "BT /F1 12 Tf 72 700 Td (Target page text) Tj ET\nBT /F1 12 Tf 72 600 Td (Link one) Tj ET\n\
                   BT /F1 12 Tf 72 500 Td (Link two) Tj ET\nBT /F1 12 Tf 72 400 Td (Link three) Tj ET";
    // Objects: 1 catalog, 2 pages, 3 page, 4 font, 5 content, 6-8 links, 9 outlines root,
    // 10.. items (bad ones, then Good).
    let n = bad.len() + 1;
    let first = 10;
    let last = first + n - 1;
    let mut objs = vec![
        "<< /Type /Catalog /Pages 2 0 R /Outlines 9 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> \
         /Contents 5 0 R /Annots [6 0 R 7 0 R 8 0 R] >>"
            .to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        stream(content),
        "<< /Type /Annot /Subtype /Link /Rect [70 595 200 615] /Border [0 0 0] /Dest [999999 /Fit] >>".to_string(),
        "<< /Type /Annot /Subtype /Link /Rect [70 495 200 515] /Border [0 0 0] /A << /S /GoTo /D [999999 /XYZ 5 5 0] >> >>"
            .to_string(),
        "<< /Type /Annot /Subtype /Link /Rect [70 395 200 415] /Border [0 0 0] /A << /S /URI /URI (#page=2147483647) >> >>"
            .to_string(),
        format!("<< /Type /Outlines /First {first} 0 R /Last {last} 0 R /Count {n} >>"),
    ];
    let mut items: Vec<String> = bad.iter().map(|b| b.to_string()).collect();
    items.push("/Title (Good) /Dest [3 0 R /XYZ 0 720 0]".to_string());
    for (k, body) in items.iter().enumerate() {
        let id = first + k;
        let prev = if k > 0 { format!(" /Prev {} 0 R", id - 1) } else { String::new() };
        let next = if k + 1 < n { format!(" /Next {} 0 R", id + 1) } else { String::new() };
        objs.push(format!("<< {body} /Parent 9 0 R{prev}{next} >>"));
    }
    write(name, &raw_pdf(&objs))
}

const PAST_FIT: &str = "/Title (Past the end, fit) /Dest [999999 /Fit]";
const PAST_XYZ: &str = "/Title (Past the end, xyz) /Dest [999999 /XYZ 10 10 0]";
const PAST_URI: &str = "/Title (Past the end, uri) /A << /S /URI /URI (#page=2147483647) >>";

/// What the reader gets from a PDF with outline entries that point past the end:
/// - every `TocEntry::page` must be a real screen (or `None`);
/// - one bad entry must not drop the good ones.
///
/// Each bad entry is tried alone (before "Good") and all together.
#[test]
#[ignore = "verifier (minor): one outline entry with a destination past the end empties the whole TOC; others give screens past the end"]
fn toc_and_link_targets_past_the_last_page_stay_in_range() {
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("fit", vec![PAST_FIT]),
        ("xyz", vec![PAST_XYZ]),
        ("uri", vec![PAST_URI]),
        ("all", vec![PAST_FIT, PAST_XYZ, PAST_URI]),
        // A bookmark left behind by a tool that deleted its page: the reference names a
        // free object, or an object that is not a page.
        ("deleted-page", vec!["/Title (Deleted page) /Dest [99 0 R /XYZ 0 700 0]"]),
        ("not-a-page", vec!["/Title (Font, not a page) /Dest [4 0 R /XYZ 0 700 0]"]),
    ];
    let mut out = String::new();
    let mut bad = Vec::new();
    for (label, items) in cases {
        let path = pdf_with_targets_past_the_end(&format!("past-end-{label}.pdf"), &items);
        let (o2, b2) = within(30, &format!("targets past the end ({label})"), move || {
            let mut out = String::new();
            let mut bad = Vec::new();
            for o in ["", "fit=width", "zoom=3"] {
                let mut d = MupdfDocument::open(&path, opts(o)).unwrap();
                layout(&mut d, 1272, 1696);
                let screens = d.page_count().pages;
                let toc = d.toc();
                let _ = writeln!(out, "[{label}] '{o}': {screens} screens, {} outline entries, {} toc entries", d.outline().len(), toc.len());
                if !toc.iter().any(|e| e.title == "Good") {
                    bad.push(format!("[{label}] '{o}': the good TOC entry is missing ({} entries)", toc.len()));
                }
                for e in &toc {
                    let _ = writeln!(out, "  toc {:?} -> {} screen {:?}", e.title, e.position, e.page);
                    if let Some(p) = e.page {
                        if p >= screens {
                            bad.push(format!("[{label}] '{o}': toc {:?} -> screen {p} of {screens}", e.title));
                            // What the reader does next: jump there.
                            let mut c = Canvas::new(1272, 1696);
                            let _ = writeln!(out, "    render_page({p}) -> {:?}", d.render_page(p, &mut c).err());
                        }
                    }
                    let _ = writeln!(out, "    page_of -> {:?}", d.page_of(&e.position));
                }
                let pos = d.position_of_page(0).unwrap();
                for l in d.links(&pos).unwrap() {
                    let _ = writeln!(out, "  link {:?} -> {:?} rect {:?}", l.text, l.target, l.rect);
                    if let LinkTarget::Internal(t) = &l.target {
                        if let Ok(s) = d.page_of(t) {
                            if s >= screens {
                                bad.push(format!("[{label}] '{o}': link {:?} -> screen {s} of {screens}", l.text));
                            }
                        }
                    }
                }
            }
            (out, bad)
        });
        out.push_str(&o2);
        bad.extend(b2);
    }
    eprintln!("{out}");
    assert!(bad.is_empty(), "{bad:#?}");
}

// --- 3. CBZ entries that are not pages ---------------------------------------------------

/// Comic archives carry metadata files: ComicInfo.xml, Thumbs.db, desktop.ini, .DS_Store,
/// a text file, folders. Only the images are pages, in natural order; a CBZ without images
/// opens with 0 pages and every call fails cleanly.
#[test]
fn cbz_metadata_entries_are_not_pages_and_an_imageless_cbz_fails_cleanly() {
    let entries = vec![
        ("ComicInfo.xml", b"<?xml version=\"1.0\"?><ComicInfo><Title>x</Title></ComicInfo>".to_vec()),
        ("Thumbs.db", vec![0xd0, 0xcf, 0x11, 0xe0, 0, 0, 0, 0]),
        ("desktop.ini", b"[.ShellClassInfo]\n".to_vec()),
        (".DS_Store", vec![0, 0, 0, 1, b'B', b'u', b'd', b'1']),
        ("readme.txt", b"scanned by nobody".to_vec()),
        ("Vol 1/", Vec::new()),
        ("Vol 1/10.png", testgen::comic_page(10)),
        ("Vol 1/9.png", testgen::comic_page(9)),
        ("Vol 1/info.nfo", b"nfo".to_vec()),
    ];
    let path = write("metadata.cbz", &testgen::write_zip(&entries));
    let d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
    let widths: Vec<f32> = (0..d.doc_pages()).map(|p| d.page_bounds(p).unwrap().w()).collect();
    eprintln!("metadata CBZ: {} pages, widths {widths:?}", d.doc_pages());
    assert_eq!(d.doc_pages(), 2, "only the two images are pages");

    let only_meta: Vec<(&str, Vec<u8>)> = entries.iter().filter(|e| !e.0.ends_with(".png")).cloned().collect();
    let path = write("no-images.cbz", &testgen::write_zip(&only_meta));
    let r = within(20, "CBZ without images", move || match MupdfDocument::open(&path, FixedOptions::default()) {
        Err(e) => format!("refused: {e}"),
        Ok(mut d) => {
            let mut s = format!("opened, {} pages", d.doc_pages());
            for (w, h) in [(1272, 1696), (1696, 1272)] {
                layout(&mut d, w, h);
                let mut c = Canvas::new(w as usize, h as usize);
                let _ = write!(
                    s,
                    "; {w}x{h}: screens {}, render {:?}, position {:?}, thumbnail {:?}, search {:?}",
                    d.page_count().pages,
                    d.render_page(0, &mut c).is_err(),
                    d.position_of_page(0).is_err(),
                    d.thumbnail(100, 100).is_err(),
                    d.search("x", 5).map(|h| h.len())
                );
                d.set_current_page(5);
                let _ = write!(s, ", current {}", d.current_page());
            }
            d.set_options(opts("crop=1,columns=2,fit=width"));
            let _ = write!(s, "; columns: screens {}", d.page_count().pages);
            s
        }
    });
    eprintln!("no images: {r}");
}

// --- 4. Public helpers with out-of-range screens -----------------------------------------

/// `pages_of_screen`, `pieces`, `highlight_rects`, `first_screen_of`, `screen_showing` and
/// `tile_cell` are public; a caller (the reader, a plugin) may pass a stale or huge index.
/// They must not panic (an overflow panics in debug builds and wraps in release).
#[test]
#[ignore = "verifier (minor, debug builds only): spread/tile helpers overflow on usize::MAX screens or pages"]
fn public_helpers_take_huge_indices() {
    let path = write("helpers.cbz", &testgen::fixture_cbz(5));
    let report = within(30, "huge indices", move || {
        let mut out = Vec::new();
        for (w, h, o) in [(1696u32, 1272u32, ""), (1696, 1272, "cover=0"), (1272, 1696, "fit=width"), (1272, 1696, "zoom=2")] {
            let mut d = MupdfDocument::open(&path, opts(o)).unwrap();
            layout(&mut d, w, h);
            let plan = d.plan();
            for &i in &[usize::MAX, usize::MAX / 2 + 1, u32::MAX as usize] {
                let pos = ink_doc::PagePosition { page: 0, quads: Vec::new() };
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (
                        d.pages_of_screen(i).len(),
                        d.pieces(i).len(),
                        d.highlight_rects(i, &pos).len(),
                        d.tile_cell(i).is_some(),
                        d.first_screen_of(i),
                        d.screen_showing(i, 1.0, 1.0),
                    )
                }));
                out.push(format!("{plan:?} index {i}: {}", if r.is_ok() { "ok".into() } else { "PANIC".to_string() }));
            }
        }
        out
    });
    for l in &report {
        eprintln!("{l}");
    }
    assert!(report.iter().all(|l| !l.ends_with("PANIC")), "{report:#?}");
}

/// A stored position (sidecar, plugin) with a page past the end: every entry point that
/// takes a position fails cleanly.
#[test]
fn positions_past_the_end_fail_cleanly() {
    let path = write("pos.pdf", &testgen::fixture_pdf());
    let mut d = MupdfDocument::open(&path, opts("fit=width,columns=2,crop=1")).unwrap();
    layout(&mut d, 1272, 1696);
    for page in [d.doc_pages() as u32, u32::MAX] {
        let pos = Position::Page(ink_doc::PagePosition { page, quads: vec![ink_doc::Quad { x0: 1.0, y0: 1.0, x1: 2.0, y1: 2.0 }] });
        assert!(d.page_of(&pos).is_err(), "page_of {page}");
        assert!(d.links(&pos).is_err(), "links {page}");
        assert!(d.read_around(&pos, 100).is_err(), "read_around {page}");
    }
    let xp: Position = "/body/DocFragment[2]/body/p[3]/text().5".parse().unwrap();
    assert!(d.page_of(&xp).is_err(), "an xpointer is not a page position");
}

// --- 5. A small file that draws a lot ----------------------------------------------------

/// Peak resident set size of this process in MiB (Linux /proc, macOS getrusage).
fn peak_rss_mib() -> Option<f64> {
    if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
        let kb: f64 = s.lines().find(|l| l.starts_with("VmHWM:"))?.split_whitespace().nth(1)?.parse().ok()?;
        return Some(kb / 1024.0);
    }
    #[cfg(target_os = "macos")]
    {
        #[repr(C)]
        struct RUsage {
            times: [i64; 4],
            maxrss: i64,
            rest: [i64; 13],
        }
        extern "C" {
            fn getrusage(who: i32, usage: *mut RUsage) -> i32;
        }
        let mut r = RUsage { times: [0; 4], maxrss: 0, rest: [0; 13] };
        // SAFETY: RUSAGE_SELF with a buffer of the size of macOS' `struct rusage`.
        if unsafe { getrusage(0, &mut r) } == 0 {
            return Some(r.maxrss as f64 / (1u64 << 20) as f64);
        }
    }
    None
}

/// A one-page PDF of a few kB whose page draws `outer` x `inner` small rectangles: the page
/// paints form XObject Fm0 `outer` times, Fm0 paints Fm1 `inner` times, Fm1 is one
/// rectangle. MuPDF records every drawn rectangle in the page's display list.
fn fanout_pdf(outer: usize, inner: usize) -> Vec<u8> {
    let calls = |n: usize, name: &str| -> String {
        let mut s = String::new();
        for i in 0..n {
            let _ = writeln!(s, "q 1 0 0 1 {} {} cm /{name} Do Q", (i % 40) as f32 * 0.5, (i / 40 % 40) as f32 * 0.5);
        }
        s
    };
    let objs = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Fm0 5 0 R >> >> /Contents 4 0 R >>"
            .to_string(),
        stream(&calls(outer, "Fm0")),
        {
            let c = calls(inner, "Fm1");
            format!(
                "<< /Type /XObject /Subtype /Form /BBox [0 0 612 792] /Resources << /XObject << /Fm1 6 0 R >> >> /Length {} >>\nstream\n{c}\nendstream",
                c.len()
            )
        },
        {
            let c = "0 g 100 100 0.3 0.3 re f";
            format!("<< /Type /XObject /Subtype /Form /BBox [0 0 612 792] /Length {} >>\nstream\n{c}\nendstream", c.len())
        },
    ];
    raw_pdf(&objs)
}

/// Measurement: time and peak memory to open, render and extract text of a page that
/// draws 10^6 rectangles from a file of about 60 kB. MuPDF has no abort in the shim (no
/// fz_cookie), so the reader waits for the whole page. Logged, not asserted beyond a
/// generous time limit.
#[test]
#[ignore = "slow: run with --ignored before a release"]
fn xobject_fanout_page_time_and_memory_are_measured() {
    let bytes = fanout_pdf(1000, 1000);
    let size = bytes.len();
    let path = write("fanout.pdf", &bytes);
    let before = peak_rss_mib();
    let line = within(120, "10^6-rectangle page", move || {
        let t = Instant::now();
        let mut d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
        layout(&mut d, 1272, 1696);
        let t_open = t.elapsed();
        let mut c = Canvas::new(1272, 1696);
        let t = Instant::now();
        let r = d.render_page(0, &mut c);
        let t_render = t.elapsed();
        let t = Instant::now();
        let n = d.page_text(0).map(|p| p.chars.len());
        let t_text = t.elapsed();
        format!("file {size} bytes: open {t_open:?}, render {t_render:?} ({:?}), text {t_text:?} ({n:?} chars), display lists {}", r.err(), d.display_lists())
    });
    eprintln!("fanout: {line}; peak RSS {before:?} -> {:?} MiB", peak_rss_mib());
}
