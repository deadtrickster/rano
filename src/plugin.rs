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
//! **An `on_*` export is a text hook, a `provider_*` export takes the viewport too, and a host
//! that speaks other hook kinds names their prefixes at load** ([`Plugin::load_hooks`]) — a
//! `pane_dashboard` is the same `(i32, i32) -> i32` call as `on_save`, so a new kind of hook
//! costs a host-side list and not an ABI version.
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
use std::sync::atomic::{AtomicU64, Ordering};

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

/// **What the host lets a plugin reach beyond its own text.** Stage 1 grants nothing by default;
/// a folder viewer needs `read-dir`, and granting it is a deliberate act — the config says
/// `capabilities = ["read-dir"]`, and a module that imports anything else is refused at
/// instantiation, by name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// The `rano.read_dir` import: **the text of a folder's listing**, rendered by the host with
    /// the same rules the built-in dired uses (`dirview::rows`: one entry per row, directories
    /// first and marked `name/`). The plugin decides *whether* and *when* to ask; the format is
    /// the host's, so a provider and the Rust listing cannot disagree about what a row means.
    pub read_dir: bool,
}

impl Capabilities {
    /// From the config's `capabilities = ["read-dir"]` list: names outside this set are ignored
    /// there (a misspelt capability is not a grant), and the empty list grants nothing.
    pub fn from_names(names: &[String]) -> Capabilities {
        Capabilities {
            read_dir: names
                .iter()
                .any(|n| n.trim().eq_ignore_ascii_case("read-dir")),
        }
    }
}

/// **A loaded plugin**: its module, compiled once, and the hooks it answers.
#[derive(Debug)]
pub struct Plugin {
    engine: Engine,
    module: Module,
    path: PathBuf,
    /// The export names that are hooks — `on_open`, `on_save`, and whatever a later version adds.
    hooks: Vec<String>,
    /// What the config let it import.
    caps: Capabilities,
    /// Fuel the last [`Plugin::call`] burned. An atomic rather than a `Mutex` because it is one
    /// number written once per call and read by a status line, and `call` takes `&self`.
    fuel: AtomicU64,
}

impl Plugin {
    /// **Load a `.wasm` file.** The module is compiled here (milliseconds, once) and its ABI and
    /// exports are checked here too, so a module that would fail on every save fails at load with
    /// one sentence instead.
    pub fn load(path: &Path) -> Result<Plugin, String> {
        Self::load_with(path, Capabilities::default())
    }

    /// [`Self::load`] with the capabilities the config granted: the module may import exactly
    /// those, and a module that imports anything else is refused here rather than at the first
    /// save.
    pub fn load_with(path: &Path, caps: Capabilities) -> Result<Plugin, String> {
        Self::load_hooks(path, caps, &[])
    }

