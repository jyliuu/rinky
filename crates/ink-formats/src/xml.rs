//! A small XML element tree on top of `quick-xml`, plus escaping and entity helpers.
//!
//! Used for the book formats that are XML (OPF, NCX, nav, FB2, FB3). Names are kept
//! as local names (namespace prefixes dropped); attributes keep their local name too,
//! so `l:href`, `xlink:href` and `href` all read as `href`.

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// One node of the tree.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// An element.
    Element(Element),
    /// Text with entities resolved.
    Text(String),
}

/// An element with local names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    /// Local name (`section` for `fb:section`).
    pub name: String,
    /// Prefix of the qualified name, if any (`epub` for `epub:type`-style element names).
    pub prefix: Option<String>,
    /// Attributes: (local name, qualified name, value).
    pub attrs: Vec<Attr>,
    /// Children in order.
    pub children: Vec<Node>,
}

/// One attribute.
#[derive(Clone, Debug, PartialEq)]
pub struct Attr {
    /// Local name (`href` for `l:href`).
    pub local: String,
    /// Qualified name as written (`l:href`).
    pub qname: String,
    /// Value with entities resolved.
    pub value: String,
}

impl Element {
    /// Value of the attribute with this local name.
    pub fn attr(&self, local: &str) -> Option<&str> {
        self.attrs.iter().find(|a| a.local == local).map(|a| a.value.as_str())
    }

    /// Value of the attribute with this exact qualified name (`epub:type`).
    pub fn qattr(&self, qname: &str) -> Option<&str> {
        self.attrs.iter().find(|a| a.qname == qname).map(|a| a.value.as_str())
    }

    /// Child elements.
    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// First child element with this local name.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|e| e.name == name)
    }

    /// Child elements with this local name.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.elements().filter(move |e| e.name == name)
    }

    /// First descendant (depth first, including self) with this local name.
    pub fn find(&self, name: &str) -> Option<&Element> {
        if self.name == name {
            return Some(self);
        }
        self.elements().find_map(|e| e.find(name))
    }

    /// All descendants (depth first, including self) with this local name.
    pub fn find_all<'a>(&'a self, name: &str, out: &mut Vec<&'a Element>) {
        if self.name == name {
            out.push(self);
        }
        for e in self.elements() {
            e.find_all(name, out);
        }
    }

    /// Concatenated text of all descendants.
    pub fn text(&self) -> String {
        let mut s = String::new();
        self.collect_text(&mut s);
        s
    }

    fn collect_text(&self, out: &mut String) {
        for c in &self.children {
            match c {
                Node::Text(t) => out.push_str(t),
                Node::Element(e) => e.collect_text(out),
            }
        }
    }

    /// Text with whitespace runs collapsed to single spaces and trimmed.
    pub fn text_collapsed(&self) -> String {
        collapse_ws(&self.text())
    }
}

/// Collapses whitespace runs to one space and trims.
pub fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// `s` with every tag (`<...>`) replaced by one space; the text is not unescaped.
pub(crate) fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Appends `s` to `out` escaped for XML: `&`, `<` and `>` always, in an attribute value
/// (double-quoted) also `"` and the white space a parser would normalise. Characters XML
/// does not allow are left out. Runs that need no escaping are copied as slices.
fn escape_into(out: &mut String, s: &str, attr: bool) {
    let mut last = 0;
    for (i, c) in s.char_indices() {
        let rep = match c {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' if attr => "&quot;",
            '\n' if attr => "&#10;",
            '\t' if attr => "&#9;",
            '\r' if attr => "&#13;",
            c if is_xml_char(c) => continue,
            _ => "",
        };
        out.push_str(&s[last..i]);
        out.push_str(rep);
        last = i + c.len_utf8();
    }
    out.push_str(&s[last..]);
}

/// Appends `s` escaped as XML text content to `out`.
pub fn push_text(out: &mut String, s: &str) {
    escape_into(out, s, false);
}

/// Appends ` name="value"` to `out`, the value escaped.
pub fn push_attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    escape_into(out, value, true);
    out.push('"');
}

/// Escapes text content for XML.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    push_text(&mut out, s);
    out
}

/// Escapes an attribute value for XML (double-quoted).
pub fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    escape_into(&mut out, s, true);
    out
}

