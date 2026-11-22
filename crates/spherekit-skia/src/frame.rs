//! Turning a [`CompiledFrame`] into Skia draw calls.
//!
//! The compiled frame was shaped for an instanced GPU pipeline: flat instance
//! arrays, indexed transform and clip tables, batches grouped by pipeline. On
//! the CPU there is no instancing to exploit, so every instance becomes one
//! native Skia call — a rounded rectangle, a blurred mask, an image blit — and
//! Skia's own analytic rasterisers do what the GPU shaders did by hand. The
//! *meaning* of each field is defined by the old wgpu shaders, and the comments
//! below say where each mapping comes from.
//!
//! ## Coordinate spaces
//!
//! Instances are in local logical pixels. The GPU vertex stage computed
//!
//! ```text
//! device = (matrix · local) · scale + translation − origin · scale
//! ```
//!
//! where `translation` is already in device pixels and `origin` is the current
//! target's scene-space origin (offscreen layers are allocated only as large
//! as their content). That is one affine map, so it becomes the canvas matrix
//! and Skia does the rest. Mesh vertices, clip rectangles and composite bounds
//! are scene-absolute logical pixels and use the same map with an identity
//! `matrix`.

use crate::color;
use crate::pool::LayerPool;
use crate::texture::TextureStore;
use skia_safe::{
    BlendMode as SkBlendMode, BlurStyle, Canvas, ClipOp, Color4f, ColorFilter, ColorSpace, IPoint,
    IRect, Image, M44, MaskFilter, Matrix, Paint, PathBuilder, PathFillType, Point, RRect, RSXform,
    Rect, SamplingOptions, Shader, Surface, TileMode, Vector,
    canvas::SrcRectConstraint,
    color_filters,
    gradient::{Colors, Gradient, Interpolation, interpolation, shaders},
    image_filters, vertices,
};
use spherekit_core::{BlendMode, Px};
use spherekit_render::{
    Batch, BatchKind, CompiledFrame, Composite, Filter, GlyphInstance, GpuClip, GpuGradient,
    GpuTransform, MAX_GRADIENT_STOPS, QuadInstance, RenderTarget, glyph_flags, gradient_kind,
    quad_flags,
};

/// Below this sigma a blur has nothing to integrate; the wgpu shadow shader
/// drew the sharp shape instead, and so does this.
const MIN_SHADOW_SIGMA: f32 = 0.02;

/// Counters for one frame, folded into `FrameStats`.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct DrawCounters {
    pub(crate) draw_calls: u32,
    pub(crate) pipeline_switches: u32,
}

/// Reusable per-frame state, kept across frames so a steady-state frame
/// allocates as little as possible.
pub(crate) struct Painter {
    srgb_linear: ColorSpace,
    /// One shader per entry in the frame's gradient table, built on first use.
    gradients: Vec<Option<Option<Shader>>>,
    /// Pool slot for each compiled target; `None` for the surface.
    slots: Vec<Option<usize>>,
    path: PathBuilder,
    /// Glyph sprites queued for one `draw_atlas` call.
    sprites: Vec<RSXform>,
    sprite_src: Vec<Rect>,
    sprite_colors: Vec<skia_safe::Color>,
    sampling: SamplingOptions,
    /// Whether tessellated paths are antialiased; see
    /// [`spherekit_render::SurfaceConfig::msaa_samples`].
    pub(crate) antialias_meshes: bool,
}

/// Everything a draw needs to read, bundled so helpers take one argument.
struct Ctx<'a> {
    frame: &'a CompiledFrame,
    textures: &'a TextureStore,
    scale: f32,
    /// The current target's origin in device pixels.
    origin: (f32, f32),
}

impl Ctx<'_> {
    /// The canvas matrix for an instance with transform `index`.
    fn instance_matrix(&self, index: u32) -> Matrix {
        let t =
            self.frame.transforms.get(index as usize).copied().unwrap_or(GpuTransform::IDENTITY);
        let s = self.scale;
        // `GpuTransform::matrix` is `[a, b, c, d]` with x' = a·x + c·y and
        // y' = b·x + d·y, as in `apply_transform` in `common/frame.wgsl`.
        Matrix::new_all(
            t.matrix[0] * s,
            t.matrix[2] * s,
            t.translation[0] - self.origin.0,
            t.matrix[1] * s,
            t.matrix[3] * s,
            t.translation[1] - self.origin.1,
            0.0,
            0.0,
            1.0,
        )
    }

    /// The canvas matrix for scene-absolute logical coordinates.
    fn scene_matrix(&self) -> Matrix {
        let s = self.scale;
        Matrix::new_all(s, 0.0, -self.origin.0, 0.0, s, -self.origin.1, 0.0, 0.0, 1.0)
    }

    /// The rounded clip an instance must respect, if it has one.
    ///
    /// Plain rectangular clips are already the batch scissor. A clip flagged
    /// rounded but with all-zero radii is a path clip the compiler could not
    /// express; the GPU path enforced only its scissor there, and so does this.
    fn rounded_clip(&self, index: u32) -> Option<RRect> {
        let c: &GpuClip = self.frame.clips.get(index as usize)?;
        if c.radii.iter().all(|r| *r <= 0.0) {
            return None;
        }
        let rect = Rect::new(c.bounds[0], c.bounds[1], c.bounds[2], c.bounds[3]);
        Some(rrect(rect, c.radii))
    }
}

