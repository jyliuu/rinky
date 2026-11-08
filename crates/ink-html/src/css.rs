//! CSS parsing: stylesheets, selectors and declaration values (a fixed subset).
//!
//! - Tokens come from `cssparser`; rules, selectors and values are parsed here.
//! - Selectors: type, universal, class, id, attribute (`[a]`, `=`, `~=`, `|=`, `^=`, `$=`,
//!   `*=`, with or without a namespace prefix), descendant, child (`>`), next sibling
//!   (`+`), subsequent sibling (`~`), `:first-child`, `:last-child`, `:first-of-type`,
//!   `:only-child`, `:root`, `:link`, `:not(simple)`. Selectors with pseudo-elements or
//!   other pseudo-classes never match (their rule is ignored).
//! - At-rules: `@media` (types `all`, `screen`, `amzn-kf8`; `print`, `speech`, `amzn-mobi`
//!   and `not ...` are skipped; features are ignored), `@supports` (contents kept),
//!   `@import`, `@font-face`. Others are ignored.

use cssparser::{Delimiter, Parser, ParserInput, Token};

/// A simplified, owned CSS token.
#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// Identifier (lowercased for keywords by the consumers).
    Ident(String),
    /// Quoted string.
    Str(String),
    /// Number.
    Num(f32),
    /// Percentage (0..100 scale).
    Pct(f32),
    /// Number with a unit (unit lowercased).
    Dim(f32, String),
    /// `#abc`.
    Hash(String),
    /// `url(...)`.
    Url(String),
    /// A function with its arguments.
    Func(String, Vec<Tok>),
    /// `,`.
    Comma,
    /// `/`.
    Slash,
    /// Any other delimiter.
    Delim(char),
}

/// One declaration: property name (lowercased), value tokens, `!important`.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    /// Property name.
    pub name: String,
    /// Value.
    pub value: Vec<Tok>,
    /// `!important`.
    pub important: bool,
}

/// Attribute selector operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttrOp {
    /// `[a]`
    Exists,
    /// `[a=v]`
    Eq(String),
    /// `[a~=v]`
    Includes(String),
    /// `[a|=v]`
    DashMatch(String),
    /// `[a^=v]`
    Prefix(String),
    /// `[a$=v]`
    Suffix(String),
    /// `[a*=v]`
    Substring(String),
}

/// `[ns|name op value]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrSel {
    /// Local attribute name (lowercase); the namespace prefix, if any, is ignored.
    pub name: String,
    /// Test.
    pub op: AttrOp,
    /// `i` flag.
    pub case_insensitive: bool,
}

/// Pseudo-classes that the matcher supports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pseudo {
    /// `:first-child`
    FirstChild,
    /// `:last-child`
    LastChild,
    /// `:only-child`
    OnlyChild,
    /// `:first-of-type`
    FirstOfType,
    /// `:root` (the chapter body's `html`; matches `body`'s parent never, so `body` here)
    Root,
    /// `:link` / `:any-link` (`a[href]`)
    Link,
    /// `:not(compound)`
    Not(Box<Compound>),
}

/// A compound selector: `tag#id.class[attr]:pseudo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Compound {
    /// Element name (lowercase), `None` = any.
    pub tag: Option<String>,
    /// `#id`.
    pub ids: Vec<String>,
    /// `.class`.
    pub classes: Vec<String>,
    /// Attribute tests.
    pub attrs: Vec<AttrSel>,
    /// Pseudo-classes.
    pub pseudos: Vec<Pseudo>,
}

/// How a compound relates to the next one on its left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    /// Whitespace.
    Descendant,
    /// `>`.
    Child,
    /// `+`.
    NextSibling,
    /// `~`.
    SubsequentSibling,
}

/// A complex selector. `parts[0]` is the rightmost compound (the subject); `parts[i].1`
/// is the combinator between `parts[i]` and `parts[i + 1]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selector {
    /// Compounds right to left with the combinator to their left neighbour.
    pub parts: Vec<(Compound, Combinator)>,
    /// (ids, classes + attributes + pseudo-classes, types) packed as a 3 x 10 bit number.
    pub specificity: u32,
}

