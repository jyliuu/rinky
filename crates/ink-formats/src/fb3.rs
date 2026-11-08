//! FictionBook 3 (`.fb3`): an OPC zip package.
//!
//! `_rels/.rels` names the description (`.../relationships/Book`) and the cover
//! thumbnail; the description's relationships name the body; the body's relationships
//! name images (`<img src="rId">`). The body is converted like FB2: chapters per
//! top-level `section`, titles to headings, `epigraph`/`blockquote`, `poem`/`stanza`,
//! lists, tables, `note` references to footnote links, `notes`/`notebody` to a notes
//! chapter.

use crate::book::{Book, BookInfo, Cover, Identifier, Target, TocNode};
use crate::fb2::{assign_section_ids, chapter_html, Chapters, FB2_CSS};
use crate::util::{read_limited, sniff_media_type, MAX_ENTRY};
use crate::xml::{self, Element, Node};
use crate::Format;
use ink_doc::{percent_decode, resolve_href, DocError, MemoryLoader, Result};
use ink_html::HtmlChapter;
use std::collections::HashMap;
use std::path::Path;

const REL_BOOK: &str = "http://www.fictionbook.org/FictionBook3/relationships/Book";
const REL_BODY: &str = "http://www.fictionbook.org/FictionBook3/relationships/body";
const REL_THUMB: &str = "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail";

struct Package {
    zip: zip::ZipArchive<std::fs::File>,
    /// Normalised part name ([`part_key`]) -> zip entry name.
    names: HashMap<String, String>,
}

/// OPC part names compare case-insensitively, and the zip stores them
/// percent-encoded (`%D1%84%D0%B13/foo/meta.xml` for `фб3/FOO/meta.xml`).
fn part_key(name: &str) -> String {
    percent_decode(name.trim_start_matches('/')).to_lowercase()
}

impl Package {
    fn open(path: &Path) -> Result<Self> {
        let zip = zip::ZipArchive::new(std::fs::File::open(path)?).map_err(|e| DocError::Format(format!("FB3: not a zip: {e}")))?;
        let names = zip.file_names().filter_map(|n| n.ok()).map(|n| (part_key(&n), n.into_owned())).collect();
        Ok(Package { zip, names })
    }

    fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        let entry = if self.zip.index_for_name(name).is_some() {
            name.to_string()
        } else {
            self.names.get(&part_key(name)).cloned().ok_or_else(|| DocError::NotFound(name.into()))?
        };
        let e = self.zip.by_name(&entry).map_err(|_| DocError::NotFound(name.into()))?;
        let size = e.size();
        read_limited(e, size, MAX_ENTRY).map_err(|e| DocError::Format(format!("FB3 {name}: {e}")))
    }

    fn xml(&mut self, name: &str) -> Result<Element> {
        let b = self.read(name)?;
        xml::parse(&xml::decode_xml_bytes(&b)).map_err(|e| DocError::Format(format!("FB3 {name}: {e}")))
    }

    /// Relationships of part `part` (`""` for the package): id -> (type, target path).
    fn rels(&mut self, part: &str) -> HashMap<String, (String, String)> {
        let (dir, file) = match part.rfind('/') {
            Some(i) => (&part[..i + 1], &part[i + 1..]),
            None => ("", part),
        };
        let name = format!("{dir}_rels/{file}.rels");
        let Ok(root) = self.xml(&name) else { return HashMap::new() };
        root.children_named("Relationship")
            .filter_map(|r| {
                let target = r.attr("Target")?;
                let path = if target.starts_with('/') { resolve_href("", target) } else { resolve_href(dir, target) };
                Some((r.attr("Id")?.to_string(), (r.attr("Type").unwrap_or("").to_string(), path)))
            })
            .collect()
    }
}

fn find_rel<'a>(rels: &'a HashMap<String, (String, String)>, ty: &str) -> Option<&'a str> {
    let mut v: Vec<(&String, &(String, String))> = rels.iter().filter(|(_, (t, _))| t == ty).collect();
    v.sort();
    v.first().map(|(_, (_, p))| p.as_str())
}

fn title_main(el: &Element) -> Option<String> {
    let t = el.child("title")?;
    let s = t.child("main").map(|m| m.text_collapsed()).unwrap_or_else(|| t.text_collapsed());
    (!s.is_empty()).then_some(s)
}

