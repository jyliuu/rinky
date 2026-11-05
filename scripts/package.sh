#!/bin/bash
# Builds the rinky install package for the Kindle (one .tar.gz).
#
#   scripts/package.sh [--allow-dirty] [--no-build] [--version V] [--out DIR] [--profile P]
#
#   --allow-dirty   package a tree with uncommitted changes (VERSION then says dirty=yes);
#                   without it the script refuses, so a release always matches a commit
#   --no-build      use the ARM binaries already in the target directory (rinky and
#                   target/dropbear/dropbearmulti)
#   --version V     version written into the package (default: [workspace.package]
#                   version in Cargo.toml). For test releases of the in-app update only.
#   --out DIR       output directory (default out/package)
#   --profile P     cargo profile of the binary (default release; deploy: quick builds
#                   for your own device, see scripts/deploy.sh)
#
# Output, in DIR:
#   rinky-V-kindlehf.tar.gz          the install package (SSH install, in-app update)
#   rinky-V-kindlehf.tar.gz.sha256   its SHA-256 (sha256sum format)
#   rinky-V-kindle.zip               the same files as a zip, for copying over USB
#   SHA256SUMS                       both lines; upload it to the GitHub release next
#                                    to the packages (the in-app update needs it)
#
# Package layout (mirrors /mnt/us on the Kindle):
#   rinky/              -> /mnt/us/rinky
#     rinky             static ARMv7 musl binary
#     rinky.sh, rinky-watchdog.sh   framework-stop launcher and watchdog
#     rinky-ctl.sh      start / stop (Exit to Kindle) / status
#     install.sh, uninstall.sh
#     VERSION, FILES (every file of the package), SHA256SUMS (of every other file)
#     README.txt, user-guide.md, LICENSE, LICENSE-Noto, LICENSE-dropbear
#     bin/dropbearmulti     static ARMv7 musl dropbear (SSH server, dropbearkey, scp) for
#                           the ssh plugin; built by scripts/build-dropbear.sh
#     bin/scp               wrapper running dropbearmulti scp (dropbear's session PATH
#                           ends in /mnt/us/rinky/bin; vfat has no symlinks)
#     fonts/NotoSans-Regular.ttf   UI fallback face (IPA in dictionary pronunciations; OFL,
#                                  LICENSE-Noto); also listed as the family "Noto Sans"
#   extensions/rinky/   -> /mnt/us/extensions/rinky (KUAL menu)
#     config.xml, menu.json, bin/kual.sh
#
# Build: cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p ink-app
# (CARGO_BUILD_JOBS defaults to all cores; CARGO_TARGET_DIR is honoured), then
# scripts/build-dropbear.sh (skipped when target/dropbear/dropbearmulti is up to date).
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
TARGET=armv7-unknown-linux-musleabihf
ALLOW_DIRTY=0
BUILD=1
VERSION=
OUT=$ROOT/out/package

die() { echo "package: error: $*" >&2; exit 1; }
log() { echo "package: $*" >&2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --allow-dirty) ALLOW_DIRTY=1 ;;
    --no-build) BUILD=0 ;;
    --version) VERSION=${2:?--version needs a value}; shift ;;
    --out) OUT=${2:?--out needs a value}; shift ;;
    --profile) PROFILE=${2:?--profile needs a value}; shift ;;
    -h|--help) sed -n '2,41p' "$0"; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
  shift
done

cd "$ROOT"

# Source state.
COMMIT=$(git rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
DIRTY=no
if [ -n "$(git status --porcelain --untracked-files=normal 2>/dev/null)" ]; then
  DIRTY=yes
  [ "$ALLOW_DIRTY" = 1 ] || die "the tree has uncommitted changes (git status); commit them or pass --allow-dirty"
  log "warning: packaging a dirty tree"
fi

if [ -z "$VERSION" ]; then
  VERSION=$(awk '/^\[workspace\.package\]/ {s = 1; next} /^\[/ {s = 0} s && $1 == "version" {gsub(/"/, "", $3); print $3; exit}' Cargo.toml)
fi
case "$VERSION" in
  ''|*[!A-Za-z0-9._-]*) die "bad version '$VERSION'" ;;
esac
NAME=rinky-$VERSION-kindlehf
log "rinky $VERSION (commit $COMMIT, dirty=$DIRTY)"

# Build.
TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/target}
PROFILE=${PROFILE:-release}
case "$PROFILE" in ''|*[!A-Za-z0-9_-]*) die "bad profile '$PROFILE'" ;; esac
BIN=$TARGET_DIR/$TARGET/$PROFILE/rinky
DROPBEAR=$TARGET_DIR/dropbear/dropbearmulti
if [ "$BUILD" = 1 ]; then
  log "building $TARGET, profile $PROFILE (cargo zigbuild, CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)})"
  # The binary keeps source paths for panic messages. Remap the home folder and this
  # checkout so a package carries no user name or folder of the build machine. These
  # flags replace .cargo/config.toml's rustflags, so its CPU flags are repeated here.
  sep=$(printf '\037')
  CARGO_ENCODED_RUSTFLAGS="-Ctarget-cpu=cortex-a9${sep}-Ctarget-feature=+neon${sep}--remap-path-prefix=$HOME=~${sep}--remap-path-prefix=$ROOT=rinky" \
    CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)} cargo zigbuild --profile "$PROFILE" --target "$TARGET" -p ink-app >&2
  scripts/build-dropbear.sh >&2
