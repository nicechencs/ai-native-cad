use std::collections::{BTreeMap, HashMap};

use cad_ir::{
    DatumPlane, FeatureSpec, IrType, IrValue, NodeId, Quantity, SketchAnchor, SketchConstraint2D,
    SketchDimValue, SketchEntity2D, SketchPlaneSpec, SketchPointRef, SketchProfileLoop,
    StructuredSketch2D,
};

use super::{Inputs, Lowering};

pub(super) struct BuiltStructuredSketch {
    pub(super) sketch: StructuredSketch2D,
    pub(super) anchor_tangents: Vec<AnchorTangentSpec>,
}

#[derive(serde::Serialize)]
pub(super) struct AnchorTangentSpec {
    pub(super) arc_entity: String,
    pub(super) anchor_index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) face_query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) at_point: Option<String>,
}

impl Lowering {
    /// Best-effort conversion of lowered CAD-DSL sketch inputs into a strongly-typed
    /// V2 `StructuredSketch2D` so `GraphEvaluator` can solve and extrude/revolve/sweep/loft it.
    pub(super) fn try_build_structured_sketch(
        &self,
        inputs: &Inputs,
        statements: &[IrValue],
    ) -> Option<BuiltStructuredSketch> {
        let offset = match inputs.get("offset") {
            None => None,
            Some(value) => Some(self.as_sketch_dim(value, IrType::Length)?),
        };
        let (datum, plane) = match inputs.get("plane") {
            None => (
                DatumPlane::Xy,
                SketchPlaneSpec::Datum {
                    plane: DatumPlane::Xy,
                    offset,
                },
            ),
            Some(IrValue::String(name)) => {
                if let Some(d) = datum_plane_from_name(name) {
                    (d, SketchPlaneSpec::Datum { plane: d, offset })
                } else {
                    return None;
                }
            }
            Some(IrValue::Map(entries))
                if entries.get("dsl_topo_query") == Some(&IrValue::Bool(true)) =>
            {
                let IrValue::TopoRefExpr(face) = entries.get("expr")? else {
                    return None;
                };
                (
                    DatumPlane::Xy,
                    SketchPlaneSpec::FaceRef {
                        face: face.clone(),
                        offset,
                    },
                )
            }
            Some(_) => return None,
        };

        let mut entities: Vec<SketchEntity2D> = Vec::new();
        let mut profiles: Vec<SketchProfileLoop> = Vec::new();
        let mut constraints: Vec<SketchConstraint2D> = Vec::new();
        let mut anchor_tangents: Vec<AnchorTangentSpec> = Vec::new();
        let mut open_path_ids: Option<Vec<String>> = None;
        let mut local_ids: HashMap<i64, String> = HashMap::new();
        let mut local_points: HashMap<i64, [f64; 2]> = HashMap::new();

        for (index, stmt) in statements.iter().enumerate() {
            let IrValue::Map(entry) = stmt else {
                return None;
            };
            let entity_id = entry
                .get("name")
                .and_then(|v| match v {
                    IrValue::String(s) if !s.trim().is_empty() => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| format!("e{index}"));
            let IrValue::Map(call) = entry.get("value")? else {
                return None;
            };
            let IrValue::String(op) = call.get("operation")? else {
                return None;
            };
            let IrValue::Map(args) = call.get("arguments")? else {
                return None;
            };

            match op.as_str() {
                "point" => {
                    let (u, v) = match datum {
                        DatumPlane::Xy => (
                            self.arg_length_opt(args, &["x", "0"]).unwrap_or(0.0),
                            self.arg_length_opt(args, &["y", "1"]).unwrap_or(0.0),
                        ),
                        DatumPlane::Xz => (
                            self.arg_length_opt(args, &["x", "0"]).unwrap_or(0.0),
                            self.arg_length_opt(args, &["z", "y", "1"]).unwrap_or(0.0),
                        ),
                        DatumPlane::Yz => (
                            self.arg_length_opt(args, &["y", "0"]).unwrap_or(0.0),
                            self.arg_length_opt(args, &["z", "1"]).unwrap_or(0.0),
                        ),
                    };
                    if !u.is_finite() || !v.is_finite() {
                        return None;
                    }
                    entities.push(SketchEntity2D::Point {
                        id: entity_id.clone(),
                        position_mm: [u, v],
                        fixed: true,
                        construction: true,
                    });
                    local_ids.insert(index as i64, entity_id);
                    local_points.insert(index as i64, [u, v]);
                }
                "line" => {
                    let from_idx = arg_local_index(args, &["from", "0"])?;
                    let to_idx = arg_local_index(args, &["to", "1"])?;
                    let start_mm = *local_points.get(&from_idx)?;
                    let end_mm = *local_points.get(&to_idx)?;
                    let from_id = local_ids.get(&from_idx)?.clone();
                    let to_id = local_ids.get(&to_idx)?.clone();
                    entities.push(SketchEntity2D::Line {
                        id: entity_id.clone(),
                        start_mm,
                        end_mm,
                        construction: false,
                    });
                    constraints.push(SketchConstraint2D::Coincident {
                        name: None,
                        a: SketchPointRef::anchor(entity_id.clone(), SketchAnchor::Start),
                        b: SketchPointRef::point(from_id),
                    });
                    constraints.push(SketchConstraint2D::Coincident {
                        name: None,
                        a: SketchPointRef::anchor(entity_id.clone(), SketchAnchor::End),
                        b: SketchPointRef::point(to_id),
                    });
                    local_ids.insert(index as i64, entity_id);
                }
                "arc" => {
                    let from_idx = arg_local_index(args, &["from", "0"])?;
                    let to_idx = arg_local_index(args, &["to", "1"])?;
                    let start_mm = *local_points.get(&from_idx)?;
                    let end_mm = *local_points.get(&to_idx)?;
                    let from_id = local_ids.get(&from_idx)?.clone();
                    let to_id = local_ids.get(&to_idx)?.clone();
                    let radius_val = args.get("radius").or_else(|| args.get("r"));
                    let center_mm = if let Some(center) = args.get("center") {
                        self.as_vec2_length(center)?
                    } else if let Some(raw_r) = radius_val {
                        let r = self.as_length_mm(raw_r)?;
                        if !r.is_finite() || r <= 0.0 {
                            return None;
                        }
                        let mx = 0.5 * (start_mm[0] + end_mm[0]);
                        let my = 0.5 * (start_mm[1] + end_mm[1]);
                        let dx = end_mm[0] - start_mm[0];
                        let dy = end_mm[1] - start_mm[1];
                        let half = 0.5 * (dx * dx + dy * dy).sqrt().max(1e-6);
                        let h = (r * r - half * half).max(0.0).sqrt();
                        let nx = -dy / (2.0 * half);
                        let ny = dx / (2.0 * half);
                        [mx + nx * h, my + ny * h]
                    } else {
                        // Match cad-gcs default arc center guess (midpoint + perpendicular offset).
                        let mx = 0.5 * (start_mm[0] + end_mm[0]);
                        let my = 0.5 * (start_mm[1] + end_mm[1]);
                        let dx = end_mm[0] - start_mm[0];
                        let dy = end_mm[1] - start_mm[1];
                        [mx - 0.5 * dy, my + 0.5 * dx]
                    };
                    entities.push(SketchEntity2D::Arc {
                        id: entity_id.clone(),
                        center_mm,
                        start_mm,
                        end_mm,
                        ccw: true,
                        construction: false,
                    });
                    constraints.push(SketchConstraint2D::Coincident {
                        name: None,
                        a: SketchPointRef::anchor(entity_id.clone(), SketchAnchor::Start),
                        b: SketchPointRef::point(from_id),
                    });
                    constraints.push(SketchConstraint2D::Coincident {
                        name: None,
                        a: SketchPointRef::anchor(entity_id.clone(), SketchAnchor::End),
                        b: SketchPointRef::point(to_id),
                    });
                    if let Some(raw_r) = radius_val {
                        constraints.push(SketchConstraint2D::Radius {
                            name: None,
                            entity: entity_id.clone(),
                            value: self.as_sketch_dim(raw_r, IrType::Length)?,
                        });
                    }
                    local_ids.insert(index as i64, entity_id);
                }
                "circle" => {
                    let center_mm = match args.get("center").or_else(|| args.get("0")) {
                        Some(v) => self.as_vec2_length(v)?,
                        None => [0.0, 0.0],
                    };
                    let radius_mm = if let Some(r) = args.get("radius").or_else(|| args.get("r")) {
                        self.as_length_mm(r)?
                    } else if let Some(d) = args
                        .get("diameter")
                        .or_else(|| args.get("d"))
                        .or_else(|| args.get("1"))
                    {
                        0.5 * self.as_length_mm(d)?
                    } else {
                        return None;
                    };
                    if !radius_mm.is_finite() || radius_mm <= 0.0 {
                        return None;
                    }
                    entities.push(SketchEntity2D::Circle {
                        id: entity_id.clone(),
                        center_mm,
                        radius_mm,
                        construction: false,
                    });
                    local_ids.insert(index as i64, entity_id);
                }
                "ellipse" => {
                    let center_mm = match args.get("center").or_else(|| args.get("origin")) {
                        Some(v) => self.as_vec2_length(v)?,
                        None => [0.0, 0.0],
                    };
                    let rx_mm = self.arg_length_opt(args, &["rx", "major_radius", "a", "0"])?;
                    let ry_mm = self.arg_length_opt(args, &["ry", "minor_radius", "b", "1"])?;
                    if !rx_mm.is_finite() || !ry_mm.is_finite() || rx_mm <= 0.0 || ry_mm <= 0.0 {
                        return None;
                    }
                    let segments = args
                        .get("segments")
                        .and_then(|v| self.as_u32_opt(v));
                    entities.push(SketchEntity2D::Ellipse {
                        id: entity_id.clone(),
                        center_mm,
                        rx_mm,
                        ry_mm,
                        segments,
                        construction: false,
                    });
                    local_ids.insert(index as i64, entity_id);
                }
                "spline" => {
                    let closed = match args.get("closed").map(|v| self.nominal(v)) {
                        Some(IrValue::Bool(b)) => *b,
                        None => true,
                        _ => return None,
                    };
                    let pts_val = args
                        .get("points")
                        .or_else(|| args.get("control_points"))
                        .or_else(|| args.get("pts"))
                        .or_else(|| args.get("0"))?;
                    let IrValue::List(items) = self.nominal(pts_val) else {
                        return None;
                    };
                    let mut control_points_mm = Vec::with_capacity(items.len());
                    for item in items {
                        let pt = if let Some(local) = local_index(item) {
                            *local_points.get(&local)?
                        } else {
                            self.as_vec2_length(item)?
                        };
                        control_points_mm.push(pt);
                    }
                    let segments_per_span = args
                        .get("segments_per_span")
                        .or_else(|| args.get("segments"))
                        .and_then(|v| self.as_u32_opt(v));
                    entities.push(SketchEntity2D::Spline {
                        id: entity_id.clone(),
                        control_points_mm,
                        closed,
                        segments_per_span,
                        construction: false,
                    });
                    local_ids.insert(index as i64, entity_id);
                }
                "rectangle" => {
                    let (origin_mm, centered) = if let Some(center) = args.get("center") {
                        (self.as_vec2_length(center)?, true)
                    } else if let Some(origin) = args.get("origin") {
                        (self.as_vec2_length(origin)?, false)
                    } else {
                        ([0.0, 0.0], true)
                    };
                    let (width_mm, height_mm) = if let Some(size) = args.get("size") {
                        let [w, h] = self.as_vec2_length(size)?;
                        (w, h)
                    } else {
                        let w = self.arg_length_opt(args, &["width", "w"])?;
                        let h = self.arg_length_opt(args, &["height", "h"])?;
                        (w, h)
                    };
                    if !width_mm.is_finite()
                        || !height_mm.is_finite()
                        || width_mm <= 0.0
                        || height_mm <= 0.0
                    {
                        return None;
                    }
                    entities.push(SketchEntity2D::Rectangle {
                        id: entity_id.clone(),
                        origin_mm,
                        width_mm,
                        height_mm,
                        centered,
                        construction: false,
                    });
                    local_ids.insert(index as i64, entity_id);
                }
                "region::closed" => {
                    let list = match args.get("0").or_else(|| args.get("edges"))? {
                        IrValue::List(items) => items,
                        _ => return None,
                    };
                    let mut entity_ids = Vec::with_capacity(list.len());
                    for item in list {
                        let local = local_index(item)?;
                        entity_ids.push(local_ids.get(&local)?.clone());
                    }
                    if entity_ids.is_empty() {
                        return None;
                    }
                    profiles.push(SketchProfileLoop {
                        id: format!("loop_{}", profiles.len()),
                        entity_ids,
                        is_outer: true,
                    });
                }
                "region::path" => {
                    let list = match args.get("0").or_else(|| args.get("edges"))? {
                        IrValue::List(items) => items,
                        _ => return None,
                    };
                    let mut entity_ids = Vec::with_capacity(list.len());
                    for item in list {
                        let local = local_index(item)?;
                        entity_ids.push(local_ids.get(&local)?.clone());
                    }
                    if entity_ids.is_empty() || open_path_ids.is_some() {
                        return None;
                    }
                    open_path_ids = Some(entity_ids);
                }
                "constraint::horizontal" => {
                    let local = arg_local_index(args, &["0", "edge", "entity"])?;
                    constraints.push(SketchConstraint2D::Horizontal {
                        name: None,
                        entity: local_ids.get(&local)?.clone(),
                    });
                }
                "constraint::vertical" => {
                    let local = arg_local_index(args, &["0", "edge", "entity"])?;
                    constraints.push(SketchConstraint2D::Vertical {
                        name: None,
                        entity: local_ids.get(&local)?.clone(),
                    });
                }
                "constraint::parallel" => {
                    let a = arg_local_index(args, &["0", "a", "edge"])?;
                    let b = arg_local_index(args, &["1", "b", "target"])?;
                    constraints.push(SketchConstraint2D::Parallel {
                        name: None,
                        a: local_ids.get(&a)?.clone(),
                        b: local_ids.get(&b)?.clone(),
                    });
                }
                "constraint::perpendicular" => {
                    let a = arg_local_index(args, &["0", "a", "edge"])?;
                    let b = arg_local_index(args, &["1", "b", "target"])?;
                    constraints.push(SketchConstraint2D::Perpendicular {
                        name: None,
                        a: local_ids.get(&a)?.clone(),
                        b: local_ids.get(&b)?.clone(),
                    });
                }
                "constraint::tangent" => {
                    let a = arg_local_index(args, &["edge", "a", "0"])?;
                    let a_id = local_ids.get(&a)?.clone();
                    if let Some(b) = arg_local_index(args, &["target", "b", "1"]) {
                        constraints.push(SketchConstraint2D::Tangent {
                            name: None,
                            a: a_id,
                            b: local_ids.get(&b)?.clone(),
                        });
                    } else if let Some((anchor_index, face_query)) =
                        arg_anchor_ref(args, &["target", "b", "1"])
                    {
                        let at_point = arg_local_index(args, &["at", "2"])
                            .and_then(|idx| local_ids.get(&idx).cloned());
                        anchor_tangents.push(AnchorTangentSpec {
                            arc_entity: a_id,
                            anchor_index,
                            face_query,
                            at_point,
                        });
                    } else {
                        return None;
                    }
                }
                "constraint::coincident" => {
                    let a_local = arg_local_index(args, &["0", "a", "edge"]);
                    let b_local = arg_local_index(args, &["1", "b", "target"]);
                    if let (Some(a), Some(b)) = (a_local, b_local) {
                        if local_points.contains_key(&a) && local_points.contains_key(&b) {
                            constraints.push(SketchConstraint2D::Coincident {
                                name: None,
                                a: SketchPointRef::point(local_ids.get(&a)?.clone()),
                                b: SketchPointRef::point(local_ids.get(&b)?.clone()),
                            });
                        }
                    } else if arg_anchor_ref(args, &["1", "b", "target"]).is_some()
                        || arg_anchor_ref(args, &["0", "a", "edge"]).is_some()
                    {
                        // Edge-to-external-face coincidence is already enforced by endpoint coordinates
                        // (matching legacy `eval_sketch.rs`).
                    } else {
                        return None;
                    }
                }
                "constraint::equal" => {
                    let a = arg_local_index(args, &["0", "a", "edge"])?;
                    let b = arg_local_index(args, &["1", "b", "target"])?;
                    constraints.push(SketchConstraint2D::Equal {
                        name: None,
                        a: local_ids.get(&a)?.clone(),
                        b: local_ids.get(&b)?.clone(),
                    });
                }
                "constraint::distance" => {
                    let a = arg_local_index(args, &["0", "a", "from"])?;
                    let b = arg_local_index(args, &["1", "b", "to"])?;
                    let val = args
                        .get("value")
                        .or_else(|| args.get("distance"))
                        .or_else(|| args.get("2"))?;
                    constraints.push(SketchConstraint2D::Distance {
                        name: None,
                        a: SketchPointRef::point(local_ids.get(&a)?.clone()),
                        b: SketchPointRef::point(local_ids.get(&b)?.clone()),
                        value: self.as_sketch_dim(val, IrType::Length)?,
                    });
                }
                "constraint::radius" => {
                    let entity = arg_local_index(args, &["0", "edge", "entity"])?;
                    let val = args
                        .get("value")
                        .or_else(|| args.get("radius"))
                        .or_else(|| args.get("1"))?;
                    constraints.push(SketchConstraint2D::Radius {
                        name: None,
                        entity: local_ids.get(&entity)?.clone(),
                        value: self.as_sketch_dim(val, IrType::Length)?,
                    });
                }
                "constraint::fixed" => {
                    // Points created via `point(...)` are already fixed.
                    let local = arg_local_index(args, &["0", "point", "entity"])?;
                    if !local_points.contains_key(&local) {
                        return None;
                    }
                }
                _ => return None,
            }
        }

        if entities.is_empty() {
            return None;
        }
        if let Some(path_ids) = open_path_ids {
            if !profiles.is_empty() {
                return None;
            }
            // Place path edges in explicit `region::path` order as non-construction entities
            // and mark any other line/arc/circle/rect entities as construction so
            // `graph_sketch_form::extract_profiles` yields the exact requested open path.
            let mut by_id: BTreeMap<String, SketchEntity2D> = entities
                .into_iter()
                .map(|entity| (entity.id().to_owned(), entity))
                .collect();
            let mut reordered = Vec::with_capacity(by_id.len());
            for path_id in &path_ids {
                if let Some(entity) = by_id.remove(path_id) {
                    reordered.push(entity);
                }
            }
            for mut entity in by_id.into_values() {
                set_construction(&mut entity, true);
                reordered.push(entity);
            }
            entities = reordered;
        }
        let sketch = StructuredSketch2D {
            plane,
            entities,
            profiles,
            constraints,
            remaining_dof: None,
        };
        FeatureSpec::Sketch2D(sketch.clone())
            .validate(&NodeId("sketch".into()))
            .ok()?;
        Some(BuiltStructuredSketch {
            sketch,
            anchor_tangents,
        })
    }

    fn as_u32_opt(&self, value: &IrValue) -> Option<u32> {
        match self.nominal(value) {
            IrValue::Int(v) if *v > 0 => u32::try_from(*v).ok(),
            IrValue::Scalar(x) if x.is_finite() && *x > 0.0 => Some(x.round() as u32),
            _ => None,
        }
    }

    fn as_length_mm(&self, value: &IrValue) -> Option<f64> {
        match self.nominal(value) {
            IrValue::Quantity(q) if q.ir_type() == IrType::Length => Some(q.canonical_value()),
            IrValue::Scalar(x) => Some(*x),
            IrValue::Int(x) => Some(*x as f64),
            _ => None,
        }
    }

    fn arg_length_opt(&self, args: &Inputs, keys: &[&str]) -> Option<f64> {
        keys.iter()
            .find_map(|key| args.get(*key))
            .and_then(|value| self.as_length_mm(value))
    }

    fn as_vec2_length(&self, value: &IrValue) -> Option<[f64; 2]> {
        let IrValue::List(items) = self.nominal(value) else {
            return None;
        };
        if items.len() != 2 {
            return None;
        }
        Some([self.as_length_mm(&items[0])?, self.as_length_mm(&items[1])?])
    }

    fn as_sketch_dim(&self, value: &IrValue, expected: IrType) -> Option<SketchDimValue> {
        if let Some(param) = self.parameter(value) {
            if param.ir_type == expected {
                return Some(SketchDimValue::param(param.name.clone()));
            }
            return None;
        }
        match value {
            IrValue::Quantity(q) if q.ir_type() == expected => Some(SketchDimValue::Literal(*q)),
            IrValue::Scalar(x) if expected == IrType::Length => {
                Some(SketchDimValue::Literal(Quantity::length(*x, cad_ir::LengthUnit::Mm).ok()?))
            }
            _ => None,
        }
    }
}

pub(super) fn datum_plane_from_name(name: &str) -> Option<DatumPlane> {
    match name {
        "XY_PLANE" | "XY" => Some(DatumPlane::Xy),
        "XZ_PLANE" | "XZ" => Some(DatumPlane::Xz),
        "YZ_PLANE" | "YZ" => Some(DatumPlane::Yz),
        _ => None,
    }
}

fn local_index(value: &IrValue) -> Option<i64> {
    let IrValue::Map(map) = value else {
        return None;
    };
    match map.get("local") {
        Some(IrValue::Int(idx)) => Some(*idx),
        _ => None,
    }
}

fn arg_local_index(args: &Inputs, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| args.get(*key))
        .and_then(local_index)
}

