//! The C ABI between rinky and native plugin libraries (`.so`, `.dylib` on the host).
//!
//! A native plugin is a shared library with one exported function, [`ENTRY_SYMBOL`]:
//!
//! ```text
//! extern "C" fn rinky_plugin_v1(host: *const HostApiV1) -> *mut PluginV1
//! ```
//!
//! rinky calls it once after loading the library. The plugin keeps the `host` pointer
//! (it stays valid for the life of the process) and returns its own table, which rinky
//! keeps until it exits. Libraries are never unloaded.
//!
//! Rules every function of both tables follows:
//!
//! - No Rust types cross the boundary. Each library carries its own copy of the Rust
//!   standard library, so a `String`, a `Box` or a panic from one side means nothing to
//!   the other. Only the `#[repr(C)]` types of this crate, integers and raw pointers do.
//! - No unwinding crosses the boundary. A panic that reaches the other side's frames
//!   aborts the process ("Rust cannot catch foreign exceptions"), so every function
//!   catches panics on its own side and returns [`ERR_PANIC`] instead.
//! - Strings are UTF-8, passed as a pointer and a length ([`Str`]) and borrowed for the
//!   duration of the call only. A string handed over for keeping ([`OwnedStr`]) is
//!   freed by the side that allocated it, with that side's `free_str`.
//! - Structured values (menu items, document rows, HTTP options) are JSON text.
//! - A `*mut Call` is valid only during the plugin hook that received it: host
//!   functions are called with it from inside that hook, never later and never from
//!   another thread.
//!
//! Versioning: [`ABI_VERSION`] changes whenever a table changes in any way (a field
//! added, removed, reordered or retyped). rinky refuses a plugin built for another
//! version, and a plugin refuses a host of another version; both say which versions
//! they saw.

#![warn(missing_docs)]
#![allow(clippy::missing_safety_doc)]

use std::ffi::c_void;

/// The ABI version of [`HostApiV1`] and [`PluginV1`].
pub const ABI_VERSION: u32 = 1;

/// Name of the entry function (NUL-terminated, for `dlsym`).
pub const ENTRY_SYMBOL: &[u8] = b"rinky_plugin_v1\0";

/// Type of the entry function.
pub type EntryFn = unsafe extern "C" fn(host: *const HostApiV1) -> *mut PluginV1;

/// Result of a call. `OK` (0) is success; everything else is a failure whose message
/// the callee kept (see each table).
pub type Status = i32;
/// Success.
pub const OK: Status = 0;
/// The call failed; a message is available.
pub const ERR_FAILED: Status = 1;
/// The callee panicked; the panic was caught on its side.
pub const ERR_PANIC: Status = 2;
/// A permission in `plugin.toml` is missing.
pub const ERR_PERMISSION: Status = 3;
/// Not available here (no book open, wrong phase, unknown method).
pub const ERR_UNSUPPORTED: Status = 4;
/// A bad argument (not UTF-8, bad JSON, unknown name).
pub const ERR_ARGUMENT: Status = 5;
/// Nothing there (a missing setting, no tile state, no word bounds). Not an error.
pub const NOT_FOUND: Status = 6;

/// Opaque per-call context of the host (see the module rules).
#[repr(C)]
pub struct Call {
    _private: [u8; 0],
}

/// A borrowed UTF-8 string (or byte string, where a field says so).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Str {
    /// First byte; may be null when `len` is 0.
    pub ptr: *const u8,
    /// Length in bytes.
    pub len: usize,
}

impl Str {
    /// The empty string.
    pub const EMPTY: Str = Str { ptr: std::ptr::null(), len: 0 };

    /// Borrows `s`.
    pub fn new(s: &str) -> Str {
        Str { ptr: s.as_ptr(), len: s.len() }
    }

    /// Borrows bytes.
    pub fn bytes(b: &[u8]) -> Str {
        Str { ptr: b.as_ptr(), len: b.len() }
    }

