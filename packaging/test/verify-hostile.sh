#!/bin/bash
# Verifier (cycle 1) attacks on install.sh / uninstall.sh / rinky-ctl.sh, on the host
# under a fake /mnt/us (RINKY_PREFIX), with the real packages.
#
#   packaging/test/verify-hostile.sh PKG.tar.gz PKG_NEW.tar.gz [OUTDIR]
#
# PKG is the release package (scripts/package.sh), PKG_NEW a later one
# (scripts/package.sh --no-build --version 0.2.1 --out out/package-test). From PKG_NEW
# this script makes a variant whose binary and launcher differ from PKG, so a mixed
# install is visible in the checksums.
#
#   V1  install twice, uninstall                       -> listing == before
#   V2  uninstall twice (second from the package copy) -> exit 0, listing == before
#   V3  --keep-data, reinstall, full uninstall         -> listing == before
#   V4  books named with quotes, $, a leading -, spaces and non-ASCII; use; uninstall
#                                                     -> listing == before
#   V5  uninstall while rinky runs (fake launcher, rinky and watchdog processes)
#                                                     -> processes gone, listing == before
#   V6  install (no --upgrade) while rinky runs   -> refused, nothing changed
#   V7  damaged copy (binary truncated)               -> refused, nothing changed
#   V8  no space while the upgrade copies the binary (a cp that fails for that file)
#                                                     -> installed files still one version
#   V9  a .sdr folder that existed (empty) before install -> still there after uninstall
#
# V8 and V9 were verifier findings in cycle 1 and are fixed; they always run now
# (RINKY_VERIFY_KNOWN_FAILS is no longer needed). Exit status = number of failed
# checks.
set -uo pipefail

PKG=${1:?usage: verify-hostile.sh PKG.tar.gz PKG_NEW.tar.gz [OUTDIR]}
PKG_NEW=${2:?usage: verify-hostile.sh PKG.tar.gz PKG_NEW.tar.gz [OUTDIR]}
PKG=$(cd "$(dirname "$PKG")" && pwd)/$(basename "$PKG")
PKG_NEW=$(cd "$(dirname "$PKG_NEW")" && pwd)/$(basename "$PKG_NEW")
OUT=${3:-$(mktemp -d "${TMPDIR:-/tmp}/rinky-verify.XXXXXX")}
KNOWN=1
mkdir -p "$OUT"
: >"$OUT/verify.log"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/rinky-verify-work.XXXXXX")
PIDS=
cleanup() {
  for p in $PIDS; do kill -KILL "$p" 2>/dev/null; done
  rm -rf "$WORK"
}
trap cleanup EXIT
fails=0

log() { echo "[verify] $*" | tee -a "$OUT/verify.log"; }
pass() { log "PASS: $*"; }
fail() { log "FAIL: $*"; fails=$((fails + 1)); }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# listing ROOT: type, path, size, sha256 per path under ROOT/mnt/us.
listing() {
  (cd "$1" && find mnt/us | LC_ALL=C sort | while IFS= read -r f; do
    if [ -d "$f" ]; then echo "D $f"; else echo "F $f $(wc -c <"$f" | tr -d ' ') $(sha <"$f" | cut -d' ' -f1)"; fi
  done)
}
same() { # same LABEL A B: pass when the listings are equal
  if diff -u "$2" "$3" >"$OUT/$1-diff.txt"; then pass "$1: listing equal"; else fail "$1: listing differs ($1-diff.txt)"; fi
}

# A ps that shows only this script's processes, so rinky-ctl.sh stop never signals a
# watchdog of anyone else on this machine; and a cp that fails for one source path.
SHIM=$WORK/shim
mkdir -p "$SHIM" "$WORK/cpfail"
cat >"$SHIM/ps" <<EOF
#!/bin/sh
/bin/ps "\$@" | grep -F "$WORK"
EOF
cat >"$WORK/cpfail/cp" <<'EOF'
#!/bin/sh
case "$1" in
  */rinky/rinky) echo "cp: write error: No space left on device" >&2; exit 1 ;;
esac
for c in /bin/cp /usr/bin/cp; do [ -x "$c" ] && exec "$c" "$@"; done
exit 127
EOF
cp "$SHIM/ps" "$WORK/cpfail/ps"
chmod 755 "$SHIM/ps" "$WORK/cpfail/cp" "$WORK/cpfail/ps"

