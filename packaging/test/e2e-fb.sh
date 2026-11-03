#!/bin/bash
# Host-side helpers for tests/e2e/packaging-*.sh, sourced after scripts/e2e/lib.sh.
#
# kual.sh prints its messages with FBInk at the bottom of the screen. On a real Kindle
# that is KUAL's screen; in the emulator KUAL is not installed, the actions run over SSH
# while Home is shown, and the stock framework never repaints that band. The text would
# stay on Home, and every later scenario's clean-Home check (scripts/e2e/lib.sh
# wait_home) would fail. So around a KUAL action that prints over Home:
#
#   fb_save LABEL           screenshot LABEL.png, then copy framebuffer rows
#                           FB_TOP..FB_TOP+FB_ROWS-1 of the visible buffer to
#                           $T/fb-band.raw in the guest (dd; one row = 1272 bytes)
#   ... KUAL action, screenshot of its message ...
#   fb_restore LABEL        write the rows back, refresh that rectangle with FBInk, and
#                           wait until the screen equals LABEL.png again below the status
#                           bar (scripts/emu-screen.py same); saves LABEL-restored.png
#
# Needs $T (guest directory) and guest, shot, wait_until, fail, $SCREEN, $OUT from lib.sh.
FB_TOP=1440
FB_ROWS=160
FB_WIDTH=1272
FBINK=/var/local/kmc/bin/fbink

fb_save() {
  FB_BEFORE=$(shot "$1") || fail "screenshot $1"
  guest "y=\$(cut -d, -f2 /sys/class/graphics/fb0/pan); dd if=/dev/fb0 of=$T/fb-band.raw bs=$FB_WIDTH skip=\$((y + $FB_TOP)) count=$FB_ROWS 2>/dev/null && test \$(wc -c <$T/fb-band.raw) -eq $((FB_WIDTH * FB_ROWS))" ||
    fail "fb_save: cannot copy framebuffer rows $FB_TOP..$((FB_TOP + FB_ROWS - 1))"
}

fb_same_as_before() {
  "$SLOT" shot "$NAME" "$OUT/.fb-probe.png" >/dev/null 2>&1 &&
    python3 "$SCREEN" same "$FB_BEFORE" "$OUT/.fb-probe.png" 110 >/dev/null
}

fb_restore() {
  guest "y=\$(cut -d, -f2 /sys/class/graphics/fb0/pan); dd if=$T/fb-band.raw of=/dev/fb0 bs=$FB_WIDTH seek=\$((y + $FB_TOP)) count=$FB_ROWS conv=notrunc 2>/dev/null && $FBINK -q -W GC16 -s top=$FB_TOP,left=0,width=$FB_WIDTH,height=$FB_ROWS" ||
    fail "fb_restore: cannot write framebuffer rows back"
  if ! wait_until 30 "screen equals $(basename "$FB_BEFORE") again" fb_same_as_before; then
    shot "$1-not-restored" >/dev/null || true
    fail "fb_restore: the KUAL message is still on screen (later scenarios need a clean Home)"
  fi
  shot "$1-restored" >/dev/null || fail "screenshot $1-restored"
}
