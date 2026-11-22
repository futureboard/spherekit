//! Shader objects, input layouts and fixed-function state.
//!
//! Direct3D 11 has no pipeline objects; a "pipeline" here is the combination
//! of an input layout, a topology, a vertex and pixel shader and a blend state.
//! Everything is created once, at device creation, from bytecode that was
//! compiled at build time (see [`crate::shader`]). Nothing is built lazily: the
//! complete set is a few kilobytes, and creating it up front means the render
//! path cannot fail on a missing state object.

use crate::shader::{CompiledShaders, Entry};
use core::mem::offset_of;
use spherekit_core::{InitError, ShaderError};
use spherekit_render::{GlyphInstance, MeshVertex, QuadInstance};
use windows::Win32::Foundation::TRUE;
use windows::Win32::Graphics::Direct3D::{
    D3D_PRIMITIVE_TOPOLOGY, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
    D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BLEND, D3D11_BLEND_DESC, D3D11_BLEND_INV_SRC_ALPHA, D3D11_BLEND_INV_SRC1_ALPHA,
    D3D11_BLEND_INV_SRC1_COLOR, D3D11_BLEND_ONE, D3D11_BLEND_OP_ADD, D3D11_BLEND_ZERO,
    D3D11_COLOR_WRITE_ENABLE_ALL, D3D11_COMPARISON_NEVER, D3D11_CULL_NONE, D3D11_FILL_SOLID,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_FLOAT32_MAX, D3D11_INPUT_CLASSIFICATION,
    D3D11_INPUT_ELEMENT_DESC, D3D11_INPUT_PER_INSTANCE_DATA, D3D11_INPUT_PER_VERTEX_DATA,
    D3D11_RASTERIZER_DESC, D3D11_RENDER_TARGET_BLEND_DESC, D3D11_SAMPLER_DESC,
    D3D11_TEXTURE_ADDRESS_CLAMP, ID3D11BlendState, ID3D11Device, ID3D11DeviceContext,
    ID3D11InputLayout, ID3D11PixelShader, ID3D11RasterizerState, ID3D11SamplerState,
    ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_R32G32_FLOAT, DXGI_FORMAT_R32G32B32_UINT,
    DXGI_FORMAT_R32G32B32A32_FLOAT, DXGI_FORMAT_R32G32B32A32_UINT,
};
use windows::core::{PCSTR, s};

/// Which pipeline a draw needs. Mirrors the wgpu backend's `PipelineKind`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PipelineKind {
    /// Rectangles, rounded rectangles, borders, gradients and shadows.
    Quad,
    /// Textured quads. Same shaders as `Quad`, with a real texture bound.
    Image,
    /// Grayscale coverage glyphs.
    Text,
    /// RGB-coverage glyphs blended with a second pixel-shader output.
    TextSubpixel,
    /// Vertex-coloured triangles.
    Mesh,
    /// Textured triangles.
    MeshTextured,
    /// Layer compositing.
    Composite,
    /// One separable blur pass.
    Blur,
    /// The multisample resolve of the surface.
    Resolve,
}

/// How a pipeline blends into its target.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Blend {
    /// Premultiplied source-over: `src + dst * (1 - src.a)`.
    Premultiplied,
    /// Dual-source subpixel text: `src0 + dst * (1 - src1)` per channel.
    Subpixel,
    /// No blending; the output replaces the target.
    Replace,
}