/// A style rule.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleRule {
    /// Selectors (rules with no usable selector are dropped).
    pub selectors: Vec<Selector>,
    /// Declarations.
    pub decls: Vec<Declaration>,
}

/// An `@font-face` rule.
#[derive(Clone, Debug, PartialEq)]
pub struct FontFaceRule {
    /// Family name (as written, unquoted).
    pub family: String,
    /// Bold (font-weight >= 600).
    pub bold: bool,
    /// Italic or oblique.
    pub italic: bool,
    /// `url(...)` sources in order (relative to the stylesheet).
    pub sources: Vec<String>,
}

/// A parsed stylesheet. `imports` are not resolved here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stylesheet {
    /// Style rules in source order.
    pub rules: Vec<StyleRule>,
    /// `@font-face` rules.
    pub font_faces: Vec<FontFaceRule>,
    /// `@import` URLs (relative to the stylesheet) in order.
    pub imports: Vec<String>,
}

/// Deepest nesting of functions, `@media`/`@supports` blocks and `:not()` that is
/// parsed. Deeper content is skipped (cssparser skips nested blocks without
/// recursion), so hostile stylesheets cannot overflow the stack.
pub const MAX_NESTING: u32 = 32;

fn tok_of(t: &Token, p: &mut Parser, depth: u32) -> Option<Tok> {
    Some(match t {
        Token::Ident(s) => Tok::Ident(s.to_string()),
        Token::QuotedString(s) => Tok::Str(s.to_string()),
        Token::Number { value, .. } => Tok::Num(*value),
        Token::Percentage { unit_value, .. } => Tok::Pct(unit_value * 100.0),
        Token::Dimension { value, unit, .. } => Tok::Dim(*value, unit.to_ascii_lowercase()),
        Token::Hash(s) | Token::IDHash(s) => Tok::Hash(s.to_string()),
        Token::UnquotedUrl(s) => Tok::Url(s.to_string()),
        Token::Comma => Tok::Comma,
        Token::Delim('/') => Tok::Slash,
        Token::Delim(c) => Tok::Delim(*c),
        Token::Function(name) => {
            let name = name.to_ascii_lowercase();
            let args = if depth < MAX_NESTING {
                p.parse_nested_block(|p| -> Result<Vec<Tok>, cssparser::ParseError<'_, ()>> { Ok(collect_tokens_at(p, depth + 1)) }).unwrap_or_default()
            } else {
                // Too deep: the arguments are skipped.
                let _ = p.parse_nested_block(|_| -> Result<(), cssparser::ParseError<'_, ()>> { Ok(()) });
                Vec::new()
            };
            if name == "url" {
                if let Some(Tok::Str(s)) = args.first() {
                    return Some(Tok::Url(s.clone()));
                }
            }
            Tok::Func(name, args)
        }
        Token::Colon => Tok::Delim(':'),
        Token::Semicolon => Tok::Delim(';'),
        _ => return None,
    })
}

fn collect_tokens_at(p: &mut Parser, depth: u32) -> Vec<Tok> {
    let mut out = Vec::new();
    while let Ok(t) = p.next() {
        let t = t.clone();
        if let Some(tok) = tok_of(&t, p, depth) {
            out.push(tok);
        }
    }
    out
}

/// Parses the declarations of a block or `style=""` attribute.
pub fn parse_declarations(src: &str) -> Vec<Declaration> {
    let mut input = ParserInput::new(src);
    let mut p = Parser::new(&mut input);
    declarations(&mut p)
}

fn declarations(p: &mut Parser) -> Vec<Declaration> {
    let mut out = Vec::new();
    loop {
        p.skip_whitespace();
        if p.is_exhausted() {
            break;
        }
        let r: Result<Option<Declaration>, cssparser::ParseError<'_, ()>> =
            p.parse_until_after(Delimiter::Semicolon, |p| {
                let name = match p.next()? {
                    Token::Ident(n) => n.to_ascii_lowercase(),
                    _ => return Ok(None),
                };
                p.expect_colon()?;
                let mut value = collect_tokens_at(p, 0);
                let mut important = false;
                let n = value.len();
                if n >= 2 && value[n - 2] == Tok::Delim('!') && matches!(&value[n - 1], Tok::Ident(i) if i.eq_ignore_ascii_case("important")) {
                    important = true;
                    value.truncate(n - 2);
                }
                Ok(Some(Declaration { name, value, important }))
            });
        if let Ok(Some(d)) = r {
            out.push(d);
        }
    }
    out
}

/// True when a `@media` prelude applies to an e-reader screen.
pub fn media_applies(prelude: &str) -> bool {
    let q = prelude.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }
    q.split(',').any(|one| {
        let one = one.trim();
        if one.starts_with("not ") {
            return false;
        }
        let one = one.strip_prefix("only ").unwrap_or(one).trim();
        let ty = one.split(|c: char| c.is_whitespace() || c == '(').next().unwrap_or("");
        matches!(ty, "" | "all" | "screen" | "amzn-kf8" | "handheld" | "and")
    })
}

/// Parses a stylesheet. Never fails: invalid parts are skipped.
pub fn parse_stylesheet(src: &str) -> Stylesheet {
    let mut sheet = Stylesheet::default();
    let mut input = ParserInput::new(src);
    let mut p = Parser::new(&mut input);
    rules_into(&mut p, &mut sheet, 0);
    sheet
}

/// Rules of a stylesheet or of a `@media`/`@supports` block at nesting `depth`. Blocks
/// nested deeper than [`MAX_NESTING`] are skipped.
fn rules_into(p: &mut Parser, sheet: &mut Stylesheet, depth: u32) {
    loop {
        p.skip_whitespace();
        let start = p.position();
        let t = match p.next_including_whitespace_and_comments() {
            Ok(t) => t.clone(),
            Err(_) => break,
        };
        match t {
            Token::WhiteSpace(_) | Token::Comment(_) | Token::CDO | Token::CDC | Token::Semicolon => continue,
            Token::AtKeyword(name) => {
                let name = name.to_ascii_lowercase();
                let pre_start = p.position();
                // Prelude up to `{` or `;`.
                let (prelude_end, has_block) = prelude_until_block(p);
                let prelude = p.slice(pre_start..prelude_end).to_string();
                match (name.as_str(), has_block) {
                    ("import", false) => {
                        if let Some(url) = import_url(&prelude) {
                            sheet.imports.push(url);
                        }
                    }
                    ("media", true) => {
                        let apply = media_applies(&prelude) && depth < MAX_NESTING;
                        let _ = p.parse_nested_block(|p| -> Result<(), cssparser::ParseError<'_, ()>> {
                            if apply {
                                rules_into(p, sheet, depth + 1);
                            }
                            Ok(())
                        });
                    }
                    ("supports", true) => {
                        let _ = p.parse_nested_block(|p| -> Result<(), cssparser::ParseError<'_, ()>> {
                            if depth < MAX_NESTING {
                                rules_into(p, sheet, depth + 1);
                            }
                            Ok(())
                        });
                    }
                    ("font-face", true) => {
                        let decls = p
                            .parse_nested_block(|p| -> Result<Vec<Declaration>, cssparser::ParseError<'_, ()>> { Ok(declarations(p)) })
                            .unwrap_or_default();
                        if let Some(ff) = font_face(&decls) {
                            sheet.font_faces.push(ff);
                        }
                    }
                    (_, true) => {
                        let _ = p.parse_nested_block(|_| -> Result<(), cssparser::ParseError<'_, ()>> { Ok(()) });
                    }
                    _ => {}
                }
            }
            _ => {
                // Qualified rule: prelude up to the `{` block.
                let (end, found) = prelude_until_block(p);
                if !found {
                    break;
                }
                let prelude = p.slice(start..end).to_string();
                let decls = p
                    .parse_nested_block(|p| -> Result<Vec<Declaration>, cssparser::ParseError<'_, ()>> { Ok(declarations(p)) })
                    .unwrap_or_default();
                let selectors = parse_selector_list(&prelude);
                if !selectors.is_empty() && !decls.is_empty() {
                    sheet.rules.push(StyleRule { selectors, decls });
                }
            }
        }
    }
}

/// Consumes tokens up to a `{}` block (true) or `;` / the end (false). Returns the
/// position after the last prelude token (nested blocks are consumed whole).
fn prelude_until_block(p: &mut Parser) -> (cssparser::SourcePosition, bool) {
    let mut end = p.position();
    loop {
        match p.next() {
            Ok(Token::CurlyBracketBlock) => return (end, true),
            Ok(Token::Semicolon) | Err(_) => return (end, false),
            Ok(Token::Function(_)) | Ok(Token::ParenthesisBlock) | Ok(Token::SquareBracketBlock) => {
                let _ = p.parse_nested_block(|_| -> Result<(), cssparser::ParseError<'_, ()>> { Ok(()) });
                end = p.position();
            }
            Ok(_) => end = p.position(),
        }
    }
}

fn import_url(prelude: &str) -> Option<String> {
    let mut input = ParserInput::new(prelude);
    let mut p = Parser::new(&mut input);
    let first = collect_tokens_at(&mut p, 0);
    let url = match first.first()? {
        Tok::Url(u) | Tok::Str(u) => u.clone(),
        _ => return None,
    };
    // A media list after the URL.
    let rest = prelude.trim();
    if let Some(pos) = rest.rfind(')').or_else(|| rest.rfind('"')).or_else(|| rest.rfind('\'')) {
        let media = &rest[pos + 1..];
        if !media.trim().is_empty() && !media_applies(media) {
            return None;
        }
    }
    Some(url)
}

fn font_face(decls: &[Declaration]) -> Option<FontFaceRule> {
    let mut ff = FontFaceRule { family: String::new(), bold: false, italic: false, sources: Vec::new() };
    for d in decls {
        match d.name.as_str() {
            "font-family" => ff.family = family_names(&d.value).into_iter().next().unwrap_or_default(),
            "font-weight" => ff.bold = parse_weight(&d.value).map(|w| w >= 600).unwrap_or(false),
            "font-style" => ff.italic = matches!(d.value.first(), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case("italic") || s.eq_ignore_ascii_case("oblique")),
            "src" => {
                for t in &d.value {
                    if let Tok::Url(u) = t {
                        ff.sources.push(u.clone());
                    }
                }
            }
            _ => {}
        }
    }
    (!ff.family.is_empty() && !ff.sources.is_empty()).then_some(ff)
}

/// `font-weight` as a number (normal = 400, bold = 700, bolder = 700, lighter = 300).
pub fn parse_weight(v: &[Tok]) -> Option<u32> {
    match v.first()? {
        Tok::Num(n) => Some(*n as u32),
        Tok::Ident(s) => match s.to_ascii_lowercase().as_str() {
            "normal" => Some(400),
            "bold" | "bolder" => Some(700),
            "lighter" => Some(300),
            _ => None,
        },
        _ => None,
    }
}

/// Family names of a `font-family` value (unquoted, in order).
pub fn family_names(v: &[Tok]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for t in v {
        match t {
            Tok::Comma => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
            }
            Tok::Ident(s) | Tok::Str(s) => {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(s);
            }
            _ => {}
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

// ---------------------------------------------------------------------------------------
// Selectors

/// Splits on top-level commas and parses each selector; unsupported ones are dropped.
pub fn parse_selector_list(src: &str) -> Vec<Selector> {
    split_top(src, ',').into_iter().filter_map(|s| parse_selector(s.trim())).collect()
}

fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '"' | '\'' => quote = Some(c),
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                c if c == sep && depth == 0 => {
                    out.push(&s[start..i]);
                    start = i + c.len_utf8();
                }
                _ => {}
            },
        }
    }
    out.push(&s[start..]);
    out
}

struct SelParser<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
    /// `:not()` nesting depth.
    depth: u32,
}

