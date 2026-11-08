//! [`HtmlDocument`]: the reflowable [`Document`].

use crate::book::Book;
use crate::fonts::{FontBook, FontSources};
use crate::images::ImageCache;
use crate::layout::{ChapterLayout, Draw, Env, Params, TextRun, NO_LINK};
use crate::nav::{Navigation, NavPoint};
use crate::paginate::{self, chapter_text_bytes, layout_chapter, paginate, ChapterPages, Claim, Interest, LaidOut, MapState, PageSpan, Shared, Worker};
use crate::xpointer::{order_key, resolve_lenient, to_xpointer, DomPos};
use ink_device::{Canvas, Point, Rect};
use ink_doc::{
    DocError, DocKind, Document, FlowAnchor, LayoutSettings, Link, LinkTarget, Location, Metadata, Note, NoteKind, PageCount,
    Position, ResourceLoader, Result, SearchHit, Selection, TocEntry, XPointer,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One spine item handed to the engine.
#[derive(Clone, Debug, PartialEq)]
pub struct HtmlChapter {
    /// Path inside the book (for resolving links and resources). Empty for a spine
    /// entry whose manifest item is missing (it gets no DocFragment, as in crengine).
    pub path: String,
    /// XHTML source.
    pub html: String,
}

/// Visible text of a chapter in reading order, with the DOM position of each char.
struct FlatText {
    chars: Vec<char>,
    /// (text node, char offset); separators map to the following text.
    map: Vec<(u32, u32)>,
}

/// A resolved TOC entry: title, level, target, (spine item, position key).
type TocItem = (String, u8, Position, Option<(usize, (u32, u32))>);

/// Pages a prefix layout of the chapter in view reaches past the page opened (the
/// reader keeps three pages after the current one known, see `page_count`).
const PREFIX_PAGES_AFTER: usize = 4;

/// How long a page turn past a prefix layout waits for the background full layout
/// before laying the chapter out itself.
const FULL_LAYOUT_WAIT: Duration = Duration::from_secs(10);

/// A reflowable document.
pub struct HtmlDocument {
    book: Arc<Book>,
    resources: Box<dyn ResourceLoader>,
    nav: Navigation,
    layout: LayoutSettings,
    params: Option<Params>,
    shared: Arc<Shared>,
    worker: Option<Worker>,
    cache: Option<(PathBuf, String)>,
    /// The cache file of the current layout.
    cache_path: Option<PathBuf>,
    /// The page map of the current layout came from the cache file.
    cache_hit: bool,
    /// The foreground wrote the cache file of the current layout, or must not (the map
    /// was corrected after a stale cache).
    cache_done: Cell<bool>,
    font_sources: FontSources,
    fonts: RefCell<FontBook>,
    laid: RefCell<HashMap<usize, Arc<LaidOut>>>,
    flats: RefCell<HashMap<usize, Arc<FlatText>>>,
    images: RefCell<ImageCache>,
    /// The page the reader shows.
    current: Cell<usize>,
    /// (chapter, key) of the reading position: the start of the page the reader turned
    /// to, or the position the book was opened at. Kept across relayouts (font size,
    /// rotation and back return to the same page) until the reader turns a page.
    reading: Cell<Option<(usize, (u32, u32))>>,
    /// (chapter, key) the next `set_layout` opens at ([`HtmlDocument::set_anchor`]).
    anchor: Option<(usize, (u32, u32))>,
    /// Spine item by path (as written and percent-decoded).
    paths: HashMap<String, usize>,
    /// Resolved image paths of the spine (scanned once, for the size prefill).
    image_refs: RefCell<Option<Arc<Vec<String>>>>,
    selection: Option<Selection>,
    toc_cache: RefCell<Option<Vec<TocItem>>>,
    /// Positions looked up before (TOC entries, bookmarks): position text -> (spine
    /// item, page within it, hash of the item's page starts). Saved next to the
    /// page-map cache file, so a reopened book numbers its TOC without parsing chapters.
    positions: RefCell<HashMap<String, PosMemo>>,
    /// `positions` has entries the file does not have.
    positions_dirty: Cell<bool>,
    /// Positions whose pages the paginators work out (see [`HtmlDocument::set_interest_positions`]).
    interest: Interest,
}

impl std::fmt::Debug for HtmlDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HtmlDocument").field("title", &self.book.meta.title).field("chapters", &self.book.chapters.len()).finish()
    }
}

fn bad(p: &Position) -> DocError {
    DocError::BadPosition(p.to_string())
}

impl HtmlDocument {
    /// Builds a document from spine items. Stylesheets and fonts are read now; chapters
    /// are parsed on demand. Fails when no spine item has content.
    pub fn new(meta: Metadata, chapters: Vec<HtmlChapter>, resources: Box<dyn ResourceLoader>) -> Result<Self> {
        if chapters.iter().all(|c| c.html.trim().is_empty()) {
            return Err(DocError::Format("no readable chapters in spine".into()));
        }
        let book = Book::new(meta, chapters.into_iter().map(|c| (c.path, c.html)).collect(), resources.as_ref());
        let mut font_sources = FontSources { fallback_dirs: crate::fonts::font_dirs(None), ..Default::default() };
        font_sources.add_font_faces(&book.font_faces, &book.fonts);
        let fonts = RefCell::new(font_sources.build_foreground());
        let mut paths = HashMap::new();
        for (i, ch) in book.chapters.iter().enumerate() {
            if ch.path.is_empty() {
                continue;
            }
            paths.entry(ch.path.clone()).or_insert(i);
            paths.entry(ink_doc::percent_decode(&ch.path)).or_insert(i);
        }
        Ok(HtmlDocument {
            book: Arc::new(book),
            resources,
            nav: Navigation::default(),
            layout: LayoutSettings::for_screen(0, 0),
            params: None,
            shared: Arc::new(Shared::default()),
            worker: None,
            cache: None,
            cache_path: None,
            cache_hit: false,
            cache_done: Cell::new(false),
            font_sources,
            fonts,
            laid: RefCell::new(HashMap::new()),
            flats: RefCell::new(HashMap::new()),
            images: RefCell::new(ImageCache::default()),
            current: Cell::new(0),
            reading: Cell::new(None),
            anchor: None,
            paths,
            image_refs: RefCell::new(None),
            selection: None,
            toc_cache: RefCell::new(None),
            positions: RefCell::new(HashMap::new()),
            positions_dirty: Cell::new(false),
            interest: Arc::new(Vec::new()),
        })
    }

    /// Sets the TOC and page list (from the EPUB navigation; see [`crate::nav`]).
    pub fn set_navigation(&mut self, nav: Navigation) {
        self.nav = nav;
        *self.toc_cache.borrow_mut() = None;
    }

    /// The navigation data.
    pub fn navigation(&self) -> &Navigation {
        &self.nav
    }

    /// Number of chapters with text (as the phase 0 engine counted them; `flow:`
    /// positions index these).
    pub fn chapter_count(&self) -> usize {
        self.book.chapters.iter().filter(|c| !crate::v1::blocks::parse_xhtml(&c.html).is_empty()).count()
    }

    /// Path inside the book of spine item `i`.
    pub fn chapter_path(&self, i: usize) -> Option<&str> {
        self.book.chapters.get(i).map(|c| c.path.as_str())
    }

    /// The resource loader (images, CSS) of this book.
    pub fn resources(&self) -> &dyn ResourceLoader {
        self.resources.as_ref()
    }

    /// The shared book data (DOMs, stylesheets).
    pub fn book(&self) -> &Arc<Book> {
        &self.book
    }

    /// Adds the system font family `name` (searched in the Kindle font directories and
    /// `extra_dir`) so `LayoutSettings::font_family` can select it. Returns false when
    /// no such font files exist.
    pub fn add_system_font(&mut self, name: &str, extra_dir: Option<&Path>) -> bool {
        let ok = self.font_sources.add_system_family(name, &crate::fonts::font_dirs(extra_dir));
        if ok {
            *self.fonts.borrow_mut() = self.font_sources.build_foreground();
        }
        ok
    }

    fn params(&self) -> Result<&Params> {
        self.params.as_ref().ok_or_else(|| DocError::Unsupported("set_layout first".into()))
    }

    fn fg_image_size(&self, path: &str) -> Option<(u32, u32)> {
        self.book.image_size(path, Some(self.resources.as_ref())).flatten()
    }

    /// Reads the sizes of every image referenced by the spine (used when the resource
    /// loader cannot be shared with the background thread). The background thread defers
    /// a chapter with an image this scan missed to the foreground.
    fn prefill_image_sizes(&self) {
        let cached = self.image_refs.borrow().clone();
        let paths = match cached {
            Some(p) => p,
            None => {
                let t = Instant::now();
                let mut seen = std::collections::HashSet::new();
                let mut paths = Vec::new();
                for ch in &self.book.chapters {
                    for v in crate::book::scan_image_refs(&ch.html) {
                        let p = ink_doc::resolve_href(&ch.dir, &v);
                        if seen.insert(p.clone()) {
                            paths.push(p);
                        }
                    }
                }
                log::debug!("ink-html: {} image references found in {} ms", paths.len(), t.elapsed().as_millis());
                let p = Arc::new(paths);
                *self.image_refs.borrow_mut() = Some(p.clone());
                p
            }
        };
        for p in paths.iter() {
            self.fg_image_size(p);
        }
    }

    fn with_env<R>(&self, f: impl FnOnce(&mut Env) -> R) -> R {
        let mut fonts = self.fonts.borrow_mut();
        let mut size = |p: &str| self.fg_image_size(p);
        let mut env = Env { fonts: &mut fonts, image_size: &mut size };
        f(&mut env)
    }

