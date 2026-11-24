//! Compile semantic commands into one atomic [`cad_ir::GraphPatch`].
//!
//! Each command is checked against a staging copy of the graph, then appended
//! to the patch. The caller's graph is not modified. [`apply_commands`] clones
//! the graph, applies the patch, and only then evaluates the formula DAG.

use std::collections::BTreeSet;

use serde::Serialize;

use cad_ir::{
    BooleanOpKind, CanonicalGraph, ComponentSourceSpec, DesignAssertionKind, DimInput,
    DirectFaceEditOp, FastenerHoleStandard, FeatureSpec, GraphPatch, HoleDepthSpec,
    HoleProfileSpec, IrNode, IrType,
    MateKindSpec, NodeId, NodeState, ParamId, ParameterDef, PatchOp, StructuredSketch2D,
    validate_graph,
};

use crate::checks::{
    allocate_node_id, check_parameter_value, check_sketch_bindings, load_sketch, require_dim,
    require_int, require_name, require_node, require_node_type, require_param,
};
use crate::command::{
    AddDesignAssertion, AddMate, BooleanSolids, ChamferEdges, CreateHole, CreatePrimitiveBox,
    CreatePrimitiveCylinder, CreateSketch, DeleteNode, DesignContract, DirectFaceEditCommand,
    DraftFaces, ExtrudeSketch, FastenerClearance, FilletEdges, HoleDefinition, InsertComponent,
    LoftSections, MirrorFeature, PatternDefinition, PatternFeature, RemoveParameter, RenameNode,
    ReorderFeature, RevolveSketch, SemanticCommand, SetDesignContract, SetNodeSuppressed,
    SetFeatureInput, ShellSolid, SweepProfile, TagTopologyCommand, UpsertParameter,
    UpsertSketchConstraints,
    UpsertSketchEntities,
};
use crate::dto::{ApplyCommandsResult, ChangeSummary, StructuredChange};
use crate::error::SemanticApiError;
use crate::walk::{
    CONTRACT_NODE_ID, STDLIB_META_KEY, constraint_name, is_outline_feature, node_users,
    parameter_users, publish_outputs, read_outline,
};

struct Step {
    ops: Vec<PatchOp>,
    changes: Vec<StructuredChange>,
}

impl Step {
    fn new() -> Self {
        Self {
            ops: Vec::new(),
            changes: Vec::new(),
        }
    }

    fn change(&mut self, action: &str, target: &str, detail: impl Into<String>) {
        self.changes.push(StructuredChange {
            action: action.to_owned(),
            target: target.to_owned(),
            detail: detail.into(),
        });
    }

    fn add_feature(&mut self, graph: &CanonicalGraph, node: IrNode, detail: impl Into<String>) {
        let id = node.id.0.clone();
        let mut outline = read_outline(graph);
        outline.push(node.id.clone());
        let outputs = publish_outputs(graph, &outline);
        self.change("create", &id, detail);
        self.ops.push(PatchOp::AddNode(node));
        self.ops.push(PatchOp::SetOutputs(outputs));
    }

    fn update_feature(&mut self, node: IrNode, action: &str, detail: impl Into<String>) {
        let id = node.id.0.clone();
        self.change(action, &id, detail);
        self.ops.push(PatchOp::UpdateNode {
            id: node.id,
            name: Some(node.name),
            inputs: Some(node.inputs),
            kind: Some(node.kind),
        });
    }
}

struct CompiledBatch {
    patch: GraphPatch,
    changes: Vec<StructuredChange>,
}

/// Compile one command into a validated patch.
pub fn compile_command(
    graph: &CanonicalGraph,
    command: &SemanticCommand,
) -> Result<GraphPatch, SemanticApiError> {
    compile_batch(graph, std::slice::from_ref(command))
}

/// Compile a batch into one patch. Later commands see earlier commands in the batch.
///
/// The patch is not applied to `graph`. Any error leaves `graph` untouched.
pub fn compile_batch(
    graph: &CanonicalGraph,
    commands: &[SemanticCommand],
) -> Result<GraphPatch, SemanticApiError> {
    Ok(compile_batch_inner(graph, commands)?.patch)
}

fn compile_batch_inner(
    graph: &CanonicalGraph,
    commands: &[SemanticCommand],
) -> Result<CompiledBatch, SemanticApiError> {
    let mut staging = graph.clone();
    let mut ops = Vec::new();
    let mut changes = Vec::new();
    for (index, command) in commands.iter().enumerate() {
        let step = compile_one(&staging, command).map_err(|err| err.at(index))?;
        let step_patch = GraphPatch {
            patch_id: format!("sem-step-{index}"),
            description: command.command_name().to_owned(),
            ops: step.ops.clone(),
        };
        staging
            .apply_patch(&step_patch)
            .map_err(|err| SemanticApiError::from_ir(err).at(index))?;
        ops.extend(step.ops);
        changes.extend(step.changes);
    }

    let title = if commands.is_empty() {
        "No semantic changes".to_owned()
    } else {
        commands
            .iter()
            .map(SemanticCommand::command_name)
            .collect::<Vec<_>>()
            .join(", ")
    };
    Ok(CompiledBatch {
        patch: GraphPatch {
            patch_id: patch_id(commands)?,
            description: title,
            ops,
        },
        changes,
    })
}

