//! End-to-end rendering: real scenes, the real batch compiler, read-back
//! pixels.
//!
//! Expected values are sRGB bytes. Where a value depends on blending, it is the
//! gamma-space result this backend produces by design (see the crate's colour
//! strategy), not the linear-light result of the GPU path.

mod common;

use common::{Harness, assert_rgb, config, font};
use spherekit_core::{
    BlendMode, Brush, Color, Corners, GlyphId, Gradient, GradientStop, ImageFit, ImageId, Path,
    Point, Px, RoundedRect, Shadow, SurfaceError, px, rect, size,
};
use spherekit_render::{
    Filter, GlyphPlacement, GlyphRun, PositionedGlyph, RendererBackend, SurfaceConfig,
};
use spherekit_skia::SkiaRenderer;

fn gradient_linear(x0: f32, x1: f32, a: Color, b: Color) -> Gradient {
    Gradient::Linear {
        start: Point::new(px(x0), px(0.0)),
        end: Point::new(px(x1), px(0.0)),
        stops: vec![GradientStop::new(0.0, a), GradientStop::new(1.0, b)].into(),
    }
}

// ------------------------------------------------------------------ basics

#[test]
fn a_solid_quad_lands_on_its_authored_srgb_value() {
    let mut h = Harness::new(64, 64);
    let img = h.draw(Color::BLACK, |c| {
        c.fill_rect(rect(px(8.0), px(8.0), px(32.0), px(32.0)), Color::hex(0x1E88E5));
    });
    assert_rgb(&img, 20, 20, [0x1E, 0x88, 0xE5], 1);
    assert_rgb(&img, 4, 4, [0, 0, 0], 0);
    assert_rgb(&img, 40, 40, [0, 0, 0], 0);
    assert_eq!(img.at(20, 20)[3], 255);
}

#[test]
fn a_clear_only_frame_fills_every_pixel() {
    let mut h = Harness::new(32, 16);
    let img = h.draw(Color::hex(0x336699), |_| {});
    for y in 0..16 {
        for x in 0..32 {
            assert_rgb(&img, x, y, [0x33, 0x66, 0x99], 1);
        }
    }
}

#[test]
fn a_translucent_clear_on_an_opaque_surface_is_composited_over_black() {
    let mut h = Harness::new(8, 8);
    let img = h.draw(Color::WHITE.with_alpha(0.5), |_| {});
    // Over black in linear light, as an opaque GPU swap chain shows it:
    // 0.5 linear is sRGB 188.
    assert_rgb(&img, 4, 4, [188, 188, 188], 2);
    assert_eq!(img.at(4, 4)[3], 255);
}

#[test]
fn a_transparent_offscreen_surface_keeps_alpha() {
    let mut r =
        SkiaRenderer::new_offscreen(SurfaceConfig { transparent: true, ..config(8, 8, 1.0) })
            .unwrap();
    let compiled = spherekit_render::CompiledFrame::default();
    let mut f = r.begin_frame().unwrap();
    r.render(&mut f, &compiled, Color::TRANSPARENT).unwrap();
    r.end_frame(f).unwrap();
    assert_eq!(&r.read_pixels().unwrap()[..4], &[0, 0, 0, 0]);
}

#[test]
fn rounded_corners_are_antialiased() {
    let mut h = Harness::new(100, 100);
    let img = h.draw(Color::BLACK, |c| {
        c.fill_rounded_rect(
            RoundedRect::uniform(rect(px(10.0), px(10.0), px(80.0), px(80.0)), px(20.0)),
            Color::WHITE,
        );
    });
    // The bounding box's corner lies outside the arc.
    assert_rgb(&img, 10, 10, [0, 0, 0], 0);
    assert_rgb(&img, 50, 50, [255, 255, 255], 0);
    // A straight edge is still crisp.
    assert_rgb(&img, 10, 50, [255, 255, 255], 0);
    // Somewhere along the diagonal through the corner the arc crosses a pixel
    // partially.
    let partial = (10..25).map(|k| img.at(k, k)[0]).filter(|v| *v > 10 && *v < 245).count();
    assert!(partial >= 1, "no antialiased pixel on the corner arc");
}

