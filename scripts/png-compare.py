#!/usr/bin/env python3
"""Compare PNGs by decoded pixels (not file bytes).

  png-compare.py A.png B.png         one pair
  png-compare.py DIR_A DIR_B         every page-*.png / *.png present in DIR_A

Prints the size and number of differing pixels per pair; exit status 1 if any pair
differs or a file is missing. Needs Pillow.
"""
import sys
from pathlib import Path

from PIL import Image, ImageChops


def compare(a: Path, b: Path) -> int:
    if not b.exists():
        print(f"MISSING {b}")
        return -1
    ia, ib = Image.open(a).convert("L"), Image.open(b).convert("L")
    if ia.size != ib.size:
        print(f"SIZE {a.name}: {ia.size} vs {ib.size}")
        return -1
    diff = ImageChops.difference(ia, ib)
    n = sum(1 for v in diff.getdata() if v)
    print(f"{'SAME' if n == 0 else 'DIFF'} {a.name}: {ia.size[0]}x{ia.size[1]}, {n} differing pixels")
    return n


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    a, b = Path(sys.argv[1]), Path(sys.argv[2])
    pairs = [(a, b)] if a.is_file() else [(p, b / p.name) for p in sorted(a.glob("*.png"))]
    bad = sum(1 for x, y in pairs if compare(x, y) != 0)
    print(f"{len(pairs)} compared, {bad} differ")
    return 1 if bad or not pairs else 0


if __name__ == "__main__":
    raise SystemExit(main())
