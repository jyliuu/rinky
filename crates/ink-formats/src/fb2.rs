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
use crate::util::{base64_decode, read_limited, sniff_media_type, MAX_ENTRY};
use crate::xml::{self, escape_attr, escape_text, Element, Node};
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
    let text = xml::decode_xml_bytes(bytes);
    let root = xml::parse(&text).map_err(|e| DocError::Format(format!("FB2: {e}")))?;
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

/// The `binary` elements: path -> (media type, bytes). With `only`, decodes just that path.
pub fn binaries(root: &Element, only: Option<&str>) -> HashMap<String, (String, Vec<u8>)> {
    let mut out = HashMap::new();
    for b in root.children_named("binary") {
        let Some(id) = b.attr("id") else { continue };
        let path = image_path(id);
        if only.is_some_and(|o| o != path) {
            continue;
        }
        let bytes = base64_decode(b.text().as_bytes());
        let mt = sniff_media_type(&bytes).map(str::to_string).or_else(|| b.attr("content-type").map(str::to_string)).unwrap_or_default();
        out.insert(path, (mt, bytes));
    }
    out
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
    ids: &'a HashMap<String, String>,
    images: &'a HashMap<String, (String, Vec<u8>)>,
    out: String,
    /// Note bodies render their sections as footnotes.
    in_notes: bool,
    /// Inside a paragraph-like element (images are inline there).
    inline: bool,
}

fn heading(depth: u8) -> String {
    format!("h{}", depth.clamp(1, 6))
}

impl Conv<'_> {
    fn id_attr(&mut self, el: &Element) {
        if let Some(id) = el.attr("id") {
            self.out.push_str(&format!(" id=\"{}\"", escape_attr(id)));
        }
    }

    fn children(&mut self, el: &Element, depth: u8) {
        for c in &el.children {
            match c {
                Node::Text(t) => xml::push_text(&mut self.out, t),
                Node::Element(e) => self.element(e, depth),
            }
        }
    }

    /// Block children only (whitespace between blocks dropped).
    fn blocks(&mut self, el: &Element, depth: u8) {
        for e in el.elements() {
            self.element(e, depth);
        }
    }

    fn wrap(&mut self, tag: &str, class: Option<&str>, el: &Element, depth: u8, inline: bool) {
        self.out.push('<');
        self.out.push_str(tag);
        if let Some(c) = class {
            self.out.push_str(&format!(" class=\"{c}\""));
        }
        self.id_attr(el);
        self.out.push('>');
        if inline {
            let was = std::mem::replace(&mut self.inline, true);
            self.children(el, depth);
            self.inline = was;
        } else {
            self.blocks(el, depth);
        }
        self.out.push_str(&format!("</{tag}>"));
    }

    fn title(&mut self, el: &Element, depth: u8, section_title: bool) {
        let lines: Vec<&Element> = el.elements().filter(|e| e.name == "p").collect();
        if section_title {
            let tag = heading(depth);
            self.out.push_str(&format!("<{tag} class=\"title\""));
            self.id_attr(el);
            self.out.push('>');
            let was = std::mem::replace(&mut self.inline, true);
            for (i, p) in lines.iter().enumerate() {
                if i > 0 {
                    self.out.push_str("<br/>");
                }
                self.children(p, depth);
            }
            self.inline = was;
            self.out.push_str(&format!("</{tag}>"));
        } else {
            self.wrap("div", Some("title"), el, depth, false);
        }
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
        let alt = el.attr("alt").or_else(|| el.attr("title")).unwrap_or("");
        let img = format!("<img src=\"{}\" alt=\"{}\"/>", escape_attr(&path), escape_attr(alt));
        if block {
            self.out.push_str("<div class=\"image\"");
            self.id_attr(el);
            self.out.push('>');
            self.out.push_str(&img);
            self.out.push_str("</div>");
        } else {
            self.out.push_str(&img);
        }
    }

    fn link(&mut self, el: &Element, depth: u8) {
        let href = el.attr("href").unwrap_or("");
        let target = match href.strip_prefix('#') {
            Some(id) => match self.ids.get(id) {
                Some(file) => format!("{file}#{id}"),
                None => href.to_string(),
            },
            None => href.to_string(),
        };
        let note = el.attr("type") == Some("note");
        self.out.push_str(&format!("<a href=\"{}\"", escape_attr(&target)));
        if note {
            self.out.push_str(" class=\"note\" epub:type=\"noteref\"");
        }
        self.id_attr(el);
        self.out.push('>');
        let was = std::mem::replace(&mut self.inline, true);
        self.children(el, depth);
        self.inline = was;
        self.out.push_str("</a>");
    }

    fn element(&mut self, el: &Element, depth: u8) {
        match el.name.as_str() {
            "section" => {
                let footnote = self.in_notes && el.attr("id").is_some();
                self.out.push_str("<div class=\"section\"");
                self.id_attr(el);
                if footnote {
                    self.out.push_str(" epub:type=\"footnote\"");
                }
                self.out.push('>');
                for c in el.elements() {
                    if c.name == "title" {
                        self.title(c, depth, true);
                    } else if c.name == "section" {
                        self.element(c, depth.saturating_add(1));
                    } else {
                        self.element(c, depth);
                    }
                }
                self.out.push_str("</div>");
            }
            "title" => self.title(el, depth, false),
            "p" => self.wrap("p", None, el, depth, true),
            "v" => self.wrap("p", Some("v"), el, depth, true),
            "subtitle" => self.wrap("p", Some("subtitle"), el, depth, true),
            "text-author" => self.wrap("p", Some("text-author"), el, depth, true),
            "empty-line" => self.out.push_str("<p class=\"empty-line\">\u{a0}</p>"),
            "epigraph" => self.wrap("blockquote", Some("epigraph"), el, depth, false),
            "cite" => self.wrap("blockquote", Some("cite"), el, depth, false),
            "poem" => self.wrap("div", Some("poem"), el, depth, false),
            "stanza" => self.wrap("div", Some("stanza"), el, depth, false),
            "annotation" => self.wrap("div", Some("annotation"), el, depth, false),
            "date" => self.wrap("p", Some("date"), el, depth, true),
            "image" => self.image(el, !self.inline),
            "table" => self.wrap("table", None, el, depth, false),
            "tr" => self.wrap("tr", None, el, depth, false),
            "th" | "td" => {
                let tag = el.name.clone();
                self.out.push('<');
                self.out.push_str(&tag);
                for a in ["colspan", "rowspan", "align", "valign"] {
                    if let Some(v) = el.attr(a) {
                        self.out.push_str(&format!(" {a}=\"{}\"", escape_attr(v)));
                    }
                }
                self.id_attr(el);
                self.out.push('>');
                let was = std::mem::replace(&mut self.inline, true);
                self.children(el, depth);
                self.inline = was;
                self.out.push_str(&format!("</{tag}>"));
            }
            "strong" => self.wrap("strong", None, el, depth, true),
            "emphasis" => self.wrap("em", None, el, depth, true),
            "strikethrough" => self.wrap("s", None, el, depth, true),
            "sub" => self.wrap("sub", None, el, depth, true),
            "sup" => self.wrap("sup", None, el, depth, true),
            "code" => self.wrap("code", None, el, depth, true),
            "style" => {
                let class = el.attr("name").map(|n| n.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_"));
                self.wrap("span", class.as_deref(), el, depth, true)
            }
            "a" => self.link(el, depth),
            // Unknown elements: keep their content.
            _ => self.children(el, depth),
        }
    }
}

