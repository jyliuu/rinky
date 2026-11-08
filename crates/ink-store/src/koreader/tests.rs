use super::*;
use crate::testutil::{set_mtime, TempDir};

const ROLLING: &[u8] = include_bytes!("../../tests/fixtures/metadata.rolling.epub.lua");
const PAGING: &[u8] = include_bytes!("../../tests/fixtures/metadata.paging.pdf.lua");
const LEGACY: &[u8] = include_bytes!("../../tests/fixtures/metadata.legacy.epub.lua");
const LEGACY_PAGING: &[u8] = include_bytes!("../../tests/fixtures/metadata.legacy-paging.pdf.lua");

fn parse(bytes: &[u8], paging: Option<bool>) -> KoBookMetadata {
    let t = lua::parse_table(bytes).unwrap();
    metadata_from_table(Path::new("/x/metadata.lua"), &t, paging)
}

#[test]
fn sidecar_paths_follow_koreader() {
    // Suffix case is kept (DocSettings.getSidecarFilename).
    assert_eq!(metadata_path(Path::new("/mnt/us/Books/juliet.EPUB")), PathBuf::from("/mnt/us/Books/juliet.sdr/metadata.EPUB.lua"));
    assert_eq!(metadata_path(Path::new("/mnt/us/Books/a.fb2.zip")), PathBuf::from("/mnt/us/Books/a.fb2.sdr/metadata.zip.lua"));
    // No suffix: "_" and the whole path.
    assert_eq!(metadata_path(Path::new("/mnt/us/Books/README")), PathBuf::from("/mnt/us/Books/README.sdr/metadata._.lua"));
    // KOReader strips the last '.' of the whole path, even in a directory name.
    assert_eq!(koreader_sidecar_dir(Path::new("/mnt/us/my.books/file")), PathBuf::from("/mnt/us/my.sdr"));
    assert_eq!(history_name(Path::new("/mnt/us/Books/x.epub")), "[#mnt#us#Books#] x.epub.lua");
    // Bytes are kept: a name that is not UTF-8 still finds its files.
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let odd = Path::new(OsStr::from_bytes(b"/b/caf\xe9.epub"));
        assert_eq!(metadata_path(odd), Path::new(OsStr::from_bytes(b"/b/caf\xe9.sdr/metadata.epub.lua")));
        assert_eq!(metadata_path(Path::new(OsStr::from_bytes(b"/b/x.\xe9"))), Path::new(OsStr::from_bytes(b"/b/x.sdr/metadata.\xe9.lua")));
        assert_eq!(history_name(odd), OsStr::from_bytes(b"[#b#] caf\xe9.epub.lua"));
    }
    // A checksum that is not a book id never names a directory (and never panics).
    let ko = KoreaderDirs { root: PathBuf::from("/k") };
    for bad in ["é", "../../x", "", "zz"] {
        let c = metadata_candidates(Path::new("/b/x.epub"), Some(&ko), Some(bad));
        assert!(c.iter().all(|p| !p.starts_with("/k/hashdocsettings")), "{bad:?}: {c:?}");
    }
    let ko = KoreaderDirs { root: PathBuf::from("/mnt/us/koreader") };
    let c = metadata_candidates(Path::new("/mnt/us/Books/x.epub"), Some(&ko), Some("cf7ce9b8d4e135c8ac340f18d1e58c85"));
    let s: Vec<String> = c.iter().map(|p| p.display().to_string()).collect();
    assert_eq!(
        s,
        [
            "/mnt/us/Books/x.sdr/metadata.epub.lua",
            "/mnt/us/Books/x.sdr/metadata.epub.lua.old",
            "/mnt/us/Books/x.sdr/x.epub.lua",
            "/mnt/us/koreader/docsettings/mnt/us/Books/x.sdr/metadata.epub.lua",
            "/mnt/us/koreader/docsettings/mnt/us/Books/x.sdr/metadata.epub.lua.old",
            "/mnt/us/koreader/hashdocsettings/cf/cf7ce9b8d4e135c8ac340f18d1e58c85.sdr/metadata.epub.lua",
            "/mnt/us/koreader/hashdocsettings/cf/cf7ce9b8d4e135c8ac340f18d1e58c85.sdr/metadata.epub.lua.old",
            "/mnt/us/koreader/history/[#mnt#us#Books#] x.epub.lua",
            "/mnt/us/koreader/history/[#mnt#us#Books#] x.epub.lua.old",
            "/mnt/us/Books/x.epub.kpdfview.lua",
        ]
    );
    assert_eq!(metadata_candidates(Path::new("/b/x.epub"), None, None).len(), 4);
    assert_eq!(KoreaderDirs::kindle().statistics(), PathBuf::from("/mnt/us/koreader/settings/statistics.sqlite3"));
    assert_eq!(KoreaderDirs::kindle().vocabulary(), PathBuf::from("/mnt/us/koreader/settings/vocabulary_builder.sqlite3"));
}

