pub mod brep_gen;
pub mod brep_kernel;
pub mod csg;
pub mod freeform;
pub mod geomd_client;
pub mod hlr;
#[cfg(feature = "occt-ffi")]
pub mod occt_ffi;
#[cfg(feature = "occt-ffi")]
pub mod occt_kernel;
mod step_geometry;
pub mod step_writer;
mod symmetric_difference;
#[cfg(feature = "occt-ffi")]
pub mod topology;
pub mod types;
pub mod xcaf;

pub use brep_gen::*;
pub use brep_kernel::{
    BRepKernelMetadata, IBRepKernel, KernelCapabilities, KernelProviderKind, default_brep_kernel,
};
pub use csg::*;
#[cfg(feature = "occt-ffi")]
pub use occt_ffi::DressUpKind;
#[cfg(feature = "occt-ffi")]
pub use occt_kernel::OcctKernel;
pub use freeform::{
    BsplineLoftSpec, DraftFacesSpec, QuadGridSurfaceSpec, VariableFilletSpec,
    draft_solid_faces, make_bspline_loft, quad_grid_to_bspline_solid, variable_fillet_solid,
};
pub use geomd_client::{
    CAD_GEOMD_BIN_ENV, GEOMD_MAX_FRAME_BYTES, GEOMD_PROTOCOL_MAGIC, GeomdBooleanOp,
    GeomdCapabilities, GeomdClient, GeomdClientConfig, GeomdError, GeomdKernelPreference,
    GeomdMeshResponse, GeomdPingResponse, GeomdShapeResponse, GeomdShutdownResponse,
    GeomdTessellationParams, decode_frame, encode_frame, locate_geomd_binary, read_frame,
    write_frame,
};
pub use step_writer::{
    AUTHORITATIVE_KERNEL_REQUIRED_CODE, NON_AUTHORITATIVE_STEP_MARKER, StepExportAuthority,
    StepExportOutput, export_step_ap242, try_export_authoritative_step_ap242,
    try_export_step_ap242, try_export_step_ap242_with_authority,
};
pub use symmetric_difference::{SymmetricDifference, measure_symmetric_difference};
pub use types::*;
pub use xcaf::{
    XcafAssemblyNode, XcafPmiItem, XcafStats, XcafStepMetadata, import_step_xcaf_metadata,
    parse_xcaf_metadata_json,
};

