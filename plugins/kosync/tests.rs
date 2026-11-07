//! KOSync plugin against ink-net's mock server (fake credentials, no real network).

use super::*;
use ink_net::kosync::userkey;
use ink_net::testserver::TestServer;
use ink_net::UreqClient;
use ink_plugin::testing::{FakeBook, TempDir};
use ink_plugin::{DocumentAccess, MemoryStorage, NetHost, NetWorker, UiQueue, UiRequest};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

const DOC: &str = "cf7ce9b8d4e135c8ac340f18d1e58c85";

struct Env {
    plugin: KoSyncPlugin,
    ui: UiQueue,
    store: MemoryStorage,
    worker: NetWorker,
    doc: FakeBook,
    perms: Vec<Permission>,
    clock: Rc<Cell<u64>>,
    _dir: TempDir,
}

fn xp(n: u32) -> String {
    format!("/body/DocFragment[{n}]/body/p[2]/text().0")
}

impl Env {
    fn new(server: &TestServer, tag: &str) -> Env {
        let dir = TempDir::new(&format!("kosync-{tag}"));
        let clock = Rc::new(Cell::new(1_000_000u64));
        let c = clock.clone();
        let mut store = MemoryStorage { dir: dir.join("data"), ..Default::default() };
        store.settings.insert("server".into(), json!(server.url("/kosync")));
        Env {
            plugin: KoSyncPlugin::new().with_clock(move || c.get()),
            ui: UiQueue::default(),
            store,
            worker: NetWorker::new(Arc::new(UreqClient::new())),
            doc: FakeBook { id: Some(DOC.into()), position: xp(1).parse().ok(), page: Some(0), pages: Some(100), ..FakeBook::titled("Juliet") },
            perms: vec![Permission::Document, Permission::Ui, Permission::Storage, Permission::Network],
            clock,
            _dir: dir,
        }
    }

    fn call(&mut self, f: impl FnOnce(&mut KoSyncPlugin, &mut HostCtx) -> Result<()>) {
        let mut jobs = self.worker.for_plugin(NAME);
        let mut cx = HostCtx::new(&mut self.ui, &mut self.store, &self.perms)
            .with_doc(Some(&mut self.doc as &mut dyn DocumentAccess))
            .with_jobs(Some(&mut jobs as &mut dyn NetHost));
        f(&mut self.plugin, &mut cx).unwrap();
    }

    /// Delivers network events until no request runs.
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

    fn login(&mut self, user: &str, pass: &str) {
        self.store.settings.insert("user".into(), json!(user));
        self.store.settings.insert("userkey".into(), json!(userkey(pass)));
    }

    fn take_ui(&mut self) -> Vec<UiRequest> {
        std::mem::take(&mut self.ui.0)
    }

    /// Puts the reader on `page` at position `pos`.
    fn at(&mut self, page: usize, pos: &str) {
        self.doc.page = Some(page);
        self.doc.position = Some(pos.parse().unwrap());
    }

    fn turn(&mut self, page: usize, pos: String, secs: u64) {
        self.clock.set(self.clock.get() + secs);
        self.at(page, &pos);
        self.call(|p, cx| p.on_page_change(cx, page));
    }
}

fn toasts(ui: &[UiRequest]) -> Vec<String> {
    ui.iter().filter_map(|r| if let UiRequest::Toast { text } = r { Some(text.clone()) } else { None }).collect()
}

fn pushes(s: &TestServer) -> Vec<Value> {
    s.state.requests().iter().filter(|r| r.method == "PUT").map(|r| serde_json::from_slice(&r.body).unwrap()).collect()
}

