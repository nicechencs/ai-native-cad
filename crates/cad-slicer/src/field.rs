/// A scalar field in millimeters whose zero level defines a solid boundary.
pub trait SliceField {
    /// Returns a negative value inside the solid and a positive value outside.
    fn eval(&self, x_mm: f64, y_mm: f64, z_mm: f64) -> f64;
}

/// Adapts a closure or function to [`SliceField`].
pub struct FnSliceField<F: Fn(f64, f64, f64) -> f64>(F);

impl<F: Fn(f64, f64, f64) -> f64> FnSliceField<F> {
    pub fn new(eval: F) -> Self {
        Self(eval)
    }
}

impl<F: Fn(f64, f64, f64) -> f64> SliceField for FnSliceField<F> {
    fn eval(&self, x_mm: f64, y_mm: f64, z_mm: f64) -> f64 {
        (self.0)(x_mm, y_mm, z_mm)
    }
}
