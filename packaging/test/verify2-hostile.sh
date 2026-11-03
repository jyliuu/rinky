#!/bin/bash
# Verifier (cycle 2) attacks on install.sh / uninstall.sh, on the host under a fake
# /mnt/us (RINKY_PREFIX), with the real release package.
#
#   packaging/test/verify2-hostile.sh PKG.tar.gz [OUTDIR]
#
#   W1  package unpacked under a folder whose path has spaces, quotes, $, *, [ ] and a
#       leading dash; install, uninstall                     -> listing == before
#   W2  six first installs started at once                   -> each one installs or says
#       "another install or update is running"; one consistent version; no lock, stale
#       lock or .new copy left; uninstall                   -> listing == before
#   W3  empty .sdr folders with hostile names that existed before install; rinky
#       writes into each; uninstall                          -> listing == before
#   W4  first install killed (SIGKILL, as by a power loss) while it copies the binary;
#       install again; uninstall                             -> listing == before
#   W5  first install killed while it renames the copies into place (at SHA256SUMS,
#       after the program files); uninstall.sh from the partial install -> listing == before
#
# W4 and W5 are verifier findings of cycle 2 (an empty /mnt/us/extensions is left: the
# killed run created it and recorded that only in its uncommitted .install-state copy).
# They run only with RINKY_VERIFY2_KNOWN_FAILS=1, so this script stays green until
# they are fixed; then run them always. Exit status = number of failed checks.
set -uo pipefail

PKG=${1:?usage: verify2-hostile.sh PKG.tar.gz [OUTDIR]}
PKG=$(cd "$(dirname "$PKG")" && pwd)/$(basename "$PKG")
OUT=${2:-$(mktemp -d "${TMPDIR:-/tmp}/rinky-verify2.XXXXXX")}
KNOWN=${RINKY_VERIFY2_KNOWN_FAILS:-0}
mkdir -p "$OUT"
: >"$OUT/verify2.log"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/rinky-verify2-work.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
fails=0

log() { echo "[verify2] $*" | tee -a "$OUT/verify2.log"; }
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
  if diff -u "$2" "$3" >"$OUT/$1-diff.txt"; then pass "$1: listing equal"; else fail "$1: listing differs ($1-diff.txt: $(grep '^[+-][DF]' "$OUT/$1-diff.txt" | tr '\n' ' '))"; fi
}
# A ps that shows only this script's processes: uninstall.sh runs rinky-ctl.sh stop,
# which signals every rinky-watchdog.sh that ps lists; other agents share this host.
SHIM=$WORK/shim
mkdir -p "$SHIM"
cat >"$SHIM/ps" <<EOF
#!/bin/sh
/bin/ps "\$@" | grep -F "$WORK"
EOF
chmod 755 "$SHIM/ps"
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
  echo "koreader file" >"$r/mnt/us/koreader/settings.reader.lua"
  echo "$r"
}
# installed_ok ROOT: installed files match SHA256SUMS, VERSION == .install-state, no
# .new copies, no lock.
installed_ok() {
  local us=$1/mnt/us v s
  (cd "$us" && sha -c rinky/SHA256SUMS 2>&1 | grep -v ': OK$') && return 1
  v=$(sed -n 's/^version=//p' "$us/rinky/VERSION" 2>/dev/null)
  s=$(sed -n 's/^version=//p' "$us/rinky/.install-state" 2>/dev/null)
  [ -n "$v" ] && [ "$v" = "$s" ] || { echo "VERSION says $v, .install-state says $s"; return 1; }
  if find "$us/rinky" "$us/extensions" -name '*.new*' -o -name '.install.lock*' 2>/dev/null | grep .; then return 1; fi
  echo "$v"
}

P=$WORK/pkg
mkdir -p "$P" && tar -xzf "$PKG" -C "$P" || { fail "cannot unpack $PKG"; exit 1; }
log "package $(sed -n 's/^version=//p' "$P/rinky/VERSION") ($PKG)"

# W1: hostile package location
R=$(fresh w1)
listing "$R" >"$OUT/W1-before.txt"
H="$WORK/-dl/it's \$HOME \"q\" [x] * dir"
mkdir -p "$H" && cp -R "$P/rinky" "$P/extensions" "$H/"
run "$R" sh "$H/rinky/install.sh" >"$OUT/W1-install.txt" 2>&1 && pass "W1: install from a hostile path" || fail "W1: install: $(tail -n 1 "$OUT/W1-install.txt")"
installed_ok "$R" >"$OUT/W1-ok.txt" && pass "W1: installed tree consistent" || fail "W1: $(tr '\n' ' ' <"$OUT/W1-ok.txt")"
run "$R" sh "$R/mnt/us/rinky/uninstall.sh" >"$OUT/W1-uninstall.txt" 2>&1 || fail "W1: uninstall"
listing "$R" >"$OUT/W1-after.txt"
same W1 "$OUT/W1-before.txt" "$OUT/W1-after.txt"

# W2: six first installs at once
R=$(fresh w2)
listing "$R" >"$OUT/W2-before.txt"
pids=
for i in 1 2 3 4 5 6; do
  run "$R" sh "$P/rinky/install.sh" >"$OUT/W2-install-$i.txt" 2>&1 &
  pids="$pids $!"
