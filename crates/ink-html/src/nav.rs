//! EPUB navigation: the table of contents and the page list from the EPUB 3 navigation
//! document (`<nav epub:type="toc">`, `<nav epub:type="page-list">`) and the EPUB 2 NCX
//! (`navMap`, `pageList`).
//!
//! The EPUB container (OPF) belongs to `ink-formats`; [`from_opf`] only looks up which
//! manifest items are the navigation document and the NCX, so the loader can hand the
//! result to [`crate::HtmlDocument::set_navigation`].

use crate::book::dir_of;
use crate::dom::{Dom, NodeId};
use ink_doc::{resolve_href, ResourceLoader};
use quick_xml::events::Event;
use quick_xml::Reader;

/// One navigation entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavPoint {
    /// Title (TOC) or page label (page list).
    pub title: String,
    /// Book path with an optional `#fragment`.
    pub href: String,
    /// Nesting depth, 0 = top level.
    pub level: u8,
}

/// TOC and page list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Navigation {
    /// Table of contents in reading order.
    pub toc: Vec<NavPoint>,
    /// Printed page labels.
    pub page_list: Vec<NavPoint>,
}

/// Resolves `href` (with fragment) against `dir`.
pub fn resolve_with_fragment(dir: &str, href: &str) -> String {
    let (path, frag) = href.split_once('#').map_or((href, None), |(p, f)| (p, Some(f)));
    let base = if path.is_empty() { String::new() } else { resolve_href(dir, path) };
    match frag {
        Some(f) => format!("{base}#{}", ink_doc::percent_decode(f)),
        None => base,
    }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parses an NCX document located at `ncx_path`.
pub fn parse_ncx(xml: &str, ncx_path: &str) -> Navigation {
    let dir = dir_of(ncx_path);
    let mut nav = Navigation::default();
    let mut r = Reader::from_str(xml);
    r.config_mut().check_end_names = false;
    let mut depth: i32 = -1;
    // (title, href) of the open navPoint / pageTarget entries.
    let mut stack: Vec<(String, Option<String>, bool, usize)> = Vec::new();
    let mut in_text = false;
    while let Ok(ev) = r.read_event() {
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let name = e.local_name().as_ref().to_vec();
                match name.as_slice() {
                    b"navPoint" | b"pageTarget" => {
                        let page = name.as_slice() == b"pageTarget";
                        if !page {
                            depth = depth.saturating_add(1);
                        }
                        let list = if page { &mut nav.page_list } else { &mut nav.toc };
                        list.push(NavPoint { title: String::new(), href: String::new(), level: depth.clamp(0, 255) as u8 });
                        stack.push((String::new(), None, page, list.len() - 1));
                    }
                    b"text" => in_text = true,
                    b"content" => {
                        if let Some(src) = e.attributes().flatten().find(|a| a.key.local_name().as_ref() == b"src") {
                            let v = crate::parse::decode_attr(&String::from_utf8_lossy(&src.value));
                            if let Some(top) = stack.last_mut() {
                                top.1 = Some(resolve_with_fragment(dir, &v));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) if in_text => {
                if let Some(top) = stack.last_mut() {
                    let s = crate::parse::decode_attr(&String::from_utf8_lossy(t.as_ref()));
                    top.0.push_str(&s);
                }
            }
            Event::CData(t) if in_text => {
                if let Some(top) = stack.last_mut() {
                    top.0.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"text" => in_text = false,
                b"navPoint" | b"pageTarget" => {
                    if let Some((title, href, page, idx)) = stack.pop() {
                        let list = if page { &mut nav.page_list } else { &mut nav.toc };
                        list[idx].title = collapse(&title);
                        list[idx].href = href.unwrap_or_default();
                        if !page {
                            depth = depth.saturating_sub(1);
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    nav.toc.retain(|p| !p.href.is_empty());
    nav.page_list.retain(|p| !p.href.is_empty());
    nav
}

struct NavDefaults;
impl crate::dom::Classifier for NavDefaults {
    fn classify(&mut self, dom: &Dom, id: NodeId, ws: crate::style::WhiteSpace) -> crate::style::StructStyle {
        crate::dom::default_struct(dom.name(id), ws)
    }
}

/// Entries of a navigation `<ol>`/`<ul>` in document order. Iterative (an explicit
/// stack), so lists nested 100000 deep cannot overflow the stack; levels saturate at 255.
fn walk_list(dom: &Dom, ol: NodeId, level: u8, dir: &str, out: &mut Vec<NavPoint>) {
    // (list, next child index, level)
    let mut stack: Vec<(NodeId, usize, u8)> = vec![(ol, 0, level)];
    while let Some(top) = stack.last_mut() {
        let (list, i, level) = *top;
        let kids = &dom.node(list).children;
        let Some(&li) = kids.get(i) else {
            stack.pop();
            continue;
        };
        top.1 += 1;
        if dom.name(li) != "li" {
            continue;
        }
        let mut title = String::new();
        let mut href = String::new();
        for &c in &dom.node(li).children {
            match dom.name(c) {
                "a" | "span" if title.is_empty() => {
                    title = collapse(&dom.text_content(c));
                    if let Some(h) = dom.element(c).and_then(|e| e.attr("href")) {
                        href = resolve_with_fragment(dir, h);
                    }
                }
                _ => {}
            }
        }
        if !href.is_empty() {
            out.push(NavPoint { title, href, level });
        }
        // Sublists of this item, in order (pushed last first).
        for &c in dom.node(li).children.iter().rev() {
            if matches!(dom.name(c), "ol" | "ul") {
                stack.push((c, 0, level.saturating_add(1)));
            }
        }
    }
}

/// Parses an EPUB 3 navigation document located at `nav_path`.
pub fn parse_nav(xhtml: &str, nav_path: &str) -> Navigation {
    let dir = dir_of(nav_path);
    let dom = crate::parse::parse_chapter(xhtml, &mut NavDefaults);
    let mut nav = Navigation::default();
    for &n in &dom.preorder {
        if dom.name(n) != "nav" {
            continue;
        }
        let ty = dom.element(n).and_then(|e| e.attr_local("type")).unwrap_or("").to_string();
        let list = dom.node(n).children.iter().copied().find(|&c| matches!(dom.name(c), "ol" | "ul"));
        let Some(list) = list else { continue };
        if ty.split_whitespace().any(|t| t == "toc") && nav.toc.is_empty() {
            walk_list(&dom, list, 0, dir, &mut nav.toc);
        } else if ty.split_whitespace().any(|t| t == "page-list") && nav.page_list.is_empty() {
            walk_list(&dom, list, 0, dir, &mut nav.page_list);
        }
    }
    nav
}

/// Finds the navigation document and NCX of an OPF and parses them. The NCX TOC is used
/// when it has entries (it always has hrefs), otherwise the navigation document's; the
/// page list comes from the navigation document, else from the NCX.
pub fn from_opf(opf: &str, opf_path: &str, loader: &dyn ResourceLoader) -> Navigation {
    let dir = dir_of(opf_path);
    let mut nav_href = None;
    let mut ncx_href = None;
    let mut toc_id = None;
    let mut items: Vec<(String, String)> = Vec::new();
    let mut r = Reader::from_str(opf);
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let get = |k: &[u8]| {
                    e.attributes()
                        .flatten()
                        .find(|a| a.key.local_name().as_ref() == k)
                        .map(|a| crate::parse::decode_attr(&String::from_utf8_lossy(&a.value)))
                };
                match e.local_name().as_ref() {
                    b"item" => {
                        let (id, href) = (get(b"id").unwrap_or_default(), get(b"href").unwrap_or_default());
                        if get(b"properties").is_some_and(|p| p.split_whitespace().any(|x| x == "nav")) {
                            nav_href = Some(href.clone());
                        }
                        if get(b"media-type").as_deref() == Some("application/x-dtbncx+xml") {
                            ncx_href = Some(href.clone());
                        }
                        items.push((id, href));
                    }
                    b"spine" => toc_id = get(b"toc"),
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if let Some(id) = toc_id {
        if let Some((_, h)) = items.iter().find(|(i, _)| *i == id) {
            ncx_href = Some(h.clone());
        }
    }
    let read = |h: &str| -> Option<(String, String)> {
        let p = resolve_href(dir, h);
        let b = loader.read(&p).ok()?;
        Some((String::from_utf8_lossy(&b).into_owned(), p))
    };
    let ncx = ncx_href.and_then(|h| read(&h)).map(|(x, p)| parse_ncx(&x, &p)).unwrap_or_default();
    let navd = nav_href.and_then(|h| read(&h)).map(|(x, p)| parse_nav(&x, &p)).unwrap_or_default();
    Navigation {
        toc: if !ncx.toc.is_empty() { ncx.toc } else { navd.toc },
        page_list: if !navd.page_list.is_empty() { navd.page_list } else { ncx.page_list },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_doc::MemoryLoader;

    const NCX: &str = r#"<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><navMap>
        <navPoint id="a" playOrder="1"><navLabel><text>Act I</text></navLabel><content src="Text/a.xhtml"/>
          <navPoint id="b"><navLabel><text>  Scene
            one </text></navLabel><content src="Text/a.xhtml#s1"/></navPoint></navPoint>
        <navPoint id="c"><navLabel><text>Act &amp; II</text></navLabel><content src="Text/b%20c.xhtml"/></navPoint></navMap>
        <pageList><pageTarget value="1"><navLabel><text>i</text></navLabel><content src="Text/a.xhtml#p1"/></pageTarget></pageList></ncx>"#;

    #[test]
    fn ncx_toc_and_page_list() {
        let n = parse_ncx(NCX, "OEBPS/toc.ncx");
        assert_eq!(
            n.toc,
            vec![
                NavPoint { title: "Act I".into(), href: "OEBPS/Text/a.xhtml".into(), level: 0 },
                NavPoint { title: "Scene one".into(), href: "OEBPS/Text/a.xhtml#s1".into(), level: 1 },
                NavPoint { title: "Act & II".into(), href: "OEBPS/Text/b c.xhtml".into(), level: 0 },
            ]
        );
        assert_eq!(n.page_list, vec![NavPoint { title: "i".into(), href: "OEBPS/Text/a.xhtml#p1".into(), level: 0 }]);
    }

    #[test]
    fn nav_document_and_opf_lookup() {
        let navx = r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
            <nav epub:type="toc"><ol><li><a href="t/a.xhtml">A</a><ol><li><a href="t/a.xhtml#x">A.1</a></li></ol></li><li><span>No link</span></li><li><a href="t/b.xhtml">B</a></li></ol></nav>
            <nav epub:type="page-list" hidden=""><ol><li><a href="t/a.xhtml#pg1">1</a></li><li><a href="t/b.xhtml#pg2">2</a></li></ol></nav></body></html>"#;
        let n = parse_nav(navx, "OPS/nav.xhtml");
        assert_eq!(n.toc.iter().map(|p| (p.title.as_str(), p.level)).collect::<Vec<_>>(), vec![("A", 0), ("A.1", 1), ("B", 0)]);
        assert_eq!(n.toc[1].href, "OPS/t/a.xhtml#x");
        assert_eq!(n.page_list.len(), 2);
        let opf = r#"<package><manifest><item id="nav" href="nav.xhtml" properties="nav" media-type="application/xhtml+xml"/><item id="n" href="toc.ncx" media-type="application/x-dtbncx+xml"/></manifest><spine toc="n"/></package>"#;
        let mut m = MemoryLoader::default();
        m.0.insert("OPS/nav.xhtml".into(), navx.as_bytes().to_vec());
        m.0.insert("OPS/toc.ncx".into(), NCX.as_bytes().to_vec());
        let all = from_opf(opf, "OPS/content.opf", &m);
        assert_eq!(all.toc.len(), 3);
        assert_eq!(all.toc[0].href, "OPS/Text/a.xhtml", "NCX TOC wins");
        assert_eq!(all.page_list.len(), 2, "nav page list wins");
        m.0.remove("OPS/toc.ncx");
        assert_eq!(from_opf(opf, "OPS/content.opf", &m).toc[0].href, "OPS/t/a.xhtml");
    }
}
