#!/bin/bash
# ink-net in the emulator (key: net). Needs a current deploy (scripts/emu-slot.sh deploy NAME).
#
# 1. Starts the ink-net mock server on the host (127.0.0.1:18471 HTTP, :18472 HTTPS with
#    the TEST-ONLY CA) and pushes the CA to the guest.
# 2. Wi-Fi not joined: `rinky selftest --lipc --net` against the host over USB
#    networking (192.168.15.201). The Wi-Fi hook must report cmState PENDING and the
#    device-hook download must be refused; all other net checks pass.
# 3. Joins the emulated AP (scripts/emu-slot.sh wifi; falls back to ap_scan 1, which this
#    guest needs) and runs the selftest again over Wi-Fi (10.0.2.2): the hook reports
#    CONNECTED, the download runs with powerd deferSuspend renewals.
# 4. Runs the cross-built ink-net test binaries on the guest (ARM) against in-guest
#    servers on 127.0.0.1: the unit tests (HTTP, TLS, SSE, downloads, OPDS, KOSync) and
#    the verifier's hostile-input tests (tests/verify1_attacks.rs). Both binaries and the
#    mock server are rebuilt on every run (cargo relinks only what changed), so the guest
#    never runs a stale build.
# 5. powerd protocol for the keep-awake hook (NET_E2E_DEFER=0 skips it): powerd takes
#    deferSuspend only during its readyToSuspend countdown. With the framework running,
#    preventScreenSaver 0 + powerButton 1 lead to goingToScreenSaver and readyToSuspend
#    (20-150 s on the emulator); at readyToSuspend the guest sends the same
#    `lipc-set-prop -i com.lab126.powerd deferSuspend 60` as LipcServices::defer_suspend.
#    Leaves the guest in the screensaver: run it last, then `down`.
#
# Ports: NET_E2E_HTTP_PORT / NET_E2E_TLS_PORT (default 18471 / 18472).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

HTTP_PORT=${NET_E2E_HTTP_PORT:-18471}
TLS_PORT=${NET_E2E_TLS_PORT:-18472}
ARM_TARGET=armv7-unknown-linux-musleabihf

MOCK="$E2E_ROOT/target/debug/examples/net-mock-server"
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=2 cargo build -q -p ink-net --example net-mock-server) >"$OUT/build-mock.log" 2>&1 || fail "build net-mock-server"
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=2 cargo zigbuild --tests --release -p ink-net --target $ARM_TARGET --message-format=json) >"$OUT/build-arm-tests.json" 2>"$OUT/build-arm-tests.log" || fail "cross-build ink-net tests"
# "target-name executable" for every test binary cargo reported.
python3 -I -c 'import json, sys
for line in open(sys.argv[1]):
    try: m = json.loads(line)
    except ValueError: continue
    if m.get("reason") == "compiler-artifact" and m.get("profile", {}).get("test") and m.get("executable"):
        print(m["target"]["name"], m["executable"])' "$OUT/build-arm-tests.json" >"$OUT/arm-test-binaries.txt"
TESTBIN=$(awk '$1 == "ink_net" {print $2}' "$OUT/arm-test-binaries.txt")
ATTACKBIN=$(awk '$1 == "verify1_attacks" {print $2}' "$OUT/arm-test-binaries.txt")
[ -x "$TESTBIN" ] && [ -x "$ATTACKBIN" ] || fail "ARM test binaries missing: $(cat "$OUT/arm-test-binaries.txt")"
log "ARM test binaries: $(basename "$TESTBIN"), $(basename "$ATTACKBIN")"

