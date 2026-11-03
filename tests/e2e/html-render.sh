#!/bin/bash
# Phase 2 reflowable engine on the emulator (ARM build):
# 1. inkhtml bench on juliet.epub twice: first page time and background pagination with
#    a cold page-map cache, then with the cache file (keyed by partial MD5 + layout);
# 2. the reader (rinky.sh, framework stopped) showing juliet.epub, the image fixture
#    and the table/list fixture, with a screenshot per page and the engine's log lines.
# Needs: scripts/emu-slot.sh deploy NAME. Builds the tool with
#   cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p ink-html --example inkhtml
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
TOOL=$E2E_ROOT/target/armv7-unknown-linux-musleabihf/release/examples/inkhtml
FIX=$E2E_ROOT/crates/ink-html/tests/fixtures
# The scenario builds the tool itself (incremental; neither run.sh nor deploy builds it).
log "building the ARM inkhtml example"
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release \
  --target armv7-unknown-linux-musleabihf -p ink-html --example inkhtml) >"$OUT/inkhtml-zigbuild.log" 2>&1 ||
  fail "inkhtml zigbuild (inkhtml-zigbuild.log)"
[ -x "$TOOL" ] || fail "missing $TOOL"
"$SLOT" push "$NAME" "$TOOL" "$FIX/images.epub" "$FIX/tables-lists.epub" "$DEST/" >/dev/null || fail "push"
guest "chmod +x $DEST/inkhtml" || fail "chmod"

# 1. Bench: cold cache, then warm cache.
guest "rm -rf /tmp/inkhtml-pm; cd $DEST && ./inkhtml bench juliet.epub --cache /tmp/inkhtml-pm" >"$OUT/bench-cold.log" 2>&1 || fail "bench cold"
guest "cd $DEST && ./inkhtml bench juliet.epub --cache /tmp/inkhtml-pm; ls -la /tmp/inkhtml-pm" >"$OUT/bench-warm.log" 2>&1 || fail "bench warm"
grep -q "first page ready" "$OUT/bench-cold.log" || fail "no first page line"
grep -q "page map paginating" "$OUT/bench-cold.log" || fail "cold run did not paginate"
grep -q "page map from cache" "$OUT/bench-warm.log" || fail "warm run did not use the cache"
grep -q ".pagemap" "$OUT/bench-warm.log" || fail "no cache file"
log "cold: $(grep 'first page' "$OUT/bench-cold.log" | tr '\n' ' ')"
log "warm: $(grep 'first page' "$OUT/bench-warm.log" | tr '\n' ' ')"
pass "bench (first page time, page-map cache)"

# 2. Reader screenshots.
first_page_shown() { guest "grep -q 'saved position' $DEST/rinky.log" 2>/dev/null; }
read_book() { # BOOK LABEL PAGES
  local book=$1 label=$2 pages=$3 i
  # No saved position from an earlier run in this boot: the book opens at page 1.
  guest "rm -rf $DEST/${book%.epub}.sdr" || fail "$label: remove sidecar"
  start_reader "$DEST/$book"
  wait_until 120 "$label: first page shown" first_page_shown || fail "$label: no first page"
  guest "grep -q 'saved position None -> page 1' $DEST/rinky.log" || fail "$label: did not open at page 1"
  sleep 3
  not_home "$label-p1"
  for i in $(seq 2 "$pages"); do
    tap 1000 900
    sleep 4
    not_home "$label-p$i"
  done
  save_logs
  cp "$OUT/rinky.log" "$OUT/rinky-$label.log"
  grep -q "ink-html: .*first page ready" "$OUT/rinky-$label.log" || fail "$label: no engine log line"
  exit_device
  wait_clean_home 300 "home-after-$label"
}

# The reader's own page-map cache (default location on the Kindle) starts empty.
guest "rm -rf $DEST/cache/pagemaps" || true
wait_home 300 home-before
read_book juliet.epub juliet 7
read_book images.epub images 7
read_book tables-lists.epub tables 5
pass "reader showed juliet, images and tables"
