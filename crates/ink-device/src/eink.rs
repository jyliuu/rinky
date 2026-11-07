//! A simple e-ink look for the host simulator and headless mode.
//!
//! The panel keeps its own pixels. Each update quantizes the new content to the levels
//! the waveform can show (16 grays, or 2 for DU) and, when ghosting is on, leaves a
//! fraction of the previous image behind on non-flashing updates, so missing GC16
//! flashes and wrong refresh classes are visible on the host.

use crate::canvas::Canvas;
use crate::geom::{Rect, Rotation, Size};
use crate::screen::{copy_to_panel, RefreshClass};

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

/// Simulated panel: the framebuffer the app writes and what the panel shows.
#[derive(Clone, Debug)]
pub struct EinkPanel {
    /// What the panel shows, in panel orientation.
    pub pixels: Canvas,
    /// The framebuffer: what the app wrote, rotated and inverted, refreshed or not.
    pub fb: Canvas,
    /// Screen rotation.
    pub rotation: Rotation,
    /// Night mode (the framebuffer holds inverted pixels).
    pub night: bool,
    /// Leave a trace of the old image on non-flashing updates.
    pub ghosting: bool,
    /// Number of updates applied.
    pub updates: u64,
}

impl EinkPanel {
    /// A white panel.
    pub fn new(size: Size, ghosting: bool) -> Self {
        let white = Canvas::new(size.w as usize, size.h as usize);
        EinkPanel { pixels: white.clone(), fb: white, rotation: Rotation::Portrait, night: false, ghosting, updates: 0 }
    }

    /// Native panel size.
    pub fn size(&self) -> Size {
        self.fb.size()
    }

    /// [`crate::Screen::update`]: writes `rect` (logical) of `canvas` into the
    /// framebuffer and refreshes what changed; returns that panel rectangle.
    pub fn update(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass) -> Rect {
        let changed = self.write(canvas, rect);
        self.refresh(changed, class);
        changed
    }

    /// Writes `rect` (logical) of `canvas` into the framebuffer only; returns the panel
    /// rectangle written.
    pub fn write(&mut self, canvas: &Canvas, rect: Rect) -> Rect {
        let stride = self.fb.width;
        copy_to_panel(canvas, rect, self.rotation, self.night, self.size(), &mut self.fb.pixels, stride)
    }

    /// Shows the framebuffer inside panel rectangle `rect` with waveform `class`.
    pub fn refresh(&mut self, rect: Rect, class: RefreshClass) {
        let rect = rect.intersect(&self.pixels.bounds());
        // Fraction (out of 16) of the old image left behind.
        let ghost: u32 = match (self.ghosting, class) {
            (false, _) | (true, RefreshClass::Gc16) => 0,
            (true, RefreshClass::Reagl) => 1,
            (true, RefreshClass::Gl16) => 2,
            (true, RefreshClass::Du) => 3,
        };
        let levels: [u8; 256] = std::array::from_fn(|v| if class == RefreshClass::Du { quantize2(v as u8) } else { quantize16(v as u8) });
        let (w, n) = (self.pixels.width, rect.w as usize);
        for y in rect.y as usize..rect.bottom() as usize {
            let i = y * w + rect.x as usize;
            for (old, &new) in self.pixels.pixels[i..i + n].iter_mut().zip(&self.fb.pixels[i..i + n]) {
                let q = levels[new as usize];
                *old = if ghost > 0 && *old != q { ((q as u32 * (16 - ghost) + *old as u32 * ghost) / 16) as u8 } else { q };
            }
        }
        self.updates += 1;
    }
}

/// The [`crate::Screen`] methods of a screen that is an [`EinkPanel`] at
/// `self.0.borrow().panel` (the headless and simulator screens): size, rotation, night
/// mode and screenshot.
macro_rules! panel_screen_state {
    () => {
        fn panel_size(&self) -> Size {
            self.0.borrow().panel.size()
        }

        fn rotation(&self) -> Rotation {
            self.0.borrow().panel.rotation
        }

        fn set_rotation(&mut self, rotation: Rotation) -> Result<()> {
            self.0.borrow_mut().panel.rotation = rotation;
            Ok(())
        }

        fn night_mode(&self) -> bool {
            self.0.borrow().panel.night
        }

        fn set_night_mode(&mut self, on: bool) -> Result<()> {
            self.0.borrow_mut().panel.night = on;
            Ok(())
        }

        fn screenshot(&self) -> Result<Canvas> {
            Ok(self.0.borrow().panel.pixels.clone())
        }
    };
}
pub(crate) use panel_screen_state;

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
        let all = Rect::from_size(size);
        let mut p = EinkPanel::new(size, true);
        p.update(&black, all, RefreshClass::Gc16);
        p.update(&white, all, RefreshClass::Reagl);
        assert!(p.pixels.pixels[0] < 255, "REAGL leaves a ghost");
        p.update(&white, all, RefreshClass::Gc16);
        assert_eq!(p.pixels.pixels[0], 255, "GC16 clears it");
        let mut clean = EinkPanel::new(size, false);
        clean.update(&black, all, RefreshClass::Reagl);
        assert_eq!(clean.pixels.pixels, vec![0, 0]);
    }
}
