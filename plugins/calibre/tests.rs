//! Frame parsing, the book list, and whole sessions against a mock calibre on localhost.

use super::*;
use crate::cache::Cache;
use crate::protocol::{decode, encode, op, Frame, FrameError};
use ink_plugin::serde_json::json;
use ink_plugin::{MemoryStorage, Registrar, TileState, UiQueue, UiRequest};
use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

// -- Frames --------------------------------------------------------------------------------

#[test]
fn frame_encode_decode() {
    let f = encode(op::OK, &json!({"a": 1}));
    assert_eq!(f, b"11[0,{\"a\":1}]");
    assert_eq!(decode(&f), Ok(Some((Frame { op: 0, arg: json!({"a": 1}) }, f.len()))));
    // The length counts UTF-8 bytes, not characters.
    let u = encode(op::NOOP, &json!({"t": "é"}));
    assert!(u.starts_with(b"15["), "{}", String::from_utf8_lossy(&u));
    assert_eq!(decode(&u).unwrap().unwrap().0.arg, json!({"t": "é"}));
}

#[test]
fn frame_partial_and_back_to_back() {
    let mut two = encode(op::GET_INITIALIZATION_INFO, &json!({"x": [1, 2]}));
    let first_len = two.len();
    two.extend(encode(op::NOOP, &json!({})));
    for cut in 0..first_len {
        assert_eq!(decode(&two[..cut]), Ok(None), "cut at {cut}");
    }
    let (a, used) = decode(&two).unwrap().unwrap();
    assert_eq!((a.op, used), (op::GET_INITIALIZATION_INFO, first_len));
    let (b, used2) = decode(&two[used..]).unwrap().unwrap();
    assert_eq!((b.op, b.arg, used + used2), (op::NOOP, json!({}), two.len()));
    // A non-object argument becomes {}; a missing one too.
    assert_eq!(decode(b"7[12,[]]").unwrap().unwrap().0.arg, json!({}));
    assert_eq!(decode(b"4[12]").unwrap().unwrap().0, Frame { op: 12, arg: json!({}) });
}

#[test]
fn frame_errors() {
    assert!(matches!(decode(b"99999999999["), Err(FrameError::BadPrefix(_))), "11 digits");
    assert_eq!(decode(b"1234567890"), Ok(None), "10 digits, waiting for '['");
    assert_eq!(decode(b"999999999["), Err(FrameError::TooLarge(999_999_999)));
    assert!(matches!(decode(b"hello[1,{}]"), Err(FrameError::BadPrefix(_))));
    assert!(matches!(decode(b"[1,{}]"), Err(FrameError::BadPrefix(_))), "no length");
    assert!(matches!(decode(b"12 [1,{}]"), Err(FrameError::BadPrefix(_))));
    assert!(matches!(decode(b"7[1,{x}]"), Err(FrameError::BadJson { len: 8, .. })), "broken JSON: skip 8 bytes");
    assert!(matches!(decode(b"5{\"a\"}"), Err(FrameError::BadPrefix(_))), "must be an array");
    assert!(matches!(decode(b"7[\"x\",1]"), Err(FrameError::BadJson { len: 8, .. })), "no opcode");
}

