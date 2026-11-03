//! Verifier attacks (cycle 1) on ink-mupdf: hostile and odd PDF/CBZ files, encrypted
//! PDFs, extreme page sizes and zoom, state fuzz (layout, rotation and option changes
//! between renders), a 2000-page PDF.
//!
//! Every fixture is generated at run time, so the test binary also runs on the Kindle
//! emulator. Cases that can kill the process (stack overflow, out of memory) run in a child
//! process: the parent runs this binary again with `--exact child_... --ignored` and
//! `INK_VERIFY_CHILD=1`, so a signal shows up as a failed exit status instead of killing
//! the test harness.
//!
//! Tests marked `#[ignore = "verifier: ..."]` fail today; each one is an open finding. The
//! cycle-1 findings (CBZ dot names, tile caps, 32-bit screen count, damaged-page search,
//! free zoom/pan, stored quads, round trips, option drift, column slivers) are fixed and
//! run by default.
//! `#[ignore = "verifier (minor, ...)"]` marks a failing test for a minor finding (inside
//! MuPDF, or debug builds only). Tests marked `#[ignore = "child body ..."]` are only the
//! bodies of child processes. `INK_VERIFY_LONG=1` makes the two fuzz tests run longer.
//!
//! Run every finding: `cargo test -p ink-mupdf --test verify_attacks -- --include-ignored`.

use ink_device::{Canvas, Point};
use ink_doc::{DocError, Document, LayoutSettings, PagePosition, Position, Quad};
use ink_mupdf::{testgen, FixedOptions, MupdfDocument, RenderSpec};
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
    let dir = std::env::temp_dir().join(format!("ink-mupdf-verify-{}", std::process::id()));
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

fn is_child() -> bool {
    std::env::var_os("INK_VERIFY_CHILD").is_some()
}

/// Runs the ignored test `name` of this binary in a child process. Returns (passed, log).
fn child(name: &str) -> (bool, String) {
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(exe)
        .args(["--exact", name, "--ignored", "--nocapture", "--test-threads=1"])
        .env("INK_VERIFY_CHILD", "1")
        .output()
        .expect("run child");
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let tail: Vec<&str> = text.lines().rev().take(30).collect();
    let log = format!("status {:?}\n{}", out.status, tail.into_iter().rev().collect::<Vec<_>>().join("\n"));
    (out.status.success() && text.contains("1 passed"), log)
}

/// Peak resident set size of this process in MiB.
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

/// A PDF from raw object bodies (object `i + 1` is `objs[i]`) with a classic xref table.
fn raw_pdf(objs: &[Vec<u8>], trailer_extra: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offs = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let mut x = format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1);
    for o in &offs {
        let _ = writeln!(x, "{o:010} 00000 n ");
    }
    let _ = write!(x, "trailer\n<< /Size {} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1);
    out.extend_from_slice(x.as_bytes());
    out
}

fn stream(content: &[u8], dict_extra: &str) -> Vec<u8> {
    let mut v = format!("<< /Length {}{dict_extra} >>\nstream\n", content.len()).into_bytes();
    v.extend_from_slice(content);
    v.extend_from_slice(b"\nendstream");
    v
}

const CATALOG: &str = "<< /Type /Catalog /Pages 2 0 R >>";
const FONT: &str = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";

/// Text drawing commands at `(x, y)` (PDF coordinates, y up).
fn text_at(x: f32, y: f32, s: &str) -> String {
    format!("BT /F1 12 Tf {x} {y} Td ({s}) Tj ET\n")
}

/// Pages with the given MediaBox texts and contents. `count` replaces `/Count`, `kids`
/// replaces the Kids array (`{refs}` in it stands for the real page references).
fn pdf_pages(pages: &[(&str, String)], count: Option<&str>, kids: Option<&str>) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![CATALOG.into(), Vec::new(), FONT.into()];
    let mut refs = Vec::new();
    for (media, content) in pages {
        let p = objs.len() + 1;
        refs.push(format!("{p} 0 R"));
        objs.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [{media}] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                p + 1
            )
            .into_bytes(),
        );
        objs.push(stream(content.as_bytes(), ""));
    }
    let n = pages.len().to_string();
    let kids = kids.map(|k| k.replace("{refs}", &refs.join(" "))).unwrap_or_else(|| refs.join(" "));
    objs[1] = format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", count.unwrap_or(&n)).into_bytes();
    raw_pdf(&objs, "")
}

fn letter_page(words: &str) -> (&'static str, String) {
    ("0 0 612 792", format!("{}0 0 0 rg 72 600 200 40 re f\n", text_at(72.0, 700.0, words)))
}

fn layout(d: &mut MupdfDocument, w: u32, h: u32) {
    d.set_layout(&LayoutSettings::for_screen(w, h)).unwrap();
}

fn open_opts(path: &std::path::Path, opts: &str) -> ink_doc::Result<MupdfDocument> {
    MupdfDocument::open(path, FixedOptions::parse(opts).unwrap())
}

/// Every screen: render, position, and back (no panic; errors allowed).
fn walk(d: &mut MupdfDocument, max: usize) -> (usize, usize) {
    let n = d.page_count().pages;
    let mut c = Canvas::new(d.layout().width as usize, d.layout().height as usize);
    let (mut ok, mut err) = (0, 0);
    let mut screens: Vec<usize> = (0..n.min(max)).collect();
    if n > max {
        screens.push(n - 1);
    }
    for s in screens {
        match d.render_page(s, &mut c) {
            Ok(()) => ok += 1,
            Err(_) => err += 1,
        }
        if let Ok(p) = d.position_of_page(s) {
            let _ = d.page_of(&p);
        }
    }
    (ok, err)
}

// --- MD5 and RC4 for the PDF standard security handler (revision 2) ----------------------

fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14,
        20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6,
        10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32).collect();
    let mut st = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let [mut a, mut b, mut c, mut d] = st;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        for (s, v) in st.iter_mut().zip([a, b, c, d]) {
            *s = s.wrapping_add(v);
        }
    }
    let mut out = [0u8; 16];
    for (i, s) in st.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
    }
    out
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: Vec<u8> = (0..=255).collect();
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|&b| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(s[i as usize]);
            s.swap(i as usize, j as usize);
            b ^ s[s[i as usize].wrapping_add(s[j as usize]) as usize]
        })
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08, 0x2E, 0x2E, 0x00,
    0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

fn padded(pw: &str) -> Vec<u8> {
    let mut v: Vec<u8> = pw.bytes().take(32).collect();
    let n = v.len();
    v.extend_from_slice(&PAD[..32 - n]);
    v
}

/// A one-page PDF encrypted with RC4-40 (standard security handler R2): user password
/// `user`, owner password `owner`; the page shows `words`.
fn encrypted_pdf(user: &str, owner: &str, words: &str) -> Vec<u8> {
    let id = *b"rinkyverify1";
    let p: i32 = -4;
    let okey = md5(&padded(if owner.is_empty() { user } else { owner }));
    let o = rc4(&okey[..5], &padded(user));
    let mut h = padded(user);
    h.extend_from_slice(&o);
    h.extend_from_slice(&p.to_le_bytes());
    h.extend_from_slice(&id);
    let key = md5(&h)[..5].to_vec();
    let u = rc4(&key, &PAD);
    let obj_key = |num: u32| {
        let mut k = key.clone();
        k.extend_from_slice(&num.to_le_bytes()[..3]);
        k.extend_from_slice(&[0, 0]);
        md5(&k)[..10].to_vec()
    };
    let content = text_at(72.0, 700.0, words);
    let enc = rc4(&obj_key(4), content.as_bytes());
    let objs: Vec<Vec<u8>> = vec![
        CATALOG.into(),
        "<< /Type /Pages /Kids [5 0 R] /Count 1 >>".into(),
        FONT.into(),
        stream(&enc, ""),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 4 0 R >>".into(),
        format!("<< /Filter /Standard /V 1 /R 2 /O <{}> /U <{}> /P {p} >>", hex(&o), hex(&u)).into_bytes(),
    ];
    raw_pdf(&objs, &format!("/Encrypt 6 0 R /ID [<{}> <{}>]", hex(&id), hex(&id)))
}

