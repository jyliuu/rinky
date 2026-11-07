//! Server-sent events (`text/event-stream`), as used by the OpenAI, DeepSeek, OpenRouter
//! and Anthropic streaming APIs (Insightful, phase 8) and by plain SSE endpoints.
//!
//! - [`SseParser`]: feed bytes as they arrive; complete events come out. Follows the WHATWG
//!   rules: a leading UTF-8 BOM is skipped, `data:` lines are joined with `\n`, a blank
//!   line dispatches, lines starting with `:` are comments, one space after the colon is
//!   removed, CR, LF and CRLF all end lines (also when CR and LF arrive in different
//!   chunks), `retry:` takes only ASCII digits, an `id:` containing NUL is ignored.
//!   Bytes are decoded per line, so a UTF-8 character split across chunks is intact.
//!   Memory is bounded: the bytes held for the event being built (the unfinished line,
//!   the joined `data:` lines and the `event:` name) may not exceed
//!   [`SseParser::max_event_bytes`] (default [`DEFAULT_MAX_EVENT_BYTES`]). Past it the
//!   parser frees its buffers and stops ([`SseParser::overflowed`]); a 1 GB line from a
//!   broken proxy cannot fill the Kindle's memory.
//! - [`SseReader`]: reads a streaming body (for example [`crate::Response::body`]) chunk by
//!   chunk and yields events as soon as their blank line arrives. Use it for one-shot
//!   streams such as an AI answer to a POST. An event over the limit ends the reader with
//!   [`NetError::TooLarge`] (after the events completed before it).
//! - [`EventSource`]: GET with reconnects: after a dropped or ended connection it waits
//!   `retry` and reconnects with `Last-Event-ID`. An event over the limit ends it too (no
//!   reconnect: the server would send the same again).

use crate::connectivity::NetActivity;
use crate::http::{HttpClient, Request};
use crate::{CancelToken, NetError, Result};
use std::io::Read;
use std::ops::ControlFlow;
use std::time::Duration;

/// One dispatched event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` field; empty means `message`.
    pub event: String,
    /// Joined `data:` lines.
    pub data: String,
    /// Last `id:` seen (it persists across events, as the specification requires).
    pub id: Option<String>,
    /// Last `retry:` in milliseconds.
    pub retry: Option<u64>,
}

impl SseEvent {
    /// The event type (`message` when no `event:` field was given).
    pub fn kind(&self) -> &str {
        if self.event.is_empty() {
            "message"
        } else {
            &self.event
        }
    }
}

/// Default limit for the bytes held for one event (4 MiB): far above any AI streaming
/// delta or final message, far below the Kindle's free memory.
pub const DEFAULT_MAX_EVENT_BYTES: usize = 4 << 20;

/// Incremental parser.
#[derive(Debug)]
pub struct SseParser {
    buf: Vec<u8>,
    event: String,
    /// `data:` lines of the current event, already joined with `\n` (one string, so the
    /// held bytes are exactly what is counted against the limit).
    data: String,
    has_data: bool,
    last_id: Option<String>,
    retry: Option<u64>,
    skip_lf: bool,
    first_line: bool,
    max_event_bytes: usize,
    overflow: bool,
}

impl Default for SseParser {
    fn default() -> Self {
        SseParser {
            buf: Vec::new(),
            event: String::new(),
            data: String::new(),
            has_data: false,
            last_id: None,
            retry: None,
            skip_lf: false,
            first_line: true,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
            overflow: false,
        }
    }
}

impl SseParser {
    /// A new parser with the default limit ([`DEFAULT_MAX_EVENT_BYTES`]).
    pub fn new() -> Self {
        Self::default()
    }

    /// A new parser that holds at most `max` bytes for one event.
    pub fn with_max_event_bytes(max: usize) -> Self {
        SseParser { max_event_bytes: max, ..Self::default() }
    }

    /// The limit for the bytes held for one event: the unfinished line plus the joined
    /// `data:` lines plus the `event:` name.
    pub fn max_event_bytes(&self) -> usize {
        self.max_event_bytes
    }

    /// Changes the limit (takes effect with the next byte).
    pub fn set_max_event_bytes(&mut self, max: usize) {
        self.max_event_bytes = max;
    }

