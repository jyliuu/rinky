//! Library and home screen (owner: the `library` agent, wave B).
//!
//! - The book folders from the settings (`/mnt/us/Books`, `/mnt/us/documents`) are
//!   scanned into ink-store's `library.json`; metadata comes from `ink_formats::read_info`
//!   (MuPDF for PDF/CBZ/XPS) and cover thumbnails are cached under `cache/covers/`.
//!   Metadata of new books is read in batches between events, so a large first scan
//!   does not block the screen.
//! - A navigation bar at the bottom with the items of `ui.nav_items` (2 to 5, chosen in
//!   Settings > Home & library): Home (continue reading, recent books), Library (list or
//!   grid, sort, status and format filters, search with the on-screen keyboard),
//!   Collections, Files, Search, Statistics, Catalogs (OPDS plugin) and Settings. The bar
//!   reacts on its whole height: a tap, and also a press that lifts in the same cell
//!   after moving (a jittery tap the recogniser reports as a swipe).
//! - KOReader import and rescan are started from Settings > Home & library
//!   ([`request`]); the import page shows the progress and the result.
//! - The library, collections and file browser views show a screenful of rows (or
//!   covers) with a scroll bar on the right (`ink_ui::ScrollBar`): a swipe up or down or
//!   a page key moves one screen; a tap or a drag on the bar jumps there (the drag moves
//!   only the bar, DU; the lift shows the rows). A scroll step refreshes only the rows
//!   and the bar (one GL16), not the whole screen.
//! - Tap a book to read it; hold it for its menu (`book_menu`: open, details page,
//!   finished/unread, collections, delete with confirmation). A book that cannot be
//!   opened shows the reader's error screen; a tap there pops back here.
//! - The power key and `UiEvent::Quit` leave the app. There is no exit button here: the
//!   only exit in the UI is Settings > "Exit to Kindle". The device's "library rescan" event (after USB drive mode) rescans.

mod book_menu;
mod browse;
mod covers;
mod details;
mod files;
mod home;
mod model;
#[cfg(test)]
mod tests;
mod view;

pub use model::{ImportResult, Library, Query, StatusFilter};

use crate::router::{AppCtx, AppScreen, Route, ScreenRegistry, Transition};
use crate::screens::common::paint_cx;
use crate::screens::settings::worker::{self, Job, Poll, Reporter};
use ink_device::{Key, PowerEvent, Rect, RefreshClass, Size};
use ink_store::koreader::KoreaderDirs;
use ink_store::ReadingStatus;
use ink_ui::{Damage, Dialog, EventResult, Gesture, KeyAction, Keyboard, Menu, ScrollBar, TextInput, UiEvent, Widget};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Work another screen (Settings) asks the library to do the next time it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Import KOReader's data (shows the import page with the progress).
    KoreaderImport,
    /// Read the book folders again.
    Rescan,
    /// Open the file browser.
    Files,
}

static REQUEST: std::sync::Mutex<Option<Request>> = std::sync::Mutex::new(None);

/// Asks the library to do `r` when it is shown next (its `on_enter`).
pub fn request(r: Request) {
    if let Ok(mut q) = REQUEST.lock() {
        *q = Some(r);
    }
}

fn take_request() -> Option<Request> {
    REQUEST.lock().ok().and_then(|mut q| q.take())
}

thread_local! {
    /// Books the reader opened by itself ("Open next" on the end card), with the time.
    /// Screens run on the app loop's thread (tests: each on its own).
    static OPENED: std::cell::RefCell<Vec<(PathBuf, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Tells the library that the reader opened `path` now: when the library is shown next,
/// the book counts as opened then, and its progress is read from its sidecar.
pub fn note_opened(path: &std::path::Path) {
    OPENED.with(|v| v.borrow_mut().push((path.to_path_buf(), model::now())));
}

fn take_opened() -> Vec<(PathBuf, u64)> {
    OPENED.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

/// Registers the library for [`Route::Library`]. The factory does no IO.
pub fn register(reg: &mut ScreenRegistry) {
    reg.register("library", |_| Ok(Box::new(LibraryScreen::new())));
}

/// While the metadata thread runs, its results are taken this often.
const META_POLL: Duration = Duration::from_millis(200);
/// While metadata is being read, the screen is repainted at most this often.
const REPAINT_EVERY: Duration = Duration::from_secs(4);
/// While typing a search, the results are repainted only after the keys rest this
/// long; each key refreshes just the field and the key face.
const SEARCH_PAUSE: Duration = Duration::from_millis(600);

/// Result of a rescan or import on the worker thread: the library comes back with it.
pub(crate) enum LibJob {
    Rescan(Library),
    Import(Library, ImportResult),
}

/// Which page of the screen is shown.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum View {
    Home,
    Books,
    Collections,
    Details(PathBuf),
    Import,
    /// File browser.
    Files,
}

/// The view of a navigation bar item (`None`: the item opens another screen or a
/// plugin).
pub(crate) fn nav_view(name: &str) -> Option<View> {
    Some(match name {
        "home" => View::Home,
        "library" | "search" => View::Books,
        "collections" => View::Collections,
        "files" => View::Files,
        _ => return None,
    })
}

/// Prefix of the overflow menu actions of plugin library entries (`lib:plugin:<p>:<id>`).
const LIB_ENTRY: &str = "lib:";

/// List or grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewMode {
    List,
    Grid,
}

/// A tap target recorded while painting.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Hit {
    Book(PathBuf),
    /// A navigation bar cell (index into the screen's nav items).
    Tab(usize),
    /// The app bar's overflow menu (Settings, Statistics, KOReader import).
    More,
    Settings,
    /// Reading statistics (screens::stats).
    Stats,
    Back,
    Sort,
    Filter,
    Search,
    /// Empty the search (the clear button in the search field).
    ClearSearch,
    /// A status filter chip ([`StatusFilter::ALL`] index).
    StatusFilter(usize),
    ToggleMode,
    Read(PathBuf),
    /// A reading status chip on the details page ([`STATUSES`] index).
    SetStatus(PathBuf, usize),
    CollectionMenu(PathBuf),
    Delete(PathBuf),
    PluginMenu(PathBuf),
    NewCollection,
    Collection(String),
    Import,
    Rescan,
    /// Open the file browser.
    Browse,
    /// File browser: the parent folder.
    FileUp,
    /// File browser: a folder or a book file.
    File(PathBuf),
}

/// What a menu is for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MenuKind {
    Sort,
    Filter,
    Collections(PathBuf),
    CollectionActions(String),
    Plugin(PathBuf),
    /// A held book's actions (`book_menu`).
    Book(PathBuf),
    /// The app bar's overflow menu.
    More,
}

