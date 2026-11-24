//! Authoritative OpenCASCADE B-Rep kernel.
//!
//! Every construction, sketch feature, Boolean, dress-up, check and STEP
//! export produced by [`OcctKernel`] operates on native `KernelPayload::Occt`
//! handles built directly by the bridge (no analytic intermediate, no
//! faceted preview, no handwritten STEP). Failures are reported as errors;
//! the legacy infallible constructors of [`GeometryKernel`] log the error and
//! return an empty solid that `check_solid` rejects. No operation silently
//! returns its input or substitutes approximate geometry.

use crate::occt_ffi::{self, DressUpKind};
use crate::{
    AnalyticKernel, CheckReport, CsgNode, GeometryKernel, KernelError, KernelOperationError,
    KernelPayload, Mat4, SketchPlane, SolidShape, try_transform_solid,
};
use cad_syntax::Span;

/// OpenCASCADE-backed implementation of [`GeometryKernel`] and
/// [`crate::IBRepKernel`]. Use [`OcctKernel::is_available`] to check that the
/// loaded bridge is the real native backend rather than the mock build.
#[derive(Debug, Default, Clone, Copy)]
pub struct OcctKernel;

impl OcctKernel {
    pub fn new() -> Self {
        Self
    }

    /// Whether the linked `occt_bridge` is the real OCCT backend.
    pub fn is_available() -> bool {
        crate::brep_kernel::loaded_occt_version().is_some()
    }

    /// Fillet exactly the listed native edges (zero-based indices from
    /// [`crate::topology::extract`] of this shape).
    pub fn fillet_edges(
        &self,
        shape: &SolidShape,
        radius: f64,
        edges: &[usize],
        span: Span,
    ) -> Result<SolidShape, KernelError> {
        occt_ffi::dress_up(shape, DressUpKind::Fillet, radius, Some(edges), span)
    }

    /// Chamfer exactly the listed native edges (zero-based indices from
    /// [`crate::topology::extract`] of this shape).
    pub fn chamfer_edges(
        &self,
        shape: &SolidShape,
        distance: f64,
        edges: &[usize],
        span: Span,
    ) -> Result<SolidShape, KernelError> {
        occt_ffi::dress_up(shape, DressUpKind::Chamfer, distance, Some(edges), span)
    }

    /// Native mitred sweep of a closed profile along an open polyline.
    pub fn try_sweep(
        &self,
        profile_2d: &[[f64; 2]],
        path_3d: &[[f64; 3]],
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        occt_ffi::sweep(profile_2d, path_3d, span)
    }

    /// Native ruled loft through the sketch sections in the given order.
    pub fn try_loft(
        &self,
        sections: &[(Vec<[f64; 2]>, SketchPlane, f64)],
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        occt_ffi::loft(sections, span)
    }

    fn union_all(&self, items: Vec<SolidShape>, name: &str) -> SolidShape {
        self.try_boolean_union(items, name)
            .unwrap_or_else(|err| failed(name, "boolean union", err))
    }

    fn transformed_copies(
        &self,
        feature: &SolidShape,
        matrices: impl IntoIterator<Item = Mat4>,
    ) -> Result<Vec<SolidShape>, KernelError> {
        let native = occt_ffi::to_native(feature)?;
        matrices
            .into_iter()
            .map(|matrix| try_transform_solid(&native, matrix))
            .collect()
    }
}

/// Infallible trait methods cannot return the error. Record it and return an
/// empty solid, which never checks as watertight and cannot be exported.
fn failed(name: &str, operation: &str, err: impl std::fmt::Display) -> SolidShape {
    tracing::error!(
        component = "cad-kernel-occt",
        operation,
        part = name,
        "native OCCT {operation} failed: {err}"
    );
    SolidShape::empty(name)
}

fn is_empty_solid(shape: &SolidShape) -> bool {
    matches!(shape.payload, KernelPayload::Analytic)
        && matches!(shape.csg, CsgNode::Empty)
        && shape.faces.is_empty()
}

