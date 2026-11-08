//! Images: intrinsic sizes, decoding (JPEG, PNG, GIF, WebP with the `image` crate; SVG
//! with `resvg`), scaling and Floyd-Steinberg dithering to the 16 gray levels of the
//! e-ink panel. Transparent pixels are composited on white.

use image::{DynamicImage, GrayImage, ImageReader};
use ink_device::Canvas;
use std::io::Cursor;
use std::sync::Arc;

/// Reads a resource an SVG refers to (by its `href` as written).
pub type HrefResolver<'a> = Arc<dyn Fn(&str) -> Option<Vec<u8>> + Send + Sync + 'a>;

/// True when `bytes` look like SVG (XML with an `<svg` element).
pub fn is_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(2048)];
    let s = String::from_utf8_lossy(head);
    let t = s.trim_start_matches('\u{feff}').trim_start();
    (t.starts_with('<') && s.contains("<svg")) || t.starts_with("<svg")
}

fn svg_options<'a>(resolve: Option<HrefResolver<'a>>) -> usvg::Options<'a> {
    let mut opt = usvg::Options::default();
    if let Some(r) = resolve {
        opt.image_href_resolver.resolve_string = Box::new(move |href: &str, _o: &usvg::Options| {
            let bytes = r(href)?;
            let kind = match image::guess_format(&bytes).ok()? {
                image::ImageFormat::Jpeg => usvg::ImageKind::JPEG(Arc::new(bytes)),
                image::ImageFormat::Png => usvg::ImageKind::PNG(Arc::new(bytes)),
                image::ImageFormat::Gif => usvg::ImageKind::GIF(Arc::new(bytes)),
                image::ImageFormat::WebP => usvg::ImageKind::WEBP(Arc::new(bytes)),
                _ => return None,
            };
            Some(kind)
        });
    } else {
        opt.image_href_resolver.resolve_string = Box::new(|_: &str, _: &usvg::Options| None);
    }
    opt
}

/// Intrinsic size in pixels (SVG: its width/height or viewBox).
pub fn intrinsic_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if is_svg(bytes) {
        let tree = usvg::Tree::from_data(bytes, &svg_options(None)).ok()?;
        let s = tree.size();
        return Some((s.width().round().max(1.0) as u32, s.height().round().max(1.0) as u32));
    }
    let r = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    r.into_dimensions().ok().filter(|(w, h)| *w > 0 && *h > 0)
}

/// A decoded gray image scaled to `w` x `h` (not dithered). `resolve` loads resources an
/// SVG refers to.
pub fn decode_scaled(bytes: &[u8], w: u32, h: u32, resolve: Option<HrefResolver<'_>>) -> Option<GrayImage> {
    decode_with(bytes, w, h, resolve, GrayMath::ImageCrate)
}

/// [`decode_scaled`] for a thumbnail that is stored, not drawn as is (library covers):
/// 8-bit colour becomes gray with integer Rec. 709 weights, where the image crate's
/// conversion (f32 colour management per pixel) takes about 70 ms for a 600 x 900
/// cover on a Kindle. The two differ by at most one level of 255 (rounding).
pub fn decode_thumbnail(bytes: &[u8], w: u32, h: u32) -> Option<GrayImage> {
    decode_with(bytes, w, h, None, GrayMath::Integer)
}

/// How [`whole_decode`] turns 8-bit colour into gray.
#[derive(Clone, Copy, PartialEq)]
enum GrayMath {
    /// The image crate's `to_luma8` (what page goldens were made with).
    ImageCrate,
    /// [`gray_of`]: integer Rec. 709 luma, rounded.
    Integer,
}

fn decode_with(bytes: &[u8], w: u32, h: u32, resolve: Option<HrefResolver<'_>>, math: GrayMath) -> Option<GrayImage> {
    if w == 0 || h == 0 || w > 8000 || h > 8000 {
        return None;
    }
    if is_svg(bytes) {
        let tree = usvg::Tree::from_data(bytes, &svg_options(resolve)).ok()?;
        if let Some(img) = svg_raster_only(&tree, w, h) {
            return Some(img);
        }
        return Some(render_svg_tree(&tree, w, h));
    }
    let (sw, sh) = intrinsic_size(bytes)?;
    let source_px = sw as u64 * sh as u64;
    if source_px > BIG_SOURCE_PX {
        // Decoding at full size would take source_px x 1..4 bytes (plus copies): a 10000 x
        // 10000 RGB PNG peaked at 406 MB on the Kindle. Decode with bounded memory instead.
        let fmt = image::guess_format(bytes).ok();
        let out = match fmt {
            Some(image::ImageFormat::Png) => png_shrunk(bytes, w, h),
            Some(image::ImageFormat::Jpeg) => jpeg_shrunk(bytes, w, h),
            _ if source_px <= MAX_OTHER_PX => whole_decode(bytes, w, h, math),
            _ => None,
        };
        if out.is_none() {
            log::warn!("ink-html: image {sw}x{sh} ({fmt:?}) not decoded (too large or corrupt)");
        }
        return out;
    }
    whole_decode(bytes, w, h, math)
}

/// Sources above this many pixels go through the bounded-memory decoders.
pub const BIG_SOURCE_PX: u64 = 4 << 20;
/// Largest PNG decoded (row by row, so memory stays small; this bounds the time).
pub const MAX_PNG_PX: u64 = 400_000_000;
/// Largest JPEG decoded (straight to 8-bit gray: this many bytes).
pub const MAX_JPEG_PX: u64 = 40 << 20;
/// Largest GIF or WebP decoded (whole, RGBA).
pub const MAX_OTHER_PX: u64 = 16 << 20;
/// Widest source accepted by the row decoders.
const MAX_SOURCE_W: u32 = 100_000;

