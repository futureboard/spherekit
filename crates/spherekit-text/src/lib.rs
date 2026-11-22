//! # spherekit-text
//!
//! The text stack: font discovery, complex-script shaping, line layout, and
//! glyph rasterisation into a coverage-bitmap atlas.
//!
//! ## Rasterisation
//!
//! Every glyph is rasterised at its exact device size and one of four
//! quarter-pixel horizontal phases, as grayscale or RGB-stripe (ClearType-style)
//! coverage. The renderer only ever samples and tints a bitmap, which is what
//! lets a CPU backend draw text exactly as cheaply as a GPU one.
//!
//! Two rasterisers produce those bitmaps, selected by [`GlyphRasterizer`]:
//!
//! * **DirectWrite** (Windows, the default there). Glyphs come out of
//!   `IDWriteGlyphRunAnalysis`, so text matches every other Windows
//!   application: the same symmetric antialiasing, the same vertical grid
//!   fitting, and ClearType filtering for the subpixel path.
//! * **Outline** (every platform). An exact-area scanline rasteriser over the
//!   font's own outlines ([`outline`], [`raster`]) with the light vertical
//!   hinting in [`hint`]. It is the fallback when DirectWrite cannot load a
//!   face, and the only rasteriser off Windows.
//!
//! ## Pipeline
//!
//! ```text
//! text + style
//!    -> bidi resolution + script itemisation   (shape)
//!    -> per-run shaping with fallback          (shape, font)
//!    -> line breaking and alignment            (layout)
//!    -> coverage rasterisation                 (dwrite | outline + raster)
//!    -> paged atlas placement                  (atlas)
//!    -> textured, tinted quads                 (spherekit-d3d11 | spherekit-skia)
//! ```

#![deny(missing_docs)]

pub mod atlas;
pub mod cache;
pub mod caret;
#[cfg(windows)]
pub mod dwrite;
pub mod font;
pub mod hint;
pub mod layout;
pub mod outline;
pub mod raster;
pub mod shape;
pub mod system;
pub mod system_ui;
pub mod types;

pub use system::{GlyphRasterizer, TextSystem, TextSystemStats};
pub use types::{
    AtlasPlacement, FontMetrics, FontRequest, FontStretch, FontStyle, FontWeight, GlyphFormat,
    GlyphImage, GlyphKey, GlyphMetrics, Overflow, ScaledFontMetrics, ShapedGlyph, ShapedRun,
    TextAlign, TextDirection, TextLayout, TextLine, TextStyle, VariationAxis, WrapMode,
};