#[test]
fn md5_and_rc4_known_answers() {
    assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(hex(&rc4(b"Key", b"Plaintext")), "bbf316e8d940af0ad3");
}

// --- 1. CBZ entry names ------------------------------------------------------------------

fn cbz_pages(names: &[&str]) -> ink_doc::Result<usize> {
    let entries: Vec<(&str, Vec<u8>)> = names.iter().enumerate().map(|(i, n)| (*n, testgen::comic_page(i + 1))).collect();
    let path = write(&format!("names-{}.cbz", names.len() * 1000 + names[0].len()), &testgen::write_zip(&entries));
    MupdfDocument::open(&path, FixedOptions::default()).map(|d| d.doc_pages())
}

#[test]
fn cbz_junk_filter_still_skips_resource_forks() {
    // Control: the patch's purpose works, and dots inside folder names are harmless.
    let entries = vec![
        ("Vol.1/001.png", testgen::comic_page(1)),
        ("Vol.1/002.png", testgen::comic_page(2)),
        ("__MACOSX/Vol.1/._001.png", b"\x00\x05\x16\x07junk".to_vec()),
    ];
    let path = write("vol-dot.cbz", &testgen::write_zip(&entries));
    assert_eq!(MupdfDocument::open(&path, FixedOptions::default()).unwrap().doc_pages(), 2);
}

/// `find . -name '*.png' | zip comic.cbz -@` stores names as `./001.png`.
#[test]
fn cbz_entries_named_dot_slash_are_pages() {
    let n = cbz_pages(&["./001.png", "./002.png", "./003.png"]);
    assert!(matches!(n, Ok(3)), "CBZ with ./NNN.png entries: {n:?} (expected 3 pages)");
}

/// A folder whose name starts with a dot (".hack Vol 1/001.png", a real manga title).
#[test]
fn cbz_entries_in_a_folder_starting_with_a_dot_are_pages() {
    let n = cbz_pages(&[".hack Vol 1/001.png", ".hack Vol 1/002.png"]);
    assert!(matches!(n, Ok(2)), "CBZ with '.hack Vol 1/' entries: {n:?} (expected 2 pages)");
}

/// Natural order with numbers of different lengths, mixed case and two folders. Page k
/// (1-based, in the expected order) is an image 100 + 10 k pixels wide, so the page
/// widths show the order MuPDF chose.
#[test]
fn cbz_natural_order_with_tricky_names() {
    let cases: &[(&str, &[&str])] = &[
        ("lengths", &["p10.png", "p2.png", "p1.png", "p100.png", "p20.png"]),
        ("case", &["Page2.PNG", "page1.png", "PAGE10.png", "page3.Png"]),
        ("folders", &["b/1.png", "a/10.png", "a/2.png", "b/02a.png"]),
    ];
    // Expected order per case (indices into the name list above).
    let expected: &[&[usize]] = &[&[2, 1, 0, 4, 3], &[1, 0, 3, 2], &[2, 1, 0, 3]];
    let mut bad = Vec::new();
    for ((label, names), want) in cases.iter().zip(expected) {
        // Width of each entry = 100 + 10 * (its rank in the expected order + 1).
        let mut entries = Vec::new();
        for (i, name) in names.iter().enumerate() {
            let rank = want.iter().position(|&w| w == i).unwrap() + 1;
            let w = 100 + 10 * rank;
            entries.push((*name, testgen::png_gray(w, 150, &vec![200u8; w * 150])));
        }
        let path = write(&format!("natural-{label}.cbz"), &testgen::write_zip(&entries));
        let d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
        let widths: Vec<f32> = (0..d.doc_pages()).map(|p| d.page_bounds(p).unwrap().w()).collect();
        let sorted = widths.windows(2).all(|w| w[0] < w[1]);
        eprintln!("{label}: {} pages, widths in page order {widths:?}", d.doc_pages());
        if d.doc_pages() != names.len() || !sorted {
            bad.push(format!("{label}: {} of {} pages, widths {widths:?}", d.doc_pages(), names.len()));
        }
    }
    assert!(bad.is_empty(), "CBZ page order: {bad:?}");
}

// --- 2. Page sizes, tile counts ----------------------------------------------------------

