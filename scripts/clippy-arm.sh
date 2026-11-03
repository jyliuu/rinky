#!/bin/bash
# cargo clippy for the Kindle target (armv7-unknown-linux-musleabihf). Plain
# `cargo clippy --target ...` has no C compiler for ring/Lua/SQLite build scripts, so this
# borrows the zig cc wrapper that `cargo zigbuild` creates (run one zigbuild first).
#   scripts/clippy-arm.sh [extra cargo clippy args]     default: --workspace --all-targets
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
CACHE=${CARGO_ZIGBUILD_CACHE:-$HOME/Library/Caches/cargo-zigbuild}
WRAP=$(find "$CACHE" -name 'zigcc-armv7-unknown-linux-musleabihf-*.sh' 2>/dev/null | head -1)
if [ -z "$WRAP" ]; then
  echo "clippy-arm: no zig cc wrapper in $CACHE; run cargo zigbuild --target armv7-unknown-linux-musleabihf once" >&2
  exit 1
fi
cd "$ROOT"
[ $# -gt 0 ] || set -- --workspace --all-targets
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)} \
CC_armv7_unknown_linux_musleabihf="$WRAP" AR_armv7_unknown_linux_musleabihf="$(command -v zig) ar" \
  cargo clippy --target armv7-unknown-linux-musleabihf "$@" -- -D warnings
