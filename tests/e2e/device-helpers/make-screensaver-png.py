#!/usr/bin/env python3
"""Writes a self-made 1272 x 1696 grayscale PNG for the "chosen image" screensaver test.

  make-screensaver-png.py OUT.png

Concentric frames, a 16-step gray ramp and diagonal stripes; every gray level is a
multiple of 17, so the panel's 16-level quantization keeps it unchanged. Needs Pillow.
"""
import sys

from PIL import Image, ImageDraw

W, H = 1272, 1696


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    img = Image.new("L", (W, H), 255)
    d = ImageDraw.Draw(img)
    # Concentric frames, alternating black and mid gray.
    for i in range(6):
        m = 40 + i * 50
        d.rectangle([m, m, W - 1 - m, H - 1 - m], outline=0 if i % 2 == 0 else 119, width=18)
    # Diagonal stripes in the middle box.
    box = (360, 360, W - 360, 900)
    for x in range(box[0] - (box[3] - box[1]), box[2], 48):
        d.line([(x, box[3]), (x + (box[3] - box[1]), box[1])], fill=0, width=14)
    d.rectangle([box[0] - 1, box[1] - 1, box[2], box[3]], outline=0, width=6)
    # 16-step ramp, dark on the left.
    step = (W - 2 * 360) // 16
    for i in range(16):
        d.rectangle([360 + i * step, 960, 360 + (i + 1) * step - 1, 1160], fill=i * 17)
    # A black disc with a white square: an orientation mark (top-left of the ramp).
    d.ellipse([520, 1240, 760, 1480], fill=0)
    d.rectangle([600, 1320, 680, 1400], fill=255)
    img.save(sys.argv[1])
    return 0


if __name__ == "__main__":
    sys.exit(main())
