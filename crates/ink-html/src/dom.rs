//! The DOM of one spine item, built the way KOReader's crengine builds it, so that
//! xpointers (`/body/DocFragment[N]/body/div/p[3]/text().42`) name the same nodes and
//! character offsets.
//!
//! Rules taken from crengine (`lvxml.cpp`, `lvtinydom.cpp`, DOM version 20240114), and
//! checked against KOReader's own output (`tests/crengine/*.json`):
//! - Text outside `pre`-like elements: `\r`, `\n`, `\t` become spaces and runs of literal
//!   spaces collapse to one, per text node. Entities are decoded in the same pass; an
//!   unknown entity stays literal. Encoded spaces (`&#32;`) do not collapse.
//! - Text in elements whose `white-space` is `pre-line`, `pre`, `pre-wrap` or
//!   `break-spaces` keeps spaces and newlines; tabs expand to the next multiple of 8
//!   columns counted with crengine's off-by-one (the column after a newline is 1).
//! - A text node longer than 8192 source characters is split after the last space
//!   inside each 8192-character window (crengine's `TEXT_SPLIT_SIZE`).
//! - Comments and processing instructions split text nodes.
//! - Elements that do not allow text (`table`, `tr`, `thead`, `tbody`, `tfoot`,
//!   `colgroup`, `col`, `img`, `svg` and most SVG children, ...) drop their text.
//! - A whitespace-only first child of a block element is dropped. A leading newline
//!   right after `<pre>` / `<textarea>` is dropped.
//! - When an element closes: in a block container with both block and inline children,
//!   each run of inline children (text, inline elements, floats) loses its leading
//!   whitespace-only text nodes and its last child if that is whitespace-only; runs of
//!   only whitespace disappear (crengine's autoBoxing). Inline elements holding blocks
//!   lose whitespace-only text next to those blocks.
//! - Element names are local names (`svg:svg` is `svg`), lowercase except in SVG.

use crate::css::SelectorElement;
use crate::style::{Display, StructStyle, StyledElement, WhiteSpace};
use std::collections::{HashMap, HashSet};

/// Index of a node in [`Dom::nodes`].
pub type NodeId = u32;

/// "No node".
pub const NONE: NodeId = u32::MAX;

/// An element.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    /// Local name.
    pub name: String,
    /// Attributes: names lowercase with their prefix (`epub:type`, `xml:lang`), values decoded.
    pub attrs: Vec<(String, String)>,
    /// Structure style used for the DOM rules (book CSS on, as crengine does).
    pub st: StructStyle,
    /// Inside an `<svg>` (its text is not reading text).
    pub in_svg: bool,
    /// How the parent sees this element after it closed.
    pub rend: Rend,
}

impl Element {
    /// Attribute by name (exact, then ASCII case-insensitive).
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .or_else(|| self.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)))
            .map(|(_, v)| v.as_str())
    }

    /// True for an `<svg>` element and everything inside one (its text is not reading
    /// text).
    pub fn in_svg_tree(&self) -> bool {
        self.in_svg || self.name == "svg"
    }

    /// Attribute by local name, ignoring any prefix (`type` finds `epub:type`).
    pub fn attr_local(&self, local: &str) -> Option<&str> {
        self.attr(local).or_else(|| {
            self.attrs
                .iter()
                .find(|(k, _)| k.rsplit_once(':').is_some_and(|(_, l)| l == local))
                .map(|(_, v)| v.as_str())
        })
    }
}

/// crengine's rendering method as far as the DOM rules need it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Rend {
    /// Inline content.
    #[default]
    Inline,
    /// Block-level.
    Block,
    /// `display: none`.
    Invisible,
    /// Floated.
    Float,
}

/// Node payload.
#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    /// Element.
    Element(Box<Element>),
    /// Text (after crengine's whitespace processing).
    Text(String),
}

/// A node.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Parent, [`NONE`] for the body.
    pub parent: NodeId,
    /// Children in order.
    pub children: Vec<NodeId>,
    /// Payload.
    pub kind: NodeKind,
    /// Document order (pre-order index), set by [`Dom::finish`].
    pub order: u32,
}

/// A stylesheet reference from the chapter's `<head>`, in cascade order.
#[derive(Clone, Debug, PartialEq)]
pub enum SheetRef {
    /// `<link rel="stylesheet" href>` (href as written).
    Link(String),
    /// `<style>` contents.
    Inline(String),
}

/// One chapter's DOM.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dom {
    /// All nodes; removed nodes stay allocated but unreachable.
    pub nodes: Vec<Node>,
    /// The `<body>` element ([`NONE`] when the chapter has none).
    pub body: NodeId,
    /// Attributes of `<html>` (`lang`, `xml:lang`, `dir`, `class`, `style`).
    pub html_attrs: Vec<(String, String)>,
    /// Stylesheets from `<head>`.
    pub sheets: Vec<SheetRef>,
    /// Nodes in document order (`order` indexes this).
    pub preorder: Vec<NodeId>,
    /// True when the chapter was parsed by html5ever (it was not well-formed XML).
    pub html5_fallback: bool,
    /// Element by `id` (or `<a name>`), the first in document order. Built by
    /// [`Dom::finish`].
    pub ids: HashMap<String, NodeId>,
    /// Per node (indexed by [`NodeId`]): its 1-based index among the sibling elements
    /// with the same name (text nodes: among the sibling text nodes) and how many such
    /// siblings there are. Built by [`Dom::finish`] for reachable nodes; xpointer steps
    /// read it instead of scanning the siblings.
    pub steps: Vec<(u32, u32)>,
}

