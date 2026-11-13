//! Native plugins: shared libraries in `<RINKY_HOME>/plugins/<name>/`, loaded at start-up.
//!
//! A native plugin folder holds a `plugin.toml` with `native = "libfoo.so"` and that
//! library, built against [`ink_plugin_abi`] (plugin authors use the
//! `ink-plugin-native` crate and its `export_plugin!`). [`load_dir`] opens every such
//! library with `dlopen`, looks up [`ink_plugin_abi::ENTRY_SYMBOL`], hands it the host
//! table ([`HOST_API`]) and wraps the returned table in a [`NativePlugin`], which
//! implements [`Plugin`] by calling through it. Registries, cards and settings treat it
//! like any other plugin.
//!
//! - Calls are plain function-pointer calls (no copies beyond the strings crossing the
//!   boundary). Structured values cross as JSON.
//! - Libraries are never unloaded: musl's `dlclose` does nothing, and unloading code
//!   another thread may still be in is unsafe. Replacing a plugin needs a restart.
//! - Panics are caught inside the plugin (by the code `export_plugin!` generates) and
//!   come back as [`PluginError::Crashed`], which turns the plugin off at once. A host
//!   function never unwinds into the plugin either: each catches its own panics.
//! - Every call into the library runs inside [`ink_device::trace::crash_scope`] with the
//!   plugin's name, so a fatal signal there (a segfault) is written to the crash marker
//!   and the next start turns the plugin off.
//! - On the macOS simulator a `native = "libfoo.so"` is looked up as `libfoo.dylib`.
//! - A native plugin is machine code in rinky's own process: the permissions in its
//!   `plugin.toml` limit what the host API gives it, not what the code can do.

use crate::host::HostCtx;
use crate::manifest::{valid_name, PluginManifest};
use crate::net::{HttpEvent, StreamMode, WHOLE_LIMIT};
use crate::registry::{Registrar, StatusSlot, TileState};
use crate::{Plugin, PluginError, Result, UiRequest};
use ink_device::trace;
use ink_doc::{Annotation, AnnotationPosition, LinkTarget, Position, Selection, TextAnnotator};
use ink_plugin_abi::{self as abi, Call, HostApiV1, OwnedStr, PluginV1, Status, Str};
use serde_json::{json, Value};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Largest text a plugin may put into a viewer or dialog body (as for Lua plugins).
const MAX_LONG_TEXT: usize = 4 << 20;

// ---------------------------------------------------------------------------------------
// Calls into the plugin

/// What a call can reach (as for Lua plugins).
enum Cx<'a, 'b> {
    /// `on_load`: registration only.
    Load(&'a mut Registrar<'b>),
    /// A hook with host services.
    Host(&'a mut HostCtx<'b>),
}

/// Behind the `*mut Call` the plugin gets for one hook.
struct CallState<'a, 'b> {
    cx: Cx<'a, 'b>,
    plugin: &'static str,
    next_ui: &'a mut u32,
    error: Option<String>,
}

type HostResult<T = ()> = std::result::Result<T, (Status, String)>;

fn status_of(e: &PluginError) -> Status {
    match e {
        PluginError::Permission(_) => abi::ERR_PERMISSION,
        PluginError::Unsupported(_) => abi::ERR_UNSUPPORTED,
        _ => abi::ERR_FAILED,
    }
}

fn perr(e: PluginError) -> (Status, String) {
    (status_of(&e), e.to_string())
}

fn arg_err(msg: impl Into<String>) -> (Status, String) {
    (abi::ERR_ARGUMENT, msg.into())
}

impl<'b> CallState<'_, 'b> {
    fn host(&mut self, what: &str) -> HostResult<&mut HostCtx<'b>> {
        match &mut self.cx {
            Cx::Host(h) => Ok(h),
            Cx::Load(_) => Err((abi::ERR_UNSUPPORTED, format!("{what} does not work while the plugin loads (on_load); only registrations do"))),
        }
    }

    fn reg(&mut self) -> HostResult<&mut Registrar<'b>> {
        match &mut self.cx {
            Cx::Load(r) => Ok(r),
            Cx::Host(_) => Err((abi::ERR_UNSUPPORTED, "registrations work only in on_load".into())),
        }
    }

    fn next_id(&mut self) -> u32 {
        let id = *self.next_ui;
        *self.next_ui = self.next_ui.wrapping_add(1).max(1);
        id
    }
}

/// # Safety
/// `s` borrowed for the call.
unsafe fn text<'x>(s: Str, what: &str) -> HostResult<&'x str> {
    s.as_str().ok_or_else(|| arg_err(format!("{what} is not UTF-8")))
}

/// # Safety
/// As [`text`].
unsafe fn json_arg(s: Str, what: &str) -> HostResult<Value> {
    let t = text(s, what)?;
    if t.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(t).map_err(|e| arg_err(format!("{what}: bad JSON: {e}")))
}