/// Selectors with more compounds than this are dropped (bounds the matcher's recursion).
pub const MAX_COMPOUNDS: usize = 64;

fn is_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80 || c == b'\\'
}

impl SelParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn skip_ws(&mut self) -> bool {
        let start = self.i;
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')) {
            self.i += 1;
        }
        self.i > start
    }

    fn name(&mut self) -> Option<String> {
        let start = self.i;
        while let Some(c) = self.peek() {
            if c == b'\\' {
                self.i += 2;
                continue;
            }
            if !is_name_char(c) {
                break;
            }
            self.i += 1;
        }
        (self.i > start).then(|| unescape(&self.src[start..self.i.min(self.s.len())]))
    }

    fn compound(&mut self) -> Option<Compound> {
        let mut c = Compound::default();
        let mut any = false;
        // Type or universal, possibly with a namespace prefix.
        if self.peek() == Some(b'*') {
            self.i += 1;
            any = true;
            if self.peek() == Some(b'|') {
                self.i += 1;
                if self.peek() == Some(b'*') {
                    self.i += 1;
                } else {
                    c.tag = Some(self.name()?.to_ascii_lowercase());
                }
            }
        } else if self.peek().is_some_and(is_name_char) {
            let n = self.name()?;
            any = true;
            if self.peek() == Some(b'|') && self.s.get(self.i + 1) != Some(&b'=') {
                self.i += 1;
                if self.peek() == Some(b'*') {
                    self.i += 1;
                } else {
                    c.tag = Some(self.name()?.to_ascii_lowercase());
                }
            } else {
                c.tag = Some(n.to_ascii_lowercase());
            }
        }
        loop {
            match self.peek() {
                Some(b'#') => {
                    self.i += 1;
                    c.ids.push(self.name()?);
                }
                Some(b'.') => {
                    self.i += 1;
                    c.classes.push(self.name()?);
                }
                Some(b'[') => {
                    self.i += 1;
                    c.attrs.push(self.attr()?);
                }
                Some(b':') => {
                    self.i += 1;
                    if self.peek() == Some(b':') {
                        return None; // pseudo-element
                    }
                    let name = self.name()?.to_ascii_lowercase();
                    let pseudo = match name.as_str() {
                        "first-child" => Pseudo::FirstChild,
                        "last-child" => Pseudo::LastChild,
                        "only-child" => Pseudo::OnlyChild,
                        "first-of-type" => Pseudo::FirstOfType,
                        "root" => Pseudo::Root,
                        "link" | "any-link" => Pseudo::Link,
                        "not" => {
                            if self.peek() != Some(b'(') || self.depth >= MAX_NESTING {
                                return None;
                            }
                            self.i += 1;
                            self.skip_ws();
                            self.depth += 1;
                            let inner = self.compound();
                            self.depth -= 1;
                            let inner = inner?;
                            self.skip_ws();
                            if self.peek() != Some(b')') {
                                return None;
                            }
                            self.i += 1;
                            Pseudo::Not(Box::new(inner))
                        }
                        // Old single-colon pseudo-elements and anything else.
                        _ => return None,
                    };
                    c.pseudos.push(pseudo);
                }
                _ => break,
            }
            any = true;
        }
        any.then_some(c)
    }

    fn attr(&mut self) -> Option<AttrSel> {
        self.skip_ws();
        let mut name = if self.peek() == Some(b'*') {
            self.i += 1;
            String::new()
        } else {
            self.name()?
        };
        if self.peek() == Some(b'|') && self.s.get(self.i + 1) != Some(&b'=') {
            self.i += 1;
            name = self.name()?;
        }
        let name = name.to_ascii_lowercase();
        self.skip_ws();
        let op_start = self.i;
        let op = match self.peek()? {
            b']' => {
                self.i += 1;
                return Some(AttrSel { name, op: AttrOp::Exists, case_insensitive: false });
            }
            b'=' => {
                self.i += 1;
                "="
            }
            b'~' | b'|' | b'^' | b'$' | b'*' if self.s.get(self.i + 1) == Some(&b'=') => {
                self.i += 2;
                &self.src[op_start..op_start + 2]
            }
            _ => return None,
        };
        self.skip_ws();
        let value = match self.peek()? {
            q @ (b'"' | b'\'') => {
                self.i += 1;
                let start = self.i;
                while self.peek()? != q {
                    self.i += 1;
                }
                let v = unescape(&self.src[start..self.i]);
                self.i += 1;
                v
            }
            _ => self.name()?,
        };
        self.skip_ws();
        let mut ci = false;
        if matches!(self.peek(), Some(b'i' | b'I')) {
            self.i += 1;
            ci = true;
            self.skip_ws();
        } else if matches!(self.peek(), Some(b's' | b'S')) {
            self.i += 1;
            self.skip_ws();
        }
        if self.peek()? != b']' {
            return None;
        }
        self.i += 1;
        let op = match op {
            "=" => AttrOp::Eq(value),
            "~=" => AttrOp::Includes(value),
            "|=" => AttrOp::DashMatch(value),
            "^=" => AttrOp::Prefix(value),
            "$=" => AttrOp::Suffix(value),
            _ => AttrOp::Substring(value),
        };
        Some(AttrSel { name, op, case_insensitive: ci })
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut hex = String::new();
        while hex.len() < 6 && chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
            hex.push(chars.next().unwrap());
        }
        if hex.is_empty() {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            if chars.peek() == Some(&' ') {
                chars.next();
            }
            if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                out.push(ch);
            }
        }
    }
    out
}

