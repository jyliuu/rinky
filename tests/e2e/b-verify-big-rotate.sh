#!/bin/bash
# e2e-cycles: 1
# Wave B verifier: library mode with a very large EPUB (juliet's chapters repeated 40
# times, about 1070 spine items, built on the host by device-helpers/make-big-epub.py),
# a DRM EPUB, quick light swipe right after a page-turn tap, leaving the big book while
# it still paginates, rotation to landscape in Settings, reopening the book in
# landscape (position kept), back to portrait, Exit to the Kindle Home.
# Coordinates: portrait 1272 x 1696; landscape turned right: panel = (1272 - ly, lx).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

B=/mnt/us/Books/inkverify-big
expect_log() { wait_until "${2:-30}" "log: $1" in_log "$1" || fail "no log line '$1'"; }
count() { local c; c=$(guest "grep -c -- '$1' $DEST/rinky.log; true" 2>/dev/null | tr -dc 0-9); echo "${c:-0}"; }
more() { [ "$(count "$1")" -gt "$2" ]; }
alive() {
  local p; p=$(rinky_pid)
  [ -n "$p" ] || fail "$1: rinky is not running"
  [ "$p" = "$PID" ] || fail "$1: rinky pid changed $PID -> $p (restarted)"
  guest "grep -q panicked $DEST/rinky.log" && fail "$1: panic in rinky.log"
  log "$1: rinky $p alive"
}
row() { echo $((345 + 150 * $1)); }

wait_home 300 home-before
clear_env
python3 -I "$(dirname "$0")/device-helpers/make-big-epub.py" "$E2E_ROOT/testdata/juliet.epub" "$OUT/bigjuliet.epub" 40 || fail "make big epub"
guest "rm -rf $B $DEST/library.json $DEST/library-view.json; mkdir -p $B" || fail "prepare $B"
"$SLOT" push "$NAME" "$OUT/bigjuliet.epub" "$E2E_ROOT/testdata/formats/drm-adept.epub" "$B/" >/dev/null || fail "push books"
rm -f "$OUT/bigjuliet.epub"
guest "rm -f $DEST/rinky.log $DEST/watchdog.log; sed -i '/^rotation/d' $DEST/settings.toml 2>/dev/null; sh $DEST/rinky.sh --library" || fail "launch"
wait_until 120 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over"
wait_until 120 "library scan logged" in_log "library scan:" || fail "no 'library scan:' line"
nap 8
PID=$(rinky_pid)
tap 477 1631; nap 5
tap 177 210; nap 3
tap 300 $((256 + 8 + 110 + 55)); nap 4
not_home library-sorted
# Sorted by title: Big Juliet, JAILBROKEN, Protected Sample (drm-adept), more.
# 1. DRM book: the reader's error page, a tap goes back.
tap 600 "$(row 2)"
expect_log "cannot open"
nap 4; alive drm
not_home drm-error
tap 600 900; nap 4
alive drm-back
not_home drm-back
# 2. Big book: open, three page turns, quick light swipe right after a page-turn tap.
t0=$(date +%s)
tap 600 "$(row 0)"
expect_log "saved position" 120
log "big book first page after $(( $(date +%s) - t0 )) s"
nap 4; alive big-open
not_home big-open
for i in 1 2 3; do tap 1100 900; nap 2; done
tap 1100 900; swipe 600 20 600 700
expect_log "quick light open"
nap 3; alive quick-light
not_home quick-light-after-turn
tap 1094 575; expect_log "quick light closed"; nap 4
not_home quick-light-closed
guest "grep -E 'saved position|pages \\(parse|pagination|page map' $DEST/rinky.log" >"$OUT/big-lines.txt" 2>&1
# 3. Leave while the page map may still be building: menu, Back.
tap 636 848; expect_log "reader overlay Menu"; nap 3
tap 62 56; nap 6
alive big-closed
pos1=$(guest "cat $B/bigjuliet.sdr/*.json" | tee "$OUT/sidecar-1.json" | sed -n 's/.*"position": "\(.*\)".*/\1/p')
pct1=$(sed -n 's/.*"percent": \(.*\),/\1/p' "$OUT/sidecar-1.json")
log "saved position $pos1 percent $pct1"
[ -n "$pos1" ] || fail "no position saved for the big book"
not_home library-after-big
# 4. Settings -> Display -> landscape, Back, Back (library in landscape).
tap 1088 75; expect_log "screen: settings$"; nap 3
tap 600 $((225 + 130 * 4)); expect_log "settings page Display"; nap 3
tap 600 270; expect_log "rotation Clockwise"; nap 5
alive rotated
tap 165 230; nap 3
tap 165 230; nap 4
not_home library-landscape
# 5. Reopen the big book in landscape: the same position must be restored.
n=$(count "saved position Some")
tap $((1272 - 345)) 600
wait_until 120 "reopened" more "saved position Some" $n || fail "big book did not reopen in landscape"
nap 5; alive reopened-landscape
not_home big-landscape
guest "grep 'saved position Some' $DEST/rinky.log | tail -1" >"$OUT/reopen-line.txt"
grep -qF "Some(\"$pos1\")" "$OUT/reopen-line.txt" || fail "reopened at another position: $(cat "$OUT/reopen-line.txt") (saved $pos1)"
# 6. Menu Back (landscape), library Settings, Display, Portrait, Back, Back.
tap 636 848; expect_log "reader overlay Menu"; nap 3
tap 1216 62; nap 5
alive landscape-back
tap 1198 1340; nap 4
tap 540 300; nap 3
tap 1050 300; expect_log "rotation Portrait"; nap 5
alive portrait-again
tap 230 1530; nap 3
tap 230 1530; nap 4
not_home library-portrait
tap 1147 75
wait_until 60 "rinky gone after Exit" reader_gone || fail "rinky still runs after Exit"
wait_clean_home 300 home-after
save_logs
grep -q "rinky exited with 0" "$OUT/rinky.log" || fail "exit status not 0 ($(grep 'exited with' "$OUT/rinky.log"))"
guest "rm -rf $B" || true
pass "big book, DRM tap, quick light after a page turn, landscape reopen kept the position, exit"
