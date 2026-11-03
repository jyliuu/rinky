#!/bin/bash
# Verifier attack on scripts/golden-update.sh (p0fix verify cycle 1): the DIFF_DIR
# argument is deleted with `rm -rf "$OUT"` before anything else. An agent that follows
# "regenerate with scripts/golden-update.sh and save the diff images as evidence" and
# passes its evidence directory (out/evidence/<key>) loses everything in it.
# The test passes a scratch directory holding an unrelated file; `cargo` is a stub on
# PATH, so no test runs and no golden is rewritten.
#   G1 an existing directory with other files: kept (or the script refuses it)
#   G2 a new directory: created, script exit 0
#   G3 an existing empty directory: used, script exit 0
#   G4 an existing file as DIFF_DIR: refused, file kept, cargo not run
#   scripts/e2e/verify-golden-update-outdir.sh     PASS/FAIL lines; exit status = failures
# G1 failed at fb10e9a (the file was deleted; verifier finding). golden-update.sh now
# refuses a DIFF_DIR that holds other files; all checks run always.
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
TMP=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/verify-golden.XXXXXX")" && pwd -P)
fails=0
ok() { echo "PASS $*"; }
bad() { echo "FAIL $*"; fails=$((fails + 1)); }
mkdir -p "$TMP/bin"
printf '#!/bin/sh\necho "cargo stub: $*" >>"%s/cargo-calls.txt"\nexit 0\n' "$TMP" >"$TMP/bin/cargo"
chmod +x "$TMP/bin/cargo"

# G1
mkdir -p "$TMP/evidence/earlier-check"
echo "screenshot of an earlier acceptance check" >"$TMP/evidence/earlier-check/keep.txt"
PATH="$TMP/bin:$PATH" "$ROOT/scripts/golden-update.sh" "$TMP/evidence" >"$TMP/g1.txt" 2>&1; rc=$?
if [ -f "$TMP/evidence/earlier-check/keep.txt" ]; then
  ok "G1 existing DIFF_DIR with other files kept (exit $rc)"
elif [ $rc != 0 ] && ! grep -q "cargo stub" "$TMP/cargo-calls.txt" 2>/dev/null; then
  ok "G1 refused (exit $rc) without running cargo"
else
  bad "G1 golden-update.sh deleted an existing directory passed as DIFF_DIR (exit $rc; earlier-check/keep.txt is gone)"
fi
sed 's/^/     /' "$TMP/g1.txt"

# G2
PATH="$TMP/bin:$PATH" "$ROOT/scripts/golden-update.sh" "$TMP/new-diff-dir" >"$TMP/g2.txt" 2>&1; rc=$?
[ $rc = 0 ] && [ -d "$TMP/new-diff-dir" ] && [ "$(grep -c 'cargo stub: test' "$TMP/cargo-calls.txt")" -ge 2 ] \
  && ok "G2 new DIFF_DIR created, both golden tests called (exit 0)" || bad "G2 exit $rc: $(tail -2 "$TMP/g2.txt")"

# G3
mkdir -p "$TMP/empty-dir"; : >"$TMP/cargo-calls.txt"
PATH="$TMP/bin:$PATH" "$ROOT/scripts/golden-update.sh" "$TMP/empty-dir" >"$TMP/g3.txt" 2>&1; rc=$?
[ $rc = 0 ] && [ "$(grep -c 'cargo stub: test' "$TMP/cargo-calls.txt")" -ge 2 ] \
  && ok "G3 existing empty DIFF_DIR used (exit 0)" || bad "G3 exit $rc: $(tail -2 "$TMP/g3.txt")"

# G4
echo "not a directory" >"$TMP/a-file"; : >"$TMP/cargo-calls.txt"
PATH="$TMP/bin:$PATH" "$ROOT/scripts/golden-update.sh" "$TMP/a-file" >"$TMP/g4.txt" 2>&1; rc=$?
[ $rc != 0 ] && [ "$(cat "$TMP/a-file")" = "not a directory" ] && [ ! -s "$TMP/cargo-calls.txt" ] \
  && ok "G4 a file as DIFF_DIR refused (exit $rc), file kept, cargo not run" || bad "G4 exit $rc: $(tail -2 "$TMP/g4.txt")"

rm -rf "$TMP"
echo "verify-golden-update-outdir: $fails failed"
exit "$fails"
