#!/bin/bash
# e2e-cycles: 1
# Dictionary plugin on the emulator: the sample StarDict dictionary (written on the host
# by `cargo run -p ink-dict --example lookup -- --write-sample`) goes to
# /mnt/us/rinky/dict/sample/; juliet page 6, long press on "households", Look up,
# popup, Full view, back. Checks the log lines and the row in vocabulary.sqlite3.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

logged() { guest "grep -q '$1' $DEST/rinky.log" 2>/dev/null; }
expect_log() { wait_until 30 "log: $1" logged "$1" || fail "no log line '$1'"; }

DICT="$OUT/dict-sample"
rm -rf "$DICT"
(cd "$E2E_ROOT" && cargo run -q -p ink-dict --example lookup -- --write-sample "$DICT") >"$OUT/write-sample.txt" 2>&1 || fail "write sample dictionary"
guest "rm -rf $DEST/dict/sample $DEST/vocabulary.sqlite3 $DEST/vocabulary.sqlite3-journal; mkdir -p $DEST/dict/sample" || fail "mkdir dict"
"$SLOT" push "$NAME" "$DICT"/sample-en.ifo "$DICT"/sample-en.idx "$DICT"/sample-en.dict.dz "$DICT"/sample-en.syn "$DEST/dict/sample/" >/dev/null || fail "push dictionary"
guest "ls -l $DEST/dict/sample" >"$OUT/guest-dict.txt" 2>&1

wait_home 240 home-before
start_reader
expect_log "saved position"
nap 3
for _ in 1 2 3 4 5; do tap 1000 900; nap 2; done
expect_log "reader position page 6 "
not_home prologue
"$SLOT" hold "$NAME" 292 312 1.5 >/dev/null || fail "hold"
expect_log "reader selection page 6"
nap 2
not_home selection
tap 570 505
expect_log "dictionary: lookup"
nap 3
not_home popup
tap 922 1596
expect_log "dictionary: answer 1 = \"full\""
nap 4
not_home full-view
tap 1210 60
nap 3
not_home closed
save_logs
grep -E "dictionary|reader selection" "$OUT/rinky.log" | tee "$OUT/dict-lines.txt"
grep -q 'dictionary: lookup "households" -> form "households", 1 hits' "$OUT/rinky.log" || fail "lookup line"
# The row is written on a worker thread; give it a moment, then copy the database out.
nap 3
"$SLOT" pull "$NAME" "$DEST/vocabulary.sqlite3" "$OUT/vocabulary.sqlite3" >/dev/null 2>&1 || fail "no vocabulary.sqlite3"
if command -v sqlite3 >/dev/null; then
  sqlite3 "$OUT/vocabulary.sqlite3" "select id, word, form, book_md5, book_title, time, found from lookup" | tee "$OUT/vocabulary-rows.txt"
  grep -q '|households|households|.*|Romeo and Juliet|' "$OUT/vocabulary-rows.txt" || fail "vocabulary row"
fi
pass "dictionary lookup, popup, full view and vocabulary row on the emulator"
exit_device
