# Testing

Which tests run when, how long they take, and how to run the slow ones. Each section
says which branch wrote it; the integrator merges them.

## Host tests: library crates and integration suites (branch v3/prune-libs)

Scope: every crate except the ink-app unit tests (`cargo test -p ink-app --lib` has its
own section).

### What to run

| When | Command | Typical time |
|---|---|---|
| Day to day | `scripts/test-fast.sh` | unit tests of every crate + ink-app integration tests |
| Before a merge | `cargo test --workspace` | see "Numbers" below |
| Before a release | `cargo test --workspace` plus the slow tests below with `--ignored` | several minutes |

A plain `cargo test --workspace` runs every unit and integration test that is not
`#[ignore]`. The heavy cases (fuzzing, timing ratios, memory measurements, inputs of
tens of MB, every-byte truncation sweeps) are kept, but marked
`#[ignore = "slow: run with --ignored before a release"]`. Most of them have a smaller
daily twin that runs the same code path on fewer cases.

Other `#[ignore]` reasons already in the tree mean something else:
`verifier: ...` reproduces a defect that is still open (expected to fail),
`measurement: ...` only prints numbers, `child process entry point` / `probe` are run by
other tests or by the emulator. So `cargo test --workspace -- --ignored` is not a pass/fail
release check; use the commands below.

### The slow tests (release run)

```sh
cargo test -p ink-html --lib -- --ignored big_sources_match_a_whole_decode_all_formats prefix_layouts_match_the_full_layout_big_books
cargo test -p ink-html --test verify_hostile -- --ignored \
  deep_soup_nesting_100k unbroken_word_time_is_not_quadratic image_10000x10000_gray_memory \
  image_10000x10000_rgb_memory single_paragraph_of_5mb spine_of_5000_items \
  many_paragraphs_build_in_linear_time many_script_tags_parse_in_linear_time \
  toc_with_thousands_of_entries rapid_relayout_while_paginating \
  relayout_there_and_back_returns_to_the_same_page_from_three_starts \
  background_page_map_matches_foreground_layout_juliet \
  corrupt_images_do_not_panic_every_mutation odd_chapter_content_opens_full_size
cargo test -p ink-formats --test verify_hostile -- --ignored fuzz_mutations txt_50mb epub_zip_bomb_chapter epub_zip_bomb_cover
cargo test -p ink-formats --test fixtures -- --ignored damaged_files_do_not_panic_full
cargo test -p ink-mupdf --test verify_attacks -- --ignored byte_flip_fuzz_does_not_crash \
  memory_levels_off_over_many_cbz_pages cbz_large_scan_page_memory_is_measured \
  a_2000_page_pdf_opens_searches_and_lists_its_toc_in_time
cargo test -p ink-mupdf --test verify_attacks_2 -- --ignored xobject_fanout_page_time_and_memory_are_measured
cargo test -p ink-store --lib -- --ignored --exact verify_tests::verifier_library_truncated_index_uses_backup_every_7th_byte verify_tests::verifier_sidecar_truncated_at_every_byte
cargo test -p ink-text --test verify_text_cycle2 -- --ignored real_collections_and_every_7th_truncation
```

The timing-ratio tests (`*_in_linear_time`, `*_not_quadratic`, `spine_of_5000_items`,
`toc_with_thousands_of_entries`, the 2000-page PDF) compare wall-clock times: run them
on an idle machine. `toc_with_thousands_of_entries` failed once in the baseline run
below while another agent's tests loaded the machine.

### What changed (v3/prune-libs)

Fixed:
- `ink-formats/tests/fixtures.rs`: 4 to 6 tests failed depending on timing. They read
  TOC pages and page counts right after `set_layout` while the background paginator
  still ran (`e.page.unwrap()` on `None`). They now call `wait_paginated(120 s)` first
  (`render_first_page`, `epub3_nav_page_list_metadata_fonts`,
  `provider_opens_every_readable_format`). Ran 4 times (once with 6 threads, 3 times with 16): all pass.

Made smaller or faster (same check, fewer cases or less waste):
- ink-html unit tests: the 4-megapixel image source is built once (`OnceLock`) and the
  png / jpeg / adam7 checks are three parallel tests at one size each; PNGs are encoded
  with fast compression. `long_book` is 600 paragraphs (24 pages, was 60).
  `prefix_layouts_match_the_full_layout` checks 4 small books (3 formats).
- ink-html `verify_hostile.rs`: smaller daily versions of `corrupt_images_do_not_panic`
  (every 8th mutation), `odd_chapter_content_opens` (big repeat counts / 10),
  `relayout_there_and_back_returns_to_the_same_page` (one start page),
  `background_page_map_matches_foreground_layout` (the fixtures without juliet).
