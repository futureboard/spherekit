//! The text system: one object that turns a string into drawable glyphs.
//!
//! Everything below this module — font discovery, shaping, line layout, glyph
//! rasterisation, atlas packing — is independently usable. This is the thing you
//! actually hold: it owns a font database, an atlas and the caches, and it
//! implements [`spherekit_render::GlyphProvider`] so the batch compiler can ask it
//! for glyphs while building a frame.
//!
//! ```no_run
//! use spherekit_text::{TextStyle, TextSystem};
//! use spherekit_core::px;
//!
//! let mut text = TextSystem::with_system_fonts();
//! let layout = text.layout("ทดสอบ 日本語 mixed", &TextStyle::default(), Some(px(320.0)));
//! assert!(layout.glyph_count() > 0);
//! ```
//!
//! ## Where the GPU boundary is
//!
//! [`TextSystem`] never touches a GPU. It fills CPU-side atlas pages and
//! records which regions changed; the renderer drains those with
//! [`TextSystem::take_dirty_regions`] and uploads them. That split is what lets
//! the whole text stack be tested without a device, and it is why glyph
//! rasterisation can move to a worker thread later without the renderer
//! noticing.

use crate::atlas::{AtlasConfig, AtlasStats, DirtyRegion, GlyphAtlas};
use crate::cache::{CacheStats, ShapeCache};
use crate::font::FontDatabase;
use crate::raster::{
    bitmap_size_px, rasterize_glyph_subpixel_with_zones, rasterize_glyph_with_zones_at_phase,
};
use crate::types::{GlyphFormat, GlyphImage, GlyphKey, TextLayout, TextStyle};
use spherekit_core::{FontId, GlyphId, Px, ScaleFactor};
use spherekit_render::{GlyphPlacement, GlyphProvider, GlyphRequest};

/// Which rasteriser turns glyph outlines into coverage bitmaps.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum GlyphRasterizer {
    /// The platform's own: DirectWrite on Windows, so text matches every other
    /// application there. Elsewhere, and for any face DirectWrite cannot load,
    /// this is the same as [`GlyphRasterizer::Outline`].
    #[default]
    Platform,
    /// SphereKit's portable outline rasteriser on every platform.
    ///
    /// Identical output everywhere, which is what pixel tests and
    /// cross-platform screenshots want.
    Outline,
}

/// Counters for the diagnostics overlay.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct TextSystemStats {
    /// Glyphs served from the atlas without rasterising.
    pub glyph_hits: u64,
    /// Glyphs rasterised this session.
    pub glyph_misses: u64,
    /// Glyphs that could not be produced at all.
    pub glyph_failures: u64,
    /// Glyphs rasterised, by any rasteriser.
    pub bitmap_glyphs: u64,
    /// Of those, glyphs rasterised as RGB-stripe subpixel coverage.
    pub subpixel_glyphs: u64,
    /// Of those, glyphs the platform rasteriser (DirectWrite) produced. The
    /// rest came from the outline rasteriser.
    pub platform_glyphs: u64,
}

impl TextSystemStats {
    /// Fraction of glyph requests served without rasterising.
    ///
    /// In steady state this should sit very close to 1.0; a hit rate that stays
    /// low means something is defeating the cache, usually a variation hash
    /// that changes every frame.
    pub fn hit_rate(&self) -> f32 {
        let total = self.glyph_hits + self.glyph_misses;
        if total == 0 { 1.0 } else { self.glyph_hits as f32 / total as f32 }
    }
}

