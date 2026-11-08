#!/bin/bash
# Release helper: the "Release X.Y.Z" commit, the checks release.yml runs on a tag, the
# release notes, and the annotated tag. See .claude/skills/release/SKILL.md.
#
#   scripts/release.sh X.Y.Z [--date YYYY-MM-DD] [--no-commit] [--trailer LINE]...
#       On a release branch with a clean tree: sets [workspace.package] version in
#       Cargo.toml, updates Cargo.lock (cargo update --workspace), turns the topmost
#       unreleased CHANGELOG.md heading (`## [Unreleased]` or `## [X.Y.Z] - Unreleased`)
#       into `## [X.Y.Z] - DATE` (default: today), adds the
#       `[X.Y.Z]: <repository>/compare/vPREV...vX.Y.Z` link, and commits
#       "Release X.Y.Z". The unreleased section must already hold the entries.
#       --no-commit stages the three files instead; --trailer adds a last line to the
#       commit message (repeatable).
#   scripts/release.sh check X.Y.Z [--ref REF]
#       Fails unless Cargo.toml says X.Y.Z and CHANGELOG.md has a dated `## [X.Y.Z]`
#       section with entries and a compare link. Reads the working tree, or REF.
#   scripts/release.sh notes X.Y.Z [--ref REF]
#       Prints the body of the `## [X.Y.Z]` changelog section (the GitHub release text).
#   scripts/release.sh tag [X.Y.Z] [--ref REF] [--push] [--remote NAME]
#       Runs `check` on REF (default HEAD; after the PR is merged use origin/main) and
#       creates the annotated tag vX.Y.Z ("rinky X.Y.Z") on it. X.Y.Z defaults to the
#       version in REF's Cargo.toml. Pushes the tag only with --push (remote: origin).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

die() { echo "release: $*" >&2; exit 1; }
usage() { sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2; exit 2; }

