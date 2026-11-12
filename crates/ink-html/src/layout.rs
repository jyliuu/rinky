//! Layout of one chapter into a vertical flow of slices (lines, images, table rows).
//!
//! - Styles: the cascade per element ([`compute_styles`]) plus the user's overrides.
//! - Blocks: margins (adjacent margins collapse; margins at a page top are dropped by
//!   the paginator), borders, padding, `width`/`max-width`, `text-indent`,
//!   `page-break-*`, lists with markers, tables with an auto column layout.
//! - Inline content: whitespace collapsing per `white-space`, `text-transform`, line
//!   breaking at UAX #14 opportunities, hyphenation by language, soft hyphens,
//!   justification, sub/superscript, inline images.
//! - Ruby: the text of `<rt>` drawn centred above its base at a reduced size (the `rt`
//!   font size, 50 % by default), and the annotations of the settings' annotator laid
//!   out the same way ([`Ruby`]). A base is never broken across lines. A ruby wider than
//!   its base reaches over the text beside it (overhang, as little off centre as keeps
//!   the bases' pitch) with a little more than a space to the next reading; readings
//!   that still do not fit are condensed, one size for the paragraph down to 70 %; only
//!   then do bases move apart, by one pitch for a run of annotated characters (one or
//!   two characters without a reading inside a run share it) ([`place_ruby`]). A paragraph with ruby gets the same
//!   extra space above every line. Ruby text is drawn only: it has no DOM position, so
//!   selection, search and page text never see it.
//! - Output: [`ChapterLayout`] with slices (the units the paginator places), draw items
//!   (text, images) per slice and border rectangles in flow coordinates.

use crate::book::Book;
use crate::dom::{Dom, ElemRef, NodeId, NodeKind, NONE};
use crate::fonts::{FaceKey, FontBook};
use crate::style::{
    ua_rules, Cascade, ComputedStyle, Display, Edges, Hyphens, Len, LineHeight, ListStyle, PageBreak, StyleContext, TextAlign, TextTransform,
    VAlign,
};
use ink_doc::{resolve_href, LayoutSettings};
use ink_text::linebreak::{break_opportunities, Break};
use ink_text::Hyphenator;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// No link.
pub const NO_LINK: u32 = u32::MAX;

/// A run of text drawn at one position.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextRun {
    /// Left edge (page pixels).
    pub x: f32,
    /// Baseline (flow coordinates).
    pub baseline: f32,
    /// Face.
    pub face: FaceKey,
    /// Size in px.
    pub px: f32,
    /// Text to draw (soft hyphens removed, `-` appended when the line breaks here).
    pub text: String,
    /// Width.
    pub width: f32,
    /// Text node.
    pub node: NodeId,
    /// First DOM char (Unicode scalar offset in the node).
    pub start: u32,
    /// DOM chars covered.
    pub len: u32,
    /// Link index in [`ChapterLayout::links`] or [`NO_LINK`].
    pub link: u32,
    /// Underline.
    pub underline: bool,
    /// Line-through.
    pub strike: bool,
    /// Gray level of the text (0 = black).
    pub gray: u8,
    /// Letter spacing.
    pub letter_spacing: f32,
    /// Width of underline / line-through (reaches the next decorated run on the line).
    pub deco_w: f32,
    /// Line top and bottom (flow coordinates), for selection rectangles.
    pub line_top: f32,
    /// Line bottom.
    pub line_bottom: f32,
    /// The line breaks inside a word here: the `-` that ends `text` was added by the
    /// layout (hyphenation or a soft hyphen), it is not in the book.
    pub hyphenated: bool,
}

/// An image drawn at a rectangle.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageRun {
    /// Left.
    pub x: f32,
    /// Top (flow coordinates).
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
    /// Resource path in the book, or `#svg:<node>` for an inline SVG element.
    pub src: String,
    /// The `img`/`image`/`svg` element.
    pub node: NodeId,
    /// Link index or [`NO_LINK`].
    pub link: u32,
}

/// A filled rectangle (borders, rules) in flow coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectRun {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
    /// Gray level.
    pub gray: u8,
}

/// A drawable item.
#[derive(Clone, Debug, PartialEq)]
pub enum Draw {
    /// Text.
    Text(TextRun),
    /// Image.
    Image(ImageRun),
    /// Rectangle (list markers drawn as squares, rules inside table rows).
    Rect(RectRun),
}

/// A unit the paginator places.
#[derive(Clone, Debug, PartialEq)]
pub struct Slice {
    /// Top of the content.
    pub top: f32,
    /// Bottom.
    pub bottom: f32,
    /// Position key (node order, char offset) of the first content.
    pub key: (u32, u32),
    /// A page break is forced before this slice.
    pub forced_break: bool,
    /// Avoid a page break before this slice (headings, widows/orphans, break-inside).
    pub keep_with_prev: bool,
    /// Draw items of this slice: `items[first_item..end_item]`.
    pub first_item: u32,
    /// End of the item range.
    pub end_item: u32,
}

/// A link target as written (`href`), resolved against the chapter path.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkRef {
    /// Book path + fragment (`OEBPS/notes.xhtml#n1`), or the external URL.
    pub target: String,
    /// True for http(s)/mailto and other external URLs.
    pub external: bool,
    /// The `<a>` element.
    pub node: NodeId,
}

/// A laid-out chapter.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChapterLayout {
    /// Slices in order.
    pub slices: Vec<Slice>,
    /// Draw items (slice ranges index this).
    pub items: Vec<Draw>,
    /// Borders and rules in flow coordinates.
    pub decor: Vec<RectRun>,
    /// Links.
    pub links: Vec<LinkRef>,
    /// Total height.
    pub height: f32,
}

/// What a chapter layout depends on.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// Page content height.
    pub content_h: f32,
    /// Left margin.
    pub left: f32,
    /// Content width.
    pub content_w: f32,
    /// Settings.
    pub settings: LayoutSettings,
    /// Whether the settings' annotator failed (shared by the clones of these params).
    pub(crate) annotator_state: AnnotatorState,
}

impl Params {
    /// From layout settings.
    pub fn new(s: &LayoutSettings) -> Self {
        let content_w = (s.width as f32 - s.margin_left - s.margin_right).max(50.0);
        let content_h = (s.height as f32 - s.margin_top - s.margin_bottom).max(50.0);
        Params { content_h, left: s.margin_left, content_w, settings: s.clone(), annotator_state: AnnotatorState::default() }
    }

    /// True once the annotator of these settings panicked: the layouts made with these
    /// params from then on have no annotations.
    pub fn annotator_failed(&self) -> bool {
        self.annotator_state.failed()
    }
}

/// Whether the annotator of one layout still runs. Every clone of one [`Params`] (the
/// foreground and the pagination threads) shares it; after a panic in the annotator it
/// is off for the rest of the book. Always equal: it is state, not a setting.
#[derive(Clone, Debug, Default)]
pub(crate) struct AnnotatorState(Arc<AtomicBool>);