/// Opens a one-page PDF with `media` and each option string; renders the last screen and
/// round-trips positions. Returns (option, page bound, plan per page, screens) per option.
fn size_case(name: &str, media: &str, opts: &[&'static str]) -> Vec<String> {
    let bytes = pdf_pages(&[(media, format!("{}0 0 0 rg 10 10 50 50 re f\n", text_at(20.0, 30.0, "corner")))], None, None);
    let path = write(&format!("{name}.pdf"), &bytes);
    let mut out = Vec::new();
    for &o in opts {
        let path = path.clone();
        let what = format!("{name} [{media}] '{o}'");
        let line = within(60, &what.clone(), move || {
            let mut d = open_opts(&path, o).unwrap_or_else(|e| panic!("{what}: open: {e}"));
            layout(&mut d, 300, 400);
            let pages = d.doc_pages() as u64;
            let per = d.plan().per_page() as u64;
            let screens = d.page_count().pages as u64;
            assert_eq!(screens, pages * per, "{what}: screen count {screens} != pages {pages} x tiles {per}");
            let last = d.page_count().pages - 1;
            let mut c = Canvas::new(300, 400);
            let r = d.render_page(last, &mut c);
            let pos = d.position_of_page(last).unwrap_or_else(|e| panic!("{what}: position_of_page({last}): {e}"));
            let back = d.page_of(&pos).unwrap_or_else(|e| panic!("{what}: page_of({pos}): {e}"));
            format!(
                "{what}: bound {:?}, plan {:?}, screens {screens}, last render {:?}, last {last} -> {pos} -> {back}",
                d.page_bounds(0).ok(),
                d.plan(),
                r.err()
            )
        });
        eprintln!("{line}");
        out.push(line);
    }
    out
}

const SIZE_OPTS: &[&str] = &["", "fit=width", "fit=width,zoom=8,columns=4", "crop=1,fit=width,zoom=8,rotate=90"];

#[test]
fn odd_mediaboxes_open_and_render() {
    size_case("tiny", "0 0 1 1", SIZE_OPTS);
    size_case("zero", "0 0 0 0", SIZE_OPTS);
    size_case("inverted", "612 792 0 0", SIZE_OPTS);
    size_case("big-square", "0 0 14400 14400", SIZE_OPTS);
    size_case("wide-strip", "0 0 14400 1", SIZE_OPTS);
}

#[test]
fn tall_mediaboxes_keep_tile_counts_sane() {
    // Heights far past the PDF limit (14400): MuPDF may or may not clamp them.
    for (name, media) in [
        ("tall-1e5", "0 0 612 100000"),
        ("tall-1e6", "0 0 612 1000000"),
        ("tall-1e9", "0 0 612 1000000000"),
        ("tall-1e30", "0 0 612 1e30"),
        ("huge-both", "-1e30 -1e30 1e30 1e30"),
        ("thin-tall", "0 0 1 14400"),
    ] {
        size_case(name, media, SIZE_OPTS);
    }
}

/// On 32-bit ARM `usize` is 32 bits: pages x tiles per page must stay below 2^32.
#[test]
fn screen_count_fits_32_bits_for_a_2000_page_tall_document() {
    let pages: Vec<(&str, String)> = (0..2000).map(|_| ("0 0 1 14400", text_at(0.0, 10.0, "x"))).collect();
    let path = write("tall2000.pdf", &pdf_pages(&pages, None, None));
    let line = within(120, "2000 tall pages", move || {
        let mut d = open_opts(&path, "fit=width,zoom=8,columns=4").unwrap();
        layout(&mut d, 1272, 1696);
        let total = d.doc_pages() as u64 * d.plan().per_page() as u64;
        eprintln!("2000 pages of 1 x 14400 pt: plan {:?}, pages x tiles = {total}, page_count() reports {}", d.plan(), d.page_count().pages);
        assert!(total <= u32::MAX as u64, "{total} screens do not fit a 32-bit usize (plan {:?})", d.plan());
        assert_eq!(d.page_count().pages as u64, total);
        format!("plan {:?}, screens {total}", d.plan())
    });
    eprintln!("{line}");
}

// --- 3. Zero and lying page counts -------------------------------------------------------

fn exercise(name: &'static str, bytes: Vec<u8>) -> String {
    let path = write(&format!("{name}.pdf"), &bytes);
    within(60, name, move || {
        let mut d = match MupdfDocument::open(&path, FixedOptions::default()) {
            Ok(d) => d,
            Err(e) => {
                assert!(matches!(e, DocError::Format(_) | DocError::Unsupported(_)), "{name}: {e:?}");
                return format!("{name}: open refused: {e}");
            }
        };
        layout(&mut d, 300, 400);
        let n = d.page_count().pages;
        let pages = d.doc_pages();
        let (ok, err) = walk(&mut d, 20);
        let _ = d.toc();
        let t = Instant::now();
        let s = d.search("zebra", 5);
        let search_time = t.elapsed();
        let thumb = d.thumbnail(60, 80).map(|c| (c.width, c.height));
        d.set_options(FixedOptions::parse("fit=width,crop=1,columns=2").unwrap());
        let (ok2, err2) = walk(&mut d, 5);
        layout(&mut d, 400, 300);
        d.set_current_page(usize::MAX);
        assert!(d.current_page() < d.page_count().pages.max(1), "{name}: current page past the end");
        format!(
            "{name}: pages {pages}, screens {n}, renders ok {ok} err {err}, after options ok {ok2} err {err2}, search {:?} in {search_time:?}, thumbnail {thumb:?}",
            s.map(|h| h.len())
        )
    })
}

#[test]
fn zero_and_lying_page_counts_fail_cleanly() {
    let one = vec![letter_page("only page")];
    let mut log = Vec::new();
    log.push(exercise("count-zero", pdf_pages(&[], None, None)));
    log.push(exercise("count-negative", pdf_pages(&one, Some("-5"), None)));
    log.push(exercise("count-int-max", pdf_pages(&one, Some("2147483647"), None)));
    log.push(exercise("count-huge", pdf_pages(&one, Some("99999999999999"), None)));
    log.push(exercise("kids-cycle", pdf_pages(&one, Some("3"), Some("{refs} 2 0 R"))));
    log.push(exercise("kids-junk", pdf_pages(&one, Some("4"), Some("{refs} 42 (text) null"))));
    for l in &log {
        eprintln!("{l}");
    }
}

// --- 4. One damaged page -----------------------------------------------------------------

/// Seven Kids entries: index 2 points at a missing object (99 0 R); "needle" is on
/// page index 5 (the fifth real page).
fn pdf_with_a_missing_page() -> PathBuf {
    let mut pages: Vec<(&str, String)> = (0..6).map(|i| letter_page(&format!("page {i} filler words"))).collect();
    pages[4] = letter_page("the needle is here");
    // Page i is object 4 + 2 i.
    let bytes = pdf_pages(&pages, Some("7"), Some("4 0 R 6 0 R 99 0 R 8 0 R 10 0 R 12 0 R 14 0 R"));
    write("missing-page.pdf", &bytes)
}

#[test]
fn a_damaged_page_renders_as_an_error_and_the_rest_still_work() {
    let path = pdf_with_a_missing_page();
    let line = within(30, "missing page", move || {
        let mut d = open_opts(&path, "").unwrap();
        layout(&mut d, 300, 400);
        let mut c = Canvas::new(300, 400);
        let results: Vec<bool> = (0..d.page_count().pages).map(|s| d.render_page(s, &mut c).is_ok()).collect();
        assert!(results.iter().filter(|r| **r).count() >= 5, "good pages render: {results:?}");
        format!("pages {}, render ok per screen {results:?}", d.doc_pages())
    });
    eprintln!("{line}");
}

/// Three pages: page 0 "page zero", page 1 a Pages node whose only kid is the root again
/// (MuPDF cannot load it: "cycle in page tree"), page 2 "the needle is here".
fn pdf_with_an_unloadable_page() -> PathBuf {
    let objs: Vec<Vec<u8>> = vec![
        CATALOG.into(),
        "<< /Type /Pages /Kids [4 0 R 8 0 R 6 0 R] /Count 3 >>".into(),
        FONT.into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>".into(),
        stream(text_at(72.0, 700.0, "page zero words").as_bytes(), ""),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 7 0 R >>".into(),
        stream(text_at(72.0, 700.0, "the needle is here").as_bytes(), ""),
        "<< /Type /Pages /Parent 2 0 R /Kids [2 0 R] /Count 1 >>".into(),
    ];
    write("unloadable-page.pdf", &raw_pdf(&objs, ""))
}

/// The missing-object fixture above is repaired by MuPDF; a page-tree cycle is not.
#[test]
fn an_unloadable_page_fails_alone_when_rendered() {
    let path = pdf_with_an_unloadable_page();
    let line = within(30, "unloadable page render", move || {
        let mut d = open_opts(&path, "").unwrap();
        layout(&mut d, 300, 400);
        let mut c = Canvas::new(300, 400);
        let results: Vec<String> =
            (0..d.page_count().pages).map(|s| d.render_page(s, &mut c).map(|_| "ok".to_string()).unwrap_or_else(|e| e.to_string())).collect();
        assert_eq!(d.doc_pages(), 3);
        assert_eq!(results[0], "ok");
        assert_eq!(results[2], "ok", "the page after the bad one renders");
        format!("render per screen {results:?}")
    });
    eprintln!("{line}");
}

#[test]
fn search_skips_a_damaged_page() {
    let path = pdf_with_an_unloadable_page();
    within(30, "search past an unloadable page", move || {
        let mut d = open_opts(&path, "").unwrap();
        layout(&mut d, 300, 400);
        let hits = d.search("needle", 10);
        assert!(matches!(&hits, Ok(h) if h.len() == 1), "search over a PDF with one unloadable page: {hits:?}");
        let pos: Position = "page:0".parse().unwrap();
        let around = d.read_around(&pos, 400);
        assert!(around.is_ok(), "read_around next to an unloadable page: {around:?}");
    });
}

// --- 5. Encrypted PDFs -------------------------------------------------------------------

#[test]
fn encrypted_pdf_needs_the_user_or_owner_password() {
    let path = write("encrypted.pdf", &encrypted_pdf("user", "owner", "secretword here"));
    let none = MupdfDocument::open(&path, FixedOptions::default());
    assert!(matches!(&none, Err(DocError::Unsupported(m)) if m.contains("password")), "no password: {:?}", none.err());
    let wrong = MupdfDocument::open_with_password(&path, FixedOptions::default(), Some("wrong"));
    assert!(matches!(wrong, Err(DocError::Unsupported(_))), "wrong password: {:?}", wrong.err());
    for pw in ["user", "owner"] {
        let mut d = MupdfDocument::open_with_password(&path, FixedOptions::default(), Some(pw))
            .unwrap_or_else(|e| panic!("password {pw}: {e}"));
        let hits = d.search("secretword", 5).unwrap();
        assert_eq!(hits.len(), 1, "password {pw}: decrypted text is searchable");
        let mut c = Canvas::new(d.layout().width as usize, d.layout().height as usize);
        d.render_page(0, &mut c).unwrap();
        assert!(c.pixels.iter().any(|&v| v < 100), "password {pw}: text is drawn");
    }
}

#[test]
fn owner_only_encryption_opens_without_a_password() {
    let path = write("owner-only.pdf", &encrypted_pdf("", "owner", "openword here"));
    let mut d = MupdfDocument::open(&path, FixedOptions::default()).expect("empty user password opens");
    assert_eq!(d.search("openword", 5).unwrap().len(), 1);
}

#[test]
fn broken_encryption_dictionaries_fail_cleanly() {
    let mut log = Vec::new();
    for (name, dict) in [
        ("enc-unknown-filter", "<< /Filter /NoSuchHandler /V 1 /R 2 /O <00> /U <00> /P -4 >>"),
        ("enc-r6-garbage", "<< /Filter /Standard /V 5 /R 6 /Length 256 /O <0011> /U <2233> /OE <44> /UE <55> /Perms <66> /P -4 >>"),
        ("enc-not-a-dict", "42"),
    ] {
        let objs: Vec<Vec<u8>> = vec![
            CATALOG.into(),
            "<< /Type /Pages /Kids [5 0 R] /Count 1 >>".into(),
            FONT.into(),
            stream(text_at(72.0, 700.0, "x").as_bytes(), ""),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 4 0 R >>".into(),
            dict.into(),
        ];
        let path = write(&format!("{name}.pdf"), &raw_pdf(&objs, "/Encrypt 6 0 R /ID [<00112233> <00112233>]"));
        let r = within(20, name, move || match MupdfDocument::open(&path, FixedOptions::default()) {
            Ok(mut d) => {
                layout(&mut d, 200, 300);
                format!("opened, walk {:?}", walk(&mut d, 3))
            }
            Err(e) => format!("refused: {e}"),
        });
        log.push(format!("{name}: {r}"));
    }
    eprintln!("{}", log.join("\n"));
}

// --- 6. Outline cycles and depth, page tree depth, drawing recursion ---------------------

/// One page plus an outline: items are objects 6.. and `items(k)` gives item k's body.
fn pdf_with_outline(root: &str, items: &[String]) -> Vec<u8> {
    let mut objs: Vec<Vec<u8>> = vec![
        "<< /Type /Catalog /Pages 2 0 R /Outlines 5 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".into(),
        stream(b"0 g 72 600 100 40 re f", ""),
        root.into(),
    ];
    objs.extend(items.iter().map(|s| s.clone().into_bytes()));
    raw_pdf(&objs, "")
}

#[test]
fn outline_cycles_terminate() {
    let items = vec![
        "<< /Title (A) /Parent 5 0 R /Dest [3 0 R /Fit] /Next 7 0 R /First 8 0 R /Last 8 0 R /Count 1 >>".to_string(),
        "<< /Title (B) /Parent 5 0 R /Dest [3 0 R /Fit] /Next 6 0 R /Prev 6 0 R >>".to_string(),
        "<< /Title (A1) /Parent 6 0 R /Dest [3 0 R /Fit] /First 6 0 R /Last 6 0 R /Next 8 0 R >>".to_string(),
    ];
    let path = write("outline-cycle.pdf", &pdf_with_outline("<< /Type /Outlines /First 6 0 R /Last 7 0 R /Count 3 >>", &items));
    let n = within(20, "outline cycle", move || {
        let d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
        d.toc().len()
    });
    assert!(n <= 100, "outline cycle gave {n} entries");
}

fn deep_outline(depth: usize) -> Vec<u8> {
    let items: Vec<String> = (0..depth)
        .map(|k| {
            let parent = if k == 0 { 5 } else { 5 + k };
            let down = if k + 1 < depth { format!(" /First {} 0 R /Last {} 0 R /Count 1", 7 + k, 7 + k) } else { String::new() };
            format!("<< /Title (L{k}) /Parent {parent} 0 R /Dest [3 0 R /Fit]{down} >>")
        })
        .collect();
    pdf_with_outline("<< /Type /Outlines /First 6 0 R /Last 6 0 R /Count 1 >>", &items)
}

fn open_deep_outline(depth: usize) {
    let path = write(&format!("outline-depth-{depth}.pdf"), &deep_outline(depth));
    let n = within(120, "deep outline", move || {
        let d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
        d.toc().len()
    });
    eprintln!("outline depth {depth}: {n} TOC entries");
}

#[test]
fn outline_nested_1000_deep_opens() {
    open_deep_outline(1000);
}

#[test]
#[ignore = "child body; run by outline_nested_100000_deep_does_not_crash"]
fn child_outline_100000_deep() {
    if is_child() {
        open_deep_outline(100_000);
    }
}

#[test]
#[ignore = "verifier (minor, MuPDF recursion): an outline nested 100000 deep overflows the stack at open"]
fn outline_nested_100000_deep_does_not_crash() {
    let (ok, log) = child("child_outline_100000_deep");
    assert!(ok, "opening a PDF whose outline is nested 100000 deep:\n{log}");
}

fn deep_page_tree(depth: usize) -> Vec<u8> {
    // Objects: 1 catalog, 2 root Pages, 3.. nested Pages nodes, then the page and contents.
    let mut objs: Vec<Vec<u8>> = vec![CATALOG.into(), Vec::new()];
    let page = 3 + depth;
    for k in 0..=depth {
        let me = 2 + k;
        let kid = if k == depth { page } else { me + 1 };
        let parent = if k == 0 { String::new() } else { format!(" /Parent {} 0 R", me - 1) };
        let body = format!("<< /Type /Pages /Kids [{kid} 0 R] /Count 1{parent} >>").into_bytes();
        if k == 0 {
            objs[1] = body;
        } else {
            objs.push(body);
        }
    }
    objs.push(format!("<< /Type /Page /Parent {} 0 R /MediaBox [0 0 612 792] /Contents {} 0 R >>", 2 + depth, page + 1).into_bytes());
    objs.push(stream(b"0 g 72 600 100 40 re f", ""));
    raw_pdf(&objs, "")
}

#[test]
#[ignore = "child body; run by page_tree_nested_100000_deep_does_not_crash"]
fn child_page_tree_100000_deep() {
    if is_child() {
        let path = write("pagetree-deep.pdf", &deep_page_tree(100_000));
        let r = within(120, "deep page tree", move || match MupdfDocument::open(&path, FixedOptions::default()) {
            Ok(mut d) => {
                layout(&mut d, 200, 300);
                format!("opened, {} pages, walk {:?}", d.doc_pages(), walk(&mut d, 2))
            }
            Err(e) => format!("refused: {e}"),
        });
        eprintln!("page tree depth 100000: {r}");
    }
}

#[test]
#[ignore = "verifier (minor, MuPDF recursion): a page tree nested 100000 deep overflows the stack at open"]
fn page_tree_nested_100000_deep_does_not_crash() {
    let (ok, log) = child("child_page_tree_100000_deep");
    assert!(ok, "PDF whose page tree is nested 100000 deep:\n{log}");
}

#[test]
fn recursive_xobjects_and_patterns_terminate() {
    // Page draws form X1; X1 draws itself. A tiling pattern P1 paints with itself.
    let objs: Vec<Vec<u8>> = vec![
        CATALOG.into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /XObject << /X1 5 0 R >> /Pattern << /P1 6 0 R >> >> >>".into(),
        stream(b"/X1 Do /Pattern cs /P1 scn 0 0 612 792 re f", ""),
        stream(b"/X1 Do 0 g 0 0 10 10 re f", " /Type /XObject /Subtype /Form /BBox [0 0 612 792] /Resources << /XObject << /X1 5 0 R >> >>"),
        stream(
            b"/Pattern cs /P1 scn 0 0 20 20 re f",
            " /Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 /Resources << /Pattern << /P1 6 0 R >> >>",
        ),
    ];
    let path = write("recursive-draw.pdf", &raw_pdf(&objs, ""));
    let r = within(60, "recursive drawing", move || {
        let mut d = MupdfDocument::open(&path, FixedOptions::parse("crop=1").unwrap()).unwrap();
        layout(&mut d, 300, 400);
        let w = walk(&mut d, 1);
        let s = d.search("x", 1).map(|h| h.len());
        format!("walk {w:?}, search {s:?}")
    });
    eprintln!("recursive drawing: {r}");
}

// --- 7. CBZ with bad and huge entries ----------------------------------------------------

#[test]
fn cbz_with_fake_empty_and_truncated_images() {
    let real = testgen::comic_page(1);
    let entries = vec![
        ("001.png", testgen::comic_page(1)),
        ("002.png", b"this is plain text, not a PNG".to_vec()),
        ("003.jpg", Vec::new()),
        ("004.png", real[..real.len() / 3].to_vec()),
        ("005.png", testgen::comic_page(5)),
        ("notes.txt", b"not a page".to_vec()),
        ("sub/", Vec::new()),
    ];
    let path = write("bad-entries.cbz", &testgen::write_zip(&entries));
    let line = within(60, "bad CBZ entries", move || {
        let mut d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
        layout(&mut d, 600, 900);
        let mut c = Canvas::new(600, 900);
        let ok: Vec<bool> = (0..d.page_count().pages).map(|s| d.render_page(s, &mut c).is_ok()).collect();
        let pages = d.doc_pages();
        assert!(ok.first() == Some(&true) && ok.last() == Some(&true), "good pages still render: {ok:?}");
        let thumb = d.thumbnail(100, 150).is_ok();
        // Landscape spreads with a bad page next to a good one.
        layout(&mut d, 1696, 1272);
        let spreads: Vec<bool> = (0..d.page_count().pages)
            .map(|s| {
                let mut c = Canvas::new(1696, 1272);
                d.render_page(s, &mut c).is_ok()
            })
            .collect();
        d.set_options(FixedOptions::parse("crop=1").unwrap());
        let crop = walk(&mut d, 10);
        format!("pages {pages}, portrait ok {ok:?}, thumbnail {thumb}, spreads ok {spreads:?}, crop walk {crop:?}")
    });
    eprintln!("{line}");
}

/// A zip whose only entry claims `claimed` bytes when inflated (method 8, tiny data).
fn zip_claiming(name: &str, claimed: u32) -> Vec<u8> {
    // Deflate stream of one empty stored block.
    let data: &[u8] = &[0x01, 0x00, 0x00, 0xff, 0xff];
    let mut z = testgen::write_zip(&[(name, data.to_vec())]);
    // Patch method (offset 8 local, 10 central) to deflate and the sizes to `claimed`.
    let le = claimed.to_le_bytes();
    z[8] = 8;
    z[22..26].copy_from_slice(&le);
    let cd = z.windows(4).position(|w| w == [0x50, 0x4b, 0x01, 0x02]).unwrap();
    z[cd + 10] = 8;
    z[cd + 24..cd + 28].copy_from_slice(&le);
    z
}

#[test]
#[ignore = "child body; run by cbz_entry_claiming_4_gib_fails_cleanly"]
fn child_cbz_claims_4_gib() {
    if is_child() {
        let path = write("claims-4g.cbz", &zip_claiming("001.png", 0xFFFF_FFF0));
        let r = within(60, "4 GiB claim", move || match MupdfDocument::open(&path, FixedOptions::default()) {
            Ok(mut d) => {
                layout(&mut d, 300, 400);
                format!("opened, pages {}, walk {:?}", d.doc_pages(), walk(&mut d, 2))
            }
            Err(e) => format!("refused: {e}"),
        });
        eprintln!("cbz claiming 4 GiB: {r}; peak RSS {:?} MiB", peak_rss_mib());
    }
}

#[test]
fn cbz_entry_claiming_4_gib_fails_cleanly() {
    let (ok, log) = child("child_cbz_claims_4_gib");
    assert!(ok, "CBZ entry that claims 4 GiB:\n{log}");
}

/// A white `side` x `side` gray PNG, written row by row (small file, huge image).
fn huge_png(side: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, side, side);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut header = enc.write_header().unwrap();
        let mut w = header.stream_writer().unwrap();
        let row = vec![255u8; side as usize];
        for _ in 0..side {
            std::io::Write::write_all(&mut w, &row).unwrap();
        }
        w.finish().unwrap();
        header.finish().unwrap();
    }
    buf
}

