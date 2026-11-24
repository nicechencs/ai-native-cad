use crate::{CurveKind, Edge, FaceAdjacencyGraph, FaceId, FaceUse, SurfaceKind};
use cad_kernel_occt::{KernelError, KernelPayload, SolidShape, topology};

impl FaceAdjacencyGraph {
    /// Builds from the native B-Rep only. Analytic preview and mesh-only
    /// shapes are not promoted or approximated. Face IDs match mesh descriptors
    /// extracted for the same native handle. An assembly preview instead names
    /// and numbers each occurrence's faces independently.
    pub fn from_occt(shape: &SolidShape) -> Result<Self, KernelError> {
        let KernelPayload::Occt(handle) = &shape.payload else {
            return Err(KernelError::OperationFailed(
                "exact face adjacency requires a native OCCT B-Rep".into(),
            ));
        };
        let raw = topology::extract(handle)?;
        let faces = raw
            .faces
            .into_iter()
            .map(|kind| match kind {
                topology::SurfaceKind::Plane => SurfaceKind::Plane,
                topology::SurfaceKind::Cylinder => SurfaceKind::Cylinder,
                topology::SurfaceKind::Cone => SurfaceKind::Cone,
                topology::SurfaceKind::Sphere => SurfaceKind::Sphere,
                topology::SurfaceKind::Torus => SurfaceKind::Torus,
                topology::SurfaceKind::BSpline => SurfaceKind::BSpline,
                topology::SurfaceKind::Other => SurfaceKind::Other,
            })
            .collect();
        let edges = raw
            .edges
            .into_iter()
            .map(|edge| Edge {
                curve_kind: match edge.curve_kind {
                    topology::CurveKind::Line => CurveKind::Line,
                    topology::CurveKind::Circle => CurveKind::Circle,
                    topology::CurveKind::Ellipse => CurveKind::Ellipse,
                    topology::CurveKind::BSpline => CurveKind::BSpline,
                    topology::CurveKind::Other => CurveKind::Other,
                },
                is_degenerate: edge.is_degenerate,
                face_uses: edge
                    .face_uses
                    .into_iter()
                    .map(|use_| FaceUse {
                        face: FaceId(use_.face_index as usize),
                        is_seam: use_.is_seam,
                    })
                    .collect(),
            })
            .collect();
        Self::from_incidence(faces, edges)
            .map_err(|error| KernelError::OperationFailed(error.to_string()))
    }
}