fn info_of(desc: &Element) -> BookInfo {
    let mut info = BookInfo::new(Format::Fb3);
    info.version = format!("FB3 {}", desc.attr("version").unwrap_or("1.0"));
    info.metadata.title = title_main(desc).unwrap_or_default();
    if let Some(rel) = desc.child("fb3-relations") {
        for s in rel.children_named("subject") {
            if s.attr("link") == Some("author") {
                let name = title_main(s).unwrap_or_else(|| {
                    ["first-name", "middle-name", "last-name"].iter().filter_map(|n| s.child(n).map(|e| e.text_collapsed())).collect::<Vec<_>>().join(" ")
                });
                if !name.trim().is_empty() {
                    info.metadata.authors.push(name.trim().to_string());
                }
            }
        }
    }
    if let Some(seq) = desc.child("sequence") {
        if let Some(name) = title_main(seq) {
            let n = seq.attr("number").and_then(|n| n.trim().parse().ok()).unwrap_or(1.0);
            info.metadata.series = Some((name, n));
        }
    }
    info.metadata.language = desc.child("lang").map(|l| l.text_collapsed()).filter(|l| !l.is_empty());
    info.metadata.description = desc.child("annotation").map(|a| a.text_collapsed()).filter(|a| !a.is_empty());
    if let Some(c) = desc.child("fb3-classification") {
        info.subjects = c.children_named("subject").map(|s| s.text_collapsed()).filter(|s| !s.is_empty()).collect();
    }
    info.date = desc
        .child("written")
        .and_then(|w| w.child("date"))
        .and_then(|d| d.attr("value").map(str::to_string).or_else(|| Some(d.text_collapsed())))
        .filter(|d| !d.is_empty());
    if let Some(id) = desc.attr("id") {
        info.identifiers.push(Identifier { scheme: Some("FB3".into()), value: id.to_string() });
    }
    if let Some(isbn) = desc.find("isbn").map(|i| i.text_collapsed()).filter(|i| !i.is_empty()) {
        info.identifiers.push(Identifier { scheme: Some("ISBN".into()), value: isbn });
    }
    info.metadata.identifier = info.identifiers.first().map(|i| i.value.clone());
    info
}

struct Conv<'a> {
    images: &'a HashMap<String, String>,
    chapters: &'a Chapters,
    out: String,
}

/// FB3 elements written as one XHTML element: (FB3 name, tag, class).
const ELEMENTS: &[(&str, &str, Option<&str>)] = &[
    ("p", "p", None),
    ("subtitle", "p", Some("subtitle")),
    ("text-author", "p", Some("text-author")),
    ("epigraph", "blockquote", Some("epigraph")),
    ("blockquote", "blockquote", Some("cite")),
    ("poem", "div", Some("poem")),
    ("annotation", "div", Some("annotation")),
    ("div", "div", None),
    ("pre", "pre", None),
    ("ol", "ol", None),
    ("ul", "ul", None),
    ("li", "li", None),
    ("table", "table", None),
    ("tr", "tr", None),
    ("strong", "strong", None),
    ("em", "em", None),
    ("sub", "sub", None),
    ("sup", "sup", None),
    ("code", "code", None),
    ("underline", "u", None),
    ("strikethrough", "s", None),
    ("spacing", "span", None),
    ("span", "span", None),
    ("paper-page-break", "span", None),
];

