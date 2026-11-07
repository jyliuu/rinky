# AGENTS.md

How to work in this repository. This file is for coding agents and is the single
source of truth for them; `CLAUDE.md` imports it, and `CONTRIBUTING.md` is the short
human version. The code layout is in `docs/ARCHITECTURE.md`, the test suites in
`docs/TESTING.md`, the plugin API in `docs/plugins.md`.

rinky is an e-book reader for jailbroken Kindles (and Kobos), written in Rust. One
static ARMv7 musl binary (`rinky`, crate `ink-app`) replaces the Kindle framework
while it runs. On a Mac or Linux host the same binary runs as a simulator (`rinky
sim`), and most tests run headless on the host.

## Layout

```text
crates/ink-device    screen, input, light, battery, power, Wi-Fi traits; kindle/kobo/sim backends
crates/ink-text      fonts, glyph cache, shaping, line breaking, hyphenation
crates/ink-ui        widgets, damage, refresh scheduling, gestures, keyboard, dialogs
crates/ink-doc       Document/Provider traits, positions, layout settings
crates/ink-html      reflowable engine (EPUB/HTML/FB2/MOBI content), pagination
crates/ink-formats   container formats into ink-html; DRM/KFX refused
crates/ink-mupdf     PDF/CBZ/XPS through MuPDF (C, built with zig by build.rs)
crates/ink-store     sidecars, library index, settings.toml, statistics DB
crates/ink-dict      StarDict reader
crates/ink-net       HTTP client (ureq + rustls), SSE, test server
crates/ink-plugin    Plugin trait, registries, host services, Lua 5.4 host
crates/ink-app       the `rinky` binary: event loop, screens, plugin host, CLI, update
plugins/<name>       first-party plugins, one crate each (sample-lua holds the Lua sample)
packaging/           KUAL extension, install/uninstall scripts
scripts/             build, package, deploy, emulator, e2e, golden and perf scripts
tests/golden/        page goldens   tests/e2e/ emulator scenarios   tests/sim/ sim scripts
testdata/            test books (juliet.epub, synthetic.epub, formats/)
docs/                ARCHITECTURE, TESTING, plugins, user-guide, install, DESIGN
```

Dependencies point one way: device <- text <- ui, doc <- html <- formats, and
everything <- ink-app (full graph in `docs/ARCHITECTURE.md`). Screens live in
`crates/ink-app/src/screens/<name>/`; compiled-in plugins are listed in
`crates/ink-app/src/plugins.rs`.

## Environment

Set these in every shell that builds:

```sh
export INK_MUPDF_CACHE=<main checkout>/target/mupdf-cache  # in a worktree: reuse the main checkout's MuPDF build
export CARGO_BUILD_JOBS=6     # the machine is shared with other agents; ARCHITECTURE.md's "2" is older advice
```

- `crates/ink-mupdf/build.rs` builds MuPDF from a checksummed tarball with zig the
  first time (about a minute per target; needs zig, curl, a tar that reads lzip, make)
  and caches it in `$INK_MUPDF_CACHE`, else in `<target dir>/mupdf-cache`. The main
  checkout needs no variable; a git worktree (`.claude/worktrees/*`) has its own empty
  target directory, so point the variable at the main checkout's cache.
- Host toolchain: stable Rust (CI uses `dtolnay/rust-toolchain@stable`).
  `rust-version = "1.91"` in `Cargo.toml` is declared, not tested.
- Kindle build: zig 0.17 and cargo-zigbuild (installed; do not reinstall), target
  `armv7-unknown-linux-musleabihf`. `.cargo/config.toml` sets the CPU flags.
- Try `cargo --offline` first; drop `--offline` only if a crate is missing.

## Commands

All verified from the repository root.

```sh
cargo build -p ink-app                                   # host debug build of rinky
cargo run --release -p ink-app -- sim testdata/juliet.epub   # simulator window
cargo run -p ink-app -- sim testdata/juliet.epub --headless --script tests/sim/turn-pages.txt --out OUT
cargo test -p ink-ui                                     # one crate
cargo test -p ink-app --lib plugins::tests::list_loads   # one unit test (path filter)
cargo test -p ink-html --test golden                     # one integration test binary
scripts/test-fast.sh                                     # day to day: every crate's unit tests + ink-app integration tests
scripts/test-fast.sh -p ink-store                        # unit tests of one crate, 4 threads
cargo test --workspace                                   # before a merge (what CI runs)
cargo clippy -p ink-ui --all-targets -- -D warnings      # one crate
cargo clippy --workspace --all-targets -- -D warnings    # whole workspace (CI)
scripts/clippy-arm.sh                                    # clippy for the ARM target (needs one zigbuild first)
scripts/golden-update.sh out/evidence/<key>/golden-diff  # regenerate goldens + diff images
cargo zigbuild --profile deploy --target armv7-unknown-linux-musleabihf -p ink-app   # Kindle binary, quick (--release: fat LTO, ~1.5 min more)
scripts/package.sh --allow-dirty --profile deploy --out OUT  # Kindle + Kobo packages, quick build (CI uses the fat-LTO release profile)
```

