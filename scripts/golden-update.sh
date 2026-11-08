#!/bin/bash
# Regenerate the golden images and keep a diff image for every one that changed:
#   - page content: tests/golden/*.png       (crates/ink-html/tests/golden.rs;
#                                              RGB diff, changed pixels red)
#   - reader footer: crates/ink-app/tests/golden/footer-*.png
#                                             (crates/ink-app/tests/footer_golden.rs;
#                                              bottom band only, changed pixels black)
#   - settings page and popup: crates/ink-app/tests/golden/settings-*.png
#                                             (crates/ink-app/tests/settings_golden.rs;
#                                              day and night, changed pixels black)
#   - reader end card: crates/ink-app/tests/golden/end-card*.png
#                                             (crates/ink-app/tests/end_card_golden.rs;
#                                              changed pixels black)
#   scripts/golden-update.sh [DIFF_DIR]      default DIFF_DIR: target/golden-diff
# DIFF_DIR must be new or empty (e.g. out/evidence/<key>/golden-diff): a directory
# that holds other files is refused before anything runs, never emptied. Only the
# default directory under target/ is cleared first.
# Review the diff images, then commit the goldens together with the change that
# caused them, and list the diff images as evidence.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEFAULT_OUT=$ROOT/target/golden-diff
OUT=${1:-$DEFAULT_OUT}
if [ "$OUT" = "$DEFAULT_OUT" ]; then
  rm -rf "$OUT"
elif [ -e "$OUT" ] && { [ ! -d "$OUT" ] || [ -n "$(ls -A "$OUT")" ]; }; then
  echo "golden-update: $OUT exists and is not an empty directory; pass a new or empty" >&2
  echo "golden-update: directory (for example $OUT/golden-diff). Nothing was changed." >&2
  exit 2
fi
mkdir -p "$OUT"
cd "$ROOT"
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)} INK_GOLDEN_UPDATE=1 INK_GOLDEN_OUT="$OUT"
cargo test -p ink-html --test golden -- --nocapture golden_pages
cargo test -p ink-app --test footer_golden -- --nocapture footer_band_goldens
cargo test -p ink-app --test settings_golden -- --nocapture settings_component_goldens
cargo test -p ink-app --test end_card_golden -- --nocapture end_card_goldens
echo "diff images:"
ls "$OUT"/*.diff.png 2>/dev/null || echo "  (none: no existing golden changed)"
echo "golden files changed in git:"
git -C "$ROOT" status --short tests/golden crates/ink-app/tests/golden
