//! Lower the typed CAD-DSL v0.2 frontend into the canonical graph.

mod expressions;
mod operations;
mod sketch_spec;

use std::collections::{BTreeMap, HashMap};

use cad_hir::{DimType, IrAnnotation, IrExpr, IrPart, Severity, check_and_lower};
use cad_ir::{
    CanonicalGraph, IrNode, IrType, IrValue, NodeId, NodeKind, NodeState, OUTPUT_PORT,
    ParameterDef, UnitSystem, validate_graph,
};
use cad_syntax::{Span, TokenKind, tokenize};

use crate::{BridgeError, model, parse_kcl_to_graph};

type Result<T> = std::result::Result<T, BridgeError>;
type Inputs = BTreeMap<String, IrValue>;

/// Lower one Solid-producing CAD-DSL part, or several parts plus one assembly.
/// A graph stores parameter defaults, including nominal values and formulas
/// for dependent expressions. Unnamed checks and mates stay in the graph.
pub fn lower_dsl_to_graph(source: &str) -> Result<CanonicalGraph> {
    let parsed = cad_syntax::parse(source.trim_start_matches('\u{feff}'));
    if let Some(error) = parsed.lex_errors.first() {
        return Err(at(error.span, &error.message));
    }
    if let Some(error) = parsed.syntax_errors.first() {
        return Err(at(error.span, &error.message));
    }
    let checked = check_and_lower(&parsed.file);
    if let Some(error) = checked
        .diagnostics
        .iter()
        .find(|d| d.severity == Severity::Error)
    {
        return Err(at(error.span, format!("{}: {}", error.code, error.message)));
    }
    let module = checked.module;
    if module.assemblies.len() > 1 {
        return Err(at(
            module.assemblies[1].span,
            "a CAD-DSL graph supports one assembly declaration",
        ));
    }
    if module.assemblies.is_empty() && module.parts.len() > 1 {
        return Err(at(
            module.parts[1].span,
            "a CAD-DSL graph supports one part declaration",
        ));
    }
    let graph_name = module
        .assemblies
        .first()
        .or_else(|| module.parts.first())
        .ok_or_else(|| BridgeError::at(1, 1, "expected a CAD-DSL part declaration"))?;
    for part in &module.parts {
        if part.return_type != DimType::Solid {
            return Err(at(part.span, "a CAD-DSL part requires a Solid return type"));
        }
    }
    if let Some(assembly) = module.assemblies.first()
        && assembly.return_type != DimType::Assembly
    {
        return Err(at(
            assembly.span,
            "a CAD-DSL assembly requires an Assembly return type",
        ));
    }
    let mut parts_by_name = HashMap::new();
    for part in &module.parts {
        parts_by_name.insert(part.name.clone(), part.clone());
    }
    let mut lower = Lowering {
        graph: CanonicalGraph::new(&graph_name.name),
        bindings: HashMap::new(),
        next_id: 0,
        parts: parts_by_name,
        default_part_outputs: HashMap::new(),
        part_instance_counter: 0,
    };
    lower.graph.units = lower_units(&module.annotations)?;
    for constant in &module.constants {
        let name = constant.name.as_deref().expect("HIR constants are named");
        let value = lower.value(&constant.expr, Some(name))?;
        lower.bindings.insert(name.to_owned(), value);
    }
    if let Some(assembly) = module.assemblies.first() {
        let outputs = lower.lower_decl(assembly, &HashMap::new(), None)?;
        if outputs.is_empty() {
            return Err(at(assembly.span, "assembly requires a returned component"));
        }
        for output in &outputs {
            let ty = lower.graph.nodes[&output.0].output_type;
            if !matches!(ty, IrType::Component | IrType::Assembly | IrType::Solid) {
                return Err(at(
                    assembly.span,
                    "assembly return values must be components or solids",
                ));
            }
        }
        lower.graph.outputs = outputs;
    } else {
        let mut part_outputs = Vec::new();
        for part in &module.parts {
            let outputs = lower.lower_decl(part, &HashMap::new(), None)?;
            let [output] = outputs.as_slice() else {
                return Err(at(part.span, "a CAD-DSL part returns one Solid"));
            };
            if lower.graph.nodes[&output.0].output_type != IrType::Solid {
                return Err(at(part.span, "a CAD-DSL part returns one Solid"));
            }
            part_outputs.push(output.clone());
            lower
                .bindings
                .insert(part.name.clone(), node_ref(output.clone()));
        }
        lower.graph.outputs = part_outputs;
    }
    let span = module
        .assemblies
        .first()
        .or_else(|| module.parts.first())
        .map(|decl| decl.span)
        .unwrap_or_default();
    validate_graph(&lower.graph).map_err(|error| at(span, error.to_string()))?;
    Ok(lower.graph)
}

