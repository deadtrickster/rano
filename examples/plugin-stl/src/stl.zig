//! STL reader: binary and ASCII.
//!
//! Ported from letim, Apache-2.0 as of commit 15d2338 (`src/stl.zig`). STL is a triangle soup with
//! a stored normal per facet and nothing else: no texture coordinates, no colours, no vertex
//! sharing. That makes it easy to load and slightly awkward to draw, because the stored normals
//! are frequently wrong.
//!
//! The changes a plugin needs: the triangles come out of a fixed array the caller owns rather than
//! an allocator, a full array is `TooMany` rather than a reallocation, and there is no file I/O —
//! the bytes arrive from the host's `read_file` import.

const std = @import("std");
const math3d = @import("math3d.zig");
const Vec3 = math3d.Vec3;

/// Binary STL: an 80 byte header, a u32 count, then 50 bytes per facet.
const binary_header = 80;
const binary_facet = 50;

pub const Error = error{
    /// Not recognisable as either flavour of STL.
    NotAnStl,
    /// ASCII STL that ran out of vertices part way through a facet.
    TruncatedFacet,
    /// More facets than the caller's array holds.
    TooMany,
};

pub const Triangle = struct {
    normal: Vec3,
    v: [3]Vec3,

    /// The normal implied by the winding, ignoring whatever the file claimed.
    pub fn geometricNormal(t: Triangle) Vec3 {
        return t.v[1].sub(t.v[0]).cross(t.v[2].sub(t.v[0]));
    }
};

pub const Mesh = struct {
    triangles: []Triangle,
    min: Vec3,
    max: Vec3,

    pub fn centre(self: Mesh) Vec3 {
        return self.min.add(self.max).scale(0.5);
    }

    /// Radius of the bounding sphere about the centre, so a model whose long axis runs diagonally
    /// still fills the view.
    pub fn radius(self: Mesh) f32 {
        const c = self.centre();
        var r: f32 = 0;
        for (self.triangles) |t| {
            for (t.v) |p| r = @max(r, p.sub(c).length());
        }
        return if (r > 0) r else 1;
    }
};

/// Parse either flavour into `out`, deciding by size.
///
/// The size test is the reliable one. Some exporters write "solid" at the start of a *binary*
/// file, so sniffing the header text is not enough, but a binary file's length is exactly
/// determined by its facet count, which an ASCII file's almost never is.
pub fn parse(bytes: []const u8, out: []Triangle) Error!Mesh {
    if (binaryFacetCount(bytes)) |count| {
        if (count > out.len) return Error.TooMany;
        return parseBinary(bytes, count, out);
    }
    return parseAscii(bytes, out);
}

fn binaryFacetCount(bytes: []const u8) ?usize {
    if (bytes.len < binary_header + 4) return null;
    const count = std.mem.readInt(u32, bytes[binary_header..][0..4], .little);
    const expected = binary_header + 4 + @as(usize, count) * binary_facet;
    if (bytes.len != expected) return null;
    return count;
}

fn readF32(bytes: []const u8) f32 {
    return @bitCast(std.mem.readInt(u32, bytes[0..4], .little));
}

fn readVec3(bytes: []const u8) Vec3 {
    return .{ .x = readF32(bytes[0..4]), .y = readF32(bytes[4..8]), .z = readF32(bytes[8..12]) };
}

fn parseBinary(bytes: []const u8, count: usize, out: []Triangle) Mesh {
    const triangles = out[0..count];

    var offset: usize = binary_header + 4;
    for (triangles) |*t| {
        const raw = bytes[offset..][0..binary_facet];
        t.* = .{
            .normal = readVec3(raw[0..12]),
            .v = .{ readVec3(raw[12..24]), readVec3(raw[24..36]), readVec3(raw[36..48]) },
        };
        offset += binary_facet;
    }
    return finish(triangles);
}

fn parseAscii(bytes: []const u8, out: []Triangle) Error!Mesh {
    if (std.mem.indexOf(u8, bytes, "facet") == null) return Error.NotAnStl;

    var count: usize = 0;
    var pending_normal: Vec3 = .{};
    var verts: [3]Vec3 = undefined;
    var have: usize = 0;

    var lines = std.mem.tokenizeAny(u8, bytes, "\r\n");
    while (lines.next()) |raw_line| {
        const line = std.mem.trim(u8, raw_line, " \t");
        if (std.mem.startsWith(u8, line, "facet normal")) {
            pending_normal = parseThreeFloats(line["facet normal".len..]) orelse Vec3{};
            have = 0;
        } else if (std.mem.startsWith(u8, line, "vertex")) {
            if (have >= 3) continue; // a fourth vertex: ignore rather than corrupt
            verts[have] = parseThreeFloats(line["vertex".len..]) orelse
                return Error.NotAnStl;
            have += 1;
            if (have == 3) {
                if (count == out.len) return Error.TooMany;
                out[count] = .{ .normal = pending_normal, .v = verts };
                count += 1;
                have = 0;
            }
        }
    }

    if (have != 0) return Error.TruncatedFacet;
    if (count == 0) return Error.NotAnStl;
    return finish(out[0..count]);
}

/// The three floats that follow the keyword on a line.
fn parseThreeFloats(rest: []const u8) ?Vec3 {
    var values: [3]f32 = undefined;
    var n: usize = 0;
    var tokens = std.mem.tokenizeAny(u8, rest, " \t");
    while (tokens.next()) |tok| {
        if (n == 3) break;
        values[n] = std.fmt.parseFloat(f32, tok) catch return null;
        n += 1;
    }
    if (n != 3) return null;
    return .{ .x = values[0], .y = values[1], .z = values[2] };
}

/// Compute the bounds, and repair normals the file got wrong.
fn finish(triangles: []Triangle) Mesh {
    var min = Vec3{ .x = std.math.floatMax(f32), .y = std.math.floatMax(f32), .z = std.math.floatMax(f32) };
    var max = Vec3{ .x = -std.math.floatMax(f32), .y = -std.math.floatMax(f32), .z = -std.math.floatMax(f32) };

    for (triangles) |*t| {
        for (t.v) |p| {
            min = min.min(p);
            max = max.max(p);
        }
        // Plenty of exporters write a zero or non-unit normal. Fall back to the winding and
        // normalise whatever we end up with, so the shader can trust the field.
        if (t.normal.lengthSq() < 1e-12) t.normal = t.geometricNormal();
        t.normal = t.normal.normalize();
    }
    if (triangles.len == 0) {
        min = .{};
        max = .{};
    }
    return .{ .triangles = triangles, .min = min, .max = max };
}
