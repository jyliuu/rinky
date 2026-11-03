#!/usr/bin/env python3
"""Touch helpers for the ui scenarios that scripts/emu-input.py does not offer.

  ui-qmp.py --machine NAME taps X1 Y1 [X2 Y2 ...] [--gap S]   several quick taps in one
                                                             QMP session (double taps)
  ui-qmp.py --machine NAME drag X1 Y1 X2 Y2 --hold S          finger down, rest S seconds,
                                                             slide to X2 Y2, lift

Coordinates are panel pixels; scaling is the one of scripts/emu-input.py.
"""

from __future__ import annotations

import argparse
import os
import sys
import time
from pathlib import Path

EMU = Path(os.environ.get("EMU", str(Path(__file__).resolve().parents[3] / "eink-emulator")))
sys.path.insert(0, str(EMU))

from eink_emulator.qmp import QMPClient  # noqa: E402

HWTCON = "/machine/soc/hwtcon"


def scanout(c: QMPClient) -> tuple[int, int]:
    w = c.execute("qom-get", {"path": HWTCON, "property": "scanout-width"})
    h = c.execute("qom-get", {"path": HWTCON, "property": "scanout-height"})
    return w - 1, h - 1


def to_abs(x: int, y: int, mx: int, my: int) -> tuple[int, int]:
    return (x * 0x7FFF + mx - 1) // mx, (y * 0x7FFF + my - 1) // my


def send(c: QMPClient, x: int, y: int, mx: int, my: int, down: bool | None) -> None:
    ax, ay = to_abs(x, y, mx, my)
    ev = [{"type": "abs", "data": {"axis": "x", "value": ax}}, {"type": "abs", "data": {"axis": "y", "value": ay}}]
    if down is not None:
        ev.append({"type": "btn", "data": {"button": "left", "down": down}})
    c.execute("input-send-event", {"events": ev})


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--machine", required=True)
    sub = p.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("taps")
    t.add_argument("coords", type=int, nargs="+")
    t.add_argument("--gap", type=float, default=0.05)
    d = sub.add_parser("drag")
    for n in ("x1", "y1", "x2", "y2"):
        d.add_argument(n, type=int)
    d.add_argument("--hold", type=float, default=1.0)
    a = p.parse_args()
    if Path(a.machine).name != a.machine:
        p.error("bad machine name")
    sock = EMU / "machines" / a.machine / f"{a.machine}.qmp.sock"
    with QMPClient(sock, 5.0) as c:
        mx, my = scanout(c)
        if a.cmd == "taps":
            pts = list(zip(a.coords[0::2], a.coords[1::2]))
            for i, (x, y) in enumerate(pts):
                send(c, x, y, mx, my, True)
                time.sleep(0.05)
                send(c, x, y, mx, my, False)
                if i + 1 < len(pts):
                    time.sleep(a.gap)
        else:
            send(c, a.x1, a.y1, mx, my, True)
            time.sleep(a.hold)
            for i in range(1, 7):
                x = a.x1 + (a.x2 - a.x1) * i // 6
                y = a.y1 + (a.y2 - a.y1) * i // 6
                send(c, x, y, mx, my, None)
                time.sleep(0.05)
            time.sleep(0.2)
            send(c, a.x2, a.y2, mx, my, False)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