    /// Makes every chapter up to `upto` known in the page map. The foreground lays out
    /// chapter `upto` itself (and keeps that layout: the reader draws it next), then helps
    /// the background threads with the free chapters before it, lowest first, and waits
    /// for the ones a background thread is doing.
    fn ensure_paginated(&self, upto: usize) -> Result<()> {
        let params = self.params()?.clone();
        let upto = upto.min(self.book.chapters.len().saturating_sub(1));
        let mut waited = Duration::ZERO;
        loop {
            let pick = {
                let mut m = self.shared.map.lock().unwrap();
                let workers = self.worker.is_some() && m.running > 0;
                let takeable = |m: &MapState, i: usize| m.chapters[i].is_none() && (!workers || matches!(m.claim(i), Claim::Free | Claim::Deferred | Claim::Foreground));
                let pick = if takeable(&m, upto) { Some(upto) } else { (0..upto).find(|&i| takeable(&m, i)) };
                match pick {
                    Some(i) => {
                        m.set_claim(i, Claim::Foreground);
                        Some(i)
                    }
                    None => {
                        if !(0..=upto).any(|i| m.chapters[i].is_none()) {
                            break;
                        }
                        // A background thread lays out the rest: wait for it.
                        let t = Instant::now();
                        let _ = self.shared.changed.wait_timeout(m, Duration::from_millis(50)).unwrap();
                        waited += t.elapsed();
                        None
                    }
                }
            };
            let Some(i) = pick else { continue };
            let pages = if i == upto {
                // Lay the chapter out once and keep the layout: the reader draws it next.
                // A prefix layout never gives the page map its keys.
                let cached = self.laid.borrow().get(&i).cloned().filter(|l| l.complete);
                let laid = match cached {
                    Some(l) => l,
                    None => self.lay_out_now(i)?,
                };
                ChapterPages { keys: laid.pages.iter().map(|p| p.key).collect(), text_bytes: chapter_text_bytes(&self.book.chapter(i).dom) }
            } else {
                // A chapter before the one needed: only its page starts.
                self.with_env(|env| paginate::chapter_pages(&self.book, i, &params, env))
            };
            paginate::resolve_interest(&self.book, i, &pages.keys, &self.interest, &self.shared.map);
            let mut m = self.shared.map.lock().unwrap();
            if m.chapters[i].is_none() {
                m.chapters[i] = Some(pages);
            }
            if m.chapters.iter().all(|c| c.is_some()) {
                m.complete = true;
            }
            drop(m);
            self.shared.changed.notify_all();
        }
        if waited > Duration::from_millis(100) {
            log::info!("ink-html: waited {} ms for background pagination up to spine item {upto}", waited.as_millis());
        }
        if !waited.is_zero() {
            ink_doc::timing::record("wait for paginators", waited);
        }
        self.write_cache_if_complete();
        Ok(())
    }

    /// Writes the cache file once the map is complete, unless the background thread
    /// wrote it or still runs (it writes the file when it finds the map complete).
    fn write_cache_if_complete(&self) {
        if self.cache_done.get() || self.cache_hit {
            return;
        }
        let Some(path) = &self.cache_path else { return };
        let chapters = {
            let m = self.shared.map.lock().unwrap();
            let worker_writes = self.worker.is_some() && (!m.finished || m.cache_written);
            if worker_writes || !m.chapters.iter().all(|c| c.is_some()) {
                return;
            }
            m.chapters.clone()
        };
        self.cache_done.set(true);
        match paginate::write_cache(path, &chapters) {
            Ok(()) => log::info!("ink-html: page map cached in {}", path.display()),
            Err(e) => log::warn!("ink-html: cannot write page map {}: {e}", path.display()),
        }
    }

    /// Keeps a chapter layout (three, the farthest from `c` goes first).
    fn keep_laid(&self, c: usize, l: Arc<LaidOut>) {
        let mut laid = self.laid.borrow_mut();
        if laid.len() >= 3 && !laid.contains_key(&c) {
            let far = *laid.keys().max_by_key(|k| (**k as i64 - c as i64).abs()).unwrap();
            laid.remove(&far);
        }
        laid.insert(c, l);
    }

    /// Fills chapters the background thread left to the foreground (image sizes it could
    /// not read) once it has finished.
    fn finish_deferred(&self) {
        let pending = {
            let m = self.shared.map.lock().unwrap();
            (self.worker.is_none() || m.finished) && m.chapters.iter().any(|c| c.is_none())
        };
        if pending {
            let _ = self.ensure_paginated(self.book.chapters.len().saturating_sub(1));
        }
    }

    /// Paginates until at least `n` pages are known or the book is complete.
    fn ensure_pages(&self, n: usize) -> Result<()> {
        loop {
            let (have, all) = self.shared.map.lock().unwrap().known_pages();
            if have >= n || all {
                return Ok(());
            }
            let next = {
                let m = self.shared.map.lock().unwrap();
                m.chapters.iter().position(|c| c.is_none())
            };
            match next {
                Some(i) => self.ensure_paginated(i)?,
                None => return Ok(()),
            }
        }
    }

    fn locate(&self, page: usize) -> Result<(usize, usize)> {
        self.ensure_pages(page.saturating_add(1))?;
        self.shared.map.lock().unwrap().locate(page).ok_or(DocError::PageOutOfRange(page))
    }

    /// The layout and pages of chapter `c` (foreground cache of three chapters). A fresh
    /// layout that disagrees with the page map corrects the map (see [`Self::correct_map`]).
    fn laid_out(&self, c: usize) -> Result<Arc<LaidOut>> {
        if let Some(l) = self.laid.borrow().get(&c) {
            return Ok(l.clone());
        }
        let l = self.lay_out_now(c)?;
        self.correct_map(c, &l);
        Ok(l)
    }

    /// Lays out chapter `c` in the foreground and keeps the layout.
    fn lay_out_now(&self, c: usize) -> Result<Arc<LaidOut>> {
        let params = self.params()?.clone();
        let lay = self.with_env(|env| layout_chapter(&self.book, c, &params, env));
        let pages = paginate(&lay, params.content_h);
        let l = Arc::new(LaidOut { lay, pages, complete: true });
        self.keep_laid(c, l.clone());
        Ok(l)
    }

    /// Lays out chapter `c` in the foreground only as far as the page holding `key` and
    /// [`PREFIX_PAGES_AFTER`] pages after it, and keeps that layout. When it is a prefix,
    /// a background thread lays the chapter out whole for the pages after it
    /// ([`Self::full_layout`]).
    fn lay_out_prefix(&self, c: usize, key: (u32, u32)) -> Result<Arc<LaidOut>> {
        let params = self.params()?.clone();
        let (lay, pages, complete) = self.with_env(|env| paginate::layout_chapter_until(&self.book, c, &params, env, key, PREFIX_PAGES_AFTER));
        let l = Arc::new(LaidOut { lay, pages, complete });
        self.keep_laid(c, l.clone());
        if !complete {
            if self.book.loader.is_none() {
                // The thread cannot read images through this loader: sizes first.
                self.prefill_chapter_images(c);
            }
            paginate::start_full_layout(self.book.clone(), params, self.font_sources.clone(), self.shared.clone(), c);
        }
        Ok(l)
    }

    /// The whole layout of chapter `c` after a prefix layout: the background thread's
    /// (waiting for it up to [`FULL_LAYOUT_WAIT`]), else a foreground layout. Corrects the
    /// page map when it differs (as [`Self::laid_out`] does for a fresh layout).
    fn full_layout(&self, c: usize) -> Result<Arc<LaidOut>> {
        let t = Instant::now();
        let deadline = t + FULL_LAYOUT_WAIT;
        let got = {
            let mut f = self.shared.full.lock().unwrap();
            loop {
                if f.chapter != Some(c) {
                    break None;
                }
                if let Some(r) = f.result.take() {
                    break Some(r);
                }
                let now = Instant::now();
                if !f.running || now >= deadline {
                    break None;
                }
                f = self.shared.full_done.wait_timeout(f, deadline - now).unwrap().0;
            }
        };
        let from_thread = got.is_some();
        let l = match got {
            Some(l) => {
                self.keep_laid(c, l.clone());
                l
            }
            None => self.lay_out_now(c)?,
        };
        log::info!(
            "ink-html: spine item {c} laid out whole ({}) for a page past its prefix in {} ms",
            if from_thread { "background thread" } else { "foreground" },
            t.elapsed().as_millis()
        );
        self.correct_map(c, &l);
        Ok(l)
    }

    /// Reads the sizes of the images of chapter `c` through the foreground loader.
    fn prefill_chapter_images(&self, c: usize) {
        let ch = &self.book.chapters[c];
        for v in crate::book::scan_image_refs(&ch.html) {
            self.fg_image_size(&ink_doc::resolve_href(&ch.dir, &v));
        }
    }

    /// True when the page map's entry for chapter `c` equals the page starts of `l`
    /// (or the chapter is not in the map yet). For a prefix layout, its pages must be
    /// the first pages of the entry.
    fn map_agrees(&self, c: usize, l: &LaidOut) -> bool {
        let m = self.shared.map.lock().unwrap();
        match m.chapters.get(c) {
            Some(Some(known)) => {
                let len_ok = if l.complete { known.keys.len() == l.pages.len() } else { known.keys.len() >= l.pages.len() };
                len_ok && known.keys.iter().zip(&l.pages).all(|(k, p)| *k == p.key)
            }
            _ => true,
        }
    }

    /// Replaces the page map's entry for chapter `c` with the page starts of a fresh
    /// foreground layout when they differ (a stale cache file, or a background layout
    /// that differed). Later pages move; the current page follows the reading position.
    /// The cache file is deleted, so the next open paginates afresh.
    fn correct_map(&self, c: usize, l: &LaidOut) {
        if !l.complete || self.map_agrees(c, l) {
            return;
        }
        {
            let mut m = self.shared.map.lock().unwrap();
            let Some(Some(known)) = m.chapters.get_mut(c) else { return };
            log::warn!("ink-html: chapter {c}: {} pages now, {} in the page map; page map corrected", l.pages.len(), known.keys.len());
            known.keys = l.pages.iter().map(|p| p.key).collect();
        }
        self.cache_done.set(true);
        self.positions.borrow_mut().clear();
        self.positions_dirty.set(false);
        if let Some(p) = &self.cache_path {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_file(positions_file(p));
        }
        if let Some((rc, key)) = self.reading.get() {
            if let Ok(p) = self.page_of_key(rc, key) {
                self.current.set(p);
            }
        }
    }

    fn key_pos(&self, c: usize, key: (u32, u32)) -> DomPos {
        let ch = self.book.chapter(c);
        let dom = &ch.dom;
        match dom.preorder.get(key.0 as usize) {
            Some(&n) if dom.text(n).is_some() => DomPos::Text { node: n, offset: key.1 },
            Some(&n) => DomPos::Element { node: n, point: Some(0) },
            None => DomPos::Element { node: dom.body, point: Some(dom.node(dom.body).children.len() as u32) },
        }
    }

    fn xpointer(&self, c: usize, pos: DomPos) -> XPointer {
        let ch = self.book.chapter(c);
        to_xpointer(&ch.dom, self.book.chapters[c].fragment.unwrap_or(1), self.book.fragments, pos)
    }

    /// (chapter, key) of a position.
    fn chapter_key(&self, pos: &Position) -> Result<(usize, (u32, u32))> {
        match pos {
            Position::XPointer(x) => {
                let frag = x.fragment().ok_or_else(|| bad(pos))?;
                let c = match self.book.chapter_of_fragment(frag) {
                    Some(c) => c,
                    None if frag > self.book.fragments && self.book.fragments > 0 => {
                        let last = self.book.chapter_of_fragment(self.book.fragments).unwrap_or(0);
                        return Ok((last, (u32::MAX, 0)));
                    }
                    None => return Err(bad(pos)),
                };
                let ch = self.book.chapter(c);
                let p = resolve_lenient(&ch.dom, x).ok_or_else(|| bad(pos))?;
                Ok((c, order_key(&ch.dom, p)))
            }
            Position::Flow(a) => self.migrate_flow(*a).ok_or_else(|| bad(pos)),
            Position::Page(p) => Err(DocError::BadPosition(format!("page {} in a reflowable book", p.page))),
        }
    }

