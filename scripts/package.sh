#!/bin/bash
# Builds the rinky install packages for the Kindle and the Kobo (one binary, two
# layouts).
#
#   scripts/package.sh [--allow-dirty] [--no-build] [--version V] [--out DIR] [--profile P]
#
#   --allow-dirty   package a tree with uncommitted changes (VERSION then says dirty=yes);
#                   without it the script refuses, so a release always matches a commit
#   --no-build      use the ARM binaries already in the target directory (rinky,
#                   target/dropbear/dropbearmulti and target/musl/ld-musl-armhf.so.1)
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
#   rinky-V-kindlehf.kpkg (.sha256)  the same Kindle files as a KPM package, for
#                                    `kpm install` on a Kindle that has KPM instead of
#                                    KUAL (no KUAL needed; see docs/install.md)
#   rinky-V-kobo.tar.gz (.sha256)    the Kobo package (same binary; Kobo launcher,
#                                    NickelMenu entry nm/rinky)
#   rinky-V-kobo.zip                 the Kobo files laid out as the drive (.adds/rinky,
#                                    .adds/nm/rinky), for copying over USB
#   SHA256SUMS                       one line per package; upload it to the GitHub
#                                    release next to them (the in-app update needs it)
#
# Kindle package layout (mirrors /mnt/us on the Kindle):
#   rinky/              -> /mnt/us/rinky
#     rinky             ARMv7 musl binary, dynamically linked (native plugins are shared
#                       libraries); its interpreter is set to lib/ld-musl-armhf.so.1 below
#     lib/ld-musl-armhf.so.1   musl's C library and dynamic loader (the Kindle has only
#                       glibc); the launchers run rinky through it. Fetched by
#                       scripts/fetch-musl-loader.sh; LICENSE-musl
#     rinky.sh, rinky-watchdog.sh   framework-stop launcher and watchdog
#     rinky-ctl.sh      start / stop (Exit to Kindle) / status
#     install.sh, uninstall.sh
#     VERSION, FILES (every file of the package), SHA256SUMS (of every other file)
#     README.txt, install.md, user-guide.md, LICENSE, LICENSE-Noto, LICENSE-dropbear,
#     LICENSE-musl
#     bin/dropbearmulti     static ARMv7 musl dropbear (SSH server, dropbearkey, scp) for
#                           the ssh plugin; built by scripts/build-dropbear.sh
#     bin/scp               wrapper running dropbearmulti scp (dropbear's session PATH
#                           ends in /mnt/us/rinky/bin; vfat has no symlinks)
#     fonts/NotoSans-Regular.ttf   UI fallback face (IPA in dictionary pronunciations; OFL,
#                                  LICENSE-Noto); also listed as the family "Noto Sans"
#   extensions/rinky/   -> /mnt/us/extensions/rinky (KUAL menu)
#     config.xml, menu.json, bin/kual.sh
#
# Kobo package layout:
#   rinky/              -> /mnt/onboard/.adds/rinky: the same files without the Kindle
#                          launcher scripts and dropbear; rinky.sh is the Kobo launcher;
#                          the binary's interpreter is the Kobo path of lib/
#   nm/rinky            -> /mnt/onboard/.adds/nm/rinky (NickelMenu entry)
#
# Build: cargo zigbuild --release --target armv7-unknown-linux-musleabihf -p ink-app
# (CARGO_BUILD_JOBS defaults to all cores; CARGO_TARGET_DIR is honoured), then
# scripts/build-dropbear.sh (skipped when target/dropbear/dropbearmulti is up to date) and
# scripts/fetch-musl-loader.sh (target/musl/ld-musl-armhf.so.1, pinned and checked).
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
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
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
log "rinky $VERSION (commit $COMMIT, dirty=$DIRTY)"

