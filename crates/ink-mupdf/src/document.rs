//! [`MupdfDocument`]: a PDF, CBZ or XPS file as an `ink_doc::Document`.
//!
//! # Screens and pages
//!
//! `Document` page indices are *screens*: what one `render_page` call shows. Positions
//! ([`Position::Page`]) always name the *document* page (0-based) plus optional quads in
//! page space, so they survive every option change.
//!
//! - Fit page, zoom 1, one column (the default): one screen per page.
//! - Fit width, zoom above 1 or columns: each page is cut into `columns x rows x cols`
//!   tiles, the same count for every page (taken from up to three sample pages, so the
//!   screen count is known at once and stays stable). A page that would need more tiles
//!   is scaled down a little to fit; tiles of a smaller page overlap more. Tiles go column
//!   by column, top to bottom, left to right inside a row. A page has at most
//!   [`MAX_TILES_PER_PAGE`] tiles ([`MAX_TILES_AXIS`] across or down) and a document at
//!   most [`MAX_SCREENS`] screens; a plan that needs more is shown at a lower zoom.
//! - Spreads (CBZ in landscape with [`crate::Spreads::Auto`], any document with
//!   `Always`): the cover alone, then two pages side by side, meeting at the middle;
//!   right to left when `rtl` is set. Only with fit page, zoom 1 and one column.
//!
//! Each tile owns a *cell*: its column region cut into an even `nx` x `ny` grid. Cells do
//! not overlap, and a tile always shows its whole cell (see
//! [`MupdfDocument::tile_cell`]). `position_of_page(screen)` returns the page plus, for a
//! tile, its cell as one quad; `page_of` maps a position to the tile whose cell holds the
//! centre of its first quad (the page's first screen without quads). So
//! `page_of(position_of_page(s)) == s` in every mode, also where tiles overlap or a short
//! page shows the same view on several tiles, and an option change that keeps the grid
//! (contrast, gamma) keeps the tile. Search hits, TOC entries and links use the same rule.
//!
//! # Caches
//!
//! - rendered screens: [`ByteCache`], default 16 MiB (`INK_MUPDF_CACHE_MB`);
//! - page text: 4 MiB;
//! - display lists: the last 4 pages;
//! - MuPDF's own store (fonts, decoded images): 48 MiB (`INK_MUPDF_STORE_MB`);
//! - page bounds, crop boxes and column cuts: small per-page maps.

use crate::analyze::{self, Gray};
use crate::cache::ByteCache;
use crate::geom::{FRect, Matrix};
use crate::raw::{RawDoc, RawOutline};
use crate::text::PageText;
use crate::{Fit, FixedOptions, Spreads};
use ink_device::{Canvas, Point, Rect};
use ink_doc::{
    DocError, DocKind, Document, Link, LinkTarget, LayoutSettings, Metadata, PageCount, PagePosition, Position,
    Result, SearchHit, Selection, TocEntry,
};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// MuPDF store cap (fonts, images) unless `INK_MUPDF_STORE_MB` says otherwise.
pub const DEFAULT_STORE_MB: usize = 48;
/// Rendered-screen cache cap unless `INK_MUPDF_CACHE_MB` says otherwise.
pub const DEFAULT_CACHE_MB: usize = 16;
/// Page text cache cap.
pub const TEXT_CACHE_BYTES: usize = 4 << 20;
/// Display lists kept.
pub const DISPLAY_LISTS: usize = 4;
/// Longest side of the gray image used for crop and column detection.
const ANALYZE_PX: f32 = 600.0;
/// Most tiles one page is cut into (columns x across x down). A plan that would need more
/// is scaled down until it fits (the zoom shown is then lower than asked).
pub const MAX_TILES_PER_PAGE: usize = 256;
/// Most tiles across or down one column.
pub const MAX_TILES_AXIS: usize = 64;
/// Most screens a document has (pages x tiles per page): far below `u32::MAX`, the size of
/// `usize` on the Kindle's 32-bit ARM.
pub const MAX_SCREENS: usize = 1 << 30;
/// Free zoom range of [`MupdfDocument::render_page_at`] in pixels per point.
pub const FREE_ZOOM_RANGE: (f32, f32) = (0.01, 64.0);
/// Largest pan offset of [`MupdfDocument::render_page_at`] in pixels.
pub const MAX_PAN: f32 = 1e7;

/// What kind of file is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// PDF.
    Pdf,
    /// Comic book archive (images in a zip).
    Cbz,
    /// XPS / OpenXPS.
    Xps,
    /// Anything else MuPDF opened.
    Other,
}

/// How screens map onto pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// `per_page = cols * nx * ny` tiles per page.
    Tiles {
        /// Columns of text per page (reading order).
        cols: usize,
        /// Tiles across one column.
        nx: usize,
        /// Tiles down one column.
        ny: usize,
    },
    /// Two pages per screen.
    Spread {
        /// Page 0 alone on the first screen.
        cover_alone: bool,
        /// First page of a pair on the right.
        rtl: bool,
    },
}

impl Plan {
    /// Screens per page in tile mode (1 for spreads).
    pub fn per_page(&self) -> usize {
        match *self {
            Plan::Tiles { cols, nx, ny } => cols * nx * ny,
            Plan::Spread { .. } => 1,
        }
    }
}

/// One page drawn on a screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    /// Document page.
    pub page: usize,
    /// Page space to screen pixels.
    pub ctm: Matrix,
    /// Screen pixels this piece may paint (white elsewhere).
    pub clip: Rect,
    /// The part of the page meant for this piece (page space).
    pub region: FRect,
}

impl Piece {
    /// The screen pixels of page-space rectangle `r` on this piece, `None` when it shows none.
    fn screen_rect(&self, r: &FRect) -> Option<Rect> {
        Some(self.ctm.map_rect(r).to_rect().intersect(&self.clip)).filter(|r| !r.is_empty())
    }
}

/// Free zoom and pan of one page: see [`MupdfDocument::render_page_at`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderSpec {
    /// Pixels per point.
    pub zoom: f32,
    /// Clockwise rotation: 0, 90, 180 or 270.
    pub rotate: u16,
    /// Left edge of the canvas inside the rotated, scaled page (pixels; negative = margin).
    pub x: f32,
    /// Top edge of the canvas inside the rotated, scaled page.
    pub y: f32,
    /// Start from the crop box instead of the page (`x`, `y` relative to it).
    pub crop: bool,
}