    /// Maps a phase 0 anchor (chapter among chapters with text, block, word) to a key.
    fn migrate_flow(&self, a: FlowAnchor) -> Option<(usize, (u32, u32))> {
        let mut nth = 0u32;
        let mut chapter = None;
        let mut blocks = Vec::new();
        for (i, ch) in self.book.chapters.iter().enumerate() {
            let b = crate::v1::blocks::parse_xhtml(&ch.html);
            if b.is_empty() {
                continue;
            }
            if nth == a.spine {
                chapter = Some(i);
                blocks = b;
                break;
            }
            nth += 1;
        }
        let c = chapter?;
        let words_before: usize = blocks.iter().take(a.block as usize).map(|b| b.text().split_whitespace().count()).sum::<usize>() + a.word as usize;
        let flat = self.flat(c);
        let mut count = 0usize;
        let mut in_word = false;
        for (i, ch) in flat.chars.iter().enumerate() {
            let ws = ch.is_whitespace();
            if !ws && !in_word {
                if count == words_before {
                    let (n, off) = flat.map[i];
                    let dom = &self.book.chapter(c).dom;
                    return Some((c, (dom.node(n).order, off)));
                }
                count += 1;
            }
            in_word = !ws;
        }
        Some((c, (0, 0)))
    }

    fn page_of_key(&self, c: usize, key: (u32, u32)) -> Result<usize> {
        self.ensure_paginated(c)?;
        let m = self.shared.map.lock().unwrap();
        let first = m.first_page(c).ok_or(DocError::PageOutOfRange(0))?;
        let keys = &m.chapters[c].as_ref().unwrap().keys;
        if keys.is_empty() {
            // An empty chapter: the next page (or the last one).
            let (total, _) = m.known_pages();
            drop(m);
            if first >= total {
                self.ensure_pages(first + 1)?;
                let (t2, _) = self.shared.map.lock().unwrap().known_pages();
                return Ok(first.min(t2.saturating_sub(1)));
            }
            return Ok(first);
        }
        let idx = keys.partition_point(|k| *k <= key).saturating_sub(1);
        Ok(first + idx)
    }

    fn flat(&self, c: usize) -> Arc<FlatText> {
        if let Some(f) = self.flats.borrow().get(&c) {
            return f.clone();
        }
        let ch = self.book.chapter(c);
        let dom = &ch.dom;
        let mut chars = Vec::new();
        let mut map = Vec::new();
        let mut last_block = u32::MAX;
        for &n in &dom.preorder {
            if dom.name(n) == "br" {
                chars.push('\n');
                map.push((n, 0));
                continue;
            }
            let Some(t) = dom.text(n) else { continue };
            let p = dom.node(n).parent;
            if dom.element(p).is_some_and(|e| e.in_svg_tree() || matches!(e.name.as_str(), "script" | "style")) {
                continue;
            }
            // A new block starts a new line.
            let b = dom.block_of(p);
            if b != last_block && !chars.is_empty() {
                chars.push('\n');
                map.push((n, 0));
            }
            last_block = b;
            for (i, ch) in t.chars().enumerate() {
                let ch = if ch == '\u{a0}' { ' ' } else { ch };
                if ch.is_whitespace() && chars.last().is_some_and(|l: &char| l.is_whitespace()) {
                    continue;
                }
                if ch == '\u{ad}' {
                    continue;
                }
                chars.push(ch);
                map.push((n, i as u32));
            }
        }
        let f = Arc::new(FlatText { chars, map });
        let mut flats = self.flats.borrow_mut();
        if flats.len() > 8 {
            flats.clear();
        }
        flats.insert(c, f.clone());
        f
    }

    fn flat_pos(&self, c: usize, i: usize) -> Position {
        let f = self.flat(c);
        let (n, off) = f.map.get(i).copied().unwrap_or_else(|| f.map.last().copied().unwrap_or((self.book.chapter(c).dom.body, 0)));
        let dom = &self.book.chapter(c).dom;
        let p = if dom.text(n).is_some() { DomPos::Text { node: n, offset: off } } else { DomPos::Element { node: n, point: Some(0) } };
        Position::XPointer(self.xpointer(c, p))
    }

    fn flat_index(&self, c: usize, key: (u32, u32)) -> usize {
        let f = self.flat(c);
        let dom = &self.book.chapter(c).dom;
        f.map.partition_point(|&(n, off)| (dom.node(n).order, off) < key).min(f.chars.len())
    }

    /// The target of an href (`path#frag`) as a position.
    fn href_position(&self, href: &str) -> Option<(usize, Position)> {
        self.href_target(href).map(|(c, p, _)| (c, p))
    }

    /// The target of an href (`path#frag`): spine item, position and position key.
    fn href_target(&self, href: &str) -> Option<(usize, Position, (u32, u32))> {
        let (path, frag) = match href.split_once('#') {
            Some((p, f)) => (p, Some(f)),
            None => (href, None),
        };
        let c = *self.paths.get(path)?;
        let frag_n = self.book.chapters[c].fragment?;
        let ch = self.book.chapter(c);
        let start = DomPos::Element { node: ch.dom.body, point: Some(0) };
        let chapter_start = (c, Position::XPointer(XPointer::fragment_point(frag_n)), order_key(&ch.dom, start));
        match frag.filter(|f| !f.is_empty()) {
            None => Some(chapter_start),
            Some(f) => match ch.dom.find_id(f) {
                Some(n) => {
                    let p = DomPos::Element { node: n, point: Some(0) };
                    Some((c, Position::XPointer(self.xpointer(c, p)), order_key(&ch.dom, p)))
                }
                None => Some(chapter_start),
            },
        }
    }

    /// The first page of every chapter in the known prefix of the page map (`None` from
    /// the first unknown chapter on). One pass, for queries over many chapters.
    fn first_pages(&self) -> Vec<Option<usize>> {
        let m = self.shared.map.lock().unwrap();
        let mut n = Some(0usize);
        m.chapters
            .iter()
            .map(|c| {
                let first = n;
                n = match (n, c) {
                    (Some(k), Some(p)) => Some(k + p.keys.len()),
                    _ => None,
                };
                first
            })
            .collect()
    }

    /// The page of key `key` in chapter `c` when that chapter's pages are known (`firsts`
    /// from [`Self::first_pages`]).
    fn known_page_key(&self, firsts: &[Option<usize>], c: usize, key: (u32, u32)) -> Option<usize> {
        let first = (*firsts.get(c)?)?;
        let m = self.shared.map.lock().unwrap();
        let keys = &m.chapters.get(c)?.as_ref()?.keys;
        if keys.is_empty() {
            return Some(first);
        }
        Some(first + keys.partition_point(|k| *k <= key).saturating_sub(1))
    }

    /// Note kind of the element `n` and its note container, if it looks like a note.
    fn note_container(&self, c: usize, n: u32) -> Option<(NoteKind, u32)> {
        let ch = self.book.chapter(c);
        let dom = &ch.dom;
        let mut cur = n;
        for _ in 0..4 {
            let e = dom.element(cur)?;
            let ty = type_and_role(e);
            if ty.contains("endnote") || ty.contains("rearnote") {
                return Some((NoteKind::Endnote, cur));
            }
            if ty.contains("footnote") || ty.split_whitespace().any(|t| t == "note") || e.name == "aside" {
                return Some((NoteKind::Footnote, cur));
            }
            let class = e.attr("class").unwrap_or("").to_ascii_lowercase();
            if class.contains("footnote") || class.contains("endnote") {
                return Some((if class.contains("endnote") { NoteKind::Endnote } else { NoteKind::Footnote }, cur));
            }
            cur = dom.node(cur).parent;
            if cur == crate::dom::NONE || cur == dom.body {
                break;
            }
        }
        None
    }

    /// True when link `a` (an `<a>` element of chapter `c`) looks like a note reference.
    fn is_noteref(&self, c: usize, a: u32, target: Option<&(usize, Position)>) -> bool {
        let ch = self.book.chapter(c);
        let dom = &ch.dom;
        let Some(e) = dom.element(a) else { return false };
        if type_and_role(e).contains("noteref") {
            return true;
        }
        if let Some((tc, Position::XPointer(x))) = target {
            let tch = self.book.chapter(*tc);
            if let Some(DomPos::Element { node, .. }) = crate::xpointer::resolve(&tch.dom, x) {
                if node != tch.dom.body && self.note_container(*tc, node).is_some() {
                    return true;
                }
            }
        }
        // Short reference text (digits, symbols, roman numerals), often in <sup>.
        let text = dom.text_content(a);
        let t = text.trim().trim_matches(|c| matches!(c, '[' | ']' | '(' | ')'));
        let in_sup = dom.node(a).parent != crate::dom::NONE && matches!(dom.name(dom.node(a).parent), "sup" | "small")
            || dom.node(a).children.iter().any(|&k| dom.name(k) == "sup");
        !t.is_empty()
            && t.chars().count() <= 4
            && (t.chars().all(|c| c.is_ascii_digit() || "*†‡§¶#".contains(c)) || (in_sup && t.chars().all(|c| c.is_alphanumeric())))
    }

    fn page_items(&self, page: usize) -> Result<(usize, Arc<LaidOut>, PageSpan, f32)> {
        let (c, _) = self.locate(page)?;
        // A fresh layout may correct the page map: locate again.
        let laid = self.laid_out(c)?;
        let (_, lp) = self.locate(page)?;
        if !laid.complete && lp >= laid.pages.len() {
            // Past the prefix the chapter was opened with: the whole layout (it may
            // correct the map too).
            self.full_layout(c)?;
        }
        let (c, lp) = self.locate(page)?;
        let laid = self.laid_out(c)?;
        let span = *laid.pages.get(lp).ok_or(DocError::PageOutOfRange(page))?;
        let dy = self.layout.margin_top - span.y0;
        Ok((c, laid, span, dy))
    }

    fn text_items(laid: &LaidOut, span: &PageSpan) -> Vec<(usize, TextRun)> {
        let slices = &laid.lay.slices[span.first as usize..span.end as usize];
        let (Some(a), Some(b)) = (slices.first(), slices.last()) else { return Vec::new() };
        (a.first_item as usize..b.end_item as usize)
            .filter_map(|i| match &laid.lay.items[i] {
                Draw::Text(t) if t.node != crate::dom::NONE => Some((i, t.clone())),
                _ => None,
            })
            .collect()
    }

    /// x position of char boundary `i` of a run (0: its left edge).
    fn char_x(&self, t: &TextRun, i: usize) -> f32 {
        if i == 0 {
            return t.x;
        }
        let end = t.text.char_indices().nth(i).map_or(t.text.len(), |(b, _)| b);
        t.x + self.fonts.borrow_mut().width(t.face, t.px, &t.text[..end]) + t.letter_spacing * i as f32
    }

