//! Broken HTML to well-formed XHTML: parse with `html5ever` (the HTML5 algorithm repairs
//! unclosed and misnested tags), then serialise every element with an end tag and every
//! void element self-closed. The phase 0 engine parses with an XML reader and stops at
//! the first error, so old MOBI markup and arbitrary `.html` files go through here.
//!
//! A [`Rewrite`] sees every element before it is written and can rename it, change its
//! attributes, drop it or unwrap it (keep the children only).

use crate::xml::{is_xml_name, push_attr, push_text, tag_end};
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, Tracer, TreeBuilder, TreeBuilderOpts, TreeSink};
use html5ever::{Attribute, QualName, TokenizerResult};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::borrow::Cow;
use std::cell::Cell;

/// What to do with an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Write it (with possibly changed name and attributes).
    Keep,
    /// Leave it and its children out.
    Drop,
    /// Leave the tags out, keep the children.
    Unwrap,
}

/// Element hook used while serialising.
pub trait Rewrite {
    /// Called for each element; `name` is the lowercase local name, `attrs` are
    /// `(name, value)` pairs.
    fn element(&mut self, _node: &Handle, _name: &mut String, _attrs: &mut Vec<(String, String)>) -> Action {
        Action::Keep
    }
}

/// Keeps everything except scripts.
pub struct NoRewrite;
impl Rewrite for NoRewrite {}

const VOID: &[&str] =
    &["area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"];

/// True for HTML void elements (written as `<x/>`).
pub fn is_void(name: &str) -> bool {
    VOID.contains(&name)
}

/// [`is_void`] for a local name in any letter case (`BR`, `Img`).
pub(crate) fn is_void_ignore_case(name: &str) -> bool {
    VOID.iter().any(|v| v.eq_ignore_ascii_case(name))
}

/// Most handles the HTML tree builder may hold (open elements plus active formatting
/// elements) before further start tags are left out.
///
/// html5ever has no depth limit of its own: each start tag scans the stack of open
/// elements (for an open `p` in button scope), so 100 000 nested `<div>` take minutes,
/// and the resulting tree is that deep. Past this limit a start tag is dropped (its text
/// and children stay, one level up; its end tag then has no open element and is
/// ignored by the HTML algorithm). The count is taken from the tree builder itself, so
/// sloppy markup with unclosed `p`, `li` or `font` does not add up. Void elements and
/// elements that switch the tokenizer state (`script`, `style`, `title`, ...) always
/// pass. Real books stay below 50.
pub const MAX_OPEN: usize = 256;

/// Start tags that are never left out: void elements (no depth), the document
/// structure and elements that switch the tokenizer to raw text.
fn always_kept(name: &str) -> bool {
    is_void(name)
        || matches!(
            name,
            "html" | "head" | "body" | "script" | "style" | "title" | "textarea" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" | "plaintext"
        )
}

/// Counts the handles the tree builder holds.
struct Count(Cell<usize>);

impl Tracer for Count {
    type Handle = Handle;
    fn trace_handle(&self, _node: &Handle) {
        self.0.set(self.0.get() + 1);
    }
}

/// Most elements one HTML document may create (about 150 MB of DOM).
pub const MAX_ELEMENTS: usize = 1_000_000;

/// Elements the tree builder may create per start tag in the input (plus a base of
/// 100 000). The HTML5 algorithm re-opens active formatting elements (reconstruction,
/// adoption agency), so it creates more elements than there are tags; sloppy real
/// markup stays below 3 per tag, while `<div><b id=N>x</div>` repeated re-opens up to
/// [`MAX_OPEN`] elements for every `x` (a 1.5 MB file became 125 MB of XHTML).
pub const ELEMENTS_PER_TAG: usize = 8;

/// [`RcDom`] that counts the elements it creates.
struct Counting {
    dom: RcDom,
    created: Cell<usize>,
}

/// `TreeSink` methods that go straight to the inner [`RcDom`].
macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
        $(fn $name(&self, $($arg: $ty),*) $(-> $ret)? {
            self.dom.$name($($arg),*)
        })*
    };
}

impl TreeSink for Counting {
    type Handle = Handle;
    type Output = RcDom;
    type ElemName<'a>
        = <RcDom as TreeSink>::ElemName<'a>
    where
        Self: 'a;

