# Format fixtures (owner: `formats` agent)

Used by `crates/ink-formats/tests/fixtures.rs` and the unit tests. Every file is public
domain, generated here, or a calibre conversion of a generated file. Each is under 2 MB.

| File | What | Source |
| --- | --- | --- |
| `alice-kf8.azw3` | KF8-only book (FDST, skeleton/fragment tables, NCX, cover) | Project Gutenberg #11, `11.kf8.images` (public domain) |
| `alice-old.mobi` | MOBI 6 book (filepos links, pagebreaks, recindex images, NCX) | Project Gutenberg #11, `11.kindle.images` |
| `alice.txt` | UTF-8 text with CRLF and a Gutenberg header | Project Gutenberg #11, `pg11.txt` |
| `alice-calibre.fb2` | calibre FB2 output (`--sectionize toc`) | `ebook-convert` of Gutenberg #11 (no images) |
| `sample.fb2`, `sample.fb2.zip` | every FB2 element the converter handles, notes, images | `make-fixtures.py` |
| `sample-cp1251.fb2` | windows-1251 FB2 (Pushkin, 1833) | `make-fixtures.py` |
| `sample.fb3` | FB3 OPC package | `make-fixtures.py` |
| `txt/*.txt` | UTF-8 (± BOM), UTF-16LE BOM, UTF-16BE without BOM, cp1252 (Andersen, 1835), cp1251, KOI8-R, Shift_JIS (Soseki, 1905), indented paragraphs | `make-fixtures.py` |
| `sample.html`, `html-pic.png` | windows-1252 HTML with broken markup, script, image | `make-fixtures.py` |
| `epub3-nav.epub` | EPUB 3: refines, series, nested nav, page list, IDPF and Adobe obfuscated fonts | `make-fixtures.py` |
| `font-subset.ttf` | ASCII subset of Noto Serif Regular (OFL, see `fonts/LICENSE-Noto`) | `make-fixtures.py` (fontTools) |
| `epub2-ncx.epub` | EPUB 2: NCX nested TOC, pageList, guide cover, calibre series | `make-fixtures.py` |
| `drm-adept.epub`, `drm-lcp.epub` | fake Adobe ADEPT and Readium LCP books (metadata readable, chapter encrypted) | `make-fixtures.py` |
| `sample-calibre.azw3`, `sample-calibre-old.mobi`, `sample-calibre-joint.mobi` | calibre KF8, MOBI 6 and joint MOBI 6 + KF8 of `epub2-ncx.epub` | `make-fixtures-mobi.py --calibre` (calibre 9.15) |
| `fonts-calibre.azw3` | calibre KF8 of `epub3-nav.epub` (FONT records) | `make-fixtures-mobi.py --calibre` |
| `drm-sample.azw` | `sample-calibre-old.mobi` with the record 0 encryption field set to 2 | `make-fixtures-mobi.py` |
| `huffcdic.mobi`, `huffcdic.expected.html` | MOBI 6 with HUFF/CDIC compression and its expected text; calibre's `MobiReader` decodes it to exactly the expected text | `make-fixtures-mobi.py` |
| `kfx-container.kfx` | KFX `CONT` container with a `book_metadata` entity and a DRMION entity | `make-fixtures-mobi.py` |
| `kfx-drmion.kfx`, `topaz.azw1` | DRMION and Topaz headers | `make-fixtures-mobi.py` |

Regenerate: `python3 -I testdata/formats/make-fixtures.py` (deterministic) and, for the
calibre files, `python3 -I testdata/formats/make-fixtures-mobi.py --calibre`.
