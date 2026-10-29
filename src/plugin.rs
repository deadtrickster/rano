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
//! (func (export "provider_open") (param i32 i32 i32 i32) (result i32))    ;; path, cols, rows
//! (func (export "provider_picture") (param i32 i32 i32 i32) (result i32)) ;; path, cols, rows
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
//! is what lets a v2 arrive without guessing. What the block's bytes *mean* is the hook's
//! business: text for `on_*`/`provider_open`, an RGB picture for [`Plugin::provider_picture`].
//!
//! # What this file is not
//!
//! Not the policy: **where** the hooks run (the open path, the save path, the loader's decision),
//! what a failure means to the reader, and the determinism rule are the editor's, in
//! `editor.rs`/`buffer.rs`. This module answers one question — "run this hook over this text" —
//! and every way that can fail is a `String` naming the reason.
//!
//! Not a sandbox the guest can leave: a module imports **exactly what the config granted** and
//! nothing by default — a module that imports anything else fails to instantiate, naming the
//! import. The guest gets [`HOOK_FUEL`] fuel per call, its memory may not grow past
//! [`HOOK_MEMORY`], and the bytes each way are capped at [`MAX_HOOK_BYTES`]. A capability
//! ([`Capabilities`]) is a deliberate addition with a test, not a switch.

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

/// **What the host lets a plugin reach beyond its own text.** Nothing is granted by default; the
/// config says `capabilities = ["read-dir"]`, and a module that imports anything else is refused
/// at instantiation, by name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// The `rano.read_dir` import: **the text of a folder's listing**, rendered by the host with
    /// the same rules the built-in dired uses (`dirview::rows`: one entry per row, directories
    /// first and marked `name/`). The plugin decides *whether* and *when* to ask; the format is
    /// the host's, so a provider and the Rust listing cannot disagree about what a row means.
    pub read_dir: bool,
    /// The `rano.read_file` import: **the bytes of the one file this call is about**, because a
    /// provider is handed a path and a viewer has to read it itself.
    ///
    /// It is not a filesystem door. The host answers only for the path it asked the hook about,
    /// resolved first, so `..`, a relative spelling and a symlink all land on the same file and
    /// any *other* path — `/etc/shadow` included — is refused. A text hook is asked about a text
    /// rather than a path, so it has no file to scope to and `read_file` refuses there too.
    pub read_file: bool,
    /// **The `rano.convert_page` and `rano.convert_page_text` imports**: one page of the file
    /// this call is about, rendered or read by the host's own converters. A plugin that shows
    /// a document asks; the host's deadline, caps and scoping apply, so a grant cannot reach
    /// another document and a page cannot run unbounded.
    pub convert_page: bool,
}

impl Capabilities {
    /// From the config's `capabilities = ["read-dir"]` list: names outside this set are ignored
    /// there (a misspelt capability is not a grant), and the empty list grants nothing.
    pub fn from_names(names: &[String]) -> Capabilities {
        let has = |name: &str| names.iter().any(|n| n.trim().eq_ignore_ascii_case(name));
        Capabilities {
            read_dir: has("read-dir"),
            read_file: has("read-file"),
            convert_page: has("convert-page"),
        }
    }
}

/// **A picture a plugin answered**: `width`×`height` RGB pixels, row-major, three bytes each.
///
/// The ABI's payload is `[width u32 LE][height u32 LE][rgb]` — the same result block as every
/// other hook, with a header the host can check. [`Plugin::provider_picture`] decodes and validates
/// it, so a host that draws one never has to look at bytes it has not had checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    /// `width * height * 3` bytes, never more than [`MAX_HOOK_BYTES`].
    pub rgb: Vec<u8>,
}

impl Picture {
    /// One pixel, or `None` outside the picture.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = (y as usize * self.width as usize + x as usize) * 3;
        Some([self.rgb[at], self.rgb[at + 1], self.rgb[at + 2]])
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
        let instance = Self::linker(&engine, caps, None)
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

