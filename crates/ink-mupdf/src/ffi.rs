//! Declarations for `csrc/shim.h`. Only the shim is called from Rust; it catches every
//! MuPDF error (MuPDF uses setjmp/longjmp, which must never cross a Rust frame).

#![allow(missing_docs)]

use crate::geom::FRect;
use std::os::raw::{c_char, c_float, c_int, c_uchar};

/// Opaque `ink_doc`.
#[repr(C)]
pub struct InkDoc {
    _private: [u8; 0],
}

/// Opaque `fz_display_list`.
#[repr(C)]
pub struct InkList {
    _private: [u8; 0],
}

/// `ink_char`; `ink_rect` is [`FRect`] (four floats, `repr(C)`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InkChar {
    pub c: c_int,
    pub rect: FRect,
    pub line: c_int,
}

#[repr(C)]
pub struct InkOutlineItem {
    pub level: c_int,
    pub page: c_int,
    pub x: c_float,
    pub y: c_float,
    pub title: *mut c_char,
    pub uri: *mut c_char,
}

#[repr(C)]
pub struct InkLinkItem {
    pub rect: FRect,
    pub external: c_int,
    pub page: c_int,
    pub x: c_float,
    pub y: c_float,
    pub uri: *mut c_char,
}

extern "C" {
    pub fn ink_mupdf_version() -> *const c_char;
    pub fn ink_open(path: *const c_char, store_max: usize, err: *mut c_char, errlen: usize) -> *mut InkDoc;
    pub fn ink_close(d: *mut InkDoc);
    pub fn ink_error(d: *mut InkDoc) -> *const c_char;
    pub fn ink_warning_count(d: *mut InkDoc) -> c_int;
    pub fn ink_needs_password(d: *mut InkDoc) -> c_int;
    pub fn ink_authenticate(d: *mut InkDoc, password: *const c_char) -> c_int;
    pub fn ink_page_count(d: *mut InkDoc) -> c_int;
    pub fn ink_metadata(d: *mut InkDoc, key: *const c_char, buf: *mut c_char, size: usize) -> c_int;
    pub fn ink_page_bound(d: *mut InkDoc, page: c_int, out: *mut FRect) -> c_int;
    pub fn ink_list_new(d: *mut InkDoc, page: c_int, bounds: *mut FRect) -> *mut InkList;
    pub fn ink_list_drop(d: *mut InkDoc, list: *mut InkList);
    pub fn ink_render(
        d: *mut InkDoc,
        list: *mut InkList,
        m: *const c_float,
        x: c_int,
        y: c_int,
        w: c_int,
        h: c_int,
        dst: *mut c_uchar,
    ) -> c_int;
    pub fn ink_text(d: *mut InkDoc, list: *mut InkList, out: *mut *mut InkChar, n: *mut c_int) -> c_int;
    pub fn ink_text_free(chars: *mut InkChar, n: c_int);
    pub fn ink_outline(d: *mut InkDoc, out: *mut *mut InkOutlineItem, n: *mut c_int) -> c_int;
    pub fn ink_outline_free(items: *mut InkOutlineItem, n: c_int);
    pub fn ink_links(d: *mut InkDoc, page: c_int, out: *mut *mut InkLinkItem, n: *mut c_int) -> c_int;
    pub fn ink_links_free(items: *mut InkLinkItem, n: c_int);
}
