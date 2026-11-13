//! Write a native rinky plugin in safe Rust.
//!
//! A native plugin is a `cdylib` crate that depends on this crate, implements
//! [`Plugin`] for a type with a [`Default`] value and exports it:
//!
//! ```text
//! use ink_plugin_native::{export_plugin, Host, Plugin, Registrar, Result};
//!
//! #[derive(Default)]
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     const NAME: &'static str = "hello";
//!     fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
//!         reg.menu_item("hi", "Say hello", 0)
//!     }
//!     fn on_action(&mut self, host: &mut Host, _id: &str) -> Result<()> {
//!         host.toast("Hello")
//!     }
//! }
//!
//! export_plugin!(Hello);
//! ```
//!
//! [`export_plugin!`] generates the C entry function `rinky_plugin_v1` and the table of
//! `extern "C"` hooks ([`ink_plugin_abi::PluginV1`]). Every generated hook catches
//! panics inside the plugin and reports them to rinky as an error, so a panic never
//! crosses into rinky (where it could not be caught and would end the process). rinky
//! counts such errors like any other plugin error and turns a plugin off after
//! repeated failures. A crash that is not a panic (a segfault in `unsafe` code or in a
//! C library) ends rinky; it then restarts with the plugin turned off.
//!
//! The host services mirror those of Lua plugins (`docs/plugins.md`) and are checked
//! against the permissions in `plugin.toml` in the same way. Structured values are
//! [`serde_json::Value`]s. Pages are 0-based.

#![warn(missing_docs)]

use ink_plugin_abi::{self as abi, Call, HostApiV1, OwnedStr, PluginV1, Status, Str};
use std::ffi::c_void;
use std::fmt;
use std::ops::Range;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub use ink_plugin_abi;
pub use serde_json::{self, json, Value};

/// An error of a plugin hook or a host call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    /// The [`ink_plugin_abi`] status (`ERR_*`).
    pub status: Status,
    /// What went wrong.
    pub message: String,
}

impl Error {
    /// A plain failure with `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Error { status: abi::ERR_FAILED, message: message.into() }
    }

    /// True when a permission in `plugin.toml` was missing.
    pub fn is_permission(&self) -> bool {
        self.status == abi::ERR_PERMISSION
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::new(s)
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::new(s)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error { status: abi::ERR_ARGUMENT, message: e.to_string() }
    }
}

/// Result of hooks and host calls.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The text the user selected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// The text.
    pub text: String,
    /// Start position string.
    pub start: String,
    /// End position string (exclusive).
    pub end: String,
    /// Page the selection was made on (0-based).
    pub page: usize,
}

impl Selection {
    fn from_json(v: &Value) -> Option<Selection> {
        Some(Selection {
            text: v.get("text")?.as_str()?.to_string(),
            start: v.get("start").and_then(Value::as_str).unwrap_or_default().to_string(),
            end: v.get("end").and_then(Value::as_str).unwrap_or_default().to_string(),
            page: v.get("page").and_then(Value::as_u64).unwrap_or(0) as usize,
        })
    }
}

/// An event of a request started with [`Host::http_request`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpEvent {
    /// Status and headers (names lowercase).
    Response {
        /// Status code.
        status: u16,
        /// Header name/value pairs.
        headers: Vec<(String, String)>,
    },
    /// A piece of the body (`stream: chunks`).
    Chunk(Vec<u8>),
    /// A server-sent event (`stream: sse`).
    Sse {
        /// `event:` field (`message` when the server sent none).
        event: String,
        /// Joined `data:` lines.
        data: String,
        /// Last `id:`.
        id: Option<String>,
    },
    /// The request finished; `body` is set unless the request streamed.
    Done {
        /// The whole body.
        body: Option<Vec<u8>>,
    },
    /// The request failed.
    Error(String),
}

/// How a request delivers its body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    /// The whole body in [`HttpEvent::Done`], at most this many bytes.
    Whole(usize),
    /// Pieces as they arrive.
    Chunks,
    /// Server-sent events.
    Sse,
}

/// An HTTP request for [`Host::http_request`].
#[derive(Clone, Debug)]
pub struct HttpRequest {
    /// `http://` or `https://` URL (its host must be allowed by `hosts` in plugin.toml).
    pub url: String,
    /// `GET`, `POST`, `PUT` or `DELETE`.
    pub method: String,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
    /// Body text.
    pub body: Option<String>,
    /// Timeout in seconds (1 to 600).
    pub timeout_secs: Option<f64>,
    /// Body delivery.
    pub stream: Stream,
}

impl HttpRequest {
    /// A GET of `url` with the whole body delivered at the end (up to 4 MiB).
    pub fn get(url: impl Into<String>) -> Self {
        HttpRequest { url: url.into(), method: "GET".into(), headers: Vec::new(), body: None, timeout_secs: None, stream: Stream::Whole(4 << 20) }
    }

