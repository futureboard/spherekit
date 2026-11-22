//! The Direct3D 11 renderer.
//!
//! A port of the wgpu backend's frame flow onto the Direct3D 11 immediate
//! context. It consumes a [`CompiledFrame`] — pure data, produced without a
//! GPU — and issues draw calls for it.
//!
//! What it deliberately does not do, same as its predecessor:
//!
//! * Allocate per frame. Buffers grow and are reused; offscreen targets come
//!   from a pool keyed on size and reaped after sixty idle frames.
//! * Build state lazily. Every shader and state object exists from device
//!   creation, so the render path cannot fail on a missing one.
//! * Panic on a surface problem. A minimised window, an occluded one and a
//!   removed device are all reported as values.
//!
//! ## Multisampling
//!
//! With MSAA on, every target is drawn through a multisampled twin and
//! resolved lazily: only when something is about to *read* it (a layer being
//! composited or blurred, a backdrop being copied) and once for the surface at
//! the end of the frame. Quads and glyphs are antialiased analytically and gain
//! nothing from it; tessellated paths have hard triangle edges and are the
//! reason it exists.

use crate::buffer::{BufferKind, DynamicBuffer};
use crate::device::{DeviceKind, Gpu};
use crate::pipeline::{PipelineKind, Pipelines};
use crate::shader;
use crate::surface::Surface;
use crate::target::{LAYER_FORMAT, PoolKind, SURFACE_VIEW, Target, TargetPool, create_target};
use crate::texture::TextureStore;
use spherekit_core::{Color, DevicePx, InitError, RenderError, Size, SurfaceError, TextureId};
use spherekit_render::{
    BackendCapabilities, Batch, BatchKind, CompiledFrame, Composite, Filter, FrameHandle,
    FrameStats, GlyphInstance, GpuClip, GpuGradient, GpuTransform, MeshVertex, QuadInstance,
    RenderTarget, RendererBackend, SurfaceConfig,
};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
    D3D11_VIEWPORT, ID3D11Buffer, ID3D11DeviceContext, ID3D11RenderTargetView,
    ID3D11ShaderResourceView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R32_UINT, DXGI_SAMPLE_DESC,
};

/// Uniforms bound once per target. Mirrors `PassUniforms` in `common.hlsl`.
///
/// Larger than [`spherekit_render::FrameUniforms`] because the target origin is
/// per target: an offscreen layer is allocated only as large as its content,
/// so its coordinate space is shifted.
#[derive(Copy, Clone, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct PassUniforms {
    viewport: [f32; 2],
    scale_factor: f32,
    time: f32,
    target_origin: [f32; 2],
    _pad: [f32; 2],
}

/// Uniforms for one layer composite. Mirrors `CompositeParams`.
#[derive(Copy, Clone, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct CompositeUniforms {
    dest: [f32; 4],
    opacity: f32,
    blend: u32,
    saturation: f32,
    _pad: f32,
}

/// Uniforms for one separable Gaussian blur pass. Mirrors `BlurParams`.
#[derive(Copy, Clone, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct BlurUniforms {
    texel: [f32; 2],
    direction: [f32; 2],
    sigma: f32,
    radius: i32,
    _pad: [f32; 2],
}

/// Scratch targets for one filter operation.
#[derive(Copy, Clone, Debug)]
struct FilterWork {
    /// What the horizontal pass reads: the layer itself for `Blur`, a copy of
    /// the destination region for `BackdropBlur`.
    source: usize,
    horizontal: usize,
    vertical: usize,
    h_uniform: BlurUniforms,
    v_uniform: BlurUniforms,
    /// Whether the blurred result replaces the layer being composited.
    /// Backdrop blur keeps the layer and composites the blurred backdrop first.
    replace_source: bool,
    /// Destination region to copy for a backdrop, in device pixels.
    region: Option<(u32, u32, u32, u32)>,
}

/// Per-frame bookkeeping for the targets a [`CompiledFrame`] names.
struct FrameTargets {
    /// Pool slot of every non-surface target.
    slots: Vec<Option<usize>>,
    /// Whether the target has been written this frame. The first write clears
    /// it; see [`D3D11Renderer::touch`].
    initialized: Vec<bool>,
    /// Whether the multisampled twin holds samples not yet resolved.
    dirty: Vec<bool>,
}

/// A Direct3D 11 renderer for one window, or for an internal texture.
///
/// Presentation goes through a DirectComposition visual, so transparent
/// windows composite with what is behind them rather than over black.
pub struct D3D11Renderer {
    gpu: Gpu,
    surface: Surface,
    pipelines: Pipelines,
    textures: TextureStore,

    quad_buffer: DynamicBuffer,
    glyph_buffer: DynamicBuffer,
    mesh_vertex_buffer: DynamicBuffer,
    mesh_index_buffer: DynamicBuffer,
    transform_buffer: DynamicBuffer,
    clip_buffer: DynamicBuffer,
    gradient_buffer: DynamicBuffer,
    pass_constants: DynamicBuffer,
    composite_constants: DynamicBuffer,
    blur_constants: DynamicBuffer,

    pool: TargetPool,
    /// Active multisample count. `1` means MSAA is off.
    samples: u32,
    /// The multisampled twin of the surface, and the size it was made for.
    surface_msaa: Option<(Target, (u32, u32))>,
    capabilities: BackendCapabilities,

    frame_index: u64,
    frame_open: bool,
    last_stats: FrameStats,
    shader_load: std::time::Duration,
}

impl D3D11Renderer {
    /// Creates a renderer presenting into `window`.
    ///
    /// Only a Win32 window handle is accepted. Only a hardware device is: when
    /// there is none at feature level 11_0 or better this returns
    /// [`InitError::NoAdapter`] rather than falling back to WARP, because the
    /// caller's fallback — the Skia CPU backend — is the better software path.
    pub fn new(
        window: &impl raw_window_handle::HasWindowHandle,
        config: SurfaceConfig,
    ) -> Result<Self, InitError> {
        let handle = window.window_handle().map_err(|e| InitError::Surface(e.to_string()))?;
        let hwnd = match handle.as_raw() {
            raw_window_handle::RawWindowHandle::Win32(h) => HWND(h.hwnd.get() as *mut _),
            other => {
                return Err(InitError::Surface(format!(
                    "Direct3D 11 needs a Win32 window handle, got {other:?}"
                )));
            }
        };
        let gpu = Gpu::create(DeviceKind::Hardware)?;
        let surface = Surface::window(&gpu, hwnd, &config)
            .map_err(|e| InitError::Surface(format!("DirectComposition swap chain: {e}")))?;
        Self::build(gpu, surface, config)
    }

