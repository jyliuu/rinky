//! FictionBook 2 (`.fb2`, `.fb2.zip`, `.fbz`) to XHTML chapters.
//!
//! - Encoding from the BOM or the XML declaration (`windows-1251` is common).
//! - Chapters: the main body's header (image, title, epigraphs) is chapter 1 when it has
//!   content, then one chapter per top-level `section` (a lone top-level section is split
//!   at its subsections), then one chapter per extra body (`notes`, `comments`).
//! - Elements keep their structure with classes: `section` -> `div.section` (every
//!   section gets an id), `title` -> `h1`-`h6`, `epigraph`/`cite` -> `blockquote`,
//!   `poem`/`stanza`/`v`, `subtitle`, `text-author`, `empty-line`, tables, inline
//!   `strong`/`emphasis`/`strikethrough`/`sub`/`sup`/`code`/`style`.
//! - Links: `#id` targets are rewritten to the chapter file that holds the id; note
//!   links (`type="note"`) get `epub:type="noteref"` and note sections
//!   `epub:type="footnote"`, so the reader can show popups.
//! - Images: `binary` elements (base64) are served by the resource loader as
//!   `images/<id>`; `image` elements become `img`.
//! - Metadata: `title-info` (title, authors, genres, language, sequence, annotation,
//!   date, cover page), `document-info/id`, `publish-info` (publisher, ISBN, year).

use crate::book::{Book, BookInfo, Cover, Identifier, Target, TocNode};
use crate::util::{base64_decode, find_bytes, read_limited, sniff_media_type, MAX_ENTRY};
use crate::xml::{self, escape_text, Element, Node};
use crate::Format;
use ink_doc::{DocError, MemoryLoader, Result};
use ink_html::HtmlChapter;
use std::collections::HashMap;
use std::path::Path;

/// Stylesheet linked from every converted chapter (FB2 defaults, close to crengine's).
pub const FB2_CSS: &str = "\
body { margin: 0; }
h1, h2, h3, h4, h5, h6 { text-align: center; page-break-after: avoid; }
h1 { page-break-before: always; }
.subtitle { text-align: center; font-weight: bold; }
.epigraph { margin-left: 25%; font-style: italic; }
.text-author { text-align: right; font-weight: bold; font-style: italic; }
.poem { margin-left: 15%; }
.stanza { margin-bottom: 1em; }
.v { text-indent: 0; margin: 0; }
.cite { margin-left: 10%; margin-right: 10%; }
.empty-line { height: 1em; }
.image { text-align: center; text-indent: 0; }
.annotation { font-style: italic; }
a.note { vertical-align: super; font-size: 75%; }
";

/// Reads the FB2 XML bytes of a `.fb2` or zipped FB2 file.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    if bytes.starts_with(b"PK\x03\x04") {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| DocError::Format(format!("FB2 zip: {e}")))?;
        let name = zip
            .file_names()
            .filter_map(|n| n.ok())
            .find(|n| n.to_ascii_lowercase().ends_with(".fb2"))
            .map(|n| n.into_owned())
            .ok_or_else(|| DocError::Format("no .fb2 file in the zip".into()))?;
        let entry = zip.by_name(&name).map_err(|e| DocError::Format(format!("FB2 zip: {e}")))?;
        let size = entry.size();
        return read_limited(entry, size, MAX_ENTRY).map_err(|e| DocError::Format(format!("FB2 zip: {e}")));
    }
    Ok(bytes)
}

/// Parses FB2 bytes into the `FictionBook` element.
pub fn parse(bytes: &[u8]) -> Result<Element> {
    parse_text(&xml::decode_xml_bytes(bytes))
}

fn parse_text(text: &str) -> Result<Element> {
    let root = xml::parse(text).map_err(|e| DocError::Format(format!("FB2: {e}")))?;
    if root.name != "FictionBook" {
        return Err(DocError::Format(format!("FB2: root element is <{}>, not <FictionBook>", root.name)));
    }
    Ok(root)
}

fn person(el: &Element) -> String {
    let part = |n: &str| el.child(n).map(|e| e.text_collapsed()).unwrap_or_default();
    let name = [part("first-name"), part("middle-name"), part("last-name")].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        part("nickname")
    } else {
        name
    }
}