use cad_syntax::Span;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum OcctImportFormat {
    Step = 1,
    Iges = 2,
    Brep = 3,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct OcctImportOptions {
    pub heal_geometry: i32,
    pub unify_same_domain: i32,
    pub sewing_tol_mm: f64,
    pub linear_deflection_mm: f64,
    pub angular_deflection_rad: f64,
}

impl Default for OcctImportOptions {
    fn default() -> Self {
        Self {
            heal_geometry: 0,
            unify_same_domain: 0,
            sewing_tol_mm: 1e-3,
            linear_deflection_mm: 0.05,
            angular_deflection_rad: 0.2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcctImportMetadata {
    pub unit_detected: String,
    pub is_watertight_solid: bool,
    pub open_shell_count: usize,
    pub component_count: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("the OCCT backend is unavailable; build with the occt-ffi feature")]
    BackendUnavailable,
    #[error("geometry contains an open or non-manifold shell")]
    OpenShell,
    #[error(
        "this boolean requires exact geometry; use STEP/BREP instead of a mesh or approximate feature"
    )]
    UnsupportedMixedBoolean,
    #[error("{0}")]
    OperationFailed(String),
}

/// Diagnostic captured on the calling thread immediately after an OCCT bridge failure.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, thiserror::Error)]
#[error("OCCT: {operation} failed (status {status_code}): {message}")]
pub struct OcctNativeError {
    pub operation: String,
    pub status_code: i32,
    pub message: String,
}

/// An operation error that preserves the native diagnostic when the bridge supplies one.
#[derive(Debug, thiserror::Error)]
pub enum KernelOperationError {
    #[error(transparent)]
    Kernel(#[from] KernelError),
    #[error(transparent)]
    Native(#[from] OcctNativeError),
}

impl From<KernelOperationError> for KernelError {
    fn from(error: KernelOperationError) -> Self {
        match error {
            KernelOperationError::Kernel(error) => error,
            KernelOperationError::Native(error) => Self::OperationFailed(error.to_string()),
        }
    }
}

fn is_windows_abs_path(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

fn is_unix_abs_path(token: &str) -> bool {
    token.starts_with('/') && token.len() > 2 && token[1..].contains('/')
}

fn sanitize_native_message(message: &str) -> String {
    let scrubbed = message
        .split_whitespace()
        .map(|token| {
            let trimmed_start = token.trim_start_matches(['\'', '"', '(', '[']);
            let prefix_len = token.len() - trimmed_start.len();
            let prefix = &token[..prefix_len];
            let core = trimmed_start.trim_end_matches(['\'', '"', ')', ']', ',', ';', ':']);
            let suffix = &trimmed_start[core.len()..];
            if is_windows_abs_path(core) || is_unix_abs_path(core) {
                format!("{prefix}{}{suffix}", cad_observe::sanitize_path(core, None))
            } else {
                token.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    cad_observe::sanitize_message(&scrubbed)
}

#[cfg_attr(not(feature = "occt-ffi"), allow(dead_code))]
pub(crate) fn record_native_error(err: &OcctNativeError) {
    let message = sanitize_native_message(&err.message);
    tracing::error!(
        component = "cad-kernel-occt",
        operation = err.operation.as_str(),
        code = cad_observe::codes::R012_KERNEL_NATIVE,
        native_status = err.status_code,
        "{message}"
    );
}

/// The native shape is released when the last SolidShape clone owning it is dropped.
#[cfg(feature = "occt-ffi")]
pub use occt_ffi::OcctShapeGuard;

/// An uninhabited native handle when OCCT support was not compiled in.
#[cfg(not(feature = "occt-ffi"))]
#[derive(Debug)]
pub enum OcctShapeGuard {}

/// Import exactly the bytes supplied by the asset resolver, not a path to reopen.
pub fn import_occt_bytes(
    bytes: &[u8],
    format: OcctImportFormat,
    options: &OcctImportOptions,
    component: Option<&str>,
    allow_open_shell: bool,
    span: Span,
) -> Result<(SolidShape, OcctImportMetadata), KernelError> {
    #[cfg(feature = "occt-ffi")]
    {
        occt_ffi::import_bytes(bytes, format, options, component, allow_open_shell, span)
    }
    #[cfg(not(feature = "occt-ffi"))]
    {
        let _ = (bytes, format, options, component, allow_open_shell, span);
        Err(KernelError::BackendUnavailable)
    }
}

/// Check candidate tessellation indices on the current solid and return their native
/// through-hole radii in the same order. Zero marks an unsupported face.
pub fn through_hole_radii(shape: &SolidShape, face_indices: &[usize]) -> Option<Vec<f64>> {
    #[cfg(feature = "occt-ffi")]
    if let KernelPayload::Occt(handle) = &shape.payload {
        return occt_ffi::through_hole_radii(handle, face_indices);
    }
    let _ = (shape, face_indices);
    None
}

/// Resize one full straight cylindrical through-hole on the supplied native solid.
/// `face_index` must belong to this revision's `faces` array; no analytic or mesh
/// fallback is provided. Positive deltas grow the hole radius.
pub fn try_offset_through_hole(
    shape: &SolidShape,
    face_index: usize,
    delta_mm: f64,
    span: Span,
) -> Result<SolidShape, KernelError> {
    if !delta_mm.is_finite() {
        return Err(KernelError::OperationFailed(
            "face::offset requires a finite delta".into(),
        ));
    }
    edit_through_hole(shape, face_index, ThroughHoleEdit::Offset(delta_mm), span)
}

/// Translate one full straight cylindrical through-hole perpendicular to its axis.
pub fn try_move_through_hole(
    shape: &SolidShape,
    face_index: usize,
    offset_mm: [f64; 3],
    span: Span,
) -> Result<SolidShape, KernelError> {
    if !offset_mm.iter().all(|value| value.is_finite()) {
        return Err(KernelError::OperationFailed(
            "face::move requires a finite offset".into(),
        ));
    }
    edit_through_hole(shape, face_index, ThroughHoleEdit::Move(offset_mm), span)
}

/// Fill one full straight cylindrical through-hole on the supplied native solid.
pub fn try_delete_through_hole(
    shape: &SolidShape,
    face_index: usize,
    span: Span,
) -> Result<SolidShape, KernelError> {
    edit_through_hole(shape, face_index, ThroughHoleEdit::Delete, span)
}

#[cfg_attr(not(feature = "occt-ffi"), allow(dead_code))]
#[derive(Clone, Copy)]
enum ThroughHoleEdit {
    Offset(f64),
    Move([f64; 3]),
    Delete,
}

impl ThroughHoleEdit {
    fn name(self) -> &'static str {
        match self {
            Self::Offset(_) => "face::offset",
            Self::Move(_) => "face::move",
            Self::Delete => "face::delete",
        }
    }
}

fn edit_through_hole(
    shape: &SolidShape,
    face_index: usize,
    edit: ThroughHoleEdit,
    span: Span,
) -> Result<SolidShape, KernelError> {
    if !matches!(shape.payload, KernelPayload::Occt(_)) {
        return Err(KernelError::OperationFailed(format!(
            "{} requires a native solid; analytic and mesh solids are unsupported",
            edit.name()
        )));
    }
    #[cfg(feature = "occt-ffi")]
    if let KernelPayload::Occt(handle) = &shape.payload {
        if handle.is_open_shell() {
            return Err(KernelError::OpenShell);
        }
        return occt_ffi::edit_through_hole(shape, handle, face_index, edit, span);
    }
    let _ = (face_index, span);
    Err(KernelError::BackendUnavailable)
}

/// Give a newly evaluated import a distinct topology owner without duplicating
/// immutable geometry from the shared asset cache.
pub fn scope_native_import_instance(shape: &mut SolidShape) {
    #[cfg(feature = "occt-ffi")]
    if let KernelPayload::Occt(handle) = &mut shape.payload {
        *handle = std::sync::Arc::new(handle.as_ref().clone());
    }
    #[cfg(not(feature = "occt-ffi"))]
    let _ = shape;
}

pub fn try_transform_solid(shape: &SolidShape, matrix: Mat4) -> Result<SolidShape, KernelError> {
    #[cfg(feature = "occt-ffi")]
    if let KernelPayload::Occt(handle) = &shape.payload {
        return occt_ffi::transform(shape, handle, matrix);
    }
    Ok(brep_gen::transform_preview(shape, matrix))
}

fn check_boolean_operands<'a>(
    items: impl IntoIterator<Item = &'a SolidShape>,
) -> Result<bool, KernelError> {
    let mut mesh = false;
    let mut native = false;
    for item in items {
        match &item.payload {
            KernelPayload::Analytic => {}
            KernelPayload::TriMesh => {
                if !item.csg.is_closed_mesh() {
                    return Err(KernelError::OpenShell);
                }
                mesh = true;
            }
            KernelPayload::Occt(handle) => {
                #[cfg(feature = "occt-ffi")]
                if handle.is_open_shell() {
                    return Err(KernelError::OpenShell);
                }
                #[cfg(not(feature = "occt-ffi"))]
                let _ = handle;
                native = true;
            }
        }
    }
    if mesh {
        Err(KernelError::UnsupportedMixedBoolean)
    } else {
        Ok(native)
    }
}

pub fn try_boolean_union(items: Vec<SolidShape>, name: &str) -> Result<SolidShape, KernelError> {
    AnalyticKernel::new().try_boolean_union(items, name)
}

pub fn try_boolean_subtract(
    base: SolidShape,
    tools: Vec<SolidShape>,
) -> Result<SolidShape, KernelError> {
    AnalyticKernel::new().try_boolean_subtract(base, tools)
}

/// Exact intersection. There was no legacy analytic intersection operator.
pub fn try_boolean_intersect(
    base: SolidShape,
    tool: SolidShape,
) -> Result<SolidShape, KernelError> {
    try_boolean_intersect_detailed(base, tool).map_err(Into::into)
}

/// Exact intersection with the native diagnostic preserved on failure.
pub fn try_boolean_intersect_detailed(
    base: SolidShape,
    tool: SolidShape,
) -> Result<SolidShape, KernelOperationError> {
    check_boolean_operands([&base, &tool])?;
    #[cfg(feature = "occt-ffi")]
    return occt_ffi::boolean_intersect(base, tool);
    #[cfg(not(feature = "occt-ffi"))]
    Err(KernelError::BackendUnavailable.into())
}

/// A violation measured on exact OCCT B-Rep geometry (millimetre units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InterferenceViolation {
    Overlap { volume_mm3: f64 },
    Clearance { distance_mm: f64 },
}

/// Check two closed solids in their supplied coordinates. A positive common
/// volume above 1e-4 mm³ takes precedence over insufficient clearance, which
/// is a minimum distance strictly below `min_clearance_mm - 1e-4` mm.
/// `Ok(None)` means the exact check passed; no approximate fallback is used.
pub fn check_interference(
    a: &SolidShape,
    b: &SolidShape,
    min_clearance_mm: f64,
) -> Result<Option<InterferenceViolation>, KernelError> {
    if !min_clearance_mm.is_finite() || min_clearance_mm < 0.0 {
        return Err(KernelError::OperationFailed(
            "minimum clearance must be a finite, nonnegative length".into(),
        ));
    }
    check_exact_operand(a)?;
    check_exact_operand(b)?;
    #[cfg(feature = "occt-ffi")]
    return occt_ffi::check_interference(a, b, min_clearance_mm);
    #[cfg(not(feature = "occt-ffi"))]
    Err(KernelError::BackendUnavailable)
}

fn check_exact_operand(solid: &SolidShape) -> Result<(), KernelError> {
    match &solid.payload {
        KernelPayload::TriMesh => Err(KernelError::UnsupportedMixedBoolean),
        KernelPayload::Analytic if !solid.exact_brep || !is_liftable_exact_csg(&solid.csg) => {
            Err(KernelError::UnsupportedMixedBoolean)
        }
        _ => Ok(()),
    }
}

fn is_liftable_exact_csg(csg: &CsgNode) -> bool {
    match csg {
        CsgNode::Mesh { .. } | CsgNode::Occt { .. } | CsgNode::ChamferedBox { .. } => false,
        CsgNode::Transform { child, .. } | CsgNode::Reflect { child, .. } => {
            is_liftable_exact_csg(child)
        }
        CsgNode::Union(items) => items.iter().all(is_liftable_exact_csg),
        CsgNode::Subtract { base, tools } => {
            is_liftable_exact_csg(base) && tools.iter().all(is_liftable_exact_csg)
        }
        CsgNode::Empty
        | CsgNode::Box { .. }
        | CsgNode::Cylinder { .. }
        | CsgNode::Counterbore { .. }
        | CsgNode::Prism { .. }
        | CsgNode::Revolve { .. } => true,
    }
}

/// Geometry operations used by the Rust evaluator and its hosts.
/// Native OCCT routing remains the default for existing `SolidShape` payloads.
pub trait GeometryKernel {
    fn backend_name(&self) -> &'static str;

    fn make_box(&self, size: [f64; 3], origin: [f64; 3], span: Span) -> SolidShape;

    fn make_cylinder(
        &self,
        diameter: f64,
        height: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        exact_brep: bool,
        span: Span,
    ) -> SolidShape;

    fn extrude(
        &self,
        plane: SketchPlane,
        polygon_2d: &[[f64; 2]],
        height: f64,
        symmetric: bool,
        span: Span,
    ) -> SolidShape;

    fn revolve(
        &self,
        plane: SketchPlane,
        polygon_2d: &[[f64; 2]],
        axis: [f64; 3],
        angle_deg: f64,
        span: Span,
    ) -> SolidShape;

    fn fillet(&self, shape: &SolidShape, radius: f64, span: Span) -> Result<SolidShape, String>;

    fn chamfer(&self, shape: &SolidShape, dist: f64, span: Span) -> Result<SolidShape, String>;

    fn shell(
        &self,
        shape: &SolidShape,
        thickness: f64,
        open_normals: &[[f64; 3]],
        span: Span,
    ) -> Result<SolidShape, String>;

    fn sweep(
        &self,
        profile_2d: &[[f64; 2]],
        path_3d: &[[f64; 3]],
        span: Span,
    ) -> Result<SolidShape, String>;

    fn loft(
        &self,
        sections: &[(Vec<[f64; 2]>, SketchPlane, f64)],
        span: Span,
    ) -> Result<SolidShape, String>;

    fn mirror(&self, shape: &SolidShape, plane: SketchPlane, merge: bool) -> SolidShape;

    fn hole_tapped(
        &self,
        spec: &str,
        depth: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        span: Span,
    ) -> Result<SolidShape, String>;

    fn hole_simple(
        &self,
        diameter: f64,
        depth: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        span: Span,
    ) -> SolidShape;

    fn hole_counterbore(
        &self,
        d_through: f64,
        d_cbore: f64,
        depth_cbore: f64,
        depth_through: f64,
        origin: [f64; 3],
        span: Span,
    ) -> SolidShape;

    fn array_linear(
        &self,
        feature: &SolidShape,
        axis: [f64; 3],
        count: usize,
        spacing: f64,
    ) -> SolidShape;

    fn array_polar(
        &self,
        feature: &SolidShape,
        axis: [f64; 3],
        count: usize,
        radius: Option<f64>,
        start_angle_deg: f64,
    ) -> SolidShape;

    fn boolean_union(&self, items: Vec<SolidShape>, name: &str) -> SolidShape;

    fn boolean_subtract(&self, base: SolidShape, tools: Vec<SolidShape>) -> SolidShape;

    fn check_solid(&self, shape: &SolidShape) -> CheckReport;

    fn export_step(&self, shape: &SolidShape) -> Result<String, String>;

    // The defaults preserve routing for the existing SolidShape payloads.
    // A replacement kernel can override these operations as well as construction/check/export.
    fn try_boolean_union(
        &self,
        items: Vec<SolidShape>,
        name: &str,
    ) -> Result<SolidShape, KernelError> {
        if !check_boolean_operands(&items)? {
            return Ok(self.boolean_union(items, name));
        }
        #[cfg(feature = "occt-ffi")]
        return occt_ffi::boolean_union(items, name).map_err(Into::into);
        #[cfg(not(feature = "occt-ffi"))]
        Err(KernelError::BackendUnavailable)
    }

    fn try_boolean_subtract(
        &self,
        base: SolidShape,
        tools: Vec<SolidShape>,
    ) -> Result<SolidShape, KernelError> {
        if !check_boolean_operands(std::iter::once(&base).chain(&tools))? {
            return Ok(self.boolean_subtract(base, tools));
        }
        #[cfg(feature = "occt-ffi")]
        return occt_ffi::boolean_subtract(base, tools).map_err(Into::into);
        #[cfg(not(feature = "occt-ffi"))]
        Err(KernelError::BackendUnavailable)
    }

    fn try_boolean_union_detailed(
        &self,
        items: Vec<SolidShape>,
        name: &str,
    ) -> Result<SolidShape, KernelOperationError> {
        self.try_boolean_union(items, name).map_err(Into::into)
    }

    fn try_boolean_subtract_detailed(
        &self,
        base: SolidShape,
        tools: Vec<SolidShape>,
    ) -> Result<SolidShape, KernelOperationError> {
        self.try_boolean_subtract(base, tools).map_err(Into::into)
    }

    fn try_fillet(
        &self,
        shape: &SolidShape,
        radius: f64,
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        self.fillet(shape, radius, span)
            .map_err(KernelError::OperationFailed)
            .map_err(Into::into)
    }

    fn try_fillet_edges(
        &self,
        shape: &SolidShape,
        radius: f64,
        edge_indices: &[usize],
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        if edge_indices.is_empty() {
            return self.try_fillet(shape, radius, span);
        }
        #[cfg(feature = "occt-ffi")]
        if matches!(shape.payload, KernelPayload::Occt(_)) {
            let native_edges = occt_kernel::resolve_native_edge_indices(shape, edge_indices)?;
            return occt_ffi::dress_up(
                shape,
                DressUpKind::Fillet,
                radius,
                Some(&native_edges),
                span,
            )
            .map_err(Into::into);
        }
        Err(KernelError::OperationFailed(
            "local edge-indexed fillet is not supported by the active kernel; use the OCCT kernel (--features occt-ffi)".into(),
        )
        .into())
    }

    fn try_chamfer(
        &self,
        shape: &SolidShape,
        distance: f64,
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        self.chamfer(shape, distance, span)
            .map_err(KernelError::OperationFailed)
            .map_err(Into::into)
    }

    fn try_chamfer_edges(
        &self,
        shape: &SolidShape,
        distance: f64,
        edge_indices: &[usize],
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        if edge_indices.is_empty() {
            return self.try_chamfer(shape, distance, span);
        }
        #[cfg(feature = "occt-ffi")]
        if matches!(shape.payload, KernelPayload::Occt(_)) {
            let native_edges = occt_kernel::resolve_native_edge_indices(shape, edge_indices)?;
            return occt_ffi::dress_up(
                shape,
                DressUpKind::Chamfer,
                distance,
                Some(&native_edges),
                span,
            )
            .map_err(Into::into);
        }
        Err(KernelError::OperationFailed(
            "local edge-indexed chamfer is not supported by the active kernel; use the OCCT kernel (--features occt-ffi)".into(),
        )
        .into())
    }

    fn try_shell(
        &self,
        shape: &SolidShape,
        thickness: f64,
        open_normals: &[[f64; 3]],
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        self.shell(shape, thickness, open_normals, span)
            .map_err(KernelError::OperationFailed)
            .map_err(Into::into)
    }

    /// Fallible box construction. Invalid parameters and failed constructions
    /// are errors; an empty or substituted solid is never returned.
    fn try_make_box(
        &self,
        size: [f64; 3],
        origin: [f64; 3],
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        validate_box_params(size, origin)?;
        constructed_or_error(self.make_box(size, origin, span), "box")
    }

    /// Fallible cylinder construction that keeps `origin` and `axis` exactly.
    fn try_make_cylinder(
        &self,
        diameter: f64,
        height: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        validate_cylinder_params(diameter, height, origin, axis)?;
        constructed_or_error(
            self.make_cylinder(diameter, height, origin, axis, true, span),
            "cylinder",
        )
    }

    /// Fallible single-sided (signed `height`) or symmetric extrusion.
    fn try_extrude(
        &self,
        plane: SketchPlane,
        polygon_2d: &[[f64; 2]],
        height: f64,
        symmetric: bool,
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        validate_occt_bridge_extrude_polygon_params(plane, polygon_2d, height)
            .map_err(KernelError::OperationFailed)?;
        constructed_or_error(
            self.extrude(plane, polygon_2d, height, symmetric, span),
            "extrude",
        )
    }

    /// Fallible revolution about `axis` through the world origin.
    fn try_revolve(
        &self,
        plane: SketchPlane,
        polygon_2d: &[[f64; 2]],
        axis: [f64; 3],
        angle_deg: f64,
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        validate_revolve_params(polygon_2d, axis, angle_deg)?;
        constructed_or_error(
            self.revolve(plane, polygon_2d, axis, angle_deg, span),
            "revolve",
        )
    }

    fn transform(&self, shape: &SolidShape, matrix: Mat4) -> Result<SolidShape, KernelError> {
        try_transform_solid(shape, matrix)
    }

    fn import_exact(
        &self,
        bytes: &[u8],
        format: OcctImportFormat,
        options: &OcctImportOptions,
        component: Option<&str>,
        allow_open_shell: bool,
        span: Span,
    ) -> Result<(SolidShape, OcctImportMetadata), KernelError> {
        import_occt_bytes(bytes, format, options, component, allow_open_shell, span)
    }

    /// Opt in to cross-evaluation exact import caching only if import results are
    /// interchangeable between all kernel instances using this namespace.
    fn exact_import_cache_namespace(&self) -> Option<&'static str> {
        None
    }

    fn scope_import_instance(&self, shape: &mut SolidShape) {
        scope_native_import_instance(shape);
    }

    fn through_hole_radii(&self, shape: &SolidShape, face_indices: &[usize]) -> Option<Vec<f64>> {
        through_hole_radii(shape, face_indices)
    }

    fn offset_through_hole(
        &self,
        shape: &SolidShape,
        face_index: usize,
        delta_mm: f64,
        span: Span,
    ) -> Result<SolidShape, KernelError> {
        try_offset_through_hole(shape, face_index, delta_mm, span)
    }

    fn move_through_hole(
        &self,
        shape: &SolidShape,
        face_index: usize,
        offset_mm: [f64; 3],
        span: Span,
    ) -> Result<SolidShape, KernelError> {
        try_move_through_hole(shape, face_index, offset_mm, span)
    }

    fn delete_through_hole(
        &self,
        shape: &SolidShape,
        face_index: usize,
        span: Span,
    ) -> Result<SolidShape, KernelError> {
        try_delete_through_hole(shape, face_index, span)
    }

    fn check_interference(
        &self,
        a: &SolidShape,
        b: &SolidShape,
        min_clearance_mm: f64,
    ) -> Result<Option<InterferenceViolation>, KernelError> {
        check_interference(a, b, min_clearance_mm)
    }

    fn measure_symmetric_difference(
        &self,
        a: &SolidShape,
        b: &SolidShape,
    ) -> Result<SymmetricDifference, KernelError> {
        measure_symmetric_difference(a, b)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AnalyticKernel;

impl AnalyticKernel {
    pub fn new() -> Self {
        Self
    }
}

impl GeometryKernel for AnalyticKernel {
    fn try_boolean_subtract(
        &self,
        base: SolidShape,
        tools: Vec<SolidShape>,
    ) -> Result<SolidShape, KernelError> {
        if !check_boolean_operands(std::iter::once(&base).chain(&tools))? {
            let base_had_faces = !base.faces.is_empty();
            let result = self.boolean_subtract(base, tools);
            if base_had_faces && result.faces.is_empty() && result.csg.compute_volume() > 1e-6 {
                return Err(KernelError::OperationFailed(
                    "analytic boolean::subtract does not support edge-grazing cylindrical cuts whose centerline lies outside the base solid; use the OCCT kernel (--features occt-ffi)".into(),
                ));
            }
            return Ok(result);
        }
        #[cfg(feature = "occt-ffi")]
        return occt_ffi::boolean_subtract(base, tools).map_err(Into::into);
        #[cfg(not(feature = "occt-ffi"))]
        Err(KernelError::BackendUnavailable)
    }

    #[cfg(feature = "occt-ffi")]
    fn try_boolean_union_detailed(
        &self,
        items: Vec<SolidShape>,
        name: &str,
    ) -> Result<SolidShape, KernelOperationError> {
        if check_boolean_operands(&items)? {
            return occt_ffi::boolean_union(items, name);
        }
        Ok(self.boolean_union(items, name))
    }

    #[cfg(feature = "occt-ffi")]
    fn try_boolean_subtract_detailed(
        &self,
        base: SolidShape,
        tools: Vec<SolidShape>,
    ) -> Result<SolidShape, KernelOperationError> {
        if check_boolean_operands(std::iter::once(&base).chain(&tools))? {
            return occt_ffi::boolean_subtract(base, tools);
        }
        self.try_boolean_subtract(base, tools).map_err(Into::into)
    }

    fn try_fillet(
        &self,
        shape: &SolidShape,
        radius: f64,
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        #[cfg(feature = "occt-ffi")]
        if matches!(shape.payload, KernelPayload::Occt(_)) {
            return occt_ffi::fillet(shape, radius, span);
        }
        fillet_shape(shape, radius, span)
            .map_err(KernelError::OperationFailed)
            .map_err(Into::into)
    }

    fn try_chamfer(
        &self,
        shape: &SolidShape,
        distance: f64,
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        #[cfg(feature = "occt-ffi")]
        if matches!(shape.payload, KernelPayload::Occt(_)) {
            return occt_ffi::chamfer(shape, distance, span);
        }
        chamfer_shape(shape, distance, span)
            .map_err(KernelError::OperationFailed)
            .map_err(Into::into)
    }

    fn try_shell(
        &self,
        shape: &SolidShape,
        thickness: f64,
        open_normals: &[[f64; 3]],
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        #[cfg(feature = "occt-ffi")]
        if matches!(shape.payload, KernelPayload::Occt(_)) {
            let normal = match open_normals {
                [] => [0.0, 0.0, 1.0],
                [normal] => *normal,
                _ => {
                    return Err(KernelError::OperationFailed(
                        "OCCT shell requires a single opening face".into(),
                    )
                    .into());
                }
            };
            return occt_ffi::shell(shape, thickness, normal, span);
        }
        shell_shape(shape, thickness, open_normals, span)
            .map_err(KernelError::OperationFailed)
            .map_err(Into::into)
    }

    fn backend_name(&self) -> &'static str {
        "rust-brep-mvp"
    }

    fn exact_import_cache_namespace(&self) -> Option<&'static str> {
        Some("cad-kernel-occt/default")
    }

    fn make_box(&self, size: [f64; 3], origin: [f64; 3], span: Span) -> SolidShape {
        make_box_shape(size, origin, span)
    }

    fn make_cylinder(
        &self,
        diameter: f64,
        height: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        exact_brep: bool,
        span: Span,
    ) -> SolidShape {
        make_cylinder_shape(diameter, height, origin, axis, exact_brep, span)
    }

    fn extrude(
        &self,
        plane: SketchPlane,
        polygon_2d: &[[f64; 2]],
        height: f64,
        symmetric: bool,
        span: Span,
    ) -> SolidShape {
        let (off_min, off_max) = if symmetric {
            (-0.5 * height, 0.5 * height)
        } else if height >= 0.0 {
            (0.0, height)
        } else {
            (height, 0.0)
        };
        extrude_polygon_shape(plane, polygon_2d, off_min, off_max, span)
    }

    fn revolve(
        &self,
        plane: SketchPlane,
        polygon_2d: &[[f64; 2]],
        axis: [f64; 3],
        angle_deg: f64,
        span: Span,
    ) -> SolidShape {
        revolve_polygon_shape(plane, polygon_2d, axis, angle_deg, span)
    }

    fn fillet(&self, shape: &SolidShape, radius: f64, span: Span) -> Result<SolidShape, String> {
        // Native solids have no analytic CSG to round; use the exact OCCT fillet.
        #[cfg(feature = "occt-ffi")]
        if matches!(shape.payload, KernelPayload::Occt(_)) {
            return occt_ffi::dress_up(shape, DressUpKind::Fillet, radius, None, span)
                .map_err(|err| err.to_string());
        }
        fillet_shape(shape, radius, span)
    }

    fn chamfer(&self, shape: &SolidShape, dist: f64, span: Span) -> Result<SolidShape, String> {
        #[cfg(feature = "occt-ffi")]
        if matches!(shape.payload, KernelPayload::Occt(_)) {
            return occt_ffi::dress_up(shape, DressUpKind::Chamfer, dist, None, span)
                .map_err(|err| err.to_string());
        }
        chamfer_shape(shape, dist, span)
    }

    fn shell(
        &self,
        shape: &SolidShape,
        thickness: f64,
        open_normals: &[[f64; 3]],
        span: Span,
    ) -> Result<SolidShape, String> {
        #[cfg(feature = "occt-ffi")]
        if matches!(shape.payload, KernelPayload::Occt(_)) {
            return occt_ffi::shell_open_normals(shape, thickness, open_normals, span)
                .map_err(|err| err.to_string());
        }
        shell_shape(shape, thickness, open_normals, span)
    }

    fn sweep(
        &self,
        profile_2d: &[[f64; 2]],
        path_3d: &[[f64; 3]],
        span: Span,
    ) -> Result<SolidShape, String> {
        sweep_profile_along_path(profile_2d, path_3d, span)
    }

    fn loft(
        &self,
        sections: &[(Vec<[f64; 2]>, SketchPlane, f64)],
        span: Span,
    ) -> Result<SolidShape, String> {
        loft_sections_shape(sections, span)
    }

    fn mirror(&self, shape: &SolidShape, plane: SketchPlane, merge: bool) -> SolidShape {
        mirror_shape(shape, plane, merge)
    }

    fn hole_tapped(
        &self,
        spec: &str,
        depth: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        span: Span,
    ) -> Result<SolidShape, String> {
        let (drill_d, clean_spec) = tap_drill_diameter_mm(spec)?;
        if !depth.is_finite() || depth <= 0.0 || origin.iter().any(|v| !v.is_finite()) {
            return Err("tapped hole requires finite coordinates and a positive depth".into());
        }
        let length_sq: f64 = axis.into_iter().map(|v| v * v).sum();
        if !length_sq.is_finite() || length_sq <= 1e-12 {
            return Err("tapped hole requires a nonzero finite axis".into());
        }
        let mut s = make_cylinder_shape(drill_d, depth, origin, axis, true, span);
        let tag = format!("tapped_hole:{clean_spec}");
        for f in &mut s.faces {
            if f.surface_type == SurfaceType::Cylinder {
                f.provenance_tags.push(tag.clone());
            }
        }
        s.pmi.push(PmiRecord {
            kind: "pmi::thread".to_string(),
            label: clean_spec,
            tolerance_mm: None,
            datum: None,
            target_tags: vec![tag],
            span,
        });
        Ok(s)
    }

    fn hole_simple(
        &self,
        diameter: f64,
        depth: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        span: Span,
    ) -> SolidShape {
        make_cylinder_shape(diameter, depth, origin, axis, true, span)
    }

    fn hole_counterbore(
        &self,
        d_through: f64,
        d_cbore: f64,
        depth_cbore: f64,
        depth_through: f64,
        origin: [f64; 3],
        span: Span,
    ) -> SolidShape {
        make_counterbore_shape(d_through, d_cbore, depth_cbore, depth_through, origin, span)
    }

    fn array_linear(
        &self,
        feature: &SolidShape,
        axis: [f64; 3],
        count: usize,
        spacing: f64,
    ) -> SolidShape {
        if count == 0 {
            return SolidShape::empty("empty_linear_array");
        }
        let mut instances = Vec::with_capacity(count);
        for i in 0..count {
            let d = (i as f64) * spacing;
            let mat = Mat4::translate(axis[0] * d, axis[1] * d, axis[2] * d);
            instances.push(transform_solid(feature, mat));
        }
        union_solids(instances, &format!("{}_linear_array", feature.part_name))
    }

    fn array_polar(
        &self,
        feature: &SolidShape,
        axis: [f64; 3],
        count: usize,
        radius: Option<f64>,
        start_angle_deg: f64,
    ) -> SolidShape {
        if count == 0 {
            return SolidShape::empty("empty_polar_array");
        }
        let mut instances = Vec::with_capacity(count);
        let feat_bb = feature.csg.bbox();
        let feat_cx = 0.5 * (feat_bb.min[0] + feat_bb.max[0]);
        let feat_cy = 0.5 * (feat_bb.min[1] + feat_bb.max[1]);
        let needs_radial_offset =
            radius.is_some() && (feat_cx * feat_cx + feat_cy * feat_cy).sqrt() < 1e-3;

        for i in 0..count {
            let angle_deg = start_angle_deg + (360.0 * i as f64) / (count as f64);
            let rot = Mat4::rotate_axis(axis, angle_deg.to_radians());
            let mat = if needs_radial_offset {
                let r = radius.unwrap();
                rot.mul_mat(Mat4::translate(r, 0.0, 0.0))
            } else {
                rot
            };
            instances.push(transform_solid(feature, mat));
        }
        union_solids(instances, &format!("{}_polar_array", feature.part_name))
    }

    fn boolean_union(&self, items: Vec<SolidShape>, name: &str) -> SolidShape {
        union_solids(items, name)
    }

    fn boolean_subtract(&self, base: SolidShape, tools: Vec<SolidShape>) -> SolidShape {
        subtract_solids(base, tools)
    }

    /// R03: Checks finite coordinates, positive volume, non-empty face boundary loops,
    /// and exactly one half-edge in each direction per undirected mesh edge.
    fn check_solid(&self, shape: &SolidShape) -> CheckReport {
        #[cfg(feature = "occt-ffi")]
        if let KernelPayload::Occt(handle) = &shape.payload {
            return handle.report(shape);
        }
        if matches!(shape.payload, KernelPayload::TriMesh) {
            let watertight = shape.csg.is_closed_mesh();
            return CheckReport {
                watertight,
                volume_mm3: if !watertight {
                    0.0
                } else if shape.components.is_empty() {
                    shape.csg.compute_volume()
                } else {
                    shape.components.iter().map(|part| part.volume_mm3).sum()
                },
                surface_area_mm2: shape.faces.iter().map(|f| f.area_mm2).sum(),
                bbox_mm: shape.csg.bbox(),
                face_count: shape.faces.len(),
                edge_count: shape.edges.len(),
                // The mesh importer supplies manifold status, not an edge count.
                open_edge_count: 0,
                issues: if watertight {
                    vec![]
                } else {
                    vec![KernelError::OpenShell.to_string()]
                },
            };
        }
        let mut issues = Vec::new();
        let bbox_mm = shape.csg.bbox();
        let volume_mm3 = if shape.components.is_empty() {
            shape.csg.compute_volume()
        } else {
            shape.components.iter().map(|c| c.volume_mm3).sum()
        };
        let surface_area_mm2: f64 = shape.faces.iter().map(|f| f.area_mm2).sum();

        let mut has_nan = false;
        let mut missing_loop = false;
        type VertexKey = (i64, i64, i64);
        let mut edge_counts: HashMap<(VertexKey, VertexKey), [usize; 2]> = HashMap::new();

        let quant = |x: f64, y: f64, z: f64| -> (i64, i64, i64) {
            (
                (x * 10_000.0).round() as i64,
                (y * 10_000.0).round() as i64,
                (z * 10_000.0).round() as i64,
            )
        };

        for f in &shape.faces {
            if f.vertices.iter().any(|v| !v.is_finite()) {
                has_nan = true;
                break;
            }
            if f.outer_loop.len() < 3 || f.indices.is_empty() {
                missing_loop = true;
            }
            let v_count = f.vertices.len() / 3;
            for tri in f.indices.chunks_exact(3) {
                let i0 = tri[0] as usize;
                let i1 = tri[1] as usize;
                let i2 = tri[2] as usize;
                if i0 >= v_count || i1 >= v_count || i2 >= v_count {
                    continue;
                }
                let k0 = quant(
                    f.vertices[i0 * 3],
                    f.vertices[i0 * 3 + 1],
                    f.vertices[i0 * 3 + 2],
                );
                let k1 = quant(
                    f.vertices[i1 * 3],
                    f.vertices[i1 * 3 + 1],
                    f.vertices[i1 * 3 + 2],
                );
                let k2 = quant(
                    f.vertices[i2 * 3],
                    f.vertices[i2 * 3 + 1],
                    f.vertices[i2 * 3 + 2],
                );
                for (a, b) in [(k0, k1), (k1, k2), (k2, k0)] {
                    if a < b {
                        edge_counts.entry((a, b)).or_default()[0] += 1;
                    } else if b < a {
                        edge_counts.entry((b, a)).or_default()[1] += 1;
                    }
                }
            }
        }

        let mut open_edge_count = 0usize;
        let mut non_manifold_edge_count = 0usize;
        for &[forward_count, reverse_count] in edge_counts.values() {
            open_edge_count += forward_count.abs_diff(reverse_count);
            if forward_count > 1 || reverse_count > 1 {
                non_manifold_edge_count += 1;
            }
        }

        if has_nan {
            issues.push("mesh contains non-finite coordinates".to_string());
        }
        if shape.faces.is_empty() {
            issues.push("solid has zero B-Rep faces".to_string());
        }
        if missing_loop {
            issues.push("face has empty or degenerate boundary loop".to_string());
        }
        if open_edge_count > 0 {
            issues.push(format!(
                "open boundary shell: {open_edge_count} unpaired directed half-edge(s)"
            ));
        }
        if non_manifold_edge_count > 0 {
            issues.push(format!(
                "non-manifold shell: {non_manifold_edge_count} edge(s) have multiple incident half-edges in the same direction"
            ));
        }
        let internal_union_face_count = count_unstitched_union_internal_faces(shape);
        if internal_union_face_count > 0 {
            issues.push(format!(
                "non-manifold union: {internal_union_face_count} internal penetrating face(s) remain from unstitched overlapping solids (use the OCCT kernel for exact boolean union)"
            ));
        }
        if volume_mm3 <= 1e-6 {
            issues.push("solid has zero or negative volume".to_string());
        }
        let watertight = !has_nan
            && !shape.faces.is_empty()
            && !missing_loop
            && open_edge_count == 0
            && non_manifold_edge_count == 0
            && internal_union_face_count == 0
            && volume_mm3 > 1e-6;

        CheckReport {
            watertight,
            volume_mm3,
            surface_area_mm2,
            bbox_mm,
            face_count: shape.faces.len(),
            edge_count: shape.edges.len(),
            open_edge_count,
            issues,
        }
    }

    fn export_step(&self, shape: &SolidShape) -> Result<String, String> {
        export_step_ap242(shape)
    }
}

fn bboxes_overlap_positive(a: &BBox3, b: &BBox3, eps: f64) -> bool {
    (0..3).all(|k| a.min[k].max(b.min[k]) + eps < a.max[k].min(b.max[k]))
}

/// Counts faces in an analytic `CsgNode::Union` that lie strictly inside the union's
/// own volume (both sides of a face triangle are inside `shape.csg`) when at least
/// one pair of union operands has positive-volume AABB overlap.
fn collect_union_leaves<'a>(node: &'a CsgNode, out: &mut Vec<&'a CsgNode>) {
    if let CsgNode::Union(children) = node {
        for child in children {
            collect_union_leaves(child, out);
        }
    } else {
        out.push(node);
    }
}

fn count_unstitched_union_internal_faces(shape: &SolidShape) -> usize {
    let CsgNode::Union(_) = &shape.csg else {
        return 0;
    };
    if !shape.components.is_empty() {
        return 0;
    }
    let mut leaves = Vec::new();
    collect_union_leaves(&shape.csg, &mut leaves);
    if leaves.len() < 2 {
        return 0;
    }
    let bboxes: Vec<BBox3> = leaves.iter().map(|node| node.bbox()).collect();
    let has_overlap = (0..bboxes.len()).any(|i| {
        ((i + 1)..bboxes.len()).any(|j| bboxes_overlap_positive(&bboxes[i], &bboxes[j], 1e-4))
    });
    if !has_overlap {
        return 0;
    }

    let mut internal_faces = 0usize;
    for face in &shape.faces {
        // Curved tessellations (e.g. 32-gon cylinders) sit inside the exact analytic
        // surface by a sagitta of r * (1 - cos(pi/32)) ~= 0.0048 * r; probe past it
        // so exterior curved walls are not misclassified as buried internal faces.
        let probe_eps = match (face.surface_type, face.radius_hint) {
            (SurfaceType::Plane, _) => 1e-3,
            (_, Some(r)) if r.is_finite() && r > 0.0 => (0.015 * r).max(1e-2),
            _ => 1e-2,
        };
        let v_count = face.vertices.len() / 3;
        let mut face_is_internal = false;
        for tri in face.indices.chunks_exact(3) {
            let i0 = tri[0] as usize;
            let i1 = tri[1] as usize;
            let i2 = tri[2] as usize;
            if i0 >= v_count || i1 >= v_count || i2 >= v_count {
                continue;
            }
            let p0 = [
                face.vertices[i0 * 3],
                face.vertices[i0 * 3 + 1],
                face.vertices[i0 * 3 + 2],
            ];
            let p1 = [
                face.vertices[i1 * 3],
                face.vertices[i1 * 3 + 1],
                face.vertices[i1 * 3 + 2],
            ];
            let p2 = [
                face.vertices[i2 * 3],
                face.vertices[i2 * 3 + 1],
                face.vertices[i2 * 3 + 2],
            ];
            let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
            let e2 = [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]];
            let n = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let n_len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if !n_len.is_finite() || n_len <= 1e-8 {
                continue;
            }
            let inv = 1.0 / n_len;
            let u = [n[0] * inv, n[1] * inv, n[2] * inv];
            let c = [
                (p0[0] + p1[0] + p2[0]) / 3.0,
                (p0[1] + p1[1] + p2[1]) / 3.0,
                (p0[2] + p1[2] + p2[2]) / 3.0,
            ];
            let front = [
                c[0] + u[0] * probe_eps,
                c[1] + u[1] * probe_eps,
                c[2] + u[2] * probe_eps,
            ];
            let back = [
                c[0] - u[0] * probe_eps,
                c[1] - u[1] * probe_eps,
                c[2] - u[2] * probe_eps,
            ];
            if shape.csg.contains(front) && shape.csg.contains(back) {
                face_is_internal = true;
                break;
            }
        }
        if face_is_internal {
            internal_faces += 1;
        }
    }
    internal_faces
}

/// Complete list of C ABI operator symbol names exposed by `cpp/occt-bridge`.
pub const OCCT_BRIDGE_SUPPORTED_OPS: &[&str] = &[
    "occt_bridge_version",
    "occt_bridge_take_last_error",
    "occt_bridge_clear_last_error",
    "occt_bridge_make_box",
    "occt_bridge_make_cylinder",
    "occt_bridge_boolean_cut",
    "occt_bridge_boolean_union",
    "occt_bridge_boolean_intersect",
    "occt_bridge_fillet_all",
    "occt_bridge_chamfer_all",
    "occt_bridge_fillet_edges",
    "occt_bridge_chamfer_edges",
    "occt_bridge_shell",
    "occt_bridge_extrude_polygon",
    "occt_bridge_check",
    "occt_bridge_export_step",
    "occt_bridge_free",
    "occt_bridge_import_file",
    "occt_bridge_get_components",
    "occt_bridge_mesh_and_query_counts",
    "occt_bridge_extract_tessellation",
    "occt_bridge_transform",
    "occt_bridge_revolve_polygon",
    "occt_bridge_make_cylinder_axis",
    "occt_bridge_sweep_polygon",
    "occt_bridge_loft_polygons",
];

/// Descriptor for a single C++20 OpenCASCADE dynamic FFI operator contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcctBridgeOpDescriptor {
    pub symbol: &'static str,
    pub category: &'static str,
    pub mutates_registry: bool,
}

