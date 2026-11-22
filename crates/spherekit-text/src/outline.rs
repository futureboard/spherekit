//! Glyph outlines: extraction from a face, and the geometry the coverage
//! rasteriser and the hinter work on.
//!
//! ## Coordinate conventions
//!
//! TrueType and CFF outlines are y-**up** and measured in font units. SphereKit
//! is y-**down** and measures glyph geometry in em units so one outline serves
//! every size. [`extract_shape`] performs both conversions in one step: `x_em =
//! x / units_per_em`, `y_em = -y / units_per_em`. Flipping y reverses contour
//! orientation, which is why orientation is re-derived from the signed area
//! rather than assumed (see [`Shape::orient`]).

use crate::types::{FontMetrics, GlyphMetrics};
use spherekit_core::{FontError, GlyphId, Point, Rect, Size};

// ---------------------------------------------------------------------------
// Small vector helpers.
//
// `Point<f32>` doubles as a vector inside this module: the algorithm constantly
// mixes positions and offsets and forcing every step through `Point`/`Size`
// conversions would obscure it. These helpers are the only place that liberty is
// taken.
// ---------------------------------------------------------------------------

#[inline]
fn v(x: f32, y: f32) -> Point<f32> {
    Point::new(x, y)
}

#[inline]
fn vsub(a: Point<f32>, b: Point<f32>) -> Point<f32> {
    v(a.x - b.x, a.y - b.y)
}

#[inline]
fn vadd(a: Point<f32>, b: Point<f32>) -> Point<f32> {
    v(a.x + b.x, a.y + b.y)
}

#[inline]
fn vscale(a: Point<f32>, s: f32) -> Point<f32> {
    v(a.x * s, a.y * s)
}

#[inline]
fn dot(a: Point<f32>, b: Point<f32>) -> f32 {
    a.x * b.x + a.y * b.y
}

/// The z component of the 3D cross product of two 2D vectors.
#[inline]
fn cross(a: Point<f32>, b: Point<f32>) -> f32 {
    a.x * b.y - a.y * b.x
}

#[inline]
fn length(a: Point<f32>) -> f32 {
    dot(a, a).sqrt()
}

