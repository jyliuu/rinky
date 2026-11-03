#!/bin/bash
# Verifier cycle 1 (check 3): while a wakeup source is active (the emulator's USB
# networking; on the device a USB cable), /sys/power/wakeup_count blocks and every
# suspend attempt is refused. The attempt is retried every RINKY_SUSPEND_RETRY s
# (default 60). rinky's thread and file-descriptor counts must stay flat over
# about 8 refused attempts (retry 3 s, 24 s; each attempt also waits 2 s for
# wakeup_count; a 1 s retry starves the power key, see device-verify1-suspend-retry-wake).
# Emulator hazard: in one boot about 14 attempts 3 s apart were followed by an eMMC
# write error ("SDHI cease activity, force initialize", mmc_write_sg ret=-2) and ext4
# remounted /mnt/us read-only; run this scenario last in a boot. The scenario checks
# that /mnt/us is still writable at the end.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

counts() { guest "p=\$(pidof rinky); echo \$(grep Threads /proc/\$p/status | tr -d '\t') fds: \$(ls /proc/\$p/fd | wc -l)"; }
wait_home 240 home-before
set_env RINKY_SUSPEND_RETRY=3
start_device device sleep --seconds 600
c0=$(counts)
power_key
wait_log 20 "suspend refused"
sleep 3
c1=$(counts)
log "awake: $c0; asleep after the first refusal: $c1"
sleep 24
c2=$(counts)
refused=$(guest "grep -c '[0-9] sleep: suspend refused' $DEST/rinky.log")
blocked=$(guest "grep -c 'wakeup_count blocked' $DEST/rinky.log")
log "after 24 s more: $c2 ($refused refusals, $blocked blocked wakeup_count reads)"
{ echo "awake:          $c0"; echo "asleep (+3 s):  $c1"; echo "asleep (+27 s): $c2"; echo "refusals $refused, blocked wakeup_count reads $blocked"; } >"$OUT/threads-fds.txt"
guest "ls -l /proc/\$(pidof rinky)/fd" >"$OUT/fd-list.txt" 2>&1
t1=$(echo "$c1" | sed 's/Threads: *\([0-9]*\).*/\1/')
t2=$(echo "$c2" | sed 's/Threads: *\([0-9]*\).*/\1/')
power_key
wait_log 20 "awake, screen restored (PowerKey)"
exit_device
clear_env
wait_clean_home 300 home-after
save_logs
check_restored
guest 'touch /mnt/us/rinky/.wtest && rm /mnt/us/rinky/.wtest' || log "WARNING: /mnt/us is no longer writable (emulator eMMC fault?)"
[ $((t2 - t1)) -le 2 ] || fail "threads grew from $t1 to $t2 over $refused refused suspends (one blocked wakeup_count reader each)"
pass "thread count flat over $refused refused suspends ($t1 -> $t2)"
