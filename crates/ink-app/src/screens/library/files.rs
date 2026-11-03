//! A simple file browser for folders outside the library (PLAN phase 4): folders and
//! book files of one directory, a tap on a book opens the reader.

use ink_store::library::BOOK_EXTENSIONS;
use std::path::{Path, PathBuf};

/// One row of the file browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
    /// File size in bytes (0 for folders).
    pub size: u64,
}

/// Whether a file name looks like a book the reader may open (`BOOK_EXTENSIONS`, `.fb2.zip`).
pub(crate) fn is_book_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".fb2.zip") {
        return true;
    }
    Path::new(&lower).extension().and_then(|e| e.to_str()).is_some_and(|e| BOOK_EXTENSIONS.contains(&e))
}

/// Folders and book files in `dir`, folders first, then by name (case-insensitive).
/// Hidden entries (a leading dot) and other files are left out.
pub(crate) fn list_dir(dir: &Path) -> Result<Vec<FileEntry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut v = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        // Follows symlinks, like the reader will.
        let Ok(md) = std::fs::metadata(&path) else { continue };
        if md.is_dir() {
            v.push(FileEntry { path, name, dir: true, size: 0 });
        } else if md.is_file() && is_book_name(&name) {
            v.push(FileEntry { path, name, dir: false, size: md.len() });
        }
    }
    v.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
    Ok(v)
}

/// Where the browser starts: `/mnt/us` on a Kindle, else the folder above the first
/// library folder, else `/`.
pub(crate) fn start_dir(library_dirs: &[PathBuf]) -> PathBuf {
    let us = Path::new("/mnt/us");
    if us.is_dir() {
        return us.to_path_buf();
    }
    library_dirs
        .iter()
        .filter_map(|d| d.parent().filter(|p| p.is_dir()).or(Some(d.as_path()).filter(|d| d.is_dir())))
        .map(Path::to_path_buf)
        .next()
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// "1.2 MB", "340 KB", "12 bytes".
pub(crate) fn size_text(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} bytes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_first_then_books_and_hides_the_rest() {
        let d = std::env::temp_dir().join(format!("ink-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("b-folder")).unwrap();
        std::fs::create_dir_all(d.join("A folder")).unwrap();
        std::fs::create_dir_all(d.join(".hidden")).unwrap();
        for f in ["z.epub", "Book.PDF", "notes.doc", ".x.epub", "story.fb2.zip"] {
            std::fs::write(d.join(f), b"0123").unwrap();
        }
        let names: Vec<String> = list_dir(&d).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["A folder", "b-folder", "Book.PDF", "story.fb2.zip", "z.epub"]);
        assert!(list_dir(&d.join("missing")).is_err());
        assert_eq!(start_dir(&[d.join("b-folder")]), if Path::new("/mnt/us").is_dir() { PathBuf::from("/mnt/us") } else { d.clone() });
        std::fs::remove_dir_all(&d).unwrap();
        assert_eq!(size_text(1_572_864), "1.5 MB");
        assert_eq!(size_text(2048), "2 KB");
        assert_eq!(size_text(12), "12 bytes");
    }
}