/// Apply commands to a clone of `graph`.
///
/// On success the result holds the new graph, the patch, the net node edits,
/// and a [`ChangeSummary`]. On failure the caller's graph is left as it was.
pub fn apply_commands(
    graph: &CanonicalGraph,
    commands: &[SemanticCommand],
) -> Result<ApplyCommandsResult, SemanticApiError> {
    let compiled = compile_batch_inner(graph, commands)?;
    let mut next = graph.clone();
    next.apply_patch(&compiled.patch)
        .map_err(SemanticApiError::from_ir)?;
    let evaluated = evaluate_parameter_formulas(&mut next)?;
    validate_graph(&next).map_err(SemanticApiError::from_ir)?;

    let mut changes = compiled.changes;
    if !evaluated.is_empty() {
        let names = evaluated
            .iter()
            .map(|param| param.0.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        changes.push(StructuredChange {
            action: "evaluate_formulas".to_owned(),
            target: names,
            detail: "Recomputed parameter formulas in dependency order.".to_owned(),
        });
    }
    let (created_nodes, modified_nodes, deleted_nodes) = classify_nodes(graph, &compiled.patch.ops);
    let summary = ChangeSummary::from_changes(compiled.patch.description.clone(), changes);
    Ok(ApplyCommandsResult {
        graph: next,
        patch: compiled.patch,
        created_nodes,
        modified_nodes,
        deleted_nodes,
        summary,
    })
}

/// Evaluate every parameter formula in dependency order and write the results back.
pub fn evaluate_parameter_formulas(
    graph: &mut CanonicalGraph,
) -> Result<Vec<ParamId>, SemanticApiError> {
    graph.evaluate_formulas().map_err(SemanticApiError::from_ir)
}

fn compile_one(
    graph: &CanonicalGraph,
    command: &SemanticCommand,
) -> Result<Step, SemanticApiError> {
    match command {
        SemanticCommand::UpsertParameter(cmd) => compile_upsert_parameter(graph, cmd),
        SemanticCommand::SetFeatureInput(cmd) => compile_set_feature_input(graph, cmd),
        SemanticCommand::RemoveParameter(cmd) => compile_remove_parameter(graph, cmd),
        SemanticCommand::SetDesignContract(cmd) => compile_contract(graph, cmd),
        SemanticCommand::CreateSketch(cmd) => compile_create_sketch(graph, cmd),
        SemanticCommand::UpsertSketchEntities(cmd) => compile_upsert_entities(graph, cmd),
        SemanticCommand::UpsertSketchConstraints(cmd) => compile_upsert_constraints(graph, cmd),
        SemanticCommand::CreatePrimitiveBox(cmd) => compile_box(graph, cmd),
        SemanticCommand::CreatePrimitiveCylinder(cmd) => compile_cylinder(graph, cmd),
        SemanticCommand::ExtrudeSketch(cmd) => compile_extrude(graph, cmd),
        SemanticCommand::RevolveSketch(cmd) => compile_revolve(graph, cmd),
        SemanticCommand::SweepProfile(cmd) => compile_sweep(graph, cmd),
        SemanticCommand::LoftSections(cmd) => compile_loft(graph, cmd),
        SemanticCommand::CreateHole(cmd) => compile_hole(graph, cmd),
        SemanticCommand::FilletEdges(cmd) => compile_fillet(graph, cmd),
        SemanticCommand::ChamferEdges(cmd) => compile_chamfer(graph, cmd),
        SemanticCommand::ShellSolid(cmd) => compile_shell(graph, cmd),
        SemanticCommand::DraftFaces(cmd) => compile_draft(graph, cmd),
        SemanticCommand::PatternFeature(cmd) => compile_pattern(graph, cmd),
        SemanticCommand::MirrorFeature(cmd) => compile_mirror(graph, cmd),
        SemanticCommand::BooleanSolids(cmd) => compile_boolean(graph, cmd),
        SemanticCommand::DirectFaceEdit(cmd) => compile_direct_edit(graph, cmd),
        SemanticCommand::InsertComponent(cmd) => compile_component(graph, cmd),
        SemanticCommand::AddMate(cmd) => compile_mate(graph, cmd),
        SemanticCommand::RenameNode(cmd) => compile_rename(graph, cmd),
        SemanticCommand::SetNodeSuppressed(cmd) => compile_suppress(graph, cmd),
        SemanticCommand::DeleteNode(cmd) => compile_delete(graph, cmd),
        SemanticCommand::ReorderFeature(cmd) => compile_reorder(graph, cmd),
        SemanticCommand::TagTopology(cmd) => compile_tag(graph, cmd),
        SemanticCommand::AddDesignAssertion(cmd) => compile_assertion(graph, cmd),
    }
}

fn compile_upsert_parameter(
    graph: &CanonicalGraph,
    cmd: &UpsertParameter,
) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "parameter")?;
    check_parameter_value(cmd.ir_type, &cmd.value)?;
    if let Some(existing) = graph.parameters.get(&cmd.name)
        && existing.ir_type != cmd.ir_type
    {
        let message = format!(
            "parameter `{}` is {:?}; refusing to change it to {:?}",
            cmd.name, existing.ir_type, cmd.ir_type
        );
        if is_quantity(existing.ir_type) && is_quantity(cmd.ir_type) {
            return Err(SemanticApiError::unit_mismatch(message));
        }
        return Err(SemanticApiError::invalid(message));
    }
    let param_id = ParamId::from(cmd.name.as_str());
    if let Some(formula) = &cmd.formula {
        cad_ir::parse_formula(&param_id, formula).map_err(SemanticApiError::from_ir)?;
        let deps = cad_ir::collect_formula_dependencies(&param_id, formula)
            .map_err(SemanticApiError::from_ir)?;
        if deps.iter().any(|dep| dep.0 == cmd.name) {
            return Err(SemanticApiError::validation(format!(
                "parameter `{}` formula refers to itself",
                cmd.name
            )));
        }
        for dep in &deps {
            require_param(graph, dep)?;
        }
    }
    let mut step = Step::new();
    let formula_note = cmd
        .formula
        .as_ref()
        .map(|formula| format!("formula `{formula}`"))
        .unwrap_or_else(|| "literal value".to_owned());
    step.change(
        "upsert_parameter",
        &cmd.name,
        format!("{:?} parameter with {formula_note}", cmd.ir_type),
    );
    step.ops.push(PatchOp::UpsertParameter(ParameterDef {
        name: param_id,
        ir_type: cmd.ir_type,
        value: cmd.value.clone(),
        formula: cmd.formula.clone(),
    }));
    Ok(step)
}

