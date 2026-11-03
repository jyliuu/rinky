#!/usr/bin/env python3
"""MOBI, KFX and Topaz fixtures for testdata/formats (called by make-fixtures.py).

  python3 -I testdata/formats/make-fixtures-mobi.py            # huffcdic.mobi, KFX/Topaz fakes, drm-sample.azw
  python3 -I testdata/formats/make-fixtures-mobi.py --calibre  # also sample-calibre*.{azw3,mobi} and fonts-calibre.azw3

- huffcdic.mobi: a MOBI 6 book written here with HUFF/CDIC compression (8-bit codes,
  10-bit codes through the dict2 table, and a phrase that is itself compressed). Its
  text is also written to huffcdic.expected.html so tests can compare.
- drm-sample.azw: sample-calibre-old.mobi with the record 0 encryption field set to 2
  (the text is not really encrypted; readers must refuse on the flag).
- kfx-container.kfx: a KFX CONT container with a book_metadata entity (Ion written
  here, field ids as used by calibre's KFX Input plugin), DRM scheme 1.
- kfx-drmion.kfx: a DRMION header followed by bytes.  topaz.azw1: a TPZ header.
"""
import struct
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def write(name, data):
    p = HERE / name
    p.write_bytes(data)
    print(f"{name}: {len(data)} bytes")


# ---------------------------------------------------------------- PalmDB / MOBI

def palmdb(name, records, tc=b"BOOKMOBI"):
    out = bytearray(name.encode()[:31].ljust(32, b"\0"))
    out += struct.pack(">HHIIIIII", 0, 0, 0x7A2B2B2B, 0x7A2B2B2B, 0, 0, 0, 0)
    out += tc
    out += struct.pack(">IIH", 2 * len(records), 0, len(records))
    off = 78 + 8 * len(records) + 2
    for i, r in enumerate(records):
        out += struct.pack(">II", off, 2 * i)
        off += len(r)
    out += b"\0\0"
    for r in records:
        out += r
    return bytes(out)


def exth(recs):
    body = b"".join(struct.pack(">II", t, len(d) + 8) + d for t, d in recs)
    out = b"EXTH" + struct.pack(">II", len(body) + 12, len(recs)) + body
    return out + b"\0" * (-len(out) % 4)


def record0(compression, text_len, n_text, title, ex, huff_rec=0xFFFFFFFF, huff_count=0, first_non_book=0, first_image=0xFFFFFFFF):
    hl = 0xE8
    r = bytearray(0x10 + hl)
    struct.pack_into(">HHIHHHH", r, 0, compression, 0, text_len, n_text, 4096, 0, 0)
    r[16:20] = b"MOBI"
    vals = {0x14: hl, 0x18: 2, 0x1C: 65001, 0x20: 0x1234ABCD, 0x24: 6, 0x28: 0xFFFFFFFF, 0x2C: 0xFFFFFFFF,
            0x50: first_non_book, 0x5C: 9, 0x68: 6, 0x6C: first_image, 0x70: huff_rec, 0x74: huff_count,
            0x80: 0x40, 0xA4: 0xFFFFFFFF, 0xA8: 0xFFFFFFFF, 0xB0: 0, 0xC0: 0xFFFFFFFF, 0xC8: 0xFFFFFFFF, 0xD0: 0xFFFFFFFF,
            0xF4: 0xFFFFFFFF}
    for o, v in vals.items():
        struct.pack_into(">I", r, o, v)
    struct.pack_into(">H", r, 0xC0, 1)
    struct.pack_into(">H", r, 0xC2, n_text)
    struct.pack_into(">H", r, 0xF2, 0)  # no trailing entries
    e = exth(ex)
    name_off = len(r) + len(e)
    struct.pack_into(">II", r, 0x54, name_off, len(title.encode()))
    return bytes(r) + e + title.encode() + b"\0\0"


# HUFF/CDIC with the same code layout as the Rust test encoder in mobi/compress.rs:
# 8-bit codes c = 0x01..0xFF stand for phrases 255 - c (0..254); 10-bit codes
# 00000000 xy stand for phrases 259 - xy (256..259).

def huff_record():
    out = b"HUFF" + struct.pack(">III", 24, 24, 24 + 1024) + b"\0" * 8
    for c in range(256):
        out += struct.pack(">I", 9 if c == 0 else (8 | 0x80 | (255 << 8)))
    for length in range(1, 33):
        lo, hi = {9: (0x1FF, 0), 10: (0, 259)}.get(length, ((0xFFFFFFFF >> (32 - length)), 0))
        out += struct.pack(">II", lo, hi)
    return out


def bits_for(r):
    if r <= 254:
        return format(255 - r, "08b")
    return "00000000" + format(259 - r, "02b")


def pack_bits(bits):
    bits += "0" * (-len(bits) % 8)
    return bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))


PHRASES = {257: b"the ", 259: b"</p>\n<p>"}


def encode(data):
    """Greedy: multi-byte phrases first, then single bytes (phrase index = byte)."""
    bits = []
    i = 0
    while i < len(data):
        for idx in (258, 257, 259):
            p = PHRASES[idx]
            if data.startswith(p, i):
                bits.append(bits_for(idx))
                i += len(p)
                break
        else:
            b = data[i]
            assert b <= 254
            bits.append(bits_for(b))
            i += 1
    return pack_bits("".join(bits))