fn reject_mesh<'a>(items: impl IntoIterator<Item = &'a SolidShape>) -> Result<(), KernelError> {
    for item in items {
        match &item.payload {
            KernelPayload::TriMesh => return Err(KernelError::UnsupportedMixedBoolean),
            KernelPayload::Occt(handle) if handle.is_open_shell() => {
                return Err(KernelError::OpenShell);
            }
            _ => {}
        }
    }
    Ok(())
}

fn not_exact_report(shape: &SolidShape, issue: String) -> CheckReport {
    let mut report = AnalyticKernel::new().check_solid(shape);
    report.watertight = false;
    report.issues.push(issue);
    report
}

impl GeometryKernel for OcctKernel {
    fn backend_name(&self) -> &'static str {
        "occt"
    }

    fn exact_import_cache_namespace(&self) -> Option<&'static str> {
        // Same native import path as AnalyticKernel.
        Some("cad-kernel-occt/default")
    }

    fn make_box(&self, size: [f64; 3], origin: [f64; 3], span: Span) -> SolidShape {
        self.try_make_box(size, origin, span)
            .unwrap_or_else(|err| failed("box", "make_box", err))
    }

    fn make_cylinder(
        &self,
        diameter: f64,
        height: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        _exact_brep: bool,
        span: Span,
    ) -> SolidShape {
        // The native cylinder is always exact.
        self.try_make_cylinder(diameter, height, origin, axis, span)
            .unwrap_or_else(|err| failed("cylinder", "make_cylinder", err))
    }

    fn extrude(
        &self,
        plane: SketchPlane,
        polygon_2d: &[[f64; 2]],
        height: f64,
        symmetric: bool,
        span: Span,
    ) -> SolidShape {
        self.try_extrude(plane, polygon_2d, height, symmetric, span)
            .unwrap_or_else(|err| failed("extrude", "extrude", err))
    }

    fn revolve(
        &self,
        plane: SketchPlane,
        polygon_2d: &[[f64; 2]],
        axis: [f64; 3],
        angle_deg: f64,
        span: Span,
    ) -> SolidShape {
        self.try_revolve(plane, polygon_2d, axis, angle_deg, span)
            .unwrap_or_else(|err| failed("revolve", "revolve", err))
    }

    fn try_make_box(
        &self,
        size: [f64; 3],
        origin: [f64; 3],
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        occt_ffi::make_box(size, origin, span)
    }

    fn try_make_cylinder(
        &self,
        diameter: f64,
        height: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        occt_ffi::make_cylinder(diameter, height, origin, axis, span)
    }

    fn try_extrude(
        &self,
        plane: SketchPlane,
        polygon_2d: &[[f64; 2]],
        height: f64,
        symmetric: bool,
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        occt_ffi::extrude(plane, polygon_2d, height, symmetric, span)
    }

    fn try_revolve(
        &self,
        plane: SketchPlane,
        polygon_2d: &[[f64; 2]],
        axis: [f64; 3],
        angle_deg: f64,
        span: Span,
    ) -> Result<SolidShape, KernelOperationError> {
        occt_ffi::revolve(plane, polygon_2d, axis, angle_deg, span)
    }

    fn fillet(&self, shape: &SolidShape, radius: f64, span: Span) -> Result<SolidShape, String> {
        // Analytic `fillet` on a box rounds the plan-view corners and extrudes
        // the full height. `fillet_all` also rounds the top and bottom rims, so
        // a corner radius of half the plate thickness (a valid plan fillet)
        // collapses the side faces and OCCT rejects it.
        if let Some(edges) = axis_aligned_box_vertical_edges(shape) {
            let plan = shape.csg.bbox().size();
            let plan_min = plan[0].min(plan[1]);
            if plan_min > 0.0 && radius >= 0.5 * plan_min {
                return Err(format!(
                    "fillet radius {radius:.2} mm exceeds half of minimum edge-pair dimension ({:.2} mm)",
                    0.5 * plan_min
                ));
            }
            return occt_ffi::dress_up(shape, DressUpKind::Fillet, radius, Some(&edges), span)
                .map_err(|e| e.to_string());
        }
        occt_ffi::dress_up(shape, DressUpKind::Fillet, radius, None, span)
            .map_err(|e| e.to_string())
    }

    fn chamfer(&self, shape: &SolidShape, dist: f64, span: Span) -> Result<SolidShape, String> {
        occt_ffi::dress_up(shape, DressUpKind::Chamfer, dist, None, span).map_err(|e| e.to_string())
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
        let native_edges = resolve_native_edge_indices(shape, edge_indices)?;
        self.fillet_edges(shape, radius, &native_edges, span)
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
        let native_edges = resolve_native_edge_indices(shape, edge_indices)?;
        self.chamfer_edges(shape, distance, &native_edges, span)
            .map_err(Into::into)
    }

    fn shell(
        &self,
        shape: &SolidShape,
        thickness: f64,
        open_normals: &[[f64; 3]],
        span: Span,
    ) -> Result<SolidShape, String> {
        occt_ffi::shell_open_normals(shape, thickness, open_normals, span)
            .map_err(|e| e.to_string())
    }

    fn sweep(
        &self,
        profile_2d: &[[f64; 2]],
        path_3d: &[[f64; 3]],
        span: Span,
    ) -> Result<SolidShape, String> {
        self.try_sweep(profile_2d, path_3d, span)
            .map_err(|e| e.to_string())
    }

    fn loft(
        &self,
        sections: &[(Vec<[f64; 2]>, SketchPlane, f64)],
        span: Span,
    ) -> Result<SolidShape, String> {
        self.try_loft(sections, span).map_err(|e| e.to_string())
    }

    fn mirror(&self, shape: &SolidShape, plane: SketchPlane, merge: bool) -> SolidShape {
        let mirrored = match self.transformed_copies(shape, [Mat4::reflect(plane)]) {
            Ok(mut copies) => copies.remove(0),
            Err(err) => return failed(&shape.part_name, "mirror", err),
        };
        if merge {
            self.union_all(vec![shape.clone(), mirrored], &shape.part_name)
        } else {
            mirrored
        }
    }

    fn hole_tapped(
        &self,
        spec: &str,
        depth: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        span: Span,
    ) -> Result<SolidShape, String> {
        let analytic = AnalyticKernel::new().hole_tapped(spec, depth, origin, axis, span)?;
        occt_ffi::to_native(&analytic).map_err(|e| e.to_string())
    }

    fn hole_simple(
        &self,
        diameter: f64,
        depth: f64,
        origin: [f64; 3],
        axis: [f64; 3],
        span: Span,
    ) -> SolidShape {
        self.try_make_cylinder(diameter, depth, origin, axis, span)
            .unwrap_or_else(|err| failed("hole", "hole_simple", err))
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
        let analytic = crate::brep_gen::make_counterbore_shape(
            d_through,
            d_cbore,
            depth_cbore,
            depth_through,
            origin,
            span,
        );
        let name = analytic.part_name.clone();
        occt_ffi::to_native(&analytic).unwrap_or_else(|err| failed(&name, "hole_counterbore", err))
    }

    fn array_linear(
        &self,
        feature: &SolidShape,
        axis: [f64; 3],
        count: usize,
        spacing: f64,
    ) -> SolidShape {
        let name = format!("{}_linear_array", feature.part_name);
        if count == 0 {
            return SolidShape::empty("empty_linear_array");
        }
        let matrices = (0..count).map(|i| {
            let d = i as f64 * spacing;
            Mat4::translate(axis[0] * d, axis[1] * d, axis[2] * d)
        });
        match self.transformed_copies(feature, matrices) {
            Ok(instances) => self.union_all(instances, &name),
            Err(err) => failed(&name, "array_linear", err),
        }
    }

    fn array_polar(
        &self,
        feature: &SolidShape,
        axis: [f64; 3],
        count: usize,
        radius: Option<f64>,
        start_angle_deg: f64,
    ) -> SolidShape {
        let name = format!("{}_polar_array", feature.part_name);
        if count == 0 {
            return SolidShape::empty("empty_polar_array");
        }
        let bbox = feature.csg.bbox();
        let cx = 0.5 * (bbox.min[0] + bbox.max[0]);
        let cy = 0.5 * (bbox.min[1] + bbox.max[1]);
        let radial = radius.filter(|_| cx.hypot(cy) < 1e-3);
        let matrices = (0..count).map(|i| {
            let angle_deg = start_angle_deg + (360.0 * i as f64) / (count as f64);
            let rotation = Mat4::rotate_axis(axis, angle_deg.to_radians());
            match radial {
                Some(r) => rotation.mul_mat(Mat4::translate(r, 0.0, 0.0)),
                None => rotation,
            }
        });
        match self.transformed_copies(feature, matrices) {
            Ok(instances) => self.union_all(instances, &name),
            Err(err) => failed(&name, "array_polar", err),
        }
    }

    fn boolean_union(&self, items: Vec<SolidShape>, name: &str) -> SolidShape {
        self.union_all(items, name)
    }

    fn boolean_subtract(&self, base: SolidShape, tools: Vec<SolidShape>) -> SolidShape {
        let name = base.part_name.clone();
        self.try_boolean_subtract(base, tools)
            .unwrap_or_else(|err| failed(&name, "boolean subtract", err))
    }

    fn try_boolean_union(
        &self,
        items: Vec<SolidShape>,
        name: &str,
    ) -> Result<SolidShape, KernelError> {
        reject_mesh(&items)?;
        Ok(occt_ffi::boolean_union(items, name)?)
    }

    fn try_boolean_subtract(
        &self,
        base: SolidShape,
        tools: Vec<SolidShape>,
    ) -> Result<SolidShape, KernelError> {
        reject_mesh(std::iter::once(&base).chain(&tools))?;
        // An empty operand is a failed upstream construction. Subtracting it
        // would hand back the unmodified base as if the cut had succeeded.
        if is_empty_solid(&base) {
            return Err(KernelError::OperationFailed(format!(
                "boolean subtract base `{}` is empty",
                base.part_name
            )));
        }
        if let Some(tool) = tools.iter().find(|tool| is_empty_solid(tool)) {
            return Err(KernelError::OperationFailed(format!(
                "boolean subtract tool `{}` is empty; refusing to return `{}` unchanged",
                tool.part_name, base.part_name
            )));
        }
        Ok(occt_ffi::boolean_subtract(base, tools)?)
    }

    fn check_solid(&self, shape: &SolidShape) -> CheckReport {
        match &shape.payload {
            // BRepCheck_Analyzer validity, closed manifold shells, positive
            // volume and exact topology counts from the native shape.
            KernelPayload::Occt(handle) => handle.report(shape),
            KernelPayload::Analytic if is_empty_solid(shape) => {
                AnalyticKernel::new().check_solid(shape)
            }
            KernelPayload::Analytic
                if shape.exact_brep && crate::is_liftable_exact_csg(&shape.csg) =>
            {
                match occt_ffi::to_native(shape) {
                    Ok(native) => match &native.payload {
                        KernelPayload::Occt(handle) => handle.report(&native),
                        _ => not_exact_report(
                            shape,
                            "exact construction produced no native B-Rep solid".into(),
                        ),
                    },
                    Err(err) => CheckReport {
                        watertight: false,
                        volume_mm3: 0.0,
                        surface_area_mm2: 0.0,
                        bbox_mm: shape.csg.bbox(),
                        face_count: 0,
                        edge_count: 0,
                        open_edge_count: 0,
                        issues: vec![format!("native B-Rep construction failed: {err}")],
                    },
                }
            }
            KernelPayload::Analytic if occt_ffi::is_native_group(&shape.csg) => {
                // An assembly groups independent native solids; check each one.
                occt_ffi::native_group_report(shape).unwrap_or_else(|err| {
                    not_exact_report(shape, format!("native assembly check failed: {err}"))
                })
            }
            KernelPayload::Analytic => not_exact_report(
                shape,
                "faceted approximation is not an exact native B-Rep solid".into(),
            ),
            // Mesh payloads are explicitly meshes; check their mesh topology.
            KernelPayload::TriMesh => AnalyticKernel::new().check_solid(shape),
        }
    }

    fn export_step(&self, shape: &SolidShape) -> Result<String, String> {
        match &shape.payload {
            KernelPayload::Occt(_) => {
                occt_ffi::export_native_step(shape).map_err(|e| e.to_string())
            }
            KernelPayload::Analytic
                if shape.exact_brep && crate::is_liftable_exact_csg(&shape.csg) =>
            {
                occt_ffi::export_native_step(shape).map_err(|e| e.to_string())
            }
            // Assemblies of native solids are written as an XCAF assembly.
            KernelPayload::Analytic if occt_ffi::is_native_group(&shape.csg) => {
                occt_ffi::export_native_group_step(shape).map_err(|e| e.to_string())
            }
            // Never fall back to the handwritten STEP writer.
            KernelPayload::Analytic => Err(
                "native STEP export requires exact B-Rep geometry; faceted approximations and \
                 assemblies with non-native members are not exported"
                    .into(),
            ),
            KernelPayload::TriMesh => Err(KernelError::UnsupportedMixedBoolean.to_string()),
        }
    }
}