fn is_quantity(ty: IrType) -> bool {
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

fn compile_set_feature_input(
    graph: &CanonicalGraph,
    cmd: &SetFeatureInput,
) -> Result<Step, SemanticApiError> {
    let node = graph
        .nodes
        .get(cmd.node_id.0.as_str())
        .ok_or_else(|| {
            SemanticApiError::invalid(format!("unknown feature node `{}`", cmd.node_id))
        })?;
    let mut spec = node
        .try_feature_spec()
        .map_err(SemanticApiError::from_ir)?;
    let input = cmd.input.trim();
    if input.is_empty() {
        return Err(SemanticApiError::invalid(
            "set_feature_input requires input",
        ));
    }
    match &mut spec {
        FeatureSpec::PrimitiveBox {
            width,
            depth,
            height,
            origin_mm,
        } => match input {
            "width" => *width = replace_literal_length(input, width, cmd)?,
            "depth" => *depth = replace_literal_length(input, depth, cmd)?,
            "height" => *height = replace_literal_length(input, height, cmd)?,
            "origin_mm" => *origin_mm = required_vector(input, cmd)?,
            other => {
                return Err(SemanticApiError::invalid(format!(
                    "primitive_box `{other}` is not editable. Use width, depth, height, or origin_mm."
                )));
            }
        },
        FeatureSpec::Hole {
            diameter,
            origin_mm,
            axis,
            ..
        } => match input {
            "diameter" => *diameter = replace_literal_length(input, diameter, cmd)?,
            "origin_mm" => *origin_mm = required_vector(input, cmd)?,
            "axis" => *axis = required_direction(input, cmd)?,
            other => {
                return Err(SemanticApiError::invalid(format!(
                    "hole `{other}` is not editable. Use diameter, origin_mm, or axis."
                )));
            }
        },
        _ => {
            return Err(SemanticApiError::invalid(format!(
                "node `{}` has no editable literal input. set_feature_input supports primitive_box and hole.",
                cmd.node_id
            )));
        }
    }
    let updated = feature_node(node.id.clone(), &node.name, spec)?;
    let mut step = Step::new();
    step.update_feature(
        updated,
        "set_feature_input",
        format!("Updated `{input}` on `{}`.", cmd.node_id),
    );
    Ok(step)
}

fn replace_literal_length(
    input: &str,
    current: &DimInput,
    cmd: &SetFeatureInput,
) -> Result<DimInput, SemanticApiError> {
    match current {
        DimInput::Param { param_id } => Err(SemanticApiError::invalid(format!(
            "`{input}` is bound to parameter `{}`. Use upsert_parameter to change it.",
            param_id.0
        ))),
        DimInput::NodeOutput { node_id, .. } => Err(SemanticApiError::invalid(format!(
            "`{input}` is bound to node `{node_id}`. Refusing to replace that binding with a literal."
        ))),
        DimInput::Literal(_) => {
            if cmd.vector_mm.is_some() {
                return Err(SemanticApiError::invalid(format!(
                    "`{input}` takes length_mm, not vector_mm"
                )));
            }
            let Some(length_mm) = cmd.length_mm else {
                return Err(SemanticApiError::invalid(format!(
                    "`{input}` requires length_mm"
                )));
            };
            if !length_mm.is_finite() || length_mm <= 0.0 {
                return Err(SemanticApiError::invalid(format!(
                    "`{input}` length_mm must be a finite number greater than zero"
                )));
            }
            DimInput::mm(length_mm).map_err(SemanticApiError::from_ir)
        }
    }
}

fn required_vector(input: &str, cmd: &SetFeatureInput) -> Result<[f64; 3], SemanticApiError> {
    if cmd.length_mm.is_some() {
        return Err(SemanticApiError::invalid(format!(
            "`{input}` takes vector_mm, not length_mm"
        )));
    }
    let Some(vector) = cmd.vector_mm else {
        return Err(SemanticApiError::invalid(format!(
            "`{input}` requires vector_mm"
        )));
    };
    require_finite(&vector, input)?;
    Ok(vector)
}

fn required_direction(input: &str, cmd: &SetFeatureInput) -> Result<[f64; 3], SemanticApiError> {
    let vector = required_vector(input, cmd)?;
    require_direction(&vector, input)?;
    Ok(vector)
}

fn compile_remove_parameter(
    graph: &CanonicalGraph,
    cmd: &RemoveParameter,
) -> Result<Step, SemanticApiError> {
    require_param(graph, &ParamId::from(cmd.name.as_str()))?;
    let users = parameter_users(graph, &cmd.name);
    if !users.is_empty() {
        return Err(SemanticApiError::validation(format!(
            "parameter `{}` is still used by {}",
            cmd.name,
            users.join(", ")
        )));
    }
    let mut step = Step::new();
    step.change(
        "remove_parameter",
        &cmd.name,
        "Removed the parameter from the table.",
    );
    step.ops.push(PatchOp::RemoveParameter {
        name: cmd.name.clone(),
    });
    Ok(step)
}

fn compile_contract(
    graph: &CanonicalGraph,
    cmd: &SetDesignContract,
) -> Result<Step, SemanticApiError> {
    let node = contract_node(&cmd.contract)?;
    let mut step = Step::new();
    let replaced = graph.nodes.contains_key(CONTRACT_NODE_ID);
    if replaced {
        step.ops.push(PatchOp::RemoveNode {
            id: NodeId::from(CONTRACT_NODE_ID),
        });
    }
    step.change(
        "set_design_contract",
        &cmd.contract.part_name,
        if replaced {
            "Replaced the document design contract."
        } else {
            "Stored the document design contract."
        },
    );
    step.ops.push(PatchOp::AddNode(node));
    if graph.outputs.iter().any(|id| id.0 == CONTRACT_NODE_ID) {
        let outline = read_outline(graph);
        step.ops
            .push(PatchOp::SetOutputs(publish_outputs(graph, &outline)));
    }
    Ok(step)
}

fn contract_node(contract: &DesignContract) -> Result<IrNode, SemanticApiError> {
    if !valid_part_name(&contract.part_name) {
        return Err(SemanticApiError::invalid(
            "design contract part_name must start with a letter and contain only letters, digits, and underscores",
        ));
    }
    let expression = serde_json::to_string(contract).map_err(|err| {
        SemanticApiError::invalid(format!("could not store the design contract: {err}"))
    })?;
    let mut params = std::collections::BTreeMap::new();
    params.insert("role".to_owned(), "design_contract".to_owned());
    let spec = FeatureSpec::DesignAssertion {
        target: None,
        assertion_id: "design_contract".to_owned(),
        kind: DesignAssertionKind::Custom {
            expression: expression.clone(),
            params,
        },
    };
    let mut node = IrNode::from_feature_spec(CONTRACT_NODE_ID, contract.part_name.clone(), spec)
        .map_err(SemanticApiError::from_ir)?;
    node.metadata
        .insert("role".to_owned(), "design_contract".to_owned());
    node.metadata.insert("contract_json".to_owned(), expression);
    Ok(node)
}

fn valid_part_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {
            chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        }
        _ => false,
    }
}

fn compile_create_sketch(
    graph: &CanonicalGraph,
    cmd: &CreateSketch,
) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "sketch")?;
    let id = allocate_node_id(graph, &cmd.node_id, "sketch")?;
    let sketch = StructuredSketch2D {
        plane: cmd.plane.clone(),
        entities: cmd.entities.clone(),
        profiles: cmd.profiles.clone(),
        constraints: cmd.constraints.clone(),
        remaining_dof: None,
    };
    check_sketch_bindings(graph, &sketch)?;
    let node = feature_node(id, &cmd.name, FeatureSpec::Sketch2D(sketch))?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Created sketch `{}`.", cmd.name));
    Ok(step)
}

