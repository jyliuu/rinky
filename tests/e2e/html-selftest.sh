#!/bin/bash
# Phase 2 engine library checks on the emulator (ARM build of examples/inkhtml):
# 1. `inkhtml info` on juliet (TOC with pages, locations), pagelist (page labels) and
#    notes: logs saved;
# 2. `inkhtml render` of golden pages on the guest, pulled back and compared pixel by
#    pixel with tests/golden (scripts/png-compare.py): the ARM build must draw exactly
#    what the goldens record.
# Builds the tool with
#   cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p ink-html --example inkhtml
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
TOOL=$E2E_ROOT/target/armv7-unknown-linux-musleabihf/release/examples/inkhtml
FIX=$E2E_ROOT/crates/ink-html/tests/fixtures
GOLDEN=$E2E_ROOT/tests/golden
# The scenario builds the tool itself (incremental; neither run.sh nor deploy builds it).
log "building the ARM inkhtml example"
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release \
  --target armv7-unknown-linux-musleabihf -p ink-html --example inkhtml) >"$OUT/inkhtml-zigbuild.log" 2>&1 ||
  fail "inkhtml zigbuild (inkhtml-zigbuild.log)"
[ -x "$TOOL" ] || fail "missing $TOOL"
"$SLOT" push "$NAME" "$TOOL" "$FIX/pagelist.epub" "$FIX/notes.epub" "$FIX/images.epub" "$FIX/tables-lists.epub" "$FIX/lang.epub" "$DEST/" >/dev/null || fail "push"
guest "chmod +x $DEST/inkhtml" || fail "chmod"

for b in juliet pagelist notes; do
  guest "cd $DEST && ./inkhtml info $b.epub" >"$OUT/info-$b.log" 2>&1 || fail "info $b"
done
grep -q "^toc Act I -> /body/DocFragment\[4\].0 (page Some(" "$OUT/info-juliet.log" || fail "juliet TOC"
[ "$(grep -c '^page-label' "$OUT/info-pagelist.log")" = 6 ] || fail "page labels"
grep -q "^locations Some(Location(1090))" "$OUT/info-juliet.log" || fail "locations"
pass "info: TOC, page labels, locations"

# render BOOK "PAGES" EXTRA: golden pages on the guest, compared with tests/golden.
compare() {
  local book=$1 pages=$2 extra=$3 prefix=$4 p f
  guest "rm -rf /tmp/g-$prefix; cd $DEST && ./inkhtml render $book.epub /tmp/g-$prefix $pages $extra" >"$OUT/render-$prefix.log" 2>&1 || fail "render $prefix"
  mkdir -p "$OUT/guest-$prefix"
  for p in $pages; do
    f=$(printf 'page-%04d.png' "$p")
    "$SLOT" pull "$NAME" "/tmp/g-$prefix/$f" "$OUT/guest-$prefix/$prefix-p$(printf '%04d' "$p").png" >/dev/null || fail "pull $f"
  done
  python3 "$E2E_ROOT/scripts/png-compare.py" "$OUT/guest-$prefix" "$GOLDEN" >"$OUT/compare-$prefix.txt" 2>&1
  local rc=$?
  cat "$OUT/compare-$prefix.txt" >>"$OUT/scenario.log"
  [ $rc = 0 ] || fail "$prefix differs from the goldens (see compare-$prefix.txt)"
}
compare juliet "1 2 3 6 51" "" juliet
compare images "1 2 3 4 5 6" "" images
compare tables-lists "1 2 4 5" "" tables-lists
compare lang "1" "--hyphenate" lang
pass "ARM renders match the golden pages"
