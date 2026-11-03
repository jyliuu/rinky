//! Verifier cycle 2 (wave-a/net): peak memory of the OPDS parser for a feed within
//! `OpdsClient`'s own size limit. One test per binary: the counting global allocator sees
//! every thread.

use ink_net::opds::{parse_any, OpdsClient};
use ink_net::UreqClient;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Counts live heap bytes and their peak.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = LIVE.fetch_add(l.size(), Ordering::SeqCst) + l.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Peak heap bytes (above the start) while parsing `body` of type `mime`.
fn parse_peak(body: &[u8], mime: &str) -> (usize, usize) {
    let base = LIVE.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    let feed = parse_any(body, mime, "http://catalog.example.com/opds/root.xml").unwrap();
    let items = feed.entries.len() + feed.links.len();
    drop(feed);
    (PEAK.load(Ordering::SeqCst).saturating_sub(base), items)
}

#[test]
#[ignore = "verifier: OPDS parse holds 50-100 bytes per feed byte; a feed at OpdsClient's 16 MiB limit needs 0.8-1.6 GiB"]
fn opds_feed_at_the_client_limit_fits_the_kindles_memory() {
    // OpdsClient::fetch reads up to `max_bytes` (16 MiB by default) and parses it. Each
    // `<entry/>` (8 bytes) becomes an OpdsEntry of about 300 bytes plus its share of the
    // growing Vec, and each `<link/>` (7 bytes) an OpdsLink with the base URL copied into
    // `href` (OPDS 2 JSON: a `{"href":""}` link, 12 bytes, also goes through a
    // serde_json DOM first). Parse a 2 MiB feed of each kind, take bytes held per feed byte, and scale
    // to the client's limit (plus the body itself, which fetch holds while it parses).
    // The Kindle has about 400 MB free with the framework stopped; 256 MiB is the budget
    // here. A hostile or broken catalog must not be able to exhaust it within the limit
    // the client itself enforces.
    let limit = OpdsClient::new(&UreqClient::new()).max_bytes;
    let size = 2usize << 20;
    let mut worst = (0f64, String::new());
    let cases = [
        ("<entry/>", "empty Atom entries", r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>T</title>"#, "</feed>", "application/atom+xml"),
        ("<link/>", "empty Atom feed links", r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>T</title>"#, "</feed>", "application/atom+xml"),
        (r#"{"href":""},"#, "empty OPDS 2 links", r#"{"metadata":{"title":"T"},"links":["#, r#"{"href":""}]}"#, "application/opds+json"),
    ];
    for (unit, name, open, close, mime) in cases {
        let mut xml = String::from(open);
        while xml.len() + unit.len() + close.len() < size {
            xml.push_str(unit);
        }
        xml.push_str(close);
        let (peak, items) = parse_peak(xml.as_bytes(), mime);
        let per_byte = peak as f64 / xml.len() as f64;
        eprintln!("{name}: {} KiB feed, {items} items, parse peak {} MiB = {per_byte:.1} bytes per feed byte", xml.len() >> 10, peak >> 20);
        if per_byte > worst.0 {
            worst = (per_byte, name.to_string());
        }
    }
    let at_limit = ((worst.0 + 1.0) * limit as f64) as u64;
    eprintln!("worst case ({}) scaled to the {} MiB client limit: {} MiB", worst.1, limit >> 20, at_limit >> 20);
    assert!(at_limit <= 256 << 20, "a {} MiB feed of {} needs about {} MiB to parse (budget 256 MiB)", limit >> 20, worst.1, at_limit >> 20);
}
