//! A small Markdown parser for the plugin text viewer (and the phase 8 chat view).
//!
//! Supported: ATX headings (`#` .. `######`), paragraphs (lines joined), bullet lists
//! (`-`, `*`, `+`) and ordered lists (`1.`, `1)`) with nesting by indentation, block
//! quotes (`>`), fenced code blocks (```` ``` ```` / `~~~`), indented code (4 spaces),
//! horizontal rules (`---`, `***`, `___`), pipe tables, and inline `**bold**`,
//! `__bold__`, `*italic*`, `_italic_`, `` `code` ``, `[text](url)`, `<https://url>` and
//! backslash escapes. Anything else is plain text. The output is data
//! ([`Block`]s of [`Span`]s); layout and painting are the app's job.

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

/// Parses `md`.
pub fn parse(md: &str) -> Vec<Block> {
    let lines: Vec<&str> = md.lines().collect();
    let mut out = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut quote: Vec<String> = Vec::new();
    let flush = |para: &mut Vec<String>, quote: &mut Vec<String>, out: &mut Vec<Block>| {
        if !para.is_empty() {
            out.push(Block::Paragraph(inline(&para.join(" "))));
            para.clear();
        }
        if !quote.is_empty() {
            out.push(Block::Quote(inline(&quote.join(" "))));
            quote.clear();
        }
    };
    let mut i = 0;
    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        // Fenced code.
        if let Some(fence) = ["```", "~~~"].into_iter().find(|f| trimmed.starts_with(f)) {
            flush(&mut para, &mut quote, &mut out);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                code.push(lines[i].to_string());
                i += 1;
            }
            out.push(Block::Code(code));
            i += 1;
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut para, &mut quote, &mut out);
            i += 1;
            continue;
        }
        // Indented code (not inside a paragraph).
        if indent >= 4 && para.is_empty() && quote.is_empty() && list_marker(trimmed).is_none() {
            let mut code = Vec::new();
            while i < lines.len() && (lines[i].starts_with("    ") || lines[i].trim().is_empty()) {
                code.push(lines[i].get(4..).unwrap_or("").to_string());
                i += 1;
            }
            while code.last().is_some_and(|l| l.trim().is_empty()) {
                code.pop();
            }
            out.push(Block::Code(code));
            continue;
        }
        if let Some(level) = heading_level(trimmed) {
            flush(&mut para, &mut quote, &mut out);
            let text = trimmed[level as usize..].trim().trim_end_matches('#').trim_end();
            out.push(Block::Heading { level, spans: inline(text) });
            i += 1;
            continue;
        }
        if is_rule(trimmed) {
            flush(&mut para, &mut quote, &mut out);
            out.push(Block::Rule);
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('>') {
            if !para.is_empty() {
                flush(&mut para, &mut quote, &mut out);
            }
            quote.push(rest.trim().to_string());
            i += 1;
            continue;
        }
        if let Some((number, text)) = list_marker(trimmed) {
            flush(&mut para, &mut quote, &mut out);
            let depth = (indent / 2).min(6) as u8;
            let mut text = text.to_string();
            // Continuation lines: indented, not a new block.
            while i + 1 < lines.len() {
                let next = lines[i + 1];
                let nt = next.trim_start();
                if nt.is_empty() || list_marker(nt).is_some() || next.len() - nt.len() < 2 || heading_level(nt).is_some() {
                    break;
                }
                text.push(' ');
                text.push_str(nt.trim_end());
                i += 1;
            }
            out.push(Block::ListItem { number, depth, spans: inline(&text) });
            i += 1;
            continue;
        }
        if trimmed.starts_with('|') && i + 1 < lines.len() {
            flush(&mut para, &mut quote, &mut out);
            let mut rows = Vec::new();
            let mut header = false;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                let cells = table_cells(lines[i].trim());
                if cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' '))) {
                    header = rows.len() == 1;
                } else {
                    rows.push(cells.iter().map(|c| inline(c)).collect());
                }
                i += 1;
            }
            out.push(Block::Table { rows, header });
            continue;
        }
        if !quote.is_empty() {
            flush(&mut para, &mut quote, &mut out);
        }
        para.push(trimmed.to_string());
        i += 1;
    }
    flush(&mut para, &mut quote, &mut out);
    out
}

