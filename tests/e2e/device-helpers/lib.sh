# Helpers for the device agent's scenarios (tests/e2e/device-*.sh). Source after
# scripts/e2e/lib.sh. Not a scenario itself (run.sh only runs tests/e2e/*.sh).
EMU=${EMU:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)/eink-emulator}

# known_failure FINDING: the scenario shows an open verifier finding (it fails until the
# product code is fixed). It runs only with VERIFY_KNOWN_FAILURES=1; otherwise it logs a
# SKIP line and exits 0, so the default suite stays green. Remove the call with the fix.
known_failure() {
  [ "${VERIFY_KNOWN_FAILURES:-0}" = 1 ] && return 0
  log "SKIP (open verifier finding; run with VERIFY_KNOWN_FAILURES=1): $*"
  exit 0
}

# start_device ARGS...: `rinky ARGS` through the launcher (framework stopped).
start_device() {
  guest "rm -f $DEST/rinky.log $DEST/watchdog.log; sh $DEST/rinky.sh -- $*" || fail "launch"
  wait_until 90 "rinky running, KPPMainApp gone" reader_running || fail "rinky did not take over"
  sleep 3
}

in_log() { guest "grep -q -- '$1' $DEST/rinky.log"; }
# wait_log DEADLINE_S PATTERN: fails the scenario if PATTERN never shows in rinky.log.
wait_log() { wait_until "$1" "log: $2" in_log "$2" || fail "not in rinky.log within $1 s: $2"; }
log_lines() { guest "grep -- '$1' $DEST/rinky.log"; }
hal_event() { guest "lipc-send-event com.lab126.hal $1" || fail "lipc-send-event hal $1"; }

# Settings for the next launcher start ($DEST/rinky.env is sourced by rinky.sh).
set_env() {
  local l body=""
  for l in "$@"; do body+="export $l"$'\n'; done
  printf '%s' "$body" | guest "cat > $DEST/rinky.env" || fail "write rinky.env"
  log "rinky.env: $(echo "$body" | tr '\n' ' ')"
}
clear_env() { guest "rm -f $DEST/rinky.env"; }

# wifi_join: joins the emulator's open AP Kindle-QEMU through rinky's own Wi-Fi code
# (wifid createProfile + cmConnect), which also runs DHCP. On a fresh -snapshot boot wifid
# has no profile and `emu-slot.sh wifi` (wpa_cli behind wifid's back) stayed in SCANNING;
# that path is only the fallback. Output in $OUT/wifi-join.txt.
wifi_join() {
  local d=$DEST/rinky
  {
    guest "$d device wifi profiles 2>/dev/null | grep -q 'essid: \"Kindle-QEMU\"' || $d device wifi create Kindle-QEMU 2>&1 | grep -v '^\[INFO'"
    guest "$d device wifi connect Kindle-QEMU 2>&1 | grep -v '^\[INFO'"
  } >"$OUT/wifi-join.txt" 2>&1
  wifi_has_ip() { guest 'ifconfig wlan0 | grep -q "inet addr:10.0.2."'; }
  if ! wait_until 60 "wlan0 has a 10.0.2.x address" wifi_has_ip; then
    log "rinky wifi connect did not give an address; trying emu-slot.sh wifi"
    "$SLOT" wifi "$NAME" >>"$OUT/wifi-join.txt" 2>&1 || return 1
  fi
  guest 'ifconfig wlan0 | grep "inet addr"' >>"$OUT/wifi-join.txt" 2>&1
}

# The emulated Wi-Fi SSH port of this slot (USB SSH port + 1).
wifi_port() { echo $(( $("$SLOT" port "$NAME") + 1 )); }
wguest() {
  ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o LogLevel=ERROR -o ServerAliveInterval=5 -o ServerAliveCountMax=3 -i "$EMU/build/ssh/id_ed25519" \
      -p "$(wifi_port)" root@127.0.0.1 "$@"
}
hmp() { python3 "$EMU/scripts/eink-qmp.py" --machine "$NAME" hmp "$1"; }

# check_no_watchers: none of the `lipc-wait-event -m` children rinky started (pids
# from its log, "lipc: watching P (pid N") is still alive. Stock watchers (the system
# runs `lipc-wait-event -m com.lab126.wifid cmStateChange ...` from boot) are not
# counted. Call after save_logs.
check_no_watchers() {
  local pids left
  pids=$(grep -o 'lipc: watching [^ ]* (pid [0-9]*' "$OUT/rinky.log" | sed 's/.*(pid //' | sort -u | tr '\n' ' ')
  [ -n "$pids" ] || fail "rinky.log names no lipc watcher pids"
  left=$(guest "for p in $pids; do grep -q lipc-wait-event /proc/\$p/cmdline 2>/dev/null && echo \$p; done")
  echo "rinky watchers: $pids; alive after exit: ${left:-none}" >"$OUT/watchers.txt"
  [ -z "$left" ] || fail "rinky's lipc watcher(s) $left outlived it"
  pass "no lipc watcher of rinky left ($(echo $pids | wc -w | tr -d ' ') checked)"
}

# Journal cleanup after a run: no restore file left, volumd disableMounting back.
check_restored() {
  local left dm gs
  left=$(guest 'cat /tmp/rinky/restore 2>/dev/null')
  [ -z "$left" ] || fail "restore journal left behind: $left"
  dm=$(guest 'lipc-get-prop com.lab126.volumd disableMounting')
  [ "$dm" = 0 ] || fail "volumd disableMounting is $dm after the run"
  pass "settings restored (no journal, disableMounting 0)"
}
