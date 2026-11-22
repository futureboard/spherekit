//! Colour conversion between the engine's contract and Skia.
//!
//! ## The strategy
//!
//! The batch compiler hands every backend **linear-light, premultiplied**
//! colours, because the GPU path renders into an sRGB swap chain whose hardware
//! encode runs after blending: there, blending happens in linear light.
//!
//! Skia's raster pipeline blends in the *destination's* encoding. Every surface
//! this crate creates is an 8-bit surface tagged sRGB, so Skia blends
//! sRGB-encoded values — gamma-space blending, which is what every traditional
//! 2D rasteriser does. Colours are therefore converted once on the way in:
//! unpremultiply, encode each channel with the sRGB transfer function, and hand
//! Skia a straight-alpha sRGB [`Color4f`]. Because the surface and the paint
//! colour share one colour space, Skia's colour-space transform steps reduce to
//! nothing and cost nothing per pixel.
//!
//! Two consequences, both deliberate:
//!
//! * **Solid colours land on exactly the same 8-bit value** as on the GPU path.
//!   Only partial coverage and translucent overlaps differ, and they differ in
//!   the direction of gamma-space blending.
//! * **`GlyphRun::coverage_contrast` is ignored.** That exponent exists only to
//!   bend coverage so that a *linear-light* blend followed by the sRGB encode
//!   reproduces a *gamma-space* blend. Skia already blends in gamma space, so
//!   applying it here would correct an error that was never made and visibly
//!   thin every glyph.
//!
//! Gradients are the one place that keeps linear light: their stops are handed
//! to Skia untouched, tagged linear sRGB, with linear premultiplied
//! interpolation, so a red-to-green ramp passes through the same yellow it does
//! on the GPU rather than a muddy brown.

use skia_safe::{Color, Color4f};
use std::sync::LazyLock;

/// Entries in the linear-to-sRGB lookup table, minus one.
///
/// 4096 steps put the worst-case error, which sits right at black where the
/// transfer function is steepest, at about a twentieth of one 8-bit step once
/// interpolated. A `powf` per channel per primitive is measurable on a frame
/// with thousands of glyphs; a table lookup is not.
const LUT_STEPS: usize = 4096;

static LINEAR_TO_SRGB: LazyLock<Box<[f32]>> = LazyLock::new(|| {
    (0..=LUT_STEPS)
        .map(|i| spherekit_core::linear_to_srgb(i as f32 / LUT_STEPS as f32))
        .collect::<Vec<_>>()
        .into_boxed_slice()
});

/// Encodes one linear-light channel as sRGB, clamped to `0..=1`.
#[inline]
pub(crate) fn encode(v: f32) -> f32 {
    // A NaN channel becomes black rather than poisoning the interpolation.
    let x = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) } * LUT_STEPS as f32;
    let i = (x as usize).min(LUT_STEPS - 1);
    let t = x - i as f32;
    let lut = &*LINEAR_TO_SRGB;
    lut[i] + (lut[i + 1] - lut[i]) * t
}

/// Converts a linear premultiplied colour into the straight-alpha sRGB colour
/// Skia expects from a paint.
#[inline]
pub(crate) fn to_srgb(c: [f32; 4]) -> Color4f {
    let a = c[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        return Color4f::new(0.0, 0.0, 0.0, 0.0);
    }
    let inv = 1.0 / a;
    Color4f::new(encode(c[0] * inv), encode(c[1] * inv), encode(c[2] * inv), a)
}

/// Same as [`to_srgb`], quantised to Skia's 8-bit `Color`, which is what
/// vertex colours are stored as.
#[inline]
pub(crate) fn to_srgb8(c: [f32; 4]) -> Color {
    let f = to_srgb(c);
    let q = |v: f32| (v * 255.0 + 0.5) as u8;
    Color::from_argb(q(f.a), q(f.r), q(f.g), q(f.b))
}

/// Recovers straight-alpha linear components for a gradient stop.
#[inline]
pub(crate) fn unpremultiply(c: [f32; 4]) -> Color4f {
    let a = c[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        return Color4f::new(0.0, 0.0, 0.0, 0.0);
    }
    Color4f::new(c[0] / a, c[1] / a, c[2] / a, a)
}

/// Turns an authoring-side clear colour into what an opaque surface shows.
///
/// A translucent clear on an opaque window has nothing behind it to blend
/// with, so it is composited over black — what the GPU path's opaque swap chain
/// presents as well.
#[inline]
pub(crate) fn clear_color(c: spherekit_core::Color, opaque: bool) -> Color4f {
    let a = c.a.clamp(0.0, 1.0);
    if opaque {
        // Over black in linear light, then re-encoded, to agree with a GPU
        // blend into an sRGB target.
        let over = |v: f32| encode(spherekit_core::srgb_to_linear(v) * a);
        Color4f::new(over(c.r), over(c.g), over(c.b), 1.0)
    } else {
        Color4f::new(c.r.clamp(0.0, 1.0), c.g.clamp(0.0, 1.0), c.b.clamp(0.0, 1.0), a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_agrees_with_the_transfer_function() {
        for i in 0..=1000 {
            let v = i as f32 / 1000.0;
            let exact = spherekit_core::linear_to_srgb(v);
            assert!((encode(v) - exact).abs() < 2e-4, "{v}: {} vs {exact}", encode(v));
        }
    }

    #[test]
    fn out_of_range_and_nan_channels_are_clamped() {
        assert_eq!(encode(-1.0), 0.0);
        assert_eq!(encode(f32::NAN), 0.0);
        assert!((encode(4.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn an_opaque_linear_colour_round_trips_to_its_authoring_value() {
        let authored = spherekit_core::Color::hex(0x1E88E5);
        let c = to_srgb(authored.to_linear().to_array());
        assert!((c.r - authored.r).abs() < 1e-3);
        assert!((c.g - authored.g).abs() < 1e-3);
        assert!((c.b - authored.b).abs() < 1e-3);
        assert_eq!(c.a, 1.0);
    }

    #[test]
    fn translucent_colours_are_unpremultiplied_before_encoding() {
        let authored = spherekit_core::Color::hex(0xFF8000).scale_alpha(0.25);
        let c = to_srgb(authored.to_linear().to_array());
        assert!((c.r - 1.0).abs() < 1e-3);
        assert!((c.g - authored.g).abs() < 2e-3);
        assert!((c.a - 0.25).abs() < 1e-6);
    }

    #[test]
    fn transparent_is_transparent_black() {
        assert_eq!(to_srgb([0.3, 0.3, 0.3, 0.0]).a, 0.0);
    }
}
