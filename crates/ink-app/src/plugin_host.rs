//! Glue between the app and `ink-plugin` (owner: the `plugin` agent).
//!
//! [`PluginHost`] owns the [`PluginManager`] with the compiled-in Rust plugins and the
//! Lua plugins found in `<RINKY_HOME>/plugins/<name>/` (`plugin.toml` + `main.lua`),
//! and gives them:
//! - storage on disk ([`DiskStorage`]): settings in `plugins/<name>/settings.json` over
//!   defaults from `[plugins.<name>]` in `settings.toml`, per-book data in the book's
//!   sidecar (`BookState::plugins[<name>]`), files in `plugins/<name>/data/`;
//! - the network: a `ureq` client (blocking, Rust plugins) and a [`NetWorker`] for
//!   streaming requests, only for plugins with the `network` permission;
//! - the UI services: [`PluginUi`] presents [`UiRequest`]s with ink-ui widgets (toast,
//!   dialog, menu, Markdown viewer, keyboard input) as overlay layers over whatever
//!   screen is showing, and routes the answers back to the plugin that asked.
//!
//! Screens call it at fixed points; `docs/plugins.md` ("For screen authors") lists them:
//! lifecycle ([`PluginHost::book_open`], [`PluginHost::page_change`],
//! [`PluginHost::selection`], [`PluginHost::suspend`], [`PluginHost::resume`],
//! [`PluginHost::book_close`]), events and painting ([`PluginHost::handle_event`],
//! [`PluginHost::paint_overlay`], [`PluginHost::timeout`]), and the registries
//! ([`PluginHost::menu_items`], [`PluginHost::trigger`], [`PluginHost::gesture`],
//! [`PluginHost::library_action`], [`PluginHost::status_line`], ...).
//!
//! Every hook call, UI request and plugin error is logged at info/warn level with the
//! prefix `plugins:`; errors are also shown as toasts and kept as notifications.

use ink_device::{Canvas, Point, PowerEvent, Rect, RefreshClass, Size};
use ink_doc::{Document, Selection};
use ink_net::HttpClient;
use ink_plugin::markdown::{self, Block, Span};
use ink_plugin::registry::{LibraryAction, MenuItem, SelectionAction, SettingsPage, StatusSlot};
use ink_plugin::serde_json::{self, Map, Value};
use ink_plugin::{
    lua, DocAccess, DocumentAccess, HostCtx, Lifecycle, NetWorker, Permission, Plugin, PluginError, PluginInfo, PluginManager, PluginManifest,
    PluginStorage, UiQueue, UiRequest,
};
use ink_store::{BookState, Paths, Settings};
use ink_text::Style;
use ink_ui::bars::APP_BAR_H;
use ink_ui::draw::{fill_round_rect, stroke_round_rect};
use crate::screens::settings::popup::{self, Anchor, SettingsPopup};
use ink_ui::{
    paint_button, AppBar, BarAction, BottomSheet, ButtonKind, Damage, Dialog, Direction, EventResult, Gesture, Icon, KeyAction, Keyboard, PaintCx, Toast, UiEvent, Widget,
};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------
// Storage

/// Storage of one plugin: settings, per-book data and the data folder.
pub struct DiskStorage {
    name: String,
    /// `<root>/plugins/<name>`; `None` keeps everything in memory.
    dir: Option<PathBuf>,
    defaults: Map<String, Value>,
    overrides: Map<String, Value>,
    book: Option<PathBuf>,
    book_data: Map<String, Value>,
}

fn store_err(e: impl std::fmt::Display) -> PluginError {
    PluginError::Failed(e.to_string())
}

fn put(map: &mut Map<String, Value>, key: &str, value: Value) {
    if value.is_null() {
        map.remove(key);
    } else {
        map.insert(key.to_string(), value);
    }
}

impl DiskStorage {
    /// Storage of `name` under `paths` (`None`: in memory), with `defaults` from
    /// `settings.toml`.
    pub fn open(name: &str, paths: Option<&Paths>, defaults: Map<String, Value>) -> Self {
        let dir = paths.map(|p| p.plugins().join(name));
        let overrides = dir
            .as_ref()
            .and_then(|d| std::fs::read(d.join("settings.json")).ok())
            .and_then(|b| match serde_json::from_slice::<Value>(&b) {
                Ok(Value::Object(m)) => Some(m),
                _ => {
                    log::warn!("plugins: {name}: settings.json is not a JSON object; ignored");
                    None
                }
            })
            .unwrap_or_default();
        DiskStorage { name: name.to_string(), dir, defaults, overrides, book: None, book_data: Map::new() }
    }

    /// Makes per-book values refer to `book` (loaded from its sidecar).
    pub fn set_book(&mut self, book: Option<&Path>) {
        if self.book.as_deref() == book {
            return;
        }
        self.book = book.map(Path::to_path_buf);
        self.book_data = match book {
            Some(b) if self.dir.is_some() => match BookState::load(b) {
                Ok(state) => match state.plugins.get(&self.name) {
                    Some(Value::Object(m)) => m.clone(),
                    _ => Map::new(),
                },
                Err(e) => {
                    log::warn!("plugins: {}: cannot read the sidecar of {}: {e}", self.name, b.display());
                    Map::new()
                }
            },
            _ => Map::new(),
        };
    }

    fn save_settings(&self) -> ink_plugin::Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        std::fs::create_dir_all(dir).map_err(store_err)?;
        let json = serde_json::to_vec_pretty(&Value::Object(self.overrides.clone())).map_err(store_err)?;
        ink_store::write_atomic(&dir.join("settings.json"), &json).map_err(store_err)
    }
}

impl PluginStorage for DiskStorage {
    fn get(&self, key: &str) -> Option<Value> {
        self.overrides.get(key).or_else(|| self.defaults.get(key)).cloned()
    }

    fn set(&mut self, key: &str, value: Value) -> ink_plugin::Result<()> {
        put(&mut self.overrides, key, value);
        self.save_settings()
    }

    fn book_get(&self, key: &str) -> Option<Value> {
        self.book_data.get(key).cloned()
    }

    fn book_set(&mut self, key: &str, value: Value) -> ink_plugin::Result<()> {
        let Some(book) = self.book.clone() else { return Err(PluginError::Unsupported("no book is open".into())) };
        put(&mut self.book_data, key, value);
        if self.dir.is_none() {
            return Ok(());
        }
        // Load, merge, save: the reader writes the same file after every page.
        let mut state = BookState::load(&book).map_err(store_err)?;
        if self.book_data.is_empty() {
            state.plugins.remove(&self.name);
        } else {
            state.plugins.insert(self.name.clone(), Value::Object(self.book_data.clone()));
        }
        state.save(&book).map_err(store_err)
    }

    fn data_dir(&self) -> PathBuf {
        let d = match &self.dir {
            Some(d) => d.join("data"),
            None => std::env::temp_dir().join("rinky-plugins").join(&self.name).join("data"),
        };
        let _ = std::fs::create_dir_all(&d);
        d
    }
}

// ---------------------------------------------------------------------------------------
// Host

/// The network: created on the first call of a plugin with the `network` permission.
#[derive(Default)]
struct Net {
    client: Option<Arc<dyn HttpClient + Send + Sync>>,
    worker: Option<NetWorker>,
}

impl Net {
    fn ensure(&mut self) -> Arc<dyn HttpClient + Send + Sync> {
        let client = self.client.get_or_insert_with(|| Arc::new(ink_net::UreqClient::new())).clone();
        if self.worker.is_none() {
            self.worker = Some(NetWorker::new(client.clone()));
        }
        client
    }
}

/// A notification kept until read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// Plugin that sent it (errors use the failing plugin's name).
    pub plugin: String,
    /// Text.
    pub text: String,
    /// Shown in the notification list already.
    pub read: bool,
}

/// What the stores need to open.
struct StoreCfg {
    paths: Option<Paths>,
    defaults: Map<String, Value>,
}

/// The open book.
struct OpenBook {
    path: PathBuf,
    id: Option<String>,
}

/// The app's plugin host.
pub struct PluginHost {
    /// Loaded plugins and their registrations.
    pub manager: PluginManager,
    /// The presenter of UI requests.
    pub ui: PluginUi,
    cfg: StoreCfg,
    stores: HashMap<String, DiskStorage>,
    net: Net,
    book: Option<OpenBook>,
    /// Requests from the last calls, not yet sorted.
    out: Vec<(String, UiRequest)>,
    notifications: Vec<Notification>,
    errors_seen: usize,
    /// Between `on_suspend` and `on_resume`.
    suspended: bool,
    /// [`PluginHost::app_exit`] ran.
    exited: bool,
    /// Plugins the user turned off (`plugins/disabled.txt`).
    off: BTreeSet<String>,
    /// The screen painted the overlay itself this frame ([`PluginHost::paint_overlay`]);
    /// [`hooks::app_paint`] then does not compose it again.
    overlay_painted: bool,
    /// Last `defer_suspend` asked for running plugin requests ([`hooks::keep_awake`]).
    last_defer: Option<Instant>,
    /// Position a plugin asked the reader to show ([`UiRequest::GoTo`]).
    pending_goto: Option<String>,
    /// A plugin changed the book folders ([`UiRequest::LibraryRescan`]).
    rescan_wanted: bool,
    /// Last `on_tick` call per plugin ([`Plugin::tick_interval`]).
    last_tick: HashMap<String, Instant>,
    /// The plugin whose menus a screen shows itself ([`PluginHost::claim_menus`]).
    claimed: Option<String>,
    /// Menus and menu label changes of the claimed plugin, not taken yet.
    claimed_out: Vec<UiRequest>,
}

/// Plugins that store books get the first library folder (`library_dirs`) as the default
/// of their `books_dir` setting (calibre wireless), unless `[plugins.<name>]` sets one.
fn library_dir_default(defaults: &mut Map<String, Value>, settings: &Settings) {
    let Some(first) = settings.library_dirs.first() else { return };
    let entry = defaults.entry(calibre::NAME.to_string()).or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(m) = entry {
        m.entry("books_dir".to_string()).or_insert_with(|| Value::String(first.to_string_lossy().into_owned()));
    }
}

/// How many entries of each registry (plural names, as the start log says them) pass
/// `of(plugin name)`.
fn registrations(r: &ink_plugin::registry::Registry, of: impl Fn(&str) -> bool) -> [(&'static str, usize); 7] {
    fn count<T>(entries: &[T], plugin: impl Fn(&T) -> &str, of: &impl Fn(&str) -> bool) -> usize {
        entries.iter().filter(|e| of(plugin(e))).count()
    }
    [
        ("menu items", count(&r.menu_items, |e| &e.plugin, &of)),
        ("selection actions", count(&r.selection_actions, |e| &e.plugin, &of)),
        ("gestures", count(&r.gestures, |e| &e.plugin, &of)),
        ("gesture-map actions", count(&r.actions, |e| &e.plugin, &of)),
        ("settings pages", count(&r.settings_pages, |e| &e.plugin, &of)),
        ("library actions", count(&r.library_actions, |e| &e.plugin, &of)),
        ("status widgets", count(&r.status_widgets, |e| &e.plugin, &of)),
    ]
}

/// Runs `f` with the context of plugin `m`.
#[allow(clippy::too_many_arguments)]
fn with_cx<R>(
    m: &PluginManifest,
    stores: &mut HashMap<String, DiskStorage>,
    cfg: &StoreCfg,
    book: Option<&Path>,
    net: &mut Net,
    doc: Option<&mut dyn DocumentAccess>,
    out: &mut Vec<(String, UiRequest)>,
    f: impl FnOnce(&mut HostCtx) -> R,
) -> R {
    let store = stores.entry(m.name.clone()).or_insert_with(|| {
        let defaults = match cfg.defaults.get(&m.name) {
            Some(Value::Object(d)) => d.clone(),
            _ => Map::new(),
        };
        DiskStorage::open(&m.name, cfg.paths.as_ref(), defaults)
    });
    store.set_book(book);
    let mut ui = UiQueue::default();
    let client = m.allows(Permission::Network).then(|| net.ensure());
    let r = {
        let mut jobs = match (&client, net.worker.as_mut()) {
            (Some(_), Some(w)) => Some(w.for_plugin(&m.name)),
            _ => None,
        };
        let mut cx = HostCtx::new(&mut ui, store, &m.permissions)
            .with_doc(doc.map(|d| d as &mut dyn DocumentAccess))
            .with_net(client.as_deref().map(|c| c as &dyn HttpClient))
            .with_jobs(jobs.as_mut().map(|j| j as &mut dyn ink_plugin::NetHost))
            .with_hosts(&m.hosts);
        f(&mut cx)
    };
    out.extend(ui.0.into_iter().map(|r| (m.name.clone(), r)));
    r
}

/// Reborrows an optional document for one call (the object lifetime shrinks with it).
fn reborrow<'s>(doc: &'s mut Option<&mut dyn Document>) -> Option<&'s mut dyn Document> {
    doc.as_deref_mut().map(|d| d as &mut dyn Document)
}

/// First line of an error, shortened for a toast.
fn short(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() <= max {
        line.to_string()
    } else {
        format!("{}...", line.chars().take(max).collect::<String>())
    }
}

/// Name of a gesture for plugin bindings (see `ink_plugin::registry::GESTURES`), or
/// `None` for gestures plugins cannot bind (plain taps and page-turn swipes).
pub fn gesture_name(g: &Gesture, screen: Size) -> Option<String> {
    let (w, h) = (screen.w as i32, screen.h as i32);
    Some(match *g {
        Gesture::TwoFingerTap { .. } => "two_finger_tap".into(),
        Gesture::DoubleTap { .. } => "double_tap".into(),
        Gesture::LongPress { pos, .. } => {
            let (left, right, top, bottom) = (pos.x < w * 15 / 100, pos.x > w * 85 / 100, pos.y < h * 12 / 100, pos.y > h * 88 / 100);
            match (left, right, top, bottom) {
                (true, _, true, _) => "corner_hold:top_left".into(),
                (_, true, true, _) => "corner_hold:top_right".into(),
                (true, _, _, true) => "corner_hold:bottom_left".into(),
                (_, true, _, true) => "corner_hold:bottom_right".into(),
                _ => "long_press".into(),
            }
        }
        Gesture::Swipe { from, dir: Direction::Up, .. } if from.y > h * 9 / 10 => "swipe_up_from_bottom".into(),
        Gesture::Swipe { from, dir: Direction::Down, .. } if from.y < h / 10 => "swipe_down_from_top".into(),
        Gesture::Pinch { scale_milli, .. } => if scale_milli > 1000 { "pinch_out" } else { "pinch_in" }.into(),
        _ => return None,
    })
}

impl PluginHost {
    /// Loads `plugins` (calls their `on_load`) with in-memory storage and no Lua plugins
    /// (tests, tools). The app uses [`PluginHost::load`].
    pub fn new(plugins: Vec<Box<dyn Plugin>>) -> Self {
        Self::build(plugins, None, Map::new(), BTreeSet::new())
    }

    /// The app's host: `compiled` plus every Lua plugin in `paths.plugins()`, storage
    /// under `paths`, setting defaults from `settings.plugins`. A Lua plugin that fails
    /// to load (bad manifest, syntax error, name taken) is skipped and reported; the
    /// others still load.
    pub fn load(compiled: Vec<Box<dyn Plugin>>, settings: &Settings, paths: &Paths) -> Self {
        let mut plugins = compiled;
        let dir = paths.plugins();
        let (lua_plugins, lua_errors) = lua::load_dir(&dir);
        log::info!("plugins: {} compiled-in, {} Lua plugin(s) in {}", plugins.len(), lua_plugins.len(), dir.display());
        plugins.extend(lua_plugins.into_iter().map(|p| Box::new(p) as Box<dyn Plugin>));
        let mut defaults = match serde_json::to_value(&settings.plugins) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        library_dir_default(&mut defaults, settings);
        let off = user_disabled(paths);
        let mut host = Self::build(plugins, Some(paths.clone()), defaults, off);
        for (name, err) in lua_errors {
            host.manager.record_error(&name, &format!("not loaded: {err}"));
        }
        host.ui.keyboard_layouts = crate::screens::settings::keyboard_layouts::layouts(settings);
        host.after_calls();
        host
    }

    fn build(plugins: Vec<Box<dyn Plugin>>, paths: Option<Paths>, defaults: Map<String, Value>, off: BTreeSet<String>) -> Self {
        let mut manager = PluginManager::new();
        let t = Instant::now();
        for p in plugins {
            if off.contains(&p.manifest().name) {
                log::info!("plugins: {} is turned off ({})", p.manifest().name, DISABLED_FILE);
                manager.load_disabled(vec![p]);
            } else {
                manager.load(vec![p]);
            }
        }
        for info in manager.info() {
            log::info!(
                "plugins: loaded {} {} ({}, {}) permissions [{}]",
                info.manifest.name,
                info.manifest.version,
                info.kind,
                if info.enabled { "enabled" } else { "disabled" },
                info.manifest.permissions.iter().map(|p| p.name()).collect::<Vec<_>>().join(", ")
            );
        }
        let r = &manager.registry;
        let counts_text = |counts: [(&str, usize); 7]| counts.map(|(k, c)| format!("{c} {k}")).join(", ");
        for info in manager.info().iter().filter(|i| i.enabled) {
            let n = &info.manifest.name;
            log::info!("plugins: {n} registered {}", counts_text(registrations(r, |p| p == n)));
        }
        log::info!("plugins: registry: {} ({:?})", counts_text(registrations(r, |_| true)), t.elapsed());
        let mut host = PluginHost {
            manager,
            ui: PluginUi::default(),
            cfg: StoreCfg { paths, defaults },
            stores: HashMap::new(),
            net: Net::default(),
            book: None,
            out: Vec::new(),
            notifications: Vec::new(),
            errors_seen: 0,
            suspended: false,
            exited: false,
            last_defer: None,
            pending_goto: None,
            rescan_wanted: false,
            last_tick: HashMap::new(),
            claimed: None,
            claimed_out: Vec::new(),
            off,
            overlay_painted: false,
        };
        host.after_calls();
        host
    }

    /// Uses `client` for plugin network access instead of the default `ureq` client
    /// (tests, mock servers).
    pub fn set_http_client(&mut self, client: Arc<dyn HttpClient + Send + Sync>) {
        self.net = Net { client: Some(client.clone()), worker: Some(NetWorker::new(client)) };
    }

    fn broadcast(&mut self, mut doc: Option<&mut dyn Document>, ev: Lifecycle) {
        // Network events that arrived meanwhile go first (screens that do not call
        // handle_event yet still deliver them on every page turn).
        self.poll(reborrow(&mut doc));
        let PluginHost { manager, cfg, stores, net, out, book, .. } = self;
        let mut access = doc.map(|d| DocAccess { doc: d, book_id: book.as_ref().and_then(|b| b.id.clone()) });
        let book_path = book.as_ref().map(|b| b.path.as_path());
        let hook = ev.hook();
        let arg = match &ev {
            Lifecycle::PageChange(p) => format!("({})", p + 1),
            Lifecycle::Selection(s) => format!("({:?})", short(&s.text, 40)),
            _ => String::new(),
        };
        manager.broadcast(&ev, |m, f| {
            let t = Instant::now();
            let doc = access.as_mut().map(|a| a as &mut dyn DocumentAccess);
            let r = with_cx(m, stores, cfg, book_path, net, doc, out, |cx| f(cx));
            log::info!("plugins: {hook}{arg} -> {} ({:.1} ms)", m.name, t.elapsed().as_secs_f64() * 1000.0);
            r.ok();
        });
        self.after_calls();
    }

    /// Calls `f` on the manager with the context of `plugin` (a single routed call).
    fn call(&mut self, plugin: &str, doc: Option<&mut dyn Document>, book: Option<&Path>, what: &str, f: impl FnOnce(&mut PluginManager, &mut HostCtx) -> ink_plugin::Result<()>) -> bool {
        let Some(m) = self.manager.manifest(plugin).cloned() else {
            log::warn!("plugins: {what}: no plugin {plugin}");
            return false;
        };
        let t = Instant::now();
        let r = self.run_as(&m, doc, book, f);
        log::info!("plugins: {what} -> {plugin} ({:.1} ms){}", t.elapsed().as_secs_f64() * 1000.0, if r.is_ok() { "" } else { " failed" });
        self.after_calls();
        r.is_ok()
    }

    /// Runs `f` on the manager with the context of plugin `m`: `doc` as its document,
    /// per-book storage for `book` (default: the open book).
    fn run_as<R>(&mut self, m: &PluginManifest, doc: Option<&mut dyn Document>, book: Option<&Path>, f: impl FnOnce(&mut PluginManager, &mut HostCtx) -> R) -> R {
        let PluginHost { manager, cfg, stores, net, out, book: open, .. } = self;
        let mut access = doc.map(|d| DocAccess { doc: d, book_id: open.as_ref().and_then(|b| b.id.clone()) });
        let book = book.or(open.as_ref().map(|b| b.path.as_path()));
        with_cx(m, stores, cfg, book, net, access.as_mut().map(|a| a as &mut dyn DocumentAccess), out, |cx| f(manager, cx))
    }

    /// Sorts the requests of the last calls (status texts go to the registry,
    /// notifications to the list, the rest to the presenter) and turns new plugin
    /// errors into toasts and notifications.
    fn after_calls(&mut self) {
        for (plugin, req) in std::mem::take(&mut self.out) {
            match req {
                UiRequest::Status { id, text } => {
                    if self.manager.registry.set_status_text(&plugin, &id, &text) {
                        log::info!("plugins: {plugin}: status {id} = {text:?}");
                    } else {
                        log::warn!("plugins: {plugin}: status widget {id:?} is not registered");
                    }
                }
                UiRequest::TileState { id, text } => {
                    if self.manager.registry.set_tile_state(&plugin, &id, &text) {
                        log::info!("plugins: {plugin}: tile {id} = {text:?}");
                    } else {
                        log::warn!("plugins: {plugin}: control tile {id:?} is not registered");
                    }
                }
                UiRequest::GoTo { position } => {
                    log::info!("plugins: {plugin}: go to {position}");
                    if self.book.is_some() {
                        self.pending_goto = Some(position);
                    }
                }
                UiRequest::LibraryRescan => {
                    log::info!("plugins: {plugin}: library rescan");
                    self.rescan_wanted = true;
                }
                UiRequest::Notification { text } => {
                    log::info!("plugins: {plugin}: notification {text:?}");
                    self.notifications.push(Notification { plugin: plugin.clone(), text: text.clone(), read: false });
                    self.ui.push(&plugin, UiRequest::Notification { text });
                }
                req @ (UiRequest::Menu { .. } | UiRequest::MenuLabel { .. }) if self.claimed.as_deref() == Some(plugin.as_str()) => {
                    log::info!("plugins: {plugin}: {} (shown by the screen)", describe_request(&req));
                    self.claimed_out.push(req);
                }
                other => {
                    log::info!("plugins: {plugin}: {}", describe_request(&other));
                    self.ui.push(&plugin, other);
                }
            }
        }
        let errors: Vec<(String, String)> = self.manager.errors[self.errors_seen..].to_vec();
        self.errors_seen = self.manager.errors.len();
        for (plugin, err) in errors {
            log::warn!("plugins: {plugin}: {err}");
            let text = format!("Plugin {plugin}: {}", short(&err, 90));
            self.notifications.push(Notification { plugin: plugin.clone(), text: text.clone(), read: false });
            self.ui.push(&plugin, UiRequest::Toast { text });
        }
        // A disabled plugin's requests stop. This runs after every call, page-turn
        // broadcasts included, so the names are borrowed, not copied.
        if let Some(w) = self.net.worker.as_mut() {
            for name in self.manager.disabled() {
                w.cancel_plugin(name);
            }
        }
    }

    // -- Lifecycle -----------------------------------------------------------------------

    /// A book was opened (after the first layout).
    pub fn book_open(&mut self, doc: &mut dyn Document, path: &Path) {
        let id = ink_store::partial_md5(path).ok();
        self.book = Some(OpenBook { path: path.to_path_buf(), id });
        self.broadcast(Some(doc), Lifecycle::BookOpen);
    }

    /// The reader shows `page` (0-based); call after `doc.set_current_page(page)`.
    pub fn page_change(&mut self, doc: &mut dyn Document, page: usize) {
        self.broadcast(Some(doc), Lifecycle::PageChange(page));
    }

    /// The user selected text (the reader's selection popup then lists
    /// [`PluginHost::selection_actions`]).
    pub fn selection(&mut self, doc: &mut dyn Document, sel: &Selection) {
        self.broadcast(Some(doc), Lifecycle::Selection(sel.clone()));
    }

    /// The device is going to sleep (before the screensaver is drawn): running plugin
    /// network requests are cancelled (each plugin gets an `Error` event for them), then
    /// `on_suspend` runs. A second call before [`PluginHost::resume`] does nothing.
    pub fn suspend(&mut self, mut doc: Option<&mut dyn Document>) {
        if self.suspended {
            return;
        }
        self.suspended = true;
        let interrupted = self.net.worker.as_mut().map(NetWorker::interrupt_all).unwrap_or_default();
        for (plugin, id) in interrupted {
            log::info!("plugins: {plugin}: http {id} interrupted by sleep");
            let ev = ink_plugin::HttpEvent::Error(SLEEP_INTERRUPT.into());
            self.call(&plugin, reborrow(&mut doc), None, &format!("on_http({id})"), |mgr, cx| mgr.http_event(&plugin, id, &ev, cx));
        }
        self.broadcast(doc, Lifecycle::Suspend);
    }

    /// The device woke up (after the screen is redrawn): `on_resume` runs. Does nothing
    /// unless [`PluginHost::suspend`] ran before.
    pub fn resume(&mut self, doc: Option<&mut dyn Document>) {
        if !self.suspended {
            return;
        }
        self.suspended = false;
        self.broadcast(doc, Lifecycle::Resume);
    }

    /// True when the user turned `plugin` off (it is listed, not loaded).
    pub fn turned_off(&self, plugin: &str) -> bool {
        self.off.contains(plugin)
    }

    /// True between [`PluginHost::suspend`] and [`PluginHost::resume`].
    pub fn suspended(&self) -> bool {
        self.suspended
    }

    /// True while plugin network requests run (the device should not sleep on its own).
    pub fn net_busy(&self) -> bool {
        self.net.worker.as_ref().is_some_and(|w| w.activity().busy())
    }

    /// The position a plugin asked the reader to go to, once ([`UiRequest::GoTo`]).
    pub fn take_goto(&mut self) -> Option<String> {
        self.pending_goto.take()
    }

    /// True once after a plugin asked for a library rescan ([`UiRequest::LibraryRescan`]).
    pub fn take_rescan(&mut self) -> bool {
        std::mem::take(&mut self.rescan_wanted)
    }

    /// rinky is exiting: `on_exit` runs once (later calls do nothing), with
    /// `restarting` from [`restart_pending`].
    pub fn app_exit(&mut self) {
        if std::mem::replace(&mut self.exited, true) {
            return;
        }
        let restarting = restart_pending();
        log::info!("plugins: exit{}", if restarting { " (restart: the framework stays stopped)" } else { "" });
        self.broadcast(None, Lifecycle::Exit(restarting));
    }

    /// The book is being closed.
    pub fn book_close(&mut self, doc: &mut dyn Document) {
        self.broadcast(Some(doc), Lifecycle::Close);
        self.book = None;
    }

    // -- Registries ------------------------------------------------------------------------

    /// Reader menu entries, sorted by order.
    pub fn menu_items(&self) -> &[MenuItem] {
        &self.manager.registry.menu_items
    }

    /// Entries for the selection popup.
    pub fn selection_actions(&self) -> &[SelectionAction] {
        &self.manager.registry.selection_actions
    }

    /// Functions plugins offer for the reader's gesture map.
    pub fn actions(&self) -> &[ink_plugin::ReaderAction] {
        &self.manager.registry.actions
    }

    /// Pages for the settings screen.
    pub fn settings_pages(&self) -> &[SettingsPage] {
        &self.manager.registry.settings_pages
    }

    /// Entries for a book's long-press menu in the library.
    pub fn library_actions(&self) -> &[LibraryAction] {
        &self.manager.registry.library_actions
    }

    /// Control-center tiles of the enabled plugins.
    pub fn control_tiles(&self) -> &[ink_plugin::ControlTile] {
        &self.manager.registry.control_tiles
    }

    /// Library-level actions of the enabled plugins (overflow menu, navigation bar).
    pub fn library_entries(&self) -> &[ink_plugin::LibraryEntry] {
        &self.manager.registry.library_entries
    }

    /// The control tile `plugin`/`id` with its current state; `None` when the plugin is
    /// off or has no such tile.
    pub fn control_tile(&self, plugin: &str, id: &str) -> Option<(ink_plugin::ControlTile, ink_plugin::TileState)> {
        let tile = self.manager.registry.control_tiles.iter().find(|t| t.plugin == plugin && t.id == id)?.clone();
        let state = self.manager.tile_state(plugin, id)?;
        Some((tile, state))
    }