fn compound_specificity(c: &Compound) -> (u32, u32, u32) {
    let mut a = c.ids.len() as u32;
    let mut b = (c.classes.len() + c.attrs.len()) as u32;
    let mut t = c.tag.is_some() as u32;
    for p in &c.pseudos {
        match p {
            Pseudo::Not(inner) => {
                let (x, y, z) = compound_specificity(inner);
                a += x;
                b += y;
                t += z;
            }
            _ => b += 1,
        }
    }
    (a, b, t)
}

/// Parses one complex selector; `None` when it uses unsupported syntax.
pub fn parse_selector(src: &str) -> Option<Selector> {
    if src.is_empty() {
        return None;
    }
    let mut p = SelParser { s: src.as_bytes(), src, i: 0, depth: 0 };
    let mut left_to_right: Vec<(Combinator, Compound)> = Vec::new();
    let mut comb = Combinator::Descendant;
    p.skip_ws();
    loop {
        let c = p.compound()?;
        left_to_right.push((comb, c));
        if left_to_right.len() > MAX_COMPOUNDS {
            return None;
        }
        let ws = p.skip_ws();
        match p.peek() {
            None => break,
            Some(b'>') => {
                p.i += 1;
                comb = Combinator::Child;
                p.skip_ws();
            }
            Some(b'+') => {
                p.i += 1;
                comb = Combinator::NextSibling;
                p.skip_ws();
            }
            Some(b'~') => {
                p.i += 1;
                comb = Combinator::SubsequentSibling;
                p.skip_ws();
            }
            Some(_) if ws => comb = Combinator::Descendant,
            Some(_) => return None,
        }
    }
    let (mut a, mut b, mut t) = (0, 0, 0);
    for (_, c) in &left_to_right {
        let (x, y, z) = compound_specificity(c);
        a += x;
        b += y;
        t += z;
    }
    // parts right to left; the combinator stored with part i links it to part i+1 (its left).
    let n = left_to_right.len();
    let mut parts = Vec::with_capacity(n);
    for i in (0..n).rev() {
        let comb_to_left = left_to_right[i].0;
        parts.push((left_to_right[i].1.clone(), comb_to_left));
    }
    Some(Selector { parts, specificity: (a.min(1023) << 20) | (b.min(1023) << 10) | t.min(1023) })
}

