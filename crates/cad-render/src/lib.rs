use cad_kernel_occt::{BBox3, CheckReport, ComponentMeta, SolidShape, SurfaceType};
use cad_syntax::Span;
use serde::{Deserialize, Serialize};
use std::fmt::Write;

mod drawing;
mod dxf;

pub use drawing::{DrawingOptions, render_drawing_svg};
pub use dxf::render_dxf;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceMesh {
    pub face_id: String,
    pub surface_type: SurfaceType,
    pub vertices: Vec<f64>,
    pub normals: Vec<f64>,
    pub indices: Vec<u32>,
    pub provenance_tags: Vec<String>,
    pub exact_interface: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    pub ast_span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopoEdge {
    pub edge_id: String,
    pub curve_type: cad_kernel_occt::CurveType,
    pub polyline: Vec<f64>,
    pub provenance_tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    pub ast_span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditHandle {
    pub handle_id: String,
    pub param_name: String,
    pub tier: u8,
    pub current_value: f64,
    pub unit: String,
    pub ast_span: Span,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneMeshPayload {
    pub format_version: String,
    pub part_name: String,
    pub kind: String,
    pub units: String,
    pub faces: Vec<FaceMesh>,
    pub edges: Vec<TopoEdge>,
    pub edit_handles: Vec<EditHandle>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentMeta>,
}

pub fn build_scene_mesh(shape: &SolidShape, edit_handles: Vec<EditHandle>) -> SceneMeshPayload {
    let faces = shape
        .faces
        .iter()
        .map(|f| FaceMesh {
            face_id: f.face_id.clone(),
            surface_type: f.surface_type,
            vertices: f.vertices.clone(),
            normals: f.normals.clone(),
            indices: f.indices.clone(),
            provenance_tags: f.provenance_tags.clone(),
            exact_interface: f.exact_interface,
            component_id: f.component_id.clone(),
            ast_span: f.ast_span,
        })
        .collect();

    let edges = shape
        .edges
        .iter()
        .map(|e| TopoEdge {
            edge_id: e.edge_id.clone(),
            curve_type: e.curve_type,
            polyline: e.polyline.clone(),
            provenance_tags: e.provenance_tags.clone(),
            component_id: e.component_id.clone(),
            ast_span: e.ast_span,
        })
        .collect();

    let kind = if shape.components.is_empty() {
        "part".to_string()
    } else {
        "assembly".to_string()
    };

    SceneMeshPayload {
        format_version: "0.1".to_string(),
        part_name: shape.part_name.clone(),
        kind,
        units: "mm".to_string(),
        faces,
        edges,
        edit_handles,
        components: shape.components.clone(),
    }
}

#[derive(Debug, Clone)]
pub struct RenderedViews {
    pub front_svg: String,
    pub top_svg: String,
    pub right_svg: String,
    pub iso_svg: String,
}

#[derive(Debug, Clone, Copy)]
enum ViewKind {
    Front,
    Top,
    Right,
    Iso,
}

impl ViewKind {
    fn title(self) -> &'static str {
        match self {
            ViewKind::Front => "FRONT VIEW (X-Z)",
            ViewKind::Top => "TOP VIEW (X-Y)",
            ViewKind::Right => "RIGHT VIEW (Y-Z)",
            ViewKind::Iso => "ISOMETRIC VIEW (3D)",
        }
    }

    fn project(self, p: [f64; 3]) -> (f64, f64, f64) {
        match self {
            ViewKind::Front => (p[0], p[2], -p[1]),
            ViewKind::Top => (p[0], p[1], p[2]),
            ViewKind::Right => (p[1], p[2], p[0]),
            ViewKind::Iso => {
                // Standard isometric projection (yaw 45deg, pitch ~35.264deg)
                let yaw = std::f64::consts::FRAC_PI_4;
                let pitch = (1.0_f64 / 2.0_f64.sqrt()).atan();
                let (sy, cy) = yaw.sin_cos();
                let (sp, cp) = pitch.sin_cos();
                let x1 = p[0] * cy - p[1] * sy;
                let y1 = p[0] * sy + p[1] * cy;
                let u = x1;
                let v = p[2] * cp - y1 * sp;
                let depth = y1 * cp + p[2] * sp;
                (u, v, depth)
            }
        }
    }
}

pub fn render_views_svg(shape: &SolidShape, check: &CheckReport) -> RenderedViews {
    RenderedViews {
        front_svg: render_single_view(shape, check, ViewKind::Front),
        top_svg: render_single_view(shape, check, ViewKind::Top),
        right_svg: render_single_view(shape, check, ViewKind::Right),
        iso_svg: render_single_view(shape, check, ViewKind::Iso),
    }
}

fn render_single_view(shape: &SolidShape, check: &CheckReport, view: ViewKind) -> String {
    let w = 640.0;
    let h = 480.0;
    let pad = 64.0;

    let bb = check.bbox_mm;
    let corners = bbox_corners(bb);
    let mut u_min = f64::INFINITY;
    let mut u_max = f64::NEG_INFINITY;
    let mut v_min = f64::INFINITY;
    let mut v_max = f64::NEG_INFINITY;
    for c in corners {
        let (u, v, _) = view.project(c);
        u_min = u_min.min(u);
        u_max = u_max.max(u);
        v_min = v_min.min(v);
        v_max = v_max.max(v);
    }
    let span_u = (u_max - u_min).max(1.0);
    let span_v = (v_max - v_min).max(1.0);
    let scale = ((w - 2.0 * pad) / span_u).min((h - 2.0 * pad - 30.0) / span_v);
    let cu = 0.5 * (u_min + u_max);
    let cv = 0.5 * (v_min + v_max);
    let to_svg = |p: [f64; 3]| -> (f64, f64, f64) {
        let (u, v, d) = view.project(p);
        let sx = w * 0.5 + (u - cu) * scale;
        let sy = (h - 20.0) * 0.5 - (v - cv) * scale;
        (sx, sy, d)
    };

    // Collect depth-sorted triangles
    struct TriDraw {
        depth: f64,
        pts: [(f64, f64); 3],
        fill: String,
    }
    let mut tris = Vec::new();
    let light = [0.408, 0.408, 0.816];
    for face in &shape.faces {
        let v_count = face.vertices.len() / 3;
        let is_tagged = !face.provenance_tags.is_empty();
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
            let s0 = to_svg(p0);
            let s1 = to_svg(p1);
            let s2 = to_svg(p2);
            let n = [
                face.normals.get(i0 * 3).copied().unwrap_or(0.0),
                face.normals.get(i0 * 3 + 1).copied().unwrap_or(0.0),
                face.normals.get(i0 * 3 + 2).copied().unwrap_or(1.0),
            ];
            let ndotl = (n[0] * light[0] + n[1] * light[1] + n[2] * light[2]).abs();
            let shade = (120.0 + 105.0 * ndotl).clamp(90.0, 235.0) as u8;
            let fill = if is_tagged {
                format!("rgb({}, {}, {})", shade / 2, shade, 230)
            } else {
                format!("rgb({}, {}, {})", shade - 15, shade - 5, shade)
            };
            tris.push(TriDraw {
                depth: (s0.2 + s1.2 + s2.2) / 3.0,
                pts: [(s0.0, s0.1), (s1.0, s1.1), (s2.0, s2.1)],
                fill,
            });
        }
    }
    tris.sort_by(|a, b| {
        a.depth
            .partial_cmp(&b.depth)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut svg = String::with_capacity(16384);
    let _ = writeln!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">"
    );
    let _ = writeln!(
        svg,
        "  <rect width=\"100%\" height=\"100%\" fill=\"#0f172a\"/>"
    );

    // Dashed bounding box
    let bmin = to_svg(bb.min);
    let bmax = to_svg(bb.max);
    let bx = bmin.0.min(bmax.0);
    let by = bmin.1.min(bmax.1);
    let bw = (bmax.0 - bmin.0).abs().max(1.0);
    let bh = (bmax.1 - bmin.1).abs().max(1.0);
    let _ = writeln!(
        svg,
        "  <rect x=\"{bx:.1}\" y=\"{by:.1}\" width=\"{bw:.1}\" height=\"{bh:.1}\" fill=\"none\" stroke=\"#38bdf8\" stroke-width=\"1\" stroke-dasharray=\"5,4\" opacity=\"0.55\"/>"
    );

    // Shaded faces
    for t in &tris {
        let _ = writeln!(
            svg,
            "  <polygon points=\"{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" fill=\"{}\" fill-opacity=\"0.78\" stroke=\"{}\" stroke-width=\"0.35\"/>",
            t.pts[0].0, t.pts[0].1, t.pts[1].0, t.pts[1].1, t.pts[2].0, t.pts[2].1, t.fill, t.fill
        );
    }

    // Topological edges
    for edge in &shape.edges {
        if edge.polyline.len() < 6 {
            continue;
        }
        let mut pts_str = String::new();
        for chunk in edge.polyline.chunks_exact(3) {
            let (sx, sy, _) = to_svg([chunk[0], chunk[1], chunk[2]]);
            let _ = write!(pts_str, "{sx:.1},{sy:.1} ");
        }
        let stroke = if edge.provenance_tags.is_empty() {
            "#e2e8f0"
        } else {
            "#38bdf8"
        };
        let _ = writeln!(
            svg,
            "  <polyline points=\"{}\" fill=\"none\" stroke=\"{stroke}\" stroke-width=\"1.2\" stroke-linejoin=\"round\"/>",
            pts_str.trim_end()
        );
    }

    // Engineering HUD footer
    let sz = bb.size();
    let _ = writeln!(
        svg,
        "  <text x=\"16\" y=\"26\" fill=\"#f8fafc\" font-family=\"monospace\" font-size=\"14\" font-weight=\"bold\">{} — {}</text>",
        drawing::escape_xml(&shape.part_name),
        view.title()
    );
    let _ = writeln!(
        svg,
        "  <text x=\"16\" y=\"{}\" fill=\"#94a3b8\" font-family=\"monospace\" font-size=\"12\">BBox: {:.2} x {:.2} x {:.2} mm | Vol: {:.1} mm3 | Faces: {} | Watertight: {}</text>",
        h - 14.0,
        sz[0],
        sz[1],
        sz[2],
        check.volume_mm3,
        check.face_count,
        check.watertight
    );
    let _ = writeln!(svg, "</svg>");
    svg
}

fn bbox_corners(bb: BBox3) -> [[f64; 3]; 8] {
    [
        [bb.min[0], bb.min[1], bb.min[2]],
        [bb.max[0], bb.min[1], bb.min[2]],
        [bb.max[0], bb.max[1], bb.min[2]],
        [bb.min[0], bb.max[1], bb.min[2]],
        [bb.min[0], bb.min[1], bb.max[2]],
        [bb.max[0], bb.min[1], bb.max[2]],
        [bb.max[0], bb.max[1], bb.max[2]],
        [bb.min[0], bb.max[1], bb.max[2]],
    ]
}

/// Reserved `wgpu` hybrid Raymarching + B-Rep viewport interface for Phase 1/2 desktop & WASM hosts.
pub trait WgpuViewportPipeline {
    fn upload_scene_mesh(&mut self, payload: &SceneMeshPayload);
    fn resize(&mut self, width: u32, height: u32);
    fn render_frame(&mut self) -> Result<(), String>;
}
