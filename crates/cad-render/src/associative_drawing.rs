//! Associative 2D engineering drawings.
//!
//! A [`DrawingSheet`] stores views and dimensions whose anchors are [`cad_topo::TopoRef`]
//! values. Resolving the sheet against a new topology snapshot recomputes every
//! callout, so a 3D parameter change refreshes the drawing. Labels that land on
//! the same spot walk outward onto the next offset track. The SVG renderer draws
//! the paper, the views, and those callouts.

use cad_ir::{DrawingDimension, DrawingSheet, DrawingViewKind, DrawingViewSpec};
use cad_kernel_occt::SolidShape;
use cad_topo::{
    MatchTolerances, ResolveResult, SurfaceClass, TopoEntityId, TopoSnapshot, resolve_topo_ref,
};
use std::fmt::Write;

const WARNING_RED: &str = "#d1242f";
const INK: &str = "#142130";
/// Boxes that merely touch on an edge are not collisions.
const OVERLAP_GAP_MM: f64 = 1.0e-6;
const LABEL_HEIGHT_MM: f64 = 4.5;
const LABEL_TRACK_STEP_MM: f64 = 6.0;
const LABEL_TRACK_ITERS: usize = 512;

/// Whether a dimension is still tied to the current 3D model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DimensionStatus {
    /// Every anchor resolved and a callout was produced.
    Resolved,
    /// One anchor is missing or ambiguous. The rest of the sheet still resolves.
    Dangling { reason: String },
}

/// One dimension after its 3D anchors have been measured.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedDimension {
    pub id: String,
    pub label: String,
    /// Sheet placement authored on the drawing, in millimetres from the lower left.
    pub placement_mm: [f64; 2],
    /// Measured callout, or the override text when the sheet provides one.
    pub text: String,
    pub status: DimensionStatus,
    /// Entities the anchors resolved to, in anchor order. Empty when the first anchor fails.
    pub entity_ids: Vec<TopoEntityId>,
}

/// A drawing sheet whose dimensions carry measured callouts.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedDrawingSheet {
    pub sheet_id: String,
    pub title: String,
    pub paper_size: String,
    pub views: Vec<DrawingViewSpec>,
    pub dimensions: Vec<ResolvedDimension>,
}

/// Axis-aligned label rectangle in sheet millimetres. Y grows upward.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelBox {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl LabelBox {
    pub fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Self {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }

    pub fn width(self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(self) -> f64 {
        self.max_y - self.min_y
    }

    pub fn center(self) -> [f64; 2] {
        [
            (self.min_x + self.max_x) * 0.5,
            (self.min_y + self.max_y) * 0.5,
        ]
    }

    /// True when the rectangles overlap on a positive area.
    /// Shared edges do not count, so adjacent offset tracks are free of collisions.
    pub fn overlaps(self, other: Self) -> bool {
        self.min_x + OVERLAP_GAP_MM < other.max_x
            && other.min_x + OVERLAP_GAP_MM < self.max_x
            && self.min_y + OVERLAP_GAP_MM < other.max_y
            && other.min_y + OVERLAP_GAP_MM < self.max_y
    }

    fn translate(&mut self, dx: f64, dy: f64) {
        self.min_x += dx;
        self.max_x += dx;
        self.min_y += dy;
        self.max_y += dy;
    }
}

/// A dimension label participating in overlap layout.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedDimension {
    pub id: String,
    pub label: LabelBox,
}

impl PlacedDimension {
    pub fn new(id: impl Into<String>, label: LabelBox) -> Self {
        Self {
            id: id.into(),
            label,
        }
    }
}

/// Geometry copied off a resolved topology entity. Owned so measurement does not
/// borrow the snapshot.
struct MeasuredEntity {
    entity_id: TopoEntityId,
    centroid_mm: [f64; 3],
    normal_or_axis: Option<[f64; 3]>,
    radius_mm: Option<f64>,
    surface_class: Option<SurfaceClass>,
}

