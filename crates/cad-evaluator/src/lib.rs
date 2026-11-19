use cad_gcs::{Constraint, EdgeId, PointId, SketchGeom, SketchSystem, SolveStatus};
use cad_hir::{
    Diagnostic, IrArg, IrExpr, IrModule, IrPart, IrSketchStmt, Severity, check_and_lower,
};
use cad_kernel_occt::{
    AnalyticKernel, BBox3, CheckReport, ComponentMeta, CsgNode, GeometryKernel, Mat4, PmiRecord,
    SketchPlane, SolidShape, SurfaceType, transform_solid,
};
use cad_render::{EditHandle, SceneMeshPayload, build_scene_mesh};
use cad_syntax::{BinaryOp, Span, UnaryOp, parse};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct EvaluationOutput {
    pub status_ok: bool,
    pub part_name: String,
    pub is_assembly: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub check_report: CheckReport,
    pub solid: SolidShape,
    /// Components in part-local coordinates, keyed by the ID in the assembly BOM.
    pub component_solids: HashMap<String, SolidShape>,
    pub scene_mesh: SceneMeshPayload,
    pub compile_time_ms: f64,
    pub kernel_backend: String,
}

#[derive(Debug, Clone)]
struct FaceSelection {
    faces: Vec<cad_kernel_occt::BRepFace>,
    disambiguated: bool,
}

#[derive(Debug, Clone)]
enum Val {
    Number(f64),
    Bool(bool),
    Str(String),
    List(Vec<Val>),
    Dir([f64; 3]),
    Plane(SketchPlane),
    SketchProfile {
        plane: SketchPlane,
        polygon_2d: Vec<[f64; 2]>,
    },
    SketchPath(Vec<[f64; 3]>),
    Solid(SolidShape),
    Component {
        part_name: String,
        solid: SolidShape,
        origin: [f64; 3],
        rotation_deg: [f64; 3],
    },
    FaceSet(FaceSelection),
    Unit,
}

impl Val {
    fn as_num(&self) -> Option<f64> {
        match self {
            Val::Number(v) => Some(*v),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn as_vec3(&self) -> Option<[f64; 3]> {
        match self {
            Val::List(v) if v.len() == 3 => Some([v[0].as_num()?, v[1].as_num()?, v[2].as_num()?]),
            Val::Dir(d) => Some(*d),
            _ => None,
        }
    }

    fn as_vec2(&self) -> Option<[f64; 2]> {
        match self {
            Val::List(v) if v.len() == 2 => Some([v[0].as_num()?, v[1].as_num()?]),
            _ => None,
        }
    }
}

pub fn evaluate_source(source: &str) -> EvaluationOutput {
    let t0 = Instant::now();
    let parsed = parse(source);
    let mut diagnostics = Vec::new();

    for e in &parsed.lex_errors {
        diagnostics.push(Diagnostic {
            code: "E002_SYNTAX_ERROR".to_string(),
            severity: Severity::Error,
            message: e.message.clone(),
            span: e.span,
        });
    }
    for e in &parsed.syntax_errors {
        diagnostics.push(Diagnostic {
            code: "E002_SYNTAX_ERROR".to_string(),
            severity: Severity::Error,
            message: e.message.clone(),
            span: e.span,
        });
    }

    let hir = check_and_lower(&parsed.file);
    diagnostics.extend(hir.diagnostics.clone());

    let has_fatal = diagnostics.iter().any(|d| d.severity == Severity::Error);
    let kernel = AnalyticKernel::new();

    if has_fatal || (hir.module.parts.is_empty() && hir.module.assemblies.is_empty()) {
        if hir.module.parts.is_empty() && hir.module.assemblies.is_empty() && !has_fatal {
            diagnostics.push(Diagnostic {
                code: "E002_SYNTAX_ERROR".to_string(),
                severity: Severity::Error,
                message: "no `part` or `assembly` declaration found in file".to_string(),
                span: Span::default(),
            });
        }
        let empty = SolidShape::empty("error_part");
        let chk = kernel.check_solid(&empty);
        let mesh = build_scene_mesh(&empty, Vec::new());
        return EvaluationOutput {
            status_ok: false,
            part_name: hir
                .module
                .parts
                .first()
                .or_else(|| hir.module.assemblies.first())
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            is_assembly: !hir.module.assemblies.is_empty(),
            diagnostics,
            check_report: CheckReport {
                watertight: false,
                volume_mm3: 0.0,
                surface_area_mm2: 0.0,
                bbox_mm: BBox3::default(),
                face_count: 0,
                edge_count: 0,
                open_edge_count: 0,
                issues: chk.issues,
            },
            solid: empty,
            component_solids: HashMap::new(),
            scene_mesh: mesh,
            compile_time_ms: t0.elapsed().as_secs_f64() * 1000.0,
            kernel_backend: kernel.backend_name().to_string(),
        };
    }

    let is_assembly = !hir.module.assemblies.is_empty();
    let target_ir = if is_assembly {
        &hir.module.assemblies[hir.module.assemblies.len() - 1]
    } else {
        &hir.module.parts[hir.module.parts.len() - 1]
    };
    let mut eval = Evaluator::new(&kernel, &hir.module);
    eval.eval_constants();
    let mut solid = if is_assembly {
        eval.eval_assembly(target_ir)
    } else {
        eval.eval_part(target_ir, &HashMap::new())
    };
    solid.part_name = target_ir.name.clone();
    diagnostics.extend(eval.diagnostics.clone());

    let check_report = kernel.check_solid(&solid);
    if !check_report.watertight {
        diagnostics.push(Diagnostic {
            code: "E301_NON_WATERTIGHT".to_string(),
            severity: Severity::Error,
            message: format!(
                "solid `{}` failed watertight check: {:?}",
                target_ir.name, check_report.issues
            ),
            span: target_ir.span,
        });
    }

    let edit_handles = target_ir
        .edit_handles
        .iter()
        .filter_map(|h| {
            let val = eval
                .env
                .get(&h.param_name)
                .and_then(|v| v.as_num())
                .or(h.current_value)?;
            Some(EditHandle {
                handle_id: h.handle_id.clone(),
                param_name: h.param_name.clone(),
                tier: h.tier,
                current_value: val,
                unit: if h.unit.is_empty() {
                    "unitless".to_string()
                } else {
                    h.unit.clone()
                },
                ast_span: h.ast_span,
                upstream_param: h.classification.base_param.clone(),
                offset_value: h.classification.offset_value,
            })
        })
        .collect();

    let scene_mesh = build_scene_mesh(&solid, edit_handles);
    let status_ok = !diagnostics.iter().any(|d| d.severity == Severity::Error);

    EvaluationOutput {
        status_ok,
        part_name: target_ir.name.clone(),
        is_assembly,
        diagnostics,
        check_report,
        solid,
        component_solids: eval.component_solids,
        scene_mesh,
        compile_time_ms: t0.elapsed().as_secs_f64() * 1000.0,
        kernel_backend: kernel.backend_name().to_string(),
    }
}

struct Evaluator<'a> {
    kernel: &'a AnalyticKernel,
    module: &'a IrModule,
    parts_by_name: HashMap<String, &'a IrPart>,
    env: HashMap<String, Val>,
    diagnostics: Vec<Diagnostic>,
    pmi_records: Vec<PmiRecord>,
    component_solids: HashMap<String, SolidShape>,
    check_cache: HashMap<(String, usize, usize), CheckReport>,
}

impl<'a> Evaluator<'a> {
    fn new(kernel: &'a AnalyticKernel, module: &'a IrModule) -> Self {
        let mut env = HashMap::new();
        env.insert("X_AXIS".to_string(), Val::Dir([1.0, 0.0, 0.0]));
        env.insert("Y_AXIS".to_string(), Val::Dir([0.0, 1.0, 0.0]));
        env.insert("Z_AXIS".to_string(), Val::Dir([0.0, 0.0, 1.0]));
        env.insert("X".to_string(), Val::Dir([1.0, 0.0, 0.0]));
        env.insert("Y".to_string(), Val::Dir([0.0, 1.0, 0.0]));
        env.insert("Z".to_string(), Val::Dir([0.0, 0.0, 1.0]));
        env.insert("XY_PLANE".to_string(), Val::Plane(SketchPlane::Xy));
        env.insert("XZ_PLANE".to_string(), Val::Plane(SketchPlane::Xz));
        env.insert("YZ_PLANE".to_string(), Val::Plane(SketchPlane::Yz));

        let mut parts_by_name = HashMap::new();
        for p in &module.parts {
            parts_by_name.insert(p.name.clone(), p);
        }

        Self {
            kernel,
            module,
            parts_by_name,
            env,
            diagnostics: Vec::new(),
            pmi_records: Vec::new(),
            component_solids: HashMap::new(),
            check_cache: HashMap::new(),
        }
    }

