#!/bin/bash
# e2e-cycles: 0
# Verifier (p0fix cycle 1): the ARM musl binary on the guest gets the malformed books of
# scripts/e2e/make-bad-books.py and odd command lines. The p0fix branch ran these only
# with the macOS host binary; the guest has another libc, other stack limits and 980 MB
# RAM. No framebuffer or framework use: `render` writes PNGs to /mnt/us/p0fix-verify
# (discarded with -snapshot), so the framework keeps running (0 cycles).
# Each run is classified as in scripts/e2e/attack-inputs.sh:
#   ok      exit 0, exit 1 with "rinky: ...", exit 2 with usage, selftest exit = failures
#   PANIC   "panicked at" in the output        SIGNAL  exit > 128 (not the time limit)
#   TIMEOUT busybox timeout (exit 143) after 120 s
# Verifier findings, fixed and run always:
#   K1 `rinky evdump 18446744073709551615` panicked (Instant + Duration overflow in
#      cli.rs evdump); now "evdump SECONDS ...: too large", exit 1;
#   K2 a book path that is not valid UTF-8 panicked in main.rs (std::env::args());
#      now "argument N is not valid UTF-8", exit 1.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
set -o pipefail

W=/mnt/us/p0fix-verify
BOOKS="$OUT/books"
python3 -I "$E2E_ROOT/scripts/e2e/make-bad-books.py" "$BOOKS" --good "$E2E_ROOT/testdata/juliet.epub" >"$OUT/books.txt" \
  || fail "make-bad-books.py"
rm -rf "$BOOKS"/*.sdr
guest "rm -rf $W; mkdir -p $W/books $W/out" || fail "guest mkdir"
files=$(find "$BOOKS" -maxdepth 1 -type f | sort)
# shellcheck disable=SC2086
"$SLOT" push "$NAME" $files "$W/books/" >/dev/null || fail "push books"
guest "mkdir -p $W/books/dir.epub"
RESULTS="$OUT/results.txt"
: >"$RESULTS"
bad=0

# run LABEL GUEST-COMMAND: one classified run on the guest, output in runs/LABEL.txt
mkdir -p "$OUT/runs"
run() {
  local label=$1 cmd=$2 log="$OUT/runs/$1.txt" rc class why
  # Output to a guest file first: over ssh, stdout and stderr can interleave mid-line.
  guest "cd $W && timeout -s TERM 120 $cmd >$W/run.log 2>&1; rc=\$?; cat $W/run.log; echo \"exit=\$rc\"" >"$log" 2>&1
  rc=$(sed -n 's/^exit=\([0-9]*\)$/\1/p' "$log" | tail -1)
  if grep -q "panicked at" "$log"; then class=PANIC
  elif [ "$rc" = 143 ]; then class=TIMEOUT
  elif [ -z "$rc" ]; then class=NO-SSH
  elif [ "$rc" -gt 128 ]; then class="SIGNAL$((rc - 128))"
  elif [ "$rc" = 0 ]; then class=ok
  elif [ "$rc" = 1 ] && grep -q "^rinky: " "$log"; then class=ok
  elif [ "$rc" = 2 ] && grep -q "^usage:" "$log"; then class=ok
  elif grep -qE "^selftest: [0-9]+ passed, $rc failed" "$log"; then class=ok
  else class=OTHER; fi
  [ "$class" = ok ] || bad=$((bad + 1))
  why=$(grep -m1 -E "^rinky: |panicked at|^FAIL|Segmentation|Killed" "$log" | cut -c1-150)
  printf '%-7s %-44s exit %-4s %s\n' "$class" "$label" "${rc:-?}" "$why" | tee -a "$RESULTS"
  guest "rm -rf $W/out/*" >/dev/null 2>&1
}

BIN=$DEST/rinky
for f in $files; do
  b=$(basename "$f")
  run "render-$b" "$BIN render books/$b out/$b 3"
done
run "render-dir.epub" "$BIN render books/dir.epub out/d 1"
run "render-missing.epub" "$BIN render books/nothere.epub out/m 1"
run "selftest-deep-nesting" "$BIN selftest --book books/deep-nesting.epub"
run "selftest-many-chapters" "$BIN selftest --book books/many-chapters.epub"
run "render-huge-count" "$BIN render $DEST/synthetic.epub out/h 18446744073709551615"
run "render-negative-count" "$BIN render $DEST/synthetic.epub out/n -1"
run "render-out-is-file" "$BIN render $DEST/synthetic.epub $DEST/juliet.epub 1"
run "selftest-book-no-value" "$BIN selftest --book"
run "spike-seconds-no-value" "$BIN spike-suspend --seconds"
run "spike-seconds-bad" "$BIN spike-suspend --seconds x"
run "screensaver-missing" "$BIN screensaver books/nothere.png"
run "screensaver-empty-file" "$BIN screensaver books/empty.txt"
run "evdump-bad-seconds" "$BIN evdump x"
run "evdump-1s" "$BIN evdump 1"
run "unknown-command" "$BIN frobnicate"

run "K1-evdump-u64-max" "$BIN evdump 18446744073709551615"
run "K2-non-utf8-book-path" "$BIN render \"\$(printf 'books/caf\\351.epub')\" out/k2 1"
grep -q "too large" "$OUT/runs/K1-evdump-u64-max.txt" || fail "K1: no 'too large' error (runs/K1-evdump-u64-max.txt)"
grep -q "not valid UTF-8" "$OUT/runs/K2-non-utf8-book-path.txt" || fail "K2: no 'not valid UTF-8' error (runs/K2-non-utf8-book-path.txt)"
guest "rm -rf $W"
echo "$bad non-ok of $(wc -l <"$RESULTS" | tr -d ' ') runs" | tee -a "$RESULTS"
[ "$bad" = 0 ] || fail "$bad non-ok runs on the guest (results.txt)"
pass "malformed books and odd arguments on the ARM binary: no panic, signal or timeout"
