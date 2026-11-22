//! End-to-end rendering, measured on a real device.
//!
//! Each test builds a scene through the public `Canvas` API, compiles it with
//! the real `BatchCompiler`, renders it offscreen and reads the pixels back.
//! Nothing here reaches into the renderer's internals: a frame that compiles,
//! binds and draws but comes out wrong fails these tests, which is the defect
//! a shader port is most likely to have.
//!
//! The offscreen constructor uses WARP when there is no GPU, so these run on
//! any Windows machine. Where there is no device at all — or off Windows —
//! each test prints why and passes: no device is an environment fact, not a
//! defect.

#![cfg(windows)]

use spherekit_core::{
    BlendMode, Color, Corners, DevicePx, FontId, GlyphId, ImageFit, ImageId, Path, Point,
    RoundedRect, ScaleFactor, Shadow, Size, TextureId, linear_to_srgb, px, rect, size,
};
use spherekit_core::{Brush, Gradient};
use spherekit_d3d11::D3D11Renderer;
use spherekit_render::{
    BatchCompiler, Canvas, Filter, GlyphPlacement, GlyphProvider, GlyphRequest, GlyphRun,
    PositionedGlyph, RendererBackend, Scene, SurfaceConfig, TextureProvider,
};

const W: u32 = 128;
const H: u32 = 96;

fn config(width: u32, height: u32, msaa: u32) -> SurfaceConfig {
    SurfaceConfig {
        size: Size::new(DevicePx(width as i32), DevicePx(height as i32)),
        msaa_samples: msaa,
        ..SurfaceConfig::default()
    }
}

fn renderer_with(msaa: u32) -> Option<D3D11Renderer> {
    match D3D11Renderer::new_offscreen(config(W, H, msaa)) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("skipping: no Direct3D 11 device ({e})");
            None
        }
    }
}

fn renderer() -> Option<D3D11Renderer> {
    renderer_with(4)
}

/// A read-back frame, RGBA8, sRGB-encoded.
struct Pixels {
    data: Vec<u8>,
    width: u32,
}

impl Pixels {
    fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }
}

struct NoGlyphs;
impl GlyphProvider for NoGlyphs {
    fn place_glyph(&mut self, _: GlyphRequest) -> Option<GlyphPlacement> {
        None
    }
}

struct NoTextures;
impl TextureProvider for NoTextures {
    fn texture_for(&mut self, _: ImageId) -> Option<TextureId> {
        None
    }
}

/// Every glyph is a 16x16 bitmap on atlas page 0 covering the whole page,
/// one em square sitting on the baseline. Subpixel when the compiler allows it.
struct OneGlyph;
impl GlyphProvider for OneGlyph {
    fn place_glyph(&mut self, request: GlyphRequest) -> Option<GlyphPlacement> {
        Some(GlyphPlacement {
            page: 0,
            uv: [0.0, 0.0, 1.0, 1.0],
            bounds_em: [0.0, -1.0, 1.0, 1.0],
            is_subpixel: request.subpixel,
            texel_size: [16, 16],
        })
    }
}

struct OneTexture(TextureId);
impl TextureProvider for OneTexture {
    fn texture_for(&mut self, _: ImageId) -> Option<TextureId> {
        Some(self.0)
    }
}

fn render_with(
    r: &mut D3D11Renderer,
    clear: Color,
    subpixel: bool,
    glyphs: &mut dyn GlyphProvider,
    textures: &mut dyn TextureProvider,
    paint: impl FnOnce(&mut Canvas),
) -> Pixels {
    let mut scene = Scene::new(size(px(W as f32), px(H as f32)), ScaleFactor::IDENTITY);
    {
        let mut canvas = Canvas::new(&mut scene);
        paint(&mut canvas);
    }
    let mut compiler = BatchCompiler::new();
    compiler.set_subpixel_text_enabled(subpixel);
    let compiled = compiler.compile(&scene, glyphs, textures);
    let mut frame = r.begin_frame().expect("begin_frame");
    r.render(&mut frame, compiled, clear).expect("render");
    r.end_frame(frame).expect("end_frame");
    let data = r.read_pixels().expect("read_pixels");
    assert_eq!(data.len(), (W * H * 4) as usize);
    Pixels { data, width: W }
}

