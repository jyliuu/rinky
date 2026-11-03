#!/bin/bash
# Host test of install.sh / uninstall.sh / rinky-ctl.sh on a fake userstore.
#
#   packaging/test/host-install-test.sh PACKAGE.tar.gz [OUTDIR]
#
# Builds <tmp>/mnt/us with books, a KOReader sidecar and a fake /mnt/us/koreader, then:
#   1. install, uninstall                       -> file list identical to before
#   2. install, "use" (sidecars, settings, logs), uninstall -> identical to before
#   3. KUAL already installed (extensions/ exists): install, uninstall -> extensions/ kept
#   4. upgrade keeps settings and removes files the new version no longer ships
#   5. --keep-data keeps settings and sidecars, removes program files and the KUAL entry
#   6. install refuses while a live launcher holds the lock (unless --upgrade)
#   7. install refuses while another install holds .install.lock, and then removes no
#      folder of the lock holder; uninstall refuses during an install and while a live
#      process holds .update.lock; a stale lock (dead pid) is taken over and removed
#   8. install copied by hand (USB, no install.sh): the first start through
#      rinky-ctl.sh lists the .sdr folders that exist (not those that already hold
#      an rinky file); uninstall keeps an empty listed one
#   9. a failed copy (cp fails for the binary) leaves the old install and no new folders
# Writes listings and diffs to OUTDIR (default: a temp dir). Exit status 0 = all passed.
set -uo pipefail

PKG=${1:?usage: host-install-test.sh PACKAGE.tar.gz [OUTDIR]}
PKG=$(cd "$(dirname "$PKG")" && pwd)/$(basename "$PKG")
OUT=${2:-$(mktemp -d "${TMPDIR:-/tmp}/rinky-host-test.XXXXXX")}
mkdir -p "$OUT"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/rinky-host-work.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
fails=0

log() { echo "[host-test] $*" | tee -a "$OUT/host-test.log"; }
pass() { log "PASS: $*"; }
fail() { log "FAIL: $*"; fails=$((fails + 1)); }

sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi; }

# listing ROOT: one line per path under ROOT/mnt/us: type, size, sha256 (files).
listing() {
  (cd "$1" && find mnt/us | LC_ALL=C sort | while IFS= read -r f; do
    if [ -d "$f" ]; then echo "D $f"
    else echo "F $f $(wc -c <"$f" | tr -d ' ') $(sha <"$f")"; fi
  done)
}

# fresh NAME: a fake userstore under WORK/NAME.
fresh() {
  local r=$WORK/$1
  rm -rf "$r"
  mkdir -p "$r/mnt/us/Books/a.sdr" "$r/mnt/us/documents" "$r/mnt/us/koreader" "$r/mnt/us/system" "$r/run"
  echo "epub a" >"$r/mnt/us/Books/a.epub"
  echo "-- koreader sidecar" >"$r/mnt/us/Books/a.sdr/metadata.epub.lua"
  echo "epub with space" >"$r/mnt/us/Books/b c.epub"
  echo "doc" >"$r/mnt/us/documents/doc.epub"
  echo "koreader file" >"$r/mnt/us/koreader/settings.reader.lua"
  echo "$r"
}

unpack() {
  local d=$WORK/pkg
  rm -rf "$d"; mkdir -p "$d"
  tar -xzf "$PKG" -C "$d"
  echo "$d"
}

run() { # run ROOT CMD...: with the fake prefix and runtime dir
  local r=$1; shift
  RINKY_PREFIX=$r RINKY_RUN=$r/run "$@"
}

P=$(unpack)
[ -f "$P/rinky/install.sh" ] || { fail "package has no rinky/install.sh"; exit 1; }
log "package $PKG ($(sed -n 's/^version=//p' "$P/rinky/VERSION"))"

