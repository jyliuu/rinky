//! HTTP(S) client API and the ureq implementation.
//!
//! [`HttpClient`] is the seam: plugins and features take `&dyn HttpClient`, tests pass a
//! fake or the [`crate::testserver`]. [`UreqClient`] uses ureq 3 with rustls (ring) and the
//! bundled webpki-roots CA list, so no OpenSSL or system certificates are needed on the
//! Kindle.
//!
//! Timeouts ([`Request`]): `timeout` limits each phase of the response head (DNS, connect,
//! TLS, sending, waiting for the status line); `read_timeout` limits every single read,
//! headers and body, so a stream (SSE, a long download) runs as long as data keeps coming;
//! `total_timeout` optionally limits the whole exchange including the body.
//!
//! Redirects are followed by [`UreqClient`] itself (not by ureq), so it can resolve
//! relative `Location`s, turn POST into GET on 301/302/303, keep the method on 307/308,
//! drop credentials when the origin changes, and report the final URL in
//! [`Response::url`] (OPDS resolves relative links against it).

use crate::{NetError, Result};
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

/// HTTP method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// GET.
    Get,
    /// HEAD.
    Head,
    /// POST.
    Post,
    /// PUT.
    Put,
    /// PATCH.
    Patch,
    /// DELETE.
    Delete,
}

impl Method {
    /// The method name (`GET`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

/// Default limit for each phase of the response head.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Default limit for one read.
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Default number of redirects followed.
pub const DEFAULT_MAX_REDIRECTS: u32 = 10;

/// A request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Method.
    pub method: Method,
    /// Absolute URL.
    pub url: String,
    /// Header name/value pairs.
    pub headers: Vec<(String, String)>,
    /// Body (empty for none).
    pub body: Vec<u8>,
    /// Limit for each phase of the response head: DNS, connect, TLS, sending the request,
    /// receiving the status line and headers.
    pub timeout: Duration,
    /// Limit for each single read (headers and body). A stream that keeps sending data
    /// never hits it; a stalled one fails with [`NetError::Timeout`].
    pub read_timeout: Duration,
    /// Limit for the whole exchange including the body; `None` = no limit.
    pub total_timeout: Option<Duration>,
    /// Redirects to follow; 0 returns 3xx responses to the caller.
    pub max_redirects: u32,
}

impl Request {
    /// A request with the default timeouts (30 s head phases, 30 s per read, no total limit)
    /// and up to 10 redirects.
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Request {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            read_timeout: DEFAULT_READ_TIMEOUT,
            total_timeout: None,
            max_redirects: DEFAULT_MAX_REDIRECTS,
        }
    }

    /// A GET.
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(Method::Get, url)
    }

    /// A HEAD.
    pub fn head(url: impl Into<String>) -> Self {
        Self::new(Method::Head, url)
    }

    /// A POST with a JSON body.
    pub fn post_json(url: impl Into<String>, json: &serde_json::Value) -> Self {
        Self::new(Method::Post, url).json_body(json)
    }

    /// A PUT with a JSON body.
    pub fn put_json(url: impl Into<String>, json: &serde_json::Value) -> Self {
        Self::new(Method::Put, url).json_body(json)
    }

    /// Sets a JSON body and `Content-Type: application/json`.
    pub fn json_body(mut self, json: &serde_json::Value) -> Self {
        self.body = json.to_string().into_bytes();
        self.set_header("Content-Type", "application/json")
    }

    /// Sets a body with a content type.
    pub fn body(mut self, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self.set_header("Content-Type", content_type)
    }

    /// Adds a header (a second header with the same name is sent too).
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Replaces every header named `name` (case-insensitive) with one value.
    pub fn set_header(mut self, name: &str, value: &str) -> Self {
        self.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        self.header(name, value)
    }

    /// First header named `name` (case-insensitive).
    pub fn header_value(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// `Authorization: Basic ...` (OPDS servers such as Calibre-Web).
    pub fn basic_auth(self, user: &str, password: &str) -> Self {
        self.set_header("Authorization", &basic_auth_value(user, password))
    }

    /// Sets the head phase timeout ([`Request::timeout`]).
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    /// Sets the per-read timeout ([`Request::read_timeout`]).
    pub fn read_timeout(mut self, t: Duration) -> Self {
        self.read_timeout = t;
        self
    }

    /// Sets the whole-exchange timeout ([`Request::total_timeout`]).
    pub fn total_timeout(mut self, t: Option<Duration>) -> Self {
        self.total_timeout = t;
        self
    }

    /// Sets how many redirects are followed ([`Request::max_redirects`]).
    pub fn max_redirects(mut self, n: u32) -> Self {
        self.max_redirects = n;
        self
    }
}