fn heading_level(s: &str) -> Option<u8> {
    let n = s.bytes().take_while(|b| *b == b'#').count();
    ((1..=6).contains(&n) && (s.len() == n || s.as_bytes()[n] == b' ')).then_some(n as u8)
}

fn is_rule(s: &str) -> bool {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3 && ["-", "*", "_"].iter().any(|c| compact.chars().all(|ch| ch.to_string() == *c))
}

fn list_marker(s: &str) -> Option<(Option<u64>, &str)> {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(m) {
            return Some((None, rest.trim_start()));
        }
    }
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if (1..=9).contains(&digits) {
        let rest = &s[digits..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((s[..digits].parse().ok(), r.trim_start()));
        }
    }
    None
}

fn table_cells(row: &str) -> Vec<String> {
    let inner = row.trim().trim_start_matches('|').trim_end_matches('|');
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

/// Parses inline markup of one paragraph.
pub fn inline(text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut style = SpanStyle::default();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    let push = |out: &mut Vec<Span>, cur: &mut String, style: SpanStyle, link: Option<String>| {
        if cur.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.style == style && last.link == link => last.text.push_str(cur),
            _ => out.push(Span { text: cur.clone(), style, link }),
        }
        cur.clear();
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() {
            cur.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|&x| x == '`') {
                push(&mut out, &mut cur, style, None);
                let code: String = chars[i + 1..i + 1 + end].iter().collect();
                cur.push_str(&code);
                push(&mut out, &mut cur, SpanStyle { code: true, ..style }, None);
                i += end + 2;
                continue;
            }
        }
        if c == '[' {
            if let Some((label, url, used)) = link_at(&chars[i..]) {
                push(&mut out, &mut cur, style, None);
                for s in inline(&label) {
                    let st = SpanStyle { bold: s.style.bold || style.bold, italic: s.style.italic || style.italic, code: s.style.code };
                    let mut t = s.text;
                    push(&mut out, &mut t, st, Some(url.clone()));
                }
                i += used;
                continue;
            }
        }
        if c == '<' {
            let rest: String = chars[i + 1..].iter().collect();
            if let Some(end) = rest.find('>') {
                let url = &rest[..end];
                if (url.starts_with("http://") || url.starts_with("https://")) && !url.contains(' ') {
                    push(&mut out, &mut cur, style, None);
                    let mut t = url.to_string();
                    push(&mut out, &mut t, style, Some(url.to_string()));
                    i += end + 2;
                    continue;
                }
            }
        }
        if c == '*' || c == '_' {
            let double = i + 1 < chars.len() && chars[i + 1] == c;
            let prev = if i > 0 { chars[i - 1] } else { ' ' };
            let next = chars.get(i + if double { 2 } else { 1 }).copied().unwrap_or(' ');
            // `_` inside a word (snake_case) is text.
            let intraword = c == '_' && prev.is_alphanumeric() && next.is_alphanumeric();
            let opening = !next.is_whitespace();
            let closing = !prev.is_whitespace();
            let on = if double { style.bold } else { style.italic };
            if !intraword && ((on && closing) || (!on && opening && has_closer(&chars[i + if double { 2 } else { 1 }..], c, double))) {
                push(&mut out, &mut cur, style, None);
                if double {
                    style.bold = !style.bold;
                    i += 2;
                } else {
                    style.italic = !style.italic;
                    i += 1;
                }
                continue;
            }
        }
        cur.push(c);
        i += 1;
    }
    push(&mut out, &mut cur, style, None);
    out
}

fn has_closer(rest: &[char], c: char, double: bool) -> bool {
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == c {
            let is_double = i + 1 < rest.len() && rest[i + 1] == c;
            if is_double == double && i > 0 && !rest[i - 1].is_whitespace() {
                return true;
            }
            i += if is_double { 2 } else { 1 };
        } else {
            i += 1;
        }
    }
    false
}

fn link_at(chars: &[char]) -> Option<(String, String, usize)> {
    let mut depth = 0;
    let mut close = None;
    for (j, &ch) in chars.iter().enumerate() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = chars[close + 2..].iter().position(|&c| c == ')')? + close + 2;
    let label: String = chars[1..close].iter().collect();
    let target: String = chars[close + 2..end].iter().collect();
    let url = target.split_whitespace().next().unwrap_or("").trim_matches(['<', '>']).to_string();
    Some((label, url, end + 1))
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
