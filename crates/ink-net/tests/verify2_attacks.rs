//! Verifier cycle 2 (wave-a/net): attacks on the cycle-1 fixes and on the paths next to
//! them.
//!
//! Raw `TcpListener` servers in this file play misbehaving peers; `ink_net::testserver`
//! plays the well-behaved ones. Three tests failed on the branch head they were written
//! against (ac340c7) and were marked `#[ignore = "verifier: ..."]`: quadratic OPDS end
//! tags, a 206 to a request without Range, a 401 portal page dropping the KOSync queue.
//! They are fixed and run normally now; none is ignored.
//!
//! The two memory attacks count allocations with a global allocator, so each one is a test
//! binary of its own: `tests/verify2_memory_sse.rs` and `tests/verify2_memory_opds.rs`.

use ink_net::connectivity::NetActivity;
use ink_net::download::{lock_path, part_path, sha256_hex, Checksum, Download, DownloadProgress, Hooks};
use ink_net::http::{HttpClient, Request, Response, UreqClient};
use ink_net::kosync::{KoSyncAccount, KoSyncClient, Progress, SyncQueue};
use ink_net::opds::parse_feed;
use ink_net::sse::EventSource;
use ink_net::testserver::{file_bytes, TestServer};
use ink_net::{CancelToken, NetError};
use std::io::Write;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

mod common;
use common::header_of;

// ---------------------------------------------------------------------------------------
// Helpers.

/// `bytes=N-` -> N.
fn range_start(head: &str) -> Option<usize> {
    header_of(head, "range").and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.trim_end_matches('-').parse().ok())
}

/// Plain HTTP server; `h(stream, request head, connection number)` runs per connection.
/// Returns the base URL and the connection counter.
fn raw_server(h: impl Fn(&mut std::net::TcpStream, String, usize) + Send + Sync + 'static) -> (String, Arc<AtomicUsize>) {
    common::serve("http", |tcp| tcp, h)
}

fn tmpdir(name: &str) -> PathBuf {
    common::tmpdir("verify2", name)
}

// ---------------------------------------------------------------------------------------
// OPDS: end tags that match no open element.