run() { # run ROOT CMD...: fake prefix, runtime dir and the ps shim
  local r=$1
  shift
  PATH=$SHIM:$PATH RINKY_PREFIX=$r RINKY_RUN=$r/run "$@"
}

fresh() {
  local r=$WORK/$1
  rm -rf "$r"
  mkdir -p "$r/mnt/us/Books/a.sdr" "$r/mnt/us/documents" "$r/mnt/us/koreader" "$r/mnt/us/system" "$r/run"
  echo "epub a" >"$r/mnt/us/Books/a.epub"
  echo "-- koreader sidecar" >"$r/mnt/us/Books/a.sdr/metadata.epub.lua"
  echo "doc" >"$r/mnt/us/documents/doc.epub"
  echo "koreader file" >"$r/mnt/us/koreader/settings.reader.lua"
  echo "$r"
}

unpack_to() { rm -rf "$2" && mkdir -p "$2" && tar -xzf "$1" -C "$2"; }

P=$WORK/pkg
N=$WORK/pkg-new
unpack_to "$PKG" "$P" || { fail "cannot unpack $PKG"; exit 1; }
unpack_to "$PKG_NEW" "$N" || { fail "cannot unpack $PKG_NEW"; exit 1; }
# The variant: binary and launcher differ from PKG; SHA256SUMS as package.sh writes it.
printf '\nverifier-variant\n' >>"$N/rinky/rinky"
printf '\n# verifier-variant\n' >>"$N/rinky/rinky.sh"
(cd "$N" && grep -vx 'rinky/SHA256SUMS' rinky/FILES | while IFS= read -r f; do sha "$f"; done >rinky/SHA256SUMS.tmp && mv rinky/SHA256SUMS.tmp rinky/SHA256SUMS)
V_OLD=$(sed -n 's/^version=//p' "$P/rinky/VERSION")
V_NEW=$(sed -n 's/^version=//p' "$N/rinky/VERSION")
log "packages: $V_OLD ($PKG), $V_NEW variant of $PKG_NEW"

# installed_ok ROOT: the installed files match the installed SHA256SUMS, VERSION and
# .install-state agree, no .new files. Prints the reason on failure.
installed_ok() {
  local us=$1/mnt/us v s
  (cd "$us" && sha -c rinky/SHA256SUMS 2>&1 | grep -v ': OK$') && return 1
  v=$(sed -n 's/^version=//p' "$us/rinky/VERSION" 2>/dev/null)
  s=$(sed -n 's/^version=//p' "$us/rinky/.install-state" 2>/dev/null)
  [ "$v" = "$s" ] || { echo "VERSION says $v, .install-state says $s"; return 1; }
  if find "$us/rinky" "$us/extensions" -name '*.new*' 2>/dev/null | grep .; then return 1; fi
  echo "$v"
}

# V1: install twice, uninstall
R=$(fresh v1)
listing "$R" >"$OUT/V1-before.txt"
run "$R" sh "$P/rinky/install.sh" >"$OUT/V1-install1.txt" 2>&1 || fail "V1: first install"
run "$R" sh "$P/rinky/install.sh" >"$OUT/V1-install2.txt" 2>&1 || fail "V1: second install"
grep -qx created=extensions "$R/mnt/us/rinky/.install-state" && pass "V1: created=extensions kept by the second install" || fail "V1: created=extensions lost"
installed_ok "$R" >"$OUT/V1-installed-ok.txt" && pass "V1: installed tree consistent ($(cat "$OUT/V1-installed-ok.txt"))" || fail "V1: $(cat "$OUT/V1-installed-ok.txt")"
run "$R" sh "$R/mnt/us/rinky/uninstall.sh" >"$OUT/V1-uninstall.txt" 2>&1 || fail "V1: uninstall"
listing "$R" >"$OUT/V1-after.txt"
same V1 "$OUT/V1-before.txt" "$OUT/V1-after.txt"

