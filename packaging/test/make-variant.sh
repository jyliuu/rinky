#!/bin/bash
# Verifier helper: a copy of an install package whose binary and launcher differ (a
# marker line appended; the ARM binary still runs), with SHA256SUMS regenerated the way
# scripts/package.sh writes it. Same version and file list. With it, an install that
# mixes two packages shows up in `sha256sum -c rinky/SHA256SUMS`.
#
#   packaging/test/make-variant.sh SRC.tar.gz OUT.tar.gz
set -euo pipefail
SRC=${1:?usage: make-variant.sh SRC.tar.gz OUT.tar.gz}
DST=${2:?usage: make-variant.sh SRC.tar.gz OUT.tar.gz}
mkdir -p "$(dirname "$DST")"
DST=$(cd "$(dirname "$DST")" && pwd)/$(basename "$DST")
W=$(mktemp -d "${TMPDIR:-/tmp}/rinky-variant.XXXXXX")
trap 'rm -rf "$W"' EXIT
tar -xzf "$SRC" -C "$W"
printf '\nverifier-variant\n' >>"$W/rinky/rinky"
printf '\n# verifier-variant\n' >>"$W/rinky/rinky.sh"
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
(cd "$W" && grep -vx 'rinky/SHA256SUMS' rinky/FILES | while IFS= read -r f; do sha "$f"; done >rinky/SHA256SUMS.tmp)
mv "$W/rinky/SHA256SUMS.tmp" "$W/rinky/SHA256SUMS"
LIST=$(cd "$W" && { find rinky extensions -type d; cat rinky/FILES; } | LC_ALL=C sort -u)
rm -f "$DST"
if tar --version 2>/dev/null | grep -q bsdtar; then
  (cd "$W" && echo "$LIST" | COPYFILE_DISABLE=1 tar --format ustar --uid 0 --gid 0 --uname root --gname root \
      --no-xattrs --no-mac-metadata --no-recursion -czf "$DST" -T -)
else
  (cd "$W" && echo "$LIST" | tar --format=ustar --owner=0 --group=0 --numeric-owner --no-recursion -czf "$DST" -T -)
fi
echo "$DST"
