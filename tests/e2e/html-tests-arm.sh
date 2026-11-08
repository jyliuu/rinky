#!/bin/bash
# Phase 2 engine tests on the emulator: the ink-html and ink-doc test binaries (unit
# tests, css, document, xpointer, golden) cross-built for armv7 musl and run on the guest.
# 1. cargo zigbuild --release --tests for ink-html and ink-doc (reuses the build cache);
#    the binary paths come from cargo's JSON messages;
# 2. the binaries plus what the tests read (feature fixtures, crengine expected
#    xpointers, testdata/*.epub, tests/golden/*.png, fonts/) are copied
#    to /mnt/us/htmltest;
# 3. before that, the emulator guard (crates/ink-html/examples/emuguard.rs) takes
#    physical page 0x40000000 out of circulation on the guest (see below); it stays
#    running after the tests (/var/tmp/emuguard, log /var/tmp/emuguard.log, copied to
#    OUT/emuguard.log);
# 4. each binary runs with INK_MANIFEST_DIR pointing at its crate directory there; one
#    log per attempt in OUT (test-<crate>-<name>-attemptN.log). The golden test compares
#    all golden pages pixel by pixel on ARM. Every binary gets up to 3 attempts (see the
#    loop below).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
GDIR=/mnt/us/htmltest
STAGE=$(mktemp -d)
GUARD=/var/tmp/emuguard
trap 'rm -rf "$STAGE"' EXIT

(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release \
   --target armv7-unknown-linux-musleabihf -p ink-html -p ink-doc --tests --example emuguard \
   --message-format=json >"$STAGE/build.json" 2>"$OUT/build.log") || fail "cross build of the test binaries (see build.log)"
# "crate name path" per test executable.
python3 -I - "$STAGE/build.json" >"$STAGE/bins.txt" <<'EOF' || fail "no test binaries in the build output"
import json, sys
n = 0
for line in open(sys.argv[1]):
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == "emuguard":
        open(sys.argv[1] + ".emuguard", "w").write(m["executable"])
    elif m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"].get("test"):
        crate = "ink-doc" if m["target"]["name"] == "ink_doc" or "/ink-doc" in m["manifest_path"] else "ink-html"
        print(crate, m["target"]["name"], m["executable"])
        n += 1
sys.exit(0 if n else 1)
EOF
cp "$STAGE/bins.txt" "$OUT/binaries.txt"
log "test binaries: $(awk '{print $1 "/" $2}' "$STAGE/bins.txt" | tr '\n' ' ')"

mkdir -p "$STAGE/t/bin" "$STAGE/t/crates/ink-html/tests" "$STAGE/t/crates/ink-doc" "$STAGE/t/tests"
while read -r crate name path; do cp "$path" "$STAGE/t/bin/$crate-$name"; done <"$STAGE/bins.txt"
cp "$(cat "$STAGE/build.json.emuguard")" "$STAGE/t/bin/emuguard" || fail "no emuguard in the build output"
cp -R "$E2E_ROOT/crates/ink-html/tests/fixtures" "$E2E_ROOT/crates/ink-html/tests/crengine" "$STAGE/t/crates/ink-html/tests/"
cp -R "$E2E_ROOT/testdata" "$E2E_ROOT/fonts" "$STAGE/t/"
cp -R "$E2E_ROOT/tests/golden" "$STAGE/t/tests/"
rm -rf "$STAGE/t/crates/ink-html/tests/fixtures/src" "$STAGE/t/testdata/"*.sdr
(cd "$STAGE/t" && COPYFILE_DISABLE=1 tar -cf "$STAGE/htmltest.tar" .) || fail "tar"

# The emulator (eink-emulator QEMU, machine mt8113-bellatrix4) maps a 32-byte MMIO "CFA
# mailbox" over guest RAM at physical 0x400000d0..0x400000ef, while the guest kernel hands
# physical page 0x40000000 to processes as ordinary RAM: whichever test gets that page
# reads zeros or mailbox values in those 32 bytes (wrong golden pixels, corrupt PNG filter
# bytes, garbage indices, segfaults). The real device has no such mailbox. emuguard (root)
# catches that page and holds it. It starts before the
# test files are unpacked (their page cache could take the page) and is left running,
# so later runs and other scenarios on this guest are covered too.
guard_state() {
  case "$(guest "tail -1 $GUARD.log" </dev/null 2>/dev/null)" in
    "emuguard: holding") echo "emuguard holds physical page 0x40000000" ;;
    *) echo "emuguard does not hold physical page 0x40000000 yet: $(guest "tail -1 $GUARD.log" </dev/null 2>/dev/null)" ;;
  esac
}
if ! guest "pidof emuguard" </dev/null >/dev/null 2>&1; then
  "$SLOT" push "$NAME" "$STAGE/t/bin/emuguard" /var/tmp/ >/dev/null || fail "push emuguard"
  guest "chmod +x $GUARD && nohup $GUARD >$GUARD.log 2>&1 </dev/null &" </dev/null || fail "start emuguard"
