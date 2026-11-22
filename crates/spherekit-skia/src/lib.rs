//! # spherekit-skia
//!
//! The CPU rendering backend: [`spherekit_render::RendererBackend`] on Skia's
//! raster pipeline, presented through `softbuffer`.
//!
//! It is the fallback wherever Direct3D 11 has no hardware device — a VM, a
//! remote session, a broken driver — and the only backend on Linux and macOS,
//! so nothing here may depend on a platform API beyond what `softbuffer`
//! abstracts.
//!
//! ## Mapping, not emulation
//!
//! The batch compiler's output was designed for instanced GPU shaders, but
//! every primitive it describes is something Skia draws natively and well:
//!
//! | Compiled primitive | Skia |
//! |--------------------|------|
//! | rounded rectangle, border | `draw_rrect`, `draw_drrect` |
//! | linear / radial / sweep gradient | gradient shaders, linear-light interpolation |
//! | box shadow, inset shadow | blur `MaskFilter`; clip + blurred ring |
//! | rounded clip, scissor | antialiased `clip_rrect`, `clip_irect` |
//! | glyph | `Alpha8` mirror of the atlas page; one `draw_atlas` per run of glyphs |
//! | image quad | `draw_image_rect` with a tint colour filter |
//! | tessellated path | one antialiased path built from its triangles |
//! | coloured / textured mesh | `draw_vertices` |
//! | layer | pooled offscreen surface, composited with opacity, blend mode, blur, colour matrix |
//!
//! Emulating the shaders per pixel would be slower and less precise than
//! Skia's own analytic rasterisers, which are what a CPU fallback should spend
//! its time in. The `frame` module's docs cover coordinate spaces and the
//! `color` module's the colour strategy, which is the one place this backend
//! deliberately differs from the GPU path.
//!
//! ## Presentation
//!
//! A windowed renderer draws straight into `softbuffer`'s buffer — no
//! intermediate copy — and presents at the end of
//! [`RendererBackend::render`], because the buffer borrows the window surface
//! and cannot be held until `end_frame`. The `present` module documents the pixel format
//! and for what cannot be honoured (transparency, vsync, latency).

#![deny(missing_docs)]

mod color;
mod frame;
mod pool;
mod present;
mod texture;

use crate::frame::Painter;
use crate::pool::LayerPool;
use crate::present::{Presenter, SoftbufferPresenter};
use crate::texture::TextureStore;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use skia_safe::{AlphaType, ColorSpace, ColorType, ISize, ImageInfo, Surface, surfaces};
use spherekit_core::{Color, DevicePx, InitError, RenderError, Size, SurfaceError, TextureId};
use spherekit_render::{
    BackendCapabilities, CompiledFrame, FrameHandle, FrameStats, RendererBackend, SurfaceConfig,
};
use std::sync::Arc;
use std::time::Instant;

/// Where frames end up.
enum Output {
    /// A window, through `softbuffer`.
    Window(Box<dyn Presenter>),
    /// An internal raster surface, for tests, screenshots and headless use.
    /// `None` until the first non-zero configuration.
    Offscreen(Option<Surface>),
}

/// The Skia CPU renderer.
///
/// Create one with [`SkiaRenderer::new`] for a window or
/// [`SkiaRenderer::new_offscreen`] for headless rendering, then drive it
/// through [`RendererBackend`].
pub struct SkiaRenderer {
    output: Output,
    config: SurfaceConfig,
    /// The current device size; `(0, 0)` until a non-zero size is configured.
    size: (u32, u32),
    textures: TextureStore,
    pool: LayerPool,
    painter: Painter,
    srgb: ColorSpace,
    frame_index: u64,
    stats: FrameStats,
    adapter_name: String,
}

impl SkiaRenderer {
    /// Creates a renderer presenting to `window`.
    ///
    /// The window is shared rather than borrowed because `softbuffer` keeps a
    /// handle to it for the surface's whole lifetime.
    pub fn new<W>(window: Arc<W>, config: SurfaceConfig) -> Result<Self, InitError>
    where
        W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
    {
        let presenter = SoftbufferPresenter::new(window)?;
        let mut renderer = Self::with_output(Output::Window(Box::new(presenter)), config);
        renderer.configure_surface(config).map_err(|e| InitError::Surface(e.to_string()))?;
        Ok(renderer)
    }

