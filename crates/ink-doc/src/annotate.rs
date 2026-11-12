//! Text annotations: short texts drawn above or below runs of text, like ruby.
//!
//! A [`TextAnnotator`] looks at the text of one paragraph and returns [`Annotation`]s:
//! byte ranges of that text, each with a short string to draw over it (readings) or
//! under it (glosses), see [`AnnotationPosition`]. A reflowable engine lays them out the
//! way it lays out `<ruby>` markup: the annotated text is the ruby base and the
//! annotation is drawn above or below it at a reduced size.
//!
//! Annotations are drawing only. They never become document text: positions, selection,
//! search, page text and text-to-speech see the book's text exactly as without them.
//!
//! Several annotators can be active at once ([`crate::LayoutSettings::annotators`]); the
//! engine asks each in order and merges their annotations. Where two annotations of the
//! same position overlap, the earlier one wins; an annotation over a range and one under
//! it coexist.
//!
//! The engine calls an annotator from its pagination threads, for every paragraph of
//! the book, so an annotator must be `Send + Sync`, cheap when it has nothing to say
//! ([`TextAnnotator::wants`]), and must not block. A panic in [`TextAnnotator::wants`] or
//! [`TextAnnotator::annotate`] is caught by the engine; it logs it once and lays the rest
//! of the book out without that annotator's annotations (the others go on).
//!
//! The layout depends on each annotator's [`TextAnnotator::id`] and
//! [`TextAnnotator::version`] (they are part of [`crate::LayoutSettings::cache_key`]).
//! Output must be a function of the paragraph, the language and the version: an annotator
//! whose output changes (a different mode, a new word list) bumps its version, and the
//! reader then paginates the book again.
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
    /// reads it before every frame and when it builds its layout settings; a different
    /// value repaginates.
    fn version(&self) -> u64;

    /// A cheap check: false means `annotate` would return nothing for this paragraph, and
    /// the engine skips it. Called for every paragraph of the book, with the paragraph's
    /// language (as for [`TextAnnotator::annotate`]), so an annotator for one language
    /// skips the others at once. A paragraph it wants gets the room of an annotation on
    /// every line even when `annotate` returns nothing, so the line pitch stays even.
    fn wants(&self, paragraph: &str, lang: Option<&str>) -> bool;

    /// The annotations of one paragraph, in any order. `lang` is the language of the
    /// paragraph (`lang`/`xml:lang`, else the book's language), if known.
    fn annotate(&self, paragraph: &str, lang: Option<&str>) -> Vec<Annotation>;
}

/// A [`TextAnnotator`] in [`crate::LayoutSettings`]: the shared annotator plus the version
/// read when the handle was made. Two handles are equal when the id and that version are,
/// so settings built after a version bump compare unequal and the book is paginated again.
#[derive(Clone)]
pub struct AnnotatorHandle {
    annotator: Arc<dyn TextAnnotator>,
    version: u64,
}

impl AnnotatorHandle {
    /// A handle on `annotator` at its current version.
    pub fn new(annotator: Arc<dyn TextAnnotator>) -> Self {
        let version = annotator.version();
        AnnotatorHandle { annotator, version }
    }

    /// The annotator.
    pub fn annotator(&self) -> &Arc<dyn TextAnnotator> {
        &self.annotator
    }

    /// The version read by [`AnnotatorHandle::new`].
    pub fn version(&self) -> u64 {
        self.version
    }

    /// `id@version`, the part of the layout cache key.
    pub fn key(&self) -> String {
        format!("{}@{}", self.annotator.id(), self.version)
    }
}

impl PartialEq for AnnotatorHandle {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version && self.annotator.id() == other.annotator.id()
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

    struct Fixed(AtomicU64);

    impl TextAnnotator for Fixed {
        fn id(&self) -> &str {
            "fixed"
        }
        fn version(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
        fn wants(&self, paragraph: &str, _lang: Option<&str>) -> bool {
            !paragraph.is_empty()
        }
        fn annotate(&self, paragraph: &str, _lang: Option<&str>) -> Vec<Annotation> {
            vec![Annotation::over(0..paragraph.len(), "x")]
        }
    }

    #[test]
    fn handles_compare_by_id_and_version_at_creation() {
        let a = Arc::new(Fixed(AtomicU64::new(1)));
        let before = AnnotatorHandle::new(a.clone());
        assert_eq!(before, AnnotatorHandle::new(a.clone()));
        a.0.store(2, Ordering::Relaxed);
        let after = AnnotatorHandle::new(a);
        assert_ne!(before, after);
        assert_eq!(before.key(), "fixed@1");
        assert_eq!(after.key(), "fixed@2");
    }
}