/// A response with a streaming body.
pub struct Response {
    /// Status code.
    pub status: u16,
    /// Header name/value pairs (names lowercase).
    pub headers: Vec<(String, String)>,
    /// Body stream (read it to the end, or drop it).
    pub body: Box<dyn Read + Send>,
    /// The URL that produced this response, after redirects.
    pub url: String,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response").field("status", &self.status).field("url", &self.url).field("headers", &self.headers).finish_non_exhaustive()
    }
}

impl Response {
    /// A response (for fakes in tests); header names are lowercased.
    pub fn new(status: u16, headers: Vec<(String, String)>, body: impl Read + Send + 'static, url: impl Into<String>) -> Self {
        let headers = headers.into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v)).collect();
        Response { status, headers, body: Box::new(body), url: url.into() }
    }

    /// First header named `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// `Content-Length`, when present and valid.
    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length").and_then(|v| v.trim().parse().ok())
    }

    /// The MIME type without parameters, lowercase (`application/atom+xml`).
    pub fn mime(&self) -> String {
        self.header("content-type").map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase()).unwrap_or_default()
    }

    /// Reads the whole body (at most `limit` bytes; `usize::MAX` means no practical limit).
    pub fn bytes(mut self, limit: usize) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        // One byte more than the limit tells "exactly at the limit" from "too large".
        (&mut self.body).take((limit as u64).saturating_add(1)).read_to_end(&mut out).map_err(NetError::from_read)?;
        if out.len() > limit {
            return Err(NetError::TooLarge(limit));
        }
        Ok(out)
    }

    /// Reads the body as UTF-8 text (lossy), at most `limit` bytes.
    pub fn text(self, limit: usize) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.bytes(limit)?).into_owned())
    }

    /// Reads and parses a JSON body (at most `limit` bytes).
    pub fn json<T: serde::de::DeserializeOwned>(self, limit: usize) -> Result<T> {
        let bytes = self.bytes(limit)?;
        serde_json::from_slice(&bytes).map_err(|e| NetError::Format(format!("json: {e}")))
    }

    /// True for 2xx.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// `Ok(self)` for 2xx; otherwise an error carrying the status and the start of the body
    /// (401 becomes [`NetError::Unauthorized`]).
    pub fn error_for_status(self) -> Result<Self> {
        if self.ok() {
            return Ok(self);
        }
        let status = self.status;
        Err(status_error(status, &self.text(4096).unwrap_or_default()))
    }
}

/// The error for an answer with a failing `status` and body `text`: the message is the
/// body's JSON `message` (or `error`) field, else the start of the body; 401 becomes
/// [`NetError::Unauthorized`].
pub fn status_error(status: u16, text: &str) -> NetError {
    let message = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("message").or(v.get("error")).and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| text.trim().chars().take(200).collect());
    if status == 401 {
        NetError::Unauthorized(message)
    } else {
        NetError::Server { status, message }
    }
}

/// First header named `name` (case-insensitive).
fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// Sends requests.
pub trait HttpClient {
    /// Sends `req`. Non-2xx statuses are returned as responses, not errors.
    fn send(&self, req: Request) -> Result<Response>;
}

impl<T: HttpClient + ?Sized> HttpClient for &T {
    fn send(&self, req: Request) -> Result<Response> {
        (**self).send(req)
    }
}

impl<T: HttpClient + ?Sized> HttpClient for Box<T> {
    fn send(&self, req: Request) -> Result<Response> {
        (**self).send(req)
    }
}

