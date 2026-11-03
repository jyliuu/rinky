#!/bin/bash
# Builds the SSH server shipped in the package: a static ARMv7 musl dropbearmulti
# (dropbear, dropbearkey, scp), cross-compiled on the Mac with zig as the C compiler.
#
#   scripts/build-dropbear.sh [--force]
#
#   --force   rebuild even when the output is up to date
#
# Output: target/dropbear/dropbearmulti (stripped). Source and build tree:
# target/dropbear/<version>/. The build is skipped when the output exists and its stamp
# (target/dropbear/dropbearmulti.stamp) matches the pinned version, the tarball SHA-256,
# the patches, localoptions.h and this script.
#
# Inputs (packaging/dropbear/):
#   localoptions.h   compile-time options (copied into the source tree)
#   *.patch          applied in name order with patch -p1; each starts with a comment
#                    saying what it does and where it comes from
#   LICENSE          dropbear's license (copied from the release tarball), shipped in
#                    the package as LICENSE-dropbear
#
# The tarball comes from https://matt.ucc.asn.au/dropbear/releases/ and is checked
# against the SHA-256 pinned below (also in the release's SHA256SUM.asc) before it is
# unpacked into a fresh, empty directory. To update dropbear: change DROPBEAR_VERSION
# and DROPBEAR_SHA256, rebase the patches, compare LICENSE with the new tarball's.
#
# Needs zig (0.17), curl, patch, make. Environment: CARGO_TARGET_DIR (default target/),
# DROPBEAR_JOBS (default: all cores).
set -euo pipefail

DROPBEAR_VERSION=2026.94
DROPBEAR_SHA256=e098034a843699200c8c977a991fff73159735bf795d5f72ef672c41a6b1ae81
DROPBEAR_URL=https://matt.ucc.asn.au/dropbear/releases/dropbear-$DROPBEAR_VERSION.tar.bz2

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PKGDIR=$ROOT/packaging/dropbear
OUTDIR=${CARGO_TARGET_DIR:-$ROOT/target}/dropbear
OUT=$OUTDIR/dropbearmulti
STAMP=$OUT.stamp
WORK=$OUTDIR/$DROPBEAR_VERSION
TARBALL=$OUTDIR/dropbear-$DROPBEAR_VERSION.tar.bz2
ZIGTARGET="-target arm-linux-musleabihf -mcpu=cortex_a7"
FORCE=0

die() { echo "build-dropbear: error: $*" >&2; exit 1; }
log() { echo "build-dropbear: $*" >&2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --force) FORCE=1 ;;
    -h|--help) sed -n '2,27p' "$0"; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
  shift
done

sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
command -v zig >/dev/null || die "zig is not installed"

