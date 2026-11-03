//! Markdown for the plugin text viewer and the chat view, parsed by pulldown-cmark
//! (CommonMark with GitHub tables and strikethrough).
//!
//! The output is data ([`Block`]s of [`Span`]s): headings, paragraphs (soft line
//! breaks become spaces), list items with their nesting depth and number, block quote
//! paragraphs, code blocks, rules and tables (cells of spans, the first row the header).
//! Inline: bold, italic, code and links. HTML is kept as text. Layout and painting are
//! the app's job.

/// Inline style of a [`Span`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct SpanStyle {
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Code.
    pub code: bool,
}

/// A run of text with one style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Text.
    pub text: String,
    /// Style.
    pub style: SpanStyle,
    /// Link target when the run is (part of) a link.
    pub link: Option<String>,
}

impl Span {
    /// Plain text.
    pub fn plain(text: &str) -> Self {
        Span { text: text.into(), style: SpanStyle::default(), link: None }
    }
}

/// A block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// `#` heading, level 1-6.
    Heading {
        /// Level.
        level: u8,
        /// Text.
        spans: Vec<Span>,
    },
    /// Paragraph.
    Paragraph(Vec<Span>),
    /// List item; `number` is set for ordered lists; `depth` 0 = top level.
    ListItem {
        /// Number of an ordered item.
        number: Option<u64>,
        /// Nesting depth.
        depth: u8,
        /// Text.
        spans: Vec<Span>,
    },
    /// Block quote paragraph.
    Quote(Vec<Span>),
    /// Code block, lines kept.
    Code(Vec<String>),
    /// Horizontal rule.
    Rule,
    /// Table: rows of cells; the first row is the header when `header` is true.
    Table {
        /// Rows of cells.
        rows: Vec<Vec<Vec<Span>>>,
        /// First row is a header.
        header: bool,
    },
}

/// Parses `md` (CommonMark with GitHub tables and strikethrough, by pulldown-cmark).
pub fn parse(md: &str) -> Vec<Block> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let mut b = Builder::default();
    for ev in Parser::new_ext(md, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH) {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Heading { .. } | Tag::Paragraph | Tag::TableCell | Tag::TableHead | Tag::TableRow => {}
                Tag::BlockQuote(_) => b.quote += 1,
                Tag::CodeBlock(_) => b.code = Some(String::new()),
                Tag::List(start) => {
                    // A nested list: the item's text so far is its own line.
                    b.flush_item();
                    b.lists.push(start);
                }
                Tag::Item => b.items.push(false),
                Tag::Table(_) => b.table = Some(Vec::new()),
                Tag::Emphasis => b.italic += 1,
                Tag::Strong => b.bold += 1,
                Tag::Link { dest_url, .. } => b.link = Some(dest_url.to_string()),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(level) => {
                    let spans = std::mem::take(&mut b.spans);
                    b.out.push(Block::Heading { level: level as u8, spans });
                }
                TagEnd::Paragraph => b.flush_text(),
                TagEnd::BlockQuote(_) => b.quote = b.quote.saturating_sub(1),
                TagEnd::CodeBlock => {
                    let code = b.code.take().unwrap_or_default();
                    b.out.push(Block::Code(code.strip_suffix('\n').unwrap_or(&code).split('\n').map(str::to_string).collect()));
                }
                TagEnd::List(_) => {
                    b.lists.pop();
                }
                TagEnd::Item => {
                    b.flush_item();
                    b.items.pop();
                    if let Some(Some(n)) = b.lists.last_mut() {
                        *n += 1;
                    }
                }
                TagEnd::TableCell => {
                    let cell = std::mem::take(&mut b.spans);
                    b.row.push(cell);
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    let row = std::mem::take(&mut b.row);
                    if let Some(t) = b.table.as_mut() {
                        t.push(row);
                    }
                }
                TagEnd::Table => {
                    let rows = b.table.take().unwrap_or_default();
                    b.out.push(Block::Table { rows, header: true });
                }
                TagEnd::Emphasis => b.italic = b.italic.saturating_sub(1),
                TagEnd::Strong => b.bold = b.bold.saturating_sub(1),
                TagEnd::Link => b.link = None,
                _ => {}
            },
            Event::Text(t) => match b.code.as_mut() {
                Some(c) => c.push_str(&t),
                None => b.text(&t, false),
            },
            Event::Code(t) => b.text(&t, true),
            Event::SoftBreak | Event::HardBreak => b.text(" ", false),
            Event::Html(t) | Event::InlineHtml(t) => b.text(t.trim_end_matches('\n'), false),
            Event::Rule => b.out.push(Block::Rule),
            Event::FootnoteReference(t) => b.text(&format!("[^{t}]"), false),
            _ => {}
        }
    }
    b.flush_text();
    b.out
}

/// The state of [`parse`] between parser events.
#[derive(Default)]
struct Builder {
    out: Vec<Block>,
    /// Inline text of the block (paragraph, item, heading, cell) being read.
    spans: Vec<Span>,
    bold: usize,
    italic: usize,
    link: Option<String>,
    quote: usize,
    code: Option<String>,
    /// Open lists: the next item's number (`None`: bullets).
    lists: Vec<Option<u64>>,
    /// Open items: whether the item's own line was written.
    items: Vec<bool>,
    table: Option<Vec<Vec<Vec<Span>>>>,
    row: Vec<Vec<Span>>,
}

impl Builder {
    fn text(&mut self, t: &str, code: bool) {
        if t.is_empty() {
            return;
        }
        let style = SpanStyle { bold: self.bold > 0, italic: self.italic > 0, code };
        match self.spans.last_mut() {
            Some(last) if last.style == style && last.link == self.link => last.text.push_str(t),
            _ => self.spans.push(Span { text: t.to_string(), style, link: self.link.clone() }),
        }
    }