impl PartialEq for AnnotatorState {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl AnnotatorState {
    fn failed(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Turns the annotator off; true the first time (the caller logs it once).
    fn fail(&self) -> bool {
        !self.0.swap(true, Ordering::Relaxed)
    }
}

/// Text drawn above a base: the text of an `<rt>` element, or an annotation of the
/// settings' annotator.
#[derive(Clone, Debug)]
struct Ruby {
    /// Byte range of the base in the paragraph (or chunk) text.
    start: usize,
    end: usize,
    /// The text drawn above.
    text: String,
    face: FaceKey,
    px: f32,
    gray: u8,
    /// Width of the base text and of the ruby text (measured by [`Flow::lay_lines`]).
    base_w: f32,
    ruby_w: f32,
    /// Space added before the base so its ruby text keeps [`RUBY_GAP`] from the ruby
    /// text before it, and the offset of the ruby text from the centre of its base (set
    /// by [`place_ruby`]).
    pad: f32,
    shift: f32,
}

/// Least space between two ruby texts side by side, in em of the ruby text: a little
/// more than a space, so two readings side by side never read as one word.
const RUBY_GAP: f32 = 0.35;
/// Smallest size ruby text is condensed to (share of its normal size) so it fits over
/// its bases without moving them apart.
const RUBY_MIN_SCALE: f32 = 0.7;

/// How far a ruby text may sit off the centre of its base (share of the base width) so
/// that its neighbours keep their pitch.
const RUBY_SHIFT: f32 = 0.25;

/// Places the ruby texts of a paragraph over their bases at `scale` of their size:
/// sets [`Ruby::pad`] (space before the base) and [`Ruby::shift`] (offset of the ruby
/// text from the centre of its base). Returns the space added in all.
///
/// Overhang: a ruby text may reach over the text beside it, as long as it keeps
/// [`RUBY_GAP`] from the ruby text before it. A run of bases with nothing between them
/// (one reading per character) keeps one pitch: every base of the run but the first gets
/// the same pad, the smallest that lets each ruby text sit within [`RUBY_SHIFT`] of its
/// base's centre without coming too close to the one before (the ruby texts sit as
/// close to their centres as their spacing allows, see `try_place`). The first base of a run moves only if the
/// ruby text before the run reaches too far. `before[i]` is the raw width of the text
/// between ruby `i - 1` and ruby `i`, and whether a forced line break lies in it (the
/// readings start afresh there). The edges of the lines the text is broken into are the
/// boxes' lead and trail.
fn place_ruby(ruby: &mut [Ruby], before: &[(f32, bool)], scale: f32) -> f32 {
    let n = ruby.len();
    let (mut x, mut reach, mut total) = (0.0f32, f32::NEG_INFINITY, 0.0f32);
    let mut i = 0;
    while i < n {
        if before[i].1 {
            reach = f32::NEG_INFINITY;
        }
        x += before[i].0;
        let mut j = i + 1;
        while j < n && before[j].0 == 0.0 && !before[j].1 {
            j += 1;
        }
        let run = &mut ruby[i..j];
        // Places the run with pad `u` before every base but the first; Some(reach after
        // it) when every ruby text fits within RUBY_SHIFT of its base's centre, the
        // shifts written into the run. The ruby texts are put as close to their centres
        // as their spacing allows (least squares: pool-adjacent-violators on the
        // positions less the spacing before them), then pushed right where the ruby
        // text before the run reaches too far. A base without a reading (an empty ruby
        // text) only keeps the pitch.
        let try_place = |run: &mut [Ruby], pad0: f32, u: f32| -> Option<f32> {
            // (index, ideal left edge, width, gap after) of the ruby texts.
            let mut items: Vec<(usize, f32, f32, f32)> = Vec::with_capacity(run.len());
            let mut left = x + pad0;
            for (k, r) in run.iter_mut().enumerate() {
                if k > 0 {
                    left += u;
                }
                r.shift = 0.0;
                if !r.text.is_empty() {
                    let rw = r.ruby_w * scale;
                    items.push((k, left + (r.base_w - rw) / 2.0, rw, RUBY_GAP * r.px * scale));
                }
                left += r.base_w;
            }
            // Pool adjacent violators on the targets less the spacing before each text.
            let mut blocks: Vec<(f32, usize)> = Vec::with_capacity(items.len());
            let mut spacing = 0.0;
            let mut offsets = Vec::with_capacity(items.len());
            for &(_, ideal, rw, gap) in &items {
                offsets.push(spacing);
                let mut b = (ideal - spacing, 1usize);
                while let Some(&(sum, n)) = blocks.last() {
                    if sum / n as f32 > b.0 / b.1 as f32 {
                        blocks.pop();
                        b = (b.0 + sum, b.1 + n);
                    } else {
                        break;
                    }
                }
                blocks.push(b);
                spacing += rw + gap;
            }
            let mut reach = reach;
            let mut k = 0;
            for (sum, n) in blocks {
                let q = sum / n as f32;
                for _ in 0..n {
                    let (ri, ideal, rw, gap) = items[k];
                    let at = (q + offsets[k]).max(reach);
                    let r = &mut run[ri];
                    if (at - ideal).abs() > RUBY_SHIFT * r.base_w + 0.01 {
                        return None;
                    }
                    r.shift = at - ideal;
                    reach = at + rw + gap;
                    k += 1;
                }
            }
            Some(reach)
        };
        let first = &run[0];
        let pad0 = (reach - (x + (first.base_w - first.ruby_w * scale) / 2.0 + RUBY_SHIFT * first.base_w)).max(0.0);
        let mut u = 0.0;
        if run.len() > 1 && try_place(run, pad0, 0.0).is_none() {
            let mut hi = run.iter().map(|r| r.ruby_w * scale + RUBY_GAP * r.px * scale).fold(0.0, f32::max) * 2.0;
            let mut lo = 0.0;
            for _ in 0..14 {
                let mid = (lo + hi) / 2.0;
                if try_place(run, pad0, mid).is_some() {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            u = hi;
        }
        reach = try_place(run, pad0, u).unwrap_or(reach);
        for (k, r) in run.iter_mut().enumerate() {
            r.pad = if k == 0 { pad0 } else { u };
        }
        let added = pad0 + u * (run.len() - 1) as f32;
        total += added;
        x += added + run.iter().map(|r| r.base_w).sum::<f32>();
        i = j;
    }
    total
}

/// One or two ideographs without a reading between two with readings get an empty ruby
/// each, so they share the even pitch of the run around them ([`place_ruby`]); an empty
/// ruby text draws nothing and blocks no other.
fn fill_short_gaps(text: &str, ruby: Vec<Ruby>) -> Vec<Ruby> {
    let ideograph = |c: char| matches!(c, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{3005}');
    let mut out: Vec<Ruby> = Vec::with_capacity(ruby.len());
    for r in ruby {
        if let Some(prev) = out.last() {
            let between = &text[prev.end..r.start];
            let n = between.chars().count();
            if (1..=2).contains(&n) && between.chars().all(ideograph) {
                let mut at = prev.end;
                for c in between.chars() {
                    let e = at + c.len_utf8();
                    out.push(Ruby { start: at, end: e, text: String::new(), ..r.clone() });
                    at = e;
                }
            }
        }
        out.push(r);
    }
    out
}

/// Annotation text size as a share of the base text's size.
const ANNOTATION_SCALE: f32 = 0.5;

/// Things layout needs from outside.
pub struct Env<'a> {
    /// Fonts of this thread.
    pub fonts: &'a mut FontBook,
    /// Image size by resolved path (`#svg:` sources are handled here).
    pub image_size: &'a mut dyn FnMut(&str) -> Option<(u32, u32)>,
}

/// Computed styles of every element of a chapter (`None` for text nodes and unreachable nodes).
pub fn compute_styles(book: &Book, ch: &crate::book::ChapterDom, p: &Params) -> Vec<Option<Box<ComputedStyle>>> {
    let s = &p.settings;
    let cascade = Cascade {
        ua: ua_rules(),
        author: ch.author.clone(),
        publisher: s.publisher_styles,
        ctx: StyleContext { font_px: s.font_px, page_width: p.content_w, page_height: p.content_h },
    };
    let dom = &ch.dom;
    let mut out: Vec<Option<Box<ComputedStyle>>> = vec![None; dom.nodes.len()];
    let mut root = ComputedStyle::root(s.font_px);
    let lang = dom.root_lang().map(str::to_string).or_else(|| book.meta.language.clone());
    root.lang = lang.map(|l| Arc::from(l.to_ascii_lowercase().as_str()));
    if dom.body == NONE {
        return out;
    }
    // (element, parent): a parent's style is computed before its children's.
    let mut stack = vec![(dom.body, NONE)];
    while let Some((id, parent)) = stack.pop() {
        let parent_style = if parent == NONE { &root } else { out[parent as usize].as_deref().expect("parent styled first") };
        let mut st = cascade.compute(ElemRef { dom, id }, parent_style);
        apply_user(&mut st, s, dom.name(id));
        stack.extend(dom.node(id).children.iter().rev().filter(|&&c| dom.element(c).is_some()).map(|&c| (c, id)));
        out[id as usize] = Some(Box::new(st));
    }
    out
}

/// The user's settings on top of the cascade: the font family, justification, and for
/// `<p>` the first-line indent (replaces the book's, except in centred and right-aligned
/// paragraphs) and extra space after the paragraph (added to the book's bottom margin).
fn apply_user(st: &mut ComputedStyle, s: &LayoutSettings, name: &str) {
    if name == "p" {
        let em = st.font_size;
        if let Some(ind) = s.paragraph_indent {
            if !matches!(st.text_align, TextAlign::Center | TextAlign::Right) {
                st.text_indent = Len::Px(ind.max(0.0) * em);
            }
        }
        if s.paragraph_spacing > 0.0 {
            let book = match st.margin.bottom {
                Len::Px(v) => v.max(0.0),
                _ => 0.0,
            };
            st.margin.bottom = Len::Px(book + s.paragraph_spacing * em);
        }
    }
    if let Some(f) = &s.font_family {
        if !st.is_monospace() {
            st.font_family = Arc::from(vec![f.clone()]);
        }
    }
    if !s.justify && st.text_align == TextAlign::Justify {
        st.text_align = TextAlign::Left;
    }
}

const SOFT_HYPHEN: char = '\u{ad}';
const OBJ: char = '\u{fffc}';

/// Per-character source info of a paragraph's display text.
#[derive(Clone, Copy, Debug)]
struct CharSrc {
    /// Segment index.
    seg: u32,
    /// DOM char offset in the segment's text node (images: 0).
    dom: u32,
}

/// [`CharSrc`] of every char of a paragraph, stored as runs of consecutive DOM offsets
/// (12 bytes per run instead of 8 per char: a giant paragraph of plain prose is a few
/// runs per text node).
#[derive(Default)]
struct SrcRuns {
    /// (segment, first DOM offset, chars).
    runs: Vec<(u32, u32, u32)>,
    n: usize,
}

impl SrcRuns {
    fn push(&mut self, c: CharSrc) {
        self.n += 1;
        if let Some(last) = self.runs.last_mut() {
            if last.0 == c.seg && last.1.checked_add(last.2) == Some(c.dom) {
                last.2 += 1;
                return;
            }
        }
        self.runs.push((c.seg, c.dom, 1));
    }

    fn pop(&mut self) {
        if let Some(last) = self.runs.last_mut() {
            self.n -= 1;
            last.2 -= 1;
            if last.2 == 0 {
                self.runs.pop();
            }
        }
    }

    fn truncate(&mut self, k: usize) {
        while self.n > k {
            self.pop();
        }
    }

    /// Every char's source, in order.
    fn iter(&self) -> impl Iterator<Item = CharSrc> + '_ {
        self.runs.iter().flat_map(|&(seg, dom, len)| (0..len).map(move |k| CharSrc { seg, dom: dom + k }))
    }
}

/// The display text of an inline run being collected, with the source of each char and
/// the segments the sources point into.
#[derive(Default)]
struct InlineText {
    text: String,
    src: SrcRuns,
    segs: Vec<Seg>,
    /// Ruby of the book's markup, in text order.
    ruby: Vec<Ruby>,
}

impl InlineText {
    /// Adds a segment; returns its index.
    fn add_seg(&mut self, seg: Seg) -> u32 {
        self.segs.push(seg);
        (self.segs.len() - 1) as u32
    }

    fn push(&mut self, c: char, seg: u32, dom: u32) {
        self.text.push(c);
        self.src.push(CharSrc { seg, dom });
    }

    fn pop(&mut self) {
        self.text.pop();
        self.src.pop();
    }
}

/// A style segment of a paragraph (one text node, image or line break).
#[derive(Clone, Debug)]
struct Seg {
    node: NodeId,
    style: NodeId,
    link: u32,
    kind: SegKind,
    /// Baseline shift from `vertical-align` of the inline ancestors (up = positive).
    shift: f32,
}

#[derive(Clone, Debug)]
enum SegKind {
    /// Text in face `face` at `px` (the style's size, smaller for the small capitals of
    /// `font-variant: small-caps`), resolved once per segment.
    Text { face: FaceKey, px: f32 },
    Image { src: String, w: f32, h: f32 },
    Break,
}

/// A piece of a line: same segment, consecutive chars.
#[derive(Clone, Debug)]
struct Piece {
    seg: u32,
    /// Byte range in the paragraph text.
    start: usize,
    end: usize,
    width: f32,
}

/// An unbreakable box: text between two break opportunities.
#[derive(Clone, Copy, Debug)]
struct WordBox {
    /// Byte range of the content (without trailing spaces).
    start: usize,
    end: usize,
    width: f32,
    space: f32,
    forced: bool,
    /// The line may end here with a hyphen (soft hyphen or dictionary split).
    hyphen: bool,
    /// Room the box needs before it when it starts a line, and after it when it ends
    /// one: its ruby text reaching over the line's edge (0 without ruby).
    lead: f32,
    trail: f32,
}

impl WordBox {
    /// The box cut at byte `cut` into a head `head` wide and the rest `rest` wide.
    fn split(&self, cut: usize, head: f32, hyphen: bool, rest: f32) -> (WordBox, WordBox) {
        (
            WordBox { start: self.start, end: cut, width: head, space: 0.0, forced: false, hyphen, lead: self.lead, trail: 0.0 },
            WordBox { start: cut, width: rest, lead: 0.0, ..*self },
        )
    }
}

/// A line of a paragraph: its boxes are `start..end` of the paragraph's line boxes.
struct Line {
    start: usize,
    end: usize,
    forced: bool,
    first: bool,
}

/// A paragraph (or one chunk of a giant paragraph) being broken into lines: its display
/// text, the source of each char, where its multi-byte chars are and its segments.
struct Para<'p> {
    text: &'p str,
    src: &'p [CharSrc],
    /// (char index, byte offset) just after each multi-byte char. Between two of them
    /// (and before the first) the text is ASCII, so char and byte indices advance
    /// together: plain ASCII text needs no table and no search.
    wide: Vec<(u32, u32)>,
    segs: &'p [Seg],
    /// Ruby bases in this text, sorted, not overlapping (usually none).
    ruby: Vec<Ruby>,
}

impl<'p> Para<'p> {
    fn new(text: &'p str, src: &'p [CharSrc], segs: &'p [Seg]) -> Self {
        let mut wide = Vec::new();
        if !text.is_ascii() {
            for (ci, (b, c)) in text.char_indices().enumerate() {
                if !c.is_ascii() {
                    wide.push((ci as u32 + 1, (b + c.len_utf8()) as u32));
                }
            }
        }
        Para { text, src, wide, segs, ruby: Vec::new() }
    }

    /// The index of the ruby whose base holds the bytes `start..end`.
    fn ruby_at(&self, start: usize, end: usize) -> Option<usize> {
        let i = self.ruby.partition_point(|r| r.end <= start);
        self.ruby.get(i).filter(|r| r.start <= start && end <= r.end).map(|_| i)
    }

    /// True when byte `b` is strictly inside a ruby base (no line break there).
    fn inside_ruby(&self, b: usize) -> bool {
        let i = self.ruby.partition_point(|r| r.end <= b);
        self.ruby.get(i).is_some_and(|r| r.start < b)
    }

    /// The first ruby base boundary strictly inside `start..end`.
    fn ruby_cut(&self, start: usize, end: usize) -> Option<usize> {
        let r = self.ruby[self.ruby.partition_point(|r| r.end <= start)..].first()?;
        if r.start > start {
            (r.start < end).then_some(r.start)
        } else {
            (r.end < end).then_some(r.end)
        }
    }

    /// Space added before the ruby bases wholly inside `start..end`.
    fn ruby_pads(&self, start: usize, end: usize) -> f32 {
        let i = self.ruby.partition_point(|r| r.start < start);
        self.ruby[i..].iter().take_while(|r| r.start < end).filter(|r| r.end <= end).map(|r| r.pad).sum()
    }

    /// Index of the char at byte `b` (or of the first char after it).
    fn char_index(&self, b: usize) -> usize {
        let b = b.min(self.text.len());
        // The last multi-byte char ending at or before `b`; ASCII from there on, up to
        // the next multi-byte char (a byte inside it counts that char as before `b`).
        let k = self.wide.partition_point(|&(_, wb)| wb as usize <= b);
        let (ci, wb) = if k == 0 { (0, 0) } else { (self.wide[k - 1].0 as usize, self.wide[k - 1].1 as usize) };
        let i = ci + (b - wb);
        match self.wide.get(k) {
            Some(&(next, _)) => i.min(next as usize),
            None => i,
        }
    }

    /// Byte offset of char `i` (the text length past the end).
    fn byte(&self, i: usize) -> usize {
        let k = self.wide.partition_point(|&(wi, _)| wi as usize <= i);
        let b = if k == 0 { i } else { self.wide[k - 1].1 as usize + (i - self.wide[k - 1].0 as usize) };
        b.min(self.text.len())
    }

    /// The segment of the char at byte `b`.
    fn seg_at(&self, b: usize) -> &'p Seg {
        &self.segs[self.src[self.char_index(b)].seg as usize]
    }

    /// The bytes `start..end` cut into runs of one segment: (segment, start, end).
    fn runs(&self, start: usize, end: usize) -> impl Iterator<Item = (u32, usize, usize)> + '_ {
        let stop = self.char_index(end);
        let mut i = self.char_index(start);
        std::iter::from_fn(move || {
            if i >= stop {
                return None;
            }
            let seg = self.src[i].seg;
            let mut j = i + 1;
            while j < stop && self.src[j].seg == seg {
                j += 1;
            }
            let run = (seg, self.byte(i), self.byte(j).min(end));
            i = j;
            Some(run)
        })
    }

