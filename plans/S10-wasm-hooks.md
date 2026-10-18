# S10 — WASM plugins: file-open and file-save hooks

Grounded to code as of 2026-10-18: `rano` 0.9.0 @ `669ab57`, `Cargo.toml` (no `[features]` today,
one dependency family per job, no C but `libc` and the tree-sitter grammars), `src/editor.rs`
(`write_file` :3085, `format_for_save` :3040), `src/buffer.rs` (`from_file` :97, `from_text` :116),
`src/load_ctrl.rs` (`start_load` :25, `load_poll` :239 and its `Adopted::Finished` arm :351,
`load_now` :513), `src/config.rs` (11 keys in `parse_config`, plus the `color.*` prefix, :203–259),
`src/lib.rs` (module list :83–184).

Runtime facts verified, not recalled: **wasmi 0.40.0**, source read in the local registry
(`src/store.rs`: `set_fuel` :292, `get_fuel` :303, `limiter` :913; `src/limits.rs`:
`ResourceLimiter::memory_growing` :61). Cost measured in a probe built with rano's release
profile (`strip`, `lto`, `codegen-units=1`, plus `panic=abort` — which rano does **not** set, so
the real delta is if anything a little larger): **302,448 B → 1,229,920 B, a +927,472 B
(+0.88 MiB) delta, 14 crates in the tree, module compile 903 µs** for a two-function module.
For contrast, `wasmtime` 40 trimmed to `std` + `runtime` resolves **105 crates**.

---

## 1. What this is, and what it is not

**The shape the operator asked for:** *"lets create a plan first. something simple - say we start
with file open/file save hooks."* So: one `.wasm` file, two optional exports, text in and text
out. Nothing else.

**In scope**
- `on_open(path-independent text)` — a file's text may be transformed as it is opened.
- `on_save(text)` — a buffer's text may be transformed as it is written.
- A plugin that exports one hook and not the other answers only the one it exports.
- A host that *cannot* be reached from the plugin: **the guest imports nothing at all**.

**Out of scope (deliberately, and named so a reviewer can hold the line)**
- Host functions of any kind — no filesystem, no network, no process, no editor API. The first
  one to arrive should need its own decision, because it is the moment the sandbox stops being
  "a pure function".
- More than one plugin, ordering between plugins, per-path matching.
- Hot reload (the module is compiled once per process), and state that survives a call.
- Key bindings, status-line segments, commands, non-text hooks (`on_key`, `on_buffer_change`).
- A JS/TS/`Component Model` API: the ABI is **core wasm module, C-like exports**, so a plugin can
  be written in Rust, C, Go (TinyGo), Zig or AssemblyScript.

## 2. Anchors (where the hook goes, and why there)

| seam | today | the hook's place |
|---|---|---|
| sync read | `Buffer::from_file` :97 — `fs::read` → `encoding::detect`/`decode` → `is_binary` → `from_text` | **after** decode and **after** the binary check, **before** `from_text`. A binary is read-only (`dbf2845`) and a plugin never sees one. |
| save | `write_file` :3085 — `format_for_save()` → `file_bytes()` → `fs::write` | **after** `format_for_save` (the language's rule precedes the reader's — gofmt on a `.go` buffer runs first, so the hook sees formatted text), **before** `file_bytes`. |
| streaming open | `start_load` :25 → `LoadJob` → `load_poll` adopts rows in batches; `Adopted::Finished` :351 | **the loader cannot hook**: a text transform needs the whole text, and the rows are already on the glass. Decision in §4.3. |
| tails, log mode | `start_load_from` :42, `log_mode_for` | **no hook.** A tail is not an open: it seeks into a file being written and shows what arrives. |
| `^R` insert-file | `do_read` :3179 → `Buffer::from_file` | hooked: it is a file being read. |

