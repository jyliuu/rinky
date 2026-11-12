//! Annotations of the settings' [`ink_doc::TextAnnotator`]s drawn over a laid-out page.
//!
//! The layout only reserves room: every line of a paragraph an annotator wants gets the
//! room of an annotation, and the extra space between ideographs it declares
//! ([`crate::layout::AnnotatedPara`]). [`ink_doc::TextAnnotator::annotate`] is never called
//! while paginating. When a page is drawn, the annotations of its paragraphs come from a
//! small cache; the missing ones are computed by a background thread ([`Layer`]), the
//! page's first, then those of the pages around it, and the page is drawn again when
//! they are ready ([`ink_doc::Document::annotations_pending`]).
//!
//! Annotations never move the text. They are placed into the room the layout reserved,
//! in one pass per line ([`place`]): centred over their characters at one size for the
//! paragraph, the largest that keeps [`GAP`] between neighbours (closed form from the
//! tightest pair), at least [`FLOOR`] of the normal size; where even that does not fit,
//! one pass to the right and one back from the line's end keep them apart, off centre.

use crate::fonts::{FaceKey, FontBook};
use crate::layout::{AnnotatedPara, AnnotatorState, ChapterLayout, Draw, TextRun};
use crate::paginate::PageSpan;
use ink_device::Canvas;
use ink_doc::{Annotation, AnnotationPosition, LayoutSettings, TextAnnotator};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

/// Annotation size as a share of the text's.
const SCALE: f32 = 0.5;
/// Least space between two annotations side by side, em of the annotation.
pub const GAP: f32 = 0.35;
/// Smallest size annotations are condensed to, share of their normal size.
pub const FLOOR: f32 = 0.55;
/// Paragraphs (per annotator) the cache keeps.
pub const CACHE_PARAGRAPHS: usize = 384;

/// A cached paragraph: (layout generation, chapter, paragraph, annotator).
pub type Key = (u64, u32, u32, u8);

/// The annotations of one paragraph by one annotator, at a version.
#[derive(Clone)]
struct Entry {
    version: u64,
    list: Arc<Vec<Annotation>>,
    used: u64,
}

/// Work for the background thread.
struct Job {
    key: Key,
    text: Arc<str>,
    lang: Option<Arc<str>>,
    annotator: Arc<dyn TextAnnotator>,
    version: u64,
    state: AnnotatorState,
}

#[derive(Default)]
struct State {
    cache: HashMap<Key, Entry>,
    queue: VecDeque<Job>,
    clock: u64,
    stop: bool,
    running: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

/// The annotation cache and its background thread (started on first use).
#[derive(Default)]
pub struct Layer {
    shared: Arc<Shared>,
    /// Layout generation: bumped by every new layout, so old entries are never used.
    pub generation: u64,
}

impl Drop for Layer {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        self.shared.wake.notify_all();
    }
}

/// Runs `annotate` for one job, catching a panic (which turns that annotator off).
fn run(job: &Job) -> Option<Vec<Annotation>> {
    let a = &job.annotator;
    let ai = job.key.3 as usize;
    if job.state.failed(ai) {
        return None;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.annotate(&job.text, job.lang.as_deref()))) {
        Ok(list) => Some(list),
        Err(e) => {
            if job.state.fail(ai) {
                let what = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_default();
                log::error!("ink-html: annotator {} panicked ({what}); no more of its annotations in this book", a.id());
            }
            None
        }
    }
}

impl Layer {
    /// A new layout: the cache and the queue start empty.
    pub fn reset(&mut self) {
        self.generation += 1;
        let mut st = self.shared.state.lock().unwrap();
        st.cache.clear();
        st.queue.clear();
    }

    /// The cached annotations of `key` and whether they are of `version` (stale ones
    /// are still drawn while the new ones are computed).
    fn get(&self, key: Key, version: u64) -> Option<(Arc<Vec<Annotation>>, bool)> {
        let mut st = self.shared.state.lock().unwrap();
        st.clock += 1;
        let clock = st.clock;
        let e = st.cache.get_mut(&key)?;
        e.used = clock;
        Some((e.list.clone(), e.version == version))
    }