/// Resolve every dimension on `sheet` against one topology snapshot.
///
/// A missing or ambiguous anchor marks that dimension [`DimensionStatus::Dangling`]
/// and leaves the other dimensions on the sheet usable.
pub fn resolve_drawing_sheet(
    sheet: &DrawingSheet,
    snapshot: &TopoSnapshot,
    tolerances: &MatchTolerances,
) -> ResolvedDrawingSheet {
    let dimensions = sheet
        .dimensions
        .iter()
        .map(|dimension| resolve_dimension(dimension, snapshot, tolerances))
        .collect();
    ResolvedDrawingSheet {
        sheet_id: sheet.sheet_id.clone(),
        title: sheet.title.clone(),
        paper_size: sheet.paper_size.clone(),
        views: sheet.views.clone(),
        dimensions,
    }
}

fn resolve_dimension(
    dimension: &DrawingDimension,
    snapshot: &TopoSnapshot,
    tolerances: &MatchTolerances,
) -> ResolvedDimension {
    let mut measured = Vec::with_capacity(dimension.anchors.len());
    let mut failure = None;
    for (index, anchor) in dimension.anchors.iter().enumerate() {
        match resolve_topo_ref(anchor, snapshot, tolerances) {
            Ok(ResolveResult::Unique(id)) => {
                match snapshot
                    .candidates
                    .iter()
                    .find(|candidate| candidate.entity_id == id)
                {
                    Some(candidate) => measured.push(MeasuredEntity {
                        entity_id: candidate.entity_id.clone(),
                        centroid_mm: candidate.fingerprint.centroid_mm,
                        normal_or_axis: candidate.fingerprint.normal_or_axis,
                        radius_mm: candidate.fingerprint.radius_mm,
                        surface_class: candidate.fingerprint.surface_class,
                    }),
                    None => {
                        failure = Some(format!(
                            "anchor {index}: resolved entity '{}' is not in the snapshot",
                            id.0
                        ));
                        break;
                    }
                }
            }
            Ok(ResolveResult::Multiple(ids)) => {
                failure = Some(format!(
                    "anchor {index}: topology reference is ambiguous: matched {} entities",
                    ids.len()
                ));
                break;
            }
            Err(error) => {
                failure = Some(format!("anchor {index}: {error}"));
                break;
            }
        }
    }

    let entity_ids = measured
        .iter()
        .map(|entity| entity.entity_id.clone())
        .collect();
    if let Some(reason) = failure {
        return finished_dimension(
            dimension,
            dimension.override_text.clone().unwrap_or_default(),
            DimensionStatus::Dangling { reason },
            entity_ids,
        );
    }

    match callout_text(&measured, dimension.override_text.as_deref(), tolerances) {
        Ok(text) => finished_dimension(dimension, text, DimensionStatus::Resolved, entity_ids),
        Err(reason) => finished_dimension(
            dimension,
            dimension.override_text.clone().unwrap_or_default(),
            DimensionStatus::Dangling { reason },
            entity_ids,
        ),
    }
}

fn finished_dimension(
    dimension: &DrawingDimension,
    text: String,
    status: DimensionStatus,
    entity_ids: Vec<TopoEntityId>,
) -> ResolvedDimension {
    ResolvedDimension {
        id: dimension.id.clone(),
        label: dimension.label.clone(),
        placement_mm: dimension.placement_mm,
        text,
        status,
        entity_ids,
    }
}

fn callout_text(
    entities: &[MeasuredEntity],
    override_text: Option<&str>,
    tolerances: &MatchTolerances,
) -> Result<String, String> {
    if let Some(text) = override_text {
        return Ok(text.to_owned());
    }
    match entities {
        [] => Err("dimension has no topology anchors".to_owned()),
        [one] => diameter_callout(one),
        [first, second] => format_length(separation(first, second, tolerances)?),
        many => {
            let mut total = 0.0;
            for pair in many.windows(2) {
                total += separation(&pair[0], &pair[1], tolerances)?;
            }
            format_length(total)
        }
    }
}