    /// [`Para::runs`] cut also at ruby base boundaries, so a base is its own pieces.
    fn pieces(&self, start: usize, end: usize) -> impl Iterator<Item = (u32, usize, usize)> + '_ {
        let mut runs = self.runs(start, end);
        let mut rest: Option<(u32, usize, usize)> = None;
        std::iter::from_fn(move || {
            let (seg, s, e) = match rest.take() {
                Some(r) => r,
                None => runs.next()?,
            };
            if !self.ruby.is_empty() {
                if let Some(cut) = self.ruby_cut(s, e) {
                    rest = Some((seg, cut, e));
                    return Some((seg, s, cut));
                }
            }
            Some((seg, s, e))
        })
    }
}

/// Layout state of a chapter flow.
pub struct Flow<'a, 'e> {
    env: &'a mut Env<'e>,
    p: &'a Params,
    dom: &'a Dom,
    styles: &'a [Option<Box<ComputedStyle>>],
    chapter_dir: &'a str,
    chapter_path: &'a str,
    /// The book's language (when elements have none).
    book_lang: Option<String>,
    out: ChapterLayout,
    y: f32,
    margin: f32,
    neg_margin: f32,
    force_break: bool,
    keep_next: bool,
    first_line_pending: bool,
    /// The list marker the next line starts with, and whether it hangs outside.
    marker: Option<(String, bool)>,
    /// True while laying out a table cell (no page-break handling inside).
    in_cell: bool,
    /// Elements whose subtree contains block-level descendants (for inline elements).
    has_block_desc: Vec<bool>,
    /// Nesting depth of block containers and inline elements (bounded against
    /// pathological documents).
    depth: u32,
    /// Laying out a giant paragraph: runs of one line are merged and the text is not
    /// justified (see [`GIANT_PARAGRAPH`]).
    merge_runs: bool,
    /// Counter values of the list items of a list element (computed once per list).
    list_numbers: std::collections::HashMap<NodeId, std::collections::HashMap<NodeId, i64>>,
    /// Where a prefix layout stops ([`Flow::layout_until`]).
    stop: Option<StopAt>,
    /// The stop was reached: the open blocks unwind without laying out more children.
    stopped: bool,
    /// Lines make text draw items. False for a page-map layout ([`Flow::layout_slices`]),
    /// which needs only the slices: their geometry and keys do not depend on the items.
    text_items: bool,
    /// Height of the tallest ruby text of the paragraph being laid out (0: no ruby).
    ruby_h: f32,
}

/// Where [`Flow::layout_until`] stops: once the flow is `extra` px below the first slice
/// whose key is after `key`. The flow only stops between the children of a block (never
/// inside a paragraph or a table), so a chapter of one giant paragraph is laid out whole.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StopAt {
    /// Position key (node order, char offset) to lay out past.
    pub key: (u32, u32),
    /// Flow height to add below the first slice after `key`.
    pub extra: f32,
    /// Top of the first slice after `key`, once laid out.
    found: Option<f32>,
    /// Slices already looked at.
    scanned: usize,
}

impl StopAt {
    /// Stops `extra` px below the first slice after `key`.
    pub fn new(key: (u32, u32), extra: f32) -> Self {
        StopAt { key, extra, found: None, scanned: 0 }
    }
}

/// Paragraphs with more text than this (bytes) are broken into lines chunk by chunk
/// ([`PARAGRAPH_CHUNK`] bytes, each ending after a space, with a line end there), are
/// not justified and get one text run per line and style instead of one per word. A
/// 5 MB paragraph otherwise needed ~250 MB for its per-char and per-word vectors.
pub const GIANT_PARAGRAPH: usize = 256 << 10;
/// Chunk size for giant paragraphs.
pub const PARAGRAPH_CHUNK: usize = 64 << 10;
/// Words longer than this (bytes) are not hyphenated by dictionary.
const MAX_HYPHENATE_BYTES: usize = 128;
/// Boxes longer than this (bytes) are split with linear-time width bookkeeping.
const LONG_BOX_BYTES: usize = 512;

fn style_of<'s>(styles: &'s [Option<Box<ComputedStyle>>], dom: &Dom, mut id: NodeId) -> &'s ComputedStyle {
    loop {
        if let Some(Some(s)) = styles.get(id as usize) {
            return s;
        }
        let p = dom.node(id).parent;
        if p == NONE {
            break;
        }
        id = p;
    }
    styles[dom.body as usize].as_deref().expect("body style")
}

fn is_block_level(st: &ComputedStyle) -> bool {
    !matches!(st.display, Display::Inline | Display::InlineBlock | Display::None) || st.float != 0
}

/// Elements laid out as images.
fn is_image(name: &str) -> bool {
    matches!(name, "img" | "image" | "svg")
}

impl<'a, 'e> Flow<'a, 'e> {
    /// Lays out a chapter.
    pub fn layout(env: &'a mut Env<'e>, p: &'a Params, dom: &'a Dom, styles: &'a [Option<Box<ComputedStyle>>], chapter_path: &'a str, book_lang: Option<String>) -> ChapterLayout {
        Self::layout_until(env, p, dom, styles, chapter_path, book_lang, None).0
    }

    /// [`Flow::layout`] for the page map: the same slices and height, without the draw
    /// items of the lines (no text runs to measure again, build and drop).
    pub fn layout_slices(env: &'a mut Env<'e>, p: &'a Params, dom: &'a Dom, styles: &'a [Option<Box<ComputedStyle>>], chapter_path: &'a str, book_lang: Option<String>) -> ChapterLayout {
        Self::run(env, p, dom, styles, chapter_path, book_lang, None, false).0
    }