/// Decodes the whole image with the `image` crate, then scales it.
fn whole_decode(bytes: &[u8], w: u32, h: u32, math: GrayMath) -> Option<GrayImage> {
    let mut r = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(256 << 20);
    r.limits(limits);
    let img = r.decode().ok()?;
    if math == GrayMath::ImageCrate {
        return Some(finish_size(image_crate_gray(img)?, w, h));
    }
    let (iw, ih) = (img.width(), img.height());
    let raw = match img {
        DynamicImage::ImageLuma8(i) => return Some(finish_size(i, w, h)),
        DynamicImage::ImageLumaA8(i) => gray_pixels::<2>(i.as_raw()),
        DynamicImage::ImageRgb8(i) => gray_pixels::<3>(i.as_raw()),
        DynamicImage::ImageRgba8(i) => gray_pixels::<4>(i.as_raw()),
        other => return Some(finish_size(image_crate_gray(other)?, w, h)),
    };
    Some(finish_size(GrayImage::from_raw(iw, ih, raw)?, w, h))
}

/// An image as 8-bit gray with the image crate's conversion, transparency composited
/// on white.
fn image_crate_gray(img: DynamicImage) -> Option<GrayImage> {
    if img.color().has_alpha() {
        let la = img.to_luma_alpha8();
        let (iw, ih) = la.dimensions();
        let raw: Vec<u8> = la
            .as_raw()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| over_white(p[0] as u32, p[1] as u32))
            .collect();
        GrayImage::from_raw(iw, ih, raw)
    } else {
        Some(img.to_luma8())
    }
}

/// [`gray_of`] for every pixel of `C`-channel 8-bit data.
fn gray_pixels<const C: usize>(raw: &[u8]) -> Vec<u8> {
    raw.as_chunks::<C>().0.iter().map(|p| gray_of(p, C)).collect()
}

/// 8-bit gray of one pixel with `channels` (1 gray, 2 gray+alpha, 3 RGB, 4 RGBA)
/// samples; transparency is composited on white.
fn gray_of(px: &[u8], channels: usize) -> u8 {
    match channels {
        1 => px[0],
        2 => over_white(px[0] as u32, px[1] as u32),
        3 => luma(px[0], px[1], px[2]) as u8,
        _ => over_white(luma(px[0], px[1], px[2]), px[3] as u32),
    }
}

/// Gray `l` with alpha `a` composited on white paper.
fn over_white(l: u32, a: u32) -> u8 {
    ((l * a + 255 * (255 - a)) / 255) as u8
}

/// Rec. 709 luma, as the `image` crate converts RGB to gray.
fn luma(r: u8, g: u8, b: u8) -> u32 {
    (2126 * r as u32 + 7152 * g as u32 + 722 * b as u32 + 5000) / 10000
}

/// Area-average shrinking of a gray image fed one source row at a time, top to bottom.
/// Memory: one output image plus one row of sums.
struct RowShrink {
    sw: u32,
    sh: u32,
    w: u32,
    h: u32,
    /// Output column of each source column.
    xmap: Vec<u32>,
    /// Source columns per output column.
    xcnt: Vec<u32>,
    acc: Vec<u64>,
    rows_in: u32,
    y: u32,
    out: Vec<u8>,
}

impl RowShrink {
    /// Shrinks `sw` x `sh` to `w` x `h` (`w <= sw`, `h <= sh`).
    fn new(sw: u32, sh: u32, w: u32, h: u32) -> RowShrink {
        let xmap: Vec<u32> = (0..sw).map(|x| (x as u64 * w as u64 / sw as u64) as u32).collect();
        let mut xcnt = vec![0u32; w as usize];
        for &o in &xmap {
            xcnt[o as usize] += 1;
        }
        RowShrink { sw, sh, w, h, xmap, xcnt, acc: vec![0; w as usize], rows_in: 0, y: 0, out: vec![255; w as usize * h as usize] }
    }

    fn oy(&self, y: u32) -> u32 {
        (y as u64 * self.h as u64 / self.sh as u64) as u32
    }

    /// Adds source row `self.y` (`sw` gray values).
    fn push(&mut self, row: impl Iterator<Item = u8>) {
        if self.y >= self.sh {
            return;
        }
        for (x, v) in row.take(self.sw as usize).enumerate() {
            self.acc[self.xmap[x] as usize] += v as u64;
        }
        let oy = self.oy(self.y);
        self.rows_in += 1;
        self.y += 1;
        if self.y == self.sh || self.oy(self.y) != oy {
            let base = oy as usize * self.w as usize;
            for ox in 0..self.w as usize {
                let n = self.xcnt[ox] as u64 * self.rows_in as u64;
                self.out[base + ox] = ((self.acc[ox] + n / 2) / n.max(1)) as u8;
                self.acc[ox] = 0;
            }
            self.rows_in = 0;
        }
    }

    fn finish(self) -> Option<GrayImage> {
        (self.y == self.sh).then_some(())?;
        GrayImage::from_raw(self.w, self.h, self.out)
    }
}

/// Scales an image to exactly `w` x `h` unless it has that size (the shrinking
/// decoders give at most `w` x `h`: axes the source was too small for grow).
fn finish_size(img: GrayImage, w: u32, h: u32) -> GrayImage {
    if img.dimensions() == (w, h) {
        img
    } else {
        resize_gray(&img, w, h)
    }
}

/// The Adam7 passes: (x step, x offset, y step, y offset).
const ADAM7: [(u32, u32, u32, u32); 7] = [(8, 0, 8, 0), (8, 4, 8, 0), (4, 0, 8, 4), (4, 2, 4, 0), (2, 0, 4, 2), (2, 1, 2, 0), (1, 0, 2, 1)];