/// A rounded rectangle from `[tl, tr, br, bl]` radii — the order of both
/// SphereKit's `Corners` and Skia's `RRect` corner array.
fn rrect(rect: Rect, radii: [f32; 4]) -> RRect {
    if radii.iter().all(|r| *r <= 0.0) {
        return RRect::new_rect(rect);
    }
    let v = |r: f32| Vector::new(r.max(0.0), r.max(0.0));
    RRect::new_rect_radii(rect, &[v(radii[0]), v(radii[1]), v(radii[2]), v(radii[3])])
}

fn xywh(b: [f32; 4]) -> Rect {
    Rect::from_xywh(b[0], b[1], b[2], b[3])
}

/// Maps an engine blend mode onto Skia's.
///
/// The GPU path composited every layer with premultiplied source-over whatever
/// its blend mode said; Skia implements all of them natively, so they are
/// honoured here.
pub(crate) fn blend_mode(b: BlendMode) -> SkBlendMode {
    match b {
        BlendMode::Normal => SkBlendMode::SrcOver,
        BlendMode::Add => SkBlendMode::Plus,
        BlendMode::Multiply => SkBlendMode::Multiply,
        BlendMode::Screen => SkBlendMode::Screen,
        BlendMode::Overlay => SkBlendMode::Overlay,
        BlendMode::Darken => SkBlendMode::Darken,
        BlendMode::Lighten => SkBlendMode::Lighten,
        BlendMode::Difference => SkBlendMode::Difference,
        BlendMode::Copy => SkBlendMode::Src,
    }
}

/// Converts a batch scissor into a clamped rectangle in its target's pixels.
///
/// A port of `clamp_scissor` from the wgpu backend: the compiler's scissors
/// are scene-absolute device pixels, and an offscreen target's own space is
/// shifted by its origin.
pub(crate) fn clamp_scissor(
    batch: &Batch,
    frame: &CompiledFrame,
    target_index: u32,
    target_w: i32,
    target_h: i32,
) -> IRect {
    let scale = frame.uniforms.scale_factor.max(1e-3);
    let (ox, oy) = frame
        .targets
        .get(target_index as usize)
        .map(|t| (t.origin.x.get() * scale, t.origin.y.get() * scale))
        .unwrap_or((0.0, 0.0));
    let s = batch.scissor;
    let x0 = (s.min_x().get() as f32 - ox).floor().clamp(0.0, target_w as f32) as i32;
    let y0 = (s.min_y().get() as f32 - oy).floor().clamp(0.0, target_h as f32) as i32;
    let x1 = (s.max_x().get() as f32 - ox).ceil().clamp(0.0, target_w as f32) as i32;
    let y1 = (s.max_y().get() as f32 - oy).ceil().clamp(0.0, target_h as f32) as i32;
    IRect::new(x0, y0, x1.max(x0), y1.max(y0))
}

/// Resolves a scene-space rectangle into a clamped region of a target, the
/// port of `target_region` from the wgpu backend.
fn target_region(
    bounds: spherekit_core::Rect<Px>,
    target: &RenderTarget,
    target_size: (i32, i32),
    scale: f32,
) -> Option<IRect> {
    let scale = scale.max(1e-3);
    let (ox, oy) = (target.origin.x.get(), target.origin.y.get());
    let (w, h) = (target_size.0 as f32, target_size.1 as f32);
    let x0 = ((bounds.min_x().get() - ox) * scale).floor().clamp(0.0, w) as i32;
    let y0 = ((bounds.min_y().get() - oy) * scale).floor().clamp(0.0, h) as i32;
    let x1 = ((bounds.max_x().get() - ox) * scale).ceil().clamp(0.0, w) as i32;
    let y1 = ((bounds.max_y().get() - oy) * scale).ceil().clamp(0.0, h) as i32;
    (x1 > x0 && y1 > y0).then(|| IRect::new(x0, y0, x1, y1))
}

/// A colour filter that blends toward luminance, applied to straight linear
/// colour like `composite.wgsl`.
fn saturate_filter(s: f32, linear: &ColorSpace) -> Option<ColorFilter> {
    // Rec. 709 luma weights, as in the shader.
    let (lr, lg, lb) = (0.2126, 0.7152, 0.0722);
    let i = 1.0 - s;
    #[rustfmt::skip]
    let m = [
        lr * i + s, lg * i,     lb * i,     0.0, 0.0,
        lr * i,     lg * i + s, lb * i,     0.0, 0.0,
        lr * i,     lg * i,     lb * i + s, 0.0, 0.0,
        0.0,        0.0,        0.0,        1.0, 0.0,
    ];
    color_filters::matrix_row_major(&m, None).with_working_color_space(linear.clone())
}

impl Painter {
    pub(crate) fn new() -> Self {
        Self {
            srgb_linear: ColorSpace::new_srgb_linear(),
            gradients: Vec::new(),
            slots: Vec::new(),
            path: PathBuilder::new(),
            sprites: Vec::new(),
            sprite_src: Vec::new(),
            sprite_colors: Vec::new(),
            // Bilinear, like the GPU path's samplers. Skia drops to nearest on
            // its own when a draw is an integer translation, which every
            // snapped glyph and every unscaled layer composite is.
            sampling: SamplingOptions::new(
                skia_safe::FilterMode::Linear,
                skia_safe::MipmapMode::None,
            ),
            antialias_meshes: true,
        }
    }

