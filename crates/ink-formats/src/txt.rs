//! Plain text: encoding detection, paragraph reconstruction and chapter guessing.
//!
//! - Encoding: [`crate::encoding::decode_text`] (BOM, UTF-16 pattern, UTF-8, chardetng).
//! - Paragraphs: when blank lines are common they separate paragraphs and the hard-wrapped
//!   lines in between are joined; otherwise indented lines start paragraphs; otherwise
//!   every line is a paragraph.
//! - Chapters: short blocks that start like a heading (`CHAPTER IV.`, `Part Two`,
//!   `Глава 3`, `Kapitel 1`, `XII`, `Prologue`, ...), optionally with a subtitle line,
//!   become `<h1>` (parts, books, volumes) or `<h2>` (chapters) and start a new chapter.
//!   A run of headings with no text between them (a contents list) is kept as text.
//!   When no pattern matches, all-caps lines between blank lines are tried.
//! - Gutenberg headers (`Title:`, `Author:`, `Language:`) fill the metadata.

use crate::book::{Book, BookInfo, Target, TocNode};
use crate::encoding::{decode_text, decode_text_owned};
use crate::util::LANGUAGES;
use crate::xml::{escape_text, push_text};
use crate::Format;
use ink_doc::{DirLoader, DocError, Result};
use ink_html::HtmlChapter;
use std::borrow::Cow;
use std::fmt::Write as _;
use std::path::Path;

/// Characters of text per chapter when the book has no headings (keeps chapters small
/// for incremental pagination).
const CHUNK_CHARS: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq)]
enum Block {
    Para(String),
    Heading { level: u8, title: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Blank,
    Indent,
    Line,
}

/// `\r\n`/`\r` to `\n`, form feeds to blank lines (a copy only when needed, made in one
/// pass: a book can be tens of MB).
fn normalise(text: &str) -> Cow<'_, str> {
    if !text.bytes().any(|b| b == b'\r' || b == 0x0C) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 64);
    let mut rest = text;
    while let Some(i) = rest.bytes().position(|b| b == b'\r' || b == 0x0C) {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        rest = if rest.as_bytes()[i] == b'\r' {
            out.push('\n');
            after.strip_prefix('\n').unwrap_or(after)
        } else {
            out.push_str("\n\n");
            after
        };
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Lines of normalised text without trailing whitespace (tabs and indents kept), so a
/// blank line is an empty one.
fn lines_of(t: &str) -> Vec<&str> {
    let t = t.strip_prefix('\u{feff}').unwrap_or(t);
    t.split('\n').map(|l| l.trim_end()).collect()
}

fn mode_of(lines: &[&str]) -> Mode {
    let nonblank = lines.iter().filter(|l| !l.is_empty()).count();
    if nonblank == 0 {
        return Mode::Line;
    }
    // Runs of blank lines between text lines: the steps from a blank line to text, less
    // the one after blank lines at the start.
    let steps = lines.windows(2).filter(|w| w[0].is_empty() && !w[1].is_empty()).count();
    if (steps - usize::from(lines[0].is_empty())) * 10 >= nonblank {
        return Mode::Blank;
    }
    let indented = lines.iter().filter(|l| l.starts_with("  ") || l.starts_with('\t') || l.starts_with('\u{3000}')).count();
    if indented * 5 >= nonblank && indented < nonblank {
        Mode::Indent
    } else {
        Mode::Line
    }
}

/// Groups of lines that form one paragraph each.
fn groups<'t>(lines: &[&'t str], mode: Mode) -> Vec<Vec<&'t str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let flush = |cur: &mut Vec<&'t str>, out: &mut Vec<Vec<&'t str>>| {
        if !cur.is_empty() {
            out.push(std::mem::take(cur));
        }
    };
    for &l in lines {
        let blank = l.is_empty();
        match mode {
            Mode::Blank => {
                if blank {
                    flush(&mut cur, &mut out);
                } else {
                    cur.push(l);
                }
            }
            Mode::Indent => {
                if blank {
                    flush(&mut cur, &mut out);
                } else {
                    let indented = l.starts_with(char::is_whitespace) || l.starts_with('\u{3000}');
                    if indented {
                        flush(&mut cur, &mut out);
                    }
                    cur.push(l);
                }
            }
            Mode::Line => {
                flush(&mut cur, &mut out);
                if !blank {
                    cur.push(l);
                }
            }
        }
    }
    flush(&mut cur, &mut out);
    out
}

