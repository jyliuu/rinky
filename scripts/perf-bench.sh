#!/bin/bash
# Host benchmark of the hot paths: book open (no cache / cached), page
# turns, menu, Go to, Aa sheet, relayout after a font-size change, library scan of 500
# books. Runs a release `rinky` headless (no window) and reads its log.
#
#   scripts/perf-bench.sh BIN OUTDIR [RUNS]
#
# BIN: a copy of target/release/rinky (copy it first: other builds replace the
# original). OUTDIR gets the test books (generated once), one folder per run and
# summary.txt. RUNS (default 5) repeats every case; the summary gives median/min/max.
# Each case starts with an empty sim home (TMPDIR: page-map cache) and no sidecar
# (<book>.sdr next to the book); "cached" reopens right after the "no cache" run.
set -euo pipefail
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
mkdir -p "$2"
OUT=$(cd "$2" && pwd)
RUNS=${3:-5}
HERE=$(cd "$(dirname "$0")" && pwd)

mkdir -p "$OUT/books"
[ -f "$OUT/books/big36.epub" ] || python3 -I "$HERE/make-big-epub.py" "$OUT/books/big36.epub" 36 36000 10 >/dev/null
[ -f "$OUT/books/dune63.epub" ] || python3 -I "$HERE/make-big-epub.py" "$OUT/books/dune63.epub" 63 5200 3 >/dev/null
[ -d "$OUT/lib500" ] || python3 -I "$HERE/make-perf-library.py" "$OUT/lib500" 500 >/dev/null
printf 'key power\n' >"$OUT/open-only.txt"
uptime | sed 's/.*load averages*: //' >"$OUT/load.txt"

# sim RUNDIR TMPHOME SCRIPT BOOK
sim() {
    mkdir -p "$1" "$2"
    (cd "$1" && TMPDIR="$2" RINKY_LOG=debug "$BIN" sim --headless --script "$3" --out "$1" "$4" 2>"$1/log.txt")
}

for i in $(seq 1 "$RUNS"); do
    for b in big36 dune63; do
        rm -rf "$OUT/home-$b" "$OUT/books/$b.sdr"
        sim "$OUT/open-$b-nocache-$i" "$OUT/home-$b" "$OUT/open-only.txt" "$OUT/books/$b.epub"
        sim "$OUT/open-$b-cached-$i" "$OUT/home-$b" "$OUT/open-only.txt" "$OUT/books/$b.epub"
    done
    rm -rf "$OUT/home-reader" "$OUT/books/big36.sdr"
    sim "$OUT/reader-$i" "$OUT/home-reader" "$HERE/perf/reader.txt" "$OUT/books/big36.epub"
    rm -rf "$OUT/library-$i"
    mkdir -p "$OUT/library-$i"
    "$BIN" perf library "$OUT/lib500" "$OUT/library-$i/home" >"$OUT/library-$i/out.txt" 2>"$OUT/library-$i/log.txt"
    rm -rf "$OUT/home-lib" "$OUT/libscreen-$i"
    sim "$OUT/libscreen-$i" "$OUT/home-lib" "$HERE/perf/library.txt" "$OUT/lib500"
done
uptime | sed 's/.*load averages*: //' >>"$OUT/load.txt"
python3 -I "$HERE/perf-summary.py" "$OUT" | tee "$OUT/summary.txt"