def cdic_record():
    phrases = []
    for i in range(260):
        if i <= 254:
            phrases.append((bytes([i]), True))
        elif i == 255:
            phrases.append((b"", True))
        elif i == 256:
            phrases.append((b"\xff", True))
        elif i == 258:
            # Compressed phrase: "Chapter " written with byte codes.
            phrases.append((pack_bits("".join(bits_for(b) for b in b"Chapter ")), False))
        else:
            phrases.append((PHRASES[i], True))
    table = b""
    body = b""
    for p, literal in phrases:
        table += struct.pack(">H", len(phrases) * 2 + len(body))
        body += struct.pack(">H", len(p) | (0x8000 if literal else 0)) + p
    return b"CDIC" + struct.pack(">III", 16, len(phrases), 9) + table + body


def huffcdic_book():
    PHRASES[258] = b"Chapter "
    paras = "".join(f"<p>This is paragraph {n} of the HUFF/CDIC test; the text goes on with the same words.</p>\n" for n in range(1, 40))
    html = ("<html><head><guide></guide></head><body>"
            "<h2>Chapter One</h2>\n<p>" + paras + "</p>\n<mbp:pagebreak/>"
            "<h2>Chapter Two</h2>\n<p>The end of the test.</p>\n" + paras + "</body></html>").encode()
    recs = [html[i:i + 4096] for i in range(0, len(html), 4096)]
    text = [encode(r) for r in recs]
    huff = len(text) + 1
    r0 = record0(17480, len(html), len(text), "HUFF CDIC Sample", [(100, b"Inkreader Fixtures"), (503, b"HUFF/CDIC Sample"), (524, b"en")],
                 huff_rec=huff, huff_count=2, first_non_book=huff)
    eof = b"\xe9\x8e\r\n"
    write("huffcdic.mobi", palmdb("HUFF_CDIC_Sample", [r0] + text + [huff_record(), cdic_record(), eof]))
    write("huffcdic.expected.html", html)


# ---------------------------------------------------------------- KFX (Ion)

def varuint(v):
    out = [v & 0x7F | 0x80]
    v >>= 7
    while v:
        out.insert(0, v & 0x7F)
        v >>= 7
    return bytes(out)


def tl(t, body):
    if len(body) < 14:
        return bytes([t << 4 | len(body)]) + body
    return bytes([t << 4 | 14]) + varuint(len(body)) + body


def ion_str(s):
    return tl(8, s.encode())


def ion_int(v):
    b = v.to_bytes(8, "big").lstrip(b"\0")
    return tl(2, b)


def ion_list(items):
    return tl(11, b"".join(items))


def ion_struct(fields):
    return tl(13, b"".join(varuint(k) + v for k, v in fields))


BVM = b"\xe0\x01\x00\xea"


def kfx_container():
    meta = [("title", "KFX Sample Title"), ("author", "Inkreader Fixtures"), ("language", "en"), ("ASIN", "B0INKTEST0"), ("cde_content_type", "EBOK")]
    entries = [ion_struct([(492, ion_str(k)), (307, ion_str(v))]) for k, v in meta]
    value = BVM + ion_struct([(491, ion_list([ion_struct([(495, ion_str("kindle_title_metadata")), (258, ion_list(entries))])]))])
    ehdr = ion_struct([(410, ion_int(0)), (411, ion_int(0))])
    ent = b"ENTY" + struct.pack("<HI", 1, 10 + len(ehdr)) + ehdr + value
    drm_ent = b"ENTY" + struct.pack("<HI", 1, 10 + len(ehdr)) + ehdr + b"\xeaDRMION\xee" + bytes(range(64))
    header_len = 18
    idx = struct.pack("<IIQQ", 1000, 490, 0, len(ent)) + struct.pack("<IIQQ", 1001, 145, len(ent), len(drm_ent))
    idx_off = header_len + len(ent) + len(drm_ent)
    ci = ion_struct([(409, ion_str("CR!INKREADERTEST")), (411, ion_int(1)), (413, ion_int(idx_off)), (414, ion_int(len(idx)))])
    ci_off = idx_off + len(idx)
    return b"CONT" + struct.pack("<HIII", 2, header_len, ci_off, len(ci)) + ent + drm_ent + idx + ci


# ---------------------------------------------------------------- main

def calibre():
    src = HERE / "epub2-ncx.epub"
    for out, extra in [("sample-calibre.azw3", []), ("sample-calibre-old.mobi", ["--mobi-file-type", "old"]),
                       ("sample-calibre-joint.mobi", ["--mobi-file-type", "both"])]:
        subprocess.run(["ebook-convert", str(src), str(HERE / out)] + extra, check=True, stdout=subprocess.DEVNULL)
        print(f"{out}: {(HERE / out).stat().st_size} bytes (calibre)")
    subprocess.run(["ebook-convert", str(HERE / "epub3-nav.epub"), str(HERE / "fonts-calibre.azw3")], check=True, stdout=subprocess.DEVNULL)
    print(f"fonts-calibre.azw3: {(HERE / 'fonts-calibre.azw3').stat().st_size} bytes (calibre)")


def drm_copy():
    src = HERE / "sample-calibre-old.mobi"
    if not src.exists():
        return
    data = bytearray(src.read_bytes())
    rec0 = struct.unpack_from(">I", data, 78)[0]
    struct.pack_into(">H", data, rec0 + 12, 2)
    write("drm-sample.azw", bytes(data))


def main():
    if "--calibre" in sys.argv:
        calibre()
    huffcdic_book()
    drm_copy()
    write("kfx-container.kfx", kfx_container())
    write("kfx-drmion.kfx", b"\xeaDRMION\xee" + bytes((i * 7) & 0xFF for i in range(256)))
    write("topaz.azw1", b"TPZ0" + bytes(252))


if __name__ == "__main__":
    main()
