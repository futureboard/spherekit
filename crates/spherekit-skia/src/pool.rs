//! Pooled offscreen surfaces for layers.
//!
//! A layer is allocated only as large as its content, so sizes vary, but the
//! same UI tends to produce the same layers frame after frame: a translucent
//! panel, a blurred popup. Pooling by exact size turns those into zero
//! allocations in steady state, while an idle timeout keeps a popup that was
//! shown once from pinning its memory for the rest of the session — the same
//! policy as the GPU backend's layer pool.

use skia_safe::{AlphaType, Color4f, ColorSpace, ColorType, ISize, ImageInfo, Surface, surfaces};

/// Frames a pooled surface may go unused before it is freed.
///
/// Long enough that a menu opening and closing repeatedly does not reallocate
/// every time, short enough that memory comes back within a second or so.
const KEEP_FOR_FRAMES: u64 = 60;

struct Pooled {
    size: (i32, i32),
    surface: Surface,
    last_used: u64,
}

/// Offscreen surfaces, reused across frames by exact size.
pub(crate) struct LayerPool {
    entries: Vec<Pooled>,
    srgb: ColorSpace,
}

impl LayerPool {
    pub(crate) fn new() -> Self {
        Self { entries: Vec::new(), srgb: ColorSpace::new_srgb() }
    }

    /// Hands out a cleared, transparent surface of exactly `width × height`.
    ///
    /// Returns `None` only if Skia cannot allocate the pixels.
    pub(crate) fn acquire(&mut self, width: i32, height: i32, frame: u64) -> Option<usize> {
        let size = (width.max(1), height.max(1));
        let index = match self.entries.iter().position(|e| e.size == size && e.last_used != frame) {
            Some(i) => i,
            None => {
                let info = ImageInfo::new(
                    ISize::new(size.0, size.1),
                    ColorType::N32,
                    AlphaType::Premul,
                    self.srgb.clone(),
                );
                let surface = surfaces::raster(&info, None, None)?;
                self.entries.push(Pooled { size, surface, last_used: frame });
                self.entries.len() - 1
            }
        };
        let entry = &mut self.entries[index];
        entry.last_used = frame;
        // Offscreen layers must start fully transparent, or what a previous
        // frame left in the pooled surface bleeds through the composite.
        let canvas = entry.surface.canvas();
        canvas.restore_to_count(1);
        canvas.reset_matrix();
        canvas.clear(Color4f::new(0.0, 0.0, 0.0, 0.0));
        Some(index)
    }

    pub(crate) fn surface(&mut self, index: usize) -> &mut Surface {
        &mut self.entries[index].surface
    }

    /// Frees surfaces that have sat idle for [`KEEP_FOR_FRAMES`].
    pub(crate) fn reap(&mut self, frame: u64) {
        self.entries.retain(|e| frame.saturating_sub(e.last_used) < KEEP_FOR_FRAMES);
    }

    pub(crate) fn memory_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.size.0 as u64 * e.size.1 as u64 * 4).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_frame_never_gets_the_same_surface_twice() {
        let mut p = LayerPool::new();
        let a = p.acquire(10, 10, 1).unwrap();
        let b = p.acquire(10, 10, 1).unwrap();
        assert_ne!(a, b);
        // The next frame reuses both rather than allocating.
        assert!(p.acquire(10, 10, 2).unwrap() < 2);
        assert!(p.acquire(10, 10, 2).unwrap() < 2);
        assert_eq!(p.entries.len(), 2);
    }

    #[test]
    fn idle_surfaces_are_reaped() {
        let mut p = LayerPool::new();
        p.acquire(10, 10, 1);
        p.reap(1 + KEEP_FOR_FRAMES - 1);
        assert_eq!(p.entries.len(), 1);
        p.reap(1 + KEEP_FOR_FRAMES);
        assert_eq!(p.entries.len(), 0);
    }
}
