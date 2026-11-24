use std::collections::{BTreeMap, HashMap};

use cad_hir::{DimType, IrArg, IrExpr, IrSketchStmt};
use cad_ir::{
    AngleUnit, BooleanOpKind, ComponentSourceSpec, DatumPlane, DesignAssertionKind, DimInput,
    DirectFaceEditOp, FEATURE_SPEC_PORT, FeatureBodyOp, FeatureSpec, IntInput, IrType, IrValue,
    LengthUnit, NodeId, NodeKind, Quantity, SketchDimValue, SketchEntity2D, SketchPlaneSpec,
    SketchProfileLoop, StructuredSketch2D, SweepPathSpec,
};
use cad_syntax::Span;
use cad_topo::{
    AxisDir, CoordAxis, EntityKind, QueryTopoRef, SurfaceClass, TieBreaker, TopoFilter, TopoRef,
};

use super::{Inputs, Lowering, Result, at, sketch_spec::datum_plane_from_name};

impl Lowering {
    pub(super) fn call(
        &mut self,
        callee: &str,
        args: &[IrArg],
        piped: Option<IrValue>,
        name: Option<&str>,
        span: Span,
    ) -> Result<IrValue> {
        use NodeKind::*;
        if piped.is_none() && self.parts.contains_key(callee) {
            return self.call_part(callee, args, span);
        }
        let (kind, output, receiver, positional): (_, _, _, &[&str]) = match callee {
            "box" => (PrimitiveBox, IrType::Solid, None, &["size", "origin"]),
            "cylinder" => (
                PrimitiveCylinder,
                IrType::Solid,
                None,
                &["diameter", "height", "origin", "axis"],
            ),
            "boolean::union" => (
                BooleanUnion,
                IrType::Solid,
                Some("base"),
                &["items", "tool"],
            ),
            "boolean::subtract" => (
                BooleanSubtract,
                IrType::Solid,
                Some("base"),
                &["base", "tools"],
            ),
            "fillet" => (
                Fillet,
                IrType::Solid,
                Some("solid"),
                &["solid", "radius", "edges"],
            ),
            "chamfer" => (
                Chamfer,
                IrType::Solid,
                Some("solid"),
                &["solid", "distance", "edges"],
            ),
            "shell" => (
                Shell,
                IrType::Solid,
                Some("solid"),
                &["solid", "thickness", "open_faces"],
            ),
            "extrude" | "extrude_symmetric" => (
                Extrude,
                IrType::Solid,
                Some("profile"),
                &["profile", "height"],
            ),
            "revolve" => (
                Revolve,
                IrType::Solid,
                Some("profile"),
                &["profile", "axis", "angle"],
            ),
            "hole::simple" => (
                Hole,
                IrType::Solid,
                Some("solid"),
                &["diameter", "depth", "origin", "axis"],
            ),
            "hole::counterbore" => (
                Hole,
                IrType::Solid,
                Some("solid"),
                &["diameter", "d_cbore", "depth_cbore", "depth", "origin"],
            ),
            "hole::tapped" => (
                Hole,
                IrType::Solid,
                Some("solid"),
                &["thread", "depth", "origin", "axis"],
            ),
            "array::linear" => (
                PatternLinear,
                IrType::Solid,
                Some("solid"),
                &["solid", "direction", "count", "spacing"],
            ),
            "array::polar" => (
                PatternPolar,
                IrType::Solid,
                Some("solid"),
                &["solid", "axis", "count", "radius", "start_angle"],
            ),
            "mirror" => (
                Mirror,
                IrType::Solid,
                Some("solid"),
                &["solid", "plane", "merge"],
            ),
            "sweep" => (Sweep, IrType::Solid, Some("profile"), &["profile", "path"]),
            "loft" => (LoftGuided, IrType::Solid, None, &["sections", "height"]),
            "implicit::smooth_union" => (
                ImplicitSmoothBlend,
                IrType::Solid,
                None,
                &["items", "blend_radius"],
            ),
            "tag::faces" | "tag::cylindrical_faces" => {
                if let Some(ref value) = piped
                    && let Some((root, query)) = decode_topo_query(value)
                {
                    return self.tag_face_query(callee, args, root, query, name, span);
                }
                (TagFaces, IrType::Solid, Some("geometry"), &["tag"])
            }
            "face::offset" | "face::move" | "face::delete" => {
                return self.direct_face_edit(callee, args, piped, name, span);
            }
            "profile::circle" | "profile::ellipse" | "profile::spline" => {
                return self.profile_sketch(callee, args, name, span);
            }
            "path::helix" | "path::spline3d" | "path::polyline" => {
                return self.path_3d(callee, args, span);
            }
            "transform" | "translate" | "rotate" => {
                return self.transform(callee, args, piped, name, span);
            }
            "import::step" | "import::mesh" => {
                return self.import_asset(callee, args, name, span);
            }
            "component" => return self.component(args, piped, name, span),
            _ if callee.starts_with("mate::") => {
                return self.mate(callee, args, name, span);
            }
            "field::stress_driven" => {
                return self.extension(callee, IrType::Length, args, piped, name);
            }
            "force::distributed" => {
                return self.extension(callee, IrType::Force, args, piped, name);
            }
            "faces" | "edges" | "planar" | "cylindrical" | "conical" | "freeform" | "filter"
            | "tagged" | "max_by" | "min_by" | "nearest_to" | "then_by" => {
                if let Some(query) = self.try_fold_selector(callee, args, piped.as_ref(), span)? {
                    return Ok(query);
                }
                return self.extension(callee, IrType::TopoRef, args, piped, name);
            }
            "pmi::datum" | "pmi::perpendicularity" | "pmi::flatness" | "pmi::tolerance"
            | "pmi::thread" => {
                return self.extension(callee, IrType::TopoRef, args, piped, name);
            }
            "assert::bbox"
            | "assert::dfm_accessible"
            | "assert::no_interference"
            | "assert::physics" => {
                return self.assert_call(callee, args, name, span);
            }
            _ => {
                return Err(at(
                    span,
                    format!("unsupported CAD-DSL operation `{callee}`"),
                ));
            }
        };
        let mut inputs = Inputs::new();
        let mut arg_spans = BTreeMap::new();
        let mut positions = positional.iter().copied();
        if let Some(value) = piped {
            let port = receiver
                .ok_or_else(|| at(span, format!("`{callee}` does not accept a pipe input")))?;
            inputs.insert(port.to_owned(), value);
        }
        for arg in args {
            let port = match arg.name.as_deref() {
                Some(port) => port,
                None => positions
                    .find(|port| !inputs.contains_key(*port))
                    .ok_or_else(|| {
                        at(
                            arg.span,
                            format!("unexpected positional argument to `{callee}`"),
                        )
                    })?,
            };
            let port = canonical_port(&kind, port);
            let value = self.value(&arg.value, None)?;
            put(&mut inputs, port, value, arg.span)?;
            arg_spans.insert(port, arg.value.span());
        }
        if kind == PrimitiveBox
            && let Some(size) = inputs.remove("size")
        {
            let IrValue::List(size) = size else {
                return Err(at(
                    arg_spans["size"],
                    "box size requires a list of three lengths",
                ));
            };
            let [width, depth, height]: [IrValue; 3] = size
                .try_into()
                .map_err(|_| at(arg_spans["size"], "box size requires three lengths"))?;
            for (port, value) in [("width", width), ("depth", depth), ("height", height)] {
                put(&mut inputs, port, value, arg_spans["size"])?;
                arg_spans.insert(port, arg_spans["size"]);
            }
        }
        if kind == BooleanUnion
            && let Some(value) = inputs.remove("items")
        {
            let solids = match value {
                IrValue::List(items) => items,
                item => vec![item],
            };
            put(&mut inputs, "solids", IrValue::List(solids), span)?;
        }
        if kind == BooleanSubtract
            && let Some(tool) = inputs.remove("tools")
        {
            inputs.insert(
                "tools".to_owned(),
                match tool {
                    IrValue::List(_) => tool,
                    _ => IrValue::List(vec![tool]),
                },
            );
        }
        if kind == ImplicitSmoothBlend
            && let Some(value) = inputs.remove("solids")
        {
            let solids = match value {
                IrValue::List(items) => items,
                item => vec![item],
            };
            put(&mut inputs, "solids", IrValue::List(solids), span)?;
        }
        if kind == LoftGuided
            && let Some(value) = inputs.remove("sections")
        {
            let sections = match value {
                IrValue::List(items) => items,
                item => vec![item],
            };
            if sections.is_empty() {
                return Err(at(span, "`loft` requires one or more sections"));
            }
            put(&mut inputs, "sections", IrValue::List(sections), span)?;
        }
        if kind == Extrude && callee == "extrude_symmetric" {
            put(&mut inputs, "symmetric", IrValue::Bool(true), span)?;
        }
        if kind == Hole {
            put(
                &mut inputs,
                "hole_type",
                IrValue::String(callee.trim_start_matches("hole::").to_owned()),
                span,
            )?;
        }
        if callee == "tag::cylindrical_faces" {
            let selector = face_query(vec![TopoFilter::Surface(SurfaceClass::Cylinder)], span)?;
            put(&mut inputs, "faces", IrValue::List(vec![selector]), span)?;
        }
        if kind == PatternPolar && !inputs.contains_key("angle") {
            inputs.insert(
                "angle".to_owned(),
                IrValue::Quantity(Quantity::Angle {
                    value: 360.0,
                    unit: AngleUnit::Deg,
                }),
            );
        }
        insert_primitive_defaults(&kind, &mut inputs);
        let mut metadata = BTreeMap::new();
        if matches!(kind, PrimitiveBox | PrimitiveCylinder | Hole) && span.end_byte > span.start_byte {
            metadata.insert(
                "dsl.ast_span".to_owned(),
                format!(
                    "{}:{}:{}:{}:{}:{}",
                    span.start_line,
                    span.start_col,
                    span.end_line,
                    span.end_col,
                    span.start_byte,
                    span.end_byte
                ),
            );
        }
        let mut raw_normalized = Inputs::new();
        let mut normalized_inputs = Inputs::new();
        for (port, raw) in inputs {
            let arg_span = arg_spans.get(port.as_str()).copied().unwrap_or(span);
            let normalized = match (port.as_str(), &kind) {
                ("origin", PrimitiveBox | PrimitiveCylinder | Hole) => {
                    self.vector(raw, IrType::Vec3Length, arg_span)?
                }
                ("axis", PrimitiveCylinder | Hole | Revolve | PatternPolar)
                | ("direction", Extrude | PatternLinear) => {
                    self.vector(raw, IrType::Vec3Dir, arg_span)?
                }
                ("count", PatternLinear | PatternPolar) => self.count(raw, arg_span)?,
                ("path", Sweep) => self.normalize_path(raw, arg_span)?,
                ("edges", Fillet | Chamfer) | ("open_faces", Shell) | ("faces", TagFaces) => {
                    topology_list(raw, &port, arg_span)?
                }
                _ => raw,
            };
            raw_normalized.insert(port.clone(), normalized.clone());
            let value = if connected_port(&kind, &port) {
                normalized
            } else {
                self.literal_option(&port, normalized, &mut metadata, arg_span)?
            };
            normalized_inputs.insert(port, value);
        }
        if kind == Hole && !normalized_inputs.contains_key("solid") {
            if let Some(tool) =
                self.try_standalone_hole_tool(callee, name, &normalized_inputs, &metadata, span)?
            {
                return Ok(tool);
            }
        }
        if kind == TagFaces && !normalized_inputs.contains_key("faces") {
            self.insert_all_face_query(&mut normalized_inputs)?;
        }
        self.project_topo_ports(&kind, &mut normalized_inputs);
        self.attach_feature_spec(&kind, &raw_normalized, &mut normalized_inputs);
        Ok(self.add_node(name, kind, output, normalized_inputs, metadata))
    }

    fn transform(
        &mut self,
        callee: &str,
        args: &[IrArg],
        piped: Option<IrValue>,
        name: Option<&str>,
        span: Span,
    ) -> Result<IrValue> {
        let slots: &[&str] = match callee {
            "translate" => &["solid", "offset"],
            "rotate" => &["solid", "axis", "angle"],
            _ => &["solid", "matrix"],
        };
        let mut positions = slots.iter().copied();
        let mut inputs = Inputs::new();
        if let Some(value) = piped {
            inputs.insert("solid".to_owned(), value);
        }
        for arg in args {
            let port = match arg.name.as_deref() {
                Some("body" | "target" | "base") => "solid",
                Some(port) => port,
                None => positions
                    .find(|port| !inputs.contains_key(*port))
                    .ok_or_else(|| at(arg.span, "unexpected positional transform argument"))?,
            };
            let value = self.value(&arg.value, None)?;
            put(&mut inputs, port, value, arg.span)?;
        }
        let Some(IrValue::NodeRef { node_id, .. }) = inputs.get("solid") else {
            return Err(at(span, format!("`{callee}` requires an input Solid")));
        };
        if self.graph.nodes[&node_id.0].output_type != IrType::Solid {
            return Err(at(span, format!("`{callee}` requires an input Solid")));
        }
        put(
            &mut inputs,
            "function",
            IrValue::String(callee.to_owned()),
            span,
        )?;
        Ok(self.add_node(
            name,
            NodeKind::StdLibCall,
            IrType::Solid,
            inputs,
            BTreeMap::new(),
        ))
    }