    /// [`Flow::layout`] that stops early at `stop`. Returns the layout and whether it is
    /// complete (the stop was not reached). A stopped layout holds every slice up to the
    /// stop, as the full layout has them, except that blocks still open at the stop end
    /// there (their borders reach the stop, `page-break-inside: avoid` of a block shorter
    /// than a page may still change its slices): pages ending more than a page above the
    /// stop are the full layout's pages ([`crate::paginate::prefix_pages`]).
    pub fn layout_until(
        env: &'a mut Env<'e>,
        p: &'a Params,
        dom: &'a Dom,
        styles: &'a [Option<Box<ComputedStyle>>],
        chapter_path: &'a str,
        book_lang: Option<String>,
        stop: Option<StopAt>,
    ) -> (ChapterLayout, bool) {
        Self::run(env, p, dom, styles, chapter_path, book_lang, stop, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn run(
        env: &'a mut Env<'e>,
        p: &'a Params,
        dom: &'a Dom,
        styles: &'a [Option<Box<ComputedStyle>>],
        chapter_path: &'a str,
        book_lang: Option<String>,
        stop: Option<StopAt>,
        text_items: bool,
    ) -> (ChapterLayout, bool) {
        let mut has_block_desc = vec![false; dom.nodes.len()];
        // Post-order: an element has block descendants if a child is block-level or has some.
        for &n in dom.preorder.iter().rev() {
            if let Some(Some(st)) = styles.get(n as usize) {
                let me = is_block_level(st);
                let parent = dom.node(n).parent;
                if parent != NONE && (me || has_block_desc[n as usize]) {
                    has_block_desc[parent as usize] = true;
                }
            }
        }
        let mut f = Flow {
            env,
            p,
            dom,
            styles,
            chapter_dir: crate::book::dir_of(chapter_path),
            chapter_path,
            book_lang,
            out: ChapterLayout::default(),
            y: 0.0,
            margin: 0.0,
            neg_margin: 0.0,
            force_break: false,
            keep_next: false,
            first_line_pending: true,
            marker: None,
            in_cell: false,
            has_block_desc,
            depth: 0,
            merge_runs: false,
            list_numbers: std::collections::HashMap::new(),
            stop,
            stopped: false,
            text_items,
            ruby_h: 0.0,
        };
        if dom.body != NONE {
            f.block(dom.body, p.left, p.content_w);
        }
        f.flush_margin();
        f.out.height = f.y;
        (f.out, !f.stopped)
    }

    fn st(&self, id: NodeId) -> &'a ComputedStyle {
        style_of(self.styles, self.dom, id)
    }

    fn add_margin(&mut self, m: f32) {
        if m >= 0.0 {
            self.margin = self.margin.max(m);
        } else {
            self.neg_margin = self.neg_margin.min(m);
        }
    }

    fn flush_margin(&mut self) {
        self.y += self.margin + self.neg_margin;
        self.margin = 0.0;
        self.neg_margin = 0.0;
    }

    fn push_slice(&mut self, top: f32, bottom: f32, key: (u32, u32), first_item: usize) {
        let forced = std::mem::take(&mut self.force_break);
        let keep = std::mem::take(&mut self.keep_next);
        self.out.slices.push(Slice {
            top,
            bottom,
            key,
            forced_break: forced && !self.in_cell,
            keep_with_prev: keep && !self.in_cell,
            first_item: first_item as u32,
            end_item: self.out.items.len() as u32,
        });
    }

    fn face(&mut self, st: &ComputedStyle) -> FaceKey {
        FaceKey { family: self.env.fonts.family(&st.font_family), bold: st.bold, italic: st.italic }
    }

    fn line_height(&self, st: &ComputedStyle) -> f32 {
        let ls = self.p.settings.line_spacing;
        let f = match st.line_height {
            LineHeight::Normal => ls,
            LineHeight::Factor(x) => (x * ls / 1.2).max(1.0),
            LineHeight::Px(px) => (px / st.font_size.max(1.0) * ls / 1.2).max(1.0),
        };
        st.font_size * f
    }

    /// A block-level element.
    fn block(&mut self, id: NodeId, x: f32, w: f32) {
        let st = self.st(id);
        if st.display == Display::None {
            return;
        }
        let first_slice = self.out.slices.len();
        if st.break_before == PageBreak::Always && !self.in_cell {
            self.force_break = true;
        }
        let Edges { top: mt, right: mr, bottom: mb, left: ml } = st.margin.resolve(w);
        let Edges { top: pt, right: pr, bottom: pb, left: pl } = st.padding.resolve(w);
        let b = st.border;
        let mut box_x = x + ml;
        let mut box_w = (w - ml - mr).max(10.0);
        let image = is_image(self.dom.name(id));
        let is_table = st.display == Display::Table;
        if !st.width.is_auto() && !image {
            let want = st.width.resolve(w, box_w) + if is_table { 0.0 } else { pl + pr + b.left + b.right };
            let want = want.min(box_w);
            if st.margin.left.is_auto() && st.margin.right.is_auto() {
                box_x += (box_w - want) / 2.0;
            } else if st.margin.left.is_auto() {
                box_x += box_w - want;
            }
            box_w = want;
        }
        // An `auto` maximum resolves to the box width: no change.
        let mw = st.max_width.resolve(w, box_w);
        if mw < box_w && !image {
            if st.margin.left.is_auto() && st.margin.right.is_auto() {
                box_x += (box_w - mw) / 2.0;
            }
            box_w = mw;
        }
        self.add_margin(mt);
        let framed_top = b.top > 0.0 || pt > 0.0;
        if framed_top {
            self.flush_margin();
        }
        let box_top = self.y;
        self.y += b.top + pt;
        let cx = box_x + b.left + pl;
        let cw = (box_w - b.left - b.right - pl - pr).max(10.0);
        let content_top = self.y;
        self.first_line_pending = true;
        if st.display == Display::ListItem {
            self.list_marker(id, st);
        }
        if image {
            self.block_image(id, cx, cw);
        } else if is_table {
            self.table(id, cx, cw);
        } else {
            self.block_children(id, cx, cw);
        }
        self.first_line_pending = false;
        if let Len::Px(h) = st.height {
            if !image && self.y + self.margin - content_top < h {
                self.flush_margin();
                self.y = self.y.max(content_top + h);
            }
        }
        let framed_bottom = b.bottom > 0.0 || pb > 0.0;
        if framed_bottom {
            self.flush_margin();
        }
        self.y += pb + b.bottom;
        let box_bottom = self.y;
        self.out.decor.extend(border_rects(b, box_x, box_w, box_top, box_bottom, box_bottom - box_top));
        self.add_margin(mb);
        if !self.in_cell {
            match st.break_after {
                PageBreak::Always => self.force_break = true,
                PageBreak::Avoid => self.keep_next = true,
                PageBreak::Auto => {}
            }
            if st.avoid_break_inside {
                let n = self.out.slices.len();
                let h: f32 = if n > first_slice { self.out.slices[n - 1].bottom - self.out.slices[first_slice].top } else { 0.0 };
                if h <= self.p.content_h {
                    for s in &mut self.out.slices[(first_slice + 1).min(n)..n] {
                        s.keep_with_prev = true;
                    }
                }
            }
        }
    }

    fn list_marker(&mut self, id: NodeId, st: &ComputedStyle) {
        if st.list_style == ListStyle::None {
            return;
        }
        // Counter: position among list-item siblings (ol start / li value).
        let parent = self.dom.node(id).parent;
        let mut n: i64 = 1;
        if parent != NONE {
            if !self.list_numbers.contains_key(&parent) {
                let mut reversed = false;
                if let Some(pe) = self.dom.element(parent) {
                    n = pe.attr("start").and_then(|s| s.trim().parse().ok()).unwrap_or(1);
                    reversed = pe.attr("reversed").is_some();
                }
                let items: Vec<NodeId> = self.dom.node(parent).children.iter().copied().filter(|&c| self.dom.element(c).is_some() && self.st(c).display == Display::ListItem).collect();
                if reversed {
                    n = n.saturating_add(items.len() as i64 - 1);
                }
                let mut numbers = std::collections::HashMap::new();
                for (k, &c) in items.iter().enumerate() {
                    if k > 0 {
                        n = if reversed { n.saturating_sub(1) } else { n.saturating_add(1) };
                    }
                    if let Some(v) = self.dom.element(c).and_then(|e| e.attr("value")).and_then(|v| v.trim().parse().ok()) {
                        n = v;
                    }
                    numbers.insert(c, n);
                }
                self.list_numbers.insert(parent, numbers);
            }
            n = self.list_numbers[&parent].get(&id).copied().unwrap_or(1);
        }
        let text = marker_text(st.list_style, n);
        self.marker = Some((text, !st.list_inside));
    }

    /// Children of a block container: inline runs become paragraphs.
    fn block_children(&mut self, id: NodeId, x: f32, w: f32) {
        let kids = &self.dom.node(id).children;
        if self.depth > 150 {
            // Deeper nesting is laid out as one paragraph.
            self.paragraph(id, kids, x, w);
            return;
        }
        self.depth += 1;
        self.block_children_inner(id, kids, x, w);
        self.depth -= 1;
    }

    /// True once the flow is past its stop (checked between block children, outside
    /// table cells, whose slices go to a separate layout).
    fn at_stop(&mut self) -> bool {
        if self.stopped {
            return true;
        }
        if self.in_cell {
            return false;
        }
        let Some(stop) = self.stop.as_mut() else { return false };
        if stop.found.is_none() {
            let slices = &self.out.slices;
            stop.found = slices[stop.scanned.min(slices.len())..].iter().find(|s| s.key > stop.key).map(|s| s.top);
            stop.scanned = slices.len();
        }
        self.stopped = stop.found.is_some_and(|top| self.y > top + stop.extra);
        self.stopped
    }

    fn block_children_inner(&mut self, id: NodeId, kids: &[NodeId], x: f32, w: f32) {
        let mut run: Vec<NodeId> = Vec::new();
        for &c in kids {
            if self.at_stop() {
                // The text gathered so far stays out too: a paragraph is never cut.
                return;
            }
            let block = match &self.dom.node(c).kind {
                NodeKind::Text(_) => false,
                NodeKind::Element(_) => {
                    let st = self.st(c);
                    if st.display == Display::None {
                        continue;
                    }
                    is_block_level(st) || (self.has_block_desc[c as usize] && !matches!(self.dom.name(c), "svg"))
                }
            };
            if block {
                if !run.is_empty() {
                    self.paragraph(id, &std::mem::take(&mut run), x, w);
                }
                let st = self.st(c);
                if is_block_level(st) {
                    self.block(c, x, w);
                } else {
                    // An inline element holding blocks: its content flows as blocks.
                    self.block_children(c, x, w);
                }
            } else {
                run.push(c);
            }
        }
        if !run.is_empty() {
            self.paragraph(id, &run, x, w);
        }
    }

    /// Size of an image element in device px for a containing width `w`.
    fn image_box(&mut self, id: NodeId, w: f32, max_h: f32) -> Option<(String, f32, f32)> {
        let e = self.dom.element(id)?;
        let st = self.st(id);
        let (src, iw, ih) = if e.name == "svg" {
            let src = format!("#svg:{id}");
            let (iw, ih) = svg_intrinsic(e, w / self.px_scale(), max_h / self.px_scale());
            (src, iw, ih)
        } else {
            let raw = e.attr("src").or_else(|| e.attr_local("href"))?;
            if raw.starts_with("data:") {
                return None;
            }
            let src = resolve_href(self.chapter_dir, raw);
            let (iw, ih) = (self.env.image_size)(&src)?;
            (src, iw as f32, ih as f32)
        };
        if iw <= 0.0 || ih <= 0.0 {
            return None;
        }
        let scale = self.px_scale();
        let aspect = ih / iw;
        let mut dw = iw * scale;
        let mut dh = ih * scale;
        let (dw0, dh0) = (st.width.resolve(w, dw), st.height.resolve(max_h, dh));
        // A size given on one axis only keeps the aspect ratio.
        match (st.width.is_auto(), st.height.is_auto()) {
            (false, false) => (dw, dh) = (dw0, dh0),
            (false, true) => (dw, dh) = (dw0, dw0 * aspect),
            (true, false) => (dw, dh) = (dh0 / aspect, dh0),
            (true, true) => {}
        }
        // `auto` maxima resolve to the available size.
        let max_w = st.max_width.resolve(w, w).min(w);
        if dw > max_w {
            dh *= max_w / dw;
            dw = max_w;
        }
        let max_hh = st.max_height.resolve(max_h, max_h).min(max_h);
        if dh > max_hh {
            dw *= max_hh / dh;
            dh = max_hh;
        }
        Some((src, dw.max(1.0), dh.max(1.0)))
    }

    fn px_scale(&self) -> f32 {
        self.p.settings.font_px / 16.0
    }

    fn block_image(&mut self, id: NodeId, x: f32, w: f32) {
        let max_h = self.p.content_h;
        let Some((src, dw, dh)) = self.image_box(id, w, max_h) else { return };
        self.flush_margin();
        let parent = self.dom.node(id).parent;
        let align = if parent != NONE { self.st(parent).text_align } else { TextAlign::Left };
        let st = self.st(id);
        let ix = if st.margin.left.is_auto() && st.margin.right.is_auto() || align == TextAlign::Center {
            x + (w - dw) / 2.0
        } else if align == TextAlign::Right || st.float > 0 {
            x + w - dw
        } else {
            x
        };
        let first = self.out.items.len();
        let top = self.y;
        let link = self.link_of(id);
        self.out.items.push(Draw::Image(ImageRun { x: ix, y: top, w: dw, h: dh, src, node: id, link }));
        self.y += dh;
        let key = (self.dom.node(id).order, 0);
        self.push_slice(top, self.y, key, first);
    }

    fn link_of(&mut self, id: NodeId) -> u32 {
        let mut n = id;
        while n != NONE {
            if let Some(e) = self.dom.element(n) {
                if e.name == "a" {
                    if let Some(h) = e.attr("href").or_else(|| e.attr_local("href")) {
                        return self.add_link(n, h);
                    }
                }
            }
            n = self.dom.node(n).parent;
        }
        NO_LINK
    }

    fn add_link(&mut self, node: NodeId, href: &str) -> u32 {
        if let Some(i) = self.out.links.iter().position(|l| l.node == node) {
            return i as u32;
        }
        let h = href.trim();
        let external = h.contains("://") || h.starts_with("mailto:") || h.starts_with("tel:");
        let target = if external {
            h.to_string()
        } else if let Some(frag) = h.strip_prefix('#') {
            format!("{}#{}", self.chapter_path, frag)
        } else {
            let frag = h.split_once('#').map(|(_, f)| f);
            let path = resolve_href(self.chapter_dir, h);
            match frag {
                Some(f) => format!("{path}#{f}"),
                None => path,
            }
        };
        self.out.links.push(LinkRef { target, external, node });
        (self.out.links.len() - 1) as u32
    }

    /// Appends the display text of text node `n`.
    fn push_text(&mut self, n: NodeId, out: &mut InlineText, link: u32, shift: f32) {
        let dom = self.dom;
        let Some(t) = dom.text(n) else { return };
        let parent = dom.node(n).parent;
        let st = self.st(parent);
        if st.hidden {
            return;
        }
        let face = self.face(st);
        let text_seg = |scale: f32| Seg { node: n, style: parent, link, kind: SegKind::Text { face, px: st.font_size * scale }, shift };
        let mut seg = out.add_seg(text_seg(1.0));
        let small_caps = st.small_caps;
        let mut small = false;
        let ws = st.white_space;
        let keep_spaces = ws.keeps_spaces();
        let keep_nl = ws.keeps_newlines();
        let tt = st.text_transform;
        let capitalize = tt == TextTransform::Capitalize;
        // The last char of the collected text, kept here instead of decoded again for
        // every char.
        let mut last = out.text.chars().last();
        let mut prev_alpha = last.is_some_and(|c| c.is_alphanumeric());
        out.text.reserve(t.len());
        for (i, c) in t.chars().enumerate() {
            let i = i as u32;
            if c == '\n' && keep_nl {
                out.push('\n', seg, i);
                last = Some('\n');
                prev_alpha = false;
                continue;
            }
            let c = if c == '\n' || c == '\t' || c == '\r' { ' ' } else { c };
            if c == ' ' && !keep_spaces && matches!(last, None | Some(' ') | Some('\n')) {
                continue;
            }
            let mut shown = match tt {
                TextTransform::Upper => c.to_uppercase().next().unwrap_or(c),
                TextTransform::Lower => c.to_lowercase().next().unwrap_or(c),
                TextTransform::Capitalize if !prev_alpha => c.to_uppercase().next().unwrap_or(c),
                _ => c,
            };
            if small_caps && shown != ' ' {
                // Lower-case letters become smaller capitals (a segment per case run).
                let want_small = shown.is_lowercase();
                if want_small != small {
                    small = want_small;
                    seg = out.add_seg(text_seg(if small { 0.78 } else { 1.0 }));
                }
                if small {
                    shown = shown.to_uppercase().next().unwrap_or(shown);
                }
            }
            if capitalize {
                prev_alpha = c.is_alphanumeric();
            }
            out.push(shown, seg, i);
            last = Some(shown);
        }
    }

    /// Collects the display text of inline nodes of block `owner` (containing width `w`).
    fn collect(&mut self, nodes: &[NodeId], owner: NodeId, out: &mut InlineText, link: u32, w: f32, shift: f32) {
        let dom = self.dom;
        for &n in nodes {
            match &dom.node(n).kind {
                NodeKind::Text(_) => self.push_text(n, out, link, shift),
                NodeKind::Element(e) => {
                    let st = self.st(n);
                    if st.display == Display::None {
                        continue;
                    }
                    let name = e.name.as_str();
                    if matches!(name, "rt" | "rp" | "script" | "style" | "head") {
                        continue;
                    }
                    if name == "br" {
                        let seg = out.add_seg(Seg { node: n, style: n, link, kind: SegKind::Break, shift });
                        // A trailing space before a forced break is dropped.
                        if out.text.ends_with(' ') && !self.st(owner).white_space.keeps_spaces() {
                            out.pop();
                        }
                        out.push('\n', seg, 0);
                        continue;
                    }
                    if is_image(name) {
                        if let Some((s, dw, dh)) = self.image_box(n, w, self.p.content_h) {
                            let l = if link == NO_LINK { self.link_of(n) } else { link };
                            let own = self.valign_shift(n);
                            let seg = out.add_seg(Seg { node: n, style: n, link: l, kind: SegKind::Image { src: s, w: dw, h: dh }, shift: shift + own });
                            out.push(OBJ, seg, 0);
                        }
                        continue;
                    }
                    let mut l = link;
                    if name == "a" {
                        if let Some(h) = e.attr("href").or_else(|| e.attr_local("href")) {
                            l = self.add_link(n, h);
                        }
                    }
                    let kids = &dom.node(n).children;
                    let sh = shift + self.valign_shift(n);
                    if name == "ruby" && self.p.settings.ruby && self.depth <= 400 {
                        self.depth += 1;
                        self.collect_ruby(kids, owner, out, l, w, sh);
                        self.depth -= 1;
                        continue;
                    }
                    if self.depth > 400 {
                        // Too deep: the rest of the subtree is plain text.
                        let mut stack: Vec<NodeId> = kids.iter().rev().copied().collect();
                        while let Some(k) = stack.pop() {
                            match &dom.node(k).kind {
                                NodeKind::Text(_) => self.push_text(k, out, l, sh),
                                NodeKind::Element(_) => stack.extend(dom.node(k).children.iter().rev()),
                            }
                        }
                        continue;
                    }
                    self.depth += 1;
                    self.collect(kids, owner, out, l, w, sh);
                    self.depth -= 1;
                }
            }
        }
    }

    /// Collects the children `kids` of a `<ruby>` element: the base text goes into the
    /// paragraph text, each `<rt>` becomes a [`Ruby`] over its base. Bases are `<rb>`
    /// elements or the content between two `<rt>`s; the `<rt>`s pair with the bases in
    /// order (`<rb>a</rb><rb>b</rb><rt>x</rt><rt>y</rt>` pairs a-x and b-y). `<rp>` is
    /// left out, and an `<rt>` that is `display: none` or hidden draws nothing.
    fn collect_ruby(&mut self, kids: &[NodeId], owner: NodeId, out: &mut InlineText, link: u32, w: f32, shift: f32) {
        let dom = self.dom;
        let mut bases: Vec<(usize, usize)> = Vec::new();
        let mut next = 0usize;
        let mut anon = out.text.len();
        // The base text since `from`, without the spaces around it.
        let base = |out: &InlineText, from: usize| -> Option<(usize, usize)> {
            let t = &out.text[from..];
            let lead = t.len() - t.trim_start_matches(' ').len();
            let s = from + lead;
            let e = from + t.trim_end_matches(' ').len();
            (e > s).then_some((s, e))
        };
        for &k in kids {
            match &dom.node(k).kind {
                NodeKind::Text(t) => {
                    // Spaces between the parts of a ruby are not text.
                    if !t.trim().is_empty() {
                        self.push_text(k, out, link, shift);
                    }
                }
                NodeKind::Element(e) => match e.name.as_str() {
                    "rp" | "rtc" => {}
                    "rt" => {
                        if let Some(b) = base(out, anon) {
                            bases.push(b);
                        }
                        anon = out.text.len();
                        let Some(&(bs, be)) = bases.get(next) else { continue };
                        next += 1;
                        let st = self.st(k);
                        if st.display == Display::None || st.hidden {
                            continue;
                        }
                        let text = self.dom.text_content(k).split_whitespace().collect::<Vec<_>>().join(" ");
                        if text.is_empty() {
                            continue;
                        }
                        let face = self.face(st);
                        out.ruby.push(Ruby { start: bs, end: be, text, face, px: st.font_size, gray: st.color, base_w: 0.0, ruby_w: 0.0, pad: 0.0, shift: 0.0 });
                    }
                    "rb" => {
                        if let Some(b) = base(out, anon) {
                            bases.push(b);
                        }
                        let from = out.text.len();
                        self.collect(&[k], owner, out, link, w, shift);
                        if let Some(b) = base(out, from) {
                            bases.push(b);
                        }
                        anon = out.text.len();
                    }
                    _ => self.collect(&[k], owner, out, link, w, shift),
                },
            }
        }
    }

    /// The annotations of the settings' annotator for the paragraph `text` (with the
    /// sources `src` of its chars), as ruby. Ranges that are empty, out of the text,
    /// inside a char, overlapping an earlier annotation or a ruby of the book (`book`,
    /// sorted) are dropped. A panic in the annotator turns it off for the rest of the
    /// book (logged once). Also returns whether the annotator wanted the paragraph.
    fn annotations(&mut self, owner: NodeId, text: &str, src: &SrcRuns, segs: &[Seg], book: &[Ruby]) -> (Vec<Ruby>, bool) {
        let s = &self.p.settings;
        let Some(handle) = s.annotator.as_ref().filter(|_| s.ruby) else { return (Vec::new(), false) };
        if self.p.annotator_state.failed() {
            return (Vec::new(), false);
        }
        let a = handle.annotator();
        let lang = self.lang_of(owner);
        let mut wanted = false;
        let called = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            wanted = a.wants(text);
            if wanted {
                a.annotate(text, lang.as_deref())
            } else {
                Vec::new()
            }
        }));
        let list = match called {
            Ok(list) => list,
            Err(e) => {
                if self.p.annotator_state.fail() {
                    let what = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_default();
                    log::error!("ink-html: annotator {} panicked ({what}); the rest of the book is laid out without annotations", handle.key());
                }
                return (Vec::new(), false);
            }
        };
        if list.is_empty() {
            return (Vec::new(), wanted);
        }
        // Taken ranges by start (the book's ruby, then each accepted annotation).
        let mut taken: std::collections::BTreeMap<usize, usize> = book.iter().map(|r| (r.start, r.end)).collect();
        let mut keep: Vec<(usize, usize, String)> = Vec::new();
        for an in list {
            let (st, en) = (an.range.start, an.range.end);
            if st >= en || en > text.len() || !text.is_char_boundary(st) || !text.is_char_boundary(en) || an.text.trim().is_empty() {
                continue;
            }
            if taken.range(..en).next_back().is_some_and(|(_, &e)| e > st) {
                continue;
            }
            taken.insert(st, en);
            keep.push((st, en, an.text.trim().to_string()));
        }
        keep.sort_by_key(|k| k.0);
        // The segment at each start (one pass over the chars).
        let mut out = Vec::with_capacity(keep.len());
        let mut k = 0;
        for ((b, _), cs) in text.char_indices().zip(src.iter()) {
            while k < keep.len() && keep[k].0 < b {
                k += 1;
            }
            while k < keep.len() && keep[k].0 == b {
                let seg = &segs[cs.seg as usize];
                if let SegKind::Text { face, px } = seg.kind {
                    let (st, en, ref t) = keep[k];
                    let gray = self.st(seg.style).color;
                    out.push(Ruby { start: st, end: en, text: t.clone(), face, px: px * ANNOTATION_SCALE, gray, base_w: 0.0, ruby_w: 0.0, pad: 0.0, shift: 0.0 });
                }
                k += 1;
            }
            if k >= keep.len() {
                break;
            }
        }
        (out, wanted)
    }

    /// Baseline shift of an inline element relative to its parent.
    fn valign_shift(&self, n: NodeId) -> f32 {
        let st = self.st(n);
        let parent = self.dom.node(n).parent;
        let pf = if parent != NONE { self.st(parent).font_size } else { st.font_size };
        match st.vertical_align {
            VAlign::Super => pf * 0.38,
            VAlign::Sub => -pf * 0.2,
            VAlign::Raise(r) => r,
            _ => 0.0,
        }
    }

    fn lang_of(&self, style_node: NodeId) -> Option<String> {
        self.st(style_node).lang.as_deref().map(str::to_string).or_else(|| self.book_lang.clone())
    }

    /// Face and size of a segment's text.
    fn seg_face(&mut self, seg: &Seg) -> (FaceKey, f32) {
        match seg.kind {
            SegKind::Text { face, px } => (face, px),
            _ => {
                let st = self.st(seg.style);
                (self.face(st), st.font_size)
            }
        }
    }

    /// Width of a text range of one segment.
    fn range_width(&mut self, text: &str, seg: &Seg) -> f32 {
        let (face, px) = self.seg_face(seg);
        let st = self.st(seg.style);
        // Most words have no soft hyphen (UTF-8 C2 AD) or newline: measure them in place.
        let dirty = text.as_bytes().iter().any(|&b| b == b'\n' || b == 0xAD);
        let owned: String;
        let clean: &str = if dirty {
            owned = text.chars().filter(|&c| c != SOFT_HYPHEN && c != '\n').collect();
            &owned
        } else {
            text
        };
        let mut wdt = self.env.fonts.width(face, px, clean);
        if st.letter_spacing != 0.0 {
            wdt += st.letter_spacing * clean.chars().count() as f32;
        }
        if st.word_spacing != 0.0 {
            wdt += st.word_spacing * clean.chars().filter(|&c| c == ' ').count() as f32;
        }
        wdt
    }

    /// Width of one run of [`Para::runs`].
    fn run_width(&mut self, p: &Para, (seg, start, end): (u32, usize, usize)) -> f32 {
        let seg = &p.segs[seg as usize];
        match &seg.kind {
            SegKind::Image { w, .. } => *w,
            SegKind::Break => 0.0,
            SegKind::Text { .. } => self.range_width(&p.text[start..end], seg),
        }
    }

    /// Width of the bytes `start..end` of a paragraph (with the space added before the
    /// ruby bases in it).
    fn width(&mut self, p: &Para, start: usize, end: usize) -> f32 {
        let w: f32 = p.runs(start, end).map(|r| self.run_width(p, r)).sum();
        if p.ruby.is_empty() {
            w
        } else {
            w + p.ruby_pads(start, end)
        }
    }

    /// Lays out an inline run as one paragraph owned by block `owner`.
    fn paragraph(&mut self, owner: NodeId, nodes: &[NodeId], x: f32, w: f32) {
        let mut out = InlineText::default();
        self.collect(nodes, owner, &mut out, NO_LINK, w, 0.0);
        // Trailing collapsible spaces go (leading ones were never collected).
        if !self.st(owner).white_space.keeps_spaces() {
            while out.text.ends_with(' ') {
                out.pop();
            }
        }
        let only_breaks = !out.text.is_empty() && out.text.chars().all(|c| c == '\n');
        if only_breaks {
            // `<p><br/></p>`: one empty line.
            out.text.truncate(1);
            out.src.truncate(1);
        } else if out.text.ends_with('\n') {
            // A final line break (or newline in pre text) makes no empty line.
            out.pop();
        }
        let InlineText { text, src, segs, mut ruby } = out;
        if text.is_empty() {
            return;
        }
        if only_breaks {
            ruby.clear();
        }
        // The text may have lost trailing spaces and breaks since the book's ruby was
        // collected.
        ruby.retain(|r| r.end <= text.len());
        let mut reserve = false;
        if self.p.settings.annotator.is_some() {
            let (more, wanted) = self.annotations(owner, &text, &src, &segs, &ruby);
            reserve = wanted;
            if !more.is_empty() {
                ruby.extend(more);
                ruby.sort_by_key(|r| r.start);
            }
        }
        self.ruby_h = 0.0;
        for r in &ruby {
            let (a, d) = self.env.fonts.vmetrics(r.face, r.px);
            self.ruby_h = self.ruby_h.max(a - d);
        }
        if reserve {
            // The annotator wanted this paragraph: its lines get the room of an
            // annotation even when it had none to give, so every paragraph it reads has
            // the same line pitch.
            let ost = self.st(owner);
            let face = self.face(ost);
            let (a, d) = self.env.fonts.vmetrics(face, ost.font_size * ANNOTATION_SCALE);
            self.ruby_h = self.ruby_h.max(a - d);
        }
        let para_first_slice = self.out.slices.len();
        if text.len() <= GIANT_PARAGRAPH {
            let mut chars = Vec::with_capacity(src.n);
            chars.extend(src.iter());
            let src = chars;
            self.lay_lines(owner, &text, &src, &segs, ruby, x, w);
        } else {
            let mut src_iter = src.iter();
            let merge = std::mem::replace(&mut self.merge_runs, true);
            let mut start = 0usize;
            while start < text.len() {
                let mut end = (start + PARAGRAPH_CHUNK).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                if end < text.len() {
                    // End the chunk after the last space or line break of its second half.
                    let mut lo = start + PARAGRAPH_CHUNK / 2;
                    while !text.is_char_boundary(lo) {
                        lo += 1;
                    }
                    if let Some(sp) = text[lo..end].rfind([' ', '\n']) {
                        end = lo + sp + 1;
                    }
                    // Never inside a ruby base.
                    if let Some(r) = ruby.iter().find(|r| r.start < end && end < r.end) {
                        end = if r.start > start { r.start } else { r.end };
                    }
                }
                let n = text[start..end].chars().count();
                let chunk_src: Vec<CharSrc> = src_iter.by_ref().take(n).collect();
                let chunk_ruby: Vec<Ruby> =
                    ruby.iter().filter(|r| r.start >= start && r.end <= end).map(|r| Ruby { start: r.start - start, end: r.end - start, ..r.clone() }).collect();
                self.lay_lines(owner, &text[start..end], &chunk_src, &segs, chunk_ruby, x, w);
                start = end;
            }
            self.merge_runs = merge;
        }
        self.ruby_h = 0.0;
        // Widows and orphans: keep two lines together at both ends.
        let n = self.out.slices.len() - para_first_slice;
        if n >= 2 && !self.in_cell {
            self.out.slices[para_first_slice + 1].keep_with_prev = true;
            self.out.slices[para_first_slice + n - 1].keep_with_prev = true;
        }
    }

    /// Breaks the display text of a paragraph (or of one chunk of a giant paragraph)
    /// into lines and places them.
    #[allow(clippy::too_many_arguments)]
    fn lay_lines(&mut self, owner: NodeId, text: &str, src: &[CharSrc], segs: &[Seg], ruby: Vec<Ruby>, x: f32, w: f32) {
        let ost = self.st(owner);
        let keep_spaces = ost.white_space.keeps_spaces();
        let mut para = Para::new(text, src, segs);
        if !ruby.is_empty() {
            let mut ruby = fill_short_gaps(text, ruby);
            for r in &mut ruby {
                r.base_w = self.width(&para, r.start, r.end);
                r.ruby_w = self.env.fonts.width(r.face, r.px, &r.text);
            }
            // The text before each base since the last one (raw widths), and whether a
            // forced line break lies in it.
            let mut before = Vec::with_capacity(ruby.len());
            let mut at = 0usize;
            for r in &ruby {
                before.push((self.width(&para, at, r.start), text[at..r.start].contains('\n')));
                at = r.end;
            }
            // When the readings do not fit over their bases (overhang below), they are
            // condensed first: one smaller size for every ruby text of the paragraph (a
            // uniform look), as large as lets them fit, at least RUBY_MIN_SCALE of the
            // normal size. Only what is still missing then moves bases apart.
            if place_ruby(&mut ruby, &before, 1.0) > 0.01 {
                let scale = if place_ruby(&mut ruby, &before, RUBY_MIN_SCALE) > 0.01 {
                    RUBY_MIN_SCALE
                } else {
                    let (mut lo, mut hi) = (RUBY_MIN_SCALE, 1.0f32);
                    for _ in 0..8 {
                        let mid = (lo + hi) / 2.0;
                        if place_ruby(&mut ruby, &before, mid) > 0.01 {
                            hi = mid;
                        } else {
                            lo = mid;
                        }
                    }
                    lo
                };
                for r in &mut ruby {
                    r.px *= scale;
                    r.ruby_w = self.env.fonts.width(r.face, r.px, &r.text);
                }
            }
            place_ruby(&mut ruby, &before, 1.0);
            para.ruby = ruby;
        }
        // Break opportunities.
        let wraps = ost.white_space.wraps() || keep_spaces;
        let mut ops: Vec<(usize, Break)> = break_opportunities(text);
        if !wraps {
            ops.retain(|(b, k)| *k == Break::Mandatory || *b == text.len());
        }
        // Allow breaks around images.
        if text.contains(OBJ) {
            for (ci, c) in text.char_indices() {
                if c == OBJ {
                    ops.push((ci, Break::Allowed));
                    ops.push((ci + c.len_utf8(), Break::Allowed));
                }
            }
        }
        ops.retain(|(b, _)| *b > 0);
        if !para.ruby.is_empty() {
            // No break inside a ruby base.
            ops.retain(|(b, k)| *k == Break::Mandatory || !para.inside_ruby(*b));
        }
        ops.sort_by_key(|o| (o.0, o.1 == Break::Allowed));
        ops.dedup_by_key(|o| o.0);
        // Boxes.
        let mut boxes: Vec<WordBox> = Vec::with_capacity(ops.len());
        let mut prev = 0usize;
        // The width of a space in the last segment one was measured in (most boxes of a
        // paragraph end with a space of the same segment).
        let mut space_of: Option<(usize, f32)> = None;
        for (b, kind) in ops {
            let seg_text = &text[prev..b];
            let forced = kind == Break::Mandatory && b < text.len() || seg_text.ends_with('\n');
            let trimmed = if keep_spaces { seg_text.trim_end_matches('\n') } else { seg_text.trim_end_matches([' ', '\n']) };
            let end = prev + trimmed.len();
            let width = self.width(&para, prev, end);
            let space = if end < b && !keep_spaces {
                let sp_seg = src[para.char_index(end).min(src.len() - 1)].seg as usize;
                let n_sp = text[end..b].chars().filter(|&c| c == ' ').count();
                if n_sp > 0 {
                    match space_of {
                        Some((s, sw)) if s == sp_seg => sw,
                        _ => {
                            let sw = self.range_width(" ", &segs[sp_seg]);
                            space_of = Some((sp_seg, sw));
                            sw
                        }
                    }
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let hyphen = trimmed.ends_with(SOFT_HYPHEN);
            boxes.push(WordBox { start: prev, end, width, space, forced, hyphen, lead: 0.0, trail: 0.0 });
            prev = b;
        }
        if !para.ruby.is_empty() {
            // How far the ruby text of each box reaches past its start and end: room the
            // box needs at the start and the end of a line.
            for b in &mut boxes {
                let first = para.ruby.partition_point(|r| r.start < b.start);
                for i in first..para.ruby.len() {
                    let (rs, re, pad, bw, rw) = { let r = &para.ruby[i]; (r.start, r.end, r.pad + r.shift, r.base_w, r.ruby_w) };
                    if rs >= b.end {
                        break;
                    }
                    if re > b.end {
                        continue;
                    }
                    let left = self.width(&para, b.start, rs) + pad;
                    b.lead = b.lead.max(-(left + (bw - rw) / 2.0));
                    b.trail = b.trail.max(left + (bw + rw) / 2.0 - b.width);
                }
            }
        }
        // Lines.
        let indent = if self.first_line_pending { ost.text_indent.resolve(w, 0.0) } else { 0.0 };
        self.first_line_pending = false;
        let hyph_ok = self.p.settings.hyphenate;
        let mut lines: Vec<Line> = Vec::new();
        // The boxes of every line, in order; the current line is `kept[cur..]`.
        let mut kept: Vec<WordBox> = Vec::with_capacity(boxes.len());
        let mut cur = 0usize;
        let mut cur_w = 0.0f32;
        let mut queue: std::collections::VecDeque<WordBox> = boxes.into();
        while let Some(bx) = queue.pop_front() {
            let first_line = lines.is_empty();
            let avail = if first_line { w - indent.max(0.0) } else { w };
            let empty = kept.len() == cur;
            let gap = if empty { 0.0 } else { kept[kept.len() - 1].space };
            let lead = if empty { bx.lead } else { 0.0 };
            let need = cur_w + gap + lead + bx.width;
            if wraps && empty && need + bx.trail > avail + 0.01 && bx.width > 0.0 {
                // Alone at the start of a line and still too wide (a long URL or word):
                // hyphenate or cut it.
                if let Some((a, b)) = self.split_box(&para, &bx, avail, hyph_ok).or_else(|| self.cut_box(&para, &bx, avail)) {
                    kept.push(a);
                    lines.push(Line { start: cur, end: kept.len(), forced: false, first: first_line });
                    cur = kept.len();
                    queue.push_front(b);
                    continue;
                }
            }
            if empty || need + bx.trail <= avail + 0.01 {
                cur_w = need;
                let forced = bx.forced;
                kept.push(bx);
                if forced {
                    lines.push(Line { start: cur, end: kept.len(), forced: true, first: first_line });
                    cur = kept.len();
                    cur_w = 0.0;
                }
                continue;
            }
            // Does not fit: try to hyphenate it into the remaining space.
            let remaining = avail - cur_w - gap;
            if let Some((a, b)) = self.split_box(&para, &bx, remaining, hyph_ok) {
                kept.push(a);
                lines.push(Line { start: cur, end: kept.len(), forced: false, first: first_line });
                cur = kept.len();
                cur_w = 0.0;
                queue.push_front(b);
                continue;
            }
            lines.push(Line { start: cur, end: kept.len(), forced: false, first: first_line });
            cur = kept.len();
            cur_w = 0.0;
            // On a line of its own next (cut there when still too wide).
            queue.push_front(bx);
        }
        if kept.len() > cur {
            let first = lines.is_empty();
            lines.push(Line { start: cur, end: kept.len(), forced: true, first });
        }
        let n_lines = lines.len();
        for (li, line) in lines.iter().enumerate() {
            let last = li + 1 == n_lines;
            self.place_line(owner, &para, line, &kept[line.start..line.end], x, w, indent, last);
        }
    }

    /// Splits a box at a hyphenation point (soft hyphen or dictionary) so the first part
    /// plus a hyphen fits `avail`.
    fn split_box(&mut self, p: &Para, bx: &WordBox, avail: f32, dict: bool) -> Option<(WordBox, WordBox)> {
        if avail <= 0.0 {
            return None;
        }
        let word = &p.text[bx.start..bx.end];
        let first_seg = p.seg_at(bx.start);
        if !matches!(first_seg.kind, SegKind::Text { .. }) {
            return None;
        }
        let st = self.st(first_seg.style);
        let mut points: Vec<usize> = word.char_indices().filter(|&(_, c)| c == SOFT_HYPHEN).map(|(i, c)| i + c.len_utf8()).filter(|&i| i < word.len()).collect();
        if dict && st.hyphens == Hyphens::Auto && points.is_empty() && word.len() <= MAX_HYPHENATE_BYTES {
            // The alphabetic core of the word (leading/trailing punctuation removed).
            let core_start = word.char_indices().find(|(_, c)| c.is_alphabetic()).map(|(i, _)| i).unwrap_or(word.len());
            let core_end = word.char_indices().rev().find(|(_, c)| c.is_alphabetic()).map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
            if core_end > core_start {
                let core = &word[core_start..core_end];
                if core.chars().all(|c| c.is_alphabetic() || c == '\'' || c == '\u{2019}') {
                    if let Some(h) = self.lang_of(first_seg.style).and_then(|l| crate::hyph::for_lang(&l)) {
                        points = h.hyphenate(core).into_iter().map(|p| core_start + p).collect();
                    }
                }
            }
        }
        if !p.ruby.is_empty() {
            points.retain(|&pt| !p.inside_ruby(bx.start + pt));
        }
        if points.is_empty() {
            return None;
        }
        let (face, px) = self.seg_face(first_seg);
        let hyph_w = self.env.fonts.width(face, px, "-");
        if word.len() > LONG_BOX_BYTES {
            // A very long box (soft hyphens in a giant word): widths added piece by piece
            // from the start, so the search is linear.
            let mut best = None;
            let (mut acc, mut prev) = (0.0f32, bx.start);
            for &pt in &points {
                let cut = bx.start + pt;
                acc += self.width(p, prev, cut);
                prev = cut;
                if acc + hyph_w > avail {
                    break;
                }
                best = Some((cut, acc));
            }
            let (cut, wdt) = best?;
            return Some(bx.split(cut, wdt + hyph_w, true, (bx.width - wdt).max(0.0)));
        }
        for &pt in points.iter().rev() {
            let cut = bx.start + pt;
            let wdt = self.width(p, bx.start, cut);
            if wdt + hyph_w <= avail {
                let rest = self.width(p, cut, bx.end);
                return Some(bx.split(cut, wdt + hyph_w, true, rest));
            }
        }
        None
    }

    /// Cuts a box that cannot fit a line at all (long URLs) at the last char that fits.
    fn cut_box(&mut self, p: &Para, bx: &WordBox, avail: f32) -> Option<(WordBox, WordBox)> {
        let mut cut = None;
        for (i, _) in p.text[bx.start..bx.end].char_indices().skip(1) {
            let wdt = self.width(p, bx.start, bx.start + i);
            if wdt > avail {
                break;
            }
            cut = Some((bx.start + i, wdt));
        }
        let (c, wdt) = cut?;
        // The rest's width: measured when short, else the box width minus the cut part
        // (measuring the rest of a giant word at every line was quadratic).
        let rest: f32 = if bx.end - c <= LONG_BOX_BYTES {
            self.width(p, c, bx.end)
        } else {
            (bx.width - wdt).max(0.0)
        };
        Some(bx.split(c, wdt, false, rest))
    }

    /// Positions one line, emits its draw items and its slice.
    #[allow(clippy::too_many_arguments)]
    fn place_line(&mut self, owner: NodeId, p: &Para, line: &Line, boxes: &[WordBox], x: f32, w: f32, indent: f32, last: bool) {
        let (text, src, segs) = (p.text, p.src, p.segs);
        let ost = self.st(owner);
        // Vertical metrics: the block's strut plus every piece.
        let strut_face = self.face(ost);
        let (sa, sd) = self.env.fonts.vmetrics(strut_face, ost.font_size);
        let lh = self.line_height(ost);
        let half = (lh - (sa - sd)) / 2.0;
        let mut asc = sa + half;
        let mut desc = lh - asc;
        // (piece, x offset, a hyphen is drawn after it)
        let n_boxes = boxes.len();
        let mut placed: Vec<(Piece, f32, bool)> = Vec::with_capacity(n_boxes);
        // A ruby text at the start of the line stays inside it.
        let mut pen = boxes.first().map_or(0.0, |b| b.lead);
        let mut spaces = 0usize;
        // The segment whose metrics are in `asc` and `desc` already (the same segment
        // gives the same metrics, so the next pieces of a run of words skip them).
        let mut measured_seg = None;
        for (bi, b) in boxes.iter().enumerate() {
            for run in p.pieces(b.start, b.end) {
                // Widths only place the pieces across the line.
                let width = if self.text_items { self.run_width(p, run) } else { 0.0 };
                if self.text_items && !p.ruby.is_empty() {
                    // The space before a base its ruby text needs (overhang in lay_lines).
                    if let Some(r) = p.ruby_at(run.1, run.2).map(|i| &p.ruby[i]).filter(|r| r.start == run.1) {
                        pen += r.pad;
                    }
                }
                let pc = Piece { seg: run.0, start: run.1, end: run.2, width };
                if measured_seg != Some(pc.seg) {
                    measured_seg = Some(pc.seg);
                    let seg = &segs[pc.seg as usize];
                    let st = self.st(seg.style);
                    let shift = seg.shift;
                    match seg.kind {
                        SegKind::Image { h, .. } => {
                            let a = image_ascent(st.vertical_align, h, sa);
                            let shift = if matches!(st.vertical_align, VAlign::Middle | VAlign::Top) { 0.0 } else { shift };
                            asc = asc.max(a + shift);
                            desc = desc.max(h - a - shift);
                        }
                        SegKind::Text { face, px } => {
                            let (a, d) = self.env.fonts.vmetrics(face, px);
                            let l = self.line_height(st);
                            let hl = (l - (a - d)) / 2.0;
                            if shift == 0.0 {
                                asc = asc.max(a + hl + shift);
                                desc = desc.max(l - a - hl - shift);
                            } else {
                                // Sub/superscripts do not grow the line beyond their glyphs.
                                asc = asc.max(a + shift);
                                desc = desc.max(-d - shift);
                            }
                        }
                        SegKind::Break => {}
                    }
                }
                let wdt = pc.width;
                placed.push((pc, pen, false));
                pen += wdt;
            }
            if b.hyphen && bi + 1 == n_boxes && !line.forced && self.text_items {
                if let Some(last_p) = placed.last_mut() {
                    last_p.2 = true;
                    let seg = &segs[last_p.0.seg as usize];
                    let (face, px) = self.seg_face(seg);
                    pen += self.env.fonts.width(face, px, "-");
                }
            }
            if bi + 1 < n_boxes {
                pen += b.space;
                if b.space > 0.0 {
                    spaces += 1;
                }
            }
        }
        if self.ruby_h > 0.0 {
            // Room for the ruby above every line of the paragraph: what the half-leading
            // does not give already.
            asc += (self.ruby_h - half).max(0.0);
        }
        // A ruby text at the end of the line stays inside it too.
        let line_w = pen + boxes.last().map_or(0.0, |b| b.trail);
        let first_indent = if line.first { indent } else { 0.0 };
        let avail = w - first_indent.max(0.0);
        let align = ost.text_align;
        let justify = align == TextAlign::Justify && !last && !line.forced && spaces > 0 && !self.merge_runs;
        let extra = (avail - line_w).max(0.0);
        let mut x0 = x + first_indent;
        match align {
            TextAlign::Center => x0 += extra / 2.0,
            TextAlign::Right => x0 += extra,
            _ => {}
        }
        let per_space = if justify { extra / spaces as f32 } else { 0.0 };
        // Recompute x offsets with justification: walk boxes again.
        self.flush_margin();
        let top = self.y;
        let baseline = top + asc;
        let bottom = top + asc + desc;
        let first_item = self.out.items.len();
        // Marker.
        if let Some((m, outside)) = self.marker.take() {
            let st = ost;
            let face = FaceKey { family: self.env.fonts.family(&st.font_family), bold: false, italic: false };
            let shape = m.chars().next().filter(|&c| m.chars().count() == 1 && !self.env.fonts.has_glyph(face, c));
            let mw = if shape.is_some() { st.font_size * 0.3 } else { self.env.fonts.width(face, st.font_size, &m) };
            let gap = st.font_size * 0.4;
            let mx = if outside { x - mw - gap } else { x0 };
            if let Some(c) = shape {
                // No glyph for ◦ or ▪: draw a square, hollow for circles.
                let sz = mw;
                let y = baseline - st.font_size * 0.32 - sz / 2.0;
                let t = (st.font_size / 16.0).max(1.0);
                if c == '▪' {
                    self.out.items.push(Draw::Rect(RectRun { x: mx, y, w: sz, h: sz, gray: 0 }));
                } else {
                    self.out.items.extend(border_rects(Edges::all(t), mx, sz, y, y + sz, sz).map(Draw::Rect));
                }
            } else {
                // Black, undecorated, outside the DOM text and links.
                let run = TextRun {
                    x: mx,
                    baseline,
                    face,
                    px: st.font_size,
                    text: m,
                    width: mw,
                    node: NONE,
                    link: NO_LINK,
                    deco_w: mw,
                    line_top: top,
                    line_bottom: bottom,
                    ..Default::default()
                };
                self.out.items.push(Draw::Text(run));
            }
            if !outside {
                x0 += mw + gap;
            }
        }
        // Box ends (all but the last box's) for justification: a box with a space after
        // it widens that space. Each box counts its own space (an empty box, such as the
        // space after an inline image, ends where the box before it ends).
        let mut gaps = boxes[..n_boxes.saturating_sub(1)].iter().map(|b| (b.end, b.space > 0.0)).peekable();
        let mut add = 0.0f32;
        let mut key: Option<(u32, u32)> = None;
        // (ruby, left edge of its base) of the ruby bases that start on this line.
        let mut ruby_cells: Vec<(usize, f32)> = Vec::new();
        let line_end = boxes.last().map_or(0, |b| b.end);
        for (pc, off, hyphen) in placed {
            if !self.text_items && key.is_some() {
                // A page-map layout needs only the key of the first piece.
                break;
            }
            // Extra space accumulated before this piece.
            while let Some((_, spaced)) = gaps.next_if(|&(e, _)| pc.start >= e) {
                if spaced {
                    add += per_space;
                }
            }
            let px_x = x0 + off + add;
            let seg = &segs[pc.seg as usize];
            let st = self.st(seg.style);
            let ci = p.char_index(pc.start);
            if !p.ruby.is_empty() && self.text_items {
                if let Some(i) = p.ruby_at(pc.start, pc.end) {
                    if p.ruby[i].start == pc.start && p.ruby[i].end <= line_end && !p.ruby[i].text.is_empty() {
                        ruby_cells.push((i, px_x));
                    }
                }
            }
            match &seg.kind {
                SegKind::Image { src, w, h } => {
                    let y = baseline - image_ascent(st.vertical_align, *h, sa) - seg.shift;
                    if self.text_items {
                        self.out.items.push(Draw::Image(ImageRun { x: px_x, y, w: *w, h: *h, src: src.clone(), node: seg.node, link: seg.link }));
                    }
                    key.get_or_insert((self.dom.node(seg.node).order, 0));
                }
                SegKind::Break => {
                    key.get_or_insert((self.dom.node(seg.node).order, 0));
                }
                &SegKind::Text { face, px } => {
                    if !self.text_items {
                        key.get_or_insert((self.dom.node(seg.node).order, src[ci].dom));
                        continue;
                    }
                    let raw = &text[pc.start..pc.end];
                    let mut shown = String::with_capacity(raw.len() + usize::from(hyphen));
                    if raw.contains([SOFT_HYPHEN, '\n']) {
                        shown.extend(raw.chars().filter(|&c| c != SOFT_HYPHEN && c != '\n'));
                    } else {
                        shown.push_str(raw);
                    }
                    if hyphen {
                        shown.push('-');
                    }
                    let dom_start = src[ci].dom;
                    let ce = p.char_index(pc.end);
                    let dom_end = if ce < src.len() && src[ce].seg == pc.seg { src[ce].dom } else { src[ce - 1].dom + 1 };
                    key.get_or_insert((self.dom.node(seg.node).order, dom_start));
                    let mut width = pc.width;
                    if hyphen {
                        width += self.env.fonts.width(face, px, "-");
                    }
                    if shown.trim().is_empty() && !st.white_space.keeps_spaces() {
                        continue;
                    }
                    if self.merge_runs && st.letter_spacing == 0.0 && st.word_spacing == 0.0 && self.out.items.len() > first_item {
                        // Giant paragraph: extend the previous run of this line (same text
                        // node and style) instead of adding one run per word.
                        if let Some(Draw::Text(prev)) = self.out.items.last_mut() {
                            if prev.node == seg.node
                                && prev.face == face
                                && prev.px == px
                                && prev.baseline == baseline - seg.shift
                                && prev.link == seg.link
                                && prev.gray == st.color
                                && prev.underline == st.underline
                                && prev.strike == st.line_through
                                && prev.letter_spacing == 0.0
                                && prev.start + prev.len <= dom_start
                                && !prev.text.ends_with('-')
                            {
                                if px_x > prev.x + prev.width + 0.01 {
                                    prev.text.push(' ');
                                }
                                prev.text.push_str(&shown);
                                prev.width = px_x + width - prev.x;
                                prev.deco_w = prev.width;
                                prev.len = dom_end.saturating_sub(prev.start);
                                prev.hyphenated = hyphen;
                                continue;
                            }
                        }
                    }
                    self.out.items.push(Draw::Text(TextRun {
                        x: px_x,
                        baseline: baseline - seg.shift,
                        face,
                        px,
                        text: shown,
                        width,
                        node: seg.node,
                        start: dom_start,
                        len: dom_end.saturating_sub(dom_start),
                        link: seg.link,
                        underline: st.underline,
                        strike: st.line_through,
                        gray: st.color,
                        letter_spacing: st.letter_spacing,
                        deco_w: width,
                        line_top: top,
                        line_bottom: bottom,
                        hyphenated: hyphen,
                    }));
                }
            }
        }
        // Underlines and strike-throughs continue over the spaces between runs.
        for a in first_item..self.out.items.len().saturating_sub(1) {
            if let [Draw::Text(ta), Draw::Text(tb)] = &mut self.out.items[a..a + 2] {
                if (ta.underline && tb.underline || ta.strike && tb.strike) && ta.link == tb.link && tb.x > ta.x {
                    ta.deco_w = tb.x - ta.x;
                }
            }
        }
        // Ruby text over its base (centred unless shifted by place_ruby), its bottom on the top of the base's
        // glyphs. Drawn only: no DOM node, so selection and page text skip it.
        for (i, left) in ruby_cells {
            let r = &p.ruby[i];
            let seg = p.seg_at(r.start);
            let (bface, bpx) = self.seg_face(seg);
            let (a, _) = self.env.fonts.vmetrics(bface, bpx);
            let (_, rd) = self.env.fonts.vmetrics(r.face, r.px);
            let x = left + (r.base_w - r.ruby_w) / 2.0 + r.shift;
            self.out.items.push(Draw::Text(TextRun {
                x,
                baseline: baseline - seg.shift - a + rd,
                face: r.face,
                px: r.px,
                text: r.text.clone(),
                width: r.ruby_w,
                node: NONE,
                link: NO_LINK,
                gray: r.gray,
                deco_w: r.ruby_w,
                line_top: top,
                line_bottom: bottom,
                ..Default::default()
            }));
        }
        self.y = bottom;
        let key = key.unwrap_or((self.dom.node(owner).order, 0));
        self.push_slice(top, bottom, key, first_item);
    }

    // -----------------------------------------------------------------------------------
    // Tables

    fn table(&mut self, id: NodeId, x: f32, w: f32) {
        // Caption(s) first, then rows in document order.
        let mut rows: Vec<NodeId> = Vec::new();
        let mut captions: Vec<NodeId> = Vec::new();
        let mut stack: Vec<NodeId> = self.dom.node(id).children.iter().rev().copied().collect();
        while let Some(c) = stack.pop() {
            if self.dom.element(c).is_none() {
                continue;
            }
            let st = self.st(c);
            match st.display {
                Display::TableRow => rows.push(c),
                Display::TableRowGroup => {
                    for &g in self.dom.node(c).children.iter().rev() {
                        stack.push(g);
                    }
                }
                Display::TableCaption => captions.push(c),
                Display::None | Display::TableColumn => {}
                _ => {
                    if self.dom.name(c) == "tr" {
                        rows.push(c);
                    }
                }
            }
        }
        for c in captions {
            self.block(c, x, w);
        }
        // Cells per row.
        let cells: Vec<Vec<(NodeId, usize)>> = rows
            .iter()
            .map(|&r| {
                self.dom
                    .node(r)
                    .children
                    .iter()
                    .copied()
                    .filter(|&c| self.dom.element(c).is_some() && self.st(c).display != Display::None)
                    .map(|c| {
                        let span = self.dom.element(c).and_then(|e| e.attr("colspan")).and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(1).clamp(1, 50);
                        (c, span)
                    })
                    .collect()
            })
            .collect();
        let ncols = cells.iter().map(|r| r.iter().map(|c| c.1).sum::<usize>()).max().unwrap_or(0);
        if ncols == 0 {
            return;
        }
        // Column min/max content widths.
        let mut cmin = vec![0.0f32; ncols];
        let mut cmax = vec![0.0f32; ncols];
        for row in &cells {
            let mut col = 0;
            for &(c, span) in row {
                let (mn, mx) = self.measure(c, w);
                let st = self.st(c);
                let extra = st.padding.left.resolve(w, 0.0) + st.padding.right.resolve(w, 0.0) + st.border.left + st.border.right;
                let (mn, mut mx) = (mn + extra, mx + extra);
                if let Len::Px(px) = st.width {
                    mx = mx.max(px);
                }
                if span == 1 && col < ncols {
                    cmin[col] = cmin[col].max(mn);
                    cmax[col] = cmax[col].max(mx);
                } else if col + span <= ncols {
                    // A spanning cell spreads what its columns lack evenly over them.
                    for (widths, need) in [(&mut cmin, mn), (&mut cmax, mx)] {
                        let cols = &mut widths[col..col + span];
                        let have: f32 = cols.iter().sum();
                        if need > have {
                            cols.iter_mut().for_each(|v| *v += (need - have) / span as f32);
                        }
                    }
                }
                col += span;
            }
        }
        for i in 0..ncols {
            cmax[i] = cmax[i].max(cmin[i]);
        }
        let tst = self.st(id);
        let full = matches!(tst.width, Len::Pct(p) if p >= 99.0);
        let sum_min: f32 = cmin.iter().sum();
        let sum_max: f32 = cmax.iter().sum();
        let widths: Vec<f32> = if sum_max <= w {
            if full && sum_max > 0.0 {
                cmax.iter().map(|m| m * w / sum_max).collect()
            } else {
                cmax.clone()
            }
        } else if sum_min >= w {
            cmin.iter().map(|m| m * w / sum_min.max(1.0)).collect()
        } else {
            let k = (w - sum_min) / (sum_max - sum_min).max(1.0);
            cmin.iter().zip(&cmax).map(|(a, b)| a + (b - a) * k).collect()
        };
        let table_w: f32 = widths.iter().sum();
        let tx = if tst.margin.left.is_auto() && tst.margin.right.is_auto() { x + (w - table_w) / 2.0 } else { x };
        // Rows.
        for (ri, row) in cells.iter().enumerate() {
            self.flush_margin();
            let row_top = self.y;
            let mut col = 0;
            let mut sub: Vec<(ChapterLayout, f32, f32, NodeId)> = Vec::new(); // (layout, x, width, cell)
            for &(c, span) in row {
                let cw: f32 = widths[col.min(ncols - 1)..(col + span).min(ncols)].iter().sum();
                let cx = tx + widths[..col.min(ncols)].iter().sum::<f32>();
                let lay = self.cell_layout(c, cx, cw);
                sub.push((lay, cx, cw, c));
                col += span;
            }
            let row_h = sub.iter().map(|s| s.0.height).fold(0.0, f32::max);
            let first_item = self.out.items.len();
            let key = sub
                .iter()
                .find_map(|s| s.0.slices.first().map(|sl| sl.key))
                .unwrap_or((self.dom.node(rows[ri]).order, 0));
            if row_h > self.p.content_h {
                // Too tall for a page: stack the cells' slices instead.
                for (lay, _, _, _) in sub {
                    let dy = self.y;
                    self.append_sub(lay, dy);
                }
                continue;
            }
            for (lay, cx, cw, c) in sub {
                self.append_drawn(lay, row_top);
                // Cell borders span the full row height.
                self.out.decor.extend(border_rects(self.st(c).border, cx, cw, row_top, row_top + row_h, row_h));
            }
            self.y = row_top + row_h;
            self.push_slice(row_top, self.y, key, first_item);
        }
    }

    /// Appends the draw items, decorations and links of a cell's layout moved down by
    /// `dy` (its link indexes rebased); returns its slices.
    fn append_drawn(&mut self, lay: ChapterLayout, dy: f32) -> Vec<Slice> {
        let link_base = self.out.links.len() as u32;
        for it in lay.items {
            let mut it = shift_item(it, dy);
            match &mut it {
                Draw::Text(t) if t.link != NO_LINK => t.link += link_base,
                Draw::Image(i) if i.link != NO_LINK => i.link += link_base,
                _ => {}
            }
            self.out.items.push(it);
        }
        self.out.links.extend(lay.links);
        self.out.decor.extend(lay.decor.into_iter().map(|d| RectRun { y: d.y + dy, ..d }));
        lay.slices
    }

    /// Appends a cell's layout as slices of this flow at `dy`.
    fn append_sub(&mut self, lay: ChapterLayout, dy: f32) {
        let item_base = self.out.items.len() as u32;
        let height = lay.height;
        for s in self.append_drawn(lay, dy) {
            self.out.slices.push(Slice {
                top: s.top + dy,
                bottom: s.bottom + dy,
                first_item: s.first_item + item_base,
                end_item: s.end_item + item_base,
                ..s
            });
        }
        self.y = dy + height;
    }

    /// Lays out a table cell in its own flow (y from 0).
    fn cell_layout(&mut self, cell: NodeId, x: f32, w: f32) -> ChapterLayout {
        let st = self.st(cell);
        let (pad, b) = (st.padding.resolve(w), st.border);
        let (pl, pr, pt, pb) = (pad.left + b.left, pad.right + b.right, pad.top + b.top, pad.bottom + b.bottom);
        // The flow state the cell's flow replaces, put back after it.
        let saved = (
            std::mem::take(&mut self.out),
            std::mem::replace(&mut self.y, pt),
            std::mem::take(&mut self.margin),
            std::mem::take(&mut self.neg_margin),
            self.force_break,
            self.keep_next,
            std::mem::replace(&mut self.first_line_pending, true),
            self.marker.take(),
            std::mem::replace(&mut self.in_cell, true),
        );
        self.block_children(cell, x + pl, (w - pl - pr).max(10.0));
        self.y += pb;
        let lay = ChapterLayout { height: self.y, ..std::mem::take(&mut self.out) };
        (self.out, self.y, self.margin, self.neg_margin, self.force_break, self.keep_next, self.first_line_pending, self.marker, self.in_cell) = saved;
        lay
    }

    /// (min, max) content width of a cell's content.
    fn measure(&mut self, cell: NodeId, w: f32) -> (f32, f32) {
        let mut min = 0.0f32;
        let mut max = 0.0f32;
        let mut line = 0.0f32;
        let dom = self.dom;
        let mut stack = vec![cell];
        while let Some(n) = stack.pop() {
            stack.extend(dom.node(n).children.iter().rev());
            match &dom.node(n).kind {
                NodeKind::Text(t) => {
                    let st = self.st(dom.node(n).parent);
                    if st.display == Display::None {
                        continue;
                    }
                    let face = self.face(st);
                    for word in t.split_whitespace() {
                        let ww = self.env.fonts.width(face, st.font_size, word);
                        min = min.max(ww);
                        line += ww + st.font_size * 0.25;
                    }
                }
                NodeKind::Element(e) => {
                    let st = self.st(n);
                    if is_image(&e.name) {
                        if let Some((_, iw, _)) = self.image_box(n, w, self.p.content_h) {
                            min = min.max(iw.min(w / 2.0));
                            line += iw;
                        }
                    } else if e.name == "br" || is_block_level(st) {
                        max = max.max(line);
                        line = 0.0;
                    }
                }
            }
        }
        max = max.max(line);
        (min.min(w), max)
    }
}

/// The rectangles of a box border from `top` to `bottom` (`h`: the height of the side
/// borders): top, bottom, left and right, leaving out sides of width 0.
fn border_rects(b: Edges<f32>, x: f32, w: f32, top: f32, bottom: f32, h: f32) -> impl Iterator<Item = RectRun> {
    let r = |x, y, w, h| RectRun { x, y, w, h, gray: 0 };
    let sides = [
        (b.top, r(x, top, w, b.top)),
        (b.bottom, r(x, bottom - b.bottom, w, b.bottom)),
        (b.left, r(x, top, b.left, h)),
        (b.right, r(x + w - b.right, top, b.right, h)),
    ];
    sides.into_iter().filter(|(width, _)| *width > 0.0).map(|(_, r)| r)
}

/// Height of an inline image above the baseline (`sa`: the line's strut ascent).
fn image_ascent(va: VAlign, h: f32, sa: f32) -> f32 {
    match va {
        VAlign::Middle => h / 2.0 + sa * 0.3,
        VAlign::Top => sa,
        _ => h,
    }
}

fn shift_item(it: Draw, dy: f32) -> Draw {
    match it {
        Draw::Text(t) => Draw::Text(TextRun { baseline: t.baseline + dy, line_top: t.line_top + dy, line_bottom: t.line_bottom + dy, ..t }),
        Draw::Image(i) => Draw::Image(ImageRun { y: i.y + dy, ..i }),
        Draw::Rect(r) => Draw::Rect(RectRun { y: r.y + dy, ..r }),
    }
}

/// Intrinsic size of an inline `<svg>` in CSS px (`avail_*` resolve percentages).
fn svg_intrinsic(e: &crate::dom::Element, avail_w: f32, avail_h: f32) -> (f32, f32) {
    let num = |v: Option<&str>, avail: f32| -> Option<f32> {
        let v = v?.trim();
        if let Some(p) = v.strip_suffix('%') {
            return p.trim().parse::<f32>().ok().map(|p| p * avail / 100.0);
        }
        v.trim_end_matches("px").trim().parse::<f32>().ok()
    };
    let vb: Option<(f32, f32)> = e.attr("viewBox").or_else(|| e.attr_local("viewbox")).and_then(|v| {
        let n: Vec<f32> = v.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect();
        (n.len() == 4 && n[2] > 0.0 && n[3] > 0.0).then(|| (n[2], n[3]))
    });
    let w = num(e.attr("width"), avail_w);
    let h = num(e.attr("height"), avail_h);
    match (w, h, vb) {
        (Some(w), Some(h), Some((vw, vh))) => {
            // Fit the viewBox aspect inside the given box.
            let s = (w / vw).min(h / vh);
            (vw * s, vh * s)
        }
        (Some(w), Some(h), None) => (w, h),
        (Some(w), None, Some((vw, vh))) => (w, w * vh / vw),
        (None, Some(h), Some((vw, vh))) => (h * vw / vh, h),
        (None, None, Some((vw, vh))) => {
            let s = (avail_w / vw).min(avail_h / vh);
            (vw * s, vh * s)
        }
        (w, h, None) => (w.unwrap_or(300.0), h.unwrap_or(150.0)),
    }
}

/// The text of a list marker.
pub fn marker_text(ls: ListStyle, n: i64) -> String {
    match ls {
        ListStyle::Disc => "•".into(),
        ListStyle::Circle => "◦".into(),
        ListStyle::Square => "▪".into(),
        ListStyle::None => String::new(),
        ListStyle::Decimal => format!("{n}."),
        ListStyle::DecimalLeadingZero => format!("{n:02}."),
        ListStyle::LowerAlpha => format!("{}.", alpha(n).to_lowercase()),
        ListStyle::UpperAlpha => format!("{}.", alpha(n)),
        ListStyle::LowerRoman => format!("{}.", roman(n).to_lowercase()),
        ListStyle::UpperRoman => format!("{}.", roman(n)),
        ListStyle::LowerGreek => {
            let g = "αβγδεζηθικλμνξοπρστυφχψω";
            let c = g.chars().nth(((n.max(1) - 1) % 24) as usize).unwrap_or('α');
            format!("{c}.")
        }
    }
}

fn alpha(mut n: i64) -> String {
    if n <= 0 {
        return n.to_string();
    }
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push((b'A' + (n % 26) as u8) as char);
        n /= 26;
    }
    s.iter().rev().collect()
}