    fn finish(self) -> RcDom {
        self.dom
    }
    fn elem_name<'a>(&'a self, target: &'a Handle) -> Self::ElemName<'a> {
        self.dom.elem_name(target)
    }
    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Handle {
        self.created.set(self.created.get() + 1);
        self.dom.create_element(name, attrs, flags)
    }
    forward! {
        parse_error(msg: Cow<'static, str>);
        get_document() -> Handle;
        create_comment(text: StrTendril) -> Handle;
        create_pi(target: StrTendril, data: StrTendril) -> Handle;
        append(parent: &Handle, child: NodeOrText<Handle>);
        append_based_on_parent_node(element: &Handle, prev_element: &Handle, child: NodeOrText<Handle>);
        append_doctype_to_document(name: StrTendril, public_id: StrTendril, system_id: StrTendril);
        mark_script_already_started(node: &Handle);
        pop(node: &Handle);
        get_template_contents(target: &Handle) -> Handle;
        same_node(x: &Handle, y: &Handle) -> bool;
        set_quirks_mode(mode: QuirksMode);
        append_before_sibling(sibling: &Handle, new_node: NodeOrText<Handle>);
        add_attrs_if_missing(target: &Handle, attrs: Vec<Attribute>);
        associate_with_form(target: &Handle, form: &Handle, nodes: (&Handle, Option<&Handle>));
        remove_from_parent(target: &Handle);
        reparent_children(node: &Handle, new_parent: &Handle);
        is_mathml_annotation_xml_integration_point(handle: &Handle) -> bool;
        set_current_line(line_number: u64);
        allow_declarative_shadow_roots(intended_parent: &Handle) -> bool;
        attach_declarative_shadow(location: &Handle, template: &Handle, attrs: &[Attribute]) -> bool;
        maybe_clone_an_option_into_selectedcontent(option: &Handle);
    }
}

/// The tree builder behind a filter that drops start tags past [`MAX_OPEN`] and stops
/// building past the element budget.
struct Limited {
    tb: TreeBuilder<Handle, Counting>,
    /// Start tags left out (nesting).
    skipped: Cell<usize>,
    /// Start tags passed to the tree builder.
    tags: Cell<usize>,
    /// Set when the element budget is spent: the rest of the document is left out.
    stopped: Cell<bool>,
}

impl TokenSink for Limited {
    type Handle = Handle;

    fn process_token(&self, token: Token, line_number: u64) -> TokenSinkResult<Handle> {
        if !self.stopped.get() {
            let created = self.tb.sink.created.get();
            if created > MAX_ELEMENTS || created > self.tags.get().saturating_mul(ELEMENTS_PER_TAG).saturating_add(100_000) {
                log::warn!("HTML builds {created} elements from {} tags: the rest of the document is left out", self.tags.get());
                self.stopped.set(true);
            }
        }
        if self.stopped.get() && !matches!(token, Token::EOFToken) {
            return TokenSinkResult::Continue;
        }
        if let Token::TagToken(Tag { kind: TagKind::StartTag, name, .. }) = &token {
            if !always_kept(name) {
                let held = Count(Cell::new(0));
                self.tb.trace_handles(&held);
                if held.0.get() > MAX_OPEN {
                    self.skipped.set(self.skipped.get() + 1);
                    return TokenSinkResult::Continue;
                }
            }
            self.tags.set(self.tags.get() + 1);
        }
        self.tb.process_token(token, line_number)
    }