    /// True once an event went over the limit. The parser then has freed its buffers and
    /// ignores all further input ([`SseParser::feed`] returns nothing); events completed
    /// before the overflow were returned normally. [`SseParser::finish`] does not clear it.
    pub fn overflowed(&self) -> bool {
        self.overflow
    }

    fn held(&self) -> usize {
        self.buf.len() + self.data.len() + self.event.len()
    }

    fn stop_overflowed(&mut self) {
        log::warn!("sse: event larger than {} bytes; stream stopped", self.max_event_bytes);
        self.overflow = true;
        self.buf = Vec::new();
        self.data = String::new();
        self.event = String::new();
        self.has_data = false;
    }

    /// The last event id seen (sent back as `Last-Event-ID` on reconnect).
    pub fn last_event_id(&self) -> Option<&str> {
        self.last_id.as_deref()
    }

    /// The last valid `retry:` value in milliseconds.
    pub fn retry(&self) -> Option<u64> {
        self.retry
    }

    /// Feeds bytes; returns the events completed by them. After an overflow
    /// ([`SseParser::overflowed`]) it returns only the events completed before it, and
    /// nothing on later calls.
    pub fn feed(&mut self, mut bytes: &[u8]) -> Vec<SseEvent> {
        let mut out = Vec::new();
        while !self.overflow && !bytes.is_empty() {
            let (used, ev) = self.feed_until_event(bytes);
            out.extend(ev);
            bytes = &bytes[used..];
        }
        out
    }

    /// Feeds bytes until one event completes: returns how many bytes were used and the
    /// event, if one completed. Callers that hand each event on before parsing further
    /// (as [`SseReader`] does) hold one event at a time, however many a read contains.
    /// After an overflow it uses nothing and returns nothing.
    pub fn feed_until_event(&mut self, bytes: &[u8]) -> (usize, Option<SseEvent>) {
        let mut rest = bytes;
        while !self.overflow && !rest.is_empty() {
            // The LF of a CR LF pair, possibly at the start of the next chunk.
            if std::mem::take(&mut self.skip_lf) && rest[0] == b'\n' {
                rest = &rest[1..];
                continue;
            }
            let end = rest.iter().position(|&b| b == b'\r' || b == b'\n');
            let text = &rest[..end.unwrap_or(rest.len())];
            if self.held() + text.len() > self.max_event_bytes {
                self.stop_overflowed();
                break;
            }
            self.buf.extend_from_slice(text);
            let Some(end) = end else {
                rest = &[];
                break;
            };
            self.skip_lf = rest[end] == b'\r';
            rest = &rest[end + 1..];
            if let Some(ev) = self.end_line() {
                return (bytes.len() - rest.len(), Some(ev));
            }
            if self.held() > self.max_event_bytes {
                self.stop_overflowed();
            }
        }
        (bytes.len() - rest.len(), None)
    }

    /// Handles the line collected in `buf`.
    fn end_line(&mut self) -> Option<SseEvent> {
        let mut raw = std::mem::take(&mut self.buf);
        let bom = std::mem::take(&mut self.first_line) && raw.starts_with(b"\xEF\xBB\xBF");
        let ev = self.line(&String::from_utf8_lossy(&raw[if bom { 3 } else { 0 }..]));
        // Keep a small line buffer for the next line; give a big one back.
        if raw.capacity() <= 64 << 10 {
            raw.clear();
            self.buf = raw;
        }
        ev
    }

    /// Ends the stream. A trailing event without a blank line is discarded, as the
    /// specification requires. The last event id and retry value are kept (a reconnect
    /// continues from them), and so is the overflow flag.
    pub fn finish(&mut self) {
        self.buf.clear();
        self.data.clear();
        self.event.clear();
        self.has_data = false;
        self.skip_lf = false;
        self.first_line = true;
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            if !self.has_data {
                self.event.clear();
                self.data.clear();
                return None;
            }
            let ev = SseEvent {
                event: std::mem::take(&mut self.event),
                data: std::mem::take(&mut self.data),
                id: self.last_id.clone(),
                retry: self.retry,
            };
            self.has_data = false;
            return Some(ev);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.find(':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_string(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            "id" if !value.contains('\0') => self.last_id = Some(value.to_string()),
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                if let Ok(ms) = value.parse() {
                    self.retry = Some(ms);
                }
            }
            _ => {}
        }
        None
    }
}