    /// Renders a whole frame.
    ///
    /// `main` is the surface target 0 resolves to; it has already been cleared.
    pub(crate) fn render(
        &mut self,
        main: &mut Surface,
        pool: &mut LayerPool,
        textures: &TextureStore,
        frame: &CompiledFrame,
        frame_index: u64,
    ) -> DrawCounters {
        let mut counters = DrawCounters::default();
        self.gradients.clear();
        self.gradients.resize(frame.gradients.len(), None);

        // Reserve every offscreen target before any pass runs, mirroring the
        // GPU backend, so a nested layer never allocates mid-pass.
        self.slots.clear();
        for t in &frame.targets {
            let slot = if t.is_surface {
                None
            } else {
                pool.acquire(t.size.width.get(), t.size.height.get(), frame_index)
            };
            self.slots.push(slot);
        }

        let scale = frame.uniforms.scale_factor.max(1e-3);
        for pass in &frame.passes {
            let Some(target) = frame.targets.get(pass.target as usize) else { continue };
            if !target.is_surface && self.slots[pass.target as usize].is_none() {
                continue;
            }
            // `Pass::clear` is not consulted: every target was cleared when it
            // was acquired, which is what the first pass on it would have done,
            // and ignoring the flag on later passes keeps a frame correct even
            // if a target is ever (wrongly) marked for a second clear after
            // something has already composited into it.
            let ctx = Ctx {
                frame,
                textures,
                scale,
                origin: (target.origin.x.get() * scale, target.origin.y.get() * scale),
            };
            {
                let surface = self.target_surface(main, pool, pass.target);
                let (tw, th) = (surface.width(), surface.height());
                let canvas = surface.canvas();
                let mut last_kind = None;
                for bi in pass.batches.clone() {
                    let Some(batch) = frame.batches.get(bi as usize) else { continue };
                    if batch.range.is_empty() {
                        continue;
                    }
                    let scissor = clamp_scissor(batch, frame, pass.target, tw, th);
                    if scissor.is_empty() {
                        continue;
                    }
                    let kind = core::mem::discriminant(&batch.kind);
                    if last_kind != Some(kind) {
                        counters.pipeline_switches += 1;
                        last_kind = Some(kind);
                    }
                    canvas.save();
                    canvas.clip_irect(scissor, ClipOp::Intersect);
                    counters.draw_calls += self.draw_batch(canvas, batch, &ctx);
                    canvas.restore();
                }
            }
            if let Some(c) = &pass.composite {
                counters.draw_calls += self.composite(main, pool, frame, c);
            }
        }
        counters
    }

