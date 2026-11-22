//! Shared harness: build a real `Scene`, compile it with the real
//! `BatchCompiler`, render it offscreen and read the pixels back.

#![allow(dead_code)]

use spherekit_core::{Color, DevicePx, FontId, ImageId, ScaleFactor, Size, TextureId, px, size};
use spherekit_render::{
    BatchCompiler, Canvas, GlyphPlacement, GlyphProvider, GlyphRequest, RendererBackend, Scene,
    SurfaceConfig, TextureProvider,
};
use spherekit_skia::SkiaRenderer;

/// A glyph provider that hands out one fixed placement for every glyph.
pub struct FixedGlyph(pub Option<GlyphPlacement>);

impl GlyphProvider for FixedGlyph {
    fn place_glyph(&mut self, _: GlyphRequest) -> Option<GlyphPlacement> {
        self.0
    }
}

/// A texture provider that resolves every image to one texture.
pub struct OneTexture(pub Option<TextureId>);

impl TextureProvider for OneTexture {
    fn texture_for(&mut self, _: ImageId) -> Option<TextureId> {
        self.0
    }
}

pub fn config(w: i32, h: i32, scale: f32) -> SurfaceConfig {
    SurfaceConfig {
        size: Size::new(DevicePx(w), DevicePx(h)),
        scale_factor: ScaleFactor::new(scale),
        ..SurfaceConfig::default()
    }
}

/// A device-size offscreen renderer.
pub struct Harness {
    pub renderer: SkiaRenderer,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub glyphs: FixedGlyph,
    pub textures: OneTexture,
    compiler: BatchCompiler,
}

impl Harness {
    pub fn new(w: u32, h: u32) -> Self {
        Self::with_scale(w, h, 1.0)
    }

    /// `w × h` are *device* pixels.
    pub fn with_scale(w: u32, h: u32, scale: f32) -> Self {
        let renderer = SkiaRenderer::new_offscreen(config(w as i32, h as i32, scale)).unwrap();
        Self {
            renderer,
            width: w,
            height: h,
            scale,
            glyphs: FixedGlyph(None),
            textures: OneTexture(None),
            compiler: BatchCompiler::new(),
        }
    }

    pub fn scene(&self) -> Scene {
        Scene::new(
            size(px(self.width as f32 / self.scale), px(self.height as f32 / self.scale)),
            ScaleFactor::new(self.scale),
        )
    }

    /// Records with `f`, compiles, renders over `clear` and reads back.
    pub fn draw(&mut self, clear: Color, f: impl FnOnce(&mut Canvas<'_>)) -> Image {
        let mut scene = self.scene();
        {
            let mut canvas = Canvas::new(&mut scene);
            f(&mut canvas);
        }
        self.render(&scene, clear)
    }

    pub fn render(&mut self, scene: &Scene, clear: Color) -> Image {
        let compiled = self.compiler.compile(scene, &mut self.glyphs, &mut self.textures);
        let mut frame = self.renderer.begin_frame().unwrap();
        self.renderer.render(&mut frame, compiled, clear).unwrap();
        self.renderer.end_frame(frame).unwrap();
        Image { data: self.renderer.read_pixels().unwrap(), width: self.width }
    }
}

/// Read-back pixels.
pub struct Image {
    pub data: Vec<u8>,
    pub width: u32,
}

impl Image {
    pub fn at(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        self.data[i..i + 4].try_into().unwrap()
    }
}

/// Asserts a pixel matches `expected` RGB within `tol` per channel.
#[track_caller]
pub fn assert_rgb(img: &Image, x: u32, y: u32, expected: [u8; 3], tol: u8) {
    let p = img.at(x, y);
    let ok = (0..3).all(|i| p[i].abs_diff(expected[i]) <= tol);
    assert!(ok, "pixel ({x}, {y}) = {p:?}, expected ~{expected:?} (±{tol})");
}

pub fn font() -> FontId {
    FontId::new(0, 1)
}