/// Safe resource name for a binary id.
fn image_path(id: &str) -> String {
    let clean: String = id.trim_start_matches('#').chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect();
    format!("images/{clean}")
}

/// Metadata from `description`.
pub fn info_of(root: &Element) -> BookInfo {
    let mut info = BookInfo::new(Format::Fb2);
    info.version = "FB2".into();
    let empty = Element::default();
    let desc = root.child("description").unwrap_or(&empty);
    let ti = desc.child("title-info").unwrap_or(&empty);
    info.metadata.title = ti.child("book-title").map(|t| t.text_collapsed()).unwrap_or_default();
    info.metadata.authors = ti.children_named("author").map(person).filter(|a| !a.is_empty()).collect();
    info.metadata.language = ti.child("lang").map(|l| l.text_collapsed()).filter(|l| !l.is_empty());
    info.metadata.description = ti.child("annotation").map(|a| a.text_collapsed()).filter(|a| !a.is_empty());
    info.subjects = ti.children_named("genre").map(|g| g.text_collapsed()).filter(|g| !g.is_empty()).collect();
    info.date = ti.child("date").and_then(|d| d.attr("value").map(str::to_string).or_else(|| Some(d.text_collapsed()))).filter(|d| !d.is_empty());
    if let Some(seq) = ti.child("sequence").and_then(|s| Some((s.attr("name")?.trim().to_string(), s.attr("number")))) {
        if !seq.0.is_empty() {
            let n = seq.1.and_then(|n| n.trim().parse::<f32>().ok()).unwrap_or(1.0);
            info.metadata.series = Some((seq.0, n));
        }
    }
    if let Some(img) = ti.child("coverpage").and_then(|c| c.child("image")).and_then(|i| i.attr("href")) {
        info.metadata.cover = Some(image_path(img));
    }
    if let Some(di) = desc.child("document-info") {
        if let Some(id) = di.child("id").map(|i| i.text_collapsed()).filter(|i| !i.is_empty()) {
            info.identifiers.push(Identifier { scheme: Some("FB2".into()), value: id });
        }
    }
    if let Some(pi) = desc.child("publish-info") {
        info.metadata.publisher = pi.child("publisher").map(|p| p.text_collapsed()).filter(|p| !p.is_empty());
        if let Some(isbn) = pi.child("isbn").map(|p| p.text_collapsed()).filter(|p| !p.is_empty()) {
            info.identifiers.push(Identifier { scheme: Some("ISBN".into()), value: isbn });
        }
        if info.date.is_none() {
            info.date = pi.child("year").map(|y| y.text_collapsed()).filter(|y| !y.is_empty());
        }
        if info.metadata.series.is_none() {
            if let Some(s) = pi.child("sequence").and_then(|s| s.attr("name")).filter(|s| !s.trim().is_empty()) {
                let n = pi.child("sequence").and_then(|s| s.attr("number")).and_then(|n| n.trim().parse().ok()).unwrap_or(1.0);
                info.metadata.series = Some((s.trim().to_string(), n));
            }
        }
    }
    info.metadata.identifier = info.identifiers.first().map(|i| i.value.clone());
    info
}

/// The `binary` elements: path -> (media type, bytes).
pub fn binaries(root: &Element) -> HashMap<String, (String, Vec<u8>)> {
    root.children_named("binary").filter_map(|b| Some((image_path(b.attr("id")?), binary(b)))).collect()
}

/// Media type and bytes of a `binary` element.
fn binary(b: &Element) -> (String, Vec<u8>) {
    let bytes = base64_decode(b.text().as_bytes());
    let mt = sniff_media_type(&bytes).map(str::to_string).or_else(|| b.attr("content-type").map(str::to_string)).unwrap_or_default();
    (mt, bytes)
}

