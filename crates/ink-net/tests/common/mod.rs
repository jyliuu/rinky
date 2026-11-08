//! Helpers shared by the verifier suites: raw servers that play misbehaving peers.

#![allow(dead_code)]

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

type Handler<S> = dyn Fn(&mut S, String, usize) + Send + Sync;

/// Reads one request head (up to the blank line).
pub fn read_head(s: &mut dyn Read) -> String {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") && buf.len() < 65536 {
        match s.read(&mut b) {
            Ok(1) => buf.push(b[0]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// The value of header `name` in a request head.
pub fn header_of<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim()))
}

/// A server on `127.0.0.1` whose connections `wrap` turns into streams (plain or TLS);
/// `h(stream, request head, connection number)` runs per connection. Returns the base URL
/// (`scheme://addr`) and the connection counter.
pub fn serve<S: Read + Send + 'static>(
    scheme: &str,
    wrap: impl Fn(TcpStream) -> S + Send + Sync + 'static,
    h: impl Fn(&mut S, String, usize) + Send + Sync + 'static,
) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("{scheme}://{}", l.local_addr().unwrap());
    let h: Arc<Handler<S>> = Arc::new(h);
    let wrap = Arc::new(wrap);
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(tcp) = c else { continue };
            let (h, wrap, i) = (h.clone(), wrap.clone(), n2.fetch_add(1, Ordering::SeqCst));
            std::thread::spawn(move || {
                let _ = tcp.set_nodelay(true);
                let mut s = wrap(tcp);
                let head = read_head(&mut s);
                h(&mut s, head, i);
            });
        }
    });
    (base, n)
}

/// A fresh, empty directory for one test.
pub fn tmpdir(suite: &str, name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ink-net-{suite}-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