    fn import_asset(
        &mut self,
        callee: &str,
        args: &[IrArg],
        name: Option<&str>,
        span: Span,
    ) -> Result<IrValue> {
        let format = callee
            .rsplit_once("::")
            .map(|(_, format)| format)
            .unwrap_or(callee);
        let mut inputs = self.ordered_args(args, |lower, value| lower.value(value, None))?;
        if let Some(path) = inputs.remove("0") {
            put(&mut inputs, "path", path, span)?;
        }
        if !inputs.contains_key("path") {
            return Err(at(span, format!("`{callee}` requires a path")));
        }
        put(
            &mut inputs,
            "format",
            IrValue::String(format.to_owned()),
            span,
        )?;
        let mut metadata = BTreeMap::new();
        if span.end_byte > span.start_byte {
            metadata.insert(
                "dsl.ast_span".to_owned(),
                format!(
                    "{}:{}:{}:{}:{}:{}",
                    span.start_line,
                    span.start_col,
                    span.end_line,
                    span.end_col,
                    span.start_byte,
                    span.end_byte
                ),
            );
        }
        let mut normalized = Inputs::new();
        for (port, raw) in inputs {
            normalized.insert(
                port.clone(),
                self.literal_option(&port, raw, &mut metadata, span)?,
            );
        }
        // Path-only imports stay on the legacy ports. Graph evaluation needs a
        // blob payload, so the readiness gap is kept even after this spec.
        if format == "step"
            && let Some(IrValue::String(path)) = normalized.get("path").cloned()
            && !path.trim().is_empty()
        {
            let scale = match normalized.get("scale") {
                Some(IrValue::Scalar(value)) if value.is_finite() && *value > 0.0 => *value,
                Some(IrValue::Int(value)) if *value > 0 => *value as f64,
                _ => 1.0,
            };
            let as_reference_only = matches!(
                normalized.get("as_reference_only"),
                Some(IrValue::Bool(true))
            );
            let spec = FeatureSpec::ImportStep {
                blob: None,
                path: Some(path),
                scale,
                as_reference_only,
            };
            if spec.validate(&NodeId("spec".into())).is_ok()
                && let Ok(spec_json) = serde_json::to_string(&spec)
            {
                normalized.insert(FEATURE_SPEC_PORT.to_owned(), IrValue::String(spec_json));
            }
        }
        Ok(self.add_node(
            name,
            NodeKind::ImportAsset,
            IrType::Solid,
            normalized,
            metadata,
        ))
    }

    fn component(
        &mut self,
        args: &[IrArg],
        piped: Option<IrValue>,
        name: Option<&str>,
        span: Span,
    ) -> Result<IrValue> {
        let mut inputs = Inputs::new();
        let mut positions = ["geometry", "origin", "rotation"].into_iter();
        if let Some(value) = piped {
            inputs.insert("geometry".to_owned(), value);
        }
        for arg in args {
            let port = match arg.name.as_deref() {
                Some("part" | "solid" | "body" | "geometry") => "geometry",
                Some(port) => port,
                None => positions
                    .find(|port| !inputs.contains_key(*port))
                    .ok_or_else(|| at(arg.span, "unexpected positional component argument"))?,
            };
            let raw = self.value(&arg.value, None)?;
            let value = match port {
                "origin" => self.vector(raw, IrType::Vec3Length, arg.span)?,
                "rotation" => self.angle_vector(raw, arg.span)?,
                _ => raw,
            };
            put(&mut inputs, port, value, arg.span)?;
        }
        let Some(geometry) = inputs.get("geometry").cloned() else {
            return Err(at(span, "`component` requires a part"));
        };
        self.require_output(
            &geometry,
            &[
                IrType::Solid,
                IrType::Surface,
                IrType::SubD,
                IrType::Assembly,
            ],
            span,
            "`component` requires a part geometry node",
        )?;
        let part_name = if let IrValue::NodeRef { node_id, .. } = &geometry {
            let geom_node = &self.graph.nodes[&node_id.0];
            geom_node
                .metadata
                .get("dsl.part_name")
                .cloned()
                .unwrap_or_else(|| geom_node.name.clone())
        } else {
            String::new()
        };
        if let Some(spec) = self.component_feature_spec(&inputs)
            && spec.validate(&NodeId("spec".into())).is_ok()
            && let Ok(spec_json) = serde_json::to_string(&spec)
        {
            inputs.insert(FEATURE_SPEC_PORT.to_owned(), IrValue::String(spec_json));
        }
        let mut metadata = BTreeMap::new();
        if !part_name.is_empty() {
            metadata.insert("dsl.part_name".to_owned(), part_name);
        }
        let mut normalized = Inputs::new();
        for (port, raw) in inputs {
            let value = if matches!(port.as_str(), "geometry" | "external_ref") {
                raw
            } else {
                self.literal_option(&port, raw, &mut metadata, span)?
            };
            normalized.insert(port, value);
        }
        Ok(self.add_node(
            name,
            NodeKind::AssemblyComponent,
            IrType::Component,
            normalized,
            metadata,
        ))
    }

    fn mate(
        &mut self,
        callee: &str,
        args: &[IrArg],
        name: Option<&str>,
        span: Span,
    ) -> Result<IrValue> {
        let mate = callee.trim_start_matches("mate::");
        let mut inputs = Inputs::new();
        let mut positions = ["left", "right"].into_iter();
        for arg in args {
            let port = match arg.name.as_deref() {
                Some("a" | "left") => "left",
                Some("b" | "right") => "right",
                Some(port) => port,
                None => positions
                    .find(|port| !inputs.contains_key(*port))
                    .ok_or_else(|| at(arg.span, "unexpected positional mate argument"))?,
            };
            put(&mut inputs, port, self.value(&arg.value, None)?, arg.span)?;
        }
        for port in ["left", "right"] {
            let Some(value) = inputs.get(port).cloned() else {
                return Err(at(span, format!("`{callee}` requires `{port}`")));
            };
            self.require_output(
                &value,
                &[IrType::Component],
                span,
                format!("`{callee}` requires component `{port}`"),
            )?;
        }
        put(&mut inputs, "mate", IrValue::String(mate.to_owned()), span)?;
        if !inputs.contains_key("mate_kind") {
            inputs.insert("mate_kind".to_owned(), IrValue::String(mate.to_owned()));
        }
        if let Some(axis) = inputs.get("axis").cloned() {
            let axis = self.vector(axis, IrType::Vec3Dir, span)?;
            inputs.insert("axis".to_owned(), axis.clone());
            inputs
                .entry("axis_a".to_owned())
                .or_insert_with(|| axis.clone());
            inputs.entry("axis_b".to_owned()).or_insert(axis);
        }
        let mut metadata = BTreeMap::new();
        metadata.insert("dsl.placement_mate".to_owned(), "true".to_owned());
        let mut normalized = Inputs::new();
        for (port, raw) in inputs {
            let value = if matches!(port.as_str(), "left" | "right") {
                raw
            } else {
                self.literal_option(&port, raw, &mut metadata, span)?
            };
            normalized.insert(port, value);
        }
        Ok(self.add_node(
            name,
            NodeKind::AssemblyMate,
            IrType::Assembly,
            normalized,
            metadata,
        ))
    }

    fn call_part(&mut self, callee: &str, args: &[IrArg], span: Span) -> Result<IrValue> {
        let param_names: Vec<String> = self.parts[callee]
            .params
            .iter()
            .map(|p| p.name.clone())
            .collect();
        let mut overrides = HashMap::new();
        let mut positional = 0usize;
        for arg in args {
            let key = match &arg.name {
                Some(name) => name.clone(),
                None => {
                    let Some(name) = param_names.get(positional) else {
                        return Err(at(
                            arg.span,
                            format!("unexpected positional argument to part `{callee}`"),
                        ));
                    };
                    positional += 1;
                    name.clone()
                }
            };
            let value = self.value(&arg.value, None)?;
            overrides.insert(key, value);
        }
        self.instantiate_part(callee, &overrides, span)
    }

    fn tag_face_query(
        &mut self,
        callee: &str,
        args: &[IrArg],
        root: IrValue,
        mut query: QueryTopoRef,
        name: Option<&str>,
        span: Span,
    ) -> Result<IrValue> {
        let mut raw = self.ordered_args(args, |lower, value| lower.value(value, None))?;
        let tag_val = raw
            .remove("tag")
            .or_else(|| raw.remove("0"))
            .unwrap_or_else(|| IrValue::String("tagged".to_owned()));
        if callee == "tag::cylindrical_faces" {
            query
                .filters
                .push(TopoFilter::Surface(SurfaceClass::Cylinder));
        }
        let expr = serde_json::to_string(&TopoRef::Query(query))
            .map_err(|error| at(span, error.to_string()))?;
        let mut inputs = Inputs::new();
        inputs.insert("geometry".to_owned(), root);
        inputs.insert("tag".to_owned(), tag_val);
        inputs.insert(
            "faces".to_owned(),
            IrValue::List(vec![IrValue::TopoRefExpr(expr.clone())]),
        );
        let raw_copy = inputs.clone();
        self.attach_feature_spec(&NodeKind::TagFaces, &raw_copy, &mut inputs);
        let tagged = self.add_node(name, NodeKind::TagFaces, IrType::Solid, inputs, BTreeMap::new());
        Ok(topo_query_value(tagged, expr))
    }

    fn direct_face_edit(
        &mut self,
        callee: &str,
        args: &[IrArg],
        piped: Option<IrValue>,
        name: Option<&str>,
        span: Span,
    ) -> Result<IrValue> {
        let mut raw = self.ordered_args(args, |lower, value| lower.value(value, None))?;
        let solid = piped
            .or_else(|| raw.remove("solid"))
            .or_else(|| raw.remove("base"))
            .or_else(|| raw.remove("target"))
            .ok_or_else(|| at(span, format!("`{callee}` requires an input Solid")))?;
        let IrValue::NodeRef {
            node_id: solid_id, ..
        } = &solid
        else {
            return Err(at(span, format!("`{callee}` requires an input Solid")));
        };
        let face_raw = raw
            .remove("faces")
            .or_else(|| raw.remove("face"))
            .or_else(|| raw.remove("0"))
            .ok_or_else(|| at(span, format!("`{callee}` requires `faces`")))?;
        let face_item = match face_raw {
            IrValue::List(mut items) if items.len() == 1 => items.remove(0),
            other => other,
        };
        let projected = project_query_item(Some(&solid), face_item);
        let face_expr = match projected {
            IrValue::TopoRefExpr(expr) => expr,
            IrValue::String(expr) => expr,
            _ => {
                return Err(at(
                    span,
                    format!(
                        "E202_EMPTY_SELECTOR: `{callee}` requires a face topology selector on the input solid"
                    ),
                ));
            }
        };
        let mut inputs = Inputs::new();
        inputs.insert("solid".to_owned(), solid.clone());
        inputs.insert("face".to_owned(), IrValue::TopoRefExpr(face_expr.clone()));
        let edit = match callee {
            "face::offset" => {
                let delta = raw
                    .remove("delta")
                    .or_else(|| raw.remove("distance"))
                    .or_else(|| raw.remove("offset"))
                    .or_else(|| raw.remove("1"))
                    .ok_or_else(|| at(span, "face::offset requires a length `delta`"))?;
                let dim = self
                    .dim_input_from_value(&delta)
                    .ok_or_else(|| at(span, "face::offset `delta` must be a length"))?;
                inputs.insert("offset".to_owned(), delta);
                DirectFaceEditOp::Offset { distance: dim }
            }
            "face::move" => {
                let offset_val = raw
                    .remove("offset")
                    .or_else(|| raw.remove("delta"))
                    .or_else(|| raw.remove("1"))
                    .ok_or_else(|| {
                        at(
                            span,
                            "face::move requires three length components in `offset`",
                        )
                    })?;
                let vec_val = self.vector(offset_val, IrType::Vec3Length, span)?;
                let delta_mm = origin_vec3(self, &vec_val).ok_or_else(|| {
                    at(
                        span,
                        "face::move requires three length components in `offset`",
                    )
                })?;
                DirectFaceEditOp::Move { delta_mm }
            }
            "face::delete" => DirectFaceEditOp::DeleteAndHeal,
            _ => unreachable!(),
        };
        let spec = FeatureSpec::DirectFaceEdit {
            solid: solid_id.clone(),
            face: face_expr,
            edit,
        };
        spec.validate(&NodeId("spec".into()))
            .map_err(|error| at(span, error.to_string()))?;
        let spec_json =
            serde_json::to_string(&spec).map_err(|error| at(span, error.to_string()))?;
        inputs.insert(FEATURE_SPEC_PORT.to_owned(), IrValue::String(spec_json));
        Ok(self.add_node(
            name,
            NodeKind::DirectFaceEdit,
            IrType::Solid,
            inputs,
            BTreeMap::new(),
        ))
    }