    /// True when `key` is cached at `version`.
    pub fn fresh(&self, key: Key, version: u64) -> bool {
        self.shared.state.lock().unwrap().cache.get(&key).is_some_and(|e| e.version == version)
    }

    fn insert(shared: &Shared, key: Key, version: u64, list: Vec<Annotation>) {
        let mut st = shared.state.lock().unwrap();
        st.clock += 1;
        let used = st.clock;
        st.cache.insert(key, Entry { version, list: Arc::new(list), used });
        if st.cache.len() > CACHE_PARAGRAPHS {
            // The least recently used eighth goes.
            let mut uses: Vec<u64> = st.cache.values().map(|e| e.used).collect();
            let k = uses.len() / 8;
            let (_, &mut cut, _) = uses.select_nth_unstable(k);
            st.cache.retain(|_, e| e.used > cut);
        }
    }

    /// Paragraphs in the cache (tests).
    pub fn cached(&self) -> usize {
        self.shared.state.lock().unwrap().cache.len()
    }

    /// Replaces the queue with `jobs` (the page shown first, then those around it): work
    /// for pages left behind is dropped. Starts the thread when needed.
    fn queue(&self, jobs: Vec<Job>) {
        let mut st = self.shared.state.lock().unwrap();
        st.queue.clear();
        st.queue.extend(jobs);
        if !st.running && !st.queue.is_empty() {
            st.running = true;
            let shared = self.shared.clone();
            let spawned = std::thread::Builder::new().name("ink-html-annotate".into()).spawn(move || {
                crate::paginate::lower_priority();
                loop {
                    let job = {
                        let mut st = shared.state.lock().unwrap();
                        loop {
                            if st.stop {
                                return;
                            }
                            if let Some(j) = st.queue.pop_front() {
                                break j;
                            }
                            st = shared.wake.wait(st).unwrap();
                        }
                    };
                    let t = std::time::Instant::now();
                    if let Some(list) = run(&job) {
                        log::debug!("ink-html: {} annotations of paragraph {} in {:?}", list.len(), job.key.2, t.elapsed());
                        Self::insert(&shared, job.key, job.version, list);
                    } else {
                        // A failed annotator: an empty entry, so the page is not pending.
                        Self::insert(&shared, job.key, job.version, Vec::new());
                    }
                }
            });
            if let Err(e) = spawned {
                log::warn!("ink-html: cannot start the annotation thread: {e}");
                st.running = false;
            }
        }
        drop(st);
        self.shared.wake.notify_all();
    }
}

/// One annotation placed on a line.
struct Item {
    /// Centre of its characters (page px).
    centre: f32,
    line: f32,
    /// Width at the normal size.
    w: f32,
    face: FaceKey,
    /// Normal size.
    px: f32,
    text: String,
    baseline: f32,
    base: (FaceKey, f32),
}

/// What [`Layer::draw_page`] did, for the log and the reader.
#[derive(Default, Debug)]
pub struct PageStats {
    /// Annotations drawn.
    pub drawn: usize,
    /// Paragraph annotations the page still waits for.
    pub missing: Vec<(Key, u64)>,
    /// The annotations as drawn, when recorded (tests, tools).
    pub placed: Vec<Placed>,
}

/// An annotation as drawn on a page (page coordinates).
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    /// The annotation text.
    pub text: String,
    /// Left edge.
    pub x: f32,
    /// Width as drawn.
    pub width: f32,
    /// Baseline (page px).
    pub baseline: f32,
    /// Size as drawn.
    pub px: f32,
    /// Drawn under the text.
    pub under: bool,
}

