const std = @import("std");

/// Builds `plugin.wasm`: a freestanding wasm32 module with no entry point and no WASI, whose
/// exports (`rano_abi`, `rano_alloc`, `provider_picture`, `provider_open`, `memory`) are exactly
/// what the rano ABI names.
///
/// ```sh
/// zig build -Doptimize=ReleaseSmall
/// cp zig-out/bin/plugin.wasm plugin.wasm
/// ```
pub fn build(b: *std.Build) void {
    const exe = b.addExecutable(.{
        .name = "plugin",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/main.zig"),
            .target = b.resolveTargetQuery(.{
                .cpu_arch = .wasm32,
                .os_tag = .freestanding,
            }),
            // `-Doptimize=Debug` when you want the safety checks to name a line; the default is
            // the size the committed `plugin.wasm` was built at.
            .optimize = b.option(std.builtin.OptimizeMode, "optimize", "optimisation mode") orelse .ReleaseSmall,
        }),
    });
    // A plugin is a library of exports, not a program: no `_start`, and every `export fn` visible
    // in the module's export section rather than garbage collected.
    exe.entry = .disabled;
    exe.rdynamic = true;
    b.installArtifact(exe);

    // `zig build fixtures` regenerates the binary fixture from the ASCII one, through the
    // plugin's own parser. It runs natively; it is not part of the wasm.
    const make = b.addExecutable(.{
        .name = "make_binary",
        .root_module = b.createModule(.{
            .root_source_file = b.path("make_binary.zig"),
            .target = b.resolveTargetQuery(.{}),
            .optimize = .Debug,
            .link_libc = true,
        }),
    });
    const run = b.addRunArtifact(make);
    run.setCwd(b.path("."));
    const fixtures = b.step("fixtures", "Regenerate fixtures/cube-binary.stl from fixtures/cube.stl");
    fixtures.dependOn(&run.step);
}