    fn profile_sketch(
        &mut self,
        callee: &str,
        args: &[IrArg],
        name: Option<&str>,
        span: Span,
    ) -> Result<IrValue> {
        let raw = self.ordered_args(args, |lower, value| lower.value(value, None))?;
        let datum = match raw.get("plane").map(|v| self.nominal(v)) {
            None => DatumPlane::Xy,
            Some(IrValue::String(plane_name)) => datum_plane_from_name(plane_name)
                .ok_or_else(|| at(span, format!("E207_INVALID_FEATURE: unsupported plane `{plane_name}`")))?,
            _ => {
                return Err(at(
                    span,
                    "E207_INVALID_FEATURE: profile plane must be a datum plane",
                ));
            }
        };
        let as_vec2 = |value: &IrValue| -> Option<[f64; 2]> {
            let IrValue::List(items) = self.nominal(value) else {
                return None;
            };
            if items.len() != 2 {
                return None;
            }
            Some([self.length_mm(&items[0])?, self.length_mm(&items[1])?])
        };
        let as_u32 = |value: &IrValue| -> Option<u32> {
            match self.nominal(value) {
                IrValue::Int(v) if *v > 0 => u32::try_from(*v).ok(),
                IrValue::Scalar(x) if x.is_finite() && *x > 0.0 => Some(x.round() as u32),
                _ => None,
            }
        };
        let entity_id = "profile_0".to_owned();
        let entity = match callee {
            "profile::circle" => {
                let center_mm = raw
                    .get("center")
                    .or_else(|| raw.get("origin"))
                    .and_then(as_vec2)
                    .unwrap_or([0.0, 0.0]);
                let radius = if let Some(rv) = raw.get("radius").or_else(|| raw.get("r")) {
                    self.length_mm(rv).ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: profile::circle `radius` must be a numeric length",
                        )
                    })?
                } else if let Some(dv) = raw
                    .get("diameter")
                    .or_else(|| raw.get("d"))
                    .or_else(|| raw.get("0"))
                {
                    0.5 * self.length_mm(dv).ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: profile::circle `d` must be a numeric length",
                        )
                    })?
                } else {
                    5.0
                };
                if !radius.is_finite() || radius <= 0.0 {
                    return Err(at(
                        span,
                        format!(
                            "E207_INVALID_FEATURE: profile::circle requires positive radius, found {radius}"
                        ),
                    ));
                }
                let segments = raw.get("segments").and_then(as_u32);
                SketchEntity2D::Ellipse {
                    id: entity_id.clone(),
                    center_mm,
                    rx_mm: radius,
                    ry_mm: radius,
                    segments,
                    construction: false,
                }
            }
            "profile::ellipse" => {
                let center_mm = raw
                    .get("center")
                    .or_else(|| raw.get("origin"))
                    .and_then(as_vec2)
                    .unwrap_or([0.0, 0.0]);
                let rx_mm = raw
                    .get("rx")
                    .or_else(|| raw.get("major_radius"))
                    .or_else(|| raw.get("a"))
                    .or_else(|| raw.get("0"))
                    .and_then(|v| self.length_mm(v))
                    .ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: ellipse requires semi-axis `rx` (or `major_radius` / `a`)",
                        )
                    })?;
                let ry_mm = raw
                    .get("ry")
                    .or_else(|| raw.get("minor_radius"))
                    .or_else(|| raw.get("b"))
                    .or_else(|| raw.get("1"))
                    .and_then(|v| self.length_mm(v))
                    .ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: ellipse requires semi-axis `ry` (or `minor_radius` / `b`)",
                        )
                    })?;
                if !rx_mm.is_finite() || !ry_mm.is_finite() || rx_mm <= 0.0 || ry_mm <= 0.0 {
                    return Err(at(
                        span,
                        format!(
                            "E207_INVALID_FEATURE: ellipse requires positive semi-axes rx and ry, found rx={rx_mm}, ry={ry_mm}"
                        ),
                    ));
                }
                let segments = raw.get("segments").and_then(as_u32);
                SketchEntity2D::Ellipse {
                    id: entity_id.clone(),
                    center_mm,
                    rx_mm,
                    ry_mm,
                    segments,
                    construction: false,
                }
            }
            "profile::spline" => {
                if matches!(raw.get("closed").map(|v| self.nominal(v)), Some(IrValue::Bool(false))) {
                    return Err(at(
                        span,
                        "E207_INVALID_FEATURE: closed spline profile requires `closed = true`",
                    ));
                }
                let pts_val = raw
                    .get("points")
                    .or_else(|| raw.get("control_points"))
                    .or_else(|| raw.get("pts"))
                    .or_else(|| raw.get("0"))
                    .ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: spline requires `points` (or `control_points` / `pts`)",
                        )
                    })?;
                let IrValue::List(items) = self.nominal(pts_val) else {
                    return Err(at(
                        span,
                        "E207_INVALID_FEATURE: spline `points` must be a list of 2D control points",
                    ));
                };
                let mut control_points_mm = Vec::with_capacity(items.len());
                for (idx, item) in items.iter().enumerate() {
                    let Some(pt) = as_vec2(item) else {
                        return Err(at(
                            span,
                            format!(
                                "E207_INVALID_FEATURE: spline control point {idx} must be a 2D [x, y] point"
                            ),
                        ));
                    };
                    control_points_mm.push(pt);
                }
                if control_points_mm.len() < 3 {
                    return Err(at(
                        span,
                        format!(
                            "E207_INVALID_FEATURE: spline requires at least 3 control points, found {}",
                            control_points_mm.len()
                        ),
                    ));
                }
                let segments_per_span = match raw.get("segments_per_span").or_else(|| raw.get("segments")) {
                    Some(v) => {
                        let Some(n) = as_u32(v) else {
                            return Err(at(
                                span,
                                "E207_INVALID_FEATURE: spline `segments_per_span` must be positive",
                            ));
                        };
                        Some(n)
                    }
                    None => None,
                };
                SketchEntity2D::Spline {
                    id: entity_id.clone(),
                    control_points_mm,
                    closed: true,
                    segments_per_span,
                    construction: false,
                }
            }
            _ => unreachable!(),
        };
        let sketch = StructuredSketch2D {
            plane: SketchPlaneSpec::datum(datum),
            entities: vec![entity],
            profiles: vec![SketchProfileLoop {
                id: "loop_0".to_owned(),
                entity_ids: vec![entity_id],
                is_outer: true,
            }],
            constraints: Vec::new(),
            remaining_dof: None,
        };
        let spec = FeatureSpec::Sketch2D(sketch);
        spec.validate(&NodeId("spec".into()))
            .map_err(|error| at(span, format!("E207_INVALID_FEATURE: {error}")))?;
        let spec_json =
            serde_json::to_string(&spec).map_err(|error| at(span, error.to_string()))?;
        let mut inputs = Inputs::new();
        inputs.insert(FEATURE_SPEC_PORT.to_owned(), IrValue::String(spec_json));
        Ok(self.add_node(
            name,
            NodeKind::Sketch2D,
            IrType::Sketch,
            inputs,
            BTreeMap::new(),
        ))
    }

    fn path_3d(&mut self, callee: &str, args: &[IrArg], span: Span) -> Result<IrValue> {
        let raw = self.ordered_args(args, |lower, value| lower.value(value, None))?;
        let num = |value: &IrValue| -> Option<f64> {
            match self.nominal(value) {
                IrValue::Quantity(q) => Some(q.canonical_value()),
                IrValue::Scalar(x) => Some(*x),
                IrValue::Int(x) => Some(*x as f64),
                _ => None,
            }
        };
        let as_vec3 = |value: &IrValue| -> Option<[f64; 3]> {
            match self.nominal(value) {
                IrValue::Vec3(v) => Some(*v),
                IrValue::List(items) if items.len() == 3 => {
                    Some([num(&items[0])?, num(&items[1])?, num(&items[2])?])
                }
                _ => None,
            }
        };
        let points = match callee {
            "path::helix" => {
                let radius = if let Some(rv) = raw.get("radius").or_else(|| raw.get("r")) {
                    num(rv).ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: path::helix `radius` must be a numeric length",
                        )
                    })?
                } else if let Some(dv) = raw.get("diameter").or_else(|| raw.get("d")) {
                    0.5 * num(dv).ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: path::helix `diameter` must be a numeric length",
                        )
                    })?
                } else if let Some(rv) = raw.get("0") {
                    num(rv).ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: path::helix `radius` must be a numeric length",
                        )
                    })?
                } else {
                    return Err(at(
                        span,
                        "E207_INVALID_FEATURE: path::helix requires `radius` (or `r` / `d` / `diameter`)",
                    ));
                };
                let pitch = raw
                    .get("pitch")
                    .or_else(|| raw.get("1"))
                    .and_then(num)
                    .ok_or_else(|| at(span, "E207_INVALID_FEATURE: path::helix requires `pitch`"))?;
                if !pitch.is_finite() || pitch <= 0.0 {
                    return Err(at(
                        span,
                        format!("E207_INVALID_FEATURE: path::helix requires positive `pitch`, found {pitch}"),
                    ));
                }
                let height_opt = match raw.get("height").or_else(|| raw.get("h")) {
                    Some(hv) => {
                        let h = num(hv).ok_or_else(|| {
                            at(
                                span,
                                "E207_INVALID_FEATURE: path::helix `height` must be a numeric length",
                            )
                        })?;
                        if !h.is_finite() || h <= 0.0 {
                            return Err(at(
                                span,
                                format!("E207_INVALID_FEATURE: path::helix `height` must be positive, found {h}"),
                            ));
                        }
                        Some(h)
                    }
                    None => None,
                };
                let turns = if let Some(tv) = raw.get("turns").or_else(|| raw.get("2")) {
                    let t = num(tv).ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: path::helix `turns` must be a numeric scalar",
                        )
                    })?;
                    if !t.is_finite() || t <= 0.0 {
                        return Err(at(
                            span,
                            format!("E207_INVALID_FEATURE: path::helix `turns` must be positive, found {t}"),
                        ));
                    }
                    t
                } else if let Some(h) = height_opt {
                    h / pitch
                } else {
                    return Err(at(
                        span,
                        "E207_INVALID_FEATURE: path::helix requires `turns` or `height` (or `h`)",
                    ));
                };
                let origin = match raw
                    .get("origin")
                    .or_else(|| raw.get("center"))
                    .or_else(|| raw.get("3"))
                {
                    Some(ov) => as_vec3(ov).ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: path::helix `origin` must be a 3D [x, y, z] point",
                        )
                    })?,
                    None => [0.0, 0.0, 0.0],
                };
                let segments_per_turn = match raw
                    .get("segments_per_turn")
                    .or_else(|| raw.get("segments"))
                {
                    Some(sv) => {
                        let n = num(sv).ok_or_else(|| {
                            at(
                                span,
                                "E207_INVALID_FEATURE: path::helix `segments_per_turn` must be a numeric scalar",
                            )
                        })?;
                        if !n.is_finite() || n <= 0.0 {
                            return Err(at(
                                span,
                                format!("E207_INVALID_FEATURE: path::helix `segments_per_turn` must be positive, found {n}"),
                            ));
                        }
                        (n.round() as usize).clamp(8, 96)
                    }
                    None => 24,
                };
                let left_handed = match raw.get("left_handed").map(|v| self.nominal(v)) {
                    Some(IrValue::Bool(b)) => *b,
                    None => false,
                    Some(_) => {
                        return Err(at(
                            span,
                            "E207_INVALID_FEATURE: path::helix `left_handed` must be a boolean",
                        ));
                    }
                };
                sample_helix_3d(radius, pitch, turns, origin, segments_per_turn, left_handed)
                    .map_err(|msg| at(span, format!("E207_INVALID_FEATURE: {msg}")))?
            }
            "path::spline3d" => {
                let pts_val = raw
                    .get("points")
                    .or_else(|| raw.get("control_points"))
                    .or_else(|| raw.get("pts"))
                    .or_else(|| raw.get("0"))
                    .ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: path::spline3d requires `points` (or `control_points` / `pts`)",
                        )
                    })?;
                let IrValue::List(items) = self.nominal(pts_val) else {
                    return Err(at(
                        span,
                        "E207_INVALID_FEATURE: path::spline3d `points` must be a list of 3D control points",
                    ));
                };
                let mut control_points = Vec::with_capacity(items.len());
                for (idx, item) in items.iter().enumerate() {
                    let Some(pt) = as_vec3(item) else {
                        return Err(at(
                            span,
                            format!("E207_INVALID_FEATURE: path::spline3d control point {idx} must be a 3D [x, y, z] point"),
                        ));
                    };
                    control_points.push(pt);
                }
                let segments_per_span = match raw
                    .get("segments_per_span")
                    .or_else(|| raw.get("segments"))
                {
                    Some(v) => {
                        let n = num(v).ok_or_else(|| {
                            at(
                                span,
                                "E207_INVALID_FEATURE: path::spline3d `segments_per_span` must be a numeric scalar",
                            )
                        })?;
                        if !n.is_finite() || n <= 0.0 {
                            return Err(at(
                                span,
                                format!("E207_INVALID_FEATURE: path::spline3d `segments_per_span` must be positive, found {n}"),
                            ));
                        }
                        (n.round() as usize).clamp(4, 64)
                    }
                    None => 12,
                };
                sample_spline3d(&control_points, segments_per_span)
                    .map_err(|msg| at(span, format!("E207_INVALID_FEATURE: {msg}")))?
            }
            "path::polyline" => {
                let pts_val = raw
                    .get("points")
                    .or_else(|| raw.get("pts"))
                    .or_else(|| raw.get("0"))
                    .ok_or_else(|| {
                        at(
                            span,
                            "E207_INVALID_FEATURE: path::polyline requires `points`",
                        )
                    })?;
                let IrValue::List(items) = self.nominal(pts_val) else {
                    return Err(at(
                        span,
                        "E207_INVALID_FEATURE: path::polyline `points` must be a list of 3D points",
                    ));
                };
                let mut pts = Vec::with_capacity(items.len());
                for (idx, item) in items.iter().enumerate() {
                    let Some(pt) = as_vec3(item) else {
                        return Err(at(
                            span,
                            format!("E207_INVALID_FEATURE: path::polyline point {idx} must be a 3D [x, y, z] point"),
                        ));
                    };
                    if pt.iter().any(|c| !c.is_finite()) {
                        return Err(at(
                            span,
                            format!("E207_INVALID_FEATURE: path::polyline point {idx} must have finite coordinates"),
                        ));
                    }
                    pts.push(pt);
                }
                if pts.len() < 2 {
                    return Err(at(
                        span,
                        "E207_INVALID_FEATURE: path::polyline requires at least 2 points",
                    ));
                }
                if pts.windows(2).any(|pair| {
                    let d = [
                        pair[0][0] - pair[1][0],
                        pair[0][1] - pair[1][1],
                        pair[0][2] - pair[1][2],
                    ];
                    d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= 1e-12
                }) {
                    return Err(at(
                        span,
                        "E207_INVALID_FEATURE: path::polyline consecutive points must be distinct",
                    ));
                }
                pts
            }
            _ => unreachable!(),
        };
        Ok(IrValue::List(
            points.into_iter().map(IrValue::Vec3).collect(),
        ))
    }

    fn assert_call(
        &mut self,
        callee: &str,
        args: &[IrArg],
        name: Option<&str>,
        span: Span,
    ) -> Result<IrValue> {
        let mut raw = self.ordered_args(args, |lower, value| lower.value(value, None))?;
        let mut inputs = Inputs::new();
        let mut metadata = BTreeMap::new();
        let mut params = BTreeMap::new();
        let mut target_node: Option<NodeId> = None;

        if let Some(target) = raw.remove("target") {
            if let IrValue::NodeRef { node_id, .. } = &target {
                target_node = Some(node_id.clone());
            }
            inputs.insert("target".to_owned(), target);
        }

        match callee {
            "assert::bbox" => {
                if let Some(max_size) = raw.remove("max_size") {
                    let vec_val = self.vector(max_size, IrType::Vec3Length, span)?;
                    if let Some(sz) = origin_vec3(self, &vec_val)
                        && let Ok(json) = serde_json::to_string(&sz)
                    {
                        params.insert("max_size_mm".to_owned(), json);
                    }
                    inputs.insert("max_size".to_owned(), vec_val);
                }
            }
            "assert::dfm_accessible" => {
                if let Some(axes_val) = raw.remove("axes") {
                    let items = match axes_val {
                        IrValue::List(list) => list,
                        single => vec![single],
                    };
                    let mut vecs = Vec::with_capacity(items.len());
                    let mut raw_axes = Vec::with_capacity(items.len());
                    for item in items {
                        let v = self.vector(item, IrType::Vec3Dir, span)?;
                        if let Some(dir) = origin_vec3(self, &v) {
                            raw_axes.push(dir);
                        }
                        vecs.push(v);
                    }
                    if let Ok(json) = serde_json::to_string(&raw_axes) {
                        params.insert("axes".to_owned(), json);
                    }
                    inputs.insert("axes".to_owned(), IrValue::List(vecs));
                }
                if let Some(min_wall) = raw.remove("min_wall_thickness") {
                    if let Some(mm) = self.length_mm(&min_wall) {
                        params.insert("min_wall_thickness_mm".to_owned(), mm.to_string());
                    }
                    inputs.insert("min_wall_thickness".to_owned(), min_wall);
                }
            }
            "assert::no_interference" => {
                if let Some(a) = raw.remove("a") {
                    if let IrValue::NodeRef { node_id, .. } = &a {
                        params.insert("a".to_owned(), node_id.0.clone());
                        if target_node.is_none() {
                            target_node = Some(node_id.clone());
                        }
                    }
                    inputs.insert("a".to_owned(), a);
                }
                if let Some(b) = raw.remove("b") {
                    if let IrValue::NodeRef { node_id, .. } = &b {
                        params.insert("b".to_owned(), node_id.0.clone());
                    }
                    inputs.insert("b".to_owned(), b);
                }
                if let Some(clearance) = raw.remove("min_clearance") {
                    if let Some(mm) = self.length_mm(&clearance) {
                        params.insert("min_clearance_mm".to_owned(), mm.to_string());
                    }
                    inputs.insert("min_clearance".to_owned(), clearance);
                }
            }
            "assert::physics" => {
                let target_ref = inputs.get("target").cloned();
                if let Some(fixed) = raw.remove("fixed") {
                    let list = match fixed {
                        IrValue::List(items) => items,
                        single => vec![single],
                    };
                    let projected = list
                        .into_iter()
                        .map(|item| project_query_item(target_ref.as_ref(), item))
                        .collect();
                    inputs.insert("fixed".to_owned(), IrValue::List(projected));
                }
                if let Some(loads) = raw.remove("loads") {
                    let list = match loads {
                        IrValue::List(items) => items,
                        single => vec![single],
                    };
                    inputs.insert("loads".to_owned(), IrValue::List(list));
                }
                if let Some(sf) = raw.remove("min_safety_factor") {
                    let lit = self.literal_option("min_safety_factor", sf, &mut metadata, span)?;
                    if let IrValue::Scalar(v) = lit {
                        params.insert("min_safety_factor".to_owned(), v.to_string());
                    }
                    inputs.insert("min_safety_factor".to_owned(), lit);
                }
            }
            _ => {}
        }

        for (port, val) in raw {
            let lit = self.literal_option(&port, val, &mut metadata, span)?;
            inputs.insert(port, lit);
        }

        let spec = FeatureSpec::DesignAssertion {
            target: target_node,
            assertion_id: callee.to_owned(),
            kind: DesignAssertionKind::Custom {
                expression: callee.to_owned(),
                params,
            },
        };
        if spec.validate(&NodeId("spec".into())).is_ok()
            && let Ok(spec_json) = serde_json::to_string(&spec)
        {
            inputs.insert(FEATURE_SPEC_PORT.to_owned(), IrValue::String(spec_json));
        }

        Ok(self.add_node(name, NodeKind::AssertGate, IrType::Bool, inputs, metadata))
    }

    fn require_output(
        &self,
        value: &IrValue,
        expected: &[IrType],
        span: Span,
        message: impl Into<String>,
    ) -> Result<()> {
        let IrValue::NodeRef { node_id, .. } = value else {
            return Err(at(span, message));
        };
        let actual = self.graph.nodes[&node_id.0].output_type;
        if expected.contains(&actual) {
            Ok(())
        } else {
            Err(at(span, message))
        }
    }

    fn normalize_path(&mut self, value: IrValue, span: Span) -> Result<IrValue> {
        let IrValue::List(items) = value else {
            return Ok(value);
        };
        if !items.iter().all(|item| matches!(item, IrValue::List(_))) {
            return Ok(IrValue::List(items));
        }
        items
            .into_iter()
            .map(|point| self.vector(point, IrType::Vec3Length, span))
            .collect::<Result<Vec<_>>>()
            .map(IrValue::List)
    }

    /// Sketch statement maps cannot carry node connections. External faces and
    /// solids move to the `anchors` port, and each use site keeps `@anchor:N`.
    fn detach_anchors(&self, value: &mut IrValue, anchors: &mut Vec<IrValue>) {
        if let IrValue::NodeRef { .. } = value
            && self.parameter(value).is_none()
        {
            let index = anchors.iter().position(|anchor| anchor == value);
            let index = if let Some(index) = index {
                index
            } else {
                anchors.push(value.clone());
                anchors.len() - 1
            };
            *value = IrValue::String(format!("@anchor:{index}"));
            return;
        }
        match value {
            IrValue::List(items) => {
                for item in items {
                    self.detach_anchors(item, anchors);
                }
            }
            IrValue::Map(entries) => {
                for entry in entries.values_mut() {
                    self.detach_anchors(entry, anchors);
                }
            }
            _ => {}
        }
    }

    fn extension(
        &mut self,
        callee: &str,
        output: IrType,
        args: &[IrArg],
        piped: Option<IrValue>,
        name: Option<&str>,
    ) -> Result<IrValue> {
        let mut inputs = self.ordered_args(args, |lower, value| lower.value(value, None))?;
        if let Some(receiver) = piped {
            inputs.insert("receiver".to_owned(), receiver);
        }
        let inputs = Inputs::from([
            ("function".to_owned(), IrValue::String(callee.to_owned())),
            ("arguments".to_owned(), IrValue::Map(inputs)),
        ]);
        Ok(self.add_node(name, NodeKind::StdLibCall, output, inputs, BTreeMap::new()))
    }

    pub(super) fn sketch(
        &mut self,
        args: &[IrArg],
        stmts: &[IrSketchStmt],
        name: Option<&str>,
        span: Span,
    ) -> Result<IrValue> {
        let mut inputs = self.ordered_args(args, |lower, value| lower.value(value, None))?;
        let mut statements = Vec::with_capacity(stmts.len());
        let mut shadowed = Vec::new();
        for (index, stmt) in stmts.iter().enumerate() {
            let (binding, expr) = match stmt {
                IrSketchStmt::Let { name, expr, .. } => (Some(name), expr),
                IrSketchStmt::Expr { expr, .. } => (None, expr),
            };
            let value = self.sketch_value(expr)?;
            let mut entry = Inputs::from([("value".to_owned(), value.clone())]);
            if let Some(binding) = binding {
                entry.insert("name".to_owned(), IrValue::String(binding.clone()));
                let bound = if matches!(expr.ty(), DimType::SketchEntity | DimType::SketchPoint) {
                    IrValue::Map(Inputs::from([(
                        "local".to_owned(),
                        IrValue::Int(index as i64),
                    )]))
                } else {
                    value
                };
                shadowed.push((binding, self.bindings.insert(binding.clone(), bound)));
            }
            statements.push(IrValue::Map(entry));
        }
        for (binding, previous) in shadowed.into_iter().rev() {
            if let Some(previous) = previous {
                self.bindings.insert(binding.clone(), previous);
            } else {
                self.bindings.remove(binding);
            }
        }
        put(&mut inputs, "statements", IrValue::List(statements), span)?;
        let mut anchors = Vec::new();
        for value in inputs.values_mut() {
            self.detach_anchors(value, &mut anchors);
        }
        let structured_sketch = match inputs.get("statements") {
            Some(IrValue::List(stmts)) => self.try_build_structured_sketch(&inputs, stmts),
            _ => None,
        };
        if !anchors.is_empty() {
            inputs.insert("anchors".to_owned(), IrValue::List(anchors));
        }
        let mut metadata = BTreeMap::new();
        for (port, value) in &mut inputs {
            let raw = std::mem::replace(value, IrValue::Bool(false));
            if port == "anchors" {
                *value = raw;
                continue;
            }
            *value = self.literal_option(port, raw, &mut metadata, span)?;
        }
        if let Some(built) = structured_sketch {
            if !built.anchor_tangents.is_empty()
                && let Ok(tangents_json) = serde_json::to_string(&built.anchor_tangents)
            {
                metadata.insert("dsl.sketch_anchor_tangents".to_owned(), tangents_json);
            }
            if let Ok(spec_json) = serde_json::to_string(&FeatureSpec::Sketch2D(built.sketch)) {
                inputs.insert(FEATURE_SPEC_PORT.to_owned(), IrValue::String(spec_json));
            }
        }
        Ok(self.add_node(name, NodeKind::Sketch2D, IrType::Sketch, inputs, metadata))
    }

    fn attach_feature_spec(&mut self, kind: &NodeKind, raw_inputs: &Inputs, inputs: &mut Inputs) {
        let spec = match kind {
            NodeKind::PrimitiveBox => {
                let width = inputs
                    .get("width")
                    .and_then(|v| self.dim_input_from_value(v));
                let depth = inputs
                    .get("depth")
                    .or_else(|| inputs.get("length"))
                    .and_then(|v| self.dim_input_from_value(v));
                let height = inputs
                    .get("height")
                    .and_then(|v| self.dim_input_from_value(v));
                let origin_mm = match inputs.get("origin") {
                    Some(v) => match origin_vec3(self, v) {
                        Some(o) => o,
                        None => return,
                    },
                    None => [0.0, 0.0, 0.0],
                };
                let (Some(width), Some(depth), Some(height)) = (width, depth, height) else {
                    return;
                };
                Some(FeatureSpec::PrimitiveBox {
                    width,
                    depth,
                    height,
                    origin_mm,
                })
            }
            NodeKind::PrimitiveCylinder => {
                let axis = match inputs.get("axis") {
                    Some(axis_val) => match *self.nominal(axis_val) {
                        IrValue::Vec3(axis) => axis,
                        _ => return,
                    },
                    None => [0.0, 0.0, 1.0],
                };
                let origin_mm = match inputs.get("origin") {
                    Some(v) => match origin_vec3(self, v) {
                        Some(o) => o,
                        None => return,
                    },
                    None => [0.0, 0.0, 0.0],
                };
                let radius = if let Some(r) = inputs.get("radius").cloned() {
                    let Some(dim) = self.dim_input_from_value(&r) else {
                        return;
                    };
                    dim
                } else if let Some(d) = inputs.get("diameter").cloned() {
                    let Some(dim) = self.half_length_dim_input(&d) else {
                        return;
                    };
                    dim
                } else {
                    return;
                };
                let Some(height) = inputs
                    .get("height")
                    .and_then(|v| self.dim_input_from_value(v))
                else {
                    return;
                };
                Some(FeatureSpec::PrimitiveCylinder {
                    radius,
                    height,
                    origin_mm,
                    axis,
                })
            }
            NodeKind::Extrude => {
                let Some(IrValue::NodeRef {
                    node_id: sketch, ..
                }) = inputs.get("profile").or_else(|| inputs.get("sketch"))
                else {
                    return;
                };
                let Some(distance) = inputs
                    .get("distance")
                    .or_else(|| inputs.get("height"))
                    .or_else(|| inputs.get("depth"))
                    .or_else(|| inputs.get("thickness"))
                    .and_then(|v| self.dim_input_from_value(v))
                else {
                    return;
                };
                let direction = match inputs.get("direction") {
                    Some(v) => Some(match origin_vec3(self, v) {
                        Some(d) => d,
                        None => return,
                    }),
                    None => None,
                };
                let symmetric = matches!(inputs.get("symmetric"), Some(IrValue::Bool(true)));
                Some(FeatureSpec::Extrude {
                    sketch: sketch.clone(),
                    distance,
                    direction,
                    symmetric,
                    operation: FeatureBodyOp::NewBody,
                    target: None,
                })
            }
            NodeKind::Revolve => {
                let Some(IrValue::NodeRef {
                    node_id: sketch, ..
                }) = inputs.get("profile").or_else(|| inputs.get("sketch"))
                else {
                    return;
                };
                let axis_origin_mm = match inputs.get("origin") {
                    Some(v) => match origin_vec3(self, v) {
                        Some(o) => o,
                        None => return,
                    },
                    None => [0.0, 0.0, 0.0],
                };
                let axis_direction = match inputs.get("axis") {
                    Some(v) => match origin_vec3(self, v) {
                        Some(a) => a,
                        None => return,
                    },
                    None => [0.0, 0.0, 1.0],
                };
                let angle = match inputs.get("angle") {
                    Some(v) => match self.dim_input_from_value(v) {
                        Some(a) => a,
                        None => return,
                    },
                    None => match DimInput::deg(360.0) {
                        Ok(a) => a,
                        Err(_) => return,
                    },
                };
                Some(FeatureSpec::Revolve {
                    sketch: sketch.clone(),
                    axis_origin_mm,
                    axis_direction,
                    angle,
                    operation: FeatureBodyOp::NewBody,
                    target: None,
                })
            }
            NodeKind::BooleanUnion => {
                let mut operands = Vec::new();
                for port in ["base", "target", "left", "a"] {
                    if let Some(IrValue::NodeRef { node_id, .. }) = inputs.get(port) {
                        operands.push(node_id.clone());
                    }
                }
                for port in ["solids", "tools"] {
                    if let Some(IrValue::List(items)) = inputs.get(port) {
                        for item in items {
                            let IrValue::NodeRef { node_id, .. } = item else {
                                return;
                            };
                            operands.push(node_id.clone());
                        }
                    }
                }
                for port in ["tool", "right", "b"] {
                    if let Some(IrValue::NodeRef { node_id, .. }) = inputs.get(port) {
                        operands.push(node_id.clone());
                    }
                }
                if operands.len() < 2 {
                    return;
                }
                let target = operands.remove(0);
                Some(FeatureSpec::Boolean {
                    op: BooleanOpKind::Union,
                    target,
                    tools: operands,
                })
            }
            NodeKind::BooleanSubtract => {
                let Some(IrValue::NodeRef {
                    node_id: target, ..
                }) = inputs
                    .get("base")
                    .or_else(|| inputs.get("target"))
                    .or_else(|| inputs.get("left"))
                    .or_else(|| inputs.get("a"))
                else {
                    return;
                };
                let mut tools = Vec::new();
                if let Some(IrValue::List(items)) = inputs.get("tools") {
                    for item in items {
                        let IrValue::NodeRef { node_id, .. } = item else {
                            return;
                        };
                        tools.push(node_id.clone());
                    }
                } else if let Some(IrValue::NodeRef { node_id, .. }) = inputs
                    .get("tool")
                    .or_else(|| inputs.get("right"))
                    .or_else(|| inputs.get("b"))
                {
                    tools.push(node_id.clone());
                }
                if tools.is_empty() {
                    return;
                }
                Some(FeatureSpec::Boolean {
                    op: BooleanOpKind::Subtract,
                    target: target.clone(),
                    tools,
                })
            }
            NodeKind::Fillet => {
                let Some(IrValue::NodeRef { node_id: solid, .. }) = inputs.get("solid") else {
                    return;
                };
                let Some(radius) = inputs
                    .get("radius")
                    .and_then(|v| self.dim_input_from_value(v))
                else {
                    return;
                };
                let mut edges = Vec::new();
                if let Some(IrValue::List(items)) = inputs.get("edges") {
                    for item in items {
                        let IrValue::TopoRefExpr(expr) = item else {
                            return;
                        };
                        edges.push(expr.clone());
                    }
                }
                Some(FeatureSpec::Fillet {
                    solid: solid.clone(),
                    edges,
                    radius,
                })
            }
            NodeKind::Chamfer => {
                let Some(IrValue::NodeRef { node_id: solid, .. }) = inputs.get("solid") else {
                    return;
                };
                let Some(distance) = inputs
                    .get("distance")
                    .and_then(|v| self.dim_input_from_value(v))
                else {
                    return;
                };
                let distance2 = inputs
                    .get("distance2")
                    .and_then(|v| self.dim_input_from_value(v));
                let angle = inputs
                    .get("angle")
                    .and_then(|v| self.dim_input_from_value(v));
                let mut edges = Vec::new();
                if let Some(IrValue::List(items)) = inputs.get("edges") {
                    for item in items {
                        let IrValue::TopoRefExpr(expr) = item else {
                            return;
                        };
                        edges.push(expr.clone());
                    }
                }
                Some(FeatureSpec::Chamfer {
                    solid: solid.clone(),
                    edges,
                    distance,
                    distance2,
                    angle,
                })
            }
            NodeKind::Shell => {
                let Some(IrValue::NodeRef { node_id: solid, .. }) = inputs.get("solid") else {
                    return;
                };
                let Some(thickness) = inputs
                    .get("thickness")
                    .and_then(|v| self.dim_input_from_value(v))
                else {
                    return;
                };
                let mut open_faces = Vec::new();
                if let Some(IrValue::List(items)) = inputs.get("open_faces") {
                    for item in items {
                        let IrValue::TopoRefExpr(expr) = item else {
                            return;
                        };
                        open_faces.push(expr.clone());
                    }
                }
                let inward = !matches!(inputs.get("inward"), Some(IrValue::Bool(false)));
                Some(FeatureSpec::Shell {
                    solid: solid.clone(),
                    open_faces,
                    thickness,
                    inward,
                })
            }
            NodeKind::PatternLinear => {
                let Some(IrValue::NodeRef { node_id: solid, .. }) = inputs.get("solid") else {
                    return;
                };
                let direction = match inputs.get("direction") {
                    Some(v) => match origin_vec3(self, v) {
                        Some(d) => d,
                        None => return,
                    },
                    None => [1.0, 0.0, 0.0],
                };
                let Some(count) = inputs.get("count").and_then(|v| self.int_input_from_value(v))
                else {
                    return;
                };
                let Some(spacing) = inputs
                    .get("spacing")
                    .or_else(|| inputs.get("step"))
                    .and_then(|v| self.dim_input_from_value(v))
                else {
                    return;
                };
                Some(FeatureSpec::LinearPattern {
                    solid: solid.clone(),
                    direction,
                    count,
                    spacing,
                    second_direction: None,
                })
            }
            NodeKind::PatternPolar => {
                let Some(IrValue::NodeRef { node_id: solid, .. }) = inputs.get("solid") else {
                    return;
                };
                let axis_origin_mm = match raw_inputs
                    .get("axis_origin")
                    .or_else(|| inputs.get("axis_origin"))
                {
                    Some(v) => match origin_vec3(self, v) {
                        Some(o) => o,
                        None => return,
                    },
                    None => [0.0, 0.0, 0.0],
                };
                let axis_direction = match inputs.get("axis") {
                    Some(v) => match origin_vec3(self, v) {
                        Some(a) => a,
                        None => return,
                    },
                    None => [0.0, 0.0, 1.0],
                };
                let Some(count) = inputs.get("count").and_then(|v| self.int_input_from_value(v))
                else {
                    return;
                };
                let Some(total_angle) = inputs
                    .get("angle")
                    .and_then(|v| self.dim_input_from_value(v))
                else {
                    return;
                };
                let radius = raw_inputs
                    .get("radius")
                    .or_else(|| inputs.get("radius"))
                    .and_then(|v| self.dim_input_from_value(v));
                let start_angle = raw_inputs
                    .get("start_angle")
                    .or_else(|| inputs.get("start_angle"))
                    .and_then(|v| self.dim_input_from_value(v));
                Some(FeatureSpec::PolarPattern {
                    solid: solid.clone(),
                    axis_origin_mm,
                    axis_direction,
                    count,
                    total_angle,
                    radius,
                    start_angle,
                })
            }
            NodeKind::Mirror => {
                let Some(IrValue::NodeRef { node_id: solid, .. }) = inputs.get("solid") else {
                    return;
                };
                let Some(IrValue::String(plane_name)) = inputs.get("plane") else {
                    return;
                };
                let Some(datum) = datum_plane_from_name(plane_name) else {
                    return;
                };
                let keep_original = match inputs.get("merge") {
                    Some(IrValue::Bool(value)) => *value,
                    _ => true,
                };
                Some(FeatureSpec::Mirror {
                    solid: solid.clone(),
                    plane: SketchPlaneSpec::datum(datum),
                    keep_original,
                })
            }
            NodeKind::Sweep => {
                let Some(IrValue::NodeRef {
                    node_id: profile, ..
                }) = inputs.get("profile")
                else {
                    return;
                };
                let path = match inputs.get("path") {
                    Some(IrValue::NodeRef { node_id, .. }) => SweepPathSpec::Sketch {
                        node_id: node_id.clone(),
                    },
                    Some(IrValue::List(items)) if items.len() >= 2 => {
                        let mut points = Vec::with_capacity(items.len());
                        for item in items {
                            match self.nominal(item) {
                                IrValue::Vec3(pt) => points.push(*pt),
                                _ => return,
                            }
                        }
                        SweepPathSpec::Polyline { points_mm: points }
                    }
                    _ => return,
                };
                Some(FeatureSpec::Sweep {
                    profile: profile.clone(),
                    path,
                    keep_normal: false,
                })
            }
            NodeKind::LoftGuided => {
                let Some(IrValue::List(raw_sections)) = inputs.get("sections").cloned() else {
                    return;
                };
                if raw_sections.len() < 2 {
                    return;
                }
                let mut section_ids = Vec::with_capacity(raw_sections.len());
                for item in &raw_sections {
                    match item {
                        IrValue::NodeRef { node_id, .. } => section_ids.push(node_id.clone()),
                        _ => return,
                    }
                }
                if let Some(offsets) = self.loft_section_offsets(inputs, section_ids.len()) {
                    let mut updated_refs = Vec::with_capacity(section_ids.len());
                    for (idx, section_id) in section_ids.iter_mut().enumerate() {
                        let offset = offsets[idx];
                        if offset.abs() > 1e-9
                            && let Some(shifted) = self.clone_sketch_with_offset(section_id, offset)
                        {
                            *section_id = shifted;
                        }
                        updated_refs.push(super::node_ref(section_id.clone()));
                    }
                    inputs.insert("sections".to_owned(), IrValue::List(updated_refs));
                }
                Some(FeatureSpec::Loft {
                    sections: section_ids,
                    guides: Vec::new(),
                    closed: false,
                    as_surface: false,
                })
            }
            NodeKind::TagFaces => {
                let Some(IrValue::NodeRef { node_id, .. }) = inputs
                    .get("geometry")
                    .or_else(|| inputs.get("solid"))
                    .or_else(|| inputs.get("surface"))
                else {
                    return;
                };
                let Some(IrValue::String(tag)) = inputs.get("tag") else {
                    return;
                };
                if tag.trim().is_empty() {
                    return;
                }
                let Some(IrValue::List(items)) = inputs.get("faces") else {
                    return;
                };
                let mut targets = Vec::with_capacity(items.len());
                for item in items {
                    let IrValue::TopoRefExpr(expr) = item else {
                        return;
                    };
                    targets.push(expr.clone());
                }
                Some(FeatureSpec::TagTopology {
                    solid: node_id.clone(),
                    tag: tag.clone(),
                    targets,
                })
            }
            _ => None,
        };
        if let Some(spec) = spec
            && spec.validate(&NodeId("spec".into())).is_ok()
            && let Ok(spec_json) = serde_json::to_string(&spec)
        {
            inputs.insert(FEATURE_SPEC_PORT.to_owned(), IrValue::String(spec_json));
        }
    }

    fn int_input_from_value(&self, value: &IrValue) -> Option<IntInput> {
        if let Some(param) = self.parameter(value) {
            return Some(IntInput::param(param.name.clone()));
        }
        match value {
            IrValue::Int(v) => Some(IntInput::literal(*v)),
            _ => None,
        }
    }

    fn dim_input_from_value(&self, value: &IrValue) -> Option<DimInput> {
        if let Some(param) = self.parameter(value) {
            return Some(DimInput::param(param.name.clone()));
        }
        match value {
            IrValue::Quantity(q) => Some(DimInput::Literal(*q)),
            _ => None,
        }
    }

    fn half_length_dim_input(&mut self, value: &IrValue) -> Option<DimInput> {
        if let Some(param) = self.parameter(value).cloned() {
            if param.ir_type != IrType::Length {
                return None;
            }
            let IrValue::Quantity(q) = param.value else {
                return None;
            };
            let half_q = Quantity::length(q.canonical_value() * 0.5, LengthUnit::Mm).ok()?;
            let formula = format!(
                "({} / 2.0)",
                super::expressions::parameter_symbol(&param.name.0)
            );
            let derived_id = self.generated_id();
            let _ = self.add_parameter(
                derived_id.clone(),
                IrType::Length,
                IrValue::Quantity(half_q),
                Some(formula),
            );
            return Some(DimInput::param(derived_id));
        }
        match value {
            IrValue::Quantity(q) if q.ir_type() == IrType::Length => {
                DimInput::mm(q.canonical_value() * 0.5).ok()
            }
            _ => None,
        }
    }

    fn loft_section_offsets(&self, inputs: &Inputs, count: usize) -> Option<Vec<f64>> {
        if let Some(IrValue::List(items)) = inputs.get("offsets")
            && items.len() == count
        {
            let mut offsets = Vec::with_capacity(count);
            for item in items {
                match self.nominal(item) {
                    IrValue::Quantity(q) if q.ir_type() == IrType::Length => {
                        offsets.push(q.canonical_value());
                    }
                    IrValue::Scalar(x) => offsets.push(*x),
                    _ => return None,
                }
            }
            return Some(offsets);
        }
        let height = match inputs.get("height").map(|v| self.nominal(v)) {
            Some(IrValue::Quantity(q)) if q.ir_type() == IrType::Length => q.canonical_value(),
            Some(IrValue::Scalar(x)) => *x,
            _ => return None,
        };
        let denom = count.saturating_sub(1).max(1) as f64;
        Some((0..count).map(|i| height * i as f64 / denom).collect())
    }

    fn clone_sketch_with_offset(&mut self, section_id: &NodeId, offset_mm: f64) -> Option<NodeId> {
        let existing = self.graph.nodes.get(&section_id.0)?.clone();
        if existing.kind != NodeKind::Sketch2D {
            return None;
        }
        let FeatureSpec::Sketch2D(mut sketch) = existing.try_feature_spec().ok()? else {
            return None;
        };
        match &mut sketch.plane {
            SketchPlaneSpec::Datum { offset, .. } if offset.is_none() => {
                *offset = Some(SketchDimValue::mm(offset_mm).ok()?);
            }
            _ => return None,
        }
        let spec_json = serde_json::to_string(&FeatureSpec::Sketch2D(sketch)).ok()?;
        let mut cloned_inputs = existing.inputs.clone();
        cloned_inputs.insert(
            "offset".to_owned(),
            IrValue::Quantity(Quantity::length(offset_mm, LengthUnit::Mm).ok()?),
        );
        cloned_inputs.insert(FEATURE_SPEC_PORT.to_owned(), IrValue::String(spec_json));
        let IrValue::NodeRef { node_id, .. } = self.add_node(
            None,
            NodeKind::Sketch2D,
            IrType::Sketch,
            cloned_inputs,
            existing.metadata,
        ) else {
            return None;
        };
        Some(node_id)
    }

    fn sketch_value(&mut self, expr: &IrExpr) -> Result<IrValue> {
        match expr {
            IrExpr::Call {
                callee, args, span, ..
            } => {
                if !matches!(
                    callee.as_str(),
                    "point"
                        | "line"
                        | "arc"
                        | "circle"
                        | "ellipse"
                        | "spline"
                        | "rectangle"
                        | "region::closed"
                        | "region::path"
                        | "constraint::coincident"
                        | "constraint::horizontal"
                        | "constraint::vertical"
                        | "constraint::parallel"
                        | "constraint::perpendicular"
                        | "constraint::tangent"
                        | "constraint::distance"
                        | "constraint::radius"
                        | "constraint::fixed"
                        | "constraint::equal"
                ) {
                    return Err(at(
                        *span,
                        format!("unsupported sketch operation `{callee}`"),
                    ));
                }
                let args = self.ordered_args(args, Self::sketch_value)?;
                Ok(IrValue::Map(Inputs::from([
                    ("operation".to_owned(), IrValue::String(callee.clone())),
                    ("arguments".to_owned(), IrValue::Map(args)),
                ])))
            }
            IrExpr::Array { elements, .. } => elements
                .iter()
                .map(|expr| self.sketch_value(expr))
                .collect::<Result<Vec<_>>>()
                .map(IrValue::List),
            IrExpr::Pipe { .. } | IrExpr::MethodCall { .. } | IrExpr::Sketch { .. } => {
                Err(at(expr.span(), "unsupported expression in sketch"))
            }
            _ => self.value(expr, None),
        }
    }

    fn ordered_args(
        &mut self,
        args: &[IrArg],
        lower: impl Fn(&mut Self, &IrExpr) -> Result<IrValue>,
    ) -> Result<Inputs> {
        let mut inputs = Inputs::new();
        let mut positional = 0;
        for arg in args {
            let port = arg.name.clone().unwrap_or_else(|| {
                let port = positional.to_string();
                positional += 1;
                port
            });
            put(&mut inputs, &port, lower(self, &arg.value)?, arg.span)?;
        }
        Ok(inputs)
    }

    // Canonical v1 only permits connections on declared ports. For literal-only
    // options, store the nominal value in the input and the original bindings
    // as graph-value JSON in metadata so parametric options survive round-trip.
    fn literal_option(
        &self,
        port: &str,
        value: IrValue,
        metadata: &mut BTreeMap<String, String>,
        span: Span,
    ) -> Result<IrValue> {
        let literal = self.inline_parameters(&value, span)?;
        if literal != value {
            let bindings =
                serde_json::to_string(&value).map_err(|error| at(span, error.to_string()))?;
            metadata.insert(format!("dsl.bindings.{port}"), bindings);
        }
        Ok(literal)
    }

    fn inline_parameters(&self, value: &IrValue, span: Span) -> Result<IrValue> {
        match value {
            IrValue::NodeRef { .. } => {
                self.parameter(value)
                    .map(|p| p.value.clone())
                    .ok_or_else(|| {
                        at(
                            span,
                            "this CAD-DSL input cannot connect to a geometry or selector node",
                        )
                    })
            }
            IrValue::List(items) => items
                .iter()
                .map(|item| self.inline_parameters(item, span))
                .collect::<Result<Vec<_>>>()
                .map(IrValue::List),
            IrValue::Map(entries) => entries
                .iter()
                .map(|(key, entry)| Ok((key.clone(), self.inline_parameters(entry, span)?)))
                .collect::<Result<Inputs>>()
                .map(IrValue::Map),
            _ => Ok(value.clone()),
        }
    }

    /// Standalone `hole::*` calls are cutter solids, not cuts into a target.
    /// The cutter is the same +Z cylinder union the kernel builds, with
    /// parametric origins kept as formulas. A shape that cannot be expressed
    /// that way stays a `Hole` so the readiness gap remains honest.
    fn try_standalone_hole_tool(
        &mut self,
        callee: &str,
        name: Option<&str>,
        inputs: &Inputs,
        metadata: &BTreeMap<String, String>,
        span: Span,
    ) -> Result<Option<IrValue>> {
        let tool = match callee {
            "hole::simple" => self.simple_hole_tool(name, inputs, metadata),
            "hole::counterbore" => self.counterbore_hole_tool(name, inputs, metadata),
            "hole::tapped" => self.tapped_hole_tool(name, inputs, metadata, span)?,
            _ => None,
        };
        Ok(tool)
    }

    fn simple_hole_tool(
        &mut self,
        name: Option<&str>,
        inputs: &Inputs,
        metadata: &BTreeMap<String, String>,
    ) -> Option<IrValue> {
        let diameter = inputs.get("diameter")?.clone();
        let depth = inputs.get("depth")?.clone();
        if self.length_mm(&diameter)? <= 0.0 || self.length_mm(&depth)? <= 0.0 {
            return None;
        }
        let origin = inputs
            .get("origin")
            .cloned()
            .unwrap_or_else(|| IrValue::Vec3([0.0, 0.0, 0.0]));
        let (origin, axis_is_up) =
            self.downward_simple_pose(&origin, inputs.get("axis"), &depth)?;
        if !axis_is_up {
            return None;
        }
        let mut metadata = metadata.clone();
        metadata.insert("dsl.hole_tool".to_owned(), "simple".to_owned());
        Some(self.add_cylinder_tool(
            name,
            diameter,
            depth,
            origin,
            inputs,
            metadata,
            &["diameter", "depth", "origin", "axis"],
        ))
    }

    fn tapped_hole_tool(
        &mut self,
        name: Option<&str>,
        inputs: &Inputs,
        metadata: &BTreeMap<String, String>,
        span: Span,
    ) -> Result<Option<IrValue>> {
        let spec = match inputs.get("spec").or_else(|| inputs.get("thread")) {
            Some(IrValue::String(spec)) => spec.clone(),
            _ => return Ok(None),
        };
        let (drill, clean) = tap_drill_diameter_mm(&spec)
            .map_err(|err| at(span, format!("E209_INVALID_THREAD_SPEC: {err}")))?;
        let Some(diameter) = mm_value(drill) else {
            return Ok(None);
        };
        let Some(depth) = inputs.get("depth").cloned() else {
            return Ok(None);
        };
        if self.length_mm(&depth).is_none_or(|d| d <= 0.0) {
            return Ok(None);
        }
        let origin = inputs
            .get("origin")
            .cloned()
            .unwrap_or_else(|| IrValue::Vec3([0.0, 0.0, 0.0]));
        let Some((origin, axis_is_up)) =
            self.downward_tapped_pose(&origin, inputs.get("axis"), &depth)
        else {
            return Ok(None);
        };
        if !axis_is_up {
            return Ok(None);
        }
        let mut metadata = metadata.clone();
        metadata.insert("dsl.hole_tool".to_owned(), "tapped".to_owned());
        metadata.insert("dsl.thread_tag".to_owned(), format!("tapped_hole:{clean}"));
        Ok(Some(self.add_cylinder_tool(
            name,
            diameter,
            depth,
            origin,
            inputs,
            metadata,
            &["diameter", "depth", "origin", "axis"],
        )))
    }

    fn counterbore_hole_tool(
        &mut self,
        name: Option<&str>,
        inputs: &Inputs,
        metadata: &BTreeMap<String, String>,
    ) -> Option<IrValue> {
        let diameter = inputs.get("diameter")?.clone();
        let d_cbore = inputs.get("d_cbore")?.clone();
        let depth = inputs.get("depth")?.clone();
        let depth_cbore = inputs.get("depth_cbore")?.clone();
        let through_mm = self.length_mm(&depth)?;
        let cbore_mm = self.length_mm(&depth_cbore)?;
        let through_d = self.length_mm(&diameter)?;
        let cbore_d = self.length_mm(&d_cbore)?;
        if through_mm <= 0.0 || cbore_mm <= 0.0 || through_d <= 0.0 || cbore_d <= 0.0 {
            return None;
        }
        let origin = match inputs.get("origin") {
            Some(origin) => origin.clone(),
            None => self.default_counterbore_origin(&depth)?,
        };
        let mut metadata = metadata.clone();
        metadata.insert("dsl.hole_tool".to_owned(), "counterbore".to_owned());
        // OCCT lifts a counterbore as a cbore cylinder from the shelf up through
        // 1 mm past the entry face, plus a through cylinder only below that shelf.
        if through_mm <= cbore_mm + 1e-9 {
            let height = self.add_length_mm(&depth, COUNTERBORE_ENTRY_MM)?;
            let origin = self.offset_origin_z(&origin, &depth)?;
            return Some(self.add_cylinder_tool(
                name,
                d_cbore,
                height,
                origin,
                inputs,
                metadata,
                &["diameter", "depth", "origin", "d_cbore", "depth_cbore"],
            ));
        }
        let through_height = self.sub_length(&depth, &depth_cbore)?;
        let cbore_height = self.add_length_mm(&depth_cbore, COUNTERBORE_ENTRY_MM)?;
        let through_origin = self.offset_origin_z(&origin, &depth)?;
        let cbore_origin = self.offset_origin_z(&origin, &depth_cbore)?;
        let through = self.add_cylinder_tool(
            None,
            diameter,
            through_height,
            through_origin,
            inputs,
            BTreeMap::new(),
            &[
                "diameter",
                "depth",
                "origin",
                "d_cbore",
                "depth_cbore",
                "hole_type",
                "spec",
                "thread",
            ],
        );
        let cbore = self.add_cylinder_tool(
            None,
            d_cbore,
            cbore_height,
            cbore_origin,
            inputs,
            BTreeMap::new(),
            &[
                "diameter",
                "depth",
                "origin",
                "d_cbore",
                "depth_cbore",
                "hole_type",
                "spec",
                "thread",
            ],
        );
        let mut union_inputs = Inputs::new();
        union_inputs.insert("solids".to_owned(), IrValue::List(vec![through, cbore]));
        copy_ports(
            inputs,
            &mut union_inputs,
            &[
                "diameter",
                "depth",
                "origin",
                "d_cbore",
                "depth_cbore",
                "axis",
            ],
        );
        let union_raw = union_inputs.clone();
        self.attach_feature_spec(&NodeKind::BooleanUnion, &union_raw, &mut union_inputs);
        Some(self.add_node(
            name,
            NodeKind::BooleanUnion,
            IrType::Solid,
            union_inputs,
            metadata,
        ))
    }

    /// `hole::simple` rewrites any axis with z < -0.5 into a +Z cylinder whose
    /// base is `origin.z - depth`. Other axes are not legacy +Z cylinders.
    fn downward_simple_pose(
        &mut self,
        origin: &IrValue,
        axis: Option<&IrValue>,
        depth: &IrValue,
    ) -> Option<(IrValue, bool)> {
        if axis.is_some_and(|axis| self.parameter(axis).is_some()) {
            return None;
        }
        let axis = axis_nominal(self, axis).unwrap_or([0.0, 0.0, -1.0]);
        if axis[2] < -0.5 {
            return Some((self.offset_origin_z(origin, depth)?, true));
        }
        Some((origin.clone(), is_pos_z(axis)))
    }

    /// Tapped tools flip only a vertical negative axis. `+Z` keeps the entry origin.
    fn downward_tapped_pose(
        &mut self,
        origin: &IrValue,
        axis: Option<&IrValue>,
        depth: &IrValue,
    ) -> Option<(IrValue, bool)> {
        if axis.is_some_and(|axis| self.parameter(axis).is_some()) {
            return None;
        }
        let axis = axis_nominal(self, axis).unwrap_or([0.0, 0.0, -1.0]);
        if axis[0].abs() < 1e-9 && axis[1].abs() < 1e-9 && axis[2] < 0.0 {
            return Some((self.offset_origin_z(origin, depth)?, true));
        }
        Some((origin.clone(), is_pos_z(axis)))
    }

    fn default_counterbore_origin(&mut self, depth: &IrValue) -> Option<IrValue> {
        let z = self.length_mm(depth)?;
        if self.parameter(depth).is_none() {
            return Some(IrValue::Vec3([0.0, 0.0, z]));
        }
        let formula = format!("[0mm, 0mm, {}]", self.length_atom(depth)?);
        let id = self.generated_id();
        Some(self.add_parameter(
            id,
            IrType::Vec3Length,
            IrValue::Vec3([0.0, 0.0, z]),
            Some(formula),
        ))
    }

    fn add_cylinder_tool(
        &mut self,
        name: Option<&str>,
        diameter: IrValue,
        height: IrValue,
        origin: IrValue,
        source: &Inputs,
        metadata: BTreeMap<String, String>,
        consumed: &[&str],
    ) -> IrValue {
        let mut inputs = Inputs::new();
        inputs.insert("diameter".to_owned(), diameter);
        inputs.insert("height".to_owned(), height);
        inputs.insert("origin".to_owned(), origin);
        inputs.insert("axis".to_owned(), IrValue::Vec3([0.0, 0.0, 1.0]));
        copy_ports(source, &mut inputs, consumed);
        let raw_copy = inputs.clone();
        self.attach_feature_spec(&NodeKind::PrimitiveCylinder, &raw_copy, &mut inputs);
        self.add_node(
            name,
            NodeKind::PrimitiveCylinder,
            IrType::Solid,
            inputs,
            metadata,
        )
    }

    fn add_length_mm(&mut self, value: &IrValue, extra_mm: f64) -> Option<IrValue> {
        let sum = self.length_mm(value)? + extra_mm;
        let nominal = Quantity::length(sum, LengthUnit::Mm).ok()?;
        if self.parameter(value).is_none() {
            return Some(IrValue::Quantity(nominal));
        }
        let extra = Quantity::length(extra_mm, LengthUnit::Mm).ok()?;
        let formula = format!(
            "({} + {})",
            self.length_atom(value)?,
            crate::model::format_quantity(extra)
        );
        let id = self.generated_id();
        Some(self.add_parameter(
            id,
            IrType::Length,
            IrValue::Quantity(nominal),
            Some(formula),
        ))
    }

    fn sub_length(&mut self, left: &IrValue, right: &IrValue) -> Option<IrValue> {
        let result = self.length_mm(left)? - self.length_mm(right)?;
        let nominal = Quantity::length(result, LengthUnit::Mm).ok()?;
        if self.parameter(left).is_none() && self.parameter(right).is_none() {
            return Some(IrValue::Quantity(nominal));
        }
        let formula = format!(
            "({} - {})",
            self.length_atom(left)?,
            self.length_atom(right)?
        );
        let id = self.generated_id();
        Some(self.add_parameter(
            id,
            IrType::Length,
            IrValue::Quantity(nominal),
            Some(formula),
        ))
    }

    fn offset_origin_z(&mut self, origin: &IrValue, delta: &IrValue) -> Option<IrValue> {
        let mut point = origin_vec3(self, origin)?;
        let delta_mm = self.length_mm(delta)?;
        point[2] -= delta_mm;
        if !point.iter().all(|coord| coord.is_finite()) {
            return None;
        }
        if self.parameter(origin).is_none() && self.parameter(delta).is_none() {
            return Some(IrValue::Vec3(point));
        }
        let atoms = self.vec3_length_atoms(origin)?;
        let formula = format!(
            "[{}, {}, ({} - {})]",
            atoms[0],
            atoms[1],
            atoms[2],
            self.length_atom(delta)?
        );
        let id = self.generated_id();
        Some(self.add_parameter(id, IrType::Vec3Length, IrValue::Vec3(point), Some(formula)))
    }

    fn length_mm(&self, value: &IrValue) -> Option<f64> {
        match self.nominal(value) {
            IrValue::Quantity(q) if q.ir_type() == IrType::Length => Some(q.canonical_value()),
            IrValue::Scalar(x) => Some(*x),
            IrValue::Int(x) => Some(*x as f64),
            _ => None,
        }
    }

    fn length_atom(&self, value: &IrValue) -> Option<String> {
        if let Some(parameter) = self.parameter(value) {
            return Some(super::expressions::parameter_symbol(&parameter.name.0));
        }
        match value {
            IrValue::Quantity(q) if q.ir_type() == IrType::Length => {
                Some(crate::model::format_quantity(*q))
            }
            IrValue::Scalar(x) => Some(format!("{}mm", crate::model::format_f64(*x))),
            IrValue::Int(x) => Some(format!("{x}mm")),
            _ => None,
        }
    }

    fn vec3_length_atoms(&self, origin: &IrValue) -> Option<[String; 3]> {
        if let Some(parameter) = self.parameter(origin) {
            return split_vec3_formula(parameter.formula.as_deref()?);
        }
        let IrValue::Vec3(point) = origin else {
            return None;
        };
        Some(point.map(|coord| format!("{}mm", crate::model::format_f64(coord))))
    }

    fn component_feature_spec(&self, inputs: &Inputs) -> Option<FeatureSpec> {
        let source = if let Some(IrValue::ExternalRef(ref_id)) = inputs.get("external_ref") {
            ComponentSourceSpec::ExternalRef {
                ref_id: ref_id.clone(),
            }
        } else {
            let IrValue::NodeRef { node_id, .. } = inputs.get("geometry")? else {
                return None;
            };
            ComponentSourceSpec::LocalNode {
                node_id: node_id.clone(),
            }
        };
        let translation_mm = match inputs.get("origin") {
            Some(origin) => origin_vec3(self, origin)?,
            None => [0.0, 0.0, 0.0],
        };
        let rotation_deg = match inputs.get("rotation") {
            Some(rotation) => origin_vec3(self, rotation)?,
            None => [0.0, 0.0, 0.0],
        };
        let grounded = matches!(inputs.get("grounded"), Some(IrValue::Bool(true)));
        Some(FeatureSpec::ComponentInstance {
            source,
            translation_mm,
            rotation_deg,
            grounded,
        })
    }

    fn try_fold_selector(
        &mut self,
        method: &str,
        args: &[IrArg],
        receiver: Option<&IrValue>,
        span: Span,
    ) -> Result<Option<IrValue>> {
        let Some(receiver) = receiver else {
            return Ok(None);
        };
        let (root, mut query) = if matches!(method, "faces" | "edges") {
            if self.parameter(receiver).is_some() || !matches!(receiver, IrValue::NodeRef { .. }) {
                return Ok(None);
            }
            if !args.is_empty() {
                return Ok(None);
            }
            (
                receiver.clone(),
                QueryTopoRef {
                    entity_kind: if method == "faces" {
                        EntityKind::Face
                    } else {
                        EntityKind::Edge
                    },
                    filters: Vec::new(),
                    tie_breakers: Vec::new(),
                    require_unique: false,
                },
            )
        } else if let Some((root, query)) = decode_topo_query(receiver) {
            (root, query)
        } else {
            return Ok(None);
        };
        if !self.extend_topo_query(method, args, &mut query, span)? {
            return Ok(None);
        }
        let expr = serde_json::to_string(&TopoRef::Query(query))
            .map_err(|error| at(span, error.to_string()))?;
        Ok(Some(topo_query_value(root, expr)))
    }

    fn extend_topo_query(
        &mut self,
        method: &str,
        args: &[IrArg],
        query: &mut QueryTopoRef,
        span: Span,
    ) -> Result<bool> {
        match method {
            "faces" | "edges" => Ok(true),
            "planar" | "cylindrical" | "conical" if args.is_empty() => {
                let class = match method {
                    "planar" => SurfaceClass::Plane,
                    "cylindrical" => SurfaceClass::Cylinder,
                    "conical" => SurfaceClass::Cone,
                    _ => unreachable!(),
                };
                query.filters.push(TopoFilter::Surface(class));
                Ok(true)
            }
            "filter" => {
                let Some(normal) = named_or_only(args, "normal") else {
                    return Ok(false);
                };
                let value = self.value(&normal.value, None)?;
                let Some(dir) = normal_dir(self.nominal(&value)) else {
                    return Ok(false);
                };
                query.filters.push(TopoFilter::Normal {
                    dir,
                    angle_tol_deg: 1e-6,
                });
                Ok(true)
            }
            "tagged" => {
                let Some(arg) = args.first() else {
                    return Ok(false);
                };
                if args.len() != 1 {
                    return Ok(false);
                }
                let IrValue::String(tag) = self.value(&arg.value, None)? else {
                    return Ok(false);
                };
                if tag.trim().is_empty() {
                    return Ok(false);
                }
                query.filters.push(TopoFilter::Tagged(tag));
                Ok(true)
            }
            "max_by" | "min_by" => {
                let Some(coord) = named_or_only(args, "coord") else {
                    return Ok(false);
                };
                let breaker = match &coord.value {
                    IrExpr::ConstDir { name, .. } | IrExpr::Var { name, .. }
                        if name == "radius" =>
                    {
                        if method == "max_by" {
                            TieBreaker::MaxByRadius
                        } else {
                            TieBreaker::MinByRadius
                        }
                    }
                    IrExpr::ConstDir { name, .. } | IrExpr::Var { name, .. }
                        if name == "area" || name == "length" =>
                    {
                        if method == "max_by" {
                            TieBreaker::MaxByAreaOrLength
                        } else {
                            TieBreaker::MinByAreaOrLength
                        }
                    }
                    _ => {
                        let value = self.value(&coord.value, None)?;
                        let Some(axis) = coord_axis(self.nominal(&value)) else {
                            return Ok(false);
                        };
                        if method == "max_by" {
                            TieBreaker::MaxByCoord(axis)
                        } else {
                            TieBreaker::MinByCoord(axis)
                        }
                    }
                };
                query.tie_breakers.push(breaker);
                query.require_unique = true;
                Ok(true)
            }
            "nearest_to" => {
                let Some(arg) = named_or_only(args, "point").or_else(|| args.first()) else {
                    return Ok(false);
                };
                if args.len() != 1 {
                    return Ok(false);
                }
                let value = self.value(&arg.value, None)?;
                let Some(point) = origin_vec3(self, &value) else {
                    return Ok(false);
                };
                query.tie_breakers.push(TieBreaker::NearestTo(point));
                query.require_unique = true;
                Ok(true)
            }
            _ => {
                let _ = span;
                Ok(false)
            }
        }
    }

    fn insert_all_face_query(&mut self, inputs: &mut Inputs) -> Result<()> {
        let Some(root) = inputs.get("geometry").cloned() else {
            return Ok(());
        };
        if !matches!(root, IrValue::NodeRef { .. }) || self.parameter(&root).is_some() {
            return Ok(());
        }
        let query = QueryTopoRef {
            entity_kind: EntityKind::Face,
            filters: Vec::new(),
            tie_breakers: Vec::new(),
            require_unique: false,
        };
        let expr = serde_json::to_string(&TopoRef::Query(query))
            .map_err(|error| at(Span::default(), error.to_string()))?;
        inputs.insert(
            "faces".to_owned(),
            IrValue::List(vec![topo_query_value(root, expr)]),
        );
        Ok(())
    }

    fn project_topo_ports(&self, kind: &NodeKind, inputs: &mut Inputs) {
        let (solid_port, ports): (&str, &[&str]) = match kind {
            NodeKind::Shell => ("solid", &["open_faces"]),
            NodeKind::Fillet | NodeKind::Chamfer => ("solid", &["edges"]),
            NodeKind::TagFaces => ("geometry", &["faces"]),
            _ => return,
        };
        let solid = inputs.get(solid_port).cloned();
        for port in ports {
            let Some(IrValue::List(items)) = inputs.get(*port).cloned() else {
                continue;
            };
            let projected = items
                .into_iter()
                .map(|item| {
                    let out = project_query_item(solid.as_ref(), item);
                    if matches!(kind, NodeKind::TagFaces)
                        && let IrValue::Map(ref entries) = out
                        && entries.get(TOPO_QUERY_MARK) == Some(&IrValue::Bool(true))
                    {
                        IrValue::TopoRefExpr("__foreign_solid_selector__".to_owned())
                    } else {
                        out
                    }
                })
                .collect();
            inputs.insert((*port).to_owned(), IrValue::List(projected));
        }
    }
}