/// A plain `w` x `h` PNG (gray or RGB), written row by row.
fn plain_png(w: u32, h: u32, rgb: bool) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut buf, w, h);
        enc.set_color(if rgb { png::ColorType::Rgb } else { png::ColorType::Grayscale });
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut header = enc.write_header().unwrap();
        let mut sw = header.stream_writer().unwrap();
        let row: Vec<u8> = (0..w as usize * if rgb { 3 } else { 1 }).map(|i| (i % 251) as u8).collect();
        for _ in 0..h {
            std::io::Write::write_all(&mut sw, &row).unwrap();
        }
        sw.finish().unwrap();
        header.finish().unwrap();
    }
    buf
}

/// Measurement only: peak RSS growth for one realistic large scan page (6000 x 9000, a
/// 600 dpi A4-ish comic scan), gray and RGB, rendered at the Kindle screen size.
#[test]
#[ignore = "child body; run by cbz_large_scan_page_memory_is_measured"]
fn child_cbz_large_scan_page() {
    if is_child() {
        let rgb = std::env::var_os("INK_VERIFY_RGB").is_some();
        let png = plain_png(6000, 9000, rgb);
        let path = write("large-scan.cbz", &testgen::write_zip(&[("001.png", testgen::comic_page(1)), ("002.png", png.clone())]));
        let mut d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
        layout(&mut d, 1272, 1696);
        let mut c = Canvas::new(1272, 1696);
        d.render_page(0, &mut c).unwrap();
        let before = peak_rss_mib().unwrap_or(0.0);
        let t = Instant::now();
        let r = d.render_page(1, &mut c);
        let after = peak_rss_mib().unwrap_or(0.0);
        eprintln!(
            "large scan 6000x9000 {} (png {} bytes): render {:?} in {:?}; peak RSS {before:.0} -> {after:.0} MiB (+{:.0})",
            if rgb { "RGB" } else { "gray" },
            png.len(),
            r.as_ref().err(),
            t.elapsed(),
            after - before
        );
    }
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn cbz_large_scan_page_memory_is_measured() {
    for rgb in [false, true] {
        let exe = std::env::current_exe().unwrap();
        let mut cmd = std::process::Command::new(exe);
        cmd.args(["--exact", "child_cbz_large_scan_page", "--ignored", "--nocapture", "--test-threads=1"]).env("INK_VERIFY_CHILD", "1");
        if rgb {
            cmd.env("INK_VERIFY_RGB", "1");
        }
        let out = cmd.output().expect("run child");
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let line = text.lines().find(|l| l.starts_with("large scan")).unwrap_or("no measurement line").to_string();
        eprintln!("{line}");
        assert!(out.status.success(), "large scan page child failed:\n{text}");
    }
}

/// Check 6 end to end: 48 different 2000 x 3000 gray PNG pages, each rendered (with
/// prefetch of the next) in a fresh child process. Peak RSS after page 48 may exceed the
/// peak after page 12 by at most 24 MiB: the caches are full by then.
#[test]
#[ignore = "child body; run by memory_levels_off_over_many_cbz_pages"]
fn child_memory_over_many_pages() {
    if is_child() {
        let entries: Vec<(String, Vec<u8>)> = (0..48)
            .map(|i| {
                let mut px = vec![255u8; 2000 * 3000];
                for y in (i * 50)..(i * 50 + 200) {
                    px[y * 2000..y * 2000 + 2000].fill(0);
                }
                (format!("{:03}.png", i + 1), testgen::png_gray(2000, 3000, &px))
            })
            .collect();
        let refs: Vec<(&str, Vec<u8>)> = entries.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
        let path = write("many-pages.cbz", &testgen::write_zip(&refs));
        drop(refs);
        drop(entries);
        let mut d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
        layout(&mut d, 1272, 1696);
        let mut c = Canvas::new(1272, 1696);
        let mut at12 = 0.0;
        for s in 0..48 {
            d.render_page(s, &mut c).unwrap();
            let _ = d.prefetch(s + 1);
            if s == 11 {
                at12 = peak_rss_mib().unwrap_or(0.0);
            }
        }
        let at48 = peak_rss_mib().unwrap_or(0.0);
        let (used, cap) = d.cache_usage();
        eprintln!("many pages: peak RSS after 12 pages {at12:.0} MiB, after 48 pages {at48:.0} MiB; screen cache {used}/{cap} bytes, {} display lists", d.display_lists());
        assert!(at48 - at12 <= 24.0, "peak RSS still grows: {at12:.0} -> {at48:.0} MiB");
    }
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn memory_levels_off_over_many_cbz_pages() {
    let (ok, log) = child("child_memory_over_many_pages");
    eprintln!("{}", log.lines().find(|l| l.starts_with("many pages")).unwrap_or("no measurement line"));
    assert!(ok, "memory over many CBZ pages:\n{log}");
}

/// Peak RSS growth (MiB) when a CBZ page holds a 16384 x 16384 image.
const HUGE_SIDE: u32 = 16384;
/// What the documented caps allow (48 MiB store + 16 MiB screens + lists and text), with
/// room to spare.
const MEMORY_BUDGET_MIB: f64 = 160.0;

#[test]
#[ignore = "child body; run by cbz_huge_image_stays_within_the_memory_caps"]
fn child_cbz_huge_image() {
    if is_child() {
        let png = huge_png(HUGE_SIDE);
        let path = write("huge-image.cbz", &testgen::write_zip(&[("001.png", testgen::comic_page(1)), ("002.png", png.clone())]));
        let mut d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
        layout(&mut d, 1272, 1696);
        let mut c = Canvas::new(1272, 1696);
        d.render_page(0, &mut c).unwrap();
        let before = peak_rss_mib().unwrap_or(0.0);
        let t = Instant::now();
        let r = d.render_page(1, &mut c);
        let after = peak_rss_mib().unwrap_or(0.0);
        eprintln!(
            "huge image {HUGE_SIDE}x{HUGE_SIDE} (png {} bytes): render {:?} in {:?}; peak RSS {before:.0} -> {after:.0} MiB",
            png.len(),
            r.as_ref().err(),
            t.elapsed()
        );
        assert!(after - before <= MEMORY_BUDGET_MIB, "peak RSS grew {:.0} MiB for one page", after - before);
    }
}

#[test]
#[ignore = "verifier (minor, MuPDF decodes PNG in full): a 16384x16384 PNG page raises peak RSS by about 470 MiB"]
fn cbz_huge_image_stays_within_the_memory_caps() {
    let (ok, log) = child("child_cbz_huge_image");
    eprintln!("{log}");
    assert!(ok, "CBZ page with a {HUGE_SIDE}x{HUGE_SIDE} image:\n{log}");
}

// --- 8. Extreme free zoom and pan --------------------------------------------------------

#[test]
fn extreme_free_zoom_and_pan_return() {
    let path = write("fixture-zoom.pdf", &testgen::fixture_pdf());
    let specs: Vec<(f32, u16, f32, f32)> = vec![
        (1e4, 0, 0.0, 0.0),
        (1e4, 90, 5e5, 5e5),
        (1e7, 0, 0.0, 0.0),
        (f32::MAX, 0, 0.0, 0.0),
        (f32::INFINITY, 0, 0.0, 0.0),
        (f32::NAN, 0, 0.0, 0.0),
        (-3.0, 0, 0.0, 0.0),
        (0.0, 0, 0.0, 0.0),
        (2.0, 0, f32::NAN, f32::NAN),
        (2.0, 180, f32::INFINITY, f32::NEG_INFINITY),
        (2.0, 0, -1e30, 1e30),
        (1.5, 45, 10.0, 10.0),
        (1.5, 359, 10.0, 10.0),
        (1.5, 65535, 10.0, 10.0),
    ];
    let mut panics = Vec::new();
    for (zoom, rotate, x, y) in specs {
        let path = path.clone();
        let what = format!("render_page_at zoom {zoom} rotate {rotate} at ({x}, {y})");
        // A panic here aborts the reader in release builds (panic = "abort").
        let r = std::panic::catch_unwind(|| {
            within(30, &what.clone(), move || {
                let d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
                let mut c = Canvas::new(300, 400);
                for crop in [false, true] {
                    let _ = d.render_page_at(0, &RenderSpec { zoom, rotate, x, y, crop }, &mut c);
                }
                d.render_page_at(0, &RenderSpec { zoom, rotate, x, y, crop: false }, &mut c).map(|_| c.pixels.iter().filter(|&&v| v < 128).count())
            })
        });
        match r {
            Ok(r) => eprintln!("{what}: {r:?}"),
            Err(_) => {
                eprintln!("{what}: PANIC");
                panics.push(what);
            }
        }
    }
    let d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
    for (w, h) in [(0, 0), (1, 0), (0, 1), (1, 100_000)] {
        let t = d.thumbnail(w, h);
        assert!(t.as_ref().map(|c| c.width <= w.max(1) as usize && c.height <= h.max(1) as usize).unwrap_or(true), "thumbnail({w}, {h})");
    }
    assert!(panics.is_empty(), "render_page_at panicked for: {panics:?}");
}

/// Positions stored in a sidecar may hold any number: highlight rectangles and text
/// lookups must not panic on them.
#[test]
fn hostile_stored_quads_do_not_panic() {
    let path = write("fixture-quads.pdf", &testgen::fixture_pdf());
    for opts in ["", "zoom=2"] {
        let mut d = open_opts(&path, opts).unwrap();
        layout(&mut d, 1272, 1696);
        for q in [
            Quad { x0: -1e30, y0: -1e30, x1: 1e30, y1: 1e30 },
            Quad { x0: f32::NEG_INFINITY, y0: 0.0, x1: f32::INFINITY, y1: 10.0 },
            Quad { x0: 1e9, y0: 1e9, x1: 3e9, y1: 3e9 },
            Quad { x0: f32::NAN, y0: f32::NAN, x1: f32::NAN, y1: f32::NAN },
        ] {
            let pos = PagePosition { page: 0, quads: vec![q] };
            let what = format!("'{opts}' quad {q:?}");
            let rects = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.highlight_rects(0, &pos)));
            assert!(rects.is_ok(), "highlight_rects panicked for {what}");
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.text_at(&pos).map(|t| t.len())));
            assert!(text.is_ok(), "text_at panicked for {what}");
            let p = Position::Page(pos.clone());
            let back = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.page_of(&p)));
            assert!(back.is_ok(), "page_of panicked for {what}");
        }
        for s in ["page:0;inf,inf,inf,inf", "page:0;-1e38,-1e38,1e38,1e38", "page:4294967295", "page:0;NaN,1,2,3"] {
            if let Ok(p) = s.parse::<Position>() {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (d.page_of(&p).ok(), d.read_around(&p, 50).ok(), d.links(&p).ok().map(|l| l.len()))));
                assert!(r.is_ok(), "'{opts}' position {s} panicked");
            }
        }
    }
}

