//! Pagination: slices to pages, the page map of a book, its on-disk cache and the
//! background paginator.
//!
//! - [`paginate`] cuts a chapter's slices into pages: forced breaks, `keep_with_prev`
//!   (headings, widows/orphans, `page-break-inside: avoid`), at least one slice per page.
//! - The page map is, per spine item, the position keys of its page starts plus the
//!   chapter's text size in bytes (for Kindle locations). Global page numbers are
//!   prefix sums, so pages are appended in spine order and never shift.
//! - The cache file `<dir>/<book id>-<layout hash>.pagemap` (text) stores the whole map;
//!   the layout hash covers `LayoutSettings::cache_key()` and [`ENGINE_VERSION`].
//! - [`Worker`] paginates the spine on 1-2 background threads (lower priority, each with
//!   its own fonts) while the foreground lays out the chapters it needs; every chapter is
//!   claimed by one thread in the shared map ([`Claim`]), so no chapter is laid out twice
//!   and the foreground waits for a chapter a worker is doing instead of repeating it.

use crate::book::{Book, ChapterDom};
use crate::layout::{compute_styles, ChapterLayout, Env, Flow, Params};
use crate::style::ComputedStyle;
use ink_doc::timing::step;
use md5::{Digest, Md5};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Bump when layout results change (invalidates cached page maps). The golden test
/// `page_maps_match_engine_version` fails when page breaks change without a bump.
pub const ENGINE_VERSION: u32 = 2;

/// One page of a chapter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageSpan {
    /// First slice.
    pub first: u32,
    /// One past the last slice.
    pub end: u32,
    /// Flow y of the page top.
    pub y0: f32,
    /// Flow y of the page bottom (decorations are clipped here).
    pub y1: f32,
    /// Position key of the first slice.
    pub key: (u32, u32),
}

/// Cuts slices into pages of height `page_h`.
pub fn paginate(lay: &ChapterLayout, page_h: f32) -> Vec<PageSpan> {
    let s = &lay.slices;
    let n = s.len();
    let mut pages = Vec::new();
    let mut i = 0;
    while i < n {
        let start = i;
        let y0 = s[i].top;
        let mut j = i + 1;
        while j < n && !s[j].forced_break && s[j].bottom - y0 <= page_h + 0.5 {
            j += 1;
        }
        if j < n && !s[j].forced_break {
            let mut b = j;
            while b > start + 1 && s[b].keep_with_prev {
                b -= 1;
            }
            if b == start + 1 && s[b].keep_with_prev {
                b = j;
            }
            j = b;
        }
        // Decorations below the last slice (bottom padding, borders) belong to this page
        // up to the next page's first slice.
        let next_top = if j < n { s[j].top } else { lay.height.max(s[j - 1].bottom) };
        let y1 = next_top.min(y0 + page_h).max(s[j - 1].bottom);
        pages.push(PageSpan { first: start as u32, end: j as u32, y0, y1, key: s[start].key });
        i = j;
    }
    pages
}

/// Page map of one chapter.
#[derive(Clone, Debug, PartialEq)]
pub struct ChapterPages {
    /// Page start keys.
    pub keys: Vec<(u32, u32)>,
    /// UTF-8 bytes of the chapter's text (Kindle locations).
    pub text_bytes: u64,
}

/// Who lays out a chapter that is not in the page map yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Claim {
    /// Nobody yet.
    #[default]
    Free,
    /// A background thread.
    Worker,
    /// The foreground (UI) thread.
    Foreground,
    /// A background thread gave it back to the foreground (image sizes it cannot read).
    Deferred,
}