/// Gives every section an id (`fb2-s<N>`) so TOC entries can point at it.
fn assign_ids(el: &mut Element, n: &mut usize) {
    if el.name == "section" && el.attr("id").is_none() {
        *n += 1;
        el.attrs.push(xml::Attr { local: "id".into(), qname: "id".into(), value: format!("fb2-s{n}") });
    }
    for c in &mut el.children {
        if let Node::Element(e) = c {
            assign_ids(e, n);
        }
    }
}

fn collect_ids(el: &Element, file: &str, out: &mut HashMap<String, String>) {
    if let Some(id) = el.attr("id") {
        out.entry(id.to_string()).or_insert_with(|| file.to_string());
    }
    for e in el.elements() {
        collect_ids(e, file, out);
    }
}

fn section_title(el: &Element) -> Option<String> {
    let t = el.child("title")?;
    let lines: Vec<String> = t.children_named("p").map(|p| p.text_collapsed()).filter(|s| !s.is_empty()).collect();
    let s = if lines.is_empty() { t.text_collapsed() } else { lines.join(" ") };
    (!s.is_empty()).then_some(s)
}

/// TOC nodes for the sections below `el` (sections without titles pass their children up).
fn section_toc(el: &Element, chapter_of: &dyn Fn(&str) -> Option<usize>) -> Vec<TocNode> {
    let mut out = Vec::new();
    for s in el.children_named("section") {
        let children = section_toc(s, chapter_of);
        let id = s.attr("id").unwrap_or("");
        match (section_title(s), chapter_of(id)) {
            (Some(title), Some(ch)) => out.push(TocNode { title, target: Some(Target::id(ch, id)), children }),
            _ => out.extend(children),
        }
    }
    out
}

fn chapter_html(title: &str, body: &str) -> String {
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
    assign_ids(&mut root, &mut n);
    let images = binaries(&root, None);
    let bodies: Vec<&Element> = root.children_named("body").collect();
    let main = bodies.iter().copied().find(|b| is_main_body(b)).or(bodies.first().copied()).ok_or_else(|| DocError::Format("FB2 has no <body>".into()))?;
    let mut units = units_of(main);
    for b in &bodies {
        if !std::ptr::eq(*b, main) {
            units.push(Unit::Body(b));
        }
    }
    let file = |i: usize| format!("fb2-{:03}.xhtml", i + 1);
    let mut ids = HashMap::new();
    for (i, u) in units.iter().enumerate() {
        match u {
            Unit::Header(parent, els, _) => {
                if parent.name == "section" {
                    if let Some(id) = parent.attr("id") {
                        ids.entry(id.to_string()).or_insert_with(|| file(i));
                    }
                }
                els.iter().for_each(|e| collect_ids(e, &file(i), &mut ids))
            }
            Unit::Section(e, _) | Unit::Body(e) => collect_ids(e, &file(i), &mut ids),
        }
    }
    let mut chapters = Vec::with_capacity(units.len());
    for u in &units {
        let mut c = Conv { ids: &ids, images: &images, out: String::new(), in_notes: false, inline: false };
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
        chapters.push(HtmlChapter { path: file(chapters.len()), html: chapter_html(&title, &c.out) });
    }
    // TOC: section titles of the main body (nested), then the titles of extra bodies.
    let chapter_of = |id: &str| ids.get(id).and_then(|f| f.trim_start_matches("fb2-").trim_end_matches(".xhtml").parse::<usize>().ok()).map(|n| n - 1);
    let mut toc = section_toc(main, &chapter_of);
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
pub fn read_info(path: &Path) -> Result<BookInfo> {
    let root = parse(&read_bytes(path)?)?;
    let mut info = info_of(&root);
    if info.metadata.title.is_empty() {
        info.metadata.title = crate::title_from(path);
    }
    if let Some(cover) = info.metadata.cover.clone() {
        match binaries(&root, Some(&cover)).remove(&cover) {
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