    /// [`Self::load`] with the capabilities the config granted and the hook prefixes the host
    /// recognises — see [`Self::from_bytes_hooks`] for what a prefix means.
    pub fn load_hooks(
        path: &Path,
        caps: Capabilities,
        prefixes: &[&str],
    ) -> Result<Plugin, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_bytes_hooks(&bytes, path.to_path_buf(), caps, prefixes)
    }

    /// The same, from bytes — the seam the WAT fixtures in the tests come through, so a test does
    /// not need a `wasm32` toolchain or a committed binary to exercise the ABI.
    pub fn from_bytes(bytes: &[u8], path: PathBuf) -> Result<Plugin, String> {
        Self::from_bytes_with(bytes, path, Capabilities::default())
    }

    /// [`Self::from_bytes`] under granted capabilities.
    pub fn from_bytes_with(
        bytes: &[u8],
        path: PathBuf,
        caps: Capabilities,
    ) -> Result<Plugin, String> {
        Self::from_bytes_hooks(bytes, path, caps, &[])
    }

    /// [`Self::from_bytes`] with both the capabilities and the **hook prefixes** the host
    /// recognises: `on_` and `provider_` are always hooks, and anything named under `prefixes`
    /// (letibot passes `pane_`, `condition_`, `notice_`, `digest_`, `link_`, `command_`, `tool_`)
    /// is a text hook — `(i32, i32) -> i32`, refused here if it is not.
    ///
    /// The list is the host's, not the module's: a module cannot widen what it answers by naming
    /// an export `pane_x`, because a host that does not know `pane_` does not see a hook at all.
    /// An empty prefix is ignored rather than honoured, so a host's mis-built list cannot turn
    /// every export — `rano_alloc` included — into a hook the load would then refuse.
    pub fn from_bytes_hooks(
        bytes: &[u8],
        path: PathBuf,
        caps: Capabilities,
        prefixes: &[&str],
    ) -> Result<Plugin, String> {
        let mut config = Config::new();
        // Fuel is what makes a hostile or simply broken loop a bounded cost.
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|e| format!("engine: {e}"))?;
        let module = Module::new(&engine, bytes).map_err(|e| format!("not a wasm module: {e}"))?;
        // **The ABI is checked by calling it**, not by looking for the export: a module that
        // exports the name and answers 2 is a v2 module and must not be run as v1.
        let mut store = Self::store(&engine);
        let instance = Self::linker(&engine, caps)
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
        // The hooks are whatever the module exports under the naming convention — and each one's
        // SIGNATURE is checked here, so a module whose hook the host could never call is refused at
        // load rather than failing the first save. (A plugin must not name an unrelated export
        // `on_…`: the prefix *is* the convention.) A prefix the host passed in is the same kind of
        // claim as `on_`, so it is held to the same signature.
        let mut hooks: Vec<String> = Vec::new();
        for e in module.exports() {
            let name = e.name().to_string();
            let provider = name.starts_with("provider_");
            let text = name.starts_with("on_")
                || prefixes
                    .iter()
                    .any(|p| !p.is_empty() && name.starts_with(p));
            if !(provider || text) {
                continue;
            }
            if provider {
                if instance
                    .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, &name)
                    .is_err()
                {
                    return Err(format!(
                        "`{name}` has the wrong signature (a provider is (i32, i32, i32, i32) -> i32)"
                    ));
                }
            } else if instance
                .get_typed_func::<(i32, i32), i32>(&mut store, &name)
                .is_err()
            {
                return Err(format!(
                    "`{name}` has the wrong signature (a hook is (i32, i32) -> i32)"
                ));
            }
            hooks.push(name);
        }
        Ok(Plugin {
            engine,
            module,
            path,
            hooks,
            caps,
            fuel: AtomicU64::new(0),
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

    /// **Fuel the last [`Self::call`] burned** — `HOOK_FUEL` minus what the store had left, so a
    /// hook that ran out reads as the whole budget. A host that reports slowness reports this
    /// beside the time, because fuel is the engine-independent measure of the same work.
    pub fn last_fuel(&self) -> u64 {
        self.fuel.load(Ordering::Relaxed)
    }

    /// **`provider_open`: what this path should open as**, or `None` when the plugin does not
    /// claim it (the host then opens it the ordinary way). This is the hook a folder viewer
    /// answers — and the one that needs a capability, because the decision is about a path and not
    /// a text.
    ///
    /// **It is told the viewport** (`cols`×`rows`), because a provider's answer is laid out at the
    /// width it will be drawn at: rano is width-adaptive throughout, and a plugin that renders
    /// columns has to be told how many there are. `on_open`/`on_save` take a text and need no
    /// width; a provider takes a path and does.
    pub fn provider_open(
        &self,
        path: &str,
        cols: u32,
        rows: u32,
    ) -> Result<Option<String>, String> {
        self.call_sized("provider_open", path, cols as i32, rows as i32)
    }

    /// The linker a module is instantiated with: **one import per granted capability, and nothing
    /// else**. A module that imports anything not here fails with the import named.
    fn linker(engine: &Engine, caps: Capabilities) -> Linker<StoreLimits> {
        let mut linker = Linker::new(engine);
        if caps.read_dir {
            // `(path_ptr, path_len, out_ptr, out_cap) -> i32`: bytes written, `-needed` for a
            // buffer too small, or `i32::MIN` for an error. The listing itself is the host's
            // rendering — `dirview::rows`, the same one the built-in dired uses — so a provider
            // and the Rust listing cannot disagree about what a row means.
            let f = |mut caller: wasmtime::Caller<'_, StoreLimits>,
                     a: u32,
                     b: u32,
                     c: u32,
                     d: u32|
             -> i32 {
                let Some(wasmtime::Extern::Memory(mem)) = caller.get_export("memory") else {
                    return i32::MIN;
                };
                let data = mem.data(&caller);
                let (Ok(lo), Ok(hi)) = (usize::try_from(a), usize::try_from(b)) else {
                    return i32::MIN;
                };
                let Some(hi) = lo.checked_add(hi).filter(|e| *e <= data.len()) else {
                    return i32::MIN;
                };
                let Ok(path) = std::str::from_utf8(&data[lo..hi]) else {
                    return i32::MIN;
                };
                let Ok(rows) = crate::dirview::rows(Path::new(path)) else {
                    return i32::MIN;
                };
                let text = format!("{}\n", rows.join("\n"));
                let out = usize::try_from(c).unwrap_or(usize::MAX);
                if d as usize >= text.len() && out.saturating_add(text.len()) <= data.len() {
                    let mem = mem.data_mut(&mut caller);
                    mem[out..out + text.len()].copy_from_slice(text.as_bytes());
                    return text.len() as i32;
                }
                -(text.len() as i32)
            };
            let _ = linker.func_wrap("rano", "read_dir", f);
        }
        linker
    }

    /// **Run any text hook**: `Ok(None)` when the plugin does not answer it or the hook left the
    /// text alone. One fresh store per call: the fuel and the memory budget are per call, and
    /// nothing a guest allocates can accumulate across saves.
    pub fn call(&self, hook: &str, text: &str) -> Result<Option<String>, String> {
        self.call_with(hook, text, None)
    }

    /// [`Self::call`] for a hook that takes the viewport too — a provider, whose answer is laid out
    /// at the width it will be drawn at: `(ptr, len, cols, rows) -> block`.
    fn call_sized(
        &self,
        hook: &str,
        text: &str,
        cols: i32,
        rows: i32,
    ) -> Result<Option<String>, String> {
        self.call_with(hook, text, Some((cols, rows)))
    }

    /// **One hook call.** The signature is two `i32`s for a text hook and four for a provider; the
    /// viewport, when there is one, is what picks which.
    fn call_with(
        &self,
        hook: &str,
        text: &str,
        viewport: Option<(i32, i32)>,
    ) -> Result<Option<String>, String> {
        // A call that never ran a guest must not report the previous call's fuel as its own.
        self.fuel.store(0, Ordering::Relaxed);
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
        let instance = Self::linker(&self.engine, self.caps)
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
        let got = match viewport {
            None => {
                let f = instance
                    .get_typed_func::<(i32, i32), i32>(&mut store, hook)
                    .map_err(|_| {
                        format!("`{hook}` has the wrong signature (wanted (i32, i32) -> i32)")
                    })?;
                f.call(&mut store, (at as i32, text.len() as i32))
            }
            Some((cols, rows)) => {
                let f = instance
                    .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, hook)
                    .map_err(|_| {
                        format!(
                            "`{hook}` has the wrong signature (wanted (i32, i32, i32, i32) -> i32)"
                        )
                    })?;
                f.call(&mut store, (at as i32, text.len() as i32, cols, rows))
            }
        };
        // Recorded before the trap is turned into a sentence: an out-of-fuel hook and a slow one
        // are the same measurement, and the gate wants the number either way.
        let left = store.get_fuel().unwrap_or(0);
        self.fuel
            .store(HOOK_FUEL.saturating_sub(left), Ordering::Relaxed);
        let got = got.map_err(|e| format!("trapped: {e}"))?;
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

    /// **The dired example, through the ABI**: without the grant its import is refused (the
    /// sandbox is real), and with it `provider_open` answers a folder's listing — the same rows
    /// the built-in dired renders.
    #[test]
    fn the_dired_example_needs_read_dir_and_then_lists_a_folder() {
        let wasm: &[u8] = include_bytes!("../examples/plugin-dired/plugin.wasm");
        // Ungranted: refused at load, and the sentence names the import, not the plugin's wishes.
        let e = Plugin::from_bytes_with(wasm, PathBuf::from("dired.wasm"), Capabilities::default())
            .unwrap_err();
        assert!(e.contains("imports are not granted"), "{e}");
        // Granted: the folder is claimed, and the answer is the host's listing.
        let p = Plugin::from_bytes_with(
            wasm,
            PathBuf::from("dired.wasm"),
            Capabilities { read_dir: true },
        )
        .expect("loads with the grant");
        assert_eq!(p.hooks(), vec!["provider_open".to_string()]);
        let dir = std::env::temp_dir().join(format!("rano_dired_example_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let listing = p
            .provider_open(&dir.to_string_lossy(), 80, 24)
            .expect("no failure")
            .expect("a folder is claimed");
        assert_eq!(
            listing,
            format!("{}\n", crate::dirview::rows(&dir).unwrap().join("\n"))
        );
        // A file is not claimed: `provider_open` answers the sentinel, which is `None` here.
        assert_eq!(
            p.provider_open(&dir.join("a.txt").to_string_lossy(), 80, 24)
                .unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **What a hook call costs after the module is compiled** — the number the save path pays.
    #[test]
    fn a_hook_call_is_cheap_once_the_module_is_compiled() {
        let p = Plugin::from_bytes_with(
            include_bytes!("../examples/plugin-upper/plugin.wasm"),
            PathBuf::from("upper.wasm"),
            Capabilities::default(),
        )
        .expect("loads");
        // Warm: one call to register, then time a run of them.
        p.save("hello").unwrap();
        let n = 20;
        let t = std::time::Instant::now();
        for _ in 0..n {
            assert_eq!(p.save("hello").unwrap().as_deref(), Some("HELLO"));
        }
        let per = t.elapsed() / n;
        eprintln!("per hook call: {per:?}");
        assert!(
            per < std::time::Duration::from_millis(20),
            "a hook call should be well under a frame: {per:?}"
        );
    }

    /// **The TypeScript example** (AssemblyScript, built by `asc`): the same plugin as the Rust one,
    /// through the same ABI — the point being that a plugin is not a wasm-authoring problem.
    #[test]
    fn the_typescript_example_loads_and_uppercases() {
        let p = Plugin::from_bytes(
            include_bytes!("../examples/plugin-upper-as/plugin.wasm"),
            PathBuf::from("upper-as.wasm"),
        )
        .expect("the AssemblyScript example loads");
        assert_eq!(
            p.hooks(),
            vec!["on_open".to_string(), "on_save".to_string()]
        );
        assert_eq!(p.open("hello rano").unwrap().as_deref(), Some("HELLO RANO"));
        assert_eq!(p.save("abc").unwrap().as_deref(), Some("ABC"));
    }

    /// **The TypeScript dired**: the same plugin as the Rust one, in AssemblyScript — refused
    /// without the grant, listing a folder with it, not claiming a file.
    #[test]
    fn the_typescript_dired_needs_read_dir_and_then_lists_a_folder() {
        let wasm: &[u8] = include_bytes!("../examples/plugin-dired-as/plugin.wasm");
        let e = Plugin::from_bytes_with(
            wasm,
            PathBuf::from("dired-as.wasm"),
            Capabilities::default(),
        )
        .unwrap_err();
        assert!(e.contains("imports are not granted"), "{e}");
        let p = Plugin::from_bytes_with(
            wasm,
            PathBuf::from("dired-as.wasm"),
            Capabilities { read_dir: true },
        )
        .expect("loads with the grant");
        assert_eq!(p.hooks(), vec!["provider_open".to_string()]);
        let dir = std::env::temp_dir().join(format!("rano_dired_as_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let listing = p
            .provider_open(&dir.to_string_lossy(), 80, 24)
            .expect("no failure")
            .expect("a folder is claimed");
        assert_eq!(
            listing,
            format!("{}\n", crate::dirview::rows(&dir).unwrap().join("\n"))
        );
        assert_eq!(
            p.provider_open(&dir.join("a.txt").to_string_lossy(), 80, 24)
                .unwrap(),
            None,
            "a file is not claimed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn capabilities_are_named_not_guessed() {
        let names = vec!["read-dir".to_string()];
        assert!(Capabilities::from_names(&names).read_dir);
        assert!(Capabilities::from_names(&["READ-DIR".to_string()]).read_dir);
        assert!(!Capabilities::from_names(&["read_file".to_string()]).read_dir);
        assert!(!Capabilities::from_names(&[]).read_dir);
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

    /// A pane hook as letibot writes one: `(i32, i32) -> i32`, answering a fixed block. The text it
    /// answers (`"pane:x"`, at 4096) is not the input, so the test can tell the two apart.
    const PANE: &str = r#"
        (memory (export "memory") 4)
        (func (export "rano_abi") (result i32) i32.const 1)
        (func (export "rano_alloc") (param i32) (result i32) i32.const 1024)
        (data (i32.const 4096) "pane:x")
        (func (export "pane_x") (param i32 i32) (result i32)
          (i32.store (i32.const 8192) (i32.const 4096))
          (i32.store (i32.const 8196) (i32.const 6))
          i32.const 8192)
    "#;

    /// **A prefix is the host's claim, not the module's**: without `pane_` passed, `pane_x` is an
    /// ordinary export that answers nothing; with it, the same module is a pane hook.
    #[test]
    fn a_prefixed_export_is_a_hook_only_when_the_host_names_the_prefix() {
        let p = plugin(PANE);
        assert!(p.hooks().is_empty(), "{:?}", p.hooks());
        assert_eq!(p.call("pane_x", "hello").unwrap(), None);

        let p = Plugin::from_bytes_hooks(
            &wat(PANE),
            PathBuf::from("pane.wasm"),
            Capabilities::default(),
            &["pane_"],
        )
        .expect("loads");
        assert_eq!(p.hooks(), vec!["pane_x".to_string()]);
        assert_eq!(
            p.call("pane_x", "hello").unwrap().as_deref(),
            Some("pane:x")
        );
    }

    /// The prefixes are added beside the two the ABI already defines, and hide neither.
    #[test]
    fn prefixes_are_added_beside_on_and_provider() {
        let src = format!(
            r#"{BASE}
            (func (export "on_save") (param i32 i32) (result i32) i32.const -1)
            (func (export "provider_open") (param i32 i32 i32 i32) (result i32) i32.const -1)"#
        );
        let p = Plugin::from_bytes_hooks(
            &wat(&src),
            PathBuf::from("x.wasm"),
            Capabilities::default(),
            &["pane_"],
        )
        .expect("loads");
        assert_eq!(
            p.hooks(),
            vec!["on_save".to_string(), "provider_open".to_string()]
        );
    }

    /// A prefixed export the host could never call is refused **at load, naming it** — the same rule
    /// `on_` already has, for the same reason: a hook that fails on every call is a plugin that
    /// fails at the first frame instead of at load.
    #[test]
    fn a_prefixed_export_with_the_wrong_signature_is_refused_naming_it() {
        let src = r#"(module
            (memory (export "memory") 1)
            (func (export "rano_abi") (result i32) i32.const 1)
            (func (export "pane_x") (param i32) (result i32) i32.const 0))"#;
        let e = Plugin::from_bytes_hooks(
            &wat(src),
            PathBuf::from("pane.wasm"),
            Capabilities::default(),
            &["pane_"],
        )
        .unwrap_err();
        assert!(e.contains("pane_x") && e.contains("signature"), "{e}");
        // The same module without the prefix names nothing to check, so it loads.
        Plugin::from_bytes_hooks(
            &wat(src),
            PathBuf::from("pane.wasm"),
            Capabilities::default(),
            &[],
        )
        .expect("loads when nothing claims pane_");
    }

    /// [`Plugin::load_hooks`] is the file half of the same thing: a name in the list is what makes a
    /// pane hook reachable from a `.wasm` on disk.
    #[test]
    fn load_hooks_sees_the_prefixed_hooks_of_a_file() {
        let path = std::env::temp_dir().join(format!("rano_pane_{}.wasm", std::process::id()));
        std::fs::write(&path, wat(PANE)).unwrap();
        let p = Plugin::load_hooks(&path, Capabilities::default(), &["pane_"]).expect("loads");
        assert_eq!(p.hooks(), vec!["pane_x".to_string()]);
        assert_eq!(
            p.call("pane_x", "hello").unwrap().as_deref(),
            Some("pane:x")
        );
        let _ = std::fs::remove_file(&path);
    }

    /// **`last_fuel` is the call's cost**, which is what the gate reports beside the wall time. A
    /// hook that was not called reports nothing rather than the previous call's number.
    #[test]
    fn last_fuel_is_what_the_last_call_burned() {
        let p = plugin(&format!(
            r#"{BASE}
            (func (export "on_save") (param i32 i32) (result i32) i32.const -1)"#
        ));
        assert_eq!(p.last_fuel(), 0, "no call has run yet");
        p.save("hello").unwrap();
        let burnt = p.last_fuel();
        assert!(burnt > 0 && burnt <= HOOK_FUEL, "{burnt}");
        p.call("on_open", "hello").unwrap();
        assert_eq!(p.last_fuel(), 0, "a hook it does not export is not a call");
    }
}
