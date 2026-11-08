//! Front-ends that feed [`crate::dom::Builder`]:
//!
//! - Well-formed XHTML (the normal EPUB case) is read with `quick-xml`, which keeps
//!   self-closing tags, prefixed names (`svg:svg`, `epub:type`) and the exact text
//!   nodes, as crengine's XML parser does.
//! - Anything that is not well-formed XML (unclosed or mismatched tags, HTML void tags
//!   without `/`, stray `<`) is parsed with `html5ever`. Before that, self-closing
//!   non-void tags (`<a id="x"/>`, `<div/>`) are expanded to start + end tags (HTML
//!   parsing would otherwise swallow the following content), and the `<tbody>` that
//!   HTML parsing inserts into tables is dropped when the source has none, so paths stay
//!   `table/tr/td` like crengine's.

use crate::book::find_ci;
use crate::dom::{Builder, Classifier, Dom};
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// Parses one chapter. Never fails; an empty or unreadable chapter gives an empty body.
pub fn parse_chapter(src: &str, classifier: &mut dyn Classifier) -> Dom {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    match parse_xml(src, classifier) {
        Some(dom) => dom,
        None => {
            let mut dom = parse_html5(src, classifier);
            dom.html5_fallback = true;
            dom
        }
    }
}

/// Local part of a tag name (after the last `:`), borrowed when the name is UTF-8 (two
/// fewer allocations per element than an owned copy for each start and end tag).
fn local(name: &[u8]) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    match String::from_utf8_lossy(name) {
        Cow::Borrowed(s) => Cow::Borrowed(s.rsplit_once(':').map_or(s, |(_, l)| l)),
        Cow::Owned(s) => Cow::Owned(s.rsplit_once(':').map_or(s.as_str(), |(_, l)| l).to_string()),
    }
}

/// Decodes entities in an attribute value (whitespace kept).
pub fn decode_attr(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let end = rest[1..].find(|c: char| c == ';' || c == '&' || c.is_whitespace()).map(|e| e + 1);
        if let Some(e) = end.filter(|&e| rest.as_bytes().get(e) == Some(&b';')) {
            let name = &rest[1..e];
            let decoded = if let Some(num) = name.strip_prefix('#') {
                let code = num.strip_prefix(['x', 'X']).map_or_else(|| num.parse().ok(), |h| u32::from_str_radix(h, 16).ok());
                code.and_then(char::from_u32).map(|c| c.to_string())
            } else {
                crate::dom::named_entity(name).map(|(a, b)| std::iter::once(a).chain(b).collect())
            };
            if let Some(d) = decoded {
                out.push_str(&d);
                rest = &rest[e + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

fn attrs_of(e: &BytesStart) -> Vec<(String, String)> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .map(|a| {
            // Names keep their case (SVG has viewBox, preserveAspectRatio).
            let k = String::from_utf8_lossy(a.key.as_ref()).into_owned();
            let v = decode_attr(&String::from_utf8_lossy(&a.value));
            (k, v)
        })
        .collect()
}

/// Strict XML parse; `None` when the document is not well-formed.
fn parse_xml(src: &str, classifier: &mut dyn Classifier) -> Option<Dom> {
    let mut r = Reader::from_str(src);
    {
        let c = r.config_mut();
        c.check_end_names = true;
        c.expand_empty_elements = false;
        c.trim_text(false);
        c.check_comments = false;
    }
    let mut b = Builder::new(Some(classifier));
    let mut depth = 0usize;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                b.start(&local(e.name().as_ref()), attrs_of(&e));
                depth += 1;
            }
            Ok(Event::Empty(e)) => {
                let qname = e.name();
                let name = local(qname.as_ref());
                b.start(&name, attrs_of(&e));
                b.end(&name);
            }
            Ok(Event::End(e)) => {
                b.end(&local(e.name().as_ref()));
                depth = depth.checked_sub(1)?;
            }
            Ok(Event::Text(t)) => {
                let raw = String::from_utf8_lossy(t.as_ref());
                if raw.contains('<') {
                    return None;
                }
                b.text(&raw, true);
            }
            Ok(Event::CData(t)) => b.text(&String::from_utf8_lossy(t.as_ref()), false),
            Ok(Event::Comment(_)) | Ok(Event::PI(_)) => b.comment(),
            Ok(Event::Decl(_)) | Ok(Event::DocType(_)) => {}
            Ok(Event::Eof) => break,
            Err(_) => return None,
        }
    }
    if depth != 0 {
        return None;
    }
    Some(b.finish())
}

/// HTML void elements (no end tag).
fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta" | "param" | "source" | "track"
            | "wbr" | "image" | "keygen" | "basefont" | "bgsound" | "frame"
    )
}

