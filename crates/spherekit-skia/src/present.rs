//! Getting pixels onto the screen through `softbuffer`.
//!
//! `softbuffer` hands out a CPU buffer that is, on Win32, X11 with MIT-SHM and
//! Wayland, the very memory the window system blits from. Skia wraps that
//! buffer as its raster surface and draws straight into it, so a frame is
//! rendered and presented without a single full-frame copy on our side.
//!
//! ## The pixel format
//!
//! `softbuffer` defines a pixel as a native-endian `u32` laid out `0x00RRGGBB`,
//! which on a little-endian machine is the byte sequence B, G, R, X — Skia's
//! `BGRA8888`. The surface is declared opaque, so Skia writes `0xFF` into the X
//! byte. `softbuffer` documents that byte as "should be zero", but every
//! backend it ships (GDI `BI_RGB`, X11 depth-24 visuals, Wayland `XRGB8888`,
//! CoreGraphics `NoneSkipFirst`) ignores it, and clearing it would cost a pass
//! over the whole frame. Big-endian targets are not supported: there the same
//! `u32` is X, R, G, B in memory, which Skia has no colour type for.
//!
//! ## What cannot be honoured
//!
//! * **Transparency.** `softbuffer` presents opaque pixels only, so
//!   `SurfaceConfig::transparent` is ignored for windows and the frame is
//!   rendered opaque.
//! * **Vsync and latency.** There is no swap chain: `present` blits
//!   immediately, and pacing is whatever the window system does with that.
//!   `SurfaceConfig::vsync` and `SurfaceConfig::present` are accepted and
//!   ignored.

use core::num::NonZeroU32;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use spherekit_core::{InitError, RenderError};
use std::sync::Arc;

/// A window the renderer can present to, with its handle type erased.
///
/// Erasing it keeps [`crate::SkiaRenderer`] a plain, non-generic type, which is
/// what lets the facade hold "some backend" without a type parameter.
pub(crate) trait Presenter {
    /// Resizes the presentation buffer. Never called with a zero size.
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError>;

    /// Lends the back buffer to `draw` as BGRX bytes, then presents it.
    ///
    /// Acquiring, drawing and presenting happen inside one call because the
    /// buffer borrows the surface: there is no way to hold it across
    /// `render` and `end_frame` without a self-referential struct.
    fn frame(
        &mut self,
        draw: &mut dyn FnMut(&mut [u8], u32, u32) -> Result<(), RenderError>,
    ) -> Result<(), RenderError>;

    /// Reads back what the window currently shows, as `0x00RRGGBB` pixels.
    fn fetch(&mut self) -> Result<Vec<u32>, RenderError>;
}

pub(crate) struct SoftbufferPresenter<W> {
    // The context is never used after the surface is created, but the platform
    // connection it owns must outlive the surface.
    _context: softbuffer::Context<Arc<W>>,
    surface: softbuffer::Surface<Arc<W>, Arc<W>>,
    size: (u32, u32),
}

impl<W> SoftbufferPresenter<W>
where
    W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
{
    pub(crate) fn new(window: Arc<W>) -> Result<Self, InitError> {
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| InitError::Surface(format!("softbuffer context: {e}")))?;
        let surface = softbuffer::Surface::new(&context, window)
            .map_err(|e| InitError::Surface(format!("softbuffer surface: {e}")))?;
        Ok(Self { _context: context, surface, size: (0, 0) })
    }
}

fn backend_error(what: &str, e: softbuffer::SoftBufferError) -> RenderError {
    RenderError::Backend(format!("softbuffer {what}: {e}"))
}

impl<W> Presenter for SoftbufferPresenter<W>
where
    W: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static,
{
    fn resize(&mut self, width: u32, height: u32) -> Result<(), RenderError> {
        let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) else {
            return Ok(());
        };
        if self.size != (width, height) {
            self.surface.resize(w, h).map_err(|e| backend_error("resize", e))?;
            self.size = (width, height);
        }
        Ok(())
    }

    fn frame(
        &mut self,
        draw: &mut dyn FnMut(&mut [u8], u32, u32) -> Result<(), RenderError>,
    ) -> Result<(), RenderError> {
        let mut buffer = self.surface.buffer_mut().map_err(|e| backend_error("buffer", e))?;
        let (w, h) = (buffer.width().get(), buffer.height().get());
        let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut buffer[..]);
        draw(bytes, w, h)?;
        buffer.present().map_err(|e| backend_error("present", e))
    }

    fn fetch(&mut self) -> Result<Vec<u32>, RenderError> {
        self.surface.fetch().map_err(|e| backend_error("fetch", e))
    }
}