fn anchor_index_from_str(value: &IrValue) -> Option<usize> {
    let IrValue::String(text) = value else {
        return None;
    };
    text.strip_prefix("@anchor:")?.parse().ok()
}

fn anchor_ref(value: &IrValue) -> Option<(usize, Option<String>)> {
    if let Some(index) = anchor_index_from_str(value) {
        return Some((index, None));
    }
    let IrValue::Map(entries) = value else {
        return None;
    };
    if entries.get("dsl_topo_query") != Some(&IrValue::Bool(true)) {
        return None;
    }
    let index = anchor_index_from_str(entries.get("root")?)?;
    let face_query = match entries.get("expr") {
        Some(IrValue::TopoRefExpr(expr)) => Some(expr.clone()),
        _ => None,
    };
    Some((index, face_query))
}

fn arg_anchor_ref(args: &Inputs, keys: &[&str]) -> Option<(usize, Option<String>)> {
    keys.iter()
        .find_map(|key| args.get(*key))
        .and_then(anchor_ref)
}

fn set_construction(entity: &mut SketchEntity2D, value: bool) {
    match entity {
        SketchEntity2D::Point { construction, .. }
        | SketchEntity2D::Line { construction, .. }
        | SketchEntity2D::Arc { construction, .. }
        | SketchEntity2D::Circle { construction, .. }
        | SketchEntity2D::Rectangle { construction, .. }
        | SketchEntity2D::SplineBlob { construction, .. }
        | SketchEntity2D::Ellipse { construction, .. }
        | SketchEntity2D::Spline { construction, .. } => *construction = value,
    }
}
