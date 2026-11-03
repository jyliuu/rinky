//! Services the app offers to plugins.
//!
//! - [`DocumentAccess`]: the open book (the five Insightful tools map onto it:
//!   `search_book`, `read_around`, `list_links`, `toc`, `current_position`).
//! - [`UiHost`]: UI requests. Plugins never block on UI: they queue a [`UiRequest`] and
//!   get the answer later through `Plugin::on_ui_result`. The app presents the requests
//!   with ink-ui widgets (`crates/ink-app/src/plugin_host.rs`).
//! - [`PluginStorage`]: per-plugin settings, per-book data and a data directory
//!   ([`data_path`] keeps file names inside it).
//! - Network: [`HostCtx::send`] (blocking) and [`HostCtx::start_request`] (streaming, in
//!   a worker thread, see [`crate::net`]), both only with the `network` permission and
//!   only to the manifest's `hosts`.
//!
//! Rust plugins are compiled into the binary and trusted; the fields of [`HostCtx`] are
//! public for them. The checked accessors ([`HostCtx::document`], [`HostCtx::ui`],
//! [`HostCtx::storage`], [`HostCtx::http`], [`HostCtx::send`],
//! [`HostCtx::start_request`]) apply the manifest's permissions; the Lua host uses only
//! those, so Lua plugins get exactly what their `plugin.toml` asks for.

use crate::manifest::Permission;
use crate::net::{NetHost, StreamMode};
use crate::{PluginError, Result};
use ink_device::Point;
use ink_doc::{Document, Link, Metadata, Position, SearchHit, Selection, TocEntry};
use ink_net::{HttpClient, Request, Response};
use std::path::{Component, Path, PathBuf};

/// Read access to the open book.
pub trait DocumentAccess {
    /// Metadata.
    fn metadata(&self) -> Metadata;
    /// Book id (KOReader partial MD5).
    fn book_id(&self) -> Option<String>;
    /// Table of contents.
    fn toc(&self) -> Vec<TocEntry>;
    /// Up to `limit` matches.
    fn search(&mut self, query: &str, limit: usize) -> Result<Vec<SearchHit>>;
    /// Text around a position.
    fn read_around(&mut self, pos: &Position, chars: usize) -> Result<String>;
    /// Links near a position.
    fn links(&mut self, pos: &Position) -> Result<Vec<Link>>;
    /// Where the reader is.
    fn current_position(&self) -> Result<Position>;
    /// The current selection.
    fn selection(&self) -> Option<Selection>;
    /// The page the reader shows (0-based), when known.
    fn current_page(&self) -> Option<usize> {
        None
    }
    /// Number of pages for the current layout, when known.
    fn page_count(&self) -> Option<usize> {
        None
    }
    /// Text of page `page` (0-based), when the engine can tell.
    fn page_text(&mut self, _page: usize) -> Result<String> {
        Err(PluginError::Unsupported("page text".into()))
    }
}

/// [`DocumentAccess`] over an open [`Document`].
pub struct DocAccess<'a> {
    /// The document.
    pub doc: &'a mut dyn Document,
    /// Its partial MD5.
    pub book_id: Option<String>,
}

fn doc_err(e: ink_doc::DocError) -> PluginError {
    match e {
        ink_doc::DocError::Unsupported(what) => PluginError::Unsupported(what),
        other => PluginError::Failed(other.to_string()),
    }
}

