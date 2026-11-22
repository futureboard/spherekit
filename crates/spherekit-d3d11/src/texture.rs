//! Texture storage: images, glyph atlas pages, and the 1x1 white texture.
//!
//! The white texture lets solid quads and untextured meshes run through the
//! same shader slot as textured ones; binding it is cheaper than a second
//! pipeline per primitive or a null-binding special case.
//!
//! ## Two views per texture
//!
//! The trait hands over two kinds of RGBA8 data through the same call: images
//! arrive **sRGB-encoded** (premultiplied, straight from the decoder), while
//! glyph atlas pages hold **linear coverage**, which is geometry rather than
//! colour. Sampling both through one UNORM view — what the wgpu backend did —
//! feeds sRGB bytes into a linear pipeline and an sRGB target, encoding images
//! twice and showing them too bright.
//!
//! So every texture is stored `R8G8B8A8_TYPELESS` with an `_SRGB` view, which
//! the hardware decodes to linear on sample, and a plain `UNORM` view. A
//! texture is sampled through the UNORM view only when it is bound as an atlas
//! page via `set_atlas_page`; images and textured meshes use the sRGB view, so
//! an opaque image lands on screen byte-for-byte as in its file.

use rustc_hash::FxHashMap;
use spherekit_core::{GenerationalStore, RenderError, TextureId};
use windows::Win32::Graphics::Direct3D::D3D_SRV_DIMENSION_TEXTURE2D;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11DeviceContext, ID3D11ShaderResourceView,
    ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R8G8B8A8_TYPELESS, DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, DXGI_SAMPLE_DESC,
};

struct Entry {
    texture: ID3D11Texture2D,
    /// Decodes sRGB on sample: images and textured meshes.
    srv: ID3D11ShaderResourceView,
    /// Reads the bytes as they are: glyph atlas pages.
    srv_linear: ID3D11ShaderResourceView,
    width: u32,
    height: u32,
    bytes: u64,
}

/// Owns every texture the renderer was asked to hold.
pub(crate) struct TextureStore {
    entries: GenerationalStore<TextureId, Entry>,
    /// Glyph atlas pages, keyed by page index rather than by handle, because
    /// the batch compiler names a page before the text system has uploaded it.
    atlas_pages: FxHashMap<u32, TextureId>,
    white: TextureId,
    bytes: u64,
}

impl TextureStore {
    /// Creates the store and its 1x1 white texture.
    pub fn new(device: &ID3D11Device, context: &ID3D11DeviceContext) -> Result<Self, RenderError> {
        let mut store = Self {
            entries: GenerationalStore::new(),
            atlas_pages: FxHashMap::default(),
            white: TextureId::new(u32::MAX, 1),
            bytes: 0,
        };
        store.white = store.upload(device, context, None, 1, 1, &[255, 255, 255, 255])?;
        Ok(store)
    }

    /// The sRGB-decoding view of a texture, or `None` when the handle is stale.
    #[inline]
    pub fn srv(&self, id: TextureId) -> Option<&ID3D11ShaderResourceView> {
        self.entries.get(id).map(|e| &e.srv)
    }

    /// The 1x1 white texture.
    #[inline]
    pub fn white_srv(&self) -> &ID3D11ShaderResourceView {
        &self.entries.get(self.white).expect("the white texture is never destroyed").srv
    }

    /// The linear (UNORM) view of the texture bound to atlas page `page`.
    #[inline]
    pub fn atlas_srv(&self, page: u32) -> Option<&ID3D11ShaderResourceView> {
        self.entries.get(*self.atlas_pages.get(&page)?).map(|e| &e.srv_linear)
    }

    /// Registers a texture as a glyph atlas page.
    pub fn set_atlas_page(&mut self, page: u32, id: TextureId) {
        self.atlas_pages.insert(page, id);
    }

    /// Total texture memory held, in bytes.
    #[inline]
    pub fn memory_bytes(&self) -> u64 {
        self.bytes
    }

