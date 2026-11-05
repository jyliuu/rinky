<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo/halftone-header-dark.svg">
    <img src="docs/logo/halftone-header.svg" alt="rinky" width="320">
  </picture>
</p>

<p align="center">
  An e-book reader for Kindle e-readers, written in Rust.
</p>

<p align="center">
  <a href="../../releases/latest">Download</a> ·
  <a href="#install">Install</a> ·
  <a href="docs/user-guide.md">User guide</a> ·
  <a href="docs/plugins.md">Plugins</a>
</p>

<table>
  <tr>
    <td><img src="docs/screenshots/home.png" alt="Home screen"></td>
    <td><img src="docs/screenshots/reading.png" alt="Reading a chapter"></td>
    <td><img src="docs/screenshots/dictionary.png" alt="Dictionary lookup"></td>
  </tr>
  <tr>
    <td align="center"><sub>Home</sub></td>
    <td align="center"><sub>Reading</sub></td>
    <td align="center"><sub>Dictionary</sub></td>
  </tr>
  <tr>
    <td><img src="docs/screenshots/typography.png" alt="Reading settings"></td>
    <td><img src="docs/screenshots/highlight.png" alt="Selecting text"></td>
    <td><img src="docs/screenshots/stats.png" alt="Reading statistics"></td>
  </tr>
  <tr>
    <td align="center"><sub>Reading settings</sub></td>
    <td align="center"><sub>Highlights and notes</sub></td>
    <td align="center"><sub>Statistics</sub></td>
  </tr>
</table>

rinky replaces the Kindle's reading screens while it runs and gives the Kindle back when
you exit. It is a single static binary that draws directly to the e-ink display and reads
the touchscreen from the kernel.

## Features

- Formats: EPUB, MOBI and AZW3 without DRM, FB2, TXT, HTML, PDF and CBZ.
- Justified text with hyphenation; font, size, spacing, margins and weight are adjustable.
  Any font copied to the Kindle can be used, including the built-in Bookerly.
- Dictionary lookup (StarDict), Wikipedia, and an optional AI assistant that uses your
  own API key.
- Highlights, notes and bookmarks, exportable as Markdown. Reading statistics and a
  vocabulary list.
- Calibre wireless transfers, OPDS catalogs, KOReader sync, and import of KOReader
  positions and highlights.
- Plugins in Lua or Rust.

rinky never modifies the Kindle's own library or KOReader's files.

<table>
  <tr>
    <td><img src="docs/screenshots/library.png" alt="Library"></td>
    <td><img src="docs/screenshots/menu.png" alt="Reader menu"></td>
    <td><img src="docs/screenshots/settings.png" alt="Settings"></td>
  </tr>
</table>

## Performance

rinky is written in Rust, from the touch input to the pixels on the screen (PDFs and
comics are drawn by MuPDF). Rust programs run as fast, compact native code, so the
reader stays quick on the Kindle's modest processor and memory: books open fast, page
turns don't stall, and long books are paginated in the background while you start
reading.

## Install

You need a jailbroken Kindle (see [kindlemodding.org](https://kindlemodding.org/)) with
KUAL installed. rinky is developed and tested on the Paperwhite 12th generation (2024)
on firmware 5.19.x. It also runs on the other Kindles with a 300 ppi screen, but those
haven't been tried yet:

| Kindle | Status |
|---|---|
| Paperwhite 12th gen (2024) | tested |
| Paperwhite 5 and Signature Edition (11th gen), Kindle 11th and 12th gen, Colorsoft, Scribe (all three) | untested; same display controller as the tested one |
| Paperwhite 4 (10th gen), Oasis 2 and 3 | untested |
| Paperwhite 3, Voyage, Oasis 1 | untested; old firmware and kernels, may not start |
| Paperwhite 1 and 2, Kindle 7th, 8th and 10th gen | not supported (lower-resolution screens) |

On a Colorsoft, rinky shows grayscale. If you try rinky on a model marked untested,
please open an issue with `rinky/rinky.log` from the Kindle, whether it worked or not.

1. Download `rinky-<version>-kindle.zip` from the [latest release](../../releases/latest)
   and unzip it. It contains two folders, `rinky` and `extensions`.
2. Connect the Kindle by USB and copy both folders to the top level of its storage. On a
   Mac, use an MTP app such as [OpenMTP](https://openmtp.ganeshrvel.com/).
3. Eject the Kindle, open KUAL and choose **rinky › Start rinky**.

Choose which folders the library shows under **Settings › Library folders**; the
Kindle's `documents` folder is included by default. **Settings › Exit to Kindle** returns to the Kindle's
own screens.

To update, choose **KUAL › rinky › Update rinky** with Wi-Fi on. The download is checked
against the release's SHA-256, and settings, notes and positions are kept.

Dictionaries go in `rinky/dict/<name>/` on the Kindle, as StarDict files (`.ifo`, `.idx`,
`.dict` or `.dict.dz`).

<details>
<summary>Install over SSH instead</summary>

Copy `rinky-<version>-kindlehf.tar.gz` to the Kindle, then:

```sh
mkdir -p /mnt/us/.rinky-pkg
tar -xzf /mnt/us/rinky-<version>-kindlehf.tar.gz -C /mnt/us/.rinky-pkg
sh /mnt/us/.rinky-pkg/rinky/install.sh
rm -rf /mnt/us/.rinky-pkg /mnt/us/rinky-<version>-kindlehf.tar.gz
```

This also adds a **rinky** tile to the Kindle library, so you can start it without KUAL.
</details>

<details>
<summary>Uninstall</summary>

**KUAL › rinky › Uninstall rinky** removes everything rinky added. Choose *keep notes
and settings* to remove only the program. KOReader and the Kindle's own files are
never touched.
</details>

The [user guide](docs/user-guide.md) covers everything else, including troubleshooting.

## Build from source

```sh
rustup target add armv7-unknown-linux-musleabihf
brew install zig            # or your package manager; zig 0.17
cargo install cargo-zigbuild
scripts/package.sh          # → out/package/rinky-<version>-kindle{hf.tar.gz,.zip}
```

The result is a static ARMv7 binary plus the launcher, watchdog and KUAL menu. Try the
reader on your computer without a Kindle:

```sh
cargo run --release -p ink-app -- sim path/to/books/   # simulator window
cargo test --workspace
```

Pushing a tag `vX.Y.Z` that matches the version in `Cargo.toml` builds and publishes a
release with GitHub Actions ([`.github/workflows/release.yml`](.github/workflows/release.yml)).
Screenshots are regenerated with `scripts/screenshots.sh`. The code is laid out in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md); testing is described in
[`docs/TESTING.md`](docs/TESTING.md).

## License

rinky is licensed under the [GNU AGPL v3 or later](LICENSE) because its PDF engine,
[MuPDF](https://mupdf.com/), is AGPL. The bundled Noto Serif and Noto Sans fonts
are under the SIL Open Font License ([`fonts/LICENSE-Noto`](fonts/LICENSE-Noto)).
The books in the screenshots are public domain; their covers are generated
(`scripts/make-showcase-library.py`).
