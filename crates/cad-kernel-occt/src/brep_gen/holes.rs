use super::sketch_features::revolve_polygon_shape;
use super::{CsgNode, SketchPlane, SolidShape, Span};

pub fn make_counterbore_shape(
    d_through: f64,
    d_cbore: f64,
    depth_cbore: f64,
    depth_through: f64,
    origin: [f64; 3],
    span: Span,
) -> SolidShape {
    let r_through = (0.5 * d_through).max(1e-4);
    let r_cbore = (0.5 * d_cbore).max(r_through);
    let z_bot = origin[2] - depth_through;
    let z_shelf = origin[2] - depth_cbore;
    let z_top = origin[2] + 1.0;

    let prof = vec![
        [0.0, z_bot],
        [r_through, z_bot],
        [r_through, z_shelf],
        [r_cbore, z_shelf],
        [r_cbore, z_top],
        [0.0, z_top],
    ];
    let mut s = revolve_polygon_shape(SketchPlane::Xz, &prof, [0.0, 0.0, 1.0], 360.0, span);
    s.part_name = "counterbore_tool".to_string();
    s.csg = CsgNode::Counterbore {
        origin,
        r_through,
        r_cbore,
        depth_through,
        depth_cbore,
    };
    s
}

/// Coarse metric tap drills for the supported M2–M20 sizes. Other explicitly
/// requested pitches use nominal minus pitch; the result is not a tooling recommendation.
pub fn tap_drill_diameter_mm(spec: &str) -> Result<(f64, String), String> {
    let invalid = || format!("invalid or unsupported metric thread specification `{spec}`");
    let clean = spec.trim();
    let upper = clean.to_ascii_uppercase();
    let body = upper.strip_prefix('M').ok_or_else(invalid)?;
    let (body, class) = body.split_once('-').map_or((body, None), |(b, c)| (b, Some(c)));
    if let Some(class) = class {
        let digits = class.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !matches!(&class[digits..], "G" | "H") {
            return Err(invalid());
        }
    }
    let (nominal, pitch) = body.split_once('X').map_or((body, None), |(n, p)| (n, Some(p)));
    let number = |value: &str| {
        if value.bytes().any(|b| !(b.is_ascii_digit() || b == b'.')) {
            return Err(invalid());
        }
        value.parse::<f64>().map_err(|_| invalid())
    };
    let nominal = number(nominal)?;
    let coarse = [
        (2.0, 0.4, 1.6), (2.5, 0.45, 2.05), (3.0, 0.5, 2.5), (3.5, 0.6, 2.9),
        (4.0, 0.7, 3.3), (5.0, 0.8, 4.2), (6.0, 1.0, 5.0), (8.0, 1.25, 6.8),
        (10.0, 1.5, 8.5), (12.0, 1.75, 10.2), (14.0, 2.0, 12.0), (16.0, 2.0, 14.0),
        (18.0, 2.5, 15.5), (20.0, 2.5, 17.5),
    ];
    let (_, coarse_pitch, coarse_drill) = coarse.into_iter()
        .find(|&(n, _, _)| (n - nominal).abs() < 1e-6).ok_or_else(invalid)?;
    let drill = match pitch.map(number).transpose()? {
        None => coarse_drill,
        Some(p) if (p - coarse_pitch).abs() < 1e-6 => coarse_drill,
        Some(p) if p > 0.0 && p < coarse_pitch => nominal - p,
        _ => return Err(invalid()),
    };
    Ok((drill, clean.to_string()))
}
