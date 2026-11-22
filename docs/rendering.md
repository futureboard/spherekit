# Rendering

How a canvas call becomes pixels, and why each step exists.

## The pipeline

```text
Canvas calls
   ↓  clip and transform stacks resolved to indices AT RECORD TIME
Scene                    flat command list + side tables
   ↓  cull against the device-space scissor
   ↓  batch: run-length merge of adjacent compatible commands
   ↓  tessellate paths (lyon), bake mesh transforms
CompiledFrame            instance buffers, transform/clip/gradient tables,
                         batches, passes, targets — pure data, no GPU
   ↓
RendererBackend          spherekit-d3d11          spherekit-skia
   ↓                     Direct3D 11, HLSL        Skia raster
                         DirectComposition        softbuffer
                         (Windows)                (fallback; Linux, macOS)
```

The split between `Scene`, `CompiledFrame` and the backend is the load-bearing decision in this
crate. `CompiledFrame` contains no GPU handles and is produced by code that never touches a device,
which is what makes the batch compiler unit-testable and benchmarkable on a machine with no GPU at
all. Roughly half of `spherekit-render`'s tests exercise it. It is also what makes two very different
backends cheap to keep: both consume the same `CompiledFrame`, and neither knows anything about
`Scene`, `Canvas` or the UI.

## Why the canvas does not draw

`canvas.fill_rect(...)` appends a record. It never issues a draw call. Two properties follow:

**Order is preserved and stack work is not repeated.** The clip and transform stacks are resolved
*while recording*, so each `DrawCommand` carries a resolved index into `Scene::transforms` and
`Scene::clips`. Nothing downstream re-walks a stack. A clip is stored in scene-absolute
coordinates, so the backend gets a scissor rectangle and has no transform left to apply.

**Commands stay small.** A `DrawCommand` is a fixed-size tagged record — asserted at 128 bytes or
under by a test — that indexes into side tables. Paths, gradients, glyph runs and meshes live in
their own arenas. Ten thousand rectangles are ten thousand compact records and zero per-rectangle
heap allocations.

`Scene::reset` clears every vector while keeping its allocation, so steady-state frame building
allocates nothing. A fresh `Scene` per frame would re-grow nine vectors every time.

## Batching: run-length, never sorted

UI is painted back to front and overlapping translucent content depends on that order. So batching
merges only *adjacent* compatible commands. A global sort would batch better and draw wrong.

```rust
canvas.fill_rect(a, RED);       // ─┐
canvas.draw_image(img, b);      //  │ three batches, not two —
canvas.fill_rect(c, BLUE);      // ─┘ merging the rects hides the image
```

A batch breaks when the pipeline kind, the scissor rectangle, the bound texture or the render
target changes. Each of those is a real GPU state change, and `FrameStats::pipeline_switches`
reports them so a regression is visible rather than inferred.

Within a compatible run, batching is total: five hundred rectangles with the same clip become one
instanced draw call. A test asserts exactly that.

## Culling happens before an instance exists

`BatchCompiler::visible` computes the device-space scissor and rejects the command *before* writing
an instance, not after. Rejected commands increment `SceneStats::culled`.

Two cases need care and both are tested:

- A **shadow** must be culled against its blurred footprint, not its shape. A rectangle twenty
  pixels off the left edge with a forty-pixel blur still spills visibly into view.
- A **stroke** must be culled against bounds grown by half the stroke width.

## Primitives: one pipeline for the common case

Solid fills, rounded rectangles, borders, gradients, textured quads and box shadows all share the
`QuadInstance` layout and therefore one pipeline. They differ only in how the fragment is coloured.
Splitting them would multiply pipeline switches across a frame that is overwhelmingly made of
exactly this primitive.

Circles go through it too: a circle is a rounded rectangle whose radius is half its extent. That
keeps circles analytic and sharp instead of tessellating them into a mesh.

Paths are the exception. They tessellate through `lyon` into `MeshVertex` triangles — but none of
lyon's types appear in any public signature, so the tessellator is replaceable.

## Instance layouts