    /// The first text of a list item is its line; more text in it is a paragraph.
    fn flush_item(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        match self.items.last_mut() {
            Some(done @ false) => {
                *done = true;
                let spans = std::mem::take(&mut self.spans);
                let depth = self.lists.len().saturating_sub(1).min(6) as u8;
                let number = self.lists.last().copied().flatten();
                self.out.push(Block::ListItem { number, depth, spans });
            }
            _ => self.flush_text(),
        }
    }

    fn flush_text(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        if self.items.last() == Some(&false) {
            return self.flush_item();
        }
        let spans = std::mem::take(&mut self.spans);
        self.out.push(if self.quote > 0 { Block::Quote(spans) } else { Block::Paragraph(spans) });
    }
}

/// Parses the inline markup of one paragraph.
pub fn inline(text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for b in parse(text) {
        let spans = match b {
            Block::Heading { spans, .. } | Block::Paragraph(spans) | Block::Quote(spans) | Block::ListItem { spans, .. } => spans,
            _ => continue,
        };
        for s in spans {
            match out.last_mut() {
                Some(last) if last.style == s.style && last.link == s.link => last.text.push_str(&s.text),
                _ => out.push(s),
            }
        }
    }
    out
}

/// The text of `blocks` without markup (one line per block), for logs and tests.
pub fn to_plain(blocks: &[Block]) -> String {
    let join = |s: &[Span]| s.iter().map(|x| x.text.as_str()).collect::<String>();
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Heading { spans, .. } | Block::Paragraph(spans) | Block::Quote(spans) => out.push(join(spans)),
            Block::ListItem { number, depth, spans } => {
                let marker = number.map(|n| format!("{n}.")).unwrap_or_else(|| "-".into());
                out.push(format!("{}{marker} {}", "  ".repeat(*depth as usize), join(spans)));
            }
            Block::Code(lines) => out.extend(lines.iter().cloned()),
            Block::Rule => out.push("---".into()),
            Block::Table { rows, .. } => {
                for r in rows {
                    out.push(r.iter().map(|c| join(c)).collect::<Vec<_>>().join(" | "));
                }
            }
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(bold: bool, italic: bool, code: bool) -> SpanStyle {
        SpanStyle { bold, italic, code }
    }

    #[test]
    fn blocks() {
        let md = "# Title #\n\nFirst line\nsecond line.\n\n- one\n- two\n  continued\n  - nested\n1. first\n2) second\n\n> quoted\n> more\n\n```lua\nlocal x = 1\n  y()\n```\n\n    indented code\n\n---\n| A | B |\n|---|:-:|\n| 1 | **2** |\n\n###### six\n####### seven";
        let b = parse(md);
        assert_eq!(b[0], Block::Heading { level: 1, spans: vec![Span::plain("Title")] });
        assert_eq!(b[1], Block::Paragraph(vec![Span::plain("First line second line.")]));
        assert_eq!(b[2], Block::ListItem { number: None, depth: 0, spans: vec![Span::plain("one")] });
        assert_eq!(b[3], Block::ListItem { number: None, depth: 0, spans: vec![Span::plain("two continued")] });
        assert_eq!(b[4], Block::ListItem { number: None, depth: 1, spans: vec![Span::plain("nested")] });
        assert_eq!(b[5], Block::ListItem { number: Some(1), depth: 0, spans: vec![Span::plain("first")] });
        assert_eq!(b[6], Block::ListItem { number: Some(2), depth: 0, spans: vec![Span::plain("second")] });
        assert_eq!(b[7], Block::Quote(vec![Span::plain("quoted more")]));
        assert_eq!(b[8], Block::Code(vec!["local x = 1".into(), "  y()".into()]));
        assert_eq!(b[9], Block::Code(vec!["indented code".into()]));
        assert_eq!(b[10], Block::Rule);
        let Block::Table { rows, header } = &b[11] else { panic!("{:?}", b[11]) };
        assert!(header);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][1], vec![Span { text: "2".into(), style: st(true, false, false), link: None }]);
        assert_eq!(b[12], Block::Heading { level: 6, spans: vec![Span::plain("six")] });
        assert_eq!(b[13], Block::Paragraph(vec![Span::plain("####### seven")]));
        assert!(to_plain(&b).contains("  - nested"));
    }

    #[test]
    fn inline_markup() {
        let s = inline("a **bold** and *it* and `co*de` [link **x**](https://e.org \"t\") <https://f.org> snake_case_name 2 * 3 \\*no\\*");
        let texts: Vec<(&str, SpanStyle, Option<&str>)> = s.iter().map(|x| (x.text.as_str(), x.style, x.link.as_deref())).collect();
        assert_eq!(
            texts,
            vec![
                ("a ", st(false, false, false), None),
                ("bold", st(true, false, false), None),
                (" and ", st(false, false, false), None),
                ("it", st(false, true, false), None),
                (" and ", st(false, false, false), None),
                ("co*de", st(false, false, true), None),
                (" ", st(false, false, false), None),
                ("link ", st(false, false, false), Some("https://e.org")),
                ("x", st(true, false, false), Some("https://e.org")),
                (" ", st(false, false, false), None),
                ("https://f.org", st(false, false, false), Some("https://f.org")),
                (" snake_case_name 2 * 3 *no*", st(false, false, false), None),
            ]
        );
        // Unclosed markers stay text.
        assert_eq!(inline("**open"), vec![Span::plain("**open")]);
        assert_eq!(inline("[a](b"), vec![Span::plain("[a](b")]);
        assert_eq!(inline("_x_"), vec![Span { text: "x".into(), style: st(false, true, false), link: None }]);
        assert!(parse("").is_empty());
    }
}
