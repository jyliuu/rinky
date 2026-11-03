#!/usr/bin/env python3
"""Compare two guest-list.sh listings, ignoring paths that changed on their own.

  compare-lists.py BEFORE AFTER [--churn A B]... [--report FILE]

A path is "churn" when its line differs between the listings A and B of one --churn
pair (two listings taken a minute apart before anything was installed: the Kindle's own
logs and databases). Prints added, removed and changed paths that are not churn; exit
status 0 when there are none, 1 otherwise.
"""

from __future__ import annotations

import argparse
import sys


def load(path: str) -> dict[str, str]:
    entries: dict[str, str] = {}
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line:
                continue
            parts = line.split(" ", 2)
            if len(parts) < 2:
                continue
            kind, rest = parts[0], line[2:]
            # F lines: "F path size mode md5" for /mnt/us, "F path" for /var/local.
            if kind == "F" and rest.startswith("/mnt/us"):
                path = rest.rsplit(" ", 3)[0]
            else:
                path = rest
            entries[path] = line
    return entries


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("before")
    ap.add_argument("after")
    ap.add_argument("--churn", nargs=2, action="append", default=[])
    ap.add_argument("--report")
    a = ap.parse_args()
    before, after = load(a.before), load(a.after)
    churn: set[str] = set()
    for x, y in a.churn:
        lx, ly = load(x), load(y)
        churn |= {p for p in set(lx) | set(ly) if lx.get(p) != ly.get(p)}
    added = sorted(p for p in set(after) - set(before) if p not in churn)
    removed = sorted(p for p in set(before) - set(after) if p not in churn)
    changed = sorted(p for p in set(before) & set(after) if before[p] != after[p] and p not in churn)
    lines = [f"before {a.before}: {len(before)} paths", f"after  {a.after}: {len(after)} paths",
             f"churn paths ignored: {len(churn)}"]
    lines += [f"  churn {p}" for p in sorted(churn)]
    lines += [f"added   {after[p]}" for p in added]
    lines += [f"removed {before[p]}" for p in removed]
    lines += [f"changed {before[p]}  ->  {after[p]}" for p in changed]
    lines.append(f"result: {len(added)} added, {len(removed)} removed, {len(changed)} changed (churn excluded)")
    text = "\n".join(lines) + "\n"
    sys.stdout.write(text)
    if a.report:
        with open(a.report, "w") as f:
            f.write(text)
    return 0 if not (added or removed or changed) else 1


if __name__ == "__main__":
    sys.exit(main())