    /// The page rectangle of chars `c0..c1` of a run (`dy`: flow to page).
    fn chars_rect(&self, t: &TextRun, c0: usize, c1: usize, dy: f32) -> Rect {
        let (x0, x1) = (self.char_x(t, c0), self.char_x(t, c1));
        Rect::new(x0 as i32, (t.line_top + dy) as i32, (x1 - x0).max(1.0) as u32, (t.line_bottom - t.line_top).max(1.0) as u32)
    }

    fn hit(&self, items: &[(usize, TextRun)], dy: f32, p: Point) -> Option<(usize, usize)> {
        let (px, py) = (p.x as f32, p.y as f32 - dy);
        let mut best: Option<(f32, usize)> = None;
        for (k, (_, t)) in items.iter().enumerate() {
            let dyv = if py < t.line_top { t.line_top - py } else if py > t.line_bottom { py - t.line_bottom } else { 0.0 };
            let dxv = if px < t.x { t.x - px } else if px > t.x + t.width { px - t.x - t.width } else { 0.0 };
            let d = dyv * 4.0 + dxv;
            if best.is_none_or(|b| d < b.0) {
                best = Some((d, k));
            }
        }
        let (_, k) = best?;
        let t = &items[k].1;
        let n = t.text.chars().count();
        let mut left = t.x;
        let ci = (0..n)
            .find(|&i| {
                let right = self.char_x(t, i + 1);
                let before_middle = px < (left + right) / 2.0;
                left = right;
                before_middle
            })
            .unwrap_or(n.saturating_sub(1));
        Some((k, ci))
    }

    /// Hash of the page starts of spine item `c` (`None` while it is not paginated).
    fn chapter_hash(m: &MapState, c: usize) -> Option<u64> {
        Some(paginate::keys_hash(&m.chapters.get(c)?.as_ref()?.keys))
    }

    /// Positions (the book's TOC entries and page labels) whose pages the paginators
    /// work out while they have each chapter's DOM, so numbering the TOC never parses a
    /// chapter on the UI thread. Call before `set_layout`.
    pub fn set_interest_positions(&mut self, positions: &[Position]) {
        let mut per: Vec<Vec<(String, XPointer)>> = vec![Vec::new(); self.book.chapters.len()];
        for p in positions {
            let Position::XPointer(x) = p else { continue };
            let Some(c) = x.fragment().and_then(|f| self.book.chapter_of_fragment(f)) else { continue };
            per[c].push((p.to_string(), x.clone()));
        }
        self.interest = Arc::new(per);
    }

    /// Reads the positions file of the cache file `cache`.
    fn load_positions(&self, cache: &Path) {
        let t = Instant::now();
        let Ok(text) = std::fs::read_to_string(positions_file(cache)) else { return };
        let mut lines = text.lines();
        if lines.next() != Some(POSITIONS_HEADER) {
            return;
        }
        let mut memo = self.positions.borrow_mut();
        for line in lines {
            let mut f = line.splitn(4, '\t');
            let (Some(c), Some(local), Some(hash), Some(pos)) = (f.next(), f.next(), f.next(), f.next()) else { continue };
            if let (Ok(chapter), Ok(local), Ok(hash)) = (c.parse(), local.parse(), u64::from_str_radix(hash, 16)) {
                memo.insert(pos.to_string(), PosMemo { chapter, local, hash });
            }
        }
        log::debug!("ink-html: {} position pages read in {} ms", memo.len(), t.elapsed().as_millis());
    }

    /// Writes the positions file next to the cache file when there is something new.
    fn save_positions(&self) {
        // Positions the paginators worked out go into the file too.
        let resolved: Vec<(String, PosMemo)> = {
            let m = self.shared.map.lock().unwrap_or_else(|e| e.into_inner());
            let memo = self.positions.borrow();
            m.resolved
                .iter()
                .filter(|(k, _)| !memo.contains_key(*k))
                .map(|(k, &(chapter, local, hash))| (k.clone(), PosMemo { chapter, local, hash }))
                .collect()
        };
        if !resolved.is_empty() {
            self.positions.borrow_mut().extend(resolved);
            self.positions_dirty.set(true);
        }
        if !self.positions_dirty.get() {
            return;
        }
        self.positions_dirty.set(false);
        let Some(cache) = self.cache_path.as_ref() else { return };
        use std::fmt::Write as _;
        let memo = self.positions.borrow();
        let mut out = format!("{POSITIONS_HEADER}\n");
        for (pos, p) in memo.iter() {
            if !pos.contains(['\n', '\t']) {
                let _ = writeln!(out, "{}\t{}\t{:016x}\t{pos}", p.chapter, p.local, p.hash);
            }
        }
        let path = positions_file(cache);
        let tmp = path.with_extension("pos.tmp");
        if let Err(e) = std::fs::write(&tmp, out).and_then(|_| std::fs::rename(&tmp, &path)) {
            log::warn!("ink-html: cannot write {}: {e}", path.display());
        }
    }

    /// The page of a remembered position, when its spine item is paginated as it was.
    fn memo_page(&self, key: &str) -> Option<usize> {
        let remembered = self.positions.borrow().get(key).copied();
        let m = self.shared.map.lock().unwrap();
        let p = match remembered {
            Some(p) => p,
            None => {
                // Worked out by a paginator: remember it (and save it with the map).
                let &(chapter, local, hash) = m.resolved.get(key)?;
                let p = PosMemo { chapter, local, hash };
                self.positions.borrow_mut().insert(key.to_string(), p);
                self.positions_dirty.set(true);
                p
            }
        };
        (Self::chapter_hash(&m, p.chapter)? == p.hash).then_some(())?;
        Some(m.first_page(p.chapter)? + p.local)
    }

    /// Remembers that `key` (in spine item `c`) is on global page `page`.
    fn remember(&self, key: String, c: usize, page: usize) {
        let entry = {
            let m = self.shared.map.lock().unwrap();
            let (Some(first), Some(hash)) = (m.first_page(c), Self::chapter_hash(&m, c)) else { return };
            PosMemo { chapter: c, local: page.saturating_sub(first), hash }
        };
        let mut memo = self.positions.borrow_mut();
        if memo.len() >= 20_000 && !memo.contains_key(&key) {
            return;
        }
        if memo.insert(key, entry) != Some(entry) {
            self.positions_dirty.set(true);
        }
    }

    /// Page of `pos` when known without paginating more (see [`Document::page_of_known`]):
    /// remembered, a spine item start, or in a spine item that is parsed already; `None`
    /// rather than parsing a chapter on the UI thread.
    fn known_page_of(&self, pos: &Position) -> Option<usize> {
        let Position::XPointer(x) = pos else {
            return if self.shared.map.lock().unwrap().known_pages().1 { self.page_of(pos).ok() } else { None };
        };
        let frag = x.fragment()?;
        let Some(c) = self.book.chapter_of_fragment(frag) else {
            // Past the last fragment: the end of the book, once it is known.
            return if self.shared.map.lock().unwrap().known_pages().1 { self.page_of(pos).ok() } else { None };
        };
        {
            let m = self.shared.map.lock().unwrap();
            let first = m.first_page(c)?;
            let keys = &m.chapters[c].as_ref()?.keys;
            // The start of a chapter needs no DOM.
            let start = *x == XPointer::fragment_start(frag) || *x == XPointer::fragment_point(frag);
            if start && !keys.is_empty() {
                return Some(first);
            }
        }
        let key = pos.to_string();
        if let Some(p) = self.memo_page(&key) {
            return Some(p);
        }
        if !self.book.has_chapter(c) {
            return None;
        }
        // Chapters 0..=c are paginated and c is parsed: no layout, no parse.
        self.page_of(pos).ok()
    }

    /// Total pages estimated from the source sizes while pagination runs.
    fn estimate_pages(&self) -> Option<usize> {
        let m = self.shared.map.lock().unwrap();
        let (prefix, all) = m.known_pages();
        if all {
            return None;
        }
        let (mut pages, mut bytes, mut unknown) = (0usize, 0usize, 0usize);
        for (c, src) in m.chapters.iter().zip(&self.book.chapters) {
            match c {
                Some(p) => {
                    pages += p.keys.len();
                    bytes += src.html.len();
                }
                None => unknown += src.html.len(),
            }
        }
        // About 2.5 KB of XHTML per page at the default size when nothing is known yet.
        let per_page = if pages > 0 && bytes > 0 { bytes as f64 / pages as f64 } else { 2500.0 };
        let est = pages + (unknown as f64 / per_page.max(1.0)).round() as usize;
        Some(est.max(prefix).max(1))
    }

    /// The flow layout of the chapter holding `page` (for tests and tools).
    pub fn page_layout(&self, page: usize) -> Result<(ChapterLayout, PageSpan)> {
        let (_, laid, span, _) = self.page_items(page)?;
        Ok((laid.lay.clone(), span))
    }

    /// True when the page map of the current layout came from the cache file.
    pub fn cache_hit(&self) -> bool {
        self.cache_hit
    }

    /// The cache used when [`Document::set_page_cache`] was not called: on the Kindle
    /// (`/mnt/us/rinky` exists) `/mnt/us/rinky/cache/pagemaps`, with the MD5 of
    /// the spine contents as the book id.
    fn default_cache(&self) -> Option<(PathBuf, String)> {
        let base = Path::new("/mnt/us/rinky");
        if !base.is_dir() {
            return None;
        }
        use md5::{Digest, Md5};
        let mut h = Md5::new();
        for c in &self.book.chapters {
            h.update(c.path.as_bytes());
            h.update([0]);
            h.update(c.html.as_bytes());
        }
        let id: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        Some((base.join("cache/pagemaps"), format!("content{id}")))
    }
}

impl Document for HtmlDocument {
    fn metadata(&self) -> &Metadata {
        &self.book.meta
    }

    fn kind(&self) -> DocKind {
        DocKind::Reflowable
    }

