//! 8-bit grayscale pixel buffer. 0 = black, 255 = white.
//!
//! Every renderer (text, widgets, documents) draws into a [`Canvas`]; a
//! [`crate::Screen`] shows a rectangle of it on the panel.

use crate::geom::{Rect, Size};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

/// Largest image [`Canvas::load_png`] decodes (the PW12 panel has 2.2 Mpx; a
/// 4096x4096 RGBA image needs a 64 MB decode buffer).
pub const MAX_PNG_PIXELS: u64 = 4096 * 4096;

/// Deflate cannot compress better than about 1032:1.
const DEFLATE_MAX_RATIO: u64 = 1032;

/// A row-major 8-bit grayscale image with no padding (`stride == width`).
#[derive(Clone, PartialEq, Eq)]
pub struct Canvas {
    /// Width in pixels.
    pub width: usize,
    /// Height in pixels.
    pub height: usize,
    /// `width * height` gray values, row by row.
    pub pixels: Vec<u8>,
    /// Drawing limit ([`Canvas::set_clip`]): `set`, `fill_rect`, `draw_mask`, `blit`
    /// and `invert_rect` write only inside it. `None`: the whole canvas.
    clip: Option<Rect>,
}

impl std::fmt::Debug for Canvas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Canvas({}x{})", self.width, self.height)
    }
}

impl Canvas {
    /// A white canvas.
    pub fn new(width: usize, height: usize) -> Self {
        Canvas { width, height, pixels: vec![255; width * height], clip: None }
    }

    /// A canvas over `pixels` (`width * height` gray values, row by row).
    pub fn from_pixels(width: usize, height: usize, pixels: Vec<u8>) -> Self {
        debug_assert_eq!(pixels.len(), width * height);
        Canvas { width, height, pixels, clip: None }
    }

    /// Limits drawing to `clip` (`None`: the whole canvas) and returns the previous
    /// limit, to put back when done. Widget trees set it to each widget's bounds while
    /// the widget paints, so nothing a widget draws lands outside the area that is
    /// cleared and refreshed for it. `clear` and direct `pixels` writes ignore it.
    pub fn set_clip(&mut self, clip: Option<Rect>) -> Option<Rect> {
        std::mem::replace(&mut self.clip, clip)
    }

    /// The current drawing limit ([`Canvas::set_clip`]).
    pub fn clip(&self) -> Option<Rect> {
        self.clip
    }

    /// Where drawing may write: the canvas, inside the clip.
    fn drawable(&self) -> Rect {
        match self.clip {
            Some(c) => self.bounds().intersect(&c),
            None => self.bounds(),
        }
    }

    /// Size as a [`Size`].
    pub fn size(&self) -> Size {
        Size::new(self.width as u32, self.height as u32)
    }

    /// The whole canvas as a rectangle.
    pub fn bounds(&self) -> Rect {
        Rect::from_size(self.size())
    }

    /// Fills every pixel with `value`.
    pub fn clear(&mut self, value: u8) {
        self.pixels.fill(value);
    }