/// Owns the whole text stack for one application.
pub struct TextSystem {
    fonts: FontDatabase,
    atlas: GlyphAtlas,
    shapes: ShapeCache,
    rasterizer: GlyphRasterizer,
    /// Created on the first glyph, so a system that only measures text never
    /// starts DirectWrite. `Some(None)` means it was tried and is unavailable.
    #[cfg(windows)]
    dwrite: Option<Option<crate::dwrite::DirectWriteRasterizer>>,
    stats: TextSystemStats,
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl TextSystem {
    /// An empty system with no fonts loaded.
    pub fn new() -> Self {
        Self {
            fonts: FontDatabase::new(),
            atlas: GlyphAtlas::new(),
            shapes: ShapeCache::default(),
            rasterizer: GlyphRasterizer::default(),
            #[cfg(windows)]
            dwrite: None,
            stats: TextSystemStats::default(),
        }
    }

    /// A system with the platform's fonts loaded.
    pub fn with_system_fonts() -> Self {
        Self { fonts: FontDatabase::with_system_fonts(), ..Self::new() }
    }

    /// Clamps the atlas page size to what the device can actually hold.
    ///
    /// Called once after the renderer reports its limits. A 2048-texel page on
    /// an adapter that caps at 1024 would fail at upload time rather than at
    /// allocation time, which is a much harder failure to read.
    pub fn set_max_texture_size(&mut self, max: u32) {
        let config = self.atlas.config().clamped_to_device(max);
        if config != self.atlas.config() {
            self.atlas = GlyphAtlas::with_config(config);
        }
    }

    /// Replaces the atlas configuration, discarding its contents.
    pub fn set_atlas_config(&mut self, config: AtlasConfig) {
        self.atlas = GlyphAtlas::with_config(config);
    }

    /// Chooses the glyph rasteriser, discarding every resident glyph.
    ///
    /// Bitmaps from two rasterisers differ in hinting and filtering, so mixing
    /// them in one atlas would put subtly different letters side by side.
    pub fn set_rasterizer(&mut self, rasterizer: GlyphRasterizer) {
        if rasterizer != self.rasterizer {
            self.rasterizer = rasterizer;
            self.atlas = GlyphAtlas::with_config(self.atlas.config());
        }
    }

    /// The glyph rasteriser in use.
    #[inline]
    pub fn rasterizer(&self) -> GlyphRasterizer {
        self.rasterizer
    }

    /// The font database.
    #[inline]
    pub fn fonts(&self) -> &FontDatabase {
        &self.fonts
    }

    /// The font database, mutably, for loading fonts.
    #[inline]
    pub fn fonts_mut(&mut self) -> &mut FontDatabase {
        &mut self.fonts
    }

    /// The glyph atlas.
    #[inline]
    pub fn atlas(&self) -> &GlyphAtlas {
        &self.atlas
    }

    /// Counters for the diagnostics overlay.
    #[inline]
    pub fn stats(&self) -> TextSystemStats {
        self.stats
    }

    /// Atlas occupancy.
    #[inline]
    pub fn atlas_stats(&self) -> AtlasStats {
        self.atlas.stats()
    }

    /// Shaping cache counters.
    #[inline]
    pub fn shape_stats(&self) -> CacheStats {
        self.shapes.stats()
    }

    /// Advances the atlas's frame counter, which drives idle-based eviction.
    #[inline]
    pub fn begin_frame(&mut self) -> u64 {
        self.atlas.begin_frame()
    }

    /// Lays out a paragraph, using the shaping cache.
    ///
    /// Reshaping a paragraph every frame is the single largest avoidable cost
    /// in a text-heavy interface, so the cached path is the default and the
    /// uncached one has to be asked for by name.
    pub fn layout(&mut self, text: &str, style: &TextStyle, max_width: Option<Px>) -> TextLayout {
        if let Some(cached) = self.shapes.get(text, style, max_width) {
            return cached.clone();
        }
        let layout = crate::layout::layout_text(&mut self.fonts, text, style, max_width);
        self.shapes.insert(text, style, max_width, layout.clone());
        layout
    }

    /// Lays out a paragraph without consulting or filling the cache.
    ///
    /// For one-off measurement of text that will never be drawn again, where
    /// caching would only evict something useful.
    pub fn layout_uncached(
        &mut self,
        text: &str,
        style: &TextStyle,
        max_width: Option<Px>,
    ) -> TextLayout {
        crate::layout::layout_text(&mut self.fonts, text, style, max_width)
    }

    /// Atlas regions changed since the last call, for the renderer to upload.
    #[inline]
    pub fn take_dirty_regions(&mut self) -> Vec<DirtyRegion<'_>> {
        self.atlas.take_dirty_regions()
    }

    /// Drops glyphs that have gone unused for `max_idle_frames`.
    #[inline]
    pub fn evict_unused_glyphs(&mut self, max_idle_frames: u64) -> usize {
        self.atlas.evict_unused(max_idle_frames)
    }

