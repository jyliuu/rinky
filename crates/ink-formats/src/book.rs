//! The converted book every format produces, and the [`Document`] wrapper that adds the
//! book's own table of contents and page list to the engine's document.
//!
//! A format reader yields a [`Book`]: XHTML chapters for `ink-html`, a resource loader,
//! [`BookInfo`] (metadata, identifiers, cover, fonts, DRM), a nested TOC and a page list.
//! TOC and page-list targets are a chapter index plus an optional element id. They become
//! crengine xpointers: `/body/DocFragment[N]/body/...` down to the element with that id
//! (element steps get `[k]` only when there are same-named siblings, as crengine writes
//! them). The phase 0 engine resolves an xpointer to the start of its `DocFragment`; the
//! phase 2 engine resolves the element path.

use crate::xml;
#[cfg(test)]
use crate::xml::Element;
use crate::Format;
use ink_device::{Canvas, Point};
use ink_doc::{
    DocKind, Document, LayoutSettings, Link, Location, Metadata, PageCount, Position, ResourceLoader, Result,
    SearchHit, Selection, Step, TocEntry, XPointer,
};
use ink_html::{HtmlChapter, HtmlDocument};
use std::collections::{HashMap, HashSet};

/// A place in a converted book: chapter (0-based, = `DocFragment[chapter + 1]`) and an
/// optional element id inside it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    /// Chapter index, 0-based.
    pub chapter: usize,
    /// Element id inside the chapter (`None` = chapter start).
    pub id: Option<String>,
}

impl Target {
    /// The start of chapter `chapter`.
    pub fn chapter(chapter: usize) -> Self {
        Target { chapter, id: None }
    }

    /// Element `id` in chapter `chapter`.
    pub fn id(chapter: usize, id: impl Into<String>) -> Self {
        Target { chapter, id: Some(id.into()) }
    }
}

/// One table-of-contents node.
#[derive(Clone, Debug, PartialEq)]
pub struct TocNode {
    /// Title as shown.
    pub title: String,
    /// Where it points (`None` for a heading without a target; its children still count).
    pub target: Option<Target>,
    /// Nested entries.
    pub children: Vec<TocNode>,
}

impl TocNode {
    /// A leaf entry.
    pub fn new(title: impl Into<String>, target: Target) -> Self {
        TocNode { title: title.into(), target: Some(target), children: Vec::new() }
    }
}

/// Deepest TOC nesting built from tables that give parents per entry (MOBI/KF8 NCX).
/// Trees from XML (EPUB NCX/nav, FB2 sections) are bounded by [`crate::xml::MAX_DEPTH`].
pub const MAX_TOC_DEPTH: usize = 32;

/// Number of nodes in a TOC tree.
pub fn toc_len(nodes: &[TocNode]) -> usize {
    nodes.iter().map(|n| 1 + toc_len(&n.children)).sum()
}

/// Maximum nesting depth of a TOC tree (1 for a flat list, 0 when empty).
pub fn toc_depth(nodes: &[TocNode]) -> usize {
    nodes.iter().map(|n| 1 + toc_depth(&n.children)).max().unwrap_or(0)
}

/// A printed page label (EPUB page list).
#[derive(Clone, Debug, PartialEq)]
pub struct PageTarget {
    /// Label as printed (`xii`, `42`).
    pub label: String,
    /// Where the page starts.
    pub target: Target,
}

/// A book identifier with its scheme (`ISBN`, `UUID`, `ASIN`, `calibre`, ...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identifier {
    /// Scheme, if the book names one.
    pub scheme: Option<String>,
    /// The value as written.
    pub value: String,
}

/// Cover image bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Cover {
    /// Path inside the book (readable through the book's resource loader).
    pub path: String,
    /// Media type (`image/jpeg`, `image/png`, ...).
    pub media_type: String,
    /// The image file.
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Cover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cover").field("path", &self.path).field("media_type", &self.media_type).field("bytes", &self.bytes.len()).finish()
    }
}

/// What the library needs to know about a book file without laying it out.
#[derive(Clone, Debug, PartialEq)]
pub struct BookInfo {
    /// Detected format.
    pub format: Format,
    /// Format detail for the book details page (`EPUB 3.0`, `KF8`, `MOBI 6`, `TXT windows-1251`).
    pub version: String,
    /// Title, authors, language, main identifier, publisher, description, cover path, series.
    pub metadata: Metadata,
    /// Every identifier the book declares.
    pub identifiers: Vec<Identifier>,
    /// Subjects / genres / keywords.
    pub subjects: Vec<String>,
    /// Publication date as written.
    pub date: Option<String>,
    /// Embedded font files (paths inside the book).
    pub fonts: Vec<String>,
    /// Cover image, when the book has one.
    pub cover: Option<Cover>,
    /// DRM scheme when the book is protected (`Adobe ADEPT`, `Kindle (Mobipocket)`, ...).
    pub drm: Option<String>,
    /// Why the book cannot be opened (DRM, KFX); `None` when it can be.
    pub refusal: Option<String>,
}