# Build.
TARGET_DIR=${CARGO_TARGET_DIR:-$ROOT/target}
PROFILE=${PROFILE:-release}
case "$PROFILE" in ''|*[!A-Za-z0-9_-]*) die "bad profile '$PROFILE'" ;; esac
BIN=$TARGET_DIR/$TARGET/$PROFILE/rinky
DROPBEAR=$TARGET_DIR/dropbear/dropbearmulti
LOADER=$TARGET_DIR/musl/ld-musl-armhf.so.1
# Interpreter paths written into each package's binary (scripts/set-interp.py).
KINDLE_INTERP=/mnt/us/rinky/lib/ld-musl-armhf.so.1
KOBO_INTERP=/mnt/onboard/.adds/rinky/lib/ld-musl-armhf.so.1
if [ "$BUILD" = 1 ]; then
  log "building $TARGET, profile $PROFILE (cargo zigbuild, CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)})"
  # The binary keeps source paths for panic messages. Remap the home folder and this
  # checkout so a package carries no user name or folder of the build machine. These
  # flags replace .cargo/config.toml's rustflags, so its CPU and linking flags are
  # repeated here.
  sep=$(printf '\037')
  flags="-Ctarget-cpu=cortex-a9${sep}-Ctarget-feature=+neon${sep}-Ctarget-feature=-crt-static${sep}-Clink-arg=-dynamic"
  flags="$flags${sep}-Clink-arg=-Wl,--dynamic-linker=/mnt/us/rinky/lib////////////ld-musl-armhf.so.1"
  CARGO_ENCODED_RUSTFLAGS="$flags${sep}--remap-path-prefix=$HOME=~${sep}--remap-path-prefix=$ROOT=rinky" \
    CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)} cargo zigbuild --profile "$PROFILE" --target "$TARGET" -p ink-app >&2
  scripts/build-dropbear.sh >&2
  scripts/fetch-musl-loader.sh >/dev/null
fi
[ -f "$LOADER" ] || die "$LOADER is missing (scripts/fetch-musl-loader.sh, or drop --no-build)"
case "$(file -b "$LOADER")" in
  *ELF\ 32-bit*shared\ object*ARM*) ;;
  *) die "$LOADER is not a 32-bit ARM shared object" ;;
esac
# rinky: dynamically linked, needing only musl's libc (the loader we ship).
[ -f "$BIN" ] || die "$BIN is missing (build first or drop --no-build)"
info=$(file -b "$BIN")
case "$info" in
  *ELF\ 32-bit*ARM*dynamically\ linked*ld-musl-armhf.so.1*) ;;
  *) die "$BIN is not a dynamically linked musl binary (rebuild without --no-build): $info" ;;
