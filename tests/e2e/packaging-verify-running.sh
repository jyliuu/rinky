#!/bin/bash
# Verifier (cycle 1) attacks on the installed package with a real rinky running:
#   A. install twice, uninstall twice (second time from the package copy) -> file list
#      equal to before;
#   B. start rinky from the installed path; install without --upgrade while it runs
#      -> refused, no file changed; install --upgrade while it runs -> new files in
#      place, the running rinky keeps running; uninstall while it runs -> rinky
#      stops, Home comes back, no rinky file is left.
#
#   PKG=out/package/rinky-V-kindlehf.tar.gz \
#   PKG_NEW=out/package-test/rinky-0.2.1-kindlehf.tar.gz \
#     scripts/e2e/run.sh NAME OUT packaging-verify-running
#
# One framework restart (start + uninstall). Runs on a fresh boot or after `deploy`:
# rinky files already on the guest are set aside first and put back on exit.
NAME=$1 OUT=$2
. "$(dirname "$0")/../../scripts/e2e/lib.sh"

PKG=${PKG:-$(ls -t "$E2E_ROOT"/out/package/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
PKG_NEW=${PKG_NEW:-$(ls -t "$E2E_ROOT"/out/package-test/rinky-*-kindlehf.tar.gz 2>/dev/null | head -1)}
[ -f "$PKG" ] || fail "no package (PKG)"
[ -f "$PKG_NEW" ] || fail "no test release (PKG_NEW)"
# Staging (package, unpacked files, helpers) is on /mnt/us under /mnt/us/.rinky-e2e:
# the guest /tmp is a 64 MB tmpfs and the package unpacks to about 21 MB.
T=/mnt/us/.rinky-e2e/packaging-verify
COMPARE="$E2E_ROOT/packaging/test/compare-lists.py"
VARIANT=$("$E2E_ROOT/packaging/test/make-variant.sh" "$PKG_NEW" "$OUT/variant/$(basename "$PKG_NEW")") || fail "make variant"

listing() {
  guest "sh $T/guest-list.sh" >"$OUT/$1.txt" || fail "listing $1"
  log "listing $1: $(wc -l <"$OUT/$1.txt" | tr -d ' ') lines"
}
compare() { # compare LABEL BEFORE AFTER: no difference that is ours
  if python3 "$COMPARE" "$OUT/$2.txt" "$OUT/$3.txt" --churn "$OUT/before-1.txt" "$OUT/before-2.txt" --report "$OUT/diff-$1.txt" >/dev/null; then
    pass "$1: file list equal ($(tail -n 1 "$OUT/diff-$1.txt"))"
  elif grep -E '^(added|changed|removed) .*(rinky|/mnt/us/extensions|\.fuse_hidden)' "$OUT/diff-$1.txt" >"$OUT/diff-$1-ours.txt"; then
    cat "$OUT/diff-$1-ours.txt" >&2
    fail "$1: rinky files differ (diff-$1-ours.txt)"
  else
    log "$1: only framework changes remain: $(tail -n 1 "$OUT/diff-$1.txt")"
  fi
}
files_md5() { guest 'cd /mnt/us && find rinky extensions/rinky -type f | sort | while read f; do echo "$f $(md5sum "$f" | cut -d" " -f1)"; done' >"$OUT/$1.txt" || fail "md5 list $1"; }

if ! wait_until 240 "Home tab bar" home_now; then
  shot home-before-timeout >/dev/null || true
  fail "Home did not appear within 240 s"
fi
shot home-before >/dev/null || fail "screenshot home-before"
guest "rm -rf $T && mkdir -p $T" || fail "mkdir $T"
[ "$(basename "$VARIANT")" != "$(basename "$PKG")" ] || fail "PKG and PKG_NEW have the same file name"
"$SLOT" push "$NAME" "$E2E_ROOT/packaging/test/guest-aside.sh" "$E2E_ROOT/packaging/test/guest-list.sh" "$PKG" "$VARIANT" "$T/" >/dev/null || fail "push"
guest "sh $T/guest-aside.sh save" >"$OUT/aside-save.txt" 2>&1 || { cat "$OUT/aside-save.txt" >&2; fail "set aside"; }
trap 'guest "sh /mnt/us/rinky/rinky-ctl.sh stop >/dev/null 2>&1; sh $T/guest-aside.sh restore" >"$OUT/aside-restore.txt" 2>&1; guest "rm -rf $T"' EXIT
log "$(cat "$OUT/aside-save.txt")"
guest "mkdir -p $T/pkg $T/new && tar -xzf $T/$(basename "$PKG") -C $T/pkg && tar -xzf $T/$(basename "$VARIANT") -C $T/new && sed -n 's/^version=//p' $T/pkg/rinky/VERSION $T/new/rinky/VERSION" >"$OUT/versions.txt" 2>&1 || fail "unpack on the guest"
log "packages on the guest: $(tr '\n' ' ' <"$OUT/versions.txt")"
sleep 20
listing before-1
sleep 60
listing before-2

# --- A: install twice, uninstall twice ---
guest "sh $T/pkg/rinky/install.sh && sh $T/pkg/rinky/install.sh" >"$OUT/A-install-twice.txt" 2>&1 || fail "A: install twice"
guest "cat /mnt/us/rinky/.install-state" >"$OUT/A-install-state.txt"
guest "sh /mnt/us/rinky/uninstall.sh" >"$OUT/A-uninstall-1.txt" 2>&1 || fail "A: first uninstall"
guest "sh $T/pkg/rinky/uninstall.sh; echo exit=\$?" >"$OUT/A-uninstall-2.txt" 2>&1
grep -q '^exit=0$' "$OUT/A-uninstall-2.txt" || fail "A: second uninstall: $(tail -n 2 "$OUT/A-uninstall-2.txt" | tr '\n' ' ')"
listing after-A
compare A before-2 after-A

# --- B: running rinky ---
guest "sh $T/pkg/rinky/install.sh" >"$OUT/B-install.txt" 2>&1 || fail "B: install"
guest "sh /mnt/us/rinky/rinky-ctl.sh start" >"$OUT/B-start.txt" 2>&1 || fail "B: ctl start"
wait_until 90 "rinky running from /mnt/us/rinky" reader_running || fail "B: rinky did not start"
sleep 6
not_home running-library
PID=$(rinky_pid)
guest "ls -l /proc/$PID/exe; cat /tmp/rinky/launcher.pid" >"$OUT/B-proc-before.txt" 2>&1
files_md5 B-files-before
guest "sh $T/new/rinky/install.sh; echo exit=\$?" >"$OUT/B-install-while-running.txt" 2>&1
grep -q '^exit=0$' "$OUT/B-install-while-running.txt" && fail "B: install without --upgrade ran while rinky runs"
grep -q 'rinky is running' "$OUT/B-install-while-running.txt" || fail "B: refusal message: $(cat "$OUT/B-install-while-running.txt")"
files_md5 B-files-after-refusal
diff -u "$OUT/B-files-before.txt" "$OUT/B-files-after-refusal.txt" >"$OUT/B-refusal-diff.txt" || fail "B: files changed by a refused install"
pass "B: install refused while rinky runs; no file changed"

guest "sh $T/new/rinky/install.sh --upgrade; echo exit=\$?" >"$OUT/B-upgrade-while-running.txt" 2>&1
grep -q '^exit=0$' "$OUT/B-upgrade-while-running.txt" || fail "B: --upgrade while running: $(tail -n 3 "$OUT/B-upgrade-while-running.txt" | tr '\n' ' ')"
guest "cd /mnt/us && sha256sum -c rinky/SHA256SUMS; grep -c verifier-variant rinky/rinky; ls -l /proc/$PID/exe; ls -la /mnt/us/rinky" >"$OUT/B-after-upgrade.txt" 2>&1
grep -q FAILED "$OUT/B-after-upgrade.txt" && fail "B: installed files do not match SHA256SUMS after --upgrade"
[ "$(rinky_pid)" = "$PID" ] || fail "B: the running rinky $PID did not survive --upgrade (now: $(rinky_pid))"
not_home still-running-after-upgrade
pass "B: --upgrade while running put the new files in place; rinky $PID kept running"

guest "sh /mnt/us/rinky/uninstall.sh; echo exit=\$?" >"$OUT/B-uninstall-while-running.txt" 2>&1
grep -q '^exit=0$' "$OUT/B-uninstall-while-running.txt" || fail "B: uninstall while running: $(tail -n 3 "$OUT/B-uninstall-while-running.txt" | tr '\n' ' ')"
wait_until 60 "rinky gone" reader_gone || fail "B: uninstall did not stop rinky"
wait_clean_home 300 home-after-uninstall
guest 'ls -d /mnt/us/rinky /mnt/us/extensions/rinky /tmp/rinky 2>&1' >"$OUT/B-leftovers.txt" 2>&1
# ps on the guest sometimes warns "Unknown HZ value! (94) Assume 100." on stderr; that
# line is not a process (verifier cycle 2: it failed this check once).
guest 'pidof rinky; ps -eo pid,args 2>/dev/null | grep "[i]nkreader[.-]"' >"$OUT/B-processes-left.txt" 2>&1
[ -s "$OUT/B-processes-left.txt" ] && fail "B: rinky processes left: $(cat "$OUT/B-processes-left.txt")"
listing after-B
compare B before-2 after-B
pass "install/upgrade/uninstall while rinky runs"
