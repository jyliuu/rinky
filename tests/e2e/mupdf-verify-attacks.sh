#!/bin/bash
# Verifier attacks (crates/ink-mupdf/tests/verify_attacks.rs), cross-built for ARM and run
# in the guest (framework running; nothing touches the screen).
#  - default.log: the default (non-ignored) attack tests; they must pass.
#  - findings/<test>.log and findings.txt: each cycle-1 finding test run one by one (fixed:
#    they are default tests now, so default.log already requires them to pass) and the
#    open minor finding cbz_huge_image (ignored test); "FAILED" there means the finding
#    reproduces on ARM. Minor findings do not fail the scenario.
#  - measure/<test>.log: timings and peak memory on ARM (2000-page PDF, large scan pages,
#    memory over 48 pages, round trips with the slowest page_of per mode).
# The large PNG pages (6000 x 9000 RGB, 16384 x 16384) run last: they may wake the guest's
# OOM killer. Fixtures go to $T/tmp on /mnt/us (TMPDIR), not to the tmpfs /tmp, so they do
# not take RAM.
#   tests/e2e/mupdf-verify-attacks.sh INSTANCE OUTDIR
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

TARGET=armv7-unknown-linux-musleabihf
BIN="$OUT/arm-bins.txt"
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release --target $TARGET -p ink-mupdf \
    --test verify_attacks --message-format=json 2>/dev/null) | python3 -c '
import sys, json
for line in sys.stdin:
    try:
        m = json.loads(line)
    except Exception:
        continue
    if m.get("reason") == "compiler-artifact" and m.get("executable"):
        print(m["target"]["name"], m["executable"])
' >"$BIN" || fail "cross build"
EXE=$(awk '$1 == "verify_attacks" { print $2 }' "$BIN")
[ -x "$EXE" ] || fail "no ARM executable for verify_attacks"
T=$DEST/mupdf-verify
guest "rm -rf $T /tmp/ink-mupdf-verify-*; mkdir -p $T/tmp"
"$SLOT" push "$NAME" "$EXE" "$T/" >/dev/null || fail "push binary"
guest "cd $T && mv verify_attacks-* verify_attacks && chmod +x verify_attacks && ls -la && free && cat /etc/version.txt" \
  >"$OUT/guest-files.txt" 2>&1
run() { # LOG ARGS...: run the test binary in the guest (TMPDIR=$T/tmp, emptied after)
  local log=$1
  shift
  guest "cd $T && TMPDIR=$T/tmp ./verify_attacks $* 2>&1; echo exit=\$?; rm -rf $T/tmp/*" >"$log" 2>&1
}

log "default attack tests"
run "$OUT/default.log" --test-threads=1 --skip cbz_large_scan
grep "test result" "$OUT/default.log" | tee -a "$OUT/scenario.log"
grep -q "exit=0" "$OUT/default.log" || fail "default attack tests failed on ARM (default.log)"
pass "default attack tests on ARM"

# Ignored on the host as slow (#[ignore = "slow: ..."]), still pass/fail here.
t=byte_flip_fuzz_does_not_crash
run "$OUT/$t.log" --exact "$t" --ignored --test-threads=1
grep "test result" "$OUT/$t.log" | tee -a "$OUT/scenario.log"
{ grep -q "exit=0" "$OUT/$t.log" && grep -q "1 passed" "$OUT/$t.log"; } || fail "$t failed on ARM ($t.log)"
pass "$t on ARM"

mkdir -p "$OUT/findings" "$OUT/measure"
# verdict TEST LOG: from the exit status (101 = test failed: the finding reproduces) and
# the count (a name that selects no test is "no result", not "ok").
verdict() {
  local code
  code=$(grep -o 'exit=[0-9]*' "$2" | tail -1)
  case "$code" in
    exit=0) if grep -q "1 passed" "$2"; then echo "$1: ok (finding does not reproduce)"; else echo "$1: no result (no test ran)"; fi ;;
    exit=101) echo "$1: FAILED (finding reproduces)" ;;
    *) echo "$1: no result ($code)" ;;
  esac | tee -a "$OUT/findings.txt"
}
: >"$OUT/findings.txt"
# Cycle-1 findings (fixed, default tests), cheapest first. Not run on ARM: the two
# 100000-deep nesting tests (stack overflow inside MuPDF, shown on the host).
for t in cbz_entries_named_dot_slash_are_pages cbz_entries_in_a_folder_starting_with_a_dot_are_pages \
    search_skips_a_damaged_page option_changes_keep_the_current_tile \
    column_regions_are_not_slivers_and_tile_counts_stay_readable extreme_free_zoom_and_pan_return \
    hostile_stored_quads_do_not_panic screen_count_fits_32_bits_for_a_2000_page_tall_document \
    tall_mediaboxes_keep_tile_counts_sane; do
  run "$OUT/findings/$t.log" --exact "$t" --nocapture --test-threads=1
  verdict "$t" "$OUT/findings/$t.log"
done

for t in a_2000_page_pdf_opens_searches_and_lists_its_toc_in_time memory_levels_off_over_many_cbz_pages; do
  # Both are #[ignore = "slow: ..."] on the host; they ran in the default set here before.
  run "$OUT/measure/$t.log" --exact "$t" --ignored --nocapture --test-threads=1
  { grep -q "exit=0" "$OUT/measure/$t.log" && grep -q "1 passed" "$OUT/measure/$t.log"; } || fail "$t failed on ARM (measure/$t.log)"
  grep -E "^(open|render|toc|search|set_options|file|large scan|many pages)" "$OUT/measure/$t.log" | tee -a "$OUT/scenario.log"
done
run "$OUT/measure/every_screen_round_trips_in_every_mode.log" --exact every_screen_round_trips_in_every_mode --nocapture --test-threads=1
verdict every_screen_round_trips_in_every_mode "$OUT/measure/every_screen_round_trips_in_every_mode.log"
grep -E "mismatches [1-9]|round-trip mismatches" "$OUT/measure/every_screen_round_trips_in_every_mode.log" | tee -a "$OUT/scenario.log"

t=cbz_large_scan_page_memory_is_measured
run "$OUT/measure/$t.log" --exact "$t" --ignored --nocapture --test-threads=1
grep -E "^large scan|status|Killed|signal" "$OUT/measure/$t.log" | tee -a "$OUT/scenario.log"
t=cbz_huge_image_stays_within_the_memory_caps
run "$OUT/findings/$t.log" --exact "$t" --ignored --nocapture --test-threads=1
verdict "$t" "$OUT/findings/$t.log"
grep -E "huge image|status|Killed|signal" "$OUT/findings/$t.log" | head -5 | tee -a "$OUT/scenario.log"
guest "dmesg | grep -i -E 'oom|killed process' | tail -5" >"$OUT/dmesg-oom.txt" 2>&1 || true

guest "rm -rf $T /tmp/ink-mupdf-verify-*"
pass "findings and measurements saved (findings.txt, measure/)"