    /// The library entry `plugin`/`id`.
    pub fn library_entry(&self, plugin: &str, id: &str) -> Option<&ink_plugin::LibraryEntry> {
        self.library_entries().iter().find(|e| e.plugin == plugin && e.id == id)
    }

    /// Status-bar text for `slot` (all plugin widgets in it, joined).
    pub fn status_line(&self, slot: StatusSlot) -> String {
        self.manager.registry.status_line(slot)
    }

    /// Triggers registry entry `id` of `plugin` (a reader menu item, a selection action,
    /// or `settings:<page id>` for a settings page). Returns false when the call failed.
    pub fn trigger(&mut self, plugin: &str, id: &str, mut doc: Option<&mut dyn Document>) -> bool {
        self.poll(reborrow(&mut doc));
        self.call(plugin, doc, None, &format!("on_action({id})"), |mgr, cx| mgr.action(plugin, id, cx))
    }

    /// Opens a plugin settings page (calls `on_action("settings:<id>")`).
    pub fn open_settings_page(&mut self, page: &SettingsPage, doc: Option<&mut dyn Document>) -> bool {
        let (plugin, id) = (page.plugin.clone(), format!("settings:{}", page.id));
        self.trigger(&plugin, &id, doc)
    }

    /// Runs a library action on `book` (no book open; per-book storage refers to `book`).
    pub fn library_action(&mut self, action: &LibraryAction, book: &Path) -> bool {
        let (plugin, id) = (action.plugin.clone(), action.id.clone());
        self.call(&plugin, None, Some(book), &format!("on_library_action({id}, {})", book.display()), |mgr, cx| mgr.library_action(&plugin, &id, book, cx))
    }

    /// Runs the actions bound to gesture `name` (see [`gesture_name`]). Returns true when
    /// a plugin had a binding (the screen then ignores the gesture).
    pub fn gesture(&mut self, name: &str, mut doc: Option<&mut dyn Document>) -> bool {
        let bindings: Vec<(String, String)> = self.manager.registry.gesture_bindings(name).into_iter().map(|g| (g.plugin.clone(), g.id.clone())).collect();
        for (plugin, id) in &bindings {
            self.trigger(plugin, id, reborrow(&mut doc));
        }
        !bindings.is_empty()
    }

    // -- Events, painting, timers -------------------------------------------------------------

    /// Call first in `AppScreen::handle` with every event. Delivers finished network
    /// events, lets an open plugin dialog/menu/viewer/keyboard take the event and sends
    /// its answer to the plugin. Returns true when the event was used (the screen must
    /// ignore it).
    pub fn handle_event(&mut self, ev: &UiEvent, mut doc: Option<&mut dyn Document>, screen: Size) -> bool {
        self.poll(reborrow(&mut doc));
        if let UiEvent::Power(p) = ev {
            match p {
                PowerEvent::GoingToScreenSaver => self.suspend(reborrow(&mut doc)),
                PowerEvent::OutOfScreenSaver => self.resume(reborrow(&mut doc)),
                // Ask for a deferral at once on the next keep_awake.
                PowerEvent::ReadyToSuspend => self.last_defer = None,
                _ => {}
            }
        }
        let (used, results) = self.ui.handle(ev, screen);
        for (plugin, id, result) in results {
            self.answer(&plugin, id, &result, reborrow(&mut doc));
        }
        used
    }

    /// Sends the answer `result` to request `id` of `plugin` (`on_ui_result`). The
    /// overlay does this for its own layers; a screen that shows a plugin dialog itself
    /// ([`PluginHost::take_card_dialog`]) calls it for that dialog's buttons.
    pub fn answer(&mut self, plugin: &str, id: u32, result: &str, doc: Option<&mut dyn Document>) -> bool {
        log::info!("plugins: {plugin}: answer {id} = {result:?}");
        self.call(plugin, doc, None, &format!("on_ui_result({id})"), |mgr, cx| mgr.ui_result(plugin, id, result, cx))
    }

    /// Takes the first waiting dialog of `plugin` that uses the card convention
    /// ([`CardDialog`]) out of the overlay's queue, for a screen that shows it in its own
    /// widget tree (the reader puts the dictionary card next to the selection card, so
    /// one tap on the page closes both). The screen answers it with
    /// [`PluginHost::answer`]. Other requests stay queued.
    pub fn take_card_dialog(&mut self, plugin: &str) -> Option<(u32, CardDialog)> {
        self.ui.take_card(plugin)
    }

    /// Makes the calling screen show `plugin`'s menus itself, as a settings page or a
    /// settings popup (Settings' plugin pages, the control center's long press on a
    /// plugin tile): its [`UiRequest::Menu`] and [`UiRequest::MenuLabel`] requests no
    /// longer go to the overlay but wait for [`PluginHost::take_claimed_menus`]. `None`
    /// gives the menus back to the overlay (and drops the ones not taken). The screen
    /// answers the menu with [`PluginHost::answer`]. Other requests (dialogs, the viewer,
    /// the keyboard, toasts) still go to the overlay.
    pub fn claim_menus(&mut self, plugin: Option<&str>) {
        if self.claimed.as_deref() != plugin {
            log::info!("plugins: menus of {plugin:?} shown by the screen (was {:?})", self.claimed);
        }
        self.claimed = plugin.map(str::to_string);
        if plugin.is_none() {
            self.claimed_out.clear();
        }
    }

    /// The plugin whose menus a screen shows ([`PluginHost::claim_menus`]).
    pub fn claimed_menus(&self) -> Option<&str> {
        self.claimed.as_deref()
    }

    /// The menus and label changes of the claimed plugin since the last call, oldest
    /// first.
    pub fn take_claimed_menus(&mut self) -> Vec<UiRequest> {
        std::mem::take(&mut self.claimed_out)
    }

    /// Delivers network events that arrived and runs the `on_tick` calls that are due
    /// (called by [`PluginHost::handle_event`], the lifecycle calls,
    /// [`PluginHost::trigger`] and every app loop frame).
    pub fn poll(&mut self, mut doc: Option<&mut dyn Document>) {
        let events = match self.net.worker.as_mut() {
            Some(w) if w.in_flight() > 0 => w.poll(),
            _ => Vec::new(),
        };
        for (plugin, id, ev) in events {
            log::debug!("plugins: {plugin}: http {id} {ev:?}");
            self.call(&plugin, reborrow(&mut doc), None, &format!("on_http({id})"), |mgr, cx| mgr.http_event(&plugin, id, &ev, cx));
        }
        self.tick(doc);
    }

    /// Calls `on_tick` of every plugin whose [`Plugin::tick_interval`] passed since its
    /// last tick. Not while the device sleeps.
    fn tick(&mut self, mut doc: Option<&mut dyn Document>) {
        let wanted = self.manager.tick_intervals();
        if wanted.is_empty() {
            self.last_tick.clear();
            return;
        }
        if self.suspended {
            return;
        }
        let now = Instant::now();
        self.last_tick.retain(|p, _| wanted.iter().any(|(w, _)| w == p));
        for (plugin, every) in wanted {
            let due = self.last_tick.get(&plugin).is_none_or(|t| now.saturating_duration_since(*t) >= every);
            if !due {
                continue;
            }
            self.last_tick.insert(plugin.clone(), now);
            let Some(m) = self.manager.manifest(&plugin).cloned() else { continue };
            // Not through `call`: a tick every 250 ms would flood the log.
            let n = self.out.len();
            let r = self.run_as(&m, reborrow(&mut doc), None, |manager, cx| manager.tick(&plugin, cx));
            if r.is_err() || self.out.len() > n {
                log::info!("plugins: on_tick -> {plugin}{}", if r.is_ok() { "" } else { " failed" });
            }
            self.after_calls();
        }
    }

    /// Time until the next `on_tick` is due, when a plugin wants ticks.
    fn tick_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        self.manager
            .tick_intervals()
            .into_iter()
            .map(|(p, every)| match self.last_tick.get(&p) {
                Some(t) => every.saturating_sub(now.saturating_duration_since(*t)),
                None => Duration::ZERO,
            })
            .min()
    }

    /// Call at the end of `AppScreen::paint`. Paints plugin overlays (toasts, dialogs,
    /// menus, the viewer, the keyboard) over the canvas and returns the damage to show.
    /// Pass `below_redrawn = true` when the screen repainted the canvas this time (the
    /// overlays are then drawn again on top).
    pub fn paint_overlay(&mut self, cx: &mut PaintCx, below_redrawn: bool) -> Vec<Damage> {
        self.overlay_painted = true;
        self.ui.paint(cx, below_redrawn)
    }

    /// How long the screen may sleep: until the next toast expires, 200 ms while
    /// network requests run, or until the next plugin tick is due. Fold it into
    /// `AppScreen::timeout`.
    pub fn timeout(&self) -> Option<Duration> {
        let net = self.net.worker.as_ref().filter(|w| w.in_flight() > 0).map(|_| Duration::from_millis(200));
        let tick = if self.suspended { None } else { self.tick_timeout() };
        [net, self.ui.timeout(), tick].into_iter().flatten().min()
    }

    /// True while a plugin dialog, menu, viewer or keyboard is open.
    pub fn modal_open(&self) -> bool {
        self.ui.modal_open()
    }

    /// Takes the UI requests the presenter has not started showing (for screens that
    /// present them some other way; the overlay normally does it).
    pub fn take_ui_requests(&mut self) -> Vec<UiRequest> {
        self.ui.pending.drain(..).map(|(_, r)| r).collect()
    }

    // -- Plugin list and notifications --------------------------------------------------------

    /// Loaded plugins.
    pub fn plugins(&self) -> Vec<PluginInfo> {
        self.manager.info()
    }

    /// Notifications (plugin notifications and plugin errors), oldest first.
    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    /// Number of unread notifications.
    pub fn unread_notifications(&self) -> usize {
        self.notifications.iter().filter(|n| !n.read).count()
    }

    /// Marks all notifications read.
    pub fn mark_notifications_read(&mut self) {
        for n in &mut self.notifications {
            n.read = true;
        }
    }
}

// ---------------------------------------------------------------------------------------
// Turning plugins on and off, `rinky plugins`

/// File under `paths.plugins()` with the names of the plugins the user turned off, one
/// per line (`#` comments). Read by [`PluginHost::load`] at start.
pub const DISABLED_FILE: &str = "disabled.txt";

/// Plugins the user turned off.
pub fn user_disabled(paths: &Paths) -> BTreeSet<String> {
    match std::fs::read_to_string(paths.plugins().join(DISABLED_FILE)) {
        Ok(text) => text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// Turns plugin `name` on or off for the next start (writes [`DISABLED_FILE`]).
pub fn set_user_enabled(paths: &Paths, name: &str, on: bool) -> anyhow::Result<()> {
    anyhow::ensure!(ink_plugin::manifest::valid_name(name), "{name:?} is not a valid plugin name");
    let mut off = user_disabled(paths);
    let changed = if on { off.remove(name) } else { off.insert(name.to_string()) };
    if !changed {
        return Ok(());
    }
    let dir = paths.plugins();
    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
    let mut text = String::from("# Plugins turned off (rinky plugins disable NAME); one name per line.\n");
    for n in &off {
        text.push_str(n);
        text.push('\n');
    }
    let file = dir.join(DISABLED_FILE);
    ink_store::write_atomic(&file, text.as_bytes()).map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?;
    log::info!("plugins: {name} turned {}", if on { "on" } else { "off" });
    Ok(())
}

/// One line per plugin for `rinky plugins list`.
pub fn plugin_list(host: &PluginHost) -> Vec<String> {
    let r = &host.manager.registry;
    host.plugins()
        .iter()
        .map(|i| {
            let n = &i.manifest.name;
            let state = if host.turned_off(n) {
                "off"
            } else if i.enabled {
                "on"
            } else {
                "failed"
            };
            // The short names `rinky plugins list` has always printed, in registry order.
            const SHORT: [&str; 7] = ["menu", "selection", "gesture", "action", "settings", "library", "status"];
            let counts = registrations(r, |p| p == n);
            let regs: Vec<String> = SHORT.iter().zip(counts).filter(|(_, (_, c))| *c > 0).map(|(k, (_, c))| format!("{k} {c}")).collect();
            let perms: Vec<&str> = i.manifest.permissions.iter().map(|p| p.name()).collect();
            format!(
                "{n}\t{}\t{}\t{state}\terrors {}\t[{}]\t{}\t{}",
                i.manifest.version,
                i.kind,
                i.errors,
                perms.join(","),
                if regs.is_empty() { "-".to_string() } else { regs.join(", ") },
                i.manifest.description
            )
        })
        .collect()
}

/// `rinky plugins [list | enable NAME | disable NAME]`: lists the compiled-in and
/// Lua plugins (from `<RINKY_HOME>/plugins`) with state, errors and registrations,
/// or turns one on/off for the next start. Exit status 1 for an unknown plugin.
pub fn cli(args: &[String], compiled: Vec<Box<dyn Plugin>>, settings: &Settings, paths: &Paths) -> anyhow::Result<i32> {
    let host = PluginHost::load(compiled, settings, paths);
    match args.first().map(String::as_str) {
        None | Some("list") => {
            println!("# {} (name, version, kind, state, errors, permissions, registrations, description)", paths.plugins().display());
            for line in plugin_list(&host) {
                println!("{line}");
            }
            for (name, err) in &host.manager.errors {
                println!("! {name}: {err}");
            }
            Ok(0)
        }
        Some(cmd @ ("enable" | "disable")) => {
            let Some(name) = args.get(1) else { anyhow::bail!("plugins {cmd} needs a NAME") };
            if host.manager.manifest(name).is_none() {
                eprintln!("rinky: no plugin named {name} (see `rinky plugins list`)");
                return Ok(1);
            }
            let on = cmd == "enable";
            set_user_enabled(paths, name, on)?;
            println!("{name}: {} (takes effect at the next start)", if on { "on" } else { "off" });
            Ok(0)
        }
        Some(other) => anyhow::bail!("unknown plugins command {other} (list, enable NAME, disable NAME)"),
    }
}

// ---------------------------------------------------------------------------------------
// Hooks: the API screens and the app loop call

/// Which registry a [`PluginEntry`] comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A reader menu item ([`hooks::reader_menu`]).
    ReaderMenu,
    /// A selection popup action ([`hooks::selection_actions`]).
    Selection,
    /// A library long-press action on a book ([`hooks::library_actions`]).
    Library,
    /// A plugin settings page ([`hooks::settings_pages`]).
    Settings,
}

/// One plugin entry a screen lists in its own menu. Show `label`; on a pick call
/// [`hooks::run_entry`] with the entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginEntry {
    /// Plugin name.
    pub plugin: String,
    /// The plugin's id for the entry.
    pub id: String,
    /// Text to show (a settings page's title).
    pub label: String,
    /// Registry it comes from.
    pub kind: EntryKind,
}

/// The plugin hook API for screens (`screens/reader`, `screens/library`,
/// `screens/settings`) and for the app loop. Every function takes the screen's
/// [`AppCtx`]; documents are passed as `&mut dyn Document`.
///
/// Screens call the lifecycle and registry functions. Events, overlay painting, timers,
/// network delivery and the power key are done by the app loop ([`hooks::app_handle`],
/// [`hooks::app_paint`], [`hooks::app_timeout`]); a screen only implements
/// `AppScreen::document` so plugins see its open book. Screens must not call
/// `PluginHost::handle_event` or `PluginHost::paint_overlay` themselves.
pub mod hooks {
    use super::{EntryKind, PluginEntry, PluginHost};
    use crate::router::{AppCtx, AppScreen};
    use ink_doc::{Document, Selection};
    use ink_plugin::registry::StatusSlot;
    use ink_device::{Canvas, Rect, RefreshClass, Rotation, Screen, Size};
    use ink_ui::{Damage, Gesture, PaintCx, UiEvent};
    use std::path::Path;
    use std::time::Duration;

    // -- Lifecycle (reader) ---------------------------------------------------------------

    /// The reader opened `path` and laid it out (in `on_enter`).
    pub fn on_book_open(cx: &mut AppCtx, doc: &mut dyn Document, path: &Path) {
        cx.plugins.book_open(doc, path);
    }

    /// The reader shows `page` (0-based), after `doc.set_current_page(page)`.
    pub fn on_page_change(cx: &mut AppCtx, doc: &mut dyn Document, page: usize) {
        cx.plugins.page_change(doc, page);
    }

    /// The user made a selection (before the popup opens; the popup lists
    /// [`selection_actions`]).
    pub fn on_selection(cx: &mut AppCtx, doc: &mut dyn Document, sel: &Selection) {
        cx.plugins.selection(doc, sel);
    }

    /// The device goes to sleep. The app loop already calls this on
    /// `PowerEvent::GoingToScreenSaver` (with `AppScreen::document`); a second call
    /// before [`on_resume`] does nothing.
    pub fn on_suspend(cx: &mut AppCtx, doc: Option<&mut dyn Document>) {
        cx.plugins.suspend(doc);
    }

    /// The device woke up. The app loop already calls this on
    /// `PowerEvent::OutOfScreenSaver`; a call without a preceding suspend does nothing.
    pub fn on_resume(cx: &mut AppCtx, doc: Option<&mut dyn Document>) {
        cx.plugins.resume(doc);
    }

    /// The reader closes the book (in `on_exit`).
    pub fn on_close(cx: &mut AppCtx, doc: &mut dyn Document) {
        cx.plugins.book_close(doc);
    }

    // -- Registries -----------------------------------------------------------------------

    /// Reader menu entries, in the plugins' order.
    pub fn reader_menu(cx: &AppCtx) -> Vec<PluginEntry> {
        entries(cx.plugins, EntryKind::ReaderMenu)
    }

    /// Selection popup entries.
    pub fn selection_actions(cx: &AppCtx) -> Vec<PluginEntry> {
        entries(cx.plugins, EntryKind::Selection)
    }

    /// Entries for a book's long-press menu in the library.
    pub fn library_actions(cx: &AppCtx) -> Vec<PluginEntry> {
        entries(cx.plugins, EntryKind::Library)
    }

    /// Plugin settings pages (list them under "Plugins").
    pub fn settings_pages(cx: &AppCtx) -> Vec<PluginEntry> {
        entries(cx.plugins, EntryKind::Settings)
    }

    /// The entries of one registry.
    pub fn entries(host: &PluginHost, kind: EntryKind) -> Vec<PluginEntry> {
        let e = |plugin: &str, id: &str, label: &str| PluginEntry { plugin: plugin.into(), id: id.into(), label: label.into(), kind };
        match kind {
            EntryKind::ReaderMenu => host.menu_items().iter().map(|m| e(&m.plugin, &m.id, &m.label)).collect(),
            EntryKind::Selection => host.selection_actions().iter().map(|m| e(&m.plugin, &m.id, &m.label)).collect(),
            EntryKind::Library => host.library_actions().iter().map(|m| e(&m.plugin, &m.id, &m.label)).collect(),
            EntryKind::Settings => host.settings_pages().iter().map(|m| e(&m.plugin, &m.id, &m.title)).collect(),
        }
    }

    /// Runs a picked entry. `doc`: the open book (reader); `book`: the book a library
    /// action is for (required for [`EntryKind::Library`]). The plugin answers with
    /// toasts, dialogs, menus or a viewer, which the app loop draws. Returns false when
    /// the plugin call failed (the error is shown as a toast already).
    pub fn run_entry(cx: &mut AppCtx, entry: &PluginEntry, doc: Option<&mut dyn Document>, book: Option<&Path>) -> bool {
        let host = &mut *cx.plugins;
        match entry.kind {
            EntryKind::ReaderMenu | EntryKind::Selection => host.trigger(&entry.plugin, &entry.id, doc),
            EntryKind::Settings => host.trigger(&entry.plugin, &format!("settings:{}", entry.id), doc),
            EntryKind::Library => match book {
                Some(book) => {
                    let action = ink_plugin::registry::LibraryAction { plugin: entry.plugin.clone(), id: entry.id.clone(), label: entry.label.clone() };
                    host.library_action(&action, book)
                }
                None => {
                    log::warn!("plugins: library action {}/{} without a book", entry.plugin, entry.id);
                    false
                }
            },
        }
    }

    /// Offers a gesture the screen does not use itself to plugin bindings. Returns true
    /// when a plugin took it (the screen then ignores it).
    pub fn gesture(cx: &mut AppCtx, g: &Gesture, doc: Option<&mut dyn Document>) -> bool {
        let size = cx.screen.size();
        match super::gesture_name(g, size) {
            Some(name) => cx.plugins.gesture(&name, doc),
            None => false,
        }
    }

    /// Status-bar text of the plugin widgets in `slot` (empty when none).
    pub fn status_text(cx: &AppCtx, slot: StatusSlot) -> String {
        cx.plugins.status_line(slot)
    }

    /// Unread plugin notifications (for a badge).
    pub fn unread_notifications(cx: &AppCtx) -> usize {
        cx.plugins.unread_notifications()
    }

    // -- App loop -------------------------------------------------------------------------

    /// App loop, before `top.handle(ev)`: delivers network results, gives the event to an
    /// open plugin dialog/menu/viewer/keyboard, and runs the plugins' suspend/resume on
    /// power events. Returns true when a plugin layer used the event (the screen must
    /// not see it).
    pub fn app_handle(top: &mut dyn AppScreen, ev: &UiEvent, cx: &mut AppCtx) -> bool {
        let size = cx.screen.size();
        let used = cx.plugins.handle_event(ev, top.document(), size);
        apply_goto(top, cx);
        used
    }

    /// Hands a plugin's [`UiRequest::GoTo`] to the top screen (`AppScreen::plugin_go_to`).
    fn apply_goto(top: &mut dyn AppScreen, cx: &mut AppCtx) {
        if let Some(pos) = cx.plugins.take_goto() {
            if !top.plugin_go_to(&pos) {
                log::info!("plugins: go to {pos}: screen {} has no book", top.name());
            }
        }
    }

    /// A [`Screen`] that records the rectangles shown through it.
    struct Recorder<'a> {
        inner: &'a mut dyn Screen,
        shown: Vec<Damage>,
        wave: Option<ink_device::Wave>,
    }

    impl Screen for Recorder<'_> {
        fn panel_size(&self) -> Size {
            self.inner.panel_size()
        }
        fn rotation(&self) -> Rotation {
            self.inner.rotation()
        }
        fn set_rotation(&mut self, rotation: Rotation) -> ink_device::Result<()> {
            self.inner.set_rotation(rotation)
        }
        fn size(&self) -> Size {
            self.inner.size()
        }
        fn night_mode(&self) -> bool {
            self.inner.night_mode()
        }
        fn set_night_mode(&mut self, on: bool) -> ink_device::Result<()> {
            self.inner.set_night_mode(on)
        }
        fn update(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass) -> ink_device::Result<()> {
            self.shown.push(Damage { rect, class });
            self.inner.update(canvas, rect, class)
        }
        fn update_wave(&mut self, canvas: &Canvas, rect: Rect, class: RefreshClass, wave: ink_device::Wave) -> ink_device::Result<()> {
            self.shown.push(Damage { rect, class });
            self.wave = Some(wave);
            self.inner.update_wave(canvas, rect, class, wave)
        }
        fn frame_done(&mut self, canvas: &Canvas) {
            self.inner.frame_done(canvas)
        }
        fn refresh_all(&mut self) -> ink_device::Result<()> {
            self.inner.refresh_all()
        }
        fn screenshot(&self) -> ink_device::Result<Canvas> {
            self.inner.screenshot()
        }
    }

    /// App loop, instead of `top.paint(cx)`: delivers network results, lets the screen
    /// paint, then draws the plugin overlay over what the screen changed (its damage and
    /// what it showed itself through `cx.screen`). Returns the damage to show.
    pub fn app_paint(top: &mut dyn AppScreen, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        app_frame(top, cx).map(|f| f.damage)
    }

    /// One painted frame: what the screen showed itself through `cx.screen` during the
    /// paint (page turns, full-screen restores; already on the panel) and the damage
    /// still to show.
    pub struct Frame {
        /// Updates the screen sent itself while painting.
        pub shown: Vec<Damage>,
        /// Damage of the screen and the plugin layers, not shown yet.
        pub damage: Vec<Damage>,
        /// The page-turn wave of a `shown` update ([`Screen::update_wave`]).
        pub wave: Option<ink_device::Wave>,
    }

    /// [`app_paint`], also returning the updates the screen sent itself (for the
    /// app loop's per-frame debug log).
    pub fn app_frame(top: &mut dyn AppScreen, cx: &mut AppCtx) -> anyhow::Result<Frame> {
        cx.plugins.poll(top.document());
        apply_goto(top, cx);
        cx.plugins.overlay_painted = false;
        let mut rec = Recorder { inner: &mut *cx.screen, shown: Vec::new(), wave: None };
        let mut damage = {
            let mut sub = AppCtx {
                canvas: &mut *cx.canvas,
                screen: &mut rec,
                fonts: &mut *cx.fonts,
                theme: cx.theme,
                refresh: &mut *cx.refresh,
                plugins: &mut *cx.plugins,
                settings: cx.settings,
                paths: cx.paths,
                providers: cx.providers,
                services: cx.services.reborrow(),
                settings_update: &mut *cx.settings_update,
            };
            top.paint(&mut sub)?
        };
        // A screen that still paints the overlay itself (older docs) did it already;
        // composing again would save the overlay's own pixels as its background.
        let (shown, wave) = (rec.shown, rec.wave);
        if cx.plugins.overlay_painted {
            return Ok(Frame { shown, damage, wave });
        }
        let changed: Vec<Rect> = shown.iter().chain(damage.iter()).map(|d| d.rect).collect();
        let over = cx.plugins.ui.compose(&mut PaintCx { canvas: &mut *cx.canvas, fonts: &mut *cx.fonts, theme: cx.theme }, &changed);
        if over.is_empty() {
            return Ok(Frame { shown, damage, wave });
        }
        damage.extend(over);
        Ok(Frame { shown, damage: ink_ui::refresh::merge(damage), wave })
    }

    /// App loop, once per turn: while plugin network requests run, asks `power` to
    /// hold off sleep ([`ink_net::connectivity::DEFER_HOLD`], renewed every
    /// [`ink_net::connectivity::DEFER_RENEW`], and at once after `readyToSuspend`). Returns
    /// true when it asked.
    pub fn keep_awake(host: &mut PluginHost, power: &mut dyn ink_device::Power) -> bool {
        use ink_net::connectivity::{DEFER_HOLD, DEFER_RENEW};
        if !host.net_busy() || host.suspended {
            return false;
        }
        let now = std::time::Instant::now();
        if host.last_defer.is_some_and(|t| now.saturating_duration_since(t) < DEFER_RENEW) {
            return false;
        }
        host.last_defer = Some(now);
        match power.defer_suspend(DEFER_HOLD) {
            Ok(()) => log::info!("plugins: network requests running; sleep deferred for {DEFER_HOLD:?}"),
            Err(e) => log::debug!("plugins: defer_suspend: {e}"),
        }
        true
    }

    /// App loop: how long to wait for input, the smaller of the screen's timeout and the
    /// plugins' (toasts expiring, network requests running).
    pub fn app_timeout(screen: Option<Duration>, host: &PluginHost) -> Option<Duration> {
        screen.into_iter().chain(host.timeout()).min()
    }
}

/// The `Error` text a plugin gets for a request cancelled because the device sleeps.
pub const SLEEP_INTERRUPT: &str = "interrupted: the device went to sleep";

