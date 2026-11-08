//! A small HTTP/1.1 and HTTPS mock server for tests and emulator checks.
//!
//! Not used by rinky at run time. The same routes serve the unit tests in this crate,
//! `cargo run -p ink-net --example net-mock-server` on the host (reached by the emulator
//! guest at `10.0.2.2`) and later plugins' tests. HTTPS uses the TEST-ONLY certificate
//! in `testdata/tls/` (SANs `127.0.0.1`, `10.0.2.2`, `localhost`); clients must trust
//! [`TEST_CA_PEM`]. One request per connection (`Connection: close`).
//!
//! Routes:
//!
//! | Path | Behaviour |
//! | --- | --- |
//! | `/echo` | JSON with method, path, query, headers and body of the request |
//! | `/redirect/N` | 302 to `/redirect/N-1` (relative); `/redirect/0` answers 200 |
//! | `/redirect-to?status=S&url=U` | status `S` (default 302) with `Location: U` |
//! | `/status/C` | status `C`, JSON body `{"message": "status C"}` |
//! | `/slow?ms=N` | waits N ms before the response head |
//! | `/stall?ms=N` | 10-byte body: 2 bytes, then waits N ms, then the rest |
//! | `/sse` | an event stream in awkward chunks (CR/LF split, UTF-8 split, comments, retry) |
//! | `/sse-gated` | event `first`, then waits for [`State::open_gate`], then `second` |
//! | `/sse-reconnect` | ids 1-2 then a dropped connection; with `Last-Event-ID: 2`, id 3 and `end` |
//! | `/files/N?name=F` | N deterministic bytes ([`file_bytes`]), ETag, `Range`/`If-Range`, 416 |
//! | `/flaky/N` | like `/files/N`, but the first full GET (per path and query) breaks off after N/2 bytes |
//! | `/norange/N` | ignores `Range` (always 200) |
//! | `/badrange/N` | answers `Range` with a 206 whose `Content-Range` starts at 0 |
//! | `/gzip/N` | `Content-Encoding: gzip` only when the client accepts it (it must not, for downloads) |
//! | `/unframed/N?cut=M` | N bytes of [`file_bytes`] with no `Content-Length` and no chunking (the body ends with the connection); with `cut`, only M bytes, then the socket is dropped without a TLS close_notify |
//! | `/auth/basic` | 200 for `test:secret` Basic auth, else 401 |
//! | `/opds/root.xml`, `/opds/new.xml?page=P`, `/opds/osd.xml`, `/opds/search?q=Q&page=P` | a small OPDS 1.2 catalog (navigation, paginated acquisition feed, OpenSearch) |
//! | `/opds2/catalog.json` | an OPDS 2 feed |
//! | `/users/create`, `/users/auth`, `/syncs/progress[/DOC]` (also under `/kosync/`) | a KOReader sync server (koreader-sync-server behaviour, strict about headers) |

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The TEST-ONLY CA that signed the server certificate. Trust it with
/// [`crate::UreqClient::with_ca_pem`].
pub const TEST_CA_PEM: &[u8] = include_bytes!("../testdata/tls/test-ca.pem");
const SERVER_CERT_PEM: &[u8] = include_bytes!("../testdata/tls/test-server.pem");
const SERVER_KEY_PEM: &[u8] = include_bytes!("../testdata/tls/test-server.key.pem");

/// The `Accept` header KOReader's kosync plugin sends.
pub const KOSYNC_ACCEPT: &str = "application/vnd.koreader.v1+json";

/// One request as the server saw it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recorded {
    /// Method.
    pub method: String,
    /// Path without the query.
    pub path: String,
    /// Query without `?`.
    pub query: String,
    /// Headers, names lowercase, in arrival order.
    pub headers: Vec<(String, String)>,
    /// Body.
    pub body: Vec<u8>,
}

impl Recorded {
    /// First header named `name` (lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Value of a query parameter, percent-decoded (`+` is a space).
    pub fn param(&self, name: &str) -> Option<String> {
        self.query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (k == name).then(|| crate::url::decode_component(&v.replace('+', " ")))
        })
    }
}