/// Structured descriptors for all supported `occt-bridge` C ABI operators.
pub const OCCT_BRIDGE_OP_DESCRIPTORS: &[OcctBridgeOpDescriptor] = &[
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_take_last_error",
        category: "meta",
        mutates_registry: false,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_clear_last_error",
        category: "meta",
        mutates_registry: false,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_version",
        category: "meta",
        mutates_registry: false,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_make_box",
        category: "primitive",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_make_cylinder",
        category: "primitive",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_boolean_cut",
        category: "boolean",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_boolean_union",
        category: "boolean",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_boolean_intersect",
        category: "boolean",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_fillet_all",
        category: "dressup",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_chamfer_all",
        category: "dressup",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_fillet_edges",
        category: "dressup",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_chamfer_edges",
        category: "dressup",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_shell",
        category: "dressup",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_extrude_polygon",
        category: "sketch_feature",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_check",
        category: "inspection",
        mutates_registry: false,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_export_step",
        category: "exchange",
        mutates_registry: false,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_free",
        category: "lifecycle",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_import_file",
        category: "exchange",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_get_components",
        category: "assembly",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_mesh_and_query_counts",
        category: "tessellation",
        mutates_registry: false,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_extract_tessellation",
        category: "tessellation",
        mutates_registry: false,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_transform",
        category: "placement",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_revolve_polygon",
        category: "sketch_feature",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_make_cylinder_axis",
        category: "primitive",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_sweep_polygon",
        category: "sketch_feature",
        mutates_registry: true,
    },
    OcctBridgeOpDescriptor {
        symbol: "occt_bridge_loft_polygons",
        category: "sketch_feature",
        mutates_registry: true,
    },
];

