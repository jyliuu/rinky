#!/bin/bash
# e2e-cycles: 1
# Wave B merge check: library -> book -> reader menu -> Aa font size + -> back to the
# library -> Exit to the Kindle Home. Coordinates: 1272 x 1696 portrait (library layout
# from library-smoke.sh, reader buttons from reader-menu-smoke.sh).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

B=/mnt/us/Books/inkmerge-e2e
in_log() { guest "grep -q -- '$1' $DEST/rinky.log"; }
expect_log() { wait_until 30 "log: $1" in_log "$1" || fail "no log line '$1'"; }
wait_home 300 home-before
guest "rm -rf $B $DEST/library.json $DEST/library-view.json; mkdir -p $B" || fail "prepare $B"
"$SLOT" push "$NAME" "$E2E_ROOT/testdata/juliet.epub" "$B/" >/dev/null || fail "push book"
guest "rm -rf $B/juliet.sdr; rm -f $DEST/rinky.log $DEST/watchdog.log; sh $DEST/rinky.sh --library" || fail "launch"
wait_until 120 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over"
wait_until 120 "library scan logged" in_log "library scan:" || fail "no 'library scan:' line"
nap 8
not_home library-home
# Library tab, sort by title: JAILBROKEN (from /mnt/us/documents), Romeo and Juliet.
tap 477 1631; nap 5
tap 177 210; nap 3
tap 300 $((256 + 8 + 110 + 55)); nap 4
not_home library-sorted
guest "test -e /mnt/us/documents/JAILBROKEN.txt" || fail "expected /mnt/us/documents/JAILBROKEN.txt (row order)"
tap 600 $((270 + 150 + 75))
expect_log "saved position"
nap 4
not_home reader-page
tap 636 848;  expect_log "reader overlay Menu"; nap 3; not_home reader-menu
tap 764 68;   expect_log "reader overlay Aa"; nap 3; not_home reader-aa
tap 1196 890; expect_log "reader action: aa-size"; nap 5; not_home reader-aa-bigger
tap 1158 314; nap 3   # Done
not_home reader-bigger
# Menu, then its Back button (top left): the reader pops back to the library.
tap 636 848;  expect_log "reader overlay Menu"; nap 3
tap 60 68; nap 6
reader_running || fail "rinky ended when the reader closed (should be back in the library)"
not_home library-after-reader
# Exit to Kindle (rinky-ctl.sh stop): rinky ends, the launcher starts the framework.
exit_device
wait_clean_home 300 home-after
save_logs
grep -q "start lab126_gui" "$OUT/rinky.log" || fail "launcher did not start the framework"
grep -q "\] exit: reason=" "$OUT/rinky.log" || fail "launcher logged no exit line"
grep -E "library|reader (overlay|action)|screen:" "$OUT/rinky.log" | head -40 >"$OUT/merge-lines.txt"
guest "rm -rf $B" || true
pass "wave B merge: library, book, reader menu, Aa font size, back, exit to Kindle"