/// The last `binary` element of `text` whose id gives `path`, parsed alone (the rest
/// of the book is not built into a tree).
fn find_binary(text: &str, path: &str) -> Option<(String, Vec<u8>)> {
    let b = text.as_bytes();
    let mut found = None;
    let mut at = 0;
    while let Some(start) = find_bytes(b, b"<binary", at) {
        let Some(gt) = xml::tag_end(b, start) else { break };
        at = gt;
        let open = text[start..gt].trim_end_matches('/');
        let Ok(tag) = xml::parse(&format!("{open}/>")) else { continue };
        if tag.name != "binary" || tag.attr("id").map(image_path).as_deref() != Some(path) {
            continue;
        }
        let end = if b[gt - 1] == b'/' { gt + 1 } else { find_bytes(b, b"</binary>", gt).map_or(text.len(), |e| e + "</binary>".len()) };
        if let Ok(el) = xml::parse(&text[start..end]) {
            found = Some(binary(&el));
        }
    }
    found
}

/// One chapter of the conversion: what it is made of.
enum Unit<'a> {
    /// Body header (image, title, epigraphs, annotation): elements before the first section.
    Header(&'a Element, Vec<&'a Element>, u8),
    /// A section rendered at heading depth `depth`.
    Section(&'a Element, u8),
    /// An extra body (notes).
    Body(&'a Element),
}

fn is_main_body(b: &Element) -> bool {
    !matches!(b.attr("name"), Some("notes") | Some("comments") | Some("footnotes"))
}

/// Splits the main body into units: header, then sections (a lone section is opened).
fn units_of<'a>(body: &'a Element) -> Vec<Unit<'a>> {
    let mut out = Vec::new();
    let mut parent = body;
    // Heading level of the sections listed in `parent`.
    let mut level = 1u8;
    let mut is_body = true;
    loop {
        let header: Vec<&Element> = parent.elements().take_while(|e| e.name != "section").collect();
        let sections: Vec<&Element> = parent.children_named("section").collect();
        let title_level = if is_body { 1 } else { level - 1 };
        if sections.is_empty() {
            // No sections: the whole element is one chapter.
            if is_body {
                out.push(Unit::Header(parent, parent.elements().collect(), 1));
            } else {
                out.push(Unit::Section(parent, title_level));
            }
            return out;
        }
        let header_has_content = header.iter().any(|e| e.name != "title" || !e.text_collapsed().is_empty());
        if !header.is_empty() && header_has_content {
            out.push(Unit::Header(parent, header, title_level));
        }
        if sections.len() == 1 && sections[0].children_named("section").count() >= 2 && level < 6 {
            parent = sections[0];
            level += 1;
            is_body = false;
            continue;
        }
        out.extend(sections.into_iter().map(|s| Unit::Section(s, level)));
        return out;
    }
}

struct Conv<'a> {
    /// Element id -> chapter file.
    chapters: &'a Chapters,
    images: &'a HashMap<String, (String, Vec<u8>)>,
    out: String,
    /// Note bodies render their sections as footnotes.
    in_notes: bool,
    /// Inside a paragraph-like element (images are inline there).
    inline: bool,
}

/// FB2 elements written as one XHTML element: (FB2 name, tag, class, inline content).
/// Block content keeps child elements only (white space between blocks is dropped).
const ELEMENTS: &[(&str, &str, Option<&str>, bool)] = &[
    ("p", "p", None, true),
    ("v", "p", Some("v"), true),
    ("subtitle", "p", Some("subtitle"), true),
    ("text-author", "p", Some("text-author"), true),
    ("date", "p", Some("date"), true),
    ("epigraph", "blockquote", Some("epigraph"), false),
    ("cite", "blockquote", Some("cite"), false),
    ("poem", "div", Some("poem"), false),
    ("stanza", "div", Some("stanza"), false),
    ("annotation", "div", Some("annotation"), false),
    ("table", "table", None, false),
    ("tr", "tr", None, false),
    ("strong", "strong", None, true),
    ("emphasis", "em", None, true),
    ("strikethrough", "s", None, true),
    ("sub", "sub", None, true),
    ("sup", "sup", None, true),
    ("code", "code", None, true),
];

/// Table cell attributes kept (FB2 and FB3).
pub(crate) const CELL_ATTRS: [&str; 4] = ["colspan", "rowspan", "align", "valign"];

