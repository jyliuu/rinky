#!/usr/bin/env python3
"""Writes hostile screensaver PNGs (generated at test time, never committed).

  make-hostile-png.py header OUT.png W H   a valid signature and IHDR (8-bit gray, W x H),
                                           then one tiny IDAT and IEND: the header asks for
                                           a W*H-byte buffer, the data ends at once
  make-hostile-png.py valid OUT.png W H    a complete, valid all-white W x H 8-bit gray PNG
                                           (streamed through zlib: small file, W*H pixels)
"""
import struct
import sys
import zlib


def chunk(kind: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)


def main() -> int:
    if len(sys.argv) != 5 or sys.argv[1] not in ("header", "valid"):
        print(__doc__)
        return 2
    mode, out, w, h = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 0, 0, 0, 0)
    with open(out, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", ihdr))
        if mode == "header":
            f.write(chunk(b"IDAT", zlib.compress(b"\x00" + b"\xff" * 64)))
        else:
            z = zlib.compressobj(9)
            row = b"\x00" + b"\xff" * w
            data = bytearray()
            for _ in range(h):
                data += z.compress(row)
            data += z.flush()
            f.write(chunk(b"IDAT", bytes(data)))
        f.write(chunk(b"IEND", b""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
