//! Regenerates `fixtures/cube-binary.stl` from `fixtures/cube.stl`, so the binary fixture has a
//! source a reader can diff. It parses the ASCII file with the plugin's own reader, which is also
//! the shortest proof that the two parsers agree on one model.
//!
//! ```sh
//! zig build fixtures     # from examples/plugin-stl
//! ```
//!
//! Run natively (`zig run make_binary.zig`); this is not part of the wasm.

const std = @import("std");
const stl = @import("src/stl.zig");
const math3d = @import("src/math3d.zig");

const binary_header = 80;
const binary_facet = 50;

pub fn main() !void {
    const from = "fixtures/cube.stl";
    const to = "fixtures/cube-binary.stl";

    var source: [64 * 1024]u8 = undefined;
    const in = try std.posix.openat(std.posix.AT.FDCWD, from, .{ .ACCMODE = .RDONLY }, 0);
    defer _ = std.c.close(in);
    const n = try std.posix.read(in, &source);

    var tris: [4096]stl.Triangle = undefined;
    const mesh = try stl.parse(source[0..n], &tris);

    var out: [binary_header + 4 + 4096 * binary_facet]u8 = undefined;
    @memset(out[0..binary_header], 0);
    std.mem.writeInt(u32, out[binary_header..][0..4], @intCast(mesh.triangles.len), .little);

    var at: usize = binary_header + 4;
    for (mesh.triangles) |t| {
        writeVec3(out[at..][0..12], t.normal);
        writeVec3(out[at + 12 ..][0..12], t.v[0]);
        writeVec3(out[at + 24 ..][0..12], t.v[1]);
        writeVec3(out[at + 36 ..][0..12], t.v[2]);
        // The per-facet attribute byte count, which no reader uses.
        out[at + 48] = 0;
        out[at + 49] = 0;
        at += binary_facet;
    }

    const dst = try std.posix.openat(std.posix.AT.FDCWD, to, .{
        .ACCMODE = .WRONLY,
        .CREAT = true,
        .TRUNC = true,
    }, 0o644);
    defer _ = std.c.close(dst);
    var written: usize = 0;
    while (written < at) {
        const n2 = std.c.write(dst, out[written..].ptr, at - written);
        if (n2 < 0) return error.WriteFailed;
        written += @intCast(n2);
    }

    std.debug.print("{s}: {d} facets, {d} bytes\n", .{ to, mesh.triangles.len, at });
}

fn writeVec3(dst: []u8, v: math3d.Vec3) void {
    writeF32(dst[0..4], v.x);
    writeF32(dst[4..8], v.y);
    writeF32(dst[8..12], v.z);
}

fn writeF32(dst: []u8, v: f32) void {
    std.mem.writeInt(u32, dst[0..4], @bitCast(v), .little);
}
