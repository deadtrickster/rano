# S10 — A plugin platform: WASM hooks, builtins, and reload without watching

Grounded to code as of 2026-10-18: `rano` 0.9.0 @ `669ab57` plus the folder listing landed in
`src/dirview.rs` (step 0 below) and **steps 1–3: `src/plugin.rs` on wasmtime 49, the config's
`plugin` key, and both hooks wired** (`read_file`/`run_save_hook`, `ActionKind::Plugin`). Anchors: `src/editor.rs` (`write_file` ~:3085,
`format_for_save` ~:3040, `open_dir`/`start_dir`/`dir_enter`/`dir_parent`/`dir_refresh`),
`src/buffer.rs` (`from_file` :97, `from_text` :116), `src/load_ctrl.rs` (`start_load` :25,
`load_poll` :239, `Adopted::Finished` :351, `load_now` :513), `src/config.rs` (`parse_config`
:203–259), `src/lib.rs` (modules :83–184).

Runtime facts verified in the local registry source (wasmi 0.40.0: `store.rs` `set_fuel` :292,
`get_fuel` :303, `limiter` :913; `limits.rs` `ResourceLimiter::memory_growing` :61), and cost
measured with rano's release profile (`strip`, `lto`, `codegen-units=1`; the probe also set
`panic=abort`, which rano does not, so the real delta is if anything slightly larger):

**Ruled: wasmtime** — the operator, *"jit, we will pay for it once per callsite and that is ok.
because later we will rearchitect rano and letibot around it."* The engine is behind the `Plugin`
façade, so the ruling costs one dependency and no ABI.

| | wasmi 0.40 | wasmtime 40 (`std`+`runtime`+`cranelift`) |
|---|---|---|
| engine | interpreter, no JIT, no unsafe codegen | Cranelift |
| crates resolved | **14** | **185** |
| binary delta | **+0.88 MiB** (302,448 B → 1,229,920 B) | **+4.55 MiB** (302,448 B → 5,073,264 B) |
| module compile, once | 903 µs | 3.17 ms |
| **1 KiB** (uppercase loop) | 12.4 µs | **15.3 µs** — slower: instantiation + call overhead |
| **1 MiB** | 8.3 ms | **483 µs** — 17× faster |
| **16 MiB** (the cap) | ~136 ms (extrapolated; the probe module held 4 MiB) | **7.8 ms** — ~17× faster |
| **in rano's own build** (0.9.0, wasmtime **49**, release) | — | **+6.08 MiB**: 18,497,168 B → 24,870,208 B |

---

## 0. What the operator asked for, in order

1. *"i want to have some WASM integration finally. lets create a plan first. something simple -
   say we start with file open/file save hooks."*
2. *"so, jit is absolutely needed"* — the engine question, and §4 answers it with measurements
   and a per-plugin probe rather than a reputation.
3. *"so is some sort of 'slow' plugin detection"* — §6.
4. *"and then - builtin editor for them"* / *"like we have a folder where source code lives and it
   can be opened like a dired and i can enter files subfolders supported"* — §2.3 and step 0.
5. *"so, we start with save and open it doesnt mean we stop"* and *"and we will definitely have
   more interesting hooks"* — §2.1: the ABI is built for hooks to be *added*, not for two.
6. *"say the whole 'scripts folder like dired' can be a wasm plugin itself, just builtin"* — §2.3:
   builtin plugins are the same registry as user plugins.
7. *"and we need dynamic reload obviously"* + *"we dont watch the files, reload is a user triggered
   or dbus triggered event"* — §7.
8. *"dbus can be a builtin plugin too btw"* — §8: the bus service is a builtin plugin, not core.

**Already landed (step 0, Rust):** the dired — `src/dirview.rs` (listing: directories first,
`/` marker, case-insensitive), `M-g`/`F8`/`rano folder/` → `Editor::open_dir`, `RET` enters,
`-` up, `r` refresh keeping the cursor, `q` closes, read-only, tests. It is the *reference* for
what a builtin dired plugin must produce, and the fallback for a build with plugins off.

## 1. The platform, in one paragraph

A **plugin** is a component that answers **hooks**. A hook is a named function with a fixed
signature; the host discovers what a plugin answers by **enumerating the module's exports**
(`on_<event>`, `provider_<event>`), so adding a hook is a host-side change and never an ABI
version bump. A plugin is either **WASM** (a `.wasm` file from the config) or **builtin** (shipped
in the binary and registered in the same registry — the dired, and the bus service). Hooks that
need the world outside the text ask for a **capability** (`read-dir`, later `bus`), which the
config grants and the host implements as an import; a module that imports anything not granted is
refused at instantiation.