#[test]
fn rolling_annotations() {
    let m = parse(ROLLING, Some(false));
    assert_eq!(m.last_xpointer.as_deref(), Some("/body/DocFragment[12]/body/div/p[7]/text().42"));
    assert_eq!(m.position().as_deref(), Some("/body/DocFragment[12]/body/div/p[7]/text().42"));
    assert_eq!(m.percent_finished, Some(0.53846153846154));
    assert_eq!((m.status, m.status_raw.as_deref(), m.rating), (ReadingStatus::Abandoned, Some("abandoned"), Some(4)));
    assert_eq!(m.review.as_deref(), Some("Read again in spring."));
    assert_eq!(m.status_modified.as_deref(), Some("2026-10-05"));
    assert_eq!(m.md5.as_deref(), Some("cf7ce9b8d4e135c8ac340f18d1e58c85"));
    assert_eq!(m.title.as_deref(), Some("Romeo and Juliet"));
    assert_eq!(m.authors, ["William Shakespeare", "Another Editor"]);
    assert_eq!((m.language.as_deref(), m.series.as_deref(), m.series_index), (Some("en"), Some("Tragedies"), Some(3.0)));
    assert_eq!(m.doc_pages, Some(180));
    assert_eq!(m.annotations.len(), 4, "annotations_paging is not used for an EPUB");
    let a = &m.annotations;
    assert_eq!(a[0].kind, KoAnnotationKind::Highlight);
    assert_eq!(a[0].pos0, "/body/DocFragment[5]/body/div/p[12]/text().0");
    assert_eq!(a[0].pos1, "/body/DocFragment[5]/body/div/p[12]/text().38");
    assert_eq!((a[0].drawer.as_deref(), a[0].color.as_deref(), a[0].chapter.as_deref(), a[0].pageno), (Some("lighten"), Some("yellow"), Some("ACT I"), Some(14)));
    assert_eq!(a[1].kind, KoAnnotationKind::Note);
    assert_eq!(a[1].note.as_deref(), Some("The \"name\" speech.\nSee also act III."));
    assert!(a[1].text.contains("\nBy any other word"));
    assert_eq!(a[1].datetime_updated.as_deref(), Some("2026-10-03 09:30:00"));
    assert_eq!(a[2].kind, KoAnnotationKind::Bookmark);
    assert_eq!((a[2].pos0.as_str(), a[2].pos1.as_str()), ("/body/DocFragment[14]/body/div/p[2]/text().0", ""));
    assert_eq!(a[3].kind, KoAnnotationKind::Bookmark);
    assert_eq!(a[3].note.as_deref(), Some("come back here"));
    assert_eq!((m.highlights().count(), m.bookmarks().count()), (2, 2));
    // Without a hint the format is guessed from the annotations.
    assert_eq!(parse(ROLLING, None).annotations, m.annotations);

    let s = m.to_book_state();
    assert_eq!(s.position, m.last_xpointer);
    assert_eq!((s.status, s.rating, s.percent), (ReadingStatus::Abandoned, Some(4), Some(0.53846155)));
    assert_eq!((s.highlights.len(), s.notes().count(), s.bookmarks.len()), (2, 1, 2));
    assert_eq!(s.highlights[1].style, "underscore");
    assert_eq!(s.highlights[0].color.as_deref(), Some("yellow"));
    assert_eq!(s.bookmarks[1].note.as_deref(), Some("come back here"));
    assert_eq!(s.md5.as_deref(), Some("cf7ce9b8d4e135c8ac340f18d1e58c85"));
    assert_eq!(s.imported_from.as_deref(), Some("koreader:/x/metadata.lua"));
}