#[test]
fn borders_are_inset_and_fill_the_interior_with_the_fill() {
    let mut h = Harness::new(100, 100);
    let img = h.draw(Color::BLACK, |c| {
        c.stroke_rect(rect(px(10.0), px(10.0), px(80.0), px(80.0)), Color::RED, px(4.0));
        c.quad(
            rect(px(30.0), px(30.0), px(40.0), px(40.0)),
            Corners::all(px(6.0)),
            Some(Brush::Solid(Color::BLUE)),
            Color::GREEN,
            px(3.0),
        );
    });
    assert_rgb(&img, 10, 50, [255, 0, 0], 0);
    assert_rgb(&img, 13, 50, [255, 0, 0], 0);
    assert_rgb(&img, 14, 50, [0, 0, 0], 0);
    assert_rgb(&img, 9, 50, [0, 0, 0], 0);
    assert_rgb(&img, 20, 50, [0, 0, 0], 0);
    // The filled-and-bordered quad.
    assert_rgb(&img, 31, 50, [0, 255, 0], 0);
    assert_rgb(&img, 50, 50, [0, 0, 255], 0);
}

// --------------------------------------------------------------- gradients

#[test]
fn linear_gradients_hit_their_endpoints_and_interpolate_in_linear_light() {
    let mut h = Harness::new(200, 20);
    let img = h.draw(Color::BLACK, |c| {
        c.fill_rect_with(
            rect(px(0.0), px(0.0), px(200.0), px(20.0)),
            Brush::Gradient(gradient_linear(0.0, 200.0, Color::RED, Color::BLUE)),
        );
    });
    assert_rgb(&img, 0, 10, [255, 0, 0], 8);
    assert_rgb(&img, 199, 10, [0, 0, 255], 8);
    // Halfway in linear light is (0.5, 0, 0.5), sRGB 188 — not the 128 of a
    // gamma-space ramp.
    assert_rgb(&img, 100, 10, [188, 0, 188], 4);
}

#[test]
fn radial_gradients_honour_separate_radii() {
    let mut h = Harness::new(200, 100);
    let img = h.draw(Color::BLACK, |c| {
        c.fill_rect_with(
            rect(px(0.0), px(0.0), px(200.0), px(100.0)),
            Brush::Gradient(Gradient::Radial {
                center: Point::new(px(100.0), px(50.0)),
                radius: size(px(80.0), px(40.0)),
                stops: vec![
                    GradientStop::new(0.0, Color::RED),
                    GradientStop::new(1.0, Color::BLUE),
                ]
                .into(),
            }),
        );
    });
    // The pixel centre is a fraction of a pixel off-centre, and the sRGB encode
    // is steep near zero: t = 0.014 is already blue 31.
    assert_rgb(&img, 100, 50, [255, 0, 0], 35);
    // Half way out along each axis is the same ramp position, despite the
    // different radii.
    let along_x = img.at(140, 50);
    let along_y = img.at(100, 70);
    for i in 0..3 {
        assert!(along_x[i].abs_diff(along_y[i]) <= 6, "{along_x:?} vs {along_y:?}");
    }
    assert_rgb(&img, 140, 50, [188, 0, 188], 6);
    // Past the radius the ramp clamps to the last stop.
    assert_rgb(&img, 2, 50, [0, 0, 255], 2);
}

#[test]
fn sweep_gradients_start_at_their_start_angle() {
    let mut h = Harness::new(100, 100);
    let img = h.draw(Color::BLACK, |c| {
        c.fill_rect_with(
            rect(px(0.0), px(0.0), px(100.0), px(100.0)),
            Brush::Gradient(Gradient::Sweep {
                center: Point::new(px(50.0), px(50.0)),
                start_angle: 0.0,
                sweep: 1.0,
                stops: vec![
                    GradientStop::new(0.0, Color::RED),
                    GradientStop::new(1.0, Color::BLUE),
                ]
                .into(),
            }),
        );
    });
    // Just clockwise of +x (y points down) is the start; just anticlockwise
    // is the end of the turn.
    // (Tolerances allow for the few thousandths of a turn between the pixel
    // centre and the axis, which the steep low end of the sRGB encode turns
    // into a couple of dozen levels.)
    assert_rgb(&img, 95, 51, [255, 0, 0], 30);
    assert_rgb(&img, 95, 48, [0, 0, 255], 30);
    // A quarter turn: 25% of the way to blue, in linear light.
    assert_rgb(&img, 50, 95, [225, 0, 137], 6);
}

// ----------------------------------------------------------------- shadows