/// What a confirmation is for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Confirm {
    DeleteBook(PathBuf),
    DeleteCollection(String),
}

/// What typed text is for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum InputFor {
    Search,
    NewCollection(Option<PathBuf>),
    Rename(String),
}

/// The scroll bar of the last paint (browse views with more than a screenful).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BarGeom {
    /// The scrolling region (rows and the bar's column).
    pub region: Rect,
    /// Items per screen.
    pub per: usize,
    /// Where the view is.
    pub bar: ScrollBar,
}

impl BarGeom {
    /// The screen that holds the item under height `y` of the bar.
    fn page_at(&self, y: i32) -> usize {
        self.bar.item_at(ScrollBar::track(self.region), y) / self.per
    }
}

/// A finger on the scroll bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BarDrag {
    slot: u8,
    /// The screen under the finger (shown on the bar only).
    page: usize,
    /// The bar shows `page` (a DU preview was drawn).
    shown: bool,
}

/// A modal layer over the view.
#[allow(clippy::large_enum_variant)] // one overlay at a time, held in an Option on the screen
pub(crate) enum Overlay {
    Menu { menu: Menu, rect: Rect, kind: MenuKind },
    Confirm { dialog: Dialog, kind: Confirm },
    Input { search: bool, title: String, purpose: InputFor, input: TextInput, keyboard: Keyboard },
}

/// Keyboard area at the bottom (the shared size: [`Keyboard::bottom_rect`]).
pub(crate) fn keyboard_rect(s: Size) -> Rect {
    Keyboard::bottom_rect(s)
}

/// The text field: in the header for search, in the name panel otherwise.
pub(crate) fn field_rect(s: Size, search: bool) -> Rect {
    if search {
        Rect::new(view::MARGIN, (view::HEADER_H - 104) / 2, s.w.saturating_sub(2 * view::MARGIN as u32 + 216), 104)
    } else {
        let p = name_panel(s);
        Rect::new(p.x + 24, p.y + 80, p.w - 48, 100)
    }
}

/// Panel for collection names, above the keyboard.
pub(crate) fn name_panel(s: Size) -> Rect {
    let kb = keyboard_rect(s);
    Rect::new(view::MARGIN, kb.y - 340, s.w - 2 * view::MARGIN as u32, 320)
}

/// Cancel and OK buttons of the name panel.
pub(crate) fn name_buttons(s: Size) -> (Rect, Rect) {
    let p = name_panel(s);
    let bw = (p.w - 48 - 24) / 2;
    (Rect::new(p.x + 24, p.y + 200, bw, 104), Rect::new(p.x + 48 + bw as i32, p.y + 200, bw, 104))
}

/// The library screen.
pub struct LibraryScreen {
    lib: Option<Library>,
    dirs: Vec<PathBuf>,
    view: View,
    /// View to return to from the details page.
    back_to: View,
    mode: ViewMode,
    query: Query,
    page: usize,
    coll_page: usize,
    covers: covers::CoverCache,
    hits: Vec<(Rect, Hit)>,
    overlay: Option<Overlay>,
    /// Repaint everything with this class.
    dirty: Option<RefreshClass>,
    /// Only the keyboard changed.
    keyboard_dirty: bool,
    /// The text field changed too (repainted with the keyboard).
    field_dirty: bool,
    /// The search text changed: repaint the results at this time.
    search_due: Option<Instant>,
    /// The book handed to the reader (its state is read back on return).
    opened: Option<PathBuf>,
    import_result: Option<Vec<String>>,
    /// Where KOReader's data is read from.
    pub ko: KoreaderDirs,
    last_paint: Instant,
    /// `library-view.json` (set on the first enter).
    prefs: Option<PathBuf>,
    /// The refusal text of the book on the details page.
    refusal: Option<String>,
    /// Plugin library actions (copied from the plugin host on enter).
    plugin_actions: Vec<ink_plugin::registry::LibraryAction>,
    /// A plugin library action picked from the menu, run in `handle` (needs `cx`).
    run_action: Option<(ink_plugin::registry::LibraryAction, PathBuf)>,
    /// A toast to show in `handle` (needs `cx`): the result of "Export highlights".
    toast: Option<String>,
    /// Rescan or KOReader import on a worker thread; it owns the library meanwhile
    /// (`lib` is `None`) and the screen shows only a progress line.
    pub(crate) job: Option<Job<LibJob>>,
    /// The progress line and whether it changed since the last paint.
    pub(crate) busy_text: String,
    busy_dirty: bool,
    /// For reading the library again when a job is lost.
    paths: Option<ink_store::Paths>,
    /// File browser: folder shown, its rows (or the error reading it), page.
    pub(crate) files_dir: PathBuf,
    pub(crate) files: Vec<files::FileEntry>,
    files_error: Option<String>,
    files_page: usize,
    /// Hero card data of the Home tab (chapter, time left), per book and position.
    home: home::HomeCache,
    /// Navigation bar items (`ui.nav_items`).
    pub(crate) nav: Vec<String>,
    /// On-screen keyboard layouts (`ui.keyboard_layouts`, first = default).
    keyboard_layouts: Vec<String>,
    /// A navigation item that opens a plugin page, run in `handle` (needs `cx`).
    nav_plugin: Option<String>,
    /// Library entries of the enabled plugins (copied from the plugin host on enter):
    /// overflow menu rows and navigation bar items saved as `plugin:<plugin>:<id>`.
    pub(crate) plugin_entries: Vec<ink_plugin::LibraryEntry>,
    /// A plugin library entry that was picked, run in `handle` (needs `cx`).
    run_entry: Option<(String, String)>,
    /// A finger went down on a navigation bar cell: (slot, cell).
    nav_press: Option<(u8, usize)>,
    /// The press fired on lift: the gesture that follows it is dropped.
    nav_fired: bool,
    /// The scroll bar of the last paint.
    pub(crate) bar: Option<BarGeom>,
    /// A finger on the scroll bar.
    bar_drag: Option<BarDrag>,
    /// The drag preview changed: repaint the bar only (DU).
    bar_dirty: bool,
    /// The scroll bar took the last touch: the gesture that follows it is dropped.
    bar_fired: bool,
    /// The view moved by a screen: repaint the scrolling region only (GL16).
    scrolled: bool,
}