    fn target_surface<'s>(
        &self,
        main: &'s mut Surface,
        pool: &'s mut LayerPool,
        target: u32,
    ) -> &'s mut Surface {
        match self.slots.get(target as usize).copied().flatten() {
            Some(slot) => pool.surface(slot),
            None => main,
        }
    }

    /// Draws one batch, returning how many Skia draw calls it took.
    fn draw_batch(&mut self, canvas: &Canvas, batch: &Batch, ctx: &Ctx<'_>) -> u32 {
        let range = batch.range.start as usize..batch.range.end as usize;
        match batch.kind {
            BatchKind::Quad => {
                let Some(quads) = ctx.frame.quads.get(range) else { return 0 };
                for q in quads {
                    self.draw_quad(canvas, q, None, ctx);
                }
                quads.len() as u32
            }
            BatchKind::Image { texture } => {
                // A texture that was destroyed or never uploaded draws nothing.
                // The GPU path fell back to a white texture, which shows the
                // tint as a solid box; nothing at all is the less alarming
                // failure.
                let Some(image) = ctx.textures.image(texture) else { return 0 };
                let Some(quads) = ctx.frame.quads.get(range) else { return 0 };
                for q in quads {
                    self.draw_quad(canvas, q, Some(image), ctx);
                }
                quads.len() as u32
            }
            BatchKind::Glyph { page, .. } => {
                // An unbound page is skipped, not an error; see
                // `RendererBackend::set_atlas_page`. Subpixel batches sample the
                // same grayscale mirror, whose texels hold the strongest of the
                // three channel coverages.
                let Some(atlas) = ctx.textures.atlas_coverage(page) else { return 0 };
                let Some(glyphs) = ctx.frame.glyphs.get(range) else { return 0 };
                self.draw_glyphs(canvas, glyphs, atlas, ctx)
            }
            BatchKind::Mesh { texture } => {
                let image = match texture {
                    Some(t) => match ctx.textures.image(t) {
                        Some(i) => Some(i),
                        None => return 0,
                    },
                    None => None,
                };
                self.draw_mesh(canvas, batch, image, ctx)
            }
        }
    }

    // ------------------------------------------------------------- quads

    /// One `QuadInstance`, with the semantics of `quad.wgsl`.
    fn draw_quad(
        &mut self,
        canvas: &Canvas,
        q: &QuadInstance,
        image: Option<&Image>,
        ctx: &Ctx<'_>,
    ) {
        let flags = q.flags;
        // Only a rounded clip needs its own save: the matrix set below lives
        // inside the batch's save and is simply overwritten by the next quad.
        let rounded = if flags & quad_flags::CLIP_ROUNDED != 0 {
            ctx.rounded_clip(q.clip_index)
        } else {
            None
        };
        let restore_to = rounded.map(|clip| {
            let n = canvas.save();
            canvas.set_matrix(&M44::from(ctx.scene_matrix()));
            canvas.clip_rrect(clip, ClipOp::Intersect, true);
            n
        });
        canvas.set_matrix(&M44::from(ctx.instance_matrix(q.transform_index)));

        let bounds = xywh(q.bounds);
        let shape = rrect(bounds, q.radii);

        if flags & quad_flags::SHADOW != 0 {
            self.draw_shadow(canvas, q, bounds, &shape);
            if let Some(n) = restore_to {
                canvas.restore_to_count(n);
            }
            return;
        }

        // What fills the interior. Textures are drawn as images clipped to the
        // shape rather than as an image shader on the rrect, so the common
        // unrounded case is a plain `draw_image_rect` blit.
        let fill = if flags & quad_flags::FILL_GRADIENT != 0 {
            self.gradient_shader(q.gradient, ctx).map(Fill::Shader)
        } else if flags & quad_flags::FILL_TEXTURE != 0 {
            image.map(Fill::Image)
        } else if flags & quad_flags::FILL_SOLID != 0 {
            Some(Fill::Color(color::to_srgb(q.color)))
        } else {
            None
        };

        if flags & quad_flags::BORDER != 0 {
            // The fill covers only the inner shape and the border the ring
            // between it and the outer edge. Drawing the fill first and the
            // ring over it reproduces the shader's
            // `border·ring + fill·inner·(1 − border.a·ring)` exactly, including
            // the antialiased seam where the two meet.
            let w = q.border_width.max(0.0);
            let inner_rect =
                Rect::new(bounds.left + w, bounds.top + w, bounds.right - w, bounds.bottom - w);
            let inner_radii = q.radii.map(|r| (r - w).max(0.0));
            let border = color::to_srgb(q.border_color);
            let mut paint = Paint::new(border, None);
            paint.set_anti_alias(true);
            if inner_rect.width() > 0.0 && inner_rect.height() > 0.0 {
                let inner = rrect(inner_rect, inner_radii);
                if let Some(fill) = fill {
                    self.fill_shape(canvas, &inner, bounds, fill, q);
                }
                canvas.draw_drrect(shape, inner, &paint);
            } else {
                canvas.draw_rrect(shape, &paint);
            }
        } else if let Some(fill) = fill {
            self.fill_shape(canvas, &shape, bounds, fill, q);
        }
        if let Some(n) = restore_to {
            canvas.restore_to_count(n);
        }
    }

    fn fill_shape(
        &mut self,
        canvas: &Canvas,
        shape: &RRect,
        bounds: Rect,
        fill: Fill<'_>,
        q: &QuadInstance,
    ) {
        match fill {
            Fill::Color(c) => {
                let mut paint = Paint::new(c, None);
                paint.set_anti_alias(true);
                draw_shape(canvas, shape, &paint);
            }
            Fill::Shader(shader) => {
                let mut paint = Paint::default();
                paint.set_anti_alias(true);
                paint.set_shader(shader);
                draw_shape(canvas, shape, &paint);
            }
            Fill::Image(image) => {
                // The source rectangle rides in `border_color` as normalised
                // `[x, y, w, h]`, and the tint in `color`.
                let (iw, ih) = (image.width() as f32, image.height() as f32);
                let s = q.border_color;
                let src = Rect::from_xywh(s[0] * iw, s[1] * ih, s[2] * iw, s[3] * ih);
                let mut paint = Paint::default();
                paint.set_anti_alias(true);
                apply_tint(&mut paint, q.color, &self.srgb_linear);
                let clipped = !shape.is_rect();
                if clipped {
                    canvas.save();
                    canvas.clip_rrect(shape, ClipOp::Intersect, true);
                }
                canvas.draw_image_rect_with_sampling_options(
                    image,
                    Some((&src, SrcRectConstraint::Fast)),
                    bounds,
                    self.sampling,
                    &paint,
                );
                if clipped {
                    canvas.restore();
                }
            }
        }
    }

    /// Box shadows, outer and inset.
    ///
    /// `common/shadow.wgsl` integrates a Gaussian-blurred rounded box
    /// analytically. Skia's blur mask filter computes the same convolution
    /// (with its own fast paths for exactly this shape), and `respect_ctm`
    /// scales sigma by the transform the way the shader's logical-space sigma
    /// was scaled by rasterising in device space.
    fn draw_shadow(&mut self, canvas: &Canvas, q: &QuadInstance, bounds: Rect, shape: &RRect) {
        let mut paint = Paint::new(color::to_srgb(q.color), None);
        paint.set_anti_alias(true);
        let sigma = q.blur_sigma;
        if sigma >= MIN_SHADOW_SIGMA {
            paint.set_mask_filter(MaskFilter::blur(BlurStyle::Normal, sigma, true));
        }
        if q.flags & quad_flags::SHADOW_INSET == 0 {
            draw_shape(canvas, shape, &paint);
            return;
        }
        // Inset: `(1 − blur(shape)) · inside(shape)`. The complement of the
        // shape, out to three sigma, blurred and clipped to the shape is that
        // expression: blurring the ring between a distant rectangle and the
        // shape gives `1 − blur(shape)` everywhere the distant edge's own blur
        // has decayed to nothing.
        let reach = sigma * 3.0 + 2.0;
        let outer = RRect::new_rect(Rect::new(
            bounds.left - reach,
            bounds.top - reach,
            bounds.right + reach,
            bounds.bottom + reach,
        ));
        canvas.save();
        canvas.clip_rrect(shape, ClipOp::Intersect, true);
        if bounds.width() > 0.0 && bounds.height() > 0.0 {
            canvas.draw_drrect(outer, shape, &paint);
        } else {
            canvas.draw_rrect(outer, &paint);
        }
        canvas.restore();
    }

    /// The Skia shader for gradient-table entry `index`, built once per frame.
    ///
    /// Geometry is in the quad's local space, which is the canvas space while
    /// the instance matrix is set, so the shader needs no matrix of its own
    /// beyond what radial and sweep need to express their parameters.
    fn gradient_shader(&mut self, index: u32, ctx: &Ctx<'_>) -> Option<Shader> {
        let slot = self.gradients.get_mut(index as usize)?;
        if let Some(cached) = slot {
            return cached.clone();
        }
        let built = build_gradient(&ctx.frame.gradients[index as usize], &self.srgb_linear);
        *slot = Some(built.clone());
        built
    }

    // ------------------------------------------------------------- glyphs

    /// Glyph quads from one atlas page, with the semantics of the bitmap
    /// branch of `text.wgsl`: coverage times colour. `coverage_contrast` is
    /// deliberately ignored; see the `color` module.
    ///
    /// The common case — a glyph under a translation or uniform scale with no
    /// rounded clip — goes into one `draw_atlas` call per run of such glyphs.
    /// Skia builds the raster pipeline for an atlas draw once and only updates
    /// each sprite's position and colour, where a `draw_image_rect` per glyph
    /// rebuilds it every time; on a text-heavy frame that roughly halves the
    /// cost of text. Each sprite's colour is the destination of a `DstIn`
    /// blend with the `Alpha8` page, i.e. `colour · coverage`.
    ///
    /// Anything else (rotation, skew, a rounded clip) is drawn one glyph at a
    /// time with `draw_image_rect`, which tints an `Alpha8` image by the paint
    /// colour.
    fn draw_glyphs(
        &mut self,
        canvas: &Canvas,
        glyphs: &[GlyphInstance],
        atlas: &Image,
        ctx: &Ctx<'_>,
    ) -> u32 {
        let (aw, ah) = (atlas.width() as f32, atlas.height() as f32);
        let mut calls = 0;
        self.sprites.clear();
        self.sprite_src.clear();
        self.sprite_colors.clear();
        // Whether every queued sprite sits 1:1 on whole pixels, which lets the
        // atlas draw use nearest sampling: exact, and cheaper than bilinear.
        let mut crisp = true;

        let mut paint = Paint::default();
        paint.set_anti_alias(true);
        let mut last_color = [f32::NAN; 4];
        let mut color8 = skia_safe::Color::TRANSPARENT;
        let mut last_transform = u32::MAX;
        let mut m = Matrix::default();

        for g in glyphs {
            if g.transform_index != last_transform {
                m = ctx.instance_matrix(g.transform_index);
                last_transform = g.transform_index;
            }
            if g.color != last_color {
                let c = color::to_srgb(g.color);
                paint.set_color4f(c, None);
                color8 = c.to_color();
                last_color = g.color;
            }
            let src = Rect::new(g.uv[0] * aw, g.uv[1] * ah, g.uv[2] * aw, g.uv[3] * ah);
            let rounded = if g.flags & glyph_flags::CLIP_ROUNDED != 0 {
                ctx.rounded_clip(g.clip_index)
            } else {
                None
            };

            let (sx, sy) = (m.scale_x(), m.scale_y());
            let k = g.bounds[2] * sx / src.width().max(1e-6);
            let simple = rounded.is_none()
                && m.skew_x() == 0.0
                && m.skew_y() == 0.0
                && k > 0.0
                && (g.bounds[3] * sy / src.height().max(1e-6) - k).abs() <= 1e-3 * k;
            if simple {
                let mut tx = g.bounds[0] * sx + m.translate_x();
                let mut ty = g.bounds[1] * sy + m.translate_y();
                let on_grid = (k - 1.0).abs() < 1e-4
                    && (tx - tx.round()).abs() < 1e-3
                    && (ty - ty.round()).abs() < 1e-3;
                if on_grid {
                    (tx, ty) = (tx.round(), ty.round());
                }
                crisp &= on_grid;
                self.sprites.push(RSXform::new(k, 0.0, (tx, ty)));
                self.sprite_src.push(src);
                self.sprite_colors.push(color8);
                continue;
            }

            calls += self.flush_sprites(canvas, atlas, crisp);
            crisp = true;
            canvas.save();
            if let Some(clip) = &rounded {
                canvas.set_matrix(&M44::from(ctx.scene_matrix()));
                canvas.clip_rrect(clip, ClipOp::Intersect, true);
            }
            canvas.set_matrix(&M44::from(m));
            canvas.draw_image_rect_with_sampling_options(
                atlas,
                Some((&src, SrcRectConstraint::Fast)),
                xywh(g.bounds),
                self.sampling,
                &paint,
            );
            canvas.restore();
            calls += 1;
        }
        calls + self.flush_sprites(canvas, atlas, crisp)
    }

    /// Draws the queued glyph sprites in one atlas call.
    fn flush_sprites(&mut self, canvas: &Canvas, atlas: &Image, crisp: bool) -> u32 {
        if self.sprites.is_empty() {
            return 0;
        }
        let sampling = if crisp {
            SamplingOptions::new(skia_safe::FilterMode::Nearest, skia_safe::MipmapMode::None)
        } else {
            self.sampling
        };
        canvas.save();
        // Sprite transforms are already in the target's device pixels.
        canvas.reset_matrix();
        canvas.draw_atlas(
            atlas,
            &self.sprites,
            &self.sprite_src,
            &self.sprite_colors[..],
            SkBlendMode::DstIn,
            sampling,
            None,
            None,
        );
        canvas.restore();
        self.sprites.clear();
        self.sprite_src.clear();
        self.sprite_colors.clear();
        1
    }

    // ------------------------------------------------------------- meshes

    /// Indexed triangles with baked transforms, as in `mesh.wgsl`.
    ///
    /// A GPU rasterises triangles with hard edges and relies on MSAA to smooth
    /// tessellated paths. Skia's `draw_vertices` cannot antialias at all, so a
    /// single-colour, untextured batch — which is everything the tessellator
    /// produces for `fill_path` and `stroke_path` — is instead filled as one
    /// path made of its triangles. Skia's scan converter computes coverage for
    /// the path as a whole, so the shared interior edges cancel and only the
    /// true outline is antialiased: no seams, and no MSAA buffer. Batches with
    /// per-vertex colour or a texture go through `draw_vertices` and stay
    /// aliased.
    fn draw_mesh(
        &mut self,
        canvas: &Canvas,
        batch: &Batch,
        image: Option<&Image>,
        ctx: &Ctx<'_>,
    ) -> u32 {
        let frame = ctx.frame;
        let Some(indices) =
            frame.mesh_indices.get(batch.range.start as usize..batch.range.end as usize)
        else {
            return 0;
        };
        let base = batch.base_vertex as usize;
        let vertices = &frame.mesh_vertices;
        let vertex = |i: u32| vertices.get(base + i as usize);
        let triangles = indices.len() / 3;
        if triangles == 0 {
            return 0;
        }

        canvas.set_matrix(&M44::from(ctx.scene_matrix()));

        // Uniform colour? Then it is a tessellated path and can be filled as one.
        let first = indices.iter().find_map(|&i| vertex(i)).map(|v| v.color);
        let uniform = image.is_none()
            && first
                .is_some_and(|c| indices.iter().all(|&i| vertex(i).is_none_or(|v| v.color == c)));
        if uniform && let Some(c) = first {
            self.path.reset();
            for tri in indices.chunks_exact(3) {
                let (Some(a), Some(b), Some(d)) = (vertex(tri[0]), vertex(tri[1]), vertex(tri[2]))
                else {
                    continue;
                };
                let (p0, mut p1, mut p2) = (a.position, b.position, d.position);
                // Every triangle is wound the same way, so under the non-zero
                // rule the path is exactly the union of the triangles. The
                // stroke tessellator can emit overlapping triangles of either
                // winding at joins, and mixed windings would cancel there.
                let cross = (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p1[1] - p0[1]) * (p2[0] - p0[0]);
                if cross == 0.0 {
                    continue;
                }
                if cross < 0.0 {
                    core::mem::swap(&mut p1, &mut p2);
                }
                self.path.move_to((p0[0], p0[1]));
                self.path.line_to((p1[0], p1[1]));
                self.path.line_to((p2[0], p2[1]));
                self.path.close();
            }
            self.path.set_fill_type(PathFillType::Winding);
            let path = self.path.detach();
            let mut paint = Paint::new(color::to_srgb(c), None);
            paint.set_anti_alias(self.antialias_meshes);
            canvas.draw_path(&path, &paint);
            return 1;
        }

        // General case. Skia's vertices take 16-bit indices, so a batch whose
        // indices span more than that is expanded to plain triangles.
        let (lo, hi) = indices.iter().fold((u32::MAX, 0u32), |(lo, hi), &i| (lo.min(i), hi.max(i)));
        let span = (hi - lo) as usize + 1;
        let tex_scale = image.map(|i| (i.width() as f32, i.height() as f32));
        let mut flags = vertices::BuilderFlags::HAS_COLORS;
        if tex_scale.is_some() {
            flags |= vertices::BuilderFlags::HAS_TEX_COORDS;
        }
        let indexed = span <= u16::MAX as usize + 1 && base + (hi as usize) < vertices.len();
        let builder = if indexed {
            let mut b =
                vertices::Builder::new(vertices::VertexMode::Triangles, span, triangles * 3, flags);
            let src = &vertices[base + lo as usize..=base + hi as usize];
            fill_vertex_attributes(&mut b, src.iter(), tex_scale);
            if let Some(out) = b.indices() {
                for (o, &i) in out.iter_mut().zip(indices) {
                    *o = (i - lo) as u16;
                }
            }
            b
        } else {
            let n = triangles * 3;
            let mut b = vertices::Builder::new(vertices::VertexMode::Triangles, n, 0, flags);
            let fallback = spherekit_render::MeshVertex::default();
            let src = indices[..n].iter().map(|&i| vertex(i).unwrap_or(&fallback));
            fill_vertex_attributes(&mut b, src, tex_scale);
            b
        };
        let built = builder.detach();
        let mut paint = Paint::default();
        let mode = match image {
            Some(image) => {
                // `mesh.wgsl` returns `texel · colour`; `Modulate` is exactly
                // that product of the shader (source) and the vertex colour.
                paint.set_shader(image.to_shader(
                    (TileMode::Clamp, TileMode::Clamp),
                    self.sampling,
                    None,
                ));
                SkBlendMode::Modulate
            }
            // With no shader, `Dst` keeps the vertex colours untouched.
            None => SkBlendMode::Dst,
        };
        canvas.draw_vertices(&built, mode, &paint);
        1
    }

    // ------------------------------------------------------------- layers

    /// Composites a finished layer into its destination, as the composite
    /// pass of the GPU backend did, plus the blend modes and colour matrix it
    /// left out.
    fn composite(
        &mut self,
        main: &mut Surface,
        pool: &mut LayerPool,
        frame: &CompiledFrame,
        c: &Composite,
    ) -> u32 {
        let Some(source_slot) = self.slots.get(c.source as usize).copied().flatten() else {
            return 0;
        };
        let Some(dest) = frame.targets.get(c.destination as usize) else { return 0 };
        let scale = frame.uniforms.scale_factor.max(1e-3);
        // The compiler snapped the destination rect to whole device pixels and
        // sized the texture from it, so this is a one-texel-to-one-pixel blit.
        let at = IPoint::new(
            ((c.bounds.min_x().get() - dest.origin.x.get()) * scale).round() as i32,
            ((c.bounds.min_y().get() - dest.origin.y.get()) * scale).round() as i32,
        );
        let layer = pool.surface(source_slot).image_snapshot();
        let layer_rect = IRect::from_xywh(at.x, at.y, layer.width(), layer.height());
        let opacity = c.opacity.clamp(0.0, 1.0);
        let mut calls = 1;

        let dest_surface = self.target_surface(main, pool, c.destination);
        if let Some(Filter::BackdropBlur { sigma }) = c.filter {
            // Blur what is already in the destination behind the layer, draw
            // that back, then the layer over it. Clamp tiling and a crop to the
            // region keep the edges from darkening, like the GPU path's
            // clamp-to-edge sampler over the copied region did.
            let size = (dest_surface.width(), dest_surface.height());
            let sigma_px = sigma.get().max(0.0) * scale;
            if sigma_px > 0.0
                && let Some(region) = target_region(c.bounds, dest, size, scale)
                && let Some(backdrop) = dest_surface.image_snapshot_with_bounds(region)
            {
                let mut paint = Paint::default();
                paint.set_alpha_f(opacity);
                paint.set_image_filter(image_filters::blur(
                    (sigma_px, sigma_px),
                    TileMode::Clamp,
                    None,
                    region,
                ));
                let canvas = dest_surface.canvas();
                canvas.save();
                canvas.reset_matrix();
                canvas.draw_image(&backdrop, (region.left as f32, region.top as f32), Some(&paint));
                canvas.restore();
                calls += 1;
            }
        }

        let mut paint = Paint::default();
        paint.set_alpha_f(opacity);
        paint.set_blend_mode(blend_mode(c.blend));
        match c.filter {
            Some(Filter::Blur { sigma }) => {
                let sigma_px = sigma.get().max(0.0) * scale;
                if sigma_px > 0.0 {
                    // Decal tiling fades to transparent past the layer's edge,
                    // and the crop confines the result to the layer's own
                    // rectangle, as the GPU path's blur targets did.
                    paint.set_image_filter(image_filters::blur(
                        (sigma_px, sigma_px),
                        TileMode::Decal,
                        None,
                        layer_rect,
                    ));
                }
            }
            Some(Filter::Saturate(s)) => {
                if (s - 1.0).abs() > 1e-4 {
                    paint.set_color_filter(saturate_filter(s, &self.srgb_linear));
                }
            }
            Some(Filter::ColorMatrix(m)) => {
                // Row-major 4×5 on straight colour, in linear light like the
                // saturate filter, so the two agree on what a pure luminance
                // matrix does. The GPU backend ignored this filter entirely.
                paint.set_color_filter(
                    color_filters::matrix_row_major(&m, None)
                        .with_working_color_space(self.srgb_linear.clone()),
                );
            }
            Some(Filter::BackdropBlur { .. }) | None => {}
        }
        let canvas = dest_surface.canvas();
        canvas.save();
        canvas.reset_matrix();
        canvas.draw_image(&layer, (at.x as f32, at.y as f32), Some(&paint));
        canvas.restore();
        calls
    }
}

