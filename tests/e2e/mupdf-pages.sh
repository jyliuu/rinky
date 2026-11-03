#!/bin/bash
# PDF and CBZ pages rendered by the ARM build in run mode (framework stopped).
# Generates the fixtures on the host (ink_mupdf::testgen), pushes them, opens each with
# the launcher, checks pixels of the QMP screenshots and the log, turns a page, exits.
#   tests/e2e/mupdf-pages.sh INSTANCE OUTDIR      (after scripts/emu-slot.sh deploy)
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

FIX="$OUT/fixtures"
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo run -q -p ink-mupdf --example mupdf-fixtures -- "$FIX") ||
  fail "fixture generation"
"$SLOT" push "$NAME" "$FIX/fixture.pdf" "$FIX/comic.cbz" "$DEST/" >/dev/null || fail "push fixtures"
guest "rm -rf $DEST/fixture.sdr $DEST/comic.sdr"

# dark_at SHOT X Y W H MIN: at least MIN of the box is dark (< 64).
dark_at() {
  local f
  f=$(python3 "$SCREEN" dark "$1" "$2" "$3" "$4" "$5") || return 1
  python3 -c "import sys; sys.exit(0 if float('$f') >= float('$6') else 1)" || { log "dark fraction $f < $6 in $2,$3 ${4}x$5 of $1"; return 1; }
}
light_at() {
  local f
  f=$(python3 "$SCREEN" dark "$1" "$2" "$3" "$4" "$5") || return 1
  python3 -c "import sys; sys.exit(0 if float('$f') <= float('$6') else 1)" || { log "dark fraction $f > $6 in $2,$3 ${4}x$5 of $1"; return 1; }
}

# open_book BOOK [RINKY_FIXED]: like start_reader, with fixed-layout options.
open_book() {
  guest "rm -f $DEST/rinky.log $DEST/watchdog.log; RINKY_FIXED='${2:-}' sh $DEST/rinky.sh $1" || fail "launch $1"
  wait_until 90 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over for $1"
  sleep 5
}

close_book() {
  exit_device
  wait_clean_home 300 "$1"
}

wait_home 240 home-before

# --- PDF, fit page: Letter page scaled by 1272/612, centred vertically (top 25 px).
open_book "$DEST/fixture.pdf"
f=$(shot pdf-page1) || exit 1
is_home "$f" && fail "PDF: still Home"
dark_at "$f" 330 240 170 25 0.95 || fail "PDF page 1: black marker (150..250 x 100..120 pt) missing"
light_at "$f" 330 140 170 25 0.01 || fail "PDF page 1: area above the marker not white"
guest "cat $DEST/rinky.log" >"$OUT/pdf.log"
grep -q "mupdf 1.27.2: Pdf 10 pages" "$OUT/pdf.log" || fail "PDF: provider log line missing"
grep -q "'rinky fixture': 10 pages" "$OUT/pdf.log" || fail "PDF: title/page count missing in log"
tap 1000 900
sleep 4
f=$(shot pdf-page2) || exit 1
light_at "$f" 380 240 120 25 0.01 || fail "PDF page 2: marker still shown (no page turn)"
dark_at "$f" 150 265 400 60 0.03 || fail "PDF page 2: no text in the left column"
# The position is written 2 s after the last page turn (POSITION_SAVE_IDLE) with no
# further input (the app loop takes the reader's timeout after the paint that notes it).
sidecar_page1() { guest "grep -q '\"position\": \"page:1\"' $DEST/fixture.sdr/rinky.pdf.json" 2>/dev/null; }
wait_until 12 "PDF sidecar position page:1 after the idle delay" sidecar_page1 ||
  fail "PDF: sidecar position not written 12 s after the page turn (idle save did not run)"
pass "PDF pages 1 and 2"
close_book home-after-pdf
pos=$(guest "cat $DEST/fixture.sdr/rinky.pdf.json")
echo "$pos" >"$OUT/pdf-sidecar.json"
echo "$pos" | grep -q '"position": "page:1"' || fail "PDF sidecar position is not page:1 after exit: $(echo "$pos" | tr -d '\n ' | cut -c1-120)"

# --- PDF with crop margins: crop box about 280 x 400 pt, scale 4.23 px/pt: the marker
# covers about x 86..509, y 40..125.
open_book "$DEST/fixture.pdf" "crop=1"
f=$(shot pdf-page2-crop) || exit 1
tap 100 900
sleep 4
f=$(shot pdf-page1-crop) || exit 1
dark_at "$f" 120 60 300 40 0.95 || fail "PDF crop: enlarged marker missing"
pass "PDF with margin crop"
close_book home-after-crop

# --- CBZ: page 1 = 600 x 900 px image fit to the height (scale 1.885 px per image px,
# left edge 71 px); its top bar is 40 image px long (x 109..184), page 2's is 80.
open_book "$DEST/comic.cbz"
f=$(shot cbz-page1) || exit 1
is_home "$f" && fail "CBZ: still Home"
dark_at "$f" 115 45 60 40 0.95 || fail "CBZ page 1: top bar missing"
light_at "$f" 200 45 50 40 0.02 || fail "CBZ page 1: bar too long (wrong page?)"
guest "cat $DEST/rinky.log" >"$OUT/cbz.log"
grep -q "mupdf 1.27.2: Cbz 12 pages" "$OUT/cbz.log" || fail "CBZ: provider log line missing (12 pages expected)"
tap 1000 900
sleep 4
f=$(shot cbz-page2) || exit 1
dark_at "$f" 200 45 50 40 0.95 || fail "CBZ page 2: 80 px bar missing"
light_at "$f" 280 45 40 40 0.02 || fail "CBZ page 2: bar too long"
pass "CBZ pages 1 and 2 in natural order"
# SIGTERM right after a page turn (before the 2 s idle save): `rinky-ctl.sh stop`
# as soon as the log has the turn; the app loop closes the reader, which writes page 3.
tap 1000 900
guest "i=0; until grep -q 'reader position page 3 ' $DEST/rinky.log; do usleep 100000; i=\$((i + 1)); [ \$i -lt 100 ] || exit 1; done; sh $DEST/rinky-ctl.sh stop" >/dev/null 2>&1 ||
  fail "CBZ: page 3 turn not logged within 10 s, or stop failed"
wait_until 60 "rinky exited" reader_gone || fail "rinky did not exit"
wait_clean_home 300 home-after-cbz
guest "cat $DEST/rinky.log" >"$OUT/cbz-exit.log" 2>/dev/null || true
grep "exit: reason" "$OUT/cbz-exit.log" | tail -n 1 | sed 's/^/  /'
# The app loop closed the reader (not the signal thread's 2 s fallback exit).
grep -q -- "-> (exit) (signal SIGTERM)" "$OUT/cbz-exit.log" || fail "CBZ: the app loop did not close the screens on SIGTERM"
pos=$(guest "cat $DEST/comic.sdr/rinky.cbz.json")
echo "$pos" >"$OUT/cbz-sidecar.json"
echo "$pos" | grep -q '"position": "page:2"' || fail "CBZ sidecar position is not page:2 after SIGTERM right after the turn: $(echo "$pos" | tr -d '\n ' | cut -c1-120)"
pass "CBZ position written on SIGTERM right after a page turn"
save_logs
pass "mupdf pages"