impl Conv<'_> {
    /// Writes the start tag with its class and the id of `el`.
    fn open(&mut self, tag: &str, class: Option<&str>, el: &Element) {
        self.out.push('<');
        self.out.push_str(tag);
        if let Some(c) = class {
            xml::push_attr(&mut self.out, "class", c);
        }
        if let Some(id) = el.attr("id") {
            xml::push_attr(&mut self.out, "id", id);
        }
        self.out.push('>');
    }

    fn close(&mut self, tag: &str) {
        self.out.push_str("</");
        self.out.push_str(tag);
        self.out.push('>');
    }

    fn wrap(&mut self, tag: &str, class: Option<&str>, el: &Element, depth: u8) {
        self.open(tag, class, el);
        self.children(el, depth);
        self.close(tag);
    }

    fn children(&mut self, el: &Element, depth: u8) {
        for c in &el.children {
            match c {
                Node::Text(t) => xml::push_text(&mut self.out, t),
                Node::Element(e) => self.element(e, depth),
            }
        }
    }

    /// Child elements, titles as headings of `title_depth`, the rest at `depth`.
    fn blocks(&mut self, el: &Element, title_depth: u8, depth: u8) {
        for c in el.elements() {
            if c.name == "title" {
                self.title(c, title_depth);
            } else {
                self.element(c, depth);
            }
        }
    }

    fn title(&mut self, el: &Element, depth: u8) {
        let tag = format!("h{}", depth.clamp(1, 6));
        self.open(&tag, Some("title"), el);
        let mut lines = el.children_named("p").peekable();
        if lines.peek().is_none() {
            self.children(el, depth);
        }
        for (i, p) in lines.enumerate() {
            if i > 0 {
                self.out.push_str("<br/>");
            }
            self.children(p, depth);
        }
        self.close(&tag);
    }

    /// `<a href="...">` for a link or note reference, `href` pointed at its chapter.
    fn link(&mut self, el: &Element, note: bool) {
        self.out.push_str("<a");
        xml::push_attr(&mut self.out, "href", &self.chapters.href(el.attr("href").unwrap_or("")));
        if note {
            self.out.push_str(" class=\"note\" epub:type=\"noteref\"");
        }
        self.out.push('>');
    }

    fn element(&mut self, el: &Element, depth: u8) {
        match el.name.as_str() {
            "section" => {
                self.open("div", Some("section"), el);
                for c in el.elements() {
                    match c.name.as_str() {
                        "title" => self.title(c, depth),
                        "section" => self.element(c, depth.saturating_add(1)),
                        _ => self.element(c, depth),
                    }
                }
                self.close("div");
            }
            "title" => self.title(el, depth.max(1)),
            "stanza" => {
                self.open("div", Some("stanza"), el);
                for c in el.elements() {
                    if c.name == "p" {
                        self.wrap("p", Some("v"), c, depth);
                    } else {
                        self.element(c, depth);
                    }
                }
                self.close("div");
            }
            "th" | "td" => {
                self.out.push('<');
                self.out.push_str(&el.name);
                for a in crate::fb2::CELL_ATTRS {
                    if let Some(v) = el.attr(a) {
                        xml::push_attr(&mut self.out, a, v);
                    }
                }
                self.out.push('>');
                self.children(el, depth);
                self.close(&el.name);
            }
            "br" => self.out.push_str("<br/>"),
            "a" => {
                self.link(el, false);
                self.children(el, depth);
                self.close("a");
            }
            "note" => {
                self.link(el, true);
                if el.children.is_empty() {
                    xml::push_text(&mut self.out, el.attr("autotext").unwrap_or("*"));
                } else {
                    self.children(el, depth);
                }
                self.close("a");
            }
            "img" => {
                if let Some(path) = el.attr("src").and_then(|s| self.images.get(s)) {
                    self.out.push_str("<img");
                    xml::push_attr(&mut self.out, "src", path);
                    xml::push_attr(&mut self.out, "alt", el.attr("alt").unwrap_or(""));
                    self.out.push_str("/>");
                }
            }
            "notes" => {
                self.out.push_str("<div class=\"notes\">");
                self.blocks(el, 1, 2);
                self.close("div");
            }
            "notebody" => {
                self.out.push_str("<div class=\"section\"");
                if let Some(id) = el.attr("id") {
                    xml::push_attr(&mut self.out, "id", id);
                    self.out.push_str(" epub:type=\"footnote\"");
                }
                self.out.push('>');
                self.blocks(el, depth, depth);
                self.close("div");
            }
            name => match ELEMENTS.iter().find(|e| e.0 == name) {
                Some(&(_, tag, class)) => self.wrap(tag, class, el, depth),
                None => self.children(el, depth),
            },
        }
    }
}

/// A section's TOC title: the paragraphs of its title joined.
fn section_title(s: &Element) -> Option<String> {
    s.child("title").map(|t| t.children_named("p").map(|p| p.text_collapsed()).collect::<Vec<_>>().join(" ")).filter(|t| !t.trim().is_empty())
}

struct Parts {
    info: BookInfo,
    body_path: Option<String>,
    thumb: Option<String>,
}