/// Server state shared by connections (and by an HTTP and an HTTPS server, if wanted).
#[derive(Debug, Default)]
pub struct State {
    log: Mutex<Vec<Recorded>>,
    users: Mutex<HashMap<String, String>>,
    progress: Mutex<HashMap<(String, String), serde_json::Value>>,
    counters: Mutex<HashMap<String, u32>>,
    gate: (Mutex<bool>, Condvar),
}

impl State {
    /// Every request so far.
    pub fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }

    /// Requests whose path starts with `prefix`.
    pub fn requests_to(&self, prefix: &str) -> Vec<Recorded> {
        self.requests().into_iter().filter(|r| r.path.starts_with(prefix)).collect()
    }

    /// Lets `/sse-gated` send its second event.
    pub fn open_gate(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }

    /// Adds a KOSync user (`userkey` = MD5 hex of the password).
    pub fn add_user(&self, user: &str, userkey: &str) {
        self.users.lock().unwrap().insert(user.into(), userkey.into());
    }

    fn bump(&self, key: &str) -> u32 {
        let mut c = self.counters.lock().unwrap();
        let n = c.entry(key.to_string()).or_insert(0);
        *n += 1;
        *n
    }

    fn wait_gate(&self, max: Duration) -> bool {
        let g = self.gate.0.lock().unwrap();
        let (g, _) = self.gate.1.wait_timeout_while(g, max, |open| !*open).unwrap();
        *g
    }
}

/// A running server; dropped = stopped.
pub struct TestServer {
    addr: SocketAddr,
    tls: bool,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Shared state (request log, KOSync users and progress).
    pub state: Arc<State>,
}

impl std::fmt::Debug for TestServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestServer").field("addr", &self.addr).field("tls", &self.tls).finish_non_exhaustive()
    }
}

impl TestServer {
    /// Plain HTTP on `127.0.0.1` with a free port.
    pub fn start() -> io::Result<TestServer> {
        Self::bind("127.0.0.1:0", false, Arc::default())
    }

    /// HTTPS (test CA) on `127.0.0.1` with a free port.
    pub fn start_tls() -> io::Result<TestServer> {
        Self::bind("127.0.0.1:0", true, Arc::default())
    }

    /// Listens on `addr` (`HOST:PORT`, port 0 = any) with the given state.
    pub fn bind(addr: &str, tls: bool, state: Arc<State>) -> io::Result<TestServer> {
        let listener = TcpListener::bind(addr)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let tls_cfg = if tls { Some(server_tls_config()?) } else { None };
        let (stop2, state2) = (stop.clone(), state.clone());
        let thread = std::thread::Builder::new().name(format!("testserver-{}", addr.port())).spawn(move || {
            for conn in listener.incoming() {
                if stop2.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(tcp) = conn else { continue };
                let (state, tls_cfg) = (state2.clone(), tls_cfg.clone());
                let _ = std::thread::Builder::new().name("testserver-conn".into()).spawn(move || {
                    let _ = tcp.set_read_timeout(Some(Duration::from_secs(20)));
                    let _ = tcp.set_nodelay(true);
                    let conn = match tls_cfg {
                        Some(cfg) => match rustls::ServerConnection::new(cfg) {
                            Ok(c) => Conn::Tls(Box::new(rustls::StreamOwned::new(c, tcp))),
                            Err(_) => return,
                        },
                        None => Conn::Plain(tcp),
                    };
                    handle_connection(conn, &state);
                });
            }
        })?;
        Ok(TestServer { addr, tls, stop, thread: Some(thread), state })
    }

    /// `http://127.0.0.1:PORT` or `https://...`.
    pub fn base(&self) -> String {
        format!("{}://{}", if self.tls { "https" } else { "http" }, self.addr)
    }

