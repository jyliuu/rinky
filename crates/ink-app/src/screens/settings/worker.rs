//! Long work off the UI thread (update check and install, library rescan and import).
//!
//! A [`Job`] runs a closure on its own thread. The closure reports progress lines
//! through a [`Reporter`]; the screen calls [`Job::poll`] on every event (the screen's
//! `timeout` keeps `UiEvent::Timer` events coming while a job runs) and shows
//! [`Job::status`]. The UI thread keeps calling the device's `next_event`, so the
//! watchdog heartbeat (`/tmp/rinky/heartbeat`) keeps moving during the work.
//!
//! In unit tests jobs run inline by default (the headless backend delivers no timer
//! events, so a thread's result could arrive after the script ended); see
//! [`set_inline`].

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

/// How often a screen with a running job wants a timer event.
pub const POLL: Duration = Duration::from_millis(500);

enum Msg<T> {
    Progress(String),
    Done(T),
}

/// The job's side of the channel.
pub struct Reporter<T> {
    tx: Sender<Msg<T>>,
}

impl<T> Reporter<T> {
    /// Sends a progress line (shown after the job's label).
    pub fn progress(&self, text: impl Into<String>) {
        let _ = self.tx.send(Msg::Progress(text.into()));
    }
}

/// What [`Job::poll`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Poll<T> {
    /// Still running; `true` when the progress line changed.
    Running(bool),
    /// Finished with this value.
    Done(T),
    /// The thread ended without a result (it panicked).
    Lost,
}

/// A closure running on a worker thread.
pub struct Job<T> {
    label: String,
    started: Instant,
    progress: String,
    rx: Receiver<Msg<T>>,
}

#[cfg(test)]
thread_local! {
    static INLINE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Tests only: run jobs on the calling thread (`true`, the default in tests) or on a
/// worker thread.
#[cfg(test)]
pub fn set_inline(on: bool) {
    INLINE.with(|c| c.set(on));
}

fn inline() -> bool {
    #[cfg(test)]
    {
        INLINE.with(|c| c.get())
    }
    #[cfg(not(test))]
    {
        false
    }
}

impl<T: Send + 'static> Job<T> {
    /// Starts `work` on a new thread. `label` is the status text while it runs
    /// ("Installing 0.2.1").
    pub fn spawn(label: impl Into<String>, work: impl FnOnce(&Reporter<T>) -> T + Send + 'static) -> Job<T> {
        let label = label.into();
        let (tx, rx) = mpsc::channel();
        log::info!("job start: {label}");
        if inline() {
            let r = Reporter { tx };
            let v = work(&r);
            let _ = r.tx.send(Msg::Done(v));
        } else {
            let name = format!("job {}", label.chars().take(20).collect::<String>());
            let spawned = std::thread::Builder::new().name(name).spawn(move || {
                let r = Reporter { tx };
                let v = work(&r);
                let _ = r.tx.send(Msg::Done(v));
            });
            if let Err(e) = spawned {
                log::warn!("job {label}: cannot start a thread: {e}");
            }
        }
        Job { label, started: Instant::now(), progress: String::new(), rx }
    }
}

impl<T> Job<T> {
    /// Takes the messages that arrived. Returns the result once.
    pub fn poll(&mut self) -> Poll<T> {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Progress(p)) => {
                    changed |= p != self.progress;
                    self.progress = p;
                }
                Ok(Msg::Done(v)) => {
                    log::info!("job done: {} after {} ms", self.label, self.started.elapsed().as_millis());
                    return Poll::Done(v);
                }
                Err(TryRecvError::Empty) => return Poll::Running(changed),
                Err(TryRecvError::Disconnected) => {
                    log::warn!("job lost: {}", self.label);
                    return Poll::Lost;
                }
            }
        }
    }

    /// Blocks until the job ends (tests and shutdown).
    pub fn wait(&mut self, limit: Duration) -> Poll<T> {
        let end = Instant::now() + limit;
        loop {
            match self.poll() {
                Poll::Running(_) if Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
                p => return p,
            }
        }
    }

    /// The job's label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The last progress line.
    pub fn progress(&self) -> &str {
        &self.progress
    }

    /// Status line: label, progress and elapsed seconds ("Installing 0.2.1… downloading (12 s)").
    pub fn status(&self) -> String {
        let secs = self.started.elapsed().as_secs();
        let mut s = format!("{}…", self.label);
        if !self.progress.is_empty() {
            s.push(' ');
            s.push_str(&self.progress);
        }
        if secs > 0 {
            s.push_str(&format!(" ({secs} s)"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_job_reports_progress_then_its_result() {
        set_inline(false);
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let mut job = Job::spawn("Counting", move |r: &Reporter<u32>| {
            r.progress("step 1");
            go_rx.recv().unwrap();
            41 + 1
        });
        // The thread waits for `go`: the job is running and shows its progress.
        let start = Instant::now();
        while job.progress().is_empty() && start.elapsed() < Duration::from_secs(5) {
            assert!(matches!(job.poll(), Poll::Running(_)));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(job.status(), "Counting… step 1");
        go_tx.send(()).unwrap();
        assert_eq!(job.wait(Duration::from_secs(5)), Poll::Done(42));
        set_inline(true);
    }

    #[test]
    fn a_panicking_job_is_lost_and_inline_jobs_finish_at_once() {
        set_inline(false);
        let mut job = Job::spawn("Boom", |_: &Reporter<()>| panic!("worker test panic"));
        assert_eq!(job.wait(Duration::from_secs(5)), Poll::Lost);
        set_inline(true);
        let mut job = Job::spawn("Inline", |r: &Reporter<&str>| {
            r.progress("half");
            "done"
        });
        assert_eq!(job.poll(), Poll::Done("done"));
    }

    #[test]
    fn update_client_and_config_can_move_to_a_thread() {
        fn send<T: Send + 'static>() {}
        send::<ink_net::UreqClient>();
        send::<crate::update::UpdateConfig>();
        send::<crate::update::Release>();
    }
}