    fn set_layout(&mut self, settings: &LayoutSettings) -> Result<()> {
        let t = Instant::now();
        let n = self.book.chapters.len();
        let keep = self.anchor.or(self.reading.get()).or_else(|| {
            // Keep the reading position of the previous layout.
            self.params.as_ref()?;
            let m = self.shared.map.lock().unwrap();
            let (c, lp) = m.locate(self.current.get())?;
            Some((c, m.chapters[c].as_ref()?.keys[lp]))
        });
        let keep = keep.filter(|k| k.0 < n);
        // The old worker (and a full-layout thread) stop before their next chapter
        // (dropping it does not wait).
        self.shared.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        self.worker = None;
        self.layout = settings.clone();
        self.params = Some(Params::new(settings));
        self.laid.borrow_mut().clear();
        self.save_positions();
        self.positions.borrow_mut().clear();
        let shared = Arc::new(Shared::default());
        *shared.map.lock().unwrap() = MapState::new(n);
        self.shared = shared;
        // Cache.
        if self.cache.is_none() {
            self.cache = self.default_cache();
        }
        let cache_path = self.cache.as_ref().map(|(dir, id)| paginate::cache_file(dir, id, settings));
        self.cache_path = cache_path.clone();
        self.cache_done.set(false);
        self.cache_hit = false;
        let target = keep.map(|k| k.0).unwrap_or(0);
        let mut hit = false;
        let mut partial = false;
        if let Some(p) = &cache_path {
            let tc = Instant::now();
            let cached = paginate::read_cache(p, n);
            ink_doc::timing::step("page map cache read", tc);
            if let Some(chapters) = cached {
                let complete = chapters.iter().all(|c| c.is_some());
                let target_known = chapters.get(target).is_some_and(|c| c.is_some());
                {
                    let mut m = self.shared.map.lock().unwrap();
                    m.chapters = chapters;
                    m.complete = complete;
                }
                // Check the chapter in view against a fresh layout (needed for the first
                // page anyway): a cache written by an engine or with fonts that lay out
                // differently is dropped instead of hiding text.
                // A partial map: the threads go on with the missing chapters while the
                // foreground lays out (and checks) the chapter in view.
                if !complete && target_known && n > 1 {
                    if self.book.loader.is_none() {
                        self.prefill_image_sizes();
                    }
                    self.worker = Some(Worker::start(self.book.clone(), self.params.clone().unwrap(), self.font_sources.clone(), self.shared.clone(), cache_path.clone(), self.interest.clone()));
                }
                // Only up to a few pages past the position: the cache has the rest of
                // the chapter's pages, a background thread lays it out whole.
                let agrees = if target_known {
                    let key = keep.map(|k| k.1).unwrap_or((0, 0));
                    let laid = self.lay_out_prefix(target, key)?;
                    self.map_agrees(target, &laid)
                } else {
                    true
                };
                if !agrees {
                    log::warn!("ink-html: page map cache {} does not match the layout; paginating again", p.display());
                    let _ = std::fs::remove_file(p);
                    let _ = std::fs::remove_file(positions_file(p));
                    // Threads started on the partial map stop; they keep their own
                    // (old) shared state, so the fresh map is never touched by them.
                    self.shared.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.worker = None;
                    let shared = Arc::new(Shared::default());
                    *shared.map.lock().unwrap() = MapState::new(n);
                    self.shared = shared;
                } else if complete {
                    hit = true;
                    self.load_positions(p);
                } else {
                    // A book closed while it was paginated: go on from there.
                    partial = true;
                    self.load_positions(p);
                    let known = self.shared.map.lock().unwrap().chapters.iter().filter(|c| c.is_some()).count();
                    log::info!("ink-html: partial page map {} ({known} of {n} spine items known)", p.display());
                }
            }
        }
        self.cache_hit = hit;
        if !hit {
            if self.book.loader.is_none() {
                self.prefill_image_sizes();
            }
            // The chapter in view first (claimed before the threads start, so they leave
            // it to the foreground), then the rest in the background.
            self.shared.map.lock().unwrap().set_claim(target, Claim::Foreground);
            if n > 1 && self.worker.is_none() {
                self.worker = Some(Worker::start(self.book.clone(), self.params.clone().unwrap(), self.font_sources.clone(), self.shared.clone(), cache_path.clone(), self.interest.clone()));
            }
            self.ensure_paginated(target)?;
        }
        self.ensure_pages(1)?;
        let (pages, _) = self.shared.map.lock().unwrap().known_pages();
        if pages == 0 {
            return Err(DocError::Format("book produced no pages".into()));
        }
        self.current.set(match keep {
            Some((c, key)) => self.page_of_key(c, key)?,
            None => 0,
        });
        self.reading.set(keep);
        self.anchor = None;
        log::info!(
            "ink-html: '{}' first page ready in {} ms (page map {}, {} spine items)",
            self.book.meta.title,
            t.elapsed().as_millis(),
            if hit {
                "from cache"
            } else if partial {
                "partial cache, paginating"
            } else {
                "paginating"
            },
            n
        );
        Ok(())
    }

    fn layout(&self) -> &LayoutSettings {
        &self.layout
    }

    fn page_count(&self) -> PageCount {
        // Keep a few pages after the current one known, so a reader that clamps page
        // turns to the count never waits for the background thread.
        if self.params.is_some() {
            self.finish_deferred();
            let _ = self.ensure_pages(self.current.get().saturating_add(3));
        }
        let (pages, complete) = self.shared.map.lock().unwrap().known_pages();
        PageCount { pages, complete }
    }

    fn render_page(&mut self, page: usize, canvas: &mut Canvas) -> Result<()> {
        let t = Instant::now();
        let (c, laid, span, _) = self.page_items(page)?;
        let book = self.book.clone();
        let resources = &self.resources;
        let images = &self.images;
        let mut fonts = self.fonts.borrow_mut();
        fonts.set_embolden(self.layout.embolden);
        let mut img = |src: &str, w: u32, h: u32| -> Option<Arc<Canvas>> {
            // An inline SVG is named by its node, which is only unique in its chapter.
            let name = if src.starts_with("#svg:") { std::borrow::Cow::Owned(format!("{src}@{c}")) } else { std::borrow::Cow::Borrowed(src) };
            images.borrow_mut().get(&name, w, h, || {
                if let Some(node) = src.strip_prefix("#svg:") {
                    let node: u32 = node.parse().ok()?;
                    let ch = book.chapter(c);
                    if let Some((href, par)) = crate::images::svg_single_image(&ch.dom, node) {
                        // An SVG wrapper around one raster image: decode the image directly.
                        let bytes = resources.read(&ink_doc::resolve_href(&book.chapters[c].dir, &href)).ok()?;
                        return crate::images::decode_into_box(&bytes, w, h, &par);
                    }
                    let markup = crate::images::svg_markup(&ch.dom, node);
                    let res = svg_resolver(resources.as_ref(), &book.chapters[c].dir, &markup);
                    crate::images::decode_scaled(markup.as_bytes(), w, h, Some(res))
                } else {
                    let bytes = resources.read(src).ok()?;
                    let dir = src.rfind('/').map(|i| src[..=i].to_string()).unwrap_or_default();
                    let res = if crate::images::is_svg(&bytes) { Some(svg_resolver(resources.as_ref(), &dir, &String::from_utf8_lossy(&bytes))) } else { None };
                    crate::images::decode_scaled(&bytes, w, h, res)
                }
            })
        };
        crate::render::draw_page(canvas, &mut fonts, &laid.lay, &span, self.layout.margin_top, &mut img);
        let ms = t.elapsed().as_millis();
        if ms > 300 {
            log::info!("ink-html: page {} drawn in {ms} ms", page + 1);
        } else {
            log::debug!("ink-html: page {} drawn in {ms} ms", page + 1);
        }
        Ok(())
    }

    fn position_of_page(&self, page: usize) -> Result<Position> {
        let (c, lp) = self.locate(page)?;
        let key = self.shared.map.lock().unwrap().chapters[c].as_ref().unwrap().keys[lp];
        Ok(Position::XPointer(self.xpointer(c, self.key_pos(c, key))))
    }

    fn page_of(&self, pos: &Position) -> Result<usize> {
        let text = matches!(pos, Position::XPointer(_)).then(|| pos.to_string());
        if let Some(p) = text.as_deref().and_then(|t| self.memo_page(t)) {
            return Ok(p);
        }
        let (c, key) = self.chapter_key(pos)?;
        let page = self.page_of_key(c, key)?;
        if let Some(t) = text {
            self.remember(t, c, page);
        }
        Ok(page)
    }

    fn current_page(&self) -> usize {
        self.current.get()
    }

    fn set_current_page(&mut self, page: usize) {
        let _ = self.ensure_pages(page.saturating_add(1));
        let (n, _) = self.shared.map.lock().unwrap().known_pages();
        let page = page.min(n.saturating_sub(1));
        if page != self.current.get() || self.reading.get().is_none() {
            // A page turn: the reading position becomes that page's start.
            let m = self.shared.map.lock().unwrap();
            let start = m.locate(page).and_then(|(c, lp)| Some((c, *m.chapters[c].as_ref()?.keys.get(lp)?)));
            drop(m);
            self.reading.set(start);
        }
        self.current.set(page);
    }

    fn toc(&self) -> Vec<TocEntry> {
        if self.toc_cache.borrow().is_none() {
            let mut entries = Vec::new();
            if !self.nav.toc.is_empty() {
                for NavPoint { title, href, level } in &self.nav.toc {
                    if let Some((c, pos, key)) = self.href_target(href) {
                        entries.push((title.clone(), *level, pos, Some((c, key))));
                    }
                }
            } else {
                // No navigation: the first heading of each chapter.
                for (c, ch) in self.book.chapters.iter().enumerate() {
                    if ch.fragment.is_none() {
                        continue;
                    }
                    let d = self.book.chapter(c);
                    let dom = &d.dom;
                    if let Some(h) = dom.preorder.iter().copied().find(|&n| matches!(dom.name(n), "h1" | "h2" | "h3")) {
                        let title = dom.text_content(h).split_whitespace().collect::<Vec<_>>().join(" ");
                        if !title.is_empty() {
                            let p = DomPos::Element { node: h, point: Some(0) };
                            let pos = Position::XPointer(self.xpointer(c, p));
                            entries.push((title, 0, pos, Some((c, order_key(dom, p)))));
                        }
                    }
                }
            }
            *self.toc_cache.borrow_mut() = Some(entries);
        }
        let cache = self.toc_cache.borrow();
        let firsts = self.first_pages();
        cache
            .as_ref()
            .unwrap()
            .iter()
            .map(|(title, level, pos, ck)| TocEntry { title: title.clone(), level: *level, position: pos.clone(), page: ck.and_then(|(c, key)| self.known_page_key(&firsts, c, key)) })
            .collect()
    }

    fn search(&mut self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let q: Vec<char> = query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().chars().collect();
        if q.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut hits = Vec::new();
        let firsts = self.first_pages();
        for c in 0..self.book.chapters.len() {
            if self.book.chapters[c].fragment.is_none() {
                continue;
            }
            let flat = self.flat(c);
            let lower: Vec<char> = flat.chars.iter().map(|ch| ch.to_lowercase().next().unwrap_or(*ch)).map(|ch| if ch == '\n' { ' ' } else { ch }).collect();
            let mut i = 0;
            while i + q.len() <= lower.len() {
                if lower[i..i + q.len()] == q[..] {
                    let pos = self.flat_pos(c, i);
                    let lo = i.saturating_sub(40);
                    let hi = (i + q.len() + 40).min(flat.chars.len());
                    let before: String = flat.chars[lo..i].iter().map(|c| if *c == '\n' { ' ' } else { *c }).collect();
                    let matched: String = flat.chars[i..i + q.len()].iter().collect();
                    let after: String = flat.chars[i + q.len()..hi].iter().map(|c| if *c == '\n' { ' ' } else { *c }).collect();
                    let excerpt = format!("{before}{matched}{after}");
                    let page = {
                        let (node, off) = flat.map.get(i).copied().unwrap_or((self.book.chapter(c).dom.body, 0));
                        let key = (self.book.chapter(c).dom.node(node).order, off);
                        self.known_page_key(&firsts, c, key)
                    };
                    hits.push(SearchHit { position: pos, page, excerpt, matched: before.len()..before.len() + matched.len() });
                    if hits.len() >= limit {
                        return Ok(hits);
                    }
                    i += q.len();
                } else {
                    i += 1;
                }
            }
        }
        Ok(hits)
    }