/// Rewrites `<x .../>` for non-void HTML elements to `<x ...></x>`, outside comments,
/// CDATA, scripts and styles, and outside `<svg>`/`<math>` (foreign content honours `/>`).
pub fn expand_self_closing(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len() + 64);
    let mut i = 0;
    let mut last = 0;
    let mut foreign = 0i32;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if src[i..].starts_with("<!--") {
            i = src[i..].find("-->").map(|e| i + e + 3).unwrap_or(b.len());
            continue;
        }
        if src[i..].starts_with("<![CDATA[") {
            i = src[i..].find("]]>").map(|e| i + e + 3).unwrap_or(b.len());
            continue;
        }
        let closing = b.get(i + 1) == Some(&b'/');
        let name_start = i + 1 + closing as usize;
        let mut j = name_start;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b':' || b[j] == b'-' || b[j] == b'_') {
            j += 1;
        }
        if j == name_start {
            i += 1;
            continue;
        }
        let name = src[name_start..j].to_ascii_lowercase();
        let lname = name.rsplit(':').next().unwrap_or(&name).to_string();
        // Find the end of the tag, respecting quotes.
        let mut k = j;
        let mut quote = 0u8;
        while k < b.len() {
            let c = b[k];
            if quote != 0 {
                if c == quote {
                    quote = 0;
                }
            } else if c == b'"' || c == b'\'' {
                quote = c;
            } else if c == b'>' {
                break;
            }
            k += 1;
        }
        if k >= b.len() {
            break;
        }
        if (lname == "script" || lname == "style") && !closing {
            let close = format!("</{name}");
            let end = find_ci(src, k, &close).unwrap_or(b.len());
            i = end;
            continue;
        }
        if lname == "svg" || lname == "math" {
            if closing {
                foreign -= 1;
            } else if b[k - 1] != b'/' {
                foreign += 1;
            }
        }
        if !closing && foreign == 0 && b[k - 1] == b'/' && !is_void(&lname) {
            out.push_str(&src[last..k - 1]);
            out.push_str("></");
            out.push_str(&src[name_start..j]);
            out.push('>');
            last = k + 1;
        }
        i = k + 1;
    }
    out.push_str(&src[last..]);
    out
}

fn parse_html5(src: &str, classifier: &mut dyn Classifier) -> Dom {
    let fixed = expand_self_closing(src);
    let has_tbody = find_ci(src, 0, "<tbody").is_some();
    let rc = html5ever::parse_document(RcDom::default(), Default::default()).one(fixed);
    let mut b = Builder::new(Some(classifier));
    walk(&rc.document, &mut b, has_tbody);
    b.finish()
}