/// The text runs of a page (with a DOM node), sorted by node and offset.
fn page_runs<'a>(lay: &'a ChapterLayout, span: &PageSpan) -> Vec<&'a TextRun> {
    let slices = &lay.slices[span.first as usize..span.end as usize];
    let (Some(a), Some(b)) = (slices.first(), slices.last()) else { return Vec::new() };
    let mut v: Vec<&TextRun> = lay.items[a.first_item as usize..b.end_item as usize]
        .iter()
        .filter_map(|i| match i {
            Draw::Text(t) if t.node != crate::dom::NONE => Some(t),
            _ => None,
        })
        .collect();
    v.sort_by_key(|t| (t.node, t.start));
    v
}

/// The run holding DOM char `dom` of `node`, and the char's index in it.
fn run_at<'a>(runs: &[&'a TextRun], node: u32, dom: u32) -> Option<(&'a TextRun, usize)> {
    let i = runs.partition_point(|t| (t.node, t.start) <= (node, dom)).checked_sub(1)?;
    let t = runs[i];
    (t.node == node && dom < t.start + t.len.max(1)).then(|| (t, (dom - t.start) as usize))
}

/// x of char `k` of run `t` (its left edge).
fn char_x(fonts: &mut FontBook, t: &TextRun, k: usize) -> f32 {
    if k == 0 {
        return t.x;
    }
    let end = t.text.char_indices().nth(k).map_or(t.text.len(), |(b, _)| b);
    t.x + fonts.width(t.face, t.px, &t.text[..end]) + t.letter_spacing * k as f32
}

/// The paragraphs of `lay` with text on the page: their indexes.
pub fn paragraphs_on(lay: &ChapterLayout, span: &PageSpan) -> Vec<usize> {
    if lay.annotated.is_empty() {
        return Vec::new();
    }
    let runs = page_runs(lay, span);
    let mut nodes: Vec<u32> = runs.iter().map(|t| t.node).collect();
    nodes.dedup();
    lay.annotated.iter().enumerate().filter(|(_, p)| p.map.iter().any(|m| nodes.binary_search(&m.1).is_ok())).map(|(i, _)| i).collect()
}

impl Layer {
    /// Draws the annotations of page `span` of chapter `chapter` (laid out as `lay`,
    /// flow-to-page offset `dy`) over `canvas`: the cached ones; the missing (or stale)
    /// ones of the page are queued first, those of `around` (paragraphs of the pages
    /// around it) after them. With `sync` the missing ones are computed here.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_page(&self, canvas: &mut Canvas, fonts: &mut FontBook, s: &LayoutSettings, state: &AnnotatorState, chapter: usize, lay: &ChapterLayout, span: &PageSpan, dy: f32, around: &[usize], sync: bool, record: bool) -> PageStats {
        let mut stats = PageStats::default();
        let paras = paragraphs_on(lay, span);
        if paras.is_empty() && around.is_empty() {
            return stats;
        }
        let key = |p: usize, ai: usize| -> Key { (self.generation, chapter as u32, p as u32, ai as u8) };
        let job = |p: usize, ai: usize| {
            let para = &lay.annotated[p];
            let a = s.annotators[ai].annotator().clone();
            Job { key: key(p, ai), text: para.text.clone(), lang: para.lang.clone(), version: a.version(), annotator: a, state: state.clone() }
        };
        // What the page has, and what it waits for.
        let mut lists: Vec<(usize, usize, Arc<Vec<Annotation>>)> = Vec::new();
        let mut jobs = Vec::new();
        for &p in &paras {
            for ai in 0..s.annotators.len().min(64) {
                if lay.annotated[p].wanted & (1 << ai) == 0 || state.failed(ai) {
                    continue;
                }
                let version = s.annotators[ai].annotator().version();
                let got = self.get(key(p, ai), version);
                match got {
                    Some((list, true)) => lists.push((p, ai, list)),
                    _ if sync => {
                        let j = job(p, ai);
                        let list = run(&j).unwrap_or_default();
                        Self::insert(&self.shared, j.key, version, list);
                        if let Some((list, _)) = self.get(j.key, version) {
                            lists.push((p, ai, list));
                        }
                    }
                    other => {
                        if let Some((list, false)) = other {
                            lists.push((p, ai, list));
                        }
                        stats.missing.push((key(p, ai), version));
                        jobs.push(job(p, ai));
                    }
                }
            }
        }
        if !sync {
            for &p in around {
                for ai in 0..s.annotators.len().min(64) {
                    let version = s.annotators[ai].annotator().version();
                    if lay.annotated[p].wanted & (1 << ai) != 0 && !state.failed(ai) && !self.fresh(key(p, ai), version) && !paras.contains(&p) {
                        jobs.push(job(p, ai));
                    }
                }
            }
            if !jobs.is_empty() {
                self.queue(jobs);
            }
        }
        if !lists.is_empty() {
            let mut placed = record.then(Vec::new);
            stats.drawn = place_and_draw(canvas, fonts, s, lay, span, dy, &lists, placed.as_mut());
            stats.placed = placed.unwrap_or_default();
        }
        stats
    }
}