#[test]
fn outer_shadows_spill_outside_their_shape() {
    let mut h = Harness::new(200, 200);
    let img = h.draw(Color::WHITE, |c| {
        c.draw_shadow(
            RoundedRect::uniform(rect(px(50.0), px(50.0), px(100.0), px(100.0)), Px::ZERO),
            &Shadow {
                offset: size(px(0.0), px(0.0)),
                blur_radius: px(20.0),
                spread: Px::ZERO,
                color: Color::BLACK,
                inset: false,
            },
        );
    });
    let outside = img.at(44, 100)[0];
    assert!(outside > 20 && outside < 230, "shadow tail 6px outside = {outside}");
    // At the edge the blur is at half strength.
    assert_rgb(&img, 50, 100, [128, 128, 128], 16);
    assert_rgb(&img, 100, 100, [0, 0, 0], 2);
    // Five sigma out there is nothing left.
    assert_rgb(&img, 2, 100, [255, 255, 255], 1);
}

#[test]
fn inset_shadows_darken_only_inside_the_edge() {
    let mut h = Harness::new(200, 200);
    let img = h.draw(Color::WHITE, |c| {
        c.draw_shadow(
            RoundedRect::uniform(rect(px(50.0), px(50.0), px(100.0), px(100.0)), px(8.0)),
            &Shadow {
                offset: size(px(0.0), px(0.0)),
                blur_radius: px(20.0),
                spread: Px::ZERO,
                color: Color::BLACK,
                inset: true,
            },
        );
    });
    assert_rgb(&img, 100, 100, [255, 255, 255], 2);
    let edge = img.at(51, 100)[0];
    assert!(edge < 180, "inset shadow at the edge = {edge}");
    assert_rgb(&img, 45, 100, [255, 255, 255], 0);
}

// ------------------------------------------------------------------ glyphs

/// A 16×16 page with one 8×8 glyph at texels 4..12: the left half at full
/// coverage, the right half at half coverage.
fn glyph_page() -> Vec<u8> {
    let mut page = vec![0u8; 16 * 16 * 4];
    for y in 0..16 {
        for x in 0..16 {
            let i = (y * 16 + x) * 4;
            let cov = match (x, y) {
                (4..8, 4..12) => 255,
                (8..12, 4..12) => 128,
                _ => 0,
            };
            page[i..i + 4].copy_from_slice(&[cov, cov, cov, 255]);
        }
    }
    page
}

fn glyph_placement() -> GlyphPlacement {
    GlyphPlacement {
        page: 0,
        uv: [4.0 / 16.0, 4.0 / 16.0, 12.0 / 16.0, 12.0 / 16.0],
        bounds_em: [0.0, -1.0, 0.5, 0.5],
        is_subpixel: false,
        texel_size: [8, 8],
    }
}

fn one_glyph_run(contrast: f32) -> GlyphRun {
    GlyphRun {
        font: font(),
        font_size: px(16.0),
        glyphs: vec![PositionedGlyph {
            glyph: GlyphId(1),
            position: Point::new(px(20.0), px(40.0)),
        }]
        .into(),
        coverage_contrast: contrast,
    }
}

#[test]
fn glyphs_sample_their_atlas_page_as_plain_coverage() {
    let mut h = Harness::new(64, 64);
    let page = h.renderer.upload_texture(None, 16, 16, &vec![0; 16 * 16 * 4]).unwrap();
    h.renderer.set_atlas_page(0, page);
    // Filled in afterwards through a region upload, the way the atlas grows.
    h.renderer.upload_texture_region(page, 0, 0, 16, 16, &glyph_page()).unwrap();
    h.glyphs.0 = Some(glyph_placement());

    // A strong contrast exponent, which must be ignored: Skia already blends in
    // gamma space.
    let img = h.draw(Color::BLACK, |c| c.draw_glyph_run(one_glyph_run(2.2), Color::WHITE));
    // The quad spans x 20..28, y 24..32 (16 px em, ink box one em above the
    // baseline at y 40).
    // (One level of tolerance: Skia's atlas pipeline runs at 8-bit precision.)
    assert_rgb(&img, 21, 26, [255, 255, 255], 1);
    assert_rgb(&img, 26, 26, [128, 128, 128], 2);
    assert_rgb(&img, 19, 26, [0, 0, 0], 0);
    assert_rgb(&img, 21, 33, [0, 0, 0], 0);
}

#[test]
fn glyph_colour_tints_the_coverage() {
    let mut h = Harness::new(64, 64);
    let page = h.renderer.upload_texture(None, 16, 16, &glyph_page()).unwrap();
    h.renderer.set_atlas_page(0, page);
    h.glyphs.0 = Some(glyph_placement());
    let img = h.draw(Color::WHITE, |c| c.draw_glyph_run(one_glyph_run(-1.0), Color::hex(0x1E88E5)));
    assert_rgb(&img, 21, 26, [0x1E, 0x88, 0xE5], 1);
}

