//! A small software rasteriser: triangles in, pixels out.
//!
//! Ported from letim, Apache-2.0 as of commit 15d2338 (`src/raster.zig`). Edge functions for the
//! barycentric coverage test, and a depth buffer holding reciprocal depth so the test is correct
//! under perspective. Normals are flat shaded per facet, which is what STL suits: the format has
//! no vertex sharing, so there is nothing smoother to interpolate anyway.
//!
//! The one change is ownership: letim's target allocates its depth buffer, and this one is handed
//! both buffers by the caller, because a plugin has no allocator beyond its fixed arena.

const std = @import("std");
const math3d = @import("math3d.zig");
const Vec3 = math3d.Vec3;
const Projected = math3d.Projected;

/// Triangles whose vertices fall this close to the eye are dropped rather than divided by. Proper
/// near-plane clipping would keep them, but a viewer of a centred model only ever sees this when
/// zoomed inside it.
pub const near_plane: f32 = 0.05;

pub const Shade = struct {
    /// Colour at full lighting, 0..1 per channel.
    base: [3]f32 = .{ 0.78, 0.80, 0.86 },
    ambient: f32 = 0.16,
    diffuse: f32 = 0.90,
    /// Key light, in view space, as a direction from the surface *toward* the light. The camera
    /// sits at the origin looking down +z, so a headlight points back at -z, which means a facet
    /// facing the viewer is lit.
    key: Vec3 = .{ .x = -0.40, .y = 0.50, .z = -0.77 },
    /// Fill from below and behind, so the far side is dim rather than black.
    fill: f32 = 0.16,

    /// Flat shade one facet. `normal` must already be in view space.
    pub fn colour(self: Shade, normal: Vec3) [3]u8 {
        const n = normal.normalize();
        const k = self.ambient +
            self.diffuse * @max(0, n.dot(self.key.normalize())) +
            self.fill * @max(0, n.dot(fillDir()));
        return .{ byte(self.base[0] * k), byte(self.base[1] * k), byte(self.base[2] * k) };
    }

    fn fillDir() Vec3 {
        return Vec3.init(0.35, -0.35, 0.87);
    }
};

fn byte(v: f32) u8 {
    if (v <= 0) return 0;
    if (v >= 1) return 255;
    return @intFromFloat(v * 255.0);
}

/// A colour buffer to draw into, plus the depth buffer that goes with it. Both belong to the
/// caller: the plugin's static memory, sized once.
pub const Target = struct {
    width: u32 = 0,
    height: u32 = 0,
    colour: []u8 = &.{},
    depth: []f32 = &.{},

    /// Point at a pair of buffers and a size. The buffers must hold `width*height` pixels
    /// (`*3` bytes for the colour); the caller has already checked that.
    pub fn aim(self: *Target, width: u32, height: u32, colour: []u8, depth: []f32) void {
        self.width = @max(1, width);
        self.height = @max(1, height);
        self.colour = colour;
        self.depth = depth;
    }

    /// Fill with a background colour and clear the depth buffer.
    pub fn clear(self: *Target, rgb: [3]u8) void {
        var i: usize = 0;
        while (i + 2 < self.colour.len) : (i += 3) {
            self.colour[i] = rgb[0];
            self.colour[i + 1] = rgb[1];
            self.colour[i + 2] = rgb[2];
        }
        // Zero is the "nothing here yet" value: reciprocal depth is always positive in front of
        // the eye.
        @memset(self.depth, 0);
    }

    /// Draw one facet, flat shaded. Triangles with any vertex at or behind the near plane are
    /// dropped; the rest get a barycentric coverage test and a reciprocal depth test.
    pub fn triangle(self: *Target, a: Projected, b: Projected, c: Projected, shade: Shade, normal: Vec3) void {
        if (a.view_z < near_plane or b.view_z < near_plane or c.view_z < near_plane) return;

        const fw: f32 = @floatFromInt(self.width);
        const fh: f32 = @floatFromInt(self.height);

        const lo_x = @max(0, @floor(@min(a.x, @min(b.x, c.x))));
        const hi_x = @min(fw - 1, @ceil(@max(a.x, @max(b.x, c.x))));
        const lo_y = @max(0, @floor(@min(a.y, @min(b.y, c.y))));
        const hi_y = @min(fh - 1, @ceil(@max(a.y, @max(b.y, c.y))));
        if (hi_x < lo_x or hi_y < lo_y) return;

        const area = edge(a, b, c.x, c.y);
        if (area == 0) return;
        const inv_area = 1.0 / area;

        const rgb = shade.colour(normal);
        const x0: u32 = @intFromFloat(lo_x);
        const x1: u32 = @intFromFloat(hi_x);
        const y0: u32 = @intFromFloat(lo_y);
        const y1: u32 = @intFromFloat(hi_y);

        var y = y0;
        while (y <= y1) : (y += 1) {
            const py = @as(f32, @floatFromInt(y)) + 0.5;
            const row = @as(usize, y) * self.width;
            var x = x0;
            while (x <= x1) : (x += 1) {
                const px = @as(f32, @floatFromInt(x)) + 0.5;

                const w0 = edge(b, c, px, py);
                const w1 = edge(c, a, px, py);
                const w2 = edge(a, b, px, py);
                if (area > 0) {
                    if (w0 < 0 or w1 < 0 or w2 < 0) continue;
                } else {
                    if (w0 > 0 or w1 > 0 or w2 > 0) continue;
                }

                const l0 = w0 * inv_area;
                const l1 = w1 * inv_area;
                const l2 = w2 * inv_area;
                const inv_w = l0 * a.inv_w + l1 * b.inv_w + l2 * c.inv_w;

                const idx = row + x;
                if (inv_w <= self.depth[idx]) continue;
                self.depth[idx] = inv_w;

                const off = idx * 3;
                self.colour[off] = rgb[0];
                self.colour[off + 1] = rgb[1];
                self.colour[off + 2] = rgb[2];
            }
        }
    }
};

/// Twice the signed area of the triangle (a, b, p). The sign says which side of the edge p falls
/// on.
fn edge(a: Projected, b: Projected, px: f32, py: f32) f32 {
    return (b.x - a.x) * (py - a.y) - (b.y - a.y) * (px - a.x);
}
