#!/usr/bin/env python3
"""Sets the program interpreter (PT_INTERP) of a 32-bit little-endian ELF in place.

    scripts/set-interp.py BINARY PATH      write PATH (it must fit the existing field)
    scripts/set-interp.py BINARY           print the current interpreter

rinky's ARM binary is linked with a placeholder interpreter as long as the longest
install path (.cargo/config.toml): /mnt/us/rinky/lib////////////ld-musl-armhf.so.1,
which already works on a Kindle. scripts/package.sh writes each platform's real path:
/mnt/us/rinky/lib/ld-musl-armhf.so.1 (Kindle) and
/mnt/onboard/.adds/rinky/lib/ld-musl-armhf.so.1 (Kobo). The rest of the field is
filled with NUL bytes; the kernel reads the path up to the first one.
"""
import struct
import sys

PT_INTERP = 3


def interp_field(data):
    if data[:4] != b"\x7fELF" or data[4] != 1 or data[5] != 1:
        sys.exit("set-interp: not a 32-bit little-endian ELF")
    phoff, = struct.unpack_from("<I", data, 28)
    phentsize, phnum = struct.unpack_from("<HH", data, 42)
    for i in range(phnum):
        p_type, p_offset, _, _, p_filesz = struct.unpack_from("<IIIII", data, phoff + i * phentsize)
        if p_type == PT_INTERP:
            return p_offset, p_filesz
    sys.exit("set-interp: no PT_INTERP (a static binary?)")


def main():
    if len(sys.argv) not in (2, 3):
        sys.exit(__doc__)
    with open(sys.argv[1], "rb") as f:
        data = bytearray(f.read())
    off, size = interp_field(data)
    current = bytes(data[off:off + size]).split(b"\0")[0].decode()
    if len(sys.argv) == 2:
        print(current)
        return
    new = sys.argv[2].encode()
    if not new.startswith(b"/") or len(new) + 1 > size:
        sys.exit(f"set-interp: {sys.argv[2]!r} must be an absolute path of at most {size - 1} bytes")
    data[off:off + size] = new + b"\0" * (size - len(new))
    with open(sys.argv[1], "r+b") as f:
        f.write(data)
    print(f"set-interp: {sys.argv[1]}: {current} -> {sys.argv[2]}", file=sys.stderr)


main()