/// A feed with `n` unclosed `<x>` inside an entry, then `n` end tags `</y>` that match no
/// open element, then the entry's acquisition link. With `in_content` the tags sit inside
/// `<content type="xhtml">` (the capture branch of the parser).
fn unmatched_end_tags_feed(n: usize, in_content: bool) -> String {
    let mut xml = String::from(r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>T</title><entry><title>Book</title><id>urn:b</id>"#);
    if in_content {
        xml.push_str(r#"<content type="xhtml">"#);
    }
    xml.push_str(&"<x>".repeat(n));
    xml.push_str(&"</y>".repeat(n));
    if in_content {
        xml.push_str("</content>");
    }
    xml.push_str(r#"<link rel="http://opds-spec.org/acquisition" href="/b.epub" type="application/epub+zip"/></entry></feed>"#);
    xml
}

#[test]
fn opds_unmatched_end_tags_parse_in_linear_time() {
    // 50 000 unclosed elements and 50 000 end tags that close none of them: 350 KB, far
    // below OpdsClient's 16 MB limit. Each end tag used to scan the whole stack of open
    // names (`path.iter().rposition`) and the stack never shrinks, so the parse was
    // quadratic: 9 s for this feed on the host (release), minutes for 1 MB on the
    // Kindle. The parser now keeps the open depths of each name.
    let mut slow = Vec::new();
    for in_content in [false, true] {
        let xml = unmatched_end_tags_feed(50_000, in_content);
        let t = Instant::now();
        let f = parse_feed(&xml, "http://h/c.xml").unwrap();
        let el = t.elapsed();
        eprintln!("unmatched end tags (in content: {in_content}): {} bytes parsed in {el:?}", xml.len());
        assert_eq!(f.entries.len(), 1);
        assert_eq!(f.entries[0].best_acquisition().map(|l| l.href.as_str()), Some("http://h/b.epub"));
        if el > Duration::from_secs(3) {
            slow.push(format!("in content: {in_content}: {} bytes took {el:?}", xml.len()));
        }
    }
    assert!(slow.is_empty(), "quadratic end-tag handling:\n{}", slow.join("\n"));
}

#[test]
fn opds_balanced_deep_markup_stays_fast() {
    // The control for the test above: the same size with matching end tags.
    let n = 50_000;
    let mut xml = String::from(r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>T</title><entry><title>Book</title>"#);
    xml.push_str(&"<x>".repeat(n));
    xml.push_str(&"</x>".repeat(n));
    xml.push_str(r#"<link rel="http://opds-spec.org/acquisition" href="/b.epub" type="application/epub+zip"/></entry></feed>"#);
    let t = Instant::now();
    let f = parse_feed(&xml, "http://h/c.xml").unwrap();
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    assert_eq!(f.entries[0].best_acquisition().map(|l| l.href.as_str()), Some("http://h/b.epub"));
}

// ---------------------------------------------------------------------------------------
// Downloads.

#[test]
fn download_206_without_a_range_request_is_not_renamed_short() {
    // A broken server or cache answers a plain GET with `206` and the first 1000 bytes of
    // a 5000-byte book (`Content-Range: bytes 0-999/5000`, `Content-Length: 1000`). The
    // 206 was only handled as a partial answer when a `.part` existed; otherwise it took
    // the 2xx branch, which looked only at `Content-Length`, so 1000 bytes counted as the
    // whole file and were renamed to `dest`. Now the Content-Range total is the length to
    // reach: the run fails and keeps the 1000 bytes for a resume.
    let data = Arc::new(file_bytes(5000));
    let d2 = data.clone();
    let (base, _) = raw_server(move |w, _head, _| {
        let _ = w.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Type: application/epub+zip\r\nContent-Range: bytes 0-999/5000\r\nContent-Length: 1000\r\n\r\n");
        let _ = w.write_all(&d2[..1000]);
    });
    let dir = tmpdir("206-fresh");
    let dest = dir.join("book.epub");
    let r = Download::new(format!("{base}/book.epub"), &dest).run(&UreqClient::new(), &mut Hooks::default());
    let on_disk = std::fs::metadata(&dest).map(|m| m.len()).ok();
    assert!(
        on_disk.is_none() || on_disk == Some(5000),
        "a 206 for bytes 0-999 of 5000 was renamed into place as a {on_disk:?}-byte file; run returned {:?}",
        r.map(|o| (o.bytes, o.resumed_from))
    );
    assert_eq!(std::fs::metadata(part_path(&dest)).map(|m| m.len()).ok(), Some(1000), ".part kept for a resume");
}

#[test]
fn wifi_drop_mid_download_then_offline_then_resume() {
    // The radio drops mid-body (the connection ends early), the next attempt happens while
    // wifid still says PENDING, and the third one after the join resumes with Range.
    let total = 300_000usize;
    let data = Arc::new(file_bytes(total));
    let d2 = data.clone();
    let (base, conns) = raw_server(move |w, head, conn| {
        let start = range_start(&head).filter(|_| header_of(&head, "if-range") == Some("\"w1\""));
        match (conn, start) {
            (0, _) => {
                let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: \"w1\"\r\n\r\n").as_bytes());
                let _ = w.write_all(&d2[..120_000]);
                let _ = w.flush();
                let _ = w.shutdown(std::net::Shutdown::Both);
            }
            (_, Some(s)) => {
                let _ = w.write_all(format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {s}-{}/{total}\r\nETag: \"w1\"\r\n\r\n", total - s, total - 1).as_bytes());
                let _ = w.write_all(&d2[s..]);
            }
            (_, None) => {
                let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: \"w1\"\r\n\r\n").as_bytes());
                let _ = w.write_all(&d2[..]);
            }
        }
    });
    let dir = tmpdir("wifi-drop");
    let dest = dir.join("w.epub");
    let dl = Download::new(format!("{base}/w.epub"), &dest).checksum(Checksum::Sha256(sha256_hex(&data)));
    let client = UreqClient::new();
    let r = dl.run(&client, &mut Hooks::default());
    assert!(r.is_err(), "a body cut at 120000 of {total} must fail: {r:?}");
    assert_eq!(dl.partial_bytes(), 120_000);
    assert!(!dest.exists());
    // Wi-Fi still down: refused before any request, .part untouched.
    let cm_state = std::sync::Mutex::new("PENDING");
    let wifi = || ink_net::connectivity::wifi_rule(true, *cm_state.lock().unwrap());
    let r = dl.run(&client, &mut Hooks { connectivity: Some(&wifi), ..Default::default() });
    assert!(matches!(r, Err(NetError::Offline(_))), "{r:?}");
    assert_eq!(conns.load(Ordering::SeqCst), 1, "no request while offline");
    assert_eq!(dl.partial_bytes(), 120_000);
    // Joined: resume from 120000.
    *cm_state.lock().unwrap() = "CONNECTED";
    let out = dl.run(&client, &mut Hooks { connectivity: Some(&wifi), ..Default::default() }).unwrap();
    assert_eq!(out.resumed_from, 120_000);
    assert_eq!(std::fs::read(&dest).unwrap(), *data);
    assert!(!lock_path(&dest).exists());
}

