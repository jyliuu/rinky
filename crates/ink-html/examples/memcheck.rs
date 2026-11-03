//! Memory-integrity check for the emulator corruption:
//! plain copies into freshly allocated memory and `Vec` growth by `push`, with no
//! ink-html code involved. Every byte is checked; differing runs are printed.
//!   memcheck ROUNDS MB THREADS
use std::time::Instant;

fn pattern(i: usize) -> u8 {
    (i.wrapping_mul(2_654_435_761) >> 13) as u8
}

/// (offset, length) of each run of differing bytes, at most 8.
fn runs(a: &[u8], b: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < a.len() && out.len() < 8 {
        if a[i] != b[i] {
            let s = i;
            while i < a.len() && a[i] != b[i] {
                i += 1;
            }
            out.push((s, i - s));
        }
        i += 1;
    }
    out
}

fn worker(t: usize, rounds: usize, mb: usize) -> usize {
    let n = mb << 20;
    let src: Vec<u8> = (0..n).map(pattern).collect();
    let mut bad = 0;
    for r in 0..rounds {
        // 1. Copy into untouched memory (a large allocation is a fresh mapping).
        let mut dst: Vec<u8> = Vec::with_capacity(n);
        dst.extend_from_slice(&src);
        if dst != src {
            bad += 1;
            println!("thread {t} round {r}: copy differs: {:?}", runs(&dst, &src));
        }
        drop(dst);
        // 2. A Vec<u32> grown by push (reallocations copy it again and again).
        let mut v: Vec<u32> = Vec::new();
        for i in 0..(n / 4) as u32 {
            v.push(i.wrapping_mul(2_246_822_519));
        }
        if let Some(i) = (0..v.len()).find(|&i| v[i] != (i as u32).wrapping_mul(2_246_822_519)) {
            bad += 1;
            let k = (i..v.len()).take_while(|&j| v[j] != (j as u32).wrapping_mul(2_246_822_519)).count();
            println!("thread {t} round {r}: push-grown Vec<u32> differs at index {i} ({k} entries, {} bytes)", k * 4);
        }
    }
    bad
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let rounds: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(50);
    let mb: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(16);
    let threads: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    let t0 = Instant::now();
    let hs: Vec<_> = (0..threads).map(|t| std::thread::spawn(move || worker(t, rounds, mb))).collect();
    let bad: usize = hs.into_iter().map(|h| h.join().unwrap()).sum();
    println!("memcheck rounds {rounds} x {mb} MB x {threads} threads: {bad} bad rounds in {:?}", t0.elapsed());
}