#[test]
fn paging_annotations() {
    let m = parse(PAGING, Some(true));
    assert_eq!((m.last_page, m.position().as_deref()), (Some(9), Some("page:8")));
    assert_eq!((m.status, m.rating), (ReadingStatus::Complete, Some(5)));
    assert_eq!(m.annotations.len(), 3, "annotations_rolling is not used for a PDF");
    let a = &m.annotations[0];
    assert_eq!((a.kind, a.page), (KoAnnotationKind::Highlight, Some(4)));
    assert_eq!(a.boxes, [KoBox { x: 72.5, y: 100.0, w: 200.25, h: 12.0 }, KoBox { x: 72.5, y: 114.0, w: 80.0, h: 12.0 }]);
    assert_eq!(a.pos0, "page:3;72.5,100,272.75,112");
    assert_eq!(a.pos1, "page:3;72.5,114,152.5,126");
    assert_eq!(m.annotations[1].kind, KoAnnotationKind::Note);
    assert_eq!(m.annotations[1].drawer.as_deref(), Some("strikeout"));
    assert_eq!((m.annotations[2].kind, m.annotations[2].pos0.as_str()), (KoAnnotationKind::Bookmark, "page:11"));
    assert_eq!(parse(PAGING, None).annotations, m.annotations);
    let s = m.to_book_state();
    assert_eq!((s.position.as_deref(), s.page), (Some("page:8"), Some(8)));
    let h = &s.highlights[0];
    assert_eq!(h.page, Some(3));
    assert_eq!(h.quads.len(), 2);
    assert_eq!((h.quads[0].x0, h.quads[0].y0, h.quads[0].x1, h.quads[0].y1), (72.5, 100.0, 272.75, 112.0));
    assert_eq!(s.bookmarks[0].page, Some(11));
}

#[test]
fn legacy_formats() {
    let m = parse(LEGACY, Some(false));
    assert_eq!(m.status, ReadingStatus::Reading, "no summary: reading because a position exists");
    assert_eq!(m.percent_finished, Some(1.0));
    let a = &m.annotations;
    assert_eq!(a.len(), 4, "{a:#?}");
    assert_eq!((a[0].kind, a[0].text.as_str(), a[0].note.as_deref(), a[0].drawer.as_deref()), (KoAnnotationKind::Note, "Two households", Some("my note"), Some("invert")));
    assert_eq!((a[1].kind, a[1].note.as_deref(), a[1].drawer.as_deref()), (KoAnnotationKind::Highlight, None, Some("underscore")), "automatic text is not a note");
    assert_eq!((a[2].kind, a[2].pos0.as_str()), (KoAnnotationKind::Bookmark, "/body/DocFragment[4]/body/p[1]/text().0"));
    assert_eq!((a[3].kind, a[3].text.as_str(), a[3].datetime.as_str()), (KoAnnotationKind::Highlight, "an old highlight", "2013-01-01 09:00:00"));

    let m = parse(LEGACY_PAGING, Some(true));
    assert_eq!(m.annotations.len(), 2, "highlights_imported: nothing extra from `highlight`");
    let h = &m.annotations[0];
    assert_eq!((h.kind, h.page, h.drawer.as_deref(), h.note.as_deref()), (KoAnnotationKind::Highlight, Some(3), Some("lighten"), None));
    assert_eq!(h.pos0, "page:2;48,58,152,74");
    let b = &m.annotations[1];
    assert_eq!((b.kind, b.page, b.note.as_deref(), b.pos0.as_str()), (KoAnnotationKind::Bookmark, Some(5), Some("remember"), "page:4"));
    assert_eq!(m.position().as_deref(), Some("page:4"));
}

