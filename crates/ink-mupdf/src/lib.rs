//! Fixed-layout documents (PDF, CBZ, XPS) through MuPDF 1.27.2 (phase 6).
//!
//! # Build
//!
//! `build.rs` builds MuPDF from the pinned source tarball (the same 1.27.2 release KOReader
//! uses, SHA-256 checked) as two static libraries, with `zig cc` for the Kindle
//! (`cargo zigbuild --target armv7-unknown-linux-musleabihf`) and the system compiler for
//! host builds, and caches them in `inkreader-wt/.cache/mupdf/build/`. Feature set: PDF,
//! CBZ (and other image archives), XPS; no JavaScript, no HTML/EPUB engine, no SVG, no
//! Noto/CJK fonts (the 14 URW base fonts stay). See `build.rs` for details and overrides.
//!
//! # Safety
//!
//! MuPDF reports errors with setjmp/longjmp. Rust only calls `csrc/shim.c`, which wraps
//! every MuPDF call in `fz_try`/`fz_catch` and returns error codes ([`ffi`], [`raw`]).
//!
//! # Use
//!
//! [`MupdfProvider`] opens files for the app (options from `RINKY_FIXED`, see
//! [`FixedOptions::parse`]). [`MupdfDocument`] implements [`ink_doc::Document`] (screens,
//! positions, search, selection, TOC, links) and adds fixed-layout controls: options,
//! free zoom and pan ([`MupdfDocument::render_page_at`]), crop boxes, columns, highlight
//! rectangles, thumbnails and the page cache. See [`document`] for how screens map to pages.
//!
//! # Environment
//!
//! | Variable | When | Meaning |
//! | --- | --- | --- |
//! | `INK_MUPDF_CACHE` | build | directory for the tarball, sources and built libraries |
//! | `INK_MUPDF_TARBALL` | build | use this `mupdf-1.27.2-source.tar.lz` (still checksummed) |
//! | `INK_MUPDF_LIB_DIR` | build | link prebuilt `libmupdf.a` + `libmupdf-third.a` from here |
//! | `INK_MUPDF_JOBS` | build | `make -j` (default: cargo's job count) |
//! | `RINKY_FIXED` | run | default [`FixedOptions`], e.g. `fit=width,crop=1,gamma=1.5` |
//! | `INK_MUPDF_STORE_MB` | run | MuPDF store cap (fonts, images), default 48 |
//! | `INK_MUPDF_CACHE_MB` | run | rendered-screen cache cap, default 16 |
//! | `INK_MUPDF_WARNINGS` | run | print MuPDF errors and warnings to stderr |

#![warn(missing_docs)]

pub mod analyze;
pub mod cache;
pub mod document;
pub mod ffi;
pub mod geom;
pub mod raw;
pub mod testgen;
pub mod text;

pub use document::{FileKind, MupdfDocument, Piece, Plan, RenderSpec};
pub use geom::{FRect, Matrix};
pub use text::PageText;

use ink_device::Canvas;
pub use ink_doc::{Fit, FixedView, Spreads};

use ink_doc::{Document, Provider, Result};
use std::path::Path;

/// Rendering options for fixed-layout pages (the target size comes from `LayoutSettings`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FixedOptions {
    /// Fit mode.
    pub fit: Fit,
    /// Extra zoom on top of the fit (1.0 = none; above 1 the page is cut into tiles).
    pub zoom: f32,
    /// Crop white margins automatically (detected on a small rendering of each page).
    pub crop_margins: bool,
    /// Contrast (1.0 = none; 1.5 = steeper): `(v - 0.5) * contrast + 0.5`.
    pub contrast: f32,
    /// Gamma (1.0 = none; above 1 darkens thin text): `v ^ gamma`.
    pub gamma: f32,
    /// Clockwise page rotation: 0, 90, 180 or 270 degrees.
    pub rotate: u16,
    /// Text columns read one after the other (1 = off, up to 4).
    pub columns: u8,
    /// Two-page spreads.
    pub spreads: Spreads,
    /// In spreads, the first page (cover) stands alone.
    pub cover_alone: bool,
    /// In spreads, pages run right to left (manga).
    pub rtl: bool,
}

