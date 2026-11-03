//! Safe wrapper over the C shim: one [`RawDoc`] owns one MuPDF context and document.
//!
//! Display lists (a page's drawing commands) are cached per page in a small LRU, so
//! rendering, text extraction and margin detection of one page interpret its content once.

use crate::ffi;
use crate::geom::{FRect, Matrix};
use ink_device::Rect;
use ink_doc::{DocError, Result};
use std::ffi::{CStr, CString};
use std::os::raw::c_int;
use std::path::Path;
use std::ptr::NonNull;

/// One character of page text (page space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawChar {
    /// The character (U+FFFD when MuPDF had none).
    pub c: char,
    /// Bounding box.
    pub rect: FRect,
    /// Line number on the page.
    pub line: u32,
    /// Block number on the page.
    pub block: u32,
}

/// One outline entry.
#[derive(Clone, Debug, PartialEq)]
pub struct RawOutline {
    /// Depth, 0 = top.
    pub level: u8,
    /// Target page (0-based), `None` when unresolved.
    pub page: Option<usize>,
    /// Target point on the page.
    pub x: f32,
    /// Target point on the page.
    pub y: f32,
    /// Title.
    pub title: String,
    /// URI, if the entry is a link.
    pub uri: Option<String>,
}

/// One link on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct RawLink {
    /// Hot area in page space.
    pub rect: FRect,
    /// URL (external) or `#page=..` style internal URI.
    pub uri: String,
    /// True for a URL outside the document.
    pub external: bool,
    /// Internal target page.
    pub page: Option<usize>,
    /// Internal target point.
    pub x: f32,
    /// Internal target point.
    pub y: f32,
}

struct CachedList {
    page: usize,
    list: NonNull<ffi::InkList>,
    bounds: FRect,
    tick: u64,
}

/// An open MuPDF document.
pub struct RawDoc {
    doc: NonNull<ffi::InkDoc>,
    lists: Vec<CachedList>,
    max_lists: usize,
    tick: u64,
}

// SAFETY: the context has no thread affinity; `&mut self` on every call keeps it to one
// thread at a time.
unsafe impl Send for RawDoc {}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