#[inline]
fn lerp(a: Point<f32>, b: Point<f32>, t: f32) -> Point<f32> {
    v(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

// ---------------------------------------------------------------------------
// Polynomial roots
// ---------------------------------------------------------------------------

/// Up to three real roots of a polynomial.
///
/// A fixed-size buffer rather than a `Vec`: root finding sits in the innermost
/// loop of field generation, one call per curve per texel, and must not allocate.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Roots {
    values: [f32; 3],
    count: u8,
}

impl Roots {
    /// No real roots.
    pub const NONE: Self = Self { values: [0.0; 3], count: 0 };

    /// The roots found, in solver order (not sorted).
    #[inline]
    pub fn as_slice(&self) -> &[f32] {
        &self.values[..self.count as usize]
    }

    /// How many real roots were found.
    #[inline]
    pub fn len(&self) -> usize {
        self.count as usize
    }

    /// True when the polynomial had no real roots the solver could report.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[inline]
    fn push(&mut self, x: f64) {
        if self.count < 3 {
            self.values[self.count as usize] = x as f32;
            self.count += 1;
        }
    }
}

/// Real roots of `a*x^2 + b*x + c`.
///
/// A degenerate polynomial (`a` and `b` both vanishing) reports no roots even
/// when every `x` is a solution: callers use roots to enumerate distance
/// candidates, and "every point is a candidate" is not actionable.
pub fn solve_quadratic(a: f32, b: f32, c: f32) -> Roots {
    solve_quadratic_f64(a as f64, b as f64, c as f64)
}

fn solve_quadratic_f64(a: f64, b: f64, c: f64) -> Roots {
    let mut out = Roots::NONE;
    // The threshold is absolute rather than relative because the caller feeds in
    // em-scale coefficients, where 1e-14 is far below any meaningful curvature.
    if a.abs() < 1e-14 {
        if b.abs() < 1e-14 {
            return out;
        }
        out.push(-c / b);
        return out;
    }
    let discriminant = b * b - 4.0 * a * c;
    if discriminant > 0.0 {
        let d = discriminant.sqrt();
        out.push((-b + d) / (2.0 * a));
        out.push((-b - d) / (2.0 * a));
    } else if discriminant == 0.0 {
        out.push(-b / (2.0 * a));
    }
    out
}

/// Real roots of `a*x^3 + b*x^2 + c*x + d`.
///
/// Uses the trigonometric solution in the three-real-roots case and Cardano's
/// formula otherwise, in `f64` regardless of the `f32` interface: the
/// intermediate `r^2 - q^3` loses most of its significant digits near a double
/// root, and doing that arithmetic in `f32` produces visibly wrong closest points
/// on nearly-degenerate curves.
pub fn solve_cubic(a: f32, b: f32, c: f32, d: f32) -> Roots {
    let (a, b, c, d) = (a as f64, b as f64, c as f64, d as f64);
    if a.abs() < 1e-14 {
        return solve_quadratic_f64(b, c, d);
    }
    solve_cubic_normed(b / a, c / a, d / a)
}

fn solve_cubic_normed(a: f64, b: f64, c: f64) -> Roots {
    let mut out = Roots::NONE;
    let a2 = a * a;
    let mut q = (a2 - 3.0 * b) / 9.0;
    let r = (a * (2.0 * a2 - 9.0 * b) + 27.0 * c) / 54.0;
    let r2 = r * r;
    let q3 = q * q * q;
    let a = a / 3.0;
    if r2 < q3 {
        // Three distinct real roots: the trigonometric form avoids the complex
        // arithmetic Cardano's formula would otherwise need here.
        let t = (r / q3.sqrt()).clamp(-1.0, 1.0).acos();
        q = -2.0 * q.sqrt();
        out.push(q * (t / 3.0).cos() - a);
        out.push(q * ((t + 2.0 * core::f64::consts::PI) / 3.0).cos() - a);
        out.push(q * ((t - 2.0 * core::f64::consts::PI) / 3.0).cos() - a);
    } else {
        let u = if r < 0.0 { 1.0 } else { -1.0 } * (r.abs() + (r2 - q3).sqrt()).cbrt();
        let w = if u == 0.0 { 0.0 } else { q / u };
        out.push((u + w) - a);
        // u == w is the double-root case; the tolerance catches the far more
        // common "almost a double root", where reporting one root would leave a
        // real closest point undiscovered.
        if u == w || (u - w).abs() < 1e-12 * (u + w).abs() {
            out.push(-0.5 * (u + w) - a);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Edge colors
// ---------------------------------------------------------------------------

/// One outline segment in em units, y-down.
///
/// Fonts only ever produce lines, quadratics (TrueType) and cubics (CFF), so
/// these three cases cover every outline without a general path representation.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum EdgeSegment {
    /// A straight line.
    Linear {
        /// Start point.
        p0: Point<f32>,
        /// End point.
        p1: Point<f32>,
    },
    /// A quadratic Bézier, as produced by TrueType `glyf` outlines.
    Quadratic {
        /// Start point.
        p0: Point<f32>,
        /// The single off-curve control point.
        control: Point<f32>,
        /// End point.
        p1: Point<f32>,
    },
    /// A cubic Bézier, as produced by CFF/Type 2 charstrings.
    Cubic {
        /// Start point.
        p0: Point<f32>,
        /// First control point.
        control0: Point<f32>,
        /// Second control point.
        control1: Point<f32>,
        /// End point.
        p1: Point<f32>,
    },
}

impl EdgeSegment {
    /// The segment's first point.
    #[inline]
    pub fn start(&self) -> Point<f32> {
        match *self {
            EdgeSegment::Linear { p0, .. }
            | EdgeSegment::Quadratic { p0, .. }
            | EdgeSegment::Cubic { p0, .. } => p0,
        }
    }

    /// The segment's last point.
    #[inline]
    pub fn end(&self) -> Point<f32> {
        match *self {
            EdgeSegment::Linear { p1, .. }
            | EdgeSegment::Quadratic { p1, .. }
            | EdgeSegment::Cubic { p1, .. } => p1,
        }
    }

    /// Evaluates the segment at `t` in `[0, 1]`.
    pub fn point(&self, t: f32) -> Point<f32> {
        match *self {
            EdgeSegment::Linear { p0, p1 } => lerp(p0, p1, t),
            EdgeSegment::Quadratic { p0, control, p1 } => {
                lerp(lerp(p0, control, t), lerp(control, p1, t), t)
            }
            EdgeSegment::Cubic { p0, control0, control1, p1 } => {
                let a = lerp(p0, control0, t);
                let b = lerp(control0, control1, t);
                let c = lerp(control1, p1, t);
                lerp(lerp(a, b, t), lerp(b, c, t), t)
            }
        }
    }

    /// The (unnormalised) tangent at `t`.
    ///
    /// Falls back to a chord when the derivative vanishes, which happens when a
    /// control point coincides with an endpoint — common in real fonts and fatal
    /// for corner detection if it returned a zero vector.
    pub fn direction(&self, t: f32) -> Point<f32> {
        match *self {
            EdgeSegment::Linear { p0, p1 } => vsub(p1, p0),
            EdgeSegment::Quadratic { p0, control, p1 } => {
                let tangent = lerp(vsub(control, p0), vsub(p1, control), t);
                if tangent.x == 0.0 && tangent.y == 0.0 { vsub(p1, p0) } else { tangent }
            }
            EdgeSegment::Cubic { p0, control0, control1, p1 } => {
                let a = vsub(control0, p0);
                let b = vsub(control1, control0);
                let c = vsub(p1, control1);
                let tangent = lerp(lerp(a, b, t), lerp(b, c, t), t);
                if tangent.x == 0.0 && tangent.y == 0.0 {
                    if t == 0.0 {
                        return vsub(control1, p0);
                    }
                    if t == 1.0 {
                        return vsub(p1, control0);
                    }
                }
                tangent
            }
        }
    }

    /// The portion of this segment between `t0` and `t1`, as a segment of the
    /// same degree.
    ///
    /// Uses the polar form (blossom) of the Bézier, which gives the exact
    /// control points of the sub-curve in closed form; de Casteljau splitting
    /// twice would accumulate more error and is no simpler here.
    pub fn sub_segment(&self, t0: f32, t1: f32) -> Self {
        match *self {
            EdgeSegment::Linear { .. } => {
                EdgeSegment::Linear { p0: self.point(t0), p1: self.point(t1) }
            }
            EdgeSegment::Quadratic { p0, control, p1 } => EdgeSegment::Quadratic {
                p0: self.point(t0),
                control: blossom2(p0, control, p1, t0, t1),
                p1: self.point(t1),
            },
            EdgeSegment::Cubic { p0, control0, control1, p1 } => EdgeSegment::Cubic {
                p0: self.point(t0),
                control0: blossom3(p0, control0, control1, p1, t0, t0, t1),
                control1: blossom3(p0, control0, control1, p1, t0, t1, t1),
                p1: self.point(t1),
            },
        }
    }

    /// The segment traversed backwards.
    pub fn reversed(&self) -> Self {
        match *self {
            EdgeSegment::Linear { p0, p1 } => EdgeSegment::Linear { p0: p1, p1: p0 },
            EdgeSegment::Quadratic { p0, control, p1 } => {
                EdgeSegment::Quadratic { p0: p1, control, p1: p0 }
            }
            EdgeSegment::Cubic { p0, control0, control1, p1 } => {
                EdgeSegment::Cubic { p0: p1, control0: control1, control1: control0, p1: p0 }
            }
        }
    }

    /// The exact bounding box of the segment.
    ///
    /// Exact rather than the control-point hull: the hull of a cubic can be far
    /// larger than the curve, and the box decides how much padding the glyph's
    /// field carries, i.e. atlas space.
    pub fn bounds(&self) -> Rect<f32> {
        let (mut min, mut max) = (self.start(), self.start());
        let mut include = |p: Point<f32>| {
            min = v(min.x.min(p.x), min.y.min(p.y));
            max = v(max.x.max(p.x), max.y.max(p.y));
        };
        include(self.end());
        match *self {
            EdgeSegment::Linear { .. } => {}
            EdgeSegment::Quadratic { p0, control, p1 } => {
                // The derivative is linear in each axis:
                // 2[(c - p0) + t((p1 - c) - (c - p0))].
                for (a, c, e) in [(p0.x, control.x, p1.x), (p0.y, control.y, p1.y)] {
                    let denom = a - 2.0 * c + e;
                    if denom.abs() > f32::EPSILON {
                        let t = (a - c) / denom;
                        if t > 0.0 && t < 1.0 {
                            include(self.point(t));
                        }
                    }
                }
            }
            EdgeSegment::Cubic { p0, control0, control1, p1 } => {
                // The derivative is quadratic in each axis, with coefficients
                // built from the first differences of the control points.
                for (q0, q1, q2, q3) in
                    [(p0.x, control0.x, control1.x, p1.x), (p0.y, control0.y, control1.y, p1.y)]
                {
                    let a0 = q1 - q0;
                    let a1 = q2 - q1;
                    let a2 = q3 - q2;
                    let roots = solve_quadratic(a0 - 2.0 * a1 + a2, 2.0 * (a1 - a0), a0);
                    for &t in roots.as_slice() {
                        if t > 0.0 && t < 1.0 {
                            include(self.point(t));
                        }
                    }
                }
            }
        }
        Rect::new(min, Size::new(max.x - min.x, max.y - min.y))
    }

    /// Appends a polyline approximation to `out`, excluding the start point.
    ///
    /// The segment count comes from the curve's second difference, which bounds
    /// the chord error and shrinks with `1/n^2`, so a tolerance in em units maps
    /// directly onto a segment count without any adaptive recursion.
    pub fn flatten_into(&self, tolerance: f32, out: &mut Vec<Point<f32>>) {
        let tolerance = tolerance.max(1e-7);
        let n = match *self {
            EdgeSegment::Linear { .. } => 1,
            EdgeSegment::Quadratic { p0, control, p1 } => {
                let d = length(vsub(vadd(p0, p1), vscale(control, 2.0)));
                segment_count(d / 4.0, tolerance)
            }
            EdgeSegment::Cubic { p0, control0, control1, p1 } => {
                let d0 = length(vsub(vadd(p0, control1), vscale(control0, 2.0)));
                let d1 = length(vsub(vadd(control0, p1), vscale(control1, 2.0)));
                segment_count(d0.max(d1), tolerance)
            }
        };
        for i in 1..=n {
            out.push(self.point(i as f32 / n as f32));
        }
    }
}

fn segment_count(deviation: f32, tolerance: f32) -> u32 {
    if !deviation.is_finite() || deviation <= 0.0 {
        return 1;
    }
    ((deviation / tolerance).sqrt().ceil() as u32).clamp(1, 128)
}

/// Polar form of a quadratic Bézier.
fn blossom2(p0: Point<f32>, c: Point<f32>, p1: Point<f32>, u: f32, w: f32) -> Point<f32> {
    let a = (1.0 - u) * (1.0 - w);
    let b = (1.0 - u) * w + (1.0 - w) * u;
    let d = u * w;
    v(a * p0.x + b * c.x + d * p1.x, a * p0.y + b * c.y + d * p1.y)
}

/// Polar form of a cubic Bézier.
fn blossom3(
    p0: Point<f32>,
    c0: Point<f32>,
    c1: Point<f32>,
    p1: Point<f32>,
    u: f32,
    w: f32,
    z: f32,
) -> Point<f32> {
    let (iu, iw, iz) = (1.0 - u, 1.0 - w, 1.0 - z);
    let a = iu * iw * iz;
    let b = iu * iw * z + iu * w * iz + u * iw * iz;
    let c = iu * w * z + u * iw * z + u * w * iz;
    let d = u * w * z;
    v(a * p0.x + b * c0.x + c * c1.x + d * p1.x, a * p0.y + b * c0.y + c * c1.y + d * p1.y)
}

/// A closed sequence of edges.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contour {
    /// Edges in traversal order; the last edge ends where the first begins.
    pub edges: Vec<EdgeSegment>,
}

impl Contour {
    /// An empty contour.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an edge, skipping degenerate ones.
    ///
    /// Zero-length edges carry no direction, which would make corner detection
    /// and every distance sign meaningless, and real fonts do contain them.
    pub fn push(&mut self, segment: EdgeSegment) {
        if is_degenerate(&segment) {
            return;
        }
        self.edges.push(segment);
    }

    /// Reverses traversal order and every edge with it.
    pub fn reverse(&mut self) {
        self.edges.reverse();
        for e in &mut self.edges {
            *e = e.reversed();
        }
    }

    /// A closed polyline approximation, first point repeated at the end.
    pub fn flatten(&self, tolerance: f32) -> Vec<Point<f32>> {
        let mut out = Vec::new();
        let Some(first) = self.edges.first() else { return out };
        out.push(first.start());
        for e in &self.edges {
            e.flatten_into(tolerance, &mut out);
        }
        match (out.first().copied(), out.last().copied()) {
            (Some(f), Some(l)) if f.x != l.x || f.y != l.y => out.push(f),
            _ => {}
        }
        out
    }
}

fn is_degenerate(segment: &EdgeSegment) -> bool {
    const EPS: f32 = 1e-9;
    let close = |a: Point<f32>, b: Point<f32>| (a.x - b.x).abs() < EPS && (a.y - b.y).abs() < EPS;
    match *segment {
        EdgeSegment::Linear { p0, p1 } => close(p0, p1),
        EdgeSegment::Quadratic { p0, control, p1 } => close(p0, control) && close(control, p1),
        EdgeSegment::Cubic { p0, control0, control1, p1 } => {
            close(p0, control0) && close(control0, control1) && close(control1, p1)
        }
    }
}

/// A glyph outline as closed contours in em units, y-down.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shape {
    /// The contours. Outer contours and counters are not distinguished; fill is
    /// decided by the nonzero winding rule, exactly as in the font format.
    pub contours: Vec<Contour>,
}

impl Shape {
    /// An empty shape.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when the shape has no edges at all, as for a space glyph.
    pub fn is_empty(&self) -> bool {
        self.contours.iter().all(|c| c.edges.is_empty())
    }

    /// Total edge count across every contour.
    pub fn edge_count(&self) -> usize {
        self.contours.iter().map(|c| c.edges.len()).sum()
    }

    /// Builds a shape from one closed polygon, for tests and synthetic glyphs.
    ///
    /// The polygon is closed automatically; fewer than two distinct points
    /// produces an empty shape rather than a degenerate contour.
    pub fn from_polygon(points: &[Point<f32>]) -> Self {
        let mut contour = Contour::new();
        for i in 0..points.len() {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            contour.push(EdgeSegment::Linear { p0: a, p1: b });
        }
        if contour.edges.len() < 2 {
            return Self::new();
        }
        Self { contours: vec![contour] }
    }

    /// The exact ink bounding box, or `None` when the shape has no edges.
    pub fn bounds(&self) -> Option<Rect<f32>> {
        let mut acc: Option<(Point<f32>, Point<f32>)> = None;
        for contour in &self.contours {
            for edge in &contour.edges {
                let b = edge.bounds();
                let (lo, hi) = (b.origin, b.max_point());
                acc = Some(match acc {
                    None => (lo, hi),
                    Some((min, max)) => {
                        (v(min.x.min(lo.x), min.y.min(lo.y)), v(max.x.max(hi.x), max.y.max(hi.y)))
                    }
                });
            }
        }
        acc.map(|(min, max)| Rect::new(min, Size::new(max.x - min.x, max.y - min.y)))
    }

    /// Reverses every contour.
    pub fn reverse(&mut self) {
        for c in &mut self.contours {
            c.reverse();
        }
    }

    /// Closed polyline approximations of every contour.
    pub fn flatten(&self, tolerance: f32) -> Vec<Vec<Point<f32>>> {
        self.contours.iter().filter(|c| !c.edges.is_empty()).map(|c| c.flatten(tolerance)).collect()
    }

    /// Rewinds the shape so that the filled side is the positive side.
    ///
    /// Every signed distance in this module takes its sign from edge direction,
    /// so the convention has to be pinned down once. TrueType winds outer
    /// contours clockwise in its own y-up space and CFF winds them
    /// counter-clockwise; after the y flip that is two opposite conventions
    /// reaching the same code. Deciding from the shape's own total signed area
    /// makes the module independent of which format the glyph came from, and lets
    /// a hand-built test shape be wound either way.
    ///
    /// Returns `true` if the shape was reversed.
    pub fn orient(&mut self, tolerance: f32) -> bool {
        let area: f32 = self.flatten(tolerance).iter().map(|p| polygon_area(p)).sum();
        if area > 0.0 {
            self.reverse();
            true
        } else {
            false
        }
    }
}

/// Twice-signed polygon area halved; negative for a counter-clockwise polygon in
/// y-down space, which is the orientation this module treats as "filled".
fn polygon_area(points: &[Point<f32>]) -> f32 {
    let mut sum = 0.0;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        sum += cross(a, b);
    }
    0.5 * sum
}

