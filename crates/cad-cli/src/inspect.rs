use cad_evaluator::{
    EvaluatedImport, FsAssetResolver, ImportOptions, LengthUnit, evaluate_import_with_kernel,
};
use cad_hir::{Diagnostic, Severity};
use cad_kernel_occt::{BBox3, BRepFace, GeometryKernel, SurfaceType};
use cad_syntax::Span;
use clap::Args;
use serde::Serialize;
use std::{collections::HashMap, path::PathBuf, process::ExitCode};

#[derive(Args)]
pub(crate) struct InspectArgs {
    asset_path: PathBuf,
    /// Restrict inspection to assets relative to this workspace directory
    #[arg(long, value_name = "DIR")]
    workspace_root: Option<PathBuf>,
    /// Override the source unit for meshes (um, mm, cm, in, ft, m)
    #[arg(long)]
    source_unit: Option<LengthUnit>,
    /// Repair and sew exact CAD geometry before inspection
    #[arg(long)]
    heal: bool,
    /// Sewing tolerance in millimeters (defaults to 0.001)
    #[arg(long, value_name = "MM", requires = "heal")]
    sewing_tol: Option<f64>,
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct Summary<'a> {
    status: &'static str,
    file: &'a str,
    format: &'a str,
    sha256: &'a str,
    unit_detected: &'a str,
    watertight: bool,
    bbox_mm: BBox3,
    volume_mm3: f64,
    components: Vec<Component<'a>>,
    datum_planes: Vec<DatumPlane>,
    hole_groups: Vec<HoleGroup>,
    cylindrical_bosses: Vec<Boss>,
}

#[derive(Serialize)]
struct Component<'a> {
    component_id: &'a str,
    part_name: &'a str,
    bbox_mm: BBox3,
}

#[derive(Serialize)]
struct DatumPlane {
    id: String,
    normal: Vec3,
    origin_mm: Vec3,
    area_mm2: f64,
}

#[derive(Serialize)]
struct HoleGroup {
    group_id: String,
    pattern: &'static str,
    count: usize,
    diameter_mm: f64,
    depth_mm: f64,
    axis: Vec3,
    centers_mm: Vec<Vec3>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pitch_circle_diameter_mm: Option<f64>,
}

#[derive(Serialize)]
struct Boss {
    id: String,
    diameter_mm: f64,
    height_mm: f64,
    axis: Vec3,
    base_center_mm: Vec3,
}

pub(crate) fn run(args: InspectArgs, kernel: &dyn GeometryKernel) -> ExitCode {
    let file = args.asset_path.to_string_lossy();
    let result = (|| -> Result<u8, Diagnostic> {
        let (root, path) = if let Some(root) = args.workspace_root.as_deref() {
            // Do not split or normalize the path: the resolver must check every component.
            (root, args.asset_path.as_os_str())
        } else {
            let name = args
                .asset_path
                .file_name()
                .ok_or_else(|| error("E502_UNSAFE_ASSET_PATH", "expected an asset filename"))?;
            (super::file_parent(&args.asset_path), name)
        };
        let path = path
            .to_str()
            .ok_or_else(|| error("E502_UNSAFE_ASSET_PATH", "expected a UTF-8 asset path"))?;
        let resolver = FsAssetResolver::new(root.to_path_buf()).map_err(|e| {
            error(
                e.code(),
                format!("cannot open asset directory `{}`: {e}", root.display()),
            )
        })?;
        let options = ImportOptions {
            source_unit: args.source_unit,
            heal: args.heal,
            sewing_tol_mm: args.sewing_tol,
            // Inspection reports open shells; it does not accept them as manufacturing solids.
            allow_open_shell: true,
            ..Default::default()
        };
        let import = evaluate_import_with_kernel(&resolver, path, &options, Span::default(), kernel)?;
        let summary = summarize(&file, &import, args.source_unit)?;
        let exit_code = inspection_exit_code(summary.format, summary.watertight);
        if args.json {
            println!("{}", serde_json::to_string_pretty(&summary).unwrap());
        } else if exit_code == 0 {
            println!(
                "[OK] {} ({}; source unit {}; watertight={}; volume={:.3} mm³)",
                file, summary.format, summary.unit_detected, summary.watertight, summary.volume_mm3
            );
            let size = summary.bbox_mm.size();
            println!(
                "  {:.3} × {:.3} × {:.3} mm; {} components, {} datum planes, {} hole groups, {} bosses",
                size[0],
                size[1],
                size[2],
                summary.components.len(),
                summary.datum_planes.len(),
                summary.hole_groups.len(),
                summary.cylindrical_bosses.len()
            );
            for diagnostic in &import.diagnostics {
                eprintln!("  [{}] {}", diagnostic.code, diagnostic.message);
            }
        } else {
            eprintln!(
                "[ERROR] {file}: [E410_NOT_WATERTIGHT] imported shape is not a watertight solid"
            );
        }
        Ok(exit_code)
    })();
    match result {
        Ok(code) => ExitCode::from(code),
        Err(diagnostic) => {
            let safe_msg = cad_observe::sanitize_message(&diagnostic.message);
            let is_io = diagnostic.code == "E001_IO_ERROR";
            let log_code = if is_io {
                cad_observe::codes::R016_IO_ERROR
            } else {
                diagnostic.code.as_str()
            };
            tracing::error!(
                component = "cad-cli",
                operation = "inspect",
                code = log_code,
                error_type = if is_io { "IoError" } else { "InspectError" },
                path = file.as_ref(),
                "{safe_msg}"
            );
            if args.json {
                let mut report = serde_json::json!({
                    "status": "error", "file": file, "diagnostics": [diagnostic]
                });
                if let Some(trace_id) = cad_observe::current_trace_id() {
                    report["trace_id"] = serde_json::Value::String(trace_id.to_owned());
                }
                println!("{report}");
            } else {
                eprintln!(
                    "[ERROR] {file}: [{}] {}",
                    diagnostic.code, diagnostic.message
                );
            }
            ExitCode::from(1)
        }
    }
}