#[derive(Clone, Debug)]
struct PageGeom {
    content: FRect,
    regions: Vec<FRect>,
}

#[derive(Clone, Copy, Debug)]
struct Setup {
    opts: FixedOptions,
    plan: Plan,
    w: f32,
    h: f32,
    pages: usize,
}

struct Inner {
    raw: RawDoc,
    bounds: HashMap<usize, FRect>,
    /// Keyed by (page, crop, columns).
    geom: HashMap<(usize, bool, u8), PageGeom>,
    screens: ByteCache<usize, Vec<u8>>,
    texts: ByteCache<usize, PageText>,
}

/// An open PDF/CBZ/XPS document.
pub struct MupdfDocument {
    path: PathBuf,
    kind: FileKind,
    meta: Metadata,
    /// Read on first use: opening for metadata or a cover (the library) never needs it.
    outline: OnceCell<Vec<RawOutline>>,
    pages: usize,
    layout: LayoutSettings,
    opts: FixedOptions,
    plan: Plan,
    current: usize,
    selection: Option<Selection>,
    lut: Option<Box<[u8; 256]>>,
    inner: RefCell<Inner>,
}

fn env_mb(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(default)
}

/// The page size used when MuPDF reports none (US Letter).
const LETTER: FRect = FRect::new(0.0, 0.0, 612.0, 792.0);

/// `Ok` when page or screen `i` is below `n`, else [`DocError::PageOutOfRange`].
fn in_range(i: usize, n: usize) -> Result<()> {
    if i < n {
        Ok(())
    } else {
        Err(DocError::PageOutOfRange(i))
    }
}

/// A position on document page `page` covering the page-space rectangles `rects`.
fn page_position<'a>(page: usize, rects: impl IntoIterator<Item = &'a FRect>) -> Position {
    Position::Page(PagePosition { page: page as u32, quads: rects.into_iter().map(FRect::to_quad).collect() })
}

/// The position of a TOC or link target point: a zero-size quad, none for the top-left
/// corner (the target is the whole page).
fn target_position(page: usize, x: f32, y: f32) -> Position {
    page_position(page, Some(&FRect::new(x, y, x, y)).filter(|_| x > 0.0 || y > 0.0))
}

/// Tiles needed along one axis for content `ratio` screens long (2 % slack), at most
/// [`MAX_TILES_AXIS`]. An infinite ratio needs the most; NaN (broken geometry) one.
fn tiles(ratio: f32) -> usize {
    if ratio.is_nan() || ratio <= 1.02 {
        1
    } else if ratio >= MAX_TILES_AXIS as f32 {
        MAX_TILES_AXIS
    } else {
        ((ratio - 0.02).ceil() as usize).clamp(1, MAX_TILES_AXIS)
    }
}

/// Most tiles per page for a document of `pages` pages: [`MAX_TILES_PER_PAGE`], fewer when
/// `pages` x tiles would pass [`MAX_SCREENS`].
fn tile_cap(pages: usize) -> usize {
    MAX_TILES_PER_PAGE.min(MAX_SCREENS / pages.max(1)).max(1)
}

/// Index of the `n` equal parts of `[0, len)` that holds `off` (clamped to the ends).
fn part_index(off: f32, len: f32, n: usize) -> usize {
    if n <= 1 || len.is_nan() || len <= 0.0 {
        return 0;
    }
    let k = (off / len * n as f32).floor();
    if k.is_nan() || k <= 0.0 {
        0
    } else {
        (k as usize).min(n - 1)
    }
}

/// Scale that fits a `rw` x `rh` region into a `w` x `h` screen.
fn fit_scale(fit: Fit, w: f32, h: f32, rw: f32, rh: f32) -> f32 {
    match fit {
        Fit::Page => (w / rw).min(h / rh),
        Fit::Width => w / rw,
    }
}

/// Page space to screen: rotate by `rot`, scale, and put the top-left corner of `rr`
/// (the region already rotated) at `(left, top)`.
fn place(rot: &Matrix, rr: &FRect, scale: f32, left: f32, top: f32) -> Matrix {
    rot.then(&Matrix::scale(scale)).then(&Matrix::translate(left - rr.x0 * scale, top - rr.y0 * scale))
}

/// The screen pixels that `region` covers under `ctm`, inside `screen`.
fn clip_of(ctm: &Matrix, region: &FRect, screen: &FRect) -> Rect {
    ctm.map_rect(region).intersect(screen).to_rect().intersect(&screen.to_rect())
}

impl Setup {
    /// Screen `view` of a tile plan as (page, column, row, tile across), or `None` for
    /// spreads and screens past the end.
    fn tile(&self, view: usize) -> Option<(usize, usize, usize, usize)> {
        let Plan::Tiles { cols, nx, ny } = self.plan else {
            return None;
        };
        let per = cols * nx * ny;
        let page = view / per;
        if page >= self.pages {
            return None;
        }
        let (col, rem) = (view % per / (nx * ny), view % per % (nx * ny));
        Some((page, col, rem / nx, rem % nx))
    }
}

impl Inner {
    fn bound(&mut self, page: usize) -> FRect {
        if let Some(b) = self.bounds.get(&page) {
            return *b;
        }
        let b = self.raw.page_bound(page).inspect_err(|e| log::warn!("mupdf: {e}; using letter size")).ok();
        let b = b.filter(|b| !b.is_empty() && b.is_finite()).unwrap_or(LETTER);
        self.bounds.insert(page, b);
        b
    }

    /// Small gray rendering of the whole page and its scale (pixels per point).
    fn small(&mut self, page: usize) -> Result<(Gray, f32, FRect)> {
        let b = self.bound(page);
        let k = ANALYZE_PX / b.w().max(b.h()).max(1.0);
        let w = (b.w() * k).ceil().max(1.0) as usize;
        let h = (b.h() * k).ceil().max(1.0) as usize;
        let ctm = Matrix::translate(-b.x0, -b.y0).then(&Matrix::scale(k));
        let mut px = vec![255u8; w * h];
        self.raw.render(page, &ctm, Rect::new(0, 0, w as u32, h as u32), w, &mut px)?;
        Ok((Gray { w, h, px }, k, b))
    }

    /// Crop box and column regions of `page` for `opts`, analysed on first use.
    fn geom(&mut self, page: usize, opts: &FixedOptions) -> &PageGeom {
        let key = (page, opts.crop_margins, opts.columns.max(1));
        if !self.geom.contains_key(&key) {
            let g = self.analyse(page, opts);
            self.geom.insert(key, g);
        }
        &self.geom[&key]
    }