impl Dom {
    /// The node.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    /// The element payload, if `id` is an element.
    pub fn element(&self, id: NodeId) -> Option<&Element> {
        match &self.nodes.get(id as usize)?.kind {
            NodeKind::Element(e) => Some(e),
            NodeKind::Text(_) => None,
        }
    }

    /// The text payload, if `id` is a text node.
    pub fn text(&self, id: NodeId) -> Option<&str> {
        match &self.nodes.get(id as usize)?.kind {
            NodeKind::Text(t) => Some(t),
            NodeKind::Element(_) => None,
        }
    }

    /// Element name or `""`.
    pub fn name(&self, id: NodeId) -> &str {
        self.element(id).map(|e| e.name.as_str()).unwrap_or("")
    }

    /// Computes document order, the id index and the sibling step indexes. Call after
    /// the tree is complete.
    pub fn finish(&mut self) {
        self.preorder.clear();
        self.ids.clear();
        self.steps = vec![(1, 1); self.nodes.len()];
        if self.body == NONE {
            return;
        }
        let mut stack = vec![self.body];
        while let Some(n) = stack.pop() {
            let order = self.preorder.len() as u32;
            self.nodes[n as usize].order = order;
            self.preorder.push(n);
            for &c in self.nodes[n as usize].children.iter().rev() {
                stack.push(c);
            }
        }
        for &n in &self.preorder {
            if let NodeKind::Element(e) = &self.nodes[n as usize].kind {
                if let Some(id) = e.attr("id") {
                    self.ids.entry(id.to_string()).or_insert(n);
                }
                if e.name == "a" {
                    if let Some(name) = e.attr("name") {
                        self.ids.entry(name.to_string()).or_insert(n);
                    }
                }
            }
            let kids = &self.nodes[n as usize].children;
            if kids.len() < 2 {
                continue;
            }
            // Index of each child among its kind (`None` = text), then the totals.
            let mut counts: HashMap<Option<&str>, u32> = HashMap::new();
            let mut index = Vec::with_capacity(kids.len());
            for &c in kids {
                let key = match &self.nodes[c as usize].kind {
                    NodeKind::Element(e) => Some(e.name.as_str()),
                    NodeKind::Text(_) => None,
                };
                let k = counts.entry(key).or_insert(0);
                *k += 1;
                index.push((key, *k));
            }
            let kids = kids.clone();
            for (&c, (key, i)) in kids.iter().zip(index) {
                let total = counts[&key];
                self.steps[c as usize] = (i, total);
            }
        }
    }

    /// The nearest of `id` and its ancestors that is not an inline element (at most the
    /// body).
    pub fn block_of(&self, mut id: NodeId) -> NodeId {
        while id != self.body && self.element(id).is_some_and(|e| e.st.display == Display::Inline) {
            id = self.node(id).parent;
        }
        id
    }

    /// Last node (in document order) inside `id`'s subtree, or `id` itself.
    pub fn last_descendant(&self, mut id: NodeId) -> NodeId {
        while let Some(&c) = self.node(id).children.last() {
            id = c;
        }
        id
    }

    /// Concatenated text of a subtree (SVG text excluded).
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            match &self.node(n).kind {
                NodeKind::Text(t) => out.push_str(t),
                NodeKind::Element(e) => {
                    if e.in_svg_tree() {
                        continue;
                    }
                    stack.extend(self.node(n).children.iter().rev());
                }
            }
        }
    }

    /// The element with `id="..."` (or `<a name="...">`), the first in document order.
    pub fn find_id(&self, anchor: &str) -> Option<NodeId> {
        self.ids.get(anchor).copied()
    }

    /// Lang of the chapter (`<html lang>`/`xml:lang`, or `<body>`'s).
    pub fn root_lang(&self) -> Option<&str> {
        self.html_attrs
            .iter()
            .find(|(k, _)| k == "xml:lang" || k == "lang")
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.trim().is_empty())
    }
}

/// A view of an element for selector matching (siblings skip text nodes).
#[derive(Clone, Copy)]
pub struct ElemRef<'a> {
    /// The DOM.
    pub dom: &'a Dom,
    /// The element.
    pub id: NodeId,
}