fi
for _ in $(seq 1 40); do
  guard=$(guard_state)
  case "$guard" in "emuguard holds"*) break ;; esac
  sleep 3
done
log "$guard"
guard_start=$guard
guest "grep '^emuguard: caught' $GUARD.log" </dev/null 2>/dev/null | sed 's/^/  /' | tee -a "$OUT/scenario.log" >&2

"$SLOT" push "$NAME" "$STAGE/htmltest.tar" /mnt/us/ >/dev/null || fail "push"
guest "rm -rf $GDIR && mkdir -p $GDIR && cd $GDIR && tar -xf /mnt/us/htmltest.tar && rm /mnt/us/htmltest.tar && chmod +x bin/* && ls bin" || fail "unpack on the guest"

# run_bin CRATE NAME LOG: runs one test binary on the guest; true when it passed.
# --include-ignored: the tests ignored on the host as slow (#[ignore = "slow: ..."]) run
# here as before, in the release build. malformed_vs_crengine is ignored for another
# reason (informational, fails by design) and stays skipped.
run_bin() {
  # stdin from /dev/null: ssh would otherwise read the rest of the binary list.
  guest "cd $GDIR && rm -rf target && INK_MANIFEST_DIR=$GDIR/crates/$1 ./bin/$1-$2 --test-threads=2 --include-ignored --skip malformed_vs_crengine 2>&1; echo exit=\$?" >"$3" 2>&1 </dev/null
  grep -q '^exit=0$' "$3" && grep -q '^test result: ok' "$3"
}

# Every binary is retried (at most 3 attempts) in case the guard is not active (the
# mailbox page then corrupts any test binary that gets it). With the guard holding the
# page, a retry is a new failure to look at. Every attempt keeps its log (and the golden
# actual/diff images); the PASS and FAIL lines name every retry and the guard state.
failed=0
ran=0
notes=""
while read -r crate name _; do
  ran=$((ran + 1))
  start=$(date +%s)
  attempts=3
  ok=false
  for a in $(seq 1 "$attempts"); do
    f="$OUT/test-$crate-$name-attempt$a.log"
    if run_bin "$crate" "$name" "$f"; then
      ok=true
      [ "$a" -gt 1 ] && notes="$notes $name clean on attempt $a/$attempts;"
      break
    fi
    n=$(grep -c 'pixels differ' "$f")
    log "attempt $a: $crate $name failed: $(grep -E '^test result:' "$f" | tail -1) ($n golden pages differ)"
    notes="$notes $name attempt $a failed ($n golden pages differ);"
    if guest "test -d $GDIR/target/golden-out" </dev/null 2>/dev/null; then
      mkdir -p "$OUT/golden-out-attempt$a"
      guest "cd $GDIR/target/golden-out && tar -cf - ." </dev/null | tar -xf - -C "$OUT/golden-out-attempt$a" &&
        log "golden actual/diff images in $OUT/golden-out-attempt$a"
    fi
  done
  summary=$(grep -E '^test result:' "$f" | tail -1)
  if $ok; then
    log "ok: $crate $name in $(( $(date +%s) - start )) s: $summary"
  else
    log "FAILED: $crate $name: ${summary:-no result line} (see $(basename "$f"))"
    failed=$((failed + 1))
  fi
done <"$STAGE/bins.txt"
guest "cat $GUARD.log" >"$OUT/emuguard.log" 2>&1 </dev/null || true
guard=$(guard_state)
[ "$guard" = "$guard_start" ] || guard="at the start: $guard_start; at the end: $guard"
guest "rm -rf $GDIR" || true
want=$(wc -l <"$STAGE/bins.txt" | tr -d ' ')
[ "$ran" = "$want" ] || fail "ran $ran of $want test binaries"
[ "$failed" = 0 ] || fail "$failed test binaries failed on ARM ($guard);$notes"
[ -n "$notes" ] && log "retries:$notes"
pass "ink-html and ink-doc tests pass on the ARM build ($ran binaries; $guard)${notes:+; retries:$notes}"
