# Text

Why every glyph is a coverage bitmap, where those bitmaps come from, and what it takes to make them
look sharp.

## The pipeline

```text
text + style
   ↓  bidi resolution                    unicode-bidi
   ↓  script itemisation                 unicode-script
   ↓  font resolution and fallback       fontdb
   ↓  shaping, per run                   rustybuzz
   ↓  line breaking                      unicode-linebreak
   ↓  alignment and justification
   ↓  coverage rasterisation             DirectWrite (Windows) | outline + raster
   ↓  paged atlas placement
   ↓  textured, tinted quads             spherekit-d3d11 | spherekit-skia
```

Nothing above the rasteriser is SphereKit-specific: bidi, itemisation and shaping are solved problems
with correct Rust implementations, and reimplementing them would be a large amount of work whose
best possible outcome is parity. Shaping and layout stay in SphereKit on every platform, even where
DirectWrite does the rasterising, so a layout measures identically whichever rasteriser draws it and
on whichever operating system.

## Why bitmaps

Every glyph is rasterised at its **exact device size** — `font_size × scale_factor`, rounded — and
at one of four **quarter-pixel horizontal phases**, as grayscale or RGB-stripe coverage. The result
is cached in the atlas, and the renderer only ever samples a bitmap and multiplies it by a colour.

That last property is the point. A renderer that only has to tint a texture can draw text as cheaply
on the CPU as on the GPU: the Skia backend draws a whole run of glyphs with one `draw_atlas` call
against an `Alpha8` mirror of the atlas page. Anything that needs per-pixel reconstruction in a
shader — a distance field, a curve-coverage shader — would have to be emulated per pixel on the CPU
fallback, which is exactly where there is no time to spare.

The second reason is that a bitmap rasterised for one exact size can be **fitted to that size's pixel
grid**. That is most of what separates crisp interface text from soft interface text at 100 %, and
no size-independent representation can do it, because there is no single size to fit to.

SphereKit used multi-channel signed distance fields until the renderer was rebuilt on Direct3D 11
and Skia. They were dropped for both reasons above: at interface sizes on a 100 % display a field
minified through a one-pixel antialiasing ramp leaves a regular-weight stem almost no solid core and
cannot be grid-fitted, and a CPU backend would have had to run the median-of-three reconstruction
for every glyph pixel.

**What it costs.** Bitmaps are keyed per device size and phase, so an interface with five type sizes
at one scale factor holds five sets of glyphs rather than one size-independent set, up to four times
over for glyphs that land on different phases. Interface text uses a handful of sizes, and grayscale
pages are one byte per texel, so this is a trade worth making for an interface; it would not be for a
continuously zooming canvas.

Text under a **scaling or rotating transform** is drawn from the bitmap rasterised for the scene's
scale factor and resampled, not re-rasterised at the transformed size. A zooming panel's text
softens while it is zoomed. Content that zooms as a steady state should change its font size rather
than its transform.

## The rasterisers

[`GlyphRasterizer`] picks which one produces the bitmaps; `TextSystem::set_rasterizer` changes it.

### DirectWrite — `GlyphRasterizer::Platform`, the default on Windows

`crates/spherekit-text/src/dwrite.rs` hands DirectWrite one glyph index at a time and asks
`IDWriteGlyphRunAnalysis` for its coverage, with `DWRITE_RENDERING_MODE_NATURAL_SYMMETRIC`:
grayscale glyphs as `DWRITE_TEXTURE_ALIASED_1x1`, subpixel ones as `DWRITE_TEXTURE_CLEARTYPE_3x1`.
That is the same analysis Direct2D, Edge and the shell use, so SphereKit text has the same symmetric
antialiasing, the same `gasp`-driven vertical grid fitting and the same ClearType filtering as
every other Windows application.

Two things are deliberately *not* borrowed:

- **Gamma and enhanced contrast.** The coverage is taken raw. DirectWrite's gamma and contrast are
  its answer to blending in gamma space; SphereKit blends in linear light and applies its own
  correction (`coverage_contrast`, below). Applying both would correct twice.
- **Shaping and layout**, as above.

The glyph is rasterised with its baseline origin at `(phase / 4, 0)`, so the texture bounds
DirectWrite returns are whole device pixels relative to the *integral* pen — the same convention the
outline rasteriser follows, which is what the batch compiler's snapping relies on.