/// Places the annotations `lists` ((paragraph, annotator, annotations)) on the page and
/// draws them. Returns how many were drawn.
#[allow(clippy::too_many_arguments)]
fn place_and_draw(canvas: &mut Canvas, fonts: &mut FontBook, s: &LayoutSettings, lay: &ChapterLayout, span: &PageSpan, dy: f32, lists: &[(usize, usize, Arc<Vec<Annotation>>)], mut record: Option<&mut Vec<Placed>>) -> usize {
    let runs = page_runs(lay, span);
    let (left_edge, right_edge) = (s.margin_left, s.width as f32 - s.margin_right);
    let mut drawn = 0;
    // Per paragraph: the items of each tier, then place them.
    let mut i = 0;
    let mut items: [Vec<Item>; 2] = Default::default();
    while i < lists.len() {
        let p = lists[i].0;
        let para: &AnnotatedPara = &lay.annotated[p];
        items[0].clear();
        items[1].clear();
        // Ranges taken per tier: the book's ruby first, then each annotator in order.
        let mut taken: [Vec<(u32, u32)>; 2] = Default::default();
        for &(a, b, under) in &para.book {
            taken[usize::from(under)].push((a, b));
        }
        for t in &mut taken {
            t.sort_unstable();
        }
        while i < lists.len() && lists[i].0 == p {
            let (_, ai, ref list) = lists[i];
            let positions = s.annotators[ai].annotator().positions();
            for an in list.iter() {
                let tier = match an.position {
                    AnnotationPosition::Over => 0,
                    AnnotationPosition::Under => 1,
                    #[allow(unreachable_patterns)]
                    _ => continue,
                };
                let (st, en) = (an.range.start, an.range.end);
                let text = &para.text;
                if !positions.contains(&an.position) || st >= en || en > text.len() || !text.is_char_boundary(st) || !text.is_char_boundary(en) || an.text.trim().is_empty() {
                    continue;
                }
                // Taken ranges are kept sorted: annotators usually give them in order,
                // then this is a push.
                let t = &mut taken[tier];
                let k = t.partition_point(|&(a, _)| (a as usize) < en);
                if k > 0 && t[k - 1].1 as usize > st {
                    continue;
                }
                t.insert(k, (st as u32, en as u32));
                // Its characters on the page (one line).
                let last = text[..en].chars().next_back().map_or(st, |c| en - c.len_utf8());
                let (Some((na, da)), Some((nb, db))) = (para.source(st), para.source(last)) else { continue };
                let (Some((ta, ka)), Some((tb, kb))) = (run_at(&runs, na, da), run_at(&runs, nb, db)) else { continue };
                if ta.line_top != tb.line_top {
                    continue;
                }
                let x0 = char_x(fonts, ta, ka);
                let x1 = char_x(fonts, tb, kb + 1);
                let px = ta.px * SCALE;
                let w = fonts.width(ta.face, px, an.text.trim());
                items[tier].push(Item { centre: (x0 + x1) / 2.0, line: ta.line_top, w, face: ta.face, px, text: an.text.trim().to_string(), baseline: ta.baseline, base: (ta.face, ta.px) });
            }
            i += 1;
        }
        for (tier, list) in items.iter_mut().enumerate() {
            drawn += place(list, left_edge, right_edge, |it, x, scale| {
                let px = it.px * scale;
                let (a, d) = fonts.vmetrics(it.base.0, it.base.1);
                let (ra, rd) = fonts.vmetrics(it.face, px);
                let base = if tier == 0 { it.baseline - a + rd } else { it.baseline - d + ra };
                fonts.draw(canvas, it.face, px, x, base + dy, &it.text);
                if let Some(r) = record.as_deref_mut() {
                    r.push(Placed { text: it.text.clone(), x, width: it.w * scale, baseline: base + dy, px, under: tier == 1 });
                }
            });
        }
    }
    drawn
}

