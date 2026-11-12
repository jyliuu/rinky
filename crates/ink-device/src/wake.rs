//! Wakes the app loop from another thread: background work finished (for example the
//! annotations a page waits for) and the screen should paint again.
//!
//! [`wake`] sets a flag and writes a byte to a pipe; an input backend that blocks in
//! `poll(2)` adds [`fd`] to its set, and every backend returns from
//! [`crate::input::Input::next_event`] with no event when [`take`] says a wake came, so
//! the loop paints. No polling and no timer while nothing happens.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

static PENDING: AtomicBool = AtomicBool::new(false);
static READ: AtomicI32 = AtomicI32::new(-1);
static WRITE: AtomicI32 = AtomicI32::new(-1);

/// Asks the app loop to run once more (any thread).
pub fn wake() {
    PENDING.store(true, Ordering::SeqCst);
    #[cfg(unix)]
    {
        let _ = fd();
        let w = WRITE.load(Ordering::SeqCst);
        if w >= 0 {
            // SAFETY: a one-byte write to our own non-blocking pipe; a full pipe already
            // wakes the poll.
            unsafe {
                libc::write(w, [1u8].as_ptr().cast(), 1);
            }
        }
    }
}

/// True when a [`wake`] waits to be taken (without taking it).
pub fn pending() -> bool {
    PENDING.load(Ordering::SeqCst)
}

/// True (once) when [`wake`] was called since the last call; empties the pipe.
pub fn take() -> bool {
    if !PENDING.swap(false, Ordering::SeqCst) {
        return false;
    }
    #[cfg(unix)]
    {
        let r = READ.load(Ordering::SeqCst);
        if r >= 0 {
            let mut buf = [0u8; 64];
            // SAFETY: reading into a local buffer from our own non-blocking pipe.
            while unsafe { libc::read(r, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
        }
    }
    true
}

/// The read end of the wake pipe (made on the first call; non-blocking, close-on-exec),
/// for a backend's `poll(2)` set. None when the pipe cannot be made.
#[cfg(unix)]
pub fn fd() -> Option<i32> {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut fds = [-1i32; 2];
        // SAFETY: pipe(2) into a local array of two ints.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return;
        }
        for fd in fds {
            // SAFETY: fcntl on the fds pipe(2) just returned.
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        READ.store(fds[0], Ordering::SeqCst);
        WRITE.store(fds[1], Ordering::SeqCst);
    });
    let r = READ.load(Ordering::SeqCst);
    (r >= 0).then_some(r)
}

/// No pipe on this platform.
#[cfg(not(unix))]
pub fn fd() -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wake_is_taken_once_and_empties_the_pipe() {
        take();
        assert!(!take());
        wake();
        wake();
        assert!(take());
        assert!(!take());
        #[cfg(unix)]
        {
            let mut p = libc::pollfd { fd: fd().unwrap(), events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd, no wait.
            assert_eq!(unsafe { libc::poll(&mut p, 1, 0) }, 0, "nothing left to read");
        }
    }
}