fn roman(n: i64) -> String {
    if !(1..4000).contains(&n) {
        return n.to_string();
    }
    let table = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")];
    let mut n = n;
    let mut s = String::new();
    for (v, r) in table {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers() {
        assert_eq!(marker_text(ListStyle::Decimal, 3), "3.");
        assert_eq!(marker_text(ListStyle::UpperRoman, 14), "XIV.");
        assert_eq!(marker_text(ListStyle::LowerAlpha, 28), "ab.");
        assert_eq!(marker_text(ListStyle::Disc, 1), "•");
    }

    #[test]
    fn para_char_and_byte_indices_match_a_full_table() {
        let texts = ["", "plain words", "é", "café au lait", "“Quoted”—and ‘more’.", "Съешь же ещё", "a😀b\u{ad}c\u{fffc}", "😀😀x"];
        for text in texts {
            let para = Para::new(text, &[], &[]);
            let at: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
            for b in 0..=text.len() + 2 {
                assert_eq!(para.char_index(b), at.partition_point(|&x| x < b), "char_index({b}) of {text:?}");
            }
            for i in 0..=at.len() + 2 {
                assert_eq!(para.byte(i), at.get(i).copied().unwrap_or(text.len()), "byte({i}) of {text:?}");
            }
        }
    }
}
