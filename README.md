<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo/halftone-header-dark.svg">
    <img src="docs/logo/halftone-header.svg" alt="rinky" width="320">
  </picture>
</p>

<p align="center">
  An e-book reader for Kindle and Kobo e-readers, written in Rust.
</p>

<p align="center">
  <a href="../../releases/latest">Download</a> ·
  <a href="#install">Install</a> ·
  <a href="docs/user-guide.md">User guide</a> ·
  <a href="docs/kobo.md">Kobo</a> ·
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

On a **Kobo**, see [rinky on Kobo](docs/kobo.md): no jailbreak, started from
NickelMenu; Kobo support is new and has run in an emulator only. The rest of this
section is about Kindles.

You need a jailbroken Kindle (see [kindlemodding.org](https://kindlemodding.org/)) with
KUAL installed. rinky is developed on the Paperwhite 12th generation (2024) on firmware
5.19.x and runs on the other Kindles with a 300 ppi screen. Apart from the Paperwhite
12th gen, they have been tried in an emulator ([eink-emulator](https://github.com/katadelos/eink-emulator)
with Amazon's firmware), not on real devices:

| Kindle | Status |
|---|---|
| Paperwhite 12th gen (2024) | tested on the device |
| Paperwhite 4 (5.18.1), Oasis 3 (5.18.2), Paperwhite 3 (5.16.2), Kindle 12th gen (2024, 5.20.2) | runs in the emulator: books open, taps and page keys turn pages, restart works |
| Paperwhite 5 and Signature Edition, Kindle 11th gen, Colorsoft, Scribe (all three), Oasis 2 | untested; same display interface as a model above |
| Voyage, Oasis 1 | untested; same display interface as the Paperwhite 3 |
| Paperwhite 1 and 2, Kindle 7th, 8th and 10th gen | not supported (lower-resolution screens) |

On a Colorsoft, rinky shows grayscale. If you try rinky on a model marked untested,
please open an issue with `rinky/rinky.log` from the Kindle, whether it worked or not.

The steps are the same on every Kindle:

1. Jailbreak it and install KUAL. Which jailbreak works depends on the firmware version
   (*Settings › Device Options › Device Info*); [kindlemodding.org](https://kindlemodding.org/)
   picks the right one and has the KUAL build for your firmware. The Paperwhite 3,
   Voyage and Oasis 1 stay on older firmware and use the older KUAL; rinky's package is
   the same for all.
2. Download `rinky-<version>-kindle.zip` from the [latest release](../../releases/latest)
   and unzip it. It contains two folders, `rinky` and `extensions`.
3. Connect the Kindle by USB and copy both folders to the top level of its storage.
   The Paperwhite 3 and 4, Voyage and Oasis show up as a drive named *Kindle*. Newer
   Kindles connect over MTP like a phone; on a Mac, use an MTP app such as
   [OpenMTP](https://openmtp.ganeshrvel.com/).
4. Eject the Kindle, open KUAL and choose **rinky › Start rinky**.

The [user guide](docs/user-guide.md#getting-your-kindle-ready) has notes for each
model (page keys, warm light, Scribe, Colorsoft).

Choose which folders the library shows under **Settings › Library folders**; the
Kindle's `documents` folder is included by default. **Settings › Exit to Kindle** returns to the Kindle's
own screens.

To update, turn Wi-Fi on and choose **Settings › Update › Check for update** in rinky,
then **Install and restart** (or **KUAL › rinky › Update rinky**). The download is
checked against the release's SHA-256, and settings, notes and positions are kept. This
works the same on every model; it was tried in the emulator on the Paperwhite 3 and 4,
Oasis 3 and Kindle 12th gen.

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