#[test]
fn unframed_body_cut_then_resumed_with_206_completes() {
    // First answer: no length, no chunking, cut at 20000 of 50000 (refused: nothing
    // confirms it). The .part stays; the next run asks for the rest with Range and gets a
    // framed 206, so it completes without a checksum or expected size.
    let total = 50_000usize;
    let data = Arc::new(file_bytes(total));
    let d2 = data.clone();
    let (base, _) = raw_server(move |w, head, _| match range_start(&head) {
        None => {
            let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nETag: \"u1\"\r\nConnection: close\r\n\r\n");
            let _ = w.write_all(&d2[..20_000]);
        }
        Some(s) => {
            let _ = w.write_all(format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {s}-{}/{total}\r\nETag: \"u1\"\r\n\r\n", total - s, total - 1).as_bytes());
            let _ = w.write_all(&d2[s..]);
        }
    });
    let dir = tmpdir("unframed-resume");
    let dest = dir.join("u.epub");
    let dl = Download::new(format!("{base}/u.epub"), &dest);
    let r = dl.run(&UreqClient::new(), &mut Hooks::default());
    assert!(r.is_err(), "{r:?}");
    assert!(!dest.exists());
    assert_eq!(dl.partial_bytes(), 20_000);
    let out = dl.run(&UreqClient::new(), &mut Hooks::default()).unwrap();
    assert_eq!((out.bytes, out.resumed_from), (total as u64, 20_000));
    assert_eq!(std::fs::read(&dest).unwrap(), *data);
}

#[test]
fn resume_goes_through_a_redirect_with_range_and_if_range() {
    // Catalogs often link a download URL that redirects to a file host. The resume must
    // carry Range and If-Range across the redirect.
    let s = TestServer::start().unwrap();
    let dir = tmpdir("redirect-resume");
    let dest = dir.join("r.epub");
    let target = ink_net::url::encode_component("/files/300000");
    let dl = Download::new(s.url(&format!("/redirect-to?url={target}")), &dest).checksum(Checksum::Sha256(sha256_hex(&file_bytes(300_000))));
    let token = CancelToken::new();
    let t2 = token.clone();
    let mut hooks = Hooks {
        cancel: token,
        on_progress: Some(Box::new(move |p: &DownloadProgress| {
            if p.received >= 100_000 {
                t2.cancel();
            }
        })),
        ..Default::default()
    };
    assert!(matches!(dl.run(&UreqClient::new(), &mut hooks), Err(NetError::Cancelled)));
    drop(hooks);
    let have = dl.partial_bytes();
    assert!(have >= 100_000, "{have}");
    let out = dl.run(&UreqClient::new(), &mut Hooks::default()).unwrap();
    assert_eq!(out.resumed_from, have);
    assert_eq!(std::fs::read(&dest).unwrap(), file_bytes(300_000));
    let files = s.state.requests_to("/files/");
    assert_eq!(files.len(), 2);
    assert_eq!(files[1].header("range").map(str::to_string), Some(format!("bytes={have}-")));
    assert_eq!(files[1].header("if-range"), Some("\"f300000\""));
}

