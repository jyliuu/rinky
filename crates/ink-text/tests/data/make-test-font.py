#!/usr/bin/env python3
"""Writes InkTestFallback.ttf: a tiny self-made fallback font for ink-text tests.

It covers a few CJK ideographs, one Hangul syllable and some symbols that the embedded
Noto Serif lacks. Every glyph is a different simple box drawing, so tests can tell the
glyphs apart and see that they are not the .notdef box. No outlines come from another
font; the file is part of rinky (AGPL-3.0-or-later) and may be used freely in tests.

    python3 crates/ink-text/tests/data/make-test-font.py crates/ink-text/tests/data/InkTestFallback.ttf

With `--kern`, it writes InkTestKern.ttf instead: the same glyphs as "InkTest Kern" with a
legacy `kern` table (日 then 本 closer by 250 units), the kerning the prototype path reads.

    python3 crates/ink-text/tests/data/make-test-font.py --kern crates/ink-text/tests/data/kern/InkTestKern.ttf
"""

import sys

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen

UPEM = 1000
ADVANCE = 1000
# Code point -> glyph name. The order fixes the drawing pattern of each glyph.
CHARS = [
    (0x65E5, "uni65E5"),  # 日
    (0x672C, "uni672C"),  # 本
    (0x8A9E, "uni8A9E"),  # 語
    (0x4E2D, "uni4E2D"),  # 中
    (0x6587, "uni6587"),  # 文
    (0x6F22, "uni6F22"),  # 漢
    (0x5B57, "uni5B57"),  # 字
    (0xD55C, "uniD55C"),  # 한
    (0x2605, "uni2605"),  # ★
    (0x2606, "uni2606"),  # ☆
    (0x2665, "uni2665"),  # ♥
    (0x2192, "uni2192"),  # →
    (0x2190, "uni2190"),  # ←
    (0x232B, "uni232B"),  # ⌫
    (0x21E7, "uni21E7"),  # ⇧
    (0x21B5, "uni21B5"),  # ↵
    (0x2713, "uni2713"),  # ✓
    (0x2630, "uni2630"),  # ☰
]


def rect(pen, x0, y0, x1, y1):
    """Clockwise rectangle (TrueType outer contour)."""
    pen.moveTo((x0, y0))
    pen.lineTo((x0, y1))
    pen.lineTo((x1, y1))
    pen.lineTo((x1, y0))
    pen.closePath()


def frame(pen, x0, y0, x1, y1, t):
    """A rectangular ring of thickness t (outer clockwise, inner counter-clockwise)."""
    rect(pen, x0, y0, x1, y1)
    pen.moveTo((x0 + t, y0 + t))
    pen.lineTo((x1 - t, y0 + t))
    pen.lineTo((x1 - t, y1 - t))
    pen.lineTo((x0 + t, y1 - t))
    pen.closePath()


def glyph(n):
    pen = TTGlyphPen(None)
    frame(pen, 80, -80, 920, 800, 70)
    # n horizontal bars (1..4) and n % 3 vertical bars make each glyph unique.
    bars = n % 4 + 1
    for i in range(bars):
        y = -80 + (i + 1) * 880 // (bars + 1)
        rect(pen, 150, y - 30, 850, y + 30)
    for j in range(n % 3):
        x = 80 + (j + 1) * 840 // (n % 3 + 1)
        rect(pen, x - 30, -10, x + 30, 730)
    if n >= 8:  # symbols get a filled corner block
        rect(pen, 150, 600, 350, 730)
    return pen.glyph()


def notdef():
    pen = TTGlyphPen(None)
    frame(pen, 100, 0, 600, 700, 50)
    return pen.glyph()


def kern_table():
    from fontTools.ttLib import newTable
    from fontTools.ttLib.tables._k_e_r_n import KernTable_format_0

    sub = KernTable_format_0()
    sub.version, sub.coverage, sub.format = 0, 1, 0
    sub.kernTable = {("uni65E5", "uni672C"): -250}
    kern = newTable("kern")
    kern.version, kern.kernTables = 0, [sub]
    return kern


def main(out, kern=False):
    names = [".notdef", "space"] + [n for _, n in CHARS]
    fb = FontBuilder(UPEM, isTTF=True)
    fb.setupGlyphOrder(names)
    cmap = {0x20: "space"}
    cmap.update({cp: n for cp, n in CHARS})
    fb.setupCharacterMap(cmap)
    glyphs = {".notdef": notdef(), "space": TTGlyphPen(None).glyph()}
    for i, (_, n) in enumerate(CHARS):
        glyphs[n] = glyph(i)
    fb.setupGlyf(glyphs)
    metrics = {n: (ADVANCE, 0) for n in names}
    metrics[".notdef"] = (700, 100)
    metrics["space"] = (300, 0)
    for n, g in glyphs.items():
        if hasattr(g, "xMin"):
            metrics[n] = (metrics[n][0], g.xMin)
    fb.setupHorizontalMetrics(metrics)
    fb.setupHorizontalHeader(ascent=880, descent=-120)
    fb.setupNameTable({"familyName": "InkTest Kern" if kern else "InkTest Fallback", "styleName": "Regular"})
    fb.setupOS2(sTypoAscender=880, sTypoDescender=-120, usWinAscent=880, usWinDescent=120, usWeightClass=400)
    fb.setupPost()
    if kern:
        fb.font["kern"] = kern_table()
    fb.save(out)


if __name__ == "__main__":
    if sys.argv[1] == "--kern":
        main(sys.argv[2], kern=True)
    else:
        main(sys.argv[1])
