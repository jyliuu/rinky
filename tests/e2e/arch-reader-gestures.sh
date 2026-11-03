#!/bin/bash
# e2e-cycles: 1
# Reader gestures through the emulator's touch path (fts_ts): tap, swipe left/right,
# vertical swipe (ignored), long press, then exit. Checks the page number in
# rinky.log after each step and keeps a screenshot per step.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

gestures() { guest "grep -c 'gesture:' $DEST/rinky.log" 2>/dev/null || echo 0; }
at_least() { [ "$(gestures)" -ge "$1" ]; }
expect_gesture() {
  wait_until 20 "gesture count $1" at_least "$1" || fail "expected $1 gestures, log has $(gestures)"
}

wait_home 240 home-before
start_reader
not_home page1
tap 1000 900;                expect_gesture 1; nap 3; not_home tap-next
swipe 1000 800 200 800;      expect_gesture 2; nap 3; not_home swipe-left-next
swipe 200 800 1000 800;      expect_gesture 3; nap 3; not_home swipe-right-prev
swipe 600 300 600 1200;      expect_gesture 4; nap 3; not_home swipe-down-ignored
"$SLOT" hold "$NAME" 900 900 1.0 >/dev/null || fail "hold"
expect_gesture 5; nap 3; not_home long-press-next
save_logs
grep "gesture:" "$OUT/rinky.log" | tee "$OUT/gestures.txt"
grep -q "Swipe .*dir: Left" "$OUT/gestures.txt" || fail "no left swipe recognised"
grep -q "Swipe .*dir: Right" "$OUT/gestures.txt" || fail "no right swipe recognised"
grep -q "Swipe .*dir: Down" "$OUT/gestures.txt" || fail "no vertical swipe recognised"
grep -q "LongPress" "$OUT/gestures.txt" || fail "no long press recognised"
# Turns: +1 (tap) +1 (swipe left) -1 (swipe right) 0 (down) +1 (long press) = page 3.
pos=$(guest "cat $DEST/juliet.sdr/rinky.epub.json")
echo "$pos" >"$OUT/sidecar.json"
# Phase 2 engine: positions are crengine xpointers. The sidecar percent is the location
# of the page start (wave B reader2), so the page is read from the reader's log line
# "reader position page N percent P", written with every sidecar save.
echo "$pos" | grep -q '"position": *"/body/DocFragment' || fail "expected an xpointer position in the sidecar: $pos"
grep "reader position page" "$OUT/rinky.log" | tee "$OUT/positions.txt" >/dev/null
last=$(tail -n 1 "$OUT/positions.txt")
echo "$last" | grep -q "reader position page 3 percent" \
  || fail "expected page 3 in the last 'reader position' log line: $last"
pct=$(echo "$pos" | sed -n 's/.*"percent": *\([0-9.eE-]*\).*/\1/p')
# Page 3 of juliet is still front matter: its first character is in location 1, so the
# percent is 0.0 (merged run in fumerge-jb); only an upper bound is checked.
awk -v p="$pct" 'BEGIN { exit !(p != "" && p >= 0 && p < 0.05) }' \
  || fail "expected a percent from 0 to below 5 % in the sidecar: $pos"
pass "page 3 after tap, swipes and long press"
exit_device
wait_clean_home 300 home-after
pass "reader gestures"