/// A highlighted legacy bookmark takes the style of the first highlight with equal
/// positions whose datetime does not differ; any other highlight is an old one of its
/// own. -0.0 equals 0.0; NaN equals nothing.
#[test]
fn legacy_bookmarks_match_highlights_by_position_and_time() {
    let styles = |m: &KoBookMetadata| m.annotations.iter().map(|a| (a.datetime.clone(), a.drawer.clone().unwrap_or_default())).collect::<Vec<_>>();
    let pair = |d: &str, s: &str| (d.to_string(), s.to_string());
    let rolling = br#"return { bookmarks = { { datetime = "d1", highlighted = true, notes = "a", pos0 = "/p[1]", pos1 = "/p[2]", page = "/p[1]" } },
        highlight = { [1] = { { datetime = "d2", drawer = "invert", pos0 = "/p[1]", pos1 = "/p[2]" },
                              { datetime = "d1", drawer = "underscore", pos0 = "/p[1]", pos1 = "/p[2]" },
                              { drawer = "strikeout", pos0 = "/p[1]", pos1 = "/p[2]" } } } }"#;
    assert_eq!(styles(&parse(rolling, Some(false))), [pair("d1", "underscore"), pair("d2", "invert")]);
    let paging = |x: &str| {
        format!(
            r#"return {{ bookmarks = {{ {{ datetime = "d", highlighted = true, page = 3, pos0 = {{ x = {x}, y = 1 }}, pos1 = {{ x = 2, y = 3 }} }} }},
               highlight = {{ [3] = {{ {{ datetime = "d", drawer = "underscore", pos0 = {{ x = 0, y = 1 }}, pos1 = {{ x = 2, y = 3 }} }} }} }} }}"#
        )
    };
    assert_eq!(styles(&parse(paging("-0.0").as_bytes(), Some(true))), [pair("d", "underscore")]);
    assert_eq!(styles(&parse(paging("nan").as_bytes(), Some(true))), [pair("d", ""), pair("d", "underscore")]);
}

#[test]
fn bad_and_hostile_files() {
    let tmp = TempDir::new("ko-bad");
    let f = tmp.path().join("metadata.epub.lua");
    for (content, what) in [
        (&b"return {}"[..], "empty table"),
        (b"return os.execute('touch /tmp/pwned')", "a call"),
        (b"local t = {} return t", "a statement"),
        (b"return { last_xpointer = io.open('x') }", "a call in a field"),
        (b"\x00\x01garbage", "garbage"),
    ] {
        std::fs::write(&f, content).unwrap();
        assert!(read_metadata_file(&f, None).is_err(), "{what} accepted");
    }
    // Wrong types are ignored field by field.
    std::fs::write(&f, b"return { last_xpointer = 5, percent_finished = 'x', summary = 'no', annotations = { 1, 'a', {} }, last_page = -3 }").unwrap();
    let m = read_metadata_file(&f, Some(false)).unwrap();
    assert_eq!((m.last_xpointer, m.percent_finished, m.status, m.last_page, m.annotations.len()), (None, None, ReadingStatus::New, None, 0));
    // A rating outside 1..=5 and an unknown status are ignored.
    std::fs::write(&f, b"return { summary = { rating = 9, status = 'on shelf' }, last_page = 2 }").unwrap();
    let m = read_metadata_file(&f, None).unwrap();
    assert_eq!((m.rating, m.status), (None, ReadingStatus::Reading));
}

