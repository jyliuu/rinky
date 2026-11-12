//! One plugin API for compiled-in Rust plugins and user Lua plugins.
//!
//! - [`Plugin`]: lifecycle (`on_load`, `on_book_open`, `on_page_change`, `on_selection`,
//!   `on_suspend`, `on_resume`, `on_close`) plus `on_action` / `on_library_action` for
//!   registered entries, `on_ui_result` for answers to UI requests, `on_http` for
//!   streaming network events and `on_tick` for plugins with their own worker threads.
//! - [`registry`]: reader menu items, selection actions, gestures, gesture-map actions, settings pages,
//!   library actions, status-bar widgets.
//! - [`host`]: document access, UI requests, storage, network behind a permission.
//! - [`net`]: streaming HTTP(S) requests in worker threads.
//! - [`manifest`]: `plugin.toml`, [`Permission`]s and limits.
//! - [`lua`]: the sandboxed Lua 5.4 host; [`lua::LuaPlugin`] implements [`Plugin`], so Lua
//!   plugins go through the same manager, registry and host services as Rust plugins.
//! - [`markdown`]: the Markdown subset of the text viewer.
//! - [`PluginManager`]: holds plugins, calls them, and keeps one failing plugin from
//!   taking the reader down (errors are collected; a plugin is disabled after
//!   [`PluginManager::max_errors`] errors or a time limit hit during a page turn).
//!
//! The app side (presenting UI requests with ink-ui widgets, storage on disk, the network
//! worker, loading `/mnt/us/rinky/plugins`) is `crates/ink-app/src/plugin_host.rs`;
//! `docs/plugins.md` is the guide for plugin authors and for the screens that call it.

#![warn(missing_docs)]

pub mod host;
pub mod kit;
pub mod lua;
pub mod manifest;
pub mod markdown;
pub mod net;
pub mod registry;
pub mod testing;

pub use host::{data_path, DocAccess, DocumentAccess, HostCtx, MemoryStorage, PluginStorage, UiHost, UiQueue, UiRequest};
pub use manifest::{Permission, PluginManifest};
pub use net::{HttpEvent, NetHost, NetWorker, StreamMode};
pub use registry::{ControlTile, LibraryEntry, ReaderAction, Registrar, Registry, StatusSlot, TileState};
/// The JSON crate of the storage API (`PluginStorage` values), re-exported so plugin
/// crates and the app use the same version without their own dependency.
pub use serde_json;

/// The text annotation API of [`Plugin::text_annotator`], re-exported for plugin crates.
pub use ink_doc::{Annotation, TextAnnotator};
use ink_doc::Selection;
use std::path::Path;
use std::sync::Arc;

/// Errors from plugins and the host.
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// `plugin.toml` is missing or invalid.
    #[error("manifest: {0}")]
    Manifest(String),
    /// The plugin used something its manifest does not allow.
    #[error("permission denied: {0}")]
    Permission(String),
    /// A Lua error (syntax, runtime).
    #[error("lua: {0}")]
    Lua(String),
    /// A Lua call hit its time or memory limit.
    #[error("limit: {0}")]
    Limit(String),
    /// Not available (yet, or in this context).
    #[error("not supported: {0}")]
    Unsupported(String),
    /// The plugin reported a failure.
    #[error("{0}")]
    Failed(String),
}

/// Result alias for this crate.
pub type Result<T, E = PluginError> = std::result::Result<T, E>;

/// A plugin. Every method but [`Plugin::manifest`] has a no-op default.
pub trait Plugin {
    /// Name, version, permissions.
    fn manifest(&self) -> &PluginManifest;

