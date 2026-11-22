// Shared declarations for every SphereKit Direct3D 11 shader.
//
// HLSL has `#include`, but resolving it needs an `ID3DInclude` and a search
// path, and the shaders ship inside the binary. So the Rust side prepends this
// file to each shader body before compiling (see `src/shader.rs`), followed by
// a `#line` directive so diagnostics still name the right file and line.
//
// This is a straight port of `common/math.wgsl`, `common/frame.wgsl` and
// `common/shadow.wgsl` from the wgpu backend. The arithmetic is deliberately
// kept expression-for-expression identical: both backends must produce the
// same pixels, and the easiest way to keep that true is to never "improve"
// one of them.

// ---------------------------------------------------------------- math

static const float TAU = 6.283185307179586;

/// Signed distance to an axis-aligned rounded box.
///
/// `p` is relative to the box centre, `b` is the half-extent, and `r` holds the
/// four radii in `[top_left, top_right, bottom_right, bottom_left]` order, the
/// same order SphereKit's `Corners<T>` uses on the CPU.
///
/// Negative inside, positive outside, with a gradient of unit length almost
/// everywhere, which is what makes a single `fwidth` a correct antialiasing
/// width regardless of the shape's scale.
float sd_rounded_box(float2 p, float2 b, float4 r)
{
    float radius;
    if (p.y < 0.0) {
        radius = (p.x < 0.0) ? r.x : r.y;
    } else {
        radius = (p.x < 0.0) ? r.w : r.z;
    }
    radius = min(radius, min(b.x, b.y));
    float2 q = abs(p) - b + float2(radius, radius);
    return min(max(q.x, q.y), 0.0) + length(max(q, float2(0.0, 0.0))) - radius;
}

/// Converts a signed distance into coverage using screen-space derivatives.
///
/// `fwidth` measures how much the distance changes across one actual pixel, so
/// the antialiasing band is always one pixel wide no matter the transform.
float coverage_from_distance(float d)
{
    float w = max(fwidth(d), 1e-5);
    return clamp(0.5 - d / w, 0.0, 1.0);
}

/// Abramowitz and Stegun 7.1.26 error-function approximation, accurate to
/// about 5e-4: far below one 8-bit step, and what makes an analytic Gaussian
/// box shadow possible without a blur pass.
float2 erf2(float2 x)
{
    // HLSL's `sign` returns an integer vector; the float conversion is explicit
    // so the arithmetic below stays in floating point.
    float2 s = float2(sign(x));
    float2 a = abs(x);
    float2 r = 1.0 + (0.278393 + (0.230389 + 0.078108 * a * a) * a) * a;
    r = r * r;
    r = r * r;
    return s - s / r;
}

// --------------------------------------------------------------- frame

/// Per-pass uniforms. Mirrors `PassUniforms` in `src/renderer.rs`.
cbuffer PassUniforms : register(b0)
{
    /// Render target size in device pixels.
    float2 frame_viewport;
    /// The surface's scale factor.
    float frame_scale_factor;
    /// Seconds since start, for animated effects.
    float frame_time;
    /// The current target's origin in scene-absolute logical pixels.
    ///
    /// Offscreen layers are allocated only as large as their content, so this
    /// is subtracted to bring scene coordinates into target space. On the
    /// surface it is zero.
    float2 frame_target_origin;
    float2 frame_pad;
};

/// Mirrors `spherekit_render::GpuTransform`. (`matrix` is an HLSL keyword.)
struct Transform
{
    /// The linear part, `[a, b, c, d]`.
    float4 linear_part;
    /// `[tx, ty, 0, 0]`, already scaled to device pixels.
    float4 translation;
};

/// Mirrors `spherekit_render::GpuClip`.
struct Clip
{
    /// `[min_x, min_y, max_x, max_y]` in logical pixels.
    float4 bounds;
    /// Corner radii in `[tl, tr, br, bl]` order; all zero for a plain rect.
    float4 radii;
};

#define MAX_GRADIENT_STOPS 8u

