//! The windowed path: a DirectComposition swap chain on a real HWND.
//!
//! The pixel-level tests in `render.rs` run offscreen; this one checks that the
//! presentation machinery around them works — swap chain creation for
//! composition, the frame-latency wait, present, resize and a transparent
//! swap chain — using a bare Win32 window so no windowing crate is needed.
//!
//! Skips where there is no hardware device: the windowed constructor never
//! falls back to WARP.

#![cfg(windows)]

use core::num::NonZeroIsize;
use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use spherekit_core::{Color, DevicePx, InitError, ScaleFactor, Size, px, rect, size};
use spherekit_d3d11::D3D11Renderer;
use spherekit_render::{
    BatchCompiler, Canvas, GlyphPlacement, GlyphProvider, GlyphRequest, RendererBackend, Scene,
    SurfaceConfig, TextureProvider,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

struct Window(HWND);

impl Window {
    fn new(width: i32, height: i32) -> Self {
        // SAFETY: "STATIC" is a system window class; no parent, menu or param.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                w!("STATIC"),
                w!("spherekit-d3d11 test"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                width,
                height,
                None,
                None,
                None,
                None,
            )
        }
        .expect("CreateWindowExW");
        Self(hwnd)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window was created by this thread and is destroyed once.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

impl HasWindowHandle for Window {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = Win32WindowHandle::new(NonZeroIsize::new(self.0.0 as isize).expect("hwnd"));
        // SAFETY: the HWND lives as long as `self`.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(raw)) })
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
    fn texture_for(&mut self, _: spherekit_core::ImageId) -> Option<spherekit_core::TextureId> {
        None
    }
}

fn config(w: u32, h: u32, transparent: bool) -> SurfaceConfig {
    SurfaceConfig {
        size: Size::new(DevicePx(w as i32), DevicePx(h as i32)),
        transparent,
        ..SurfaceConfig::default()
    }
}

/// Renders one frame with a white square at the origin, reading it back
/// before presenting.
fn frame(r: &mut D3D11Renderer, w: u32, h: u32, clear: Color) -> Vec<u8> {
    let mut scene = Scene::new(size(px(w as f32), px(h as f32)), ScaleFactor::IDENTITY);
    Canvas::new(&mut scene).fill_rect(rect(px(0.0), px(0.0), px(8.0), px(8.0)), Color::WHITE);
    let mut compiler = BatchCompiler::new();
    let compiled = compiler.compile(&scene, &mut NoGlyphs, &mut NoTextures);
    let mut handle = r.begin_frame().expect("begin_frame");
    assert_eq!(handle.size.width.as_u32(), w);
    r.render(&mut handle, compiled, clear).expect("render");
    let pixels = r.read_pixels().expect("a window can be read before present");
    r.end_frame(handle).expect("present");
    assert!(r.read_pixels().is_err(), "after present the back buffer is the compositor's");
    pixels
}

fn renderer_or_skip(window: &Window, config: SurfaceConfig) -> Option<D3D11Renderer> {
    match D3D11Renderer::new(window, config) {
        Ok(r) => Some(r),
        Err(InitError::NoAdapter(e)) => {
            eprintln!("skipping: no hardware Direct3D 11 device ({e})");
            None
        }
        Err(e) => panic!("windowed renderer failed: {e}"),
    }
}

#[test]
fn a_window_presents_resizes_and_ignores_zero_size() {
    let window = Window::new(160, 120);
    let Some(mut r) = renderer_or_skip(&window, config(160, 120, false)) else { return };

    for _ in 0..3 {
        let px = frame(&mut r, 160, 120, Color::BLACK);
        assert_eq!(&px[..4], &[255, 255, 255, 255]);
        let far = ((100 * 160 + 100) * 4) as usize;
        assert_eq!(&px[far..far + 4], &[0, 0, 0, 255]);
    }

    // Minimised: a no-op, and the next frame is still the old size.
    r.configure_surface(config(0, 0, false)).expect("zero size");
    frame(&mut r, 160, 120, Color::BLACK);

    r.configure_surface(config(200, 90, false)).expect("resize");
    let px = frame(&mut r, 200, 90, Color::BLACK);
    assert_eq!(px.len(), 200 * 90 * 4);
}

#[test]
fn a_transparent_window_keeps_alpha() {
    let window = Window::new(64, 64);
    let Some(mut r) = renderer_or_skip(&window, config(64, 64, true)) else { return };
    let px = frame(&mut r, 64, 64, Color::TRANSPARENT);
    assert_eq!(&px[..4], &[255, 255, 255, 255]);
    let far = ((40 * 64 + 40) * 4) as usize;
    assert_eq!(&px[far..far + 4], &[0, 0, 0, 0], "a transparent clear stays transparent");
}

#[test]
fn a_non_win32_handle_is_rejected() {
    struct Web;
    impl HasWindowHandle for Web {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let raw = RawWindowHandle::Web(raw_window_handle::WebWindowHandle::new(1));
            // SAFETY: never dereferenced; the renderer rejects it by kind.
            Ok(unsafe { WindowHandle::borrow_raw(raw) })
        }
    }
    assert!(matches!(D3D11Renderer::new(&Web, config(8, 8, false)), Err(InitError::Surface(_))));
}
