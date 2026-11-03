//! Test helpers: an HTTP client with canned bodies served in small reads.

use crate::provider::{build_request, ChatRequest, Profile, StreamAccumulator};
use ink_net::{HttpClient, Request, Response};
use ink_plugin::net::run_request_until;
use ink_plugin::{HttpEvent, StreamMode};
use serde_json::Value;
use std::io::Read;
use std::sync::Mutex;

/// Serves one canned status and body for every request, 7 bytes per read (so SSE events
/// and UTF-8 characters are split), and records the request bodies. Status 0 fails the
/// request like a device without network.
pub struct CannedClient {
    bodies: Mutex<std::collections::VecDeque<(u16, Vec<u8>)>>,
    pub sent: Mutex<Vec<Request>>,
}

struct Slow(std::io::Cursor<Vec<u8>>);
impl Read for Slow {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(7);
        self.0.read(&mut buf[..n])
    }
}

impl CannedClient {
    pub fn new(status: u16, body: &str) -> CannedClient {
        Self::seq(&[(status, body)])
    }
    /// Answers the requests in order; the last answer repeats.
    pub fn seq(answers: &[(u16, &str)]) -> CannedClient {
        CannedClient { bodies: Mutex::new(answers.iter().map(|(s, b)| (*s, b.as_bytes().to_vec())).collect()), sent: Mutex::new(Vec::new()) }
    }
    /// JSON bodies of the requests that had one (chat requests; model lists are GETs).
    pub fn bodies_sent(&self) -> Vec<Value> {
        self.sent.lock().unwrap().iter().filter(|r| !r.body.is_empty()).map(|r| serde_json::from_slice(&r.body).unwrap()).collect()
    }
    pub fn last_body(&self) -> Value {
        let s = self.sent.lock().unwrap();
        serde_json::from_slice(&s.last().unwrap().body).unwrap()
    }
}

impl HttpClient for CannedClient {
    fn send(&self, req: Request) -> ink_net::Result<Response> {
        let url = req.url.clone();
        self.sent.lock().unwrap().push(req);
        let mut q = self.bodies.lock().unwrap();
        let (status, body) = if q.len() > 1 { q.pop_front().unwrap() } else { q.front().cloned().unwrap() };
        if status == 0 {
            return Err(ink_net::NetError::Http("network is unreachable".into()));
        }
        Ok(Response::new(status, vec![("content-type".into(), "text/event-stream".into())], Slow(std::io::Cursor::new(body)), url))
    }
}

/// Runs one streamed request through ink-plugin's network path (ink-net's SSE parser)
/// and feeds every event to an accumulator. Returns it and the text deltas.
pub fn run_sse(client: &dyn HttpClient, p: &Profile, req: &ChatRequest) -> (StreamAccumulator, Vec<String>) {
    let events = Mutex::new(Vec::new());
    let r = build_request(p, req, true).unwrap();
    run_request_until(client, r, StreamMode::Sse, &|| false, &|ev| {
        events.lock().unwrap().push(ev);
        true
    });
    let mut acc = StreamAccumulator::new(p.kind);
    let mut deltas = Vec::new();
    for ev in events.into_inner().unwrap() {
        match ev {
            HttpEvent::Sse(e) => {
                let d = acc.feed(&e.data).unwrap();
                if !d.is_empty() {
                    deltas.push(d);
                }
            }
            HttpEvent::Error(e) => panic!("request failed: {e}"),
            _ => {}
        }
    }
    (acc, deltas)
}

/// A book of `n` pages: page `i` (1-based) reads `Page i. Two households ... Verona ...`;
/// one TOC entry per page (`Act 1` for pages 1-10, ...), 30 internal and 30 external
/// links on every page, the reader on page 3.
pub struct FakeBook {
    pub n: usize,
    pub page: usize,
}

impl FakeBook {
    pub fn new(n: usize) -> FakeBook {
        FakeBook { n, page: 2 }
    }
    pub fn pos(i: usize) -> ink_doc::Position {
        format!("/body/DocFragment[{}]/body/p[1]/text().0", i + 1).parse().unwrap()
    }
    fn index(p: &ink_doc::Position) -> usize {
        let s = p.to_string();
        s.split("DocFragment[").nth(1).and_then(|r| r.split(']').next()).and_then(|n| n.parse::<usize>().ok()).unwrap_or(1) - 1
    }
    pub fn text(i: usize) -> String {
        format!("Page {}. Two households, both alike in dignity, in fair Verona, where we lay our scene.", i + 1)
    }
}

impl ink_plugin::DocumentAccess for FakeBook {
    fn metadata(&self) -> ink_doc::Metadata {
        ink_doc::Metadata { title: "Romeo and Juliet".into(), authors: vec!["William Shakespeare".into()], ..Default::default() }
    }
    fn book_id(&self) -> Option<String> {
        Some("0123456789abcdef0123456789abcdef".into())
    }
    fn toc(&self) -> Vec<ink_doc::TocEntry> {
        (0..self.n).map(|i| ink_doc::TocEntry { title: format!("Act {}", i / 10 + 1), level: 0, position: Self::pos(i), page: Some(i) }).collect()
    }
    fn search(&mut self, query: &str, limit: usize) -> ink_plugin::Result<Vec<ink_doc::SearchHit>> {
        let q = query.to_lowercase();
        Ok((0..self.n)
            .filter_map(|i| {
                let t = Self::text(i);
                let at = t.to_lowercase().find(&q)?;
                Some(ink_doc::SearchHit { position: Self::pos(i), page: Some(i), excerpt: format!("{t} {}", "x ".repeat(200)), matched: at..at + q.len() })
            })
            .take(limit)
            .collect())
    }
    fn read_around(&mut self, pos: &ink_doc::Position, chars: usize) -> ink_plugin::Result<String> {
        let i = Self::index(pos);
        Ok((i..self.n).map(Self::text).collect::<Vec<_>>().join(" ").chars().take(chars).collect())
    }
    fn links(&mut self, _pos: &ink_doc::Position) -> ink_plugin::Result<Vec<ink_doc::Link>> {
        Ok((0..60)
            .map(|i| ink_doc::Link {
                text: format!("note {i} {}", "long ".repeat(80)),
                target: if i % 2 == 0 { ink_doc::LinkTarget::Internal(Self::pos(i % self.n)) } else { ink_doc::LinkTarget::External(format!("https://example.com/{i}")) },
                rect: None,
                footnote: i % 4 == 0,
            })
            .collect())
    }
    fn current_position(&self) -> ink_plugin::Result<ink_doc::Position> {
        Ok(Self::pos(self.page))
    }
    fn selection(&self) -> Option<ink_doc::Selection> {
        None
    }
    fn current_page(&self) -> Option<usize> {
        Some(self.page)
    }
    fn page_count(&self) -> Option<usize> {
        Some(self.n)
    }
    fn page_text(&mut self, page: usize) -> ink_plugin::Result<String> {
        Ok(Self::text(page))
    }
}