## 2. Hooks

### 2.1 Discovery, and why not two hardcoded exports

The stage-1 pair is `on_open` and `on_save` — **text in, text out**. The point of *"we will
definitely have more interesting hooks"* is that the second and third hook should cost a host-side
function, not a schema migration:

```wat
;; what every module must export
(memory (export "memory") 1)
(func (export "rano_abi") (result i32) i32.const 1)          ;; 1; anything else is refused
(func (export "rano_alloc") (param i32) (result i32))        ;; reserve n bytes -> offset

;; hooks: discovered by NAME from the export section
(func (export "on_open") (param i32 i32) (result i32))       ;; (in_ptr, in_len) -> block_ptr
(func (export "on_save") (param i32 i32) (result i32))
(func (export "provider_open") (param i32 i32) (result i32)) ;; §2.3
```

A hook returns the offset of a **result block** — two little-endian `u32`s, `{out_ptr, out_len}` —
or `0xFFFFFFFF` for **unchanged**. The host writes the UTF-8 input at `rano_alloc`'s answer and
reads the block back; the block is the one structure every language writes without agreeing on
layout.

**A fresh `Store` and instance per call**, with fuel and a memory cap set for that call: nothing
accumulates, the limits are per call, and guest state across calls is explicitly *not* implied by
the ABI (if a later hook needs it, that is a v2 decision with a name).

### 2.2 The hooks in stage 1

| hook | input | when |
|---|---|---|
| `on_open` | the file's decoded text | after `encoding::decode` **and after the binary check** (a binary is read-only and never reaches a hook), before `from_text` |
| `on_save` | the text about to be written | in `write_file`, **after `format_for_save`** (the language's rule precedes the reader's — gofmt first), before `file_bytes()` |

**The save hook replaces the buffer** with its output as one undo step (`ActionKind::Plugin`)
*then* writes — what gofmt-on-save already does, and for the same reason: the glass and the file
must be the same text. Consequence, documented and tested: **hooks must be deterministic**.

**The streaming loader cannot hook**: a transform needs the whole text and the rows are already
drawn. With a plugin configured, files ≤ `MAX_HOOK_BYTES` (16 MiB) take the synchronous path;
larger files, tails (`start_load_from`) and log mode skip the hook **with one line saying so**.

### 2.3 Providers, and the dired as a builtin plugin

A **provider** answers "what should this path *open as*": `provider_open(path_text) -> text`, or
`0xFFFFFFFF` for "not mine, open it normally". That is enough to express a folder listing, a
hex-ish viewer, a virtual document — the class of things *"a folder … opened like a dired"* is.

It needs the one thing the text hooks did not: **a capability**. `read-dir` exposes a host import

```wat
(func (import "rano" "read_dir") (param i32 i32 i32 i32) (result i32))
;; (path_ptr, path_len, out_ptr, out_cap) -> bytes written, or -needed for too small
```

and the listing *format* is the contract the Rust dired already defines (`dirview::rows`: one
entry per row, directories first, `name/`). The builtin dired is registered in the same registry
as a user plugin, so a build with plugins off still has the Rust one, and the two are compared by
the same test.

**The DBus service is a builtin plugin too** (§8) — the registry is the extension point, not a
special case per integration.

### 2.4 What "builtin" means: shipped, unpacked, and the reader's to edit

The operator, on the word: *"built in as in 'ships with rano and unpacks to .../scripts/built-in/
- still user editable"*. So a builtin is **not compiled into the editor's paths**: it is a script
that rides in the binary (`include_bytes!`), is **written out on first use** to
`$XDG_CONFIG_HOME/rano/scripts/built-in/` (else `~/.config/rano/scripts/built-in/`), and is
loaded from there exactly like a plugin the reader installed by hand.

- **Never overwritten.** Unpacking writes a file only when it is absent; an edited or replaced
  builtin stays put. A marker (`scripts/built-in/.rano-version`) records what was unpacked and
  from which release, so an upgrade can say *"the shipped dired is newer; the one in your
  scripts folder was left alone"* rather than silently reverting somebody's work.
- **Editable means editable**: the file in that folder is the plugin that runs. Replace the
  `.wasm` with your own build and that is the dired; delete it and the Rust fallback (§2.3) is
  what opens a folder. The folder is also what `M-g` is good for — it is a folder like any other,
  so *"builtin editor for them"* is a listing and `RET` away.
