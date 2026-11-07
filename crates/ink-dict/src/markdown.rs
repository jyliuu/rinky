//! Markdown from definition markup (StarDict types `h`, `x`, `g`), for the full view.
//!
//! The markup is parsed and styled by ink-html, the engine that lays out books
//! (html5ever or quick-xml into its DOM; its built-in rules, the dictionary's style
//! sheet and `style=""` attributes cascaded). The walk over the styled tree keeps what
//! matters on e-ink: bold and italics, paragraphs and line breaks, numbered and
//! bulleted lists with their nesting, block quotes and rules. Colors, fonts, sizes,
//! images and sounds are dropped. The style sheet is `<name>.css` next to the `.ifo`, as
//! KOReader reads it ([`Style`]).

use ink_html::css;
use ink_html::dom::{Classifier, Dom, ElemRef, NodeId, NodeKind};
use ink_html::style::{ua_rules, Cascade, ComputedStyle, Display, Origin, RuleSet, StructStyle, StyleContext, WhiteSpace};
use std::sync::Arc;

/// A dictionary's style sheet.
#[derive(Clone, Default)]
pub struct Style(Arc<RuleSet>);

impl std::fmt::Debug for Style {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Style({} rules)", self.0.len())
    }
}

/// XDXF elements as HTML would show them (`k` headword, `ex` example, `abr`
/// abbreviation, `tr` transcription, `rref` a sound file); its line breaks are kept.
const XDXF_CSS: &str = "body { white-space: pre-line } k { font-weight: bold } ex, abr, i { font-style: italic } def, blockquote { display: block } rref { display: none }";

impl Style {
    /// Parses a style sheet.
    pub fn parse(src: &str) -> Style {
        let mut rules = RuleSet::default();
        rules.add_sheet(&css::parse_stylesheet(src), Origin::Author);
        Style(Arc::new(rules))
    }

    /// True when the sheet has no rules.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn cascade(author: Arc<RuleSet>) -> Cascade {
    Cascade { ua: ua_rules(), author, publisher: true, ctx: StyleContext { font_px: 16.0, page_width: 600.0, page_height: 800.0 } }
}

/// Classifies elements with the definition's cascade while the DOM is built.
struct Classify<'a>(&'a Cascade);

impl Classifier for Classify<'_> {
    fn classify(&mut self, dom: &Dom, id: NodeId, parent_ws: WhiteSpace) -> StructStyle {
        self.0.structure(ElemRef { dom, id }, parent_ws)
    }
}

/// A run of text with one style.
#[derive(Clone, Debug, PartialEq)]
struct Seg {
    text: String,
    bold: bool,
    italic: bool,
}

#[derive(Debug)]
enum Out {
    Para { segs: Vec<Seg>, quote: usize },
    Item { indent: usize, marker: String, segs: Vec<Seg> },
    Rule,
}

/// An open list.
struct List {
    /// Next number (`None`: bullets).
    next: Option<u64>,
    /// Column of its items' markers.
    indent: usize,
    /// Column of the content of its last item (where a list inside it goes).
    content: usize,
}

#[derive(Default)]
struct Conv {
    out: Vec<Out>,
    cur: Vec<Seg>,
    lists: Vec<List>,
    /// Open items: whether the item's line was written.
    items: Vec<bool>,
    quote: usize,
}

