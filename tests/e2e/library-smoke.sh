#!/bin/bash
# e2e-cycles: 1
# Library: launch `rinky.sh --library` on a folder of test books in /mnt/us/Books.
# Checks: the scan, metadata and cover cache (library.json, cache/covers/*), home and
# library screenshots, a book's details page (hold), a DRM book opens the reader's error
# screen and a tap returns to the library, `rinky-ctl.sh stop` (exit_device) ends
# rinky (launcher exit line, framework started again) and the stock Home comes back. Coordinates are the library layout at 1272 x 1696 portrait.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

B=/mnt/us/Books/inklib-e2e
in_log() { guest "grep -q -- '$1' $DEST/rinky.log"; }
wait_home 300 home-before
guest "rm -rf $B $DEST/library.json $DEST/library-view.json $DEST/cache/covers; mkdir -p $B" || fail "prepare $B"
"$SLOT" push "$NAME" "$E2E_ROOT/testdata/juliet.epub" "$E2E_ROOT/testdata/synthetic.epub" \
  "$E2E_ROOT/testdata/formats/drm-adept.epub" "$E2E_ROOT/testdata/formats/sample.fb2" "$B/" >/dev/null || fail "push books"
guest "rm -f $DEST/rinky.log $DEST/watchdog.log; sh $DEST/rinky.sh --library" || fail "launch"
wait_until 120 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over"
wait_until 120 "library scan logged" in_log "library scan:" || fail "no 'library scan:' line"
nap 8
not_home library-home
guest "ls -la $DEST/library.json; find $DEST/cache/covers -name '*.png' | wc -l" >"$OUT/files.txt" 2>&1
log "files: $(tr '\n' ' ' <"$OUT/files.txt")"
covers=$(tail -1 "$OUT/files.txt" | tr -d ' ')
[ "${covers:-0}" -ge 1 ] || fail "no cover thumbnails in $DEST/cache/covers"
guest "grep -c 'Romeo and Juliet' $DEST/library.json" >/dev/null || fail "metadata title not in library.json"
# Library tab (list).
tap 477 1631
nap 5
not_home library-list
# Hold the first row: details page.
"$SLOT" hold "$NAME" 500 340 2 >/dev/null || fail "hold"
nap 5
not_home library-details
# Back to the list.
tap 75 75
nap 4
# Sort menu -> Title (fixed order for the next tap).
tap 177 210
nap 3
tap 300 $((256 + 8 + 110 + 55))
nap 4
not_home library-sorted-title
# Title order (with the jailbreak's /mnt/us/documents/JAILBROKEN.txt): FB2 Sample Book,
# JAILBROKEN, Protected Sample, Romeo and Juliet, Synthetic Test Book. Row 3 = DRM book.
guest "test -e /mnt/us/documents/JAILBROKEN.txt" || fail "expected /mnt/us/documents/JAILBROKEN.txt (row order)"
tap 600 $((270 + 2 * 150 + 75))
wait_until 60 "drm refusal logged" in_log "cannot open" || fail "DRM book: no 'cannot open' line"
nap 4
not_home library-drm-error
tap 600 900
nap 6
reader_running || fail "rinky ended after the error tap (should be back in the library)"
not_home library-after-error
# Exit to Kindle as the shell form does it (there is no top-right Exit control).
exit_device
wait_clean_home 300 home-after
save_logs
grep -q "start lab126_gui" "$OUT/rinky.log" || fail "launcher did not start the framework"
grep -q "\] exit: reason=" "$OUT/rinky.log" || fail "launcher logged no exit line"
grep "library" "$OUT/rinky.log" | head -20 >"$OUT/library-lines.txt"
pass "library smoke: scan, covers, list, details, DRM error and back, exit"