fn compile_upsert_entities(
    graph: &CanonicalGraph,
    cmd: &UpsertSketchEntities,
) -> Result<Step, SemanticApiError> {
    if cmd.entities.is_empty() {
        return Err(SemanticApiError::invalid(
            "sketch entity upsert cannot be empty",
        ));
    }
    let (node, mut sketch) = load_sketch(graph, &cmd.node_id)?;
    if cmd.replace {
        sketch.entities = cmd.entities.clone();
    } else {
        for entity in &cmd.entities {
            if let Some(slot) = sketch
                .entities
                .iter_mut()
                .find(|current| current.id() == entity.id())
            {
                *slot = entity.clone();
            } else {
                sketch.entities.push(entity.clone());
            }
        }
    }
    check_sketch_bindings(graph, &sketch)?;
    let rebuilt = feature_node(node.id.clone(), &node.name, FeatureSpec::Sketch2D(sketch))?;
    let mut step = Step::new();
    step.update_feature(
        rebuilt,
        "update_sketch",
        format!("Upserted {} sketch entities.", cmd.entities.len()),
    );
    Ok(step)
}

fn compile_upsert_constraints(
    graph: &CanonicalGraph,
    cmd: &UpsertSketchConstraints,
) -> Result<Step, SemanticApiError> {
    if cmd.constraints.is_empty() {
        return Err(SemanticApiError::invalid(
            "sketch constraint upsert cannot be empty",
        ));
    }
    let (node, mut sketch) = load_sketch(graph, &cmd.node_id)?;
    if cmd.replace {
        sketch.constraints = cmd.constraints.clone();
    } else {
        for constraint in &cmd.constraints {
            let name = constraint_name(constraint).map(str::to_owned);
            if let Some(name) = name.as_deref()
                && let Some(slot) = sketch
                    .constraints
                    .iter_mut()
                    .find(|current| constraint_name(current) == Some(name))
            {
                *slot = constraint.clone();
            } else {
                sketch.constraints.push(constraint.clone());
            }
        }
    }
    check_sketch_bindings(graph, &sketch)?;
    let rebuilt = feature_node(node.id.clone(), &node.name, FeatureSpec::Sketch2D(sketch))?;
    let mut step = Step::new();
    step.update_feature(
        rebuilt,
        "update_sketch",
        format!("Upserted {} sketch constraints.", cmd.constraints.len()),
    );
    Ok(step)
}

fn compile_box(graph: &CanonicalGraph, cmd: &CreatePrimitiveBox) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_finite(&cmd.origin_mm, "origin")?;
    require_dim(graph, &cmd.width, IrType::Length, "width")?;
    require_dim(graph, &cmd.depth, IrType::Length, "depth")?;
    require_dim(graph, &cmd.height, IrType::Length, "height")?;
    let id = allocate_node_id(graph, &cmd.node_id, "box")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::PrimitiveBox {
            width: cmd.width.clone(),
            depth: cmd.depth.clone(),
            height: cmd.height.clone(),
            origin_mm: cmd.origin_mm,
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Created box `{}`.", cmd.name));
    Ok(step)
}

fn compile_cylinder(
    graph: &CanonicalGraph,
    cmd: &CreatePrimitiveCylinder,
) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_finite(&cmd.origin_mm, "origin")?;
    require_direction(&cmd.axis, "axis")?;
    require_dim(graph, &cmd.radius, IrType::Length, "radius")?;
    require_dim(graph, &cmd.height, IrType::Length, "height")?;
    let id = allocate_node_id(graph, &cmd.node_id, "cylinder")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::PrimitiveCylinder {
            radius: cmd.radius.clone(),
            height: cmd.height.clone(),
            origin_mm: cmd.origin_mm,
            axis: cmd.axis,
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Created cylinder `{}`.", cmd.name));
    Ok(step)
}

fn compile_extrude(graph: &CanonicalGraph, cmd: &ExtrudeSketch) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.sketch, &[IrType::Sketch], "sketch")?;
    require_dim(graph, &cmd.distance, IrType::Length, "distance")?;
    if let Some(direction) = cmd.direction {
        require_direction(&direction, "direction")?;
    }
    if let Some(target) = &cmd.target {
        require_node_type(graph, target, &[IrType::Solid], "extrude target")?;
    }
    let id = allocate_node_id(graph, &cmd.node_id, "extrude")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::Extrude {
            sketch: cmd.sketch.clone(),
            distance: cmd.distance.clone(),
            direction: cmd.direction,
            symmetric: cmd.symmetric,
            operation: cmd.operation,
            target: cmd.target.clone(),
        },
    )?;
    let mut step = Step::new();
    step.add_feature(
        graph,
        node,
        format!("Extruded sketch `{}` as `{}`.", cmd.sketch, cmd.name),
    );
    Ok(step)
}

fn compile_revolve(graph: &CanonicalGraph, cmd: &RevolveSketch) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.sketch, &[IrType::Sketch], "sketch")?;
    require_finite(&cmd.axis_origin_mm, "axis origin")?;
    require_direction(&cmd.axis_direction, "axis direction")?;
    require_dim(graph, &cmd.angle, IrType::Angle, "angle")?;
    if let Some(target) = &cmd.target {
        require_node_type(graph, target, &[IrType::Solid], "revolve target")?;
    }
    let id = allocate_node_id(graph, &cmd.node_id, "revolve")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::Revolve {
            sketch: cmd.sketch.clone(),
            axis_origin_mm: cmd.axis_origin_mm,
            axis_direction: cmd.axis_direction,
            angle: cmd.angle.clone(),
            operation: cmd.operation,
            target: cmd.target.clone(),
        },
    )?;
    let mut step = Step::new();
    step.add_feature(
        graph,
        node,
        format!("Revolved sketch `{}` as `{}`.", cmd.sketch, cmd.name),
    );
    Ok(step)
}

fn compile_sweep(graph: &CanonicalGraph, cmd: &SweepProfile) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.profile, &[IrType::Sketch], "sweep profile")?;
    if let crate::command::SweepPathInput::Sketch { node_id } = &cmd.path {
        require_node_type(graph, node_id, &[IrType::Sketch], "sweep path")?;
    }
    let id = allocate_node_id(graph, &cmd.node_id, "sweep")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::Sweep {
            profile: cmd.profile.clone(),
            path: cmd.path.to_spec(),
            keep_normal: cmd.keep_normal,
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Swept profile `{}`.", cmd.name));
    Ok(step)
}

fn compile_loft(graph: &CanonicalGraph, cmd: &LoftSections) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    if cmd.sections.len() < 2 {
        return Err(SemanticApiError::invalid(
            "loft requires at least two section sketches",
        ));
    }
    for section in cmd.sections.iter().chain(cmd.guides.iter()) {
        require_node_type(graph, section, &[IrType::Sketch], "loft section")?;
    }
    let id = allocate_node_id(graph, &cmd.node_id, "loft")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::Loft {
            sections: cmd.sections.clone(),
            guides: cmd.guides.clone(),
            closed: cmd.closed,
            as_surface: cmd.as_surface,
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Lofted `{}`.", cmd.name));
    Ok(step)
}