"$MOCK" --http 127.0.0.1:$HTTP_PORT --https 127.0.0.1:$TLS_PORT --ca-out "$OUT/test-ca.pem" >"$OUT/mock-server.log" 2>&1 &
MOCK_PID=$!
trap 'sleep 1; kill $MOCK_PID 2>/dev/null' EXIT
# The server writes the CA when it listens (2.7 s after start on a loaded host; a fixed
# 1 s wait pushed a missing file).
for _ in $(seq 1 60); do [ -s "$OUT/test-ca.pem" ] && break; kill -0 $MOCK_PID 2>/dev/null || break; sleep 0.5; done
kill -0 $MOCK_PID 2>/dev/null || fail "mock server did not start: $(cat "$OUT/mock-server.log")"
[ -s "$OUT/test-ca.pem" ] || fail "mock server wrote no CA within 30 s: $(cat "$OUT/mock-server.log")"
log "mock server pid $MOCK_PID on ports $HTTP_PORT/$TLS_PORT"
"$SLOT" push "$NAME" "$OUT/test-ca.pem" "$DEST" >/dev/null || fail "push CA"

# Downloads go to the userstore, where books are stored (rename, fsync, UTF-8 names). On
# 5.19.5 /mnt/us is fuse.fsp over an ext4 image at /mnt/base-us (00-userstore-mount.txt).
SCRATCH=$DEST/net-scratch
selftest() { # selftest HOST LOGFILE
  guest "mkdir -p $SCRATCH && cd /tmp && $DEST/rinky selftest --lipc --net http://$1:$HTTP_PORT --net-tls https://$1:$TLS_PORT --ca $DEST/test-ca.pem --net-scratch $SCRATCH; echo exit=\$?" >"$2" 2>&1
}
guest "mount | grep -E ' /mnt/us | /mnt/base-us '; df -h /mnt/us | tail -1" >"$OUT/00-userstore-mount.txt" 2>&1
expect() { # expect LOGFILE PATTERN DESCRIPTION
  grep -q -- "$2" "$1" || fail "$3 (no '$2' in $(basename "$1"))"
  pass "$3"
}

# --- Wi-Fi not joined -------------------------------------------------------------
# A fresh boot is not joined (cmState PENDING). On a reused guest, disconnect first.
if [ "$(guest 'lipc-get-prop com.lab126.wifid cmState' 2>/dev/null)" = CONNECTED ]; then
  log "Wi-Fi is connected from an earlier run; disconnecting"
  guest 'wpa_cli -i wlan0 disconnect' >"$OUT/00-wifi-disconnect.txt" 2>&1
  wait_until 90 "wifid cmState left CONNECTED" guest '[ "$(lipc-get-prop com.lab126.wifid cmState)" != CONNECTED ]' || fail "Wi-Fi did not disconnect"
fi
guest 'date -u; echo "wifid cmState = $(lipc-get-prop com.lab126.wifid cmState)"; echo "wifid enable = $(lipc-get-prop com.lab126.wifid enable)"; ifconfig wlan0 | grep "inet addr" || echo "wlan0: no address"' >"$OUT/01-wifi-before.txt" 2>&1
log "before join: $(grep cmState "$OUT/01-wifi-before.txt")"
selftest 192.168.15.201 "$OUT/02-selftest-usb-wifi-pending.log"
L="$OUT/02-selftest-usb-wifi-pending.log"
expect "$L" "PASS net.wifi_hook: offline: Wi-Fi not connected (cmState " "hook reports offline (cmState $(sed -n 's/^wifid cmState = //p' "$OUT/01-wifi-before.txt")) before the join"
expect "$L" "PASS net.download.device_hooks: wifi hook on, keep-awake on: refused while offline" "device-hook download refused while Wi-Fi is not connected"
expect "$L" "PASS net.https.verified" "HTTPS with the test CA over USB networking"
expect "$L" "PASS net.kosync" "KOSync over USB networking"
expect "$L" "renamed over an older file in $SCRATCH" "resumed download renamed over an older file on the userstore"
expect "$L" "downloaded 'Delta æøå.epub' (7000 bytes, SHA-256 ok, name listed back intact)" "non-ASCII file name intact on the userstore"
expect "$L" "PASS net.download.locked: double start: second run busy, dest is the book (400000 bytes); flock held by another open file: busy" "double start refused; flock works on the userstore"
expect "$L" "PASS net.download.unframed: no length, no chunking: refused without confirmation" "body without length refused unless confirmed"
expect "$L" "HTTPS cut without close_notify: refused" "HTTPS body cut without close_notify refused"
grep -q "^FAIL net\." "$L" && fail "net check failed before the join: $(grep '^FAIL net\.' "$L" | head -3)"
pass "no net check failed before the join"

