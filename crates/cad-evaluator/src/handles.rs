use super::FACE_EXTREME_TOLERANCE;
use cad_kernel_occt::{GeometryKernel, KernelPayload, SolidShape, SurfaceType};
use cad_scene_mesh::HoleOffsetHandle;
use cad_syntax::Span;

pub(super) fn hole_offset_handles(
    kernel: &dyn GeometryKernel,
    solid: &SolidShape,
    return_span: Span,
) -> Vec<HoleOffsetHandle> {
    if !matches!(solid.payload, KernelPayload::Occt(_)) {
        return Vec::new();
    }
    let cylinders: Vec<_> = solid
        .faces
        .iter()
        .enumerate()
        .filter(|(_, face)| face.surface_type == SurfaceType::Cylinder)
        .collect();
    let mut candidates = Vec::new();
    for (name, axis) in [
        ("radius", None),
        ("X", Some(0)),
        ("Y", Some(1)),
        ("Z", Some(2)),
    ] {
        let key = |face: &cad_kernel_occt::BRepFace| match axis {
            Some(axis) => face.center_hint[axis],
            None => face.radius_hint.unwrap_or(0.0),
        };
        for (method, extreme) in [("min_by", f64::min as fn(f64, f64) -> f64), ("max_by", f64::max)] {
            let Some(best) = cylinders.iter().map(|(_, face)| key(face)).reduce(extreme) else {
                continue;
            };
            let mut matches = cylinders.iter()
                .filter(|(_, face)| (key(face) - best).abs() < FACE_EXTREME_TOLERANCE);
            if let Some(&(index, _)) = matches.next()
                && matches.next().is_none()
                && !candidates.iter().any(|&(i, _, _)| i == index)
            {
                candidates.push((index, method, name));
            }
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }
    let indices: Vec<_> = candidates.iter().map(|&(index, _, _)| index).collect();
    let Some(radii) = kernel.through_hole_radii(solid, &indices) else {
        return Vec::new();
    };
    candidates.into_iter().zip(radii)
        .filter(|&(_, radius)| radius.is_finite() && radius > 0.0)
        .map(|((index, method, name), radius_mm)| HoleOffsetHandle {
            face_id: solid.faces[index].face_id.clone(),
            radius_mm,
            selector: format!(".faces().cylindrical().{method}({name})"),
            return_start: return_span.start_byte,
            expression_end: return_span.end_byte,
        })
        .collect()
}
