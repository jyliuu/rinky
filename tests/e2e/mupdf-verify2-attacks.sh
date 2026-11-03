#!/bin/bash
# Verifier cycle 2 for ink-mupdf on ARM (crates/ink-mupdf/tests/verify_attacks_2.rs), and
# the column-detection hang through the real reader in run mode.
#  - tests/<test>.log, findings.txt: each test alone (--exact --include-ignored)
#    under a guest-side `timeout 300`. "FAILED" = the finding reproduces on ARM. Findings do
#    not fail the scenario; the default tests must pass.
#  - reader: a 2-page PDF (page 1 text, page 2 blank except two specks) opened with the
#    launcher. Control without options: opens, shows page 1, exits to Home. With
#    RINKY_FIXED=columns=2: rinky takes over the screen, then stays in the open call
#    (log, CPU time over 10 s, ignored exit tap); kill restores Home.
# Taps: the reader is started right after other scenarios. A tap at exactly the
# coordinates of the last touch before rinky started arrives as (0, 0) (the kernel drops
# unchanged ABS values and the reader starts without the current slot position), so every
# exit tap here follows a page-turn tap.
#   tests/e2e/mupdf-verify2-attacks.sh INSTANCE OUTDIR      (after scripts/emu-slot.sh deploy)
#   MUPDF_VERIFY2_PARTS="tests reader" (default both) selects the parts.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
PARTS=${MUPDF_VERIFY2_PARTS:-tests reader}

if [[ " $PARTS " == *" tests "* ]]; then
TARGET=armv7-unknown-linux-musleabihf
BIN="$OUT/arm-bins.txt"
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} cargo zigbuild --release --target $TARGET -p ink-mupdf \
    --test verify_attacks_2 --message-format=json 2>/dev/null) | python3 -c '
import sys, json
for line in sys.stdin:
    try:
        m = json.loads(line)
    except Exception:
        continue
    if m.get("reason") == "compiler-artifact" and m.get("executable"):
        print(m["target"]["name"], m["executable"])
' >"$BIN" || fail "cross build"
EXE=$(awk '$1 == "verify_attacks_2" { print $2 }' "$BIN")
[ -x "$EXE" ] || fail "no ARM executable for verify_attacks_2"
T=$DEST/mupdf-verify2
guest "rm -rf $T; mkdir -p $T/tmp"
"$SLOT" push "$NAME" "$EXE" "$T/" >/dev/null || fail "push binary"
guest "cd $T && mv verify_attacks_2-* verify_attacks_2 && chmod +x verify_attacks_2 && ls -la && uname -m" \
  >"$OUT/guest-files.txt" 2>&1

run() { # LOG ARGS...: the test binary in the guest, at most 300 s, fixtures on /mnt/us
  local log=$1
  shift
  guest "cd $T && TMPDIR=$T/tmp timeout 300 ./verify_attacks_2 $* 2>&1; echo exit=\$?; rm -rf $T/tmp/*" >"$log" 2>&1
}
verdict() { # TEST LOG
  local code
  code=$(grep -o 'exit=[0-9]*' "$2" | tail -1)
  case "$code" in
    exit=0) if grep -q "1 passed" "$2"; then echo "$1: ok"; else echo "$1: no result (no test ran)"; fi ;;
    exit=101) echo "$1: FAILED (finding reproduces)" ;;
    exit=143) echo "$1: FAILED (killed by timeout 300)" ;;
    *) echo "$1: no result ($code)" ;;
  esac | tee -a "$OUT/findings.txt"
}

mkdir -p "$OUT/tests"
: >"$OUT/findings.txt"
log "default tests of verify_attacks_2"
run "$OUT/tests/default.log" --test-threads=1
grep "test result" "$OUT/tests/default.log" | tee -a "$OUT/scenario.log"
grep -q "exit=0" "$OUT/tests/default.log" || fail "default verify_attacks_2 tests failed on ARM"
pass "default verify_attacks_2 tests on ARM"
# One finding per process: a hung helper thread spins until its process exits.
for t in column_cuts_returns_for_a_narrow_sparse_ink_box column_cuts_returns_for_a_narrow_blank_image \
    columns_mode_opens_a_cbz_page_with_two_small_marks columns_mode_opens_a_pdf_page_with_two_specks \
    columns_mode_turns_to_a_later_page_with_two_specks columns_mode_opens_a_blank_tall_narrow_page \
    toc_and_link_targets_past_the_last_page_stay_in_range public_helpers_take_huge_indices; do
  run "$OUT/tests/$t.log" --exact "$t" --include-ignored --nocapture --test-threads=1
  verdict "$t" "$OUT/tests/$t.log"
