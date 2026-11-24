use cad_syntax::Span;

use crate::{KernelError, SolidShape};

/// Input sections for a B-spline loft. Each section contains XYZ vertices in
/// millimetres and is closed by the native bridge.
#[derive(Debug, Clone, PartialEq)]
pub struct BsplineLoftSpec {
    pub sections: Vec<Vec<[f64; 3]>>,
    pub make_solid: bool,
    pub ruled: bool,
}

/// Parameters for drafting planar side faces of a native solid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DraftFacesSpec {
    pub draft_angle_deg: f64,
    pub pull_dir: [f64; 3],
    pub neutral_origin_mm: [f64; 3],
    pub neutral_normal: [f64; 3],
}

/// Start and end radii for a variable-radius fillet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VariableFilletSpec {
    pub r_start_mm: f64,
    pub r_end_mm: f64,
}

/// A rectangular U-major grid fitted to a B-spline surface.
#[derive(Debug, Clone, PartialEq)]
pub struct QuadGridSurfaceSpec {
    pub nu: usize,
    pub nv: usize,
    pub xyz_grid: Vec<[f64; 3]>,
    pub thickness_mm: f64,
}

const MAX_LOFT_POINTS: usize = isize::MAX as usize / std::mem::size_of::<[f64; 3]>();
const MAX_GRID_POINTS: usize = i32::MAX as usize;

fn invalid(message: impl Into<String>) -> KernelError {
    KernelError::OperationFailed(format!("freeform input invalid: {}", message.into()))
}

fn finite_point(point: &[f64; 3]) -> bool {
    point.iter().all(|value| value.is_finite())
}

fn non_zero_vector(vector: &[f64; 3]) -> bool {
    finite_point(vector) && vector.iter().any(|value| *value != 0.0)
}

fn validate_loft(spec: &BsplineLoftSpec) -> Result<(), KernelError> {
    if spec.sections.len() < 2 {
        return Err(invalid("loft requires at least two sections"));
    }
    i32::try_from(spec.sections.len())
        .map_err(|_| invalid("section count exceeds the native ABI limit"))?;
    let mut total_points = 0usize;
    for (index, section) in spec.sections.iter().enumerate() {
        if section.len() < 3 {
            return Err(invalid(format!(
                "section {index} requires at least three points"
            )));
        }
        i32::try_from(section.len())
            .map_err(|_| invalid(format!("section {index} exceeds the native ABI limit")))?;
        total_points = total_points
            .checked_add(section.len())
            .ok_or_else(|| invalid("point count overflows the native ABI limit"))?;
        for point in section {
            if !finite_point(point) {
                return Err(invalid(format!(
                    "section {index} contains non-finite coordinates"
                )));
            }
        }
    }
    if total_points > MAX_LOFT_POINTS {
        return Err(invalid("point count exceeds the native ABI limit"));
    }
    Ok(())
}

fn validate_draft(spec: &DraftFacesSpec) -> Result<(), KernelError> {
    if !spec.draft_angle_deg.is_finite()
        || spec.draft_angle_deg == 0.0
        || spec.draft_angle_deg.abs() >= 90.0
    {
        return Err(invalid(
            "draft angle must be finite and have a magnitude between 0 and 90 degrees (exclusive)",
        ));
    }
    if !non_zero_vector(&spec.pull_dir) {
        return Err(invalid("pull direction must be finite and non-zero"));
    }
    if !finite_point(&spec.neutral_origin_mm) {
        return Err(invalid("neutral origin must contain finite coordinates"));
    }
    if !non_zero_vector(&spec.neutral_normal) {
        return Err(invalid("neutral normal must be finite and non-zero"));
    }
    Ok(())
}

fn validate_fillet(spec: &VariableFilletSpec) -> Result<(), KernelError> {
    if !spec.r_start_mm.is_finite() || spec.r_start_mm <= 0.0 {
        return Err(invalid("radius r_start_mm must be finite and positive"));
    }
    if !spec.r_end_mm.is_finite() || spec.r_end_mm <= 0.0 {
        return Err(invalid("radius r_end_mm must be finite and positive"));
    }
    Ok(())
}

fn validate_grid(spec: &QuadGridSurfaceSpec) -> Result<(), KernelError> {
    if spec.nu < 2 || spec.nv < 2 {
        return Err(invalid("nu and nv must each be at least two"));
    }
    let expected = spec
        .nu
        .checked_mul(spec.nv)
        .ok_or_else(|| invalid("grid dimensions overflow"))?;
    if expected != spec.xyz_grid.len() {
        return Err(invalid("xyz_grid length must equal nu * nv"));
    }
    if expected > MAX_GRID_POINTS {
        return Err(invalid("grid point count exceeds the native ABI limit"));
    }
    if !spec.thickness_mm.is_finite() || spec.thickness_mm < 0.0 {
        return Err(invalid("thickness_mm must be finite and non-negative"));
    }
    if spec.xyz_grid.iter().any(|point| !finite_point(point)) {
        return Err(invalid("xyz_grid contains non-finite coordinates"));
    }
    Ok(())
}

/// Build a B-spline loft from closed polygonal sections.
pub fn make_bspline_loft(spec: &BsplineLoftSpec, span: Span) -> Result<SolidShape, KernelError> {
    validate_loft(spec)?;
    #[cfg(feature = "occt-ffi")]
    {
        crate::occt_ffi::make_bspline_loft(spec, span)
    }
    #[cfg(not(feature = "occt-ffi"))]
    {
        let _ = span;
        Err(KernelError::BackendUnavailable)
    }
}

/// Draft the eligible planar faces of a native solid.
pub fn draft_solid_faces(
    shape: &SolidShape,
    spec: &DraftFacesSpec,
    span: Span,
) -> Result<SolidShape, KernelError> {
    validate_draft(spec)?;
    #[cfg(feature = "occt-ffi")]
    {
        crate::occt_ffi::draft_solid_faces(shape, spec, span)
    }
    #[cfg(not(feature = "occt-ffi"))]
    {
        let _ = (shape, span);
        Err(KernelError::BackendUnavailable)
    }
}

/// Apply a linearly varying fillet radius to eligible edges of a native solid.
pub fn variable_fillet_solid(
    shape: &SolidShape,
    spec: &VariableFilletSpec,
    span: Span,
) -> Result<SolidShape, KernelError> {
    validate_fillet(spec)?;
    #[cfg(feature = "occt-ffi")]
    {
        crate::occt_ffi::variable_fillet_solid(shape, spec, span)
    }
    #[cfg(not(feature = "occt-ffi"))]
    {
        let _ = (shape, span);
        Err(KernelError::BackendUnavailable)
    }
}

/// Fit a B-spline surface through a rectangular point grid and optionally
/// extrude it along the fitted normal. Zero thickness returns a face.
pub fn quad_grid_to_bspline_solid(
    spec: &QuadGridSurfaceSpec,
    span: Span,
) -> Result<SolidShape, KernelError> {
    validate_grid(spec)?;
    #[cfg(feature = "occt-ffi")]
    {
        crate::occt_ffi::quad_grid_to_bspline_solid(spec, span)
    }
    #[cfg(not(feature = "occt-ffi"))]
    {
        let _ = span;
        Err(KernelError::BackendUnavailable)
    }
}
