#!/bin/sh
# Package a built rano as the release asset install.sh asks for.
#
#   scripts/make-dist.sh <target-triple> [outdir]
#
# The contract is install.sh's, not this script's: it downloads
# `rano-<triple>.tar.gz` from the release and expects `rano` at the archive
# root. That naming lives in ONE place — here — and both workflows call this
# script, so a release asset cannot be built with a name the installer does not
# ask for. `scripts/check-dist-names.sh` asserts the triples line up.
#
# No `--target` in the workflows: each runner builds for itself, so the output
# is `target/release/rano` and there is no cross toolchain to get wrong. The
# triple is passed for the FILE NAME only, and the workflow checks it against
# `rustc -vV`'s host so a runner-image change cannot silently mislabel a binary.
# A cross build (binary under `target/<triple>/release/`) is supported for the
# case where we want one.

set -eu

triple="${1:-}"
if [ -z "$triple" ]; then
    echo "usage: make-dist.sh <target-triple> [outdir]" >&2
    exit 2
fi
outdir="${2:-dist}"

repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

# Native build first, then a cross build's output.
bin=""
for candidate in "$repo/target/release/rano" "$repo/target/$triple/release/rano"; do
    if [ -x "$candidate" ]; then
        bin="$candidate"
        break
    fi
done
if [ -z "$bin" ]; then
    echo "mark-dist: no built binary for $triple" >&2
    echo "  looked in target/release/ and target/$triple/release/" >&2
    exit 1
fi

mkdir -p "$outdir"
name="rano-$triple.tar.gz"

# COPYFILE_DISABLE stops macOS tar writing AppleDouble `._rano` entries, which
# would otherwise land in the archive and be extracted by the installer.
#
# The determinism flags GNU tar offers (--sort, --mtime, --owner) are NOT used:
# BSD tar on macOS rejects them, and a portable script is worth more here than a
# byte-reproducible tarball. The version is in the release tag, not the archive.
COPYFILE_DISABLE=1 tar -czf "$outdir/$name" -C "$(dirname -- "$bin")" rano

# Prove the archive is what the installer expects before it is published: `rano`
# at the root, executable. A release asset that fails this is one install.sh
# downloads and then discards for the source path, which looks like a slow
# install rather than a broken one.
listing=$(tar -tzf "$outdir/$name" | head -20)
case "$listing" in
    *"rano"*) ;;
    *) echo "make-dist: $name does not contain rano at the root: $listing" >&2; exit 1 ;;
esac
if [ "$(printf '%s\n' "$listing" | grep -c '^rano$')" -ne 1 ]; then
    echo "make-dist: $name should hold exactly one top-level `rano`: $listing" >&2
    exit 1
fi

printf '%s\n' "$outdir/$name"
