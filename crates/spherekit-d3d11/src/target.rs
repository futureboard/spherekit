//! Render target creation and the offscreen target pool.

use spherekit_core::RenderError;
use windows::Win32::Graphics::Direct3D::{
    D3D_SRV_DIMENSION_TEXTURE2D, D3D_SRV_DIMENSION_TEXTURE2DMS,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_RENDER_TARGET_VIEW_DESC,
    D3D11_RENDER_TARGET_VIEW_DESC_0, D3D11_RTV_DIMENSION_TEXTURE2D,
    D3D11_RTV_DIMENSION_TEXTURE2DMS, D3D11_SHADER_RESOURCE_VIEW_DESC,
    D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_RTV, D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11RenderTargetView, ID3D11ShaderResourceView,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_TYPELESS, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
    DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, DXGI_SAMPLE_DESC,
};

/// The colour format offscreen layers render into.
///
/// sRGB, matching the wgpu backend's `LAYER_FORMAT`, so blending inside a
/// layer happens in linear light exactly as it does on the surface.
pub(crate) const LAYER_FORMAT: DXGI_FORMAT = DXGI_FORMAT_R8G8B8A8_UNORM_SRGB;

/// The storage format of the surface: what every flip-model swap chain
/// buffer is (`B8G8R8A8_UNORM`) belongs to this typeless family.
pub(crate) const SURFACE_TYPELESS: DXGI_FORMAT = DXGI_FORMAT_B8G8R8A8_TYPELESS;

/// The view format the surface is rendered and sampled through. The hardware
/// encodes linear output to sRGB on write and decodes on read.
pub(crate) const SURFACE_VIEW: DXGI_FORMAT = DXGI_FORMAT_B8G8R8A8_UNORM_SRGB;

/// A 2D texture with the views the renderer needs on it.
pub(crate) struct Target {
    pub texture: ID3D11Texture2D,
    pub rtv: ID3D11RenderTargetView,
    /// Absent for multisampled targets that are only ever resolved by
    /// `ResolveSubresource`.
    pub srv: Option<ID3D11ShaderResourceView>,
}

/// Creates a texture with storage format `storage`, viewed as `view`.
pub(crate) fn create_target(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    storage: DXGI_FORMAT,
    view: DXGI_FORMAT,
    samples: u32,
    shader_resource: bool,
) -> Result<Target, RenderError> {
    let bytes = width as u64 * height as u64 * 4 * samples.max(1) as u64;
    let mut bind = D3D11_BIND_RENDER_TARGET.0 as u32;
    if shader_resource {
        bind |= D3D11_BIND_SHADER_RESOURCE.0 as u32;
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: storage,
        SampleDesc: DXGI_SAMPLE_DESC { Count: samples.max(1), Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut texture = None;
    // SAFETY: `desc` is a valid descriptor and the out-pointer a local.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture)) }
        .map_err(|_| RenderError::OutOfMemory { what: "render target", bytes })?;
    let texture = texture.expect("CreateTexture2D succeeded");
    let rtv = create_rtv(device, &texture, view, samples > 1)?;
    let srv =
        if shader_resource { Some(create_srv(device, &texture, view, samples > 1)?) } else { None };
    Ok(Target { texture, rtv, srv })
}

/// A render target view of `texture` in format `format`.
pub(crate) fn create_rtv(
    device: &ID3D11Device,
    texture: &ID3D11Texture2D,
    format: DXGI_FORMAT,
    multisampled: bool,
) -> Result<ID3D11RenderTargetView, RenderError> {
    let desc = D3D11_RENDER_TARGET_VIEW_DESC {
        Format: format,
        ViewDimension: if multisampled {
            D3D11_RTV_DIMENSION_TEXTURE2DMS
        } else {
            D3D11_RTV_DIMENSION_TEXTURE2D
        },
        Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 } },
    };
    let mut rtv = None;
    // SAFETY: `desc` describes a view compatible with the texture's format.
    unsafe { device.CreateRenderTargetView(texture, Some(&desc), Some(&mut rtv)) }
        .map_err(|e| RenderError::Backend(format!("render target view: {e}")))?;
    Ok(rtv.expect("CreateRenderTargetView succeeded"))
}