impl Default for LibraryScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl LibraryScreen {
    /// A library screen; the scan happens in `on_enter`.
    pub fn new() -> Self {
        LibraryScreen {
            lib: None,
            dirs: Vec::new(),
            view: View::Home,
            back_to: View::Home,
            mode: ViewMode::List,
            query: Query::default(),
            page: 0,
            coll_page: 0,
            covers: covers::CoverCache::default(),
            hits: Vec::new(),
            overlay: None,
            dirty: None,
            keyboard_dirty: false,
            field_dirty: false,
            search_due: None,
            opened: None,
            import_result: None,
            ko: KoreaderDirs::device(),
            last_paint: Instant::now(),
            prefs: None,
            refusal: None,
            plugin_actions: Vec::new(),
            run_action: None,
            toast: None,
            job: None,
            busy_text: String::new(),
            busy_dirty: false,
            paths: None,
            files_dir: PathBuf::new(),
            files: Vec::new(),
            files_error: None,
            files_page: 0,
            home: home::HomeCache::default(),
            nav: ink_store::prefs::UiSettings::default().nav_items,
            keyboard_layouts: vec!["en".into()],
            nav_plugin: None,
            plugin_entries: Vec::new(),
            run_entry: None,
            nav_press: None,
            nav_fired: false,
            bar: None,
            bar_drag: None,
            bar_dirty: false,
            bar_fired: false,
            scrolled: false,
        }
    }

    /// The library, once entered.
    pub fn library(&self) -> Option<&Library> {
        self.lib.as_ref()
    }

    fn save_prefs(&self) {
        if let Some(p) = &self.prefs {
            model::save_prefs(p, &self.query, self.mode == ViewMode::Grid);
        }
    }

    /// Index of the selected navigation item (none on the details page).
    fn tab(&self) -> Option<usize> {
        self.nav.iter().position(|n| nav_view(n).as_ref() == Some(&self.view) && n != "search")
    }

    /// Opens navigation item `name`.
    fn open_nav(&mut self, name: &str, size: Size) -> Transition {
        log::info!("library: navigation bar {name} (view {:?})", self.view);
        match name {
            "settings" => return Transition::Push(Route::Settings),
            "stats" => return Transition::Push(Route::Statistics),
            "opds" => self.nav_plugin = Some("opds".into()),
            saved if ink_plugin::registry::parse_saved_id(saved).is_some() => {
                self.run_entry = ink_plugin::registry::parse_saved_id(saved).map(|(p, id)| (p.to_string(), id.to_string()));
            }
            "files" => return self.act(Hit::Browse, false, size),
            "search" => {
                self.query.collection = None;
                self.show(View::Books);
                return self.act(Hit::Search, false, size);
            }
            other => {
                if let Some(v) = nav_view(other) {
                    if v == View::Books {
                        self.query.collection = None;
                    }
                    self.show(v);
                }
            }
        }
        Transition::None
    }

    fn redraw(&mut self) {
        self.dirty = Some(match self.dirty {
            Some(RefreshClass::Gc16) => RefreshClass::Gc16,
            _ => RefreshClass::Gl16,
        });
    }

    fn hit_at(&self, pos: ink_device::Point) -> Option<Hit> {
        self.hits.iter().rev().find(|(r, _)| r.contains(pos)).map(|(_, h)| h.clone())
    }

    fn open_book(&mut self, path: PathBuf) -> Transition {
        // A refused book (DRM, KFX) only shows the reader's error: it is not "opened".
        if let Some(lib) = self.lib.as_mut().filter(|l| l.index.get(&path).is_some_and(|b| !b.unsupported)) {
            lib.record_open(&path);
        }
        self.opened = Some(path.clone());
        Transition::Push(Route::Reader(path))
    }

    fn show(&mut self, v: View) {
        if !matches!(v, View::Details(_)) {
            self.back_to = v.clone();
        }
        // Why a refused book cannot be opened (read once, for the details page).
        self.refusal = match &v {
            View::Details(p) if self.lib.as_ref().and_then(|l| l.index.get(p)).is_some_and(|b| b.unsupported) => {
                ink_formats::read_info(p).ok().and_then(|i| i.refusal)
            }
            _ => None,
        };
        if v == View::Books && self.view != View::Books && self.view != View::Collections && !matches!(self.view, View::Details(_)) {
            self.page = 0;
        }
        self.view = v;
        self.redraw();
    }