    /// The bytes.
    ///
    /// # Safety
    /// `ptr` must point to `len` readable bytes that outlive `'a`.
    pub unsafe fn as_bytes<'a>(self) -> &'a [u8] {
        if self.len == 0 || self.ptr.is_null() {
            &[]
        } else {
            std::slice::from_raw_parts(self.ptr, self.len)
        }
    }

    /// The text, or `None` when it is not UTF-8.
    ///
    /// # Safety
    /// As [`Str::as_bytes`].
    pub unsafe fn as_str<'a>(self) -> Option<&'a str> {
        std::str::from_utf8(self.as_bytes()).ok()
    }
}

/// A string handed to the other side for keeping. It must go back to the allocating
/// side's `free_str` exactly once. A null `ptr` means "no string".
#[repr(C)]
#[derive(Debug)]
pub struct OwnedStr {
    /// First byte (from a boxed byte slice of the allocating side).
    pub ptr: *mut u8,
    /// Length in bytes.
    pub len: usize,
}

impl OwnedStr {
    /// No string.
    pub const NONE: OwnedStr = OwnedStr { ptr: std::ptr::null_mut(), len: 0 };

    /// Hands `s` over. Only the side that calls this may free it ([`OwnedStr::free`]).
    pub fn from_string(s: String) -> OwnedStr {
        Self::from_bytes(s.into_bytes())
    }

    /// Hands bytes over.
    pub fn from_bytes(b: Vec<u8>) -> OwnedStr {
        let b = b.into_boxed_slice();
        let len = b.len();
        OwnedStr { ptr: Box::into_raw(b).cast::<u8>(), len }
    }

    /// Borrows the content (valid until it is freed).
    pub fn as_str(&self) -> Str {
        Str { ptr: self.ptr, len: self.len }
    }

    /// True for "no string".
    pub fn is_none(&self) -> bool {
        self.ptr.is_null()
    }

    /// Frees a string made by [`OwnedStr::from_string`] on this side.
    ///
    /// # Safety
    /// `self` must come from this side's [`OwnedStr::from_bytes`] and not be freed yet.
    pub unsafe fn free(self) {
        if !self.ptr.is_null() {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(self.ptr, self.len)));
        }
    }
}

/// `free_str` of a table: frees an [`OwnedStr`] that side allocated.
pub type FreeStrFn = unsafe extern "C" fn(s: OwnedStr);

/// Kinds of [`HttpEventV1`].
pub mod http {
    /// Status and headers: `status`, `headers_json` (array of `[name, value]`, names
    /// lowercase).
    pub const RESPONSE: u32 = 1;
    /// A piece of the body (`data`, bytes).
    pub const CHUNK: u32 = 2;
    /// A server-sent event: `data`; `headers_json` is `{"event": ..., "id": ...}`.
    pub const SSE: u32 = 3;
    /// The request finished; `data` is the whole body when `has_data` is 1.
    pub const DONE: u32 = 4;
    /// The request failed; `data` is the message.
    pub const ERROR: u32 = 5;
}

/// An event of a request started with [`HostApiV1::http_request`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HttpEventV1 {
    /// One of [`http`].
    pub kind: u32,
    /// HTTP status ([`http::RESPONSE`]).
    pub status: u32,
    /// Body bytes, event data or message (see [`http`]).
    pub data: Str,
    /// 1 when `data` is set for [`http::DONE`].
    pub has_data: u32,
    /// JSON (see [`http`]); empty when not used.
    pub headers_json: Str,
}

/// Annotation positions ([`AnnotationV1::position`], [`AnnotatorV1::positions`] bits).
pub mod position {
    /// Above the text.
    pub const OVER: u32 = 1;
    /// Below the text.
    pub const UNDER: u32 = 2;
}

/// One annotation: bytes `start..end` of the paragraph get `text` drawn at `position`.
#[repr(C)]
#[derive(Debug)]
pub struct AnnotationV1 {
    /// Byte offset of the start (a char boundary).
    pub start: usize,
    /// Byte offset of the end (a char boundary, after `start`).
    pub end: usize,
    /// The annotation text (owned by the list).
    pub text: OwnedStr,
    /// [`position::OVER`] or [`position::UNDER`].
    pub position: u32,
}