```rust
#[repr(C)]
pub struct QuadInstance {           // 96 bytes
    bounds: [f32; 4],               // local logical px
    radii: [f32; 4],                // clamped [tl, tr, br, bl]
    color: [f32; 4],                // linear premultiplied
    border_color: [f32; 4],         // (or the source rect for textured quads)
    border_width: f32,
    blur_sigma: f32,
    flags: u32,
    gradient: u32,                  // index, or u32::MAX
    transform_index: u32,
    clip_index: u32,
    _pad: [u32; 2],
}

#[repr(C)]
pub struct GlyphInstance {          // 64 bytes
    bounds: [f32; 4],               // glyph quad, local logical px
    uv: [f32; 4],                   // [u0, v0, u1, v1], normalised atlas coords
    color: [f32; 4],                // linear premultiplied
    coverage_contrast: f32,         // signed exponent, see docs/text.md
    flags: u32,                     // glyph_flags::{CLIP_ROUNDED, SUBPIXEL}
    transform_index: u32,
    clip_index: u32,
}
```

Two decisions shape this:

**Transforms and clips are indices, not inline matrices.** Sibling elements overwhelmingly share
both. Inlining a six-float matrix and a clip rectangle in every instance would roughly double the
bytes uploaded for a typical panel-heavy frame. They live in side tables — `StructuredBuffer`s on
Direct3D 11 — bound once per frame.

**Colours are linear premultiplied on the CPU.** The conversion happens once while building the
instance, never per fragment.

The source rectangle for a textured quad reuses the `border_color` slot, which is unused in that
case. That is a deliberate overload rather than growing the struct for every quad in the frame; the
shader comment and the emitter comment both say so.

`GlyphInstance` is smaller because a glyph is only ever a tinted bitmap: there is no border, radius or
shadow to describe. Both are `Pod`, so `Vec<QuadInstance>` becomes an instance buffer with one
`cast_slice` and no per-element work. A compile-time assertion pins both sizes, and the Direct3D 11
backend has a test that its input layout matches the 64-byte glyph instance.

## Layers are allocated only when they change the result

`Canvas::push_layer` returns `bool`. A fully opaque, normally-blended, unfiltered group needs no
offscreen target, and it returns `false` without recording anything. Opacity applied to individual
primitives is exact as long as they do not overlap; only overlapping content needs the group
semantics of a real layer.

When a layer *is* created:

- It is sized to its content, not to the window, and clamped to twice the surface size so a runaway
  transform or an unclipped shadow cannot allocate a gigabyte of render target.
- Its target origin is subtracted in the vertex stage (on Skia, folded into the canvas matrix),
  which is why the Direct3D 11 `PassUniforms` carries a `target_origin` that `FrameUniforms` does
  not.
- It always clears to transparent, because the texture came from a pool and may hold a previous
  frame's content.
- Only the first pass on a target clears it. A target resumed after a nested layer preserves what it
  already holds.
- When a layer is composited into a target that has no pass of its own yet — a layer at the very
  start of a frame — the compiler emits an empty clearing pass for that target first. Without it the
  surface's own first pass, which clears, came later and wiped the composited layer.

`Canvas` closes any layer left open on `Drop` and on `restore`. An unbalanced command stream would
desync the backend's target stack in a way that only shows up as a missing composite several frames
later.

## Clipping

Three tiers, cheapest first:

| Clip | Cost |
|---|---|
| `ClipKind::Rect` | Scissor rectangle. No mask, no offscreen pass, no per-fragment work. |
| `ClipKind::Rounded` | Scissor plus an analytic SDF evaluation per fragment. |
| `ClipKind::Path` | Scissor only, for now. |

A rounded clip whose radii are all zero degrades to `Rect` at record time, so the cheap path stays
alive. Instances under a rounded clip get a `CLIP_ROUNDED` flag; instances under a plain clip do
not, and pay nothing. The rounded test runs in scene space — the fragment position after the
transform and before the scale factor — because that is the space `GpuClip` is recorded in, so a
rounded clip stays put under a transformed child.

A path clip is recorded and its bounds feed the scissor, but neither backend builds the mask yet, so
the result is conservative: content is clipped to the path's bounding box, not to the path.

Nesting two rounded clips evaluates only the innermost analytically. The outer one is still enforced
by the intersected scissor, so the result is conservative rather than wrong. Nested rounded clips
are vanishingly rare in UI and are not worth a per-fragment loop.