    /// R23: Caches `check_solid` results per solid structure within one evaluation pass.
    fn cached_check_solid(&mut self, solid: &SolidShape) -> CheckReport {
        let key = (
            solid.part_name.clone(),
            solid.faces.len(),
            solid.edges.len(),
        );
        if let Some(cached) = self.check_cache.get(&key) {
            return cached.clone();
        }
        let report = self.kernel.check_solid(solid);
        self.check_cache.insert(key, report.clone());
        report
    }

    /// R15: Evaluates top-level `const` / `let` bindings into initial evaluator scope.
    fn eval_constants(&mut self) {
        for c in &self.module.constants {
            let val = self.eval_expr(&c.expr);
            if let Some(name) = &c.name {
                self.env.insert(name.clone(), val);
            }
        }
    }

    fn emit(&mut self, code: &str, severity: Severity, message: impl Into<String>, span: Span) {
        self.diagnostics.push(Diagnostic {
            code: code.to_string(),
            severity,
            message: message.into(),
            span,
        });
    }

    fn eval_part(&mut self, part: &IrPart, overrides: &HashMap<String, Val>) -> SolidShape {
        for p in &part.params {
            if let Some(v) = overrides.get(&p.name) {
                self.env.insert(p.name.clone(), v.clone());
            } else if let Some(def) = &p.default_expr {
                let v = self.eval_expr(def);
                self.env.insert(p.name.clone(), v);
            }
        }

        let mut returned: Option<SolidShape> = None;
        for node in &part.nodes {
            let val = self.eval_expr(&node.expr);
            if let Some(name) = &node.name {
                self.env.insert(name.clone(), val.clone());
            }
            if part.return_node == Some(node.id) {
                if let Val::Solid(s) = val {
                    returned = Some(s);
                } else {
                    self.emit(
                        "E_RETURN_NOT_SOLID",
                        Severity::Error,
                        "part return value did not evaluate to a Solid",
                        node.span,
                    );
                }
            }
        }

        let mut solid = returned.unwrap_or_else(|| SolidShape::empty(&part.name));
        solid.pmi.extend(self.pmi_records.clone());
        solid
    }

    fn eval_assembly(&mut self, asm: &IrPart) -> SolidShape {
        for p in &asm.params {
            if let Some(def) = &p.default_expr {
                let v = self.eval_expr(def);
                self.env.insert(p.name.clone(), v);
            }
        }

        let mut ret_val: Option<Val> = None;
        for node in &asm.nodes {
            let val = self.eval_expr(&node.expr);
            if let Some(name) = &node.name {
                self.env.insert(name.clone(), val.clone());
            }
            if asm.return_node == Some(node.id) {
                ret_val = Some(val);
            }
        }

        let comps: Vec<(String, SolidShape, [f64; 3], [f64; 3])> = match ret_val {
            Some(Val::List(items)) => items
                .into_iter()
                .filter_map(|v| match v {
                    Val::Component {
                        part_name,
                        solid,
                        origin,
                        rotation_deg,
                    } => Some((part_name, solid, origin, rotation_deg)),
                    _ => None,
                })
                .collect(),
            Some(Val::Solid(s)) => return s,
            _ => Vec::new(),
        };

        // Assembly placement groups independent shells rather than boolean-fusing them.
        let mut asm_solid = SolidShape::empty(&asm.name);
        let mut csg_items = Vec::with_capacity(comps.len());
        for (idx, (part_name, solid, origin, rotation_deg)) in comps.into_iter().enumerate() {
            let cid = format!("comp_{idx}_{part_name}");
            let mat = placement(origin, rotation_deg);
            self.component_solids.insert(cid.clone(), solid.clone());
            let mut placed = transform_solid(&solid, mat);
            for f in &mut placed.faces {
                f.component_id = Some(cid.clone());
            }
            for e in &mut placed.edges {
                e.component_id = Some(cid.clone());
            }
            // Instances of the same part can have equal topology counts but distinct
            // dimensions or placements; check their geometry rather than caching by name.
            let chk = self.kernel.check_solid(&placed);
            if !chk.watertight {
                self.emit(
                    "E301_NON_WATERTIGHT",
                    Severity::Error,
                    format!(
                        "component `{cid}` failed watertight check: {:?}",
                        chk.issues
                    ),
                    asm.span,
                );
            }
            asm_solid.components.push(ComponentMeta {
                component_id: cid,
                part_name,
                origin_mm: origin,
                rotation_deg,
                volume_mm3: chk.volume_mm3,
                bbox_mm: chk.bbox_mm,
            });
            for f in &mut placed.faces {
                f.face_id = format!("comp_{idx}_{}", f.face_id);
            }
            for e in &mut placed.edges {
                e.edge_id = format!("comp_{idx}_{}", e.edge_id);
            }
            csg_items.push(placed.csg);
            asm_solid.faces.extend(placed.faces);
            asm_solid.edges.extend(placed.edges);
            asm_solid.pmi.extend(placed.pmi);
            asm_solid.exact_brep &= placed.exact_brep;
        }
        asm_solid.csg = CsgNode::Union(csg_items);
        asm_solid
    }

