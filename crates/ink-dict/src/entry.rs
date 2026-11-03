//! Structured entries: pronunciation, part-of-speech groups, numbered senses with
//! examples, and labelled sections (origin, derivatives, phrases), read from the markup
//! of three known dictionary styles:
//!
//! - **Apple** (the macOS New Oxford American Dictionary converted to StarDict, type `h`):
//!   spans with classes (`hg`/`ph` head and pronunciation, `se1` part-of-speech blocks,
//!   `msDict` senses with `df` and `eg`/`ex`, `x_xo0` sections labelled by `x_xoLblBlk`).
//! - **XDXF** (Oxford Advanced Learner's, type `x`): head lines with `<c c="orange">`
//!   parts of speech and `<c c="darkcyan">[…]</c>` pronunciations, then `<blockquote>`
//!   lines whose depth says what they are (1 sense, 2 example, 3 section label or text).
//! - **GCIDE** (GNU Collaborative International Dictionary, type `h`): one `<p>` per
//!   block: `<b>Word</b>, <i>n.</i>` opens a part of speech, `<b>2.</b>` a sense, a
//!   paragraph in italics alone is a quotation, `<b>Term</b>` a compound.
//!
//! [`parse`] returns `None` for other markup; the caller keeps the plain text then.

use crate::text::decode_entity;

/// A parsed entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// Pronunciation without delimiters.
    pub pron: Option<String>,
    /// Part-of-speech blocks in order.
    pub groups: Vec<Group>,
    /// Labelled sections after the senses.
    pub sections: Vec<Section>,
}

/// One part of speech and its senses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Group {
    /// Part of speech with its grammar note (`noun (plural mutinies)`); may be empty.
    pub pos: String,
    /// Senses in order.
    pub senses: Vec<Sense>,
}

/// One sense.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sense {
    /// Definition text.
    pub def: String,
    /// Example phrases and quotations.
    pub examples: Vec<String>,
    /// Further lines (synonyms, notes).
    pub notes: Vec<String>,
    /// A subsense of the sense before it.
    pub sub: bool,
}

/// A labelled section (`Origin`, `Derivatives`, `Example Bank`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Section {
    /// Label, capitalized (`Origin`).
    pub label: String,
    /// Lines: (bold term, text); the term may be empty.
    pub lines: Vec<(String, String)>,
}

impl Entry {
    /// The senses that are not subsenses, with their group's part of speech.
    pub fn main_senses(&self) -> impl Iterator<Item = (&str, &Sense)> {
        self.groups.iter().flat_map(|g| g.senses.iter().filter(|s| !s.sub).map(move |s| (g.pos.as_str(), s)))
    }
}

/// Parses `raw` (one field of type `kind`); `None` when the markup is none of the
/// known styles or yields no sense.
pub fn parse(kind: char, raw: &str) -> Option<Entry> {
    let e = match kind {
        'h' if raw.contains("class=\"se1") || raw.contains("class=\"msDict") => apple(raw),
        'h' if raw.starts_with("<p>") => gcide(raw),
        'x' if raw.contains("<blockquote>") => xdxf(raw),
        _ => return None,
    };
    e.filter(|e| e.groups.iter().any(|g| !g.senses.is_empty()))
}

// ---------------------------------------------------------------------------------------
// Tokens

#[derive(Debug)]
enum Tok<'a> {
    Text(String),
    Open { name: String, attrs: &'a str },
    Close(String),
}

const VOID: &[&str] = &["br", "img", "hr", "meta", "link", "input", "wbr"];

fn tokens(s: &str) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let mut rest = s;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            out.push(Tok::Text(decode(rest)));
            break;
        };
        if lt > 0 {
            out.push(Tok::Text(decode(&rest[..lt])));
        }
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            out.push(Tok::Text(decode(&rest[lt..])));
            break;
        };
        let tag = &after[..gt];
        rest = &after[gt + 1..];
        if tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }
        let closing = tag.starts_with('/');
        let body = tag.trim_start_matches('/');
        let name: String = body.chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        if closing {
            out.push(Tok::Close(name));
        } else {
            let self_closing = tag.ends_with('/') || VOID.contains(&name.as_str());
            out.push(Tok::Open { name: name.clone(), attrs: &body[name.len()..] });
            if self_closing {
                out.push(Tok::Close(name));
            }
        }
    }
    out
}