impl ElemRef<'_> {
    fn el(&self) -> &Element {
        self.dom.element(self.id).expect("element")
    }

    fn sibling(&self, dir: i32) -> Option<Self> {
        let p = self.dom.node(self.id).parent;
        if p == NONE {
            return None;
        }
        let kids = &self.dom.node(p).children;
        let pos = kids.iter().position(|&c| c == self.id)? as i32;
        let mut i = pos + dir;
        while i >= 0 && (i as usize) < kids.len() {
            let c = kids[i as usize];
            if self.dom.element(c).is_some() {
                return Some(ElemRef { dom: self.dom, id: c });
            }
            i += dir;
        }
        None
    }
}

impl SelectorElement for ElemRef<'_> {
    fn local_name(&self) -> &str {
        &self.el().name
    }
    fn attr(&self, name: &str) -> Option<&str> {
        self.el().attr_local(name)
    }
    fn parent(&self) -> Option<Self> {
        let p = self.dom.node(self.id).parent;
        (p != NONE).then_some(ElemRef { dom: self.dom, id: p })
    }
    fn prev_sibling(&self) -> Option<Self> {
        self.sibling(-1)
    }
    fn next_sibling(&self) -> Option<Self> {
        self.sibling(1)
    }
    fn is_root(&self) -> bool {
        self.dom.node(self.id).parent == NONE
    }
}

impl StyledElement for ElemRef<'_> {
    fn lang(&self) -> Option<&str> {
        let e = self.el();
        e.attr("xml:lang").or_else(|| e.attr("lang"))
    }
}

/// Elements whose text crengine drops (`allow_text = false` in `fb2def.h`).
fn allows_text(name: &str, in_svg: bool) -> bool {
    if in_svg {
        return matches!(name, "style" | "text" | "tspan" | "textPath" | "title" | "desc" | "metadata");
    }
    !matches!(
        name,
        "html" | "head" | "table" | "colgroup" | "col" | "thead" | "tbody" | "tfoot" | "tr" | "img" | "svg" | "object"
            | "embed" | "applet" | "area" | "track" | "input" | "keygen" | "param" | "audio" | "source" | "base"
            | "basefont" | "bgsound" | "meta" | "link"
    )
}

/// True for crengine's `IsEmptySpace`: only space, tab, CR, LF.
pub fn is_empty_space(s: &str) -> bool {
    s.chars().all(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
}

/// crengine's `TEXT_SPLIT_SIZE`.
pub const TEXT_SPLIT: usize = 8192;

/// Splits raw source text into the chunks crengine turns into separate text nodes.
pub fn split_raw_text(raw: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = raw;
    loop {
        // Byte offset of the 8192nd char, if there are that many (never in 8192 bytes).
        let cut = if rest.len() > TEXT_SPLIT { rest.char_indices().nth(TEXT_SPLIT).map(|(i, _)| i) } else { None };
        let Some(cut) = cut else {
            if !rest.is_empty() {
                out.push(rest);
            }
            break;
        };
        // After the last space, or the last line break that is not half of a CR LF pair.
        let w = &rest.as_bytes()[..cut];
        let split = (0..cut)
            .rev()
            .find(|&i| match w[i] {
                b' ' => true,
                b'\r' => w.get(i + 1) != Some(&b'\n'),
                b'\n' => w.get(i + 1) != Some(&b'\r'),
                _ => false,
            })
            .map_or(cut, |i| i + 1);
        out.push(&rest[..split]);
        rest = &rest[split..];
    }
    out
}

/// crengine's `codeconvert`: numeric references 128-159 are windows-1252 codes.
const CP1252: [u16; 32] = [
    8364, 129, 8218, 402, 8222, 8230, 8224, 8225, 710, 8240, 352, 8249, 338, 141, 381, 143,
    144, 8216, 8217, 8220, 8221, 8226, 8211, 8212, 732, 8482, 353, 8250, 339, 157, 382, 376,
];

fn codeconvert(code: u32) -> u32 {
    match code {
        128..=159 => CP1252[code as usize - 128] as u32,
        c => c,
    }
}

/// Named entity lookup (the HTML5 table), returning one or two chars.
pub fn named_entity(name: &str) -> Option<(char, Option<char>)> {
    let key = format!("{name};");
    let (a, b) = *html5ever::data::NAMED_ENTITIES.get(key.as_str())?;
    let a = char::from_u32(a)?;
    let b = if b == 0 { None } else { char::from_u32(b) };
    Some((a, b))
}

/// crengine's `PreProcessXmlString`: decodes entities and handles whitespace.
/// `decode` is false for text that is already decoded (html5ever).
///
/// Works on bytes: every char it treats specially is ASCII, and other chars are copied
/// in runs.
pub fn preprocess(raw: &str, pre: bool, decode: bool) -> String {
    let b = raw.as_bytes();
    let mut out = String::with_capacity(raw.len());
    // Spaces in a row so far, and the last byte handled (only compared with `\n`).
    let mut nsp = 0usize;
    let mut last = 0u8;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if pre && c == b'\r' {
            // A lone CR is a line break; CR LF and LF CR give one.
            if (i == 0 || last != b'\n') && b.get(i + 1) != Some(&b'\n') {
                out.push('\n');
                last = b'\n';
            }
            i += 1;
            continue;
        }
        if pre && c == b'\n' {
            out.push('\n');
            last = c;
            i += 1;
            continue;
        }
        let c = if !pre && matches!(c, b'\r' | b'\n' | b'\t') { b' ' } else { c };
        if c == b'&' && decode {
            if let Some((len, term)) = decode_entity(&raw[i..], &mut out) {
                nsp = 0;
                last = term;
                i += len;
                continue;
            }
            // Not an entity: the `&` is text.
        }
        if c == b' ' {
            if pre || nsp == 0 {
                out.push(' ');
            }
            nsp += 1;
            last = c;
            i += 1;
            continue;
        }
        // A run of ordinary chars.
        let start = i;
        i += 1;
        while i < b.len() && !matches!(b[i], b'&' | b'\r' | b'\n' | b'\t' | b' ') {
            i += 1;
        }
        out.push_str(&raw[start..i]);
        nsp = 0;
        last = b[i - 1];
    }
    if pre {
        expand_tabs(&out)
    } else {
        out
    }
}