    /// **`provider_picture`: the picture this path should be drawn as**, or `None` when the plugin
    /// does not claim it. The call shape is [`Self::provider_open`]'s — `(path, cols, rows)` and
    /// the same result block — and the bytes are `[width u32 LE][height u32 LE][rgb]` rather than
    /// text: the hook for a viewer that is not a text view.
    ///
    /// **The bounds are the host's, not the plugin's to widen.** `width * height * 3` may not pass
    /// [`MAX_HOOK_BYTES`], and the header must describe exactly the bytes that came with it. An
    /// oversized or self-contradictory answer is an error — refused, never clipped to fit, because
    /// a picture silently cropped is a picture that lies about what the file looks like.
    pub fn provider_picture(
        &self,
        path: &str,
        cols: u32,
        rows: u32,
    ) -> Result<Option<Picture>, String> {
        let Some(bytes) =
            self.call_bytes("provider_picture", path, Some((cols as i32, rows as i32)))?
        else {
            return Ok(None);
        };
        Self::decode_picture(&bytes)
    }

    /// The picture payload, checked: a 12-byte header, a size inside the cap, and exactly the RGB
    /// bytes the header promised.
    fn decode_picture(bytes: &[u8]) -> Result<Option<Picture>, String> {
        if bytes.len() < 12 {
            return Err(format!(
                "`provider_picture` answered {} bytes, too short for the 12-byte header",
                bytes.len()
            ));
        }
        let width = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let height = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if width == 0 || height == 0 {
            return Err(format!(
                "`provider_picture` answered a {width}x{height} picture"
            ));
        }
        let want = u64::from(width) * u64::from(height) * 3;
        if want > MAX_HOOK_BYTES as u64 {
            return Err(format!(
                "`provider_picture` answered a {width}x{height} picture, {} MiB of RGB, above the {} MiB cap",
                want >> 20,
                MAX_HOOK_BYTES >> 20
            ));
        }
        let want = want as usize;
        if bytes.len() != 12 + want {
            return Err(format!(
                "`provider_picture` answered {} bytes for a {width}x{height} picture (wanted {})",
                bytes.len(),
                12 + want
            ));
        }
        Ok(Some(Picture {
            width,
            height,
            rgb: bytes[12..].to_vec(),
        }))
    }

