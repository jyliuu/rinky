//! Old MOBI (MOBI 6) markup to XHTML chapters.
//!
//! - `filepos` targets (from `<a filepos=...>`, the NCX and the guide) get anchors
//!   `<a id="fileposN"></a>` inserted at their byte offsets (calibre's rule: an offset
//!   inside a tag moves to after that tag).
//! - `<mbp:pagebreak/>` splits chapters (`part0000.xhtml`, ...).
//! - Links become `partNNNN.xhtml#fileposN`; `recindex` images become `res/NNNNN.ext`;
//!   `<p width height>` become `text-indent`/`margin-top`; `<font size>` becomes a span.
//! - The markup is repaired to XHTML with html5ever.
//! - TOC from the NCX index (labels in CNCX, nesting by parent or level), else from the
//!   links on the guide's TOC page.

use super::header::{decode, MobiHeader};
use super::{index, Converted, Mobi, Resources};
use crate::book::{Target, TocNode, MAX_TOC_DEPTH};
use crate::xhtml::{self, Action, Rewrite};
use crate::xml::collapse_ws;
use ink_doc::Result;
use ink_html::HtmlChapter;
use markup5ever_rcdom::Handle;
use std::collections::{BTreeSet, HashMap};

/// Lowercase ASCII search.
fn find_ci(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// Byte offsets named by `filepos=` attributes.
pub fn filepos_targets(raw: &[u8]) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    let mut i = 0;
    while let Some(p) = find_ci(raw, b"filepos=", i) {
        let mut j = p + 8;
        while j < raw.len() && (raw[j] == b'"' || raw[j] == b'\'') {
            j += 1;
        }
        let start = j;
        while j < raw.len() && raw[j].is_ascii_digit() {
            j += 1;
        }
        if let Ok(n) = std::str::from_utf8(&raw[start..j]).unwrap_or("").parse::<usize>() {
            out.insert(n);
        }
        i = j.max(p + 1);
    }
    out
}

/// Inserts `<a id="fileposN"></a>` at each position (calibre `add_anchors`).
pub fn add_anchors(raw: &[u8], positions: &BTreeSet<usize>) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + positions.len() * 24);
    let mut pos = 0;
    for &target in positions {
        if target == 0 || target > raw.len() || target < pos {
            continue;
        }
        let mut end = target;
        let l = raw[end..].iter().position(|b| *b == b'<').map(|i| i + end);
        let r = raw[end..].iter().position(|b| *b == b'>').map(|i| i + end);
        if let Some(r) = r {
            // Inside a tag: the next '>' comes before the next '<' (or '<' is right here).
            if l.is_none_or(|l| r < l || l == end) {
                end = r + 1;
            }
        }
        // Never split an entity: move to after the ';'.
        if let Some(amp) = raw[pos..end].iter().rposition(|b| *b == b'&').map(|i| i + pos) {
            if end - amp <= 10 && !raw[amp..end].contains(&b';') {
                if let Some(semi) = raw[end..raw.len().min(end + 12)].iter().position(|b| *b == b';') {
                    end += semi + 1;
                }
            }
        }
        out.extend_from_slice(&raw[pos..end]);
        out.extend_from_slice(format!("<a id=\"filepos{target}\"></a>").as_bytes());
        pos = end;
    }
    out.extend_from_slice(&raw[pos..]);
    out
}

/// Splits at `<mbp:pagebreak ...>` tags (the tags are removed).
pub fn split_pagebreaks(raw: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while let Some(p) = find_ci(raw, b"<mbp:pagebreak", i) {
        let end = raw[p..].iter().position(|b| *b == b'>').map(|e| p + e + 1).unwrap_or(raw.len());
        out.push(&raw[start..p]);
        start = end;
        i = end;
    }
    out.push(&raw[start..]);
    out
}

/// `id="fileposN"` anchors in a chunk.
fn anchors_in(chunk: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = find_ci(chunk, b"id=\"filepos", i) {
        let s = p + 11;
        let e = chunk[s..].iter().position(|b| !b.is_ascii_digit()).map(|x| s + x).unwrap_or(chunk.len());
        if let Ok(n) = std::str::from_utf8(&chunk[s..e]).unwrap_or("").parse() {
            out.push(n);
        }
        i = e.max(p + 1);
    }
    out
}

fn part_name(i: usize) -> String {
    format!("part{i:04}.xhtml")
}

struct Mobi6Rewrite<'a> {
    chapter_of: &'a HashMap<usize, usize>,
    res: &'a Resources,
}

fn take(attrs: &mut Vec<(String, String)>, name: &str) -> Option<String> {
    let i = attrs.iter().position(|(k, _)| k.eq_ignore_ascii_case(name))?;
    Some(attrs.remove(i).1)
}

