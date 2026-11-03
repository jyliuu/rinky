//! Streaming HTTP(S) requests for plugins, run in worker threads.
//!
//! A plugin starts a request with [`crate::HostCtx::start_request`] (Lua:
//! `ink.http.request`) and gets an id back at once. A worker thread sends the request and
//! reads the body in pieces; the app calls [`NetWorker::poll`] from its event loop and
//! hands every [`HttpEvent`] to `Plugin::on_http` of the plugin that started it:
//!
//! 1. `Response { status, headers }` once the headers are in;
//! 2. `Chunk(bytes)` per read ([`StreamMode::Chunks`]) or `Sse(event)` per server-sent
//!    event ([`StreamMode::Sse`]); nothing for [`StreamMode::Whole`];
//! 3. `Done { body }` (the whole body for [`StreamMode::Whole`]) or `Error(message)`.
//!
//! The reader never waits for the network: it keeps turning pages while requests run.

use crate::{PluginError, Result};
use ink_net::connectivity::NetActivity;
use ink_net::{CancelToken, HttpClient, Request, SseEvent, SseParser};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// How the body of a request is delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamMode {
    /// Collect the body (at most `limit` bytes) and deliver it with `Done`.
    Whole {
        /// Byte limit; larger bodies end with `Error`.
        limit: usize,
    },
    /// Deliver each piece read as a `Chunk`.
    Chunks,
    /// Parse `text/event-stream` and deliver each event as `Sse`.
    Sse,
}

/// Default limit for [`StreamMode::Whole`] bodies.
pub const WHOLE_LIMIT: usize = 8 << 20;
/// Limit for the total bytes of a streamed body.
pub const STREAM_LIMIT: usize = 64 << 20;
/// Requests in flight at once (all plugins).
pub const MAX_IN_FLIGHT: usize = 8;

/// One step of a running request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpEvent {
    /// Status and headers (names lowercase).
    Response {
        /// Status code.
        status: u16,
        /// Header name/value pairs.
        headers: Vec<(String, String)>,
    },
    /// A piece of the body ([`StreamMode::Chunks`]).
    Chunk(Vec<u8>),
    /// A server-sent event ([`StreamMode::Sse`]).
    Sse(SseEvent),
    /// The request finished. `body` is set for [`StreamMode::Whole`].
    Done {
        /// The collected body.
        body: Option<Vec<u8>>,
    },
    /// The request failed (connection, timeout, size limit).
    Error(String),
}

impl HttpEvent {
    /// True for `Done` and `Error` (no more events follow).
    pub fn is_last(&self) -> bool {
        matches!(self, HttpEvent::Done { .. } | HttpEvent::Error(_))
    }
}

/// Starts and cancels requests for one plugin (what [`crate::HostCtx`] holds).
pub trait NetHost {
    /// Starts `req`; returns the request id.
    fn start(&mut self, req: Request, mode: StreamMode) -> Result<u32>;
    /// Cancels a running request (no further events are delivered).
    fn cancel(&mut self, id: u32);
}

/// Runs requests of all plugins in worker threads.
pub struct NetWorker {
    client: Arc<dyn HttpClient + Send + Sync>,
    tx: Sender<(u32, HttpEvent)>,
    rx: Receiver<(u32, HttpEvent)>,
    next_id: u32,
    /// id -> (plugin, cancel token)
    running: HashMap<u32, (String, CancelToken)>,
    cancelled: Arc<Mutex<HashSet<u32>>>,
    activity: NetActivity,
}

impl NetWorker {
    /// A worker that sends requests with `client`.
    pub fn new(client: Arc<dyn HttpClient + Send + Sync>) -> Self {
        let (tx, rx) = channel();
        NetWorker { client, tx, rx, next_id: 1, running: HashMap::new(), cancelled: Arc::new(Mutex::new(HashSet::new())), activity: NetActivity::new() }
    }