#[test]
fn candidates_most_recent_first_and_backup_fallback() {
    let tmp = TempDir::new("ko-cand");
    let book = tmp.path().join("Books/juliet.epub");
    std::fs::create_dir_all(book.parent().unwrap()).unwrap();
    std::fs::write(&book, b"book").unwrap();
    let ko = KoreaderDirs { root: tmp.path().join("koreader") };
    let doc = metadata_path(&book);
    let dir_loc = metadata_candidates(&book, Some(&ko), None)[3].clone();
    std::fs::create_dir_all(doc.parent().unwrap()).unwrap();
    std::fs::create_dir_all(dir_loc.parent().unwrap()).unwrap();
    std::fs::write(&doc, ROLLING).unwrap();
    std::fs::write(&dir_loc, LEGACY).unwrap();
    set_mtime(&doc, 100);
    set_mtime(&dir_loc, 10);
    // The newer docsettings file wins, as in KOReader.
    let m = read_book_metadata_in(&book, Some(&ko), None).unwrap();
    assert_eq!(m.path, dir_loc);
    // Without KOReader's directory only the book folder is searched.
    assert_eq!(read_book_metadata(&book).unwrap().path, doc);
    // A corrupt primary falls back to its .old backup, which never sorts ahead of it.
    std::fs::write(&doc, b"return { broken").unwrap();
    let old = with_suffix(&doc, ".old");
    std::fs::write(&old, PAGING).unwrap();
    set_mtime(&doc, 50);
    set_mtime(&old, 5);
    let order = existing_candidates(&metadata_candidates(&book, None, None));
    assert_eq!(order, [doc.clone(), old.clone()], "backup newer than primary is fudged to the primary's time");
    let m = read_book_metadata(&book).unwrap();
    assert_eq!(m.path, old);
    // Nothing at all: NotFound.
    let other = tmp.path().join("Books/none.epub");
    assert!(matches!(read_book_metadata(&other), Err(StoreError::NotFound(_))));
}

#[test]
fn import_merges_into_our_sidecar_without_touching_koreader() {
    let tmp = TempDir::new("ko-import");
    let book = tmp.path().join("juliet.epub");
    std::fs::write(&book, b"pretend epub").unwrap();
    let ko_file = metadata_path(&book);
    std::fs::create_dir_all(ko_file.parent().unwrap()).unwrap();
    std::fs::write(&ko_file, ROLLING).unwrap();
    set_mtime(&ko_file, 1000);
    let before = (std::fs::read(&ko_file).unwrap(), std::fs::metadata(&ko_file).unwrap().modified().unwrap());
    let listing = |d: &Path| {
        let mut v: Vec<String> = std::fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    };
    // Our sidecar has a position already: KOReader's does not replace it by default.
    let mine = BookState { position: Some("/body/DocFragment[1]/body/p[1]/text().0".into()), ..Default::default() };
    mine.save(&book).unwrap();
    let sum = import_book(&book, None, false).unwrap().unwrap();
    assert_eq!(sum, ImportSummary { position: false, highlights: 2, bookmarks: 2 });
    let s = BookState::load(&book).unwrap();
    assert_eq!(s.position, mine.position);
    assert_eq!((s.highlights.len(), s.bookmarks.len(), s.rating), (2, 2, Some(4)));
    assert_eq!(s.md5, Some(crate::partial_md5(&book).unwrap()), "the file's own hash, not KOReader's stored one");
    // Importing again adds nothing.
    assert_eq!(import_book(&book, None, false).unwrap().unwrap(), ImportSummary::default());
    // prefer_koreader takes the position over.
    assert!(import_book(&book, None, true).unwrap().unwrap().position);
    assert_eq!(BookState::load(&book).unwrap().position.as_deref(), Some("/body/DocFragment[12]/body/div/p[7]/text().42"));
    // KOReader's file: same bytes, same mtime; the .sdr holds only its file and ours.
    let after = (std::fs::read(&ko_file).unwrap(), std::fs::metadata(&ko_file).unwrap().modified().unwrap());
    assert_eq!(before, after);
    let ours = crate::sidecar::sidecar_path(&book);
    assert_eq!(ours.parent(), ko_file.parent(), "one .sdr for both readers");
    assert_eq!(listing(ko_file.parent().unwrap()), ["metadata.epub.lua", "rinky.epub.json", "rinky.epub.json.old"]);
    // No KOReader data: None.
    let lone = tmp.path().join("lone.epub");
    std::fs::write(&lone, b"x").unwrap();
    assert_eq!(import_book(&lone, None, false).unwrap(), None);
}

