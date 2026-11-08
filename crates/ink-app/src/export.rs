//! Highlight export for the screens and the command line: runs
//! [`ink_store::export`], logs the written paths at INFO and words the toast.
//!
//! Files go to `<paths.root>/exports/` (`/mnt/us/rinky/exports` on the Kindle, the
//! simulator's own data dir on the host): `<title>.md`, `<title>.json`, or Kindle's
//! clippings layout (`<title>.txt` for one book, `My Clippings.txt` for all), in the
//! format chosen in Settings (`export_format`) or with `--format`.

use crate::plugin_host::PluginHost;
use ink_store::export::{export_all_as, export_entry_as, ExportFormat, ExportReport, Locator};
use ink_store::{BookEntry, LibraryIndex, Paths};
use std::path::Path;

/// Name shown in the toast for the plugin host (it only owns the toast layer).
const TOAST_OWNER: &str = "rinky";

/// Longest file name shown in a toast, in characters.
const TOAST_NAME_CHARS: usize = 34;

/// Shows `text` as a toast on whatever screen is on top (one DU update, no animation).
pub fn toast(plugins: &mut PluginHost, text: String) {
    plugins.ui.push(TOAST_OWNER, ink_plugin::UiRequest::Toast { text });
}

/// `exports/<file name>`, the name cut in the middle when it is long. The full path is
/// in the log.
fn short_name(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let chars: Vec<char> = name.chars().collect();
    let name = if chars.len() > TOAST_NAME_CHARS {
        let keep = TOAST_NAME_CHARS - 1;
        let tail = keep / 3;
        format!("{}…{}", chars[..keep - tail].iter().collect::<String>(), chars[chars.len() - tail..].iter().collect::<String>())
    } else {
        name
    };
    format!("exports/{name}")
}

/// Exports one book in `format` and returns the toast text. `loc` gives page numbers
/// when the book is open (the reader).
pub fn export_one(paths: &Paths, entry: &BookEntry, loc: &Locator, format: ExportFormat) -> String {
    match export_entry_as(paths, entry, loc, format) {
        Ok(Some(p)) => {
            log::info!("export: wrote {}", p.display());
            format!("Saved {}", short_name(&p))
        }
        Ok(None) => {
            log::info!("export: {} has no highlights, notes or bookmarks; no file written", entry.path.display());
            "No highlights to export".to_string()
        }
        Err(e) => {
            log::warn!("export: {}: {e}", entry.path.display());
            format!("Export failed: {e}")
        }
    }
}

/// Logs what `r` holds and words it for a toast.
fn report_text(r: &ExportReport) -> String {
    for p in &r.written {
        log::info!("export: wrote {}", p.display());
    }
    for (b, e) in &r.failed {
        log::warn!("export: {}: {e}", b.display());
    }
    log::info!("export: {} book(s) in {} file(s), {} book(s) without highlights, {} failed", r.books, r.written.len(), r.empty, r.failed.len());
    let books = |n: usize| format!("{n} book{}", if n == 1 { "" } else { "s" });
    match (r.books, r.failed.len(), r.written.as_slice()) {
        (0, 0, _) => "No highlights to export".to_string(),
        // Kindle clippings: every book in one file.
        (n, 0, [one]) if n > 1 => format!("Exported {} to {}", books(n), short_name(one)),
        (n, 0, _) => format!("Exported {} to exports/", books(n)),
        (n, f, _) => format!("Exported {n}, {f} failed (see the log)"),
    }
}

/// Exports every book of `index` that has highlights, notes or bookmarks in `format`;
/// the toast text.
pub fn export_everything(paths: &Paths, index: &LibraryIndex, format: ExportFormat) -> String {
    report_text(&export_all_as(paths, index, format))
}

/// The library index for `export_everything`, read from `paths.library()`.
pub fn load_index(paths: &Paths) -> Result<LibraryIndex, String> {
    LibraryIndex::load(&paths.library()).map_err(|e| format!("library.json: {e}"))
}