/// A PNG decoded row by row into an area-averaged `w` x `h` gray image: the full-size
/// image is never held (interlaced images: one sum per output pixel).
pub fn png_shrunk(bytes: &[u8], w: u32, h: u32) -> Option<GrayImage> {
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().ok()?;
    let (sw, sh, interlaced) = {
        let i = reader.info();
        (i.width, i.height, i.interlaced)
    };
    if sw == 0 || sh == 0 || sw > MAX_SOURCE_W || sw as u64 * sh as u64 > MAX_PNG_PX {
        return None;
    }
    let channels = match reader.output_color_type().0 {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return None,
    };
    let (tw, th) = (w.min(sw), h.min(sh));
    if !interlaced {
        let mut sh_ = RowShrink::new(sw, sh, tw, th);
        while let Ok(Some(row)) = reader.next_row() {
            let data = row.data();
            if data.len() < sw as usize * channels {
                return None;
            }
            sh_.push(data.chunks_exact(channels).map(|p| gray_of(p, channels)));
        }
        return sh_.finish().map(|i| finish_size(i, w, h));
    }
    // Adam7: rows come pass by pass; sum every source pixel into its output pixel.
    let xmap: Vec<u32> = (0..sw).map(|x| (x as u64 * tw as u64 / sw as u64) as u32).collect();
    let ymap = |y: u32| (y as u64 * th as u64 / sh as u64) as usize;
    let mut sums = vec![0u64; tw as usize * th as usize];
    let mut cnt = vec![0u32; tw as usize * th as usize];
    for (xs, xo, ys, yo) in ADAM7 {
        let samples = sw.saturating_sub(xo).div_ceil(xs);
        let lines = sh.saturating_sub(yo).div_ceil(ys);
        if samples == 0 {
            continue;
        }
        for line in 0..lines {
            let row = reader.next_row().ok()??;
            let data = row.data();
            if data.len() < samples as usize * channels {
                return None;
            }
            let y = line * ys + yo;
            let base = ymap(y) * tw as usize;
            for (i, p) in data.chunks_exact(channels).take(samples as usize).enumerate() {
                let x = i as u32 * xs + xo;
                let k = base + xmap[x as usize] as usize;
                sums[k] += gray_of(p, channels) as u64;
                cnt[k] += 1;
            }
        }
    }
    let out: Vec<u8> = sums.iter().zip(&cnt).map(|(&s, &n)| if n == 0 { 255 } else { ((s + n as u64 / 2) / n as u64) as u8 }).collect();
    GrayImage::from_raw(tw, th, out).map(|i| finish_size(i, w, h))
}

/// A JPEG decoded straight to 8-bit gray (1 byte per source pixel instead of 3 + a copy),
/// then area-averaged to `w` x `h`.
pub fn jpeg_shrunk(bytes: &[u8], w: u32, h: u32) -> Option<GrayImage> {
    use zune_core::bytestream::ZCursor;
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::Luma).set_strict_mode(false);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    dec.decode_headers().ok()?;
    let (sw, sh) = dec.dimensions().map(|(a, b)| (a as u32, b as u32))?;
    if sw == 0 || sh == 0 || sw > MAX_SOURCE_W || sw as u64 * sh as u64 > MAX_JPEG_PX {
        return None;
    }
    let gray = dec.decode().ok()?;
    if gray.len() < sw as usize * sh as usize {
        return None;
    }
    let (tw, th) = (w.min(sw), h.min(sh));
    let mut sh_ = RowShrink::new(sw, sh, tw, th);
    for row in gray.chunks_exact(sw as usize).take(sh as usize) {
        sh_.push(row.iter().copied());
    }
    drop(gray);
    sh_.finish().map(|i| finish_size(i, w, h))
}

/// Contributions of source pixels to each output pixel along one axis: a triangle
/// (bilinear) filter widened to the scale factor when shrinking, weights in 1/16384.
fn axis_weights(src: u32, dst: u32) -> Vec<(u32, Vec<i32>)> {
    let scale = src as f32 / dst as f32;
    let support = scale.max(1.0);
    (0..dst)
        .map(|o| {
            let center = (o as f32 + 0.5) * scale - 0.5;
            let lo = ((center - support).floor() as i64 + 1).max(0) as u32;
            let hi = ((center + support).ceil() as i64).min(src as i64 - 1).max(lo as i64) as u32;
            let mut ws: Vec<f32> = (lo..=hi).map(|i| (1.0 - ((i as f32 - center) / support).abs()).max(0.0)).collect();
            let sum: f32 = ws.iter().sum();
            if sum <= 0.0 {
                ws = vec![0.0; ws.len()];
                let k = (center.round().max(0.0) as u32).min(src - 1).saturating_sub(lo) as usize;
                let last = ws.len() - 1;
                ws[k.min(last)] = 1.0;
            } else {
                ws.iter_mut().for_each(|w| *w /= sum);
            }
            let mut iw: Vec<i32> = ws.iter().map(|w| (w * 16384.0).round() as i32).collect();
            // Make the weights sum to exactly 16384.
            let err = 16384 - iw.iter().sum::<i32>();
            if let Some(m) = iw.iter_mut().max_by_key(|w| **w) {
                *m += err;
            }
            (lo, iw)
        })
        .collect()
}

/// The weights of [`axis_weights`] (for a source `src` wide) as one flat array with the
/// same number of taps per output pixel: the window start of each output, the tap
/// count, and the weights, zero where the original window does not reach.
fn padded_weights(ws: &[(u32, Vec<i32>)], src: u32) -> (Vec<u32>, usize, Vec<i32>) {
    let taps = ws.iter().map(|(_, w)| w.len()).max().unwrap_or(1).min(src as usize);
    let mut starts = Vec::with_capacity(ws.len());
    let mut flat = vec![0; ws.len() * taps];
    for ((lo, w), out) in ws.iter().zip(flat.chunks_exact_mut(taps)) {
        // Move the window left when it would run past the end of the row.
        let start = (*lo).min(src - taps as u32);
        let at = (lo - start) as usize;
        out[at..at + w.len()].copy_from_slice(w);
        starts.push(start);
    }
    (starts, taps, flat)
}

/// One row of the horizontal pass of [`resize_gray`] with `N` taps per output pixel
/// (a fixed count the compiler unrolls).
fn horizontal<const N: usize>(row: &[u8], out: &mut [u16], starts: &[u32], flat: &[i32]) {
    for ((o, &lo), ws) in out.iter_mut().zip(starts).zip(flat.as_chunks::<N>().0) {
        let px: &[u8; N] = row[lo as usize..][..N].try_into().expect("N taps");
        let mut acc = 0i32;
        for k in 0..N {
            acc += px[k] as i32 * ws[k];
        }
        *o = to_8_8(acc);
    }
}

/// A horizontal sum (weights in 1/16384) rounded to 8.8 fixed point.
fn to_8_8(acc: i32) -> u16 {
    ((acc + (1 << 5)) >> 6).clamp(0, 255 << 8) as u16
}

