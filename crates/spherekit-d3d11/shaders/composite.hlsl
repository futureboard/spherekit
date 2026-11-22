// Compositing an offscreen layer back into its parent. Port of the wgpu
// backend's `composite.wgsl`.
//
// Draws the layer's destination rectangle as one quad. Only layers that
// genuinely need group semantics — group opacity, a blend mode, a filter —
// ever reach this shader.
//
// `common.hlsl` is prepended by the Rust side; the pass uniforms in `b0` are
// the *destination* target's.

cbuffer CompositeParams : register(b1)
{
    /// Destination rectangle in scene-absolute logical pixels.
    float4 composite_dest;
    /// Opacity applied while compositing.
    float composite_opacity;
    /// Blend mode; see `spherekit_core::BlendMode`. Carried for parity with the
    /// wgpu backend, which also composites every mode as source-over.
    uint composite_blend;
    /// Saturation multiplier, 1.0 for no change.
    float composite_saturation;
    float composite_pad;
};

Texture2D layer_texture : register(t3);
SamplerState layer_sampler : register(s0);

struct CompositeOut
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD;
};

CompositeOut vs_main(uint vertex_id : SV_VertexID)
{
    float2 corner = float2(float(vertex_id & 1u), float((vertex_id >> 1u) & 1u));
    float2 local_pos = composite_dest.xy + corner * composite_dest.zw;
    float2 device_px = local_pos * frame_scale_factor - frame_target_origin * frame_scale_factor;

    CompositeOut o;
    o.position = to_clip_space(device_px);
    o.uv = corner;
    return o;
}

float4 ps_main(CompositeOut i) : SV_Target0
{
    float4 c = layer_texture.SampleLevel(layer_sampler, i.uv, 0.0);

    if (abs(composite_saturation - 1.0) > 1e-4) {
        // Luminance weights are applied to unpremultiplied colour, then the
        // result is re-premultiplied; desaturating premultiplied values
        // directly would drag the colour toward black as alpha falls.
        float a = max(c.a, 1e-5);
        float3 straight = c.rgb / a;
        float luma = dot(straight, float3(0.2126, 0.7152, 0.0722));
        float3 sat = lerp(float3(luma, luma, luma), straight, composite_saturation);
        c = float4(sat * a, c.a);
    }

    return c * composite_opacity;
}
