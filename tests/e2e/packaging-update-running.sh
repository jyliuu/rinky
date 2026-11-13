#!/bin/bash
# e2e-cycles: 2
# In-app update while rinky runs, across the change from a static binary to the
# dynamically linked one with its own musl loader (lib/ld-musl-armhf.so.1):
#   1. install PKG (a release with a static binary, e.g. 0.8.0) and start it the way
#      KUAL does (rinky-ctl.sh start: framework stopped, rinky running);
#   2. `rinky update install` to PKG_NEW (served by the mock release server on the
#      host), run from the launcher's copy in /tmp/rinky/bin as the in-app Update page
#      runs it: the new binary and lib/ are installed. (Not from /mnt/us/rinky/rinky:
#      a process running from the file install.sh replaces on the userstore's FUSE
#      file system dies with SIGBUS.)
#   3. `rinky-ctl.sh restart`, as the Update page asks for: the OLD launcher, still
#      running from /tmp/rinky/bin, runs the new binary directly (its interpreter is the
#      installed loader), without starting the framework; it logs the new version and
#      `pidof rinky` finds it;
#   4. exit (Home comes back), then start again: the NEW launcher runs rinky through
#      the loader copied to /tmp/rinky/bin; exit, uninstall.
#
#   PKG=out/package-old/rinky-0.8.0-kindlehf.tar.gz \
#   PKG_NEW=out/package-test/rinky-0.8.1-kindlehf.tar.gz \
#     scripts/e2e/run.sh NAME OUT packaging-update-running
#
# PKG_NEW: `scripts/package.sh --version 0.8.1 --out out/package-test` on this tree.
# rinky files already on the guest are set aside first and put back on exit.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