- ink-html `golden.rs`: the 15 golden cases run on one thread each.
- ink-formats `damaged_files_do_not_panic`: 5 damaged copies per fixture (was 10).
- ink-ui `verify_ui_cycle2.rs`: rigs reuse a warm `Fonts` (glyph cache) instead of
  loading a fresh one per rig; panel copies and pixel diffs work on row slices.
- ink-text `real_collections_and_every_truncation`: lengths below 128 and every 97th.
- ink-store: the library-index and sidecar truncation sweeps cut at every length below
  64 and every 53rd / 13th after.
- ink-device `hub_backs_off_a_watcher_that_dies_after_each_line`: watches 4.5 s, not
  8 s (at most 3 runs with back-off; a hub without back-off gives 5, checked).
- `[lib] doctest = false` in the 17 library manifests (none has a doctest; ink-net has
  one example marked `ignore`): saves the empty rustdoc step, about 12 s per run.
  `cargo test --doc` still runs them.

Merged:
- `ink-html/tests/document.rs` `positions_are_stable_across_font_sizes` removed: a
  weaker copy of `verify_hostile.rs` `positions_are_exact_across_font_sizes`, which now
  also checks that each page start maps back to its page.

Ignored as slow (the full list is in the commands above): every fuzz sweep, every
timing-ratio test, the memory measurements, the 5 MB / 50 MB / 150 MB inputs,
`deep_soup_nesting_100k` (100 s alone), `rapid_relayout_while_paginating`, and the full
versions of the shrunk tests.

Emulator scripts that run these test binaries on the ARM guest keep running the tests
now ignored on the host: `html-tests-arm.sh` and `formats-selftest.sh` pass
`--include-ignored` (html also `--skip malformed_vs_crengine`), and
`mupdf-verify-attacks.sh` runs `byte_flip_fuzz_does_not_crash` and the measurements
with `--ignored`. The golden test runs its cases one at a time on ARM.

### Numbers

Measured on 2026-11-01 with debug builds, each test binary run alone with
`--test-threads=6`. Another agent ran the ink-app tests on the same machine during both
measurements (load average about 14 before, 20-45 after), so single numbers vary by
+-30 %.

Per test binary (seconds, wall clock of the binary):

| Binary | Before | After | Change |
|---|---:|---:|---|
| ink-html lib | 28.5 | 3.4 | image matrix split + ignored, smaller doc books |
| ink-html `verify_hostile` | 107.9 (1 failed) | 10.1 | 14 ignored, 4 shrunk |
| ink-html `golden` | 9.4 | 4.8 | cases in parallel |
| ink-html `document` | 3.4 | 3.4 | one duplicate removed |
| ink-formats `verify_hostile` | 29.6 | 3.0 | 4 ignored |
| ink-formats `fixtures` | 13.2 (4-6 failed) | 6.9 | fixed; damaged copies halved |
| ink-mupdf `verify_attacks` | 14.3 | 3.6 | 4 ignored |
| ink-mupdf `verify_attacks_2` | 5.7 | 0.7 | 1 ignored |
| ink-store lib | 9.2 | 7.5 | 2 sweeps shrunk |
| ink-device `verify1_attacks` | 8.3 | 5.2 | 4.5 s back-off window |
| ink-ui `verify_ui_cycle2` | 8.2 | 4.0 | warm fonts |
| ink-text `verify_text_cycle2` | 4.5 | 1.3 | sweep shrunk |
| all other binaries | 40 | 43 | unchanged code; machine load |
| **Sum of all binaries** (with ink-app integration tests, without ink-app lib) | **282** | **97** | |
| Doctest step (17 crates, 0 doctests) | 12 | 0 | `doctest = false` |

Plain `cargo test --workspace --exclude ink-app`, `CARGO_BUILD_JOBS=4`, whole command
timed (build + every test binary). Before: with `--no-fail-fast`; without it the run
stopped at the failing `fixtures.rs` after 47 s.

| Run | Before | After |
|---|---:|---:|
| warm build (nothing to compile) | 268 s, 2 binaries failed | 85.5 s, 41 binaries pass (load 17) |
| after touching `crates/ink-html/tests/verify_hostile.rs` | - | 100 s (4 s build; load 17-36) |
| after touching `crates/ink-html/src/layout.rs` | - | 85 s (13 s build) |
| empty target directory | build alone 132 s (`--no-run`, with ink-app) | 154 s (58 s build) |