#[test]
fn a_glyph_batch_on_an_unbound_page_is_skipped() {
    let mut h = Harness::new(64, 64);
    h.glyphs.0 = Some(glyph_placement());
    let img = h.draw(Color::BLACK, |c| c.draw_glyph_run(one_glyph_run(1.0), Color::WHITE));
    assert_rgb(&img, 21, 26, [0, 0, 0], 0);
}

// ------------------------------------------------------------------ layers

#[test]
fn a_layer_at_half_opacity_composites_as_a_group() {
    let mut h = Harness::new(100, 100);
    let img = h.draw(Color::BLACK, |c| {
        assert!(c.push_layer(
            rect(px(0.0), px(0.0), px(100.0), px(100.0)),
            0.5,
            BlendMode::Normal,
            None
        ));
        // Two overlapping opaque rects: group opacity means the overlap is no
        // brighter than either alone.
        c.fill_rect(rect(px(10.0), px(10.0), px(50.0), px(50.0)), Color::WHITE);
        c.fill_rect(rect(px(30.0), px(30.0), px(50.0), px(50.0)), Color::WHITE);
        c.end_layer();
    });
    assert_rgb(&img, 20, 20, [128, 128, 128], 2);
    assert_rgb(&img, 40, 40, [128, 128, 128], 2);
    assert_rgb(&img, 5, 5, [0, 0, 0], 0);
}

#[test]
fn an_offset_layer_lands_where_its_content_was_drawn() {
    // Offscreen layers are only as large as their bounds; the target origin
    // must be subtracted on the way in and added back on the way out.
    let mut h = Harness::with_scale(200, 200, 2.0);
    let img = h.draw(Color::BLACK, |c| {
        c.push_layer(rect(px(30.0), px(40.0), px(40.0), px(30.0)), 0.999, BlendMode::Normal, None);
        c.fill_rect(rect(px(35.0), px(45.0), px(10.0), px(10.0)), Color::RED);
        c.end_layer();
    });
    assert_rgb(&img, 71, 91, [255, 0, 0], 2);
    assert_rgb(&img, 89, 109, [255, 0, 0], 2);
    assert_rgb(&img, 69, 91, [0, 0, 0], 0);
    assert_rgb(&img, 91, 111, [0, 0, 0], 0);
}

#[test]
fn a_blur_filter_softens_the_layer() {
    let mut h = Harness::new(120, 120);
    let img = h.draw(Color::BLACK, |c| {
        c.push_layer(
            rect(px(0.0), px(0.0), px(120.0), px(120.0)),
            1.0,
            BlendMode::Normal,
            Some(Filter::Blur { sigma: px(4.0) }),
        );
        c.fill_rect(rect(px(40.0), px(40.0), px(40.0), px(40.0)), Color::WHITE);
        c.end_layer();
    });
    let edge = img.at(40, 60)[0];
    assert!(edge > 60 && edge < 200, "blurred edge = {edge}");
    assert_rgb(&img, 60, 60, [255, 255, 255], 2);
    assert_rgb(&img, 20, 60, [0, 0, 0], 1);
    let inside_tail = img.at(36, 60)[0];
    assert!(inside_tail > 0, "the blur did not spread outward");
}

#[test]
fn saturate_zero_produces_grey() {
    let mut h = Harness::new(64, 64);
    let img = h.draw(Color::BLACK, |c| {
        c.push_layer(
            rect(px(0.0), px(0.0), px(64.0), px(64.0)),
            1.0,
            BlendMode::Normal,
            Some(Filter::Saturate(0.0)),
        );
        c.fill_rect(rect(px(8.0), px(8.0), px(48.0), px(48.0)), Color::RED);
        c.end_layer();
    });
    let p = img.at(32, 32);
    assert!(p[0].abs_diff(p[1]) <= 1 && p[1].abs_diff(p[2]) <= 1, "not grey: {p:?}");
    // Rec. 709 luma of pure red, in linear light, is 0.2126: sRGB 127.
    assert_rgb(&img, 32, 32, [127, 127, 127], 3);
}

#[test]
fn a_colour_matrix_filter_is_applied() {
    let mut h = Harness::new(32, 32);
    #[rustfmt::skip]
    let swap_red_green = [
        0.0, 1.0, 0.0, 0.0, 0.0,
        1.0, 0.0, 0.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 0.0, 1.0, 0.0,
    ];
    let img = h.draw(Color::BLACK, |c| {
        c.push_layer(
            rect(px(0.0), px(0.0), px(32.0), px(32.0)),
            1.0,
            BlendMode::Normal,
            Some(Filter::ColorMatrix(swap_red_green)),
        );
        c.fill_rect(rect(px(0.0), px(0.0), px(32.0), px(32.0)), Color::RED);
        c.end_layer();
    });
    assert_rgb(&img, 16, 16, [0, 255, 0], 1);
}