#[test]
fn the_lock_file_is_gone_after_every_kind_of_failure() {
    // A lock file left next to the book would show up in the library and could make a
    // later run think the download is busy.
    let s = TestServer::start().unwrap();
    let dir = tmpdir("lock-cleanup");
    let client = UreqClient::new();
    fn check(client: &UreqClient, name: &str, dl: Download, hooks: &mut Hooks<'_>) {
        let r = dl.run(client, hooks);
        assert!(r.is_err(), "{name}: {r:?}");
        assert!(!lock_path(&dl.dest).exists(), "{name}: lock file left behind ({r:?})");
    }
    // Cancelled mid-body.
    let token = CancelToken::new();
    let t2 = token.clone();
    let mut hooks = Hooks {
        cancel: token,
        on_progress: Some(Box::new(move |p: &DownloadProgress| {
            if p.received > 50_000 {
                t2.cancel();
            }
        })),
        ..Default::default()
    };
    check(&client, "cancel", Download::new(s.url("/files/400000"), dir.join("a.epub")), &mut hooks);
    drop(hooks);
    check(&client, "checksum", Download::new(s.url("/files/1000"), dir.join("b.epub")).checksum(Checksum::Sha256("0".repeat(64))), &mut Hooks::default());
    check(&client, "404", Download::new(s.url("/status/404"), dir.join("c.epub")), &mut Hooks::default());
    let mut big = Download::new(s.url("/files/5000"), dir.join("d.epub"));
    big.max_bytes = Some(100);
    check(&client, "too large", big, &mut Hooks::default());
    check(&client, "unframed", Download::new(s.url("/unframed/3000"), dir.join("e.epub")), &mut Hooks::default());
    check(&client, "connection refused", Download::new("http://127.0.0.1:1/x.epub", dir.join("f.epub")), &mut Hooks::default());
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".lock")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn eight_simultaneous_starts_send_one_request() {
    // Rapid taps from several UI paths at once: eight threads released together. One run
    // downloads; the others are Busy and send nothing.
    let total = 200_000usize;
    let data = Arc::new(file_bytes(total));
    let d2 = data.clone();
    let (base, conns) = raw_server(move |w, _head, _| {
        let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: \"e1\"\r\n\r\n").as_bytes());
        for piece in d2.chunks(20_000) {
            if w.write_all(piece).and_then(|_| w.flush()).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let dir = tmpdir("eight");
    let dest = dir.join("e.epub");
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (dl, b) = (Download::new(format!("{base}/e.epub"), &dest), barrier.clone());
            std::thread::spawn(move || {
                b.wait();
                dl.run(&UreqClient::new(), &mut Hooks::default())
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let busy = results.iter().filter(|r| matches!(r, Err(NetError::Busy(_)))).count();
    assert_eq!((ok, busy), (1, 7), "{results:?}");
    assert_eq!(conns.load(Ordering::SeqCst), 1, "busy runs sent requests");
    assert_eq!(std::fs::read(&dest).unwrap(), *data);
    assert!(!lock_path(&dest).exists());
    assert!(!part_path(&dest).exists());
}

// ---------------------------------------------------------------------------------------
// SSE.

/// One canned event stream per connection.
struct Streams {
    bodies: Vec<&'static [u8]>,
    sent: AtomicUsize,
}

impl HttpClient for Streams {
    fn send(&self, _req: Request) -> ink_net::Result<Response> {
        let i = self.sent.fetch_add(1, Ordering::SeqCst);
        let body = self.bodies.get(i).copied().unwrap_or(b"");
        Ok(Response::new(200, vec![("Content-Type".into(), "text/event-stream".into())], std::io::Cursor::new(body.to_vec()), "http://h/sse"))
    }
}

#[test]
fn sse_retry_near_u64_max_does_not_panic_and_cancel_ends_the_wait() {
    // `retry:` takes any run of ASCII digits that fits u64; the reconnect wait then adds
    // about 584 million years to an Instant. It must not panic, and interrupt_all (the
    // power key) must still end the stream at once.
    let client = Streams { bodies: vec![b"retry: 18446744073709551615\ndata: a\n\n"], sent: AtomicUsize::new(0) };
    let activity = NetActivity::new();
    let mut es = EventSource::new(&client, Request::get("http://h/sse"));
    es.activity = Some(activity.clone());
    let stopper = activity.clone();
    let done = Arc::new(AtomicBool::new(false));
    let d2 = done.clone();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        stopper.interrupt_all();
        d2.store(true, Ordering::SeqCst);
    });
    let start = Instant::now();
    let mut got = Vec::new();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        es.run(|ev| {
            got.push(ev.data.clone());
            ControlFlow::Continue(())
        })
    }));
    t.join().unwrap();
    let r = r.expect("EventSource panicked on a huge retry value");
    assert!(matches!(r, Err(NetError::Cancelled)), "{r:?}");
    assert!(done.load(Ordering::SeqCst));
    assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
    assert_eq!(got, vec!["a"]);
    assert_eq!(es.retry, Duration::from_millis(u64::MAX));
}

