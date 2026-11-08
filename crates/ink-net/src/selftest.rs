//! Network self-test: runs every client feature against a [`crate::testserver`] instance
//! (`cargo run -p ink-net --example net-mock-server` on the host, reached from the
//! emulator at `10.0.2.2` over Wi-Fi or `192.168.15.201` over USB networking).
//!
//! `rinky selftest --net URL [--net-tls URL --ca PEM]` prints one line per check.
//! The Wi-Fi hook and the keep-awake hook are passed in by the app (ink-device on the
//! Kindle); without them those checks are skipped.

use crate::connectivity::{Connectivity, KeepAwake, NetActivity};
use crate::download::{lock_path, part_path, sha256_hex, Checksum, Download, DownloadProgress, Hooks};
use crate::http::{HttpClient, Request};
use crate::kosync::{KoSyncAccount, KoSyncClient, Progress};
use crate::opds::{FeedKind, OpdsClient, OpdsVersion};
use crate::sse::{EventSource, SseReader};
use crate::testserver::file_bytes;
use crate::{CancelToken, NetError, UreqClient};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// What to test.
pub struct NetSelftest<'a> {
    /// Plain HTTP base URL of the mock server (`http://10.0.2.2:18080`).
    pub http_base: String,
    /// HTTPS base URL of the mock server, if any.
    pub https_base: Option<String>,
    /// PEM of the CA that signed the HTTPS server's certificate.
    pub ca_pem: Option<Vec<u8>>,
    /// The Wi-Fi hook (ink-device Wi-Fi state).
    pub wifi: Option<&'a dyn Connectivity>,
    /// The suspend hold (powerd `deferSuspend`).
    pub keep_awake: Option<&'a dyn KeepAwake>,
    /// Scratch directory for downloads.
    pub scratch: PathBuf,
}

/// One result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckResult {
    /// Check name (`net.https.verified`, ...).
    pub name: String,
    /// `Ok(detail)`, `Err(reason)`; `None` = skipped (with the reason in `detail`).
    pub outcome: Option<Result<String, String>>,
    /// Detail for skipped checks.
    pub skipped: Option<String>,
    /// Duration.
    pub millis: u128,
}

impl CheckResult {
    /// `PASS name: detail (N ms)` / `FAIL ...` / `SKIP name: why`.
    pub fn line(&self) -> String {
        match (&self.outcome, &self.skipped) {
            (Some(Ok(d)), _) => format!("PASS {}: {d} ({} ms)", self.name, self.millis),
            (Some(Err(e)), _) => format!("FAIL {}: {e} ({} ms)", self.name, self.millis),
            (None, s) => format!("SKIP {}: {}", self.name, s.clone().unwrap_or_default()),
        }
    }

    /// True for a failure.
    pub fn failed(&self) -> bool {
        matches!(self.outcome, Some(Err(_)))
    }
}

fn ensure(cond: bool, msg: impl FnOnce() -> String) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(msg())
    }
}

fn e2s(e: NetError) -> String {
    e.to_string()
}

