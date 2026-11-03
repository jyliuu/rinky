//! Integration tests over the fixtures in `testdata/formats` (one test per format and
//! acceptance check). `INK_FORMATS_TESTDATA` points at the `testdata` directory when the
//! test binary runs elsewhere (the emulator); otherwise the repository's is used.

use ink_doc::{DocError, Document, LayoutSettings};
use ink_formats::book::{toc_depth, toc_len};
use ink_formats::{Book, Format, Target, TocNode};
use std::path::PathBuf;
use std::time::Duration;

fn testdata() -> PathBuf {
    match std::env::var_os("INK_FORMATS_TESTDATA") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata"),
    }
}

fn fx(name: &str) -> PathBuf {
    testdata().join("formats").join(name)
}

fn titles(nodes: &[TocNode], depth: usize, out: &mut Vec<String>) {
    for n in nodes {
        out.push(format!("{}{}", "  ".repeat(depth), n.title));
        titles(&n.children, depth + 1, out);
    }
}

fn toc_titles(book: &Book) -> Vec<String> {
    let mut v = Vec::new();
    titles(&book.toc, 0, &mut v);
    v
}

/// Every `src`/`href` to a resource of the book resolves through its loader.
fn assert_resources_resolve(book: &Book) {
    for ch in &book.chapters {
        let dir = ch.path.rfind('/').map(|i| &ch.path[..i + 1]).unwrap_or("");
        let mut rest = ch.html.as_str();
        while let Some(i) = rest.find(" src=\"") {
            let after = &rest[i + 6..];
            let end = after.find('"').unwrap();
            let src = &after[..end];
            if !src.contains(':') {
                let path = ink_doc::resolve_href(dir, src);
                assert!(book.resources.read(&path).is_ok(), "{}: {src} -> {path} missing", ch.path);
            }
            rest = &after[end..];
        }
    }
}

/// Lays the book out and renders its first page; returns the page count.
fn render_first_page(book: Book) -> usize {
    let mut doc = book.into_document().unwrap();
    doc.set_layout(&LayoutSettings::for_screen(1272, 1696)).unwrap();
    // TOC pages and the page count need the whole page map: wait for the paginator.
    assert!(doc.wait_paginated(Duration::from_secs(120)), "pagination did not finish");
    let mut c = ink_device::Canvas::new(1272, 1696);
    doc.render_page(0, &mut c).unwrap();
    assert!(c.pixels.iter().any(|p| *p < 128), "page 0 is blank");
    // TOC entries map to pages once pagination has finished (it runs on a thread).
    assert!(doc.wait_paginated(std::time::Duration::from_secs(300)), "pagination did not finish");
    let pages: Vec<usize> = doc.toc().iter().map(|e| e.page.unwrap()).collect();
    assert!(pages.windows(2).all(|w| w[0] <= w[1]), "{pages:?}");
    doc.page_count().pages
}

fn refused(r: Result<Book, DocError>) -> String {
    match r {
        Err(DocError::Unsupported(m)) => m,
        Err(e) => panic!("expected a refusal, got {e}"),
        Ok(_) => panic!("expected a refusal, got a book"),
    }
}

// ---------------------------------------------------------------- EPUB (check 1)

