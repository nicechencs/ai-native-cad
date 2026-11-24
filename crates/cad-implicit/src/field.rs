//! Scalar fields, spatial ramps, and numerical derivatives.

/// A thread-safe scalar function of a 3D position in millimeters.
///
/// For solid geometry, negative values are inside, zero is the surface, and positive values
/// are outside. Parameter fields such as thickness use the same sampling interface.
pub trait ScalarField: Send + Sync {
    fn eval(&self, p: [f64; 3]) -> f64;
}

/// A spatially uniform scalar value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConstantField(pub f64);

impl ScalarField for ConstantField {
    fn eval(&self, _p: [f64; 3]) -> f64 {
        self.0
    }
}

/// A clamped linear interpolation between two values, driven by a spatial coordinate.
#[derive(Debug, Clone, Copy)]
pub struct FieldRamp {
    coordinate: RampCoordinate,
    start_mm: f64,
    span_mm: f64,
    min_val: f64,
    max_val: f64,
}

#[derive(Debug, Clone, Copy)]
enum RampCoordinate {
    Linear([f64; 3]),
    RadialPoint([f64; 3]),
    RadialAxis { point: [f64; 3], axis: [f64; 3] },
}

impl FieldRamp {
    /// Interpolates by signed distance projected onto `direction` from the world origin.
    ///
    /// Panics for a zero or non-finite direction, a zero or non-finite span, or non-finite values.
    pub fn linear(
        direction: [f64; 3],
        start_mm: f64,
        end_mm: f64,
        min_val: f64,
        max_val: f64,
    ) -> Self {
        let axis = unit_vector(direction).expect("ramp direction must be finite and nonzero");
        Self::new(
            RampCoordinate::Linear(axis),
            start_mm,
            end_mm,
            min_val,
            max_val,
        )
    }

    /// Interpolates by Euclidean distance from `center_mm`.
    ///
    /// Panics for a non-finite center, a zero or non-finite span, or non-finite values.
    pub fn radial_point(
        center_mm: [f64; 3],
        start_mm: f64,
        end_mm: f64,
        min_val: f64,
        max_val: f64,
    ) -> Self {
        assert!(center_mm.into_iter().all(f64::is_finite));
        Self::new(
            RampCoordinate::RadialPoint(center_mm),
            start_mm,
            end_mm,
            min_val,
            max_val,
        )
    }

    /// Interpolates by perpendicular distance from the infinite line through `point_mm`.
    ///
    /// Panics for a non-finite point, a zero or non-finite direction, a zero or non-finite span,
    /// or non-finite values.
    pub fn radial_axis(
        point_mm: [f64; 3],
        direction: [f64; 3],
        start_mm: f64,
        end_mm: f64,
        min_val: f64,
        max_val: f64,
    ) -> Self {
        assert!(point_mm.into_iter().all(f64::is_finite));
        let axis = unit_vector(direction).expect("ramp direction must be finite and nonzero");
        Self::new(
            RampCoordinate::RadialAxis {
                point: point_mm,
                axis,
            },
            start_mm,
            end_mm,
            min_val,
            max_val,
        )
    }

    fn new(
        coordinate: RampCoordinate,
        start_mm: f64,
        end_mm: f64,
        min_val: f64,
        max_val: f64,
    ) -> Self {
        let span_mm = end_mm - start_mm;
        assert!(
            span_mm.is_finite() && span_mm != 0.0,
            "ramp span must be finite and nonzero"
        );
        assert!(
            min_val.is_finite() && max_val.is_finite(),
            "ramp values must be finite"
        );
        Self {
            coordinate,
            start_mm,
            span_mm,
            min_val,
            max_val,
        }
    }
}

impl ScalarField for FieldRamp {
    fn eval(&self, p: [f64; 3]) -> f64 {
        let distance_mm = match self.coordinate {
            RampCoordinate::Linear([x, y, z]) => p[0] * x + p[1] * y + p[2] * z,
            RampCoordinate::RadialPoint(center) => {
                let [x, y, z] = std::array::from_fn(|axis| p[axis] - center[axis]);
                x.hypot(y).hypot(z)
            }
            RampCoordinate::RadialAxis {
                point,
                axis: [a, b, c],
            } => {
                let [x, y, z] = std::array::from_fn(|axis| p[axis] - point[axis]);
                (y * c - z * b).hypot(z * a - x * c).hypot(x * b - y * a)
            }
        };
        let t = ((distance_mm - self.start_mm) / self.span_mm).clamp(0.0, 1.0);
        (1.0 - t) * self.min_val + t * self.max_val
    }
}

/// Estimates the spatial gradient using centered differences with step `eps` in millimeters.
///
/// Panics if `eps` is non-finite or non-positive.
pub fn gradient(field: &dyn ScalarField, p: [f64; 3], eps: f64) -> [f64; 3] {
    assert!(
        eps.is_finite() && eps > 0.0,
        "gradient step must be finite and positive"
    );
    std::array::from_fn(|axis| {
        let mut before = p;
        let mut after = p;
        before[axis] -= eps;
        after[axis] += eps;
        (field.eval(after) - field.eval(before)) / (after[axis] - before[axis])
    })
}

/// Estimates the unit normal toward increasing field values.
///
/// Returns zero when the gradient has no finite direction. Panics for an invalid `eps`.
pub fn normal(field: &dyn ScalarField, p: [f64; 3], eps: f64) -> [f64; 3] {
    unit_vector(gradient(field, p, eps)).unwrap_or([0.0; 3])
}

pub(crate) fn unit_vector(vector: [f64; 3]) -> Option<[f64; 3]> {
    if !vector.into_iter().all(f64::is_finite) {
        return None;
    }
    let scale = vector.map(f64::abs).into_iter().fold(0.0, f64::max);
    if scale == 0.0 {
        return None;
    }
    let scaled = vector.map(|v| v / scale);
    let length = scaled.into_iter().map(|v| v * v).sum::<f64>().sqrt();
    Some(scaled.map(|v| v / length))
}