/// Map `SolidShape::edges` preview indices to zero-based native OCCT `TopExp::MapShapes`
/// edge indices via incident face-pair matching against [`crate::topology::extract`].
pub(crate) fn resolve_native_edge_indices(
    shape: &SolidShape,
    preview_edge_indices: &[usize],
) -> Result<Vec<usize>, KernelError> {
    use std::collections::BTreeSet;
    use crate::topology;

    let KernelPayload::Occt(guard) = &shape.payload else {
        return Err(KernelError::OperationFailed(
            "local edge-indexed dress-up requires a native OCCT solid".into(),
        ));
    };
    let topo = topology::extract(guard)?;
    let mut native_edges = BTreeSet::new();
    for &preview_idx in preview_edge_indices {
        let preview = shape.edges.get(preview_idx).ok_or_else(|| {
            KernelError::OperationFailed(format!(
                "preview edge index {preview_idx} is out of range (edge count {})",
                shape.edges.len()
            ))
        })?;
        if preview.polyline.len() < 6 {
            return Err(KernelError::OperationFailed(format!(
                "preview edge {preview_idx} has fewer than two endpoints"
            )));
        }
        let p = [preview.polyline[0], preview.polyline[1], preview.polyline[2]];
        let n = preview.polyline.len();
        let mut q = [preview.polyline[n - 3], preview.polyline[n - 2], preview.polyline[n - 1]];
        // Closed circular rims repeat the seam vertex at both ends. Use a mid
        // sample so incident-face mapping still sees two distinct points.
        if dist_sq(p, q) <= 1e-8 && n >= 9 {
            let point_count = n / 3;
            let mid = (point_count / 2) * 3;
            let mid = if mid >= 3 && mid + 2 < n { mid } else { 3 };
            q = [preview.polyline[mid], preview.polyline[mid + 1], preview.polyline[mid + 2]];
        }
        let incident_faces = incident_occt_faces(shape, p, q);
        if incident_faces.len() < 2 {
            return Err(KernelError::OperationFailed(format!(
                "preview edge {preview_idx} could not be mapped to two incident native B-Rep faces"
            )));
        }
        let mut matched_any = false;
        for (native_idx, edge) in topo.edges.iter().enumerate() {
            if edge.is_degenerate {
                continue;
            }
            let face_set: BTreeSet<u32> = edge
                .face_uses
                .iter()
                .filter(|u| !u.is_seam)
                .map(|u| u.face_index)
                .collect();
            if face_set.len() >= 2 && face_set.is_subset(&incident_faces) {
                native_edges.insert(native_idx);
                matched_any = true;
            }
        }
        if !matched_any {
            return Err(KernelError::OperationFailed(format!(
                "preview edge {preview_idx} did not match any sharp native OCCT edge"
            )));
        }
    }
    Ok(native_edges.into_iter().collect())
}