impl Conv {
    /// Adds text in `st`'s style, white space collapsed.
    fn text(&mut self, t: &str, st: &ComputedStyle) {
        let (bold, italic) = (st.bold, st.italic);
        let mut s = String::with_capacity(t.len());
        let mut space = self.cur.last().is_none_or(|l| l.text.ends_with(' '));
        for c in t.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                if !space {
                    s.push(' ');
                    space = true;
                }
            } else {
                s.push(c);
                space = false;
            }
        }
        if s.is_empty() {
            return;
        }
        match self.cur.last_mut() {
            Some(l) if l.bold == bold && l.italic == italic => l.text.push_str(&s),
            _ => self.cur.push(Seg { text: s, bold, italic }),
        }
    }

    /// A block boundary: the text so far is a paragraph; in a list item, more text of
    /// the item goes on its line after a space.
    fn boundary(&mut self, st: &ComputedStyle) {
        if self.items.is_empty() {
            self.flush();
        } else {
            self.text(" ", st);
        }
    }

    fn has_text(&self) -> bool {
        self.cur.iter().any(|s| !s.text.trim().is_empty())
    }

    /// Writes the text so far: the open item's line, else a paragraph.
    fn flush(&mut self) {
        if !self.has_text() {
            self.cur.clear();
            return;
        }
        let segs = std::mem::take(&mut self.cur);
        match (self.items.last_mut(), self.lists.last_mut()) {
            (Some(done @ false), Some(list)) => {
                *done = true;
                let marker = match list.next.as_mut() {
                    Some(n) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    None => "- ".to_string(),
                };
                list.content = list.indent + marker.len();
                self.out.push(Out::Item { indent: list.indent, marker, segs });
            }
            _ => self.out.push(Out::Para { segs, quote: self.quote }),
        }
    }

    /// Walks element `id` of `dom` styled by `cx`, inside `parent`'s style.
    fn element(&mut self, dom: &Dom, cx: &Cascade, id: NodeId, parent: &ComputedStyle) {
        let st = cx.compute(ElemRef { dom, id }, parent);
        if st.display == Display::None {
            return;
        }
        let name = dom.name(id);
        let list = matches!(name, "ul" | "ol") && st.display.is_block_like();
        let item = name == "li" && !self.lists.is_empty();
        match name {
            _ if list => {
                // A list in an item: the item's text so far is its line.
                self.flush();
                let indent = self.lists.last().map_or(0, |l| l.content);
                self.lists.push(List { next: (name == "ol").then_some(1), indent, content: indent + 2 });
            }
            _ if item => {
                self.flush();
                self.items.push(false);
            }
            "blockquote" => {
                self.flush();
                self.quote += 1;
            }
            "hr" => {
                self.flush();
                self.out.push(Out::Rule);
            }
            "br" => self.boundary(&st),
            _ if st.display.is_block_like() => self.boundary(&st),
            _ => {}
        }
        for &c in &dom.node(id).children {
            match &dom.node(c).kind {
                NodeKind::Text(t) if st.white_space.keeps_newlines() => {
                    for (i, l) in t.split('\n').enumerate() {
                        if i > 0 {
                            self.boundary(&st);
                        }
                        self.text(l, &st);
                    }
                }
                NodeKind::Text(t) => self.text(t, &st),
                NodeKind::Element(_) => self.element(dom, cx, c, &st),
            }
        }
        match name {
            _ if list => {
                self.flush();
                self.lists.pop();
            }
            _ if item => {
                self.flush();
                self.items.pop();
            }
            "blockquote" => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            _ if st.display.is_block_like() => self.boundary(&st),
            _ => {}
        }
    }
}

/// Converts one definition of type `kind` (`h` HTML, `x` XDXF, `g` Pango) to Markdown,
/// with the dictionary's `style`.
pub fn to_markdown(kind: char, raw: &str, style: &Style) -> String {
    let mut author = style.0.clone();
    let body = if kind == 'h' {
        raw.to_string()
    } else {
        // XDXF's `tr` is a transcription, which HTML parsing would take for a table row.
        let mut rules = RuleSet::default();
        rules.add_sheet(&css::parse_stylesheet(XDXF_CSS), Origin::Author);
        author = Arc::new(rules);
        raw.replace("<tr>", "<span>").replace("</tr>", "</span>")
    };
    let cx = cascade(author);
    let dom = ink_html::parse::parse_chapter(&format!("<html><body>{body}</body></html>"), &mut Classify(&cx));
    let mut c = Conv::default();
    if dom.body != ink_html::dom::NONE {
        c.element(&dom, &cx, dom.body, &ComputedStyle::root(16.0));
    }
    c.flush();
    render(&c.out)
}

/// Markdown of plain text: one paragraph per line, shown as written.
pub fn plain(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| line(&[Seg { text: l.to_string(), bold: false, italic: false }]))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Escapes Markdown punctuation so text shows as written.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '#' | '|' | '-' | '+' | '!' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The inline Markdown of a line of runs. Emphasis markers go around the letters and
/// digits of a run only: spaces and punctuation at its ends stay outside, where
/// CommonMark's flanking rules would otherwise show the markers as text.
fn line(segs: &[Seg]) -> String {
    let mut out = String::new();
    for s in segs {
        let marks = match (s.bold, s.italic) {
            (true, true) => "***",
            (true, false) => "**",
            (false, true) => "*",
            (false, false) => "",
        };
        let t = s.text.as_str();
        let core_start = t.find(char::is_alphanumeric);
        let core_end = t.rfind(char::is_alphanumeric).map(|i| i + t[i..].chars().next().map_or(1, char::len_utf8));
        match (marks, core_start, core_end) {
            ("", ..) | (_, None, _) | (_, _, None) => out.push_str(&escape(t)),
            (m, Some(a), Some(b)) => {
                out.push_str(&escape(&t[..a]));
                out.push_str(m);
                out.push_str(&escape(&t[a..b]));
                out.push_str(m);
                out.push_str(&escape(&t[b..]));
            }
        }
    }
    let out = out.trim().to_string();
    // "1. " or "1) " at the start would be a list item.
    let digits = out.find(|c: char| !c.is_ascii_digit()).unwrap_or(out.len());
    if digits > 0 && out[digits..].starts_with(['.', ')']) {
        return format!("{}\\{}", &out[..digits], &out[digits..]);
    }
    out
}

