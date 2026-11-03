#!/bin/bash
# Phase 5 check 12 (normal exit) with the tmpfs copy: rinky runs from
# /tmp/rinky/bin, exits on the top-right tap, the launcher restores and starts the
# framework, the watchdog sees the clean finish.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
start_device device touch --seconds 300
not_home touch-test-page
exe=$(guest 'readlink /proc/$(pidof rinky)/exe')
# /tmp is a symlink on the Kindle (5.19.5: /tmp -> /var/tmp), so the kernel reports the
# resolved path.
tmpdir=$(guest 'readlink -f /tmp')
guest 'ls -ld /tmp; mount | grep -E " on /var | on /tmp | on /var/tmp "; df /tmp/rinky/bin' >"$OUT/tmpfs.txt" 2>&1
log "rinky runs from $exe (/tmp is $tmpdir)"
case "$exe" in
  /tmp/rinky/bin/rinky* | "$tmpdir"/rinky/bin/rinky*) ;;
  *) fail "binary runs from $exe" ;;
esac
grep -q tmpfs "$OUT/tmpfs.txt" || fail "no tmpfs under /tmp (see tmpfs.txt)"
pass "binary runs from the tmpfs copy ($exe)"
guest 'ls -l /proc/$(cat /tmp/rinky/launcher.pid)/fd 2>/dev/null' >"$OUT/launcher-fds.txt"
grep -q "/mnt/us/rinky/rinky.sh" "$OUT/launcher-fds.txt" && fail "launcher keeps its script on /mnt/us open"
tap 640 900
wait_log 10 "tap 1 at 640,900"
exit_device
wait_clean_home 300 home-after
save_logs
grep -q "rinky exited with 0" "$OUT/rinky.log" || fail "exit status"
grep -q "launcher finished" "$OUT/watchdog.log" || fail "watchdog did not see the clean exit"
pass "normal exit restores Home"
check_no_watchers
check_restored
