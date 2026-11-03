#!/bin/bash
# Verifier cycle 1 (check 3, malformed input): hostile "chosen image" screensavers, each
# in its own rinky run (a crash on one must not hide the result of the other):
#  1. a valid 16000 x 16000 all-white PNG (280 KB file, 256 MB of pixels);
#  2. a PNG whose header asks for 38730 x 38730 8-bit gray (1.5 GB buffer) with no data
#     (a truncated download). Canvas::load_png used to allocate the whole output buffer
#     from the header before it read any data (fixed: more than 4096x4096 pixels, or
#     more than the file can hold, is refused before allocating; both images now give
#     the fallback screensaver).
# Pressing power must draw a screensaver (the image, scaled, or the fallback message)
# and rinky must keep running; it must not abort or be killed for memory.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"
H="$(dirname "$0")/device-helpers"

wait_home 240 home-before
python3 "$H/make-hostile-png.py" valid "$OUT/huge-valid.png" 16000 16000 || fail "make huge-valid.png"
python3 "$H/make-hostile-png.py" header "$OUT/huge-header.png" 38730 38730 || fail "make huge-header.png"
"$SLOT" push "$NAME" "$OUT/huge-valid.png" "$OUT/huge-header.png" "$DEST/" >/dev/null || fail "push the PNGs"
guest 'free; cat /proc/sys/vm/overcommit_memory' >"$OUT/memory-before.txt" 2>&1
clear_env
crashed=""
for which in huge-valid huge-header; do
  start_device device sleep --seconds 600 --screensaver "image:$DEST/$which.png"
  pid=$(rinky_pid)
  log "power key with the $which screensaver (rinky pid $pid)"
  power_key
  sleep 15
  if [ "$(rinky_pid)" != "$pid" ]; then
    save_logs
    cp "$OUT/rinky.log" "$OUT/rinky-$which.log"
    grep -E "exited with|memory|alloc|panick|abort" "$OUT/rinky.log" | tail -5 >"$OUT/crash-$which.txt"
    guest 'dmesg | grep -iE "out of memory|killed process|oom" | tail -5' >>"$OUT/crash-$which.txt" 2>&1
    log "rinky died on the $which screensaver: $(tr '\n' ' ' <"$OUT/crash-$which.txt")"
    wait_clean_home 300 "home-after-crash-$which"
    crashed+="$which; "
    continue
  fi
  wait_log 30 '[0-9] sleep: screensaver drawn (PowerKey)'
  shot "screensaver-$which" >/dev/null
  guest "grep -E 'VmPeak|VmHWM' /proc/$pid/status" >"$OUT/memory-$which.txt" 2>&1
  log "memory with the $which screensaver: $(tr '\n' ' ' <"$OUT/memory-$which.txt")"
  power_key
  wait_log 30 '[0-9] sleep: awake, screen restored (PowerKey)'
  sleep 3
  exit_device
  wait_clean_home 300 "home-after-$which"
  save_logs
  cp "$OUT/rinky.log" "$OUT/rinky-$which.log"
done
guest "rm -f $DEST/huge-header.png $DEST/huge-valid.png"
[ -z "$crashed" ] || fail "rinky died on the hostile screensaver(s): $crashed(see crash-*.txt)"
pass "hostile screensaver images: rinky kept running"
check_restored
