#!/usr/bin/env python3
"""Prove a comment-only change: every Rust file's tokens, comments stripped,
match the same file at a base commit.

    scripts/same-code.py <base-commit> [paths...]

Exit 0 when every .rs file under the paths (default: the repo) has the same
code as at <base>; otherwise list the files that differ and exit 1.
"""

import subprocess
import sys


def strip(src: str) -> str:
    """Rust source without comments, whitespace runs collapsed to one space.

    Strings, raw strings (r#".."#), byte strings and char literals are copied
    whole; a `'` that does not close as a char literal is a lifetime.
    """
    out = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
            out.append(" ")
            continue
        if src.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif src.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
            out.append(" ")
            continue
        # Raw strings: r"..", r#".."#, br#".."#.
        j = i + (1 if c == "b" and src.startswith("br", i) else 0)
        if src[j : j + 1] == "r" and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            k = j + 1
            while k < n and src[k] == "#":
                k += 1
            if k < n and src[k] == '"':
                close = '"' + "#" * (k - j - 1)
                end = src.find(close, k + 1)
                end = n if end < 0 else end + len(close)
                out.append(src[i:end])
                i = end
                continue
        if c == '"':
            k = i + 1
            while k < n and src[k] != '"':
                k += 2 if src[k] == "\\" else 1
            out.append(src[i : k + 1])
            i = k + 1
            continue
        if c == "'":
            # A char literal closes within a short escape; otherwise a lifetime.
            if src.startswith("\\", i + 1):
                k = src.find("'", i + 2)
                if 0 < k <= i + 12:
                    out.append(src[i : k + 1])
                    i = k + 1
                    continue
            elif i + 2 < n and src[i + 2] == "'":
                out.append(src[i : i + 3])
                i += 3
                continue
            elif i + 1 < n and ord(src[i + 1]) > 127:
                k = src.find("'", i + 1)
                if 0 < k <= i + 5:
                    out.append(src[i : k + 1])
                    i = k + 1
                    continue
            out.append(c)
            i += 1
            continue
        if c.isspace():
            while i < n and src[i].isspace():
                i += 1
            out.append(" ")
            continue
        out.append(c)
        i += 1
    return " ".join("".join(out).split())


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    base, paths = sys.argv[1], sys.argv[2:] or ["."]
    files = [f for f in git("ls-files", "--", *paths).split() if f.endswith(".rs")]
    bad = []
    for f in files:
        try:
            old = git("show", f"{base}:{f}")
        except subprocess.CalledProcessError:
            bad.append(f"{f} (new since {base})")
            continue
        with open(f, encoding="utf-8") as fh:
            now = fh.read()
        if strip(old) != strip(now):
            bad.append(f)
    for f in bad:
        print(f"differs: {f}")
    print(f"{len(files) - len(bad)}/{len(files)} files same code as {base}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
