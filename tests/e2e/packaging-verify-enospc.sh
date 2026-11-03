#!/bin/bash
# Verifier (cycle 1): no space left during an upgrade, on the guest with busybox cp and a
# real full file system (a tmpfs used as RINKY_PREFIX, sized to the unpacked package
# plus 3 MB: the old version fits, a second copy of the binary or the downloaded package
# does not; the real /mnt/us is not touched). No framework restart.
#   1. update scratch on the full file system: `rinky update install` must fail
#      cleanly (old install intact, no scratch left);
#   2. install.sh --upgrade runs out of space while copying the binary;
#   3. `rinky update install` with its scratch on roomy /mnt/us: the installer
#      runs out of space while copying the binary.
#   After each part the installed files must still be one version: `sha256sum -c
#   rinky/SHA256SUMS` passes, VERSION equals .install-state, the binary is the old
#   one (no variant marker), and no .new copy, .install.lock, .update.lock or .update
#   scratch is left.
#
# Parts 2 and 3 were a verifier finding in cycle 1 (install.sh was not all-or-nothing);
# fixed, so all three parts always run.
#
#   PKG=out/package/rinky-V-kindlehf.tar.gz \
#   PKG_NEW=out/package-test/rinky-0.2.1-kindlehf.tar.gz \
#     scripts/e2e/run.sh NAME OUT packaging-verify-enospc
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