fn create_srv(
    device: &ID3D11Device,
    texture: &ID3D11Texture2D,
    format: DXGI_FORMAT,
    multisampled: bool,
) -> Result<ID3D11ShaderResourceView, RenderError> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: if multisampled {
            D3D_SRV_DIMENSION_TEXTURE2DMS
        } else {
            D3D_SRV_DIMENSION_TEXTURE2D
        },
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 },
        },
    };
    let mut srv = None;
    // SAFETY: `desc` describes a view compatible with the texture's format.
    unsafe { device.CreateShaderResourceView(texture, Some(&desc), Some(&mut srv)) }
        .map_err(|e| RenderError::Backend(format!("shader resource view: {e}")))?;
    Ok(srv.expect("CreateShaderResourceView succeeded"))
}

/// What a pooled target is for. Part of the pool key.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum PoolKind {
    /// An offscreen layer: `LAYER_FORMAT`, with a multisampled twin when MSAA
    /// is on, because a layer is sampled after it is drawn and a multisampled
    /// texture cannot be.
    Layer,
    /// A single-sample `LAYER_FORMAT` scratch target for a blur pass.
    Filter,
    /// A single-sample copy of a region of the surface, for backdrop blur.
    /// Same typeless family as the swap chain so `CopySubresourceRegion` is
    /// legal, read through an sRGB view.
    SurfaceCopy,
}

/// One pooled offscreen target.
pub(crate) struct PoolTarget {
    pub kind: PoolKind,
    pub size: (u32, u32),
    /// The single-sample, sampleable texture.
    pub resolved: Target,
    /// The multisampled attachment drawn into, when MSAA is on.
    pub msaa: Option<Target>,
    /// Frame index this target was last used on.
    pub last_used: u64,
}

impl PoolTarget {
    /// GPU memory held, in bytes.
    pub fn bytes(&self, samples: u32) -> u64 {
        let px = self.size.0 as u64 * self.size.1 as u64 * 4;
        px + if self.msaa.is_some() { px * samples as u64 } else { 0 }
    }
}

/// Offscreen render targets, pooled across frames and keyed by size and kind.
#[derive(Default)]
pub(crate) struct TargetPool {
    pub targets: Vec<PoolTarget>,
}

impl TargetPool {
    /// A target of the requested size and kind not yet used this frame.
    pub fn acquire(
        &mut self,
        device: &ID3D11Device,
        frame: u64,
        width: u32,
        height: u32,
        kind: PoolKind,
        samples: u32,
    ) -> Result<usize, RenderError> {
        let (w, h) = (width.max(1), height.max(1));
        if let Some(i) = self
            .targets
            .iter()
            .position(|t| t.size == (w, h) && t.kind == kind && t.last_used != frame)
        {
            self.targets[i].last_used = frame;
            return Ok(i);
        }
        let (storage, view) = match kind {
            PoolKind::Layer | PoolKind::Filter => (LAYER_FORMAT, LAYER_FORMAT),
            PoolKind::SurfaceCopy => (SURFACE_TYPELESS, SURFACE_VIEW),
        };
        let resolved = create_target(device, w, h, storage, view, 1, true)?;
        let msaa = if kind == PoolKind::Layer && samples > 1 {
            Some(create_target(device, w, h, LAYER_FORMAT, LAYER_FORMAT, samples, false)?)
        } else {
            None
        };
        self.targets.push(PoolTarget { kind, size: (w, h), resolved, msaa, last_used: frame });
        Ok(self.targets.len() - 1)
    }

    /// Drops targets that have gone `KEEP_FOR_FRAMES` frames without use.
    ///
    /// A window that briefly showed a blurred popup should not hold that render
    /// target for the rest of the session, but neither should a target be freed
    /// the moment one frame skips it: a menu opening and closing repeatedly
    /// would then reallocate every time.
    pub fn reap(&mut self, frame: u64) {
        const KEEP_FOR_FRAMES: u64 = 60;
        self.targets.retain(|t| frame.saturating_sub(t.last_used) < KEEP_FOR_FRAMES);
    }

    /// Drops everything, after the sample count changed.
    pub fn clear(&mut self) {
        self.targets.clear();
    }

    /// GPU memory held, in bytes.
    pub fn bytes(&self, samples: u32) -> u64 {
        self.targets.iter().map(|t| t.bytes(samples)).sum()
    }
}