fn canonical_port<'a>(kind: &NodeKind, port: &'a str) -> &'a str {
    use NodeKind::*;
    match (kind, port) {
        (PrimitiveCylinder | Hole, "d") | (Hole, "d_through") => "diameter",
        (PrimitiveCylinder | Extrude, "h") => "height",
        (Hole, "depth_through") => "depth",
        (Fillet, "r") => "radius",
        (Chamfer, "d") => "distance",
        (Extrude | Revolve, "sketch") => "profile",
        (PatternLinear, "axis") => "direction",
        (PatternLinear | PatternPolar, "feature") => "solid",
        (Sweep, "sketch") => "profile",
        (ImplicitSmoothBlend, "items") => "solids",
        (ImplicitSmoothBlend, "blend_radius") => "radius",
        (
            Shell | Fillet | Chamfer | PatternLinear | PatternPolar | Hole,
            "base" | "target" | "body",
        ) => "solid",
        (TagFaces, "solid" | "surface") => "geometry",
        _ => port,
    }
}

fn insert_primitive_defaults(kind: &NodeKind, inputs: &mut Inputs) {
    let length = |value| {
        IrValue::Quantity(Quantity::Length {
            value,
            unit: LengthUnit::Mm,
        })
    };
    match kind {
        NodeKind::PrimitiveBox => {
            for port in ["width", "height"] {
                inputs
                    .entry(port.to_owned())
                    .or_insert_with(|| length(10.0));
            }
            if !inputs.contains_key("length") {
                inputs
                    .entry("depth".to_owned())
                    .or_insert_with(|| length(10.0));
            }
        }
        NodeKind::PrimitiveCylinder => {
            inputs
                .entry("height".to_owned())
                .or_insert_with(|| length(10.0));
            if !inputs.contains_key("radius") {
                inputs
                    .entry("diameter".to_owned())
                    .or_insert_with(|| length(10.0));
            }
        }
        _ => {}
    }
}