enum Fill<'a> {
    Color(Color4f),
    Shader(Shader),
    Image(&'a Image),
}

/// Draws a shape with the cheapest call that describes it.
fn draw_shape(canvas: &Canvas, shape: &RRect, paint: &Paint) {
    if shape.is_rect() {
        canvas.draw_rect(shape.rect(), paint);
    } else {
        canvas.draw_rrect(shape, paint);
    }
}

/// Applies an image tint: `texel · tint`, both premultiplied, as in the
/// textured branch of `quad.wgsl`.
fn apply_tint(paint: &mut Paint, tint: [f32; 4], linear: &ColorSpace) {
    let a = tint[3].clamp(0.0, 1.0);
    // A grey tint at full strength (white, possibly faded) is just an alpha,
    // which the paint applies for free. Only a coloured tint needs a filter.
    let is_alpha_only =
        (tint[0] - a).abs() < 1e-4 && (tint[1] - a).abs() < 1e-4 && (tint[2] - a).abs() < 1e-4;
    if is_alpha_only {
        paint.set_alpha_f(a);
        return;
    }
    // Modulate in linear light, where the GPU path multiplied.
    if let Some(filter) = color_filters::blend_with_color_space(
        color::unpremultiply(tint),
        linear.clone(),
        SkBlendMode::Modulate,
    ) {
        paint.set_color_filter(filter.with_working_color_space(linear.clone()));
    }
}

