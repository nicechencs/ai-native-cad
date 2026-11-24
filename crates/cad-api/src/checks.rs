//! Preconditions checked before a command is turned into patch operations.

use cad_ir::{
    CanonicalGraph, DimInput, IntInput, IrNode, IrType, IrValue, NodeId, NodeKind, ParamId,
    ParameterDef, Quantity, SketchConstraint2D, SketchDimValue, SketchEntity2D, StructuredSketch2D,
};

use crate::error::SemanticApiError;
use crate::walk::CONTRACT_NODE_ID;

pub(crate) fn require_name(name: &str, what: &str) -> Result<(), SemanticApiError> {
    if name.trim().is_empty() {
        Err(SemanticApiError::invalid(format!(
            "{what} name cannot be empty"
        )))
    } else {
        Ok(())
    }
}

pub(crate) fn require_node<'a>(
    graph: &'a CanonicalGraph,
    id: &NodeId,
) -> Result<&'a IrNode, SemanticApiError> {
    graph
        .nodes
        .get(&id.0)
        .ok_or_else(|| SemanticApiError::not_found(format!("node `{id}` was not found")))
}

pub(crate) fn require_node_type<'a>(
    graph: &'a CanonicalGraph,
    id: &NodeId,
    expected: &[IrType],
    role: &str,
) -> Result<&'a IrNode, SemanticApiError> {
    let node = require_node(graph, id)?;
    if !expected.contains(&node.output_type) {
        return Err(SemanticApiError::invalid(format!(
            "{role} `{id}` has output {:?}; expected {expected:?}",
            node.output_type
        )));
    }
    Ok(node)
}

pub(crate) fn require_param<'a>(
    graph: &'a CanonicalGraph,
    id: &ParamId,
) -> Result<&'a ParameterDef, SemanticApiError> {
    if id.0.trim().is_empty() {
        return Err(SemanticApiError::invalid("parameter name cannot be empty"));
    }
    graph
        .parameters
        .get(&id.0)
        .ok_or_else(|| SemanticApiError::not_found(format!("parameter `{}` was not found", id.0)))
}

pub(crate) fn require_param_type<'a>(
    graph: &'a CanonicalGraph,
    id: &ParamId,
    expected: IrType,
    field: &str,
) -> Result<&'a ParameterDef, SemanticApiError> {
    let def = require_param(graph, id)?;
    if def.ir_type != expected {
        let message = format!(
            "{field} expects {expected:?}, but parameter `{}` is {:?}",
            id.0, def.ir_type
        );
        if is_quantity_type(expected) && is_quantity_type(def.ir_type) {
            return Err(SemanticApiError::unit_mismatch(message));
        }
        return Err(SemanticApiError::invalid(message));
    }
    Ok(def)
}

fn is_quantity_type(ty: IrType) -> bool {
    matches!(
        ty,
        IrType::Length
            | IrType::Angle
            | IrType::Force
            | IrType::Pressure
            | IrType::Mass
            | IrType::Density
    )
}

pub(crate) fn require_dim(
    graph: &CanonicalGraph,
    dim: &DimInput,
    expected: IrType,
    field: &str,
) -> Result<(), SemanticApiError> {
    match dim {
        DimInput::Literal(quantity) => check_quantity(quantity, expected, field),
        DimInput::Param { param_id } => {
            require_param_type(graph, param_id, expected, field)?;
            Ok(())
        }
        DimInput::NodeOutput { node_id, .. } => {
            require_node_type(graph, node_id, &[expected], field)?;
            Ok(())
        }
    }
}

fn check_quantity(
    quantity: &Quantity,
    expected: IrType,
    field: &str,
) -> Result<(), SemanticApiError> {
    if !quantity.canonical_value().is_finite() {
        return Err(SemanticApiError::invalid(format!(
            "{field} must be a finite number"
        )));
    }
    if quantity.ir_type() != expected {
        let message = format!("{field} expects {expected:?}, got {:?}", quantity.ir_type());
        if is_quantity_type(expected) && is_quantity_type(quantity.ir_type()) {
            return Err(SemanticApiError::unit_mismatch(message));
        }
        return Err(SemanticApiError::invalid(message));
    }
    Ok(())
}

pub(crate) fn require_int(
    graph: &CanonicalGraph,
    input: &IntInput,
    field: &str,
) -> Result<(), SemanticApiError> {
    match input {
        IntInput::Literal { value } => {
            if *value < 1 {
                return Err(SemanticApiError::invalid(format!(
                    "{field} count must be at least 1"
                )));
            }
            Ok(())
        }
        IntInput::Param { param_id } => {
            require_param_type(graph, param_id, IrType::Int, field)?;
            Ok(())
        }
        IntInput::NodeOutput { node_id, .. } => {
            require_node_type(graph, node_id, &[IrType::Int], field)?;
            Ok(())
        }
    }
}

