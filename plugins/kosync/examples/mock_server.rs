//! Mock KOSync + OPDS server for simulator runs (ink-net's TestServer on a fixed port).
//!
//! `cargo run -p kosync --example mock_server -- 127.0.0.1:18080 [BOOK PAGE]` adds the
//! user `reader` with the fake password `fake-pass` and, with BOOK, a progress from
//! another device ("kpw") at page PAGE of BOOK (laid out for the 1272x1696 panel).
//! Serves `/kosync/...`, `/opds/...` and `/opds2/...` until killed.

use ink_doc::{Document, LayoutSettings};
use ink_net::kosync::{userkey, KoSyncAccount, KoSyncClient, Progress};
use ink_net::testserver::{State, TestServer};
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let addr = args.first().cloned().unwrap_or_else(|| "127.0.0.1:18080".into());
    let state = Arc::new(State::default());
    state.add_user("reader", &userkey("fake-pass"));
    let server = TestServer::bind(&addr, false, state).expect("bind");
    println!("mock server at {}", server.base());
    if let (Some(book), Some(page)) = (args.get(1), args.get(2)) {
        let page: usize = page.parse().expect("page number");
        let path = std::path::Path::new(book);
        let mut doc = ink_formats::open(path).expect("open book");
        doc.set_layout(&LayoutSettings::for_screen(1272, 1696)).expect("layout");
        let pos = doc.position_of_page(page).expect("position").to_string();
        let count = doc.page_count().pages.max(1);
        let id = ink_store::partial_md5(path).expect("md5");
        let http = ink_net::UreqClient::new();
        let c = KoSyncClient::new(&http, KoSyncAccount::new(&server.url("/kosync"), "reader", "fake-pass"));
        let p = Progress { document: id.clone(), progress: pos.clone(), percentage: page as f64 / count as f64, device: "kpw".into(), device_id: "0123456789ABCDEF0123456789ABCDEF".into(), timestamp: None, metadata: None };
        c.push(&p).expect("push");
        println!("remote progress for {id}: page {page} of {count}: {pos}");
    }
    server.join();
}
