//! OPDS plugin against ink-net's mock catalog (no real network).

use super::*;
use ink_net::testserver::TestServer;
use ink_net::UreqClient;
use ink_plugin::testing::TempDir;
use ink_plugin::{MemoryStorage, NetHost, NetWorker, UiQueue, UiRequest};
use std::sync::Arc;
use std::time::Duration;

struct Env {
    plugin: OpdsPlugin,
    ui: UiQueue,
    store: MemoryStorage,
    worker: NetWorker,
    perms: Vec<Permission>,
    books: PathBuf,
    _root: TempDir,
}

impl Env {
    fn new(tag: &str) -> Env {
        let root = TempDir::new(&format!("opds-{tag}"));
        let books = root.join("Books");
        let mut store = MemoryStorage { dir: root.join("data"), ..Default::default() };
        store.settings.insert("download_dir".into(), json!(books.to_str().unwrap()));
        Env {
            plugin: OpdsPlugin::new(),
            ui: UiQueue::default(),
            store,
            worker: NetWorker::new(Arc::new(UreqClient::new())),
            perms: vec![Permission::Ui, Permission::Storage, Permission::Network],
            books,
            _root: root,
        }
    }

    fn call(&mut self, f: impl FnOnce(&mut OpdsPlugin, &mut HostCtx) -> Result<()>) {
        let mut jobs = self.worker.for_plugin(NAME);
        let mut cx = HostCtx::new(&mut self.ui, &mut self.store, &self.perms).with_jobs(Some(&mut jobs as &mut dyn NetHost));
        f(&mut self.plugin, &mut cx).unwrap();
    }

    fn settle(&mut self) {
        for _ in 0..500 {
            if self.worker.in_flight() == 0 {
                return;
            }
            for (_, id, ev) in self.worker.wait(Duration::from_millis(200)) {
                self.call(|p, cx| p.on_http(cx, id, &ev));
            }
        }
        panic!("requests did not finish");
    }

    fn answer(&mut self, id: u32, result: &str) {
        self.call(|p, cx| p.on_ui_result(cx, id, result));
        self.settle();
    }

    /// The last menu shown: (id, title, items).
    fn last_menu(&self) -> (u32, String, Vec<(String, String)>) {
        self.ui.0.iter().rev().find_map(|r| if let UiRequest::Menu { id, title, items } = r { Some((*id, title.clone(), items.clone())) } else { None }).expect("a menu")
    }

    fn action(&self, label_part: &str) -> String {
        let (_, _, items) = self.last_menu();
        items.iter().find(|(l, _)| l.contains(label_part)).map(|(_, a)| a.clone()).unwrap_or_else(|| panic!("no {label_part:?} in {items:?}"))
    }
}

#[test]
fn names_and_extensions() {
    assert_eq!(extension("application/epub+zip", "/x"), "epub");
    assert_eq!(extension("application/pdf; charset=binary", "/x"), "pdf");
    assert_eq!(extension("application/octet-stream", "/get/book.FB2?x=1"), "fb2");
    assert_eq!(extension("", "/get/12"), "bin");
    assert_eq!(safe_name("Alpha & Omega: The/End?"), "Alpha & Omega_ The_End_");
    assert_eq!(safe_name("  ..  "), "book");
    assert_eq!(safe_name(&"x".repeat(200)).len(), 80);
    let d = TempDir::new("opds-unique");
    std::fs::write(d.join("a.epub"), b"x").unwrap();
    assert_eq!(unique_path(&d, "a", "epub"), d.join("a (2).epub"));
    assert_eq!(unique_path(&d, "b", "epub"), d.join("b.epub"));
}