    /// Clears every cache. Used when fonts change under the application.
    pub fn clear_caches(&mut self) {
        self.shapes.clear();
        self.atlas = GlyphAtlas::with_config(self.atlas.config());
        #[cfg(windows)]
        if let Some(Some(dw)) = &mut self.dwrite {
            dw.clear();
        }
        self.stats = TextSystemStats::default();
    }

    /// Rasterises a glyph with the platform rasteriser, if it applies.
    ///
    /// `None` means "use the outline rasteriser": not Windows, the outline
    /// rasteriser was asked for, DirectWrite is unavailable, or it could not
    /// load this face.
    #[cfg(windows)]
    fn rasterize_platform(
        &mut self,
        font: FontId,
        glyph: GlyphId,
        size_px: f32,
        format: GlyphFormat,
        subpixel_phase: u8,
    ) -> Option<GlyphImage> {
        if self.rasterizer != GlyphRasterizer::Platform {
            return None;
        }
        let dw =
            self.dwrite.get_or_insert_with(crate::dwrite::DirectWriteRasterizer::new).as_mut()?;
        dw.rasterize(&self.fonts, font, glyph, size_px, subpixel_phase, format)
    }

    #[cfg(not(windows))]
    fn rasterize_platform(
        &mut self,
        _font: FontId,
        _glyph: GlyphId,
        _size_px: f32,
        _format: GlyphFormat,
        _subpixel_phase: u8,
    ) -> Option<GlyphImage> {
        None
    }

    /// Rasterises a glyph with the portable outline rasteriser.
    fn rasterize_outline(
        &mut self,
        font: FontId,
        glyph: GlyphId,
        size_px: f32,
        subpixel: bool,
        subpixel_phase: u8,
    ) -> Option<GlyphImage> {
        let zones = self.fonts.vertical_zones(font);
        self.fonts
            .with_outline_face(font, |face| {
                if subpixel {
                    rasterize_glyph_subpixel_with_zones(face, glyph, size_px, subpixel_phase, zones)
                } else {
                    rasterize_glyph_with_zones_at_phase(face, glyph, size_px, subpixel_phase, zones)
                }
            })?
            .ok()
    }

    /// Rasterises and inserts a glyph, returning its placement.
    fn rasterize(
        &mut self,
        key: GlyphKey,
        font: FontId,
        glyph: GlyphId,
        size_px: f32,
        subpixel: bool,
        subpixel_phase: u8,
    ) -> Option<crate::types::AtlasPlacement> {
        let format = if subpixel { GlyphFormat::Subpixel } else { GlyphFormat::Grayscale };
        let (image, platform) =
            match self.rasterize_platform(font, glyph, size_px, format, subpixel_phase) {
                Some(image) => (image, true),
                None => {
                    (self.rasterize_outline(font, glyph, size_px, subpixel, subpixel_phase)?, false)
                }
            };

        // A glyph with no ink — a space, a control character — is a normal
        // result, not a failure. It occupies no atlas area and is reported as a
        // zero-extent placement that the batch compiler skips.
        if image.is_empty() {
            return Some(crate::types::AtlasPlacement {
                page: 0,
                uv: [0.0; 4],
                // `Rect<u32>` has no `ZERO`: `Scalar` requires `Neg`, which
                // unsigned texel coordinates cannot provide.
                texels: spherekit_core::Rect {
                    origin: spherekit_core::Point { x: 0, y: 0 },
                    size: spherekit_core::Size { width: 0, height: 0 },
                },
                bounds_em: image.bounds_em,
                format: image.format,
            });
        }

        match self.atlas.insert(key, &image) {
            Ok(p) => {
                self.stats.glyph_misses += 1;
                self.stats.bitmap_glyphs += 1;
                if subpixel {
                    self.stats.subpixel_glyphs += 1;
                }
                if platform {
                    self.stats.platform_glyphs += 1;
                }
                Some(p)
            }
            Err(e) => {
                // A full atlas must degrade to a missing glyph, never to a
                // failed frame. One dropped label is recoverable; a blank
                // window is not.
                tracing::warn!(target: "spherekit_text", "glyph atlas insert failed: {e}");
                self.stats.glyph_failures += 1;
                None
            }
        }
    }
}