/// Annotations returned by [`AnnotatorV1::annotate`]; freed with
/// [`AnnotatorV1::free_list`].
#[repr(C)]
#[derive(Debug)]
pub struct AnnotationListV1 {
    /// First element (a boxed slice of the plugin).
    pub ptr: *mut AnnotationV1,
    /// Number of elements.
    pub len: usize,
}

/// A text annotator ([`PluginV1::annotator`]). Its functions are called on the
/// reader's pagination thread as well as the UI thread, so the plugin's annotator must
/// be thread-safe. `state` lives until `drop`.
#[repr(C)]
pub struct AnnotatorV1 {
    /// The annotator's own state.
    pub state: *mut c_void,
    /// A short stable name (borrowed until `drop`).
    pub id: unsafe extern "C" fn(state: *mut c_void) -> Str,
    /// Changes whenever the output of `annotate` may change.
    pub version: unsafe extern "C" fn(state: *mut c_void) -> u64,
    /// Changes whenever `wants`, `positions` or `ideograph_spacing` may change.
    pub layout_version: unsafe extern "C" fn(state: *mut c_void) -> u64,
    /// [`position`] bits this annotator draws in.
    pub positions: unsafe extern "C" fn(state: *mut c_void) -> u32,
    /// Extra space between ideographs in em; `lang` empty when unknown.
    pub ideograph_spacing: unsafe extern "C" fn(state: *mut c_void, lang: Str) -> f32,
    /// 1 when the paragraph may get annotations, 0 when not, negative on failure.
    pub wants: unsafe extern "C" fn(state: *mut c_void, paragraph: Str, lang: Str) -> i32,
    /// Fills `out` with the annotations of `paragraph`.
    pub annotate: unsafe extern "C" fn(state: *mut c_void, paragraph: Str, lang: Str, out: *mut AnnotationListV1) -> Status,
    /// Frees a list `annotate` returned.
    pub free_list: unsafe extern "C" fn(list: AnnotationListV1),
    /// Frees `state` and this table.
    pub drop: unsafe extern "C" fn(table: *mut AnnotatorV1),
}