pub(crate) fn inspection_exit_code(format: &str, watertight: bool) -> u8 {
    // Open meshes stay a successful preview. An exact solid that is not
    // watertight is a failed inspection.
    let exact = matches!(format, "STEP" | "BREP" | "IGES");
    u8::from(exact && !watertight)
}

fn error(code: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        severity: Severity::Error,
        message: message.into(),
        span: Span::default(),
    }
}

fn summarize<'a>(
    file: &'a str,
    import: &'a EvaluatedImport,
    source_override: Option<LengthUnit>,
) -> Result<Summary<'a>, Diagnostic> {
    let format = import.format.as_str();
    // The 3MF importer retains the declared unit even when geometry is scaled by an override.
    let unit_detected = match source_override {
        Some(unit) => unit.symbol(),
        None => import.unit_detected.as_str(),
    };
    if !matches!(unit_detected, "um" | "mm" | "cm" | "in" | "ft" | "m" | "unknown") {
        return Err(error(
            "E504_UNSUPPORTED_IMPORT_FORMAT",
            format!("unsupported source unit `{unit_detected}`"),
        ));
    }
    let report = &import.check_report;
    if !valid_bbox(report.bbox_mm) || !report.volume_mm3.is_finite() {
        return Err(error(
            "E504_UNSUPPORTED_IMPORT_FORMAT",
            "asset has invalid bounds or volume",
        ));
    }
    let components = import
        .solid
        .components
        .iter()
        .map(|c| Component {
            component_id: &c.component_id,
            part_name: if c.part_name.is_empty() {
                &c.component_id
            } else {
                &c.part_name
            },
            bbox_mm: c.bbox_mm,
        })
        .collect::<Vec<_>>();
    if components
        .iter()
        .any(|c| c.component_id.is_empty() || !valid_bbox(c.bbox_mm))
    {
        return Err(error(
            "E504_UNSUPPORTED_IMPORT_FORMAT",
            "asset has a component with invalid identity or bounds",
        ));
    }
    let (datum_planes, hole_groups, cylindrical_bosses) = features(&import.solid.faces);
    Ok(Summary {
        status: "ok",
        file,
        format,
        sha256: &import.sha256,
        unit_detected,
        watertight: report.watertight,
        bbox_mm: report.bbox_mm,
        volume_mm3: report.volume_mm3.max(0.0),
        components,
        datum_planes,
        hole_groups,
        cylindrical_bosses,
    })
}

