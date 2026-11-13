#!/bin/bash
# Fetches the dynamic loader shipped in the package: musl's ld-musl-armhf.so.1 (the C
# library and the dynamic loader in one file) for ARMv7 hard-float. rinky is a
# dynamically linked musl program so that it can load native plugin libraries; the
# Kindle and the Kobo have only glibc, so the package brings its own musl and the
# launchers start rinky through it.
#
#   scripts/fetch-musl-loader.sh [--force]
#
#   --force   fetch again even when the output is up to date
#
# Prints the path of the loader: target/musl/ld-musl-armhf.so.1.
#
# Source: Alpine Linux's armv7 musl package, pinned by version and SHA-256 below (the
# package file and the loader inside it are both checked) and unpacked into a fresh,
# empty directory. Alpine builds musl for armv7-a hard-float (VFPv3-D16, Thumb-2), which
# runs on every supported Kindle and Kobo, and adds the security fixes released after
# musl 1.2.5 (Alpine's APKBUILD lists them). Building musl from its release tarball
# with zig does not work: zig links its own compiler runtime into the library, whose
# hidden copies of the math and string functions (expf, memcpy, ...) make those
# symbols hidden in the result too.
#
# The musl version must match the one zig links rinky against (zig 0.17: musl 1.2.5).
# Alpine keeps only the latest build of a package on its mirrors: when the pinned one
# is gone, pin the current one (MUSL_APK, both SHA-256s) after checking its APKBUILD.
# packaging/musl/COPYRIGHT (musl's MIT license) is shipped as LICENSE-musl.
#
# Needs curl and tar. Environment: CARGO_TARGET_DIR (default target/).
set -euo pipefail

MUSL_APK=musl-1.2.5-r13.apk
ALPINE_PATH=alpine/v3.22/main/armv7
APK_SHA256=f6f874092f120ea418d72abf554ac114c4aa34618ae5be84dc7cb3e48e614422
LOADER_SHA256=20729cec964a7ab5cc4be6d3817ef23129cf61e7159fd501527f5d1aa8b0717b
MIRRORS="https://dl-cdn.alpinelinux.org https://mirrors.edge.kernel.org https://dl-4.alpinelinux.org"

ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUTDIR=${CARGO_TARGET_DIR:-$ROOT/target}/musl
OUT=$OUTDIR/ld-musl-armhf.so.1
APK=$OUTDIR/$MUSL_APK
FORCE=0

die() { echo "fetch-musl-loader: error: $*" >&2; exit 1; }
log() { echo "fetch-musl-loader: $*" >&2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --force) FORCE=1 ;;
    -h|--help) sed -n '2,29p' "$0"; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
  shift
done

sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
hash_of() { sha256 "$1" | cut -d' ' -f1; }

if [ "$FORCE" = 0 ] && [ -f "$OUT" ] && [ "$(hash_of "$OUT")" = "$LOADER_SHA256" ]; then
  log "$MUSL_APK loader is up to date ($OUT)"
  echo "$OUT"
  exit 0
fi

mkdir -p "$OUTDIR"
if [ ! -f "$APK" ] || [ "$(hash_of "$APK")" != "$APK_SHA256" ]; then
  rm -f "$APK"
  for m in $MIRRORS; do
    log "downloading $m/$ALPINE_PATH/$MUSL_APK"
    rm -f "$APK.part"
    if curl -fsSL --proto '=https' --tlsv1.2 -o "$APK.part" "$m/$ALPINE_PATH/$MUSL_APK"; then
      mv "$APK.part" "$APK"
      break
    fi
  done
  rm -f "$APK.part"
  [ -f "$APK" ] || die "cannot download $MUSL_APK from any mirror (pin a current build, see the header)"
fi
got=$(hash_of "$APK")
if [ "$got" != "$APK_SHA256" ]; then
  rm -f "$APK"
  die "SHA-256 mismatch for $MUSL_APK: got $got, want $APK_SHA256"
fi

# An .apk is gzip streams of tar segments (signature, metadata, files); bsdtar reads
# them as one, GNU tar needs --ignore-zeros to read past the first segment's end.
WORK=$OUTDIR/unpack
rm -rf "$WORK"
mkdir -p "$WORK"
TARFLAGS=
tar --version 2>/dev/null | grep -q 'GNU tar' && TARFLAGS=--ignore-zeros
# shellcheck disable=SC2086 # TARFLAGS is empty or one flag
tar -xzf "$APK" $TARFLAGS -C "$WORK" --no-same-owner --no-same-permissions lib/ld-musl-armhf.so.1 2>/dev/null ||
  [ -f "$WORK/lib/ld-musl-armhf.so.1" ] || die "$MUSL_APK holds no lib/ld-musl-armhf.so.1"
[ -f "$WORK/lib/ld-musl-armhf.so.1" ] && [ ! -L "$WORK/lib/ld-musl-armhf.so.1" ] || die "lib/ld-musl-armhf.so.1 is not a regular file"
got=$(hash_of "$WORK/lib/ld-musl-armhf.so.1")
[ "$got" = "$LOADER_SHA256" ] || die "SHA-256 mismatch for the loader in $MUSL_APK: got $got, want $LOADER_SHA256"

cp "$WORK/lib/ld-musl-armhf.so.1" "$OUT.tmp"
chmod 755 "$OUT.tmp"
mv -f "$OUT.tmp" "$OUT"
rm -rf "$WORK"
log "fetched $OUT ($(wc -c <"$OUT" | tr -d ' ') bytes)"
echo "$OUT"