/// Runs a host function for the plugin: catches panics, keeps the error message.
///
/// # Safety
/// `call` is the `CallState` of the running hook.
unsafe fn host_call(call: *mut Call, f: impl FnOnce(&mut CallState<'_, '_>) -> HostResult) -> Status {
    if call.is_null() {
        return abi::ERR_ARGUMENT;
    }
    let st = call.cast::<CallState<'_, '_>>();
    // rinky's own code from here on: a crash in it (a hostile book in a document query,
    // say) is not the plugin's.
    let _host = trace::no_crash_scope();
    let r = catch_unwind(AssertUnwindSafe(|| f(&mut *st)));
    let (status, msg) = match r {
        Ok(Ok(())) => return abi::OK,
        Ok(Err((s, _))) if s == abi::NOT_FOUND => return s,
        Ok(Err(e)) => e,
        Err(_) => (abi::ERR_PANIC, "rinky's host function panicked (see the log)".into()),
    };
    (*st).error = Some(msg);
    status
}

/// # Safety
/// `out` is a valid pointer.
unsafe fn put<T>(out: *mut T, v: T) -> HostResult {
    if out.is_null() {
        return Err(arg_err("null out pointer"));
    }
    *out = v;
    Ok(())
}

fn pairs(v: &Value, what: &str) -> HostResult<Vec<(String, String)>> {
    let Some(a) = v.as_array() else {
        return if v.is_null() { Ok(Vec::new()) } else { Err(arg_err(format!("{what} must be an array of [label, action]"))) };
    };
    a.iter()
        .map(|p| match (p.get(0).and_then(Value::as_str), p.get(1).and_then(Value::as_str)) {
            (Some(l), Some(a)) => Ok((l.to_string(), a.to_string())),
            _ => Err(arg_err(format!("{what}: each item must be [label, action]"))),
        })
        .collect()
}

fn long(t: &str, what: &str) -> HostResult<String> {
    if t.len() > MAX_LONG_TEXT {
        return Err(arg_err(format!("{what}: text larger than {MAX_LONG_TEXT} bytes")));
    }
    Ok(t.to_string())
}

fn ui_request(st: &mut CallState, req: UiRequest, what: &str) -> HostResult {
    st.host(what)?.ui().map_err(perr)?.request(req);
    Ok(())
}

// Host functions ------------------------------------------------------------------------

unsafe extern "C" fn h_free_str(s: OwnedStr) {
    let _ = catch_unwind(AssertUnwindSafe(|| s.free()));
}

unsafe extern "C" fn h_take_error(call: *mut Call) -> OwnedStr {
    if call.is_null() {
        return OwnedStr::NONE;
    }
    let st = call.cast::<CallState<'_, '_>>();
    match (*st).error.take() {
        Some(m) => OwnedStr::from_string(m),
        None => OwnedStr::NONE,
    }
}

unsafe extern "C" fn h_log(call: *mut Call, t: Str) -> Status {
    host_call(call, |st| {
        let t = String::from_utf8_lossy(t.as_bytes());
        log::info!("plugin {}: {}", st.plugin, t.chars().take(2000).collect::<String>());
        Ok(())
    })
}

unsafe extern "C" fn h_register(call: *mut Call, kind: Str, id: Str, label: Str, extra: Str, order: i32) -> Status {
    host_call(call, |st| {
        let (kind, id, label, extra) = (text(kind, "kind")?, text(id, "id")?, text(label, "label")?, text(extra, "extra")?);
        let reg = st.reg()?;
        let or = |s: &str, d: &str| if s.is_empty() { d.to_string() } else { s.to_string() };
        match kind {
            "menu_item" => reg.menu_item(id, label, order),
            "selection_action" => reg.selection_action(id, label),
            "gesture" => reg.gesture(id, extra, &or(label, id)),
            "action" => reg.action(id, label),
            "settings_page" => reg.settings_page(id, label),
            "library_action" => reg.library_action(id, label),
            "control_tile" => {
                reg.control_tile(id, label, &or(extra, "info"));
            }
            "library_entry" => reg.library_entry(id, label, &or(extra, "more")),
            "status_widget" => {
                let slot_name = or(extra, "right");
                let slot = StatusSlot::parse(&slot_name).ok_or_else(|| arg_err(format!("status widget slot must be left, center or right, not {slot_name:?}")))?;
                reg.status_widget(id, slot, label);
            }
            other => return Err(arg_err(format!("unknown registration kind {other:?}"))),
        }
        Ok(())
    })
}

unsafe extern "C" fn h_toast(call: *mut Call, t: Str) -> Status {
    host_call(call, |st| ui_request(st, UiRequest::Toast { text: text(t, "text")?.into() }, "toast"))
}

unsafe extern "C" fn h_notify(call: *mut Call, t: Str) -> Status {
    host_call(call, |st| ui_request(st, UiRequest::Notification { text: text(t, "text")?.into() }, "notify"))
}

unsafe extern "C" fn h_status(call: *mut Call, id: Str, t: Str) -> Status {
    host_call(call, |st| ui_request(st, UiRequest::Status { id: text(id, "id")?.into(), text: text(t, "text")?.into() }, "status"))
}

unsafe extern "C" fn h_tile_state(call: *mut Call, id: Str, t: Str) -> Status {
    host_call(call, |st| ui_request(st, UiRequest::TileState { id: text(id, "id")?.into(), text: text(t, "text")?.into() }, "tile_state"))
}