# V2: uninstall twice
R=$(fresh v2)
listing "$R" >"$OUT/V2-before.txt"
run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "V2: install"
run "$R" sh "$R/mnt/us/rinky/uninstall.sh" >"$OUT/V2-uninstall1.txt" 2>&1 || fail "V2: first uninstall"
run "$R" sh "$P/rinky/uninstall.sh" >"$OUT/V2-uninstall2.txt" 2>&1 && pass "V2: second uninstall exits 0" || fail "V2: second uninstall exit $?"
listing "$R" >"$OUT/V2-after.txt"
same V2 "$OUT/V2-before.txt" "$OUT/V2-after.txt"

# V3: --keep-data, reinstall, full uninstall
R=$(fresh v3)
listing "$R" >"$OUT/V3-before.txt"
us=$R/mnt/us
run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "V3: install"
echo 'language = "da"' >"$us/rinky/settings.toml"
echo '{}' >"$us/Books/a.sdr/rinky.json"
mkdir -p "$us/documents/doc.sdr" && echo '{}' >"$us/documents/doc.sdr/rinky.json"
run "$R" sh "$us/rinky/uninstall.sh" --keep-data >"$OUT/V3-keep.txt" 2>&1 || fail "V3: --keep-data"
run "$R" sh "$P/rinky/install.sh" >"$OUT/V3-reinstall.txt" 2>&1 || fail "V3: reinstall"
[ "$(cat "$us/rinky/settings.toml" 2>/dev/null)" = 'language = "da"' ] && pass "V3: settings survive keep-data + reinstall" || fail "V3: settings lost"
grep -qx created=extensions "$us/rinky/.install-state" && pass "V3: reinstall records created=extensions again" || fail "V3: created=extensions not recorded after keep-data"
run "$R" sh "$us/rinky/uninstall.sh" >"$OUT/V3-uninstall.txt" 2>&1 || fail "V3: full uninstall"
listing "$R" >"$OUT/V3-after.txt"
same V3 "$OUT/V3-before.txt" "$OUT/V3-after.txt"

# V4: hostile book names
R=$(fresh v4)
us=$R/mnt/us
names=("it's \$HOME \"q\" \`x\`" "-dash" "with  two spaces " "ünïcødé ブック" "back\\slash" "semi;colon & amp")
for n in "${names[@]}"; do echo "book" >"$us/Books/$n.epub"; done
mkdir -p "$us/Books/kept.sdr" && echo "ko" >"$us/Books/kept.sdr/metadata.epub.lua" && echo b >"$us/Books/kept.epub"
listing "$R" >"$OUT/V4-before.txt"
run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "V4: install"
for n in "${names[@]}" kept; do mkdir -p "$us/Books/$n.sdr" && echo '{}' >"$us/Books/$n.sdr/rinky.json" && echo t >"$us/Books/$n.sdr/rinky.tmp123"; done
run "$R" sh "$us/rinky/uninstall.sh" >"$OUT/V4-uninstall.txt" 2>&1 || fail "V4: uninstall"
listing "$R" >"$OUT/V4-after.txt"
same V4 "$OUT/V4-before.txt" "$OUT/V4-after.txt"

# V5: uninstall while running
R=$(fresh v5)
us=$R/mnt/us
listing "$R" >"$OUT/V5-before.txt"
run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "V5: install"
mkdir -p "$WORK/v5fake" "$R/run/lock"
printf '#!/bin/sh\nsleep 120\n' >"$WORK/v5fake/rinky-watchdog.sh"
sleep 120 &
LAUNCHER=$!
sleep 120 &
INK=$!
sh "$WORK/v5fake/rinky-watchdog.sh" &
WD=$!
PIDS="$LAUNCHER $INK $WD"
echo "$LAUNCHER" >"$R/run/launcher.pid"
echo "$INK" >"$R/run/rinky.pid"
start=$(date +%s)
run "$R" sh "$us/rinky/uninstall.sh" >"$OUT/V5-uninstall.txt" 2>&1 && pass "V5: uninstall while running exits 0 ($(( $(date +%s) - start )) s)" || fail "V5: uninstall exit $?"
sleep 0.5
alive=
for p in $LAUNCHER $INK $WD; do kill -0 "$p" 2>/dev/null && alive="$alive $p"; done
[ -z "$alive" ] && pass "V5: launcher, rinky and watchdog stopped" || fail "V5: still running:$alive"
[ ! -e "$R/run" ] && pass "V5: runtime dir removed" || fail "V5: runtime dir left"
listing "$R" >"$OUT/V5-after.txt"
same V5 "$OUT/V5-before.txt" "$OUT/V5-after.txt"

