#!/usr/bin/env python3
"""Writes a small library of public-domain classics for the README screenshots.

Every book gets a generated grayscale cover (Noto Serif, no artwork from elsewhere).
Alice's Adventures in Wonderland is the full text of testdata/formats/alice.txt
(Project Gutenberg #11), split into its twelve chapters; the other books hold their
opening lines only. Deterministic: re-running gives identical bytes.

  python3 -I scripts/make-showcase-library.py [OUTDIR [DICTDIR]]
      OUTDIR    the books (default out/showcase/books)
      DICTDIR   also write a small StarDict dictionary of words from Alice there

Needs Pillow.
"""
import html
import io
import re
import sys
import zipfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "out" / "showcase" / "books"
FONTS = ROOT / "fonts"
W, H = 600, 900

BOOKS = [
    ("alice", "Alice's Adventures in Wonderland", "Lewis Carroll", 1865, None),
    ("pride", "Pride and Prejudice", "Jane Austen", 1813,
     "It is a truth universally acknowledged, that a single man in possession of a good "
     "fortune, must be in want of a wife."),
    ("moby", "Moby-Dick", "Herman Melville", 1851,
     "Call me Ishmael. Some years ago—never mind how long precisely—having little or no "
     "money in my purse, and nothing particular to interest me on shore, I thought I would "
     "sail about a little and see the watery part of the world."),
    ("frankenstein", "Frankenstein", "Mary Shelley", 1818,
     "You will rejoice to hear that no disaster has accompanied the commencement of an "
     "enterprise which you have regarded with such evil forebodings."),
    ("dracula", "Dracula", "Bram Stoker", 1897,
     "Left Munich at 8:35 P.M., on 1st May, arriving at Vienna early next morning."),
    ("jane-eyre", "Jane Eyre", "Charlotte Brontë", 1847,
     "There was no possibility of taking a walk that day."),
    ("walden", "Walden", "Henry David Thoreau", 1854,
     "When I wrote the following pages, or rather the bulk of them, I lived alone, in the "
     "woods, a mile from any neighbor."),
    ("dorian", "The Picture of Dorian Gray", "Oscar Wilde", 1890,
     "The studio was filled with the rich odour of roses."),
    ("middlemarch", "Middlemarch", "George Eliot", 1871,
     "Miss Brooke had that kind of beauty which seems to be thrown into relief by poor dress."),
    ("odyssey", "The Odyssey", "Homer", -700,
     "Tell me, O Muse, of that ingenious hero who travelled far and wide after he had "
     "sacked the famous town of Troy."),
    ("great-expectations", "Great Expectations", "Charles Dickens", 1861,
     "My father's family name being Pirrip, and my Christian name Philip, my infant tongue "
     "could make of both names nothing longer or more explicit than Pip."),
]


def font(style, size):
    name = {"r": "NotoSerif-Regular.ttf", "b": "NotoSerif-Bold.ttf", "i": "NotoSerif-Italic.ttf"}[style]
    return ImageFont.truetype(str(FONTS / name), size)


def wrap(draw, text, f, width):
    lines, line = [], ""
    for word in text.split():
        t = f"{line} {word}".strip()
        if draw.textlength(t, font=f) <= width or not line:
            line = t
        else:
            lines.append(line)
            line = word
    return lines + [line]


def block(draw, text, f, y, fill, width=W - 120, gap=1.12, align="center", x=60):
    """Draws wrapped text from y; returns the y below it."""
    size = f.size
    for line in wrap(draw, text, f, width):
        lw = draw.textlength(line, font=f)
        lx = x + (width - lw) / 2 if align == "center" else x
        draw.text((lx, y), line, font=f, fill=fill)
        y += size * gap
    return y


def fit(draw, text, style, width, start, lines_max=3):
    size = start
    while size > 30:
        f = font(style, size)
        if len(wrap(draw, text, f, width)) <= lines_max and all(
            draw.textlength(w, font=f) <= width for w in text.split()
        ):
            return f
        size -= 4
    return font(style, size)