/// Returns true if the given C ABI symbol name is supported by `cpp/occt-bridge`.
pub fn is_occt_bridge_op_supported(symbol: &str) -> bool {
    OCCT_BRIDGE_SUPPORTED_OPS.contains(&symbol)
}

/// Converts a `SketchPlane` to the C ABI `plane_code` expected by `occt_bridge_extrude_polygon`.
pub const fn sketch_plane_to_occt_code(plane: SketchPlane) -> i32 {
    match plane {
        SketchPlane::Xy => 0,
        SketchPlane::Xz => 1,
        SketchPlane::Yz => 2,
    }
}

/// Converts a C ABI `plane_code` (`0 = XY`, `1 = XZ`, `2 = YZ`) into a `SketchPlane`.
pub fn occt_code_to_sketch_plane(plane_code: i32) -> Result<SketchPlane, String> {
    match plane_code {
        0 => Ok(SketchPlane::Xy),
        1 => Ok(SketchPlane::Xz),
        2 => Ok(SketchPlane::Yz),
        other => Err(format!(
            "invalid occt_bridge plane_code {other}: expected 0 (XY), 1 (XZ), or 2 (YZ)"
        )),
    }
}

/// Pre-validates non-null shape handles for binary boolean operators (`cut`, `union`, `intersect`).
pub fn validate_occt_bridge_boolean_handles(
    base_handle: u64,
    tool_handle: u64,
) -> Result<(), String> {
    if base_handle == 0 {
        return Err("occt_bridge boolean base_handle must be non-zero".to_string());
    }
    if tool_handle == 0 {
        return Err("occt_bridge boolean tool_handle must be non-zero".to_string());
    }
    Ok(())
}