impl Conv<'_> {
    /// Writes `<tag`, the class, the attributes `copy` of `el` and its id; the caller
    /// adds more attributes or the closing `>`.
    fn open(&mut self, tag: &str, class: Option<&str>, copy: &[&str], el: &Element) {
        self.out.push('<');
        self.out.push_str(tag);
        if let Some(c) = class {
            xml::push_attr(&mut self.out, "class", c);
        }
        for a in copy {
            if let Some(v) = el.attr(a) {
                xml::push_attr(&mut self.out, a, v);
            }
        }
        self.id_attr(el);
    }

    fn id_attr(&mut self, el: &Element) {
        if let Some(id) = el.attr("id") {
            xml::push_attr(&mut self.out, "id", id);
        }
    }

    fn close(&mut self, tag: &str) {
        self.out.push_str("</");
        self.out.push_str(tag);
        self.out.push('>');
    }

    fn children(&mut self, el: &Element, depth: u8) {
        for c in &el.children {
            match c {
                Node::Text(t) => xml::push_text(&mut self.out, t),
                Node::Element(e) => self.element(e, depth),
            }
        }
    }

    /// Text and inline elements, with images inline.
    fn inline_children(&mut self, el: &Element, depth: u8) {
        let was = std::mem::replace(&mut self.inline, true);
        self.children(el, depth);
        self.inline = was;
    }

    fn wrap(&mut self, tag: &str, class: Option<&str>, copy: &[&str], el: &Element, depth: u8, inline: bool) {
        self.open(tag, class, copy, el);
        self.out.push('>');
        if inline {
            self.inline_children(el, depth);
        } else {
            for e in el.elements() {
                self.element(e, depth);
            }
        }
        self.close(tag);
    }

    fn title(&mut self, el: &Element, depth: u8, section_title: bool) {
        if !section_title {
            return self.wrap("div", Some("title"), &[], el, depth, false);
        }
        let tag = format!("h{}", depth.clamp(1, 6));
        self.open(&tag, Some("title"), &[], el);
        self.out.push('>');
        let was = std::mem::replace(&mut self.inline, true);
        for (i, p) in el.children_named("p").enumerate() {
            if i > 0 {
                self.out.push_str("<br/>");
            }
            self.children(p, depth);
        }
        self.inline = was;
        self.close(&tag);
    }

    fn image(&mut self, el: &Element, block: bool) {
        let Some(href) = el.attr("href") else { return };
        if !href.starts_with('#') {
            return;
        }
        let path = image_path(href);
        if !self.images.contains_key(&path) {
            return;
        }
        if block {
            self.open("div", Some("image"), &[], el);
            self.out.push('>');
        }
        self.out.push_str("<img");
        xml::push_attr(&mut self.out, "src", &path);
        xml::push_attr(&mut self.out, "alt", el.attr("alt").or_else(|| el.attr("title")).unwrap_or(""));
        self.out.push_str("/>");
        if block {
            self.close("div");
        }
    }

    fn link(&mut self, el: &Element, depth: u8) {
        self.out.push_str("<a");
        xml::push_attr(&mut self.out, "href", &self.chapters.href(el.attr("href").unwrap_or("")));
        if el.attr("type") == Some("note") {
            self.out.push_str(" class=\"note\" epub:type=\"noteref\"");
        }
        self.id_attr(el);
        self.out.push('>');
        self.inline_children(el, depth);
        self.close("a");
    }

    fn element(&mut self, el: &Element, depth: u8) {
        match el.name.as_str() {
            "section" => {
                self.open("div", Some("section"), &[], el);
                if self.in_notes && el.attr("id").is_some() {
                    self.out.push_str(" epub:type=\"footnote\"");
                }
                self.out.push('>');
                for c in el.elements() {
                    match c.name.as_str() {
                        "title" => self.title(c, depth, true),
                        "section" => self.element(c, depth.saturating_add(1)),
                        _ => self.element(c, depth),
                    }
                }
                self.close("div");
            }
            "title" => self.title(el, depth, false),
            "empty-line" => self.out.push_str("<p class=\"empty-line\">\u{a0}</p>"),
            "image" => self.image(el, !self.inline),
            "th" | "td" => self.wrap(&el.name, None, &CELL_ATTRS, el, depth, true),
            "style" => {
                let class = el.attr("name").map(|n| n.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_"));
                self.wrap("span", class.as_deref(), &[], el, depth, true)
            }
            "a" => self.link(el, depth),
            name => match ELEMENTS.iter().find(|e| e.0 == name) {
                Some(&(_, tag, class, inline)) => self.wrap(tag, class, &[], el, depth, inline),
                // Unknown elements: keep their content.
                None => self.children(el, depth),
            },
        }
    }
}