/// A null out pointer, refused before anything is done.
fn need_out<T>(out: *mut T) -> HostResult {
    if out.is_null() {
        return Err(arg_err("null out pointer"));
    }
    Ok(())
}

unsafe extern "C" fn h_dialog(call: *mut Call, title: Str, body: Str, buttons: Str, out: *mut u32) -> Status {
    host_call(call, |st| {
        need_out(out)?;
        let mut buttons = pairs(&json_arg(buttons, "buttons")?, "buttons")?;
        if buttons.is_empty() {
            buttons.push(("OK".into(), "ok".into()));
        }
        let (title, body) = (text(title, "title")?.to_string(), long(text(body, "body")?, "dialog")?);
        st.host("dialog")?.ui().map_err(perr)?;
        let id = st.next_id();
        ui_request(st, UiRequest::Dialog { id, title, body, buttons }, "dialog")?;
        put(out, id)
    })
}

unsafe extern "C" fn h_menu(call: *mut Call, title: Str, items: Str, out: *mut u32) -> Status {
    host_call(call, |st| {
        need_out(out)?;
        let items = pairs(&json_arg(items, "items")?, "items")?;
        if items.len() > 1000 {
            return Err(arg_err("menu: more than 1000 items"));
        }
        let title = text(title, "title")?.to_string();
        st.host("menu")?.ui().map_err(perr)?;
        let id = st.next_id();
        ui_request(st, UiRequest::Menu { id, title, items }, "menu")?;
        put(out, id)
    })
}

unsafe extern "C" fn h_menu_label(call: *mut Call, id: u32, action: Str, label: Str) -> Status {
    host_call(call, |st| ui_request(st, UiRequest::MenuLabel { id, action: text(action, "action")?.into(), label: text(label, "label")?.into() }, "menu_label"))
}

unsafe extern "C" fn h_view(call: *mut Call, id: u32, title: Str, markdown: Str, out: *mut u32) -> Status {
    host_call(call, |st| {
        need_out(out)?;
        let (title, markdown) = (text(title, "title")?.to_string(), long(text(markdown, "markdown")?, "view")?);
        st.host("view")?.ui().map_err(perr)?;
        let id = if id == 0 { st.next_id() } else { id };
        ui_request(st, UiRequest::TextViewer { id, title, markdown }, "view")?;
        put(out, id)
    })
}

unsafe extern "C" fn h_input(call: *mut Call, prompt: Str, initial: Str, out: *mut u32) -> Status {
    host_call(call, |st| {
        need_out(out)?;
        let (prompt, initial) = (text(prompt, "prompt")?.to_string(), text(initial, "initial")?.to_string());
        st.host("input")?.ui().map_err(perr)?;
        let id = st.next_id();
        ui_request(st, UiRequest::TextInput { id, prompt, initial }, "input")?;
        put(out, id)
    })
}

unsafe extern "C" fn h_go_to(call: *mut Call, position: Str) -> Status {
    host_call(call, |st| ui_request(st, UiRequest::GoTo { position: text(position, "position")?.into() }, "go_to"))
}

unsafe extern "C" fn h_rescan_library(call: *mut Call) -> Status {
    host_call(call, |st| ui_request(st, UiRequest::LibraryRescan, "rescan_library"))
}

unsafe fn get_value(call: *mut Call, key: Str, out: *mut OwnedStr, book: bool) -> Status {
    host_call(call, |st| {
        need_out(out)?;
        let key = text(key, "key")?;
        let s = st.host("storage")?.storage().map_err(perr)?;
        match if book { s.book_get(key) } else { s.get(key) } {
            Some(v) => put(out, OwnedStr::from_string(v.to_string())),
            None => Err((abi::NOT_FOUND, String::new())),
        }
    })
}

unsafe fn set_value(call: *mut Call, key: Str, value: Str, book: bool) -> Status {
    host_call(call, |st| {
        let (key, value) = (text(key, "key")?, json_arg(value, "value")?);
        let s = st.host("storage")?.storage().map_err(perr)?;
        if book { s.book_set(key, value) } else { s.set(key, value) }.map_err(perr)
    })
}

unsafe extern "C" fn h_settings_get(call: *mut Call, key: Str, out: *mut OwnedStr) -> Status {
    get_value(call, key, out, false)
}

unsafe extern "C" fn h_settings_set(call: *mut Call, key: Str, value: Str) -> Status {
    set_value(call, key, value, false)
}

unsafe extern "C" fn h_book_get(call: *mut Call, key: Str, out: *mut OwnedStr) -> Status {
    get_value(call, key, out, true)
}

unsafe extern "C" fn h_book_set(call: *mut Call, key: Str, value: Str) -> Status {
    set_value(call, key, value, true)
}

unsafe extern "C" fn h_data_dir(call: *mut Call, out: *mut OwnedStr) -> Status {
    host_call(call, |st| {
        need_out(out)?;
        let dir = st.host("data_dir")?.storage().map_err(perr)?.data_dir();
        std::fs::create_dir_all(&dir).map_err(|e| (abi::ERR_FAILED, format!("{}: {e}", dir.display())))?;
        put(out, OwnedStr::from_string(dir.display().to_string()))
    })
}