impl DocumentAccess for DocAccess<'_> {
    fn metadata(&self) -> Metadata {
        self.doc.metadata().clone()
    }
    fn book_id(&self) -> Option<String> {
        self.book_id.clone()
    }
    fn toc(&self) -> Vec<TocEntry> {
        self.doc.toc()
    }
    fn search(&mut self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.doc.search(query, limit).map_err(doc_err)
    }
    fn read_around(&mut self, pos: &Position, chars: usize) -> Result<String> {
        self.doc.read_around(pos, chars).map_err(doc_err)
    }
    fn links(&mut self, pos: &Position) -> Result<Vec<Link>> {
        self.doc.links(pos).map_err(doc_err)
    }
    fn current_position(&self) -> Result<Position> {
        self.doc.current_position().map_err(doc_err)
    }
    fn selection(&self) -> Option<Selection> {
        self.doc.selection().cloned()
    }
    fn current_page(&self) -> Option<usize> {
        Some(self.doc.current_page())
    }
    fn page_count(&self) -> Option<usize> {
        Some(self.doc.page_count().pages)
    }
    /// Selects the whole page and returns its text. Refused while the user has a
    /// selection (it would be replaced); engines without selection return
    /// [`PluginError::Unsupported`].
    fn page_text(&mut self, page: usize) -> Result<String> {
        if page >= self.doc.page_count().pages {
            return Err(PluginError::Failed(format!("no page {}", page + 1)));
        }
        if self.doc.selection().is_some() {
            return Err(PluginError::Unsupported("page text while text is selected".into()));
        }
        let l = self.doc.layout().clone();
        let to = Point::new(l.width.saturating_sub(1) as i32, l.height.saturating_sub(1) as i32);
        let sel = self.doc.select(page, Point::new(0, 0), to).map_err(doc_err);
        self.doc.clear_selection();
        Ok(sel?.map(|s| s.text).unwrap_or_default())
    }
}

/// A UI request from a plugin. `id` comes back in `on_ui_result` (ids are chosen by the
/// plugin and only need to be unique within it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiRequest {
    /// Short message near the bottom; no result.
    Toast {
        /// Text.
        text: String,
    },
    /// Message with buttons; result is the chosen button's action (`""` when dismissed
    /// with the back key).
    Dialog {
        /// Request id.
        id: u32,
        /// Title.
        title: String,
        /// Body.
        body: String,
        /// `(label, action)` pairs.
        buttons: Vec<(String, String)>,
    },
    /// Menu; result is the chosen item's action (`""` when cancelled).
    Menu {
        /// Request id.
        id: u32,
        /// Title.
        title: String,
        /// `(label, action)` pairs.
        items: Vec<(String, String)>,
    },
    /// Full-screen text viewer with Markdown; result `closed` or a tapped link URL.
    /// Sending another `TextViewer` with the id of the open viewer replaces its text
    /// (streamed answers) and keeps it open.
    TextViewer {
        /// Request id.
        id: u32,
        /// Title.
        title: String,
        /// Markdown text.
        markdown: String,
    },
    /// Keyboard input; result is the entered text (or empty when cancelled).
    TextInput {
        /// Request id.
        id: u32,
        /// Prompt.
        prompt: String,
        /// Initial text.
        initial: String,
    },
    /// Notification that stays in a list until read (also shown briefly like a toast).
    Notification {
        /// Text.
        text: String,
    },
    /// New text for one of the plugin's status-bar widgets (registered in `on_load`).
    Status {
        /// Widget id.
        id: String,
        /// Text.
        text: String,
    },
    /// Move the reader to `position` (a position string as `current_position` returns,
    /// e.g. a crengine xpointer or `page:N`); ignored when no book is open. No result.
    GoTo {
        /// Position string.
        position: String,
    },
    /// The book folders changed (a download finished): the library rescans when it is
    /// shown next. No result.
    LibraryRescan,
    /// New state text for one of the plugin's control-center tiles
    /// ([`crate::registry::ControlTile`]); used when the plugin has no
    /// [`crate::Plugin::tile_state`] (Lua plugins).
    TileState {
        /// Tile id.
        id: String,
        /// Text under the caption.
        text: String,
    },
    /// New label for the item with action `action` in the plugin's menu `id` while that
    /// menu is open (or waiting to open). Only that row is repainted. Ignored when the
    /// menu or item does not exist. No result.
    MenuLabel {
        /// Menu request id.
        id: u32,
        /// Action of the item.
        action: String,
        /// New label (same `label<TAB>detail` convention as menu items).
        label: String,
    },
}

impl UiRequest {
    /// The request id for requests that answer, `None` for toasts, notifications and
    /// status updates.
    pub fn id(&self) -> Option<u32> {
        match self {
            UiRequest::Dialog { id, .. }
            | UiRequest::Menu { id, .. }
            | UiRequest::TextViewer { id, .. }
            | UiRequest::TextInput { id, .. } => Some(*id),
            UiRequest::Toast { .. } | UiRequest::Notification { .. } | UiRequest::Status { .. } | UiRequest::GoTo { .. } | UiRequest::LibraryRescan | UiRequest::TileState { .. } | UiRequest::MenuLabel { .. } => None,
        }
    }
}

