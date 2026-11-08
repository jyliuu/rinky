<!--
Title: the changes in a few words, comma separated ("0.5.2: Wi-Fi auto-join, keyboard press").
One section per commit, in order. Delete what does not apply. Example: PR #19.
-->

<One paragraph: what this PR brings and why.>

**<Change in a few words>** (`abc1234`)
<Why it was needed and what changes, in a few lines or bullets.>

**<Next change>** (`def5678`)
<...>

**Checks:** <commands that ran and passed, e.g. `cargo clippy --workspace --all-targets -- -D warnings`, `scripts/test-fast.sh`, `cargo test --workspace`; simulator or emulator runs; goldens regenerated and why>

**Not tested on a device:** <behaviour only real hardware can show: sleep and wake, Wi-Fi, e-ink refresh, a specific model>

- [ ] `CHANGELOG.md` entry under the unreleased heading for user-visible changes
- [ ] `docs/user-guide.md` updated where the behaviour it describes changed
- [ ] No version bump outside a "Release X.Y.Z" commit