    /// `base() + path`.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base(), path)
    }

    /// The bound address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Runs until the process ends (for the example binary).
    pub fn join(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        self.state.open_gate();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The rustls server configuration with the TEST-ONLY certificate (for raw servers in
/// tests that play misbehaving HTTPS peers).
pub fn server_tls_config() -> io::Result<Arc<rustls::ServerConfig>> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let bad = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
    let mut certs = Vec::new();
    for item in ureq::tls::parse_pem(SERVER_CERT_PEM) {
        if let ureq::tls::PemItem::Certificate(c) = item.map_err(|e| bad(e.to_string()))? {
            certs.push(CertificateDer::from(c.der().to_vec()));
        }
    }
    let mut key = None;
    for item in ureq::tls::parse_pem(SERVER_KEY_PEM) {
        if let ureq::tls::PemItem::PrivateKey(k) = item.map_err(|e| bad(e.to_string()))? {
            key = Some(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(k.der().to_vec())));
        }
    }
    let key = key.ok_or_else(|| bad("no key in test-server.key.pem".into()))?;
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| bad(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| bad(e.to_string()))?;
    Ok(Arc::new(cfg))
}

enum Conn {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Conn::Plain(s) => s.read(buf),
            Conn::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Conn::Plain(s) => s.write(buf),
            Conn::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Conn::Plain(s) => s.flush(),
            Conn::Tls(s) => s.flush(),
        }
    }
}

impl Conn {
    /// Ends the connection cleanly (TLS close_notify).
    fn close(mut self) {
        if let Conn::Tls(s) = &mut self {
            s.conn.send_close_notify();
            let _ = s.flush();
        }
        let _ = self.flush();
    }
}

/// How a handler wants the connection to end.
enum End {
    Clean,
    /// Drop the socket without a TLS close_notify or a final chunk (a broken transfer).
    Abrupt,
}

fn read_request(r: &mut BufReader<Conn>) -> io::Result<Option<Recorded>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end_matches(['\r', '\n']);
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let len: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0; len.min(16 << 20)];
    r.read_exact(&mut body)?;
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    Ok(Some(Recorded { method, path, query, headers, body }))
}

