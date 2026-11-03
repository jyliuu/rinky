#!/bin/bash
# In-app update against a mock GitHub release server on the host (reached from the guest
# at 10.0.2.2 over the emulated Wi-Fi):
#   1. a release whose SHA256SUMS is wrong is refused; /mnt/us/rinky is unchanged;
#   2. the KUAL "Update rinky" action with the release URL from settings.toml
#      installs the next version; settings are kept and the binary still runs. The
#      release served is a variant of PKG_NEW whose binary and launcher differ from PKG
#      (packaging/test/make-variant.sh), so a mixed install fails `sha256sum -c`;
#   3. two KUAL updates started at the same moment (KUAL tapped twice), with a slow
#      download: one installs, the other stops at once with "An update is already
#      running"; the package is downloaded once; afterwards the install is one complete
#      version and no lock or scratch file is left (flock on the guest's /mnt/us).
#
#   PKG=out/package/rinky-0.2.0-kindlehf.tar.gz \
#   PKG_NEW=out/package-test/rinky-0.2.1-kindlehf.tar.gz \
#     scripts/e2e/run.sh NAME OUT packaging-update
#
# PKG_NEW is a test release built with `scripts/package.sh --no-build --version 0.2.1`.
# Runs on a fresh boot or after other scenarios: rinky files already on the guest are
# set aside in /mnt/us/.rinky-e2e/aside first and put back on exit (packaging/test/guest-aside.sh).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$E2E_ROOT/packaging/test/e2e-fb.sh"

