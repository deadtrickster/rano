#!/bin/sh
# Every triple the release workflow builds must be one install.sh asks for.
#
# The failure this prevents is silent and expensive-ish: publishing
# `rano-aarch64-unknown-linux-musl.tar.gz` when the installer downloads
# `rano-aarch64-unknown-linux-gnu.tar.gz` means the asset exists, the download
# 404s, and every install falls back to a source build. Nothing reports it —
# the workflow is green and the release looks right.
#
# The reverse is reported but not fatal: a triple the installer knows and we do
# not ship degrades to the source build, which is what install.sh is designed
# to do. That is a gap worth seeing, not an error.

set -eu

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# What install.sh derives from `uname`. Read from its `triple=<name>` assignments
# rather than by pattern-matching what a triple looks like: the first version of
# this script required `-unknown-` in the middle, so it silently ignored both
# `*-apple-darwin` targets and reported the real workflow as building assets the
# installer never asks for. The assignment is the contract; the shape is not.
known=$(grep -oE 'triple=[A-Za-z0-9_.-]+' "$repo/install.sh" | sed 's/^triple=//' | sort -u)
if [ -z "$known" ]; then
    echo "check-dist-names: found no triples in install.sh — did the layout change?" >&2
    exit 1
fi

# What the workflows build. The matrix lists them literally, as `triple: <name>`,
# so this does not have to understand YAML.
built=$(grep -h 'triple:' "$repo"/.github/workflows/*.yml 2>/dev/null |
    sed 's/.*triple:[[:space:]]*//' |
    grep -v '^$' |
    grep -v '^[$]' |
    sort -u)
if [ -z "$built" ]; then
    echo "check-dist-names: no triples found in the workflows" >&2
    exit 1
fi

status=0
for t in $built; do
    if ! printf '%s\n' "$known" | grep -qx "$t"; then
        echo "check-dist-names: the workflow builds '$t', which install.sh never asks for" >&2
        echo "  install.sh asks for:" >&2
        printf '    %s\n' $known >&2
        status=1
    fi
done

for t in $known; do
    if ! printf '%s\n' "$built" | grep -qx "$t"; then
        echo "check-dist-names: install.sh asks for '$t' but no workflow builds it" >&2
        echo "  (not fatal: install.sh falls back to a source build for it)" >&2
    fi
done

if [ "$status" -eq 0 ]; then
    echo "check-dist-names: OK — $(printf '%s\n' "$built" | wc -l | tr -d ' ') triples, all known to install.sh"
fi
exit "$status"
