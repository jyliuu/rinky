#!/bin/bash
# Feeds malformed books (scripts/e2e/make-bad-books.py) and odd arguments to the host
# rinky binary and classifies each run:
#   ok      exit 0, or exit 1 with an "rinky: ..." error message
#   PANIC   a Rust panic (exit 101) - a bug in the crate named in the panic message
#   SIGNAL  killed by a signal (stack overflow = SIGSEGV/SIGBUS/SIGABRT)
#   TIMEOUT longer than ATTACK_TIMEOUT_S (default 60)
#   OTHER   any other exit status
#   scripts/e2e/attack-inputs.sh OUTDIR [BIN]     exit status = number of non-ok runs
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${1:?usage: scripts/e2e/attack-inputs.sh OUTDIR [BIN]}
BIN=${2:-$ROOT/target/debug/rinky}
LIMIT=${ATTACK_TIMEOUT_S:-60}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd -P)
BOOKS="$OUT/books"
rm -rf "$BOOKS" "$OUT/runs"
mkdir -p "$OUT/runs"
python3 -I "$ROOT/scripts/e2e/make-bad-books.py" "$BOOKS" --good "$ROOT/testdata/juliet.epub" >"$OUT/books.txt" || exit 2
RESULTS="$OUT/results.txt"
: >"$RESULTS"
bad=0

# run LABEL CMD...: one classified run, output in runs/LABEL.txt
run() {
  local label=$1 log rc t0 t1 class
  shift
  log="$OUT/runs/$label.txt"
  t0=$(perl -MTime::HiRes=time -e 'printf "%.2f", time')
  { echo "\$ $*"; } >"$log"
  RUST_BACKTRACE=1 perl -e 'alarm shift; exec @ARGV or exit 126' "$LIMIT" "$@" >>"$log" 2>&1 </dev/null
  rc=$?
  t1=$(perl -MTime::HiRes=time -e 'printf "%.2f", time')
  echo "exit=$rc" >>"$log"
  if grep -q "panicked at" "$log"; then class=PANIC
  elif [ "$rc" = 142 ]; then class=TIMEOUT
  elif [ "$rc" -gt 128 ]; then class="SIGNAL$((rc - 128))"
  elif [ "$rc" = 0 ]; then class=ok
  elif [ "$rc" = 1 ] && grep -q "^rinky: " "$log"; then class=ok
  elif [ "$rc" = 2 ] && grep -q "^usage:" "$log"; then class=ok
  elif grep -qE "^selftest: [0-9]+ passed, $rc failed" "$log"; then class=ok   # exit = failed checks
  else class=OTHER; fi
  [ "$class" = ok ] || bad=$((bad + 1))
  local why
  why=$(grep -m1 -E "^rinky: |panicked at|thread .* overflowed|^Error|^FAIL" "$log" | cut -c1-160)
  printf '%-7s %-58s exit %-3s %6.2fs  %s\n' "$class" "$label" "$rc" "$(echo "$t1 - $t0" | bc)" "$why" | tee -a "$RESULTS"
}

for b in "$BOOKS"/* "$BOOKS/missing.epub"; do
  n=$(basename "$b")
  run "render-$n" "$BIN" render "$b" "$OUT/runs/render-$n.out" 3
  run "sim-$n" "$BIN" sim "$b" --headless --script "$ROOT/tests/sim/turn-pages.txt" --out "$OUT/runs/sim-$n.out"
  run "selftest-$n" "$BIN" selftest --book "$b"
done

# Odd arguments.
run "render-0-pages" "$BIN" render "$ROOT/testdata/juliet.epub" "$OUT/runs/zero" 0
run "render-huge-count" "$BIN" render "$ROOT/testdata/synthetic.epub" "$OUT/runs/huge" 18446744073709551615
run "render-negative" "$BIN" render "$ROOT/testdata/juliet.epub" "$OUT/runs/neg" -1
run "render-out-is-file" "$BIN" render "$ROOT/testdata/juliet.epub" "$ROOT/testdata/juliet.epub" 1
printf 'tap -5 -5\ntap 99999 99999\nswipe 0 0 5000 5000\nhold 1271 1695 9\nkey next\n' >"$OUT/edge-script.txt"
run "sim-edge-coordinates" "$BIN" sim "$ROOT/testdata/juliet.epub" --headless --script "$OUT/edge-script.txt" --out "$OUT/runs/edge"
printf 'tap 10\nbogus step\n' >"$OUT/bad-script.txt"
run "sim-bad-script" "$BIN" sim "$ROOT/testdata/juliet.epub" --headless --script "$OUT/bad-script.txt" --out "$OUT/runs/badscript"
run "sim-missing-script" "$BIN" sim "$ROOT/testdata/juliet.epub" --headless --script "$OUT/nothere.txt"
run "sim-scale-nan" "$BIN" sim "$ROOT/testdata/juliet.epub" --headless --scale NaN --script "$OUT/edge-script.txt"
run "demo-no-script" "$BIN" demo --headless --out "$OUT/runs/demo"
run "selftest-url-bad" "$BIN" selftest --url "notaurl"
run "spike-negative" "$BIN" spike-suspend --seconds -5
run "unknown-cmd" "$BIN" frobnicate
run "no-args" "$BIN"
echo "attack-inputs: $bad non-ok of $(wc -l <"$RESULTS" | tr -d ' ') runs" | tee -a "$RESULTS"
exit "$bad"
