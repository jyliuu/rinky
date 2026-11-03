//! Verifier cycle 1 (wave-a/net): hostile inputs and timing against ink-net's public API.
//!
//! Each test is one attack. Raw `TcpListener` servers in this file play misbehaving peers
//! (hung TLS handshakes, truncated bodies, stalls, 1 GB bodies, servers that ignore
//! `Accept-Encoding`); `ink_net::testserver` plays the well-behaved ones.
//!
//! Seven tests here (six findings) failed on the branch head they were written against
//! (6a14b73) and were marked `#[ignore = "verifier: ..."]`: the double-started download,
//! the HTTPS body without a length cut without close_notify, the two SSE buffer caps, the
//! unclosed tag inside OPDS content, the quadratic `url::resolve` and
//! `Response::bytes(usize::MAX)`. They are fixed and run normally now; none is ignored.

use ink_net::download::{part_path, sha256_hex, Checksum, Download, Hooks};
use ink_net::http::{ClientConfig, HttpClient, Request, Response, TlsRoots, UreqClient};
use ink_net::kosync::{decide, KoSyncAccount, KoSyncClient, Progress, SyncDecision};
use ink_net::opds::{fill_template, parse_any, parse_feed, OpdsClient};
use ink_net::sse::{SseParser, SseReader};
use ink_net::testserver::{file_bytes, State, TestServer, TEST_CA_PEM};
use ink_net::{CancelToken, NetError};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------
// Helpers: raw servers.

type Handler = dyn Fn(&mut dyn ReadWrite, String, usize) + Send + Sync;

trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

/// Reads one request head (up to the blank line).
fn read_head(s: &mut dyn Read) -> String {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") && buf.len() < 65536 {
        match s.read(&mut b) {
            Ok(1) => buf.push(b[0]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn header_of<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim()))
}

/// Plain HTTP server; `h(stream, request head, connection number)` runs per connection.
fn raw_server(h: impl Fn(&mut dyn ReadWrite, String, usize) + Send + Sync + 'static) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let h: Arc<Handler> = Arc::new(h);
    let n = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut s) = c else { continue };
            let (h, i) = (h.clone(), n.fetch_add(1, Ordering::SeqCst));
            std::thread::spawn(move || {
                let _ = s.set_nodelay(true);
                let head = read_head(&mut s);
                h(&mut s, head, i);
            });
        }
    });
    base
}

fn server_tls_config() -> Arc<rustls::ServerConfig> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let mut certs = Vec::new();
    for item in ureq::tls::parse_pem(include_bytes!("../testdata/tls/test-server.pem")) {
        if let ureq::tls::PemItem::Certificate(c) = item.unwrap() {
            certs.push(CertificateDer::from(c.der().to_vec()));
        }
    }
    let mut key = None;
    for item in ureq::tls::parse_pem(include_bytes!("../testdata/tls/test-server.key.pem")) {
        if let ureq::tls::PemItem::PrivateKey(k) = item.unwrap() {
            key = Some(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(k.der().to_vec())));
        }
    }
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key.unwrap())
        .unwrap();
    Arc::new(cfg)
}

/// HTTPS server with the TEST-ONLY certificate. Dropping the stream at the end of the
/// handler closes TCP without a TLS close_notify (a broken connection).
fn raw_tls_server(h: impl Fn(&mut dyn ReadWrite, String, usize) + Send + Sync + 'static) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("https://{}", l.local_addr().unwrap());
    let cfg = server_tls_config();
    let h: Arc<Handler> = Arc::new(h);
    let n = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(tcp) = c else { continue };
            let (h, i, cfg) = (h.clone(), n.fetch_add(1, Ordering::SeqCst), cfg.clone());
            std::thread::spawn(move || {
                let _ = tcp.set_nodelay(true);
                let mut s = rustls::StreamOwned::new(rustls::ServerConnection::new(cfg).unwrap(), tcp);
                let head = read_head(&mut s);
                h(&mut s, head, i);
            });
        }
    });
    base
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-net-verify1-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn trusted() -> UreqClient {
    UreqClient::with_ca_pem(TEST_CA_PEM).unwrap()
}

fn echo_headers(resp: Response, name: &str) -> Vec<String> {
    let v: serde_json::Value = resp.json(1 << 16).unwrap();
    v["headers"].as_array().unwrap().iter().filter(|kv| kv[0] == name).map(|kv| kv[1].as_str().unwrap().to_string()).collect()
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// A valid gzip member holding `data` in one stored (uncompressed) deflate block.
fn gzip_stored(data: &[u8]) -> Vec<u8> {
    assert!(data.len() < 65536);
    let mut v = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff, 1];
    let len = data.len() as u16;
    v.extend_from_slice(&len.to_le_bytes());
    v.extend_from_slice(&(!len).to_le_bytes());
    v.extend_from_slice(data);
    v.extend_from_slice(&crc32(data).to_le_bytes());
    v.extend_from_slice(&(data.len() as u32).to_le_bytes());
    v
}

