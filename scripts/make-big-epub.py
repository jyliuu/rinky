#!/usr/bin/env python3
"""Writes a big synthetic EPUB 2 for open/pagination timing.

Deterministic (fixed seed, zip timestamps and order). The book has a linked stylesheet,
an NCX with several entries per spine item pointing at ids inside the chapters (like
real non-fiction books), headings, paragraphs with italic and bold runs.

  python3 -I scripts/make-big-epub.py OUT.epub SPINE_ITEMS WORDS_PER_ITEM [TOC_PER_ITEM]

At the default layout (1272 x 1696, 300 dpi) one page holds about 260 words, so
  36 x 36000 words  ~ 5000 pages (like "Dialogues": 36 spine items, 4607 pages)
  63 x 5200 words   ~ 1260 pages (like "Dune": 63 spine items, 1226 pages)
"""
import random
import sys
import zipfile

if len(sys.argv) < 4:
    sys.exit(__doc__)
OUT = sys.argv[1]
SPINE = int(sys.argv[2])
WORDS = int(sys.argv[3])
TOC_PER = int(sys.argv[4]) if len(sys.argv) > 4 else 8

rng = random.Random(1234)
VOCAB = (
    "the of and to in a is that it was for on are as with his they be at one have this from or had by "
    "word but what some we can out other were all there when up use your how said an each she which do "
    "their time if will way about many then them write would like so these her long make thing see him "
    "two has look more day could go come did number sound no most people my over know water than call "
    "first who may down side been now find any new work part take get place made live where after back "
    "little only round man year came show every good me give our under name very through just form "
    "sentence great think say help low line differ turn cause much mean before move right boy old too "
    "same tell does set three want air well also play small end put home read hand port large spell add "
    "even land here must big high such follow act why ask men change went light kind off need house "
    "picture try us again animal point mother world near build self earth father dialogue reason virtue "
    "justice knowledge soul city philosopher question answer argument Socrates Glaucon Thrasymachus"
).split()


def sentence():
    n = rng.randint(6, 24)
    words = [rng.choice(VOCAB) for _ in range(n)]
    words[0] = words[0].capitalize()
    if rng.random() < 0.25:
        i = rng.randint(0, n - 2)
        tag = rng.choice(["i", "b", "em"])
        words[i] = f"<{tag}>{words[i]}"
        words[i + 1] = f"{words[i + 1]}</{tag}>"
    return " ".join(words) + rng.choice([".", ".", ".", "?", "!", ";"])


def chapter(idx):
    out = [f'<h1 id="c{idx}">Book {idx + 1}</h1>']
    left = WORDS
    para = 0
    sections = max(TOC_PER - 1, 0)
    per_section = WORDS // (sections + 1)
    next_section = per_section
    sec = 0
    while left > 0:
        if sections and WORDS - left >= next_section and sec < sections:
            sec += 1
            next_section += per_section
            out.append(f'<h2 id="c{idx}s{sec}">Section {sec}</h2>')
        text = []
        n = 0
        target = rng.randint(30, 160)
        while n < target:
            s = sentence()
            n += s.count(" ") + 1
            text.append(s)
        left -= n
        cls = ' class="first"' if para == 0 else ""
        out.append(f'<p id="p{idx}_{para}"{cls}>{" ".join(text)}</p>')
        para += 1
    body = "\n".join(out)
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">\n'
        '<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Book</title>'
        '<link rel="stylesheet" type="text/css" href="../Styles/style.css"/></head>\n<body>\n'
        + body
        + "\n</body></html>\n"
    )


CSS = """body { margin: 0; padding: 0; }
h1 { font-size: 1.6em; text-align: center; margin: 2em 0 1em; page-break-before: always; }
h2 { font-size: 1.2em; margin: 1.5em 0 0.5em; }
p { text-indent: 1.2em; margin: 0; text-align: justify; }
p.first { text-indent: 0; }
"""

CONTAINER = """<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>
"""


def opf(title):
    items = "\n".join(
        f'    <item id="ch{i}" href="Text/ch{i:03}.xhtml" media-type="application/xhtml+xml"/>' for i in range(SPINE)
    )
    refs = "".join(f'<itemref idref="ch{i}"/>' for i in range(SPINE))
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>{title}</dc:title>
    <dc:creator>rinky perf</dc:creator>
    <dc:language>en</dc:language>
    <dc:identifier id="id">urn:uuid:00000000-0000-4000-8000-{SPINE:06d}{WORDS:06d}</dc:identifier>
  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="css" href="Styles/style.css" media-type="text/css"/>
{items}
  </manifest>
  <spine toc="ncx">{refs}</spine>
</package>
"""


def ncx(title):
    points = []
    order = 1
    for i in range(SPINE):
        kids = []
        for s in range(1, TOC_PER):
            order += 1
            kids.append(
                f'<navPoint id="n{order}" playOrder="{order}"><navLabel><text>Section {s}</text></navLabel>'
                f'<content src="Text/ch{i:03}.xhtml#c{i}s{s}"/></navPoint>'
            )
        points.append(
            f'<navPoint id="b{i}" playOrder="{order}"><navLabel><text>Book {i + 1}</text></navLabel>'
            f'<content src="Text/ch{i:03}.xhtml#c{i}"/>{"".join(kids)}</navPoint>'
        )
        order += 1
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><head></head>'
        f"<docTitle><text>{title}</text></docTitle><navMap>" + "".join(points) + "</navMap></ncx>\n"
    )


def add(z, name, data, compress=zipfile.ZIP_DEFLATED):
    info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
    info.compress_type = compress
    z.writestr(info, data)


title = f"Perf {SPINE}x{WORDS}"
with zipfile.ZipFile(OUT, "w") as z:
    add(z, "mimetype", "application/epub+zip", zipfile.ZIP_STORED)
    add(z, "META-INF/container.xml", CONTAINER)
    add(z, "OEBPS/content.opf", opf(title))
    add(z, "OEBPS/toc.ncx", ncx(title))
    add(z, "OEBPS/Styles/style.css", CSS)
    for i in range(SPINE):
        add(z, f"OEBPS/Text/ch{i:03}.xhtml", chapter(i))
print(OUT)
