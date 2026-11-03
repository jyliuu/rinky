//! Test guard for the eink-emulator PW6 guest. The emulator maps a 32-byte MMIO "CFA mailbox" over
//! guest RAM at physical 0x400000d0..0x400000ef (`MT8113_HWTCON_CFA_MAILBOX_ADDR`), but
//! the guest kernel uses physical page 0x40000000 as ordinary RAM. Whichever process gets
//! that page reads and writes those 32 bytes through the mailbox, not RAM.
//!
//! `emuguard` (root, Linux): allocates memory until /proc/self/pagemap shows that this
//! process owns physical page 0x40000000, keeps only that page (mlock;
//! vm.compact_unevictable_allowed=0 so compaction does not move it), checks the mailbox
//! window against a control window with word accesses, and holds the page until killed,
//! so no other process gets it. It searches until it has the page, in 64 MiB sweeps:
//! back to back for the first 120 s, then one sweep every 10 s. At most once a minute it
//! drops the clean page cache (`drop_caches` 1), because a page holding cached file data
//! only goes back to the free lists that way; a page held by a process comes back when
//! that process frees it. Lines start with "emuguard:"; the last one is
//! "emuguard: holding" while the guard is active.
#[cfg(target_os = "linux")]
mod imp {
    use std::io::{Read, Seek, SeekFrom};
    use std::time::{Duration, Instant};

    const TARGET_PFN: u64 = 0x40000; // physical 0x40000000
    const PAGE: usize = 4096;
    const CHUNK: usize = 16 << 20;
    const MAX_HELD: usize = 4; // chunks held at once while searching (64 MiB)

    unsafe extern "C" {
        fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
        fn munmap(addr: *mut u8, len: usize) -> i32;
        fn mlock(addr: *const u8, len: usize) -> i32;
    }
    const PROT_READ: i32 = 1;
    const PROT_WRITE: i32 = 2;
    const MAP_PRIVATE: i32 = 2;
    const MAP_ANONYMOUS: i32 = 0x20;

    fn pfns(pagemap: &mut std::fs::File, addr: usize, pages: usize) -> Vec<u64> {
        let mut raw = vec![0u8; pages * 8];
        if pagemap
            .seek(SeekFrom::Start((addr / PAGE * 8) as u64))
            .is_err()
            || pagemap.read_exact(&mut raw).is_err()
        {
            return vec![0; pages];
        }
        raw.as_chunks::<8>()
            .0
            .iter()
            .map(|e| {
                let v = u64::from_le_bytes(*e);
                if v >> 63 == 1 { v & ((1 << 55) - 1) } else { 0 }
            })
            .collect()
    }

    /// Maps and touches one chunk; null on failure.
    fn chunk() -> *mut u8 {
        // SAFETY: an anonymous private mapping; the kernel picks the address.
        let p = unsafe {
            mmap(
                std::ptr::null_mut(),
                CHUNK,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p as isize == -1 {
            return std::ptr::null_mut();
        }
        for i in (0..CHUNK).step_by(PAGE) {
            // SAFETY: p..p+CHUNK is our mapping; offset 0 of each page is ordinary RAM.
            unsafe { p.add(i).write_volatile(1) };
        }
        p
    }

    /// Looks for the target page until found; returns its address in this process (the
    /// rest unmapped).
    fn catch(pagemap: &mut std::fs::File) -> *mut u8 {
        let t0 = Instant::now();
        let mut note_at = 30;
        let mut dropped_at: Option<Instant> = None;
        let mut held: Vec<*mut u8> = Vec::new();
        loop {
            let p = chunk();
            if p.is_null() {
                std::thread::sleep(Duration::from_millis(200));
            } else {
                let found = pfns(pagemap, p as usize, CHUNK / PAGE)
                    .iter()
                    .position(|&f| f == TARGET_PFN);
                if let Some(i) = found {
                    let page = unsafe { p.add(i * PAGE) };
                    // SAFETY: unmapping parts of our own mappings, never the kept page.
                    unsafe {
                        if i > 0 {
                            munmap(p, i * PAGE);
                        }
                        if i + 1 < CHUNK / PAGE {
                            munmap(page.add(PAGE), CHUNK - (i + 1) * PAGE);
                        }
                        for q in held.drain(..) {
                            munmap(q, CHUNK);
                        }
                        if mlock(page, PAGE) != 0 {
                            println!(
                                "emuguard: mlock failed: {}",
                                std::io::Error::last_os_error()
                            );
                        }
                    }
                    return page;
                }
                held.push(p);
            }
            if held.len() >= MAX_HELD {
                for q in held.drain(..) {
                    // SAFETY: our own chunk mappings.
                    unsafe { munmap(q, CHUNK) };
                }
                if dropped_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
                    let _ = std::fs::write("/proc/sys/vm/drop_caches", "1");
                    dropped_at = Some(Instant::now());
                }
                let pause = if t0.elapsed() < Duration::from_secs(120) {
                    100
                } else {
                    10_000
                };
                std::thread::sleep(Duration::from_millis(pause));
                if t0.elapsed().as_secs() >= note_at {
                    println!(
                        "emuguard: still looking after {} s (another owner holds the page)",
                        t0.elapsed().as_secs()
                    );
                    note_at += 60;
                }
            }
        }
    }

    /// Word writes and reads on 0xd0..0xe8 (the mailbox) and 0x200..0x218 (control).
    /// Word 7 (0xec) is not written: on the mailbox it starts a CFA capture.
    fn probe(page: *mut u8) -> (Vec<u32>, Vec<u32>) {
        let pattern = 0x5a5a_5a5a_u32;
        let run = |off: usize| {
            // SAFETY: page is our mapped page; offsets are word aligned and inside it.
            unsafe {
                let w = page.add(off) as *mut u32;
                for k in 1..7 {
                    w.add(k).write_volatile(pattern);
                }
                w.write_volatile(pattern);
                (0..7)
                    .map(|k| w.add(k).read_volatile())
                    .collect::<Vec<u32>>()
            }
        };
        (run(0xd0), run(0x200))
    }

    pub fn main() {
        match std::fs::write("/proc/sys/vm/compact_unevictable_allowed", "0") {
            Ok(()) => println!("emuguard: vm.compact_unevictable_allowed=0"),
            Err(e) => println!("emuguard: cannot set vm.compact_unevictable_allowed: {e}"),
        }
        let mut pagemap = match std::fs::File::open("/proc/self/pagemap") {
            Ok(f) => f,
            Err(e) => {
                println!("emuguard: cannot open /proc/self/pagemap: {e}");
                std::process::exit(1);
            }
        };
        let t0 = Instant::now();
        let mut page = catch(&mut pagemap);
        let (mailbox, control) = probe(page);
        let hex = |v: &[u32]| {
            v.iter()
                .map(|w| format!("{w:08x}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        println!(
            "emuguard: caught physical page 0x40000000 at {:#x} after {:.1} s; word check (RAM reads 5a5a5a5a x7): 0xd0..0xeb {}; control 0x200..0x21b {}",
            page as usize,
            t0.elapsed().as_secs_f64(),
            hex(&mailbox),
            hex(&control)
        );
        println!("emuguard: holding");
        loop {
            std::thread::sleep(Duration::from_secs(2));
            if pfns(&mut pagemap, page as usize, 1)[0] != TARGET_PFN {
                println!("emuguard: page moved; catching it again");
                // SAFETY: our own one-page mapping.
                unsafe { munmap(page, PAGE) };
                page = catch(&mut pagemap);
                println!("emuguard: holding");
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    imp::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("emuguard: Linux only");
    std::process::exit(2);
}
