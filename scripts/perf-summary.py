#!/usr/bin/env python3
"""Reads the logs of scripts/perf-bench.sh and prints one table (median, min, max, n).

  python3 -I scripts/perf-summary.py OUTDIR

Values per case (milliseconds):
  open <book> no cache / cached   `open:` line, reader->page
  page turn                       `frame time` of each `key next|prev` (scripts/perf/reader.txt);
                                  includes the headless panel emulation the Kindle does not run
  page turn: render page + bars   `reader: page N drawn in` (no panel copy; builds after 7c23a25)
  menu, goto, aa                  frame that opened the overlay (`reader overlay X`)
  font size + (Aa)                frame after `reader action: aa-size:`
  relayout after Done             frame after the `close` that follows aa-size
  first turns after relayout      first 3 page turns after it
  library *                       `perf library` lines
"""
import re
import statistics
import sys
from collections import defaultdict
from pathlib import Path

OUT = Path(sys.argv[1])
vals = defaultdict(list)

FRAME = re.compile(r"(?:frame time|slow frame): (\d+) ms on screen (\w+)")


def reader_log(text):
    """Classifies each frame by the lines logged since the previous frame."""
    since = []
    after_size = False
    after_relayout = 0
    menus = 0
    for line in text.splitlines():
        m = FRAME.search(line)
        if not m:
            since.append(line)
            continue
        ms = int(m.group(1))
        joined = "\n".join(since)
        since = []
        if "reader overlay Menu" in joined and "gesture: Tap" in joined:
            menus += 1
            vals["menu open (first)" if menus == 1 else "menu open (again)"].append(ms)
        elif "reader overlay Goto" in joined:
            vals["goto open"].append(ms)
        elif "reader overlay Aa" in joined:
            vals["aa open"].append(ms)
        elif "reader action: aa-size:" in joined:
            vals["font size + (Aa sheet)"].append(ms)
            after_size = True
        elif after_size and "reader action: close" in joined:
            vals["relayout after Done"].append(ms)
            after_size = False
            after_relayout = 3
        elif "shown in 1 update" in joined and "open:" not in joined:
            if after_relayout:
                vals["page turn after relayout (first 3)"].append(ms)
                after_relayout -= 1
            else:
                vals["page turn"].append(ms)
                d = re.search(r"reader: page \d+ drawn in ([\d.]+) ms", joined)
                if d and "." in d.group(1):
                    vals["page turn: render page + bars (no panel)"].append(float(d.group(1)))


for f in sorted(OUT.glob("reader-*/log.txt")):
    reader_log(f.read_text(errors="replace"))
for f in sorted(OUT.glob("open-*/log.txt")):
    case = f.parent.name.rsplit("-", 1)[0]  # open-<book>-<nocache|cached>-<i>
    for m in re.finditer(r"open: '.*?' (?:reader|tap)->page ([\d.]+) ms", f.read_text(errors="replace")):
        vals[case].append(float(m.group(1)))
for f in sorted(OUT.glob("library-*/out.txt")):
    t = f.read_text()
    for key, pat in [
        ("library scan (500 books, empty index)", r"perf library scan: ([\d.]+) ms"),
        ("library metadata+covers (500 books)", r"perf library metadata: ([\d.]+) ms"),
        ("library reopen (500 books indexed)", r"perf library reopen: ([\d.]+) ms"),
    ]:
        m = re.search(pat, t)
        if m:
            vals[key].append(float(m.group(1)))
for f in sorted(OUT.glob("libscreen-*/log.txt")):
    m = re.search(r"time: app start: process start -> first frame (\d+) ms", f.read_text(errors="replace"))
    if m:
        vals["library screen first frame (500 new books)"].append(float(m.group(1)))

load = (OUT / "load.txt").read_text().strip() if (OUT / "load.txt").exists() else "?"
print(f"# {OUT}  load: {load}")
print(f"{'case':48} {'median':>8} {'min':>8} {'max':>8} {'n':>4}")
for k in sorted(vals):
    v = vals[k]
    print(f"{k:48} {statistics.median(v):8.1f} {min(v):8.1f} {max(v):8.1f} {len(v):4}")