const PART_WORDS: &[&str] = &[
    "part", "book", "volume", "teil", "buch", "band", "partie", "livre", "tome", "parte", "libro", "deel", "boek", "часть",
    "книга", "том", "del", "bog", "bind", "osa", "część", "księga",
];
const CHAPTER_WORDS: &[&str] = &[
    "chapter", "chap.", "section", "act", "scene", "canto", "letter", "stave", "kapitel", "abschnitt", "chapitre",
    "capítulo", "capitulo", "capitolo", "hoofdstuk", "rozdział", "kapitola", "глава", "раздел", "luku", "fejezet",
    "κεφάλαιο", "章",
];
const STANDALONE: &[&str] = &[
    "prologue", "epilogue", "preface", "introduction", "foreword", "afterword", "appendix", "conclusion", "postscript",
    "vorwort", "nachwort", "einleitung", "prolog", "epilog", "préface", "avant-propos", "épilogue", "prólogo",
    "epílogo", "prefacio", "пролог", "эпилог", "предисловие", "введение", "послесловие", "заключение", "forord",
    "efterord", "indledning",
];

fn is_roman(s: &str) -> bool {
    !s.is_empty() && s.len() <= 8 && s.chars().all(|c| "IVXLCDMivxlcdm".contains(c)) && {
        let u = s.to_ascii_uppercase();
        // Reject words made of roman letters ("MIX", "DID", "CIVIL" would not pass length).
        !matches!(u.as_str(), "MID" | "DID" | "MIX" | "DIM" | "LID" | "MILD" | "VIM" | "CID" | "DIV")
    }
}

const NUMBER_WORDS: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve", "thirteen",
    "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen", "twenty", "thirty", "forty", "fifty",
    "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth", "last", "the",
    "первая", "вторая", "третья", "четвёртая", "четвертая", "пятая", "шестая", "первый", "второй", "третий",
    "erster", "zweiter", "dritter", "erstes", "zweites", "drittes", "première", "premier", "deuxième", "troisième",
    "første", "anden", "andet", "tredje", "fjerde", "femte", "primera", "primero", "segunda", "segundo", "tercera",
    "prima", "secondo", "terzo", "eins", "zwei", "drei", "один", "два", "три",
];

/// Heading level of a single line: 0 for parts/books, 1 for chapters, `None` otherwise.
fn heading_level(line: &str) -> Option<u8> {
    let t = line.trim();
    let chars = t.chars().count();
    if chars == 0 || chars > 80 {
        return None;
    }
    // Only the first two words are compared, so only they are lower-cased.
    let first_raw = t.split_whitespace().next().unwrap_or("");
    let rest = t[first_raw.len()..].trim();
    let first = first_raw.to_lowercase();
    let first_bare = first.trim_end_matches(|c: char| ".:,;".contains(c));
    let rest_lower = rest.split_whitespace().next().unwrap_or("").to_lowercase();
    let rest_first = rest_lower.trim_end_matches(|c: char| ".:,;—-".contains(c));
    let numberish = |w: &str| {
        !w.is_empty()
            && (w.chars().all(|c| c.is_ascii_digit()) || is_roman(w) || NUMBER_WORDS.contains(&w) || w.chars().count() == 1)
    };
    if PART_WORDS.contains(&first_bare) && numberish(rest_first) {
        return Some(0);
    }
    if (CHAPTER_WORDS.contains(&first_bare) || CHAPTER_WORDS.contains(&first.as_str())) && (rest.is_empty() || numberish(rest_first)) {
        return Some(1);
    }
    if STANDALONE.contains(&first_bare) && chars <= 40 {
        return Some(1);
    }
    // "12", "12.", "XII", "XII." alone on a line.
    let bare = t.trim_end_matches('.');
    if (bare.chars().all(|c| c.is_ascii_digit()) && bare.len() <= 4 && !bare.is_empty()) || (is_roman(bare) && bare == bare.to_uppercase()) {
        return Some(1);
    }
    None
}