    /// Creates a renderer with no window: frames render into an internal
    /// raster surface, readable with [`SkiaRenderer::read_pixels`].
    ///
    /// Unlike a window, the offscreen surface honours
    /// [`SurfaceConfig::transparent`]: when set, the surface keeps an alpha
    /// channel and the clear colour's alpha survives.
    pub fn new_offscreen(config: SurfaceConfig) -> Result<Self, InitError> {
        let mut renderer = Self::with_output(Output::Offscreen(None), config);
        renderer.configure_surface(config).map_err(|e| InitError::Surface(e.to_string()))?;
        Ok(renderer)
    }

    fn with_output(output: Output, config: SurfaceConfig) -> Self {
        let mut painter = Painter::new();
        painter.antialias_meshes = config.msaa_samples > 1;
        Self {
            output,
            config,
            size: (0, 0),
            textures: TextureStore::new(),
            pool: LayerPool::new(),
            painter,
            srgb: ColorSpace::new_srgb(),
            frame_index: 0,
            stats: FrameStats::default(),
            adapter_name: format!("Skia CPU raster ({})", skia_milestone()),
        }
    }

    /// Reads back the last rendered frame.
    ///
    /// Returns `width × height × 4` bytes of **premultiplied** RGBA8 in the
    /// surface's sRGB encoding, rows top to bottom. On an opaque surface every
    /// alpha is 255, so premultiplied and straight are the same thing.
    ///
    /// For a windowed renderer this asks the window system for what it shows,
    /// which not every platform supports.
    pub fn read_pixels(&mut self) -> Result<Vec<u8>, RenderError> {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return Err(RenderError::Surface(SurfaceError::ZeroSized));
        }
        match &mut self.output {
            Output::Offscreen(Some(surface)) => {
                let info = ImageInfo::new(
                    ISize::new(w as i32, h as i32),
                    ColorType::RGBA8888,
                    AlphaType::Premul,
                    self.srgb.clone(),
                );
                let mut out = vec![0u8; w as usize * h as usize * 4];
                if surface.read_pixels(&info, &mut out, w as usize * 4, (0, 0)) {
                    Ok(out)
                } else {
                    Err(RenderError::Backend("Skia could not read the surface back".into()))
                }
            }
            Output::Offscreen(None) => Err(RenderError::Surface(SurfaceError::ZeroSized)),
            Output::Window(presenter) => {
                let pixels = presenter.fetch()?;
                Ok(pixels
                    .iter()
                    .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8, 255])
                    .collect())
            }
        }
    }

    fn surface_info(&self, w: u32, h: u32, opaque: bool) -> ImageInfo {
        ImageInfo::new(
            ISize::new(w as i32, h as i32),
            // BGRA is softbuffer's byte order; using it offscreen too means the
            // two outputs go through identical raster pipelines.
            ColorType::BGRA8888,
            if opaque { AlphaType::Opaque } else { AlphaType::Premul },
            self.srgb.clone(),
        )
    }

    fn is_opaque(&self) -> bool {
        // softbuffer cannot present alpha, so windows are always opaque.
        matches!(self.output, Output::Window(_)) || !self.config.transparent
    }
}

/// The Skia milestone this crate was built against, for diagnostics.
fn skia_milestone() -> &'static str {
    // skia-safe 0.157 binds Skia m157; there is no runtime query for it.
    "m157"
}

