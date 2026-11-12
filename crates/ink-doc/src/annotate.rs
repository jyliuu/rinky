//! Text annotations: short texts drawn above or below runs of text, like ruby.
//!
//! A [`TextAnnotator`] looks at the text of one paragraph and returns [`Annotation`]s:
//! byte ranges of that text, each with a short string to draw over it (readings) or
//! under it (glosses), see [`AnnotationPosition`]. A reflowable engine draws them the
//! way it draws `<ruby>` markup: the annotated text is the base and the annotation is
//! drawn above or below it at a reduced size.
//!
//! Annotations are drawing only. They never become document text: positions, selection,
//! search, page text and text-to-speech see the book's text exactly as without them.
//!
//! **Layout and drawing are separate.** Pagination asks only the cheap, stable part of
//! an annotator: [`TextAnnotator::wants`] for each paragraph, and what it declares up
//! front ([`TextAnnotator::positions`], [`TextAnnotator::ideograph_spacing`],
//! [`TextAnnotator::layout_version`]). A paragraph it wants gets the room of an
//! annotation on every line. [`TextAnnotator::annotate`] is called when pages are drawn,
//! in the background, for the page shown first and then the pages around it; a page
//! drawn before its annotations are ready is drawn again when they are. Annotations
//! never move the text: they fit into the room and pitch the layout reserved (centred,
//! overhanging their neighbours, condensed when needed).
//!
//! Several annotators can be active at once ([`crate::LayoutSettings::annotators`]); the
//! engine asks each in order and merges their annotations. Where two annotations of the
//! same position overlap, the earlier one wins; an annotation over a range and one under
//! it coexist.
//!
//! An annotator must be `Send + Sync` (the engine calls it from its own threads), cheap
//! in `wants`, and must not block. A panic in `wants` or `annotate` is caught by the
//! engine; it logs it once and goes on without that annotator's annotations (the others
//! go on).
//!
//! Two versions say what changed: [`TextAnnotator::layout_version`] (the answers of
//! `wants`, the positions or the spacing changed: the book is paginated again; part of
//! [`crate::LayoutSettings::cache_key`]) and [`TextAnnotator::version`] (only the output
//! of `annotate` changed: the annotations are computed again and the page is redrawn,
//! the pages stay). Output must be a function of the paragraph, the language and the
//! two versions.
//!
//! [`Annotation`] and [`AnnotationPosition`] are `#[non_exhaustive]`: build annotations
//! with [`Annotation::over`] and [`Annotation::under`], so fields and positions added later
//! do not break annotators.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

/// Where an annotation is drawn relative to the text it annotates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AnnotationPosition {
    /// Above the text (readings, like ruby; CSS `ruby-position: over`).
    #[default]
    Over,
    /// Below the text (glosses, translations; CSS `ruby-position: under`).
    Under,
}

/// One annotation: the text in `range` gets `text` drawn over or under it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Annotation {
    /// Byte range in the paragraph text passed to [`TextAnnotator::annotate`]. Both ends
    /// must be on `char` boundaries and the range must not be empty.
    pub range: Range<usize>,
    /// The annotation, drawn at `position` (for example a reading or a gloss).
    pub text: String,
    /// Above or below the range.
    pub position: AnnotationPosition,
}

impl Annotation {
    /// An annotation of `range` with `text`, drawn above it.
    pub fn over(range: Range<usize>, text: impl Into<String>) -> Self {
        Annotation { range, text: text.into(), position: AnnotationPosition::Over }
    }

    /// An annotation of `range` with `text`, drawn below it.
    pub fn under(range: Range<usize>, text: impl Into<String>) -> Self {
        Annotation { range, text: text.into(), position: AnnotationPosition::Under }
    }
}

/// Supplies annotations for paragraphs of reflowable text.
///
/// The engine passes the text of one paragraph at a time: its characters in reading
/// order as they are laid out (after whitespace collapsing), without the text of existing
/// `<rt>`/`<rp>` elements. Annotations it cannot use are dropped silently: out-of-range or
/// empty ranges, ends inside a character, ranges overlapping an earlier annotation of the
/// same position (from this annotator or an earlier one), and ranges that overlap a ruby
/// base of the book's own markup in the same position (the book's ruby wins). The engine
/// does not break a line inside an annotated range.
pub trait TextAnnotator: Send + Sync {
    /// A short stable name, for logs and the layout cache key (for example the plugin id).
    fn id(&self) -> &str;

    /// Changes whenever the output of [`TextAnnotator::annotate`] may change. The reader
    /// reads it before every frame; a different value has the annotations computed again
    /// and the page redrawn, without paginating the book again.
    fn version(&self) -> u64;

    /// Changes whenever the layout this annotator asks for may change: the answers of
    /// [`TextAnnotator::wants`], [`TextAnnotator::positions`] or
    /// [`TextAnnotator::ideograph_spacing`] (for example a mode that turns annotations
    /// on or off). Part of [`crate::LayoutSettings`] equality and cache key: a different
    /// value paginates the book again. The default, a constant, never does.
    fn layout_version(&self) -> u64 {
        0
    }