    fn analyse(&mut self, page: usize, opts: &FixedOptions) -> PageGeom {
        let b = self.bound(page);
        let cols = opts.columns.max(1) as usize;
        if !opts.crop_margins && cols == 1 {
            return PageGeom { content: b, regions: vec![b] };
        }
        self.small(page).map(|(img, k, b)| analyze_geom(&img, k, b, opts.crop_margins, cols)).unwrap_or_else(|e| {
            log::warn!("mupdf: cannot analyse page {page}: {e}");
            PageGeom { content: b, regions: split_even(b, cols) }
        })
    }

    fn text(&mut self, page: usize) -> Result<PageText> {
        if let Some(t) = self.texts.get(&page) {
            return Ok(t.clone());
        }
        let t = PageText::new(page, self.raw.text(page)?);
        let bytes = t.bytes();
        self.texts.insert(page, t.clone(), bytes);
        Ok(t)
    }

    /// The part of `page` that text column `col` of a `cols`-column tile plan shows (the
    /// crop box when the plan has one column).
    fn tile_region(&mut self, s: &Setup, page: usize, cols: usize, col: usize) -> FRect {
        let g = self.geom(page, &s.opts);
        if cols <= 1 {
            g.content
        } else {
            g.regions.get(col).or(g.regions.last()).copied().unwrap_or(g.content)
        }
    }

    /// Document page and tile cell of screen `view` in a tile plan.
    ///
    /// Cells cut each column region into an even `nx` x `ny` grid (in the rotated frame, so
    /// rows run down the screen). They do not overlap, and the tile of a cell always shows
    /// all of it: tiles are at least one cell long and the last tile ends where the region
    /// ends. [`Inner::screen_at`] maps any point of a cell back to that tile.
    fn cell(&mut self, s: &Setup, view: usize) -> Option<(usize, FRect)> {
        let Plan::Tiles { cols, nx, ny } = s.plan else {
            return None;
        };
        let (page, col, row, xi) = s.tile(view)?;
        let region = self.tile_region(s, page, cols, col);
        let rot = Matrix::rotate(s.opts.rotate as i32);
        let rr = rot.map_rect(&region);
        let at = |a: f32, b: f32, k: usize, n: usize| if k >= n { b } else { a + (b - a) * k as f32 / n as f32 };
        let cell = FRect::new(at(rr.x0, rr.x1, xi, nx), at(rr.y0, rr.y1, row, ny), at(rr.x0, rr.x1, xi + 1, nx), at(rr.y0, rr.y1, row + 1, ny));
        Some((page, rot.invert().map_rect(&cell)))
    }

    /// The screen of `page` whose tile cell holds page point `(x, y)`: the column region
    /// that holds `x` (else the nearest one), then the cell of the even grid over it. A
    /// point outside the region goes to the nearest cell. O(columns), no tile scan.
    fn screen_at(&mut self, s: &Setup, page: usize, x: f32, y: f32) -> usize {
        let Plan::Tiles { cols, nx, ny } = s.plan else {
            return 0;
        };
        let per = cols * nx * ny;
        let first = page * per;
        if per <= 1 {
            return first;
        }
        let mut col = 0;
        if cols > 1 {
            let mut best = f32::INFINITY;
            for c in 0..cols {
                let r = self.tile_region(s, page, cols, c);
                let d = if x < r.x0 {
                    r.x0 - x
                } else if x >= r.x1 {
                    x - r.x1
                } else {
                    0.0
                };
                if d < best {
                    best = d;
                    col = c;
                }
            }
        }
        let region = self.tile_region(s, page, cols, col);
        let rot = Matrix::rotate(s.opts.rotate as i32);
        let rr = rot.map_rect(&region);
        let (px, py) = rot.apply(x, y);
        let xi = part_index(px - rr.x0, rr.w(), nx);
        let row = part_index(py - rr.y0, rr.h(), ny);
        first + col * nx * ny + row * nx + xi
    }

    /// The pieces that make up screen `view`.
    fn pieces(&mut self, s: &Setup, view: usize) -> Vec<Piece> {
        let screen = FRect::new(0.0, 0.0, s.w, s.h);
        let rot = Matrix::rotate(s.opts.rotate as i32);
        let mut out = Vec::new();
        match s.plan {
            Plan::Tiles { cols, nx, ny } => {
                let Some((page, col, row, xi)) = s.tile(view) else { return out };
                let region = self.tile_region(s, page, cols, col);
                let rr = rot.map_rect(&region);
                let (rw, rh) = (rr.w().max(1.0), rr.h().max(1.0));
                let base = fit_scale(s.opts.fit, s.w, s.h, rw, rh);
                let scale = (base * s.opts.zoom).min(nx as f32 * s.w / rw).min(ny as f32 * s.h / rh);
                let (cw, ch) = (rw * scale, rh * scale);
                let left = if cw <= s.w + 0.5 || nx < 2 { (s.w - cw) / 2.0 } else { -(xi as f32) * (cw - s.w) / (nx - 1) as f32 };
                let top = if ch <= s.h + 0.5 || ny < 2 { (s.h - ch) / 2.0 } else { -(row as f32) * (ch - s.h) / (ny - 1) as f32 };
                let ctm = place(&rot, &rr, scale, left, top);
                out.push(Piece { page, ctm, clip: clip_of(&ctm, &region, &screen), region });
            }
            Plan::Spread { cover_alone, rtl } => {
                let pages = spread_pages(view, s.pages, cover_alone);
                let n = pages.len();
                for (k, &page) in pages.iter().enumerate() {
                    let g = self.geom(page, &s.opts);
                    let region = g.content;
                    let rr = rot.map_rect(&region);
                    let (rw, rh) = (rr.w().max(1.0), rr.h().max(1.0));
                    let area_w = if n == 2 { s.w / 2.0 } else { s.w };
                    let scale = (area_w / rw).min(s.h / rh) * s.opts.zoom.min(1.0);
                    let (cw, ch) = (rw * scale, rh * scale);
                    let right_side = n == 2 && ((k == 1) != rtl);
                    let left = if n == 1 {
                        (s.w - cw) / 2.0
                    } else if right_side {
                        s.w / 2.0
                    } else {
                        s.w / 2.0 - cw
                    };
                    let ctm = place(&rot, &rr, scale, left, (s.h - ch) / 2.0);
                    out.push(Piece { page, ctm, clip: clip_of(&ctm, &region, &screen), region });
                }
            }
        }
        out
    }
}