`Scene::scissor_for` walks the clip chain intersecting bounds, with a depth guard so a malformed
chain degrades to an over-large scissor instead of hanging the render thread.

## Two backends

Both implement `RendererBackend` and consume the same `CompiledFrame`. `spherekit::Backend` picks
one per surface; see `docs/platform.md` for the selection rules and `SPHEREKIT_BACKEND`.

| | `spherekit-d3d11` | `spherekit-skia` |
|---|---|---|
| Where | Windows, any GPU at feature level 11_0 | Fallback on Windows; the only backend on Linux and macOS |
| Draws with | HLSL SM5 shaders, instanced | Skia's raster pipeline, one native call per instance |
| Presents through | Flip-model swap chain on a DirectComposition visual | `softbuffer` (GDI, X11, Wayland, CoreGraphics) |
| Blends in | Linear light | sRGB (gamma) space |
| Transparent windows | Yes, premultiplied alpha | No — rendered opaque |
| Vsync / latency | Sync interval, frame-latency waitable | Ignored; blit on present |
| RGB subpixel text | Yes, dual-source blending | No — grayscale |
| Layer blend modes | Normal only | All |
| `Filter::ColorMatrix` | Not applied | Yes |
| `Saturate`, `Blur`, `BackdropBlur` | Yes | Yes |
| Offscreen + `read_pixels` | Yes (WARP if no GPU) | Yes |

### Why Direct3D 11

A 2D UI needs instancing, a handful of blend states and offscreen targets. Direct3D 11 has all of
that on every Windows GPU of the last fifteen years, ships its shader compiler with the OS, and has
the best-supported path into DirectComposition, which is what makes transparent windows composite
properly. It replaced a wgpu backend; the HLSL shaders are expression-for-expression ports of the old
WGSL ones, so both produce the same pixels where both implement a feature.

A window is only ever given a **hardware** device. When there is none at feature level 11_0,
`D3D11Renderer::new` fails with `InitError::NoAdapter` rather than falling back to WARP, because the
Skia backend is the better software path for a window. `D3D11Renderer::new_offscreen` does use WARP,
so tests and screenshots run on any Windows machine.

### Why Skia, and why on the CPU

A CPU fallback is for the machines a GPU path cannot reach — a VM, a remote session, a broken driver —
and for platforms SphereKit has no GPU backend on. Everything the batch compiler emits is something
Skia draws natively and analytically: rounded rectangles and borders as `draw_rrect`/`draw_drrect`,
shadows as blur mask filters, glyph runs as one `draw_atlas` per run against an `Alpha8` mirror of
the atlas page, meshes as `draw_vertices`. Emulating the GPU shaders per pixel would be slower and
less precise than letting Skia's own rasterisers do the work.

It uses `skia-safe` with its default features. That is deliberate: rust-skia publishes prebuilt
binaries only for particular feature sets, and any other set forces a from-source Skia build (see the
comment in the root `Cargo.toml`, and `tools/bootstrap.ps1` for the toolchain that build needs).

The renderer is not `Send`.

## Shaders

The Direct3D 11 shaders live in `crates/spherekit-d3d11/shaders/`:

| Shader | Purpose |
|---|---|
| `common.hlsl` | Rounded-box SDF, derivative-based coverage, `erf`, the analytic shadow integral, frame bindings, transform/clip/gradient lookup |
| `quad.hlsl` | Rectangles, rounded rectangles, borders, gradients, textures, shadows |
| `text.hlsl` | Coverage-bitmap glyphs: grayscale, and the dual-source subpixel variant |
| `mesh.hlsl` | Vertex-coloured and textured triangles |
| `composite.hlsl` | Layer composite with opacity and saturation |
| `blur.hlsl` | Separable Gaussian, two passes; the surface's multisample resolve |

HLSL has `#include`, but resolving it needs an `ID3DInclude` and a search path, and the shaders ship
inside the binary. So `common.hlsl` is prepended to each shader body, followed by a `#line` directive
so a compiler diagnostic still names the right file and line.

