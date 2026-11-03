#!/usr/bin/env python3
"""Write malformed and unusual books for robustness tests (all self-generated, tiny).

  scripts/e2e/make-bad-books.py OUTDIR [--good testdata/juliet.epub]

Each file name says what is wrong with it. scripts/e2e/attack-inputs.sh feeds them to
`rinky render`, `rinky sim --headless` and `rinky selftest --book`.
"""
import os
import sys
import zipfile
from pathlib import Path

CONTAINER = """<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="{opf}" media-type="application/oebps-package+xml"/></rootfiles>
</container>"""

OPF = """<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">{meta}</metadata>
  <manifest>{manifest}</manifest>
  <spine>{spine}</spine>
</package>"""

META = '<dc:title>Bad book</dc:title><dc:identifier id="id">bad-1</dc:identifier><dc:language>en</dc:language>'


def xhtml(body: str) -> str:
    return ('<?xml version="1.0" encoding="utf-8"?>\n<html xmlns="http://www.w3.org/1999/xhtml">'
            f"<head><title>c</title></head><body>{body}</body></html>")


def epub(path: Path, files: dict, opf_path="OEBPS/content.opf", container=True, mimetype=True):
    with zipfile.ZipFile(path, "w") as z:
        if mimetype:
            z.writestr("mimetype", "application/epub+zip", compress_type=zipfile.ZIP_STORED)
        if container:
            z.writestr("META-INF/container.xml", CONTAINER.format(opf=opf_path))
        for name, data in files.items():
            z.writestr(name, data)


def simple(path: Path, chapters: list, meta=META, extra=None, spine=None, manifest=None):
    files = {}
    man, sp = [], []
    for i, body in enumerate(chapters):
        name = f"c{i}.xhtml"
        files[f"OEBPS/{name}"] = body if isinstance(body, bytes) else xhtml(body)
        man.append(f'<item id="c{i}" href="{name}" media-type="application/xhtml+xml"/>')
        sp.append(f'<itemref idref="c{i}"/>')
    files.update(extra or {})
    files["OEBPS/content.opf"] = OPF.format(meta=meta, manifest=manifest if manifest is not None else "".join(man),
                                           spine=spine if spine is not None else "".join(sp))
    epub(path, files)


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    out = Path(sys.argv[1])
    good = Path(sys.argv[3]) if len(sys.argv) > 3 and sys.argv[2] == "--good" else Path("testdata/juliet.epub")
    out.mkdir(parents=True, exist_ok=True)
    w = lambda name, data: (out / name).write_bytes(data)

    w("empty.epub", b"")
    w("junk.epub", b"this is not a zip file\n" * 10)
    (out / "dir.epub").mkdir(exist_ok=True)
    w("empty.txt", b"")
    w("binary.txt", bytes(range(256)) * 64)
    w("one-long-line.txt", b"word " * 40000)
    w("no-spaces.txt", b"x" * 50000)
    w("only-newlines.txt", b"\r\n" * 5000)
    w("junk.pdf", b"%PDF-1.4 not really\n")
    w("junk.cbz", b"PK\x03\x04 truncated")
    w("book.fb2", b'<?xml version="1.0"?><FictionBook><body><p>hi</p></body></FictionBook>')
    w("book.mobi", b"\0" * 100)
    w("broken.html", b"<html><body><p>unclosed <b>bold <i>both</p></div></span>&bogus; &#xffffffff;")
    w("noext", b"hello")

    epub(out / "zip-only-mimetype.epub", {}, container=False)
    epub(out / "container-missing-opf.epub", {}, opf_path="OEBPS/nothere.opf")
    epub(out / "container-absolute-path.epub", {"OEBPS/content.opf": OPF.format(meta=META, manifest="", spine="")},
         opf_path="/OEBPS/content.opf")
    epub(out / "garbage-container.epub", {"META-INF/container.xml": "<<<not xml"}, container=False)
    simple(out / "empty-spine.epub", [], spine="", manifest="")
    simple(out / "spine-bad-idref.epub", ["<p>x</p>"], spine='<itemref idref="nope"/>')
    simple(out / "spine-missing-file.epub", [], manifest='<item id="a" href="gone.xhtml" media-type="application/xhtml+xml"/>',
           spine='<itemref idref="a"/>')
    simple(out / "no-metadata.epub", ["<p>text</p>"], meta="")
    simple(out / "empty-chapter.epub", [""])
    simple(out / "whitespace-chapter.epub", ["   \n\t  "])
    simple(out / "empty-then-text.epub", ["", "<p>after an empty chapter</p>", ""])
    simple(out / "long-word.epub", ["<p>" + "a" * 20000 + "</p>"])
    simple(out / "long-paragraph.epub", ["<p>" + "word " * 60000 + "</p>"])
    simple(out / "deep-nesting.epub", ["<div>" * 50000 + "deep" + "</div>" * 50000])
    simple(out / "invalid-utf8.epub", [b"<html><body><p>bad \xff\xfe bytes \xc3</p></body></html>"])
    simple(out / "nul-chars.epub", ["<p>nul \0 here</p>".replace("\0", "&#0;") + "<p>raw \x01\x02</p>"])
    simple(out / "missing-image.epub", ['<p><img src="nothere.png"/></p><p>text</p>'])
    simple(out / "bad-image.epub", ['<p><img src="bad.png"/></p>'], extra={"OEBPS/bad.png": b"\x89PNG\r\n\x1a\nbroken"})
    simple(out / "path-traversal.epub", ["<p>x</p>"], manifest='<item id="c0" href="../../../../etc/passwd" media-type="application/xhtml+xml"/>')
    simple(out / "many-chapters.epub", [f"<p>chapter {i}</p>" for i in range(1500)])
    simple(out / "huge-entities.epub", ["<p>" + "&amp;" * 30000 + "</p>"])
    simple(out / "rtl-combining.epub", ["<p>שלום é́́ \U0001F600 ​​</p>"])
    simple(out / "table-pre.epub", ["<table><tr><td>" + "c " * 500 + "</td></tr></table><pre>" + "x" * 3000 + "</pre>"])
    data = good.read_bytes()
    w("truncated-half.epub", data[: len(data) // 2])
    w("truncated-tail.epub", data[:-200])
    w("UPPER.EPUB", data)
    for p in sorted(out.iterdir()):
        print(p.name, p.stat().st_size if p.is_file() else "dir")
    return 0


if __name__ == "__main__":
    sys.exit(main())