// Connection-capable ports from the canonical v1 contract, restricted to the
// node kinds produced here. Other inputs are literal operator options.
fn connected_port(kind: &NodeKind, port: &str) -> bool {
    use NodeKind::*;
    matches!(
        (kind, port),
        (
            PrimitiveBox,
            "width" | "depth" | "height" | "length" | "origin"
        ) | (
            PrimitiveCylinder,
            "diameter" | "radius" | "height" | "origin"
        ) | (
            Extrude,
            "profile" | "height" | "depth" | "distance" | "thickness" | "direction"
        ) | (Revolve, "profile" | "axis" | "angle")
            | (
                BooleanUnion | BooleanSubtract,
                "base" | "target" | "left" | "right" | "tool" | "a" | "b" | "solids" | "tools"
            )
            | (Fillet, "solid" | "radius" | "edges")
            | (Chamfer, "solid" | "distance" | "edges")
            | (Shell, "solid" | "thickness" | "open_faces")
            | (
                Hole,
                "solid" | "diameter" | "radius" | "depth" | "origin" | "axis"
            )
            | (
                PatternLinear,
                "solid" | "direction" | "count" | "spacing" | "step"
            )
            | (PatternPolar, "solid" | "axis" | "count" | "angle")
            | (TagFaces, "geometry" | "faces")
            | (Mirror, "solid")
            | (Sweep, "profile" | "sketch" | "path")
            | (LoftGuided, "sections")
            | (
                ImplicitSmoothBlend,
                "solids" | "left" | "right" | "a" | "b" | "radius"
            )
            | (AssemblyComponent, "geometry" | "external_ref")
            | (AssemblyMate, "left" | "right")
    )
}