/// What a plugin library gives rinky ([`ENTRY_SYMBOL`]).
///
/// Hooks that take a `*mut Call` may call back into [`HostApiV1`] with it. A hook that
/// fails returns a non-`OK` [`Status`] and puts its message into `*err` (an
/// [`OwnedStr`] of the plugin, freed with `free_str`). All hooks run on rinky's UI
/// thread unless noted.
#[repr(C)]
pub struct PluginV1 {
    /// Must be [`ABI_VERSION`].
    pub abi_version: u32,
    /// The plugin's own state.
    pub state: *mut c_void,
    /// The plugin name; must equal `name` in `plugin.toml` (borrowed until `drop`).
    pub name: unsafe extern "C" fn(state: *mut c_void) -> Str,
    /// Frees an [`OwnedStr`] of the plugin.
    pub free_str: FreeStrFn,
    /// Start-up: registrations (menu items, actions, gestures, ...) via
    /// [`HostApiV1::register`].
    pub on_load: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status,
    /// A book was opened.
    pub on_book_open: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status,
    /// The reader moved to `page` (0-based).
    pub on_page_change: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, page: u64, err: *mut OwnedStr) -> Status,
    /// The user selected text: JSON `{"text", "start", "end", "page"}` (page 0-based).
    pub on_selection: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, selection_json: Str, err: *mut OwnedStr) -> Status,
    /// The device is going to sleep.
    pub on_suspend: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status,
    /// The device woke up.
    pub on_resume: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status,
    /// The book was closed.
    pub on_close: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status,
    /// rinky is about to exit; `restarting` is 1 when a new rinky follows at once.
    pub on_exit: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, restarting: u32, err: *mut OwnedStr) -> Status,
    /// A registered entry `id` was triggered (settings pages as `settings:<id>`).
    pub on_action: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, id: Str, err: *mut OwnedStr) -> Status,
    /// A library action `id` on the book file `book`.
    pub on_library_action: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, id: Str, book: Str, err: *mut OwnedStr) -> Status,
    /// The answer to UI request `id`.
    pub on_ui_result: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, id: u32, result: Str, err: *mut OwnedStr) -> Status,
    /// An event of HTTP request `id`.
    pub on_http: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, id: u32, event: *const HttpEventV1, err: *mut OwnedStr) -> Status,
    /// How often the plugin wants `on_tick`, in milliseconds; 0 for no ticks.
    pub tick_interval_ms: unsafe extern "C" fn(state: *mut c_void) -> u64,
    /// Periodic call while `tick_interval_ms` is not 0.
    pub on_tick: unsafe extern "C" fn(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status,
    /// State of control tile `id`: `OK` with `*text` and `*on` (-1 action tile, 0 off,
    /// 1 on), or [`NOT_FOUND`]. Must be quick (called while the tile is drawn).
    pub tile_state: unsafe extern "C" fn(state: *mut c_void, id: Str, text: *mut OwnedStr, on: *mut i32) -> Status,
    /// 1 when the plugin takes the dictionary lookup of `text`, 0 when not (`lang`
    /// empty when unknown); negative on failure (treated as 0).
    pub claims_lookup: unsafe extern "C" fn(state: *mut c_void, text: Str, lang: Str) -> i32,
    /// The word around character `at` of `text` as character indices `*start..*end`
    /// (`OK`), or [`NOT_FOUND`].
    pub word_bounds: unsafe extern "C" fn(state: *mut c_void, text: Str, at: u64, lang: Str, start: *mut u64, end: *mut u64) -> Status,
    /// A new text annotator table (rinky calls its `drop`), or null for none. Asked when
    /// a book opens.
    pub annotator: unsafe extern "C" fn(state: *mut c_void) -> *mut AnnotatorV1,
    /// Frees `state` and this table. Called at most once, at exit, if at all.
    pub drop: unsafe extern "C" fn(table: *mut PluginV1),
}

