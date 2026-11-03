#!/usr/bin/env python3
"""Writes testdata/synthetic.epub: a small, self-written EPUB 2 for golden tests.

Deterministic (fixed zip timestamps and order), so re-running gives identical bytes.
It exercises what the phase 0 engine handles: headings h1-h3, paragraphs, italic,
bold, <br>, entities, a non-breaking space, a long paragraph that spans pages, a
spine item without text, and a missing spine item.

  python3 -I scripts/make-synthetic-epub.py [OUT.epub]
"""
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "testdata" / "synthetic.epub"

CONTAINER = """<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>
"""

OPF = """<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Synthetic Test Book</dc:title>
    <dc:creator>rinky tests</dc:creator>
    <dc:language>en</dc:language>
    <dc:identifier id="id">urn:uuid:00000000-0000-4000-8000-000000000001</dc:identifier>
  </metadata>
  <manifest>
    <item id="c1" href="Text/one.xhtml" media-type="application/xhtml+xml"/>
    <item id="blank" href="Text/blank.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="Text/two%20words.xhtml" media-type="application/xhtml+xml"/>
    <item id="gone" href="Text/missing.xhtml" media-type="application/xhtml+xml"/>
    <item id="c3" href="Text/three.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="c1"/><itemref idref="blank"/><itemref idref="c2"/><itemref idref="gone"/><itemref idref="c3"/>
  </spine>
</package>
"""


def page(body: str) -> str:
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">\n'
        '<html xmlns="http://www.w3.org/1999/xhtml"><head><title>t</title>'
        "<style>p { text-indent: 1em }</style></head>\n<body>\n" + body + "\n</body></html>\n"
    )


ONE = page(
    "<h1>Chapter One</h1>\n"
    "<p>This book is written for the rinky golden tests. It has <i>italic words</i>, "
    "<b>bold words</b> and <i>mixed <b>styles</b></i> in one paragraph, so a change in the "
    "layout engine shows up as a changed pixel.</p>\n"
    "<p>Entities: caf&eacute;, &ldquo;quotes&rdquo;, an em dash&mdash;here, an ellipsis&hellip; "
    "and 100&nbsp;km kept together.</p>\n"
    "<p>A line break follows<br/>and the text goes on without an indent.</p>\n"
    "<h2>A Second-Level Heading</h2>\n"
    "<p>Short paragraph after a heading has no first-line indent.</p>\n"
    "<p>The next one does.</p>\n"
)

BLANK = page("<div> </div>")

SENTENCE = (
    "Every page of this paragraph repeats the same plain sentence so that the line "
    "breaker and the justification logic run across a page boundary. "
)
TWO = page(
    "<h2>Chapter Two</h2>\n<p>" + SENTENCE * 40 + "</p>\n"
    "<h3>Third-Level Heading Near The End Of A Page</h3>\n"
    "<p>The heading above must stay together with at least two following lines.</p>\n"
)

THREE = page(
    "<h1>Chapter Three</h1>\n"
    "<p>Numbers 0123456789 and punctuation: (a) [b] {c} a/b a-b a+b=c 50% #1 @home.</p>\n"
    "<blockquote>A block quote is a block in the phase 0 engine.</blockquote>\n"
    "<ul><li>First list item</li><li>Second list item</li></ul>\n"
    "<p>The end.</p>\n"
)

FILES = [
    ("META-INF/container.xml", CONTAINER),
    ("OEBPS/content.opf", OPF),
    ("OEBPS/Text/one.xhtml", ONE),
    ("OEBPS/Text/blank.xhtml", BLANK),
    ("OEBPS/Text/two words.xhtml", TWO),
    ("OEBPS/Text/three.xhtml", THREE),
]


def main() -> None:
    OUT.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(OUT, "w") as z:
        info = zipfile.ZipInfo("mimetype", date_time=(2026, 10, 24, 0, 0, 0))
        info.compress_type = zipfile.ZIP_STORED
        z.writestr(info, "application/epub+zip")
        for name, text in FILES:
            info = zipfile.ZipInfo(name, date_time=(2026, 10, 24, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            z.writestr(info, text.encode("utf-8"))
    print(f"{OUT} {OUT.stat().st_size} bytes")


if __name__ == "__main__":
    main()