/// Feeds the html5ever tree to the builder (iteratively: deep nesting must not overflow
/// the stack).
fn walk(root: &Handle, b: &mut Builder, has_tbody: bool) {
    enum Step {
        Enter(Handle),
        Exit(String),
    }
    let mut stack = vec![Step::Enter(root.clone())];
    while let Some(step) = stack.pop() {
        let h = match step {
            Step::Exit(name) => {
                b.end(&name);
                continue;
            }
            Step::Enter(h) => h,
        };
        let push_children = |stack: &mut Vec<Step>| {
            for c in h.children.borrow().iter().rev() {
                stack.push(Step::Enter(c.clone()));
            }
        };
        match &h.data {
            NodeData::Document => push_children(&mut stack),
            NodeData::Element { name, attrs, .. } => {
                let lname = name.local.to_string();
                let lname = lname.rsplit(':').next().unwrap_or(&lname).to_string();
                if lname == "tbody" && !has_tbody {
                    push_children(&mut stack);
                    continue;
                }
                let attrs: Vec<(String, String)> = attrs
                    .borrow()
                    .iter()
                    .map(|a| {
                        let k = a.name.prefix.as_ref().map_or_else(|| a.name.local.to_string(), |p| format!("{p}:{}", a.name.local));
                        (k, a.value.to_string())
                    })
                    .collect();
                b.start(&lname, attrs);
                stack.push(Step::Exit(lname));
                push_children(&mut stack);
            }
            NodeData::Text { contents } => b.text(&contents.borrow(), false),
            NodeData::Comment { .. } | NodeData::ProcessingInstruction { .. } => b.comment(),
            NodeData::Doctype { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::{NodeId, NONE};
    use crate::style::{StructStyle, WhiteSpace};

    struct Defaults;
    impl Classifier for Defaults {
        fn classify(&mut self, dom: &Dom, id: NodeId, parent_ws: WhiteSpace) -> StructStyle {
            crate::dom::default_struct(dom.name(id), parent_ws)
        }
    }

    fn outline(d: &Dom, id: NodeId, out: &mut String) {
        match d.text(id) {
            Some(t) => out.push_str(&format!("'{t}'")),
            None => {
                out.push_str(d.name(id));
                if !d.node(id).children.is_empty() {
                    out.push('(');
                    for (i, &c) in d.node(id).children.iter().enumerate() {
                        if i > 0 {
                            out.push(' ');
                        }
                        outline(d, c, out);
                    }
                    out.push(')');
                }
            }
        }
    }

    fn tree(src: &str) -> (String, bool) {
        let d = parse_chapter(src, &mut Defaults);
        let mut s = String::new();
        if d.body != NONE {
            outline(&d, d.body, &mut s);
        }
        (s, d.html5_fallback)
    }

    #[test]
    fn xhtml_keeps_namespaces_and_self_closing_tags() {
        let src = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" xml:lang="da"><head><link rel="stylesheet" href="s.css"/><style>p{}</style></head>
<body><p><a id="x"/>After <span epub:type="noteref">1</span></p><svg:svg xmlns:svg="http://www.w3.org/2000/svg"><svg:rect/></svg:svg><table><tr><td>c</td></tr></table></body></html>"#;
        let d = parse_chapter(src, &mut Defaults);
        assert!(!d.html5_fallback);
        assert_eq!(d.root_lang(), Some("da"));
        assert_eq!(d.sheets, vec![crate::dom::SheetRef::Link("s.css".into()), crate::dom::SheetRef::Inline("p{}".into())]);
        let mut s = String::new();
        outline(&d, d.body, &mut s);
        assert_eq!(s, "body(p(a 'After ' span('1')) svg(rect) table(tr(td('c'))))");
        let span = d.preorder.iter().copied().find(|&n| d.name(n) == "span").unwrap();
        assert_eq!(d.element(span).unwrap().attr("epub:type"), Some("noteref"));
        assert_eq!(d.element(span).unwrap().attr_local("type"), Some("noteref"));
    }

    #[test]
    fn broken_markup_goes_through_html5ever() {
        let cases = [
            // unclosed paragraphs and void tags
            ("<html><body><p>One<br>two<p>Three</body></html>", "body(p('One' br 'two') p('Three'))"),
            // mismatched nesting
            ("<body><p><b>bold <i>both</b> italic</i></p></body>", "body(p(b('bold ' i('both')) i(' italic')))"),
            // self-closing non-void anchor in HTML syntax, no tbody in source
            ("<body><p><a id='n1'/>Text<br></p><table><tr><td>x</td></tr></table></body>", "body(p(a 'Text' br) table(tr(td('x'))))"),
            // no html/body at all, stray &
            ("<p>Fish & chips &amp; peas", "body(p('Fish & chips & peas'))"),
            ("<<<>>>", "body('<<<>>>')"),
        ];
        for (src, want) in cases {
            let (got, html5) = tree(src);
            assert!(html5, "{src}");
            assert_eq!(got, want, "{src}");
        }
        assert_eq!(tree(""), ("body".to_string(), false));
    }

    #[test]
    fn html5ever_keeps_prefixed_attributes_and_svg() {
        // Not well-formed (unclosed <p>, <br>): html5ever, with epub:type, xml:lang,
        // xlink:href and the SVG element names kept.
        let src = r##"<html xml:lang="de"><body><p>Text<a epub:type="noteref" href="#n1">1</a><br>
            <svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 10 10"><image xlink:href="c.jpg"/><linearGradient/></svg>
            <aside epub:type="footnote" id="n1"><p>Note</aside></body></html>"##;
        let d = parse_chapter(src, &mut Defaults);
        assert!(d.html5_fallback);
        assert_eq!(d.root_lang(), Some("de"));
        let a = d.preorder.iter().copied().find(|&n| d.name(n) == "a").unwrap();
        assert_eq!(d.element(a).unwrap().attr("epub:type"), Some("noteref"));
        let img = d.preorder.iter().copied().find(|&n| d.name(n) == "image").unwrap();
        assert_eq!(d.element(img).unwrap().attr("xlink:href"), Some("c.jpg"));
        assert!(d.element(img).unwrap().in_svg);
        assert!(d.preorder.iter().any(|&n| d.name(n) == "linearGradient"), "SVG camelCase names");
        let aside = d.preorder.iter().copied().find(|&n| d.name(n) == "aside").unwrap();
        assert_eq!(d.element(aside).unwrap().attr_local("type"), Some("footnote"));
    }

    #[test]
    fn self_closing_expansion() {
        assert_eq!(expand_self_closing("<a id='x'/><br/><p class=\"a/b\"/>"), "<a id='x'></a><br/><p class=\"a/b\"></p>");
        assert_eq!(expand_self_closing("<svg><rect/></svg><!-- <a/> -->"), "<svg><rect/></svg><!-- <a/> -->");
        assert_eq!(decode_attr("a&amp;b &bogus; &#x41;&#66;"), "a&b &bogus; AB");
    }
}
