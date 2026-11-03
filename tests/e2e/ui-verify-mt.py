#!/usr/bin/env python3
"""Raw multi-touch records for the verifier scenarios (ARM32 input_event, 16 bytes each,
protocol B), one file per SYN frame, written into the touch node by the scenario.

  ui-verify-mt.py DIR ghost X1 Y1 X2 Y2   two fingers down in one frame; then only slot 0
                                         lifts (slot 1's release is "lost")
  ui-verify-mt.py DIR lift1               the missing release of slot 1
  ui-verify-mt.py DIR hold X Y            00.bin: slot 0 down at X Y; 01.bin: slot 0 lifts
                                         (the scenario writes them with a pause between)
"""

import os
import struct
import sys

EV_SYN, EV_ABS = 0, 3
ABS_MT_SLOT, ABS_MT_TOUCH_MAJOR, ABS_MT_POSITION_X, ABS_MT_POSITION_Y, ABS_MT_TRACKING_ID = 0x2F, 0x30, 0x35, 0x36, 0x39


def ev(t, c, v):
    return struct.pack("<iiHHi", 0, 0, t, c, v)


def contact(slot, x, y, tid):
    return (ev(EV_ABS, ABS_MT_SLOT, slot) + ev(EV_ABS, ABS_MT_TRACKING_ID, tid) + ev(EV_ABS, ABS_MT_TOUCH_MAJOR, 30)
            + ev(EV_ABS, ABS_MT_POSITION_X, x) + ev(EV_ABS, ABS_MT_POSITION_Y, y))


def lift(slot):
    return ev(EV_ABS, ABS_MT_SLOT, slot) + ev(EV_ABS, ABS_MT_TRACKING_ID, -1)


SYN = ev(EV_SYN, 0, 0)


def main():
    out, kind = sys.argv[1], sys.argv[2]
    n = [int(v) for v in sys.argv[3:]]
    if kind == "ghost":
        x1, y1, x2, y2 = n
        frames = [contact(0, x1, y1, 920) + contact(1, x2, y2, 921) + SYN, lift(0) + SYN]
    elif kind == "lift1":
        # Back to slot 0 afterwards, as the single-pointer driver expects.
        frames = [lift(1) + ev(EV_ABS, ABS_MT_SLOT, 0) + SYN]
    elif kind == "hold":
        x, y = n
        frames = [contact(0, x, y, 930) + SYN, lift(0) + SYN]
    else:
        sys.exit(__doc__)
    os.makedirs(out, exist_ok=True)
    for i, f in enumerate(frames):
        with open(os.path.join(out, "%02d.bin" % i), "wb") as fh:
            fh.write(f)


main()