/// Separable bilinear / area resize of a gray image in fixed point.
pub fn resize_gray(src: &GrayImage, w: u32, h: u32) -> GrayImage {
    let (sw, sh) = src.dimensions();
    if sw == 0 || sh == 0 {
        return GrayImage::from_pixel(w, h, image::Luma([255]));
    }
    let xs = axis_weights(sw, w);
    let ys = axis_weights(sh, h);
    let s = src.as_raw();
    // Horizontal pass: sh rows of w values (x 16384 scale kept as u16 after rounding).
    // Every output pixel reads the same number of source pixels (its weights padded
    // with zeros), a constant up to 8 taps, so the compiler unrolls the inner loop; the
    // sums, and so the result, are the same.
    let (starts, taps, flat) = padded_weights(&xs, sw);
    let mut tmp = vec![0u16; (w * sh) as usize];
    for y in 0..sh as usize {
        let row = &s[y * sw as usize..(y + 1) * sw as usize];
        let out = &mut tmp[y * w as usize..(y + 1) * w as usize];
        match taps {
            2 => horizontal::<2>(row, out, &starts, &flat),
            3 => horizontal::<3>(row, out, &starts, &flat),
            4 => horizontal::<4>(row, out, &starts, &flat),
            5 => horizontal::<5>(row, out, &starts, &flat),
            6 => horizontal::<6>(row, out, &starts, &flat),
            7 => horizontal::<7>(row, out, &starts, &flat),
            8 => horizontal::<8>(row, out, &starts, &flat),
            _ => {
                for ((o, &lo), ws) in out.iter_mut().zip(&starts).zip(flat.chunks_exact(taps)) {
                    let px = &row[lo as usize..lo as usize + taps];
                    *o = to_8_8(px.iter().zip(ws).map(|(&p, &wt)| p as i32 * wt).sum());
                }
            }
        }
    }
    // Vertical pass, row by row: each source row is added into one accumulator row
    // (sequential reads instead of a column walk per output pixel). Integer sums, so the
    // result is the same in any order. i32 is enough: the weights are >= 0 and sum to
    // 16384, so a sum is at most 65280 * 16384 (about 1.07e9 < 2^31).
    let wu = w as usize;
    let mut out = vec![0u8; wu * h as usize];
    let mut acc = vec![0i32; wu];
    for (oy, (lo, ws)) in ys.iter().enumerate() {
        acc.fill(0);
        for (k, wt) in ws.iter().enumerate() {
            let row = &tmp[(*lo as usize + k) * wu..][..wu];
            for (a, &v) in acc.iter_mut().zip(row) {
                *a += v as i32 * *wt;
            }
        }
        let dst = &mut out[oy * wu..(oy + 1) * wu];
        for (d, &a) in dst.iter_mut().zip(&acc) {
            *d = ((a as i64 + (1 << 21)) >> 22).clamp(0, 255) as u8;
        }
    }
    GrayImage::from_raw(w, h, out).expect("size")
}

/// An SVG that only shows one raster image (a cover page: `<svg><image .../></svg>`,
/// with or without a viewBox, sizes or `preserveAspectRatio`) drawn by decoding the
/// image straight into its place in the `w` x `h` box. resvg would decode it at full
/// size and resample it with a bicubic filter, seconds on a Kindle. `None` when the
/// SVG draws anything else, or the image is transformed, clipped, masked, filtered or
/// translucent: resvg renders those.
fn svg_raster_only(tree: &usvg::Tree, w: u32, h: u32) -> Option<GrayImage> {
    let s = tree.size();
    let (fx, fy) = (w as f32 / s.width().max(1.0), h as f32 / s.height().max(1.0));
    let canvas = tiny_skia::Rect::from_xywh(0.0, 0.0, s.width(), s.height())?;
    let mut found = None;
    if !only_one_image(tree.root(), canvas, &mut found) {
        return None;
    }
    let image = found?;
    let bytes = match image.kind() {
        usvg::ImageKind::JPEG(b) | usvg::ImageKind::PNG(b) | usvg::ImageKind::GIF(b) | usvg::ImageKind::WEBP(b) => b,
        usvg::ImageKind::SVG(_) => return None,
    };
    let t = image.abs_transform();
    if !image.is_visible() || t.kx != 0.0 || t.ky != 0.0 || t.sx <= 0.0 || t.sy <= 0.0 {
        return None;
    }
    let r = image.abs_bounding_box();
    let (x0, y0) = ((r.left() * fx).round(), (r.top() * fy).round());
    let (x1, y1) = ((r.right() * fx).round(), (r.bottom() * fy).round());
    let (dw, dh) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    if dw > 8000.0 || dh > 8000.0 {
        return None;
    }
    let img = decode_scaled(bytes, dw as u32, dh as u32, None)?;
    let mut out = GrayImage::from_pixel(w, h, image::Luma([255]));
    image::imageops::replace(&mut out, &img, x0 as i64, y0 as i64);
    Some(out)
}

/// True when `g` and its descendants draw nothing but at most one image (stored in
/// `found`) with plain groups: full opacity, normal blending, no mask or filter, and
/// no clip other than a rectangle covering the whole `canvas` (an image with
/// `preserveAspectRatio="... slice"` gets one).
fn only_one_image<'a>(g: &'a usvg::Group, canvas: tiny_skia::Rect, found: &mut Option<&'a usvg::Image>) -> bool {
    let plain = g.opacity() == usvg::Opacity::ONE
        && g.blend_mode() == usvg::BlendMode::Normal
        && g.mask().is_none()
        && g.filters().is_empty();
    if !plain || g.clip_path().is_some_and(|c| !clip_covers(c, g.abs_transform(), canvas)) {
        return false;
    }
    g.children().iter().all(|n| match n {
        usvg::Node::Group(c) => only_one_image(c, canvas, found),
        usvg::Node::Image(i) => found.replace(i).is_none(),
        usvg::Node::Path(_) | usvg::Node::Text(_) => false,
    })
}

