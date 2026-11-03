"""Generates the rinky logo options in docs/logo/: each mark with a light and a dark header
lockup, plus a wordmark-only logo. The wordmark and tagline are Noto Serif outlines, so the
SVGs need no fonts. preview.html shows them all.

  python3 -I docs/logo/make-logos.py .     (from the repository root; needs fontTools)
"""
import sys, pathlib
from PIL import Image, ImageDraw, ImageFont
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.pens.boundsPen import BoundsPen

ROOT = pathlib.Path(sys.argv[1]); OUT = ROOT / "docs/logo"
FONT = TTFont(ROOT / "fonts/NotoSerif-Regular.ttf")
INK, PAPER, GRAY = "#1c1b19", "#f5f2ea", "#8a867d"

def text_path(s, size, x, y, font=FONT, track=0.0):
    """Outlines of s, baseline at y, starting at x; returns (d, width)."""
    gs = font.getGlyphSet(); cmap = font.getBestCmap(); upm = font["head"].unitsPerEm
    k = size / upm; pen = SVGPathPen(gs); cx = 0.0
    for ch in s:
        g = cmap[ord(ch)]
        tp = TransformPen(pen, (k, 0, 0, -k, x + cx, y))
        gs[g].draw(tp); cx += gs[g].width * k + track
    return pen.getCommands(), cx - track

DROP = "M0,-1 C0.35,-0.45 0.62,-0.12 0.62,0.24 C0.62,0.6 0.34,0.86 0,0.86 C-0.34,0.86 -0.62,0.6 -0.62,0.24 C-0.62,-0.12 -0.35,-0.45 0,-1Z"
def drop(cx, cy, r, fill):
    return f'<path transform="translate({cx} {cy}) scale({r})" d="{DROP}" fill="{fill}"/>'

# Each mark draws in a 512 box with colours (fg, bg).
def mark_drop_book(fg, bg):
    return f'''<rect width="512" height="512" rx="112" fill="{bg}"/>
<path d="M256 178 C206 150 142 142 92 150 L92 382 C142 374 206 382 256 410 Z" fill="none" stroke="{fg}" stroke-width="22" stroke-linejoin="round"/>
<path d="M256 178 C306 150 370 142 420 150 L420 382 C370 374 306 382 256 410 Z" fill="{fg}"/>
<g stroke="{bg}" stroke-width="12" stroke-linecap="round"><path d="M300 214 C330 202 358 198 384 198"/><path d="M300 256 C330 244 358 240 384 240"/><path d="M300 298 C330 286 358 282 384 282"/><path d="M300 340 C330 328 358 324 384 324"/></g>
{drop(256, 112, 46, fg)}'''

def mark_dotted_i(fg, bg):
    d, w = text_path("ı", 430, 0, 0)  # dotless i, serif
    return f'''<rect width="512" height="512" rx="112" fill="{bg}"/>
<path transform="translate({256 - w/2} 432)" d="{d}" fill="{fg}"/>
{drop(256, 118, 50, fg)}'''

def mark_device(fg, bg):
    lines = "".join(f'<rect x="170" y="{y}" width="{w}" height="14" rx="7" fill="{fg}"/>' for y, w in [(150,172),(184,172),(218,172),(252,120),(304,172),(338,150)])
    return f'''<rect width="512" height="512" rx="112" fill="{bg}"/>
<rect x="124" y="70" width="264" height="372" rx="38" fill="none" stroke="{fg}" stroke-width="22"/>
{lines}
{drop(324, 396, 44, fg)}'''

def mark_ribbon(fg, bg):
    return f'''<rect width="512" height="512" rx="112" fill="{bg}"/>
<path d="M112 120 C112 98 128 86 150 86 L362 86 C384 86 400 98 400 120 L400 414 C340 392 300 392 256 418 C212 392 172 392 112 414 Z" fill="none" stroke="{fg}" stroke-width="22" stroke-linejoin="round"/>
<path d="M226 86 L226 268 C226 300 286 300 286 268 L286 86 Z" fill="{fg}"/>
<path transform="translate(256 290) scale(30)" d="{DROP}" fill="{fg}"/>'''

def mark_monogram(fg, bg):
    d, w = text_path("r", 340, 0, 0)
    return f'''<circle cx="256" cy="256" r="240" fill="{bg}"/>
<circle cx="256" cy="256" r="206" fill="none" stroke="{fg}" stroke-width="10"/>
<path transform="translate({256 - w/2 + 6} 350)" d="{d}" fill="{fg}"/>'''

def glyph_bounds(s, size, font=FONT):
    """(xmin, ymin, xmax, ymax) of s at size, y up, origin at the baseline."""
    gs = font.getGlyphSet(); k = size / font["head"].unitsPerEm
    pen = BoundsPen(gs); gs[font.getBestCmap()[ord(s)]].draw(pen)
    return tuple(v * k for v in pen.bounds)

