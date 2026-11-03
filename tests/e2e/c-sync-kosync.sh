#!/bin/bash
# KOSync plugin in the real reader (key: sync). Needs a current deploy.
#
# 1. Starts plugins/kosync/examples/mock_server on the host (127.0.0.1:$PORT): user
#    "reader" with the fake password "fake-pass", and a progress from device "kpw" at
#    page 12 of testdata/juliet.epub (same file as the deployed one, same partial MD5).
# 2. Joins the emulated AP (the guest reaches the host at 10.0.2.2) and writes
#    $DEST/plugins/kosync/settings.json (server, user, MD5 of the password).
# 3. Starts the reader on juliet.epub: the pull on open must show the "Jump to the synced
#    position?" dialog; taps Jump; the reader must report the new page to the plugins.
# 4. Turns a page, exits the reader; the close push must reach the mock server
#    ("kosync: pushed" in rinky.log).
# Port: SYNC_E2E_PORT (default 18481).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
PORT=${SYNC_E2E_PORT:-18481}
PLUG=$DEST/plugins

MOCK="$E2E_ROOT/target/debug/examples/mock_server"
(cd "$E2E_ROOT" && CARGO_BUILD_JOBS=2 cargo build -q -p kosync --example mock_server) >"$OUT/build-mock.log" 2>&1 || fail "build mock_server"
"$MOCK" "127.0.0.1:$PORT" "$E2E_ROOT/testdata/juliet.epub" 12 >"$OUT/mock-server.log" 2>&1 &
MOCK_PID=$!
trap 'kill $MOCK_PID 2>/dev/null; guest "rm -rf $PLUG" >/dev/null 2>&1' EXIT
sleep 2
grep -q "remote progress for" "$OUT/mock-server.log" || fail "mock server did not start: $(cat "$OUT/mock-server.log")"
log "$(cat "$OUT/mock-server.log")"

wait_home 240 home-before
"$SLOT" wifi "$NAME" >"$OUT/wifi.log" 2>&1 || fail "wifi: $(tail -3 "$OUT/wifi.log")"
KEY=$(printf %s fake-pass | md5 -q 2>/dev/null || printf %s fake-pass | md5sum | cut -d' ' -f1)
guest "rm -rf $PLUG; mkdir -p $PLUG/kosync && printf '%s' '{\"server\":\"http://10.0.2.2:$PORT/kosync\",\"user\":\"reader\",\"userkey\":\"$KEY\"}' > $PLUG/kosync/settings.json && cat $PLUG/kosync/settings.json" >"$OUT/settings.txt" 2>&1 || fail "write settings.json"
log "settings: $(cat "$OUT/settings.txt")"

has() { guest "grep -qF '$1' $DEST/rinky.log" 2>/dev/null; }
start_reader
wait_until 60 "jump dialog" has 'kosync: dialog 5 "Progress sync"' || { save_logs; fail "no jump dialog in the log"; }
nap 3
shot dialog >/dev/null
tap 870 1050
wait_until 30 "jump" has "kosync: jump to /body/DocFragment" || { save_logs; fail "Jump not answered"; }
nap 4
not_home jumped
tap 1000 900
nap 4
not_home next-page
exit_device
save_logs
grep -E "kosync" "$OUT/rinky.log" >"$OUT/kosync-lines.txt"
check() { grep -qF "$1" "$OUT/rinky.log" || fail "rinky.log lacks: $1"; log "log has: $1"; }
check "kosync: pull cf7ce9b8d4e135c8ac340f18d1e58c85: RemoteNewer"
check "plugins: kosync: go to /body/DocFragment"
check "kosync: pushed /body/DocFragment"
if grep -E "WARN.*kosync|-> kosync .*failed" "$OUT/rinky.log"; then fail "kosync errors in the log"; fi
wait_clean_home 300 home-after
pass "kosync pull on open, jump dialog, jump, push over Wi-Fi to the mock server"