fn read_parts(pkg: &mut Package) -> Result<Parts> {
    let root = pkg.rels("");
    let desc_path = find_rel(&root, REL_BOOK).map(str::to_string).unwrap_or_else(|| "fb3/description.xml".into());
    let desc = pkg.xml(&desc_path)?;
    let info = info_of(&desc);
    let body_path = find_rel(&pkg.rels(&desc_path), REL_BODY).map(str::to_string).or_else(|| Some("fb3/body.xml".into()));
    let thumb = find_rel(&root, REL_THUMB).map(str::to_string);
    Ok(Parts { info, body_path, thumb })
}

/// Metadata and cover of an FB3 file.
pub fn read_info(path: &Path) -> Result<BookInfo> {
    let mut pkg = Package::open(path)?;
    let mut p = read_parts(&mut pkg)?;
    if p.info.metadata.title.is_empty() {
        p.info.metadata.title = crate::title_from(path);
    }
    if let Some(t) = p.thumb {
        if let Ok(bytes) = pkg.read(&t) {
            if let Some(mt) = sniff_media_type(&bytes) {
                p.info.metadata.cover = Some(t.clone());
                p.info.cover = Some(Cover { path: t, media_type: mt.to_string(), bytes });
            }
        }
    }
    Ok(p.info)
}

