#!/bin/bash
# e2e-cycles: 1
# Native plugins on the Kindle: rinky (dynamically linked, started through the bundled
# musl loader) loads a native plugin library from /mnt/us/rinky/plugins. The fixture
# plugin native-faulty segfaults when a book opens (its data folder holds
# crash-on-open): the signal handler names it in /tmp/rinky/plugin-crashed, the
# launcher runs rinky again without starting the framework, and that rinky turns the
# plugin off (plugins/disabled.txt), says so, and opens the book. Then the sample
# plugin, loaded normally, is listed by `rinky plugins`.
# Needs the fixture built for ARM next to the binary:
#   cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p native-faulty -p sample-native
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
LIBDIR=${E2E_NATIVE_LIBS:-$ROOT/target/armv7-unknown-linux-musleabihf/release}
for l in libnative_faulty.so libsample_native.so; do
  [ -f "$LIBDIR/$l" ] || fail "$LIBDIR/$l missing (build it, see the header)"
done
PL=$DEST/plugins

guest "test -x $DEST/lib/ld-musl-armhf.so.1" || fail "no musl loader in $DEST/lib (deploy again)"
guest "rm -rf $PL/native-faulty $PL/sample-native $PL/disabled.txt /tmp/rinky/plugin-crashed; mkdir -p $PL/native-faulty/data $PL/sample-native"
"$SLOT" push "$NAME" "$LIBDIR/libnative_faulty.so" "$PL/native-faulty/" >/dev/null || fail "push fixture"
"$SLOT" push "$NAME" "$LIBDIR/libsample_native.so" "$ROOT/plugins/sample-native/plugin.toml" "$PL/sample-native/" >/dev/null || fail "push sample"
guest "printf 'name = \"native-faulty\"\nversion = \"1\"\npermissions = [\"storage\", \"ui\"]\nnative = \"libnative_faulty.so\"\n' >$PL/native-faulty/plugin.toml; : >$PL/native-faulty/data/crash-on-open"

# The binary runs directly too (its interpreter is the loader's install path).
v=$(guest "$DEST/rinky --version | head -n 1")
case "$v" in rinky\ *) pass "the dynamic binary runs directly: $v" ;; *) fail "rinky --version: $v" ;; esac
guest "$DEST/rinky plugins list" >"$OUT/plugins-before.txt" 2>&1
grep -q "^native-faulty.*native.*on" "$OUT/plugins-before.txt" || fail "native-faulty not loaded: $(cat "$OUT/plugins-before.txt")"
grep -q "^sample-native.*native.*on" "$OUT/plugins-before.txt" || fail "sample-native not loaded"
pass "rinky plugins lists both native plugins as loaded"

wait_home 240 home-before
start_reader "$DEST/juliet.epub"
# The first rinky crashes on the book open; the second must be up and stay up.
wait_until 90 "crash restart logged" eval 'guest "grep -q \"crash restart\" $DEST/rinky.log"' || fail "no crash restart in rinky.log"
nap 10
reader_running || fail "rinky is not running after the crash restart"
case "$(framework_state)" in *stop/*) pass "the framework stayed stopped across the crash restart" ;; *) fail "framework: $(framework_state)" ;; esac
not_home after-crash-restart
save_logs
grep -q "inside native plugin native-faulty" "$OUT/rinky.log" || fail "the crash was not blamed on the plugin"
grep -q "rinky exited with 139" "$OUT/rinky.log" || fail "no segfault exit status"
grep -q "native-faulty crashed rinky last time; turning it off" "$OUT/rinky.log" || fail "the second rinky did not turn the plugin off"
guest "cat $PL/disabled.txt" | grep -qx "native-faulty" || fail "native-faulty not in disabled.txt"
guest "test ! -e /tmp/rinky/plugin-crashed" || fail "the crash marker is still there"
[ "$(guest 'pidof rinky' | wc -w | tr -d ' ')" = 1 ] || fail "pidof rinky does not find exactly one rinky"
pass "segfault in a native plugin: rinky ran again with the plugin off, framework stopped, pidof works"

exit_device
wait_clean_home 300 home-after
guest "rm -rf $PL/native-faulty $PL/sample-native $PL/disabled.txt"
pass "exit after the crash restart restores Home"
