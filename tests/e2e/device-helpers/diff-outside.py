#!/usr/bin/env python3
"""Counts differing pixels of two same-size PNGs outside one rectangle.

  diff-outside.py A.png B.png X0 Y0 X1 Y1      (X1/Y1 exclusive)

Prints the count; exit status 1 when it is not zero. Used to compare a "current page"
screensaver (page + badge) with the page after the wake. Needs Pillow.
"""
import sys

from PIL import Image


def main() -> int:
    if len(sys.argv) != 7:
        print(__doc__)
        return 2
    a = Image.open(sys.argv[1]).convert("L")
    b = Image.open(sys.argv[2]).convert("L")
    if a.size != b.size:
        print(f"size {a.size} vs {b.size}")
        return 1
    x0, y0, x1, y1 = (int(v) for v in sys.argv[3:])
    w = a.size[0]
    n = 0
    for i, (p, q) in enumerate(zip(a.getdata(), b.getdata())):
        if p != q:
            x, y = i % w, i // w
            if not (x0 <= x < x1 and y0 <= y < y1):
                n += 1
    print(n)
    return 1 if n else 0


if __name__ == "__main__":
    sys.exit(main())