    fn act(&mut self, hit: Hit, long: bool, size: Size) -> Transition {
        match hit {
            Hit::Book(p) if long => self.open_book_menu(p, size),
            Hit::Book(p) | Hit::Read(p) => return self.open_book(p),
            Hit::Tab(i) => {
                let Some(name) = self.nav.get(i).cloned() else { return Transition::None };
                return self.open_nav(&name, size);
            }
            Hit::More => {
                let mut items = menu_items(&[("Settings", "settings"), ("Reading statistics", "stats"), ("KOReader import", "import")]);
                items.extend(self.plugin_entries.iter().map(|e| (e.label.clone(), format!("{LIB_ENTRY}{}", ink_plugin::registry::saved_id(&e.plugin, &e.id)))));
                self.open_menu(items, MenuKind::More, Hit::More, size);
            }
            Hit::Settings => return Transition::Push(Route::Settings),
            Hit::Stats => return Transition::Push(Route::Statistics),
            Hit::Back => {
                if matches!(self.view, View::Details(_)) {
                    let v = self.back_to.clone();
                    self.view = v;
                } else if self.view == View::Files {
                    self.view = View::Import;
                } else if self.view == View::Books && self.query.collection.is_some() {
                    self.query.collection = None;
                    self.view = View::Collections;
                }
                self.redraw();
            }
            Hit::Sort => {
                let items = model::SORTS.iter().enumerate().map(|(i, (_, l))| (mark(self.query.sort == model::SORTS[i].0, l), format!("{i}"))).collect();
                self.open_menu(items, MenuKind::Sort, Hit::Sort, size);
            }
            Hit::Filter => {
                let mut items: Vec<(String, String)> =
                    StatusFilter::ALL.iter().enumerate().map(|(i, s)| (mark(self.query.format.is_none() && self.query.status == *s, s.label()), format!("s{i}"))).collect();
                for f in self.lib.as_ref().map(|l| l.formats()).unwrap_or_default() {
                    items.push((mark(self.query.format.as_deref() == Some(&f), &f.to_uppercase()), format!("f{f}")));
                }
                self.open_menu(items, MenuKind::Filter, Hit::Filter, size);
            }
            Hit::Search => self.open_input(InputFor::Search, "", &self.query.search.clone()),
            Hit::ClearSearch => {
                self.query.search.clear();
                self.page = 0;
                self.redraw();
            }
            Hit::StatusFilter(i) => {
                self.query.status = StatusFilter::ALL.get(i).copied().unwrap_or(StatusFilter::All);
                self.query.format = None;
                self.page = 0;
                self.save_prefs();
                self.redraw();
            }
            Hit::ToggleMode => {
                self.mode = if self.mode == ViewMode::List { ViewMode::Grid } else { ViewMode::List };
                self.page = 0;
                self.save_prefs();
                self.redraw();
            }
            Hit::SetStatus(p, i) => {
                if let (Some(st), Some(lib)) = (STATUSES.get(i).copied(), self.lib.as_mut()) {
                    lib.set_status(&p, st);
                }
                self.redraw();
            }
            Hit::CollectionMenu(p) => {
                let Some(lib) = self.lib.as_ref() else { return Transition::None };
                let mine = lib.index.get(&p).map(|b| b.collections.clone()).unwrap_or_default();
                let mut items: Vec<(String, String)> = lib.index.collections.iter().map(|c| (mark(mine.contains(&c.name), &c.name), format!("c{}", c.name))).collect();
                items.push(("New collection…".into(), "new".into()));
                self.open_menu(items, MenuKind::Collections(p.clone()), Hit::CollectionMenu(p), size);
            }
            Hit::Delete(p) => {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let body = format!("{name} and rinky's notes for it are removed from the device. This cannot be undone.");
                self.confirm_delete("Delete this book?", body, Confirm::DeleteBook(p));
            }
            Hit::PluginMenu(p) => {
                let items = self.plugin_actions.iter().enumerate().map(|(i, a)| (a.label.clone(), format!("{i}"))).collect();
                self.open_menu(items, MenuKind::Plugin(p.clone()), Hit::PluginMenu(p), size);
            }
            Hit::NewCollection => self.open_input(InputFor::NewCollection(None), "New collection", ""),
            Hit::Collection(name) if long => {
                let items = menu_items(&[("Rename", "rename"), ("Delete", "delete")]);
                self.open_menu(items, MenuKind::CollectionActions(name.clone()), Hit::Collection(name), size);
            }
            Hit::Collection(name) => {
                self.query.collection = Some(name);
                self.page = 0;
                self.view = View::Books;
                self.back_to = View::Books;
                self.redraw();
            }
            Hit::Import => {
                if let Some(mut lib) = self.lib.take() {
                    let ko = self.ko.clone();
                    self.start_job(Job::spawn("Importing from KOReader", move |r: &Reporter<LibJob>| {
                        let res = lib.import_koreader_with(&ko, &mut |i, n| {
                            r.progress(if i < n { format!("book {} of {n}", i + 1) } else { "statistics".to_string() })
                        });
                        LibJob::Import(lib, res)
                    }));
                }
            }
            Hit::Rescan => self.start_rescan(),
            Hit::Browse => {
                let dir = if self.files_dir.as_os_str().is_empty() { files::start_dir(&self.dirs) } else { self.files_dir.clone() };
                self.open_dir(dir);
                self.view = View::Files;
                self.redraw();
            }
            Hit::FileUp => {
                if let Some(parent) = self.files_dir.parent().map(PathBuf::from) {
                    self.open_dir(parent);
                    self.redraw();
                }
            }
            Hit::File(path) => {
                if path.is_dir() {
                    self.open_dir(path);
                    self.redraw();
                } else {
                    log::info!("library: file browser opens {}", path.display());
                    return self.open_book(path);
                }
            }
        }
        Transition::None
    }

    /// Reads `dir` into the file browser (page 0).
    fn open_dir(&mut self, dir: PathBuf) {
        match files::list_dir(&dir) {
            Ok(v) => {
                log::info!("library: file browser {} ({} entries)", dir.display(), v.len());
                self.files = v;
                self.files_error = None;
            }
            Err(e) => {
                log::warn!("library: file browser: {e}");
                self.files.clear();
                self.files_error = Some(e);
            }
        }
        self.files_dir = dir;
        self.files_page = 0;
    }

    fn start_rescan(&mut self) {
        let Some(mut lib) = self.lib.take() else { return };
        self.start_job(Job::spawn("Rescanning the library", move |r: &Reporter<LibJob>| {
            r.progress("reading the book folders");
            lib.rescan();
            LibJob::Rescan(lib)
        }));
    }

    fn start_job(&mut self, job: Job<LibJob>) {
        self.busy_text = job.status();
        self.job = Some(job);
        self.redraw();
        self.poll_job();
    }

    /// Takes a finished job's library back, or updates the progress line.
    pub(crate) fn poll_job(&mut self) {
        let Some(job) = self.job.as_mut() else { return };
        match job.poll() {
            Poll::Running => {
                let text = job.status();
                if text != self.busy_text {
                    self.busy_text = text;
                    self.busy_dirty = true;
                }
            }
            Poll::Done(out) => {
                self.job = None;
                match out {
                    LibJob::Rescan(lib) => self.lib = Some(lib),
                    LibJob::Import(lib, r) => {
                        log::info!("library: KOReader import {r:?}");
                        self.import_result = Some(r.lines());
                        self.lib = Some(lib);
                    }
                }
                self.covers.clear();
                self.redraw();
            }
            Poll::Lost => {
                self.job = None;
                self.import_result = Some(vec!["The work stopped with an internal error; the library was read again.".into()]);
                if let Some(paths) = &self.paths {
                    self.lib = Some(Library::open(paths, &self.dirs));
                }
                self.redraw();
            }
        }
    }

    /// The scroll position of the current view: (items, items per screen, screen).
    /// `None` for views that do not scroll.
    fn scroll_state(&self, size: Size) -> Option<(usize, usize, usize)> {
        let h = size.h as i32;
        match self.view {
            View::Books => Some((self.lib.as_ref().map_or(0, |l| l.query(&self.query).len()), self.per_page(h).max(1), self.page)),
            View::Files => Some((self.files_rows(), Self::files_per_page(h), self.files_page)),
            View::Collections => Some((self.lib.as_ref().map_or(0, |l| l.index.collections.len()), Self::coll_per_page(h), self.coll_page)),
            _ => None,
        }
    }

    /// Shows screen `page` of the current view (clamped); only the scrolling region is
    /// repainted. False when the view does not scroll or is there already.
    fn scroll_to(&mut self, page: usize, size: Size) -> bool {
        self.scroll_state(size).is_some_and(|state| self.scroll_from(state, page))
    }

    /// [`Self::scroll_to`] from the view's [`Self::scroll_state`] (the library view's
    /// query runs once per step).
    fn scroll_from(&mut self, (total, per, now): (usize, usize, usize), page: usize) -> bool {
        let last = ScrollBar::pages_of(total, per) - 1;
        let (page, now) = (page.min(last), now.min(last));
        if page == now {
            return false;
        }
        match self.view {
            View::Books => self.page = page,
            View::Files => self.files_page = page,
            View::Collections => self.coll_page = page,
            _ => return false,
        }
        log::debug!("library: {:?} scrolled to screen {}", self.view, page + 1);
        self.scrolled = true;
        true
    }