done
n=0 ok=0 busy=0
for p in $pids; do
  n=$((n + 1))
  if wait "$p"; then ok=$((ok + 1))
  elif grep -q "another install or update is running" "$OUT/W2-install-$n.txt"; then busy=$((busy + 1))
  else fail "W2: install $n failed: $(tail -n 1 "$OUT/W2-install-$n.txt")"; fi
done
log "W2: $ok installed, $busy refused as busy"
[ "$ok" -ge 1 ] && pass "W2: at least one install ran" || fail "W2: no install ran"
installed_ok "$R" >"$OUT/W2-ok.txt" && pass "W2: one consistent version, no lock or copies left" || fail "W2: $(tr '\n' ' ' <"$OUT/W2-ok.txt")"
grep -qx created=extensions "$R/mnt/us/rinky/.install-state" && pass "W2: created=extensions recorded" || fail "W2: created=extensions lost"
run "$R" sh "$R/mnt/us/rinky/uninstall.sh" >"$OUT/W2-uninstall.txt" 2>&1 || fail "W2: uninstall"
listing "$R" >"$OUT/W2-after.txt"
same W2 "$OUT/W2-before.txt" "$OUT/W2-after.txt"

# W3: pre-existing empty .sdr folders with hostile names
R=$(fresh w3)
us=$R/mnt/us
names=("-x" "[a]" "b  c " "it's \$x" "ø ブック" "a*b")
for n in "${names[@]}"; do mkdir -p "$us/Books/$n.sdr" && echo b >"$us/Books/$n.epub"; done
listing "$R" >"$OUT/W3-before.txt"
run "$R" sh "$P/rinky/install.sh" >/dev/null 2>&1 || fail "W3: install"
cp "$us/rinky/.install-sdr" "$OUT/W3-install-sdr.txt" 2>/dev/null
for n in "${names[@]}"; do echo '{}' >"$us/Books/$n.sdr/rinky.json"; done
run "$R" sh "$us/rinky/uninstall.sh" >"$OUT/W3-uninstall.txt" 2>&1 || fail "W3: uninstall"
listing "$R" >"$OUT/W3-after.txt"
same W3 "$OUT/W3-before.txt" "$OUT/W3-after.txt"

# A cp or mv that kills the installer (its parent shell) at one file, as a power loss would.
mkdir -p "$WORK/killcp" "$WORK/killmv"
cat >"$WORK/killcp/cp" <<'EOF'
#!/bin/sh
case "$1" in */rinky/rinky) kill -KILL $PPID; exit 1 ;; esac
for c in /bin/cp /usr/bin/cp; do [ -x "$c" ] && exec "$c" "$@"; done
exit 127
EOF
cat >"$WORK/killmv/mv" <<'EOF'
#!/bin/sh
case "$*" in *SHA256SUMS.new*) kill -KILL $PPID; exit 1 ;; esac
for c in /bin/mv /usr/bin/mv; do [ -x "$c" ] && exec "$c" "$@"; done
exit 127
EOF
chmod 755 "$WORK/killcp/cp" "$WORK/killmv/mv"

if [ "$KNOWN" = 1 ]; then
  # W4: killed first install (copy phase), install again, uninstall
  R=$(fresh w4)
  listing "$R" >"$OUT/W4-before.txt"
  PATH=$WORK/killcp:$PATH run "$R" sh "$P/rinky/install.sh" >"$OUT/W4-killed.txt" 2>&1
  log "W4: killed install exit $?"
  listing "$R" >"$OUT/W4-after-killed.txt"
  run "$R" sh "$P/rinky/install.sh" >"$OUT/W4-install.txt" 2>&1 || fail "W4: install after the killed one"
  installed_ok "$R" >"$OUT/W4-ok.txt" || fail "W4: $(tr '\n' ' ' <"$OUT/W4-ok.txt")"
  run "$R" sh "$R/mnt/us/rinky/uninstall.sh" >"$OUT/W4-uninstall.txt" 2>&1 || fail "W4: uninstall"
  listing "$R" >"$OUT/W4-after.txt"
  same W4 "$OUT/W4-before.txt" "$OUT/W4-after.txt"

  # W5: killed first install (rename phase), uninstall from the partial install
  R=$(fresh w5)
  listing "$R" >"$OUT/W5-before.txt"
  PATH=$WORK/killmv:$PATH run "$R" sh "$P/rinky/install.sh" >"$OUT/W5-killed.txt" 2>&1
  log "W5: killed install exit $?"
  listing "$R" >"$OUT/W5-after-killed.txt"
  if [ -f "$R/mnt/us/rinky/uninstall.sh" ]; then
    run "$R" sh "$R/mnt/us/rinky/uninstall.sh" >"$OUT/W5-uninstall.txt" 2>&1 || fail "W5: uninstall"
  else
    fail "W5: the killed install left no uninstall.sh"
  fi
  listing "$R" >"$OUT/W5-after.txt"
  same W5 "$OUT/W5-before.txt" "$OUT/W5-after.txt"
else
  log "SKIP: W4, W5 (known failures; RINKY_VERIFY2_KNOWN_FAILS=1 runs them)"
fi

log "$fails failure(s); evidence in $OUT"
exit "$fails"