/// The shared page map.
#[derive(Debug, Default)]
pub struct MapState {
    /// Per spine item.
    pub chapters: Vec<Option<ChapterPages>>,
    /// Who lays out each chapter not in the map yet (same length as `chapters`).
    pub claims: Vec<Claim>,
    /// Set when every chapter is known.
    pub complete: bool,
    /// The background threads have finished (map complete and the cache file written,
    /// or cancelled).
    pub finished: bool,
    /// The background thread wrote the cache file.
    pub cache_written: bool,
    /// Background threads still running.
    pub running: usize,
    /// Interest positions resolved by whoever paginated their spine item: position
    /// text -> (spine item, page within it, [`keys_hash`] of the item's page starts).
    pub resolved: std::collections::HashMap<String, (usize, usize, u64)>,
}

impl MapState {
    /// An empty map of `n` chapters.
    pub fn new(n: usize) -> Self {
        MapState { chapters: vec![None; n], claims: vec![Claim::Free; n], ..Default::default() }
    }

    /// Claims the first free chapter (lowest index) for a background thread.
    pub fn claim_next(&mut self) -> Option<usize> {
        let i = (0..self.chapters.len()).find(|&i| self.chapters[i].is_none() && self.claim(i) == Claim::Free)?;
        self.claims[i] = Claim::Worker;
        Some(i)
    }

    /// The claim of chapter `i` (`Free` when out of range of the claim list).
    pub fn claim(&self, i: usize) -> Claim {
        self.claims.get(i).copied().unwrap_or_default()
    }

    /// Sets the claim of chapter `i`.
    pub fn set_claim(&mut self, i: usize, c: Claim) {
        let n = self.chapters.len();
        if self.claims.len() < n {
            self.claims.resize(n, Claim::Free);
        }
        if let Some(x) = self.claims.get_mut(i) {
            *x = c;
        }
    }

    /// Pages known in the contiguous prefix of chapters, and whether that is all.
    pub fn known_pages(&self) -> (usize, bool) {
        let n = self.chapters.iter().map_while(Option::as_ref).map(|c| c.keys.len()).sum();
        (n, self.chapters.iter().all(Option::is_some))
    }

    /// First global page of chapter `i`, when all chapters before it are known.
    pub fn first_page(&self, i: usize) -> Option<usize> {
        self.chapters[..i].iter().map(|c| Some(c.as_ref()?.keys.len())).sum()
    }

    /// [`MapState::first_page`] of every chapter, in one pass.
    pub fn first_pages(&self) -> Vec<Option<usize>> {
        let firsts = self.chapters.iter().scan(Some(0), |next, c| {
            let first = *next;
            *next = first.zip(c.as_ref()).map(|(n, c)| n + c.keys.len());
            Some(first)
        });
        firsts.collect()
    }

    /// (chapter, local page) of global page `p`, within the known prefix.
    pub fn locate(&self, p: usize) -> Option<(usize, usize)> {
        let mut n = 0;
        for (i, c) in self.chapters.iter().enumerate() {
            let c = c.as_ref()?;
            if p < n + c.keys.len() {
                return Some((i, p - n));
            }
            n += c.keys.len();
        }
        None
    }
}

/// Map plus synchronisation.
#[derive(Debug, Default)]
pub struct Shared {
    /// The map.
    pub map: Mutex<MapState>,
    /// Notified when a chapter is added.
    pub changed: Condvar,
    /// Asks the worker to stop.
    pub cancel: AtomicBool,
    /// The full layout of the chapter in view, made in the background after the
    /// foreground drew its first page from a prefix layout ([`start_full_layout`]).
    pub full: Mutex<FullSlot>,
    /// Notified when the full-layout thread ends.
    pub full_done: Condvar,
}

/// A laid-out chapter and its pages.
#[derive(Debug)]
pub struct LaidOut {
    /// The layout.
    pub lay: ChapterLayout,
    /// Its pages (all of them, or the first ones of a prefix layout).
    pub pages: Vec<PageSpan>,
    /// The whole chapter is laid out (false: a prefix, see [`layout_chapter_until`]).
    pub complete: bool,
}