fn valid_bbox(bbox: BBox3) -> bool {
    (0..3).all(|i| bbox.min[i].is_finite() && bbox.max[i].is_finite() && bbox.min[i] <= bbox.max[i])
}

type Vec3 = [f64; 3];
const DIST: f64 = 1e-4; // millimetres; also used for stitching split cylinder patches
const ANGLE_DOT: f64 = 1.0 - 1e-6;
// Strictly greater than sqrt(2 * (1 - ANGLE_DOT)) ≈ 0.001415 so any two normals
// with dot >= ANGLE_DOT are guaranteed to lie within ±1 cell on every axis.
const PLANE_NORMAL_CELL: f64 = 0.002;

fn quantize_normal(normal: Vec3) -> [i32; 3] {
    normal.map(|v| (v / PLANE_NORMAL_CELL).floor() as i32)
}

fn quantize_scalar(value: f64, step: f64) -> i64 {
    const LIMIT: f64 = (i64::MAX / 2) as f64;
    (value / step).floor().clamp(-LIMIT, LIMIT) as i64
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: Vec3, b: Vec3) -> Vec3 {
    std::array::from_fn(|i| a[i] - b[i])
}
fn scale(a: Vec3, s: f64) -> Vec3 {
    a.map(|v| v * s)
}
fn dot(a: Vec3, b: Vec3) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: Vec3) -> f64 {
    a[0].hypot(a[1]).hypot(a[2])
}
fn unit(a: Vec3) -> Option<Vec3> {
    let len = norm(a);
    (len.is_finite() && len > 0.0).then(|| a.map(|v| v / len))
}
fn compare(a: Vec3, b: Vec3) -> std::cmp::Ordering {
    (0..3)
        .map(|i| a[i].total_cmp(&b[i]))
        .find(|c| c.is_ne())
        .unwrap_or(std::cmp::Ordering::Equal)
}
fn vertex(face: &BRepFace, index: u32) -> Option<Vec3> {
    let start = (index as usize).checked_mul(3)?;
    face.vertices.get(start..)?.first_chunk::<3>().copied()
}
fn triangles(face: &BRepFace) -> impl Iterator<Item = [Vec3; 3]> + '_ {
    face.indices.as_chunks::<3>().0.iter().filter_map(|t| {
        Some([
            vertex(face, t[0])?,
            vertex(face, t[1])?,
            vertex(face, t[2])?,
        ])
    })
}

struct Plane<'a> {
    normal: Vec3,
    origin: Vec3,
    area: f64,
    patches: Vec<&'a BRepFace>,
}

struct Cylinder<'a> {
    axis: Vec3,
    line: Vec3,
    lo: f64,
    hi: f64,
    radius: f64,
    area: f64,
    inward: bool,
    component: Option<&'a str>,
}

impl<'a> Cylinder<'a> {
    fn from_face(face: &'a BRepFace) -> Option<Self> {
        let radius = face.radius_hint.filter(|r| r.is_finite() && *r > DIST)?;
        let mut axis = unit(face.normal_hint)?;
        // Canonicalize the axis independently of the orientation of the wall.
        let dominant = (0..3).max_by(|&i, &j| axis[i].abs().total_cmp(&axis[j].abs()))?;
        if axis[dominant] < 0.0 {
            axis = scale(axis, -1.0);
        }
        let center = face.center_hint;
        let line = sub(center, scale(axis, dot(center, axis)));
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut radial_score, mut area_twice) = (0.0, 0.0);
        for [a, b, c] in triangles(face) {
            for p in [a, b, c] {
                let h = dot(p, axis);
                lo = lo.min(h);
                hi = hi.max(h);
            }
            let mid = scale(add(add(a, b), c), 1.0 / 3.0);
            let radial = sub(sub(mid, line), scale(axis, dot(mid, axis)));
            if let Some(dir) = unit(radial) {
                let normal_area = cross(sub(b, a), sub(c, a));
                radial_score += dot(dir, normal_area);
                area_twice += norm(normal_area);
            }
        }
        if !(hi - lo > DIST && radial_score.abs() > area_twice * 0.5) {
            return None;
        }
        Some(Self {
            axis,
            line,
            lo,
            hi,
            radius,
            area: face.area_mm2,
            inward: radial_score < 0.0,
            component: face.component_id.as_deref(),
        })
    }

    fn midpoint(&self) -> Vec3 {
        add(
            self.line,
            scale(self.axis, self.lo + (self.hi - self.lo) * 0.5),
        )
    }

    fn compatible(&self, other: &Self) -> bool {
        self.inward == other.inward
            && self.component == other.component
            && (self.radius - other.radius).abs() < DIST
            && dot(self.axis, other.axis) >= ANGLE_DOT
            && norm(cross(sub(other.midpoint(), self.midpoint()), self.axis)) < DIST
            && self.lo <= other.hi + DIST
            && other.lo <= self.hi + DIST
    }
}