#[test]
fn epub3_nav_page_list_metadata_fonts() {
    let p = fx("epub3-nav.epub");
    let info = ink_formats::read_info(&p).unwrap();
    assert_eq!(info.format, Format::Epub);
    assert_eq!(info.version, "EPUB 3.0");
    assert_eq!(info.metadata.title, "EPUB 3 Navigation Sample");
    assert_eq!(info.metadata.authors, vec!["Inkreader Fixtures"]);
    assert_eq!(info.metadata.language.as_deref(), Some("en"));
    assert_eq!(info.metadata.series, Some(("Inkreader Tests".into(), 1.0)));
    assert_eq!(info.metadata.publisher.as_deref(), Some("inkreader"));
    assert_eq!(info.metadata.identifier.as_deref(), Some("urn:uuid:5e1a8f53-7c1d-4c55-9a51-2f3b4c5d6e7f"));
    let ids: Vec<(Option<&str>, &str)> = info.identifiers.iter().map(|i| (i.scheme.as_deref(), i.value.as_str())).collect();
    assert_eq!(ids, vec![(Some("UUID"), "urn:uuid:5e1a8f53-7c1d-4c55-9a51-2f3b4c5d6e7f"), (Some("ISBN"), "urn:isbn:9780000000002")]);
    assert_eq!(info.metadata.description.as_deref(), Some("An EPUB 3 with a nested nav, a page list and obfuscated fonts."));
    assert_eq!(info.date.as_deref(), Some("2026-10-24"));
    let cover = info.cover.as_ref().unwrap();
    assert_eq!((cover.path.as_str(), cover.media_type.as_str()), ("OEBPS/images/cover.png", "image/png"));
    assert_eq!(info.fonts, vec!["OEBPS/fonts/plain.ttf", "OEBPS/fonts/idpf.ttf", "OEBPS/fonts/adobe.ttf"]);
    assert!(info.readable());

    let book = ink_formats::load(&p).unwrap();
    assert_eq!(book.chapters.len(), 4, "3 chapters + the nav document (linear=no)");
    assert_eq!(
        toc_titles(&book),
        vec!["First Chapter", "  First, Section A", "Later Chapters", "  Second Chapter", "    Second, Section A", "  Third Chapter"]
    );
    assert_eq!(toc_depth(&book.toc), 3);
    assert_eq!(book.toc[1].children[0].target, Some(Target::id(1, "top")));
    assert_eq!(book.toc[1].children[0].children[0].target, Some(Target::id(1, "s2")));
    let labels: Vec<&str> = book.page_list.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(labels, vec!["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
    assert_eq!(book.page_list[4].target, Target::id(1, "page5"));
    // Fonts: both obfuscated copies read back equal to the plain one and the source.
    let plain = book.resources.read("OEBPS/fonts/plain.ttf").unwrap();
    assert_eq!(plain, std::fs::read(fx("font-subset.ttf")).unwrap());
    assert_eq!(book.resources.read("OEBPS/fonts/idpf.ttf").unwrap(), plain);
    assert_eq!(book.resources.read("OEBPS/fonts/adobe.ttf").unwrap(), plain);
    assert_eq!(&plain[..4], &[0, 1, 0, 0]);
    assert_resources_resolve(&book);

    // Through the document: xpointers, pages, page labels.
    let (toc, labels) = book.positions();
    assert_eq!(toc[3].title, "Second, Section A");
    assert_eq!(toc[3].position.to_string(), "/body/DocFragment[2]/body/h2");
    assert_eq!(labels[0].1.to_string(), "/body/DocFragment[1]/body/span[1]");
    let mut doc = ink_formats::open_book(&p).unwrap();
    doc.set_layout(&LayoutSettings::for_screen(1272, 1696)).unwrap();
    assert!(doc.wait_paginated(Duration::from_secs(120)), "pagination did not finish");
    assert_eq!(doc.page_labels().len(), 9);
    assert_eq!(doc.toc().len(), 5, "the group heading without a target is not a TOC entry");
    assert!(doc.toc().iter().all(|e| e.page.is_some()));
}

#[test]
fn epub2_ncx_guide_cover_series() {
    let p = fx("epub2-ncx.epub");
    let info = ink_formats::read_info(&p).unwrap();
    assert_eq!(info.version, "EPUB 2.0");
    assert_eq!(info.metadata.authors, vec!["Inkreader Fixtures"], "the illustrator is not an author");
    assert_eq!(info.metadata.series, Some(("Inkreader Tests".into(), 2.0)));
    assert_eq!(info.identifiers[0].scheme.as_deref(), Some("UUID"));
    assert_eq!(info.identifiers[1].scheme.as_deref(), Some("ISBN"));
    assert_eq!(info.cover.as_ref().unwrap().path, "OEBPS/images/cover.png");
    let book = ink_formats::load(&p).unwrap();
    assert_eq!(
        toc_titles(&book),
        vec!["Chapter 1", "  Section 1.1", "Chapter 2", "  Section 2.1", "Chapter 3", "  Section 3.1"]
    );
    assert_eq!(book.toc[1].children[0].target, Some(Target::id(2, "sec2")));
    assert_eq!(book.page_list.len(), 3);
    assert_eq!(book.page_list[0].target, Target::id(1, "pg1"));
    assert_resources_resolve(&book);
    assert!(render_first_page(book) > 3);
}

#[test]
fn drm_epubs_are_listed_and_refused() {
    for (name, scheme) in [("drm-adept.epub", "Adobe ADEPT"), ("drm-lcp.epub", "Readium LCP")] {
        let p = fx(name);
        let info = ink_formats::read_info(&p).unwrap();
        assert_eq!(info.metadata.title, "Protected Sample", "{name}");
        assert_eq!(info.drm.as_deref(), Some(scheme));
        assert!(info.cover.is_some(), "{name}: the cover is not encrypted");
        assert!(!info.readable());
        let msg = refused(ink_formats::load(&p));
        assert!(msg.contains("DRM") && msg.contains(scheme), "{msg}");
        assert!(ink_formats::open_book(&p).is_err());
    }
}

#[test]
fn juliet_metadata_and_ncx() {
    let p = testdata().join("juliet.epub");
    let info = ink_formats::read_info(&p).unwrap();
    assert_eq!(info.metadata.title, "Romeo and Juliet");
    assert_eq!(info.metadata.authors, vec!["William Shakespeare"]);
    assert!(info.cover.as_ref().is_some_and(|c| c.media_type == "image/png"));
    let book = ink_formats::load(&p).unwrap();
    // Every one of the 30 spine itemrefs is a chapter (29 have text, see the lib tests).
    assert_eq!(book.chapters.len(), 30);
    assert!(toc_len(&book.toc) >= 5, "{:?}", toc_titles(&book));
}

// ---------------------------------------------------------------- FB2 / FB3 (check 2)

#[test]
fn fb2_sample_structure() {
    let p = fx("sample.fb2");
    let info = ink_formats::read_info(&p).unwrap();
    assert_eq!(info.format, Format::Fb2);
    assert_eq!(info.metadata.title, "FB2 Sample Book");
    assert_eq!(info.metadata.authors, vec!["Inkreader Fixtures"]);
    assert_eq!(info.metadata.series, Some(("Inkreader Tests".into(), 3.0)));
    assert_eq!(info.subjects, vec!["prose_classic", "fairy_tale"]);
    assert_eq!(info.cover.as_ref().map(|c| (c.path.as_str(), c.media_type.as_str())), Some(("images/cover.png", "image/png")));
    let book = ink_formats::load(&p).unwrap();
    assert_eq!(
        toc_titles(&book),
        vec!["Part One Prose", "  Chapter 1 The Lamp", "  Chapter 2 The Poem", "Part Two", "  Chapter 3", "Notes"]
    );
    let all: String = book.chapters.iter().map(|c| c.html.as_str()).collect();
    for needle in [
        "<blockquote class=\"epigraph\">",
        "<p class=\"text-author\">The Converter</p>",
        "<div class=\"poem\">",
        "<p class=\"v\">The rain is on the window,</p>",
        "<blockquote class=\"cite\">",
        "<p class=\"subtitle\">* * *</p>",
        "<th>Format</th>",
        "<div class=\"image\"><img src=\"images/pic.png\" alt=\"A dark rectangle\"/></div>",
        "class=\"note\" epub:type=\"noteref\">[1]</a>",
        "id=\"n2\" epub:type=\"footnote\"",
        "<sub>2</sub>",
        "<s>struck</s>",
        "<span class=\"big\">A styled run</span>",
    ] {
        assert!(all.contains(needle), "missing {needle}");
    }
    assert!(book.resources.read("fb2.css").unwrap().ends_with(b".big { font-size: 120%; }"));
    assert_resources_resolve(&book);
    let zipped = ink_formats::load(&fx("sample.fb2.zip")).unwrap();
    assert_eq!(zipped.chapters, book.chapters);
    assert_eq!(ink_formats::read_info(&fx("sample.fb2.zip")).unwrap().version, "FB2");
    assert!(render_first_page(book) > 5);
}

#[test]
fn fb2_cp1251_and_calibre_output() {
    let book = ink_formats::load(&fx("sample-cp1251.fb2")).unwrap();
    assert_eq!(book.info.metadata.title, "Евгений Онегин");
    assert_eq!(book.info.metadata.authors, vec!["Александр Сергеевич Пушкин"]);
    assert_eq!(book.info.metadata.series, Some(("Романы в стихах".into(), 1.0)));
    assert_eq!(toc_titles(&book), vec!["Глава первая", "  I", "  II", "Глава вторая"]);
    assert!(book.chapters.iter().any(|c| c.html.contains("Мой дядя самых честных правил")));
    let calibre = ink_formats::load(&fx("alice-calibre.fb2")).unwrap();
    assert_eq!(calibre.info.metadata.title, "Alice's Adventures in Wonderland");
    let t = toc_titles(&calibre);
    assert!(t.iter().any(|t| t.contains("Down the Rabbit-Hole")), "{t:?}");
    assert!(render_first_page(calibre) > 50);
}

#[test]
fn fb3_package() {
    let p = fx("sample.fb3");
    assert_eq!(ink_formats::format_of(&p), Some(Format::Fb3));
    let info = ink_formats::read_info(&p).unwrap();
    assert_eq!(info.metadata.title, "FB3 Sample Book");
    assert!(info.cover.is_some());
    let book = ink_formats::load(&p).unwrap();
    assert_eq!(toc_titles(&book), vec!["Chapter One", "Chapter Two", "  Two, Part A", "Notes"]);
    assert_resources_resolve(&book);
    assert!(render_first_page(book) >= 3);
}

// ---------------------------------------------------------------- TXT (check 3)

#[test]
fn txt_encodings() {
    let en_chapters = ["CHAPTER I. The First Test", "CHAPTER II. The Second Test", "CHAPTER III. The Last Test"];
    for (name, encoding, needle) in [
        ("en-utf8.txt", "UTF-8", "The lamp on the desk"),
        ("en-utf8-bom.txt", "UTF-8", "The lamp on the desk"),
        ("en-utf16le-bom.txt", "UTF-16LE", "The lamp on the desk"),
        ("en-utf16be-nobom.txt", "UTF-16BE", "The lamp on the desk"),
        ("da-cp1252.txt", "windows-1252", "Æbler, øl og små søde kager"),
        ("ru-cp1251.txt", "windows-1251", "Мой дядя самых честных правил"),
        ("ru-koi8r.txt", "KOI8", "Мой дядя самых честных правил"),
        ("ja-shiftjis.txt", "Shift_JIS", "吾輩は猫である"),
    ] {
        let p = fx(&format!("txt/{name}"));
        let info = ink_formats::read_info(&p).unwrap();
        assert!(info.version.starts_with(&format!("TXT {encoding}")), "{name}: {}", info.version);
        let book = ink_formats::load(&p).unwrap();
        assert!(book.chapters.iter().any(|c| c.html.contains(needle)), "{name}");
        if name.starts_with("en-") {
            let t = toc_titles(&book);
            assert_eq!(t, en_chapters, "{name}");
        }
    }
    let da = ink_formats::load(&fx("txt/da-cp1252.txt")).unwrap();
    assert_eq!(toc_titles(&da), vec!["KAPITEL 1", "KAPITEL 2"]);
    let ru = ink_formats::load(&fx("txt/ru-cp1251.txt")).unwrap();
    assert_eq!(toc_titles(&ru), vec!["Глава 1", "Глава 2"]);
    let ind = ink_formats::load(&fx("txt/indented.txt")).unwrap();
    assert!(ind.chapters[0].html.contains("<p>The first paragraph is indented and wraps onto a second line.</p>"));
}

#[test]
fn txt_gutenberg_alice() {
    let p = fx("alice.txt");
    let info = ink_formats::read_info(&p).unwrap();
    assert_eq!(info.metadata.title, "Alice's Adventures in Wonderland");
    assert_eq!(info.metadata.authors, vec!["Lewis Carroll"]);
    assert_eq!(info.metadata.language.as_deref(), Some("en"));
    let book = ink_formats::load(&p).unwrap();
    let t = toc_titles(&book);
    assert_eq!(t[0], "CHAPTER I. Down the Rabbit-Hole");
    assert_eq!(t[11], "CHAPTER XII. Alice’s Evidence");
    // The licence at the end has numbered sections of its own.
    assert_eq!(t.len(), 17, "{t:?}");
    assert!(t[12].starts_with("Section 1."));
    assert!(render_first_page(book) > 100);
}

// ---------------------------------------------------------------- HTML (check 4)

#[test]
fn single_html_file() {
    let p = fx("sample.html");
    let info = ink_formats::read_info(&p).unwrap();
    assert_eq!(info.format, Format::Html);
    assert_eq!(info.version, "HTML windows-1252");
    assert_eq!(info.metadata.title, "Prinsessen på ærten");
    assert_eq!(info.metadata.authors, vec!["H. C. Andersen"]);
    assert_eq!(info.metadata.language.as_deref(), Some("da"));
    let book = ink_formats::load(&p).unwrap();
    assert_eq!(toc_titles(&book), vec!["Prinsessen på ærten", "  Anden del", "  Tredje del"]);
    assert_eq!(book.toc[0].children[1].target, Some(Target::id(0, "tredje")));
    let html = &book.chapters[0].html;
    assert!(!html.contains("not shown"));
    assert!(html.contains("<img src=\"html-pic.png\" alt=\"pic\"/>"));
    assert_resources_resolve(&book);
    assert!(render_first_page(book) >= 2);
}

// ---------------------------------------------------------------- MOBI / AZW3 (check 5)

#[test]
fn azw3_kf8_gutenberg() {
    let p = fx("alice-kf8.azw3");
    let info = ink_formats::read_info(&p).unwrap();
    assert_eq!(info.format, Format::Mobi);
    assert!(info.version.starts_with("KF8 (AZW3)"), "{}", info.version);
    assert_eq!(info.metadata.title, "Alice's Adventures in Wonderland");
    assert_eq!(info.metadata.authors, vec!["Lewis Carroll"]);
    assert_eq!(info.metadata.language.as_deref(), Some("en"));
    let cover = info.cover.unwrap();
    assert_eq!((cover.media_type.as_str(), cover.bytes.len()), ("image/jpeg", 104775));
    let book = ink_formats::load(&p).unwrap();
    assert_eq!(book.chapters.len(), 19);
    let t = toc_titles(&book);
    assert_eq!(t.len(), 16);
    assert_eq!(t[3], "CHAPTER I. Down the Rabbit-Hole");
    assert_eq!(book.toc[3].target, Some(Target::id(4, "calibre_pb_1")));
    // Stylesheets come from FDST flows; links are rewritten.
    let all: String = book.chapters.iter().map(|c| c.html.as_str()).collect();
    assert!(all.contains("href=\"flows/flow0001.css\""));
    assert!(!all.contains("kindle:"), "unresolved kindle: URI");
    assert!(book.resources.read("flows/flow0001.css").is_ok());
    assert_resources_resolve(&book);
    assert!(render_first_page(book) > 100);
}

#[test]
fn mobi6_gutenberg() {
    let p = fx("alice-old.mobi");
    let info = ink_formats::read_info(&p).unwrap();
    assert!(info.version.starts_with("MOBI 6"), "{}", info.version);
    assert_eq!(info.cover.as_ref().map(|c| c.bytes.len()), Some(104775));
    let book = ink_formats::load(&p).unwrap();
    assert_eq!(book.chapters.len(), 20);
    let t = toc_titles(&book);
    assert_eq!(t.len(), 16);
    assert_eq!(book.toc[3].target, Some(Target::id(4, "filepos4363")));
    assert!(book.chapters[4].html.contains("<a id=\"filepos4363\"></a>"));
    // The contents page links into other chapters by filepos.
    assert!(book.chapters[3].html.contains("href=\"part0004.xhtml#filepos4363\""), "{}", &book.chapters[3].html[..2000.min(book.chapters[3].html.len())]);
    for c in &book.chapters {
        assert!(ink_formats::xml::is_well_formed(&c.html), "{}", c.path);
    }
    assert_resources_resolve(&book);
    assert!(render_first_page(book) > 100);
}

#[test]
fn calibre_mobi6_azw3_and_joint() {
    let want = vec!["Chapter 1", "  Section 1.1", "Chapter 2", "  Section 2.1", "Chapter 3", "  Section 3.1"];
    for name in ["sample-calibre.azw3", "sample-calibre-joint.mobi"] {
        let book = ink_formats::load(&fx(name)).unwrap();
        assert!(book.info.version.contains("KF8"), "{name}: {}", book.info.version);
        assert_eq!(toc_titles(&book), want, "{name}");
        assert_resources_resolve(&book);
        assert!(book.chapters.iter().any(|c| c.html.contains("<img")), "{name}: images");
        render_first_page(book);
    }
    let old = ink_formats::load(&fx("sample-calibre-old.mobi")).unwrap();
    assert_eq!(old.info.version, "MOBI 6 EBOK");
    assert_eq!(toc_len(&old.toc), 6);
    assert_resources_resolve(&old);
    assert!(old.chapters.iter().any(|c| c.html.contains(" src=\"res/00001.jpg\"")));
    render_first_page(old);
}

#[test]
fn kf8_embedded_fonts() {
    let p = fx("fonts-calibre.azw3");
    let info = ink_formats::read_info(&p).unwrap();
    assert!(!info.fonts.is_empty(), "FONT records listed");
    let book = ink_formats::load(&p).unwrap();
    let subset = std::fs::read(fx("font-subset.ttf")).unwrap();
    // calibre deobfuscated the IDPF and Adobe fonts of epub3-nav.epub with its own code
    // before writing the FONT records: each record must decode to the original font.
    for f in &info.fonts {
        let bytes = book.resources.read(f).unwrap();
        assert_eq!(bytes, subset, "{f}");
    }
    let css: String = (1..4).filter_map(|i| book.resources.read(&format!("flows/flow{i:04}.css")).ok()).map(|b| String::from_utf8_lossy(&b).into_owned()).collect();
    assert!(css.contains("url(../res/"), "font URLs rewritten: {css}");
}

#[test]
fn huffcdic_mobi() {
    let p = fx("huffcdic.mobi");
    let book = ink_formats::load(&p).unwrap();
    assert_eq!(book.info.metadata.title, "HUFF/CDIC Sample");
    assert_eq!(book.chapters.len(), 2, "split at mbp:pagebreak");
    let expected = std::fs::read_to_string(fx("huffcdic.expected.html")).unwrap();
    let words = |s: &str| {
        let mut out = String::new();
        let mut tag = false;
        for c in s.chars() {
            match c {
                '<' => {
                    tag = true;
                    out.push(' ');
                }
                '>' => tag = false,
                c if !tag => out.push(c),
                _ => {}
            }
        }
        out.split_whitespace().map(str::to_string).collect::<Vec<_>>()
    };
    let got: Vec<String> = book.chapters.iter().flat_map(|c| words(&c.html[c.html.find("<body").unwrap()..])).collect();
    assert_eq!(got, words(&expected));
}

// ---------------------------------------------------------------- KFX / DRM (check 6)

#[test]
fn drm_mobi_kfx_topaz_listed_and_refused() {
    let drm = ink_formats::read_info(&fx("drm-sample.azw")).unwrap();
    assert_eq!(drm.metadata.title, "EPUB 2 NCX Sample");
    assert_eq!(drm.drm.as_deref(), Some("Kindle/Mobipocket"));
    assert!(drm.cover.is_some());
    assert!(refused(ink_formats::load(&fx("drm-sample.azw"))).contains("DRM"));

    let kfx = ink_formats::read_info(&fx("kfx-container.kfx")).unwrap();
    assert_eq!(kfx.format, Format::Kfx);
    assert_eq!(kfx.metadata.title, "KFX Sample Title");
    assert_eq!(kfx.metadata.authors, vec!["Inkreader Fixtures"]);
    assert_eq!(kfx.identifiers[0].value, "B0INKTEST0");
    assert_eq!(kfx.drm.as_deref(), Some("Amazon KFX"));
    assert!(refused(ink_formats::load(&fx("kfx-container.kfx"))).contains("KFX"));

    let drmion = ink_formats::read_info(&fx("kfx-drmion.kfx")).unwrap();
    assert_eq!(drmion.metadata.title, "kfx-drmion");
    assert!(refused(ink_formats::load(&fx("kfx-drmion.kfx"))).contains("DRM"));

    let topaz = ink_formats::read_info(&fx("topaz.azw1")).unwrap();
    assert_eq!(topaz.format, Format::Topaz);
    assert!(refused(ink_formats::load(&fx("topaz.azw1"))).contains("Topaz"));

    // Text that starts like a KFX or Topaz signature stays text.
    let dir = std::env::temp_dir().join(format!("ink-formats-sig-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, body) in [("contents.txt", "CONTENTS\n\nCHAPTER I.\n\nText.\n"), ("tpz.txt", "TPZ is a format.\n")] {
        let f = dir.join(name);
        std::fs::write(&f, body).unwrap();
        assert_eq!(ink_formats::format_of(&f), Some(Format::Txt), "{name}");
        assert!(ink_formats::load(&f).is_ok(), "{name}");
    }
    std::fs::remove_dir_all(&dir).unwrap();

    // Content beats the extension: a KFX file named .azw is still KFX.
    let dir = std::env::temp_dir().join(format!("ink-formats-kfx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let renamed = dir.join("book.azw");
    std::fs::copy(fx("kfx-container.kfx"), &renamed).unwrap();
    assert_eq!(ink_formats::format_of(&renamed), Some(Format::Kfx));
    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------- Library API (check 7)

#[test]
fn read_info_for_every_fixture() {
    let mut files: Vec<PathBuf> = std::fs::read_dir(testdata().join("formats"))
        .unwrap()
        .chain(std::fs::read_dir(testdata().join("formats/txt")).unwrap())
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| ink_formats::detect(p).is_some())
        .collect();
    files.push(testdata().join("juliet.epub"));
    files.push(testdata().join("synthetic.epub"));
    files.sort();
    let mut seen = std::collections::HashSet::new();
    for f in &files {
        let info = ink_formats::read_info(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        assert!(!info.metadata.title.is_empty(), "{}", f.display());
        seen.insert(info.format);
        if let Some(c) = &info.cover {
            assert!(c.media_type.starts_with("image/"), "{}", f.display());
            assert!(!c.bytes.is_empty());
        }
        // Readable books open; refused books say why.
        match ink_formats::load(f) {
            Ok(_) => assert!(info.readable(), "{} opened but read_info refuses it", f.display()),
            Err(DocError::Unsupported(m)) => assert_eq!(info.refusal.as_deref(), Some(m.as_str()), "{}", f.display()),
            Err(e) => panic!("{}: {e}", f.display()),
        }
    }
    for f in [Format::Epub, Format::Fb2, Format::Fb3, Format::Txt, Format::Html, Format::Mobi, Format::Kfx, Format::Topaz] {
        assert!(seen.contains(&f), "no fixture for {f:?}");
    }
}

#[test]
fn provider_opens_every_readable_format() {
    use ink_doc::Provider;
    let p = ink_formats::FormatsProvider;
    for name in ["sample.fb2", "sample.fb2.zip", "sample.fb3", "alice-kf8.azw3", "alice-old.mobi", "sample.html", "txt/en-utf8.txt", "epub3-nav.epub"] {
        let path = fx(name);
        assert!(p.can_open(&path), "{name}");
        let mut d = p.open(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        d.set_layout(&LayoutSettings::for_screen(1272, 1696)).unwrap();
        assert!(d.wait_paginated(Duration::from_secs(120)), "{name}: pagination did not finish");
        assert!(d.page_count().pages >= 1);
        assert!(!d.toc().is_empty(), "{name}: TOC");
        assert!(!d.metadata().title.is_empty());
    }
}

// ---------------------------------------------------------------- Damaged files

/// Truncated and corrupted copies of every fixture must give an error or a book, never a panic.
/// The daily run uses 5 damaged copies per fixture; the full set of 10 is ignored.
#[test]
fn damaged_files_do_not_panic() {
    damaged_files(&[3, 50, 99], &[1009]);
}

#[test]
#[ignore = "slow: run with --ignored before a release"]
fn damaged_files_do_not_panic_full() {
    damaged_files(&[1, 3, 10, 50, 90, 99], &[97, 1009, 7919]);
}

fn damaged_files(cuts: &[usize], flips: &[usize]) {
    let dir = std::env::temp_dir().join(format!("ink-formats-damaged-{}-{}", std::process::id(), cuts.len()));
    std::fs::create_dir_all(&dir).unwrap();
    let names = [
        "alice-kf8.azw3", "alice-old.mobi", "sample-calibre-joint.mobi", "fonts-calibre.azw3", "huffcdic.mobi", "sample.fb2",
        "sample.fb2.zip", "sample.fb3", "epub3-nav.epub", "epub2-ncx.epub", "sample.html", "txt/ru-cp1251.txt",
        "kfx-container.kfx",
    ];
    let mut cases = 0;
    for name in names {
        let bytes = std::fs::read(fx(name)).unwrap();
        let file = dir.join(name.replace('/', "_"));
        let mut variants: Vec<Vec<u8>> = cuts.iter().map(|pct| bytes[..bytes.len() * pct / 100].to_vec()).collect();
        for &step in flips {
            let mut v = bytes.clone();
            for i in (step / 2..v.len()).step_by(step) {
                v[i] ^= 0xA5;
            }
            variants.push(v);
        }
        // Headers only, rest zeroed.
        let mut z = bytes.clone();
        for b in z.iter_mut().skip(256) {
            *b = 0;
        }
        variants.push(z);
        for v in variants {
            std::fs::write(&file, &v).unwrap();
            let _ = ink_formats::read_info(&file);
            if let Ok(book) = ink_formats::load(&file) {
                let _ = book.positions();
                // Layout is slow in debug builds; lay out the small books only.
                if v.len() < 64 * 1024 {
                    if let Ok(mut d) = book.into_document() {
                        let _ = d.set_layout(&LayoutSettings::for_screen(600, 800));
                    }
                }
            }
            cases += 1;
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
    assert_eq!(cases, names.len() * (cuts.len() + flips.len() + 1));
}