/// The background full layout of one chapter.
#[derive(Default)]
pub struct FullSlot {
    /// The chapter being (or last) laid out.
    pub chapter: Option<usize>,
    /// The thread is still running.
    pub running: bool,
    /// Its layout, until the foreground takes it.
    pub result: Option<Arc<LaidOut>>,
}

impl std::fmt::Debug for FullSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FullSlot").field("chapter", &self.chapter).field("running", &self.running).field("ready", &self.result.is_some()).finish()
    }
}

/// Lays out chapter `c` whole on a low-priority background thread (with its own fonts)
/// and leaves the result in `shared.full`. The foreground drew the first page from a
/// prefix layout; it takes this when the reader turns past the prefix
/// ([`FullSlot`]). Gives up (no result) when an image size is only readable on the
/// foreground or the map is cancelled; a panic gives no result either. The slot's
/// `running` is cleared and `full_done` notified in every case.
pub fn start_full_layout(book: Arc<Book>, params: Params, fonts: crate::fonts::FontSources, shared: Arc<Shared>, c: usize) {
    {
        let mut f = shared.full.lock().unwrap();
        f.chapter = Some(c);
        f.running = true;
        f.result = None;
    }
    struct Done(Arc<Shared>);
    impl Drop for Done {
        fn drop(&mut self) {
            self.0.full.lock().unwrap_or_else(|e| e.into_inner()).running = false;
            self.0.full_done.notify_all();
        }
    }
    let sh = shared.clone();
    // The guard moves into the thread; a thread that cannot start drops it at once.
    let done = Done(shared);
    let spawned = std::thread::Builder::new().name("ink-html-layout".into()).stack_size(16 << 20).spawn(move || {
        let _done = done;
        lower_priority();
        if sh.cancel.load(Ordering::Relaxed) {
            return;
        }
        let t = std::time::Instant::now();
        let (lay, known) = with_shared_env(&book, &mut fonts.build(), |env| layout_chapter(&book, c, &params, env));
        let pages = paginate(&lay, params.content_h);
        if !known || sh.cancel.load(Ordering::Relaxed) {
            log::info!("ink-html: background layout of spine item {c} dropped (image sizes unknown here or cancelled)");
            return;
        }
        log::info!("ink-html: spine item {c} laid out whole in the background in {} ms ({} pages)", t.elapsed().as_millis(), pages.len());
        sh.full.lock().unwrap().result = Some(Arc::new(LaidOut { lay, pages, complete: true }));
    });
    if let Err(e) = spawned {
        log::warn!("ink-html: cannot start the layout thread: {e}");
    }
}

/// Text size of a chapter DOM in UTF-8 bytes (SVG text excluded).
pub fn chapter_text_bytes(dom: &crate::dom::Dom) -> u64 {
    let mut n = 0u64;
    for &id in &dom.preorder {
        if let Some(t) = dom.text(id) {
            let p = dom.node(id).parent;
            if dom.element(p).is_some_and(crate::dom::Element::in_svg_tree) {
                continue;
            }
            n += t.len() as u64;
        }
    }
    n
}

/// Chapter `i` parsed and styled, and when that was done. Records `chapter parse` and
/// `chapter style` in [`ink_doc::timing`].
fn styled_chapter(book: &Book, i: usize, params: &Params) -> (Arc<ChapterDom>, Vec<Option<Box<ComputedStyle>>>, Instant) {
    let t = Instant::now();
    let ch = book.chapter(i);
    let t = step("chapter parse", t);
    let styles = compute_styles(book, &ch, params);
    (ch, styles, step("chapter style", t))
}

/// Lays out chapter `i` and returns its layout (no pages).
/// Records `chapter parse`, `chapter style` and `chapter layout` in [`ink_doc::timing`].
pub fn layout_chapter(book: &Book, i: usize, params: &Params, env: &mut Env) -> ChapterLayout {
    let (ch, styles, t) = styled_chapter(book, i, params);
    let lay = Flow::layout(env, params, &ch.dom, &styles, &book.chapters[i].path, book.meta.language.clone());
    step("chapter layout", t);
    lay
}