shopt -s nullglob
PATCHES=("$PKGDIR"/*.patch)
shopt -u nullglob
[ -f "$PKGDIR/localoptions.h" ] || die "$PKGDIR/localoptions.h is missing"

# Stamp: everything that changes the binary.
want=$({
  echo "version $DROPBEAR_VERSION $DROPBEAR_SHA256"
  echo "zig $(zig version) $ZIGTARGET"
  (cd "$ROOT" && sha256 scripts/build-dropbear.sh packaging/dropbear/localoptions.h)
  for p in ${PATCHES[@]+"${PATCHES[@]}"}; do (cd "$ROOT" && sha256 "${p#"$ROOT"/}"); done
})
if [ "$FORCE" = 0 ] && [ -f "$OUT" ] && [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$want" ]; then
  log "dropbear $DROPBEAR_VERSION is up to date ($OUT)"
  exit 0
fi

mkdir -p "$OUTDIR"

# Download (kept between builds) and verify before anything reads the archive.
if [ ! -f "$TARBALL" ] || [ "$(sha256 "$TARBALL" | cut -d' ' -f1)" != "$DROPBEAR_SHA256" ]; then
  log "downloading $DROPBEAR_URL"
  rm -f "$TARBALL" "$TARBALL.part"
  curl -fsSL --proto '=https' --tlsv1.2 -o "$TARBALL.part" "$DROPBEAR_URL" || die "download failed"
  mv "$TARBALL.part" "$TARBALL"
fi
got=$(sha256 "$TARBALL" | cut -d' ' -f1)
if [ "$got" != "$DROPBEAR_SHA256" ]; then
  rm -f "$TARBALL"
  die "SHA-256 mismatch for $TARBALL: got $got, want $DROPBEAR_SHA256"
fi

# Unpack into a fresh, empty directory; expect exactly one top-level dropbear-V/.
rm -rf "$WORK"
mkdir -p "$WORK/unpack"
tar -xjf "$TARBALL" -C "$WORK/unpack" --no-same-owner --no-same-permissions
entries=$(ls -A "$WORK/unpack")
[ "$entries" = "dropbear-$DROPBEAR_VERSION" ] || die "unexpected tarball contents: $entries"
SRC=$WORK/unpack/dropbear-$DROPBEAR_VERSION
[ -f "$SRC/configure" ] || die "$SRC/configure is missing"
cmp -s "$SRC/LICENSE" "$PKGDIR/LICENSE" ||
  die "the tarball's LICENSE differs from packaging/dropbear/LICENSE; review it and copy it over"

for p in ${PATCHES[@]+"${PATCHES[@]}"}; do
  log "patch $(basename "$p")"
  patch -d "$SRC" -p1 --forward --batch --quiet <"$p" || die "patch $(basename "$p") does not apply"
done
cp "$PKGDIR/localoptions.h" "$SRC/localoptions.h"

# Configure and build in the source tree (dropbear's Makefile looks for
# ./localoptions.h in the build directory).
JOBS=${DROPBEAR_JOBS:-$(sysctl -n hw.ncpu 2>/dev/null || nproc 2>/dev/null || echo 4)}
log "configure + make dropbear $DROPBEAR_VERSION for arm-linux-musleabihf (zig cc, $JOBS jobs)"
(
  cd "$SRC"
  export CC="zig cc $ZIGTARGET" AR="zig ar" RANLIB="zig ranlib"
  export CFLAGS="-Os -ffunction-sections -fdata-sections" LDFLAGS="-Wl,--gc-sections -s"
  ./configure --host=arm-linux-musleabihf --enable-static --enable-bundled-libtom \
    --disable-zlib --disable-pam --disable-lastlog --disable-utmp --disable-utmpx \
    --disable-wtmp --disable-wtmpx --disable-loginfunc --disable-pututline --disable-pututxline \
    >"$WORK/configure.log" 2>&1 || { tail -n 30 "$WORK/configure.log" >&2; die "configure failed (log: $WORK/configure.log)"; }
  make -j"$JOBS" PROGRAMS="dropbear dropbearkey scp" MULTI=1 STATIC=1 \
    >"$WORK/make.log" 2>&1 || { tail -n 30 "$WORK/make.log" >&2; die "make failed (log: $WORK/make.log)"; }
)
[ -f "$SRC/dropbearmulti" ] || die "make did not produce dropbearmulti"

info=$(file -b "$SRC/dropbearmulti")
case "$info" in
  *ELF\ 32-bit*ARM*) ;;
  *) die "dropbearmulti is not a 32-bit ARM ELF: $info" ;;
esac
case "$info" in
  *statically\ linked*|*static-pie\ linked*) ;;
  *) die "dropbearmulti is not statically linked: $info" ;;
esac

case "$info" in
  *not\ stripped*) die "dropbearmulti is not stripped (the link uses -s): $info" ;;
esac

cp "$SRC/dropbearmulti" "$OUT.tmp"
chmod 755 "$OUT.tmp"
mv -f "$OUT.tmp" "$OUT"
echo "$want" >"$STAMP"
log "built $OUT ($(wc -c <"$OUT" | tr -d ' ') bytes)"
