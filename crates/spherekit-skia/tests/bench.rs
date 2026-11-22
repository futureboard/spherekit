//! A frame-time benchmark, ignored by default.
//!
//! ```text
//! cargo test -p spherekit-skia --release --test bench -- --ignored --nocapture
//! ```
//!
//! A 1920×1080 frame of a dense UI: 2000 quads (plain, rounded, bordered, a
//! few shadows) and 3000 glyphs in 100 runs.

mod common;

use common::{config, font};
use spherekit_core::{
    Color, Corners, GlyphId, Point, Px, RoundedRect, ScaleFactor, Shadow, px, rect, size,
};
use spherekit_render::{
    BatchCompiler, Canvas, GlyphPlacement, GlyphProvider, GlyphRequest, GlyphRun, PositionedGlyph,
    RendererBackend, Scene,
};
use spherekit_skia::SkiaRenderer;
use std::time::Instant;

/// 16 glyph cells of 10×14 texels on a 64×64 page.
struct Glyphs;

impl GlyphProvider for Glyphs {
    fn place_glyph(&mut self, r: GlyphRequest) -> Option<GlyphPlacement> {
        let cell = (r.glyph.0 % 16) as f32;
        let (cx, cy) = (cell % 4.0 * 16.0, (cell / 4.0).floor() * 16.0);
        Some(GlyphPlacement {
            page: 0,
            uv: [cx / 64.0, cy / 64.0, (cx + 10.0) / 64.0, (cy + 14.0) / 64.0],
            bounds_em: [0.0, -14.0 / 16.0, 10.0 / 16.0, 14.0 / 16.0],
            is_subpixel: false,
            texel_size: [10, 14],
        })
    }
}

fn atlas() -> Vec<u8> {
    let mut page = vec![0u8; 64 * 64 * 4];
    for y in 0..64usize {
        for x in 0..64usize {
            let (lx, ly) = (x % 16, y % 16);
            // A ring-ish blob with soft edges, so blending does real work.
            let d = ((lx as f32 - 5.0).powi(2) + (ly as f32 - 7.0).powi(2)).sqrt();
            let cov = if lx < 10 && ly < 14 {
                ((4.5 - (d - 3.0).abs()) * 120.0).clamp(0.0, 255.0)
            } else {
                0.0
            };
            let i = (y * 64 + x) * 4;
            let c = cov as u8;
            page[i..i + 4].copy_from_slice(&[c, c, c, 255]);
        }
    }
    page
}

fn build_scene(with_quads: bool, with_glyphs: bool) -> Scene {
    let mut scene = Scene::new(size(px(1920.0), px(1080.0)), ScaleFactor::IDENTITY);
    let mut c = Canvas::new(&mut scene);
    let mut n = 0u32;
    'outer: for row in 0..if with_quads { 50 } else { 0 } {
        for col in 0..40 {
            let r =
                rect(px(col as f32 * 48.0 + 2.0), px(row as f32 * 21.6 + 1.0), px(44.0), px(19.0));
            let hue = Color::rgba8((col * 6) as u8, (row * 5) as u8, 180, 255);
            match n % 4 {
                0 => c.fill_rect(r, hue),
                1 => c.fill_rounded_rect(RoundedRect::uniform(r, px(5.0)), hue),
                2 => c.quad(
                    r,
                    Corners::all(px(4.0)),
                    Some(hue.into()),
                    Color::WHITE.with_alpha(0.6),
                    px(1.0),
                ),
                _ => {
                    if n % 40 == 3 {
                        c.draw_shadow(
                            RoundedRect::uniform(r, px(4.0)),
                            &Shadow {
                                offset: size(px(0.0), px(2.0)),
                                blur_radius: px(8.0),
                                spread: Px::ZERO,
                                color: Color::BLACK.with_alpha(0.4),
                                inset: false,
                            },
                        );
                    } else {
                        c.fill_rect(r, hue.with_alpha(0.7));
                    }
                }
            }
            n += 1;
            if n == 2000 {
                break 'outer;
            }
        }
    }
    for run in 0..if with_glyphs { 100 } else { 0 } {
        let x0 = (run % 5) as f32 * 380.0 + 10.0;
        let y0 = (run / 5) as f32 * 52.0 + 30.0;
        let glyphs = (0..30)
            .map(|i| PositionedGlyph {
                glyph: GlyphId((run * 7 + i) as u16),
                position: Point::new(px(x0 + i as f32 * 11.0), px(y0)),
            })
            .collect::<Vec<_>>();
        c.draw_glyph_run(
            GlyphRun {
                font: font(),
                font_size: px(16.0),
                glyphs: glyphs.into(),
                coverage_contrast: 1.0,
            },
            Color::WHITE,
        );
    }
    drop(c);
    scene
}

fn time_frames(r: &mut SkiaRenderer, compiled: &spherekit_render::CompiledFrame) -> (f64, f64) {
    let mut times = Vec::new();
    for i in 0..60 {
        let start = Instant::now();
        let mut f = r.begin_frame().unwrap();
        r.render(&mut f, compiled, Color::hex(0x202020)).unwrap();
        r.end_frame(f).unwrap();
        if i >= 10 {
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (times[times.len() / 2], times[0])
}

#[test]
#[ignore = "benchmark; run with --release -- --ignored --nocapture"]
fn frame_of_2000_quads_and_3000_glyphs_at_1080p() {
    let mut r = SkiaRenderer::new_offscreen(config(1920, 1080, 1.0)).unwrap();
    let page = r.upload_texture(None, 64, 64, &atlas()).unwrap();
    r.set_atlas_page(0, page);

    let mut compiler = BatchCompiler::new();
    for (label, quads, glyphs) in [
        ("clear only", false, false),
        ("quads only", true, false),
        ("glyphs only", false, true),
        ("full frame", true, true),
    ] {
        let scene = build_scene(quads, glyphs);
        let compiled = compiler.compile(&scene, &mut Glyphs, &mut common::OneTexture(None)).clone();
        if quads {
            assert_eq!(compiled.quads.len(), 2000);
        }
        if glyphs {
            assert_eq!(compiled.glyphs.len(), 3000);
        }
        let (median, min) = time_frames(&mut r, &compiled);
        println!(
            "1920x1080 {label:>12}: {:>4} quads + {:>4} glyphs: median {median:.2} ms, min {min:.2} ms",
            compiled.quads.len(),
            compiled.glyphs.len(),
        );
    }
}
