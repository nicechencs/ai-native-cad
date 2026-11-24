use std::fmt::Write;

use crate::SliceLayer;

/// Renders one layer in the supplied XY bounds, flipping the Y axis for SVG.
/// Closed contours share an even-odd fill so holes remain transparent.
/// Open contours are drawn as dashed lines.
pub fn export_layer_svg(
    layer: &SliceLayer,
    bbox_min_xy: [f64; 2],
    bbox_max_xy: [f64; 2],
) -> String {
    let width = bbox_max_xy[0] - bbox_min_xy[0];
    let height = bbox_max_xy[1] - bbox_min_xy[1];
    let stroke_width = width.max(height) * 0.0025;
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width} {height}\">\n\
         <title>Layer {} at {} mm</title>\n",
        layer.layer_index, layer.z_mm,
    );

    for closed in [true, false] {
        let mut path = String::new();
        for contour in layer
            .loops
            .iter()
            .filter(|contour| contour.closed == closed)
        {
            let mut points = contour.points_mm.iter();
            let Some(&[first_x, first_y]) = points.next() else {
                continue;
            };
            write!(
                path,
                "M {} {}",
                first_x - bbox_min_xy[0],
                bbox_max_xy[1] - first_y
            )
            .expect("writing to a String cannot fail");
            for &[x, y] in points {
                write!(path, " L {} {}", x - bbox_min_xy[0], bbox_max_xy[1] - y)
                    .expect("writing to a String cannot fail");
            }
            path.push_str(if closed { " Z " } else { " " });
        }
        if path.is_empty() {
            continue;
        }
        let style = if closed {
            "fill=\"#bfdbfe\" fill-rule=\"evenodd\" stroke=\"#1e3a8a\"".to_owned()
        } else {
            format!(
                "fill=\"none\" stroke=\"#a33b11\" stroke-dasharray=\"{} {}\"",
                4.0 * stroke_width,
                2.0 * stroke_width,
            )
        };
        writeln!(
            svg,
            "<path d=\"{}\" {style} stroke-width=\"{stroke_width}\" stroke-linejoin=\"round\"/>",
            path.trim_end(),
        )
        .expect("writing to a String cannot fail");
    }
    svg.push_str("</svg>\n");
    svg
}
