# Contributing to rinky

Thanks for helping. This page is the short version for people; the full working guide
(commands, layout, conventions, what "done" means) is [AGENTS.md](AGENTS.md), and it
applies to people too.

## Set up

```sh
rustup target add armv7-unknown-linux-musleabihf   # only for Kindle builds
brew install zig                                    # zig 0.17; also builds MuPDF for the host
cargo install cargo-zigbuild
cargo run --release -p ink-app -- sim testdata/juliet.epub   # the reader in a window
```

The first build compiles MuPDF from source (about a minute); later builds reuse it.

## Before you open a pull request

```sh
cargo clippy --workspace --all-targets -- -D warnings
scripts/test-fast.sh            # or cargo test --workspace, which CI runs
```

- Work on a branch (`fix/<topic>`, `feat/<topic>`), one concern per commit.
- Commit subjects read `Area: what changes`, for example
  `Reader: a long press selects the whole of a word hyphenated at a line end`; the
  body says why.
- Add a test next to the code you change. If a golden image changes on purpose,
  regenerate it with `scripts/golden-update.sh <empty dir>` and say why in the commit.
- User-visible changes get a line in `CHANGELOG.md` under the unreleased heading, in
  words a reader would use. Don't change the version; releases do that.
- Say in the PR what you could not test on a real Kindle or Kobo.

## Where to read more

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): crates, traits, data flow
- [docs/TESTING.md](docs/TESTING.md): test suites, slow tests, emulator scenarios
- [docs/plugins.md](docs/plugins.md): the plugin API (Rust and Lua)
- [docs/user-guide.md](docs/user-guide.md): what users see

## Reporting a bug

Open an issue with the Kindle or Kobo model, firmware, rinky version
(`/mnt/us/rinky/VERSION`), what you did, what you expected, and the log
(`/mnt/us/rinky/rinky.log`; on a Kobo `.adds/rinky/rinky.log`). The user guide's
"Logs and reporting a problem" section lists the files.

By contributing you agree that your contribution is licensed under the
AGPL-3.0-or-later, like the rest of rinky.
