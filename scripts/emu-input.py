#!/usr/bin/env python3
"""Touch input for an eink-emulator instance over its QMP socket.

Uses the emulator's own QMP client (imported read-only from EMU/eink_emulator).
Coordinates are panel pixels, 0..1272 x 0..1696 on the PW12, scaled the same
way as `eink-qmp.py tap` (inverse ceiling, so the guest receives the exact
requested coordinate).

  emu-input.py --machine NAME tap X Y [--hold S]
  emu-input.py --machine NAME swipe X1 Y1 X2 Y2 [--steps N] [--duration S]
  emu-input.py --machine NAME hold X Y SECONDS
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

EMU = Path(os.environ.get("EMU", str(Path(__file__).resolve().parents[2] / "eink-emulator")))
sys.path.insert(0, str(EMU))

from eink_emulator.qmp import QMPClient, QMPError  # noqa: E402

HWTCON = "/machine/soc/hwtcon"


def scanout(client: QMPClient) -> tuple[int, int]:
    w = client.execute("qom-get", {"path": HWTCON, "property": "scanout-width"})
    h = client.execute("qom-get", {"path": HWTCON, "property": "scanout-height"})
    if w <= 1 or h <= 1:
        raise QMPError("display scanout is not initialized yet")
    return w - 1, h - 1


def to_abs(x: int, y: int, max_x: int, max_y: int) -> tuple[int, int]:
    if not (0 <= x <= max_x and 0 <= y <= max_y):
        raise QMPError(f"coordinates must be within 0..{max_x} x 0..{max_y}")
    return (x * 0x7FFF + max_x - 1) // max_x, (y * 0x7FFF + max_y - 1) // max_y


def move(client: QMPClient, ax: int, ay: int, button: bool | None) -> None:
    events = [
        {"type": "abs", "data": {"axis": "x", "value": ax}},
        {"type": "abs", "data": {"axis": "y", "value": ay}},
    ]
    if button is not None:
        events.append({"type": "btn", "data": {"button": "left", "down": button}})
    client.execute("input-send-event", {"events": events})


def release(client: QMPClient) -> None:
    client.execute("input-send-event", {
        "events": [{"type": "btn", "data": {"button": "left", "down": False}}]})


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--machine", required=True)
    p.add_argument("--timeout", type=float, default=5.0)
    sub = p.add_subparsers(dest="action", required=True)
    t = sub.add_parser("tap")
    t.add_argument("x", type=int)
    t.add_argument("y", type=int)
    t.add_argument("--hold", type=float, default=0.08)
    s = sub.add_parser("swipe")
    for name in ("x1", "y1", "x2", "y2"):
        s.add_argument(name, type=int)
    s.add_argument("--steps", type=int, default=8)
    s.add_argument("--duration", type=float, default=0.4)
    h = sub.add_parser("hold")
    h.add_argument("x", type=int)
    h.add_argument("y", type=int)
    h.add_argument("seconds", type=float)
    args = p.parse_args()
    if Path(args.machine).name != args.machine:
        p.error("machine name cannot contain a path separator")
    sock = EMU / "machines" / args.machine / f"{args.machine}.qmp.sock"
    try:
        with QMPClient(sock, args.timeout) as client:
            mx, my = scanout(client)
            if args.action in ("tap", "hold"):
                ax, ay = to_abs(args.x, args.y, mx, my)
                move(client, ax, ay, True)
                time.sleep(args.hold if args.action == "tap" else args.seconds)
                release(client)
                print(json.dumps({"action": args.action, "x": args.x, "y": args.y}))
            else:
                steps = max(1, args.steps)
                pts = [
                    (round(args.x1 + (args.x2 - args.x1) * i / steps),
                     round(args.y1 + (args.y2 - args.y1) * i / steps))
                    for i in range(steps + 1)
                ]
                ax, ay = to_abs(*pts[0], mx, my)
                move(client, ax, ay, True)
                for x, y in pts[1:]:
                    time.sleep(args.duration / steps)
                    ax, ay = to_abs(x, y, mx, my)
                    move(client, ax, ay, None)
                release(client)
                print(json.dumps({"action": "swipe", "points": pts}))
    except (OSError, QMPError) as error:
        print(f"emu-input: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
