//! Text shaping API.
//!
//! A [`Shaper`] turns a run of text in one style into positioned glyphs.
//!
//! - [`SimpleShaper`]: one glyph per char, `fontdue` advances plus pair kerning: the same
//!   math as [`crate::Fonts::text_width`] (golden pages).
//! - [`RustybuzzShaper`]: `rustybuzz` through [`crate::Fonts::shape`]: OpenType kerning,
//!   ligatures, mark positioning, complex scripts and fallback faces. A ligature is one
//!   glyph whose `ch` is the first char of its cluster.
//!
//! For drawing shaped text with fallback faces use [`crate::Fonts::shape`] and
//! [`crate::Fonts::draw_run`] directly; [`ShapedRun`] keeps the phase 0 shape.

use crate::{Fonts, Style};

/// One positioned glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
    /// The character it came from (for the simple shaper; a cluster start otherwise).
    pub ch: char,
    /// Byte offset of the cluster in the input text.
    pub cluster: usize,
    /// Pen x offset of this glyph from the run start, in pixels.
    pub x: f32,
    /// Advance in pixels (kerning with the next glyph is included in the next `x`).
    pub advance: f32,
}

/// A shaped run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapedRun {
    /// Glyphs in visual order.
    pub glyphs: Vec<ShapedGlyph>,
    /// Total advance width.
    pub width: f32,
}

/// Turns text into positioned glyphs.
pub trait Shaper {
    /// Shapes `text` in `style` at `px` pixels per em.
    fn shape(&mut self, fonts: &mut Fonts, style: Style, px: f32, text: &str) -> ShapedRun;
}

/// One glyph per char with pair kerning (the prototype's measuring).
#[derive(Clone, Copy, Debug, Default)]
pub struct SimpleShaper;

impl Shaper for SimpleShaper {
    fn shape(&mut self, fonts: &mut Fonts, style: Style, px: f32, text: &str) -> ShapedRun {
        let mut run = ShapedRun::default();
        let mut pen = 0.0;
        let mut prev: Option<char> = None;
        for (cluster, ch) in text.char_indices() {
            if let Some(p) = prev {
                pen += fonts.font(style).horizontal_kern(p, ch, px).unwrap_or(0.0);
            }
            let advance = fonts.advance(style, ch, px);
            run.glyphs.push(ShapedGlyph { ch, cluster, x: pen, advance });
            pen += advance;
            prev = Some(ch);
        }
        run.width = pen;
        run
    }
}

/// OpenType shaping with `rustybuzz` (kerning, ligatures, marks, fallback faces).
#[derive(Clone, Copy, Debug, Default)]
pub struct RustybuzzShaper;

impl Shaper for RustybuzzShaper {
    fn shape(&mut self, fonts: &mut Fonts, style: Style, px: f32, text: &str) -> ShapedRun {
        let run = fonts.shape(style, px, text);
        let glyphs = run
            .glyphs
            .iter()
            .map(|g| ShapedGlyph {
                ch: text.get(g.cluster..).and_then(|s| s.chars().next()).unwrap_or('\u{fffd}'),
                cluster: g.cluster,
                x: g.x,
                advance: g.advance,
            })
            .collect();
        ShapedRun { glyphs, width: run.width }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_shaper_matches_text_width() {
        let mut fonts = Fonts::load();
        let run = SimpleShaper.shape(&mut fonts, Style::Regular, 37.0, "AVATAR fi");
        assert_eq!(run.glyphs.len(), 9);
        assert_eq!(run.glyphs[1].cluster, 1);
        let w = fonts.text_width(Style::Regular, "AVATAR fi", 37.0);
        assert!((run.width - w).abs() < 1e-3);
    }

    #[test]
    fn rustybuzz_shaper_makes_ligatures_and_kerns() {
        let mut fonts = Fonts::load();
        let run = RustybuzzShaper.shape(&mut fonts, Style::Regular, 37.0, "AVATAR fi");
        // "fi" is one ligature glyph: 9 chars, 8 glyphs.
        assert_eq!(run.glyphs.len(), 8, "{run:?}");
        let fi = run.glyphs.last().unwrap();
        assert_eq!((fi.ch, fi.cluster), ('f', 7));
        let simple = SimpleShaper.shape(&mut fonts, Style::Regular, 37.0, "AVATAR");
        let shaped = RustybuzzShaper.shape(&mut fonts, Style::Regular, 37.0, "AVATAR");
        // Both kern A-V, V-A, A-T, T-A; GPOS kerning is at least as tight as the kern table.
        assert!(shaped.width <= simple.width + 0.5, "{} vs {}", shaped.width, simple.width);
    }
}