/// Parse `.kcl 2.0` if an `@ir_schema` header is present, or lower CAD-DSL v0.2.
/// Detection uses tokens so comments, quoted strings and whitespace are harmless.
pub fn parse_any_kcl_to_graph(source: &str) -> Result<CanonicalGraph> {
    use TokenKind::*;
    let source = source.trim_start_matches('\u{feff}');
    let (tokens, errors) = tokenize(source);
    if let Some(error) = errors.first() {
        return Err(at(error.span, &error.message));
    }
    let mut depth = 0usize;
    for pair in tokens.windows(2) {
        match &pair[0].kind {
            At if depth == 0 && matches!(&pair[1].kind, Ident(name) if name == "ir_schema") => {
                return parse_kcl_to_graph(source);
            }
            Part | Assembly if depth == 0 => return lower_dsl_to_graph(source),
            LParen | LBrace | LBracket => depth += 1,
            RParen | RBrace | RBracket => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Err(BridgeError::at(
        1,
        1,
        "expected `@ir_schema(...)`, `part`, or `assembly`",
    ))
}

fn lower_units(annotations: &[IrAnnotation]) -> Result<UnitSystem> {
    let mut units = UnitSystem::default();
    let mut seen = std::collections::BTreeSet::new();
    for annotation in annotations.iter().filter(|ann| ann.path == "unit") {
        for arg in &annotation.args {
            let (Some(name), IrExpr::Str { value, .. }) = (arg.name.as_deref(), &arg.value) else {
                return Err(at(
                    arg.span,
                    "expected a named string unit, such as length = \"mm\"",
                ));
            };
            if !seen.insert(name) {
                return Err(at(arg.span, format!("duplicate `{name}` unit")));
            }
            let unknown = || at(arg.span, format!("unsupported {name} unit `{value}`"));
            match name {
                "length" => {
                    units.length = model::length_unit_from_name(value).ok_or_else(unknown)?
                }
                "angle" => units.angle = model::angle_unit_from_name(value).ok_or_else(unknown)?,
                "force" => units.force = model::force_unit_from_name(value).ok_or_else(unknown)?,
                "pressure" => {
                    units.pressure = model::pressure_unit_from_name(value).ok_or_else(unknown)?
                }
                "mass" => units.mass = model::mass_unit_from_name(value).ok_or_else(unknown)?,
                _ => return Err(at(arg.span, format!("unknown unit dimension `{name}`"))),
            }
        }
    }
    Ok(units)
}

fn output_ids(value: &IrValue, span: Span) -> Result<Vec<NodeId>> {
    match value {
        IrValue::NodeRef { node_id, .. } => Ok(vec![node_id.clone()]),
        IrValue::List(items) => items
            .iter()
            .map(|item| match item {
                IrValue::NodeRef { node_id, .. } => Ok(node_id.clone()),
                _ => Err(at(span, "return list items must be graph nodes")),
            })
            .collect(),
        _ => Err(at(span, "return expression must produce a graph node")),
    }
}

fn at(span: Span, message: impl Into<String>) -> BridgeError {
    BridgeError::at(span.start_line, span.start_col, message)
}

struct Lowering {
    graph: CanonicalGraph,
    bindings: HashMap<String, IrValue>,
    next_id: usize,
    parts: HashMap<String, IrPart>,
    default_part_outputs: HashMap<String, NodeId>,
    part_instance_counter: usize,
}

fn node_ref(node_id: NodeId) -> IrValue {
    IrValue::NodeRef {
        node_id,
        port: OUTPUT_PORT.to_owned(),
    }
}

impl Lowering {
    fn lower_decl(
        &mut self,
        decl: &IrPart,
        overrides: &HashMap<String, IrValue>,
        instance_prefix: Option<&str>,
    ) -> Result<Vec<NodeId>> {
        let saved = self.bindings.clone();
        for param in &decl.params {
            if let Some(override_val) = overrides.get(&param.name) {
                self.bindings
                    .insert(param.name.clone(), override_val.clone());
                continue;
            }
            let default = param.default_expr.as_ref().ok_or_else(|| {
                at(
                    param.span,
                    format!("parameter `{}` requires a default value", param.name),
                )
            })?;
            let ty = expressions::parameter_type(&param.ty, param.span)?;
            let (value, formula) = self.parameter_default(default, ty)?;
            let param_graph_name = if let Some(prefix) = instance_prefix {
                format!("{prefix}__{}", param.name)
            } else if self.graph.parameters.contains_key(&param.name) {
                let candidate = format!("{}__{}", decl.name, param.name);
                if self.graph.parameters.contains_key(&candidate) {
                    format!("{}_{}", self.generated_id(), param.name)
                } else {
                    candidate
                }
            } else {
                param.name.clone()
            };
            let reference = self.add_parameter(param_graph_name, ty, value, formula);
            self.bindings.insert(param.name.clone(), reference);
        }
        let return_id = decl
            .return_node
            .ok_or_else(|| at(decl.span, "declaration requires a return expression"))?;
        let mut outputs = None;
        for node in &decl.nodes {
            let name = node.name.clone().or_else(|| {
                (node.id == return_id).then(|| match instance_prefix {
                    Some(prefix) => prefix.to_owned(),
                    None => decl.name.clone(),
                })
            });
            let scoped_name = match (instance_prefix, name) {
                (Some(prefix), Some(raw)) if node.id != return_id => Some(format!("{prefix}__{raw}")),
                (_, other) => other,
            };
            let value = self.value(&node.expr, scoped_name.as_deref())?;
            if let Some(name) = &node.name {
                self.bindings.insert(name.clone(), value.clone());
            }
            if node.id == return_id {
                outputs = Some(output_ids(&value, node.span)?);
            }
        }
        self.bindings = saved;
        outputs.ok_or_else(|| at(decl.span, "declaration requires a return expression"))
    }

    pub(super) fn instantiate_part(
        &mut self,
        part_name: &str,
        overrides: &HashMap<String, IrValue>,
        span: Span,
    ) -> Result<IrValue> {
        if overrides.is_empty()
            && let Some(existing) = self.default_part_outputs.get(part_name)
        {
            return Ok(node_ref(existing.clone()));
        }
        let Some(part) = self.parts.get(part_name).cloned() else {
            return Err(at(span, format!("unknown part `{part_name}`")));
        };
        let instance_prefix = if overrides.is_empty() {
            None
        } else {
            self.part_instance_counter += 1;
            Some(format!("{part_name}__inst{}", self.part_instance_counter))
        };
        let outputs = self.lower_decl(&part, overrides, instance_prefix.as_deref())?;
        let [output] = outputs.as_slice() else {
            return Err(at(part.span, "a CAD-DSL part returns one Solid"));
        };
        if self.graph.nodes[&output.0].output_type != IrType::Solid {
            return Err(at(part.span, "a CAD-DSL part returns one Solid"));
        }
        if let Some(node) = self.graph.nodes.get_mut(&output.0) {
            node.metadata
                .insert("dsl.part_name".to_owned(), part.name.clone());
        }
        if overrides.is_empty() {
            self.default_part_outputs
                .insert(part_name.to_owned(), output.clone());
        }
        Ok(node_ref(output.clone()))
    }

    fn generated_id(&mut self) -> String {
        self.next_id += 1;
        format!("dsl:{}", self.next_id)
    }

    fn add_node(
        &mut self,
        name: Option<&str>,
        kind: NodeKind,
        output_type: IrType,
        inputs: Inputs,
        metadata: BTreeMap<String, String>,
    ) -> IrValue {
        let id = match name {
            Some(name) if !self.graph.nodes.contains_key(name) => name.to_owned(),
            _ => self.generated_id(),
        };
        let node_id = NodeId(id.clone());
        self.graph.nodes.insert(
            id.clone(),
            IrNode {
                id: node_id.clone(),
                name: name.unwrap_or(&id).to_owned(),
                kind,
                inputs,
                output_type,
                state: NodeState::Valid,
                kernel_op_version: None,
                metadata,
            },
        );
        IrValue::NodeRef {
            node_id,
            port: OUTPUT_PORT.to_owned(),
        }
    }

    fn add_parameter(
        &mut self,
        name: String,
        ty: IrType,
        value: IrValue,
        formula: Option<String>,
    ) -> IrValue {
        let param_id = name.as_str().into();
        self.graph.parameters.insert(
            name.clone(),
            ParameterDef {
                name: name.as_str().into(),
                ir_type: ty,
                value,
                formula,
            },
        );
        let reference_name = format!("param:{name}");
        self.add_node(
            Some(&reference_name),
            NodeKind::ParameterRef { param_id },
            ty,
            Inputs::new(),
            BTreeMap::new(),
        )
    }

    fn parameter(&self, value: &IrValue) -> Option<&ParameterDef> {
        let IrValue::NodeRef { node_id, .. } = value else {
            return None;
        };
        let NodeKind::ParameterRef { param_id } = &self.graph.nodes[&node_id.0].kind else {
            return None;
        };
        self.graph.parameters.get(&param_id.0)
    }
}