    fn read_around(&mut self, pos: &Position, chars: usize) -> Result<String> {
        let (c, key) = self.chapter_key(pos)?;
        let flat = self.flat(c);
        let at = self.flat_index(c, key);
        let half = chars / 2;
        let mut lo = at.saturating_sub(half);
        let mut hi = at.saturating_add(half).min(flat.chars.len());
        // Use more text on the other side near the chapter edges.
        if hi - lo < chars {
            if lo == 0 {
                hi = chars.min(flat.chars.len());
            } else {
                lo = flat.chars.len().saturating_sub(chars);
            }
        }
        let mut text: String = flat.chars[lo..hi].iter().collect();
        // Short chapters: add the neighbours.
        if text.chars().count() < chars {
            if let Some(next) = (c + 1..self.book.chapters.len()).find(|&i| self.book.chapters[i].fragment.is_some()) {
                let f = self.flat(next);
                let need = chars - text.chars().count();
                text.push('\n');
                text.extend(f.chars.iter().take(need));
            }
        }
        Ok(text)
    }

    fn links(&mut self, pos: &Position) -> Result<Vec<Link>> {
        let page = self.page_of(pos)?;
        let (c, laid, span, dy) = self.page_items(page)?;
        let slices = &laid.lay.slices[span.first as usize..span.end as usize];
        let (Some(a), Some(b)) = (slices.first(), slices.last()) else { return Ok(Vec::new()) };
        let mut order: Vec<u32> = Vec::new();
        let mut text: HashMap<u32, String> = HashMap::new();
        let mut rect: HashMap<u32, Rect> = HashMap::new();
        for it in &laid.lay.items[a.first_item as usize..b.end_item as usize] {
            let (link, r, t) = match it {
                Draw::Text(t) if t.link != NO_LINK => (t.link, Rect::new(t.x as i32, (t.line_top + dy) as i32, t.width.max(1.0) as u32, (t.line_bottom - t.line_top).max(1.0) as u32), t.text.as_str()),
                Draw::Image(i) if i.link != NO_LINK => (i.link, Rect::new(i.x as i32, (i.y + dy) as i32, i.w as u32, i.h as u32), ""),
                _ => continue,
            };
            if !order.contains(&link) {
                order.push(link);
            }
            let s = text.entry(link).or_default();
            if !s.is_empty() && !t.is_empty() {
                s.push(' ');
            }
            s.push_str(t);
            rect.entry(link).and_modify(|x| *x = x.union(&r)).or_insert(r);
        }
        let mut out = Vec::new();
        for l in order {
            let lr = &laid.lay.links[l as usize];
            let (target, footnote) = if lr.external {
                (LinkTarget::External(lr.target.clone()), false)
            } else {
                let href = if lr.target.starts_with('#') { format!("{}{}", self.book.chapters[c].path, lr.target) } else { lr.target.clone() };
                match self.href_position(&href) {
                    Some(t) => {
                        let fnote = self.is_noteref(c, lr.node, Some(&t));
                        (LinkTarget::Internal(t.1), fnote)
                    }
                    None => (LinkTarget::External(lr.target.clone()), false),
                }
            };
            out.push(Link { text: text.remove(&l).unwrap_or_default(), target, rect: rect.remove(&l), footnote });
        }
        Ok(out)
    }

    fn select(&mut self, page: usize, from: Point, to: Point) -> Result<Option<Selection>> {
        let (c, laid, span, dy) = self.page_items(page)?;
        let items = Self::text_items(&laid, &span);
        let (Some(a), Some(b)) = (self.hit(&items, dy, from), self.hit(&items, dy, to)) else { return Ok(None) };
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        let mut text = String::new();
        let mut rects: Vec<Rect> = Vec::new();
        let mut last_line: Option<f32> = None;
        // The previous line ended in a hyphen ("in-" | "finities", "well-" | "known"):
        // the next piece continues that word with no space.
        let mut joins_next = false;
        // Right edge of the previous run: runs that touch on a line are one word.
        let mut prev_right = f32::MIN;
        for (k, (_, t)) in items.iter().enumerate().take(e.0 + 1).skip(s.0) {
            let n = t.text.chars().count();
            let c0 = if k == s.0 { s.1 } else { 0 };
            let c1 = if k == e.0 { (e.1 + 1).min(n) } else { n };
            if c0 >= c1 {
                continue;
            }
            match last_line {
                Some(l) if joins_next && l != t.line_top => {}
                Some(l) if l == t.line_top && t.x > prev_right + 0.5 => text.push(' '),
                Some(l) if l == t.line_top => {}
                Some(_) => text.push('\n'),
                None => {}
            }
            last_line = Some(t.line_top);
            prev_right = t.x + t.width;
            let mut piece: String = t.text.chars().skip(c0).take(c1 - c0).collect();
            // Only the hyphen the layout added goes; a hyphen of the book stays.
            if t.hyphenated && c1 == n {
                piece.pop();
                joins_next = true;
            } else {
                joins_next = c1 == n && piece.ends_with('-');
            }
            text.push_str(&piece);
            push_line_rect(&mut rects, self.chars_rect(t, c0, c1, dy));
        }
        let ts = &items[s.0].1;
        let te = &items[e.0].1;
        let start = DomPos::Text { node: ts.node, offset: ts.start + (s.1 as u32).min(ts.len) };
        let end = DomPos::Text { node: te.node, offset: te.start + ((e.1 + 1) as u32).min(te.len) };
        let sel = Selection {
            start: Position::XPointer(self.xpointer(c, start)),
            end: Position::XPointer(self.xpointer(c, end)),
            text: text.trim().to_string(),
            page,
            rects,
        };
        self.selection = Some(sel.clone());
        Ok(Some(sel))
    }

    fn range_rects(&mut self, page: usize, start: &Position, end: &Position) -> Vec<Rect> {
        let Ok((c, laid, span, dy)) = self.page_items(page) else { return Vec::new() };
        // A range in other chapters needs no DOM: the reader asks for every saved
        // highlight on every page, and resolving one parsed (and, in big books, evicted
        // again) the highlight's chapter on each page turn.
        let quick = |p: &Position| match p {
            Position::XPointer(x) => x.fragment().and_then(|f| self.book.chapter_of_fragment(f)),
            _ => None,
        };
        if let (Some(qa), Some(qb)) = (quick(start), quick(end)) {
            if qa.max(qb) < c || qa.min(qb) > c {
                return Vec::new();
            }
        }
        let (Ok(a), Ok(b)) = (self.chapter_key(start), self.chapter_key(end)) else { return Vec::new() };
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if b.0 < c || a.0 > c {
            return Vec::new();
        }
        // The range clamped to the page's chapter, as (node order, char offset) keys.
        let lo = if a.0 < c { (0, 0) } else { a.1 };
        let hi = if b.0 > c { (u32::MAX, u32::MAX) } else { b.1 };
        if lo >= hi {
            return Vec::new();
        }
        let items = Self::text_items(&laid, &span);
        let mut rects: Vec<Rect> = Vec::new();
        for (_, t) in &items {
            let order = self.book.chapter(c).dom.node(t.node).order;
            let (run_lo, run_hi) = ((order, t.start), (order, t.start + t.len));
            if run_hi <= lo || run_lo >= hi {
                continue;
            }
            // Visible chars map to DOM chars from the run's start (as in `select`).
            let n = t.text.chars().count();
            let c0 = if lo > run_lo { (lo.1 - t.start) as usize } else { 0 }.min(n);
            let c1 = if hi < run_hi { (hi.1 - t.start) as usize } else { n }.min(n);
            if c0 >= c1 {
                continue;
            }
            push_line_rect(&mut rects, self.chars_rect(t, c0, c1, dy));
        }
        rects
    }

    fn hit_rects(&mut self, page: usize, hit: &SearchHit) -> Vec<Rect> {
        let Ok((c, key)) = self.chapter_key(&hit.position) else { return Vec::new() };
        let len = hit.excerpt.get(hit.matched.clone()).map_or(0, |m| m.chars().count());
        if len == 0 {
            return Vec::new();
        }
        // End: one past the last matched character, in that character's text node (the
        // flat text's next entry can be a block break mapped elsewhere).
        let mut i = self.flat_index(c, key);
        let f = self.flat(c);
        // A block break before the match maps to the same key as its first character.
        let starts_blank = hit.excerpt.get(hit.matched.clone()).and_then(|m| m.chars().next()).is_some_and(char::is_whitespace);
        while !starts_blank && f.chars.get(i) == Some(&'\n') {
            i += 1;
        }
        let Some(&(n, off)) = f.map.get((i + len - 1).min(f.map.len().saturating_sub(1))) else { return Vec::new() };
        if self.book.chapter(c).dom.text(n).is_none() {
            return Vec::new();
        }
        let end = Position::XPointer(self.xpointer(c, DomPos::Text { node: n, offset: off + 1 }));
        self.range_rects(page, &hit.position, &end)
    }

    fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    fn clear_selection(&mut self) {
        self.selection = None;
    }

    fn set_selection(&mut self, sel: Selection) {
        self.selection = Some(sel);
    }

    fn location(&self, pos: &Position) -> Option<Location> {
        // A position in a chapter nobody has parsed yet (chapter ticks while pages are
        // counted): the chapter's start, without parsing it on the UI thread.
        if let Position::XPointer(x) = pos {
            if let Some(c) = x.fragment().and_then(|f| self.book.chapter_of_fragment(f)) {
                if !self.book.has_chapter(c) {
                    let bytes: u64 = (0..c).map(|i| self.chapter_bytes(i)).sum();
                    return Some(Location::from_offset(bytes));
                }
            }
        }
        let (c, key) = self.chapter_key(pos).ok()?;
        let mut bytes: u64 = (0..c).map(|i| self.chapter_bytes(i)).sum();
        let ch = self.book.chapter(c);
        let dom = &ch.dom;
        for &n in &dom.preorder {
            let order = dom.node(n).order;
            if order > key.0 {
                break;
            }
            if let Some(t) = dom.text(n) {
                if dom.element(dom.node(n).parent).is_some_and(crate::dom::Element::in_svg_tree) {
                    continue;
                }
                if order == key.0 {
                    bytes += crate::xpointer::char_to_byte(t, key.1) as u64;
                } else {
                    bytes += t.len() as u64;
                }
            }
        }
        Some(Location::from_offset(bytes))
    }

