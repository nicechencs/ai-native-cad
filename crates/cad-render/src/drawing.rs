use super::{ViewKind, bbox_corners};
use cad_kernel_occt::{BBox3, CheckReport, PmiRecord, SolidShape};
use std::fmt::Write;

#[derive(Debug, Clone, Copy, Default)]
pub struct DrawingOptions<'a> {
    pub material: Option<&'a str>,
    pub manufacturing: Option<&'a str>,
    pub additional_pmi: &'a [PmiRecord],
}

/// Produces a third-angle engineering sheet, with a common orthographic scale.
/// Views show projected outlines and the available B-Rep edge polylines.
pub fn render_drawing_svg(
    shape: &SolidShape,
    check: &CheckReport,
    options: DrawingOptions<'_>,
) -> String {
    let mut pmi: Vec<_> = shape.pmi.iter().chain(options.additional_pmi).collect();
    pmi.dedup();
    let pmi_rows: Vec<_> = pmi
        .into_iter()
        .map(|r| {
            let kind = r.kind.strip_prefix("pmi::").unwrap_or(&r.kind);
            let value = r
                .tolerance_mm
                .map(|v| {
                    if kind == "thread" {
                        format!("Drill Ø{v:.3} mm")
                    } else {
                        format!("{v:.3} mm")
                    }
                })
                .unwrap_or_default();
            vec![
                kind.into(),
                r.label.clone(),
                value,
                r.datum.clone().unwrap_or_default(),
                r.target_tags.join(", "),
            ]
        })
        .collect();
    let bom_rows: Vec<_> = shape
        .components
        .iter()
        .enumerate()
        .map(|(i, c)| {
            vec![
                (i + 1).to_string(),
                c.component_id.clone(),
                c.part_name.clone(),
                format!("{:.2}", c.volume_mm3),
            ]
        })
        .collect();
    let table_space = [pmi_rows.len(), bom_rows.len()]
        .iter()
        .map(|&n| if n == 0 { 0 } else { 14 + 7 * n })
        .sum::<usize>() as f64;
    let height = (254.0 + table_space + 42.0).max(297.0);
    let mut svg = String::new();
    let _ = writeln!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="420mm" height="{height}mm" viewBox="0 0 420 {height}" role="img">"#
    );
    let _ = writeln!(
        svg,
        "<title>{} engineering drawing</title>",
        escape_xml(&shape.part_name)
    );
    svg.push_str(r##"<defs><marker id="arrow" markerWidth="4" markerHeight="4" refX="3" refY="2" orient="auto-start-reverse" markerUnits="userSpaceOnUse"><path d="M0 0.7 L3 2 L0 3.3" fill="none" stroke="#324050" stroke-width="0.35"/></marker></defs>
<style>text{font-family:Arial,sans-serif;fill:#142130;font-size:3.6px}.title{font-size:6px;font-weight:bold}.heading{font-weight:bold}.minor{font-size:3.2px}.frame,.table-line,.edge,.outline,.extension,.dimension{fill:none;stroke:#142130}.frame{stroke-width:.55}.table-line{stroke-width:.25}.edge,.outline{stroke-linejoin:round;stroke-linecap:round}.edge{stroke:#687584;stroke-width:.28}.outline{stroke-width:.45}.extension,.dimension{stroke:#324050;stroke-width:.25}.dimension{marker-start:url(#arrow);marker-end:url(#arrow)}</style>
<rect width="100%" height="100%" fill="white"/>"##);
    let _ = writeln!(
        svg,
        r#"<rect class="frame" x="10" y="10" width="400" height="{}"/>"#,
        height - 20.0
    );
    let _ = writeln!(
        svg,
        "<text class=\"title\" x=\"16\" y=\"22\">{}</text>",
        escape_xml(&shape.part_name)
    );
    text(
        &mut svg,
        16.0,
        29.0,
        "Third-angle projection · Orthographic wireframe",
    );

    let bbox = check.bbox_mm;
    let size = bbox.size();
    let scale = fit_scale(
        (154.0 / size[0].max(size[1]).max(1e-9)).min(66.0 / size[1].max(size[2]).max(1e-9)),
    );
    let projected = [
        ("top", ViewKind::Top, [108.0, 87.0], scale, 42.0),
        ("front", ViewKind::Front, [108.0, 195.0], scale, 150.0),
        ("right", ViewKind::Right, [309.0, 195.0], scale, 150.0),
        ("iso", ViewKind::Iso, [309.0, 87.0], iso_scale(bbox), 42.0),
    ];
    for (id, view, center, s, label_y) in projected {
        let _ = writeln!(svg, r#"<g id="view-{id}">"#);
        let _ = writeln!(
            svg,
            r#"<text class="heading" x="{}" y="{label_y}">{}{}</text>"#,
            center[0] - 92.0,
            view.title(),
            if id == "iso" { " · NTS" } else { "" }
        );
        let bounds = draw_view(&mut svg, shape, bbox, view, center, s);
        match view {
            ViewKind::Front => horizontal_dimension(&mut svg, bounds, size[0], "Length"),
            ViewKind::Top => vertical_dimension(&mut svg, bounds, size[1], "Width"),
            ViewKind::Right => vertical_dimension(&mut svg, bounds, size[2], "Height"),
            ViewKind::Iso => {}
        }
        svg.push_str("</g>\n");
    }

    let mut y = 251.0;
    if !pmi_rows.is_empty() {
        table(
            &mut svg,
            "pmi",
            "PMI / tolerances and threads",
            &[
                ("Kind", 45.0),
                ("Specification", 102.0),
                ("Value", 55.0),
                ("Datum", 38.0),
                ("Target", 160.0),
            ],
            &pmi_rows,
            y,
        );
        y += (14 + 7 * pmi_rows.len()) as f64;
    }
    if !bom_rows.is_empty() {
        table(
            &mut svg,
            "bom",
            "Bill of materials (one row per instance)",
            &[
                ("Item", 30.0),
                ("Component ID", 145.0),
                ("Part", 145.0),
                ("Volume (mm³)", 80.0),
            ],
            &bom_rows,
            y,
        );
    }

    let y = height - 40.0;
    svg.push_str("<g id=\"title-block\">\n");
    for d in [
        [10.0, y, 410.0, y],
        [10.0, y + 15.0, 410.0, y + 15.0],
        [158.0, y, 158.0, y + 30.0],
        [302.0, y, 302.0, y + 30.0],
    ] {
        line(&mut svg, "table-line", d);
    }
    let title = format!("Part / assembly: {}", shape.part_name);
    let material = format!("Material: {}", options.material.unwrap_or("Not specified"));
    let manufacturing = format!(
        "Process: {}",
        options.manufacturing.unwrap_or("Not specified")
    );
    let view_scale = if scale >= 1.0 {
        format!("{scale:.0}:1")
    } else {
        format!("1:{:.0}", 1.0 / scale)
    };
    let volume = format!("Volume: {:.2} mm³", check.volume_mm3);
    let mass = match options.material.and_then(known_density) {
        Some(density) => format!("Est. mass: {:.2} g", check.volume_mm3 * density / 1000.0),
        None => "Est. mass: unavailable".into(),
    };
    for (x, row, w, value) in [
        (13.0, 8.5, 142.0, title.as_str()),
        (161.0, 8.5, 138.0, material.as_str()),
        (
            305.0,
            6.0,
            102.0,
            &format!("Scale {view_scale} · Dimensions: mm"),
        ),
        (
            305.0,
            11.0,
            102.0,
            "General tolerance reference: ISO 2768-mK",
        ),
        (13.0, 23.5, 142.0, manufacturing.as_str()),
        (161.0, 23.5, 138.0, volume.as_str()),
        (305.0, 23.5, 102.0, mass.as_str()),
    ] {
        fitted_text(&mut svg, x, y + row, w, value);
    }
    svg.push_str("</g>\n</svg>\n");
    svg
}

fn fit_scale(max: f64) -> f64 {
    [20.0, 10.0, 5.0, 2.0, 1.0, 0.5, 0.2, 0.1, 0.05, 0.02, 0.01]
        .into_iter()
        .find(|&s| s <= max)
        .unwrap_or_else(|| 1.0 / (1.0 / max).ceil())
}

fn iso_scale(bb: BBox3) -> f64 {
    let projected = bbox_corners(bb).map(|p| ViewKind::Iso.project(p));
    let range = |axis: fn((f64, f64, f64)) -> f64| {
        let min = projected
            .iter()
            .copied()
            .map(axis)
            .fold(f64::INFINITY, f64::min);
        let max = projected
            .iter()
            .copied()
            .map(axis)
            .fold(f64::NEG_INFINITY, f64::max);
        (max - min).max(1e-9)
    };
    (154.0 / range(|p| p.0)).min(66.0 / range(|p| p.1))
}

// Screen bounds are [left, top, right, bottom].
fn draw_view(
    svg: &mut String,
    shape: &SolidShape,
    bb: BBox3,
    view: ViewKind,
    center: [f64; 2],
    scale: f64,
) -> [f64; 4] {
    let mid = std::array::from_fn(|i| (bb.min[i] + bb.max[i]) * 0.5);
    let origin = view.project(mid);
    let projected_to_svg = |[x, y]: [f64; 2]| {
        [
            center[0] + (x - origin.0) * scale,
            center[1] - (y - origin.1) * scale,
        ]
    };
    let to_svg = |p| {
        let (x, y, _) = view.project(p);
        projected_to_svg([x, y])
    };
    for edge in &shape.edges {
        let points: Vec<_> = edge
            .polyline
            .as_chunks::<3>()
            .0
            .iter()
            .copied()
            .map(to_svg)
            .collect();
        if points.len() < 2 || points.iter().all(|p| *p == points[0]) {
            continue;
        }
        svg.push_str("<polyline class=\"edge\" points=\"");
        for [x, y] in points {
            let _ = write!(svg, "{x:.3},{y:.3} ");
        }
        svg.push_str("\"/>\n");
    }
    // Tangent silhouettes (for example the sides of a cylinder when seen
    // from the front) are not topological B-Rep edges.
    if let Ok(profiles) = super::dxf::projected_profiles(shape, view) {
        for profile in profiles {
            svg.push_str("<polygon class=\"outline\" points=\"");
            for [x, y] in profile.into_iter().map(projected_to_svg) {
                let _ = write!(svg, "{x:.3},{y:.3} ");
            }
            svg.push_str("\"/>\n");
        }
    }
    bbox_corners(bb).map(to_svg).iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |r, p| {
            [
                r[0].min(p[0]),
                r[1].min(p[1]),
                r[2].max(p[0]),
                r[3].max(p[1]),
            ]
        },
    )
}

fn horizontal_dimension(svg: &mut String, [l, _, r, b]: [f64; 4], value: f64, label: &str) {
    let y = b + 10.0;
    line(svg, "extension", [l, b + 1.0, l, y + 2.0]);
    line(svg, "extension", [r, b + 1.0, r, y + 2.0]);
    line(svg, "dimension", [l, y, r, y]);
    let x = (l + r) * 0.5;
    let _ = writeln!(
        svg,
        r#"<text text-anchor="middle" x="{x:.3}" y="{:.3}">{label} {value:.2} mm</text>"#,
        y - 1.5
    );
}

fn vertical_dimension(svg: &mut String, [_, t, r, b]: [f64; 4], value: f64, label: &str) {
    let x = r + 10.0;
    line(svg, "extension", [r + 1.0, t, x + 2.0, t]);
    line(svg, "extension", [r + 1.0, b, x + 2.0, b]);
    line(svg, "dimension", [x, t, x, b]);
    let y = (t + b) * 0.5;
    let _ = writeln!(
        svg,
        r#"<text text-anchor="middle" transform="translate({:.3} {y:.3}) rotate(-90)">{label} {value:.2} mm</text>"#,
        x + 4.5
    );
}

fn table(
    svg: &mut String,
    id: &str,
    name: &str,
    columns: &[(&str, f64)],
    rows: &[Vec<String>],
    y: f64,
) {
    let _ = writeln!(
        svg,
        "<g id=\"{id}\"><text class=\"heading\" x=\"13\" y=\"{}\">{name}</text>",
        y - 1.0
    );
    let header_y = y + 1.0;
    for (i, values) in std::iter::once(columns.iter().map(|(h, _)| *h).collect::<Vec<_>>())
        .chain(rows.iter().map(|r| r.iter().map(String::as_str).collect()))
        .enumerate()
    {
        let row_y = header_y + 7.0 * i as f64;
        line(svg, "table-line", [10.0, row_y, 410.0, row_y]);
        let mut x = 10.0;
        for (&(_, width), value) in columns.iter().zip(values) {
            line(svg, "table-line", [x, row_y, x, row_y + 7.0]);
            fitted_text(svg, x + 3.0, row_y + 4.9, width - 5.0, value);
            x += width;
        }
    }
    let bottom = header_y + 7.0 * (rows.len() + 1) as f64;
    line(svg, "table-line", [10.0, bottom, 410.0, bottom]);
    svg.push_str("</g>\n");
}

fn line(svg: &mut String, class: &str, [x1, y1, x2, y2]: [f64; 4]) {
    let _ = writeln!(
        svg,
        r#"<line class="{class}" x1="{x1:.3}" y1="{y1:.3}" x2="{x2:.3}" y2="{y2:.3}"/>"#
    );
}
fn text(svg: &mut String, x: f64, y: f64, value: &str) {
    let _ = writeln!(
        svg,
        r#"<text x="{x:.3}" y="{y:.3}">{}</text>"#,
        escape_xml(value)
    );
}
fn fitted_text(svg: &mut String, x: f64, y: f64, width: f64, value: &str) {
    let estimated = value
        .chars()
        .map(|c| if c.is_ascii() { 2.0 } else { 3.5 })
        .sum::<f64>();
    if estimated > width {
        let _ = writeln!(
            svg,
            r#"<text x="{x:.3}" y="{y:.3}" textLength="{width:.3}" lengthAdjust="spacingAndGlyphs">{}</text>"#,
            escape_xml(value)
        );
    } else {
        text(svg, x, y, value);
    }
}

fn known_density(material: &str) -> Option<f64> {
    match material.to_ascii_uppercase().as_str() {
        "AL-6061-T6" => Some(2.70),
        "AL-7075-T6" => Some(2.81),
        "SUS304" => Some(7.93),
        "45# STEEL" => Some(7.85),
        _ => None,
    }
}

pub(super) fn escape_xml(value: &str) -> String {
    value.chars().filter(|&c| matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}'))
        .fold(String::new(), |mut out, ch| {
            match ch {
                '&' => out.push_str("&amp;"), '<' => out.push_str("&lt;"), '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"), '\'' => out.push_str("&apos;"), _ => out.push(ch),
            }
            out
        })
}