/// Decodes the entity at the start of `s` (which starts with `&`) into `out`, as
/// crengine does: `&#N;` and `&#xN;` (code 0 or invalid: nothing), or a named entity of
/// at most 31 chars ended by `;` or a space. Returns the bytes used and the last one.
fn decode_entity(s: &str, out: &mut String) -> Option<(usize, u8)> {
    let b = s.as_bytes();
    if b.get(1) == Some(&b'#') {
        let hex = matches!(b.get(2), Some(b'x' | b'X'));
        let start = 2 + hex as usize;
        let radix = if hex { 16 } else { 10 };
        let digits = b[start..].iter().take_while(|&&c| (c as char).is_digit(radix)).count();
        let end = start + digits;
        if digits == 0 || b.get(end) != Some(&b';') {
            return None;
        }
        let code = b[start..end].iter().fold(0u32, |n, &c| n.saturating_mul(radix).saturating_add((c as char).to_digit(radix).unwrap_or(0)));
        if code != 0 {
            out.extend(char::from_u32(codeconvert(code)));
        }
        return Some((end + 1, b';'));
    }
    if !b.get(1).is_some_and(u8::is_ascii_alphabetic) {
        return None;
    }
    let k = b[1..].iter().take(31).take_while(|&&c| c != b';' && c != b' ').count();
    let term = *b.get(1 + k).filter(|&&t| t == b';' || t == b' ')?;
    let (a, second) = named_entity(s.get(1..1 + k)?)?;
    out.push(a);
    out.extend(second);
    Some((2 + k, term))
}

/// crengine's `ExpandTabs` (column counting restarts at 1 after a newline).
fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut x = 0usize;
    for ch in s.chars() {
        if ch == '\r' || ch == '\n' {
            x = 0;
        }
        if ch == '\t' {
            let delta = 8 - (x & 7);
            x += delta;
            for _ in 0..delta {
                out.push(' ');
            }
        } else {
            out.push(ch);
            x += 1;
        }
    }
    out
}

/// Classifies elements while the DOM is built (the book's CSS, publisher styles on).
pub trait Classifier {
    /// Structure style of element `id` (its ancestors and previous siblings exist).
    fn classify(&mut self, dom: &Dom, id: NodeId, parent_ws: WhiteSpace) -> StructStyle;
}

/// Builds a [`Dom`] from parser events, applying crengine's rules.
pub struct Builder<'c> {
    dom: Dom,
    stack: Vec<NodeId>,
    classifier: Option<&'c mut dyn Classifier>,
    in_head: bool,
    in_head_style: Option<String>,
    body_done: bool,
    /// Depth of elements ignored outside the body (head contents, html children).
    skip_depth: usize,
    /// First text of a `<pre>`/`<textarea>` may lose its leading newline.
    strip_newline: Vec<bool>,
}

impl<'c> Builder<'c> {
    /// A builder; `classifier` decides display/white-space (None = element defaults
    /// only). It is first asked at `<body>`, when the head's stylesheets are known.
    pub fn new(classifier: Option<&'c mut dyn Classifier>) -> Self {
        Builder {
            dom: Dom { body: NONE, ..Default::default() },
            stack: Vec::new(),
            classifier,
            in_head: false,
            in_head_style: None,
            body_done: false,
            skip_depth: 0,
            strip_newline: Vec::new(),
        }
    }

    /// Stylesheet references met so far (complete once the body starts).
    pub fn sheets(&self) -> &[SheetRef] {
        &self.dom.sheets
    }

    fn top(&self) -> Option<NodeId> {
        self.stack.last().copied()
    }