    fn location_count(&self) -> Option<Location> {
        let total: u64 = (0..self.book.chapters.len()).map(|i| self.chapter_bytes(i)).sum();
        Some(Location::from_offset(total.saturating_sub(1)))
    }

    fn page_labels(&self) -> Vec<(String, Position)> {
        self.nav.page_list.iter().filter_map(|p| self.href_position(&p.href).map(|(_, pos)| (p.title.clone(), pos))).collect()
    }

    fn note(&mut self, target: &Position) -> Result<Option<Note>> {
        let Position::XPointer(x) = target else { return Ok(None) };
        let frag = x.fragment().ok_or_else(|| bad(target))?;
        let Some(c) = self.book.chapter_of_fragment(frag) else { return Ok(None) };
        let ch = self.book.chapter(c);
        let Some(p) = crate::xpointer::resolve(&ch.dom, x) else { return Ok(None) };
        let node = match p {
            DomPos::Element { node, .. } => node,
            DomPos::Text { node, .. } => ch.dom.node(node).parent,
        };
        if node == ch.dom.body {
            return Ok(None);
        }
        let (kind, container) = match self.note_container(c, node) {
            Some(k) => k,
            None => {
                // An anchor inside a paragraph of a notes list: the paragraph is the note.
                let b = ch.dom.block_of(node);
                if b == ch.dom.body {
                    return Ok(None);
                }
                (NoteKind::Endnote, b)
            }
        };
        let (label, spans) = note_body(&ch.dom, container);
        let text = spans.iter().map(|s| s.text.as_str()).collect();
        let position = Position::XPointer(self.xpointer(c, DomPos::Element { node: container, point: Some(0) }));
        Ok(Some(Note { kind, position, text, label, spans }))
    }

    fn set_page_cache(&mut self, dir: &Path, book_id: &str) {
        self.cache = Some((dir.to_path_buf(), book_id.to_string()));
    }

    fn wait_paginated(&mut self, timeout: Duration) -> bool {
        if let Some(w) = &self.worker {
            if !w.wait(timeout) {
                return false;
            }
        }
        // Chapters the background thread left to the foreground, or no background thread.
        let n = self.book.chapters.len();
        self.ensure_paginated(n.saturating_sub(1)).is_ok()
    }

    fn page_estimate(&self) -> Option<usize> {
        self.estimate_pages()
    }

    fn page_of_known(&self, pos: &Position) -> Option<usize> {
        self.known_page_of(pos)
    }

    fn spine_starts(&self) -> Vec<usize> {
        if !self.page_count().complete {
            return Vec::new();
        }
        let mut v: Vec<usize> = self.first_pages().into_iter().flatten().collect();
        v.dedup();
        v
    }

    fn open_at(&mut self, pos: &Position) {
        if let Err(e) = self.set_anchor(pos) {
            log::warn!("ink-html: cannot open at {pos}: {e}");
        }
    }
}

impl Drop for HtmlDocument {
    fn drop(&mut self) {
        // Stop the background threads (not waiting), then keep what they found: the next
        // open goes on from a partial map instead of paginating from the start.
        self.shared.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        self.save_positions();
        if self.cache_done.get() || self.cache_hit {
            return;
        }
        let Some(path) = &self.cache_path else { return };
        let snapshot = {
            let m = self.shared.map.lock().unwrap_or_else(|e| e.into_inner());
            let known = m.chapters.iter().filter(|c| c.is_some()).count();
            (known > 0 && known < m.chapters.len()).then(|| (m.chapters.clone(), known))
        };
        if let Some((chapters, known)) = snapshot {
            match paginate::write_cache(path, &chapters) {
                Ok(()) => log::info!("ink-html: partial page map ({known} of {} spine items) saved in {}", chapters.len(), path.display()),
                Err(e) => log::warn!("ink-html: cannot write page map {}: {e}", path.display()),
            }
        }
    }
}

/// Adds `r` to `rects`, merged with the last one when it is on the same line.
fn push_line_rect(rects: &mut Vec<Rect>, r: Rect) {
    match rects.last_mut() {
        Some(last) if last.y == r.y => *last = last.union(&r),
        _ => rects.push(r),
    }
}

/// `epub:type` and `role` of an element, lowercase, with a space between.
fn type_and_role(e: &crate::dom::Element) -> String {
    format!("{} {}", e.attr_local("type").unwrap_or(""), e.attr("role").unwrap_or("")).to_ascii_lowercase()
}

/// A remembered position: spine item, page within it, hash of the item's page starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PosMemo {
    chapter: usize,
    local: usize,
    hash: u64,
}

/// First line of a positions file.
const POSITIONS_HEADER: &str = "rinky-positions 1";

/// The positions file next to the page-map cache file `cache`.
fn positions_file(cache: &Path) -> PathBuf {
    cache.with_extension("pos")
}

impl HtmlDocument {
    /// Text bytes of chapter `i` (Kindle locations): exact from the page map or a parsed
    /// DOM; while the chapter is neither paginated nor parsed, estimated from its source
    /// size (parsing every chapter on the UI thread for a location took seconds on big
    /// books). Locations can move a little when pagination finishes.
    fn chapter_bytes(&self, i: usize) -> u64 {
        let ratio = {
            let m = self.shared.map.lock().unwrap();
            if let Some(Some(c)) = m.chapters.get(i) {
                return c.text_bytes;
            }
            if self.book.chapters[i].fragment.is_none() {
                return 0;
            }
            if self.book.has_chapter(i) {
                None
            } else {
                // Text bytes per source byte of the chapters known so far.
                let (mut text, mut src) = (0u64, 0u64);
                for (c, ch) in m.chapters.iter().zip(&self.book.chapters) {
                    if let Some(c) = c {
                        text += c.text_bytes;
                        src += ch.html.len() as u64;
                    }
                }
                Some(if src > 0 { text as f64 / src as f64 } else { 0.6 })
            }
        };
        match ratio {
            Some(r) => (self.book.chapters[i].html.len() as f64 * r) as u64,
            None => paginate::chapter_text_bytes(&self.book.chapter(i).dom),
        }
    }

    /// Sets the position the next `set_layout` opens at.
    pub fn set_anchor(&mut self, pos: &Position) -> Result<()> {
        self.anchor = Some(self.chapter_key(pos)?);
        Ok(())
    }
}

/// Reads every `href` an SVG refers to (through the main-thread loader) for resvg.
fn svg_resolver(loader: &dyn ResourceLoader, dir: &str, svg: &str) -> crate::images::HrefResolver<'static> {
    let mut found: HashMap<String, Vec<u8>> = HashMap::new();
    let mut from = 0;
    while let Some(p) = svg[from..].find("href=\"") {
        let s = from + p + 6;
        let Some(e) = svg[s..].find('"').map(|e| s + e) else { break };
        let href = crate::parse::decode_attr(&svg[s..e]);
        if !href.starts_with("data:") && !href.starts_with('#') {
            if let Ok(b) = loader.read(&ink_doc::resolve_href(dir, &href)) {
                found.insert(href, b);
            }
        }
        from = e;
    }
    Arc::new(move |h: &str| found.get(h).cloned())
}

/// A backlink inside a note (back to the reference): `role="doc-backlink"`,
/// `epub:type="backlink"`, or a link whose text is only an arrow (`↩`, `↑`, `^`).
fn is_backlink(dom: &crate::dom::Dom, n: crate::dom::NodeId, e: &crate::dom::Element) -> bool {
    if e.name != "a" {
        return false;
    }
    if type_and_role(e).contains("backlink") {
        return true;
    }
    let t = dom.text_content(n);
    let t = t.trim();
    !t.is_empty() && t.chars().all(|c| "↩↪↑⤴⇧^\u{fe0e}\u{fe0f}".contains(c) || c.is_whitespace())
}

/// The note's mark when the whole text of an element is one: up to three digits, a few
/// of `*†‡§¶#` or, in a link or `<sup>`, up to four letters (`iv`, `a`); with optional
/// brackets or a trailing dot (`[12]`, `12.`).
fn element_mark(name: &str, text: &str) -> Option<String> {
    let m = ink_doc::note_mark(text)?;
    let digits = m.chars().all(|c| c.is_ascii_digit()) && m.len() <= 3;
    let symbols = m.chars().all(|c| "*†‡§¶#".contains(c));
    let letters = matches!(name, "a" | "sup") && m.chars().all(char::is_alphanumeric);
    (digits || symbols || letters).then_some(m)
}

/// The mark at the start of plain text when it is set off as one: `[12]`, `(12)`,
/// `12.`, `12)`, or `*`/`†` marks, followed by white space. A bare number (`12 The
/// text`) stays: it could be the text itself (`1984 was ...`); the reader strips it
/// when it equals the reference ([`Note::strip_label`]).
fn text_mark(text: &str) -> Option<(String, usize)> {
    let t = text.trim_start();
    let lead = text.len() - t.len();
    let (token, _) = t.split_once(char::is_whitespace)?;
    let set_off = (token.starts_with('[') && token.ends_with(']'))
        || (token.starts_with('(') && token.ends_with(')'))
        || (token.ends_with(['.', ')', ':']) && token.trim_end_matches(['.', ')', ':']).chars().all(|c| c.is_ascii_digit()))
        || token.chars().all(|c| "*†‡§¶".contains(c));
    let m = ink_doc::note_mark(token).filter(|_| set_off)?;
    if m.chars().all(|c| c.is_ascii_digit()) && m.len() > 3 {
        return None;
    }
    Some((m, lead + token.len()))
}