    /// The linker a module is instantiated with: **one import per granted capability, and nothing
    /// else**. A module that imports anything not here fails with the import named.
    ///
    /// `scope` is the path this call is about, and it is what [`Capabilities::read_file`] is
    /// scoped to: the host resolves it once, here, and the import answers for that file and no
    /// other.
    fn linker(engine: &Engine, caps: Capabilities, scope: Option<PathBuf>) -> Linker<StoreLimits> {
        // **The one file this call is about**, resolved once: every scoped import answers for
        // it and for nothing else, so `..`, a relative spelling and a symlink all land on it.
        let scoped = scope.and_then(|p| std::fs::canonicalize(p).ok());
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
                let Some(path) = guest_path(&mut caller, a, b) else {
                    return i32::MIN;
                };
                let Ok(rows) = crate::dirview::rows(&path) else {
                    return i32::MIN;
                };
                let text = format!("{}\n", rows.join("\n"));
                write_guest(&mut caller, c, d, text.as_bytes())
            };
            let _ = linker.func_wrap("rano", "read_dir", f);
        }
        if caps.read_file {
            // `(path_ptr, path_len, out_ptr, out_cap) -> i32`, the same shape as `read_dir` and
            // the same answers. What it will not do is read *any* path: the grant is the one file
            // this call is about, resolved so that `..`, a relative spelling and a symlink all
            // land on it, and anything else is `i32::MIN`.
            let want = scoped.clone();
            let f = move |mut caller: wasmtime::Caller<'_, StoreLimits>,
                          a: u32,
                          b: u32,
                          c: u32,
                          d: u32|
                  -> i32 {
                let Some(path) = guest_path(&mut caller, a, b) else {
                    return i32::MIN;
                };
                let Some(want) = want.as_deref() else {
                    return i32::MIN;
                };
                let Ok(asked) = std::fs::canonicalize(&path) else {
                    return i32::MIN;
                };
                if asked != want {
                    return i32::MIN;
                }
                let Ok(bytes) = std::fs::read(&asked) else {
                    return i32::MIN;
                };
                // Over the cap is a refusal, not a clip: half a file is not the file.
                if bytes.len() > MAX_HOOK_BYTES {
                    return i32::MIN;
                }
                write_guest(&mut caller, c, d, &bytes)
            };
            let _ = linker.func_wrap("rano", "read_file", f);
        }
        if caps.convert_page {
            // `(page, dpi, out_ptr, out_cap) -> i32`: one page of the file this call is about,
            // as a PNG. `pdf::page_png` owns the deadline and the caps, and the path is scoped
            // exactly as `read_file`'s is, so the grant reaches this document and no other.
            let want = scoped.clone();
            let f = move |mut caller: wasmtime::Caller<'_, StoreLimits>,
                          page: i32,
                          dpi: i32,
                          out_ptr: u32,
                          out_cap: u32|
                  -> i32 {
                let Some(want) = want.as_deref() else {
                    return i32::MIN;
                };
                if page < 0 || dpi < 0 {
                    return i32::MIN;
                }
                let Ok(png) = crate::pdf::page_png(want, page as u32, dpi as u32) else {
                    return i32::MIN;
                };
                if png.len() > MAX_HOOK_BYTES {
                    return i32::MIN;
                }
                write_guest(&mut caller, out_ptr, out_cap, &png)
            };
            let _ = linker.func_wrap("rano", "convert_page", f);

            // `(page, out_ptr, out_cap) -> i32`: the page's text, for a terminal that cannot
            // draw — the answer a reader falls back to, not a lesser one.
            let want = scoped.clone();
            let f = move |mut caller: wasmtime::Caller<'_, StoreLimits>,
                          page: i32,
                          out_ptr: u32,
                          out_cap: u32|
                  -> i32 {
                let Some(want) = want.as_deref() else {
                    return i32::MIN;
                };
                if page < 0 {
                    return i32::MIN;
                }
                let Ok(text) = crate::pdf::page_text(want, page as u32) else {
                    return i32::MIN;
                };
                if text.len() > MAX_HOOK_BYTES {
                    return i32::MIN;
                }
                write_guest(&mut caller, out_ptr, out_cap, text.as_bytes())
            };
            let _ = linker.func_wrap("rano", "convert_page_text", f);
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
        let Some(bytes) = self.call_bytes(hook, text, viewport)? else {
            return Ok(None);
        };
        let out = std::str::from_utf8(&bytes)
            .map_err(|_| format!("`{hook}` answered bytes that are not text"))?;
        Ok(Some(out.to_string()))
    }

    /// **One hook call, in bytes**: the same engine work for a text hook and for a picture, which
    /// differ only in what the host makes of the answer.
    fn call_bytes(
        &self,
        hook: &str,
        text: &str,
        viewport: Option<(i32, i32)>,
    ) -> Result<Option<Vec<u8>>, String> {
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
        // **What `read_file` may answer for**: a call with a viewport is a provider, and a
        // provider's input is a path — so that path is the one file this call is about. A text
        // hook is asked about a text, so it has no file to scope to and the import refuses.
        let scope = viewport.map(|_| PathBuf::from(text));
        let mut store = Self::store(&self.engine);
        let instance = Self::linker(&self.engine, self.caps, scope)
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
        if out_len > MAX_HOOK_BYTES {
            return Err(format!(
                "`{hook}` answered {} MiB, above the {} MiB cap",
                out_len >> 20,
                MAX_HOOK_BYTES >> 20
            ));
        }
        Ok(Some(data[out_ptr..end].to_vec()))
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
            Some(format!("imports are not granted to this plugin ({text})"))
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

/// The path a capability import was handed, read out of the guest's own memory: a range inside
/// `memory` that is text, or nothing.
fn guest_path(
    caller: &mut wasmtime::Caller<'_, StoreLimits>,
    ptr: u32,
    len: u32,
) -> Option<PathBuf> {
    let Some(wasmtime::Extern::Memory(mem)) = caller.get_export("memory") else {
        return None;
    };
    let data = mem.data(&*caller);
    let lo = usize::try_from(ptr).ok()?;
    let len = usize::try_from(len).ok()?;
    let hi = lo.checked_add(len).filter(|end| *end <= data.len())?;
    let text = std::str::from_utf8(&data[lo..hi]).ok()?;
    Some(PathBuf::from(text))
}

/// A capability import's answer: the bytes written, `-needed` when the guest's buffer is too
/// small for them, `i32::MIN` when there is no guest memory to write into at all.
fn write_guest(
    caller: &mut wasmtime::Caller<'_, StoreLimits>,
    out_ptr: u32,
    out_cap: u32,
    bytes: &[u8],
) -> i32 {
    let Some(wasmtime::Extern::Memory(mem)) = caller.get_export("memory") else {
        return i32::MIN;
    };
    let out = usize::try_from(out_ptr).unwrap_or(usize::MAX);
    let room = mem.data(&*caller).len();
    if out_cap as usize >= bytes.len() && out.saturating_add(bytes.len()) <= room {
        mem.data_mut(caller)[out..out + bytes.len()].copy_from_slice(bytes);
        return bytes.len() as i32;
    }
    -(bytes.len() as i32)
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
            Capabilities {
                read_dir: true,
                ..Default::default()
            },
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
            Capabilities {
                read_dir: true,
                ..Default::default()
            },
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

    // -- `read_file`, and what it will not read ----------------------------------------------

    /// A module with the `read_file` import and two answers, `ok` and `no`, so a test reads the
    /// host's decision rather than a plugin's opinion.
    const READ_FILE: &str = r#"
        (import "rano" "read_file" (func $read_file (param i32 i32 i32 i32) (result i32)))
        (memory (export "memory") 4)
        (func (export "rano_abi") (result i32) i32.const 1)
        (func (export "rano_alloc") (param i32) (result i32) i32.const 1024)
        (data (i32.const 6000) "ok")
        (data (i32.const 6010) "no")
    "#;

    fn read_file_caps() -> Capabilities {
        Capabilities {
            read_file: true,
            ..Default::default()
        }
    }

    /// A fresh directory for a test, named so two runs cannot collide.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rano_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A provider that reads the path it was handed — the one file the call is about — and answers
    /// those bytes, or `no` when the host refused.
    fn self_reader() -> Plugin {
        let src = format!(
            r#"{READ_FILE}
            (func (export "provider_open") (param $p i32) (param $l i32) (param i32) (param i32) (result i32)
              (local $got i32)
              (local.set $got
                (call $read_file (local.get $p) (local.get $l) (i32.const 7000) (i32.const 2048)))
              (if (i32.le_s (local.get $got) (i32.const 0))
                (then
                  (i32.store (i32.const 8000) (i32.const 6010))
                  (i32.store (i32.const 8004) (i32.const 2)))
                (else
                  (i32.store (i32.const 8000) (i32.const 7000))
                  (i32.store (i32.const 8004) (local.get $got))))
              (i32.const 8000))"#
        );
        Plugin::from_bytes_with(
            &wat(&src),
            PathBuf::from("read-file.wasm"),
            read_file_caps(),
        )
        .expect("loads with the grant")
    }

    /// A provider that reads one path compiled into it, whatever path the call was about.
    fn other_reader(path: &str) -> Plugin {
        let src = format!(
            r#"{READ_FILE}
            (data (i32.const 4096) "{path}")
            (func (export "provider_open") (param i32 i32 i32 i32) (result i32)
              (local $got i32)
              (local.set $got
                (call $read_file (i32.const 4096) (i32.const {len}) (i32.const 7000) (i32.const 2048)))
              (if (i32.le_s (local.get $got) (i32.const 0))
                (then
                  (i32.store (i32.const 8000) (i32.const 6010))
                  (i32.store (i32.const 8004) (i32.const 2)))
                (else
                  (i32.store (i32.const 8000) (i32.const 6000))
                  (i32.store (i32.const 8004) (i32.const 2))))
              (i32.const 8000))"#,
            len = path.len()
        );
        Plugin::from_bytes_with(
            &wat(&src),
            PathBuf::from("read-file.wasm"),
            read_file_caps(),
        )
        .expect("loads with the grant")
    }

    #[test]
    fn read_file_is_refused_without_the_grant() {
        let src = format!("(module {READ_FILE})");
        let e = Plugin::from_bytes(&wat(&src), PathBuf::from("read-file.wasm")).unwrap_err();
        assert!(
            e.contains("imports are not granted") && e.contains("read_file"),
            "{e}"
        );
    }

    /// **The grant is the file the call is about, not a filesystem door.** The same module, called
    /// the same way, is answered for the path it was handed and refused for every other one — a
    /// sibling, `/etc/shadow`, or a path a text hook would have liked to read.
    #[test]
    fn read_file_answers_only_for_the_file_the_call_was_about() {
        let dir = temp_dir("read_file_scope");
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, "A\n").unwrap();
        std::fs::write(&b, "B\n").unwrap();
        let a_text = a.to_string_lossy().to_string();

        // The file the call is about: answered, byte for byte.
        let p = self_reader();
        assert_eq!(
            p.provider_open(&a_text, 80, 24).unwrap().as_deref(),
            Some("A\n")
        );

        // A sibling in the same folder: refused, because it is not the file that was asked about.
        let p = other_reader(&b.to_string_lossy());
        assert_eq!(
            p.provider_open(&a_text, 80, 24).unwrap().as_deref(),
            Some("no"),
            "a granted read_file must not read a path the call is not about"
        );

        // A file outside the reader's world entirely: the same refusal.
        let p = other_reader("/etc/shadow");
        assert_eq!(
            p.provider_open(&a_text, 80, 24).unwrap().as_deref(),
            Some("no"),
            "read-file is not a filesystem capability"
        );

        // Another spelling of the same file resolves to it, and is answered.
        let dots = format!(
            "{}/../{}/a.txt",
            dir.display(),
            dir.file_name().unwrap().to_string_lossy()
        );
        let p = other_reader(&dots);
        assert_eq!(
            p.provider_open(&a_text, 80, 24).unwrap().as_deref(),
            Some("ok"),
            "`..` and `.` spell the same file"
        );

        // A text hook is asked about a text, not a path, so it has no file to scope to.
        let src = format!(
            r#"{READ_FILE}
            (data (i32.const 4096) "{a_text}")
            (func (export "on_open") (param i32 i32) (result i32)
              (local $got i32)
              (local.set $got
                (call $read_file (i32.const 4096) (i32.const {len}) (i32.const 7000) (i32.const 2048)))
              (if (i32.le_s (local.get $got) (i32.const 0))
                (then
                  (i32.store (i32.const 8000) (i32.const 6010))
                  (i32.store (i32.const 8004) (i32.const 2)))
                (else
                  (i32.store (i32.const 8000) (i32.const 6000))
                  (i32.store (i32.const 8004) (i32.const 2))))
              (i32.const 8000))"#,
            len = a_text.len()
        );
        let p = Plugin::from_bytes_with(
            &wat(&src),
            PathBuf::from("read-file.wasm"),
            read_file_caps(),
        )
        .expect("loads with the grant");
        assert_eq!(
            p.call("on_open", "some text").unwrap().as_deref(),
            Some("no"),
            "a text hook has no file to read"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file above [`MAX_HOOK_BYTES`] is refused rather than handed over in pieces: the guest
    /// asked for a buffer it cannot fill, and the host says no instead of clipping.
    #[test]
    fn read_file_will_not_answer_a_file_above_the_cap() {
        let dir = temp_dir("read_file_cap");
        let big = dir.join("big.bin");
        std::fs::write(&big, vec![b'x'; MAX_HOOK_BYTES + 1]).unwrap();
        let p = self_reader();
        assert_eq!(
            p.provider_open(&big.to_string_lossy(), 80, 24)
                .unwrap()
                .as_deref(),
            Some("no")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- `provider_picture` ------------------------------------------------------------------

    /// The fixture, from the crate root, so the test does not depend on the working directory.
    fn stl_fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/plugin-stl/fixtures")
            .join(name)
    }

    fn stl_plugin() -> Plugin {
        Plugin::from_bytes_with(
            include_bytes!("../examples/plugin-stl/plugin.wasm"),
            PathBuf::from("plugin-stl.wasm"),
            read_file_caps(),
        )
        .expect("the STL example loads with read-file")
    }

    /// **The Zig example, through both new capabilities**: refused without `read-file`, and with
    /// it a `.stl` path is a picture and, for a terminal with no graphics protocol, a character
    /// ramp. Anything else is "not mine".
    #[test]
    fn the_stl_example_needs_read_file_and_then_renders() {
        let wasm: &[u8] = include_bytes!("../examples/plugin-stl/plugin.wasm");
        let e = Plugin::from_bytes_with(wasm, PathBuf::from("stl.wasm"), Capabilities::default())
            .unwrap_err();
        assert!(
            e.contains("imports are not granted") && e.contains("read_file"),
            "{e}"
        );

        let p = stl_plugin();
        assert_eq!(
            p.hooks(),
            vec!["provider_open".to_string(), "provider_picture".to_string()]
        );

        // Not an STL: not mine, either way.
        let txt = stl_fixture("cube.stl").with_extension("txt");
        assert_eq!(
            p.provider_picture(&txt.to_string_lossy(), 80, 24).unwrap(),
            None
        );
        assert_eq!(
            p.provider_open(&txt.to_string_lossy(), 80, 24).unwrap(),
            None
        );

        // An STL: a well-formed header, two pixels per cell, and a cube actually drawn.
        let ascii = stl_fixture("cube.stl");
        let picture = p
            .provider_picture(&ascii.to_string_lossy(), 80, 24)
            .unwrap()
            .expect("a cube is claimed");
        assert_eq!(
            (picture.width, picture.height),
            (80, 48),
            "2 pixels per cell"
        );
        assert_eq!(picture.rgb.len(), 80 * 48 * 3);
        let corner = picture.pixel(0, 0).unwrap();
        let lit = (0..48u32)
            .flat_map(|y| (0..80u32).map(move |x| (x, y)))
            .filter(|&(x, y)| picture.pixel(x, y).unwrap() != corner)
            .count();
        assert!(lit > 100, "the cube should be drawn: {lit} pixels lit");
        eprintln!("provider_picture fuel: {}", p.last_fuel());

        // The binary flavour of the same cube: the same picture, so both readers agree.
        let binary = p
            .provider_picture(&stl_fixture("cube-binary.stl").to_string_lossy(), 80, 24)
            .unwrap()
            .expect("claimed");
        assert_eq!(binary, picture, "binary and ASCII parse to one cube");

        // A viewport no plugin could fill is refused by the plugin itself, not clipped.
        assert_eq!(
            p.provider_picture(&ascii.to_string_lossy(), 4000, 4000)
                .unwrap(),
            None,
            "a viewport above the plugin's own cap is not mine"
        );

        // The fallback: one character per cell, for a terminal with no graphics protocol.
        let text = p
            .provider_open(&ascii.to_string_lossy(), 40, 12)
            .unwrap()
            .expect("claimed");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 12, "{text}");
        assert!(
            lines.iter().all(|l| l.chars().count() == 40),
            "a line per row of 40 cells: {text}"
        );
        assert!(
            text.chars().any(|c| c != ' ' && c != '\n'),
            "the ramp should draw something: {text}"
        );
        eprintln!("provider_open fuel: {}", p.last_fuel());
    }

    /// A `provider_picture` that answers a header and nothing else, so the bounds can be tested
    /// without a renderer.
    fn picture_answer(width: u32, height: u32, out_len: u32) -> Plugin {
        plugin(&format!(
            r#"{BASE}
            (func (export "provider_picture") (param i32 i32 i32 i32) (result i32)
              (i32.store (i32.const 4096) (i32.const {width}))
              (i32.store (i32.const 4100) (i32.const {height}))
              (i32.store (i32.const 8192) (i32.const 4096))
              (i32.store (i32.const 8196) (i32.const {out_len}))
              i32.const 8192)"#
        ))
    }

    /// **Refused, never clipped**: 4096×4096 is 48 MiB of RGB, and the header alone says so.
    #[test]
    fn an_oversized_picture_is_refused_rather_than_clipped() {
        let p = picture_answer(4096, 4096, 12);
        let e = p.provider_picture("/tmp/model.stl", 80, 24).unwrap_err();
        assert!(e.contains("cap") && e.contains("4096x4096"), "{e}");
    }

    /// A header is a claim about the bytes that follow it, and a claim that does not hold is an
    /// error rather than a picture the host has to guess the shape of.
    #[test]
    fn a_picture_header_that_disagrees_with_its_bytes_is_refused() {
        let p = picture_answer(2, 2, 12);
        let e = p.provider_picture("/tmp/model.stl", 80, 24).unwrap_err();
        assert!(e.contains("wanted 24"), "{e}");
        // The same header with its bytes is a picture.
        let p = picture_answer(2, 2, 24);
        let pic = p
            .provider_picture("/tmp/model.stl", 80, 24)
            .unwrap()
            .expect("claimed");
        assert_eq!((pic.width, pic.height, pic.rgb.len()), (2, 2, 12));
    }

    #[test]
    fn a_picture_shorter_than_its_header_is_refused() {
        let p = picture_answer(2, 2, 8);
        let e = p.provider_picture("/tmp/model.stl", 80, 24).unwrap_err();
        assert!(e.contains("too short"), "{e}");
    }

    #[test]
    fn a_picture_with_no_pixels_is_refused() {
        let p = picture_answer(0, 4, 12);
        let e = p.provider_picture("/tmp/model.stl", 80, 24).unwrap_err();
        assert!(e.contains("0x4"), "{e}");
    }

    /// A plugin that answers no picture claims nothing — the hook is found by name like any other.
    #[test]
    fn a_plugin_with_no_picture_hook_claims_nothing() {
        let p = Plugin::from_bytes(
            include_bytes!("../examples/plugin-upper/plugin.wasm"),
            PathBuf::from("upper.wasm"),
        )
        .expect("loads");
        assert_eq!(p.provider_picture("/tmp/model.stl", 80, 24).unwrap(), None);
    }

    /// **The fuel budget is the call's, whatever hook it is**: a picture that never finishes ends
    /// in `OutOfFuel` rather than in a hung editor, and the whole budget is what it burnt.
    #[test]
    fn a_picture_that_never_finishes_runs_out_of_fuel() {
        let p = plugin(&format!(
            r#"{BASE}
            (func (export "provider_picture") (param i32 i32 i32 i32) (result i32)
              (loop $l (br $l)) (i32.const -1))"#
        ));
        let t = std::time::Instant::now();
        let e = p.provider_picture("/tmp/model.stl", 80, 24).unwrap_err();
        assert!(e.contains("fuel") || e.contains("trapped"), "{e}");
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "bounded");
        assert_eq!(p.last_fuel(), HOOK_FUEL, "the whole budget was burnt");
    }

    /// **The memory cap is the call's too.** The guest tries to grow 125 MiB and answers a 1×1
    /// picture when the limiter says no — which it does, so the picture comes back rather than a
    /// grown memory.
    #[test]
    fn a_picture_hook_cannot_grow_past_the_memory_cap() {
        let p = plugin(&format!(
            r#"{BASE}
            (func (export "provider_picture") (param i32 i32 i32 i32) (result i32)
              (if (i32.eq (memory.grow (i32.const 2000)) (i32.const -1))
                (then
                  (i32.store (i32.const 4096) (i32.const 1))
                  (i32.store (i32.const 4100) (i32.const 1))
                  (i32.store (i32.const 8192) (i32.const 4096))
                  (i32.store (i32.const 8196) (i32.const 15)))
                (else))
              (i32.const 8192))"#
        ));
        let pic = p
            .provider_picture("/tmp/model.stl", 80, 24)
            .unwrap()
            .expect("the limiter refused the growth, so the 1x1 picture is the answer");
        assert_eq!((pic.width, pic.height), (1, 1));
    }
}

#[cfg(test)]
mod convert_page_tests {
    use super::*;

    /// A module that merely IMPORTS the hook: the grant is decided at instantiation, so an
    /// import with no call is exactly the shape that tells a refusal from an admission.
    fn importer(import: &str, params: &str) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(module
                 (import "rano" "{import}" (func (param {params}) (result i32)))
                 (memory (export "memory") 1)
                 (func (export "rano_abi") (result i32) i32.const 1)
                 (func (export "rano_alloc") (param i32) (result i32) i32.const 1024)
               )"#
        ))
        .expect("a module")
        .to_vec()
    }

    /// **A grant is a capability, not a default**: a module importing `convert_page` is refused
    /// at instantiation without it, which is the same rule `read_dir` and `read_file` follow.
    #[test]
    fn the_grant_is_required_to_import_it() {
        let wasm = importer("convert_page", "i32 i32 i32 i32");
        assert!(
            Plugin::from_bytes_hooks(
                &wasm,
                PathBuf::from("t.wasm"),
                Capabilities::default(),
                &["provider_open"],
            )
            .is_err(),
            "an ungranted import must be refused"
        );
        assert!(
            Plugin::from_bytes_hooks(
                &wasm,
                PathBuf::from("t.wasm"),
                Capabilities {
                    convert_page: true,
                    ..Capabilities::default()
                },
                &["provider_open"],
            )
            .is_ok(),
            "the grant is what admits it"
        );
    }

    /// The name in the config, and its spellings.
    #[test]
    fn the_capability_is_named_convert_page() {
        for name in ["convert-page", "CONVERT-PAGE"] {
            let caps = Capabilities::from_names(&[name.to_string()]);
            assert!(caps.convert_page, "{name}");
            assert!(!caps.read_file, "{name} is not read-file");
        }
    }
}
