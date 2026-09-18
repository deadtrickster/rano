# Vendored: tree-sitter-dockerfile

The C sources of the [dockerfile grammar][upstream] for tree-sitter, copied
verbatim from the `tree-sitter-dockerfile` crate v0.2.0
(`src/parser.c`, `src/scanner.c`, `src/tree_sitter/*.h`), MIT licensed,
© 2020 Camden Cheek.

Vendored rather than depended on because the crate binds tree-sitter 0.20:
its `language()` returns a `tree_sitter::Language` type foreign to the 0.27
runtime rano uses, and referencing the crate at all drags the 0.20 C runtime
into the link, which collides with 0.27's. The grammar itself is ABI-14,
which the 0.27 runtime loads fine — `src/syntax.rs` declares the
`tree_sitter_dockerfile` symbol directly and `build.rs` compiles these
files into rano.

[upstream]: https://github.com/camdencheek/tree-sitter-dockerfile
