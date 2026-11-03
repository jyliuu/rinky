#!/bin/bash
# Regenerates the crengine expected-xpointer files from the fixtures:
#   crates/ink-html/tests/crengine/dump-all.sh [EPUB...]
# Needs a KOReader desktop build (KO_BUILD, KO_DATA; defaults point to the local checkout).
# juliet.epub keeps every 7th word so its file stays small.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../../.." && pwd)
KO_BUILD=${KO_BUILD:-$ROOT/../koreader/base/build/arm64-apple-darwin25.5.0-debug}
CACHE=${CRE_CACHE:-$ROOT/target/cre-cache}
mkdir -p "$CACHE"
books=("$@")
if [ ${#books[@]} -eq 0 ]; then
  books=("$ROOT"/crates/ink-html/tests/fixtures/*.epub "$ROOT/testdata/juliet.epub")
fi
for b in "${books[@]}"; do
  b=$(cd "$(dirname "$b")" && pwd)/$(basename "$b")
  name=$(basename "$b" .epub)
  nth=1
  [ "$name" = juliet ] && nth=7
  (cd "$CACHE" && KO_BUILD=$KO_BUILD INK_FONTS="$ROOT/fonts" CRE_CACHE="$CACHE" \
     "$KO_BUILD/luajit" "$HERE/dump.lua" "$b" "$HERE/$name.json" "$nth")
done
