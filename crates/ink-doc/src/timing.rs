//! Open-time breakdown: engines record how long each step of opening a book took, the
//! app reads the steps back and logs one `open:` line.
//!
//! The steps live in a thread-local list, so a step recorded on the UI thread while it
//! opens a book never mixes with another thread's work. Recording costs one `Vec` push.

use std::cell::RefCell;
use std::time::{Duration, Instant};

thread_local! {
    static STEPS: RefCell<Vec<(&'static str, Duration)>> = const { RefCell::new(Vec::new()) };
}

/// Records step `name` that took `d` (adds to an earlier step of the same name).
pub fn record(name: &'static str, d: Duration) {
    STEPS.with(|s| {
        let mut s = s.borrow_mut();
        match s.iter_mut().find(|(n, _)| *n == name) {
            Some((_, t)) => *t += d,
            None => s.push((name, d)),
        }
    });
}

/// Records step `name` as the time since `since`; returns now (the start of the next step).
pub fn step(name: &'static str, since: Instant) -> Instant {
    let now = Instant::now();
    record(name, now.saturating_duration_since(since));
    now
}

/// Takes the steps recorded on this thread so far, in order.
pub fn take() -> Vec<(&'static str, Duration)> {
    STEPS.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

/// Drops the steps recorded on this thread.
pub fn clear() {
    STEPS.with(|s| s.borrow_mut().clear());
}

/// Formats steps as `name 12.3 ms, name 4.0 ms`.
pub fn format(steps: &[(&'static str, Duration)]) -> String {
    let mut out = String::new();
    for (i, (n, d)) in steps.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("{n} {:.1} ms", d.as_secs_f64() * 1000.0));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_add_up_and_drain() {
        clear();
        record("zip", Duration::from_millis(2));
        record("parse", Duration::from_millis(5));
        record("zip", Duration::from_millis(1));
        let s = take();
        assert_eq!(s, vec![("zip", Duration::from_millis(3)), ("parse", Duration::from_millis(5))]);
        assert_eq!(format(&s), "zip 3.0 ms, parse 5.0 ms");
        assert!(take().is_empty());
    }
}