fn features(faces: &[BRepFace]) -> (Vec<DatumPlane>, Vec<HoleGroup>, Vec<Boss>) {
    let mut planes: Vec<Plane<'_>> = Vec::new();
    let mut plane_buckets: HashMap<(Option<&str>, [i32; 3]), Vec<usize>> = HashMap::new();
    let mut cylinder_slots: Vec<Option<Cylinder<'_>>> = Vec::new();
    let mut cylinder_buckets: HashMap<(bool, Option<&str>, i64), Vec<usize>> = HashMap::new();
    let mut selected = faces
        .iter()
        .filter(|f| {
            matches!(f.surface_type, SurfaceType::Plane | SurfaceType::Cylinder)
                && f.area_mm2.is_finite()
                && f.area_mm2 > 0.0
                && f.center_hint.iter().all(|v| v.is_finite())
        })
        .collect::<Vec<_>>();
    selected.sort_by(|a, b| {
        a.component_id
            .cmp(&b.component_id)
            .then(compare(a.center_hint, b.center_hint))
            .then(a.face_id.cmp(&b.face_id))
    });
    for face in selected {
        if face.surface_type == SurfaceType::Plane {
            let Some(normal) = unit(face.normal_hint) else {
                continue;
            };
            let origin = face.center_hint;
            let component = face.component_id.as_deref();
            let key = quantize_normal(normal);
            let mut matched: Option<usize> = None;
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let neighbor = [key[0] + dx, key[1] + dy, key[2] + dz];
                        if let Some(bucket) = plane_buckets.get(&(component, neighbor)) {
                            for &idx in bucket {
                                if matched.is_some_and(|best| idx >= best) {
                                    continue;
                                }
                                let p = &planes[idx];
                                if dot(p.normal, normal) >= ANGLE_DOT
                                    && dot(p.normal, sub(origin, p.origin)).abs() < DIST
                                {
                                    matched = Some(idx);
                                }
                            }
                        }
                    }
                }
            }
            if let Some(idx) = matched {
                let p = &mut planes[idx];
                let total = p.area + face.area_mm2;
                p.origin = add(
                    p.origin,
                    scale(sub(origin, p.origin), face.area_mm2 / total),
                );
                p.area = total;
                p.patches.push(face);
            } else {
                let idx = planes.len();
                planes.push(Plane {
                    normal,
                    origin,
                    area: face.area_mm2,
                    patches: vec![face],
                });
                plane_buckets.entry((component, key)).or_default().push(idx);
            }
        } else if let Some(mut c) = Cylinder::from_face(face) {
            // A periodic wall can consist of multiple patches, e.g. two STEP half-faces.
            let r_cell = quantize_scalar(c.radius, DIST);
            loop {
                let mut found: Option<usize> = None;
                for dr in -1..=1 {
                    if let Some(bucket) =
                        cylinder_buckets.get(&(c.inward, c.component, r_cell + dr))
                    {
                        for &idx in bucket {
                            if found.is_some_and(|best| idx >= best) {
                                continue;
                            }
                            if let Some(old) = &cylinder_slots[idx]
                                && old.compatible(&c)
                            {
                                found = Some(idx);
                            }
                        }
                    }
                }
                let Some(idx) = found else {
                    break;
                };
                let old = cylinder_slots[idx].take().unwrap();
                c.lo = c.lo.min(old.lo);
                c.hi = c.hi.max(old.hi);
                c.area += old.area;
            }
            let idx = cylinder_slots.len();
            let key = (c.inward, c.component, r_cell);
            cylinder_slots.push(Some(c));
            cylinder_buckets.entry(key).or_default().push(idx);
        }
    }
    planes.sort_by(|a, b| {
        b.area
            .total_cmp(&a.area)
            .then(compare(a.origin, b.origin))
            .then(compare(a.normal, b.normal))
    });
    let mut cylinders: Vec<Cylinder<'_>> = cylinder_slots.into_iter().flatten().collect();
    cylinders
        .sort_by(|a, b| compare(a.midpoint(), b.midpoint()).then(a.radius.total_cmp(&b.radius)));

    let sorted_plane_buckets = if cylinders.is_empty() {
        HashMap::new()
    } else {
        let mut buckets: HashMap<(Option<&str>, [i32; 3]), Vec<usize>> =
            HashMap::with_capacity(planes.len());
        for (idx, p) in planes.iter().enumerate() {
            buckets
                .entry((
                    p.patches[0].component_id.as_deref(),
                    quantize_normal(p.normal),
                ))
                .or_default()
                .push(idx);
        }
        buckets
    };

    let mut holes: Vec<(Option<&str>, HoleGroup)> = Vec::new();
    let mut hole_buckets: HashMap<(Option<&str>, i64, i64), Vec<usize>> = HashMap::new();
    let mut bosses = Vec::new();
    for c in cylinders {
        let depth = c.hi - c.lo;
        // Reject partial arcs (e.g. a corner fillet) rather than invent a full bore or boss.
        if c.area < 0.9 * std::f64::consts::TAU * c.radius * depth {
            continue;
        }
        let low = add(c.line, scale(c.axis, c.lo));
        let high = add(c.line, scale(c.axis, c.hi));
        let low_end = end_planes(
            &planes,
            &sorted_plane_buckets,
            low,
            scale(c.axis, -1.0),
            c.component,
        );
        let high_end = end_planes(&planes, &sorted_plane_buckets, high, c.axis, c.component);
        if c.inward {
            // A blind bore opens opposite its cap. For through holes, use the larger datum.
            let use_high =
                (low_end.0 && !high_end.0) || (low_end.0 == high_end.0 && high_end.1 >= low_end.1);
            let (center, axis) = if use_high {
                (high, scale(c.axis, -1.0))
            } else {
                (low, c.axis)
            };
            let diameter = 2.0 * c.radius;
            let d_cell = quantize_scalar(diameter, DIST);
            let h_cell = quantize_scalar(depth, DIST);
            let mut matched: Option<usize> = None;
            for dd in -1..=1 {
                for dh in -1..=1 {
                    if let Some(bucket) = hole_buckets.get(&(c.component, d_cell + dd, h_cell + dh))
                    {
                        for &idx in bucket {
                            if matched.is_some_and(|best| idx >= best) {
                                continue;
                            }
                            let (component, g) = &holes[idx];
                            if *component == c.component
                                && dot(g.axis, axis) >= ANGLE_DOT
                                && (g.diameter_mm - diameter).abs() < DIST
                                && (g.depth_mm - depth).abs() < DIST
                                && dot(axis, sub(center, g.centers_mm[0])).abs() < DIST
                            {
                                matched = Some(idx);
                            }
                        }
                    }
                }
            }
            if let Some(idx) = matched {
                holes[idx].1.centers_mm.push(center);
            } else {
                let idx = holes.len();
                holes.push((
                    c.component,
                    HoleGroup {
                        group_id: format!("holes_{}", holes.len() + 1),
                        pattern: "single",
                        count: 1,
                        diameter_mm: diameter,
                        depth_mm: depth,
                        axis,
                        centers_mm: vec![center],
                        pitch_circle_diameter_mm: None,
                    },
                ));
                hole_buckets
                    .entry((c.component, d_cell, h_cell))
                    .or_default()
                    .push(idx);
            }
        } else {
            let (base_center_mm, axis) = if low_end.0 && !high_end.0 {
                (high, scale(c.axis, -1.0))
            } else {
                (low, c.axis)
            };
            bosses.push(Boss {
                id: format!("boss_{}", bosses.len() + 1),
                diameter_mm: 2.0 * c.radius,
                height_mm: depth,
                axis,
                base_center_mm,
            });
        }
    }
    let holes = holes
        .into_iter()
        .map(|(_, mut g)| {
            g.centers_mm.sort_by(|a, b| compare(*a, *b));
            g.count = g.centers_mm.len();
            (g.pattern, g.pitch_circle_diameter_mm) = classify_pattern(&g.centers_mm, g.axis);
            g
        })
        .collect();
    let datums = planes
        .into_iter()
        .enumerate()
        .map(|(i, p)| DatumPlane {
            id: format!("datum_{}", i + 1),
            normal: p.normal,
            origin_mm: p.origin,
            area_mm2: p.area,
        })
        .collect();
    (datums, holes, bosses)
}

