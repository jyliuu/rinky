//! Open-time tracing: from the tap in the library to the first page on the panel.
//!
//! The app loop calls [`input_seen`] for every event it dispatches. The reader starts an
//! [`OpenTrace`] when it opens a book, adds a step after each part of the open path
//! (the engines add their own steps through [`ink_doc::timing`]) and finishes it when the
//! first page has been handed to the panel. [`OpenTrace::finish`] writes one line:
//!
//! ```text
//! open: 'Title' tap->page 412.3 ms: tap->reader 2.1 ms, opening screen 30.2 ms, state 1.0 ms, ...
//! ```

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// When the app loop last dispatched an input event.
static LAST_INPUT: Mutex<Option<Instant>> = Mutex::new(None);

/// Notes that an input event is being dispatched now (the tap that may open a book).
pub fn input_seen() {
    if let Ok(mut t) = LAST_INPUT.lock() {
        *t = Some(Instant::now());
    }
}

/// The last input event, when it was less than `max` ago.
fn last_input(max: Duration) -> Option<Instant> {
    let t = (*LAST_INPUT.lock().ok()?)?;
    (t.elapsed() <= max).then_some(t)
}

/// Steps of one book open.
#[derive(Debug)]
pub struct OpenTrace {
    /// The tap (or the reader start when there was no recent input).
    t0: Instant,
    /// End of the last step.
    last: Instant,
    steps: Vec<(&'static str, Duration)>,
    /// The tap was seen (the first step is `tap->reader`).
    from_tap: bool,
}

impl OpenTrace {
    /// Starts a trace when the reader begins to open a book. A tap less than 5 s ago is
    /// the start; the time from it to now is the step `tap->reader` (library hit test,
    /// screen push).
    pub fn start() -> Self {
        let now = Instant::now();
        ink_doc::timing::clear();
        match last_input(Duration::from_secs(5)) {
            Some(tap) => OpenTrace { t0: tap, last: now, steps: vec![("tap->reader", now - tap)], from_tap: true },
            None => OpenTrace { t0: now, last: now, steps: Vec::new(), from_tap: false },
        }
    }

    /// Ends step `name` now.
    pub fn step(&mut self, name: &'static str) {
        let now = Instant::now();
        self.steps.push((name, now - self.last));
        self.last = now;
    }

    /// Ends step `name` now and puts the engine's own steps (recorded on this thread
    /// since the last call) in front of it as `name/<step>`.
    pub fn step_with_engine(&mut self, name: &'static str) {
        let now = Instant::now();
        let inner = ink_doc::timing::take();
        let total = now - self.last;
        let known: Duration = inner.iter().map(|(_, d)| *d).sum();
        for (n, d) in inner {
            self.steps.push((n, d));
        }
        self.steps.push((name, total.saturating_sub(known)));
        self.last = now;
    }

    /// Time since the start.
    pub fn elapsed(&self) -> Duration {
        self.t0.elapsed()
    }

    /// Logs the `open:` line and returns the total.
    pub fn finish(&mut self, title: &str, page: usize, cache: &str) -> Duration {
        self.step("refresh submitted");
        let total = self.t0.elapsed();
        log::info!(
            "open: '{title}' {} {:.1} ms (page {}, page map {cache}): {}",
            if self.from_tap { "tap->page" } else { "reader->page" },
            total.as_secs_f64() * 1000.0,
            page + 1,
            ink_doc::timing::format(&self.steps)
        );
        total
    }
}

/// `rinky perf library DIR HOME`: the library's first scan of `DIR` with an empty
/// `HOME` (folder walk, then metadata and covers of every book, as the library screen
/// does between events), then a second open of the same index (what every later start
/// pays). `HOME` must not exist or be empty. Prints one line per step
/// (`scripts/perf-bench.sh` reads them).
pub fn library_bench(dir: &std::path::Path, home: &std::path::Path) -> anyhow::Result<()> {
    use crate::screens::library::Library;
    if home.exists() && std::fs::read_dir(home)?.next().is_some() {
        anyhow::bail!("{} is not empty", home.display());
    }
    std::fs::create_dir_all(home)?;
    let paths = ink_store::Paths { root: home.to_path_buf() };
    let dirs = [dir.to_path_buf()];
    let t = Instant::now();
    let mut lib = Library::open(&paths, &dirs);
    let scan = t.elapsed();
    let books = lib.pending();
    // As the library screen does: a background thread reads, the caller takes results.
    let t = Instant::now();
    lib.start_background();
    while lib.pending() > 0 {
        std::thread::sleep(Duration::from_millis(5));
        lib.take_background();
        lib.start_background();
    }
    let meta = t.elapsed();
    lib.save();
    let t = Instant::now();
    let again = Library::open(&paths, &dirs);
    let reopen = t.elapsed();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!("perf library: {books} books");
    println!("perf library scan: {:.1} ms", ms(scan));
    println!("perf library metadata: {:.1} ms ({:.2} ms per book)", ms(meta), ms(meta) / books.max(1) as f64);
    println!("perf library reopen: {:.1} ms ({} books, {} pending)", ms(reopen), again.index.books.len(), again.pending());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_collects_engine_steps_in_order() {
        let mut t = OpenTrace { t0: Instant::now(), last: Instant::now(), steps: Vec::new(), from_tap: false };
        ink_doc::timing::clear();
        ink_doc::timing::record("zip", Duration::from_millis(1));
        t.step_with_engine("open");
        t.step("render");
        let names: Vec<_> = t.steps.iter().map(|s| s.0).collect();
        assert_eq!(names, vec!["zip", "open", "render"]);
    }
}
