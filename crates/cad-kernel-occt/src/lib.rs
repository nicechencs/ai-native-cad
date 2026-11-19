pub mod brep_gen;
pub mod csg;
#[cfg(feature = "occt-ffi")]
pub mod occt_ffi;
pub mod step_writer;
pub mod types;

pub use brep_gen::*;
pub use csg::*;
pub use step_writer::export_step_ap242;
pub use types::*;

use cad_syntax::Span;
use std::collections::HashMap;

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

    fn export_step(&self, shape: &SolidShape) -> String;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct AnalyticKernel;

impl AnalyticKernel {
    pub fn new() -> Self {
        Self
    }
}

impl GeometryKernel for AnalyticKernel {
    fn backend_name(&self) -> &'static str {
        "rust-brep-mvp"
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
        fillet_shape(shape, radius, span)
    }

    fn chamfer(&self, shape: &SolidShape, dist: f64, span: Span) -> Result<SolidShape, String> {
        chamfer_shape(shape, dist, span)
    }

    fn shell(
        &self,
        shape: &SolidShape,
        thickness: f64,
        open_normals: &[[f64; 3]],
        span: Span,
    ) -> Result<SolidShape, String> {
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
    /// AND strict closed-2-manifold directed edge pairing (zero open boundary edges).
    fn check_solid(&self, shape: &SolidShape) -> CheckReport {
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
        let mut half_edges: HashMap<((i64, i64, i64), (i64, i64, i64)), usize> = HashMap::new();

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
                    if a != b {
                        *half_edges.entry((a, b)).or_insert(0) += 1;
                    }
                }
            }
        }

        let mut open_edge_count = 0usize;
        for (&(a, b), &cnt) in &half_edges {
            let twin = half_edges.get(&(b, a)).copied().unwrap_or(0);
            if twin != cnt {
                open_edge_count += cnt.abs_diff(twin);
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
        if volume_mm3 <= 1e-6 {
            issues.push("solid has zero or negative volume".to_string());
        }
        let watertight = !has_nan
            && !shape.faces.is_empty()
            && !missing_loop
            && open_edge_count == 0
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

    fn export_step(&self, shape: &SolidShape) -> String {
        export_step_ap242(shape)
    }
}