fn compile_hole(graph: &CanonicalGraph, cmd: &CreateHole) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.solid, &[IrType::Solid], "hole solid")?;
    require_finite(&cmd.origin_mm, "hole origin")?;
    require_direction(&cmd.axis, "hole axis")?;
    let (spec, resolved) = hole_spec(graph, cmd)?;
    let id = allocate_node_id(graph, &cmd.node_id, "hole")?;
    let mut node = feature_node(id, &cmd.name, spec)?;
    if let Some(resolved) = resolved {
        node.metadata.insert(STDLIB_META_KEY.to_owned(), resolved);
    }
    let mut step = Step::new();
    step.add_feature(
        graph,
        node,
        format!("Created hole `{}` on `{}`.", cmd.name, cmd.solid),
    );
    Ok(step)
}

fn hole_spec(
    graph: &CanonicalGraph,
    cmd: &CreateHole,
) -> Result<(FeatureSpec, Option<String>), SemanticApiError> {
    match &cmd.definition {
        HoleDefinition::Generic {
            diameter,
            depth,
            profile,
        } => {
            require_dim(graph, diameter, IrType::Length, "hole diameter")?;
            check_depth(graph, depth)?;
            check_profile(graph, profile)?;
            Ok((
                FeatureSpec::Hole {
                    solid: cmd.solid.clone(),
                    target_face: cmd.target_face.clone(),
                    origin_mm: cmd.origin_mm,
                    axis: cmd.axis,
                    diameter: diameter.clone(),
                    depth: depth.clone(),
                    profile: profile.clone(),
                },
                None,
            ))
        }
        HoleDefinition::Standard {
            standard,
            designation,
            clearance_class,
            depth,
            thread_depth,
        } => {
            if designation.trim().is_empty() {
                return Err(SemanticApiError::invalid(
                    "fastener designation cannot be empty",
                ));
            }
            check_depth(graph, depth)?;
            if let Some(thread_depth) = thread_depth {
                require_dim(graph, thread_depth, IrType::Length, "thread depth")?;
            }
            let resolved = resolve_fastener(*standard, designation, *clearance_class)?;
            Ok((
                FeatureSpec::StdFastenerHole {
                    solid: cmd.solid.clone(),
                    target_face: cmd.target_face.clone(),
                    origin_mm: cmd.origin_mm,
                    axis: cmd.axis,
                    standard: *standard,
                    designation: designation.clone(),
                    clearance_class: clearance_class.map(clearance_name).map(str::to_owned),
                    depth: depth.clone(),
                    thread_depth: thread_depth.clone(),
                },
                Some(resolved),
            ))
        }
    }
}

fn check_depth(graph: &CanonicalGraph, depth: &HoleDepthSpec) -> Result<(), SemanticApiError> {
    if let HoleDepthSpec::Blind { depth } = depth {
        require_dim(graph, depth, IrType::Length, "hole depth")?;
    }
    Ok(())
}

fn check_profile(
    graph: &CanonicalGraph,
    profile: &HoleProfileSpec,
) -> Result<(), SemanticApiError> {
    match profile {
        HoleProfileSpec::Simple => Ok(()),
        HoleProfileSpec::Counterbore {
            bore_diameter,
            bore_depth,
        } => {
            require_dim(graph, bore_diameter, IrType::Length, "counterbore diameter")?;
            require_dim(graph, bore_depth, IrType::Length, "counterbore depth")?;
            Ok(())
        }
        HoleProfileSpec::Countersink {
            sink_diameter,
            sink_angle,
        } => {
            require_dim(graph, sink_diameter, IrType::Length, "countersink diameter")?;
            require_dim(graph, sink_angle, IrType::Angle, "countersink angle")?;
            Ok(())
        }
    }
}

#[derive(Serialize)]
struct ResolvedFastener {
    standard: &'static str,
    designation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    clearance_class: Option<&'static str>,
    diameter_mm: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cbore_diameter_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cbore_depth_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    csink_diameter_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    csink_angle_deg: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tap_drill_mm: Option<f64>,
}

fn resolve_fastener(
    standard: FastenerHoleStandard,
    designation: &str,
    clearance: Option<FastenerClearance>,
) -> Result<String, SemanticApiError> {
    let class = clearance.unwrap_or(FastenerClearance::Normal);
    let stdlib_class = match class {
        FastenerClearance::Close => cad_stdlib::ClearanceClass::Close,
        FastenerClearance::Normal => cad_stdlib::ClearanceClass::Normal,
        FastenerClearance::Loose => cad_stdlib::ClearanceClass::Loose,
    };
    let resolved = match standard {
        FastenerHoleStandard::Iso4762Counterbore => {
            let spec = cad_stdlib::lookup_iso4762_cbore(designation).map_err(stdlib_error)?;
            ResolvedFastener {
                standard: "iso4762_counterbore",
                designation: designation.to_owned(),
                clearance_class: Some(clearance_name(class)),
                diameter_mm: spec.clearance_mm(stdlib_class),
                cbore_diameter_mm: Some(spec.cbore_diameter_mm),
                cbore_depth_mm: Some(spec.cbore_depth_mm),
                csink_diameter_mm: None,
                csink_angle_deg: None,
                tap_drill_mm: Some(spec.tap_drill_mm),
            }
        }
        FastenerHoleStandard::Iso10642Countersink => {
            let spec = cad_stdlib::lookup_iso10642_csink(designation).map_err(stdlib_error)?;
            ResolvedFastener {
                standard: "iso10642_countersink",
                designation: designation.to_owned(),
                clearance_class: clearance.map(clearance_name),
                diameter_mm: spec.clearance_normal_mm,
                cbore_diameter_mm: None,
                cbore_depth_mm: None,
                csink_diameter_mm: Some(spec.csink_diameter_mm),
                csink_angle_deg: Some(spec.csink_angle_deg),
                tap_drill_mm: None,
            }
        }
        FastenerHoleStandard::MetricTapDrill => {
            let drill = cad_stdlib::lookup_metric_tap_drill(designation).map_err(stdlib_error)?;
            ResolvedFastener {
                standard: "metric_tap_drill",
                designation: designation.to_owned(),
                clearance_class: None,
                diameter_mm: drill,
                cbore_diameter_mm: None,
                cbore_depth_mm: None,
                csink_diameter_mm: None,
                csink_angle_deg: None,
                tap_drill_mm: Some(drill),
            }
        }
        FastenerHoleStandard::MetricClearance => {
            let spec = cad_stdlib::lookup_iso4762_cbore(designation).map_err(stdlib_error)?;
            ResolvedFastener {
                standard: "metric_clearance",
                designation: designation.to_owned(),
                clearance_class: Some(clearance_name(class)),
                diameter_mm: spec.clearance_mm(stdlib_class),
                cbore_diameter_mm: None,
                cbore_depth_mm: None,
                csink_diameter_mm: None,
                csink_angle_deg: None,
                tap_drill_mm: None,
            }
        }
    };
    serde_json::to_string(&resolved).map_err(|err| {
        SemanticApiError::invalid(format!("could not record fastener lookup: {err}"))
    })
}