    /// Creates a renderer with no window, drawing into an internal texture
    /// that [`D3D11Renderer::read_pixels`] reads back.
    ///
    /// Uses WARP when there is no hardware device, so tests run anywhere
    /// Windows does.
    pub fn new_offscreen(config: SurfaceConfig) -> Result<Self, InitError> {
        let gpu = Gpu::create(DeviceKind::HardwareOrWarp)?;
        let surface = Surface::offscreen(&gpu, &config)
            .map_err(|e| InitError::Surface(format!("offscreen target: {e}")))?;
        Self::build(gpu, surface, config)
    }

    fn build(gpu: Gpu, surface: Surface, config: SurfaceConfig) -> Result<Self, InitError> {
        let device_error = |e: RenderError| InitError::DeviceCreation(e.to_string());
        let shaders = shader::load()?;
        let pipelines = Pipelines::new(&gpu.device, &shaders)?;
        let textures = TextureStore::new(&gpu.device, &gpu.context).map_err(device_error)?;

        let max_msaa = max_sample_count(&gpu);
        let samples = choose_samples(config.msaa_samples, max_msaa);
        if samples != config.msaa_samples.max(1) {
            tracing::info!(
                target: "spherekit_d3d11",
                requested = config.msaa_samples,
                using = samples,
                "multisample count reduced to what the adapter supports"
            );
        }
        let capabilities = BackendCapabilities {
            max_texture_size: D3D11_REQ_TEXTURE2D_U_OR_V_DIMENSION,
            // Direct3D 11 has timestamp queries, but nothing here reads them
            // yet, so `FrameStats::gpu_ms` is always `None`. Saying so is more
            // useful than advertising a number that never arrives.
            timestamp_queries: false,
            offscreen_targets: true,
            compute: true,
            // Part of feature level 10_0 and therefore of every device here.
            dual_source_blending: true,
            max_instances_per_draw: u32::MAX,
            max_msaa_samples: max_msaa,
        };

        let mut renderer = Self {
            quad_buffer: DynamicBuffer::new(BufferKind::Vertex, 64 * 1024),
            glyph_buffer: DynamicBuffer::new(BufferKind::Vertex, 64 * 1024),
            mesh_vertex_buffer: DynamicBuffer::new(BufferKind::Vertex, 32 * 1024),
            mesh_index_buffer: DynamicBuffer::new(BufferKind::Index, 16 * 1024),
            transform_buffer: DynamicBuffer::new(
                BufferKind::Structured(size_of::<GpuTransform>() as u32),
                8 * 1024,
            ),
            clip_buffer: DynamicBuffer::new(
                BufferKind::Structured(size_of::<GpuClip>() as u32),
                8 * 1024,
            ),
            gradient_buffer: DynamicBuffer::new(
                BufferKind::Structured(size_of::<GpuGradient>() as u32),
                8 * 1024,
            ),
            pass_constants: DynamicBuffer::new(BufferKind::Constant, 0),
            composite_constants: DynamicBuffer::new(BufferKind::Constant, 0),
            blur_constants: DynamicBuffer::new(BufferKind::Constant, 0),
            pool: TargetPool::default(),
            samples,
            surface_msaa: None,
            capabilities,
            frame_index: 0,
            frame_open: false,
            last_stats: FrameStats::default(),
            shader_load: shaders.elapsed,
            gpu,
            surface,
            pipelines,
            textures,
        };
        // Constant buffers exist from the start so the frame can bind all three
        // unconditionally.
        let (device, context) = (renderer.gpu.device.clone(), renderer.gpu.context.clone());
        renderer
            .pass_constants
            .write(&device, &context, bytemuck::bytes_of(&PassUniforms::default()))
            .map_err(device_error)?;
        renderer
            .composite_constants
            .write(&device, &context, bytemuck::bytes_of(&CompositeUniforms::default()))
            .map_err(device_error)?;
        renderer
            .blur_constants
            .write(&device, &context, bytemuck::bytes_of(&BlurUniforms::default()))
            .map_err(device_error)?;
        Ok(renderer)
    }

    /// The multisample count in use. `1` means MSAA is off.
    #[inline]
    pub fn sample_count(&self) -> u32 {
        self.samples
    }

    /// Statistics from the most recently rendered frame.
    #[inline]
    pub fn last_stats(&self) -> FrameStats {
        self.last_stats
    }

    /// How long obtaining shader bytecode took at construction: zero when the
    /// build precompiled it, the compiler's time otherwise.
    #[inline]
    pub fn shader_load_time(&self) -> std::time::Duration {
        self.shader_load
    }

