//! **An STL viewer as a rano plugin, in Zig**: the model becomes a picture, or, on a terminal
//! with no graphics protocol, a luminance ramp in the text buffer. This is the example for both
//! additions the plugin side grew for it: the `read_file` capability (a provider is handed a path,
//! so it must read the bytes itself) and the `provider_picture` hook (an RGB answer, not text).
//!
//! ```toml
//! plugin = "…/examples/plugin-stl/plugin.wasm"
//! capabilities = ["read-file"]
//! ```
//!
//! # What it answers
//!
//! - `provider_picture(path, cols, rows)` for a `*.stl` path: `[width u32][height u32][rgb]`, at
//!   `width = cols` and `height = 2*rows`, because a terminal cell is about twice as tall as it is
//!   wide. An image with that aspect is drawn into the cell area without the model looking squashed.
//! - `provider_open(path, cols, rows)` for the same path: the same render reduced to one character
//!   per cell through a luminance ramp, so a terminal with no graphics protocol still sees the
//!   model. Which of the two the host asks is the host's decision; this plugin answers both.
//! - The "not mine" sentinel for every other path, so rano opens it the ordinary way.
//!
//! # The ABI, written by hand
//!
//! No SDK, no WASI, no general-purpose allocator: the exports the host looks for are written
//! directly, `rano_alloc` bumps a fixed buffer, and everything else is a fixed array. The host
//! makes a fresh instance per call (plans/S10 §2.1), so there is nothing to free and nothing that
//! accumulates.
//!
//! ```sh
//! zig build -Doptimize=ReleaseSmall     # zig-out/bin/plugin.wasm
//! cp zig-out/bin/plugin.wasm plugin.wasm
//! ```
//!
//! # Limits, and why they are refusals rather than clips
//!
//! The host allows 50 million units of fuel and 64 MiB of guest memory per call, and 16 MiB of
//! bytes each way. Rendering is not free, so this plugin refuses rather than half-renders: a model
//! above 12,000 facets or 1 MiB, or a viewport above 131,072 pixels (`cols * 2*rows`), is answered
//! with "not mine" and rano opens the file as text. The read itself asks the host for exactly the
//! buffer size it can use, so an oversized model is never copied into this memory at all.
//!
//! # Licence
//!
//! The STL reading, the 3D maths and the depth-buffered flat-shaded rasterising are ported from
//! **letim**, which is Apache-2.0 as of commit `15d2338` (`src/stl.zig`, `src/math3d.zig`,
//! `src/raster.zig`). This file, and the wiring around them, is rano's own.

const std = @import("std");
const stl = @import("stl.zig");
const raster = @import("raster.zig");
const math3d = @import("math3d.zig");

/// The ABI's "not mine" answer: `0xFFFFFFFF` as the host reads it.
const not_mine: u32 = 0xFFFFFFFF;

/// The host's `rano.read_file`: bytes written, `-needed` for a buffer too small, `i32::MIN` when
/// the host will not answer — which includes a path other than the one this call is about.
extern "rano" fn read_file(path_ptr: u32, path_len: u32, out_ptr: u32, out_cap: u32) i32;

/// The most file bytes this plugin will take. It asks `read_file` for exactly this much, so a
/// larger model is refused by the host before a byte of it reaches this memory.
const max_file: usize = 1 << 20;

/// The most facets it will parse and draw. Past this the hook's fuel would not render the model
/// either, so it says "not mine" instead of drawing half of it.
const max_tris: usize = 12_000;

/// The most pixels it will render: `cols * 2*rows`. 300 columns by 200 rows is 120,000.
const max_pixels: usize = 1 << 17;

/// One character per cell of the fallback text, and a newline per row on top of that.
const max_cells: usize = max_pixels / 2;

/// Where the host writes its input (the path), and where `rano_alloc` hands out room.
var arena: [4096]u8 = undefined;
var arena_used: usize = 0;

/// The model's bytes, straight from `read_file`.
var file_buf: [max_file]u8 = undefined;