    fn end(&self) {
        self.tb.end();
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.tb.adjusted_current_node_present_but_not_in_html_namespace()
    }
}

/// Parses HTML into a DOM (the HTML5 algorithm, nesting limited to about
/// [`MAX_OPEN`] levels, element count to [`MAX_ELEMENTS`] and [`ELEMENTS_PER_TAG`]).
pub fn parse(html: &str) -> RcDom {
    parse_counted(html).0
}

/// [`parse`], also returning the number of start tags left out.
pub(crate) fn parse_counted(html: &str) -> (RcDom, usize) {
    let sink = Counting { dom: RcDom::default(), created: Cell::new(0) };
    let tb = TreeBuilder::new(sink, TreeBuilderOpts::default());
    let limited = Limited { tb, skipped: Cell::new(0), tags: Cell::new(0), stopped: Cell::new(false) };
    let tok = Tokenizer::new(limited, TokenizerOpts::default());
    let input = BufferQueue::default();
    input.push_back(StrTendril::from_slice(html));
    while !matches!(tok.feed(&input), TokenizerResult::Done) {}
    tok.end();
    let skipped = tok.sink.skipped.get();
    if skipped > 0 {
        log::warn!("HTML nested deeper than {MAX_OPEN} elements: {skipped} start tags left out");
    }
    (tok.sink.tb.sink.dom, skipped)
}

/// Concatenated text below `node`, stopping after about `max` bytes (titles and
/// headings; a heading may hold a whole broken document).
pub fn text_of_max(node: &Handle, max: usize) -> String {
    let mut s = String::new();
    let mut stack = vec![node.clone()];
    while let Some(n) = stack.pop() {
        if s.len() >= max {
            break;
        }
        match &n.data {
            NodeData::Text { contents } => s.push_str(&contents.borrow()),
            _ => stack.extend(n.children.borrow().iter().rev().cloned()),
        }
    }
    s
}

fn attr_name(a: &html5ever::Attribute) -> Option<String> {
    let local = a.name.local.to_string();
    let name = match a.name.prefix.as_ref().map(|p| p.to_string()) {
        Some(p) if p == "xlink" || p == "xml" => format!("{p}:{local}"),
        Some(_) | None => match local.split_once(':') {
            Some(("epub", l)) => format!("epub:{l}"),
            Some(("xml", l)) => format!("xml:{l}"),
            Some((p, l)) => format!("{p}-{l}"),
            None => local,
        },
    };
    is_xml_name(&name).then_some(name)
}

fn element_name(raw: &str) -> String {
    let n = raw.to_string().replace(':', "-");
    if is_xml_name(&n) {
        n
    } else {
        "span".into()
    }
}

struct Writer<'a> {
    out: String,
    rw: &'a mut dyn Rewrite,
}

/// Serialiser work items (an explicit stack instead of recursion).
enum Work {
    /// Write a node; `true` for children of the document.
    Node(Handle, bool),
    /// Write an end tag.
    Close(String),
}

impl Writer<'_> {
    /// Writes `start` and everything below it, in document order.
    fn write(&mut self, start: &Handle) {
        let mut stack = vec![Work::Node(start.clone(), true)];
        while let Some(w) = stack.pop() {
            match w {
                Work::Close(n) => {
                    self.out.push_str("</");
                    self.out.push_str(&n);
                    self.out.push('>');
                }
                Work::Node(node, root) => self.node(&node, root, &mut stack),
            }
        }
    }

    /// Writes one node; its children (and end tag) go on `stack`.
    fn node(&mut self, node: &Handle, root: bool, stack: &mut Vec<Work>) {
        let push_children = |stack: &mut Vec<Work>, root: bool| {
            stack.extend(node.children.borrow().iter().rev().map(|c| Work::Node(c.clone(), root)));
        };
        match &node.data {
            NodeData::Document => push_children(stack, true),
            NodeData::Text { contents } => push_text(&mut self.out, &contents.borrow()),
            NodeData::Element { name, attrs, .. } => {
                let mut n = name.local.to_string();
                if n == "script" || n == "noscript" || n == "template" {
                    return;
                }
                let mut a: Vec<(String, String)> =
                    attrs.borrow().iter().filter_map(|a| Some((attr_name(a)?, a.value.to_string()))).collect();
                match self.rw.element(node, &mut n, &mut a) {
                    Action::Drop => return,
                    Action::Unwrap => {
                        push_children(stack, false);
                        return;
                    }
                    Action::Keep => {}
                }
                let n = element_name(&n);
                self.out.push('<');
                self.out.push_str(&n);
                if root && n == "html" {
                    a.retain(|(k, _)| !k.starts_with("xmlns"));
                    self.out.push_str(
                        " xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xmlns:xlink=\"http://www.w3.org/1999/xlink\"",
                    );
                } else if n == "svg" {
                    a.retain(|(k, _)| k != "xmlns");
                    self.out.push_str(" xmlns=\"http://www.w3.org/2000/svg\"");
                } else if n == "math" {
                    a.retain(|(k, _)| k != "xmlns");
                    self.out.push_str(" xmlns=\"http://www.w3.org/1998/Math/MathML\"");
                } else {
                    a.retain(|(k, _)| !k.starts_with("xmlns"));
                }
                // A repeated name keeps its first value. Elements have a few attributes:
                // the earlier ones are scanned, a set is built only for long lists.
                let mut seen = std::collections::HashSet::new();
                for (i, (k, v)) in a.iter().enumerate() {
                    let repeated = if a.len() <= 8 { a[..i].iter().any(|(p, _)| p == k) } else { !seen.insert(k.as_str()) };
                    if !repeated {
                        push_attr(&mut self.out, k, v);
                    }
                }
                if node.children.borrow().is_empty() && is_void(&n) {
                    self.out.push_str("/>");
                    return;
                }
                self.out.push('>');
                let void = is_void(&n);
                stack.push(Work::Close(n));
                if !void {
                    push_children(stack, false);
                }
            }
            NodeData::Doctype { .. } | NodeData::Comment { .. } | NodeData::ProcessingInstruction { .. } => {}
        }
    }
}