# 1. install + uninstall
R=$(fresh t1)
listing "$R" >"$OUT/1-before.txt"
run "$R" sh "$P/rinky/install.sh" >"$OUT/1-install.txt" 2>&1 || fail "1: install failed"
listing "$R" >"$OUT/1-installed.txt"
grep -q "^F mnt/us/rinky/rinky " "$OUT/1-installed.txt" && grep -q "^F mnt/us/extensions/rinky/menu.json " "$OUT/1-installed.txt" \
  && pass "1: installed into mnt/us/rinky and mnt/us/extensions/rinky" || fail "1: files missing after install"
grep -qx "created=extensions" "$R/mnt/us/rinky/.install-state" && pass "1: .install-state records created=extensions" || fail "1: no created=extensions"
grep -qx "# Author: rinky" "$R/mnt/us/documents/rinky.sh" && grep -q "rinky-ctl.sh start --library" "$R/mnt/us/documents/rinky.sh" \
  && pass "1: library entry documents/rinky.sh created" || fail "1: no library entry documents/rinky.sh"
grep -qx "scriptlet=documents/rinky.sh" "$R/mnt/us/rinky/.install-state" && pass "1: .install-state records the library entry" || fail "1: no scriptlet= line"
run "$R" sh "$R/mnt/us/rinky/uninstall.sh" >"$OUT/1-uninstall.txt" 2>&1 || fail "1: uninstall failed"
listing "$R" >"$OUT/1-after.txt"
if diff -u "$OUT/1-before.txt" "$OUT/1-after.txt" >"$OUT/1-diff.txt"; then pass "1: listing after uninstall == before"; else fail "1: listing differs (1-diff.txt)"; fi

# 2. install, use, uninstall
R=$(fresh t2)
listing "$R" >"$OUT/2-before.txt"
run "$R" sh "$P/rinky/install.sh" >"$OUT/2-install.txt" 2>&1 || fail "2: install failed"
us=$R/mnt/us
echo '{"position":"x"}' >"$us/Books/a.sdr/rinky.json"
mkdir -p "$us/Books/b c.sdr" "$us/documents/doc.sdr"
echo '{}' >"$us/Books/b c.sdr/rinky.json"
echo '{}' >"$us/documents/doc.sdr/rinky.json"
echo 'tmp' >"$us/documents/doc.sdr/rinky.tmp4242"
# current sidecar names (wave A store): one file per format
echo '{"position":"y"}' >"$us/Books/a.sdr/rinky.epub.json"
echo '{}' >"$us/documents/doc.sdr/rinky.pdf.json"
echo 'old' >"$us/documents/doc.rinky"
echo 'language = "da"' >"$us/rinky/settings.toml"
echo 'log' >"$us/rinky/rinky.log"
mkdir -p "$us/rinky/cache" "$us/rinky/plugins/x/data" && echo 1 >"$us/rinky/plugins/x/data/f"
run "$R" sh "$us/rinky/uninstall.sh" >"$OUT/2-uninstall.txt" 2>&1 || fail "2: uninstall failed"
listing "$R" >"$OUT/2-after.txt"
if diff -u "$OUT/2-before.txt" "$OUT/2-after.txt" >"$OUT/2-diff.txt"; then pass "2: after use + uninstall, listing == before (KOReader sidecar kept)"; else fail "2: listing differs (2-diff.txt)"; fi
grep -q "removed 7 per-book file" "$OUT/2-uninstall.txt" && pass "2: 7 per-book files removed (legacy and rinky.<ext>.json)" || fail "2: per-book count ($(grep per-book "$OUT/2-uninstall.txt"))"

# 3. KUAL already installed
R=$(fresh t3)
mkdir -p "$R/mnt/us/extensions/kual-other" && echo x >"$R/mnt/us/extensions/kual-other/menu.json"
listing "$R" >"$OUT/3-before.txt"
run "$R" sh "$P/rinky/install.sh" >"$OUT/3-install.txt" 2>&1 || fail "3: install failed"
grep -q created= "$R/mnt/us/rinky/.install-state" && fail "3: created= recorded although extensions/ existed" || pass "3: nothing recorded as created"
run "$R" sh "$R/mnt/us/rinky/uninstall.sh" >"$OUT/3-uninstall.txt" 2>&1 || fail "3: uninstall failed"
listing "$R" >"$OUT/3-after.txt"
if diff -u "$OUT/3-before.txt" "$OUT/3-after.txt" >"$OUT/3-diff.txt"; then pass "3: existing extensions/ and other extension kept"; else fail "3: listing differs (3-diff.txt)"; fi

