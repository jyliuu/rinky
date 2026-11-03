//! Renders screens of a PDF/CBZ/XPS file to PNG with the same code the reader uses.
//!
//!     cargo run -p ink-mupdf --example mupdf-render -- FILE OUTDIR [WxH] [OPTIONS] [SCREENS]
//!
//! `WxH` defaults to 1272x1696, `OPTIONS` uses `FixedOptions::parse` syntax
//! (`fit=width,crop=1`), `SCREENS` is a count (default 3) or a list `0,4,7`.
//! Prints page count, plan, timing, TOC and metadata.

use ink_device::Canvas;
use ink_doc::{Document, LayoutSettings};
use ink_mupdf::{FixedOptions, MupdfDocument};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: mupdf-render FILE OUTDIR [WxH] [OPTIONS] [SCREENS]");
        std::process::exit(2);
    }
    let file = PathBuf::from(&args[0]);
    let out = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&out)?;
    let (w, h) = match args.get(2).and_then(|s| s.split_once('x')) {
        Some((a, b)) => (a.parse()?, b.parse()?),
        None => (1272u32, 1696u32),
    };
    let opts = FixedOptions::parse(args.get(3).map(String::as_str).unwrap_or(""))?;
    let t = Instant::now();
    let mut doc = MupdfDocument::open(&file, opts)?;
    let t_open = t.elapsed();
    doc.set_layout(&LayoutSettings::for_screen(w, h))?;
    let total = doc.page_count().pages;
    println!(
        "{}: {:?}, {} pages, {} screens, plan {:?}, open {:?}, mupdf {}",
        file.display(),
        doc.file_kind(),
        doc.doc_pages(),
        total,
        doc.plan(),
        t_open,
        ink_mupdf::mupdf_version().unwrap_or("?")
    );
    println!("metadata: {:?}", doc.metadata());
    for e in doc.toc() {
        println!("toc: {}{} -> {} (screen {:?})", "  ".repeat(e.level as usize), e.title, e.position, e.page);
    }
    let screens: Vec<usize> = match args.get(4) {
        Some(s) if s.contains(',') => s.split(',').filter_map(|v| v.parse().ok()).collect(),
        Some(s) => (0..s.parse::<usize>()?.min(total)).collect(),
        None => (0..3.min(total)).collect(),
    };
    let mut canvas = Canvas::new(w as usize, h as usize);
    for s in screens {
        let t = Instant::now();
        doc.render_page(s, &mut canvas)?;
        let dt = t.elapsed();
        let path = out.join(format!("screen-{s:03}.png"));
        canvas.save_png(&path)?;
        println!("screen {s}: {:?} -> {} ({:?})", doc.pages_of_screen(s), path.display(), dt);
        println!("  position {}", doc.position_of_page(s)?);
    }
    let thumb = doc.thumbnail(300, 400)?;
    thumb.save_png(&out.join("thumbnail.png"))?;
    let (used, cap) = doc.cache_usage();
    println!("cache {used} / {cap} bytes, warnings {}", doc.warnings());
    Ok(())
}