    /// Gray value at `(x, y)`, or `None` outside the canvas.
    pub fn get(&self, x: i32, y: i32) -> Option<u8> {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return None;
        }
        Some(self.pixels[y as usize * self.width + x as usize])
    }

    /// Sets one pixel; outside the canvas it does nothing.
    pub fn set(&mut self, x: i32, y: i32, value: u8) {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return;
        }
        if self.clip.is_some_and(|c| !c.contains(crate::Point::new(x, y))) {
            return;
        }
        self.pixels[y as usize * self.width + x as usize] = value;
    }

    /// Fills a rectangle, clipped to the canvas (and the clip).
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, value: u8) {
        let d = self.drawable();
        let x0 = x.max(d.x).max(0) as usize;
        let y0 = y.max(d.y).max(0) as usize;
        let x1 = ((x.saturating_add(w)).min(d.right()).max(0) as usize).min(self.width);
        let y1 = ((y.saturating_add(h)).min(d.bottom()).max(0) as usize).min(self.height);
        if x0 >= x1 {
            return;
        }
        for row in y0..y1 {
            self.pixels[row * self.width + x0..row * self.width + x1].fill(value);
        }
    }

    /// Draws a 1-pixel-wide rectangle outline of `thickness` pixels.
    pub fn stroke_rect(&mut self, r: Rect, thickness: i32, value: u8) {
        let (x, y, w, h) = (r.x, r.y, r.w as i32, r.h as i32);
        self.fill_rect(x, y, w, thickness, value);
        self.fill_rect(x, y + h - thickness, w, thickness, value);
        self.fill_rect(x, y, thickness, h, value);
        self.fill_rect(x + w - thickness, y, thickness, h, value);
    }

    /// Darkens pixels with a glyph coverage mask (black ink on the existing background).
    /// `mask` is `w * h` coverage values, 255 = full ink.
    pub fn draw_mask(&mut self, x: i32, y: i32, w: usize, h: usize, mask: &[u8]) {
        let r = self.drawable().intersect(&Rect::new(x, y, w as u32, h as u32));
        let (mx, n) = ((r.x - x) as usize, r.w as usize);
        for py in r.y..r.bottom() {
            let m = (py - y) as usize * w + mx;
            let d = py as usize * self.width + r.x as usize;
            for (dst, &a) in self.pixels[d..d + n].iter_mut().zip(&mask[m..m + n]) {
                *dst = (*dst as u32 * (255 - a as u32) / 255) as u8;
            }
        }
    }

    /// Copies `src` with its top-left corner at `(x, y)`, clipped to this canvas.
    pub fn blit(&mut self, src: &Canvas, x: i32, y: i32) {
        let dst_rect = self.drawable().intersect(&Rect::new(x, y, src.width as u32, src.height as u32));
        for row in 0..dst_rect.h as i32 {
            let dy = dst_rect.y + row;
            let sy = dy - y;
            let sx = dst_rect.x - x;
            let n = dst_rect.w as usize;
            let s = sy as usize * src.width + sx as usize;
            let d = dy as usize * self.width + dst_rect.x as usize;
            self.pixels[d..d + n].copy_from_slice(&src.pixels[s..s + n]);
        }
    }

    /// A copy of the pixels inside `r` (clipped to the canvas).
    pub fn crop(&self, r: Rect) -> Canvas {
        let r = self.bounds().intersect(&r);
        let mut out = Canvas::new(r.w as usize, r.h as usize);
        for row in 0..r.h as usize {
            let s = (r.y as usize + row) * self.width + r.x as usize;
            out.pixels[row * r.w as usize..(row + 1) * r.w as usize].copy_from_slice(&self.pixels[s..s + r.w as usize]);
        }
        out
    }

    /// Inverts every pixel (night mode in software).
    pub fn invert(&mut self) {
        for p in &mut self.pixels {
            *p = 255 - *p;
        }
    }

    /// Inverts the pixels inside `r` (night mode, selection highlight).
    pub fn invert_rect(&mut self, r: Rect) {
        let r = self.drawable().intersect(&r);
        for row in r.y as usize..r.bottom() as usize {
            for p in &mut self.pixels[row * self.width + r.x as usize..row * self.width + r.right() as usize] {
                *p = 255 - *p;
            }
        }
    }

    /// A copy scaled to `w` x `h` (bilinear when enlarging, box average when shrinking).
    pub fn scaled(&self, w: usize, h: usize) -> Canvas {
        let mut out = Canvas::new(w, h);
        if self.width == 0 || self.height == 0 || w == 0 || h == 0 {
            return out;
        }
        let (sw, sh) = (self.width as f32, self.height as f32);
        let (fx, fy) = (sw / w as f32, sh / h as f32);
        if fx > 1.0 || fy > 1.0 {
            // Average the source box each pixel covers: the rows of an output row are
            // summed per source column once, then each output column sums its span.
            let span = |i: usize, f: f32, n: usize| {
                let a = (i as f32 * f) as usize;
                (a, (((i + 1) as f32 * f) as usize).clamp(a + 1, n))
            };
            let cols: Vec<(usize, usize)> = (0..w).map(|x| span(x, fx, self.width)).collect();
            let mut sums = vec![0u32; self.width];
            for (y, out_row) in out.pixels.chunks_exact_mut(w).enumerate() {
                let (y0, y1) = span(y, fy, self.height);
                sums.fill(0);
                for row in self.pixels[y0 * self.width..y1 * self.width].chunks_exact(self.width) {
                    sums.iter_mut().zip(row).for_each(|(s, p)| *s += *p as u32);
                }
                for (o, &(x0, x1)) in out_row.iter_mut().zip(&cols) {
                    *o = (sums[x0..x1].iter().sum::<u32>() / ((x1 - x0) * (y1 - y0)) as u32) as u8;
                }
            }
        } else {
            // Bilinear: the two source pixels and the weight of the second, per column
            // and per row.
            let taps = |i: usize, f: f32, n: usize| {
                let s = ((i as f32 + 0.5) * f - 0.5).clamp(0.0, n as f32 - 1.0);
                let a = s as usize;
                (a, (a + 1).min(n - 1), s - a as f32)
            };
            let cols: Vec<(usize, usize, f32)> = (0..w).map(|x| taps(x, fx, self.width)).collect();
            for (y, out_row) in out.pixels.chunks_exact_mut(w).enumerate() {
                let (y0, y1, ty) = taps(y, fy, self.height);
                let (r0, r1) = (&self.pixels[y0 * self.width..], &self.pixels[y1 * self.width..]);
                for (o, &(x0, x1, tx)) in out_row.iter_mut().zip(&cols) {
                    let top = r0[x0] as f32 * (1.0 - tx) + r0[x1] as f32 * tx;
                    let bot = r1[x0] as f32 * (1.0 - tx) + r1[x1] as f32 * tx;
                    *o = (top * (1.0 - ty) + bot * ty).round() as u8;
                }
            }
        }
        out
    }

    /// Scales to fit inside `w` x `h` keeping the aspect ratio, centred on `background`.
    pub fn fit_into(&self, w: usize, h: usize, background: u8) -> Canvas {
        let mut out = Canvas::new(w, h);
        out.clear(background);
        if self.width == 0 || self.height == 0 {
            return out;
        }
        let scale = (w as f32 / self.width as f32).min(h as f32 / self.height as f32);
        let (nw, nh) = (((self.width as f32 * scale).round() as usize).max(1), ((self.height as f32 * scale).round() as usize).max(1));
        let img = self.scaled(nw.min(w), nh.min(h));
        out.blit(&img, (w as i32 - img.width as i32) / 2, (h as i32 - img.height as i32) / 2);
        out
    }

    /// Writes an 8-bit grayscale PNG.
    pub fn save_png(&self, path: &Path) -> std::io::Result<()> {
        let file = BufWriter::new(File::create(path)?);
        let mut enc = png::Encoder::new(file, self.width as u32, self.height as u32);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(std::io::Error::other)?;
        w.write_image_data(&self.pixels).map_err(std::io::Error::other)?;
        w.finish().map_err(std::io::Error::other)?;
        Ok(())
    }

    /// Reads a PNG and converts it to 8-bit gray (RGB is averaged, alpha is
    /// composited on white). Used by golden tests and screensaver images.
    ///
    /// The size in the header is checked before anything is allocated: more than
    /// [`MAX_PNG_PIXELS`], or more pixel data than the file can hold even at deflate's
    /// best ratio (a truncated download whose header says 38730x38730), is an error,
    /// not a huge allocation that aborts the process.
    pub fn load_png(path: &Path) -> std::io::Result<Canvas> {
        let file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut dec = png::Decoder::new(BufReader::new(file));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().map_err(std::io::Error::other)?;
        let (w, h) = (reader.info().width as u64, reader.info().height as u64);
        if w * h > MAX_PNG_PIXELS {
            return Err(std::io::Error::other(format!("PNG {w}x{h} has more than {MAX_PNG_PIXELS} pixels")));
        }
        let bits = reader.info().color_type.samples() as u64 * reader.info().bit_depth as u8 as u64;
        let raw = h * (1 + (w * bits).div_ceil(8));
        if file_len.saturating_mul(DEFLATE_MAX_RATIO) < raw {
            return Err(std::io::Error::other(format!("PNG {w}x{h} cannot fit in {file_len} bytes (truncated?)")));
        }
        let size = reader.output_buffer_size().ok_or_else(|| std::io::Error::other("PNG too large"))?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(size).map_err(|e| std::io::Error::other(format!("PNG {w}x{h}: {e}")))?;
        buf.resize(size, 0);
        let info = reader.next_frame(&mut buf).map_err(std::io::Error::other)?;
        let (w, h) = (info.width as usize, info.height as usize);
        let channels = info.color_type.samples();
        let mut out = Canvas::new(w, h);
        for i in 0..w * h {
            let px = &buf[i * channels..(i + 1) * channels];
            out.pixels[i] = match channels {
                1 => px[0],
                2 => composite(px[0], px[1]),
                3 => ((px[0] as u32 + px[1] as u32 + px[2] as u32) / 3) as u8,
                _ => composite(((px[0] as u32 + px[1] as u32 + px[2] as u32) / 3) as u8, px[3]),
            };
        }
        Ok(out)
    }
}