done
guest "rm -rf $T"
fi
[[ " $PARTS " == *" reader "* ]] || { pass "tests part done (findings.txt)"; exit 0; }

# --- The reader: 2-page PDF, page 2 = two specks at x 264 pt, 300 analysis px apart.
FIX="$OUT/fixtures"
mkdir -p "$FIX"
python3 - "$FIX/specks.pdf" <<'EOF'
import sys
page1 = b"BT /F1 24 Tf 72 700 Td (Verifier page 1: text) Tj ET\n0 g 72 600 300 40 re f\n"
page2 = b"0 g 264 658.68 5.28 1.32 re f\n264 262.68 5.28 1.32 re f\n"
def stream(c):
    return b"<< /Length %d >>\nstream\n" % len(c) + c + b"\nendstream"
objs = [
    b"<< /Type /Catalog /Pages 2 0 R >>",
    b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>",
    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 4 0 R >>",
    stream(page1),
    b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>",
    stream(page2),
    b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
]
out = b"%PDF-1.4\n"
offs = []
for i, o in enumerate(objs):
    offs.append(len(out))
    out += b"%d 0 obj\n" % (i + 1) + o + b"\nendobj\n"
x = len(out)
out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
for o in offs:
    out += b"%010d 00000 n \n" % o
out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, x)
open(sys.argv[1], "wb").write(out)
EOF
"$SLOT" push "$NAME" "$FIX/specks.pdf" "$DEST/" >/dev/null || fail "push specks.pdf"

launch() { # [RINKY_FIXED]
  guest "rm -rf $DEST/specks.sdr $DEST/rinky.log $DEST/watchdog.log; RINKY_FIXED='${1:-}' sh $DEST/rinky.sh $DEST/specks.pdf" ||
    fail "launch specks.pdf '${1:-}'"
  wait_until 90 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over ('${1:-}')"
}
cpu_ticks() { guest "awk '{ print \$14 + \$15 }' /proc/$1/stat" 2>/dev/null; }

wait_home 240 home-before

# Control: no options.
launch ""
sleep 6
f=$(shot reader-control-page1) || exit 1
is_home "$f" && fail "control: still Home"
guest "cat $DEST/rinky.log" >"$OUT/reader-control.log"
grep -q "mupdf 1.27.2: Pdf 2 pages" "$OUT/reader-control.log" || fail "control: provider log line missing"
pass "control: specks.pdf opens without columns"
tap 1000 900
sleep 4
f=$(shot reader-control-page2) || exit 1
exit_device
wait_clean_home 300 home-after-control

# columns=2: the plan analyses page 2 (the specks) at open.
launch "columns=2"
pid=$(rinky_pid)
log "rinky pid $pid with RINKY_FIXED=columns=2"
sleep 50
a=$(cpu_ticks "$pid")
sleep 10
b=$(cpu_ticks "$pid")
log "rinky CPU ticks (1/100 s) over 10 s: $a -> $b"
echo "cpu ticks over 10 s: $a -> $b" >"$OUT/reader-columns2-cpu.txt"
f=$(shot reader-columns2-after-60s) || exit 1
guest "cat $DEST/rinky.log" >"$OUT/reader-columns2.log"
guest "top -b -n 1 | head -15" >"$OUT/reader-columns2-top.txt" 2>&1 || true
tap 1000 900
sleep 4
tap 1205 65
sleep 15
f=$(shot reader-columns2-after-exit-tap) || exit 1
if [ -n "$(rinky_pid)" ]; then
  log "FINDING: rinky still runs 15 s after the exit tap (pid $(rinky_pid)); open() never returned"
  echo "reader columns=2: FAILED (open hangs: still running after 75 s and the exit tap)" | tee -a "$OUT/findings.txt"
  grep -q "mupdf 1.27.2: Pdf 2 pages" "$OUT/reader-columns2.log" &&
    log "note: the provider log line is present (open returned before the hang)"
  guest "kill $pid; sleep 5; kill -0 $pid 2>/dev/null && kill -9 $pid; true"
else
  echo "reader columns=2: ok (reader exited)" | tee -a "$OUT/findings.txt"
fi
wait_until 30 "reader gone" reader_gone || fail "rinky survived kill -9"
wait_clean_home 300 home-after-columns2
save_logs
guest "rm -rf $DEST/specks.pdf $DEST/specks.sdr"
pass "verifier cycle 2 on ARM done (findings.txt)"