/// Where UI requests go. The convenience methods build [`UiRequest`]s.
pub trait UiHost {
    /// Queues a request.
    fn request(&mut self, req: UiRequest);

    /// A toast.
    fn toast(&mut self, text: &str) {
        self.request(UiRequest::Toast { text: text.into() });
    }
    /// A notification.
    fn notify(&mut self, text: &str) {
        self.request(UiRequest::Notification { text: text.into() });
    }
    /// A dialog with `(label, action)` buttons.
    fn dialog(&mut self, id: u32, title: &str, body: &str, buttons: &[(&str, &str)]) {
        let buttons = buttons.iter().map(|(l, a)| (l.to_string(), a.to_string())).collect();
        self.request(UiRequest::Dialog { id, title: title.into(), body: body.into(), buttons });
    }
    /// A menu with `(label, action)` items.
    fn menu(&mut self, id: u32, title: &str, items: &[(&str, &str)]) {
        let items = items.iter().map(|(l, a)| (l.to_string(), a.to_string())).collect();
        self.request(UiRequest::Menu { id, title: title.into(), items });
    }
    /// The Markdown viewer.
    fn view_markdown(&mut self, id: u32, title: &str, markdown: &str) {
        self.request(UiRequest::TextViewer { id, title: title.into(), markdown: markdown.into() });
    }
    /// Keyboard input.
    fn input(&mut self, id: u32, prompt: &str, initial: &str) {
        self.request(UiRequest::TextInput { id, prompt: prompt.into(), initial: initial.into() });
    }
    /// New status-bar text.
    fn status(&mut self, id: &str, text: &str) {
        self.request(UiRequest::Status { id: id.into(), text: text.into() });
    }

    /// New state text of a control-center tile ([`UiRequest::TileState`]).
    fn tile_state(&mut self, id: &str, text: &str) {
        self.request(UiRequest::TileState { id: id.into(), text: text.into() });
    }

    /// New label of one row of an open menu ([`UiRequest::MenuLabel`]).
    fn menu_label(&mut self, id: u32, action: &str, label: &str) {
        self.request(UiRequest::MenuLabel { id, action: action.into(), label: label.into() });
    }

    /// Moves the reader to `position` ([`UiRequest::GoTo`]).
    fn go_to(&mut self, position: &str) {
        self.request(UiRequest::GoTo { position: position.into() });
    }

    /// Asks the library to rescan the book folders ([`UiRequest::LibraryRescan`]).
    fn rescan_library(&mut self) {
        self.request(UiRequest::LibraryRescan);
    }
}

/// Collects requests in a list (the app drains it after each plugin call; tests inspect it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UiQueue(pub Vec<UiRequest>);

impl UiHost for UiQueue {
    fn request(&mut self, req: UiRequest) {
        self.0.push(req);
    }
}

/// Plugin storage.
pub trait PluginStorage {
    /// A plugin setting.
    fn get(&self, key: &str) -> Option<serde_json::Value>;
    /// Sets a plugin setting (`null` removes it).
    fn set(&mut self, key: &str, value: serde_json::Value) -> Result<()>;
    /// Per-book value for the open book.
    fn book_get(&self, key: &str) -> Option<serde_json::Value>;
    /// Sets a per-book value for the open book (`null` removes it).
    fn book_set(&mut self, key: &str, value: serde_json::Value) -> Result<()>;
    /// `plugins/<name>/data`, created on demand.
    fn data_dir(&self) -> PathBuf;
}

/// In-memory storage (tests and plugins without persistence).
#[derive(Clone, Debug, Default)]
pub struct MemoryStorage {
    /// Settings.
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// Per-book values.
    pub book: serde_json::Map<String, serde_json::Value>,
    /// Data directory.
    pub dir: PathBuf,
}

fn put(map: &mut serde_json::Map<String, serde_json::Value>, key: &str, value: serde_json::Value) {
    if value.is_null() {
        map.remove(key);
    } else {
        map.insert(key.into(), value);
    }
}