So a plain run takes about 1.5 minutes with a warm build and about 2.5 minutes from an
empty target directory on this machine under the load above (another agent's tests).

Still the longest after the change: ink-html `verify_hostile` (two juliet relayout
tests, about 7 s each), ink-store lib (sqlite and rename races), ink-formats
`fixtures` (azw3 pagination, damaged copies), ink-device (the 4.5 s back-off window).

Not changed here, but would help (the workspace `Cargo.toml` belongs to the
integrator): `[profile.dev.package."*"] opt-level = 1`, or `[profile.dev.package.<name>]`
`opt-level = 2` for image, png, zune-jpeg, zip, flate2 and fontdue, would make image decoding,
inflate and font rasterising in tests several times faster, at the cost of a longer
first build of those dependencies.

## Emulator end-to-end tests (tests/e2e)

76 scenario scripts (`tests/e2e/*.sh`, plus 2 `*.sh.disabled`), run on the emulator
through `scripts/e2e/run.sh`.

### Smoke set (10 scenarios)

`scripts/e2e/smoke.sh NAME [OUTDIR]` runs these on one slot (NAME up and deployed), or
`scripts/e2e/smoke.sh --up NAME` brings the slot up, deploys, runs and takes it down.
Each runs through `scripts/e2e/run.sh` (same restarts, retries and cycle budget).

| Scenario | Path it covers |
|---|---|
| `arch-launcher-normal` | Home -> launcher stops the framework -> rinky -> page turn -> exit -> Home |
| `library-smoke` | library scan, covers, list, details, DRM book error screen, exit |
| `wave-b-merge-smoke` | library -> book -> reader menu -> font size -> library -> exit |
| `reader-menu-smoke` | reader menu, Go to, Aa panel, bookmark, search, selection |
| `arch-reader-gestures` | tap, swipes, long press through the emulator touch path |
| `formats-books` | FB2, TXT (cp1251) and AZW3 open and turn pages |
| `mupdf-pages` | PDF and CBZ pages rendered by the ARM build |
| `settings-smoke` | settings screen, brightness, saved settings |
| `device-reader-sleep` | power key: screensaver with the page, wake, page turns |
| `dict-smoke` | dictionary plugin: long press, look up, popup, vocabulary row |

### Full release set

Every `tests/e2e/*.sh`: `scripts/e2e/run.sh NAME` (no pattern). Groups:

- Launcher and framework stop/start: `arch-launcher-*` (incl. `arch-launcher-restart`), `arch-appmgrd-alert`,
  `arch-cli-tools`, `arch-suspend-spike`, `device-launcher-*`, `p0fix-verify-*`,
  `device-fix1-*`.
- Reader and library: `arch-reader-gestures`, `reader-menu-smoke`, `library-smoke`,
  `wave-b-merge-smoke`, `b-verify-*`, `settings-smoke`, `stats-record`.
- Device: `device-selftest`, `device-sleep`, `device-reader-sleep`, `device-night`,
  `device-rotate`, `device-usb`, `device-wifi`, `device-verify1-*`.
- Engines on the ARM build: `html-render`, `html-selftest`, `html-tests-arm`,
  `formats-books`, `formats-kfx-refusal`, `formats-selftest`, `mupdf-pages`,
  `mupdf-selftest`, `mupdf-verify-attacks`, `mupdf-verify2-attacks`, `ui-arm-tests`,
  `ui-gallery`, `ui-verify-*`, `store-selftest`, `store-verify`, `store-verify2`.
- Packaging: `packaging-*`.
- Plugins and network: `dict-smoke`, `plugin-sample`, `plugin-broken`, `plugin-net`,
  `net-selftest`, `c-sync-kosync`, `packaging-update` (mock servers on the host),
  `plugin-tls` (skips unless `INK_E2E_INTERNET=1`; fetches from gutenberg.org).

## Deploying to a device or an emulator slot (`scripts/deploy.sh`)

`scripts/deploy.sh HOST [PORT]` builds (profile `deploy`), packages, copies and
restarts. By default it compares the sha256 of every packaged file with the device's
installed `/mnt/us/rinky/SHA256SUMS` and sends only the files that differ (normally
`rinky`, `VERSION`, `SHA256SUMS`) as one gzip tar. On the device each file is placed
as `<target>.fast<pid>`, checked with sha256, and only then renamed over the old one
(`SHA256SUMS` last), so a running rinky never has its binary overwritten. The full
package path (`install.sh --upgrade`) runs instead when the device has no
`SHA256SUMS`/`FILES`/`.install-state`, when the file list differs, when `install.sh`
changed, or with `--full`. Every step prints its time.

The binary is almost all of the package (about 12.7 MB of 13 MB with the `deploy`
profile), so the fast path saves the `install.sh` work and a few hundred KB, not the
binary transfer. Measured on an emulator slot (one-line change in `ink-app`, build
10-13 s): `--full` copy 4.1 s + install 27.1 s; fast path pack 1.2 s + copy 2.2 s +
install 21.3 s (most of the install time was stopping and starting rinky, about
15 s on the emulator: stop restored the Kindle framework, start stopped it again).
The restart now uses `rinky-ctl.sh restart`: ctl writes `/tmp/rinky/restart` and
sends SIGTERM to rinky only; the launcher (`rinky.sh`) sees the file after
rinky exits, undoes the journaled settings, copies the installed binary to
`/tmp/rinky/bin` and runs it again. `lab126_gui` is not touched. The watchdog keeps
running; if the new binary fails to start or crashes, the launcher ends and starts the
framework as before, and if the launcher dies the watchdog does it. A launcher from
before this change (no `/tmp/rinky/restart-capable`) or no running rinky makes
`restart` do stop + start. deploy.sh also polls for the new "start: rinky" log line
instead of sleeping 12 s. Measured on the emulator, fast path, one-line change:
install + restart 16.2 s -> 3.0 s, whole deploy.sh run 34.0 s -> 8.8 s (build 3 s).
Over Wi-Fi the copy time counts; a smaller transfer needs a
binary delta, and the Kindle has no tool to apply one.

On an emulator slot (the host key changes, so pass the key and no host check):

```sh
scripts/emu-slot.sh up my-jb
scripts/emu-slot.sh deploy my-jb          # first install; it has no SHA256SUMS, so deploy.sh sends the full package once
DEPLOY_SSH_OPTS="-i ../eink-emulator/build/ssh/id_ed25519 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR" \
  scripts/deploy.sh 127.0.0.1 "$(scripts/emu-slot.sh port my-jb)"
```

Scenarios leave rinky with `exit_device` (defined in `scripts/e2e/lib.sh`):
`rinky-ctl.sh stop` over SSH, the shell form of "Exit to Kindle", then wait until
rinky is gone (its exit status is then 143, not 0). There is no top-right exit corner
any more, so do not `tap 1200 60`; and the power key puts rinky to sleep, it does
not exit. Only `arch-suspend-spike` still taps the corner: its `spike-suspend`
subcommand has its own.

## Monkey test: seeded random input

`crates/ink-app/tests/monkey.rs` writes random but plausible input scripts (taps, quick
taps, holds, swipes incl. edge swipes for the control center and Aa, two-finger taps and
pinches, page and menu keys, powerd events, raw down/move/up, waits; rare `key power`)
and runs the `rinky sim DIR --headless --script` binary on them. Books in DIR: EPUB
(3 files), TXT, FB2, AZW3, PDF, CBZ. One seed = 8 scripts of 250 events, run one after the
other on the same state directory (like the launcher starting the app again). The binary
runs under `sandbox-exec` with network access denied (macOS). After each run it checks:
exit status 0 and an `exit: reason=` line, no `crash-*.log`, no hang (the log must grow
at least every `MONKEY_HANG_SECS`), and `settings.toml`, the library index and every
sidecar load.

```sh
cargo test --release -p ink-app --test monkey -- --ignored monkey_daily  # 3 seeds x 3 x 100 events, 8 s (2 min in debug, so not in test-fast)
cargo test --release -p ink-app --test monkey -- --ignored --nocapture monkey_long   # seeds 1..21
MONKEY_SEEDS=1..200 MONKEY_BIN=/path/to/copy/of/rinky cargo test --release -p ink-app --test monkey -- --ignored --nocapture monkey_long
MONKEY_MINIMISE=<work>/seed-N/seg-K.txt cargo test --release -p ink-app --test monkey -- --ignored --nocapture monkey_minimise
```

A failure prints the seed, the segment, and the directory with the scripts and the state
from before that segment; `monkey_minimise` shrinks the script by ddmin (same failure kind
required) and writes `minimised.txt`. Options: `MONKEY_STYLE=2` (more tap bursts),
`MONKEY_STRICT_COVERAGE=1` (a frame with pixels outside every update rect fails),
`MONKEY_LOGS=DIR`, `MONKEY_KEEP=1`; see the header of the file.
Copy the `rinky` binary before a long run (do not overwrite a running binary on macOS:
`rm` first, or the old process is killed with signal 9).