PKG=${PKG:-$(ls -t "$E2E_ROOT"/out/package-old/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
PKG_NEW=${PKG_NEW:-$(ls -t "$E2E_ROOT"/out/package-test/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
[ -f "$PKG" ] || skip "no old package (PKG, e.g. the 0.8.0 release asset)"
[ -f "$PKG_NEW" ] || skip "no new package (PKG_NEW): scripts/package.sh --version X --out out/package-test"
NEW_VERSION=$(basename "$PKG_NEW" | sed 's/^rinky-\(.*\)-kindlehf\.tar\.gz$/\1/')
T=/mnt/us/.rinky-e2e/update-running
MOCK=
ASIDE_DONE=
on_exit() {
  [ -n "$MOCK" ] && kill "$MOCK" 2>/dev/null
  guest "sh /mnt/us/rinky/rinky-ctl.sh stop" >/dev/null 2>&1
  guest "test -f /mnt/us/rinky/uninstall.sh && sh /mnt/us/rinky/uninstall.sh" >"$OUT/uninstall.txt" 2>&1
  [ -n "$ASIDE_DONE" ] && guest "sh $T/guest-aside.sh restore" >"$OUT/aside-restore.txt" 2>&1
  guest "rm -rf $T"
}
trap on_exit EXIT

wait_until 240 "Home tab bar" home_now || fail "Home did not appear within 240 s"
guest "mkdir -p $T" || fail "mkdir $T"
"$SLOT" push "$NAME" "$E2E_ROOT/packaging/test/guest-aside.sh" "$T/" >/dev/null || fail "push guest-aside.sh"
guest "sh $T/guest-aside.sh save" >"$OUT/aside-save.txt" 2>&1 || fail "set aside: $(cat "$OUT/aside-save.txt")"
ASIDE_DONE=1
if ! guest 'ifconfig wlan0 2>/dev/null | grep -q "inet addr:10.0.2."'; then
  "$SLOT" wifi "$NAME" >"$OUT/wifi.txt" 2>&1 || true
  wait_until 60 "wlan0 has a 10.0.2.x address" guest 'ifconfig wlan0 | grep -q "inet addr:10.0.2."' || fail "no Wi-Fi address"
fi

# 1. The old release, started as KUAL starts it.
"$SLOT" push "$NAME" "$PKG" "$T/" >/dev/null || fail "push PKG"
guest "mkdir -p $T/pkg && tar -xzf $T/$(basename "$PKG") -C $T/pkg && sh $T/pkg/rinky/install.sh" >"$OUT/install.txt" 2>&1 || fail "install"
guest "file /mnt/us/rinky/rinky 2>/dev/null; sed -n 's/^version=//p' /mnt/us/rinky/VERSION" >"$OUT/old-version.txt" 2>&1
log "installed: $(tr '\n' ' ' <"$OUT/old-version.txt")"
guest "sh /mnt/us/rinky/rinky-ctl.sh start --library" >"$OUT/start-old.txt" 2>&1 || fail "rinky-ctl.sh start"
wait_until 120 "old rinky running, KPPMainApp gone" reader_running || fail "the old rinky did not take over"
OLD_PID=$(rinky_pid)
nap 5
log "old rinky pid $OLD_PID"

# 2. Update while it runs.
rm -f "$OUT/port"
python3 "$E2E_ROOT/packaging/test/mock-release-server.py" --package "$PKG_NEW" --tag "v$NEW_VERSION" \
  --public-host 10.0.2.2 --port-file "$OUT/port" 2>"$OUT/mock.log" >/dev/null &
MOCK=$!
wait_until 10 "mock server" test -s "$OUT/port" || fail "mock server did not start"
URL=http://10.0.2.2:$(cat "$OUT/port")/repos/rinky/rinky/releases/latest
guest "/tmp/rinky/bin/rinky update install --url $URL --allow-http; echo exit=\$?" >"$OUT/update.txt" 2>&1
grep -q "^exit=0$" "$OUT/update.txt" || fail "update install: $(tail -3 "$OUT/update.txt")"
guest "sed -n 's/^version=//p' /mnt/us/rinky/VERSION; test -x /mnt/us/rinky/lib/ld-musl-armhf.so.1 && echo loader-installed" >"$OUT/after-update.txt" 2>&1
grep -qx "$NEW_VERSION" "$OUT/after-update.txt" && grep -qx loader-installed "$OUT/after-update.txt" \
  || fail "after the update: $(tr '\n' ' ' <"$OUT/after-update.txt")"
[ "$(rinky_pid)" = "$OLD_PID" ] || fail "the update stopped the running rinky"
pass "updated to $NEW_VERSION while the old rinky ran; lib/ld-musl-armhf.so.1 installed"

# 3. Restart under the old launcher.
guest "sh /mnt/us/rinky/rinky-ctl.sh restart" >"$OUT/restart.txt" 2>&1 || fail "restart: $(cat "$OUT/restart.txt")"
new_pid() { p=$(rinky_pid); [ -n "$p" ] && [ "$p" != "$OLD_PID" ]; }
wait_until 90 "a new rinky pid" new_pid || fail "no new rinky after the restart"
nap 8
reader_running || fail "rinky not running after the restart"
case "$(framework_state)" in *stop/*) ;; *) fail "framework not stopped after the restart: $(framework_state)" ;; esac
guest "cat /mnt/us/rinky/rinky.log" >"$OUT/rinky-after-restart.log" 2>&1
# The binary's own version may equal the old one (a test package made with
# --version); the commit in each start line tells the binaries apart.
commits=$(sed -n 's/.*start: rinky [^ ]* commit \([0-9a-f]*\) .*/\1/p' "$OUT/rinky-after-restart.log")
first=$(echo "$commits" | head -n 1) last=$(echo "$commits" | tail -n 1)
[ -n "$last" ] && [ "$first" != "$last" ] || fail "the restarted rinky is the old binary (commits: $(echo $commits))"
log "restart: commit $first -> $last"
[ "$(guest 'pidof rinky' | wc -w | tr -d ' ')" = 1 ] || fail "pidof rinky does not find exactly one rinky"
not_home after-restart
pass "the old launcher restarted into the new dynamic binary without the framework"

# 4. Exit, then a fresh start with the new launcher (through the loader).
exit_device
wait_until 300 "Home after the exit" home_now || fail "Home did not come back"
guest "sh /mnt/us/rinky/rinky-ctl.sh start --library" >"$OUT/start-new.txt" 2>&1 || fail "start with the new launcher"
wait_until 120 "new rinky running" reader_running || fail "the new launcher did not start rinky"
nap 5
guest 'ps -eo pid,args | grep -v grep | grep "/tmp/rinky/bin/ld-musl-armhf.so.1 /tmp/rinky/bin/rinky"' >"$OUT/ps-new.txt" 2>&1 \
  || fail "rinky does not run through the loader in /tmp/rinky/bin: $(guest 'ps -eo pid,args | grep rinky | grep -v grep')"
[ "$(guest 'pidof rinky' | wc -w | tr -d ' ')" = 1 ] || fail "pidof rinky does not find exactly one rinky (new launcher)"
not_home new-launcher
exit_device
wait_until 300 "Home after the second exit" home_now || fail "Home did not come back after the second exit"
guest 'test ! -e /tmp/rinky/bin/rinky && test ! -e /tmp/rinky/bin/ld-musl-armhf.so.1' || fail "binary or loader copy left in /tmp/rinky/bin"
pass "the new launcher runs rinky through the loader; both copies are removed on exit"