fn clearance_name(class: FastenerClearance) -> &'static str {
    match class {
        FastenerClearance::Close => "close",
        FastenerClearance::Normal => "normal",
        FastenerClearance::Loose => "loose",
    }
}

fn stdlib_error(err: cad_stdlib::StdLibError) -> SemanticApiError {
    SemanticApiError::invalid(err.to_string())
}

fn compile_fillet(graph: &CanonicalGraph, cmd: &FilletEdges) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.solid, &[IrType::Solid], "fillet solid")?;
    require_dim(graph, &cmd.radius, IrType::Length, "fillet radius")?;
    let id = allocate_node_id(graph, &cmd.node_id, "fillet")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::Fillet {
            solid: cmd.solid.clone(),
            edges: cmd.edges.clone(),
            radius: cmd.radius.clone(),
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Filleted `{}`.", cmd.name));
    Ok(step)
}

fn compile_chamfer(graph: &CanonicalGraph, cmd: &ChamferEdges) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.solid, &[IrType::Solid], "chamfer solid")?;
    require_dim(graph, &cmd.distance, IrType::Length, "chamfer distance")?;
    if let Some(distance2) = &cmd.distance2 {
        require_dim(graph, distance2, IrType::Length, "chamfer distance2")?;
    }
    if let Some(angle) = &cmd.angle {
        require_dim(graph, angle, IrType::Angle, "chamfer angle")?;
    }
    let id = allocate_node_id(graph, &cmd.node_id, "chamfer")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::Chamfer {
            solid: cmd.solid.clone(),
            edges: cmd.edges.clone(),
            distance: cmd.distance.clone(),
            distance2: cmd.distance2.clone(),
            angle: cmd.angle.clone(),
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Chamfered `{}`.", cmd.name));
    Ok(step)
}

fn compile_shell(graph: &CanonicalGraph, cmd: &ShellSolid) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.solid, &[IrType::Solid], "shell solid")?;
    require_dim(graph, &cmd.thickness, IrType::Length, "shell thickness")?;
    let id = allocate_node_id(graph, &cmd.node_id, "shell")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::Shell {
            solid: cmd.solid.clone(),
            open_faces: cmd.open_faces.clone(),
            thickness: cmd.thickness.clone(),
            inward: cmd.inward,
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Shelled `{}`.", cmd.name));
    Ok(step)
}

fn compile_draft(graph: &CanonicalGraph, cmd: &DraftFaces) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.solid, &[IrType::Solid], "draft solid")?;
    if cmd.faces.is_empty() {
        return Err(SemanticApiError::invalid(
            "draft requires at least one face",
        ));
    }
    require_direction(&cmd.pull_direction, "pull direction")?;
    require_dim(graph, &cmd.angle, IrType::Angle, "draft angle")?;
    check_plane_offset(graph, &cmd.neutral_plane)?;
    let id = allocate_node_id(graph, &cmd.node_id, "draft")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::Draft {
            solid: cmd.solid.clone(),
            faces: cmd.faces.clone(),
            pull_direction: cmd.pull_direction,
            neutral_plane: cmd.neutral_plane.clone(),
            angle: cmd.angle.clone(),
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Drafted `{}`.", cmd.name));
    Ok(step)
}

fn compile_pattern(graph: &CanonicalGraph, cmd: &PatternFeature) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.solid, &[IrType::Solid], "pattern feature")?;
    let spec = match &cmd.definition {
        PatternDefinition::Linear {
            direction,
            count,
            spacing,
            second_direction,
        } => {
            require_direction(direction, "pattern direction")?;
            require_int(graph, count, "pattern")?;
            require_dim(graph, spacing, IrType::Length, "pattern spacing")?;
            if let Some(axis) = second_direction {
                require_direction(&axis.direction, "second pattern direction")?;
                require_int(graph, &axis.count, "second pattern")?;
                require_dim(
                    graph,
                    &axis.spacing,
                    IrType::Length,
                    "second pattern spacing",
                )?;
            }
            FeatureSpec::LinearPattern {
                solid: cmd.solid.clone(),
                direction: *direction,
                count: count.clone(),
                spacing: spacing.clone(),
                second_direction: second_direction.clone(),
            }
        }
        PatternDefinition::Polar {
            axis_origin_mm,
            axis_direction,
            count,
            total_angle,
        } => {
            require_finite(axis_origin_mm, "pattern axis origin")?;
            require_direction(axis_direction, "pattern axis")?;
            require_int(graph, count, "pattern")?;
            require_dim(graph, total_angle, IrType::Angle, "pattern angle")?;
            FeatureSpec::PolarPattern {
                solid: cmd.solid.clone(),
                axis_origin_mm: *axis_origin_mm,
                axis_direction: *axis_direction,
                count: count.clone(),
                total_angle: total_angle.clone(),
                radius: None,
                start_angle: None,
            }
        }
    };
    let id = allocate_node_id(graph, &cmd.node_id, "pattern")?;
    let node = feature_node(id, &cmd.name, spec)?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Patterned `{}`.", cmd.name));
    Ok(step)
}

fn compile_mirror(graph: &CanonicalGraph, cmd: &MirrorFeature) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.solid, &[IrType::Solid], "mirror solid")?;
    check_plane_offset(graph, &cmd.plane)?;
    let id = allocate_node_id(graph, &cmd.node_id, "mirror")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::Mirror {
            solid: cmd.solid.clone(),
            plane: cmd.plane.clone(),
            keep_original: cmd.keep_original,
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Mirrored `{}`.", cmd.name));
    Ok(step)
}

fn compile_boolean(graph: &CanonicalGraph, cmd: &BooleanSolids) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.target, &[IrType::Solid], "boolean target")?;
    if cmd.tools.is_empty() {
        return Err(SemanticApiError::invalid(
            "boolean feature requires at least one tool solid",
        ));
    }
    for tool in &cmd.tools {
        if tool == &cmd.target {
            return Err(SemanticApiError::invalid(
                "boolean tool cannot be the same node as the target",
            ));
        }
        require_node_type(graph, tool, &[IrType::Solid], "boolean tool")?;
    }
    let id = allocate_node_id(graph, &cmd.node_id, "boolean")?;
    let op_name = match cmd.op {
        BooleanOpKind::Union => "union",
        BooleanOpKind::Subtract => "subtract",
        BooleanOpKind::Intersect => "intersect",
    };
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::Boolean {
            op: cmd.op,
            target: cmd.target.clone(),
            tools: cmd.tools.clone(),
        },
    )?;
    let mut step = Step::new();
    step.add_feature(
        graph,
        node,
        format!("Boolean {op_name} produced `{}`.", cmd.name),
    );
    Ok(step)
}