    fn eval_expr(&mut self, expr: &IrExpr) -> Val {
        match expr {
            IrExpr::Quantity {
                canonical_value, ..
            } => Val::Number(*canonical_value),
            IrExpr::Str { value, .. } => Val::Str(value.clone()),
            IrExpr::Bool { value, .. } => Val::Bool(*value),
            IrExpr::Var { name, .. } | IrExpr::ConstDir { name, .. } => {
                self.env.get(name).cloned().unwrap_or(Val::Unit)
            }
            IrExpr::Array { elements, .. } => {
                Val::List(elements.iter().map(|e| self.eval_expr(e)).collect())
            }
            IrExpr::Unary { op, operand, .. } => {
                let v = self.eval_expr(operand);
                match (op, v) {
                    (UnaryOp::Plus, x) => x,
                    (UnaryOp::Minus, Val::Number(n)) => Val::Number(-n),
                    (UnaryOp::Minus, Val::Dir(d)) => Val::Dir([-d[0], -d[1], -d[2]]),
                    (UnaryOp::Not, Val::Bool(b)) => Val::Bool(!b),
                    (_, other) => other,
                }
            }
            IrExpr::Binary {
                op, left, right, ..
            } => {
                let l = self.eval_expr(left);
                let r = self.eval_expr(right);
                match (l, r) {
                    (Val::Number(a), Val::Number(b)) => match op {
                        BinaryOp::Add => Val::Number(a + b),
                        BinaryOp::Sub => Val::Number(a - b),
                        BinaryOp::Mul => Val::Number(a * b),
                        BinaryOp::Div => Val::Number(if b.abs() > 1e-12 { a / b } else { 0.0 }),
                        BinaryOp::Rem => Val::Number(a % b),
                        BinaryOp::Eq => Val::Bool((a - b).abs() < 1e-9),
                        BinaryOp::Ne => Val::Bool((a - b).abs() >= 1e-9),
                        BinaryOp::Lt => Val::Bool(a < b),
                        BinaryOp::Le => Val::Bool(a <= b),
                        BinaryOp::Gt => Val::Bool(a > b),
                        BinaryOp::Ge => Val::Bool(a >= b),
                        _ => Val::Unit,
                    },
                    (Val::Bool(a), Val::Bool(b)) => match op {
                        BinaryOp::And => Val::Bool(a && b),
                        BinaryOp::Or => Val::Bool(a || b),
                        BinaryOp::Eq => Val::Bool(a == b),
                        BinaryOp::Ne => Val::Bool(a != b),
                        _ => Val::Unit,
                    },
                    _ => Val::Unit,
                }
            }
            IrExpr::Call {
                callee, args, span, ..
            } => self.eval_call(callee, args, None, *span),
            IrExpr::MethodCall {
                receiver,
                method,
                args,
                span,
                ..
            } => {
                let recv = self.eval_expr(receiver);
                self.eval_method(recv, method, args, *span)
            }
            IrExpr::FieldAccess { .. } => Val::Unit,
            IrExpr::Sketch { args, stmts, span } => self.eval_sketch(args, stmts, *span),
            IrExpr::Pipe { left, right, .. } => {
                let lhs = self.eval_expr(left);
                match right.as_ref() {
                    IrExpr::Call {
                        callee, args, span, ..
                    } => self.eval_call(callee, args, Some(lhs), *span),
                    IrExpr::MethodCall {
                        method, args, span, ..
                    } => self.eval_method(lhs, method, args, *span),
                    _ => self.eval_expr(right),
                }
            }
        }
    }

    fn eval_args(&mut self, args: &[IrArg]) -> Vec<(Option<String>, Val)> {
        args.iter()
            .map(|a| (a.name.clone(), self.eval_expr(&a.value)))
            .collect()
    }