# --- Join Wi-Fi ---------------------------------------------------------------------
if ! guest 'wpa_cli -i wlan0 list_networks | grep -q Kindle-QEMU' >/dev/null 2>&1 && "$SLOT" wifi "$NAME" >"$OUT/03-wifi-join.txt" 2>&1; then
  :
else
  log "joining with ap_scan 1 (emu-slot wifi got no address, or the network is already known)"
  guest 'wpa_cli -i wlan0 ap_scan 1; id=$(wpa_cli -i wlan0 list_networks | awk "/Kindle-QEMU/ {print \$1; exit}"); [ -n "$id" ] || id=$(wpa_cli -i wlan0 add_network | tail -1); wpa_cli -i wlan0 set_network $id ssid Kindle-QEMU; wpa_cli -i wlan0 set_network $id key_mgmt NONE; wpa_cli -i wlan0 select_network $id; wpa_cli -i wlan0 reassociate' >>"$OUT/03-wifi-join.txt" 2>&1
  wait_until 90 "wlan0 has a 10.0.2.x address" guest 'ifconfig wlan0 | grep -q "inet addr:10.0.2."' || fail "Wi-Fi join failed"
fi
wait_until 60 "wifid cmState CONNECTED" guest '[ "$(lipc-get-prop com.lab126.wifid cmState)" = CONNECTED ]' || fail "cmState never CONNECTED"
guest 'date -u; echo "wifid cmState = $(lipc-get-prop com.lab126.wifid cmState)"; echo "wifid signalStrength = $(lipc-get-prop com.lab126.wifid signalStrength)"; ifconfig wlan0 | grep "inet addr"' >"$OUT/04-wifi-after.txt" 2>&1
log "after join: $(grep cmState "$OUT/04-wifi-after.txt")"

# --- Wi-Fi connected ------------------------------------------------------------------
selftest 10.0.2.2 "$OUT/05-selftest-wifi-connected.log"
L="$OUT/05-selftest-wifi-connected.log"
expect "$L" "PASS net.wifi_hook: online (wifid cmState CONNECTED)" "hook reports CONNECTED after the join"
expect "$L" "PASS net.download.device_hooks: wifi hook on, keep-awake on: downloaded 400000 bytes" "download runs with the Wi-Fi hook and deferSuspend"
expect "$L" "PASS net.keep_awake: idle: no deferral; busy:" "keep-awake hook reaches powerd (deferSuspend outside a countdown = nothing to defer)"
for c in net.https.verified net.https.unknown_ca_refused net.http.user_agent net.http.redirects net.http.timeouts net.sse.stream net.sse.reconnect net.download.resume net.download.cancel net.download.checksum_mismatch net.download.locked net.download.unframed net.download.offline_refused net.opds net.kosync; do
  expect "$L" "PASS $c:" "$c over Wi-Fi"
done
grep -q "^FAIL net\." "$L" && fail "net check failed over Wi-Fi: $(grep '^FAIL net\.' "$L" | head -3)"
guest "ls -la $SCRATCH; rmdir $SCRATCH/* $SCRATCH 2>/dev/null; true" >"$OUT/05b-scratch-after.txt" 2>&1
pass "no net check failed over Wi-Fi"

