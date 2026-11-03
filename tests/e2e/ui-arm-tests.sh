#!/bin/bash
# Runs the cross-built ink-text and ink-ui unit tests (and the recorded touch-stream
# tests) on the guest's ARMv7: shaping, fallback faces, rasterizing, hyphenation with the
# embedded dictionaries, Knuth-Plass breaking, gestures from fts_ts/pt_mt recordings,
# widgets and the keyboard. Fixtures go to /tmp/ui-tests; INK_TEST_DATA points there.
# The framework keeps running (no screen or input is used).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

cd "$E2E_ROOT" || fail "cd"
bins=$(CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release --target armv7-unknown-linux-musleabihf \
         -p ink-text -p ink-ui --tests --message-format=json 2>"$OUT/build.log" |
       python3 -I -c '
import json, sys
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    if m.get("reason") == "compiler-artifact" and m.get("profile", {}).get("test") and m.get("executable"):
        print(m["target"]["name"] + " " + m["executable"])
')
[ -n "$bins" ] || fail "no test binaries (see build.log)"
echo "$bins" >"$OUT/binaries.txt"
guest "rm -rf /tmp/ui-tests; mkdir -p /tmp/ui-tests/text-data /tmp/ui-tests/ui-data /tmp/ui-tests/fonts" || fail "mkdir"
"$SLOT" push "$NAME" crates/ink-text/tests/data/InkTestFallback.ttf /tmp/ui-tests/text-data/ >/dev/null || fail "push text data"
"$SLOT" push "$NAME" crates/ink-ui/tests/data/fts_ts-emu-5.19.5.tsv crates/ink-ui/tests/data/pt_mt-pw6-typing.tsv /tmp/ui-tests/ui-data/ >/dev/null || fail "push ui data"
"$SLOT" push "$NAME" fonts/NotoSerif-Regular.ttf fonts/NotoSerif-Bold.ttf fonts/NotoSerif-Italic.ttf /tmp/ui-tests/fonts/ >/dev/null || fail "push fonts"
status=0
while read -r name path; do
  # ssh reads stdin: keep it away from the loop's input.
  "$SLOT" push "$NAME" "$path" /tmp/ui-tests/ >/dev/null </dev/null || fail "push $name"
  base=$(basename "$path")
  data=/tmp/ui-tests/ui-data
  # ink-text's test binaries (unit tests and tests/verify_text_attacks.rs) use its fixtures.
  case "$name" in ink_text | verify_text_*) data=/tmp/ui-tests/text-data ;; esac
  log "running $name on the guest"
  if guest "cd /tmp/ui-tests && INK_TEST_DATA=$data INK_REPO_FONTS=/tmp/ui-tests/fonts ./$base --test-threads=1" >"$OUT/$name.log" 2>&1 </dev/null; then
    log "$name: $(grep 'test result' "$OUT/$name.log")"
  else
    log "$name FAILED: $(grep -E 'test result|FAILED|panicked' "$OUT/$name.log" | head -5)"
    status=1
  fi
done <<<"$bins"
guest "uname -m; cat /etc/version.txt" >"$OUT/guest.txt" 2>&1
[ "$status" = 0 ] || fail "test binaries failed on the guest"
pass "ink-text and ink-ui tests on ARMv7"