fn describe_request(r: &UiRequest) -> String {
    match r {
        UiRequest::Toast { text } => format!("toast {text:?}"),
        UiRequest::Dialog { id, title, buttons, .. } => format!("dialog {id} {title:?} ({} buttons)", buttons.len()),
        UiRequest::Menu { id, title, items } => format!("menu {id} {title:?} ({} items)", items.len()),
        UiRequest::TextViewer { id, title, markdown } => format!("viewer {id} {title:?} ({} bytes of Markdown)", markdown.len()),
        UiRequest::TextInput { id, prompt, .. } => format!("input {id} {prompt:?}"),
        UiRequest::Notification { text } => format!("notification {text:?}"),
        UiRequest::Status { id, text } => format!("status {id} = {text:?}"),
        UiRequest::GoTo { position } => format!("go to {position}"),
        UiRequest::LibraryRescan => "library rescan".into(),
        UiRequest::TileState { id, text } => format!("tile {id} = {text:?}"),
        UiRequest::MenuLabel { id, action, label } => format!("menu {id} item {action} = {label:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// Presenter

#[path = "plugin_card.rs"]
mod card;
pub use card::CardDialog;

/// The largest `k` in `0..=n` for which `fits(k)` holds, for a `fits` that holds up to
/// some `k` and fails above it (`0` when it fails everywhere). A bisection: about
/// log2(n) calls, where trimming one character at a time made `n` calls.
fn last_fitting(n: usize, mut fits: impl FnMut(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Truncates `text` with `...` to fit `max` pixels.
fn fit(cx: &mut PaintCx, text: &str, px: f32, style: Style, max: f32) -> String {
    if cx.fonts.text_width(style, text, px) <= max {
        return text.to_string();
    }
    // Byte ends of the proper prefixes, by character count. None for empty text (it
    // misses only a negative `max`): just the dots, as trimming gave.
    let ends: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let Some(n) = ends.len().checked_sub(1) else { return "...".to_string() };
    let k = last_fitting(n, |k| cx.fonts.text_width(style, &format!("{}...", &text[..ends[k]]), px) <= max);
    format!("{}...", text[..ends[k]].trim_end())
}

/// The longest end of `text` that fits `max` pixels (the end of typed text, before the
/// cursor).
fn fit_end<'a>(cx: &mut PaintCx, text: &'a str, px: f32, max: f32) -> &'a str {
    let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).chain([text.len()]).collect();
    let n = starts.len() - 1;
    let kept = last_fitting(n, |m| cx.fonts.text_width(Style::Regular, &text[starts[n - m]..], px) <= max);
    &text[starts[n - kept]..]
}

/// A plugin menu: the settings popup ([`SettingsPopup`]) with the menu's items as
/// settings rows ([`popup::menu_rows`]: checks, switches, details, back rows, notes), the
/// Close button in the title row and a list that jumps when the items do not fit. A tap
/// on a row reports the item's action; Close reports `""` (cancelled).
pub struct MenuBox {
    /// Title.
    pub title: String,
    /// All `(label, action)` items.
    pub items: Vec<(String, String)>,
    /// The popup showing them.
    pub popup: SettingsPopup,
}

impl MenuBox {
    /// A menu of `items`.
    pub fn new(title: String, items: Vec<(String, String)>) -> Self {
        let popup = SettingsPopup::new(title.clone(), popup::menu_rows(&items)).with_close("");
        MenuBox { title, items, popup }
    }

    /// The menu's rectangle on a screen of `size`: centred, at most four fifths of the
    /// screen high.
    pub fn frame(&self, size: Size) -> Rect {
        let area = Rect::new(0, (size.h / 10) as i32, size.w, size.h * 4 / 5);
        self.popup.frame(area, Anchor::Center)
    }

    /// The Close button in the menu drawn at `b` (a tap cancels the menu like a tap
    /// outside it or the Back key).
    pub fn close_rect(b: Rect) -> Rect {
        SettingsPopup::close_rect(b)
    }

    /// Sets the label of the item with `action`. Returns the rectangle (in the menu's
    /// own coordinates, the menu drawn at `b`) to repaint: the row when it is on
    /// screen, `None` when nothing is shown differently.
    pub fn set_label(&mut self, b: Rect, action: &str, label: &str) -> Option<Option<Rect>> {
        let i = self.items.iter().position(|(_, a)| a == action)?;
        if self.items[i].0 == label {
            return None;
        }
        self.items[i].0 = label.to_string();
        let choices = self.items.iter().any(|(l, _)| ink_ui::dialog::split_check(l).0);
        self.popup.set_row(i, popup::menu_row(i, label, action, choices));
        let area = self.popup.list_rect(b);
        Some(self.popup.list.shown_rect(area, i))
    }

    /// Scroll offset of the list.
    pub fn scroll(&self) -> i32 {
        self.popup.list.scroll
    }
}

impl Widget for MenuBox {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        self.popup.paint(cx, b);
        // The overlay refreshes whole layers or the rows it names: reset the record.
        let _ = self.popup.damage(b);
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        match self.popup.event(ev, b) {
            EventResult::Action(a) if a.is_empty() => EventResult::Action(a),
            EventResult::Action(a) => match popup::menu_pick(&a, &self.items) {
                Some(action) => EventResult::Action(action),
                None => EventResult::Handled,
            },
            other => other,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        self.popup.list.refresh_class()
    }
}

/// One laid-out run of text.
#[derive(Clone, Debug)]
struct Run {
    x: f32,
    text: String,
    style: Style,
    px: f32,
    link: Option<String>,
    /// Secondary text (drawn in `muted`).
    muted: bool,
}

/// One laid-out line.
#[derive(Clone, Debug, Default)]
struct Line {
    height: f32,
    runs: Vec<Run>,
    /// Gray background (code).
    shade: bool,
    /// Quote bar at this x.
    bar: Option<f32>,
    /// Horizontal rule.
    rule: bool,
    /// A thin gray rule (between the rows of a wrapped table).
    hairline: bool,
    /// The `:::` box this line belongs to (index into `MarkdownView::groups`).
    group: Option<usize>,
    /// Icon with its left edge at this x (a square of 0.8 x the line height, 56 at most).
    icon: Option<(f32, Icon)>,
}

/// What a `:::` block of the viewer draws (see [`MarkdownView`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoxKind {
    /// Plain Markdown.
    Plain,
    /// The user's message: a `light` filled card on the right.
    You,
    /// An answer: an outlined card with a label line on top and a muted caption below.
    Answer,
    /// A compact gray chip (an agent action).
    Action,
    /// Tappable outlined pills, one per list item (the item's link).
    Chip,
    /// A tappable outlined card.
    Card,
    /// A sheet row: icon and label.
    Row,
}

/// A `:::` block: kind, its two `|`-separated arguments, its Markdown.
#[derive(Clone, Debug)]
struct Seg {
    kind: BoxKind,
    a: String,
    b: String,
    blocks: Vec<Block>,
}

/// A laid-out box: its x and width across the content area and what a tap reports.
#[derive(Clone, Debug)]
struct Group {
    kind: BoxKind,
    x: f32,
    w: f32,
    link: Option<String>,
}

/// What [`split_directives`] returns.
type Directives = (Vec<Seg>, Option<(String, String)>, bool, Option<String>);

/// Splits `md` at `:::` directive lines: the segments, the input bar
/// `(link, placeholder)` and whether the view is a sheet. Lines starting with `:::` and
/// an unknown word stay text.
fn split_directives(md: &str) -> Directives {
    let mut segs = Vec::new();
    let mut input = None;
    let mut sheet = false;
    let mut back = None;
    let mut cur = (BoxKind::Plain, String::new(), String::new(), String::new());
    let flush = |cur: (BoxKind, String, String, String), segs: &mut Vec<Seg>| {
        if cur.0 != BoxKind::Plain || !cur.3.trim().is_empty() {
            segs.push(Seg { kind: cur.0, a: cur.1, b: cur.2, blocks: markdown::parse(&cur.3) });
        }
    };
    for line in md.lines() {
        if let Some(rest) = line.trim().strip_prefix(":::") {
            let rest = rest.trim();
            let (word, args) = rest.split_once(' ').unwrap_or((rest, ""));
            let (a, b) = match args.split_once('|') {
                Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
                None => (args.trim().to_string(), String::new()),
            };
            let kind = match word {
                "" => Some(BoxKind::Plain),
                "you" => Some(BoxKind::You),
                "answer" => Some(BoxKind::Answer),
                "action" => Some(BoxKind::Action),
                "chips" => Some(BoxKind::Chip),
                "card" => Some(BoxKind::Card),
                "row" => Some(BoxKind::Row),
                "input" => {
                    input = Some((a, b));
                    continue;
                }
                "sheet" => {
                    sheet = true;
                    continue;
                }
                "back" => {
                    back = Some(a);
                    continue;
                }
                // Read by `PluginUi::push` and `MarkdownView::set_text`.
                "top" | "emphasis" => continue,
                _ => None,
            };
            if let Some(kind) = kind {
                flush(std::mem::replace(&mut cur, (kind, a, b, String::new())), &mut segs);
                continue;
            }
        }
        cur.3.push_str(line);
        cur.3.push('\n');
    }
    flush(cur, &mut segs);
    (segs, input, sheet, back)
}

thread_local! {
    /// The UI family with bold and italics for viewers with `:::emphasis`, and its name
    /// (built again when the UI font changes).
    static EMPHASIS_FONTS: std::cell::RefCell<Option<(String, ink_text::Fonts)>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` with the UI family's bold and italic faces
/// ([`crate::fonts::emphasis_fonts`]) when `emphasis`, else with `cx`'s UI fonts, which
/// draw bold and italic text in the regular face.
fn body_fonts<R>(emphasis: bool, cx: &mut PaintCx, f: impl FnOnce(&mut PaintCx) -> R) -> R {
    if !emphasis {
        return f(cx);
    }
    EMPHASIS_FONTS.with(|s| {
        let mut s = s.borrow_mut();
        let family = crate::fonts::ui_family();
        if s.as_ref().is_none_or(|(n, _)| *n != family) {
            *s = Some((family.clone(), crate::fonts::emphasis_fonts(&family)));
        }
        let (_, fonts) = s.as_mut().expect("set above");
        f(&mut PaintCx { canvas: &mut *cx.canvas, fonts, theme: cx.theme })
    })
}

/// The icon called `name` (`chat`, `info`, ...; any case).
pub(crate) fn icon_named(name: &str) -> Option<Icon> {
    Icon::ALL.iter().copied().find(|i| format!("{i:?}").eq_ignore_ascii_case(name.trim()))
}

/// Full-screen Markdown viewer (ink-ui [`Widget`]): app bar with a Close button, pages
/// turned by taps on the left/right or swipes, links reported as actions.
///
/// Lines of the form `:::kind a | b` draw the chat look (plain Markdown is unchanged):
/// `:::you` (the user's message, a filled card on the right), `:::answer LABEL | FOOT`
/// (an outlined card, label line on top, muted caption at the bottom), `:::action` (a
/// compact gray chip), `:::chips` (each list item a tappable pill reporting its link),
/// `:::card LINK | CAPTION` (a tappable outlined card), each ended by `:::` or the next
/// block. `:::input LINK | PLACEHOLDER` adds an input bar at the bottom whose tap
/// reports LINK. `:::sheet` shows the view as a bottom sheet of `:::row LINK | ICON`
/// rows (label as the body); a tap on a row or outside the sheet closes it (a row whose
/// link starts with `stay:` reports it and leaves the sheet open, for a checkbox the
/// plugin redraws by sending the sheet again). A long press on any link reports
/// `hold:LINK` and leaves the view open. `:::top` in
/// a replacing request opens the new text at its first page (tabs; otherwise a view
/// that was on its last page follows the end of the new text). `:::back LINK` adds a Back
/// button to the app bar (left of the title) that reports LINK.
pub struct MarkdownView {
    /// Title (a second line is the app bar's subtitle).
    pub title: String,
    blocks: Vec<Block>,
    segs: Vec<Seg>,
    /// Input bar: `(link, placeholder)`.
    input: Option<(String, String)>,
    /// Shown as a bottom sheet.
    pub sheet: bool,
    /// Link the Back button reports (`:::back`); no button when `None`.
    back: Option<String>,
    /// Bold and italic text is drawn bold and italic (`:::emphasis`), not in the UI's
    /// regular face.
    emphasis: bool,
    /// Current page (0-based).
    pub page: usize,
    pages: Vec<Vec<Line>>,
    groups: Vec<Group>,
    laid_for: Option<(u32, u32)>,
    links: Vec<(Rect, String)>,
    /// Jump to the last page at the next layout (streamed text that grows).
    pub follow_end: bool,
    /// Height of the docked keyboard panel at the bottom (0: none). While docked the
    /// view lays its pages out above it and leaves that strip blank: the panel shows the
    /// input bar lifted above the keyboard ([`InputBox::docked`]).
    dock: u32,
    /// Text typed into the docked input bar and not sent (the keyboard was put away):
    /// shown in the input bar and typed on when the keyboard opens again.
    draft: Option<String>,
    /// A chat view ([`MarkdownView::scrolls`]): px of the content scrolled off the top,
    /// the content's height and the window's height (from the last layout).
    scroll: f32,
    content_h: f32,
    window_h: f32,
    /// A chat view puts its `n`th `:::you` message at the top of the window at the next
    /// layout (a question was just sent: its answer streams in below it).
    anchor: Option<usize>,
    /// Widths of the words laid out so far.
    widths: WordWidths,
}

const FOOT_H: i32 = 80;
const MARGIN: f32 = 48.0;
/// Padding inside cards.
const PAD: f32 = 24.0;
/// Height of a sheet row.
const ROW_H: f32 = 120.0;
/// Input bar height, and the page counter band above it.
const INPUT_H: i32 = 136;
const COUNTER_H: i32 = 48;
/// Top of the content in a sheet (below its title, as [`BottomSheet::content`]).
const SHEET_TOP: i32 = 130;

/// Removes blank gap lines at the end.
fn trim_gaps(lines: &mut Vec<Line>) {
    while lines.last().is_some_and(|l| l.runs.is_empty() && !l.rule && l.icon.is_none()) {
        lines.pop();
    }
}

/// Moves lines `dx` to the right.
fn shift(lines: &mut [Line], dx: f32) {
    for l in lines {
        for r in &mut l.runs {
            r.x += dx;
        }
        if let Some(b) = &mut l.bar {
            *b += dx;
        }
        if let Some((x, _)) = &mut l.icon {
            *x += dx;
        }
    }
}

/// Shaped widths of the words a viewer laid out, kept across its layouts: streamed text
/// lays the whole text out again on every update, mostly with the same words. Valid for
/// one set of fonts ([`WordWidths::for_fonts`]).
#[derive(Default)]
struct WordWidths {
    /// The UI family and emphasis the widths were measured with.
    fonts: (String, bool),
    /// (style, px bits) -> text -> width.
    map: HashMap<(Style, u32), HashMap<String, f32>>,
    entries: usize,
}

impl WordWidths {
    /// Most widths kept; more start the cache again.
    const MAX: usize = 20_000;

    /// Forgets the widths when the fonts are not the ones they were measured with.
    fn for_fonts(&mut self, family: String, emphasis: bool) {
        if self.fonts != (family.clone(), emphasis) || self.entries > Self::MAX {
            *self = WordWidths { fonts: (family, emphasis), ..WordWidths::default() };
        }
    }
}

/// A viewer layout in progress: the paint context and the word widths of the view.
struct Lay<'a, 'c> {
    cx: &'a mut PaintCx<'c>,
    widths: &'a mut WordWidths,
}

impl<'c> std::ops::Deref for Lay<'_, 'c> {
    type Target = PaintCx<'c>;
    fn deref(&self) -> &PaintCx<'c> {
        self.cx
    }
}

impl std::ops::DerefMut for Lay<'_, '_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.cx
    }
}

impl Lay<'_, '_> {
    /// The shaped width of `text` (a word, a run), measured once per view.
    fn width(&mut self, style: Style, px: f32, text: &str) -> f32 {
        let words = self.widths.map.entry((style, px.to_bits())).or_default();
        if let Some(w) = words.get(text) {
            return *w;
        }
        let w = self.cx.fonts.shaped_width(style, px, text);
        words.insert(text.to_string(), w);
        self.widths.entries += 1;
        w
    }
}

/// Right end of the widest line.
fn extent(cx: &mut Lay, lines: &[Line]) -> f32 {
    let mut w: f32 = 0.0;
    for l in lines {
        for r in &l.runs {
            w = w.max(r.x + cx.width(r.style, r.px, &r.text));
        }
    }
    w
}

fn gap_line(h: f32) -> Line {
    Line { height: h, ..Line::default() }
}

/// The spans of all blocks, joined by spaces.
fn spans_of(blocks: &[Block]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for b in blocks {
        let spans = match b {
            Block::Heading { spans, .. } | Block::Paragraph(spans) | Block::Quote(spans) | Block::ListItem { spans, .. } => spans.clone(),
            Block::Code(lines) => vec![Span::plain(&lines.join(" "))],
            _ => continue,
        };
        if !out.is_empty() {
            out.push(Span::plain(" "));
        }
        out.extend(spans);
    }
    out
}

/// Adds a box: a top padding line, `body` and a bottom padding line, all in a new group.
fn push_box(lines: &mut Vec<Line>, groups: &mut Vec<Group>, group: Group, body: Vec<Line>, pad_top: f32, pad_bottom: f32) {
    let g = groups.len();
    groups.push(group);
    lines.push(Line { height: pad_top, group: Some(g), ..Line::default() });
    for mut l in body {
        l.group = Some(g);
        lines.push(l);
    }
    lines.push(Line { height: pad_bottom, group: Some(g), ..Line::default() });
}

/// Plain Markdown blocks laid out over `width`.
fn layout_blocks(cx: &mut Lay, blocks: &[Block], base: f32, width: f32) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let gap = |lines: &mut Vec<Line>, h: f32| lines.push(gap_line(h));
    for block in blocks {
        match block {
            Block::Heading { level, spans } => {
                let px = base * match level { 1 => 1.5, 2 => 1.3, _ => 1.15 };
                gap(&mut lines, base * 0.5);
                wrap_spans(cx, spans, px, Some(Style::Bold), 0.0, width, None, &mut lines);
                gap(&mut lines, base * 0.3);
            }
            Block::Paragraph(spans) => {
                wrap_spans(cx, spans, base, None, 0.0, width, None, &mut lines);
                gap(&mut lines, base * 0.5);
            }
            Block::ListItem { number, depth, spans } => {
                let indent = 56.0 + *depth as f32 * 48.0;
                let marker = number.map(|n| format!("{n}.")).unwrap_or_else(|| "\u{2022}".into());
                wrap_spans(cx, spans, base, None, indent, width, Some(marker), &mut lines);
                gap(&mut lines, base * 0.2);
            }
            Block::Quote(spans) => {
                let start = lines.len();
                wrap_spans(cx, spans, base, Some(Style::Italic), 40.0, width, None, &mut lines);
                for l in &mut lines[start..] {
                    l.bar = Some(10.0);
                }
                gap(&mut lines, base * 0.5);
            }
            Block::Code(code) => {
                let px = base * 0.8;
                for text in code {
                    let text = if text.is_empty() { " ".to_string() } else { text.replace('\t', "    ") };
                    lines.push(Line { height: px * 1.35, runs: vec![Run { x: 16.0, text, style: Style::Regular, px, link: None, muted: false }], shade: true, ..Line::default() });
                }
                gap(&mut lines, base * 0.5);
            }
            Block::Rule => {
                lines.push(Line { height: base, rule: true, ..Line::default() });
            }
            Block::Table { rows, header } => {
                // Columns as wide as their text when the table fits; otherwise each
                // column keeps its longest word and the rest of the width goes to the
                // columns with more text, and cells wrap inside their column.
                let ncol = rows.iter().map(Vec::len).max().unwrap_or(0);
                let col_gap = 32.0;
                let force = |i: usize| (*header && i == 0).then_some(Style::Bold);
                let mut natural = vec![0f32; ncol];
                let mut longest = vec![0f32; ncol];
                for (i, row) in rows.iter().enumerate() {
                    for (j, cell) in row.iter().enumerate() {
                        let w: f32 = cell.iter().map(|sp| cx.width(span_style(sp, force(i)), base, &sp.text)).sum();
                        natural[j] = natural[j].max(w);
                        for sp in cell {
                            for word in sp.text.split(' ') {
                                longest[j] = longest[j].max(cx.width(span_style(sp, force(i)), base, word));
                            }
                        }
                    }
                }
                let avail = (width - col_gap * ncol.saturating_sub(1) as f32).max(1.0);
                let fits = natural.iter().sum::<f32>() <= avail;
                let widths: Vec<f32> = if fits {
                    natural.clone()
                } else {
                    let least: f32 = longest.iter().sum();
                    if least >= avail {
                        // Even the longest words do not fit: share by them (words break).
                        longest.iter().map(|w| (w / least * avail).max(1.0)).collect()
                    } else {
                        let slack: Vec<f32> = natural.iter().zip(&longest).map(|(n, l)| (n - l).max(0.0)).collect();
                        let total: f32 = slack.iter().sum::<f32>().max(1.0);
                        longest.iter().zip(&slack).map(|(l, s)| l + (avail - least) * s / total).collect()
                    }
                };
                let xs: Vec<f32> = widths.iter().scan(0.0, |x, w| {
                    let at = *x;
                    *x += w + col_gap;
                    Some(at)
                }).collect();
                for (i, row) in rows.iter().enumerate() {
                    let cells: Vec<Vec<Line>> = (0..ncol)
                        .map(|j| {
                            let mut ls = Vec::new();
                            if let Some(cell) = row.get(j) {
                                wrap_spans(cx, cell, base, force(i), 0.0, widths[j], None, &mut ls);
                            }
                            ls
                        })
                        .collect();
                    let n = cells.iter().map(Vec::len).max().unwrap_or(0).max(1);
                    for k in 0..n {
                        let mut line = Line { height: base * 1.4, ..Line::default() };
                        for (j, ls) in cells.iter().enumerate() {
                            for r in ls.get(k).map(|l| l.runs.as_slice()).unwrap_or_default() {
                                line.runs.push(Run { x: r.x + xs[j], ..r.clone() });
                            }
                        }
                        lines.push(line);
                    }
                    if *header && i == 0 {
                        lines.push(Line { height: 8.0, rule: true, ..Line::default() });
                    } else if !fits && i + 1 < rows.len() {
                        lines.push(Line { height: 12.0, hairline: true, ..Line::default() });
                    }
                }
                gap(&mut lines, base * 0.5);
            }
        }
    }
    lines
}

impl MarkdownView {
    /// A viewer for `markdown`.
    pub fn new(title: &str, markdown: &str) -> Self {
        let mut v = MarkdownView {
            title: title.into(),
            blocks: Vec::new(),
            segs: Vec::new(),
            input: None,
            sheet: false,
            back: None,
            emphasis: false,
            page: 0,
            pages: Vec::new(),
            groups: Vec::new(),
            laid_for: None,
            links: Vec::new(),
            follow_end: false,
            dock: 0,
            draft: None,
            scroll: 0.0,
            content_h: 0.0,
            window_h: 0.0,
            anchor: None,
            widths: WordWidths::default(),
        };
        v.set_text(markdown);
        // A chat view opens at its newest message (`:::top`: at its start).
        v.follow_end = v.scrolls() && !markdown.lines().any(|l| l.trim() == ":::top");
        v
    }

    /// Replaces the text (keeps the page when possible).
    pub fn set_text(&mut self, markdown: &str) {
        let (segs, input, sheet, back) = split_directives(markdown);
        self.blocks = segs.iter().flat_map(|s| s.blocks.iter().cloned()).collect();
        self.segs = segs;
        self.input = input;
        self.sheet = sheet;
        self.back = back;
        self.emphasis = markdown.lines().any(|l| l.trim() == ":::emphasis");
        self.laid_for = None;
    }

    /// Number of pages (after the first paint).
    pub fn page_count(&self) -> usize {
        self.pages.len().max(1)
    }

    /// True for a view with an input bar (the chat view): the keyboard or a dialog of the
    /// same plugin may open on top of it.
    pub fn stacks(&self) -> bool {
        self.input.is_some()
    }

    /// Height of the sheet (`:::sheet`) holding all rows.
    pub fn sheet_height(&self) -> u32 {
        let rows = self.segs.iter().filter(|s| s.kind == BoxKind::Row).count() as u32;
        SHEET_TOP as u32 + rows * ROW_H as u32 + 32
    }

    fn side(&self) -> f32 {
        if self.sheet {
            24.0
        } else {
            MARGIN
        }
    }

    fn top(&self) -> i32 {
        if self.sheet {
            SHEET_TOP
        } else {
            APP_BAR_H as i32 + 24
        }
    }

    fn bottom_h(&self) -> i32 {
        if self.sheet {
            16
        } else if self.scrolls() {
            // No page counter: the window ends above the input bar or the docked panel.
            if self.dock > 0 { self.dock as i32 } else { INPUT_H }
        } else if self.dock > 0 {
            self.dock as i32 + COUNTER_H
        } else if self.input.is_some() {
            INPUT_H + COUNTER_H
        } else {
            FOOT_H
        }
    }

    fn bar(&self) -> AppBar {
        let (title, sub) = match self.title.split_once('\n') {
            Some((t, s)) => (t, Some(s)),
            None => (self.title.as_str(), None),
        };
        let mut bar = AppBar::new(title).action(BarAction::icon(Icon::Close, "closed"));
        if self.back.is_some() {
            bar = bar.with_back();
        }
        match sub {
            Some(s) => bar.with_subtitle(s),
            None => bar,
        }
    }

    /// Where a tap closes the view: the Close button with `space(2)` of slack.
    fn close_rect(&self, b: Rect) -> Rect {
        with_slack(self.bar().action_rects(Rect::new(b.x, b.y, b.w, APP_BAR_H)).first().copied().unwrap_or_default())
    }

    fn input_rect(b: Rect) -> Rect {
        Rect::new(b.x, b.bottom() - INPUT_H, b.w, INPUT_H as u32)
    }

    /// Docks a keyboard panel `h` px tall at the bottom (0 undocks): the pages are laid
    /// out again above it (or over the whole height) and the view shows its last page,
    /// so the newest messages stay in sight.
    fn set_dock(&mut self, h: u32) {
        if self.dock != h {
            self.dock = h;
            self.laid_for = None;
        }
        self.follow_end = true;
    }

    /// True for a chat view (a viewer with an input bar): one long page that scrolls
    /// (swipes, taps on the sides and page keys move it a whole window, starting at the
    /// first line that was not wholly shown; a scroll bar shows where it is) instead of
    /// pages. Streamed text never moves it.
    pub fn scrolls(&self) -> bool {
        self.input.is_some() && !self.sheet
    }

    /// The scroll window of a chat view for bounds `b`: below the app bar, above the
    /// input bar or the docked panel.
    fn window(&self, b: Rect) -> Rect {
        let top = b.y + self.top();
        Rect::new(b.x, top, b.w, (b.bottom() - self.bottom_h() - 16 - top).max(1) as u32)
    }

    /// Top of each line of a chat view's content (from the last layout).
    fn line_tops(&self) -> Vec<(f32, f32)> {
        let mut y = 0.0;
        self.pages.first().map_or(&[][..], |p| &p[..]).iter().map(|l| {
            let t = y;
            y += l.height;
            (t, l.height)
        }).collect()
    }

    /// The scroll offset that shows the end of a chat view: the first line that has the
    /// rest of the content fit below it.
    fn end_scroll(&self) -> f32 {
        let target = self.content_h - self.window_h;
        if target <= 0.0 {
            return 0.0;
        }
        self.line_tops().into_iter().map(|(t, _)| t).find(|t| *t >= target - 0.5).unwrap_or(target)
    }

    /// Largest scroll offset: a page turn may go past [`Self::end_scroll`] so that it
    /// moves a whole window (blank below the last line), up to the last line's top.
    fn max_scroll(&self) -> f32 {
        let last = self.line_tops().last().map_or(0.0, |(t, _)| *t);
        self.end_scroll().max(last)
    }

    /// True when the view shows its end (the last page, or the last line of a chat
    /// view): a paged view then follows the end of a replaced text.
    pub fn at_end(&self) -> bool {
        if self.scrolls() {
            self.scroll + self.window_h >= self.content_h - 0.5
        } else {
            self.page + 1 >= self.page_count()
        }
    }

    /// Where the view is (page, scroll px): a change means it is drawn again.
    fn position(&self) -> (usize, i32) {
        (self.page, self.scroll.round() as i32)
    }

    /// Moves a page (`step` pages), or for a chat view `step` windows: forward to the
    /// first line not wholly shown, back so that the window ends where it started. A
    /// turn never moves part of a window, so the eye finds where it was.
    fn turn(&mut self, step: i32) {
        self.follow_end = false;
        self.anchor = None;
        if self.scrolls() {
            let tops = self.line_tops();
            for _ in 0..step.unsigned_abs() {
                if step > 0 {
                    let edge = self.scroll + self.window_h;
                    if edge >= self.content_h - 0.5 {
                        break;
                    }
                    let next = tops.iter().find(|(t, h)| t + h > edge + 0.5).map_or(edge, |(t, _)| *t);
                    // A line taller than the window: a window further.
                    self.scroll = if next > self.scroll + 0.5 { next } else { edge };
                } else {
                    if self.scroll <= 0.5 {
                        break;
                    }
                    let target = self.scroll - self.window_h;
                    let prev = if target <= 0.0 { 0.0 } else { tops.iter().map(|(t, _)| *t).find(|t| *t >= target - 0.5).unwrap_or(target) };
                    self.scroll = if prev < self.scroll - 0.5 { prev } else { target.max(0.0) };
                }
            }
            self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        } else {
            self.page = (self.page as i32 + step).clamp(0, self.page_count() as i32 - 1) as usize;
        }
    }

    /// [`Self::turn`] for an event that is used up by it.
    fn turn_handled(&mut self, step: i32) -> EventResult {
        self.turn(step);
        EventResult::Handled
    }

    /// A thin scroll bar at the right edge of `win` (in the margin) when the content
    /// does not fit.
    fn paint_scroll_bar(&self, cx: &mut PaintCx, win: Rect) {
        if self.content_h <= self.window_h || win.h < 40 {
            return;
        }
        // The shared bar, in pixels of content; the track sits in the right margin.
        let bar = ink_ui::ScrollBar::new(
            // Past the end (a whole-window turn): blank below counts as content.
            self.content_h.max(self.scroll + self.window_h).round() as usize,
            self.scroll.round() as usize,
            self.window_h.round() as usize,
        );
        let w = ink_ui::scrollbar::SCROLLBAR_W;
        let track = Rect::new(win.right() - (MARGIN as i32 + w as i32) / 2, win.y + 8, w, win.h - 16);
        bar.paint(cx, track);
    }

    /// What changes when the pages move: below the app bar, above the docked panel (or
    /// the bottom), for bounds `b`.
    fn pages_rect(&self, b: Rect) -> Rect {
        Rect::new(b.x, b.y + APP_BAR_H as i32, b.w, b.h.saturating_sub(self.dock + APP_BAR_H))
    }

    fn layout(&mut self, cx: &mut PaintCx, b: Rect) {
        let emphasis = self.emphasis;
        let mut widths = std::mem::take(&mut self.widths);
        widths.for_fonts(crate::fonts::ui_family(), emphasis);
        body_fonts(emphasis, cx, |cx| self.layout_text(&mut Lay { cx, widths: &mut widths }, b));
        self.widths = widths;
    }

    fn layout_text(&mut self, cx: &mut Lay, b: Rect) {
        let t = cx.theme.clone();
        let base = t.font_px * 0.9;
        let cap = t.caption.px;
        let width = b.w as f32 - 2.0 * self.side();
        let page_h = (b.h as i32 - self.top() - self.bottom_h()) as f32;
        let mut lines: Vec<Line> = Vec::new();
        let mut groups: Vec<Group> = Vec::new();
        for seg in &self.segs {
            match seg.kind {
                BoxKind::Plain => lines.extend(layout_blocks(cx, &seg.blocks, base, width)),
                BoxKind::You => {
                    let inner = (width * 0.85 - 2.0 * PAD).max(100.0);
                    let mut body = layout_blocks(cx, &seg.blocks, base, inner);
                    trim_gaps(&mut body);
                    if !seg.b.is_empty() {
                        body.push(gap_line(base * 0.3));
                        body.extend(caption_lines(cx, &seg.b, cap, inner));
                    }
                    let w = extent(cx, &body).min(inner) + 2.0 * PAD;
                    let x = width - w;
                    shift(&mut body, x + PAD);
                    let link = (!seg.a.is_empty()).then(|| seg.a.clone());
                    push_box(&mut lines, &mut groups, Group { kind: seg.kind, x, w, link }, body, PAD * 0.75, PAD * 0.75);
                    lines.push(gap_line(base * 0.6));
                }
                BoxKind::Answer => {
                    let inner = width - 2.0 * PAD;
                    let mut body = Vec::new();
                    if !seg.a.is_empty() {
                        let h = cap * 1.8;
                        let ix = PAD + (h * 0.8).min(56.0) + 12.0;
                        let label = fit(cx, &seg.a, cap, Style::Bold, inner - ix);
                        body.push(Line { height: h, runs: vec![Run { x: ix, text: label, style: Style::Bold, px: cap, link: None, muted: false }], icon: Some((PAD, Icon::Chat)), ..Line::default() });
                        body.push(gap_line(base * 0.3));
                    }
                    let mut text = layout_blocks(cx, &seg.blocks, base, inner);
                    trim_gaps(&mut text);
                    shift(&mut text, PAD);
                    body.extend(text);
                    if !seg.b.is_empty() {
                        body.push(gap_line(base * 0.3));
                        let mut foot = caption_lines(cx, &seg.b, cap, inner);
                        shift(&mut foot, PAD);
                        body.extend(foot);
                    }
                    push_box(&mut lines, &mut groups, Group { kind: seg.kind, x: 0.0, w: width, link: None }, body, PAD * 0.6, PAD);
                    lines.push(gap_line(base * 0.6));
                }
                BoxKind::Action => {
                    let lead = (cap * 1.4 * 0.8).min(56.0) + 12.0;
                    let inner = width - 2.0 * PAD - lead;
                    let mut body = Vec::new();
                    wrap_spans(cx, &spans_of(&seg.blocks), cap, None, 0.0, inner, None, &mut body);
                    let w = (extent(cx, &body) + 2.0 * PAD + lead).min(width);
                    shift(&mut body, PAD + lead);
                    if let Some(f) = body.first_mut() {
                        f.icon = Some((PAD, Icon::Sync));
                    }
                    push_box(&mut lines, &mut groups, Group { kind: seg.kind, x: 0.0, w, link: None }, body, 10.0, 10.0);
                    lines.push(gap_line(16.0));
                }
                BoxKind::Chip => {
                    let px = t.label.px;
                    let hpad = 32.0;
                    for block in &seg.blocks {
                        let spans = spans_of(std::slice::from_ref(block));
                        if spans.is_empty() {
                            continue;
                        }
                        let link = spans.iter().find_map(|s| s.link.clone());
                        let spans: Vec<Span> = spans.into_iter().map(|mut s| {
                            s.link = None;
                            s
                        }).collect();
                        let mut body = Vec::new();
                        wrap_spans(cx, &spans, px, None, 0.0, width - 2.0 * hpad, None, &mut body);
                        let th: f32 = body.iter().map(|l| l.height).sum();
                        let vpad = ((t.control_h() as f32 - th) / 2.0).max(16.0);
                        let w = (extent(cx, &body) + 2.0 * hpad).min(width);
                        shift(&mut body, hpad);
                        push_box(&mut lines, &mut groups, Group { kind: seg.kind, x: 0.0, w, link }, body, vpad, vpad);
                        lines.push(gap_line(16.0));
                    }
                    lines.push(gap_line(base * 0.4));
                }
                BoxKind::Card => {
                    let inner = width - 2.0 * PAD;
                    let mut body = if seg.b.is_empty() { Vec::new() } else { caption_lines(cx, &seg.b, cap, inner) };
                    body.extend(layout_blocks(cx, &seg.blocks, base, inner));
                    trim_gaps(&mut body);
                    shift(&mut body, PAD);
                    let th: f32 = body.iter().map(|l| l.height).sum();
                    let vpad = ((t.control_h() as f32 - th) / 2.0).max(PAD);
                    let link = (!seg.a.is_empty()).then(|| seg.a.clone());
                    push_box(&mut lines, &mut groups, Group { kind: seg.kind, x: 0.0, w: width, link }, body, vpad, vpad);
                    lines.push(gap_line(16.0));
                }
                BoxKind::Row => {
                    let px = t.body.px;
                    let icon = icon_named(&seg.b);
                    let tx = if icon.is_some() { PAD + 56.0 + 32.0 } else { PAD };
                    let label = markdown::to_plain(&seg.blocks).split_whitespace().collect::<Vec<_>>().join(" ");
                    let label = fit(cx, &label, px, Style::Regular, width - tx - PAD);
                    let line = Line { height: ROW_H, runs: vec![Run { x: tx, text: label, style: Style::Regular, px, link: None, muted: false }], icon: icon.map(|i| (PAD, i)), ..Line::default() };
                    let link = (!seg.a.is_empty()).then(|| seg.a.clone());
                    push_box(&mut lines, &mut groups, Group { kind: seg.kind, x: 0.0, w: width, link }, vec![line], 0.0, 0.0);
                }
            }
        }
        if self.scrolls() {
            // One long page: the window shows part of it.
            self.content_h = lines.iter().map(|l| l.height).sum();
            self.window_h = self.window(b).h as f32;
            self.pages = vec![lines];
            self.groups = groups;
            self.page = 0;
            // A question just sent goes to the top; opening or docking shows the end.
            // Both happen once: streamed text that grows later leaves the window where
            // it is.
            if let Some(n) = self.anchor.take() {
                let mut y = 0.0;
                let mut seen = 0;
                let mut last_group = None;
                for l in &self.pages[0] {
                    if l.group != last_group {
                        last_group = l.group;
                        if l.group.is_some_and(|g| self.groups[g].kind == BoxKind::You) {
                            if seen == n {
                                self.scroll = y;
                                break;
                            }
                            seen += 1;
                        }
                    }
                    y += l.height;
                }
                self.follow_end = false;
            } else if self.follow_end {
                self.scroll = self.end_scroll();
                self.follow_end = false;
            }
            self.scroll = self.scroll.clamp(0.0, self.max_scroll());
            self.laid_for = Some((b.w, b.h));
            return;
        }
        // Pages; a box that fits on a page is not split.
        let mut box_h = vec![0f32; groups.len()];
        for l in &lines {
            if let Some(g) = l.group {
                box_h[g] += l.height;
            }
        }
        let mut seen = vec![false; groups.len()];
        let mut pages: Vec<Vec<Line>> = vec![Vec::new()];
        let mut y = 0.0;
        for l in lines {
            let mut need = l.height;
            if let Some(g) = l.group {
                if !seen[g] {
                    seen[g] = true;
                    if box_h[g] <= page_h {
                        need = box_h[g];
                    }
                }
            }
            if y + need > page_h && !pages.last().is_some_and(|p| p.is_empty()) {
                pages.push(Vec::new());
                y = 0.0;
                if l.runs.is_empty() && !l.rule && l.group.is_none() {
                    continue; // no blank gap at the top of a page
                }
            }
            y += l.height;
            if let Some(p) = pages.last_mut() {
                p.push(l);
            }
        }
        self.pages = pages;
        self.groups = groups;
        if self.follow_end {
            self.page = self.pages.len() - 1;
        }
        self.page = self.page.min(self.pages.len() - 1);
        self.laid_for = Some((b.w, b.h));
    }
}

/// `text` as muted caption lines (`px`) wrapped to `width`.
fn caption_lines(cx: &mut Lay, text: &str, px: f32, width: f32) -> Vec<Line> {
    let mut lines = Vec::new();
    wrap_spans(cx, &[Span::plain(text)], px, Some(Style::Regular), 0.0, width, None, &mut lines);
    for r in lines.iter_mut().flat_map(|l| &mut l.runs) {
        r.muted = true;
    }
    lines
}

/// `r` grown by `space(2)` on every side: where a tap still hits an app bar button.
fn with_slack(r: Rect) -> Rect {
    let s = ink_ui::theme::space(2);
    Rect::new(r.x - s, r.y - s, r.w + 2 * s as u32, r.h + 2 * s as u32)
}

/// The face for a span (`force` overrides it: headings, table headers, quotes).
fn span_style(s: &Span, force: Option<Style>) -> Style {
    match force {
        Some(f) => f,
        None if s.style.bold => Style::Bold,
        None if s.style.italic => Style::Italic,
        None => Style::Regular,
    }
}

/// Greedy line breaking of styled spans into `lines`.
#[allow(clippy::too_many_arguments)]
fn wrap_spans(cx: &mut Lay, spans: &[Span], px: f32, force: Option<Style>, indent: f32, width: f32, marker: Option<String>, lines: &mut Vec<Line>) {
    let style_of = |s: &Span| span_style(s, force);
    // Words with their style; a word may span several spans (no space between).
    let mut words: Vec<Vec<(String, Style, Option<String>)>> = vec![Vec::new()];
    for s in spans {
        let st = style_of(s);
        for (i, part) in s.text.split(' ').enumerate() {
            if i > 0 {
                words.push(Vec::new());
            }
            if !part.is_empty() {
                if let Some(w) = words.last_mut() {
                    w.push((part.to_string(), st, s.link.clone()));
                }
            }
        }
    }
    words.retain(|w| !w.is_empty());
    let height = px * 1.4;
    let space = cx.width(Style::Regular, px, " ");
    let mut line = Line { height, ..Line::default() };
    if let Some(m) = marker {
        let mw = cx.width(Style::Regular, px, &m);
        line.runs.push(Run { x: (indent - mw - 14.0).max(0.0), text: m, style: Style::Regular, px, link: None, muted: false });
    }
    let mut x = indent;
    let mut first = true;
    for word in words {
        // Each part is shaped once here; its width is reused below.
        let widths: Vec<f32> = word.iter().map(|(t, st, _)| cx.width(*st, px, t)).collect();
        let ww: f32 = widths.iter().sum();
        if !first && x + space + ww > width {
            lines.push(std::mem::replace(&mut line, Line { height, ..Line::default() }));
            x = indent;
            first = true;
        }
        if !first {
            x += space;
        }
        for ((mut t, st, link), mut w) in word.into_iter().zip(widths) {
            // Hard-break words wider than the line: the longest head that fits the rest
            // of the line (at least one character) goes on it.
            while w > width - indent && t.chars().count() > 1 {
                let ends: Vec<usize> = t.char_indices().map(|(i, _)| i).chain([t.len()]).collect();
                let k = last_fitting(ends.len() - 1, |k| cx.fonts.shaped_width(st, px, &t[..ends[k]]) <= width - x).max(1);
                let rest = t.split_off(ends[k]);
                line.runs.push(Run { x, text: t, style: st, px, link: link.clone(), muted: false });
                lines.push(std::mem::replace(&mut line, Line { height, ..Line::default() }));
                x = indent;
                t = rest;
                w = cx.fonts.shaped_width(st, px, &t);
            }
            line.runs.push(Run { x, text: t, style: st, px, link: link.clone(), muted: false });
            x += w;
        }
        first = false;
    }
    if !line.runs.is_empty() {
        lines.push(line);
    }
}

impl MarkdownView {
    /// Draws the current page's boxes and lines from `top`, lines starting at `x0` and
    /// `cw` wide; collects the link rectangles.
    fn paint_content(&mut self, cx: &mut PaintCx, x0: f32, top: f32, cw: f32) {
        let t = cx.theme.clone();
        let (fg, muted) = (t.fg, t.muted);
        let page: &[Line] = self.pages.get(self.page).map_or(&[], |p| p);
        let mut boxes: Vec<(usize, f32, f32)> = Vec::new();
        let mut y = top;
        for l in page {
            if let Some(g) = l.group {
                match boxes.last_mut() {
                    Some(bx) if bx.0 == g => bx.2 = y + l.height,
                    _ => boxes.push((g, y, y + l.height)),
                }
            }
            y += l.height;
        }
        let mut box_links = Vec::new();
        for (i, &(g, y0, y1)) in boxes.iter().enumerate() {
            let gr = &self.groups[g];
            let r = Rect::new((x0 + gr.x).round() as i32, y0.round() as i32, gr.w.round() as u32, (y1 - y0).round().max(1.0) as u32);
            let round = (r.h as f32 / 2.0).min(52.0);
            match gr.kind {
                BoxKind::You => {
                    fill_round_rect(cx.canvas, r, t.radius_lg, t.light);
                    stroke_round_rect(cx.canvas, r, t.radius_lg, t.border_px as f32, fg);
                }
                BoxKind::Answer | BoxKind::Card => stroke_round_rect(cx.canvas, r, t.radius_lg, t.border_px as f32, fg),
                BoxKind::Action => {
                    let rad = (r.h as f32 / 2.0).min(t.radius_lg);
                    fill_round_rect(cx.canvas, r, rad, t.light);
                    stroke_round_rect(cx.canvas, r, rad, 2.0, fg);
                }
                BoxKind::Chip => stroke_round_rect(cx.canvas, r, round, t.border_px as f32, fg),
                BoxKind::Row => {
                    let next_row = boxes.get(i + 1).is_some_and(|n| self.groups[n.0].kind == BoxKind::Row);
                    if next_row {
                        cx.canvas.fill_rect(r.x, r.bottom() - t.rule_px, r.w as i32, t.rule_px, t.light);
                    }
                }
                BoxKind::Plain => {}
            }
            if let Some(link) = &gr.link {
                box_links.push((r, link.clone()));
            }
        }
        let mut y = top;
        let canvas_h = cx.canvas.height as f32;
        let mut links = Vec::new();
        for line in page {
            // Lines outside the canvas (a scrolled chat view) are not drawn.
            if y + line.height < 0.0 || y > canvas_h {
                y += line.height;
                continue;
            }
            let (lx, lw) = match line.group.and_then(|g| self.groups.get(g)) {
                Some(gr) if gr.kind != BoxKind::Plain => (x0 + gr.x + PAD, gr.w - 2.0 * PAD),
                _ => (x0, cw),
            };
            if line.shade {
                cx.canvas.fill_rect(lx as i32, y as i32, lw as i32, line.height.ceil() as i32, t.light);
            }
            if let Some(bx) = line.bar {
                cx.canvas.fill_rect((x0 + bx) as i32, y as i32, 6, line.height.ceil() as i32, muted);
            }
            if line.rule {
                cx.canvas.fill_rect(lx as i32, (y + line.height / 2.0) as i32, lw as i32, 2, fg);
            }
            if line.hairline {
                cx.canvas.fill_rect(lx as i32, (y + line.height / 2.0) as i32, lw as i32, 1, muted);
            }
            if let Some((ix, icon)) = line.icon {
                let side = (line.height * 0.8).min(56.0) as u32;
                let r = Rect::new((x0 + ix) as i32, (y + (line.height - side as f32) / 2.0) as i32, side, side);
                cx.icon(icon, r, side, fg);
            }
            for r in &line.runs {
                let (asc, desc) = cx.fonts.vmetrics(r.style, r.px);
                let base = y + (line.height - (asc - desc)) / 2.0 + asc;
                cx.draw_text(r.style, r.px, x0 + r.x, base, &r.text, if r.muted { muted } else { fg });
                if let Some(url) = &r.link {
                    let w = cx.fonts.shaped_width(r.style, r.px, &r.text);
                    cx.canvas.fill_rect((x0 + r.x) as i32, (base + 6.0) as i32, w.ceil() as i32, 2, fg);
                    links.push((Rect::new((x0 + r.x) as i32, y as i32, w.ceil() as u32, line.height.ceil() as u32), url.clone()));
                }
            }
            y += line.height;
        }
        // Inline links win over the box around them.
        self.links.extend(links);
        self.links.extend(box_links);
    }
}

impl Widget for MarkdownView {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        if self.laid_for != Some((b.w, b.h)) {
            self.layout(cx, b);
        }
        let t = cx.theme.clone();
        let (fg, muted) = (t.fg, t.muted);
        if self.sheet {
            BottomSheet { title: self.title.clone(), taken: false }.paint(cx, b);
        } else {
            let px = t.title.px;
            let mut bar = self.bar();
            bar.title = fit(cx, &bar.title, px, Style::Bold, b.w as f32 - 2.0 * MARGIN - 160.0 - if self.back.is_some() { 120.0 } else { 0.0 });
            bar.paint(cx, Rect::new(b.x, b.y, b.w, APP_BAR_H));
        }
        // Content: boxes first, then the lines.
        self.links.clear();
        let x0 = b.x as f32 + self.side();
        let cw = b.w as f32 - 2.0 * self.side();
        let top = (b.y + self.top()) as f32;
        if self.scrolls() {
            // One long page drawn through a window: lines cut at its edges, the links
            // that show, and a scroll bar when it does not all fit.
            let win = self.window(b);
            let mut tmp = Canvas::new(win.w as usize, win.h as usize);
            tmp.clear(t.bg);
            {
                let mut tcx = PaintCx { canvas: &mut tmp, fonts: &mut *cx.fonts, theme: cx.theme };
                let emphasis = self.emphasis;
                body_fonts(emphasis, &mut tcx, |cx| self.paint_content(cx, x0 - win.x as f32, -self.scroll, cw));
            }
            cx.canvas.blit(&tmp, win.x, win.y);
            self.links = std::mem::take(&mut self.links)
                .into_iter()
                .filter_map(|(r, l)| {
                    let r = Rect::new(r.x + win.x, r.y + win.y, r.w, r.h).intersect(&win);
                    (!r.is_empty()).then_some((r, l))
                })
                .collect();
            self.paint_scroll_bar(cx, win);
        } else {
            let emphasis = self.emphasis;
            body_fonts(emphasis, cx, |cx| self.paint_content(cx, x0, top, cw));
        }
        // Footer: the input bar, or the page counter.
        let label = format!("{} / {}", self.page + 1, self.page_count());
        if self.sheet {
        } else if self.dock > 0 {
            // The docked panel draws the input bar; the page counter sits above it.
            if self.page_count() > 1 {
                let band = Rect::new(b.x, b.bottom() - self.dock as i32 - COUNTER_H, b.w, COUNTER_H as u32);
                cx.text_centered(band, &label, t.caption.px, Style::Regular, fg);
            }
        } else if let Some((_, placeholder)) = &self.input {
            let ib = Self::input_rect(b);
            if self.page_count() > 1 {
                let band = Rect::new(b.x, ib.y - COUNTER_H, b.w, COUNTER_H as u32);
                cx.text_centered(band, &label, t.caption.px, Style::Regular, fg);
            }
            // An unsent draft shows instead of the placeholder.
            let (text, color) = match self.draft.as_deref().filter(|d| !d.trim().is_empty()) {
                Some(d) => (d, fg),
                None => (placeholder.as_str(), muted),
            };
            paint_input_bar(cx, ib, text, color, false);
        } else {
            let foot = Rect::new(b.x, b.bottom() - FOOT_H, b.w, FOOT_H as u32);
            cx.text_centered(foot, &label, cx.theme.small_px, Style::Regular, fg);
        }
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        // A held link reports `hold:LINK` (a chat list deletes a chat that way).
        if let UiEvent::Gesture(Gesture::LongPress { pos, .. }) = ev {
            return match self.links.iter().find(|(r, _)| r.contains(*pos)) {
                Some((_, url)) => EventResult::Action(format!("hold:{url}")),
                None => EventResult::Handled,
            };
        }
        if self.sheet {
            return match ev {
                UiEvent::Gesture(Gesture::Tap { pos }) => match self.links.iter().find(|(r, _)| r.contains(*pos)) {
                    Some((_, url)) => EventResult::Action(url.clone()),
                    None => EventResult::Handled,
                },
                UiEvent::Gesture(Gesture::Swipe { dir: Direction::Down, .. }) => EventResult::Action("closed".into()),
                _ => EventResult::Handled,
            };
        }
        match ev {
            UiEvent::Gesture(Gesture::Tap { pos }) => {
                if self.close_rect(b).contains(*pos) {
                    return EventResult::Action("closed".into());
                }
                if let Some(link) = &self.back {
                    if with_slack(AppBar::back_rect(Rect::new(b.x, b.y, b.w, APP_BAR_H))).contains(*pos) {
                        return EventResult::Action(link.clone());
                    }
                }
                if let Some((link, _)) = &self.input {
                    if Self::input_rect(b).contains(*pos) {
                        return EventResult::Action(link.clone());
                    }
                }
                if let Some((_, url)) = self.links.iter().find(|(r, _)| r.contains(*pos)) {
                    return EventResult::Action(url.clone());
                }
                if pos.y > b.y + APP_BAR_H as i32 {
                    self.turn(if pos.x < b.x + b.w as i32 / 3 { -1 } else { 1 });
                }
                EventResult::Handled
            }
            UiEvent::Gesture(Gesture::Swipe { dir: Direction::Left | Direction::Up, .. }) => self.turn_handled(1),
            UiEvent::Gesture(Gesture::Swipe { dir: Direction::Down | Direction::Right, .. }) => self.turn_handled(-1),
            UiEvent::Key(k) if k.pressed && k.key == ink_device::Key::PageNext => self.turn_handled(1),
            UiEvent::Key(k) if k.pressed && k.key == ink_device::Key::PagePrev => self.turn_handled(-1),
            _ => EventResult::Handled,
        }
    }
}

/// Field and send button of an input bar `bar` (the chat view's, and the docked
/// keyboard panel's top row): a pill field and a round send button, `ctl` tall,
/// centred below the rule at the bar's top edge.
fn input_bar_rects(bar: Rect, ctl: u32, rule_px: i32) -> (Rect, Rect) {
    let cy = bar.y + rule_px + (bar.h as i32 - rule_px - ctl as i32) / 2;
    let send = Rect::new(bar.right() - 32 - ctl as i32, cy, ctl, ctl);
    let field = Rect::new(bar.x + 32, cy, (send.x - 16 - (bar.x + 32)).max(0) as u32, ctl);
    (field, send)
}

/// Paints an input bar: the rule on top, the field with `text` in `color` (the end of a
/// long text and the cursor bar after it when `cursor`; else cut at the end)
/// and the send button.
fn paint_input_bar(cx: &mut PaintCx, bar: Rect, text: &str, color: u8, cursor: bool) {
    let t = cx.theme.clone();
    cx.canvas.fill_rect(bar.x, bar.y, bar.w as i32, t.rule_px, t.fg);
    let ctl = t.control_h();
    let (field, send) = input_bar_rects(bar, ctl, t.rule_px);
    stroke_round_rect(cx.canvas, field, ctl as f32 / 2.0, t.border_px as f32, t.fg);
    let px = t.label.px;
    let room = field.w as f32 - 80.0;
    let tr = Rect::new(field.x + 40, field.y, field.w.saturating_sub(80), field.h);
    if cursor {
        let shown = fit_end(cx, text, px, room - 8.0);
        let w = cx.fonts.text_width(Style::Regular, shown, px);
        cx.text_left(tr, shown, px, Style::Regular, color);
        cx.canvas.fill_rect(tr.x + w.ceil() as i32 + 2, field.y + 20, 3, field.h as i32 - 40, t.fg);
    } else {
        let shown = fit(cx, text, px, Style::Regular, room);
        cx.text_left(tr, &shown, px, Style::Regular, color);
    }
    paint_button(cx, send, "", Some(Icon::Forward), ButtonKind::Filled, true, false);
}

/// Keyboard input panel at the bottom: prompt, text field, Cancel/OK and the ink-ui
/// [`Keyboard`]. Docked under a chat view ([`InputBox::docked`]) it is the view's input
/// bar lifted above the keyboard instead: field and send button on top of the keys.
pub struct InputBox {
    /// Prompt.
    pub prompt: String,
    /// Text typed so far.
    pub text: String,
    /// The keyboard widget.
    pub keyboard: Keyboard,
    /// Panel-local rectangles changed since the last paint (`None`: all of it).
    damage: Option<Vec<Rect>>,
    /// Docked under a chat view: no prompt and no Cancel/OK; the top row is the view's
    /// input bar (the prompt as its placeholder, a send button), lifted above the keys.
    pub docked: bool,
    /// Control height and rule width of the last paint (the docked bar's field and send
    /// button).
    ctl: u32,
    rule: i32,
}

const INPUT_TOP_H: u32 = 300;

impl InputBox {
    /// A panel for `prompt` with `text` typed and a keyboard of `layouts`.
    pub fn new(prompt: String, text: String, layouts: &[String]) -> Self {
        InputBox { prompt, text, keyboard: Keyboard::with_language_list(layouts), damage: Some(Vec::new()), docked: false, ctl: 96, rule: 2 }
    }

    /// The docked panel of a chat view's input bar: `prompt` is the placeholder.
    pub fn docked(prompt: String, text: String, layouts: &[String]) -> Self {
        InputBox { docked: true, ..InputBox::new(prompt, text, layouts) }
    }

    /// Field, Cancel, OK and keyboard rectangles. The keyboard is full width with its
    /// preferred height and ends `BOTTOM_MARGIN` px above the panel bottom (blank).
    fn rects(b: Rect) -> (Rect, Rect, Rect, Rect) {
        let field = Rect::new(b.x + 32, b.y + 76, b.w - 64, 100);
        let cancel = Rect::new(b.x + 32, b.y + 188, 280, 104);
        let ok = Rect::new(b.right() - 32 - 280, b.y + 188, 280, 104);
        let kb = Rect::new(b.x, b.y + INPUT_TOP_H as i32, b.w, Keyboard::preferred_height(4));
        (field, cancel, ok, kb)
    }

    /// [`InputBox::rects`] of this panel. Docked: the field, no Cancel, the send button
    /// as OK, and the keyboard with the input bar lifted onto its top edge
    /// ([`ink_ui::keyboard::lift_above`]).
    fn parts(&self, b: Rect) -> (Rect, Rect, Rect, Rect) {
        if !self.docked {
            return Self::rects(b);
        }
        let kb = Rect::new(b.x, b.bottom() - (Keyboard::preferred_height(4) + ink_ui::keyboard::BOTTOM_MARGIN) as i32, b.w, Keyboard::preferred_height(4));
        let bar = ink_ui::keyboard::lift_above(Rect::new(b.x, b.y, b.w, INPUT_H as u32), kb, 0);
        let (field, send) = input_bar_rects(bar, self.ctl, self.rule);
        (field, Rect::default(), send, kb)
    }

    /// Height of the panel.
    pub fn height() -> u32 {
        INPUT_TOP_H + Keyboard::preferred_height(4) + ink_ui::keyboard::BOTTOM_MARGIN
    }

    /// Height of the docked panel: the input bar and the keyboard.
    pub fn docked_height() -> u32 {
        INPUT_H as u32 + Keyboard::preferred_height(4) + ink_ui::keyboard::BOTTOM_MARGIN
    }

    /// Takes what changed since the last call: `None` for the whole panel.
    fn take_damage(&mut self) -> Option<Vec<Rect>> {
        self.damage.replace(Vec::new())
    }
}

impl Widget for InputBox {
    fn paint(&mut self, cx: &mut PaintCx, b: Rect) {
        let fg = cx.theme.fg;
        if self.docked {
            (self.ctl, self.rule) = (cx.theme.control_h(), cx.theme.rule_px);
            let (_, _, _, kb) = self.parts(b);
            let bar = Rect::new(b.x, b.y, b.w, (kb.y - b.y).max(0) as u32);
            let (text, color) = if self.text.is_empty() { (self.prompt.as_str(), cx.theme.muted) } else { (self.text.as_str(), fg) };
            // The placeholder is cut at its end; typed text shows its end and the cursor.
            paint_input_bar(cx, bar, text, color, !self.text.is_empty());
            if self.text.is_empty() {
                let (field, _, _, _) = self.parts(b);
                cx.canvas.fill_rect(field.x + 38, field.y + 20, 3, field.h as i32 - 40, fg);
            }
            self.keyboard.paint(cx, kb);
            return;
        }
        cx.canvas.fill_rect(b.x, b.y, b.w as i32, cx.theme.border_px, fg);
        let (field, cancel, ok, kb) = Self::rects(b);
        let px = cx.theme.font_px;
        let heading = cx.theme.heading;
        let prompt = fit(cx, &self.prompt, heading.px, heading.style, b.w as f32 - 64.0);
        cx.text_left(Rect::new(b.x + 32, b.y + 8, b.w - 64, 68), &prompt, heading.px, heading.style, fg);
        ink_ui::draw::stroke_round_rect(cx.canvas, field, cx.theme.radius, cx.theme.border_px as f32, fg);
        // Show the end of long text, with a cursor bar.
        let shown = fit_end(cx, &self.text, px, field.w as f32 - 40.0);
        let (asc, desc) = cx.fonts.vmetrics(Style::Regular, px);
        let base = field.y as f32 + (field.h as f32 - (asc - desc)) / 2.0 + asc;
        let w = cx.fonts.text_width(Style::Regular, shown, px);
        cx.draw_text(Style::Regular, px, (field.x + 16) as f32, base, shown, fg);
        cx.canvas.fill_rect(field.x + 18 + w as i32, field.y + 18, 3, field.h as i32 - 36, fg);
        paint_button(cx, cancel, "Cancel", None, ButtonKind::Outlined, true, false);
        paint_button(cx, ok, "OK", None, ButtonKind::Filled, true, false);
        self.keyboard.paint(cx, kb);
    }

    fn event(&mut self, ev: &UiEvent, b: Rect) -> EventResult {
        let (_, cancel, ok, kb) = self.parts(b);
        match ev {
            UiEvent::Gesture(Gesture::Tap { pos }) if ok.contains(*pos) => EventResult::Action("input:submit".into()),
            UiEvent::Gesture(Gesture::Tap { pos }) if cancel.contains(*pos) => EventResult::Action("input:cancel".into()),
            // The ink-ui keyboard types on per-contact touches (down/up, hold for repeat
            // and alternates), not on tap gestures (wave-a/ui); a tap on it is used up.
            UiEvent::Touch(t) if kb.contains(t.start) => {
                let r = self.keyboard.event(ev, kb);
                // Pressed and released keys: only their faces; a new layout: everything.
                match (self.keyboard.damage(kb), self.damage.as_mut()) {
                    (Some(keys), Some(d)) => d.extend(keys),
                    _ => self.damage = None,
                }
                let mut out = match r {
                    EventResult::Ignored => EventResult::Ignored,
                    // Press feedback, key preview, popups: repaint the panel.
                    _ => EventResult::Action("input:keys".into()),
                };
                for action in self.keyboard.take_actions() {
                    match action {
                        KeyAction::Char(c) => {
                            self.text.push_str(&c);
                            out = EventResult::Action("input:edit".into());
                        }
                        KeyAction::Space => {
                            self.text.push(' ');
                            out = EventResult::Action("input:edit".into());
                        }
                        KeyAction::Backspace => {
                            self.text.pop();
                            out = EventResult::Action("input:edit".into());
                        }
                        KeyAction::Enter => return EventResult::Action("input:submit".into()),
                        KeyAction::Close => return EventResult::Action("input:cancel".into()),
                        _ => {}
                    }
                }
                out
            }
            _ => EventResult::Handled,
        }
    }

    fn refresh_class(&self) -> RefreshClass {
        RefreshClass::Gl16
    }
}

/// What a layer shows.
enum Kind {
    Toast(Toast),
    Dialog(Dialog),
    Card(CardDialog),
    Menu(MenuBox),
    Viewer(MarkdownView),
    Input(InputBox),
}

impl Kind {
    fn widget(&mut self) -> &mut dyn Widget {
        match self {
            Kind::Toast(w) => w,
            Kind::Dialog(w) => w,
            Kind::Card(w) => w,
            Kind::Menu(w) => w,
            Kind::Viewer(w) => w,
            Kind::Input(w) => w,
        }
    }

    fn modal(&self) -> bool {
        !matches!(self, Kind::Toast(_))
    }

    /// Answer when the user backs out.
    fn cancel_result(&self) -> &'static str {
        match self {
            Kind::Viewer(_) => "closed",
            _ => "",
        }
    }

    /// Where a scrolling layer is (a viewer's page and scroll, a menu's scroll): a
    /// change after an event means it is drawn again.
    fn position(&self) -> (usize, i32) {
        match self {
            Kind::Viewer(v) => v.position(),
            Kind::Menu(m) => (m.scroll() as usize, 0),
            _ => (0, 0),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Kind::Toast(_) => "toast",
            Kind::Dialog(_) | Kind::Card(_) => "dialog",
            Kind::Menu(_) => "menu",
            Kind::Viewer(_) => "viewer",
            Kind::Input(_) => "input",
        }
    }
}

/// One overlay layer with the pixels it covers.
struct Layer {
    plugin: String,
    id: Option<u32>,
    rect: Rect,
    backing: Option<Canvas>,
    dirty: Option<RefreshClass>,
    /// Only these rectangles (layer-local) changed: the layer is drawn again but only
    /// they are refreshed (keyboard key faces, the text field). `None`: the whole layer.
    partial: Option<Vec<Rect>>,
    /// The content was replaced in place (streamed text): the canvas under the layer
    /// still holds what the panel shows of it, so only the pixels that differ are
    /// refreshed. Cleared when anything else repaints the layer's area.
    stream: bool,
    kind: Kind,
    expires: Option<Instant>,
}

impl Layer {
    /// True when the whole layer is drawn and refreshed at the next paint already.
    fn whole_pending(&self) -> bool {
        self.dirty.is_some() && self.partial.is_none()
    }

    /// `class`, or the stronger class already pending.
    fn at_least(&self, class: RefreshClass) -> RefreshClass {
        self.dirty.map_or(class, |c| c.max(class))
    }

    /// Draws the whole layer again at the next paint and refreshes all of it with `class`.
    fn redraw(&mut self, class: RefreshClass) {
        self.dirty = Some(class);
        self.partial = None;
    }

    /// Adds layer-local `rects` to what the next paint refreshes, unless all of the
    /// layer is refreshed then anyway. The caller sets the class.
    fn refresh_parts(&mut self, rects: impl IntoIterator<Item = Rect>) {
        if !self.whole_pending() {
            self.partial.get_or_insert_with(Vec::new).extend(rects);
        }
    }
}

/// Pixels of a closed layer waiting to be put back: rect, pixels, refresh class.
type Restore = (Rect, Canvas, RefreshClass);

/// Copies the pixels of `changed` (rectangles the screen repainted) that lie in `r` from
/// `canvas` into `backing` (the saved pixels of `r`). Returns true when any did.
fn patch_backing(canvas: &Canvas, r: Rect, backing: &mut Canvas, changed: &[Rect]) -> bool {
    let r = r.intersect(&canvas.bounds());
    if r.is_empty() || (backing.width, backing.height) != (r.w as usize, r.h as usize) {
        return false;
    }
    let mut any = false;
    for c in changed {
        let i = r.intersect(c);
        if i.is_empty() {
            continue;
        }
        backing.blit(&canvas.crop(i), i.x - r.x, i.y - r.y);
        any = true;
    }
    any
}

/// Requests kept while no screen paints the overlay.
const MAX_PENDING: usize = 64;

/// Presents plugin UI requests as overlay layers (a stack; at most one modal layer plus
/// a toast on top). Each layer keeps a copy of the pixels under it and puts them back
/// when it closes. Painting goes through an off-screen canvas of the layer's size, so a
/// widget never draws outside its layer.
pub struct PluginUi {
    pending: VecDeque<(String, UiRequest)>,
    layers: Vec<Layer>,
    restore: Vec<Restore>,
    /// How long toasts stay.
    pub toast_time: Duration,
    /// How long notifications stay on screen.
    pub notification_time: Duration,
    /// Keyboard languages of the input panel (`ui.keyboard_layouts`; first = default).
    pub keyboard_layouts: Vec<String>,
}

impl Default for PluginUi {
    fn default() -> Self {
        PluginUi {
            pending: VecDeque::new(),
            layers: Vec::new(),
            restore: Vec::new(),
            toast_time: Duration::from_millis(2500),
            notification_time: Duration::from_secs(4),
            keyboard_layouts: vec!["en".into()],
        }
    }
}

/// True for a request that opens a modal layer (not a toast, a notification or a
/// status text).
fn opens_modal(req: &UiRequest) -> bool {
    !matches!(req, UiRequest::Toast { .. } | UiRequest::Notification { .. } | UiRequest::Status { .. })
}

/// `ev` with positions relative to `r`'s origin.
fn local(ev: &UiEvent, r: Rect) -> UiEvent {
    let m = |p: Point| Point::new(p.x - r.x, p.y - r.y);
    match ev {
        UiEvent::Gesture(g) => UiEvent::Gesture(match *g {
            Gesture::Tap { pos } => Gesture::Tap { pos: m(pos) },
            Gesture::DoubleTap { pos } => Gesture::DoubleTap { pos: m(pos) },
            Gesture::LongPress { pos, held } => Gesture::LongPress { pos: m(pos), held },
            Gesture::Swipe { from, to, dir } => Gesture::Swipe { from: m(from), to: m(to), dir },
            Gesture::TwoFingerTap { pos } => Gesture::TwoFingerTap { pos: m(pos) },
            Gesture::TwoFingerSwipe { from, to, dir } => Gesture::TwoFingerSwipe { from: m(from), to: m(to), dir },
            Gesture::Pinch { center, scale_milli } => Gesture::Pinch { center: m(center), scale_milli },
            Gesture::CornerHold { corner, pos } => Gesture::CornerHold { corner, pos: m(pos) },
        }),
        // Per-contact notifications (wave-a/ui) carry positions too.
        UiEvent::Touch(t) => UiEvent::Touch(ink_ui::Touch { pos: m(t.pos), start: m(t.start), ..*t }),
        other => other.clone(),
    }
}

fn gesture_pos(ev: &UiEvent) -> Option<Point> {
    match ev {
        UiEvent::Gesture(g) => ink_ui::widget::gesture_pos(g),
        _ => None,
    }
}

impl PluginUi {
    /// Queues a request of `plugin`. A viewer request with the id of the plugin's open
    /// viewer updates it in place.
    pub fn push(&mut self, plugin: &str, req: UiRequest) {
        if let UiRequest::MenuLabel { id, action, label } = req {
            self.menu_label(plugin, id, &action, &label);
            return;
        }
        if let UiRequest::TextViewer { id, title, markdown } = &req {
            if let Some(l) = self.layers.iter_mut().find(|l| l.plugin == plugin && l.id == Some(*id)) {
                if let Kind::Viewer(v) = &mut l.kind {
                    let top = markdown.lines().any(|l| l.trim() == ":::top");
                    let at_end = v.at_end();
                    let yous = |v: &MarkdownView| v.segs.iter().filter(|s| s.kind == BoxKind::You).count();
                    let before = yous(v);
                    v.title = title.clone();
                    v.set_text(markdown);
                    if !v.scrolls() {
                        v.follow_end = at_end && !top;
                    } else if !top && yous(v) > before {
                        // A new question: shown at the top, its answer streams in below.
                        v.anchor = Some(yous(v) - 1);
                    }
                    // Otherwise a chat view stays where it is (a pending follow of a
                    // dock change stays too).
                    if top {
                        v.page = 0;
                        v.scroll = 0.0;
                    }
                    // Only the changed lines are refreshed (paint: `stream`), so streamed
                    // text never flashes. A pending REAGL (the view docking or undocking
                    // the keyboard) stays.
                    l.redraw(l.at_least(RefreshClass::Gl16));
                    l.stream = true;
                    return;
                }
            }
            // The same viewer still waiting to open (a chat shown, then shown again as its
            // request starts, before a paint): the waiting one takes the new text. A second
            // copy would open under the first, and closing the chat would need two taps.
            let waiting = self.pending.iter_mut().find_map(|(p, r)| match r {
                UiRequest::TextViewer { id: i, title, markdown } if p == plugin && i == id => Some((title, markdown)),
                _ => None,
            });
            if let Some((t, m)) = waiting {
                (*t, *m) = (title.clone(), markdown.clone());
                return;
            }
        }
        if let UiRequest::Dialog { id, title, body, buttons } = &req {
            // The same dialog again (repeated taps on what opens it): one copy only. Each
            // copy was modal; a stack of them, shown one after another long after their
            // chat closed, swallowed every tap and looked like a frozen reader.
            let open = self.layers.iter().any(|l| {
                l.plugin == plugin
                    && l.id == Some(*id)
                    && match &l.kind {
                        Kind::Dialog(d) => d.title == *title && d.body == *body && d.buttons == *buttons,
                        _ => false,
                    }
            });
            let waiting = self.pending.iter().any(|(p, r)| p == plugin && r == &req);
            if open || waiting {
                log::info!("plugins: {plugin}: dialog {id} {title:?} is already {}; the repeat is dropped", if open { "open" } else { "waiting" });
                return;
            }
        }
        self.pending.push_back((plugin.to_string(), req));
        // A screen that never paints the overlay must not collect requests forever: keep
        // the newest MAX_PENDING (toasts go first).
        while self.pending.len() > MAX_PENDING {
            let i = self.pending.iter().position(|(_, r)| matches!(r, UiRequest::Toast { .. } | UiRequest::Notification { .. })).unwrap_or(0);
            if let Some((p, r)) = self.pending.remove(i) {
                log::warn!("plugins: {p}: dropped unshown {}", describe_request(&r));
            }
        }
    }

    /// Sets the label of the item with `action` in the plugin's menu `id` (open or still
    /// waiting). An open menu repaints only that row (GL16, one update); a row scrolled
    /// out of the menu changes silently.
    fn menu_label(&mut self, plugin: &str, id: u32, action: &str, label: &str) {
        if let Some(l) = self.layers.iter_mut().find(|l| l.plugin == plugin && l.id == Some(id) && matches!(l.kind, Kind::Menu(_))) {
            let Kind::Menu(m) = &mut l.kind else { return };
            let local = Rect::new(0, 0, l.rect.w, l.rect.h);
            let Some(Some(rect)) = m.set_label(local, action, label) else { return };
            l.refresh_parts([rect]);
            l.dirty = l.dirty.or(Some(RefreshClass::Gl16));
            return;
        }
        let waiting = self.pending.iter_mut().find_map(|(p, r)| match r {
            UiRequest::Menu { id: i, items, .. } if p == plugin && *i == id => Some(items),
            _ => None,
        });
        if let Some(item) = waiting.and_then(|items| items.iter_mut().find(|(_, a)| a == action)) {
            item.0 = label.to_string();
        }
    }

    /// True while a modal layer is open.
    pub fn modal_open(&self) -> bool {
        self.layers.iter().any(|l| l.kind.modal())
    }

    /// Removes the first waiting [`UiRequest::Dialog`] of `plugin` that uses the card
    /// convention and returns its id and card (see [`PluginHost::take_card_dialog`]).
    pub fn take_card(&mut self, plugin: &str) -> Option<(u32, CardDialog)> {
        let i = self.pending.iter().position(|(p, r)| p == plugin && matches!(r, UiRequest::Dialog { title, body, buttons, .. } if CardDialog::parse(title, body, buttons).is_some()))?;
        let (_, UiRequest::Dialog { id, title, body, buttons }) = self.pending.remove(i)? else { return None };
        log::debug!("plugins: {plugin}: card dialog {id} {title:?} taken by the screen");
        Some((id, CardDialog::parse(&title, &body, &buttons)?))
    }

    /// True when `plugin`'s request `req` may open now: not modal, no modal layer open,
    /// or the keyboard or a dialog over the plugin's own chat view (a viewer with an
    /// input bar, [`MarkdownView::stacks`]) when that viewer is the only modal layer.
    fn can_start(&self, plugin: &str, req: &UiRequest) -> bool {
        if !opens_modal(req) || !self.modal_open() {
            return true;
        }
        let mut modals = self.layers.iter().filter(|l| l.kind.modal());
        let (Some(only), None) = (modals.next(), modals.next()) else { return false };
        matches!(req, UiRequest::TextInput { .. } | UiRequest::Dialog { .. }) && only.plugin == plugin && matches!(&only.kind, Kind::Viewer(v) if v.stacks())
    }

    /// Requests waiting to be shown.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Names of the open layers, bottom first (`toast`, `dialog`, `menu`, `viewer`, `input`).
    pub fn layer_names(&self) -> Vec<&'static str> {
        self.layers.iter().map(|l| l.kind.name()).collect()
    }

    /// Time until the next toast expires.
    pub fn timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let next = self.layers.iter().filter_map(|l| l.expires).min();
        // Paint now when a waiting request can start (a modal one waits while another
        // modal layer is open; it must not make the loop spin) or pixels wait to go back.
        let startable = self.pending.front().is_some_and(|(p, r)| self.can_start(p, r));
        let soon = (startable || !self.restore.is_empty()).then_some(Duration::ZERO);
        next.map(|t| t.saturating_duration_since(now)).into_iter().chain(soon).min()
    }

    /// Closes the toasts whose time is up.
    fn close_expired(&mut self) {
        let now = Instant::now();
        while let Some(i) = self.layers.iter().position(|l| l.expires.is_some_and(|t| t <= now)) {
            self.close(i);
        }
    }

    /// Puts the pixels of closed layers back on the canvas and adds their damage.
    fn put_back(&mut self, canvas: &mut Canvas, damage: &mut Vec<Damage>) {
        for (r, pixels, class) in self.restore.drain(..) {
            canvas.blit(&pixels, r.x, r.y);
            damage.push(Damage { rect: r, class });
        }
    }

    fn close(&mut self, i: usize) -> Layer {
        let l = self.layers.remove(i);
        // A docked keyboard: the chat view below takes the whole height again and is
        // drawn again (no saved pixels go back), without a flash.
        if matches!(&l.kind, Kind::Input(b) if b.docked) {
            if let Some(v) = self.docked_view(&l.plugin) {
                if let Kind::Viewer(view) = &mut v.kind {
                    view.set_dock(0);
                }
                v.redraw(v.at_least(RefreshClass::Reagl));
                return l;
            }
        }
        if let Some(b) = &l.backing {
            // A closing toast puts gray content back (page text, covers): GL16, not DU
            // (DU + FORCE_MONOCHROME left the text under it crushed to 2 levels).
            // A closing full-screen viewer is REAGL (every pixel, no flash), as the
            // reader's full-screen overlays (Change::FullOverlayClose).
            let class = if matches!(l.kind, Kind::Viewer(_)) { RefreshClass::Reagl } else { RefreshClass::Gl16 };
            self.restore.push((l.rect, b.clone(), class));
        }
        // Layers above it covered part of its pixels: they are drawn again.
        for above in self.layers.iter_mut().skip(i) {
            above.backing = None;
            above.redraw(RefreshClass::Gl16);
        }
        l
    }

    /// The open chat view of `plugin` (a viewer with an input bar, not a sheet) that a
    /// keyboard request docks under.
    fn docked_view(&mut self, plugin: &str) -> Option<&mut Layer> {
        self.layers.iter_mut().rev().find(|l| l.plugin == plugin && matches!(&l.kind, Kind::Viewer(v) if v.stacks() && !v.sheet))
    }

    /// Handles `ev` for the overlay: `(used, answers)` where an answer is
    /// `(plugin, request id, result)`.
    pub fn handle(&mut self, ev: &UiEvent, screen: Size) -> (bool, Vec<(String, u32, String)>) {
        self.close_expired();
        // Per-contact touches go only to an open keyboard (it types on them); other
        // layers work on gestures.
        let keyboard_open = self.layers.last().is_some_and(|l| matches!(l.kind, Kind::Input(_)));
        let input = matches!(ev, UiEvent::Gesture(_) | UiEvent::Key(_)) || (keyboard_open && matches!(ev, UiEvent::Touch(_)));
        if !input {
            // Under a modal layer (the chat view, a dialog) the screen gets no touches
            // either: the reader's edge slides set the light from them, and they belong
            // to the page alone.
            return (matches!(ev, UiEvent::Touch(_)) && self.modal_open(), Vec::new());
        }
        if let UiEvent::Key(k) = ev {
            if k.key == ink_device::Key::Power {
                return (false, Vec::new());
            }
        }
        // A touch or key dismisses a toast; it is not modal.
        while let Some(i) = self.layers.iter().rposition(|l| !l.kind.modal()) {
            self.close(i);
        }
        let Some(top) = self.layers.len().checked_sub(1) else { return (false, Vec::new()) };
        let _ = screen;
        // Swipes over the chat view above a docked keyboard turn the view's pages.
        let over_view = matches!(&self.layers[top].kind, Kind::Input(i) if i.docked) && gesture_pos(ev).is_some_and(|p| !self.layers[top].rect.contains(p));
        if over_view && matches!(ev, UiEvent::Gesture(Gesture::Swipe { .. })) {
            let plugin = self.layers[top].plugin.clone();
            if let Some(v) = self.docked_view(&plugin) {
                let (vr, lev) = (Rect::new(0, 0, v.rect.w, v.rect.h), local(ev, v.rect));
                if let Kind::Viewer(view) = &mut v.kind {
                    let before = view.position();
                    view.event(&lev, vr);
                    if view.position() != before {
                        // Only the pages above the panel: the keyboard stays.
                        let pages = view.pages_rect(vr);
                        v.refresh_parts([pages]);
                        v.dirty = Some(v.at_least(RefreshClass::Reagl));
                    }
                }
            }
            return (true, Vec::new());
        }
        let l = &mut self.layers[top];
        let back = matches!(ev, UiEvent::Key(k) if k.pressed && matches!(k.key, ink_device::Key::Back | ink_device::Key::Home));
        let outside = gesture_pos(ev).is_some_and(|p| !l.rect.contains(p));
        let result = if back {
            Some(l.kind.cancel_result().to_string())
        } else if outside {
            // A tap outside a menu or a card cancels it; dialogs, viewers and the
            // keyboard stay.
            match (&l.kind, ev) {
                (Kind::Menu(_) | Kind::Card(_), UiEvent::Gesture(Gesture::Tap { .. })) => Some(String::new()),
                // Over the chat view a tap puts the docked keyboard away (the draft
                // stays); swipes turn the view's pages above it.
                (Kind::Input(i), UiEvent::Gesture(Gesture::Tap { .. })) if i.docked => Some(String::new()),
                // A sheet (`:::sheet` viewer) closes like a menu.
                (Kind::Viewer(v), UiEvent::Gesture(Gesture::Tap { .. })) if v.sheet => Some("closed".into()),
                _ => None,
            }
        } else {
            let lev = local(ev, l.rect);
            let lr = Rect::new(0, 0, l.rect.w, l.rect.h);
            let before = l.kind.position();
            match l.kind.widget().event(&lev, lr) {
                EventResult::Action(a) => match a.as_str() {
                    "input:submit" => match &l.kind {
                        Kind::Input(i) => Some(i.text.clone()),
                        _ => None,
                    },
                    "input:cancel" => Some(String::new()),
                    "input:edit" | "input:keys" => {
                        // Gray-preserving GL16 on the changed key faces (and the field
                        // when the text changed), not the whole panel.
                        let mut rects = match &mut l.kind {
                            Kind::Input(i) => i.take_damage(),
                            _ => None,
                        };
                        if a == "input:edit" {
                            if let (Some(r), Kind::Input(i)) = (rects.as_mut(), &l.kind) {
                                r.push(i.parts(lr).0);
                            }
                        }
                        match rects {
                            Some(r) => l.refresh_parts(r),
                            None => l.partial = None,
                        }
                        l.dirty = Some(RefreshClass::Gl16);
                        None
                    }
                    _ => Some(a),
                },
                _ => {
                    if l.kind.position() != before {
                        match &l.kind {
                            // A chat view scrolled: only the text below the app bar
                            // moves, REAGL like a page turn (no flash).
                            Kind::Viewer(v) if v.scrolls() => {
                                let pages = v.pages_rect(lr);
                                l.refresh_parts([pages]);
                                l.dirty = Some(l.at_least(RefreshClass::Reagl));
                            }
                            // A full-screen viewer page is text like a book page: REAGL,
                            // no flash (its GL16 would be promoted to a full-screen GC16).
                            Kind::Viewer(v) if !v.sheet => l.redraw(RefreshClass::Reagl),
                            // A menu's list jump is one DU of the layer (its rows are
                            // black and white, as on a settings page).
                            Kind::Menu(m) => {
                                let class = m.refresh_class();
                                l.redraw(class);
                            }
                            _ => l.redraw(RefreshClass::Gl16),
                        }
                    }
                    None
                }
            }
        };
        let mut answers = Vec::new();
        if let Some(r) = result {
            let (plugin, id) = (l.plugin.clone(), l.id);
            // A docked keyboard put away keeps its text in the chat view's input bar.
            if let (Kind::Input(i), true) = (&self.layers[top].kind, r.is_empty()) {
                if i.docked {
                    let draft = i.text.clone();
                    if let Some(Kind::Viewer(v)) = self.docked_view(&plugin).map(|v| &mut v.kind) {
                        v.draft = (!draft.trim().is_empty()).then_some(draft);
                    }
                }
            }
            let l = &self.layers[top];
            // Link taps keep the viewer open; a sheet stays for a `stay:` row (a
            // checkbox the plugin redraws in place).
            let keep = matches!(&l.kind, Kind::Viewer(v) if !v.sheet || r.starts_with("stay:") || r.starts_with("hold:")) && r != "closed";
            if !keep {
                self.close(top);
            }
            if let Some(id) = id {
                answers.push((plugin, id, r));
            }
        }
        (true, answers)
    }

    fn open(&mut self, plugin: String, req: UiRequest, screen: Size, cx: &mut PaintCx) {
        let (w, h) = (screen.w, screen.h);
        let (id, kind, rect, expires) = match req {
            UiRequest::Toast { text } => (None, Kind::Toast(Toast { text, timeout: self.toast_time }), Toast::frame(screen), Some(Instant::now() + self.toast_time)),
            UiRequest::Notification { text } => {
                (None, Kind::Toast(Toast { text, timeout: self.notification_time }), Toast::frame(screen), Some(Instant::now() + self.notification_time))
            }
            UiRequest::Dialog { id, title, body, buttons } => match CardDialog::parse(&title, &body, &buttons) {
                Some(mut c) => {
                    let r = c.frame(cx, screen);
                    (Some(id), Kind::Card(c), r, None)
                }
                None => (Some(id), Kind::Dialog(Dialog { title, body, buttons }), Dialog::frame(screen), None),
            },
            UiRequest::Menu { id, title, items } => {
                let m = MenuBox::new(title, items);
                let rect = m.frame(screen);
                (Some(id), Kind::Menu(m), rect, None)
            }
            UiRequest::TextViewer { id, title, markdown } => {
                let v = MarkdownView::new(&title, &markdown);
                let rect = if v.sheet { BottomSheet::frame(screen, v.sheet_height()) } else { Rect::new(0, 0, w, h) };
                (Some(id), Kind::Viewer(v), rect, None)
            }
            UiRequest::TextInput { id, prompt, initial } => {
                let layouts = self.keyboard_layouts.clone();
                match self.docked_view(&plugin) {
                    // Over the plugin's chat view: its input bar moves up onto the
                    // keyboard and the view lays its pages out above them (REAGL on the
                    // pages, GL16 on the panel: no flash).
                    Some(v) => {
                        let ih = InputBox::docked_height().min(h);
                        let Kind::Viewer(view) = &mut v.kind else { unreachable!() };
                        let initial = if initial.is_empty() { view.draft.take().unwrap_or_default() } else { initial };
                        view.set_dock(ih);
                        let pages = view.pages_rect(Rect::new(0, 0, v.rect.w, v.rect.h));
                        if !v.whole_pending() {
                            v.partial = Some(vec![pages]);
                            v.dirty = Some(v.at_least(RefreshClass::Reagl));
                        }
                        (Some(id), Kind::Input(InputBox::docked(prompt, initial, &layouts)), Rect::new(0, (h - ih) as i32, w, ih), None)
                    }
                    None => {
                        let ih = InputBox::height().min(h);
                        (Some(id), Kind::Input(InputBox::new(prompt, initial, &layouts)), Rect::new(0, (h - ih) as i32, w, ih), None)
                    }
                }
            }
            UiRequest::Status { .. } | UiRequest::GoTo { .. } | UiRequest::LibraryRescan | UiRequest::TileState { .. } | UiRequest::MenuLabel { .. } => return,
        };
        let class = match &kind {
            Kind::Toast(_) => RefreshClass::Du,
            // A full-screen viewer opens with REAGL: every pixel, no flash (a GL16 of the
            // whole screen would be promoted to a GC16 by `coalesce`).
            Kind::Viewer(v) if !v.sheet => RefreshClass::Reagl,
            _ => RefreshClass::Gl16,
        };
        self.layers.push(Layer { plugin, id, rect, backing: None, dirty: Some(class), partial: None, stream: false, kind, expires });
    }

    /// Paints the overlay into `cx.canvas`; returns the damage. `below_redrawn`: the
    /// screen repainted the canvas, so every layer is drawn again on top.
    pub fn paint(&mut self, cx: &mut PaintCx, below_redrawn: bool) -> Vec<Damage> {
        let screen = cx.canvas.size();
        let mut damage = Vec::new();
        self.close_expired();
        if below_redrawn {
            // The pixels under every layer are new: drop the copies and the restores.
            self.restore.clear();
            for l in &mut self.layers {
                l.backing = None;
                l.redraw(l.dirty.unwrap_or(RefreshClass::Gl16));
                l.stream = false;
            }
        }
        self.put_back(cx.canvas, &mut damage);
        // Start waiting requests.
        while let Some((plugin, req)) = self.pending.front().cloned() {
            let modal = opens_modal(&req);
            if !self.can_start(&plugin, &req) {
                break;
            }
            self.pending.pop_front();
            // Several toasts at once: show the first and how many more there are (all
            // of them are in the notification list or the log).
            let note = matches!(req, UiRequest::Notification { .. });
            let req = match req {
                UiRequest::Toast { text } | UiRequest::Notification { text } if !modal => {
                    let mut more = 0;
                    while matches!(self.pending.front(), Some((_, UiRequest::Toast { .. } | UiRequest::Notification { .. }))) {
                        self.pending.pop_front();
                        more += 1;
                    }
                    let text = if more > 0 { format!("{text} (+{more} more)") } else { text };
                    if note {
                        UiRequest::Notification { text }
                    } else {
                        UiRequest::Toast { text }
                    }
                }
                other => other,
            };
            // One toast at a time; a modal layer replaces a toast.
            while let Some(i) = self.layers.iter().rposition(|l| !l.kind.modal()) {
                self.close(i);
            }
            self.put_back(cx.canvas, &mut damage);
            self.open(plugin, req, screen, cx);
        }
        // Paint dirty layers bottom to top; a repainted layer forces the ones above that
        // its new pixels reach (`drawn`: screen rectangles put on the canvas so far).
        let mut drawn: Vec<Rect> = Vec::new();
        for l in &mut self.layers {
            if l.dirty.is_none() && drawn.iter().any(|d| !d.intersect(&l.rect).is_empty()) {
                l.redraw(RefreshClass::Gl16);
                l.backing = None;
            }
            let Some(class) = l.dirty.take() else { continue };
            // The canvas under the layer is what the panel shows of it unless a layer
            // below was drawn over it now (`drawn`) or it was never shown.
            let stream = std::mem::take(&mut l.stream) && l.backing.is_some() && !drawn.iter().any(|d| !d.intersect(&l.rect).is_empty());
            let r = l.rect.intersect(&cx.canvas.bounds());
            if r.is_empty() {
                continue;
            }
            if l.backing.is_none() {
                l.backing = Some(cx.canvas.crop(r));
            }
            let mut off = Canvas::new(r.w as usize, r.h as usize);
            off.clear(cx.theme.bg);
            {
                let mut pcx = PaintCx { canvas: &mut off, fonts: &mut *cx.fonts, theme: cx.theme };
                if let Kind::Toast(t) = &mut l.kind {
                    // Keep a "(+N more)" suffix visible when the text is cut.
                    let px = pcx.theme.font_px * 0.9;
                    let (head, tail) = match t.text.rfind(" (+") {
                        Some(i) if t.text.ends_with(" more)") => (t.text[..i].to_string(), t.text[i..].to_string()),
                        _ => (t.text.clone(), String::new()),
                    };
                    let tail_w = pcx.fonts.text_width(Style::Regular, &tail, px);
                    t.text = format!("{}{tail}", fit(&mut pcx, &head, px, Style::Regular, r.w as f32 - 40.0 - tail_w));
                }
                l.kind.widget().paint(&mut pcx, Rect::new(0, 0, r.w, r.h));
            }
            if stream {
                // Streamed text: the changed lines only. A whole new page (the view
                // followed the end) of a large view is REAGL: no flash either way.
                let old = cx.canvas.crop(r);
                cx.canvas.blit(&off, r.x, r.y);
                drawn.push(r);
                let local = Rect::new(0, 0, r.w, r.h);
                let bands = ink_ui::refresh::changed_bands(&old, &off, vec![Damage { rect: local, class }]);
                let moved: Vec<Damage> = bands.into_iter().map(|d| Damage { rect: Rect::new(d.rect.x + r.x, d.rect.y + r.y, d.rect.w, d.rect.h), class: d.class }).collect();
                let big = ink_ui::refresh::would_flash(&moved, cx.canvas.bounds());
                damage.extend(moved.into_iter().map(|d| if big && d.class == RefreshClass::Gl16 { Damage { class: RefreshClass::Reagl, ..d } } else { d }));
                l.partial = None;
                continue;
            }
            // The whole layer is drawn again, but pixels outside `partial` did not
            // change: only those rectangles go on the canvas and are refreshed (a layer
            // above them that they do not reach keeps its pixels).
            match l.partial.take().filter(|_| l.rect.x == r.x && l.rect.y == r.y) {
                Some(parts) => {
                    for p in parts {
                        let sr = Rect::new(p.x + r.x, p.y + r.y, p.w, p.h).intersect(&r);
                        if sr.is_empty() {
                            continue;
                        }
                        cx.canvas.blit(&off.crop(Rect::new(sr.x - r.x, sr.y - r.y, sr.w, sr.h)), sr.x, sr.y);
                        drawn.push(sr);
                        damage.push(Damage { rect: sr, class });
                    }
                }
                None => {
                    cx.canvas.blit(&off, r.x, r.y);
                    drawn.push(r);
                    damage.push(Damage { rect: r, class });
                }
            }
        }
        ink_ui::refresh::merge(damage)
    }

    /// Paints the overlay after a screen painted the canvas. `changed`: the rectangles
    /// the screen repainted (its damage and what it showed itself). Those pixels under an
    /// open layer go into the layer's saved background, and the layer is drawn again on
    /// top; layers the screen did not touch are left alone. A rectangle covering the
    /// whole canvas acts as `paint(cx, true)`. The app loop calls it after every
    /// `AppScreen::paint` (see [`hooks::app_paint`]).
    pub fn compose(&mut self, cx: &mut PaintCx, changed: &[Rect]) -> Vec<Damage> {
        let bounds = cx.canvas.bounds();
        if changed.iter().any(|c| c.intersect(&bounds) == bounds) {
            return self.paint(cx, true);
        }
        let mut first_dirty = None;
        for (i, l) in self.layers.iter_mut().enumerate() {
            let Some(b) = l.backing.as_mut() else { continue };
            if patch_backing(cx.canvas, l.rect, b, changed) {
                l.redraw(l.dirty.unwrap_or(RefreshClass::Gl16));
                l.stream = false;
                first_dirty.get_or_insert(i);
            }
        }
        // A layer above a changed one saved pixels of the old lower layer: its saved
        // pixels (patched above) go back first, top layer first, and it takes a new copy
        // after the lower layer is drawn.
        if let Some(i) = first_dirty {
            for l in self.layers.iter_mut().skip(i + 1).rev() {
                if let Some(b) = l.backing.take() {
                    let r = l.rect.intersect(&bounds);
                    cx.canvas.blit(&b, r.x, r.y);
                }
                l.redraw(l.dirty.unwrap_or(RefreshClass::Gl16));
            }
        }
        for (r, b, _) in &mut self.restore {
            patch_backing(cx.canvas, *r, b, changed);
        }
        self.paint(cx, false)
    }
}

/// True when the launcher restarts rinky after this exit (`rinky-ctl.sh restart`
/// writes `$RINKY_RUN/restart` before its SIGTERM; the Kindle UI stays stopped).
pub fn restart_pending() -> bool {
    std::env::var_os("RINKY_RUN").is_some_and(|run| std::path::Path::new(&run).join("restart").exists())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ink_device::{Key, KeyEvent, Timestamp};
    use ink_plugin::{HostCtx, Permission, PluginManifest, Result};
    use ink_text::Fonts;
    use ink_ui::{KeyboardLayout, Theme};

    const W: u32 = 1272;
    const H: u32 = 1696;
    /// Title row of a plugin menu (the settings popup).
    const TITLE_H: u32 = popup::TITLE_H;
    /// A menu row.
    const ROW: u32 = crate::screens::settings::rows::ROW_H;

    fn screen() -> Size {
        Size::new(W, H)
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ink-plugin-host-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A repository file; `INK_REPO` points elsewhere (a cross-built test binary on the
    /// emulator gets the files pushed next to it).
    fn repo(rel: &str) -> PathBuf {
        match std::env::var_os("INK_REPO") {
            Some(r) => PathBuf::from(r).join(rel),
            None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(rel),
        }
    }

    /// Where screenshots go (`INK_PLUGIN_SHOTS=DIR cargo test -p ink-app plugin_host`).
    fn shot(canvas: &Canvas, name: &str) {
        if let Some(dir) = std::env::var_os("INK_PLUGIN_SHOTS") {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            canvas.save_png(&dir.join(format!("{name}.png"))).unwrap();
        }
    }

    fn tap(x: i32, y: i32) -> UiEvent {
        UiEvent::Gesture(Gesture::Tap { pos: Point::new(x, y) })
    }

    /// One short tap as `ink_ui::pump` delivers it: touch down, touch up, then the gesture
    /// (the keyboard types on the touches).
    fn press(x: i32, y: i32) -> [UiEvent; 3] {
        let p = Point::new(x, y);
        let t = |kind| UiEvent::Touch(ink_ui::Touch { slot: 0, kind, pos: p, start: p, held: Duration::from_millis(80), moved: false });
        [t(ink_ui::TouchKind::Down), t(ink_ui::TouchKind::Up), tap(x, y)]
    }

    fn key(k: Key) -> UiEvent {
        UiEvent::Key(KeyEvent { key: k, pressed: true, time: Timestamp::default() })
    }

    struct Screen {
        canvas: Canvas,
        fonts: Fonts,
        theme: Theme,
    }

    impl Screen {
        fn new() -> Self {
            Screen { canvas: Canvas::new(W as usize, H as usize), fonts: Fonts::load(), theme: Theme::day() }
        }
        fn page(&mut self, doc: &mut dyn Document, page: usize) {
            crate::screens::reader::render_page(doc, page, &mut self.canvas, &mut self.fonts).unwrap();
        }
        fn overlay(&mut self, host: &mut PluginHost, below: bool) -> Vec<Damage> {
            let mut cx = PaintCx { canvas: &mut self.canvas, fonts: &mut self.fonts, theme: &self.theme };
            host.paint_overlay(&mut cx, below)
        }
        fn ui(&mut self, ui: &mut PluginUi, below: bool) -> Vec<Damage> {
            let mut cx = PaintCx { canvas: &mut self.canvas, fonts: &mut self.fonts, theme: &self.theme };
            ui.paint(&mut cx, below)
        }
    }

    /// A viewer shown twice before a paint (Insightful's Explain: the chat is shown, then
    /// shown again as its request starts) opens once, with the newer text.
    #[test]
    fn a_viewer_shown_twice_before_a_paint_opens_once() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        ui.push("ai", UiRequest::TextViewer { id: 3, title: "Chat".into(), markdown: "Thinking".into() });
        ui.push("ai", UiRequest::TextViewer { id: 3, title: "Chat 2".into(), markdown: "Thinking…".into() });
        assert_eq!(ui.pending(), 1);
        s.ui(&mut ui, false);
        assert_eq!(ui.layer_names(), ["viewer"]);
        assert!(matches!(&ui.layers[0].kind, Kind::Viewer(v) if v.title == "Chat 2"));
        assert_eq!(ui.pending(), 0);
    }

    /// Streamed text replacing a viewer's content (an AI answer arriving in batches)
    /// refreshes only the lines that changed, GL16, never the whole view: a whole-view
    /// GL16 over 80 % of the screen would be promoted to a GC16 flash per batch. A batch
    /// that moves the view to a new page is REAGL (no flash either).
    #[test]
    fn streamed_viewer_text_refreshes_only_the_changed_lines() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        let screen_rect = Rect::new(0, 0, W, H);
        let mut text = String::from("# Answer\n\nThe first sentence of the answer.");
        ui.push("ai", UiRequest::TextViewer { id: 3, title: "Chat".into(), markdown: text.clone() });
        let opened = s.ui(&mut ui, false);
        assert!(!opened.is_empty());
        let view = ui.layers[0].rect;
        for i in 0..4 {
            text.push_str(&format!(" More words in batch {i}."));
            ui.push("ai", UiRequest::TextViewer { id: 3, title: "Chat".into(), markdown: text.clone() });
            let d = s.ui(&mut ui, false);
            assert!(!d.is_empty(), "batch {i}: the new words are shown");
            for u in &d {
                assert_eq!(u.class, RefreshClass::Gl16, "batch {i}: {d:?}");
                assert!(u.rect.intersect(&view) == u.rect && u.rect.h < view.h / 2, "batch {i}: only the changed lines: {d:?}");
            }
            assert!(!ink_ui::refresh::would_flash(&d, screen_rect), "batch {i}: {d:?}");
        }
        // A long batch that fills pages: the view follows the end. Never a flash.
        for i in 0..80 {
            text.push_str(&format!("\n\nParagraph {i} of a long streamed answer with enough words to wrap."));
        }
        ui.push("ai", UiRequest::TextViewer { id: 3, title: "Chat".into(), markdown: text.clone() });
        let d = s.ui(&mut ui, false);
        assert!(!d.is_empty());
        let promoted = ink_ui::refresh::coalesce(d.clone(), screen_rect);
        assert!(promoted.iter().all(|u| u.class != RefreshClass::Gc16), "{d:?} -> {promoted:?}");
        // The panel shows the same as a full repaint of the layer would.
        let mut fresh = Screen::new();
        let mut ui2 = PluginUi::default();
        ui2.push("ai", UiRequest::TextViewer { id: 3, title: "Chat".into(), markdown: text.clone() });
        fresh.ui(&mut ui2, false);
        if let (Kind::Viewer(a), Kind::Viewer(b)) = (&ui.layers[0].kind, &ui2.layers[0].kind) {
            if a.page == b.page {
                assert!(s.canvas.pixels == fresh.canvas.pixels, "the streamed view is drawn as a fresh one");
            }
        }
    }

    /// A chat view (scrolling, with an input bar) streaming an answer, with the keyboard
    /// docked and without: each batch refreshes only what changed and never flashes (a
    /// new line scrolls the window: a large GL16, which `coalesce` may widen to a
    /// full-screen GL16, still partial).
    #[test]
    fn streamed_chat_answers_never_flash_docked_or_not() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        let all = Rect::new(0, 0, W, H);
        let chat = |answer: &str| {
            let mut md = String::new();
            for i in 0..12 {
                md.push_str(&format!(":::you\nQuestion {i}?\n:::answer model |\nAnswer number {i}, a few words long.\n"));
            }
            md.push_str(&format!(":::you\nThe last question?\n:::answer model |\n{answer}\n:::\n:::input chat:ask | Ask about this book…"));
            md
        };
        let mut answer = String::from("The");
        ui.push("p", UiRequest::TextViewer { id: 1, title: "Chat".into(), markdown: chat(&answer) });
        s.ui(&mut ui, false);
        for docked in [false, true] {
            if docked {
                ui.push("p", UiRequest::TextInput { id: 2, prompt: "Ask".into(), initial: String::new() });
                s.ui(&mut ui, false);
                assert_eq!(ui.layer_names(), vec!["viewer", "input"]);
            }
            for i in 0..6 {
                answer.push_str(&format!(" words of batch {i}"));
                ui.push("p", UiRequest::TextViewer { id: 1, title: "Chat".into(), markdown: chat(&answer) });
                let d = s.ui(&mut ui, false);
                let shown = ink_ui::refresh::coalesce(d.clone(), all);
                assert!(shown.iter().all(|u| u.class != RefreshClass::Gc16), "docked {docked}, batch {i}: {d:?} -> {shown:?}");
                assert!(!d.is_empty() && d.iter().all(|u| u.rect.y >= APP_BAR_H as i32), "the app bar stays: {d:?}");
            }
        }
    }

    /// A chat view pages by whole windows: each turn starts at the first line the last
    /// window did not wholly show (back: ends where the last one started), no line is
    /// cut at the top, and the last turn forward is a whole window too (blank below).
    /// Streamed text never moves the window; a new question goes to its top.
    #[test]
    fn chat_view_pages_whole_windows_and_streaming_does_not_scroll() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        let chat = |questions: usize, answer: &str| {
            let mut md = String::new();
            for i in 0..questions {
                md.push_str(&format!(":::you\nQuestion {i}?\n:::answer model |\nAnswer number {i}, with enough words in it to wrap over a couple of lines on the panel.\n"));
            }
            md.push_str(&format!(":::you\nThe last question?\n:::answer model |\n{answer}\n:::\n:::input chat:ask | Ask about this book…"));
            md
        };
        ui.push("p", UiRequest::TextViewer { id: 1, title: "Chat".into(), markdown: chat(20, "Short.") });
        s.ui(&mut ui, false);
        let v = |ui: &PluginUi| match &ui.layers[0].kind {
            Kind::Viewer(v) => (v.scroll, v.window_h, v.content_h, v.at_end(), v.line_tops()),
            _ => panic!(),
        };
        let (end, win, _, at_end, tops) = v(&ui);
        assert!(at_end && end > 0.0);
        let on_line = |y: f32| tops.iter().any(|(t, _)| (t - y).abs() < 0.5);
        assert!(on_line(end), "the end starts on a line");
        // Back to the start, then forward to the end.
        let mut at = end;
        while at > 0.0 {
            ui.handle(&key(Key::PagePrev), screen());
            let (now, ..) = v(&ui);
            assert!(now < at && now >= at - win - 0.5 && on_line(now), "back from {at} to {now}");
            at = now;
        }
        loop {
            ui.handle(&key(Key::PageNext), screen());
            let (now, _, content, ..) = v(&ui);
            if now == at {
                break;
            }
            assert!(now > at && now <= at + win + 0.5 && on_line(now), "forward from {at} to {now}");
            assert!(tops.iter().all(|(t, h)| !(*t < now - 0.5 && t + h > at + win + 0.5)), "no line skipped");
            assert!(now <= content, "{now}");
            at = now;
        }
        assert!(v(&ui).3, "at the end");
        s.ui(&mut ui, false);

        // Streamed text that grows past the window leaves it where it is.
        ui.handle(&key(Key::PagePrev), screen());
        ui.handle(&key(Key::PageNext), screen());
        let (before, ..) = v(&ui);
        // Five batches of five sentences: each push lays out the whole chat again (about
        // 0.7 s in a debug build), and the answer still ends well below the window.
        let mut answer = String::from("The");
        for i in 0..5 {
            for j in 0..5 {
                answer.push_str(&format!(" streamed words of batch {i}.{j}, more of them, and then some more words to fill a few lines of the answer card."));
            }
            ui.push("p", UiRequest::TextViewer { id: 1, title: "Chat".into(), markdown: chat(20, &answer) });
            s.ui(&mut ui, false);
            assert_eq!(v(&ui).0, before, "batch {i}");
        }
        assert!(!v(&ui).3, "the answer goes on below the window");

        // A new question: at the top of the window.
        let mut md = chat(20, &answer).replace(":::\n:::input", "");
        md = md.trim_end_matches(" Ask about this book…").trim_end_matches("chat:ask |").to_string();
        let md = format!("{}\n:::you\nA new question?\n:::answer model |\nThinking…\n:::\n:::input chat:ask | Ask about this book…", md.trim_end());
        ui.push("p", UiRequest::TextViewer { id: 1, title: "Chat".into(), markdown: md });
        s.ui(&mut ui, false);
        let Kind::Viewer(view) = &ui.layers[0].kind else { panic!() };
        let top_line = view.pages[0].iter().zip(view.line_tops()).find(|(_, (t, _))| (*t - view.scroll).abs() < 0.5).map(|(l, _)| l.group).expect("a line at the top");
        let yous: Vec<usize> = view.groups.iter().enumerate().filter(|(_, g)| g.kind == BoxKind::You).map(|(i, _)| i).collect();
        assert_eq!(top_line, yous.last().copied(), "the new question is at the top");
    }

    /// A sheet row whose link starts with `stay:` reports it and leaves the sheet open
    /// (a checkbox the plugin redraws in place); other rows close it. A long press on a
    /// link reports `hold:LINK` and leaves the view open.
    #[test]
    fn sheet_stay_rows_and_held_links() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        ui.push("p", UiRequest::TextViewer { id: 1, title: "Menu".into(), markdown: ":::sheet\n:::row go | chat\nGo\n:::row stay:tick | check\nTick\n".into() });
        s.ui(&mut ui, false);
        let link = |ui: &PluginUi, name: &str| {
            let l = &ui.layers[0];
            let Kind::Viewer(v) = &l.kind else { panic!() };
            let (r, _) = v.links.iter().find(|(_, k)| k == name).cloned().expect(name);
            Point::new(l.rect.x + r.x + 20, l.rect.y + r.y + r.h as i32 / 2)
        };
        let p = link(&ui, "stay:tick");
        assert_eq!(ui.handle(&tap(p.x, p.y), screen()).1, vec![("p".to_string(), 1, "stay:tick".to_string())]);
        assert_eq!(ui.layer_names(), vec!["viewer"], "the sheet stays");
        ui.push("p", UiRequest::TextViewer { id: 1, title: "Menu".into(), markdown: ":::sheet\n:::row go | chat\nGo\n:::row stay:tick | minus\nTick\n".into() });
        s.ui(&mut ui, false);
        assert_eq!(ui.layers.len(), 1, "redrawn in place");
        let p = link(&ui, "go");
        assert_eq!(ui.handle(&tap(p.x, p.y), screen()).1, vec![("p".to_string(), 1, "go".to_string())]);
        assert!(ui.layers.is_empty(), "a plain row closes the sheet");

        ui.push("p", UiRequest::TextViewer { id: 2, title: "Chats".into(), markdown: ":::card open/c1 | caption\n**Chat**\n".into() });
        s.ui(&mut ui, false);
        let p = link(&ui, "open/c1");
        let held = UiEvent::Gesture(Gesture::LongPress { pos: p, held: Duration::from_millis(500) });
        assert_eq!(ui.handle(&held, screen()).1, vec![("p".to_string(), 2, "hold:open/c1".to_string())]);
        assert_eq!(ui.layer_names(), vec!["viewer"], "the view stays");
    }

    /// Tables as models write them, in an answer card: one without outer pipes, one
    /// with long prose cells. Every row is laid out in columns (no "a | b" fallback):
    /// cells wrap inside their column, nothing runs past the card, and a row is as tall
    /// as its tallest cell.
    #[test]
    fn answer_tables_wrap_inside_their_columns() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        let md = ":::answer AI — model |\nThe two houses:\n\nHouse | Members | Seat\n--- | --- | ---\nMontague | Romeo, Lord and Lady Montague, Benvolio | Verona\nCapulet | Juliet, Lord and Lady Capulet, Tybalt, the Nurse | Verona\n\n| Theme | Where it shows | Why it matters |\n|---|---|---|\n| Fate | The prologue calls the lovers \"star-cross'd\" and their deaths are foretold before the play begins | It frames every choice the lovers make as already decided, so suspense comes from how, not whether |\n| Haste | Romeo and Juliet marry a day after meeting; Romeo kills Tybalt in a moment of rage | Speed drives the tragedy: every delay that could save them is skipped |\n:::\n:::input chat:ask | Ask";
        let blocks = markdown::parse(":::x\nHouse | Members\n--- | ---\nA | B\n".trim_start_matches(":::x\n"));
        assert!(matches!(&blocks[0], Block::Table { rows, header: true } if rows.len() == 2 && rows[0].len() == 2), "a table without outer pipes: {blocks:?}");
        ui.push("p", UiRequest::TextViewer { id: 1, title: "Chat".into(), markdown: md.into() });
        s.ui(&mut ui, false);
        shot(&s.canvas, "30-answer-tables");
        let Kind::Viewer(v) = &ui.layers[0].kind else { panic!() };
        let lines = &v.pages[0];
        let text: String = lines.iter().flat_map(|l| l.runs.iter().map(|r| r.text.clone())).collect::<Vec<_>>().join(" ");
        assert!(!text.contains(" | ") && !text.contains("---"), "no bar-separated fallback: {text}");
        let mut right = 0f32;
        for r in lines.iter().flat_map(|l| l.runs.iter()) {
            right = right.max(r.x + s.fonts.text_width(r.style, &r.text, r.px));
        }
        let group_w = v.groups.iter().map(|g| g.w).fold(0f32, f32::max);
        assert!(right <= group_w, "text stays inside the card: {right} > {group_w}");
        // The prose table wraps: the row of "Fate" takes several lines, its cells side by side.
        let fate = lines.iter().position(|l| l.runs.iter().any(|r| r.text == "Fate")).expect("Fate row");
        let xs: std::collections::BTreeSet<i32> = lines[fate].runs.iter().map(|r| r.x as i32).collect();
        assert!(xs.len() >= 3, "three columns on the row's first line: {:?}", lines[fate].runs);
        assert!(lines[fate + 1].runs.iter().all(|r| r.x > lines[fate].runs[0].x + 50.0), "the next line continues the other columns");
        assert!(lines.iter().any(|l| l.hairline), "rows of a wrapped table are ruled");
    }

    fn juliet(dir: &Path) -> (PathBuf, Box<dyn Document>) {
        let book = dir.join("juliet.epub");
        std::fs::copy(repo("testdata/juliet.epub"), &book).unwrap();
        let doc = crate::screens::reader::open_book(&crate::app::providers(), &Settings::default(), &book, W, H).unwrap();
        (book, doc)
    }

    fn compose(s: &mut Screen, ui: &mut PluginUi, changed: &[Rect]) -> Vec<Damage> {
        let mut cx = PaintCx { canvas: &mut s.canvas, fonts: &mut s.fonts, theme: &s.theme };
        ui.compose(&mut cx, changed)
    }

    /// A new label for one row of an open menu repaints that row only: one GL16 damage
    /// rectangle the height of a row; a label that did not change, a wrong action and a
    /// wrong menu id do nothing.
    #[test]
    fn menu_label_repaints_one_row() {
        let mut s = Screen::new();
        s.canvas.clear(255);
        let mut ui = PluginUi::default();
        let items = vec![("Not connected".to_string(), "status".to_string()), ("Connect".to_string(), "connect".to_string()), ("Server".to_string(), "server".to_string())];
        ui.push("p", UiRequest::Menu { id: 7, title: "Calibre".into(), items });
        let first = compose(&mut s, &mut ui, &[]);
        assert_eq!(first.len(), 1, "{first:?}");
        let menu = first[0].rect;
        let before = s.canvas.crop(menu);
        for req in [
            UiRequest::MenuLabel { id: 7, action: "status".into(), label: "Not connected".into() },
            UiRequest::MenuLabel { id: 7, action: "nope".into(), label: "x".into() },
            UiRequest::MenuLabel { id: 8, action: "status".into(), label: "x".into() },
        ] {
            ui.push("p", req);
        }
        ui.push("other", UiRequest::MenuLabel { id: 7, action: "status".into(), label: "x".into() });
        assert!(compose(&mut s, &mut ui, &[]).is_empty(), "nothing changed, nothing repainted");
        ui.push("p", UiRequest::MenuLabel { id: 7, action: "status".into(), label: "Connected to host".into() });
        let d = compose(&mut s, &mut ui, &[]);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].class, RefreshClass::Gl16);
        assert_eq!(d[0].rect, Rect::new(menu.x + 4, menu.y + TITLE_H as i32, menu.w - 8, ROW), "the first row only");
        assert_ne!(s.canvas.crop(menu).pixels, before.pixels, "the row has new text");
        assert_eq!(ui.pending(), 0);
        // The second row has the same height and sits right under the first.
        ui.push("p", UiRequest::MenuLabel { id: 7, action: "server".into(), label: "Server\tauto".into() });
        let d = compose(&mut s, &mut ui, &[]);
        assert_eq!(d.iter().map(|d| d.rect).collect::<Vec<_>>(), vec![Rect::new(menu.x + 4, menu.y + (TITLE_H + 2 * ROW) as i32, menu.w - 8, ROW)]);
        // A menu still waiting behind another modal layer takes the label too.
        ui.push("p", UiRequest::Dialog { id: 9, title: "T".into(), body: "B".into(), buttons: vec![("ok".into(), "OK".into())] });
        ui.push("q", UiRequest::Menu { id: 1, title: "M".into(), items: vec![("a".into(), "A".into())] });
        ui.push("q", UiRequest::MenuLabel { id: 1, action: "A".into(), label: "b".into() });
        assert_eq!(ui.pending(), 2);
    }

    /// The screen repaints part of the canvas under an open dialog (and a toast over
    /// it): the dialog is drawn again on top, and closing it puts back the new screen
    /// pixels, not the old ones.
    #[test]
    fn compose_follows_partial_repaints_under_layers() {
        let mut s = Screen::new();
        s.canvas.clear(200);
        let mut ui = PluginUi::default();
        assert!(compose(&mut s, &mut ui, &[]).is_empty(), "no layers: no damage");
        ui.push("p", UiRequest::Dialog { id: 1, title: "T".into(), body: "B".into(), buttons: vec![("ok".into(), "OK".into())] });
        let d = compose(&mut s, &mut ui, &[]);
        assert!(!d.is_empty());
        let r = Dialog::frame(screen());
        let dialog_px = s.canvas.crop(r);
        // Nothing changed: no damage.
        assert!(compose(&mut s, &mut ui, &[]).is_empty());
        // The screen repaints a band across the dialog (as a page footer would).
        let band = Rect::new(0, r.y + 10, W, 40);
        s.canvas.fill_rect(band.x, band.y, band.w as i32, band.h as i32, 50);
        let d = compose(&mut s, &mut ui, &[band]);
        assert!(d.iter().any(|d| !d.rect.intersect(&r).is_empty()), "{d:?}");
        assert_eq!(s.canvas.crop(r).pixels, dialog_px.pixels, "dialog drawn again on top");
        // Close it (back key): the band is there under it, the rest is the old background.
        let (used, answers) = ui.handle(&key(Key::Back), screen());
        assert!(used && answers.len() == 1);
        compose(&mut s, &mut ui, &[]);
        let mut want = Canvas::new(W as usize, H as usize);
        want.clear(200);
        want.fill_rect(band.x, band.y, band.w as i32, band.h as i32, 50);
        assert!(s.canvas.pixels == want.pixels, "restored pixels are the new screen pixels");
    }

    /// A toast over a dialog; the screen repaints under both; closing both leaves the
    /// new screen.
    #[test]
    fn compose_with_two_layers() {
        let mut s = Screen::new();
        s.canvas.clear(255);
        let mut ui = PluginUi::default();
        ui.push("p", UiRequest::Menu { id: 2, title: "M".into(), items: vec![("a".into(), "A".into()), ("b".into(), "B".into())] });
        compose(&mut s, &mut ui, &[]);
        ui.push("p", UiRequest::Toast { text: "hello".into() });
        compose(&mut s, &mut ui, &[]);
        assert_eq!(ui.layer_names(), vec!["menu", "toast"]);
        // A band across both (not the whole canvas).
        let band = Rect::new(0, 0, W, H - 1);
        s.canvas.fill_rect(0, 0, W as i32, H as i32 - 1, 120);
        compose(&mut s, &mut ui, &[band]);
        assert_eq!(ui.layer_names(), vec!["menu", "toast"]);
        // A key closes the toast, the back key the menu.
        ui.handle(&key(Key::Back), screen());
        compose(&mut s, &mut ui, &[]);
        assert!(ui.layer_names().is_empty());
        let mut want = Canvas::new(W as usize, H as usize);
        want.clear(255);
        want.fill_rect(0, 0, W as i32, H as i32 - 1, 120);
        let diff = s.canvas.pixels.iter().zip(&want.pixels).filter(|(a, b)| a != b).count();
        assert_eq!(diff, 0);
        // A whole-canvas repaint under a layer.
        ui.push("p", UiRequest::Toast { text: "again".into() });
        compose(&mut s, &mut ui, &[]);
        s.canvas.clear(30);
        let all = s.canvas.bounds();
        compose(&mut s, &mut ui, &[all]);
        ui.handle(&key(Key::Back), screen());
        compose(&mut s, &mut ui, &[]);
        assert!(s.canvas.pixels.iter().all(|&p| p == 30));
    }

    /// A paged card as a plugin layer (typed lookup): a swipe on it answers `next`, the
    /// plugin's next page opens in the same rectangle with one update, and a swipe
    /// outside it is used (it never reaches the screen). A screen can take a card dialog
    /// out of the queue; toasts stay queued.
    #[test]
    fn paged_card_layer_swipes_and_take_card() {
        let mut s = Screen::new();
        s.canvas.clear(255);
        let mut ui = PluginUi::default();
        let card = |page: usize| UiRequest::Dialog {
            id: 1,
            title: "word".into(),
            body: format!("_noun_\ndefinition {page}\n— {page}/2 · Dict {page}"),
            buttons: vec![("Close".into(), "close".into()), ("Add".into(), "add".into())],
        };
        ui.push("dictionary", card(1));
        s.ui(&mut ui, false);
        assert_eq!(ui.layer_names(), vec!["dialog"]);
        let r = ui.layers[0].rect;
        let swipe = |x: i32, y: i32| UiEvent::Gesture(Gesture::Swipe { from: Point::new(x, y), to: Point::new(x - 400, y), dir: ink_ui::Direction::Left });
        // Outside the card: used, no answer.
        assert_eq!(ui.handle(&swipe(900, 100), screen()), (true, Vec::new()));
        let (used, answers) = ui.handle(&swipe(r.x + 900, r.y + 200), screen());
        assert!(used);
        assert_eq!(answers, vec![("dictionary".to_string(), 1, "next".to_string())]);
        ui.push("dictionary", card(2));
        let damage = s.ui(&mut ui, false);
        assert_eq!(ui.layer_names(), vec!["dialog"]);
        assert_eq!(ui.layers[0].rect, r, "a paged card keeps its rectangle");
        assert_eq!(damage, vec![Damage { rect: r, class: RefreshClass::Gl16 }], "one update for the new page");
        // take_card: the card leaves the queue, the toast stays.
        ui.push("dictionary", UiRequest::Toast { text: "t".into() });
        ui.push("dictionary", card(1));
        assert!(ui.take_card("other").is_none());
        let (id, c) = ui.take_card("dictionary").unwrap();
        assert_eq!((id, c.pages), (1, Some((1, 2))));
        assert_eq!(ui.pending(), 1);
    }

    /// The hook API on the sample plugin: registry entries and running them.
    #[test]
    fn hook_entries_and_run_entry() {
        let root = tmp("hooks");
        let paths = Paths { root: root.clone() };
        install_sample(&paths.plugins());
        let mut host = PluginHost::load(Vec::new(), &Settings::default(), &paths);
        let menu = hooks::entries(&host, EntryKind::ReaderMenu);
        assert_eq!(menu.len(), 2, "{menu:?}");
        assert!(menu.iter().all(|e| e.plugin == "sample-lua" && e.kind == EntryKind::ReaderMenu));
        let pages = hooks::entries(&host, EntryKind::Settings);
        assert_eq!(pages[0].label, "Reading timer");
        assert!(hooks::entries(&host, EntryKind::Library).len() == host.library_actions().len());
        // Settings entry opens the plugin's menu.
        let host_ref = &mut host;
        let mut canvas = Canvas::new(W as usize, H as usize);
        let mut fonts = Fonts::load();
        let theme = Theme::day();
        let mut refresh = ink_ui::RefreshScheduler::new(6);
        let (mut dev, _probe) = ink_device::headless::open(ink_device::headless::HeadlessOptions::default());
        let settings = Settings::default();
        let mut cx = crate::router::AppCtx {
            canvas: &mut canvas,
            screen: dev.screen.as_mut(),
            fonts: &mut fonts,
            theme: &theme,
            refresh: &mut refresh,
            plugins: host_ref,
            settings: &settings,
            paths: &paths,
            providers: &[],
            services: crate::router::Services {
                info: &dev.info,
                light: dev.light.as_mut(),
                battery: dev.battery.as_mut(),
                power: dev.power.as_mut(),
                wifi: dev.wifi.as_mut(),
                usb: dev.usb.as_mut(),
                clock: dev.clock.as_mut(),
            },
            settings_update: &mut None,
        };
        assert!(hooks::run_entry(&mut cx, &pages[0], None, None));
        assert_eq!(cx.plugins.ui.pending(), 1);
        let lib = PluginEntry { plugin: "sample-lua".into(), id: "x".into(), label: "x".into(), kind: EntryKind::Library };
        assert!(!hooks::run_entry(&mut cx, &lib, None, None), "library entry needs a book");
        assert_eq!(hooks::app_timeout(Some(Duration::from_secs(5)), cx.plugins), Some(Duration::ZERO), "pending request: paint now");
        let _ = std::fs::remove_dir_all(root);
    }

    struct Pager(PluginManifest);
    impl Plugin for Pager {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn on_page_change(&mut self, cx: &mut HostCtx, page: usize) -> Result<()> {
            let pos = cx.document()?.current_position()?;
            cx.ui()?.toast(&format!("{page} {pos}"));
            Ok(())
        }
    }

    #[test]
    fn page_changes_reach_plugins_with_document_access() {
        let p = Pager(PluginManifest::new("pager", "1", &[Permission::Document, Permission::Ui]));
        let mut host = PluginHost::new(vec![Box::new(p)]);
        let book = repo("testdata/juliet.epub");
        let mut doc = ink_formats::open(&book).unwrap();
        doc.set_layout(&ink_doc::LayoutSettings::for_screen(W, H)).unwrap();
        host.book_open(&mut doc, &book);
        doc.set_current_page(3);
        host.page_change(&mut doc, 3);
        let reqs = host.take_ui_requests();
        assert_eq!(reqs.len(), 1);
        assert!(matches!(&reqs[0], UiRequest::Toast { text } if text.starts_with("3 /body/DocFragment[")), "{reqs:?}");
        host.book_close(&mut doc);
        assert!(host.manager.errors.is_empty(), "{:?}", host.manager.errors);
    }

    struct Mover(PluginManifest);

    impl Plugin for Mover {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn on_action(&mut self, cx: &mut HostCtx, id: &str) -> Result<()> {
            match id {
                "go" => cx.ui()?.go_to("page:4"),
                _ => cx.ui()?.rescan_library(),
            }
            Ok(())
        }
    }

    /// Wants ticks until it got three; each tick toasts.
    struct Ticker(PluginManifest, u32);

    impl Plugin for Ticker {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn tick_interval(&self) -> Option<Duration> {
            (self.1 < 3).then_some(Duration::from_millis(100))
        }
        fn on_tick(&mut self, cx: &mut HostCtx) -> Result<()> {
            self.1 += 1;
            cx.ui()?.toast(&format!("tick {}", self.1));
            Ok(())
        }
    }

    #[test]
    fn ticks_run_from_poll_and_shape_the_timeout() {
        let mut host = PluginHost::new(vec![Box::new(Ticker(PluginManifest::new("ticker", "1", &[Permission::Ui]), 0))]);
        assert_eq!(host.timeout(), Some(Duration::ZERO), "first tick due at once");
        host.poll(None);
        host.poll(None);
        assert_eq!(host.take_ui_requests().len(), 1, "the second poll came before the interval");
        let t = host.timeout().unwrap();
        assert!(t > Duration::from_millis(50) && t <= Duration::from_millis(100), "{t:?}");
        for _ in 0..2 {
            std::thread::sleep(Duration::from_millis(110));
            host.poll(None);
        }
        assert_eq!(host.take_ui_requests().len(), 2);
        assert_eq!(host.timeout(), None, "no ticks wanted any more");
        // Sleeping: no ticks.
        let mut host = PluginHost::new(vec![Box::new(Ticker(PluginManifest::new("ticker", "1", &[Permission::Ui]), 0))]);
        host.suspend(None);
        host.poll(None);
        assert_eq!(host.timeout(), None);
        assert!(host.manager.errors.is_empty(), "{:?}", host.manager.errors);
    }

    #[test]
    fn go_to_and_rescan_requests_wait_for_the_screens() {
        let mut host = PluginHost::new(vec![Box::new(Mover(PluginManifest::new("mover", "1", &[Permission::Ui])))]);
        // No book open: a jump is dropped.
        host.trigger("mover", "go", None);
        assert_eq!(host.take_goto(), None);
        host.trigger("mover", "rescan", None);
        assert!(host.take_rescan());
        assert!(!host.take_rescan(), "once");
        let book = repo("testdata/juliet.epub");
        let mut doc = ink_formats::open(&book).unwrap();
        doc.set_layout(&ink_doc::LayoutSettings::for_screen(W, H)).unwrap();
        host.book_open(&mut doc, &book);
        host.trigger("mover", "go", Some(&mut doc));
        assert_eq!(host.take_goto().as_deref(), Some("page:4"));
        assert_eq!(host.take_goto(), None);
        assert!(host.take_ui_requests().is_empty(), "not shown by the presenter");
    }

    /// Writes a Lua plugin folder.
    fn lua_plugin(plugins: &Path, name: &str, toml_extra: &str, main: &str) {
        let d = plugins.join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("plugin.toml"), format!("name = \"{name}\"\nversion = \"0.1\"\n{toml_extra}\n")).unwrap();
        std::fs::write(d.join("main.lua"), main).unwrap();
    }

    fn install_sample(plugins: &Path) {
        let d = plugins.join("sample-lua");
        std::fs::create_dir_all(&d).unwrap();
        for f in ["plugin.toml", "main.lua"] {
            std::fs::copy(repo(&format!("plugins/sample-lua/{f}")), d.join(f)).unwrap();
        }
    }

    /// The sample plugin and two broken ones from a plugin folder on disk, on Juliet,
    /// through the public PluginHost API (what the reader will call), with screenshots.
    #[test]
    fn sample_plugin_from_disk_on_juliet() {
        let root = tmp("sample");
        let paths = Paths { root: root.clone() };
        install_sample(&paths.plugins());
        lua_plugin(&paths.plugins(), "broken", "", "return {");
        lua_plugin(&paths.plugins(), "looper", "time_limit_ms = 50", "return { on_page_change = function() while true do end end }");
        lua_plugin(&paths.plugins(), "Wrong", "", "return {}");
        std::fs::write(paths.settings(), "[plugins.sample-lua]\nshow_timer = false\n").unwrap();
        let settings = Settings::load(&paths.settings()).unwrap();
        let mut host = PluginHost::load(Vec::new(), &settings, &paths);
        let names: Vec<(String, bool)> = host.manager.list();
        assert_eq!(names, vec![("looper".to_string(), true), ("sample-lua".to_string(), true)]);
        let not_loaded: Vec<&str> = host.manager.errors.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(not_loaded, vec!["Wrong", "broken"], "{:?}", host.manager.errors);
        assert_eq!(host.menu_items().len(), 2);
        assert_eq!(host.settings_pages()[0].title, "Reading timer");

        let (book, mut doc) = juliet(&root);
        let mut s = Screen::new();
        host.book_open(doc.as_mut(), &book);
        for page in [5usize, 6] {
            doc.set_current_page(page);
            s.page(doc.as_mut(), page);
            host.page_change(doc.as_mut(), page);
            s.overlay(&mut host, true);
        }
        // The looper hit its limit on the first page turn and was disabled; the reader went on.
        assert!(!host.manager.is_enabled("looper"));
        assert!(host.notifications().iter().any(|n| n.text.contains("looper") && n.text.contains("time limit")), "{:?}", host.notifications());
        assert!(host.unread_notifications() >= 3);
        // show_timer = false from settings.toml: the status widget is empty.
        assert_eq!(host.status_line(StatusSlot::Right), "");
        shot(&s.canvas, "01-error-toast");

        // Reader menu entry: the Markdown viewer with the stats.
        assert!(host.trigger("sample-lua", "stats", Some(doc.as_mut())));
        s.overlay(&mut host, false);
        assert_eq!(host.ui.layer_names(), vec!["viewer"]);
        shot(&s.canvas, "02-stats-viewer");
        assert!(host.handle_event(&tap(W as i32 - 120, 60), Some(doc.as_mut()), screen()));
        assert!(!host.modal_open());
        s.overlay(&mut host, false);

        // Settings page: menu, toggle the timer on.
        let page = host.settings_pages()[0].clone();
        assert!(host.open_settings_page(&page, Some(doc.as_mut())));
        s.overlay(&mut host, false);
        shot(&s.canvas, "03-settings-menu");
        let menu = host.ui.layers[0].rect;
        assert!(host.handle_event(&tap(menu.x + 100, menu.y + TITLE_H as i32 + 50), Some(doc.as_mut()), screen()));
        assert!(host.status_line(StatusSlot::Right).starts_with("Read "), "{}", host.status_line(StatusSlot::Right));
        let saved = std::fs::read_to_string(paths.plugins().join("sample-lua/settings.json")).unwrap();
        assert!(saved.contains("\"show_timer\": true"), "{saved}");
        s.overlay(&mut host, false);
        shot(&s.canvas, "04-toast-timer-on");

        // Reset through the dialog.
        assert!(host.open_settings_page(&page, Some(doc.as_mut())));
        s.overlay(&mut host, false);
        let menu = host.ui.layers.iter().find(|l| matches!(l.kind, Kind::Menu(_))).unwrap().rect;
        host.handle_event(&tap(menu.x + 100, menu.y + TITLE_H as i32 + 160), Some(doc.as_mut()), screen());
        s.overlay(&mut host, false);
        assert_eq!(host.ui.layer_names(), vec!["dialog"]);
        shot(&s.canvas, "05-reset-dialog");
        let d = Dialog::frame(screen());
        // Second of two buttons ("Reset").
        let bw = (d.w - 48) / 2;
        host.handle_event(&tap(d.x + 24 + bw as i32 + 40, d.bottom() - 24 - 50), Some(doc.as_mut()), screen());
        assert!(host.notifications().is_empty() || !host.modal_open());

        // Note through the keyboard.
        s.overlay(&mut host, false);
        assert!(host.trigger("sample-lua", "note", Some(doc.as_mut())));
        s.overlay(&mut host, false);
        assert_eq!(host.ui.layer_names(), vec!["input"]);
        let input = host.ui.layers[0].rect;
        let (_, _, _, kb) = InputBox::rects(input);
        let layout = KeyboardLayout::qwerty_en();
        let area = Keyboard::new().keys_area(kb);
        for ch in ["n", "i", "c", "e"] {
            let (r, _) = layout.key_rects(area).into_iter().find(|(_, k)| k.label == ch).unwrap();
            for ev in press(r.x + 10, r.y + 10) {
                host.handle_event(&ev, Some(doc.as_mut()), screen());
            }
        }
        s.overlay(&mut host, false);
        shot(&s.canvas, "06-note-keyboard");
        let (_, _, ok, _) = InputBox::rects(input);
        host.handle_event(&tap(ok.x + 10, ok.y + 10), Some(doc.as_mut()), screen());
        let notes = std::fs::read_to_string(paths.plugins().join("sample-lua/data/notes.md")).unwrap();
        assert!(notes.trim_end().ends_with(": nice"), "{notes}");

        // Gesture and selection action, suspend and resume, close.
        assert!(host.gesture("two_finger_tap", Some(doc.as_mut())));
        assert!(!host.gesture("pinch_in", Some(doc.as_mut())));
        host.suspend(Some(doc.as_mut()));
        host.resume(Some(doc.as_mut()));
        doc.set_current_page(7);
        host.page_change(doc.as_mut(), 7);
        host.book_close(doc.as_mut());
        let state = BookState::load(&book).unwrap();
        assert_eq!(state.plugins["sample-lua"]["pages"], 1, "{:?}", state.plugins);
        let csv = std::fs::read_to_string(paths.plugins().join("sample-lua/data/sessions.csv")).unwrap();
        assert!(csv.contains("\"Romeo and Juliet\""), "{csv}");
        // Library action on the closed book reads its sidecar.
        let action = host.library_actions()[0].clone();
        assert!(host.library_action(&action, &book));
        assert!(matches!(host.ui.pending.back(), Some((_, UiRequest::Toast { text })) if text.starts_with("juliet.epub: 0 min, 1 pages")), "{:?}", host.ui.pending);
        let errors: Vec<_> = host.manager.errors.iter().filter(|(n, _)| n == "sample-lua").collect();
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn overlay_layers_show_answer_and_restore() {
        let root = tmp("overlay");
        let (_, mut doc) = juliet(&root);
        let mut s = Screen::new();
        s.page(doc.as_mut(), 4);
        let page = s.canvas.clone();
        let mut ui = PluginUi { toast_time: Duration::from_millis(30), ..PluginUi::default() };

        // Toast: drawn, then gone after its time, pixels back.
        ui.push("p", UiRequest::Toast { text: "A toast from a plugin with a long text that does not fit on one line at all".into() });
        let d = s.ui(&mut ui, false);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].class, RefreshClass::Du);
        assert_ne!(s.canvas.pixels, page.pixels);
        shot(&s.canvas, "10-toast");
        std::thread::sleep(Duration::from_millis(40));
        assert!(ui.timeout().is_some());
        ui.handle(&UiEvent::Timer(0), screen());
        let d = s.ui(&mut ui, false);
        assert_eq!(s.canvas.pixels, page.pixels, "toast area restored");
        assert!(!d.is_empty() && d.iter().all(|d| d.class == RefreshClass::Gl16), "toast close is GL16, not DU: {d:?}");

        // Dialog: the button answers, pixels back.
        ui.push("p", UiRequest::Dialog { id: 7, title: "Delete note?".into(), body: "The note is removed from this book.".into(), buttons: vec![("Cancel".into(), "cancel".into()), ("Delete".into(), "delete".into())] });
        s.ui(&mut ui, false);
        shot(&s.canvas, "11-dialog");
        let f = Dialog::frame(screen());
        assert_eq!(ui.handle(&tap(f.x + 5, f.y + 5), screen()), (true, vec![]), "taps beside the buttons are swallowed");
        let bw = (f.w - 48) / 2;
        let (used, answers) = ui.handle(&tap(f.x + 24 + bw as i32 + 40, f.bottom() - 24 - 50), screen());
        assert!(used);
        assert_eq!(answers, vec![("p".to_string(), 7, "delete".to_string())]);
        s.ui(&mut ui, false);
        assert_eq!(s.canvas.pixels, page.pixels);

        // A long menu: the settings popup's list jumps on a swipe; a tap outside cancels.
        let items: Vec<(String, String)> = (1..=30).map(|i| (format!("Item {i}"), format!("a{i}"))).collect();
        ui.push("q", UiRequest::Menu { id: 3, title: "Long menu".into(), items });
        s.ui(&mut ui, false);
        shot(&s.canvas, "12-menu");
        let r = ui.layers[0].rect;
        assert!(r.h <= H * 4 / 5 && r.w <= popup::MAX_W, "{r:?}");
        let mid = r.y + r.h as i32 / 2;
        let up = UiEvent::Gesture(Gesture::Swipe { from: Point::new(r.x + 300, mid + 200), to: Point::new(r.x + 300, mid - 100), dir: Direction::Up });
        assert_eq!(ui.handle(&up, screen()), (true, vec![]));
        let Kind::Menu(m) = &ui.layers[0].kind else { panic!() };
        let scroll = m.scroll();
        assert!(scroll > 0);
        assert_eq!(ui.layers[0].dirty, Some(RefreshClass::Du), "a list jump is one DU");
        s.ui(&mut ui, false);
        shot(&s.canvas, "13-menu-page2");
        // The first row shown is the item after the ones scrolled past.
        let (_, answers) = ui.handle(&tap(r.x + 50, r.y + TITLE_H as i32 + 50), screen());
        assert_eq!(answers[0].2, format!("a{}", scroll / ROW as i32 + 1));
        s.ui(&mut ui, false);
        assert_eq!(s.canvas.pixels, page.pixels);
        ui.push("q", UiRequest::Menu { id: 4, title: "Short".into(), items: vec![("One".into(), "1".into())] });
        s.ui(&mut ui, false);
        assert_eq!(ui.handle(&tap(5, 5), screen()).1, vec![("q".to_string(), 4, String::new())]);
        s.ui(&mut ui, false);
        assert_eq!(s.canvas.pixels, page.pixels);
        // The visible Close button in the title row cancels the same way.
        ui.push("q", UiRequest::Menu { id: 5, title: "Short".into(), items: vec![("One".into(), "1".into())] });
        s.ui(&mut ui, false);
        let c = MenuBox::close_rect(ui.layers[0].rect);
        assert!(c.w >= 96 && c.h >= 96);
        assert_eq!(ui.handle(&tap(c.x + c.w as i32 / 2, c.y + c.h as i32 / 2), screen()).1, vec![("q".to_string(), 5, String::new())]);
        s.ui(&mut ui, false);
        assert_eq!(s.canvas.pixels, page.pixels);

        // Markdown viewer: pages, links, close; a toast on top while it is open.
        let mut md = String::from("# Viewer test\n\nA paragraph with **bold**, *italic*, `code` and a [link](https://example.org/x).\n\n## Lists\n\n- one\n- two\n  - nested\n1. first\n2. second\n\n> A quote that wraps over more than one line because it is long enough to do so.\n\n```\nfn main() {}\n```\n\n| Col A | Col B |\n|---|---|\n| 1 | 2 |\n\n---\n\n");
        for i in 0..60 {
            md.push_str(&format!("Paragraph {i} with some text to fill the pages of the viewer.\n\n"));
        }
        ui.push("v", UiRequest::TextViewer { id: 9, title: "Viewer".into(), markdown: md.clone() });
        let d = s.ui(&mut ui, false);
        assert_eq!(d[0].class, RefreshClass::Reagl, "a full-screen viewer opens without a flash");
        shot(&s.canvas, "14-viewer");
        let Kind::Viewer(v) = &ui.layers[0].kind else { panic!() };
        assert!(v.page_count() >= 3, "{}", v.page_count());
        let (link, url) = v.links[0].clone();
        assert_eq!(url, "https://example.org/x");
        let (_, answers) = ui.handle(&tap(link.x + 5, link.y + 5), screen());
        assert_eq!(answers, vec![("v".to_string(), 9, url)]);
        assert_eq!(ui.layer_names(), vec!["viewer"], "a link keeps the viewer open");
        ui.handle(&tap(1000, 900), screen());
        ui.push("v", UiRequest::Toast { text: "Page 2".into() });
        s.ui(&mut ui, false);
        assert_eq!(ui.layer_names(), vec!["viewer", "toast"]);
        shot(&s.canvas, "15-viewer-page2-toast");
        // Streaming: the same id replaces the text.
        ui.push("v", UiRequest::TextViewer { id: 9, title: "Viewer".into(), markdown: "# Replaced\n\nShort.".into() });
        assert_eq!(ui.pending(), 0);
        // The tap closes the toast first, then reaches the viewer's Close button.
        let (_, answers) = ui.handle(&tap(W as i32 - 120, 60), screen());
        assert_eq!(answers, vec![("v".to_string(), 9, "closed".to_string())]);
        s.ui(&mut ui, false);
        assert_eq!(s.canvas.pixels, page.pixels);

        // Keyboard input: type, delete, OK; back key cancels.
        ui.push("k", UiRequest::TextInput { id: 1, prompt: "Ask AI".into(), initial: "Hi".into() });
        s.ui(&mut ui, false);
        let input = ui.layers[0].rect;
        let (_, _, _, kb) = InputBox::rects(input);
        for label in ["space", "t", "o", "p", "del"] {
            let (r, _) = KeyboardLayout::qwerty_en().key_rects(Keyboard::new().keys_area(kb)).into_iter().find(|(_, k)| k.label == label).unwrap();
            for ev in press(r.x + 10, r.y + 10) {
                ui.handle(&ev, screen());
            }
        }
        let d = s.ui(&mut ui, false);
        // Key faces and the field only, gray-preserving.
        assert!(d.iter().all(|d| d.class == RefreshClass::Gl16 && d.rect.h < input.h / 2), "{d:?}");
        shot(&s.canvas, "16-keyboard");
        let (_, _, ok, _) = InputBox::rects(input);
        assert_eq!(ui.handle(&tap(ok.x + 5, ok.y + 5), screen()).1, vec![("k".to_string(), 1, "Hi to".to_string())]);
        s.ui(&mut ui, false);
        assert_eq!(s.canvas.pixels, page.pixels);
        ui.push("k", UiRequest::TextInput { id: 2, prompt: "x".into(), initial: String::new() });
        s.ui(&mut ui, false);
        assert_eq!(ui.handle(&key(Key::Back), screen()).1, vec![("k".to_string(), 2, String::new())]);
        assert_eq!(ui.handle(&key(Key::Power), screen()), (false, vec![]), "power goes to the screen");

        // Nobody paints: the queue stays bounded, toasts are dropped first.
        let mut idle = PluginUi::default();
        idle.push("x", UiRequest::Dialog { id: 1, title: "kept".into(), body: String::new(), buttons: vec![] });
        for i in 0..200 {
            idle.push("x", UiRequest::Toast { text: format!("t{i}") });
        }
        assert_eq!(idle.pending(), MAX_PENDING);
        assert!(matches!(idle.pending.front(), Some((_, UiRequest::Dialog { .. }))));

        // Two modal requests: the second waits for the first.
        ui.push("a", UiRequest::Dialog { id: 1, title: "A".into(), body: String::new(), buttons: vec![("OK".into(), "ok".into())] });
        ui.push("b", UiRequest::Dialog { id: 2, title: "B".into(), body: String::new(), buttons: vec![("OK".into(), "ok".into())] });
        s.ui(&mut ui, false);
        assert_eq!((ui.layer_names(), ui.pending()), (vec!["dialog"], 1));
        let f = Dialog::frame(screen());
        assert_eq!(ui.handle(&tap(f.x + 60, f.bottom() - 70), screen()).1[0].0, "a");
        s.ui(&mut ui, false);
        assert_eq!(ui.handle(&tap(f.x + 60, f.bottom() - 70), screen()).1[0].0, "b");
        s.ui(&mut ui, false);
        assert_eq!(s.canvas.pixels, page.pixels);

        // The screen below redraws while a dialog is open: the dialog is drawn again on the
        // new page and closing it shows the new page.
        ui.push("a", UiRequest::Dialog { id: 5, title: "Over a page turn".into(), body: String::new(), buttons: vec![("OK".into(), "ok".into())] });
        s.ui(&mut ui, false);
        s.page(doc.as_mut(), 5);
        let page5 = s.canvas.clone();
        s.ui(&mut ui, true);
        ui.handle(&tap(f.x + 60, f.bottom() - 70), screen());
        s.ui(&mut ui, false);
        assert_eq!(s.canvas.pixels, page5.pixels);
    }

    /// The bisecting `fit` and `fit_end` cut where trimming one character at a time did.
    #[test]
    fn fitting_cuts_where_trimming_one_character_at_a_time_did() {
        let mut canvas = Canvas::new(8, 8);
        let mut fonts = Fonts::load();
        let theme = Theme::day();
        let mut cx = PaintCx { canvas: &mut canvas, fonts: &mut fonts, theme: &theme };
        let px = 40.0;
        let slow_fit = |cx: &mut PaintCx, text: &str, max: f32| {
            if cx.fonts.text_width(Style::Regular, text, px) <= max {
                return text.to_string();
            }
            let mut s = text.to_string();
            while !s.is_empty() && cx.fonts.text_width(Style::Regular, &format!("{s}..."), px) > max {
                s.pop();
            }
            format!("{}...", s.trim_end())
        };
        let slow_end = |cx: &mut PaintCx, text: &str, max: f32| {
            let mut s = text.to_string();
            while !s.is_empty() && cx.fonts.text_width(Style::Regular, &s, px) > max {
                s.remove(0);
            }
            s
        };
        for text in ["", "a", "Household words", "Ünïcödé — ✓ wide WWWW iiii", "word word word word word word word word"] {
            for max in [-10.0, 0.0, 5.0, 30.0, 61.0, 150.0, 333.0, 1000.0] {
                assert_eq!(fit(&mut cx, text, px, Style::Regular, max), slow_fit(&mut cx, text, max), "fit {text:?} {max}");
                assert_eq!(fit_end(&mut cx, text, px, max), slow_end(&mut cx, text, max), "fit_end {text:?} {max}");
            }
        }
    }

    /// A word wider than the line starts a line of its own and is cut into pieces that
    /// each fit their line.
    #[test]
    fn long_words_break_into_pieces_that_fit() {
        let mut canvas = Canvas::new(8, 8);
        let mut fonts = Fonts::load();
        let theme = Theme::day();
        let mut cx = PaintCx { canvas: &mut canvas, fonts: &mut fonts, theme: &theme };
        let (px, width) = (40.0, 600.0);
        let word = "https://example.org/a/very/long/link/that/does/not/fit/on/one/line/at/all";
        let mut lines = Vec::new();
        let mut widths = WordWidths::default();
        wrap_spans(&mut Lay { cx: &mut cx, widths: &mut widths }, &[Span::plain(&format!("See {word} now"))], px, None, 0.0, width, None, &mut lines);
        let runs: Vec<&Run> = lines.iter().flat_map(|l| &l.runs).collect();
        let pieces: String = runs.iter().filter(|r| r.text != "See" && r.text != "now").map(|r| r.text.as_str()).collect();
        assert_eq!(pieces, word);
        assert!(lines.len() >= 3, "{lines:?}");
        for r in &runs {
            assert!(r.x + cx.fonts.shaped_width(r.style, px, &r.text) <= width, "{r:?}");
        }
        assert_eq!((lines[0].runs.len(), runs[1].x), (1, 0.0), "the word starts a new line");
    }

    /// A streamed update lays the viewer out again from the widths it measured; other
    /// fonts start the widths again.
    #[test]
    fn viewer_word_widths_are_kept_across_layouts_of_one_font() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        let text = "Some words of an answer, and some more words of it.";
        ui.push("ai", UiRequest::TextViewer { id: 3, title: "Chat".into(), markdown: text.into() });
        s.ui(&mut ui, false);
        let entries = |ui: &PluginUi| match &ui.layers[0].kind {
            Kind::Viewer(v) => v.widths.entries,
            _ => unreachable!(),
        };
        let first = entries(&ui);
        assert!(first > 5, "{first}");
        ui.push("ai", UiRequest::TextViewer { id: 3, title: "Chat".into(), markdown: format!("{text} words") });
        s.ui(&mut ui, false);
        assert_eq!(entries(&ui), first, "no word measured again");
        let mut w = WordWidths::default();
        w.for_fonts("A".into(), false);
        w.entries = 3;
        w.for_fonts("A".into(), false);
        assert_eq!(w.entries, 3);
        w.for_fonts("B".into(), false);
        assert_eq!(w.entries, 0, "another family");
    }

    #[test]
    fn gesture_names() {
        let g = |g: Gesture| gesture_name(&g, screen());
        let p = Point::new;
        assert_eq!(g(Gesture::TwoFingerTap { pos: p(600, 800) }).as_deref(), Some("two_finger_tap"));
        assert_eq!(g(Gesture::LongPress { pos: p(20, 20), held: Duration::from_secs(1) }).as_deref(), Some("corner_hold:top_left"));
        assert_eq!(g(Gesture::LongPress { pos: p(1250, 1680), held: Duration::from_secs(1) }).as_deref(), Some("corner_hold:bottom_right"));
        assert_eq!(g(Gesture::LongPress { pos: p(600, 800), held: Duration::from_secs(1) }).as_deref(), Some("long_press"));
        assert_eq!(g(Gesture::Swipe { from: p(600, 1650), to: p(600, 900), dir: Direction::Up }).as_deref(), Some("swipe_up_from_bottom"));
        assert_eq!(g(Gesture::Swipe { from: p(600, 800), to: p(100, 800), dir: Direction::Left }), None);
        assert_eq!(g(Gesture::Tap { pos: p(1, 1) }), None);
        for name in ["two_finger_tap", "corner_hold:top_left", "swipe_up_from_bottom", "long_press"] {
            assert!(ink_plugin::registry::GESTURES.contains(&name));
        }
    }

    /// Streaming HTTP from a local server through ureq, the worker and a Lua callback that
    /// grows an open viewer; the reader keeps handling events meanwhile.
    /// `rinky plugins`: list, disable (listed as off, on_load not run, nothing
    /// registered), enable again, unknown name.
    #[test]
    fn plugins_command_list_disable_enable() {
        let root = tmp("cli");
        let paths = Paths { root: root.clone() };
        install_sample(&paths.plugins());
        let settings = Settings::default();
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(cli(&args(&["list"]), Vec::new(), &settings, &paths).unwrap(), 0);
        let host = PluginHost::load(Vec::new(), &settings, &paths);
        let line = &plugin_list(&host)[0];
        assert!(line.starts_with("sample-lua\t") && line.contains("\tlua\ton\t") && line.contains("menu 2"), "{line}");

        assert_eq!(cli(&args(&["disable", "sample-lua"]), Vec::new(), &settings, &paths).unwrap(), 0);
        assert_eq!(user_disabled(&paths).into_iter().collect::<Vec<_>>(), vec!["sample-lua".to_string()]);
        let host = PluginHost::load(Vec::new(), &settings, &paths);
        assert!(host.turned_off("sample-lua") && !host.manager.is_enabled("sample-lua"));
        assert!(host.menu_items().is_empty() && host.notifications().is_empty(), "no registrations, no error toast");
        assert!(plugin_list(&host)[0].contains("\tlua\toff\terrors 0"), "{}", plugin_list(&host)[0]);

        assert_eq!(cli(&args(&["enable", "sample-lua"]), Vec::new(), &settings, &paths).unwrap(), 0);
        assert!(user_disabled(&paths).is_empty());
        assert_eq!(PluginHost::load(Vec::new(), &settings, &paths).menu_items().len(), 2);

        assert_eq!(cli(&args(&["disable", "nope"]), Vec::new(), &settings, &paths).unwrap(), 1);
        assert!(cli(&args(&["bogus"]), Vec::new(), &settings, &paths).is_err());
        assert!(cli(&args(&["enable"]), Vec::new(), &settings, &paths).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    /// A chat view (viewer with an input bar) scrolls instead of paging; a keyboard request
    /// of the same plugin docks under it: the view's input bar sits on the keyboard, the
    /// view keeps its newest lines in sight above it, typing happens in that bar, and
    /// neither docking nor undocking flashes the screen.
    #[test]
    fn keyboard_docks_under_the_chat_view_and_lifts_its_input_bar() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        let no_flash = |d: Vec<Damage>| {
            let d = ink_ui::refresh::coalesce(d, Rect::new(0, 0, W, H));
            assert!(d.iter().all(|d| d.class != RefreshClass::Gc16), "{d:?}");
            d
        };
        let mut md = String::new();
        for i in 0..30 {
            md.push_str(&format!(":::you\nQuestion {i}?\n:::answer model |\nAnswer number {i}, a few words long.\n"));
        }
        // A preset question: a tappable bubble with a caption.
        md.push_str(":::you chat:prompt/30 | Preset prompt · tap to see what was sent\nPeople/characters\n");
        md.push_str(":::\n:::input chat:ask | Ask about this book…");
        ui.push("p", UiRequest::TextViewer { id: 1, title: "Chat".into(), markdown: md });
        s.ui(&mut ui, false);
        let view = |ui: &PluginUi| match &ui.layers[0].kind {
            Kind::Viewer(v) => (v.scrolls(), v.page_count(), v.at_end(), v.scroll, v.dock, v.draft.clone()),
            _ => panic!(),
        };
        let (scrolls, pages, at_end, end, _, _) = view(&ui);
        assert!(scrolls && pages == 1 && at_end && end > 0.0, "one long page, opened at its end");
        shot(&s.canvas, "20-chat-scrolled");
        let Kind::Viewer(v) = &ui.layers[0].kind else { panic!() };
        let (r, _) = v.links.iter().find(|(_, l)| l == "chat:prompt/30").cloned().expect("bubble link");
        assert_eq!(ui.handle(&tap(r.x + 20, r.y + 20), screen()).1, vec![("p".to_string(), 1, "chat:prompt/30".to_string())]);
        // A swipe down scrolls back by about a screenful, no page counter.
        let (from, to) = (Point::new(600, 600), Point::new(600, 1200));
        ui.handle(&UiEvent::Gesture(Gesture::Swipe { from, to, dir: Direction::Down }), screen());
        let (_, _, at_end, up, _, _) = view(&ui);
        assert!(!at_end && up < end - 500.0, "{up} {end}");
        no_flash(s.ui(&mut ui, false));

        // The keyboard docks: the view is at its end again above the panel.
        ui.push("p", UiRequest::TextInput { id: 2, prompt: "Ask a follow-up question".into(), initial: String::new() });
        let d = no_flash(s.ui(&mut ui, false));
        assert_eq!(ui.layer_names(), vec!["viewer", "input"]);
        let input = ui.layers[1].rect;
        assert_eq!((input.h, input.bottom()), (InputBox::docked_height(), H as i32));
        let (_, _, at_end, _, dock, _) = view(&ui);
        assert!(at_end && dock == input.h, "newest lines above the keyboard");
        assert!(d.iter().all(|d| d.rect.y >= APP_BAR_H as i32), "the app bar stays: {d:?}");
        shot(&s.canvas, "21-chat-keyboard");
        let Kind::Input(b) = &ui.layers[1].kind else { panic!() };
        assert!(b.docked);
        let lr = Rect::new(0, 0, input.w, input.h);
        let (field, _, send, kb) = b.parts(lr);
        assert!(field.bottom() <= kb.y && send.bottom() <= kb.y, "the bar sits on the keyboard");
        let kb = Rect::new(kb.x, kb.y + input.y, kb.w, kb.h);
        for label in ["h", "i"] {
            let (r, _) = KeyboardLayout::qwerty_en().key_rects(Keyboard::new().keys_area(kb)).into_iter().find(|(_, k)| k.label == label).unwrap();
            for ev in press(r.x + 10, r.y + 10) {
                ui.handle(&ev, screen());
            }
        }
        let d = s.ui(&mut ui, false);
        assert!(d.iter().all(|d| d.class == RefreshClass::Gl16 && d.rect.y >= input.y && d.rect.h < input.h / 2), "keys and the field only: {d:?}");
        shot(&s.canvas, "22-chat-typing");

        // A tap above puts the keyboard away; the draft stays in the view's bar.
        assert_eq!(ui.handle(&tap(600, 400), screen()).1, vec![("p".to_string(), 2, String::new())]);
        no_flash(s.ui(&mut ui, false));
        assert_eq!(ui.layer_names(), vec!["viewer"]);
        let (_, _, at_end, _, dock, draft) = view(&ui);
        assert!(at_end && dock == 0 && draft.as_deref() == Some("hi"));
        shot(&s.canvas, "23-chat-draft");

        // The keyboard again: the draft comes back; the send button sends it.
        ui.push("p", UiRequest::TextInput { id: 3, prompt: "Ask".into(), initial: String::new() });
        no_flash(s.ui(&mut ui, false));
        let input = ui.layers[1].rect;
        let Kind::Input(b) = &ui.layers[1].kind else { panic!() };
        assert_eq!(b.text, "hi");
        let (_, _, send, _) = b.parts(Rect::new(0, 0, input.w, input.h));
        assert_eq!(ui.handle(&tap(send.x + 10, send.y + input.y + 10), screen()).1, vec![("p".to_string(), 3, "hi".to_string())]);
        no_flash(s.ui(&mut ui, false));
        assert_eq!(view(&ui).4, 0);

        // Another plugin's keyboard, or one with no chat view, is the plain panel.
        ui.handle(&key(Key::Back), screen());
        s.ui(&mut ui, false);
        ui.push("q", UiRequest::TextInput { id: 1, prompt: "Name".into(), initial: String::new() });
        s.ui(&mut ui, false);
        assert!(ui.layers.iter().all(|l| !matches!(&l.kind, Kind::Input(b) if b.docked)));
    }

    #[test]
    fn waiting_modal_does_not_spin_the_loop() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        ui.push("p", UiRequest::Dialog { id: 1, title: "A".into(), body: "".into(), buttons: vec![("ok".into(), "OK".into())] });
        assert_eq!(ui.timeout(), Some(Duration::ZERO), "startable: paint now");
        compose(&mut s, &mut ui, &[]);
        ui.push("p", UiRequest::Dialog { id: 2, title: "B".into(), body: "".into(), buttons: vec![("ok".into(), "OK".into())] });
        assert_eq!(ui.timeout(), None, "waits for the first dialog");
        ui.push("p", UiRequest::Toast { text: "t".into() });
        assert_eq!(ui.pending(), 2);
        ui.handle(&key(Key::Back), screen());
        assert_eq!(ui.timeout(), Some(Duration::ZERO), "first closed: B can start");
    }

    /// The same dialog requested again and again (13 taps on a chat bubble while an
    /// answer streamed, seen on a PW12) opens once. A stack of copies, shown one after
    /// another long after the chat closed, swallowed every tap: the reader looked frozen.
    #[test]
    fn repeated_dialog_opens_once() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        let dialog = |body: &str| UiRequest::Dialog { id: 16, title: "The prompt sent".into(), body: body.into(), buttons: vec![("OK".into(), "ok".into())] };
        for _ in 0..13 {
            ui.push("p", dialog("Explain the passage"));
        }
        assert_eq!(ui.pending(), 1, "waiting copies are dropped");
        compose(&mut s, &mut ui, &[]);
        assert_eq!(ui.layer_names(), ["dialog"]);
        for _ in 0..5 {
            ui.push("p", dialog("Explain the passage"));
        }
        assert_eq!(ui.pending(), 0, "copies of the open dialog are dropped");
        // Another plugin's dialog, or new text under the same id, still waits its turn.
        ui.push("q", dialog("Explain the passage"));
        ui.push("p", dialog("Something else"));
        assert_eq!(ui.pending(), 2);
        ui.handle(&key(Key::Back), screen());
        compose(&mut s, &mut ui, &[]);
        ui.handle(&key(Key::Back), screen());
        compose(&mut s, &mut ui, &[]);
        ui.handle(&key(Key::Back), screen());
        compose(&mut s, &mut ui, &[]);
        assert!(!ui.modal_open() && ui.pending() == 0, "nothing left to swallow taps: {:?}", ui.layer_names());
    }

    /// Under a modal layer (the Insightful chat, a dialog) the screen gets no touches:
    /// the reader's edge slides set brightness and warmth from them, on the page only.
    #[test]
    fn modal_layer_keeps_touches_from_the_screen() {
        let mut s = Screen::new();
        let mut ui = PluginUi::default();
        let p = Point::new(20, 800);
        let touch = UiEvent::Touch(ink_ui::Touch { slot: 0, kind: ink_ui::TouchKind::Move, pos: p, start: Point::new(20, 900), held: Duration::from_millis(80), moved: true });
        assert!(!ui.handle(&touch, screen()).0, "no layer: the page gets it");
        ui.push("p", UiRequest::Toast { text: "t".into() });
        compose(&mut s, &mut ui, &[]);
        assert!(!ui.handle(&touch, screen()).0, "a toast is not modal");
        ui.push("p", UiRequest::Dialog { id: 1, title: "T".into(), body: "B".into(), buttons: vec![("OK".into(), "ok".into())] });
        compose(&mut s, &mut ui, &[]);
        assert!(ui.modal_open());
        assert!(ui.handle(&touch, screen()).0, "the modal layer keeps it");
    }

    /// A test screen: fills the canvas with `fill` when dirty and shows it itself;
    /// `own_overlay` also paints the plugin overlay itself (the pre-wave-B way).
    struct FillScreen {
        fill: u8,
        dirty: bool,
        own_overlay: bool,
    }

    impl crate::router::AppScreen for FillScreen {
        fn name(&self) -> &'static str {
            "fill"
        }
        fn handle(&mut self, _ev: &UiEvent, _cx: &mut crate::router::AppCtx) -> anyhow::Result<crate::router::Transition> {
            Ok(crate::router::Transition::None)
        }
        fn paint(&mut self, cx: &mut crate::router::AppCtx) -> anyhow::Result<Vec<Damage>> {
            if !std::mem::take(&mut self.dirty) {
                return Ok(Vec::new());
            }
            cx.canvas.clear(self.fill);
            if self.own_overlay {
                cx.plugins.paint_overlay(&mut PaintCx { canvas: &mut *cx.canvas, fonts: &mut *cx.fonts, theme: cx.theme }, true);
            }
            cx.show_all(RefreshClass::Gl16)?;
            Ok(Vec::new())
        }
    }

    /// hooks::app_paint with a screen that shows its page itself, and with one that
    /// still paints the overlay itself: the overlay is drawn, and closing it leaves the
    /// screen's pixels (no ghost of the overlay).
    #[test]
    fn app_paint_with_self_showing_screens() {
        for own_overlay in [false, true] {
            let mut host = PluginHost::new(Vec::new());
            let mut canvas = Canvas::new(W as usize, H as usize);
            let mut fonts = Fonts::load();
            let theme = Theme::day();
            let mut refresh = ink_ui::RefreshScheduler::new(6);
            let (mut dev, _probe) = ink_device::headless::open(ink_device::headless::HeadlessOptions::default());
            let settings = Settings::default();
            let paths = Paths { root: tmp("apppaint") };
            let mut cx = crate::router::AppCtx {
                canvas: &mut canvas,
                screen: dev.screen.as_mut(),
                fonts: &mut fonts,
                theme: &theme,
                refresh: &mut refresh,
                plugins: &mut host,
                settings: &settings,
                paths: &paths,
                providers: &[],
                services: crate::router::Services {
                    info: &dev.info,
                    light: dev.light.as_mut(),
                    battery: dev.battery.as_mut(),
                    power: dev.power.as_mut(),
                    wifi: dev.wifi.as_mut(),
                    usb: dev.usb.as_mut(),
                    clock: dev.clock.as_mut(),
                },
                settings_update: &mut None,
            };
            let mut top = FillScreen { fill: 200, dirty: true, own_overlay };
            hooks::app_paint(&mut top, &mut cx).unwrap();
            cx.plugins.ui.push("p", UiRequest::Dialog { id: 1, title: "T".into(), body: "B".into(), buttons: vec![("ok".into(), "OK".into())] });
            hooks::app_paint(&mut top, &mut cx).unwrap();
            assert_eq!(cx.plugins.ui.layer_names(), vec!["dialog"]);
            let r = Dialog::frame(screen());
            assert!(cx.canvas.crop(r).pixels.iter().any(|&p| p != 200), "dialog drawn");
            // The page changes under the dialog (a page turn), then the dialog closes.
            top.fill = 90;
            top.dirty = true;
            hooks::app_paint(&mut top, &mut cx).unwrap();
            assert!(cx.canvas.crop(r).pixels.iter().any(|&p| p != 90), "dialog drawn again over the new page");
            assert!(hooks::app_handle(&mut top, &key(Key::Back), &mut cx));
            hooks::app_paint(&mut top, &mut cx).unwrap();
            if own_overlay {
                // The screen's own paint_overlay does the restore on its next repaint.
                top.dirty = true;
                hooks::app_paint(&mut top, &mut cx).unwrap();
            }
            assert!(cx.canvas.pixels.iter().all(|&p| p == 90), "own_overlay={own_overlay}: no overlay pixels left");
        }
    }

    struct FakePower(Vec<Duration>);
    impl ink_device::Power for FakePower {
        fn defer_suspend(&mut self, d: Duration) -> ink_device::Result<()> {
            self.0.push(d);
            Ok(())
        }
    }

    /// Power loop: a running request holds off sleep; the screensaver cancels it (the
    /// plugin gets an error event) and runs on_suspend once; an RTC wake does not resume;
    /// leaving the screensaver runs on_resume once.
    #[test]
    fn power_events_suspend_resume_and_interrupt_requests() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = c.read(&mut buf).unwrap();
            c.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: a\ndata: x\n\n").unwrap();
            c.flush().unwrap();
            // Stall until the test ends.
            let _ = stop_rx.recv_timeout(Duration::from_secs(20));
        });
        let src = format!(
            r#"local n = {{ s = 0, r = 0 }}
            return {{
              on_action = function(id)
                ink.http.request({{ url = "http://127.0.0.1:{port}/sse", stream = "sse" }}, function(ev)
                  if ev.type == "error" then ink.ui.toast("error " .. ev.message) end
                end)
              end,
              on_suspend = function() n.s = n.s + 1; ink.ui.toast("suspend " .. n.s) end,
              on_resume = function() n.r = n.r + 1; ink.ui.toast("resume " .. n.r) end,
            }}"#
        );
        let m = PluginManifest::new("sleepy", "1", &[Permission::Ui, Permission::Network]);
        let dir = tmp("sleepy");
        let p = lua::LuaPlugin::from_source(m, &dir, &src).unwrap();
        let mut host = PluginHost::new(vec![Box::new(p)]);
        let mut power = FakePower(Vec::new());
        assert!(!hooks::keep_awake(&mut host, &mut power), "idle: no deferral");
        assert!(host.trigger("sleepy", "go", None));
        let t = Instant::now();
        while !host.net_busy() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(host.net_busy());
        assert!(hooks::keep_awake(&mut host, &mut power));
        assert!(!hooks::keep_awake(&mut host, &mut power), "renewed only every DEFER_RENEW");
        assert_eq!(power.0, vec![ink_net::connectivity::DEFER_HOLD]);
        assert!(!host.handle_event(&UiEvent::Power(PowerEvent::ReadyToSuspend), None, screen()));
        assert!(hooks::keep_awake(&mut host, &mut power), "readyToSuspend asks again at once");

        let toasts = |host: &mut PluginHost| -> Vec<String> {
            host.take_ui_requests().into_iter().filter_map(|r| if let UiRequest::Toast { text } = r { Some(text) } else { None }).collect()
        };
        toasts(&mut host);
        let power_ev = |p| UiEvent::Power(p);
        assert!(!host.handle_event(&power_ev(PowerEvent::GoingToScreenSaver), None, screen()));
        assert_eq!(toasts(&mut host), vec![format!("error {SLEEP_INTERRUPT}"), "suspend 1".to_string()]);
        assert!(host.suspended() && !hooks::keep_awake(&mut host, &mut power));
        host.handle_event(&power_ev(PowerEvent::GoingToScreenSaver), None, screen());
        host.handle_event(&power_ev(PowerEvent::WakeupFromSuspend), None, screen());
        host.handle_event(&power_ev(PowerEvent::Resuming), None, screen());
        assert!(toasts(&mut host).is_empty(), "no second suspend, RTC wakes do not resume");
        host.handle_event(&power_ev(PowerEvent::OutOfScreenSaver), None, screen());
        host.handle_event(&power_ev(PowerEvent::OutOfScreenSaver), None, screen());
        assert_eq!(toasts(&mut host), vec!["resume 1".to_string()]);
        // The worker thread ends once the stalled read sees the cancel.
        stop_tx.send(()).unwrap();
        server.join().unwrap();
        let t = Instant::now();
        while host.net_busy() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!host.net_busy(), "the request thread ended");
        assert!(host.manager.errors.is_empty(), "{:?}", host.manager.errors);
    }

    #[test]
    fn streaming_http_into_a_viewer() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // The server sends the first word, then the rest once the viewer has shown it:
        // the text arrives in steps however slow the machine is.
        let (shown_tx, shown_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut c, _) = listener.accept().unwrap();
                let mut buf = [0u8; 2048];
                let n = c.read(&mut buf).unwrap();
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                if head.starts_with("GET /sse") {
                    c.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").unwrap();
                    for (i, word) in ["Streaming ", "from ", "a ", "local ", "server."].into_iter().enumerate() {
                        if i == 1 {
                            let _ = shown_rx.recv_timeout(Duration::from_secs(10));
                        }
                        c.write_all(format!("event: delta\ndata: {word}\n\n").as_bytes()).unwrap();
                        c.flush().unwrap();
                        std::thread::sleep(Duration::from_millis(30));
                    }
                } else {
                    c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope").unwrap();
                }
            }
        });
        let src = format!(
            r#"local text, view = "", nil
            return {{ on_action = function(id)
              view = ink.ui.view({{ title = "Answer", markdown = "..." }})
              ink.http.request({{ url = "http://127.0.0.1:{port}/sse", stream = "sse" }}, function(ev)
                if ev.type == "sse" then text = text .. ev.data
                elseif ev.type == "done" then text = text .. " [done]" end
                ink.ui.view({{ id = view, title = "Answer", markdown = text }})
              end)
              ink.http.get("http://127.0.0.1:{port}/missing", function(ev) ink.ui.toast("status " .. ev.status .. " " .. ev.body) end)
            end }}"#
        );
        let m = PluginManifest::new("net", "1", &[Permission::Ui, Permission::Network]);
        let dir = tmp("net");
        let p = lua::LuaPlugin::from_source(m, &dir, &src).unwrap();
        let mut host = PluginHost::new(vec![Box::new(p)]);
        let mut s = Screen::new();
        assert!(host.trigger("net", "go", None));
        s.overlay(&mut host, false);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut partial = false;
        while Instant::now() < deadline {
            assert!(host.timeout().is_some());
            host.handle_event(&UiEvent::Timer(0), None, screen());
            s.overlay(&mut host, false);
            if !partial && host.ui.layers.iter().any(|l| matches!(&l.kind, Kind::Viewer(v) if markdown::to_plain(&v.blocks).trim() == "Streaming")) {
                partial = true;
                let _ = shown_tx.send(());
            }
            let done = host.ui.layers.iter().any(|l| matches!(&l.kind, Kind::Viewer(v) if markdown::to_plain(&v.blocks).ends_with("[done]")));
            if done && host.net.worker.as_ref().is_some_and(|w| w.in_flight() == 0) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        server.join().unwrap();
        let Kind::Viewer(v) = &host.ui.layers.iter().find(|l| matches!(l.kind, Kind::Viewer(_))).unwrap().kind else { panic!() };
        assert_eq!(markdown::to_plain(&v.blocks), "Streaming from a local server. [done]");
        assert!(partial, "the viewer showed the first word before the rest arrived");
        shot(&s.canvas, "17-streamed-viewer");
        // The 404 answer waits behind the viewer as a toast? No: toasts show on top.
        assert!(host.ui.layer_names().contains(&"viewer"));
        assert!(host.manager.errors.is_empty(), "{:?}", host.manager.errors);
    }
}