# 4. upgrade keeps data, removes stale files
R=$(fresh t4)
run "$R" sh "$P/rinky/install.sh" >"$OUT/4-install.txt" 2>&1 || fail "4: install failed"
us=$R/mnt/us
echo 'language = "da"' >"$us/rinky/settings.toml"
echo 'rinky/obsolete.txt' >>"$us/rinky/FILES"
echo old >"$us/rinky/obsolete.txt"
run "$R" sh "$P/rinky/install.sh" --upgrade >"$OUT/4-upgrade.txt" 2>&1 || fail "4: upgrade failed"
[ "$(cat "$us/rinky/settings.toml")" = 'language = "da"' ] && pass "4: settings.toml kept" || fail "4: settings.toml changed"
[ ! -e "$us/rinky/obsolete.txt" ] && pass "4: stale file removed" || fail "4: stale file still there"
grep -qx "created=extensions" "$us/rinky/.install-state" && pass "4: created=extensions survives the upgrade" || fail "4: created= lost on upgrade"
ls "$us/rinky" | grep -q '\.new' && fail "4: temporary .new files left" || pass "4: no temporary files left"

# 5. --keep-data
R=$(fresh t5)
run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "5: install failed"
us=$R/mnt/us
echo 'language = "da"' >"$us/rinky/settings.toml"
echo '{}' >"$us/Books/a.sdr/rinky.json"
run "$R" sh "$us/rinky/uninstall.sh" --keep-data >"$OUT/5-uninstall.txt" 2>&1 || fail "5: uninstall failed"
listing "$R" >"$OUT/5-after.txt"
[ -f "$us/rinky/settings.toml" ] && [ -f "$us/Books/a.sdr/rinky.json" ] && pass "5: settings and sidecar kept" || fail "5: data removed"
[ ! -e "$us/rinky/rinky" ] && [ ! -e "$us/extensions" ] && [ ! -e "$us/rinky/.install-state" ] && pass "5: program, KUAL entry and created extensions/ removed" || fail "5: program files left: $(ls -A "$us/rinky" | tr '\n' ' ')"

# 6. refuse while running
R=$(fresh t6)
mkdir -p "$R/run/lock" && echo $$ >"$R/run/launcher.pid"
run "$R" sh "$P/rinky/install.sh" >"$OUT/6-install.txt" 2>&1 && fail "6: install ran while 'running'" || pass "6: install refused while running"
grep -q "rinky is running" "$OUT/6-install.txt" || fail "6: message: $(cat "$OUT/6-install.txt")"
run "$R" sh "$P/rinky/install.sh" --upgrade >"$OUT/6-upgrade.txt" 2>&1 && pass "6: --upgrade allowed while running" || fail "6: --upgrade refused"