fn put(inputs: &mut Inputs, key: &str, value: IrValue, span: Span) -> Result<()> {
    if inputs.insert(key.to_owned(), value).is_some() {
        return Err(at(span, format!("duplicate input `{key}`")));
    }
    Ok(())
}

fn topology_list(value: IrValue, port: &str, span: Span) -> Result<IrValue> {
    let items = match value {
        IrValue::List(items) => items,
        item => vec![item],
    };
    items
        .into_iter()
        .map(|item| match item {
            IrValue::String(text) => Ok(IrValue::TopoRefExpr(text)),
            IrValue::Vec3(normal) if port == "open_faces" => face_query(
                vec![
                    TopoFilter::Surface(SurfaceClass::Plane),
                    TopoFilter::Normal {
                        dir: AxisDir::Custom(normal),
                        angle_tol_deg: 1e-6,
                    },
                ],
                span,
            ),
            _ => Ok(item),
        })
        .collect::<Result<Vec<_>>>()
        .map(IrValue::List)
}

fn face_query(filters: Vec<TopoFilter>, span: Span) -> Result<IrValue> {
    let query = TopoRef::Query(QueryTopoRef {
        entity_kind: EntityKind::Face,
        filters,
        tie_breakers: Vec::new(),
        require_unique: false,
    });
    let text = serde_json::to_string(&query).map_err(|error| at(span, error.to_string()))?;
    Ok(IrValue::TopoRefExpr(text))
}