/// True when the clip path is one axis-aligned rectangle that contains `canvas`, so
/// clipping to it changes nothing on the canvas.
fn clip_covers(clip: &usvg::ClipPath, abs: tiny_skia::Transform, canvas: tiny_skia::Rect) -> bool {
    let [usvg::Node::Path(p)] = clip.root().children() else { return false };
    if clip.clip_path().is_some() || clip.root().transform() != tiny_skia::Transform::identity() {
        return false;
    }
    let data = p.data();
    let b = data.bounds();
    let corner = |q: &tiny_skia::Point| (q.x == b.left() || q.x == b.right()) && (q.y == b.top() || q.y == b.bottom());
    if data.points().len() != 4 || !data.points().iter().all(corner) {
        return false;
    }
    let ts = abs.pre_concat(clip.transform());
    if ts.kx != 0.0 || ts.ky != 0.0 {
        return false;
    }
    let Some(r) = b.transform(ts) else { return false };
    r.left() <= canvas.left() && r.top() <= canvas.top() && r.right() >= canvas.right() && r.bottom() >= canvas.bottom()
}

fn render_svg_tree(tree: &usvg::Tree, w: u32, h: u32) -> GrayImage {
    let mut pix = tiny_skia::Pixmap::new(w, h).expect("pixmap");
    pix.fill(tiny_skia::Color::WHITE);
    let s = tree.size();
    let sx = w as f32 / s.width().max(1.0);
    let sy = h as f32 / s.height().max(1.0);
    resvg::render(tree, tiny_skia::Transform::from_scale(sx, sy), &mut pix.as_mut());
    let mut out = GrayImage::new(w, h);
    for (i, px) in pix.pixels().iter().enumerate() {
        let c = px.demultiply();
        let (r, g, b, a) = (c.red() as u32, c.green() as u32, c.blue() as u32, c.alpha() as u32);
        out.as_mut()[i] = over_white((r * 299 + g * 587 + b * 114) / 1000, a);
    }
    out
}

/// Floyd-Steinberg dithering of an 8-bit gray buffer to 16 levels (0, 17, ..., 255).
/// Integer arithmetic (errors in 1/16 units over two rows), fast on ARM.
pub fn dither16(buf: &mut [u8], w: usize, h: usize) {
    if w == 0 {
        return;
    }
    // Quantized value for each 1/16-scaled input 0..=4080.
    let lut: Vec<u8> = (0..=255 * 16).map(|v: i32| (((v + 136) / 272) * 17).min(255) as u8).collect();
    let mut cur = vec![0i32; w + 2];
    let mut next = vec![0i32; w + 2];
    for y in 0..h {
        let row = &mut buf[y * w..(y + 1) * w];
        for x in 0..w {
            let old = (((row[x] as i32) << 4) + cur[x + 1]).clamp(0, 255 * 16);
            let q = lut[old as usize];
            row[x] = q;
            let e = old - ((q as i32) << 4);
            cur[x + 2] += (e * 7) >> 4;
            next[x] += (e * 3) >> 4;
            next[x + 1] += (e * 5) >> 4;
            next[x + 2] += e >> 4;
        }
        std::mem::swap(&mut cur, &mut next);
        next.iter_mut().for_each(|v| *v = 0);
    }
}

/// The `href` of an inline `<svg>` that only wraps one raster `<image>` (the usual EPUB
/// cover page), with its `preserveAspectRatio`.
pub fn svg_single_image(dom: &crate::dom::Dom, svg: crate::dom::NodeId) -> Option<(String, String)> {
    let mut image = None;
    let mut stack = vec![svg];
    while let Some(n) = stack.pop() {
        let Some(e) = dom.element(n) else { continue };
        match e.name.as_str() {
            "svg" | "g" | "title" | "desc" | "metadata" => {}
            "image" if image.is_none() => image = Some(n),
            _ => return None,
        }
        if e.name != "title" && e.name != "desc" && e.name != "metadata" {
            stack.extend(dom.node(n).children.iter().copied());
        }
    }
    let e = dom.element(image?)?;
    let href = e.attr_local("href")?.to_string();
    if href.starts_with("data:") {
        return None;
    }
    let par = dom.element(svg).and_then(|s| s.attr("preserveAspectRatio")).unwrap_or("xMidYMid meet").to_string();
    Some((href, par))
}

/// Decodes a raster image into a `w` x `h` box following an SVG `preserveAspectRatio`
/// (`none` stretches, `slice` covers, otherwise the image fits and is centred on white).
pub fn decode_into_box(bytes: &[u8], w: u32, h: u32, par: &str) -> Option<GrayImage> {
    if par.trim() == "none" {
        return decode_scaled(bytes, w, h, None);
    }
    let (iw, ih) = intrinsic_size(bytes)?;
    let (sx, sy) = (w as f32 / iw as f32, h as f32 / ih as f32);
    let s = if par.contains("slice") { sx.max(sy) } else { sx.min(sy) };
    let (dw, dh) = (((iw as f32 * s).round() as u32).max(1), ((ih as f32 * s).round() as u32).max(1));
    let img = decode_scaled(bytes, dw, dh, None)?;
    let mut out = GrayImage::from_pixel(w, h, image::Luma([255]));
    let ox = (w as i64 - dw as i64) / 2;
    let oy = (h as i64 - dh as i64) / 2;
    image::imageops::replace(&mut out, &img, ox, oy);
    Some(out)
}