/// Places the items of one paragraph's tier (in any order) and calls `draw(item, x,
/// scale)` for each: one size for all (the largest that keeps every pair on a line
/// [`GAP`] apart when centred and inside the line, at least [`FLOOR`]), centred; where
/// that still does not fit, one pass right and one back from the line's end. Linear
/// after the sort. Returns the count.
fn place(items: &mut [Item], left: f32, right: f32, mut draw: impl FnMut(&Item, f32, f32)) -> usize {
    if items.is_empty() {
        return 0;
    }
    items.sort_by(|a, b| a.line.total_cmp(&b.line).then(a.centre.total_cmp(&b.centre)));
    // The scale: the tightest pair (or line edge) decides.
    let mut scale = 1.0f32;
    for (k, it) in items.iter().enumerate() {
        let gap = GAP * it.px;
        if let Some(n) = items.get(k + 1).filter(|n| n.line == it.line) {
            let need = (it.w + n.w) / 2.0 + gap;
            if need > 0.0 {
                scale = scale.min((n.centre - it.centre) / need);
            }
        }
        if it.w > 0.0 {
            scale = scale.min(2.0 * (it.centre - left) / it.w).min(2.0 * (right - it.centre) / it.w);
        }
    }
    let scale = scale.clamp(FLOOR, 1.0);
    // Centred at that size; where that overlaps (at the floor), pushed apart.
    let mut xs: Vec<f32> = items.iter().map(|it| it.centre - it.w * scale / 2.0).collect();
    let mut start = 0;
    while start < items.len() {
        let mut end = start + 1;
        while end < items.len() && items[end].line == items[start].line {
            end += 1;
        }
        let mut reach = left;
        for k in start..end {
            xs[k] = xs[k].max(reach);
            reach = xs[k] + items[k].w * scale + GAP * items[k].px * scale;
        }
        let mut limit = right;
        for k in (start..end).rev() {
            xs[k] = xs[k].min(limit - items[k].w * scale);
            limit = xs[k] - GAP * items[k].px * scale;
        }
        start = end;
    }
    for (it, &x) in items.iter().zip(&xs) {
        draw(it, x, scale);
    }
    items.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_keeps_a_bounded_number_of_paragraphs() {
        let layer = Layer::default();
        for p in 0..(CACHE_PARAGRAPHS as u32 * 3) {
            Layer::insert(&layer.shared, (1, 0, p, 0), 1, vec![Annotation::over(0..1, "a")]);
        }
        assert!(layer.cached() <= CACHE_PARAGRAPHS, "{}", layer.cached());
        // The most recent stay.
        assert!(layer.fresh((1, 0, CACHE_PARAGRAPHS as u32 * 3 - 1, 0), 1));
        assert!(!layer.fresh((1, 0, 0, 0), 1));
        // A new version is not fresh; a reset empties the cache.
        assert!(!layer.fresh((1, 0, CACHE_PARAGRAPHS as u32 * 3 - 1, 0), 2));
        let mut layer = layer;
        layer.reset();
        assert_eq!(layer.cached(), 0);
    }
}