    fn eval_call(&mut self, callee: &str, args: &[IrArg], piped: Option<Val>, span: Span) -> Val {
        let vals = self.eval_args(args);
        let get = |name: &str| -> Option<&Val> {
            vals.iter()
                .find(|(k, _)| k.as_deref() == Some(name))
                .map(|(_, v)| v)
        };
        let pos0 = || vals.first().map(|(_, v)| v);

        match callee {
            "box" => {
                let size = get("size")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([10.0, 10.0, 10.0]);
                let origin = get("origin")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([0.0, 0.0, 0.0]);
                Val::Solid(self.kernel.make_box(size, origin, span))
            }
            "cylinder" => {
                let d = get("d").and_then(|v| v.as_num()).unwrap_or(10.0);
                let h = get("h").and_then(|v| v.as_num()).unwrap_or(10.0);
                let origin = get("origin")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([0.0, 0.0, 0.0]);
                let axis = get("axis")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([0.0, 0.0, 1.0]);
                let exact = match get("exact_brep") {
                    Some(Val::Bool(b)) => *b,
                    _ => true,
                };
                Val::Solid(self.kernel.make_cylinder(d, h, origin, axis, exact, span))
            }
            "extrude" | "extrude_symmetric" => {
                let symmetric = callee == "extrude_symmetric";
                let h = get("height")
                    .or_else(|| get("thickness"))
                    .or_else(pos0)
                    .and_then(|v| v.as_num())
                    .unwrap_or(10.0);
                let profile = piped
                    .as_ref()
                    .or_else(|| get("sketch"))
                    .or_else(|| get("profile"));
                if let Some(Val::SketchProfile { plane, polygon_2d }) = profile {
                    Val::Solid(self.kernel.extrude(*plane, polygon_2d, h, symmetric, span))
                } else {
                    self.emit(
                        "E203_UNCLOSED_SKETCH",
                        Severity::Error,
                        "extrude requires a valid closed sketch profile",
                        span,
                    );
                    Val::Solid(SolidShape::empty("invalid_extrude"))
                }
            }
            "revolve" => {
                let axis = get("axis")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([0.0, 0.0, 1.0]);
                let angle = get("angle").and_then(|v| v.as_num()).unwrap_or(360.0);
                let profile = piped
                    .as_ref()
                    .or_else(|| get("sketch"))
                    .or_else(|| get("profile"));
                if let Some(Val::SketchProfile { plane, polygon_2d }) = profile {
                    Val::Solid(self.kernel.revolve(*plane, polygon_2d, axis, angle, span))
                } else {
                    self.emit(
                        "E203_UNCLOSED_SKETCH",
                        Severity::Error,
                        "revolve requires a valid closed sketch profile",
                        span,
                    );
                    Val::Solid(SolidShape::empty("invalid_revolve"))
                }
            }
            "shell" | "mirror" | "sweep" | "loft" | "hole::tapped" => {
                let result = (|| -> Result<SolidShape, String> {
                    let solid = || match piped.as_ref().or_else(|| get("solid")) {
                        Some(Val::Solid(s)) => Ok(s),
                        _ => Err(format!("{callee} requires an input solid")),
                    };
                    match callee {
                        "shell" => {
                            let thickness = get("thickness")
                                .or_else(pos0)
                                .and_then(Val::as_num)
                                .ok_or("shell requires thickness")?;
                            let mut normals = Vec::new();
                            if let Some(value) = get("open_faces") {
                                let Val::List(items) = value else {
                                    return Err("open_faces must be a list".into());
                                };
                                for item in items {
                                    match item {
                                        Val::FaceSet(fs) if fs.faces.len() == 1 && fs.faces[0].surface_type == SurfaceType::Plane => {
                                            normals.push(fs.faces[0].normal_hint);
                                        }
                                        Val::Dir(n) => normals.push(*n),
                                        _ => return Err("open_faces entries must be single planar faces or face normals".into()),
                                    }
                                }
                            }
                            self.kernel.shell(solid()?, thickness, &normals, span)
                        }
                        "mirror" => {
                            let Some(Val::Plane(plane)) = get("plane").or_else(pos0) else {
                                return Err("mirror requires a datum plane".into());
                            };
                            let merge = !matches!(get("merge"), Some(Val::Bool(false)));
                            Ok(self.kernel.mirror(solid()?, *plane, merge))
                        }
                        "sweep" => {
                            let profile = piped.as_ref().or_else(|| get("profile")).or_else(pos0);
                            let Some(Val::SketchProfile {
                                plane: SketchPlane::Xy,
                                polygon_2d,
                            }) = profile
                            else {
                                return Err(
                                    "sweep requires a closed XY profile in path-local coordinates"
                                        .into(),
                                );
                            };
                            let path = get("path")
                                .or_else(|| vals.get(usize::from(piped.is_none())).map(|(_, v)| v));
                            let points = match path {
                                Some(Val::SketchPath(points)) => points.clone(),
                                Some(Val::List(items)) => items
                                    .iter()
                                    .map(Val::as_vec3)
                                    .collect::<Option<Vec<_>>>()
                                    .ok_or("sweep path requires 3D points")?,
                                _ => return Err("sweep requires a path".into()),
                            };
                            if points.iter().flatten().any(|x| !x.is_finite())
                                || points.windows(2).any(|pair| {
                                    let d = delta(pair[0], pair[1]);
                                    dot(d, d) <= 1e-12
                                })
                            {
                                return Err(
                                    "sweep path must have finite, distinct consecutive points"
                                        .into(),
                                );
                            }
                            self.kernel.sweep(polygon_2d, &points, span)
                        }
                        "loft" => {
                            let Some(Val::List(profiles)) = get("sections").or_else(pos0) else {
                                return Err("loft requires a list of sections".into());
                            };
                            let height = get("height").and_then(Val::as_num).unwrap_or(10.0);
                            let offsets = if let Some(value) = get("offsets") {
                                let Val::List(items) = value else {
                                    return Err("loft offsets must be a list".into());
                                };
                                if items.len() != profiles.len() {
                                    return Err("loft requires one offset per section".into());
                                }
                                items
                                    .iter()
                                    .map(Val::as_num)
                                    .collect::<Option<Vec<_>>>()
                                    .ok_or("loft offsets must be lengths")?
                            } else {
                                (0..profiles.len())
                                    .map(|i| {
                                        height * i as f64
                                            / profiles.len().saturating_sub(1).max(1) as f64
                                    })
                                    .collect()
                            };
                            let sections = profiles
                                .iter()
                                .zip(offsets)
                                .map(|(profile, offset)| {
                                    if let Val::SketchProfile { plane, polygon_2d } = profile {
                                        Ok((polygon_2d.clone(), *plane, offset))
                                    } else {
                                        Err("loft sections must be closed sketches")
                                    }
                                })
                                .collect::<Result<Vec<_>, _>>()?;
                            self.kernel.loft(&sections, span)
                        }
                        "hole::tapped" => {
                            let spec = get("spec")
                                .or_else(pos0)
                                .and_then(Val::as_str)
                                .ok_or("tapped hole requires spec")?;
                            let depth = get("depth")
                                .or_else(|| vals.get(1).map(|(_, v)| v))
                                .and_then(Val::as_num)
                                .filter(|d| d.is_finite() && *d > 0.0)
                                .ok_or("tapped hole requires positive depth")?;
                            let origin = get("origin").and_then(Val::as_vec3).unwrap_or([0.0; 3]);
                            let axis = get("axis")
                                .and_then(Val::as_vec3)
                                .unwrap_or([0.0, 0.0, -1.0]);
                            // The subtractor expects vertical tools expressed along +Z.
                            let (origin, axis) =
                                if axis[0].abs() < 1e-9 && axis[1].abs() < 1e-9 && axis[2] < 0.0 {
                                    ([origin[0], origin[1], origin[2] - depth], [0.0, 0.0, 1.0])
                                } else {
                                    (origin, axis)
                                };
                            self.kernel.hole_tapped(spec, depth, origin, axis, span)
                        }
                        _ => unreachable!(),
                    }
                })();
                match result {
                    Ok(shape) => Val::Solid(shape),
                    Err(message) => {
                        let code = match callee {
                            "shell" => "E206_INVALID_SHELL_THICKNESS",
                            "hole::tapped" => "E209_INVALID_THREAD_SPEC",
                            _ => "E207_INVALID_FEATURE",
                        };
                        self.emit(code, Severity::Error, message, span);
                        Val::Solid(SolidShape::empty(callee))
                    }
                }
            }
            "mate::coaxial" | "mate::coincident" => {
                let member =
                    |key, index: usize| get(key).or_else(|| vals.get(index).map(|(_, v)| v));
                let result = check_placement_mate(
                    callee,
                    member("a", 0),
                    member("b", 1),
                    get("axis"),
                    get("offset"),
                );
                if let Err(message) = &result {
                    self.emit(
                        "E208_INVALID_ASSEMBLY_MATE",
                        Severity::Error,
                        *message,
                        span,
                    );
                }
                Val::Bool(result.is_ok())
            }
            "fillet" => {
                let r = get("r")
                    .or_else(pos0)
                    .and_then(|v| v.as_num())
                    .unwrap_or(0.0);
                match piped {
                    Some(Val::Solid(s)) => match self.kernel.fillet(&s, r, span) {
                        Ok(filleted) => Val::Solid(filleted),
                        Err(msg) => {
                            self.emit("E205_INVALID_FILLET_CHAMFER", Severity::Error, msg, span);
                            Val::Solid(s)
                        }
                    },
                    _ => {
                        self.emit(
                            "E205_INVALID_FILLET_CHAMFER",
                            Severity::Error,
                            "fillet requires an input Solid",
                            span,
                        );
                        Val::Solid(SolidShape::empty("invalid_fillet"))
                    }
                }
            }
            "chamfer" => {
                let d = get("d")
                    .or_else(pos0)
                    .and_then(|v| v.as_num())
                    .unwrap_or(0.0);
                match piped {
                    Some(Val::Solid(s)) => match self.kernel.chamfer(&s, d, span) {
                        Ok(chamfered) => Val::Solid(chamfered),
                        Err(msg) => {
                            self.emit("E205_INVALID_FILLET_CHAMFER", Severity::Error, msg, span);
                            Val::Solid(s)
                        }
                    },
                    _ => {
                        self.emit(
                            "E205_INVALID_FILLET_CHAMFER",
                            Severity::Error,
                            "chamfer requires an input Solid",
                            span,
                        );
                        Val::Solid(SolidShape::empty("invalid_chamfer"))
                    }
                }
            }
            "component" => {
                let origin = get("origin")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([0.0, 0.0, 0.0]);
                let rotation_deg = get("rotation")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([0.0, 0.0, 0.0]);
                // Check if `part` arg is a bare part name (`Var`) or call (`PartName(w = 30mm)`)
                let part_arg = args
                    .iter()
                    .find(|a| a.name.as_deref() == Some("part"))
                    .or_else(|| args.first());
                let (part_name, overrides) = match part_arg.map(|a| &a.value) {
                    Some(IrExpr::Var { name, .. }) => (name.clone(), HashMap::new()),
                    Some(IrExpr::Call {
                        callee,
                        args: pargs,
                        ..
                    }) => {
                        let mut ov = HashMap::new();
                        for pa in pargs {
                            if let Some(k) = &pa.name {
                                ov.insert(k.clone(), self.eval_expr(&pa.value));
                            }
                        }
                        (callee.clone(), ov)
                    }
                    _ => (String::new(), HashMap::new()),
                };
                if let Some(target_part) = self.parts_by_name.get(&part_name).copied() {
                    let mut sub_eval = Evaluator::new(self.kernel, self.module);
                    sub_eval.eval_constants();
                    let mut solid = sub_eval.eval_part(target_part, &overrides);
                    solid.part_name = part_name.clone();
                    self.diagnostics.extend(sub_eval.diagnostics);
                    Val::Component {
                        part_name,
                        solid,
                        origin,
                        rotation_deg,
                    }
                } else {
                    self.emit(
                        "E101_UNDEFINED_SYMBOL",
                        Severity::Error,
                        format!("unknown part `{part_name}` in `component(...)`"),
                        span,
                    );
                    Val::Unit
                }
            }
            "hole::simple" => {
                let d = get("d").and_then(|v| v.as_num()).unwrap_or(6.0);
                let depth = get("depth").and_then(|v| v.as_num()).unwrap_or(12.0);
                let origin = get("origin")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([0.0, 0.0, 0.0]);
                let axis = get("axis")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([0.0, 0.0, -1.0]);
                // Normalize downward hole origin so cylinder spans the cut
                let (cyl_origin, cyl_axis) = if axis[2] < -0.5 {
                    ([origin[0], origin[1], origin[2] - depth], [0.0, 0.0, 1.0])
                } else {
                    (origin, axis)
                };
                Val::Solid(
                    self.kernel
                        .hole_simple(d, depth, cyl_origin, cyl_axis, span),
                )
            }
            "hole::counterbore" => {
                let d_through = get("d_through").and_then(|v| v.as_num()).unwrap_or(6.6);
                let d_cbore = get("d_cbore").and_then(|v| v.as_num()).unwrap_or(11.0);
                let depth_cbore = get("depth_cbore").and_then(|v| v.as_num()).unwrap_or(6.2);
                let depth_through = get("depth_through")
                    .and_then(|v| v.as_num())
                    .unwrap_or(20.0);
                let origin =
                    get("origin")
                        .and_then(|v| v.as_vec3())
                        .unwrap_or([0.0, 0.0, depth_through]);
                Val::Solid(self.kernel.hole_counterbore(
                    d_through,
                    d_cbore,
                    depth_cbore,
                    depth_through,
                    origin,
                    span,
                ))
            }
            "array::linear" => {
                let feat = piped
                    .as_ref()
                    .or_else(|| get("feature"))
                    .and_then(|v| match v {
                        Val::Solid(s) => Some(s),
                        _ => None,
                    });
                let axis = get("axis")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([1.0, 0.0, 0.0]);
                let count = get("count")
                    .and_then(|v| v.as_num())
                    .unwrap_or(1.0)
                    .max(1.0) as usize;
                let spacing = get("spacing").and_then(|v| v.as_num()).unwrap_or(10.0);
                if let Some(s) = feat {
                    Val::Solid(self.kernel.array_linear(s, axis, count, spacing))
                } else {
                    Val::Solid(SolidShape::empty("invalid_linear_array"))
                }
            }
            "array::polar" => {
                let feat = piped
                    .as_ref()
                    .or_else(|| get("feature"))
                    .and_then(|v| match v {
                        Val::Solid(s) => Some(s),
                        _ => None,
                    });
                let axis = get("axis")
                    .and_then(|v| v.as_vec3())
                    .unwrap_or([0.0, 0.0, 1.0]);
                let count = get("count")
                    .and_then(|v| v.as_num())
                    .unwrap_or(1.0)
                    .max(1.0) as usize;
                let radius = get("radius").and_then(|v| v.as_num());
                let start_angle = get("start_angle").and_then(|v| v.as_num()).unwrap_or(0.0);
                if let Some(s) = feat {
                    Val::Solid(self.kernel.array_polar(s, axis, count, radius, start_angle))
                } else {
                    Val::Solid(SolidShape::empty("invalid_polar_array"))
                }
            }
            "boolean::union" | "implicit::smooth_union" => {
                let mut solids = Vec::new();
                if let Some(Val::Solid(s)) = piped {
                    solids.push(s);
                }
                if let Some(Val::List(items)) = get("items").or_else(pos0) {
                    for it in items {
                        if let Val::Solid(s) = it {
                            solids.push(s.clone());
                        }
                    }
                }
                if let Some(Val::Solid(s)) = pos0() {
                    if get("items").is_none() {
                        solids.push(s.clone());
                    }
                }
                Val::Solid(self.kernel.boolean_union(solids, "union_body"))
            }
            "boolean::subtract" => {
                let base = match piped.or_else(|| get("base").cloned()) {
                    Some(Val::Solid(s)) => s,
                    _ => SolidShape::empty("missing_base"),
                };
                let mut tools = Vec::new();
                if let Some(Val::List(list)) = get("tools") {
                    for t in list {
                        if let Val::Solid(s) = t {
                            tools.push(s.clone());
                        }
                    }
                }
                Val::Solid(self.kernel.boolean_subtract(base, tools))
            }
            "tag::faces" | "tag::cylindrical_faces" => {
                let tag = get("tag")
                    .or_else(pos0)
                    .and_then(|v| v.as_str())
                    .unwrap_or("tagged")
                    .to_string();
                let only_cyl = callee == "tag::cylindrical_faces";
                if let Some(Val::Solid(mut s)) = piped {
                    for f in &mut s.faces {
                        if !only_cyl || f.surface_type == SurfaceType::Cylinder {
                            if !f.provenance_tags.contains(&tag) {
                                f.provenance_tags.push(tag.clone());
                            }
                        }
                    }
                    for e in &mut s.edges {
                        if !e.provenance_tags.contains(&tag) {
                            e.provenance_tags.push(tag.clone());
                        }
                    }
                    Val::Solid(s)
                } else {
                    Val::Unit
                }
            }
            "pmi::datum" | "pmi::perpendicularity" | "pmi::flatness" => {
                let label = pos0()
                    .and_then(|v| v.as_str())
                    .or_else(|| get("datum").and_then(|v| v.as_str()))
                    .unwrap_or("PMI")
                    .to_string();
                let tol = get("flatness")
                    .or_else(|| get("tol"))
                    .and_then(|v| v.as_num());
                let datum = get("datum").and_then(|v| v.as_str()).map(str::to_string);
                let target_tags = match &piped {
                    Some(Val::FaceSet(fs)) => {
                        if callee == "pmi::datum" && fs.faces.len() > 1 && !fs.disambiguated {
                            self.emit(
                                "E201_AMBIGUOUS_TOPOLOGY",
                                Severity::Error,
                                "pmi::datum requires a disambiguated single face (use .max_by/.min_by)",
                                span,
                            );
                        }
                        fs.faces
                            .iter()
                            .flat_map(|f| f.provenance_tags.clone())
                            .collect()
                    }
                    _ => Vec::new(),
                };
                self.pmi_records.push(PmiRecord {
                    kind: callee.to_string(),
                    label,
                    tolerance_mm: tol,
                    datum,
                    target_tags,
                    span,
                });
                piped.unwrap_or(Val::Unit)
            }
            "assert::bbox" => {
                if let Some(Val::Solid(s)) = get("target") {
                    let bb = s.csg.bbox();
                    let sz = bb.size();
                    if let Some(max_sz) = get("max_size").and_then(|v| v.as_vec3()) {
                        if sz[0] > max_sz[0] + 1e-3
                            || sz[1] > max_sz[1] + 1e-3
                            || sz[2] > max_sz[2] + 1e-3
                        {
                            self.emit(
                                "E303_BBOX_ASSERTION_FAILED",
                                Severity::Error,
                                format!(
                                    "bounding box [{:.2}, {:.2}, {:.2}] mm exceeds max_size [{:.2}, {:.2}, {:.2}] mm",
                                    sz[0], sz[1], sz[2], max_sz[0], max_sz[1], max_sz[2]
                                ),
                                span,
                            );
                        }
                    }
                }
                Val::Bool(true)
            }
            "assert::dfm_accessible" => {
                if let Some(Val::Solid(s)) = get("target") {
                    let chk = self.cached_check_solid(s);
                    if !chk.watertight {
                        self.emit(
                            "E301_NON_WATERTIGHT",
                            Severity::Error,
                            "DFM accessibility check failed on non-watertight solid",
                            span,
                        );
                    }
                }
                Val::Bool(true)
            }
            "assert::physics" => {
                self.emit(
                    "I_PHASE2_PHYSICS_FALLBACK",
                    Severity::Info,
                    "Phase 2 `assert::physics` structural FEA check deferred; L0-L2 geometry gates verified",
                    span,
                );
                Val::Bool(true)
            }
            "field::stress_driven" => {
                let min_v = get("min").and_then(|v| v.as_num()).unwrap_or(2.0);
                Val::Number(min_v)
            }
            "force::distributed" => Val::Unit,
            _ => Val::Unit,
        }
    }