#[test]
fn progress_strings_follow_koreader() {
    assert_eq!(to_koreader("/body/DocFragment[3]/body/p[2]/text().4").as_deref(), Some("/body/DocFragment[3]/body/p[2]/text().4"));
    assert_eq!(to_koreader("page:0").as_deref(), Some("1"));
    assert_eq!(to_koreader("page:11;1,2,3,4").as_deref(), Some("12"));
    assert_eq!(to_koreader("flow:1.2.3"), None);
    assert_eq!(from_koreader("12").as_deref(), Some("page:11"));
    assert_eq!(from_koreader("/body/p[1]").as_deref(), Some("/body/p[1]"));
    assert_eq!(from_koreader("garbage"), None);
    assert!(matches!(check_answer(200, "text/html", b"<html>", &[200]), Err(NetError::Format(_))));
    assert!(matches!(check_answer(401, "application/json", br#"{"message":"Unauthorized"}"#, &[200]), Err(NetError::Unauthorized(m)) if m == "Unauthorized"));
}

#[test]
fn control_tile_is_registered_with_the_old_sync_tile_look() {
    let mut reg = ink_plugin::Registry::default();
    KoSyncPlugin::new().on_load(&mut ink_plugin::Registrar { plugin: NAME, registry: &mut reg }).unwrap();
    let t = reg.control_tiles.iter().find(|t| t.id == "sync_now").expect("tile");
    assert_eq!((t.label.as_str(), t.icon.as_str(), t.state.as_str()), ("Sync progress", "sync", "Reading progress"));
    assert_eq!(t.needs_wifi.as_deref(), Some("Turn on Wi-Fi to sync"));
    assert_eq!(t.working, "Syncing reading progress…");
}

#[test]
fn settings_page_registers_logs_in_and_stores_only_the_md5() {
    let s = TestServer::start().unwrap();
    let mut e = Env::new(&s, "login");
    e.call(|p, cx| p.on_action(cx, "settings:kosync"));
    let ui = e.take_ui();
    assert!(matches!(&ui[0], UiRequest::Menu { id: MENU, items, .. } if items.len() == 7), "{ui:?}");
    e.call(|p, cx| p.on_ui_result(cx, MENU, "user"));
    assert!(matches!(e.take_ui()[0], UiRequest::TextInput { id: IN_USER, .. }));
    e.call(|p, cx| p.on_ui_result(cx, IN_USER, " reader "));
    e.call(|p, cx| p.on_ui_result(cx, MENU, "password"));
    e.call(|p, cx| p.on_ui_result(cx, IN_PASS, "fake-pass-1"));
    assert_eq!(e.store.settings["user"], json!("reader"));
    assert_eq!(e.store.settings["userkey"], json!(userkey("fake-pass-1")));
    assert!(!serde_json::to_string(&e.store.settings).unwrap().contains("fake-pass-1"), "clear password never stored");
    e.take_ui();

    // Login before the account exists fails; register; login works; register again is taken.
    e.call(|p, cx| p.on_ui_result(cx, MENU, "login"));
    e.settle();
    assert!(toasts(&e.take_ui()).iter().any(|t| t.contains("wrong user name or password")));
    e.call(|p, cx| p.on_ui_result(cx, MENU, "register"));
    e.settle();
    assert!(toasts(&e.take_ui()).iter().any(|t| t.contains("registered")));
    e.call(|p, cx| p.on_ui_result(cx, MENU, "login"));
    e.settle();
    assert!(toasts(&e.take_ui()).iter().any(|t| t == "Logged in as reader"));
    e.call(|p, cx| p.on_ui_result(cx, MENU, "register"));
    e.settle();
    assert!(toasts(&e.take_ui()).iter().any(|t| t.contains("taken")));

    // Auto sync toggles; logout drops the key.
    e.call(|p, cx| p.on_ui_result(cx, MENU, "auto"));
    assert_eq!(e.store.settings["auto_sync"], json!(false));
    e.call(|p, cx| p.on_ui_result(cx, MENU, "logout"));
    assert!(!e.store.settings.contains_key("userkey"));
}

#[test]
fn open_pushes_when_the_server_has_nothing_then_debounces_page_turns_and_pushes_on_close() {
    let s = TestServer::start().unwrap();
    s.state.add_user("reader", &userkey("pw"));
    let mut e = Env::new(&s, "push");
    e.login("reader", "pw");
    e.call(|p, cx| p.on_book_open(cx));
    e.settle();
    let p = pushes(&s);
    assert_eq!(p.len(), 1, "no remote: local pushed after the pull");
    assert_eq!(p[0]["document"], DOC);
    assert_eq!(p[0]["progress"], xp(1));
    let device_id = e.store.settings["device_id"].as_str().unwrap().to_string();
    assert_eq!(device_id.len(), 32);
    assert_eq!(p[0]["device_id"], device_id.as_str());
    assert_eq!(p[0]["device"], DEFAULT_DEVICE);

    // Turns within PUSH_EVERY only record; the next turn after it pushes the latest.
    e.turn(1, xp(2), 5);
    e.turn(2, xp(3), 5);
    e.settle();
    assert_eq!(pushes(&s).len(), 1);
    e.turn(3, xp(4), PUSH_EVERY);
    e.settle();
    let p = pushes(&s);
    assert_eq!(p.len(), 2);
    assert_eq!(p[1]["progress"], xp(4));
    assert_eq!(p[1]["percentage"], 0.03);
    // Same position again: nothing to push.
    e.turn(3, xp(4), 1);
    e.settle();
    assert_eq!(pushes(&s).len(), 2);

    // Close pushes the trailing position at once, and it reaches the server.
    e.turn(4, xp(5), 1);
    e.call(|p, cx| p.on_close(cx));
    e.settle();
    let p = pushes(&s);
    assert_eq!(p.len(), 3, "{:?}", s.state.requests().iter().map(|r| (r.method.clone(), r.path.clone())).collect::<Vec<_>>());
    assert_eq!(p[2]["progress"], xp(5));
    let q = SyncQueue::load(&e.store.dir.join("queue.json"));
    assert!(q.items.is_empty(), "the close push left the queue after success: {q:?}");
    assert!(e.store.book.contains_key("changed_at"));
}

#[test]
fn a_newer_remote_position_asks_and_jumps() {
    let s = TestServer::start().unwrap();
    s.state.add_user("reader", &userkey("pw"));
    // Another device pushed 50% (server timestamp = now, newer than our changed_at 0).
    let http = UreqClient::new();
    let other = KoSyncClient::new(&http, KoSyncAccount::new(&s.url("/kosync"), "reader", "pw"));
    other
        .push(&Progress { document: DOC.into(), progress: xp(9), percentage: 0.5, device: "kpw".into(), device_id: "OTHERDEVICE".into(), timestamp: None, metadata: None })
        .unwrap();
    let mut e = Env::new(&s, "jump");
    e.login("reader", "pw");
    e.call(|p, cx| p.on_book_open(cx));
    e.settle();
    let ui = e.take_ui();
    let dialog = ui.iter().find_map(|r| if let UiRequest::Dialog { id: JUMP, body, buttons, .. } = r { Some((body.clone(), buttons.clone())) } else { None });
    let (body, buttons) = dialog.unwrap_or_else(|| panic!("no jump dialog: {ui:?}"));
    assert!(body.contains("kpw is at 50%") && body.contains("Jump to the synced position?"), "{body}");
    assert_eq!(buttons[1].1, "jump");
    assert_eq!(pushes(&s).len(), 1, "no push while the user decides");
    // Page turns before the answer do not push either.
    e.turn(1, xp(2), PUSH_EVERY + 1);
    e.settle();
    assert_eq!(pushes(&s).len(), 1);
    // Sleep while the question is open: nothing pushed.
    e.call(|p, cx| p.on_suspend(cx));
    e.settle();
    assert_eq!(pushes(&s).len(), 1);
    e.call(|p, cx| p.on_ui_result(cx, JUMP, "jump"));
    assert_eq!(e.take_ui(), vec![UiRequest::GoTo { position: xp(9) }]);
    // The reader lands there: no push needed for that position.
    e.turn(50, xp(9), PUSH_EVERY + 1);
    e.settle();
    assert_eq!(pushes(&s).len(), 1);
}

#[test]
fn declining_the_jump_pushes_this_device_and_pdf_pages_are_one_based() {
    let s = TestServer::start().unwrap();
    s.state.add_user("reader", &userkey("pw"));
    let http = UreqClient::new();
    let other = KoSyncClient::new(&http, KoSyncAccount::new(&s.base(), "reader", "pw"));
    other
        .push(&Progress { document: DOC.into(), progress: "40".into(), percentage: 0.4, device: "kpw".into(), device_id: "OTHERDEVICE".into(), timestamp: None, metadata: None })
        .unwrap();
    let mut e = Env::new(&s, "pdf");
    e.store.settings.insert("server".into(), json!(s.base()));
    e.login("reader", "pw");
    e.at(4, "page:4");
    e.call(|p, cx| p.on_book_open(cx));
    e.settle();
    assert!(e.ui.0.iter().any(|r| matches!(r, UiRequest::Dialog { id: JUMP, .. })));
    e.take_ui();
    e.call(|p, cx| p.on_ui_result(cx, JUMP, "no"));
    e.settle();
    let p = pushes(&s);
    assert_eq!(p.last().unwrap()["progress"], "5", "page:4 is KOReader page 5");
    assert_eq!(p.last().unwrap()["percentage"], 0.05);
    // A manual sync now finds it in sync.
    e.call(|p, cx| p.on_action(cx, "sync_now"));
    e.settle();
    assert!(toasts(&e.take_ui()).iter().any(|t| t.contains("in sync") || t.contains("sent")), "manual sync answered");
}

#[test]
fn failed_pushes_are_queued_and_sent_later() {
    let s = TestServer::start().unwrap();
    s.state.add_user("reader", &userkey("pw"));
    let mut e = Env::new(&s, "queue");
    e.login("reader", "pw");
    // Server down: the pull and the close push fail; the close push stays queued.
    e.store.settings.insert("server".into(), json!("http://127.0.0.1:1"));
    e.call(|p, cx| p.on_book_open(cx));
    e.settle();
    e.turn(7, xp(8), 1);
    e.call(|p, cx| p.on_close(cx));
    e.settle();
    let q = SyncQueue::load(&e.store.dir.join("queue.json"));
    assert_eq!(q.items.len(), 1, "{q:?}");
    assert_eq!(q.items[0].progress.progress, xp(8));
    // Server back: the next wake sends it.
    e.store.settings.insert("server".into(), json!(s.url("/kosync")));
    e.call(|p, cx| p.on_resume(cx));
    e.settle();
    assert_eq!(pushes(&s).len(), 1);
    assert_eq!(pushes(&s)[0]["progress"], xp(8));
    assert!(SyncQueue::load(&e.store.dir.join("queue.json")).items.is_empty());
}

#[test]
fn no_account_means_no_requests() {
    let s = TestServer::start().unwrap();
    let mut e = Env::new(&s, "noacct");
    e.call(|p, cx| p.on_book_open(cx));
    e.turn(3, xp(3), 100);
    e.call(|p, cx| p.on_close(cx));
    assert_eq!(e.worker.in_flight(), 0);
    assert!(s.state.requests().is_empty());
    e.call(|p, cx| p.on_book_open(cx));
    e.call(|p, cx| p.on_action(cx, "sync_now"));
    assert!(toasts(&e.take_ui()).iter().any(|t| t.contains("log in first")));
}

#[test]
fn closing_with_the_jump_question_open_keeps_the_remote_position() {
    let s = TestServer::start().unwrap();
    s.state.add_user("reader", &userkey("pw"));
    let http = UreqClient::new();
    let other = KoSyncClient::new(&http, KoSyncAccount::new(&s.url("/kosync"), "reader", "pw"));
    other
        .push(&Progress { document: DOC.into(), progress: xp(9), percentage: 0.5, device: "kpw".into(), device_id: "OTHERDEVICE".into(), timestamp: None, metadata: None })
        .unwrap();
    let mut e = Env::new(&s, "closeopen");
    e.login("reader", "pw");
    e.call(|p, cx| p.on_book_open(cx));
    e.settle();
    e.call(|p, cx| p.on_close(cx));
    e.settle();
    assert_eq!(pushes(&s).len(), 1, "only the other device's push");
    assert!(SyncQueue::load(&e.store.dir.join("queue.json")).items.is_empty());
}

#[test]
fn an_old_queued_push_never_overwrites_a_newer_sync() {
    let s = TestServer::start().unwrap();
    s.state.add_user("reader", &userkey("pw"));
    let mut e = Env::new(&s, "stale");
    e.login("reader", "pw");
    // A push of this book from two days ago is still queued.
    let old = Progress { document: DOC.into(), progress: xp(2), percentage: 0.02, device: DEFAULT_DEVICE.into(), device_id: "X".repeat(32), timestamp: None, metadata: None };
    let qpath = e.store.dir.join("queue.json");
    std::fs::create_dir_all(&e.store.dir).unwrap();
    let mut q = SyncQueue::default();
    q.push(old, e.clock.get() - 2 * 86400);
    q.save(&qpath).unwrap();
    e.at(5, &xp(5));
    e.call(|p, cx| p.on_book_open(cx));
    e.settle();
    e.turn(6, xp(6), 1);
    e.call(|p, cx| p.on_close(cx));
    e.settle();
    let p = pushes(&s);
    assert!(p.iter().all(|b| b["progress"] != xp(2)), "the stale entry was sent: {p:?}");
    assert_eq!(p.last().unwrap()["progress"], xp(6));
    assert!(SyncQueue::load(&qpath).items.is_empty());
    // Waking up later sends nothing more.
    e.call(|p, cx| p.on_resume(cx));
    e.settle();
    assert_eq!(pushes(&s).len(), p.len());
}
