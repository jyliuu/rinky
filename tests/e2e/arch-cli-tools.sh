#!/bin/bash
# e2e-cycles: 1
# The screen tools of the rinky binary on the guest, with the framework stopped by
# hand (as the launcher does: stop lab126_gui, wait for KPPMainApp/pillowd to go; at the
# end restart appmgrd, wait until it answers on lipc, start lab126_gui):
#   1. selftest --screen --lipc --book juliet.epub: 0 failures; the gray-step pattern is
#      on the screen (16 steps, black left, white right);
#   2. refresh: a black band written straight into /dev/fb0 (no update ioctl) - the
#      screenshots before and after `rinky refresh` record whether the emulator
#      shows framebuffer memory before an update; refresh must exit 0 and the band must
#      be visible after it;
#   3. screensaver IMAGE.png: an 800x600 test image appears centred; an oversized
#      image (1600x2000) is clipped without an error; a non-PNG file is an error;
#   4. Home comes back clean.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
set -o pipefail

fw_stopped() { [ -z "$(guest 'pidof KPPMainApp; pidof pillowd' 2>/dev/null)" ]; }
appmgrd_answers() { guest 'lipc-get-prop com.lab126.appmgrd activeApp >/dev/null 2>&1'; }
restore_framework() {
  guest 'status lab126_gui | grep -q start/ && exit 0; stop appmgrd; start appmgrd' >>"$OUT/scenario.log" 2>&1
  wait_until 60 "appmgrd answers on lipc" appmgrd_answers || log "appmgrd did not answer within 60 s"
  guest 'sleep 2; status lab126_gui | grep -q start/ || (cd / && start lab126_gui)' >>"$OUT/scenario.log" 2>&1
}
# On any failure after the stop: bring the framework back before exiting.
cleanup_fail() { log "FAIL: $*"; restore_framework; save_logs; exit 1; }

# py CODE ARGS...: Pillow checks on screenshots (exit status = result).
py() { python3 -I -c "$@"; }

wait_home 240 home-before
guest "ls -la $DEST/rinky $DEST/juliet.epub" >"$OUT/deployed.txt" 2>&1 || fail "rinky not deployed"

log "stop lab126_gui"
guest 'stop lab126_gui' >>"$OUT/scenario.log" 2>&1
wait_until 60 "KPPMainApp and pillowd gone" fw_stopped || cleanup_fail "framework did not stop"
guest 'status lab126_gui' | tee -a "$OUT/scenario.log"