**Compiled at build time.** Compiling all thirteen entry points with FXC (`d3dcompiler_47.dll`, part
of Windows) takes about 180 ms, nearly all of it the quad pixel shader, whose shadow integral the
compiler unrolls. That is too much to pay every time a window opens, so `build.rs` compiles
everything when the build host is Windows and embeds the DXBC; startup then spends nothing on
shaders. A cross build from another host has no FXC and falls back to compiling at device creation —
correct, only slower to start. `every_shader_compiles_from_source` compiles every source from scratch
*and* asserts the embedded bytecode is byte-identical to it, so stale precompiled shaders fail a test
rather than shipping.

Direct3D 11 has no pipeline objects; a "pipeline" here is an input layout, a topology, a shader pair
and a blend state. All of them are created once at device creation, so the render path cannot fail on
a missing state object.

The flag constants at the top of `quad.hlsl` and `text.hlsl` are hard-coded mirrors of
`quad_flags`/`glyph_flags` and carry a "keep in sync" comment. Nothing checks them mechanically; the
pixel tests in `tests/render.rs` catch a mismatch only by its effect.

### Antialiasing

There are two mechanisms, because there are two kinds of edge.

**Analytic, for the shapes with an equation.** `coverage_from_distance` divides the signed distance
by `fwidth(d)`. A fixed threshold would break the moment a shape is scaled or zoomed; `fwidth`
measures how much the distance changes across one actual pixel, so the antialiasing band is always
exactly one pixel wide regardless of transform. Rectangles, rounded rectangles, borders, circles and
shadows take this path and are smooth at any size for free. Glyphs need neither mechanism: their
antialiasing is in the coverage bitmap.

**Multisampling, for the shapes without one.** A tessellated path is a bag of triangles and has no
analytic edge at all — nothing in the pixel shader knows where the outline was. Those edges are
therefore *hard*, and an icon, a curve or a waveform outline is visibly jagged without help.

So on Direct3D 11 every target is drawn through a multisampled twin, 4× by default
([`SurfaceConfig::msaa_samples`]). The count is chosen once at startup from what the surface format
*and* the layer format both support, and rounds down rather than failing — a machine that cannot do
4× still opens a window.

Three details worth stating:

- A multisampled texture cannot be sampled, so it is resolved **lazily**: only when something is
  about to *read* the target (a layer being composited or blurred, a backdrop being copied) and once
  for the surface at the end of the frame.
- Layers resolve with `ResolveSubresource`. The surface cannot: the swap chain buffer is
  `B8G8R8A8_UNORM` while its multisampled twin is `_SRGB`, and differently typed formats may not be
  resolved into each other. A small pixel shader (`ps_resolve` in `blur.hlsl`) averages the samples
  in linear light instead.
- `a_layer_with_msaa_off_composites_the_same` renders the same frame at 1× and 4× so a sample-count
  mismatch between a layer and its composite cannot ship.

`alpha_to_coverage` is deliberately **off**: it would quantise every translucent primitive to the
sample count, which is far worse than the analytic alpha the shaders already produce.

The Skia backend has no sample count. `msaa_samples > 1` turns on antialiasing for the paths it builds
from tessellated meshes; at 1 they are drawn aliased, matching the GPU.

### Box shadows

Solved analytically rather than blurred. The x direction uses an `erf` approximation exactly; the y
direction integrates over eight slices of the rounded box's half-width at each height. That is enough
that the residual error is invisible at the opacities UI actually uses, and it costs a fraction of a
separable two-pass blur plus its render target. Skia draws the same shadow with a blur mask filter.

## Colour

Three representations, kept apart on purpose:

| Type | Space | Alpha | Where |
|---|---|---|---|
| `Color` | sRGB-encoded | straight | Authoring |
| `LinearColor` | linear light | premultiplied | Handed to every backend |
| `Hsla` | — | straight | Theme authoring |

`Color::lerp` round-trips through linear space. A test asserts the black-to-white midpoint is
`0.735` and not `0.5`; if that ever reads `0.5`, something started interpolating sRGB directly.

**Direct3D 11 blends in linear light.** Every render target is viewed through an sRGB format — the
swap chain's `B8G8R8A8_UNORM` buffers through `B8G8R8A8_UNORM_SRGB` views (flip model forbids an
sRGB *buffer* format but allows the view), offscreen layers as `R8G8B8A8_UNORM_SRGB` — so the hardware
encodes on write and blending happens before it, in linear light. Everything handed to the GPU is
linear and premultiplied, which is what avoids dark fringes on filtered edges and muddy midpoints in
gradients.