fn decode(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        match after.find(';').filter(|n| *n <= 10).and_then(|n| decode_entity(&after[..n]).map(|c| (n, c))) {
            Some((n, c)) => {
                out.push_str(&c);
                rest = &after[n + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of attribute `key` in a tag's attribute text.
fn attr<'a>(attrs: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("{key}=\"");
    let a = attrs.find(&pat)? + pat.len();
    let len = attrs[a..].find('"')?;
    Some(&attrs[a..a + len])
}

/// Collapses white space and trims.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Trims spaces and the punctuation that joins parts (`:`, `;`, `,`, `|`).
fn trim_joins(s: &str) -> String {
    squash(s).trim_matches(|c: char| c.is_whitespace() || matches!(c, ':' | ';' | ',' | '|')).trim().to_string()
}

/// Capitalizes a section label: `DERIVATIVES` -> `Derivatives`, `Word Origin` stays.
fn label(s: &str) -> String {
    let s = squash(s).trim_end_matches(':').trim().to_string();
    if s.chars().any(|c| c.is_lowercase()) {
        return s;
    }
    let lower = s.to_lowercase();
    let mut c = lower.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn push_section(e: &mut Entry, s: Section) {
    if !s.lines.is_empty() && !e.sections.contains(&s) {
        e.sections.push(s);
    }
}

// ---------------------------------------------------------------------------------------
// Apple (NOAD)

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    None,
    Pron,
    Pos,
    Def,
    Example,
    Label,
    Term,
    Line,
}

fn apple(raw: &str) -> Option<Entry> {
    let mut e = Entry::default();
    // Open elements: their classes.
    let mut stack: Vec<String> = Vec::new();
    let mut section: Option<Section> = None;
    let mut pron = String::new();
    let mut in_head_pron = true;
    let has = |stack: &[String], c: &str| stack.iter().any(|s| s.split_whitespace().any(|x| x == c));
    for t in tokens(raw) {
        match t {
            Tok::Open { name, attrs } => {
                let class = attr(attrs, "class").unwrap_or("").to_string();
                let cls: Vec<&str> = class.split_whitespace().collect();
                let in_sg = has(&stack, "sg") || has(&stack, "se1");
                if cls.contains(&"se1") {
                    e.groups.push(Group::default());
                } else if cls.contains(&"msDict") && in_sg {
                    if e.groups.is_empty() {
                        e.groups.push(Group::default());
                    }
                    let sub = cls.contains(&"t_subsense") || has(&stack, "msDict");
                    e.groups.last_mut().expect("pushed").senses.push(Sense { sub, ..Sense::default() });
                } else if cls.contains(&"eg") && in_sg {
                    if let Some(s) = e.groups.last_mut().and_then(|g| g.senses.last_mut()) {
                        s.examples.push(String::new());
                    }
                } else if cls.contains(&"x_xo0") && !in_sg {
                    if let Some(s) = section.take() {
                        push_section(&mut e, s);
                    }
                    section = Some(Section::default());
                } else if let (true, Some(sec)) = (cls.contains(&"x_xo1"), section.as_mut()) {
                    sec.lines.push((String::new(), String::new()));
                }
                if cls.contains(&"se1") || cls.contains(&"sg") {
                    in_head_pron = false;
                }
                stack.push(if name == "br" { String::new() } else { class });
            }
            Tok::Close(_) => {
                stack.pop();
            }
            Tok::Text(text) => {
                let slot = if has(&stack, "x_xoLblBlk") {
                    Slot::Label
                } else if section.is_some() && has(&stack, "x_xo0") {
                    // Sections keep their punctuation (`gp`): `late 16th century: from …`.
                    if has(&stack, "hsb") {
                        Slot::None
                    } else if has(&stack, "l") && !has(&stack, "msDict") {
                        Slot::Term
                    } else {
                        Slot::Line
                    }
                } else if has(&stack, "se1") && !has(&stack, "msDict") && (has(&stack, "posg") || has(&stack, "x_xdh")) && !has(&stack, "sn") {
                    // The part of speech is often in a `gp` span itself.
                    Slot::Pos
                } else if has(&stack, "gp") || has(&stack, "sn") || has(&stack, "hsb") {
                    Slot::None
                } else if has(&stack, "hg") {
                    if has(&stack, "ph") && in_head_pron {
                        Slot::Pron
                    } else {
                        Slot::None
                    }
                } else if has(&stack, "msDict") {
                    if has(&stack, "eg") {
                        Slot::Example
                    } else {
                        Slot::Def
                    }
                } else {
                    Slot::None
                };
                match slot {
                    Slot::None => {}
                    Slot::Pron => {
                        if !pron.is_empty() && !pron.ends_with(", ") {
                            pron.push_str(", ");
                        }
                        pron.push_str(text.trim());
                    }
                    Slot::Pos => {
                        if let Some(g) = e.groups.last_mut() {
                            g.pos.push_str(&text);
                        }
                    }
                    Slot::Def | Slot::Example => {
                        if let Some(s) = e.groups.last_mut().and_then(|g| g.senses.last_mut()) {
                            match (slot, s.examples.last_mut()) {
                                (Slot::Example, Some(x)) => x.push_str(&text),
                                _ => s.def.push_str(&text),
                            }
                        }
                    }
                    Slot::Label => {
                        if let Some(s) = section.as_mut() {
                            s.label.push_str(&text);
                        }
                    }
                    Slot::Term | Slot::Line => {
                        if let Some(s) = section.as_mut() {
                            if s.lines.is_empty() {
                                s.lines.push((String::new(), String::new()));
                            }
                            let l = s.lines.last_mut().expect("pushed");
                            if slot == Slot::Term {
                                l.0.push_str(&text)
                            } else {
                                l.1.push_str(&text)
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(s) = section.take() {
        push_section(&mut e, s);
    }
    e.pron = Some(trim_joins(&pron)).filter(|p| !p.is_empty());
    for g in &mut e.groups {
        g.pos = squash(&g.pos);
        for s in &mut g.senses {
            s.def = trim_joins(&s.def);
            s.examples = s.examples.iter().map(|x| trim_joins(x)).filter(|x| !x.is_empty()).collect();
        }
        g.senses.retain(|s| !s.def.is_empty() || !s.examples.is_empty());
    }
    e.groups.retain(|g| !g.senses.is_empty());
    for s in &mut e.sections {
        s.label = label(&s.label);
        s.lines = s.lines.iter().map(|(t, l)| (squash(t), squash(l))).filter(|(t, l)| !t.is_empty() || !l.is_empty()).collect();
    }
    e.sections.retain(|s| !s.lines.is_empty());
    Some(e)
}

// ---------------------------------------------------------------------------------------
// XDXF (OALD)

/// Plain text of a markup line: tags dropped, `rref` (sound files) and `↑` dropped.
fn xdxf_text(s: &str) -> String {
    let mut out = String::new();
    let mut skip = 0usize;
    for t in tokens(s) {
        match t {
            Tok::Open { name, .. } if name == "rref" => skip += 1,
            Tok::Close(name) if name == "rref" => skip = skip.saturating_sub(1),
            Tok::Text(t) if skip == 0 => out.push_str(&t),
            _ => {}
        }
    }
    squash(&out.replace(['↑', '▪'], ""))
}

/// Texts of the `<c c="color">` elements of a line.
fn xdxf_colored(s: &str, color: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth: Option<usize> = None;
    let mut open = 0usize;
    for t in tokens(s) {
        match t {
            Tok::Open { name, attrs } if name == "c" => {
                open += 1;
                if depth.is_none() && attr(attrs, "c") == Some(color) {
                    depth = Some(open);
                    out.push(String::new());
                }
            }
            Tok::Open { .. } => {}
            Tok::Close(name) if name == "c" => {
                if depth == Some(open) {
                    depth = None;
                }
                open = open.saturating_sub(1);
            }
            Tok::Close(_) => {}
            Tok::Text(t) => {
                if depth.is_some() {
                    out.last_mut().expect("pushed").push_str(&t);
                }
            }
        }
    }
    out.into_iter().map(|s| squash(&s)).filter(|s| !s.is_empty()).collect()
}

/// Strips a leading sense number (`1.`, `1 `, `2)`).
fn strip_number(s: &str) -> &str {
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return s;
    }
    let rest = &s[digits..];
    match rest.strip_prefix(['.', ')']) {
        Some(r) => r.trim_start(),
        None if rest.starts_with(' ') => rest.trim_start(),
        None => s,
    }
}

fn xdxf(raw: &str) -> Option<Entry> {
    let mut e = Entry::default();
    let mut section: Option<Section> = None;
    let mut first_head = true;
    // The last part of speech of a head line (`verb`), for guide words (`RACE`).
    let mut base_pos = String::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("<k>") {
            continue;
        }
        let mut depth = 0;
        let mut rest = line;
        while let Some(r) = rest.strip_prefix("<blockquote>") {
            depth += 1;
            rest = r;
        }
        let text = xdxf_text(rest);
        if depth == 0 {
            // A head line: pronunciation (first one) and part of speech.
            if e.pron.is_none() {
                e.pron = xdxf_colored(line, "darkcyan").into_iter().find(|p| p.starts_with('[')).map(|p| p.trim_matches(['[', ']']).to_string());
            }
            let orange = xdxf_colored(line, "orange");
            let guide = xdxf_colored(line, "darkmagenta");
            let pos = if first_head && line.contains("</rref>") {
                xdxf_text(&line[line.rfind("</rref>").expect("contains") + "</rref>".len()..])
            } else if first_head {
                String::new()
            } else if orange.is_empty() && !guide.is_empty() {
                // A guide word: `verb: race`.
                let g = guide.join(" ").to_lowercase();
                if base_pos.is_empty() {
                    g
                } else {
                    format!("{base_pos}: {g}")
                }
            } else if !orange.is_empty() {
                // The part of speech and grammar only (not inflections, pronunciations or
                // the `II. house·hold` of a second entry).
                orange.iter().chain(&xdxf_colored(line, "orangered")).cloned().collect::<Vec<_>>().join(" ")
            } else {
                text.clone()
            };
            if !first_head && !orange.is_empty() {
                base_pos = orange.join(" ").trim_end_matches(',').to_string();
            }
            first_head = false;
            if !pos.is_empty() {
                if let Some(s) = section.take() {
                    push_section(&mut e, s);
                }
                match e.groups.last_mut() {
                    Some(g) if g.senses.is_empty() => g.pos = pos,
                    _ => e.groups.push(Group { pos, senses: Vec::new() }),
                }
            }
            continue;
        }
        // Cross-references to the entry itself and labels without text.
        if text.is_empty() || text.starts_with("Main entry:") || (depth > 1 && text.ends_with(':') && (depth == 2 || !rest.trim_start().starts_with("<c c=\"darkslategray\"><c>"))) {
            continue;
        }
        match depth {
            1 => {
                if let Some(s) = section.take() {
                    push_section(&mut e, s);
                }
                if e.groups.is_empty() {
                    e.groups.push(Group::default());
                }
                let def = strip_number(&text).trim_start_matches('•').trim().to_string();
                e.groups.last_mut().expect("pushed").senses.push(Sense { def, ..Sense::default() });
            }
            2 if section.is_none() => {
                let Some(s) = e.groups.last_mut().and_then(|g| g.senses.last_mut()) else { continue };
                match text.strip_prefix('•') {
                    Some(x) => s.examples.push(x.trim().to_string()),
                    None => s.notes.push(text),
                }
            }
            _ => {
                // `Word Origin:` alone, or `Idioms: …` with its text (labels are
                // `<c c="darkslategray"><c>…:</c></c>`).
                let is_label = depth >= 3 && rest.trim_start().starts_with("<c c=\"darkslategray\"><c>");
                let head = text.split_once(':').filter(|(l, _)| is_label && !l.is_empty());
                match head {
                    Some((l, r)) => {
                        if let Some(s) = section.take() {
                            push_section(&mut e, s);
                        }
                        let mut s = Section { label: label(l), lines: Vec::new() };
                        if !r.trim().is_empty() {
                            s.lines.push((String::new(), r.trim().to_string()));
                        }
                        section = Some(s);
                    }
                    None => {
                        let l = text.trim_start_matches('•').trim().to_string();
                        match section.as_mut() {
                            Some(s) => s.lines.push((String::new(), l)),
                            None => {
                                if let Some(s) = e.groups.last_mut().and_then(|g| g.senses.last_mut()) {
                                    s.notes.push(l);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(s) = section.take() {
        push_section(&mut e, s);
    }
    e.groups.retain(|g| !g.senses.is_empty());
    Some(e)
}

// ---------------------------------------------------------------------------------------
// GCIDE

/// Webster abbreviations of parts of speech.
fn gcide_pos(abbr: &str) -> String {
    let a = squash(abbr);
    let full = match a.trim_end_matches(';').trim() {
        "n." => "noun",
        "a." | "adj." => "adjective",
        "adv." => "adverb",
        "v. t." => "transitive verb",
        "v. i." => "intransitive verb",
        "v." => "verb",
        "prep." => "preposition",
        "conj." => "conjunction",
        "interj." => "interjection",
        "pron." => "pronoun",
        "p. p." => "past participle",
        "p. a." => "participial adjective",
        "n. pl." => "plural noun",
        other => other,
    };
    full.to_string()
}

/// One `<p>`: its text, and the text of its first `<b>` and first `<i>` when the
/// paragraph starts with them (`lead_b`), or is an `<i>` (`lead_i`).
struct Para {
    text: String,
    lead_b: Option<String>,
    first_i: Option<String>,
    /// Text after the first `<i>` (where the senses begin).
    after_i: String,
    lead_i: bool,
}

fn gcide_para(p: &str) -> Para {
    let mut text = String::new();
    let mut lead_b = None;
    let mut first_i: Option<String> = None;
    let mut after_i = String::new();
    let mut cur_b: Option<String> = None;
    let mut cur_i: Option<String> = None;
    let mut lead_i = false;
    let mut i_done = false;
    for t in tokens(p) {
        match t {
            Tok::Open { name, .. } if name == "b" => cur_b = Some(String::new()),
            Tok::Open { name, .. } if name == "i" => {
                if text.trim().is_empty() {
                    lead_i = true;
                }
                cur_i = Some(String::new());
            }
            Tok::Close(name) if name == "b" => {
                if let Some(b) = cur_b.take() {
                    if lead_b.is_none() && text.trim() == b.trim() {
                        lead_b = Some(squash(&b));
                    }
                }
            }
            Tok::Close(name) if name == "i" => {
                if let Some(i) = cur_i.take() {
                    // The part of speech: the first italics after the lead word that is
                    // not a pronunciation (`(?)`, `(rŭn)`).
                    if !i_done && lead_b.is_some() && !i.contains('?') && !i.contains('#') && !i.trim_start().starts_with('(') {
                        first_i = Some(squash(&i));
                        i_done = true;
                        continue;
                    }
                }
            }
            Tok::Text(t) => {
                text.push_str(&t);
                if let Some(b) = cur_b.as_mut() {
                    b.push_str(&t);
                }
                if let Some(i) = cur_i.as_mut() {
                    i.push_str(&t);
                }
                if i_done {
                    after_i.push_str(&t);
                }
            }
            _ => {}
        }
    }
    let clean = |s: &str| squash(&s.replace("(?)", "").replace("(#)", "").replace(" ,", ","));
    Para { text: clean(&text), lead_b, first_i, after_i: clean(&after_i), lead_i }
}

/// Splits the `[…]` groups (nested brackets kept together) off the text before the
/// first sense number (`1. `); returns (text without them, the groups' insides).
fn gcide_head(s: &str) -> (String, Vec<String>) {
    let mut text = String::new();
    let mut groups = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for (i, c) in s.char_indices() {
        if depth == 0 && text.ends_with("1. ") {
            text.push_str(&s[i..]);
            break;
        }
        match c {
            '[' => {
                if depth > 0 {
                    cur.push(c);
                }
                depth += 1;
            }
            ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    groups.push(squash(&std::mem::take(&mut cur)));
                } else {
                    cur.push(c);
                }
            }
            _ if depth > 0 => cur.push(c),
            _ => text.push(c),
        }
    }
    (squash(&text), groups)
}

fn gcide(raw: &str) -> Option<Entry> {
    let mut e = Entry::default();
    let mut compounds = Section { label: "Compounds".into(), lines: Vec::new() };
    let mut origin = Section { label: "Origin".into(), lines: Vec::new() };
    let mut syn = Section { label: "Synonyms".into(), lines: Vec::new() };
    for p in raw.split("<p>").map(|p| p.trim_end_matches("</p>")).filter(|p| !p.trim().is_empty()) {
        let para = gcide_para(p);
        if para.text.is_empty() {
            continue;
        }
        let num = para.lead_b.as_deref().filter(|b| b.trim_end_matches('.').chars().all(|c| c.is_ascii_digit()) && !b.is_empty());
        // `<b>Word</b>, <i>n.</i> …`: a new part of speech (not `<b>Term</b> <i>(Mech.)</i>`).
        if let (Some(_), None, Some(pos)) = (&para.lead_b, num, &para.first_i) {
            // Abbreviations only (`n.`, `v. i.`), close after the word (and its `(rŭn)`).
            let abbr = pos.ends_with('.') && pos.chars().count() <= 14 && !pos.starts_with('(');
            if abbr && para.text.find(pos.as_str()).is_some_and(|n| n < para.lead_b.as_ref().map_or(0, |b| b.len()) + 20) {
                let (rest, brackets) = gcide_head(para.after_i.trim_start_matches([',', ';', ' ']));
                // `[…]` before the first sense: inflections (`[imp. & p. p. …]`) are
                // dropped, the etymology (`[From …]`, `[OE. …]`) goes to the origin.
                for b in brackets {
                    if !["imp.", "p. p.", "p. pr.", "pl.", "Written also", "Obs."].iter().any(|k| b.starts_with(k)) {
                        origin.lines.push((String::new(), b));
                    }
                }
                let mut g = Group { pos: gcide_pos(pos), senses: Vec::new() };
                // A grammar note (`; pl. Mutinies.`) before the first sense.
                let (note, def) = match rest.find("1. ") {
                    Some(n) => (rest[..n].trim().to_string(), rest[n..].to_string()),
                    None => (String::new(), rest),
                };
                let note = note.trim_matches([';', '.', ' ']).to_string();
                if !note.is_empty() {
                    g.pos = format!("{} ({note})", g.pos);
                }
                g.senses.push(Sense { def: strip_number(&def).trim().to_string(), ..Sense::default() });
                e.groups.push(g);
                continue;
            }
        }
        if let Some(n) = num {
            let def = para.text[para.text.find(n).map_or(0, |i| i + n.len())..].trim().to_string();
            if e.groups.is_empty() {
                e.groups.push(Group::default());
            }
            e.groups.last_mut().expect("pushed").senses.push(Sense { def, ..Sense::default() });
            continue;
        }
        if para.lead_i && para.lead_b.is_none() {
            if let Some(s) = e.groups.last_mut().and_then(|g| g.senses.last_mut()) {
                s.examples.push(para.text.clone());
                continue;
            }
        }
        if let Some(b) = &para.lead_b {
            // `(b) To flee …`: part of the sense above.
            if b.starts_with('(') {
                if let Some(s) = e.groups.last_mut().and_then(|g| g.senses.last_mut()) {
                    s.notes.push(para.text.clone());
                    continue;
                }
            }
            let rest = para.text[b.len().min(para.text.len())..].trim_start_matches([',', ' ']).to_string();
            if b.starts_with("Syn") {
                syn.lines.push((String::new(), rest.trim_start_matches(['-', ' ']).to_string()));
            } else {
                // `<b>A</b>, … -- B (Law), … -- C, …`: one line per compound.
                let mut parts = rest.split(" -- ");
                compounds.lines.push((b.clone(), parts.next().unwrap_or("").trim().to_string()));
                for p in parts {
                    // ` -- called also …` inside a definition: not a new compound.
                    if !p.starts_with(|c: char| c.is_uppercase()) {
                        let last = compounds.lines.last_mut().expect("pushed");
                        last.1 = format!("{} — {}", last.1, p.trim());
                        continue;
                    }
                    let cut = p.find([',', '(']).filter(|n| *n > 0 && *n <= 40);
                    match cut {
                        Some(n) => compounds.lines.push((p[..n].trim().to_string(), p[n..].trim_start_matches([',', ' ']).to_string())),
                        None => compounds.lines.push((String::new(), p.trim().to_string())),
                    }
                }
            }
            continue;
        }
        if let Some(s) = e.groups.last_mut().and_then(|g| g.senses.last_mut()) {
            s.notes.push(para.text.clone());
        }
    }
    for s in [compounds, syn, origin] {
        push_section(&mut e, s);
    }
    for g in &mut e.groups {
        g.senses.retain(|s| !s.def.is_empty());
    }
    e.groups.retain(|g| !g.senses.is_empty());
    Some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic entries in each dictionary's markup (not dictionary text).

    #[test]
    fn apple_style() {
        let raw = r#"<div><span class="hg x_xh0"><span class="hw">blip<span class="hsb">•</span>pet </span><span class="prx"> | <span class="ph t_respell">ˈblipət</span> | </span></span><span class="sg"><span class="se1 x_xd0"><span class="posg x_xdh"><span class="pos"><span class="gp tg_pos">noun </span></span><span class="infg">(<span class="inf">blippets</span>) </span></span><span class="msDict x_xd1 t_core"><span class="sn">1</span><span class="df">a small round thing</span><span class="gp tg_df">: </span><span class="eg"><span class="ex"> a blippet on the table</span><span class="gp tg_eg">. </span></span></span><span class="msDict x_xd1 hasSn t_subsense"><span class="gp sn">  • </span><span class="df">a dot</span></span><span class="msDict x_xd1 t_core"><span class="sn">2</span><span class="df">a sound</span></span></span><span class="se1 x_xd0"><span class="posg x_xdh"><span class="pos">verb </span></span><span class="msDict x_xd1 t_core"><span class="df">make a blip</span></span></span></span><span class="subEntryBlock x_xo0 t_derivatives"><span class="gp x_xoLblBlk ty_label">DERIVATIVES </span><span class="subEntry x_xo1"><span class="x_xoh"><span class="l">blippety </span></span><span class="posg"><span class="pos">adjective</span></span></span></span><span class="etym x_xo0"><span class="gp x_xoLblBlk ty_label">ORIGIN </span><span class="x_xo1">1990s: imitative.</span></span></div>"#;
        let e = parse('h', raw).unwrap();
        assert_eq!(e.pron.as_deref(), Some("ˈblipət"));
        assert_eq!(e.groups.len(), 2);
        assert_eq!(e.groups[0].pos, "noun (blippets)");
        let s = &e.groups[0].senses;
        assert_eq!((s[0].def.as_str(), s[0].examples.clone(), s[0].sub), ("a small round thing", vec!["a blippet on the table".to_string()], false));
        assert_eq!((s[1].def.as_str(), s[1].sub), ("a dot", true));
        assert_eq!(s[2].def, "a sound");
        assert_eq!((e.groups[1].pos.as_str(), e.groups[1].senses[0].def.as_str()), ("verb", "make a blip"));
        assert_eq!(e.sections[0].label, "Derivatives");
        assert_eq!(e.sections[0].lines, vec![("blippety".to_string(), "adjective".to_string())]);
        assert_eq!(e.sections[1].label, "Origin");
        assert_eq!(e.sections[1].lines, vec![(String::new(), "1990s: imitative.".to_string())]);
        assert_eq!(e.main_senses().count(), 3);
    }

    #[test]
    fn xdxf_style() {
        let raw = "<k>blip</k>\n<b>blip</b> <c c=\"rosybrown\">[</c><c c=\"darkslategray\"><b>blips</b></c><c c=\"rosybrown\">]</c> <abr>BrE</abr> <c c=\"darkcyan\">[blɪp]</c> <rref>z_blip.wav</rref> <abr>NAmE</abr> <c c=\"darkcyan\">[blɪp]</c> <rref>z_blip_us.wav</rref> <c c=\"orange\"> noun</c> <c c=\"darkgray\"> (</c><c c=\"rosybrown\">informal</c><c c=\"darkgray\">)</c>\n<blockquote>1. a small dot of light</blockquote>\n<blockquote><blockquote><c c=\"darkcyan\"><b>Syn:</b></c> ↑<kref>dot</kref></blockquote></blockquote>\n<blockquote><blockquote><ex>• </ex><ex>a blip on the screen </ex></blockquote></blockquote>\n<blockquote>2. a short sound</blockquote>\n<blockquote><blockquote><blockquote><c c=\"darkslategray\"><c>Word Origin:</c></c></blockquote></blockquote></blockquote>\n<blockquote><blockquote><blockquote>imitative</blockquote></blockquote></blockquote>\n<c c=\"orange\"> verb</c> <c c=\"orangered\">intransitive</c>\n<blockquote>to make a blip</blockquote>\n<blockquote><blockquote><blockquote><c c=\"darkslategray\"><c>Word Origin:</c></c></blockquote></blockquote></blockquote>\n<blockquote><blockquote><blockquote>imitative</blockquote></blockquote></blockquote>";
        let e = parse('x', raw).unwrap();
        assert_eq!(e.pron.as_deref(), Some("blɪp"));
        assert_eq!(e.groups[0].pos, "noun (informal)");
        let s = &e.groups[0].senses;
        assert_eq!((s[0].def.as_str(), s[0].examples.clone(), s[0].notes.clone()), ("a small dot of light", vec!["a blip on the screen".to_string()], vec!["Syn: dot".to_string()]));
        assert_eq!(s[1].def, "a short sound");
        assert_eq!((e.groups[1].pos.as_str(), e.groups[1].senses[0].def.as_str()), ("verb intransitive", "to make a blip"));
        // The repeated origin is kept once.
        assert_eq!(e.sections.len(), 1);
        assert_eq!((e.sections[0].label.as_str(), e.sections[0].lines[0].1.as_str()), ("Word Origin", "imitative"));
    }

    #[test]
    fn gcide_style() {
        let raw = r#"<p><b style="color: #00b">Blip</b> <i>(?)</i>, <i style="color: #a00">n.</i>; <i>pl.</i> Blips. [From <span>blop</span> a drop.] <b>1.</b> A small dot.</p><p><i style="color: #33f">A <b>blip</b> on the glass.</i> <small>Anon.</small></p><p><b>2.</b> A short sound. [Obs.]</p><p><b style="color: #00b">Blip</b>, <i style="color: #a00">v. i.</i> To make a blip.</p><p><b>Blip lamp</b>, a lamp that blips.</p><p><b>Syn.</b> -- Dot.</p>"#;
        let e = parse('h', raw).unwrap();
        assert_eq!(e.groups.len(), 2);
        assert_eq!(e.groups[0].pos, "noun (pl. Blips)");
        let s = &e.groups[0].senses;
        assert_eq!((s[0].def.as_str(), s[0].examples.clone()), ("A small dot.", vec!["A blip on the glass. Anon.".to_string()]));
        assert_eq!(s[1].def, "A short sound. [Obs.]");
        assert_eq!((e.groups[1].pos.as_str(), e.groups[1].senses[0].def.as_str()), ("intransitive verb", "To make a blip."));
        let labels: Vec<&str> = e.sections.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Compounds", "Synonyms", "Origin"]);
        assert_eq!(e.sections[0].lines[0], ("Blip lamp".to_string(), "a lamp that blips.".to_string()));
        assert_eq!(e.sections[2].lines[0].1, "From blop a drop.");
    }

    #[test]
    fn other_markup_is_not_parsed() {
        assert!(parse('h', "<b>word</b> <i>noun</i><br>a thing").is_none());
        assert!(parse('m', "plain").is_none());
    }
}
