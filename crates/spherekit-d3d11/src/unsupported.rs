//! The backend's shape on platforms without Direct3D.
//!
//! An uninhabited type rather than a missing one, so code that names
//! `D3D11Renderer` compiles everywhere and simply takes its fallback path at
//! runtime: the constructors always fail, and no value can ever exist for the
//! trait methods to be called on.

use spherekit_core::{Color, InitError, RenderError, SurfaceError, TextureId};
use spherekit_render::{
    BackendCapabilities, CompiledFrame, FrameHandle, FrameStats, RendererBackend, SurfaceConfig,
};

/// The Direct3D 11 renderer. Cannot be constructed on this platform.
pub enum D3D11Renderer {}

fn unavailable() -> InitError {
    InitError::NoAdapter("Direct3D 11 is only available on Windows".into())
}

impl D3D11Renderer {
    /// Always fails: Direct3D 11 is only available on Windows.
    pub fn new(
        window: &impl raw_window_handle::HasWindowHandle,
        config: SurfaceConfig,
    ) -> Result<Self, InitError> {
        let _ = (window, config);
        Err(unavailable())
    }

    /// Always fails: Direct3D 11 is only available on Windows.
    pub fn new_offscreen(config: SurfaceConfig) -> Result<Self, InitError> {
        let _ = config;
        Err(unavailable())
    }

    /// Unreachable: no value of this type can exist.
    pub fn read_pixels(&mut self) -> Result<Vec<u8>, RenderError> {
        match *self {}
    }
}

impl RendererBackend for D3D11Renderer {
    fn adapter_name(&self) -> &str {
        match *self {}
    }

    fn capabilities(&self) -> BackendCapabilities {
        match *self {}
    }

    fn configure_surface(&mut self, _config: SurfaceConfig) -> Result<(), RenderError> {
        match *self {}
    }

    fn begin_frame(&mut self) -> Result<FrameHandle, SurfaceError> {
        match *self {}
    }

    fn render(
        &mut self,
        _frame: &mut FrameHandle,
        _compiled: &CompiledFrame,
        _clear: Color,
    ) -> Result<(), RenderError> {
        match *self {}
    }

    fn end_frame(&mut self, _frame: FrameHandle) -> Result<FrameStats, RenderError> {
        match *self {}
    }

    fn upload_texture(
        &mut self,
        _id: Option<TextureId>,
        _width: u32,
        _height: u32,
        _rgba: &[u8],
    ) -> Result<TextureId, RenderError> {
        match *self {}
    }

    fn upload_texture_region(
        &mut self,
        _id: TextureId,
        _x: u32,
        _y: u32,
        _width: u32,
        _height: u32,
        _rgba: &[u8],
    ) -> Result<(), RenderError> {
        match *self {}
    }

    fn destroy_texture(&mut self, _id: TextureId) {
        match *self {}
    }

    fn read_pixels(&mut self) -> Result<Vec<u8>, RenderError> {
        match *self {}
    }

    fn set_atlas_page(&mut self, _page: u32, _texture: TextureId) {
        match *self {}
    }

    fn memory_usage(&self) -> u64 {
        match *self {}
    }
}
