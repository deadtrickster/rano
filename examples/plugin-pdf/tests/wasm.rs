//! **The committed `plugin.wasm`, checked as an artifact.**
//!
//! The reader's tests run natively and never touch the module the host will actually load, so the
//! two things that decide whether that module can be *used at all* are checked here, out of the
//! file: the ABI's exports are present, and every import it declares is one of the two page
//! renderers — a module that imported anything else would be refused at instantiation and could
//! not even say why.
//!
//! It also pins **which build is committed**: the default features carry no imports, because
//! `rano.convert_page` and `rano.convert_page_text` are not in the host yet. Rebuild with
//! `--features page-png,page-text` when they land, and change the one line below that says so.

use std::path::PathBuf;

/// The exports a host needs to run this plugin: the ABI's two, the three hooks, and the memory the
/// ABI's pointers are into.
const EXPORTS: [&str; 6] = [
    "memory",
    "rano_abi",
    "rano_alloc",
    "provider_open",
    "provider_picture",
    "on_key",
];

/// The imports this plugin may ever declare.
const IMPORTS: [&str; 2] = ["convert_page", "convert_page_text"];

fn module() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugin.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e} (build it first)", path.display()))
}

#[test]
fn the_committed_module_exports_the_abi_and_the_three_hooks() {
    let found = sections(&module());
    for name in EXPORTS {
        assert!(
            found.exports.iter().any(|e| e == name),
            "no `{name}` export"
        );
    }
}

#[test]
fn every_import_it_declares_is_a_page_renderer_from_rano() {
    let found = sections(&module());
    for (module_name, field) in &found.imports {
        assert_eq!(module_name, "rano", "an import from {module_name:?}");
        assert!(IMPORTS.contains(&field.as_str()), "an import of `{field}`");
    }
}

#[test]
fn the_committed_build_is_the_one_that_runs_against_todays_host() {
    // The default features carry no imports. When the host grows `convert_page` and
    // `convert_page_text`, rebuild `plugin.wasm` with
    // `--features page-png,page-text` and flip this to expect two imports.
    let found = sections(&module());
    assert!(
        found.imports.is_empty(),
        "the committed plugin.wasm imports {:?}: it was built with the page renderers, so either \
         the host has them now (flip this test) or it was rebuilt by mistake",
        found.imports
    );
    assert!(found.exports.len() >= EXPORTS.len(), "{:?}", found.exports);
}

// ---------------------------------------------------------------- the wasm binary

/// The two sections a host's load depends on: what the module imports and what it exports.
#[derive(Debug, Default)]
struct Sections {
    imports: Vec<(String, String)>,
    exports: Vec<String>,
}

fn sections(bytes: &[u8]) -> Sections {
    assert_eq!(&bytes[..4], b"\0asm", "not a wasm module");
    let mut found = Sections::default();
    let mut at = 8;
    while at < bytes.len() {
        let id = bytes[at];
        at += 1;
        let size = leb(bytes, &mut at) as usize;
        let end = at + size;
        match id {
            // Import section: one entry a (module, name, kind, descriptor).
            2 => {
                let count = leb(bytes, &mut at);
                for _ in 0..count {
                    let module = name(bytes, &mut at);
                    let field = name(bytes, &mut at);
                    let kind = bytes[at];
                    at += 1;
                    assert_eq!(kind, 0, "a non-function import ({kind})");
                    let _type = leb(bytes, &mut at);
                    found.imports.push((module, field));
                }
            }
            // Export section: one entry a (name, kind, index).
            7 => {
                let count = leb(bytes, &mut at);
                for _ in 0..count {
                    let name = name(bytes, &mut at);
                    let _kind = bytes[at];
                    at += 1;
                    let _index = leb(bytes, &mut at);
                    found.exports.push(name);
                }
            }
            _ => {}
        }
        at = end;
    }
    found
}

/// A length-prefixed UTF-8 string, which is how wasm spells a name.
fn name(bytes: &[u8], at: &mut usize) -> String {
    let len = leb(bytes, at) as usize;
    let text = String::from_utf8(bytes[*at..*at + len].to_vec()).expect("a name is UTF-8");
    *at += len;
    text
}

/// LEB128, unsigned, which is how wasm spells every count and index.
fn leb(bytes: &[u8], at: &mut usize) -> u32 {
    let mut value = 0;
    let mut shift = 0;
    loop {
        let byte = bytes[*at];
        *at += 1;
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
        shift += 7;
    }
}