    /// `rust` or `lua` (for plugin lists and logs).
    fn kind(&self) -> &'static str {
        "rust"
    }

    /// Called once at start-up; register menu items, actions, gestures here.
    fn on_load(&mut self, _reg: &mut Registrar) -> Result<()> {
        Ok(())
    }

    /// A book was opened.
    fn on_book_open(&mut self, _cx: &mut HostCtx) -> Result<()> {
        Ok(())
    }

    /// The reader moved to `page` (0-based).
    fn on_page_change(&mut self, _cx: &mut HostCtx, _page: usize) -> Result<()> {
        Ok(())
    }

    /// The user selected text.
    fn on_selection(&mut self, _cx: &mut HostCtx, _sel: &Selection) -> Result<()> {
        Ok(())
    }

    /// The device is going to sleep.
    fn on_suspend(&mut self, _cx: &mut HostCtx) -> Result<()> {
        Ok(())
    }

    /// The device woke up.
    fn on_resume(&mut self, _cx: &mut HostCtx) -> Result<()> {
        Ok(())
    }

    /// The book was closed.
    fn on_close(&mut self, _cx: &mut HostCtx) -> Result<()> {
        Ok(())
    }

    /// rinky is about to exit (once, after the last screen closed): stop processes
    /// and give back devices the plugin took, so the Kindle UI finds them as before.
    /// `restarting` is true when a new rinky starts right after this one and the
    /// Kindle UI stays stopped (`rinky-ctl.sh restart`, used by deploys and updates):
    /// a plugin may then keep a device as it is, or note what to start again.
    fn on_exit(&mut self, _cx: &mut HostCtx, _restarting: bool) -> Result<()> {
        Ok(())
    }

    /// A registered entry (menu item, selection action, gesture, settings page as
    /// `settings:<id>`) was triggered.
    fn on_action(&mut self, _cx: &mut HostCtx, _id: &str) -> Result<()> {
        Ok(())
    }

    /// A library action was triggered on `book` (no book is open; per-book storage refers
    /// to `book`). Defaults to [`Plugin::on_action`].
    fn on_library_action(&mut self, cx: &mut HostCtx, id: &str, _book: &Path) -> Result<()> {
        self.on_action(cx, id)
    }

    /// The state of control-center tile `id` (registered with
    /// [`Registrar::control_tile`]): the line under the caption and the on/off look.
    /// Called on the UI thread whenever the control center draws the tile, so it must be
    /// quick and read only the plugin's own memory. `None` (the default) uses the text
    /// set with [`UiRequest::TileState`] or at registration.
    fn tile_state(&self, _id: &str) -> Option<TileState> {
        None
    }

    /// The answer to a [`UiRequest`] with this `id`.
    fn on_ui_result(&mut self, _cx: &mut HostCtx, _id: u32, _result: &str) -> Result<()> {
        Ok(())
    }

    /// An event of a request started with [`HostCtx::start_request`].
    fn on_http(&mut self, _cx: &mut HostCtx, _id: u32, _ev: &HttpEvent) -> Result<()> {
        Ok(())
    }

    /// How often the plugin wants [`Plugin::on_tick`] calls, or `None` (the default) for
    /// none. Asked after every call; return `Some` only while there is work, for example
    /// while a worker thread the plugin started itself (a TCP connection) may have
    /// results. The app loop then wakes at least this often (no shorter than 100 ms).
    fn tick_interval(&self) -> Option<std::time::Duration> {
        None
    }

    /// Called on the UI thread about every [`Plugin::tick_interval`] while that is
    /// `Some`: collect worker results here and show them (toasts, menus, rescans).
    fn on_tick(&mut self, _cx: &mut HostCtx) -> Result<()> {
        Ok(())
    }
    /// A [`TextAnnotator`] whose annotations the reader draws above the text of
    /// reflowable books, like ruby, or `None` (the default). Asked when a book opens;
    /// the reader keeps the `Arc` and reads [`TextAnnotator::version`] again whenever it
    /// rebuilds its layout settings (after a plugin action or a settings page closes), so
    /// a plugin changes the annotations by bumping the version, not by returning a
    /// different annotator. Rust plugins only: the annotator runs on the pagination
    /// thread for every paragraph of the book, where a Lua call would be too slow.
    fn text_annotator(&self) -> Option<Arc<dyn TextAnnotator>> {
        None
    }
}