- **`scripts/` is the reader's**: `.../rano/scripts/` holds `built-in/` (unpacked) and whatever
  else they put beside it (`*​.wasm` discovered in the folder is a plugin too, in name order), so a
  builtin can be copied, renamed and modified without touching the shipped copy.
- **Nothing runs that the config did not enable**: unpacking writes files; it does not load one
  unless `plugin` (or the `scripts/` folder) says so.

## 3. Capabilities

- Declared in the config: `capabilities = ["read-dir"]`. Only those imports exist for the module;
  a module importing anything else fails to instantiate and the failure names the import.
- **The first capability is `read-dir`**, and the plan treats it as the precedent: adding a
  capability is a deliberate act with a test, because it is the moment a plugin stops being a pure
  function over bytes.
- Nothing in stage 1 grants `bus`, `read-file`, `write-file`, `run`, or network. A later
  `capabilities` list that names one is a decision with its own section here.

## 4. The engine: measured, and then decided per plugin

A hook runs **once per open and once per save** — never per keystroke, never per frame — so the
cost that is always paid is the binary and the lockfile, and the cost that is sometimes paid is
per-byte work. **Measured on the same WAT module, same host, same profile** (top table):

- **The JIT is 17× faster per byte.** 1 MiB: 483 µs against 8.3 ms. 16 MiB: 7.8 ms against
  ~136 ms. On the file that is large enough to matter, that is a real difference: an eighth of a
  second a reader can feel, against one they cannot.
- **The JIT is 5× the size and 13× the crates.** +4.55 MiB against +0.88 MiB, 185 crates against
  14 — a quarter of the binary, for a feature that does nothing when no plugin is installed.
- **At small sizes the interpreter wins**: 12.4 µs against 15.3 µs at 1 KiB, because a wasmtime
  call pays instantiation and call overhead the interpreter does not. A 100 KB file costs either
  way a fraction of a millisecond.

So *"jit is absolutely needed"* is true for **what the plugin does**, not for the hook itself. The
plan's answer is the probe (§6): it reports each hook's cost per MiB on this machine, so the engine
question becomes "is this plugin slow?" — a measurement — and the engine stays behind the `Plugin`
façade, where switching it is one line and the ABI does not move. If the ruling is "wasmtime from
the start", §4.1's cost is the number above, and the gate question in §13 comes back with it.

## 5. Bounds (all of them per call)

- `HOOK_FUEL = 50_000_000` with `Config::consume_fuel(true)` — an infinite loop ends in
  `OutOfFuel`, not a hung editor. Fuel is deterministic and, with no *granted* import able to
  block, a real bound on work.
- `HOOK_MEMORY = 64 MiB` via `Store::limiter(ResourceLimiter)`, refusing a memory that grows past
  it.
- `MAX_HOOK_BYTES = 16 MiB` in, the same out: a hook cannot inflate a one-line file into a
  gigabyte, and the copy through guest memory stays bounded.
- A fresh store per call, so a leak cannot accumulate.

## 6. What a hook costs: the slow-plugin rule

*"so is some sort of 'slow' plugin detection"* — yes, and it is what makes the engine question a
measurement instead of a guess.

**At load, a probe.** Each exported hook is called once on a synthetic 64 KiB input (fuel-capped,
wall-timed). The result is one line and a datum:

```
plugin: on_save 0.6 ms on 64 KiB (~9 ms/MiB) — ok
plugin: on_open 41 ms on 64 KiB (~660 ms/MiB) — SLOW (above 250 ms/MiB)
```

The extrapolation is stated as one (a transform is linear in the input), and the fuel burnt is
recorded beside the time because **fuel is the engine-independent measure**: switch wasmi for
wasmtime later and the same plugin's fuel count compares.

**Per call, a counter.** Every real call's wall time and fuel are accumulated per hook. Crossing
`SLOW_CALL = 50 ms` says so **once per session per hook** (`plugin: on_save took 240 ms on 1.2
MiB`), and after `SLOW_DISABLE = 3` such calls the hook is turned off for the session with the
reason — a save that takes a second every time is a plugin the reader would rather know about than
keep paying for. `M-x plugin-reload` (or `r` in the plugin listing) brings it back; the counter
survives a reload so the same plugin is not re-adopted by accident.

**Nothing is ever failed on slowness**: fuel bounds the work, the counters report, and only the
third crossing disables — a *slow* hook is never a reason to lose a save.

## 7. Reload: user-triggered, or the bus — never a watcher