    /// A cheap check: true when this paragraph may get annotations. Called for every
    /// paragraph of the book while it is paginated, with the paragraph's language (as for
    /// [`TextAnnotator::annotate`]), so an annotator for one language skips the others at
    /// once. Every line of a paragraph it wants gets the room of an annotation in each of
    /// [`TextAnnotator::positions`], whether or not `annotate` then returns any, so the
    /// line pitch stays even and the layout never waits for `annotate`. Paragraphs it does
    /// not want are never passed to `annotate`.
    fn wants(&self, paragraph: &str, lang: Option<&str>) -> bool;

    /// The positions this annotator draws in; the layout reserves room in each for the
    /// paragraphs it wants. Annotations in other positions are not drawn. Default: over
    /// the text only.
    fn positions(&self) -> &[AnnotationPosition] {
        &[AnnotationPosition::Over]
    }

    /// Extra space between two ideographs (CJK characters) of a paragraph it wants, in
    /// em of the paragraph's text, so readings wider than their characters fit (for
    /// example 0.2 for readings in Latin letters over Chinese). Default 0: none.
    fn ideograph_spacing(&self, _lang: Option<&str>) -> f32 {
        0.0
    }

    /// The annotations of one paragraph it wants, in any order. `lang` is the language of
    /// the paragraph (`lang`/`xml:lang`, else the book's language), if known. Called when
    /// pages are drawn, on a background thread; may take a little time, but should not
    /// block.
    fn annotate(&self, paragraph: &str, lang: Option<&str>) -> Vec<Annotation>;
}

/// A [`TextAnnotator`] in [`crate::LayoutSettings`]: the shared annotator plus the
/// versions read when the handle was made. Two handles are equal when the id and the
/// layout version are, so settings built after a [`TextAnnotator::layout_version`] bump
/// compare unequal and the book is paginated again; a [`TextAnnotator::version`] bump
/// alone leaves them equal (the reader redraws instead).
#[derive(Clone)]
pub struct AnnotatorHandle {
    annotator: Arc<dyn TextAnnotator>,
    version: u64,
    layout_version: u64,
}

impl AnnotatorHandle {
    /// A handle on `annotator` at its current versions.
    pub fn new(annotator: Arc<dyn TextAnnotator>) -> Self {
        let (version, layout_version) = (annotator.version(), annotator.layout_version());
        AnnotatorHandle { annotator, version, layout_version }
    }

    /// The annotator.
    pub fn annotator(&self) -> &Arc<dyn TextAnnotator> {
        &self.annotator
    }

    /// The [`TextAnnotator::version`] read by [`AnnotatorHandle::new`].
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The [`TextAnnotator::layout_version`] read by [`AnnotatorHandle::new`].
    pub fn layout_version(&self) -> u64 {
        self.layout_version
    }

    /// `id@layout_version`, the part of the layout cache key.
    pub fn key(&self) -> String {
        format!("{}@{}", self.annotator.id(), self.layout_version)
    }
}

impl PartialEq for AnnotatorHandle {
    fn eq(&self, other: &Self) -> bool {
        self.layout_version == other.layout_version && self.annotator.id() == other.annotator.id()
    }
}

impl fmt::Debug for AnnotatorHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AnnotatorHandle({})", self.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixed(AtomicU64, AtomicU64);

    impl TextAnnotator for Fixed {
        fn id(&self) -> &str {
            "fixed"
        }
        fn version(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
        fn layout_version(&self) -> u64 {
            self.1.load(Ordering::Relaxed)
        }
        fn wants(&self, paragraph: &str, _lang: Option<&str>) -> bool {
            !paragraph.is_empty()
        }
        fn annotate(&self, paragraph: &str, _lang: Option<&str>) -> Vec<Annotation> {
            vec![Annotation::over(0..paragraph.len(), "x")]
        }
    }

    struct Plain;

    impl TextAnnotator for Plain {
        fn id(&self) -> &str {
            "plain"
        }
        fn version(&self) -> u64 {
            7
        }
        fn wants(&self, _: &str, _: Option<&str>) -> bool {
            true
        }
        fn annotate(&self, _: &str, _: Option<&str>) -> Vec<Annotation> {
            Vec::new()
        }
    }

    #[test]
    fn handles_compare_by_id_and_layout_version_at_creation() {
        let a = Arc::new(Fixed(AtomicU64::new(1), AtomicU64::new(1)));
        let before = AnnotatorHandle::new(a.clone());
        assert_eq!(before, AnnotatorHandle::new(a.clone()));
        // The output changed: the layout stays (equal handles, same key).
        a.0.store(2, Ordering::Relaxed);
        let redrawn = AnnotatorHandle::new(a.clone());
        assert_eq!(before, redrawn);
        assert_eq!((before.version(), redrawn.version()), (1, 2));
        // The layout changed: the book is paginated again.
        a.1.store(2, Ordering::Relaxed);
        let after = AnnotatorHandle::new(a);
        assert_ne!(before, after);
        assert_eq!(before.key(), "fixed@1");
        assert_eq!(after.key(), "fixed@2");
    }

    #[test]
    fn defaults_keep_an_annotator_without_layout_declarations_working() {
        let p = Plain;
        assert_eq!(p.layout_version(), 0);
        assert_eq!(p.positions(), &[AnnotationPosition::Over]);
        assert_eq!(p.ideograph_spacing(Some("zh")), 0.0);
        assert_eq!(AnnotatorHandle::new(Arc::new(Plain)).key(), "plain@0");
    }
}