    /// The last rendered frame as tightly packed RGBA8, row-major, top row
    /// first.
    ///
    /// Bytes are exactly what the target stores: sRGB-encoded colour,
    /// **premultiplied** by alpha (the renderer composites premultiplied, and
    /// an opaque frame has alpha 255 everywhere so the distinction vanishes).
    ///
    /// Offscreen renderers can be read at any time after a frame. A windowed
    /// renderer can only be read between [`RendererBackend::render`] and
    /// [`RendererBackend::end_frame`]: once presented, a flip-model back buffer
    /// belongs to the compositor and its contents are undefined.
    pub fn read_pixels(&mut self) -> Result<Vec<u8>, RenderError> {
        if self.surface.is_window() && !self.frame_open {
            return Err(RenderError::Backend(
                "a window's pixels can only be read between render and end_frame".into(),
            ));
        }
        let texture = self.surface.texture().ok_or(RenderError::Surface(SurfaceError::Lost))?;
        let (width, height) = self.surface.size();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let bytes = width as u64 * height as u64 * 4;
        let mut staging = None;
        // SAFETY: `desc` is a valid staging descriptor.
        unsafe { self.gpu.device.CreateTexture2D(&desc, None, Some(&mut staging)) }
            .map_err(|_| RenderError::OutOfMemory { what: "readback texture", bytes })?;
        let staging: ID3D11Texture2D = staging.expect("CreateTexture2D succeeded");
        let context = &self.gpu.context;
        let mut out = vec![0u8; bytes as usize];
        // SAFETY: both textures are the same size and in the same typeless
        // family; the mapped pointer is valid for `RowPitch * height` bytes
        // until `Unmap`.
        unsafe {
            context.CopyResource(&staging, texture);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| RenderError::Backend(format!("mapping the readback: {e}")))?;
            let row_bytes = width as usize * 4;
            for y in 0..height as usize {
                let src = core::slice::from_raw_parts(
                    mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize),
                    row_bytes,
                );
                let dst = &mut out[y * row_bytes..(y + 1) * row_bytes];
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    // BGRA in memory, RGBA out.
                    d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
                }
            }
            context.Unmap(&staging, 0);
        }
        Ok(out)
    }

    // ------------------------------------------------------------ uploads

    fn upload(&mut self, frame: &CompiledFrame) -> Result<u64, RenderError> {
        let (device, context) = (&self.gpu.device, &self.gpu.context);
        // Every table gets at least one element: a frame with no gradients is
        // normal, and a zero-sized buffer cannot be created.
        let identity = [GpuTransform::IDENTITY];
        let transforms: &[GpuTransform] =
            if frame.transforms.is_empty() { &identity } else { &frame.transforms };
        let infinite = [GpuClip::INFINITE];
        let clips: &[GpuClip] = if frame.clips.is_empty() { &infinite } else { &frame.clips };
        let no_gradient = [GpuGradient::default()];
        let gradients: &[GpuGradient] =
            if frame.gradients.is_empty() { &no_gradient } else { &frame.gradients };

        let mut bytes = 0u64;
        let mut put = |buffer: &mut DynamicBuffer, data: &[u8]| -> Result<(), RenderError> {
            buffer.write(device, context, data)?;
            bytes += data.len() as u64;
            Ok(())
        };
        put(&mut self.transform_buffer, bytemuck::cast_slice(transforms))?;
        put(&mut self.clip_buffer, bytemuck::cast_slice(clips))?;
        put(&mut self.gradient_buffer, bytemuck::cast_slice(gradients))?;
        if !frame.quads.is_empty() {
            put(&mut self.quad_buffer, bytemuck::cast_slice(&frame.quads))?;
        }
        if !frame.glyphs.is_empty() {
            put(&mut self.glyph_buffer, bytemuck::cast_slice(&frame.glyphs))?;
        }
        if !frame.mesh_vertices.is_empty() && !frame.mesh_indices.is_empty() {
            put(&mut self.mesh_vertex_buffer, bytemuck::cast_slice(&frame.mesh_vertices))?;
            put(&mut self.mesh_index_buffer, bytemuck::cast_slice(&frame.mesh_indices))?;
        }
        Ok(bytes)
    }

    /// Ensures the surface's multisampled twin matches the surface size.
    fn ensure_surface_msaa(&mut self) -> Result<(), RenderError> {
        if self.samples <= 1 {
            self.surface_msaa = None;
            return Ok(());
        }
        let size = self.surface.size();
        if self.surface_msaa.as_ref().is_some_and(|(_, s)| *s == size) {
            return Ok(());
        }
        self.surface_msaa = None;
        // Shader-resource bound: the surface is resolved by a pixel shader; see
        // `ps_resolve` in `blur.hlsl` for why `ResolveSubresource` cannot.
        let t = create_target(
            &self.gpu.device,
            size.0,
            size.1,
            SURFACE_VIEW,
            SURFACE_VIEW,
            self.samples,
            true,
        )?;
        self.surface_msaa = Some((t, size));
        Ok(())
    }

    // ------------------------------------------------------------ targets

    /// The view draws into for frame target `t`: the multisampled twin when
    /// there is one.
    fn draw_view(&self, targets: &FrameTargets, t: usize) -> Option<ID3D11RenderTargetView> {
        match targets.slots.get(t).copied().flatten() {
            None => match &self.surface_msaa {
                Some((msaa, _)) => Some(msaa.rtv.clone()),
                None => self.surface.rtv().cloned(),
            },
            Some(slot) => {
                let p = &self.pool.targets[slot];
                Some(p.msaa.as_ref().unwrap_or(&p.resolved).rtv.clone())
            }
        }
    }

    /// The attachment size of frame target `t`.
    fn target_size(&self, targets: &FrameTargets, t: usize) -> (u32, u32) {
        match targets.slots.get(t).copied().flatten() {
            None => self.surface.size(),
            Some(slot) => self.pool.targets[slot].size,
        }
    }

    /// Binds frame target `t` for drawing: render target, viewport, its pass
    /// uniforms, and clears it if this is the first write of the frame.
    fn bind_target(
        &mut self,
        targets: &mut FrameTargets,
        uniforms: &[PassUniforms],
        t: usize,
        clear: Color,
    ) -> Result<ID3D11RenderTargetView, RenderError> {
        let context = self.gpu.context.clone();
        let rtv = self.draw_view(targets, t).ok_or(RenderError::Surface(SurfaceError::Lost))?;
        let (w, h) = self.target_size(targets, t);
        // SAFETY: plain state-setting calls with live objects.
        unsafe {
            // Unbind sampled textures first: a texture still bound as a shader
            // resource is forcibly unbound (with a debug-layer warning) when it
            // becomes a render target, and that texture is often exactly the
            // one the previous composite or blur read.
            context.PSSetShaderResources(3, Some(&[None, None]));
            context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            context.RSSetViewports(Some(&[viewport(w, h)]));
        }
        let u = uniforms.get(t).copied().unwrap_or_default();
        self.pass_constants.write(&self.gpu.device, &context, bytemuck::bytes_of(&u))?;
        self.touch(targets, t, &rtv, clear);
        Ok(rtv)
    }

    /// Clears target `t` the first time it is written this frame.
    ///
    /// The batch compiler marks the first pass on each target with
    /// [`spherekit_render::Pass::clear`], emitting an empty clearing pass for a
    /// target that is composited into before it has content of its own. Every
    /// pass binds its target through here, so such an empty pass clears it.
    ///
    /// Keying the clear on "first write" rather than on the flag alone is a
    /// belt-and-braces choice: an older compiler marked the surface's first
    /// *own* pass as clearing even when a layer had already composited into
    /// it, which erased the layer. Either way a target is cleared exactly once,
    /// before anything lands in it.
    fn touch(
        &self,
        targets: &mut FrameTargets,
        t: usize,
        rtv: &ID3D11RenderTargetView,
        clear: Color,
    ) {
        if targets.initialized[t] {
            return;
        }
        targets.initialized[t] = true;
        targets.dirty[t] = true;
        // The surface clears to the caller's colour, in linear premultiplied
        // form because the view is sRGB and encodes on write. Offscreen layers
        // always start transparent, or a pooled texture's previous contents
        // bleed through the composite.
        let colour = if targets.slots[t].is_none() {
            let l = clear.to_linear();
            [l.r, l.g, l.b, l.a]
        } else {
            [0.0; 4]
        };
        // SAFETY: plain call on a live view.
        unsafe { self.gpu.context.ClearRenderTargetView(rtv, &colour) };
    }

    /// Resolves target `t`'s multisampled twin into its sampleable texture,
    /// if it holds anything unresolved.
    fn resolve(&mut self, targets: &mut FrameTargets, t: usize) -> Result<(), RenderError> {
        if self.samples <= 1 || !targets.dirty[t] {
            return Ok(());
        }
        targets.dirty[t] = false;
        let context = self.gpu.context.clone();
        match targets.slots[t] {
            Some(slot) => {
                let p = &self.pool.targets[slot];
                if let Some(msaa) = &p.msaa {
                    // SAFETY: same size, same typed format, one sample vs many.
                    unsafe {
                        context.ResolveSubresource(
                            &p.resolved.texture,
                            0,
                            &msaa.texture,
                            0,
                            LAYER_FORMAT,
                        )
                    };
                }
            }
            None => {
                let Some((msaa, _)) = &self.surface_msaa else { return Ok(()) };
                let srv = msaa.srv.clone();
                let rtv =
                    self.surface.rtv().cloned().ok_or(RenderError::Surface(SurfaceError::Lost))?;
                let (w, h) = self.surface.size();
                self.pipelines.bind(&context, PipelineKind::Resolve);
                // SAFETY: plain state-setting and draw calls with live objects.
                unsafe {
                    context.PSSetShaderResources(3, Some(&[None, None]));
                    context.OMSetRenderTargets(Some(&[Some(rtv)]), None);
                    context.RSSetViewports(Some(&[viewport(w, h)]));
                    context.RSSetScissorRects(Some(&[full_rect(w, h)]));
                    context.PSSetShaderResources(4, Some(&[srv]));
                    context.Draw(3, 0);
                    context.PSSetShaderResources(4, Some(&[None]));
                }
            }
        }
        Ok(())
    }

    /// The single-sample texture of frame target `t`.
    fn resolved_texture(&self, targets: &FrameTargets, t: usize) -> Option<ID3D11Texture2D> {
        match targets.slots[t] {
            Some(slot) => Some(self.pool.targets[slot].resolved.texture.clone()),
            None => self.surface.texture().cloned(),
        }
    }

    // -------------------------------------------------------------- frame

    fn render_frame(&mut self, compiled: &CompiledFrame, clear: Color) -> Result<(), RenderError> {
        let device = self.gpu.device.clone();
        let context = self.gpu.context.clone();
        let mut bytes = self.upload(compiled)?;
        self.ensure_surface_msaa()?;

        // Uniforms are a property of the target, not of the pass: every pass
        // and every composite into one target sees the same viewport and
        // origin.
        let mut uniforms: Vec<PassUniforms> = compiled
            .targets
            .iter()
            .map(|t| PassUniforms {
                viewport: [
                    t.size.width.as_u32().max(1) as f32,
                    t.size.height.as_u32().max(1) as f32,
                ],
                scale_factor: compiled.uniforms.scale_factor,
                time: compiled.uniforms.time,
                target_origin: [t.origin.x.get(), t.origin.y.get()],
                _pad: [0.0; 2],
            })
            .collect();
        if uniforms.is_empty() {
            // A default-constructed frame names no targets at all; it still
            // clears the surface below, which needs target 0 to exist.
            let (w, h) = self.surface.size();
            uniforms.push(PassUniforms {
                viewport: [w as f32, h as f32],
                scale_factor: compiled.uniforms.scale_factor.max(1e-3),
                ..Default::default()
            });
        }
        bytes += (uniforms.len() * size_of::<PassUniforms>()) as u64;

        // Reserve an offscreen texture for every non-surface target before any
        // pass runs, so nothing allocates mid-frame.
        let target_count = uniforms.len();
        let mut targets = FrameTargets {
            slots: Vec::with_capacity(target_count),
            initialized: vec![false; target_count],
            dirty: vec![false; target_count],
        };
        for t in &compiled.targets {
            targets.slots.push(if t.is_surface {
                None
            } else {
                Some(self.pool.acquire(
                    &device,
                    self.frame_index,
                    t.size.width.as_u32(),
                    t.size.height.as_u32(),
                    PoolKind::Layer,
                    self.samples,
                )?)
            });
        }
        if targets.slots.is_empty() {
            targets.slots.push(None);
        }

        // Filter scratch targets, likewise reserved up front: a blur target
        // must never alias its own source.
        let scale = compiled.uniforms.scale_factor;
        let mut filter_work: Vec<Option<FilterWork>> = vec![None; compiled.passes.len()];
        for (pass_index, pass) in compiled.passes.iter().enumerate() {
            let Some(composite) = &pass.composite else { continue };
            let Some(filter) = composite.filter else { continue };
            let Some(source_slot) = targets.slots.get(composite.source as usize).copied().flatten()
            else {
                continue;
            };
            match filter {
                Filter::Blur { sigma } => {
                    let (w, h) = self.pool.targets[source_slot].size;
                    let horizontal = self.acquire_filter(w, h, PoolKind::Filter)?;
                    let vertical = self.acquire_filter(w, h, PoolKind::Filter)?;
                    filter_work[pass_index] = Some(FilterWork {
                        source: source_slot,
                        horizontal,
                        vertical,
                        h_uniform: make_blur_uniform(sigma, scale, w, h, [1.0, 0.0]),
                        v_uniform: make_blur_uniform(sigma, scale, w, h, [0.0, 1.0]),
                        replace_source: true,
                        region: None,
                    });
                }
                Filter::BackdropBlur { sigma } => {
                    let Some(destination) = compiled.targets.get(composite.destination as usize)
                    else {
                        continue;
                    };
                    let Some((x, y, w, h)) = target_region(composite.bounds, destination, scale)
                    else {
                        continue;
                    };
                    // The copy has to match the destination's storage family:
                    // the surface is BGRA, layers are RGBA.
                    let copy_kind = if destination.is_surface {
                        PoolKind::SurfaceCopy
                    } else {
                        PoolKind::Filter
                    };
                    let source = self.acquire_filter(w, h, copy_kind)?;
                    let horizontal = self.acquire_filter(w, h, PoolKind::Filter)?;
                    let vertical = self.acquire_filter(w, h, PoolKind::Filter)?;
                    filter_work[pass_index] = Some(FilterWork {
                        source,
                        horizontal,
                        vertical,
                        h_uniform: make_blur_uniform(sigma, scale, w, h, [1.0, 0.0]),
                        v_uniform: make_blur_uniform(sigma, scale, w, h, [0.0, 1.0]),
                        replace_source: false,
                        region: Some((x, y, w, h)),
                    });
                }
                // The wgpu backend implemented neither as a filter pass:
                // saturation rides in the composite uniforms, and a colour
                // matrix was never applied.
                Filter::ColorMatrix(_) | Filter::Saturate(_) => {}
            }
        }

        // Frame-wide bindings. `ClearState` on resize wipes these, so they are
        // rebound every frame rather than once.
        let constants = [
            self.pass_constants.buffer().cloned(),
            self.composite_constants.buffer().cloned(),
            self.blur_constants.buffer().cloned(),
        ];
        let tables = [
            self.transform_buffer.srv().cloned(),
            self.clip_buffer.srv().cloned(),
            self.gradient_buffer.srv().cloned(),
        ];
        // SAFETY: plain state-setting calls with live objects.
        unsafe {
            context.RSSetState(&self.pipelines.rasterizer);
            context.PSSetSamplers(0, Some(&[Some(self.pipelines.linear_sampler.clone())]));
            context.VSSetConstantBuffers(0, Some(&constants));
            context.PSSetConstantBuffers(0, Some(&constants));
            context.VSSetShaderResources(0, Some(&tables));
            context.PSSetShaderResources(0, Some(&tables));
        }

        let mut draw_calls = 0u32;
        let mut pipeline_switches = 0u32;

        for (pass_index, pass) in compiled.passes.iter().enumerate() {
            let t = pass.target as usize;
            if t >= targets.slots.len() {
                continue;
            }
            self.bind_target(&mut targets, &uniforms, t, clear)?;
            let (target_w, target_h) = compiled
                .targets
                .get(t)
                .map(|t| (t.size.width.as_u32().max(1), t.size.height.as_u32().max(1)))
                .unwrap_or_else(|| self.surface.size());

            let mut last_kind: Option<PipelineKind> = None;
            for bi in pass.batches.clone() {
                let Some(batch) = compiled.batches.get(bi as usize) else { continue };
                if batch.range.is_empty() {
                    continue;
                }
                let texture: ID3D11ShaderResourceView = match batch.kind {
                    BatchKind::Quad => self.textures.white_srv().clone(),
                    BatchKind::Image { texture } => {
                        self.textures.srv(texture).unwrap_or(self.textures.white_srv()).clone()
                    }
                    // A page nobody has bound yet is skipped, not an error.
                    BatchKind::Glyph { page, .. } => match self.textures.atlas_srv(page) {
                        Some(srv) => srv.clone(),
                        None => continue,
                    },
                    BatchKind::Mesh { texture } => texture
                        .and_then(|t| self.textures.srv(t))
                        .unwrap_or(self.textures.white_srv())
                        .clone(),
                };
                let kind = pipeline_kind_for(batch);
                if last_kind != Some(kind) {
                    self.pipelines.bind(&context, kind);
                    self.bind_geometry(&context, kind);
                    pipeline_switches += 1;
                    last_kind = Some(kind);
                }

                let (sx, sy, sw, sh) =
                    clamp_scissor(batch, compiled, pass.target, target_w, target_h);
                if sw == 0 || sh == 0 {
                    continue;
                }
                let start = batch.range.start;
                let count = batch.range.end - batch.range.start;
                // SAFETY: plain state-setting and draw calls; the ranges index
                // buffers uploaded from this same frame.
                unsafe {
                    context.RSSetScissorRects(Some(&[RECT {
                        left: sx as i32,
                        top: sy as i32,
                        right: (sx + sw) as i32,
                        bottom: (sy + sh) as i32,
                    }]));
                    context.PSSetShaderResources(3, Some(&[Some(texture)]));
                    match batch.kind {
                        BatchKind::Mesh { .. } => {
                            context.DrawIndexed(count, start, batch.base_vertex as i32)
                        }
                        _ => context.DrawInstanced(4, count, 0, start),
                    }
                }
                targets.dirty[t] = true;
                draw_calls += 1;
            }

            if let Some(work) = filter_work[pass_index] {
                let destination = pass.composite.as_ref().map(|c| c.destination as usize);
                if let (Some((x, y, w, h)), Some(dest)) = (work.region, destination) {
                    // The backdrop is whatever the destination holds right now,
                    // so its pending samples must land first.
                    self.resolve(&mut targets, dest)?;
                    let from = self
                        .resolved_texture(&targets, dest)
                        .ok_or(RenderError::Surface(SurfaceError::Lost))?;
                    let to = self.pool.targets[work.source].resolved.texture.clone();
                    let region = D3D11_BOX {
                        left: x,
                        top: y,
                        front: 0,
                        right: x + w,
                        bottom: y + h,
                        back: 1,
                    };
                    // SAFETY: the region lies inside the destination (clamped by
                    // `target_region`) and the copy target is exactly its size,
                    // in the same typeless family.
                    unsafe {
                        context.CopySubresourceRegion(&to, 0, 0, 0, 0, &from, 0, Some(&region))
                    };
                } else {
                    self.resolve(&mut targets, t)?;
                }

                for (source, target, uniform) in [
                    (work.source, work.horizontal, work.h_uniform),
                    (work.horizontal, work.vertical, work.v_uniform),
                ] {
                    let src = self.pool.targets[source].resolved.srv.clone();
                    let dst = self.pool.targets[target].resolved.rtv.clone();
                    let (w, h) = self.pool.targets[target].size;
                    self.blur_constants.write(&device, &context, bytemuck::bytes_of(&uniform))?;
                    bytes += size_of::<BlurUniforms>() as u64;
                    self.pipelines.bind(&context, PipelineKind::Blur);
                    // SAFETY: plain state-setting and draw calls; source and
                    // target are distinct pool entries.
                    unsafe {
                        context.PSSetShaderResources(3, Some(&[None, None]));
                        context.OMSetRenderTargets(Some(&[Some(dst)]), None);
                        context.RSSetViewports(Some(&[viewport(w, h)]));
                        context.RSSetScissorRects(Some(&[full_rect(w, h)]));
                        context.PSSetShaderResources(3, Some(&[src]));
                        context.Draw(3, 0);
                    }
                    draw_calls += 1;
                    pipeline_switches += 1;
                }
            }

            if let Some(c) = &pass.composite {
                let source = c.source as usize;
                let dest = c.destination as usize;
                if source >= targets.slots.len() || dest >= targets.slots.len() {
                    continue;
                }
                let Some(original) = targets.slots[source] else { continue };
                self.resolve(&mut targets, source)?;
                let work = filter_work[pass_index];
                let backdrop = work.filter(|w| !w.replace_source).map(|w| w.vertical);
                let layer =
                    work.filter(|w| w.replace_source).map(|w| w.vertical).unwrap_or(original);

                let u = composite_uniform(c);
                self.composite_constants.write(&device, &context, bytemuck::bytes_of(&u))?;
                bytes += size_of::<CompositeUniforms>() as u64;

                // Backdrop blur is a destination effect: first lay the blurred
                // snapshot of the destination region down, then the layer's own
                // content over it, both through the same composite uniforms.
                for slot in [backdrop, Some(layer)].into_iter().flatten() {
                    self.bind_target(&mut targets, &uniforms, dest, clear)?;
                    let (w, h) = self.target_size(&targets, dest);
                    let srv = self.pool.targets[slot].resolved.srv.clone();
                    self.pipelines.bind(&context, PipelineKind::Composite);
                    // SAFETY: plain state-setting and draw calls.
                    unsafe {
                        context.RSSetScissorRects(Some(&[full_rect(w, h)]));
                        context.PSSetShaderResources(3, Some(&[srv]));
                        context.Draw(4, 0);
                    }
                    targets.dirty[dest] = true;
                    draw_calls += 1;
                    pipeline_switches += 1;
                }
            }
        }

        // A frame that never touched the surface — no passes at all, or only
        // layers that composite nowhere — still shows the clear colour rather
        // than whatever the buffer held.
        if !targets.initialized[0] {
            self.bind_target(&mut targets, &uniforms, 0, clear)?;
        }
        self.resolve(&mut targets, 0)?;
        // SAFETY: plain state-setting call; leaves no view of a swap chain
        // buffer bound past the frame.
        unsafe {
            context.PSSetShaderResources(3, Some(&[None, None]));
            context.OMSetRenderTargets(None, None);
        }

        self.last_stats = FrameStats {
            cpu_ms: 0.0,
            gpu_ms: None,
            draw_calls,
            pipeline_switches,
            quads: compiled.stats.quads,
            glyphs: compiled.stats.glyphs,
            triangles: compiled.stats.triangles,
            bytes_uploaded: bytes,
            layers: compiled.stats.layers,
        };
        Ok(())
    }

    fn acquire_filter(&mut self, w: u32, h: u32, kind: PoolKind) -> Result<usize, RenderError> {
        self.pool.acquire(&self.gpu.device, self.frame_index, w, h, kind, self.samples)
    }

    /// Binds the vertex (and index) buffer a pipeline draws from.
    fn bind_geometry(&self, context: &ID3D11DeviceContext, kind: PipelineKind) {
        let (buffer, stride): (Option<&ID3D11Buffer>, u32) = match kind {
            PipelineKind::Quad | PipelineKind::Image => {
                (self.quad_buffer.buffer(), size_of::<QuadInstance>() as u32)
            }
            PipelineKind::Text | PipelineKind::TextSubpixel => {
                (self.glyph_buffer.buffer(), size_of::<GlyphInstance>() as u32)
            }
            PipelineKind::Mesh | PipelineKind::MeshTextured => {
                (self.mesh_vertex_buffer.buffer(), size_of::<MeshVertex>() as u32)
            }
            _ => (None, 0),
        };
        let buffers = [buffer.cloned()];
        let offset = 0u32;
        // SAFETY: the arrays outlive the call; a `None` buffer unbinds the slot.
        unsafe {
            context.IASetVertexBuffers(0, 1, Some(buffers.as_ptr()), Some(&stride), Some(&offset));
            if matches!(kind, PipelineKind::Mesh | PipelineKind::MeshTextured) {
                context.IASetIndexBuffer(self.mesh_index_buffer.buffer(), DXGI_FORMAT_R32_UINT, 0);
            }
        }
    }
}