/// What the matcher needs to know about an element.
pub trait SelectorElement: Sized + Copy {
    /// Lowercase local name.
    fn local_name(&self) -> &str;
    /// Attribute value by local name (lowercase; prefixes such as `epub:` ignored).
    fn attr(&self, name: &str) -> Option<&str>;
    /// Parent element (not crossing the chapter root's parent).
    fn parent(&self) -> Option<Self>;
    /// Previous element sibling.
    fn prev_sibling(&self) -> Option<Self>;
    /// Next element sibling (may be unknown during parsing: `None`).
    fn next_sibling(&self) -> Option<Self>;
    /// True for the root element.
    fn is_root(&self) -> bool;
    /// `id` attribute.
    fn id(&self) -> Option<&str> {
        self.attr("id")
    }
    /// True when `class` contains `c`.
    fn has_class(&self, c: &str) -> bool {
        self.attr("class").is_some_and(|v| v.split_ascii_whitespace().any(|x| x == c))
    }
}

fn attr_matches(a: &AttrSel, v: Option<&str>) -> bool {
    let Some(v) = v else { return false };
    let owned;
    let v = if a.case_insensitive {
        owned = v.to_lowercase();
        owned.as_str()
    } else {
        v
    };
    let norm = |s: &str| if a.case_insensitive { s.to_lowercase() } else { s.to_string() };
    match &a.op {
        AttrOp::Exists => true,
        AttrOp::Eq(x) => v == norm(x),
        AttrOp::Includes(x) => v.split_ascii_whitespace().any(|w| w == norm(x)),
        AttrOp::DashMatch(x) => {
            let x = norm(x);
            v == x || v.starts_with(&format!("{x}-"))
        }
        AttrOp::Prefix(x) => !x.is_empty() && v.starts_with(&norm(x)),
        AttrOp::Suffix(x) => !x.is_empty() && v.ends_with(&norm(x)),
        AttrOp::Substring(x) => !x.is_empty() && v.contains(&norm(x)),
    }
}

