---
name: release
description: Cut a rinky release - release/X.Y.Z branch, version bump, dated changelog, PR, and after the merge the annotated vX.Y.Z tag that makes release.yml publish the Kindle and Kobo packages. Use when the user asks to release, cut, tag, ship or publish a version, or to bump the version.
argument-hint: "X.Y.Z"
disable-model-invocation: true
---

# Release rinky $ARGUMENTS

A release is a short-lived `release/X.Y.Z` branch whose last commit is
"Release X.Y.Z", merged into `main` by a PR (merge commit), then an annotated tag
`vX.Y.Z` on the merge commit. Pushing the tag runs `.github/workflows/release.yml`,
which checks the tag against `Cargo.toml` and `CHANGELOG.md`, builds the packages and
publishes them with the changelog section as the release notes.

Pushing a branch, opening a PR, merging and pushing a tag all reach GitHub: do each only
when the user asked for it in this conversation.

## 1. Branch

```sh
git fetch origin
git switch -c release/X.Y.Z origin/main     # or continue on the branch that holds the work
```

Version: patch (`0.5.2` -> `0.5.3`) for fixes and small changes, minor for new
features or new devices. Ask the user if unsure.

## 2. Changelog entries

The feature commits should already have added entries under the topmost unreleased
heading of `CHANGELOG.md` (`## [Unreleased]` or `## [X.Y.Z] - Unreleased`), grouped as
`### Added`, `### Changed`, `### Fixed`. Check that every user-visible commit since the
last tag has an entry:

```sh
git log --oneline --no-merges "$(git describe --tags --abbrev=0)"..HEAD
```

Entries are written for users, in plain sentences, and one entry may cover several
commits (0.5.3's dictionary entry covers 0c6d390 and 24a4bb3). Add
missing entries in their own commit (`Changelog: ...`) before the release commit. Update
`docs/user-guide.md` where the behaviour it describes changed.

## 3. Release commit

```sh
scripts/release.sh X.Y.Z            # add --trailer "Claude-Session: ..." when an agent commits
```

It refuses a dirty tree, `main`, an older version, or an empty unreleased section. It
sets `[workspace.package] version`, runs `cargo update --workspace` (only the workspace
crates' versions change in `Cargo.lock`), renames the heading to
`## [X.Y.Z] - <today>`, adds the `[X.Y.Z]: .../compare/vPREV...vX.Y.Z` link and commits
"Release X.Y.Z". `--date YYYY-MM-DD` sets another date; `--no-commit` only stages.

If the version was already bumped by hand and only the heading is undated, the same
command dates it and commits (Cargo.toml then stays as it is).

Then:

```sh
scripts/release.sh check X.Y.Z      # what release.yml will check on the tag
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Before a release also run the slow tests listed in `docs/TESTING.md` ("The slow tests")
and, if an emulator slot is free, `scripts/e2e/smoke.sh --up rel-jb` (see the `testing`
skill). Report what you did not run.

## 4. PR

```sh
git push -u origin release/X.Y.Z
gh pr create --base main --title "X.Y.Z: <three or four changes, comma separated>" --body-file <file>
```

Body format (see PR #19): one paragraph summary, then one bold heading per commit with
its short hash and a few lines, `**Checks:**` and `**Not tested on a device:**`.
`.github/pull_request_template.md` has the skeleton. Wait for "CI / test" and
"Package / package" to pass, then the user merges with a merge commit.

If the merge happens on a later day than the release commit, either keep the earlier
date or re-date it on the branch before merging (edit the heading, amend or add a
commit).

## 5. Tag (after the merge)

```sh
git fetch origin
scripts/release.sh tag --ref origin/main --push     # checks, creates vX.Y.Z and pushes it
```

Without `--push` it only creates the tag locally; push that one with
`git push origin vX.Y.Z` (running `tag` again refuses, since the tag exists).

The tag is annotated, message "rinky X.Y.Z", on the merge commit (as `v0.5.2` on
5f40997). Never move or delete a tag that was pushed: a published release points at it,
and the in-app update reads the latest release.

## 6. Verify the release

```sh
gh run list --workflow release.yml --limit 3
gh run watch <run id>
gh release view vX.Y.Z --json assets -q '.assets[].name'
```

Expected assets: `rinky-X.Y.Z-kindlehf.tar.gz`, `rinky-X.Y.Z-kindle.zip`,
`rinky-X.Y.Z-kobo.tar.gz`, `rinky-X.Y.Z-kobo.zip`, `SHA256SUMS`. The release body
should be the changelog section followed by GitHub's generated PR list. If the tag
check fails, fix `Cargo.toml`/`CHANGELOG.md` on `main` with a new PR and tag again
only if the tag was never pushed; otherwise ask the user.

## Done when

- `scripts/release.sh check X.Y.Z --ref vX.Y.Z` passes.
- The GitHub release vX.Y.Z exists with the five assets.
- Your report gives the PR link, the tag, the release run and the asset list.