/// Writes `total` zero bytes in 64 KiB pieces until done or the peer goes away.
fn pour_zeros(w: &mut dyn ReadWrite, total: u64) {
    let block = vec![0u8; 64 * 1024];
    let mut sent = 0u64;
    while sent < total {
        let n = block.len().min((total - sent) as usize);
        if w.write_all(&block[..n]).is_err() {
            return;
        }
        sent += n as u64;
    }
}

const GB: u64 = 1 << 30;

// ---------------------------------------------------------------------------------------
// TLS and hanging servers.

#[test]
fn hung_tls_handshake_hits_the_head_timeout() {
    // Accepts TCP, never answers the ClientHello.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("https://{}/", l.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for c in l.incoming().flatten() {
            held.push(c);
        }
    });
    let t = Instant::now();
    let r = trusted().send(Request::get(&url).timeout(Duration::from_millis(500)));
    let el = t.elapsed();
    assert!(matches!(r, Err(NetError::Timeout) | Err(NetError::Tls(_))), "{r:?}");
    assert!(el < Duration::from_secs(12), "hung handshake held the caller {el:?}");
}

#[test]
fn plain_http_server_behind_an_https_url_fails_fast() {
    let base = raw_server(|w, _head, _| {
        let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");
    });
    let url = base.replacen("http://", "https://", 1);
    let t = Instant::now();
    let r = trusted().send(Request::get(format!("{url}/")).timeout(Duration::from_secs(2)));
    assert!(matches!(r, Err(NetError::Tls(_)) | Err(NetError::Http(_)) | Err(NetError::Timeout)), "{r:?}");
    assert!(t.elapsed() < Duration::from_secs(15), "{:?}", t.elapsed());
}

#[test]
fn certificate_for_another_name_is_refused() {
    // The test certificate names 127.0.0.1, 10.0.2.2, 192.168.15.201 and localhost, not ::1.
    let Ok(s) = TestServer::bind("[::1]:0", true, Arc::new(State::default())) else {
        eprintln!("no IPv6 loopback here; skipped");
        return;
    };
    let r = trusted().send(Request::get(s.url("/echo")).timeout(Duration::from_secs(3)));
    assert!(matches!(&r, Err(NetError::Tls(_))), "{r:?}");
    // Same CA, a name that is in the certificate: accepted.
    let ok = TestServer::start_tls().unwrap();
    let url = ok.url("/redirect/0").replacen("127.0.0.1", "localhost", 1);
    assert_eq!(trusted().send(Request::get(url)).unwrap().status, 200);
}