/// The note text of `id` as styled spans, without its leading number (an element that
/// holds only the number, such as `<a href="#ref1">1</a>`, `<sup>1</sup>` or an FB2
/// `<title>`, or a set-off `[1]` / `1.` in the text) and without backlinks anywhere.
/// Returns the number found (the note's label) and the spans, white space collapsed.
/// Italic and bold come from the elements (`i`, `em`, `b`, `strong`, ...) inside the
/// note, so a note set in italic as a whole stays plain.
fn note_body(dom: &crate::dom::Dom, id: crate::dom::NodeId) -> (Option<String>, Vec<ink_doc::NoteSpan>) {
    use crate::dom::NodeKind;
    use ink_doc::NoteSpan;
    // (node, bold, italic, end of a block)
    let mut stack = vec![(id, false, false, false)];
    let mut label: Option<String> = None;
    let mut raw: Vec<NoteSpan> = Vec::new();
    // Any text yet (a mark counts only before the text).
    let mut seen = false;
    while let Some((n, bold, italic, end)) = stack.pop() {
        if end {
            raw.push(NoteSpan { text: " ".into(), bold, italic });
            continue;
        }
        match &dom.node(n).kind {
            NodeKind::Text(t) => {
                let mut t = t.as_str();
                if !seen && !t.trim().is_empty() {
                    seen = true;
                    if label.is_none() {
                        if let Some((m, len)) = text_mark(t) {
                            label = Some(m);
                            t = &t[len..];
                        }
                    }
                }
                raw.push(NoteSpan { text: t.to_string(), bold, italic });
            }
            NodeKind::Element(e) => {
                if e.in_svg_tree() || is_backlink(dom, n, e) {
                    continue;
                }
                if n != id && !seen {
                    if let Some(m) = element_mark(&e.name, &dom.text_content(n)) {
                        label.get_or_insert(m);
                        continue;
                    }
                }
                let bold = bold || matches!(e.name.as_str(), "b" | "strong");
                let italic = italic || matches!(e.name.as_str(), "i" | "em" | "cite" | "var" | "dfn");
                if n != id && e.st.display != crate::style::Display::Inline {
                    raw.push(NoteSpan { text: " ".into(), bold, italic });
                    stack.push((n, bold, italic, true));
                }
                stack.extend(dom.node(n).children.iter().rev().map(|&c| (c, bold, italic, false)));
            }
        }
    }
    // Collapse white space across spans, merge equal styles, trim both ends.
    let mut spans: Vec<NoteSpan> = Vec::new();
    let mut space = true;
    for s in raw {
        let mut out = String::new();
        for ch in s.text.chars() {
            if ch.is_whitespace() {
                if !space {
                    out.push(' ');
                    space = true;
                }
            } else {
                out.push(ch);
                space = false;
            }
        }
        if out.is_empty() {
            continue;
        }
        match spans.last_mut() {
            // A space alone joins the previous span whatever its style.
            Some(l) if (l.bold, l.italic) == (s.bold, s.italic) || out == " " => l.text.push_str(&out),
            _ => spans.push(NoteSpan { text: out, bold: s.bold, italic: s.italic }),
        }
    }
    if let Some(l) = spans.last_mut() {
        l.text.truncate(l.text.trim_end().len());
    }
    spans.retain(|s| !s.text.is_empty());
    match label {
        // Nothing but the number: show the number rather than nothing.
        Some(l) if spans.is_empty() => (None, vec![NoteSpan { text: l, ..Default::default() }]),
        l => (l, spans),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pages a prefix layout must reach after the page in view (the reader keeps three
    /// pages after the current one known).
    const AFTER: usize = 4;

    /// For every spine item and every 5th page of its full layout: the prefix layout
    /// up to that page gives exactly the full layout's first pages, at least `AFTER`
    /// pages past it, and draws them pixel for pixel the same.
    fn check_prefix_layouts(path: &Path) -> usize {
        // ink-formats links its own copy of this crate: rebuild the document here.
        let src = ink_formats::load(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let chapters = src.chapters.into_iter().map(|c| HtmlChapter { path: c.path, html: c.html }).collect();
        let mut doc = HtmlDocument::new(src.info.metadata, chapters, src.resources).unwrap();
        let settings = LayoutSettings::for_screen(1272, 1696);
        doc.set_layout(&settings).unwrap();
        doc.worker = None;
        let params = doc.params.clone().unwrap();
        let book = doc.book.clone();
        let mut checked = 0;
        let mut a = Canvas::new(settings.width as usize, settings.height as usize);
        let mut b = Canvas::new(settings.width as usize, settings.height as usize);
        for c in 0..book.chapters.len() {
            let full = doc.with_env(|env| layout_chapter(&book, c, &params, env));
            let full_pages = paginate(&full, params.content_h);
            for t in (0..full_pages.len()).step_by(5) {
                let key = full_pages[t].key;
                let (lay, pages, complete) = doc.with_env(|env| paginate::layout_chapter_until(&book, c, &params, env, key, AFTER));
                let what = format!("{} spine item {c} page {t} of {}", path.display(), full_pages.len());
                if complete {
                    assert_eq!(pages, full_pages, "{what}: complete prefix");
                    continue;
                }
                assert!(pages.len() > t + AFTER, "{what}: only {} pages", pages.len());
                assert_eq!(pages[..], full_pages[..pages.len()], "{what}");
                let mut none = |_: &str, _: u32, _: u32| None;
                for (p, span) in pages.iter().enumerate().skip(t.saturating_sub(1)) {
                    let mut fonts = doc.fonts.borrow_mut();
                    crate::render::draw_page(&mut a, &mut fonts, &lay, span, settings.margin_top, &mut none);
                    crate::render::draw_page(&mut b, &mut fonts, &full, &full_pages[p], settings.margin_top, &mut none);
                    assert!(a.pixels == b.pixels, "{what}: page {p} drawn differently");
                }
                checked += 1;
            }
        }
        checked
    }

    /// A two-chapter book whose first chapter is long (about 24 pages).
    fn long_book() -> HtmlDocument {
        let mut body = String::new();
        for i in 0..600 {
            if i % 97 == 0 {
                body.push_str(&format!("<h2 id='h{i}'>Part {i}</h2>"));
            }
            body.push_str(&format!("<p>Paragraph {i}: the quick brown fox jumps over the lazy dog, again and again, so that this line wraps.</p>"));
        }
        let html = |b: &str| format!("<html xmlns='http://www.w3.org/1999/xhtml'><head><title>t</title></head><body>{b}</body></html>");
        let chapters = vec![
            HtmlChapter { path: "a.xhtml".into(), html: html(&body) },
            HtmlChapter { path: "b.xhtml".into(), html: html("<p>The end.</p>") },
        ];
        HtmlDocument::new(Metadata::default(), chapters, Box::new(ink_doc::MemoryLoader::default())).unwrap()
    }

    /// Two chapters whose inline `<svg>` wrappers have the same node id and size but show
    /// different images: each page draws its own image.
    #[test]
    fn inline_svg_images_of_two_chapters_are_not_mixed_up() {
        let png = |v: u8| {
            let mut c = std::io::Cursor::new(Vec::new());
            image::GrayImage::from_pixel(8, 8, image::Luma([v])).write_to(&mut c, image::ImageFormat::Png).unwrap();
            c.into_inner()
        };
        let mut res = ink_doc::MemoryLoader::default();
        res.0.insert("black.png".into(), png(0));
        res.0.insert("white.png".into(), png(255));
        let page = |img: &str| {
            format!(
                "<html xmlns='http://www.w3.org/1999/xhtml' xmlns:xlink='http://www.w3.org/1999/xlink'><body><svg xmlns='http://www.w3.org/2000/svg' width='100' height='100' viewBox='0 0 8 8'><image width='8' height='8' xlink:href='{img}'/></svg></body></html>"
            )
        };
        let chapters = vec![HtmlChapter { path: "a.xhtml".into(), html: page("black.png") }, HtmlChapter { path: "b.xhtml".into(), html: page("white.png") }];
        let mut doc = HtmlDocument::new(Metadata::default(), chapters, Box::new(res)).unwrap();
        doc.set_layout(&LayoutSettings::for_screen(600, 800)).unwrap();
        let mut c = Canvas::new(600, 800);
        let mut dark = |p: usize| {
            doc.render_page(p, &mut c).unwrap();
            c.pixels.iter().filter(|&&v| v < 128).count()
        };
        let (black, white) = (dark(0), dark(1));
        assert!(black > 1000, "the black image is drawn: {black} dark pixels");
        assert!(white < black / 10, "the white image is drawn: {white} dark pixels");
    }

    #[test]
    fn reopen_from_cache_mid_chapter_uses_a_prefix_and_keeps_pages() {
        let dir = std::env::temp_dir().join(format!("ink-html-prefix-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let settings = LayoutSettings::for_screen(1272, 1696);
        // The reference: laid out whole, page map cached.
        let mut full = long_book();
        full.set_page_cache(&dir, "book");
        full.set_layout(&settings).unwrap();
        assert!(full.wait_paginated(Duration::from_secs(60)));
        let total = full.page_count().pages;
        assert!(total > 18, "{total} pages");
        let open_at = 8;
        let pos = full.position_of_page(open_at).unwrap();
        let mut want = Vec::new();
        let mut canvas = Canvas::new(settings.width as usize, settings.height as usize);
        for p in 0..total {
            full.render_page(p, &mut canvas).unwrap();
            want.push((full.position_of_page(p).unwrap(), canvas.pixels.clone()));
        }
        drop(full);
        // Reopen at page 9 from the complete cache: only a prefix is laid out.
        let mut doc = long_book();
        doc.set_page_cache(&dir, "book");
        doc.open_at(&pos);
        doc.set_layout(&settings).unwrap();
        assert!(doc.cache_hit());
        assert_eq!(doc.current_page(), open_at);
        let prefix = doc.laid.borrow().get(&0).cloned().unwrap();
        assert!(!prefix.complete, "chapter laid out whole");
        assert!(prefix.pages.len() > open_at + PREFIX_PAGES_AFTER && prefix.pages.len() < total - 5, "{} prefix pages", prefix.pages.len());
        assert_eq!(doc.page_count().pages, total);
        // Every page, inside and past the prefix: same position, same pixels.
        for p in (open_at..total).chain(0..open_at) {
            doc.render_page(p, &mut canvas).unwrap();
            assert_eq!(doc.position_of_page(p).unwrap(), want[p].0, "page {p}");
            assert!(canvas.pixels == want[p].1, "page {p} drawn differently");
        }
        assert!(doc.laid.borrow().get(&0).unwrap().complete);
        assert_eq!(doc.page_of(&pos).unwrap(), open_at);
        assert!(doc.cache_hit() && doc.page_count().complete && doc.page_count().pages == total);
        drop(doc);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Small books of three formats; the big ones are in the ignored test below.
    #[test]
    fn prefix_layouts_match_the_full_layout() {
        prefix_layouts(&["synthetic.epub", "formats/epub3-nav.epub", "formats/sample.fb2", "formats/sample.html"], false);
    }

    #[test]
    #[ignore = "slow: run with --ignored before a release"]
    fn prefix_layouts_match_the_full_layout_big_books() {
        prefix_layouts(&["juliet.epub", "formats/epub2-ncx.epub", "formats/alice-kf8.azw3"], true);
    }

    fn prefix_layouts(names: &[&str], extra: bool) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
        let mut books: Vec<PathBuf> = names.iter().map(|b| root.join(b)).collect();
        // More big books for a release run: INK_PREFIX_BOOKS=a.epub:b.epub.
        if let Some(more) = std::env::var_os("INK_PREFIX_BOOKS").filter(|_| extra) {
            books.extend(std::env::split_paths(&more));
        }
        let mut checked = 0;
        for b in &books {
            checked += check_prefix_layouts(b);
        }
        eprintln!("prefix layouts checked: {checked}");
    }
}