# 7. install lock
R=$(fresh t7)
us=$R/mnt/us
run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "7: install failed"
listing "$R" >"$OUT/7-before.txt"
sleep 60 &
HOLDER=$!
mkdir "$us/rinky/.install.lock" && echo "$HOLDER $(cat /proc/sys/kernel/random/boot_id 2>/dev/null)" >"$us/rinky/.install.lock/owner"
run "$R" sh "$P/rinky/install.sh" >"$OUT/7-install-locked.txt" 2>&1 && fail "7: install ran while another held the lock" || pass "7: install refused while another install holds the lock"
grep -q "another install or update is running (pid $HOLDER)" "$OUT/7-install-locked.txt" || fail "7: message: $(cat "$OUT/7-install-locked.txt")"
run "$R" sh "$us/rinky/uninstall.sh" >"$OUT/7-uninstall-locked.txt" 2>&1 && fail "7: uninstall ran during an install" || pass "7: uninstall refused during an install"
rm -rf "$us/rinky/.install.lock"
echo "$HOLDER" >"$us/rinky/.update.lock"
run "$R" sh "$us/rinky/uninstall.sh" >"$OUT/7-uninstall-update-locked.txt" 2>&1 && fail "7: uninstall ran during an update" \
  || { grep -q "an update is running (pid $HOLDER)" "$OUT/7-uninstall-update-locked.txt" && pass "7: uninstall refused while .update.lock names a live process"; } \
  || fail "7: message: $(cat "$OUT/7-uninstall-update-locked.txt")"
rm -f "$us/rinky/.update.lock"
kill "$HOLDER" 2>/dev/null
wait "$HOLDER" 2>/dev/null
listing "$R" >"$OUT/7-after-refusals.txt"
diff -u "$OUT/7-before.txt" "$OUT/7-after-refusals.txt" >"$OUT/7-diff.txt" && pass "7: refusals changed nothing" || fail "7: files changed (7-diff.txt)"
# A first install that holds the lock and has created empty folders so far; a second
# install that loses the lock must leave them alone.
R2=$(fresh t7b)
us2=$R2/mnt/us
sleep 60 &
HOLDER=$!
# The holder has just made its lock (no owner line yet). The second install starts, finds
# the lock and waits 1 s for the owner line; meanwhile the holder creates extensions/ and
# writes its owner line. (Before cycle 2 the second install noted "extensions/ is new"
# before it looked at the lock, and removed the holder's folders on its way out.)
mkdir -p "$us2/rinky/.install.lock"
run "$R2" sh "$P/rinky/install.sh" >"$OUT/7-loser.txt" 2>&1 &
LOSER=$!
sleep 0.3
mkdir -p "$us2/extensions/rinky/bin" && echo "$HOLDER $(cat /proc/sys/kernel/random/boot_id 2>/dev/null)" >"$us2/rinky/.install.lock/owner"
wait "$LOSER" && fail "7: second install ran while the first held the lock"
grep -q "another install or update is running (pid $HOLDER)" "$OUT/7-loser.txt" || fail "7: loser message: $(cat "$OUT/7-loser.txt")"
[ -d "$us2/extensions/rinky/bin" ] && [ -d "$us2/rinky/.install.lock" ] && pass "7: the refused install kept the lock holder's empty folders" \
  || fail "7: the refused install removed the lock holder's folders: $(cd "$us2" && find . | tr '\n' ' ')"
kill "$HOLDER" 2>/dev/null
wait "$HOLDER" 2>/dev/null
mkdir "$us/rinky/.install.lock" && echo "999999 stale-boot" >"$us/rinky/.install.lock/owner"
run "$R" sh "$P/rinky/install.sh" >"$OUT/7-install-stale.txt" 2>&1 && grep -q "removed a stale install lock (999999 stale-boot)" "$OUT/7-install-stale.txt" \
  && pass "7: stale lock taken over" || fail "7: stale lock: $(cat "$OUT/7-install-stale.txt")"
[ ! -e "$us/rinky/.install.lock" ] && ! ls -d "$us"/rinky/.install.lock.stale* >/dev/null 2>&1 && pass "7: lock removed after the install" || fail "7: lock left"

