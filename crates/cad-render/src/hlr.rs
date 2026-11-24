use crate::ViewKind;
use cad_kernel_occt::{SolidShape, hlr::HlrProjection};
use std::collections::BTreeSet;
use std::fmt::Write;

const DEFLECTION_MM: f64 = 0.01;
// Match the OCCT confusion tolerance when suppressing wholly coincident paths.
const MERGE_MM: f64 = 1e-7;

pub(super) fn project(shape: &SolidShape, view: ViewKind) -> Result<Option<HlrProjection>, String> {
    let (toward_eye, view_x) = match view {
        ViewKind::Front => ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0]),
        ViewKind::Top => ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0]),
        ViewKind::Right => ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        // Use a right-handed camera for the engineering isometric view.
        ViewKind::Iso => ([1.0, 1.0, 1.0], [-1.0, 1.0, 0.0]),
    };
    let Some(mut projection) =
        cad_kernel_occt::hlr::compute_hlr(shape, toward_eye, view_x, DEFLECTION_MM)
            .map_err(|e| format!("{}: {e}", view.title()))?
    else {
        return Ok(None);
    };
    if projection.visible.is_empty() {
        return Err(format!(
            "{}: exact HLR produced no visible edges",
            view.title()
        ));
    }
    // OCCT may report coincident back edges independently of visible edges
    // (e.g. the two rectangular rims in a top view of a box). Keep the visible
    // instance and do not export a dashed path exactly over it.
    let mut seen = BTreeSet::new();
    for paths in [&mut projection.visible, &mut projection.hidden] {
        paths.retain(|path| {
            if path
                .iter()
                .flatten()
                .any(|x| x.abs() >= MERGE_MM * i64::MAX as f64)
            {
                return true;
            }
            let mut key: Vec<_> = path
                .iter()
                .map(|p| p.map(|x| (x / MERGE_MM).round() as i64))
                .collect();
            if key.iter().rev().lt(&key) {
                key.reverse();
            }
            seen.insert(key)
        });
    }
    Ok(Some(projection))
}

pub(super) fn project_point(view: ViewKind, point: [f64; 3]) -> [f64; 2] {
    let (x, y, _) = view.project(point);
    [if matches!(view, ViewKind::Iso) { -x } else { x }, y]
}

pub(crate) fn draw_svg(
    svg: &mut String,
    projection: &HlrProjection,
    to_svg: impl Fn([f64; 2]) -> [f64; 2],
) {
    // Draw solid lines last so they win where projected features overlap.
    // Class stays visible or hidden. Closed cut profiles are separate
    // `outline` polygons from `projected_profiles`, not relabeled HLR strokes.
    for (class, paths) in [
        ("hidden", &projection.hidden),
        ("visible", &projection.visible),
    ] {
        let _ = writeln!(svg, "<g class=\"hlr-{class}\">");
        for path in paths {
            let _ = write!(svg, "<polyline class=\"{class}\" points=\"");
            for [x, y] in path.iter().copied().map(&to_svg) {
                let _ = write!(svg, "{x:.3},{y:.3} ");
            }
            svg.push_str("\"/>\n");
        }
        svg.push_str("</g>\n");
    }
}
