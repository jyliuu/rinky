#!/bin/bash
# Re-runs the host-side phase 0 acceptance checks and writes one log per step plus
# OUTDIR/steps.txt ("STEP exit=N" lines, N = the step's real exit status).
#   scripts/e2e/phase0-host.sh OUTDIR [--no-window] [--no-arm] [--only "STEP STEP..."]
# --only runs just the named steps and appends to an existing steps.txt.
# Steps (all on the host; no emulator, no network except what cargo itself fetches):
#   layout, license, workspace-deps, no-unimplemented, missing-docs, unit-tests-per-crate,
#   cargo-test, clippy-host, render + render-compare (prototype pages), zigbuild + file
#   type, clippy-arm, sim-headless, demo-headless, sim-window (minifb, scripted, closes
#   itself; --no-window skips), screens-test, golden-pass, golden-one-pixel (must fail),
#   footer-golden-pass, footer-one-pixel (must fail), golden-update-noop,
#   slot-test (scripts/e2e/slot-test.sh), slot-test-main (scripts/emu-slot-test.sh),
#   verify-slot-races, verify-golden-update-outdir, verify-lib-evidence,
#   e2e-runner-test, launcher-race-test, device-sh-no-args, arch-doc.
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${1:?usage: scripts/e2e/phase0-host.sh OUTDIR [--no-window] [--no-arm] [--only "STEP..."]}
shift
WINDOW=1 ARM=1 ONLY=
while [ $# -gt 0 ]; do
  case $1 in
    --no-window) WINDOW=0 ;;
    --no-arm) ARM=0 ;;
    --only) ONLY=" ${2:?--only needs a list} "; shift ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd -P)
cd "$ROOT" || exit 2
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
PROTO=${PROTO_PAGES:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)/rinky/out/evidence/arch/proto-pages}
STEPS="$OUT/steps.txt"
[ -n "$ONLY" ] || : >"$STEPS"
BIN="$ROOT/target/debug/rinky"

# step NAME [expect=N] -- CMD...: runs CMD, output to OUT/NAME.txt, records the exit status.
step() {
  local name=$1 expect=0 rc
  shift
  case "$ONLY" in ''|*" $name "*) ;; *) return 0 ;; esac
  case ${1:-} in expect=*) expect=${1#expect=}; shift ;; esac
  [ "${1:-}" = -- ] && shift
  { echo "\$ $*"; echo "# $(date '+%Y-%m-%d %H:%M:%S') in $ROOT ($(git rev-parse --short HEAD))"; } >"$OUT/$name.txt"
  "$@" >>"$OUT/$name.txt" 2>&1
  rc=$?
  echo "exit=$rc" >>"$OUT/$name.txt"
  if [ "$rc" = "$expect" ]; then
    echo "$name exit=$rc (expected $expect) OK" | tee -a "$STEPS"
  else
    echo "$name exit=$rc (expected $expect) BAD" | tee -a "$STEPS"
  fi
}

layout() {
  local c bad=0
  for c in ink-device ink-text ink-ui ink-doc ink-html ink-formats ink-mupdf ink-store ink-net ink-plugin ink-app; do
    if [ -f "crates/$c/Cargo.toml" ] && [ -f "crates/$c/src/lib.rs" ]; then echo "ok crates/$c"; else echo "MISSING crates/$c"; bad=1; fi
  done
  for p in plugins tests/golden tests/e2e tests/sim docs crates/ink-app/src/screens/reader crates/ink-app/src/screens/library \
           crates/ink-app/src/screens/settings crates/ink-app/src/plugin_host.rs crates/ink-app/src/plugins.rs; do
    if [ -e "$p" ]; then echo "ok $p"; else echo "MISSING $p"; bad=1; fi
  done
  grep -q '^\[\[bin\]\]' crates/ink-app/Cargo.toml && grep -q 'name = "rinky"' crates/ink-app/Cargo.toml && echo "ok binary rinky" || bad=1
  return $bad
}

license() {
  local bad=0 f
  grep -n '^license = "AGPL-3.0-or-later"' Cargo.toml || bad=1
  for f in crates/*/Cargo.toml plugins/*/Cargo.toml; do
    [ -f "$f" ] || continue
    if grep -qE '^license(\.workspace)? *= *(true|"AGPL-3.0-or-later")' "$f"; then echo "ok $f"; else echo "BAD $f: $(grep -n license "$f")"; bad=1; fi
  done
  head -3 LICENSE
  grep -q "GNU AFFERO GENERAL PUBLIC LICENSE" LICENSE && grep -q "Version 3, 19 November 2007" LICENSE || { echo "LICENSE is not the AGPL v3 text"; bad=1; }
  grep -n "AGPL" README.md | head -3 || { echo "README does not state the license"; bad=1; }
  return $bad
}