unsafe extern "C" fn h_doc(call: *mut Call, method: Str, args: Str, out: *mut OwnedStr) -> Status {
    host_call(call, |st| {
        need_out(out)?;
        let (method, args) = (text(method, "method")?, json_arg(args, "args")?);
        let d = st.host("doc")?.document().map_err(perr)?;
        let pos = |args: &Value| -> HostResult<Position> {
            match args.get("position").and_then(Value::as_str) {
                Some(p) => p.parse::<Position>().map_err(|e| arg_err(format!("bad position {p:?}: {e}"))),
                None => d.current_position().map_err(perr),
            }
        };
        let v = match method {
            "metadata" => {
                let m = d.metadata();
                json!({
                    "title": m.title, "authors": m.authors, "language": m.language, "identifier": m.identifier,
                    "publisher": m.publisher, "description": m.description,
                    "series": m.series.as_ref().map(|(n, _)| n.clone()), "series_index": m.series.as_ref().map(|(_, i)| *i),
                })
            }
            "book_id" => json!(d.book_id()),
            "toc" => Value::Array(d.toc().iter().map(|e| json!({ "title": e.title, "level": e.level, "position": e.position.to_string(), "page": e.page })).collect()),
            "search" => {
                let q = args.get("query").and_then(Value::as_str).ok_or_else(|| arg_err("search: query is missing"))?;
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 500) as usize;
                let hits = d.search(q, limit).map_err(perr)?;
                Value::Array(
                    hits.iter()
                        .map(|h| json!({ "position": h.position.to_string(), "page": h.page, "excerpt": h.excerpt, "match_start": h.matched.start, "match_end": h.matched.end }))
                        .collect(),
                )
            }
            "read_around" => {
                let chars = args.get("chars").and_then(Value::as_u64).unwrap_or(1000).clamp(1, 100_000) as usize;
                let p = pos(&args)?;
                json!(d.read_around(&p, chars).map_err(perr)?)
            }
            "links" => {
                let p = pos(&args)?;
                let links = d.links(&p).map_err(perr)?;
                Value::Array(
                    links
                        .iter()
                        .map(|l| match &l.target {
                            LinkTarget::Internal(p) => json!({ "text": l.text, "footnote": l.footnote, "internal": true, "position": p.to_string() }),
                            LinkTarget::External(u) => json!({ "text": l.text, "footnote": l.footnote, "internal": false, "url": u }),
                        })
                        .collect(),
                )
            }
            "current_position" => json!(d.current_position().map_err(perr)?.to_string()),
            "selection" => d.selection().map_or(Value::Null, |s| selection_json(&s)),
            "page" => json!(d.current_page()),
            "page_count" => json!(d.page_count()),
            "page_text" => {
                let page = match args.get("page").and_then(Value::as_u64) {
                    Some(p) => p as usize,
                    None => d.current_page().ok_or_else(|| (abi::ERR_UNSUPPORTED, "current page".to_string()))?,
                };
                json!(d.page_text(page).map_err(perr)?)
            }
            other => return Err((abi::ERR_UNSUPPORTED, format!("doc: no method {other:?}"))),
        };
        put(out, OwnedStr::from_string(v.to_string()))
    })
}

unsafe extern "C" fn h_http_request(call: *mut Call, options: Str, out: *mut u32) -> Status {
    host_call(call, |st| {
        need_out(out)?;
        let o = json_arg(options, "options")?;
        let url = o.get("url").and_then(Value::as_str).ok_or_else(|| arg_err("http_request: url is missing"))?;
        let mut req = ink_net::Request::get(url);
        req.method = match o.get("method").and_then(Value::as_str).unwrap_or("GET").to_ascii_uppercase().as_str() {
            "GET" => ink_net::Method::Get,
            "POST" => ink_net::Method::Post,
            "PUT" => ink_net::Method::Put,
            "DELETE" => ink_net::Method::Delete,
            other => return Err(arg_err(format!("http_request: method {other} is not supported"))),
        };
        if let Some(h) = o.get("headers").and_then(Value::as_object) {
            for (k, v) in h {
                req.headers.push((k.clone(), v.as_str().unwrap_or_default().to_string()));
            }
        }
        match o.get("body") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => req.body = s.clone().into_bytes(),
            Some(other) => {
                req.body = other.to_string().into_bytes();
                if !req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                    req.headers.push(("Content-Type".into(), "application/json".into()));
                }
            }
        }
        if let Some(t) = o.get("timeout").and_then(Value::as_f64) {
            req.timeout = Duration::from_secs_f64(t.clamp(1.0, 600.0));
        }
        let mode = match o.get("stream").and_then(Value::as_str) {
            None => StreamMode::Whole { limit: o.get("limit").and_then(Value::as_u64).map_or(WHOLE_LIMIT, |l| l as usize).min(64 << 20) },
            Some("chunks") => StreamMode::Chunks,
            Some("sse") => StreamMode::Sse,
            Some(other) => return Err(arg_err(format!("http_request: stream must be \"chunks\" or \"sse\", not {other:?}"))),
        };
        let id = st.host("http_request")?.start_request(req, mode).map_err(perr)?;
        put(out, id)
    })
}