/// Pre-validates handle and positive radius/distance for `occt_bridge_fillet_all` and `occt_bridge_chamfer_all`.
pub fn validate_occt_bridge_edge_dressup(
    handle: u64,
    value_mm: f64,
    op_name: &str,
) -> Result<(), String> {
    if handle == 0 {
        return Err(format!("{op_name} requires a non-zero shape handle"));
    }
    if !value_mm.is_finite() || value_mm <= 0.0 {
        return Err(format!(
            "{op_name} requires a finite positive dimension in mm (got {value_mm})"
        ));
    }
    Ok(())
}

/// Pre-validates handle, positive wall thickness, and non-zero opening normal for `occt_bridge_shell`.
pub fn validate_occt_bridge_shell_params(
    handle: u64,
    thickness_mm: f64,
    open_normal: [f64; 3],
) -> Result<[f64; 3], String> {
    if handle == 0 {
        return Err("occt_bridge_shell requires a non-zero shape handle".to_string());
    }
    if !thickness_mm.is_finite() || thickness_mm <= 0.0 {
        return Err(format!(
            "occt_bridge_shell requires a finite positive thickness (got {thickness_mm})"
        ));
    }
    if open_normal.iter().any(|v| !v.is_finite()) {
        return Err("occt_bridge_shell requires finite opening normal coordinates".to_string());
    }
    let norm_sq = open_normal[0] * open_normal[0]
        + open_normal[1] * open_normal[1]
        + open_normal[2] * open_normal[2];
    if !norm_sq.is_finite() || norm_sq <= 1.0e-12 {
        return Err("occt_bridge_shell requires a non-zero opening normal vector".to_string());
    }
    let inv_len = 1.0 / norm_sq.sqrt();
    Ok([
        open_normal[0] * inv_len,
        open_normal[1] * inv_len,
        open_normal[2] * inv_len,
    ])
}

