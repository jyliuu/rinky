#!/bin/bash
# Formats: cross-builds the ink-formats test binaries (unit, fixtures, library, the
# adversarial verify_hostile suite) and the
# bookinfo example, copies them and the fixtures to the guest, runs them there with
# INK_FORMATS_TESTDATA, and keeps the logs. Covers parsing on the ARM target (EPUB, FB2,
# FB3, TXT, HTML, MOBI/AZW3, KFX/DRM refusal, read_info) without the UI.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

T=$DEST/formats-test
TD="$E2E_ROOT/testdata"
log "building ARM test binaries"
bins=$(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release \
  --target armv7-unknown-linux-musleabihf -p ink-formats --tests --examples --message-format=json 2>/dev/null |
  python3 -c '
import json, sys
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") == "compiler-artifact" and m.get("executable"):
        print(m["executable"])
')
[ -n "$bins" ] || fail "no test binaries built"
echo "$bins" >"$OUT/binaries.txt"
guest "rm -rf $T; mkdir -p $T/bin $T/data/formats/txt" || fail "mkdir"
# shellcheck disable=SC2086
"$SLOT" push "$NAME" $bins "$T/bin/" >/dev/null || fail "push binaries"
"$SLOT" push "$NAME" "$TD/juliet.epub" "$TD/synthetic.epub" "$T/data/" >/dev/null || fail "push epubs"
fixtures=$(ls "$TD"/formats/*.* | grep -v -e '\.py$' -e '\.md$')
# shellcheck disable=SC2086
"$SLOT" push "$NAME" $fixtures "$T/data/formats/" >/dev/null || fail "push fixtures"
"$SLOT" push "$NAME" "$TD"/formats/txt/* "$T/data/formats/txt/" >/dev/null || fail "push txt fixtures"
guest "uname -a; cat /etc/version.txt 2>/dev/null; ls -la $T/bin" >"$OUT/guest-env.txt" 2>&1
status=0
# Every test binary writes its temporary files to $T/tmp on /mnt/us: the guest /tmp is the
# 64 MB /var tmpfs, which also holds the launcher's 20 MB copy of rinky (ENOSPC and an
# OOM kill of `fixtures` when the unit tests used /tmp). `df` before and after each binary.
guest "mkdir -p $T/tmp" || fail "mkdir tmp"
for b in ink_formats fixtures library; do
  guest "cd $T && df -k /tmp $T; for f in bin/$b-*; do TMPDIR=$T/tmp INK_FORMATS_TESTDATA=$T/data ./\$f --test-threads=2 2>&1; echo \"exit=\$?\"; done; df -k /tmp $T" \
    >"$OUT/guest-test-$b.log" 2>&1
  if grep -q "^test result: ok" "$OUT/guest-test-$b.log" && grep -q "^exit=0" "$OUT/guest-test-$b.log"; then
    log "ok: $b: $(grep '^test result' "$OUT/guest-test-$b.log")"
  else
    log "FAIL: $b (see guest-test-$b.log)"
    status=1
  fi
done
# The adversarial suite (crafted MOBI/KFX/EPUB/FB2/HTML files and the mutation sweep) on
# the 32-bit guest: 8 MiB stacks and 32-bit usize, the conditions the verifier's
# crashes depend on. Crafted files go to $T/tmp (txt_50mb writes 50 MB; /tmp is small).
# --include-ignored: fuzz_mutations, txt_50mb and the zip bombs are ignored on the host as
# slow; here they run as before (run_payload, also ignored, is a no-op without its env).
guest "mkdir -p $T/tmp && cd $T && for f in bin/verify_hostile-*; do TMPDIR=$T/tmp INK_VERIFY_DEADLINE_SCALE=${INK_VERIFY_DEADLINE_SCALE:-30} INK_FORMATS_TESTDATA=$T/data ./\$f --test-threads=1 --nocapture --include-ignored 2>&1; echo \"exit=\$?\"; done; free; df -h $T" \
  >"$OUT/guest-test-verify_hostile.log" 2>&1
# The last fuzz case started (marker written before each case; an abort or a kill leaves
# it naming the case) and the kernel log tail (an OOM kill or a segfault shows there, an
# empty child stderr does not tell them apart). Kept before `rm -rf $T` below.
guest "echo '-- last fuzz case --'; cat $T/tmp/ink-verify-fuzz-case.txt; echo '-- dmesg tail --'; dmesg | tail -n 40" \
  >"$OUT/guest-verify_hostile-after.txt" 2>&1
if grep -q "^test result: ok" "$OUT/guest-test-verify_hostile.log" && grep -q "^exit=0" "$OUT/guest-test-verify_hostile.log"; then
  log "ok: verify_hostile: $(grep '^test result' "$OUT/guest-test-verify_hostile.log")"
else
  log "FAIL: verify_hostile (see guest-test-verify_hostile.log)"
  status=1
fi
# bookinfo on the guest must print what the host build prints for the same files (same
# glob order: the file list is built here and passed to both).
books=$(cd "$TD/formats" && LC_ALL=C ls *.epub *.fb2 *.zip *.fb3 *.mobi *.azw* *.kfx *.html txt/*.txt | tr '\n' ' ')
nbooks=$(($(echo $books | wc -w) + 1))
guest "cd $T/data/formats && $T/bin/bookinfo $books ../juliet.epub; echo \"exit=\$?\"" \
  >"$OUT/guest-bookinfo.log" 2>&1
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo build -q -p ink-formats --example bookinfo) >"$OUT/host-bookinfo-build.log" 2>&1 || fail "host bookinfo build"
# shellcheck disable=SC2086
(cd "$TD/formats" && "$E2E_ROOT/target/debug/examples/bookinfo" $books ../juliet.epub; echo "exit=$?") >"$OUT/host-bookinfo.log" 2>&1
listed=$(grep -c "^== " "$OUT/guest-bookinfo.log")
loaded=$(grep -c "^load: " "$OUT/guest-bookinfo.log")
log "bookinfo listed $listed of $nbooks files, $loaded load lines"
if [ "$listed" != "$nbooks" ] || [ "$loaded" != "$nbooks" ] || ! grep -q "^exit=0" "$OUT/guest-bookinfo.log" || grep -q "panicked" "$OUT/guest-bookinfo.log"; then
  log "FAIL: bookinfo on the guest (see guest-bookinfo.log)"
  status=1
elif ! diff -u "$OUT/host-bookinfo.log" "$OUT/guest-bookinfo.log" >"$OUT/bookinfo-diff.txt"; then
  log "FAIL: bookinfo output differs between host and guest (bookinfo-diff.txt)"
  status=1
else
  log "ok: bookinfo output on the guest equals the host's"
fi
guest "rm -rf $T"
[ $status -eq 0 ] || fail "ink-formats tests failed on the guest"
pass "ink-formats tests pass on the guest"