    /// Moves the current view by `step` screens.
    fn turn(&mut self, step: i64, size: Size) {
        let Some(state @ (total, per, now)) = self.scroll_state(size) else { return };
        let now = now.min(ScrollBar::pages_of(total, per) - 1);
        self.scroll_from(state, (now as i64 + step).max(0) as usize);
    }

    /// Touches on the scroll bar: a press there takes the touch; moving shows the
    /// screen under the finger on the bar; the lift jumps there. True when the bar has
    /// the touch.
    fn bar_touch(&mut self, t: &ink_ui::Touch, size: Size) -> bool {
        use ink_ui::TouchKind;
        if t.kind == TouchKind::Down {
            self.bar_fired = false;
            let geom = self.bar.filter(|g| ScrollBar::hit_rect(g.region).contains(t.pos));
            self.bar_drag = geom.map(|g| BarDrag { slot: t.slot, page: g.bar.first / g.per, shown: false });
            return self.bar_drag.is_some();
        }
        let (Some(drag), Some(geom)) = (self.bar_drag, self.bar) else { return false };
        if drag.slot != t.slot {
            return false;
        }
        match t.kind {
            TouchKind::Move | TouchKind::Hold | TouchKind::Press => {
                let page = geom.page_at(t.pos.y);
                if page != drag.page {
                    self.bar_drag = Some(BarDrag { page, shown: true, ..drag });
                    self.bar_dirty = true;
                }
            }
            TouchKind::Up => {
                self.bar_drag = None;
                self.bar_fired = true;
                let page = geom.page_at(t.pos.y);
                log::info!("library: scroll bar lift at y {} = screen {}", t.pos.y, page + 1);
                // A preview on the bar is repainted with the rows (or alone).
                if !self.scroll_to(page, size) && drag.shown {
                    self.scrolled = true;
                }
            }
            TouchKind::Cancel => {
                self.bar_drag = None;
                self.scrolled |= drag.shown;
            }
            TouchKind::Down => {}
        }
        true
    }

    fn open_menu(&mut self, items: Vec<(String, String)>, kind: MenuKind, anchor: Hit, size: Size) {
        let a = self.hits.iter().find(|(_, h)| *h == anchor).map(|(r, _)| *r).unwrap_or(Rect::new(view::MARGIN, view::HEADER_H, 400, 0));
        let mut menu = Menu::new(items);
        let w = 620.min(size.w - 2 * view::MARGIN as u32);
        let max_rows = ((size.h as i32 - view::TAB_H - a.bottom() - 16) / menu.row_h as i32).max(1) as usize;
        let above = max_rows < menu.items.len().min(6);
        let room = if above { ((a.y - view::HEADER_H) / menu.row_h as i32).max(1) as usize } else { max_rows };
        menu.items.truncate(room);
        let h = menu.height();
        let x = a.x.clamp(view::MARGIN, (size.w as i32 - view::MARGIN - w as i32).max(view::MARGIN));
        let y = if above { a.y - 8 - h as i32 } else { a.bottom() + 8 };
        self.overlay = Some(Overlay::Menu { menu, rect: Rect::new(x, y, w, h), kind });
        self.redraw();
    }

    fn open_input(&mut self, purpose: InputFor, title: &str, text: &str) {
        let search = purpose == InputFor::Search;
        let mut input = TextInput::new(if search { "Title, author or series" } else { "Name" }, "name");
        input.set_text(text);
        input.focused = true;
        self.overlay = Some(Overlay::Input { search, title: title.to_string(), purpose, input, keyboard: Keyboard::with_language_list(&self.keyboard_layouts) });
        self.redraw();
    }