pub(crate) fn require_sketch_dim(
    graph: &CanonicalGraph,
    dim: &SketchDimValue,
    expected: IrType,
    field: &str,
) -> Result<(), SemanticApiError> {
    match dim {
        SketchDimValue::Literal(quantity) => check_quantity(quantity, expected, field),
        SketchDimValue::Param { param_id } => {
            require_param_type(graph, param_id, expected, field)?;
            Ok(())
        }
    }
}

pub(crate) fn check_parameter_value(
    ir_type: IrType,
    value: &IrValue,
) -> Result<(), SemanticApiError> {
    match (ir_type, value) {
        (
            IrType::Length
            | IrType::Angle
            | IrType::Force
            | IrType::Pressure
            | IrType::Mass
            | IrType::Density,
            IrValue::Quantity(quantity),
        ) => check_quantity(quantity, ir_type, "parameter value"),
        (IrType::Scalar, IrValue::Scalar(number)) => {
            if number.is_finite() {
                Ok(())
            } else {
                Err(SemanticApiError::invalid("parameter value must be finite"))
            }
        }
        (IrType::Int, IrValue::Int(_))
        | (IrType::Bool, IrValue::Bool(_))
        | (IrType::String, IrValue::String(_)) => Ok(()),
        _ => Err(SemanticApiError::invalid(format!(
            "parameter value does not match declared type {ir_type:?}"
        ))),
    }
}

pub(crate) fn allocate_node_id(
    graph: &CanonicalGraph,
    explicit: &Option<NodeId>,
    prefix: &str,
) -> Result<NodeId, SemanticApiError> {
    if let Some(id) = explicit {
        if id.0.trim().is_empty() {
            return Err(SemanticApiError::invalid("node id cannot be empty"));
        }
        if id.0 == CONTRACT_NODE_ID {
            return Err(SemanticApiError::invalid(format!(
                "node id `{CONTRACT_NODE_ID}` is reserved for the design contract"
            )));
        }
        if graph.nodes.contains_key(&id.0) {
            return Err(SemanticApiError::invalid(format!(
                "node id `{}` already exists",
                id.0
            )));
        }
        return Ok(id.clone());
    }

    for number in 1..=1_000_000 {
        let candidate = format!("{prefix}_{number}");
        if !graph.nodes.contains_key(&candidate) {
            return Ok(NodeId::from(candidate));
        }
    }
    Err(SemanticApiError::invalid(format!(
        "could not allocate a free id with prefix `{prefix}`"
    )))
}

pub(crate) fn load_sketch<'a>(
    graph: &'a CanonicalGraph,
    id: &NodeId,
) -> Result<(&'a IrNode, StructuredSketch2D), SemanticApiError> {
    let node = require_node(graph, id)?;
    if node.kind != NodeKind::Sketch2D {
        return Err(SemanticApiError::invalid(format!(
            "node `{id}` is not a sketch"
        )));
    }
    let spec = node.try_feature_spec().map_err(SemanticApiError::from_ir)?;
    match spec {
        cad_ir::FeatureSpec::Sketch2D(sketch) => Ok((node, sketch)),
        _ => Err(SemanticApiError::invalid(format!(
            "node `{id}` does not carry a sketch payload"
        ))),
    }
}

pub(crate) fn check_sketch_bindings(
    graph: &CanonicalGraph,
    sketch: &StructuredSketch2D,
) -> Result<(), SemanticApiError> {
    if let Some(dim) = plane_offset(&sketch.plane) {
        require_sketch_dim(graph, dim, IrType::Length, "sketch plane offset")?;
    }
    for constraint in &sketch.constraints {
        match constraint {
            SketchConstraint2D::Distance { value, .. }
            | SketchConstraint2D::Radius { value, .. }
            | SketchConstraint2D::Diameter { value, .. } => {
                require_sketch_dim(graph, value, IrType::Length, "sketch constraint")?;
            }
            SketchConstraint2D::Angle { value, .. } => {
                require_sketch_dim(graph, value, IrType::Angle, "sketch angle constraint")?;
            }
            _ => {}
        }
    }
    for entity in &sketch.entities {
        if let SketchEntity2D::SplineBlob { blob, .. } = entity
            && !graph.blobs.contains_key(&blob.0)
        {
            return Err(SemanticApiError::not_found(format!(
                "blob `{}` was not found",
                blob.0
            )));
        }
    }
    Ok(())
}

fn plane_offset(plane: &cad_ir::SketchPlaneSpec) -> Option<&SketchDimValue> {
    match plane {
        cad_ir::SketchPlaneSpec::Datum { offset, .. }
        | cad_ir::SketchPlaneSpec::FaceRef { offset, .. } => offset.as_ref(),
        cad_ir::SketchPlaneSpec::Custom { .. } => None,
    }
}