impl<T: HttpClient + ?Sized> HttpClient for Arc<T> {
    fn send(&self, req: Request) -> Result<Response> {
        (**self).send(req)
    }
}

/// Which certificate authorities a [`UreqClient`] trusts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TlsRoots {
    /// Mozilla's CA list compiled into the binary (webpki-roots). The default.
    #[default]
    WebPki,
    /// Only these DER certificates (a self-hosted server's own CA, or a test CA).
    /// webpki-roots holds trust anchors, not certificates, so the two sets cannot be
    /// merged into one ureq agent; a client for such a server uses only its CA.
    Custom(Vec<Vec<u8>>),
}

impl TlsRoots {
    /// [`TlsRoots::Custom`] from every certificate in a PEM file.
    pub fn from_pem(pem: &[u8]) -> Result<Self> {
        let mut certs = Vec::new();
        for item in ureq::tls::parse_pem(pem) {
            match item.map_err(|e| NetError::Tls(format!("pem: {e}")))? {
                ureq::tls::PemItem::Certificate(c) => certs.push(c.der().to_vec()),
                _ => continue,
            }
        }
        if certs.is_empty() {
            return Err(NetError::Tls("no certificate in PEM".into()));
        }
        Ok(TlsRoots::Custom(certs))
    }

    /// Number of trusted roots.
    pub fn len(&self) -> usize {
        match self {
            TlsRoots::WebPki => webpki_roots::TLS_SERVER_ROOTS.len(),
            TlsRoots::Custom(c) => c.len(),
        }
    }

    /// True when no root is trusted (an empty custom list).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Settings for a [`UreqClient`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    /// `User-Agent` header (a request's own `User-Agent` header wins).
    pub user_agent: String,
    /// Trusted CAs.
    pub roots: TlsRoots,
    /// Refuse plain `http://` URLs (also as redirect targets).
    pub https_only: bool,
}

/// The default user agent: `rinky/<version> (Kindle)`.
pub fn default_user_agent() -> String {
    format!("rinky/{} (Kindle)", env!("CARGO_PKG_VERSION"))
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig { user_agent: default_user_agent(), roots: TlsRoots::WebPki, https_only: false }
    }
}

/// ureq-based client. Cheap to clone (the connection pool is shared).
#[derive(Clone, Debug)]
pub struct UreqClient {
    agent: ureq::Agent,
    https_only: bool,
    /// Sent as `User-Agent` unless a request sets its own.
    pub user_agent: String,
}

impl Default for UreqClient {
    fn default() -> Self {
        Self::with_config(ClientConfig::default())
    }
}

impl UreqClient {
    /// A client with the default settings (webpki roots).
    pub fn new() -> Self {
        Self::default()
    }

    /// A client with these settings.
    pub fn with_config(cfg: ClientConfig) -> Self {
        let roots = match &cfg.roots {
            TlsRoots::WebPki => ureq::tls::RootCerts::WebPki,
            TlsRoots::Custom(ders) => {
                let certs: Vec<_> = ders.iter().map(|d| ureq::tls::Certificate::from_der(d).to_owned()).collect();
                ureq::tls::RootCerts::new_with_certs(&certs)
            }
        };
        let tls = ureq::tls::TlsConfig::builder().provider(ureq::tls::TlsProvider::Rustls).root_certs(roots).build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(ureq::config::AutoHeaderValue::None)
            .https_only(cfg.https_only)
            .tls_config(tls)
            .build()
            .into();
        UreqClient { agent, https_only: cfg.https_only, user_agent: cfg.user_agent }
    }

    /// A client that trusts only the CA certificates in `pem`.
    pub fn with_ca_pem(pem: &[u8]) -> Result<Self> {
        Ok(Self::with_config(ClientConfig { roots: TlsRoots::from_pem(pem)?, ..ClientConfig::default() }))
    }