/// Lifecycle events the manager broadcasts.
#[derive(Clone, Debug, PartialEq)]
pub enum Lifecycle {
    /// [`Plugin::on_book_open`].
    BookOpen,
    /// [`Plugin::on_page_change`].
    PageChange(usize),
    /// [`Plugin::on_selection`].
    Selection(Selection),
    /// [`Plugin::on_suspend`].
    Suspend,
    /// [`Plugin::on_resume`].
    Resume,
    /// [`Plugin::on_close`].
    Close,
    /// [`Plugin::on_exit`]; true when rinky restarts (see there).
    Exit(bool),
}

impl Lifecycle {
    /// Hook name (`on_page_change`, ...), for logs.
    pub fn hook(&self) -> &'static str {
        match self {
            Lifecycle::BookOpen => "on_book_open",
            Lifecycle::PageChange(_) => "on_page_change",
            Lifecycle::Selection(_) => "on_selection",
            Lifecycle::Suspend => "on_suspend",
            Lifecycle::Resume => "on_resume",
            Lifecycle::Close => "on_close",
            Lifecycle::Exit(_) => "on_exit",
        }
    }
}

/// One loaded plugin as the plugin list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginInfo {
    /// Manifest.
    pub manifest: PluginManifest,
    /// `rust` or `lua`.
    pub kind: &'static str,
    /// False after a failed `on_load`, too many errors or [`PluginManager::disable`].
    pub enabled: bool,
    /// Errors so far.
    pub errors: u32,
}

/// One loaded plugin and its bookkeeping.
struct Slot {
    plugin: Box<dyn Plugin>,
    /// A copy of the plugin's manifest (manifests never change after loading), so a
    /// broadcast can hand it to the context builder while the plugin is borrowed
    /// mutably, without cloning it on every page turn.
    manifest: PluginManifest,
    enabled: bool,
    errors: u32,
}

/// Holds the plugins and the registry.
pub struct PluginManager {
    slots: Vec<Slot>,
    /// Everything registered by enabled plugins.
    pub registry: Registry,
    /// `(plugin, error)` for every failed call (and every disabling), oldest first.
    pub errors: Vec<(String, String)>,
    /// A plugin is disabled after this many failed calls (default 5).
    pub max_errors: u32,
}

impl Default for PluginManager {
    fn default() -> Self {
        PluginManager { slots: Vec::new(), registry: Registry::default(), errors: Vec::new(), max_errors: 5 }
    }
}

impl PluginManager {
    /// An empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds plugins and calls their `on_load`. A plugin whose `on_load` fails is disabled
    /// and its registrations are removed. A plugin whose name is already loaded is
    /// refused (recorded in [`PluginManager::errors`]).
    pub fn load(&mut self, plugins: Vec<Box<dyn Plugin>>) {
        for mut p in plugins {
            let Some(manifest) = self.admit(p.as_ref()) else { continue };
            let name = &manifest.name;
            let mut reg = Registrar { plugin: name, registry: &mut self.registry };
            let ok = match p.on_load(&mut reg) {
                Ok(()) => true,
                Err(e) => {
                    self.errors.push((name.clone(), format!("on_load: {e}")));
                    self.registry.remove_plugin(name);
                    false
                }
            };
            self.slots.push(Slot { plugin: p, manifest, enabled: ok, errors: u32::from(!ok) });
        }
    }

    /// Adds plugins the user turned off: they are listed (disabled) but `on_load` is
    /// not called, nothing is registered and no error is recorded. A name already
    /// loaded is refused as in [`PluginManager::load`].
    pub fn load_disabled(&mut self, plugins: Vec<Box<dyn Plugin>>) {
        for p in plugins {
            if let Some(manifest) = self.admit(p.as_ref()) {
                self.slots.push(Slot { plugin: p, manifest, enabled: false, errors: 0 });
            }
        }
    }

    /// The manifest of a plugin about to be added, or `None` (and an error) when a
    /// plugin of that name is already loaded.
    fn admit(&mut self, p: &dyn Plugin) -> Option<PluginManifest> {
        let manifest = p.manifest().clone();
        if self.index(&manifest.name).is_some() {
            let name = manifest.name;
            self.errors.push((name.clone(), format!("a plugin named {name} is already loaded; this {} plugin is skipped", p.kind())));
            return None;
        }
        Some(manifest)
    }