impl BookInfo {
    /// Empty info for `format`.
    pub fn new(format: Format) -> Self {
        BookInfo {
            format,
            version: String::new(),
            metadata: Metadata::default(),
            identifiers: Vec::new(),
            subjects: Vec::new(),
            date: None,
            fonts: Vec::new(),
            cover: None,
            drm: None,
            refusal: None,
        }
    }

    /// True when the book can be opened.
    pub fn readable(&self) -> bool {
        self.refusal.is_none()
    }
}

/// A book converted for the engine.
pub struct Book {
    /// Metadata and cover.
    pub info: BookInfo,
    /// XHTML chapters in reading order (`DocFragment[i + 1]`).
    pub chapters: Vec<HtmlChapter>,
    /// Nested table of contents.
    pub toc: Vec<TocNode>,
    /// Printed page labels.
    pub page_list: Vec<PageTarget>,
    /// Images, stylesheets and fonts by path.
    pub resources: Box<dyn ResourceLoader>,
}

impl std::fmt::Debug for Book {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Book")
            .field("title", &self.info.metadata.title)
            .field("chapters", &self.chapters.len())
            .field("toc", &toc_len(&self.toc))
            .field("page_list", &self.page_list.len())
            .finish()
    }
}

/// Element path from a chapter's `<body>` to the element with each wanted id.
///
/// The element tree is the one [`xml::parse`] builds (same `quick-xml` events, same
/// tolerance for mismatched end tags, same depth limit), but only element names, the
/// child order and the wanted `id`/`name` values are kept: no text, no other
/// attributes. Names are interned, so a 100 000-element chapter allocates a few
/// hundred small strings instead of one per element, text node and attribute. The walk
/// then descends only into elements with a wanted id below them.
fn id_paths(xhtml: &str, wanted: &HashSet<&str>) -> HashMap<String, Vec<Step>> {
    let mut out = HashMap::new();
    if wanted.is_empty() {
        return out;
    }
    let Some(t) = Skeleton::parse(xhtml, wanted) else { return out };
    let Some(&root) = t.nodes[0].children.first() else { return out };
    let body = t.find(root, t.body_name);
    if let Some(body) = body {
        let mut path = Vec::new();
        t.walk(body, &mut path, &mut out);
    }
    out
}

/// One element of a [`Skeleton`].
struct SkelNode {
    /// Interned local name.
    name: u32,
    /// Child elements in document order.
    children: Vec<u32>,
    /// The first `id` / `name` attribute (local name), when its value is wanted.
    ids: [Option<String>; 2],
    /// An element below this one has a wanted id.
    wanted_below: bool,
}

/// The element structure of an XHTML source, as [`xml::parse`] would nest it.
struct Skeleton {
    /// Node 0 is the document.
    nodes: Vec<SkelNode>,
    names: Vec<String>,
    /// Interned id of `body` (`u32::MAX` when the source has no such element).
    body_name: u32,
}