    /// A POST of `body` to `url`.
    pub fn post(url: impl Into<String>, body: impl Into<String>) -> Self {
        HttpRequest { method: "POST".into(), body: Some(body.into()), ..Self::get(url) }
    }

    fn to_json(&self) -> Value {
        let headers: serde_json::Map<String, Value> = self.headers.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
        let mut o = json!({ "url": self.url, "method": self.method, "headers": headers });
        if let Some(b) = &self.body {
            o["body"] = json!(b);
        }
        if let Some(t) = self.timeout_secs {
            o["timeout"] = json!(t);
        }
        match self.stream {
            Stream::Whole(limit) => o["limit"] = json!(limit),
            Stream::Chunks => o["stream"] = json!("chunks"),
            Stream::Sse => o["stream"] = json!("sse"),
        }
        o
    }
}

/// The state of a control-center tile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileState {
    /// Line under the caption.
    pub text: String,
    /// `Some(on)` draws a toggle, `None` an action tile.
    pub on: Option<bool>,
}

/// Where an annotation is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnnotationPosition {
    /// Above the text.
    #[default]
    Over,
    /// Below the text.
    Under,
}

/// One annotation: the text in `range` (bytes of the paragraph) gets `text` drawn at
/// `position`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Annotation {
    /// Byte range on char boundaries, not empty.
    pub range: Range<usize>,
    /// The annotation text.
    pub text: String,
    /// Above or below.
    pub position: AnnotationPosition,
}

/// Annotations drawn over or under paragraphs of reflowable books (like ruby). The same
/// contract as rinky's own `TextAnnotator`: called on the pagination thread for every
/// paragraph, so it must be quick and thread-safe.
pub trait TextAnnotator: Send + Sync {
    /// A short stable name.
    fn id(&self) -> &str;
    /// Changes whenever [`TextAnnotator::annotate`] may answer differently.
    fn version(&self) -> u64;
    /// Changes whenever `wants`, `positions` or `ideograph_spacing` may change.
    fn layout_version(&self) -> u64 {
        0
    }
    /// True when the paragraph may get annotations.
    fn wants(&self, paragraph: &str, lang: Option<&str>) -> bool;
    /// The positions drawn in.
    fn positions(&self) -> &[AnnotationPosition] {
        &[AnnotationPosition::Over]
    }
    /// Extra space between ideographs, in em.
    fn ideograph_spacing(&self, _lang: Option<&str>) -> f32 {
        0.0
    }
    /// The annotations of one paragraph.
    fn annotate(&self, paragraph: &str, lang: Option<&str>) -> Vec<Annotation>;
}

/// A native plugin. Every hook but [`Plugin::NAME`] has a default that does nothing.
/// Hooks run on rinky's UI thread; keep them short.
pub trait Plugin: 'static {
    /// The plugin name; must equal `name` in `plugin.toml`.
    const NAME: &'static str;

    /// Start-up: register menu items, actions, gestures and tiles.
    fn on_load(&mut self, _reg: &mut Registrar) -> Result<()> {
        Ok(())
    }
    /// A book was opened.
    fn on_book_open(&mut self, _host: &mut Host) -> Result<()> {
        Ok(())
    }
    /// The reader moved to `page` (0-based).
    fn on_page_change(&mut self, _host: &mut Host, _page: usize) -> Result<()> {
        Ok(())
    }
    /// The user selected text.
    fn on_selection(&mut self, _host: &mut Host, _sel: &Selection) -> Result<()> {
        Ok(())
    }
    /// The device is going to sleep.
    fn on_suspend(&mut self, _host: &mut Host) -> Result<()> {
        Ok(())
    }
    /// The device woke up.
    fn on_resume(&mut self, _host: &mut Host) -> Result<()> {
        Ok(())
    }
    /// The book was closed.
    fn on_close(&mut self, _host: &mut Host) -> Result<()> {
        Ok(())
    }
    /// rinky is about to exit (`restarting`: a new rinky follows at once).
    fn on_exit(&mut self, _host: &mut Host, _restarting: bool) -> Result<()> {
        Ok(())
    }
    /// A registered entry was triggered (a settings page as `settings:<id>`).
    fn on_action(&mut self, _host: &mut Host, _id: &str) -> Result<()> {
        Ok(())
    }
    /// A library action on the book file `book`. Defaults to [`Plugin::on_action`].
    fn on_library_action(&mut self, host: &mut Host, id: &str, _book: &str) -> Result<()> {
        self.on_action(host, id)
    }
    /// The answer to UI request `id` (a dialog's or menu's action, input text, `closed`
    /// or a tapped link of a viewer; `""` when cancelled).
    fn on_ui_result(&mut self, _host: &mut Host, _id: u32, _result: &str) -> Result<()> {
        Ok(())
    }
    /// An event of request `id`.
    fn on_http(&mut self, _host: &mut Host, _id: u32, _ev: &HttpEvent) -> Result<()> {
        Ok(())
    }
    /// How often [`Plugin::on_tick`] should run, or `None` for never (asked after every
    /// call; at least 100 ms).
    fn tick_interval(&self) -> Option<Duration> {
        None
    }
    /// Periodic call while [`Plugin::tick_interval`] is `Some`.
    fn on_tick(&mut self, _host: &mut Host) -> Result<()> {
        Ok(())
    }
    /// The state of control tile `id`, or `None` for the registered text.
    fn tile_state(&self, _id: &str) -> Option<TileState> {
        None
    }
    /// Whether this plugin takes the dictionary lookup of `text` (when it registered the
    /// selection action `lookup`).
    fn claims_lookup(&self, _text: &str, _lang: Option<&str>) -> bool {
        true
    }
    /// The word around character `at` of `text`, as character indices.
    fn word_bounds(&self, _text: &str, _at: usize, _lang: Option<&str>) -> Option<Range<usize>> {
        None
    }
    /// A text annotator (asked when a book opens).
    fn text_annotator(&self) -> Option<Arc<dyn TextAnnotator>> {
        None
    }
}