/// Mirrors `spherekit_render::GpuGradient`, 192 bytes.
///
/// Structured buffers use tight packing, unlike constant buffers, so the
/// `float[8]` really is 32 contiguous bytes here.
struct Gradient
{
    /// Linear: `[x0, y0, x1, y1]`. Radial: `[cx, cy, rx, ry]`.
    /// Sweep: `[cx, cy, start_turns, sweep_turns]`.
    float4 geometry;
    /// Stop offsets, tail-padded with the final offset.
    float offsets[8];
    /// Stop colors, linear premultiplied.
    float4 colors[8];
    /// 0 linear, 1 radial, 2 sweep.
    uint kind;
    /// How many entries are real.
    uint stop_count;
    uint2 pad;
};

StructuredBuffer<Transform> transforms : register(t0);
StructuredBuffer<Clip> clips : register(t1);
StructuredBuffer<Gradient> gradients : register(t2);

/// Applies an indexed transform to a point in logical space, then converts the
/// result to device pixels relative to the current target.
float2 apply_transform(uint index, float2 p)
{
    Transform t = transforms[index];
    float2 world = float2(
        t.linear_part.x * p.x + t.linear_part.z * p.y,
        t.linear_part.y * p.x + t.linear_part.w * p.y) * frame_scale_factor + t.translation.xy;
    return world - frame_target_origin * frame_scale_factor;
}

/// Applies an indexed transform to a point in logical space, staying in
/// scene-absolute *logical* pixels: the space `GpuClip` bounds live in.
///
/// The translation arrives with the scale factor already folded in (see
/// `GpuTransform::from_affine`), so it is divided back out here.
float2 scene_position(uint index, float2 p)
{
    Transform t = transforms[index];
    return float2(
        t.linear_part.x * p.x + t.linear_part.z * p.y,
        t.linear_part.y * p.x + t.linear_part.w * p.y)
        + t.translation.xy / max(frame_scale_factor, 1e-3);
}

/// Converts a device-pixel position into clip space.
///
/// SphereKit's y axis points down and Direct3D's clip space points up, so y is
/// flipped exactly here and nowhere else.
float4 to_clip_space(float2 device_px)
{
    float2 ndc = float2(
        device_px.x / max(frame_viewport.x, 1.0) * 2.0 - 1.0,
        1.0 - device_px.y / max(frame_viewport.y, 1.0) * 2.0);
    return float4(ndc, 0.0, 1.0);
}

/// Coverage contributed by a clip region, evaluated in logical space.
///
/// A plain rectangular clip is already enforced by the scissor rect, so this
/// returns 1.0 for it and only does real work when the clip is rounded.
float clip_coverage(uint index, float2 local_logical)
{
    Clip c = clips[index];
    if (c.radii.x <= 0.0 && c.radii.y <= 0.0 && c.radii.z <= 0.0 && c.radii.w <= 0.0) {
        return 1.0;
    }
    float2 center = (c.bounds.xy + c.bounds.zw) * 0.5;
    float2 half_extent = max((c.bounds.zw - c.bounds.xy) * 0.5, float2(0.0, 0.0));
    float d = sd_rounded_box(local_logical - center, half_extent, c.radii);
    return coverage_from_distance(d);
}

/// Where along a gradient's ramp a point falls, in `0..=1`.
float gradient_position(Gradient g, float2 local)
{
    if (g.kind == 0u) {
        float2 d = g.geometry.zw - g.geometry.xy;
        float len_sq = dot(d, d);
        if (len_sq < 1e-9) {
            return 0.0;
        }
        return clamp(dot(local - g.geometry.xy, d) / len_sq, 0.0, 1.0);
    } else if (g.kind == 1u) {
        float2 d = (local - g.geometry.xy) / max(g.geometry.zw, float2(1e-4, 1e-4));
        return clamp(length(d), 0.0, 1.0);
    }
    float2 d = local - g.geometry.xy;
    float angle = atan2(d.y, d.x) / TAU;
    float span = (abs(g.geometry.w) < 1e-6) ? 1.0 : g.geometry.w;
    return clamp(frac(angle - g.geometry.z) / span, 0.0, 1.0);
}