/// A page of a prefix layout is kept when the slice that starts the next page lies this
/// many pages above the stop (see [`prefix_pages`]).
const PREFIX_MARGIN_PAGES: f32 = 3.0;

/// The pages of a layout made by [`Flow::layout_until`] that the full layout has too.
///
/// A complete layout gives all its pages. A stopped one gives the pages whose next page
/// starts more than [`PREFIX_MARGIN_PAGES`] pages above the stop: a page's end depends
/// only on slices up to about one page below its start, and the only slices a stopped
/// layout can lack or have different are the ones of blocks still open at the stop
/// (`page-break-inside: avoid` changes a block only when it is shorter than a page;
/// borders are clipped to the page).
pub fn prefix_pages(lay: &ChapterLayout, complete: bool, page_h: f32) -> Vec<PageSpan> {
    let mut pages = paginate(lay, page_h);
    if complete {
        return pages;
    }
    let s = &lay.slices;
    let keep = pages.iter().take_while(|p| s.get(p.end as usize).is_some_and(|next| next.top + PREFIX_MARGIN_PAGES * page_h < lay.height)).count();
    pages.truncate(keep);
    pages
}

/// Lays out chapter `i` only as far as needed for the page holding `key` and
/// `pages_after` pages after it. Returns the layout, its pages (a prefix of the full
/// layout's pages) and whether that is the whole chapter. When the stopped layout does
/// not reach far enough (a giant paragraph, a tall table), the chapter is laid out whole.
/// Records the same [`ink_doc::timing`] steps as [`layout_chapter`].
pub fn layout_chapter_until(book: &Book, i: usize, params: &Params, env: &mut Env, key: (u32, u32), pages_after: usize) -> (ChapterLayout, Vec<PageSpan>, bool) {
    let (ch, styles, t) = styled_chapter(book, i, params);
    let path = &book.chapters[i].path;
    let extra = (pages_after as f32 + PREFIX_MARGIN_PAGES + 2.0) * params.content_h;
    let stop = crate::layout::StopAt::new(key, extra);
    let (lay, complete) = Flow::layout_until(env, params, &ch.dom, &styles, path, book.meta.language.clone(), Some(stop));
    let pages = prefix_pages(&lay, complete, params.content_h);
    let at = pages.partition_point(|p| p.key <= key).saturating_sub(1);
    let out = if complete || at + pages_after < pages.len() {
        (lay, pages, complete)
    } else {
        log::debug!("ink-html: prefix layout of spine item {i} too short ({} pages, need {}); laying it out whole", pages.len(), at + pages_after + 1);
        let lay = Flow::layout(env, params, &ch.dom, &styles, path, book.meta.language.clone());
        let pages = paginate(&lay, params.content_h);
        (lay, pages, true)
    };
    step("chapter layout", t);
    out
}

/// Lays out and paginates chapter `i` for the page map.
pub fn chapter_pages(book: &Book, i: usize, params: &Params, env: &mut Env) -> ChapterPages {
    let lay = layout_chapter(book, i, params, env);
    let pages = paginate(&lay, params.content_h);
    let text_bytes = chapter_text_bytes(&book.chapter(i).dom);
    ChapterPages { keys: pages.iter().map(|p| p.key).collect(), text_bytes }
}

/// FNV-1a hash of a spine item's page starts (a remembered page within the item is
/// valid while this does not change).
pub fn keys_hash(keys: &[(u32, u32)]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut add = |v: u32| {
        for b in v.to_le_bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
    };
    add(keys.len() as u32);
    for &(o, k) in keys {
        add(o);
        add(k);
    }
    h
}

/// Positions (TOC entries, page labels) per spine item whose pages the paginator works
/// out while it has the item's DOM: (position text, pointer).
pub type Interest = Arc<Vec<Vec<(String, ink_doc::XPointer)>>>;