workspace_deps() {
  local bad=0 d
  for d in html5ever markup5ever_rcdom cssparser selectors rustybuzz ttf-parser fontdue unicode-linebreak \
           unicode-segmentation hyphenation image resvg usvg tiny-skia zip quick-xml png md-5 serde serde_json \
           toml encoding_rs chardetng mlua ureq webpki-roots minifb rusqlite crossbeam-channel log \
           anyhow thiserror libc; do
    if sed -n '/^\[workspace.dependencies\]/,/^\[/p' Cargo.toml | grep -qE "^$d *="; then echo "ok $d"; else echo "MISSING $d"; bad=1; fi
  done
  # rustls is not a direct dependency: ureq's "rustls" feature brings it in.
  if grep -qE '^ureq = .*features = \[[^]]*"rustls"' crates/*/Cargo.toml && grep -q '^name = "rustls"$' Cargo.lock; then
    echo "ok rustls (through ureq's rustls feature: $(grep -lE '^ureq = .*"rustls"' crates/*/Cargo.toml | tr '\n' ' '))"
  else echo "MISSING rustls"; bad=1; fi
  return $bad
}

no_unimplemented() { ! grep -rnE 'unimplemented!|todo!\(' crates plugins --include=*.rs; }

missing_docs() {
  local bad=0 f
  for f in crates/*/src/lib.rs; do
    if grep -qE '#!\[(warn|deny)\(missing_docs\)\]' "$f"; then echo "ok $f"; else echo "NO missing_docs lint: $f"; bad=1; fi
  done
  return $bad
}

# Every crate's own unit tests run and are > 0 (from cargo-test.txt).
unit_tests_per_crate() {
  awk '/Running unittests src\/lib.rs/ { match($0, /deps\/[a-z_]+-/); c = substr($0, RSTART + 5, RLENGTH - 6); next }
       c != "" && /^running [0-9]+ tests?/ { print c, $2; c = "" }' "$OUT/cargo-test.txt" | sort -u >"$OUT/unit-test-counts.txt"
  cat "$OUT/unit-test-counts.txt"
  local bad=0 c n
  for c in ink_device ink_text ink_ui ink_doc ink_html ink_formats ink_mupdf ink_store ink_net ink_plugin ink_app; do
    n=$(awk -v c="$c" '$1 == c { print $2 }' "$OUT/unit-test-counts.txt")
    if [ "${n:-0}" -gt 0 ]; then echo "ok $c: $n unit tests"; else echo "NO unit tests ran for $c"; bad=1; fi
  done
  grep -E "^test result:" "$OUT/cargo-test.txt" | grep -v " 0 failed" && bad=1
  return $bad
}

file_type() {
  local b=target/armv7-unknown-linux-musleabihf/release/rinky
  ls -la "$b" && file "$b" | tee /dev/stderr | grep -q "ELF 32-bit LSB.*ARM.*statically linked"
}

# One pixel of the first page golden flipped in a copy: the golden test must fail.
golden_one_pixel() {
  local m="$OUT/golden-mutated"
  rm -rf "$m" "$OUT/golden-one-pixel-out"; mkdir -p "$m"; cp tests/golden/*.png "$m/"
  python3 -I -c 'import sys; from PIL import Image
p = sys.argv[1]; im = Image.open(p); px = im.load(); v = px[600, 800]
px[600, 800] = (255 - v) if isinstance(v, int) else tuple(255 - x for x in v); im.save(p)
print("flipped (600,800) of", p, "from", v)' "$(ls "$m"/*.png | head -1)"
  INK_GOLDEN_DIR="$m" INK_GOLDEN_OUT="$OUT/golden-one-pixel-out" cargo test -p ink-html --test golden golden_pages
}

footer_one_pixel() {
  local f g
  f=$(ls crates/ink-app/tests/golden/footer-3-of-208.png)
  g="$OUT/footer-orig.png"; cp "$f" "$g"
  python3 -I -c 'import sys; from PIL import Image
p = sys.argv[1]; im = Image.open(p); px = im.load(); w, h = im.size; v = px[w // 2, h // 2]
px[w // 2, h // 2] = (255 - v) if isinstance(v, int) else tuple(255 - x for x in v); im.save(p)
print("flipped", (w // 2, h // 2), "of", p, "from", v)' "$f"
  cargo test -p ink-app --test footer_golden footer_band_goldens; local rc=$?
  cp "$g" "$f"; git diff --quiet -- "$f" && echo "golden restored (git diff clean)"
  return $rc
}

golden_update_noop() {
  scripts/golden-update.sh "$OUT/golden-update-diff" || return 1
  git status --short tests/golden crates/ink-app/tests/golden | grep . && { echo "golden-update changed files"; return 1; }
  echo "no golden changed"
}

sim_window() {
  # minifb window driven by the script; it quits at the script's end. perl alarm: never
  # leave a window open.
  rm -rf "$OUT/sim-window" testdata/*.sdr; mkdir -p "$OUT/sim-window"
  perl -e 'alarm 120; exec @ARGV' "$BIN" sim testdata/juliet.epub --script tests/sim/turn-pages.txt --shots "$OUT/sim-window"
  local rc=$?
  ls "$OUT/sim-window"
  return $rc
}

device_sh_no_args() { ! bash scripts/device.sh; }   # must refuse: no default address

# The sections of docs/ARCHITECTURE.md that other docs and AGENTS.md point to.
ARCH_HEADINGS="^## Where things are$|^## Device layer$|^## Running on the Kindle$|^## Plugins$|^## Extending rinky$|^## Build and toolchain$|^### Sleep and the Kindle framework$|^### Kindle system services$"
arch_doc() {
  grep -nE "$ARCH_HEADINGS" docs/ARCHITECTURE.md
  [ "$(grep -cE "$ARCH_HEADINGS" docs/ARCHITECTURE.md)" = 8 ]
}

step layout -- layout
step license -- license
step workspace-deps -- workspace_deps
step no-unimplemented -- no_unimplemented
step missing-docs -- missing_docs
step cargo-test -- cargo test --workspace
step unit-tests-per-crate -- unit_tests_per_crate
step clippy-host -- cargo clippy --workspace --all-targets -- -D warnings
[ -n "$ONLY" ] || rm -rf "$OUT/render"
step render -- "$BIN" render testdata/juliet.epub "$OUT/render" 6
step render-compare -- python3 -I scripts/png-compare.py "$OUT/render" "$PROTO"
if [ $ARM = 1 ]; then
  step zigbuild -- cargo zigbuild --release --target armv7-unknown-linux-musleabihf --workspace
  step arm-file-type -- file_type
  step clippy-arm -- scripts/clippy-arm.sh
fi
[ -n "$ONLY" ] || rm -rf "$OUT/sim-headless" "$OUT/demo"
# The reader saves its position next to the book (testdata/juliet.sdr, git-ignored);
# start every scripted sim run from page 1.
rm -rf testdata/*.sdr
step sim-headless -- "$BIN" sim testdata/juliet.epub --headless --script tests/sim/turn-pages.txt --out "$OUT/sim-headless"
step demo-headless -- "$BIN" demo --headless --script tests/sim/demo.txt --out "$OUT/demo"
[ $WINDOW = 1 ] && step sim-window -- sim_window
step screens-test -- cargo test -p ink-app --test screens
step golden-pass -- cargo test -p ink-html --test golden
step golden-one-pixel expect=101 -- golden_one_pixel
step footer-golden-pass -- cargo test -p ink-app --test footer_golden
step footer-one-pixel expect=101 -- footer_one_pixel
step golden-update-noop -- golden_update_noop
step slot-test -- scripts/e2e/slot-test.sh
step slot-test-main -- scripts/emu-slot-test.sh
step verify-slot-races -- scripts/e2e/verify-slot-races.sh
step verify-golden-update-outdir -- scripts/e2e/verify-golden-update-outdir.sh
step verify-lib-evidence -- scripts/e2e/verify-lib-evidence.sh
step e2e-runner-test -- scripts/e2e/run-test.sh "$OUT/e2e-runner-test-out"
step launcher-race-test -- scripts/e2e/launcher-race-test.sh "$OUT/launcher-race-sandboxes" 10
step device-sh-no-args -- device_sh_no_args
step arch-doc -- arch_doc
echo "bad steps: $(grep -c ' BAD$' "$STEPS")" | tee -a "$STEPS"