fn fill_vertex_attributes<'v>(
    b: &mut vertices::Builder,
    src: impl Iterator<Item = &'v spherekit_render::MeshVertex> + Clone,
    tex_scale: Option<(f32, f32)>,
) {
    for (p, v) in b.positions().iter_mut().zip(src.clone()) {
        *p = Point::new(v.position[0], v.position[1]);
    }
    if let Some(colors) = b.colors() {
        for (c, v) in colors.iter_mut().zip(src.clone()) {
            *c = color::to_srgb8(v.color);
        }
    }
    if let Some((w, h)) = tex_scale
        && let Some(texs) = b.tex_coords()
    {
        // Image shaders sample in texel units, not normalised ones.
        for (t, v) in texs.iter_mut().zip(src) {
            *t = Point::new(v.uv[0] * w, v.uv[1] * h);
        }
    }
}

/// Builds the Skia shader for one gradient, with the semantics of
/// `gradient_position` and `sample_gradient` in `common/frame.wgsl`.
///
/// Stops stay in linear light and interpolate premultiplied there, as the
/// shader's `mix` over linear premultiplied stops did; Skia converts to the
/// sRGB surface after interpolating. Clamp tiling matches the shader clamping
/// `t` to `0..=1`.
fn build_gradient(g: &GpuGradient, linear: &ColorSpace) -> Option<Shader> {
    let n = (g.stop_count as usize).min(MAX_GRADIENT_STOPS);
    if n == 0 {
        return None;
    }
    let mut colors = [Color4f::new(0.0, 0.0, 0.0, 0.0); MAX_GRADIENT_STOPS];
    let mut offsets = [0.0f32; MAX_GRADIENT_STOPS];
    for i in 0..n {
        colors[i] = color::unpremultiply(g.colors[i]);
        // Skia wants non-decreasing offsets; the compiler sorted them already,
        // this only guards against a malformed table.
        let prev = if i == 0 { 0.0 } else { offsets[i - 1] };
        offsets[i] = g.offsets[i].clamp(0.0, 1.0).max(prev);
    }
    if n == 1 {
        return Some(shaders_color(colors[0], linear));
    }
    let gradient = Gradient::new(
        Colors::new(&colors[..n], Some(&offsets[..n]), TileMode::Clamp, linear.clone()),
        Interpolation {
            in_premul: interpolation::InPremul::Yes,
            color_space: interpolation::ColorSpace::SRGBLinear,
            hue_method: interpolation::HueMethod::Shorter,
        },
    );
    let [x, y, z, w] = g.geometry;
    match g.kind {
        gradient_kind::RADIAL => {
            // Separate x and y radii: a unit circle at the origin, stretched
            // and moved by the local matrix.
            let m = Matrix::new_all(z.max(1e-4), 0.0, x, 0.0, w.max(1e-4), y, 0.0, 0.0, 1.0);
            shaders::radial_gradient((Point::new(0.0, 0.0), 1.0), &gradient, &m)
        }
        gradient_kind::SWEEP => {
            // `[cx, cy, start_turns, sweep_turns]`. The shader measured the
            // angle from `start` with `fract`, i.e. in `[0, 1)` turns past it,
            // then divided by the span. Rotating the shader by `start` and
            // sweeping `0..span` turns is the same mapping. A zero span meant a
            // full turn there; a negative one clamped everything to the first
            // stop.
            let span = if z.is_finite() && w.abs() >= 1e-6 { w } else { 1.0 };
            if span < 0.0 {
                return Some(shaders_color(colors[0], linear));
            }
            let mut m = Matrix::translate((x, y));
            m.pre_rotate(z.rem_euclid(1.0) * 360.0, None);
            shaders::sweep_gradient(Point::new(0.0, 0.0), (0.0, span * 360.0), &gradient, &m)
        }
        _ => {
            let (p0, p1) = (Point::new(x, y), Point::new(z, w));
            if (p1 - p0).length() < 1e-6 {
                // A degenerate line samples `t = 0` everywhere.
                return Some(shaders_color(colors[0], linear));
            }
            shaders::linear_gradient((p0, p1), &gradient, None)
        }
    }
}

fn shaders_color(c: Color4f, linear: &ColorSpace) -> Shader {
    skia_safe::shaders::color_in_space(c, linear.clone())
}