fn diameter_callout(entity: &MeasuredEntity) -> Result<String, String> {
    match entity.radius_mm {
        Some(radius) if radius.is_finite() && radius > 0.0 => Ok(format!("Ø{:.2}", radius * 2.0)),
        _ => Err("single anchor has no positive radius to measure".to_owned()),
    }
}

/// Distance between two anchors.
///
/// Parallel or anti-parallel planar faces use the gap between the planes.
/// Every other pair uses the straight-line distance between centroids. Cylinder
/// axes stay on the straight-line path: projecting them onto the axis would
/// report a hole-to-hole dimension of zero whenever the holes share a height.
fn separation(
    left: &MeasuredEntity,
    right: &MeasuredEntity,
    tolerances: &MatchTolerances,
) -> Result<f64, String> {
    let delta = sub(right.centroid_mm, left.centroid_mm);
    let distance = if let Some(gap) = parallel_plane_gap(left, right, delta, tolerances) {
        gap
    } else {
        length(delta)
    };
    if distance.is_finite() {
        Ok(distance)
    } else {
        Err("measured distance is not finite".to_owned())
    }
}

fn parallel_plane_gap(
    left: &MeasuredEntity,
    right: &MeasuredEntity,
    delta: [f64; 3],
    tolerances: &MatchTolerances,
) -> Option<f64> {
    if left.surface_class != Some(SurfaceClass::Plane)
        || right.surface_class != Some(SurfaceClass::Plane)
    {
        return None;
    }
    let left_normal = left.normal_or_axis?;
    let right_normal = right.normal_or_axis?;
    if !normals_parallel(
        left_normal,
        right_normal,
        tolerances.normal_angle_tolerance_deg,
    ) {
        return None;
    }
    let normal = unit(left_normal)?;
    Some(dot(delta, normal).abs())
}

fn normals_parallel(left: [f64; 3], right: [f64; 3], tolerance_deg: f64) -> bool {
    let (Some(left), Some(right)) = (unit(left), unit(right)) else {
        return false;
    };
    let cosine = dot(left, right).clamp(-1.0, 1.0);
    let angle_deg = cosine.acos().to_degrees();
    let tolerance = if tolerance_deg.is_finite() && tolerance_deg >= 0.0 {
        tolerance_deg
    } else {
        1.0
    };
    angle_deg <= tolerance || (180.0 - angle_deg) <= tolerance
}

fn format_length(value: f64) -> Result<String, String> {
    if !value.is_finite() || value < 0.0 {
        return Err("measured distance is not a finite length".to_owned());
    }
    Ok(format!("{value:.2}"))
}

/// Move overlapping labels onto successive offset tracks.
///
/// Earlier entries keep their box. Each later box that still touches an earlier
/// box steps `step_mm` along +Y, which is the next horizontal track away from
/// the view. Index order makes the same input produce the same layout.
///
/// Returns the number of track steps applied while clearing collisions.
pub fn resolve_label_overlaps(
    boxes: &mut [PlacedDimension],
    step_mm: f64,
    max_iters: usize,
) -> usize {
    if boxes.len() < 2 || max_iters == 0 || !step_mm.is_finite() || step_mm <= 0.0 {
        return 0;
    }
    let mut steps = 0usize;
    for later in 1..boxes.len() {
        while steps < max_iters && overlaps_earlier(boxes, later) {
            boxes[later].label.translate(0.0, step_mm);
            steps += 1;
        }
    }
    steps
}

fn overlaps_earlier(boxes: &[PlacedDimension], later: usize) -> bool {
    boxes[..later]
        .iter()
        .any(|earlier| earlier.label.overlaps(boxes[later].label))
}