/// Reads events from a streaming body, one chunk at a time.
pub struct SseReader<R> {
    inner: R,
    parser: SseParser,
    buf: Vec<u8>,
    /// `buf[start..end]`: read but not parsed yet.
    start: usize,
    end: usize,
    eof: bool,
    cancel: Option<CancelToken>,
}

impl<R> std::fmt::Debug for SseReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SseReader").field("parser", &self.parser).field("unparsed", &(self.end - self.start)).field("eof", &self.eof).finish_non_exhaustive()
    }
}

impl<R: Read> SseReader<R> {
    /// Wraps a body.
    pub fn new(inner: R) -> Self {
        SseReader { inner, parser: SseParser::new(), buf: vec![0; 8192], start: 0, end: 0, eof: false, cancel: None }
    }

    /// Sets the per-event limit ([`SseParser::max_event_bytes`]; default
    /// [`DEFAULT_MAX_EVENT_BYTES`]).
    pub fn max_event_bytes(mut self, max: usize) -> Self {
        self.parser.set_max_event_bytes(max);
        self
    }

    /// Checks `token` before every read.
    pub fn with_cancel(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// The parser (last event id, retry).
    pub fn parser(&self) -> &SseParser {
        &self.parser
    }

    /// The next event; `Ok(None)` at the end of the stream. An event over the limit gives
    /// [`NetError::TooLarge`] once (after the events completed before it), then `Ok(None)`.
    ///
    /// Events are parsed one at a time from what was read, so only one is held however
    /// many a single read completes (each event carries its own copy of the last id).
    pub fn next_event(&mut self) -> Result<Option<SseEvent>> {
        loop {
            if self.start < self.end {
                let (used, ev) = self.parser.feed_until_event(&self.buf[self.start..self.end]);
                self.start += used;
                if ev.is_some() {
                    return Ok(ev);
                }
                if self.parser.overflowed() {
                    self.start = self.end;
                    self.eof = true;
                    return Err(NetError::TooLarge(self.parser.max_event_bytes()));
                }
            }
            if self.eof {
                return Ok(None);
            }
            if let Some(c) = &self.cancel {
                c.check()?;
            }
            let n = match self.inner.read(&mut self.buf) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(NetError::from_read(e)),
            };
            if n == 0 {
                self.eof = true;
                self.parser.finish();
            }
            (self.start, self.end) = (0, n);
        }
    }
}

impl<R: Read> Iterator for SseReader<R> {
    type Item = Result<SseEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_event().transpose()
    }
}

/// A reconnecting SSE client for GET endpoints.
pub struct EventSource<'a> {
    client: &'a dyn HttpClient,
    request: Request,
    /// Sent as `Last-Event-ID` on the next connection.
    pub last_event_id: Option<String>,
    /// Wait before reconnecting (the server's `retry:` replaces it).
    pub retry: Duration,
    /// Reconnects allowed in a row without an event in between (a connection that
    /// delivers at least one event resets the count).
    pub max_reconnects: u32,
    /// Stops the stream between reads and during the reconnect wait.
    pub cancel: CancelToken,
    /// Connections made so far.
    pub connections: u32,
    /// Per-event limit ([`SseParser::max_event_bytes`]).
    pub max_event_bytes: usize,
    /// When set, the stream counts as a running transfer while [`EventSource::run`] runs:
    /// it holds off rinky's idle sleep ([`NetActivity::may_auto_suspend`]) and
    /// [`NetActivity::interrupt_all`] stops it through [`EventSource::cancel`].
    pub activity: Option<NetActivity>,
}

impl std::fmt::Debug for EventSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSource").field("url", &self.request.url).field("last_event_id", &self.last_event_id).field("connections", &self.connections).finish_non_exhaustive()
    }
}

