#!/usr/bin/env bash
# Fail when code changed relative to a base ref but the crate version did not
# go up.
#
#   scripts/check_version_bump.sh [BASE_REF]     (default: origin/main)
#
# "Code" is everything that ends up in the binary: src/ and the dependency
# set. Docs, tests, CI and the README images can change without a bump.
set -euo pipefail

base="${1:-origin/main}"
code_paths=(src Cargo.toml Cargo.lock)

version_at() { # version_at <ref|""> — "" reads the working tree
    local toml
    if [ -n "$1" ]; then toml=$(git show "$1:Cargo.toml"); else toml=$(cat Cargo.toml); fi
    # First `version = "..."` inside [package].
    awk '/^\[/{in_pkg = ($0 == "[package]")} in_pkg && /^version *=/{gsub(/[^0-9A-Za-z.+-]/, "", $3); print $3; exit}' <<<"$toml"
}

merge_base=$(git merge-base "$base" HEAD)
changed=$(git diff --name-only "$merge_base" -- "${code_paths[@]}")

if [ -z "$changed" ]; then
    echo "No code changes since $base; no version bump needed."
    exit 0
fi

old=$(version_at "$merge_base")
new=$(version_at "")

if [ "$old" != "$new" ] && [ "$(printf '%s\n%s\n' "$old" "$new" | sort -V | tail -n1)" = "$new" ]; then
    echo "Version bumped: $old -> $new"
    exit 0
fi

echo "Code changed since $base but the version did not go up ($old -> $new)."
echo "Bump \`version\` in Cargo.toml (and run \`cargo check\` to update Cargo.lock)."
echo
echo "Changed code files:"
sed 's/^/  /' <<<"$changed"
exit 1