def r_centered(size, cy=256):
    """Path and translate for a serif r centred in the 512 box."""
    d, _ = text_path("r", size, 0, 0)
    x0, y0, x1, y1 = glyph_bounds("r", size)
    return d, 256 - (x0 + x1) / 2, cy + (y0 + y1) / 2

def mark_drop_r(fg, bg):
    d, tx, ty = r_centered(250, 300)
    return f'''<rect width="512" height="512" rx="112" fill="{bg}"/>
<path transform="translate(256 286) scale(196 214)" d="{DROP}" fill="{fg}"/>
<path transform="translate({tx} {ty})" d="{d}" fill="{bg}"/>'''

def mark_ring(fg, bg):
    return f'''<rect width="512" height="512" rx="112" fill="{bg}"/>
<circle cx="256" cy="256" r="150" fill="none" stroke="{fg}" stroke-width="44"/>
{drop(256, 262, 62, fg)}'''

def mark_halftone(fg, bg):
    """A serif r drawn as e-ink dots: dot size follows the glyph's coverage."""
    n, cell = 17, 21
    big = Image.new("L", (800, 800), 0)
    ImageDraw.Draw(big).text((400, 400), "r", font=ImageFont.truetype(str(ROOT / "fonts/NotoSerif-Bold.ttf"), 700), fill=255, anchor="mm")
    x0, y0, x1, y1 = big.getbbox()
    side = max(x1 - x0, y1 - y0) * 1.08
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    small = big.crop((int(cx - side / 2), int(cy - side / 2), int(cx + side / 2), int(cy + side / 2))).resize((n, n), Image.BOX)
    x0 = 256 - n * cell / 2 + cell / 2
    dots = []
    for j in range(n):
        for i in range(n):
            v = small.getpixel((i, j)) / 255
            if v > 0.08:
                dots.append(f'<circle cx="{x0 + i * cell:.1f}" cy="{x0 + j * cell:.1f}" r="{cell * 0.5 * v ** 0.5:.1f}"/>')
    return f'''<rect width="512" height="512" rx="112" fill="{bg}"/>
<g fill="{fg}">{"".join(dots)}</g>'''

def mark_page_curl(fg, bg):
    d, tx, ty = r_centered(230, 290)
    return f'''<rect width="512" height="512" rx="112" fill="{bg}"/>
<path d="M120 96 L318 96 L392 170 L392 416 L120 416 Z" fill="none" stroke="{fg}" stroke-width="22" stroke-linejoin="round"/>
<path d="M318 96 L318 170 L392 170" fill="none" stroke="{fg}" stroke-width="22" stroke-linejoin="round"/>
<path transform="translate({tx} {ty})" d="{d}" fill="{fg}"/>'''

def mark_spines(fg, bg):
    return f'''<rect width="512" height="512" rx="112" fill="{bg}"/>
<rect x="104" y="150" width="62" height="262" rx="8" fill="{fg}"/>
<rect x="182" y="110" width="62" height="302" rx="8" fill="none" stroke="{fg}" stroke-width="18"/>
<rect x="260" y="170" width="62" height="242" rx="8" fill="{fg}"/>
<g transform="rotate(-16 392 412)"><rect x="350" y="138" width="62" height="274" rx="8" fill="none" stroke="{fg}" stroke-width="18"/></g>
<rect x="88" y="412" width="336" height="18" rx="9" fill="{fg}"/>
{drop(291, 118, 34, fg)}'''

def mark_bookmark(fg, bg):
    d, tx, ty = r_centered(190, 236)
    return f'''<rect width="512" height="512" rx="112" fill="{bg}"/>
<path d="M150 64 L362 64 L362 448 L256 362 L150 448 Z" fill="{fg}"/>
<path transform="translate({tx} {ty})" d="{d}" fill="{bg}"/>'''

MARKS = {"drop-book": mark_drop_book, "dotted-i": mark_dotted_i, "device": mark_device, "ribbon": mark_ribbon, "monogram": mark_monogram,
         "drop-r": mark_drop_r, "ring": mark_ring, "halftone": mark_halftone, "page-curl": mark_page_curl,
         "spines": mark_spines, "bookmark": mark_bookmark}

def svg(w, h, body, title):
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" role="img" aria-label="{title}"><title>{title}</title>\n{body}\n</svg>\n'

