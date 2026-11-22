// The mesh pipeline: tessellated paths and realtime audio geometry.
//
// Mesh vertices carry their transform baked in and their colour per vertex, so
// this pipeline is deliberately thin. Port of the wgpu backend's `mesh.wgsl`.
//
// `common.hlsl` is prepended by the Rust side.

Texture2D mesh_texture : register(t3);
SamplerState mesh_sampler : register(s0);

struct MeshIn
{
    /// Position in scene-absolute logical pixels; the transform is already baked.
    float2 position : POSITION;
    /// Texture coordinate, ignored when the batch has no texture.
    float2 uv : TEXCOORD;
    /// Linear premultiplied colour.
    float4 color : COLOR;
};

struct MeshOut
{
    float4 position : SV_Position;
    float2 uv : TEXCOORD;
    float4 color : COLOR;
};

MeshOut vs_main(MeshIn v)
{
    MeshOut o;
    float2 device_px = v.position * frame_scale_factor - frame_target_origin * frame_scale_factor;
    o.position = to_clip_space(device_px);
    o.uv = v.uv;
    o.color = v.color;
    return o;
}

float4 ps_main(MeshOut i) : SV_Target0
{
    return i.color;
}

float4 ps_textured(MeshOut i) : SV_Target0
{
    return mesh_texture.SampleLevel(mesh_sampler, i.uv, 0.0) * i.color;
}