// ---------------------------------------------------------------------------------------
// Host services

static HOST: AtomicPtr<HostApiV1> = AtomicPtr::new(std::ptr::null_mut());

fn api() -> &'static HostApiV1 {
    let p = HOST.load(Ordering::Acquire);
    assert!(!p.is_null(), "rinky host table not set (the plugin was not loaded by rinky)");
    // SAFETY: set once from the entry function to rinky's table, which lives forever.
    unsafe { &*p }
}

/// rinky's services during one hook call. Not usable after the hook returns.
pub struct Host<'a> {
    call: *mut Call,
    _hook: std::marker::PhantomData<&'a mut ()>,
}

/// Converts a host status into a result, taking the host's message.
fn check(call: *mut Call, s: Status) -> Result<()> {
    if s == abi::OK {
        return Ok(());
    }
    let a = api();
    // SAFETY: take_error with the live call; the message is ours to free with free_str.
    let message = unsafe {
        let m = (a.take_error)(call);
        let text = m.as_str().as_str().unwrap_or("").to_string();
        (a.free_str)(m);
        text
    };
    let message = if message.is_empty() { format!("host call failed (status {s})") } else { message };
    Err(Error { status: s, message })
}

/// Takes a host-owned string.
fn take_host(s: OwnedStr) -> String {
    let a = api();
    // SAFETY: `s` was allocated by the host and is freed once here.
    unsafe {
        let text = String::from_utf8_lossy(s.as_str().as_bytes()).into_owned();
        (a.free_str)(s);
        text
    }
}

fn pairs_json(pairs: &[(&str, &str)]) -> String {
    Value::Array(pairs.iter().map(|(l, a)| json!([l, a])).collect()).to_string()
}