fn render(out: &[Out]) -> String {
    let mut md = String::new();
    let mut prev_item = false;
    for o in out {
        match o {
            Out::Para { segs, quote } => {
                let l = line(segs);
                if l.is_empty() {
                    continue;
                }
                if !md.is_empty() {
                    md.push('\n');
                }
                md.push_str(&"> ".repeat(*quote));
                md.push_str(&l);
                md.push('\n');
                prev_item = false;
            }
            Out::Item { indent, marker, segs } => {
                if !md.is_empty() && !prev_item {
                    md.push('\n');
                }
                md.push_str(&" ".repeat(*indent));
                md.push_str(marker);
                md.push_str(&line(segs));
                md.push('\n');
                prev_item = true;
            }
            Out::Rule => {
                md.push_str("\n---\n");
                prev_item = false;
            }
        }
    }
    md.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(kind: char, raw: &str) -> String {
        to_markdown(kind, raw, &Style::default())
    }

    #[test]
    fn inline_styles_and_paragraphs() {
        assert_eq!(md('h', "<b>cat</b> <i>n.</i><br>A small <em>domesticated</em> animal."), "**cat** *n*.\n\nA small *domesticated* animal.");
        // Markers around letters only: punctuation and spaces stay outside.
        assert_eq!(md('h', "see<i> (archaic)</i>, done"), "see (*archaic*), done");
        assert_eq!(md('h', "un<i>believ</i>able"), "un*believ*able");
        assert_eq!(md('h', "<b>,</b> x"), ", x");
        // Markdown punctuation in the text is escaped; entities decoded.
        assert_eq!(md('h', "<p>1. a *star* &amp; [x]</p>"), "1\\. a \\*star\\* & \\[x\\]");
    }

    #[test]
    fn lists_keep_numbers_and_nesting() {
        let raw = "<i>noun</i><ol><li>A house.<ul><li><i>a big house</i></li></ul></li><li>A home<br>(rare).</li></ol><p>From Old English.</p>";
        assert_eq!(md('h', raw), "*noun*\n\n1. A house.\n   - *a big house*\n2. A home (rare).\n\nFrom Old English.");
    }

    #[test]
    fn hidden_and_raw_text_are_dropped() {
        assert_eq!(md('h', "<style>.a{color:red}</style><script>x<y</script>word<img src=a.png>"), "word");
    }

    #[test]
    fn a_style_sheet_styles_classes() {
        let css = "/* c */ .pos { font-style: italic } span.hw { font-weight: bold } .audio { display: none } .sense { display: block }\n.a .b { display: none } @media print { .x { display: none } } .x2 { font-weight: 700 }";
        // A descendant rule applies only inside its ancestor.
        assert_eq!(to_markdown('h', r#"<div class="a"><span class="b">gone</span></div>kept"#, &Style::parse(css)), "kept");
        let st = Style::parse(css);
        let raw = r#"<span class="hw">run</span> <span class="pos">verb</span><span class="audio">play</span><span class="sense">To move fast.</span><span class="sense b">Go.</span><span class="x x2">!</span>"#;
        assert_eq!(to_markdown('h', raw, &st), "**run** *verb*\n\nTo move fast.\n\nGo.\n\n\\!");
        // An inline style too.
        assert_eq!(md('h', r#"<span style="font-weight:bold">a</span> b"#), "**a** b");
    }

    #[test]
    fn xdxf() {
        assert_eq!(md('x', "<k>dog</k>\n<tr>dɒɡ</tr> <abr>n</abr>\n<blockquote>a pet <ex>my dog</ex></blockquote>"), "**dog**\n\ndɒɡ *n*\n\n> a pet *my dog*");
    }
}