#[test]
fn add_browse_page_download_and_rescan() {
    let s = TestServer::start().unwrap();
    let mut e = Env::new("browse");
    // Add a catalog from the settings page.
    e.call(|p, cx| p.on_action(cx, "settings:catalogs"));
    assert_eq!(e.action("Add"), "add");
    e.answer(CATALOGS, "add");
    assert!(matches!(e.ui.0.last(), Some(UiRequest::TextInput { id: IN_URL, .. })));
    e.answer(IN_URL, &s.url("/opds/root.xml"));
    e.answer(IN_TITLE, "Test catalog");
    assert_eq!(e.store.settings["catalogs"], json!([{"title": "Test catalog", "url": s.url("/opds/root.xml")}]));
    let (id, _, items) = e.last_menu();
    assert_eq!(id, CATALOGS);
    assert_eq!(items[0], ("Test catalog".to_string(), "c0".to_string()));

    // Open it: navigation feed with search.
    e.answer(CATALOGS, "c0");
    let (id, title, items) = e.last_menu();
    assert_eq!((id, title.as_str()), (FEED, "rinky test catalog"));
    let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
    assert_eq!(labels, vec!["< Back", "Search", "New books", "Popular"]);
    // Sub-feed (relative href), books, next page.
    let a = e.action("New books");
    e.answer(FEED, &a);
    let (_, title, _) = e.last_menu();
    assert_eq!(title, "New books, page 1");
    assert!(e.action("Alpha & Omega\tAnn Author").starts_with('e'));
    let a = e.action("Next page");
    e.answer(FEED, &a);
    assert_eq!(e.last_menu().1, "New books, page 2");
    assert!(e.action("Gamma").starts_with('e'));
    // Back goes to the navigation feed (next page replaced page 1).
    e.answer(FEED, "back");
    assert_eq!(e.last_menu().1, "rinky test catalog");
    let a = e.action("New books");
    e.answer(FEED, &a);

    // A book: its formats; download the EPUB into the download folder.
    let a = e.action("Beta");
    e.answer(FEED, &a);
    let (id, title, items) = e.last_menu();
    assert_eq!((id, title.as_str()), (BOOK, "Beta - Bob Writer"));
    assert_eq!(items[0].0, "Download EPUB (5 KB)");
    e.ui.0.clear();
    e.answer(BOOK, "d0");
    let got = std::fs::read(e.books.join("Beta.epub")).unwrap();
    assert_eq!(got, ink_net::testserver::file_bytes(5000));
    assert!(!e.books.join("Beta.epub.part").exists());
    assert!(e.ui.0.contains(&UiRequest::LibraryRescan), "{:?}", e.ui.0);
    assert!(e.ui.0.iter().any(|r| matches!(r, UiRequest::Toast { text } if text == "Downloaded Beta.epub to the library")));
    // Again: a second file, the first is kept.
    e.answer(BOOK, "d0");
    assert!(e.books.join("Beta (2).epub").exists());
    assert_eq!(std::fs::read(e.books.join("Beta.epub")).unwrap().len(), 5000);
}

#[test]
fn search_through_opensearch_and_errors() {
    let s = TestServer::start().unwrap();
    let mut e = Env::new("search");
    e.store.settings.insert("catalogs".into(), json!([{"title": "T", "url": s.url("/opds/root.xml")}, {"title": "Gone", "url": s.url("/opds/missing.xml")}]));
    e.call(|p, cx| p.on_action(cx, "settings:catalogs"));
    e.answer(CATALOGS, "c0");
    e.answer(FEED, "search");
    assert!(matches!(e.ui.0.last(), Some(UiRequest::TextInput { id: IN_SEARCH, .. })));
    e.answer(IN_SEARCH, "moby dick");
    assert_eq!(e.last_menu().1, "Search: moby dick");
    assert!(e.action("Result for moby").starts_with('e'));
    assert!(e.action("Result for dick").starts_with('e'));
    let reqs = s.state.requests_to("/opds/search");
    assert_eq!(reqs[0].param("q").as_deref(), Some("moby dick"));

    // A missing feed: a toast, and the catalog list again.
    e.answer(FEED, "");
    e.call(|p, cx| p.on_action(cx, "settings:catalogs"));
    e.answer(CATALOGS, "c1");
    assert!(e.ui.0.iter().any(|r| matches!(r, UiRequest::Toast { text } if text.contains("Cannot load the catalog: server answered 404"))), "{:?}", e.ui.0);
    assert_eq!(e.last_menu().0, CATALOGS);

    // Remove the broken catalog.
    e.answer(CATALOGS, "remove");
    e.answer(REMOVE, "r1");
    assert_eq!(OpdsPlugin::catalogs(&mut HostCtx::new(&mut e.ui, &mut e.store, &e.perms)).len(), 1);
}

#[test]
fn opds2_catalogs_browse_too() {
    let s = TestServer::start().unwrap();
    let mut e = Env::new("opds2");
    e.store.settings.insert("catalogs".into(), json!([{"title": "Two", "url": s.url("/opds2/catalog.json")}]));
    e.call(|p, cx| p.on_action(cx, "settings:catalogs"));
    e.answer(CATALOGS, "c0");
    assert_eq!(e.last_menu().1, "rinky OPDS 2 test");
    let a = e.action("Epsilon");
    e.answer(FEED, &a);
    e.answer(BOOK, "d0");
    assert_eq!(std::fs::read(e.books.join("Epsilon.epub")).unwrap().len(), 5000);
}