/// True for characters allowed in XML 1.0 documents.
pub fn is_xml_char(c: char) -> bool {
    matches!(c, '\u{9}' | '\u{a}' | '\u{d}' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

/// True when `name` is usable as an XML element or attribute name in our output.
pub fn is_xml_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// Replacement text for HTML named entities that XHTML books use without a DTD.
pub fn html_entity(name: &str) -> Option<&'static str> {
    const LATIN1: [&str; 96] = [
        "nbsp", "iexcl", "cent", "pound", "curren", "yen", "brvbar", "sect", "uml", "copy", "ordf", "laquo", "not",
        "shy", "reg", "macr", "deg", "plusmn", "sup2", "sup3", "acute", "micro", "para", "middot", "cedil", "sup1",
        "ordm", "raquo", "frac14", "frac12", "frac34", "iquest", "Agrave", "Aacute", "Acirc", "Atilde", "Auml",
        "Aring", "AElig", "Ccedil", "Egrave", "Eacute", "Ecirc", "Euml", "Igrave", "Iacute", "Icirc", "Iuml", "ETH",
        "Ntilde", "Ograve", "Oacute", "Ocirc", "Otilde", "Ouml", "times", "Oslash", "Ugrave", "Uacute", "Ucirc",
        "Uuml", "Yacute", "THORN", "szlig", "agrave", "aacute", "acirc", "atilde", "auml", "aring", "aelig", "ccedil",
        "egrave", "eacute", "ecirc", "euml", "igrave", "iacute", "icirc", "iuml", "eth", "ntilde", "ograve", "oacute",
        "ocirc", "otilde", "ouml", "divide", "oslash", "ugrave", "uacute", "ucirc", "uuml", "yacute", "thorn", "yuml",
    ];
    const LATIN1_CHARS: &str = "\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ";
    if let Some(i) = LATIN1.iter().position(|n| *n == name) {
        let (start, c) = LATIN1_CHARS.char_indices().nth(i)?;
        return LATIN1_CHARS.get(start..start + c.len_utf8());
    }
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "lsquo" => "‘",
        "rsquo" => "’",
        "sbquo" => "‚",
        "ldquo" => "“",
        "rdquo" => "”",
        "bdquo" => "„",
        "lsaquo" => "‹",
        "rsaquo" => "›",
        "dagger" => "†",
        "Dagger" => "‡",
        "bull" => "•",
        "prime" => "′",
        "Prime" => "″",
        "permil" => "‰",
        "euro" => "€",
        "trade" => "™",
        "larr" => "←",
        "rarr" => "→",
        "uarr" => "↑",
        "darr" => "↓",
        "harr" => "↔",
        "minus" => "−",
        "infin" => "∞",
        "ne" => "≠",
        "le" => "≤",
        "ge" => "≥",
        "asymp" => "≈",
        "ensp" => "\u{2002}",
        "emsp" => "\u{2003}",
        "thinsp" => "\u{2009}",
        "zwnj" => "\u{200c}",
        "zwj" => "\u{200d}",
        "lrm" => "\u{200e}",
        "rlm" => "\u{200f}",
        "OElig" => "Œ",
        "oelig" => "œ",
        "Scaron" => "Š",
        "scaron" => "š",
        "Yuml" => "Ÿ",
        "fnof" => "ƒ",
        "circ" => "ˆ",
        "tilde" => "˜",
        "alpha" => "α",
        "beta" => "β",
        "gamma" => "γ",
        "delta" => "δ",
        "pi" => "π",
        "sigma" => "σ",
        "omega" => "ω",
        "Omega" => "Ω",
        "loz" => "◊",
        "spades" => "♠",
        "clubs" => "♣",
        "hearts" => "♥",
        "diams" => "♦",
        _ => return None,
    })
}

fn qname_parts(raw: &[u8]) -> (Option<String>, String) {
    let s = String::from_utf8_lossy(raw);
    match s.split_once(':') {
        Some((p, l)) => (Some(p.to_string()), l.to_string()),
        None => (None, s.into_owned()),
    }
}