// ---------------------------------------------------------------------------
// Outline extraction
// ---------------------------------------------------------------------------

/// Collects a `ttf-parser` outline into em-space, y-down contours.
struct OutlineCollector {
    scale: f32,
    shape: Shape,
    current: Contour,
    start: Point<f32>,
    pen: Point<f32>,
}

impl OutlineCollector {
    fn new(units_per_em: f32) -> Self {
        Self {
            scale: 1.0 / units_per_em,
            shape: Shape::new(),
            current: Contour::new(),
            start: v(0.0, 0.0),
            pen: v(0.0, 0.0),
        }
    }

    /// Font units to em units, flipping y: font space is y-up, SphereKit is y-down.
    #[inline]
    fn map(&self, x: f32, y: f32) -> Point<f32> {
        v(x * self.scale, -y * self.scale)
    }

    fn finish_contour(&mut self) {
        if self.pen != self.start {
            self.current.push(EdgeSegment::Linear { p0: self.pen, p1: self.start });
            self.pen = self.start;
        }
        if !self.current.edges.is_empty() {
            self.shape.contours.push(core::mem::take(&mut self.current));
        } else {
            self.current.edges.clear();
        }
    }

    fn finish(mut self) -> Shape {
        self.finish_contour();
        self.shape
    }
}