Textures are stored `R8G8B8A8_TYPELESS` with two views. Images arrive sRGB-encoded and are sampled
through the `_SRGB` view, which decodes them to linear; glyph atlas pages hold linear coverage and
are sampled through the `UNORM` view, as-is. Which view a texture gets is decided by whether it was
bound with `set_atlas_page`. The wgpu backend sampled everything through `UNORM`, which fed sRGB bytes
into a linear pipeline and encoded images twice; they showed too bright. `an_opaque_image_lands_byte_exact`
pins the fix.

**Skia blends in sRGB space.** Skia's raster pipeline blends in the destination's encoding, and every
surface it renders into is 8-bit sRGB, so colours are unpremultiplied and sRGB-encoded once on the
way in. Solid colours land on exactly the same 8-bit value as on Direct3D 11; only partial coverage
and translucent overlaps differ, in the direction of gamma-space blending — white at 50 % over black
is 128 on Skia and 188 on Direct3D 11. Gradients are the exception: their stops are handed to Skia as
linear sRGB with linear interpolation, so a red-to-green ramp passes through the same yellow on both.
`GlyphRun::coverage_contrast` is ignored on Skia, because it exists to emulate the gamma-space blend
Skia already does; see `docs/text.md`.

## Buffers and targets

On Direct3D 11 every per-frame buffer is `D3D11_USAGE_DYNAMIC` and written with
`Map(WRITE_DISCARD)`. Discard hands back fresh memory while the GPU may still be reading last frame's
contents, so there is no stall and no hand-written ring buffer: the driver renames the allocation.
Buffers grow to the next power of two and never shrink; a UI's frame size is stable, so after the
first few frames nothing reallocates.

An empty frame is entirely normal for a UI, and a zero-sized structured buffer cannot be bound, so
an empty transform, clip or gradient table is replaced by a single identity, infinite or empty
element rather than special-cased at each bind site.

Offscreen layer targets come from a pool keyed on size and are released after sixty unused frames —
on both backends. Freeing them the moment one frame skips them would reallocate every time a menu
opened and closed. Every target a frame names is reserved before the first pass runs, so nothing
allocates mid-frame.

## Surface failure is not an error

`SurfaceError` classifies every failure into exactly one recovery class, and a test asserts the
classes are disjoint and total:

| Class | Variants | Response |
|---|---|---|
| Transient | `Timeout`, `Occluded`, `ZeroSized` | Skip the frame |
| Reconfigure | `Outdated`, `Lost` | Reconfigure, retry |
| Fatal | `DeviceLost`, `OutOfMemory` | Rebuild everything |

Minimising a window reports `Occluded` and is transient. It must never tear down the device, and
there is a regression test named after exactly that. On Direct3D 11, `DXGI_STATUS_OCCLUDED` maps to
`Occluded` and a removed, reset or hung device to `DeviceLost`.

## Verification

Both backends have a pixel-level suite that builds a scene through the public `Canvas` API, compiles
it with the real `BatchCompiler`, renders offscreen and reads the pixels back:

```bash
cargo test -p spherekit-d3d11 -p spherekit-skia -- --nocapture
```

- `crates/spherekit-d3d11/tests/render.rs` — offscreen, on hardware or WARP. Where there is no device
  at all, or off Windows, each test prints why and passes: no device is an environment fact, not a
  defect. It also compiles every shader from source and checks the embedded bytecode.
- `crates/spherekit-d3d11/tests/window.rs` — the presentation path: a composition swap chain on a
  bare Win32 window, the frame-latency wait, present, resize and a transparent swap chain. Skips
  without a hardware device, since windows never use WARP.
- `crates/spherekit-skia/tests/render.rs` — the same ground on the CPU, plus the features only Skia
  implements: blend modes and colour matrices.

CI runs the suite on every platform; the Windows runners have no GPU, so the Direct3D 11 tests run
on WARP there.

To see a whole interface rather than a test scene, `crates/spherekit/examples/screenshot.rs` renders
offscreen through either backend and writes a PNG; see `docs/performance.md`.