    /// Creates or replaces a texture from RGBA8 pixels.
    pub fn upload(
        &mut self,
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        id: Option<TextureId>,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<TextureId, RenderError> {
        let expected = (width as usize) * (height as usize) * 4;
        if rgba.len() < expected {
            return Err(RenderError::Backend(format!(
                "texture upload is {} bytes but {width}x{height} RGBA needs {expected}",
                rgba.len()
            )));
        }

        // Replacing a texture of the same size reuses the allocation; a glyph
        // atlas page that is re-uploaded whole would otherwise churn memory.
        if let Some(existing) = id
            && let Some(e) = self.entries.get(existing)
            && e.width == width
            && e.height == height
        {
            // SAFETY: the source holds `height` rows of `width * 4` bytes.
            unsafe {
                context.UpdateSubresource(&e.texture, 0, None, rgba.as_ptr().cast(), width * 4, 0)
            };
            return Ok(existing);
        }
        if let Some(existing) = id {
            self.destroy(existing);
        }

        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            // Typeless, so it can be viewed both ways; see the module docs.
            Format: DXGI_FORMAT_R8G8B8A8_TYPELESS,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: rgba.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };
        let bytes = expected as u64;
        let mut texture = None;
        // SAFETY: `init` points at `height` rows of `width * 4` bytes.
        unsafe { device.CreateTexture2D(&desc, Some(&init), Some(&mut texture)) }
            .map_err(|_| RenderError::OutOfMemory { what: "texture", bytes })?;
        let texture = texture.expect("CreateTexture2D succeeded");
        let srv = view(device, &texture, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB)?;
        let srv_linear = view(device, &texture, DXGI_FORMAT_R8G8B8A8_UNORM)?;

        self.bytes += bytes;
        Ok(self.entries.insert(Entry { texture, srv, srv_linear, width, height, bytes }))
    }

    /// Writes a sub-rectangle of an existing texture.
    ///
    /// Applied immediately: `UpdateSubresource` is ordered on the immediate
    /// context, so there is nothing to gain from queueing it the way the wgpu
    /// backend had to.
    #[allow(clippy::too_many_arguments)]
    pub fn upload_region(
        &mut self,
        context: &ID3D11DeviceContext,
        id: TextureId,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), RenderError> {
        let Some(e) = self.entries.get(id) else {
            return Err(RenderError::StaleResource("texture"));
        };
        if x.checked_add(width).is_none_or(|r| r > e.width)
            || y.checked_add(height).is_none_or(|b| b > e.height)
        {
            return Err(RenderError::Backend(format!(
                "region {width}x{height} at ({x},{y}) is outside a {}x{} texture",
                e.width, e.height
            )));
        }
        let expected = (width as usize) * (height as usize) * 4;
        if rgba.len() < expected {
            return Err(RenderError::Backend(format!(
                "region upload is {} bytes but needs {expected}",
                rgba.len()
            )));
        }
        if width == 0 || height == 0 {
            return Ok(());
        }
        let region =
            D3D11_BOX { left: x, top: y, front: 0, right: x + width, bottom: y + height, back: 1 };
        // SAFETY: the box lies inside the texture (checked above) and the source
        // holds `height` rows of `width * 4` bytes.
        unsafe {
            context.UpdateSubresource(
                &e.texture,
                0,
                Some(&region),
                rgba.as_ptr().cast(),
                width * 4,
                0,
            )
        };
        Ok(())
    }

    /// Releases a texture. The white texture is internal and survives.
    pub fn destroy(&mut self, id: TextureId) {
        if id == self.white {
            return;
        }
        if let Some(e) = self.entries.remove(id) {
            self.bytes = self.bytes.saturating_sub(e.bytes);
        }
        self.atlas_pages.retain(|_, v| *v != id);
    }
}

fn view(
    device: &ID3D11Device,
    texture: &ID3D11Texture2D,
    format: DXGI_FORMAT,
) -> Result<ID3D11ShaderResourceView, RenderError> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 },
        },
    };
    let mut srv = None;
    // SAFETY: `format` is a typed member of the texture's typeless family.
    unsafe { device.CreateShaderResourceView(texture, Some(&desc), Some(&mut srv)) }
        .map_err(|e| RenderError::Backend(format!("texture view: {e}")))?;
    Ok(srv.expect("CreateShaderResourceView succeeded"))
}