    fn eval_method(&mut self, recv: Val, method: &str, args: &[IrArg], span: Span) -> Val {
        let vals = self.eval_args(args);
        let get = |name: &str| -> Option<&Val> {
            vals.iter()
                .find(|(k, _)| k.as_deref() == Some(name))
                .map(|(_, v)| v)
        };
        let pos0 = || vals.first().map(|(_, v)| v);

        match (recv, method) {
            (Val::Solid(s), "faces") => Val::FaceSet(FaceSelection {
                faces: s.faces,
                disambiguated: false,
            }),
            (Val::FaceSet(mut fs), "planar") => {
                fs.faces.retain(|f| f.surface_type == SurfaceType::Plane);
                Val::FaceSet(fs)
            }
            (Val::FaceSet(mut fs), "cylindrical") => {
                fs.faces.retain(|f| f.surface_type == SurfaceType::Cylinder);
                Val::FaceSet(fs)
            }
            (Val::FaceSet(mut fs), "conical") => {
                fs.faces.retain(|f| f.surface_type == SurfaceType::Cone);
                Val::FaceSet(fs)
            }
            (Val::FaceSet(mut fs), "filter") => {
                if let Some(norm_dir) = get("normal").and_then(|v| v.as_vec3()) {
                    let tol_deg = get("angle_tol").and_then(|v| v.as_num()).unwrap_or(2.0);
                    let cos_tol = tol_deg.to_radians().cos();
                    fs.faces.retain(|f| {
                        let dot = f.normal_hint[0] * norm_dir[0]
                            + f.normal_hint[1] * norm_dir[1]
                            + f.normal_hint[2] * norm_dir[2];
                        dot >= cos_tol
                    });
                }
                if fs.faces.is_empty() {
                    self.emit(
                        "E202_EMPTY_SELECTOR",
                        Severity::Error,
                        "topology filter matched 0 faces",
                        span,
                    );
                }
                Val::FaceSet(fs)
            }
            (Val::FaceSet(mut fs), "tagged") => {
                let tag = pos0()
                    .and_then(|v| v.as_str())
                    .or_else(|| get("tag").and_then(|v| v.as_str()))
                    .unwrap_or("");
                fs.faces
                    .retain(|f| f.provenance_tags.iter().any(|t| t == tag));
                if fs.faces.is_empty() {
                    self.emit(
                        "E202_EMPTY_SELECTOR",
                        Severity::Error,
                        format!("topology selector `.tagged(\"{tag}\")` matched 0 faces"),
                        span,
                    );
                }
                fs.disambiguated = true;
                Val::FaceSet(fs)
            }
            (Val::FaceSet(mut fs), "max_by" | "min_by") => {
                let is_max = method == "max_by";
                if fs.faces.is_empty() {
                    self.emit(
                        "E202_EMPTY_SELECTOR",
                        Severity::Error,
                        format!("`.{method}(...)` called on empty face selection"),
                        span,
                    );
                    return Val::FaceSet(fs);
                }
                let key_fn = |f: &cad_kernel_occt::BRepFace| -> f64 {
                    if let Some(Val::Dir(d)) = get("coord") {
                        f.center_hint[0] * d[0] + f.center_hint[1] * d[1] + f.center_hint[2] * d[2]
                    } else if let Some(IrArg {
                        value: IrExpr::ConstDir { name, .. },
                        ..
                    }) = args.first()
                    {
                        match name.as_str() {
                            "X" => f.center_hint[0],
                            "Y" => f.center_hint[1],
                            "Z" => f.center_hint[2],
                            "radius" => f.radius_hint.unwrap_or(0.0),
                            _ => f.area_mm2,
                        }
                    } else {
                        f.center_hint[2]
                    }
                };
                let best_val = fs.faces.iter().map(&key_fn).fold(
                    if is_max {
                        f64::NEG_INFINITY
                    } else {
                        f64::INFINITY
                    },
                    |acc, v| {
                        if is_max { acc.max(v) } else { acc.min(v) }
                    },
                );
                fs.faces.retain(|f| (key_fn(f) - best_val).abs() < 1e-4);
                fs.disambiguated = true;
                Val::FaceSet(fs)
            }
            (other, _) => other,
        }
    }