/// The parsed triangles: one fixed array, refilled per call.
var tris: [max_tris]stl.Triangle = undefined;

/// The depth buffer that goes with the picture, and the picture itself.
var depth: [max_pixels]f32 = undefined;

/// The fallback text: one character per cell plus a newline per row.
var text: [2 * max_cells]u8 = undefined;

/// The picture, laid out exactly as the ABI wants it: `[width u32][height u32][rgb …]`. An
/// `extern struct` is C layout, so `rgb` follows the two `u32`s with no padding.
const Picture = extern struct {
    width: u32 = 0,
    height: u32 = 0,
    // **Three bytes a pixel**, and the bound `fits` enforces is a PIXEL count: `max_pixels`
    // bytes here is a third of the picture and writes 262,144 bytes past the array at the
    // largest viewport `fits` admits. ReleaseSmall has no runtime safety, so it corrupts the
    // neighbouring statics silently.
    rgb: [3 * max_pixels]u8 = undefined,
};
var picture: Picture = .{};

/// The result block: `{out_ptr, out_len}`, two little-endian `u32`s.
var block: [2]u32 = undefined;

// -- the ABI ------------------------------------------------------------------

export fn rano_abi() i32 {
    return 1;
}

/// A bump allocator over one fixed buffer. A fresh instance per call means the bump starts at
/// zero every time, and `0xFFFFFFFF` is not an offset the host will accept.
export fn rano_alloc(n: u32) u32 {
    const want: usize = n;
    if (want > arena.len - arena_used) return not_mine;
    const at = arena_used;
    arena_used += want;
    return @intCast(@intFromPtr(&arena) + at);
}

export fn provider_picture(path_ptr: u32, path_len: u32, cols: u32, rows: u32) u32 {
    const mesh = model(path_ptr, path_len, cols, rows) orelse return not_mine;
    // Two pixels per cell, so the picture has the aspect of the cell area it is drawn into.
    const height = rows * 2;
    render(mesh, cols, height);
    picture.width = cols;
    picture.height = height;
    // Eight bytes of header, not twelve: the struct is two `u32`s and the pixels, and a length
    // that disagrees with the layout is a picture the host refuses.
    return answer(@intFromPtr(&picture), 8 + @as(usize, cols) * height * 3);
}

/// The same model, as text: one character per cell through a luminance ramp, for a terminal that
/// cannot take a picture.
export fn provider_open(path_ptr: u32, path_len: u32, cols: u32, rows: u32) u32 {
    const mesh = model(path_ptr, path_len, cols, rows) orelse return not_mine;
    render(mesh, cols, rows * 2);
    return answer(@intFromPtr(&text), ramp(cols, rows));
}

// -- reading the model --------------------------------------------------------

/// The path this call is about, read and parsed — or `null` for "not mine": not an STL, not a
/// viewport this plugin can fill, or bytes the host would not hand over.
fn model(path_ptr: u32, path_len: u32, cols: u32, rows: u32) ?stl.Mesh {
    if (!fits(cols, rows)) return null;
    const path = guestSlice(path_ptr, path_len) orelse return null;
    if (!isStl(path)) return null;
    const bytes = readModel(path) orelse return null;
    return stl.parse(bytes, &tris) catch null;
}

/// `read_file` with the one path this call was handed. The host answers only for that file, so
/// this is not a filesystem read — it is the model the reader asked to see.
fn readModel(path: []const u8) ?[]const u8 {
    const got = read_file(
        @intCast(@intFromPtr(path.ptr)),
        @intCast(path.len),
        @intCast(@intFromPtr(&file_buf)),
        @intCast(file_buf.len),
    );
    // Negative is "refused" or "your buffer is too small", and neither is a model this plugin
    // can draw.
    if (got <= 0) return null;
    return file_buf[0..@intCast(got)];
}

fn isStl(path: []const u8) bool {
    if (path.len < 4) return false;
    return std.ascii.eqlIgnoreCase(path[path.len - 4 ..], ".stl");
}

