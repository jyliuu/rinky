#!/usr/bin/env python3
"""Compares a PNG drawn on the panel with an emulator screendump by e-ink level.

  compare-levels.py DRAWN.png SCREENDUMP.png

The panel has 16 gray levels: a framebuffer value v is level v // 17 (values that are
multiples of 17 map exactly), and the emulator's screendump draws level k as k * 16
(white paper = 240). Prints the level mapping and the number of pixels whose levels
differ; exit status 1 if any differ or the sizes do not match. Needs Pillow.
"""
import sys
from collections import Counter

from PIL import Image


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    a = Image.open(sys.argv[1]).convert("L")
    b = Image.open(sys.argv[2]).convert("L")
    if a.size != b.size:
        print(f"SIZE {a.size} vs {b.size}")
        return 1
    pairs = Counter(zip(a.getdata(), b.getdata()))
    bad = 0
    for (x, y), n in sorted(pairs.items()):
        ok = x // 17 == y // 16
        bad += 0 if ok else n
        print(f"drawn {x:3} (level {x // 17:2}) -> screendump {y:3} (level {y // 16:2}): {n} px{'' if ok else '  MISMATCH'}")
    print(f"{a.size[0]}x{a.size[1]}: {bad} pixels with a different level")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