*"we dont watch the files"* is already rano's rule (README, "Files changed on disk"), and it holds
for plugins:

- **User-triggered**: `M-x plugin-reload` (a command; also a key in the plugin listing).
- **Bus-triggered**: a method on rano's session-bus service (§8) — a build system, an editor
  hook, or `busctl call` can ask without touching the editor's keyboard.
- **No filesystem watching, no mtime polling.** A hook call does not `stat` the module: the module
  in memory is the module until something asks for a reload.

**A reload is atomic and never loses a working plugin.** It reads the file, compiles it into a
*new* module, runs the probe, and only then swaps it in. A file that is half-written, missing, or
for the wrong ABI leaves the old module in place and says so once (`plugin: reload failed — kept
the one already loaded`). Guest state is free to reset: there is none between calls (§2.1).

## 8. The bus service, as a builtin plugin

- **Name**: `dev.rano.Editor` on the session bus, object `/dev/rano/Editor`, interface
  `dev.rano.Plugin1`.
- **Methods**, in the order they earn their place: `Reload()` (the reload above), `Status()` (the
  probe's numbers and the counters, as a struct — what a plugin pane would draw), and `Open(path)`
  (which also answers "show me this file in the running rano" for a head that has no pane of its
  own).
- **Cost, to be measured before step 1 of this part**: `zbus` (pure Rust) versus the C `dbus`
  crate. The plan does not pick one by name; the probe measures the delta the way §4's table does
  for the engine, and the ruling is the operator's. If a bus library is too heavy for what is one
  method, the fallback is a Unix-socket control channel with the same three messages — but that is
  a fallback, and the operator asked for DBus.
- **Builtin, not core, and unpacked like any other** (§2.4): the service is a script in
  `scripts/built-in/`, starts when the config enables it (`bus = true`), and a build without it is
  a build without the service — not a build with a hole in the editor. Its manifest (name, methods,
  capabilities) is a plain file beside it, so a reader can see and change what it is allowed to do.

## 9. Failure semantics (one line each, nothing lost)

| what went wrong | what happens |
|---|---|
| no `rano_abi`, or not `1` | hook skipped: `plugin: no ABI (wanted 1) — opened unchanged` |
| an import the config does not grant | module refused: `plugin: read_file is not granted — skipped` |
| trap / `OutOfFuel` / memory cap | hook skipped, the save written as the buffer had it |
| output not UTF-8, or over the cap | hook skipped, named |
| the module file missing or not wasm | plugin off for the session, said once at load |
| **reload** failed | the old module stays, said once |
| the probe is slow | reported (§6); three crossings disable the hook for the session |
| any failure on **save** | the file is written **as the buffer had it** — a plugin is never a precondition for a save |

## 10. Touches

- **NEW `src/plugin.rs`** (`pub mod plugin`): `Registry` (builtins + the configured `.wasm`),
  `Plugin::load`, the ABI reader (enumeration, `rano_alloc`, the result block), fuel/limiter
  per call, the probe and the counters, the capability imports, `reload()`.
- `src/buffer.rs`: `from_file_with(path, transform)` — one seam, so `from_file` keeps its callers.
- `src/editor.rs`: `read_file` (the one place a file's text enters: `from_file_with` + provider +
  `on_open`), `write_file`'s hook + buffer replacement + `ActionKind::Plugin`, the `plugin` field,
  `set_plugin`, `plugin_reload`.
- `src/load_ctrl.rs`: the small-file path in `start_load`, the skip line for the rest.
- `src/config.rs`: `plugin`, `capabilities`, `bus` (+ tests); `src/main.rs`: wiring (the bus
  service starts here, in the standalone binary).
- **NEW `src/bus.rs`**: the builtin bus plugin (zbus or the fallback), `dev.rano.Editor`.
- `src/lib.rs`, `Cargo.toml` (`wasmi`; `zbus` if chosen; `[dev-dependencies] wat`), `README.md`
  (a Plugins section: the ABI in one WAT block, capabilities, determinism, the caps, reload),
  `TODO.md` §23.

## 11. Tests

**`src/plugin.rs`, WAT fixtures compiled in the tests** (`wat` as a dev-dependency — no binary
blob, no `wasm32` target for the unit suite):

- `a_module_without_rano_abi_is_refused`; `a_module_that_imports_an_ungranted_capability_is_refused`;
  `a_granted_import_is_the_only_one_visible`.
- `a_hook_returning_the_sentinel_leaves_the_text_alone`; `a_hook_that_writes_a_block_replaces_it`.
- `an_infinite_loop_runs_out_of_fuel_and_is_reported` (returns in ms);
  `a_hook_that_grows_memory_past_the_cap_is_refused`.
- `output_that_is_not_utf8_is_refused`; `output_past_the_cap_is_refused`;
  `a_module_exporting_only_on_save_answers_only_that`.
- `the_probe_reports_a_cost_and_marks_a_slow_hook`; `three_slow_calls_disable_the_hook_once`;
  `a_reload_that_fails_keeps_the_loaded_module`; `a_reload_that_succeeds_swaps_and_re_probes`.

**Editor-level** (`ed_tests.rs`): open transforms (and the file on disk is untouched); save writes
the hook's text and the buffer equals it, `M-U` one step; a second save writes the same bytes;
a trapping plugin leaves the save intact and names itself; a binary file never reaches a hook;
a file above the cap skips with a line; `^R` runs `on_open`; a tail never runs it.

**Providers**: a WAT provider that answers `provider_open` with a listing for a path, over
`read-dir`; and the builtin dired compared with it **row for row** — the contract §2.3 names.

**Bus**: with the service on, `Reload()` from a second thread swaps the module and `Status()`
reports the probe's numbers; with it off, nothing listens.

**End to end, ignored**: a ~30-line Rust plugin (`rano_alloc`, `on_open`, `on_save`, `rano_abi`)
committed as `.wasm` with its source and `scripts/build-plugin-fixture.sh` (not run in CI).

## 12. Steps

0. **Landed (Rust)**: the dired — `src/dirview.rs`, `M-g`, `rano folder/`, `RET`/`-`/`r`/`q`,
   read-only, tests. The reference format and the fallback.
1. **The runtime, and its number.** `cargo add wasmi` (+`wat` dev), a throwaway instantiation
   test, and the measured delta in *this* build; **and the same probe for wasmtime**, so §4's
   decision is made on two numbers rather than one. This is the step to revert if the ruling is
   "not this".
2. **`src/plugin.rs`** — enumeration, ABI, fuel/limiter, the result block; the WAT unit tests.
3. **Config + `Editor::set_plugin`** — load once, the one-line failure, `on_open`/`on_save` for
   the sync paths and the save path, `ActionKind::Plugin`.
4. **Capabilities + `read-dir`**, and `provider_open`; the builtin dired registered as a provider
   and compared with the Rust one.
5. **The probe and the counters** (§6), which is what makes "slow" a fact.
6. **Reload**: `M-x plugin-reload`, atomic swap, the failure rule.
7. **The bus plugin** (§8): the dependency measured, `Reload`/`Status`, then `Open(path)`.
8. **Docs** and the ignored end-to-end fixture.

Each step lands green and is independently revertable; 1–3 are invisible to a user with no
`plugin` set.

### 12.1 The unpacking step (added by §2.4)

- `src/scripts/` in the tree: `built-in/dired.wasm`, `built-in/dired.toml` (name, hooks,
  capabilities), `built-in/bus.wasm` — built by `scripts/build-plugins.sh` when a `wasm32`
  toolchain is present, committed as their outputs so CI needs no target.
- `plugin::unpack(config_dir)` on start: write each shipped script where it is absent, leave
  everything else alone, note an upgraded shipped script whose local copy was edited.
- A test that the second run does not overwrite an edited file, and that deleting one falls back
  to Rust (the `dirview` path) with nothing lost.

## 13. Open questions (each cheap, each the operator's)

1. **Engine for stage 1**, now with both numbers: **wasmi** (+0.88 MiB, 14 crates, 8.3 ms/MiB) or
   **wasmtime** (+4.55 MiB, 185 crates, 483 µs/MiB, and 15 µs against 12 µs on a tiny input)?
   The probe (§6) makes it a per-plugin fact after step 5 either way, and the façade makes the
   switch one line — but the binary's size is paid whether or not a plugin exists, so this is the
   one decision the operator should make first.
2. **Bus library**: `zbus` or a Unix socket with the same three messages? The plan's preference is
   whatever the measurement says; the operator asked for DBus, so the socket is only the fallback.
3. **The first capability** is `read-dir` — confirm that is the one to open the sandbox with.
4. Does the Rust dired stay as the fallback when the builtin script is missing, deleted or
   refuses (§2.4 says it is the fallback)? The plan's assumption: **yes, it stays** — a folder
   that cannot be listed because a script is absent would be a regression from step 0, and the
   Rust one is 200 lines that already has tests.