/// Converts an FB3 file.
pub fn load(path: &Path) -> Result<Book> {
    let mut pkg = Package::open(path)?;
    let mut p = read_parts(&mut pkg)?;
    if p.info.metadata.title.is_empty() {
        p.info.metadata.title = crate::title_from(path);
    }
    let body_path = p.body_path.clone().unwrap_or_default();
    let mut body = pkg.xml(&body_path)?;
    let mut n = 0;
    assign_section_ids(&mut body, "fb3", &mut n);
    // Images: relationship id -> loader path (the zip path).
    let mut loader = MemoryLoader::default();
    let mut images = HashMap::new();
    for (id, (_, target)) in pkg.rels(&body_path) {
        if let Ok(bytes) = pkg.read(&target) {
            if sniff_media_type(&bytes).is_some() {
                images.insert(id, target.clone());
                loader.0.insert(target, bytes);
            }
        }
    }
    if let Some(t) = &p.thumb {
        if let Ok(bytes) = pkg.read(t) {
            p.info.metadata.cover = Some(t.clone());
            loader.0.insert(t.clone(), bytes);
        }
    }
    loader.0.insert("fb2.css".into(), FB2_CSS.as_bytes().to_vec());
    // Units: header (before the first section), sections, notes.
    let header: Vec<&Element> = body.elements().take_while(|e| e.name != "section" && e.name != "notes").collect();
    let sections: Vec<&Element> = body.children_named("section").collect();
    let notes: Vec<&Element> = body.children_named("notes").collect();
    let mut units: Vec<(&str, Vec<&Element>)> = Vec::new();
    if !header.is_empty() {
        units.push(("header", header));
    }
    for s in &sections {
        units.push(("section", vec![*s]));
    }
    for n in &notes {
        units.push(("notes", vec![*n]));
    }
    if units.is_empty() {
        return Err(DocError::Format("FB3 body is empty".into()));
    }
    let mut files = Chapters::new("fb3");
    for (i, (_, els)) in units.iter().enumerate() {
        for e in els {
            files.add(e, i);
        }
    }
    let mut chapters = Vec::new();
    for (i, (kind, els)) in units.iter().enumerate() {
        let mut c = Conv { images: &images, chapters: &files, out: String::new() };
        for e in els {
            if *kind == "header" && e.name == "title" {
                c.title(e, 1);
            } else {
                c.element(e, 1);
            }
        }
        let title = match *kind {
            "section" => els[0].child("title").map(|t| t.text_collapsed()).unwrap_or_default(),
            "notes" => els[0].child("title").map(|t| t.text_collapsed()).unwrap_or_else(|| "Notes".into()),
            _ => p.info.metadata.title.clone(),
        };
        chapters.push(HtmlChapter { path: files.path(i), html: chapter_html(&title, &c.out) });
    }
    let mut toc = files.section_toc(&body, section_title);
    for (i, (kind, els)) in units.iter().enumerate() {
        if *kind == "notes" {
            if let Some(t) = els[0].child("title").map(|t| t.text_collapsed()).filter(|t| !t.is_empty()) {
                toc.push(TocNode::new(t, Target::chapter(i)));
            }
        }
    }
    Ok(Book { info: p.info, chapters, toc, page_list: Vec::new(), resources: Box::new(loader) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> std::path::PathBuf {
        crate::tests::testdata("formats/sample.fb3")
    }

    #[test]
    fn sample_fb3() {
        let info = read_info(&fixture()).unwrap();
        assert_eq!(info.metadata.title, "FB3 Sample Book");
        assert_eq!(info.metadata.authors, vec!["Inkreader Fixtures"]);
        assert_eq!(info.metadata.series, Some(("Inkreader Tests".into(), 4.0)));
        assert_eq!(info.metadata.language.as_deref(), Some("en"));
        assert_eq!(info.cover.as_ref().map(|c| c.media_type.as_str()), Some("image/png"));
        let book = load(&fixture()).unwrap();
        assert_eq!(book.chapters.len(), 4);
        for c in &book.chapters {
            assert!(xml::is_well_formed(&c.html), "{}", c.html);
        }
        let ch1 = &book.chapters[1].html;
        assert!(ch1.contains("<a href=\"fb3-004.xhtml#n1\" class=\"note\" epub:type=\"noteref\">1</a>"), "{ch1}");
        assert!(ch1.contains("<img src=\"fb3/img/pic.png\" alt=\"picture\"/>"), "{ch1}");
        assert!(ch1.contains("<u>underlined</u>"));
        assert!(ch1.contains("<a href=\"fb3-003.xhtml#s2\">link</a>"));
        assert!(book.chapters[3].html.contains("id=\"n1\" epub:type=\"footnote\""));
        let titles: Vec<&str> = book.toc.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["Chapter One", "Chapter Two", "Notes"]);
        assert_eq!(book.toc[1].children[0].target, Some(Target::id(2, "s2a")));
        assert!(book.resources.read("fb3/img/pic.png").is_ok());
    }

    #[test]
    fn opc_part_names_are_case_insensitive_and_percent_encoded() {
        // Like "Hardcore file structure.fb3" from github.com/gribuser/FB3: zip names are
        // percent-encoded, relationship targets differ in case, the rels part of
        // `фб3/foo/meta.xml` is `_RELS/META.XML.RELS`, the body has no extension.
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("ink-fb3-opc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("opc.fb3");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&f).unwrap());
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";
        let parts: [(&str, &[u8]); 5] = [
            ("_rels/.rels", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="t" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail" Target="cover/thumb.png"/><Relationship Id="b" Type="http://www.fictionbook.org/FictionBook3/relationships/Book" Target="%D1%84%D0%B13/FOO/meta.xml"/></Relationships>"#),
            ("cover/THUMB.PNG", png),
            ("%D1%84%D0%B13/foo/meta.xml", r#"<fb3-description xmlns="http://www.fictionbook.org/FictionBook3/description"><title><main>Книга</main></title><fb3-relations><subject link="author"><title><main>Автор</main></title></subject></fb3-relations><lang>ru</lang></fb3-description>"#.as_bytes()),
            ("%D1%84%D0%B13/foo/_RELS/META.XML.RELS", br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r" Type="http://www.fictionbook.org/FictionBook3/relationships/body" Target="../../%D0%BA%D0%BD%D0%B8%D0%B3%D0%B0"/></Relationships>"#),
            ("%D0%BA%D0%BD%D0%B8%D0%B3%D0%B0", r#"<fb3-body xmlns="http://www.fictionbook.org/FictionBook3/body"><section id="s1"><title><p>Глава</p></title><p>Текст</p></section></fb3-body>"#.as_bytes()),
        ];
        for (name, data) in parts {
            z.start_file(name, opts).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
        let info = read_info(&f).unwrap();
        assert_eq!(info.metadata.title, "Книга");
        assert_eq!(info.metadata.language.as_deref(), Some("ru"));
        assert_eq!(info.cover.map(|c| c.media_type), Some("image/png".to_string()));
        let book = load(&f).unwrap();
        assert!(book.chapters.iter().any(|c| c.html.contains("Текст")));
        assert_eq!(book.toc[0].title, "Глава");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