    fn send_once(&self, req: &Request) -> Result<Response> {
        let url = req.url.as_str();
        let resp = match req.method {
            Method::Get => self.configure(self.agent.get(url), req).call(),
            Method::Head => self.configure(self.agent.head(url), req).call(),
            Method::Delete => self.configure(self.agent.delete(url), req).call(),
            Method::Post => self.configure(self.agent.post(url), req).send(&req.body[..]),
            Method::Put => self.configure(self.agent.put(url), req).send(&req.body[..]),
            Method::Patch => self.configure(self.agent.patch(url), req).send(&req.body[..]),
        }
        .map_err(map_ureq)?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_ascii_lowercase(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect();
        let body = Box::new(resp.into_body().into_reader());
        Ok(Response { status, headers, body, url: req.url.clone() })
    }

    /// Timeouts, user agent and headers for one request.
    fn configure<B>(&self, b: ureq::RequestBuilder<B>, req: &Request) -> ureq::RequestBuilder<B> {
        let t = Some(req.timeout);
        let mut b = b
            .config()
            .timeout_resolve(t)
            .timeout_connect(t)
            .timeout_send_request(t)
            .timeout_send_body(t)
            .timeout_recv_response(t)
            .timeout_per_read(Some(req.read_timeout))
            .timeout_global(req.total_timeout)
            .build();
        if req.header_value("user-agent").is_none() {
            b = b.header("User-Agent", self.user_agent.as_str());
        }
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        b
    }
}

/// Maps a ureq error to [`NetError`].
pub fn map_ureq(e: ureq::Error) -> NetError {
    use std::io::ErrorKind::{TimedOut, WouldBlock};
    // Other errors are TLS failures when their text says so (ureq wraps some of them).
    let (msg, http_prefix) = match e {
        ureq::Error::Timeout(_) => return NetError::Timeout,
        ureq::Error::Tls(m) => return NetError::Tls(m.to_string()),
        ureq::Error::Pem(p) => return NetError::Tls(format!("pem: {p}")),
        ureq::Error::Rustls(r) => return NetError::Tls(r.to_string()),
        ureq::Error::Io(io) if matches!(io.kind(), TimedOut | WouldBlock) => return NetError::Timeout,
        ureq::Error::Io(io) => (io.to_string(), "io: "),
        other => (other.to_string(), ""),
    };
    let m = msg.to_ascii_lowercase();
    if ["certificate", "unknownissuer", "handshake", "tls"].iter().any(|w| m.contains(w)) {
        NetError::Tls(msg)
    } else {
        NetError::Http(format!("{http_prefix}{msg}"))
    }
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Header names that carry credentials; dropped when a redirect changes the origin.
pub fn is_sensitive_header(name: &str) -> bool {
    // `auth` covers Authorization and Proxy-Authorization.
    let n = name.to_ascii_lowercase();
    n == "cookie" || ["auth", "key", "token", "secret"].iter().any(|w| n.contains(w))
}

/// Applies one redirect to `req` (RFC 9110 section 15.4 plus browser practice): resolves
/// `location` against the current URL, turns POST into GET on 301/302 and every method
/// but HEAD into GET on 303 (dropping the body), keeps method and body on 307/308, and
/// drops credential headers when the origin changes.
pub fn apply_redirect(req: &mut Request, status: u16, location: &str) {
    let next = crate::url::resolve(&req.url, location);
    if !crate::url::same_origin(&req.url, &next) {
        req.headers.retain(|(k, _)| !is_sensitive_header(k));
    }
    let to_get = match status {
        303 => req.method != Method::Head,
        301 | 302 => req.method == Method::Post,
        _ => false,
    };
    if to_get {
        req.method = Method::Get;
        req.body.clear();
        req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type") && !k.eq_ignore_ascii_case("content-length"));
    }
    req.url = next;
}

impl HttpClient for UreqClient {
    fn send(&self, mut req: Request) -> Result<Response> {
        let mut hops = 0;
        loop {
            if self.https_only && !req.url.to_ascii_lowercase().starts_with("https://") {
                return Err(NetError::Http(format!("https only: refusing {}", req.url)));
            }
            let resp = self.send_once(&req)?;
            if req.max_redirects == 0 || !is_redirect(resp.status) {
                return Ok(resp);
            }
            let Some(location) = resp.header("location").map(str::to_string) else {
                return Ok(resp);
            };
            if hops >= req.max_redirects {
                return Err(NetError::Http(format!("too many redirects (more than {})", req.max_redirects)));
            }
            hops += 1;
            let status = resp.status;
            drop(resp);
            log::debug!("redirect {hops} ({status}): {} -> {location}", req.url);
            apply_redirect(&mut req, status, &location);
        }
    }
}

/// The `Authorization` value for HTTP Basic auth: `Basic base64(user:password)`.
pub fn basic_auth_value(user: &str, password: &str) -> String {
    format!("Basic {}", base64(format!("{user}:{password}").as_bytes()))
}

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{State, TestServer, TEST_CA_PEM};
    use std::time::Instant;

    fn echo(resp: Response) -> serde_json::Value {
        resp.json(1 << 16).unwrap()
    }

    fn headers_named<'a>(v: &'a serde_json::Value, name: &str) -> Vec<&'a str> {
        v["headers"].as_array().unwrap().iter().filter(|kv| kv[0] == name).map(|kv| kv[1].as_str().unwrap()).collect()
    }

    #[test]
    fn request_builders() {
        let r = Request::post_json("http://x/", &serde_json::json!({"a": 1})).header("X-A", "b");
        assert_eq!(r.method, Method::Post);
        assert_eq!(r.body, br#"{"a":1}"#);
        assert_eq!(r.headers.len(), 2);
        let r = r.set_header("content-type", "text/plain");
        assert_eq!(r.header_value("Content-Type"), Some("text/plain"));
        assert_eq!(r.headers.len(), 2);
        let r = Request::get("http://x/").basic_auth("test", "secret");
        assert_eq!(r.header_value("authorization"), Some("Basic dGVzdDpzZWNyZXQ="));
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        let r = Request::get("u");
        assert_eq!((r.timeout, r.read_timeout, r.total_timeout, r.max_redirects), (DEFAULT_TIMEOUT, DEFAULT_READ_TIMEOUT, None, 10));
        assert_eq!(Method::Patch.as_str(), "PATCH");
    }

    #[test]
    fn one_user_agent_and_custom_headers() {
        let s = TestServer::start().unwrap();
        let c = UreqClient::new();
        let v = echo(c.send(Request::get(s.url("/echo?x=1")).header("X-Hello", "1")).unwrap());
        assert_eq!(headers_named(&v, "user-agent"), vec![default_user_agent()]);
        assert_eq!(headers_named(&v, "x-hello"), vec!["1"]);
        assert_eq!(v["query"], "x=1");
        // A request's own User-Agent replaces the client's, without a duplicate.
        let v = echo(c.send(Request::get(s.url("/echo")).header("User-Agent", "plugin/1")).unwrap());
        assert_eq!(headers_named(&v, "user-agent"), vec!["plugin/1"]);
        let mut c2 = UreqClient::with_config(ClientConfig { user_agent: "custom/2".into(), ..Default::default() });
        let v = echo(c2.send(Request::get(s.url("/echo"))).unwrap());
        assert_eq!(headers_named(&v, "user-agent"), vec!["custom/2"]);
        c2.user_agent = "changed/3".into();
        let v = echo(c2.send(Request::get(s.url("/echo"))).unwrap());
        assert_eq!(headers_named(&v, "user-agent"), vec!["changed/3"]);
    }

    #[test]
    fn status_headers_and_bodies() {
        let s = TestServer::start().unwrap();
        let c = UreqClient::new();
        let resp = c.send(Request::get(s.url("/status/404"))).unwrap();
        assert_eq!(resp.status, 404);
        assert!(!resp.ok());
        assert_eq!(resp.mime(), "application/json");
        assert!(matches!(resp.error_for_status(), Err(NetError::Server { status: 404, message }) if message == "status 404"));
        let resp = c.send(Request::get(s.url("/status/401"))).unwrap();
        assert!(matches!(resp.error_for_status(), Err(NetError::Unauthorized(_))));
        let resp = c.send(Request::get(s.url("/files/10"))).unwrap();
        assert_eq!(resp.content_length(), Some(10));
        assert_eq!(resp.header("ETag"), Some("\"f10\""));
        assert!(matches!(resp.bytes(4), Err(NetError::TooLarge(4))));
        let resp = c.send(Request::head(s.url("/files/10"))).unwrap();
        assert_eq!(resp.content_length(), Some(10));
        assert_eq!(resp.bytes(100).unwrap(), b"");
        let v = echo(c.send(Request::put_json(s.url("/echo"), &serde_json::json!({"k": "v"}))).unwrap());
        assert_eq!(v["method"], "PUT");
        assert_eq!(v["body"], r#"{"k":"v"}"#);
        let resp = c.send(Request::get(s.url("/auth/basic")).basic_auth("test", "secret")).unwrap();
        assert_eq!(resp.text(100).unwrap(), "welcome");
        assert!(matches!(c.send(Request::get("http://127.0.0.1:1/").timeout(Duration::from_secs(2))), Err(NetError::Http(_))));
    }

    #[test]
    fn redirects_relative_limits_and_methods() {
        let s = TestServer::start().unwrap();
        let c = UreqClient::new();
        let resp = c.send(Request::get(s.url("/redirect/4"))).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.url, s.url("/redirect/0"));
        assert_eq!(resp.text(100).unwrap(), "redirect done");
        assert!(matches!(c.send(Request::get(s.url("/redirect/4")).max_redirects(3)), Err(NetError::Http(m)) if m.contains("too many redirects")));
        let resp = c.send(Request::get(s.url("/redirect/4")).max_redirects(0)).unwrap();
        assert_eq!(resp.status, 302);
        assert_eq!(resp.header("location"), Some("/redirect/3"));
        // 303 turns POST into GET without the body; 307 keeps both.
        let to = crate::url::encode_component("/echo");
        let v = echo(c.send(Request::post_json(s.url(&format!("/redirect-to?status=303&url={to}")), &serde_json::json!({"a": 1}))).unwrap());
        assert_eq!((v["method"].as_str(), v["body"].as_str()), (Some("GET"), Some("")));
        assert!(headers_named(&v, "content-type").is_empty());
        let v = echo(c.send(Request::post_json(s.url(&format!("/redirect-to?status=307&url={to}")), &serde_json::json!({"a": 1}))).unwrap());
        assert_eq!((v["method"].as_str(), v["body"].as_str()), (Some("POST"), Some(r#"{"a":1}"#)));
        let v = echo(c.send(Request::put_json(s.url(&format!("/redirect-to?status=302&url={to}")), &serde_json::json!({}))).unwrap());
        assert_eq!(v["method"], "PUT", "302 keeps methods other than POST");
    }

    #[test]
    fn credentials_do_not_follow_a_redirect_to_another_origin() {
        let state = std::sync::Arc::new(State::default());
        let a = TestServer::bind("127.0.0.1:0", false, state.clone()).unwrap();
        let b = TestServer::bind("127.0.0.1:0", false, state).unwrap();
        let c = UreqClient::new();
        let same = crate::url::encode_component(&a.url("/echo"));
        let v = echo(c.send(Request::get(a.url(&format!("/redirect-to?url={same}"))).basic_auth("u", "p").header("x-auth-key", "k").header("X-Keep", "1")).unwrap());
        assert_eq!(headers_named(&v, "authorization").len(), 1, "same origin keeps credentials");
        let other = crate::url::encode_component(&b.url("/echo"));
        let v = echo(c.send(Request::get(a.url(&format!("/redirect-to?url={other}"))).basic_auth("u", "p").header("x-auth-key", "k").header("X-Keep", "1")).unwrap());
        assert!(headers_named(&v, "authorization").is_empty());
        assert!(headers_named(&v, "x-auth-key").is_empty());
        assert_eq!(headers_named(&v, "x-keep"), vec!["1"]);
    }

    #[test]
    fn head_and_read_timeouts() {
        let s = TestServer::start().unwrap();
        let c = UreqClient::new();
        let t = Instant::now();
        let r = c.send(Request::get(s.url("/slow?ms=3000")).timeout(Duration::from_millis(300)));
        assert!(matches!(r, Err(NetError::Timeout)), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        // The head arrives at once; the body stalls for 3 s.
        let t = Instant::now();
        let resp = c.send(Request::get(s.url("/stall?ms=3000")).read_timeout(Duration::from_millis(300))).unwrap();
        assert!(matches!(resp.bytes(100), Err(NetError::Timeout)));
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
        // A total limit cuts a body that trickles.
        let r = c.send(Request::get(s.url("/stall?ms=3000")).total_timeout(Some(Duration::from_millis(500)))).and_then(|r| r.bytes(100));
        assert!(matches!(r, Err(NetError::Timeout)), "{r:?}");
        // Without limits below the stall the body completes.
        let resp = c.send(Request::get(s.url("/stall?ms=200"))).unwrap();
        assert_eq!(resp.bytes(100).unwrap(), b"0123456789");
    }

    #[test]
    fn tls_with_the_test_ca_and_refusal_without_it() {
        let s = TestServer::start_tls().unwrap();
        assert!(s.base().starts_with("https://127.0.0.1:"));
        let trusted = UreqClient::with_ca_pem(TEST_CA_PEM).unwrap();
        let resp = trusted.send(Request::get(s.url("/redirect/1"))).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.text(100).unwrap(), "redirect done");
        let r = UreqClient::new().send(Request::get(s.url("/echo")));
        assert!(matches!(&r, Err(NetError::Tls(m)) if m.contains("UnknownIssuer")), "{r:?}");
        assert!(TlsRoots::WebPki.len() > 100);
        assert!(matches!(TlsRoots::from_pem(b"not pem"), Err(NetError::Tls(_))));
        assert_eq!(TlsRoots::from_pem(TEST_CA_PEM).unwrap().len(), 1);
        let only = UreqClient::with_config(ClientConfig { https_only: true, roots: TlsRoots::from_pem(TEST_CA_PEM).unwrap(), ..Default::default() });
        assert!(only.send(Request::get(s.url("/echo"))).is_ok());
        assert!(matches!(only.send(Request::get("http://127.0.0.1:1/")), Err(NetError::Http(m)) if m.contains("https only")));
    }

    /// Real CA chain through the compiled-in webpki roots. Needs the internet:
    /// `cargo test -p ink-net -- --ignored internet`.
    #[test]
    #[ignore]
    fn internet_gutenberg_over_webpki_roots() {
        let resp = UreqClient::new().send(Request::get("https://www.gutenberg.org/ebooks.opds/").header("Accept", crate::opds::OPDS_ACCEPT)).unwrap();
        assert_eq!(resp.status, 200);
        assert!(resp.url.starts_with("https://www.gutenberg.org/"));
        let base = resp.url.clone();
        let feed = crate::opds::parse_any(&resp.bytes(1 << 20).unwrap(), "application/atom+xml", &base).unwrap();
        assert!(!feed.entries.is_empty());
        eprintln!("gutenberg over webpki: {} entries in '{}'", feed.entries.len(), feed.title);
    }

    #[test]
    fn redirect_rules_unit() {
        let mut r = Request::post_json("https://a.example/x/y", &serde_json::json!({})).header("Authorization", "Bearer t").header("X-Api-Key", "k").header("Accept", "a");
        apply_redirect(&mut r, 301, "z?q=1");
        assert_eq!(r.url, "https://a.example/x/z?q=1");
        assert_eq!(r.method, Method::Get);
        assert!(r.body.is_empty());
        assert!(r.header_value("authorization").is_some());
        apply_redirect(&mut r, 307, "https://b.example/");
        assert_eq!(r.header_value("authorization"), None);
        assert_eq!(r.header_value("x-api-key"), None);
        assert_eq!(r.header_value("accept"), Some("a"));
        let mut h = Request::head("http://a/");
        apply_redirect(&mut h, 303, "/b");
        assert_eq!(h.method, Method::Head);
    }
}