fi
for b in "$BIN" "$DROPBEAR"; do
  [ -f "$b" ] || die "$b is missing (build first or drop --no-build)"
  info=$(file -b "$b")
  case "$info" in
    *ELF\ 32-bit*ARM*) ;;
    *) die "$b is not a 32-bit ARM ELF: $info" ;;
  esac
  case "$info" in
    *statically\ linked*|*static-pie\ linked*) ;;
    *) die "$b is not statically linked: $info" ;;
  esac
done
# No build-machine paths in the package (see the remap above).
for b in "$BIN" "$DROPBEAR"; do
  if LC_ALL=C grep -a -q -F "$HOME/" "$b" || LC_ALL=C grep -a -q -F "$ROOT/" "$b"; then
    die "$b contains paths of this machine ($HOME); rebuild without --no-build"
  fi
done

# Stage.
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/rinky-package.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
P=$STAGE/rinky
E=$STAGE/extensions/rinky
mkdir -p "$P" "$E/bin"

install -m 755 "$BIN" "$P/rinky"
install -m 755 scripts/rinky.sh "$P/rinky.sh"
install -m 755 scripts/rinky-watchdog.sh "$P/rinky-watchdog.sh"
install -m 755 packaging/rinky-ctl.sh "$P/rinky-ctl.sh"
install -m 755 packaging/install.sh "$P/install.sh"
install -m 755 packaging/uninstall.sh "$P/uninstall.sh"
install -m 644 packaging/README.txt "$P/README.txt"
install -m 644 docs/user-guide.md "$P/user-guide.md"
install -m 644 LICENSE "$P/LICENSE"
install -m 644 fonts/LICENSE-Noto "$P/LICENSE-Noto"
install -m 644 packaging/dropbear/LICENSE "$P/LICENSE-dropbear"
mkdir -p "$P/bin"
install -m 755 "$DROPBEAR" "$P/bin/dropbearmulti"
install -m 755 packaging/dropbear/scp "$P/bin/scp"
mkdir -p "$P/fonts"
install -m 644 fonts/ui/NotoSans-Regular.ttf "$P/fonts/NotoSans-Regular.ttf"
install -m 644 packaging/kual/rinky/menu.json "$E/menu.json"
sed "s/@VERSION@/$VERSION/" packaging/kual/rinky/config.xml >"$E/config.xml"
chmod 644 "$E/config.xml"
install -m 755 packaging/kual/rinky/bin/kual.sh "$E/bin/kual.sh"

cat >"$P/VERSION" <<EOF
version=$VERSION
commit=$COMMIT
dirty=$DIRTY
built=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
target=$TARGET
profile=$PROFILE
EOF

sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# FILES: every file in the package (sorted); SHA256SUMS: every file but itself.
(cd "$STAGE" && { find rinky extensions -type f; echo rinky/FILES; echo rinky/SHA256SUMS; } | LC_ALL=C sort -u >rinky/FILES)
(cd "$STAGE" && grep -vx 'rinky/SHA256SUMS' rinky/FILES | while IFS= read -r f; do sha256 "$f"; done >rinky/SHA256SUMS)

# Archive: plain ustar, root-owned, no macOS metadata (AppleDouble ._ files, xattrs),
# which busybox tar on the Kindle and the in-app updater both read.
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
PKG=$OUT/$NAME.tar.gz
rm -f "$PKG" "$PKG.sha256"
LIST=$(cd "$STAGE" && { find rinky extensions -type d; cat rinky/FILES; } | LC_ALL=C sort -u)
if tar --version 2>/dev/null | grep -q bsdtar; then
  (cd "$STAGE" && echo "$LIST" | COPYFILE_DISABLE=1 tar --format ustar --uid 0 --gid 0 --uname root --gname root \
      --no-xattrs --no-mac-metadata --no-recursion -czf "$PKG" -T -)
else
  (cd "$STAGE" && echo "$LIST" | tar --format=ustar --owner=0 --group=0 --numeric-owner --no-recursion \
      -czf "$PKG" -T -)
fi

# Self-check: the archive lists exactly the staged files, no ._ entries.
listed=$(tar -tzf "$PKG" | grep -v '/$' | LC_ALL=C sort)
[ "$listed" = "$(LC_ALL=C sort "$STAGE/rinky/FILES")" ] || die "archive contents differ from FILES"
if tar -tzf "$PKG" | grep -q '\(^\|/\)\._'; then die "archive contains AppleDouble ._ files"; fi

# Zip of the same tree for USB installs (Windows and macOS open zips without tools).
ZIP=$OUT/rinky-$VERSION-kindle.zip
rm -f "$ZIP"
(cd "$STAGE" && LC_ALL=C sort rinky/FILES | TZ=UTC zip -q -X -D "$ZIP" -@)

(cd "$OUT" && sha256 "$NAME.tar.gz" >"$NAME.tar.gz.sha256" && { cat "$NAME.tar.gz.sha256"; sha256 "$(basename "$ZIP")"; } >SHA256SUMS)
log "$(wc -l <"$STAGE/rinky/FILES" | tr -d ' ') files, $(du -k "$PKG" | cut -f1) KiB"
log "sha256 $(head -1 "$OUT/SHA256SUMS" | cut -d' ' -f1)"
echo "$PKG"