fn handle_connection(conn: Conn, state: &State) {
    let mut r = BufReader::new(conn);
    let req = match read_request(&mut r) {
        Ok(Some(req)) => req,
        _ => return,
    };
    state.log.lock().unwrap().push(req.clone());
    let mut conn = r.into_inner();
    match route(&req, state, &mut conn) {
        Ok(End::Clean) => conn.close(),
        Ok(End::Abrupt) | Err(_) => drop(conn),
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        406 => "Not Acceptable",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

fn head(w: &mut Conn, status: u16, headers: &[(&str, String)]) -> io::Result<()> {
    let mut s = format!("HTTP/1.1 {status} {}\r\nConnection: close\r\n", reason(status));
    for (k, v) in headers {
        s.push_str(&format!("{k}: {v}\r\n"));
    }
    s.push_str("\r\n");
    w.write_all(s.as_bytes())?;
    w.flush()
}

fn respond(w: &mut Conn, req: &Recorded, status: u16, headers: &[(&str, String)], body: &[u8]) -> io::Result<End> {
    let mut h: Vec<(&str, String)> = headers.to_vec();
    h.push(("Content-Length", body.len().to_string()));
    head(w, status, &h)?;
    if req.method != "HEAD" {
        w.write_all(body)?;
    }
    w.flush()?;
    Ok(End::Clean)
}

fn json(w: &mut Conn, req: &Recorded, status: u16, v: &serde_json::Value) -> io::Result<End> {
    respond(w, req, status, &[("Content-Type", "application/json".into())], v.to_string().as_bytes())
}

fn chunk(w: &mut Conn, data: &[u8]) -> io::Result<()> {
    w.write_all(format!("{:x}\r\n", data.len()).as_bytes())?;
    w.write_all(data)?;
    w.write_all(b"\r\n")?;
    w.flush()
}

fn end_chunks(w: &mut Conn) -> io::Result<()> {
    w.write_all(b"0\r\n\r\n")?;
    w.flush()
}

/// The deterministic content of `/files/N`.
pub fn file_bytes(size: usize) -> Vec<u8> {
    (0..size).map(|i| ((i * 31 + 7) % 251) as u8).collect()
}

fn host_base(req: &Recorded, w: &Conn) -> String {
    let scheme = if matches!(w, Conn::Tls(_)) { "https" } else { "http" };
    format!("{scheme}://{}", req.header("host").unwrap_or("127.0.0.1"))
}

fn route(req: &Recorded, state: &State, w: &mut Conn) -> io::Result<End> {
    let path = req.path.as_str();
    let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let num = |i: usize| seg.get(i).and_then(|s| s.parse::<usize>().ok());
    match seg[0] {
        "echo" => {
            let v = serde_json::json!({
                "method": req.method,
                "path": req.path,
                "query": req.query,
                "headers": req.headers,
                "body": String::from_utf8_lossy(&req.body),
            });
            json(w, req, 200, &v)
        }
        "redirect" => match num(1) {
            Some(0) => respond(w, req, 200, &[("Content-Type", "text/plain".into())], b"redirect done"),
            Some(n) => respond(w, req, 302, &[("Location", format!("/redirect/{}", n - 1))], b""),
            None => respond(w, req, 404, &[], b"bad redirect"),
        },
        "redirect-to" => {
            let status = req.param("status").and_then(|s| s.parse().ok()).unwrap_or(302);
            respond(w, req, status, &[("Location", req.param("url").unwrap_or_default())], b"")
        }
        "status" => {
            let code = num(1).unwrap_or(500) as u16;
            json(w, req, code, &serde_json::json!({ "message": format!("status {code}") }))
        }
        "slow" => {
            let ms = req.param("ms").and_then(|s| s.parse().ok()).unwrap_or(2000);
            std::thread::sleep(Duration::from_millis(ms));
            respond(w, req, 200, &[], b"slow")
        }
        "stall" => {
            let ms = req.param("ms").and_then(|s| s.parse().ok()).unwrap_or(5000);
            head(w, 200, &[("Content-Length", "10".into())])?;
            w.write_all(b"01")?;
            w.flush()?;
            std::thread::sleep(Duration::from_millis(ms));
            w.write_all(b"23456789")?;
            w.flush()?;
            Ok(End::Clean)
        }
        "sse" => {
            head(w, 200, &[("Content-Type", "text/event-stream".into()), ("Cache-Control", "no-cache".into()), ("Transfer-Encoding", "chunked".into())])?;
            let text = "data: {\"text\":\"\u{f8}\"}\n\n".as_bytes();
            let split = text.iter().position(|&b| b == 0xC3).unwrap() + 1; // inside the 2-byte "ø"
            let parts: Vec<&[u8]> = vec![
                b"\xEF\xBB\xBFretry: 2500\n",
                b"data: one\n\n",
                b": keep-alive comment\n\n",
                b"event: multi\nda",
                b"ta: line1\ndata: line2\r",
                b"\nid: 7\n\r\n",
                &text[..split],
                &text[split..],
                b"data: [DONE]\n\n",
            ];
            for p in parts {
                chunk(w, p)?;
                std::thread::sleep(Duration::from_millis(15));
            }
            end_chunks(w)?;
            Ok(End::Clean)
        }
        "sse-gated" => {
            head(w, 200, &[("Content-Type", "text/event-stream".into()), ("Transfer-Encoding", "chunked".into())])?;
            chunk(w, b"event: first\ndata: 1\n\n")?;
            let opened = state.wait_gate(Duration::from_secs(15));
            chunk(w, if opened { b"event: second\ndata: 2\n\n" } else { b"event: timeout\ndata: gate\n\n" })?;
            end_chunks(w)?;
            Ok(End::Clean)
        }
        "sse-reconnect" => {
            head(w, 200, &[("Content-Type", "text/event-stream".into()), ("Transfer-Encoding", "chunked".into())])?;
            match req.header("last-event-id") {
                None => {
                    chunk(w, b"retry: 50\nid: 1\ndata: a\n\n")?;
                    chunk(w, b"id: 2\ndata: b\n\n")?;
                    // Broken connection: no final chunk.
                    Ok(End::Abrupt)
                }
                Some(_) => {
                    chunk(w, b"id: 3\ndata: c\n\nevent: end\ndata: bye\n\n")?;
                    end_chunks(w)?;
                    Ok(End::Clean)
                }
            }
        }
        "files" | "flaky" | "norange" | "badrange" => {
            let Some(size) = num(1) else { return respond(w, req, 404, &[], b"size?") };
            serve_file(req, state, w, seg[0], size)
        }
        "unframed" => {
            let size = num(1).unwrap_or(1000);
            let cut = req.param("cut").and_then(|c| c.parse::<usize>().ok()).filter(|&c| c < size);
            head(w, 200, &[("Content-Type", "application/epub+zip".into()), ("ETag", format!("\"u{size}\""))])?;
            let data = file_bytes(size);
            w.write_all(&data[..cut.unwrap_or(size)])?;
            w.flush()?;
            Ok(if cut.is_some() { End::Abrupt } else { End::Clean })
        }
        "gzip" => {
            let size = num(1).unwrap_or(1000);
            let accepts = req.header("accept-encoding").unwrap_or("");
            if accepts.contains("gzip") {
                // Not real gzip: a client that asked for gzip and decodes it would fail, which
                // is the point (downloads must ask for identity).
                respond(w, req, 200, &[("Content-Encoding", "gzip".into())], b"\x1f\x8bnot really gzip")
            } else {
                respond(w, req, 200, &[], &file_bytes(size))
            }
        }
        "auth" => {
            let ok = req.header("authorization") == Some("Basic dGVzdDpzZWNyZXQ=");
            if ok {
                respond(w, req, 200, &[], b"welcome")
            } else {
                respond(w, req, 401, &[("WWW-Authenticate", "Basic realm=\"test\"".into())], b"who are you")
            }
        }
        "opds" => opds_route(req, w),
        "opds2" => {
            let base = host_base(req, w);
            respond(w, req, 200, &[("Content-Type", "application/opds+json".into())], opds2_catalog(&base).as_bytes())
        }
        "kosync" => kosync_route(req, state, w, &seg[1..]),
        "users" | "syncs" => kosync_route(req, state, w, &seg),
        _ => respond(w, req, 404, &[("Content-Type", "text/plain".into())], b"not found"),
    }
}

fn serve_file(req: &Recorded, state: &State, w: &mut Conn, kind: &str, size: usize) -> io::Result<End> {
    let data = file_bytes(size);
    let etag = format!("\"f{size}\"");
    let mut base: Vec<(&str, String)> = vec![("Content-Type", req.param("type").unwrap_or_else(|| "application/octet-stream".into())), ("Last-Modified", "Sat, 24 Oct 2026 12:00:00 GMT".into())];
    if kind != "norange" {
        base.push(("ETag", etag.clone()));
        base.push(("Accept-Ranges", "bytes".into()));
    }
    if let Some(name) = req.param("name") {
        base.push(("Content-Disposition", format!("attachment; filename=\"{name}\"")));
    }
    let range = req.header("range").and_then(|r| r.strip_prefix("bytes=")).and_then(|r| r.split('-').next()).and_then(|s| s.parse::<usize>().ok());
    let if_range_ok = req.header("if-range").is_none_or(|v| v == etag);
    match range {
        Some(start) if kind != "norange" && if_range_ok => {
            if start >= size {
                base.push(("Content-Range", format!("bytes */{size}")));
                return respond(w, req, 416, &base, b"");
            }
            let claimed = if kind == "badrange" { 0 } else { start };
            base.push(("Content-Range", format!("bytes {claimed}-{}/{size}", size - 1)));
            respond(w, req, 206, &base, &data[start..])
        }
        _ => {
            // Keyed by path and query: `/flaky/N?run=X` breaks once per X.
            if kind == "flaky" && req.method == "GET" && state.bump(&format!("{}?{}", req.path, req.query)) == 1 {
                base.push(("Content-Length", size.to_string()));
                head(w, 200, &base)?;
                w.write_all(&data[..size / 2])?;
                w.flush()?;
                std::thread::sleep(Duration::from_millis(50));
                return Ok(End::Abrupt);
            }
            respond(w, req, 200, &base, &data)
        }
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

const ATOM_NS: &str = r#"xmlns="http://www.w3.org/2005/Atom" xmlns:opds="http://opds-spec.org/2010/catalog" xmlns:dc="http://purl.org/dc/terms/" xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/""#;
const NAV: &str = "application/atom+xml;profile=opds-catalog;kind=navigation";
const ACQ: &str = "application/atom+xml;profile=opds-catalog;kind=acquisition";

fn book_entry(id: usize, title: &str, author: &str) -> String {
    let size = 3000 + id * 1000;
    format!(
        r#"<entry>
  <title>{title}</title>
  <id>urn:rinky:test:{id}</id>
  <updated>2026-10-24T12:00:00Z</updated>
  <author><name>{author}</name></author>
  <dc:language>en</dc:language>
  <summary type="text">Test book number {id}.</summary>
  <link rel="http://opds-spec.org/image" href="/files/64?name=cover{id}.png&amp;type=image/png" type="image/png"/>
  <link rel="http://opds-spec.org/image/thumbnail" href="/files/32?name=thumb{id}.png&amp;type=image/png" type="image/png"/>
  <link rel="http://opds-spec.org/acquisition/open-access" href="/files/{size}?name=book{id}.epub&amp;type=application/epub%2Bzip" type="application/epub+zip" length="{size}"/>
</entry>
"#,
        title = xml_escape(title),
        author = xml_escape(author)
    )
}

fn opds_route(req: &Recorded, w: &mut Conn) -> io::Result<End> {
    let base = host_base(req, w);
    let feed = |id: &str, title: &str, links: &str, extra: &str, entries: &str| {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<feed {ATOM_NS}>\n<id>{id}</id>\n<title>{title}</title>\n<updated>2026-10-24T12:00:00Z</updated>\n{links}{extra}{entries}</feed>\n")
    };
    let common = format!(
        "<link rel=\"start\" href=\"/opds/root.xml\" type=\"{NAV}\"/>\n<link rel=\"search\" href=\"/opds/osd.xml\" type=\"application/opensearchdescription+xml\" title=\"Search\"/>\n"
    );
    let xml = match req.path.as_str() {
        "/opds/root.xml" => {
            let entries = format!(
                "<entry><title>New books</title><id>urn:rinky:test:new</id><updated>2026-10-24T12:00:00Z</updated><content type=\"text\">Recently added</content><link rel=\"subsection\" href=\"new.xml?page=1\" type=\"{ACQ}\"/></entry>\n<entry><title>Popular</title><id>urn:rinky:test:popular</id><updated>2026-10-24T12:00:00Z</updated><link rel=\"http://opds-spec.org/sort/popular\" href=\"/opds/new.xml?page=2\" type=\"{ACQ}\"/></entry>\n"
            );
            feed("urn:rinky:test:root", "rinky test catalog", &format!("{common}<link rel=\"self\" href=\"/opds/root.xml\" type=\"{NAV}\"/>\n"), "", &entries)
        }
        "/opds/new.xml" => {
            let page: usize = req.param("page").and_then(|p| p.parse().ok()).unwrap_or(1).clamp(1, 2);
            let mut links = format!("{common}<link rel=\"self\" href=\"/opds/new.xml?page={page}\" type=\"{ACQ}\"/>\n");
            if page == 1 {
                links.push_str(&format!("<link rel=\"next\" href=\"new.xml?page=2\" type=\"{ACQ}\"/>\n"));
            } else {
                links.push_str(&format!("<link rel=\"previous\" href=\"new.xml?page=1\" type=\"{ACQ}\"/>\n<link rel=\"first\" href=\"new.xml?page=1\" type=\"{ACQ}\"/>\n"));
            }
            let extra = format!("<opensearch:totalResults>4</opensearch:totalResults>\n<opensearch:itemsPerPage>2</opensearch:itemsPerPage>\n<opensearch:startIndex>{}</opensearch:startIndex>\n", (page - 1) * 2 + 1);
            let entries = if page == 1 {
                book_entry(1, "Alpha & Omega", "Ann Author") + &book_entry(2, "Beta", "Bob Writer")
            } else {
                book_entry(3, "Gamma", "Cora Scribe") + &book_entry(4, "Delta \u{e6}\u{f8}\u{e5}", "Dag Forfatter")
            };
            feed("urn:rinky:test:new", &format!("New books, page {page}"), &links, &extra, &entries)
        }
        "/opds/osd.xml" => {
            let osd = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<OpenSearchDescription xmlns=\"http://a9.com/-/spec/opensearch/1.1/\">\n<ShortName>Test</ShortName>\n<Description>Search the test catalog</Description>\n<InputEncoding>UTF-8</InputEncoding>\n<Url type=\"text/html\" template=\"{base}/html/search?q={{searchTerms}}\"/>\n<Url type=\"{ACQ}\" template=\"{base}/opds/search?q={{searchTerms}}&amp;page={{startPage?}}\"/>\n</OpenSearchDescription>\n"
            );
            return respond(w, req, 200, &[("Content-Type", "application/opensearchdescription+xml".into())], osd.as_bytes());
        }
        "/opds/search" => {
            let q = req.param("q").unwrap_or_default();
            let entries: String = q.split_whitespace().enumerate().map(|(i, word)| book_entry(10 + i, &format!("Result for {word}"), "Search Engine")).collect();
            feed("urn:rinky:test:search", &format!("Search: {}", xml_escape(&q)), &common, "", &entries)
        }
        _ => return respond(w, req, 404, &[], b"no such feed"),
    };
    respond(w, req, 200, &[("Content-Type", format!("{ACQ};charset=utf-8"))], xml.as_bytes())
}

fn opds2_catalog(base: &str) -> String {
    serde_json::json!({
        "metadata": { "title": "rinky OPDS 2 test", "numberOfItems": 2, "itemsPerPage": 2, "currentPage": 1 },
        "links": [
            { "rel": "self", "href": "/opds2/catalog.json", "type": "application/opds+json" },
            { "rel": "next", "href": "/opds2/catalog.json?page=2", "type": "application/opds+json" },
            { "rel": "search", "href": format!("{base}/opds2/search{{?query}}"), "type": "application/opds+json", "templated": true }
        ],
        "navigation": [
            { "href": "/opds/new.xml?page=1", "title": "New books", "type": "application/atom+xml;profile=opds-catalog", "rel": "subsection" }
        ],
        "publications": [
            {
                "metadata": { "@type": "http://schema.org/Book", "title": "Epsilon", "author": [{ "name": "Eve Novelist" }], "language": "en", "identifier": "urn:rinky:test:5", "description": "An OPDS 2 book." },
                "links": [ { "rel": "http://opds-spec.org/acquisition/open-access", "href": "/files/5000?name=book5.epub", "type": "application/epub+zip" } ],
                "images": [ { "href": "/files/64?name=cover5.png", "type": "image/png" } ]
            },
            {
                "metadata": { "title": "Zeta", "author": "Zed Poet" },
                "links": [ { "rel": "http://opds-spec.org/acquisition", "href": "/files/6000?name=book6.epub", "type": "application/epub+zip" } ]
            }
        ]
    })
    .to_string()
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn kosync_route(req: &Recorded, state: &State, w: &mut Conn, seg: &[&str]) -> io::Result<End> {
    let err = |code: u32, message: &str| serde_json::json!({ "code": code, "message": message });
    if req.header("accept") != Some(KOSYNC_ACCEPT) {
        return json(w, req, 406, &err(100, "Accept must be application/vnd.koreader.v1+json"));
    }
    let body: serde_json::Value = if req.body.is_empty() {
        serde_json::Value::Null
    } else {
        if !req.header("content-type").unwrap_or("").starts_with("application/json") {
            return json(w, req, 400, &err(2003, "Invalid request"));
        }
        match serde_json::from_slice(&req.body) {
            Ok(v) => v,
            Err(_) => return json(w, req, 400, &err(2003, "Invalid request")),
        }
    };
    let authorized = || -> Option<String> {
        let user = req.header("x-auth-user")?;
        let key = req.header("x-auth-key")?;
        (state.users.lock().unwrap().get(user).map(String::as_str) == Some(key)).then(|| user.to_string())
    };
    let valid = |s: &str| !s.is_empty() && !s.contains(':');
    match (req.method.as_str(), seg) {
        ("POST", ["users", "create"]) => {
            let user = body.get("username").and_then(|v| v.as_str()).unwrap_or("");
            let pass = body.get("password").and_then(|v| v.as_str()).unwrap_or("");
            if !valid(user) || pass.is_empty() {
                return json(w, req, 403, &err(2003, "Invalid request"));
            }
            let mut users = state.users.lock().unwrap();
            if users.contains_key(user) {
                return json(w, req, 402, &err(2002, "Username is already registered."));
            }
            users.insert(user.into(), pass.into());
            json(w, req, 201, &serde_json::json!({ "username": user }))
        }
        ("GET", ["users", "auth"]) => match authorized() {
            Some(_) => json(w, req, 200, &serde_json::json!({ "authorized": "OK" })),
            None => json(w, req, 401, &err(2001, "Unauthorized")),
        },
        ("PUT", ["syncs", "progress"]) => {
            let Some(user) = authorized() else { return json(w, req, 401, &err(2001, "Unauthorized")) };
            let doc = body.get("document").and_then(|v| v.as_str()).unwrap_or("");
            if !valid(doc) {
                return json(w, req, 403, &err(2004, "Field 'document' not provided."));
            }
            let has_all = body.get("progress").is_some_and(|v| v.is_string()) && body.get("percentage").is_some_and(|v| v.is_number()) && body.get("device").is_some_and(|v| v.is_string());
            if !has_all {
                return json(w, req, 403, &err(2003, "Invalid request"));
            }
            let ts = now_secs();
            let mut stored = body.clone();
            stored["timestamp"] = ts.into();
            state.progress.lock().unwrap().insert((user, doc.to_string()), stored);
            json(w, req, 200, &serde_json::json!({ "document": doc, "timestamp": ts }))
        }
        ("GET", ["syncs", "progress", doc]) => {
            let Some(user) = authorized() else { return json(w, req, 401, &err(2001, "Unauthorized")) };
            if !valid(doc) {
                return json(w, req, 403, &err(2004, "Field 'document' not provided."));
            }
            let stored = state.progress.lock().unwrap().get(&(user, doc.to_string())).cloned();
            let mut out = serde_json::json!({});
            if let Some(s) = stored {
                for k in ["percentage", "progress", "device", "device_id", "timestamp"] {
                    if let Some(v) = s.get(k) {
                        out[k] = v.clone();
                    }
                }
                out["document"] = serde_json::Value::String(doc.to_string());
            }
            json(w, req, 200, &out)
        }
        _ => json(w, req, 404, &err(404, "not found")),
    }
}

/// Waits until `f` is true or `max` passes (test helper).
pub fn wait_for(max: Duration, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + max;
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_bytes_are_stable() {
        let b = file_bytes(300);
        assert_eq!(b[0], 7);
        assert_eq!(b[1], 38);
        assert_eq!(b[251], 7);
        assert_eq!(b.len(), 300);
    }

    #[test]
    fn plain_server_answers_and_logs() {
        let s = TestServer::start().unwrap();
        let mut c = TcpStream::connect(s.addr()).unwrap();
        c.write_all(b"GET /status/418?x=1 HTTP/1.1\r\nHost: t\r\nX-A: b\r\n\r\n").unwrap();
        let mut out = String::new();
        c.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 418 "), "{out}");
        let log = s.state.requests();
        assert_eq!(log[0].path, "/status/418");
        assert_eq!(log[0].param("x").as_deref(), Some("1"));
        assert_eq!(log[0].header("x-a"), Some("b"));
    }
}