// ---------------------------------------------------------------------------------------
// KOSync.

/// A captive portal or filtering proxy in front of a plain-HTTP sync server (self-hosted
/// servers on the LAN are often `http://`): it answers every request with `401` and its
/// own HTML login page.
struct Portal401;

impl HttpClient for Portal401 {
    fn send(&self, _req: Request) -> ink_net::Result<Response> {
        let page = b"<!DOCTYPE html><html><body><h1>401 Authorization Required</h1><form action=/login>Log in to Guest Wi-Fi</form></body></html>".to_vec();
        Ok(Response::new(401, vec![("Content-Type".into(), "text/html; charset=utf-8".into())], std::io::Cursor::new(page), "http://portal/login"))
    }
}

#[test]
fn kosync_queue_survives_a_401_html_page_from_a_portal() {
    // The cycle-1 fix stops a 2xx HTML page from counting as success, "so SyncQueue keeps
    // the entry". The same portal answering 401 gave NetError::Unauthorized, and drain()
    // cleared the whole queue: weeks of unsynced progress from every book were lost
    // without the sync server ever being reached. A 401 HTML page is now a format error
    // too. KOReader's KOSyncQueue:drain keeps the remaining items on any failure (it
    // clears only on logout).
    let c = KoSyncClient::new(&Portal401, KoSyncAccount::new("http://192.0.2.10:7200", "u", "p"));
    let mut q = SyncQueue::default();
    for (i, doc) in ["a", "b", "c"].iter().enumerate() {
        q.push(
            Progress { document: (*doc).into(), progress: "/body/DocFragment[2]/body/p[1]/text().0".into(), percentage: 0.1, device: "rinky".into(), device_id: "X".into(), timestamp: None, metadata: None },
            1_800_000_000 + i as u64 * 86_400,
        );
    }
    let r = c.push(&q.items[0].progress);
    assert!(matches!(&r, Err(NetError::Format(m)) if m.contains("captive portal")), "{r:?}");
    assert_eq!(q.drain(&c), 0);
    assert_eq!(q.items.len(), 3, "a 401 login page from a portal dropped the queued pushes");
}