unsafe extern "C" fn h_http_cancel(call: *mut Call, id: u32) -> Status {
    host_call(call, |st| {
        st.host("http_cancel")?.cancel_request(id);
        Ok(())
    })
}

/// The table every native plugin gets.
pub static HOST_API: HostApiV1 = HostApiV1 {
    abi_version: abi::ABI_VERSION,
    free_str: h_free_str,
    take_error: h_take_error,
    log: h_log,
    register: h_register,
    toast: h_toast,
    notify: h_notify,
    status: h_status,
    tile_state: h_tile_state,
    dialog: h_dialog,
    menu: h_menu,
    menu_label: h_menu_label,
    view: h_view,
    input: h_input,
    go_to: h_go_to,
    rescan_library: h_rescan_library,
    settings_get: h_settings_get,
    settings_set: h_settings_set,
    book_get: h_book_get,
    book_set: h_book_set,
    data_dir: h_data_dir,
    doc: h_doc,
    http_request: h_http_request,
    http_cancel: h_http_cancel,
};

fn selection_json(s: &Selection) -> Value {
    json!({ "text": s.text, "start": s.start.to_string(), "end": s.end.to_string(), "page": s.page })
}

// ---------------------------------------------------------------------------------------
// The plugin

/// A loaded native plugin library as a [`Plugin`].
pub struct NativePlugin {
    manifest: PluginManifest,
    /// The plugin's name for [`trace::crash_scope`] (leaked once per plugin: libraries,
    /// and so plugins, live until the process ends).
    name: &'static str,
    table: *mut PluginV1,
    next_ui: u32,
    /// Set when a call outside the hooks (tile state, lookup claim, word bounds, an
    /// annotator on the pagination thread) panicked: the plugin is not called again
    /// and the manager turns it off ([`Plugin::crashed`]).
    crash: Arc<Crash>,
}

/// Takes a plugin-owned error string.
///
/// # Safety
/// `s` comes from the plugin of `table`.
unsafe fn plugin_text(table: &PluginV1, s: OwnedStr) -> String {
    let t = String::from_utf8_lossy(s.as_str().as_bytes()).into_owned();
    (table.free_str)(s);
    t
}

/// What [`NativePlugin`] and its annotators know about a panic outside the hooks.
#[derive(Default)]
struct Crash {
    hit: AtomicBool,
    what: Mutex<String>,
}

impl Crash {
    fn hit(&self) -> bool {
        self.hit.load(Ordering::Relaxed)
    }

    fn record(&self, what: &str) {
        if !self.hit.swap(true, Ordering::SeqCst) {
            if let Ok(mut w) = self.what.lock() {
                *w = format!("{what} panicked");
            }
        }
    }
}

impl NativePlugin {
    /// Loads the plugin folder `dir` (`plugin.toml` with `native`).
    pub fn load(dir: &Path) -> Result<Self> {
        let toml = dir.join("plugin.toml");
        let text = std::fs::read_to_string(&toml).map_err(|e| PluginError::Manifest(format!("{}: {e}", toml.display())))?;
        let manifest = PluginManifest::parse(&text)?;
        let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if folder != manifest.name {
            return Err(PluginError::Manifest(format!("plugin name {:?} differs from its folder name {folder:?}", manifest.name)));
        }
        let lib = manifest.native.as_deref().ok_or_else(|| PluginError::Manifest("plugin.toml has no native library".into()))?;
        Self::open(manifest.clone(), &library_path(dir, lib))
    }

    /// Opens `path` as the library of the plugin described by `manifest`.
    pub fn open(manifest: PluginManifest, path: &Path) -> Result<Self> {
        let name: &'static str = Box::leak(manifest.name.clone().into_boxed_str());
        let failed = |what: String| PluginError::Failed(format!("{}: {what}", path.display()));
        let _scope = trace::crash_scope(name);
        // SAFETY: loading a library runs its initialisers; that is the point of a native
        // plugin, and the crash scope blames a crash there on this plugin.
        let lib = unsafe { libloading::Library::new(path) }.map_err(|e| failed(e.to_string()))?;
        // SAFETY: the symbol has the entry function's type by the ABI contract.
        let entry: abi::EntryFn = unsafe {
            *lib.get::<abi::EntryFn>(abi::ENTRY_SYMBOL)
                .map_err(|_| failed("no rinky_plugin_v1 in the library (not a rinky plugin, or built without export_plugin!)".into()))?
        };
        // Never unloaded (see the module documentation).
        std::mem::forget(lib);
        // SAFETY: the entry function of a rinky plugin, given the host table.
        let table = unsafe { entry(&HOST_API) };
        if table.is_null() {
            return Err(failed(format!("the plugin refused rinky's plugin ABI {} (it was built for another version; see the log)", abi::ABI_VERSION)));
        }
        // SAFETY: a non-null table from the plugin; the version is its first field.
        let version = unsafe { (*table).abi_version };
        if version != abi::ABI_VERSION {
            return Err(failed(format!("the plugin was built for plugin ABI {version}; this rinky has {}", abi::ABI_VERSION)));
        }
        // SAFETY: a v1 table.
        let own = unsafe { (*table).name };
        let declared = unsafe { own((*table).state).as_str() }.unwrap_or("").to_string();
        if declared != manifest.name {
            return Err(failed(format!("the library calls itself {declared:?}, plugin.toml {:?}", manifest.name)));
        }
        Ok(NativePlugin { manifest, name, table, next_ui: 1, crash: Arc::default() })
    }

