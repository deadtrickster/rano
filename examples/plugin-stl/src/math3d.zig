//! Just enough 3D maths for a mesh viewer: vectors, an orbit, and the camera transform.
//!
//! Ported from letim, Apache-2.0 as of commit 15d2338 (`src/math3d.zig`). The changes are the
//! ones a fixed-buffer plugin needs: no allocator, and no unit tests here because the wasm is
//! exercised end to end by `src/plugin.rs`'s tests instead.

const std = @import("std");

pub const Vec3 = struct {
    x: f32 = 0,
    y: f32 = 0,
    z: f32 = 0,

    pub fn init(x: f32, y: f32, z: f32) Vec3 {
        return .{ .x = x, .y = y, .z = z };
    }

    pub fn add(a: Vec3, b: Vec3) Vec3 {
        return .{ .x = a.x + b.x, .y = a.y + b.y, .z = a.z + b.z };
    }

    pub fn sub(a: Vec3, b: Vec3) Vec3 {
        return .{ .x = a.x - b.x, .y = a.y - b.y, .z = a.z - b.z };
    }

    pub fn scale(a: Vec3, s: f32) Vec3 {
        return .{ .x = a.x * s, .y = a.y * s, .z = a.z * s };
    }

    pub fn dot(a: Vec3, b: Vec3) f32 {
        return a.x * b.x + a.y * b.y + a.z * b.z;
    }

    pub fn cross(a: Vec3, b: Vec3) Vec3 {
        return .{
            .x = a.y * b.z - a.z * b.y,
            .y = a.z * b.x - a.x * b.z,
            .z = a.x * b.y - a.y * b.x,
        };
    }

    pub fn lengthSq(a: Vec3) f32 {
        return a.dot(a);
    }

    pub fn length(a: Vec3) f32 {
        return @sqrt(a.lengthSq());
    }

    pub fn normalize(a: Vec3) Vec3 {
        const len = a.length();
        if (len <= 1e-20) return .{};
        return a.scale(1.0 / len);
    }

    pub fn min(a: Vec3, b: Vec3) Vec3 {
        return .{ .x = @min(a.x, b.x), .y = @min(a.y, b.y), .z = @min(a.z, b.z) };
    }

    pub fn max(a: Vec3, b: Vec3) Vec3 {
        return .{ .x = @max(a.x, b.x), .y = @max(a.y, b.y), .z = @max(a.z, b.z) };
    }
};

/// Row major matrix: `m[row][col]`.
pub const Mat3 = struct {
    m: [3][3]f32,

    pub fn mulVec(a: Mat3, v: Vec3) Vec3 {
        return .{
            .x = a.m[0][0] * v.x + a.m[0][1] * v.y + a.m[0][2] * v.z,
            .y = a.m[1][0] * v.x + a.m[1][1] * v.y + a.m[1][2] * v.z,
            .z = a.m[2][0] * v.x + a.m[2][1] * v.y + a.m[2][2] * v.z,
        };
    }
};

/// A rotation kept as the angle pair a mouse drag produces: yaw and pitch, no roll to preserve,
/// nothing to drift.
pub const Orbit = struct {
    yaw: f32 = 0,
    pitch: f32 = 0,

    pub const max_pitch: f32 = 1.5533; // a shade under a right angle

    pub fn matrix(self: Orbit) Mat3 {
        const cy = @cos(self.yaw);
        const sy = @sin(self.yaw);
        const cp = @cos(self.pitch);
        const sp = @sin(self.pitch);
        // Pitch first, then yaw: vertical drag tips the model toward you whatever the yaw is.
        return .{ .m = .{
            .{ cy, sy * sp, -sy * cp },
            .{ 0, cp, sp },
            .{ sy, -cy * sp, cy * cp },
        } };
    }
};

/// A vertex after transformation: where it lands, how far away it is.
pub const Projected = struct {
    x: f32,
    y: f32,
    /// Reciprocal depth: interpolating 1/w across a triangle is what makes the depth test correct
    /// under perspective, because w itself is not linear in screen space.
    inv_w: f32,
    /// View-space z, used for the near-plane test.
    view_z: f32,
};

pub const Camera = struct {
    /// How far the eye stands off the model, in model radii.
    distance: f32 = 3.2,
    /// Vertical field of view, radians.
    fov: f32 = 0.9,
    /// Screen centre, in pixels.
    cx: f32 = 0,
    cy: f32 = 0,
    /// Distance from the eye to the projection plane, in pixels.
    focal: f32 = 1,
    width: u32 = 1,
    height: u32 = 1,

    pub fn forViewport(width: u32, height: u32, fov: f32) Camera {
        const w: f32 = @floatFromInt(@max(1, width));
        const h: f32 = @floatFromInt(@max(1, height));
        return .{
            .fov = fov,
            .width = @max(1, width),
            .height = @max(1, height),
            .cx = w * 0.5,
            .cy = h * 0.5,
            // Fit the vertical extent; a wide viewport then shows more to the sides.
            .focal = (h * 0.5) / @tan(fov * 0.5),
        };
    }

    /// Model space to screen space, for a point already centred and scaled to unit radius.
    pub fn project(self: Camera, rotation: Mat3, p: Vec3) Projected {
        const r = rotation.mulVec(p);
        const view_z = r.z + self.distance;
        return .{
            .x = self.cx + r.x * self.focal / view_z,
            // Screen y grows downward.
            .y = self.cy - r.y * self.focal / view_z,
            .inv_w = 1.0 / view_z,
            .view_z = view_z,
        };
    }
};
