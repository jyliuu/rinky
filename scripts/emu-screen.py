#!/usr/bin/env python3
"""Checks on emulator screenshots (QMP screendump PNGs). Needs Pillow.

  emu-screen.py home SHOT.png            exit 0 when the stock Home screen is shown
  emu-screen.py clean-home SHOT.png REF.png
                                         Home and identical to REF below the status bar
                                         (no toast, no dialog); prints the differing fraction
  emu-screen.py app-error SHOT.png REF.png
                                         exit 0 when the appmgrd "Application Error" dialog
                                         covers Home (its Close button is at 958,960)
  emu-screen.py dark SHOT.png X Y W H    print the fraction of dark pixels (< 64) in a box
  emu-screen.py diff A.png B.png [TOP]   print the fraction of differing pixels below row TOP
  emu-screen.py same A.png B.png [TOP] [MAX]
                                         exit 0 when at most MAX (default 0.002) differ

The Home test looks for the 5.19.5 Home/Library tab bar: rows 1585-1589 black across the
full width and the separator at x 634-637 between rows 1600 and 1680 (measured on
arch-jb, 2026-10-24).
"""

from __future__ import annotations

import sys

try:
    from PIL import Image
except ImportError:  # pragma: no cover
    sys.exit("emu-screen.py needs Pillow (python3 -m pip install pillow)")


def gray(path: str) -> Image.Image:
    return Image.open(path).convert("L")


def dark_fraction(img: Image.Image, box: tuple[int, int, int, int]) -> float:
    x, y, w, h = box
    region = img.crop((x, y, x + w, y + h))
    hist = region.histogram()
    return sum(hist[:64]) / max(1, w * h)


def is_home(img: Image.Image) -> bool:
    if img.size != (1272, 1696):
        return False
    bar = dark_fraction(img, (0, 1585, 1272, 5))
    sep = dark_fraction(img, (634, 1600, 4, 80))
    above = dark_fraction(img, (0, 1575, 1272, 8))
    return bar > 0.98 and sep > 0.7 and above < 0.05


def diff_fraction(a: Image.Image, b: Image.Image, top: int) -> float:
    if a.size != b.size:
        return 1.0
    w, h = a.size
    pa = a.crop((0, top, w, h)).tobytes()
    pb = b.crop((0, top, w, h)).tobytes()
    changed = sum(1 for x, y in zip(pa, pb) if abs(x - y) > 24)
    return changed / max(1, len(pa))


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        print(__doc__)
        return 2
    cmd = argv[1]
    if cmd == "home":
        ok = is_home(gray(argv[2]))
        print("home" if ok else "not-home")
        return 0 if ok else 1
    if cmd == "clean-home" and len(argv) == 4:
        shot, ref = gray(argv[2]), gray(argv[3])
        frac = diff_fraction(shot, ref, 110)
        ok = is_home(shot) and frac <= 0.003
        print(f"{'clean-home' if ok else 'not-clean-home'} {frac:.5f}")
        return 0 if ok else 1
    if cmd == "app-error" and len(argv) == 4:
        shot, ref = gray(argv[2]), gray(argv[3])
        # The dialog is a white box with a frame over the tree: the tree area turns light.
        box = (177, 700, 920, 350)
        ok = is_home(shot) and dark_fraction(ref, box) - dark_fraction(shot, box) > 0.1 \
            and dark_fraction(shot, (862, 912, 192, 4)) > 0.5
        print("app-error" if ok else "no-app-error")
        return 0 if ok else 1
    if cmd == "dark" and len(argv) == 7:
        box = tuple(int(v) for v in argv[3:7])
        print(f"{dark_fraction(gray(argv[2]), box):.4f}")  # type: ignore[arg-type]
        return 0
    if cmd in ("diff", "same") and len(argv) >= 4:
        top = int(argv[4]) if len(argv) > 4 else 0
        frac = diff_fraction(gray(argv[2]), gray(argv[3]), top)
        print(f"{frac:.5f}")
        if cmd == "same":
            limit = float(argv[5]) if len(argv) > 5 else 0.002
            return 0 if frac <= limit else 1
        return 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