    /// An element starts. `name` is the local name; attribute names keep prefixes.
    pub fn start(&mut self, name: &str, attrs: Vec<(String, String)>) {
        if self.body_done {
            self.skip_depth += 1;
            return;
        }
        if self.stack.is_empty() {
            match name {
                "html" => {
                    self.dom.html_attrs = attrs;
                    return;
                }
                "head" => {
                    self.in_head = true;
                    return;
                }
                "body" => {}
                _ if self.in_head || self.skip_depth > 0 => {
                    if name == "link" {
                        let rel = attrs.iter().find(|(k, _)| k == "rel").map(|(_, v)| v.to_ascii_lowercase()).unwrap_or_default();
                        if rel.split_whitespace().any(|r| r == "stylesheet") && !rel.contains("alternate") {
                            if let Some((_, href)) = attrs.iter().find(|(k, _)| k == "href") {
                                self.dom.sheets.push(SheetRef::Link(href.clone()));
                            }
                        }
                    } else if name == "style" {
                        self.in_head_style = Some(String::new());
                    }
                    self.skip_depth += 1;
                    return;
                }
                _ => {
                    // Content before <body>: ignored like crengine's fragment writer does.
                    self.skip_depth += 1;
                    return;
                }
            }
        }
        if name == "body" && self.stack.is_empty() {
            self.in_head = false;
        }
        let parent = self.top().unwrap_or(NONE);
        let in_svg = parent != NONE && self.dom.element(parent).is_some_and(Element::in_svg_tree);
        let name = if in_svg { name.to_string() } else { name.to_ascii_lowercase() };
        let id = self.dom.nodes.len() as NodeId;
        self.dom.nodes.push(Node {
            parent,
            children: Vec::new(),
            kind: NodeKind::Element(Box::new(Element { name: name.clone(), attrs, st: StructStyle::default(), in_svg, rend: Rend::Inline })),
            order: 0,
        });
        if parent != NONE {
            self.dom.nodes[parent as usize].children.push(id);
        } else {
            self.dom.body = id;
        }
        let parent_ws = if parent == NONE {
            WhiteSpace::Normal
        } else {
            self.dom.element(parent).map(|e| e.st.white_space).unwrap_or_default()
        };
        let mut st = match self.classifier.as_deref_mut() {
            Some(c) => c.classify(&self.dom, id, parent_ws),
            None => default_struct(&name, parent_ws),
        };
        // `body` is a block whatever the CSS says.
        if name == "body" {
            st.display = Display::Block;
        }
        if in_svg || name == "svg" {
            st.display = if name == "svg" { st.display } else { Display::Inline };
            st.white_space = if allows_text(&name, true) && in_svg { WhiteSpace::Pre } else { st.white_space };
        }
        if let NodeKind::Element(e) = &mut self.dom.nodes[id as usize].kind {
            e.st = st;
        }
        self.stack.push(id);
        self.strip_newline.push(matches!(name.as_str(), "pre" | "textarea"));
    }

    /// An element ends. Unmatched end tags are ignored; a match closes everything
    /// opened after it (crengine's behaviour).
    pub fn end(&mut self, name: &str) {
        if self.skip_depth > 0 {
            self.skip_depth -= 1;
            if self.skip_depth == 0 || name == "style" {
                if let Some(css) = self.in_head_style.take() {
                    if name == "style" {
                        self.dom.sheets.push(SheetRef::Inline(css));
                    } else {
                        self.in_head_style = Some(css);
                    }
                }
            }
            return;
        }
        if self.stack.is_empty() {
            if name == "head" {
                self.in_head = false;
            }
            return;
        }
        // HTML element names are stored lowercased: an ASCII-case-insensitive compare is
        // the compare with the lowercased end tag, without allocating it.
        let Some(pos) = self.stack.iter().rposition(|&n| {
            let Some(e) = self.dom.element(n) else { self.bad_stack_entry(n) };
            e.name == name || (!e.in_svg && e.name.eq_ignore_ascii_case(name))
        }) else {
            return;
        };
        while self.stack.len() > pos {
            let n = self.stack.pop().unwrap();
            self.strip_newline.pop();
            self.close(n);
        }
        if self.stack.is_empty() {
            self.body_done = true;
        }
    }

    /// Source text (raw: entities not decoded when `raw` is true).
    pub fn text(&mut self, src: &str, raw: bool) {
        if let Some(css) = self.in_head_style.as_mut() {
            css.push_str(src);
            return;
        }
        if self.skip_depth > 0 {
            return;
        }
        let Some(parent) = self.top() else { return };
        let (pre, allow, is_block) = {
            let Some(e) = self.dom.element(parent) else { self.bad_stack_entry(parent) };
            (e.st.white_space.parse_as_pre(), allows_text(&e.name, e.in_svg), e.st.display.is_block_like())
        };
        if !allow {
            return;
        }
        for chunk in split_raw_text(src) {
            let mut text = preprocess(chunk, pre, raw);
            let first = self.dom.node(parent).children.is_empty();
            if is_block && first && !pre && is_empty_space(&text) {
                continue;
            }
            if let Some(strip) = self.strip_newline.last_mut() {
                if *strip {
                    if first && text.starts_with('\n') {
                        text.remove(0);
                    }
                    *strip = false;
                }
            }
            if text.is_empty() {
                continue;
            }
            let id = self.dom.nodes.len() as NodeId;
            self.dom.nodes.push(Node { parent, children: Vec::new(), kind: NodeKind::Text(text), order: 0 });
            self.dom.nodes[parent as usize].children.push(id);
        }
    }

