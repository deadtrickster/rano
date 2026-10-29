#!/usr/bin/env python3
"""Generate rano's name table from GitHub linguist's own `languages.yml`.

    ./scripts/linguist-table.py /path/to/languages.yml

Writes `src/syntax/linguist.rs` and `tests/fixtures/linguist/our-languages.tsv` from that
one input, so the `linguist_table` test checks the source data and not a second copy of
the table.
"""

import sys
import os
import datetime

# linguist's language name -> rano's `Lang` variant. Only the ones with a grammar:
# a name that maps to a language rano cannot style is not a superset, it is a lie.
OURS = {
    "Rust": "Rust",
    "Go": "Go",
    "Shell": "Bash",
    "Python": "Python",
    "C": "C",
    "JSON": "Json",
    "Common Lisp": "CommonLisp",
    "JavaScript": "JavaScript",
    "TypeScript": "TypeScript",
    "TSX": "Tsx",
    "Markdown": "Markdown",
    "TOML": "Toml",
    "YAML": "Yaml",
    "HTML": "Html",
    "CSS": "Css",
    "Lua": "Lua",
    "Ruby": "Ruby",
    "PHP": "Php",
    "Java": "Java",
    "Makefile": "Make",
    "Dockerfile": "Dockerfile",
    "INI": "Ini",
    "Diff": "Diff",
    "Emacs Lisp": "Elisp",
    "Scheme": "Scheme",
    "SQL": "Sql",
    "Clojure": "Clojure",
}

FIELDS = {"extensions": "ext", "filenames": "filename", "interpreters": "interpreter"}

# The names `Lang::name()` answers with, which is what the fixture carries so the test can
# go back through `Lang::from_name` — one mapping, not two.
DISPLAY = {
    "Rust": "rust", "Go": "go", "Bash": "bash", "Python": "python", "C": "c",
    "Json": "json", "CommonLisp": "commonlisp", "JavaScript": "javascript",
    "TypeScript": "typescript", "Tsx": "tsx", "Markdown": "markdown",
    "Toml": "toml", "Yaml": "yaml", "Html": "html", "Css": "css", "Lua": "lua",
    "Ruby": "ruby", "Php": "php", "Java": "java", "Make": "make",
    "Dockerfile": "dockerfile", "Ini": "ini", "Diff": "diff", "Elisp": "elisp",
    "Scheme": "scheme", "Sql": "sql", "Clojure": "clojure",
}

# Linguist lists some names under more than one of our languages, because IT disambiguates
# them by content and we do not. First-wins in `OURS` order is the default; this is the
# short list where that order is wrong, and it is written down rather than left to chance.
PREFER = {("ext", "inc"): "Php"}

# Names rano claims by a better rule and must not take from here: `hosts` is a columnar
# system database (`syntax::columns`), and ini would mis-describe it.
SKIP = {("filename", "hosts")}


