#!/bin/bash
# Verifier cycle 1 (checks 1, 3, 6, 12): daemons that stop answering. powerd is
# SIGSTOPped while the power key is pressed (sleep needs powerd's flIntensity for the
# restore journal), then wifid while `rinky device wifi status` runs. Every lipc
# call must time out (3 s in the input loop, 10 s in the wifi tool), the screensaver
# and the wake must still happen within 10 s, and the watchdog must not fire.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$(dirname "$0")/device-helpers/lib.sh"

wait_home 240 home-before
clear_env
start_device device sleep --seconds 600
powerd=$(guest 'pidof powerd')
wifid=$(guest 'pidof wifid')
trap 'guest "kill -CONT $powerd $wifid" >/dev/null 2>&1' EXIT
log "SIGSTOP powerd ($powerd)"
guest "kill -STOP $powerd"
# Latency from the key to the log line, measured on the guest (seconds).
latency() {
  guest "t0=\$(cut -d' ' -f1 /proc/uptime); n0=\$(grep -c -- '$1' $DEST/rinky.log); $DEST/rinky inject power >/dev/null 2>&1;
    i=0; while [ \$(grep -c -- '$1' $DEST/rinky.log) -le \$n0 ] && [ \$i -lt 300 ]; do usleep 100000; i=\$((i+1)); done;
    t1=\$(cut -d' ' -f1 /proc/uptime); echo \"\$t0 \$t1\" | awk '{printf \"%.1f\", \$2-\$1}'"
}
l1=$(latency '[0-9] sleep: screensaver drawn')
log "power -> screensaver with powerd stopped: ${l1} s"
sleep 3
shot screensaver-powerd-stopped >/dev/null
l2=$(latency '[0-9] sleep: awake, screen restored')
log "power -> wake with powerd stopped: ${l2} s"
guest "kill -CONT $powerd"
log "SIGCONT powerd; SIGSTOP wifid ($wifid)"
guest "kill -STOP $wifid"
w=$(guest "t0=\$(cut -d' ' -f1 /proc/uptime); $DEST/rinky device wifi status >/tmp/wifi-status.txt 2>&1; e=\$?; t1=\$(cut -d' ' -f1 /proc/uptime); echo \"\$t0 \$t1 \$e\" | awk '{printf \"%.1f s exit %d\", \$2-\$1, \$3}'")
guest 'cat /tmp/wifi-status.txt' >"$OUT/wifi-status-wifid-stopped.txt" 2>&1
log "rinky device wifi status with wifid stopped: $w"
guest "kill -CONT $wifid"
trap - EXIT
{ echo "power -> screensaver (powerd stopped): $l1 s"; echo "power -> wake (powerd stopped): $l2 s"; echo "wifi status (wifid stopped): $w"; } >"$OUT/latencies.txt"
guest "grep -E 'hung|killing' $DEST/watchdog.log" && fail "the watchdog fired during the stalls"
[ -n "$(rinky_pid)" ] || fail "rinky died during the stalls"
awk -v a="$l1" -v b="$l2" 'BEGIN { exit !(a < 10 && b < 10) }' || fail "sleep/wake took $l1 / $l2 s with powerd stopped"
ws=${w%% s*}
awk -v a="$ws" 'BEGIN { exit !(a < 15) }' || fail "wifi status took $w with wifid stopped"
pass "powerd and wifid stalls: sleep $l1 s, wake $l2 s, wifi status $w, no watchdog kill"
exit_device
wait_clean_home 300 home-after
save_logs
check_no_watchers
check_restored