def lockup(name, fg, bg, sub):
    """Mark at 128 px plus the serif wordmark and a sans-free tagline, 880x200."""
    word, ww = text_path("rinky", 112, 196, 130, track=2.0)
    tag, tw = text_path(sub, 26, 200, 198, track=1.5)
    mark = MARKS[name](PAPER if fg == INK else INK, fg)  # tile in fg, glyphs in the page colour
    return svg(int(max(ww, tw) + 236), 216, f'<g transform="translate(24 44) scale(0.25)">{mark}</g>\n<path d="{word}" fill="{fg}"/>\n<path d="{tag}" fill="{GRAY}"/>', "rinky")

def wordmark(fg, sub):
    """rinky alone, the dot of the i an ink drop."""
    size, x, y, tr = 140, 24, 150, 2.5
    parts, cx = [], x
    for ch in "rınky":
        d, w = text_path(ch, size, cx, y)
        parts.append(f'<path d="{d}" fill="{fg}"/>')
        if ch == "ı":
            parts.append(drop(cx + w / 2, y - 106, 15, fg))
        cx += w + tr
    tag, tw = text_path(sub, 26, x + 4, y + 70, track=1.5)
    return svg(int(max(cx, x + tw) + 24), y + 90, "\n".join(parts) + f'\n<path d="{tag}" fill="{GRAY}"/>', "rinky")

TAGLINE = "a fast e-book reader for Kindle, in Rust"
(OUT / "wordmark.svg").write_text(wordmark(INK, TAGLINE))
(OUT / "wordmark-dark.svg").write_text(wordmark(PAPER, TAGLINE))

preview = ['<!doctype html><meta charset="utf-8"><title>rinky logos</title><body style="margin:0;font:14px sans-serif">',
           '<div style="display:flex;flex-wrap:wrap;gap:16px;padding:16px;background:#fff">']
preview += [f'<figure style="margin:0;text-align:center"><img src="{n}.svg" width="140"><figcaption>{n}</figcaption></figure>' for n in MARKS]
preview += ['</div><div style="display:flex;flex-wrap:wrap;gap:12px 32px;padding:16px;background:#fff">']
preview += [f'<img src="{n}-header.svg" height="72">' for n in MARKS] + ['<img src="wordmark.svg" height="72">']
preview += ['</div><div style="display:flex;flex-wrap:wrap;gap:12px 32px;padding:16px;background:#0d1117">']
preview += [f'<img src="{n}-header-dark.svg" height="72">' for n in MARKS] + ['<img src="wordmark-dark.svg" height="72">', '</div>']
(OUT / "preview.html").write_text("\n".join(preview) + "\n")

def halftone_dots(fg, x, baseline, size, rows):
    """The halftone r as a letter: e-ink dots filling the box of a Noto Serif r at size,
    left edge x, on baseline. Returns (svg, advance)."""
    big = Image.new("L", (900, 900), 0)
    ImageDraw.Draw(big).text((450, 450), "r", font=ImageFont.truetype(str(ROOT / "fonts/NotoSerif-Bold.ttf"), 700), fill=255, anchor="mm")
    box = big.getbbox()
    gx0, gy0, gx1, gy1 = glyph_bounds("r", size)
    h = gy1 - gy0
    w = h * (box[2] - box[0]) / (box[3] - box[1])
    cols = round(rows * w / h)
    cell = h / rows
    small = big.crop(box).resize((cols, rows), Image.BOX)
    top = baseline - gy1
    dots = []
    for j in range(rows):
        for i in range(cols):
            v = small.getpixel((i, j)) / 255
            if v > 0.08:
                dots.append(f'<circle cx="{x + (i + 0.5) * cell:.1f}" cy="{top + (j + 0.5) * cell:.1f}" r="{cell * 0.5 * v ** 0.5:.2f}"/>')
    return f'<g fill="{fg}">{"".join(dots)}</g>', cols * cell

def halftone_header(fg):
    """rinky with the halftone mark as its r: dots, then "inky" in Noto Serif."""
    size, x, baseline = 120, 16, 112
    dots, w = halftone_dots(fg, x, baseline, size, 13)
    word, ww = text_path("inky", size, x + w + 0.04 * size, baseline, track=2.0)
    return svg(int(x + w + 0.04 * size + ww + 16), baseline + 40, f'{dots}\n<path d="{word}" fill="{fg}"/>', "rinky")

for name, f in MARKS.items():
    (OUT / f"{name}.svg").write_text(svg(512, 512, f(INK, PAPER), f"rinky logo ({name})"))
    (OUT / f"{name}-header.svg").write_text(lockup(name, INK, PAPER, TAGLINE))
    (OUT / f"{name}-header-dark.svg").write_text(lockup(name, PAPER, INK, TAGLINE))
# The halftone header writes its r as the mark itself.
(OUT / "halftone-header.svg").write_text(halftone_header(INK))
(OUT / "halftone-header-dark.svg").write_text(halftone_header(PAPER))
print("ok", list(MARKS))