    fn t(&self) -> &PluginV1 {
        // SAFETY: the table lives until the process ends.
        unsafe { &*self.table }
    }

    /// Calls a hook with `cx`; maps the plugin's status to a result.
    fn hook(&mut self, cx: Cx, f: impl FnOnce(&PluginV1, *mut Call, *mut OwnedStr) -> Status) -> Result<()> {
        let _scope = trace::crash_scope(self.name);
        // SAFETY: the table lives until the process ends.
        let table = unsafe { &*self.table };
        let mut st = CallState { cx, plugin: self.name, next_ui: &mut self.next_ui, error: None };
        let mut err = OwnedStr::NONE;
        let s = f(table, (&mut st as *mut CallState).cast::<Call>(), &mut err);
        // SAFETY: err is NONE or a string of this plugin.
        let msg = unsafe { plugin_text(table, err) };
        match s {
            abi::OK => Ok(()),
            abi::ERR_PANIC => Err(PluginError::Crashed(if msg.is_empty() { "panic".into() } else { msg })),
            abi::ERR_PERMISSION => Err(PluginError::Permission(msg)),
            abi::ERR_UNSUPPORTED => Err(PluginError::Unsupported(msg)),
            other => Err(PluginError::Failed(if msg.is_empty() { format!("failed (status {other})") } else { msg })),
        }
    }

    fn with_host(&mut self, cx: &mut HostCtx, f: impl FnOnce(&PluginV1, *mut Call, *mut OwnedStr) -> Status) -> Result<()> {
        self.hook(Cx::Host(cx), f)
    }
}

/// `dir/lib`; on macOS a `.so` name is looked up as `.dylib`.
fn library_path(dir: &Path, lib: &str) -> PathBuf {
    let p = dir.join(lib);
    if cfg!(target_os = "macos") {
        if let Some(stem) = lib.strip_suffix(".so") {
            return dir.join(format!("{stem}.dylib"));
        }
    }
    p
}

fn lang_str(lang: Option<&str>) -> Str {
    Str::new(lang.unwrap_or(""))
}

