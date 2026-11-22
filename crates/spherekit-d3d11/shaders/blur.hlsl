// Separable Gaussian blur for layer filters and backdrop blur, plus the
// multisample resolve. Port of the wgpu backend's `blur.wgsl`.
//
// Two passes, horizontal then vertical: a separable kernel turns an O(r^2)
// convolution into O(r).
//
// `common.hlsl` is prepended by the Rust side.

cbuffer BlurParams : register(b2)
{
    /// Texel size of the source, `1.0 / dimensions`.
    float2 blur_texel;
    /// `(1, 0)` for the horizontal pass, `(0, 1)` for the vertical.
    float2 blur_direction;
    /// Gaussian standard deviation, in source texels.
    float blur_sigma;
    /// How many taps to take on each side of centre.
    int blur_radius;
    float2 blur_pad;
};

Texture2D src_texture : register(t3);
SamplerState src_sampler : register(s0);

struct BlurOut
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD;
};

/// A single oversized triangle covers the target with no vertex buffer and no
/// diagonal seam between two triangles.
BlurOut vs_fullscreen(uint vertex_id : SV_VertexID)
{
    float2 uv = float2(float((vertex_id << 1u) & 2u), float(vertex_id & 2u));
    BlurOut o;
    o.position = float4(uv * 2.0 - 1.0, 0.0, 1.0);
    o.uv = float2(uv.x, 1.0 - uv.y);
    return o;
}

float4 ps_blur(BlurOut i) : SV_Target0
{
    float sigma = max(blur_sigma, 1e-3);
    float inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);

    float4 total = src_texture.SampleLevel(src_sampler, i.uv, 0.0);
    float weight_sum = 1.0;

    // Symmetric taps, accumulated in pairs. The kernel is normalised at the end
    // so a truncated radius still integrates to one and edges do not darken.
    [loop]
    for (int t = 1; t <= blur_radius; t = t + 1) {
        float2 offset = float(t) * blur_texel * blur_direction;
        float w = exp(-float(t) * float(t) * inv_two_sigma_sq);
        total = total
            + src_texture.SampleLevel(src_sampler, i.uv + offset, 0.0) * w
            + src_texture.SampleLevel(src_sampler, i.uv - offset, 0.0) * w;
        weight_sum = weight_sum + 2.0 * w;
    }

    return total / weight_sum;
}

Texture2DMS<float4> msaa_source : register(t4);

/// Averages every sample of a multisampled target, in linear light.
///
/// `ResolveSubresource` cannot be used for the surface: the swap chain buffer
/// is `B8G8R8A8_UNORM` while the multisampled attachment is `_SRGB`, and two
/// differently typed formats may not be resolved into each other. Loading
/// through an sRGB view decodes each sample and the sRGB render target view
/// re-encodes the average, which is the linear-space resolve wgpu performs.
float4 ps_resolve(BlurOut i) : SV_Target0
{
    uint w;
    uint h;
    uint samples;
    msaa_source.GetDimensions(w, h, samples);
    int2 texel = int2(i.position.xy);
    float4 sum = float4(0.0, 0.0, 0.0, 0.0);
    [loop]
    for (uint s = 0u; s < samples; s = s + 1u) {
        sum = sum + msaa_source.Load(texel, s);
    }
    return sum / float(max(samples, 1u));
}