All four sync call sites (`open_file` :3220, `load_now` :513, `do_read` :3179, and the startup
path's sync fallback) go through **one** wrapper (§6), so there is one place where "a file's text
enters rano" is true.

## 3. The ABI (v1)

A core wasm module. The guest **must** export:

```wat
(memory (export "memory") 1)                       ;; its linear memory
(func (export "rano_abi") (result i32) i32.const 1) ;; the ABI version; anything else is refused
(func (export "rano_alloc") (param i32) (result i32)) ;; reserve n bytes, return the offset
```

and **may** export either or both of:

```wat
(func (export "on_open") (param i32 i32) (result i32)) ;; (in_ptr, in_len) -> out_ptr
(func (export "on_save") (param i32 i32) (result i32)) ;; (in_ptr, in_len) -> out_ptr
```

The host writes the UTF-8 text at `in_ptr` (from `rano_alloc`), calls the hook, and reads a
**result block** at `out_ptr` — two little-endian `u32`s, `{ out_ptr, out_len }` — into the
guest's memory. `out_ptr == 0xFFFFFFFF` (or `out_len == 0`) means **unchanged**, and the text
passes through untouched. A hook returning an offset outside its memory, a block that overruns
it, or bytes that are not UTF-8 is a broken hook, not a transformed file (§4.5).

Why this shape rather than a packed `i64` or a returned `Vec`: a two-`u32` block at an offset is
the one thing every language can write without agreeing on struct layout, and the version
export is what lets a v2 ABI arrive without guessing.

**A fresh `Store` and instance per call.** The module is compiled once per process (903 µs
measured, once); each hook call gets a new store, 50 M fuel and a 64 MiB memory cap. That makes a
leak impossible to accumulate, makes fuel and limits *per call* rather than a running total, and
costs microseconds for a two-function module. Guest state across calls is therefore explicitly
not a v1 feature (it is the first thing a v2 would want, and it should be asked for).

## 4. Design

### 4.1 The runtime: wasmi, and the number that decides it

| | wasmi 0.40 | wasmtime 40 (`std`+`runtime`) |
|---|---|---|
| engine | interpreter, no JIT, no unsafe codegen | Cranelift |
| crates resolved | **14** | **105** |
| binary delta (measured) | **+0.88 MiB** | not built — the crate count is already the answer |
| per-call cost for a text hook | a few µs of interpretation | a few ns of JIT'd code |

A hook runs once per open and once per save — not per keystroke, not per frame — so the thing
that matters is what it costs when nobody uses it, and that is the binary and the lockfile. The
dependency list is entirely pure Rust (`arrayvec`, `multi-stash`, `smallvec`, `spin`,
`wasmi_collections`, `wasmi_core`, `wasmi_ir`, `wasmparser`, `bitflags`, `downcast-rs`, `libm`).

**The ruling is the operator's, and the two options are:**
1. **Default-on, no feature gate** (recommended): +0.88 MiB and +14 crates for a feature that is
   simply there. A gate that the release build must remember to set is a footgun, and this is
   not the resvg case — no C, no rasterizer, a third of that cost.
2. **`--features wasm`** (off by default): the library side of rano (letibot's pane) does not pay
   for a feature it may never use; the standalone binary is built with the feature on. This is
   one line in `Cargo.toml` plus one in CI, and it can be added at any point — including after
   step 1, which is why step 1 lands the dependency alone.

### 4.2 The sandbox

- **No imports.** The module is instantiated with an empty import list, so a module that imports
  anything fails to instantiate (`Linker`/`Instance::new` error) and the hook is skipped by name.
  This is the whole capability story of v1, and it is worth stating as a rule: the plugin is a
  pure function over bytes.
- **Fuel**: `Config::consume_fuel(true)`, `Store::set_fuel(HOOK_FUEL)` per call (`50_000_000` —
  measured: a trivial call burns 4). An infinite loop ends in `OutOfFuel`, not in a hung editor.
  Fuel is deterministic, so the bound is "work", not wall clock — and with no imports there is
  nothing a guest can block on.
- **Memory**: `Store::limiter(ResourceLimiter)` refusing a memory growing past
  `HOOK_MEMORY = 64 MiB` (and any table beyond a small bound). A guest that tries allocates
  until it is told no; the call fails and is reported.
- **Size**: the input text is capped at `MAX_HOOK_BYTES = 16 MiB` (a hook copies the text through
  guest memory twice; a larger buffer is opened/saved unchanged, with one line saying so), and
  the *output* the same — a hook cannot inflate a 1-line file into a gigabyte.

### 4.3 The open hook and the streaming loader

`start_load` exists so a 2 GiB file opens in a frame; a text-transforming hook cannot know the
text until the file is whole. So:

- With a plugin configured and the file **≤ `MAX_HOOK_BYTES`**, `start_load` reads it
  synchronously through the hook (`read_file`), and the rows that land are the hooked text. This
  covers the common case (the file on the command line, an F8 open) at the price of the size cap.
- Above the cap, or for a tail or log mode, the hook is **skipped** and one status line says why
  (`plugin: not run on example.log (above 16 MiB)` — a sentence, not silence, because a reader
  who configured a plugin will otherwise conclude it is broken).
- The loader's `Adopted::Finished` arm is deliberately **not** where the hook runs: a transform
  applied after the rows are on the glass is a buffer that visibly snaps, and a load is not an
  undo step, so there would be no way back.

### 4.4 The save hook

In `write_file`, after `format_for_save()`, the hook gets the text that is about to be written.

- If it returns **unchanged**, nothing happens: one extra call, nothing else.
- If it returns **different text**, the **buffer is replaced** with it as one undo step
  (`ActionKind::Plugin`, so undo says what it was and `M-U` takes it back), and *then* the text is
  encoded and written. This is what gofmt-on-save already does (`ActionKind::Format`), and the
  reason is the same: what is on the glass and what is on disk must be the same text. A plugin
  whose output is written but not shown would be silently re-applied on every save against a
  buffer the reader cannot reconcile.
- **Hooks must therefore be deterministic.** A hook that stamps a timestamp fights its own next
  save. The README says so; a test pins that a second save of unchanged text writes the same
  bytes.

### 4.5 Failure semantics: never lose a save, never stop an open

| what went wrong | what happens |
|---|---|
| no `rano_abi`, or not `1` | skipped; `plugin: ra no ABI (wanted 1) — opened unchanged` |
| the module imports anything | skipped; `plugin: imports are not allowed — opened unchanged` |
| trap / `OutOfFuel` | skipped; `plugin: out of fuel — saved unchanged` |
| memory cap hit | skipped; `plugin: wanted more than 64 MiB — saved unchanged` |
| output not UTF-8, or > cap | skipped; `plugin: output is not text — …` |
| the module file is missing or not wasm | the plugin is **off for the session**, said once at load, and the editor is as if it were not configured |
| any of the above on **save** | the file is written **as the buffer had it** — the hook is an improvement, never a precondition |

Every one of these is one status line naming the plugin and the reason. Nothing is written to the
buffer, and nothing is silently changed, on any failure path.

## 5. Config and wiring

```toml
# A WASM plugin: its .wasm file. Hooks are whatever the module exports — on_open, on_save or
# both. No imports are available to it, and it cannot see the filesystem.
plugin = "~/plugins/rano-minify.wasm"
```

- `RANO_PLUGIN` overrides the path (the test seam, and how a host sets one without a config file).
- `Editor` gains `plugin: Option<plugin::Plugin>`; `Editor::set_plugin` for a host. Compiled on
  first use (or at config load), once per process; a failure to load is reported once and leaves
  `None`.
- No per-hook switches: a module exporting only `on_save` is a save-only plugin, which is the
  honest expression of "what this module can do".
- No hot reload in v1 (the file is read once). A changed plugin takes effect on restart — noted in
  the README rather than implemented, because a reload needs an mtime check per save and a
  decision about a module that loads but cannot instantiate.

## 6. Touches

- **NEW `src/plugin.rs`** (`pub mod plugin`): `Plugin { module, engine, path }`,
  `Plugin::load(path) -> Result<Self, String>`, `Plugin::open(&self, text: &str) -> Result<Option<String>, String>`,
  `Plugin::save(...)`, the ABI reader (`rano_abi`, `rano_alloc`, `on_open`/`on_save`, the result
  block), the constants. Unit tests live here against WAT fixtures.
- `src/buffer.rs`: `Buffer::from_file_with(path, transform)` — the same function with one seam,
  so `from_file` stays for every existing caller and tests can pass a closure.
- `src/editor.rs`: `read_file(&self, path) -> io::Result<Buffer>` (the one place a file's text
  enters rano: `Buffer::from_file_with` + the plugin); `write_file`'s hook call and the
  buffer-replacement step; `ActionKind::Plugin`.
- `src/load_ctrl.rs`: `start_load`'s small-file path; the skip line for the rest. `load_now`,
  `open_file`, `do_read` route through `read_file`.
- `src/config.rs`: the `plugin` key (+ its test), `src/main.rs`: the field is set from config (no
  host-side draining — the plugin runs in-process, unlike pictures and the clipboard).
- `src/lib.rs`: the module + the host example line (`ed.set_plugin(...)`).
- `Cargo.toml`: `wasmi = "0.40"`; `[dev-dependencies] wat = "1"` (WAT fixtures compiled in the
  test, so no binary blob and no `wasm32` target needed for the unit suite).
- `README.md` (a "Plugins" section: the ABI in one WAT block, what a plugin cannot do, the
  determinism rule, the 16 MiB cap), `TODO.md` §23.

## 7. Tests (red first, in this order)

**`src/plugin.rs`, WAT fixtures inline** (`wat::parse_str` in a `dev-dependency`):

- `a_module_without_rano_abi_is_refused` — a bare module → `Err` naming the ABI.
- `a_module_that_imports_anything_is_refused` — one `(import "env" "x" (func))` → `Err`.
- `a_hook_returning_the_sentinel_leaves_the_text_alone` — `0xFFFFFFFF` → `None`.
- `a_hook_that_writes_a_result_block_replaces_the_text` — WAT copies a fixed string into memory
  and returns its block → `Some(...)`.
- `an_infinite_loop_runs_out_of_fuel_and_is_reported` — `(loop br 0)` → `Err` mentioning fuel,
  and the call returns in milliseconds.
- `a_hook_that_grows_memory_past_the_cap_is_refused` — a module declaring a huge memory or growing
  in a loop → `Err`, no allocation on this side.
- `output_that_is_not_utf8_is_refused`; `output_past_the_cap_is_refused`.
- `a_module_exporting_only_on_save_answers_only_that` — the other hook is `None`, not an error.

**`ed_tests.rs` (editor-level, with a small real plugin)**

- `opening_a_file_runs_the_open_hook` — a temp file, a plugin that uppercases; the rows are the
  hook's text, `modified == false`, and the file on disk is untouched.
- `saving_runs_the_save_hook_and_the_buffer_is_what_was_written` — the hook appends a line; the
  disk has it, the buffer equals it, `modified == false`, and `M-U` brings the pre-hook text back
  as one step.
- `a_second_save_writes_the_same_bytes` — the determinism rule, pinned.
- `a_plugin_that_traps_leaves_the_save_intact` — the file is written as the buffer had it, and the
  flash names the plugin.
- `a_binary_file_is_never_given_to_the_plugin` — a file with a NUL in its head opens read-only,
  unmodified, as it does today.
- `a_file_above_the_cap_skips_the_hook_and_says_so`.
- `insert_file_runs_the_open_hook` (`^R`, `do_read`).
- `a_tail_never_runs_the_hook` (log mode / `start_load_from`).

**End to end, ignored like the LSP e2e**

- `tests/fixtures/plugins/upper/` — a ~30-line Rust plugin (`rano_alloc`, `on_open`, `on_save`,
  `rano_abi`), its built `.wasm` (~200 B, committed), and `scripts/build-plugin-fixture.sh` that
  rebuilds it (`rustup target add wasm32-unknown-unknown`; **not** run in CI, which compiles the
  committed bytes).

## 8. Steps

1. **The dependency, and the number.** `cargo add wasmi`, the `[dev-dependencies] wat`, and a
   throwaway `#[test]` that instantiates an empty module — confirming +0.88 MiB in *this* build.
   Nothing else. This is the step to revert if the ruling is "no".
2. **`src/plugin.rs`** — ABI read, fuel, limiter, the result block; the WAT unit tests above.
3. **Config + wiring** — the `plugin` key, `Editor::set_plugin`, load-once and the one-line
   failure.
4. **The save hook** — `write_file`, `ActionKind::Plugin`, the buffer replacement, and the editor
   tests for save.
5. **The open hook** — `Buffer::from_file_with`, `Editor::read_file`, the four sync call sites, the
   `start_load` small-file path and the skip line, and the open-side editor tests.
6. **The fixture and the documentation** — the Rust plugin + script + ignored e2e, README
   "Plugins", TODO §23.

Each step lands with its tests green and is independently revertable; 1–3 are invisible to a user
with no `plugin` set.

## 9. Risks and interactions

- **Two writers, one invariant.** `write_file`'s stamp is taken after the write, and `save_to`
  compares it on the next save — a hook that replaces the buffer before the write keeps
  buffer == file, which is what stops the next save from asking "changed on disk?". Pinned by
  `a_second_save_writes_the_same_bytes`.
- **The loader.** Deciding "small files take the sync path when a plugin is on" changes how the
  startup file is read; the plan makes it explicit and bounded (16 MiB), and the log/tail paths
  are excluded by rule.
- **Library surface.** `plugin` is part of `rano::plugin` and `Editor::set_plugin`, so a host
  (letibot) can use it; whether the *dependency* is gated is §4.1's question.
- **A plugin in the save path** is a new way for a save to be slow. Fuel bounds the work; the
  16 MiB cap bounds the copy; the failure rule means a slow-but-legal plugin can delay a save but
  cannot corrupt one. A wall clock is *not* added in v1 — with no imports, fuel is a real bound.
- **Determinism** is a documented contract, not enforced: a hook that maps `a → b` and `b → c`
  would churn on every save. The README states "hooks must be deterministic"; the test suite pins
  the case it can see.

## 10. Open questions (the operator's, and each is cheap to answer)

1. **Default-on, or `--features wasm`?** §4.1. My recommendation: default-on.
2. **Stateless per call** (v1, §3) or a long-lived instance a plugin could keep state in? My
   recommendation: stateless now; state when a plugin needs it.
3. **One plugin** (v1) or a list with an order? My recommendation: one now; a list is a config
   shape and a loop, and it can wait for a second plugin to exist.
