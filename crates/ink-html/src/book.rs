//! Book-level data shared by the foreground and the background paginator.
//!
//! [`Book`] holds the spine sources, the parsed stylesheets and font bytes (read on the
//! main thread when the book opens, because the resource loader of an EPUB is not
//! `Send`), and caches of chapter DOMs and image sizes. Everything here is `Send + Sync`.

use crate::css::{self, FontFaceRule, Stylesheet};
use crate::dom::{Classifier, Dom, ElemRef, NodeId, SheetRef};
use crate::style::{ua_rules, Cascade, Origin, RuleSet, StructStyle, StyleContext, WhiteSpace};
use ink_doc::{resolve_href, Metadata, ResourceLoader};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Reads spine item sources when they are first needed, from any thread, so a book
/// opens without reading (inflating, decoding) every spine item first.
pub trait SpineSource: Send + Sync {
    /// The XHTML source of spine item `i` (empty when it cannot be read).
    fn read(&self, i: usize) -> String;

    /// A prefix of what [`SpineSource::read`] returns for item `i` that reaches past
    /// its first `<body` tag, read without the rest of the item; `None` when that is
    /// not possible (the whole item is read then).
    fn read_head(&self, _i: usize) -> Option<String> {
        None
    }

    /// The length [`SpineSource::read`] gives for item `i`, when it is known without
    /// reading the item (the stored size of the file). Only estimates use it: pages
    /// and Kindle locations of spine items nobody has read yet.
    fn len_hint(&self, _i: usize) -> Option<usize> {
        None
    }
}

/// Where a spine item's source is read from: the source and the item's index.
type Source = Option<(Arc<dyn SpineSource>, usize)>;

/// One spine item.
#[derive(Clone)]
pub struct ChapterSrc {
    /// Path inside the book.
    pub path: String,
    /// Directory of `path` (for resolving links), with a trailing `/` or empty.
    pub dir: String,
    /// XHTML source, once read.
    html: OnceLock<String>,
    /// Where the source is read from, with the item's index (`None`: `html` was given).
    source: Source,
    /// DocFragment number (1-based); `None` for spine entries crengine skips (an idref
    /// that is not in the manifest, passed by the EPUB loader with an empty path).
    pub fragment: Option<u32>,
}

impl std::fmt::Debug for ChapterSrc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChapterSrc")
            .field("path", &self.path)
            .field("read", &self.is_read())
            .field("fragment", &self.fragment)
            .finish()
    }
}

impl ChapterSrc {
    /// The XHTML source (read now when it has not been).
    pub fn html(&self) -> &str {
        self.html.get_or_init(|| self.source.as_ref().map(|(s, i)| s.read(*i)).unwrap_or_default())
    }

    /// True when the source has been read (or was given).
    pub fn is_read(&self) -> bool {
        self.html.get().is_some()
    }

    /// The length of the source in bytes: exact once it is read, else the source's
    /// [`SpineSource::len_hint`] (an estimate), else read now.
    pub fn src_len(&self) -> usize {
        if let Some(h) = self.html.get() {
            return h.len();
        }
        match self.source.as_ref().and_then(|(s, i)| s.len_hint(*i)) {
            Some(n) => n,
            None => self.html().len(),
        }
    }

    /// The source up to its first `<body` tag at least: a prefix from
    /// [`SpineSource::read_head`] while the item is not read (owned), else the whole
    /// source (borrowed).
    fn head(&self) -> Cow<'_, str> {
        if !self.is_read() {
            if let Some(head) = self.source.as_ref().and_then(|(s, i)| s.read_head(*i)) {
                if find_ci(&head, 0, "<body").is_some() {
                    return Cow::Owned(head);
                }
            }
        }
        Cow::Borrowed(self.html())
    }
}

/// A parsed chapter: DOM plus the book rules that apply to it.
pub struct ChapterDom {
    /// The DOM.
    pub dom: Dom,
    /// Author rules for this chapter (its `<link>`s and `<style>`s).
    pub author: Arc<RuleSet>,
}

/// Image size cache entry: `None` when the resource is missing or not an image.
pub type ImageSize = Option<(u32, u32)>;