impl PluginStorage for MemoryStorage {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.settings.get(key).cloned()
    }
    fn set(&mut self, key: &str, value: serde_json::Value) -> Result<()> {
        put(&mut self.settings, key, value);
        Ok(())
    }
    fn book_get(&self, key: &str) -> Option<serde_json::Value> {
        self.book.get(key).cloned()
    }
    fn book_set(&mut self, key: &str, value: serde_json::Value) -> Result<()> {
        put(&mut self.book, key, value);
        Ok(())
    }
    fn data_dir(&self) -> PathBuf {
        self.dir.clone()
    }
}

/// `dir/name` for a relative file name inside a plugin's data directory. Refuses empty
/// names, absolute paths, `..` and `.` components, backslashes and NUL bytes, so a plugin
/// cannot leave its directory. Subdirectories (`notes/a.txt`) are allowed.
pub fn data_path(dir: &Path, name: &str) -> Result<PathBuf> {
    let bad = || PluginError::Permission(format!("file name {name:?} is outside the plugin's data folder"));
    if name.is_empty() || name.len() > 1024 || name.contains(['\\', '\0']) {
        return Err(bad());
    }
    let p = Path::new(name);
    let mut out = dir.to_path_buf();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            _ => return Err(bad()),
        }
    }
    if out == dir {
        return Err(bad());
    }
    Ok(out)
}

/// What a plugin call gets. Build it with [`HostCtx::new`] and the `with_*` methods.
#[non_exhaustive]
pub struct HostCtx<'a> {
    /// The open book, if any.
    pub doc: Option<&'a mut dyn DocumentAccess>,
    /// UI.
    pub ui: &'a mut dyn UiHost,
    /// Storage of this plugin.
    pub storage: &'a mut dyn PluginStorage,
    /// Blocking HTTP client (see [`HostCtx::http`] and [`HostCtx::send`]).
    pub net: Option<&'a dyn HttpClient>,
    /// Streaming requests in worker threads (see [`HostCtx::start_request`]).
    pub jobs: Option<&'a mut dyn NetHost>,
    /// The calling plugin's permissions.
    pub permissions: &'a [Permission],
    /// Hosts the calling plugin may contact (empty = any).
    pub hosts: &'a [String],
}

impl<'a> HostCtx<'a> {
    /// A context without book, network or host list.
    pub fn new(ui: &'a mut dyn UiHost, storage: &'a mut dyn PluginStorage, permissions: &'a [Permission]) -> Self {
        HostCtx { doc: None, ui, storage, net: None, jobs: None, permissions, hosts: &[] }
    }

    /// Sets the open book.
    pub fn with_doc(mut self, doc: Option<&'a mut dyn DocumentAccess>) -> Self {
        self.doc = doc;
        self
    }

    /// Sets the blocking HTTP client.
    pub fn with_net(mut self, net: Option<&'a dyn HttpClient>) -> Self {
        self.net = net;
        self
    }

    /// Sets the streaming request runner.
    pub fn with_jobs(mut self, jobs: Option<&'a mut dyn NetHost>) -> Self {
        self.jobs = jobs;
        self
    }

    /// Sets the allowed hosts (the manifest's `hosts`).
    pub fn with_hosts(mut self, hosts: &'a [String]) -> Self {
        self.hosts = hosts;
        self
    }

    fn need(&self, p: Permission) -> Result<()> {
        if self.permissions.contains(&p) {
            Ok(())
        } else {
            Err(PluginError::Permission(format!("{} (add it to permissions in plugin.toml)", p.name())))
        }
    }

    fn check_url(&self, url: &str) -> Result<()> {
        self.need(Permission::Network)?;
        let m = crate::PluginManifest { hosts: self.hosts.to_vec(), ..crate::PluginManifest::new("x", "0", &[]) };
        if crate::manifest::url_host(url).is_none() {
            return Err(PluginError::Failed(format!("not an http(s) URL: {url}")));
        }
        if !m.allows_url(url) {
            return Err(PluginError::Permission(format!("network host of {url} is not in hosts of plugin.toml")));
        }
        Ok(())
    }

    /// The HTTP client, or [`PluginError::Permission`] without the `network` permission.
    pub fn http(&self) -> Result<&dyn HttpClient> {
        self.need(Permission::Network)?;
        self.net.ok_or_else(|| PluginError::Unsupported("no network in this context".into()))
    }

