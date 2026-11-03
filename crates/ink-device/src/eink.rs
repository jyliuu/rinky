//! A simple e-ink look for the host simulator and headless mode.
//!
//! The panel keeps its own pixels. Each update quantizes the new content to the levels
//! the waveform can show (16 grays, or 2 for DU) and, when ghosting is on, leaves a
//! fraction of the previous image behind on non-flashing updates, so missing GC16
//! flashes and wrong refresh classes are visible on the host.

use crate::canvas::Canvas;
use crate::geom::{Rect, Size};
use crate::screen::RefreshClass;

/// Quantizes a gray value to 16 levels (0, 17, ..., 255).
pub fn quantize16(v: u8) -> u8 {
    ((v as u16 * 15 + 127) / 255 * 17) as u8
}

/// Quantizes to black or white (DU waveform).
pub fn quantize2(v: u8) -> u8 {
    if v < 128 {
        0
    } else {
        255
    }
}

/// Simulated panel state.
#[derive(Clone, Debug)]
pub struct EinkPanel {
    /// What the panel shows, in panel orientation.
    pub pixels: Canvas,
    /// Leave a trace of the old image on non-flashing updates.
    pub ghosting: bool,
    /// Number of updates applied.
    pub updates: u64,
}

impl EinkPanel {
    /// A white panel.
    pub fn new(size: Size, ghosting: bool) -> Self {
        EinkPanel { pixels: Canvas::new(size.w as usize, size.h as usize), ghosting, updates: 0 }
    }

    /// Applies `src` (panel orientation, same size as the panel) inside `rect`.
    pub fn apply(&mut self, src: &Canvas, rect: Rect, class: RefreshClass) {
        let rect = rect.intersect(&self.pixels.bounds());
        // Fraction (out of 16) of the old image left behind.
        let ghost: u32 = match (self.ghosting, class) {
            (false, _) | (true, RefreshClass::Gc16) => 0,
            (true, RefreshClass::Reagl) => 1,
            (true, RefreshClass::Gl16) => 2,
            (true, RefreshClass::Du) => 3,
        };
        let w = self.pixels.width;
        for y in rect.y as usize..rect.bottom() as usize {
            for x in rect.x as usize..rect.right() as usize {
                let i = y * w + x;
                let new = src.pixels[i];
                let q = if class == RefreshClass::Du { quantize2(new) } else { quantize16(new) };
                let old = self.pixels.pixels[i] as u32;
                self.pixels.pixels[i] = if ghost > 0 && old != q as u32 {
                    ((q as u32 * (16 - ghost) + old * ghost) / 16) as u8
                } else {
                    q
                };
            }
        }
        self.updates += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantization_levels() {
        assert_eq!(quantize16(0), 0);
        assert_eq!(quantize16(255), 255);
        assert_eq!(quantize16(128), 136);
        let levels: std::collections::BTreeSet<u8> = (0..=255).map(quantize16).collect();
        assert_eq!(levels.len(), 16);
        assert_eq!(quantize2(127), 0);
    }

    #[test]
    fn ghosting_only_without_flash() {
        let size = Size::new(2, 1);
        let mut black = Canvas::new(2, 1);
        black.clear(0);
        let white = Canvas::new(2, 1);
        let mut p = EinkPanel::new(size, true);
        p.apply(&black, Rect::from_size(size), RefreshClass::Gc16);
        p.apply(&white, Rect::from_size(size), RefreshClass::Reagl);
        assert!(p.pixels.pixels[0] < 255, "REAGL leaves a ghost");
        p.apply(&white, Rect::from_size(size), RefreshClass::Gc16);
        assert_eq!(p.pixels.pixels[0], 255, "GC16 clears it");
        let mut clean = EinkPanel::new(size, false);
        clean.apply(&black, Rect::from_size(size), RefreshClass::Reagl);
        assert_eq!(clean.pixels.pixels, vec![0, 0]);
    }
}
