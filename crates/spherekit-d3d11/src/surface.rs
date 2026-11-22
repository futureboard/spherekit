//! Where frames land: a DirectComposition swap chain, or an internal texture.
//!
//! ## Why DirectComposition
//!
//! A swap chain created for an HWND composites opaquely; the only way DXGI
//! offers per-pixel transparency is a *composition* swap chain presented
//! through a DirectComposition visual. Using that path for every window means
//! a transparent window is not a special case with its own bugs, which is what
//! it was under the wgpu backend.
//!
//! The swap chain is flip-model with two `B8G8R8A8_UNORM` buffers, rendered
//! through a `B8G8R8A8_UNORM_SRGB` view so the hardware encodes linear output
//! on write — the same arrangement as the wgpu backend's sRGB surface. Flip
//! model forbids an sRGB *buffer* format but allows the sRGB *view*.
//!
//! A transparent window must be created without a redirection bitmap
//! (`WS_EX_NOREDIRECTIONBITMAP`; in winit, `with_no_redirection_bitmap(true)`),
//! or GDI's opaque redirection surface stays underneath the visual.

use crate::device::Gpu;
use crate::target::{SURFACE_TYPELESS, SURFACE_VIEW, create_rtv, create_target};
use spherekit_core::{RenderError, SurfaceError};
use spherekit_render::{SurfaceConfig, VsyncMode};
use windows::Win32::Foundation::{
    CloseHandle, DXGI_STATUS_OCCLUDED, HANDLE, HWND, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Graphics::Direct3D11::{ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_DEVICE_HUNG, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_PRESENT,
    DXGI_PRESENT_TEST, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice, IDXGIOutput, IDXGISwapChain1, IDXGISwapChain2,
};
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use windows::core::{HRESULT, Interface};

/// How long `begin_frame` waits for the swap chain to accept another frame
/// before reporting [`SurfaceError::Timeout`]. Several refresh intervals even
/// at 24 Hz, short enough that a stalled compositor cannot freeze the caller.
const FRAME_WAIT_MS: u32 = 100;

/// The frame's destination.
pub(crate) enum Surface {
    /// A window, through DirectComposition.
    Window(WindowSurface),
    /// An internal texture, for tests and screenshots.
    Offscreen(OffscreenSurface),
}

/// A composition swap chain bound to a window.
pub(crate) struct WindowSurface {
    swapchain: IDXGISwapChain2,
    /// Signalled when the swap chain can accept another frame within the
    /// configured latency.
    waitable: HANDLE,
    // DirectComposition objects are kept alive for as long as the window shows
    // this swap chain; releasing the visual or target would unhook it.
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    buffer: Option<ID3D11Texture2D>,
    rtv: Option<ID3D11RenderTargetView>,
    size: (u32, u32),
    flags: DXGI_SWAP_CHAIN_FLAG,
    sync_interval: u32,
    /// The waitable has been consumed for the frame in flight. Waiting twice
    /// without a present in between would time out spuriously.
    waited: bool,
    /// The last present reported the window as occluded.
    occluded: bool,
}

/// An internal texture standing in for a swap chain.
pub(crate) struct OffscreenSurface {
    texture: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    size: (u32, u32),
}

/// Maps a DXGI failure onto the engine's surface errors.
pub(crate) fn surface_error(hr: HRESULT) -> SurfaceError {
    match hr {
        DXGI_ERROR_DEVICE_REMOVED | DXGI_ERROR_DEVICE_RESET | DXGI_ERROR_DEVICE_HUNG => {
            SurfaceError::DeviceLost
        }
        windows::Win32::Foundation::E_OUTOFMEMORY => SurfaceError::OutOfMemory,
        _ => SurfaceError::Lost,
    }
}

impl Surface {
    /// A composition swap chain for `hwnd`.
    pub fn window(gpu: &Gpu, hwnd: HWND, config: &SurfaceConfig) -> windows::core::Result<Self> {
        let (w, h) = clamp_size(config);
        let flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: w,
            Height: h,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            // Composition swap chains only support stretch scaling. While a
            // resize is in flight the compositor stretches the old buffer,
            // which is the least jarring thing it could do.
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: if config.transparent {
                DXGI_ALPHA_MODE_PREMULTIPLIED
            } else {
                DXGI_ALPHA_MODE_IGNORE
            },
            Flags: flags.0 as u32,
        };
        // SAFETY: plain COM calls on live interfaces with valid descriptors.
        unsafe {
            let factory = gpu.factory()?;
            let swapchain1: IDXGISwapChain1 =
                factory.CreateSwapChainForComposition(&gpu.device, &desc, None::<&IDXGIOutput>)?;
            let swapchain: IDXGISwapChain2 = swapchain1.cast()?;
            swapchain.SetMaximumFrameLatency(config.present.max_frame_latency())?;
            let waitable = swapchain.GetFrameLatencyWaitableObject();

            let dxgi: IDXGIDevice = gpu.device.cast()?;
            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swapchain1)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;

            let mut surface = WindowSurface {
                swapchain,
                waitable,
                _dcomp: dcomp,
                _target: target,
                _visual: visual,
                buffer: None,
                rtv: None,
                size: (w, h),
                flags,
                sync_interval: sync_interval(config.vsync),
                waited: false,
                occluded: false,
            };
            surface.create_views(gpu).map_err(|e| {
                windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string())
            })?;
            Ok(Surface::Window(surface))
        }
    }

    /// An internal texture.
    pub fn offscreen(gpu: &Gpu, config: &SurfaceConfig) -> Result<Self, RenderError> {
        let (w, h) = clamp_size(config);
        Ok(Surface::Offscreen(OffscreenSurface::new(gpu, w, h)?))
    }

    /// Current size in device pixels.
    pub fn size(&self) -> (u32, u32) {
        match self {
            Surface::Window(s) => s.size,
            Surface::Offscreen(s) => s.size,
        }
    }

    /// The single-sample texture the frame ends up in.
    pub fn texture(&self) -> Option<&ID3D11Texture2D> {
        match self {
            Surface::Window(s) => s.buffer.as_ref(),
            Surface::Offscreen(s) => Some(&s.texture),
        }
    }

    /// The sRGB render target view of [`Surface::texture`].
    pub fn rtv(&self) -> Option<&ID3D11RenderTargetView> {
        match self {
            Surface::Window(s) => s.rtv.as_ref(),
            Surface::Offscreen(s) => Some(&s.rtv),
        }
    }

    /// Applies a new configuration. The caller has already rejected zero
    /// sizes and unbound the old views from the pipeline.
    pub fn reconfigure(&mut self, gpu: &Gpu, config: &SurfaceConfig) -> Result<(), RenderError> {
        let (w, h) = (config.size.width.as_u32(), config.size.height.as_u32());
        match self {
            Surface::Window(s) => {
                s.sync_interval = sync_interval(config.vsync);
                // SAFETY: plain COM call.
                if let Err(e) = unsafe {
                    s.swapchain.SetMaximumFrameLatency(config.present.max_frame_latency())
                } {
                    tracing::warn!(target: "spherekit_d3d11", "SetMaximumFrameLatency: {e}");
                }
                if s.size != (w, h) {
                    // Every reference to a buffer must be gone before
                    // `ResizeBuffers`, including the views held here.
                    s.rtv = None;
                    s.buffer = None;
                    // SAFETY: the context no longer binds the buffers (the
                    // caller cleared its state) and this surface holds none.
                    unsafe { s.swapchain.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, s.flags) }
                        .map_err(|e| RenderError::Surface(surface_error(e.code())))?;
                    s.size = (w, h);
                    s.create_views(gpu)?;
                }
            }
            Surface::Offscreen(s) => {
                if s.size != (w, h) {
                    *s = OffscreenSurface::new(gpu, w, h)?;
                }
            }
        }
        Ok(())
    }

    /// Waits until the surface can accept a frame.
    pub fn acquire(&mut self) -> Result<(), SurfaceError> {
        let Surface::Window(s) = self else { return Ok(()) };
        if s.occluded {
            // A test present asks whether the window is visible again without
            // presenting anything.
            // SAFETY: plain COM call.
            let hr = unsafe { s.swapchain.Present(0, DXGI_PRESENT_TEST) };
            if hr == DXGI_STATUS_OCCLUDED {
                return Err(SurfaceError::Occluded);
            }
            if hr.is_err() {
                return Err(surface_error(hr));
            }
            s.occluded = false;
        }
        if !s.waited {
            // SAFETY: the waitable stays open for the swap chain's lifetime.
            let result = unsafe { WaitForSingleObjectEx(s.waitable, FRAME_WAIT_MS, true) };
            if result == WAIT_TIMEOUT {
                return Err(SurfaceError::Timeout);
            }
            if result != WAIT_OBJECT_0 {
                tracing::debug!(target: "spherekit_d3d11", ?result, "frame latency wait failed");
            }
            s.waited = true;
        }
        if s.rtv.is_none() {
            return Err(SurfaceError::Lost);
        }
        Ok(())
    }

    /// Presents the frame.
    pub fn present(&mut self) -> Result<(), RenderError> {
        let Surface::Window(s) = self else { return Ok(()) };
        // SAFETY: plain COM call.
        let hr = unsafe { s.swapchain.Present(s.sync_interval, DXGI_PRESENT(0)) };
        s.waited = false;
        if hr == DXGI_STATUS_OCCLUDED {
            s.occluded = true;
            return Ok(());
        }
        if hr.is_err() {
            return Err(RenderError::Surface(surface_error(hr)));
        }
        Ok(())
    }

    /// Whether this is a window surface.
    pub fn is_window(&self) -> bool {
        matches!(self, Surface::Window(_))
    }
}