    /// A comment or processing instruction (only splits text nodes).
    pub fn comment(&mut self) {}

    /// Panics with the builder state when the open-element stack holds an id that is not
    /// an element. Only `start` pushes, and it pushes elements, so this means corrupted
    /// memory or a builder bug; the message carries the evidence.
    #[cold]
    fn bad_stack_entry(&self, n: NodeId) -> ! {
        let i = self.stack.iter().rposition(|&x| x == n).unwrap_or(usize::MAX);
        let lo = i.min(self.stack.len()).saturating_sub(8);
        let hi = i.saturating_add(9).min(self.stack.len());
        let around: Vec<String> = self.stack[lo..hi].iter().map(|x| format!("{x:#010x}")).collect();
        let what = match self.dom.nodes.get(n as usize) {
            None => "out of range".to_string(),
            Some(node) => match &node.kind {
                NodeKind::Text(t) => format!("a text node, parent {:#x}, {} bytes", node.parent, t.len()),
                NodeKind::Element(e) => format!("element {}", e.name),
            },
        };
        panic!(
            "builder: open-element stack entry {n:#010x} at index {i} of {} is not an element ({what}); {} nodes; stack[{lo}..{hi}] = [{}]",
            self.stack.len(),
            self.dom.nodes.len(),
            around.join(" ")
        );
    }

    fn is_ws_text(&self, id: NodeId) -> bool {
        self.dom.text(id).is_some_and(is_empty_space)
    }

    fn child_rend(&self, id: NodeId) -> Option<Rend> {
        self.dom.element(id).map(|e| e.rend)
    }

    /// crengine's `initNodeRendMethod` as far as it changes the DOM.
    fn close(&mut self, id: NodeId) {
        let (display, ws, float, name) = {
            let e = self.dom.element(id).unwrap();
            (e.st.display, e.st.white_space, e.st.float, e.name.clone())
        };
        let kids = self.dom.node(id).children.clone();
        let rend = if display == Display::None {
            Rend::Invisible
        } else if float && id != self.dom.body {
            self.cleanup_block(id, ws);
            Rend::Float
        } else if matches!(name.as_str(), "img" | "image") {
            if display == Display::Inline || display == Display::InlineBlock {
                Rend::Inline
            } else {
                Rend::Block
            }
        } else if display == Display::Inline {
            let has_block = kids.iter().any(|&c| self.child_rend(c) == Some(Rend::Block));
            let has_inline = kids.iter().any(|&c| self.child_rend(c) == Some(Rend::Inline));
            if has_block {
                let has_text = kids.iter().any(|&c| self.dom.text(c).is_some_and(|t| !is_empty_space(t)));
                if !has_inline && !has_text {
                    Rend::Block
                } else {
                    // Blocks inside an inline get wrapped; drop whitespace next to them
                    // (one pass: a whitespace text is dropped when a neighbour is a block).
                    let block: Vec<bool> = kids.iter().map(|&c| self.child_rend(c) == Some(Rend::Block)).collect();
                    let keep: Vec<NodeId> = kids
                        .iter()
                        .enumerate()
                        .filter(|&(i, &c)| !(self.is_ws_text(c) && (block.get(i + 1) == Some(&true) || i > 0 && block[i - 1])))
                        .map(|(_, &c)| c)
                        .collect();
                    self.dom.nodes[id as usize].children = keep;
                    Rend::Inline
                }
            } else {
                Rend::Inline
            }
        } else if display == Display::InlineBlock {
            self.cleanup_block(id, ws);
            Rend::Inline
        } else if display == Display::Table {
            Rend::Block
        } else {
            self.cleanup_block(id, ws);
            Rend::Block
        };
        if let NodeKind::Element(e) = &mut self.dom.nodes[id as usize].kind {
            e.rend = rend;
        }
    }

    /// Block container cleanup (autoBoxing of inline runs).
    fn cleanup_block(&mut self, id: NodeId, ws: WhiteSpace) {
        let kids = self.dom.node(id).children.clone();
        let is_inlineish = |b: &Self, c: NodeId| b.dom.text(c).is_some() || matches!(b.child_rend(c), Some(Rend::Inline | Rend::Float));
        let has_block = kids.iter().any(|&c| self.child_rend(c) == Some(Rend::Block));
        let has_inline = kids.iter().any(|&c| self.dom.text(c).is_some() || self.child_rend(c) == Some(Rend::Inline));
        let has_float = kids.iter().any(|&c| self.child_rend(c) == Some(Rend::Float));
        let pre_line_or_more = ws.parse_as_pre();
        if has_block && has_inline {
            let mut remove: HashSet<NodeId> = HashSet::new();
            let mut i = kids.len() as i64 - 1;
            while i >= 0 {
                if is_inlineish(self, kids[i as usize]) {
                    let mut j = i - 1;
                    while j >= 0 && is_inlineish(self, kids[j as usize]) {
                        j -= 1;
                    }
                    let j = j + 1;
                    if j > 0 || (i as usize) < kids.len() - 1 {
                        self.autobox(&mut remove, &kids[j as usize..=i as usize], pre_line_or_more);
                    }
                    i = j - 1;
                } else {
                    i -= 1;
                }
            }
            // One pass over the children (removing per inline run was quadratic).
            let mut new_kids = kids;
            if !remove.is_empty() {
                new_kids.retain(|k| !remove.contains(k));
            }
            self.dom.nodes[id as usize].children = new_kids;
        } else if !has_block && has_inline && has_float && !matches!(ws, WhiteSpace::Pre | WhiteSpace::PreWrap) {
            // cleanIfOnlyEmptyTextInline
            let only_empty = kids.iter().all(|&c| self.is_ws_text(c) || self.child_rend(c) == Some(Rend::Float));
            if only_empty {
                let keep: Vec<NodeId> = kids.iter().copied().filter(|&c| self.dom.text(c).is_none()).collect();
                self.dom.nodes[id as usize].children = keep;
            }
        }
    }