/// Samples a gradient's ramp, returning a linear premultiplied color.
///
/// Stops are already sorted and tail-padded by the CPU, so this is a fixed
/// eight-iteration loop with no sorting.
float4 sample_gradient(uint index, float2 local)
{
    Gradient g = gradients[index];
    if (g.stop_count == 0u) {
        return float4(0.0, 0.0, 0.0, 0.0);
    }
    float t = gradient_position(g, local);
    float4 result = g.colors[0];
    [unroll]
    for (uint i = 1u; i < MAX_GRADIENT_STOPS; i = i + 1u) {
        if (i >= g.stop_count) {
            break;
        }
        float a = g.offsets[i - 1u];
        float b = g.offsets[i];
        float span = b - a;
        // Coincident stops are a legal way to author a hard colour break.
        float local_t = (span > 1e-6) ? clamp((t - a) / span, 0.0, 1.0) : 0.0;
        float step_taken = (t >= a) ? 1.0 : 0.0;
        result = lerp(result, lerp(g.colors[i - 1u], g.colors[i], local_t), step_taken);
    }
    return result;
}

// -------------------------------------------------------------- shadow
//
// The coverage of a rounded rectangle convolved with a two-dimensional
// Gaussian: an exact `erf` closed form per horizontal slice, integrated over y
// with a midpoint sum restricted to where both the kernel and the box exist.
// See `common/shadow.wgsl` in the wgpu backend for the full derivation and for
// why the sum is *not* renormalised by the weights it happened to sample.

/// How many slices the vertical integral is taken in.
#define SHADOW_SLICES 8

/// `sqrt(2 * pi)`, the Gaussian's normalising constant.
static const float SQRT_TAU = 2.5066282746310002;

/// The normalised one-dimensional Gaussian.
float shadow_gaussian(float x, float sigma)
{
    return exp(-0.5 * (x * x) / (sigma * sigma)) / (SQRT_TAU * sigma);
}

/// Coverage of one horizontal slice of a rounded box, blurred in x only.
float shadow_slice(float px, float y, float2 half_extent, float4 r, float sigma)
{
    float ay = abs(y);
    if (ay >= half_extent.y) {
        return 0.0;
    }
    float left_r;
    float right_r;
    if (y < 0.0) {
        left_r = r.x;
        right_r = r.y;
    } else {
        left_r = r.w;
        right_r = r.z;
    }
    float limit = min(half_extent.x, half_extent.y);
    left_r = clamp(left_r, 0.0, limit);
    right_r = clamp(right_r, 0.0, limit);

    float into_left = max(ay - (half_extent.y - left_r), 0.0);
    float into_right = max(ay - (half_extent.y - right_r), 0.0);
    float left = -(half_extent.x - left_r
        + sqrt(max(left_r * left_r - into_left * into_left, 0.0)));
    float right = half_extent.x - right_r
        + sqrt(max(right_r * right_r - into_right * into_right, 0.0));

    float k = 1.0 / (sigma * 1.4142135623730951);
    float2 e = erf2(float2((px - left) * k, (px - right) * k));
    return clamp((e.x - e.y) * 0.5, 0.0, 1.0);
}

/// Coverage of a Gaussian-blurred rounded box at `p`, relative to its centre.
float shadow_coverage(float2 half_extent, float4 r, float2 p, float sigma)
{
    // Below a fraction of a pixel there is nothing to integrate and the answer
    // is the sharp shape, antialiased the same way every other quad is.
    if (sigma < 0.02) {
        return coverage_from_distance(sd_rounded_box(p, half_extent, r));
    }
    if (half_extent.x <= 0.0 || half_extent.y <= 0.0) {
        return 0.0;
    }

    float reach = 3.0 * sigma;
    float low = max(p.y - reach, -half_extent.y);
    float high = min(p.y + reach, half_extent.y);
    if (high <= low) {
        return 0.0;
    }

    float step_size = (high - low) / float(SHADOW_SLICES);
    float total = 0.0;
    [unroll]
    for (int i = 0; i < SHADOW_SLICES; i = i + 1) {
        float y = low + (float(i) + 0.5) * step_size;
        total = total + shadow_slice(p.x, y, half_extent, r, sigma)
            * shadow_gaussian(p.y - y, sigma)
            * step_size;
    }
    // Three sigma leaves 0.3 % of the kernel unaccounted for; scaling it back
    // removes a seam where a solid shadow meets the shape that cast it.
    return clamp(total * 1.0027, 0.0, 1.0);
}