    /// Sends `req` and waits for the response headers (blocking; the body streams).
    /// Checks the permission and the host list.
    pub fn send(&self, req: Request) -> Result<Response> {
        self.check_url(&req.url)?;
        self.http()?.send(req).map_err(|e| PluginError::Failed(e.to_string()))
    }

    /// Starts `req` in a worker thread; events arrive in `Plugin::on_http` with the
    /// returned id. Checks the permission and the host list.
    pub fn start_request(&mut self, req: Request, mode: StreamMode) -> Result<u32> {
        self.check_url(&req.url)?;
        match self.jobs.as_deref_mut() {
            Some(j) => j.start(req, mode),
            None => Err(PluginError::Unsupported("no network in this context".into())),
        }
    }

    /// Cancels a request started with [`HostCtx::start_request`].
    pub fn cancel_request(&mut self, id: u32) {
        if let Some(j) = self.jobs.as_deref_mut() {
            j.cancel(id);
        }
    }

    /// The open book, or [`PluginError::Permission`] without the `document` permission.
    pub fn document(&mut self) -> Result<&mut dyn DocumentAccess> {
        self.need(Permission::Document)?;
        match self.doc.as_deref_mut() {
            Some(d) => Ok(d),
            None => Err(PluginError::Unsupported("no book is open".into())),
        }
    }

    /// The UI, or [`PluginError::Permission`] without the `ui` permission.
    pub fn ui(&mut self) -> Result<&mut dyn UiHost> {
        self.need(Permission::Ui)?;
        Ok(&mut *self.ui)
    }

    /// The storage, or [`PluginError::Permission`] without the `storage` permission.
    pub fn storage(&mut self) -> Result<&mut dyn PluginStorage> {
        self.need(Permission::Storage)?;
        Ok(&mut *self.storage)
    }

    /// `name` inside the data directory (with the `storage` permission), see [`data_path`].
    pub fn data_file(&mut self, name: &str) -> Result<PathBuf> {
        let dir = self.storage()?.data_dir();
        data_path(&dir, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_paths_stay_inside() {
        let d = Path::new("/mnt/us/rinky/plugins/x/data");
        assert_eq!(data_path(d, "a.txt").unwrap(), d.join("a.txt"));
        assert_eq!(data_path(d, "notes/b.txt").unwrap(), d.join("notes/b.txt"));
        for bad in ["", "/etc/passwd", "../x", "a/../../b", "./a", "a\\b", ".", "..", "a\0b"] {
            assert!(data_path(d, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn checked_accessors() {
        let mut ui = UiQueue::default();
        let mut st = MemoryStorage::default();
        let perms = [Permission::Ui];
        {
            let mut cx = HostCtx::new(&mut ui, &mut st, &perms);
            cx.ui().unwrap().toast("hi");
            cx.ui().unwrap().status("w", "1 min");
            assert!(matches!(cx.storage(), Err(PluginError::Permission(_))));
            assert!(matches!(cx.document(), Err(PluginError::Permission(_))));
            assert!(matches!(cx.send(Request::get("https://x.org/")), Err(PluginError::Permission(_))));
            assert!(matches!(cx.data_file("a"), Err(PluginError::Permission(_))));
        }
        assert_eq!(ui.0.len(), 2);
        assert_eq!(ui.0[0].id(), None);

        let perms = [Permission::Network, Permission::Storage];
        let hosts = vec!["example.com".to_string()];
        let mut cx = HostCtx::new(&mut ui, &mut st, &perms).with_hosts(&hosts);
        assert!(matches!(cx.send(Request::get("https://other.org/")), Err(PluginError::Permission(_))));
        assert!(matches!(cx.send(Request::get("https://example.com/")), Err(PluginError::Unsupported(_))));
        assert!(matches!(cx.start_request(Request::get("https://example.com/"), StreamMode::Chunks), Err(PluginError::Unsupported(_))));
        assert!(matches!(cx.send(Request::get("file:///etc/passwd")), Err(PluginError::Failed(_))));
        cx.storage().unwrap().set("k", serde_json::json!(1)).unwrap();
        cx.storage().unwrap().set("k", serde_json::Value::Null).unwrap();
        assert_eq!(cx.storage().unwrap().get("k"), None);
    }
}