fn render(r: &mut D3D11Renderer, clear: Color, paint: impl FnOnce(&mut Canvas)) -> Pixels {
    render_with(r, clear, false, &mut NoGlyphs, &mut NoTextures, paint)
}

/// The 8-bit sRGB encoding of a linear value, as the hardware writes it.
fn encode(linear: f32) -> u8 {
    (linear_to_srgb(linear) * 255.0).round() as u8
}

fn near(actual: [u8; 4], expected: [u8; 4], tolerance: u8) -> bool {
    actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= tolerance)
}

#[track_caller]
fn assert_near(actual: [u8; 4], expected: [u8; 4], tolerance: u8) {
    assert!(
        near(actual, expected, tolerance),
        "got {actual:?}, expected {expected:?} ±{tolerance}"
    );
}

const BLACK: [u8; 4] = [0, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

#[test]
fn every_shader_compiles_from_source() {
    let compiled = spherekit_d3d11::shader::compile_all().expect("HLSL compiles");
    eprintln!("runtime compile of every entry point: {:?}", compiled.elapsed);
    for entry in spherekit_d3d11::shader::Entry::ALL {
        assert!(!compiled.get(entry).is_empty(), "{entry:?} produced no bytecode");
    }
    // The renderer loads the build script's bytecode; it must be the same
    // compiler's output for the same source, or the test above proves nothing.
    let loaded = spherekit_d3d11::shader::load().expect("embedded shaders");
    for entry in spherekit_d3d11::shader::Entry::ALL {
        assert_eq!(loaded.get(entry), compiled.get(entry), "{entry:?} is stale");
    }
}

#[test]
fn adapter_and_capabilities_are_reported() {
    let Some(r) = renderer() else { return };
    let name = r.adapter_name();
    eprintln!("{name}");
    assert!(name.starts_with("Direct3D 11 ("), "{name}");
    assert!(name.contains(", FL 11_"), "{name}");
    let caps = r.capabilities();
    assert!(caps.dual_source_blending);
    assert!(caps.offscreen_targets);
    assert!(caps.max_texture_size >= 16384);
    assert!(caps.max_msaa_samples >= 4, "every FL 11 device does 4x on RGBA8");
    assert_eq!(r.sample_count(), 4);
}

#[test]
fn a_clear_only_frame_shows_the_clear_colour() {
    let Some(mut r) = renderer() else { return };
    let out = render(&mut r, Color::rgb(0.2, 0.4, 0.6), |_| {});
    // The clear is given in sRGB, converted to linear, and encoded back by the
    // sRGB view: the bytes must round-trip.
    for (x, y) in [(0, 0), (W - 1, H - 1), (W / 2, H / 2)] {
        assert_near(out.at(x, y), [51, 102, 153, 255], 1);
    }
}

#[test]
fn a_solid_quad_is_written_srgb_encoded() {
    let Some(mut r) = renderer() else { return };
    let out = render(&mut r, Color::BLACK, |c| {
        c.fill_rect(rect(px(10.0), px(10.0), px(30.0), px(30.0)), Color::rgb(0.5, 0.5, 0.5));
        c.fill_rect(rect(px(60.0), px(10.0), px(30.0), px(30.0)), Color::rgba8(255, 0, 0, 255));
        // Half-transparent white over black blends in linear light: linear 0.5,
        // which the surface encodes as 188, not the 128 a gamma-space blend
        // would give.
        c.fill_rect(rect(px(10.0), px(50.0), px(30.0), px(30.0)), Color::rgba(1.0, 1.0, 1.0, 0.5));
    });
    assert_near(out.at(25, 25), [128, 128, 128, 255], 1);
    assert_near(out.at(75, 25), [255, 0, 0, 255], 0);
    assert_near(out.at(25, 65), [encode(0.5), encode(0.5), encode(0.5), 255], 1);
    assert_eq!(out.at(5, 5), BLACK);
    assert_eq!(out.at(45, 25), BLACK);
    // Edges are pixel-exact for an axis-aligned rect on whole pixels.
    assert_near(out.at(10, 10), [128, 128, 128, 255], 1);
    assert_eq!(out.at(9, 10), BLACK);
    assert_eq!(out.at(40, 25), BLACK);
}

#[test]
fn rounded_corners_are_antialiased() {
    let Some(mut r) = renderer() else { return };
    let out = render(&mut r, Color::BLACK, |c| {
        c.fill_rounded_rect(
            RoundedRect::uniform(rect(px(10.0), px(10.0), px(60.0), px(60.0)), px(20.0)),
            Color::WHITE,
        );
    });
    assert_eq!(out.at(10, 10), BLACK, "the corner outside the arc is empty");
    assert_eq!(out.at(40, 40), WHITE);
    assert_eq!(out.at(40, 10), WHITE, "the straight edge is unaffected");
    // Somewhere along the diagonal the arc crosses a pixel partially.
    let partial = (10..30).map(|i| out.at(i, i)[0]).filter(|v| *v > 8 && *v < 247).count();
    assert!(partial >= 1, "no antialiased pixel along the corner diagonal");
    // The four corners are symmetric, to within what screen-space derivatives
    // allow: `fwidth` is evaluated per 2x2 pixel quad, and a mirrored corner
    // falls at a different position within its quad, so the antialiasing
    // width differs by a hair. A few codes either way, never more.
    for i in 10..30 {
        let tl = out.at(i, i)[0];
        assert!(tl.abs_diff(out.at(79 - i, i)[0]) <= 6, "row {i}");
        assert!(tl.abs_diff(out.at(i, 79 - i)[0]) <= 6, "row {i}");
        assert!(tl.abs_diff(out.at(79 - i, 79 - i)[0]) <= 6, "row {i}");
    }
}

#[test]
fn a_border_is_drawn_inset_over_its_fill() {
    let Some(mut r) = renderer() else { return };
    let out = render(&mut r, Color::BLACK, |c| {
        c.quad(
            rect(px(10.0), px(10.0), px(40.0), px(40.0)),
            Corners::ZERO,
            Some(Brush::Solid(Color::rgba8(0, 0, 255, 255))),
            Color::WHITE,
            px(4.0),
        );
        c.stroke_rect(rect(px(60.0), px(10.0), px(40.0), px(40.0)), Color::WHITE, px(2.0));
    });
    assert_eq!(out.at(11, 30), WHITE);
    assert_eq!(out.at(13, 30), WHITE);
    assert_eq!(out.at(14, 30), [0, 0, 255, 255]);
    assert_eq!(out.at(30, 30), [0, 0, 255, 255]);
    assert_eq!(out.at(5, 30), BLACK);
    // A stroke alone leaves its interior empty.
    assert_eq!(out.at(61, 30), WHITE);
    assert_eq!(out.at(80, 30), BLACK);
}

#[test]
fn a_linear_gradient_reaches_both_endpoints() {
    let Some(mut r) = renderer() else { return };
    let red = Color::rgba8(255, 0, 0, 255);
    let blue = Color::rgba8(0, 0, 255, 255);
    let out = render(&mut r, Color::BLACK, |c| {
        c.fill_rect_with(
            rect(px(0.0), px(0.0), px(100.0), px(20.0)),
            Brush::Gradient(Gradient::horizontal(px(100.0), red, blue)),
        );
    });
    let first = out.at(0, 10);
    let last = out.at(99, 10);
    assert!(first[0] >= 250 && first[2] <= 20, "start {first:?}");
    assert!(last[2] >= 250 && last[0] <= 20, "end {last:?}");
    // Interpolated in linear light: the midpoint is 0.5 of each, encoded.
    assert_near(out.at(50, 10), [encode(0.5), 0, encode(0.5), 255], 3);
    // Monotonic along the ramp.
    let reds: Vec<u8> = (0..100).map(|x| out.at(x, 10)[0]).collect();
    assert!(reds.windows(2).all(|w| w[1] <= w[0]), "{reds:?}");
}

#[test]
fn a_shadow_spills_outside_its_shape() {
    let Some(mut r) = renderer() else { return };
    let out = render(&mut r, Color::WHITE, |c| {
        c.draw_shadow(
            RoundedRect::uniform(rect(px(40.0), px(30.0), px(40.0), px(40.0)), px(0.0)),
            &Shadow {
                offset: Size::new(px(0.0), px(0.0)),
                blur_radius: px(16.0),
                spread: px(0.0),
                color: Color::BLACK,
                inset: false,
            },
        );
    });
    let inside = out.at(60, 50)[0];
    let edge = out.at(40, 50)[0];
    let outside = out.at(34, 50)[0];
    let far = out.at(5, 5)[0];
    // Twenty pixels from every edge is 2.5 sigma, so a little light still
    // leaks in from all four sides: (1 - 0.988) linear, about 30 encoded.
    assert!(inside < 50, "deep inside the shadow is near-black, got {inside}");
    assert_eq!(far, 255, "far outside is untouched");
    // Half coverage of black over white is linear 0.5, encoded 188.
    assert!(edge.abs_diff(encode(0.5)) <= 6, "on the edge, got {edge}");
    assert!(outside > edge && outside < 255, "the falloff spills past the edge, got {outside}");
}

#[test]
fn glyphs_sample_their_atlas_page() {
    let Some(mut r) = renderer() else { return };
    // Left half fully covered, right half empty: grayscale coverage replicated
    // into rgb with alpha 255, as the text system uploads it.
    let mut page = vec![0u8; 16 * 16 * 4];
    for y in 0..16 {
        for x in 0..16 {
            let v = if x < 8 { 255 } else { 0 };
            page[(y * 16 + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let texture = r.upload_texture(None, 16, 16, &page).expect("upload");
    let run = || GlyphRun {
        font: FontId::new(0, 1),
        font_size: px(16.0),
        glyphs: vec![PositionedGlyph {
            glyph: GlyphId(1),
            position: Point::new(px(20.0), px(40.0)),
        }]
        .into(),
        coverage_contrast: 1.0,
    };

    // Unbound page: the glyph batch is skipped, not an error.
    let out_unbound =
        render_with(&mut r, Color::BLACK, false, &mut OneGlyph, &mut NoTextures, |c| {
            c.draw_glyph_run(run(), Color::WHITE);
        });
    assert_eq!(out_unbound.at(22, 30), BLACK);

    r.set_atlas_page(0, texture);
    let out = render_with(&mut r, Color::BLACK, false, &mut OneGlyph, &mut NoTextures, |c| {
        c.draw_glyph_run(run(), Color::WHITE);
    });
    // The quad spans x 20..36, y 24..40.
    assert_eq!(out.at(22, 30), WHITE);
    assert_eq!(out.at(27, 30), WHITE);
    assert_eq!(out.at(28, 30), BLACK);
    assert_eq!(out.at(22, 23), BLACK);
    assert_eq!(out.at(22, 24), WHITE);
    assert_eq!(out.at(22, 40), BLACK);

    // Coverage contrast bends partial coverage, not full coverage: a page of
    // 50 % coverage under k = 2.2 lands at 0.5^2.2 in linear light.
    let half = vec![128u8; 16 * 16 * 4];
    r.upload_texture(Some(texture), 16, 16, &half).expect("re-upload");
    let out = render_with(&mut r, Color::BLACK, false, &mut OneGlyph, &mut NoTextures, |c| {
        let mut run = run();
        run.coverage_contrast = 2.2;
        c.draw_glyph_run(run, Color::WHITE);
    });
    let expected = encode((128.0f32 / 255.0).powf(2.2));
    assert!(out.at(25, 30)[0].abs_diff(expected) <= 2, "got {:?}, want {expected}", out.at(25, 30));
}

#[test]
fn subpixel_glyphs_blend_each_channel_independently() {
    let Some(mut r) = renderer() else { return };
    // Red stripe only: rgb coverage (1, 0, 0), alpha = max = 1.
    let page: Vec<u8> = (0..16 * 16).flat_map(|_| [255u8, 0, 0, 255]).collect();
    let texture = r.upload_texture(None, 16, 16, &page).expect("upload");
    r.set_atlas_page(0, texture);
    let run = GlyphRun {
        font: FontId::new(0, 1),
        font_size: px(16.0),
        glyphs: vec![PositionedGlyph {
            glyph: GlyphId(1),
            position: Point::new(px(20.0), px(40.0)),
        }]
        .into(),
        coverage_contrast: 1.0,
    };

    // White on black: only the red channel receives coverage.
    let out = render_with(&mut r, Color::BLACK, true, &mut OneGlyph, &mut NoTextures, |c| {
        c.draw_glyph_run(run.clone(), Color::WHITE);
    });
    assert_eq!(out.at(25, 30), [255, 0, 0, 255]);

    // Black on white: the red channel is darkened and green and blue keep the
    // destination, which only a dual-source blend can express.
    let out = render_with(&mut r, Color::WHITE, true, &mut OneGlyph, &mut NoTextures, |c| {
        c.draw_glyph_run(run.clone(), Color::BLACK);
    });
    assert_eq!(out.at(25, 30), [0, 255, 255, 255]);
    assert_eq!(out.at(5, 5), WHITE);
}

#[test]
fn a_layer_composites_with_group_opacity() {
    let Some(mut r) = renderer() else { return };
    let out = render(&mut r, Color::BLACK, |c| {
        assert!(c.push_opacity_layer(rect(px(10.0), px(10.0), px(50.0), px(50.0)), 0.5));
        // Two overlapping children: group opacity applies once to the union,
        // so the overlap is no brighter than either child alone.
        c.fill_rect(rect(px(10.0), px(10.0), px(30.0), px(30.0)), Color::WHITE);
        c.fill_rect(rect(px(30.0), px(30.0), px(30.0), px(30.0)), Color::WHITE);
        c.end_layer();
        c.fill_rect(rect(px(100.0), px(10.0), px(10.0), px(10.0)), Color::WHITE);
    });
    let half = [encode(0.5), encode(0.5), encode(0.5), 255];
    assert_near(out.at(15, 15), half, 1);
    assert_near(out.at(35, 35), half, 1);
    assert_near(out.at(55, 55), half, 1);
    assert_eq!(out.at(15, 55), BLACK);
    assert_eq!(out.at(105, 15), WHITE);
    assert_eq!(r.last_stats().layers, 1);
}

#[test]
fn a_layer_with_msaa_off_composites_the_same() {
    let Some(mut r) = renderer_with(1) else { return };
    assert_eq!(r.sample_count(), 1);
    let out = render(&mut r, Color::BLACK, |c| {
        c.fill_rect(rect(px(0.0), px(0.0), px(5.0), px(5.0)), Color::WHITE);
        c.push_opacity_layer(rect(px(10.0), px(10.0), px(50.0), px(50.0)), 0.5);
        c.fill_rect(rect(px(10.0), px(10.0), px(50.0), px(50.0)), Color::WHITE);
        c.end_layer();
    });
    assert_near(out.at(30, 30), [encode(0.5), encode(0.5), encode(0.5), 255], 1);
    assert_eq!(out.at(2, 2), WHITE);
}

#[test]
fn a_blur_filter_softens_the_layer() {
    let Some(mut r) = renderer() else { return };
    let out = render(&mut r, Color::BLACK, |c| {
        c.fill_rect(rect(px(0.0), px(0.0), px(4.0), px(4.0)), Color::WHITE);
        assert!(c.push_layer(
            rect(px(0.0), px(0.0), px(W as f32), px(H as f32)),
            1.0,
            BlendMode::Normal,
            Some(Filter::Blur { sigma: px(4.0) }),
        ));
        c.fill_rect(rect(px(40.0), px(30.0), px(40.0), px(40.0)), Color::WHITE);
        c.end_layer();
    });
    let centre = out.at(60, 50)[0];
    let edge = out.at(40, 50)[0];
    let outside = out.at(36, 50)[0];
    assert!(centre >= 250, "the middle stays solid, got {centre}");
    assert!(edge.abs_diff(encode(0.5)) <= 8, "the edge is half covered, got {edge}");
    assert!(outside > 0 && outside < edge, "blur spills outward, got {outside}");
    assert_eq!(out.at(20, 50), BLACK, "four sigma out nothing remains");
    assert_eq!(out.at(1, 1), WHITE, "content outside the layer is untouched");
}

#[test]
fn a_backdrop_blur_blurs_what_is_behind_the_layer() {
    let Some(mut r) = renderer() else { return };
    let out = render(&mut r, Color::BLACK, |c| {
        // A hard vertical edge at x = 64, then a frosted panel across it with
        // no content of its own.
        c.fill_rect(rect(px(64.0), px(0.0), px(64.0), px(H as f32)), Color::WHITE);
        assert!(c.push_layer(
            rect(px(32.0), px(16.0), px(64.0), px(64.0)),
            1.0,
            BlendMode::Normal,
            Some(Filter::BackdropBlur { sigma: px(4.0) }),
        ));
        c.end_layer();
    });
    // Inside the panel the edge is soft…
    let left = out.at(61, 48)[0];
    let right = out.at(66, 48)[0];
    assert!(left > 20 && left < 235, "backdrop left of the edge is blurred, got {left}");
    assert!(right > 20 && right < 235, "backdrop right of the edge is blurred, got {right}");
    // …outside it the edge stays hard.
    assert_eq!(out.at(63, 8), BLACK);
    assert_eq!(out.at(64, 8), WHITE);
}

#[test]
fn a_filled_path_is_tessellated_and_multisampled() {
    let Some(mut r) = renderer() else { return };
    let triangle = || {
        let mut b = Path::builder();
        b.move_to(Point::new(px(10.0), px(10.0)));
        b.line_to(Point::new(px(90.0), px(10.0)));
        b.line_to(Point::new(px(10.0), px(80.0)));
        b.close();
        b.build()
    };
    let out =
        render(&mut r, Color::BLACK, |c| c.fill_path(triangle(), Color::rgba8(0, 255, 0, 255)));
    assert_eq!(out.at(20, 20), [0, 255, 0, 255]);
    assert_eq!(out.at(80, 70), BLACK);
    assert!(r.last_stats().triangles >= 1);
    // The hypotenuse crosses pixels partially; with 4x MSAA some land between.
    let partial = (12..88)
        .filter_map(|x| {
            let y = 10.0 + (90.0 - x as f32) * 70.0 / 80.0;
            let g = out.at(x, y.floor() as u32)[1];
            (g > 10 && g < 245).then_some(g)
        })
        .count();
    assert!(partial > 5, "the diagonal edge should be antialiased, {partial} partial pixels");
}

#[test]
fn an_image_quad_samples_its_texture() {
    let Some(mut r) = renderer() else { return };
    let texels = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
    let texture = r.upload_texture(None, 2, 2, &texels).expect("upload");
    let image = ImageId::new(0, 1);
    let out =
        render_with(&mut r, Color::BLACK, false, &mut NoGlyphs, &mut OneTexture(texture), |c| {
            c.draw_image(image, rect(px(0.0), px(0.0), px(40.0), px(40.0)), ImageFit::Fill);
        });
    assert_eq!(out.at(2, 2), [255, 0, 0, 255]);
    assert_eq!(out.at(37, 2), [0, 255, 0, 255]);
    assert_eq!(out.at(2, 37), [0, 0, 255, 255]);
    assert_eq!(out.at(37, 37), WHITE);
    assert_eq!(out.at(50, 20), BLACK);

    // A region upload replaces just that texel.
    r.upload_texture_region(texture, 0, 0, 1, 1, &[0, 0, 0, 255]).expect("region");
    let out =
        render_with(&mut r, Color::WHITE, false, &mut NoGlyphs, &mut OneTexture(texture), |c| {
            c.draw_image(image, rect(px(0.0), px(0.0), px(40.0), px(40.0)), ImageFit::Fill);
        });
    assert_eq!(out.at(2, 2), BLACK);
    assert!(r.memory_usage() > 0);
    r.destroy_texture(texture);
}

#[test]
fn resizing_to_zero_is_a_no_op() {
    let Some(mut r) = renderer() else { return };
    r.configure_surface(config(0, 0, 4)).expect("zero size is accepted");
    let out = render(&mut r, Color::WHITE, |c| {
        c.fill_rect(rect(px(0.0), px(0.0), px(10.0), px(10.0)), Color::BLACK)
    });
    assert_eq!(out.at(5, 5), BLACK);
    assert_eq!(out.at(50, 50), WHITE);

    // A real resize takes effect.
    r.configure_surface(config(64, 32, 4)).expect("resize");
    let mut frame = r.begin_frame().expect("begin");
    assert_eq!(frame.size.width.as_u32(), 64);
    let scene = Scene::new(size(px(64.0), px(32.0)), ScaleFactor::IDENTITY);
    let compiled = BatchCompiler::new().compile(&scene, &mut NoGlyphs, &mut NoTextures).clone();
    r.render(&mut frame, &compiled, Color::BLACK).expect("render");
    r.end_frame(frame).expect("end");
    assert_eq!(r.read_pixels().expect("read").len(), 64 * 32 * 4);
}

#[test]
fn a_layer_that_opens_the_frame_survives_later_surface_drawing() {
    // The shape that used to erase a layer: the compiler now clears the
    // surface with an empty pass before the layer composites into it, and the
    // surface's own later pass must not clear again.
    let Some(mut r) = renderer() else { return };
    let out = render(&mut r, Color::BLACK, |c| {
        assert!(c.push_opacity_layer(rect(px(10.0), px(10.0), px(30.0), px(30.0)), 0.5));
        c.fill_rect(rect(px(10.0), px(10.0), px(30.0), px(30.0)), Color::WHITE);
        c.end_layer();
        c.fill_rect(rect(px(80.0), px(10.0), px(20.0), px(20.0)), Color::WHITE);
    });
    assert_near(out.at(25, 25), [encode(0.5), encode(0.5), encode(0.5), 255], 1);
    assert_eq!(out.at(90, 20), WHITE);
    assert_eq!(out.at(60, 60), BLACK, "the surface was cleared");
}

#[test]
fn an_opaque_image_lands_byte_exact() {
    // Images arrive sRGB-encoded. Sampled through the sRGB view they decode to
    // linear and the target re-encodes them: mid-grey stays 128. Sampled as
    // UNORM, as the wgpu backend did, they would come out at 188.
    let Some(mut r) = renderer() else { return };
    let grey: Vec<u8> = (0..32 * 32).flat_map(|_| [128u8, 128, 128, 255]).collect();
    let texture = r.upload_texture(None, 32, 32, &grey).expect("upload");
    let image = ImageId::new(0, 1);
    let out =
        render_with(&mut r, Color::BLACK, false, &mut NoGlyphs, &mut OneTexture(texture), |c| {
            c.draw_image(image, rect(px(10.0), px(10.0), px(32.0), px(32.0)), ImageFit::Fill);
        });
    assert_near(out.at(20, 20), [128, 128, 128, 255], 1);
    assert_near(out.at(41, 25), [128, 128, 128, 255], 1);

    // The same bytes bound as an atlas page are coverage, read as-is: 50 %
    // coverage of white is linear 0.5, encoded 188.
    r.set_atlas_page(0, texture);
    let out = render_with(&mut r, Color::BLACK, false, &mut OneGlyph, &mut NoTextures, |c| {
        c.draw_glyph_run(
            GlyphRun {
                font: FontId::new(0, 1),
                font_size: px(16.0),
                glyphs: vec![PositionedGlyph {
                    glyph: GlyphId(1),
                    position: Point::new(px(20.0), px(40.0)),
                }]
                .into(),
                coverage_contrast: 1.0,
            },
            Color::WHITE,
        );
    });
    let v = encode(128.0 / 255.0);
    assert_near(out.at(25, 30), [v, v, v, 255], 1);
}

#[test]
fn a_rounded_clip_is_evaluated_in_scene_space_under_a_transform() {
    let Some(mut r) = renderer() else { return };
    let out = render(&mut r, Color::BLACK, |c| {
        c.translate(Size::new(px(50.0), px(20.0)));
        c.clip_rounded_rect(RoundedRect::uniform(
            rect(px(0.0), px(0.0), px(40.0), px(40.0)),
            px(20.0),
        ));
        c.fill_rect(rect(px(0.0), px(0.0), px(40.0), px(40.0)), Color::WHITE);
    });
    // The clip is a circle centred at scene (70, 40).
    assert_eq!(out.at(70, 40), WHITE, "the centre is inside the clip");
    assert_eq!(out.at(70, 21), WHITE, "the top of the circle is inside");
    assert_eq!(out.at(51, 21), BLACK, "the corner of the square is clipped away");
    assert_eq!(out.at(88, 58), BLACK, "so is the opposite corner");
}