PKG=${PKG:-$(ls -t "$E2E_ROOT"/out/package/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
PKG_NEW=${PKG_NEW:-$(ls -t "$E2E_ROOT"/out/package-test/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
[ -f "$PKG" ] || fail "no package (PKG)"
[ -f "$PKG_NEW" ] || fail "no test release (PKG_NEW): scripts/package.sh --no-build --version 0.2.1 --out out/package-test"
NEW_VERSION=$(basename "$PKG_NEW" | sed 's/^rinky-\(.*\)-kindlehf\.tar\.gz$/\1/')
VARIANT=$("$E2E_ROOT/packaging/test/make-variant.sh" "$PKG_NEW" "$OUT/variant/$(basename "$PKG_NEW")") || fail "make variant"
# Staging (package, unpacked files, helpers) is on /mnt/us under /mnt/us/.rinky-e2e:
# the guest /tmp is a 64 MB tmpfs and the package unpacks to about 21 MB.
T=/mnt/us/.rinky-e2e/packaging-test
EXT=/mnt/us/extensions/rinky
MOCK=
ASIDE_DONE=
on_exit() {
  [ -n "$MOCK" ] && kill "$MOCK" 2>/dev/null
  if [ -n "$ASIDE_DONE" ]; then
    guest "sh $T/guest-aside.sh restore" >"$OUT/aside-restore.txt" 2>&1
  fi
  guest "rm -rf $T"
}
trap on_exit EXIT

# start_mock LABEL [mock options]: prints nothing; sets PORT and MOCK.
start_mock() {
  local label=$1
  shift
  [ -n "$MOCK" ] && kill "$MOCK" 2>/dev/null && wait "$MOCK" 2>/dev/null
  rm -f "$OUT/port-$label"
  python3 "$E2E_ROOT/packaging/test/mock-release-server.py" --package "$VARIANT" --tag "v$NEW_VERSION" \
    --public-host 10.0.2.2 --port-file "$OUT/port-$label" "$@" 2>"$OUT/mock-$label.log" >/dev/null &
  MOCK=$!
  local i=0
  while [ ! -s "$OUT/port-$label" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
  PORT=$(cat "$OUT/port-$label" 2>/dev/null)
  [ -n "$PORT" ] || fail "mock server did not start"
  log "mock release server ($label) on 127.0.0.1:$PORT, guest URL http://10.0.2.2:$PORT"
}
rinky_files() { guest 'cd /mnt/us && find rinky extensions -type f | sort | while read f; do echo "$f $(md5sum "$f" | cut -d" " -f1)"; done' >"$OUT/$1.txt" || fail "list $1"; }
# one_version LABEL VERSION MARKERS: the installed files are exactly one package: every
# line of rinky/SHA256SUMS checks out, VERSION and .install-state name VERSION, the
# binary has MARKERS variant lines, and no lock, scratch or .new file is left.
one_version() {
  guest 'cd /mnt/us && sha256sum -c rinky/SHA256SUMS 2>&1 | grep -c ": OK$"; sha256sum -c rinky/SHA256SUMS 2>&1 | grep -v ": OK$"; echo VERSION=$(sed -n "s/^version=//p" rinky/VERSION); echo STATE=$(sed -n "s/^version=//p" rinky/.install-state); echo markers=$(grep -c verifier-variant rinky/rinky); echo leftovers=$(ls -A rinky extensions/rinky extensions/rinky/bin | grep -c -e "\.new[0-9]" -e "^\.install\.lock" -e "^\.update"); ls -A rinky' >"$OUT/$1-state.txt" 2>&1
  local want_ok
  want_ok=$(guest 'grep -c . /mnt/us/rinky/SHA256SUMS')
  [ "$(head -n 1 "$OUT/$1-state.txt")" = "$want_ok" ] || return 1
  grep -q FAILED "$OUT/$1-state.txt" && return 1
  grep -qx "VERSION=$2" "$OUT/$1-state.txt" && grep -qx "STATE=$2" "$OUT/$1-state.txt" || return 1
  grep -qx "markers=$3" "$OUT/$1-state.txt" && grep -qx "leftovers=0" "$OUT/$1-state.txt"
}

# Start gate: the Home tab bar and a running framework. Not the pixel-exact clean Home of
# wait_home: when packaging-install ran before in the same boot, its last KUAL message
# (FBInk text above the tab bar) stays on Home, because the framework does not repaint
# that band. This scenario does not depend on Home's pixels.
if ! wait_until 240 "Home tab bar" home_now; then
  shot home-before-timeout >/dev/null || true
  fail "Home did not appear within 240 s"
fi
shot home-before >/dev/null || fail "screenshot home-before"
guest 'status lab126_gui' >"$OUT/framework-before.txt" 2>&1
grep -q "start/" "$OUT/framework-before.txt" || fail "framework not running: $(cat "$OUT/framework-before.txt")"
guest "mkdir -p $T" || fail "mkdir $T"
"$SLOT" push "$NAME" "$E2E_ROOT/packaging/test/guest-aside.sh" "$T/" >/dev/null || fail "push guest-aside.sh"
guest "sh $T/guest-aside.sh save" >"$OUT/aside-save.txt" 2>&1 || { cat "$OUT/aside-save.txt" >&2; fail "set aside"; }
ASIDE_DONE=1
log "$(cat "$OUT/aside-save.txt")"
guest 'test ! -e /mnt/us/rinky && test ! -e /mnt/us/extensions/rinky' || fail "/mnt/us/rinky still exists after set aside"
EXT_BEFORE=$(guest 'test -d /mnt/us/extensions && echo yes')
if ! guest 'ifconfig wlan0 2>/dev/null | grep -q "inet addr:10.0.2."'; then
  # On 5.19.5 the supplicant can stay in SCANNING after `emu-slot.sh wifi`; then
  # ap_scan 1 + reassociate joins (found by the net and plugin agents, checked here).
  if ! "$SLOT" wifi "$NAME" >"$OUT/wifi.txt" 2>&1; then
    log "wifi: no address yet; ap_scan 1 + reassociate"
    guest 'wpa_cli -i wlan0 ap_scan 1; id=$(wpa_cli -i wlan0 list_networks | awk "/Kindle-QEMU/ {print \$1; exit}"); [ -n "$id" ] || id=$(wpa_cli -i wlan0 add_network | tail -1); wpa_cli -i wlan0 set_network $id ssid Kindle-QEMU; wpa_cli -i wlan0 set_network $id key_mgmt NONE; wpa_cli -i wlan0 select_network $id; wpa_cli -i wlan0 reassociate' >>"$OUT/wifi.txt" 2>&1
    wait_until 60 "wlan0 has a 10.0.2.x address" guest 'ifconfig wlan0 | grep -q "inet addr:10.0.2."' || fail "wifi: $(tr '\n' ' ' <"$OUT/wifi.txt")"
  fi
fi
guest 'ifconfig wlan0 | grep "inet addr"' >>"$OUT/wifi.txt" 2>&1
log "wifi: $(tail -n 1 "$OUT/wifi.txt")"

"$SLOT" push "$NAME" "$PKG" "$T/" >/dev/null || fail "push"
guest "mkdir -p $T/pkg && tar -xzf $T/$(basename "$PKG") -C $T/pkg && sh $T/pkg/rinky/install.sh" >"$OUT/install.txt" 2>&1 || fail "install"
guest 'cat /mnt/us/rinky/VERSION' >"$OUT/version-installed.txt"
log "installed $(head -n 1 "$OUT/version-installed.txt")"

# 1. Wrong checksum: refused, nothing changed.
rinky_files files-before-bad
start_mock bad --bad-checksum
URL=http://10.0.2.2:$PORT/repos/rinky/rinky/releases/latest
guest "/mnt/us/rinky/rinky update install --url $URL --allow-http; echo exit=\$?" >"$OUT/1-bad-checksum.txt" 2>&1
cat "$OUT/1-bad-checksum.txt" >&2
grep -q "checksum mismatch" "$OUT/1-bad-checksum.txt" || fail "no checksum mismatch reported"
grep -q "^exit=1$" "$OUT/1-bad-checksum.txt" || fail "bad checksum did not exit 1"
rinky_files files-after-bad
diff -u "$OUT/files-before-bad.txt" "$OUT/files-after-bad.txt" >"$OUT/files-bad-diff.txt" || fail "files changed after a refused update (files-bad-diff.txt)"
guest 'test ! -e /mnt/us/rinky/.update && test ! -e /mnt/us/rinky/.update.lock' || fail "scratch folder .update or .update.lock left behind"
grep -q "GET /objects/" "$OUT/mock-bad.log" || fail "the package was not downloaded (mock-bad.log)"
pass "wrong SHA-256 refused after download; /mnt/us/rinky unchanged; no scratch files"

# 2. Correct release through the KUAL action, URL from settings.toml.
start_mock good --digest
URL=http://10.0.2.2:$PORT/repos/rinky/rinky/releases/latest
guest "printf '[plugins.update]\nurl = \"%s\"\nallow_http = true\n' '$URL' >/mnt/us/rinky/settings.toml; cat /mnt/us/rinky/settings.toml" >"$OUT/settings.toml.txt"
guest "/mnt/us/rinky/rinky update check; echo exit=\$?" >"$OUT/2-check.txt" 2>&1
grep -q "update available: $NEW_VERSION" "$OUT/2-check.txt" || { cat "$OUT/2-check.txt" >&2; fail "update check did not see $NEW_VERSION"; }
fb_save home-before-kual-update
guest "cd $EXT && ./bin/kual.sh update; echo exit=\$?" >"$OUT/3-kual-update.txt" 2>&1
cat "$OUT/3-kual-update.txt" >&2
shot kual-update-message >/dev/null || fail "screenshot kual-update-message"
fb_restore kual-update
grep -q "^exit=0$" "$OUT/3-kual-update.txt" || fail "KUAL update failed"
guest 'cat /mnt/us/rinky/update.log; echo ---; cat /mnt/us/rinky/VERSION; echo ---; /mnt/us/rinky/rinky version; cat /mnt/us/rinky/settings.toml; cat /mnt/us/rinky/kual.log' >"$OUT/4-after-update.txt" 2>&1
grep -q "installed rinky $NEW_VERSION" "$OUT/4-after-update.txt" || fail "update.log does not say installed"
grep -q "^version=$NEW_VERSION$" "$OUT/4-after-update.txt" || fail "VERSION is not $NEW_VERSION"
grep -q "allow_http = true" "$OUT/4-after-update.txt" || fail "settings.toml lost"
guest 'test ! -e /mnt/us/rinky/.update' || fail "scratch folder .update left behind"
rinky_files files-after-good
one_version 4-installed "$NEW_VERSION" 1 || fail "after the KUAL update the install is not one complete $NEW_VERSION ($(tr '\n' ' ' <"$OUT/4-installed-state.txt"))"
guest '/mnt/us/rinky/rinky version' >"$OUT/4-binary-runs.txt" 2>&1 || fail "the updated binary does not run: $(cat "$OUT/4-binary-runs.txt")"
pass "KUAL update installed $NEW_VERSION from the release URL in settings.toml; settings kept; sha256sum -c of the installed files OK, variant binary in place"

# 3. Two KUAL updates at the same moment, slow download.
guest "sh $T/pkg/rinky/install.sh" >"$OUT/5-reinstall-old.txt" 2>&1 || fail "reinstall $(basename "$PKG")"
one_version 5-old "$(sed -n 's/^version=//p' "$OUT/version-installed.txt")" 0 || fail "reinstalled old version is not one version ($(tr '\n' ' ' <"$OUT/5-old-state.txt"))"
start_mock twice --digest --slow 6
URL=http://10.0.2.2:$PORT/repos/rinky/rinky/releases/latest
guest "printf '[plugins.update]\nurl = \"%s\"\nallow_http = true\n' '$URL' >/mnt/us/rinky/settings.toml"
fb_save home-before-two-updates
guest "cd $EXT && { ./bin/kual.sh update >$T/upd-a.txt 2>&1; echo exit=\$? >>$T/upd-a.txt; } & { cd $EXT && ./bin/kual.sh update >$T/upd-b.txt 2>&1; echo exit=\$? >>$T/upd-b.txt; } & wait; echo '--- a'; cat $T/upd-a.txt; echo '--- b'; cat $T/upd-b.txt" >"$OUT/6-two-updates.txt" 2>&1
cat "$OUT/6-two-updates.txt" >&2
shot two-updates-message >/dev/null || fail "screenshot two-updates-message"
fb_restore two-updates
guest 'cat /mnt/us/rinky/update.log' >"$OUT/6-update.log.txt" 2>&1
n_installed=$(grep -c "^Installed $NEW_VERSION; restart rinky$" "$OUT/6-two-updates.txt")
n_busy=$(grep -c "^An update is already running$" "$OUT/6-two-updates.txt")
n_get=$(grep -c "GET /objects/" "$OUT/mock-twice.log")
log "two updates: $n_installed installed, $n_busy refused as busy, $n_get package download(s)"
[ "$n_installed" = 1 ] || fail "expected exactly one 'Installed $NEW_VERSION' (6-two-updates.txt)"
[ "$n_busy" = 1 ] || fail "expected exactly one 'An update is already running' (6-two-updates.txt)"
grep -q "another update is running" "$OUT/6-update.log.txt" || fail "update.log does not show the refused second update"
[ "$n_get" = 1 ] || fail "the package was downloaded $n_get times (mock-twice.log)"
grep -c "^exit=0$" "$OUT/6-two-updates.txt" | grep -qx 1 || fail "expected one exit=0"
one_version 7-after-two "$NEW_VERSION" 1 || fail "after two updates at once the install is not one complete $NEW_VERSION ($(tr '\n' ' ' <"$OUT/7-after-two-state.txt"))"
guest 'grep -q "allow_http = true" /mnt/us/rinky/settings.toml' || fail "settings.toml lost"
pass "two updates at once: one installed $NEW_VERSION, the other refused at once; one download; install is one version; no lock left"

guest 'sh /mnt/us/rinky/uninstall.sh' >"$OUT/uninstall.txt" 2>&1 || fail "uninstall"
guest 'test ! -e /mnt/us/rinky && test ! -e /mnt/us/extensions/rinky' || fail "uninstall left files"
if [ -z "$EXT_BEFORE" ]; then
  guest 'test ! -e /mnt/us/extensions' || fail "uninstall left /mnt/us/extensions, which the installer created"
fi
pass "in-app update with checksum verification"