#[test]
fn headers_trickling_forever_hit_the_head_timeout() {
    // One header line every 100 ms, never the blank line: every single read succeeds, so
    // only a limit on the whole head phase can stop it.
    let base = raw_server(|w, _head, _| {
        let _ = w.write_all(b"HTTP/1.1 200 OK\r\n");
        for i in 0..600 {
            if w.write_all(format!("X-Drip-{i}: x\r\n").as_bytes()).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let t = Instant::now();
    let r = UreqClient::new().send(Request::get(format!("{base}/")).timeout(Duration::from_secs(1)));
    let el = t.elapsed();
    assert!(r.is_err(), "a never-ending head was accepted");
    assert!(el < Duration::from_secs(15), "head phase limit 1 s, caller held {el:?}: {r:?}");
}

// ---------------------------------------------------------------------------------------
// Redirects.

#[test]
fn redirect_loop_to_itself_ends_with_an_error() {
    let s = TestServer::start().unwrap();
    // /redirect-to?url=<this same URL>
    let selfref = s.url("/redirect-to?url=%2Fredirect-to%3Furl%3D%252Fredirect-to");
    let t = Instant::now();
    let r = UreqClient::new().send(Request::get(selfref));
    assert!(t.elapsed() < Duration::from_secs(15));
    match r {
        Err(NetError::Http(m)) => assert!(m.contains("too many redirects"), "{m}"),
        Ok(resp) => panic!("loop answered {} at {}", resp.status, resp.url),
        Err(e) => panic!("{e:?}"),
    }
    // Two servers bouncing a request between them.
    let a = TestServer::start().unwrap();
    let b = TestServer::start().unwrap();
    let to_a = ink_net::url::encode_component(&a.url("/redirect-to?url=") );
    let b_to_a = b.url(&format!("/redirect-to?url={to_a}"));
    let r = UreqClient::new().send(Request::get(a.url(&format!("/redirect-to?url={}", ink_net::url::encode_component(&b_to_a)))));
    assert!(r.is_err() || r.as_ref().is_ok_and(|x| x.status != 200), "{r:?}");
    assert!(a.state.requests().len() + b.state.requests().len() <= 12, "more hops than the limit");
}

#[test]
fn redirects_to_file_data_and_ftp_urls_fail_cleanly() {
    let s = TestServer::start().unwrap();
    for target in ["file:///etc/passwd", "data:text/plain,hello", "ftp://127.0.0.1/x", "javascript:alert(1)", "http://", "http://[::1"] {
        let url = s.url(&format!("/redirect-to?url={}", ink_net::url::encode_component(target)));
        let r = UreqClient::new().send(Request::get(url).timeout(Duration::from_secs(2)));
        match r {
            Err(_) => {}
            Ok(resp) => assert_ne!(resp.status, 200, "{target} was followed to a 200"),
        }
    }
}

#[test]
fn https_to_http_redirect_drops_credentials_and_https_only_refuses_it() {
    let state = Arc::new(State::default());
    let tls = TestServer::bind("127.0.0.1:0", true, state.clone()).unwrap();
    let plain = TestServer::bind("127.0.0.1:0", false, state).unwrap();
    // Same host and port number cannot be both; a scheme change alone is a new origin.
    let to = ink_net::url::encode_component(&plain.url("/echo"));
    let req = Request::get(tls.url(&format!("/redirect-to?url={to}"))).basic_auth("u", "p").header("x-auth-user", "u").header("x-auth-key", "k");
    let resp = trusted().send(req.clone()).unwrap();
    let v: serde_json::Value = resp.json(1 << 16).unwrap();
    let names: Vec<&str> = v["headers"].as_array().unwrap().iter().map(|kv| kv[0].as_str().unwrap()).collect();
    assert!(!names.contains(&"authorization"), "{names:?}");
    assert!(!names.contains(&"x-auth-key"), "{names:?}");
    let only = UreqClient::with_config(ClientConfig { https_only: true, roots: TlsRoots::from_pem(TEST_CA_PEM).unwrap(), ..Default::default() });
    assert!(matches!(only.send(req), Err(NetError::Http(m)) if m.contains("https only")));
}

#[test]
fn identity_request_sends_one_accept_encoding() {
    let s = TestServer::start().unwrap();
    let resp = UreqClient::new().send(Request::get(s.url("/echo")).set_header("Accept-Encoding", "identity")).unwrap();
    assert_eq!(echo_headers(resp, "accept-encoding"), vec!["identity".to_string()]);
}

// ---------------------------------------------------------------------------------------
// 1 GB bodies.

#[test]
fn one_gigabyte_body_is_cut_at_the_limit() {
    let base = raw_server(|w, _head, _| {
        let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/atom+xml\r\nContent-Length: {GB}\r\n\r\n").as_bytes());
        pour_zeros(w, GB);
    });
    let c = UreqClient::new();
    let t = Instant::now();
    let r = c.send(Request::get(format!("{base}/big"))).unwrap().bytes(1 << 20);
    assert!(matches!(r, Err(NetError::TooLarge(_))), "{:?}", r.map(|b| b.len()));
    let opds = OpdsClient { max_bytes: 1 << 20, ..OpdsClient::new(&c) };
    assert!(matches!(opds.fetch(&format!("{base}/feed")), Err(NetError::TooLarge(_))));
    let ks = KoSyncClient::new(&c, KoSyncAccount::new(&base, "u", "p"));
    assert!(matches!(ks.pull("d"), Err(NetError::TooLarge(_))));
    assert!(t.elapsed() < Duration::from_secs(30), "{:?}", t.elapsed());
}

#[test]
fn download_refuses_a_1gb_file_above_max_bytes_without_writing_it() {
    let base = raw_server(|w, head, _| {
        let chunked = head.contains("/chunked");
        if chunked {
            let _ = w.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
            let block = vec![b'z'; 64 * 1024];
            for _ in 0..(GB / block.len() as u64) {
                if w.write_all(format!("{:x}\r\n", block.len()).as_bytes()).and_then(|_| w.write_all(&block)).and_then(|_| w.write_all(b"\r\n")).is_err() {
                    return;
                }
            }
        } else {
            let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {GB}\r\n\r\n").as_bytes());
            pour_zeros(w, GB);
        }
    });
    let dir = tmpdir("1gb");
    let c = UreqClient::new();
    for path in ["/declared", "/chunked"] {
        let dest = dir.join(format!("{}.bin", &path[1..]));
        let mut dl = Download::new(format!("{base}{path}"), &dest);
        dl.max_bytes = Some(4 << 20);
        let t = Instant::now();
        let r = dl.run(&c, &mut Hooks::default());
        assert!(matches!(r, Err(NetError::TooLarge(_))), "{path}: {r:?}");
        assert!(t.elapsed() < Duration::from_secs(30), "{path}: {:?}", t.elapsed());
        assert!(!dest.exists(), "{path}");
        assert!(!part_path(&dest).exists() || std::fs::metadata(part_path(&dest)).unwrap().len() <= 5 << 20, "{path}");
    }
}

#[test]
fn response_bytes_with_no_practical_limit() {
    // A caller that means "no limit" passes usize::MAX.
    let r = Response::new(200, vec![], std::io::Cursor::new(b"hello".to_vec()), "http://h/");
    let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| r.bytes(usize::MAX).map_err(|e| e.to_string())));
    assert_eq!(got.ok(), Some(Ok(b"hello".to_vec())), "Response::bytes(usize::MAX) must return the body");
}

// ---------------------------------------------------------------------------------------
// SSE.

/// A body of `total` bytes produced by repeating `unit`.
struct Repeat {
    unit: Vec<u8>,
    left: u64,
    pos: usize,
}

impl Read for Repeat {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() && self.left > 0 {
            buf[n] = self.unit[self.pos];
            self.pos = (self.pos + 1) % self.unit.len();
            self.left -= 1;
            n += 1;
        }
        Ok(n)
    }
}

