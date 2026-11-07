---
name: new-feature
description: Add or change a feature or fix a bug in rinky end to end - which crate the code goes in, the tests and goldens that go with it, the user guide and changelog entry, commits and the PR body. Use when asked to implement, add, change or fix behaviour in the reader, library, settings, a device backend, an engine (ink-html, ink-formats, ink-mupdf) or the UI toolkit.
argument-hint: "[short description]"
---

# Feature or fix: $ARGUMENTS

Read `AGENTS.md` first (commands, environment, commit style). This is the order of
work; each step names where things go.

## 1. Branch

`git switch -c feat/<topic> origin/main` (or `fix/<topic>`). If the work is part of an
upcoming release, the user may name a `release/X.Y.Z` branch instead.

## 2. Find the right crate

| The change is about | Code goes in | Tests go in |
|---|---|---|
| A screen (reader, library, settings, dictionary view, battery page) | `crates/ink-app/src/screens/<name>/` | `screens/<name>/tests.rs` or a `#[cfg(test)] mod tests`, plus `crates/ink-app/tests/screens.rs` for scripted runs |
| Reader footer / settings page pixels | `screens/reader/footer.rs`, `screens/settings/` | `crates/ink-app/tests/footer_golden.rs`, `settings_golden.rs` (goldens in `crates/ink-app/tests/golden/`) |
| Page layout, CSS, images, pagination | `crates/ink-html/src/` | unit tests in the module, `crates/ink-html/tests/document.rs`, `golden.rs` (goldens in `tests/golden/`) |
| Opening a format (EPUB, FB2, MOBI, TXT) | `crates/ink-formats/src/` | `crates/ink-formats/tests/fixtures.rs` with files in `testdata/formats/` |
| PDF/CBZ | `crates/ink-mupdf/src/` | `crates/ink-mupdf/tests/` |
| Widgets, gestures, keyboard, refresh | `crates/ink-ui/src/` | unit tests in the module |
| Fonts, shaping, hyphenation | `crates/ink-text/src/` | unit tests in the module |
| Kindle/Kobo hardware (light, Wi-Fi, battery, sleep) | `crates/ink-device/src/` (backends `kindle/`, `kobo/`, `sim`, `headless`) | unit tests with fakes; real behaviour only on a device |
| Settings, sidecars, library index, stats DB | `crates/ink-store/src/` | unit tests (temp dirs) |
| A plugin | `plugins/<name>/` | see the `new-plugin` skill |
| CLI subcommands, app loop, update | `crates/ink-app/src/{cli,app,update}.rs` | `crates/ink-app/tests/verify_cli.rs`, unit tests |

Rules:

- Keep the dependency direction (`docs/ARCHITECTURE.md` "Crate map"). A lower crate
  never imports `ink-app`.
- Copy patterns from current code next to your change, e.g. a screen from
  `screens/settings/battery.rs`, a plugin from `plugins/wikipedia`.
- Public API of another crate: additive only, or update every caller in the same commit.
- A new dependency: `[workspace.dependencies]` with an exact version; it must build for
  `armv7-unknown-linux-musleabihf`.
- Hot paths (page turn, frame paint, the app loop) must not gain disk I/O, allocation
  per pixel, or polling while idle. If you touch them, use the `perf-measure` skill.

## 3. Tests

- Write the failing test first when fixing a bug; it is the evidence the bug existed.
- Run the crate's tests while working: `cargo test -p <crate>` or one test by path,
  e.g. `cargo test -p ink-app --lib screens::reader::tests::<name>`.
- Visual check on the host: `cargo run -p ink-app -- sim testdata/juliet.epub --headless
  --script <script> --out <dir>` writes PNGs for `shot NAME` steps; scripts are in
  `tests/sim/`. Look at the PNGs.
- Goldens: if pixels change on purpose, `scripts/golden-update.sh out/evidence/<topic>/golden-diff`,
  look at every `*.diff.png`, and commit the new goldens in the same commit as the code
  that changed them. The commit body says what changed in the image and why. Never
  regenerate goldens to make an unexplained failure go away.
- Device-facing behaviour: an emulator scenario in `tests/e2e/` if it can be checked on
  the emulator (see the `testing` skill), else list it under "Not tested on a device".

## 4. Docs and changelog

- `CHANGELOG.md`: under the topmost unreleased heading (`## [Unreleased]`; create it
  above the newest version if missing), in `### Added`, `### Changed` or `### Fixed`.
  User language: what the user sees and why it is better, with menu paths written
  `Settings › Battery`. Not the commit subject. Several commits can share one entry.
- `docs/user-guide.md`: update the section that describes the behaviour (sections 4-9).
- `docs/plugins.md` for plugin API changes, `docs/ARCHITECTURE.md` for new crates or
  traits. Keep doc edits to what the change needs.
- Do not touch the version in `Cargo.toml`.

## 5. Commits

Small, one concern each. Subject `Area: a plain statement of what changes`
(`Reader: count of pages updates after a font size change`). Body: what was wrong or
missing and what the change does, wrapped at about 72 columns. A goldens change, a
changelog entry and the code they belong to go in the same commit.

Before each commit:

```sh
cargo clippy -p <crate> --all-targets -- -D warnings
cargo test -p <crate>
```

Before handing over:

```sh
cargo clippy --workspace --all-targets -- -D warnings
scripts/test-fast.sh
```

## 6. PR

Title: short list of the changes. Body (template in `.github/pull_request_template.md`,
example PR #19):

```markdown
<one paragraph: what this PR brings>

**<Change in a few words>** (`abc1234`)
<why and what, a few lines or bullets>

**<Next change>** (`def5678`)
...

**Checks:** <commands run, simulator or emulator runs, measurements>

**Not tested on a device:** <what only real hardware can show>
```

## Done when

- clippy and `scripts/test-fast.sh` pass; paste their summary lines in your report.
- The new behaviour has a test, and goldens changed only with an explanation.
- `CHANGELOG.md` (and the user guide, if affected) says what users will notice.
