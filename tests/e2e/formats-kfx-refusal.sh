#!/bin/bash
# e2e-cycles: 2
# Formats: launching rinky on a KFX file and on a DRM EPUB. ink-formats refuses them
# with a message; the reader shows "Cannot open this book" and the message on screen
# (screenshot), stays until a tap, then exits 0 and the launcher brings the stock UI back.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

F=$DEST/formats
in_log() { guest "grep -q -- '$1' $DEST/rinky.log"; }
guest "mkdir -p $F" || fail "mkdir $F"
"$SLOT" push "$NAME" "$E2E_ROOT/testdata/formats/kfx-container.kfx" "$E2E_ROOT/testdata/formats/drm-adept.epub" "$F/" >/dev/null || fail "push books"
# refused LABEL BOOK TEXT: open BOOK, the error screen stays, a tap ends the reader.
refused() {
  local label=$1 book=$2 text=$3
  wait_home 300 "$label-home-before"
  guest "rm -f $DEST/rinky.log; sh $DEST/rinky.sh $book" || fail "launch $label"
  wait_until 120 "$label: refusal logged" in_log "cannot open" || fail "$label: no 'cannot open' line in rinky.log"
  sleep 5
  reader_running || fail "$label: rinky ended without waiting for a tap"
  not_home "$label-error"
  save_logs
  cp "$OUT/rinky.log" "$OUT/$label-rinky.log"
  grep -q -- "$text" "$OUT/$label-rinky.log" || fail "$label: no '$text' in rinky.log"
  grep "cannot open" "$OUT/$label-rinky.log" | tee "$OUT/$label-refusal.txt"
  tap 600 900
  wait_until 60 "$label: reader gone after the tap" reader_gone || fail "$label: rinky still runs after the tap"
  wait_clean_home 300 "$label-home-after"
  save_logs
  grep -q "rinky exited with 0" "$OUT/rinky.log" || fail "$label: exit status not 0 ($(grep 'exited with' "$OUT/rinky.log"))"
  pass "$label: error shown ($label-error.png), tap returned to Home"
}
refused kfx "$F/kfx-container.kfx" "KFX book"
refused drm "$F/drm-adept.epub" "protected by DRM"
pass "formats kfx and drm refusal"