# 1. selftest --screen --lipc --book
guest "cd $DEST && ./rinky selftest --screen --lipc --book $DEST/juliet.epub" >"$OUT/selftest.txt" 2>&1
rc=$?
log "selftest exit $rc: $(tail -1 "$OUT/selftest.txt")"
nap 3
f=$(shot selftest-screen) || cleanup_fail "screendump"
[ "$rc" = 0 ] || cleanup_fail "selftest reported $rc failure(s) (selftest.txt)"
grep -q "^PASS device.screen" "$OUT/selftest.txt" || cleanup_fail "device.screen did not pass"
grep -q "^PASS lipc.set_int" "$OUT/selftest.txt" || cleanup_fail "lipc.set_int did not pass"
py 'import sys
from PIL import Image
im = Image.open(sys.argv[1]).convert("L"); w, h = im.size; step = w // 16; y = h * 2 // 3 + 100
vals = [im.getpixel((i * step + step // 2, y)) for i in range(16)]
print("gray steps on screen:", vals)
sys.exit(0 if vals[0] < 32 and vals[-1] > 220 and all(b >= a for a, b in zip(vals, vals[1:])) else 1)' "$f" \
  | tee "$OUT/selftest-steps.txt" || cleanup_fail "the 16-step pattern is not on the screen ($f)"
pass "selftest --screen --lipc: 0 failures, pattern on the screen"

# 2. refresh
geom=$(guest 'cat /sys/class/graphics/fb0/virtual_size /sys/class/graphics/fb0/stride /sys/class/graphics/fb0/pan 2>/dev/null' | tr '\n' ' ')
log "fb0 virtual_size stride pan: $geom"
yoff=$(echo "$geom" | awk '{ split($3, p, ","); print p[2] + 0 }')
stride=$(echo "$geom" | awk '{ print $2 + 0 }')
[ "${stride:-0}" -gt 0 ] || stride=1272
guest "dd if=/dev/zero of=/dev/fb0 bs=$stride seek=$((yoff + 100)) count=200 2>&1" | tee -a "$OUT/scenario.log"
nap 2
before=$(shot fb-written-no-ioctl) || cleanup_fail "screendump"
guest "$DEST/rinky refresh" >"$OUT/refresh.txt" 2>&1
rc=$?
echo "exit=$rc" >>"$OUT/refresh.txt"
nap 3
after=$(shot after-refresh) || cleanup_fail "screendump"
[ "$rc" = 0 ] || cleanup_fail "rinky refresh exit $rc"
band() { python3 "$SCREEN" dark "$1" 0 120 1272 160; }
b0=$(band "$before"); b1=$(band "$after")
log "dark fraction of rows 120-279: before refresh $b0, after refresh $b1"
echo "band dark fraction before refresh: $b0  after refresh: $b1" >"$OUT/refresh-band.txt"
awk -v a="$b1" 'BEGIN { exit !(a > 0.95) }' || cleanup_fail "the band is not on the screen after refresh"
if awk -v b="$b0" 'BEGIN { exit !(b < 0.5) }'; then
  pass "refresh made the framebuffer write visible (dark $b0 -> $b1)"
else
  log "the emulator showed the raw framebuffer write before any update ioctl (dark $b0); refresh exit 0, band still shown ($b1)"
  pass "refresh exit 0; screen unchanged (emulator displays framebuffer memory directly)"
fi

# 3. screensaver
py 'import sys
from PIL import Image, ImageDraw
im = Image.new("L", (800, 600), 255); d = ImageDraw.Draw(im)
d.rectangle((0, 0, 799, 599), outline=0, width=12)
for i in range(8): d.rectangle((60 + i * 85, 380, 120 + i * 85, 560), fill=i * 36)
d.rectangle((60, 60, 740, 300), fill=0); d.rectangle((120, 120, 680, 240), fill=255)
im.save(sys.argv[1])
big = Image.new("L", (1600, 2000), 0); ImageDraw.Draw(big).rectangle((400, 400, 1200, 1600), fill=255); big.save(sys.argv[2])' \
  "$OUT/screensaver-test.png" "$OUT/screensaver-oversized.png"
printf 'not a png\n' >"$OUT/not-a-png.png"
"$SLOT" push "$NAME" "$OUT/screensaver-test.png" "$OUT/screensaver-oversized.png" "$OUT/not-a-png.png" /tmp/ >/dev/null || cleanup_fail "push"
guest "$DEST/rinky screensaver /tmp/screensaver-test.png" >"$OUT/screensaver.txt" 2>&1
rc=$?
echo "exit=$rc" >>"$OUT/screensaver.txt"
nap 3
f=$(shot screensaver) || cleanup_fail "screendump"
[ "$rc" = 0 ] || cleanup_fail "screensaver exit $rc"
py 'import sys
from PIL import Image, ImageChops
shot = Image.open(sys.argv[1]).convert("L"); ref = Image.open(sys.argv[2]).convert("L")
x, y = (shot.width - ref.width) // 2, (shot.height - ref.height) // 2
crop = shot.crop((x, y, x + ref.width, y + ref.height))
# The emulated panel shows 16 levels (0, 16, ... 240; white reads back as 240): compare
# with the reference quantized the same way.
refq = ref.point(lambda v: v // 16 * 16)
diff = ImageChops.difference(crop, refq); hist = diff.histogram()
mean = sum(i * n for i, n in enumerate(hist)) / (ref.width * ref.height)
outside = Image.open(sys.argv[1]).convert("L").crop((0, 0, shot.width, y - 10)).getextrema()
print(f"centred at {x},{y}: mean abs diff to the 16-level reference {mean:.2f}; area above the image min/max {outside}")
sys.exit(0 if mean < 3 and outside[0] > 200 else 1)' "$f" "$OUT/screensaver-test.png" \
  | tee "$OUT/screensaver-compare.txt" || cleanup_fail "screensaver image not shown centred ($f)"
pass "screensaver shows the test image centred"

guest "$DEST/rinky screensaver /tmp/screensaver-oversized.png" >"$OUT/screensaver-oversized.txt" 2>&1
rc=$?
echo "exit=$rc" >>"$OUT/screensaver-oversized.txt"
nap 3
f=$(shot screensaver-oversized) || cleanup_fail "screendump"
[ "$rc" = 0 ] || cleanup_fail "oversized screensaver exit $rc"
py 'import sys
from PIL import Image
im = Image.open(sys.argv[1]).convert("L"); w, h = im.size
# 1600x2000 centred on 1272x1696: offset (-164, -152); the white box 400..1200 x 400..1600
# lands at 236..1036 x 248..1448, the rest is black.
pts = {"black corner": im.getpixel((20, 20)), "white box": im.getpixel((636, 848)), "black right": im.getpixel((1200, 848))}
print(pts); sys.exit(0 if pts["black corner"] < 40 and pts["white box"] > 215 and pts["black right"] < 40 else 1)' "$f" \
  | tee "$OUT/screensaver-oversized-check.txt" || cleanup_fail "oversized image not clipped as expected ($f)"
pass "oversized screensaver image clipped"

guest "$DEST/rinky screensaver /tmp/not-a-png.png" >"$OUT/screensaver-not-png.txt" 2>&1
rc=$?
echo "exit=$rc" >>"$OUT/screensaver-not-png.txt"
[ "$rc" = 1 ] && grep -q "^rinky: " "$OUT/screensaver-not-png.txt" || cleanup_fail "non-PNG screensaver: exit $rc"
pass "non-PNG screensaver is an error (exit 1)"
guest 'rm -f /tmp/screensaver-test.png /tmp/screensaver-oversized.png /tmp/not-a-png.png'

# 4. framework back
log "restore the framework (appmgrd restart, wait for lipc, start lab126_gui)"
restore_framework
wait_clean_home 300 home-after
save_logs
pass "refresh, selftest --screen and screensaver on the guest"
