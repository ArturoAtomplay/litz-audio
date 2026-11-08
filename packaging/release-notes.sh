#!/bin/sh
# Prints Markdown release notes for TAG from Conventional Commit subjects
# since the previous tag. Usage: packaging/release-notes.sh v1.2.3
set -eu

tag="$1"
case "$tag" in
*-*) previous="$(git describe --tags --abbrev=0 "$tag^" 2>/dev/null || true)" ;;
*) previous="$(git describe --tags --abbrev=0 --exclude='*-*' "$tag^" 2>/dev/null || true)" ;;
esac
range="${previous:+$previous..}$tag"

section() {
    items="$(git log --no-merges --format='%s' "$range" |
        sed -n "s/^$2\(([^)]*)\)\{0,1\}!\{0,1\}: */- /p")"
    if [ -n "$items" ]; then
        printf '## %s\n\n%s\n\n' "$1" "$items"
    fi
}

section Features feat
section Fixes fix
section Performance perf
section Documentation docs
if [ -n "$previous" ]; then
    printf '**Full changelog**: https://github.com/%s/compare/%s...%s\n' \
        "${GITHUB_REPOSITORY:-ArturoAtomplay/litz-audio}" "$previous" "$tag"
fi