fn compile_direct_edit(
    graph: &CanonicalGraph,
    cmd: &DirectFaceEditCommand,
) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(graph, &cmd.solid, &[IrType::Solid], "face edit solid")?;
    if cmd.face.trim().is_empty() {
        return Err(SemanticApiError::invalid("face reference cannot be empty"));
    }
    match &cmd.edit {
        DirectFaceEditOp::Offset { distance } => {
            require_dim(graph, distance, IrType::Length, "face offset")?;
        }
        DirectFaceEditOp::Move { delta_mm } => require_finite(delta_mm, "face move")?,
        DirectFaceEditOp::DeleteAndHeal => {}
    }
    let id = allocate_node_id(graph, &cmd.node_id, "face_edit")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::DirectFaceEdit {
            solid: cmd.solid.clone(),
            face: cmd.face.clone(),
            edit: cmd.edit.clone(),
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Edited face on `{}`.", cmd.name));
    Ok(step)
}

fn compile_component(
    graph: &CanonicalGraph,
    cmd: &InsertComponent,
) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "component")?;
    require_finite(&cmd.translation_mm, "translation")?;
    require_finite(&cmd.rotation_deg, "rotation")?;
    match &cmd.source {
        ComponentSourceSpec::LocalNode { node_id } => {
            require_node_type(
                graph,
                node_id,
                &[IrType::Solid, IrType::Surface, IrType::Assembly],
                "component geometry",
            )?;
        }
        ComponentSourceSpec::ExternalRef { ref_id } => {
            if !graph.external_refs.contains_key(&ref_id.0) {
                return Err(SemanticApiError::not_found(format!(
                    "external reference `{}` was not found",
                    ref_id.0
                )));
            }
        }
    }
    let id = allocate_node_id(graph, &cmd.node_id, "component")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::ComponentInstance {
            source: cmd.source.clone(),
            translation_mm: cmd.translation_mm,
            rotation_deg: cmd.rotation_deg,
            grounded: cmd.grounded,
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Inserted component `{}`.", cmd.name));
    Ok(step)
}

fn compile_mate(graph: &CanonicalGraph, cmd: &AddMate) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "mate")?;
    if cmd.left == cmd.right {
        return Err(SemanticApiError::invalid(
            "mate endpoints must be different components",
        ));
    }
    require_node_type(graph, &cmd.left, &[IrType::Component], "mate left")?;
    require_node_type(graph, &cmd.right, &[IrType::Component], "mate right")?;
    if cmd.left_ref.trim().is_empty() || cmd.right_ref.trim().is_empty() {
        return Err(SemanticApiError::invalid(
            "mate topology references cannot be empty",
        ));
    }
    match &cmd.mate_kind {
        MateKindSpec::Distance { value } => {
            require_dim(graph, value, IrType::Length, "mate distance")?
        }
        MateKindSpec::Angle { value } => require_dim(graph, value, IrType::Angle, "mate angle")?,
        MateKindSpec::Gear { ratio } => {
            if !ratio.is_finite() || *ratio == 0.0 {
                return Err(SemanticApiError::invalid(
                    "gear mate ratio must be a non-zero finite number",
                ));
            }
        }
        _ => {}
    }
    let id = allocate_node_id(graph, &cmd.node_id, "mate")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::MateConstraint {
            left: cmd.left.clone(),
            right: cmd.right.clone(),
            left_ref: cmd.left_ref.clone(),
            right_ref: cmd.right_ref.clone(),
            mate_kind: cmd.mate_kind.clone(),
            flip: cmd.flip,
        },
    )?;
    let mut step = Step::new();
    step.add_feature(
        graph,
        node,
        format!("Mated `{}` to `{}`.", cmd.left, cmd.right),
    );
    Ok(step)
}

fn compile_rename(graph: &CanonicalGraph, cmd: &RenameNode) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "node")?;
    require_node(graph, &cmd.node_id)?;
    let mut step = Step::new();
    step.change(
        "rename",
        &cmd.node_id.0,
        format!("Renamed the node to `{}`.", cmd.name),
    );
    step.ops.push(PatchOp::UpdateNode {
        id: cmd.node_id.clone(),
        name: Some(cmd.name.clone()),
        inputs: None,
        kind: None,
    });
    Ok(step)
}

fn compile_suppress(
    graph: &CanonicalGraph,
    cmd: &SetNodeSuppressed,
) -> Result<Step, SemanticApiError> {
    require_node(graph, &cmd.node_id)?;
    let state = if cmd.suppressed {
        NodeState::Suppressed
    } else {
        NodeState::Valid
    };
    let mut step = Step::new();
    step.change(
        if cmd.suppressed {
            "suppress"
        } else {
            "unsuppress"
        },
        &cmd.node_id.0,
        if cmd.suppressed {
            "Suppressed the node."
        } else {
            "Restored the node."
        },
    );
    step.ops.push(PatchOp::SetNodeState {
        id: cmd.node_id.clone(),
        state,
    });
    Ok(step)
}

