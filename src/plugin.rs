//! **Plugins: WASM modules that answer hooks.** Stage 1 answers `on_open` and `on_save` — a
//! file's text in, the text to use out — and the design is the one `plans/S10-wasm-hooks.md`
//! argues for: the host discovers what a module answers by **enumerating its exports**, so the
//! next hook is a host-side function and not an ABI change.
//!
//! # The ABI (v1)
//!
//! ```wat
//! (memory (export "memory") 1)                              ;; the guest's linear memory
//! (func (export "rano_abi") (result i32) i32.const 1)       ;; 1; anything else is refused
//! (func (export "rano_alloc") (param i32) (result i32))     ;; reserve n bytes -> offset
//! (func (export "on_open") (param i32 i32) (result i32))    ;; (in_ptr, in_len) -> block
//! (func (export "on_save") (param i32 i32) (result i32))
//! ```
//!
//! A hook returns the offset of an 8-byte **result block** — `out_ptr` and `out_len` as
//! little-endian `u32`s — or `0xFFFFFFFF` for **unchanged**. Two integers at an offset is the one
//! structure every language can write without agreeing on a struct layout, and the version export
//! is what lets a v2 arrive without guessing.
//!
//! # What this file is not
//!
//! Not the policy: **where** the hooks run (the open path, the save path, the loader's decision),
//! what a failure means to the reader, and the determinism rule are the editor's, in
//! `editor.rs`/`buffer.rs`. This module answers one question — "run this hook over this text" —
//! and every way that can fail is a `String` naming the reason.
//!
//! Not a sandbox the guest can leave: there are **no imports at all** in stage 1 (a module that
//! imports anything fails to instantiate, and the failure names the import), the guest gets
//! [`HOOK_FUEL`] fuel per call, its memory may not grow past [`HOOK_MEMORY`], and the text each way
//! is capped at [`MAX_HOOK_BYTES`]. A capability (the first would be `read-dir`, for a folder
//! listing as a plugin) is a deliberate addition, not a switch.

use std::path::{Path, PathBuf};

use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

/// **Fuel per call.** An infinite loop ends in `OutOfFuel` rather than in a hung editor, and it is
/// deterministic: the same guest does the same work for the same budget on any machine. Measured
/// on the probe (plans/S10): a trivial call burns 4, a 16 MiB uppercase loop burns a few million —
/// so this is a ceiling with room, not a tight budget.
pub const HOOK_FUEL: u64 = 50_000_000;

/// **Guest memory per call**, refused by the store's limiter when a module grows past it. A hook
/// works on a text that is at most [`MAX_HOOK_BYTES`] each way, so this is several times what a
/// working hook needs and well under a decompression bomb.
pub const HOOK_MEMORY: usize = 64 << 20;

/// **The largest text handed to a hook, and the largest it may hand back.** Symmetric on purpose:
/// a 1-line file must not become a gigabyte, and the copy through guest memory stays bounded.
pub const MAX_HOOK_BYTES: usize = 16 << 20;

/// The ABI this host speaks. A module that says anything else is refused rather than guessed at.
pub const ABI: i32 = 1;

/// The sentinel a hook returns to mean "leave the text as it is".
const UNCHANGED: i32 = -1;

/// **A loaded plugin**: its module, compiled once, and the hooks it answers.
#[derive(Debug)]
pub struct Plugin {
    engine: Engine,
    module: Module,
    path: PathBuf,
    /// The export names that are hooks — `on_open`, `on_save`, and whatever a later version adds.
    hooks: Vec<String>,
}

