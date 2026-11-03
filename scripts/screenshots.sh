#!/bin/bash
# Renders the README screenshots into docs/screenshots/ with the headless simulator.
#
#   scripts/screenshots.sh [--no-build] [OUTDIR]
#
# The library is scripts/make-showcase-library.py's public-domain classics (generated
# covers), statistics come from the ink-stats seed example, the dictionary popup uses
# that script's small sample dictionary. Input steps: tests/sim/screenshots.txt.
# Shots are 1272 x 1696 (the PW12 panel), stored as 8-bit grayscale PNGs.
# Needs python3 with Pillow.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BUILD=1
[ "${1:-}" = --no-build ] && { BUILD=0; shift; }
OUT=${1:-$ROOT/docs/screenshots}

cd "$ROOT"
if [ "$BUILD" = 1 ]; then
  CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)} \
    cargo build --release -p ink-app -p ink-stats --bins --examples >&2
fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/rinky-shots.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
HOME_DIR=$WORK/tmp/rinky-sim   # `sim` keeps its state in $TMPDIR/rinky-sim
mkdir -p "$HOME_DIR" "$WORK/shots"
python3 -I scripts/make-showcase-library.py "$WORK/books" "$HOME_DIR/dict/showcase" >/dev/null
target/release/examples/seed "$HOME_DIR" >/dev/null

TMPDIR=$WORK/tmp/ target/release/rinky sim "$WORK/books" --headless \
  --script tests/sim/screenshots.txt --out "$WORK/shots" >"$WORK/sim.log" 2>&1 || {
  tail -20 "$WORK/sim.log" >&2
  exit 1
}

mkdir -p "$OUT"
python3 -I - "$WORK/shots" "$OUT" <<'EOF'
import sys
from pathlib import Path
from PIL import Image
src, out = Path(sys.argv[1]), Path(sys.argv[2])
for p in sorted(src.glob("*.png")):
    Image.open(p).convert("L").save(out / p.name, optimize=True)
    print(out / p.name)
EOF