fn incident_occt_faces(
    shape: &SolidShape,
    p: [f64; 3],
    q: [f64; 3],
) -> std::collections::BTreeSet<u32> {
    const TOL_SQ: f64 = 1e-8;
    let mut faces = std::collections::BTreeSet::new();
    for face in &shape.faces {
        let Some(face_idx) = face
            .face_id
            .strip_prefix("occt_face_")
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let mut has_p = false;
        let mut has_q = false;
        for chunk in face.vertices.chunks_exact(3) {
            let v = [chunk[0], chunk[1], chunk[2]];
            if !has_p && dist_sq(v, p) <= TOL_SQ {
                has_p = true;
            }
            if !has_q && dist_sq(v, q) <= TOL_SQ {
                has_q = true;
            }
            if has_p && has_q {
                faces.insert(face_idx);
                break;
            }
        }
    }
    faces
}

fn dist_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    dx * dx + dy * dy + dz * dz
}

/// Four vertical edges of an axis-aligned box, in native `MapShapes` order.
///
/// Returns `None` for every other solid so general fillets still use every
/// sharp edge. Face ids `occt_face_<index>` match `topology::extract`.
fn axis_aligned_box_vertical_edges(shape: &SolidShape) -> Option<Vec<usize>> {
    use crate::SurfaceType;
    use crate::topology::{self, CurveKind};

    let KernelPayload::Occt(guard) = &shape.payload else {
        return None;
    };
    if shape.faces.len() != 6 {
        return None;
    }
    let mut axis_of_face = [None; 6];
    let mut covered = [false; 6];
    for face in &shape.faces {
        if face.surface_type != SurfaceType::Plane {
            return None;
        }
        let axis = dominant_axis(face.normal_hint)?;
        let index: usize = face.face_id.rsplit('_').next()?.parse().ok()?;
        if index >= 6 || axis_of_face[index].is_some() {
            return None;
        }
        let slot = axis * 2 + usize::from(face.normal_hint[axis] < 0.0);
        if covered[slot] {
            return None;
        }
        covered[slot] = true;
        axis_of_face[index] = Some(axis);
    }
    if covered.iter().any(|slot| !slot) {
        return None;
    }
    let topo = topology::extract(guard).ok()?;
    if topo.faces.len() != 6 || topo.edges.len() != 12 {
        return None;
    }
    let mut vertical = Vec::with_capacity(4);
    for (index, edge) in topo.edges.iter().enumerate() {
        if edge.is_degenerate || edge.curve_kind != CurveKind::Line || edge.face_uses.len() != 2 {
            return None;
        }
        let a = axis_of_face
            .get(edge.face_uses[0].face_index as usize)?
            .as_ref()?;
        let b = axis_of_face
            .get(edge.face_uses[1].face_index as usize)?
            .as_ref()?;
        if *a != 2 && *b != 2 && a != b {
            vertical.push(index);
        }
    }
    (vertical.len() == 4).then_some(vertical)
}

fn dominant_axis(normal: [f64; 3]) -> Option<usize> {
    let mut axis = 0;
    for index in 1..3 {
        if normal[index].abs() > normal[axis].abs() {
            axis = index;
        }
    }
    if normal[axis].abs() < 1.0 - 1e-6 {
        return None;
    }
    if (0..3).any(|index| index != axis && normal[index].abs() > 1e-6) {
        return None;
    }
    Some(axis)
}
