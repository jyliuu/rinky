//! Text for rinky: fonts, glyph caches, measuring and drawing ([`Fonts`]), shaping
//! (`rustybuzz`, [`Fonts::shape`]), fallback faces ([`face`], [`registry`]), glyph rasterizing
//! ([`raster`]), line breaking ([`linebreak`]) and hyphenation ([`hyphen`]).
//!
//! Two text paths share one [`Fonts`]:
//!
//! - the prototype path (`text_width`, `draw_text`): char by char with `fontdue` and pair
//!   kerning. Golden pages depend on its exact math; it never changes silently.
//! - the shaped path (`shape`, `draw_run`, `draw_run_clipped`): `rustybuzz` shaping (kerning,
//!   ligatures, marks), fallback faces for characters the main face lacks (CJK,
//!   symbols), synthetic bold and italic, and a bounded glyph cache.
//!
//! Owner from wave A: the `ui` agent (with `ink-ui`).

#![warn(missing_docs)]

pub mod face;
pub mod fonts;
pub mod hyphen;
pub mod linebreak;
pub mod raster;
pub mod registry;

pub use face::{FaceData, FaceInfo};
pub use fonts::{embedded_font_bytes, FaceId, Fonts, Glyph, ShapeOptions, TextRun};
pub use hyphen::{hyphenator_for, Hyphenator, Language, NoHyphenation, PatternHyphenator};
pub use raster::{GlyphMask, Synth};

/// Font style of a text run.
///
/// The embedded family has Regular, Bold and Italic faces; Bold Italic is synthesized
/// from the italic face (synthetic bold). Faces without a bold or italic variant get
/// synthetic bold and italic on the shaped path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Style {
    /// Upright, normal weight.
    #[default]
    Regular,
    /// Bold.
    Bold,
    /// Italic.
    Italic,
    /// Bold italic (phase 1).
    BoldItalic,
}

impl Style {
    /// Bold or bold italic.
    pub fn is_bold(self) -> bool {
        matches!(self, Style::Bold | Style::BoldItalic)
    }

    /// Italic or bold italic.
    pub fn is_italic(self) -> bool {
        matches!(self, Style::Italic | Style::BoldItalic)
    }
}

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum TextError {
    /// A font could not be read or parsed.
    #[error("font: {0}")]
    Font(String),
    /// A hyphenation dictionary could not be loaded.
    #[error("hyphenation: {0}")]
    Hyphenation(String),
}

/// Test fixture directories; `INK_TEST_DATA` and `INK_REPO_FONTS` override them so the
/// cross-built test binary can run on the Kindle (fixtures copied to the device).
#[cfg(test)]
pub(crate) mod testdirs {
    use std::path::{Path, PathBuf};

    /// `crates/ink-text/tests/data`.
    pub fn data() -> PathBuf {
        std::env::var_os("INK_TEST_DATA").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data"))
    }

    /// The repository's `fonts/`.
    pub fn repo_fonts() -> PathBuf {
        std::env::var_os("INK_REPO_FONTS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fonts"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_flags() {
        let flags = |s: Style| (s.is_bold(), s.is_italic());
        assert_eq!(flags(Style::Regular), (false, false));
        assert_eq!(flags(Style::Bold), (true, false));
        assert_eq!(flags(Style::Italic), (false, true));
        assert_eq!(flags(Style::BoldItalic), (true, true));
    }

    #[test]
    fn fonts_can_move_between_threads() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Fonts>();
        send_sync::<PatternHyphenator>();
    }
}