impl Host<'_> {
    fn call(&self) -> *mut Call {
        self.call
    }

    fn status(&self, s: Status) -> Result<()> {
        check(self.call, s)
    }

    fn get_json(&self, f: unsafe extern "C" fn(*mut Call, Str, *mut OwnedStr) -> Status, key: &str) -> Result<Option<Value>> {
        let mut out = OwnedStr::NONE;
        // SAFETY: host function with the live call and a valid out pointer.
        let s = unsafe { f(self.call(), Str::new(key), &mut out) };
        if s == abi::NOT_FOUND {
            return Ok(None);
        }
        self.status(s)?;
        Ok(Some(serde_json::from_str(&take_host(out))?))
    }

    fn id_call(&self, f: impl FnOnce(*mut u32) -> Status) -> Result<u32> {
        let mut id = 0u32;
        let s = f(&mut id);
        self.status(s)?;
        Ok(id)
    }

    /// A line in rinky's log.
    pub fn log(&mut self, text: &str) {
        // SAFETY: host function with the live call.
        let _ = unsafe { (api().log)(self.call(), Str::new(text)) };
    }

    /// A short message near the bottom of the screen (`ui` permission).
    pub fn toast(&mut self, text: &str) -> Result<()> {
        // SAFETY: as above.
        self.status(unsafe { (api().toast)(self.call(), Str::new(text)) })
    }

    /// A notification that stays in the list until read.
    pub fn notify(&mut self, text: &str) -> Result<()> {
        // SAFETY: as above.
        self.status(unsafe { (api().notify)(self.call(), Str::new(text)) })
    }

    /// New text of status-bar widget `id`.
    pub fn status_text(&mut self, id: &str, text: &str) -> Result<()> {
        // SAFETY: as above.
        self.status(unsafe { (api().status)(self.call(), Str::new(id), Str::new(text)) })
    }

    /// New state text of control tile `id`.
    pub fn tile_state(&mut self, id: &str, text: &str) -> Result<()> {
        // SAFETY: as above.
        self.status(unsafe { (api().tile_state)(self.call(), Str::new(id), Str::new(text)) })
    }

    /// A dialog with `(label, action)` buttons (none: one OK button). Returns the
    /// request id; the chosen action arrives in [`Plugin::on_ui_result`].
    pub fn dialog(&mut self, title: &str, body: &str, buttons: &[(&str, &str)]) -> Result<u32> {
        let b = pairs_json(buttons);
        // SAFETY: as above.
        self.id_call(|id| unsafe { (api().dialog)(self.call, Str::new(title), Str::new(body), Str::new(&b), id) })
    }

    /// A menu of `(label, action)` items. Returns the request id.
    pub fn menu(&mut self, title: &str, items: &[(&str, &str)]) -> Result<u32> {
        let it = pairs_json(items);
        // SAFETY: as above.
        self.id_call(|id| unsafe { (api().menu)(self.call, Str::new(title), Str::new(&it), id) })
    }

    /// New label of the item with `action` in open menu `id`.
    pub fn menu_label(&mut self, id: u32, action: &str, label: &str) -> Result<()> {
        // SAFETY: as above.
        self.status(unsafe { (api().menu_label)(self.call(), id, Str::new(action), Str::new(label)) })
    }

    /// Opens the Markdown viewer (a card); returns its id.
    pub fn view(&mut self, title: &str, markdown: &str) -> Result<u32> {
        // SAFETY: as above.
        self.id_call(|out| unsafe { (api().view)(self.call, 0, Str::new(title), Str::new(markdown), out) })
    }

    /// Replaces the text of open viewer `id` (streamed answers).
    pub fn update_view(&mut self, id: u32, title: &str, markdown: &str) -> Result<()> {
        // SAFETY: as above.
        self.id_call(|out| unsafe { (api().view)(self.call, id, Str::new(title), Str::new(markdown), out) }).map(|_| ())
    }

    /// Keyboard input; the text arrives in [`Plugin::on_ui_result`].
    pub fn input(&mut self, prompt: &str, initial: &str) -> Result<u32> {
        // SAFETY: as above.
        self.id_call(|id| unsafe { (api().input)(self.call, Str::new(prompt), Str::new(initial), id) })
    }

    /// Moves the reader to a position string.
    pub fn go_to(&mut self, position: &str) -> Result<()> {
        // SAFETY: as above.
        self.status(unsafe { (api().go_to)(self.call(), Str::new(position)) })
    }

    /// Asks the library to rescan the book folders.
    pub fn rescan_library(&mut self) -> Result<()> {
        // SAFETY: as above.
        self.status(unsafe { (api().rescan_library)(self.call()) })
    }

    /// A plugin setting (`storage` permission).
    pub fn setting(&mut self, key: &str) -> Result<Option<Value>> {
        self.get_json(api().settings_get, key)
    }

    /// Sets a plugin setting (`Value::Null` removes it).
    pub fn set_setting(&mut self, key: &str, value: &Value) -> Result<()> {
        let v = value.to_string();
        // SAFETY: as above.
        self.status(unsafe { (api().settings_set)(self.call(), Str::new(key), Str::new(&v)) })
    }

    /// A value stored for the open book.
    pub fn book_value(&mut self, key: &str) -> Result<Option<Value>> {
        self.get_json(api().book_get, key)
    }

    /// Stores a value for the open book (`Value::Null` removes it).
    pub fn set_book_value(&mut self, key: &str, value: &Value) -> Result<()> {
        let v = value.to_string();
        // SAFETY: as above.
        self.status(unsafe { (api().book_set)(self.call(), Str::new(key), Str::new(&v)) })
    }

    /// The plugin's data folder (`storage` permission).
    pub fn data_dir(&mut self) -> Result<PathBuf> {
        let mut out = OwnedStr::NONE;
        // SAFETY: as above.
        self.status(unsafe { (api().data_dir)(self.call(), &mut out) })?;
        Ok(PathBuf::from(take_host(out)))
    }

    /// A query on the open book (`document` permission); see
    /// [`ink_plugin_abi::HostApiV1::doc`] for the methods.
    pub fn doc(&mut self, method: &str, args: &Value) -> Result<Value> {
        let a = args.to_string();
        let mut out = OwnedStr::NONE;
        // SAFETY: as above.
        self.status(unsafe { (api().doc)(self.call(), Str::new(method), Str::new(&a), &mut out) })?;
        Ok(serde_json::from_str(&take_host(out))?)
    }

    /// Text of `page` (0-based; `None`: the current page).
    pub fn page_text(&mut self, page: Option<usize>) -> Result<String> {
        let v = self.doc("page_text", &json!({ "page": page }))?;
        Ok(v.as_str().unwrap_or_default().to_string())
    }

    /// The current selection.
    pub fn selection(&mut self) -> Result<Option<Selection>> {
        let v = self.doc("selection", &Value::Null)?;
        Ok(Selection::from_json(&v))
    }

    /// The reader's position string.
    pub fn current_position(&mut self) -> Result<String> {
        let v = self.doc("current_position", &Value::Null)?;
        Ok(v.as_str().unwrap_or_default().to_string())
    }

    /// Starts `req` (`network` permission); events arrive in [`Plugin::on_http`] with
    /// the returned id.
    pub fn http_request(&mut self, req: &HttpRequest) -> Result<u32> {
        let o = req.to_json().to_string();
        // SAFETY: as above.
        self.id_call(|id| unsafe { (api().http_request)(self.call, Str::new(&o), id) })
    }

    /// Cancels request `id`.
    pub fn http_cancel(&mut self, id: u32) -> Result<()> {
        // SAFETY: as above.
        self.status(unsafe { (api().http_cancel)(self.call(), id) })
    }
}