fn is_all_caps_title(line: &str) -> bool {
    let t = line.trim();
    let letters: Vec<char> = t.chars().filter(|c| c.is_alphabetic()).collect();
    letters.len() >= 3 && t.chars().count() <= 60 && letters.iter().all(|c| !c.is_lowercase()) && letters.iter().any(|c| c.is_uppercase())
}

fn join_lines(lines: &[&str]) -> String {
    let mut out = String::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(l.trim_start());
    }
    out
}

fn blocks_of(groups: &[Vec<&str>], mode: Mode, allow_caps: bool) -> Vec<Block> {
    let mut out = Vec::with_capacity(groups.len());
    for g in groups {
        // A heading line, optionally followed by up to two subtitle lines that are not
        // headings themselves.
        let short = g.len() <= 3 && g.iter().all(|l| l.trim().chars().count() <= 80);
        let heading = if short {
            match heading_level(g[0]) {
                // The subtitle lines are looked at only for a heading line (rare).
                Some(level) => g[1..].iter().all(|l| heading_level(l).is_none()).then_some(level),
                None if allow_caps && mode == Mode::Blank && g.len() == 1 && is_all_caps_title(g[0]) => Some(1),
                None => None,
            }
        } else {
            None
        };
        match heading {
            Some(level) => out.push(Block::Heading { level, title: join_lines(g) }),
            None if mode == Mode::Line => out.push(Block::Para(g[0].trim_start().to_string())),
            None => out.push(Block::Para(join_lines(g))),
        }
    }
    demote_contents_lists(&mut out);
    out
}

/// In a run of headings with no text between them only a trailing chain of strictly
/// nested headings (`PART ONE` then `Chapter 1`) is real; the rest is a contents list.
fn demote_contents_lists(blocks: &mut [Block]) {
    let level = |b: &Block| match b {
        Block::Heading { level, .. } => Some(*level),
        Block::Para(_) => None,
    };
    let mut i = 0;
    while i < blocks.len() {
        if level(&blocks[i]).is_none() {
            i += 1;
            continue;
        }
        let start = i;
        while i < blocks.len() && level(&blocks[i]).is_some() {
            i += 1;
        }
        // blocks[start..i] is a run of headings; keep the last and its strict parents.
        let mut keep_from = i - 1;
        while keep_from > start && level(&blocks[keep_from - 1]) < level(&blocks[keep_from]) {
            keep_from -= 1;
        }
        for b in &mut blocks[start..keep_from] {
            if let Block::Heading { title, .. } = b {
                *b = Block::Para(std::mem::take(title));
            }
        }
    }
}

fn xhtml(title: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>{}</title></head><body>{}</body></html>",
        escape_text(title),
        body
    )
}