- The slow tests (`#[ignore = "slow: ..."]`) run before a release with the exact
  commands in `docs/TESTING.md`. `cargo test --workspace -- --ignored` is **not** a
  pass/fail check: other ignored tests reproduce open defects on purpose.
- Timing-ratio tests (`*_in_linear_time`, `*_not_quadratic`) compare wall-clock times
  and fail on a loaded machine. Re-run them alone before calling them broken.
- Golden tests are exact on this machine; CI allows 32 pixels per page
  (`INK_GOLDEN_TOLERANCE=32`) for float rounding on Linux.
- On macOS, never overwrite a `rinky` binary that is running: `rm` it first or copy it
  (`cp target/release/rinky <scratch>/rinky-x`). Overwriting kills the process with
  signal 9.

## Emulator and devices

- The emulator is `../eink-emulator` (QEMU). Use **only** `scripts/emu-slot.sh`
  (`up NAME`, `deploy NAME`, `tap`, `shot`, `log`, `down NAME`); at most 4 slots are
  shared by every agent. Always `down` the slot you took, also after a failure.
- Never touch the instance `pw12-5195-jb` or ports 4022/4023: it is the user's.
- Scenarios: `scripts/e2e/smoke.sh --up NAME` (10 scenarios, brings the slot up and
  down) or `scripts/e2e/run.sh NAME OUTDIR 'pattern-*'`. Details in
  `docs/TESTING.md` and `docs/ARCHITECTURE.md` "Emulator slots".
- Never contact a real Kindle or Kobo (`scripts/deploy.sh`, `scripts/device.sh`)
  unless the user asks for it in this conversation.
- Evidence (screenshots, logs) goes to `out/evidence/<key>/` (gitignored). Look at a
  screenshot before saying what it shows.

## Where changes go

- A new screen: `docs/ARCHITECTURE.md` "How to add a screen". A new plugin: the
  `new-plugin` skill. A feature in general: the `new-feature` skill.
- Do not change or remove a public item of another crate without updating every
  caller in the same commit. Prefer an additive change in the crate you are in.
- New third-party crates go into `[workspace.dependencies]` in the root `Cargo.toml`
  with an exact version, and members use `foo.workspace = true`. Every new crate must
  build for the ARM target (`cargo zigbuild ...`).
- `#[ignore]` reasons mean something: `slow: ...` (release run), `verifier: ...` (an
  open defect, expected to fail), `measurement: ...` (prints numbers only).
- Do not edit `docs/*.md` to describe your own process; keep them about the product
  and the code.

## Commits, branches, PRs

- Branches: `release/X.Y.Z` for a release, `fix/<topic>`, `feat/<topic>`,
  `docs/<topic>`. Never commit to `main`; never force-push a branch with an open PR.
- One concern per commit. Subject: `Area: a plain statement of what changes`, for
  example `Reader: a long press selects the whole of a word hyphenated at a line end`.
  Area is a screen or crate the user would recognise (Reader, Library, Dictionary,
  Battery page, Keyboard, ink-html, CI). No conventional-commit prefixes.
- Body: plain prose, wrapped at about 72 columns, saying why: what was wrong or
  missing, and what the change does about it.
- A change the user can see gets a `CHANGELOG.md` line in user language (not the
  commit subject) under the topmost unreleased heading (`## [Unreleased]` or
  `## [X.Y.Z] - Unreleased`), in Added / Changed / Fixed. Several commits may share
  one entry. Update `docs/user-guide.md` when behaviour described there changes.
- Never bump the version in a feature commit; only the "Release X.Y.Z" commit does.
- PR body: one bold heading per commit with its short hash and a few lines or bullets,
  then `**Checks:**` (what ran) and `**Not tested on a device:**` (what only real
  hardware can show). PR #19 is a good example.

## Done means

1. `cargo clippy --workspace --all-targets -- -D warnings` passes.
2. `scripts/test-fast.sh` passes (and `cargo test --workspace` before a merge).
3. New behaviour has a test next to the code it changes (unit test in the crate, a
   screen test in `crates/ink-app/tests/screens.rs`, or a golden).
4. Goldens changed only on purpose, regenerated with `scripts/golden-update.sh`, and
   the commit says which pixels changed and why.
5. Changelog and user guide updated for user-visible changes.
6. Your report names the commands you ran and their result, and what was not tested
   (device-only behaviour, the emulator if you did not run it).
