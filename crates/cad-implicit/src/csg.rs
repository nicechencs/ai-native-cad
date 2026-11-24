//! Composable constructive-solid-geometry fields and lattice/domain blends.

use crate::field::ScalarField;
use crate::tpms::TpmsSheetField;

/// A hard union, whose field is the pointwise minimum of its operands.
pub struct UnionSdf {
    /// First operand.
    pub a: Box<dyn ScalarField>,
    /// Second operand.
    pub b: Box<dyn ScalarField>,
}

impl UnionSdf {
    /// Creates a union of two fields.
    #[must_use]
    pub fn new(a: Box<dyn ScalarField>, b: Box<dyn ScalarField>) -> Self {
        Self { a, b }
    }
}

impl ScalarField for UnionSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
        self.a.eval(p).min(self.b.eval(p))
    }
}

/// A hard intersection, whose field is the pointwise maximum of its operands.
pub struct IntersectSdf {
    /// First operand.
    pub a: Box<dyn ScalarField>,
    /// Second operand.
    pub b: Box<dyn ScalarField>,
}

impl IntersectSdf {
    /// Creates an intersection of two fields.
    #[must_use]
    pub fn new(a: Box<dyn ScalarField>, b: Box<dyn ScalarField>) -> Self {
        Self { a, b }
    }
}

impl ScalarField for IntersectSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
        self.a.eval(p).max(self.b.eval(p))
    }
}

/// Subtracts the second solid from the first.
pub struct SubtractSdf {
    /// Solid being cut.
    pub a: Box<dyn ScalarField>,
    /// Cutting solid.
    pub b: Box<dyn ScalarField>,
}

impl SubtractSdf {
    /// Creates `a \ b`.
    #[must_use]
    pub fn new(a: Box<dyn ScalarField>, b: Box<dyn ScalarField>) -> Self {
        Self { a, b }
    }
}

impl ScalarField for SubtractSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
        self.a.eval(p).max(-self.b.eval(p))
    }
}

/// Polynomial smooth-minimum union.
pub struct SmoothUnionSdf {
    /// First operand.
    pub a: Box<dyn ScalarField>,
    /// Second operand.
    pub b: Box<dyn ScalarField>,
    /// Blend radius in millimetres. Non-positive values select a hard union.
    pub k_mm: f64,
}

impl SmoothUnionSdf {
    /// Creates a polynomial smooth union with blend radius `k_mm`.
    #[must_use]
    pub fn new(a: Box<dyn ScalarField>, b: Box<dyn ScalarField>, k_mm: f64) -> Self {
        Self { a, b, k_mm }
    }
}

impl ScalarField for SmoothUnionSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
        smooth_min(self.a.eval(p), self.b.eval(p), self.k_mm)
    }
}

/// Polynomial smooth-subtraction operator.
pub struct SmoothSubtractSdf {
    /// Solid being cut.
    pub a: Box<dyn ScalarField>,
    /// Cutting solid.
    pub b: Box<dyn ScalarField>,
    /// Blend radius in millimetres. Non-positive values select hard subtraction.
    pub k_mm: f64,
}

impl SmoothSubtractSdf {
    /// Creates a smooth subtraction with blend radius `k_mm`.
    #[must_use]
    pub fn new(a: Box<dyn ScalarField>, b: Box<dyn ScalarField>, k_mm: f64) -> Self {
        Self { a, b, k_mm }
    }
}

impl ScalarField for SmoothSubtractSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
        -smooth_min(-self.a.eval(p), self.b.eval(p), self.k_mm)
    }
}

/// A shell around an input level set.
pub struct ShellSdf {
    /// Input surface field.
    pub field: Box<dyn ScalarField>,
    /// Shell half-thickness in millimetres.
    pub thickness_mm: f64,
}

impl ShellSdf {
    /// Creates a symmetric shell around the zero level set. The supplied
    /// thickness is the distance on either side of the source surface.
    #[must_use]
    pub fn new(field: Box<dyn ScalarField>, thickness_mm: f64) -> Self {
        Self {
            field,
            thickness_mm,
        }
    }
}

impl ScalarField for ShellSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
        self.field.eval(p).abs() - self.thickness_mm.max(0.0)
    }
}

/// A TPMS lattice clipped to a domain, optionally retaining a solid outer
/// skin and a protected interface/boss zone.
pub struct LatticeInfillSdf {
    /// Domain SDF. Its interior bounds the returned material.
    pub domain: Box<dyn ScalarField>,
    /// Sheet lattice field.
    pub sheet: TpmsSheetField,
    skin_thickness_mm: Option<f64>,
    protected_interface: Option<(Box<dyn ScalarField>, f64)>,
}

impl LatticeInfillSdf {
    /// Creates a domain-clipped TPMS sheet lattice.
    #[must_use]
    pub fn new(domain: Box<dyn ScalarField>, sheet: TpmsSheetField) -> Self {
        Self {
            domain,
            sheet,
            skin_thickness_mm: None,
            protected_interface: None,
        }
    }

    /// Retains a solid band of `thickness_mm` along the inside of the domain
    /// boundary. This is useful as a printable outer skin.
    #[must_use]
    pub fn with_skin(mut self, thickness_mm: f64) -> Self {
        self.skin_thickness_mm = (thickness_mm > 0.0).then_some(thickness_mm);
        self
    }

    /// Retains the interface field and a surrounding solid margin. The
    /// interface is expanded by `margin_mm` before it is unioned with the
    /// lattice, protecting exact mechanical interfaces from lattice cuts.
    #[must_use]
    pub fn with_protected_interface(
        mut self,
        interface: Box<dyn ScalarField>,
        margin_mm: f64,
    ) -> Self {
        self.protected_interface = Some((interface, margin_mm.max(0.0)));
        self
    }
}

impl ScalarField for LatticeInfillSdf {
    fn eval(&self, p: [f64; 3]) -> f64 {
        let domain = self.domain.eval(p);
        let mut material = self.sheet.eval(p);

        if let Some((interface, margin_mm)) = &self.protected_interface {
            // Expand the protected region before unioning it with the sheet.
            material = material.min(interface.eval(p) - *margin_mm);
        }

        if let Some(skin_thickness_mm) = self.skin_thickness_mm {
            material = material.min(-domain - skin_thickness_mm);
        }

        // Clip all material (including the skin's outside half-space) to the domain.
        material.max(domain)
    }
}

fn smooth_min(a: f64, b: f64, k: f64) -> f64 {
    let min = a.min(b);
    let difference = (a - b).abs();
    if difference < k {
        let h = 1.0 - difference / k;
        min - (0.25 * k) * h * h
    } else {
        min
    }
}