#[test]
fn layer_blend_modes_are_honoured() {
    let mut h = Harness::new(32, 32);
    let img = h.draw(Color::BLACK, |c| {
        c.fill_rect(rect(px(0.0), px(0.0), px(32.0), px(32.0)), Color::RED);
        c.push_layer(rect(px(0.0), px(0.0), px(32.0), px(32.0)), 1.0, BlendMode::Multiply, None);
        c.fill_rect(rect(px(0.0), px(0.0), px(32.0), px(32.0)), Color::rgba8(128, 255, 255, 255));
        c.end_layer();
    });
    assert_rgb(&img, 16, 16, [128, 0, 0], 2);
}

#[test]
fn backdrop_blur_blurs_what_is_behind_the_layer() {
    let mut h = Harness::new(200, 100);
    let img = h.draw(Color::BLACK, |c| {
        c.fill_rect(rect(px(0.0), px(0.0), px(100.0), px(100.0)), Color::WHITE);
        c.push_layer(
            rect(px(50.0), px(20.0), px(100.0), px(60.0)),
            1.0,
            BlendMode::Normal,
            Some(Filter::BackdropBlur { sigma: px(6.0) }),
        );
        c.fill_rect(rect(px(50.0), px(20.0), px(100.0), px(60.0)), Color::BLUE.with_alpha(0.1));
        c.end_layer();
    });
    // Inside the frosted region the hard white/black edge at x = 100 is soft.
    let soft = img.at(100, 50)[0];
    assert!(soft > 40 && soft < 215, "backdrop edge inside the layer = {soft}");
    // Outside it the backdrop is untouched.
    assert_rgb(&img, 99, 5, [255, 255, 255], 0);
    assert_rgb(&img, 100, 5, [0, 0, 0], 0);
    // The layer's own content is drawn over the blurred backdrop.
    let tinted = img.at(140, 50);
    assert!(tinted[2] > tinted[0], "layer content missing: {tinted:?}");
}

// --------------------------------------------------------------- geometry

#[test]
fn a_filled_path_becomes_an_antialiased_mesh() {
    let mut h = Harness::new(100, 100);
    let img = h.draw(Color::BLACK, |c| {
        let mut p = Path::builder();
        p.move_to(Point::new(px(10.0), px(10.0)));
        p.line_to(Point::new(px(90.0), px(10.0)));
        p.line_to(Point::new(px(10.0), px(90.0)));
        p.close();
        c.fill_path(p.build(), Color::RED);
    });
    assert_rgb(&img, 20, 20, [255, 0, 0], 0);
    assert_rgb(&img, 80, 80, [0, 0, 0], 0);
    // The hypotenuse x + y = 100 runs through pixel (49, 50)'s centre.
    let edge = img.at(49, 50)[0];
    assert!(edge > 40 && edge < 230, "hypotenuse pixel = {edge}");
    // No seam inside, where the tessellator's triangles meet.
    for k in 12..45 {
        assert_rgb(&img, k, 12, [255, 0, 0], 0);
        assert_rgb(&img, 12, k, [255, 0, 0], 0);
        assert_rgb(&img, k, k, [255, 0, 0], 0);
    }
}

#[test]
fn a_stroked_path_is_drawn() {
    let mut h = Harness::new(100, 100);
    let img = h.draw(Color::BLACK, |c| {
        c.draw_line(
            Point::new(px(10.0), px(50.0)),
            Point::new(px(90.0), px(50.0)),
            Color::GREEN,
            px(6.0),
        );
    });
    assert_rgb(&img, 50, 50, [0, 255, 0], 0);
    assert_rgb(&img, 50, 40, [0, 0, 0], 0);
}

