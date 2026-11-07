//! Book formats that feed the reflowable engine (`ink-html`).
//!
//! | Format | Reader |
//! | --- | --- |
//! | EPUB 2/3 | [`epub`]: container, OPF metadata, spine, manifest, NCX and nav TOC, page list, cover, embedded fonts with IDPF/Adobe deobfuscation; DRM refused |
//! | FB2, FB2.zip | [`fb2`]: sections, titles, epigraphs, poems, notes, binary images |
//! | FB3 | [`fb3`]: OPC package, description, body, notes, images |
//! | TXT | [`txt`]: encoding detection, paragraphs, chapter guessing |
//! | HTML/XHTML file | [`html`]: charset detection, broken markup repaired, heading TOC |
//! | MOBI, AZW3, AZW, PRC | [`mobi`]: PalmDB, MOBI/EXTH, PalmDOC and HUFF/CDIC, KF8 skeleton/fragments/FDST, old MOBI markup, images, filepos links, NCX TOC; DRM refused |
//! | KFX, Topaz | [`kfx`]: detected, metadata where readable, refused |
//!
//! Entry points:
//! - [`open_book`]: a [`BookDocument`] with the book's own TOC and page list (what
//!   [`FormatsProvider`] returns to the app).
//! - [`open`]: the plain engine document ([`HtmlDocument`], kept for golden tests).
//! - [`load`]: the converted [`Book`] (chapters, TOC, page list, resources, info).
//! - [`read_info`]: metadata and cover for the library, without laying anything out.
//!
//! Owner from wave A: the `formats` agent (also owns `testdata/formats`).

#![warn(missing_docs)]

pub mod book;
pub mod encoding;
pub mod epub;
pub mod fb2;
pub mod fb3;
pub mod html;
pub mod kfx;
pub mod mobi;
pub mod txt;
pub mod util;
pub mod xhtml;
pub mod xml;

pub use book::{Book, BookDocument, BookInfo, Cover, Identifier, PageTarget, Target, TocNode};

use ink_doc::{DocError, Document, Provider, Result};
use ink_html::HtmlDocument;
use std::path::Path;

/// Formats this crate recognises.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Format {
    /// EPUB.
    Epub,
    /// HTML or XHTML file.
    Html,
    /// Plain text.
    Txt,
    /// FictionBook 2 (also zipped).
    Fb2,
    /// FictionBook 3 (zip package).
    Fb3,
    /// Mobipocket / Kindle KF8 (MOBI, AZW, AZW3, PRC).
    Mobi,
    /// Amazon KFX (never readable).
    Kfx,
    /// Amazon Topaz (never readable).
    Topaz,
}

impl Format {
    /// Short display name.
    pub fn name(self) -> &'static str {
        match self {
            Format::Epub => "EPUB",
            Format::Html => "HTML",
            Format::Txt => "TXT",
            Format::Fb2 => "FB2",
            Format::Fb3 => "FB3",
            Format::Mobi => "MOBI",
            Format::Kfx => "KFX",
            Format::Topaz => "Topaz",
        }
    }
}

/// File extensions (lowercase, without the dot) and their formats, in the order the
/// provider lists them.
const EXTENSIONS: [(&str, Format); 20] = [
    ("epub", Format::Epub),
    ("kepub", Format::Epub),
    ("html", Format::Html),
    ("htm", Format::Html),
    ("xhtml", Format::Html),
    ("txt", Format::Txt),
    ("fb2", Format::Fb2),
    ("fb2.zip", Format::Fb2),
    ("fbz", Format::Fb2),
    ("fb3", Format::Fb3),
    ("mobi", Format::Mobi),
    ("azw3", Format::Mobi),
    ("azw", Format::Mobi),
    ("prc", Format::Mobi),
    ("pdb", Format::Mobi),
    ("kfx", Format::Kfx),
    ("azw8", Format::Kfx),
    ("kfx-zip", Format::Kfx),
    ("tpz", Format::Topaz),
    ("azw1", Format::Topaz),
];

/// The extensions of [`EXTENSIONS`] alone ([`Provider::extensions`] wants a static slice).
const EXTENSION_NAMES: [&str; EXTENSIONS.len()] = {
    let mut out = [""; EXTENSIONS.len()];
    let mut i = 0;
    while i < out.len() {
        out[i] = EXTENSIONS[i].0;
        i += 1;
    }
    out
};

/// Detects the format by file name.
pub fn detect(path: &Path) -> Option<Format> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    EXTENSIONS
        .iter()
        .find(|(ext, _)| name.strip_suffix(ext).is_some_and(|stem| stem.ends_with('.')))
        .map(|&(_, f)| f)
}

