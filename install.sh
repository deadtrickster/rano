#!/bin/sh
# Installs rano.
#
#   curl -fsSL https://raw.githubusercontent.com/deadtrickster/rano/master/install.sh | sh
#
# Downloads the prebuilt binary for this platform from the latest GitHub release
# and puts it in ~/.local/bin. Fallback order, so the one-liner works on a
# platform with no published asset as well as on one with:
#
#   1. the release asset for this platform (no toolchain needed)
#   2. a build from a checkout, if this script is being run from one
#   3. a clone of the default branch, built from source
#
# For (2) and (3) you need Rust (https://rustup.rs) and a C compiler, because the
# tree-sitter grammars are C and build.rs compiles them. The script checks for
# both and names the one that is missing rather than letting cargo fail in its
# own words.
#
# Environment:
#   RANO_INSTALL_DIR   where the binary goes   (default: ~/.local/bin)
#   RANO_VERSION       tag or branch to build   (default: the default branch)
#   RANO_FROM_SOURCE   set to anything to skip the download and build instead
#
# `cargo install --git https://github.com/deadtrickster/rano.git` also works and
# is shorter, but it always builds from source, installs to ~/.cargo/bin whether
# or not that is on your PATH, and on failure says so in cargo's words rather
# than naming what is missing.

set -eu

REPO="deadtrickster/rano"

INSTALL_DIR="${RANO_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${RANO_VERSION:-}"

say() { printf '%s\n' "$*"; }
# Everything that is not the binary path goes to stderr, so the functions below
# can be used in a command substitution without their chatter becoming the
# answer.
warn() { printf '%s\n' "$*" >&2; }
die() { printf 'rano: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1; }

# A checkout to build from, or nothing: the directory holding this script, if it
# holds rano's own Cargo.toml. The `name = "rano"` test is what keeps a stray
# `Cargo.toml` in the current directory from being mistaken for the project —
# when this is piped into `sh`, `$0` is `sh` and the directory is the caller's.
local_checkout() {
    dir=$(CDPATH= cd -- "$(dirname -- "$0")" 2>/dev/null && pwd) || return 1
    if [ -f "$dir/Cargo.toml" ] && grep -q '^name = "rano"$' "$dir/Cargo.toml" 2>/dev/null; then
        printf '%s' "$dir"
    else
        return 1
    fi
}

# The prebuilt release asset for this platform, extracted, or nothing.
#
# Returning non-zero is a normal outcome, not an error: a platform with no
# published asset falls through to the source build. The assets are named by
# `scripts/make-dist.sh` from the triple below, and `scripts/check-dist-names.sh`
# keeps this case arm and the release workflow's matrix in step — so a rename on
# one side fails in CI rather than silently degrading every install to a build.
try_prebuilt() {
    tmp="$1"
    case "$(uname -s)/$(uname -m)" in
        Linux/x86_64)              triple=x86_64-unknown-linux-gnu ;;
        Linux/aarch64 | Linux/arm64) triple=aarch64-unknown-linux-gnu ;;
        Darwin/arm64)              triple=aarch64-apple-darwin ;;
        Darwin/x86_64)             triple=x86_64-apple-darwin ;;
        *) return 1 ;;
    esac
    name="rano-$triple.tar.gz"
    if [ -n "$VERSION" ]; then
        url="https://github.com/$REPO/releases/download/$VERSION/$name"
    else
        url="https://github.com/$REPO/releases/latest/download/$name"
    fi
    need curl || return 1
    curl -fsSL -o "$tmp/$name" "$url" 2>/dev/null || return 1
    tar -xzf "$tmp/$name" -C "$tmp" 2>/dev/null || return 1
    [ -x "$tmp/rano" ] || return 1
    printf '%s' "$tmp/rano"
}

# Build a checkout and print the binary's path on stdout.
build_from_source() {
    src="$1"
    need cargo || die "no cargo found: install Rust from https://rustup.rs and re-run"
    # build.rs compiles the vendored tree-sitter grammars with the `cc` crate,
    # which needs a working C compiler on PATH.
    if ! need cc && ! need gcc && ! need clang; then
        die "no C compiler found (cc, gcc or clang): the tree-sitter grammars are C and are compiled at build time"
    fi
    warn "Building rano from $src — this takes a minute or two..."
    cargo build --release --manifest-path "$src/Cargo.toml" >&2
    bin="$src/target/release/rano"
    [ -x "$bin" ] || die "the build produced no binary at $bin"
    printf '%s' "$bin"
}

main() {
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT INT TERM

    bin=""
    if src=$(local_checkout); then
        bin=$(build_from_source "$src")
    else
        if [ -z "${RANO_FROM_SOURCE:-}" ]; then
            bin=$(try_prebuilt "$tmp") || bin=""
        fi
        if [ -z "$bin" ]; then
            need git || die "no git found: needed to fetch the source (or run this from a checkout)"
            warn "Fetching $REPO..."
            if [ -n "$VERSION" ]; then
                git clone --depth 1 --branch "$VERSION" "https://github.com/$REPO.git" "$tmp/rano" >&2
            else
                git clone --depth 1 "https://github.com/$REPO.git" "$tmp/rano" >&2
            fi
            bin=$(build_from_source "$tmp/rano")
        fi
    fi

    mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"
    # cp+chmod rather than install(1): `install` is in GNU and BSD but not in
    # POSIX, and the difference is not worth a portability question here.
    cp "$bin" "$INSTALL_DIR/rano" || die "cannot write $INSTALL_DIR/rano"
    chmod 755 "$INSTALL_DIR/rano"
    # Say the version rather than assuming: a binary that cannot run is a
    # failure this script would otherwise report as success.
    got=$("$INSTALL_DIR/rano" --version) || die "installed $INSTALL_DIR/rano, but it does not run"
    say "$got  ->  $INSTALL_DIR/rano"

    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            say ""
            say "That directory is not on your PATH. Add it with:"
            say "  export PATH=\"$INSTALL_DIR:\$PATH\""
            ;;
    esac
}

main "$@"
