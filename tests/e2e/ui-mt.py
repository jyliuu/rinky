#!/usr/bin/env python3
"""Writes raw multi-touch input_event records (ARM32, 16 bytes each, protocol B) for
gestures the emulator's single-pointer QMP input cannot make. The ui scenarios copy the
file to the guest and write it into the touch device node (`cat FILE > /dev/input/event1`):
the kernel delivers written records to the reader that grabbed the device, with its own
timestamps.

  ui-mt.py DIR twotap X1 Y1 X2 Y2        two fingers down in one frame, then up
  ui-mt.py DIR pinch CX CY D0 D1         two fingers on a horizontal line, distance D0 -> D1

One file per SYN frame (DIR/00.bin, 01.bin, ...): the scenario writes them one by one
with a pause, so the reader's evdev buffer never overflows (a burst of a whole pinch
did: the kernel dropped events and no pinch was seen).
"""

import os
import struct
import sys

EV_SYN, EV_ABS = 0, 3
ABS_MT_SLOT, ABS_MT_TOUCH_MAJOR, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_TRACKING_ID = 0x2F, 0x30, 0x35, 0x36, 0x39


def ev(t, c, v):
    return struct.pack("<iiHHi", 0, 0, t, c, v)


def contact(slot, x, y, tid=None):
    out = ev(EV_ABS, ABS_MT_SLOT, slot)
    if tid is not None:
        out += ev(EV_ABS, ABS_MT_TRACKING_ID, tid) + ev(EV_ABS, ABS_MT_TOUCH_MAJOR, 30)
    return out + ev(EV_ABS, ABS_MT_POSITION_X, x) + ev(EV_ABS, ABS_MT_POSITION_Y, y)


def lift(slot):
    return ev(EV_ABS, ABS_MT_SLOT, slot) + ev(EV_ABS, ABS_MT_TRACKING_ID, -1)


SYN = ev(EV_SYN, 0, 0)


def main():
    out, kind = sys.argv[1], sys.argv[2]
    n = [int(v) for v in sys.argv[3:]]
    if kind == "twotap":
        x1, y1, x2, y2 = n
        frames = [contact(0, x1, y1, 900) + contact(1, x2, y2, 901) + SYN, lift(0) + lift(1) + SYN]
    elif kind == "pinch":
        cx, cy, d0, d1 = n
        frames = [contact(0, cx - d0 // 2, cy, 910) + contact(1, cx + d0 // 2, cy, 911) + SYN]
        for i in range(1, 7):
            d = d0 + (d1 - d0) * i // 6
            frames.append(contact(0, cx - d // 2, cy) + contact(1, cx + d // 2, cy) + SYN)
        frames.append(lift(0) + lift(1) + SYN)
    else:
        sys.exit(__doc__)
    os.makedirs(out, exist_ok=True)
    for i, f in enumerate(frames):
        with open(os.path.join(out, "%02d.bin" % i), "wb") as fh:
            fh.write(f)


main()
