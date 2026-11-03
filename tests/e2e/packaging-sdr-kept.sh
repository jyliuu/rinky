#!/bin/bash
# Uninstall keeps a book .sdr folder that existed (empty) before the install (verifier
# finding 3, cycle 1), on the guest's real /mnt/us. No framework restart, no Wi-Fi.
#   1. /mnt/us/documents/pkgtest-kept.sdr/ exists and is empty (KOReader can leave such a
#      folder); the file list is taken twice (the second pass finds framework churn);
#   2. install.sh; .install-sdr lists documents/pkgtest-kept.sdr;
#   3. rinky's sidecar rinky.epub.json is written into pkgtest-kept.sdr/ and into a new
#      pkgtest-new.sdr/ (by hand here: the same file name rinky's store writes);
#   4. uninstall.sh: pkgtest-kept.sdr/ is still there and empty, pkgtest-new.sdr/ is gone,
#      and the file list equals the list before the install (compare-lists.py).
# No book files are created, so the framework indexes nothing new.
#
#   PKG=out/package/rinky-V-kindlehf.tar.gz scripts/e2e/run.sh NAME OUT packaging-sdr-kept
#
# Runs on a fresh boot or after other scenarios: rinky files already on the guest are
# set aside in /mnt/us/.rinky-e2e/aside first and put back on exit (packaging/test/guest-aside.sh).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

PKG=${PKG:-$(ls -t "$E2E_ROOT"/out/package/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
[ -f "$PKG" ] || fail "no package (PKG)"
# Staging (package, unpacked files, helpers) is on /mnt/us under /mnt/us/.rinky-e2e:
# the guest /tmp is a 64 MB tmpfs and the package unpacks to about 21 MB.
T=/mnt/us/.rinky-e2e/packaging-sdr
KEPT=/mnt/us/documents/pkgtest-kept.sdr
NEW=/mnt/us/documents/pkgtest-new.sdr
COMPARE="$E2E_ROOT/packaging/test/compare-lists.py"
listing() { guest "sh $T/guest-list.sh" >"$OUT/$1.txt" || fail "listing $1"; }

guest "rm -rf $T && mkdir -p $T" || fail "mkdir $T"
"$SLOT" push "$NAME" "$E2E_ROOT/packaging/test/guest-aside.sh" "$E2E_ROOT/packaging/test/guest-list.sh" "$T/" >/dev/null || fail "push helpers"
guest "sh $T/guest-aside.sh save" >"$OUT/aside-save.txt" 2>&1 || { cat "$OUT/aside-save.txt" >&2; fail "set aside"; }
trap 'guest "rm -rf $KEPT $NEW; sh $T/guest-aside.sh restore" >"$OUT/aside-restore.txt" 2>&1; guest "rm -rf $T"' EXIT
log "$(cat "$OUT/aside-save.txt")"
guest 'test ! -e /mnt/us/rinky && test ! -e /mnt/us/extensions/rinky' || fail "/mnt/us/rinky still exists after set aside"

# 1. The empty .sdr folder, then the file lists before the install.
guest "mkdir -p /mnt/us/documents && rm -rf $KEPT $NEW && mkdir $KEPT" || fail "mkdir $KEPT"
listing before-1
sleep 5
listing before-2

# 2. Install.
"$SLOT" push "$NAME" "$PKG" "$T/" >/dev/null || fail "push package"
guest "mkdir -p $T/pkg && tar -xzf $T/$(basename "$PKG") -C $T/pkg && sh $T/pkg/rinky/install.sh" >"$OUT/install.txt" 2>&1 || fail "install: $(cat "$OUT/install.txt")"
guest 'cat /mnt/us/rinky/.install-sdr' >"$OUT/install-sdr.txt" 2>&1
grep -qx "documents/pkgtest-kept.sdr" "$OUT/install-sdr.txt" || fail ".install-sdr does not list documents/pkgtest-kept.sdr ($(tr '\n' ' ' <"$OUT/install-sdr.txt"))"
pass "install listed the existing empty .sdr folder in .install-sdr"

# 3. Sidecars as rinky writes them.
guest "echo '{\"pos\":\"/body/DocFragment[2]/body/p[3]/text().0\"}' >$KEPT/rinky.epub.json && mkdir -p $NEW && echo '{}' >$NEW/rinky.epub.json && ls -la $KEPT $NEW" >"$OUT/sidecars.txt" 2>&1 || fail "write sidecars"

# 4. Uninstall.
guest 'sh /mnt/us/rinky/uninstall.sh' >"$OUT/uninstall.txt" 2>&1 || fail "uninstall: $(cat "$OUT/uninstall.txt")"
guest "test -d $KEPT && test -z \"\$(ls -A $KEPT)\"" || fail "$KEPT was removed or is not empty after uninstall"
guest "test ! -e $NEW" || fail "$NEW (created after the install) is still there"
pass "uninstall kept the empty .sdr folder that existed before, removed the one created after"
listing after
if python3 "$COMPARE" "$OUT/before-2.txt" "$OUT/after.txt" --churn "$OUT/before-1.txt" "$OUT/before-2.txt" --report "$OUT/diff.txt" >/dev/null; then
  pass "file list after uninstall equals the list before install ($(tail -n 1 "$OUT/diff.txt"))"
else
  cat "$OUT/diff.txt" >&2
  fail "file list differs (diff.txt)"
fi