/// Find a cap (the endpoint is covered) and the largest nearby outward-facing datum.
fn end_planes(
    planes: &[Plane<'_>],
    plane_buckets: &HashMap<(Option<&str>, [i32; 3]), Vec<usize>>,
    center: Vec3,
    outward: Vec3,
    component: Option<&str>,
) -> (bool, f64) {
    let mut covered = false;
    let mut area: f64 = 0.0;
    for dir in [outward, scale(outward, -1.0)] {
        let key = quantize_normal(dir);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let neighbor = [key[0] + dx, key[1] + dy, key[2] + dz];
                    let Some(bucket) = plane_buckets.get(&(component, neighbor)) else {
                        continue;
                    };
                    for &idx in bucket {
                        let p = &planes[idx];
                        if dot(p.normal, outward).abs() < ANGLE_DOT
                            || dot(p.normal, sub(center, p.origin)).abs() >= DIST
                        {
                            continue;
                        }
                        if dot(p.normal, outward) > 0.0 {
                            area = area.max(p.area);
                        }
                        covered |= p.patches.iter().any(|face| {
                            triangles(face).any(|[a, b, c]| {
                                let d = [(a, b), (b, c), (c, a)].map(|(u, v)| {
                                    let edge = sub(v, u);
                                    dot(p.normal, cross(edge, sub(center, u))) / norm(edge)
                                });
                                d.iter().all(|&s| s >= -DIST) || d.iter().all(|&s| s <= DIST)
                            })
                        });
                    }
                }
            }
        }
    }
    (covered, area)
}