def cover(i, title, author):
    """One of six layouts, picked by index, in grays that read well on e-ink."""
    kind = i % 6
    dark = kind in (0, 3)
    bg, fg = (34, 34, 34) if dark else (238, 235, 228), (245, 243, 238) if dark else (30, 30, 30)
    img = Image.new("L", (W, H), bg[0])
    d = ImageDraw.Draw(img)
    fg, mid = fg[0], 130
    if kind == 0:  # dark, framed
        d.rectangle([28, 28, W - 29, H - 29], outline=fg, width=3)
        d.rectangle([40, 40, W - 41, H - 41], outline=mid, width=1)
        y = block(d, title, fit(d, title, "b", W - 140, 76), 230, fg)
        d.line([W / 2 - 60, y + 30, W / 2 + 60, y + 30], fill=mid, width=2)
        block(d, author.upper(), font("r", 30), H - 170, fg)
    elif kind == 1:  # light, big initial
        d.text((44, -40), title.removeprefix("The ")[0], font=font("b", 520), fill=208)
        y = block(d, title, fit(d, title, "b", W - 120, 72), 470, fg, align="left")
        d.line([60, y + 24, 200, y + 24], fill=fg, width=4)
        block(d, author, font("i", 36), y + 52, fg, align="left")
    elif kind == 2:  # light, diagonal hatching band
        for x in range(-H, W, 22):
            d.line([x, 300, x + 260, 560], fill=200, width=6)
        d.rectangle([0, 300, W, 560], outline=fg, width=0)
        block(d, title, fit(d, title, "b", W - 120, 72), 90, fg)
        block(d, author, font("i", 38), 640, fg)
    elif kind == 3:  # dark, concentric circles
        for r in range(60, 420, 34):
            d.ellipse([W / 2 - r, 560 - r, W / 2 + r, 560 + r], outline=70, width=3)
        y = block(d, title, fit(d, title, "b", W - 120, 72), 90, fg)
        block(d, author.upper(), font("r", 28), y + 20, 200)
    elif kind == 4:  # light, bands
        for k, g in enumerate((60, 110, 160, 205)):
            d.rectangle([0, H - 300 + k * 75, W, H - 225 + k * 75], fill=g)
        block(d, author.upper(), font("r", 28), 110, mid)
        block(d, title, fit(d, title, "b", W - 120, 76), 180, fg)
    else:  # light, centred ornament
        d.rectangle([24, 24, W - 25, H - 25], outline=fg, width=2)
        block(d, title, fit(d, title, "b", W - 140, 72), 200, fg)
        cx, cy = W / 2, 560
        d.polygon([(cx, cy - 34), (cx + 34, cy), (cx, cy + 34), (cx - 34, cy)], fill=fg)
        d.line([cx - 150, cy, cx - 50, cy], fill=fg, width=2)
        d.line([cx + 50, cy, cx + 150, cy], fill=fg, width=2)
        block(d, author, font("i", 38), 680, fg)
    buf = io.BytesIO()
    img.save(buf, "PNG", optimize=True)
    return buf.getvalue()


def alice_chapters():
    text = (ROOT / "testdata/formats/alice.txt").read_text(encoding="utf-8-sig").replace("\r\n", "\n")
    body = text.split("*** START OF THE PROJECT GUTENBERG EBOOK", 1)[1].split("\n", 1)[1]
    body = body.split("*** END OF THE PROJECT GUTENBERG EBOOK", 1)[0]
    parts = re.split(r"\n(CHAPTER [IVXL]+\.)\n", body)[1:]
    chapters = []
    for k in range(0, len(parts), 2):
        paras = [re.sub(r"\s+", " ", p).strip() for p in parts[k + 1].split("\n\n")]
        paras = [p for p in paras if p]
        name, paras = paras[0], paras[1:]
        chapters.append((f"{parts[k].title().rstrip('.')}: {name}", paras))
    return chapters