/// Detects the format from the first bytes of the file (and zip entry names). `None`
/// when the content says nothing (plain text, HTML) or the file cannot be read.
pub fn sniff(path: &Path) -> Option<Format> {
    let head = util::read_head(path, 80).ok()?;
    let head = head.as_slice();
    // KFX container: "CONT", little-endian version 1 or 2, header length >= 18. A text
    // file starting with "CONTENTS" must not match.
    let cont = head.len() >= 10
        && head.starts_with(b"CONT")
        && matches!(u16::from_le_bytes([head[4], head[5]]), 1 | 2)
        && u32::from_le_bytes([head[6], head[7], head[8], head[9]]) >= 18;
    if cont || head.starts_with(b"\xeaDRMION\xee") {
        return Some(Format::Kfx);
    }
    // Topaz: "TPZ" and a version digit.
    if head.len() >= 4 && head.starts_with(b"TPZ") && head[3].is_ascii_digit() {
        return Some(Format::Topaz);
    }
    if head.len() >= 68 && (&head[60..68] == b"BOOKMOBI" || &head[60..68] == b"TEXtREAd") {
        return Some(Format::Mobi);
    }
    if head.starts_with(b"PK\x03\x04") {
        let f = std::fs::File::open(path).ok()?;
        let zip = zip::ZipArchive::new(f).ok()?;
        let names: Vec<String> = zip.file_names().filter_map(|n| n.ok()).map(|n| n.to_ascii_lowercase()).collect();
        if names.iter().any(|n| n == "meta-inf/container.xml" || n == "mimetype") {
            return Some(Format::Epub);
        }
        if names.iter().any(|n| n.ends_with(".fb2")) {
            return Some(Format::Fb2);
        }
        if names.iter().any(|n| n.starts_with("fb3/") || n.ends_with("fb3/description.xml")) {
            return Some(Format::Fb3);
        }
        if names.iter().any(|n| n.ends_with(".kfx") || n.ends_with(".azw") || n.contains("assets/")) {
            return Some(Format::Kfx);
        }
        return None;
    }
    let text = String::from_utf8_lossy(head);
    if text.contains("<FictionBook") || (text.starts_with("<?xml") && path.to_string_lossy().to_lowercase().ends_with(".fb2")) {
        return Some(Format::Fb2);
    }
    None
}

/// The format of `path`: content first, then file name.
pub fn format_of(path: &Path) -> Option<Format> {
    match (sniff(path), detect(path)) {
        // A zip named .fb2.zip is FB2 even if it also looks like something else.
        (Some(s), Some(d)) if s == d => Some(s),
        (Some(s), _) => Some(s),
        (None, d) => d,
    }
}