/// Every shader and state object the renderer uses.
pub(crate) struct Pipelines {
    quad_vs: ID3D11VertexShader,
    quad_ps: ID3D11PixelShader,
    quad_layout: ID3D11InputLayout,
    text_vs: ID3D11VertexShader,
    text_ps: ID3D11PixelShader,
    text_subpixel_ps: ID3D11PixelShader,
    glyph_layout: ID3D11InputLayout,
    mesh_vs: ID3D11VertexShader,
    mesh_ps: ID3D11PixelShader,
    mesh_textured_ps: ID3D11PixelShader,
    mesh_layout: ID3D11InputLayout,
    composite_vs: ID3D11VertexShader,
    composite_ps: ID3D11PixelShader,
    fullscreen_vs: ID3D11VertexShader,
    blur_ps: ID3D11PixelShader,
    resolve_ps: ID3D11PixelShader,
    premultiplied: ID3D11BlendState,
    subpixel: ID3D11BlendState,
    replace: ID3D11BlendState,
    /// Scissor on, culling off. UI geometry can legitimately be wound either
    /// way, particularly after a mirroring transform.
    pub rasterizer: ID3D11RasterizerState,
    /// Bilinear, clamp-to-edge; the one sampler every pipeline uses.
    pub linear_sampler: ID3D11SamplerState,
}

/// Vertex attribute layout for [`QuadInstance`]. Offsets come from the Rust
/// struct itself, so a field reorder cannot silently garble geometry.
pub(crate) fn quad_elements() -> [D3D11_INPUT_ELEMENT_DESC; 6] {
    let inst = D3D11_INPUT_PER_INSTANCE_DATA;
    [
        element(
            s!("BOUNDS"),
            DXGI_FORMAT_R32G32B32A32_FLOAT,
            offset_of!(QuadInstance, bounds),
            inst,
        ),
        element(s!("RADII"), DXGI_FORMAT_R32G32B32A32_FLOAT, offset_of!(QuadInstance, radii), inst),
        element(s!("COLOR"), DXGI_FORMAT_R32G32B32A32_FLOAT, offset_of!(QuadInstance, color), inst),
        element(
            s!("BORDER"),
            DXGI_FORMAT_R32G32B32A32_FLOAT,
            offset_of!(QuadInstance, border_color),
            inst,
        ),
        // `border_width`, `blur_sigma`.
        element(
            s!("PARAMS"),
            DXGI_FORMAT_R32G32_FLOAT,
            offset_of!(QuadInstance, border_width),
            inst,
        ),
        // `flags`, `gradient`, `transform_index`, `clip_index`.
        element(
            s!("INDICES"),
            DXGI_FORMAT_R32G32B32A32_UINT,
            offset_of!(QuadInstance, flags),
            inst,
        ),
    ]
}

/// Vertex attribute layout for [`GlyphInstance`].
pub(crate) fn glyph_elements() -> [D3D11_INPUT_ELEMENT_DESC; 5] {
    let inst = D3D11_INPUT_PER_INSTANCE_DATA;
    [
        element(
            s!("BOUNDS"),
            DXGI_FORMAT_R32G32B32A32_FLOAT,
            offset_of!(GlyphInstance, bounds),
            inst,
        ),
        element(s!("UV"), DXGI_FORMAT_R32G32B32A32_FLOAT, offset_of!(GlyphInstance, uv), inst),
        element(
            s!("COLOR"),
            DXGI_FORMAT_R32G32B32A32_FLOAT,
            offset_of!(GlyphInstance, color),
            inst,
        ),
        element(
            s!("CONTRAST"),
            DXGI_FORMAT_R32_FLOAT,
            offset_of!(GlyphInstance, coverage_contrast),
            inst,
        ),
        // `flags`, `transform_index`, `clip_index`.
        element(s!("INDICES"), DXGI_FORMAT_R32G32B32_UINT, offset_of!(GlyphInstance, flags), inst),
    ]
}

/// Vertex attribute layout for [`MeshVertex`].
pub(crate) fn mesh_elements() -> [D3D11_INPUT_ELEMENT_DESC; 3] {
    let vert = D3D11_INPUT_PER_VERTEX_DATA;
    [
        element(s!("POSITION"), DXGI_FORMAT_R32G32_FLOAT, offset_of!(MeshVertex, position), vert),
        element(s!("TEXCOORD"), DXGI_FORMAT_R32G32_FLOAT, offset_of!(MeshVertex, uv), vert),
        element(s!("COLOR"), DXGI_FORMAT_R32G32B32A32_FLOAT, offset_of!(MeshVertex, color), vert),
    ]
}