impl Plugin for NativePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn kind(&self) -> &'static str {
        "native"
    }

    fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
        // SAFETY (all hooks below): table functions with the table's state, a live call
        // and a valid error slot.
        self.hook(Cx::Load(reg), |t, c, e| unsafe { (t.on_load)(t.state, c, e) })
    }

    fn on_book_open(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.with_host(cx, |t, c, e| unsafe { (t.on_book_open)(t.state, c, e) })
    }

    fn on_page_change(&mut self, cx: &mut HostCtx, page: usize) -> Result<()> {
        self.with_host(cx, |t, c, e| unsafe { (t.on_page_change)(t.state, c, page as u64, e) })
    }

    fn on_selection(&mut self, cx: &mut HostCtx, sel: &Selection) -> Result<()> {
        let j = selection_json(sel).to_string();
        self.with_host(cx, |t, c, e| unsafe { (t.on_selection)(t.state, c, Str::new(&j), e) })
    }

    fn on_suspend(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.with_host(cx, |t, c, e| unsafe { (t.on_suspend)(t.state, c, e) })
    }

    fn on_resume(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.with_host(cx, |t, c, e| unsafe { (t.on_resume)(t.state, c, e) })
    }

    fn on_close(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.with_host(cx, |t, c, e| unsafe { (t.on_close)(t.state, c, e) })
    }

    fn on_exit(&mut self, cx: &mut HostCtx, restarting: bool) -> Result<()> {
        self.with_host(cx, |t, c, e| unsafe { (t.on_exit)(t.state, c, u32::from(restarting), e) })
    }

    fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
        self.with_host(cx, |t, c, e| unsafe { (t.on_action)(t.state, c, Str::new(id), e) })
    }

    fn on_library_action(&mut self, cx: &mut HostCtx, id: &str, book: &Path) -> Result<()> {
        let b = book.display().to_string();
        self.with_host(cx, |t, c, e| unsafe { (t.on_library_action)(t.state, c, Str::new(id), Str::new(&b), e) })
    }

    fn tile_state(&self, id: &str) -> Option<TileState> {
        if self.crash.hit() {
            return None;
        }
        let _scope = trace::crash_scope(self.name);
        let t = self.t();
        let (mut out, mut on) = (OwnedStr::NONE, -1i32);
        // SAFETY: table function with valid out pointers; the string is the plugin's.
        let s = unsafe { (t.tile_state)(t.state, Str::new(id), &mut out, &mut on) };
        let text = unsafe { plugin_text(t, out) };
        if s == abi::ERR_PANIC {
            self.crash.record("tile_state");
        }
        (s == abi::OK).then(|| TileState { text, on: (on >= 0).then_some(on != 0) })
    }

    fn crashed(&self) -> Option<String> {
        self.crash.hit().then(|| self.crash.what.lock().map(|w| w.clone()).unwrap_or_default())
    }

    fn on_ui_result(&mut self, cx: &mut HostCtx, id: u32, result: &str) -> Result<()> {
        self.with_host(cx, |t, c, e| unsafe { (t.on_ui_result)(t.state, c, id, Str::new(result), e) })
    }

    fn on_http(&mut self, cx: &mut HostCtx, id: u32, ev: &HttpEvent) -> Result<()> {
        let none = String::new();
        let (kind, status, data, has_data, extra): (u32, u32, &[u8], u32, String) = match ev {
            HttpEvent::Response { status, headers } => (abi::http::RESPONSE, u32::from(*status), &[], 0, json!(headers).to_string()),
            HttpEvent::Chunk(b) => (abi::http::CHUNK, 0, b, 1, none),
            HttpEvent::Sse(s) => (abi::http::SSE, 0, s.data.as_bytes(), 1, json!({ "event": if s.event.is_empty() { "message" } else { &s.event }, "id": s.id }).to_string()),
            HttpEvent::Done { body } => (abi::http::DONE, 0, body.as_deref().unwrap_or(&[]), u32::from(body.is_some()), none),
            HttpEvent::Error(m) => (abi::http::ERROR, 0, m.as_bytes(), 1, none),
        };
        let raw = abi::HttpEventV1 { kind, status, data: Str::bytes(data), has_data, headers_json: Str::new(&extra) };
        self.with_host(cx, |t, c, e| unsafe { (t.on_http)(t.state, c, id, &raw, e) })
    }

    fn tick_interval(&self) -> Option<Duration> {
        if self.crash.hit() {
            return None;
        }
        let _scope = trace::crash_scope(self.name);
        let t = self.t();
        // SAFETY: table function.
        let ms = unsafe { (t.tick_interval_ms)(t.state) };
        (ms > 0).then(|| Duration::from_millis(ms))
    }

    fn on_tick(&mut self, cx: &mut HostCtx) -> Result<()> {
        self.with_host(cx, |t, c, e| unsafe { (t.on_tick)(t.state, c, e) })
    }

    /// Asked whenever a book opens (a new table each time), so a plugin may offer an
    /// annotator for some books or settings only.
    fn text_annotator(&self) -> Option<Arc<dyn TextAnnotator>> {
        if self.crash.hit() {
            return None;
        }
        let _scope = trace::crash_scope(self.name);
        let t = self.t();
        // SAFETY: table function; a non-null result is a table we now own.
        let a = unsafe { (t.annotator)(t.state) };
        (!a.is_null()).then(|| Arc::new(NativeAnnotator::new(a, self.name, self.crash.clone())) as Arc<dyn TextAnnotator>)
    }

    fn claims_lookup(&self, text: &str, lang: Option<&str>) -> bool {
        if self.crash.hit() {
            return false;
        }
        let _scope = trace::crash_scope(self.name);
        let t = self.t();
        // SAFETY: table function.
        let r = unsafe { (t.claims_lookup)(t.state, Str::new(text), lang_str(lang)) };
        if r < 0 {
            self.crash.record("claims_lookup");
        }
        r > 0
    }

    fn word_bounds(&self, text: &str, at: usize, lang: Option<&str>) -> Option<std::ops::Range<usize>> {
        if self.crash.hit() {
            return None;
        }
        let _scope = trace::crash_scope(self.name);
        let t = self.t();
        let (mut start, mut end) = (0u64, 0u64);
        // SAFETY: table function with valid out pointers.
        let s = unsafe { (t.word_bounds)(t.state, Str::new(text), at as u64, lang_str(lang), &mut start, &mut end) };
        if s == abi::ERR_PANIC {
            self.crash.record("word_bounds");
        }
        (s == abi::OK).then_some(start as usize..end as usize)
    }
}

// ---------------------------------------------------------------------------------------
// Annotators

/// A plugin's [`abi::AnnotatorV1`] as a [`TextAnnotator`].
struct NativeAnnotator {
    table: *mut abi::AnnotatorV1,
    plugin: &'static str,
    /// Shared with the plugin: once the annotator panics it is not called again.
    crash: Arc<Crash>,
    id: String,
    positions: Vec<AnnotationPosition>,
}

// SAFETY: the plugin side requires its annotator to be Send + Sync (the
// `ink-plugin-native` trait does); the table itself is immutable.
unsafe impl Send for NativeAnnotator {}
unsafe impl Sync for NativeAnnotator {}

impl NativeAnnotator {
    fn new(table: *mut abi::AnnotatorV1, plugin: &'static str, crash: Arc<Crash>) -> Self {
        // SAFETY: a fresh table from the plugin.
        let t = unsafe { &*table };
        let id = unsafe { (t.id)(t.state).as_str() }.unwrap_or(plugin).to_string();
        let bits = unsafe { (t.positions)(t.state) };
        let mut positions = Vec::new();
        if bits & abi::position::OVER != 0 {
            positions.push(AnnotationPosition::Over);
        }
        if bits & abi::position::UNDER != 0 {
            positions.push(AnnotationPosition::Under);
        }
        if positions.is_empty() {
            positions.push(AnnotationPosition::Over);
        }
        NativeAnnotator { table, plugin, crash, id, positions }
    }

