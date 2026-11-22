//! Texture storage: the CPU copies behind every [`TextureId`].
//!
//! A "texture" on a raster backend is a block of memory plus a cached
//! [`Image`] wrapping a copy of it. Two decisions shape this module:
//!
//! * **The RGBA texels are kept, and the `Image` is rebuilt lazily.** The glyph
//!   atlas fills incrementally through region uploads, often several per frame
//!   while new text appears. Rebuilding an image per region would copy a whole
//!   page per glyph; marking the texture dirty and rebuilding once, just before
//!   the frame that needs it, copies it at most once per frame.
//! * **Atlas pages get an `Alpha8` mirror.** Glyph coverage arrives replicated
//!   into RGBA so the GPU path can share one texture format, but Skia tints an
//!   alpha-only image with the paint colour for free — exactly a glyph draw. The
//!   mirror is built only for textures bound with
//!   [`RendererBackend::set_atlas_page`](spherekit_render::RendererBackend::set_atlas_page),
//!   and kept up to date by region uploads from then on.
//!
//! ## How texels are interpreted
//!
//! Images arrive from `spherekit-image` as **premultiplied, sRGB-encoded** RGBA8
//! (the decoder's defaults; see that crate's module docs). They are wrapped as
//! premultiplied `RGBA8888` tagged sRGB, which on an sRGB surface means the
//! bytes reach the screen unchanged: an image looks exactly like its file.

use rustc_hash::FxHashMap;
use skia_safe::{AlphaType, ColorSpace, ColorType, Data, ISize, Image, ImageInfo, images};
use spherekit_core::{RenderError, TextureId};

/// The largest texture edge accepted.
///
/// Skia's raster backend has no hard limit beyond memory, but advertising one
/// keeps callers honest, and 16384² RGBA is already a gigabyte.
pub(crate) const MAX_TEXTURE_SIZE: u32 = 16_384;

/// One uploaded texture.
pub(crate) struct Texture {
    width: u32,
    height: u32,
    /// Premultiplied RGBA8, tightly packed.
    rgba: Vec<u8>,
    /// The colour image, valid unless `image_dirty`.
    image: Option<Image>,
    image_dirty: bool,
    /// Coverage mirror for atlas pages, one byte per texel. Empty until the
    /// texture is first bound as an atlas page.
    coverage: Vec<u8>,
    coverage_image: Option<Image>,
    coverage_dirty: bool,
}

impl Texture {
    fn bytes(&self) -> u64 {
        (self.rgba.len() + self.coverage.len()) as u64
    }

    /// Fills `coverage` for one region from the RGBA texels.
    ///
    /// Grayscale atlas pages replicate coverage into red, green and blue, and
    /// RGB-subpixel pages store one coverage per channel. Taking the maximum
    /// handles both: it is exact for the first and, for the second, the
    /// conservative grayscale fallback the capabilities promise (this backend
    /// reports no dual-source blending, so the batch compiler should never ask
    /// for subpixel glyphs in the first place).
    fn mirror_region(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let stride = self.width as usize;
        for row in y as usize..(y + h) as usize {
            let start = row * stride + x as usize;
            let src = &self.rgba[start * 4..(start + w as usize) * 4];
            let dst = &mut self.coverage[start..start + w as usize];
            for (d, px) in dst.iter_mut().zip(src.chunks_exact(4)) {
                *d = px[0].max(px[1]).max(px[2]);
            }
        }
        self.coverage_dirty = true;
    }
}

struct Slot {
    generation: u32,
    texture: Option<Texture>,
}

/// Every texture the renderer owns, addressed by generational id.
pub(crate) struct TextureStore {
    slots: Vec<Slot>,
    free: Vec<u32>,
    /// Atlas page → texture, as declared through `set_atlas_page`.
    atlas_pages: FxHashMap<u32, TextureId>,
    srgb: ColorSpace,
    bytes: u64,
    /// Texels uploaded since the last frame, reported as `bytes_uploaded`.
    uploaded: u64,
}