// --- 9. Rapid option, layout and rotation changes ----------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

const LAYOUTS: &[(u32, u32)] = &[(1, 1), (2000, 1), (1, 2000), (1272, 1696), (1696, 1272), (600, 800), (800, 600), (37, 1001)];
const OPTIONS: &[&str] = &[
    "",
    "fit=width",
    "zoom=2",
    "zoom=8,rotate=90",
    "crop=1",
    "crop=1,columns=2,fit=width",
    "columns=3,zoom=1.5",
    "spreads=always,rtl=1",
    "spreads=always,cover=0",
    "rotate=270,fit=width,crop=1",
    "gamma=2,contrast=1.5",
    "zoom=0.25",
    "columns=4,fit=width,zoom=3,rotate=180",
];

fn fuzz_states(path: PathBuf, seed: u64, steps: usize) -> String {
    let mut rng = Rng(seed);
    let mut d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
    let mut log = String::new();
    let (mut renders, mut errors) = (0, 0);
    for step in 0..steps {
        let n = d.page_count().pages;
        let op = rng.below(8);
        let ctx = format!("seed {seed} step {step} op {op} layout {}x{} opts {:?}", d.layout().width, d.layout().height, d.options());
        match op {
            0 => {
                let (w, h) = LAYOUTS[rng.below(LAYOUTS.len())];
                let before = d.position_of_page(d.current_page()).ok();
                layout(&mut d, w, h);
                let _ = writeln!(log, "{ctx}: layout {w}x{h} from {before:?} to screen {}", d.current_page());
            }
            1 => d.set_options(FixedOptions::parse(OPTIONS[rng.below(OPTIONS.len())]).unwrap()),
            2 | 3 => {
                let s = rng.below(n + 1);
                let (w, h) = (d.layout().width as usize, d.layout().height as usize);
                let mut c = if rng.below(4) == 0 { Canvas::new(1 + rng.below(50), 1 + rng.below(50)) } else { Canvas::new(w, h) };
                match d.render_page(s, &mut c) {
                    Ok(()) => renders += 1,
                    Err(e) => {
                        errors += 1;
                        assert!(s >= n, "{ctx}: render of screen {s} of {n} failed: {e}");
                    }
                }
            }
            4 => {
                let _ = d.prefetch(rng.below(n + 2));
            }
            5 => {
                let s = rng.below(n.max(1));
                let p = |r: &mut Rng| Point { x: r.below(2400) as i32 - 200, y: r.below(2400) as i32 - 200 };
                let (a, b) = (p(&mut rng), p(&mut rng));
                if let Ok(Some(sel)) = d.select(s, a, b) {
                    if let Position::Page(pp) = &sel.start {
                        let _ = d.highlight_rects(s, pp);
                    }
                }
            }
            6 => {
                if n > 0 {
                    let s = rng.below(n);
                    let pos = d.position_of_page(s).unwrap_or_else(|e| panic!("{ctx}: position_of_page({s}): {e}"));
                    let back = d.page_of(&pos).unwrap_or_else(|e| panic!("{ctx}: page_of({pos}): {e}"));
                    let pages_back = d.pages_of_screen(back);
                    if let Position::Page(pp) = &pos {
                        assert!(pages_back.contains(&(pp.page as usize)), "{ctx}: screen {s} -> {pos} -> screen {back} shows {pages_back:?}");
                    }
                }
            }
            _ => {
                d.set_current_page(rng.below(2 * n + 2));
            }
        }
        let n = d.page_count().pages;
        assert!(d.current_page() < n.max(1), "{ctx}: current {} >= {n}", d.current_page());
        let (used, cap) = d.cache_usage();
        assert!(used <= cap, "{ctx}: cache {used} > {cap}");
    }
    format!("seed {seed}: {steps} steps, {renders} renders, {errors} out-of-range errors")
}

