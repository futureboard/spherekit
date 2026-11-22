//! Glyph rasterisation through DirectWrite.
//!
//! On Windows this is the default rasteriser: text drawn by SphereKit comes out
//! of the same `IDWriteGlyphRunAnalysis` that Direct2D, Edge and the shell use,
//! with the same symmetric antialiasing, the same `gasp`-driven vertical grid
//! fitting and, for the subpixel path, the same ClearType filtering. The
//! portable outline rasteriser in [`crate::raster`] remains the fallback for any
//! face DirectWrite refuses to load.
//!
//! ## What is borrowed from DirectWrite, and what is not
//!
//! Only rasterisation. Shaping, fallback and layout stay in SphereKit, so a
//! layout measures identically whichever rasteriser draws it and on every
//! platform. DirectWrite is handed one glyph index at a time and asked for its
//! coverage.
//!
//! The coverage is returned *raw*: no gamma, no enhanced contrast. Those are
//! DirectWrite's answer to blending in gamma space, and SphereKit blends in
//! linear light with its own correction (`GlyphRun::coverage_contrast`) applied
//! in the text shader. Applying both would correct twice.
//!
//! ## Placement
//!
//! The glyph is rasterised with its baseline origin at `(phase / 4, 0)`, so the
//! returned texture bounds are whole device pixels relative to the *integral*
//! pen — exactly the convention the outline rasteriser follows and the batch
//! compiler's snapping relies on.

use crate::font::{FaceSource, FontDatabase};
use crate::types::{GlyphFormat, GlyphImage};
use rustc_hash::FxHashMap;
use spherekit_core::{FontId, GlyphId, Point, Rect, Size};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FACE_TYPE, DWRITE_FONT_FILE_TYPE,
    DWRITE_FONT_SIMULATIONS_NONE, DWRITE_GLYPH_OFFSET, DWRITE_GLYPH_RUN,
    DWRITE_GRID_FIT_MODE_DEFAULT, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC, DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE,
    DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE, DWRITE_TEXTURE_ALIASED_1x1, DWRITE_TEXTURE_CLEARTYPE_3x1,
    DWriteCreateFactory, IDWriteFactory, IDWriteFactory2, IDWriteFactory5, IDWriteFontFace,
    IDWriteFontFile, IDWriteInMemoryFontFileLoader,
};
use windows::core::{BOOL, HSTRING, Interface};

/// Rasterises glyphs with DirectWrite, caching one font face per [`FontId`].
pub struct DirectWriteRasterizer {
    factory: IDWriteFactory2,
    /// Present on Windows 10 1703 and later, which is every supported
    /// Windows. Without it, in-memory fonts fall back to the outline path.
    factory5: Option<IDWriteFactory5>,
    /// Created on the first in-memory font and registered with the factory for
    /// the rasteriser's lifetime.
    memory_loader: Option<IDWriteInMemoryFontFileLoader>,
    /// `None` records a face DirectWrite could not load, so it is not retried
    /// for every glyph.
    faces: FxHashMap<FontId, Option<IDWriteFontFace>>,
}

impl core::fmt::Debug for DirectWriteRasterizer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DirectWriteRasterizer").field("faces", &self.faces.len()).finish()
    }
}

// SAFETY: the shared DirectWrite factory and the objects it creates are
// documented as free-threaded. The in-memory loader is only ever used through
// `&mut self`, so it is never touched from two threads at once.
unsafe impl Send for DirectWriteRasterizer {}

impl Drop for DirectWriteRasterizer {
    fn drop(&mut self) {
        if let (Some(factory5), Some(loader)) = (&self.factory5, &self.memory_loader) {
            // SAFETY: the loader was registered with this factory in
            // `memory_font_file`; unregistering it is the documented teardown.
            unsafe {
                let _ = factory5.UnregisterFontFileLoader(loader);
            }
        }
    }
}