/// Converts decoded text into chapters.
pub fn convert_text(text: &str, title: &str) -> (Vec<HtmlChapter>, Vec<TocNode>) {
    let norm = normalise(text);
    let lines = lines_of(&norm);
    let mode = mode_of(&lines);
    let gs = groups(&lines, mode);
    drop(lines);
    let mut blocks = blocks_of(&gs, mode, false);
    // The all-caps fallback can only add headings for single all-caps lines in blank-line
    // mode; skip building a second copy of the book when there are fewer than two.
    let caps_candidates = || gs.iter().filter(|g| g.len() == 1 && is_all_caps_title(g[0])).take(2).count();
    if blocks.iter().filter(|b| matches!(b, Block::Heading { .. })).count() < 2 && mode == Mode::Blank && caps_candidates() >= 2 {
        let caps = blocks_of(&gs, mode, true);
        if caps.iter().filter(|b| matches!(b, Block::Heading { .. })).count() >= 2 {
            blocks = caps;
        }
    }
    drop(gs);
    let mut chapters: Vec<(String, String)> = Vec::new(); // (title, body)
    let mut toc: Vec<TocNode> = Vec::new();
    let mut body = String::new();
    let mut chars = 0usize;
    let mut chapter_title = title.to_string();
    let push_chapter = |chapters: &mut Vec<(String, String)>, t: &str, body: &mut String| {
        if !body.is_empty() {
            chapters.push((t.to_string(), std::mem::take(body)));
        }
    };
    let mut headings = 0;
    let mut in_part = false;
    // By value: each block's text is freed once it is in a chapter body.
    for b in blocks {
        match b {
            Block::Heading { level, title: t } => {
                headings += 1;
                push_chapter(&mut chapters, &chapter_title, &mut body);
                chars = 0;
                chapter_title = t.clone();
                let id = format!("h{headings}");
                let tag = if level == 0 { "h1" } else { "h2" };
                let _ = write!(body, "<{tag} id=\"{id}\">");
                push_text(&mut body, &t);
                let _ = write!(body, "</{tag}>");
                let node = TocNode::new(t, Target::id(chapters.len(), id));
                match (level, toc.last_mut()) {
                    (1, Some(part)) if in_part => part.children.push(node),
                    _ => {
                        in_part = level == 0;
                        toc.push(node);
                    }
                }
            }
            Block::Para(p) => {
                if (headings == 0 && chars > CHUNK_CHARS) || chars > 4 * CHUNK_CHARS {
                    push_chapter(&mut chapters, &chapter_title, &mut body);
                    chars = 0;
                }
                chars += p.len();
                body.push_str("<p>");
                push_text(&mut body, &p);
                body.push_str("</p>");
            }
        }
    }
    push_chapter(&mut chapters, &chapter_title, &mut body);
    if chapters.is_empty() {
        chapters.push((title.to_string(), "<p></p>".into()));
    }
    let out = chapters
        .into_iter()
        .enumerate()
        .map(|(i, (t, b))| HtmlChapter { path: format!("text-{:03}.xhtml", i + 1), html: xhtml(&t, &b) })
        .collect();
    (out, toc)
}

/// Metadata from a Project Gutenberg style header in the first lines.
fn header_metadata(text: &str, info: &mut BookInfo) {
    for line in text.lines().take(120) {
        let l = line.trim();
        let field = |p: &str| l.strip_prefix(p).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        if let Some(v) = field("Title:") {
            if info.metadata.title.is_empty() {
                info.metadata.title = v;
            }
        } else if let Some(v) = field("Author:") {
            if info.metadata.authors.is_empty() {
                info.metadata.authors = v.split(" and ").map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
            }
        } else if let Some(v) = field("Language:") {
            let name = v.to_lowercase();
            info.metadata.language.get_or_insert(LANGUAGES.iter().find(|l| l.0 == name).map_or(name.clone(), |l| l.2.to_string()));
        } else if l.starts_with("*** START OF") {
            break;
        }
    }
}

fn info_of(path: &Path, text: &str, encoding: &str) -> BookInfo {
    let mut info = BookInfo::new(Format::Txt);
    info.version = format!("TXT {encoding}");
    header_metadata(text, &mut info);
    if info.metadata.title.is_empty() {
        info.metadata.title = crate::title_from(path);
    }
    info
}

/// Metadata of a text file (title from a Gutenberg header or the file name).
pub fn read_info(path: &Path) -> Result<BookInfo> {
    let bytes = std::fs::read(path)?;
    let d = decode_text(&bytes[..bytes.len().min(64 * 1024)]);
    Ok(info_of(path, &d.text, d.encoding))
}

/// Share of control characters (other than tab, line and page breaks), U+FFFD and
/// unassigned C1 codes above which a "text" file is taken as binary data.
const BINARY_SHARE: f64 = 0.05;