fn owned(p: *const std::os::raw::c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        // SAFETY: the shim hands out NUL-terminated strings.
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

impl RawDoc {
    /// Opens `path`. `store_max` caps MuPDF's resource store (fonts, decoded images) in
    /// bytes; `max_lists` is how many page display lists stay cached.
    pub fn open(path: &Path, store_max: usize, max_lists: usize) -> Result<RawDoc> {
        use std::os::unix::ffi::OsStrExt;
        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| DocError::Format(format!("path with NUL: {}", path.display())))?;
        let mut err = vec![0 as std::os::raw::c_char; 512];
        // SAFETY: valid C string and buffer of the given length.
        let doc = unsafe { ffi::ink_open(c_path.as_ptr(), store_max, err.as_mut_ptr(), err.len()) };
        match NonNull::new(doc) {
            Some(doc) => Ok(RawDoc { doc, lists: Vec::new(), max_lists: max_lists.max(1), tick: 0 }),
            None => {
                let msg = owned(err.as_ptr()).unwrap_or_default();
                if !path.exists() {
                    Err(DocError::NotFound(path.display().to_string()))
                } else {
                    Err(DocError::Format(format!("{}: {msg}", path.display())))
                }
            }
        }
    }

    fn ptr(&self) -> *mut ffi::InkDoc {
        self.doc.as_ptr()
    }

    fn error(&self, what: &str) -> DocError {
        // SAFETY: valid handle.
        let msg = owned(unsafe { ffi::ink_error(self.ptr()) }).unwrap_or_default();
        DocError::Format(format!("{what}: {msg}"))
    }

    /// Warnings MuPDF reported so far (damaged files repair themselves with warnings).
    pub fn warnings(&self) -> usize {
        // SAFETY: valid handle.
        unsafe { ffi::ink_warning_count(self.ptr()) as usize }
    }

    /// True when the document is encrypted and no password was accepted yet.
    pub fn needs_password(&mut self) -> bool {
        // SAFETY: valid handle.
        unsafe { ffi::ink_needs_password(self.ptr()) != 0 }
    }

    /// Tries a password; true when accepted.
    pub fn authenticate(&mut self, password: &str) -> bool {
        let p = cstr(password);
        // SAFETY: valid handle and C string.
        unsafe { ffi::ink_authenticate(self.ptr(), p.as_ptr()) > 0 }
    }

    /// Number of pages.
    pub fn page_count(&mut self) -> Result<usize> {
        // SAFETY: valid handle.
        let n = unsafe { ffi::ink_page_count(self.ptr()) };
        if n < 0 {
            Err(self.error("count pages"))
        } else {
            Ok(n as usize)
        }
    }

    /// A metadata value (`info:Title`, `info:Author`, `format`, `encryption`, ...).
    pub fn metadata(&mut self, key: &str) -> Option<String> {
        let k = cstr(key);
        let mut buf = vec![0 as std::os::raw::c_char; 1024];
        loop {
            // SAFETY: valid handle, C string, buffer.
            let n = unsafe { ffi::ink_metadata(self.ptr(), k.as_ptr(), buf.as_mut_ptr(), buf.len()) };
            if n <= 0 {
                return None;
            }
            if n as usize > buf.len() && buf.len() < 1 << 20 {
                buf = vec![0; n as usize];
                continue;
            }
            return owned(buf.as_ptr()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        }
    }

    /// Anti-aliasing bits (0 = none, 8 = best).
    pub fn set_aa(&mut self, bits: u8) {
        // SAFETY: valid handle.
        unsafe { ffi::ink_set_aa(self.ptr(), bits.min(8) as c_int) }
    }

    /// Page bounds in page space.
    pub fn page_bound(&mut self, page: usize) -> Result<FRect> {
        if let Some(l) = self.lists.iter().find(|l| l.page == page) {
            return Ok(l.bounds);
        }
        let mut r = ffi::InkRect::default();
        // SAFETY: valid handle and out pointer.
        if unsafe { ffi::ink_page_bound(self.ptr(), page as c_int, &mut r) } < 0 {
            return Err(self.error(&format!("page {page}")));
        }
        Ok(FRect::new(r.x0, r.y0, r.x1, r.y1))
    }

    fn list(&mut self, page: usize) -> Result<(*mut ffi::InkList, FRect)> {
        self.tick += 1;
        let tick = self.tick;
        if let Some(l) = self.lists.iter_mut().find(|l| l.page == page) {
            l.tick = tick;
            return Ok((l.list.as_ptr(), l.bounds));
        }
        let mut r = ffi::InkRect::default();
        // SAFETY: valid handle and out pointer.
        let list = unsafe { ffi::ink_list_new(self.ptr(), page as c_int, &mut r) };
        let list = NonNull::new(list).ok_or_else(|| self.error(&format!("page {page}")))?;
        while self.lists.len() >= self.max_lists {
            let oldest = self.lists.iter().enumerate().min_by_key(|(_, l)| l.tick).map(|(i, _)| i).unwrap_or(0);
            let old = self.lists.swap_remove(oldest);
            // SAFETY: the list came from this document and is dropped once.
            unsafe { ffi::ink_list_drop(self.ptr(), old.list.as_ptr()) };
        }
        let bounds = FRect::new(r.x0, r.y0, r.x1, r.y1);
        self.lists.push(CachedList { page, list, bounds, tick });
        Ok((list.as_ptr(), bounds))
    }

    /// Number of cached display lists (for tests).
    pub fn cached_lists(&self) -> usize {
        self.lists.len()
    }

    /// Draws `page` with `ctm` into `dst` (8-bit gray, `area.w * area.h` bytes, white
    /// background), which covers device pixels `area`.
    pub fn render(&mut self, page: usize, ctm: &Matrix, area: Rect, dst: &mut [u8]) -> Result<()> {
        let (w, h) = (area.w as usize, area.h as usize);
        if w == 0 || h == 0 {
            return Ok(());
        }
        assert!(dst.len() >= w * h, "render buffer too small");
        let (list, _) = self.list(page)?;
        // SAFETY: list belongs to this document; dst has w*h bytes.
        let rc = unsafe {
            ffi::ink_render(self.ptr(), list, ctm.m.as_ptr(), area.x, area.y, w as c_int, h as c_int, dst.as_mut_ptr())
        };
        if rc < 0 {
            return Err(self.error(&format!("render page {page}")));
        }
        Ok(())
    }

    /// Structured text of `page` in reading order.
    pub fn text(&mut self, page: usize) -> Result<Vec<RawChar>> {
        let (list, _) = self.list(page)?;
        let mut out: *mut ffi::InkChar = std::ptr::null_mut();
        let mut n: c_int = 0;
        // SAFETY: valid handle/list and out pointers.
        if unsafe { ffi::ink_text(self.ptr(), list, &mut out, &mut n) } < 0 {
            return Err(self.error(&format!("text of page {page}")));
        }
        let chars = if out.is_null() || n <= 0 {
            Vec::new()
        } else {
            // SAFETY: the shim returned n initialised entries.
            let slice = unsafe { std::slice::from_raw_parts(out, n as usize) };
            slice
                .iter()
                .map(|c| RawChar {
                    c: char::from_u32(c.c as u32).unwrap_or('\u{fffd}'),
                    rect: FRect::new(c.x0, c.y0, c.x1, c.y1),
                    line: c.line as u32,
                    block: c.block as u32,
                })
                .collect()
        };
        // SAFETY: allocated by the shim with malloc.
        unsafe { ffi::ink_free(out.cast()) };
        Ok(chars)
    }

    /// The outline, depth first.
    pub fn outline(&mut self) -> Result<Vec<RawOutline>> {
        let mut out: *mut ffi::InkOutlineItem = std::ptr::null_mut();
        let mut n: c_int = 0;
        // SAFETY: valid handle and out pointers.
        if unsafe { ffi::ink_outline(self.ptr(), &mut out, &mut n) } < 0 {
            return Err(self.error("outline"));
        }
        let mut items = Vec::new();
        if !out.is_null() && n > 0 {
            // SAFETY: n initialised entries.
            for it in unsafe { std::slice::from_raw_parts(out, n as usize) } {
                items.push(RawOutline {
                    level: it.level.clamp(0, 255) as u8,
                    page: (it.page >= 0).then_some(it.page as usize),
                    x: it.x,
                    y: it.y,
                    title: owned(it.title).unwrap_or_default(),
                    uri: owned(it.uri),
                });
            }
        }
        // SAFETY: allocated by the shim; freed once.
        unsafe { ffi::ink_outline_free(out, n) };
        Ok(items)
    }

    /// Links on `page`.
    pub fn links(&mut self, page: usize) -> Result<Vec<RawLink>> {
        let mut out: *mut ffi::InkLinkItem = std::ptr::null_mut();
        let mut n: c_int = 0;
        // SAFETY: valid handle and out pointers.
        if unsafe { ffi::ink_links(self.ptr(), page as c_int, &mut out, &mut n) } < 0 {
            return Err(self.error(&format!("links of page {page}")));
        }
        let mut items = Vec::new();
        if !out.is_null() && n > 0 {
            // SAFETY: n initialised entries.
            for it in unsafe { std::slice::from_raw_parts(out, n as usize) } {
                items.push(RawLink {
                    rect: FRect::new(it.x0, it.y0, it.x1, it.y1),
                    uri: owned(it.uri).unwrap_or_default(),
                    external: it.external != 0,
                    page: (it.page >= 0).then_some(it.page as usize),
                    x: it.x,
                    y: it.y,
                });
            }
        }
        // SAFETY: allocated by the shim; freed once.
        unsafe { ffi::ink_links_free(out, n) };
        Ok(items)
    }
}

impl Drop for RawDoc {
    fn drop(&mut self) {
        for l in self.lists.drain(..) {
            // SAFETY: lists belong to this document; dropped before the document.
            unsafe { ffi::ink_list_drop(self.doc.as_ptr(), l.list.as_ptr()) };
        }
        // SAFETY: closed once.
        unsafe { ffi::ink_close(self.doc.as_ptr()) };
    }
}
