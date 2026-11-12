//! Text annotations: short readings drawn above runs of text, like ruby.
//!
//! A [`TextAnnotator`] looks at the text of one paragraph and returns [`Annotation`]s:
//! byte ranges of that text, each with a short string to draw above it. A reflowable
//! engine lays them out the way it lays out `<ruby>` markup: the annotated text is the
//! ruby base and the annotation is drawn above it at a reduced size.
//!
//! Annotations are drawing only. They never become document text: positions, selection,
//! search, page text and text-to-speech see the book's text exactly as without them.
//!
//! The engine calls an annotator from its pagination thread, for every paragraph of the
//! book, so an annotator must be `Send + Sync`, cheap when it has nothing to say
//! ([`TextAnnotator::wants`]), and must not block. A panic in [`TextAnnotator::wants`] or
//! [`TextAnnotator::annotate`] is caught by the engine; it logs it once and lays the rest
//! of the book out without annotations.
//!
//! The layout depends on the annotator's [`TextAnnotator::id`] and
//! [`TextAnnotator::version`] (they are part of [`crate::LayoutSettings::cache_key`]).
//! Output must be a function of the paragraph, the language and the version: an annotator
//! whose output changes (a different mode, a new word list) bumps its version, and the
//! reader then paginates the book again.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

/// One annotation: the text in `range` gets `text` drawn above it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    /// Byte range in the paragraph text passed to [`TextAnnotator::annotate`]. Both ends
    /// must be on `char` boundaries and the range must not be empty.
    pub range: Range<usize>,
    /// The annotation, drawn above the range (for example a reading).
    pub text: String,
}

impl Annotation {
    /// An annotation of `range` with `text`.
    pub fn new(range: Range<usize>, text: impl Into<String>) -> Self {
        Annotation { range, text: text.into() }
    }
}

/// Supplies annotations for paragraphs of reflowable text.
///
/// The engine passes the text of one paragraph at a time: its characters in reading
/// order as they are laid out (after whitespace collapsing), without the text of existing
/// `<rt>`/`<rp>` elements. Annotations it cannot use are dropped silently: out-of-range or
/// empty ranges, ends inside a character, ranges overlapping an earlier annotation, and
/// ranges that overlap a ruby base of the book's own markup (the book's ruby wins).
/// The engine does not break a line inside an annotated range.
pub trait TextAnnotator: Send + Sync {
    /// A short stable name, for logs and the layout cache key (for example the plugin id).
    fn id(&self) -> &str;

    /// Changes whenever the output of [`TextAnnotator::annotate`] may change. The reader
    /// reads it when it builds its layout settings; a different value repaginates.
    fn version(&self) -> u64;

    /// A cheap check: false means `annotate` would return nothing for this paragraph, and
    /// the engine skips it. Called for every paragraph of the book.
    fn wants(&self, paragraph: &str) -> bool;

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
        fn wants(&self, paragraph: &str) -> bool {
            !paragraph.is_empty()
        }
        fn annotate(&self, paragraph: &str, _lang: Option<&str>) -> Vec<Annotation> {
            vec![Annotation::new(0..paragraph.len(), "x")]
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