/// Shared book data.
pub struct Book {
    /// Metadata.
    pub meta: Metadata,
    /// Spine items in order.
    pub chapters: Vec<ChapterSrc>,
    /// Number of DocFragments.
    pub fragments: u32,
    /// Stylesheets by resolved path.
    pub sheets: HashMap<String, Arc<Stylesheet>>,
    /// Every `@font-face` rule of the book's stylesheets and `<style>` blocks, with the
    /// directory its URLs are relative to.
    pub font_faces: Vec<(String, FontFaceRule)>,
    /// Font file bytes by resolved path.
    pub fonts: HashMap<String, Arc<Vec<u8>>>,
    /// A loader usable from any thread, when the format provides one.
    pub loader: Option<Arc<dyn ResourceLoader + Send + Sync>>,
    /// Image sizes by resolved path.
    pub images: Mutex<HashMap<String, ImageSize>>,
    /// Rule sets by the list of sheets a chapter uses.
    rulesets: Mutex<HashMap<String, Arc<RuleSet>>>,
    /// Parsed chapters (built on demand).
    doms: Mutex<HashMap<usize, Arc<ChapterDom>>>,
}

impl std::fmt::Debug for Book {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Book").field("title", &self.meta.title).field("chapters", &self.chapters.len()).finish()
    }
}

/// The directory part of a book path, with its trailing `/` (empty for none).
pub(crate) fn dir_of(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..=i])
}

/// Byte offset of the first `<body` (any case) in `html`, or its length. Steps from `<`
/// to `<` (a memchr search), so the whole source is never copied or lowercased.
pub fn head_end(html: &str) -> usize {
    find_ci(html, 0, "<body").unwrap_or(html.len())
}

/// First offset at or after `from` where `needle` (lowercase ASCII, starting with `<`)
/// occurs in `hay`, ignoring ASCII case.
pub(crate) fn find_ci(hay: &str, from: usize, needle: &str) -> Option<usize> {
    let b = hay.as_bytes();
    let n = needle.as_bytes();
    let mut i = from;
    while i < b.len() {
        let at = i + hay[i..].find('<')?;
        if b.len() - at >= n.len() && b[at + 1..at + n.len()].eq_ignore_ascii_case(&n[1..]) {
            return Some(at);
        }
        i = at + 1;
    }
    None
}

/// `<link rel="stylesheet" href>` targets in the head of an (X)HTML source (cheap scan).
pub fn scan_head_links(html: &str) -> Vec<String> {
    // Only the head is lowercased (a whole big chapter was copied twice per open).
    let head_end = head_end(html);
    let lower = html[..head_end].to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = lower[from..head_end].find("<link") {
        let start = from + p;
        let end = lower[start..head_end].find('>').map(|e| start + e).unwrap_or(head_end);
        let tag = &html[start..end];
        let tag_l = &lower[start..end];
        if tag_l.contains("stylesheet") {
            if let Some(h) = attr_in_tag(tag, "href") {
                out.push(h);
            }
        }
        from = end;
    }
    out
}