/// Gives every `section` without an id the id `<prefix>-s<N>`, so TOC entries can
/// point at it (FB2 and FB3).
pub(crate) fn assign_section_ids(el: &mut Element, prefix: &str, n: &mut usize) {
    if el.name == "section" && el.attr("id").is_none() {
        *n += 1;
        el.attrs.push(xml::Attr { local: "id".into(), qname: "id".into(), value: format!("{prefix}-s{n}") });
    }
    for c in &mut el.children {
        if let Node::Element(e) = c {
            assign_section_ids(e, prefix, n);
        }
    }
}

/// The chapter files of a converted FictionBook (`<prefix>-001.xhtml`, ...) and the
/// chapter that holds each element id (FB2 and FB3).
pub(crate) struct Chapters {
    prefix: &'static str,
    ids: HashMap<String, usize>,
}

impl Chapters {
    pub(crate) fn new(prefix: &'static str) -> Self {
        Chapters { prefix, ids: HashMap::new() }
    }

    /// File name of chapter `i` (0-based).
    pub(crate) fn path(&self, i: usize) -> String {
        format!("{}-{:03}.xhtml", self.prefix, i + 1)
    }

    /// Records `id` as being in chapter `i` (the first chapter that has it wins).
    pub(crate) fn add_id(&mut self, id: &str, i: usize) {
        if !self.ids.contains_key(id) {
            self.ids.insert(id.to_string(), i);
        }
    }

    /// Records the ids of `el` and of everything below it as being in chapter `i`.
    pub(crate) fn add(&mut self, el: &Element, i: usize) {
        if let Some(id) = el.attr("id") {
            self.add_id(id, i);
        }
        for e in el.elements() {
            self.add(e, i);
        }
    }

    /// `href` with a `#id` reference pointed at the chapter file that holds the id.
    pub(crate) fn href(&self, href: &str) -> String {
        match href.strip_prefix('#').and_then(|id| Some((*self.ids.get(id)?, id))) {
            Some((i, id)) => format!("{}#{id}", self.path(i)),
            None => href.to_string(),
        }
    }

    /// TOC nodes for the sections below `el` whose id is in a chapter; sections without a
    /// title (`title_of`) pass their children up.
    pub(crate) fn section_toc(&self, el: &Element, title_of: fn(&Element) -> Option<String>) -> Vec<TocNode> {
        let mut out = Vec::new();
        for s in el.children_named("section") {
            let children = self.section_toc(s, title_of);
            let id = s.attr("id").unwrap_or("");
            match (title_of(s), self.ids.get(id)) {
                (Some(title), Some(&ch)) => out.push(TocNode { title, target: Some(Target::id(ch, id)), children }),
                _ => out.extend(children),
            }
        }
        out
    }
}

fn section_title(el: &Element) -> Option<String> {
    let t = el.child("title")?;
    let lines: Vec<String> = t.children_named("p").map(|p| p.text_collapsed()).filter(|s| !s.is_empty()).collect();
    let s = if lines.is_empty() { t.text_collapsed() } else { lines.join(" ") };
    (!s.is_empty()).then_some(s)
}

/// A converted chapter page: XHTML with the FB2 stylesheet (FB2 and FB3).
pub(crate) fn chapter_html(title: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\"><head><title>{}</title><link rel=\"stylesheet\" type=\"text/css\" href=\"fb2.css\"/></head><body>{}</body></html>",
        escape_text(title),
        body
    )
}