/// True when `e` matches compound `c`.
pub fn matches_compound<E: SelectorElement>(c: &Compound, e: E) -> bool {
    if let Some(t) = &c.tag {
        if !e.local_name().eq_ignore_ascii_case(t) {
            return false;
        }
    }
    if !c.ids.is_empty() && !c.ids.iter().all(|i| e.id() == Some(i.as_str())) {
        return false;
    }
    if !c.classes.iter().all(|cl| e.has_class(cl)) {
        return false;
    }
    if !c.attrs.iter().all(|a| attr_matches(a, e.attr(&a.name))) {
        return false;
    }
    c.pseudos.iter().all(|p| match p {
        Pseudo::FirstChild => e.prev_sibling().is_none(),
        Pseudo::LastChild => e.next_sibling().is_none(),
        Pseudo::OnlyChild => e.prev_sibling().is_none() && e.next_sibling().is_none(),
        Pseudo::FirstOfType => {
            let mut s = e.prev_sibling();
            while let Some(x) = s {
                if x.local_name() == e.local_name() {
                    return false;
                }
                s = x.prev_sibling();
            }
            true
        }
        Pseudo::Root => e.is_root(),
        Pseudo::Link => e.local_name() == "a" && e.attr("href").is_some(),
        Pseudo::Not(inner) => !matches_compound(inner, e),
    })
}

