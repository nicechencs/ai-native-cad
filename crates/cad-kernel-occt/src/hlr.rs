use crate::{KernelError, KernelPayload, SolidShape};

/// Projected curves in millimetres. Visibility is exact B-Rep HLR; the
/// resulting visible and hidden curve intervals are sampled for vector export.
#[derive(Debug, Default, PartialEq)]
pub struct HlrProjection {
    pub visible: Vec<Vec<[f64; 2]>>,
    pub hidden: Vec<Vec<[f64; 2]>>,
}

/// Computes an orthographic projection for a native B-Rep. Non-native
/// payloads return `Ok(None)` so callers can retain their existing renderer.
/// `toward_eye` and `view_x` specify the camera's +Z and +X directions.
pub fn compute_hlr(
    shape: &SolidShape,
    toward_eye: [f64; 3],
    view_x: [f64; 3],
    deflection_mm: f64,
) -> Result<Option<HlrProjection>, KernelError> {
    let KernelPayload::Occt(handle) = &shape.payload else {
        return Ok(None);
    };
    #[cfg(feature = "occt-ffi")]
    {
        crate::occt_ffi::compute_hlr(handle, toward_eye, view_x, deflection_mm).map(Some)
    }
    #[cfg(not(feature = "occt-ffi"))]
    {
        let _ = (handle, toward_eye, view_x, deflection_mm);
        Err(KernelError::BackendUnavailable)
    }
}