REF=
# file PATH: the contents of PATH in the working tree, or at $REF.
file() {
    if [ -n "$REF" ]; then git show "$REF:$1"; else cat "$1"; fi
}
# [workspace.package] version of the Cargo.toml on stdin. It reads to the end (no
# `exit`), so the writer of `file Cargo.toml | cargo_version` never gets SIGPIPE,
# which pipefail would turn into a silent exit.
cargo_version() {
    awk '/^\[workspace\.package\]/ {s = 1; next} /^\[/ {s = 0} s && $1 == "version" && !done {gsub(/"/, "", $3); print $3; done = 1}'
}
repo_url() {
    awk '/^\[workspace\.package\]/ {s = 1; next} /^\[/ {s = 0} s && $1 == "repository" {gsub(/"/, "", $3); print $3; exit}' Cargo.toml
}
check_version_arg() {
    [[ $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "not a version: '$1' (want X.Y.Z)"
}
# Body of the `## [V]` section of the changelog on stdin (blank lines at both ends cut).
section() {
    awk -v v="$1" '
        index($0, "## [" v "]") == 1 { s = 1; next }
        s && /^## \[/ { exit }
        s && /^\[[^]]*\]: / { exit }
        s { buf[++n] = $0 }
        END {
            a = 1; while (a <= n && buf[a] ~ /^[ \t]*$/) a++
            b = n; while (b >= a && buf[b] ~ /^[ \t]*$/) b--
            for (i = a; i <= b; i++) print buf[i]
        }'
}

cmd_check() {
    local v=$1 cv log
    check_version_arg "$v"
    cv=$(file Cargo.toml | cargo_version)
    [ "$cv" = "$v" ] || die "Cargo.toml version is $cv, not $v"
    log=$(file CHANGELOG.md)
    grep -Eq "^## \[$v\] - [0-9]{4}-[0-9]{2}-[0-9]{2}\$" <<<"$log" \
        || die "CHANGELOG.md has no dated heading '## [$v] - YYYY-MM-DD' (still Unreleased?)"
    # grep without -q: an early exit would SIGPIPE awk and fail the pipeline (pipefail).
    section "$v" <<<"$log" | grep '^- ' >/dev/null || die "CHANGELOG.md section $v has no entries"
    grep -q "^\[$v\]: " <<<"$log" || die "CHANGELOG.md has no '[$v]: .../compare/...' link"
    echo "release: Cargo.toml and CHANGELOG.md agree on $v${REF:+ at $REF}"
}

cmd_notes() {
    local v=$1 out
    check_version_arg "$v"
    out=$(file CHANGELOG.md | section "$v")
    [ -n "$out" ] || die "CHANGELOG.md has no section $v"
    printf '%s\n' "$out"
}

cmd_prepare() {
    local v=$1 date=$2 commit=$3; shift 3
    local trailers=("$@") cur head prev url branch msg
    check_version_arg "$v"
    [[ $date =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || die "not a date: '$date' (want YYYY-MM-DD)"
    [ -z "$(git status --porcelain --untracked-files=no)" ] \
        || die "the tree has uncommitted changes; commit or set them aside first"
    branch=$(git symbolic-ref --quiet --short HEAD || true)
    case "$branch" in
        main|master) die "on $branch: cut a release/$v branch first" ;;
        "") die "HEAD is detached: check out a release/$v branch first" ;;
    esac

    cur=$(cargo_version <Cargo.toml)
    [ -n "$cur" ] || die "no [workspace.package] version in Cargo.toml"
    if [ "$cur" != "$v" ] && [ "$(printf '%s\n%s\n' "$cur" "$v" | sort -t. -k1,1n -k2,2n -k3,3n | tail -1)" != "$v" ]; then
        die "$v is older than the current version $cur"
    fi

    # The topmost version heading must be the unreleased one.
    head=$(grep -m1 '^## \[' CHANGELOG.md || true)
    case "$head" in
        "## [Unreleased]"|"## [Unreleased] - "*|"## [$v] - Unreleased") ;;
        "## [$v] - "*) die "CHANGELOG.md already has '$head'" ;;
        *) die "the topmost CHANGELOG.md heading is '$head'; want '## [Unreleased]' or '## [$v] - Unreleased'" ;;
    esac
    awk -v h="$head" '$0 == h { s = 1; next } s && /^## \[/ { exit } s && /^- / { found = 1 } END { exit !found }' CHANGELOG.md \
        || die "the section under '$head' has no entries; write them before the release"
    prev=$(grep '^## \[[0-9]' CHANGELOG.md | grep -v -F "## [$v]" | head -1 | sed 's/^## \[\([^]]*\)\].*/\1/')
    [ -n "$prev" ] || die "no earlier version heading in CHANGELOG.md for the compare link"
    url=$(repo_url)
    [ -n "$url" ] || die "no [workspace.package] repository in Cargo.toml"

    # CHANGELOG.md: date the heading, add or fix the links.
    awk -v h="$head" -v v="$v" -v d="$date" -v prev="$prev" -v url="$url" '
        $0 == h && !done { print "## [" v "] - " d; done = 1; next }
        /^\[Unreleased\]: / { print "[Unreleased]: " url "/compare/v" v "...HEAD"; next }
        index($0, "[" v "]: ") == 1 { next }
        /^\[[0-9][^]]*\]: / && !linked { print "[" v "]: " url "/compare/v" prev "...v" v; linked = 1 }
        { print }
        END { if (!linked) print "[" v "]: " url "/compare/v" prev "...v" v }
    ' CHANGELOG.md >CHANGELOG.md.tmp
    mv CHANGELOG.md.tmp CHANGELOG.md

    # Cargo.toml and Cargo.lock.
    if [ "$cur" != "$v" ]; then
        awk -v v="$v" '
            /^\[workspace\.package\]/ { s = 1; print; next }
            /^\[/ { s = 0 }
            s && $1 == "version" && !done { print "version = \"" v "\""; done = 1; next }
            { print }
        ' Cargo.toml >Cargo.toml.tmp
        mv Cargo.toml.tmp Cargo.toml
    fi
    cargo update --workspace --offline --quiet 2>/dev/null || cargo update --workspace --quiet
    [ "$(cargo_version <Cargo.toml)" = "$v" ] || die "Cargo.toml was not updated to $v"

    git add Cargo.toml Cargo.lock CHANGELOG.md
    echo "release: $cur -> $v, CHANGELOG.md '## [$v] - $date', link vs v$prev"
    if [ "$commit" = 0 ]; then
        echo "release: changes staged; commit them as \"Release $v\""
        return
    fi
    msg=$(printf 'Release %s\n\nVersion, changelog.' "$v")
    if [ ${#trailers[@]} -gt 0 ]; then
        msg=$(printf '%s\n\n%s' "$msg" "$(printf '%s\n' "${trailers[@]}")")
    fi
    git commit --quiet -m "$msg"
    echo "release: committed $(git log -1 --format='%h %s')"
    echo "release: next: push the branch, open the PR, merge, then: scripts/release.sh tag --ref origin/main --push"
}

cmd_tag() {
    local v=$1 remote=$2 push=$3 tag
    REF=${REF:-HEAD}
    git rev-parse --quiet --verify "$REF^{commit}" >/dev/null || die "unknown ref '$REF'"
    [ -n "$v" ] || v=$(file Cargo.toml | cargo_version)
    cmd_check "$v"
    tag=v$v
    if git rev-parse --quiet --verify "refs/tags/$tag" >/dev/null; then
        die "tag $tag already exists (at $(git rev-parse --short "$tag^{commit}"))"
    fi
    if [ "$push" = 1 ] && [ -n "$(git ls-remote --tags "$remote" "refs/tags/$tag")" ]; then
        die "$remote already has tag $tag"
    fi
    if git rev-parse --quiet --verify "$remote/main" >/dev/null \
        && ! git merge-base --is-ancestor "$REF" "$remote/main"; then
        echo "release: warning: $REF is not on $remote/main; tag the merge commit (--ref $remote/main) after the PR is merged" >&2
    fi
    git tag -a "$tag" -m "rinky $v" "$REF"
    echo "release: tagged $tag at $(git rev-parse --short "$REF^{commit}")"
    if [ "$push" = 1 ]; then
        git push "$remote" "refs/tags/$tag"
        echo "release: pushed $tag; release.yml builds and publishes it"
    else
        echo "release: not pushed; push with: git push $remote $tag"
    fi
}

[ $# -gt 0 ] || usage
case "$1" in
    -h|--help) usage ;;
    check|notes)
        cmd=$1; shift
        [ $# -ge 1 ] || usage
        v=$1; shift
        while [ $# -gt 0 ]; do
            case "$1" in
                --ref) REF=${2:?--ref needs a value}; shift 2 ;;
                *) usage ;;
            esac
        done
        "cmd_$cmd" "$v"
        ;;
    tag)
        shift
        v= remote=origin push=0
        while [ $# -gt 0 ]; do
            case "$1" in
                --ref) REF=${2:?--ref needs a value}; shift 2 ;;
                --remote) remote=${2:?--remote needs a value}; shift 2 ;;
                --push) push=1; shift ;;
                -*) usage ;;
                *) [ -z "$v" ] || usage; v=$1; check_version_arg "$v"; shift ;;
            esac
        done
        cmd_tag "$v" "$remote" "$push"
        ;;
    [0-9]*)
        v=$1; shift
        date=$(date +%F) commit=1 trailers=()
        while [ $# -gt 0 ]; do
            case "$1" in
                --date) date=${2:?--date needs a value}; shift 2 ;;
                --no-commit) commit=0; shift ;;
                --trailer) trailers+=("${2:?--trailer needs a value}"); shift 2 ;;
                *) usage ;;
            esac
        done
        cmd_prepare "$v" "$date" "$commit" ${trailers[@]+"${trailers[@]}"}
        ;;
    *) usage ;;
esac