fn element(
    name: PCSTR,
    format: DXGI_FORMAT,
    offset: usize,
    class: D3D11_INPUT_CLASSIFICATION,
) -> D3D11_INPUT_ELEMENT_DESC {
    D3D11_INPUT_ELEMENT_DESC {
        SemanticName: name,
        SemanticIndex: 0,
        Format: format,
        InputSlot: 0,
        AlignedByteOffset: offset as u32,
        InputSlotClass: class,
        InstanceDataStepRate: if class == D3D11_INPUT_PER_INSTANCE_DATA { 1 } else { 0 },
    }
}

fn shader_error(name: &str, e: windows::core::Error) -> InitError {
    InitError::Shader(ShaderError { name: name.into(), message: e.to_string() })
}

impl Pipelines {
    /// Creates every shader and state object.
    pub fn new(device: &ID3D11Device, shaders: &CompiledShaders) -> Result<Self, InitError> {
        let vs = |entry: Entry| -> Result<ID3D11VertexShader, InitError> {
            let mut out = None;
            // SAFETY: the bytecode is a complete DXBC blob from FXC.
            unsafe { device.CreateVertexShader(shaders.get(entry), None, Some(&mut out)) }
                .map_err(|e| shader_error(&format!("{entry:?}"), e))?;
            Ok(out.expect("CreateVertexShader succeeded"))
        };
        let ps = |entry: Entry| -> Result<ID3D11PixelShader, InitError> {
            let mut out = None;
            // SAFETY: as above.
            unsafe { device.CreatePixelShader(shaders.get(entry), None, Some(&mut out)) }
                .map_err(|e| shader_error(&format!("{entry:?}"), e))?;
            Ok(out.expect("CreatePixelShader succeeded"))
        };
        let layout = |elements: &[D3D11_INPUT_ELEMENT_DESC], entry: Entry| {
            let mut out = None;
            // SAFETY: the semantic names are static C strings and the bytecode
            // carries the input signature the layout is validated against.
            unsafe { device.CreateInputLayout(elements, shaders.get(entry), Some(&mut out)) }
                .map_err(|e| shader_error(&format!("{entry:?} input layout"), e))?;
            Ok::<_, InitError>(out.expect("CreateInputLayout succeeded"))
        };

        let state_error = |what: &str, e: windows::core::Error| {
            InitError::DeviceCreation(format!("creating the {what} state: {e}"))
        };
        let blend = |kind: Blend| {
            let mut out = None;
            let desc = blend_desc(kind);
            // SAFETY: `desc` is a valid descriptor.
            unsafe { device.CreateBlendState(&desc, Some(&mut out)) }
                .map_err(|e| state_error("blend", e))?;
            Ok::<_, InitError>(out.expect("CreateBlendState succeeded"))
        };

        let rasterizer_desc = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            FrontCounterClockwise: false.into(),
            DepthBias: 0,
            DepthBiasClamp: 0.0,
            SlopeScaledDepthBias: 0.0,
            DepthClipEnable: TRUE,
            ScissorEnable: TRUE,
            MultisampleEnable: TRUE,
            AntialiasedLineEnable: false.into(),
        };
        let mut rasterizer = None;
        // SAFETY: `rasterizer_desc` is a valid descriptor.
        unsafe { device.CreateRasterizerState(&rasterizer_desc, Some(&mut rasterizer)) }
            .map_err(|e| state_error("rasterizer", e))?;

