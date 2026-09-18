//! Compiles the vendored tree-sitter grammars (see `vendor/`) into rano.

fn main() {
    let mut build = cc::Build::new();
    build
        .include("vendor/tree-sitter-dockerfile")
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-Wno-trigraphs");
    for f in [
        "vendor/tree-sitter-dockerfile/parser.c",
        "vendor/tree-sitter-dockerfile/scanner.c",
    ] {
        build.file(f);
        println!("cargo:rerun-if-changed={f}");
    }
    build.compile("tree_sitter_dockerfile");

    // `cargo:rustc-link-lib` from this script reaches the library's own
    // links but, observed in practice, not the binary's — the bin link
    // fails with `undefined symbol: tree_sitter_dockerfile` while lib
    // tests link. Hand the archive to every link explicitly, by full path:
    // rustc appends `-C link-arg` values after all objects and libraries,
    // so resolution does not depend on flag propagation or on where the
    // archive lands in search order.
    let out = std::env::var("OUT_DIR").expect("OUT_DIR");
    let lib = if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        format!("{out}/tree_sitter_dockerfile.lib")
    } else {
        format!("{out}/libtree_sitter_dockerfile.a")
    };
    println!("cargo:rustc-link-arg={lib}");
}