    fn eval_sketch(&mut self, args: &[IrArg], stmts: &[IrSketchStmt], span: Span) -> Val {
        let arg_vals = self.eval_args(args);
        let plane = arg_vals
            .iter()
            .find(|(k, _)| k.as_deref() == Some("plane"))
            .map(|(_, v)| match v {
                Val::Plane(p) => *p,
                Val::FaceSet(fs) => {
                    if fs.faces.len() > 1 && !fs.disambiguated {
                        self.emit(
                            "E201_AMBIGUOUS_TOPOLOGY",
                            Severity::Error,
                            "sketch plane face selection is ambiguous",
                            span,
                        );
                    }
                    SketchPlane::Xy
                }
                _ => SketchPlane::Xy,
            })
            .unwrap_or(SketchPlane::Xy);

        let mut sys = SketchSystem::new();
        let mut local_points: HashMap<String, PointId> = HashMap::new();
        let mut local_edges: HashMap<String, EdgeId> = HashMap::new();
        let mut closed_loops: Vec<Vec<EdgeId>> = Vec::new();
        let mut path = None;

        for stmt in stmts {
            match stmt {
                IrSketchStmt::Let { name, expr, .. } => match expr {
                    IrExpr::Call {
                        callee,
                        args: cargs,
                        ..
                    } => {
                        let cvals = self.eval_args(cargs);
                        let get_c = |k: &str| -> Option<&Val> {
                            cvals
                                .iter()
                                .find(|(n, _)| n.as_deref() == Some(k))
                                .map(|(_, v)| v)
                        };
                        match callee.as_str() {
                            "point" => {
                                let (u, v) = match plane {
                                    SketchPlane::Xy => (
                                        get_c("x").and_then(|x| x.as_num()).unwrap_or(0.0),
                                        get_c("y").and_then(|x| x.as_num()).unwrap_or(0.0),
                                    ),
                                    SketchPlane::Xz => (
                                        get_c("x").and_then(|x| x.as_num()).unwrap_or(0.0),
                                        get_c("z").and_then(|x| x.as_num()).unwrap_or(0.0),
                                    ),
                                    SketchPlane::Yz => (
                                        get_c("y").and_then(|x| x.as_num()).unwrap_or(0.0),
                                        get_c("z").and_then(|x| x.as_num()).unwrap_or(0.0),
                                    ),
                                };
                                let pid = sys.add_point(u, v, true);
                                local_points.insert(name.clone(), pid);
                            }
                            "line" => {
                                let p1 = arg_var_name(cargs, "from", 0)
                                    .and_then(|n| local_points.get(n).copied());
                                let p2 = arg_var_name(cargs, "to", 1)
                                    .and_then(|n| local_points.get(n).copied());
                                if let (Some(a), Some(b)) = (p1, p2) {
                                    let eid = sys.add_line(a, b);
                                    local_edges.insert(name.clone(), eid);
                                }
                            }
                            "arc" => {
                                let p1 = arg_var_name(cargs, "from", 0)
                                    .and_then(|n| local_points.get(n).copied());
                                let p2 = arg_var_name(cargs, "to", 1)
                                    .and_then(|n| local_points.get(n).copied());
                                if let (Some(a), Some(b)) = (p1, p2) {
                                    let r_opt = get_c("radius").and_then(|v| v.as_num());
                                    let center_guess = r_opt.map(|r| {
                                        let pa = sys.points[a];
                                        let pb = sys.points[b];
                                        let mx = 0.5 * (pa.x + pb.x);
                                        let my = 0.5 * (pa.y + pb.y);
                                        let dx = pb.x - pa.x;
                                        let dy = pb.y - pa.y;
                                        let half = 0.5 * (dx * dx + dy * dy).sqrt().max(1e-6);
                                        let h = (r * r - half * half).max(0.0).sqrt();
                                        let nx = -dy / (2.0 * half);
                                        let ny = dx / (2.0 * half);
                                        (mx + nx * h, my + ny * h)
                                    });
                                    let eid = sys.add_arc(a, b, center_guess);
                                    if let Some(r) = r_opt {
                                        sys.add_constraint(Constraint::Radius {
                                            name: format!("{name}_r"),
                                            edge_id: eid,
                                            target: r,
                                        });
                                    }
                                    local_edges.insert(name.clone(), eid);
                                }
                            }
                            "circle" => {
                                let c = get_c("center")
                                    .and_then(|v| v.as_vec2())
                                    .unwrap_or([0.0, 0.0]);
                                let r = get_c("radius")
                                    .and_then(|v| v.as_num())
                                    .or_else(|| {
                                        get_c("diameter")
                                            .or_else(|| get_c("d"))
                                            .and_then(Val::as_num)
                                            .map(|d| 0.5 * d)
                                    })
                                    .unwrap_or(5.0);
                                let eid = sys.add_circle(c[0], c[1], r, true);
                                local_edges.insert(name.clone(), eid);
                            }
                            "rectangle" => {
                                let (ox, oy, w, h) = if let Some(c) =
                                    get_c("center").and_then(|v| v.as_vec2())
                                {
                                    let w = get_c("width").and_then(|v| v.as_num()).unwrap_or(10.0);
                                    let h =
                                        get_c("height").and_then(|v| v.as_num()).unwrap_or(10.0);
                                    (c[0] - 0.5 * w, c[1] - 0.5 * h, w, h)
                                } else {
                                    let o = get_c("origin")
                                        .and_then(|v| v.as_vec2())
                                        .unwrap_or([0.0, 0.0]);
                                    let sz = get_c("size")
                                        .and_then(|v| v.as_vec2())
                                        .unwrap_or([10.0, 10.0]);
                                    (o[0], o[1], sz[0], sz[1])
                                };
                                let eid = sys.add_rectangle(ox, oy, w, h, true);
                                local_edges.insert(name.clone(), eid);
                            }
                            _ => {}
                        }
                    }
                    _ => {
                        let v = self.eval_expr(expr);
                        self.env.insert(name.clone(), v);
                    }
                },
                IrSketchStmt::Expr {
                    expr,
                    span: st_span,
                } => {
                    if let IrExpr::Call {
                        callee,
                        args: cargs,
                        ..
                    } = expr
                    {
                        match callee.as_str() {
                            "region::closed" => {
                                if let Some(IrArg {
                                    value: IrExpr::Array { elements, .. },
                                    ..
                                }) = cargs.first()
                                {
                                    let mut ids = Vec::new();
                                    for el in elements {
                                        if let IrExpr::Var { name, .. } = el {
                                            if let Some(&eid) = local_edges.get(name) {
                                                ids.push(eid);
                                            }
                                        }
                                    }
                                    if !ids.is_empty() {
                                        closed_loops.push(ids);
                                    }
                                }
                            }
                            "region::path" => {
                                let ids = match cargs.first().map(|arg| &arg.value) {
                                    Some(IrExpr::Array { elements, .. }) => elements
                                        .iter()
                                        .map(|e| {
                                            if let IrExpr::Var { name, .. } = e {
                                                local_edges.get(name).copied()
                                            } else {
                                                None
                                            }
                                        })
                                        .collect::<Option<Vec<_>>>(),
                                    _ => None,
                                };
                                if let Some(ids) =
                                    ids.filter(|ids| !ids.is_empty()).filter(|_| path.is_none())
                                {
                                    path = Some(ids);
                                } else {
                                    self.emit(
                                        "E207_INVALID_FEATURE",
                                        Severity::Error,
                                        "region::path requires one nonempty list of sketch edges",
                                        *st_span,
                                    );
                                }
                            }
                            "constraint::horizontal" => {
                                if let Some(n) = arg_var_name(cargs, "edge", 0) {
                                    if let Some(&eid) = local_edges.get(n) {
                                        if let SketchGeom::Line { p1, p2, .. } = sys.geoms[eid] {
                                            sys.add_constraint(Constraint::Horizontal {
                                                name: format!("horiz_{n}"),
                                                p1,
                                                p2,
                                            });
                                        }
                                    }
                                }
                            }
                            "constraint::vertical" => {
                                if let Some(n) = arg_var_name(cargs, "edge", 0) {
                                    if let Some(&eid) = local_edges.get(n) {
                                        if let SketchGeom::Line { p1, p2, .. } = sys.geoms[eid] {
                                            sys.add_constraint(Constraint::Vertical {
                                                name: format!("vert_{n}"),
                                                p1,
                                                p2,
                                            });
                                        }
                                    }
                                }
                            }
                            "constraint::tangent" => {
                                let edge_name = arg_var_name(cargs, "edge", 0);
                                let target_name = arg_var_name(cargs, "target", 1);
                                let at_name = arg_var_name(cargs, "at", 2);
                                if let Some(eid) =
                                    edge_name.and_then(|n| local_edges.get(n).copied())
                                {
                                    let at_pid = at_name.and_then(|n| local_points.get(n).copied());
                                    let (line_dir, line_point) = if let Some(tn) = target_name {
                                        if let Some(&teid) = local_edges.get(tn) {
                                            if let SketchGeom::Line { p1, p2, .. } = sys.geoms[teid]
                                            {
                                                let a = sys.points[p1];
                                                let b = sys.points[p2];
                                                ([b.x - a.x, b.y - a.y], [a.x, a.y])
                                            } else {
                                                ([1.0, 0.0], [0.0, 0.0])
                                            }
                                        } else if let Some(Val::FaceSet(fs)) = self.env.get(tn) {
                                            if let Some(f) = fs.faces.first() {
                                                // Project face normal & center onto sketch 2D plane
                                                let n2 = match plane {
                                                    SketchPlane::Xy => {
                                                        [f.normal_hint[0], f.normal_hint[1]]
                                                    }
                                                    SketchPlane::Xz => {
                                                        [f.normal_hint[0], f.normal_hint[2]]
                                                    }
                                                    SketchPlane::Yz => {
                                                        [f.normal_hint[1], f.normal_hint[2]]
                                                    }
                                                };
                                                let c2 = match plane {
                                                    SketchPlane::Xy => {
                                                        [f.center_hint[0], f.center_hint[1]]
                                                    }
                                                    SketchPlane::Xz => {
                                                        [f.center_hint[0], f.center_hint[2]]
                                                    }
                                                    SketchPlane::Yz => {
                                                        [f.center_hint[1], f.center_hint[2]]
                                                    }
                                                };
                                                ([-n2[1], n2[0]], c2)
                                            } else {
                                                ([1.0, 0.0], [0.0, 0.0])
                                            }
                                        } else {
                                            ([1.0, 0.0], [0.0, 0.0])
                                        }
                                    } else {
                                        ([1.0, 0.0], [0.0, 0.0])
                                    };
                                    sys.add_constraint(Constraint::TangentLineArc {
                                        name: "tangent".to_string(),
                                        arc_id: eid,
                                        line_dir,
                                        line_point,
                                        at_point: at_pid,
                                    });
                                }
                            }
                            "constraint::distance" => {
                                let p1 = arg_var_name(cargs, "a", 0)
                                    .and_then(|n| local_points.get(n).copied());
                                let p2 = arg_var_name(cargs, "b", 1)
                                    .and_then(|n| local_points.get(n).copied());
                                let dist = cargs
                                    .iter()
                                    .find(|a| a.name.as_deref() == Some("value"))
                                    .or_else(|| cargs.get(2))
                                    .and_then(|a| self.eval_expr(&a.value).as_num());
                                if let (Some(a), Some(b), Some(d)) = (p1, p2, dist) {
                                    sys.add_constraint(Constraint::Distance {
                                        name: "distance".to_string(),
                                        p1: a,
                                        p2: b,
                                        target: d,
                                    });
                                }
                            }
                            "constraint::coincident" => {
                                // Point-point or edge-face coincident (already satisfied if coordinates match, checked by solver)
                                let a = arg_var_name(cargs, "a", 0);
                                let b = arg_var_name(cargs, "b", 1);
                                if let (Some(an), Some(bn)) = (a, b) {
                                    if let (Some(&pa), Some(&pb)) =
                                        (local_points.get(an), local_points.get(bn))
                                    {
                                        sys.add_constraint(Constraint::Coincident {
                                            name: format!("coincident_{an}_{bn}"),
                                            p1: pa,
                                            p2: pb,
                                        });
                                    }
                                }
                            }
                            _ => {
                                let _ = st_span;
                            }
                        }
                    }
                }
            }
        }

        let report = sys.solve(100, 1e-6);
        if report.status == SolveStatus::OverConstrained {
            self.emit(
                "E204_OVERCONSTRAINED",
                Severity::Error,
                format!(
                    "2D sketch constraints are over-constrained (rms={:.4})",
                    report.rms_residual
                ),
                span,
            );
        }

        if let Some(ids) = path {
            if !closed_loops.is_empty() {
                self.emit(
                    "E207_INVALID_FEATURE",
                    Severity::Error,
                    "a sketch cannot define both a closed region and a sweep path",
                    span,
                );
                return Val::Unit;
            }
            return match sample_sketch_path(&sys, &ids, plane) {
                Ok(points) => Val::SketchPath(points),
                Err(message) => {
                    self.emit("E207_INVALID_FEATURE", Severity::Error, message, span);
                    Val::Unit
                }
            };
        }

        if closed_loops.is_empty() {
            self.emit(
                "E203_UNCLOSED_SKETCH",
                Severity::Error,
                "sketch block missing `region::closed([...])`",
                span,
            );
            return Val::SketchProfile {
                plane,
                polygon_2d: Vec::new(),
            };
        }

        match sys.sample_closed_region(&closed_loops[0], 16) {
            Ok(poly) => Val::SketchProfile {
                plane,
                polygon_2d: poly,
            },
            Err(err) => {
                self.emit("E204_OPEN_SKETCH_REGION", Severity::Error, err, span);
                Val::SketchProfile {
                    plane,
                    polygon_2d: Vec::new(),
                }
            }
        }
    }
}

fn placement(origin: [f64; 3], rotation: [f64; 3]) -> Mat4 {
    Mat4::translate(origin[0], origin[1], origin[2]).mul_mat(Mat4::rotate_euler_deg(
        rotation[0],
        rotation[1],
        rotation[2],
    ))
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(x, y)| x * y).sum()
}