impl Skeleton {
    fn parse(src: &str, wanted: &HashSet<&str>) -> Option<Skeleton> {
        use quick_xml::events::Event;
        let mut r = quick_xml::Reader::from_str(src);
        r.config_mut().check_end_names = false;
        let mut nodes = vec![SkelNode { name: u32::MAX, children: Vec::new(), ids: [None, None], wanted_below: false }];
        let mut stack: Vec<u32> = vec![0];
        let mut skipped = 0usize;
        // Local name as `xml::qname_parts` takes it: after the first `:`.
        fn local(q: &[u8]) -> std::borrow::Cow<'_, str> {
            let s = String::from_utf8_lossy(q);
            match s {
                std::borrow::Cow::Borrowed(b) => std::borrow::Cow::Borrowed(b.split_once(':').map_or(b, |(_, l)| l)),
                std::borrow::Cow::Owned(o) => std::borrow::Cow::Owned(o.split_once(':').map_or(o.as_str(), |(_, l)| l).to_string()),
            }
        }
        struct Names {
            list: Vec<String>,
            ids: HashMap<String, u32>,
        }
        let mut nm = Names { list: Vec::new(), ids: HashMap::new() };
        fn add(e: &quick_xml::events::BytesStart, nodes: &mut Vec<SkelNode>, stack: &[u32], nm: &mut Names, wanted: &HashSet<&str>) -> u32 {
            let q = e.name();
            let l = local(q.as_ref());
            let name = match nm.ids.get(l.as_ref()) {
                Some(&n) => n,
                None => {
                    let n = nm.list.len() as u32;
                    nm.list.push(l.to_string());
                    nm.ids.insert(l.into_owned(), n);
                    n
                }
            };
            let mut ids: [Option<String>; 2] = [None, None];
            let mut seen = [false, false];
            for a in e.attributes().with_checks(false).flatten() {
                let k = match a.key.local_name().as_ref() {
                    b"id" => 0,
                    b"name" => 1,
                    _ => continue,
                };
                if std::mem::replace(&mut seen[k], true) {
                    continue; // `Element::attr` takes the first one
                }
                // Copied only when wanted (some books give every paragraph an id).
                let v = match a.unescape_value_with(xml::html_entity) {
                    Ok(v) => v,
                    Err(_) => String::from_utf8_lossy(&a.value),
                };
                if wanted.contains(v.as_ref()) {
                    ids[k] = Some(v.into_owned());
                }
            }
            let id = nodes.len() as u32;
            if ids.iter().any(Option::is_some) {
                // The open elements are this one's ancestors.
                for &a in stack.iter().rev() {
                    if std::mem::replace(&mut nodes[a as usize].wanted_below, true) {
                        break;
                    }
                }
            }
            nodes.push(SkelNode { name, children: Vec::new(), ids, wanted_below: false });
            nodes[*stack.last().expect("document") as usize].children.push(id);
            id
        }
        loop {
            let ev = match r.read_event() {
                Ok(ev) => ev,
                Err(_) => {
                    // `xml::parse` keeps what was parsed so far, unless that is nothing.
                    if stack.len() > 1 || !nodes[0].children.is_empty() {
                        break;
                    }
                    return None;
                }
            };
            match ev {
                Event::Start(e) => {
                    if stack.len() > xml::MAX_DEPTH {
                        skipped += 1;
                    } else {
                        let id = add(&e, &mut nodes, &stack, &mut nm, wanted);
                        stack.push(id);
                    }
                }
                Event::Empty(e) => {
                    add(&e, &mut nodes, &stack, &mut nm, wanted);
                }
                Event::End(e) => {
                    if skipped > 0 {
                        skipped -= 1;
                        continue;
                    }
                    let q = e.name();
                    let l = local(q.as_ref());
                    let Some(&name) = nm.ids.get(l.as_ref()) else { continue };
                    if let Some(pos) = stack.iter().rposition(|&n| nodes[n as usize].name == name) {
                        if pos > 0 {
                            stack.truncate(pos);
                        }
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        let body_name = nm.ids.get("body").copied().unwrap_or(u32::MAX);
        Some(Skeleton { nodes, names: nm.list, body_name })
    }

    /// First element named `name` at or below `n`, depth first (as `Element::find`).
    fn find(&self, n: u32, name: u32) -> Option<u32> {
        if self.nodes[n as usize].name == name {
            return Some(n);
        }
        self.nodes[n as usize].children.iter().find_map(|&c| self.find(c, name))
    }

    /// Records the element path of every wanted id below `el`; `path` holds the steps
    /// down to `el` as (interned name, index).
    fn walk(&self, el: u32, path: &mut Vec<(u32, Option<u32>)>, out: &mut HashMap<String, Vec<Step>>) {
        let kids = &self.nodes[el as usize].children;
        // Siblings per name, counted once (a chapter body can have 50 000 paragraphs).
        let mut totals: HashMap<u32, u32> = HashMap::new();
        for &k in kids {
            *totals.entry(self.nodes[k as usize].name).or_insert(0) += 1;
        }
        let mut seen: HashMap<u32, u32> = HashMap::new();
        for &k in kids {
            let node = &self.nodes[k as usize];
            let n = seen.entry(node.name).or_insert(0);
            *n += 1;
            let has_id = node.ids.iter().any(Option::is_some);
            if !has_id && !node.wanted_below {
                continue;
            }
            path.push((node.name, (totals[&node.name] > 1).then_some(*n)));
            for id in node.ids.iter().flatten() {
                if !out.contains_key(id) {
                    let steps = path.iter().map(|&(name, index)| Step { name: self.names[name as usize].clone(), index }).collect();
                    out.insert(id.clone(), steps);
                }
            }
            if node.wanted_below {
                self.walk(k, path, out);
            }
            path.pop();
        }
    }
}

/// The tree walk [`id_paths`] replaced (reference for the tests).
#[cfg(test)]
fn id_paths_tree(xhtml: &str, wanted: &HashSet<&str>) -> HashMap<String, Vec<Step>> {
    let mut out = HashMap::new();
    if wanted.is_empty() {
        return out;
    }
    let Ok(root) = xml::parse(xhtml) else { return out };
    let body = if root.name == "body" { Some(&root) } else { root.find("body") };
    if let Some(body) = body {
        let mut path = Vec::new();
        walk_ids(body, &mut path, wanted, &mut out);
    }
    out
}

#[cfg(test)]
fn walk_ids(el: &Element, path: &mut Vec<Step>, wanted: &HashSet<&str>, out: &mut HashMap<String, Vec<Step>>) {
    let kids: Vec<&Element> = el.elements().collect();
    let mut totals: HashMap<&str, u32> = HashMap::new();
    for k in &kids {
        *totals.entry(k.name.as_str()).or_insert(0) += 1;
    }
    let mut seen: HashMap<&str, u32> = HashMap::new();
    for k in &kids {
        let n = seen.entry(k.name.as_str()).or_insert(0);
        *n += 1;
        let index = *n;
        let total = totals[k.name.as_str()];
        path.push(Step { name: k.name.clone(), index: (total > 1).then_some(index) });
        for key in ["id", "name"] {
            if let Some(id) = k.attr(key) {
                if wanted.contains(id) && !out.contains_key(id) {
                    out.insert(id.to_string(), path.clone());
                }
            }
        }
        walk_ids(k, path, wanted, out);
        path.pop();
    }
}

/// Resolves targets to positions, parsing each chapter at most once.
pub struct Resolver<'a> {
    chapters: Vec<&'a str>,
    cache: HashMap<usize, HashMap<String, Vec<Step>>>,
}

impl<'a> Resolver<'a> {
    /// Prepares resolution of `targets` in `chapters`.
    pub fn new<'t>(chapters: &'a [HtmlChapter], targets: impl IntoIterator<Item = &'t Target>) -> Self {
        Self::from_sources(chapters.iter().map(|c| c.html.as_str()).collect(), targets)
    }

    /// [`Resolver::new`] over the XHTML sources of the chapters.
    pub fn from_sources<'t>(chapters: Vec<&'a str>, targets: impl IntoIterator<Item = &'t Target>) -> Self {
        let mut wanted: HashMap<usize, HashSet<&str>> = HashMap::new();
        for t in targets {
            if let Some(id) = &t.id {
                wanted.entry(t.chapter).or_default().insert(id.as_str());
            }
        }
        // Each chapter with targets is parsed once; big books spread that over threads.
        let jobs: Vec<(usize, HashSet<&str>)> = wanted.into_iter().filter(|(ch, _)| *ch < chapters.len()).collect();
        let bytes: usize = jobs.iter().map(|(ch, _)| chapters[*ch].len()).sum();
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(4);
        let threads = if bytes > 512 * 1024 { cores.min(jobs.len()).max(1) } else { 1 };
        let cache = if threads <= 1 {
            jobs.into_iter().map(|(ch, ids)| (ch, id_paths(chapters[ch], &ids))).collect()
        } else {
            let per = jobs.len().div_ceil(threads);
            let chapters = &chapters;
            std::thread::scope(|scope| {
                let handles: Vec<_> = jobs
                    .chunks(per)
                    .map(|part| scope.spawn(move || part.iter().map(|(ch, ids)| (*ch, id_paths(chapters[*ch], ids))).collect::<Vec<_>>()))
                    .collect();
                // A panicking part loses its targets (they open at the chapter start).
                handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
            })
        };
        Resolver { chapters, cache }
    }