#[test]
fn hashes_discovery_and_paths() {
    assert_eq!(protocol::sha1_hex("abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(protocol::password_hash("", "x"), "");
    assert_eq!(protocol::password_hash("pw", ""), "");
    assert_eq!(protocol::password_hash("pw", "ch"), protocol::sha1_hex("pwch"));
    assert_eq!(protocol::parse_discovery("calibre wireless device client (on mac-mini);8080,9090"), Some(("mac-mini".into(), 9090)));
    assert_eq!(protocol::parse_discovery("calibre wireless device client (on a;b (c));0,9091\n"), Some(("a;b (c)".into(), 9091)));
    assert_eq!(protocol::parse_discovery("hello"), None);
    for ok in ["a.epub", "Author/Title - x.epub", "Ä/ö.epub"] {
        assert!(protocol::safe_lpath(ok), "{ok}");
    }
    for bad in ["", "/etc/passwd", "../x.epub", "a/../../x", "a//b", "a\\b", "a/./b", "x\0y"] {
        assert!(!protocol::safe_lpath(bad), "{bad:?}");
    }
    assert_eq!(parse_server("192.168.1.2:9090"), Some(("192.168.1.2".into(), 9090)));
    assert_eq!(parse_server(" host "), Some(("host".into(), DEFAULT_PORT)));
    assert_eq!(parse_server("[::1]:7"), Some(("::1".into(), 7)));
    assert_eq!(parse_server("h:x"), None);
    assert_eq!(parse_server(""), None);
}

#[test]
fn cache_slim_add_remove_prune() {
    let dir = tmp("cache");
    let path = dir.join("data/metadata.calibre.json");
    let mut c = Cache::load(&path);
    assert!(c.books.is_empty());
    c.add(&json!({"lpath": "a.epub", "uuid": "u1", "title": "A", "thumbnail": [1, 2, 3], "comments": "long"}));
    c.add(&json!({"lpath": "b.epub", "uuid": "u2", "title": "B"}));
    c.add(&json!({"lpath": "a.epub", "uuid": "u1", "title": "A2"}));
    assert_eq!(c.books.len(), 2);
    assert_eq!(c.books[0]["title"], "A2");
    assert!(c.books[0].get("thumbnail").is_none(), "only the used fields are kept");
    assert_eq!(c.books[0]["authors"], json!([]));
    assert!(c.update(&json!({"lpath": "b.epub", "uuid": "u2", "title": "B2"})));
    assert!(!c.update(&json!({"lpath": "zz.epub"})));
    c.save().unwrap();
    let mut back = Cache::load(&path);
    assert_eq!(back.books.len(), 2);
    std::fs::write(dir.join("b.epub"), b"x").unwrap();
    assert_eq!(back.prune(&dir), 1, "a.epub has no file");
    assert_eq!(back.remove("b.epub"), Some("u2".into()));
    assert_eq!(back.remove("b.epub"), None);
    std::fs::write(&path, b"{not json").unwrap();
    assert!(Cache::load(&path).books.is_empty(), "a broken file gives an empty list");
    let _ = std::fs::remove_dir_all(&dir);
}

// -- Mock calibre ----------------------------------------------------------------------------

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("calibre-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(root: &Path, password: &str) -> Config {
    Config {
        books_dir: root.join("Books"),
        password: password.into(),
        device_name: "rinky (Kindle)".into(),
        app_version: "0.2.0".into(),
        cache_file: root.join("data/metadata.calibre.json"),
        drive_file: root.join("data/driveinfo.calibre.json"),
        idle_timeout: Duration::from_secs(20),
    }
}

/// The calibre side of a connection.
struct Calibre {
    s: TcpStream,
    buf: Vec<u8>,
}

impl Calibre {
    fn send(&mut self, opcode: i64, arg: Value) {
        self.s.write_all(&encode(opcode, &arg)).unwrap();
    }
    fn raw(&mut self, bytes: &[u8]) {
        self.s.write_all(bytes).unwrap();
        self.s.flush().unwrap();
    }
    fn recv(&mut self) -> Frame {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some((f, used)) = decode(&self.buf).unwrap() {
                self.buf.drain(..used);
                return f;
            }
            assert!(Instant::now() < deadline, "no frame from the device");
            let mut tmp = [0u8; 4096];
            let n = self.s.read(&mut tmp).unwrap();
            assert!(n > 0, "the device closed the connection");
            self.buf.extend_from_slice(&tmp[..n]);
        }
    }
    fn ok(&mut self) -> Value {
        let f = self.recv();
        assert_eq!(f.op, op::OK, "expected OK, got {f:?}");
        f.arg
    }
    fn raw_in(&mut self, n: usize) -> Vec<u8> {
        while self.buf.len() < n {
            let mut tmp = [0u8; 4096];
            let k = self.s.read(&mut tmp).unwrap();
            assert!(k > 0);
            self.buf.extend_from_slice(&tmp[..k]);
        }
        self.buf.drain(..n).collect()
    }
    fn handshake(&mut self, challenge: &str) -> Value {
        self.send(
            op::GET_INITIALIZATION_INFO,
            json!({"serverProtocolVersion": 1, "calibre_version": [7, 20, 0], "passwordChallenge": challenge, "currentLibraryName": "Library", "validExtensions": ["epub"]}),
        );
        self.ok()
    }
}

/// Runs `script` as calibre on a localhost port and a session against it; returns the
/// session's end, its events and the script's result.
fn session_with<R: Send + 'static>(cfg: &Config, cancel: &AtomicBool, script: impl FnOnce(Calibre) -> R + Send + 'static) -> (End, Vec<Event>, R) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        script(Calibre { s, buf: Vec::new() })
    });
    let stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    let events = Mutex::new(Vec::new());
    let end = session::run(&stream, cfg, &|e| events.lock().unwrap().push(e), &|| cancel.load(Ordering::Relaxed));
    drop(stream);
    let r = server.join().unwrap();
    (end, events.into_inner().unwrap(), r)
}