/// Pre-validates polygon extrusion parameters for `occt_bridge_extrude_polygon` and returns
/// the flattened `(plane_code, xy_pairs)` buffer ready for FFI invocation.
pub fn validate_occt_bridge_extrude_polygon_params(
    plane: SketchPlane,
    polygon_2d: &[[f64; 2]],
    height_mm: f64,
) -> Result<(i32, Vec<f64>), String> {
    if polygon_2d.len() < 3 {
        return Err(format!(
            "occt_bridge_extrude_polygon requires at least 3 vertices (got {})",
            polygon_2d.len()
        ));
    }
    if !height_mm.is_finite() || height_mm.abs() <= 1.0e-9 {
        return Err(format!(
            "occt_bridge_extrude_polygon requires a finite non-zero height (got {height_mm})"
        ));
    }
    let mut flat_xy = Vec::with_capacity(polygon_2d.len() * 2);
    let mut double_area = 0.0;
    for (i, pt) in polygon_2d.iter().enumerate() {
        if !pt[0].is_finite() || !pt[1].is_finite() {
            return Err(format!(
                "occt_bridge_extrude_polygon vertex {i} has non-finite coordinates"
            ));
        }
        flat_xy.push(pt[0]);
        flat_xy.push(pt[1]);
        let next = &polygon_2d[(i + 1) % polygon_2d.len()];
        double_area += pt[0] * next[1] - next[0] * pt[1];
    }
    if double_area.abs() <= 1.0e-9 {
        return Err("occt_bridge_extrude_polygon rejects zero-area collinear polygon".to_string());
    }
    Ok((sketch_plane_to_occt_code(plane), flat_xy))
}