/// The host's input, but only where this plugin's allocator could have put it: a pointer outside
/// the arena is a host bug or a trap, and either way it is "not mine".
fn guestSlice(ptr: u32, len: u32) ?[]u8 {
    const base = @intFromPtr(&arena);
    const at: usize = ptr;
    if (at < base) return null;
    const off = at - base;
    if (len > arena.len or off > arena.len - len) return null;
    return arena[off..][0..len];
}

fn fits(cols: u32, rows: u32) bool {
    if (cols == 0 or rows == 0) return false;
    return @as(u64, cols) * @as(u64, rows) * 2 <= max_pixels;
}

// -- drawing ------------------------------------------------------------------

/// The still, three-quarter view the model opens at. A viewer with a mouse would orbit it; a
/// provider answers once, so the angle is chosen to show depth in a single frame.
const yaw: f32 = 0.7;
const pitch: f32 = 0.3;
const fov: f32 = 0.9;
/// Close enough that a model of unit bounding radius fills most of the view, far enough that it
/// never crosses the near plane (the nearest vertex sits at `distance - 1`).
const distance: f32 = 2.4;
/// Dark, so a lit facet reads against it on any terminal theme.
const background = [3]u8{ 0x14, 0x16, 0x1c };

fn render(mesh: stl.Mesh, width: u32, height: u32) void {
    const px = @as(usize, width) * height;
    var target = raster.Target{};
    target.aim(width, height, picture.rgb[0 .. px * 3], depth[0..px]);
    target.clear(background);

    var cam = math3d.Camera.forViewport(width, height, fov);
    cam.distance = distance;

    const rotation = (math3d.Orbit{ .yaw = yaw, .pitch = pitch }).matrix();
    const centre = mesh.centre();
    const scale = 1.0 / mesh.radius();
    const shade = raster.Shade{};

    for (mesh.triangles) |tri| {
        var p: [3]math3d.Projected = undefined;
        for (tri.v, 0..) |v, k| {
            // Centre and scale the model so any file frames the same way.
            p[k] = cam.project(rotation, v.sub(centre).scale(scale));
        }

        // STL normals are in model space; shading happens in view space, where the light lives.
        // A facet facing away is flipped rather than dropped: culling is only safe when the
        // file's winding is consistent, and STL does not promise that.
        var normal = rotation.mulVec(tri.normal);
        if (normal.z > 0) normal = normal.scale(-1);

        target.triangle(p[0], p[1], p[2], shade, normal);
    }
}

/// The picture as one character per cell, darkest to brightest, with a newline per row.
fn ramp(cols: u32, rows: u32) usize {
    const chars = " .:-=+*#%@";
    var at: usize = 0;
    var y: u32 = 0;
    while (y < rows) : (y += 1) {
        var x: u32 = 0;
        while (x < cols) : (x += 1) {
            // Two pixels per cell, so the character takes their mean and the art keeps the
            // model's proportions.
            const top = @as(usize, y * 2) * cols + x;
            const lum = (luminance(top) + luminance(top + cols)) / 2;
            const last: f32 = @floatFromInt(chars.len - 1);
            const i: usize = @intFromFloat(lum * last + 0.5);
            text[at] = chars[@min(chars.len - 1, i)];
            at += 1;
        }
        text[at] = '\n';
        at += 1;
    }
    return at;
}

fn luminance(i: usize) f32 {
    const o = i * 3;
    const r: f32 = @floatFromInt(picture.rgb[o]);
    const g: f32 = @floatFromInt(picture.rgb[o + 1]);
    const b: f32 = @floatFromInt(picture.rgb[o + 2]);
    return (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
}

/// Answer the ABI's result block: `{out_ptr, out_len}`, and its offset.
fn answer(at: usize, len: usize) u32 {
    block[0] = @intCast(at);
    block[1] = @intCast(len);
    return @intCast(@intFromPtr(&block));
}