/// True when `e` matches selector `s`.
pub fn matches<E: SelectorElement>(s: &Selector, e: E) -> bool {
    matches_from(s, 0, e) == Match::Yes
}

/// Result of matching the selector from part `i` on (the failure kinds of Servo's
/// `selectors` crate). They tell a caller further right where retrying can still
/// succeed, so a descendant combinator never re-walks ancestors that already failed:
/// matching is linear in the depth instead of exponential in the number of parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Match {
    Yes,
    /// The compound did not match: try the next candidate of the caller.
    RestartFromClosestLaterSibling,
    /// Only a candidate of a descendant combinator further right can still match.
    RestartFromClosestDescendant,
    /// No candidate further right can match.
    NoGlobally,
}

fn matches_from<E: SelectorElement>(s: &Selector, i: usize, e: E) -> Match {
    let (c, comb) = &s.parts[i];
    if !matches_compound(c, e) {
        return Match::RestartFromClosestLaterSibling;
    }
    if i + 1 == s.parts.len() {
        return Match::Yes;
    }
    let next_of = |x: E| match comb {
        Combinator::Child | Combinator::Descendant => x.parent(),
        Combinator::NextSibling | Combinator::SubsequentSibling => x.prev_sibling(),
    };
    let not_found = match comb {
        Combinator::NextSibling | Combinator::SubsequentSibling => Match::RestartFromClosestDescendant,
        Combinator::Child | Combinator::Descendant => Match::NoGlobally,
    };
    let mut next = next_of(e);
    loop {
        let Some(x) = next else { return not_found };
        let r = matches_from(s, i + 1, x);
        match (r, comb) {
            (Match::Yes, _) | (Match::NoGlobally, _) | (_, Combinator::NextSibling) => return r,
            (_, Combinator::Child) => return Match::RestartFromClosestDescendant,
            (Match::RestartFromClosestDescendant, Combinator::SubsequentSibling) => return r,
            _ => {}
        }
        next = next_of(x);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stylesheet_rules_media_imports_fontface() {
        let css = r#"@charset "utf-8"; @import url("a.css"); @import "b.css" print;
            p, div.x > span { margin: 0 1em; color: red !important }
            @media print { p { color: blue } }
            @media screen, amzn-kf8 { h1 { font-size: 2em } }
            @media amzn-mobi { h2 { x: y } }
            @font-face { font-family: "My Font"; font-weight: bold; src: url(fonts/a.ttf) format("truetype"), url('b.otf'); }
            p::first-letter { float: left }
            broken { ; ; color: green; }
            @page { margin: 0 }
            a:hover { x: y }"#;
        let s = parse_stylesheet(css);
        assert_eq!(s.imports, vec!["a.css".to_string()]);
        assert_eq!(s.rules.len(), 3, "{:#?}", s.rules);
        assert_eq!(s.rules[0].selectors.len(), 2);
        assert_eq!(s.rules[0].decls[0].name, "margin");
        assert_eq!(s.rules[0].decls[0].value, vec![Tok::Num(0.0), Tok::Dim(1.0, "em".into())]);
        assert!(s.rules[0].decls[1].important);
        assert_eq!(s.rules[1].decls[0].value, vec![Tok::Dim(2.0, "em".into())]);
        assert_eq!(s.rules[2].decls[0].name, "color");
        assert_eq!(s.font_faces.len(), 1);
        assert_eq!(s.font_faces[0].family, "My Font");
        assert!(s.font_faces[0].bold && !s.font_faces[0].italic);
        assert_eq!(s.font_faces[0].sources, vec!["fonts/a.ttf".to_string(), "b.otf".to_string()]);
    }

    #[test]
    fn selectors_and_specificity() {
        let s = parse_selector("div#main > p.note + span[epub|type~=\"noteref\"]:first-child").unwrap();
        assert_eq!(s.parts.len(), 3);
        assert_eq!(s.parts[0].0.tag.as_deref(), Some("span"));
        assert_eq!(s.parts[0].1, Combinator::NextSibling);
        assert_eq!(s.parts[1].1, Combinator::Child);
        assert_eq!(s.specificity, (1 << 20) | (3 << 10) | 3);
        assert!(parse_selector("p::before").is_none());
        assert!(parse_selector("a:hover").is_none());
        assert!(parse_selector("p:not(.x)").is_some());
        assert_eq!(parse_selector_list("h1, h2 ,  p:first-letter").len(), 2);
        assert_eq!(parse_selector("*").unwrap().specificity, 0);
        let a = parse_selector("[lang|=en]").unwrap();
        assert_eq!(a.parts[0].0.attrs[0].op, AttrOp::DashMatch("en".into()));
    }

    #[test]
    fn media_queries() {
        assert!(media_applies("screen"));
        assert!(media_applies("only screen and (min-width: 100px)"));
        assert!(media_applies("print, amzn-kf8"));
        assert!(!media_applies("print"));
        assert!(!media_applies("amzn-mobi"));
        assert!(!media_applies("not screen"));
        assert!(media_applies("(orientation: portrait)"));
    }

    #[test]
    fn declarations_and_values() {
        let d = parse_declarations("font: italic bold 12pt/1.5 'Book Font', serif; margin:auto;;bogus");
        assert_eq!(d.len(), 2);
        assert_eq!(family_names(&d[0].value[4..]), vec!["Book Font".to_string(), "serif".to_string()]);
        assert_eq!(parse_weight(&[Tok::Num(600.0)]), Some(600));
    }
}