fn compile_delete(graph: &CanonicalGraph, cmd: &DeleteNode) -> Result<Step, SemanticApiError> {
    require_node(graph, &cmd.node_id)?;
    let users = node_users(graph, &cmd.node_id);
    if !users.is_empty() {
        let names = users
            .iter()
            .map(|id| id.0.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(SemanticApiError::validation(format!(
            "node `{}` is still used by {names}",
            cmd.node_id
        )));
    }
    let mut outline = read_outline(graph);
    outline.retain(|id| id != &cmd.node_id);
    let mut outputs = publish_outputs(graph, &outline);
    outputs.retain(|id| id != &cmd.node_id);
    let mut step = Step::new();
    step.change("delete", &cmd.node_id.0, "Deleted the node.");
    step.ops.push(PatchOp::RemoveNode {
        id: cmd.node_id.clone(),
    });
    step.ops.push(PatchOp::SetOutputs(outputs));
    Ok(step)
}

fn compile_reorder(graph: &CanonicalGraph, cmd: &ReorderFeature) -> Result<Step, SemanticApiError> {
    require_node(graph, &cmd.node_id)?;
    if !is_outline_feature(graph, &cmd.node_id) {
        return Err(SemanticApiError::invalid(
            "only feature nodes can be reordered",
        ));
    }
    let mut outline = read_outline(graph);
    let Some(from) = outline.iter().position(|id| id == &cmd.node_id) else {
        return Err(SemanticApiError::invalid(format!(
            "feature `{}` is not in the feature tree",
            cmd.node_id
        )));
    };
    let node = outline.remove(from);
    let insert_at = if let Some(before) = &cmd.before {
        if before == &cmd.node_id {
            return Err(SemanticApiError::invalid(
                "reorder target cannot be the same node",
            ));
        }
        require_node(graph, before)?;
        outline.iter().position(|id| id == before).ok_or_else(|| {
            SemanticApiError::not_found(format!("feature `{before}` is not in the feature tree"))
        })?
    } else {
        outline.len()
    };
    outline.insert(insert_at, node);
    let place = cmd
        .before
        .as_ref()
        .map(|id| format!("before `{id}`"))
        .unwrap_or_else(|| "to the end".to_owned());
    let mut step = Step::new();
    step.change(
        "reorder",
        &cmd.node_id.0,
        format!("Moved the feature {place}."),
    );
    step.ops
        .push(PatchOp::SetOutputs(publish_outputs(graph, &outline)));
    Ok(step)
}

fn compile_tag(graph: &CanonicalGraph, cmd: &TagTopologyCommand) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    require_node_type(
        graph,
        &cmd.solid,
        &[IrType::Solid, IrType::Surface],
        "tagged solid",
    )?;
    let id = allocate_node_id(graph, &cmd.node_id, "tag")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::TagTopology {
            solid: cmd.solid.clone(),
            tag: cmd.tag.clone(),
            targets: cmd.targets.clone(),
        },
    )?;
    let mut step = Step::new();
    step.add_feature(graph, node, format!("Tagged topology `{}`.", cmd.tag));
    Ok(step)
}

fn compile_assertion(
    graph: &CanonicalGraph,
    cmd: &AddDesignAssertion,
) -> Result<Step, SemanticApiError> {
    require_name(&cmd.name, "feature")?;
    if cmd.assertion_id.trim().is_empty() {
        return Err(SemanticApiError::invalid("assertion id cannot be empty"));
    }
    if let Some(target) = &cmd.target {
        require_node(graph, target)?;
    }
    match &cmd.kind {
        DesignAssertionKind::VolumeBounds { min_mm3, max_mm3 } => {
            if let Some(min) = min_mm3
                && !min.is_finite()
            {
                return Err(SemanticApiError::invalid("volume bound must be finite"));
            }
            if let Some(max) = max_mm3
                && !max.is_finite()
            {
                return Err(SemanticApiError::invalid("volume bound must be finite"));
            }
        }
        DesignAssertionKind::MinWallThickness { min } => {
            require_dim(graph, min, IrType::Length, "minimum wall thickness")?;
        }
        DesignAssertionKind::BooleanCondition { condition } => {
            require_node_type(graph, condition, &[IrType::Bool], "assertion condition")?;
        }
        DesignAssertionKind::Custom { expression, .. } => {
            if expression.trim().is_empty() {
                return Err(SemanticApiError::invalid(
                    "custom assertion expression cannot be empty",
                ));
            }
        }
    }
    let id = allocate_node_id(graph, &cmd.node_id, "assertion")?;
    let node = feature_node(
        id,
        &cmd.name,
        FeatureSpec::DesignAssertion {
            target: cmd.target.clone(),
            assertion_id: cmd.assertion_id.clone(),
            kind: cmd.kind.clone(),
        },
    )?;
    let mut step = Step::new();
    step.add_feature(
        graph,
        node,
        format!("Added design assertion `{}`.", cmd.assertion_id),
    );
    Ok(step)
}

fn feature_node(id: NodeId, name: &str, spec: FeatureSpec) -> Result<IrNode, SemanticApiError> {
    IrNode::from_feature_spec(id, name, spec).map_err(SemanticApiError::from_ir)
}

fn check_plane_offset(
    graph: &CanonicalGraph,
    plane: &cad_ir::SketchPlaneSpec,
) -> Result<(), SemanticApiError> {
    let sketch = StructuredSketch2D::new(plane.clone());
    check_sketch_bindings(graph, &sketch)
}

fn require_finite(value: &[f64; 3], field: &str) -> Result<(), SemanticApiError> {
    if value.iter().all(|component| component.is_finite()) {
        Ok(())
    } else {
        Err(SemanticApiError::invalid(format!("{field} must be finite")))
    }
}

fn require_direction(value: &[f64; 3], field: &str) -> Result<(), SemanticApiError> {
    require_finite(value, field)?;
    if value.iter().all(|component| *component == 0.0) {
        Err(SemanticApiError::invalid(format!(
            "{field} cannot be the zero vector"
        )))
    } else {
        Ok(())
    }
}

fn classify_nodes(
    original: &CanonicalGraph,
    ops: &[PatchOp],
) -> (Vec<NodeId>, Vec<NodeId>, Vec<NodeId>) {
    let mut exists: BTreeSet<String> = original.nodes.keys().cloned().collect();
    let mut modified: BTreeSet<String> = BTreeSet::new();
    for op in ops {
        match op {
            PatchOp::AddNode(node) => {
                if original.nodes.contains_key(&node.id.0) {
                    modified.insert(node.id.0.clone());
                }
                exists.insert(node.id.0.clone());
            }
            PatchOp::RemoveNode { id } => {
                exists.remove(&id.0);
                modified.remove(&id.0);
            }
            PatchOp::UpdateNode { id, .. } | PatchOp::SetNodeState { id, .. } => {
                if exists.contains(&id.0) {
                    modified.insert(id.0.clone());
                }
            }
            _ => {}
        }
    }
    let created_nodes = exists
        .iter()
        .filter(|id| !original.nodes.contains_key(id.as_str()))
        .map(|id| NodeId::from(id.as_str()))
        .collect();
    let deleted_nodes = original
        .nodes
        .keys()
        .filter(|id| !exists.contains(*id))
        .map(|id| NodeId::from(id.as_str()))
        .collect();
    modified.retain(|id| exists.contains(id) && original.nodes.contains_key(id));
    let modified_nodes = modified.into_iter().map(NodeId::from).collect();
    (created_nodes, modified_nodes, deleted_nodes)
}

fn patch_id(commands: &[SemanticCommand]) -> Result<String, SemanticApiError> {
    if commands.is_empty() {
        return Ok("sem-empty".to_owned());
    }
    let bytes = serde_json::to_vec(commands)
        .map_err(|err| SemanticApiError::invalid(format!("could not encode commands: {err}")))?;
    Ok(format!("sem-{:016x}", fnv1a64(&bytes)))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