fn delta(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}

/// Verify explicit placements; mates do not reposition parts or solve a mechanism.
fn check_placement_mate(
    callee: &str,
    a: Option<&Val>,
    b: Option<&Val>,
    axis: Option<&Val>,
    offset: Option<&Val>,
) -> Result<(), &'static str> {
    let (
        Some(Val::Component {
            solid: sa,
            origin: oa,
            rotation_deg: ra,
            ..
        }),
        Some(Val::Component {
            solid: sb,
            origin: ob,
            rotation_deg: rb,
            ..
        }),
    ) = (a, b)
    else {
        return Err("mate requires two component instances");
    };
    let ma = placement(*oa, *ra);
    let mb = placement(*ob, *rb);
    const DIST_TOL: f64 = 1e-4;
    const ANG_TOL: f64 = 1e-8;
    if callee == "mate::coaxial" {
        let direction = match axis {
            Some(v) => v.as_vec3().ok_or("coaxial mate axis must be a direction")?,
            None => [0.0, 0.0, 1.0],
        };
        let norm = dot(direction, direction).sqrt();
        if !norm.is_finite() || norm < 1e-9 {
            return Err("coaxial mate axis must be nonzero");
        }
        let unit = direction.map(|x| x / norm);
        let da = ma.transform_dir(unit);
        let db = mb.transform_dir(unit);
        let d = delta(*ob, *oa);
        let axial = dot(d, da);
        let radial = delta(d, da.map(|x| x * axial));
        if dot(da, db).abs() >= 1.0 - ANG_TOL && dot(radial, radial) <= DIST_TOL.powi(2) {
            return Ok(());
        }
        return Err("component placement axes are not coaxial");
    }
    let offset = match offset {
        Some(v) => v
            .as_num()
            .ok_or("coincident mate offset must be a length")?,
        None => 0.0,
    };
    for fa in sa
        .faces
        .iter()
        .filter(|f| f.surface_type == SurfaceType::Plane)
    {
        let na = ma.transform_dir(fa.normal_hint);
        let ca = ma.transform_point(fa.center_hint);
        for fb in sb
            .faces
            .iter()
            .filter(|f| f.surface_type == SurfaceType::Plane)
        {
            let nb = mb.transform_dir(fb.normal_hint);
            if dot(na, nb) <= -1.0 + ANG_TOL
                && (dot(na, delta(mb.transform_point(fb.center_hint), ca)) - offset).abs()
                    <= DIST_TOL
            {
                return Ok(());
            }
        }
    }
    Err("no opposing planar face pair satisfies the coincident mate offset")
}