esac
needed=$(LC_ALL=C python3 -I - "$BIN" <<'PY'
import struct, sys
d = open(sys.argv[1], "rb").read()
phoff, = struct.unpack_from("<I", d, 28); phentsize, phnum = struct.unpack_from("<HH", d, 42)
segs = [struct.unpack_from("<IIIII", d, phoff + i * phentsize) for i in range(phnum)]
def off(vaddr):
    for t, o, v, _, fs in segs:
        if t == 1 and v <= vaddr < v + fs:
            return vaddr - v + o
dyn = next(s for s in segs if s[0] == 2)
ents = [struct.unpack_from("<iI", d, dyn[1] + 8 * i) for i in range(dyn[4] // 8)]
strtab = off(next(v for t, v in ents if t == 5))
print(" ".join(d[strtab + v:d.index(b"\0", strtab + v)].decode() for t, v in ents if t == 1))
PY
)
[ "$needed" = "libc.so" ] || die "$BIN needs libraries other than musl's libc.so: $needed"
for b in "$DROPBEAR"; do
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
for b in "$BIN" "$DROPBEAR" "$LOADER"; do
  if LC_ALL=C grep -a -q -F "$HOME/" "$b" || LC_ALL=C grep -a -q -F "$ROOT/" "$b"; then
    die "$b contains paths of this machine ($HOME); rebuild without --no-build"
  fi
done

# Stage, seal and archive one package per platform. Both carry the same binary, but for
# the path of the loader written into it.
STAGE=$(mktemp -d "${TMPDIR:-/tmp}/rinky-package.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)

sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }

# common ROOT: the files every package has in ROOT/rinky.
common() {
  local p=$1/rinky
  mkdir -p "$p/fonts"
  install -m 755 "$BIN" "$p/rinky"
  mkdir -p "$p/lib"
  install -m 755 "$LOADER" "$p/lib/ld-musl-armhf.so.1"
  install -m 644 packaging/musl/COPYRIGHT "$p/LICENSE-musl"
  install -m 755 packaging/install.sh "$p/install.sh"
  install -m 644 packaging/README.txt "$p/README.txt"
  install -m 644 docs/user-guide.md "$p/user-guide.md"
  install -m 644 docs/install.md "$p/install.md"
  install -m 644 LICENSE "$p/LICENSE"
  install -m 644 fonts/LICENSE-Noto "$p/LICENSE-Noto"
  install -m 644 fonts/ui/NotoSans-Regular.ttf "$p/fonts/NotoSans-Regular.ttf"
  printf '%s\n' "version=$VERSION" "commit=$COMMIT" "dirty=$DIRTY" "built=$(date -u '+%Y-%m-%dT%H:%M:%SZ')" \
    "target=$TARGET" "profile=$PROFILE" >"$p/VERSION"
}

# seal ROOT DIR...: rinky/FILES lists every file under the DIRs (sorted, itself and
# SHA256SUMS included); rinky/SHA256SUMS has every file but itself.
seal() {
  local root=$1; shift
  (cd "$root" && { find "$@" -type f; echo rinky/FILES; echo rinky/SHA256SUMS; } | LC_ALL=C sort -u >rinky/FILES)
  (cd "$root" && grep -vx 'rinky/SHA256SUMS' rinky/FILES | while IFS= read -r f; do sha256 "$f"; done >rinky/SHA256SUMS)
}

# archive ROOT TAR DIR...: plain ustar, root-owned, no macOS metadata (AppleDouble ._
# files, xattrs), which busybox tar and the in-app updater both read; then checks the
# archive lists exactly rinky/FILES, and writes TAR.sha256.
archive() {
  local root=$1 tar=$2 list; shift 2
  list=$(cd "$root" && { find "$@" -type d; cat rinky/FILES; } | LC_ALL=C sort -u)
  rm -f "$tar" "$tar.sha256"
  if tar --version 2>/dev/null | grep -q bsdtar; then
    (cd "$root" && echo "$list" | COPYFILE_DISABLE=1 tar --format ustar --uid 0 --gid 0 --uname root --gname root \
        --no-xattrs --no-mac-metadata --no-recursion -czf "$tar" -T -)
  else
    (cd "$root" && echo "$list" | tar --format=ustar --owner=0 --group=0 --numeric-owner --no-recursion -czf "$tar" -T -)
  fi
  [ "$(tar -tzf "$tar" | grep -v '/$' | LC_ALL=C sort)" = "$(LC_ALL=C sort "$root/rinky/FILES")" ] ||
    die "$(basename "$tar"): archive contents differ from FILES"
  if tar -tzf "$tar" | grep -q '\(^\|/\)\._'; then die "$(basename "$tar") contains AppleDouble ._ files"; fi
  (cd "$OUT" && sha256 "$(basename "$tar")" >"$(basename "$tar").sha256")
}

# zipdir ROOT ZIP: every file under ROOT, for copying over USB (Windows and macOS open
# zips without tools).
zipdir() {
  rm -f "$2"
  (cd "$1" && find . -type f | sed 's|^\./||' | LC_ALL=C sort | TZ=UTC zip -q -X -D "$2" -@)
}

# Kindle: rinky/ -> /mnt/us/rinky, extensions/rinky/ (the KUAL menu) -> /mnt/us/extensions.
K=$STAGE/kindle
common "$K"
python3 -I scripts/set-interp.py "$K/rinky/rinky" "$KINDLE_INTERP"
install -m 755 scripts/rinky.sh "$K/rinky/rinky.sh"
install -m 755 scripts/rinky-watchdog.sh "$K/rinky/rinky-watchdog.sh"
install -m 755 packaging/rinky-ctl.sh "$K/rinky/rinky-ctl.sh"
install -m 755 packaging/uninstall.sh "$K/rinky/uninstall.sh"
install -m 644 packaging/dropbear/LICENSE "$K/rinky/LICENSE-dropbear"
mkdir -p "$K/rinky/bin" "$K/extensions/rinky/bin"
install -m 755 "$DROPBEAR" "$K/rinky/bin/dropbearmulti"
install -m 755 packaging/dropbear/scp "$K/rinky/bin/scp"
install -m 644 packaging/kual/rinky/menu.json "$K/extensions/rinky/menu.json"
sed "s/@VERSION@/$VERSION/" packaging/kual/rinky/config.xml >"$K/extensions/rinky/config.xml"
chmod 644 "$K/extensions/rinky/config.xml"
install -m 755 packaging/kual/rinky/bin/kual.sh "$K/extensions/rinky/bin/kual.sh"
seal "$K" rinky extensions
PKG=$OUT/rinky-$VERSION-kindlehf.tar.gz
archive "$K" "$PKG" rinky extensions
zipdir "$K" "$OUT/rinky-$VERSION-kindle.zip"

# KPM: the same Kindle tree, plus KPM's manifest and its three hooks, as a .kpkg (a
# tar.gz KPM unpacks and runs). KPM installs a package by running install.sh in the
# unpacked tree, keeps the tree for launch.sh and calls uninstall.sh to remove it; the
# library entry is still created by rinky/install.sh itself, so KPM is only a
# distribution channel here, not a second launcher. KPM has no dependency on KUAL, which
# is why devices that use KPM alone (the Scribe, for one) can install this.
M=$STAGE/kpm
mkdir -p "$M"
cp -R "$K/rinky" "$K/extensions" "$M/"
install -m 755 packaging/kpm/install.sh "$M/install.sh"
install -m 755 packaging/kpm/launch.sh "$M/launch.sh"
install -m 755 packaging/kpm/uninstall.sh "$M/uninstall.sh"
install -m 644 packaging/kpm/manifest.json "$M/manifest.json"
# KPM wants the version as a [major, minor, patch] array, so it is filled in here from
# the same VERSION the rest of the package uses.
python3 -I - "$M/manifest.json" "$VERSION" <<'PY'
import json, re, sys

path, version = sys.argv[1], sys.argv[2]
nums = [int(part) for part in re.findall(r"\d+", version)][:3]
nums += [0] * (3 - len(nums))
with open(path) as f:
    manifest = json.load(f)
manifest["version"] = nums
with open(path, "w") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
PY
KP=$OUT/rinky-$VERSION-kindlehf.kpkg
rm -f "$KP" "$KP.sha256"
if tar --version 2>/dev/null | grep -q bsdtar; then
  (cd "$M" && COPYFILE_DISABLE=1 tar --format ustar --uid 0 --gid 0 --uname root --gname root \
      --no-xattrs --no-mac-metadata -czf "$KP" .)
else
  (cd "$M" && tar --format=ustar --owner=0 --group=0 --numeric-owner -czf "$KP" .)
fi
if tar -tzf "$KP" | grep -q '\(^\|/\)\._'; then die "$(basename "$KP") contains AppleDouble ._ files"; fi
for f in manifest.json install.sh launch.sh uninstall.sh rinky/rinky rinky/FILES; do
  tar -tzf "$KP" | grep -qx "\./$f" || die "$(basename "$KP") is missing $f"
done
(cd "$OUT" && sha256 "$(basename "$KP")" >"$(basename "$KP").sha256")

# A ready-made KPM repository manifest pointing at the .kpkg next to it. Upload both as
# release assets and `kpm add-repo <manifest URL>` installs rinky; KPM resolves the
# relative URL against the manifest's own URL, which is exactly how a release asset is
# served. A stable repo URL (GitHub Pages) needs only the artifact and version lines
# updated per release.
REPO=$OUT/rinky-$VERSION-kpm-repo.json
sed -e "s/rinky-VERSION-kindlehf.kpkg/$(basename "$KP")/" packaging/kpm/repo.json >"$REPO"
python3 -I - "$REPO" "$VERSION" <<'PY'
import json, re, sys

path, version = sys.argv[1], sys.argv[2]
nums = [int(part) for part in re.findall(r"\d+", version)][:3]
nums += [0] * (3 - len(nums))
with open(path) as f:
    repo = json.load(f)
repo["packages"]["rinky"]["artifacts"][0]["version"] = nums
with open(path, "w") as f:
    json.dump(repo, f, indent=2)
    f.write("\n")
PY

# Kobo: rinky/ -> /mnt/onboard/.adds/rinky, nm/rinky (the NickelMenu entry) ->
# /mnt/onboard/.adds/nm/rinky; install.sh tells the packages apart by nm/rinky. The
# zip is laid out as the Kobo drive: .adds/rinky and .adds/nm/rinky.
B=$STAGE/kobo
common "$B"
python3 -I scripts/set-interp.py "$B/rinky/rinky" "$KOBO_INTERP"
install -m 755 packaging/kobo/rinky.sh "$B/rinky/rinky.sh"
mkdir -p "$B/nm" "$STAGE/kobo-drive/.adds"
install -m 644 packaging/kobo/nm-rinky "$B/nm/rinky"
seal "$B" rinky nm
KPKG=$OUT/rinky-$VERSION-kobo.tar.gz
archive "$B" "$KPKG" rinky nm
cp -R "$B/rinky" "$B/nm" "$STAGE/kobo-drive/.adds/"
zipdir "$STAGE/kobo-drive" "$OUT/rinky-$VERSION-kobo.zip"

(cd "$OUT" && cat "$(basename "$PKG").sha256" "$(basename "$KP").sha256" "$(basename "$KPKG").sha256" >SHA256SUMS &&
  sha256 "rinky-$VERSION-kindle.zip" "rinky-$VERSION-kobo.zip" >>SHA256SUMS)
for d in "$K" "$B"; do log "$(basename "$d"): $(wc -l <"$d/rinky/FILES" | tr -d ' ') files"; done
log "KPM: $(basename "$KP") ($(wc -c <"$KP" | tr -d ' ') bytes)"
echo "$PKG"
echo "$KP"
echo "$KPKG"
