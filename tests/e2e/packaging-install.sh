#!/bin/bash
# Install package on a fresh guest: install, uninstall, and compare file lists; then
# install again, start from the installed path the way KUAL does, exit, uninstall the
# way KUAL does, and compare again.
#
#   PKG=out/package/rinky-V-kindlehf.tar.gz scripts/e2e/run.sh NAME OUT packaging-install
#
# Runs on a fresh `scripts/emu-slot.sh up NAME` boot or after other scenarios: rinky
# files already on the guest (a `deploy` writes /mnt/us/rinky) are set aside in
# /mnt/us/.rinky-e2e/aside first and put back on exit (packaging/test/guest-aside.sh).
# KUAL is not installed by --jbsh, so the KUAL actions are run as KUAL runs them:
# `cd /mnt/us/extensions/rinky && ./bin/kual.sh PARAMS` (action + params of menu.json).
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"
. "$E2E_ROOT/packaging/test/e2e-fb.sh"

PKG=${PKG:-$(ls -t "$E2E_ROOT"/out/package/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
[ -f "$PKG" ] || fail "no package: run scripts/package.sh or set PKG"
# Staging (package, unpacked files, helpers) is on /mnt/us under /mnt/us/.rinky-e2e:
# the guest /tmp is a 64 MB tmpfs and the package unpacks to about 21 MB.
T=/mnt/us/.rinky-e2e/packaging-test
EXT=/mnt/us/extensions/rinky
COMPARE="$E2E_ROOT/packaging/test/compare-lists.py"
BOOK=/mnt/us/Books/pkgtest-juliet.epub
SDR=/mnt/us/Books/pkgtest-juliet.sdr

listing() {
  guest "sh $T/guest-list.sh" >"$OUT/$1.txt" || fail "listing $1"
  log "listing $1: $(wc -l <"$OUT/$1.txt" | tr -d ' ') lines"
}
kual() { guest "cd $EXT && ./bin/kual.sh $1" >"$OUT/kual-$1.txt" 2>&1; }
install_pkg() {
  guest "rm -rf $T/pkg && mkdir -p $T/pkg && tar -xzf $T/$(basename "$PKG") -C $T/pkg && sh $T/pkg/rinky/install.sh" \
    >"$OUT/$1.txt" 2>&1 || { cat "$OUT/$1.txt" >&2; fail "$1"; }
  log "$1: $(tail -n 3 "$OUT/$1.txt" | head -n 1)"
}

wait_home 240 home-before
guest "mkdir -p $T" || fail "mkdir $T"
"$SLOT" push "$NAME" "$E2E_ROOT/packaging/test/guest-aside.sh" "$E2E_ROOT/packaging/test/guest-list.sh" "$T/" >/dev/null || fail "push helpers"
guest "sh $T/guest-aside.sh save" >"$OUT/aside-save.txt" 2>&1 || { cat "$OUT/aside-save.txt" >&2; fail "set aside"; }
trap 'guest "sh $T/guest-aside.sh restore" >"$OUT/aside-restore.txt" 2>&1; guest "rm -rf $T"' EXIT
log "$(cat "$OUT/aside-save.txt")"
guest 'test ! -e /mnt/us/rinky && test ! -e /mnt/us/extensions/rinky' || fail "/mnt/us/rinky still exists after set aside"
guest 'cat /etc/version.txt; mount | grep -E " /mnt/(us|base-us) "; ls -la /mnt/us; ls -d /mnt/us/extensions 2>&1; busybox | head -1' >"$OUT/guest-env.txt" 2>&1
log "package $(basename "$PKG")"

# The user's state before install: a book in /mnt/us/Books with a KOReader sidecar.
BOOKS_NEW=$(guest 'test -d /mnt/us/Books || echo yes')
"$SLOT" push "$NAME" "$PKG" "$E2E_ROOT/testdata/juliet.epub" "$T/" >/dev/null || fail "push package and book"
guest "mkdir -p $SDR && cp $T/juliet.epub $BOOK && echo '-- fake KOReader sidecar' >$SDR/metadata.epub.lua" || fail "setup"
guest "tar -tzf $T/$(basename "$PKG")" >"$OUT/busybox-tar-list.txt" 2>&1 || fail "busybox tar cannot list the package"
log "busybox tar lists $(wc -l <"$OUT/busybox-tar-list.txt" | tr -d ' ') entries"
sleep 30
listing before-1
sleep 60
listing before-2

# --- A: install, uninstall, compare (acceptance diff) ---
install_pkg install-A
listing installed-A
grep -q '^F /mnt/us/rinky/rinky ' "$OUT/installed-A.txt" || fail "binary not in /mnt/us/rinky"
grep -q "^F $EXT/menu.json " "$OUT/installed-A.txt" || fail "KUAL menu not in $EXT"
guest '/mnt/us/rinky/rinky version; cat /mnt/us/rinky/.install-state' >"$OUT/installed-version.txt" 2>&1 || fail "installed binary does not run"
pass "installed: $(head -n 1 "$OUT/installed-version.txt")"
guest 'sh /mnt/us/rinky/uninstall.sh' >"$OUT/uninstall-A.txt" 2>&1 || fail "uninstall A"
listing after-A
if python3 "$COMPARE" "$OUT/before-2.txt" "$OUT/after-A.txt" --churn "$OUT/before-1.txt" "$OUT/before-2.txt" --report "$OUT/diff-A.txt" >/dev/null; then
  pass "A: file list after uninstall equals the list before install ($(tail -n 1 "$OUT/diff-A.txt"))"
else
  cat "$OUT/diff-A.txt" >&2
  fail "A: file list differs after install + uninstall (diff-A.txt)"
fi

# --- B: install, KUAL start, read a book, exit, KUAL uninstall ---
install_pkg install-B
kual start
wait_until 90 "rinky running from /mnt/us/rinky, KPPMainApp gone" reader_running || fail "KUAL start did not start rinky"
sleep 6
# The launcher runs a tmpfs copy of the installed binary (/tmp/rinky/bin, see
# scripts/rinky.sh): the process path is the copy, its bytes are the installed file's.
guest 'ps -eo pid,args | grep "[/]tmp/rinky/bin/rinky " && cmp /tmp/rinky/bin/rinky /mnt/us/rinky/rinky && echo "same bytes as /mnt/us/rinky/rinky"' \
  >"$OUT/ps-started.txt" 2>&1 || fail "no rinky process from the tmpfs copy of /mnt/us/rinky/rinky (ps-started.txt)"
log "process: $(head -n 1 "$OUT/ps-started.txt")"
not_home kual-start-library
kual exit
wait_until 60 "rinky gone" reader_gone || fail "KUAL exit did not stop rinky"
wait_clean_home 300 home-after-kual-exit
pass "KUAL Exit to Kindle restores Home"

guest "sh /mnt/us/rinky/rinky-ctl.sh start $BOOK" >"$OUT/ctl-start-book.txt" 2>&1 || fail "ctl start"
wait_until 90 "rinky reading $BOOK" reader_running || fail "ctl start did not start rinky"
sleep 8
not_home reading-juliet
tap 1000 900
sleep 4
not_home reading-juliet-page2
guest "cat $SDR/rinky.epub.json; ls -la $SDR" >"$OUT/sidecar.txt" 2>&1 || fail "no sidecar $SDR/rinky.epub.json"
pass "sidecar written next to the book"
exit_device
wait_clean_home 300 home-after-exit
save_logs
fb_save home-before-kual-uninstall
kual uninstall || { cat "$OUT/kual-uninstall.txt" >&2; fail "KUAL uninstall"; }
shot kual-uninstall-message >/dev/null || fail "screenshot kual-uninstall-message"
fb_restore kual-uninstall
cp "$OUT/kual-uninstall.txt" "$OUT/uninstall-B.txt"
guest "ls -la $SDR" >"$OUT/koreader-sidecar-after.txt" 2>&1 || fail "KOReader sidecar folder removed"
grep -q metadata.epub.lua "$OUT/koreader-sidecar-after.txt" || fail "KOReader sidecar file removed"
pass "KOReader sidecar kept"
listing after-B
if python3 "$COMPARE" "$OUT/before-2.txt" "$OUT/after-B.txt" --churn "$OUT/before-1.txt" "$OUT/before-2.txt" --report "$OUT/diff-B.txt" >/dev/null; then
  pass "B: file list after use + uninstall equals the list before install"
else
  # Only framework activity may remain; anything of ours is a failure.
  if grep -E '^(added|changed|removed) .*(rinky|/mnt/us/extensions|pkgtest-juliet)' "$OUT/diff-B.txt" >"$OUT/diff-B-ours.txt"; then
    cat "$OUT/diff-B-ours.txt" >&2
    fail "B: rinky files left after uninstall (diff-B-ours.txt)"
  fi
  log "B: only framework changes remain (diff-B.txt): $(tail -n 1 "$OUT/diff-B.txt")"
fi
guest 'ls /tmp/rinky* 2>&1; ls -d /mnt/us/rinky /mnt/us/extensions 2>&1' >"$OUT/leftovers.txt" 2>&1
log "leftovers check: $(tr '\n' ' ' <"$OUT/leftovers.txt")"
guest "rm -rf $BOOK $SDR"
[ -n "$BOOKS_NEW" ] && guest 'rmdir /mnt/us/Books'
pass "install, KUAL start/exit, uninstall"