/// Pages shown on spread screen `view`.
fn spread_pages(view: usize, pages: usize, cover_alone: bool) -> Vec<usize> {
    // First page and how many pages the screen holds (cover alone: page 0 by itself).
    let (a, n) = match (cover_alone, view) {
        (true, 0) => (0, 1),
        (true, v) => (2 * v - 1, 2),
        (false, v) => (2 * v, 2),
    };
    (a..a.saturating_add(n).min(pages)).collect()
}

fn split_even(b: FRect, cols: usize) -> Vec<FRect> {
    let cols = cols.max(1);
    let w = b.w() / cols as f32;
    (0..cols).map(|i| FRect::new(b.x0 + w * i as f32, b.y0, b.x0 + w * (i + 1) as f32, b.y1)).collect()
}

fn analyze_geom(img: &Gray, k: f32, b: FRect, crop: bool, cols: usize) -> PageGeom {
    let to_page_x = |x: usize| b.x0 + x as f32 / k;
    let to_page_y = |y: usize| b.y0 + y as f32 / k;
    let ink = analyze::content_box(img);
    let content = match (crop, ink) {
        (true, Some((x0, y0, x1, y1))) => {
            let pad = 0.012 * b.w().max(b.h());
            FRect::new(to_page_x(x0) - pad, to_page_y(y0) - pad, to_page_x(x1) + pad, to_page_y(y1) + pad).intersect(&b)
        }
        _ => b,
    };
    if cols < 2 {
        return PageGeom { content, regions: vec![content] };
    }
    let px_box = ink.unwrap_or((0, 0, img.w, img.h));
    let to_px = |x: f32| (((x - b.x0) * k).round().max(0.0) as usize).min(img.w);
    let span = (to_px(content.x0), to_px(content.x1));
    let mut edges = vec![content.x0];
    edges.extend(analyze::column_cuts(img, px_box, span, cols).into_iter().map(to_page_x).filter(|&c| c > content.x0 && c < content.x1));
    edges.push(content.x1);
    // Exactly `cols` distinct regions, left to right: without usable cuts (a page a few
    // pixels wide in the analysis image, a cut outside the crop box) the even split.
    if edges.len() != cols + 1 || edges.windows(2).any(|e| e[1] <= e[0]) {
        return PageGeom { content, regions: split_even(content, cols) };
    }
    let mut regions: Vec<FRect> = edges.windows(2).map(|e| FRect::new(e[0], content.y0, e[1], content.y1)).collect();
    if crop {
        // Tighten each column to its own ink horizontally (the gutter is not shown), but
        // never below half an even column (the rule column_cuts follows).
        let min_w = content.w() / (2 * cols) as f32;
        for r in regions.iter_mut() {
            let wide = *r;
            let x0 = ((r.x0 - b.x0) * k).floor().max(0.0) as usize;
            let x1 = (((r.x1 - b.x0) * k).ceil() as usize).min(img.w);
            let y0 = ((r.y0 - b.y0) * k).floor().max(0.0) as usize;
            let y1 = (((r.y1 - b.y0) * k).ceil() as usize).min(img.h);
            if x1 > x0 && y1 > y0 {
                let sub = Gray {
                    w: x1 - x0,
                    h: y1 - y0,
                    px: (y0..y1).flat_map(|y| img.px[y * img.w + x0..y * img.w + x1].iter().copied()).collect(),
                };
                if let Some((sx0, _, sx1, _)) = analyze::content_box(&sub) {
                    let pad = 0.006 * b.w().max(b.h());
                    r.x0 = (to_page_x(x0 + sx0) - pad).max(r.x0);
                    r.x1 = (to_page_x(x0 + sx1) + pad).min(r.x1);
                }
            }
            if r.w() < min_w.min(wide.w()) {
                *r = wide;
            }
        }
    }
    PageGeom { content, regions }
}