/// Serialises a parsed DOM as XHTML.
pub fn serialize(dom: &RcDom, rw: &mut dyn Rewrite) -> String {
    let mut w = Writer { out: String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"), rw };
    w.write(&dom.document);
    w.out
}

/// Expands XML self-closing tags of non-void elements (`<a id="x"/>` to
/// `<a id="x"></a>`), so the HTML parser does not leave them open.
pub fn expand_self_closing(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() + 64);
    let bytes = xml.as_bytes();
    let mut i = 0;
    let mut last = 0;
    while let Some(off) = xml[i..].find('<') {
        let start = i + off;
        // Skip comments, CDATA, declarations and processing instructions.
        let rest = &xml[start..];
        if rest.starts_with("<!--") {
            i = start + rest.find("-->").map(|e| e + 3).unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") || rest.starts_with("</") {
            i = start + 1;
            continue;
        }
        let Some(j) = tag_end(bytes, start) else { break };
        if bytes[j - 1] == b'/' {
            let tag = &xml[start + 1..j - 1];
            let name = &tag[..tag.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(tag.len())];
            let local = name.rsplit(':').next().unwrap_or(name);
            if !name.is_empty() && !is_void_ignore_case(local) {
                out.push_str(&xml[last..start]);
                out.push('<');
                out.push_str(tag.trim_end());
                out.push_str("></");
                out.push_str(name);
                out.push('>');
                last = j + 1;
            }
        }
        i = j + 1;
    }
    out.push_str(&xml[last..]);
    out
}