    fn t(&self) -> &abi::AnnotatorV1 {
        // SAFETY: owned until drop.
        unsafe { &*self.table }
    }
}

impl Drop for NativeAnnotator {
    fn drop(&mut self) {
        let _scope = trace::crash_scope(self.plugin);
        // SAFETY: we own the table; drop frees it.
        unsafe { (self.t().drop)(self.table) };
    }
}

impl TextAnnotator for NativeAnnotator {
    fn id(&self) -> &str {
        &self.id
    }

    fn version(&self) -> u64 {
        if self.crash.hit() {
            return 0;
        }
        let _scope = trace::crash_scope(self.plugin);
        // SAFETY: table function.
        unsafe { (self.t().version)(self.t().state) }
    }

    fn layout_version(&self) -> u64 {
        if self.crash.hit() {
            return 0;
        }
        let _scope = trace::crash_scope(self.plugin);
        // SAFETY: table function.
        unsafe { (self.t().layout_version)(self.t().state) }
    }

    fn positions(&self) -> &[AnnotationPosition] {
        &self.positions
    }

    fn ideograph_spacing(&self, lang: Option<&str>) -> f32 {
        if self.crash.hit() {
            return 0.0;
        }
        let _scope = trace::crash_scope(self.plugin);
        // SAFETY: table function.
        let v = unsafe { (self.t().ideograph_spacing)(self.t().state, lang_str(lang)) };
        if v.is_finite() { v.clamp(0.0, 2.0) } else { 0.0 }
    }

    fn wants(&self, paragraph: &str, lang: Option<&str>) -> bool {
        if self.crash.hit() {
            return false;
        }
        let _scope = trace::crash_scope(self.plugin);
        // SAFETY: table function.
        let r = unsafe { (self.t().wants)(self.t().state, Str::new(paragraph), lang_str(lang)) };
        if r < 0 {
            self.crash.record("the annotator (wants)");
        }
        r > 0
    }

    fn annotate(&self, paragraph: &str, lang: Option<&str>) -> Vec<Annotation> {
        if self.crash.hit() {
            return Vec::new();
        }
        let _scope = trace::crash_scope(self.plugin);
        let t = self.t();
        let mut list = abi::AnnotationListV1 { ptr: std::ptr::null_mut(), len: 0 };
        // SAFETY: table function with a valid out pointer.
        let s = unsafe { (t.annotate)(t.state, Str::new(paragraph), lang_str(lang), &mut list) };
        if s == abi::ERR_PANIC {
            self.crash.record("the annotator (annotate)");
        }
        if s != abi::OK || list.ptr.is_null() {
            if !list.ptr.is_null() {
                // SAFETY: a list the plugin filled anyway is still its to free.
                unsafe { (t.free_list)(list) };
            }
            return Vec::new();
        }
        // SAFETY: the plugin's list of `len` annotations, read before it is freed.
        let items = unsafe { std::slice::from_raw_parts(list.ptr, list.len) };
        let out = items
            .iter()
            .filter(|a| a.start < a.end && a.end <= paragraph.len() && paragraph.is_char_boundary(a.start) && paragraph.is_char_boundary(a.end))
            .map(|a| {
                // SAFETY: the text is owned by the list, which is still alive.
                let text = unsafe { String::from_utf8_lossy(a.text.as_str().as_bytes()).into_owned() };
                if a.position == abi::position::UNDER {
                    Annotation::under(a.start..a.end, text)
                } else {
                    Annotation::over(a.start..a.end, text)
                }
            })
            .collect();
        // SAFETY: the list came from this annotator's annotate.
        unsafe { (t.free_list)(list) };
        out
    }
}

// ---------------------------------------------------------------------------------------
// Loading

/// True when `dir/plugin.toml` names a native library.
pub fn is_native_dir(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("plugin.toml")).ok().and_then(|t| PluginManifest::parse(&t).ok()).is_some_and(|m| m.native.is_some())
}

/// Loads every native plugin under `root` (folders whose `plugin.toml` has `native`):
/// `(plugins, errors)` where an error is `(folder name, message)`.
pub fn load_dir(root: &Path) -> (Vec<NativePlugin>, Vec<(String, String)>) {
    let mut plugins = Vec::new();
    let mut errors = Vec::new();
    for dir in crate::lua::plugin_dirs(root).into_iter().filter(|d| is_native_dir(d)) {
        let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !valid_name(&folder) {
            errors.push((folder.clone(), format!("folder name {folder:?} is not a valid plugin name (a-z, 0-9, _ and -)")));
            continue;
        }
        match NativePlugin::load(&dir) {
            Ok(p) => plugins.push(p),
            Err(e) => errors.push((folder, e.to_string())),
        }
    }
    (plugins, errors)
}

