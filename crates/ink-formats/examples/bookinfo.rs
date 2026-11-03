//! Prints what ink-formats reads from book files: format, metadata, identifiers, cover,
//! DRM status, chapters and the nested TOC. Optionally writes the converted chapters
//! and the cover into a directory.
//!
//!   cargo run -p ink-formats --example bookinfo -- [--dump DIR] [--pages] BOOK...
//!
//! `--pages` also lays the book out at 1272 x 1696 and prints the page count and the
//! page of each TOC entry.

use ink_doc::{Document, LayoutSettings};
use ink_formats::book::{toc_depth, toc_len};
use ink_formats::TocNode;
use std::path::{Path, PathBuf};

fn print_toc(nodes: &[TocNode], depth: usize, out: &mut Vec<String>) {
    for n in nodes {
        let t = n.target.as_ref().map(|t| format!("ch{}{}", t.chapter, t.id.as_ref().map(|i| format!("#{i}")).unwrap_or_default())).unwrap_or_else(|| "-".into());
        out.push(format!("{}{} -> {}", "  ".repeat(depth + 1), n.title, t));
        print_toc(&n.children, depth + 1, out);
    }
}

fn main() {
    let mut dump: Option<PathBuf> = None;
    let mut pages = false;
    let mut books = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dump" => dump = args.next().map(PathBuf::from),
            "--pages" => pages = true,
            _ => books.push(PathBuf::from(a)),
        }
    }
    let mut failed = 0;
    for path in &books {
        println!("== {}", path.display());
        println!("format_of: {:?}", ink_formats::format_of(path));
        match ink_formats::read_info(path) {
            Ok(info) => {
                println!("read_info: {} | title {:?} | authors {:?} | language {:?}", info.version, info.metadata.title, info.metadata.authors, info.metadata.language);
                println!("  publisher {:?} | series {:?} | date {:?} | subjects {:?}", info.metadata.publisher, info.metadata.series, info.date, info.subjects);
                println!("  identifiers {:?}", info.identifiers.iter().map(|i| format!("{}:{}", i.scheme.as_deref().unwrap_or("?"), i.value)).collect::<Vec<_>>());
                println!("  cover {:?} {}", info.metadata.cover, info.cover.as_ref().map(|c| format!("{} {} bytes", c.media_type, c.bytes.len())).unwrap_or_default());
                println!("  fonts {:?}", info.fonts);
                println!("  drm {:?} | refusal {:?}", info.drm, info.refusal);
                if let (Some(dir), Some(c)) = (&dump, &info.cover) {
                    let _ = std::fs::create_dir_all(dir);
                    let name = format!("{}-cover.{}", stem(path), c.path.rsplit('.').next().unwrap_or("bin"));
                    let _ = std::fs::write(dir.join(name), &c.bytes);
                }
            }
            Err(e) => println!("read_info: error: {e}"),
        }
        match ink_formats::load(path) {
            Ok(book) => {
                println!("load: {} chapters, TOC {} entries (depth {}), page list {}", book.chapters.len(), toc_len(&book.toc), toc_depth(&book.toc), book.page_list.len());
                let mut lines = Vec::new();
                print_toc(&book.toc, 0, &mut lines);
                for l in lines.iter().take(60) {
                    println!("{l}");
                }
                if lines.len() > 60 {
                    println!("  ... {} more", lines.len() - 60);
                }
                for p in book.page_list.iter().take(5) {
                    println!("  page {} -> ch{} {:?}", p.label, p.target.chapter, p.target.id);
                }
                if let Some(dir) = &dump {
                    let d = dir.join(stem(path));
                    let _ = std::fs::create_dir_all(&d);
                    for c in &book.chapters {
                        let _ = std::fs::write(d.join(c.path.replace('/', "_")), &c.html);
                    }
                }
                if pages {
                    match book.into_document() {
                        Ok(mut doc) => match doc.set_layout(&LayoutSettings::for_screen(1272, 1696)) {
                            Ok(()) => {
                                println!("layout: {} pages", doc.page_count().pages);
                                for e in doc.toc().iter().take(20) {
                                    println!("  {:?} page {:?} {}", e.title, e.page, e.position);
                                }
                            }
                            Err(e) => println!("layout: error: {e}"),
                        },
                        Err(e) => println!("document: error: {e}"),
                    }
                }
            }
            Err(e) => {
                println!("load: refused/error: {e}");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        println!("{failed} of {} books did not open", books.len());
    }
}

fn stem(p: &Path) -> String {
    p.file_name().and_then(|s| s.to_str()).unwrap_or("book").replace('.', "_")
}