fn invalid_construction(message: String) -> KernelOperationError {
    KernelError::OperationFailed(message).into()
}

fn all_finite(values: &[f64]) -> bool {
    values.iter().all(|value| value.is_finite())
}

fn validate_box_params(size: [f64; 3], origin: [f64; 3]) -> Result<(), KernelOperationError> {
    if !all_finite(&size) || !all_finite(&origin) || size.iter().any(|&extent| extent <= 0.0) {
        return Err(invalid_construction(format!(
            "box requires finite positive size and finite origin (got size {size:?}, origin {origin:?})"
        )));
    }
    Ok(())
}

fn validate_cylinder_params(
    diameter: f64,
    height: f64,
    origin: [f64; 3],
    axis: [f64; 3],
) -> Result<(), KernelOperationError> {
    if !all_finite(&[diameter, height]) || diameter <= 0.0 || height <= 0.0 {
        return Err(invalid_construction(format!(
            "cylinder requires a finite positive diameter and height (got d={diameter}, h={height})"
        )));
    }
    let length = axis[0].hypot(axis[1]).hypot(axis[2]);
    if !all_finite(&origin) || !length.is_finite() || length <= 1.0e-12 {
        return Err(invalid_construction(format!(
            "cylinder requires a finite origin and a finite nonzero axis (got origin {origin:?}, axis {axis:?})"
        )));
    }
    Ok(())
}