/// Resolves the interest positions of spine item `i` against its page starts `keys`
/// and stores them in the map (the DOM is parsed already when this runs).
pub fn resolve_interest(book: &Book, i: usize, keys: &[(u32, u32)], interest: &Interest, m: &Mutex<MapState>) {
    let Some(list) = interest.get(i).filter(|l| !l.is_empty()) else { return };
    if keys.is_empty() {
        return;
    }
    let ch = book.chapter(i);
    let hash = keys_hash(keys);
    let found: Vec<(String, usize)> = list
        .iter()
        .filter_map(|(text, xp)| {
            let p = crate::xpointer::resolve_lenient(&ch.dom, xp)?;
            let key = crate::xpointer::order_key(&ch.dom, p);
            Some((text.clone(), keys.partition_point(|k| *k <= key).saturating_sub(1)))
        })
        .collect();
    let mut m = m.lock().unwrap_or_else(|e| e.into_inner());
    for (text, local) in found {
        m.resolved.insert(text, (i, local, hash));
    }
}

/// Runs `f` with an [`Env`] for a thread off the main thread: image sizes come from the
/// size cache, else the shared loader. Also returns whether every size was known (false
/// when an image is neither cached nor readable here); a layout made with an unknown
/// size is not the foreground's layout.
fn with_shared_env<R>(book: &Book, fonts: &mut crate::fonts::FontBook, f: impl FnOnce(&mut Env) -> R) -> (R, bool) {
    let mut known = true;
    let loader = book.loader.as_deref().map(|l| l as &dyn ink_doc::ResourceLoader);
    let mut size = |p: &str| {
        let s = book.image_size(p, loader);
        known &= s.is_some();
        s.flatten()
    };
    let r = f(&mut Env { fonts, image_size: &mut size });
    (r, known)
}

/// The cache file for a book and layout.
pub fn cache_file(dir: &Path, book_id: &str, settings: &ink_doc::LayoutSettings) -> PathBuf {
    let mut h = Md5::new();
    h.update(format!("{}|v{}", settings.cache_key(), ENGINE_VERSION).as_bytes());
    let hex: String = h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect();
    let id: String = book_id.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    dir.join(format!("{id}-{hex}.pagemap"))
}

/// Serializes a map; chapters not paginated yet are written as `?` (a partial map,
/// written when a book is closed before its pagination finished).
pub fn write_cache(path: &Path, chapters: &[Option<ChapterPages>]) -> std::io::Result<()> {
    use std::fmt::Write as _;
    let mut s = format!("rinky-pagemap {ENGINE_VERSION} {}\n", chapters.len());
    for c in chapters {
        match c {
            Some(c) => {
                let _ = write!(s, "{}", c.text_bytes);
                for (o, k) in &c.keys {
                    let _ = write!(s, " {o}.{k}");
                }
            }
            None => s.push('?'),
        }
        s.push('\n');
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, s)?;
    std::fs::rename(&tmp, path)
}

