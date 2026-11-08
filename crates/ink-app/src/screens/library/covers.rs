//! Cover thumbnails: decoded once from the book (ink-html's image decoder, or MuPDF for
//! fixed layouts), stored as gray PNGs at `Paths::cover(md5)`, and kept scaled in
//! memory for the pages on screen.

use ink_device::Canvas;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Thumbnail box in the cache (the image fits inside, aspect kept).
pub const THUMB_W: u32 = 300;
/// Thumbnail box height.
pub const THUMB_H: u32 = 400;

/// Decodes a cover image (JPEG, PNG, GIF, WebP, SVG) into a gray canvas that fits
/// [`THUMB_W`] x [`THUMB_H`].
pub fn from_bytes(bytes: &[u8]) -> Option<Canvas> {
    let (iw, ih) = ink_html::images::intrinsic_size(bytes)?;
    let s = (THUMB_W as f32 / iw as f32).min(THUMB_H as f32 / ih as f32);
    let (w, h) = (((iw as f32 * s).round() as u32).max(1), ((ih as f32 * s).round() as u32).max(1));
    let img = ink_html::images::decode_thumbnail(bytes, w, h)?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let pixels = img.into_raw();
    (pixels.len() == w * h).then(|| Canvas::from_pixels(w, h, pixels))
}

/// Writes `img` to `path` (creating the folder). Returns the path, or `None` (logged)
/// when it cannot be written.
pub fn save(img: &Canvas, path: &Path) -> Option<PathBuf> {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    match img.save_png_fast(path) {
        Ok(()) => Some(path.to_path_buf()),
        Err(e) => {
            log::warn!("library: cannot write cover {}: {e}", path.display());
            None
        }
    }
}

/// Scaled thumbnails for drawing, keyed by file and box size. Cleared when it grows
/// past a few pages' worth. Shared (`Arc`), so a [`ink_ui::Cover`] takes one without
/// copying the pixels.
#[derive(Default)]
pub struct CoverCache {
    scaled: HashMap<(PathBuf, u32, u32), Option<Arc<Canvas>>>,
}

impl CoverCache {
    const MAX: usize = 96;

    /// The cover at `path` fitted into `w` x `h` (white margins), or `None` when it
    /// cannot be read.
    pub fn get(&mut self, path: &Path, w: u32, h: u32) -> Option<&Arc<Canvas>> {
        let key = (path.to_path_buf(), w, h);
        if self.scaled.len() >= Self::MAX && !self.scaled.contains_key(&key) {
            self.scaled.clear();
        }
        let c = self.scaled.entry(key).or_insert_with(|| match Canvas::load_png(path) {
            Ok(c) => Some(Arc::new(fit(&c, w, h))),
            Err(e) => {
                log::info!("library: cover {}: {e}", path.display());
                None
            }
        });
        c.as_ref()
    }

    /// Forgets everything (after covers were rewritten or deleted).
    pub fn clear(&mut self) {
        self.scaled.clear();
    }
}

/// `c` scaled to fit `w` x `h` without margins (the result may be smaller than the box).
fn fit(c: &Canvas, w: u32, h: u32) -> Canvas {
    if c.width == 0 || c.height == 0 {
        return Canvas::new(1, 1);
    }
    let s = (w as f32 / c.width as f32).min(h as f32 / c.height as f32);
    let (nw, nh) = (((c.width as f32 * s).round() as usize).clamp(1, w as usize), ((c.height as f32 * s).round() as usize).clamp(1, h as usize));
    c.scaled(nw, nh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_cover_fits_the_box_and_round_trips() {
        let mut img = Canvas::new(600, 900);
        img.clear(200);
        img.fill_rect(0, 0, 300, 450, 0);
        let dir = std::env::temp_dir().join(format!("ink-lib-covers-{}", std::process::id()));
        let png = dir.join("src.png");
        std::fs::create_dir_all(&dir).unwrap();
        img.save_png(&png).unwrap();
        let bytes = std::fs::read(&png).unwrap();
        let t = from_bytes(&bytes).expect("decoded");
        assert_eq!((t.width, t.height), (267, 400));
        assert!(t.pixels[0] < 30 && t.pixels[t.pixels.len() - 1] > 170);
        let out = save(&t, &dir.join("a/b/c.png")).expect("saved");
        let mut cache = CoverCache::default();
        let s = cache.get(&out, 80, 110).expect("loaded");
        assert!(s.width <= 80 && s.height == 110 || s.width == 80 && s.height <= 110, "{}x{}", s.width, s.height);
        assert!(cache.get(&dir.join("missing.png"), 80, 110).is_none());
        assert!(from_bytes(b"not an image").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