    /// Whitespace at the edges of an inline run (and whole whitespace runs) to remove:
    /// added to `remove`.
    fn autobox(&self, remove: &mut HashSet<NodeId>, run: &[NodeId], pre: bool) {
        if pre {
            return;
        }
        let mut first = 0usize;
        while first < run.len() && self.is_ws_text(run[first]) {
            first += 1;
        }
        let mut last = run.len() as i64 - 1;
        // crengine checks only the last node (`lastNonEmpty >= endIndex`).
        if last >= 0 && self.is_ws_text(run[last as usize]) {
            last -= 1;
        }
        let mut has_non_empty_inline = false;
        let mut has_float = false;
        if (first as i64) <= last {
            for &n in &run[first..=last as usize] {
                if let Some(t) = self.dom.text(n) {
                    if !is_empty_space(t) {
                        has_non_empty_inline = true;
                    }
                } else if self.child_rend(n) == Some(Rend::Float) {
                    has_float = true;
                } else {
                    has_non_empty_inline = true;
                }
            }
        }
        if has_float && !has_non_empty_inline {
            remove.extend(run.iter().copied().filter(|&n| self.child_rend(n) != Some(Rend::Float)));
        } else if has_non_empty_inline || has_float {
            remove.extend(run[..first].iter().copied());
            if ((last + 1) as usize) < run.len() {
                remove.extend(run[(last + 1) as usize..].iter().copied());
            }
        } else {
            remove.extend(run.iter().copied());
        }
    }

    /// Closes everything still open and returns the DOM.
    pub fn finish(mut self) -> Dom {
        while let Some(n) = self.stack.pop() {
            self.close(n);
        }
        if self.dom.body == NONE {
            // No <body>: an empty one, so every spine item has a DocFragment body.
            self.dom.nodes.push(Node {
                parent: NONE,
                children: Vec::new(),
                kind: NodeKind::Element(Box::new(Element {
                    name: "body".into(),
                    attrs: Vec::new(),
                    st: StructStyle { display: Display::Block, ..Default::default() },
                    in_svg: false,
                    rend: Rend::Block,
                })),
                order: 0,
            });
            self.dom.body = self.dom.nodes.len() as NodeId - 1;
        }
        self.dom.finish();
        self.dom
    }
}