impl ttf_parser::OutlineBuilder for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish_contour();
        self.start = self.map(x, y);
        self.pen = self.start;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p1 = self.map(x, y);
        self.current.push(EdgeSegment::Linear { p0: self.pen, p1 });
        self.pen = p1;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = self.map(x1, y1);
        let p1 = self.map(x, y);
        self.current.push(EdgeSegment::Quadratic { p0: self.pen, control, p1 });
        self.pen = p1;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let control0 = self.map(x1, y1);
        let control1 = self.map(x2, y2);
        let p1 = self.map(x, y);
        self.current.push(EdgeSegment::Cubic { p0: self.pen, control0, control1, p1 });
        self.pen = p1;
    }

    fn close(&mut self) {
        self.finish_contour();
    }
}

/// Extracts a glyph outline as a [`Shape`] in em units, y-down.
///
/// A glyph with no outline (a space, or a glyph defined only by a bitmap) yields
/// an empty shape rather than an error: it is a perfectly normal thing to shape
/// and lay out, it simply draws nothing.
pub fn extract_shape(face: &ttf_parser::Face<'_>, glyph: GlyphId) -> Result<Shape, FontError> {
    if glyph.0 >= face.number_of_glyphs() {
        return Err(FontError::MissingGlyph(glyph));
    }
    let upem = face.units_per_em().max(1) as f32;
    let mut collector = OutlineCollector::new(upem);
    if face.outline_glyph(ttf_parser::GlyphId(glyph.0), &mut collector).is_none() {
        return Ok(Shape::new());
    }
    Ok(collector.finish())
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Per-glyph metrics in em units, y-down.
///
/// The bounding box comes from the actual outline rather than the `glyf` header,
/// which is allowed to be wrong and in practice sometimes is.
pub fn glyph_metrics(
    face: &ttf_parser::Face<'_>,
    glyph: GlyphId,
) -> Result<GlyphMetrics, FontError> {
    if glyph.0 >= face.number_of_glyphs() {
        return Err(FontError::MissingGlyph(glyph));
    }
    let gid = ttf_parser::GlyphId(glyph.0);
    let upem = face.units_per_em().max(1) as f32;
    let advance = face.glyph_hor_advance(gid).unwrap_or(0) as f32 / upem;
    let left_bearing = face.glyph_hor_side_bearing(gid).unwrap_or(0) as f32 / upem;
    let bounds = match face.glyph_bounding_box(gid) {
        Some(b) => Rect::from_corners(
            v(b.x_min as f32 / upem, -(b.y_max as f32) / upem),
            v(b.x_max as f32 / upem, -(b.y_min as f32) / upem),
        ),
        None => Rect::ZERO,
    };
    Ok(GlyphMetrics { advance, left_bearing, bounds })
}

/// Vertical metrics for a face, in em units.
///
/// `x_height` and `cap_height` fall back to measuring `x` and `H` when the
/// `OS/2` table omits them, which older and hand-built fonts routinely do, and
/// only then to a proportion of the ascent.
pub fn face_metrics(face: &ttf_parser::Face<'_>) -> FontMetrics {
    let units_per_em = face.units_per_em().max(1);
    let upem = units_per_em as f32;
    let ascent = face.ascender() as f32 / upem;
    let descent = -(face.descender() as f32) / upem;

    let measured = |ch: char| -> Option<f32> {
        let gid = face.glyph_index(ch)?;
        let bbox = face.glyph_bounding_box(gid)?;
        Some(bbox.y_max as f32 / upem)
    };
    let x_height = face
        .x_height()
        .map(|v| v as f32 / upem)
        .filter(|v| *v > 0.0)
        .or_else(|| measured('x'))
        .unwrap_or(0.5 * ascent);
    let cap_height = face
        .capital_height()
        .map(|v| v as f32 / upem)
        .filter(|v| *v > 0.0)
        .or_else(|| measured('H'))
        .unwrap_or(0.7 * ascent);

    let underline = face.underline_metrics();
    FontMetrics {
        ascent,
        descent,
        line_gap: face.line_gap() as f32 / upem,
        x_height,
        cap_height,
        // Font units put the underline below the baseline as a negative value;
        // SphereKit measures it positive-down.
        underline_offset: underline.map(|m| -(m.position as f32) / upem).unwrap_or(0.1),
        underline_thickness: underline.map(|m| m.thickness as f32 / upem).unwrap_or(0.05),
        units_per_em,
    }
}