PKG=${PKG:-$(ls -t "$E2E_ROOT"/out/package/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
PKG_NEW=${PKG_NEW:-$(ls -t "$E2E_ROOT"/out/package-test/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
[ -f "$PKG" ] || fail "no package (PKG)"
[ -f "$PKG_NEW" ] || fail "no test release (PKG_NEW)"
KNOWN=1
# Staging (package, unpacked files, helpers) is on /mnt/us under /mnt/us/.rinky-e2e:
# the guest /tmp is a 64 MB tmpfs and the package unpacks to about 21 MB.
T=/mnt/us/.rinky-e2e/packaging-enospc
SMALL=/tmp/packaging-enospc-small
HOME_BIG=$T/home
RUNDIR=/tmp/packaging-enospc-run
SMALL_KB=
ENV="RINKY_PREFIX=$SMALL RINKY_RUN=$RUNDIR"
VARIANT=$("$E2E_ROOT/packaging/test/make-variant.sh" "$PKG_NEW" "$OUT/variant/$(basename "$PKG_NEW")") || fail "make variant"
NEW_VERSION=$(basename "$PKG_NEW" | sed 's/^rinky-\(.*\)-kindlehf\.tar\.gz$/\1/')
MOCK=
on_exit() {
  [ -n "$MOCK" ] && kill "$MOCK" 2>/dev/null
  guest "umount $SMALL 2>/dev/null; rm -rf $T $SMALL $RUNDIR"
}
trap on_exit EXIT

# small_fs: a fresh tmpfs of SMALL_KB at $SMALL with $SMALL/mnt/us, and PKG installed into it.
small_fs() {
  guest "umount $SMALL 2>/dev/null; rm -rf $SMALL $RUNDIR $HOME_BIG; mkdir -p $SMALL $HOME_BIG && mount -t tmpfs -o size=${SMALL_KB}k tmpfs $SMALL && mkdir -p $SMALL/mnt/us" || fail "mount tmpfs"
  guest "$ENV sh $T/pkg/rinky/install.sh" >"$OUT/$1-install-old.txt" 2>&1 || fail "$1: install into the small fs: $(cat "$OUT/$1-install-old.txt")"
  guest "df -k $SMALL | tail -n 1" >"$OUT/$1-df.txt"
  log "$1: small fs after the first install: $(cat "$OUT/$1-df.txt")"
}
# state LABEL: is the small fs one version? Writes LABEL-state.txt; status 0 = yes.
state() {
  guest "cd $SMALL/mnt/us && sha256sum -c rinky/SHA256SUMS 2>&1 | grep -v ': OK\$'; echo VERSION=\$(sed -n 's/^version=//p' rinky/VERSION); echo STATE=\$(sed -n 's/^version=//p' rinky/.install-state); echo variant-binary=\$(grep -c verifier-variant rinky/rinky); echo leftovers=\$(ls -A rinky extensions/rinky extensions/rinky/bin | grep -c -e '\\.new[0-9]' -e '^\\.install\\.lock' -e '^\\.update'); ls -A rinky" >"$OUT/$1-state.txt" 2>&1
  grep -q FAILED "$OUT/$1-state.txt" && return 1
  [ "$(sed -n 's/^VERSION=//p' "$OUT/$1-state.txt")" = "$(sed -n 's/^STATE=//p' "$OUT/$1-state.txt")" ] || return 1
  grep -qx "variant-binary=0" "$OUT/$1-state.txt" || return 1
  grep -qx "leftovers=0" "$OUT/$1-state.txt" || return 1
  return 0
}

guest "rm -rf $T && mkdir -p $T" || fail "mkdir $T"
"$SLOT" push "$NAME" "$PKG" "$T/" >/dev/null || fail "push PKG"
guest "mkdir -p $T/pkg && tar -xzf $T/$(basename "$PKG") -C $T/pkg" || fail "unpack PKG"
SMALL_KB=$(( $(guest "du -sk $T/pkg/rinky | cut -f1") + 3072 ))
log "small fs: ${SMALL_KB} KB (unpacked package + 3 MB)"
"$SLOT" push "$NAME" "$VARIANT" "$T/" >/dev/null || fail "push variant"
guest "mkdir -p $T/new && tar -xzf $T/$(basename "$VARIANT") -C $T/new" || fail "unpack variant"

# Wi-Fi for the mock release server (same steps as packaging-update.sh).
if ! guest 'ifconfig wlan0 2>/dev/null | grep -q "inet addr:10.0.2."'; then
  if ! "$SLOT" wifi "$NAME" >"$OUT/wifi.txt" 2>&1; then
    guest 'wpa_cli -i wlan0 ap_scan 1; id=$(wpa_cli -i wlan0 list_networks | awk "/Kindle-QEMU/ {print \$1; exit}"); [ -n "$id" ] || id=$(wpa_cli -i wlan0 add_network | tail -1); wpa_cli -i wlan0 set_network $id ssid Kindle-QEMU; wpa_cli -i wlan0 set_network $id key_mgmt NONE; wpa_cli -i wlan0 select_network $id; wpa_cli -i wlan0 reassociate' >>"$OUT/wifi.txt" 2>&1
    wait_until 60 "wlan0 has a 10.0.2.x address" guest 'ifconfig wlan0 | grep -q "inet addr:10.0.2."' || fail "wifi"
  fi
fi
rm -f "$OUT/port"
python3 "$E2E_ROOT/packaging/test/mock-release-server.py" --package "$VARIANT" --tag "v$NEW_VERSION" \
  --public-host 10.0.2.2 --port-file "$OUT/port" 2>"$OUT/mock.log" >/dev/null &
MOCK=$!
i=0
while [ ! -s "$OUT/port" ] && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
URL=http://10.0.2.2:$(cat "$OUT/port")/repos/rinky/rinky/releases/latest
log "mock release ($NEW_VERSION variant) at $URL"

# 1. Update scratch on the full file system.
small_fs p1
guest "$ENV RINKY_HOME=$SMALL/mnt/us/rinky $SMALL/mnt/us/rinky/rinky update install --url $URL --allow-http; echo exit=\$?" >"$OUT/p1-update.txt" 2>&1
log "p1: $(tail -n 2 "$OUT/p1-update.txt" | tr '\n' ' ')"
grep -q '^exit=1$' "$OUT/p1-update.txt" || fail "p1: update did not fail on a full file system"
state p1 || fail "p1: install not intact after a failed download/unpack ($(tr '\n' ' ' <"$OUT/p1-state.txt"))"
grep -q "^VERSION=0" "$OUT/p1-state.txt" && grep -q "variant-binary=0" "$OUT/p1-state.txt" || fail "p1: not the old version"
guest "test ! -e $SMALL/mnt/us/rinky/.update" || fail "p1: scratch .update left on the full file system"
pass "p1: no space for the download: update refused, old install intact, no scratch left"

if [ "$KNOWN" != 1 ]; then
  log "SKIP: parts 2 and 3 (known failure; RINKY_VERIFY_KNOWN_FAILS=1 runs them)"
  exit 0
fi

# 2. install.sh --upgrade out of space at the binary.
small_fs p2
guest "$ENV sh $T/new/rinky/install.sh --upgrade; echo exit=\$?" >"$OUT/p2-upgrade.txt" 2>&1
log "p2: $(tail -n 2 "$OUT/p2-upgrade.txt" | tr '\n' ' ')"
grep -q '^exit=1$' "$OUT/p2-upgrade.txt" || fail "p2: the upgrade did not run out of space (small fs too big?)"
p2=ok
state p2 || p2=mixed
log "p2: after the failed upgrade: $(tr '\n' ' ' <"$OUT/p2-state.txt")"

# 3. In-app update, scratch on /var (roomy), install target full.
small_fs p3
guest "$ENV RINKY_HOME=$HOME_BIG $SMALL/mnt/us/rinky/rinky update install --url $URL --allow-http; echo exit=\$?" >"$OUT/p3-update.txt" 2>&1
log "p3: $(tail -n 2 "$OUT/p3-update.txt" | tr '\n' ' ')"
grep -q '^exit=1$' "$OUT/p3-update.txt" || fail "p3: the update did not fail"
p3=ok
state p3 || p3=mixed
log "p3: after the failed update: $(tr '\n' ' ' <"$OUT/p3-state.txt")"

[ "$p2" = ok ] || fail "p2: install.sh --upgrade out of space left a mixed install (p2-state.txt)"
[ "$p3" = ok ] || fail "p3: in-app update out of space left a mixed install (p3-state.txt)"
pass "no space left: installs stay one version"
