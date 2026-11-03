//! The library path: `ink_store::LibraryIndex::scan` lists the files, `read_info` fills
//! title and authors (what the library screen does in wave B). Uses only public APIs of
//! both crates; `ink-store` is a dev-dependency here, never a dependency.

use ink_formats::Format;
use ink_store::library::LibraryIndex;
use std::path::PathBuf;

fn testdata() -> PathBuf {
    match std::env::var_os("INK_FORMATS_TESTDATA") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata"),
    }
}

#[test]
fn scan_then_fill_from_read_info() {
    let dir = testdata().join("formats");
    let mut lib = LibraryIndex::default();
    let (added, _) = lib.scan(&[&dir], 1);
    assert!(added >= 25, "{added}");
    let mut refused = 0;
    let mut covers = 0;
    for b in &mut lib.books {
        let info = ink_formats::read_info(&b.path).unwrap_or_else(|e| panic!("{}: {e}", b.path.display()));
        b.title = info.metadata.title.clone();
        b.authors = info.metadata.authors.clone();
        refused += usize::from(!info.readable());
        covers += usize::from(info.cover.is_some());
        assert!(!b.title.is_empty(), "{}", b.path.display());
    }
    let title = |name: &str| lib.books.iter().find(|b| b.path.ends_with(name)).map(|b| b.title.clone());
    assert_eq!(title("sample.fb2").as_deref(), Some("FB2 Sample Book"));
    assert_eq!(title("alice-kf8.azw3").as_deref(), Some("Alice's Adventures in Wonderland"));
    assert_eq!(title("sample.fb3").as_deref(), Some("FB3 Sample Book"));
    assert_eq!(title("epub3-nav.epub").as_deref(), Some("EPUB 3 Navigation Sample"));
    assert_eq!(title("ru-cp1251.txt").as_deref(), Some("ru-cp1251"));
    assert_eq!(title("kfx-container.kfx").as_deref(), Some("KFX Sample Title"));
    assert!(refused >= 4, "DRM/KFX books are listed with a refusal: {refused}");
    assert!(covers >= 8, "{covers}");
    let alice = lib.books.iter().find(|b| b.path.ends_with("alice-old.mobi")).unwrap();
    assert_eq!(alice.authors, vec!["Lewis Carroll"]);

    // Files ink-formats reads that the scanner does not list (suffixes such as
    // .fb2.zip, .azw1).
    let mut missed = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if ink_formats::detect(&p).is_some() && lib.get(&p).is_none() {
            missed.push(p.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    missed.sort();
    eprintln!("not listed by LibraryIndex::scan: {missed:?}");
    assert!(missed.iter().all(|m| matches!(ink_formats::detect(&dir.join(m)), Some(Format::Fb2 | Format::Topaz))), "{missed:?}");
}
