//! Emulator diagnosis: a passive
//! victim process. `memsentinel SECONDS MB HELD` keeps HELD buffers of MB MiB each (fresh
//! allocations, so fresh pages). A new buffer is filled with 0xA5, then with 0x5A, and
//! checked at once ("immediate"); every held buffer is checked again on each pass
//! ("delayed"); the oldest one is then replaced. Every bad run is printed with its value,
//! page offset and when it was seen:
//! - 0xA5 in a run: our second write was lost (rolled back);
//! - 0x00: a zero write from outside, or a rollback to the fresh zero page;
//! - seen only "delayed": something wrote the page after our check.
//!
//! No other work runs in this process (one thread), so a bad run comes from outside it.
use std::time::{Duration, Instant};

/// Physical address of the page holding `addr`, from /proc/self/pagemap (needs root);
/// "?" when it cannot be read.
fn phys(addr: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open("/proc/self/pagemap") else {
        return "?".into();
    };
    let mut e = [0u8; 8];
    if f.seek(SeekFrom::Start((addr / 4096 * 8) as u64)).is_err() || f.read_exact(&mut e).is_err() {
        return "?".into();
    }
    let v = u64::from_le_bytes(e);
    if v >> 63 == 0 || v & ((1 << 55) - 1) == 0 {
        return "?".into();
    }
    format!("{:#x}", (v & ((1 << 55) - 1)) * 4096 + (addr % 4096) as u64)
}

fn check(buf: &[u8], when: &str, round: usize) -> usize {
    let mut bad = 0;
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == 0x5A {
            i += 1;
            continue;
        }
        let s = i;
        while i < buf.len() && buf[i] != 0x5A {
            i += 1;
        }
        bad += 1;
        let addr = buf.as_ptr() as usize + s;
        let hex: Vec<String> = buf[s..i.min(s + 48)]
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect();
        println!(
            "sentinel: {when} round {round}: {} bad bytes at offset {s} addr {addr:#x} phys {} addr%32 {} addr%4096 {:#x}: {}",
            i - s,
            phys(addr),
            addr % 32,
            addr % 4096,
            hex.join(" ")
        );
    }
    bad
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let secs: u64 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    let mb: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let held: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(4).max(1);
    let n = mb << 20;
    let t0 = Instant::now();
    let mut bufs: Vec<Vec<u8>> = Vec::new();
    let (mut rounds, mut bad_now, mut bad_later) = (0usize, 0usize, 0usize);
    while t0.elapsed() < Duration::from_secs(secs) {
        let mut b = vec![0xA5u8; n];
        std::hint::black_box(&mut b);
        b.fill(0x5A);
        std::hint::black_box(&mut b);
        bad_now += check(&b, "immediate", rounds);
        for old in &bufs {
            bad_later += check(old, "delayed", rounds);
        }
        if bufs.len() == held {
            bufs.remove(0);
        }
        bufs.push(b);
        rounds += 1;
        std::thread::sleep(Duration::from_millis(200));
    }
    for old in &bufs {
        bad_later += check(old, "delayed", rounds);
    }
    println!(
        "sentinel: {rounds} buffers of {mb} MiB in {:.0} s: {bad_now} bad runs seen at once, {bad_later} seen later",
        t0.elapsed().as_secs_f64()
    );
}
