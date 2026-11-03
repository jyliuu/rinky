//! Single HTML/XHTML files.
//!
//! The charset comes from a BOM, `<meta charset>`, `<meta http-equiv="Content-Type">`
//! or the XML declaration, else from detection. The markup is repaired to XHTML
//! ([`crate::xhtml`]); headings `h1`-`h3` get ids (when they have none) and form the TOC.
//! Title, author and language come from `<title>`, `<meta name="author">` (or
//! `DC.creator`) and `<html lang>`. Images and stylesheets resolve against the file's
//! directory.

use crate::book::{Book, BookInfo, Target, TocNode};
use crate::encoding::decode_with_label;
use crate::xhtml::{self, Action, Rewrite};
use crate::xml::collapse_ws;
use crate::Format;
use ink_doc::{DirLoader, Result};
use ink_html::HtmlChapter;
use markup5ever_rcdom::Handle;
use std::path::Path;

/// The charset an HTML document declares in its first 4 KiB.
pub fn declared_charset(bytes: &[u8]) -> Option<String> {
    if let Some(e) = crate::xml::declared_encoding(bytes) {
        return Some(e);
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = head[from..].find("<meta") {
        let start = from + i;
        let end = head[start..].find('>').map(|e| start + e).unwrap_or(head.len());
        let tag = &head[start..end];
        if let Some(c) = tag.find("charset") {
            let rest = tag[c + 7..].trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let v: String = rest
                    .trim_start()
                    .trim_start_matches(['"', '\''])
                    .chars()
                    .take_while(|c| !matches!(c, '"' | '\'' | ';' | ' ' | '/' | '>'))
                    .collect();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        from = end;
    }
    None
}

/// Text read from a `<title>` or heading (a heading in broken markup can hold the rest
/// of the document; reading it all for every nested heading would be quadratic).
const TITLE_MAX: usize = 1024;

#[derive(Default)]
struct Collect {
    title: Option<String>,
    authors: Vec<String>,
    language: Option<String>,
    description: Option<String>,
    headings: Vec<(u8, String, String)>,
    ids: usize,
}

impl Rewrite for Collect {
    fn element(&mut self, node: &Handle, name: &mut String, attrs: &mut Vec<(String, String)>) -> Action {
        let get = |k: &str| attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        match name.as_str() {
            "html" => self.language = get("lang").or_else(|| get("xml:lang")),
            "title" if self.title.is_none() => {
                let t = collapse_ws(&xhtml::text_of_max(node, TITLE_MAX));
                if !t.is_empty() {
                    self.title = Some(t);
                }
            }
            "meta" => {
                let key = get("name").unwrap_or_default().to_ascii_lowercase();
                let content = get("content").unwrap_or_default();
                if !content.trim().is_empty() {
                    match key.as_str() {
                        "author" | "dc.creator" | "dcterms.creator" => self.authors.push(content.trim().to_string()),
                        "description" | "dc.description" => self.description = Some(content.trim().to_string()),
                        "dc.title" if self.title.is_none() => self.title = Some(content.trim().to_string()),
                        _ => {}
                    }
                }
            }
            "h1" | "h2" | "h3" => {
                let text = collapse_ws(&xhtml::text_of_max(node, TITLE_MAX));
                if !text.is_empty() {
                    let id = match get("id") {
                        Some(id) => id,
                        None => {
                            self.ids += 1;
                            let id = format!("ink-h{}", self.ids);
                            attrs.push(("id".into(), id.clone()));
                            id
                        }
                    };
                    self.headings.push((name.as_bytes()[1] - b'0', text, id));
                }
            }
            _ => {}
        }
        Action::Keep
    }
}

/// Nests headings by level (h1 > h2 > h3) into a TOC for chapter `chapter`.
pub fn heading_toc(headings: &[(u8, String, String)], chapter: usize) -> Vec<TocNode> {
    fn insert(nodes: &mut Vec<TocNode>, levels: &mut Vec<u8>, level: u8, node: TocNode) {
        // levels[i] is the level of the last node at depth i.
        while let Some(&l) = levels.last() {
            if l >= level {
                levels.pop();
            } else {
                break;
            }
        }
        let depth = levels.len();
        let mut list = nodes;
        for _ in 0..depth {
            list = &mut list.last_mut().expect("parent").children;
        }
        list.push(node);
        levels.push(level);
    }
    let mut nodes = Vec::new();
    let mut levels = Vec::new();
    for (level, text, id) in headings {
        insert(&mut nodes, &mut levels, *level, TocNode::new(text.clone(), Target::id(chapter, id.clone())));
    }
    nodes
}

/// Converted HTML: XHTML plus what was found in it.
struct Converted {
    xhtml: String,
    info: BookInfo,
    toc: Vec<TocNode>,
}

fn convert(path: &Path, bytes: &[u8]) -> Converted {
    let label = declared_charset(bytes);
    let d = decode_with_label(bytes, label.as_deref());
    let mut c = Collect::default();
    let xhtml = xhtml::to_xhtml(&d.text, &mut c);
    let mut info = BookInfo::new(Format::Html);
    info.version = format!("HTML {}", d.encoding);
    info.metadata.title = c.title.clone().or_else(|| c.headings.first().map(|h| h.1.clone())).unwrap_or_else(|| crate::title_from(path));
    info.metadata.authors = c.authors.clone();
    info.metadata.language = c.language.clone();
    info.metadata.description = c.description.clone();
    let toc = heading_toc(&c.headings, 0);
    Converted { xhtml, info, toc }
}

/// Metadata of an HTML file.
pub fn read_info(path: &Path) -> Result<BookInfo> {
    Ok(convert(path, &std::fs::read(path)?).info)
}

/// Opens an HTML file as a one-chapter book.
pub fn load(path: &Path) -> Result<Book> {
    let c = convert(path, &std::fs::read(path)?);
    let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("index.html").to_string();
    Ok(Book {
        info: c.info,
        chapters: vec![HtmlChapter { path: name, html: c.xhtml }],
        toc: c.toc,
        page_list: Vec::new(),
        resources: Box::new(DirLoader { root }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charsets() {
        assert_eq!(declared_charset(b"<html><head><meta charset=\"windows-1251\">").as_deref(), Some("windows-1251"));
        assert_eq!(
            declared_charset(b"<META HTTP-EQUIV='Content-Type' CONTENT='text/html; charset=ISO-8859-1'>").as_deref(),
            Some("iso-8859-1")
        );
        assert_eq!(declared_charset(b"<?xml version='1.0' encoding='koi8-r'?><html>").as_deref(), Some("koi8-r"));
        assert_eq!(declared_charset(b"<html><body>no</body></html>"), None);
    }

    #[test]
    fn metadata_toc_and_repair() {
        let src = "<html lang=da><head><title>Eventyr</title><meta name=author content='H. C. Andersen'></head><body>\
                   <h1>Del 1</h1><p>tekst<h2 id=k1>Kapitel 1</h2><p>mere<h2>Kapitel 2</h2><h3>Afsnit</h3><h1>Del 2</h1></body>";
        let (cp, _, _) = encoding_rs::WINDOWS_1252.encode(src);
        let c = convert(Path::new("/x/eventyr.html"), &cp);
        assert_eq!(c.info.metadata.title, "Eventyr");
        assert_eq!(c.info.metadata.authors, vec!["H. C. Andersen"]);
        assert_eq!(c.info.metadata.language.as_deref(), Some("da"));
        assert!(crate::xml::is_well_formed(&c.xhtml), "{}", c.xhtml);
        let t: Vec<(String, usize, Option<String>)> =
            c.toc.iter().map(|n| (n.title.clone(), n.children.len(), n.target.as_ref().and_then(|t| t.id.clone()))).collect();
        assert_eq!(t, vec![("Del 1".into(), 2, Some("ink-h1".into())), ("Del 2".into(), 0, Some("ink-h4".into()))]);
        assert_eq!(c.toc[0].children[0].target, Some(Target::id(0, "k1")));
        assert_eq!(c.toc[0].children[1].children[0].title, "Afsnit");
    }
}
