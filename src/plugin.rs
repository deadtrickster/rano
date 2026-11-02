//! Plugins: WASM modules that answer hooks — text in, text out. The host
//! discovers what a module answers by enumerating its exports (`on_*` is a
//! text hook, `provider_*` takes the viewport too, a host names other prefixes
//! at load), so a new hook is a host-side function, not an ABI change. A hook
//! answers the offset of an 8-byte result block — `out_ptr`/`out_len` as LE
//! `u32`s — or `0xFFFFFFFF` for unchanged. Policy is the editor's
//! (`editor.rs`/`buffer.rs`); a module imports exactly what the config
//! granted, and gets [`HOOK_FUEL`], [`HOOK_MEMORY`] and [`MAX_HOOK_BYTES`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

/// Fuel per call: an infinite loop ends in `OutOfFuel`, not a hung editor.
pub const HOOK_FUEL: u64 = 50_000_000;

/// Guest memory per call, refused by the store's limiter.
pub const HOOK_MEMORY: usize = 64 << 20;

/// The largest text handed to a hook, and the largest it may hand back.
pub const MAX_HOOK_BYTES: usize = 16 << 20;

/// The ABI this host speaks. A module that says anything else is refused rather than guessed at.
pub const ABI: i32 = 1;

/// The sentinel a hook returns to mean "leave the text as it is".
const UNCHANGED: i32 = -1;

/// What the host lets a plugin reach beyond its own text. Nothing is granted
/// by default; the config says `capabilities = ["read-dir"]`, and a module
/// importing anything else is refused at instantiation, by name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// `rano.read_dir`: a folder's listing, rendered by the host with the same
    /// rules as the built-in dired.
    pub read_dir: bool,
    /// `rano.read_file`: the bytes of the one file this call is about. Not a
    /// filesystem door: the host answers only for that path, resolved first;
    /// anything else — `/etc/shadow` included — is refused, and a text hook
    /// has no file to scope to at all.
    pub read_file: bool,
    /// `rano.convert_page`/`rano.convert_page_text`: one page of the file this
    /// call is about, under the host's converters, deadline, caps and scoping.
    pub convert_page: bool,
}

impl Capabilities {
    /// From the config's `capabilities = ["read-dir"]` list: unknown names are
    /// ignored (a misspelt capability is not a grant), and the empty list
    /// grants nothing.
    pub fn from_names(names: &[String]) -> Capabilities {
        let has = |name: &str| names.iter().any(|n| n.trim().eq_ignore_ascii_case(name));
        Capabilities {
            read_dir: has("read-dir"),
            read_file: has("read-file"),
            convert_page: has("convert-page"),
        }
    }
}

/// A picture a plugin answered: `width`×`height` RGB, row-major. The ABI
/// payload is `[width u32 LE][height u32 LE][rgb]`.
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
    /// The export names that are hooks.
    hooks: Vec<String>,
    /// What the config let it import.
    caps: Capabilities,
    /// Fuel the last call burned; an atomic because `call` takes `&self`.
    fuel: AtomicU64,
}

impl Plugin {
    /// Load a `.wasm`: compiled and checked here, so a module that would fail
    /// on every save fails at load with one sentence instead.
    pub fn load(path: &Path) -> Result<Plugin, String> {
        Self::load_with(path, Capabilities::default())
    }

    /// [`Self::load`] with granted capabilities; a module importing anything
    /// else is refused here rather than at the first save.
    pub fn load_with(path: &Path, caps: Capabilities) -> Result<Plugin, String> {
        Self::load_hooks(path, caps, &[])
    }

