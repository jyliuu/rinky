#!/bin/bash
# Render from a clean clone: git clone BRANCH of this repository into CACHE/clean-clone
# (only committed files), build rinky there, render 6 pages of juliet.epub, compare
# them with the prototype pages, and run both golden test sets in the clone.
#   scripts/e2e/clean-clone-check.sh OUTDIR [BRANCH] [--cold]
# The clone builds into CACHE/clean-clone-target. Without --cold that directory starts
# as an APFS clone (cp -c) of this worktree's target/, so third-party crates are reused;
# every workspace crate is still compiled from the clone's own sources (other paths).
# --cold starts from an empty target directory (slow: every dependency is compiled).
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${1:?usage: scripts/e2e/clean-clone-check.sh OUTDIR [BRANCH] [--cold]}
BRANCH=${2:-$(git -C "$ROOT" rev-parse --abbrev-ref HEAD)}
COLD=0
[ "${3:-}" = --cold ] && COLD=1
CACHE=${CLEAN_CLONE_CACHE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/inkreader-wt/.cache}
PROTO=${PROTO_PAGES:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/rinky/out/evidence/arch/proto-pages}
CLONE="$CACHE/clean-clone"
TGT="$CACHE/clean-clone-target"
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd -P)
LOG="$OUT/clean-clone.txt"
: >"$LOG"
bad=0
note() { echo "$*" | tee -a "$LOG"; }
run() {   # LABEL CMD...: records the real exit status
  local label=$1 rc
  shift
  note "\$ $*"
  "$@" >>"$LOG" 2>&1
  rc=$?
  note "$label exit=$rc"
  [ "$rc" = 0 ] || bad=$((bad + 1))
  return $rc
}

rm -rf "$CLONE"
run clone git clone --quiet --branch "$BRANCH" --single-branch "$ROOT" "$CLONE" || exit 1
note "clone: $(git -C "$CLONE" log --oneline -1)"
note "untracked or modified in the clone: $(git -C "$CLONE" status --porcelain | wc -l | tr -d ' ')"
note "files in the clone: $(git -C "$CLONE" ls-files | wc -l | tr -d ' ')"
if [ $COLD = 1 ]; then
  rm -rf "$TGT"
  note "target: empty (cold build)"
elif [ ! -d "$TGT" ]; then
  cp -cR "$ROOT/target" "$TGT" 2>/dev/null || cp -R "$ROOT/target" "$TGT"
  note "target: APFS clone of $ROOT/target (dependencies warm; workspace crates rebuilt from the clone)"
else
  note "target: existing $TGT reused"
fi
cd "$CLONE" || exit 1
export CARGO_TARGET_DIR="$TGT"
t0=$(date +%s)
run build cargo build -p ink-app
note "build took $(( $(date +%s) - t0 )) s; compiled from: $(grep -c 'Compiling' "$LOG") 'Compiling' lines (workspace crates: $(grep -c 'Compiling ink-' "$LOG"))"
rm -rf "$OUT/pages"
run render "$TGT/debug/rinky" render testdata/juliet.epub "$OUT/pages" 6
run compare python3 -I scripts/png-compare.py "$OUT/pages" "$PROTO"
run golden-pages cargo test -p ink-html --test golden
run golden-footer cargo test -p ink-app --test footer_golden
note "clean-clone-check: $bad step(s) failed"
exit "$bad"
