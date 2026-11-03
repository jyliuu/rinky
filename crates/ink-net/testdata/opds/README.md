# OPDS fixtures

| File | Source | Fetched | License |
| --- | --- | --- | --- |
| `gutenberg-root.xml` | https://www.gutenberg.org/ebooks.opds/ | 2026-10-24 | Project Gutenberg catalog metadata (public-domain books) |
| `gutenberg-search-austen.xml` | https://www.gutenberg.org/ebooks/search.opds/?query=austen | 2026-10-24 | same |
| `gutenberg-1342.xml` | https://www.gutenberg.org/ebooks/1342.opds | 2026-10-24 | same |
| `gutenberg-osd.xml` | https://www.gutenberg.org/catalog/osd-books.xml | 2026-10-24 | same |
| `test-catalog-home.json` | https://github.com/opds-community/test-catalog `2.0/home.json` | 2026-10-24 | BSD-3-Clause, see `LICENSE-test-catalog.txt` |
| `test-catalog-navigation.json` | same repository, `2.0/navigation.json` | 2026-10-24 | BSD-3-Clause |
| `test-catalog-publications.json` | same repository, `2.0/publications.json` | 2026-10-24 | BSD-3-Clause |
| `edge-cases.xml` | written for rinky; cases from KOReader's `spec/unit/opds_spec.lua` and real catalogs | 2026-10-24 | AGPL-3.0-or-later (this project) |

Each file was fetched once (no scraping) and is committed unchanged, except
`edge-cases.xml`. Standard Ebooks' OPDS feeds answered 401 (login needed) on
2026-10-24 and are not included.