/// Element defaults when no stylesheet is available.
pub fn default_struct(name: &str, parent_ws: WhiteSpace) -> StructStyle {
    let display = match name {
        "head" | "link" | "meta" | "script" | "style" | "title" | "template" => Display::None,
        "li" => Display::ListItem,
        "table" => Display::Table,
        "tr" => Display::TableRow,
        "td" | "th" => Display::TableCell,
        "thead" | "tbody" | "tfoot" => Display::TableRowGroup,
        "caption" => Display::TableCaption,
        "html" | "body" | "div" | "p" | "address" | "article" | "aside" | "blockquote" | "center" | "dd" | "dl" | "dt"
        | "figcaption" | "figure" | "footer" | "form" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "header" | "hgroup"
        | "hr" | "main" | "nav" | "ol" | "pre" | "section" | "ul" => Display::Block,
        _ => Display::Inline,
    };
    let white_space = if matches!(name, "pre" | "xmp" | "listing" | "textarea" | "plaintext") { WhiteSpace::Pre } else { parent_ws };
    StructStyle { display, white_space, float: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_and_entities_like_crengine() {
        assert_eq!(preprocess("Plain    text\n   x\t", false, true), "Plain text x ");
        assert_eq!(preprocess("Soft&shy;h &amp; &#169; &#x2014; &nbsp;n &bogus; &#32;&#32;x", false, true), "Soft\u{ad}h & © — \u{a0}n &bogus; \u{20}\u{20}x");
        assert_eq!(preprocess("&nbsp x", false, true), "\u{a0}x", "space-terminated entity eats the space");
        assert_eq!(preprocess("&#150;", false, true), "\u{2013}", "windows-1252 range");
        assert_eq!(preprocess("&#127;&#128;&#129;&#141;&#156;&#159;&#160;", false, true), "\u{7f}€\u{81}\u{8d}œŸ\u{a0}", "its ends and holes");
        assert_eq!(preprocess("a\r\nb\rc", true, true), "a\nb\nc");
        // crengine's tab quirk: the column after a newline is 1.
        assert_eq!(preprocess("x\n\ty", true, true), "x\n       y");
        assert_eq!(preprocess("\ty", true, true), "        y");
        assert_eq!(preprocess("a &amp; b", false, false), "a &amp; b");
        // Code 0 gives nothing, unterminated or empty references stay text, a name of
        // more than 31 chars is not looked up; a CR right after a line break is dropped.
        assert_eq!(preprocess("a&#0;b &#12 &#x; &; &nbspx", false, true), "ab &#12 &#x; &; &nbspx");
        assert_eq!(preprocess("&abcdefghijklmnopqrstuvwxyzabcdef; é&amp;\r\rz", true, true), "&abcdefghijklmnopqrstuvwxyzabcdef; é&\nz");
    }

    #[test]
    fn long_text_splits_like_crengine() {
        let s: String = (0..3000).map(|i| format!("w{i} ")).collect();
        let parts = split_raw_text(&s);
        assert!(parts.len() >= 2);
        assert_eq!(parts.concat(), s);
        assert!(parts[0].chars().count() <= TEXT_SPLIT && parts[0].ends_with(' '));
        let no_space = "x".repeat(9000);
        let p = split_raw_text(&no_space);
        assert_eq!(p[0].chars().count(), TEXT_SPLIT);
        assert_eq!(split_raw_text("short"), vec!["short"]);
        assert!(split_raw_text("").is_empty());
    }

    fn build(events: &[(&str, &str)]) -> Dom {
        let mut b = Builder::new(None);
        for (kind, v) in events {
            match *kind {
                "s" => b.start(v, Vec::new()),
                "e" => b.end(v),
                "t" => b.text(v, true),
                _ => b.comment(),
            }
        }
        b.finish()
    }

    fn texts(d: &Dom, id: NodeId) -> Vec<String> {
        d.node(id).children.iter().filter_map(|&c| d.text(c).map(String::from)).collect()
    }

    #[test]
    fn builder_rules() {
        let d = build(&[
            ("s", "html"),
            ("s", "head"),
            ("s", "title"),
            ("t", "T"),
            ("e", "title"),
            ("e", "head"),
            ("s", "body"),
            ("t", "\n  "),
            ("s", "div"),
            ("t", "\n "),
            ("s", "p"),
            ("t", "A"),
            ("e", "p"),
            ("t", "\n loose \n"),
            ("s", "p"),
            ("t", "B"),
            ("e", "p"),
            ("t", "\n "),
            ("s", "span"),
            ("t", "x"),
            ("e", "span"),
            ("t", " tail\n"),
            ("e", "div"),
            ("s", "p"),
            ("t", "a"),
            ("c", ""),
            ("t", "b"),
            ("s", "br"),
            ("e", "br"),
            ("t", "\n c"),
            ("e", "p"),
            ("s", "table"),
            ("t", "\n"),
            ("s", "tr"),
            ("t", " "),
            ("s", "td"),
            ("t", " cell "),
            ("e", "td"),
            ("e", "tr"),
            ("e", "table"),
            ("s", "pre"),
            ("t", "\n  x"),
            ("e", "pre"),
            ("e", "body"),
            ("e", "html"),
        ]);
        let body = d.body;
        assert_eq!(d.name(body), "body");
        assert!(d.sheets.is_empty());
        let div = d.node(body).children[0];
        // " loose " kept between blocks; whitespace before the span dropped; " tail " kept.
        assert_eq!(texts(&d, div), vec![" loose ".to_string(), " tail ".to_string()]);
        let p = d.node(body).children[1];
        assert_eq!(texts(&d, p), vec!["a", "b", " c"]);
        let table = d.node(body).children[2];
        let tr = d.node(table).children[0];
        assert!(texts(&d, table).is_empty() && texts(&d, tr).is_empty());
        let td = d.node(tr).children[0];
        assert_eq!(texts(&d, td), vec![" cell "]);
        let pre = d.node(body).children[3];
        assert_eq!(texts(&d, pre), vec!["  x"]);
        assert_eq!(d.preorder.len(), d.preorder.iter().collect::<std::collections::HashSet<_>>().len());
        assert_eq!(d.node(d.preorder[1]).order, 1);
    }

    #[test]
    fn unmatched_end_tags_and_missing_body() {
        let d = build(&[("s", "body"), ("s", "p"), ("t", "x"), ("e", "div"), ("s", "b"), ("t", "y"), ("e", "p"), ("t", "z")]);
        let p = d.node(d.body).children[0];
        assert_eq!(d.name(p), "p");
        assert_eq!(d.node(p).children.len(), 2, "</div> ignored, </p> closes <b> too");
        let d = build(&[("s", "html"), ("e", "html")]);
        assert_eq!(d.name(d.body), "body");
        assert!(d.node(d.body).children.is_empty());
    }
}
