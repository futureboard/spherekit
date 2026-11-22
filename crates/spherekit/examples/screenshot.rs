//! Renders a small interface offscreen and writes it to a PNG.
//!
//! No window: this drives [`SphereKitSurface::new_offscreen`] through the whole
//! stack — UI tree, layout, shaping, glyph rasterisation (DirectWrite on
//! Windows), the atlas, and whichever renderer is chosen — and reads the frame
//! back. It is the quickest way to see what a backend actually draws, and to
//! compare the two side by side.
//!
//! ```bash
//! cargo run -p spherekit --example screenshot --release -- out.png
//! SPHEREKIT_BACKEND=skia  cargo run -p spherekit --example screenshot --release -- skia.png
//! SPHEREKIT_BACKEND=d3d11 cargo run -p spherekit --example screenshot --release -- d3d11.png
//! SPHEREKIT_SCALE=2 cargo run -p spherekit --example screenshot --release -- hidpi.png
//! ```

use spherekit::core::{Color, DevicePx, Gradient, ScaleFactor, Shadow, Size, px, relative};
use spherekit::ui::prelude::*;
use spherekit::ui::{AnyElement, Theme};
use spherekit::{SphereKitSurface, SurfaceOptions};

fn ui(theme: &Theme) -> AnyElement {
    let c = theme.colors;
    let card = |title: &str| {
        div()
            .flex_col()
            .gap(px(6.0))
            .p(px(14.0))
            .w(px(220.0))
            .bg(c.surface)
            .rounded(px(10.0))
            .border(px(1.0), c.border)
            .shadow(Shadow {
                offset: Size::new(px(0.0), px(6.0)),
                blur_radius: px(18.0),
                color: Color::BLACK.with_alpha(0.35),
                ..Shadow::default()
            })
            .child(label(title.to_string()).text_size(px(15.0)))
    };

    div()
        .flex_col()
        .gap(px(18.0))
        .p(px(24.0))
        .w(relative(1.0))
        .h(relative(1.0))
        .bg(c.background)
        .child(label("SphereKit offscreen render").text_size(px(28.0)))
        .child(
            label("The quick brown fox jumps over the lazy dog — 0123456789")
                .text_size(px(13.0))
                .text_color(c.text_muted),
        )
        .child(label("ไทย  日本語  中文  한국어  العربية  Ελληνικά").text_size(px(16.0)))
        .child(
            div()
                .flex_row()
                .gap(px(18.0))
                .child(card("Gradient").child(
                    div().h(px(48.0)).w(relative(1.0)).rounded(px(6.0)).bg(Gradient::horizontal(
                        px(190.0),
                        Color::hex(0x1E88E5),
                        Color::hex(0xE91E63),
                    )),
                ))
                .child(
                    card("Translucent layer").child(
                        div()
                            .h(px(48.0))
                            .w(relative(1.0))
                            .rounded(px(6.0))
                            .bg(Color::hex(0x43A047))
                            .opacity(0.5),
                    ),
                )
                .child(
                    card("Controls")
                        .child(button("Button"))
                        .child(label("Outlined label").outline(px(1.0), Color::hex(0xE91E63))),
                ),
        )
        .into_element()
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "spherekit.png".into());
    let scale: f32 =
        std::env::var("SPHEREKIT_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let scale = ScaleFactor::new(scale);
    let size =
        Size::new(DevicePx((760.0 * scale.get()) as i32), DevicePx((340.0 * scale.get()) as i32));

    let mut surface = match SphereKitSurface::new_offscreen(size, scale, SurfaceOptions::default())
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("could not create a surface: {e}");
            std::process::exit(1);
        }
    };
    // `SPHEREKIT_RASTER=outline` swaps DirectWrite for the portable rasteriser,
    // to tell a rasteriser difference from a renderer difference.
    if std::env::var("SPHEREKIT_RASTER").is_ok_and(|v| v.eq_ignore_ascii_case("outline")) {
        surface.text_mut().set_rasterizer(spherekit::text::GlyphRasterizer::Outline);
    }
    let theme = Theme::dark();
    surface.set_theme(theme.clone());
    println!("backend: {:?} — {}", surface.backend(), surface.adapter_name());

    // Two frames: the first rasterises every glyph, the second is steady state.
    for _ in 0..2 {
        if let Err(e) = surface.render(ui(&theme), theme.colors.background) {
            eprintln!("render failed: {e}");
            std::process::exit(1);
        }
    }
    let stats = surface.stats();
    println!(
        "frame: {} draw calls, {} quads, {} glyphs, cpu {:.2} ms",
        stats.frame.draw_calls, stats.frame.quads, stats.frame.glyphs, stats.cpu_ms
    );

    let mut pixels = match surface.read_pixels() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("read back failed: {e}");
            std::process::exit(1);
        }
    };
    // Premultiplied to straight alpha, which is what PNG stores. The frame is
    // opaque here, so this is a no-op in practice; it keeps a transparent
    // surface from coming out dark-fringed.
    for px in pixels.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    let (w, h) = (size.width.get() as u32, size.height.get() as u32);
    if let Err(e) = image::save_buffer(&out, &pixels, w, h, image::ExtendedColorType::Rgba8) {
        eprintln!("could not write {out}: {e}");
        std::process::exit(1);
    }
    println!("wrote {out} ({w}x{h})");
}
