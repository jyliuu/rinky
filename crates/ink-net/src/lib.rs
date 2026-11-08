//! Networking for rinky.
//!
//! - [`http`]: the [`HttpClient`] trait and [`UreqClient`] (HTTPS with rustls + webpki-roots,
//!   timeouts, redirects, user agent).
//! - [`sse`]: server-sent events parser, a streaming reader over a response body and an
//!   [`sse::EventSource`] that reconnects with `Last-Event-ID`.
//! - [`download`]: downloads to a file with progress, cancel, resume (`Range`), checksum
//!   verification and an atomic rename.
//! - [`connectivity`]: the Wi-Fi hook ([`Connectivity`]) and the suspend hold
//!   ([`KeepAwake`], [`NetActivity`]) that downloads use.
//! - [`opds`]: OPDS 1.x Atom and OPDS 2 JSON catalogs, OpenSearch.
//! - [`kosync`]: KOReader sync server client (register, authorize, push, pull).
//! - [`url`]: RFC 3986 reference resolution and percent-encoding (no `url` crate).
//! - [`testserver`]: a local HTTP/HTTPS mock server for tests and emulator checks.
//! - [`selftest`]: network checks run by `rinky selftest --net URL`.
//!
//! This crate depends on no other ink crate: the app wires ink-device's Wi-Fi and power
//! services into [`Connectivity`] and [`KeepAwake`].
//!
//! Owner from wave A: the `net` agent.

#![warn(missing_docs)]

/// `impl Debug` that shows only the listed fields, for types that hold a body or a trait
/// object: `debug_fields!([generics] Type, "Name", |s| "field": expr, ...)`.
macro_rules! debug_fields {
    ([$($g:tt)*] $t:ty, $name:literal, |$s:ident| $($f:literal: $e:expr),+) => {
        impl<$($g)*> std::fmt::Debug for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let $s = self;
                f.debug_struct($name)$(.field($f, &$e))+.finish_non_exhaustive()
            }
        }
    };
}

pub mod connectivity;
pub mod download;
pub mod http;
pub mod kosync;
pub mod opds;
pub mod selftest;
pub mod sse;
pub mod testserver;
pub mod url;

pub use connectivity::{Connectivity, KeepAwake, NetActivity};
pub use download::{Checksum, Download, DownloadOutcome, DownloadProgress};
pub use http::{ClientConfig, HttpClient, Method, Request, Response, TlsRoots, UreqClient};
pub use sse::{SseEvent, SseParser, SseReader};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Errors from this crate.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// Connection or protocol failure.
    #[error("http: {0}")]
    Http(String),
    /// TLS failure: handshake, unknown issuer, bad certificate, PEM parsing.
    #[error("tls: {0}")]
    Tls(String),
    /// The request took too long (connect, response head, a read, or the total limit).
    #[error("timed out")]
    Timeout,
    /// The body exceeded the limit given by the caller.
    #[error("response larger than {0} bytes")]
    TooLarge(usize),
    /// The server answered with an unexpected status.
    #[error("unexpected status {0}")]
    Status(u16),
    /// The server refused the credentials (HTTP 401).
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// The server answered with an error status and a message.
    #[error("server error {status}: {message}")]
    Server {
        /// HTTP status.
        status: u16,
        /// Message from the body (or the status reason).
        message: String,
    },
    /// The payload could not be parsed.
    #[error("bad response: {0}")]
    Format(String),
    /// The downloaded bytes do not match the expected checksum.
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Checksum {
        /// Expected digest (lowercase hex).
        expected: String,
        /// Digest of the received bytes (lowercase hex).
        actual: String,
    },
    /// The transfer was cancelled through a [`CancelToken`].
    #[error("cancelled")]
    Cancelled,
    /// No network: Wi-Fi is off or not connected ([`Connectivity`]).
    #[error("offline: {0}")]
    Offline(String),
    /// Another transfer is writing the same file (the same download started twice, for
    /// example by a double tap). The running one is not disturbed.
    #[error("busy: {0}")]
    Busy(String),
    /// Not implemented yet; the message says when.
    #[error("not supported: {0}")]
    Unsupported(String),
    /// I/O while reading a body or writing a file.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl NetError {
    /// Maps an I/O error from a body read: `TimedOut`/`WouldBlock` become [`NetError::Timeout`].
    /// ureq reports its own errors inside `io::Error`s while a body is read; those are
    /// unwrapped and mapped like request errors.
    pub fn from_read(e: std::io::Error) -> NetError {
        match e.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => NetError::Timeout,
            _ if e.get_ref().is_some_and(|i| i.is::<ureq::Error>()) => match e.into_inner().map(|i| i.downcast::<ureq::Error>()) {
                Some(Ok(u)) => http::map_ureq(*u),
                Some(Err(other)) => NetError::Io(std::io::Error::other(other)),
                None => NetError::Http("read error".into()),
            },
            _ => NetError::Io(e),
        }
    }
}

/// Result alias for this crate.
pub type Result<T, E = NetError> = std::result::Result<T, E>;

/// A cancel flag shared between the UI thread and a transfer. Cloning shares the flag.
///
/// Transfers check it between reads; a read that is blocked returns at the next chunk or
/// at the read timeout ([`Request::read_timeout`]).
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A new, not cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// True after [`CancelToken::cancel`].
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// `Err(NetError::Cancelled)` after [`CancelToken::cancel`].
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(NetError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_is_shared() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(a.check().is_ok());
        b.cancel();
        assert!(a.is_cancelled());
        assert!(matches!(a.check(), Err(NetError::Cancelled)));
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0xff, 0x10]), "00abff10");
    }

    #[test]
    fn read_errors_map_timeouts() {
        let e = NetError::from_read(std::io::Error::new(std::io::ErrorKind::TimedOut, "t"));
        assert!(matches!(e, NetError::Timeout));
        let e = NetError::from_read(std::io::Error::other("x"));
        assert!(matches!(e, NetError::Io(_)));
    }
}
