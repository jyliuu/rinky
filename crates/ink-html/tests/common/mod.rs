//! Helpers shared by the integration tests.
#![allow(dead_code)]

use ink_doc::{Metadata, ResourceLoader};
use ink_formats::epub::{read_package, ZipLoader};
use ink_html::book::Book;
use std::path::{Path, PathBuf};

/// The crate directory: `INK_MANIFEST_DIR` when set (test binaries run on the Kindle
/// emulator with the fixtures copied elsewhere), else the build-time directory.
pub fn manifest_dir() -> PathBuf {
    std::env::var_os("INK_MANIFEST_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

/// The repository root.
pub fn root() -> PathBuf {
    manifest_dir().join("../..")
}

/// A fixture EPUB under `crates/ink-html/tests/fixtures`, or `testdata/juliet.epub`.
pub fn book_path(name: &str) -> PathBuf {
    if name == "juliet" {
        return root().join("testdata/juliet.epub");
    }
    manifest_dir().join(format!("tests/fixtures/{name}.epub"))
}

/// Spine (path, html) pairs exactly as the EPUB loader passes them (missing manifest
/// ids as empty paths), the metadata and the loader.
pub fn spine(path: &Path) -> (Metadata, Vec<(String, String)>, ZipLoader<std::fs::File>) {
    let zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let loader = ZipLoader::new(zip);
    let pkg = read_package(&loader).unwrap();
    let chapters = pkg
        .spine
        .iter()
        .map(|id| {
            let p = pkg.item_path(id).unwrap_or_default();
            let html = if p.is_empty() { String::new() } else { loader.read(&p).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default() };
            (p, html)
        })
        .collect();
    (pkg.metadata, chapters, loader)
}

/// The shared book data of an EPUB.
pub fn book(path: &Path) -> Book {
    let (meta, chapters, loader) = spine(path);
    Book::new(meta, chapters, &loader)
}