System fonts are referenced by file path, so nothing is copied. Fonts loaded from memory go through an
`IDWriteInMemoryFontFileLoader`. A face DirectWrite refuses to load is remembered as refused and
falls through to the outline rasteriser; `TextSystemStats::platform_glyphs` counts how many glyphs
DirectWrite actually produced.

### Outline — `GlyphRasterizer::Outline`, everywhere else

The portable rasteriser, and the only one off Windows. Forcing it on Windows gives identical output on
every platform, which is what cross-platform pixel tests want.

**Outline extraction** (`outline.rs`). `ttf-parser` yields contours as line, quadratic and cubic
segments. Coordinates are normalised into em units by dividing by `units_per_em`, and TrueType's y-up
convention is flipped to SphereKit's y-down exactly once, in the outline collector. Rather than
assuming a winding convention, orientation is re-derived per glyph from the total signed area, which
makes glyf (clockwise) and CFF (counter-clockwise) outlines reach the same code correctly.

**Coverage** (`raster.rs`). An analytic-coverage scanline rasteriser: signed area accumulated per
cell, then prefix-summed along each scanline. Exact for polygons, no supersampling. Curves are
flattened first, so flattening error is the only error in the output. The subpixel variant rasterises
at three times the horizontal resolution and runs FreeType's default five-tap LCD filter over it.

**Vertical grid-fitting** (`hint.rs`), below. DirectWrite does its own and gets no help from it.

## Vertical grid-fitting

Snapping a glyph's quad to the pixel grid puts its *baseline* on a whole pixel and does nothing for
the other horizontal edges — and those are most of what the eye reads: the x-height line across the
top of `n`, `o` and `x`, the cap line across `H`, the crossbar of `e`. At 13 device pixels an
x-height of 0.5 em is 6.5 pixels, so the top of every lowercase letter is two rows of grey instead of
one row of black.

`hint.rs` builds a monotonic, piecewise-linear map from unfitted em `y` to fitted em `y`. Each of the
five zones — ascender, cap height, x-height, baseline, descender — is moved to the nearest whole
pixel, and the outline between them is stretched linearly to follow.

Four properties, each with a test:

- **The baseline is a fixed point.** Zero pixels rounds to zero, so a fitted glyph sits on exactly
  the baseline layout computed. A fit that moved it would shift every line against the boxes
  around it.
- **The map is monotonic.** A non-monotonic map puts a lower point above a higher one and turns the
  glyph inside out, which is worse than no fitting at all. A face whose reported metrics are
  nonsense — zeroed, inverted, absurdly scaled — is refused rather than fitted.
- **Nothing moves more than half a pixel.** Fitting is a nudge. A larger displacement would mean a
  zone snapped to the wrong grid line and the glyph changed proportion visibly.
- **Points beyond the zones extrapolate rather than clamp**, so a tall accent or a deep tail moves
  with the part of the glyph it is attached to.

The map is applied to the **flattened polylines**, not to the curve control points. Pushing control
points through a piecewise-linear map bends the curve between them by an amount nothing accounts for;
moving the flattened vertices is exact for the geometry actually rasterised.

### Only vertical

Horizontal grid-fitting means moving stems onto pixel boundaries, which changes the width of every
glyph and therefore its advance. Doing it properly needs the font's own hinting bytecode or an
autohinter that understands stem detection; doing it improperly quantises letter spacing into visible
clumps. Vertical fitting changes no advance at all, because it only ever moves `y`.

Fitting needs one exact device size, which every glyph now has. `GridFit::new` returns `None` when
there is nothing useful to do — a size so small that zones collide, or so large that they already sit
within a fraction of a pixel of the grid.

### Overshoot suppression

Round letters — `o`, `e`, `c`, `s`, `O`, `C`, `G`, `S` — are drawn slightly taller than flat ones so
they do not read as smaller. Measured across ten faces installed here, that overshoot is **0.0088 em
(Consolas) to 0.0166 em (Georgia)**, which is 0.11 to 0.22 device pixels at 13 px. The x-height line
snaps onto a whole pixel and the apex of `o` lands a fifth of a pixel above it — one extra grey row
that `x` does not have.

The mechanism is one line: **`shoot.fit = ref.fit`**. The round extremum is not rounded on its own,
it is *given* the position the flat one rounded to. In a piecewise-linear map that is a segment whose
two control points share a target — a flat slab. Five control points become eight:

```text
  (ascender,                    snap(ascender))
  (cap_height - cap_overshoot,  snap(cap_height))   <- slab
  (cap_height,                  snap(cap_height))
  (x_height   - x_overshoot,    snap(x_height))     <- slab
  (x_height,                    snap(x_height))
  (0.0,                         0.0)
  (0.0        + base_overshoot, 0.0)                <- slab
  (descender,                   snap(descender))
```

Rounding the shoot independently is what goes wrong: two positions a fifth of a pixel apart can round
onto different rows, or onto the same row from opposite sides, and neither is a suppression.

Suppression only applies while the overshoot is under half a device pixel. Georgia's 0.0166 em
crosses that at about 30 device pixels, and from there on flattening it would visibly flatten round
letters, so the slab is not built.

**The furthest, never the median.** FreeType can take a median because its capture radius is far
wider than the probe set's spread; a slab exactly as wide as the overshoot has no such slack and must
span the whole set. Measured here, a median met the objective in **30 of 100** face/size combinations
against **100 of 100** for the furthest — Tahoma's round tops spread 0.0024 em and a median slab
leaves its `o` outside, nine rows at 13 px against `x`'s seven.

**Capitals belong in the baseline set.** Arial's `O` bottoms 0.0005 em lower than its `o`, Georgia's
0.0020, Trebuchet's 0.0024. A lowercase-only probe set leaves `O` one row taller than `H` on all
three.

### The invariant that actually matters is bounded slope, not monotonicity

The obvious implementation is a point-wise capture band: `if |y - line| < band { fitted } else
{ map(y) }`. That is a non-decreasing step function of y, so it **passes a dense monotonicity sweep
with a green light** while tearing two adjacent flattened vertices a quarter of a pixel apart at the
band edge. `Rasterizer::coverage` takes `acc.abs()`, so a folded contour does not fail loudly — it
quietly produces a wrong row.

`GridFit::max_slope` is what separates the two. A slab has zero slope; a tear has infinite slope.
Measured worst over ten faces at ten sizes: **1.608**, against 1.472 for the five-knot map on the same
face and size.

A constant band cannot be tuned to work, either: it must be at least 0.0166 em to cover Georgia's cap
overshoot, and at 0.0166 it exceeds Tahoma's i-dot headroom of 0.0049 em. The ranges overlap across
faces, so no single constant separates overshoots from features. That is the measured reason the
per-face probe is not a nicety.

### The cost, and what it buys

Reading a face's zones is seventeen glyph lookups plus two `OS/2` reads — **5 to 12 µs** measured
here, once per face, memoised in `FontDatabase::vertical_zones` beside the coverage and fallback
memos. It survives cache invalidation for the same reason coverage does: it is a property of bytes
already loaded, and those cannot change.

What it buys, over ten faces at ten sizes: `o`/`e`/`s`/`c` land on exactly `x`'s height and
`O`/`G`/`S` on exactly `H`'s in **100 of 100** combinations, against 10 of 100 without it.

### The known cost: a slab can catch a feature that is not an overshoot

A slab is as wide as the face's overshoot, and on some faces a real feature sits inside that band. The
cap slab is the one that bites, because lowercase ascenders reach just past the cap line:

| face | cap slab | nearest feature above the cap line | headroom | |
|---|---|---|---|---|
| Arial | 0.0122 | `f` at −0.7280 | 0.0117 | inside |
| Tahoma | 0.0151 | `j` at −0.7319 | 0.0049 | inside |
| Verdana | 0.0151 | `j` at −0.7319 | 0.0049 | inside |
| Trebuchet MS | 0.0127 | `j` at −0.7207 | 0.0054 | inside |
| Consolas | 0.0088 | `t` at −0.6470 | 0.0088 | at the edge |

Five of ten faces pull the dot of `j`, or the hook of `f`, or the ascender of `t`, onto the cap line
along with the round capitals. The displacement is bounded by the slab — at most 0.22 device pixels
at 13 px — and it is not fixable by narrowing the band, because the same measurement that shows the
collision shows the overshoots and the headrooms overlapping across faces.

### What is still missing

Stem darkening — thickening strokes slightly at small sizes, because the eye reads a thin dark stroke
as lighter than its coverage says. DirectWrite does not apply any to the raw coverage SphereKit asks
for, and the outline rasteriser has none. The linear-light correction below recovers most of the
weight that gamma-space blending would have given; what is left is perceptual.

## Subpixel (RGB) text