pub(crate) fn title_from(path: &Path) -> String {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let lower = name.to_ascii_lowercase();
    for ext in [".fb2.zip", ".kfx-zip"] {
        if lower.ends_with(ext) {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string()
}

/// Opens an HTML/XHTML file as a one-chapter book (markup repaired, see [`html`]).
pub fn open_html(path: &Path) -> Result<HtmlDocument> {
    html::load(path)?.into_html_document()
}

/// Converts plain text to XHTML: blank lines separate paragraphs (the phase 0 helper;
/// [`txt`] does detection and chapters).
pub fn txt_to_xhtml(text: &str) -> String {
    let mut out = String::from("<html><body>");
    for para in text.replace("\r\n", "\n").split("\n\n") {
        let p = para.trim();
        if !p.is_empty() {
            out.push_str("<p>");
            xml::push_text(&mut out, p);
            out.push_str("</p>");
        }
    }
    out.push_str("</body></html>");
    out
}

/// Opens a text file with encoding detection and chapter guessing.
pub fn open_txt(path: &Path) -> Result<HtmlDocument> {
    txt::open(path)?.into_html_document()
}

fn unknown(path: &Path) -> DocError {
    DocError::Unsupported(format!("unknown format: {}", path.display()))
}

/// Converts any supported format to a [`Book`]. DRM-protected, KFX and Topaz books are
/// refused with [`DocError::Unsupported`] and a message for the user.
pub fn load(path: &Path) -> Result<Book> {
    match format_of(path) {
        Some(Format::Epub) => epub::load(path),
        Some(Format::Txt) => txt::open(path),
        Some(Format::Html) => html::load(path),
        Some(Format::Fb2) => fb2::load(path),
        Some(Format::Mobi) => mobi::load(path),
        Some(Format::Fb3) => fb3::load(path),
        Some(f @ (Format::Kfx | Format::Topaz)) => {
            let info = kfx::read_info(path, f)?;
            Err(DocError::Unsupported(info.refusal.unwrap_or_else(|| kfx::KFX_MESSAGE.into())))
        }
        None => Err(unknown(path)),
    }
}

/// Opens any supported format as the plain engine document.
pub fn open(path: &Path) -> Result<HtmlDocument> {
    match format_of(path) {
        // EPUB: the engine also gets the book's NCX / nav (crengine-exact TOC and page
        // list resolved by ink-html; api-request html.md).
        Some(Format::Epub) => epub::open(path),
        _ => load(path)?.into_html_document(),
    }
}

/// Opens any supported format with the book's own TOC and page list.
pub fn open_book(path: &Path) -> Result<BookDocument> {
    load(path)?.into_document()
}

/// Metadata, identifiers, cover and DRM status of a book file, for the library. Works
/// for DRM-protected and KFX books too (as far as their metadata is readable); check
/// [`BookInfo::refusal`] before opening.
pub fn read_info(path: &Path) -> Result<BookInfo> {
    match format_of(path) {
        Some(Format::Epub) => epub::read_info(path),
        Some(Format::Txt) => txt::read_info(path),
        Some(Format::Html) => html::read_info(path),
        Some(Format::Fb2) => fb2::read_info(path),
        Some(Format::Mobi) => mobi::read_info(path),
        Some(Format::Fb3) => fb3::read_info(path),
        Some(f @ (Format::Kfx | Format::Topaz)) => kfx::read_info(path, f),
        None => Err(unknown(path)),
    }
}

/// The provider registered by the app.
#[derive(Clone, Copy, Debug, Default)]
pub struct FormatsProvider;

impl Provider for FormatsProvider {
    fn name(&self) -> &'static str {
        "formats"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &EXTENSION_NAMES
    }

    fn open(&self, path: &Path) -> Result<Box<dyn Document>> {
        Ok(Box::new(open_book(path)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_doc::LayoutSettings;

    /// `INK_FORMATS_TESTDATA` or the repository's `testdata` directory.
    pub(crate) fn testdata(name: &str) -> std::path::PathBuf {
        match std::env::var_os("INK_FORMATS_TESTDATA") {
            Some(d) => std::path::PathBuf::from(d).join(name),
            None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(name),
        }
    }

    #[test]
    fn detection() {
        assert_eq!(detect(Path::new("a/B.EPUB")), Some(Format::Epub));
        assert_eq!(detect(Path::new("x.fb2.zip")), Some(Format::Fb2));
        assert_eq!(detect(Path::new("x.fb3")), Some(Format::Fb3));
        assert_eq!(detect(Path::new("x.azw3")), Some(Format::Mobi));
        assert_eq!(detect(Path::new("x.kfx")), Some(Format::Kfx));
        assert_eq!(detect(Path::new("x.azw1")), Some(Format::Topaz));
        assert_eq!(detect(Path::new("x.zip")), None);
        assert_eq!(sniff(&testdata("juliet.epub")), Some(Format::Epub));
        assert_eq!(title_from(Path::new("/a/Book.fb2.zip")), "Book");
    }

    #[test]
    fn juliet_opens_like_the_prototype() {
        let mut d = epub::open(&testdata("juliet.epub")).unwrap();
        assert_eq!(d.metadata().title, "Romeo and Juliet");
        assert_eq!(d.chapter_count(), 29);
        d.set_layout(&LayoutSettings::for_screen(1272, 1696)).unwrap();
        // The prototype printed 208 pages; the phase 2 engine (CSS, images) differs and
        // paginates in the background.
        assert!(d.wait_paginated(std::time::Duration::from_secs(120)));
        assert!(d.page_count().pages > 150, "{:?}", d.page_count());
        let mut b = open_book(&testdata("juliet.epub")).unwrap();
        b.set_layout(&LayoutSettings::for_screen(1272, 1696)).unwrap();
        assert!(b.wait_paginated(std::time::Duration::from_secs(120)));
        assert_eq!(b.page_count().pages, d.page_count().pages, "open_book and open paginate alike");
    }

    #[test]
    fn unknown_and_missing_files_say_why() {
        let err = open(Path::new("a.doc")).unwrap_err().to_string();
        assert!(err.contains("unknown"), "{err}");
        assert!(matches!(open(Path::new("/nonexistent/a.fb2")), Err(DocError::Io(_)) | Err(DocError::Unsupported(_))));
        assert!(FormatsProvider.can_open(Path::new("x.FB2.zip")));
    }

    #[test]
    fn txt_and_html_files() {
        let dir = std::env::temp_dir().join(format!("ink-formats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let t = dir.join("note.txt");
        std::fs::write(&t, "First <para>.\r\n\r\nSecond & last.\n").unwrap();
        let mut d = open_txt(&t).unwrap();
        assert_eq!(d.metadata().title, "note");
        let start = ink_doc::Position::Flow(ink_doc::FlowAnchor::default());
        assert!(d.read_around(&start, 100).unwrap().contains("Second & last."));
        let h = dir.join("page.html");
        std::fs::write(&h, "<html><body><h1>Hi</h1><p>There</p></body></html>").unwrap();
        let d = FormatsProvider.open(&h).unwrap();
        assert_eq!(d.toc().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
        assert!(txt_to_xhtml("a<b").contains("a&lt;b"));
    }
}