    /// The xpointer of `t`: the element path when the id is found, else the chapter start.
    pub fn position(&self, t: &Target) -> Position {
        let n = t.chapter.min(self.chapters.len().saturating_sub(1)) as u32 + 1;
        let mut x = XPointer::fragment_start(n);
        if let Some(steps) = t.id.as_ref().and_then(|id| self.cache.get(&t.chapter)?.get(id)) {
            x.steps.extend(steps.iter().cloned());
        }
        Position::XPointer(x)
    }
}

/// Every TOC target (depth first), then every page-list target: the order of
/// [`assemble`] and of a resolved-TOC cache file.
fn all_targets<'a>(toc: &'a [TocNode], page_list: &'a [PageTarget]) -> Vec<&'a Target> {
    fn walk<'a>(nodes: &'a [TocNode], out: &mut Vec<&'a Target>) {
        for n in nodes {
            out.extend(&n.target);
            walk(&n.children, out);
        }
    }
    let mut out = Vec::new();
    walk(toc, &mut out);
    out.extend(page_list.iter().map(|p| &p.target));
    out
}

/// The flattened TOC (levels from 0) and the page labels, given the position of each
/// target of [`all_targets`] in order.
fn assemble(toc: &[TocNode], page_list: &[PageTarget], positions: Vec<Position>) -> ResolvedToc {
    fn flatten(nodes: &[TocNode], level: u8, next: &mut dyn FnMut() -> Position, out: &mut Vec<TocEntry>) {
        for n in nodes {
            let mut child_level = level;
            if n.target.is_some() {
                out.push(TocEntry { title: n.title.clone(), level, position: next(), page: None });
                child_level = level.saturating_add(1);
            }
            flatten(&n.children, child_level, next, out);
        }
    }
    let mut it = positions.into_iter();
    let mut next = || it.next().unwrap_or_else(|| Position::XPointer(XPointer::fragment_start(1)));
    let mut entries = Vec::new();
    flatten(toc, 0, &mut next, &mut entries);
    let labels = page_list.iter().map(|p| (p.label.clone(), next())).collect();
    (entries, labels)
}

