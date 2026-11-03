#!/bin/bash
# Verifier (cycle 2), on the guest with busybox sh, cp and mv, on a tmpfs used as
# RINKY_PREFIX (the real /mnt/us is not touched). No framework restart.
#   A. six first installs started at once: each installs or says "another install or
#      update is running"; afterwards one consistent version (sha256sum -c), no lock or
#      .new copy; uninstall -> file list equal to before.
#   B. (known failure, runs only with RINKY_VERIFY2_KNOWN_FAILS=1) the first install
#      is killed (SIGKILL, as by a power loss) while it copies the binary; install
#      again; uninstall -> file list equal to before. Found: an empty /mnt/us/extensions
#      is left (the killed run created it and recorded that only in its uncommitted
#      .install-state copy).
#
#   PKG=out/package/rinky-V-kindlehf.tar.gz scripts/e2e/run.sh NAME OUT packaging-verify2-killed
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

PKG=${PKG:-$(ls -t "$E2E_ROOT"/out/package/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
[ -f "$PKG" ] || fail "no package (PKG)"
KNOWN=${RINKY_VERIFY2_KNOWN_FAILS:-0}
# Staging (package, unpacked files, helpers) is on /mnt/us under /mnt/us/.rinky-e2e:
# the guest /tmp is a 64 MB tmpfs and the package unpacks to about 21 MB.
T=/mnt/us/.rinky-e2e/packaging-verify2
FS=/tmp/packaging-verify2-fs
RUNDIR=/tmp/packaging-verify2-run
FS_KB=
ENV="RINKY_PREFIX=$FS RINKY_RUN=$RUNDIR"
trap 'guest "umount $FS 2>/dev/null; rm -rf $T $FS $RUNDIR"' EXIT

# fresh: a new tmpfs (twice the unpacked package plus 8 MB: one install and its .new
# copies) with a book, a KOReader sidecar and nothing of rinky.
fresh() {
  guest "umount $FS 2>/dev/null; rm -rf $FS $RUNDIR; mkdir -p $FS && mount -t tmpfs -o size=${FS_KB}k tmpfs $FS && mkdir -p $FS/mnt/us/Books/a.sdr $RUNDIR && echo b >$FS/mnt/us/Books/a.epub && echo k >$FS/mnt/us/Books/a.sdr/metadata.epub.lua" || fail "tmpfs"
}
listing() { guest "cd $FS && find mnt/us | sort" >"$OUT/$1.txt" 2>&1 || fail "listing $1"; }
consistent() {
  guest "cd $FS/mnt/us && sha256sum -c rinky/SHA256SUMS 2>&1 | grep -v ': OK\$'; echo VERSION=\$(sed -n 's/^version=//p' rinky/VERSION); echo STATE=\$(sed -n 's/^version=//p' rinky/.install-state); echo leftovers=\$(ls -A rinky extensions/rinky extensions/rinky/bin | grep -c -e '\\.new[0-9]' -e '^\\.install\\.lock')" >"$OUT/$1.txt" 2>&1
  ! grep -q FAILED "$OUT/$1.txt" && grep -qx "leftovers=0" "$OUT/$1.txt" &&
    [ "$(sed -n 's/^VERSION=//p' "$OUT/$1.txt")" = "$(sed -n 's/^STATE=//p' "$OUT/$1.txt")" ]
}
same() {
  if diff -u "$OUT/$2.txt" "$OUT/$3.txt" >"$OUT/$1-diff.txt"; then pass "$1: file list equal to before"; else fail "$1: file list differs: $(grep '^[+-]m' "$OUT/$1-diff.txt" | tr '\n' ' ')"; fi
}

guest "rm -rf $T && mkdir -p $T" || fail "mkdir $T"
"$SLOT" push "$NAME" "$PKG" "$T/" >/dev/null || fail "push PKG"
guest "mkdir -p $T/pkg && tar -xzf $T/$(basename "$PKG") -C $T/pkg" || fail "unpack PKG"
FS_KB=$(( 2 * $(guest "du -sk $T/pkg/rinky | cut -f1") + 8192 ))
log "install fs: ${FS_KB} KB"
guest 'busybox | head -1; ls -l /bin/cp /bin/mv /bin/sh' >"$OUT/guest-tools.txt" 2>&1

# A. six first installs at once
fresh
listing A-before
guest "cd $T && for i in 1 2 3 4 5 6; do (env $ENV sh $T/pkg/rinky/install.sh >$T/A-\$i.txt 2>&1; echo exit=\$? >>$T/A-\$i.txt) & done; wait; for i in 1 2 3 4 5 6; do echo \"--- \$i\"; cat $T/A-\$i.txt; done" >"$OUT/A-installs.txt" 2>&1
ok=$(grep -c '^exit=0$' "$OUT/A-installs.txt")
busy=$(grep -c 'another install or update is running' "$OUT/A-installs.txt")
log "A: $ok installed, $busy refused as busy"
[ "$ok" -ge 1 ] && [ $((ok + busy)) -eq 6 ] || fail "A: unexpected install results (A-installs.txt)"
consistent A-state || fail "A: not one consistent version: $(tr '\n' ' ' <"$OUT/A-state.txt")"
guest "grep -qx created=extensions $FS/mnt/us/rinky/.install-state" || fail "A: created=extensions not recorded"
pass "A: one consistent version after six installs at once"
guest "$ENV sh $FS/mnt/us/rinky/uninstall.sh" >"$OUT/A-uninstall.txt" 2>&1 || fail "A: uninstall"
listing A-after
same A A-before A-after

if [ "$KNOWN" != 1 ]; then
  log "SKIP: B (known failure; RINKY_VERIFY2_KNOWN_FAILS=1 runs it)"
  exit 0
fi

# B. killed first install, install again, uninstall. busybox sh runs cp and mv as built-in
# applets (a PATH shim is never called), so the installer is killed from outside: as soon
# as it has created /mnt/us/extensions, kill -9. The kill must land before .install-state
# is committed; otherwise the attempt is repeated on a fresh file system. The wait loop
# also ends when the installer has exited (its /proc/PID/exe is gone, also as a zombie).
killed=no
for attempt in 1 2 3 4 5; do
  fresh
  listing B-before
  guest "env $ENV sh $T/pkg/rinky/install.sh >$T/B-killed.txt 2>&1 & p=\$!; n=0; until [ -d $FS/mnt/us/extensions ] || [ ! -e /proc/\$p/exe ] || [ \$n -ge 2000000 ]; do n=\$((n + 1)); done; kill -9 \$p; wait \$p; echo exit=\$?; cat $T/B-killed.txt; if [ -f $FS/mnt/us/rinky/.install-state ]; then echo state=committed; else echo state=none; fi" >"$OUT/B-killed.txt" 2>&1
  log "B: attempt $attempt: $(grep -e '^exit=' -e '^state=' "$OUT/B-killed.txt" | tr '\n' ' ')"
  if grep -qx 'state=none' "$OUT/B-killed.txt" && grep -qx 'exit=137' "$OUT/B-killed.txt"; then
    killed=yes
    break
  fi
done
[ "$killed" = yes ] || fail "B: could not kill the installer before it committed .install-state"
listing B-after-killed
guest "$ENV sh $T/pkg/rinky/install.sh" >"$OUT/B-install.txt" 2>&1 || fail "B: install after the killed one"
consistent B-state || fail "B: not consistent after the second install: $(tr '\n' ' ' <"$OUT/B-state.txt")"
guest "cat $FS/mnt/us/rinky/.install-state" >"$OUT/B-install-state.txt" 2>&1
guest "$ENV sh $FS/mnt/us/rinky/uninstall.sh" >"$OUT/B-uninstall.txt" 2>&1 || fail "B: uninstall"
listing B-after
same B B-before B-after