`SurfaceOptions::rgb_subpixel_text` is on by default. A subpixel glyph carries three independent
coverages, one per stripe, and blending that onto a surface needs three independent destination
factors — which is what dual-source blending provides and ordinary premultiplied-alpha blending
cannot. So it is enabled only where every link can preserve it:

- the backend reports `dual_source_blending` (Direct3D 11 does; Skia does not, and draws grayscale);
- the surface is not transparent — stripe coverage over an unknown backdrop has nothing to filter
  against;
- the glyph is drawn straight onto the surface, not into a layer, because a layer would collapse the
  three coverages into ordinary RGBA before its opacity or filter is known;
- the transform is a translation, because the stripes are physical.

The physical stripe order and panel rotation cannot be queried through any graphics API, so an
application targeting BGR, non-striped, rotated, remotely displayed or rescaled output must turn the
option off.

## The atlas

**Paged, not one giant texture.** Pages default to 2048², clamped at startup to the backend's
maximum. A single unbounded texture would fail late and badly on a device that cannot hold it.

**Separate page sets per format.** A grayscale glyph is R8, a subpixel glyph RGB8 and a colour glyph
RGBA8. Those cannot share storage, so each [`GlyphFormat`] has its own page list and its own page
limit. The renderer is told which texture backs each page through
`RendererBackend::set_atlas_page`.

**Shelf packing**, implemented in `atlas.rs` rather than pulled from a crate, because it is core
functionality and the eviction policy has to be integrated with it. Glyphs are placed into the
first shelf whose height fits within a slack threshold, else a new shelf opens.

**One texel of padding** between glyphs. Without it, bilinear sampling bleeds a neighbour into a
glyph's edge — a real, visible defect, and there is a test for it.

**Dirty-region upload.** A 2048² page is 4 MB of R8, 16 MB of RGBA8; uploading it every frame would
dominate the frame budget. `take_dirty_regions` returns only what changed, borrowing the atlas's own
mirror so the upload costs no copy. In steady state, once every glyph on screen has been rasterised,
it returns nothing — the demo reports `glyph texels up: 0`.

**Atlas pages hold linear coverage.** The Direct3D 11 backend stores every texture typeless with two
views and samples atlas pages through the `UNORM` one: coverage is a geometric fraction, not a
colour, and gamma-decoding it on sample would bend every edge.

**Graceful failure.** A glyph larger than a page, or a full atlas at the page limit, returns
`FontError::AtlasFull`. It never panics. One missing glyph is recoverable; a blank window is not.

## The default family

A [`TextStyle`] with no family named resolves through `fontdb`'s generic sans-serif, whose default is
**Arial**. That is the Windows 95 interface font: the shell has not used it in decades, and on a
non-Latin system it has no coverage for the language the user actually reads in, so every unstyled
string fell through to a fallback chosen by accident.

`spherekit-text::system_ui` fixes it by asking the platform. On Windows that means reading
`NONCLIENTMETRICSW::lfMessageFont` through `SPI_GETNONCLIENTMETRICS`, because the interface font
there is **not a constant** — the shell picks it per locale:

| Locale | `lfMessageFont` |
|---|---|
| Latin | Segoe UI |
| Japanese | Yu Gothic UI |
| Korean | Malgun Gothic |
| Traditional Chinese | Microsoft JhengHei UI |
| Thai | Leelawadee UI |

Hard-coding `Segoe UI` would put Latin metrics on a Japanese desktop. Reading it also picks up a
user's own font change, which is an accessibility setting on Windows and not merely a preference.

The monospace generic moves too, from `Courier New` — a typewriter face that hints badly at
interface sizes — to Consolas on Windows, SF Mono on macOS and DejaVu Sans Mono elsewhere. That one
*is* a constant, because no platform exposes a system-monospace setting to query, and the code says
so rather than pretending otherwise.

## Fallback

A codepoint the primary face lacks walks a chain built from its script: Latin, Thai, Japanese,
Chinese, Korean, Arabic, Hebrew, Cyrillic, Devanagari, plus an emoji slot. Platform-specific family
lists are behind `cfg(target_os)` — Segoe UI, Leelawadee UI, Yu Gothic UI, Microsoft YaHei, Malgun
Gothic on Windows; Noto and DejaVu on Linux; Hiragino, PingFang and Apple Color Emoji on macOS —
but none of them is required. When no named family matches, the database is scanned for any face
containing the codepoint.