#[test]
fn a_vertex_coloured_mesh_is_drawn() {
    use spherekit_core::LinearColor;
    use spherekit_render::{Mesh, MeshVertex};
    let mut h = Harness::new(100, 100);
    let red: LinearColor = Color::RED.to_linear();
    let blue: LinearColor = Color::BLUE.to_linear();
    let mesh = Mesh {
        vertices: vec![
            MeshVertex::new(Point::new(px(0.0), px(0.0)), red),
            MeshVertex::new(Point::new(px(100.0), px(0.0)), blue),
            MeshVertex::new(Point::new(px(100.0), px(100.0)), blue),
            MeshVertex::new(Point::new(px(0.0), px(100.0)), red),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    };
    let img = h.draw(Color::BLACK, |c| c.draw_mesh(mesh, None));
    let left = img.at(2, 50);
    let right = img.at(97, 50);
    assert!(left[0] > 240 && left[2] < 20, "{left:?}");
    assert!(right[2] > 240 && right[0] < 20, "{right:?}");
}

#[test]
fn image_quads_show_their_texels_unchanged() {
    let mut h = Harness::new(100, 100);
    #[rustfmt::skip]
    let texels = [
        255, 0, 0, 255,    0, 255, 0, 255,
        0, 0, 255, 255,    100, 100, 100, 255,
    ];
    let tex = h.renderer.upload_texture(None, 2, 2, &texels).unwrap();
    h.textures.0 = Some(tex);
    let img = h.draw(Color::BLACK, |c| {
        c.draw_image(
            ImageId::new(0, 1),
            rect(px(0.0), px(0.0), px(100.0), px(100.0)),
            ImageFit::Fill,
        );
    });
    assert_rgb(&img, 5, 5, [255, 0, 0], 1);
    assert_rgb(&img, 95, 5, [0, 255, 0], 1);
    assert_rgb(&img, 5, 95, [0, 0, 255], 1);
    // An sRGB-encoded texel reaches the screen as the same byte.
    assert_rgb(&img, 95, 95, [100, 100, 100], 1);
}

#[test]
fn image_tint_and_rounded_corners_apply() {
    let mut h = Harness::new(100, 100);
    let tex = h.renderer.upload_texture(None, 1, 1, &[255, 255, 255, 255]).unwrap();
    h.textures.0 = Some(tex);
    let img = h.draw(Color::BLACK, |c| {
        c.draw_image_tinted(
            ImageId::new(0, 1),
            rect(px(10.0), px(10.0), px(80.0), px(80.0)),
            ImageFit::Fill,
            Color::hex(0x1E88E5),
            Corners::all(px(30.0)),
        );
    });
    assert_rgb(&img, 50, 50, [0x1E, 0x88, 0xE5], 2);
    assert_rgb(&img, 12, 12, [0, 0, 0], 0);
}

#[test]
fn a_destroyed_image_texture_draws_nothing() {
    let mut h = Harness::new(32, 32);
    let tex = h.renderer.upload_texture(None, 1, 1, &[255, 255, 255, 255]).unwrap();
    h.renderer.destroy_texture(tex);
    h.textures.0 = Some(tex);
    let img = h.draw(Color::BLACK, |c| {
        c.draw_image(
            ImageId::new(0, 1),
            rect(px(0.0), px(0.0), px(32.0), px(32.0)),
            ImageFit::Fill,
        );
    });
    assert_rgb(&img, 16, 16, [0, 0, 0], 0);
}

#[test]
fn rounded_clips_are_enforced() {
    let mut h = Harness::new(100, 100);
    let img = h.draw(Color::BLACK, |c| {
        c.clip_rounded_rect(RoundedRect::uniform(
            rect(px(10.0), px(10.0), px(80.0), px(80.0)),
            px(30.0),
        ));
        c.fill_rect(rect(px(0.0), px(0.0), px(100.0), px(100.0)), Color::WHITE);
    });
    assert_rgb(&img, 13, 13, [0, 0, 0], 0);
    assert_rgb(&img, 5, 50, [0, 0, 0], 0);
    assert_rgb(&img, 50, 50, [255, 255, 255], 0);
    assert_rgb(&img, 11, 50, [255, 255, 255], 0);
}

#[test]
fn transforms_and_a_non_unit_scale_factor_compose() {
    let mut h = Harness::with_scale(200, 200, 2.0);
    let img = h.draw(Color::BLACK, |c| {
        c.fill_rect(rect(px(10.0), px(10.0), px(20.0), px(20.0)), Color::RED);
        c.save();
        c.translate(size(px(50.0), px(0.0)));
        c.scale(2.0, 1.0);
        c.fill_rect(rect(px(0.0), px(10.0), px(10.0), px(20.0)), Color::BLUE);
        c.restore();
    });
    // Logical 10..30 is device 20..60.
    assert_rgb(&img, 21, 21, [255, 0, 0], 0);
    assert_rgb(&img, 58, 58, [255, 0, 0], 0);
    assert_rgb(&img, 19, 30, [0, 0, 0], 0);
    assert_rgb(&img, 61, 30, [0, 0, 0], 0);
    // Translate 50, scale x by 2: logical x 50..70, device 100..140.
    assert_rgb(&img, 101, 30, [0, 0, 255], 0);
    assert_rgb(&img, 138, 30, [0, 0, 255], 0);
    assert_rgb(&img, 141, 30, [0, 0, 0], 0);
}

#[test]
fn glyphs_stay_crisp_at_a_non_unit_scale_factor() {
    let mut h = Harness::with_scale(128, 128, 2.0);
    let page = h.renderer.upload_texture(None, 16, 16, &glyph_page()).unwrap();
    h.renderer.set_atlas_page(0, page);
    h.glyphs.0 = Some(glyph_placement());
    let img = h.draw(Color::BLACK, |c| c.draw_glyph_run(one_glyph_run(1.0), Color::WHITE));
    // The compiler snaps the quad to the 8×8 texels at device scale: logical
    // (20, 24) is device (40, 48).
    assert_rgb(&img, 40, 48, [255, 255, 255], 1);
    assert_rgb(&img, 43, 55, [255, 255, 255], 1);
    assert_rgb(&img, 44, 48, [128, 128, 128], 2);
    assert_rgb(&img, 48, 48, [0, 0, 0], 0);
}

// ----------------------------------------------------------- the backend

#[test]
fn zero_size_configuration_is_a_no_op() {
    let mut h = Harness::new(32, 32);
    h.renderer.configure_surface(config(0, 0, 1.0)).unwrap();
    let img = h.draw(Color::WHITE, |c| {
        c.fill_rect(rect(px(0.0), px(0.0), px(8.0), px(8.0)), Color::RED);
    });
    assert_eq!(img.data.len(), 32 * 32 * 4);
    assert_rgb(&img, 4, 4, [255, 0, 0], 0);
}

#[test]
fn a_renderer_created_at_zero_size_skips_frames() {
    let mut r = SkiaRenderer::new_offscreen(config(0, 0, 1.0)).unwrap();
    assert_eq!(r.begin_frame().unwrap_err(), SurfaceError::ZeroSized);
    r.configure_surface(config(4, 4, 1.0)).unwrap();
    assert!(r.begin_frame().is_ok());
}

#[test]
fn resizing_changes_the_read_back_size() {
    let mut h = Harness::new(16, 16);
    h.renderer.configure_surface(config(24, 8, 1.0)).unwrap();
    h.width = 24;
    h.height = 8;
    let img = h.draw(Color::WHITE, |_| {});
    assert_eq!(img.data.len(), 24 * 8 * 4);
}

#[test]
fn capabilities_are_honest() {
    let h = Harness::new(4, 4);
    let caps = h.renderer.capabilities();
    assert!(!caps.dual_source_blending);
    assert!(caps.offscreen_targets);
    assert!(!caps.compute);
    assert!(!caps.timestamp_queries);
    assert_eq!(caps.max_msaa_samples, 1);
    assert!(h.renderer.adapter_name().starts_with("Skia CPU raster"));
}

#[test]
fn frame_stats_report_the_workload() {
    let mut h = Harness::new(64, 64);
    let mut scene = h.scene();
    {
        let mut c = spherekit_render::Canvas::new(&mut scene);
        for i in 0..10 {
            c.fill_rect(rect(px(i as f32 * 4.0), px(0.0), px(3.0), px(3.0)), Color::RED);
        }
    }
    let mut compiler = spherekit_render::BatchCompiler::new();
    let compiled = compiler.compile(&scene, &mut h.glyphs, &mut h.textures);
    let mut f = h.renderer.begin_frame().unwrap();
    h.renderer.render(&mut f, compiled, Color::BLACK).unwrap();
    let stats = h.renderer.end_frame(f).unwrap();
    assert_eq!(stats.quads, 10);
    assert_eq!(stats.draw_calls, 10);
    assert!(stats.gpu_ms.is_none());
}

#[test]
fn memory_usage_tracks_textures_and_layers() {
    let mut h = Harness::new(16, 16);
    let base = h.renderer.memory_usage();
    let tex = h.renderer.upload_texture(None, 8, 8, &[0; 8 * 8 * 4]).unwrap();
    assert_eq!(h.renderer.memory_usage(), base + 256);
    h.renderer.destroy_texture(tex);
    assert_eq!(h.renderer.memory_usage(), base);
}

#[test]
fn rotated_and_rounded_clipped_glyphs_take_the_per_glyph_path() {
    let mut h = Harness::new(64, 64);
    let page = h.renderer.upload_texture(None, 16, 16, &glyph_page()).unwrap();
    h.renderer.set_atlas_page(0, page);
    h.glyphs.0 = Some(glyph_placement());
    let img = h.draw(Color::BLACK, |c| {
        // A quarter turn about the origin, then back into view: the glyph's
        // full-coverage left half ends up as its top half.
        c.save();
        c.translate(size(px(60.0), px(0.0)));
        c.rotate(spherekit_core::Rad(core::f32::consts::FRAC_PI_2));
        c.draw_glyph_run(one_glyph_run(1.0), Color::WHITE);
        c.restore();
    });
    // Rotating (x, y) by 90° gives (-y, x); local x 20..28, y 24..32 lands on
    // device x 28..36, y 20..28, full coverage in rows 20..24.
    assert_rgb(&img, 31, 21, [255, 255, 255], 2);
    assert_rgb(&img, 31, 26, [128, 128, 128], 4);
    assert_rgb(&img, 31, 30, [0, 0, 0], 0);

    let img = h.draw(Color::BLACK, |c| {
        c.clip_rounded_rect(RoundedRect::uniform(
            rect(px(22.0), px(0.0), px(40.0), px(64.0)),
            px(4.0),
        ));
        c.draw_glyph_run(one_glyph_run(1.0), Color::WHITE);
    });
    assert_rgb(&img, 21, 28, [0, 0, 0], 0);
    assert_rgb(&img, 23, 28, [255, 255, 255], 1);
}

#[test]
fn meshes_beyond_sixteen_bit_indices_are_drawn() {
    use spherekit_render::{Mesh, MeshVertex};
    let mut h = Harness::new(64, 64);
    // 70 000 vertices, most of them unused, with the triangle's corners at the
    // far end of the index range. Two colours keep it off the path route.
    let mut vertices = vec![MeshVertex::default(); 70_000];
    let red = Color::RED.to_linear();
    let green = Color::GREEN.to_linear();
    vertices[0] = MeshVertex::new(Point::new(px(0.0), px(0.0)), red);
    vertices[69_998] = MeshVertex::new(Point::new(px(64.0), px(0.0)), green);
    vertices[69_999] = MeshVertex::new(Point::new(px(0.0), px(64.0)), red);
    let mesh = Mesh { vertices, indices: vec![0, 69_998, 69_999] };
    let img = h.draw(Color::BLACK, |c| c.draw_mesh(mesh, None));
    let p = img.at(4, 4);
    assert!(p[0] > 200, "{p:?}");
    assert_rgb(&img, 60, 60, [0, 0, 0], 0);
}

#[test]
fn textured_meshes_multiply_texels_by_vertex_colour() {
    use spherekit_render::{Mesh, MeshVertex};
    let mut h = Harness::new(64, 64);
    let tex = h.renderer.upload_texture(None, 1, 1, &[255, 255, 0, 255]).unwrap();
    let white = Color::WHITE.to_linear();
    let v = |x: f32, y: f32| {
        MeshVertex::textured(Point::new(px(x), px(y)), [x / 64.0, y / 64.0], white)
    };
    let mesh = Mesh {
        vertices: vec![v(0.0, 0.0), v(64.0, 0.0), v(64.0, 64.0), v(0.0, 64.0)],
        indices: vec![0, 1, 2, 0, 2, 3],
    };
    let img = h.draw(Color::BLACK, |c| c.draw_mesh(mesh, Some(tex)));
    assert_rgb(&img, 32, 32, [255, 255, 0], 1);
}

#[test]
fn a_layer_drawn_before_anything_else_survives_later_surface_passes() {
    let mut h = Harness::new(64, 32);
    let mut scene = h.scene();
    {
        let mut c = spherekit_render::Canvas::new(&mut scene);
        c.push_layer(rect(px(0.0), px(0.0), px(32.0), px(32.0)), 0.5, BlendMode::Normal, None);
        c.fill_rect(rect(px(0.0), px(0.0), px(32.0), px(32.0)), Color::WHITE);
        c.end_layer();
        c.fill_rect(rect(px(32.0), px(0.0), px(32.0), px(32.0)), Color::RED);
    }
    let mut compiler = spherekit_render::BatchCompiler::new();
    let passes = compiler.compile(&scene, &mut h.glyphs, &mut h.textures).passes.clone();
    // The compiler marks the surface's first *own* pass as clearing even though
    // the layer has already composited into the surface by then. This backend
    // clears each target once, when it is acquired, so the layer survives.
    let surface_pass = passes.iter().find(|p| p.target == 0).unwrap();
    assert!(surface_pass.clear);
    let img = h.render(&scene, Color::BLACK);
    assert_rgb(&img, 16, 16, [128, 128, 128], 2);
    assert_rgb(&img, 48, 16, [255, 0, 0], 0);
}