fn book_frame(lpath: &str, uuid: &str, title: &str, len: usize, this: usize, total: usize, wants_ok: bool) -> Value {
    json!({
        "lpath": lpath, "length": len, "thisBook": this, "totalBooks": total,
        "willStreamBooks": true, "willStreamBinary": true, "wantsSendOkToSendbook": wants_ok, "canSupportLpathChanges": true,
        "metadata": {"lpath": lpath, "uuid": uuid, "title": title, "authors": ["Ann Author"], "last_modified": "2026-11-01T10:00:00+00:00", "thumbnail": [240, 320, "AAAA"]},
    })
}

#[test]
fn mock_calibre_full_session() {
    let root = tmp("full");
    let cfg = config(&root, "secret");
    let one: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    let two: Vec<u8> = b"second book bytes".repeat(500);
    let (one2, two2) = (one.clone(), two.clone());
    let cancel = AtomicBool::new(false);
    let (end, events, got) = session_with(&cfg, &cancel, move |mut c| {
        let init = c.handshake("chal-123");
        assert_eq!(init["passwordHash"], protocol::sha1_hex("secretchal-123"));
        assert_eq!(init["appName"], "rinky");
        assert_eq!(init["canSendOkToSendbook"], true);
        assert_eq!(init["acceptedExtensions"][0], "epub");
        c.send(op::GET_DEVICE_INFORMATION, json!({}));
        let di = c.ok();
        assert_eq!(di["device_info"]["device_name"], "rinky (Kindle)");
        assert!(di["device_info"].get("device_store_uuid").is_none(), "first connection: calibre makes the id");
        c.send(op::SET_CALIBRE_DEVICE_INFO, json!({"device_store_uuid": "store-1", "device_name": "rinky (Kindle)", "location_code": "main"}));
        c.ok();
        c.send(op::SET_LIBRARY_INFO, json!({"libraryName": "Library", "libraryUuid": "lib-1"}));
        c.ok();
        c.send(op::TOTAL_SPACE, json!({}));
        assert!(c.ok()["total_space_on_device"].as_u64().unwrap() > 0);
        c.send(op::FREE_SPACE, json!({}));
        assert!(c.ok()["free_space_on_device"].as_u64().unwrap() > 0);
        c.send(op::GET_BOOK_COUNT, json!({"canStream": true, "canScan": true, "willUseCachedMetadata": true}));
        assert_eq!(c.ok()["count"], 0);
        c.send(op::SEND_BOOKLISTS, json!({"count": 0, "collections": {}, "willStreamMetadata": true}));

        // Book 1: OK first, then the bytes in pieces; the last piece carries book 2's frame.
        c.send(op::SEND_BOOK, book_frame("Ann Author/One.epub", "uuid-1", "One", one2.len(), 0, 2, true));
        assert_eq!(c.ok(), json!({}));
        c.raw(&one2[..1]);
        c.raw(&one2[1..40_000]);
        let mut tail = one2[40_000..].to_vec();
        // Book 2 without the OK handshake: frame and all bytes at once.
        tail.extend(encode(op::SEND_BOOK, &book_frame("Ann Author/Two.epub", "uuid-2", "Two", two2.len(), 1, 2, false)));
        tail.extend(&two2);
        c.raw(&tail);
        // A malformed frame is skipped; the stream goes on.
        c.raw(b"7[1,{x}]");
        c.send(op::SEND_BOOK_METADATA, json!({"index": 0, "count": 1, "data": {"lpath": "Ann Author/Two.epub", "uuid": "uuid-2", "title": "Two (revised)"}}));

        c.send(op::DELETE_BOOK, json!({"lpaths": ["Ann Author/One.epub"]}));
        assert_eq!(c.ok(), json!({}));
        assert_eq!(c.ok(), json!({"uuid": "uuid-1"}));

        c.send(op::GET_BOOK_COUNT, json!({"canStream": true, "canScan": true, "willUseCachedMetadata": true}));
        assert_eq!(c.ok()["count"], 1);
        let id = c.ok();
        assert_eq!((id["priKey"].clone(), id["lpath"].clone(), id["uuid"].clone()), (json!(1), json!("Ann Author/Two.epub"), json!("uuid-2")));
        assert_eq!(id["last_modified"], "2026-11-01T10:00:00+00:00");
        c.send(op::NOOP, json!({"count": 1}));
        c.send(op::NOOP, json!({"priKey": 1}));
        let meta = c.ok();
        assert_eq!(meta["title"], "Two (revised)");
        assert_eq!(meta["size"], two2.len());

        c.send(op::NOOP, json!({}));
        assert_eq!(c.ok(), json!({}), "keep-alive");
        c.send(op::GET_BOOK_FILE_SEGMENT, json!({"lpath": "Ann Author/Two.epub", "position": 0, "thisBook": 0, "totalBooks": 1, "canStream": true, "canStreamBinary": true}));
        let n = c.ok()["fileLength"].as_u64().unwrap() as usize;
        let back = c.raw_in(n);
        c.send(op::GET_BOOK_FILE_SEGMENT, json!({"lpath": "missing.epub"}));
        assert_eq!(c.recv().op, op::NOOP);
        c.send(op::NOOP, json!({"ejecting": true}));
        c.ok();
        back
    });
    assert_eq!(end, End::Ejected);
    assert_eq!(got, two, "GET_BOOK_FILE_SEGMENT sends the stored book");
    let books = root.join("Books");
    assert!(!books.join("Ann Author/One.epub").exists(), "deleted");
    assert_eq!(std::fs::read(books.join("Ann Author/Two.epub")).unwrap(), two);
    let leftovers: Vec<_> = std::fs::read_dir(books.join("Ann Author")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(leftovers.len(), 1, "no .part files: {leftovers:?}");
    assert!(events.contains(&Event::Connected { calibre_version: "7.20.0".into() }));
    let received: Vec<_> = events.iter().filter_map(|e| if let Event::Received { this, total, title, .. } = e { Some((*this, *total, title.clone())) } else { None }).collect();
    assert_eq!(received, [(1, 2, "One".to_string()), (2, 2, "Two".to_string())]);
    assert_eq!(events.iter().filter(|e| matches!(e, Event::BatchDone { total: 2 })).count(), 1);
    assert!(events.contains(&Event::Deleted(vec!["One".into()])));
    let cache: Value = ink_plugin::serde_json::from_slice(&std::fs::read(&cfg.cache_file).unwrap()).unwrap();
    assert_eq!(cache.as_array().map(Vec::len), Some(1));
    assert!(cache[0].get("thumbnail").is_none());
    let drive: Value = ink_plugin::serde_json::from_slice(&std::fs::read(&cfg.drive_file).unwrap()).unwrap();
    assert_eq!(drive["device_store_uuid"], "store-1");

    // Next connection: the stored drive id goes back to calibre; the book list stays.
    let (end, _, ()) = session_with(&cfg, &cancel, |mut c| {
        assert_eq!(c.handshake("")["passwordHash"], "", "no challenge, no hash");
        c.send(op::GET_DEVICE_INFORMATION, json!({}));
        assert_eq!(c.ok()["device_info"]["device_store_uuid"], "store-1");
        c.send(op::GET_BOOK_COUNT, json!({}));
        assert_eq!(c.ok()["count"], 1);
        c.ok();
        c.send(op::NOOP, json!({"ejecting": true}));
        c.ok();
    });
    assert_eq!(end, End::Ejected);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn wrong_password() {
    let root = tmp("pw");
    let (end, events, ()) = session_with(&config(&root, "nope"), &AtomicBool::new(false), |mut c| {
        c.handshake("ch");
        c.send(op::DISPLAY_MESSAGE, json!({"messageKind": 1, "message": "Invalid password"}));
    });
    assert_eq!(end, End::WrongPassword);
    assert!(!events.iter().any(|e| matches!(e, Event::Connected { .. })));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn broken_streams_end_the_session() {
    let root = tmp("broken");
    let cfg = config(&root, "");
    let cancel = AtomicBool::new(false);
    for (bytes, want) in [(&b"99999999999["[..], "length prefix"), (b"999999999[", "larger than"), (b"hello[0,{}]", "length prefix")] {
        let (end, _, ()) = session_with(&cfg, &cancel, move |mut c| c.raw(bytes));
        assert!(matches!(&end, End::Failed(m) if m.contains(want)), "{bytes:?}: {end:?}");
    }
    let (end, _, ()) = session_with(&cfg, &cancel, drop);
    assert_eq!(end, End::Closed);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn disconnect_mid_book_leaves_no_file() {
    let root = tmp("midbook");
    let (end, events, ()) = session_with(&config(&root, ""), &AtomicBool::new(false), |mut c| {
        c.handshake("");
        c.send(op::SEND_BOOK, book_frame("A/Half.epub", "u", "Half", 10_000, 0, 1, true));
        c.ok();
        c.raw(&[7u8; 3000]);
    });
    assert_eq!(end, End::Closed);
    assert!(events.contains(&Event::Receiving { this: 1, total: 1, title: "Half".into() }));
    assert!(!events.iter().any(|e| matches!(e, Event::Received { .. })));
    let dir = root.join("Books/A");
    let left: Vec<_> = std::fs::read_dir(&dir).map(|r| r.map(|e| e.unwrap().file_name()).collect()).unwrap_or_default();
    assert!(left.is_empty(), "no book and no .part: {left:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unsafe_lpath_is_refused() {
    let root = tmp("lpath");
    let (end, events, ()) = session_with(&config(&root, ""), &AtomicBool::new(false), |mut c| {
        c.handshake("");
        c.send(op::SEND_BOOK, book_frame("../evil.epub", "u", "Evil", 5, 0, 1, true));
        let f = c.recv();
        assert_eq!(f.op, op::ERROR, "calibre 4.18+ gets ERROR and sends no bytes");
        assert!(f.arg["message"].as_str().unwrap().contains("refused path"));
        c.send(op::NOOP, json!({"ejecting": true}));
        c.ok();
    });
    assert_eq!(end, End::Ejected);
    assert!(!root.join("evil.epub").exists());
    assert!(events.iter().any(|e| matches!(e, Event::ReceiveFailed { .. })));
    assert!(events.contains(&Event::BatchDone { total: 1 }));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cancel_sends_a_parting_noop() {
    let root = tmp("cancel");
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let (end, _, parting) = session_with(&config(&root, ""), &cancel.clone(), move |mut c| {
        c.handshake("");
        c2.store(true, Ordering::Relaxed);
        c.recv()
    });
    assert_eq!(end, End::Cancelled);
    assert_eq!(parting.op, op::NOOP);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn discovery_on_localhost() {
    let calibre = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = calibre.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let (n, from) = calibre.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
        calibre.send_to(b"calibre wireless device client (on studio);8080,9123", from).unwrap();
    });
    // A silent port first: discovery moves on after the wait.
    let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
    let targets = [silent.local_addr().unwrap(), SocketAddr::from(([127, 0, 0, 1], port))];
    let found = discovery::discover(&targets, Duration::from_millis(300), &|| false).unwrap().unwrap();
    t.join().unwrap();
    assert_eq!(found.name, "studio");
    assert_eq!(found.addr, SocketAddr::from(([127, 0, 0, 1], 9123)));
}

// -- Plugin ----------------------------------------------------------------------------------

#[test]
fn plugin_connects_receives_and_rescans() {
    let root = tmp("plugin");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut c = Calibre { s, buf: Vec::new() };
        c.handshake("");
        c.send(op::GET_DEVICE_INFORMATION, json!({}));
        c.ok();
        c.send(op::SEND_BOOK, book_frame("B/Book.epub", "u1", "Book", 100, 0, 1, true));
        c.ok();
        c.raw(&[1u8; 100]);
        c.send(op::NOOP, json!({"ejecting": true}));
        c.ok();
    });
    let mut store = MemoryStorage { dir: root.join("data"), ..Default::default() };
    store.settings.insert("server".into(), json!(addr.to_string()));
    store.settings.insert("books_dir".into(), json!(root.join("Books").to_str().unwrap()));
    let perms = [Permission::Ui, Permission::Storage, Permission::Network];
    let mut ui = UiQueue::default();
    let mut p = CalibrePlugin::new();
    assert_eq!(p.tick_interval(), None, "no ticks while idle");
    p.on_action(&mut HostCtx::new(&mut ui, &mut store, &perms), "connect").unwrap();
    assert!(p.tick_interval().is_some());
    let deadline = Instant::now() + Duration::from_secs(10);
    while p.connected() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        p.on_tick(&mut HostCtx::new(&mut ui, &mut store, &perms)).unwrap();
    }
    server.join().unwrap();
    assert!(!p.connected());
    assert_eq!(p.status(), &Status::Idle);
    let toasts: Vec<String> = ui.0.iter().filter_map(|r| if let UiRequest::Toast { text } = r { Some(text.clone()) } else { None }).collect();
    assert!(toasts.iter().any(|t| t.starts_with("Connecting to 127.0.0.1")), "{toasts:?}");
    assert!(toasts.iter().any(|t| t == "Received Book (1/1)"), "{toasts:?}");
    assert!(toasts.iter().any(|t| t == "calibre disconnected the reader"), "{toasts:?}");
    assert_eq!(ui.0.iter().filter(|r| matches!(r, UiRequest::LibraryRescan)).count(), 1, "one rescan per batch");
    assert!(root.join("Books/B/Book.epub").is_file());
    assert!(root.join("data").join(CACHE_FILE).is_file());

    // The settings page shows the status and the settings.
    ui.0.clear();
    p.on_action(&mut HostCtx::new(&mut ui, &mut store, &perms), "settings:calibre").unwrap();
    let Some(UiRequest::Menu { items, .. }) = ui.0.last() else { panic!("{:?}", ui.0) };
    assert_eq!(items[0].0, "Not connected");
    assert_eq!(items[1].1, "connect");
    assert!(items.iter().any(|(l, _)| l == &format!("Server\t{addr}")));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn plugin_reports_refused_connection() {
    let root = tmp("refused");
    // A port nobody listens on.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut store = MemoryStorage { dir: root.join("data"), ..Default::default() };
    store.settings.insert("server".into(), json!(format!("127.0.0.1:{port}")));
    let perms = [Permission::Ui, Permission::Storage, Permission::Network];
    let mut ui = UiQueue::default();
    let mut p = CalibrePlugin::new();
    p.connect(&mut HostCtx::new(&mut ui, &mut store, &perms)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while p.connected() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        p.on_tick(&mut HostCtx::new(&mut ui, &mut store, &perms)).unwrap();
    }
    assert!(matches!(p.status(), Status::Ended(m) if m.contains("connection refused")), "{:?}", p.status());
    // Without the network permission nothing starts.
    let mut q = CalibrePlugin::new();
    assert!(q.connect(&mut HostCtx::new(&mut ui, &mut store, &[Permission::Ui, Permission::Storage])).is_err());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tile_entry_and_the_open_settings_row() {
    let root = tmp("tile");
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut store = MemoryStorage { dir: root.join("data"), ..Default::default() };
    store.settings.insert("server".into(), json!(format!("127.0.0.1:{port}")));
    let perms = [Permission::Ui, Permission::Storage, Permission::Network];
    let mut ui = UiQueue::default();
    let mut p = CalibrePlugin::new();

    // Registered: the "Calibre" tile (needs Wi-Fi) and the library action.
    let mut reg = ink_plugin::Registry::default();
    p.on_load(&mut Registrar { plugin: NAME, registry: &mut reg }).unwrap();
    let tile = reg.control_tiles.iter().find(|t| t.id == "toggle").expect("tile");
    assert_eq!((tile.label.as_str(), tile.icon.as_str()), ("Calibre", "library"));
    assert!(tile.needs_wifi.is_some());
    let entry = reg.library_entries.iter().find(|e| e.id == "connect").expect("library entry");
    assert_eq!(entry.label, "Connect to calibre");
    assert_eq!(p.tile_state("toggle"), Some(TileState { text: "Tap to connect".into(), on: Some(false) }));
    assert_eq!(p.tile_state("other"), None);

    // A tap connects (the tile shows it at once) and ticks bring the end of the attempt.
    // The settings menu is closed: no row to update.
    p.on_action(&mut HostCtx::new(&mut ui, &mut store, &perms), "toggle").unwrap();
    assert!(p.connected());
    let on = p.tile_state("toggle").unwrap();
    assert_eq!(on.on, Some(true));
    assert!(on.text.ends_with('…'), "{on:?}");
    let run_ticks = |p: &mut CalibrePlugin, ui: &mut UiQueue, store: &mut MemoryStorage| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while p.connected() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            p.on_tick(&mut HostCtx::new(ui, store, &perms)).unwrap();
        }
    };
    run_ticks(&mut p, &mut ui, &mut store);
    assert!(!p.connected());
    assert!(!ui.0.iter().any(|r| matches!(r, UiRequest::MenuLabel { .. })), "{:?}", ui.0);
    assert_eq!(p.tile_state("toggle"), Some(TileState { text: "Not connected".into(), on: Some(false) }));

    // With the menu open, a status change updates its first row and nothing else.
    ui.0.clear();
    p.on_action(&mut HostCtx::new(&mut ui, &mut store, &perms), "settings:calibre").unwrap();
    assert!(matches!(ui.0.last(), Some(UiRequest::Menu { id: MENU, .. })));
    ui.0.clear();
    p.on_action(&mut HostCtx::new(&mut ui, &mut store, &perms), "toggle").unwrap();
    run_ticks(&mut p, &mut ui, &mut store);
    let labels: Vec<&UiRequest> = ui.0.iter().filter(|r| matches!(r, UiRequest::MenuLabel { .. })).collect();
    assert!(!labels.is_empty(), "{:?}", ui.0);
    let Some(UiRequest::MenuLabel { id, action, label }) = labels.last() else { unreachable!() };
    assert_eq!((*id, action.as_str()), (MENU, "status"));
    assert!(label.starts_with("Not connected (") && label.contains("connection refused"), "{label}");
    assert!(labels.windows(2).all(|w| w[0] != w[1]), "a label is sent only when it changed");

    // Once the menu is answered (closed), no more row updates.
    p.on_ui_result(&mut HostCtx::new(&mut ui, &mut store, &perms), MENU, "").unwrap();
    ui.0.clear();
    p.on_action(&mut HostCtx::new(&mut ui, &mut store, &perms), "toggle").unwrap();
    run_ticks(&mut p, &mut ui, &mut store);
    assert!(!ui.0.iter().any(|r| matches!(r, UiRequest::MenuLabel { .. })));

    // A tap while connected disconnects.
    let mut q = CalibrePlugin::new();
    q.on_action(&mut HostCtx::new(&mut ui, &mut store, &perms), "toggle").unwrap();
    q.on_action(&mut HostCtx::new(&mut ui, &mut store, &perms), "toggle").unwrap();
    assert_eq!(q.status(), &Status::Disconnecting);
    let _ = std::fs::remove_dir_all(&root);
}