/// The start tags of an (X)HTML source: (name, attributes as written), in order.
/// Attribute values may contain `>`; comments, CDATA sections, processing instructions,
/// doctypes, end tags and the contents of `<script>` and `<style>` are skipped. Values
/// are not entity-decoded.
pub fn scan_start_tags(html: &str) -> Vec<(&str, Vec<(&str, &str)>)> {
    let b = html.as_bytes();
    let n = b.len();
    let skip_to = |from: usize, pat: &str| html[from..].find(pat).map(|e| from + e + pat.len()).unwrap_or(n);
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = html[i..].find('<') {
        let at = i + p;
        let rest = &html[at..];
        if rest.starts_with("<!--") {
            i = skip_to(at + 4, "-->");
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = skip_to(at + 9, "]]>");
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") || rest.starts_with("</") {
            i = skip_to(at + 2, ">");
            continue;
        }
        let name_start = at + 1;
        let mut j = name_start;
        while j < n && (b[j].is_ascii_alphanumeric() || matches!(b[j], b':' | b'-' | b'_' | b'.')) {
            j += 1;
        }
        if j == name_start {
            i = at + 1;
            continue;
        }
        let name = &html[name_start..j];
        let mut attrs = Vec::new();
        let mut self_closing = false;
        // Attributes up to the closing `>`.
        loop {
            while j < n && (b[j].is_ascii_whitespace() || b[j] == b'/') {
                self_closing = b[j] == b'/';
                j += 1;
            }
            if j >= n || b[j] == b'>' {
                break;
            }
            self_closing = false;
            let a0 = j;
            while j < n && !b[j].is_ascii_whitespace() && !matches!(b[j], b'=' | b'>' | b'/') {
                j += 1;
            }
            let aname = &html[a0..j];
            while j < n && b[j].is_ascii_whitespace() {
                j += 1;
            }
            let mut value = "";
            if j < n && b[j] == b'=' {
                j += 1;
                while j < n && b[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < n && (b[j] == b'"' || b[j] == b'\'') {
                    let q = b[j];
                    let v0 = j + 1;
                    let v1 = html[v0..].find(q as char).map(|e| v0 + e).unwrap_or(n);
                    value = &html[v0..v1];
                    j = (v1 + 1).min(n);
                } else {
                    let v0 = j;
                    while j < n && !b[j].is_ascii_whitespace() && b[j] != b'>' {
                        j += 1;
                    }
                    value = &html[v0..j];
                }
            }
            if aname.is_empty() {
                j += 1;
            } else {
                attrs.push((aname, value));
            }
        }
        let end = (j + 1).min(n);
        let local = name.rsplit(':').next().unwrap_or(name);
        i = end;
        if !self_closing && (local.eq_ignore_ascii_case("script") || local.eq_ignore_ascii_case("style")) {
            // Raw text up to the end tag.
            i = find_ci(html, end, &format!("</{}", name.to_ascii_lowercase())).unwrap_or(n);
        }
        out.push((name, attrs));
    }
    out
}

/// Image references of an (X)HTML source, entity-decoded and as written (relative to the
/// chapter): `src` of `<img>`, else `href` / `xlink:href` (any prefix) of `<img>` and
/// `<image>`, as layout reads them. `data:` URLs are left out.
pub fn scan_image_refs(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Most chapters have no image: skip the tag scan for them.
    if !has_image_tag(html) {
        return out;
    }
    for (name, attrs) in scan_start_tags(html) {
        let local = name.rsplit(':').next().unwrap_or(name);
        if !local.eq_ignore_ascii_case("img") && !local.eq_ignore_ascii_case("image") {
            continue;
        }
        let src = attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case("src")).or_else(|| {
            attrs.iter().find(|(k, _)| {
                let l = k.rsplit(':').next().unwrap_or(k);
                l.eq_ignore_ascii_case("href")
            })
        });
        if let Some((_, v)) = src {
            let v = crate::parse::decode_attr(v);
            if !v.trim_start().starts_with("data:") && !v.is_empty() {
                out.push(v);
            }
        }
    }
    out
}

/// True when `html` has a start tag whose local name is `img` or `image` (any case,
/// any prefix). Jumps from `<` to `<` (a memchr search) and reads only tag names, so a
/// chapter whose text says "imagine" is not tag-scanned. May say true for a tag inside a
/// comment or script; [`scan_image_refs`] then skips it.
pub fn has_image_tag(html: &str) -> bool {
    let b = html.as_bytes();
    let mut i = 0;
    while let Some(p) = html.get(i..).and_then(|r| r.find('<')) {
        let start = i + p + 1;
        let mut j = start;
        while j < b.len() && (b[j].is_ascii_alphanumeric() || matches!(b[j], b':' | b'-' | b'_' | b'.')) {
            j += 1;
        }
        let name = &b[start..j];
        let local = name.rsplit(|&c| c == b':').next().unwrap_or(name);
        if local.eq_ignore_ascii_case(b"img") || local.eq_ignore_ascii_case(b"image") {
            return true;
        }
        i = start;
    }
    false
}