/// Reads a cached map for `n` chapters (`None` entries: not paginated in that map).
pub fn read_cache(path: &Path, n: usize) -> Option<Vec<Option<ChapterPages>>> {
    let s = std::fs::read_to_string(path).ok()?;
    let mut lines = s.lines();
    let head: Vec<&str> = lines.next()?.split(' ').collect();
    if head.len() != 3 || head[0] != "rinky-pagemap" || head[1] != ENGINE_VERSION.to_string() || head[2] != n.to_string() {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for line in lines.take(n) {
        if line == "?" {
            out.push(None);
            continue;
        }
        let mut parts = line.split(' ');
        let text_bytes = parts.next()?.parse().ok()?;
        let mut keys = Vec::new();
        for p in parts {
            let (o, k) = p.split_once('.')?;
            keys.push((o.parse().ok()?, k.parse().ok()?));
        }
        out.push(Some(ChapterPages { keys, text_bytes }));
    }
    (out.len() == n).then_some(out)
}

/// Number of background pagination threads: `RINKY_PAGINATE_THREADS` when set
/// (1-8), else the cores minus one for the UI thread, at most 2. The PW12 has four
/// Cortex-A53 cores; each thread holds one chapter's DOM and layout (about 50 MB for a
/// 36 000-word chapter on the host), so a third thread costs more memory than it saves
/// time (big36: 1 thread 720 ms / 255 MB, 2: 430 ms / 309 MB, 3: 310 ms / 354 MB).
pub fn worker_threads() -> usize {
    if let Some(n) = std::env::var("RINKY_PAGINATE_THREADS").ok().and_then(|v| v.parse::<usize>().ok()) {
        return n.clamp(1, 8);
    }
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    cores.saturating_sub(1).clamp(1, 2)
}

/// Lowers the calling thread's scheduling priority (nice 10 on Linux) so page turns on
/// the UI thread stay smooth while it paginates (also the library metadata thread).
/// Elsewhere it does nothing.
pub fn lower_priority() {
    #[cfg(target_os = "linux")]
    {
        // Linux applies nice per thread (the task id from gettid).
        // SAFETY: plain syscalls on the calling thread; no memory is passed.
        let r = unsafe {
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            libc::setpriority(libc::PRIO_PROCESS, tid, 10)
        };
        if r != 0 {
            log::debug!("ink-html: paginator nice 10 failed: {}", std::io::Error::last_os_error());
        }
    }
}

/// A chapter whose source is bigger than this is dropped from the DOM cache after a
/// background thread paginated it (memory stays flat on books with huge spine items).
const EVICT_BYTES: usize = 256 * 1024;

/// The background paginator: 1-2 threads that take chapters in spine order.
pub struct Worker {
    /// Shared state.
    pub shared: Arc<Shared>,
}

/// Work the last background thread runs on the shared state when it ends.
type LastWork = Box<dyn FnOnce(&Shared) + Send>;

/// Counts a background thread out when it ends (also by a panic); the last one marks
/// the map finished.
struct RunGuard {
    shared: Arc<Shared>,
    last: Option<LastWork>,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let last = {
            let mut m = self.shared.map.lock().unwrap_or_else(|e| e.into_inner());
            m.running = m.running.saturating_sub(1);
            m.running == 0
        };
        if last {
            if let Some(f) = self.last.take() {
                f(&self.shared);
            }
            self.shared.map.lock().unwrap_or_else(|e| e.into_inner()).finished = true;
        }
        self.shared.changed.notify_all();
    }
}

impl Worker {
    /// Starts paginating every chapter that is not in the map and not claimed yet, on
    /// [`worker_threads`] threads. When the map is complete and `cache` is set, the last
    /// thread to end writes the cache file.
    pub fn start(book: Arc<Book>, params: Params, fonts: crate::fonts::FontSources, shared: Arc<Shared>, cache: Option<PathBuf>, interest: Interest) -> Worker {
        let threads = worker_threads().min(book.chapters.len().max(1));
        let started = std::time::Instant::now();
        {
            let mut m = shared.map.lock().unwrap();
            m.running = threads;
            let n = m.chapters.len();
            m.claims.resize(n, Claim::Free);
        }
        log::info!("ink-html: background pagination of {} spine items on {threads} threads", book.chapters.len());
        for t in 0..threads {
            let (book, params, fonts, sh, interest) = (book.clone(), params.clone(), fonts.clone(), shared.clone(), interest.clone());
            // Every thread gets the path: whichever ends last writes the file (when the
            // map is complete), marks the map complete and logs the total time.
            let (cache, n) = (cache.clone(), book.chapters.len());
            let last: LastWork = Box::new(move |sh: &Shared| finish_map(sh, cache, n, started));
            // The guard moves into the thread; a thread that cannot start drops it at
            // once, so the running count never waits for it.
            let guard = RunGuard { shared: sh.clone(), last: Some(last) };
            let spawned = std::thread::Builder::new()
                .name(format!("ink-html-paginate-{t}"))
                .stack_size(16 << 20)
                .spawn(move || {
                    let _guard = guard;
                    lower_priority();
                    paginate_claimed(&book, &params, &fonts, &sh, &interest);
                });
            if let Err(e) = spawned {
                log::warn!("ink-html: cannot start paginator thread {t}: {e}");
            }
        }
        Worker { shared }
    }