impl DirectWriteRasterizer {
    /// Creates a rasteriser, or `None` when DirectWrite is unavailable.
    pub fn new() -> Option<Self> {
        // SAFETY: plain factory creation; the result is reference counted.
        let factory: IDWriteFactory2 =
            unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) }.ok()?;
        let factory5 = factory.cast::<IDWriteFactory5>().ok();
        Some(Self { factory, factory5, memory_loader: None, faces: FxHashMap::default() })
    }

    /// Forgets every cached face, after the font set changed.
    pub fn clear(&mut self) {
        self.faces.clear();
    }

    /// Rasterises one glyph at `size_px` device pixels per em.
    ///
    /// `format` must be [`GlyphFormat::Grayscale`] or [`GlyphFormat::Subpixel`].
    /// Returns `None` when DirectWrite cannot produce the glyph, in which case
    /// the caller should use the outline rasteriser instead; an empty image (a
    /// space) is `Some`.
    pub fn rasterize(
        &mut self,
        fonts: &FontDatabase,
        font: FontId,
        glyph: GlyphId,
        size_px: f32,
        subpixel_phase: u8,
        format: GlyphFormat,
    ) -> Option<GlyphImage> {
        if !size_px.is_finite() || size_px <= 0.0 {
            return Some(GlyphImage::empty(format));
        }
        let face = self.face(fonts, font)?;
        // SAFETY: `face` is a live font face; `GetGlyphCount` has no preconditions.
        if u32::from(glyph.0) >= u32::from(unsafe { face.GetGlyphCount() }) {
            return None;
        }

        let (antialias, texture, bytes_per_pixel) = match format {
            GlyphFormat::Grayscale => {
                (DWRITE_TEXT_ANTIALIAS_MODE_GRAYSCALE, DWRITE_TEXTURE_ALIASED_1x1, 1usize)
            }
            GlyphFormat::Subpixel => {
                (DWRITE_TEXT_ANTIALIAS_MODE_CLEARTYPE, DWRITE_TEXTURE_CLEARTYPE_3x1, 3usize)
            }
            GlyphFormat::ColorBitmap => return None,
        };

        let index = glyph.0;
        let advance = 0.0f32;
        let offset = DWRITE_GLYPH_OFFSET::default();
        let run = DWRITE_GLYPH_RUN {
            fontFace: core::mem::ManuallyDrop::new(Some(face)),
            fontEmSize: size_px,
            glyphCount: 1,
            glyphIndices: &index,
            glyphAdvances: &advance,
            glyphOffsets: &offset,
            isSideways: BOOL(0),
            bidiLevel: 0,
        };
        let origin_x = f32::from(subpixel_phase.min(3)) * 0.25;

        // SAFETY: `run` and everything it points at outlive the call. With no
        // transform DirectWrite treats one DIP as one pixel, so `fontEmSize` is
        // already in device pixels.
        let analysis = unsafe {
            self.factory.CreateGlyphRunAnalysis(
                &run,
                None,
                DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC,
                DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_GRID_FIT_MODE_DEFAULT,
                antialias,
                origin_x,
                0.0,
            )
        };
        // The run borrowed a reference to the face; give it back.
        drop(core::mem::ManuallyDrop::into_inner(run.fontFace));
        let analysis = analysis.ok()?;

        // SAFETY: `analysis` is live; the texture type matches the antialias
        // mode it was created with.
        let bounds = unsafe { analysis.GetAlphaTextureBounds(texture) }.ok()?;
        let width = (bounds.right - bounds.left).max(0) as u32;
        let height = (bounds.bottom - bounds.top).max(0) as u32;
        if width == 0 || height == 0 {
            return Some(GlyphImage::empty(format));
        }

        let mut data = vec![0u8; width as usize * height as usize * bytes_per_pixel];
        // SAFETY: `data` is exactly `width * height * bytes_per_pixel` bytes,
        // which is what DirectWrite writes for these bounds and texture type.
        unsafe { analysis.CreateAlphaTexture(texture, &bounds, &mut data) }.ok()?;

        let s = 1.0 / size_px;
        Some(GlyphImage {
            width,
            height,
            format,
            data,
            bounds_em: Rect::new(
                Point::new(bounds.left as f32 * s, bounds.top as f32 * s),
                Size::new(width as f32 * s, height as f32 * s),
            ),
        })
    }

    /// The DirectWrite face for `font`, loading it on first use.
    fn face(&mut self, fonts: &FontDatabase, font: FontId) -> Option<IDWriteFontFace> {
        if let Some(cached) = self.faces.get(&font) {
            return cached.clone();
        }
        let loaded = fonts.face_source(font).and_then(|source| self.load(&source));
        if loaded.is_none() {
            tracing::debug!(
                target: "spherekit_text",
                "DirectWrite could not load {:?}; using the outline rasteriser",
                fonts.family_name(font)
            );
        }
        self.faces.insert(font, loaded.clone());
        loaded
    }

    fn load(&mut self, source: &FaceSource) -> Option<IDWriteFontFace> {
        let (file, index) = match source {
            FaceSource::File { path, index } => {
                let path = HSTRING::from(path.as_os_str());
                // SAFETY: `path` is a valid, NUL-terminated wide string for the
                // duration of the call.
                let file = unsafe { self.factory.CreateFontFileReference(&path, None) }.ok()?;
                (file, *index)
            }
            FaceSource::Memory { data, index } => {
                (self.memory_font_file(data.as_ref().as_ref())?, *index)
            }
        };

        let mut supported = BOOL(0);
        let mut file_type = DWRITE_FONT_FILE_TYPE::default();
        let mut face_type = DWRITE_FONT_FACE_TYPE::default();
        let mut faces = 0u32;
        // SAFETY: every out-pointer refers to a live local.
        unsafe { file.Analyze(&mut supported, &mut file_type, Some(&mut face_type), &mut faces) }
            .ok()?;
        if !supported.as_bool() || index >= faces {
            return None;
        }
        // SAFETY: `file` was just analysed as a supported font of `face_type`.
        unsafe {
            self.factory.CreateFontFace(
                face_type,
                &[Some(file)],
                index,
                DWRITE_FONT_SIMULATIONS_NONE,
            )
        }
        .ok()
    }

    /// Wraps in-memory font bytes as a DirectWrite font file.
    ///
    /// DirectWrite copies the bytes (no owner object is passed), so the
    /// database stays free to drop its own copy. This path is only taken for
    /// fonts an application loaded from memory; system fonts are referenced by
    /// path and never copied.
    fn memory_font_file(&mut self, bytes: &[u8]) -> Option<IDWriteFontFile> {
        let factory5 = self.factory5.as_ref()?;
        if self.memory_loader.is_none() {
            // SAFETY: creating and registering a loader with the factory that
            // owns it; it is unregistered in `Drop`.
            let loader = unsafe { factory5.CreateInMemoryFontFileLoader() }.ok()?;
            unsafe { factory5.RegisterFontFileLoader(&loader) }.ok()?;
            self.memory_loader = Some(loader);
        }
        let loader = self.memory_loader.as_ref()?;
        let factory: IDWriteFactory = factory5.cast().ok()?;
        let len = u32::try_from(bytes.len()).ok()?;
        // SAFETY: `bytes` is valid for `len` bytes and is copied before the
        // call returns, because no owner object is supplied.
        unsafe {
            loader.CreateInMemoryFontFileReference(&factory, bytes.as_ptr().cast(), len, None)
        }
        .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FontRequest;

    fn setup() -> Option<(DirectWriteRasterizer, FontDatabase, FontId)> {
        let mut fonts = FontDatabase::with_system_fonts();
        let font = fonts.resolve(&FontRequest::default())?;
        Some((DirectWriteRasterizer::new()?, fonts, font))
    }

    fn glyph(fonts: &FontDatabase, font: FontId, c: char) -> GlyphId {
        fonts.glyph_index(font, c).expect("the default face covers ASCII")
    }

    #[test]
    fn a_letter_has_antialiased_grayscale_coverage() {
        let Some((mut dw, fonts, font)) = setup() else { return };
        let g = glyph(&fonts, font, 'O');
        let img = dw.rasterize(&fonts, font, g, 16.0, 0, GlyphFormat::Grayscale).unwrap();
        assert_eq!(img.format, GlyphFormat::Grayscale);
        assert_eq!(img.data.len(), (img.width * img.height) as usize);
        assert!(img.data.contains(&255) || img.data.iter().any(|&c| c > 200), "no solid ink");
        assert!(img.data.iter().any(|&c| c > 0 && c < 255), "no antialiased edge");
        // The ink sits above the baseline: a capital's top is negative, y-down.
        assert!(img.bounds_em.min_y() < -0.5, "{:?}", img.bounds_em);
        // About 16 device pixels tall for a 16 px font's cap height (~0.7 em).
        assert!((8..=16).contains(&img.height), "height {}", img.height);
    }

    #[test]
    fn subpixel_coverage_is_three_bytes_per_pixel() {
        let Some((mut dw, fonts, font)) = setup() else { return };
        let g = glyph(&fonts, font, 'l');
        let img = dw.rasterize(&fonts, font, g, 13.0, 0, GlyphFormat::Subpixel).unwrap();
        assert_eq!(img.format, GlyphFormat::Subpixel);
        assert_eq!(img.data.len(), (img.width * img.height * 3) as usize);
        // ClearType coverage differs between channels at a vertical stem's edge.
        assert!(
            img.data.chunks_exact(3).any(|p| p[0] != p[2]),
            "RGB coverage collapsed to grayscale"
        );
    }

    #[test]
    fn a_space_is_empty_not_a_failure() {
        let Some((mut dw, fonts, font)) = setup() else { return };
        let g = glyph(&fonts, font, ' ');
        let img = dw.rasterize(&fonts, font, g, 16.0, 0, GlyphFormat::Grayscale).unwrap();
        assert!(img.is_empty());
    }

    #[test]
    fn the_phase_moves_coverage_rightwards() {
        let Some((mut dw, fonts, font)) = setup() else { return };
        let g = glyph(&fonts, font, 'l');
        let mut centroid = |phase| {
            let img = dw.rasterize(&fonts, font, g, 20.0, phase, GlyphFormat::Grayscale).unwrap();
            let (mut sum, mut weight) = (0.0f64, 0.0f64);
            for (i, &c) in img.data.iter().enumerate() {
                let x = (i as u32 % img.width) as f64 + img.bounds_em.min_x() as f64 * 20.0;
                sum += x * c as f64;
                weight += c as f64;
            }
            sum / weight
        };
        let (a, b) = (centroid(0), centroid(2));
        assert!(b > a + 0.25 && b < a + 0.75, "half-pixel phase moved ink from {a} to {b}");
    }

    #[test]
    fn an_out_of_range_glyph_defers_to_the_outline_path() {
        let Some((mut dw, fonts, font)) = setup() else { return };
        assert!(
            dw.rasterize(&fonts, font, GlyphId(u16::MAX), 16.0, 0, GlyphFormat::Grayscale)
                .is_none()
        );
    }

    #[test]
    fn in_memory_fonts_load_without_a_file() {
        let Some((mut dw, fonts, font)) = setup() else { return };
        let FaceSource::File { path, .. } = fonts.face_source(font).unwrap() else { return };
        let bytes = std::fs::read(path).unwrap();
        let mut mem = FontDatabase::new();
        let Some(&id) = mem.load_font_data(bytes).first() else { return };
        assert!(matches!(mem.face_source(id), Some(FaceSource::Memory { .. })));
        let g = glyph(&mem, id, 'A');
        let img = dw.rasterize(&mem, id, g, 16.0, 0, GlyphFormat::Grayscale);
        assert!(img.is_some_and(|i| !i.is_empty()));
    }
}