impl RendererBackend for D3D11Renderer {
    fn adapter_name(&self) -> &str {
        &self.gpu.adapter_name
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.capabilities
    }

    fn configure_surface(&mut self, config: SurfaceConfig) -> Result<(), RenderError> {
        // A minimised window legitimately reports zero. A zero-sized swap chain
        // is invalid, so keep the last good configuration and carry on.
        if config.size.width.as_u32() == 0 || config.size.height.as_u32() == 0 {
            return Ok(());
        }
        let (w, h) = (config.size.width.as_u32(), config.size.height.as_u32());
        if self.surface.size() != (w, h) {
            // `ResizeBuffers` fails while anything still references a buffer,
            // and a view bound to the pipeline counts. Flushing lets the
            // runtime release what it was deferring.
            // SAFETY: plain calls on the immediate context.
            unsafe {
                self.gpu.context.ClearState();
                self.gpu.context.Flush();
            }
        }
        self.surface.reconfigure(&self.gpu, &config)?;
        let samples = choose_samples(config.msaa_samples, self.capabilities.max_msaa_samples);
        if samples != self.samples {
            self.samples = samples;
            self.pool.clear();
            self.surface_msaa = None;
        }
        self.ensure_surface_msaa()
    }

    fn begin_frame(&mut self) -> Result<FrameHandle, SurfaceError> {
        // SAFETY: plain COM call.
        if let Err(e) = unsafe { self.gpu.device.GetDeviceRemovedReason() } {
            tracing::error!(target: "spherekit_d3d11", "device removed: {e}");
            return Err(SurfaceError::DeviceLost);
        }
        self.surface.acquire()?;
        self.frame_index += 1;
        self.frame_open = true;
        let (w, h) = self.surface.size();
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
        if !self.frame_open {
            return Err(RenderError::Surface(SurfaceError::Lost));
        }
        let _ = frame;
        self.render_frame(compiled, clear)
    }