    /// [`Self::load`] with granted capabilities and the host's hook prefixes.
    pub fn load_hooks(
        path: &Path,
        caps: Capabilities,
        prefixes: &[&str],
    ) -> Result<Plugin, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::from_bytes_hooks(&bytes, path.to_path_buf(), caps, prefixes)
    }

    /// The same, from bytes — the seam the WAT fixtures come through.
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

    /// [`Self::from_bytes`] with capabilities and the **hook prefixes** the
    /// host recognises: `on_`/`provider_` always, anything under `prefixes` a
    /// text hook — `(i32, i32) -> i32`, refused here if not. The list is the
    /// host's, not the module's; an empty prefix is ignored.
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
        // The ABI is checked by calling it: a module answering 2 must not run
        // as v1.
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
        // Each hook's SIGNATURE is checked here, so a hook the host could
        // never call is refused at load rather than at the first save.
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

    /// `on_open`/`on_save`: the text to use, or `None` when the hook left it alone.
    pub fn open(&self, text: &str) -> Result<Option<String>, String> {
        self.call("on_open", text)
    }

    /// `on_save`: the text to write, or `None` when the hook left it alone.
    pub fn save(&self, text: &str) -> Result<Option<String>, String> {
        self.call("on_save", text)
    }

    /// Fuel the last call burned — `HOOK_FUEL` minus what the store had left,
    /// so an out-of-fuel hook reads as the whole budget.
    pub fn last_fuel(&self) -> u64 {
        self.fuel.load(Ordering::Relaxed)
    }

    /// `provider_open`: what this path should open as, or `None` when the
    /// plugin does not claim it. Told the viewport because a provider's
    /// answer is laid out at the width it will be drawn at.
    pub fn provider_open(
        &self,
        path: &str,
        cols: u32,
        rows: u32,
    ) -> Result<Option<String>, String> {
        self.call_sized("provider_open", path, cols as i32, rows as i32)
    }

    /// `provider_picture`: the picture this path should be drawn as, or `None`
    /// when not claimed; `[width u32 LE][height u32 LE][rgb]`, bounds the
    /// host's — `width * height * 3` within [`MAX_HOOK_BYTES`], the header
    /// describing exactly the bytes that came with it. Refused, never clipped.
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

    /// The payload checked: header, size inside the cap, bytes as promised.
    fn decode_picture(bytes: &[u8]) -> Result<Option<Picture>, String> {
        if bytes.len() < 8 {
            return Err(format!(
                "`provider_picture` answered {} bytes, too short for the 8-byte header",
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
        if bytes.len() != 8 + want {
            return Err(format!(
                "`provider_picture` answered {} bytes for a {width}x{height} picture (wanted {})",
                bytes.len(),
                8 + want
            ));
        }
        Ok(Some(Picture {
            width,
            height,
            rgb: bytes[8..].to_vec(),
        }))
    }

    /// The linker a module is instantiated with: one import per granted
    /// capability, and nothing else. `scope` is the path this call is about,
    /// resolved once here, and it is what [`Capabilities::read_file`] is scoped
    /// to.
    fn linker(engine: &Engine, caps: Capabilities, scope: Option<PathBuf>) -> Linker<StoreLimits> {
        // The one file this call is about, resolved once, with the part before
        // a `#` taken (a provider keeps state in a fragment:
        // `paper.pdf#p=4` is about `paper.pdf`).
        let scoped = scope.and_then(|p| {
            let text = p.to_string_lossy().into_owned();
            let file = text.split('#').next().unwrap_or(&text);
            std::fs::canonicalize(file).ok()
        });
        let mut linker = Linker::new(engine);
        if caps.read_dir {
            // `(path_ptr, path_len, out_ptr, out_cap) -> i32`: bytes written,
            // `-needed`, or `i32::MIN` for an error.
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
            // The same shape as `read_dir`, but scoped: it reads only the one
            // file this call is about, resolved; anything else is `i32::MIN`.
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
            // One page of the file this call is about, counted from zero as
            // `pdf::page_rgb` documents. pdf owns the deadline and caps; the
            // path is scoped as `read_file`'s is.
            let want = scoped.clone();
            let f = move |mut caller: wasmtime::Caller<'_, StoreLimits>,
                          page: i32,
                          dpi: i32,
                          out_ptr: u32,
                          out_cap: u32|
                  -> i32 {
                let Some(want) = want.as_deref() else {
                    if std::env::var_os("RANO_PLUGIN_DEBUG").is_some() {
                        eprintln!("rano: convert_page: no file in scope");
                    }
                    return i32::MIN;
                };
                if page < 0 || dpi < 0 {
                    return i32::MIN;
                }
                let (w, h, rgb) = match crate::pdf::page_rgb(want, page as u32, dpi as u32) {
                    Ok(v) => v,
                    Err(e) => {
                        // `i32::MIN` is one answer for several reasons, and a plugin cannot say
                        // which. `RANO_PLUGIN_DEBUG` is where a person finds out.
                        if std::env::var_os("RANO_PLUGIN_DEBUG").is_some() {
                            eprintln!("rano: convert_page({page}, {dpi}) refused: {e}");
                        }
                        return i32::MIN;
                    }
                };
                // **The same block `provider_picture` answers with** — one
                // picture format: `[width][height][rgb]`, no decoder.
                let mut payload = Vec::with_capacity(8 + rgb.len());
                payload.extend_from_slice(&w.to_le_bytes());
                payload.extend_from_slice(&h.to_le_bytes());
                payload.extend_from_slice(&rgb);
                if payload.len() > MAX_HOOK_BYTES {
                    if std::env::var_os("RANO_PLUGIN_DEBUG").is_some() {
                        eprintln!(
                            "rano: convert_page({page}, {dpi}) is {} MiB, above the {} MiB cap",
                            payload.len() >> 20,
                            MAX_HOOK_BYTES >> 20
                        );
                    }
                    return i32::MIN;
                }
                let wrote = write_guest(&mut caller, out_ptr, out_cap, &payload);
                if std::env::var_os("RANO_PLUGIN_DEBUG").is_some() {
                    eprintln!(
                        "rano: convert_page({page}, {dpi}) -> {w}x{h}, {} bytes, wrote {wrote}, cap {out_cap}",
                        payload.len()
                    );
                }
                if wrote < 0 && std::env::var_os("RANO_PLUGIN_DEBUG").is_some() {
                    eprintln!(
                        "rano: convert_page({page}) needs {} bytes, the plugin offered {out_cap}",
                        payload.len()
                    );
                }
                wrote
            };
            let _ = linker.func_wrap("rano", "convert_page", f);

            // The page's text, for a terminal that cannot draw.
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

    /// Run any text hook: `Ok(None)` when the plugin does not answer it or
    /// left the text alone. One fresh store per call, so nothing a guest
    /// allocates can accumulate across saves.
    pub fn call(&self, hook: &str, text: &str) -> Result<Option<String>, String> {
        self.call_with(hook, text, None)
    }

    /// [`Self::call`] for a hook that takes the viewport too — a provider.
    fn call_sized(
        &self,
        hook: &str,
        text: &str,
        cols: i32,
        rows: i32,
    ) -> Result<Option<String>, String> {
        self.call_with(hook, text, Some((cols, rows)))
    }

    /// One hook call: two `i32`s for a text hook, four for a provider; the
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

    /// One hook call, in bytes: a text hook and a picture differ only in what
    /// the host makes of the answer.
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
        // `read_file`'s scope: a provider's input is a path, so that path is
        // the one file this call is about; a text hook has none.
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
        // The input goes where the guest asked; an offset outside its own
        // memory is a broken hook, not a transformed file.
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
        // Recorded before the trap becomes a sentence: the gate wants the
        // number either way.
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

    /// A store with the budget this host allows: fuel, and a memory cap the
    /// guest's own `memory.grow` cannot pass.
    fn store(engine: &Engine) -> Store<StoreLimits> {
        let limits = StoreLimitsBuilder::new().memory_size(HOOK_MEMORY).build();
        let mut store = Store::new(engine, limits);
        store.limiter(|s| s);
        // A store without fuel would be unbounded; `consume_fuel(true)` at the engine makes this
        // the one place the budget is set.
        let _ = store.set_fuel(HOOK_FUEL);
        store
    }

    /// Why a module could not be instantiated when the reason is the sandbox
    /// rather than a mistake.
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

/// The path an import was handed, read out of the guest's own memory.
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

/// An import's answer: bytes written, `-needed` for a small buffer,
/// `i32::MIN` for no memory to write into.
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

/// A WAT fixture (tests only: no WAT parser rides in the binary).
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

    /// The shape every fixture starts from: memory, ABI, a fixed arena at 1024.
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
        // The block points at the input's own bytes, so the save returns its
        // text by the block path rather than the sentinel.
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
    fn the_dired_example_needs_read_dir_and_then_lists_a_folder() {
        let wasm: &[u8] = include_bytes!("../examples/plugin-dired/plugin.wasm");
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
        // A file is not claimed: `provider_open` answers the sentinel.
        assert_eq!(
            p.provider_open(&dir.join("a.txt").to_string_lossy(), 80, 24)
                .unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hook_call_is_cheap_once_the_module_is_compiled() {
        let p = Plugin::from_bytes_with(
            include_bytes!("../examples/plugin-upper/plugin.wasm"),
            PathBuf::from("upper.wasm"),
            Capabilities::default(),
        )
        .expect("loads");
        // Warm: one call to register, then a run of them.
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
        Plugin::from_bytes_hooks(
            &wat(src),
            PathBuf::from("pane.wasm"),
            Capabilities::default(),
            &[],
        )
        .expect("loads when nothing claims pane_");
    }

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

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rano_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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

        // A sibling in the same folder: refused.
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

        // Not an STL: not mine.
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

    #[test]
    fn an_oversized_picture_is_refused_rather_than_clipped() {
        let p = picture_answer(4096, 4096, 12);
        let e = p.provider_picture("/tmp/model.stl", 80, 24).unwrap_err();
        assert!(e.contains("cap") && e.contains("4096x4096"), "{e}");
    }

    #[test]
    fn a_picture_header_that_disagrees_with_its_bytes_is_refused() {
        let p = picture_answer(2, 2, 12);
        let e = p.provider_picture("/tmp/model.stl", 80, 24).unwrap_err();
        assert!(e.contains("wanted 20"), "{e}");
        // The same header with its bytes is a picture; an eight-byte header and 2x2 RGB.
        let p = picture_answer(2, 2, 20);
        let pic = p
            .provider_picture("/tmp/model.stl", 80, 24)
            .unwrap()
            .expect("claimed");
        assert_eq!((pic.width, pic.height, pic.rgb.len()), (2, 2, 12));
    }

    #[test]
    fn a_picture_shorter_than_its_header_is_refused() {
        let p = picture_answer(2, 2, 4);
        let e = p.provider_picture("/tmp/model.stl", 80, 24).unwrap_err();
        assert!(e.contains("too short"), "{e}");
    }

    #[test]
    fn a_picture_with_no_pixels_is_refused() {
        let p = picture_answer(0, 4, 12);
        let e = p.provider_picture("/tmp/model.stl", 80, 24).unwrap_err();
        assert!(e.contains("0x4"), "{e}");
    }

    #[test]
    fn the_stl_examples_rgb_buffer_is_three_bytes_a_pixel() {
        let src = include_str!("../examples/plugin-stl/src/main.zig");
        assert!(
            src.contains("rgb: [3 * max_pixels]u8"),
            "the picture buffer must be three bytes a pixel; `max_pixels` bytes is a third of \
             the picture and the rest is written past it"
        );
    }

    #[test]
    fn a_plugin_with_no_picture_hook_claims_nothing() {
        let p = Plugin::from_bytes(
            include_bytes!("../examples/plugin-upper/plugin.wasm"),
            PathBuf::from("upper.wasm"),
        )
        .expect("loads");
        assert_eq!(p.provider_picture("/tmp/model.stl", 80, 24).unwrap(), None);
    }

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
                  (i32.store (i32.const 8196) (i32.const 11)))
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

    #[test]
    fn the_capability_is_named_convert_page() {
        for name in ["convert-page", "CONVERT-PAGE"] {
            let caps = Capabilities::from_names(&[name.to_string()]);
            assert!(caps.convert_page, "{name}");
            assert!(!caps.read_file, "{name} is not read-file");
        }
    }
}