/// Render a resolved sheet to a vector SVG.
///
/// Paper borders follow A4 (297 × 210 mm) and A3 (420 × 297 mm) landscape
/// sheets. Dangling dimensions are drawn in warning red. Label boxes are
/// stacked with [`resolve_label_overlaps`] before the callouts are drawn.
pub fn render_associative_sheet_svg(resolved: &ResolvedDrawingSheet, solid: &SolidShape) -> String {
    let (page_w, page_h) = paper_size_mm(&resolved.paper_size);
    let mut svg = String::with_capacity(8 * 1024);
    let _ = writeln!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{page_w:.0}mm" height="{page_h:.0}mm" viewBox="0 0 {page_w:.0} {page_h:.0}" role="img">"#
    );
    let _ = writeln!(svg, "<title>{}</title>", escape_xml(&resolved.title));
    svg.push_str(
        r##"<defs><marker id="assoc-arrow" markerWidth="4" markerHeight="4" refX="3" refY="2" orient="auto-start-reverse" markerUnits="userSpaceOnUse"><path d="M0 0.6 L3 2 L0 3.4" fill="none" stroke="#324050" stroke-width="0.35"/></marker></defs>
<style>
text{font-family:Arial,sans-serif;fill:#142130;font-size:3.5px}
.sheet-title{font-size:5px;font-weight:bold}
.view-title,.section-marker{font-size:4px;font-weight:bold}
.paper-border,.title-frame,.view-frame{fill:none;stroke:#142130}
.paper-border{stroke-width:.7}.title-frame,.view-frame{stroke-width:.4}
.view-outline{fill:none;stroke:#142130;stroke-width:.45;stroke-linejoin:round;stroke-linecap:round}
.section-cut{fill:none;stroke:#142130;stroke-width:.45;stroke-dasharray:4 1.4}
.detail-window{fill:none;stroke:#142130;stroke-width:.4}
.dimension-line{fill:none;stroke:#324050;stroke-width:.25;marker-start:url(#assoc-arrow);marker-end:url(#assoc-arrow)}
.leader{fill:none;stroke:#324050;stroke-width:.2}
.dimension-text{text-anchor:middle}
.dimension-dangling .dimension-line,.dimension-dangling .leader{stroke:#d1242f}
.dimension-dangling text{fill:#d1242f}
</style>
"##,
    );
    write_paper_and_title(&mut svg, resolved, page_w, page_h);
    write_views(&mut svg, resolved, solid, page_h);
    write_dimensions(&mut svg, resolved, page_h);
    svg.push_str("</svg>\n");
    svg
}

fn paper_size_mm(name: &str) -> (f64, f64) {
    match name.trim().to_ascii_uppercase().as_str() {
        "A4" | "A4-LANDSCAPE" | "A4L" => (297.0, 210.0),
        "A4-PORTRAIT" | "A4P" => (210.0, 297.0),
        "A3-PORTRAIT" | "A3P" => (297.0, 420.0),
        _ => (420.0, 297.0),
    }
}

fn write_paper_and_title(
    svg: &mut String,
    resolved: &ResolvedDrawingSheet,
    page_w: f64,
    page_h: f64,
) {
    let _ = writeln!(
        svg,
        r#"<rect id="paper-border" class="paper-border" x="8" y="8" width="{:.3}" height="{:.3}"/>"#,
        page_w - 16.0,
        page_h - 16.0
    );
    let block_w = 118.0;
    let block_h = 28.0;
    let block_x = page_w - 8.0 - block_w;
    let block_y = page_h - 8.0 - block_h;
    let _ = writeln!(
        svg,
        r#"<g id="title-block"><rect class="title-frame" x="{block_x:.3}" y="{block_y:.3}" width="{block_w:.3}" height="{block_h:.3}"/>"#
    );
    text_at(
        svg,
        "sheet-title",
        block_x + 4.0,
        block_y + 8.0,
        INK,
        &resolved.title,
    );
    text_at(
        svg,
        "sheet-id",
        block_x + 4.0,
        block_y + 16.0,
        INK,
        &resolved.sheet_id,
    );
    text_at(
        svg,
        "sheet-paper",
        block_x + 4.0,
        block_y + 23.0,
        INK,
        &resolved.paper_size,
    );
    svg.push_str("</g>\n");
}

fn write_views(svg: &mut String, resolved: &ResolvedDrawingSheet, solid: &SolidShape, page_h: f64) {
    let model_points = model_points(solid);
    for view in &resolved.views {
        let scale = usable_scale(view_scale(view));
        let projected = project_model_points(&view.kind, &model_points);
        let center = bounds_center(&projected).unwrap_or([0.0, 0.0]);
        let sheet_points = place_points(&projected, view.placement_mm, center, scale);
        let frame = view_frame(&sheet_points, view.placement_mm);
        let token = xml_token(&view.view_id);
        let _ = writeln!(svg, r#"<g id="view-{token}" class="drawing-view">"#);
        rect(svg, "view-frame", frame, page_h);
        text_at(
            svg,
            "view-title",
            frame.min_x,
            sheet_y_to_svg(frame.max_y + 5.0, page_h),
            INK,
            &view_title(&view.kind),
        );
        write_outlines(
            svg,
            solid,
            &view.kind,
            view.placement_mm,
            center,
            scale,
            page_h,
        );
        write_view_markers(svg, view, frame, page_h);
        svg.push_str("</g>\n");
    }
}

fn write_outlines(
    svg: &mut String,
    solid: &SolidShape,
    kind: &DrawingViewKind,
    placement: [f64; 2],
    center: [f64; 2],
    scale: f64,
    page_h: f64,
) {
    let has_edges = solid.edges.iter().any(|edge| edge.polyline.len() >= 6);
    if has_edges {
        for edge in &solid.edges {
            let points = place_points(
                &project_flat(kind, &edge.polyline),
                placement,
                center,
                scale,
            );
            polyline(svg, "view-outline", &points, page_h, false);
        }
        return;
    }
    for face in &solid.faces {
        let flat: Vec<f64> = face.outer_loop.iter().flat_map(|point| *point).collect();
        let points = place_points(&project_flat(kind, &flat), placement, center, scale);
        polyline(svg, "view-outline", &points, page_h, true);
    }
}

fn write_view_markers(svg: &mut String, view: &DrawingViewSpec, frame: LabelBox, page_h: f64) {
    match &view.kind {
        DrawingViewKind::Section {
            label,
            plane_normal,
            ..
        } => {
            let marker = section_marker(label);
            let direction = section_cut_direction(*plane_normal);
            let half = frame.width().min(frame.height()) * 0.42;
            let start = [
                frame.center()[0] - direction[0] * half,
                frame.center()[1] - direction[1] * half,
            ];
            let end = [
                frame.center()[0] + direction[0] * half,
                frame.center()[1] + direction[1] * half,
            ];
            line(svg, "section-cut", start, end, page_h);
            text_at(
                svg,
                "section-marker",
                start[0],
                sheet_y_to_svg(start[1] + 4.0, page_h),
                INK,
                &marker,
            );
        }
        DrawingViewKind::Detail {
            radius_mm,
            center_mm,
            ..
        } => {
            let center = [
                frame.center()[0] + center_mm[0],
                frame.center()[1] + center_mm[1],
            ];
            let radius = radius_mm
                .abs()
                .clamp(2.0, frame.width().min(frame.height()) * 0.45);
            let (cx, cy) = sheet_to_svg(center, page_h);
            let _ = writeln!(
                svg,
                r#"<circle class="detail-window" cx="{cx:.3}" cy="{cy:.3}" r="{radius:.3}"/>"#
            );
            text_at(
                svg,
                "view-title",
                center[0] + radius + 1.5,
                sheet_y_to_svg(center[1], page_h),
                INK,
                "DETAIL",
            );
        }
        _ => {}
    }
}

fn write_dimensions(svg: &mut String, resolved: &ResolvedDrawingSheet, page_h: f64) {
    let mut placed = Vec::with_capacity(resolved.dimensions.len());
    for dimension in &resolved.dimensions {
        placed.push(PlacedDimension::new(
            dimension.id.clone(),
            label_box_at(callout_of(dimension), dimension.placement_mm),
        ));
    }
    resolve_label_overlaps(&mut placed, LABEL_TRACK_STEP_MM, LABEL_TRACK_ITERS);
    for (dimension, layout) in resolved.dimensions.iter().zip(&placed) {
        let dangling = matches!(dimension.status, DimensionStatus::Dangling { .. });
        let token = xml_token(&dimension.id);
        let class = if dangling {
            "dimension dimension-dangling"
        } else {
            "dimension"
        };
        let fill = if dangling { WARNING_RED } else { INK };
        let _ = writeln!(svg, r#"<g id="dim-{token}" class="{class}">"#);
        if let DimensionStatus::Dangling { reason } = &dimension.status {
            let _ = writeln!(svg, "<title>{}</title>", escape_xml(reason));
        }
        let center = layout.label.center();
        let authored = dimension.placement_mm;
        if (center[0] - authored[0]).abs() > 0.5 || (center[1] - authored[1]).abs() > 0.5 {
            line(svg, "leader", authored, center, page_h);
        }
        let y = center[1];
        line(
            svg,
            "dimension-line",
            [layout.label.min_x, y],
            [layout.label.max_x, y],
            page_h,
        );
        text_at(
            svg,
            "dimension-text",
            center[0],
            sheet_y_to_svg(y + 1.4, page_h),
            fill,
            callout_of(dimension),
        );
        svg.push_str("</g>\n");
    }
}

fn callout_of(dimension: &ResolvedDimension) -> &str {
    if !dimension.text.is_empty() {
        dimension.text.as_str()
    } else if !dimension.label.is_empty() {
        dimension.label.as_str()
    } else {
        dimension.id.as_str()
    }
}

fn label_box_at(text: &str, placement: [f64; 2]) -> LabelBox {
    let width = (text.chars().count() as f64 * 1.9 + 2.0).max(10.0);
    LabelBox {
        min_x: placement[0] - width * 0.5,
        max_x: placement[0] + width * 0.5,
        min_y: placement[1] - LABEL_HEIGHT_MM * 0.5,
        max_y: placement[1] + LABEL_HEIGHT_MM * 0.5,
    }
}

fn view_scale(view: &DrawingViewSpec) -> f64 {
    let extra = match &view.kind {
        DrawingViewKind::Detail { scale, .. } => *scale,
        _ => 1.0,
    };
    view.scale * extra
}

fn usable_scale(scale: f64) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

fn view_title(kind: &DrawingViewKind) -> String {
    match kind {
        DrawingViewKind::Front => "Front".to_owned(),
        DrawingViewKind::Top => "Top".to_owned(),
        DrawingViewKind::Right => "Right".to_owned(),
        DrawingViewKind::Isometric => "Isometric".to_owned(),
        DrawingViewKind::Section { label, .. } => format!("Section {}", section_marker(label)),
        DrawingViewKind::Detail { .. } => "Detail".to_owned(),
    }
}

fn section_marker(label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        "A-A".to_owned()
    } else if label.contains('-') {
        label.to_owned()
    } else {
        format!("{label}-{label}")
    }
}

fn section_cut_direction(normal: [f64; 3]) -> [f64; 2] {
    let Some(normal) = unit(normal) else {
        return [1.0, 0.0];
    };
    // The cut mark lies in the section plane, so it is perpendicular to the
    // normal once the normal is flattened onto the sheet (X to the right, Z up).
    let flat = [normal[0], normal[2]];
    let length = (flat[0] * flat[0] + flat[1] * flat[1]).sqrt();
    if length < 1.0e-8 {
        [1.0, 0.0]
    } else {
        [-flat[1] / length, flat[0] / length]
    }
}

fn model_points(solid: &SolidShape) -> Vec<[f64; 3]> {
    let mut points = Vec::new();
    if solid.edges.iter().any(|edge| edge.polyline.len() >= 6) {
        for edge in &solid.edges {
            push_flat(&mut points, &edge.polyline);
        }
    } else {
        for face in &solid.faces {
            points.extend(face.outer_loop.iter().copied());
        }
    }
    points
}

fn push_flat(points: &mut Vec<[f64; 3]>, flat: &[f64]) {
    for chunk in flat.chunks_exact(3) {
        points.push([chunk[0], chunk[1], chunk[2]]);
    }
}

fn project_model_points(kind: &DrawingViewKind, points: &[[f64; 3]]) -> Vec<[f64; 2]> {
    points
        .iter()
        .map(|point| project_point(kind, *point))
        .collect()
}

fn project_flat(kind: &DrawingViewKind, flat: &[f64]) -> Vec<[f64; 2]> {
    flat.chunks_exact(3)
        .map(|chunk| project_point(kind, [chunk[0], chunk[1], chunk[2]]))
        .collect()
}

fn project_point(kind: &DrawingViewKind, point: [f64; 3]) -> [f64; 2] {
    match kind {
        DrawingViewKind::Front | DrawingViewKind::Detail { .. } => [point[0], point[2]],
        DrawingViewKind::Top => [point[0], point[1]],
        DrawingViewKind::Right => [point[1], point[2]],
        DrawingViewKind::Isometric => project_iso(point),
        DrawingViewKind::Section {
            plane_origin_mm,
            plane_normal,
            ..
        } => {
            let (axis_u, axis_v) = section_basis(*plane_normal);
            let delta = sub(point, *plane_origin_mm);
            [dot(delta, axis_u), dot(delta, axis_v)]
        }
    }
}

fn project_iso(point: [f64; 3]) -> [f64; 2] {
    let yaw = std::f64::consts::FRAC_PI_4;
    let pitch = (1.0 / 2.0_f64.sqrt()).atan();
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    let x1 = point[0] * cy - point[1] * sy;
    let y1 = point[0] * sy + point[1] * cy;
    [x1, point[2] * cp - y1 * sp]
}

fn section_basis(normal: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let normal = unit(normal).unwrap_or([0.0, 0.0, 1.0]);
    let helper = if normal[0].abs() < 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let axis_u = unit(cross(helper, normal)).unwrap_or([1.0, 0.0, 0.0]);
    let axis_v = cross(normal, axis_u);
    (axis_u, axis_v)
}

fn place_points(
    projected: &[[f64; 2]],
    placement: [f64; 2],
    center: [f64; 2],
    scale: f64,
) -> Vec<[f64; 2]> {
    projected
        .iter()
        .map(|point| {
            [
                placement[0] + (point[0] - center[0]) * scale,
                placement[1] + (point[1] - center[1]) * scale,
            ]
        })
        .collect()
}

fn bounds_center(points: &[[f64; 2]]) -> Option<[f64; 2]> {
    if points.is_empty() {
        return None;
    }
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for point in points {
        min_x = min_x.min(point[0]);
        min_y = min_y.min(point[1]);
        max_x = max_x.max(point[0]);
        max_y = max_y.max(point[1]);
    }
    if !min_x.is_finite() || !min_y.is_finite() {
        return None;
    }
    Some([(min_x + max_x) * 0.5, (min_y + max_y) * 0.5])
}

fn view_frame(points: &[[f64; 2]], placement: [f64; 2]) -> LabelBox {
    let mut frame = if let Some(bounds) = bounds_2d(points) {
        LabelBox {
            min_x: bounds.min_x - 8.0,
            min_y: bounds.min_y - 8.0,
            max_x: bounds.max_x + 8.0,
            max_y: bounds.max_y + 8.0,
        }
    } else {
        LabelBox::new(
            placement[0] - 24.0,
            placement[1] - 18.0,
            placement[0] + 24.0,
            placement[1] + 18.0,
        )
    };
    expand_to_minimum(&mut frame, 36.0, 28.0);
    frame
}

fn bounds_2d(points: &[[f64; 2]]) -> Option<LabelBox> {
    if points.is_empty() {
        return None;
    }
    let mut bounds = LabelBox::new(
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for point in points {
        bounds.min_x = bounds.min_x.min(point[0]);
        bounds.min_y = bounds.min_y.min(point[1]);
        bounds.max_x = bounds.max_x.max(point[0]);
        bounds.max_y = bounds.max_y.max(point[1]);
    }
    if bounds.min_x.is_finite() && bounds.min_y.is_finite() {
        Some(bounds)
    } else {
        None
    }
}

fn expand_to_minimum(frame: &mut LabelBox, min_width: f64, min_height: f64) {
    let center = frame.center();
    if frame.width() < min_width {
        frame.min_x = center[0] - min_width * 0.5;
        frame.max_x = center[0] + min_width * 0.5;
    }
    if frame.height() < min_height {
        frame.min_y = center[1] - min_height * 0.5;
        frame.max_y = center[1] + min_height * 0.5;
    }
}

fn rect(svg: &mut String, class: &str, frame: LabelBox, page_h: f64) {
    let (x, y) = sheet_to_svg([frame.min_x, frame.max_y], page_h);
    let _ = writeln!(
        svg,
        r#"<rect class="{class}" x="{x:.3}" y="{y:.3}" width="{:.3}" height="{:.3}"/>"#,
        frame.width(),
        frame.height()
    );
}

fn line(svg: &mut String, class: &str, start: [f64; 2], end: [f64; 2], page_h: f64) {
    let (x1, y1) = sheet_to_svg(start, page_h);
    let (x2, y2) = sheet_to_svg(end, page_h);
    let _ = writeln!(
        svg,
        r#"<line class="{class}" x1="{x1:.3}" y1="{y1:.3}" x2="{x2:.3}" y2="{y2:.3}"/>"#
    );
}

fn polyline(svg: &mut String, class: &str, points: &[[f64; 2]], page_h: f64, close: bool) {
    if points.len() < 2 {
        return;
    }
    let mut body = String::new();
    for point in points {
        let (x, y) = sheet_to_svg(*point, page_h);
        let _ = write!(body, "{x:.3},{y:.3} ");
    }
    if close {
        let (x, y) = sheet_to_svg(points[0], page_h);
        let _ = write!(body, "{x:.3},{y:.3} ");
    }
    let _ = writeln!(svg, r#"<polyline class="{class}" points="{body}"/>"#);
}

fn text_at(svg: &mut String, class: &str, x: f64, y: f64, fill: &str, value: &str) {
    let _ = writeln!(
        svg,
        r#"<text class="{class}" x="{x:.3}" y="{y:.3}" fill="{fill}">{}</text>"#,
        escape_xml(value)
    );
}

fn sheet_to_svg(point: [f64; 2], page_h: f64) -> (f64, f64) {
    (point[0], sheet_y_to_svg(point[1], page_h))
}

fn sheet_y_to_svg(y: f64, page_h: f64) -> f64 {
    page_h - y
}

fn xml_token(raw: &str) -> String {
    let mut token = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            token.push(ch);
        } else {
            token.push('_');
        }
    }
    if token.is_empty() {
        token.push_str("item");
    }
    token
}

fn escape_xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn sub(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn cross(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn length(value: [f64; 3]) -> f64 {
    dot(value, value).sqrt()
}

fn unit(value: [f64; 3]) -> Option<[f64; 3]> {
    let magnitude = length(value);
    if !magnitude.is_finite() || magnitude < 1.0e-12 {
        None
    } else {
        Some([
            value[0] / magnitude,
            value[1] / magnitude,
            value[2] / magnitude,
        ])
    }
}
