#!/usr/bin/env python3
"""SNTP responder for the device agent's clock test. Binds 127.0.0.1 only (the guest
reaches it at 10.0.2.2 over the emulated Wi-Fi). Replies with the host time plus
--offset seconds; logs each request. Usage: sntp-mock.py --port P [--offset S] [--count N]"""
import argparse, socket, struct, sys, time

NTP_DELTA = 2208988800

def ntp(t):
    return struct.pack("!II", int(t) + NTP_DELTA, int((t % 1) * (1 << 32)))

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--offset", type=float, default=0.0)
ap.add_argument("--count", type=int, default=0, help="exit after N replies (0 = never)")
a = ap.parse_args()
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.bind(("127.0.0.1", a.port))
print(f"sntp-mock: 127.0.0.1:{a.port} offset {a.offset:+} s", flush=True)
n = 0
while True:
    data, peer = s.recvfrom(512)
    if len(data) < 48:
        continue
    now = time.time() + a.offset
    reply = bytes([0x24, 2, 6, 0xEC]) + b"\0" * 8 + b"INKT" + ntp(now) + data[40:48] + ntp(now) + ntp(now)
    s.sendto(reply, peer)
    n += 1
    print(f"sntp-mock: reply {n} to {peer[0]}:{peer[1]} time {now:.3f}", flush=True)
    if a.count and n >= a.count:
        sys.exit(0)