impl RendererBackend for SkiaRenderer {
    fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            max_texture_size: texture::MAX_TEXTURE_SIZE,
            timestamp_queries: false,
            offscreen_targets: true,
            compute: false,
            // There is no second blend source on the CPU. Reporting it keeps
            // the compiler from ever asking for RGB-subpixel glyphs.
            dual_source_blending: false,
            max_instances_per_draw: u32::MAX,
            // Skia antialiases analytically; there are no sample counts.
            max_msaa_samples: 1,
        }
    }

    fn configure_surface(&mut self, config: SurfaceConfig) -> Result<(), RenderError> {
        let (w, h) = (config.size.width.as_u32(), config.size.height.as_u32());
        self.painter.antialias_meshes = config.msaa_samples > 1;
        // A minimised window legitimately reports zero; keep the last good
        // surface rather than failing.
        if w == 0 || h == 0 {
            return Ok(());
        }
        let resized = self.size != (w, h) || self.config.transparent != config.transparent;
        self.config = config;
        self.size = (w, h);
        if !resized {
            return Ok(());
        }
        let info = self.surface_info(w, h, self.is_opaque());
        match &mut self.output {
            Output::Window(presenter) => presenter.resize(w, h)?,
            Output::Offscreen(surface) => {
                *surface =
                    Some(surfaces::raster(&info, None, None).ok_or(RenderError::OutOfMemory {
                        what: "offscreen surface",
                        bytes: w as u64 * h as u64 * 4,
                    })?);
            }
        }
        Ok(())
    }

    fn begin_frame(&mut self) -> Result<FrameHandle, SurfaceError> {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return Err(SurfaceError::ZeroSized);
        }
        self.frame_index += 1;
        Ok(FrameHandle {
            index: self.frame_index,
            size: Size::new(DevicePx(w as i32), DevicePx(h as i32)),
            slot: 0,
        })
    }

    fn render(
        &mut self,
        frame: &mut FrameHandle,
        compiled: &CompiledFrame,
        clear: Color,
    ) -> Result<(), RenderError> {
        let _ = frame;
        let start = Instant::now();
        self.textures.prepare(compiled);
        let clear = color::clear_color(clear, self.is_opaque());
        let info = self.surface_info(self.size.0, self.size.1, self.is_opaque());
        let frame_index = self.frame_index;

        let (painter, pool, textures) = (&mut self.painter, &mut self.pool, &self.textures);
        let mut draw = |surface: &mut Surface| {
            let canvas = surface.canvas();
            canvas.restore_to_count(1);
            canvas.reset_matrix();
            canvas.clear(clear);
            painter.render(surface, pool, textures, compiled, frame_index)
        };

        let counters = match &mut self.output {
            Output::Offscreen(Some(surface)) => draw(surface),
            Output::Offscreen(None) => return Err(RenderError::Surface(SurfaceError::ZeroSized)),
            Output::Window(presenter) => {
                let mut counters = Default::default();
                presenter.frame(&mut |bytes, w, h| {
                    let info = info.with_dimensions(ISize::new(w as i32, h as i32));
                    let mut surface = surfaces::wrap_pixels(&info, bytes, w as usize * 4, None)
                        .ok_or_else(|| {
                            RenderError::Backend("Skia could not wrap the softbuffer buffer".into())
                        })?;
                    counters = draw(&mut surface);
                    Ok(())
                })?;
                counters
            }
        };

        self.stats = FrameStats {
            cpu_ms: start.elapsed().as_secs_f32() * 1000.0,
            gpu_ms: None,
            draw_calls: counters.draw_calls,
            pipeline_switches: counters.pipeline_switches,
            quads: compiled.stats.quads,
            glyphs: compiled.stats.glyphs,
            triangles: compiled.stats.triangles,
            bytes_uploaded: self.textures.take_uploaded(),
            layers: compiled.stats.layers,
        };
        Ok(())
    }

    fn end_frame(&mut self, frame: FrameHandle) -> Result<FrameStats, RenderError> {
        let _ = frame;
        // Presentation already happened at the end of `render`; see the crate
        // docs. All that is left is returning idle layers to the system.
        self.pool.reap(self.frame_index);
        Ok(self.stats)
    }

    fn upload_texture(
        &mut self,
        id: Option<TextureId>,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<TextureId, RenderError> {
        self.textures.upload(id, width, height, rgba)
    }

    fn upload_texture_region(
        &mut self,
        id: TextureId,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), RenderError> {
        self.textures.upload_region(id, x, y, width, height, rgba)
    }

    fn destroy_texture(&mut self, id: TextureId) {
        self.textures.destroy(id);
    }

    fn read_pixels(&mut self) -> Result<Vec<u8>, RenderError> {
        Self::read_pixels(self)
    }

    fn set_atlas_page(&mut self, page: u32, texture: TextureId) {
        self.textures.set_atlas_page(page, texture);
    }

    fn memory_usage(&self) -> u64 {
        let surface = match &self.output {
            Output::Offscreen(Some(_)) => self.size.0 as u64 * self.size.1 as u64 * 4,
            _ => 0,
        };
        self.textures.memory_bytes() + self.pool.memory_bytes() + surface
    }
}