/// Value of attribute `name` in a raw start tag (quotes required or not).
pub fn attr_in_tag(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(name) {
        let at = from + p;
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace() || lower.as_bytes()[at - 1] == b':';
        let rest = lower[at + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            let vstart = tag.len() - rest.len() + 1;
            let v = tag[vstart..].trim_start();
            let value = match v.chars().next()? {
                q @ ('"' | '\'') => v[1..].split(q).next()?.to_string(),
                _ => v.split(|c: char| c.is_whitespace() || c == '>' || c == '/').next()?.to_string(),
            };
            return Some(crate::parse::decode_attr(&value));
        }
        from = at + name.len();
    }
    None
}

/// What the head of a spine item gives [`Book::new`]: its linked stylesheets and its
/// `<style>` blocks.
struct HeadSheets {
    links: Vec<String>,
    styles: Vec<Stylesheet>,
}

impl HeadSheets {
    /// The sheets of `ch`'s head, from a prefix of its source when that is enough.
    fn of(ch: &ChapterSrc) -> Self {
        let head = ch.head();
        if let Some(styles) = head_styles(&head, matches!(head, Cow::Borrowed(_))) {
            return HeadSheets { links: scan_head_links(&head), styles };
        }
        // A `<style>` the prefix does not close: the whole source.
        let html = ch.html();
        HeadSheets { links: scan_head_links(html), styles: head_styles(html, true).unwrap_or_default() }
    }
}

/// The `<style>` blocks of the head of `html` (up to its first `<body`), parsed. A
/// block not closed before `<body` runs to its `</style` after it, or to the end of the
/// head without one. `None` when `html` is only a prefix of the source (`complete`
/// false) and has no `</style` for such a block.
fn head_styles(html: &str, complete: bool) -> Option<Vec<Stylesheet>> {
    let head_end = head_end(html);
    let lower = html[..head_end].to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = lower.get(from..).and_then(|l| l.find("<style")) {
        let s = from + p;
        let Some(open_end) = lower[s..head_end].find('>').map(|e| s + e + 1) else { break };
        let close = match find_ci(html, open_end, "</style") {
            Some(c) => c,
            None if complete => head_end,
            None => return None,
        };
        out.push(css::parse_stylesheet(&html[open_end..close.min(html.len())]));
        from = close;
    }
    Some(out)
}

impl Book {
    /// Builds the book data. Spine items with an empty path do not get a DocFragment
    /// number. Stylesheets linked from any chapter, their imports and their
    /// `@font-face` fonts are read through `loader` now.
    pub fn new(meta: Metadata, chapters: Vec<(String, String)>, loader: &dyn ResourceLoader) -> Self {
        let chapters = chapters.into_iter().map(|(path, html)| (path, OnceLock::from(html), None)).collect();
        Self::build(meta, chapters, loader)
    }

    /// [`Book::new`] for spine items at `paths` whose sources `source` reads when they
    /// are first needed. Only the heads of the items are read now (for their
    /// stylesheets), as far as `source` can read a head alone.
    pub fn new_lazy(meta: Metadata, paths: Vec<String>, source: Arc<dyn SpineSource>, loader: &dyn ResourceLoader) -> Self {
        let chapters = paths.into_iter().enumerate().map(|(i, path)| (path, OnceLock::new(), Some((source.clone(), i)))).collect();
        Self::build(meta, chapters, loader)
    }