/// A real KOReader sidecar and its book from a device backup (outside git). Set
/// `KOREADER_SAMPLE_DIR` to a directory holding `help/<book>` and `help/<book>.sdr/`;
/// skipped when absent.
#[test]
fn real_koreader_sidecar_if_available() {
    let dir = std::env::var("KOREADER_SAMPLE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../inkreader-wt/.cache/store-ko/koreader")));
    let book = dir.join("help/quickstart-en-v2025.08.html");
    if !book.exists() {
        eprintln!("skipped: {} not found", book.display());
        return;
    }
    let m = read_book_metadata(&book).unwrap();
    assert_eq!(m.md5.as_deref(), Some(crate::partial_md5(&book).unwrap().as_str()), "partial MD5 equals KOReader's stored checksum");
    assert_eq!(m.md5.as_deref(), Some("f57f1d89ea1f37a9c43b68371cb7899d"));
    assert_eq!(m.last_xpointer.as_deref(), Some("/html/body/p[19]/strong/text()[1].0"));
    assert_eq!((m.status, m.percent_finished, m.title.as_deref()), (ReadingStatus::Complete, Some(1.0), Some("KOReader Quickstart Guide")));
    assert!(m.annotations.is_empty());
}

#[test]
fn sync_takes_the_newer_position_and_merges_annotations() {
    let tmp = TempDir::new("ko-sync");
    let book = tmp.path().join("juliet.epub");
    std::fs::write(&book, b"pretend epub").unwrap();
    let ko_file = metadata_path(&book);
    std::fs::create_dir_all(ko_file.parent().unwrap()).unwrap();
    std::fs::write(&ko_file, ROLLING).unwrap();
    set_mtime(&ko_file, 1000);
    let ours = crate::sidecar::sidecar_path(&book);
    let mine = BookState { position: Some("/body/DocFragment[1]/body/p[1]/text().0".into()), ..Default::default() };
    mine.save(&book).unwrap();
    // Our sidecar is newer: our position stays, KOReader's annotations come in.
    let r = sync_book(&book, None, false).unwrap().unwrap();
    assert!(!r.koreader_newer && !r.summary.position);
    assert_eq!((r.summary.highlights, r.summary.bookmarks), (2, 2));
    assert_eq!(BookState::load(&book).unwrap().position, mine.position);
    // KOReader's file is newer: its position wins; nothing is added twice.
    set_mtime(&ours, 2000);
    let dry = sync_book(&book, None, true).unwrap().unwrap();
    assert!(dry.koreader_newer && dry.summary.position);
    assert_eq!(BookState::load(&book).unwrap().position, mine.position, "a dry run writes nothing");
    let r = sync_book(&book, None, false).unwrap().unwrap();
    assert_eq!((r.summary.highlights, r.summary.bookmarks), (0, 0));
    assert_eq!(BookState::load(&book).unwrap().position.as_deref(), Some("/body/DocFragment[12]/body/div/p[7]/text().42"));
    assert_eq!(r.source, ko_file);
}