def parse(path):
    """{language: {field: [values]}} for the fields we care about."""
    out, cur, field = {}, None, None
    for line in open(path, encoding="utf-8"):
        if line.startswith("#") or line.strip() == "---":
            continue
        if line[:1] not in (" ", "\t"):
            name = line.split(":", 1)[0].strip()
            cur = name if line.rstrip().endswith(":") else None
            if cur:
                out.setdefault(cur, {})
            field = None
            continue
        if cur is None:
            continue
        stripped = line.strip()
        if stripped.startswith(("extensions:", "filenames:", "interpreters:")):
            field = stripped.split(":", 1)[0]
            out[cur].setdefault(field, [])
            continue
        if stripped.startswith("- ") and field:
            value = stripped[2:].strip().strip('"').strip("'")
            if field == "extensions" and value.startswith("."):
                value = value[1:]
            # Both keys are matched case-insensitively — `detect` lowers what a person
            # typed — so the table is lowered too, and `.JSON-tmLanguage` is one entry
            # rather than a miss.
            value = value.lower()
            out[cur][field].append(value.lower() if field == "filenames" else value)
            continue
        if not line.startswith("    "):
            field = None
    return out


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    src = sys.argv[1]
    data = parse(src)
    rows, seen, conflicts = [], {}, []
    for name in OURS:
        entry = data.get(name)
        if entry is None:
            conflicts.append(f"linguist has no entry named {name!r}")
            continue
        for field in ("extensions", "filenames", "interpreters"):
            for value in entry.get(field, []):
                key = (FIELDS[field], value)
                if key in SKIP:
                    conflicts.append(f"{value!r} skipped: rano claims it by its own rule")
                    continue
                if key in seen:
                    want = PREFER.get(key)
                    conflicts.append(
                        f"{value!r} claimed by {seen[key]} and {OURS[name]}"
                        + (f" -> {want}" if want else " -> first wins")
                    )
                    if want != OURS[name] and (want is None or seen[key] != want):
                        continue
                    # The preferred one arrives after the other: replace the row.
                    if want == OURS[name]:
                        rows = [r for r in rows if (r[1], r[2]) != key]
                        seen[key] = want
                        rows.append((want, FIELDS[field], value))
                    continue
                seen[key] = OURS[name]
                rows.append((OURS[name], FIELDS[field], value))
    stamp = datetime.date.today().isoformat()
    rows.sort(key=lambda r: (r[1], r[2]))

    # ---- the fixture: the source data, for the test to check against ----
    os.makedirs("tests/fixtures/linguist", exist_ok=True)
    with open("tests/fixtures/linguist/our-languages.tsv", "w", encoding="utf-8") as f:
        f.write("# GitHub linguist, languages.yml — the entries for the languages rano has a\n")
        f.write("# grammar for. Generated by scripts/linguist-table.py; do not hand-edit.\n")
        f.write(f"# source: {os.path.basename(src)}  fetched {stamp}\n")
        f.write("# lang\tkind\tvalue\n")
        for lang, kind, value in rows:
            # The DISPLAY name, so the test can go back through `Lang::from_name`.
            f.write(f"{DISPLAY[lang]}\t{kind}\t{value}\n")

    # ---- the table: what detect consults ----
    def arm(kind, rust_name):
        items = [r for r in rows if r[1] == kind]
        body = "\n".join(f'    ("{v}", Lang::{l}),' for l, _, v in items)
        return f"pub static {rust_name}: &[(&str, Lang)] = &[\n{body}\n];\n"

    counts = {k: sum(1 for r in rows if r[1] == k) for k in ("ext", "filename", "interpreter")}
    with open("src/syntax/linguist.rs", "w", encoding="utf-8") as f:
        f.write(f'''//! The names a language is known by, taken from GitHub linguist's `languages.yml` rather
//! than from memory — from memory is how a table acquires `.h` and loses `.hpp`. Only
//! languages rano has a grammar for: a name it cannot style is a mislabel, not a superset.
//!
//! Generated by `scripts/linguist-table.py`; do not hand-edit. The rows also live in
//! `tests/fixtures/linguist/our-languages.tsv`, which the `linguist_table` test checks
//! `detect_sample` against, so a lost row or a drifting table fails there.
//!
//! Counts, this generation: {counts["ext"]} extensions, {counts["filename"]} file names,
//! {counts["interpreter"]} interpreters.

use super::Lang;

''')
        f.write(arm("ext", "EXTENSIONS"))
        f.write("\n")
        f.write(arm("filename", "FILENAMES"))
        f.write("\n")
        f.write(arm("interpreter", "INTERPRETERS"))
        f.write('''
/// The language a FILE NAME's extension names, or `None` — matched as a suffix of the
/// name, which is what linguist itself does and what multi-part extensions need:
/// `.cljs.hl` and `.json.example` are entries here, and `Path::extension()` can never see
/// them because it returns only the component after the last dot.
pub fn by_suffix(name: &str) -> Option<Lang> {
    let lower = name.to_ascii_lowercase();
    EXTENSIONS
        .iter()
        .filter(|(e, _)| {
            lower.len() > e.len()
                && lower.ends_with(*e)
                && lower.as_bytes()[lower.len() - e.len() - 1] == b'.'
        })
        // The LONGEST key wins: `.yml.mysql` is YAML and `.mysql` is SQL, and both are
        // suffixes of the same name.
        .max_by_key(|(e, _)| e.len())
        .map(|(_, l)| *l)
}

/// The language a file NAME names — `Cargo.lock`, `Gemfile`, `.bashrc` — or `None`.
pub fn by_filename(name: &str) -> Option<Lang> {
    let want = name.to_ascii_lowercase();
    FILENAMES
        .iter()
        .find(|(f, _)| *f == want)
        .map(|(_, l)| *l)
}

/// The language an interpreter's name names — the `python3` of `#!/usr/bin/env python3`.
pub fn by_interpreter(program: &str) -> Option<Lang> {
    INTERPRETERS
        .iter()
        .find(|(p, _)| *p == program)
        .map(|(_, l)| *l)
}
''')

    print(f"rows: {len(rows)}  {counts}")
    for c in conflicts:
        print("CONFLICT:", c)


if __name__ == "__main__":
    main()
