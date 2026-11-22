//! # spherekit-d3d11
//!
//! The Direct3D 11 rendering backend for SphereKit, presented through
//! DirectComposition.
//!
//! This is the GPU backend on Windows. It implements
//! [`spherekit_render::RendererBackend`], consuming the pure-data
//! [`spherekit_render::CompiledFrame`] the batch compiler produces, and renders
//! the same pixels the wgpu backend it replaces did: the HLSL shaders are
//! expression-for-expression ports of the WGSL ones.
//!
//! ## Why Direct3D 11
//!
//! A 2D UI needs instancing, a handful of blend states and offscreen targets.
//! Direct3D 11 has all of that on every Windows GPU of the last fifteen years,
//! ships its shader compiler with the OS, and has the best-supported path into
//! DirectComposition, which is what makes transparent windows composite
//! properly. Anything without a usable device falls back to `spherekit-skia`.
//!
//! ## Colour
//!
//! Everything SphereKit hands the GPU is linear and premultiplied. Every
//! render target is viewed through an sRGB format, so blending happens in
//! linear light and the hardware encodes on write: the swap chain's
//! `B8G8R8A8_UNORM` buffers through `B8G8R8A8_UNORM_SRGB` views, offscreen
//! layers as `R8G8B8A8_UNORM_SRGB`. Textures are stored typeless with two
//! views: images arrive sRGB-encoded and are decoded on sample, while glyph
//! atlas pages hold linear coverage and are read as-is through a `UNORM` view.
//! (The wgpu backend read images as `UNORM` too, which encoded them twice and
//! showed them too bright; that is deliberately not ported.)
//!
//! ## Shaders
//!
//! HLSL sources live in `shaders/` and are embedded in the binary. When the
//! build host is Windows they are also compiled to DXBC by `build.rs`, because
//! compiling at runtime costs about 180 ms; see [`shader`].
//!
//! ## Other platforms
//!
//! The crate compiles everywhere so a workspace can depend on it
//! unconditionally. Off Windows, [`D3D11Renderer`] is an uninhabited type whose
//! constructors return [`spherekit_core::InitError::NoAdapter`].

#![deny(missing_docs)]

#[cfg(windows)]
mod buffer;
#[cfg(windows)]
mod device;
#[cfg(windows)]
mod pipeline;
#[cfg(windows)]
mod renderer;
#[cfg(windows)]
pub mod shader;
#[cfg(windows)]
mod surface;
#[cfg(windows)]
mod target;
#[cfg(windows)]
mod texture;

#[cfg(windows)]
pub use renderer::D3D11Renderer;

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
pub use unsupported::D3D11Renderer;
