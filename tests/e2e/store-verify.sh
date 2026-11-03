#!/bin/bash
# ink-store verifier checks on the emulator (no UI), after the cycle 1 fixes. The verifier
# tests that are not ignored also run inside store-selftest.sh (the whole unit-test
# binary). This scenario adds:
#  1. whether the user store (fuse.fsp on 5.19.5) and /tmp accept a file name that is not
#     UTF-8 (logged);
#  2. the verifier tests plus store_load_never_misses_a_file_being_renamed and
#     stats_open_at_once_on_a_new_file on /mnt/us (with INK_VERIFY_RO_DIR=/, EROFS) and on
#     /tmp (enforced);
#  3. the rename gap, 3 runs per file system: the raw file-system property
#     (verifier_write_atomic_target_never_missing_for_a_reader, logged), the store's loads
#     during renames (store_load_never_misses_a_file_being_renamed) and the two-writer
#     sidecar test (both enforced), and the renameat2(RENAME_EXCHANGE) probe (logged);
#  4. statistics writers on /mnt/us, 3 runs (enforced);
#  5. Lua literal memory on ARM: MAX_VALUES - 1 entries (bytes per entry), then the
#     largest input MAX_INPUT accepts under `ulimit -v 500000` (enforced: parsed or
#     refused with an error, exit 0, no abort).
# Nothing here reads or writes /mnt/us/koreader.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

log "building the ink-store test binary for ARM"
TESTBIN=$(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release \
    --target armv7-unknown-linux-musleabihf -p ink-store --lib --tests --message-format=json 2>"$OUT/build-tests.log" \
  | grep -o '"executable":"[^"]*"' | grep -v null | tail -1 | sed 's/^"executable":"//; s/"$//')
[ -n "$TESTBIN" ] && [ -x "$TESTBIN" ] || fail "no ink-store test binary: $(tail -3 "$OUT/build-tests.log")"
G=$DEST/store-verify
guest "rm -rf $G /mnt/us/rinky/tmp/store-verify; mkdir -p $G /mnt/us/rinky/tmp/store-verify" || fail "mkdir $G"
"$SLOT" push "$NAME" "$TESTBIN" "$G/" || fail "push test binary"
guest "mv $G/$(basename "$TESTBIN") $G/ink_store_tests && chmod +x $G/ink_store_tests" || fail "chmod"

log "non-UTF-8 file names"
# printf '\351' is the Latin-1 byte for e-acute, invalid on its own in UTF-8.
guest 'for d in /mnt/us/rinky/tmp/store-verify /tmp; do
  f="$d/caf$(printf "\351").epub"
  if echo x > "$f" 2>/dev/null; then
    echo "$d: created a non-UTF-8 name: $(ls "$d" | grep -c caf) entries; $(ls "$d" | grep caf | od -c | head -1)"
    rm -f "$f"
  else
    echo "$d: refused a non-UTF-8 name"
  fi
done; mount | grep -E " /mnt/us | /tmp "' >"$OUT/non-utf8-names.txt" 2>&1
pass "non-UTF-8 names: $(tr '\n' ';' <"$OUT/non-utf8-names.txt")"

CHECKS="verifier_ store_load_never_misses_a_file_being_renamed stats_open_at_once_on_a_new_file"
log "verifier tests (not ignored) on the user store"
guest "cd $G && INK_VERIFY_RO_DIR=/ TMPDIR=/mnt/us/rinky/tmp/store-verify ./ink_store_tests $CHECKS --test-threads=1 --nocapture" \
  >"$OUT/verifier-userstore.log" 2>&1 || fail "verifier tests on /mnt/us: $(grep -E 'FAILED|panicked' "$OUT/verifier-userstore.log" | head -3)"
pass "verifier tests on /mnt/us: $(grep 'test result' "$OUT/verifier-userstore.log")"
grep -q 'verifier_read_only_file_system_errors_cleanly ... ok' "$OUT/verifier-userstore.log" || fail "read-only file system test did not pass"
pass "read-only mount (/, EROFS): every write errors cleanly"

log "verifier tests (not ignored) on /tmp"
guest "cd $G && TMPDIR=/tmp ./ink_store_tests $CHECKS --test-threads=1 --nocapture" \
  >"$OUT/verifier-tmp.log" 2>&1 || fail "verifier tests on /tmp: $(grep -E 'FAILED|panicked' "$OUT/verifier-tmp.log" | head -3)"
