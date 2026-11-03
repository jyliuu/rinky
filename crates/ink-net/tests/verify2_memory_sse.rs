//! Verifier cycle 2 (wave-a/net): peak memory of `SseReader` when the server sets a long
//! `id:`. One test per binary: the counting global allocator sees every thread.

use ink_net::sse::{SseReader, DEFAULT_MAX_EVENT_BYTES};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Read;
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

/// A body: `head`, then `unit` repeated until `left` bytes are sent.
struct Body {
    head: Vec<u8>,
    pos: usize,
    unit: &'static [u8],
    i: usize,
    left: usize,
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() && self.pos < self.head.len() {
            buf[n] = self.head[self.pos];
            self.pos += 1;
            n += 1;
        }
        if n > 0 {
            return Ok(n);
        }
        while n < buf.len() && self.left > 0 {
            buf[n] = self.unit[self.i % self.unit.len()];
            self.i += 1;
            self.left -= 1;
            n += 1;
        }
        Ok(n)
    }
}

#[test]
#[ignore = "verifier: SSE id is cloned into every buffered event; one 8 KiB read of data lines holds ~1365 copies (memory cap bypassed)"]
fn sse_reader_memory_stays_bounded_with_a_long_event_id() {
    // The cycle-1 fix caps the bytes held for the event being built (line + data + event
    // name, default 4 MiB). The `id:` value is not counted: it persists across events
    // (as it should) and every dispatched SseEvent gets its own clone of it. SseReader
    // feeds a whole 8 KiB read into the parser and queues every event that read
    // completes, so `data\n\n` (6 bytes per event) puts ~1365 events, each with its own
    // copy of the id, in memory at once.
    //
    // Here: a 64 KiB id, then 24 KB of `data\n\n` -> about 85 MiB at peak with the
    // default limit. The same stream after a 4 MiB id (allowed by the default limit)
    // needs ~1365 x 4 MiB = 5.4 GiB for one 8 KiB read; the Kindle has about 400 MB free.
    let mut head = b"id: ".to_vec();
    head.extend(std::iter::repeat_n(b'i', 64 << 10));
    head.extend_from_slice(b"\n");
    let body = Body { head, pos: 0, unit: b"data\n\n", i: 0, left: 6 * 4000 };
    let base = LIVE.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    let mut r = SseReader::new(body);
    let mut events = 0usize;
    // Drop each event at once, as EventSource and an AI stream handler do.
    while let Ok(Some(ev)) = r.next_event() {
        assert_eq!(ev.id.as_ref().map(String::len), Some(64 << 10));
        events += 1;
    }
    drop(r);
    let peak = PEAK.load(Ordering::SeqCst).saturating_sub(base);
    eprintln!("SseReader: {events} events, peak {} KiB above the start", peak >> 10);
    assert_eq!(events, 4000);
    let bound = 3 * DEFAULT_MAX_EVENT_BYTES;
    assert!(peak <= bound, "SseReader held {} MiB at once for a 64 KiB id and 24 KB of events (bound {} MiB = 3 x the per-event limit)", peak >> 20, bound >> 20);
}