/// What rinky gives a plugin library. Valid for the life of the process.
///
/// Every function takes the `*mut Call` of the running hook. One that returns a
/// [`Status`] other than `OK`/[`NOT_FOUND`] keeps a message, which
/// [`HostApiV1::take_error`] hands over. The permission checks are those of
/// `plugin.toml`, as for Lua plugins.
#[repr(C)]
pub struct HostApiV1 {
    /// Must be [`ABI_VERSION`].
    pub abi_version: u32,
    /// Frees an [`OwnedStr`] of the host.
    pub free_str: FreeStrFn,
    /// The message of the last failed call with this `call` (none: [`OwnedStr::NONE`]).
    pub take_error: unsafe extern "C" fn(call: *mut Call) -> OwnedStr,
    /// A line in rinky's log.
    pub log: unsafe extern "C" fn(call: *mut Call, text: Str) -> Status,
    /// Registration, only in `on_load`. `kind` is one of `menu_item` (label, order),
    /// `selection_action` (label), `gesture` (extra: gesture name, label),
    /// `action` (label), `settings_page` (label), `library_action` (label),
    /// `control_tile` (label, extra: icon), `library_entry` (label, extra: icon),
    /// `status_widget` (extra: slot `left`/`center`/`right`, label: initial text).
    pub register: unsafe extern "C" fn(call: *mut Call, kind: Str, id: Str, label: Str, extra: Str, order: i32) -> Status,
    /// A short message.
    pub toast: unsafe extern "C" fn(call: *mut Call, text: Str) -> Status,
    /// A notification that stays in the list until read.
    pub notify: unsafe extern "C" fn(call: *mut Call, text: Str) -> Status,
    /// New text of status-bar widget `id`.
    pub status: unsafe extern "C" fn(call: *mut Call, id: Str, text: Str) -> Status,
    /// New state text of control tile `id`.
    pub tile_state: unsafe extern "C" fn(call: *mut Call, id: Str, text: Str) -> Status,
    /// A dialog; `buttons_json` is `[[label, action], ...]` (empty: one OK button).
    /// `*id` gets the request id; the chosen action arrives in `on_ui_result`.
    pub dialog: unsafe extern "C" fn(call: *mut Call, title: Str, body: Str, buttons_json: Str, id: *mut u32) -> Status,
    /// A menu; `items_json` is `[[label, action], ...]`.
    pub menu: unsafe extern "C" fn(call: *mut Call, title: Str, items_json: Str, id: *mut u32) -> Status,
    /// New label of the item with `action` in open menu `id`.
    pub menu_label: unsafe extern "C" fn(call: *mut Call, id: u32, action: Str, label: Str) -> Status,
    /// The Markdown viewer (card). `id` 0 opens a new one and writes its id to `*out`;
    /// the id of an open viewer replaces its text.
    pub view: unsafe extern "C" fn(call: *mut Call, id: u32, title: Str, markdown: Str, out: *mut u32) -> Status,
    /// Keyboard input; the text arrives in `on_ui_result`.
    pub input: unsafe extern "C" fn(call: *mut Call, prompt: Str, initial: Str, id: *mut u32) -> Status,
    /// Moves the reader to a position string.
    pub go_to: unsafe extern "C" fn(call: *mut Call, position: Str) -> Status,
    /// Asks the library to rescan the book folders.
    pub rescan_library: unsafe extern "C" fn(call: *mut Call) -> Status,
    /// A plugin setting as JSON into `*out`, or [`NOT_FOUND`].
    pub settings_get: unsafe extern "C" fn(call: *mut Call, key: Str, out: *mut OwnedStr) -> Status,
    /// Sets a plugin setting to JSON `value` (`null` removes it).
    pub settings_set: unsafe extern "C" fn(call: *mut Call, key: Str, value_json: Str) -> Status,
    /// A per-book value of the open book as JSON, or [`NOT_FOUND`].
    pub book_get: unsafe extern "C" fn(call: *mut Call, key: Str, out: *mut OwnedStr) -> Status,
    /// Sets a per-book value (`null` removes it).
    pub book_set: unsafe extern "C" fn(call: *mut Call, key: Str, value_json: Str) -> Status,
    /// The plugin's data folder (created on demand) into `*out`.
    pub data_dir: unsafe extern "C" fn(call: *mut Call, out: *mut OwnedStr) -> Status,
    /// A query on the open book: `method` and JSON `args`, JSON result into `*out`.
    /// Methods: `metadata`, `book_id`, `toc`, `search` (`{"query", "limit"}`),
    /// `read_around` (`{"position", "chars"}`), `links` (`{"position"}`),
    /// `current_position`, `selection`, `page`, `page_count`, `page_text` (`{"page"}`,
    /// default the current page). Pages are 0-based.
    pub doc: unsafe extern "C" fn(call: *mut Call, method: Str, args_json: Str, out: *mut OwnedStr) -> Status,
    /// Starts an HTTP request (JSON `{"url", "method", "headers": {..}, "body",
    /// "timeout", "stream": "chunks"|"sse", "limit"}`); events arrive in `on_http`
    /// with the id written to `*id`.
    pub http_request: unsafe extern "C" fn(call: *mut Call, options_json: Str, id: *mut u32) -> Status,
    /// Cancels a request.
    pub http_cancel: unsafe extern "C" fn(call: *mut Call, id: u32) -> Status,
}

// The tables only hold function pointers and opaque state the owning side guards.
unsafe impl Sync for HostApiV1 {}
unsafe impl Send for HostApiV1 {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_strings_round_trip() {
        let s = OwnedStr::from_string("héllo".into());
        assert_eq!(unsafe { s.as_str().as_str() }, Some("héllo"));
        unsafe { s.free() };
        assert!(OwnedStr::NONE.is_none());
        unsafe { OwnedStr::NONE.free() };
        assert_eq!(unsafe { Str::EMPTY.as_bytes() }, b"");
        assert_eq!(unsafe { Str::bytes(&[0xff]).as_str() }, None);
    }
}