def xhtml(title, heading, paras):
    # Gutenberg marks italics as _words_.
    ps = "\n".join("<p>" + re.sub(r"_([^_]+)_", r"<i>\1</i>", html.escape(p, quote=False)) + "</p>" for p in paras)
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="en">
<head><title>{html.escape(title)}</title><link rel="stylesheet" href="style.css"/></head>
<body><section epub:type="chapter"><h2>{html.escape(heading)}</h2>
{ps}
</section></body></html>
"""


CSS = "h2 { text-align: center; margin: 2em 0 1.5em; font-weight: normal; }\np { margin: 0; text-indent: 1.4em; }\nh2 + p { text-indent: 0; }\n"


def epub(path, n, key, title, author, year, opening):
    chapters = alice_chapters() if opening is None else [("Chapter I", [opening])]
    files = {"OEBPS/style.css": CSS, "OEBPS/cover.png": cover(n, title, author)}
    for k, (head, paras) in enumerate(chapters, 1):
        files[f"OEBPS/c{k:02}.xhtml"] = xhtml(title, head, paras)
    nav = "\n".join(f'<li><a href="c{k:02}.xhtml">{html.escape(h)}</a></li>' for k, (h, _) in enumerate(chapters, 1))
    files["OEBPS/nav.xhtml"] = f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head>
<body><nav epub:type="toc"><h1>Contents</h1><ol>
{nav}
</ol></nav></body></html>
"""
    items = "\n".join(f'    <item id="c{k:02}" href="c{k:02}.xhtml" media-type="application/xhtml+xml"/>' for k in range(1, len(chapters) + 1))
    spine = "".join(f'<itemref idref="c{k:02}"/>' for k in range(1, len(chapters) + 1))
    date = f"{year}" if year > 0 else ""
    files["OEBPS/content.opf"] = f"""<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">urn:rinky:showcase:{key}</dc:identifier>
    <dc:title>{html.escape(title)}</dc:title>
    <dc:creator>{html.escape(author)}</dc:creator>
    <dc:language>en</dc:language>
    {f"<dc:date>{date}</dc:date>" if date else ""}
    <meta property="dcterms:modified">2026-01-01T00:00:00Z</meta>
    <meta name="cover" content="cover"/>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/>
    <item id="css" href="style.css" media-type="text/css"/>
{items}
  </manifest>
  <spine>{spine}</spine>
</package>
"""
    files = {
        "META-INF/container.xml": """<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>
""",
        **files,
    }
    with zipfile.ZipFile(path, "w") as z:
        z.writestr(zipfile.ZipInfo("mimetype", (2026, 1, 1, 0, 0, 0)), "application/epub+zip", zipfile.ZIP_STORED)
        for name in sorted(files):
            data = files[name]
            z.writestr(zipfile.ZipInfo(name, (2026, 1, 1, 0, 0, 0)), data if isinstance(data, bytes) else data.encode(), zipfile.ZIP_DEFLATED)


# A few words of Alice's first chapter, for the dictionary screenshot (written here).
DICT = {
    "marmalade": "<b>marmalade</b> <i>noun</i><br>a preserve made from citrus fruit, especially bitter oranges, "
    "prepared like jam: <i>a jar of orange marmalade</i>.",
    "latitude": "<b>latitude</b> <i>noun</i><ol><li>the angular distance of a place north or south of the "
    "earth's equator.</li><li>scope for freedom of action or thought.</li></ol>",
    "longitude": "<b>longitude</b> <i>noun</i><br>the angular distance of a place east or west of the "
    "meridian at Greenwich, England.",
    "antipathies": "<b>antipathy</b> <i>noun</i> (<i>plural</i> antipathies)<br>a deep-seated feeling of "
    "aversion. Alice means <i>the Antipodes</i>, the far side of the earth.",
    "curtsey": "<b>curtsy</b> (also <b>curtsey</b>) <i>noun</i><br>a woman's or girl's formal greeting, made "
    "by bending the knees with one foot in front of the other.",
    "hedge": "<b>hedge</b> <i>noun</i><br>a fence or boundary formed by closely growing bushes or shrubs.",
    "waistcoat": "<b>waistcoat</b> <i>noun</i><br>a close-fitting waist-length garment, with no sleeves "
    "or collar, worn especially over a shirt and under a jacket.",
}


def stardict(folder):
    """Writes showcase.{ifo,idx,dict} (StarDict 2.4.2, sametypesequence h) into folder."""
    folder.mkdir(parents=True, exist_ok=True)
    data, idx = b"", b""
    for word in sorted(DICT, key=lambda w: (w.lower(), w)):
        entry = DICT[word].encode()
        idx += word.encode() + b"\0" + len(data).to_bytes(4, "big") + len(entry).to_bytes(4, "big")
        data += entry
    (folder / "showcase.dict").write_bytes(data)
    (folder / "showcase.idx").write_bytes(idx)
    (folder / "showcase.ifo").write_text(
        f"StarDict's dict ifo file\nversion=2.4.2\nbookname=Showcase Dictionary\nwordcount={len(DICT)}\n"
        f"idxfilesize={len(idx)}\nsametypesequence=h\n"
    )


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for n, (key, title, author, year, opening) in enumerate(BOOKS):
        p = OUT / f"{key}.epub"
        epub(p, n, key, title, author, year, opening)
        print(p)
    if len(sys.argv) > 2:
        stardict(Path(sys.argv[2]))
        print(sys.argv[2])


if __name__ == "__main__":
    main()