    /// Waits until the threads have finished (map complete, cache file written) or
    /// `timeout` passes. Returns true when they have.
    pub fn wait(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut m = self.shared.map.lock().unwrap();
        while !m.finished {
            let now = std::time::Instant::now();
            if now >= deadline {
                return false;
            }
            m = self.shared.changed.wait_timeout(m, deadline - now).unwrap().0;
        }
        true
    }
}

/// One background thread: takes the first free chapter until none is left.
fn paginate_claimed(book: &Book, params: &Params, fonts: &crate::fonts::FontSources, sh: &Shared, interest: &Interest) {
    let mut fb = fonts.build();
    let n = book.chapters.len();
    loop {
        if sh.cancel.load(Ordering::Relaxed) {
            return;
        }
        let Some(i) = sh.map.lock().unwrap().claim_next() else { return };
        let was_parsed = book.has_chapter(i);
        let (pages, known) = with_shared_env(book, &mut fb, |env| chapter_pages(book, i, params, env));
        let mut m = sh.map.lock().unwrap();
        if !known {
            // An image whose size only the foreground can read (the EPUB loader is not
            // shared and the prefill scan missed it): leave the chapter to the
            // foreground instead of storing a layout without the image.
            m.set_claim(i, Claim::Deferred);
            drop(m);
            log::info!("ink-html: chapter {i} left to the foreground (image sizes unknown here)");
        } else {
            drop(m);
            resolve_interest(book, i, &pages.keys, interest, &sh.map);
            let mut m = sh.map.lock().unwrap();
            if m.chapters[i].is_none() {
                m.chapters[i] = Some(pages);
            }
        }
        sh.changed.notify_all();
        // Keep memory flat on big books: drop the DOMs only this thread parsed.
        if !was_parsed && (n > 40 || book.chapters[i].src_len() > EVICT_BYTES) {
            book.evict_chapter(i);
        }
    }
}