impl Default for FixedOptions {
    fn default() -> Self {
        FixedOptions {
            fit: Fit::Page,
            zoom: 1.0,
            crop_margins: false,
            contrast: 1.0,
            gamma: 1.0,
            rotate: 0,
            columns: 1,
            spreads: Spreads::Auto,
            cover_alone: true,
            rtl: false,
        }
    }
}

impl FixedOptions {
    /// Clamps values into their ranges (zoom 0.25..8, columns 1..4, rotation to a
    /// multiple of 90, contrast 0.2..4, gamma 0.2..4).
    pub fn normalized(mut self) -> Self {
        let finite = |v: f32, d: f32| if v.is_finite() { v } else { d };
        self.zoom = finite(self.zoom, 1.0).clamp(0.25, 8.0);
        self.contrast = finite(self.contrast, 1.0).clamp(0.2, 4.0);
        self.gamma = finite(self.gamma, 1.0).clamp(0.2, 4.0);
        self.columns = self.columns.clamp(1, 4);
        self.rotate = ((self.rotate as u32 + 45) / 90 * 90 % 360) as u16;
        self
    }

    /// Parses `key=value` pairs separated by commas or spaces, starting from the
    /// defaults: `fit=page|width`, `zoom=F`, `crop=0|1`, `contrast=F`, `gamma=F`,
    /// `rotate=0|90|180|270`, `columns=N`, `spreads=auto|always|never`, `cover=0|1`,
    /// `rtl=0|1`.
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        let mut o = FixedOptions::default();
        let flag = |v: &str| matches!(v, "1" | "true" | "yes" | "on");
        for part in s.split([',', ' ', ';']).map(str::trim).filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=').ok_or_else(|| format!("expected key=value: {part}"))?;
            let num = || v.parse::<f32>().map_err(|_| format!("bad number in {part}"));
            match k {
                "fit" => {
                    o.fit = match v {
                        "page" => Fit::Page,
                        "width" => Fit::Width,
                        _ => return Err(format!("fit must be page or width: {part}")),
                    }
                }
                "zoom" => o.zoom = num()?,
                "crop" => o.crop_margins = flag(v),
                "contrast" => o.contrast = num()?,
                "gamma" => o.gamma = num()?,
                "rotate" => o.rotate = num()? as u16,
                "columns" => o.columns = num()? as u8,
                "spreads" => {
                    o.spreads = match v {
                        "auto" => Spreads::Auto,
                        "always" | "on" => Spreads::Always,
                        "never" | "off" => Spreads::Never,
                        _ => return Err(format!("spreads must be auto, always or never: {part}")),
                    }
                }
                "cover" => o.cover_alone = flag(v),
                "rtl" => o.rtl = flag(v),
                _ => return Err(format!("unknown option {k}")),
            }
        }
        Ok(o.normalized())
    }

    /// The reader-facing part of these options (the one mapping between the two, with
    /// [`FixedOptions::with_view`]).
    pub fn view(&self) -> FixedView {
        FixedView {
            fit: self.fit,
            crop_margins: self.crop_margins,
            columns: self.columns,
            contrast: self.contrast,
            spreads: self.spreads,
            rtl: self.rtl,
        }
    }

    /// These options with the reader-facing part replaced by `view` (zoom, gamma,
    /// rotation and the lone cover stay), normalized.
    pub fn with_view(self, view: &FixedView) -> Self {
        FixedOptions {
            fit: view.fit,
            crop_margins: view.crop_margins,
            columns: view.columns,
            contrast: view.contrast,
            spreads: view.spreads,
            rtl: view.rtl,
            ..self
        }
        .normalized()
    }

    /// Options from `RINKY_FIXED` (defaults when unset or invalid; an invalid value is
    /// logged).
    pub fn from_env() -> Self {
        let Ok(s) = std::env::var("RINKY_FIXED") else { return FixedOptions::default() };
        FixedOptions::parse(&s).unwrap_or_else(|e| {
            log::warn!("RINKY_FIXED: {e}");
            FixedOptions::default()
        })
    }
}

/// The provider registered by the app.
#[derive(Clone, Copy, Debug, Default)]
pub struct MupdfProvider;