/// Registration during [`Plugin::on_load`].
pub struct Registrar<'a> {
    host: Host<'a>,
}

impl Registrar<'_> {
    fn register(&mut self, kind: &str, id: &str, label: &str, extra: &str, order: i32) -> Result<()> {
        // SAFETY: host function with the live call.
        self.host.status(unsafe { (api().register)(self.host.call(), Str::new(kind), Str::new(id), Str::new(label), Str::new(extra), order) })
    }

    /// A reader menu item (`order`: lower first).
    pub fn menu_item(&mut self, id: &str, label: &str, order: i32) -> Result<()> {
        self.register("menu_item", id, label, "", order)
    }

    /// A selection action (`lookup` makes the plugin a dictionary lookup provider).
    pub fn selection_action(&mut self, id: &str, label: &str) -> Result<()> {
        self.register("selection_action", id, label, "", 0)
    }

    /// A gesture binding (gesture names as in `docs/plugins.md`).
    pub fn gesture(&mut self, id: &str, gesture: &str, label: &str) -> Result<()> {
        self.register("gesture", id, label, gesture, 0)
    }

    /// An action the user can bind to gestures.
    pub fn action(&mut self, id: &str, label: &str) -> Result<()> {
        self.register("action", id, label, "", 0)
    }

    /// A settings page; opening it calls [`Plugin::on_action`] with `settings:<id>`.
    pub fn settings_page(&mut self, id: &str, label: &str) -> Result<()> {
        self.register("settings_page", id, label, "", 0)
    }

    /// An action in the library's book menu.
    pub fn library_action(&mut self, id: &str, label: &str) -> Result<()> {
        self.register("library_action", id, label, "", 0)
    }

    /// A control-center tile.
    pub fn control_tile(&mut self, id: &str, label: &str, icon: &str) -> Result<()> {
        self.register("control_tile", id, label, icon, 0)
    }

    /// An entry on the library screen.
    pub fn library_entry(&mut self, id: &str, label: &str, icon: &str) -> Result<()> {
        self.register("library_entry", id, label, icon, 0)
    }

    /// A status-bar widget in `slot` (`left`, `center` or `right`).
    pub fn status_widget(&mut self, id: &str, slot: &str, text: &str) -> Result<()> {
        self.register("status_widget", id, text, slot, 0)
    }

    /// A log line.
    pub fn log(&mut self, text: &str) {
        self.host.log(text)
    }
}

// ---------------------------------------------------------------------------------------
// The generated C table

fn opt(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// Text of a caught panic.
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "(no message)".into()
    }
}

/// Runs `f`, catching panics; writes the message of a failure to `err`.
fn guard(err: *mut OwnedStr, f: impl FnOnce() -> Result<()>) -> Status {
    let (status, msg) = match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => return abi::OK,
        Ok(Err(e)) => (if e.status == abi::OK { abi::ERR_FAILED } else { e.status }, e.message),
        Err(p) => (abi::ERR_PANIC, format!("panic: {}", panic_text(p.as_ref()))),
    };
    if !err.is_null() {
        // SAFETY: rinky passes a valid out pointer; the string is freed by our free_str.
        unsafe { *err = OwnedStr::from_string(msg) };
    }
    status
}

/// Arguments that are not UTF-8.
fn bad_utf8() -> Error {
    Error { status: abi::ERR_ARGUMENT, message: "argument is not UTF-8".into() }
}