fn lut(contrast: f32, gamma: f32) -> Option<Box<[u8; 256]>> {
    if (contrast - 1.0).abs() < 1e-3 && (gamma - 1.0).abs() < 1e-3 {
        return None;
    }
    let mut t = Box::new([0u8; 256]);
    for (i, v) in t.iter_mut().enumerate() {
        let mut x = i as f32 / 255.0;
        x = x.powf(gamma.max(0.05));
        x = (x - 0.5) * contrast.max(0.0) + 0.5;
        *v = (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    Some(t)
}

impl MupdfDocument {
    /// Opens `path` with `opts`.
    pub fn open(path: &Path, opts: FixedOptions) -> Result<Self> {
        Self::open_with_password(path, opts, None)
    }

    /// Opens an encrypted document with `password` (an empty password is always tried).
    pub fn open_with_password(path: &Path, opts: FixedOptions, password: Option<&str>) -> Result<Self> {
        if !path.exists() {
            return Err(DocError::NotFound(path.display().to_string()));
        }
        // At least 1 MiB: 0 would make the shim fall back to MuPDF's 256 MiB default.
        let store = env_mb("INK_MUPDF_STORE_MB", DEFAULT_STORE_MB).max(1) << 20;
        let mut raw = RawDoc::open(path, store, DISPLAY_LISTS)?;
        if raw.needs_password() && !raw.authenticate("") && !password.is_some_and(|p| raw.authenticate(p)) {
            return Err(DocError::Unsupported(format!("{} is password-protected", path.display())));
        }
        let pages = raw.page_count()?;
        let format = raw.metadata("format").unwrap_or_default();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let kind = if format.starts_with("PDF") || ext == "pdf" {
            FileKind::Pdf
        } else if format.starts_with("CBZ") || ext == "cbz" || ext == "zip" {
            FileKind::Cbz
        } else if format.contains("XPS") || ext == "xps" || ext == "oxps" {
            FileKind::Xps
        } else {
            FileKind::Other
        };
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let title = raw.metadata("info:Title").unwrap_or(stem);
        let authors = raw
            .metadata("info:Author")
            .map(|a| a.split([';', '&']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        let meta = Metadata {
            title,
            authors,
            description: raw.metadata("info:Subject"),
            language: raw.metadata("info:Language"),
            ..Default::default()
        };
        let cache = env_mb("INK_MUPDF_CACHE_MB", DEFAULT_CACHE_MB) << 20;
        let opts = opts.normalized();
        let mut doc = MupdfDocument {
            path: path.to_path_buf(),
            kind,
            meta,
            outline: OnceCell::new(),
            pages,
            layout: LayoutSettings::for_screen(1272, 1696),
            opts,
            plan: Plan::Tiles { cols: 1, nx: 1, ny: 1 },
            current: 0,
            selection: None,
            lut: lut(opts.contrast, opts.gamma),
            inner: RefCell::new(Inner {
                raw,
                bounds: HashMap::new(),
                geom: HashMap::new(),
                screens: ByteCache::new(cache),
                texts: ByteCache::new(TEXT_CACHE_BYTES),
            }),
        };
        doc.replan();
        Ok(doc)
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// PDF, CBZ, XPS.
    pub fn file_kind(&self) -> FileKind {
        self.kind
    }

    /// Number of document pages (not screens).
    pub fn doc_pages(&self) -> usize {
        self.pages
    }

    /// Current options.
    pub fn options(&self) -> FixedOptions {
        self.opts
    }

    /// Changes the options; screens are recomputed (positions stay valid).
    pub fn set_options(&mut self, opts: FixedOptions) {
        let opts = opts.normalized();
        let pos = self.position_of_page(self.current).ok();
        self.opts = opts;
        self.lut = lut(opts.contrast, opts.gamma);
        self.inner.borrow_mut().screens.clear();
        self.replan();
        if let Some(p) = pos {
            self.current = self.page_of(&p).unwrap_or(0);
        }
        self.selection = None;
    }

    /// How screens map onto pages now.
    pub fn plan(&self) -> Plan {
        self.plan
    }

    fn setup(&self) -> Setup {
        Setup { opts: self.opts, plan: self.plan, w: self.layout.width as f32, h: self.layout.height as f32, pages: self.pages }
    }

    fn spreads_active(&self) -> bool {
        let landscape = self.layout.width > self.layout.height;
        let simple = self.opts.fit == Fit::Page && self.opts.zoom <= 1.0 && self.opts.columns <= 1;
        simple
            && match self.opts.spreads {
                Spreads::Never => false,
                Spreads::Always => true,
                Spreads::Auto => landscape && self.kind == FileKind::Cbz,
            }
    }

    fn replan(&mut self) {
        self.plan = if self.spreads_active() {
            Plan::Spread { cover_alone: self.opts.cover_alone, rtl: self.opts.rtl }
        } else {
            let cols = self.opts.columns.max(1) as usize;
            let cap = tile_cap(self.pages);
            if (self.opts.fit == Fit::Page && self.opts.zoom <= 1.0 && cols == 1) || cols > cap {
                // `cols > cap`: so many pages that even one tile per column would pass
                // MAX_SCREENS; show whole (cropped) pages.
                Plan::Tiles { cols: 1, nx: 1, ny: 1 }
            } else {
                let (w, h) = (self.layout.width as f32, self.layout.height as f32);
                let rot = Matrix::rotate(self.opts.rotate as i32);
                let mut samples = vec![0, 1, self.pages / 2];
                samples.retain(|&p| p < self.pages);
                samples.dedup();
                // Screens needed across and down by the widest/tallest sampled column.
                let (mut fx, mut fy) = (0.0f32, 0.0f32);
                let mut inner = self.inner.borrow_mut();
                for p in samples {
                    let g = inner.geom(p, &self.opts);
                    for r in g.regions.iter().take(cols) {
                        let rr = rot.map_rect(r);
                        let (rw, rh) = (rr.w().max(1.0), rr.h().max(1.0));
                        let s = fit_scale(self.opts.fit, w, h, rw, rh) * self.opts.zoom;
                        fx = fx.max(rw * s / w);
                        fy = fy.max(rh * s / h);
                    }
                }
                let (fx, fy) = (fx.min(MAX_TILES_AXIS as f32), fy.min(MAX_TILES_AXIS as f32));
                // Too many tiles for one page: shrink the zoom by 10 % steps until they fit
                // (pieces() then scales every page down to the tile grid).
                let (mut nx, mut ny) = (tiles(fx), tiles(fy));
                let mut m = 1.0f32;
                while cols * nx * ny > cap && (nx > 1 || ny > 1) {
                    m *= 0.9;
                    nx = tiles(fx * m);
                    ny = tiles(fy * m);
                }
                Plan::Tiles { cols, nx, ny }
            }
        };
    }

    /// Number of screens for the current layout and options.
    pub fn screens(&self) -> usize {
        match self.plan {
            // At most MAX_SCREENS (tile_cap); saturating in case MuPDF reports absurd counts.
            Plan::Tiles { .. } => self.pages.saturating_mul(self.plan.per_page()),
            // One past the screen of the last page.
            Plan::Spread { .. } => self.pages.checked_sub(1).map_or(0, |last| self.first_screen_of(last) + 1),
        }
    }

    /// Document pages shown on `screen`.
    pub fn pages_of_screen(&self, screen: usize) -> Vec<usize> {
        match self.plan {
            Plan::Tiles { .. } => Some(screen / self.plan.per_page()).filter(|&p| p < self.pages).into_iter().collect(),
            Plan::Spread { cover_alone, .. } => spread_pages(screen, self.pages, cover_alone),
        }
    }

    /// First screen that shows document page `page`.
    pub fn first_screen_of(&self, page: usize) -> usize {
        match self.plan {
            Plan::Tiles { .. } => page * self.plan.per_page(),
            Plan::Spread { cover_alone: true, .. } => page.div_ceil(2),
            Plan::Spread { cover_alone: false, .. } => page / 2,
        }
    }

    /// The pieces (pages, transforms, clip rectangles) of `screen`.
    pub fn pieces(&self, screen: usize) -> Vec<Piece> {
        let s = self.setup();
        self.inner.borrow_mut().pieces(&s, screen)
    }

    /// The screen of `page` that shows page-space point `(x, y)`: in tile modes the tile
    /// whose cell holds it (see [`MupdfDocument::tile_cell`]); a point outside the page's
    /// columns goes to the nearest cell. Cost: O(columns), whatever the tile count.
    pub fn screen_showing(&self, page: usize, x: f32, y: f32) -> usize {
        if self.plan.per_page() <= 1 || page >= self.pages {
            return self.first_screen_of(page);
        }
        let s = self.setup();
        self.inner.borrow_mut().screen_at(&s, page, x, y)
    }

    /// In tile modes: the document page of `screen` and the cell of it that this tile is
    /// responsible for (page space). Cells split each column region into an even grid of
    /// `nx` x `ny`; they do not overlap and the tile shows all of its cell, so
    /// `screen_showing` on any point of the cell returns `screen`. `None` for spreads and
    /// out-of-range screens.
    pub fn tile_cell(&self, screen: usize) -> Option<(usize, FRect)> {
        if screen >= self.screens() {
            return None;
        }
        let s = self.setup();
        self.inner.borrow_mut().cell(&s, screen)
    }

    /// Size of document page `page` in points (after its own `/Rotate`).
    pub fn page_bounds(&self, page: usize) -> Result<FRect> {
        in_range(page, self.pages)?;
        Ok(self.inner.borrow_mut().bound(page))
    }

    /// The crop box of `page` (the whole page when crop is off).
    pub fn content_box(&self, page: usize) -> Result<FRect> {
        in_range(page, self.pages)?;
        let opts = self.opts;
        Ok(self.inner.borrow_mut().geom(page, &opts).content)
    }

    /// The column regions of `page` (one region when columns are off).
    pub fn columns(&self, page: usize) -> Result<Vec<FRect>> {
        in_range(page, self.pages)?;
        let opts = self.opts;
        Ok(self.inner.borrow_mut().geom(page, &opts).regions.clone())
    }

    /// The text of document page `page`.
    pub fn page_text(&self, page: usize) -> Result<PageText> {
        in_range(page, self.pages)?;
        self.inner.borrow_mut().text(page)
    }

    /// Raw outline entries (document pages), read from the file on the first call.
    pub fn outline(&self) -> &[RawOutline] {
        self.outline.get_or_init(|| {
            self.inner.borrow_mut().raw.outline().unwrap_or_else(|e| {
                log::warn!("mupdf: {e}");
                Vec::new()
            })
        })
    }

    /// Bytes held by the rendered-screen cache and its cap.
    pub fn cache_usage(&self) -> (usize, usize) {
        let inner = self.inner.borrow();
        (inner.screens.used(), inner.screens.cap())
    }

    /// Screens evicted from the cache so far.
    pub fn cache_evictions(&self) -> u64 {
        self.inner.borrow().screens.evictions()
    }

    /// True when `screen` is in the rendered-screen cache.
    pub fn is_cached(&self, screen: usize) -> bool {
        self.inner.borrow().screens.contains(&screen)
    }

    /// Changes the rendered-screen cache cap.
    pub fn set_cache_cap(&mut self, bytes: usize) {
        self.inner.borrow_mut().screens.set_cap(bytes);
    }

    /// MuPDF warnings so far (a damaged file repairs itself with warnings).
    pub fn warnings(&self) -> usize {
        self.inner.borrow().raw.warnings()
    }

    /// Display lists held now (at most [`DISPLAY_LISTS`]).
    pub fn display_lists(&self) -> usize {
        self.inner.borrow().raw.cached_lists()
    }

    /// Draws `page` with `ctm` into the rectangle `clip` of `dst`, an image `dst_w`
    /// pixels wide, and applies the contrast and gamma table. `clip` lies inside `dst`.
    fn render_into(&self, page: usize, ctm: &Matrix, clip: Rect, dst: &mut [u8], dst_w: usize) -> Result<()> {
        // Drawn in place, rows `dst_w` apart; the shim clears the clip to white first.
        let at = clip.y as usize * dst_w + clip.x as usize;
        self.inner.borrow_mut().raw.render(page, ctm, clip, dst_w, &mut dst[at..])?;
        if let Some(t) = &self.lut {
            for row in dst[at..].chunks_mut(dst_w).take(clip.h as usize) {
                for p in &mut row[..clip.w as usize] {
                    *p = t[*p as usize];
                }
            }
        }
        Ok(())
    }

    /// Renders `screen` into a new `w * h` buffer (no cache).
    fn draw_screen(&self, screen: usize) -> Result<Vec<u8>> {
        let s = self.setup();
        let (w, h) = (self.layout.width as usize, self.layout.height as usize);
        let mut buf = vec![255u8; w * h];
        let pieces = self.inner.borrow_mut().pieces(&s, screen);
        if pieces.is_empty() {
            return Err(DocError::PageOutOfRange(screen));
        }
        for p in pieces.iter().filter(|p| !p.clip.is_empty()) {
            self.render_into(p.page, &p.ctm, p.clip, &mut buf, w)?;
        }
        Ok(buf)
    }

    /// Calls `f` with the pixels of `screen`, from the cache or drawn (and cached) now,
    /// without copying them.
    fn with_screen<R>(&self, screen: usize, f: impl FnOnce(&[u8]) -> R) -> Result<R> {
        if let Some(px) = self.inner.borrow_mut().screens.get(&screen) {
            return Ok(f(px));
        }
        let px = self.draw_screen(screen)?;
        let r = f(&px);
        let bytes = px.len();
        self.inner.borrow_mut().screens.insert(screen, px, bytes);
        Ok(r)
    }

    /// Renders `screen` into the cache without showing it (call after a page turn for the
    /// next screen).
    pub fn prefetch(&self, screen: usize) -> Result<()> {
        if screen >= self.screens() || self.is_cached(screen) {
            return Ok(());
        }
        self.with_screen(screen, |_| ())
    }

    /// Free zoom and pan: draws document page `page` scaled by `spec.zoom` pixels per
    /// point and rotated, with the canvas' top-left corner at `(spec.x, spec.y)` of the
    /// rotated, scaled page (or crop box). Contrast and gamma apply.
    ///
    /// A zoom that is not a positive finite number, or a NaN or infinite pan, is an error
    /// (it typically comes from a 0/0 in gesture code). The zoom is clamped to
    /// [`FREE_ZOOM_RANGE`] and the pan to +-[`MAX_PAN`] pixels.
    pub fn render_page_at(&self, page: usize, spec: &RenderSpec, canvas: &mut Canvas) -> Result<()> {
        in_range(page, self.pages)?;
        if !(spec.zoom.is_finite() && spec.zoom > 0.0 && spec.x.is_finite() && spec.y.is_finite()) {
            return Err(DocError::Format(format!(
                "render_page_at: zoom {} and pan ({}, {}) must be finite, zoom above 0",
                spec.zoom, spec.x, spec.y
            )));
        }
        let (zx, zy) = (spec.x.clamp(-MAX_PAN, MAX_PAN), spec.y.clamp(-MAX_PAN, MAX_PAN));
        let region = if spec.crop {
            let opts = FixedOptions { crop_margins: true, columns: 1, ..self.opts };
            self.inner.borrow_mut().geom(page, &opts).content
        } else {
            self.inner.borrow_mut().bound(page)
        };
        let rot = Matrix::rotate(spec.rotate as i32);
        let z = spec.zoom.clamp(FREE_ZOOM_RANGE.0, FREE_ZOOM_RANGE.1);
        let rr = rot.map_rect(&region);
        let ctm = place(&rot, &rr, z, -zx, -zy);
        let (w, h) = (canvas.width, canvas.height);
        let canvas_rect = Rect::new(0, 0, w.min(i32::MAX as usize) as u32, h.min(i32::MAX as usize) as u32);
        let clip = ctm.map_rect(&region).intersect(&FRect::new(0.0, 0.0, w as f32, h as f32)).to_rect().intersect(&canvas_rect);
        canvas.clear(255);
        if clip.is_empty() || canvas.pixels.len() < w * h {
            return Ok(());
        }
        self.render_into(page, &ctm, clip, &mut canvas.pixels, w)
    }

    /// A thumbnail of the first page (the cover) that fits in `max_w` x `max_h`.
    pub fn thumbnail(&self, max_w: u32, max_h: u32) -> Result<Canvas> {
        if self.pages == 0 {
            return Err(DocError::Format("no pages".into()));
        }
        let b = self.inner.borrow_mut().bound(0);
        let z = (max_w as f32 / b.w().max(1.0)).min(max_h as f32 / b.h().max(1.0));
        let w = ((b.w() * z).round() as usize).clamp(1, max_w.max(1) as usize);
        let h = ((b.h() * z).round() as usize).clamp(1, max_h.max(1) as usize);
        let mut c = Canvas::new(w, h);
        let ctm = Matrix::translate(-b.x0, -b.y0).then(&Matrix::scale(z));
        self.inner.borrow_mut().raw.render(0, &ctm, Rect::new(0, 0, w as u32, h as u32), w, &mut c.pixels)?;
        Ok(c)
    }

    /// Screen rectangles of the quads of `pos` on `screen` (empty when that page is not on
    /// that screen). Use it to draw stored highlights at any zoom.
    pub fn highlight_rects(&self, screen: usize, pos: &PagePosition) -> Vec<Rect> {
        let pieces = self.pieces(screen).into_iter().filter(|p| p.page == pos.page as usize);
        pieces.flat_map(|p| pos.quads.iter().filter_map(move |q| p.screen_rect(&FRect::from_quad(q)))).collect()
    }

    /// The text under the quads of `pos`.
    pub fn text_at(&self, pos: &PagePosition) -> Result<String> {
        let t = self.page_text(pos.page as usize)?;
        let rects: Vec<FRect> = pos.quads.iter().map(FRect::from_quad).collect();
        Ok(t.range_in(&rects).map(|r| t.text_of(r)).unwrap_or_default())
    }

    fn page_pos<'a>(&self, pos: &'a Position) -> Result<&'a PagePosition> {
        match pos {
            Position::Page(p) if (p.page as usize) < self.pages => Ok(p),
            Position::Page(p) => Err(DocError::PageOutOfRange(p.page as usize)),
            other => Err(DocError::BadPosition(format!("{other} is not a page position"))),
        }
    }
}

impl Document for MupdfDocument {
    fn metadata(&self) -> &Metadata {
        &self.meta
    }

    fn kind(&self) -> DocKind {
        DocKind::Fixed
    }

    fn set_layout(&mut self, settings: &LayoutSettings) -> Result<()> {
        if settings.width == 0 || settings.height == 0 {
            return Err(DocError::Format("empty layout size".into()));
        }
        let pos = self.position_of_page(self.current).ok();
        self.layout = settings.clone();
        self.inner.borrow_mut().screens.clear();
        self.replan();
        self.current = pos.and_then(|p| self.page_of(&p).ok()).unwrap_or(0).min(self.screens().saturating_sub(1));
        self.selection = None;
        Ok(())
    }

    fn zoom_step(&mut self, steps: i32) -> Option<f32> {
        // Steps of the ladder; an odd current zoom (from `RINKY_FIXED`) counts as the
        // nearest rung below it.
        const LADDER: [f32; 7] = [0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0];
        let mut opts = self.options();
        let at = LADDER.iter().rposition(|z| *z <= opts.zoom + 0.001).unwrap_or(0) as i32;
        let to = (at + steps).clamp(0, LADDER.len() as i32 - 1) as usize;
        if to as i32 == at && (LADDER[to] - opts.zoom).abs() < 0.001 {
            return None;
        }
        opts.zoom = LADDER[to];
        self.set_options(opts);
        Some(LADDER[to])
    }

    fn layout(&self) -> &LayoutSettings {
        &self.layout
    }

    fn page_count(&self) -> PageCount {
        PageCount { pages: self.screens(), complete: true }
    }

    fn render_page(&mut self, page: usize, canvas: &mut Canvas) -> Result<()> {
        in_range(page, self.screens())?;
        let (w, h) = (self.layout.width as usize, self.layout.height as usize);
        self.with_screen(page, |px| {
            if canvas.width == w && canvas.height == h {
                canvas.pixels.copy_from_slice(px);
            } else {
                canvas.clear(255);
                let n = w.min(canvas.width);
                if n > 0 {
                    for (dst, src) in canvas.pixels.chunks_exact_mut(canvas.width).zip(px.chunks_exact(w)) {
                        dst[..n].copy_from_slice(&src[..n]);
                    }
                }
            }
        })
    }

    fn position_of_page(&self, page: usize) -> Result<Position> {
        in_range(page, self.screens())?;
        if self.plan.per_page() > 1 {
            // The tile's cell: page_of maps its centre back to this very tile, also when
            // tiles overlap or a short page repeats the same view on several tiles.
            let (pg, cell) = self.tile_cell(page).ok_or(DocError::PageOutOfRange(page))?;
            return Ok(page_position(pg, Some(&cell).filter(|c| c.is_finite() && !c.is_empty())));
        }
        let first = *self.pages_of_screen(page).first().ok_or(DocError::PageOutOfRange(page))?;
        Ok(page_position(first, &[]))
    }

    fn page_of(&self, pos: &Position) -> Result<usize> {
        let p = self.page_pos(pos)?;
        let page = p.page as usize;
        match p.quads.first() {
            Some(q) if self.plan.per_page() > 1 => {
                let (x, y) = FRect::from_quad(q).center();
                Ok(self.screen_showing(page, x, y))
            }
            _ => Ok(self.first_screen_of(page)),
        }
    }

    fn current_page(&self) -> usize {
        self.current
    }

    fn set_current_page(&mut self, page: usize) {
        self.current = page.min(self.screens().saturating_sub(1));
    }

    fn toc(&self) -> Vec<TocEntry> {
        self.outline()
            .iter()
            .filter_map(|o| {
                let page = o.page?;
                // In tile modes: the tile that shows the target point.
                let screen = if (o.x > 0.0 || o.y > 0.0) && page < self.pages {
                    self.screen_showing(page, o.x, o.y)
                } else {
                    self.first_screen_of(page)
                };
                let position = target_position(page, o.x, o.y);
                Some(TocEntry { title: o.title.clone(), level: o.level, page: Some(screen), position })
            })
            .collect()
    }

    fn search(&mut self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();
        if query.trim().is_empty() || limit == 0 {
            return Ok(hits);
        }
        for page in 0..self.pages {
            // A page MuPDF cannot load has no hits; the rest of the document still counts.
            let Ok(text) = self.page_text(page).inspect_err(|e| log::warn!("mupdf: search skips page {page}: {e}")) else {
                continue;
            };
            for m in text.find(query, 40) {
                let rects = text.rects(m.chars.clone());
                let (cx, cy) = rects.first().map(|r| r.center()).unwrap_or((0.0, 0.0));
                hits.push(SearchHit {
                    position: page_position(page, &rects),
                    page: Some(self.screen_showing(page, cx, cy)),
                    excerpt: m.excerpt,
                    matched: m.matched,
                });
                if hits.len() >= limit {
                    return Ok(hits);
                }
            }
        }
        Ok(hits)
    }

    fn read_around(&mut self, pos: &Position, chars: usize) -> Result<String> {
        let p = self.page_pos(pos)?;
        let page = p.page as usize;
        let text = self.page_text(page)?;
        let rects: Vec<FRect> = p.quads.iter().map(FRect::from_quad).collect();
        let center = text.range_in(&rects).map(|r| r.start).unwrap_or(0);
        let mut out = text.text_of(text.window(center, chars));
        let mut next = page + 1;
        while out.chars().count() < chars && next < self.pages && next <= page + 3 {
            // The page of `pos` must load; a damaged page after it is skipped.
            let more = self.page_text(next).map(|t| t.plain()).unwrap_or_else(|e| {
                log::warn!("mupdf: read_around skips page {next}: {e}");
                String::new()
            });
            if !more.is_empty() {
                out.push(' ');
                out.push_str(&more);
            }
            next += 1;
        }
        Ok(out.chars().take(chars.max(1)).collect())
    }

    fn links(&mut self, pos: &Position) -> Result<Vec<Link>> {
        let p = self.page_pos(pos)?;
        let page = p.page as usize;
        let screen = self.page_of(pos)?;
        let piece = self.pieces(screen).into_iter().find(|x| x.page == page);
        let raw = self.inner.borrow_mut().raw.links(page)?;
        let labels = self.page_text(page)?.texts_under(&raw.iter().map(|l| l.rect).collect::<Vec<_>>());
        Ok(raw
            .into_iter()
            .zip(labels)
            .map(|(l, label)| {
                let footnote = !label.is_empty()
                    && label.chars().count() <= 3
                    && label.chars().all(|c| c.is_ascii_digit() || "*\u{2020}\u{2021}\u{a7}".contains(c));
                let target = match l.page {
                    Some(tp) if !l.external => LinkTarget::Internal(target_position(tp, l.x, l.y)),
                    _ => LinkTarget::External(l.uri.clone()),
                };
                let rect = piece.and_then(|pc| pc.screen_rect(&l.rect));
                Link { text: label, target, rect, footnote }
            })
            .collect())
    }

    fn select(&mut self, page: usize, from: Point, to: Point) -> Result<Option<Selection>> {
        in_range(page, self.screens())?;
        let pieces = self.pieces(page);
        let (fx, fy) = (from.x as f32, from.y as f32);
        let Some(piece) = pieces
            .iter()
            .find(|p| FRect::from_rect(p.clip).contains(fx, fy))
            .or_else(|| pieces.iter().min_by(|a, b| {
                FRect::from_rect(a.clip).distance(fx, fy, 1.0).total_cmp(&FRect::from_rect(b.clip).distance(fx, fy, 1.0))
            }))
            .copied()
        else {
            return Ok(None);
        };
        let inv = piece.ctm.invert();
        let (ax, ay) = inv.apply(fx, fy);
        let (bx, by) = inv.apply(to.x as f32, to.y as f32);
        let text = self.page_text(piece.page)?;
        let (Some(a), Some(b)) = (text.nearest(ax, ay), text.nearest(bx, by)) else {
            self.selection = None;
            return Ok(None);
        };
        let range = a.min(b)..a.max(b) + 1;
        let rects = text.rects(range.clone());
        if rects.is_empty() {
            self.selection = None;
            return Ok(None);
        }
        let last = text.chars[range.end - 1].rect;
        let sel = Selection {
            start: page_position(piece.page, &rects),
            end: page_position(piece.page, [&last]),
            text: text.text_of(range),
            page,
            rects: rects.iter().filter_map(|r| piece.screen_rect(r)).collect(),
        };
        self.selection = Some(sel.clone());
        Ok(Some(sel))
    }

    fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    fn clear_selection(&mut self) {
        self.selection = None;
    }

    fn set_selection(&mut self, sel: Selection) {
        self.selection = Some(sel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spread_pages_pair_up_after_the_cover() {
        let none = Vec::<usize>::new();
        assert_eq!(spread_pages(0, 5, true), vec![0]);
        assert_eq!(spread_pages(1, 5, true), vec![1, 2]);
        assert_eq!(spread_pages(2, 5, true), vec![3, 4]);
        assert_eq!(spread_pages(3, 5, true), none);
        assert_eq!(spread_pages(2, 4, true), vec![3]);
        assert_eq!(spread_pages(0, 5, false), vec![0, 1]);
        assert_eq!(spread_pages(2, 5, false), vec![4]);
        assert_eq!(spread_pages(3, 5, false), none);
        assert_eq!(spread_pages(0, 0, true), none);
    }
}