    fn end_frame(&mut self, frame: FrameHandle) -> Result<FrameStats, RenderError> {
        let _ = frame;
        self.frame_open = false;
        let presented = self.surface.present();
        self.pool.reap(self.frame_index);
        presented.map(|()| self.last_stats)
    }

    fn upload_texture(
        &mut self,
        id: Option<TextureId>,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<TextureId, RenderError> {
        let max = self.capabilities.max_texture_size;
        if width == 0 || height == 0 || width > max || height > max {
            return Err(RenderError::Backend(format!(
                "texture {width}x{height} is outside the supported range 1..={max}"
            )));
        }
        self.textures.upload(&self.gpu.device, &self.gpu.context, id, width, height, rgba)
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
        self.textures.upload_region(&self.gpu.context, id, x, y, width, height, rgba)
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
        let (sw, sh) = self.surface.size();
        let surface_msaa = if self.surface_msaa.is_some() {
            sw as u64 * sh as u64 * 4 * self.samples as u64
        } else {
            0
        };
        self.textures.memory_bytes()
            + self.quad_buffer.capacity()
            + self.glyph_buffer.capacity()
            + self.mesh_vertex_buffer.capacity()
            + self.mesh_index_buffer.capacity()
            + self.transform_buffer.capacity()
            + self.clip_buffer.capacity()
            + self.gradient_buffer.capacity()
            + self.pool.bytes(self.samples)
            + surface_msaa
    }
}

/// The highest sample count both the surface and layer formats support.
///
/// A pipeline's output is resolved into the format it was drawn in, and layers
/// and the surface share one count, so the answer is the count both support.
fn max_sample_count(gpu: &Gpu) -> u32 {
    [16u32, 8, 4, 2]
        .into_iter()
        .find(|n| {
            // SAFETY: plain query.
            let ok = |format| unsafe {
                gpu.device.CheckMultisampleQualityLevels(format, *n).is_ok_and(|q| q > 0)
            };
            ok(LAYER_FORMAT) && ok(SURFACE_VIEW)
        })
        .unwrap_or(1)
}

/// Rounds a requested sample count down to a supported one rather than
/// failing: a machine that cannot do 4x should still open a window.
fn choose_samples(requested: u32, max: u32) -> u32 {
    if requested <= 1 {
        return 1;
    }
    [16u32, 8, 4, 2].into_iter().find(|n| *n <= requested && *n <= max).unwrap_or(1)
}

fn viewport(w: u32, h: u32) -> D3D11_VIEWPORT {
    D3D11_VIEWPORT {
        TopLeftX: 0.0,
        TopLeftY: 0.0,
        Width: w as f32,
        Height: h as f32,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    }
}

fn full_rect(w: u32, h: u32) -> RECT {
    RECT { left: 0, top: 0, right: w as i32, bottom: h as i32 }
}

fn composite_uniform(c: &Composite) -> CompositeUniforms {
    CompositeUniforms {
        dest: [
            c.bounds.min_x().get(),
            c.bounds.min_y().get(),
            c.bounds.width().get(),
            c.bounds.height().get(),
        ],
        opacity: c.opacity,
        blend: c.blend as u32,
        saturation: match c.filter {
            Some(Filter::Saturate(s)) => s,
            _ => 1.0,
        },
        _pad: 0.0,
    }
}

fn make_blur_uniform(
    sigma: spherekit_core::Px,
    scale: f32,
    width: u32,
    height: u32,
    direction: [f32; 2],
) -> BlurUniforms {
    let sigma = (sigma.get().max(0.0) * scale.max(1e-3)).max(1e-3);
    let radius = (sigma * 3.0).ceil().clamp(1.0, 32.0) as i32;
    BlurUniforms {
        texel: [1.0 / width.max(1) as f32, 1.0 / height.max(1) as f32],
        direction,
        sigma,
        radius,
        _pad: [0.0; 2],
    }
}

/// Resolves a scene-space rectangle into a clamped region of a render target.
fn target_region(
    bounds: spherekit_core::Rect<spherekit_core::Px>,
    target: &RenderTarget,
    scale: f32,
) -> Option<(u32, u32, u32, u32)> {
    let scale = scale.max(1e-3);
    let origin = target.origin;
    let x0 = ((bounds.min_x().get() - origin.x.get()) * scale).floor().max(0.0) as u32;
    let y0 = ((bounds.min_y().get() - origin.y.get()) * scale).floor().max(0.0) as u32;
    let x1 = ((bounds.max_x().get() - origin.x.get()) * scale).ceil().max(0.0) as u32;
    let y1 = ((bounds.max_y().get() - origin.y.get()) * scale).ceil().max(0.0) as u32;
    let x0 = x0.min(target.size.width.as_u32());
    let y0 = y0.min(target.size.height.as_u32());
    let x1 = x1.min(target.size.width.as_u32());
    let y1 = y1.min(target.size.height.as_u32());
    let (width, height) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    (width > 0 && height > 0).then_some((x0, y0, width, height))
}

fn pipeline_kind_for(batch: &Batch) -> PipelineKind {
    match batch.kind {
        BatchKind::Quad => PipelineKind::Quad,
        BatchKind::Image { .. } => PipelineKind::Image,
        BatchKind::Glyph { subpixel: true, .. } => PipelineKind::TextSubpixel,
        BatchKind::Glyph { subpixel: false, .. } => PipelineKind::Text,
        BatchKind::Mesh { texture: None } => PipelineKind::Mesh,
        BatchKind::Mesh { texture: Some(_) } => PipelineKind::MeshTextured,
    }
}

/// Clamps a batch's scissor into its attachment.
///
/// The batch compiler works in scene-absolute device space; an offscreen
/// target's own space is shifted by its origin. Converting and clamping here,
/// once, keeps that knowledge out of the compiler, and a scissor outside the
/// attachment is never handed to the runtime.
fn clamp_scissor(
    batch: &Batch,
    frame: &CompiledFrame,
    target_index: u32,
    target_w: u32,
    target_h: u32,
) -> (u32, u32, u32, u32) {
    let scale = frame.uniforms.scale_factor.max(1e-3);
    let (ox, oy) = frame
        .targets
        .get(target_index as usize)
        .map(|t| (t.origin.x.get() * scale, t.origin.y.get() * scale))
        .unwrap_or((0.0, 0.0));

    let x0 = (batch.scissor.min_x().get() as f32 - ox).floor().max(0.0) as u32;
    let y0 = (batch.scissor.min_y().get() as f32 - oy).floor().max(0.0) as u32;
    let x1 = (batch.scissor.max_x().get() as f32 - ox).ceil().max(0.0) as u32;
    let y1 = (batch.scissor.max_y().get() as f32 - oy).ceil().max(0.0) as u32;

    let x0 = x0.min(target_w);
    let y0 = y0.min(target_h);
    let x1 = x1.min(target_w);
    let y1 = y1.min(target_h);
    (x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use spherekit_core::{Point, Rect, px, rect};

    fn frame_with_target(origin: (f32, f32), scale: f32) -> CompiledFrame {
        let mut f = CompiledFrame::default();
        f.uniforms.scale_factor = scale;
        f.targets.push(RenderTarget {
            is_surface: false,
            size: Size::new(DevicePx(100), DevicePx(100)),
            origin: Point::new(px(origin.0), px(origin.1)),
        });
        f
    }

    fn batch_with_scissor(r: Rect<DevicePx>) -> Batch {
        Batch { kind: BatchKind::Quad, scissor: r, range: 0..1, base_vertex: 0, target: 0 }
    }

    #[test]
    fn scissor_is_shifted_into_offscreen_target_space() {
        let f = frame_with_target((50.0, 20.0), 1.0);
        let b = batch_with_scissor(rect(DevicePx(60), DevicePx(30), DevicePx(10), DevicePx(10)));
        assert_eq!(clamp_scissor(&b, &f, 0, 100, 100), (10, 10, 10, 10));
    }

    #[test]
    fn scissor_origin_shift_accounts_for_dpi() {
        let f = frame_with_target((50.0, 0.0), 2.0);
        let b = batch_with_scissor(rect(DevicePx(120), DevicePx(0), DevicePx(10), DevicePx(10)));
        assert_eq!(clamp_scissor(&b, &f, 0, 200, 200).0, 20);
    }

    #[test]
    fn a_scissor_reaching_past_the_attachment_is_clamped() {
        let f = frame_with_target((0.0, 0.0), 1.0);
        let b = batch_with_scissor(rect(DevicePx(90), DevicePx(90), DevicePx(500), DevicePx(500)));
        assert_eq!(clamp_scissor(&b, &f, 0, 100, 100), (90, 90, 10, 10));
    }

    #[test]
    fn a_scissor_entirely_outside_the_attachment_collapses_to_zero() {
        let f = frame_with_target((0.0, 0.0), 1.0);
        let b = batch_with_scissor(rect(DevicePx(500), DevicePx(500), DevicePx(10), DevicePx(10)));
        let (_, _, w, h) = clamp_scissor(&b, &f, 0, 100, 100);
        assert_eq!((w, h), (0, 0));
    }

    #[test]
    fn a_negative_scissor_origin_is_clamped_to_zero() {
        let f = frame_with_target((0.0, 0.0), 1.0);
        let b =
            batch_with_scissor(rect(DevicePx(-50), DevicePx(-50), DevicePx(100), DevicePx(100)));
        assert_eq!(clamp_scissor(&b, &f, 0, 100, 100), (0, 0, 50, 50));
    }

    #[test]
    fn target_region_is_clamped_and_origin_relative() {
        let t = RenderTarget {
            is_surface: false,
            size: Size::new(DevicePx(100), DevicePx(100)),
            origin: Point::new(px(10.0), px(10.0)),
        };
        let r = target_region(rect(px(20.0), px(20.0), px(500.0), px(5.5)), &t, 1.0);
        assert_eq!(r, Some((10, 10, 90, 6)));
        assert_eq!(target_region(rect(px(500.0), px(0.0), px(5.0), px(5.0)), &t, 1.0), None);
    }

    #[test]
    fn blur_radius_covers_three_sigma_and_is_bounded() {
        let u = make_blur_uniform(px(4.0), 2.0, 100, 50, [1.0, 0.0]);
        assert_eq!(u.sigma, 8.0);
        assert_eq!(u.radius, 24);
        assert_eq!(make_blur_uniform(px(100.0), 1.0, 1, 1, [0.0, 1.0]).radius, 32);
        assert_eq!(make_blur_uniform(px(0.0), 1.0, 1, 1, [0.0, 1.0]).radius, 1);
    }

    #[test]
    fn sample_counts_round_down_to_something_supported() {
        assert_eq!(choose_samples(4, 8), 4);
        assert_eq!(choose_samples(4, 2), 2);
        assert_eq!(choose_samples(3, 8), 2);
        assert_eq!(choose_samples(1, 8), 1);
        assert_eq!(choose_samples(0, 8), 1);
        assert_eq!(choose_samples(8, 1), 1);
    }

    #[test]
    fn pipeline_kind_mapping_covers_every_batch_kind() {
        let b = |k| Batch { kind: k, scissor: Rect::ZERO, range: 0..1, base_vertex: 0, target: 0 };
        assert_eq!(pipeline_kind_for(&b(BatchKind::Quad)), PipelineKind::Quad);
        assert_eq!(
            pipeline_kind_for(&b(BatchKind::Image { texture: TextureId::new(0, 1) })),
            PipelineKind::Image
        );
        assert_eq!(
            pipeline_kind_for(&b(BatchKind::Glyph { page: 0, subpixel: false })),
            PipelineKind::Text
        );
        assert_eq!(
            pipeline_kind_for(&b(BatchKind::Glyph { page: 0, subpixel: true })),
            PipelineKind::TextSubpixel
        );
        assert_eq!(pipeline_kind_for(&b(BatchKind::Mesh { texture: None })), PipelineKind::Mesh);
        assert_eq!(
            pipeline_kind_for(&b(BatchKind::Mesh { texture: Some(TextureId::new(0, 1)) })),
            PipelineKind::MeshTextured
        );
    }

    #[test]
    fn uniform_layouts_match_the_hlsl_constant_buffers() {
        // Constant buffers are 16-byte granular; each struct must be exactly the
        // size of its `cbuffer`.
        assert_eq!(size_of::<PassUniforms>(), 32);
        assert_eq!(size_of::<CompositeUniforms>(), 32);
        assert_eq!(size_of::<BlurUniforms>(), 32);
    }
}