fn css_len(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    let num_end = v.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(v.len());
    if num_end == 0 {
        return None;
    }
    let unit = &v[num_end..];
    Some(if unit.is_empty() { format!("{}px", &v[..num_end]) } else { v.to_string() })
}

fn add_style(attrs: &mut Vec<(String, String)>, css: &str) {
    match attrs.iter_mut().find(|(k, _)| k == "style") {
        Some((_, v)) => {
            if !v.trim_end().ends_with(';') && !v.trim().is_empty() {
                v.push(';');
            }
            v.push_str(css);
        }
        None => attrs.push(("style".into(), css.to_string())),
    }
}

impl Rewrite for Mobi6Rewrite<'_> {
    fn element(&mut self, _node: &Handle, name: &mut String, attrs: &mut Vec<(String, String)>) -> Action {
        if name.starts_with("mbp:") {
            return if name == "mbp:pagebreak" { Action::Drop } else { Action::Unwrap };
        }
        match name.as_str() {
            "guide" | "reference" => return Action::Drop,
            "a" => {
                if let Some(fp) = take(attrs, "filepos") {
                    if let Ok(n) = fp.trim().trim_matches(['"', '\'']).parse::<usize>() {
                        let href = match self.chapter_of.get(&n) {
                            Some(c) => format!("{}#filepos{n}", part_name(*c)),
                            None => format!("#filepos{n}"),
                        };
                        attrs.retain(|(k, _)| k != "href");
                        attrs.push(("href".into(), href));
                    }
                }
            }
            "img" => {
                let n = ["recindex", "hirecindex", "lorecindex"].iter().find_map(|k| take(attrs, k)).and_then(|v| v.trim().parse::<usize>().ok());
                for k in ["recindex", "hirecindex", "lorecindex"] {
                    take(attrs, k);
                }
                if let Some(path) = n.and_then(|n| self.res.path(n)) {
                    attrs.retain(|(k, _)| k != "src");
                    attrs.push(("src".into(), path.to_string()));
                }
                if !attrs.iter().any(|(k, _)| k == "src") {
                    return Action::Drop;
                }
            }
            "font" => {
                *name = "span".into();
                let size = take(attrs, "size");
                take(attrs, "face");
                take(attrs, "color");
                if let Some(s) = size {
                    let css = match s.trim() {
                        "1" | "-2" => "x-small",
                        "2" | "-1" => "small",
                        "4" | "+1" => "large",
                        "5" | "+2" => "x-large",
                        "6" | "7" | "+3" | "+4" => "xx-large",
                        _ => "medium",
                    };
                    add_style(attrs, &format!("font-size:{css}"));
                }
            }
            "table" | "td" | "th" | "col" | "colgroup" | "hr" | "svg" => {}
            _ => {
                if let Some(w) = take(attrs, "width").and_then(|w| css_len(&w)) {
                    add_style(attrs, &format!("text-indent:{w}"));
                }
                if let Some(h) = take(attrs, "height").and_then(|h| css_len(&h)) {
                    add_style(attrs, &format!("margin-top:{h}"));
                }
            }
        }
        Action::Keep
    }
}

/// The guide's references in the raw markup: (type, filepos).
fn guide(raw: &[u8]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let Some(start) = find_ci(raw, b"<guide", 0) else { return out };
    let end = find_ci(raw, b"</guide>", start).unwrap_or(raw.len().min(start + 8192));
    let g = &raw[start..end];
    let mut i = 0;
    while let Some(p) = find_ci(g, b"<reference", i) {
        let e = g[p..].iter().position(|b| *b == b'>').map(|x| p + x).unwrap_or(g.len());
        let tag = String::from_utf8_lossy(&g[p..e]).to_lowercase();
        let ty = tag.split("type=").nth(1).map(|s| s.trim_start_matches(['"', '\'']).chars().take_while(|c| c.is_alphanumeric() || *c == '-').collect::<String>());
        let fp = filepos_targets(&g[p..e]).into_iter().next();
        if let (Some(t), Some(f)) = (ty, fp) {
            out.push((t, f));
        }
        i = e;
    }
    out
}

/// One NCX entry of a MOBI 6 or KF8 book.
#[derive(Clone, Debug, PartialEq)]
pub struct NcxEntry {
    /// Label.
    pub title: String,
    /// Byte offset in the text (`filepos`), tag 1.
    pub pos: Option<usize>,
    /// KF8 position: fragment number and offset, tag 6.
    pub pos_fid: Option<(usize, usize)>,
    /// Heading level, tag 4.
    pub level: u32,
    /// Parent entry number, tag 21.
    pub parent: Option<usize>,
}