impl<'a> EventSource<'a> {
    /// An event source for `request` (normally a GET). Defaults: 3 s retry, 5 reconnects,
    /// [`DEFAULT_MAX_EVENT_BYTES`], no activity tracking.
    pub fn new(client: &'a dyn HttpClient, request: Request) -> Self {
        EventSource {
            client,
            request,
            last_event_id: None,
            retry: Duration::from_secs(3),
            max_reconnects: 5,
            cancel: CancelToken::new(),
            connections: 0,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
            activity: None,
        }
    }

    /// Streams events into `on_event` until it returns `Break` (then `Ok`), the reconnect
    /// budget is spent (the last error), the server answers with a non-200 status or a
    /// non-event-stream type (no reconnect, as the specification says), an event is over
    /// the limit ([`NetError::TooLarge`], no reconnect), or the token is cancelled
    /// ([`NetError::Cancelled`]).
    pub fn run(&mut self, mut on_event: impl FnMut(&SseEvent) -> ControlFlow<()>) -> Result<()> {
        let activity = self.activity.clone();
        let _guard = activity.as_ref().map(|a| a.begin(&self.cancel, None));
        let mut attempts = 0u32;
        loop {
            self.cancel.check()?;
            let mut req = self.request.clone().set_header("Accept", "text/event-stream").set_header("Cache-Control", "no-cache");
            if let Some(id) = &self.last_event_id {
                req = req.set_header("Last-Event-ID", id);
            }
            self.connections += 1;
            let outcome = match self.client.send(req) {
                Ok(resp) => {
                    if resp.status != 200 {
                        return Err(NetError::Status(resp.status));
                    }
                    let mime = resp.mime();
                    if mime != "text/event-stream" {
                        return Err(NetError::Format(format!("content type {mime:?}, expected text/event-stream")));
                    }
                    let mut reader = SseReader::new(resp.body).with_cancel(self.cancel.clone()).max_event_bytes(self.max_event_bytes);
                    let r = loop {
                        match reader.next_event() {
                            Ok(Some(ev)) => {
                                // This connection works: the reconnect budget starts over.
                                attempts = 0;
                                self.last_event_id = ev.id.clone();
                                if let Some(ms) = ev.retry {
                                    self.retry = Duration::from_millis(ms);
                                }
                                if on_event(&ev).is_break() {
                                    return Ok(());
                                }
                            }
                            Ok(None) => break Ok(()),
                            Err(NetError::Cancelled) => return Err(NetError::Cancelled),
                            // The server would send the same oversized event again.
                            Err(e @ NetError::TooLarge(_)) => return Err(e),
                            Err(e) => break Err(e),
                        }
                    };
                    if let Some(id) = reader.parser().last_event_id() {
                        self.last_event_id = Some(id.to_string());
                    }
                    if let Some(ms) = reader.parser().retry() {
                        self.retry = Duration::from_millis(ms);
                    }
                    r
                }
                Err(e) => Err(e),
            };
            if attempts >= self.max_reconnects {
                return match outcome {
                    Ok(()) => Err(NetError::Http("event stream ended; reconnect budget spent".into())),
                    Err(e) => Err(e),
                };
            }
            attempts += 1;
            if let Err(e) = &outcome {
                log::debug!("sse: connection {} failed: {e}; retry in {:?}", self.connections, self.retry);
            }
            sleep_cancellable(self.retry, &self.cancel)?;
        }
    }
}

