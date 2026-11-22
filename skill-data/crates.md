# The crate map

The engine's crates, all at `2026.8.0`. The layering is
enforced, not aspirational: the graphics engine has no idea the UI layer exists,
and the UI layer has no idea which backend is drawing it.

```text
Application
    -> spherekit-ui         elements, events, focus, widgets
    -> spherekit-layout     retained nodes, style, dirty flags, hit testing
    -> spherekit-render     canvas, scene, culling, batching -> CompiledFrame
    -> spherekit-d3d11      Direct3D 11 + DirectComposition (Windows GPU)
     | spherekit-skia       Skia on the CPU + softbuffer (fallback; Linux, macOS)
```

`SphereKitSurface` picks the backend: `SurfaceOptions::backend` is
`Backend::Auto` (Direct3D 11 on a hardware device, else Skia), `D3D11` or
`Skia`, and `SPHEREKIT_BACKEND=d3d11|skia|auto` overrides it at run time.
`SphereKitSurface::backend()` reports the one actually in use.
`SphereKitSurface::new_offscreen` + `read_pixels()` render without a window
(screenshots, pixel tests); see `crates/spherekit/examples/screenshot.rs`.

`spherekit` is a facade over all of it. Prefer `spherekit::ui::…` in an
application; depend on a leaf crate directly only when you genuinely want just
that layer (a plug-in that needs `spherekit-core`'s geometry and nothing else).

| Crate | What lives there |
|---|---|
| `spherekit-core` | `Px`, `Length`, `Size`, `Rect`, `Point`, `Edges`, `Corners`, `Color`, `Brush`, `Shadow`, transforms, paths, `animate`, `time`, ids |
| `spherekit-render` | `Canvas`, `Scene`, `DrawCommand`, `Filter`, batching, culling, the `RendererBackend` seam |
| `spherekit-d3d11` | `D3D11Renderer`, HLSL in `shaders/*.hlsl` (precompiled by `build.rs`), DirectComposition swap chain. Off Windows it compiles to a stub that reports `NoAdapter` |
| `spherekit-skia` | `SkiaRenderer`: Skia raster presented through `softbuffer`. Blends in sRGB space; no transparent windows, no vsync, not `Send` |
| `spherekit-text` | `FontDatabase`, `TextSystem`, `GlyphRasterizer` (DirectWrite / outline), shaping via rustybuzz, glyph atlas, `TextStyle`, `FontRequest`, `FontWeight`, `TextAlign` |
| `spherekit-layout` | `Style` and its enums, `LayoutTree`, `DirtyFlags`, hit testing. Taffy is the backend and appears in no public signature |
| `spherekit-svg` | `SvgCache`. Refuses unsupported documents rather than silently dropping content |
| `spherekit-image` | Decoding and the texture cache |
| `spherekit-platform` | `App`, `AppContext`, `AppHandler`, `Window`, `WindowAttributes`, `WindowEvent`, `InputTranslator`, clipboard, monitors, custom Windows chrome |
| `spherekit-ui` | `Element`, `Div`, `AnyElement`, `Styled`, `Interactive`, `ParentElement`, `IntoElement`, `PaintStyle`, `UiTree`, `Theme`, focus, semantics, widgets |
| `spherekit-audio-ui` | `dsp`, `realtime`, `transfer`. Meters, waveforms, spectra, and the lock-free audio-thread boundary |
| `spherekit-css` | `Stylesheet`, `ResolvedStyle`, `InteractiveStyle`, `StyleContext`, `ElementState`, `MatchPath`, `Node`, `TextProperties` |
| `spherekit-react` | `ReactHost`, `NativeTree`, `NativeNode`, `EventQueue`, `HostEvent`, `LowerContext`, `NodeBuilder`, `PRELUDE` |
| `spherekit-bridge` | `BridgeMessage`, `ApiBridge`, `JsonLines`, and `JsBridge` behind the `v8` feature |
| `spherekit-jsengine` | `Engine` over V8 via rusty_v8 (`v8` crate, prebuilt static lib downloaded at build time) |
| `spherekit` | The facade, `SphereKitSurface`, `SurfaceOptions`, `Backend` |
| `spherekit-cli` | `spherekit react <name>` scaffolding and `spherekit build` |

The npm half is `@spherekit/react`, also `2026.8.0`.

## Applications in the repo

| Path | What it demonstrates |
|---|---|
| `app/uigallery` | Every built-in widget, live. Custom Windows chrome over DWM Mica. The reference for the house visual style |
| `app/reactdemo` | The smallest complete React-in-V8 app |
| `app/musicplayer` | The full stack: React UI, real audio via rodio, realtime visualisers as native host types, a folder browser |

## Feature flags that matter

- `spherekit/v8` and `spherekit-bridge/v8` pull `spherekit-jsengine`. Off by
  default, because rusty_v8 downloads a ~230 MB prebuilt V8.
- `spherekit-platform` with `--no-default-features` is the plug-in
  configuration: no window runner, no clipboard, host-owned window only. It is
  a real build that CI checks; do not break it.