/// Parses `html` (XHTML is first made safe for the HTML parser) and writes XHTML.
pub fn to_xhtml(html: &str, rw: &mut dyn Rewrite) -> String {
    let src = if html.contains("/>") { expand_self_closing(html) } else { html.to_string() };
    serialize(&parse(&src), rw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::is_well_formed;

    #[test]
    fn repairs_broken_markup() {
        let x = to_xhtml("<html><body><p>One<p>Two <b>bold <i>both</b> it</i><br><img src=a.jpg><font size=3>x</font>&nbsp;&copy;</body>", &mut NoRewrite);
        assert!(is_well_formed(&x), "{x}");
        assert!(x.contains("<p>One</p><p>Two <b>bold <i>both</i></b><i> it</i><br/><img src=\"a.jpg\"/>"), "{x}");
        assert!(x.contains("\u{a0}©"));
        assert!(x.contains("xmlns=\"http://www.w3.org/1999/xhtml\""));
    }

    #[test]
    fn xhtml_self_closing_tags_survive() {
        let src = "<?xml version='1.0'?><html xmlns='http://www.w3.org/1999/xhtml'><head><title/></head><body><p><a id='x'/>After</p><div/><p>Next<br/></p></body></html>";
        let x = to_xhtml(src, &mut NoRewrite);
        assert!(is_well_formed(&x), "{x}");
        assert!(x.contains("<p><a id=\"x\"></a>After</p><div></div><p>Next<br/></p>"), "{x}");
        assert_eq!(expand_self_closing("<a href='x/y'/><br/><!-- <b/> -->"), "<a href='x/y'></a><br/><!-- <b/> -->");
    }

    /// Deepest element nesting of an XML string.
    fn max_depth(x: &str) -> usize {
        let mut r = quick_xml::Reader::from_str(x);
        let (mut d, mut max) = (0usize, 0usize);
        loop {
            match r.read_event() {
                Ok(quick_xml::events::Event::Start(_)) => {
                    d += 1;
                    max = max.max(d);
                }
                Ok(quick_xml::events::Event::End(_)) => d -= 1,
                Ok(quick_xml::events::Event::Eof) | Err(_) => return max,
                _ => {}
            }
        }
    }

    #[test]
    fn deep_nesting_is_limited_and_linear() {
        // 20 000 nested divs: the output keeps the text, nests at most about MAX_OPEN
        // levels and is well-formed.
        let n = 20_000;
        let src = format!("<html><body>{}deep text{}<p>after</p></body></html>", "<div>".repeat(n), "</div>".repeat(n));
        let t = std::time::Instant::now();
        let x = to_xhtml(&src, &mut NoRewrite);
        assert!(t.elapsed() < std::time::Duration::from_secs(20), "{:?}", t.elapsed());
        assert!(crate::xml::is_well_formed(&x));
        assert!(x.contains("deep text") && x.contains("<p>after</p>"));
        let d = max_depth(&x);
        assert!((MAX_OPEN - 5..=MAX_OPEN + 5).contains(&d), "depth {d}");
        // Nested tables add implied tbody/tr levels; still bounded.
        let src = format!("<body>{}cell{}</body>", "<table><tr><td>".repeat(2000), "</td></tr></table>".repeat(2000));
        let x = to_xhtml(&src, &mut NoRewrite);
        assert!(crate::xml::is_well_formed(&x) && x.contains("cell"));
        assert!(max_depth(&x) <= MAX_OPEN + 10, "depth {}", max_depth(&x));
    }

    #[test]
    fn reconstructed_formatting_elements_are_bounded() {
        // Each `<b id=N>` is popped by `</div>` but stays an active formatting element;
        // the next `<b>` re-opens all earlier ones (HTML5 reconstruction). Unbounded,
        // this nests N deep and takes quadratic time.
        // With the depth limit alone, every `x` still re-opened ~250 elements: a 1.5 MB
        // file became 125 MB of XHTML. The element budget stops the document instead.
        let src: String = (0..30_000).map(|i| format!("<div><b id=b{i}>x</div>")).collect();
        let t = std::time::Instant::now();
        let x = to_xhtml(&format!("<body>{src}</body>"), &mut NoRewrite);
        assert!(t.elapsed() < std::time::Duration::from_secs(20), "{:?}", t.elapsed());
        assert!(crate::xml::is_well_formed(&x));
        assert!(max_depth(&x) <= 2 * MAX_OPEN + 10, "depth {}", max_depth(&x));
        assert!(x.len() < 4 * src.len(), "{} bytes of XHTML from {}", x.len(), src.len());
    }

    #[test]
    fn sloppy_markup_does_not_reach_the_limit() {
        // Unclosed <p>, <li>, <font> and <b> in a long book: the tree builder closes
        // them (Noah's Ark keeps at most 3 equal formatting elements), so no start tag
        // is left out even after thousands of them.
        let mut src = String::from("<body>");
        for i in 0..3000 {
            src.push_str(&format!("<p><font size=2>para {i}<li>item<b>bold</p><div><span>s<i>i</div>"));
        }
        let (dom, skipped) = parse_counted(&src);
        assert_eq!(skipped, 0);
        let x = serialize(&dom, &mut NoRewrite);
        assert!(crate::xml::is_well_formed(&x));
        assert_eq!(x.matches("<li>").count(), 3000);
        assert!(x.contains("para 2999"));
        // The deep cases above do leave tags out.
        let (_, skipped) = parse_counted(&format!("<body>{}x</body>", "<div>".repeat(1000)));
        assert!(skipped > 700, "{skipped}");
    }

    struct Pagebreak;
    impl Rewrite for Pagebreak {
        fn element(&mut self, node: &Handle, name: &mut String, attrs: &mut Vec<(String, String)>) -> Action {
            match name.as_str() {
                "mbp:pagebreak" => Action::Drop,
                "font" => Action::Unwrap,
                "h1" => {
                    attrs.push(("id".into(), text_of_max(node, usize::MAX).to_lowercase()));
                    Action::Keep
                }
                _ => Action::Keep,
            }
        }
    }

    #[test]
    fn rewrites_and_odd_names() {
        let x = to_xhtml("<body><h1>Top</h1><mbp:pagebreak/><font color=red>t</font><p epub:type=x 1a=b data-q='\"'>q</p><svg viewBox='0 0 1 1'><image xlink:href='i.png'/></svg><script>bad()</script>", &mut Pagebreak);
        assert!(is_well_formed(&x), "{x}");
        assert!(x.contains("<h1 id=\"top\">Top</h1>t<p epub:type=\"x\" data-q=\"&quot;\">q</p>"), "{x}");
        assert!(x.contains("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\"><image xlink:href=\"i.png\"></image></svg>"), "{x}");
        assert!(!x.contains("bad()"));
    }
}