impl Plugin {
    /// **Load a `.wasm` file.** The module is compiled here (milliseconds, once) and its ABI and
    /// exports are checked here too, so a module that would fail on every save fails at load with
    /// one sentence instead.
    pub fn load(path: &Path) -> Result<Plugin, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_bytes(&bytes, path.to_path_buf())
    }

    /// The same, from bytes — the seam the WAT fixtures in the tests come through, so a test does
    /// not need a `wasm32` toolchain or a committed binary to exercise the ABI.
    pub fn from_bytes(bytes: &[u8], path: PathBuf) -> Result<Plugin, String> {
        let mut config = Config::new();
        // Fuel is what makes a hostile or simply broken loop a bounded cost.
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|e| format!("engine: {e}"))?;
        let module = Module::new(&engine, bytes).map_err(|e| format!("not a wasm module: {e}"))?;
        // **The ABI is checked by calling it**, not by looking for the export: a module that
        // exports the name and answers 2 is a v2 module and must not be run as v1.
        let mut store = Self::store(&engine);
        let instance = Linker::new(&engine)
            .instantiate(&mut store, &module)
            .map_err(|e| match Self::refusal(&e) {
                Some(why) => why,
                None => format!("cannot instantiate: {e}"),
            })?;
        let abi = instance
            .get_typed_func::<(), i32>(&mut store, "rano_abi")
            .map_err(|_| "no `rano_abi` export (wanted a function returning 1)".to_string())?;
        let got = abi
            .call(&mut store, ())
            .map_err(|e| format!("`rano_abi` failed: {e}"))?;
        if got != ABI {
            return Err(format!("ABI {got}, this rano speaks {ABI}"));
        }
        // The hooks are whatever the module exports under the naming convention.
        let hooks: Vec<String> = module
            .exports()
            .map(|e| e.name().to_string())
            .filter(|n| n.starts_with("on_") || n.starts_with("provider_"))
            .collect();
        Ok(Plugin {
            engine,
            module,
            path,
            hooks,
        })
    }

    /// The file it was loaded from (for a message that names the plugin).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The hooks it answers, in export order.
    pub fn hooks(&self) -> &[String] {
        &self.hooks
    }

    /// Whether it answers `hook`.
    pub fn has(&self, hook: &str) -> bool {
        self.hooks.iter().any(|h| h == hook)
    }

    /// `on_open`: the text a file should open with, or `None` when the hook left it alone.
    pub fn open(&self, text: &str) -> Result<Option<String>, String> {
        self.call("on_open", text)
    }

    /// `on_save`: the text to write, or `None` when the hook left it alone.
    pub fn save(&self, text: &str) -> Result<Option<String>, String> {
        self.call("on_save", text)
    }

    /// **Run one hook over `text`.** One fresh store per call: the fuel and the memory budget are
    /// per call, and nothing a guest allocates can accumulate across saves.
    fn call(&self, hook: &str, text: &str) -> Result<Option<String>, String> {
        if !self.has(hook) {
            return Ok(None);
        }
        if text.len() > MAX_HOOK_BYTES {
            return Err(format!(
                "{hook}: {} MiB is above the {} MiB cap",
                text.len() >> 20,
                MAX_HOOK_BYTES >> 20
            ));
        }
        let mut store = Self::store(&self.engine);
        let instance = Linker::new(&self.engine)
            .instantiate(&mut store, &self.module)
            .map_err(|e| match Self::refusal(&e) {
                Some(why) => why,
                None => format!("cannot instantiate: {e}"),
            })?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| "no `memory` export".to_string())?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "rano_alloc")
            .map_err(|_| "no `rano_alloc` export (wanted (i32) -> i32)".to_string())?;
        let f = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, hook)
            .map_err(|_| format!("`{hook}` has the wrong signature (wanted (i32, i32) -> i32)"))?;
        // The input goes where the guest asked for it, and a guest that answers with an offset
        // outside its own memory is a broken hook, not a transformed file.
        let at = alloc
            .call(&mut store, text.len() as i32)
            .map_err(|e| format!("`rano_alloc` failed: {e}"))?;
        let at = usize::try_from(at)
            .map_err(|_| "`rano_alloc` answered a negative offset".to_string())?;
        {
            let data = memory.data_mut(&mut store);
            let end = at
                .checked_add(text.len())
                .filter(|end| *end <= data.len())
                .ok_or_else(|| "`rano_alloc` answered outside the guest's memory".to_string())?;
            data[at..end].copy_from_slice(text.as_bytes());
        }
        let got = f
            .call(&mut store, (at as i32, text.len() as i32))
            .map_err(|e| format!("trapped: {e}"))?;
        if got == UNCHANGED {
            return Ok(None);
        }
        // The result block: two little-endian u32s at the offset the hook answered.
        let block =
            usize::try_from(got).map_err(|_| format!("`{hook}` answered a negative offset"))?;
        let data = memory.data(&store);
        let head = block
            .checked_add(8)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| format!("`{hook}` answered outside the guest's memory"))?;
        let out_ptr = u32::from_le_bytes(data[block..block + 4].try_into().unwrap()) as usize;
        let out_len = u32::from_le_bytes(data[block + 4..head].try_into().unwrap()) as usize;
        if out_len == 0 {
            return Ok(None);
        }
        let end = out_ptr
            .checked_add(out_len)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| format!("`{hook}` answered {out_len} bytes outside its memory"))?;
        let out = std::str::from_utf8(&data[out_ptr..end])
            .map_err(|_| format!("`{hook}` answered bytes that are not text"))?;
        if out.len() > MAX_HOOK_BYTES {
            return Err(format!(
                "`{hook}` answered {} MiB, above the {} MiB cap",
                out.len() >> 20,
                MAX_HOOK_BYTES >> 20
            ));
        }
        Ok(Some(out.to_string()))
    }

    /// A store with the budget this host allows a hook: fuel, and a memory cap the guest's own
    /// `memory.grow` cannot pass.
    fn store(engine: &Engine) -> Store<StoreLimits> {
        let limits = StoreLimitsBuilder::new().memory_size(HOOK_MEMORY).build();
        let mut store = Store::new(engine, limits);
        store.limiter(|s| s);
        // A store without fuel would be unbounded; `consume_fuel(true)` at the engine makes this
        // the one place the budget is set.
        let _ = store.set_fuel(HOOK_FUEL);
        store
    }

    /// **Why a module could not be instantiated, when the reason is the sandbox rather than a
    /// mistake**: an import is the one thing a stage-1 plugin may not have, and the message says
    /// which import and that none are granted.
    fn refusal(e: &wasmtime::Error) -> Option<String> {
        let text = e.to_string();
        if text.contains("unknown import") || text.contains("incompatible import type") {
            Some(format!(
                "imports are not granted to a plugin in stage 1 ({text})"
            ))
        } else if text.contains("out of fuel") {
            Some("out of fuel".to_string())
        } else {
            None
        }
    }

    /// A one-line description for a status line: the file and what it answers.
    pub fn summary(&self) -> String {
        if self.hooks.is_empty() {
            format!("{} (no hooks)", self.path.display())
        } else {
            format!("{} ({})", self.path.display(), self.hooks.join(", "))
        }
    }
}