#[test]
fn sse_line_without_a_newline_is_capped() {
    // 64 MB of one never-ending line (a broken proxy, a binary body with the SSE type).
    // The parser buffers all of it; on the Kindle (about 400 MB free) a 1 GB body would
    // end in the OOM killer. The reader must give up with an error at a sane line limit.
    let mut r = SseReader::new(Repeat { unit: b"a".to_vec(), left: 64 << 20, pos: 0 });
    let got = r.next_event();
    assert!(got.is_err(), "64 MB line accepted: {:?}", got.map(|e| e.map(|e| e.data.len())));
}

#[test]
fn sse_data_without_a_blank_line_is_capped() {
    let mut r = SseReader::new(Repeat { unit: b"data: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789\n".to_vec(), left: 64 << 20, pos: 0 });
    let got = r.next_event();
    assert!(got.is_err(), "64 MB event accepted: {:?}", got.map(|e| e.map(|e| e.data.len())));
}

#[test]
fn sse_invalid_utf8_and_4_byte_characters_split_everywhere() {
    let stream: &[u8] = b"data: \xF0\x9F\x98\x80 bad:\xFF\xFE\xC3 end\r\nid: \xE2\x82\xAC\r\n\r\ndata: \xF0\x9F\x8E\x89\n\n";
    let want0 = "\u{1F600} bad:\u{FFFD}\u{FFFD}\u{FFFD} end";
    for split in 0..=stream.len() {
        for split2 in split..=stream.len() {
            let mut p = SseParser::new();
            let mut evs = p.feed(&stream[..split]);
            evs.extend(p.feed(&stream[split..split2]));
            evs.extend(p.feed(&stream[split2..]));
            assert_eq!(evs.len(), 2, "splits {split}/{split2}");
            assert_eq!(evs[0].data, want0, "splits {split}/{split2}");
            assert_eq!(evs[0].id.as_deref(), Some("\u{20AC}"));
            assert_eq!(evs[1].data, "\u{1F389}");
        }
    }
}

#[test]
fn sse_server_dying_mid_event_emits_no_half_event() {
    // Chunked stream, one full event, half of a second, then the TCP connection drops.
    let base = raw_server(|w, _head, _| {
        let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n");
        let body = b"data: whole\n\ndata: half";
        let _ = w.write_all(format!("{:x}\r\n", body.len()).as_bytes());
        let _ = w.write_all(body);
        let _ = w.write_all(b"\r\n");
        let _ = w.flush();
        std::thread::sleep(Duration::from_millis(100));
    });
    let resp = UreqClient::new().send(Request::get(format!("{base}/sse"))).unwrap();
    let mut r = SseReader::new(resp.body);
    assert_eq!(r.next_event().unwrap().unwrap().data, "whole");
    match r.next_event() {
        Ok(None) | Err(_) => {}
        Ok(Some(ev)) => panic!("half event delivered: {ev:?}"),
    }
}

// ---------------------------------------------------------------------------------------
// Downloads: truncation, stalls, resume, servers that ignore the request.

#[test]
fn truncated_chunked_body_is_not_renamed_into_place() {
    let base = raw_server(|w, _head, _| {
        let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nTransfer-Encoding: chunked\r\n\r\n");
        for _ in 0..3 {
            let _ = w.write_all(b"1000\r\n");
            let _ = w.write_all(&[b'q'; 0x1000]);
            let _ = w.write_all(b"\r\n");
        }
        let _ = w.flush();
        // No terminating 0-chunk: the connection just ends.
    });
    let dir = tmpdir("chunked-cut");
    let dest = dir.join("book.epub");
    let r = Download::new(format!("{base}/book"), &dest).run(&UreqClient::new(), &mut Hooks::default());
    assert!(r.is_err(), "truncated chunked body accepted: {r:?}");
    assert!(!dest.exists(), "truncated file renamed into place");
}

#[test]
fn truncated_tls_body_without_length_is_not_renamed_into_place() {
    // HTTPS, no Content-Length, no chunking: the body ends when the connection ends. The
    // server drops TCP without a TLS close_notify (Wi-Fi gone, server crash): that is a
    // truncation attack/accident TLS can detect, so it must not count as the end.
    let base = raw_tls_server(|w, _head, _| {
        let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nConnection: close\r\n\r\n");
        let _ = w.write_all(&[b'k'; 10_000]);
        let _ = w.flush();
    });
    let dir = tmpdir("tls-cut");
    let dest = dir.join("book.epub");
    let r = Download::new(format!("{base}/book"), &dest).run(&trusted(), &mut Hooks::default());
    assert!(r.is_err(), "body cut without close_notify accepted as complete: {r:?}");
    assert!(!dest.exists(), "truncated file renamed into place");
}

