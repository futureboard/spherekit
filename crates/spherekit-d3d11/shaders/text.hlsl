// The text pipelines: coverage-bitmap glyphs, grayscale and RGB subpixel.
//
// Every glyph is rasterised at its exact device size and the batch compiler
// snaps its quad to the device grid, so there is no distance reconstruction
// and no size-dependent ramp here: the vertex stage expands the quad and lerps
// the UVs, and the pixel stage samples coverage, bends it for the linear-light
// blend, and multiplies by the colour.
//
// Both entry points live in one file. Unlike WGSL, merely declaring a
// dual-source output in HLSL needs no device feature, and every Direct3D 11
// device this backend accepts supports dual-source blending anyway.
//
// `common.hlsl` is prepended by the Rust side.

// Keep in sync with `spherekit_render::primitives::glyph_flags`. Neither flag
// changes what the shader does: the clip is evaluated from its own radii, and
// the subpixel choice is made per batch by picking `ps_subpixel`.
#define FLAG_CLIP_ROUNDED 1u
#define FLAG_SUBPIXEL 2u

Texture2D glyph_atlas : register(t3);
SamplerState glyph_sampler : register(s0);

/// Bends the coverage ramp so the linear blend and the sRGB encode downstream
/// reproduce a gamma-space blend.
///
/// Copied verbatim from the wgpu backend's `text.wgsl`, and the CPU twin of
/// `spherekit_render::scene::alpha_from_coverage`; none of the three may drift.
/// `contrast` carries both the strength and the direction: positive steepens
/// the low end (light text on dark), negative mirrors it (dark text on light).
float apply_coverage_contrast(float coverage, float contrast)
{
    float c = clamp(coverage, 0.0, 1.0);
    float k = abs(contrast);
    if (k <= 1.0) {
        return c;
    }
    if (contrast > 0.0) {
        return pow(c, k);
    }
    return 1.0 - pow(1.0 - c, k);
}

struct GlyphIn
{
    /// `[x, y, w, h]` of the glyph quad in local logical pixels.
    float4 bounds : BOUNDS;
    /// `[u0, v0, u1, v1]` in normalised atlas coordinates.
    float4 uv : UV;
    /// Linear premultiplied fill colour.
    float4 color : COLOR;
    /// Signed coverage exponent; see `apply_coverage_contrast`.
    float coverage_contrast : CONTRAST;
    /// `[flags, transform_index, clip_index]`.
    uint3 indices : INDICES;
    uint vertex_id : SV_VertexID;
};

struct GlyphOut
{
    float4 position : SV_Position;
    float2 uv : UV;
    /// Scene-absolute logical position, where clips are defined.
    float2 world : WORLD;
    nointerpolation float4 color : COLOR;
    nointerpolation float coverage_contrast : CONTRAST;
    nointerpolation uint clip_index : CLIP;
};

GlyphOut vs_main(GlyphIn inst)
{
    float2 corner = float2(float(inst.vertex_id & 1u), float((inst.vertex_id >> 1u) & 1u));
    float2 local_pos = inst.bounds.xy + corner * inst.bounds.zw;

    GlyphOut o;
    o.position = to_clip_space(apply_transform(inst.indices.y, local_pos));
    o.uv = lerp(inst.uv.xy, inst.uv.zw, corner);
    o.world = scene_position(inst.indices.y, local_pos);
    o.color = inst.color;
    o.coverage_contrast = inst.coverage_contrast;
    o.clip_index = inst.indices.z;
    return o;
}

float4 ps_main(GlyphOut i) : SV_Target0
{
    float clip = clip_coverage(i.clip_index, i.world);
    // Grayscale pages replicate coverage into rgb; red is the one to read.
    float4 s = glyph_atlas.SampleLevel(glyph_sampler, i.uv, 0.0);
    return i.color * apply_coverage_contrast(s.r, i.coverage_contrast) * clip;
}

struct SubpixelOut
{
    /// Premultiplied foreground: blend source zero.
    float4 foreground : SV_Target0;
    /// Per-channel coverage: blend source one, consumed by `INV_SRC1_COLOR`.
    float4 coverage : SV_Target1;
};

/// RGB subpixel text. The blend state computes `src0 + dst * (1 - src1)`
/// independently per channel, so each LCD stripe gets its own coverage.
SubpixelOut ps_subpixel(GlyphOut i)
{
    float clip = clip_coverage(i.clip_index, i.world);
    float4 s = glyph_atlas.SampleLevel(glyph_sampler, i.uv, 0.0);
    float3 coverage_rgb = float3(
        apply_coverage_contrast(s.r, i.coverage_contrast),
        apply_coverage_contrast(s.g, i.coverage_contrast),
        apply_coverage_contrast(s.b, i.coverage_contrast)) * clip;

    // The atlas alpha channel is max(R, G, B), a conservative scalar coverage
    // for callers that inspect the destination alpha. The same transfer curve
    // keeps source zero and source one on one coverage scale.
    float coverage_alpha = apply_coverage_contrast(s.a, i.coverage_contrast) * clip;

    SubpixelOut o;
    o.foreground = float4(i.color.rgb * coverage_rgb, i.color.a * coverage_alpha);
    o.coverage = float4(i.color.a * coverage_rgb, i.color.a * coverage_alpha);
    return o;
}