fn sample_sketch_path(
    sys: &SketchSystem,
    ids: &[EdgeId],
    plane: SketchPlane,
) -> Result<Vec<[f64; 3]>, String> {
    let mut points: Vec<[f64; 2]> = Vec::new();
    let xy = |i: PointId| [sys.points[i].x, sys.points[i].y];
    let distance = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).hypot(a[1] - b[1]);
    for (index, &id) in ids.iter().enumerate() {
        let (mut a, mut b, center) = match sys.geoms[id] {
            SketchGeom::Line { p1, p2, .. } => (xy(p1), xy(p2), None),
            SketchGeom::Arc { p1, p2, center, .. } => (xy(p1), xy(p2), Some(xy(center))),
            _ => return Err("region::path supports lines and arcs only".into()),
        };
        if distance(a, b) < 1e-6 {
            return Err(format!("degenerate path edge {index}"));
        }
        if let Some(&prev) = points.last() {
            if distance(prev, a) > 1e-3 {
                if distance(prev, b) <= 1e-3 {
                    std::mem::swap(&mut a, &mut b);
                } else {
                    return Err(format!("disconnected path edge {index}"));
                }
            }
        } else {
            points.push(a);
        }
        if let Some(c) = center {
            let r = distance(a, c);
            if r <= 1e-6 || (r - distance(b, c)).abs() > 1e-3 {
                return Err(format!("invalid radius on path arc {index}"));
            }
            let start = (a[1] - c[1]).atan2(a[0] - c[0]);
            let end = (b[1] - c[1]).atan2(b[0] - c[0]);
            let sweep = (end - start).sin().atan2((end - start).cos());
            let segments = ((sweep.abs() / std::f64::consts::PI) * 24.0).ceil() as usize;
            for i in 1..segments {
                let theta = start + sweep * i as f64 / segments as f64;
                points.push([c[0] + r * theta.cos(), c[1] + r * theta.sin()]);
            }
        }
        points.push(b);
    }
    Ok(points
        .into_iter()
        .map(|[u, v]| plane.to_3d(u, v, 0.0))
        .collect())
}

fn arg_var_name<'a>(args: &'a [IrArg], named: &str, pos: usize) -> Option<&'a str> {
    let found = args
        .iter()
        .find(|a| a.name.as_deref() == Some(named))
        .or_else(|| args.get(pos))?;
    match &found.value {
        IrExpr::Var { name, .. } => Some(name.as_str()),
        _ => None,
    }
}
