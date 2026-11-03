#!/bin/bash
# e2e-cycles: 1
# Wave B verifier, bug: the reader saves `percent` = (page + 1) / page_count().pages,
# and while the background page map is still being built page_count() is only the
# pages known so far (screens/reader/mod.rs save_position). Reopening a large book
# at a far position and leaving within a few seconds saves a percent near 1.0
# (the library then shows ~99%) although the position is early in the book.
# Steps: big EPUB (juliet x 40, device-helpers/make-big-epub.py), open, menu, slider
# end (a far page), menu Back; reopen at once and leave through menu Back within a
# few seconds; read the sidecar. FAIL when percent > 0.9 although the position's
# spine item (DocFragment[N]) is in the first half of the 1070 spine items.
# NOTE: on the emulator (2026-10-31) this PASSED: the first visit stayed until the
# page map was complete and cached (24 s), so the reopen had all 8370 pages. The host
# sim reproduces it (out/evidence/b-verify/sim/pct: percent 0.922 at DocFragment[55]).
# To reproduce here, the first visit must end before "background pagination done".
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

B=/mnt/us/Books/inkverify-pct
expect_log() { wait_until "${2:-30}" "log: $1" in_log "$1" || fail "no log line '$1'"; }
count() { local c; c=$(guest "grep -c -- '$1' $DEST/rinky.log; true" 2>/dev/null | tr -dc 0-9); echo "${c:-0}"; }
more() { [ "$(count "$1")" -gt "$2" ]; }

wait_home 300 home-before
clear_env
python3 -I "$(dirname "$0")/device-helpers/make-big-epub.py" "$E2E_ROOT/testdata/juliet.epub" "$OUT/bigjuliet.epub" 40 || fail "make big epub"
guest "rm -rf $B $DEST/library.json $DEST/library-view.json $DEST/cache/pagemaps; mkdir -p $B" || fail "prepare $B"
"$SLOT" push "$NAME" "$OUT/bigjuliet.epub" "$B/" >/dev/null || fail "push book"
rm -f "$OUT/bigjuliet.epub"
guest "rm -f $DEST/rinky.log $DEST/watchdog.log; sed -i '/^rotation/d' $DEST/settings.toml 2>/dev/null; sh $DEST/rinky.sh --library" || fail "launch"
wait_until 120 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over"
wait_until 120 "library scan logged" in_log "library scan:" || fail "no 'library scan:' line"
nap 8
tap 477 1631; nap 5
tap 177 210; nap 3
tap 300 $((256 + 8 + 110 + 55)); nap 4
# Sorted by title: Big Juliet first.
tap 600 345
expect_log "saved position" 120
nap 2
tap 636 848; expect_log "reader overlay Menu"; nap 2
tap 1100 1600; expect_log "reader action: page:"; nap 3
# The jump closed the menu: open it again, then Back.
tap 636 848; expect_log "reader overlay Menu"; nap 2
tap 62 56; expect_log "reader action: back"; nap 3
# Reopen at once (the page map was dropped with the reader) and leave fast.
n=$(count "saved position Some")
tap 600 345
wait_until 60 "reopened" more "saved position Some" "$n" || fail "no reopen"
tap 636 848; nap 2
tap 62 56; nap 4
shot library-after >/dev/null
guest "cat $B/bigjuliet.sdr/*.json" >"$OUT/sidecar.json" || fail "no sidecar"
save_logs
grep -E "saved position|pages \(parse|pagination done|reader action: page" "$OUT/rinky.log" >"$OUT/lines.txt"
pct=$(sed -n 's/.*"percent": \([0-9.e-]*\).*/\1/p' "$OUT/sidecar.json")
frag=$(sed -n 's/.*"position": "\/body\/DocFragment\[\([0-9]*\)\].*/\1/p' "$OUT/sidecar.json")
log "sidecar: percent $pct, DocFragment[$frag] of 1070"
tap 1147 75
wait_until 60 "rinky gone after Exit" reader_gone || fail "rinky still runs after Exit"
wait_clean_home 300 home-after
guest "rm -rf $B" || true
[ -n "$pct" ] && [ -n "$frag" ] || fail "cannot read percent/position from the sidecar"
if awk -v p="$pct" -v f="$frag" 'BEGIN { exit !(p > 0.9 && f < 535) }'; then
  fail "saved percent $pct for a position in spine item $frag of 1070 (partial page count used)"
fi
pass "saved percent $pct matches spine item $frag of 1070"
