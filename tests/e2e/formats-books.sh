#!/bin/bash
# e2e-cycles: 3
# Formats: opens an FB2, a TXT (windows-1251) and an AZW3 book in rinky one after the
# other (framework stopped by the launcher each time), screenshots the first page and the
# page after a tap, checks the "'TITLE': N pages" line in rinky.log, exits with the
# top-right corner and waits for the stock Home.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

F=$DEST/formats
TD="$E2E_ROOT/testdata/formats"
guest "mkdir -p $F" || fail "mkdir $F"
"$SLOT" push "$NAME" "$TD/sample.fb2" "$TD/alice-kf8.azw3" "$TD/txt/ru-cp1251.txt" "$F/" >/dev/null ||
  fail "push fixtures"

opened() { guest "grep -c \"'$1': [0-9]* \(known \)\{0,1\}pages\" $DEST/rinky.log" 2>/dev/null | grep -qv '^0$'; }

# book FILE LABEL TITLE
book() {
  local file=$1 label=$2 title=$3
  wait_home 300 "$label-home-before"
  start_reader "$F/$file"
  wait_until 60 "'$title' opened" opened "$title" || { save_logs; fail "no \"'$title': N pages\" in rinky.log"; }
  not_home "$label-page1"
  tap 1000 900
  sleep 4
  not_home "$label-page2"
  save_logs
  cp "$OUT/rinky.log" "$OUT/$label-rinky.log"
  grep "'$title'" "$OUT/$label-rinky.log" | tee -a "$OUT/opened.txt"
  exit_device
  wait_clean_home 300 "$label-home-after"
  pass "$label: '$title' opened, page turned, framework back"
}

book sample.fb2 fb2 "FB2 Sample Book"
book ru-cp1251.txt txt "ru-cp1251"
book alice-kf8.azw3 azw3 "Alice's Adventures in Wonderland"

pass "formats books"