const COUNTERBORE_ENTRY_MM: f64 = 1.0;
const TOPO_QUERY_MARK: &str = "dsl_topo_query";

fn mm_value(value: f64) -> Option<IrValue> {
    Quantity::length(value, LengthUnit::Mm)
        .ok()
        .map(IrValue::Quantity)
}

fn is_pos_z(axis: [f64; 3]) -> bool {
    axis[0].abs() <= 1e-9 && axis[1].abs() <= 1e-9 && (axis[2] - 1.0).abs() <= 1e-9
}

fn axis_nominal(lower: &Lowering, axis: Option<&IrValue>) -> Option<[f64; 3]> {
    origin_vec3(lower, axis?)
}

fn origin_vec3(lower: &Lowering, value: &IrValue) -> Option<[f64; 3]> {
    match lower.nominal(value) {
        IrValue::Vec3(point) => Some(*point),
        _ => None,
    }
}

fn copy_ports(source: &Inputs, dest: &mut Inputs, consumed: &[&str]) {
    for (key, value) in source {
        if consumed.contains(&key.as_str()) || dest.contains_key(key) {
            continue;
        }
        dest.insert(key.clone(), value.clone());
    }
}

fn split_vec3_formula(formula: &str) -> Option<[String; 3]> {
    let text = formula.trim();
    let inner = text.strip_prefix('[')?.strip_suffix(']')?;
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quoted = false;
    let mut start = 0usize;
    for (index, ch) in inner.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            '(' | '[' if !quoted => depth += 1,
            ')' | ']' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => {
                parts.push(inner[start..index].trim().to_owned());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(inner[start..].trim().to_owned());
    let [x, y, z] = parts.try_into().ok()?;
    if x.is_empty() || y.is_empty() || z.is_empty() {
        return None;
    }
    Some([x, y, z])
}

fn topo_query_value(root: IrValue, expr: String) -> IrValue {
    IrValue::Map(Inputs::from([
        (TOPO_QUERY_MARK.to_owned(), IrValue::Bool(true)),
        ("root".to_owned(), root),
        ("expr".to_owned(), IrValue::TopoRefExpr(expr)),
    ]))
}

fn decode_topo_query(value: &IrValue) -> Option<(IrValue, QueryTopoRef)> {
    let IrValue::Map(entries) = value else {
        return None;
    };
    if entries.get(TOPO_QUERY_MARK) != Some(&IrValue::Bool(true)) {
        return None;
    }
    let root = entries.get("root")?.clone();
    let IrValue::TopoRefExpr(expr) = entries.get("expr")? else {
        return None;
    };
    let TopoRef::Query(query) = serde_json::from_str(expr).ok()? else {
        return None;
    };
    Some((root, query))
}

fn project_query_item(solid: Option<&IrValue>, item: IrValue) -> IrValue {
    let IrValue::Map(entries) = &item else {
        return item;
    };
    if entries.get(TOPO_QUERY_MARK) != Some(&IrValue::Bool(true)) {
        return item;
    }
    let Some(root) = entries.get("root") else {
        return item;
    };
    if !query_root_matches(solid, root) {
        return item;
    }
    let Some(IrValue::TopoRefExpr(expr)) = entries.get("expr") else {
        return item;
    };
    IrValue::TopoRefExpr(expr.clone())
}

fn query_root_matches(solid: Option<&IrValue>, root: &IrValue) -> bool {
    match (solid, root) {
        (Some(IrValue::NodeRef { node_id: solid, .. }), IrValue::NodeRef { node_id: root, .. }) => {
            solid == root
        }
        _ => false,
    }
}

fn named_or_only<'a>(args: &'a [IrArg], name: &str) -> Option<&'a IrArg> {
    if let Some(arg) = args.iter().find(|arg| arg.name.as_deref() == Some(name)) {
        return Some(arg);
    }
    if args.len() == 1 && args[0].name.is_none() {
        return Some(&args[0]);
    }
    None
}