/// Converts a parsed FB2 document.
pub fn convert(mut root: Element) -> Result<Book> {
    let mut info = info_of(&root);
    let mut n = 0;
    assign_section_ids(&mut root, "fb2", &mut n);
    let images = binaries(&root);
    let bodies: Vec<&Element> = root.children_named("body").collect();
    let main = bodies.iter().copied().find(|b| is_main_body(b)).or(bodies.first().copied()).ok_or_else(|| DocError::Format("FB2 has no <body>".into()))?;
    let mut units = units_of(main);
    for b in &bodies {
        if !std::ptr::eq(*b, main) {
            units.push(Unit::Body(b));
        }
    }
    let mut files = Chapters::new("fb2");
    for (i, u) in units.iter().enumerate() {
        match u {
            Unit::Header(parent, els, _) => {
                if parent.name == "section" {
                    if let Some(id) = parent.attr("id") {
                        files.add_id(id, i);
                    }
                }
                els.iter().for_each(|e| files.add(e, i))
            }
            Unit::Section(e, _) | Unit::Body(e) => files.add(e, i),
        }
    }
    let mut chapters = Vec::with_capacity(units.len());
    for u in &units {
        let mut c = Conv { chapters: &files, images: &images, out: String::new(), in_notes: false, inline: false };
        let title = match u {
            Unit::Header(parent, els, level) => {
                let section = parent.name == "section";
                if section {
                    c.out.push_str("<div class=\"section\"");
                    c.id_attr(parent);
                    c.out.push('>');
                }
                for e in els {
                    if e.name == "title" {
                        c.title(e, *level, true);
                    } else {
                        c.element(e, *level);
                    }
                }
                if section {
                    c.out.push_str("</div>");
                    section_title(parent).unwrap_or_default()
                } else {
                    info.metadata.title.clone()
                }
            }
            Unit::Section(e, depth) => {
                c.element(e, *depth);
                section_title(e).unwrap_or_default()
            }
            Unit::Body(b) => {
                c.in_notes = true;
                for e in b.elements() {
                    if e.name == "title" {
                        c.title(e, 1, true);
                    } else {
                        c.element(e, 2);
                    }
                }
                b.child("title").map(|t| t.text_collapsed()).unwrap_or_else(|| b.attr("name").unwrap_or("").to_string())
            }
        };
        chapters.push(HtmlChapter { path: files.path(chapters.len()), html: chapter_html(&title, &c.out) });
    }
    // TOC: section titles of the main body (nested), then the titles of extra bodies.
    let mut toc = files.section_toc(main, section_title);
    for (i, u) in units.iter().enumerate() {
        if let Unit::Body(b) = u {
            let title = b.child("title").map(|t| t.text_collapsed()).filter(|t| !t.is_empty());
            if let Some(title) = title {
                toc.push(TocNode::new(title, Target::chapter(i)));
            }
        }
    }
    let mut loader = MemoryLoader::default();
    loader.0.insert("fb2.css".into(), [FB2_CSS, &stylesheet(&root)].concat().into_bytes());
    if let Some(cover) = &info.metadata.cover {
        if !images.contains_key(cover) {
            info.metadata.cover = None;
        }
    }
    for (path, (_, bytes)) in images {
        loader.0.insert(path, bytes);
    }
    Ok(Book { info, chapters, toc, page_list: Vec::new(), resources: Box::new(loader) })
}

fn stylesheet(root: &Element) -> String {
    root.children_named("stylesheet").filter(|s| s.attr("type").is_none_or(|t| t == "text/css")).map(|s| s.text()).collect::<Vec<_>>().join("\n")
}

/// Converts an FB2 file.
pub fn load(path: &Path) -> Result<Book> {
    let root = parse(&read_bytes(path)?)?;
    let mut book = convert(root)?;
    if book.info.metadata.title.is_empty() {
        book.info.metadata.title = crate::title_from(path);
    }
    if path.to_string_lossy().to_ascii_lowercase().ends_with(".zip") || path.to_string_lossy().to_ascii_lowercase().ends_with(".fbz") {
        book.info.version = "FB2 (zip)".into();
    }
    Ok(book)
}