    fn build(meta: Metadata, chapters: Vec<(String, OnceLock<String>, Source)>, loader: &dyn ResourceLoader) -> Self {
        let mut fragment = 0;
        let chapters: Vec<ChapterSrc> = chapters
            .into_iter()
            .map(|(path, html, source)| {
                let frag = if path.is_empty() {
                    None
                } else {
                    fragment += 1;
                    Some(fragment)
                };
                ChapterSrc { dir: dir_of(&path).to_string(), path, html, source, fragment: frag }
            })
            .collect();
        let mut sheets: HashMap<String, Arc<Stylesheet>> = HashMap::new();
        let mut fonts: HashMap<String, Arc<Vec<u8>>> = HashMap::new();
        let mut todo: Vec<String> = Vec::new();
        let heads: Vec<HeadSheets> = chapters.iter().map(HeadSheets::of).collect();
        for (ch, head) in chapters.iter().zip(&heads) {
            for href in &head.links {
                let p = resolve_href(&ch.dir, href);
                if !todo.contains(&p) {
                    todo.push(p);
                }
            }
        }
        // `<style>` blocks may @import or declare fonts too: their @font-face rules with
        // the chapter's directory.
        let mut inline_faces = Vec::new();
        for (ch, head) in chapters.iter().zip(heads) {
            for sheet in head.styles {
                for imp in &sheet.imports {
                    todo.push(resolve_href(&ch.dir, imp));
                }
                inline_faces.extend(sheet.font_faces.into_iter().map(|ff| (ch.dir.clone(), ff)));
            }
        }
        let mut seen = std::collections::HashSet::new();
        while let Some(path) = todo.pop() {
            if !seen.insert(path.clone()) || path.split('/').any(|p| p == "..") {
                continue;
            }
            let Ok(bytes) = loader.read(&path) else { continue };
            let sheet = css::parse_stylesheet(&String::from_utf8_lossy(&bytes));
            let dir = dir_of(&path);
            for imp in &sheet.imports {
                todo.push(resolve_href(dir, imp));
            }
            sheets.insert(path, Arc::new(sheet));
        }
        // The linked sheets' @font-face rules (by path, so the order does not depend on
        // hashing), then the inline ones.
        let mut linked: Vec<(&String, &Arc<Stylesheet>)> = sheets.iter().collect();
        linked.sort_by_key(|(path, _)| *path);
        let mut font_faces: Vec<(String, FontFaceRule)> =
            linked.into_iter().flat_map(|(path, sheet)| sheet.font_faces.iter().map(|ff| (dir_of(path).to_string(), ff.clone()))).collect();
        font_faces.extend(inline_faces);
        for (dir, ff) in &font_faces {
            for src in &ff.sources {
                let p = resolve_href(dir, src);
                if fonts.contains_key(&p) {
                    continue;
                }
                if let Ok(bytes) = loader.read(&p) {
                    fonts.insert(p, Arc::new(bytes));
                }
            }
        }
        let fragments = fragment;
        Book {
            meta,
            chapters,
            fragments,
            sheets,
            font_faces,
            fonts,
            loader: loader.shared(),
            images: Mutex::new(HashMap::new()),
            rulesets: Mutex::new(HashMap::new()),
            doms: Mutex::new(HashMap::new()),
        }
    }

    /// The size of image `path`: cached, else read through `loader` and cached. `None`
    /// when it is not cached and there is no loader.
    pub fn image_size(&self, path: &str, loader: Option<&dyn ResourceLoader>) -> Option<ImageSize> {
        if let Some(s) = self.images.lock().unwrap().get(path) {
            return Some(*s);
        }
        let size = loader?.read(path).ok().and_then(|b| crate::images::intrinsic_size(&b));
        self.images.lock().unwrap().insert(path.to_string(), size);
        Some(size)
    }

    /// Index of the chapter with DocFragment number `n`.
    pub fn chapter_of_fragment(&self, n: u32) -> Option<usize> {
        self.chapters.iter().position(|c| c.fragment == Some(n))
    }

    /// The rules of the sheets `refs` (in order) for a chapter in `dir`.
    pub fn author_rules(&self, dir: &str, refs: &[SheetRef]) -> Arc<RuleSet> {
        let key: String = refs
            .iter()
            .map(|r| match r {
                SheetRef::Link(h) => format!("L{}\u{1}", resolve_href(dir, h)),
                SheetRef::Inline(css) => format!("I{dir}\u{2}{css}\u{1}"),
            })
            .collect();
        if let Some(r) = self.rulesets.lock().unwrap().get(&key) {
            return r.clone();
        }
        let mut rs = RuleSet::default();
        let mut added = std::collections::HashSet::new();
        for r in refs {
            match r {
                SheetRef::Link(h) => self.add_sheet_path(&mut rs, &resolve_href(dir, h), &mut added),
                SheetRef::Inline(css) => {
                    let sheet = css::parse_stylesheet(css);
                    for imp in &sheet.imports {
                        self.add_sheet_path(&mut rs, &resolve_href(dir, imp), &mut added);
                    }
                    rs.add_sheet(&sheet, Origin::Author);
                }
            }
        }
        let rs = Arc::new(rs);
        self.rulesets.lock().unwrap().insert(key, rs.clone());
        rs
    }