/// Runs when the last background thread ends: writes the cache file when the map is
/// complete (before the map is marked complete, so a process that exits after
/// [`Worker::wait`] keeps it) and logs the time.
fn finish_map(sh: &Shared, cache: Option<PathBuf>, n: usize, started: std::time::Instant) {
    let snapshot = {
        let m = sh.map.lock().unwrap_or_else(|e| e.into_inner());
        let complete = m.chapters.iter().all(|c| c.is_some());
        if complete && cache.is_some() && !sh.cancel.load(Ordering::Relaxed) {
            Some(m.chapters.clone())
        } else {
            None
        }
    };
    if let (Some(path), Some(chapters)) = (cache, snapshot) {
        match write_cache(&path, &chapters) {
            Ok(()) => {
                sh.map.lock().unwrap_or_else(|e| e.into_inner()).cache_written = true;
                log::info!("ink-html: page map cached in {}", path.display())
            }
            Err(e) => log::warn!("ink-html: cannot write page map {}: {e}", path.display()),
        }
    }
    let pages = {
        let mut m = sh.map.lock().unwrap_or_else(|e| e.into_inner());
        m.complete = m.chapters.iter().all(|c| c.is_some());
        m.known_pages().0
    };
    let how = if sh.cancel.load(Ordering::Relaxed) { "stopped" } else { "done" };
    log::info!("ink-html: background pagination {how} in {:?} ({n} spine items, {pages} pages known)", started.elapsed());
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Do not wait: the threads stop before their next chapter.
        self.shared.cancel.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Slice;

    fn slice(top: f32, bottom: f32, key: u32) -> Slice {
        Slice { top, bottom, key: (key, 0), forced_break: false, keep_with_prev: false, first_item: 0, end_item: 0 }
    }

    #[test]
    fn breaks_forced_keep_and_tall_slices() {
        let mut lay = ChapterLayout::default();
        for i in 0..10 {
            lay.slices.push(slice(i as f32 * 30.0, i as f32 * 30.0 + 30.0, i));
        }
        let p = paginate(&lay, 100.0);
        assert_eq!(p.iter().map(|p| (p.first, p.end)).collect::<Vec<_>>(), vec![(0, 3), (3, 6), (6, 9), (9, 10)]);
        lay.slices[5].forced_break = true;
        lay.slices[3].keep_with_prev = true; // e.g. a heading at 2
        let p = paginate(&lay, 100.0);
        assert_eq!(p.iter().map(|p| (p.first, p.end)).collect::<Vec<_>>(), vec![(0, 2), (2, 5), (5, 8), (8, 10)]);
        assert_eq!(p[1].key, (2, 0));
        // A slice taller than the page gets its own page.
        let mut tall = ChapterLayout::default();
        tall.slices.push(slice(0.0, 30.0, 0));
        tall.slices.push(slice(30.0, 300.0, 1));
        tall.slices.push(slice(300.0, 330.0, 2));
        let p = paginate(&tall, 100.0);
        assert_eq!(p.iter().map(|p| (p.first, p.end)).collect::<Vec<_>>(), vec![(0, 1), (1, 2), (2, 3)]);
        // keep_with_prev everywhere cannot empty a page.
        for s in &mut lay.slices {
            s.keep_with_prev = true;
            s.forced_break = false;
        }
        assert!(paginate(&lay, 100.0).iter().all(|p| p.end > p.first));
    }

    #[test]
    fn cache_round_trip_and_map_queries() {
        let dir = std::env::temp_dir().join(format!("ink-html-cache-{}", std::process::id()));
        let s = ink_doc::LayoutSettings::for_screen(1272, 1696);
        let f = cache_file(&dir, "abc123", &s);
        let mut s2 = s.clone();
        s2.font_px += 1.0;
        assert_ne!(f, cache_file(&dir, "abc123", &s2));
        let chapters = vec![Some(ChapterPages { keys: vec![(0, 0), (40, 3)], text_bytes: 99 }), Some(ChapterPages { keys: vec![], text_bytes: 0 })];
        write_cache(&f, &chapters).unwrap();
        assert_eq!(read_cache(&f, 2), Some(chapters.clone()));
        assert_eq!(read_cache(&f, 3), None);
        // A partial map (book closed while paginating) keeps its unknown chapters.
        let partial = vec![chapters[0].clone(), None];
        write_cache(&f, &partial).unwrap();
        assert_eq!(read_cache(&f, 2), Some(partial));
        std::fs::remove_dir_all(&dir).unwrap();
        let m = MapState { chapters: vec![chapters[0].clone(), None, chapters[0].clone()], ..Default::default() };
        assert_eq!(m.known_pages(), (2, false));
        assert_eq!(m.locate(1), Some((0, 1)));
        assert_eq!(m.locate(2), None);
        assert_eq!(m.first_page(2), None);
        assert_eq!(m.first_page(1), Some(2));
        assert_eq!(m.first_pages(), vec![Some(0), Some(2), None]);
        let full = MapState { chapters: vec![chapters[0].clone(), chapters[1].clone(), chapters[0].clone()], ..Default::default() };
        assert_eq!(full.known_pages(), (4, true));
        assert_eq!(full.first_pages(), vec![Some(0), Some(2), Some(2)]);
        assert_eq!((full.first_page(3), full.locate(3)), (Some(4), Some((2, 1))));
    }
}