fn classify_pattern(points: &[Vec3], axis: Vec3) -> (&'static str, Option<f64>) {
    if points.len() == 1 {
        return ("single", None);
    }
    let a = points[0];
    let b = points
        .iter()
        .copied()
        .max_by(|b, c| norm(sub(*b, a)).total_cmp(&norm(sub(*c, a))))
        .unwrap();
    let ab = sub(b, a);
    let length = norm(ab);
    if length <= DIST {
        return ("irregular", None);
    }
    let direction = scale(ab, 1.0 / length);
    let off_line = |p| dot(cross(direction, sub(p, a)), axis).abs();
    let c = points
        .iter()
        .copied()
        .max_by(|b, c| off_line(*b).total_cmp(&off_line(*c)))
        .unwrap();
    if off_line(c) <= DIST {
        return ("linear", None);
    }
    let ac = sub(c, a);
    let side = cross(axis, direction);
    let y = (dot(ac, ac) - length * dot(ac, direction)) / (2.0 * dot(ac, side));
    let center = add(a, add(scale(direction, length / 2.0), scale(side, y)));
    let radius = norm(sub(center, a));
    let tolerance = DIST.max(radius * 1e-4);
    if !points
        .iter()
        .all(|p| (norm(sub(*p, center)) - radius).abs() < tolerance)
    {
        return ("irregular", None);
    }
    let mut angles = points
        .iter()
        .map(|p| {
            let v = sub(*p, center);
            dot(v, side).atan2(dot(v, direction))
        })
        .collect::<Vec<_>>();
    angles.sort_by(f64::total_cmp);
    let tau = std::f64::consts::TAU;
    if angles.len() == 4
        && (angles[2] - angles[0] - tau / 2.0).abs() < 1e-3
        && (angles[3] - angles[1] - tau / 2.0).abs() < 1e-3
    {
        return ("rectangular", Some(radius * 2.0));
    }
    let gap = tau / angles.len() as f64;
    if angles
        .iter()
        .zip(angles.iter().cycle().skip(1))
        .all(|(a, b)| ((b - a).rem_euclid(tau) - gap).abs() < 1e-3)
    {
        ("polar", Some(radius * 2.0))
    } else {
        ("irregular", None)
    }
}