    fn handle_overlay(&mut self, ev: &UiEvent, size: Size) -> Transition {
        let Some(ov) = self.overlay.as_mut() else { return Transition::None };
        match ov {
            Overlay::Menu { menu, rect, kind } => {
                let (rect, kind) = (*rect, kind.clone());
                if let UiEvent::Gesture(Gesture::Tap { pos }) = ev {
                    let action = if rect.contains(*pos) {
                        match menu.event(ev, rect) {
                            EventResult::Action(a) => Some(a),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    self.overlay = None;
                    self.redraw();
                    if let (Some(a), MenuKind::Book(p)) = (&action, &kind) {
                        return self.book_menu_action(p.clone(), a, size);
                    }
                    if let Some(a) = action {
                        return self.menu_action(kind, &a);
                    }
                } else if matches!(ev, UiEvent::Key(k) if k.pressed && k.key == Key::Back) {
                    self.overlay = None;
                    self.redraw();
                }
            }
            Overlay::Confirm { dialog, kind } => {
                let kind = kind.clone();
                let r = match ev {
                    UiEvent::Key(k) if k.pressed && k.key == Key::Back => EventResult::Action("cancel".into()),
                    _ => dialog.event(ev, Dialog::frame(size)),
                };
                if let EventResult::Action(a) = r {
                    self.overlay = None;
                    self.redraw();
                    if a == "ok" {
                        self.confirmed(kind);
                    }
                }
            }
            Overlay::Input { search, purpose, input, keyboard, .. } => {
                let (search, purpose) = (*search, purpose.clone());
                let kb = keyboard_rect(size);
                if let UiEvent::Touch(_) = ev {
                    keyboard.event(ev, kb);
                    self.keyboard_dirty = true;
                }
                let mut done: Option<bool> = None; // Some(true) = accept, Some(false) = cancel
                let mut edited = false;
                for a in keyboard.take_actions() {
                    match input.apply(&a) {
                        ink_ui::input::InputResult::Edited => edited = true,
                        ink_ui::input::InputResult::Submitted => done = Some(true),
                        ink_ui::input::InputResult::Closed => done = Some(search),
                        ink_ui::input::InputResult::Unchanged => {
                            if a == KeyAction::Close {
                                done = Some(search);
                            }
                        }
                    }
                }
                let text = input.text.clone();
                if let UiEvent::Gesture(Gesture::Tap { pos }) = ev {
                    let field = field_rect(size, search);
                    if field.contains(*pos) {
                        input.event(ev, field);
                        self.keyboard_dirty = true;
                        self.field_dirty = true;
                        self.redraw();
                    } else if !kb.contains(*pos) {
                        if search {
                            done = Some(true);
                        } else {
                            let (cancel, ok) = name_buttons(size);
                            if cancel.contains(*pos) {
                                done = Some(false);
                            } else if ok.contains(*pos) {
                                done = Some(true);
                            }
                        }
                    }
                }
                if let UiEvent::Key(k) = ev {
                    if k.pressed && k.key == Key::Back {
                        done = Some(search);
                    }
                }
                if edited {
                    if search {
                        // The field now; the results once the keys rest (a list repaint
                        // per key took a frame and a large DU per letter).
                        self.query.search = text.clone();
                        self.page = 0;
                        self.keyboard_dirty = true;
                        self.field_dirty = true;
                        self.search_due = Some(Instant::now() + SEARCH_PAUSE);
                    } else if self.dirty.is_none() {
                        // The text changed: black and white is enough.
                        self.dirty = Some(RefreshClass::Du);
                    }
                }
                if let Some(accept) = done {
                    self.search_due = None;
                    self.overlay = None;
                    self.redraw();
                    if accept {
                        self.input_done(purpose, text.trim());
                    }
                }
            }
        }
        Transition::None
    }

    fn menu_action(&mut self, kind: MenuKind, a: &str) -> Transition {
        match kind {
            MenuKind::More => {
                log::info!("library: menu {a}");
                return match a {
                    "settings" => Transition::Push(Route::Settings),
                    "stats" => Transition::Push(Route::Statistics),
                    "import" => {
                        self.show(View::Import);
                        Transition::None
                    }
                    entry => {
                        if let Some((p, id)) = entry.strip_prefix(LIB_ENTRY).and_then(ink_plugin::registry::parse_saved_id) {
                            self.run_entry = Some((p.to_string(), id.to_string()));
                        }
                        Transition::None
                    }
                };
            }
            MenuKind::Sort => {
                if let Some((k, _)) = a.parse::<usize>().ok().and_then(|i| model::SORTS.get(i)) {
                    self.query.sort = *k;
                    self.page = 0;
                }
                self.save_prefs();
            }
            MenuKind::Filter => {
                if let Some(i) = a.strip_prefix('s').and_then(|i| i.parse::<usize>().ok()) {
                    self.query.status = StatusFilter::ALL.get(i).copied().unwrap_or(StatusFilter::All);
                    self.query.format = None;
                } else if let Some(f) = a.strip_prefix('f') {
                    self.query.format = Some(f.to_string());
                    self.query.status = StatusFilter::All;
                }
                self.page = 0;
                self.save_prefs();
            }
            MenuKind::Collections(p) => {
                if a == "new" {
                    self.open_input(InputFor::NewCollection(Some(p)), "New collection", "");
                } else if let (Some(name), Some(lib)) = (a.strip_prefix('c'), self.lib.as_mut()) {
                    lib.toggle_collection(&p, name);
                }
            }
            MenuKind::Book(_) => {}
            MenuKind::Plugin(p) => {
                if let Some(a) = a.parse::<usize>().ok().and_then(|i| self.plugin_actions.get(i)) {
                    self.run_action = Some((a.clone(), p));
                }
            }
            MenuKind::CollectionActions(name) => {
                if a == "rename" {
                    self.open_input(InputFor::Rename(name.clone()), "Rename collection", &name);
                } else if a == "delete" {
                    let body = format!("“{name}” is removed. Its books stay in the library.");
                    self.confirm_delete("Delete this collection?", body, Confirm::DeleteCollection(name));
                }
            }
        }
        self.redraw();
        Transition::None
    }

    /// Asks before a delete: Cancel or Delete, then [`Self::confirmed`].
    fn confirm_delete(&mut self, title: &str, body: String, kind: Confirm) {
        let dialog = Dialog { title: title.into(), body, buttons: menu_items(&[("Cancel", "cancel"), ("Delete", "ok")]) };
        self.overlay = Some(Overlay::Confirm { dialog, kind });
        self.redraw();
    }

    fn confirmed(&mut self, kind: Confirm) {
        let Some(lib) = self.lib.as_mut() else { return };
        match kind {
            Confirm::DeleteBook(p) => {
                if let Err(e) = lib.delete_book(&p) {
                    log::warn!("library: cannot delete {}: {e}", p.display());
                } else {
                    log::info!("library: deleted {}", p.display());
                    self.covers.clear();
                    if self.view == View::Details(p) {
                        self.view = self.back_to.clone();
                    }
                }
            }
            Confirm::DeleteCollection(name) => {
                lib.delete_collection(&name);
                if self.query.collection.as_deref() == Some(name.as_str()) {
                    self.query.collection = None;
                }
            }
        }
        self.redraw();
    }

    fn input_done(&mut self, purpose: InputFor, text: &str) {
        let Some(lib) = self.lib.as_mut() else { return };
        match purpose {
            InputFor::Search => {}
            InputFor::NewCollection(book) => {
                if text.is_empty() {
                    return;
                }
                match lib.create_collection(text) {
                    Ok(()) => {
                        if let Some(p) = book {
                            lib.toggle_collection(&p, text);
                        }
                    }
                    Err(e) => log::warn!("library: {e}"),
                }
            }
            InputFor::Rename(old) => {
                if let Err(e) = lib.rename_collection(&old, text) {
                    log::warn!("library: {e}");
                } else if self.query.collection.as_deref() == Some(old.as_str()) {
                    self.query.collection = Some(text.to_string());
                }
            }
        }
    }
}

/// Status choices on the details page, in menu order.
const STATUSES: [ReadingStatus; 4] = [ReadingStatus::New, ReadingStatus::Reading, ReadingStatus::Complete, ReadingStatus::Abandoned];

/// Owned `(label, action)` rows for a menu or dialog.
fn menu_items(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items.iter().map(|(l, a)| (l.to_string(), a.to_string())).collect()
}

/// A menu label with a leading "\u{2713} " when `on` (`ink_ui::Menu` draws it as an icon).
fn mark(on: bool, label: &str) -> String {
    if on {
        format!("✓ {label}")
    } else {
        label.to_string()
    }
}

impl AppScreen for LibraryScreen {
    fn name(&self) -> &'static str {
        "library"
    }

    fn on_enter(&mut self, cx: &mut AppCtx) -> anyhow::Result<()> {
        self.plugin_entries = cx.plugins.library_entries().to_vec();
        let nav = crate::plugin_entries::nav_chosen(cx.settings, cx.plugins);
        self.nav = if nav.len() >= 2 { nav.into_iter().take(5).collect() } else { ink_store::prefs::UiSettings::default().nav_items };
        self.keyboard_layouts = crate::screens::settings::keyboard_layouts::layouts(cx.settings);
        log::info!("library: enter, navigation bar {:?}", self.nav);
        if self.job.is_some() {
            // The worker has the library; it comes back in `poll_job`.
            self.dirty = Some(RefreshClass::Gc16);
            return Ok(());
        }
        match self.lib.as_mut() {
            None => {
                self.dirs = cx.settings.library_dirs.clone();
                self.paths = Some(cx.paths.clone());
                self.prefs = Some(model::prefs_path(cx.paths));
                if let Some(p) = &self.prefs {
                    self.mode = if model::load_prefs(p, &mut self.query) { ViewMode::Grid } else { ViewMode::List };
                }
                let mut lib = Library::open(cx.paths, &self.dirs);
                // The "Book cover" screensaver shows the last book read until one is opened.
                if let Some(b) = lib.index.recent(1).first() {
                    crate::sleep_cover::prepare(cx.paths, &b.path, cx.screen.panel_size());
                }
                // Metadata and covers of new books are read on a thread; the first
                // frame does not wait for them (it waited up to 1.5 s).
                lib.start_background();
                lib.save();
                self.lib = Some(lib);
            }
            Some(lib) => {
                if let Some(p) = self.opened.take() {
                    lib.refresh_state(&p);
                    lib.save();
                }
                // Books the reader went on to ("Open next"): opened then, read since.
                let more = take_opened();
                for (p, t) in &more {
                    lib.record_open_at(p, *t);
                    lib.refresh_state(p);
                }
                if !more.is_empty() {
                    lib.save();
                }
                if self.dirs != cx.settings.library_dirs {
                    log::info!("library: folders changed to {:?}; rescan", cx.settings.library_dirs);
                    self.dirs = cx.settings.library_dirs.clone();
                    lib.set_dirs(&self.dirs);
                    request(Request::Rescan);
                }
            }
        }
        self.plugin_actions = cx.plugins.library_actions().to_vec();
        // Nothing of the previous screen may stay on the panel.
        self.dirty = Some(RefreshClass::Gc16);
        // A plugin added books (OPDS download).
        if cx.plugins.take_rescan() {
            self.start_rescan();
        }
        let size = cx.screen.size();
        match take_request() {
            Some(Request::KoreaderImport) => {
                log::info!("library: KOReader import asked by settings");
                self.show(View::Import);
                let _ = self.act(Hit::Import, false, size);
            }
            Some(Request::Rescan) => {
                log::info!("library: rescan asked by settings");
                self.start_rescan();
            }
            Some(Request::Files) => {
                log::info!("library: file browser asked by settings");
                let _ = self.act(Hit::Browse, false, size);
            }
            None => {}
        }
        Ok(())
    }

    fn on_exit(&mut self, _cx: &mut AppCtx) {
        if let Some(lib) = self.lib.as_mut() {
            lib.save();
        }
    }

    fn handle(&mut self, ev: &UiEvent, cx: &mut AppCtx) -> anyhow::Result<Transition> {
        let size = cx.screen.size();
        if matches!(ev, UiEvent::Quit) {
            return Ok(Transition::Exit);
        }
        // Plugin events, overlay and timeout are handled by the app loop
        // (plugin_host::hooks::{app_handle, app_paint, app_timeout}).
        if self.job.is_some() {
            self.poll_job();
            if self.job.is_some() {
                // Only Quit leaves while the worker has the library.
                return Ok(Transition::None);
            }
        }
        // A plugin added books while the library is shown (calibre, OPDS): rescan now,
        // not only on the next on_enter.
        if cx.plugins.take_rescan() {
            log::info!("library: plugin asked for a rescan");
            self.start_rescan();
            return Ok(Transition::None);
        }
        let t = self.handle_own(ev, size);
        if let Some(plugin) = self.nav_plugin.take() {
            let page = cx.plugins.settings_pages().iter().find(|p| p.plugin == plugin).cloned();
            match page {
                Some(p) => {
                    log::info!("library: navigation opens plugin page {}/{}", p.plugin, p.id);
                    cx.plugins.open_settings_page(&p, None);
                }
                None => log::warn!("library: navigation item {plugin}: the plugin has no page (turned off?)"),
            }
        }
        if let Some(text) = self.toast.take() {
            crate::export::toast(cx.plugins, text);
        }
        if let Some((plugin, id)) = self.run_entry.take() {
            log::info!("library: plugin entry {plugin}:{id}");
            cx.plugins.trigger(&plugin, &id, None);
        }
        if let Some((action, book)) = self.run_action.take() {
            log::info!("library: plugin action {}:{} on {}", action.plugin, action.id, book.display());
            cx.plugins.library_action(&action, &book);
        }
        Ok(t)
    }

    fn paint(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        self.paint_own(cx)
    }

    fn timeout(&self) -> Option<Duration> {
        if self.job.is_some() {
            return Some(worker::POLL);
        }
        let search = self.search_due.map(|t| t.saturating_duration_since(Instant::now()));
        let extract = self.lib.as_ref().filter(|l| l.pending() > 0).map(|_| META_POLL);
        match (search, extract) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

impl LibraryScreen {
    fn handle_own(&mut self, ev: &UiEvent, size: Size) -> Transition {
        match ev {
            UiEvent::Key(k) if k.pressed && k.key == Key::Power => return Transition::Exit,
            UiEvent::Power(PowerEvent::LibraryRescan) => {
                self.start_rescan();
                return Transition::None;
            }
            UiEvent::Timer(_) => {
                if self.search_due.is_some_and(|t| Instant::now() >= t) {
                    self.search_due = None;
                    log::debug!("library: search '{}': results repainted after the keys rested", self.query.search);
                    if self.dirty.is_none() {
                        self.dirty = Some(RefreshClass::Du);
                    }
                }
                if let Some(lib) = self.lib.as_mut() {
                    // Books queued by a rescan while the thread ran start the next one.
                    lib.start_background();
                    if lib.take_background() > 0 && (lib.pending() == 0 || self.last_paint.elapsed() >= REPAINT_EVERY) {
                        self.redraw();
                    }
                }
                return Transition::None;
            }
            _ => {}
        }
        if self.overlay.is_some() {
            return self.handle_overlay(ev, size);
        }
        if let UiEvent::Touch(t) = ev {
            if self.bar_touch(t, size) {
                return Transition::None;
            }
            return self.nav_touch(t, size);
        }
        if let UiEvent::Gesture(_) = ev {
            // A hold on the bar, or the tap or swipe its lift ends with.
            if self.bar_drag.is_some() || std::mem::take(&mut self.bar_fired) {
                return Transition::None;
            }
        }
        if let UiEvent::Gesture(g) = ev {
            if self.nav_fired {
                self.nav_fired = false;
                if ink_ui::widget::gesture_pos(g).is_some_and(|p| self.nav_cell(p).is_some()) {
                    // The lift already opened the item.
                    return Transition::None;
                }
            }
        }
        match ev {
            UiEvent::Key(k) if k.pressed => match k.key {
                Key::PageNext => {
                    self.turn(1, size);
                    Transition::None
                }
                Key::PagePrev => {
                    self.turn(-1, size);
                    Transition::None
                }
                Key::Back if self.hits.iter().any(|(_, h)| *h == Hit::Back) => self.act(Hit::Back, false, size),
                Key::Home => self.open_nav("home", size),
                _ => Transition::None,
            },
            UiEvent::Gesture(Gesture::Tap { pos }) => match self.hit_at(*pos) {
                Some(h) => self.act(h, false, size),
                None => Transition::None,
            },
            UiEvent::Gesture(Gesture::LongPress { pos, .. }) => match self.hit_at(*pos) {
                Some(h @ (Hit::Book(_) | Hit::Collection(_))) => self.act(h, true, size),
                Some(h) => self.act(h, false, size),
                None => Transition::None,
            },
            UiEvent::Gesture(Gesture::Swipe { .. }) => {
                // Up (or left) shows the next screen, down (or right) the previous one.
                self.turn(crate::screens::common::swipe_step(ev).unwrap_or(0).into(), size);
                Transition::None
            }
            _ => Transition::None,
        }
    }

    /// The navigation bar cell at `p` (from the last paint).
    fn nav_cell(&self, p: ink_device::Point) -> Option<usize> {
        self.hits.iter().rev().find_map(|(r, h)| match h {
            Hit::Tab(i) if r.contains(p) => Some(*i),
            _ => None,
        })
    }

    /// Touches on the navigation bar: a press that lifts in the cell it went down in
    /// opens the item, however much the finger moved (the bottom edge of the panel
    /// reports jittery positions; such a press is no tap for the recogniser).
    fn nav_touch(&mut self, t: &ink_ui::Touch, size: Size) -> Transition {
        match t.kind {
            ink_ui::TouchKind::Down => {
                self.nav_fired = false;
                self.nav_press = self.nav_cell(t.pos).map(|i| (t.slot, i));
                if let Some((_, i)) = self.nav_press {
                    log::info!("library: navigation bar touch-down on cell {i} at {:?} (no feedback drawn)", t.pos);
                }
            }
            ink_ui::TouchKind::Up => {
                if let Some((slot, i)) = self.nav_press.take() {
                    let at = self.nav_cell(t.pos);
                    if slot == t.slot && at == Some(i) {
                        self.nav_fired = true;
                        log::info!("library: navigation bar lift on cell {i} at {:?} (down at {:?})", t.pos, t.start);
                        return self.act(Hit::Tab(i), false, size);
                    }
                    log::info!("library: navigation bar press left cell {i} (lift at {:?})", t.pos);
                }
            }
            ink_ui::TouchKind::Cancel => self.nav_press = None,
            _ => {}
        }
        Transition::None
    }

    fn paint_own(&mut self, cx: &mut AppCtx) -> anyhow::Result<Vec<Damage>> {
        if self.dirty.is_some() || self.overlay.is_some() {
            // A full repaint (or an overlay) shows the scroll position as it is.
            self.dirty = self.dirty.or(self.scrolled.then_some(RefreshClass::Gl16));
            self.scrolled = false;
            self.bar_dirty = false;
        }
        if std::mem::take(&mut self.scrolled) {
            // One screen further: everything is drawn (the same pixels outside), but only
            // the rows and the scroll bar are sent to the panel.
            self.bar_dirty = false;
            self.last_paint = Instant::now();
            let mut p = paint_cx(cx);
            self.draw(&mut p);
            let s = cx.screen.size();
            let r = Self::scroll_region(s.w as i32, s.h as i32);
            return Ok(vec![Damage { rect: Rect::new(0, r.y, s.w, r.h), class: RefreshClass::Gl16 }]);
        }
        if std::mem::take(&mut self.bar_dirty) {
            if let (Some(g), Some(d)) = (self.bar, self.bar_drag) {
                let mut p = paint_cx(cx);
                let track = ScrollBar::track(g.region);
                ScrollBar::paged(g.bar.total, g.per, d.page).paint_drag(&mut p, track);
                return Ok(vec![Damage { rect: track, class: RefreshClass::Du }]);
            }
        }
        if let Some(class) = self.dirty.take() {
            self.keyboard_dirty = false;
            self.field_dirty = false;
            self.last_paint = Instant::now();
            // With the keyboard open a DU (typing, live search results) stays above it:
            // DU would make the antialiased key captions jagged. Only the changed key faces
            // below get a GL16 (taken before `draw`, which drops them).
            let kb = keyboard_rect(cx.screen.size());
            let keys = match self.overlay.as_mut() {
                Some(Overlay::Input { keyboard, .. }) if class == RefreshClass::Du => {
                    let c = keyboard.refresh_class();
                    Some(keyboard.damage(kb).unwrap_or_else(|| vec![kb]).into_iter().map(|rect| Damage { rect, class: c }).collect::<Vec<_>>())
                }
                _ => None,
            };
            let mut p = paint_cx(cx);
            self.draw(&mut p);
            let bounds = cx.canvas.bounds();
            if let Some(keys) = keys {
                let mut out = vec![Damage { rect: Rect::new(0, 0, bounds.w, kb.y.max(0) as u32), class }];
                out.extend(keys);
                return Ok(out);
            }
            return Ok(vec![Damage { rect: bounds, class }]);
        }
        if std::mem::take(&mut self.busy_dirty) && self.job.is_some() {
            let mut p = paint_cx(cx);
            let r = self.draw_busy_line(&mut p);
            return Ok(vec![Damage { rect: r, class: RefreshClass::Du }]);
        }
        if std::mem::take(&mut self.keyboard_dirty) {
            let size = cx.screen.size();
            if let Some(Overlay::Input { search, input, keyboard, .. }) = self.overlay.as_mut() {
                let mut p = paint_cx(cx);
                let kb = keyboard_rect(size);
                p.canvas.fill_rect(kb.x, kb.y, kb.w as i32, kb.h as i32, p.theme.bg);
                keyboard.paint(&mut p, kb);
                let class = keyboard.refresh_class();
                let mut rects = keyboard.damage(kb).unwrap_or_else(|| vec![kb]);
                // A key press alone leaves the field as it is: no refresh there.
                if std::mem::take(&mut self.field_dirty) {
                    let field = field_rect(size, *search);
                    p.canvas.fill_rect(field.x, field.y, field.w as i32, field.h as i32, p.theme.bg);
                    input.paint(&mut p, field);
                    rects.push(field);
                }
                return Ok(rects.into_iter().map(|rect| Damage { rect, class }).collect());
            }
        }
        Ok(Vec::new())
    }
}