#[test]
fn rapid_option_and_layout_changes_keep_invariants() {
    let pdf = write("fuzz.pdf", &testgen::fixture_pdf());
    let cbz = write("fuzz.cbz", &testgen::fixture_cbz(7));
    // The default run is short; INK_VERIFY_LONG=1 runs 4 seeds x 600 steps (about 4 min
    // in a host debug build: modes with thousands of tiles make page_of slow).
    let long = std::env::var_os("INK_VERIFY_LONG").is_some();
    let steps = if long { 600 } else { 100 };
    let seeds: &[u64] = if long { &[1, 7, 42, 1234] } else { &[1, 42] };
    for &seed in seeds {
        let (p, c) = (pdf.clone(), cbz.clone());
        eprintln!("{}", within(300, "state fuzz pdf", move || fuzz_states(p, seed, steps)));
        eprintln!("{}", within(300, "state fuzz cbz", move || fuzz_states(c, seed + 100, steps)));
    }
}

/// Exact round trip in every mode (the summary claims it). `page_of` scans every tile of
/// the page, so checking every screen costs screens x tiles; this checks up to 400 screens
/// per mode (all of them when fewer, else an even sample plus the first and last tile of
/// every page) and logs the slowest `page_of` per mode.
#[test]
fn every_screen_round_trips_in_every_mode() {
    let pdf = write("roundtrip.pdf", &testgen::fixture_pdf());
    let cbz = write("roundtrip.cbz", &testgen::fixture_cbz(7));
    let mut bad = Vec::new();
    let layouts: &[(u32, u32)] = if cfg!(target_arch = "arm") { &[(1272, 1696), (1696, 1272)] } else { &[(1272, 1696), (1696, 1272), (600, 800)] };
    for path in [pdf, cbz] {
        for &(w, h) in layouts {
            for opts in OPTIONS {
                let mut d = open_opts(&path, opts).unwrap();
                layout(&mut d, w, h);
                let n = d.page_count().pages;
                let per = d.plan().per_page();
                let mut screens: Vec<usize> = if n <= 400 {
                    (0..n).collect()
                } else {
                    let mut v: Vec<usize> = (0..400).map(|k| k * n / 400).collect();
                    for p in 0..d.doc_pages() {
                        v.push(p * per);
                        v.push(p * per + per - 1);
                    }
                    v
                };
                screens.sort_unstable();
                screens.dedup();
                let mut slowest = Duration::ZERO;
                let bad_before = bad.len();
                for &s in &screens {
                    let pos = d.position_of_page(s).unwrap();
                    let t = Instant::now();
                    let back = d.page_of(&pos).unwrap();
                    slowest = slowest.max(t.elapsed());
                    if back != s {
                        bad.push(format!("{} {w}x{h} '{opts}': screen {s} -> {pos} -> {back}", path.file_name().unwrap().to_string_lossy()));
                    }
                }
                eprintln!(
                    "{} {w}x{h} '{opts}': {n} screens ({per} per page), checked {}, mismatches {}, slowest page_of {slowest:?}",
                    path.file_name().unwrap().to_string_lossy(),
                    screens.len(),
                    bad.len() - bad_before
                );
            }
        }
    }
    eprintln!("{} round-trip mismatches:\n{}", bad.len(), bad.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
    assert!(bad.is_empty(), "{} screens do not round-trip (first: {})", bad.len(), bad[0]);
}

/// What a reader sees from the round-trip failure: on a lower tile, changing only the
/// contrast (set_options keeps the position through position_of_page/page_of) moves the
/// view back up, one tile per change.
#[test]
fn option_changes_keep_the_current_tile() {
    let pdf = write("drift.pdf", &testgen::fixture_pdf());
    let cbz = write("drift.cbz", &testgen::fixture_cbz(7));
    let mut bad = Vec::new();
    for (path, opts, (w, h)) in [(cbz, "fit=width", (1272u32, 1696u32)), (pdf.clone(), "crop=1,columns=2,fit=width", (1272, 1696)), (pdf, "zoom=2", (1696, 1272))] {
        let mut d = open_opts(&path, opts).unwrap();
        layout(&mut d, w, h);
        let per = d.plan().per_page();
        let start = per - 1; // last tile of page 0
        d.set_current_page(start);
        let mut seen = vec![d.current_page()];
        for k in 0..4 {
            let mut o = FixedOptions::parse(opts).unwrap();
            o.contrast = if k % 2 == 0 { 1.2 } else { 1.0 };
            d.set_options(o);
            seen.push(d.current_page());
        }
        eprintln!("{} '{opts}' {w}x{h}: {per} tiles per page, current screen after each contrast change {seen:?}", path.file_name().unwrap().to_string_lossy());
        if seen.iter().any(|&s| s != start) {
            bad.push(format!("{} '{opts}': {seen:?}", path.file_name().unwrap().to_string_lossy()));
        }
    }
    assert!(bad.is_empty(), "the view moved on contrast changes: {bad:?}");
}

/// Column regions should be real columns: none narrower than 5 % of the page width. A
/// sliver region gets "fit width" scaling of hundreds of pixels per point and thousands
/// of tiles per page.
#[test]
fn column_regions_are_not_slivers_and_tile_counts_stay_readable() {
    let pdf = write("slivers.pdf", &testgen::fixture_pdf());
    let mut bad = Vec::new();
    for opts in ["columns=2", "columns=3", "columns=4", "crop=1,columns=3", "columns=4,fit=width", "columns=4,fit=width,zoom=3,rotate=180"] {
        let mut d = open_opts(&pdf, opts).unwrap();
        layout(&mut d, 1272, 1696);
        for p in 0..d.doc_pages() {
            let pw = d.page_bounds(p).unwrap().w();
            let regions = d.columns(p).unwrap();
            let narrow: Vec<f32> = regions.iter().map(|r| r.w()).filter(|&w| w < 0.05 * pw).collect();
            if !narrow.is_empty() {
                bad.push(format!("'{opts}' page {p}: region widths {:?} of page width {pw}", regions.iter().map(|r| r.w()).collect::<Vec<_>>()));
            }
        }
        let per = d.plan().per_page();
        eprintln!("'{opts}': plan {:?}, {per} tiles per page", d.plan());
        // Fit width of a quarter-page column at zoom 3 needs about 4 x 3 x 12 tiles.
        if per > 400 {
            bad.push(format!("'{opts}': {per} tiles per page"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn rotation_back_and_forth_keeps_the_page() {
    let pdf = write("rotate.pdf", &testgen::fixture_pdf());
    let cbz = write("rotate.cbz", &testgen::fixture_cbz(12));
    for (path, opts, start) in [(cbz.clone(), "", 5usize), (cbz, "", 6), (pdf.clone(), "fit=width", 3), (pdf, "crop=1,columns=2,fit=width", 5)] {
        let mut d = open_opts(&path, opts).unwrap();
        layout(&mut d, 1272, 1696);
        let first = d.first_screen_of(start);
        d.set_current_page(first);
        let page = |d: &MupdfDocument| d.pages_of_screen(d.current_page());
        let mut seen = Vec::new();
        for i in 0..20 {
            if i % 2 == 0 {
                layout(&mut d, 1696, 1272);
            } else {
                layout(&mut d, 1272, 1696);
            }
            seen.push(page(&d));
        }
        let last = page(&d);
        assert!(
            last.contains(&start) || last.contains(&(start - 1)),
            "{} '{opts}': started on page {start}, after 20 rotations on {last:?} (history {seen:?})",
            path.file_name().unwrap().to_string_lossy()
        );
    }
}

/// A selection made on a rotated, zoomed tile is stored as page + quads in page space: it
/// names the same text, and draws a highlight over the same word, under other options.
#[test]
fn selections_in_rotated_tiles_survive_option_changes() {
    let path = write("select-rotated.pdf", &testgen::fixture_pdf());
    let mut bad = Vec::new();
    for opts in ["rotate=90,fit=width,zoom=2", "rotate=180,zoom=3", "rotate=270,crop=1,fit=width", "crop=1,columns=2,fit=width,zoom=1.5"] {
        let mut d = open_opts(&path, opts).unwrap();
        layout(&mut d, 1272, 1696);
        let hit = d.search("needle", 1).unwrap().remove(0);
        let Position::Page(pp) = &hit.position else { panic!("not a page position") };
        let q = FRectLike::from(pp.quads[0]);
        let screen = hit.page.expect("search hit has a screen");
        let Some(piece) = d.pieces(screen).into_iter().find(|p| p.page == pp.page as usize) else {
            bad.push(format!("'{opts}': hit screen {screen} does not show page {}", pp.page));
            continue;
        };
        // Drag from the first to the last letter, along the word's baseline middle.
        let ym = (q.y0 + q.y1) / 2.0;
        let (ax, ay) = piece.ctm.apply(q.x0 + 1.5, ym);
        let (bx, by) = piece.ctm.apply(q.x1 - 1.5, ym);
        let sel = d.select(screen, Point { x: ax.round() as i32, y: ay.round() as i32 }, Point { x: bx.round() as i32, y: by.round() as i32 }).unwrap();
        let Some(sel) = sel else {
            bad.push(format!("'{opts}': no selection on screen {screen}"));
            continue;
        };
        if sel.text != "needle" {
            bad.push(format!("'{opts}': selected {:?}", sel.text));
        }
        let stored: Position = sel.start.to_string().parse().unwrap();
        let Position::Page(back) = &stored else { panic!() };
        for other in ["", "zoom=2", "rotate=90,fit=width"] {
            d.set_options(FixedOptions::parse(other).unwrap());
            let text = d.text_at(back).unwrap();
            let v = d.page_of(&stored).unwrap();
            let rects = d.highlight_rects(v, back);
            if text != "needle" || rects.is_empty() {
                bad.push(format!("'{opts}' -> '{other}': text_at {text:?}, highlight on screen {v}: {rects:?}"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// `Quad` without the crate's FRect import (keeps this file on the public API).
struct FRectLike {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl From<Quad> for FRectLike {
    fn from(q: Quad) -> Self {
        FRectLike { x0: q.x0.min(q.x1), y0: q.y0.min(q.y1), x1: q.x0.max(q.x1), y1: q.y0.max(q.y1) }
    }
}

// --- 10. A 2000-page PDF -----------------------------------------------------------------

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn a_2000_page_pdf_opens_searches_and_lists_its_toc_in_time() {
    let mut pages = Vec::new();
    for i in 0..2000 {
        let mut p = testgen::PdfPage::letter();
        p.text(72.0, 72.0, 18.0, testgen::Font::HelveticaBold, &format!("Page {}", i + 1));
        for l in 0..20 {
            p.text(72.0, 110.0 + 24.0 * l as f32, 11.0, testgen::Font::Times, &testgen::filler(i * 31 + l, 12));
        }
        pages.push(p);
    }
    let outline: Vec<testgen::PdfOutline> =
        (0..200).map(|k| testgen::PdfOutline { title: format!("Part {k}"), level: (k % 2) as u8, page: k * 10, top: 72.0 }).collect();
    let bytes = testgen::write_pdf(&pages, &outline, &[("Title", "Two thousand pages")]);
    let size = bytes.len();
    let path = write("pages2000.pdf", &bytes);
    let report = within(600, "2000-page PDF", move || {
        let mut r = format!("file {size} bytes\n");
        let t = Instant::now();
        let mut d = MupdfDocument::open(&path, FixedOptions::default()).unwrap();
        let _ = writeln!(r, "open {:?}, {} pages", t.elapsed(), d.doc_pages());
        assert_eq!(d.doc_pages(), 2000);
        layout(&mut d, 1272, 1696);
        let t = Instant::now();
        let mut c = Canvas::new(1272, 1696);
        d.render_page(1999, &mut c).unwrap();
        let _ = writeln!(r, "render last page {:?}", t.elapsed());
        let t = Instant::now();
        let toc = d.toc();
        let _ = writeln!(r, "toc ({} entries, fit page) {:?}", toc.len(), t.elapsed());
        let t = Instant::now();
        let hits = d.search("zebra", 10).unwrap();
        let _ = writeln!(r, "search for a missing word over 2000 pages {:?} ({} hits)", t.elapsed(), hits.len());
        let t = Instant::now();
        let hits = d.search("Page 1999", 10).unwrap();
        let _ = writeln!(r, "search 'Page 1999' {:?} ({} hits)", t.elapsed(), hits.len());
        assert!(!hits.is_empty());
        let t = Instant::now();
        d.set_options(FixedOptions::parse("crop=1,fit=width").unwrap());
        let _ = writeln!(r, "set_options crop+fit width {:?}, {} screens", t.elapsed(), d.page_count().pages);
        let t = Instant::now();
        let toc = d.toc();
        let _ = writeln!(r, "toc ({} entries, crop + fit width) {:?}", toc.len(), t.elapsed());
        r
    });
    eprintln!("{report}");
}

// --- 11. Byte-flip fuzz on PDF and CBZ ---------------------------------------------------

fn fuzz_file(seed: u64, base: &[u8], name: &str, cases: usize) {
    let mut rng = Rng(seed);
    for case in 0..cases {
        let mut b = base.to_vec();
        let flips = 1 + rng.below(40);
        for _ in 0..flips {
            let i = rng.below(b.len());
            b[i] = match rng.below(4) {
                0 => rng.next() as u8,
                1 => b'0' + rng.below(10) as u8,
                2 => b' ',
                _ => b[i] ^ 0x80,
            };
        }
        if rng.below(5) == 0 {
            let cut = rng.below(b.len());
            b.truncate(cut);
        }
        eprintln!("{name} case {case}");
        let path = write(&format!("{name}-{case}.bin"), &b);
        let p = path.clone();
        within(60, &format!("{name} fuzz case {case}"), move || {
            if let Ok(mut d) = MupdfDocument::open(&p, FixedOptions::default()) {
                layout(&mut d, 200, 300);
                let _ = walk(&mut d, 12);
                let _ = d.search("needle", 3);
                let _ = d.toc();
                if let Ok(pos) = "page:0".parse::<Position>() {
                    let _ = d.links(&pos);
                }
                let _ = d.select(0, Point { x: 10, y: 10 }, Point { x: 150, y: 200 });
                let _ = d.thumbnail(40, 60);
                d.set_options(FixedOptions::parse("crop=1,columns=2,fit=width").unwrap());
                let _ = walk(&mut d, 4);
            }
        });
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
#[ignore = "child body; run by byte_flip_fuzz_does_not_crash"]
fn child_byte_flip_fuzz() {
    if is_child() {
        let long = std::env::var_os("INK_VERIFY_LONG").is_some();
        let cases = if long { 300 } else if cfg!(target_arch = "arm") { 60 } else { 100 };
        fuzz_file(99, &testgen::fixture_pdf(), "pdf", cases);
        fuzz_file(77, &testgen::fixture_cbz(4), "cbz", cases / 2);
    }
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn byte_flip_fuzz_does_not_crash() {
    let (ok, log) = child("child_byte_flip_fuzz");
    assert!(ok, "byte-flip fuzz (the last 'case N' line is the crashing input):\n{log}");
}
