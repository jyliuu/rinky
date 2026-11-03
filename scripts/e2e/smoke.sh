#!/bin/bash
# Quick emulator smoke set: 10 scenarios of tests/e2e, one per main path, on one slot.
# The full release set is every tests/e2e/*.sh (scripts/e2e/run.sh NAME); see
# docs/TESTING.md for both lists.
#   scripts/e2e/smoke.sh NAME [OUTDIR]        NAME is up and deployed:
#                                             scripts/emu-slot.sh up NAME
#                                             scripts/emu-slot.sh deploy NAME
#   scripts/e2e/smoke.sh --up NAME [OUTDIR]   up + deploy NAME first, down at the end
# Each scenario runs through scripts/e2e/run.sh (restarts, retries, time scale and the
# framework-cycle budget as there). OUTDIR defaults to out/e2e/smoke-<timestamp>.
# Exit status = number of scenarios that did not pass (SKIP, exit 77, counts as passed).
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
SLOT="$ROOT/scripts/emu-slot.sh"
UP=0
if [ "${1:-}" = --up ]; then UP=1; shift; fi
NAME=${1:?usage: scripts/e2e/smoke.sh [--up] NAME [OUTDIR]}
OUTDIR=${2:-$ROOT/out/e2e/smoke-$(date +%Y%m%d-%H%M%S)}

# Order: launcher first (if it fails, the rest is noise), then library, reader, formats,
# PDF/CBZ, settings, sleep, plugin.
SMOKE=(
  arch-launcher-normal
  library-smoke
  wave-b-merge-smoke
  reader-menu-smoke
  arch-reader-gestures
  formats-books
  mupdf-pages
  settings-smoke
  device-reader-sleep
  dict-smoke
)

for s in "${SMOKE[@]}"; do
  [ -f "$ROOT/tests/e2e/$s.sh" ] || { echo "smoke: tests/e2e/$s.sh is missing" >&2; exit 100; }
done

if [ "$UP" = 1 ]; then
  "$SLOT" up "$NAME" || { echo "smoke: emu-slot.sh up $NAME failed" >&2; exit 100; }
  "$SLOT" deploy "$NAME" || { echo "smoke: emu-slot.sh deploy $NAME failed" >&2; "$SLOT" down "$NAME"; exit 100; }
fi

mkdir -p "$OUTDIR"
fail=0
results=()
start=$(date +%s)
for s in "${SMOKE[@]}"; do
  "$ROOT/scripts/e2e/run.sh" "$NAME" "$OUTDIR" "$s"
  rc=$?
  if [ "$rc" = 0 ]; then results+=("PASS $s"); else results+=("FAIL $s (run.sh exit $rc)"); fail=$((fail + 1)); fi
  # run.sh exits 100 when the instance holds no slot or does not answer: stop.
  if [ "$rc" = 100 ]; then echo "smoke: $NAME is not usable; stopping" >&2; break; fi
done

[ "$UP" = 1 ] && "$SLOT" down "$NAME"

echo
echo "smoke: $NAME, $(( $(date +%s) - start )) s, output in $OUTDIR"
printf '  %s\n' "${results[@]}"
echo "smoke: ${#results[@]} run, $fail not passed (of ${#SMOKE[@]})"
exit "$fail"
