//! XHTML to text blocks (the prototype parser, unchanged in behaviour).
//!
//! Recognises paragraphs and other block tags, headings h1-h6, `<br>`, bold and italic,
//! and common HTML entities. No CSS, images, tables or links (phase 2 replaces this
//! with html5ever and a CSS cascade).

use ink_text::Style;
use quick_xml::events::Event;
use quick_xml::Reader;

/// A run of text in one style.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Style of the whole run.
    pub style: Style,
    /// Text, whitespace not yet collapsed.
    pub text: String,
}

/// Paragraph or heading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// Body paragraph.
    Para,
    /// Heading level 1-6.
    Heading(u8),
}

/// One block of text.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// Paragraph or heading.
    pub kind: BlockKind,
    /// false for blocks that continue after a `<br>`.
    pub indent: bool,
    /// Styled runs.
    pub runs: Vec<Run>,
}

impl Block {
    /// The block's text with all runs concatenated.
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// Replacement text for a named HTML entity.
pub fn html_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "nbsp" => "\u{a0}",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "laquo" => "«",
        "raquo" => "»",
        "copy" => "©",
        "shy" => "",
        "eacute" => "é",
        "egrave" => "è",
        "aacute" => "á",
        "agrave" => "à",
        "ouml" => "ö",
        "uuml" => "ü",
        "auml" => "ä",
        "aring" => "å",
        "oslash" => "ø",
        "aelig" => "æ",
        "ccedil" => "ç",
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        _ => return None,
    })
}

fn is_block_tag(name: &[u8]) -> bool {
    matches!(
        name,
        b"p" | b"div" | b"li" | b"blockquote" | b"section" | b"article" | b"pre"
            | b"tr" | b"dt" | b"dd" | b"table" | b"ul" | b"ol" | b"hr" | b"figure"
            | b"figcaption" | b"header" | b"footer" | b"aside" | b"body"
    )
}

fn heading_level(name: &[u8]) -> Option<u8> {
    match name {
        [b'h', d @ b'1'..=b'6'] => Some(d - b'0'),
        _ => None,
    }
}

struct BlockBuilder {
    blocks: Vec<Block>,
    runs: Vec<Run>,
    kind: BlockKind,
    indent: bool,
}

impl BlockBuilder {
    fn push_text(&mut self, style: Style, text: &str) {
        if let Some(last) = self.runs.last_mut() {
            if last.style == style {
                last.text.push_str(text);
                return;
            }
        }
        self.runs.push(Run { style, text: text.to_string() });
    }

    fn flush(&mut self, next_kind: BlockKind, next_indent: bool) {
        let has_text = self.runs.iter().any(|r| !r.text.trim().is_empty());
        if has_text {
            let runs = std::mem::take(&mut self.runs);
            let kind = std::mem::replace(&mut self.kind, BlockKind::Para);
            self.blocks.push(Block { kind, indent: self.indent, runs });
        } else {
            self.runs.clear();
        }
        self.kind = next_kind;
        self.indent = next_indent;
    }
}

/// Parses an XHTML document into blocks. Never fails: broken markup ends the parse.
pub fn parse_xhtml(xml: &str) -> Vec<Block> {
    let mut r = Reader::from_str(xml);
    r.config_mut().check_end_names = false;
    let mut b = BlockBuilder { blocks: Vec::new(), runs: Vec::new(), kind: BlockKind::Para, indent: true };
    let mut skip_depth = 0u32;
    let mut in_body = false;
    let mut bold = 0u32;
    let mut italic = 0u32;

    while let Ok(ev) = r.read_event() {
        match ev {
            Event::Start(e) => {
                let name = e.local_name();
                let name = name.as_ref();
                if skip_depth > 0 || matches!(name, b"head" | b"script" | b"style") {
                    skip_depth += 1;
                    continue;
                }
                if name == b"body" {
                    in_body = true;
                }
                if let Some(level) = heading_level(name) {
                    b.flush(BlockKind::Heading(level), false);
                } else if is_block_tag(name) {
                    b.flush(BlockKind::Para, true);
                }
                match name {
                    b"b" | b"strong" => bold += 1,
                    b"i" | b"em" | b"cite" => italic += 1,
                    _ => {}
                }
            }
            Event::Empty(e) => {
                if skip_depth > 0 {
                    continue;
                }
                let name = e.local_name();
                if name.as_ref() == b"br" {
                    if matches!(b.kind, BlockKind::Heading(_)) {
                        b.push_text(Style::Bold, " ");
                    } else {
                        b.flush(BlockKind::Para, false);
                    }
                } else if is_block_tag(name.as_ref()) {
                    b.flush(BlockKind::Para, true);
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                let name = name.as_ref();
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                if heading_level(name).is_some() {
                    b.flush(BlockKind::Para, false);
                } else if is_block_tag(name) {
                    b.flush(BlockKind::Para, true);
                }
                match name {
                    b"b" | b"strong" => bold = bold.saturating_sub(1),
                    b"i" | b"em" | b"cite" => italic = italic.saturating_sub(1),
                    _ => {}
                }
            }
            Event::Text(t) => {
                if skip_depth > 0 || !in_body {
                    continue;
                }
                let text = t
                    .unescape_with(html_entity)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| String::from_utf8_lossy(t.as_ref()).into_owned());
                let style = if matches!(b.kind, BlockKind::Heading(_)) || bold > 0 {
                    Style::Bold
                } else if italic > 0 {
                    Style::Italic
                } else {
                    Style::Regular
                };
                b.push_text(style, &text);
            }
            Event::CData(t) => {
                if skip_depth == 0 && in_body {
                    b.push_text(Style::Regular, &String::from_utf8_lossy(t.as_ref()));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    b.flush(BlockKind::Para, true);
    b.blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xhtml_blocks_and_styles() {
        let x = r#"<html><head><title>T</title><style>p{}</style></head><body>
            <h2>Act I</h2><p>Two <i>households</i>&nbsp;both&mdash;alike.</p><p>Line one<br/>line two</p></body></html>"#;
        let blocks = parse_xhtml(x);
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0].kind, BlockKind::Heading(2));
        assert_eq!(blocks[1].runs[1].style, Style::Italic);
        assert!(blocks[1].runs[2].text.contains('\u{2014}'));
        assert!(!blocks[3].indent);
        assert_eq!(blocks[0].text(), "Act I");
    }

    #[test]
    fn broken_markup_does_not_panic() {
        for x in ["", "<body><p>open", "<body><p>a</b></i></p>", "<body>&bogus; &#xZZ; <p/></body>", "\u{0}<<<>>>"] {
            let _ = parse_xhtml(x);
        }
    }
}