    fn add_sheet_path(&self, rs: &mut RuleSet, path: &str, added: &mut std::collections::HashSet<String>) {
        if !added.insert(path.to_string()) {
            return;
        }
        let sheet = match self.sheets.get(path) {
            Some(s) => s.clone(),
            None => match self.loader.as_ref().and_then(|l| l.read(path).ok()) {
                Some(bytes) => Arc::new(css::parse_stylesheet(&String::from_utf8_lossy(&bytes))),
                None => return,
            },
        };
        let dir = dir_of(path);
        for imp in &sheet.imports {
            self.add_sheet_path(rs, &resolve_href(dir, imp), added);
        }
        rs.add_sheet(&sheet, Origin::Author);
    }

    /// The parsed DOM of chapter `i` (cached).
    pub fn chapter(&self, i: usize) -> Arc<ChapterDom> {
        if let Some(c) = self.doms.lock().unwrap().get(&i) {
            return c.clone();
        }
        let built = Arc::new(self.build_chapter(i));
        self.doms.lock().unwrap().entry(i).or_insert(built).clone()
    }

    /// True when chapter `i` is parsed already.
    pub fn has_chapter(&self, i: usize) -> bool {
        self.doms.lock().unwrap().contains_key(&i)
    }

    /// Drops the cached DOM of chapter `i`.
    pub fn evict_chapter(&self, i: usize) {
        self.doms.lock().unwrap().remove(&i);
    }

    fn build_chapter(&self, i: usize) -> ChapterDom {
        let ch = &self.chapters[i];
        let mut cl = ChapterClassifier { book: self, dir: &ch.dir, cascade: None };
        let dom = crate::parse::parse_chapter(ch.html(), &mut cl);
        let author = cl.cascade.map_or_else(|| self.author_rules(&ch.dir, &dom.sheets), |c| c.author);
        ChapterDom { dom, author }
    }
}

/// Classifies elements with the UA rules plus the chapter's own stylesheets.
struct ChapterClassifier<'a> {
    book: &'a Book,
    dir: &'a str,
    cascade: Option<Cascade>,
}