/// A WAT fixture, compiled with the `wat` dev-dependency. Tests only: production loads `.wasm`
/// bytes, so no WAT parser rides in the binary.
#[cfg(test)]
fn wat(src: &str) -> Vec<u8> {
    wat::parse_str(src).expect("the fixture is valid wat")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(src: &str) -> Plugin {
        Plugin::from_bytes(&wat(src), PathBuf::from("fixture.wasm")).expect("loads")
    }

    /// The shape every fixture starts from: memory, the ABI, an allocator that hands out a fixed
    /// arena at offset 1024 (nothing here needs a real one).
    const BASE: &str = r#"
        (memory (export "memory") 4)
        (func (export "rano_abi") (result i32) i32.const 1)
        (func (export "rano_alloc") (param i32) (result i32) i32.const 1024)
    "#;

    #[test]
    fn a_module_without_rano_abi_is_refused() {
        let e = Plugin::from_bytes(
            &wat("(module (memory (export \"memory\") 1))"),
            PathBuf::from("x.wasm"),
        )
        .unwrap_err();
        assert!(e.contains("rano_abi"), "{e}");
    }

    #[test]
    fn a_module_for_another_abi_is_refused_by_number() {
        let e = Plugin::from_bytes(
            &wat(r#"(module
                (memory (export "memory") 1)
                (func (export "rano_abi") (result i32) i32.const 2))"#),
            PathBuf::from("x.wasm"),
        )
        .unwrap_err();
        assert!(e.contains("ABI 2") && e.contains("speaks 1"), "{e}");
    }

    #[test]
    fn a_module_that_imports_anything_is_refused() {
        let e = Plugin::from_bytes(
            &wat(r#"(module
                (import "rano" "read_dir" (func (param i32 i32 i32 i32) (result i32)))
                (memory (export "memory") 1)
                (func (export "rano_abi") (result i32) i32.const 1))"#),
            PathBuf::from("x.wasm"),
        )
        .unwrap_err();
        assert!(e.contains("imports are not granted"), "{e}");
    }

    #[test]
    fn the_sentinel_leaves_the_text_alone() {
        let p = plugin(&format!(
            r#"{BASE}
            (func (export "on_save") (param i32 i32) (result i32) i32.const -1)"#
        ));
        assert_eq!(p.save("hello").unwrap(), None);
        assert_eq!(p.open("hello").unwrap(), None, "a hook it does not export");
        assert_eq!(
            p.hooks(),
            vec!["on_save".to_string()],
            "discovered by export"
        );
    }

    #[test]
    fn a_result_block_replaces_the_text() {
        // Answers the block {1024, 5} — the input's own bytes, unchanged in place — so a save
        // through it returns exactly the text it was given, by the block path rather than the
        // sentinel.
        let p = plugin(&format!(
            r#"{BASE}
            (func (export "on_save") (param i32 i32) (result i32)
              (i32.store (i32.const 2048) (i32.const 1024))
              (i32.store (i32.const 2052) (i32.const 5))
              i32.const 2048)"#
        ));
        assert_eq!(p.save("hello").unwrap().as_deref(), Some("hello"));
    }

    #[test]
    fn an_infinite_loop_runs_out_of_fuel() {
        let p = plugin(&format!(
            r#"{BASE}
            (func (export "on_save") (param i32 i32) (result i32)
              (loop $l (br $l)) (i32.const -1))"#
        ));
        let t = std::time::Instant::now();
        let e = p.save("hello").unwrap_err();
        assert!(e.contains("fuel") || e.contains("trapped"), "{e}");
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "bounded");
    }

    #[test]
    fn output_that_is_not_text_is_refused() {
        // The block points at 0xFF bytes: not UTF-8.
        let p = plugin(&format!(
            r#"{BASE}
            (func (export "on_save") (param i32 i32) (result i32)
              (i32.store8 (i32.const 1024) (i32.const 255))
              (i32.store (i32.const 2048) (i32.const 1024))
              (i32.store (i32.const 2052) (i32.const 1))
              i32.const 2048)"#
        ));
        let e = p.save("hello").unwrap_err();
        assert!(e.contains("not text"), "{e}");
    }

    #[test]
    fn a_block_pointing_outside_the_memory_is_refused() {
        let p = plugin(&format!(
            r#"{BASE}
            (func (export "on_save") (param i32 i32) (result i32)
              (i32.store (i32.const 2048) (i32.const 999999))
              (i32.store (i32.const 2052) (i32.const 5))
              i32.const 2048)"#
        ));
        let e = p.save("hello").unwrap_err();
        assert!(e.contains("outside"), "{e}");
    }

    #[test]
    fn a_text_above_the_cap_is_refused_before_the_guest_sees_it() {
        let p = plugin(&format!(
            r#"{BASE}
            (func (export "on_save") (param i32 i32) (result i32) i32.const -1)"#
        ));
        let big = "a".repeat(MAX_HOOK_BYTES + 1);
        let e = p.save(&big).unwrap_err();
        assert!(e.contains("cap"), "{e}");
    }

    /// **The committed example module loads and transforms** — the artifact half of
    /// `examples/plugin-upper`, so a change to the ABI that breaks real modules is caught here.
    #[test]
    fn the_example_module_loads_and_uppercases() {
        let p = Plugin::from_bytes(
            include_bytes!("../examples/plugin-upper/plugin.wasm"),
            PathBuf::from("plugin-upper.wasm"),
        )
        .expect("the example loads");
        assert_eq!(
            p.hooks(),
            vec!["on_open".to_string(), "on_save".to_string()]
        );
        assert_eq!(p.open("hello rano").unwrap().as_deref(), Some("HELLO RANO"));
        assert_eq!(p.save("abc").unwrap().as_deref(), Some("ABC"));
    }

    #[test]
    fn the_summary_names_the_file_and_the_hooks() {
        let p = plugin(&format!(
            r#"{BASE}
            (func (export "on_open") (param i32 i32) (result i32) i32.const -1)
            (func (export "on_save") (param i32 i32) (result i32) i32.const -1)"#
        ));
        let s = p.summary();
        assert!(
            s.contains("fixture.wasm") && s.contains("on_open") && s.contains("on_save"),
            "{s}"
        );
    }
}