/// Reads the NCX index of `h`.
pub fn ncx(m: &Mobi, h: &MobiHeader) -> Vec<NcxEntry> {
    let Some(rec) = h.abs(h.ncx_index) else { return Vec::new() };
    let idx = index::read(&m.db, rec);
    idx.entries
        .iter()
        .map(|e| NcxEntry {
            title: e.tag(3).and_then(|o| idx.cncx.get(&o)).map(|b| collapse_ws(&decode(b, h.codepage))).unwrap_or_default(),
            pos: e.tag(1).map(|p| p as usize),
            pos_fid: e.tag_at(6, 0).zip(e.tag_at(6, 1)).map(|(a, b)| (a as usize, b as usize)),
            level: e.tag(4).unwrap_or(0),
            parent: e.tag(21).map(|p| p as usize),
        })
        .collect()
}

/// Nests NCX entries by parent (or by level when parents are missing), at most
/// [`MAX_TOC_DEPTH`] levels deep (deeper entries become siblings of their parent).
pub fn nest(entries: &[NcxEntry], targets: &[Option<Target>]) -> Vec<TocNode> {
    let n = entries.len();
    let by_parent = entries.iter().any(|e| e.parent.is_some());
    let mut parent: Vec<Option<usize>> = Vec::with_capacity(n);
    let mut depth: Vec<usize> = Vec::with_capacity(n);
    // By level: indices of earlier entries with strictly rising levels; the parent is
    // the nearest earlier entry with a smaller level (the top of this stack).
    let mut rising: Vec<usize> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let mut p = if by_parent {
            e.parent.filter(|p| *p < i)
        } else {
            while rising.last().is_some_and(|&j| entries[j].level >= e.level) {
                rising.pop();
            }
            let p = rising.last().copied();
            rising.push(i);
            p
        };
        // A file can chain every entry to the one before: cap the nesting.
        while let Some(q) = p.filter(|&q| depth[q] + 1 >= MAX_TOC_DEPTH) {
            p = parent[q];
        }
        depth.push(p.map_or(0, |q| depth[q] + 1));
        parent.push(p);
    }
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut roots = Vec::new();
    for (i, p) in parent.iter().enumerate() {
        match p {
            Some(p) => children[*p].push(i),
            None => roots.push(i),
        }
    }
    // Parents come before their children: build from the end.
    let mut built: Vec<Option<TocNode>> =
        entries.iter().enumerate().map(|(i, e)| Some(TocNode { title: e.title.clone(), target: targets.get(i).cloned().flatten(), children: Vec::new() })).collect();
    for i in (0..n).rev() {
        let kids: Vec<TocNode> = children[i].iter().filter_map(|&c| built[c].take()).collect();
        if let Some(node) = built[i].as_mut() {
            node.children = kids;
        }
    }
    roots.into_iter().filter_map(|r| built[r].take()).collect()
}

/// Converts the MOBI 6 text of `h`.
pub fn convert(m: &Mobi, h: &MobiHeader, res: &Resources) -> Result<Converted> {
    let raw = m.text(h)?;
    let entries = ncx(m, h);
    let guide = guide(&raw);
    let mut positions = filepos_targets(&raw);
    positions.extend(entries.iter().filter_map(|e| e.pos).filter(|p| *p > 0));
    positions.extend(guide.iter().map(|g| g.1).filter(|p| *p > 0));
    let processed = add_anchors(&raw, &positions);
    let chunks = split_pagebreaks(&processed);
    let mut chapter_of: HashMap<usize, usize> = HashMap::new();
    for (i, c) in chunks.iter().enumerate() {
        for n in anchors_in(c) {
            chapter_of.entry(n).or_insert(i);
        }
    }
    let mut chapters = Vec::with_capacity(chunks.len());
    for (i, c) in chunks.iter().enumerate() {
        let text = decode(c, h.codepage);
        let mut rw = Mobi6Rewrite { chapter_of: &chapter_of, res };
        chapters.push(HtmlChapter { path: part_name(i), html: xhtml::to_xhtml(&text, &mut rw) });
    }
    let target = |pos: usize| -> Option<Target> {
        if pos == 0 {
            return Some(Target::chapter(0));
        }
        chapter_of.get(&pos).map(|c| Target::id(*c, format!("filepos{pos}")))
    };
    let toc = if !entries.is_empty() {
        let targets: Vec<Option<Target>> = entries.iter().map(|e| e.pos.and_then(target)).collect();
        nest(&entries, &targets)
    } else {
        guide_toc(&chapters, &guide, &chapter_of, &target)
    };
    Ok((chapters, toc, HashMap::new()))
}