/// Metadata and cover of an FB2 file.
///
/// Only the part before the first `body` or `binary` (the description) is built into a
/// tree, and the cover's `binary` element alone: a book of many MB of text and images
/// is not parsed whole for the library.
pub fn read_info(path: &Path) -> Result<BookInfo> {
    let text = xml::decode_xml_bytes(&read_bytes(path)?);
    let head_end = [find_bytes(text.as_bytes(), b"<body", 0), find_bytes(text.as_bytes(), b"<binary", 0)].into_iter().flatten().min();
    let mut info = info_of(&parse_text(&text[..head_end.unwrap_or(text.len())])?);
    if info.metadata.title.is_empty() {
        info.metadata.title = crate::title_from(path);
    }
    if let Some(cover) = info.metadata.cover.clone() {
        match find_binary(&text, &cover) {
            Some((media_type, bytes)) => info.cover = Some(Cover { path: cover, media_type, bytes }),
            None => info.metadata.cover = None,
        }
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const SAMPLE: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0" xmlns:l="http://www.w3.org/1999/xlink">
<description><title-info><genre>sf</genre><author><first-name>Ann</first-name><middle-name>B.</middle-name><last-name>Writer</last-name></author>
<book-title>Test Book</book-title><annotation><p>About it.</p></annotation><date value="1900-01-01">1900</date>
<coverpage><image l:href="#cover.png"/></coverpage><lang>en</lang><sequence name="Saga" number="2"/></title-info>
<document-info><id>abc-123</id></document-info><publish-info><publisher>Pub</publisher><isbn>978-0-00-000000-2</isbn></publish-info></description>
<body><title><p>Test Book</p></title><epigraph><p>Body epigraph.</p><text-author>Someone</text-author></epigraph>
<section><title><p>Part One</p></title>
  <section id="c1"><title><p>Chapter 1</p><p>The Start</p></title><p>Text with a note<a l:href="#n1" type="note">[1]</a> and <emphasis>emphasis</emphasis>.</p>
    <poem><title><p>Song</p></title><stanza><v>Line one</v><v>Line two</v></stanza><text-author>Poet</text-author></poem>
    <image l:href="#pic.png"/><empty-line/><subtitle>* * *</subtitle><cite><p>Quoted.</p></cite>
    <table><tr><th>A</th><td colspan="2">B</td></tr></table></section>
  <section><title><p>Chapter 2</p></title><p>Back to <a l:href="#c1">chapter 1</a>.</p></section>
</section></body>
<body name="notes"><title><p>Notes</p></title><section id="n1"><title><p>1</p></title><p>The note.</p></section></body>
<binary id="cover.png" content-type="image/png">iVBORw0KGgo=</binary>
<binary id="pic.png" content-type="image/png">iVBORw0K
Ggo=</binary>
</FictionBook>"##;

    #[test]
    fn metadata() {
        let root = parse(SAMPLE.as_bytes()).unwrap();
        let info = info_of(&root);
        assert_eq!(info.metadata.title, "Test Book");
        assert_eq!(info.metadata.authors, vec!["Ann B. Writer"]);
        assert_eq!(info.metadata.language.as_deref(), Some("en"));
        assert_eq!(info.metadata.series, Some(("Saga".into(), 2.0)));
        assert_eq!(info.metadata.cover.as_deref(), Some("images/cover.png"));
        assert_eq!(info.metadata.publisher.as_deref(), Some("Pub"));
        assert_eq!(info.metadata.description.as_deref(), Some("About it."));
        assert_eq!(info.identifiers.len(), 2);
        assert_eq!(info.subjects, vec!["sf"]);
        assert_eq!(info.date.as_deref(), Some("1900-01-01"));
    }

    #[test]
    fn chapters_toc_notes_images() {
        let book = convert(parse(SAMPLE.as_bytes()).unwrap()).unwrap();
        // Header, then the lone "Part One" section is opened: its header, ch 1, ch 2; then notes.
        let paths: Vec<&str> = book.chapters.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["fb2-001.xhtml", "fb2-002.xhtml", "fb2-003.xhtml", "fb2-004.xhtml", "fb2-005.xhtml"]);
        for c in &book.chapters {
            assert!(xml::is_well_formed(&c.html), "{}", c.html);
        }
        let ch1 = &book.chapters[2].html;
        assert!(ch1.contains("<h2 class=\"title\">Chapter 1<br/>The Start</h2>"), "{ch1}");
        assert!(ch1.contains("<a href=\"fb2-005.xhtml#n1\" class=\"note\" epub:type=\"noteref\">[1]</a>"), "{ch1}");
        assert!(ch1.contains("<em>emphasis</em>"));
        assert!(ch1.contains("<p class=\"v\">Line one</p>"));
        assert!(ch1.contains("<div class=\"image\"><img src=\"images/pic.png\" alt=\"\"/></div>"));
        assert!(ch1.contains("<td colspan=\"2\">B</td>"));
        assert!(book.chapters[3].html.contains("<a href=\"fb2-003.xhtml#c1\">chapter 1</a>"));
        assert!(book.chapters[4].html.contains("id=\"n1\" epub:type=\"footnote\""));
        assert!(book.chapters[0].html.contains("Body epigraph."));
        assert_eq!(book.toc.len(), 2);
        assert_eq!(book.toc[0].title, "Part One");
        assert_eq!(book.toc[0].target, Some(Target::id(1, "fb2-s1")));
        let kids: Vec<&str> = book.toc[0].children.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(kids, vec!["Chapter 1 The Start", "Chapter 2"]);
        assert_eq!(book.toc[0].children[0].target, Some(Target::id(2, "c1")));
        assert_eq!(book.toc[1].title, "Notes");
        assert_eq!(book.resources.read("images/pic.png").unwrap(), b"\x89PNG\r\n\x1a\n");
        assert!(book.resources.read("fb2.css").is_ok());
    }

    #[test]
    fn read_info_parses_the_description_and_the_cover_alone() {
        // What the whole tree gives (the old read_info), for the sample and variants: the
        // cover binary before the body, given twice (the last one counts), self-closing,
        // missing.
        let whole = |src: &str| {
            let root = parse(src.as_bytes()).unwrap();
            let info = info_of(&root);
            let cover = info.metadata.cover.as_ref().and_then(|c| binaries(&root).remove(c));
            (info.metadata, cover)
        };
        let dir = std::env::temp_dir().join(format!("ink-fb2-info-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = "<binary id=\"cover.png\" content-type=\"image/png\">iVBORw0KGgo=</binary>";
        let jpg = "<binary content-type=\"image/jpeg\" id=\"cover.png\">/9j/4AAQ</binary>";
        let variants = [
            SAMPLE.to_string(),
            SAMPLE.replacen("<body>", &format!("{png}<body>"), 1),
            SAMPLE.replace("</FictionBook>", &format!("{jpg}</FictionBook>")),
            SAMPLE.replace(png, "<binary id=\"cover.png\" content-type=\"image/png\"/>"),
            SAMPLE.replace(png, ""),
        ];
        for (i, src) in variants.iter().enumerate() {
            let f = dir.join(format!("v{i}.fb2"));
            std::fs::write(&f, src).unwrap();
            let info = read_info(&f).unwrap();
            let (meta, cover) = whole(src);
            assert_eq!(info.metadata.title, meta.title, "{i}");
            assert_eq!(info.metadata.authors, meta.authors, "{i}");
            assert_eq!(info.metadata.cover, cover.as_ref().map(|_| "images/cover.png".to_string()), "{i}");
            assert_eq!(info.cover.map(|c| (c.media_type, c.bytes)), cover, "{i}");
        }
        assert_eq!(whole(&variants[2]).1.map(|c| c.0).as_deref(), Some("image/jpeg"));
        assert_eq!(whole(&variants[4]).1, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cp1251_and_errors() {
        let src = "<?xml version=\"1.0\" encoding=\"windows-1251\"?><FictionBook><description><title-info><book-title>Война и мир</book-title></title-info></description><body><section><p>Ещё</p></section></body></FictionBook>";
        let (bytes, _, _) = encoding_rs::WINDOWS_1251.encode(src);
        let book = convert(parse(&bytes).unwrap()).unwrap();
        assert_eq!(book.info.metadata.title, "Война и мир");
        assert!(book.chapters[0].html.contains("Ещё"));
        assert!(parse(b"<html/>").is_err());
        assert!(convert(parse(b"<FictionBook><description/></FictionBook>").unwrap()).is_err());
    }
}