pass "verifier tests on /tmp: $(grep 'test result' "$OUT/verifier-tmp.log")"

for fs in userstore tmp; do
  if [ $fs = userstore ]; then T=/mnt/us/rinky/tmp/store-verify; else T=/tmp; fi
  for run in 1 2 3; do
    guest "cd $G && TMPDIR=$T ./ink_store_tests verify_tests::verifier_write_atomic_target_never_missing_for_a_reader --exact --include-ignored --nocapture" \
      >>"$OUT/rename-raw-$fs.log" 2>&1
    guest "cd $G && TMPDIR=$T ./ink_store_tests verify_tests::store_load_never_misses_a_file_being_renamed verify_tests::verifier_sidecar_two_writers_and_a_reader --test-threads=1 --nocapture" \
      >>"$OUT/rename-store-$fs.log" 2>&1 || fail "store loads during renames on $fs, run $run: $(grep -E 'FAILED|panicked' -A2 "$OUT/rename-store-$fs.log" | head -4)"
    guest "cd $G && TMPDIR=$T ./ink_store_tests verify_tests::probe_rename_exchange_keeps_the_file_visible --exact --include-ignored --nocapture" \
      >>"$OUT/rename-exchange-$fs.log" 2>&1
  done
  log "plain reads during renames, $fs (file-system property): $(grep 'verifier rename:' "$OUT/rename-raw-$fs.log" | sed 's/ at .*//' | tr '\n' ';')"
  pass "store loads during renames, $fs, 3 runs: $(grep 'store rename:' "$OUT/rename-store-$fs.log" | sed 's/ at .*//' | tr '\n' ';')"
  pass "two writers and a reader + store loads, $fs: $(grep -c 'test result: ok' "$OUT/rename-store-$fs.log") of 3 runs ok; $(grep -o 'verifier two writers: .*' "$OUT/rename-store-$fs.log" | tr '\n' ';')"
  log "renameat2(RENAME_EXCHANGE) probe, $fs: $(grep 'probe rename exchange' "$OUT/rename-exchange-$fs.log" | sed 's/ at [^;]*;/;/' | sed 's/ at .*//' | tr '\n' ';')"
done

log "statistics writers on the user store"
for run in 1 2 3; do
  guest "cd $G && TMPDIR=/mnt/us/rinky/tmp/store-verify ./ink_store_tests verifier_stats stats_open_at_once --test-threads=1 --nocapture" \
    >>"$OUT/stats-writers-userstore.log" 2>&1 || fail "stats writers on /mnt/us, run $run: $(grep -E 'FAILED|panicked|locked' "$OUT/stats-writers-userstore.log" | head -3)"
done
pass "statistics writers on /mnt/us, 3 runs: $(grep 'test result' "$OUT/stats-writers-userstore.log" | tr '\n' ';')"

log "Lua literal memory on ARM"
guest "cd $G && INK_VERIFY_LUA_ENTRIES=999999 ./ink_store_tests verify_tests::verifier_lua_near_limit_literal_memory --exact --include-ignored --nocapture" \
  >"$OUT/lua-memory-1m.log" 2>&1 || fail "Lua 999999 entries: $(grep -E 'panicked|verifier lua' "$OUT/lua-memory-1m.log" | head -2)"
pass "Lua 999999 entries: $(grep 'verifier lua' "$OUT/lua-memory-1m.log")"
guest "free -m; cd $G && ulimit -v 500000 && ./ink_store_tests verify_tests::verifier_lua_near_limit_literal_memory --exact --include-ignored --nocapture; echo exit=\$?" \
  >"$OUT/lua-memory-limit.log" 2>&1
grep -q '^exit=0$' "$OUT/lua-memory-limit.log" || fail "Lua at MAX_INPUT under ulimit -v 500000: $(grep -E 'memory allocation|verifier lua|exit=' "$OUT/lua-memory-limit.log" | tr '\n' ';')"
pass "Lua at MAX_INPUT under ulimit -v 500000: $(grep -E 'verifier lua|exit=' "$OUT/lua-memory-limit.log" | tr '\n' ';')"

guest "rm -rf $G /mnt/us/rinky/tmp/store-verify" >"$OUT/cleanup.txt" 2>&1
pass "store verifier scenario done"