Font discovery and fallback never go through DirectWrite or CoreText. DirectWrite is used on Windows
only to rasterise a glyph SphereKit has already chosen, and when it is unavailable the outline
rasteriser draws the same glyph.

## Shaping and clusters

Script itemisation treats `Common` and `Inherited` as continuations of the surrounding run rather
than starting a new one; otherwise every space and comma splits a run and shaping quality collapses.

Every shaped glyph carries a `cluster: Range<usize>` mapping back to **original string** byte
indices, not run-local ones. Without that, a click can be turned into a pixel offset but not into a
text index, and caret placement and selection are impossible. Getting it wrong is easy and the
symptom is subtle, so the cluster ranges are tested for monotonicity and full coverage.

Letter spacing is applied after shaping and only at grapheme cluster boundaries, so it is never
inserted inside a ligature or a combining sequence.

## Caching

| Cache | Key | Prevents |
|---|---|---|
| Shaping | `(text, style, max_width)` | Reshaping a paragraph every frame |
| Glyph raster | `GlyphKey` | Rasterising a glyph twice |

Both are bounded by bytes with LRU eviction and hit/miss counters. The shaping cache stores the
source text alongside the fingerprint so a hash collision cannot silently return the wrong layout;
collisions are counted and expected to stay at zero.

`GlyphKey` is `(font, glyph, variation_hash, bitmap_size, format, subpixel_phase)`. `bitmap_size` is
the rounded device size and is never zero. `format` keeps grayscale and RGB coverage of the same
outline from aliasing in the atlas, and each nonzero phase names a distinct image because the
rasteriser shifts the outline by it before snapping the ink box.

## Placement on the pixel grid

The atlas's UV convention is *edge-aligned*: `uv_rect` bounds the outer edge of the texel block, and
the documented promise is that interpolating across a quad covering exactly `texels.size` device
pixels lands on texel centres. A bitmap sampled at a fractional offset, or stretched by a fraction of
a texel, is a blurred copy of itself.

So `snap_glyph_quad` in the batch compiler snaps both the origin and the extent — the extent to the
exact texel count the atlas allocated. Horizontal position is not lost to that: before asking for the
glyph, the compiler splits the device-space pen into an integral part and one of four quarter-pixel
**phases**. The rasteriser shifts the outline by the phase, and the bitmap is drawn at the integral
pen, so the fractional position is applied exactly once and letter spacing is quantised to a quarter
pixel rather than to a whole one.

**The correction is derived from the pen, not from the ink.** Every glyph has a different ink top — an
`x`, an `l` and a `g` all begin at different heights, at fractional offsets — so rounding each quad's
own top edge independently of the pen would hand every glyph in the line a *different* sub-pixel
shift and tear the shared baseline apart. Both rasterisers snap the ink box outwards to whole device
pixels *from the integral pen*, so the box is already an integer number of pixels from it and
rounding the quad is grid-exact and run-consistent.

Snapping only happens when the transform is a translation. Under rotation there is no pixel grid to
snap to, and forcing one would make text crawl as the transform animates.

## Coverage in linear light

Everything the Direct3D 11 backend draws blends in linear light, which is physically correct and is
what makes gradients and translucent overlaps come out right. Text is the one primitive it is wrong
for, and it was wrong by a lot.

Coverage is a geometric fraction of a pixel. Written into a linear buffer and encoded by an sRGB
surface, half coverage of white on black shows up at **sRGB 0.735** — the single grey pixel beside a
stem comes out nearly three quarters as bright as the stem itself. A one-pixel antialiased edge
therefore reads as a two-pixel glow, and that glow is what "soft text" is. No rasteriser change fixes
it, because the coverage the rasteriser produced was correct.

Every traditional text renderer composites glyph coverage in *gamma* space instead. egui goes as far
as asking its backend for a non-sRGB framebuffer specifically so that its whole pipeline blends that
way, and warns when it is handed an sRGB one. SphereKit cannot follow it there — the linear blend is
load-bearing for every other primitive — so it reproduces the same result by bending the coverage
ramp before the blend, per glyph, in the text shader.

Which way it bends depends on which side of the background the text sits:

```text
light text on dark:  alpha = coverage ^ k
dark text on light:  alpha = 1 - (1 - coverage) ^ k
```