fn close_axis(value: [f64; 3], axis: [f64; 3]) -> bool {
    value
        .iter()
        .zip(axis)
        .all(|(coord, expected)| (coord - expected).abs() <= 1e-9)
}

fn normal_dir(value: &IrValue) -> Option<AxisDir> {
    let IrValue::Vec3(dir) = value else {
        return None;
    };
    if !dir.iter().all(|coord| coord.is_finite()) {
        return None;
    }
    let known = [
        ([1.0, 0.0, 0.0], AxisDir::PosX),
        ([-1.0, 0.0, 0.0], AxisDir::NegX),
        ([0.0, 1.0, 0.0], AxisDir::PosY),
        ([0.0, -1.0, 0.0], AxisDir::NegY),
        ([0.0, 0.0, 1.0], AxisDir::PosZ),
        ([0.0, 0.0, -1.0], AxisDir::NegZ),
    ];
    for (axis, dir_kind) in known {
        if close_axis(*dir, axis) {
            return Some(dir_kind);
        }
    }
    let length_sq = dir.iter().map(|coord| coord * coord).sum::<f64>();
    if length_sq <= 1e-12 {
        return None;
    }
    Some(AxisDir::Custom(*dir))
}

fn coord_axis(value: &IrValue) -> Option<CoordAxis> {
    match value {
        IrValue::Vec3(dir) if close_axis(*dir, [1.0, 0.0, 0.0]) => Some(CoordAxis::X),
        IrValue::Vec3(dir) if close_axis(*dir, [0.0, 1.0, 0.0]) => Some(CoordAxis::Y),
        IrValue::Vec3(dir) if close_axis(*dir, [0.0, 0.0, 1.0]) => Some(CoordAxis::Z),
        IrValue::String(text) => match text.trim().to_ascii_uppercase().as_str() {
            "X" => Some(CoordAxis::X),
            "Y" => Some(CoordAxis::Y),
            "Z" => Some(CoordAxis::Z),
            _ => None,
        },
        _ => None,
    }
}

/// Coarse metric tap drills matching `cad-kernel-occt` `tap_drill_diameter_mm`.
/// The bridge cannot depend on the kernel crate. `M4x0.7-6H` is drill 3.3 mm.
fn tap_drill_diameter_mm(spec: &str) -> std::result::Result<(f64, String), String> {
    let invalid = || format!("invalid or unsupported metric thread specification `{spec}`");
    let clean = spec.trim();
    let upper = clean.to_ascii_uppercase();
    let body = upper.strip_prefix('M').ok_or_else(invalid)?;
    let (body, class) = body
        .split_once('-')
        .map_or((body, None), |(body, class)| (body, Some(class)));
    if let Some(class) = class {
        let digits = class.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !matches!(&class[digits..], "G" | "H") {
            return Err(invalid());
        }
    }
    let (nominal, pitch) = body
        .split_once('X')
        .map_or((body, None), |(nominal, pitch)| (nominal, Some(pitch)));
    let number = |value: &str| {
        if value
            .bytes()
            .any(|byte| !(byte.is_ascii_digit() || byte == b'.'))
        {
            return Err(invalid());
        }
        value.parse::<f64>().map_err(|_| invalid())
    };
    let nominal = number(nominal)?;
    let coarse = [
        (2.0, 0.4, 1.6),
        (2.5, 0.45, 2.05),
        (3.0, 0.5, 2.5),
        (3.5, 0.6, 2.9),
        (4.0, 0.7, 3.3),
        (5.0, 0.8, 4.2),
        (6.0, 1.0, 5.0),
        (8.0, 1.25, 6.8),
        (10.0, 1.5, 8.5),
        (12.0, 1.75, 10.2),
        (14.0, 2.0, 12.0),
        (16.0, 2.0, 14.0),
        (18.0, 2.5, 15.5),
        (20.0, 2.5, 17.5),
    ];
    let (_, coarse_pitch, coarse_drill) = coarse
        .into_iter()
        .find(|&(size, _, _)| (size - nominal).abs() < 1e-6)
        .ok_or_else(invalid)?;
    let drill = match pitch.map(number).transpose()? {
        None => coarse_drill,
        Some(pitch) if (pitch - coarse_pitch).abs() < 1e-6 => coarse_drill,
        Some(pitch) if pitch > 0.0 && pitch < coarse_pitch => nominal - pitch,
        _ => return Err(invalid()),
    };
    Ok((drill, clean.to_owned()))
}

fn sample_helix_3d(
    radius: f64,
    pitch: f64,
    turns: f64,
    origin: [f64; 3],
    segments_per_turn: usize,
    left_handed: bool,
) -> std::result::Result<Vec<[f64; 3]>, String> {
    if !radius.is_finite() || radius <= 0.0 {
        return Err(format!(
            "path::helix requires positive `radius`, found {radius}"
        ));
    }
    if !pitch.is_finite() || pitch <= 0.0 {
        return Err(format!(
            "path::helix requires positive `pitch`, found {pitch}"
        ));
    }
    if !turns.is_finite() || turns <= 0.0 {
        return Err(format!(
            "path::helix requires positive `turns`, found {turns}"
        ));
    }
    if origin.iter().any(|v| !v.is_finite()) {
        return Err("path::helix `origin` must have finite coordinates".to_owned());
    }
    let segs_per_turn = segments_per_turn.clamp(8, 96);
    let total_segments = ((turns * segs_per_turn as f64).ceil() as usize).clamp(8, 2048);
    let sign = if left_handed { -1.0 } else { 1.0 };
    let total_theta = turns * std::f64::consts::TAU;
    let [ox, oy, oz] = origin;

    let mut pts = Vec::with_capacity(total_segments + 1);
    for i in 0..=total_segments {
        let frac = (i as f64) / (total_segments as f64);
        let theta = frac * total_theta;
        pts.push([
            ox + radius * (sign * theta).cos(),
            oy + radius * (sign * theta).sin(),
            oz + pitch * (theta / std::f64::consts::TAU),
        ]);
    }
    if pts.windows(2).any(|pair| {
        let d = [
            pair[0][0] - pair[1][0],
            pair[0][1] - pair[1][1],
            pair[0][2] - pair[1][2],
        ];
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2] <= 1e-12
    }) {
        return Err("path::helix generated degenerate consecutive points".to_owned());
    }
    Ok(pts)
}

fn sample_spline3d(
    control_points: &[[f64; 3]],
    segments_per_span: usize,
) -> std::result::Result<Vec<[f64; 3]>, String> {
    if control_points.len() < 3 {
        return Err(format!(
            "path::spline3d requires at least 3 control points, found {}",
            control_points.len()
        ));
    }
    if control_points.iter().flatten().any(|c| !c.is_finite()) {
        return Err("path::spline3d control points must have finite coordinates".to_owned());
    }
    let dist3 = |a: [f64; 3], b: [f64; 3]| -> f64 {
        let dx = a[0] - b[0];
        let dy = a[1] - b[1];
        let dz = a[2] - b[2];
        (dx * dx + dy * dy + dz * dz).sqrt()
    };
    let mut cleaned: Vec<[f64; 3]> = Vec::with_capacity(control_points.len());
    for &pt in control_points {
        if cleaned.last().is_none_or(|&prev| dist3(pt, prev) > 1e-5) {
            cleaned.push(pt);
        }
    }
    if cleaned.len() < 3 {
        return Err(format!(
            "path::spline3d requires at least 3 distinct control points, found {}",
            cleaned.len()
        ));
    }
    let segs = segments_per_span.clamp(4, 64);
    let n = cleaned.len();
    let mut sampled: Vec<[f64; 3]> = Vec::with_capacity((n - 1) * segs + 1);
    for i in 0..(n - 1) {
        let p_prev = if i == 0 { cleaned[0] } else { cleaned[i - 1] };
        let p_curr = cleaned[i];
        let p_next = cleaned[i + 1];
        let p_next2 = if i + 2 >= n {
            cleaned[n - 1]
        } else {
            cleaned[i + 2]
        };
        for step in 0..segs {
            let t = (step as f64) / (segs as f64);
            let t2 = t * t;
            let t3 = t2 * t;
            let pt: [f64; 3] = std::array::from_fn(|k| {
                0.5 * ((2.0 * p_curr[k])
                    + (-p_prev[k] + p_next[k]) * t
                    + (2.0 * p_prev[k] - 5.0 * p_curr[k] + 4.0 * p_next[k] - p_next2[k]) * t2
                    + (-p_prev[k] + 3.0 * p_curr[k] - 3.0 * p_next[k] + p_next2[k]) * t3)
            });
            if sampled.last().is_none_or(|&prev| dist3(pt, prev) > 1e-5) {
                sampled.push(pt);
            }
        }
    }
    let end_pt = cleaned[n - 1];
    if sampled
        .last()
        .is_none_or(|&prev| dist3(end_pt, prev) > 1e-5)
    {
        sampled.push(end_pt);
    } else if let Some(last) = sampled.last_mut() {
        *last = end_pt;
    }
    let total_len: f64 = sampled.windows(2).map(|w| dist3(w[0], w[1])).sum();
    if sampled.len() < 2 || !total_len.is_finite() || total_len <= 1e-3 {
        return Err(format!(
            "path::spline3d total path length must exceed 1e-3 mm, found {total_len}"
        ));
    }
    Ok(sampled)
}
