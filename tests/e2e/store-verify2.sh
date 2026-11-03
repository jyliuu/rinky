#!/bin/bash
# ink-store verifier, cycle 2, on the emulator (no UI). store-verify.sh selects tests by
# `verifier_`, which does not match the cycle 2 names (`verifier2_`); this scenario runs
# them:
#  1. the verifier2 tests that are not ignored, on the user store (fuse.fsp) and on /tmp
#     (enforced);
#  2. the open defects about values that cannot be read back (ignored `verifier:` tests),
#     on the user store: logged with their result (each is expected to fail until fixed;
#     a pass is logged as fixed). The quadratic-import scaling tests run on the host; on
#     ARM their cost shows in the measurements below;
#  3. only with STORE_VERIFY2_MEASURE=1 (they take about 27 minutes while the import is
#     quadratic): the measurements (ignored `measurement:` tests) under `timeout`: KOReader
#     import of the largest accepted metadata files (compact hostile shapes: 600 s each;
#     KOReader's own dump.lua layout: 900 s), and sidecar save/load times with many
#     highlights. Logged; a timeout is logged as "did not finish". The cycle 2 run's
#     numbers are in out/evidence/store/verify-2/e2e/store-verify2/measure-*.log.
# Nothing here reads or writes /mnt/us/koreader.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

log "building the ink-store test binary for ARM"
TESTBIN=$(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release \
    --target armv7-unknown-linux-musleabihf -p ink-store --lib --tests --message-format=json 2>"$OUT/build-tests.log" \
  | grep -o '"executable":"[^"]*"' | grep -v null | tail -1 | sed 's/^"executable":"//; s/"$//')
[ -n "$TESTBIN" ] && [ -x "$TESTBIN" ] || fail "no ink-store test binary: $(tail -3 "$OUT/build-tests.log")"
echo "$TESTBIN $(cd "$E2E_ROOT" && git rev-parse --short HEAD)" >"$OUT/test-binary.txt"
G=$DEST/store-verify2
T=/mnt/us/rinky/tmp/store-verify2
guest "rm -rf $G $T; mkdir -p $G $T" || fail "mkdir $G"
"$SLOT" push "$NAME" "$TESTBIN" "$G/" || fail "push test binary"
guest "mv $G/$(basename "$TESTBIN") $G/ink_store_tests && chmod +x $G/ink_store_tests" || fail "chmod"
guest "uname -m; mount | grep -E ' /mnt/us | /tmp '; free -m" >"$OUT/guest.txt" 2>&1

log "verifier2 tests (not ignored) on the user store"
guest "cd $G && TMPDIR=$T ./ink_store_tests verifier2_ --test-threads=1 --nocapture" \
  >"$OUT/verifier2-userstore.log" 2>&1 || fail "verifier2 tests on /mnt/us: $(grep -E 'FAILED|panicked' "$OUT/verifier2-userstore.log" | head -3)"
pass "verifier2 tests on /mnt/us: $(grep 'test result' "$OUT/verifier2-userstore.log")"

log "verifier2 tests (not ignored) on /tmp"
guest "cd $G && TMPDIR=/tmp ./ink_store_tests verifier2_ --test-threads=1 --nocapture" \
  >"$OUT/verifier2-tmp.log" 2>&1 || fail "verifier2 tests on /tmp: $(grep -E 'FAILED|panicked' "$OUT/verifier2-tmp.log" | head -3)"
pass "verifier2 tests on /tmp: $(grep 'test result' "$OUT/verifier2-tmp.log")"

log "open defects (ignored verifier: tests) on the user store"
for t in verifier2_plugin_settings_deep_value_never_loses_other_settings \
         verifier2_plugin_settings_first_deep_value_keeps_the_file_loadable \
         verifier2_settings_deep_plugin_value_keeps_settings_loadable \
         verifier2_sidecar_deep_plugin_value_does_not_block_position_saves; do
  guest "cd $G && TMPDIR=$T timeout 900 ./ink_store_tests verify2_tests::$t --exact --include-ignored --nocapture; echo exit=\$?" \
    >"$OUT/defect-$t.log" 2>&1
  if grep -q 'test result: ok' "$OUT/defect-$t.log"; then
    log "defect test $t: passes (fixed)"
  else
    log "defect test $t: fails as reported: $(grep -E 'panicked|exit=' -A1 "$OUT/defect-$t.log" | grep -v '^--' | head -3 | tr '\n' ' ' | cut -c1-300)"
  fi
done

if [ "${STORE_VERIFY2_MEASURE:-0}" = 1 ]; then
  log "measurements (KOReader import at the parser's limits; sidecar save times)"
  for spec in verifier2_limits_timing_koreader_dump_format:900 verifier2_sidecar_save_time_with_many_highlights:900 \
              verifier2_limits_timing_modern_highlights:600 verifier2_limits_timing_legacy_pairs:600 \
              verifier2_limits_timing_modern_bookmarks:600; do
    t=${spec%%:*} limit=${spec##*:}
    start=$(date +%s)
    guest "cd $G && TMPDIR=$T timeout $limit ./ink_store_tests verify2_tests::$t --exact --include-ignored --nocapture; echo exit=\$?" \
      >"$OUT/measure-$t.log" 2>&1
    secs=$(( $(date +%s) - start ))
    if grep -q '^exit=0' "$OUT/measure-$t.log"; then
      log "measurement $t (${secs}s): $(grep 'verifier2' "$OUT/measure-$t.log" | tr '\n' ';' | cut -c1-600)"
    elif grep -q '^exit=143' "$OUT/measure-$t.log"; then
      log "measurement $t: did not finish in $limit s: $(grep 'verifier2' "$OUT/measure-$t.log" | tr '\n' ';' | cut -c1-400)"
    else
      log "measurement $t (${secs}s): $(grep -E 'verifier2|panicked|exit=' "$OUT/measure-$t.log" | tr '\n' ';' | cut -c1-400)"
    fi
  done
else
  log "measurements skipped (set STORE_VERIFY2_MEASURE=1 to run them, about 27 minutes)"
fi

guest "rm -rf $G $T" >"$OUT/cleanup.txt" 2>&1
pass "store verifier2 scenario done"
