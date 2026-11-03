//! Library tests: the model on copied test books, and the screen driven headless through
//! touch frames as the app's loop feeds it. With `INK_LIBRARY_OUT=DIR` the screen tests
//! write PNGs of the simulated panel there.

use super::*;
use crate::app::providers;
use crate::plugin_host::PluginHost;
use ink_device::headless::{self, HeadlessOptions, HeadlessProbe};
use ink_device::{Canvas, Device, InputEvent, Point, Timestamp, TouchContact, TouchFrame, TouchPhase};
use ink_store::library::SortKey;
use ink_store::{BookState, LibraryIndex, Paths, Settings};
use ink_text::Fonts;
use ink_ui::{GestureRecognizer, RefreshScheduler, ScreenSpace, Theme};
use std::path::{Path, PathBuf};

fn testdata(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(rel)
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-library-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A books folder with `files` copied from testdata, and a separate rinky home.
fn books(name: &str, files: &[&str]) -> (PathBuf, Paths) {
    let root = tmp(name);
    let dir = root.join("Books");
    std::fs::create_dir_all(&dir).unwrap();
    for f in files {
        let src = testdata(f);
        std::fs::copy(&src, dir.join(src.file_name().unwrap())).unwrap();
    }
    (dir, Paths { root: root.join("home") })
}

fn titles(v: &[&ink_store::BookEntry]) -> Vec<String> {
    v.iter().map(|b| b.title.clone()).collect()
}

#[test]
fn scan_reads_metadata_covers_and_refusals() {
    let (dir, paths) = books("scan", &["juliet.epub", "formats/sample.fb2", "formats/drm-adept.epub", "formats/alice.txt"]);
    let mut lib = Library::open(&paths, std::slice::from_ref(&dir));
    assert_eq!(lib.index.books.len(), 4);
    assert_eq!(lib.pending(), 4);
    assert_eq!(lib.extract_some(Duration::from_secs(60)), 4);
    assert_eq!(lib.pending(), 0);
    let juliet = lib.index.get(&dir.join("juliet.epub")).unwrap().clone();
    assert_eq!(juliet.title, "Romeo and Juliet");
    assert_eq!(juliet.authors, vec!["William Shakespeare"]);
    assert_eq!(juliet.md5.as_deref().map(str::len), Some(32));
    let cover = juliet.cover.expect("cover thumbnail");
    assert!(cover.starts_with(paths.covers()), "{}", cover.display());
    let img = Canvas::load_png(&cover).unwrap();
    assert!(img.width <= covers::THUMB_W as usize && img.height <= covers::THUMB_H as usize && img.height > 300);
    assert!(lib.index.get(&dir.join("drm-adept.epub")).unwrap().unsupported);
    assert!(!lib.index.get(&dir.join("sample.fb2")).unwrap().unsupported);
    // Saved: a fresh open finds the metadata and does not read the books again.
    let again = Library::open(&paths, std::slice::from_ref(&dir));
    assert_eq!(again.pending(), 0);
    assert_eq!(again.index.get(&dir.join("juliet.epub")).unwrap().title, "Romeo and Juliet");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn queries_sort_filter_and_search() {
    let (dir, paths) = books("query", &["juliet.epub", "formats/sample.fb2", "formats/alice.txt", "synthetic.epub"]);
    let mut lib = Library::open(&paths, std::slice::from_ref(&dir));
    lib.extract_some(Duration::from_secs(60));
    let by_title = lib.query(&Query { sort: SortKey::Title, ..Default::default() });
    let t = titles(&by_title);
    let mut sorted = t.clone();
    sorted.sort_by_key(|s| s.to_lowercase());
    assert_eq!(t, sorted);
    let epubs = lib.query(&Query { format: Some("epub".into()), ..Default::default() });
    assert_eq!(epubs.len(), 2);
    let found = lib.query(&Query { search: "shakesp".into(), ..Default::default() });
    assert_eq!(titles(&found), vec!["Romeo and Juliet"]);
    assert_eq!(lib.formats(), vec!["epub", "fb2", "txt"]);
    // Reading status: opening makes a book "Reading" and current; finished drops it.
    let j = dir.join("juliet.epub");
    lib.record_open(&j);
    let mut st = BookState::load(&j).unwrap();
    st.position = Some("/body/DocFragment[3].0".into());
    st.percent = Some(0.25);
    st.save(&j).unwrap();
    lib.refresh_state(&j);
    let b = lib.index.get(&j).unwrap();
    assert_eq!((b.status, b.percent), (ReadingStatus::Reading, Some(0.25)));
    assert_eq!(lib.current().map(|b| b.path.clone()), Some(j.clone()));
    assert_eq!(lib.query(&Query { status: StatusFilter::Reading, ..Default::default() }).len(), 1);
    assert_eq!(lib.query(&Query { status: StatusFilter::Unread, ..Default::default() }).len(), 3);
    assert_eq!(lib.query(&Query { sort: SortKey::Recent, ..Default::default() })[0].path, j);
    lib.set_status(&j, ReadingStatus::Complete);
    assert_eq!(BookState::load(&j).unwrap().status, ReadingStatus::Complete, "status saved in the sidecar");
    assert!(lib.current().is_none());
    assert_eq!(lib.query(&Query { status: StatusFilter::Finished, ..Default::default() }).len(), 1);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn delete_removes_the_book_and_our_files_but_not_koreader_files() {
    let (dir, paths) = books("delete", &["juliet.epub", "synthetic.epub"]);
    let mut lib = Library::open(&paths, std::slice::from_ref(&dir));
    lib.extract_some(Duration::from_secs(60));
    let (j, s) = (dir.join("juliet.epub"), dir.join("synthetic.epub"));
    let cover = lib.index.get(&j).unwrap().cover.clone().unwrap();
    for b in [&j, &s] {
        BookState { percent: Some(0.5), ..Default::default() }.save(b).unwrap();
    }
    // KOReader's file next to ours must survive.
    let ko = dir.join("juliet.sdr/metadata.epub.lua");
    std::fs::write(&ko, b"return { percent_finished = 0.5 }").unwrap();
    lib.delete_book(&j).unwrap();
    assert!(!j.exists() && !cover.exists());
    assert!(!ink_store::sidecar::sidecar_path(&j).exists());
    assert_eq!(std::fs::read(&ko).unwrap(), b"return { percent_finished = 0.5 }");
    assert!(lib.index.get(&j).is_none());
    // Without foreign files the whole .sdr goes.
    lib.delete_book(&s).unwrap();
    assert!(!dir.join("synthetic.sdr").exists());
    let saved = LibraryIndex::load(&paths.library()).unwrap();
    assert!(saved.books.is_empty());
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn collections_create_toggle_rename_delete() {
    let (dir, paths) = books("coll", &["juliet.epub", "synthetic.epub"]);
    let mut lib = Library::open(&paths, std::slice::from_ref(&dir));
    let j = dir.join("juliet.epub");
    lib.create_collection("Plays").unwrap();
    assert!(lib.create_collection("Plays").is_err());
    assert!(lib.create_collection("  ").is_err());
    assert!(lib.toggle_collection(&j, "Plays"));
    assert_eq!(lib.query(&Query { collection: Some("Plays".into()), ..Default::default() }).len(), 1);
    lib.rename_collection("Plays", "Drama").unwrap();
    assert_eq!(lib.index.get(&j).unwrap().collections, vec!["Drama"]);
    assert!(!lib.toggle_collection(&j, "Drama"));
    assert!(lib.index.collection_books("Drama").is_empty());
    lib.delete_collection("Drama");
    assert!(lib.index.collections.is_empty());
    assert_eq!(LibraryIndex::load(&paths.library()).unwrap().collections.len(), 0);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn koreader_import_reads_koreader_files_and_never_changes_them() {
    let (dir, paths) = books("import", &["juliet.epub", "synthetic.epub"]);
    let j = dir.join("juliet.epub");
    let lua = b"return { last_xpointer = \"/body/DocFragment[4]/body/p[3]/text().0\", percent_finished = 0.4, summary = { status = \"reading\", rating = 4 } }";
    let ko_file = dir.join("juliet.sdr/metadata.epub.lua");
    std::fs::create_dir_all(ko_file.parent().unwrap()).unwrap();
    std::fs::write(&ko_file, lua).unwrap();
    let ko = KoreaderDirs { root: dir.parent().unwrap().join("koreader") };
    let (stats_db, _) = ink_store::koreader::sqlite::write_sample_databases(&ko.settings_dir(), false, false).unwrap();
    let stats_before = std::fs::read(&stats_db).unwrap();
    let mut lib = Library::open(&paths, std::slice::from_ref(&dir));
    let r = lib.import_koreader(&ko);
    assert_eq!((r.books, r.positions, r.errors.len()), (1, 1, 0), "{r:?}");
    assert_eq!(std::fs::read(&ko_file).unwrap(), lua, "KOReader file unchanged");
    assert_eq!(std::fs::read(&stats_db).unwrap(), stats_before, "KOReader statistics unchanged");
    assert!(r.statistics.as_deref().is_some_and(|s| s.contains("page records")), "{r:?}");
    assert!(paths.statistics().exists());
    let st = BookState::load(&j).unwrap();
    assert_eq!(st.position.as_deref(), Some("/body/DocFragment[4]/body/p[3]/text().0"));
    let b = lib.index.get(&j).unwrap();
    assert_eq!((b.status, b.rating), (ReadingStatus::Reading, Some(4)));
    assert!((b.percent.unwrap() - 0.4).abs() < 1e-6);
    assert!(r.lines().iter().any(|l| l == "Reading positions: 1"));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

/// The screen with the context the app would give it, fed with touch frames.
struct Harness {
    dev: Device,
    probe: HeadlessProbe,
    canvas: Canvas,
    fonts: Fonts,
    theme: Theme,
    refresh: RefreshScheduler,
    plugins: PluginHost,
    settings: Settings,
    paths: Paths,
    rec: GestureRecognizer,
    screen: LibraryScreen,
    clock_ms: u64,
    out: Option<PathBuf>,
    transitions: Vec<Transition>,
    /// What the panel shows (the app loop's copy, `crate::shown`).
    shown: crate::shown::Shown,
}

macro_rules! cx {
    ($h:ident, $p:ident) => {
        AppCtx {
            canvas: &mut $h.canvas,
            screen: $h.dev.screen.as_mut(),
            fonts: &mut $h.fonts,
            theme: &$h.theme,
            refresh: &mut $h.refresh,
            plugins: &mut $h.plugins,
            settings: &$h.settings,
            paths: &$h.paths,
            providers: &$p,
            services: crate::router::Services {
                info: &$h.dev.info,
                light: $h.dev.light.as_mut(),
                battery: $h.dev.battery.as_mut(),
                power: $h.dev.power.as_mut(),
                wifi: $h.dev.wifi.as_mut(),
                usb: $h.dev.usb.as_mut(),
                clock: $h.dev.clock.as_mut(),
            },
            settings_update: &mut None,
        }
    };
}

impl Harness {
    fn new(test: &str, dir: &Path, paths: Paths) -> Self {
        let (mut dev, probe) = headless::open(HeadlessOptions::default());
        // As App::new: the screen keeps a copy of the panel for the refresh policy.
        let shown: crate::shown::Shown = Default::default();
        let inner = std::mem::replace(&mut dev.screen, headless::open(HeadlessOptions::default()).0.screen);
        dev.screen = Box::new(crate::shown::ShownScreen::new(inner, shown.clone()));
        let size = dev.screen.size();
        let space = ScreenSpace { panel: dev.screen.panel_size(), rotation: dev.screen.rotation() };
        let out = std::env::var_os("INK_LIBRARY_OUT").map(|d| PathBuf::from(d).join(test));
        let mut h = Harness {
            dev,
            probe,
            canvas: Canvas::new(size.w as usize, size.h as usize),
            fonts: Fonts::load(),
            theme: Theme::day(),
            refresh: RefreshScheduler::new(8),
            plugins: PluginHost::new(crate::plugins::compiled_in()),
            settings: Settings { library_dirs: vec![dir.to_path_buf()], ..Settings::default() },
            paths,
            rec: GestureRecognizer::new(space),
            screen: LibraryScreen::new(),
            clock_ms: 1000,
            out,
            transitions: Vec::new(),
            shown,
        };
        let p = providers();
        h.screen.on_enter(&mut cx!(h, p)).unwrap();
        h.settle_metadata();
        h.paint();
        h
    }

    /// Lets the metadata thread finish: timer events, as the app loop sends them while
    /// the screen has a timeout, until no book is left.
    fn settle_metadata(&mut self) {
        let t = std::time::Instant::now();
        while self.screen.library().is_some_and(|l| l.pending() > 0) {
            assert!(t.elapsed() < Duration::from_secs(60), "metadata thread did not finish");
            std::thread::sleep(Duration::from_millis(5));
            self.event(UiEvent::Timer(0));
        }
    }

    /// One frame as the app loop shows it (`app::present_over`: coalesced, cut to the
    /// changed pixels when it would flash).
    fn paint(&mut self) {
        let p = providers();
        let shown = self.shown.clone();
        let mut cx = cx!(self, p);
        let frame = crate::plugin_host::hooks::app_frame(&mut self.screen, &mut cx).unwrap();
        crate::app::present_over(&mut cx, frame, Some(&shown)).unwrap();
    }

    fn event(&mut self, ev: UiEvent) {
        let p = providers();
        // As App::run: plugins first, then the screen.
        let mut cx = cx!(self, p);
        let t = if crate::plugin_host::hooks::app_handle(&mut self.screen, &ev, &mut cx) {
            Transition::None
        } else {
            self.screen.handle(&ev, &mut cx).unwrap()
        };
        self.transitions.push(t);
        self.paint();
    }

    fn feed(&mut self, ev: InputEvent) {
        for ev in ink_ui::translate(&mut self.rec, ev) {
            self.event(ev);
        }
    }

    fn frame(&mut self, after_ms: u64, x: i32, y: i32, phase: TouchPhase) {
        self.clock_ms += after_ms;
        self.feed(InputEvent::Touch(TouchFrame { time: Timestamp::from_millis(self.clock_ms), contacts: vec![TouchContact { slot: 0, x, y, phase }] }));
    }

    fn tap_at(&mut self, p: Point) {
        self.frame(400, p.x, p.y, TouchPhase::Down);
        self.frame(60, p.x, p.y, TouchPhase::Up);
    }

    fn hold_at(&mut self, p: Point) {
        self.frame(400, p.x, p.y, TouchPhase::Down);
        self.frame(1000, p.x, p.y, TouchPhase::Up);
    }

    fn target(&self, want: impl Fn(&Hit) -> bool) -> Point {
        let (r, _) = self.screen.hits.iter().find(|(_, h)| want(h)).unwrap_or_else(|| panic!("no target among {:?}", self.screen.hits));
        Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2)
    }

    /// Taps the row with action `action` of the open menu.
    fn pick(&mut self, action: &str) {
        let Some(Overlay::Menu { rect, menu, .. }) = &self.screen.overlay else { panic!("no menu open") };
        let i = menu.items.iter().position(|(_, a)| a == action).unwrap_or_else(|| panic!("no {action} in {:?}", menu.items)) as i32;
        let pt = Point::new(rect.x + 100, rect.y + i * menu.row_h as i32 + menu.row_h as i32 / 2);
        self.tap_at(pt);
    }

    fn tap(&mut self, want: impl Fn(&Hit) -> bool) {
        let p = self.target(want);
        self.tap_at(p);
    }

    /// Shows the KOReader import page (reached from Settings, no tab any more).
    fn open_import(&mut self) {
        self.screen.show(View::Import);
        self.paint();
    }

    /// The overflow menu has no exit (only Settings exits); the power key leaves.
    fn exit_with_power(&mut self) {
        self.tap(|x| *x == Hit::More);
        let Some(Overlay::Menu { menu, .. }) = &self.screen.overlay else { panic!("overflow menu") };
        assert!(menu.items.iter().all(|(label, action)| !label.to_lowercase().contains("exit") && action != "exit"), "no exit in the overflow menu");
        self.event(UiEvent::Key(ink_device::KeyEvent { key: ink_device::Key::Power, pressed: true, time: ink_device::Timestamp::default() }));
    }

    fn type_text(&mut self, text: &str) {
        let size = self.dev.screen.size();
        for ch in text.chars() {
            let Some(Overlay::Input { keyboard, .. }) = self.screen.overlay.as_ref() else { panic!("no keyboard") };
            let kb = keyboard_rect(size);
            let (r, _) = keyboard.layout.key_rects(keyboard.keys_area(kb)).into_iter().find(|(_, k)| k.label == ch.to_string()).unwrap_or_else(|| panic!("key {ch}"));
            self.tap_at(Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2));
        }
    }

    fn shot(&self, name: &str) {
        if let Some(d) = &self.out {
            std::fs::create_dir_all(d).unwrap();
            self.probe.panel().save_png(&d.join(format!("{name}.png"))).unwrap();
        }
    }

    fn key(&mut self, key: Key) {
        self.event(UiEvent::Key(ink_device::KeyEvent { key, pressed: true, time: ink_device::Timestamp::default() }));
    }

    fn swipe(&mut self, up: bool) {
        let (a, b) = (Point::new(600, 1200), Point::new(600, 500));
        let (from, to) = if up { (a, b) } else { (b, a) };
        let dir = if up { ink_ui::Direction::Up } else { ink_ui::Direction::Down };
        self.event(UiEvent::Gesture(Gesture::Swipe { from, to, dir }));
    }

    /// Panel updates since `before` (an earlier `probe.updates().len()`).
    fn updates_since(&self, before: usize) -> Vec<ink_device::headless::UpdateRecord> {
        self.probe.updates().into_iter().skip(before).collect()
    }

    /// The first screen index of the current view.
    fn screen_index(&self) -> usize {
        match self.screen.view {
            View::Books => self.screen.page,
            View::Files => self.screen.files_page,
            View::Collections => self.screen.coll_page,
            _ => 0,
        }
    }

    /// Checks one scroll step to screen `want`: a single GL16 of the scrolling region
    /// (full width, from under the toolbar to the navigation bar), no flash. The app
    /// loop's `coalesce` widens a GL16 over 60 % of the screen to one full-screen GL16
    /// (still partial: only changed pixels are driven); it never becomes a GC16.
    fn assert_scrolled(&self, before: usize, want: usize) {
        assert_eq!(self.screen_index(), want);
        let ups = self.updates_since(before);
        let region = LibraryScreen::scroll_region(1272, 1696);
        let band = Rect::new(0, region.y, 1272, region.h);
        assert_eq!(ups.len(), 1, "one update per scroll step: {ups:?}");
        assert_eq!(ups[0].class, RefreshClass::Gl16, "no flash: {ups:?}");
        assert!(ups[0].rect == band || ups[0].rect == Rect::new(0, 0, 1272, 1696), "the scrolling region: {ups:?}");
    }

    fn pushed(&self) -> Vec<Route> {
        self.transitions.iter().filter_map(|t| if let Transition::Push(r) = t { Some(r.clone()) } else { None }).collect()
    }
}

#[test]
fn search_keys_refresh_the_field_and_results_wait_for_a_pause() {
    let (dir, paths) = books("search-pause", &["juliet.epub", "formats/alice.txt"]);
    let mut h = Harness::new("search-pause", &dir, paths);
    h.tap(|x| *x == Hit::Tab(1));
    h.tap(|x| *x == Hit::Search);
    let size = h.dev.screen.size();
    let kb = keyboard_rect(size);
    let field = field_rect(size, true);
    let key = |h: &Harness, label: &str| {
        let Some(Overlay::Input { keyboard, .. }) = h.screen.overlay.as_ref() else { panic!("no keyboard") };
        let (r, _) = keyboard.layout.key_rects(keyboard.keys_area(kb)).into_iter().find(|(_, k)| k.label == label).unwrap();
        r
    };
    h.type_text("al");
    let del = key(&h, "del");
    let at = Point::new(del.x + del.w as i32 / 2, del.y + del.h as i32 / 2);
    // Press: only the key face; the field did not change.
    let n = h.probe.updates().len();
    h.frame(400, at.x, at.y, TouchPhase::Down);
    let rects: Vec<Rect> = h.probe.updates()[n..].iter().map(|u| u.rect).collect();
    assert_eq!(rects, vec![del], "press refreshes only the key");
    // Release: the key face and the field, never the results above the keyboard.
    let n = h.probe.updates().len();
    h.frame(60, at.x, at.y, TouchPhase::Up);
    assert_eq!(h.screen.query.search, "a");
    let mut rects: Vec<Rect> = h.probe.updates()[n..].iter().map(|u| u.rect).collect();
    rects.sort_by_key(|r| r.y);
    assert_eq!(rects, vec![field, del], "release refreshes the key and the field");
    // A timer before the pause repaints nothing; after it, the results above the keyboard.
    let n = h.probe.updates().len();
    h.event(UiEvent::Timer(0));
    assert_eq!(h.probe.updates().len(), n, "results wait for the pause");
    assert!(h.screen.timeout().is_some_and(|t| t <= SEARCH_PAUSE));
    h.screen.search_due = Some(Instant::now());
    h.event(UiEvent::Timer(0));
    let ups = h.probe.updates()[n..].to_vec();
    assert_eq!(ups.iter().map(|u| (u.rect, u.class)).collect::<Vec<_>>(), vec![(Rect::new(0, 0, size.w, kb.y as u32), RefreshClass::Du)]);
    assert!(h.screen.search_due.is_none());
}

#[test]
fn home_lists_books_then_shows_continue_reading_after_the_reader() {
    let (dir, paths) = books("home", &["juliet.epub", "formats/sample.fb2", "formats/drm-adept.epub"]);
    let mut h = Harness::new("home", &dir, paths);
    h.shot("01-home-new");
    assert_eq!(h.screen.view, View::Home);
    // Before anything was read, the home page lists every book; the first row is under
    // the point the phase 0 library tests tap.
    let first = h.screen.hits.iter().find(|(_, x)| matches!(x, Hit::Book(_))).map(|(r, _)| *r).unwrap();
    assert!(first.contains(Point::new(500, 270)), "{first:?}");
    let j = dir.join("juliet.epub");
    h.tap(|x| *x == Hit::Book(j.clone()));
    assert_eq!(h.pushed(), vec![Route::Reader(j.clone())]);
    // The reader saved a position; back on the library the book is "continue reading".
    BookState { position: Some("/body/DocFragment[5].0".into()), percent: Some(0.3), ..Default::default() }.save(&j).unwrap();
    let p = providers();
    h.screen.on_enter(&mut cx!(h, p)).unwrap();
    h.paint();
    h.shot("02-home-continue");
    let card = h.screen.hits.iter().find(|(_, x)| *x == Hit::Book(j.clone())).map(|(r, _)| *r).unwrap();
    assert!(card.h > 300, "continue-reading card first: {card:?}");
    let b = h.screen.library().unwrap().index.get(&j).unwrap();
    assert_eq!((b.status, b.percent), (ReadingStatus::Reading, Some(0.3)));
    // A refused book goes to the reader too: its error screen pops back here.
    let drm = dir.join("drm-adept.epub");
    h.tap(|x| *x == Hit::Book(drm.clone()));
    assert_eq!(h.pushed().last(), Some(&Route::Reader(drm.clone())));
    let p = providers();
    h.screen.on_enter(&mut cx!(h, p)).unwrap();
    h.paint();
    let pt = h.target(|x| *x == Hit::Book(drm.clone()));
    h.hold_at(pt);
    h.pick("details");
    assert!(h.screen.refusal.as_deref().is_some_and(|r| r.contains("DRM")), "{:?}", h.screen.refusal);
    h.shot("03-drm-details");
    let d = h.screen.library().unwrap().index.get(&drm).unwrap();
    assert_eq!((d.status, d.last_opened), (ReadingStatus::New, None), "a refused book is not recorded as opened");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn home_empty_state_offers_a_rescan() {
    let (dir, paths) = books("home-empty", &[]);
    let h = Harness::new("home-empty", &dir, paths);
    h.shot("01-empty");
    assert!(h.screen.hits.iter().all(|(_, x)| !matches!(x, Hit::Book(_))));
    let (r, _) = h.screen.hits.iter().find(|(_, x)| *x == Hit::Rescan).unwrap();
    assert!(r.w >= 96 && r.h >= 96, "{r:?}");
    // The bottom navigation: four cells at least 96 px tall, Home selected.
    let tabs: Vec<Rect> = h.screen.hits.iter().filter(|(_, x)| matches!(x, Hit::Tab(_))).map(|(r, _)| *r).collect();
    assert_eq!(tabs.len(), 4);
    assert!(tabs.iter().all(|r| r.h >= 96 && r.w >= 96), "{tabs:?}");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn home_hero_shows_chapter_and_time_left_and_shelves() {
    let (dir, paths) = books("home-shelves", &["juliet.epub", "synthetic.epub", "formats/sample.fb2", "formats/alice.txt"]);
    let mut h = Harness::new("home-shelves", &dir, paths.clone());
    let (j, s) = (dir.join("juliet.epub"), dir.join("synthetic.epub"));
    // Read synthetic first, then juliet: juliet is current, synthetic on the shelf.
    for (b, pc) in [(&s, 0.6), (&j, 0.3)] {
        h.tap(|x| *x == Hit::Book(b.clone()));
        let mut st = BookState::load(b).unwrap();
        st.position = Some("/body/DocFragment[3].0".into());
        st.percent = Some(pc);
        if b == &j {
            st.extra.insert("chapter".into(), "Act II, Scene 2".into());
        }
        st.save(b).unwrap();
        let p = providers();
        h.screen.on_enter(&mut cx!(h, p)).unwrap();
        h.paint();
        // Different last-open seconds for a stable order.
        std::thread::sleep(Duration::from_millis(1100));
    }
    // 10 pages of juliet at 60 s each; juliet has 100 pages: 70 % left = 70 min.
    let md5 = h.screen.library().unwrap().index.get(&j).unwrap().md5.clone().unwrap();
    {
        let mut db = ink_store::StatsDb::open(&paths.statistics()).unwrap();
        let id = db.upsert_book(&ink_store::stats::StatsBook { md5, title: "Romeo and Juliet".into(), pages: Some(100), ..Default::default() }, 1000).unwrap();
        let sess = db.start_session(id, 1000).unwrap();
        let recs: Vec<_> = (0..10u32).map(|i| ink_store::stats::PageRecord { page: i + 1, total_pages: 100, start_time: 1000 + i as u64 * 60, duration: 60 }).collect();
        db.record_pages(sess, &recs).unwrap();
        db.end_session(sess, 1600).unwrap();
    }
    // The cache is per book and position: reading on reads the statistics again.
    h.tap(|x| *x == Hit::Book(j.clone()));
    let mut st = BookState::load(&j).unwrap();
    st.percent = Some(0.3001);
    st.save(&j).unwrap();
    let p = providers();
    h.screen.on_enter(&mut cx!(h, p)).unwrap();
    h.paint();
    h.shot("01-shelves");
    assert_eq!(h.screen.home.chapter.as_deref(), Some("Act II, Scene 2"));
    let entry = h.screen.library().unwrap().index.get(&j).unwrap().clone();
    let all = ink_store::StatsDb::open(&paths.statistics()).unwrap().all_books().unwrap();
    let left = h.screen.home.left_secs.unwrap_or_else(|| panic!("no time left: {:?} {:?} {:?}", entry.pages, entry.percent, all));
    assert!((4190..=4200).contains(&left), "{left}");
    assert_eq!(home::left_text(left), "1 h 10 min left in book");
    let books: Vec<(Rect, PathBuf)> =
        h.screen.hits.iter().filter_map(|(r, x)| if let Hit::Book(p) = x { Some((*r, p.clone())) } else { None }).collect();
    assert_eq!(books[0].1, j, "hero first");
    assert_eq!(books[0].0.h as i32, home::HERO_H);
    assert_eq!(books[1].1, s, "recently opened shelf");
    assert!(books.iter().any(|(_, p)| p.ends_with("alice.txt")), "want to read shelf: {books:?}");
    assert!(books.iter().all(|(r, _)| r.w >= 96 && r.h >= 96));
    // Every target ends above the navigation bar.
    let nav_top = h.screen.hits.iter().find(|(_, x)| *x == Hit::Tab(0)).unwrap().0.y;
    assert!(books.iter().all(|(r, _)| r.bottom() <= nav_top), "{books:?}");
    // Night mode paints the same layout.
    h.theme = Theme::night();
    h.screen.redraw();
    h.paint();
    h.shot("02-shelves-night");
    // The recently opened cover opens its book.
    h.tap(|x| *x == Hit::Book(s.clone()));
    assert_eq!(h.pushed().last(), Some(&Route::Reader(s.clone())));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn library_view_sort_filter_search_grid_and_pages() {
    let files = ["juliet.epub", "synthetic.epub", "formats/sample.fb2", "formats/alice.txt", "formats/epub3-nav.epub", "formats/alice-calibre.fb2", "formats/sample.html", "formats/epub2-ncx.epub", "formats/sample.fb3"];
    let (dir, paths) = books("view", &files);
    let mut h = Harness::new("view", &dir, paths);
    h.tap(|x| *x == Hit::Tab(1));
    assert_eq!(h.screen.view, View::Books);
    h.shot("01-list");
    let rows = h.screen.hits.iter().filter(|(_, x)| matches!(x, Hit::Book(_))).count();
    assert_eq!(rows, h.screen.per_page(1272, 1696).min(9));
    // Sort by title through the menu.
    h.tap(|x| *x == Hit::Sort);
    assert!(matches!(h.screen.overlay, Some(Overlay::Menu { .. })));
    h.shot("02-sort-menu");
    let Some(Overlay::Menu { rect, menu, .. }) = &h.screen.overlay else { unreachable!() };
    let i = menu.items.iter().position(|(l, _)| l.ends_with("Title")).unwrap() as i32;
    let p = Point::new(rect.x + 100, rect.y + i * menu.row_h as i32 + 40);
    h.tap_at(p);
    assert_eq!(h.screen.query.sort, SortKey::Title);
    let firsts: Vec<String> = h.screen.page_books(1272, 1696).0.iter().map(|b| b.title.to_lowercase()).collect();
    let mut s = firsts.clone();
    s.sort();
    assert_eq!(firsts, s);
    // Filter: FB2 only.
    h.tap(|x| *x == Hit::Filter);
    let Some(Overlay::Menu { rect, menu, .. }) = &h.screen.overlay else { panic!("filter menu") };
    let i = menu.items.iter().position(|(_, a)| a == "ffb2").unwrap() as i32;
    let p = Point::new(rect.x + 100, rect.y + i * menu.row_h as i32 + 40);
    h.tap_at(p);
    assert_eq!(h.screen.query.format.as_deref(), Some("fb2"));
    assert!(h.screen.page_books(1272, 1696).0.iter().all(|b| b.format == "fb2"));
    h.shot("03-filter-fb2");
    // Back to all; grid with pages.
    h.tap(|x| *x == Hit::Filter);
    let Some(Overlay::Menu { rect, .. }) = &h.screen.overlay else { panic!() };
    let p = Point::new(rect.x + 100, rect.y + 40);
    h.tap_at(p);
    assert_eq!((h.screen.query.format.as_deref(), h.screen.query.status), (None, StatusFilter::All));
    h.tap(|x| *x == Hit::ToggleMode);
    assert_eq!(h.screen.mode, ViewMode::Grid);
    h.shot("04-grid");
    // Nine books fill one screen of covers: no scroll bar.
    assert_eq!(h.screen.page_books(1272, 1696).0.len(), 9);
    assert!(h.screen.bar.is_none());
    // Search with the on-screen keyboard.
    h.tap(|x| *x == Hit::Search);
    h.type_text("alice");
    assert_eq!(h.screen.query.search, "alice");
    let found = h.screen.page_books(1272, 1696).0;
    assert!(!found.is_empty() && found.iter().all(|b| b.title.to_lowercase().contains("alice")), "{found:?}");
    h.shot("06-search");
    // Done closes the keyboard and keeps the search.
    h.tap(|x| *x == Hit::Back);
    assert!(h.screen.overlay.is_none());
    assert_eq!(h.screen.query.search, "alice");
    assert!(h.transitions.iter().all(|t| *t != Transition::Exit), "Done is not Exit");
    h.shot("07-search-done");
    // Sort and grid survive a restart; the search does not.
    let h2 = Harness::new("view2", &dir, h.paths.clone());
    assert_eq!((h2.screen.query.sort, h2.screen.mode, h2.screen.query.search.as_str()), (SortKey::Title, ViewMode::Grid, ""));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn details_status_collections_and_delete_with_confirmation() {
    let (dir, paths) = books("details", &["juliet.epub", "synthetic.epub"]);
    let mut h = Harness::new("details", &dir, paths);
    let j = dir.join("juliet.epub");
    let p = h.target(|x| *x == Hit::Book(j.clone()));
    h.hold_at(p);
    h.pick("details");
    assert_eq!(h.screen.view, View::Details(j.clone()));
    h.shot("01-details");
    // Reading status -> Finished.
    h.tap(|x| matches!(x, Hit::SetStatus(_, 2)));
    assert!(h.screen.overlay.is_none());
    assert_eq!(h.screen.library().unwrap().index.get(&j).unwrap().status, ReadingStatus::Complete);
    // Collections -> New collection… -> type "plays" -> OK.
    h.tap(|x| matches!(x, Hit::CollectionMenu(_)));
    let Some(Overlay::Menu { rect, menu, .. }) = &h.screen.overlay else { panic!("collections menu") };
    let i = menu.items.iter().position(|(_, a)| a == "new").unwrap() as i32;
    let pt = Point::new(rect.x + 100, rect.y + i * menu.row_h as i32 + 40);
    h.tap_at(pt);
    h.type_text("plays");
    h.shot("02-new-collection");
    let (_, ok) = name_buttons(h.dev.screen.size());
    h.tap_at(Point::new(ok.x + 20, ok.y + 20));
    assert!(h.screen.overlay.is_none());
    assert_eq!(h.screen.library().unwrap().index.get(&j).unwrap().collections, vec!["plays"]);
    h.shot("03-details-after");
    // Delete: Cancel keeps the book, Delete removes it and returns to the list.
    h.tap(|x| matches!(x, Hit::Delete(_)));
    assert!(matches!(h.screen.overlay, Some(Overlay::Confirm { .. })));
    h.shot("04-delete-confirm");
    let f = Dialog::frame(h.dev.screen.size());
    // Buttons sit at the bottom of the dialog: Cancel left, Delete right.
    let y = f.bottom() - 24 - 55;
    h.tap_at(Point::new(f.x + f.w as i32 / 4, y));
    assert!(j.exists() && h.screen.overlay.is_none());
    h.tap(|x| matches!(x, Hit::Delete(_)));
    h.tap_at(Point::new(f.x + f.w as i32 * 3 / 4, y));
    assert!(!j.exists());
    assert_eq!(h.screen.view, View::Home);
    assert!(h.screen.library().unwrap().index.get(&j).is_none());
    h.shot("05-after-delete");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn collections_tab_opens_a_collection_and_renames_it() {
    let (dir, paths) = books("colltab", &["juliet.epub", "synthetic.epub"]);
    let mut h = Harness::new("colltab", &dir, paths);
    h.tap(|x| *x == Hit::Tab(2));
    h.tap(|x| *x == Hit::NewCollection);
    h.type_text("tests");
    let (_, ok) = name_buttons(h.dev.screen.size());
    h.tap_at(Point::new(ok.x + 20, ok.y + 20));
    let s = dir.join("synthetic.epub");
    if let Some(lib) = h.screen.lib.as_mut() {
        lib.toggle_collection(&s, "tests");
    }
    h.screen.redraw();
    h.paint();
    h.shot("01-collections");
    h.tap(|x| *x == Hit::Collection("tests".into()));
    assert_eq!((h.screen.view.clone(), h.screen.query.collection.as_deref()), (View::Books, Some("tests")));
    let shown = h.screen.page_books(1272, 1696).0;
    assert_eq!(shown.len(), 1);
    h.shot("02-collection-books");
    h.tap(|x| *x == Hit::Back);
    assert_eq!(h.screen.view, View::Collections);
    // Hold -> Rename.
    let p = h.target(|x| *x == Hit::Collection("tests".into()));
    h.hold_at(p);
    let Some(Overlay::Menu { rect, .. }) = &h.screen.overlay else { panic!("actions menu") };
    let pt = Point::new(rect.x + 100, rect.y + 40);
    h.tap_at(pt);
    for _ in 0..5 {
        // Clear "tests" with backspace.
        let Some(Overlay::Input { keyboard, .. }) = h.screen.overlay.as_ref() else { panic!() };
        let kb = keyboard_rect(h.dev.screen.size());
        let (r, _) = keyboard.layout.key_rects(keyboard.keys_area(kb)).into_iter().find(|(_, k)| k.action == KeyAction::Backspace).unwrap();
        h.tap_at(Point::new(r.x + r.w as i32 / 2, r.y + r.h as i32 / 2));
    }
    h.type_text("misc");
    h.tap_at(Point::new(ok.x + 20, ok.y + 20));
    let lib = h.screen.library().unwrap();
    assert_eq!(lib.index.collections.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["misc"]);
    assert_eq!(lib.index.get(&s).unwrap().collections, vec!["misc"]);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn metadata_is_read_on_timer_ticks_and_exit_keys_leave() {
    let (dir, paths) = books("ticks", &["juliet.epub"]);
    let mut h = Harness::new("ticks", &dir, paths);
    assert_eq!(h.screen.timeout(), None, "the metadata thread read the book");
    // Queue a changed book: the next timer tick starts the thread, later ticks take
    // its result.
    if let Some(lib) = h.screen.lib.as_mut() {
        if let Some(b) = lib.index.books.first_mut() {
            b.meta_mtime = 0;
            b.title = "x".into();
        }
        lib.rescan();
    }
    assert!(h.screen.timeout().is_some());
    h.settle_metadata();
    assert_eq!(h.screen.timeout(), None);
    assert_eq!(h.screen.library().unwrap().index.books[0].title, "Romeo and Juliet");
    h.event(UiEvent::Power(PowerEvent::LibraryRescan));
    h.exit_with_power();
    assert_eq!(h.transitions.last(), Some(&Transition::Exit));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn koreader_tab_imports() {
    let (dir, paths) = books("kotab", &["juliet.epub"]);
    let ko_file = dir.join("juliet.sdr/metadata.epub.lua");
    std::fs::create_dir_all(ko_file.parent().unwrap()).unwrap();
    std::fs::write(&ko_file, b"return { percent_finished = 0.5, last_xpointer = \"/body/DocFragment[2].0\" }").unwrap();
    let mut h = Harness::new("kotab", &dir, paths);
    h.screen.ko = KoreaderDirs { root: dir.parent().unwrap().join("no-koreader") };
    h.open_import();
    h.shot("01-import");
    h.tap(|x| *x == Hit::Import);
    h.shot("02-imported");
    let lines = h.screen.import_result.clone().expect("result shown");
    assert!(lines.contains(&"Reading positions: 1".to_string()), "{lines:?}");
    assert_eq!(h.screen.library().unwrap().index.books[0].percent, Some(0.5));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

/// A Rust plugin with one library action that shows a toast.
struct ToastPlugin(ink_plugin::PluginManifest);

impl ink_plugin::Plugin for ToastPlugin {
    fn manifest(&self) -> &ink_plugin::PluginManifest {
        &self.0
    }
    fn on_load(&mut self, reg: &mut ink_plugin::Registrar) -> ink_plugin::Result<()> {
        reg.library_action("hello", "Say hello");
        Ok(())
    }
    fn on_library_action(&mut self, cx: &mut ink_plugin::HostCtx, id: &str, book: &Path) -> ink_plugin::Result<()> {
        cx.ui.toast(&format!("{id}: {}", book.file_name().unwrap().to_string_lossy()));
        Ok(())
    }
}

#[test]
fn plugin_library_action_runs_from_the_details_page_and_its_toast_shows() {
    let (dir, paths) = books("plugin", &["juliet.epub"]);
    let manifest = ink_plugin::PluginManifest::parse("name = \"libtoast\"\nversion = \"0.1.0\"\npermissions = [\"ui\"]\n").unwrap();
    let mut h = Harness::new("plugin", &dir, paths);
    h.plugins = PluginHost::new(vec![Box::new(ToastPlugin(manifest))]);
    let p = providers();
    h.screen.on_enter(&mut cx!(h, p)).unwrap();
    h.paint();
    assert_eq!(h.screen.plugin_actions.len(), 1);
    let j = dir.join("juliet.epub");
    let pt = h.target(|x| *x == Hit::Book(j.clone()));
    h.hold_at(pt);
    h.pick("details");
    h.tap(|x| matches!(x, Hit::PluginMenu(_)));
    let Some(Overlay::Menu { rect, .. }) = &h.screen.overlay else { panic!("plugin menu") };
    let pt = Point::new(rect.x + 100, rect.y + 40);
    let before = h.probe.updates().len();
    h.tap_at(pt);
    assert!(crate::plugin_host::hooks::app_timeout(h.screen.timeout(), &h.plugins).is_some(), "toast expiry is folded into the timeout");
    assert!(h.probe.updates().len() > before);
    h.shot("01-toast");
    // Settings is in the navigation bar, so the app bar has no gear icon for it.
    assert!(!h.screen.hits.iter().any(|(_, x)| *x == Hit::Settings));
    let i = h.screen.nav.iter().position(|n| n == "settings").expect("settings in the nav bar");
    h.tap(|x| *x == Hit::Tab(i));
    assert_eq!(h.pushed().last(), Some(&Route::Settings));
    // The top-right corner no longer exits; only Settings and the power key do.
    h.tap_at(Point::new(1265, 8));
    assert_ne!(h.transitions.last(), Some(&Transition::Exit));
    h.exit_with_power();
    assert_eq!(h.transitions.last(), Some(&Transition::Exit));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn a_worker_job_shows_progress_blocks_taps_and_gives_the_library_back() {
    worker::set_inline(false);
    let (dir, paths) = books("busy", &["juliet.epub"]);
    let mut h = Harness::new("busy", &dir, paths);
    let lib = h.screen.lib.take().unwrap();
    let (go, rx) = std::sync::mpsc::channel::<()>();
    // The progress comes after the first paint (else that paint shows it and no DU of
    // the progress line follows: a race under load).
    let (shown, painted) = std::sync::mpsc::channel::<()>();
    h.screen.start_job(Job::spawn("Rescanning the library", move |r: &Reporter<LibJob>| {
        painted.recv().unwrap();
        r.progress("reading the book folders");
        rx.recv().unwrap();
        LibJob::Rescan(lib)
    }));
    h.paint();
    shown.send(()).unwrap();
    assert_eq!(h.screen.timeout(), Some(worker::POLL));
    let start = Instant::now();
    while !h.screen.busy_text.contains("reading") && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
        h.event(UiEvent::Timer(0));
    }
    assert!(h.screen.busy_text.starts_with("Rescanning the library… reading the book folders"), "{}", h.screen.busy_text);
    h.shot("01-busy");
    // The progress line is shown with a DU update of its own rectangle.
    assert!(h.probe.updates().iter().any(|u| u.class == RefreshClass::Du && u.rect.y == view::HEADER_H + 220), "{:?}", h.probe.updates());
    // Taps wait while the worker has the library.
    h.tap(|x| *x == Hit::More);
    assert_eq!(h.transitions.last(), Some(&Transition::None));
    assert!(h.screen.overlay.is_none());
    go.send(()).unwrap();
    while h.screen.job.is_some() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
        h.event(UiEvent::Timer(0));
    }
    worker::set_inline(true);
    assert_eq!(h.screen.library().unwrap().index.books.len(), 1);
    assert_eq!(h.screen.timeout(), None);
    h.shot("02-back");
    h.exit_with_power();
    assert_eq!(h.transitions.last(), Some(&Transition::Exit));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn import_reports_progress_per_book() {
    let (dir, paths) = books("import-progress", &["juliet.epub", "synthetic.epub"]);
    let mut lib = Library::open(&paths, std::slice::from_ref(&dir));
    let ko = KoreaderDirs { root: dir.parent().unwrap().join("no-koreader") };
    let mut seen = Vec::new();
    lib.import_koreader_with(&ko, &mut |i, n| seen.push((i, n)));
    assert_eq!(seen, vec![(0, 2), (1, 2), (2, 2)]);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn file_browser_walks_folders_and_opens_a_book() {
    let (dir, paths) = books("files", &["juliet.epub"]);
    std::fs::write(dir.join("readme.doc"), b"not a book").unwrap();
    let mut h = Harness::new("files", &dir, paths);
    h.open_import();
    h.tap(|x| *x == Hit::Browse);
    assert_eq!(h.screen.view, View::Files);
    let start = h.screen.files_dir.clone();
    if !std::path::Path::new("/mnt/us").is_dir() {
        assert_eq!(start, dir.parent().unwrap());
    }
    h.shot("01-start");
    // Into the books folder: the book is listed, the .doc is not.
    h.screen.files_dir = dir.clone();
    h.tap(|x| *x == Hit::FileUp);
    h.tap(|x| *x == Hit::File(dir.clone()));
    assert_eq!(h.screen.files_dir, dir);
    let names: Vec<&str> = h.screen.files.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"juliet.epub") && !names.contains(&"readme.doc"), "{names:?}");
    h.shot("02-books");
    let book = dir.join("juliet.epub");
    h.tap(|x| *x == Hit::File(book.clone()));
    assert_eq!(h.pushed(), vec![Route::Reader(book)]);
    // Back from the browser returns to the KOReader tab.
    h.tap(|x| *x == Hit::Back);
    assert_eq!(h.screen.view, View::Import);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn status_chips_clear_search_badges_and_one_chip_row() {
    let (dir, paths) = books("chips", &["juliet.epub", "synthetic.epub", "formats/sample.fb2"]);
    let mut h = Harness::new("chips", &dir, paths);
    h.tap(|x| *x == Hit::Tab(1));
    // Every chip sits on one row inside the margins, at least 96 px tall.
    let chips: Vec<Rect> = h.screen.hits.iter().filter(|(_, x)| matches!(x, Hit::Sort | Hit::Filter | Hit::StatusFilter(_))).map(|(r, _)| *r).collect();
    assert_eq!(chips.len(), 6);
    assert!(chips.iter().all(|r| r.y == chips[0].y && r.h >= 96 && r.x >= view::MARGIN && r.right() <= 1272 - view::MARGIN), "{chips:?}");
    let j = dir.join("juliet.epub");
    h.screen.lib.as_mut().unwrap().set_status(&j, ReadingStatus::Complete);
    h.tap(|x| *x == Hit::StatusFilter(3));
    assert_eq!(h.screen.query.status, StatusFilter::Finished);
    let shown = h.screen.page_books(1272, 1696).0;
    assert_eq!(shown.iter().map(|b| b.path.clone()).collect::<Vec<_>>(), vec![j.clone()]);
    h.shot("01-finished");
    assert_eq!(super::browse::book_state(&shown[0]), Err(super::browse::Badge::Finished));
    h.tap(|x| *x == Hit::StatusFilter(0));
    assert_eq!(h.screen.query.status, StatusFilter::All);
    // The clear button in the search field empties the search.
    h.screen.query.search = "zzz".into();
    h.screen.redraw();
    h.paint();
    assert!(h.screen.page_books(1272, 1696).0.is_empty());
    h.tap(|x| *x == Hit::ClearSearch);
    assert_eq!(h.screen.query.search, "");
    assert_eq!(h.screen.page_books(1272, 1696).0.len(), 3);
    // Grid: one book cell per book, the mode button in the app bar switches back.
    h.tap(|x| *x == Hit::ToggleMode);
    assert_eq!(h.screen.mode, ViewMode::Grid);
    h.shot("02-grid");
    h.tap(|x| *x == Hit::ToggleMode);
    assert_eq!(h.screen.mode, ViewMode::List);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn nav_bar_reacts_on_its_whole_height_and_to_a_jittery_press() {
    let (dir, paths) = books("nav", &["juliet.epub"]);
    let mut h = Harness::new("nav", &dir, paths);
    let cells: Vec<Rect> = h.screen.hits.iter().filter(|(_, x)| matches!(x, Hit::Tab(_))).map(|(r, _)| *r).collect();
    assert_eq!(cells.len(), 4, "the default ui.nav_items");
    assert!(cells.iter().all(|r| r.bottom() == 1696 && r.y <= 1570 && r.h >= 96), "{cells:?}");
    // A press that moves 100 px inside the Library cell: no tap for the recogniser.
    let c = cells[1];
    h.frame(400, c.x + 40, 1575, TouchPhase::Down);
    h.frame(30, c.x + 140, 1675, TouchPhase::Move);
    h.frame(30, c.x + 140, 1675, TouchPhase::Up);
    assert_eq!(h.screen.view, View::Books);
    // The last item opens Settings with a plain tap at the very bottom.
    h.tap_at(Point::new(cells[3].x + 60, 1690));
    assert_eq!(h.pushed().last(), Some(&Route::Settings));
    // Nothing is drawn in the last 8 rows.
    let panel = h.probe.panel();
    assert!((1688..1696).all(|y| (0..1272).all(|x| panel.get(x, y) == Some(0xFF))));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

/// A plugin library entry saved in `ui.nav_items` is a navigation bar item with the
/// plugin's icon and label (ids no plugin answers are skipped); a tap on it runs the
/// plugin and does not leave the page. The overflow menu lists the entry too.
#[test]
fn plugin_library_entry_in_the_nav_bar_and_the_overflow_menu() {
    let (dir, paths) = books("navplugin", &["juliet.epub"]);
    let mut h = Harness::new("navplugin", &dir, paths);
    let calls: crate::plugin_entries::test_plugin::Calls = Default::default();
    h.plugins = crate::plugin_entries::test_plugin::Probe::host(&calls);
    h.settings.ui.nav_items = ["home", "plugin:gone:x", "plugin:probe:lib", "library"].map(String::from).to_vec();
    let p = providers();
    h.screen.on_enter(&mut cx!(h, p)).unwrap();
    h.paint();
    assert_eq!(h.screen.nav, ["home", "plugin:probe:lib", "library"]);
    let cells: Vec<Rect> = h.screen.hits.iter().filter(|(_, x)| matches!(x, Hit::Tab(_))).map(|(r, _)| *r).collect();
    assert_eq!(cells.len(), 3);
    let view = h.screen.view.clone();
    h.tap(|x| *x == Hit::Tab(1));
    assert_eq!(*calls.borrow(), ["lib"], "the plugin got on_action(lib)");
    assert_eq!(h.screen.view, view, "the page stays");
    // Overflow menu: the built-in rows, then the plugin entry.
    h.tap(|x| *x == Hit::More);
    let Some(Overlay::Menu { menu, .. }) = &h.screen.overlay else { panic!("overflow menu") };
    assert_eq!(menu.items.last().map(|i| (i.0.as_str(), i.1.as_str())), Some(("Probe lib", "lib:plugin:probe:lib")));
    h.pick("lib:plugin:probe:lib");
    assert_eq!(*calls.borrow(), ["lib", "lib"]);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn holding_a_book_opens_its_menu_in_one_update_and_a_tap_outside_closes_it() {
    let (dir, paths) = books("bookmenu", &["juliet.epub", "synthetic.epub"]);
    let mut h = Harness::new("bookmenu", &dir, paths);
    let j = dir.join("juliet.epub");
    for grid in [false, true] {
        h.tap(|x| *x == Hit::Tab(1));
        if (h.screen.mode == ViewMode::Grid) != grid {
            h.tap(|x| *x == Hit::ToggleMode);
        }
        let pt = h.target(|x| *x == Hit::Book(j.clone()));
        let n = h.probe.updates().len();
        h.hold_at(pt);
        let Some(Overlay::Menu { menu, kind, .. }) = &h.screen.overlay else { panic!("book menu") };
        assert_eq!(kind, &MenuKind::Book(j.clone()));
        let actions: Vec<&str> = menu.items.iter().map(|(_, a)| a.as_str()).collect();
        assert_eq!(actions, ["open", "details", "finished", "collections", "export", "delete"]);
        let ups = h.probe.updates()[n..].to_vec();
        assert_eq!(ups.len(), 1, "one update shows the menu: {ups:?}");
        assert_ne!(ups[0].class, RefreshClass::Gc16, "no flash");
        h.shot(if grid { "02-grid-menu" } else { "01-list-menu" });
        // A tap outside the menu closes it, in one update, and opens nothing.
        let Some(Overlay::Menu { rect, .. }) = &h.screen.overlay else { panic!() };
        let outside = if rect.y > 400 { Point::new(rect.x + 50, rect.y - 40) } else { Point::new(rect.x + 50, rect.bottom() + 40) };
        let n = h.probe.updates().len();
        h.tap_at(outside);
        assert!(h.screen.overlay.is_none());
        assert_eq!(h.probe.updates().len() - n, 1, "one update closes the menu");
        assert_eq!(h.screen.view, View::Books);
        assert!(h.pushed().is_empty(), "{:?}", h.pushed());
    }
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn book_menu_marks_finished_then_unread_and_asks_before_delete() {
    let (dir, paths) = books("bookmenu-act", &["juliet.epub", "synthetic.epub"]);
    let mut h = Harness::new("bookmenu-act", &dir, paths);
    let j = dir.join("juliet.epub");
    let status = |h: &Harness| h.screen.library().unwrap().index.get(&j).unwrap().status;
    let hold = |h: &mut Harness| {
        let pt = h.target(|x| *x == Hit::Book(j.clone()));
        h.hold_at(pt);
    };
    // On Home first, then in the Library list (Home hides finished books).
    assert_eq!(h.screen.view, View::Home);
    hold(&mut h);
    h.pick("finished");
    assert_eq!(status(&h), ReadingStatus::Complete);
    assert!(h.screen.overlay.is_none());
    h.tap(|x| *x == Hit::Tab(1));
    hold(&mut h);
    h.pick("unread");
    assert_eq!(status(&h), ReadingStatus::New);
    hold(&mut h);
    h.pick("delete");
    assert!(matches!(&h.screen.overlay, Some(Overlay::Confirm { kind: Confirm::DeleteBook(p), .. }) if *p == j));
    assert!(j.exists(), "nothing deleted before the confirmation");
    // Back cancels the dialog; then Open pushes the reader.
    h.event(UiEvent::Key(ink_device::KeyEvent { key: Key::Back, pressed: true, time: ink_device::Timestamp::default() }));
    assert!(h.screen.overlay.is_none() && j.exists());
    hold(&mut h);
    h.pick("open");
    assert_eq!(h.pushed().last(), Some(&Route::Reader(j.clone())));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn book_menu_exports_highlights_to_markdown_and_toasts() {
    let (dir, paths) = books("bookmenu-export", &["juliet.epub", "synthetic.epub"]);
    let mut h = Harness::new("bookmenu-export", &dir, paths);
    let j = dir.join("juliet.epub");
    let hold = |h: &mut Harness| {
        let pt = h.target(|x| *x == Hit::Book(j.clone()));
        h.hold_at(pt);
    };
    // No highlights: a toast, no file.
    let toasts = |h: &Harness| h.plugins.ui.pending() + h.plugins.ui.layer_names().iter().filter(|n| **n == "toast").count();
    let before = toasts(&h);
    hold(&mut h);
    h.pick("export");
    assert_eq!(toasts(&h), before + 1);
    assert!(!h.paths.exports().exists());
    // With one highlight: the file exists under the paths' exports dir.
    ink_store::BookState {
        highlights: vec![ink_store::sidecar::Highlight {
            start: "/body/DocFragment[2]/body/p[1]/text().0".into(),
            end: "/body/DocFragment[2]/body/p[1]/text().5".into(),
            text: "Two households".into(),
            created: "2026-10-30 21:04:11".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
    .save(&j)
    .unwrap();
    hold(&mut h);
    h.pick("export");
    assert!(toasts(&h) >= 1, "a toast for the second export too");
    let files: Vec<_> = std::fs::read_dir(h.paths.exports()).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    assert!(std::fs::read_to_string(files[0].path()).unwrap().contains("> Two households"));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

#[test]
fn one_name_for_the_unread_state() {
    assert_eq!(super::browse::Badge::New.label(), "Unread");
    assert_eq!(super::model::status_label(ReadingStatus::New), "Unread");
    assert_eq!(StatusFilter::Unread.label(), "Unread");
    assert_eq!(super::browse::STATUS_CHIPS[1], "Unread");
}

/// The updates since `n`, as (rect, class).
fn since(h: &Harness, n: usize) -> Vec<(Rect, RefreshClass)> {
    h.probe.updates()[n..].iter().map(|u| (u.rect, u.class)).collect()
}

fn back_key(h: &mut Harness) {
    h.event(UiEvent::Key(ink_device::KeyEvent { key: Key::Back, pressed: true, time: ink_device::Timestamp::default() }));
}

/// Asserts that `ups` are partial updates (GL16 or DU) inside `inside`.
fn partial_inside(what: &str, ups: &[(Rect, RefreshClass)], inside: Rect) {
    assert!(!ups.is_empty(), "{what}: nothing shown");
    for (r, c) in ups {
        assert!(*c == RefreshClass::Gl16 || *c == RefreshClass::Du, "{what}: {c:?} in {ups:?}");
        assert!(r.intersect(&inside) == *r, "{what}: {r:?} outside {inside:?} ({ups:?})");
    }
}

/// Popups over the library (the app bar's 3-dots menu, a book's menu, the delete
/// dialog) open and close without a flash: the screen repaints its whole canvas and
/// reports it as GL16, which `coalesce` alone promotes to a full-screen GC16; the app
/// loop cuts it to the pixels that changed (`crate::shown`), so each open and each
/// close is a GL16 inside the popup's rectangle.
#[test]
fn popups_open_and_close_with_a_partial_gl16_of_their_rectangle() {
    let (dir, paths) = books("popup-refresh", &["juliet.epub", "synthetic.epub"]);
    let mut h = Harness::new("popup-refresh", &dir, paths);
    let j = dir.join("juliet.epub");
    for tab in [0, 1] {
        h.tap(|x| *x == Hit::Tab(tab));
        // The 3-dots menu: Back closes it.
        let n = h.probe.updates().len();
        h.tap(|x| *x == Hit::More);
        let Some(Overlay::Menu { rect, kind: MenuKind::More, .. }) = &h.screen.overlay else { panic!("overflow menu") };
        let rect = *rect;
        let ups = since(&h, n);
        partial_inside("3-dots open", &ups, rect);
        assert_eq!(ups, vec![(rect, RefreshClass::Gl16)], "one GL16 of the menu");
        let n = h.probe.updates().len();
        back_key(&mut h);
        assert!(h.screen.overlay.is_none());
        let ups = since(&h, n);
        partial_inside("3-dots close", &ups, rect);
        assert_eq!(ups.len(), 1, "one update: {ups:?}");
        // A tap outside closes it the same way.
        h.tap(|x| *x == Hit::More);
        let n = h.probe.updates().len();
        h.tap_at(Point::new(rect.x + 20, rect.bottom() + 60));
        assert!(h.screen.overlay.is_none());
        partial_inside("3-dots tap outside", &since(&h, n), rect);
        // A book's menu.
        let pt = h.target(|x| *x == Hit::Book(j.clone()));
        let n = h.probe.updates().len();
        h.hold_at(pt);
        let Some(Overlay::Menu { rect, .. }) = &h.screen.overlay else { panic!("book menu") };
        let rect = *rect;
        partial_inside("book menu open", &since(&h, n), rect);
        let n = h.probe.updates().len();
        back_key(&mut h);
        partial_inside("book menu close", &since(&h, n), rect);
    }
    // The delete dialog, from the book menu.
    let pt = h.target(|x| *x == Hit::Book(j.clone()));
    h.hold_at(pt);
    let Some(Overlay::Menu { rect: menu, .. }) = &h.screen.overlay else { panic!("book menu") };
    let menu = *menu;
    let n = h.probe.updates().len();
    h.pick("delete");
    assert!(matches!(h.screen.overlay, Some(Overlay::Confirm { .. })));
    let dialog = ink_ui::Dialog::frame(h.dev.screen.size());
    partial_inside("dialog open (the menu closes under it)", &since(&h, n), menu.union(&dialog));
    let n = h.probe.updates().len();
    back_key(&mut h);
    assert!(h.screen.overlay.is_none() && j.exists());
    partial_inside("dialog close", &since(&h, n), dialog);
    // Another tab with two books changes a part of the screen only: no flash either.
    let n = h.probe.updates().len();
    h.tap(|x| *x == Hit::Tab(0));
    assert!(since(&h, n).iter().all(|(_, c)| *c != RefreshClass::Gc16), "tab switch: {:?}", since(&h, n));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

/// A swipe up or down, the page keys and the scroll bar move the list view a screen at
/// a time; each step refreshes only the rows and the bar.
#[test]
fn library_list_scrolls_a_screen_at_a_time_with_a_scroll_bar() {
    let files = ["juliet.epub", "synthetic.epub", "formats/sample.fb2", "formats/alice.txt", "formats/epub3-nav.epub", "formats/alice-calibre.fb2", "formats/sample.html", "formats/epub2-ncx.epub", "formats/sample.fb3"];
    let (dir, paths) = books("scroll", &files);
    let mut h = Harness::new("scroll", &dir, paths);
    h.tap(|x| *x == Hit::Tab(1));
    assert_eq!((h.screen.view.clone(), h.screen.mode), (View::Books, ViewMode::List));
    let per = h.screen.per_page(1272, 1696);
    assert!(per < 9, "the test needs two screens of rows ({per} per screen)");
    let g = h.screen.bar.expect("scroll bar with two screens");
    assert_eq!((g.bar.total, g.bar.first, g.bar.shown), (9, 0, per));
    // No pager footer any more: no "Previous" / "Next" buttons, the rows reach the
    // navigation bar.
    assert!(!h.screen.hits.iter().any(|(r, x)| matches!(x, Hit::Back) || r.y > g.region.bottom() && !matches!(x, Hit::Tab(_))));
    h.shot("01-first-screen");
    // The track is drawn in its column right of the rows; rows end before it.
    let track = ScrollBar::track(g.region);
    let panel = h.probe.panel();
    let px = |x: i32, y: i32| panel.pixels[y as usize * panel.width + x as usize];
    let cx_ = track.x + track.w as i32 / 2;
    assert_eq!(px(cx_, track.y + 20), 0, "thumb at the top");
    assert_eq!(px(cx_, track.bottom() - 20), h.theme.track(), "track below the thumb");
    assert!(h.screen.hits.iter().filter(|(_, x)| matches!(x, Hit::Book(_))).all(|(r, _)| r.right() < track.x));

    // Swipe up: the next screen, one GL16 of the region.
    let n = h.probe.updates().len();
    h.swipe(true);
    h.assert_scrolled(n, 1);
    assert_eq!(h.screen.page_books(1272, 1696).0.len(), 9 - per);
    h.shot("02-second-screen");
    // At the end a swipe up does nothing (no refresh).
    let n = h.probe.updates().len();
    h.swipe(true);
    assert_eq!((h.screen.page, h.updates_since(n).len()), (1, 0));
    // Swipe down and the page keys.
    h.swipe(false);
    h.assert_scrolled(n, 0);
    let n = h.probe.updates().len();
    h.key(Key::PageNext);
    h.assert_scrolled(n, 1);
    let n = h.probe.updates().len();
    h.key(Key::PagePrev);
    h.assert_scrolled(n, 0);

    // A tap on the lower end of the bar jumps there; the row under the bar's touch zone
    // is not opened.
    let g = h.screen.bar.unwrap();
    let track = ScrollBar::track(g.region);
    let n = h.probe.updates().len();
    h.tap_at(Point::new(g.region.right() - 30, track.bottom() - 10));
    h.assert_scrolled(n, 1);
    assert!(h.pushed().is_empty(), "the tap on the bar opened nothing: {:?}", h.pushed());
    // A drag from the bottom to the top: the bar alone follows the finger (DU of the
    // track), the lift shows the first screen (one GL16).
    let n = h.probe.updates().len();
    let x = g.region.right() - 24;
    h.frame(400, x, track.bottom() - 10, TouchPhase::Down);
    for y in [1200, 900, 600, track.y + 10] {
        h.frame(30, x, y, TouchPhase::Move);
    }
    assert_eq!(h.screen.page, 1, "the rows wait for the lift");
    let ups = h.updates_since(n);
    assert!(!ups.is_empty() && ups.iter().all(|u| u.rect == track && u.class == RefreshClass::Du), "{ups:?}");
    let n = h.probe.updates().len();
    h.frame(30, x, track.y + 10, TouchPhase::Up);
    h.assert_scrolled(n, 0);
    assert!(h.pushed().is_empty() && h.screen.overlay.is_none());
    // A tap on a row still opens its book.
    let first = h.screen.page_books(1272, 1696).0[0].path.clone();
    h.tap(|x| *x == Hit::Book(first.clone()));
    assert_eq!(h.pushed().last(), Some(&Route::Reader(first)));
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

/// Collections and the file browser scroll the same way.
#[test]
fn collections_and_files_scroll_with_the_bar() {
    let (dir, paths) = books("scroll-more", &["juliet.epub"]);
    for i in 0..14 {
        std::fs::create_dir_all(dir.join(format!("folder {i:02}"))).unwrap();
    }
    let mut h = Harness::new("scroll-more", &dir, paths);
    {
        let lib = h.screen.lib.as_mut().unwrap();
        for i in 0..9 {
            lib.create_collection(&format!("Shelf {i}")).unwrap();
        }
    }
    let coll = h.screen.nav.iter().position(|n| n == "collections");
    if let Some(i) = coll {
        h.tap(|x| *x == Hit::Tab(i));
    } else {
        h.screen.show(View::Collections);
        h.paint();
    }
    assert_eq!(h.screen.view, View::Collections);
    let per = LibraryScreen::coll_per_page(1696);
    let g = h.screen.bar.expect("collections scroll bar");
    assert_eq!((g.bar.total, g.per), (9, per));
    h.shot("01-collections");
    let n = h.probe.updates().len();
    h.swipe(true);
    h.assert_scrolled(n, 1);
    h.shot("02-collections-2");

    h.screen.show(View::Import);
    h.paint();
    h.tap(|x| *x == Hit::Browse);
    assert_eq!(h.screen.view, View::Files);
    if h.screen.files_dir != dir {
        // The browser starts above the library folder.
        h.tap(|x| *x == Hit::File(dir.clone()));
    }
    assert_eq!(h.screen.files_dir, dir);
    let g = h.screen.bar.expect("file browser scroll bar");
    assert_eq!(g.bar.total, h.screen.files_rows());
    h.shot("03-files");
    let n = h.probe.updates().len();
    h.key(Key::PageNext);
    h.assert_scrolled(n, 1);
    h.shot("04-files-2");
    // Opening a folder starts at its top.
    let sub = dir.join("folder 13");
    h.tap(|x| *x == Hit::File(sub.clone()));
    assert_eq!((h.screen.files_dir.clone(), h.screen.files_page), (sub, 0));
    assert!(h.screen.bar.is_none(), "an empty folder fits");
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

/// The longest horizontal run of the progress track colour in `r` of the panel: a
/// bar's track is hundreds of pixels long, text edges and badges a few.
fn track_run(h: &Harness, r: Rect) -> usize {
    let panel = h.probe.panel();
    let c = h.theme.track();
    let mut best = 0;
    for y in r.y.max(0)..r.bottom().min(panel.height as i32) {
        let mut run = 0;
        for x in r.x.max(0)..r.right().min(panel.width as i32) {
            run = if panel.pixels[y as usize * panel.width + x as usize] == c { run + 1 } else { 0 };
            best = best.max(run);
        }
    }
    best
}

/// A book at 0 % or marked unread shows no progress bar anywhere in the library (list,
/// grid, Home, details); one with reading progress does.
#[test]
fn no_progress_bar_for_unread_or_zero_percent_books() {
    use super::browse::{book_state, shows_progress, Badge};
    let (dir, paths) = books("no-bar", &["juliet.epub", "synthetic.epub", "formats/sample.fb2", "formats/alice.txt"]);
    let mut h = Harness::new("no-bar", &dir, paths);
    let (zero, unread, tiny, read) = (dir.join("juliet.epub"), dir.join("synthetic.epub"), dir.join("alice.txt"), dir.join("sample.fb2"));
    {
        let lib = h.screen.lib.as_mut().unwrap();
        let mut set = |p: &Path, status: ReadingStatus, percent: Option<f32>| {
            let b = lib.index.get_mut(p).unwrap();
            (b.status, b.percent, b.last_opened) = (status, percent, Some(1_000));
        };
        // Opened, still at 0 %.
        set(&zero, ReadingStatus::Reading, Some(0.0));
        // Read to 40 %, then marked unread: the position stays, the bar goes.
        set(&unread, ReadingStatus::New, Some(0.4));
        // Under half a percent: the label would say "0%".
        set(&tiny, ReadingStatus::Reading, Some(0.004));
        set(&read, ReadingStatus::Reading, Some(0.4));
    }
    let entry = |h: &Harness, p: &Path| h.screen.library().unwrap().index.get(p).unwrap().clone();
    for p in [&zero, &unread, &tiny] {
        let b = entry(&h, p);
        assert!(!shows_progress(&b), "{}", p.display());
        assert_eq!(book_state(&b), Err(Badge::New), "{}", p.display());
    }
    let b = entry(&h, &read);
    assert!(shows_progress(&b));
    assert_eq!(book_state(&b), Ok(0.4));
    let mut done = b.clone();
    done.status = ReadingStatus::Complete;
    assert!(shows_progress(&done), "finished books keep their full bar on Home");
    // Opened but no position yet: no bar.
    let mut fresh = b.clone();
    (fresh.status, fresh.percent) = (ReadingStatus::Reading, None);
    assert!(!shows_progress(&fresh));

    // List: the bar line of each row (see `draw_book_row`).
    h.tap(|x| *x == Hit::Tab(1));
    h.shot("01-list");
    let row = |h: &Harness, p: &Path| h.screen.hits.iter().find(|(_, x)| *x == Hit::Book(p.to_path_buf())).map(|(r, _)| *r).unwrap();
    let line = |r: Rect| Rect::new(r.x + 96 + 24, r.y + 16 + 100, 560, 40);
    assert!(track_run(&h, line(row(&h, &read))) > 150, "the read book has its bar");
    for p in [&zero, &unread, &tiny] {
        let n = track_run(&h, line(row(&h, p)));
        assert!(n < 40, "no bar in the list for {} ({n} track pixels)", p.display());
    }
    // Grid: the strip between the cover and the title (see `draw_book_cell`).
    h.tap(|x| *x == Hit::ToggleMode);
    h.shot("02-grid");
    let strip = |r: Rect| Rect::new(r.x + 16, r.bottom() - 84, r.w - 32, 40);
    assert!(track_run(&h, strip(row(&h, &read))) > 150);
    for p in [&zero, &unread, &tiny] {
        assert!(track_run(&h, strip(row(&h, p))) < 40, "no bar in the grid for {}", p.display());
    }
    // Details: the thick bar right of the cover.
    let right_of_cover = Rect::new(440, 300, 790, 140);
    h.screen.show(View::Details(read.clone()));
    h.paint();
    assert!(track_run(&h, right_of_cover) > 150, "details of the read book show its bar");
    for p in [&zero, &unread, &tiny] {
        h.screen.show(View::Details(p.clone()));
        h.paint();
        h.shot(&format!("03-details-{}", p.file_stem().unwrap().to_string_lossy()));
        assert!(track_run(&h, right_of_cover) < 40, "no bar on the details of {}", p.display());
    }
    // Home: the hero card of the current book (the bottom block holds the bar).
    for (i, p) in [&read, &zero, &unread, &tiny].into_iter().enumerate() {
        h.screen.lib.as_mut().unwrap().index.get_mut(p).unwrap().last_opened = Some(10_000 + i as u64);
        h.screen.show(View::Home);
        h.paint();
        h.shot(&format!("04-home-{}", p.file_stem().unwrap().to_string_lossy()));
        let card = h.screen.hits.iter().find(|(_, x)| *x == Hit::Book(p.clone())).map(|(r, _)| *r).unwrap();
        let bottom = Rect::new(card.x + card.w as i32 / 3, card.bottom() - 150, card.w * 2 / 3 - 16, 110);
        let n = track_run(&h, bottom);
        if p == &read {
            assert!(card.h > 300 && n > 150, "the hero card of the read book shows its bar ({card:?}, {n})");
        } else {
            assert!(n < 40, "no bar on the hero card of {} ({n})", p.display());
        }
    }
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}

/// Asks for a library rescan on any action (as calibre does after a received book).
struct Rescanner(ink_plugin::PluginManifest);

impl ink_plugin::Plugin for Rescanner {
    fn manifest(&self) -> &ink_plugin::PluginManifest {
        &self.0
    }
    fn on_action(&mut self, cx: &mut ink_plugin::HostCtx, _id: &str) -> ink_plugin::Result<()> {
        cx.ui()?.rescan_library();
        Ok(())
    }
}

/// A plugin that adds a book while the library is shown gets it listed at the next
/// event, without leaving the library.
#[test]
fn plugin_rescan_while_the_library_is_shown() {
    let (dir, paths) = books("plugin-rescan", &["juliet.epub"]);
    let mut h = Harness::new("plugin-rescan", &dir, paths);
    h.plugins = PluginHost::new(vec![Box::new(Rescanner(ink_plugin::PluginManifest::new("rescanner", "1", &[ink_plugin::Permission::Ui])))]);
    let p = providers();
    h.screen.on_enter(&mut cx!(h, p)).unwrap();
    h.paint();
    assert_eq!(h.screen.library().unwrap().index.books.len(), 1);
    std::fs::copy(testdata("synthetic.epub"), dir.join("synthetic.epub")).unwrap();
    h.plugins.trigger("rescanner", "go", None);
    let start = std::time::Instant::now();
    h.event(UiEvent::Timer(0));
    while h.screen.job.is_some() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
        h.event(UiEvent::Timer(0));
    }
    assert_eq!(h.screen.library().unwrap().index.books.len(), 2);
    std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
}