    /// Records a plugin that could not even be built (bad manifest, Lua syntax error).
    pub fn record_error(&mut self, plugin: &str, error: &str) {
        self.errors.push((plugin.to_string(), error.to_string()));
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.slots.iter().position(|s| s.manifest.name == name)
    }

    /// The slot of `name` when it is loaded and enabled.
    fn enabled(&self, name: &str) -> Option<&Slot> {
        self.slots.iter().find(|s| s.manifest.name == name && s.enabled)
    }

    /// Names of the loaded plugins and whether each is enabled.
    pub fn list(&self) -> Vec<(String, bool)> {
        self.slots.iter().map(|s| (s.manifest.name.clone(), s.enabled)).collect()
    }

    /// Names of the loaded plugins that are disabled, borrowed (no allocation; the
    /// host asks after every plugin call).
    pub fn disabled(&self) -> impl Iterator<Item = &str> + '_ {
        self.slots.iter().filter(|s| !s.enabled).map(|s| s.manifest.name.as_str())
    }

    /// Details of every loaded plugin.
    pub fn info(&self) -> Vec<PluginInfo> {
        self.slots.iter().map(|s| PluginInfo { manifest: s.manifest.clone(), kind: s.plugin.kind(), enabled: s.enabled, errors: s.errors }).collect()
    }

    /// The manifest of `plugin`.
    pub fn manifest(&self, plugin: &str) -> Option<&PluginManifest> {
        self.index(plugin).map(|i| &self.slots[i].manifest)
    }

    /// True when `plugin` is loaded and enabled.
    pub fn is_enabled(&self, plugin: &str) -> bool {
        self.enabled(plugin).is_some()
    }

    /// Disables `plugin` and removes its registrations; `reason` goes to the error list.
    pub fn disable(&mut self, plugin: &str, reason: &str) {
        if let Some(s) = self.slots.iter_mut().find(|s| s.manifest.name == plugin && s.enabled) {
            s.enabled = false;
            self.registry.remove_plugin(plugin);
            self.errors.push((plugin.to_string(), format!("disabled: {reason}")));
        }
    }

    /// Records the outcome of a call of plugin `i`; disables it when needed.
    fn record(&mut self, i: usize, what: &str, r: &Result<()>, page_turn: bool) {
        let Err(e) = r else { return };
        let slot = &mut self.slots[i];
        slot.errors += 1;
        let (name, n) = (slot.manifest.name.clone(), slot.errors);
        self.errors.push((name.clone(), format!("{what}: {e}")));
        if page_turn && matches!(e, PluginError::Limit(_)) {
            self.disable(&name, "too slow on a page turn");
        } else if n >= self.max_errors {
            self.disable(&name, &format!("{n} errors"));
        }
    }

    /// Calls `f` with a context for every enabled plugin. `with_ctx` builds the context
    /// for a plugin's manifest and runs the call inside it; errors are recorded, never
    /// propagated.
    pub fn broadcast(&mut self, ev: &Lifecycle, mut with_ctx: impl FnMut(&PluginManifest, &mut dyn FnMut(&mut HostCtx) -> Result<()>)) {
        for i in 0..self.slots.len() {
            let Slot { plugin: p, manifest, enabled, .. } = &mut self.slots[i];
            if !*enabled {
                continue;
            }
            let mut outcome = Ok(());
            with_ctx(manifest, &mut |cx| {
                outcome = match ev {
                    Lifecycle::BookOpen => p.on_book_open(cx),
                    Lifecycle::PageChange(n) => p.on_page_change(cx, *n),
                    Lifecycle::Selection(s) => p.on_selection(cx, s),
                    Lifecycle::Suspend => p.on_suspend(cx),
                    Lifecycle::Resume => p.on_resume(cx),
                    Lifecycle::Close => p.on_close(cx),
                    Lifecycle::Exit(restarting) => p.on_exit(cx, *restarting),
                };
                Ok(())
            });
            self.record(i, ev.hook(), &outcome, matches!(ev, Lifecycle::PageChange(_)));
        }
    }

    fn call(&mut self, plugin: &str, what: &str, f: impl FnOnce(&mut dyn Plugin) -> Result<()>) -> Result<()> {
        let i = self.index(plugin).ok_or_else(|| PluginError::Unsupported(format!("no plugin {plugin}")))?;
        if !self.slots[i].enabled {
            return Err(PluginError::Unsupported(format!("plugin {plugin} is disabled")));
        }
        let r = f(self.slots[i].plugin.as_mut());
        self.record(i, what, &r, false);
        r
    }

    /// Routes a triggered registry entry to its plugin.
    pub fn action(&mut self, plugin: &str, id: &str, cx: &mut HostCtx) -> Result<()> {
        self.call(plugin, &format!("on_action({id})"), |p| p.on_action(cx, id))
    }

    /// Routes a library action on `book` to its plugin.
    pub fn library_action(&mut self, plugin: &str, id: &str, book: &Path, cx: &mut HostCtx) -> Result<()> {
        self.call(plugin, &format!("on_library_action({id})"), |p| p.on_library_action(cx, id, book))
    }

    /// Enabled plugins that want ticks now, with their interval ([`Plugin::tick_interval`]).
    pub fn tick_intervals(&self) -> Vec<(String, std::time::Duration)> {
        self.slots
            .iter()
            .filter(|s| s.enabled)
            .filter_map(|s| s.plugin.tick_interval().map(|d| (s.manifest.name.clone(), d.max(std::time::Duration::from_millis(100)))))
            .collect()
    }

    /// Calls [`Plugin::on_tick`] of `plugin`.
    pub fn tick(&mut self, plugin: &str, cx: &mut HostCtx) -> Result<()> {
        self.call(plugin, "on_tick", |p| p.on_tick(cx))
    }

    /// The state of control tile `id` of `plugin`: what the plugin says
    /// ([`Plugin::tile_state`]) or else the registered text. `None` when the plugin is
    /// disabled or has no such tile.
    pub fn tile_state(&self, plugin: &str, id: &str) -> Option<TileState> {
        let slot = self.enabled(plugin)?;
        let tile = self.registry.control_tiles.iter().find(|t| t.plugin == plugin && t.id == id)?;
        slot.plugin.tile_state(id).or_else(|| Some(TileState { text: tile.state.clone(), on: None }))
    }

    /// Delivers the answer to UI request `id` of `plugin`.
    pub fn ui_result(&mut self, plugin: &str, id: u32, result: &str, cx: &mut HostCtx) -> Result<()> {
        self.call(plugin, "on_ui_result", |p| p.on_ui_result(cx, id, result))
    }

    /// Delivers a network event of request `id` to `plugin`.
    pub fn http_event(&mut self, plugin: &str, id: u32, ev: &HttpEvent, cx: &mut HostCtx) -> Result<()> {
        self.call(plugin, "on_http", |p| p.on_http(cx, id, ev))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counter {
        m: PluginManifest,
        pages: Vec<usize>,
        fail_load: bool,
    }

    impl Plugin for Counter {
        fn manifest(&self) -> &PluginManifest {
            &self.m
        }
        fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
            reg.menu_item("count", "Count pages", 0);
            if self.fail_load {
                Err(PluginError::Failed("boom".into()))
            } else {
                Ok(())
            }
        }
        fn on_page_change(&mut self, cx: &mut HostCtx, page: usize) -> Result<()> {
            self.pages.push(page);
            cx.ui.request(UiRequest::Toast { text: format!("page {page}") });
            cx.storage.set("last", serde_json::json!(page))
        }
        fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
            match id {
                "net" => cx.http().map(|_| ()),
                _ => Ok(()),
            }
        }
    }

    fn counter(name: &str, fail_load: bool) -> Box<dyn Plugin> {
        Box::new(Counter { m: PluginManifest::new(name, "1", &[Permission::Ui]), pages: Vec::new(), fail_load })
    }

    #[test]
    fn load_broadcast_action_and_permissions() {
        let mut mgr = PluginManager::new();
        mgr.load(vec![counter("good", false), counter("bad", true)]);
        assert_eq!(mgr.list(), vec![("good".to_string(), true), ("bad".to_string(), false)]);
        assert_eq!(mgr.registry.menu_items.len(), 1, "the failed plugin's entries are removed");
        assert_eq!(mgr.errors.len(), 1);

        let mut ui = UiQueue::default();
        let mut storage = MemoryStorage::default();
        mgr.broadcast(&Lifecycle::PageChange(4), |m, f| {
            let mut cx = HostCtx::new(&mut ui, &mut storage, &m.permissions);
            let _ = f(&mut cx);
        });
        assert_eq!(ui.0, vec![UiRequest::Toast { text: "page 4".into() }]);
        assert_eq!(storage.get("last"), Some(serde_json::json!(4)));

        let perms = [Permission::Ui];
        let mut cx = HostCtx::new(&mut ui, &mut storage, &perms);
        assert!(matches!(mgr.action("good", "net", &mut cx), Err(PluginError::Permission(_))));
        assert!(matches!(cx.document(), Err(PluginError::Permission(_))));
        assert!(mgr.action("bad", "x", &mut cx).is_err());
        assert!(mgr.action("none", "x", &mut cx).is_err());
        // Library actions default to on_action; results and HTTP events reach the plugin.
        assert!(mgr.library_action("good", "x", Path::new("/b.epub"), &mut cx).is_ok());
        assert!(mgr.ui_result("good", 1, "ok", &mut cx).is_ok());
        assert!(mgr.http_event("good", 1, &HttpEvent::Done { body: None }, &mut cx).is_ok());
        let info = mgr.info();
        assert_eq!((info[0].kind, info[0].enabled, info[0].errors), ("rust", true, 1));
        assert_eq!(mgr.manifest("good").map(|m| m.version.as_str()), Some("1"));
    }

    /// Fails every call; one variant hits a "time limit" on page turns.
    struct Flaky(PluginManifest, bool);
    impl Plugin for Flaky {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
            reg.status_widget("w", StatusSlot::Right, "-");
            Ok(())
        }
        fn on_page_change(&mut self, _cx: &mut HostCtx, _page: usize) -> Result<()> {
            Err(if self.1 { PluginError::Limit("time limit".into()) } else { PluginError::Failed("bad".into()) })
        }
    }

    #[test]
    fn user_disabled_plugins_are_listed_not_loaded() {
        let mut mgr = PluginManager::new();
        mgr.load(vec![counter("a", false)]);
        mgr.load_disabled(vec![counter("b", false), counter("a", false)]);
        assert_eq!(mgr.list(), vec![("a".to_string(), true), ("b".to_string(), false)]);
        assert_eq!(mgr.disabled().collect::<Vec<_>>(), vec!["b"]);
        assert_eq!(mgr.registry.menu_items.len(), 1, "b's on_load did not run");
        assert_eq!(mgr.info()[1].errors, 0);
        assert_eq!(mgr.errors.len(), 1, "only the duplicate a: {:?}", mgr.errors);
    }

    #[test]
    fn duplicates_and_auto_disable() {
        let mut mgr = PluginManager::new();
        mgr.max_errors = 3;
        let flaky = |n: &str, slow: bool| Box::new(Flaky(PluginManifest::new(n, "1", &[]), slow)) as Box<dyn Plugin>;
        mgr.load(vec![flaky("a", false), flaky("a", false), flaky("slow", true)]);
        assert_eq!(mgr.list().len(), 2, "the second plugin named a is refused");
        assert!(mgr.errors[0].1.contains("already loaded"));
        let (mut ui, mut st) = (UiQueue::default(), MemoryStorage::default());
        for page in 0..4 {
            mgr.broadcast(&Lifecycle::PageChange(page), |m, f| {
                let _ = f(&mut HostCtx::new(&mut ui, &mut st, &m.permissions));
            });
        }
        assert!(!mgr.is_enabled("slow"), "a time limit on a page turn disables at once");
        assert!(!mgr.is_enabled("a"), "three errors disable");
        assert!(mgr.registry.status_widgets.is_empty());
        let disabled: Vec<_> = mgr.errors.iter().filter(|(_, e)| e.starts_with("disabled")).collect();
        assert_eq!(disabled.len(), 2, "{:?}", mgr.errors);
        assert_eq!(mgr.info().iter().map(|i| i.errors).collect::<Vec<_>>(), vec![3, 1]);
        mgr.record_error("broken", "lua: syntax error");
        assert_eq!(mgr.errors.last().map(|e| e.0.as_str()), Some("broken"));
        assert_eq!(Lifecycle::Suspend.hook(), "on_suspend");
    }

    /// Wants ticks while `on`; counts them.
    struct Ticker(PluginManifest, bool, u32);
    impl Plugin for Ticker {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn tick_interval(&self) -> Option<std::time::Duration> {
            self.1.then_some(std::time::Duration::from_millis(10))
        }
        fn on_tick(&mut self, cx: &mut HostCtx) -> Result<()> {
            self.2 += 1;
            self.1 = self.2 < 2;
            cx.ui.request(UiRequest::Toast { text: format!("tick {}", self.2) });
            Ok(())
        }
    }

    #[test]
    fn ticks_while_wanted() {
        let mut mgr = PluginManager::new();
        mgr.load(vec![Box::new(Ticker(PluginManifest::new("t", "1", &[]), true, 0)), counter("c", false)]);
        assert_eq!(mgr.tick_intervals(), vec![("t".to_string(), std::time::Duration::from_millis(100))], "clamped to 100 ms; plugins without ticks are not listed");
        let (mut ui, mut st) = (UiQueue::default(), MemoryStorage::default());
        let mut cx = HostCtx::new(&mut ui, &mut st, &[]);
        mgr.tick("t", &mut cx).unwrap();
        mgr.tick("t", &mut cx).unwrap();
        assert!(mgr.tick_intervals().is_empty(), "the plugin stopped asking");
        assert_eq!(ui.0.len(), 2);
        mgr.disable("t", "test");
        assert!(mgr.tick("t", &mut HostCtx::new(&mut ui, &mut st, &[])).is_err());
    }

    /// Registers two tiles and a library entry; answers `tile_state` for one tile.
    struct Tiles(PluginManifest);
    impl Plugin for Tiles {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn on_load(&mut self, reg: &mut Registrar) -> Result<()> {
            reg.control_tile("live", "Live", "sync");
            reg.control_tile("pushed", "Pushed", "info").state("first");
            reg.library_entry("go", "Go", "download");
            Ok(())
        }
        fn tile_state(&self, id: &str) -> Option<TileState> {
            (id == "live").then(|| TileState { text: "from the plugin".into(), on: Some(true) })
        }
        fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
            cx.ui.tile_state(id, "tapped");
            Ok(())
        }
    }

    #[test]
    fn tile_state_comes_from_the_plugin_or_the_registry() {
        let mut mgr = PluginManager::new();
        mgr.load(vec![Box::new(Tiles(PluginManifest::new("t", "1", &[Permission::Ui])))]);
        assert_eq!(mgr.registry.control_tiles.len(), 2);
        assert_eq!(mgr.registry.library_entries.len(), 1);
        assert_eq!(mgr.tile_state("t", "live"), Some(TileState { text: "from the plugin".into(), on: Some(true) }));
        assert_eq!(mgr.tile_state("t", "pushed").map(|s| s.text), Some("first".into()));
        assert_eq!(mgr.tile_state("t", "none"), None);
        assert_eq!(mgr.tile_state("x", "live"), None);
        let (mut ui, mut st) = (UiQueue::default(), MemoryStorage::default());
        let perms = [Permission::Ui];
        mgr.action("t", "go", &mut HostCtx::new(&mut ui, &mut st, &perms)).unwrap();
        assert_eq!(ui.0, vec![UiRequest::TileState { id: "go".into(), text: "tapped".into() }]);
        mgr.disable("t", "test");
        assert_eq!(mgr.tile_state("t", "live"), None);
        assert!(mgr.registry.control_tiles.is_empty() && mgr.registry.library_entries.is_empty());
    }
}