#[test]
fn stalled_transfer_times_out_keeps_the_part_and_resumes() {
    // Wi-Fi drops without a FIN: the first connection sends half and goes silent.
    let total = 200_000usize;
    let data = Arc::new(file_bytes(total));
    let d2 = data.clone();
    let base = raw_server(move |w, head, conn| {
        let range = header_of(&head, "range").and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
        if conn == 0 {
            let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: \"v1\"\r\n\r\n").as_bytes());
            let _ = w.write_all(&d2[..total / 2]);
            let _ = w.flush();
            std::thread::sleep(Duration::from_secs(20));
        } else if let Some(start) = range.filter(|_| header_of(&head, "if-range") == Some("\"v1\"")) {
            let _ = w.write_all(format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{}/{total}\r\nETag: \"v1\"\r\n\r\n", total - start, total - 1).as_bytes());
            let _ = w.write_all(&d2[start..]);
        } else {
            let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: \"v1\"\r\n\r\n").as_bytes());
            let _ = w.write_all(&d2[..]);
        }
    });
    let dir = tmpdir("stall");
    let dest = dir.join("s.bin");
    let mut dl = Download::new(format!("{base}/s"), &dest).checksum(Checksum::Sha256(sha256_hex(&data)));
    dl.read_timeout = Duration::from_millis(300);
    let t = Instant::now();
    let r = dl.run(&UreqClient::new(), &mut Hooks::default());
    assert!(matches!(r, Err(NetError::Timeout)), "{r:?}");
    assert!(t.elapsed() < Duration::from_secs(9), "{:?}", t.elapsed());
    assert_eq!(dl.partial_bytes(), (total / 2) as u64);
    let out = dl.run(&UreqClient::new(), &mut Hooks::default()).unwrap();
    assert_eq!(out.resumed_from, (total / 2) as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), *data);
}

#[test]
fn cancel_during_a_stall_returns_within_the_read_timeout() {
    // Documents the latency the power-key path sees: a blocked read only returns at the
    // next chunk or at read_timeout.
    let base = raw_server(|w, _head, _| {
        let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n");
        let _ = w.write_all(&[1u8; 1000]);
        let _ = w.flush();
        std::thread::sleep(Duration::from_secs(20));
    });
    let dir = tmpdir("cancel-stall");
    let mut dl = Download::new(format!("{base}/c"), dir.join("c.bin"));
    dl.read_timeout = Duration::from_secs(2);
    let token = CancelToken::new();
    let t2 = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        t2.cancel();
    });
    let t = Instant::now();
    let r = dl.run(&UreqClient::new(), &mut Hooks { cancel: token, ..Default::default() });
    let el = t.elapsed();
    eprintln!("cancel during a stall returned after {el:?}: {r:?}");
    assert!(matches!(r, Err(NetError::Cancelled) | Err(NetError::Timeout)), "{r:?}");
    assert!(el < Duration::from_secs(9), "{el:?}");
}

#[test]
fn changed_file_on_resume_starts_over_with_the_new_bytes() {
    let s = TestServer::start().unwrap();
    let dir = tmpdir("changed");
    let dest = dir.join("c.bin");
    let url = s.url("/files/5000");
    std::fs::write(part_path(&dest), vec![0xEE; 1500]).unwrap();
    let meta = serde_json::json!({"url": url, "etag": "\"old-version\"", "last_modified": null, "total": 5000});
    std::fs::write(dest.with_extension("bin.part.json"), meta.to_string()).unwrap();
    let out = Download::new(&url, &dest).run(&UreqClient::new(), &mut Hooks::default()).unwrap();
    assert_eq!(out.resumed_from, 0);
    assert_eq!(std::fs::read(&dest).unwrap(), file_bytes(5000));
    let req = &s.state.requests_to("/files/")[0];
    assert_eq!(req.header("if-range"), Some("\"old-version\""));
}

#[test]
fn corrupt_or_foreign_part_metadata_starts_over() {
    let s = TestServer::start().unwrap();
    let dir = tmpdir("corrupt-meta");
    let url = s.url("/files/5000");
    // Garbage metadata.
    let dest = dir.join("a.bin");
    std::fs::write(part_path(&dest), vec![0xAA; 100]).unwrap();
    std::fs::write(dir.join("a.bin.part.json"), b"{not json").unwrap();
    let out = Download::new(&url, &dest).run(&UreqClient::new(), &mut Hooks::default()).unwrap();
    assert_eq!((out.resumed_from, std::fs::read(&dest).unwrap()), (0, file_bytes(5000)));
    // A .part whose metadata belongs to a different URL is not reused.
    let dest = dir.join("b.bin");
    std::fs::write(part_path(&dest), vec![0xBB; 4000]).unwrap();
    std::fs::write(dir.join("b.bin.part.json"), serde_json::json!({"url": format!("{url}?other"), "etag": "\"f5000\"", "last_modified": null, "total": 5000}).to_string()).unwrap();
    let out = Download::new(&url, &dest).run(&UreqClient::new(), &mut Hooks::default()).unwrap();
    assert_eq!((out.resumed_from, std::fs::read(&dest).unwrap()), (0, file_bytes(5000)));
}

#[test]
fn server_that_ignores_identity_and_sends_gzip() {
    // Range offsets and checksums only work on the stored bytes. A server that gzips
    // anyway must give either the original bytes or an error, never a gzip file renamed
    // as the book.
    let data = file_bytes(20_000);
    let gz = gzip_stored(&data);
    let body = gz.clone();
    let base = raw_server(move |w, _head, _| {
        let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
        let _ = w.write_all(&body);
    });
    let dir = tmpdir("gzip-anyway");
    let dest = dir.join("g.epub");
    let r = Download::new(format!("{base}/g"), &dest).run(&UreqClient::new(), &mut Hooks::default());
    eprintln!("gzip-ignoring server: {r:?}");
    if r.is_ok() {
        assert_eq!(std::fs::read(&dest).unwrap(), data, "stored bytes are not the book");
    }
}