fn validate_revolve_params(
    polygon_2d: &[[f64; 2]],
    axis: [f64; 3],
    angle_deg: f64,
) -> Result<(), KernelOperationError> {
    if polygon_2d.len() < 3 || polygon_2d.iter().any(|p| !all_finite(p)) {
        return Err(invalid_construction(
            "revolve requires a finite polygon with at least 3 vertices".into(),
        ));
    }
    let length = axis[0].hypot(axis[1]).hypot(axis[2]);
    if !length.is_finite() || length <= 1.0e-12 {
        return Err(invalid_construction(format!(
            "revolve requires a finite nonzero axis (got {axis:?})"
        )));
    }
    if !angle_deg.is_finite() || angle_deg == 0.0 || angle_deg.abs() > 360.0 {
        return Err(invalid_construction(format!(
            "revolve angle must be finite, nonzero and at most 360 degrees (got {angle_deg})"
        )));
    }
    Ok(())
}

/// Reject an empty construction result instead of passing it downstream.
fn constructed_or_error(shape: SolidShape, what: &str) -> Result<SolidShape, KernelOperationError> {
    if matches!(shape.csg, CsgNode::Empty) && shape.faces.is_empty() {
        return Err(invalid_construction(format!(
            "{what} construction produced an empty solid"
        )));
    }
    Ok(shape)
}
