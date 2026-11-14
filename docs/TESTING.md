# Testing

Which checks to run when you change something, and how to run the slower ones: host
tests, golden images, the simulator, the seeded monkey test, emulator scenarios and
installs on a device.

## What to run when

| When | Run |
| --- | --- |
| While working on one crate | `cargo test -p <crate>` and `cargo clippy -p <crate> --all-targets -- -D warnings` |
| Before every commit | `scripts/test-fast.sh` and `cargo clippy --workspace --all-targets -- -D warnings` |
| You changed how pages, the reader footer or settings rows look | `cargo test -p ink-html --test golden` for pages (not in `test-fast.sh`); the footer and settings goldens run in `test-fast.sh`. Regenerate only on purpose, see [Golden images](#golden-images) |
| You changed C dependencies, `unsafe` code or anything `cfg(target_os = "linux")` | `scripts/clippy-arm.sh` (clippy for the ARM target) |
| Before a merge | `cargo test --workspace` (also runs the attack, hostile-input and fixture suites) |
| You changed the launcher, sleep, input or anything the device does | the emulator scenarios for that area, see [Emulator scenarios](#emulator-scenarios) |
| Before a release | everything above, the [slow tests](#slow-tests), `monkey_daily`, the emulator smoke set, then the full scenario set |

Set `CARGO_BUILD_JOBS` when the machine is shared, and point `INK_MUPDF_CACHE` at a
directory with a built MuPDF to skip its C build (about a minute per target on a cold
cache; see `crates/ink-mupdf/build.rs`).

## Host tests

`scripts/test-fast.sh` is the daily run: the unit tests of every crate (`--lib`), then
the integration tests of ink-app (every screen driven headless, the golden images, the
CLI). It skips every integration suite of the library crates, the page goldens of
ink-html among them, and the heavier ones (`crates/*/tests/verify_*.rs`, `fixtures.rs`
and the like). With arguments it runs only the unit-test step, with those arguments
added: `scripts/test-fast.sh -p ink-ui`. `TEST_THREADS` sets the test threads
(default 4).

`cargo test --workspace` runs every test that is not `#[ignore]`, including those
suites. Library crates set `doctest = false` (none has a doctest), so there is no
rustdoc step; `cargo test --doc` still runs them if one is added.

Useful single suites:

```sh
cargo test -p ink-app --test screens            # every screen headless with scripted input
cargo test -p ink-html --test golden            # page goldens
cargo test -p ink-app plugin_host               # plugin host and overlays (INK_PLUGIN_SHOTS=DIR writes screenshots)
cargo test -p ink-html --test verify_hostile    # hostile HTML: deep nesting, huge images, broken markup
```

### Ignored tests

An `#[ignore]` carries a reason that says what the test is (two by-hand tests have a
bare `#[ignore]` and say how to run them in their doc comment:
`ui_font_render_png` in ink-app and `internet_gutenberg_over_webpki_roots` in ink-net):

| Reason starts with | Meaning | Run it |
| --- | --- | --- |
| `slow:` | a heavy version of a test that has a smaller daily twin (fuzz sweeps, timing ratios, memory measurements, inputs of tens of MB) | before a release, [commands below](#slow-tests) |
| `verifier:` | reproduces a defect that is still open; expected to fail | when working on that defect |
| `measurement:`, `informational:`, `heavy:` | prints numbers, does not pass or fail on them | by hand, alone |
| `child body`, `child process entry point`, `probe` | started by another test or by an emulator scenario | never directly |
| `file-system property:` | documents a property of the Kindle's user store | by hand |

So `cargo test --workspace -- --ignored` is not a pass/fail check. Use the slow-test
commands instead.

### Slow tests

```sh
cargo test -p ink-html --lib -- --ignored big_sources_match_a_whole_decode_all_formats prefix_layouts_match_the_full_layout_big_books
cargo test -p ink-html --test verify_hostile -- --ignored \
  deep_soup_nesting_100k unbroken_word_time_is_not_quadratic image_10000x10000_gray_memory \
  image_10000x10000_rgb_memory single_paragraph_of_5mb spine_of_5000_items \
  many_paragraphs_build_in_linear_time many_script_tags_parse_in_linear_time \
  toc_with_thousands_of_entries_time_ratio rapid_relayout_while_paginating \
  relayout_there_and_back_returns_to_the_same_page_from_three_starts \
  background_page_map_matches_foreground_layout_juliet \
  corrupt_images_do_not_panic_every_mutation odd_chapter_content_opens_full_size
cargo test -p ink-formats --test verify_hostile -- --ignored fuzz_mutations txt_50mb epub_zip_bomb_chapter epub_zip_bomb_cover
cargo test -p ink-formats --test fixtures -- --ignored damaged_files_do_not_panic_full
cargo test -p ink-mupdf --test verify_attacks -- --ignored byte_flip_fuzz_does_not_crash \
  memory_levels_off_over_many_cbz_pages cbz_large_scan_page_memory_is_measured \
  a_2000_page_pdf_opens_searches_and_lists_its_toc_in_time
cargo test -p ink-mupdf --test verify_attacks_2 -- --ignored xobject_fanout_page_time_and_memory_are_measured
cargo test -p ink-store --lib -- --ignored --exact verify_tests::verifier_library_truncated_index_uses_backup_every_7th_byte verify_tests::verifier_sidecar_truncated_at_every_byte \
  verify2_tests::verifier2_koreader_import_highlights_scale_linearly verify2_tests::verifier2_koreader_import_bookmarks_scale_linearly \
  verify2_tests::verifier2_koreader_legacy_import_scales_linearly
cargo test -p ink-text --test verify_text_cycle2 -- --ignored real_collections_and_every_7th_truncation
```

The timing-ratio tests (`*_in_linear_time`, `*_not_quadratic`, `spine_of_5000_items`,
`toc_with_thousands_of_entries_time_ratio`, the 2000-page PDF) compare wall-clock
times. Run them on an idle machine; another build or test run on the same machine can
make them fail. `deep_soup_nesting_100k` takes about 100 s on its own.

The emulator scenarios that run these test binaries on the ARM guest include the slow
tests: `html-tests-arm` and `formats-selftest` pass `--include-ignored`, and
`mupdf-verify-attacks` runs the fuzz test and the measurements with `--ignored`.

## Writing tests that don't flake

Rules for new tests. Each comes from a test that failed on a loaded machine and was fixed:

- Read only files the test wrote itself, or files under `testdata/` and `tests/`. Never
  read a file of the source checkout such as `Cargo.toml`: a branch switch during a long
  run can rewrite it.
- A test that starts `rinky sim` or `rinky demo` sets `TMPDIR` to its own directory. The
  binary keeps its state in `$TMPDIR/rinky-sim`, which is otherwise shared with other test
  runs and with your own sim sessions.
- No wall-clock bound that a busy machine can exceed while the code is right. Check what
  the bound stands for instead: a log line, a counter, a per-thread count. A script sleep
  that must outlast a poll needs margin (the control-center tile test sleeps 7 s for a 4 s
  state wait plus a 2 s tile poll).
- Something that must happen inside a bounded wait (ink-store's 127 ms settle wait) runs
  from a hook at that wait (`on_settle_wait` in test builds), not from a thread that
  sleeps first.
- The reader's status bars redraw on the wall-clock minute. The reader test harness moves
  a tick that is not yet due out of reach, so screen-update counts stay exact; a test that
  wants the tick sets `bars.next_tick` to now.

Still slow (worth a look): `update::tests::verifier_two_concurrent_updates_leave_one_consistent_version`
(15-18 s) and `settings::tests::without_device_services_every_page_opens` (about 11 s).

## Golden images

Four sets, all exact pixel matches:

| Set | Test | Goldens | What it draws |
| --- | --- | --- | --- |
| Page content | `crates/ink-html/tests/golden.rs` | `tests/golden/*.png` | book pages through ink-formats at 1272 x 1696, content only |
| Reader footer | `crates/ink-app/tests/footer_golden.rs` | `crates/ink-app/tests/golden/footer-*.png` | the status bar band for fixed page totals and two font sizes; also checks that `screens::reader::render_page` equals page content plus footer |
| Settings rows | `crates/ink-app/tests/settings_golden.rs` | `crates/ink-app/tests/golden/settings-*.png` | the settings popup and page, day and night |
| Reader end card | `crates/ink-app/tests/end_card_golden.rs` | `crates/ink-app/tests/golden/end-card*.png` | the card shown past the last page: not finished, finished with a two-line title, nothing to suggest |

The page goldens use ink-formats as a dev-dependency of ink-html. ink-formats depends
on ink-html, but integration tests link the same library, so the cycle is harmless.
`tests/golden/pagemaps.txt` records the page breaks of the golden books. Page maps
are cached on the device under `ink_html::paginate::ENGINE_VERSION`, so when page
breaks change the golden test fails until that version is bumped; bump it, then
regenerate.

When a change alters pixels on purpose:

```sh
scripts/golden-update.sh [DIFF_DIR]     # default DIFF_DIR: target/golden-diff
```

It regenerates all four sets and writes a `*.diff.png` for every golden that changed.
A `DIFF_DIR` you pass must be new or empty; the script refuses a directory with other
files in it and never empties one (only the default under `target/` is cleared). Look
at every diff image, commit the goldens with the change that caused them, and say in
the commit why the pixels changed. Never regenerate to make a failing test pass
without knowing why it failed.

Variables: `INK_GOLDEN_UPDATE=1` rewrites goldens, `INK_GOLDEN_OUT` is where actual and
diff images go (default `target/golden-out`), `INK_GOLDEN_DIR` reads page goldens from
another directory. `scripts/png-compare.py A B` compares two PNGs or two directories of
them.

## Simulator and headless runs

The `rinky` binary runs on the host. Without a window it writes PNGs, which is how
screen tests and screenshots are made:

```sh
cargo run -p ink-app -- render testdata/juliet.epub OUT 6        # first 6 pages as PNG
cargo run -p ink-app -- sim testdata/juliet.epub                 # window: mouse = touch
cargo run -p ink-app -- sim testdata/juliet.epub --script tests/sim/turn-pages.txt
cargo run -p ink-app -- sim BOOK_OR_DIR --headless --script FILE --out DIR [--frames]
cargo run -p ink-app -- demo --headless --script crates/ink-app/src/screens/demo/gallery.txt --out DIR
```

`sim` takes `--ghost` (simulated ghosting), `--panel WxH` (another screen size, for
example `1072x1448` for a PW4) and `--shots DIR`. A directory instead of a book opens
the library on it. The script language (taps, swipes, holds, keys, power events,
`wait`, `shot NAME`) is documented in `crates/ink-device/src/script.rs`. Headless
`wait` steps advance a virtual clock, so timers fire without real waiting.

The simulator keeps its state (settings, library index, plugins) in
`$TMPDIR/rinky-sim`. Plugin authors: see `docs/plugins.md`, "Testing".

## Monkey test

`crates/ink-app/tests/monkey.rs` writes random but plausible input scripts (taps,
holds, swipes including edge swipes, two-finger gestures, page and menu keys, powerd
events, rare power key presses) and runs `rinky sim DIR --headless --script` on them
over a folder of EPUB, TXT, FB2, AZW3, PDF and CBZ books. One seed is several scripts
run one after another on the same state directory, as if the launcher started the app
again. On macOS the binary runs under `sandbox-exec` with the network denied.

After each run it checks: exit status 0 with an `exit: reason=` log line, no
`crash-*.log`, no hang (the log grows at least every `MONKEY_HANG_SECS`), and that
`settings.toml`, the library index and every sidecar still load.

```sh
cargo test --release -p ink-app --test monkey -- --ignored monkey_daily                  # 3 seeds, seconds in release
cargo test --release -p ink-app --test monkey -- --ignored --nocapture monkey_long       # seeds 1..21
MONKEY_SEEDS=1..200 MONKEY_BIN=/path/to/copy/of/rinky \
  cargo test --release -p ink-app --test monkey -- --ignored --nocapture monkey_long
MONKEY_MINIMISE=<work>/seed-N/seg-K.txt \
  cargo test --release -p ink-app --test monkey -- --ignored --nocapture monkey_minimise
```

`monkey_daily` takes minutes in a debug build, which is why it is not in
`test-fast.sh`. A failure prints the seed, the segment and the directory with the
scripts and the state from before that segment. `monkey_minimise` shrinks a failing
script (ddmin, same failure kind) to `minimised.txt`. Other options are in the header
of the file (`MONKEY_STYLE`, `MONKEY_STRICT_COVERAGE`, `MONKEY_LOGS`, `MONKEY_KEEP`).
Copy the binary before a long run: on macOS, overwriting a running binary kills the
process with signal 9.

## Emulator scenarios

`tests/e2e/*.sh` are scenarios that run on the eink-emulator (QEMU with Amazon's
firmware 5.19.5, a jailbroken PW12 image), driven by `scripts/e2e/run.sh`. They
install the ARM build, drive it with taps, swipes and power presses, and check
screenshots and `rinky.log`.

### Slots

Several people or agents can share one machine, so emulator instances run in slots.
Use only `scripts/emu-slot.sh`, never the emulator by hand:

```sh
scripts/emu-slot.sh up NAME           # take a slot, create from the 5.19.5 base, boot (about 5 min), jailbreak, Home
scripts/emu-slot.sh deploy NAME       # build with zigbuild, copy binary, launcher, watchdog, test books (--no-build: last build)
scripts/emu-slot.sh ssh NAME 'cmd'    # also: push, pull, shot, tap, swipe, hold, power, log, wifi
scripts/emu-slot.sh restart NAME      # fresh boot in the same slot
scripts/e2e/run.sh NAME OUTDIR [PATTERN]
scripts/emu-slot.sh down NAME         # stop QEMU and give the slot back; do this on failure too
scripts/emu-slot.sh status | cleanup | port NAME | slotdir NAME
```

There are at most 4 slots; slot N listens for SSH on port 4110 + 10N. If none is free,
do host-side work and try `up` again later (a failed `up` gives back a slot it took).
Every boot uses QEMU's `-snapshot`: guest writes vanish when QEMU exits, so boot once
and redeploy many times. The locking rules (atomic `mkdir`, `flock`, what `cleanup`
may remove) and the environment variables (`EMU`, `EMU_SLOTS`, `EMU_TIME_SCALE`,
deadlines) are described at the top of `scripts/emu-slot.sh`. Its own tests:
`scripts/e2e/slot-test.sh`, `scripts/e2e/verify-slot-races.sh` and
`scripts/emu-slot-test.sh`, all against a fake emulator.

Notes on driving the guest:

- `tap`, `swipe` and `hold` send QMP pointer events, which the emulator's touch model
  turns into multi-touch. `power` runs `rinky inject power` in the guest. The emulated
  USB cable keeps the kernel from suspending, so real sleep and wake can only be tested
  on a device.
- `wifi` joins the emulator's open `Kindle-QEMU` network; the host is then `10.0.2.2`
  (mock servers bound to 127.0.0.1 on the host). Without Wi-Fi the host is
  `192.168.15.201` over USB networking.
- Scenarios leave rinky with `exit_device` from `scripts/e2e/lib.sh` (`rinky-ctl.sh
  stop`, the shell form of "Exit to Kindle"). The power key puts rinky to sleep; it
  does not exit.

### Running scenarios

`scripts/e2e/run.sh NAME OUTDIR [PATTERN]` runs the matching scenarios on slot NAME
and writes `OUTDIR/summary.txt`. It copes with a slow host and a tired guest: a
scenario declares `# e2e-cycles: N`, and run.sh restarts the guest before a scenario
that would push it past `E2E_MAX_CYCLES` (12) framework restarts. A scenario that
exits 75 (guest not ready) or times out is retried once after a restart; one that
exits 77 checked nothing and is listed as SKIP (for example `plugin-tls` without
`INK_E2E_INTERNET=1`). `E2E_TIME_SCALE` scales every deadline (default: from the boot
time and the machine load). `scripts/e2e/run-test.sh` tests these rules.

Each scenario writes its screenshots (`NN-label.png`), logs and `summary.txt` under
OUTDIR. Look at the screenshots before claiming what they show.

**Smoke set.** `scripts/e2e/smoke.sh NAME [OUTDIR]` runs ten scenarios that cover the
main paths, on a slot that is up and deployed; `--up NAME` also brings the slot up,
deploys and takes it down.

| Scenario | Path it covers |
| --- | --- |
| `arch-launcher-normal` | Home, launcher stops the framework, rinky, page turn, exit, Home |
| `library-smoke` | library scan, covers, list, details, DRM book error screen, exit |
| `wave-b-merge-smoke` | library, book, reader menu, font size, library, exit |
| `reader-menu-smoke` | reader menu, Go to, Aa panel, bookmark, search, selection |
| `arch-reader-gestures` | tap, swipes, long press through the emulator touch path |
| `formats-books` | FB2, TXT (cp1251) and AZW3 open and turn pages |
| `mupdf-pages` | PDF and CBZ pages rendered by the ARM build |
| `settings-smoke` | settings screen, brightness, saved settings |
| `device-reader-sleep` | power key: screensaver with the page, wake, page turns |
| `dict-smoke` | dictionary plugin: long press, look up, popup, vocabulary row |

**Full set.** `scripts/e2e/run.sh NAME OUTDIR` with no pattern runs every scenario.
Pick a pattern for the area you changed:

| Area | Scenarios |
| --- | --- |
| Launcher, framework stop and start | `arch-launcher-*`, `arch-appmgrd-alert`, `arch-cli-tools`, `arch-suspend-spike`, `device-launcher-*`, `p0fix-verify-*`, `device-fix1-*` |
| Reader and library | `arch-reader-gestures`, `reader-menu-smoke`, `library-smoke`, `wave-b-merge-smoke`, `b-verify-*`, `settings-smoke`, `stats-record` |
| Device (sleep, night, rotation, USB, Wi-Fi) | `device-selftest`, `device-sleep`, `device-reader-sleep`, `device-night`, `device-rotate`, `device-usb`, `device-wifi`, `device-verify1-*` |
| Engines and test binaries on ARM | `html-*`, `formats-*`, `mupdf-*`, `ui-*`, `store-*` |
| Packaging and update | `packaging-*` (`packaging-update-running` updates a running install from an older release, `PKG`, to this tree's package, `PKG_NEW`; see its header) |
| Plugins and network | `dict-smoke`, `plugin-*`, `net-selftest`, `c-sync-kosync` (mock servers on the host); `plugin-tls` fetches from the internet; `plugin-native-crash` needs `cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p native-faulty -p sample-native` first (else it is skipped) |

Host-only scripts in `scripts/e2e/` need no emulator: `launcher-race-test.sh`
(launcher and watchdog against a stub upstart), `attack-inputs.sh` (malformed books
and odd arguments against the binary) and `phase0-host.sh` (all of them in one run).

## Installing on a device or a slot

`scripts/deploy.sh HOST [PORT]` builds with the `deploy` profile (no LTO, incremental),
packages, copies and restarts rinky. It compares the sha256 of every packaged file with
the device's `/mnt/us/rinky/SHA256SUMS` and sends only the files that differ, usually
the binary, `VERSION` and `SHA256SUMS`. Each file lands next to its target, is checked,
and only then renamed over the old one, so a running rinky never has its binary
overwritten. The restart (`rinky-ctl.sh restart`) asks the launcher to start the new
binary without bringing the Kindle framework back. `--full` sends the whole package
through `install.sh --upgrade`, which also happens by itself when the file list or
`install.sh` changed. `--release` builds with fat LTO, as release packages are. The
options are listed at the top of the script.

On an emulator slot the host key changes with every boot, so pass the key and skip the
host check:

```sh
scripts/emu-slot.sh up my-jb
scripts/emu-slot.sh deploy my-jb          # first install: sends the full package once
DEPLOY_SSH_OPTS="-i ../eink-emulator/build/ssh/id_ed25519 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR" \
  scripts/deploy.sh 127.0.0.1 "$(scripts/emu-slot.sh port my-jb)"
```

On the device itself, `rinky selftest` checks the store, and with options the device
layer, lipc, a book and the network (`rinky selftest --device --net URL`; the full
list is in `rinky` without arguments). `rinky device help` lists the device-layer test
tools.