/// # Safety
/// `s` borrowed for the call.
unsafe fn text<'a>(s: Str) -> Result<&'a str> {
    s.as_str().ok_or_else(bad_utf8)
}

#[doc(hidden)]
pub mod __export {
    //! Used by [`crate::export_plugin!`]; not a public API.
    use super::*;

    /// The plugin object behind `PluginV1::state`.
    pub struct State<P> {
        plugin: P,
    }

    /// # Safety
    /// `state` is a `State<P>` made by [`entry`].
    unsafe fn me<'a, P>(state: *mut c_void) -> &'a mut P {
        &mut (*state.cast::<State<P>>()).plugin
    }

    fn host<'a>(call: *mut Call) -> Host<'a> {
        Host { call, _hook: std::marker::PhantomData }
    }

    /// The entry function's body: checks the host version and builds the table.
    ///
    /// # Safety
    /// `host_api` is rinky's table (or null).
    pub unsafe fn entry<P: Plugin + Default>(host_api: *const HostApiV1) -> *mut PluginV1 {
        if host_api.is_null() || (*host_api).abi_version != abi::ABI_VERSION {
            let seen = if host_api.is_null() { 0 } else { (*host_api).abi_version };
            eprintln!("plugin {}: rinky plugin ABI {seen}, this plugin needs {}", P::NAME, abi::ABI_VERSION);
            return std::ptr::null_mut();
        }
        HOST.store(host_api.cast_mut(), Ordering::Release);
        let Ok(plugin) = catch_unwind(P::default) else {
            eprintln!("plugin {}: Default panicked", P::NAME);
            return std::ptr::null_mut();
        };
        let state = Box::into_raw(Box::new(State { plugin })).cast::<c_void>();
        Box::into_raw(Box::new(PluginV1 {
            abi_version: abi::ABI_VERSION,
            state,
            name: name::<P>,
            free_str,
            on_load: on_load::<P>,
            on_book_open: on_book_open::<P>,
            on_page_change: on_page_change::<P>,
            on_selection: on_selection::<P>,
            on_suspend: on_suspend::<P>,
            on_resume: on_resume::<P>,
            on_close: on_close::<P>,
            on_exit: on_exit::<P>,
            on_action: on_action::<P>,
            on_library_action: on_library_action::<P>,
            on_ui_result: on_ui_result::<P>,
            on_http: on_http::<P>,
            tick_interval_ms: tick_interval_ms::<P>,
            on_tick: on_tick::<P>,
            tile_state: tile_state::<P>,
            claims_lookup: claims_lookup::<P>,
            word_bounds: word_bounds::<P>,
            annotator: annotator::<P>,
            drop: drop_table::<P>,
        }))
    }

    unsafe extern "C" fn name<P: Plugin>(_state: *mut c_void) -> Str {
        Str::new(P::NAME)
    }

    unsafe extern "C" fn free_str(s: OwnedStr) {
        let _ = catch_unwind(AssertUnwindSafe(|| s.free()));
    }