        let sampler_desc = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MinLOD: 0.0,
            MaxLOD: D3D11_FLOAT32_MAX,
        };
        let mut linear_sampler = None;
        // SAFETY: `sampler_desc` is a valid descriptor.
        unsafe { device.CreateSamplerState(&sampler_desc, Some(&mut linear_sampler)) }
            .map_err(|e| state_error("sampler", e))?;

        Ok(Self {
            quad_vs: vs(Entry::QuadVs)?,
            quad_ps: ps(Entry::QuadPs)?,
            quad_layout: layout(&quad_elements(), Entry::QuadVs)?,
            text_vs: vs(Entry::TextVs)?,
            text_ps: ps(Entry::TextPs)?,
            text_subpixel_ps: ps(Entry::TextSubpixelPs)?,
            glyph_layout: layout(&glyph_elements(), Entry::TextVs)?,
            mesh_vs: vs(Entry::MeshVs)?,
            mesh_ps: ps(Entry::MeshPs)?,
            mesh_textured_ps: ps(Entry::MeshTexturedPs)?,
            mesh_layout: layout(&mesh_elements(), Entry::MeshVs)?,
            composite_vs: vs(Entry::CompositeVs)?,
            composite_ps: ps(Entry::CompositePs)?,
            fullscreen_vs: vs(Entry::FullscreenVs)?,
            blur_ps: ps(Entry::BlurPs)?,
            resolve_ps: ps(Entry::ResolvePs)?,
            premultiplied: blend(Blend::Premultiplied)?,
            subpixel: blend(Blend::Subpixel)?,
            replace: blend(Blend::Replace)?,
            rasterizer: rasterizer.expect("CreateRasterizerState succeeded"),
            linear_sampler: linear_sampler.expect("CreateSamplerState succeeded"),
        })
    }

    /// Binds everything a pipeline needs except its resources.
    pub fn bind(&self, context: &ID3D11DeviceContext, kind: PipelineKind) {
        use PipelineKind as K;
        let (layout, topology, vs, ps, blend): (
            Option<&ID3D11InputLayout>,
            D3D_PRIMITIVE_TOPOLOGY,
            &ID3D11VertexShader,
            &ID3D11PixelShader,
            &ID3D11BlendState,
        ) = match kind {
            K::Quad | K::Image => (
                Some(&self.quad_layout),
                D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
                &self.quad_vs,
                &self.quad_ps,
                &self.premultiplied,
            ),
            K::Text => (
                Some(&self.glyph_layout),
                D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
                &self.text_vs,
                &self.text_ps,
                &self.premultiplied,
            ),
            K::TextSubpixel => (
                Some(&self.glyph_layout),
                D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
                &self.text_vs,
                &self.text_subpixel_ps,
                &self.subpixel,
            ),
            K::Mesh => (
                Some(&self.mesh_layout),
                D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
                &self.mesh_vs,
                &self.mesh_ps,
                &self.premultiplied,
            ),
            K::MeshTextured => (
                Some(&self.mesh_layout),
                D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
                &self.mesh_vs,
                &self.mesh_textured_ps,
                &self.premultiplied,
            ),
            // Every blend mode composites as premultiplied source-over, as in
            // the wgpu backend, whose composite pipeline used the same state
            // for every mode.
            K::Composite => (
                None,
                D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
                &self.composite_vs,
                &self.composite_ps,
                &self.premultiplied,
            ),
            K::Blur => (
                None,
                D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
                &self.fullscreen_vs,
                &self.blur_ps,
                &self.replace,
            ),
            K::Resolve => (
                None,
                D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
                &self.fullscreen_vs,
                &self.resolve_ps,
                &self.replace,
            ),
        };
        // SAFETY: plain state-setting calls with live objects.
        unsafe {
            context.IASetInputLayout(layout);
            context.IASetPrimitiveTopology(topology);
            context.VSSetShader(vs, None);
            context.PSSetShader(ps, None);
            context.OMSetBlendState(blend, None, u32::MAX);
        }
    }
}