impl Provider for MupdfProvider {
    fn name(&self) -> &'static str {
        "mupdf"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pdf", "cbz", "xps", "oxps"]
    }

    fn open(&self, path: &Path) -> Result<Box<dyn Document>> {
        let doc = MupdfDocument::open(path, FixedOptions::from_env())?;
        log::info!(
            "mupdf {}: {:?} {} pages, plan {:?}",
            mupdf_version().unwrap_or("?"),
            doc.file_kind(),
            doc.doc_pages(),
            doc.plan()
        );
        Ok(Box::new(doc))
    }
}

/// Version of the linked MuPDF (`1.27.2`).
pub fn mupdf_version() -> Option<&'static str> {
    // SAFETY: the shim returns a pointer to a static NUL-terminated string.
    let v = unsafe { std::ffi::CStr::from_ptr(ffi::ink_mupdf_version()) };
    v.to_str().ok()
}

/// A cover thumbnail (first page) of `path` that fits in `max_w` x `max_h`, for the
/// library's thumbnail cache.
pub fn cover_thumbnail(path: &Path, max_w: u32, max_h: u32) -> Result<Canvas> {
    MupdfDocument::open(path, FixedOptions::default())?.thumbnail(max_w, max_h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_doc::DocError;

    #[test]
    fn provider_basics() {
        assert!(MupdfProvider.can_open(Path::new("x/Scan.PDF")));
        assert!(MupdfProvider.can_open(Path::new("x/comic.cbz")));
        assert!(!MupdfProvider.can_open(Path::new("x.epub")));
        assert!(matches!(MupdfProvider.open(Path::new("/nope.pdf")), Err(DocError::NotFound(_))));
        // A text file named .pdf (written at run time: the test also runs on the Kindle).
        let fake = std::env::temp_dir().join(format!("ink-mupdf-not-a-pdf-{}.pdf", std::process::id()));
        std::fs::write(&fake, "[package]\nname = \"not a pdf\"\n").unwrap();
        let err = MupdfProvider.open(&fake).err().expect("a text file is not a PDF").to_string();
        let _ = std::fs::remove_file(&fake);
        assert!(err.starts_with("bad document"), "{err}");
        assert_eq!(mupdf_version(), Some("1.27.2"));
    }

    #[test]
    fn options_parse_and_clamp() {
        assert_eq!(FixedOptions::parse("").unwrap(), FixedOptions::default());
        let o = FixedOptions::parse("fit=width, zoom=1.5 crop=1;columns=2,contrast=1.4,gamma=2,rotate=90,spreads=never,rtl=1,cover=0")
            .unwrap();
        assert_eq!(o.fit, Fit::Width);
        assert_eq!((o.zoom, o.crop_margins, o.columns, o.rotate), (1.5, true, 2, 90));
        assert_eq!((o.contrast, o.gamma, o.spreads, o.rtl, o.cover_alone), (1.4, 2.0, Spreads::Never, true, false));
        assert!(FixedOptions::parse("fit=diagonal").is_err());
        assert!(FixedOptions::parse("zoom").is_err());
        assert!(FixedOptions::parse("bogus=1").is_err());
        let c = FixedOptions { zoom: 100.0, columns: 9, rotate: 100, gamma: f32::NAN, ..Default::default() }.normalized();
        assert_eq!((c.zoom, c.columns, c.rotate, c.gamma), (8.0, 4, 90, 1.0));
        assert_eq!(FixedOptions { rotate: 300, ..Default::default() }.normalized().rotate, 270);
    }

    #[test]
    fn view_maps_both_ways_and_keeps_the_rest() {
        assert_eq!(FixedOptions::default().view(), FixedView::default());
        let base = FixedOptions::parse("zoom=1.5,gamma=2,rotate=90,cover=0").unwrap();
        let v = FixedView { fit: Fit::Width, crop_margins: true, columns: 2, contrast: 1.6, spreads: Spreads::Never, rtl: true };
        let o = base.with_view(&v);
        assert_eq!(o.view(), v);
        assert_eq!((o.zoom, o.gamma, o.rotate, o.cover_alone), (1.5, 2.0, 90, false), "the engine's own options stay");
        // Out of range values are clamped like any option.
        let wild = base.with_view(&FixedView { columns: 9, contrast: f32::NAN, ..v });
        assert_eq!((wild.columns, wild.contrast), (4, 1.0));
    }
}