#[test]
fn hostile_content_disposition_names() {
    use ink_net::download::{content_disposition_filename, suggest_filename};
    for v in [
        "attachment; filename=\"..\"",
        "attachment; filename*=UTF-8''..%2F..%2F.ssh%2Fauthorized_keys",
        "attachment; filename=\"/etc/passwd\"",
        "attachment; filename=\"C:\\\\Windows\\\\x.epub\"",
        "attachment; filename*=UTF-8''%00%0A.epub",
        "attachment; filename=\"\"",
    ] {
        let safe = suggest_filename(content_disposition_filename(v).as_deref(), None, "application/epub+zip", "http://h/x");
        assert!(!safe.is_empty() && safe != "." && safe != ".." && !safe.contains('/') && !safe.contains('\\') && !safe.contains('\0'), "{v} -> {safe:?}");
    }
}

// ---------------------------------------------------------------------------------------
// OPDS.

/// Fixtures are compiled in, so the cross-built test binary runs on the guest too.
fn fixture(name: &str) -> String {
    match name {
        "gutenberg-root.xml" => include_str!("../testdata/opds/gutenberg-root.xml"),
        "gutenberg-1342.xml" => include_str!("../testdata/opds/gutenberg-1342.xml"),
        "edge-cases.xml" => include_str!("../testdata/opds/edge-cases.xml"),
        "gutenberg-osd.xml" => include_str!("../testdata/opds/gutenberg-osd.xml"),
        "test-catalog-home.json" => include_str!("../testdata/opds/test-catalog-home.json"),
        "test-catalog-publications.json" => include_str!("../testdata/opds/test-catalog-publications.json"),
        other => panic!("no fixture {other}"),
    }
    .to_string()
}

#[test]
fn truncated_and_garbled_feeds_never_panic() {
    for name in ["gutenberg-root.xml", "gutenberg-1342.xml", "edge-cases.xml", "gutenberg-osd.xml"] {
        let xml = fixture(name);
        let cuts: Vec<usize> = (0..xml.len()).step_by(7).filter(|&i| xml.is_char_boundary(i)).collect();
        for &cut in &cuts {
            let _ = parse_feed(&xml[..cut], "http://h/a/b.xml");
            let _ = ink_net::opds::parse_opensearch(&xml[..cut], "http://h/a/b.xml");
        }
        // Bytes flipped into markup characters.
        let mut garbled = xml.clone().into_bytes();
        for i in (0..garbled.len()).step_by(97) {
            garbled[i] = b"<>&\"'/="[i % 7];
        }
        let _ = parse_any(&garbled, "application/atom+xml", "http://h/");
    }
    for name in ["test-catalog-home.json", "test-catalog-publications.json"] {
        let json = fixture(name);
        // Byte cuts, also inside UTF-8 sequences.
        for cut in (0..json.len()).step_by(11) {
            let _ = parse_any(&json.as_bytes()[..cut], "application/opds+json", "http://h/");
        }
    }
}

#[test]
fn captive_portal_page_is_a_format_error() {
    let html = b"<!DOCTYPE html><html><head><title>Hotel Wi-Fi</title></head><body><form action=/login>...</form></body></html>";
    assert!(matches!(parse_any(html, "text/html", "http://h/"), Err(NetError::Format(_))));
    assert!(matches!(parse_any(b"", "application/atom+xml", "http://h/"), Err(NetError::Format(_))));
    assert!(matches!(parse_any(b"\xFF\xFE\x00<", "", "http://h/"), Err(NetError::Format(_))));
    let base = raw_server(|w, _head, _| {
        let body = b"<html><body>Please log in</body></html>";
        let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
        let _ = w.write_all(body);
    });
    let c = UreqClient::new();
    assert!(matches!(OpdsClient::new(&c).fetch(&format!("{base}/opds")), Err(NetError::Format(_))));
}

#[test]
fn unclosed_tag_inside_content_keeps_the_links_after_it() {
    // `<br>` without `/` inside XHTML content, and an HTML `<p>` left open in a plain text
    // summary: the capture must end at </content> / </summary>, not at </entry>.
    for content in [r#"<content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml">line<br>more</div></content>"#, "<summary><p>An unclosed paragraph</summary>"] {
        let xml = format!(
            r#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"><title>T</title>
<entry><title>Book</title><id>urn:b</id>
{content}
<link rel="http://opds-spec.org/acquisition" href="/b.epub" type="application/epub+zip"/>
</entry></feed>"#
        );
        let f = parse_feed(&xml, "http://h/c.xml").unwrap();
        assert_eq!(f.entries.len(), 1, "{content}");
        assert_eq!(f.entries[0].best_acquisition().map(|l| l.href.as_str()), Some("http://h/b.epub"), "{content}");
    }
}