# --- ARM unit tests -----------------------------------------------------------------
"$SLOT" push "$NAME" "$TESTBIN" "$DEST" >/dev/null || fail "push test binary"
guest "chmod +x $DEST/$(basename "$TESTBIN") && cd /tmp && $DEST/$(basename "$TESTBIN") --test-threads=2; echo exit=\$?" >"$OUT/06-ink-net-unit-tests-arm.log" 2>&1
guest "rm -f $DEST/$(basename "$TESTBIN")" || true
L="$OUT/06-ink-net-unit-tests-arm.log"
expect "$L" "test result: ok." "ink-net unit tests pass on ARM"
expect "$L" "exit=0" "test binary exit status 0"
"$SLOT" push "$NAME" "$ATTACKBIN" "$DEST" >/dev/null || fail "push verifier test binary"
guest "chmod +x $DEST/$(basename "$ATTACKBIN") && cd /tmp && $DEST/$(basename "$ATTACKBIN") --test-threads=2; echo exit=\$?" >"$OUT/06b-verify1-attacks-arm.log" 2>&1
guest "rm -f $DEST/$(basename "$ATTACKBIN")" || true
L="$OUT/06b-verify1-attacks-arm.log"
expect "$L" "test result: ok. 37 passed; 0 failed; 0 ignored" "verifier hostile-input tests pass on ARM, none ignored"
expect "$L" "exit=0" "verifier test binary exit status 0"

# --- powerd deferSuspend protocol ---------------------------------------------------
if [ "${NET_E2E_DEFER:-1}" = 1 ]; then
  guest 'st=$(lipc-get-prop com.lab126.powerd state); r=$(lipc-set-prop -i com.lab126.powerd deferSuspend 60 2>&1); x=$?; echo "state $st: deferSuspend 60 -> exit $x $r"' >"$OUT/07-defer-in-active-state.txt" 2>&1
  guest 'rm -f /tmp/net-defer.txt; lipc-set-prop -i com.lab126.powerd preventScreenSaver 0;
    ( lipc-wait-event -m -t com.lab126.powerd "*" >/tmp/net-powerd-events.txt 2>&1 </dev/null & );
    ( sh -c "ev=\$(lipc-wait-event -s 300 com.lab126.powerd readyToSuspend); st=\$(lipc-get-prop com.lab126.powerd state); r=\$(lipc-set-prop -i com.lab126.powerd deferSuspend 60 2>&1); x=\$?; echo \"\$(date -u +%T) event [\$ev] state [\$st] deferSuspend 60 exit \$x \$r\"" >/tmp/net-defer.txt 2>&1 </dev/null & );
    ( lipc-set-prop -i com.lab126.powerd powerButton 1 >/dev/null 2>&1 </dev/null & ); date -u +%T' >"$OUT/08-power-button-at.txt" 2>&1
  log "powerButton sent at $(cat "$OUT/08-power-button-at.txt"); waiting for readyToSuspend"
  wait_until 330 "readyToSuspend answered" guest 'test -s /tmp/net-defer.txt' || log "no readyToSuspend within 330 s"
  sleep 5
  guest 'cat /tmp/net-defer.txt; echo ---; cat /tmp/net-powerd-events.txt; echo ---; grep -i -E "defer|readyToSuspend" /var/log/messages | tail -20' >"$OUT/09-defer-at-ready-to-suspend.txt" 2>&1
  shot screensaver-after-defer >/dev/null
  guest 'pkill -f "lipc-wait-event -m -t com.lab126.powerd" 2>/dev/null; true'
  if grep -q "deferSuspend 60 exit 0" "$OUT/09-defer-at-ready-to-suspend.txt"; then
    pass "powerd accepted deferSuspend during its readyToSuspend countdown"
  else
    fail "deferSuspend at readyToSuspend not accepted: $(head -3 "$OUT/09-defer-at-ready-to-suspend.txt")"
  fi
fi

sleep 1
kill $MOCK_PID 2>/dev/null
trap - EXIT
grep -c " GET \| PUT \| POST \| HEAD " "$OUT/mock-server.log" >"$OUT/mock-server-requests.txt" || true
grep -q "user-agent=rinky/" "$OUT/mock-server.log" || fail "the mock server saw no rinky requests"
pass "mock server logged $(cat "$OUT/mock-server-requests.txt") requests from the guest"
