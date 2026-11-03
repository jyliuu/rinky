#!/bin/sh
# Fast host test run for day-to-day work: unit tests of every crate plus ink-app's
# integration tests (screens, goldens, CLI). Skips the heavy attack, hostile-input,
# fuzz and fixture suites (crates/*/tests/verify_*.rs, fixtures.rs, ...); run those with
# a plain `cargo test --workspace` before a release.
#   scripts/test-fast.sh            all crates
#   scripts/test-fast.sh -p ink-ui  extra cargo arguments for the unit-test step
set -eu
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(getconf _NPROCESSORS_ONLN)}
THREADS=${TEST_THREADS:-4}
if [ $# -gt 0 ]; then
    cargo test "$@" --lib -- --test-threads="$THREADS"
else
    cargo test --workspace --lib -- --test-threads="$THREADS"
    # Integration tests only (screens, goldens, CLI): the unit tests ran above.
    cargo test -p ink-app --test '*' -- --test-threads="$THREADS"
fi
