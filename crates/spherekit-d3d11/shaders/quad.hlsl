// The quad pipeline: rectangles, rounded rectangles, borders, gradients,
// textured quads and box shadows. Port of the wgpu backend's `quad.wgsl`.
//
// One pipeline covers all of them because they share a shape (a rectangle with
// four radii) and differ only in how the fragment is coloured. Splitting them
// would multiply pipeline switches across a frame that is overwhelmingly made
// of exactly this primitive.
//
// `common.hlsl` is prepended by the Rust side.

// Keep in sync with `spherekit_render::primitives::quad_flags`.
#define FLAG_FILL_SOLID 1u
#define FLAG_FILL_GRADIENT 2u
#define FLAG_FILL_TEXTURE 4u
#define FLAG_SHADOW 8u
#define FLAG_SHADOW_INSET 16u
#define FLAG_CLIP_ROUNDED 32u
#define FLAG_BORDER 64u

Texture2D quad_texture : register(t3);
SamplerState quad_sampler : register(s0);

struct QuadIn
{
    /// `[x, y, w, h]` in local logical pixels.
    float4 bounds : BOUNDS;
    /// `[tl, tr, br, bl]` radii in logical pixels.
    float4 radii : RADII;
    /// Linear premultiplied fill or shadow colour.
    float4 color : COLOR;
    /// Border colour, or the source rectangle for textured quads.
    float4 border_color : BORDER;
    /// `[border_width, blur_sigma]`.
    float2 params : PARAMS;
    /// `[flags, gradient, transform_index, clip_index]`.
    uint4 indices : INDICES;
    uint vertex_id : SV_VertexID;
};

struct QuadOut
{
    float4 position : SV_Position;
    /// Position within the quad's local logical space.
    float2 local_pos : LOCAL;
    /// Scene-absolute logical position, which is where clips are defined.
    ///
    /// The wgpu backend passed the pre-transform local position here, which
    /// put every rounded clip in the wrong place under any non-identity
    /// transform. Not ported.
    float2 world : WORLD;
    nointerpolation float4 bounds : BOUNDS;
    nointerpolation float4 radii : RADII;
    nointerpolation float4 color : COLOR;
    nointerpolation float4 border_color : BORDER;
    nointerpolation float2 params : PARAMS;
    nointerpolation uint4 indices : INDICES;
};

QuadOut vs_main(QuadIn inst)
{
    // A triangle strip over the unit square. Generating corners from the vertex
    // index means no vertex buffer at all for the most common primitive.
    float2 corner = float2(float(inst.vertex_id & 1u), float((inst.vertex_id >> 1u) & 1u));

    uint flags = inst.indices.x;
    // A shadow's blurred footprint extends beyond its shape, so the quad has to
    // be grown or the tail would be clipped by its own geometry. Everything
    // else still needs a pixel for its antialiased edge.
    float pad = 1.0;
    if ((flags & FLAG_SHADOW) != 0u) {
        pad = inst.params.y * 3.0 + 1.0;
    }

    float2 origin = inst.bounds.xy - float2(pad, pad);
    float2 extent = inst.bounds.zw + float2(pad * 2.0, pad * 2.0);
    float2 local_pos = origin + corner * extent;

    QuadOut o;
    o.position = to_clip_space(apply_transform(inst.indices.z, local_pos));
    o.local_pos = local_pos;
    o.world = scene_position(inst.indices.z, local_pos);
    o.bounds = inst.bounds;
    o.radii = inst.radii;
    o.color = inst.color;
    o.border_color = inst.border_color;
    o.params = inst.params;
    o.indices = inst.indices;
    return o;
}

float4 ps_main(QuadOut i) : SV_Target0
{
    uint flags = i.indices.x;
    float2 center = i.bounds.xy + i.bounds.zw * 0.5;
    float2 half_extent = max(i.bounds.zw * 0.5, float2(0.0, 0.0));
    float2 p = i.local_pos - center;

    float4 color = i.color;
    float coverage;

    if ((flags & FLAG_SHADOW) != 0u) {
        float blur = shadow_coverage(half_extent, i.radii, p, max(i.params.y, 1e-3));
        if ((flags & FLAG_SHADOW_INSET) != 0u) {
            // An inset shadow is the complement of the blur, confined to the
            // shape itself.
            float inside = coverage_from_distance(sd_rounded_box(p, half_extent, i.radii));
            coverage = (1.0 - blur) * inside;
        } else {
            coverage = blur;
        }
    } else {
        float d = sd_rounded_box(p, half_extent, i.radii);
        coverage = coverage_from_distance(d);

        if ((flags & FLAG_FILL_GRADIENT) != 0u) {
            color = sample_gradient(i.indices.y, i.local_pos);
        } else if ((flags & FLAG_FILL_TEXTURE) != 0u) {
            // The source rectangle rides in the border-colour slot for
            // textured quads, which keeps the instance at 96 bytes.
            float2 uv01 = clamp((i.local_pos - i.bounds.xy) / max(i.bounds.zw, float2(1e-4, 1e-4)),
                                float2(0.0, 0.0), float2(1.0, 1.0));
            float2 uv = i.border_color.xy + uv01 * i.border_color.zw;
            // Level 0 explicitly: every texture has a single mip, so this is
            // what an implicit-derivative sample resolves to anyway, and it is
            // legal inside this per-instance branch.
            float4 texel = quad_texture.SampleLevel(quad_sampler, uv, 0.0);
            color = texel * i.color;
        } else if ((flags & FLAG_FILL_SOLID) == 0u) {
            color = float4(0.0, 0.0, 0.0, 0.0);
        }

        if ((flags & FLAG_BORDER) != 0u) {
            float w = max(i.params.x, 0.0);
            // The inner edge is the same rounded box shrunk by the border
            // width; radii shrink with it so the corner stays concentric.
            float2 inner_half = max(half_extent - float2(w, w), float2(0.0, 0.0));
            float4 inner_radii = max(i.radii - float4(w, w, w, w), float4(0.0, 0.0, 0.0, 0.0));
            float inner_d = sd_rounded_box(p, inner_half, inner_radii);
            float inner_cov = coverage_from_distance(inner_d);
            float ring = max(coverage - inner_cov, 0.0);
            // Both colours are premultiplied, so compositing the fill under the
            // border is a plain source-over.
            float4 fill = color * inner_cov;
            float4 border = i.border_color * ring;
            return (border + fill * (1.0 - i.border_color.a * ring))
                * clip_coverage(i.indices.w, i.world);
        }
    }

    return color * coverage * clip_coverage(i.indices.w, i.world);
}
