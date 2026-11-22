// The shader entry-point table and source composition.
//
// Shared verbatim between `build.rs`, which precompiles every entry point, and
// `src/shader.rs`, which can also compile them at runtime. It is pulled into
// both with `include!`, so it must stay plain items with no `use` and no
// crate-relative paths.

/// Every entry point as `(output stem, file, function, profile)`.
///
/// The order is the order of `shader::Entry::ALL`; the build script writes the
/// precompiled bytecode in exactly this order.
const SHADER_TABLE: [(&str, &str, &str, &str); 13] = [
    ("quad_vs", "quad.hlsl", "vs_main", "vs_5_0"),
    ("quad_ps", "quad.hlsl", "ps_main", "ps_5_0"),
    ("text_vs", "text.hlsl", "vs_main", "vs_5_0"),
    ("text_ps", "text.hlsl", "ps_main", "ps_5_0"),
    ("text_subpixel_ps", "text.hlsl", "ps_subpixel", "ps_5_0"),
    ("mesh_vs", "mesh.hlsl", "vs_main", "vs_5_0"),
    ("mesh_ps", "mesh.hlsl", "ps_main", "ps_5_0"),
    ("mesh_textured_ps", "mesh.hlsl", "ps_textured", "ps_5_0"),
    ("composite_vs", "composite.hlsl", "vs_main", "vs_5_0"),
    ("composite_ps", "composite.hlsl", "ps_main", "ps_5_0"),
    ("fullscreen_vs", "blur.hlsl", "vs_fullscreen", "vs_5_0"),
    ("blur_ps", "blur.hlsl", "ps_blur", "ps_5_0"),
    ("resolve_ps", "blur.hlsl", "ps_resolve", "ps_5_0"),
];

/// `D3DCOMPILE_OPTIMIZATION_LEVEL3 | D3DCOMPILE_ENABLE_STRICTNESS`.
///
/// Spelled as a literal so the build script needs no bindings; both callers
/// must use the same flags or precompiled and runtime-compiled bytecode would
/// differ.
const SHADER_COMPILE_FLAGS: u32 = 0x8000 | 0x0800;

/// The text handed to the compiler for one file: the common prelude, then the
/// file itself under a `#line` directive so diagnostics name the right place.
fn compose_shader_source(common: &str, file: &str, body: &str) -> String {
    let mut out = String::with_capacity(common.len() + body.len() + 64);
    out.push_str("#line 1 \"common.hlsl\"\n");
    out.push_str(common);
    out.push_str("\n#line 1 \"");
    out.push_str(file);
    out.push_str("\"\n");
    out.push_str(body);
    out
}