/// First line of a resolved-TOC cache file.
const TOC_FILE_HEADER: &str = "rinky-toc 1";

/// FNV-1a hash of the targets (chapter and id) and the chapter count: a cache file
/// written for other targets is not used.
fn targets_hash(targets: &[&Target], chapters: usize) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut add = |b: &[u8]| {
        for &x in b {
            h = (h ^ x as u64).wrapping_mul(0x100000001b3);
        }
    };
    add(&(chapters as u64).to_le_bytes());
    for t in targets {
        add(&(t.chapter as u64).to_le_bytes());
        add(t.id.as_deref().unwrap_or("").as_bytes());
        add(&[0xff]);
    }
    h
}

/// Reads the resolved positions of `targets` from a cache file.
fn read_toc_file(path: &std::path::Path, hash: u64, count: usize) -> Option<Vec<Position>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != format!("{TOC_FILE_HEADER} {count} {hash:016x}") {
        return None;
    }
    let out: Vec<Position> = lines.map(|l| l.parse::<Position>().ok()).collect::<Option<_>>()?;
    (out.len() == count).then_some(out)
}

/// Writes the resolved positions (temporary file, then rename).
fn write_toc_file(path: &std::path::Path, hash: u64, positions: &[Position]) -> std::io::Result<()> {
    let mut s = format!("{TOC_FILE_HEADER} {} {hash:016x}\n", positions.len());
    for p in positions {
        s.push_str(&p.to_string());
        s.push('\n');
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("toc-tmp");
    std::fs::write(&tmp, s)?;
    std::fs::rename(&tmp, path)
}

impl Book {
    /// The flattened TOC (levels from 0) and page labels as xpointer positions.
    pub fn positions(&self) -> (Vec<TocEntry>, Vec<(String, Position)>) {
        let targets = all_targets(&self.toc, &self.page_list);
        let r = Resolver::new(&self.chapters, targets.iter().copied());
        assemble(&self.toc, &self.page_list, targets.iter().map(|t| r.position(t)).collect())
    }

    /// Builds the engine document (without the book's TOC; see [`Book::into_document`]).
    pub fn into_html_document(self) -> Result<HtmlDocument> {
        HtmlDocument::new(self.info.metadata, self.chapters, self.resources)
    }

    /// Builds the engine document wrapped with the book's TOC, page list and info. The
    /// TOC and page-list targets are resolved to xpointers when first needed (see
    /// [`BookDocument`]).
    pub fn into_document(self) -> Result<BookDocument> {
        let t = std::time::Instant::now();
        let info = self.info.clone();
        let doc = HtmlDocument::new(self.info.metadata, self.chapters, self.resources)?;
        ink_doc::timing::step("styles+fonts", t);
        Ok(BookDocument {
            doc,
            info,
            nodes: self.toc,
            page_list: self.page_list,
            resolved: std::cell::OnceCell::new(),
            toc_file: None,
            interest_set: false,
        })
    }
}

/// The flattened table of contents and the page labels, with xpointer positions.
type ResolvedToc = (Vec<TocEntry>, Vec<(String, Position)>);

/// [`HtmlDocument`] plus the book's own TOC (NCX/nav, FB2 sections, MOBI NCX), page list
/// and [`BookInfo`]. Every other [`Document`] method goes to the engine.
///
/// The TOC and page-list targets (chapter + element id) become xpointers on first use:
/// in `set_layout` (the paginators number them while they hold each chapter) or in
/// `toc` / `page_labels`. That needs a scan of every chapter with targets; with a page
/// cache ([`Document::set_page_cache`]) the result is kept in `<dir>/<book id>.toc`, so
/// a book opened again reads it instead.
pub struct BookDocument {
    doc: HtmlDocument,
    info: BookInfo,
    /// The book's TOC, unresolved.
    nodes: Vec<TocNode>,
    /// The book's page list, unresolved.
    page_list: Vec<PageTarget>,
    /// The flattened TOC and page labels with xpointers.
    resolved: std::cell::OnceCell<ResolvedToc>,
    /// Resolved positions cache file.
    toc_file: Option<std::path::PathBuf>,
    /// The engine has the resolved positions as interest positions.
    interest_set: bool,
}

impl BookDocument {
    /// The resolved TOC and page labels (resolved now when needed).
    fn resolved(&self) -> &ResolvedToc {
        self.resolved.get_or_init(|| self.resolve())
    }

    fn resolve(&self) -> ResolvedToc {
        let t = std::time::Instant::now();
        let targets = all_targets(&self.nodes, &self.page_list);
        if targets.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let chapters = &self.doc.book().chapters;
        let hash = targets_hash(&targets, chapters.len());
        let cached = self.toc_file.as_deref().and_then(|f| read_toc_file(f, hash, targets.len()));
        let from_file = cached.is_some();
        let positions = match cached {
            Some(p) => p,
            None => {
                let r = Resolver::from_sources(chapters.iter().map(|c| c.html.as_str()).collect(), targets.iter().copied());
                let p: Vec<Position> = targets.iter().map(|t| r.position(t)).collect();
                if let Some(f) = &self.toc_file {
                    if let Err(e) = write_toc_file(f, hash, &p) {
                        log::warn!("cannot write {}: {e}", f.display());
                    }
                }
                p
            }
        };
        let resolved = assemble(&self.nodes, &self.page_list, positions);
        ink_doc::timing::step(if from_file { "toc cache read" } else { "toc resolve" }, t);
        resolved
    }
}

impl std::fmt::Debug for BookDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BookDocument").field("doc", &self.doc).field("toc", &toc_len(&self.nodes)).finish()
    }
}