/// True when the start of `text` looks like binary data rather than text: more than
/// [`BINARY_SHARE`] of its first 64 Ki characters are control characters or U+FFFD.
pub fn looks_binary(text: &str) -> bool {
    let (mut n, mut bad) = (0usize, 0usize);
    for c in text.chars().take(64 * 1024) {
        n += 1;
        let ctl = matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{e}'..='\u{19}' | '\u{1b}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{fffd}');
        if ctl {
            bad += 1;
        }
    }
    n > 0 && bad as f64 > n as f64 * BINARY_SHARE
}

/// Opens a text file. Binary data (an image or program renamed to `.txt`) is refused.
pub fn open(path: &Path) -> Result<Book> {
    // The raw bytes are freed once decoded.
    let d = decode_text_owned(std::fs::read(path)?);
    if looks_binary(&d.text) {
        return Err(DocError::Format(format!(
            "{} is not a text file (binary data; detected encoding {})",
            path.display(),
            d.encoding
        )));
    }
    let info = info_of(path, &d.text, d.encoding);
    let (chapters, toc) = convert_text(&d.text, &info.metadata.title);
    let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    Ok(Book { info, chapters, toc, page_list: Vec::new(), resources: Box::new(DirLoader { root }) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(toc: &[TocNode]) -> Vec<String> {
        let mut v = Vec::new();
        for n in toc {
            v.push(n.title.clone());
            for c in &n.children {
                v.push(format!("  {}", c.title));
            }
        }
        v
    }

    #[test]
    fn binary_data_is_refused_and_text_is_not() {
        // make-bad-books.py binary.txt: bytes 0..=255 repeated.
        let bin: Vec<u8> = (0..64).flat_map(|_| 0u8..=255).collect();
        assert!(looks_binary(&decode_text(&bin).text));
        let dir = std::env::temp_dir().join(format!("ink-txt-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("binary.txt");
        std::fs::write(&f, &bin).unwrap();
        let err = open(&f).expect_err("binary refused").to_string();
        assert!(err.contains("not a text file") && err.contains("binary.txt"), "{err}");
        // Text with tabs, CRLF, form feeds and a few stray controls stays text.
        let mut t = "Chapter 1\r\n\tIt was a dark night.\x0c\r\n".repeat(200);
        t.push('\u{7}');
        assert!(!looks_binary(&t));
        assert!(!looks_binary(""));
        // INK_FORMATS_TESTDATA on the emulator, where CARGO_MANIFEST_DIR does not exist.
        for name in std::fs::read_dir(crate::tests::testdata("formats/txt")).unwrap() {
            let p = name.unwrap().path();
            let text = decode_text(&std::fs::read(&p).unwrap()).text;
            assert!(!looks_binary(&text), "{} taken as binary", p.display());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn line_ends_and_form_feeds() {
        for s in ["a\r\nb\rc\u{c}d", "\r\r\n\r", "x\r\u{c}\r\n", "\u{c}\u{c}", "plain\n", "", "é\r\nü\r"] {
            let want = s.replace("\r\n", "\n").replace('\r', "\n").replace('\u{c}', "\n\n");
            assert_eq!(normalise(s), want, "{s:?}");
        }
    }

    #[test]
    fn heading_patterns() {
        for (l, want) in [
            ("CHAPTER I.", Some(1)),
            ("Chapter 12: The End", Some(1)),
            ("PART TWO", Some(0)),
            ("Book III", Some(0)),
            ("Глава 3", Some(1)),
            ("ЧАСТЬ ПЕРВАЯ", Some(0)),
            ("Kapitel 7", Some(1)),
            ("XIV.", Some(1)),
            ("12", Some(1)),
            ("Prologue", Some(1)),
            ("Chapter and verse were quoted at length by the vicar.", None),
            ("I walked home.", None),
            ("It was the best of times.", None),
        ] {
            assert_eq!(heading_level(l), want, "{l}");
        }
    }

    #[test]
    fn gutenberg_style_wrapped_text() {
        let text = "Title: A Tale\r\nAuthor: Some One\r\n\r\nContents\r\n\r\nCHAPTER I.\r\nCHAPTER II.\r\n\r\n\r\nCHAPTER I.\r\nThe Start\r\n\r\nIt was a dark\r\nand stormy night.\r\n\r\nSecond para.\r\n\r\nCHAPTER II.\r\nThe End\r\n\r\nDone.\r\n";
        let (chapters, toc) = convert_text(text, "x");
        assert_eq!(titles(&toc), vec!["CHAPTER I. The Start", "CHAPTER II. The End"]);
        assert_eq!(chapters.len(), 3);
        assert!(chapters[1].html.contains("<p>It was a dark and stormy night.</p>"), "{}", chapters[1].html);
        assert!(chapters[1].html.contains("<h2 id=\"h1\">CHAPTER I. The Start</h2>"));
        // The contents group (two heading lines in one block) stays text.
        assert!(chapters[0].html.contains("Contents"));
        assert_eq!(toc[0].target, Some(Target::id(1, "h1")));
        assert!(crate::xml::is_well_formed(&chapters[1].html));
    }

    #[test]
    fn parts_nest_chapters_and_contents_lists_are_text() {
        let text = "PART ONE\n\nChapter 1\n\nChapter 2\n\nPART ONE\n\nChapter 1\n\nText a.\n\nChapter 2\n\nText b.\n\nPART TWO\n\nChapter 3\n\nText c.\n";
        let (_, toc) = convert_text(text, "x");
        assert_eq!(titles(&toc), vec!["PART ONE", "  Chapter 1", "  Chapter 2", "PART TWO", "  Chapter 3"]);
    }

    #[test]
    fn indent_and_line_modes() {
        let indented = "  First paragraph starts\nand continues here.\n  Second one.\n  Third.\n";
        let (ch, _) = convert_text(indented, "t");
        assert!(ch[0].html.contains("<p>First paragraph starts and continues here.</p><p>Second one.</p>"), "{}", ch[0].html);
        let lines = "one\ntwo\nthree\n";
        let (ch, toc) = convert_text(lines, "t");
        assert!(ch[0].html.contains("<p>one</p><p>two</p><p>three</p>"));
        assert!(toc.is_empty());
    }

    #[test]
    fn caps_fallback_and_chunking() {
        let mut text = String::from("THE FIRST STORY\n\nSome text here.\n\nTHE SECOND STORY\n\nMore text.\n");
        let (_, toc) = convert_text(&text, "t");
        assert_eq!(titles(&toc), vec!["THE FIRST STORY", "THE SECOND STORY"]);
        text = "Lorem ipsum dolor sit amet, consectetur adipiscing elit.\n\n".repeat(4000);
        let (ch, toc) = convert_text(&text, "t");
        assert!(toc.is_empty());
        assert!(ch.len() >= 3, "{}", ch.len());
    }

    #[test]
    fn encodings_and_header() {
        let src = "Глава 1\n\nМой дядя самых честных правил, когда не в шутку занемог.\n\nГлава 2\n\nОн уважать себя заставил и лучше выдумать не мог.\n".repeat(20);
        let (cp, _, _) = encoding_rs::WINDOWS_1251.encode(&src);
        let d = decode_text(&cp);
        assert_eq!(d.encoding, "windows-1251");
        assert!(convert_text(&d.text, "t").0.iter().any(|ch| ch.html.contains("Мой дядя")));
        let mut info = BookInfo::new(Format::Txt);
        header_metadata("Title: Alice\nAuthor: Lewis Carroll\nLanguage: English\n*** START OF", &mut info);
        assert_eq!(info.metadata.title, "Alice");
        assert_eq!(info.metadata.authors, vec!["Lewis Carroll"]);
        assert_eq!(info.metadata.language.as_deref(), Some("en"));
    }
}
