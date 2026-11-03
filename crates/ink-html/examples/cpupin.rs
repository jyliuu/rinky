//! Emulator diagnosis: the Kindle guest
//! has no `taskset`. `cpupin CPU PROGRAM [ARGS...]` sets the CPU affinity of this process
//! to the one CPU number CPU and execs PROGRAM; the mask is inherited by the program and
//! by every thread it starts. Linux only.
#[cfg(target_os = "linux")]
fn main() {
    use std::os::unix::process::CommandExt;
    unsafe extern "C" {
        // musl and glibc: int sched_setaffinity(pid_t, size_t, const cpu_set_t *).
        fn sched_setaffinity(pid: i32, size: usize, mask: *const u64) -> i32;
    }
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: cpupin CPU PROGRAM [ARGS...]");
        std::process::exit(2);
    }
    let cpu: u32 = a[1].parse().expect("CPU number");
    let mask: [u64; 16] = std::array::from_fn(|i| {
        if i as u32 == cpu / 64 {
            1u64 << (cpu % 64)
        } else {
            0
        }
    });
    // SAFETY: the mask is a live 128-byte array; pid 0 is this process.
    let r = unsafe { sched_setaffinity(0, std::mem::size_of_val(&mask), mask.as_ptr()) };
    if r != 0 {
        eprintln!(
            "cpupin: sched_setaffinity({cpu}) failed: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }
    let e = std::process::Command::new(&a[2]).args(&a[3..]).exec();
    eprintln!("cpupin: exec {}: {e}", a[2]);
    std::process::exit(1);
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cpupin: Linux only");
    std::process::exit(2);
}