impl NetSelftest<'_> {
    /// Runs all checks; `on_result` sees each as it finishes (for live output).
    pub fn run(&self, mut on_result: impl FnMut(&CheckResult)) -> Vec<CheckResult> {
        let mut r = Runner { out: Vec::new(), on_result: &mut on_result };
        let _ = std::fs::create_dir_all(&self.scratch);
        let http = UreqClient::new();
        let base = self.http_base.trim_end_matches('/').to_string();
        let url = |p: &str| format!("{base}{p}");

        // Wi-Fi hook.
        match self.wifi {
            Some(w) => r.check("net.wifi_hook", move || Ok(match w.check() {
                Ok(()) => "online (wifid cmState CONNECTED)".to_string(),
                Err(e) => format!("{e}"),
            })),
            None => r.skip("net.wifi_hook", "no Wi-Fi hook (pass --lipc on the Kindle)"),
        }

        // HTTPS.
        match (&self.https_base, &self.ca_pem) {
            (Some(hb), Some(pem)) => {
                let hb = hb.trim_end_matches('/').to_string();
                let hb2 = hb.clone();
                r.check("net.https.verified", move || {
                    let c = UreqClient::with_ca_pem(pem).map_err(e2s)?;
                    let resp = c.send(Request::get(format!("{hb}/redirect/2"))).map_err(e2s)?;
                    ensure(resp.status == 200, || format!("status {}", resp.status))?;
                    let final_url = resp.url.clone();
                    let body = resp.text(1000).map_err(e2s)?;
                    ensure(body == "redirect done", || format!("body {body:?}"))?;
                    Ok(format!("TLS with the test CA (rustls/ring), 2 redirects to {final_url}"))
                });
                r.check("net.https.unknown_ca_refused", move || {
                    match UreqClient::new().send(Request::get(format!("{hb2}/echo")).timeout(Duration::from_secs(10))) {
                        Err(NetError::Tls(m)) => Ok(format!("webpki roots ({} anchors) refuse the test server: {}", crate::TlsRoots::WebPki.len(), m.chars().take(90).collect::<String>())),
                        Err(other) => Err(format!("expected a TLS error, got {other}")),
                        Ok(r) => Err(format!("expected a TLS error, got status {}", r.status)),
                    }
                });
            }
            _ => {
                r.skip("net.https.verified", "pass --net-tls URL and --ca PEM");
                r.skip("net.https.unknown_ca_refused", "pass --net-tls URL and --ca PEM");
            }
        }

        // Plain HTTP: user agent, redirects, timeouts.
        let h = &http;
        let u = url("/echo");
        r.check("net.http.user_agent", move || {
            let v: serde_json::Value = h.send(Request::get(&u)).map_err(e2s)?.json(1 << 16).map_err(e2s)?;
            let uas: Vec<String> = v["headers"].as_array().into_iter().flatten().filter(|kv| kv[0] == "user-agent").map(|kv| kv[1].as_str().unwrap_or("").to_string()).collect();
            ensure(uas.len() == 1 && uas[0].starts_with("rinky/"), || format!("user-agent headers {uas:?}"))?;
            Ok(format!("server saw one User-Agent: {}", uas[0]))
        });
        let u = url("/redirect/3");
        r.check("net.http.redirects", move || {
            let r = h.send(Request::get(&u)).map_err(e2s)?;
            ensure(r.status == 200 && r.url.ends_with("/redirect/0"), || format!("status {} url {}", r.status, r.url))?;
            let r = h.send(Request::get(&u).max_redirects(1));
            ensure(matches!(r, Err(NetError::Http(ref m)) if m.contains("too many redirects")), || format!("limit not enforced: {r:?}"))?;
            Ok("3 relative 302s followed; a limit of 1 is enforced".into())
        });
        let (u1, u2) = (url("/slow?ms=3000"), url("/stall?ms=3000"));
        r.check("net.http.timeouts", move || {
            let t = Instant::now();
            let r = h.send(Request::get(&u1).timeout(Duration::from_millis(700)));
            let head_ms = t.elapsed().as_millis();
            ensure(matches!(r, Err(NetError::Timeout)) && head_ms < 2500, || format!("head timeout: {r:?} after {head_ms} ms"))?;
            let t = Instant::now();
            let resp = h.send(Request::get(&u2).read_timeout(Duration::from_millis(700))).map_err(e2s)?;
            let r = resp.bytes(100);
            let read_ms = t.elapsed().as_millis();
            ensure(matches!(r, Err(NetError::Timeout)) && read_ms < 2500, || format!("read timeout: {r:?} after {read_ms} ms"))?;
            Ok(format!("head limit fired after {head_ms} ms, stalled body after {read_ms} ms"))
        });

        // SSE.
        let u = url("/sse");
        r.check("net.sse.stream", move || {
            let resp = h.send(Request::get(&u)).map_err(e2s)?;
            let evs: Vec<_> = SseReader::new(resp.body).collect::<crate::Result<Vec<_>>>().map_err(e2s)?;
            let data: Vec<&str> = evs.iter().map(|e| e.data.as_str()).collect();
            ensure(data == ["one", "line1\nline2", "{\"text\":\"\u{f8}\"}", "[DONE]"], || format!("events {data:?}"))?;
            ensure(evs[1].event == "multi" && evs[1].id.as_deref() == Some("7") && evs[0].retry == Some(2500), || format!("fields {:?}", evs[1]))?;
            Ok("4 events from 9 chunks (BOM, CR|LF split, UTF-8 split, comment, retry, multi-line data)".into())
        });
        let u = url("/sse-reconnect");
        r.check("net.sse.reconnect", move || {
            let mut es = EventSource::new(h, Request::get(&u));
            let mut got = Vec::new();
            es.run(|ev| {
                got.push(ev.id.clone().unwrap_or_default());
                if ev.event == "end" {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
            .map_err(e2s)?;
            ensure(got == ["1", "2", "3", "3"] && es.connections == 2, || format!("ids {got:?}, {} connections", es.connections))?;
            Ok("dropped after id 2, reconnected with Last-Event-ID: 2 after retry 50 ms".into())
        });

        // Downloads.
        let dir = self.scratch.clone();
        // A fresh query per run: the server breaks the first transfer of each URL once.
        let run_id = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let u = url(&format!("/flaky/300000?run={run_id}"));
        r.check("net.download.resume", move || {
            let dest = dir.join("resume.bin");
            let _ = std::fs::remove_file(&dest);
            let want = sha256_hex(&file_bytes(300_000));
            let dl = Download::new(&u, &dest).checksum(Checksum::Sha256(want.clone()));
            dl.discard_partial();
            let first = dl.run(h, &mut Hooks::default());
            ensure(first.is_err(), || "the first transfer should break".into())?;
            let have = dl.partial_bytes();
            // An older file at the destination is replaced by the rename.
            std::fs::write(&dest, b"old version").map_err(|e| e.to_string())?;
            let out = dl.run(h, &mut Hooks::default()).map_err(e2s)?;
            ensure(out.resumed_from == have && have > 0 && out.sha256 == want && !part_path(&dest).exists(), || format!("{out:?}"))?;
            let on_disk = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            ensure(on_disk == 300_000, || format!("{} has {on_disk} bytes after the rename", dest.display()))?;
            let _ = std::fs::remove_file(&dest);
            Ok(format!("broken at {have} bytes, resumed with Range, SHA-256 verified, renamed over an older file in {}", dir.display()))
        });
        let dir = self.scratch.clone();
        let u = url("/files/1000000");
        r.check("net.download.cancel", move || {
            let dest = dir.join("cancel.bin");
            let _ = std::fs::remove_file(&dest);
            let dl = Download::new(&u, &dest);
            dl.discard_partial();
            let token = CancelToken::new();
            let t2 = token.clone();
            let mut events = 0u32;
            let mut hooks = Hooks {
                cancel: token,
                on_progress: Some(Box::new(|p: &DownloadProgress| {
                    events += 1;
                    if p.received >= 200_000 {
                        t2.cancel();
                    }
                })),
                ..Default::default()
            };
            let r = dl.run(h, &mut hooks);
            drop(hooks);
            ensure(matches!(r, Err(NetError::Cancelled)), || format!("expected cancel, got {r:?}"))?;
            let have = dl.partial_bytes();
            let out = dl.run(h, &mut Hooks::default()).map_err(e2s)?;
            ensure(out.bytes == 1_000_000 && out.resumed_from == have, || format!("{out:?}"))?;
            let _ = std::fs::remove_file(&dest);
            Ok(format!("cancelled at {have} bytes after {events} progress events; .part kept and resumed"))
        });
        let dir = self.scratch.clone();
        let u = url("/files/5000");
        r.check("net.download.checksum_mismatch", move || {
            let dest = dir.join("bad.bin");
            let dl = Download::new(&u, &dest).checksum(Checksum::Sha256("0".repeat(64)));
            match dl.run(h, &mut Hooks::default()) {
                Err(NetError::Checksum { actual, .. }) if !dest.exists() && !part_path(&dest).exists() => Ok(format!("refused: got {}...; nothing left on disk", &actual[..12])),
                other => Err(format!("expected a checksum error, got {other:?}")),
            }
        });
        let dir = self.scratch.clone();
        let u = url("/files/400000");
        r.check("net.download.locked", move || {
            let dest = dir.join("twice.bin");
            let _ = std::fs::remove_file(&dest);
            let dl = Download::new(&u, &dest);
            dl.discard_partial();
            // A double tap: the second run starts while the first one writes the .part.
            let mut second: Option<crate::Result<_>> = None;
            let mut hooks = Hooks {
                on_progress: Some(Box::new(|p: &DownloadProgress| {
                    if p.received > 0 && second.is_none() {
                        second = Some(Download::new(&u, &dest).run(h, &mut Hooks::default()));
                    }
                })),
                ..Default::default()
            };
            let first = dl.run(h, &mut hooks);
            drop(hooks);
            let first = first.map_err(e2s)?;
            ensure(matches!(second, Some(Err(NetError::Busy(_)))), || format!("second run: {second:?}"))?;
            let on_disk = std::fs::read(&dest).map_err(|e| e.to_string())?;
            ensure(first.bytes == 400_000 && on_disk == file_bytes(400_000) && !lock_path(&dest).exists(), || format!("{first:?}, {} bytes on disk", on_disk.len()))?;
            let _ = std::fs::remove_file(&dest);
            // Another process: an flock on the lock file from a separate open file.
            let other = std::fs::File::create(lock_path(&dest)).map_err(|e| e.to_string())?;
            let flock = match other.try_lock() {
                Ok(()) => {
                    let r = dl.run(h, &mut Hooks::default());
                    ensure(matches!(r, Err(NetError::Busy(_))), || format!("run under a foreign flock: {r:?}"))?;
                    drop(other);
                    let out = dl.run(h, &mut Hooks::default()).map_err(e2s)?;
                    ensure(out.bytes == 400_000 && !lock_path(&dest).exists(), || format!("after the flock: {out:?}"))?;
                    let _ = std::fs::remove_file(&dest);
                    "flock held by another open file: busy; released (stale lock file): ran and removed it"
                }
                Err(e) => {
                    drop(other);
                    let _ = std::fs::remove_file(lock_path(&dest));
                    return Ok(format!("double start: second run busy, dest is the book; no flock on this file system ({e}): in-process guard only"));
                }
            };
            Ok(format!("double start: second run busy, dest is the book (400000 bytes); {flock}; in {}", dir.display()))
        });
        let (dir, https, pem) = (self.scratch.clone(), self.https_base.clone(), self.ca_pem.clone());
        let (u_full, u_cut) = (url("/unframed/50000"), url("/unframed/50000?cut=20000"));
        r.check("net.download.unframed", move || {
            let want = file_bytes(50_000);
            let dest = dir.join("unframed.bin");
            let _ = std::fs::remove_file(&dest);
            let dl = Download::new(&u_full, &dest);
            dl.discard_partial();
            let r = dl.run(h, &mut Hooks::default());
            ensure(matches!(&r, Err(NetError::Http(m)) if m.contains("no Content-Length")) && !dest.exists() && dl.partial_bytes() == 50_000, || format!("unconfirmed: {r:?}"))?;
            dl.discard_partial();
            let out = dl.clone().checksum(Checksum::Sha256(sha256_hex(&want))).run(h, &mut Hooks::default()).map_err(e2s)?;
            ensure(out.bytes == 50_000, || format!("{out:?}"))?;
            let _ = std::fs::remove_file(&dest);
            let cut = Download::new(&u_cut, &dest).expected_len(Some(50_000));
            cut.discard_partial();
            let r = cut.run(h, &mut Hooks::default());
            ensure(matches!(&r, Err(NetError::Http(m)) if m.contains("after 20000 bytes")) && !dest.exists(), || format!("cut: {r:?}"))?;
            cut.discard_partial();
            let mut detail = String::from("no length, no chunking: refused without confirmation (.part kept), renamed with SHA-256; cut at 20000 of expected 50000: refused");
            if let (Some(hb), Some(pem)) = (https, pem) {
                let c = UreqClient::with_ca_pem(&pem).map_err(e2s)?;
                let tls = Download::new(format!("{}/unframed/50000?cut=20000", hb.trim_end_matches('/')), &dest);
                tls.discard_partial();
                let r = tls.run(&c, &mut Hooks::default());
                ensure(matches!(&r, Err(NetError::Http(m)) if m.contains("after 20000 bytes")) && !dest.exists(), || format!("TLS cut: {r:?}"))?;
                tls.discard_partial();
                detail.push_str("; HTTPS cut without close_notify: refused");
            }
            Ok(detail)
        });
        let u = url("/files/1000");
        r.check("net.download.offline_refused", move || {
            let offline = || crate::connectivity::wifi_rule(true, "PENDING");
            let mut hooks = Hooks { connectivity: Some(&offline), ..Default::default() };
            match Download::new(&u, std::env::temp_dir().join("never.bin")).run(h, &mut hooks) {
                Err(NetError::Offline(m)) => Ok(format!("simulated cmState PENDING: {m}")),
                other => Err(format!("expected Offline, got {other:?}")),
            }
        });
        let (wifi, keep, dir) = (self.wifi, self.keep_awake, self.scratch.clone());
        let u = url("/files/400000");
        r.check("net.download.device_hooks", move || {
            let activity = NetActivity::new();
            let act2 = activity.clone();
            let mut busy_seen = false;
            let dest = dir.join("hooks.bin");
            let mut hooks = Hooks {
                connectivity: wifi,
                keep_awake: keep,
                activity: Some(&activity),
                on_progress: Some(Box::new(|_p: &DownloadProgress| busy_seen |= !act2.may_auto_suspend())),
                ..Default::default()
            };
            let r = Download::new(&u, &dest).run(h, &mut hooks);
            drop(hooks);
            let _ = std::fs::remove_file(&dest);
            let hooks_desc = format!("wifi hook {}, keep-awake {}", if wifi.is_some() { "on" } else { "off" }, if keep.is_some() { "on" } else { "off" });
            match r {
                Ok(o) => {
                    ensure(busy_seen && !activity.busy(), || "activity not tracked".into())?;
                    Ok(format!("{hooks_desc}: downloaded {} bytes; auto-suspend blocked during the transfer, allowed after", o.bytes))
                }
                Err(NetError::Offline(m)) if wifi.is_some() => Ok(format!("{hooks_desc}: refused while offline ({m})")),
                Err(e) => Err(format!("{hooks_desc}: {e}")),
            }
        });
        match self.keep_awake {
            Some(k) => r.check("net.keep_awake", move || {
                let activity = NetActivity::new();
                let idle = activity.on_ready_to_suspend(k).map_err(e2s)?;
                ensure(!idle, || "deferred while idle".into())?;
                let _g = activity.begin(&CancelToken::new(), None);
                match k.defer_suspend(Duration::from_secs(60)) {
                    Ok(()) => Ok("idle: no deferral; busy: powerd accepted deferSuspend 60".into()),
                    Err(NetError::Unsupported(m)) => Ok(format!("idle: no deferral; busy: hook reached powerd, nothing to defer now ({m})")),
                    Err(e) => Err(e.to_string()),
                }
            }),
            None => r.skip("net.keep_awake", "no keep-awake hook (pass --lipc on the Kindle)"),
        }

        // OPDS.
        let dir = self.scratch.clone();
        let (root, opds2) = (url("/opds/root.xml"), url("/opds2/catalog.json"));
        r.check("net.opds", move || {
            let c = OpdsClient::new(h);
            let feed = c.fetch(&root).map_err(e2s)?;
            ensure(feed.kind() == FeedKind::Navigation && feed.entries.len() == 2, || format!("root {feed:?}"))?;
            let nav = feed.entries[0].navigation().ok_or("no navigation link")?.href.clone();
            let pages = c.fetch_pages(&nav, 5).map_err(e2s)?;
            let books: Vec<_> = pages.iter().flat_map(|p| p.entries.iter()).collect();
            ensure(pages.len() == 2 && books.len() == 4, || format!("{} pages, {} books", pages.len(), books.len()))?;
            let results = c.search(&feed, "moby dick").map_err(e2s)?;
            ensure(results.entries.len() == 2, || format!("search gave {:?}", results.entries.iter().map(|e| &e.title).collect::<Vec<_>>()))?;
            let book = books[3];
            let link = book.best_acquisition().ok_or("no acquisition")?;
            let size = link.length.ok_or("no length")? as usize;
            let dl = c.download_for(book, link, &dir).checksum(Checksum::Sha256(sha256_hex(&file_bytes(size))));
            let out = dl.run(h, &mut Hooks::default()).map_err(e2s)?;
            let name = out.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // The non-ASCII name must come back unchanged from the directory listing.
            let listed: Vec<String> = std::fs::read_dir(&dir).map_err(|e| e.to_string())?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            ensure(listed.contains(&name), || format!("'{name}' not in the listing {listed:?}"))?;
            let _ = std::fs::remove_file(&out.path);
            let f2 = c.fetch(&opds2).map_err(e2s)?;
            ensure(f2.version == OpdsVersion::Json2 && f2.entries.len() == 3, || format!("opds2 {f2:?}"))?;
            Ok(format!("navigation -> 2 acquisition pages (4 books) -> OpenSearch 'moby dick' (2 hits) -> downloaded '{name}' ({size} bytes, SHA-256 ok, name listed back intact); OPDS 2 feed with {} entries", f2.entries.len()))
        });

        // KOSync.
        let server = url("/kosync");
        r.check("net.kosync", move || {
            let user = format!("selftest{}", std::process::id());
            let c = KoSyncClient::new(h, KoSyncAccount::new(&server, &user, "selftest-password"));
            c.register().map_err(e2s)?;
            c.authorize().map_err(e2s)?;
            let doc = "0123456789abcdef0123456789abcdef";
            ensure(c.pull(doc).map_err(e2s)?.is_none(), || "server had progress before the push".into())?;
            let p = Progress {
                document: doc.into(),
                progress: "/body/DocFragment[12]/body/div/p[7]/text().31".into(),
                percentage: 0.53219,
                device: "rinky".into(),
                device_id: crate::kosync::new_device_id(),
                timestamp: None,
                metadata: None,
            };
            c.push(&p).map_err(e2s)?;
            let got = c.pull(doc).map_err(e2s)?.ok_or("nothing pulled")?;
            ensure(got.progress == p.progress && got.percentage == 0.5321 && got.device_id == p.device_id, || format!("pulled {got:?}"))?;
            let bad = KoSyncClient::new(h, KoSyncAccount::new(&server, &user, "wrong"));
            ensure(matches!(bad.authorize(), Err(NetError::Unauthorized(_))), || "wrong password accepted".into())?;
            Ok(format!("register/auth/push/pull as {user}: {} at {}; wrong password -> 401", got.progress, got.percentage))
        });
        let _ = std::fs::remove_dir(&self.scratch);
        r.out
    }
}

/// Runs checks, collects their results and shows each as it finishes.
struct Runner<'f> {
    out: Vec<CheckResult>,
    on_result: &'f mut dyn FnMut(&CheckResult),
}

impl Runner<'_> {
    fn check(&mut self, name: &str, f: impl FnOnce() -> Result<String, String>) {
        let t = Instant::now();
        let outcome = Some(f());
        self.record(CheckResult { name: name.into(), outcome, skipped: None, millis: t.elapsed().as_millis() });
    }

    fn skip(&mut self, name: &str, why: &str) {
        self.record(CheckResult { name: name.into(), outcome: None, skipped: Some(why.into()), millis: 0 });
    }

    fn record(&mut self, r: CheckResult) {
        (self.on_result)(&r);
        self.out.push(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testserver::{State, TestServer, TEST_CA_PEM};
    use std::sync::Arc;

    #[test]
    fn selftest_passes_against_the_mock_server() {
        let state = Arc::new(State::default());
        let plain = TestServer::bind("127.0.0.1:0", false, state.clone()).unwrap();
        let tls = TestServer::bind("127.0.0.1:0", true, state).unwrap();
        let online = || Ok(());
        let keeps = std::cell::Cell::new(0);
        let keep = |_d: Duration| {
            keeps.set(keeps.get() + 1);
            Ok(())
        };
        let t = NetSelftest {
            http_base: plain.base(),
            https_base: Some(tls.base()),
            ca_pem: Some(TEST_CA_PEM.to_vec()),
            wifi: Some(&online),
            keep_awake: Some(&keep),
            scratch: std::env::temp_dir().join(format!("ink-net-selftest-{}", std::process::id())),
        };
        let results = t.run(|r| eprintln!("{}", r.line()));
        let failed: Vec<_> = results.iter().filter(|r| r.failed()).map(CheckResult::line).collect();
        assert!(failed.is_empty(), "{failed:#?}");
        assert!(results.iter().all(|r| r.outcome.is_some()), "nothing skipped");
        assert!(keeps.get() >= 2);
        assert_eq!(results.len(), 18);
    }

    #[test]
    fn offline_hook_refuses_the_device_download() {
        let s = TestServer::start().unwrap();
        let offline = || crate::connectivity::wifi_rule(true, "PENDING");
        let t = NetSelftest { http_base: s.base(), https_base: None, ca_pem: None, wifi: Some(&offline), keep_awake: None, scratch: std::env::temp_dir().join(format!("ink-net-selftest-off-{}", std::process::id())) };
        let results = t.run(|_| {});
        let hooks = results.iter().find(|r| r.name == "net.download.device_hooks").unwrap();
        assert!(matches!(&hooks.outcome, Some(Ok(d)) if d.contains("refused while offline")), "{}", hooks.line());
        let wifi = results.iter().find(|r| r.name == "net.wifi_hook").unwrap();
        assert!(wifi.line().contains("PENDING"), "{}", wifi.line());
        assert!(results.iter().find(|r| r.name == "net.https.verified").unwrap().line().starts_with("SKIP"));
    }
}