fn element_of(e: &BytesStart) -> Element {
    let (prefix, name) = qname_parts(e.name().as_ref());
    let mut attrs = Vec::new();
    for a in e.attributes().with_checks(false).flatten() {
        let qname = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let local = String::from_utf8_lossy(a.key.local_name().as_ref()).into_owned();
        let value = match a.unescape_value_with(html_entity) {
            Ok(v) => v.into_owned(),
            Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
        };
        attrs.push(Attr { local, qname, value });
    }
    Element { name, prefix, attrs, children: Vec::new() }
}

/// Deepest element nesting kept by [`parse`] (and accepted by [`is_well_formed`]).
///
/// Elements opened below this depth are left out of the tree (their text and children go
/// to the element at the limit). Every walker over the tree, the tree's `Drop` and the
/// converters that recurse over it are bounded by this depth, so a crafted book with
/// 100 000 nested elements cannot overflow the stack, and the end-tag search in
/// [`parse`] stays linear. Real books nest fewer than 50 levels.
pub const MAX_DEPTH: usize = 256;

/// Parses an XML document into its root element. Unknown entities are resolved through
/// [`html_entity`] or kept as written; mismatched end tags are tolerated (the open
/// element is closed). Nesting is limited to [`MAX_DEPTH`]. Returns an error only when
/// there is no root element.
pub fn parse(xml: &str) -> Result<Element, String> {
    let mut r = Reader::from_str(xml);
    r.config_mut().check_end_names = false;
    let mut stack: Vec<Element> = vec![Element { name: "#document".into(), ..Default::default() }];
    // Start tags left out below MAX_DEPTH; the same number of end tags is skipped.
    let mut skipped = 0usize;
    loop {
        let ev = match r.read_event() {
            Ok(ev) => ev,
            Err(e) => {
                // Keep what was parsed so far.
                if stack.len() > 1 || stack[0].elements().next().is_some() {
                    log::debug!("XML error after partial parse: {e}");
                    break;
                }
                return Err(format!("XML: {e}"));
            }
        };
        match ev {
            Event::Start(e) => {
                // stack[0] is the document node, so the depth is stack.len() - 1.
                if stack.len() > MAX_DEPTH {
                    skipped += 1;
                } else {
                    stack.push(element_of(&e));
                }
            }
            Event::Empty(e) => {
                // A leaf: at most one level below the limit.
                let el = element_of(&e);
                stack.last_mut().expect("document").children.push(Node::Element(el));
            }
            Event::End(e) => {
                if skipped > 0 {
                    skipped -= 1;
                    continue;
                }
                let (_, name) = qname_parts(e.name().as_ref());
                // Close up to the matching open element (tolerates unclosed children).
                // The search is bounded by MAX_DEPTH.
                if let Some(pos) = stack.iter().rposition(|el| el.name == name) {
                    if pos == 0 {
                        continue;
                    }
                    while stack.len() > pos {
                        let el = stack.pop().expect("open element");
                        stack.last_mut().expect("document").children.push(Node::Element(el));
                    }
                }
            }
            Event::Text(t) => {
                let s = match t.unescape_with(html_entity) {
                    Ok(s) => s.into_owned(),
                    Err(_) => String::from_utf8_lossy(&t).into_owned(),
                };
                append_text(stack.last_mut().expect("document"), s);
            }
            Event::CData(t) => {
                let s = String::from_utf8_lossy(&t).into_owned();
                append_text(stack.last_mut().expect("document"), s);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    while stack.len() > 1 {
        let el = stack.pop().expect("open element");
        stack.last_mut().expect("document").children.push(Node::Element(el));
    }
    let doc = stack.pop().expect("document");
    doc.children
        .into_iter()
        .find_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
        .ok_or_else(|| "XML: no root element".to_string())
}

fn append_text(el: &mut Element, s: String) {
    if s.is_empty() {
        return;
    }
    if let Some(Node::Text(last)) = el.children.last_mut() {
        last.push_str(&s);
    } else {
        el.children.push(Node::Text(s));
    }
}

/// Index of the `>` that closes the tag opening at `start` (a `<`), skipping quoted
/// attribute values; `None` when the tag is never closed.
pub(crate) fn tag_end(b: &[u8], start: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (j, &c) in b.iter().enumerate().skip(start + 1) {
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'>' {
            return Some(j);
        }
    }
    None
}

/// True when the markup in `s` opens more than `limit` nested elements. A byte scan
/// (start tags minus end tags; void HTML elements, self-closing tags, comments, CDATA,
/// declarations and processing instructions do not count), fast enough for every
/// chapter of a book.
pub fn nesting_exceeds(s: &str, limit: usize) -> bool {
    let b = s.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while let Some(off) = s.get(i..).and_then(|r| r.find('<')) {
        let p = i + off;
        let rest = &s[p..];
        let skip_to = |end: &str| rest.find(end).map_or(s.len(), |e| p + e + end.len());
        if rest.starts_with("<!--") {
            i = skip_to("-->");
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = skip_to("]]>");
            continue;
        }
        if rest.starts_with("</") {
            depth = depth.saturating_sub(1);
            i = p + 2;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i = p + 2;
            continue;
        }
        let Some(j) = tag_end(b, p) else { break };
        // The name up to whitespace, `/` or `>`, without copying it.
        let name_len = rest[1..].find(|c: char| c.is_whitespace() || matches!(c, '/' | '>')).unwrap_or(rest.len() - 1);
        let local = rest[1..1 + name_len].rsplit(':').next().unwrap_or("");
        if b[j - 1] != b'/' && !crate::xhtml::is_void_ignore_case(local) {
            depth += 1;
            if depth > limit {
                return true;
            }
        }
        i = j + 1;
    }
    false
}

/// True when `xml` parses with matching end tags from start to end and nests no deeper
/// than [`MAX_DEPTH`] (deeper markup goes through the HTML repair, which flattens it).
pub fn is_well_formed(xml: &str) -> bool {
    let mut r = Reader::from_str(xml);
    r.config_mut().check_end_names = true;
    let mut depth = 0usize;
    let mut roots = 0;
    loop {
        match r.read_event() {
            Ok(Event::Start(_)) => {
                if depth == 0 {
                    roots += 1;
                }
                depth += 1;
                if depth > MAX_DEPTH {
                    return false;
                }
            }
            Ok(Event::Empty(_)) => {
                if depth == 0 {
                    roots += 1;
                }
            }
            Ok(Event::End(_)) => depth = depth.saturating_sub(1),
            Ok(Event::Text(t)) => {
                // Unknown entities break the v1 parser too.
                if t.unescape_with(html_entity).is_err() {
                    return false;
                }
            }
            Ok(Event::Eof) => return depth == 0 && roots == 1,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

/// Decodes XML bytes: BOM first, then the `encoding` of the XML declaration, else UTF-8
/// (invalid sequences replaced).
pub fn decode_xml_bytes(bytes: &[u8]) -> String {
    if let Some((enc, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc.decode_without_bom_handling(&bytes[bom..]).0.into_owned();
    }
    if let Some(label) = declared_encoding(bytes) {
        if let Some(enc) = encoding_rs::Encoding::for_label(label.as_bytes()) {
            if enc != encoding_rs::UTF_8 {
                return enc.decode_without_bom_handling(bytes).0.into_owned();
            }
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// The `encoding="..."` value of an XML declaration at the start of `bytes`.
pub fn declared_encoding(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(200)];
    let head = String::from_utf8_lossy(head);
    let decl = head.strip_prefix("<?xml")?;
    let end = decl.find("?>")?;
    let decl = &decl[..end];
    let i = decl.find("encoding")?;
    let rest = decl[i + 8..].trim_start().strip_prefix('=')?.trim_start();
    let q = rest.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let rest = &rest[1..];
    Some(rest[..rest.find(q)?].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_names_attrs_text() {
        let x = r##"<?xml version="1.0"?><fb:root xmlns:fb="x" xmlns:l="y"><a l:href="#n1" id='i'>A &amp; &nbsp;B<br/>C</a><b>tail</b></fb:root>"##;
        let root = parse(x).unwrap();
        assert_eq!(root.name, "root");
        let a = root.child("a").unwrap();
        assert_eq!(a.attr("href"), Some("#n1"));
        assert_eq!(a.qattr("l:href"), Some("#n1"));
        assert_eq!(a.text(), "A & \u{a0}BC");
        assert_eq!(root.text_collapsed(), "A & BCtail");
        assert!(root.find("br").is_some());
    }

    #[test]
    fn tolerates_unclosed_and_mismatched_tags() {
        let root = parse("<r><p>one<i>two</p><p>three</p></r>").unwrap();
        assert_eq!(root.children_named("p").count(), 2);
        assert_eq!(root.text(), "onetwothree");
        assert!(parse("").is_err());
        assert!(is_well_formed("<a><b/>x</a>"));
        assert!(!is_well_formed("<a><b>x</a>"));
        assert!(!is_well_formed("<a>&bogus;</a>"));
        assert!(is_well_formed("<a>&eacute;&#233;</a>"));
    }

    #[test]
    fn depth_limit_and_nesting_scan() {
        // Deeper elements are left out of the tree; their text stays.
        let n = 2000;
        let src = format!("<r>{}deep<img src='x'/>{}<tail/></r>", "<s>".repeat(n), "</s>".repeat(n));
        let root = parse(&src).unwrap();
        assert!(root.text().contains("deep"));
        assert!(root.find("img").is_some() && root.child("tail").is_some());
        let mut depth = 0;
        let mut el = &root;
        while let Some(c) = el.child("s") {
            depth += 1;
            el = c;
        }
        assert_eq!(depth, MAX_DEPTH - 1);
        assert!(!is_well_formed(&src));
        assert!(is_well_formed(&format!("<r>{}x{}</r>", "<s>".repeat(100), "</s>".repeat(100))));
        // The byte scan.
        let deep = format!("<html><body>{}x{}</body></html>", "<div>".repeat(300), "</div>".repeat(300));
        assert!(nesting_exceeds(&deep, MAX_DEPTH));
        let flat = format!("<html><body>{}<!-- {} --><![CDATA[{}]]><p a='>'>x</p></body></html>", "<br><img src='a'/><hr/>".repeat(500), "<div>".repeat(300), "<div>".repeat(300));
        assert!(!nesting_exceeds(&flat, MAX_DEPTH));
        assert!(!nesting_exceeds("<a><b>", 2) && nesting_exceeds("<a><b><c>", 2));
        // Void tags in any case or prefix do not count; long and odd names do.
        assert!(!nesting_exceeds("<a><BR><h:Img src='x'><b>", 2));
        assert!(nesting_exceeds("<a><averyveryverylongelementname><b>", 2) && nesting_exceeds("<a><brr><b>", 2));
        assert!(!nesting_exceeds("<a><br/><b>", 2) && !nesting_exceeds("<a><hr\n class='x'><b>", 2));
    }

    #[test]
    fn escaping_and_entities() {
        assert_eq!(escape_text("a<b&c>\u{1}"), "a&lt;b&amp;c&gt;");
        assert_eq!(escape_attr("\"x\"\n"), "&quot;x&quot;&#10;");
        let mut out = String::from("<p");
        push_attr(&mut out, "title", "a\tb & ü\u{0}");
        out.push('>');
        push_text(&mut out, "x \"y\" <é>");
        assert_eq!(out, "<p title=\"a&#9;b &amp; ü\">x \"y\" &lt;é&gt;");
        assert_eq!(collapse_ws("  a \n\t b  c "), "a b c");
        assert_eq!(collapse_ws(" \n "), "");
        assert_eq!(html_entity("eacute"), Some("é"));
        assert_eq!(html_entity("nbsp"), Some("\u{a0}"));
        assert_eq!(html_entity("yuml"), Some("ÿ"));
        assert_eq!(html_entity("mdash"), Some("—"));
        assert_eq!(html_entity("nope"), None);
        assert!(is_xml_name("mbp:pagebreak") && !is_xml_name("1a") && !is_xml_name("a\"b"));
    }

    #[test]
    fn xml_declaration_encoding() {
        let cp = b"<?xml version=\"1.0\" encoding=\"windows-1251\"?><a>\xcf\xf0\xe8\xe2\xe5\xf2</a>";
        assert_eq!(declared_encoding(cp).as_deref(), Some("windows-1251"));
        assert!(decode_xml_bytes(cp).contains("Привет"));
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("<a>Æ</a>".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(decode_xml_bytes(&utf16), "<a>Æ</a>");
        assert_eq!(declared_encoding(b"<a/>"), None);
    }
}