/// Sleeps `d`, waking every 50 ms to check `cancel`.
pub fn sleep_cancellable(d: Duration, cancel: &CancelToken) -> Result<()> {
    let end = std::time::Instant::now() + d;
    loop {
        cancel.check()?;
        let now = std::time::Instant::now();
        if now >= end {
            return Ok(());
        }
        std::thread::sleep((end - now).min(Duration::from_millis(50)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::TestServer;
    use crate::UreqClient;

    #[test]
    fn openai_style_stream_split_anywhere() {
        let stream = b"data: {\"a\":1}\n\n: keep-alive\n\ndata: {\"b\":2}\r\n\r\ndata: [DONE]\n\n";
        for split in 0..stream.len() {
            let mut p = SseParser::new();
            let mut evs = p.feed(&stream[..split]);
            evs.extend(p.feed(&stream[split..]));
            let data: Vec<_> = evs.iter().map(|e| e.data.as_str()).collect();
            assert_eq!(data, vec!["{\"a\":1}", "{\"b\":2}", "[DONE]"], "split at {split}");
        }
    }

    #[test]
    fn every_byte_split_of_a_rich_stream() {
        let stream = "\u{feff}retry: 900\nevent: delta\ndata: l\u{f8}ne 1\r\ndata:line 2\rid: 4\r\n\r\n: comment\n\ndata\n\nretry: +5\nretry: x1\ndata: end\n\n".as_bytes();
        let want = vec![
            SseEvent { event: "delta".into(), data: "l\u{f8}ne 1\nline 2".into(), id: Some("4".into()), retry: Some(900) },
            SseEvent { event: "".into(), data: "".into(), id: Some("4".into()), retry: Some(900) },
            SseEvent { event: "".into(), data: "end".into(), id: Some("4".into()), retry: Some(900) },
        ];
        for split in 0..stream.len() {
            let mut p = SseParser::new();
            let mut evs = p.feed(&stream[..split]);
            evs.extend(p.feed(&stream[split..]));
            assert_eq!(evs, want, "split at {split}");
        }
        // Byte by byte.
        let mut p = SseParser::new();
        let evs: Vec<_> = stream.iter().flat_map(|b| p.feed(std::slice::from_ref(b))).collect();
        assert_eq!(evs, want);
    }

    #[test]
    fn cr_at_chunk_end_and_lf_at_next_start_is_one_line_end() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: a\r").is_empty());
        // The LF belongs to the CR above; the next CR LF is the blank line.
        let evs = p.feed(b"\n\r\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "a");
    }

    #[test]
    fn anthropic_style_events_and_fields() {
        let mut p = SseParser::new();
        let evs = p.feed(b"event: content_block_delta\ndata: x\ndata:y\nid: 7\nretry: 1500\n\nevent: ping\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0], SseEvent { event: "content_block_delta".into(), data: "x\ny".into(), id: Some("7".into()), retry: Some(1500) });
        assert_eq!(evs[0].kind(), "content_block_delta");
        // "event: ping" without data dispatches nothing, and its name does not leak.
        let evs = p.feed(b"data: z\n\n");
        assert_eq!(evs[0].event, "");
        assert_eq!(evs[0].kind(), "message");
        assert!(p.feed(b"data: partial").is_empty());
        p.finish();
        assert!(p.feed(b"\n").is_empty());
        assert_eq!(p.last_event_id(), Some("7"));
    }

    #[test]
    fn bom_only_at_stream_start_and_nul_ids() {
        let mut p = SseParser::new();
        let evs = p.feed(b"\xEF\xBB\xBFdata: a\nid: x\x00y\n\n\xEF\xBB\xBFdata: b\n\n");
        assert_eq!(evs[0].data, "a");
        assert_eq!(evs[0].id, None);
        // A BOM later in the stream is part of the field name, so the line is ignored.
        assert_eq!(evs.len(), 1);
    }

    #[test]
    fn reader_over_a_chunked_body() {
        struct Chunks(Vec<&'static [u8]>);
        impl Read for Chunks {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.0.is_empty() {
                    return Ok(0);
                }
                let c = self.0.remove(0);
                buf[..c.len()].copy_from_slice(c);
                Ok(c.len())
            }
        }
        let r = SseReader::new(Chunks(vec![b"data: a", b"\n\ndata: b\n", b"\ndata: trailing"]));
        let evs: Vec<_> = r.map(|e| e.unwrap().data).collect();
        assert_eq!(evs, vec!["a", "b"]);
    }

    #[test]
    fn stream_from_server_in_awkward_chunks() {
        let s = TestServer::start().unwrap();
        let resp = UreqClient::new().send(Request::get(s.url("/sse"))).unwrap();
        assert_eq!(resp.mime(), "text/event-stream");
        let evs: Vec<SseEvent> = SseReader::new(resp.body).collect::<Result<_>>().unwrap();
        let data: Vec<_> = evs.iter().map(|e| e.data.as_str()).collect();
        assert_eq!(data, vec!["one", "line1\nline2", "{\"text\":\"\u{f8}\"}", "[DONE]"]);
        assert_eq!(evs[1].event, "multi");
        assert_eq!(evs[1].id.as_deref(), Some("7"));
        assert_eq!(evs[0].retry, Some(2500));
    }

    #[test]
    fn events_arrive_before_the_stream_ends() {
        let s = TestServer::start().unwrap();
        let resp = UreqClient::new().send(Request::get(s.url("/sse-gated"))).unwrap();
        let mut r = SseReader::new(resp.body);
        // The server holds the rest of the stream until the gate opens; the first event
        // must already be here.
        let first = r.next_event().unwrap().unwrap();
        assert_eq!(first.event, "first");
        s.state.open_gate();
        let second = r.next_event().unwrap().unwrap();
        assert_eq!(second.event, "second");
        assert!(r.next_event().unwrap().is_none());
    }

    #[test]
    fn event_source_reconnects_with_last_event_id() {
        let s = TestServer::start().unwrap();
        let client = UreqClient::new();
        let mut es = EventSource::new(&client, Request::get(s.url("/sse-reconnect")));
        es.retry = Duration::from_secs(10); // the server's retry: 50 must replace it
        let mut got = Vec::new();
        let t = std::time::Instant::now();
        es.run(|ev| {
            got.push(format!("{}:{}", ev.id.clone().unwrap_or_default(), ev.data));
            if ev.event == "end" {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .unwrap();
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        assert_eq!(got, vec!["1:a", "2:b", "3:c", "3:bye"]);
        assert_eq!(es.connections, 2);
        let reqs = s.state.requests_to("/sse-reconnect");
        assert_eq!(reqs[0].header("last-event-id"), None);
        assert_eq!(reqs[1].header("last-event-id"), Some("2"));
        assert_eq!(reqs[1].header("accept"), Some("text/event-stream"));
    }

    #[test]
    fn parser_caps_one_event_and_stops() {
        // A line without a newline.
        let mut p = SseParser::with_max_event_bytes(100);
        assert!(p.feed(&[b'a'; 101]).is_empty());
        assert!(p.overflowed());
        assert!(p.feed(b"\n\ndata: later\n\n").is_empty(), "stopped for good");
        // Events completed before the overflow come out; at exactly the limit is fine.
        let mut p = SseParser::with_max_event_bytes(100);
        let mut chunk = b"data: a\n\n".to_vec();
        chunk.extend_from_slice(&[b'x'; 100]);
        let evs = p.feed(&chunk);
        assert_eq!(evs.iter().map(|e| e.data.as_str()).collect::<Vec<_>>(), vec!["a"]);
        assert!(!p.overflowed(), "100 held bytes are within a 100-byte limit");
        assert!(p.feed(b"y").is_empty());
        assert!(p.overflowed());
        // Many empty data lines: each counts (the joined data holds the newlines).
        let mut p = SseParser::with_max_event_bytes(100);
        let evs = p.feed(&b"data\n".repeat(200));
        assert!(evs.is_empty() && p.overflowed());
        // An event name that never ends.
        let mut p = SseParser::with_max_event_bytes(100);
        p.feed(b"event: ");
        p.feed(&[b'e'; 200]);
        assert!(p.overflowed());
        // finish() keeps the flag.
        p.finish();
        assert!(p.overflowed());
        // The default.
        assert_eq!(SseParser::new().max_event_bytes(), DEFAULT_MAX_EVENT_BYTES);
    }

    #[test]
    fn reader_delivers_earlier_events_then_too_large_then_end() {
        let mut body = b"data: one\n\ndata: two\n\n".to_vec();
        body.extend_from_slice(&[b'z'; 5000]);
        let mut r = SseReader::new(std::io::Cursor::new(body)).max_event_bytes(1000);
        assert_eq!(r.next_event().unwrap().unwrap().data, "one");
        assert_eq!(r.next_event().unwrap().unwrap().data, "two");
        assert!(matches!(r.next_event(), Err(NetError::TooLarge(1000))));
        assert!(r.next_event().unwrap().is_none());
    }

    /// Serves canned event streams, one per connection; counts connections.
    struct Streams {
        bodies: Vec<&'static [u8]>,
        sent: std::cell::Cell<usize>,
    }

    impl HttpClient for Streams {
        fn send(&self, _req: Request) -> Result<crate::Response> {
            let i = self.sent.get();
            self.sent.set(i + 1);
            let body = self.bodies.get(i).copied().unwrap_or(b"");
            Ok(crate::Response::new(200, vec![("Content-Type".into(), "text/event-stream".into())], std::io::Cursor::new(body.to_vec()), "http://h/sse"))
        }
    }

    #[test]
    fn event_source_ends_on_an_oversized_event() {
        let big: &'static [u8] = Box::leak(format!("data: {}\n\n", "q".repeat(3000)).into_bytes().into_boxed_slice());
        let client = Streams { bodies: vec![b"data: ok\n\n", big, b"data: never\n\n"], sent: Default::default() };
        let mut es = EventSource::new(&client, Request::get("http://h/sse"));
        es.retry = Duration::from_millis(1);
        es.max_event_bytes = 1000;
        let mut got = Vec::new();
        let r = es.run(|ev| {
            got.push(ev.data.clone());
            ControlFlow::Continue(())
        });
        assert!(matches!(r, Err(NetError::TooLarge(1000))), "{r:?}");
        assert_eq!(got, vec!["ok"]);
        assert_eq!(client.sent.get(), 2, "no reconnect after the oversized event");
    }

    #[test]
    fn event_source_budget_counts_failures_in_a_row() {
        // Every connection delivers one event and ends: with max_reconnects 1 the stream
        // still runs on, because each connection works.
        let client = Streams { bodies: vec![b"data: 1\n\n", b"data: 2\n\n", b"data: 3\n\n", b"event: end\ndata: 4\n\n"], sent: Default::default() };
        let activity = NetActivity::new();
        let mut es = EventSource::new(&client, Request::get("http://h/sse"));
        es.retry = Duration::from_millis(1);
        es.max_reconnects = 1;
        es.activity = Some(activity.clone());
        let mut got = Vec::new();
        es.run(|ev| {
            assert!(activity.busy() && !activity.may_auto_suspend(), "the stream counts as activity");
            got.push(ev.data.clone());
            if ev.event == "end" {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .unwrap();
        assert_eq!(got, vec!["1", "2", "3", "4"]);
        assert!(!activity.busy());
        // After the last working connection, two empty reconnects spend a budget of 2.
        let client = Streams { bodies: vec![b"data: 1\n\n", b"", b""], sent: Default::default() };
        let mut es = EventSource::new(&client, Request::get("http://h/sse"));
        es.retry = Duration::from_millis(1);
        es.max_reconnects = 2;
        assert!(es.run(|_| ControlFlow::Continue(())).is_err());
        assert_eq!(client.sent.get(), 3);
    }

    #[test]
    fn interrupt_all_stops_an_event_source() {
        let client = Streams { bodies: vec![b"data: 1\n\n"; 50], sent: Default::default() };
        let activity = NetActivity::new();
        let mut es = EventSource::new(&client, Request::get("http://h/sse"));
        es.retry = Duration::from_millis(1);
        es.activity = Some(activity.clone());
        let r = es.run(|_| {
            activity.interrupt_all();
            ControlFlow::Continue(())
        });
        assert!(matches!(r, Err(NetError::Cancelled)), "{r:?}");
    }

    #[test]
    fn event_source_stops_on_errors_and_cancel() {
        let s = TestServer::start().unwrap();
        let client = UreqClient::new();
        let mut es = EventSource::new(&client, Request::get(s.url("/status/500")));
        assert!(matches!(es.run(|_| ControlFlow::Continue(())), Err(NetError::Status(500))));
        let mut es = EventSource::new(&client, Request::get(s.url("/echo")));
        assert!(matches!(es.run(|_| ControlFlow::Continue(())), Err(NetError::Format(_))));
        let mut es = EventSource::new(&client, Request::get(s.url("/sse-reconnect")));
        es.retry = Duration::from_secs(30);
        let token = es.cancel.clone();
        let mut n = 0;
        let r = es.run(|_| {
            n += 1;
            if n == 2 {
                token.cancel();
            }
            ControlFlow::Continue(())
        });
        assert!(matches!(r, Err(NetError::Cancelled)), "{r:?}");
    }
}
