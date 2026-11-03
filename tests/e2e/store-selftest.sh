#!/bin/bash
# ink-store on the emulator (no UI): `rinky selftest` (store checks included), the
# store_selftest example on the user store (fuse.fsp) and on /tmp, and the cross-built ink-store
# unit-test binary on both file systems. Needs a current deploy; builds the example and
# the test binary itself (cargo zigbuild, release, ARM).
# Optional: KOREADER_TEST_DIR (host) with KOReader's test/leaves.epub and test/tall.pdf for
# the spec hashes; KOREADER_SAMPLE_DIR (host) with help/<book> + help/<book>.sdr for a real
# sidecar. Nothing here reads or writes /mnt/us/koreader.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

ARM="$E2E_ROOT/target/armv7-unknown-linux-musleabihf/release"
EXAMPLE="$ARM/examples/store_selftest"
log "building store_selftest and the ink-store test binary for ARM"
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release \
    --target armv7-unknown-linux-musleabihf -p ink-store --example store_selftest) >"$OUT/build-example.log" 2>&1 \
  || fail "build store_selftest: $(tail -3 "$OUT/build-example.log")"
# The test executable's name carries a hash; take it from cargo's JSON messages.
TESTBIN=$(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release \
    --target armv7-unknown-linux-musleabihf -p ink-store --lib --tests --message-format=json 2>"$OUT/build-tests.log" \
  | grep -o '"executable":"[^"]*"' | grep -v null | tail -1 | sed 's/^"executable":"//; s/"$//')
[ -x "$EXAMPLE" ] || fail "missing $EXAMPLE"
[ -n "$TESTBIN" ] && [ -x "$TESTBIN" ] || fail "no ink-store test binary: $(tail -3 "$OUT/build-tests.log")"
log "test binary $TESTBIN"
G=$DEST/store-test
guest "rm -rf $G /mnt/us/rinky/tmp/store-*; mkdir -p $G/kotest $G/sample/help" || fail "mkdir $G"
"$SLOT" push "$NAME" "$EXAMPLE" "$TESTBIN" "$G/" || fail "push binaries"
guest "mv $G/$(basename "$TESTBIN") $G/ink_store_tests && chmod +x $G/store_selftest $G/ink_store_tests" || fail "chmod"
KT=${KOREADER_TEST_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/koreader/test}
KT_ARG=
if [ -f "$KT/leaves.epub" ] && [ -f "$KT/tall.pdf" ]; then
  "$SLOT" push "$NAME" "$KT/leaves.epub" "$KT/tall.pdf" "$G/kotest/" || fail "push KOReader test files"
  KT_ARG="--koreader-test-dir $G/kotest"
fi
KS=${KOREADER_SAMPLE_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/inkreader-wt/.cache/store-ko/koreader}
if [ -f "$KS/help/quickstart-en-v2025.08.html" ]; then
  guest "mkdir -p $G/sample/help/quickstart-en-v2025.08.sdr"
  "$SLOT" push "$NAME" "$KS/help/quickstart-en-v2025.08.html" "$G/sample/help/" || fail "push sample book"
  "$SLOT" push "$NAME" "$KS/help/quickstart-en-v2025.08.sdr/metadata.html.lua" "$G/sample/help/quickstart-en-v2025.08.sdr/" || fail "push sample sidecar"
fi

guest "uname -a; cat /etc/version.txt 2>/dev/null; mount | grep -E ' /mnt/us | /tmp '; df -h /mnt/us /tmp" >"$OUT/guest.txt" 2>&1

log "rinky selftest"
guest "cd $DEST && ./rinky selftest --book $DEST/juliet.epub" >"$OUT/rinky-selftest.log" 2>&1
grep -q "selftest: .* 0 failed" "$OUT/rinky-selftest.log" || fail "rinky selftest: $(tail -1 "$OUT/rinky-selftest.log")"
n=$(grep -c "^PASS store\." "$OUT/rinky-selftest.log")
[ "$n" -ge 12 ] || fail "only $n store checks passed in rinky selftest"
pass "rinky selftest: $n store checks passed"

log "store_selftest on the user store"
guest "$G/store_selftest --scratch /mnt/us/rinky/tmp/store-selftest $KT_ARG $DEST/juliet.epub" >"$OUT/store-selftest-userstore.log" 2>&1 \
  || fail "store_selftest on /mnt/us: $(grep FAIL "$OUT/store-selftest-userstore.log" | head -3)"
pass "store_selftest on /mnt/us: $(tail -1 "$OUT/store-selftest-userstore.log")"

log "store_selftest on /tmp"
guest "$G/store_selftest --scratch /tmp/store-selftest $KT_ARG" >"$OUT/store-selftest-tmp.log" 2>&1 \
  || fail "store_selftest on /tmp: $(grep FAIL "$OUT/store-selftest-tmp.log" | head -3)"
pass "store_selftest on /tmp: $(tail -1 "$OUT/store-selftest-tmp.log")"

ENVS="INK_TESTDATA_DIR=$DEST KOREADER_TEST_DIR=$G/kotest KOREADER_SAMPLE_DIR=$G/sample"
log "unit tests, TMPDIR on the user store"
guest "mkdir -p /mnt/us/rinky/tmp/store-tests && cd $G && $ENVS TMPDIR=/mnt/us/rinky/tmp/store-tests ./ink_store_tests --test-threads=1 --nocapture" \
  >"$OUT/unit-tests-userstore.log" 2>&1 || fail "unit tests on /mnt/us: $(grep -E 'FAILED|panicked' "$OUT/unit-tests-userstore.log" | head -3)"
grep -q "test result: ok" "$OUT/unit-tests-userstore.log" || fail "no test result on /mnt/us"
pass "unit tests on /mnt/us: $(grep 'test result' "$OUT/unit-tests-userstore.log")"

log "unit tests, TMPDIR=/tmp"
guest "cd $G && $ENVS TMPDIR=/tmp ./ink_store_tests --test-threads=2" >"$OUT/unit-tests-tmp.log" 2>&1 \
  || fail "unit tests on /tmp: $(grep -E 'FAILED|panicked' "$OUT/unit-tests-tmp.log" | head -3)"
pass "unit tests on /tmp: $(grep 'test result' "$OUT/unit-tests-tmp.log")"

guest "ls -la /mnt/us/rinky/tmp 2>/dev/null; rm -rf /mnt/us/rinky/tmp/store-* $G" >"$OUT/cleanup.txt" 2>&1
pass "store checks done"
