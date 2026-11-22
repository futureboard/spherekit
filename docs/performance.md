# Performance

What is measured, where the counters are, and what they are protecting.

This document does not contain benchmark figures for hardware you do not have. It names the
counters, says what each one defends, and reports the numbers actually observed on the one machine
this was developed against.

## Targets

| Goal | Enforced by |
|---|---|
| Idle CPU near zero when nothing changes | `RedrawPolicy::Idle` blocks the loop |
| No layout work for paint-only updates | `DirtyFlags::PAINT` does not propagate |
| Very low allocation per frame | Reused scenes, growable buffers, pooled targets |
| Minimal draw calls | Run-length batching, one pipeline for the common primitive |
| Fast text cache hits | Shaping cache, glyph bitmaps cached per size and phase in the atlas |

## The counters

Every layer reports what it did. These are the numbers to watch; a regression shows up here before
it shows up as a dropped frame.

### `LayoutStats` — `spherekit-layout`

| Counter | Watch for |
|---|---|
| `nodes_laid_out` | **Anything but zero on a paint-only frame.** The headline number. |
| `nodes_restyled` | Growth means styles are rebuilt when they did not change |
| `passes` / `skipped_passes` | A low skip ratio means something is over-invalidating |

`nodes_laid_out` counts layout-algorithm *invocations*, not distinct nodes: a flex container may
size a child twice under different constraints, and hiding that would make the number reassuring
rather than useful.

### `TreeStats` — `spherekit-ui`

| Counter | Watch for |
|---|---|
| `nodes_created` | Anything but zero on a steady frame means identity is unstable |
| `nodes_reused` | Should equal `elements` in steady state |
| `nodes_removed` | Churn means elements are appearing and disappearing |
| `elements_culled` | Should be large in a scrolled list; zero means culling is not working |

### `SceneStats` — `spherekit-render`

| Counter | Watch for |
|---|---|
| `commands` | Total recorded |
| `culled` | Rejected before an instance was written |
| `batches` | **Draw calls.** A jump at constant wall-clock is still a regression |
| `quads` / `glyphs` / `triangles` | Instance counts by kind |

### `FrameStats` — `spherekit-d3d11` or `spherekit-skia`

| Counter | Watch for |
|---|---|
| `draw_calls`, `pipeline_switches` | State changes, the thing batching exists to reduce |
| `bytes_uploaded` | Should fall to near-constant once caches are warm |
| `layers` | Each one is a render target and an extra pass |

Both backends fill in the same struct, so the workload counters are comparable across them, and
`SphereKitSurface::backend()` says which one produced the numbers. `cpu_ms` is not comparable: Skia
reports the time its `render` took, which *is* the rasterisation, while Direct3D 11 leaves it at zero.
`SurfaceStats::cpu_ms` — build, layout, paint, compile and upload, measured by the surface — is the
number to compare.

### Text and atlas

| Counter | Watch for |
|---|---|
| `TextSystemStats::hit_rate` | Should sit near 1.0; a low rate means a key changes every frame |
| `glyph_texels_uploaded` | Should reach zero once every on-screen glyph is rasterised |
| `AtlasStats::utilization` | Low with many pages means packing is fragmenting |
| `CacheStats::collisions` | Expected to stay at zero forever |

### Memory

`RendererBackend::memory_usage` reports the bytes a backend holds: textures, instance and table
buffers, pooled layer targets and, on Direct3D 11, the surface's multisampled twin. In steady state
it stops changing. A number that keeps climbing means a workload is still growing, or a pool is not
being reaped.

## Per-frame allocation

Designed out rather than optimised later:

- `Scene::reset` clears nine vectors and keeps every allocation. A fresh `Scene` per frame would
  re-grow all nine every time.
- Direct3D 11's dynamic buffers grow to a power of two and never shrink; they are rewritten with
  `Map(WRITE_DISCARD)`, so the driver renames them instead of stalling.
- `Tessellator` holds `lyon`'s scratch buffers across frames.
- Offscreen layer targets come from a pool keyed on size, released after 60 unused frames, on both
  backends. Freeing them the moment one frame skips them would reallocate every time a menu opened
  and closed.
- `RealtimeFrame` carries a scratch `Vec` for envelope extraction; a test asserts its capacity stops
  changing after the second frame.
- Hit testing has a `hit_test_all_into` variant, because hit testing runs on every mouse move and
  allocating there would be a per-frame allocation in the most frequent code path in the engine.
- `SmallVec` inline capacity covers glyph runs, gradient stops, the canvas state stack, the hit
  chain and handler lists.

## Caches

Every cache has a key, a byte budget, an eviction policy and hit/miss counters. There is no cache in
the engine that can grow without bound, because unbounded growth inside a host process that runs for
eight hours is not a performance problem, it is a crash.

| Cache | Keyed on | Bounded by |
|---|---|---|
| Shaping | text + style + width | Bytes, LRU |
| Glyph atlas | `GlyphKey` | Page count, LRU page eviction, idle-frame eviction |
| Image | Content hash | Bytes, LRU, with a this-frame guard |
| Direct3D 11 shaders and states | — | A fixed set, created once at device creation from precompiled bytecode |
| SVG geometry | id + size + scale | Bytes, LRU |

## What multisampling costs

On Direct3D 11 the surface and every offscreen layer are multisampled 4× so that tessellated paths
have smooth edges. That is not free:

- The surface's multisampled attachment is `width × height × 4 bytes × samples`. At 1180×720 that is
  roughly 13 MB; at 4K it is roughly 130 MB.
- Every layer allocates a second texture, because a multisampled texture cannot be sampled and has
  to resolve into a plain one before compositing.
- `memory_usage` accounts for both, so the cost is visible rather than inferred.

Two things keep it from being worse than it needs to be. Resolves are lazy: a target is resolved only
when something is about to read it, and the surface once at the end of the frame. And the count is
chosen once at startup from what the formats actually support, so a device that cannot manage 4×
silently runs at the highest it can rather than failing.

`SurfaceOptions::msaa_samples = 1` turns it off entirely. Everything except tessellated paths is
antialiased analytically and looks identical either way. The Skia backend has no sample count; the
option only switches antialiasing for its path meshes on or off.

## Culling and batching

Culling happens **before** an instance is written, not after, and increments `SceneStats::culled`.
Two cases need care and both have tests: a shadow must be culled against its blurred footprint, and
a stroke against bounds grown by half its width.

Batching is run-length over *adjacent* compatible commands. A global sort would batch better and
draw wrong, because UI is painted back to front. Within a compatible run the merge is total: 500
rectangles sharing a clip become one draw call, and a test asserts exactly that.

## Measured on one machine

NVIDIA GTX 1060 3 GB, Windows 11, `--release`, 180 frames of the plug-in demo. These figures predate
the move from wgpu (here on Vulkan) to Direct3D 11 and from distance-field to bitmap text, and have
not been re-measured since. The batch compiler that determines the draw-call and instance counts is
the same; the backend and the text path are not.

```text
draw calls:          26
pipeline switches:   25
quad instances:      10018
glyph instances:     178
mesh triangles:      1980
elements built:      39
elements painted:    39
nodes created:       0
nodes reused:        39
nodes laid out:      0
worst laid out:      0
cpu this frame:      1.150 ms
worst cpu frame:     2.243 ms
glyph texels up:     0
```

Read that as: ten thousand rectangles, multilingual text and three continuously-animating
realtime visualisations, at roughly one millisecond of CPU per frame, with the layout engine doing
**nothing at all**.

The 26 draw calls are what painter's order costs. The frame alternates panel backgrounds, labels
and meshes, and merging across that boundary would draw the labels underneath the panels.
Twenty-six for a full plug-in editor is the expected shape, not a defect.

These are figures from one GPU on one machine, reported because they were measured. They are not a
guarantee about yours.

### The Skia backend

`crates/spherekit-skia/tests/bench.rs` renders a dense 1920×1080 frame — 2000 quads (plain, rounded,
bordered, a few shadows) and 3000 glyphs in 100 runs — on the CPU. Median in `--release` on the
development machine:

```text
clear       0.15 ms
quads       3.55 ms
glyphs      2.0  ms   (one draw_atlas per run)
frame       5.5  ms
```

That is the fallback's budget: inside a 60 Hz frame for a dense interface, and the reason glyphs are
plain bitmaps rather than anything that needs per-pixel reconstruction (see `docs/text.md`).

## Reproducing

```bash
# Bounded run that prints the report above.
SPHEREKIT_DEMO_FRAMES=180 cargo run -p spherekit --example plugin_ui_demo --release

# Both backends' pixel suites. Direct3D 11 runs on WARP where there is no GPU,
# and also compiles every HLSL shader and checks the embedded bytecode is current.
cargo test -p spherekit-d3d11 -p spherekit-skia -- --nocapture

# The Skia frame benchmark above.
cargo test -p spherekit-skia --release --test bench -- --ignored --nocapture

# A whole interface rendered offscreen to a PNG, through either backend.
cargo run -p spherekit --example screenshot --release -- out.png
SPHEREKIT_BACKEND=skia   cargo run -p spherekit --example screenshot --release -- skia.png
SPHEREKIT_BACKEND=d3d11  cargo run -p spherekit --example screenshot --release -- d3d11.png
SPHEREKIT_SCALE=2        cargo run -p spherekit --example screenshot --release -- hidpi.png
SPHEREKIT_RASTER=outline cargo run -p spherekit --example screenshot --release -- outline.png
```

`SPHEREKIT_RASTER=outline` swaps DirectWrite for the portable outline rasteriser, so the same
interface can be compared across rasterisers as well as backends.

On Windows, set `CARGO_INCREMENTAL=0` if a workspace-wide test run fails at the link step; see the
build notes in the README.

## Not yet measured

Honesty about the gaps:

- There is **no Criterion benchmark suite yet.** The counters above are what exists, and the demo's
  bounded run is the only end-to-end measurement. A benchmark crate is on the roadmap.
- GPU frame time is not measured. `FrameStats::gpu_ms` is `None` on both backends: Direct3D 11 has
  timestamp queries, but nothing reads them yet.
- There are no golden-image tests yet. The readback path now exists —
  `SphereKitSurface::new_offscreen` and `read_pixels` on both backends, which the screenshot example
  uses and the backend pixel suites assert against — but nothing compares whole frames to stored
  references. The demos' `SPHEREKIT_DEMO_FRAMES` report catches *structural* regressions — a jump in
  draw calls, a nonzero `nodes_laid_out` — but not visual ones.
- The demo figures above come from one discrete NVIDIA GPU, before the current backends existed.
  Integrated graphics, WARP and the Skia backend on Linux and macOS are untested for end-to-end
  performance.
