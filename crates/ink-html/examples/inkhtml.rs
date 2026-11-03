//! A tool for the reflowable engine (host and Kindle):
//!
//! ```text
//! inkhtml render BOOK OUTDIR [PAGE...] [--font-px N] [--no-publisher] [--hyphenate] [--size WxH]
//! inkhtml bench  BOOK [--cache DIR] [--font-px N]      first page time, pagination time, page count
//! inkhtml info   BOOK                                   metadata, TOC, page labels, locations
//! ```
//! Pages are 1-based. The book id for the page-map cache is KOReader's partial MD5.

use ink_device::Canvas;
use ink_doc::{Document, LayoutSettings};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn open(book: &Path) -> Box<dyn Document> {
    // The app's document: the engine plus the book's own TOC and page list.
    Box::new(ink_formats::open_book(book).unwrap_or_else(|e| panic!("{}: {e}", book.display())))
}

fn settings(args: &[String]) -> LayoutSettings {
    let mut s = LayoutSettings::for_screen(1272, 1696);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--font-px" => {
                s.font_px = args[i + 1].parse().unwrap();
                i += 1;
            }
            "--no-publisher" => s.publisher_styles = false,
            "--hyphenate" => s.hyphenate = true,
            "--no-justify" => s.justify = false,
            "--size" => {
                let (w, h) = args[i + 1].split_once('x').unwrap();
                s.width = w.parse().unwrap();
                s.height = h.parse().unwrap();
                i += 1;
            }
            "--font" => {
                s.font_family = Some(args[i + 1].clone());
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    s
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let _ = env_logger_init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    match cmd {
        "render" => {
            let book = PathBuf::from(&args[1]);
            let out = PathBuf::from(&args[2]);
            std::fs::create_dir_all(&out).unwrap();
            let mut doc = open(&book);
            let s = settings(&args[3..]);
            doc.set_layout(&s).unwrap();
            doc.wait_paginated(std::time::Duration::from_secs(600));
            let total = doc.page_count().pages;
            let mut pages: Vec<usize> = args[3..].iter().take_while(|a| !a.starts_with("--")).filter_map(|a| a.parse().ok()).collect();
            if pages.is_empty() {
                pages = (1..=total.min(8)).collect();
            }
            let mut c = Canvas::new(s.width as usize, s.height as usize);
            for p in pages {
                if p == 0 || p > total {
                    continue;
                }
                doc.render_page(p - 1, &mut c).unwrap();
                let f = out.join(format!("page-{p:04}.png"));
                c.save_png(&f).unwrap();
                println!("{} {}", f.display(), doc.position_of_page(p - 1).unwrap());
            }
            println!("pages {total}");
        }
        "bench" => {
            let book = PathBuf::from(&args[1]);
            let s = settings(&args[2..]);
            let t0 = Instant::now();
            let mut doc = open(&book);
            let t_open = t0.elapsed();
            if let Some(dir) = flag(&args, "--cache") {
                let id = ink_store::partial_md5(&book).unwrap();
                doc.set_page_cache(Path::new(&dir), &id);
                println!("book id (partial MD5) {id}");
            }
            let t1 = Instant::now();
            doc.set_layout(&s).unwrap();
            let mut c = Canvas::new(s.width as usize, s.height as usize);
            doc.render_page(0, &mut c).unwrap();
            let first = t1.elapsed();
            println!("open {} ms, first page laid out and drawn {} ms (total {} ms), pages known {:?}", t_open.as_millis(), first.as_millis(), t0.elapsed().as_millis(), doc.page_count());
            let t2 = Instant::now();
            doc.wait_paginated(std::time::Duration::from_secs(600));
            println!("all pages {} after {} ms more; complete {}", doc.page_count().pages, t2.elapsed().as_millis(), doc.page_count().complete);
            let tt = Instant::now();
            let toc = doc.toc();
            let numbered = toc.iter().filter(|e| e.page.is_some()).count();
            for e in &toc {
                let _ = doc.page_of(&e.position);
            }
            println!("toc: {} entries, {numbered} with pages, toc + page_of each in {:.1} ms", toc.len(), tt.elapsed().as_secs_f64() * 1000.0);
            let t3 = Instant::now();
            let n = doc.page_count().pages;
            for p in [n / 2, n.saturating_sub(1)] {
                doc.render_page(p, &mut c).unwrap();
            }
            println!("render 2 pages in other chapters {} ms", t3.elapsed().as_millis());
            println!("location count {:?}", doc.location_count());
        }
        "info" => {
            let book = PathBuf::from(&args[1]);
            let mut doc = open(&book);
            doc.set_layout(&settings(&args[2..])).unwrap();
            doc.wait_paginated(std::time::Duration::from_secs(600));
            let m = doc.metadata();
            println!("title {:?} authors {:?} language {:?} pages {}", m.title, m.authors, m.language, doc.page_count().pages);
            for t in doc.toc() {
                println!("toc {}{} -> {} (page {:?})", "  ".repeat(t.level as usize), t.title, t.position, t.page.map(|p| p + 1));
            }
            for (l, p) in doc.page_labels() {
                println!("page-label {l} -> {p} (page {:?})", doc.page_of(&p).ok().map(|p| p + 1));
            }
            println!("locations {:?}", doc.location_count());
        }
        "stress" => stress(&args[1..]),
        _ => eprintln!("usage: inkhtml render|bench|info|stress ..."),
    }
}