Those are **mirror images, not reciprocal exponents**. The blend is linear in the *destination*, so
it is always the end of the ramp nearest the background that has to bend. Applying one exponent in
both directions — which is what this engine did before, with 1.25 for light-on-dark and 0.8 for
dark-on-light — cancels a quarter of the error at one end and adds to it at the other.

[`GlyphRun::coverage_contrast`] carries both: `|value|` is `k`, and its sign picks the branch.
`1.0` disables the correction, which is what text over an image wants, since there is no one
background to correct against. `Label::coverage_contrast` sets it per label. For RGB subpixel glyphs
the same correction is applied to each channel's coverage.

**The exponent is solved, not interpolated.** [`coverage_contrast_for`] takes the two colours, finds
their midpoint in sRGB, decodes it, and reads off the alpha a linear blend needs in order to land
there; `k` follows from inverting whichever branch applies. Black on white solves to 2.224, which is
where the documented `FULL_COVERAGE_CONTRAST` of 2.2 comes from.

Interpolating on the luminance gap instead — `1 + |delta| * 1.2`, the obvious thing to write — is
wrong in the case that matters most. Muted grey text on a dark ground has roughly a third of the
luminance contrast of white on black, but it sits almost entirely inside the steep part of the
encode and needs nearly the same exponent: **1.82 against 2.22**. An interpolated ramp hands it under
1.5, and every secondary label in the interface stays soft while the primary ones come good.

Measured against a gamma-space blend across the whole coverage ramp, as RMS and worst-case error in
8-bit levels:

| pair | solved `k` | before | after |
|---|---|---|---|
| white on black | +2.224 | 39 / 53 | 3.2 / 9.0 |
| black on white | −2.224 | 43 / 62 | 3.2 / 9.0 |
| dark theme, `#E6E9EF` on `#14161A` | +1.970 | 27 / 38 | 3.7 / 8.3 |
| dark theme muted, `#939BA8` | +1.817 | 18 / 25 | 2.5 / 5.5 |
| light theme, `#1A1D22` on `#F5F6F8` | −1.924 | 29 / 41 | 4.0 / 8.7 |
| light theme muted, `#606772` | −1.439 | 8 / 12 | 2.3 / 4.7 |

In displacement terms the old calibration moved the perceived edge by 0.18 to 0.24 px on each side
of every stem — most of half a pixel of stroke weight, on every glyph.

`the_correction_reproduces_a_gamma_space_blend` checks every one of those pairs at 33 points along
the ramp. `alpha_from_coverage` is the CPU copy of `apply_coverage_contrast` in `text.hlsl`; the
tests call it rather than writing the formula a second time.

This is a *perceptual* correction, not a physical one, and the documentation says so where the
constant is defined rather than leaving it to be discovered.

**The Skia backend ignores it.** Skia blends in the destination's encoding, and every surface it
renders into is 8-bit sRGB, so its blend already *is* the gamma-space blend the exponent exists to
reproduce. Applying it there would correct an error that was never made and visibly thin every glyph.
This is also why DirectWrite's coverage is taken raw: its own gamma and contrast are a third answer to
the same question.

## Synthetic outlines

`Label::outline(width, color)` draws a halo, not a stroked outline: the run is stamped
`OUTLINE_TAPS` (8) times around a circle of radius `width` in the outline colour, then the fill is
drawn on top. That reuses the cached glyph bitmaps, so it costs draw instances rather than
rasterisation, and it reads as a solid edge up to about two pixels wide.

## Layers have to land on the device grid

`snap_glyph_quad` rounds a glyph to the device grid in *absolute* coordinates, knowing nothing about
which render target it will be bound into. That is fine for the surface, whose origin is zero, and it
was not fine for an offscreen layer: `begin_layer` sized the target from `bounds.round_out(scale)` —
floored origin, ceiled extent — but handed the vertex stage the raw fractional `bounds.origin` to
subtract. Every snapped glyph inside a layer therefore sat `frac(origin * scale)` off its texel and
went through the atlas's linear sampler as a real bilinear blur, and the composite then resampled a
`round_out`-sized texture into a fractional destination rect on top of that.

Both now come from one snapped rectangle: the origin is the device-aligned one the texture actually
starts at, and the extent is the *clamped* size, since an oversized layer is capped and describing
the destination with the unclamped rect would stretch the texture across it.

This matters in practice: the UI tree opens a layer for any element with opacity below 1 or a filter,
so without it "all the text in that panel goes blurry while it fades" — a hard thing to attribute
after the fact.