impl WindowSurface {
    fn create_views(&mut self, gpu: &Gpu) -> Result<(), RenderError> {
        // SAFETY: plain COM call; in Direct3D 11 buffer 0 of a flip-model swap
        // chain always aliases the current back buffer, so one view serves
        // every frame until the next resize.
        let buffer: ID3D11Texture2D = unsafe { self.swapchain.GetBuffer(0) }
            .map_err(|e| RenderError::Surface(surface_error(e.code())))?;
        self.rtv = Some(create_rtv(&gpu.device, &buffer, SURFACE_VIEW, false)?);
        self.buffer = Some(buffer);
        Ok(())
    }
}

impl Drop for WindowSurface {
    fn drop(&mut self) {
        // SAFETY: the handle came from `GetFrameLatencyWaitableObject` and is
        // closed exactly once.
        unsafe {
            let _ = CloseHandle(self.waitable);
        }
    }
}

impl OffscreenSurface {
    fn new(gpu: &Gpu, width: u32, height: u32) -> Result<Self, RenderError> {
        // Typeless storage, so the same texture can be viewed as sRGB for
        // rendering and copied byte-for-byte like a swap chain buffer.
        let t =
            create_target(&gpu.device, width, height, SURFACE_TYPELESS, SURFACE_VIEW, 1, false)?;
        Ok(Self { texture: t.texture, rtv: t.rtv, size: (width, height) })
    }
}

fn clamp_size(config: &SurfaceConfig) -> (u32, u32) {
    // A swap chain cannot be zero-sized. A window created minimised starts at
    // one pixel and is resized when it is first shown.
    (config.size.width.as_u32().max(1), config.size.height.as_u32().max(1))
}

/// `Present` sync interval for a vsync mode.
///
/// Mailbox has no direct equivalent in a flip-model swap chain; interval zero
/// with a bounded frame latency is the closest, as the compositor shows the
/// newest frame each refresh.
pub(crate) fn sync_interval(vsync: VsyncMode) -> u32 {
    match vsync {
        VsyncMode::On => 1,
        VsyncMode::Off | VsyncMode::Mailbox => 0,
    }
}
