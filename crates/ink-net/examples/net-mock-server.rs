//! Host mock server for `rinky selftest --net` in the emulator.
//!
//! ```sh
//! cargo run -p ink-net --example net-mock-server -- --http 127.0.0.1:18080 --https 127.0.0.1:18443 --ca-out /tmp/ca.pem
//! ```
//!
//! The guest reaches it at `http://10.0.2.2:18080` (Wi-Fi) or `http://192.168.15.201:18080`
//! (USB networking). Every request is printed to stdout as `HH:MM:SS.mmm METHOD PATH?QUERY
//! [headers of interest]`. Runs until killed.

use ink_net::testserver::{State, TestServer, TEST_CA_PEM};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn stamp() -> String {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let s = (ms / 1000) % 86400;
    format!("{:02}:{:02}:{:02}.{:03}Z", s / 3600, (s / 60) % 60, s % 60, ms % 1000)
}

fn main() -> std::io::Result<()> {
    let mut http = "127.0.0.1:18080".to_string();
    let mut https = Some("127.0.0.1:18443".to_string());
    let mut ca_out = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--http" => http = args.next().expect("--http ADDR"),
            "--https" => https = Some(args.next().expect("--https ADDR")),
            "--no-https" => https = None,
            "--ca-out" => ca_out = args.next(),
            other => {
                eprintln!("unknown option {other}; usage: net-mock-server [--http ADDR] [--https ADDR | --no-https] [--ca-out FILE]");
                std::process::exit(2);
            }
        }
    }
    if let Some(p) = &ca_out {
        std::fs::write(p, TEST_CA_PEM)?;
    }
    let state = Arc::new(State::default());
    let plain = TestServer::bind(&http, false, state.clone())?;
    println!("{} listening {}", stamp(), plain.base());
    let tls = match &https {
        Some(a) => {
            let s = TestServer::bind(a, true, state.clone())?;
            println!("{} listening {} (TEST-ONLY CA{})", stamp(), s.base(), ca_out.as_deref().map(|p| format!(" written to {p}")).unwrap_or_default());
            Some(s)
        }
        None => None,
    };
    let mut seen = 0;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let reqs = state.requests();
        for r in &reqs[seen..] {
            let q = if r.query.is_empty() { String::new() } else { format!("?{}", r.query) };
            let interesting: Vec<String> = r
                .headers
                .iter()
                .filter(|(k, _)| matches!(k.as_str(), "user-agent" | "range" | "if-range" | "last-event-id" | "x-auth-user" | "accept-encoding"))
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            println!("{} {} {}{} [{}]", stamp(), r.method, r.path, q, interesting.join("; "));
        }
        seen = reqs.len();
        let _ = &tls;
    }
}