# 8. USB-style install, first start through rinky-ctl.sh
R=$(fresh t8)
us=$R/mnt/us
mkdir -p "$us/Books/empty.sdr" && echo e >"$us/Books/empty.epub"
listing "$R" >"$OUT/8-before.txt"
# An older rinky (without .install-sdr) ran before and created old.sdr.
mkdir -p "$us/Books/old.sdr" && echo '{}' >"$us/Books/old.sdr/rinky.json"
(cd "$P" && cp -R rinky "$us/" && mkdir -p "$us/extensions" && cp -R extensions/rinky "$us/extensions/")
rm -rf "$us/rinky/.install-state"
printf '#!/bin/sh\necho "stub launcher $*"\n' >"$us/rinky/rinky.sh"
run "$R" sh "$us/rinky/rinky-ctl.sh" start >"$OUT/8-start.txt" 2>&1 || fail "8: ctl start: $(cat "$OUT/8-start.txt")"
cp "$us/rinky/.install-sdr" "$OUT/8-install-sdr.txt" 2>/dev/null
grep -qx "Books/empty.sdr" "$us/rinky/.install-sdr" 2>/dev/null && grep -qx "Books/a.sdr" "$us/rinky/.install-sdr" \
  && ! grep -qx "Books/old.sdr" "$us/rinky/.install-sdr" \
  && pass "8: first start recorded the existing .sdr folders, not old.sdr (holds rinky.json)" || fail "8: .install-sdr: $(cat "$us/rinky/.install-sdr" 2>&1)"
echo '{}' >"$us/Books/empty.sdr/rinky.json"
mkdir -p "$us/Books/b c.sdr" && echo '{}' >"$us/Books/b c.sdr/rinky.json"
run "$R" sh "$us/rinky/uninstall.sh" >"$OUT/8-uninstall.txt" 2>&1 || fail "8: uninstall failed"
rmdir "$us/extensions" 2>/dev/null # created by hand in this test, as the guide says
listing "$R" >"$OUT/8-after.txt"
diff -u "$OUT/8-before.txt" "$OUT/8-after.txt" >"$OUT/8-diff.txt" && pass "8: listing == before (empty.sdr kept, b c.sdr and old.sdr removed)" || fail "8: listing differs (8-diff.txt)"

# 9. failed copy
R=$(fresh t9)
us=$R/mnt/us
listing "$R" >"$OUT/9-before-first.txt"
mkdir -p "$WORK/cpfail"
cat >"$WORK/cpfail/cp" <<'EOF'
#!/bin/sh
case "$1" in */rinky/rinky) echo "cp: write error: No space left on device" >&2; exit 1 ;; esac
for c in /bin/cp /usr/bin/cp; do [ -x "$c" ] && exec "$c" "$@"; done
exit 127
EOF
chmod 755 "$WORK/cpfail/cp"
PATH=$WORK/cpfail:$PATH run "$R" sh "$P/rinky/install.sh" >"$OUT/9-first-install.txt" 2>&1 && fail "9: first install did not fail" || pass "9: first install failed at the binary: $(tail -n 1 "$OUT/9-first-install.txt")"
listing "$R" >"$OUT/9-after-first.txt"
diff -u "$OUT/9-before-first.txt" "$OUT/9-after-first.txt" >"$OUT/9-diff-first.txt" && pass "9: failed first install left nothing (no rinky/, no extensions/)" || fail "9: failed first install left files (9-diff-first.txt)"
run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "9: install"
echo 'language = "da"' >"$us/rinky/settings.toml"
listing "$R" >"$OUT/9-before-upgrade.txt"
PATH=$WORK/cpfail:$PATH run "$R" sh "$P/rinky/install.sh" --upgrade >"$OUT/9-upgrade.txt" 2>&1 && fail "9: upgrade did not fail" || pass "9: upgrade failed at the binary"
grep -q "nothing was changed" "$OUT/9-upgrade.txt" || fail "9: message: $(tail -n 1 "$OUT/9-upgrade.txt")"
listing "$R" >"$OUT/9-after-upgrade.txt"
diff -u "$OUT/9-before-upgrade.txt" "$OUT/9-after-upgrade.txt" >"$OUT/9-diff-upgrade.txt" && pass "9: failed upgrade changed nothing (no .new files, no lock)" || fail "9: failed upgrade changed files (9-diff-upgrade.txt)"

log "$fails failure(s); evidence in $OUT"
exit "$fails"