/// TOC from the links on the guide's `toc` page.
fn guide_toc(chapters: &[HtmlChapter], guide: &[(String, usize)], chapter_of: &HashMap<usize, usize>, target: &dyn Fn(usize) -> Option<Target>) -> Vec<TocNode> {
    let Some(&(_, pos)) = guide.iter().find(|(t, _)| t == "toc") else { return Vec::new() };
    let Some(&ch) = chapter_of.get(&pos) else { return Vec::new() };
    let Ok(root) = crate::xml::parse(&chapters[ch].html) else { return Vec::new() };
    let mut links = Vec::new();
    root.find_all("a", &mut links);
    links
        .iter()
        .filter_map(|a| {
            let href = a.attr("href")?;
            let n: usize = href.split("#filepos").nth(1)?.parse().ok()?;
            let title = a.text_collapsed();
            (!title.is_empty()).then(|| TocNode { title, target: target(n), children: Vec::new() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_and_pagebreaks() {
        let raw = b"<html><head><guide><reference type=\"toc\" title=\"Contents\" filepos=0000000060 /></guide></head><body><p>One &amp; two</p><mbp:pagebreak/><p>Three <a filepos=0000000010>link</a></p></body></html>";
        let pos = filepos_targets(raw);
        assert_eq!(pos.into_iter().collect::<Vec<_>>(), vec![10, 60]);
        // 10 is inside "<head>": moves after the tag.
        let set: BTreeSet<usize> = [10, 117].into();
        let out = add_anchors(raw, &set);
        let s = String::from_utf8_lossy(&out);
        assert!(s.starts_with("<html><head><a id=\"filepos10\"></a>"), "{s}");
        // 117 points into "&amp;" (offset of 'a' after '&'): moves after the entity.
        let amp = raw.windows(5).position(|w| w == b"&amp;").unwrap();
        let set: BTreeSet<usize> = [amp + 2].into();
        let s = String::from_utf8_lossy(&add_anchors(raw, &set)).into_owned();
        assert!(s.contains(&format!("&amp;<a id=\"filepos{}\"></a>", amp + 2)), "{s}");
        let parts = split_pagebreaks(raw);
        assert_eq!(parts.len(), 2);
        assert!(parts[1].starts_with(b"<p>Three"));
        assert_eq!(guide(raw), vec![("toc".to_string(), 60)]);
    }

    #[test]
    fn rewrite_links_images_and_widths() {
        let mut res = Resources::default();
        res.by_number.insert(2, super::super::Resource { number: 2, path: "res/00002.jpg".into(), record: 9, kind: super::super::ResKind::Image, media_type: "image/jpeg".into() });
        let chapter_of: HashMap<usize, usize> = [(500, 3)].into();
        let mut rw = Mobi6Rewrite { chapter_of: &chapter_of, res: &res };
        let x = xhtml::to_xhtml("<p width=\"-18pt\" height=\"1em\">a<a filepos=0000000500>b</a><img recindex=\"00002\"><img recindex=\"00009\"><font size=\"+1\">c</font><mbp:nu>d</mbp:nu></p>", &mut rw);
        assert!(x.contains("<p style=\"text-indent:-18pt;margin-top:1em\">a<a href=\"part0003.xhtml#filepos500\">b</a><img src=\"res/00002.jpg\"/><span style=\"font-size:large\">c</span>d</p>"), "{x}");
    }

    #[test]
    fn nesting() {
        let e = |t: &str, level: u32, parent: Option<usize>| NcxEntry { title: t.into(), pos: None, pos_fid: None, level, parent };
        let entries = vec![e("A", 0, None), e("A1", 1, Some(0)), e("A2", 1, Some(0)), e("B", 0, None)];
        let toc = nest(&entries, &vec![None; 4]);
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].children.len(), 2);
        let entries = vec![e("A", 0, None), e("A1", 1, None), e("A1a", 2, None), e("B", 0, None)];
        let toc = nest(&entries, &vec![None; 4]);
        assert_eq!(toc[0].children[0].children[0].title, "A1a");
        assert_eq!(toc[1].title, "B");
        // 100 000 entries, each the child of the one before (by parent and by level):
        // the tree is cut at MAX_TOC_DEPTH, no entry is lost, and it is built in linear
        // time (the old builder inserted at the front of each list).
        let n: usize = 100_000;
        for by_parent in [true, false] {
            let chain: Vec<NcxEntry> = (0..n).map(|i| e("x", i as u32, if by_parent { i.checked_sub(1) } else { None })).collect();
            let t = std::time::Instant::now();
            let toc = nest(&chain, &[]);
            assert!(t.elapsed() < std::time::Duration::from_secs(10));
            assert_eq!(crate::book::toc_len(&toc), n);
            assert_eq!(crate::book::toc_depth(&toc), MAX_TOC_DEPTH);
            drop(toc);
        }
        // Many siblings under one parent keep their order.
        let flat: Vec<NcxEntry> = std::iter::once(e("P", 0, None)).chain((0..50_000).map(|i| e(&i.to_string(), 1, Some(0)))).collect();
        let toc = nest(&flat, &[]);
        assert_eq!(toc[0].children.len(), 50_000);
        assert_eq!(toc[0].children[49_999].title, "49999");
    }
}
