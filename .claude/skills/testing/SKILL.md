---
name: testing
description: Choose and run rinky's tests - fast host run, full workspace run, the slow ignored tests before a release, golden images, simulator screenshots, the monkey test, and emulator end-to-end scenarios through emu-slot.sh. Use when asked to test, verify, check a change, run CI locally, update goldens, or debug a failing or flaky test.
argument-hint: "[crate or scenario]"
---

# Testing rinky

Full background: `docs/TESTING.md` (suites, timings, the slow-test list) and
`docs/ARCHITECTURE.md` "Emulator slots", "Golden tests". This skill says which to run
when. Always set the environment from `AGENTS.md` first (`INK_MUPDF_CACHE`,
`CARGO_BUILD_JOBS`).

## Pick the level

| When | Run | Time (warm build) |
|---|---|---|
| While editing one crate | `cargo test -p <crate>` or one test by path | seconds |
| Before each commit | `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo test -p <crate>` | under a minute |
| Before handing over | `cargo clippy --workspace --all-targets -- -D warnings` + `scripts/test-fast.sh` | 1-3 min |
| Before a merge (what CI runs) | `cargo test --workspace` (CI adds `--locked --no-fail-fast`) | ~1.5 min warm, ~2.5 cold |
| Before a release | the above, the slow tests, emulator smoke set | several minutes + emulator |

Single tests:

```sh
cargo test -p ink-app --lib plugins::tests::list_loads      # unit test by module path
cargo test -p ink-html --test verify_hostile -- deep_xml      # integration binary, name filter
cargo test -p ink-app --test screens -- --nocapture           # every screen headless
```

`scripts/test-fast.sh` = `cargo test --workspace --lib` (4 threads, `TEST_THREADS`
changes it) + `cargo test -p ink-app --test '*'`. With arguments
(`scripts/test-fast.sh -p ink-ui`) it runs only those unit tests.

## Slow tests (release)

Marked `#[ignore = "slow: run with --ignored before a release"]`. Run the exact
commands in `docs/TESTING.md` "The slow tests (release run)", on an idle machine (the
timing-ratio tests compare wall-clock times). Do **not** use
`cargo test --workspace -- --ignored` as a pass/fail check: `verifier: ...` ignored
tests reproduce open defects and are expected to fail, and `measurement: ...` ones
only print numbers.

## Failing or flaky tests

1. Run the one test alone (`-- --exact <path> --test-threads=1`).
2. A timing test that fails under load (`*_in_linear_time`, `*_not_quadratic`,
   `toc_with_thousands_of_entries`) is not a regression until it fails alone on an
   idle machine. Check `uptime`.
3. Tests that start `rinky` as a child (monkey, CLI) need a fresh binary; never
   overwrite a running binary on macOS (`rm` first).
4. Never mark a failing test `#[ignore]` or loosen an assertion to get green; report it.

## Golden images

Four sets, exact match locally (CI tolerates 32 pixels per page):

- page content: `tests/golden/*.png` (`crates/ink-html/tests/golden.rs`)
- reader footer: `crates/ink-app/tests/golden/footer-*.png` (`footer_golden.rs`)
- settings page and popup: `crates/ink-app/tests/golden/settings-*.png` (`settings_golden.rs`)
- reader end card: `crates/ink-app/tests/golden/end-card*.png` (`end_card_golden.rs`)

When a pixel change is intended:

```sh
scripts/golden-update.sh out/evidence/<topic>/golden-diff   # new or empty dir; refuses one with files
```

Look at every `*.diff.png` it lists (changed pixels red or black). Commit the goldens
with the code that changed them and say in the commit body what changed and why. An
unexplained golden diff is a regression: fix the code, do not regenerate.

## Simulator screenshots (host, no emulator)

```sh
cargo run -p ink-app -- sim testdata/juliet.epub --headless --script tests/sim/turn-pages.txt --out OUT
cargo run -p ink-app -- demo --headless --script tests/sim/demo.txt --out OUT
```

Script steps: `tap X Y`, `swipe ...`, `hold ...`, `key next|prev|menu|back|home|power`, `wait MS`,
`shot NAME` (see `tests/sim/*.txt` and `crates/ink-device/src/script.rs`). Panel
coordinates are 1272 x 1696. `--frames` writes every update. Open the PNGs and look.

## Monkey test (random input)

```sh
cargo test --release -p ink-app --test monkey -- --ignored monkey_daily         # ~8 s release
cargo test --release -p ink-app --test monkey -- --ignored --nocapture monkey_long
```

Failures print a seed and a directory; `MONKEY_MINIMISE=<dir>/seed-N/seg-K.txt ... monkey_minimise`
shrinks the script. Options are in the header of `crates/ink-app/tests/monkey.rs`.

## Emulator end-to-end (ARM build in QEMU)

Needs `../eink-emulator` and a free slot (at most 4, shared by all agents). Use only
`scripts/emu-slot.sh`; never the instance `pw12-5195-jb` or ports 4022/4023.

```sh
scripts/e2e/smoke.sh --up <key>-jb out/evidence/<key>/smoke   # up, deploy, 10 scenarios, down
# or by hand:
scripts/emu-slot.sh up <key>-jb                # ~5 min boot, jailbreak, Home
scripts/emu-slot.sh deploy <key>-jb            # zigbuild + copy (--no-build: copy last build)
scripts/e2e/run.sh <key>-jb out/evidence/<key>/e2e 'reader-*'
scripts/emu-slot.sh shot <key>-jb out/evidence/<key>/now.png
scripts/emu-slot.sh log <key>-jb 200
scripts/emu-slot.sh down <key>-jb              # always, also after a failure
```

- If no slot is free, do host work and retry `up` later (a failed `up` gives its slot
  back). `scripts/emu-slot.sh status` shows who holds what.
- Exit codes: 0 pass, 77 skip (nothing checked), 75 guest not ready (retried once).
- The full release set is every `tests/e2e/*.sh`: `scripts/e2e/run.sh NAME OUTDIR`.
- The emulator cannot show real sleep/wake (the USB wakeup source is always active),
  e-ink waveform timing, or real Wi-Fi radios. List those under "Not tested on a
  device".
- A new scenario: `tests/e2e/<area>-<what>.sh` using `scripts/e2e/lib.sh`, a
  `# e2e-cycles: N` line, `exit_device` to leave rinky, screenshots `NN-label.png`.

## Done when

You can state, with the summary lines as evidence: which commands ran, pass/fail
counts, any skipped scenarios and why, and what remains untested.