#[test]
fn entity_edge_cases_and_dtd_bombs() {
    let xml = r#"<?xml version="1.0"?>
<!DOCTYPE feed [<!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;"><!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;"><!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;">]>
<feed xmlns="http://www.w3.org/2005/Atom"><title>&d;&d;&d; &#0; &#xFFFFFFFF; &#xD800; &#99999999999; &amp</title>
<entry><title>&#x1F600;&#128512;</title><link rel="http://opds-spec.org/acquisition" href="&#x2F;b&amp;c.epub" type="application/epub+zip"/></entry></feed>"#;
    let t = Instant::now();
    let f = parse_feed(xml, "http://h/").unwrap();
    assert!(t.elapsed() < Duration::from_secs(3));
    assert!(f.title.len() < 200, "DTD entities expanded: {} bytes", f.title.len());
    assert_eq!(f.entries[0].title, "\u{1F600}\u{1F600}");
    assert_eq!(f.entries[0].links[0].href, "http://h/b&c.epub");
}

#[test]
fn deep_nesting_and_huge_attributes() {
    let depth = 200_000;
    let mut xml = String::from(r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>Deep</title><entry><title>E</title>"#);
    xml.push_str(&"<x>".repeat(depth));
    xml.push_str(&"</x>".repeat(depth));
    xml.push_str(&format!(r#"<link rel="http://opds-spec.org/acquisition" href="/{}" type="application/epub+zip"/></entry></feed>"#, "a".repeat(1 << 20)));
    let t = Instant::now();
    let f = parse_feed(&xml, "http://h/").unwrap();
    assert!(t.elapsed() < Duration::from_secs(15), "{:?}", t.elapsed());
    assert_eq!(f.entries.len(), 1);
    assert_eq!(f.entries[0].links[0].href.len(), "http://h/".len() + (1 << 20));
}

#[test]
fn long_dotted_hrefs_resolve_in_linear_time() {
    // A 1 MB href made of short segments (a hostile or broken catalog): every link in a
    // feed goes through url::resolve on the UI thread.
    for href in ["a/".repeat(500_000), "./".repeat(500_000), format!("{}{}", "a/".repeat(250_000), "../".repeat(250_000))] {
        let t = Instant::now();
        let _ = ink_net::url::resolve("http://h/x/y.xml", &href);
        let el = t.elapsed();
        assert!(el < Duration::from_secs(2), "resolve of a {} byte href took {el:?}", href.len());
    }
}

#[test]
fn next_links_that_cycle_stop() {
    let base = raw_server(|w, head, _| {
        let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
        let next = if path.starts_with("/a") { "/b" } else { "/a" };
        let body = format!(r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>{path}</title><link rel="next" href="{next}"/><entry><title>e</title></entry></feed>"#);
        let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/atom+xml\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
    });
    let c = UreqClient::new();
    let pages = OpdsClient::new(&c).fetch_pages(&format!("{base}/a"), 100).unwrap();
    assert_eq!(pages.len(), 2);
}

#[test]
fn search_terms_are_percent_encoded() {
    let t = "http://h/search?q={searchTerms}&p={startPage?}";
    assert_eq!(fill_template(t, "a&b=c #d+\u{e9}/?"), "http://h/search?q=a%26b%3Dc%20%23d%2B%C3%A9%2F%3F&p=");
    assert_eq!(fill_template("http://h/s{?query}", "x y"), "http://h/s?query=x%20y");
    assert_eq!(fill_template("http://h/s{searchTerms", "x"), "http://h/s{searchTerms");
}

// ---------------------------------------------------------------------------------------
// KOSync.

#[test]
fn kosync_header_injection_is_not_sent() {
    let s = TestServer::start().unwrap();
    let http = UreqClient::new();
    let c = KoSyncClient::new(&http, KoSyncAccount::new(&s.base(), "eve\r\nX-Injected: 1", "pw"));
    let r = c.authorize();
    assert!(r.is_err(), "{r:?}");
    assert!(s.state.requests().iter().all(|q| q.header("x-injected").is_none()));
}

#[test]
fn kosync_non_ascii_user_name() {
    // KOReader sends the user name as raw UTF-8 bytes in x-auth-user (LuaSocket does not
    // filter header bytes) and koreader-sync-server accepts any non-empty name without ':'.
    // A Danish or Russian KOReader account must work here too.
    let s = TestServer::start().unwrap();
    let http = UreqClient::new();
    for user in ["j\u{f8}rgen", "\u{447}\u{438}\u{442}\u{430}\u{442}\u{435}\u{43b}\u{44c}"] {
        let c = KoSyncClient::new(&http, KoSyncAccount::new(&s.base(), user, "pw"));
        c.register().unwrap();
        let r = c.authorize();
        assert!(r.is_ok(), "{user}: registered, then authorize: {r:?}");
    }
}

#[test]
fn kosync_hostile_pull_answers() {
    // Answers a real server never sends, from a broken proxy or a hostile server.
    let answers: Vec<&'static str> = vec![
        r#"{"percentage": "NaN", "progress": "/body/p", "device": "x", "device_id": "y"}"#,
        r#"{"percentage": -3, "progress": null, "timestamp": -1}"#,
        r#"{"percentage": 1e308, "timestamp": 1e300}"#,
        r#"{"percentage": [1], "progress": {"a": 1}}"#,
        r#"[]"#,
        r#"null"#,
        "<html>login</html>",
    ];
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let base = raw_server(move |w, _head, _| {
        let body = answers[n2.fetch_add(1, Ordering::SeqCst) % answers.len()];
        let _ = w.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
    });
    let http = UreqClient::new();
    let c = KoSyncClient::new(&http, KoSyncAccount::new(&base, "u", "p"));
    for _ in 0..7 {
        if let Ok(Some(p)) = c.pull("doc") {
            // Whatever came back, the decision must not panic.
            let _ = decide(Some(&p), "/body/x", 0.5, 100, "rinky", "ME");
        }
    }
}

#[test]
fn kosync_conflicts_follow_koreader() {
    let remote = Progress { document: "d".into(), progress: "/r".into(), percentage: 0.7, device: "kpw".into(), device_id: "OTHER".into(), timestamp: Some(1000), metadata: None };
    // Equal timestamps: KOReader's `body.timestamp > last_page_turn` is false -> local wins.
    assert_eq!(decide(Some(&remote), "/l", 0.2, 1000, "rinky", "ME"), SyncDecision::LocalNewer);
    // Same device name, different id: not "same device".
    assert_eq!(decide(Some(&remote), "/l", 0.2, 10, "kpw", "ME"), SyncDecision::RemoteNewer);
    // Rounding: 0.70009 floors to 0.7000 = remote.
    assert_eq!(decide(Some(&remote), "/l", 0.70009, 10, "rinky", "ME"), SyncDecision::InSync);
}

#[test]
fn kosync_401_everywhere_and_queue_keeps_on_5xx() {
    use ink_net::kosync::SyncQueue;
    let base = raw_server(|w, head, _| {
        let (status, body) = if head.contains("/users/auth") {
            (401, r#"{"code":2001,"message":"Unauthorized"}"#)
        } else {
            (503, "busy")
        };
        let _ = w.write_all(format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
    });
    let http = UreqClient::new();
    let c = KoSyncClient::new(&http, KoSyncAccount::new(&base, "u", "p"));
    assert!(matches!(c.authorize(), Err(NetError::Unauthorized(m)) if m == "Unauthorized"));
    let p = Progress { document: "d".into(), progress: "/p".into(), percentage: 0.1, device: "i".into(), device_id: "X".into(), timestamp: None, metadata: None };
    let mut q = SyncQueue::default();
    q.push(p.clone(), 0);
    assert_eq!(q.drain(&c), 0);
    assert_eq!(q.items.len(), 1, "503 keeps the queue");
    assert!(matches!(c.push(&p), Err(NetError::Server { status: 503, .. })));
}

// ---------------------------------------------------------------------------------------
// Rapid taps: the same download started twice.

#[test]
fn the_same_download_started_twice_never_leaves_a_wrong_file() {
    // A double tap on "Download" in a catalog starts two runs for one destination. The
    // second run finds the first one's .part and .part.json and resumes from them while
    // the first is still writing. Whatever each run returns, the file that ends up at
    // `dest` must be the book, byte for byte.
    let total = 400_000usize;
    let data = Arc::new(file_bytes(total));
    let d2 = data.clone();
    let base = raw_server(move |w, head, _| {
        let start = header_of(&head, "range").and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.trim_end_matches('-').parse::<usize>().ok()).filter(|_| header_of(&head, "if-range") == Some("\"v1\""));
        let from = start.unwrap_or(0);
        let head_out = match start {
            Some(s) => format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {s}-{}/{total}\r\nETag: \"v1\"\r\n\r\n", total - s, total - 1),
            None => format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nETag: \"v1\"\r\n\r\n"),
        };
        if w.write_all(head_out.as_bytes()).is_err() {
            return;
        }
        // 10 KB every 5 ms: the whole file takes about 200 ms.
        for piece in d2[from..].chunks(10_000) {
            if w.write_all(piece).and_then(|_| w.flush()).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let mut wrong = Vec::new();
    for round in 0..5 {
        let dir = tmpdir(&format!("twice-{round}"));
        let dest = dir.join("book.epub");
        let dl = Download::new(format!("{base}/book"), &dest);
        let (a, b) = (dl.clone(), dl.clone());
        let ta = std::thread::spawn(move || a.run(&UreqClient::new(), &mut Hooks::default()));
        std::thread::sleep(Duration::from_millis(40 + 10 * round as u64));
        let tb = std::thread::spawn(move || b.run(&UreqClient::new(), &mut Hooks::default()));
        let (ra, rb) = (ta.join().unwrap(), tb.join().unwrap());
        let got = std::fs::read(&dest).ok();
        let ok = got.as_deref() == Some(&data[..]);
        if (ra.is_ok() || rb.is_ok()) && !ok {
            wrong.push(format!(
                "round {round}: first {:?}, second {:?}, dest has {:?} bytes",
                ra.as_ref().map(|o| (o.bytes, o.resumed_from)).map_err(|e| e.to_string()),
                rb.as_ref().map(|o| (o.bytes, o.resumed_from)).map_err(|e| e.to_string()),
                got.map(|g| g.len())
            ));
        }
    }
    assert!(wrong.is_empty(), "a run reported success but dest is not the book:\n{}", wrong.join("\n"));
}