impl GlyphProvider for TextSystem {
    fn place_glyph(&mut self, request: GlyphRequest) -> Option<GlyphPlacement> {
        let scale = ScaleFactor::new(request.device_scale);
        let subpixel_phase = request.subpixel_phase.min(3);
        let size_key = bitmap_size_px(request.font_size, scale);
        let key = if request.subpixel {
            GlyphKey::subpixel_bitmap(request.font, request.glyph, 0, size_key, subpixel_phase)
        } else {
            GlyphKey::bitmap_with_phase(request.font, request.glyph, 0, size_key, subpixel_phase)
        };

        let placement = if let Some(p) = self.atlas.touch(&key) {
            self.stats.glyph_hits += 1;
            p
        } else {
            let size_px = crate::raster::device_font_size(request.font_size, scale);
            self.rasterize(
                key,
                request.font,
                request.glyph,
                size_px,
                request.subpixel,
                subpixel_phase,
            )?
        };

        let b = placement.bounds_em;
        Some(GlyphPlacement {
            page: placement.page,
            uv: placement.uv,
            bounds_em: [b.min_x(), b.min_y(), b.width(), b.height()],
            is_subpixel: placement.format == GlyphFormat::Subpixel,
            texel_size: [placement.texels.size.width, placement.texels.size.height],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FontRequest;
    use spherekit_core::px;

    /// A system with a real face, or `None` on a machine with no usable fonts.
    ///
    /// Never fails the suite: which fonts exist is an environment fact.
    fn with_font() -> Option<(TextSystem, FontId)> {
        let mut system = TextSystem::with_system_fonts();
        let font = system.fonts_mut().resolve(&FontRequest::default())?;
        Some((system, font))
    }

    fn request(font: FontId, glyph: GlyphId, size: f32, scale: f32) -> GlyphRequest {
        GlyphRequest {
            font,
            glyph,
            font_size: px(size),
            device_scale: scale,
            subpixel: false,
            subpixel_phase: 0,
        }
    }

    #[test]
    fn an_empty_system_serves_no_glyphs_and_does_not_panic() {
        let mut system = TextSystem::new();
        let r = request(FontId::new(0, 1), GlyphId(1), 14.0, 1.0);
        assert_eq!(system.place_glyph(r), None);
        assert_eq!(system.stats().glyph_failures, 0, "a missing face is not an atlas failure");
    }

    #[test]
    fn hit_rate_of_an_untouched_system_is_one() {
        assert_eq!(TextSystemStats::default().hit_rate(), 1.0);
    }

    #[test]
    fn the_second_request_for_a_glyph_is_a_cache_hit() {
        let Some((mut system, font)) = with_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let Some(glyph) = system.fonts().glyph_index(font, 'A') else { return };

        let r = request(font, glyph, 20.0, 1.0);
        let first = system.place_glyph(r).expect("glyph A should rasterise");
        let second = system.place_glyph(r).expect("glyph A should still resolve");
        assert_eq!(first, second);
        assert_eq!(system.stats().glyph_misses, 1);
        assert_eq!(system.stats().glyph_hits, 1);
    }

    #[test]
    fn bitmap_cache_separates_quarter_phases_and_rgb_coverage() {
        let Some((mut system, font)) = with_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let Some(glyph) = system.fonts().glyph_index(font, 'A') else { return };
        let base = request(font, glyph, 16.0, 1.0);

        let gray0 = system.place_glyph(base).expect("gray phase zero");
        let gray1 =
            system.place_glyph(GlyphRequest { subpixel_phase: 1, ..base }).expect("gray phase one");
        let gray1_again = system
            .place_glyph(GlyphRequest { subpixel_phase: 1, ..base })
            .expect("gray phase one cache hit");
        let rgb1 = system
            .place_glyph(GlyphRequest { subpixel: true, subpixel_phase: 1, ..base })
            .expect("RGB phase one");

        assert_ne!(gray0.uv, gray1.uv, "phases aliased in the atlas");
        assert_eq!(gray1, gray1_again);
        assert!(!gray1.is_subpixel);
        assert!(rgb1.is_subpixel);
        assert_ne!(gray1.page, rgb1.page, "R8 and RGB8 coverage shared a page");
        assert_eq!(system.stats().glyph_misses, 3);
        assert_eq!(system.stats().glyph_hits, 1);
        assert_eq!(system.stats().subpixel_glyphs, 1);
    }

    #[test]
    fn each_device_size_gets_its_own_bitmap_at_exactly_that_size() {
        let Some((mut system, font)) = with_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let Some(glyph) = system.fonts().glyph_index(font, 'M') else { return };

        let small = system.place_glyph(request(font, glyph, 12.0, 1.0)).expect("12 px");
        let large = system.place_glyph(request(font, glyph, 48.0, 1.0)).expect("48 px");
        assert_ne!(small.uv, large.uv, "two sizes aliased in the atlas");
        // A capital M is roughly 0.7 em tall, so the bitmaps scale with the size.
        assert!(large.texel_size[1] > small.texel_size[1] * 3, "{small:?} vs {large:?}");
        // And 13 logical px at 2x is the same device size as 26 at 1x.
        let hidpi = system.place_glyph(request(font, glyph, 13.0, 2.0)).expect("13 px @2x");
        let lodpi = system.place_glyph(request(font, glyph, 26.0, 1.0)).expect("26 px @1x");
        assert_eq!(hidpi, lodpi, "the cache must key on device pixels");
        assert_eq!(system.stats().bitmap_glyphs, 3);
    }

    #[cfg(windows)]
    #[test]
    fn windows_rasterises_through_directwrite_by_default() {
        let Some((mut system, font)) = with_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let Some(glyph) = system.fonts().glyph_index(font, 'g') else { return };
        system.place_glyph(request(font, glyph, 14.0, 1.0)).expect("g");
        assert_eq!(system.stats().platform_glyphs, 1, "{:?}", system.stats());
    }

    #[test]
    fn the_outline_rasteriser_can_be_forced() {
        let Some((mut system, font)) = with_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        system.set_rasterizer(GlyphRasterizer::Outline);
        let Some(glyph) = system.fonts().glyph_index(font, 'g') else { return };
        let p = system.place_glyph(request(font, glyph, 14.0, 1.0)).expect("g");
        assert!(p.texel_size[0] > 0 && p.texel_size[1] > 0);
        assert_eq!(system.stats().platform_glyphs, 0);
    }

    #[test]
    fn a_space_resolves_to_a_zero_area_placement_rather_than_a_failure() {
        let Some((mut system, font)) = with_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let Some(glyph) = system.fonts().glyph_index(font, ' ') else { return };
        let p = system.place_glyph(request(font, glyph, 16.0, 1.0)).expect("a space must resolve");
        assert_eq!(p.uv, [0.0; 4], "a space must not occupy atlas area");
        assert_eq!(system.stats().glyph_failures, 0);
    }

    #[test]
    fn rasterising_marks_atlas_regions_dirty_exactly_once() {
        let Some((mut system, font)) = with_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let Some(glyph) = system.fonts().glyph_index(font, 'W') else { return };
        system.place_glyph(request(font, glyph, 24.0, 1.0)).expect("W");

        assert!(!system.take_dirty_regions().is_empty(), "a new glyph must dirty its page");
        assert!(
            system.take_dirty_regions().is_empty(),
            "draining twice must not re-upload the same texels"
        );
    }

    #[test]
    fn layout_produces_glyphs_for_multilingual_text() {
        let Some((mut system, _)) = with_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let layout = system.layout("Hello ไทย 日本語", &TextStyle::default(), None);
        assert!(layout.glyph_count() > 0);
        assert!(layout.size.width > Px::ZERO);
    }

    #[test]
    fn changing_the_rasteriser_invalidates_resident_glyphs() {
        // Two rasterisers hint and filter differently; keeping the old glyphs
        // would put two renditions of the same letter side by side.
        let Some((mut system, font)) = with_font() else {
            eprintln!("no system font; skipping");
            return;
        };
        let Some(glyph) = system.fonts().glyph_index(font, 'A') else { return };
        system.place_glyph(request(font, glyph, 24.0, 1.0));
        assert!(!system.atlas().is_empty());

        system.set_rasterizer(GlyphRasterizer::Platform);
        assert!(!system.atlas().is_empty(), "an idempotent set threw the atlas away");
        system.set_rasterizer(GlyphRasterizer::Outline);
        assert!(system.atlas().is_empty(), "stale glyphs survived a rasteriser change");
    }
}
