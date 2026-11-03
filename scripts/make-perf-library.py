#!/usr/bin/env python3
"""Writes N small synthetic EPUBs with a cover image for library-scan timing.

Deterministic (fixed seed, zip timestamps and order). Every book has its own title,
author, identifier and text, so the partial MD5s differ. The cover is a 600 x 900 JPEG
(Pillow) or, without Pillow, a 600 x 900 gray PNG written with zlib.

  python3 -I scripts/make-perf-library.py OUTDIR N [WORDS_PER_BOOK]
"""
import io
import random
import struct
import sys
import zipfile
import zlib
from pathlib import Path

if len(sys.argv) < 3:
    sys.exit(__doc__)
OUT = Path(sys.argv[1])
N = int(sys.argv[2])
WORDS = int(sys.argv[3]) if len(sys.argv) > 3 else 20000
OUT.mkdir(parents=True, exist_ok=True)

VOCAB = (
    "the of and to in a is that it was for on are as with his they be at one have this from or had by "
    "word but what some we can out other were all there when up use your how said an each she which do "
    "their time if will way about many then them write would like so these her long make thing see him "
    "two has look more day could go come did number sound no most people my over know water than call "
    "first who may down side been now find any new work part take get place made live where after back"
).split()

CONTAINER = """<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>
"""


def cover(rng, i):
    w, h = 600, 900
    try:
        from PIL import Image, ImageDraw

        img = Image.new("RGB", (w, h), (rng.randint(0, 255), rng.randint(0, 255), rng.randint(0, 255)))
        d = ImageDraw.Draw(img)
        for _ in range(40):
            x0, y0 = rng.randint(0, w), rng.randint(0, h)
            d.rectangle([x0, y0, x0 + rng.randint(10, 200), y0 + rng.randint(10, 200)],
                        fill=(rng.randint(0, 255), rng.randint(0, 255), rng.randint(0, 255)))
        d.text((40, 100), f"Book {i}", fill=(0, 0, 0))
        buf = io.BytesIO()
        img.save(buf, "JPEG", quality=85)
        return buf.getvalue(), "image/jpeg", "cover.jpg"
    except ImportError:
        g = rng.randint(0, 255)
        raw = b"".join(b"\0" + bytes([(g + x + y) & 255 for x in range(w)]) for y in range(h))

        def chunk(t, data):
            c = t + data
            return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))

        png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 0, 0, 0, 0))
        png += chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b"")
        return png, "image/png", "cover.png"


def chapter(rng, words):
    paras = []
    left = words
    while left > 0:
        n = rng.randint(40, 160)
        left -= n
        paras.append("<p>" + " ".join(rng.choice(VOCAB) for _ in range(n)) + ".</p>")
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n<html xmlns="http://www.w3.org/1999/xhtml"><head><title>c</title>'
        "</head><body>" + "\n".join(paras) + "</body></html>\n"
    )


def add(z, name, data, compress=zipfile.ZIP_DEFLATED):
    info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
    info.compress_type = compress
    z.writestr(info, data)


for i in range(N):
    rng = random.Random(1000 + i)
    chapters = 8
    img, mtype, cname = cover(rng, i)
    items = "\n".join(f'<item id="c{k}" href="c{k}.xhtml" media-type="application/xhtml+xml"/>' for k in range(chapters))
    refs = "".join(f'<itemref idref="c{k}"/>' for k in range(chapters))
    opf = f"""<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
    <dc:title>Library Book {i:04}</dc:title>
    <dc:creator opf:file-as="Author, Perf {i % 37}">Perf Author {i % 37}</dc:creator>
    <dc:language>en</dc:language>
    <dc:identifier id="id">urn:uuid:00000000-0000-4000-9000-{i:012d}</dc:identifier>
    <meta name="cover" content="cover"/>
  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="cover" href="{cname}" media-type="{mtype}"/>
{items}
  </manifest>
  <spine toc="ncx">{refs}</spine>
</package>
"""
    points = "".join(
        f'<navPoint id="n{k}" playOrder="{k + 1}"><navLabel><text>Chapter {k + 1}</text></navLabel>'
        f'<content src="c{k}.xhtml"/></navPoint>'
        for k in range(chapters)
    )
    ncx = (
        '<?xml version="1.0" encoding="UTF-8"?>\n<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">'
        f"<head></head><docTitle><text>Library Book {i:04}</text></docTitle><navMap>{points}</navMap></ncx>\n"
    )
    path = OUT / f"book-{i:04}.epub"
    with zipfile.ZipFile(path, "w") as z:
        add(z, "mimetype", "application/epub+zip", zipfile.ZIP_STORED)
        add(z, "META-INF/container.xml", CONTAINER)
        add(z, "OEBPS/content.opf", opf)
        add(z, "OEBPS/toc.ncx", ncx)
        add(z, f"OEBPS/{cname}", img, zipfile.ZIP_STORED)
        for k in range(chapters):
            add(z, f"OEBPS/c{k}.xhtml", chapter(rng, WORDS // chapters))
print(f"{N} books in {OUT}")
