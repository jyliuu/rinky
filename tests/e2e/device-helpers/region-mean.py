#!/usr/bin/env python3
"""Prints the mean gray value (0..255, integer) of a rectangle of a PNG.

  region-mean.py IMAGE.png X0 Y0 X1 Y1      (X1/Y1 exclusive)

Used by the verifier scenarios to tell the screensaver badge, the USB lock-screen
frame and the test pages apart on emulator screendumps. Needs Pillow.
"""
import sys

from PIL import Image


def main() -> int:
    if len(sys.argv) != 6:
        print(__doc__)
        return 2
    img = Image.open(sys.argv[1]).convert("L")
    x0, y0, x1, y1 = (int(v) for v in sys.argv[2:])
    px = list(img.crop((x0, y0, x1, y1)).getdata())
    print(sum(px) // max(1, len(px)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
