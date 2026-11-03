#!/bin/bash
# Verifier attack on scripts/e2e/lib.sh (p0fix verify cycle 1): wait_clean_home and
# wait_home decide on one screendump (the probe) and then save a second screendump as
# the evidence file LABEL.png. When the screen changes between the two, the evidence
# file does not show what was checked (seen on the guest: p0fix-verify-exit-kill R3,
# 06-R3-home.png is a half-drawn screen that emu-screen.py rejects, logged as
# "(clean Home)").
# Fake emulator (scripts/e2e/fake-emu.sh) whose screendump returns the Home reference
# and a torn frame alternately; no real emulator, no network.
#   L1 the file wait_clean_home saves passes `emu-screen.py clean-home`
#   scripts/e2e/verify-lib-evidence.sh     PASS/FAIL lines; exit status = failures
# L1 failed at fb10e9a (verifier finding). lib.sh now copies the probe screendump that
# passed the check (keep_probe); L1 runs always.
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
fails=0
ok() { echo "PASS $*"; }
bad() { echo "FAIL $*"; fails=$((fails + 1)); }
TMP=$(cd "$(mktemp -d "${TMPDIR:-/tmp}/verify-lib.XXXXXX")" && pwd -P)
. "$ROOT/scripts/e2e/fake-emu.sh"
fake_emu_setup "$TMP"
export EMU_SLOT_PORT_BASE=47410
REF="$ROOT/tests/e2e/ref/home-5.19.5.png"
# A torn frame: the reference shifted up by 60 rows (as in the guest screenshot).
python3 -I -c 'import sys
from PIL import Image
im = Image.open(sys.argv[1]); out = Image.new(im.mode, im.size, 240)
out.paste(im.crop((0, 60, im.width, im.height)), (0, 0)); out.save(sys.argv[2])' "$REF" "$TMP/torn.png"
cat >"$EMU/scripts/eink-qmp.py" <<'EOF'
# Fake QMP: screendump alternates between the Home reference and a torn frame.
import os, shutil, sys
name, cmd = sys.argv[2], sys.argv[3]
if cmd == "screendump":
    n_file = os.path.join(os.environ["FAKE_DIR"], "dump-count")
    n = int(open(n_file).read()) if os.path.exists(n_file) else 0
    open(n_file, "w").write(str(n + 1))
    src = os.environ["FAKE_HOME_PNG"] if n % 2 == 0 else os.path.join(os.environ["FAKE_DIR"], "torn.png")
    shutil.copy(src, sys.argv[4])
EOF
"$ROOT/scripts/emu-slot.sh" acquire vl-x >/dev/null
rm -f "$FAKE_DIR/dump-count"
OUTD="$TMP/out"
( NAME=vl-x OUT="$OUTD"
  . "$ROOT/scripts/e2e/lib.sh"
  wait_clean_home 20 checked ) >"$TMP/lib.txt" 2>&1
f=$(ls "$OUTD"/[0-9][0-9]-checked.png 2>/dev/null | head -1)
if [ -z "$f" ]; then
  bad "L1 wait_clean_home saved no file: $(tail -3 "$TMP/lib.txt")"
elif python3 "$ROOT/scripts/emu-screen.py" clean-home "$f" "$REF" >/dev/null; then
  ok "L1 the saved evidence file is the frame that passed the check"
else
  bad "L1 wait_clean_home logged '(clean Home)' but saved $(basename "$f"), which emu-screen.py rejects ($(python3 "$ROOT/scripts/emu-screen.py" clean-home "$f" "$REF"))"
fi
"$ROOT/scripts/emu-slot.sh" release vl-x >/dev/null 2>&1
fake_emu_teardown "$TMP"
rm -rf "$TMP"
echo "verify-lib-evidence: $fails failed"
exit "$fails"