fn blend_desc(kind: Blend) -> D3D11_BLEND_DESC {
    let target = |src: D3D11_BLEND, dst: D3D11_BLEND, src_a: D3D11_BLEND, dst_a: D3D11_BLEND| {
        D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: (kind != Blend::Replace).into(),
            SrcBlend: src,
            DestBlend: dst,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: src_a,
            DestBlendAlpha: dst_a,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        }
    };
    let rt0 = match kind {
        Blend::Premultiplied => target(
            D3D11_BLEND_ONE,
            D3D11_BLEND_INV_SRC_ALPHA,
            D3D11_BLEND_ONE,
            D3D11_BLEND_INV_SRC_ALPHA,
        ),
        // The subpixel pixel shader emits premultiplied foreground in source
        // zero and per-channel coverage in source one, so each of R, G and B
        // gets `src0 + dst * (1 - src1)` with its own coverage. Alpha follows
        // the same equation on source one's alpha.
        Blend::Subpixel => target(
            D3D11_BLEND_ONE,
            D3D11_BLEND_INV_SRC1_COLOR,
            D3D11_BLEND_ONE,
            D3D11_BLEND_INV_SRC1_ALPHA,
        ),
        Blend::Replace => {
            target(D3D11_BLEND_ONE, D3D11_BLEND_ZERO, D3D11_BLEND_ONE, D3D11_BLEND_ZERO)
        }
    };
    let mut desc = D3D11_BLEND_DESC {
        // Alpha-to-coverage would quantise every translucent primitive to the
        // sample count, far worse than the analytic alpha the shaders produce.
        AlphaToCoverageEnable: false.into(),
        IndependentBlendEnable: false.into(),
        RenderTarget: Default::default(),
    };
    desc.RenderTarget[0] = rt0;
    desc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format_size(f: DXGI_FORMAT) -> usize {
        match f {
            DXGI_FORMAT_R32G32B32A32_FLOAT | DXGI_FORMAT_R32G32B32A32_UINT => 16,
            DXGI_FORMAT_R32G32B32_UINT => 12,
            DXGI_FORMAT_R32G32_FLOAT => 8,
            DXGI_FORMAT_R32_FLOAT => 4,
            other => panic!("unhandled format {other:?}"),
        }
    }

    fn span(elements: &[D3D11_INPUT_ELEMENT_DESC]) -> usize {
        elements.iter().map(|e| e.AlignedByteOffset as usize + format_size(e.Format)).max().unwrap()
    }

    #[test]
    fn attributes_fit_inside_their_structs() {
        // An attribute running past the struct reads the next instance's bytes:
        // subtly wrong frames rather than an error.
        assert!(span(&quad_elements()) <= size_of::<QuadInstance>());
        assert_eq!(span(&glyph_elements()), size_of::<GlyphInstance>());
        assert_eq!(span(&mesh_elements()), size_of::<MeshVertex>());
    }

    #[test]
    fn glyph_layout_matches_the_64_byte_instance() {
        let e = glyph_elements();
        let offsets: Vec<u32> = e.iter().map(|e| e.AlignedByteOffset).collect();
        assert_eq!(offsets, [0, 16, 32, 48, 52]);
    }

    #[test]
    fn subpixel_blend_uses_the_second_source_as_coverage() {
        let d = blend_desc(Blend::Subpixel).RenderTarget[0];
        assert_eq!(d.SrcBlend, D3D11_BLEND_ONE);
        assert_eq!(d.DestBlend, D3D11_BLEND_INV_SRC1_COLOR);
        assert_eq!(d.SrcBlendAlpha, D3D11_BLEND_ONE);
        assert_eq!(d.DestBlendAlpha, D3D11_BLEND_INV_SRC1_ALPHA);
    }

    #[test]
    fn ordinary_pipelines_blend_premultiplied() {
        let d = blend_desc(Blend::Premultiplied).RenderTarget[0];
        assert!(d.BlendEnable.as_bool());
        assert_eq!((d.SrcBlend, d.DestBlend), (D3D11_BLEND_ONE, D3D11_BLEND_INV_SRC_ALPHA));
        assert!(!blend_desc(Blend::Replace).RenderTarget[0].BlendEnable.as_bool());
    }
}