    /// The tracker every request registers with while its thread runs (shared: clones
    /// see the same count). The app asks it before an idle sleep
    /// ([`NetActivity::may_auto_suspend`]) and on `readyToSuspend`.
    pub fn activity(&self) -> &NetActivity {
        &self.activity
    }

    /// Uses `activity` (for example the app's shared tracker) for requests started from
    /// now on.
    pub fn set_activity(&mut self, activity: NetActivity) {
        self.activity = activity;
    }

    /// Cancels every running request (before the device sleeps). Returns
    /// `(plugin, request id)` of each, so the caller can tell the plugins.
    pub fn interrupt_all(&mut self) -> Vec<(String, u32)> {
        let mut ids: Vec<(String, u32)> = self.running.iter().map(|(id, (p, _))| (p.clone(), *id)).collect();
        ids.sort_by_key(|(_, id)| *id);
        for (p, id) in &ids {
            self.cancel(p, *id);
        }
        ids
    }

    /// Number of requests not finished yet.
    pub fn in_flight(&self) -> usize {
        self.running.len()
    }

    /// Request runner for `plugin`.
    pub fn for_plugin<'a>(&'a mut self, plugin: &'a str) -> PluginJobs<'a> {
        PluginJobs { worker: self, plugin }
    }

    /// Starts `req` for `plugin`.
    pub fn start(&mut self, plugin: &str, req: Request, mode: StreamMode) -> Result<u32> {
        if self.running.len() >= MAX_IN_FLIGHT {
            return Err(PluginError::Failed(format!("too many network requests ({MAX_IN_FLIGHT} running)")));
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let cancel = CancelToken::new();
        let (client, tx, token, activity) = (self.client.clone(), self.tx.clone(), cancel.clone(), self.activity.clone());
        std::thread::Builder::new()
            .name(format!("plugin-http-{id}"))
            .stack_size(256 * 1024)
            .spawn(move || {
                // Counted as a running transfer until the thread ends.
                let _busy = activity.begin(&token, None);
                run_request_until(client.as_ref(), req, mode, &|| token.is_cancelled(), &|ev| tx.send((id, ev)).is_ok())
            })
            .map_err(|e| PluginError::Failed(format!("cannot start a network thread: {e}")))?;
        self.running.insert(id, (plugin.to_string(), cancel));
        Ok(id)
    }

    /// Cancels request `id` if it belongs to `plugin`.
    pub fn cancel(&mut self, plugin: &str, id: u32) {
        if self.running.get(&id).is_some_and(|(p, _)| p == plugin) {
            if let Some((_, token)) = self.running.remove(&id) {
                token.cancel();
            }
            if let Ok(mut c) = self.cancelled.lock() {
                c.insert(id);
            }
        }
    }

    /// Cancels every request of `plugin` (it was disabled or unloaded).
    pub fn cancel_plugin(&mut self, plugin: &str) {
        let ids: Vec<u32> = self.running.iter().filter(|(_, (p, _))| p == plugin).map(|(id, _)| *id).collect();
        for id in ids {
            self.cancel(plugin, id);
        }
    }

    /// Events that arrived since the last call: `(plugin, request id, event)`, in order.
    pub fn poll(&mut self) -> Vec<(String, u32, HttpEvent)> {
        let mut out = Vec::new();
        while let Ok((id, ev)) = self.rx.try_recv() {
            let Some((plugin, _)) = self.running.get(&id) else {
                // Cancelled: drop the rest; forget the id once the thread is done.
                if ev.is_last() {
                    if let Ok(mut c) = self.cancelled.lock() {
                        c.remove(&id);
                    }
                }
                continue;
            };
            let plugin = plugin.clone();
            if ev.is_last() {
                self.running.remove(&id);
            }
            out.push((plugin, id, ev));
        }
        out
    }

    /// Waits up to `timeout` for the next event (tests and command-line tools).
    pub fn wait(&mut self, timeout: std::time::Duration) -> Vec<(String, u32, HttpEvent)> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let got = self.poll();
            if !got.is_empty() || std::time::Instant::now() >= deadline || self.running.is_empty() {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

/// [`NetHost`] for one plugin over a [`NetWorker`].
pub struct PluginJobs<'a> {
    worker: &'a mut NetWorker,
    plugin: &'a str,
}

impl NetHost for PluginJobs<'_> {
    fn start(&mut self, req: Request, mode: StreamMode) -> Result<u32> {
        self.worker.start(self.plugin, req, mode)
    }
    fn cancel(&mut self, id: u32) {
        self.worker.cancel(self.plugin, id);
    }
}

/// Sends `req` and reports its events through `emit` (which returns false when nobody
/// listens any more). Runs in a worker thread; public for tests and tools.
pub fn run_request(client: &dyn HttpClient, req: Request, mode: StreamMode, cancel: &AtomicBool, emit: &dyn Fn(HttpEvent) -> bool) {
    run_request_until(client, req, mode, &|| cancel.load(Ordering::Relaxed), emit)
}

/// [`run_request`] with a cancel check function.
pub fn run_request_until(client: &dyn HttpClient, req: Request, mode: StreamMode, cancelled: &dyn Fn() -> bool, emit: &dyn Fn(HttpEvent) -> bool) {
    let mut resp = match client.send(req) {
        Ok(r) => r,
        Err(e) => {
            emit(HttpEvent::Error(e.to_string()));
            return;
        }
    };
    if !emit(HttpEvent::Response { status: resp.status, headers: resp.headers.clone() }) {
        return;
    }
    let mut buf = vec![0u8; 8192];
    let mut total = 0usize;
    let mut body = Vec::new();
    let mut sse = SseParser::new();
    let limit = match mode {
        StreamMode::Whole { limit } => limit,
        _ => STREAM_LIMIT,
    };
    loop {
        if cancelled() {
            return;
        }
        let n = match resp.body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                emit(HttpEvent::Error(format!("reading the response: {e}")));
                return;
            }
        };
        total += n;
        if total > limit {
            emit(HttpEvent::Error(format!("response larger than {limit} bytes")));
            return;
        }
        let ok = match mode {
            StreamMode::Whole { .. } => {
                body.extend_from_slice(&buf[..n]);
                true
            }
            StreamMode::Chunks => emit(HttpEvent::Chunk(buf[..n].to_vec())),
            StreamMode::Sse => sse.feed(&buf[..n]).into_iter().all(|e| emit(HttpEvent::Sse(e))),
        };
        if !ok {
            return;
        }
    }
    // A last event without the closing blank line is dropped (WHATWG rule).
    sse.finish();
    let body = matches!(mode, StreamMode::Whole { .. }).then_some(body);
    emit(HttpEvent::Done { body });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ink_net::{NetError, Response};
    use std::time::Duration;

    /// Serves canned bodies by URL, in small pieces.
    pub struct FakeClient(pub HashMap<String, (u16, Vec<u8>)>);

    struct Slow(std::io::Cursor<Vec<u8>>);
    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(7);
            self.0.read(&mut buf[..n])
        }
    }

    impl HttpClient for FakeClient {
        fn send(&self, req: Request) -> ink_net::Result<Response> {
            let (status, body) = self.0.get(&req.url).cloned().ok_or_else(|| NetError::Http(format!("connection refused: {}", req.url)))?;
            Ok(Response::new(status, vec![("content-type".into(), "text/plain".into())], Slow(std::io::Cursor::new(body)), req.url.clone()))
        }
    }

    pub fn fake() -> Arc<dyn HttpClient + Send + Sync> {
        let mut m = HashMap::new();
        m.insert("https://x.org/text".to_string(), (200, b"hello streaming world".to_vec()));
        m.insert("https://x.org/sse".to_string(), (200, b"data: one\n\nevent: delta\ndata: two\n\ndata: three".to_vec()));
        m.insert("https://x.org/big".to_string(), (200, vec![b'a'; 100]));
        Arc::new(FakeClient(m))
    }

    fn collect(w: &mut NetWorker) -> Vec<(String, u32, HttpEvent)> {
        let mut all = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while w.in_flight() > 0 && std::time::Instant::now() < deadline {
            all.extend(w.wait(Duration::from_millis(100)));
        }
        all
    }

    #[test]
    fn chunks_sse_whole_errors_and_cancel() {
        let mut w = NetWorker::new(fake());
        let a = w.for_plugin("p").start(Request::get("https://x.org/text"), StreamMode::Chunks).unwrap();
        let b = w.start("q", Request::get("https://x.org/sse"), StreamMode::Sse).unwrap();
        let c = w.start("p", Request::get("https://x.org/big"), StreamMode::Whole { limit: 10 }).unwrap();
        let d = w.start("p", Request::get("https://x.org/missing"), StreamMode::Whole { limit: 10 }).unwrap();
        let e = w.start("p", Request::get("https://x.org/text"), StreamMode::Whole { limit: WHOLE_LIMIT }).unwrap();
        let ev = collect(&mut w);
        let of = |id: u32| ev.iter().filter(|(_, i, _)| *i == id).map(|(_, _, e)| e.clone()).collect::<Vec<_>>();
        let text: Vec<u8> = of(a).iter().filter_map(|e| if let HttpEvent::Chunk(c) = e { Some(c.clone()) } else { None }).flatten().collect();
        assert_eq!(text, b"hello streaming world");
        assert!(of(a).len() > 3, "several chunks: {:?}", of(a));
        assert!(matches!(of(a)[0], HttpEvent::Response { status: 200, .. }));
        assert_eq!(of(a).last(), Some(&HttpEvent::Done { body: None }));
        let datas: Vec<String> = of(b).iter().filter_map(|e| if let HttpEvent::Sse(s) = e { Some(s.data.clone()) } else { None }).collect();
        assert_eq!(datas, ["one", "two"], "an event without the closing blank line is dropped");
        assert!(ev.iter().any(|(p, i, _)| p == "q" && *i == b));
        assert!(matches!(of(c).last(), Some(HttpEvent::Error(m)) if m.contains("larger")));
        assert!(matches!(of(d).last(), Some(HttpEvent::Error(m)) if m.contains("refused")));
        assert_eq!(of(e).last(), Some(&HttpEvent::Done { body: Some(b"hello streaming world".to_vec()) }));
        assert_eq!(w.in_flight(), 0);

        let f = w.start("p", Request::get("https://x.org/text"), StreamMode::Chunks).unwrap();
        w.cancel("other", f);
        assert_eq!(w.in_flight(), 1, "only the owner cancels");
        w.cancel_plugin("p");
        assert_eq!(w.in_flight(), 0);
        std::thread::sleep(Duration::from_millis(100));
        assert!(w.poll().is_empty(), "no events after cancel");
    }

    #[test]
    fn in_flight_limit() {
        // A client that never answers keeps requests in flight.
        struct Hang;
        impl HttpClient for Hang {
            fn send(&self, _req: Request) -> ink_net::Result<Response> {
                std::thread::sleep(Duration::from_millis(300));
                Err(NetError::Timeout)
            }
        }
        let mut w = NetWorker::new(Arc::new(Hang));
        for _ in 0..MAX_IN_FLIGHT {
            w.start("p", Request::get("https://x.org/"), StreamMode::Chunks).unwrap();
        }
        assert!(w.start("p", Request::get("https://x.org/"), StreamMode::Chunks).is_err());
        let ev = collect(&mut w);
        assert_eq!(ev.len(), MAX_IN_FLIGHT);
        assert!(ev.iter().all(|(_, _, e)| matches!(e, HttpEvent::Error(_))));
    }
}