    unsafe extern "C" fn on_load<P: Plugin>(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_load(&mut Registrar { host: host(call) }))
    }

    unsafe extern "C" fn on_book_open<P: Plugin>(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_book_open(&mut host(call)))
    }

    unsafe extern "C" fn on_page_change<P: Plugin>(state: *mut c_void, call: *mut Call, page: u64, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_page_change(&mut host(call), page as usize))
    }

    unsafe extern "C" fn on_selection<P: Plugin>(state: *mut c_void, call: *mut Call, sel: Str, err: *mut OwnedStr) -> Status {
        guard(err, || {
            let v: Value = serde_json::from_str(text(sel)?)?;
            let sel = Selection::from_json(&v).ok_or("selection without text")?;
            me::<P>(state).on_selection(&mut host(call), &sel)
        })
    }

    unsafe extern "C" fn on_suspend<P: Plugin>(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_suspend(&mut host(call)))
    }

    unsafe extern "C" fn on_resume<P: Plugin>(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_resume(&mut host(call)))
    }

    unsafe extern "C" fn on_close<P: Plugin>(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_close(&mut host(call)))
    }

    unsafe extern "C" fn on_exit<P: Plugin>(state: *mut c_void, call: *mut Call, restarting: u32, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_exit(&mut host(call), restarting != 0))
    }

    unsafe extern "C" fn on_action<P: Plugin>(state: *mut c_void, call: *mut Call, id: Str, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_action(&mut host(call), text(id)?))
    }

    unsafe extern "C" fn on_library_action<P: Plugin>(state: *mut c_void, call: *mut Call, id: Str, book: Str, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_library_action(&mut host(call), text(id)?, text(book)?))
    }

    unsafe extern "C" fn on_ui_result<P: Plugin>(state: *mut c_void, call: *mut Call, id: u32, result: Str, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_ui_result(&mut host(call), id, text(result)?))
    }

    unsafe extern "C" fn on_http<P: Plugin>(state: *mut c_void, call: *mut Call, id: u32, ev: *const abi::HttpEventV1, err: *mut OwnedStr) -> Status {
        guard(err, || {
            let ev = http_event(ev.as_ref().ok_or("no event")?)?;
            me::<P>(state).on_http(&mut host(call), id, &ev)
        })
    }

    unsafe fn http_event(e: &abi::HttpEventV1) -> Result<HttpEvent> {
        let data = e.data.as_bytes();
        let extra = || -> Result<Value> {
            let t = text(e.headers_json)?;
            Ok(if t.is_empty() { Value::Null } else { serde_json::from_str(t)? })
        };
        Ok(match e.kind {
            abi::http::RESPONSE => {
                let headers = extra()?
                    .as_array()
                    .map(|a| a.iter().filter_map(|p| Some((p.get(0)?.as_str()?.to_string(), p.get(1)?.as_str()?.to_string()))).collect())
                    .unwrap_or_default();
                HttpEvent::Response { status: e.status as u16, headers }
            }
            abi::http::CHUNK => HttpEvent::Chunk(data.to_vec()),
            abi::http::SSE => {
                let x = extra()?;
                HttpEvent::Sse {
                    event: x.get("event").and_then(Value::as_str).unwrap_or("message").to_string(),
                    data: String::from_utf8_lossy(data).into_owned(),
                    id: x.get("id").and_then(Value::as_str).map(String::from),
                }
            }
            abi::http::DONE => HttpEvent::Done { body: (e.has_data != 0).then(|| data.to_vec()) },
            abi::http::ERROR => HttpEvent::Error(String::from_utf8_lossy(data).into_owned()),
            k => return Err(format!("unknown HTTP event kind {k}").into()),
        })
    }

    unsafe extern "C" fn tick_interval_ms<P: Plugin>(state: *mut c_void) -> u64 {
        catch_unwind(AssertUnwindSafe(|| me::<P>(state).tick_interval().map_or(0, |d| d.as_millis().clamp(1, u64::MAX as u128) as u64))).unwrap_or(0)
    }

    unsafe extern "C" fn on_tick<P: Plugin>(state: *mut c_void, call: *mut Call, err: *mut OwnedStr) -> Status {
        guard(err, || me::<P>(state).on_tick(&mut host(call)))
    }

    unsafe extern "C" fn tile_state<P: Plugin>(state: *mut c_void, id: Str, out: *mut OwnedStr, on: *mut i32) -> Status {
        let r = catch_unwind(AssertUnwindSafe(|| me::<P>(state).tile_state(id.as_str()?)));
        match r {
            Ok(Some(t)) => {
                *out = OwnedStr::from_string(t.text);
                *on = t.on.map_or(-1, i32::from);
                abi::OK
            }
            Ok(None) => abi::NOT_FOUND,
            Err(_) => abi::ERR_PANIC,
        }
    }

    unsafe extern "C" fn claims_lookup<P: Plugin>(state: *mut c_void, t: Str, lang: Str) -> i32 {
        let r = catch_unwind(AssertUnwindSafe(|| {
            let (Some(t), Some(lang)) = (t.as_str(), lang.as_str()) else { return -1 };
            i32::from(me::<P>(state).claims_lookup(t, opt(lang)))
        }));
        r.unwrap_or(-1)
    }

    unsafe extern "C" fn word_bounds<P: Plugin>(state: *mut c_void, t: Str, at: u64, lang: Str, start: *mut u64, end: *mut u64) -> Status {
        let r = catch_unwind(AssertUnwindSafe(|| {
            let (Some(t), Some(lang)) = (t.as_str(), lang.as_str()) else { return None };
            me::<P>(state).word_bounds(t, at as usize, opt(lang))
        }));
        match r {
            Ok(Some(range)) => {
                *start = range.start as u64;
                *end = range.end as u64;
                abi::OK
            }
            Ok(None) => abi::NOT_FOUND,
            Err(_) => abi::ERR_PANIC,
        }
    }

    unsafe extern "C" fn annotator<P: Plugin>(state: *mut c_void) -> *mut abi::AnnotatorV1 {
        match catch_unwind(AssertUnwindSafe(|| me::<P>(state).text_annotator())) {
            Ok(Some(a)) => annotator_table(a),
            _ => std::ptr::null_mut(),
        }
    }

    unsafe extern "C" fn drop_table<P: Plugin>(table: *mut PluginV1) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if table.is_null() {
                return;
            }
            let t = Box::from_raw(table);
            drop(Box::from_raw(t.state.cast::<State<P>>()));
        }));
    }

    // Annotators -----------------------------------------------------------------------

    type Ann = Arc<dyn TextAnnotator>;

    fn annotator_table(a: Ann) -> *mut abi::AnnotatorV1 {
        Box::into_raw(Box::new(abi::AnnotatorV1 {
            state: Box::into_raw(Box::new(a)).cast::<c_void>(),
            id: ann_id,
            version: ann_version,
            layout_version: ann_layout_version,
            positions: ann_positions,
            ideograph_spacing: ann_spacing,
            wants: ann_wants,
            annotate: ann_annotate,
            free_list: ann_free_list,
            drop: ann_drop,
        }))
    }

    unsafe fn ann<'a>(state: *mut c_void) -> &'a Ann {
        &*state.cast::<Ann>()
    }

    unsafe extern "C" fn ann_id(state: *mut c_void) -> Str {
        catch_unwind(AssertUnwindSafe(|| Str::new(ann(state).id()))).unwrap_or(Str::EMPTY)
    }

    unsafe extern "C" fn ann_version(state: *mut c_void) -> u64 {
        catch_unwind(AssertUnwindSafe(|| ann(state).version())).unwrap_or(0)
    }

    unsafe extern "C" fn ann_layout_version(state: *mut c_void) -> u64 {
        catch_unwind(AssertUnwindSafe(|| ann(state).layout_version())).unwrap_or(0)
    }

    unsafe extern "C" fn ann_positions(state: *mut c_void) -> u32 {
        catch_unwind(AssertUnwindSafe(|| {
            ann(state).positions().iter().fold(0, |bits, p| {
                bits | match p {
                    AnnotationPosition::Over => abi::position::OVER,
                    AnnotationPosition::Under => abi::position::UNDER,
                }
            })
        }))
        .unwrap_or(abi::position::OVER)
    }

    unsafe extern "C" fn ann_spacing(state: *mut c_void, lang: Str) -> f32 {
        catch_unwind(AssertUnwindSafe(|| ann(state).ideograph_spacing(lang.as_str().and_then(opt)))).unwrap_or(0.0)
    }

    unsafe extern "C" fn ann_wants(state: *mut c_void, p: Str, lang: Str) -> i32 {
        catch_unwind(AssertUnwindSafe(|| match p.as_str() {
            Some(p) => i32::from(ann(state).wants(p, lang.as_str().and_then(opt))),
            None => -1,
        }))
        .unwrap_or(-1)
    }

    unsafe extern "C" fn ann_annotate(state: *mut c_void, p: Str, lang: Str, out: *mut abi::AnnotationListV1) -> Status {
        let r = catch_unwind(AssertUnwindSafe(|| {
            let p = p.as_str()?;
            Some(ann(state).annotate(p, lang.as_str().and_then(opt)))
        }));
        let list = match r {
            Ok(Some(list)) => list,
            Ok(None) => return abi::ERR_ARGUMENT,
            Err(_) => return abi::ERR_PANIC,
        };
        let items: Box<[abi::AnnotationV1]> = list
            .into_iter()
            .map(|a| abi::AnnotationV1 {
                start: a.range.start,
                end: a.range.end,
                text: OwnedStr::from_string(a.text),
                position: match a.position {
                    AnnotationPosition::Over => abi::position::OVER,
                    AnnotationPosition::Under => abi::position::UNDER,
                },
            })
            .collect();
        let len = items.len();
        *out = abi::AnnotationListV1 { ptr: Box::into_raw(items).cast::<abi::AnnotationV1>(), len };
        abi::OK
    }

    unsafe extern "C" fn ann_free_list(list: abi::AnnotationListV1) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if list.ptr.is_null() {
                return;
            }
            let items = Box::from_raw(std::ptr::slice_from_raw_parts_mut(list.ptr, list.len));
            for a in items.into_vec() {
                a.text.free();
            }
        }));
    }

    unsafe extern "C" fn ann_drop(table: *mut abi::AnnotatorV1) {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if table.is_null() {
                return;
            }
            let t = Box::from_raw(table);
            drop(Box::from_raw(t.state.cast::<Ann>()));
        }));
    }
}

/// Exports `$plugin` (a type implementing [`Plugin`] and [`Default`]) as the plugin of
/// this library: generates the C entry function `rinky_plugin_v1`.
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        /// Entry point rinky looks up after loading this library.
        ///
        /// # Safety
        /// Called by rinky with its host table.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn rinky_plugin_v1(host: *const $crate::ink_plugin_abi::HostApiV1) -> *mut $crate::ink_plugin_abi::PluginV1 {
            $crate::__export::entry::<$plugin>(host)
        }
    };
}