/// Words for the `stress` workloads.
const WORDS: &[&str] = &["Romeo", "Juliet", "Verona", "households", "dignity", "mutiny", "star-crossed", "Montague", "Capulet", "quarrel", "unclean", "misadventured", "overthrows"];

/// `inkhtml stress PNG BOOK ROUNDS [layout|open|zip|spawn|fontparse|raster|hyph|copy|none]`:
/// memory-integrity check for one process with two busy threads. Thread A decodes PNG
/// ROUNDS times and compares the pixels with the first decode. Thread B, until A is done:
/// `layout` opens BOOK and paginates it again and again (alternating font sizes, no
/// cache), `open` only opens BOOK, `zip` reads every ZIP entry, `spawn` starts and
/// joins a thread with a 16 MB stack, `fontparse` builds a font book and parses its four serif faces, `raster`
/// draws words with parsed faces, `hyph` hyphenates English words, `copy` copies
/// canvas-sized buffers, `none` idles. Prints every mismatch (count and first offset).
fn stress(args: &[String]) {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    let png = PathBuf::from(&args[0]);
    let book = PathBuf::from(&args[1]);
    let rounds: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50);
    let mode = args.get(3).cloned().unwrap_or_else(|| "layout".into());
    let done = Arc::new(AtomicBool::new(false));
    let b_rounds = Arc::new(AtomicUsize::new(0));
    let b = {
        let (done, b_rounds) = (done.clone(), b_rounds.clone());
        std::thread::Builder::new()
            .stack_size(16 << 20)
            .spawn(move || {
                let mut fonts = ink_html::fonts::FontBook::builtin();
                let mut i = 0usize;
                while !done.load(Ordering::Relaxed) {
                    match mode.as_str() {
                        "layout" => {
                            let mut doc = open(&book);
                            let mut s = LayoutSettings::for_screen(1272, 1696);
                            s.font_px = if i.is_multiple_of(2) { 40.0 } else { 34.0 };
                            doc.set_layout(&s).unwrap();
                            doc.wait_paginated(std::time::Duration::from_secs(600));
                        }
                        "fontparse" => {
                            let mut fb = ink_html::fonts::FontBook::builtin();
                            for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
                                let k = ink_html::fonts::FaceKey { family: 0, bold, italic };
                                std::hint::black_box(fb.width(k, 40.0, "Romeo"));
                            }
                        }
                        "raster" => {
                            let mut c = Canvas::new(1272, 400);
                            for (j, w) in WORDS.iter().cycle().take(400).enumerate() {
                                let k = ink_html::fonts::FaceKey { family: 0, bold: j % 3 == 0, italic: j % 5 == 0 };
                                let px = 20.0 + (j % 40) as f32;
                                fonts.draw(&mut c, k, px, (j % 20 * 60) as f32, 50.0 + (j % 6 * 60) as f32, w);
                            }
                        }
                        "hyph" => {
                            let h = ink_html::hyph::for_lang("en").unwrap();
                            for w in WORDS.iter().cycle().take(5000) {
                                std::hint::black_box(ink_text::Hyphenator::hyphenate(&*h, w));
                            }
                        }
                        "open" => {
                            std::hint::black_box(open(&book).page_count());
                        }
                        "zip" => {
                            use ink_doc::ResourceLoader;
                            let zip = zip::ZipArchive::new(std::fs::File::open(&book).unwrap()).unwrap();
                            let names: Vec<String> = zip.file_names().filter_map(|r| r.ok().map(|c| c.into_owned())).collect();
                            let loader = ink_formats::epub::ZipLoader::new(zip);
                            for n in &names {
                                std::hint::black_box(loader.read(n).map(|b| b.len()).unwrap_or(0));
                            }
                        }
                        "spawn" => {
                            let h = std::thread::Builder::new()
                                .stack_size(16 << 20)
                                .spawn(|| {
                                    let mut a = [0u8; 64 * 1024];
                                    for (j, v) in a.iter_mut().enumerate() {
                                        *v = j as u8;
                                    }
                                    std::hint::black_box(a.iter().map(|v| *v as u32).sum::<u32>())
                                })
                                .unwrap();
                            std::hint::black_box(h.join().unwrap());
                        }
                        "copy" => {
                            let src = vec![(i % 251) as u8; 1272 * 1696];
                            let dst = src.clone();
                            assert!(dst.iter().all(|v| *v == (i % 251) as u8), "copy thread: corrupted copy");
                        }
                        _ => std::thread::sleep(std::time::Duration::from_millis(50)),
                    }
                    i += 1;
                    b_rounds.store(i, Ordering::Relaxed);
                }
            })
            .unwrap()
    };
    if let Ok(up) = std::fs::read_to_string("/proc/uptime") {
        println!("stress: mode {} rounds {rounds}; guest uptime {}", args.get(3).map(|s| s.as_str()).unwrap_or("layout"), up.trim());
    }
    let first = Canvas::load_png(&png).unwrap();
    let mut bad = 0;
    for r in 0..rounds {
        let c = Canvas::load_png(&png).unwrap();
        let n = c.pixels.iter().zip(&first.pixels).filter(|(a, b)| a != b).count();
        if n > 0 {
            bad += 1;
            let at = c.pixels.iter().zip(&first.pixels).position(|(a, b)| a != b).unwrap();
            let last = c.pixels.iter().zip(&first.pixels).rposition(|(a, b)| a != b).unwrap();
            println!("round {r}: {n} pixels differ, first at x {} y {}", at % c.width, at / c.width);
            // Where the bad bytes are and what they hold: page offset and content tell a
            // lost write, a stale read and a foreign write apart (the emulator's CFA
            // mailbox at physical 0x400000d0 shows as page offset 0xd0, mostly zeros).
            // At most 8 runs; bytes that differ less than 8 apart join one run.
            println!("  new base {:#x}, first base {:#x}", c.pixels.as_ptr() as usize, first.pixels.as_ptr() as usize);
            let mut i = at;
            let mut shown = 0;
            while i <= last && shown < 8 {
                if c.pixels[i] == first.pixels[i] {
                    i += 1;
                    continue;
                }
                let s = i;
                let mut e = i + 1;
                while e <= last && (c.pixels[e] != first.pixels[e] || (e + 8 <= last && (e..e + 8).any(|k| c.pixels[k] != first.pixels[k]))) {
                    e += 1;
                }
                for (name, buf) in [("new", &c.pixels), ("first", &first.pixels)] {
                    let addr = buf.as_ptr() as usize + s;
                    let hex: Vec<String> = buf[s..e.min(s + 64)].iter().map(|v| format!("{v:02x}")).collect();
                    println!("    {name} bytes {s}..{e} addr {addr:#x} addr%32 {} addr%4096 {:#x}: {}", addr % 32, addr % 4096, hex.join(" "));
                }
                shown += 1;
                i = e;
            }
        }
    }
    done.store(true, Ordering::Relaxed);
    b.join().unwrap();
    println!("stress: {bad} of {rounds} decodes differ; thread B ran {} rounds", b_rounds.load(Ordering::Relaxed));
}

/// Minimal stderr logger for `log` records (no extra dependency).
fn env_logger_init() -> Result<(), log::SetLoggerError> {
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("[{}] {}", r.level(), r.args());
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: L = L;
    log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info))
}
