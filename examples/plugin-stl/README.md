# An STL viewer as a rano plugin

A model file, drawn. This is the example for the two capabilities a viewer needs:

- **`read-file`** — a provider is handed a path, so it reads the bytes itself. The host answers
  only for the path that call is about, which is why this plugin asks for exactly one file.
- **`provider_picture`** — the answer is an RGB picture, not text.

It is written in **Zig**, against the raw ABI: no SDK, no WASI, a fixed-buffer allocator, and
`rano_abi` / `rano_alloc` / `provider_picture` / `provider_open` written by hand. That is the
point of the example — a plugin is a contract, not a Rust project.

```sh
RANO_PLUGIN=$PWD/examples/plugin-stl/plugin.wasm \
RANO_CAPABILITIES=read-file \
rano examples/plugin-stl/fixtures/cube.stl
```

## What it answers

| export | for a `*.stl` path | for anything else |
|---|---|---|
| `provider_picture` | `[width u32][height u32][rgb]`, `cols` × `2*rows` pixels | "not mine" |
| `provider_open` | the same render as `rows` lines of `cols` characters, through a luminance ramp | "not mine" |

A cell is about twice as tall as it is wide, so the picture is rendered at two pixels per cell
and the character fallback averages each pair: the model keeps its proportions either way. A host
that can draw a picture asks `provider_picture`; one that cannot asks `provider_open` and gets
the ramp.

The view is still and three-quarter (yaw 0.7, pitch 0.3, a headlight and a fill), because a
provider answers once and there is no mouse to orbit with.

## Limits

It refuses rather than half-draws, and says "not mine" instead:

- a model above **12,000 facets or 1 MiB** (the hook's fuel would not render it either),
- a viewport above **131,072 pixels** (`cols * 2 * rows`),
- any path that is not the one the call was about.

The read is asked for with exactly the buffer this plugin can fill, so an oversized model is
never copied into its memory at all.

## Building

Zig 0.16.0, target `wasm32-freestanding`, no entry point, `ReleaseSmall`, `-rdynamic` so the
`export fn`s are in the module's export section:

```sh
zig build -Doptimize=ReleaseSmall     # zig-out/bin/plugin.wasm, ~32 KB
cp zig-out/bin/plugin.wasm plugin.wasm
```

`plugin.wasm` beside this file is the built artifact, committed the way
`examples/plugin-upper/plugin.wasm` is, so the example runs without a Zig toolchain.

## Fixtures

`fixtures/cube.stl` is a 12-facet ASCII cube; `fixtures/cube-binary.stl` is the same cube in the
binary flavour, generated from it by the plugin's own reader:

```sh
zig build fixtures
```

The test in `src/plugin.rs` renders both and asserts the two pictures are identical, which is
what says the two parsers agree.

## Licence

`src/stl.zig`, `src/math3d.zig` and `src/raster.zig` are ported from **letim**, which is
Apache-2.0 as of commit `15d2338`. The ABI wiring, the hooks and the build are rano's own.
