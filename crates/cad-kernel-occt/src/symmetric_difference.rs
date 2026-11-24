use crate::{KernelError, SolidShape, check_exact_operand};

/// Exact B-Rep Boolean volumes in cubic millimetres. `relative_error` is the
/// sum of the two differences divided by `reference_volume_mm3`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SymmetricDifference {
    pub candidate_only_mm3: f64,
    pub reference_only_mm3: f64,
    pub reference_volume_mm3: f64,
    pub symmetric_difference_mm3: f64,
    pub relative_error: f64,
}

impl SymmetricDifference {
    #[cfg(feature = "occt-ffi")]
    pub(crate) fn from_volumes(
        candidate_only_mm3: f64,
        reference_only_mm3: f64,
        reference_volume_mm3: f64,
    ) -> Result<Self, KernelError> {
        let symmetric_difference_mm3 = candidate_only_mm3 + reference_only_mm3;
        let relative_error = symmetric_difference_mm3 / reference_volume_mm3;
        if candidate_only_mm3 < 0.0
            || reference_only_mm3 < 0.0
            || !reference_volume_mm3.is_finite()
            || reference_volume_mm3 <= 0.0
            || !relative_error.is_finite()
        {
            return Err(KernelError::OperationFailed(
                "invalid exact symmetric difference volumes".into(),
            ));
        }
        Ok(Self {
            candidate_only_mm3,
            reference_only_mm3,
            reference_volume_mm3,
            symmetric_difference_mm3,
            relative_error,
        })
    }
}

/// Compare two single, closed, positive-volume solids in their supplied
/// coordinates. Uses native OCCT cuts without a mesh or bounding-box fallback.
pub fn measure_symmetric_difference(
    candidate: &SolidShape,
    reference: &SolidShape,
) -> Result<SymmetricDifference, KernelError> {
    check_exact_operand(candidate)?;
    check_exact_operand(reference)?;
    #[cfg(feature = "occt-ffi")]
    return crate::occt_ffi::measure_symmetric_difference(candidate, reference);
    #[cfg(not(feature = "occt-ffi"))]
    Err(KernelError::BackendUnavailable)
}