impl Classifier for ChapterClassifier<'_> {
    fn classify(&mut self, dom: &Dom, id: NodeId, parent_ws: WhiteSpace) -> StructStyle {
        if self.cascade.is_none() {
            let author = self.book.author_rules(self.dir, &dom.sheets);
            self.cascade = Some(Cascade {
                ua: ua_rules(),
                author,
                publisher: true,
                ctx: StyleContext { font_px: 16.0, page_width: 600.0, page_height: 800.0 },
            });
        }
        self.cascade.as_ref().unwrap().structure(ElemRef { dom, id }, parent_ws)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_doc::MemoryLoader;

    /// Spine items from memory that count whole reads; heads end at `<body`.
    struct Counting(Vec<String>, std::sync::atomic::AtomicUsize);

    impl SpineSource for Counting {
        fn read(&self, i: usize) -> String {
            self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.0[i].clone()
        }
        fn read_head(&self, i: usize) -> Option<String> {
            let end = self.0[i].to_ascii_lowercase().find("<body")? + 5;
            Some(self.0[i][..end].to_string())
        }
        fn len_hint(&self, i: usize) -> Option<usize> {
            Some(self.0[i].len())
        }
    }

    #[test]
    fn lazy_spine_reads_heads_only() {
        let a = r#"<html><head><link rel="stylesheet" href="a.css"/><style>@import "b.css";</style></head><body><p>A</p></body></html>"#;
        let b = r#"<html><head><style>@import "c.css";</head><body><p>B</p><style>x</style></body></html>"#;
        let mut m = MemoryLoader::default();
        for css in ["t/a.css", "t/b.css", "t/c.css"] {
            m.0.insert(css.into(), b"p { margin: 0 }".to_vec());
        }
        let items = vec![(String::from("t/a.xhtml"), a.to_string()), (String::from("t/b.xhtml"), b.to_string())];
        let eager = Book::new(Metadata::default(), items.clone(), &m);
        let source = Arc::new(Counting(items.iter().map(|(_, h)| h.clone()).collect(), Default::default()));
        let paths = items.into_iter().map(|(p, _)| p).collect();
        let lazy = Book::new_lazy(Metadata::default(), paths, source.clone(), &m);
        let keys = |b: &Book| {
            let mut k: Vec<String> = b.sheets.keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(keys(&lazy), keys(&eager));
        assert_eq!(keys(&lazy), ["t/a.css", "t/b.css", "t/c.css"]);
        // The first item's head is enough; the second's `<style>` closes after `<body`.
        assert!(!lazy.chapters[0].is_read() && lazy.chapters[1].is_read());
        assert_eq!(source.1.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(lazy.chapters[0].src_len(), a.len());
        assert_eq!(lazy.chapters[0].html(), a);
    }

    #[test]
    fn head_scanning_and_sheet_loading() {
        let html = r#"<html><head><link href="../css/a.css" rel="stylesheet" type="text/css"/><LINK REL='stylesheet' HREF='b.css'><link rel="icon" href="x.png"/>
            <style>@import "c.css"; @font-face { font-family: F; src: url(../f.ttf) }</style></head><body><p class="x">t</p></body></html>"#;
        assert_eq!(scan_head_links(html), vec!["../css/a.css".to_string(), "b.css".to_string()]);
        let mut m = MemoryLoader::default();
        m.0.insert("css/a.css".into(), b"@import url(d.css); .x { color: red }".to_vec());
        m.0.insert("css/d.css".into(), b"p { margin: 0 }".to_vec());
        m.0.insert("text/b.css".into(), b"p { text-align: center }".to_vec());
        m.0.insert("text/c.css".into(), b"p { font-weight: bold }".to_vec());
        m.0.insert("f.ttf".into(), b"FONT".to_vec());
        let book = Book::new(Metadata::default(), vec![("text/c1.xhtml".into(), html.into()), (String::new(), String::new())], &m);
        assert_eq!(book.fragments, 1);
        assert_eq!(book.chapters[1].fragment, None);
        let mut keys: Vec<_> = book.sheets.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, vec!["css/a.css", "css/d.css", "text/b.css", "text/c.css"]);
        assert_eq!(book.fonts.get("f.ttf").map(|b| b.as_slice()), Some(&b"FONT"[..]));
        assert_eq!(book.font_faces.iter().map(|(dir, ff)| (dir.as_str(), ff.family.as_str())).collect::<Vec<_>>(), [("text/", "F")]);
        let ch = book.chapter(0);
        // a.css (+ d.css), b.css, the <style> (+ c.css): one rule each.
        assert_eq!(ch.author.len(), 4);
        assert!(Arc::ptr_eq(&ch, &book.chapter(0)));
        assert_eq!(attr_in_tag("<img src=a.png alt='x'>", "src").as_deref(), Some("a.png"));
        assert_eq!(head_end("<html><head></head><BODY>x</body>"), 19);
        assert_eq!(head_end("<p>no body</p>"), 14);
        assert_eq!(head_end("<bod"), 4);
        assert!(has_image_tag("<p>x</p><IMG src='a'>") && has_image_tag("<svg:image xlink:href='a'/>"));
        assert!(!has_image_tag("<p>imagine <imgx> an image</p><images/>"));
        assert_eq!(scan_image_refs("<p>imagine</p><svg><image xlink:href=\"b.png\"/></svg><img src='a.png'/>"), vec!["b.png".to_string(), "a.png".to_string()]);
        assert_eq!(attr_in_tag("<image xlink:href=\"c.jpg\"/>", "href").as_deref(), Some("c.jpg"));
    }
}