impl BookDocument {
    /// Format, metadata, identifiers, cover and DRM status.
    pub fn info(&self) -> &BookInfo {
        &self.info
    }

    /// The engine document.
    pub fn html(&self) -> &HtmlDocument {
        &self.doc
    }
}

impl std::ops::Deref for BookDocument {
    type Target = HtmlDocument;
    fn deref(&self) -> &HtmlDocument {
        &self.doc
    }
}

impl std::ops::DerefMut for BookDocument {
    fn deref_mut(&mut self) -> &mut HtmlDocument {
        &mut self.doc
    }
}

impl Document for BookDocument {
    fn metadata(&self) -> &Metadata {
        self.doc.metadata()
    }
    fn kind(&self) -> DocKind {
        self.doc.kind()
    }
    fn set_layout(&mut self, settings: &LayoutSettings) -> Result<()> {
        if !self.interest_set {
            // The paginators number the TOC and page labels while they have each chapter.
            let (toc, labels) = self.resolved();
            let interest: Vec<Position> = toc.iter().map(|e| e.position.clone()).chain(labels.iter().map(|(_, p)| p.clone())).collect();
            self.doc.set_interest_positions(&interest);
            self.interest_set = true;
        }
        self.doc.set_layout(settings)
    }
    fn layout(&self) -> &LayoutSettings {
        self.doc.layout()
    }
    fn page_count(&self) -> PageCount {
        self.doc.page_count()
    }
    fn render_page(&mut self, page: usize, canvas: &mut Canvas) -> Result<()> {
        self.doc.render_page(page, canvas)
    }
    fn position_of_page(&self, page: usize) -> Result<Position> {
        self.doc.position_of_page(page)
    }
    fn page_of(&self, pos: &Position) -> Result<usize> {
        self.doc.page_of(pos)
    }
    fn current_page(&self) -> usize {
        self.doc.current_page()
    }
    fn set_current_page(&mut self, page: usize) {
        self.doc.set_current_page(page)
    }
    fn current_position(&self) -> Result<Position> {
        self.doc.current_position()
    }
    /// The book's TOC (NCX/nav, FB2 sections, MOBI NCX, guessed TXT chapters); the
    /// engine's heading TOC when the book has none.
    fn toc(&self) -> Vec<TocEntry> {
        let toc = &self.resolved().0;
        if toc.is_empty() {
            return self.doc.toc();
        }
        // Only pages known without paginating more: the reader asks for the TOC while it
        // draws the first page, and `page_of` would lay out the whole book then.
        let has_pages = self.doc.page_count().pages > 0;
        toc.iter()
            .map(|e| TocEntry { page: if has_pages { self.doc.page_of_known(&e.position) } else { None }, ..e.clone() })
            .collect()
    }
    fn search(&mut self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.doc.search(query, limit)
    }
    fn read_around(&mut self, pos: &Position, chars: usize) -> Result<String> {
        self.doc.read_around(pos, chars)
    }
    fn links(&mut self, pos: &Position) -> Result<Vec<Link>> {
        self.doc.links(pos)
    }
    fn select(&mut self, page: usize, from: Point, to: Point) -> Result<Option<Selection>> {
        self.doc.select(page, from, to)
    }
    fn selection(&self) -> Option<&Selection> {
        self.doc.selection()
    }
    fn clear_selection(&mut self) {
        self.doc.clear_selection()
    }
    fn set_selection(&mut self, sel: Selection) {
        self.doc.set_selection(sel)
    }
    fn range_rects(&mut self, page: usize, start: &Position, end: &Position) -> Vec<ink_device::Rect> {
        self.doc.range_rects(page, start, end)
    }
    fn hit_rects(&mut self, page: usize, hit: &SearchHit) -> Vec<ink_device::Rect> {
        self.doc.hit_rects(page, hit)
    }
    fn location(&self, pos: &Position) -> Option<Location> {
        self.doc.location(pos)
    }
    fn location_count(&self) -> Option<Location> {
        self.doc.location_count()
    }
    /// Printed page labels from the book's page list; the engine's when the book has none.
    fn page_labels(&self) -> Vec<(String, Position)> {
        let labels = &self.resolved().1;
        if labels.is_empty() {
            self.doc.page_labels()
        } else {
            labels.clone()
        }
    }
    // Added to `Document` by the phase 2 engine (wave-a/html); without these the trait
    // defaults would hide the engine's notes, page-map cache and pagination wait.
    fn note(&mut self, target: &Position) -> Result<Option<ink_doc::Note>> {
        self.doc.note(target)
    }
    fn set_page_cache(&mut self, dir: &std::path::Path, book_id: &str) {
        let id: String = book_id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if !id.is_empty() && self.resolved.get().is_none() {
            self.toc_file = Some(dir.join(format!("{id}.toc")));
        }
        self.doc.set_page_cache(dir, book_id)
    }
    fn wait_paginated(&mut self, timeout: std::time::Duration) -> bool {
        self.doc.wait_paginated(timeout)
    }
    fn page_estimate(&self) -> Option<usize> {
        self.doc.page_estimate()
    }
    fn page_of_known(&self, pos: &Position) -> Option<usize> {
        self.doc.page_of_known(pos)
    }
    fn spine_starts(&self) -> Vec<usize> {
        self.doc.spine_starts()
    }
    fn open_at(&mut self, pos: &Position) {
        self.doc.open_at(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_doc::MemoryLoader;

    fn book() -> Book {
        let ch = |p: &str, body: &str| HtmlChapter { path: p.into(), html: format!("<html><head><title>x</title></head><body>{body}</body></html>") };
        let long = "<p>Lorem ipsum dolor sit amet.</p>".repeat(150);
        Book {
            info: BookInfo::new(Format::Epub),
            chapters: vec![
                ch("a.xhtml", "<h1 id='top'>One</h1><p>x</p>"),
                ch("b.xhtml", &format!("<div><h2>Two</h2>{long}<div><p id='deep'>d</p></div><p name='old'>o</p></div><div/>")),
            ],
            toc: vec![TocNode {
                title: "One".into(),
                target: Some(Target::id(0, "top")),
                children: vec![TocNode::new("Two", Target::chapter(1)), TocNode::new("Deep", Target::id(1, "deep"))],
            }],
            page_list: vec![PageTarget { label: "ii".into(), target: Target::id(1, "old") }],
            resources: Box::new(MemoryLoader::default()),
        }
    }

    #[test]
    fn targets_become_crengine_xpointers() {
        let b = book();
        let (toc, labels) = b.positions();
        let s: Vec<(String, u8, String)> = toc.iter().map(|e| (e.title.clone(), e.level, e.position.to_string())).collect();
        assert_eq!(
            s,
            vec![
                ("One".into(), 0, "/body/DocFragment[1]/body/h1".into()),
                ("Two".into(), 1, "/body/DocFragment[2]/body".into()),
                ("Deep".into(), 1, "/body/DocFragment[2]/body/div[1]/div/p".into()),
            ]
        );
        assert_eq!(labels[0].1.to_string(), "/body/DocFragment[2]/body/div[1]/p[151]");
        assert_eq!(toc_len(&b.toc), 3);
        assert_eq!(toc_depth(&b.toc), 2);
    }

    /// Every `id`/`name` value of a source, through the old tree.
    fn all_ids(src: &str) -> Vec<String> {
        fn walk(e: &Element, out: &mut Vec<String>) {
            for k in ["id", "name"] {
                if let Some(v) = e.attr(k) {
                    out.push(v.to_string());
                }
            }
            for c in e.elements() {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        if let Ok(root) = xml::parse(src) {
            walk(&root, &mut out);
        }
        out
    }

    fn same_paths(src: &str, extra: &[&str]) {
        let mut ids = all_ids(src);
        ids.extend(extra.iter().map(|s| s.to_string()));
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        assert_eq!(id_paths(src, &wanted), id_paths_tree(src, &wanted), "{}", &src[..src.len().min(200)]);
    }

    #[test]
    fn streaming_id_paths_match_the_tree_walk() {
        // Odd sources: mismatched and unclosed tags, prefixes, entities, duplicate ids,
        // xml:id, a body that is the root, no body, deep nesting, broken XML.
        let deep = format!("<html><body>{}<p id='deep'>x</p>{}</body></html>", "<div>".repeat(300), "</div>".repeat(300));
        for src in [
            "<html><head><title>t</title></head><body><p id='a'>1</p><p id=\"b\">2<b id='c'/></p><div><p name='d'>3</p></div></body></html>",
            "<html><body><p id='a'>1<span id='b'>x</p><p id='c'>2</p></div><p id='d'/></body></html>",
            "<h:html xmlns:h='x'><h:body><h:p id='a'/><h:p xml:id='b'/><h:p id='a'/></h:body></h:html>",
            "<body><p id='x&amp;y'>1</p><p id='&eacute;'>2</p><p id='a' id='b'>3</p></body>",
            "<html><p id='nobody'>1</p></html>",
            "<html><body><p id='a'>1</p><p id='b'>2",
            "<html><body><p id='a'>1</p></body></html><extra id='z'/>",
            "<html><body><p id='a'>& broken <</p></body></html>",
            "not xml at all",
            "",
            &deep,
        ] {
            same_paths(src, &["missing"]);
        }
        // Every chapter of the EPUB test books.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        let mut books: Vec<std::path::PathBuf> = vec![root.join("synthetic.epub"), root.join("juliet.epub"), root.join("formats/epub2-ncx.epub"), root.join("formats/epub3-nav.epub")];
        // Extra books from the environment (big synthetic books for timing).
        if let Some(more) = std::env::var_os("INK_ID_PATH_BOOKS") {
            books.extend(std::env::split_paths(&more));
        }
        let mut chapters = 0;
        for b in books {
            let book = crate::epub::load(&b).unwrap_or_else(|e| panic!("{}: {e}", b.display()));
            for ch in &book.chapters {
                same_paths(&ch.html, &[]);
                chapters += 1;
            }
        }
        assert!(chapters > 5, "{chapters} chapters checked");
    }

    #[test]
    fn lazy_toc_and_its_cache_file_match_the_eager_resolution() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        let mut books: Vec<std::path::PathBuf> = vec![root.join("juliet.epub"), root.join("formats/epub2-ncx.epub"), root.join("formats/epub3-nav.epub"), root.join("formats/sample.fb2")];
        if let Some(more) = std::env::var_os("INK_ID_PATH_BOOKS") {
            books.extend(std::env::split_paths(&more));
        }
        let dir = std::env::temp_dir().join(format!("ink-toc-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let flat = |toc: &[TocEntry]| toc.iter().map(|e| (e.title.clone(), e.level, e.position.to_string())).collect::<Vec<_>>();
        let flat_l = |l: &[(String, Position)]| l.iter().map(|(a, p)| (a.clone(), p.to_string())).collect::<Vec<_>>();
        for (k, b) in books.iter().enumerate() {
            let (want_toc, want_labels) = crate::load(b).unwrap().positions();
            assert!(!want_toc.is_empty(), "{}", b.display());
            let id = format!("book{k}");
            // First open: resolved from the chapters, file written.
            let mut d = crate::load(b).unwrap().into_document().unwrap();
            d.set_page_cache(&dir, &id);
            let (toc, labels) = d.resolved();
            assert_eq!(flat(toc), flat(&want_toc), "{}", b.display());
            assert_eq!(flat_l(labels), flat_l(&want_labels), "{}", b.display());
            let file = dir.join(format!("{id}.toc"));
            assert!(file.exists());
            // Reopen: from the file, the same entries.
            let mut d = crate::load(b).unwrap().into_document().unwrap();
            d.set_page_cache(&dir, &id);
            let (toc, labels) = d.resolved();
            assert_eq!(flat(toc), flat(&want_toc), "{} (cache file)", b.display());
            assert_eq!(flat_l(labels), flat_l(&want_labels), "{} (cache file)", b.display());
            // A file for other targets is not used.
            let text = std::fs::read_to_string(&file).unwrap();
            let mut lines: Vec<&str> = text.lines().collect();
            let bad_head = format!("{TOC_FILE_HEADER} {} 0000000000000000", want_toc.len() + want_labels.len());
            lines[0] = &bad_head;
            lines[1] = "/body/DocFragment[1]/body/nowhere";
            std::fs::write(&file, lines.join("\n")).unwrap();
            let mut d = crate::load(b).unwrap().into_document().unwrap();
            d.set_page_cache(&dir, &id);
            assert_eq!(flat(&d.resolved().0), flat(&want_toc), "{} (stale file)", b.display());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrapper_delegates_and_maps_toc_to_pages() {
        let mut d = book().into_document().unwrap();
        d.set_layout(&LayoutSettings::for_screen(1272, 1696)).unwrap();
        let toc = d.toc();
        assert_eq!(toc.len(), 3);
        assert_eq!(toc[0].page, Some(0));
        assert_eq!(toc[1].page, Some(1));
        assert!(toc[2].page.unwrap() >= 1);
        assert_eq!(d.page_labels().len(), 1);
        assert!(d.page_count().pages >= 3);
        assert_eq!(d.chapter_count(), 2);
        assert_eq!(d.info().format, Format::Epub);
    }
}
