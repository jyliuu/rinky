---
name: perf-measure
description: Measure rinky's speed, CPU and memory before and after a change - host benchmark with scripts/perf-bench.sh and perf-summary.py, and page-turn, frame, CPU and RSS numbers on the Kindle emulator from rinky's debug log. Use when a change touches a hot path (page turn, paint, app loop, pagination, library scan, idle wakeups), when asked to profile, benchmark, compare performance or check that something is not slower.
argument-hint: "[what to measure]"
---

# Measure performance: $ARGUMENTS

Numbers are only worth reporting as a before/after pair, built the same way, run on the
same input, back to back, with the machine load written down. Report median and spread
(min-max or 90th percentile) and n, never a single run.

## 1. Two binaries

Build the base and your branch with the same profile and keep copies (other builds
replace `target/...`, and on macOS overwriting a running binary kills it):

```sh
S=<scratch dir>
git worktree add $S/base <base commit>      # or build the base first, then your branch
(cd $S/base && cargo build --release -p ink-app) && cp $S/base/target/release/rinky $S/rinky-base
cargo build --release -p ink-app && cp target/release/rinky $S/rinky-new
```

For the emulator use the ARM target. The `emu` profile (thin LTO, incremental) is
minutes faster than `release` and close enough for comparisons; use `release` for
numbers you publish:

```sh
cargo zigbuild --profile emu --target armv7-unknown-linux-musleabihf -p ink-app
cp target/armv7-unknown-linux-musleabihf/emu/rinky $S/rinky-new-arm   # the same in the base worktree: $S/rinky-base-arm
```

## 2. Host benchmark (no emulator)

```sh
scripts/perf-bench.sh $S/rinky-base $S/perf-base 5
scripts/perf-bench.sh $S/rinky-new  $S/perf-new 5
python3 -I scripts/perf-summary.py $S/perf-base
python3 -I scripts/perf-summary.py $S/perf-new
```

`perf-bench.sh BIN OUTDIR [RUNS]` generates the books once (big36, dune63, a 500-book
library), runs each case RUNS times with an empty sim home, and writes
`OUTDIR/summary.txt` and `OUTDIR/load.txt`. Cases: open without and with the page-map
cache, page turns (`scripts/perf/reader.txt`), menu, Go to, Aa sheet, font size,
relayout, library scan (`rinky perf library`, `scripts/perf/library.txt`). The page-turn
frame includes the headless panel emulation the Kindle does not run; "render page +
bars" is the reader's own time. Run base and new back to back; other agents' builds
change the load (`uptime`).

## 3. Emulator (ARM, closer to the device)

Use a slot (`scripts/emu-slot.sh`, see the `testing` skill). Slot instances are the
Paperwhite 12th gen model on firmware 5.19.5. The per-model machines in
`../eink-emulator/machines/` (`rk-kt6`, `rk-pw3`, ...) belong to the user; use them only
when asked.

```sh
N=<key>-perf
scripts/emu-slot.sh up $N
EMU_DEPLOY_BIN=$S/rinky-base-arm scripts/emu-slot.sh deploy $N --no-build
```

Turn on debug logging: Settings › Diagnostics › log level `debug` (stored as
`[plugins.diagnostics] log_level` in `/mnt/us/rinky/settings.toml`), or
`RINKY_LOG=debug` for a binary started by hand. Then drive the same input on each
binary, e.g. open the same book and do 60 page turns with
`scripts/emu-slot.sh tap $N 1000 900` (right two thirds: next page) (wait a second between taps), and collect:

```sh
scripts/emu-slot.sh pull $N /mnt/us/rinky/rinky.log $S/base-rinky.log
grep -o 'page [0-9]* drawn in [0-9.]* ms' $S/base-rinky.log      # reader draw time
grep -o 'frame time: [0-9]* ms on screen [a-z]*' $S/base-rinky.log   # whole frame
scripts/emu-slot.sh ssh $N 'pid=$(cat /tmp/rinky/rinky.pid); grep -E "VmRSS|Threads" /proc/$pid/status; cat /proc/$pid/stat'
```

CPU time is fields 14+15 (utime+stime, in clock ticks of 1/100 s) of
`/proc/<pid>/stat`; read it before and after the input (or after 2 minutes idle on a
page, to check idle wakeups). Repeat with `EMU_DEPLOY_BIN=$S/rinky-new-arm`, same book,
same taps. `scripts/emu-slot.sh down $N` at the end.

Limits: QEMU timings are not device timings (no e-ink waveform time, a different CPU
speed); compare ratios, not absolute values, and say so. Sleep, real Wi-Fi and the
panel refresh need a real device.

## 4. Report

A table like PR #19's "Measured on the Kindle 12th gen emulator":

```markdown
**Measured on <host or emulator model>** (<profile> builds of <base> and <branch>, same book, same input)

| | base | new |
|---|---|---|
| Page draw, median / 90th pct (61 turns) | 4.9 / 22.3 ms | 4.5 / 13.4 ms |
| rinky CPU, 30 page turns | 0.52 s | 0.53 s |
| Memory (RSS) / threads | 149.6 MB / 10 | 149.8 MB / 11 |
```

Add the method (commands, n, load average) under the table. If the difference is within
the run-to-run spread, say "no measurable change".

## Done when

- Base and new numbers come from the same profile, input and machine, with n and spread.
- The method is written down so someone else can repeat it.
- Any emulator slot you took is down (`scripts/emu-slot.sh status`).