/// Serializes an inline `<svg>` element of a chapter DOM back to SVG markup.
pub fn svg_markup(dom: &crate::dom::Dom, svg: crate::dom::NodeId) -> String {
    let mut out = String::new();
    write_svg(dom, svg, &mut out, true, 0);
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn write_svg(dom: &crate::dom::Dom, id: crate::dom::NodeId, out: &mut String, root: bool, depth: u32) {
    if depth > 200 {
        return;
    }
    match &dom.node(id).kind {
        crate::dom::NodeKind::Text(t) => out.push_str(&esc(t)),
        crate::dom::NodeKind::Element(e) => {
            out.push('<');
            out.push_str(&e.name);
            if root {
                out.push_str(" xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\"");
            }
            for (k, v) in &e.attrs {
                if k == "xmlns" || k.starts_with("xmlns:") {
                    continue;
                }
                let k = if k.ends_with(":href") { "xlink:href" } else { k.as_str() };
                out.push_str(&format!(" {k}=\"{}\"", esc(v)));
            }
            out.push('>');
            for &c in &dom.node(id).children {
                write_svg(dom, c, out, false, depth + 1);
            }
            out.push_str(&format!("</{}>", e.name));
        }
    }
}

/// A small cache of decoded, scaled and dithered images for rendering, ready to blit:
/// the 8 made last.
#[derive(Default)]
pub struct ImageCache {
    entries: Vec<((String, u32, u32), Arc<Canvas>)>,
}

impl ImageCache {
    /// The dithered image `name` at `w` x `h`, decoding with `make` on a miss.
    pub fn get(&mut self, name: &str, w: u32, h: u32, make: impl FnOnce() -> Option<GrayImage>) -> Option<Arc<Canvas>> {
        if let Some((_, img)) = self.entries.iter().find(|((n, ew, eh), _)| n == name && (*ew, *eh) == (w, h)) {
            return Some(img.clone());
        }
        let img = make()?;
        let (iw, ih) = (img.width() as usize, img.height() as usize);
        let mut px = img.into_raw();
        dither16(&mut px, iw, ih);
        let img = Arc::new(Canvas::from_pixels(iw, ih, px));
        if self.entries.len() >= 8 {
            self.entries.remove(0);
        }
        self.entries.push(((name.to_string(), w, h), img.clone()));
        Some(img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

    /// The resize before the row-wise vertical pass (2026-11-02): per output pixel, a
    /// column walk with i64 sums.
    fn resize_gray_reference(src: &GrayImage, w: u32, h: u32) -> GrayImage {
        let (sw, sh) = src.dimensions();
        let xs = axis_weights(sw, w);
        let ys = axis_weights(sh, h);
        let s = src.as_raw();
        let mut tmp = vec![0u16; (w * sh) as usize];
        for y in 0..sh as usize {
            let row = &s[y * sw as usize..(y + 1) * sw as usize];
            let out = &mut tmp[y * w as usize..(y + 1) * w as usize];
            for (o, (lo, ws)) in out.iter_mut().zip(&xs) {
                let mut acc = 0i32;
                for (k, wt) in ws.iter().enumerate() {
                    acc += row[*lo as usize + k] as i32 * wt;
                }
                *o = ((acc + (1 << 5)) >> 6).clamp(0, 255 << 8) as u16;
            }
        }
        let mut out = vec![0u8; (w * h) as usize];
        for (oy, (lo, ws)) in ys.iter().enumerate() {
            for x in 0..w as usize {
                let mut acc = 0i64;
                for (k, wt) in ws.iter().enumerate() {
                    acc += tmp[(*lo as usize + k) * w as usize + x] as i64 * *wt as i64;
                }
                out[oy * w as usize + x] = ((acc + (1 << 21)) >> 22).clamp(0, 255) as u8;
            }
        }
        GrayImage::from_raw(w, h, out).expect("size")
    }

    #[test]
    fn resize_gray_matches_the_reference_byte_for_byte() {
        let mut seed = 12345u32;
        let sizes = [
            (600, 900, 267, 400),
            (1200, 1800, 300, 400),
            (37, 53, 80, 110),
            (1000, 10, 7, 3),
            (5, 700, 5, 9),
            (3000, 2000, 1272, 848),
            (800, 1104, 290, 400),
            (3, 4, 1, 1),
            (9, 9, 4, 4),
        ];
        for (sw, sh, w, h) in sizes {
            let px: Vec<u8> = (0..sw * sh)
                .map(|i| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    if i % 7 == 0 { 255 } else { (seed >> 16) as u8 }
                })
                .collect();
            let src = GrayImage::from_raw(sw, sh, px).unwrap();
            assert_eq!(resize_gray(&src, w, h).as_raw(), resize_gray_reference(&src, w, h).as_raw(), "{sw}x{sh} -> {w}x{h}");
        }
    }

    fn encode(img: &DynamicImage, f: ImageFormat) -> Vec<u8> {
        let mut c = Cursor::new(Vec::new());
        if f == ImageFormat::Png {
            // Fast compression: the default level takes seconds on 4-megapixel test images
            // in a debug build; the decoders see the same PNG structure.
            use image::codecs::png::{CompressionType, FilterType, PngEncoder};
            img.write_with_encoder(PngEncoder::new_with_quality(&mut c, CompressionType::Fast, FilterType::Sub)).unwrap();
        } else {
            img.write_to(&mut c, f).unwrap();
        }
        c.into_inner()
    }

    #[test]
    fn formats_sizes_scaling_and_dithering() {
        let rgba = RgbaImage::from_fn(40, 20, |x, _| Rgba([(x * 6) as u8, 100, 50, if x < 5 { 0 } else { 255 }]));
        let img = DynamicImage::ImageRgba8(rgba);
        for f in [ImageFormat::Png, ImageFormat::Gif, ImageFormat::WebP] {
            let b = encode(&img, f);
            assert_eq!(intrinsic_size(&b), Some((40, 20)), "{f:?}");
            let g = decode_scaled(&b, 20, 10, None).unwrap();
            assert_eq!(g.dimensions(), (20, 10));
            assert!(g.get_pixel(0, 5).0[0] > 240, "transparent is white ({f:?})");
        }
        let jpg = encode(&DynamicImage::ImageRgb8(img.to_rgb8()), ImageFormat::Jpeg);
        assert_eq!(intrinsic_size(&jpg), Some((40, 20)));
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="15" viewBox="0 0 30 15"><rect x="0" y="0" width="15" height="15" fill="black"/></svg>"#;
        assert!(is_svg(svg));
        assert_eq!(intrinsic_size(svg), Some((30, 15)));
        let g = decode_scaled(svg, 60, 30, None).unwrap();
        assert!(g.get_pixel(5, 5).0[0] < 20 && g.get_pixel(50, 5).0[0] > 240);
        let mut buf: Vec<u8> = (0..256).map(|v| v as u8).collect();
        dither16(&mut buf, 16, 16);
        assert!(buf.iter().all(|v| v % 17 == 0));
        let mean: f32 = buf.iter().map(|&v| v as f32).sum::<f32>() / 256.0;
        assert!((mean - 127.5).abs() < 4.0, "{mean}");
        assert!(intrinsic_size(b"not an image").is_none());
    }

    fn mean_diff(a: &GrayImage, b: &GrayImage) -> f32 {
        assert_eq!(a.dimensions(), b.dimensions());
        a.as_raw().iter().zip(b.as_raw()).map(|(&x, &y)| (x as i32 - y as i32).unsigned_abs()).sum::<u32>() as f32 / a.as_raw().len() as f32
    }

    // Big sources: just above BIG_SOURCE_PX, so the bounded-memory decoders run. Each
    // target size is compared with a whole decode + triangle resize of the same file:
    // shrinking, and growing one axis (finish_size). The daily tests run one format and
    // one size each, in parallel, on one shared source image; the ignored test runs every
    // format and size (about 22 s in a debug build).
    const BIG_W: u32 = 2100;
    const BIG_H: u32 = 2000;

    /// The RGBA gradient source, made once per test binary.
    fn big_image() -> &'static DynamicImage {
        static IMG: std::sync::OnceLock<DynamicImage> = std::sync::OnceLock::new();
        IMG.get_or_init(|| {
            let (w, h) = (BIG_W, BIG_H);
            assert!(w as u64 * h as u64 > BIG_SOURCE_PX);
            DynamicImage::ImageRgba8(RgbaImage::from_fn(w, h, |x, y| Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, ((x + y) * 127 / (w + h)) as u8, (255 - (x * 200 / w)) as u8])))
        })
    }

    fn check_big(name: &str, bytes: &[u8], sizes: &[(u32, u32)]) {
        for &(tw, th) in sizes {
            let fast = decode_scaled(bytes, tw, th, None).unwrap_or_else(|| panic!("{name} {tw}x{th}"));
            let whole = if name == "jpeg rgb" {
                // Big JPEGs give JPEG's own Y (Rec. 601 weights; about 9 levels from the
                // Rec. 709 luma of a whole decode on these saturated colours): compared with
                // a Y reference.
                let rgb = image::load_from_memory(bytes).unwrap().to_rgb8();
                let y = GrayImage::from_fn(BIG_W, BIG_H, |x, y| {
                    let p = rgb.get_pixel(x, y).0;
                    image::Luma([((299 * p[0] as u32 + 587 * p[1] as u32 + 114 * p[2] as u32 + 500) / 1000) as u8])
                });
                resize_gray(&y, tw, th)
            } else {
                whole_decode(bytes, tw, th, GrayMath::ImageCrate).unwrap()
            };
            let d = mean_diff(&fast, &whole);
            assert!(d < 2.0, "{name} {tw}x{th}: mean difference {d}");
        }
    }

    fn big_png_rgba() -> Vec<u8> {
        encode(big_image(), ImageFormat::Png)
    }

    fn big_jpeg_rgb() -> Vec<u8> {
        encode(&DynamicImage::ImageRgb8(big_image().to_rgb8()), ImageFormat::Jpeg)
    }

    /// Adam7 interlaced gray+alpha (made with ImageMagick: vertical gray gradient,
    /// horizontal alpha gradient).
    fn adam7_bytes() -> Vec<u8> {
        let dir = std::env::var_os("INK_MANIFEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into());
        let bytes = std::fs::read(dir.join("tests/fixtures/adam7-ga.png")).unwrap();
        assert_eq!((bytes[25], bytes[28]), (4, 1), "gray+alpha, interlaced");
        assert!(intrinsic_size(&bytes).is_some_and(|(a, b)| a as u64 * b as u64 > BIG_SOURCE_PX));
        bytes
    }

    #[test]
    fn big_png_matches_a_whole_decode() {
        let png = big_png_rgba();
        // One target that grows x and shrinks y.
        check_big("png rgba", &png, &[(2400, 1000)]);
        // A corrupt big source gives None, not a panic.
        assert!(decode_scaled(&png[..png.len() / 2], 100, 100, None).is_none());
    }

    #[test]
    fn big_jpeg_matches_a_whole_decode() {
        let jpg = big_jpeg_rgb();
        check_big("jpeg rgb", &jpg, &[(210, 200)]);
        let _ = decode_scaled(&jpg[..jpg.len() / 2], 100, 100, None);
    }

    #[test]
    fn big_adam7_matches_a_whole_decode() {
        check_big("adam7", &adam7_bytes(), &[(210, 210)]);
    }

    #[test]
    #[ignore = "slow: run with --ignored before a release"]
    fn big_sources_match_a_whole_decode_all_formats() {
        let img = big_image();
        check_big("png rgba", &big_png_rgba(), &[(210, 200), (2100, 2400), (2400, 1000)]);
        let both = [(210, 200), (2100, 2400)];
        check_big("jpeg rgb", &big_jpeg_rgb(), &both);
        check_big("png gray+alpha", &encode(&DynamicImage::ImageLumaA8(img.to_luma_alpha8()), ImageFormat::Png), &both);
        check_big("png rgb", &encode(&DynamicImage::ImageRgb8(img.to_rgb8()), ImageFormat::Png), &both);
        check_big("png gray", &encode(&DynamicImage::ImageLuma8(img.to_luma8()), ImageFormat::Png), &both);
        check_big("jpeg gray", &encode(&DynamicImage::ImageLuma8(img.to_luma8()), ImageFormat::Jpeg), &both);
        check_big("adam7", &adam7_bytes(), &[(210, 210), (1000, 700), (2100, 2400)]);
    }

    #[test]
    fn fixed_point_resize() {
        // A flat image stays flat at every scale, including 1-pixel sources and targets.
        let flat = GrayImage::from_pixel(37, 23, image::Luma([200]));
        for (w, h) in [(37, 23), (10, 7), (1, 1), (100, 60), (37, 1)] {
            let r = resize_gray(&flat, w, h);
            assert_eq!(r.dimensions(), (w, h));
            assert!(r.pixels().all(|p| p.0[0] == 200), "{w}x{h}");
        }
        let one = GrayImage::from_pixel(1, 1, image::Luma([9]));
        assert!(resize_gray(&one, 5, 3).pixels().all(|p| p.0[0] == 9));
        // An empty source gives white paper, not a panic.
        let empty = resize_gray(&GrayImage::new(0, 0), 5, 3);
        assert!(empty.dimensions() == (5, 3) && empty.pixels().all(|p| p.0[0] == 255));
        // Halving averages alternating 0/255 columns to mid gray (inside the image; at the
        // borders the filter is cut and renormalized).
        let stripes = GrayImage::from_fn(64, 4, |x, _| image::Luma([if x % 2 == 0 { 0 } else { 255 }]));
        let r = resize_gray(&stripes, 32, 4);
        assert!((1..31).all(|x| (r.get_pixel(x, 2).0[0] as i32 - 128).abs() <= 1), "{:?}", r.as_raw());
        // A left-to-right ramp keeps its order after scaling.
        let ramp = GrayImage::from_fn(256, 2, |x, _| image::Luma([x as u8]));
        let r = resize_gray(&ramp, 50, 2);
        let row: Vec<u8> = (0..50).map(|x| r.get_pixel(x, 0).0[0]).collect();
        assert!(row.windows(2).all(|w| w[0] <= w[1]), "{row:?}");
        assert!(row[0] < 8 && row[49] > 247, "{row:?}");
    }

    #[test]
    fn thumbnails_match_the_image_crate_gray_within_rounding() {
        let px = |x: u32, y: u32| Rgba([(x * 4) as u8, (y * 5) as u8, ((x * y) % 256) as u8, (255 - y * 3) as u8]);
        let rgba = RgbaImage::from_fn(64, 48, px);
        let img = DynamicImage::ImageRgba8(rgba);
        let sources = [
            ("png rgba", encode(&img, ImageFormat::Png), 1),
            ("png rgb", encode(&DynamicImage::ImageRgb8(img.to_rgb8()), ImageFormat::Png), 1),
            ("jpeg rgb", encode(&DynamicImage::ImageRgb8(img.to_rgb8()), ImageFormat::Jpeg), 1),
            ("png gray", encode(&DynamicImage::ImageLuma8(img.to_luma8()), ImageFormat::Png), 0),
            ("png gray+alpha", encode(&DynamicImage::ImageLumaA8(img.to_luma_alpha8()), ImageFormat::Png), 0),
        ];
        for (name, bytes, most) in sources {
            // At the source size (no resize to blur the difference) and scaled down.
            for (w, h) in [(64, 48), (20, 15)] {
                let exact = decode_scaled(&bytes, w, h, None).unwrap();
                let fast = decode_thumbnail(&bytes, w, h).unwrap();
                let worst = exact.as_raw().iter().zip(fast.as_raw()).map(|(&a, &b)| a.abs_diff(b)).max();
                assert!(worst <= Some(most), "{name} {w}x{h}: {worst:?}");
            }
        }
    }

    /// A 40 x 60 gray PNG (a diagonal ramp with a dark frame), served as `a.png`.
    fn svg_tree(markup: &str) -> usvg::Tree {
        let px = GrayImage::from_fn(40, 60, |x, y| image::Luma([if x < 2 || y < 2 { 0 } else { (x * 3 + y * 2) as u8 }]));
        let png = encode(&DynamicImage::ImageLuma8(px), ImageFormat::Png);
        let res: HrefResolver<'static> = Arc::new(move |href: &str| (href == "a.png").then(|| png.clone()));
        let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" {markup}</svg>"#);
        usvg::Tree::from_data(svg.as_bytes(), &svg_options(Some(res))).unwrap()
    }

    #[test]
    fn single_image_svgs_decode_the_image_where_resvg_draws_it() {
        let cases = [
            // The KF8 cover flow (Calibre): percent sizes, a viewBox, the image filling it.
            (r#"width="100%" height="100%" viewBox="0 0 40 60" preserveAspectRatio="xMidYMid meet"><image width="40" height="60" xlink:href="a.png"/>"#, 80, 120),
            (r#"width="40" height="60"><image href="a.png"/>"#, 40, 60),
            (r#"viewBox="0 0 100 60"><image width="100" height="60" href="a.png"/>"#, 200, 120),
            (r#"viewBox="0 0 100 60"><image width="100" height="60" preserveAspectRatio="xMinYMid meet" href="a.png"/>"#, 100, 60),
            (r#"viewBox="0 0 100 60"><image width="100" height="60" preserveAspectRatio="xMidYMid slice" href="a.png"/>"#, 100, 60),
            (r#"viewBox="0 0 40 60"><title>Cover</title><g transform="translate(5 10)"><image width="30" height="40" href="a.png"/></g>"#, 80, 120),
            (r#"viewBox="0 0 40 60"><image width="40" height="60" preserveAspectRatio="none" href="a.png"/>"#, 120, 90),
        ];
        for (markup, w, h) in cases {
            let tree = svg_tree(markup);
            let fast = svg_raster_only(&tree, w, h).unwrap_or_else(|| panic!("not a single image: {markup}"));
            let d = mean_diff(&fast, &render_svg_tree(&tree, w, h));
            assert!(d < 1.5, "{markup}: mean difference {d}");
        }
    }

    #[test]
    fn svgs_with_more_than_one_plain_image_go_to_resvg() {
        for markup in [
            r#"viewBox="0 0 40 60"><image width="40" height="60" href="a.png"/><rect width="5" height="5"/>"#,
            r#"viewBox="0 0 40 60"><image width="40" height="60" href="a.png"/><image width="10" height="10" href="a.png"/>"#,
            r#"viewBox="0 0 40 60"><image width="40" height="60" opacity="0.5" href="a.png"/>"#,
            r#"viewBox="0 0 40 60"><g transform="rotate(10)"><image width="40" height="60" href="a.png"/></g>"#,
            r#"viewBox="0 0 40 60"><clipPath id="c"><circle r="20"/></clipPath><image clip-path="url(#c)" width="40" height="60" href="a.png"/>"#,
            r#"viewBox="0 0 40 60"><image width="40" height="60" href="missing.png"/>"#,
        ] {
            assert!(svg_raster_only(&svg_tree(markup), 80, 120).is_none(), "{markup}");
        }
    }
}