impl TextureStore {
    pub(crate) fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            atlas_pages: FxHashMap::default(),
            srgb: ColorSpace::new_srgb(),
            bytes: 0,
            uploaded: 0,
        }
    }

    fn get(&self, id: TextureId) -> Option<&Texture> {
        let slot = self.slots.get(id.slot())?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.texture.as_ref()
    }

    fn get_mut(&mut self, id: TextureId) -> Option<&mut Texture> {
        let slot = self.slots.get_mut(id.slot())?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.texture.as_mut()
    }

    pub(crate) fn upload(
        &mut self,
        id: Option<TextureId>,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<TextureId, RenderError> {
        if width == 0 || height == 0 || width > MAX_TEXTURE_SIZE || height > MAX_TEXTURE_SIZE {
            return Err(RenderError::Backend(format!(
                "texture {width}x{height} is outside the supported range 1..={MAX_TEXTURE_SIZE}"
            )));
        }
        let len = width as usize * height as usize * 4;
        if rgba.len() < len {
            return Err(RenderError::Backend(format!(
                "texture {width}x{height} needs {len} bytes of RGBA8, got {}",
                rgba.len()
            )));
        }
        self.uploaded += len as u64;

        // Replacing an existing texture keeps its id and, when it is an atlas
        // page, its coverage mirror.
        if let Some(id) = id
            && let Some(tex) = self.get_mut(id)
        {
            let before = tex.bytes();
            let was_atlas = !tex.coverage.is_empty();
            tex.width = width;
            tex.height = height;
            tex.rgba.clear();
            tex.rgba.extend_from_slice(&rgba[..len]);
            tex.image = None;
            tex.image_dirty = true;
            tex.coverage.clear();
            tex.coverage_image = None;
            if was_atlas {
                tex.coverage.resize(width as usize * height as usize, 0);
                tex.mirror_region(0, 0, width, height);
            }
            let after = tex.bytes();
            self.bytes = self.bytes - before + after;
            return Ok(id);
        }

        let texture = Texture {
            width,
            height,
            rgba: rgba[..len].to_vec(),
            image: None,
            image_dirty: true,
            coverage: Vec::new(),
            coverage_image: None,
            coverage_dirty: false,
        };
        self.bytes += texture.bytes();
        let index = match self.free.pop() {
            Some(i) => i,
            None => {
                self.slots.push(Slot { generation: 1, texture: None });
                (self.slots.len() - 1) as u32
            }
        };
        let slot = &mut self.slots[index as usize];
        slot.texture = Some(texture);
        Ok(TextureId::new(index, slot.generation))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upload_region(
        &mut self,
        id: TextureId,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), RenderError> {
        let Some(tex) = self.get_mut(id) else {
            return Err(RenderError::StaleResource("texture"));
        };
        if x.checked_add(width).is_none_or(|r| r > tex.width)
            || y.checked_add(height).is_none_or(|b| b > tex.height)
        {
            return Err(RenderError::Backend(format!(
                "region {width}x{height}+{x}+{y} exceeds texture {}x{}",
                tex.width, tex.height
            )));
        }
        let row = width as usize * 4;
        if rgba.len() < row * height as usize {
            return Err(RenderError::Backend(format!(
                "region {width}x{height} needs {} bytes of RGBA8, got {}",
                row * height as usize,
                rgba.len()
            )));
        }
        if width == 0 || height == 0 {
            return Ok(());
        }
        let stride = tex.width as usize * 4;
        for r in 0..height as usize {
            let dst = (y as usize + r) * stride + x as usize * 4;
            tex.rgba[dst..dst + row].copy_from_slice(&rgba[r * row..(r + 1) * row]);
        }
        tex.image_dirty = true;
        if !tex.coverage.is_empty() {
            tex.mirror_region(x, y, width, height);
        }
        self.uploaded += (row * height as usize) as u64;
        Ok(())
    }

    pub(crate) fn destroy(&mut self, id: TextureId) {
        let Some(slot) = self.slots.get_mut(id.slot()) else { return };
        if slot.generation != id.generation() {
            return;
        }
        if let Some(tex) = slot.texture.take() {
            self.bytes -= tex.bytes();
            // Bumping the generation is what turns every outstanding copy of
            // this id into a detectably stale handle.
            slot.generation = slot.generation.wrapping_add(1).max(1);
            self.free.push(id.index());
        }
        self.atlas_pages.retain(|_, t| *t != id);
    }

    pub(crate) fn set_atlas_page(&mut self, page: u32, id: TextureId) {
        self.atlas_pages.insert(page, id);
        if let Some(tex) = self.get_mut(id)
            && tex.coverage.is_empty()
        {
            let (w, h) = (tex.width, tex.height);
            tex.coverage.resize(w as usize * h as usize, 0);
            tex.mirror_region(0, 0, w, h);
            let added = tex.coverage.len() as u64;
            self.bytes += added;
        }
    }

    /// Rebuilds every dirty image a frame is about to sample.
    ///
    /// Called once per frame before drawing, so drawing itself can borrow the
    /// store immutably and never pays for a rebuild mid-batch.
    pub(crate) fn prepare(&mut self, compiled: &spherekit_render::CompiledFrame) {
        use spherekit_render::BatchKind;
        for batch in &compiled.batches {
            match batch.kind {
                BatchKind::Image { texture } | BatchKind::Mesh { texture: Some(texture) } => {
                    self.refresh_image(texture)
                }
                BatchKind::Glyph { page, .. } => {
                    if let Some(id) = self.atlas_pages.get(&page).copied() {
                        self.refresh_coverage(id);
                    }
                }
                BatchKind::Quad | BatchKind::Mesh { texture: None } => {}
            }
        }
    }

    fn refresh_image(&mut self, id: TextureId) {
        let srgb = self.srgb.clone();
        let Some(tex) = self.get_mut(id) else { return };
        if !tex.image_dirty && tex.image.is_some() {
            return;
        }
        let info = ImageInfo::new(
            ISize::new(tex.width as i32, tex.height as i32),
            ColorType::RGBA8888,
            AlphaType::Premul,
            srgb,
        );
        tex.image =
            images::raster_from_data(&info, Data::new_copy(&tex.rgba), tex.width as usize * 4);
        tex.image_dirty = false;
    }

    fn refresh_coverage(&mut self, id: TextureId) {
        let Some(tex) = self.get_mut(id) else { return };
        if tex.coverage.is_empty() || (!tex.coverage_dirty && tex.coverage_image.is_some()) {
            return;
        }
        let info = ImageInfo::new_a8(ISize::new(tex.width as i32, tex.height as i32));
        tex.coverage_image =
            images::raster_from_data(&info, Data::new_copy(&tex.coverage), tex.width as usize);
        tex.coverage_dirty = false;
    }

    /// The colour image for a texture, if it exists and has been prepared.
    pub(crate) fn image(&self, id: TextureId) -> Option<&Image> {
        self.get(id)?.image.as_ref()
    }

    /// The coverage image bound to an atlas page, if any.
    pub(crate) fn atlas_coverage(&self, page: u32) -> Option<&Image> {
        let id = self.atlas_pages.get(&page)?;
        self.get(*id)?.coverage_image.as_ref()
    }

    pub(crate) fn memory_bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns and resets the bytes uploaded since the last call.
    pub(crate) fn take_uploaded(&mut self) -> u64 {
        core::mem::take(&mut self.uploaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(w: u32, h: u32, v: u8) -> Vec<u8> {
        (0..w * h).flat_map(|_| [v, v, v, 255]).collect()
    }

    #[test]
    fn destroyed_ids_go_stale_and_slots_are_reused() {
        let mut s = TextureStore::new();
        let a = s.upload(None, 2, 2, &page(2, 2, 0)).unwrap();
        s.destroy(a);
        let b = s.upload(None, 2, 2, &page(2, 2, 0)).unwrap();
        assert_eq!(a.index(), b.index());
        assert_ne!(a.generation(), b.generation());
        assert!(s.get(a).is_none());
        assert!(s.upload_region(a, 0, 0, 1, 1, &[0; 4]).is_err());
    }

    #[test]
    fn region_uploads_update_the_coverage_mirror() {
        let mut s = TextureStore::new();
        let id = s.upload(None, 4, 4, &page(4, 4, 0)).unwrap();
        s.set_atlas_page(0, id);
        s.upload_region(id, 1, 2, 2, 1, &[10, 10, 10, 255, 200, 200, 200, 255]).unwrap();
        let tex = s.get(id).unwrap();
        assert_eq!(tex.coverage[2 * 4 + 1], 10);
        assert_eq!(tex.coverage[2 * 4 + 2], 200);
        assert_eq!(tex.coverage[0], 0);
    }

    #[test]
    fn subpixel_coverage_collapses_to_its_strongest_channel() {
        let mut s = TextureStore::new();
        let id = s.upload(None, 1, 1, &[30, 90, 60, 255]).unwrap();
        s.set_atlas_page(3, id);
        assert_eq!(s.get(id).unwrap().coverage[0], 90);
    }

    #[test]
    fn out_of_bounds_regions_are_rejected() {
        let mut s = TextureStore::new();
        let id = s.upload(None, 4, 4, &page(4, 4, 0)).unwrap();
        assert!(s.upload_region(id, 3, 0, 2, 1, &[0; 8]).is_err());
        assert!(s.upload_region(id, u32::MAX, 0, 2, 1, &[0; 8]).is_err());
        assert!(s.upload(None, 0, 4, &[]).is_err());
        assert!(s.upload(None, 4, 4, &[0; 3]).is_err());
    }

    #[test]
    fn memory_accounting_returns_to_zero() {
        let mut s = TextureStore::new();
        let id = s.upload(None, 8, 8, &page(8, 8, 0)).unwrap();
        s.set_atlas_page(0, id);
        assert_eq!(s.memory_bytes(), 8 * 8 * 5);
        s.destroy(id);
        assert_eq!(s.memory_bytes(), 0);
    }
}