fn composite(gray: u8, alpha: u8) -> u8 {
    ((gray as u32 * alpha as u32 + 255 * (255 - alpha as u32)) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip_keeps_pixels() {
        let mut c = Canvas::new(7, 5);
        c.fill_rect(1, 1, 3, 2, 0);
        c.set(6, 4, 17);
        let dir = std::env::temp_dir().join(format!("ink-canvas-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.png");
        c.save_png(&path).unwrap();
        assert_eq!(Canvas::load_png(&path).unwrap(), c);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A small valid PNG whose IHDR is then changed to claim `w` x `h` gray pixels
    /// (the data ends long before that, like a truncated download).
    fn lying_png(path: &Path, w: u32, h: u32) {
        Canvas::new(64, 1).save_png(path).unwrap();
        let mut v = std::fs::read(path).unwrap();
        assert_eq!(&v[12..16], b"IHDR");
        v[16..20].copy_from_slice(&w.to_be_bytes());
        v[20..24].copy_from_slice(&h.to_be_bytes());
        let crc = crc32(&v[12..29]);
        v[29..33].copy_from_slice(&crc.to_be_bytes());
        std::fs::write(path, v).unwrap();
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for b in data {
            c ^= *b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }

    #[test]
    fn png_headers_that_ask_for_too_much_are_refused_before_allocating() {
        let dir = std::env::temp_dir().join(format!("ink-canvas-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.png");
        for (w, h, what) in [(38730, 38730, "more than"), (4096, 4097, "more than"), (2000, 2000, "cannot fit")] {
            lying_png(&path, w, h);
            let e = Canvas::load_png(&path).unwrap_err().to_string();
            assert!(e.contains(what), "{w}x{h}: {e}");
        }
        // Within the limits and complete: decoded.
        let mut c = Canvas::new(1272, 1696);
        c.fill_rect(10, 10, 100, 100, 0);
        c.save_png(&path).unwrap();
        assert_eq!(Canvas::load_png(&path).unwrap(), c);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn clip_limits_every_drawing_call() {
        let mut c = Canvas::new(20, 20);
        assert_eq!(c.set_clip(Some(Rect::new(5, 5, 10, 10))), None);
        c.fill_rect(0, 0, 20, 20, 0);
        c.set(1, 1, 0);
        c.draw_mask(0, 0, 20, 1, &[255; 20]);
        c.blit(&Canvas::from_pixels(20, 20, vec![0; 400]), 0, 0);
        c.invert_rect(Rect::new(0, 0, 20, 20));
        let ink = |c: &Canvas| (0..20).flat_map(|y| (0..20).map(move |x| (x, y))).filter(|(x, y)| c.get(*x, *y) != Some(255)).collect::<Vec<_>>();
        let inside = |(x, y): &(i32, i32)| (5..15).contains(x) && (5..15).contains(y);
        assert!(ink(&c).iter().all(inside), "nothing written outside the clip");
        assert_eq!(c.get(7, 7), Some(255), "fill, blit and invert all ran inside");
        assert_eq!(c.set_clip(None), Some(Rect::new(5, 5, 10, 10)));
        c.set(1, 1, 0);
        assert_eq!(c.get(1, 1), Some(0));
    }

    #[test]
    fn mask_darkens_and_clips() {
        let mut c = Canvas::new(4, 4);
        c.draw_mask(-1, -1, 3, 3, &[255; 9]);
        assert_eq!(c.get(0, 0), Some(0));
        assert_eq!(c.get(1, 1), Some(0));
        assert_eq!(c.get(2, 2), Some(255));
        // Every offset in and around a clipped canvas: the same pixels as darkening
        // each covered pixel on its own.
        let mask: Vec<u8> = (0..5 * 3).map(|i| (i * 40 % 256) as u8).collect();
        for clip in [None, Some(Rect::new(1, 2, 4, 3))] {
            for (x, y) in (-6..9).flat_map(|x| (-4..8).map(move |y| (x, y))) {
                let mut got = Canvas::from_pixels(6, 5, (0..30).map(|i| 200 - i as u8).collect());
                let mut want = got.clone();
                got.set_clip(clip);
                got.draw_mask(x, y, 5, 3, &mask);
                for (i, a) in mask.iter().enumerate() {
                    let (px, py) = (x + (i % 5) as i32, y + (i / 5) as i32);
                    if let Some(v) = want.get(px, py).filter(|_| clip.is_none_or(|c| c.contains(crate::Point::new(px, py)))) {
                        want.pixels[py as usize * 6 + px as usize] = (v as u32 * (255 - *a as u32) / 255) as u8;
                    }
                }
                assert_eq!(got.pixels, want.pixels, "mask at {x},{y} clip {clip:?}");
            }
        }
    }

    #[test]
    fn scaling_and_fit() {
        let mut a = Canvas::new(4, 2);
        a.fill_rect(0, 0, 2, 2, 0);
        let down = a.scaled(2, 1);
        assert_eq!(down.pixels, vec![0, 255]);
        let up = a.scaled(8, 4);
        assert_eq!(up.get(0, 0), Some(0));
        assert_eq!(up.get(7, 3), Some(255));
        let f = a.fit_into(10, 10, 128);
        assert_eq!((f.width, f.height), (10, 10));
        assert_eq!(f.get(0, 0), Some(128), "letterbox");
        assert_eq!(f.get(1, 4), Some(0));
        assert_eq!(f.get(9, 4), Some(255));
    }

    /// Shrinking averages exactly the source box each pixel covers, in either
    /// direction, also when the other one grows.
    #[test]
    fn shrinking_averages_each_source_box() {
        let mut c = Canvas::new(61, 37);
        c.pixels.iter_mut().enumerate().for_each(|(i, p)| *p = (i * 37 % 251) as u8);
        for (w, h) in [(20, 12), (7, 37), (61, 5), (100, 9), (1, 1)] {
            let (fx, fy) = (61.0 / w as f32, 37.0 / h as f32);
            let s = c.scaled(w, h);
            for (y, x) in (0..h).flat_map(|y| (0..w).map(move |x| (y, x))) {
                let (x0, y0) = ((x as f32 * fx) as usize, (y as f32 * fy) as usize);
                let x1 = (((x + 1) as f32 * fx) as usize).clamp(x0 + 1, 61);
                let y1 = (((y + 1) as f32 * fy) as usize).clamp(y0 + 1, 37);
                let sum: u32 = (y0..y1).flat_map(|yy| (x0..x1).map(move |xx| (xx, yy))).map(|(xx, yy)| c.pixels[yy * 61 + xx] as u32).sum();
                assert_eq!(s.pixels[y * w + x] as u32, sum / ((x1 - x0) * (y1 - y0)) as u32, "{w}x{h} at {x},{y}");
            }
        }
    }

    #[test]
    fn blit_crop_invert() {
        let mut a = Canvas::new(10, 10);
        let mut b = Canvas::new(3, 3);
        b.clear(0);
        a.blit(&b, 8, 8);
        assert_eq!(a.get(9, 9), Some(0));
        assert_eq!(a.get(7, 7), Some(255));
        let c = a.crop(Rect::new(8, 8, 5, 5));
        assert_eq!((c.width, c.height), (2, 2));
        a.invert_rect(Rect::new(0, 0, 1, 1));
        assert_eq!(a.get(0, 0), Some(0));
    }
}