/// `rinky export-highlights [--format markdown|clippings|json] [--all | BOOK]`. BOOK is
/// a file path or a part of a title in the library; the format defaults to Markdown.
/// Prints each written path. Only reads sidecars and the library index (safe while the
/// app runs). Exit 0 when files were written or there was nothing to write, 2 for bad
/// arguments, 1 when a book is not found or a write failed.
pub fn cli(args: &[String], paths: &Paths) -> anyhow::Result<i32> {
    const USAGE: &str = "usage: rinky export-highlights [--format markdown|clippings|json] [--all | BOOK]   (BOOK: file path or part of a title)";
    let mut format = ExportFormat::Markdown;
    let mut rest: Vec<&String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let value = match a.strip_prefix("--format") {
            Some("") => it.next().map(String::as_str),
            Some(v) if v.starts_with('=') => Some(&v[1..]),
            _ => {
                rest.push(a);
                continue;
            }
        };
        match value.and_then(ExportFormat::from_name) {
            Some(f) => format = f,
            None => {
                eprintln!("{USAGE}");
                return Ok(2);
            }
        }
    }
    let index = load_index(paths).map_err(|e| anyhow::anyhow!(e))?;
    let mut failed = false;
    match rest.as_slice() {
        [a] if a.as_str() == "--all" => {
            let r = export_all_as(paths, &index, format);
            for p in &r.written {
                println!("{}", p.display());
            }
            for (b, e) in &r.failed {
                eprintln!("{}: {e}", b.display());
            }
            let text = report_text(&r);
            eprintln!("{text}");
            failed = !r.failed.is_empty();
        }
        [book] if !book.starts_with("--") => {
            let entry = find_book(&index, book)?;
            match export_entry_as(paths, &entry, &Locator::none(), format) {
                Ok(Some(p)) => {
                    log::info!("export: wrote {}", p.display());
                    println!("{}", p.display());
                }
                Ok(None) => eprintln!("{}: no highlights, notes or bookmarks; no file written", entry.path.display()),
                Err(e) => {
                    eprintln!("{}: {e}", entry.path.display());
                    failed = true;
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            return Ok(2);
        }
    }
    Ok(i32::from(failed))
}

/// The library entry for `arg`: an existing file (its entry, or a bare one named by its
/// file name), else the one book whose title contains `arg` (case-insensitive).
fn find_book(index: &LibraryIndex, arg: &str) -> anyhow::Result<BookEntry> {
    let path = Path::new(arg);
    if path.is_file() {
        let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(e) = index.get(&abs).or_else(|| index.get(path)) {
            return Ok(e.clone());
        }
        let title = abs.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok(BookEntry { path: abs, title, ..Default::default() });
    }
    let q = arg.to_lowercase();
    let hits: Vec<&BookEntry> = index.books.iter().filter(|b| b.title.to_lowercase().contains(&q)).collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => anyhow::bail!("no book file or library title matches {arg:?}"),
        many => {
            let mut list: Vec<String> = many.iter().map(|b| format!("  {} ({})", b.title, b.path.display())).collect();
            list.sort();
            anyhow::bail!("{} books match {arg:?}; give the file path:\n{}", many.len(), list.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_store::sidecar::Highlight;
    use ink_store::BookState;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ink-app-export-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn report_toasts_name_the_one_clippings_file() {
        let r = |books: usize, written: &[&str], failed: usize| ExportReport {
            written: written.iter().map(std::path::PathBuf::from).collect(),
            books,
            empty: 0,
            failed: (0..failed).map(|i| (std::path::PathBuf::from(format!("/b/{i}")), "e".into())).collect(),
        };
        assert_eq!(report_text(&r(0, &[], 0)), "No highlights to export");
        assert_eq!(report_text(&r(1, &["/x/exports/A.md"], 0)), "Exported 1 book to exports/");
        assert_eq!(report_text(&r(3, &["/x/exports/A.md", "/x/exports/B.md", "/x/exports/C.md"], 0)), "Exported 3 books to exports/");
        assert_eq!(report_text(&r(3, &["/x/exports/My Clippings.txt"], 0)), "Exported 3 books to exports/My Clippings.txt");
        assert_eq!(report_text(&r(2, &["/x/a", "/x/b"], 1)), "Exported 2, 1 failed (see the log)");
    }

    #[test]
    fn toast_names_are_short() {
        let p = Path::new("/mnt/us/rinky/exports/Short.md");
        assert_eq!(short_name(p), "exports/Short.md");
        let long = format!("/x/{}.md", "ab".repeat(40));
        let s = short_name(Path::new(&long));
        assert!(s.chars().count() <= "exports/".len() + TOAST_NAME_CHARS && s.contains('…'), "{s}");
    }

    #[test]
    fn cli_exports_a_book_by_title_and_all() {
        let dir = tmp("cli");
        let paths = Paths { root: dir.join("home") };
        let book = dir.join("b.epub");
        std::fs::write(&book, "x").unwrap();
        BookState { highlights: vec![Highlight { start: "/body/p[1]/text().0".into(), text: "hi".into(), ..Default::default() }], ..Default::default() }
            .save(&book)
            .unwrap();
        std::fs::create_dir_all(&paths.root).unwrap();
        let mut idx = LibraryIndex::default();
        idx.books.push(BookEntry { path: book.clone(), title: "The Book".into(), format: "epub".into(), ..Default::default() });
        idx.save(&paths.library()).unwrap();
        let a = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(cli(&a(&["the bo"]), &paths).unwrap(), 0);
        let out = paths.exports().join("The Book.md");
        assert!(std::fs::read_to_string(&out).unwrap().contains("> hi"));
        std::fs::remove_file(&out).unwrap();
        assert_eq!(cli(&a(&["--all"]), &paths).unwrap(), 0);
        assert!(out.exists());
        assert!(cli(&a(&["nothing like it"]), &paths).is_err());
        assert_eq!(cli(&a(&[]), &paths).unwrap(), 2);
        // --format, before or after the book, with a space or '='.
        assert_eq!(cli(&a(&["--format", "json", "the bo"]), &paths).unwrap(), 0);
        assert!(std::fs::read_to_string(paths.exports().join("The Book.json")).unwrap().contains("\"text\": \"hi\""));
        assert_eq!(cli(&a(&["--all", "--format=clippings"]), &paths).unwrap(), 0);
        let clip = std::fs::read_to_string(paths.exports().join("My Clippings.txt")).unwrap();
        assert!(clip.starts_with("\u{feff}The Book\r\n- Your Highlight") && clip.contains("\r\nhi\r\n=========="), "{clip:?}");
        assert_eq!(cli(&a(&["--format", "pdf", "--all"]), &paths).unwrap(), 2);
        assert_eq!(cli(&a(&["--all", "--format"]), &paths).unwrap(), 2);
        for f in ["The Book.json", "My Clippings.txt"] {
            std::fs::remove_file(paths.exports().join(f)).unwrap();
        }
        // A book with nothing to export writes no file and exits 0.
        let empty = dir.join("e.epub");
        std::fs::write(&empty, "y").unwrap();
        let before = std::fs::read_dir(paths.exports()).unwrap().count();
        assert_eq!(cli(&a(&[empty.to_str().unwrap()]), &paths).unwrap(), 0);
        assert_eq!(std::fs::read_dir(paths.exports()).unwrap().count(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
