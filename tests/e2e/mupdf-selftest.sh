#!/bin/bash
# The ink-mupdf test suite and the screen renderer, cross-built for ARM, run in the guest
# (framework running; nothing touches the screen). Saves the test log and PNGs rendered
# by the ARM build (fit page, crop, zoom tiles, columns, CBZ portrait and landscape
# spreads) under OUTDIR/guest-render/.
#   tests/e2e/mupdf-selftest.sh INSTANCE OUTDIR
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

TARGET=armv7-unknown-linux-musleabihf
BIN="$OUT/arm-bins.txt"
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release --target $TARGET -p ink-mupdf \
    --tests --examples --message-format=json 2>/dev/null) | python3 -c '
import sys, json
for line in sys.stdin:
    try:
        m = json.loads(line)
    except Exception:
        continue
    if m.get("reason") == "compiler-artifact" and m.get("executable"):
        print(m["target"]["name"], m["executable"])
' >"$BIN" || fail "cross build"
exe() { awk -v n="$1" '$1 == n { print $2 }' "$BIN"; }
for n in fixed ink_mupdf mupdf-render mupdf-fixtures; do
  [ -x "$(exe $n)" ] || fail "no ARM executable for $n"
done
T=$DEST/mupdf-test
guest "rm -rf $T; mkdir -p $T"
"$SLOT" push "$NAME" "$(exe fixed)" "$(exe ink_mupdf)" "$(exe mupdf-render)" "$(exe mupdf-fixtures)" "$T/" >/dev/null ||
  fail "push binaries"
guest "cd $T && for f in fixed-* ink_mupdf-*; do mv \$f \${f%-*}; done; chmod +x *; ls -la; uname -m; cat /etc/version.txt" \
  >"$OUT/guest-files.txt" 2>&1

guest "cd $T && ./ink_mupdf 2>&1; echo exit=\$?" >"$OUT/unit-tests.log" 2>&1
guest "cd $T && ./fixed 2>&1; echo exit=\$?" >"$OUT/fixed-tests.log" 2>&1
grep -q "exit=0" "$OUT/unit-tests.log" || fail "unit tests failed on ARM (unit-tests.log)"
grep -q "exit=0" "$OUT/fixed-tests.log" || fail "integration tests failed on ARM (fixed-tests.log)"
grep "test result" "$OUT/unit-tests.log" "$OUT/fixed-tests.log" | tee "$OUT/test-results.txt"
pass "ink-mupdf tests on ARM"

guest "cd $T && ./mupdf-fixtures $T/fx" >"$OUT/render.log" 2>&1 || fail "fixtures on ARM"
render() { # LABEL FILE WxH OPTIONS SCREENS (a list "0,4" or a count "3"; one screen: "5,")
  echo "== $1: $2 $3 '$4' $5" >>"$OUT/render.log"
  guest "cd $T && ./mupdf-render $T/fx/$2 $T/out/$1 $3 '$4' $5" >>"$OUT/render.log" 2>&1 || fail "render $1"
}
render pdf-fit fixture.pdf 1272x1696 "" 0,1,2
render pdf-crop fixture.pdf 1272x1696 "crop=1" 0,
render pdf-zoom2 fixture.pdf 1272x1696 "zoom=2" 0,1,2,3
render pdf-columns fixture.pdf 1272x1696 "crop=1,columns=2,fit=width" 10,15
render pdf-fitwidth-landscape fixture.pdf 1696x1272 "fit=width" 0,1
render pdf-rot90-gamma fixture.pdf 1272x1696 "rotate=90,gamma=1.8" 0,
render cbz-portrait comic.cbz 1272x1696 "" 0,1
render cbz-spread-landscape comic.cbz 1696x1272 "" 0,1,6
render cbz-spread-rtl comic.cbz 1696x1272 "rtl=1" 1,
mkdir -p "$OUT/guest-render"
for d in pdf-fit pdf-crop pdf-zoom2 pdf-columns pdf-fitwidth-landscape pdf-rot90-gamma cbz-portrait cbz-spread-landscape cbz-spread-rtl; do
  mkdir -p "$OUT/guest-render/$d"
  for f in $(guest "ls $T/out/$d"); do
    "$SLOT" pull "$NAME" "$T/out/$d/$f" "$OUT/guest-render/$d/$f" >/dev/null || fail "pull $d/$f"
  done
done
grep -E "screens, plan|screen [0-9]+:|cache" "$OUT/render.log" | head -60
guest "rm -rf $T"
pass "ARM renders saved in guest-render/"