# V6: install while running
R=$(fresh v6)
us=$R/mnt/us
run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "V6: install"
listing "$R" >"$OUT/V6-before.txt"
mkdir -p "$R/run/lock"
sleep 120 &
LAUNCHER=$!
PIDS="$PIDS $LAUNCHER"
echo "$LAUNCHER" >"$R/run/launcher.pid"
run "$R" sh "$N/rinky/install.sh" >"$OUT/V6-install.txt" 2>&1 && fail "V6: install ran while running" || pass "V6: install refused while running"
listing "$R" >"$OUT/V6-after.txt"
same V6 "$OUT/V6-before.txt" "$OUT/V6-after.txt"
kill "$LAUNCHER" 2>/dev/null

# V7: damaged copy
R=$(fresh v7)
listing "$R" >"$OUT/V7-before.txt"
D=$WORK/pkg-damaged
rm -rf "$D" && cp -R "$P" "$D"
head -c 100000 "$P/rinky/rinky" >"$D/rinky/rinky"
run "$R" sh "$D/rinky/install.sh" >"$OUT/V7-install.txt" 2>&1 && fail "V7: damaged package installed" || pass "V7: damaged package refused: $(tail -n 1 "$OUT/V7-install.txt")"
listing "$R" >"$OUT/V7-after.txt"
same V7 "$OUT/V7-before.txt" "$OUT/V7-after.txt"

# V8: no space while the upgrade copies the binary
if [ "$KNOWN" = 1 ]; then
  R=$(fresh v8)
  run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "V8: install $V_OLD"
  installed_ok "$R" >"$OUT/V8-before-ok.txt" || fail "V8: not consistent before: $(cat "$OUT/V8-before-ok.txt")"
  PATH=$WORK/cpfail:$PATH RINKY_PREFIX=$R RINKY_RUN=$R/run sh "$N/rinky/install.sh" --upgrade >"$OUT/V8-upgrade.txt" 2>&1
  log "V8: upgrade exit $?: $(tail -n 1 "$OUT/V8-upgrade.txt")"
  if installed_ok "$R" >"$OUT/V8-after-ok.txt"; then
    pass "V8: after the failed upgrade the install is one version ($(cat "$OUT/V8-after-ok.txt"))"
  else
    fail "V8: after the failed upgrade the install is mixed: $(tr '\n' ' ' <"$OUT/V8-after-ok.txt")"
  fi
  {
    echo "VERSION: $(sed -n 's/^version=//p' "$R/mnt/us/rinky/VERSION")"
    echo ".install-state: $(sed -n 's/^version=//p' "$R/mnt/us/rinky/.install-state")"
    echo "new binary installed (variant marker): $(grep -c verifier-variant "$R/mnt/us/rinky/rinky")"
    echo "new launcher installed (variant marker): $(grep -c verifier-variant "$R/mnt/us/rinky/rinky.sh")"
  } >"$OUT/V8-state.txt" 2>&1
else
  log "SKIP: V8 (known failure; RINKY_VERIFY_KNOWN_FAILS=1 runs it)"
fi

# V9: an empty .sdr folder that existed before install
if [ "$KNOWN" = 1 ]; then
  R=$(fresh v9)
  us=$R/mnt/us
  mkdir -p "$us/Books/e.sdr" && echo e >"$us/Books/e.epub"
  listing "$R" >"$OUT/V9-before.txt"
  run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "V9: install"
  echo '{}' >"$us/Books/e.sdr/rinky.json"
  run "$R" sh "$us/rinky/uninstall.sh" >"$OUT/V9-uninstall.txt" 2>&1 || fail "V9: uninstall"
  listing "$R" >"$OUT/V9-after.txt"
  same V9 "$OUT/V9-before.txt" "$OUT/V9-after.txt"
else
  log "SKIP: V9 (known failure; RINKY_VERIFY_KNOWN_FAILS=1 runs it)"
fi

log "$fails failure(s); evidence in $OUT"
exit "$fails"
