//! Shared evaluation-routing policy between KCL/DSL sources, `CanonicalGraph`,
//! and `GraphEvaluator`.
//!
//! `cad-evaluator`, `cad-cli`, and the desktop host (`apps/desktop/src-tauri`)
//! use this module as the single source of truth for:
//! - Detecting `.kcl 2.0` (`@ir_schema(...)`) vs CAD-DSL v0.2 (`part`/`assembly`)
//! - Checking whether a source lowers losslessly to a `GraphEvaluator`-ready graph
//! - Recording deterministic, structured diagnostic reasons when a source cannot
//!   be evaluated by `GraphEvaluator`
//! - Supporting `--eval-engine graph` / `CAD_EVAL_ENGINE=graph` configuration.

use std::collections::BTreeSet;

use cad_hir::{Diagnostic, Severity};
use cad_ir::{
    CanonicalGraph, ComponentSourceSpec, FEATURE_SPEC_PORT, FeatureSpec, IrValue, NodeKind,
};
use cad_syntax::{Span, TokenKind, tokenize};

use crate::{BridgeError, parse_any_kcl_to_graph};

/// Environment variable selecting the unified evaluation mode (`auto` | `graph`).
pub const EVAL_ENGINE_ENV: &str = "CAD_EVAL_ENGINE";

/// Evaluation engine policy for KCL/DSL source compilation.
///
/// The legacy HIR evaluator has been retired; both `Auto` and `GraphOnly`
/// evaluate exclusively through `CanonicalGraph` + `GraphEvaluator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EvalEngineMode {
    /// Unified single-path `GraphEvaluator` execution (default).
    #[default]
    Auto,
    /// Explicit `GraphEvaluator`-only mode (equivalent to `Auto` now that the
    /// legacy evaluator is retired).
    GraphOnly,
}

impl EvalEngineMode {
    /// Parse an explicit CLI flag or environment variable value.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "unified" => Ok(Self::Auto),
            "graph" | "graph-only" | "graph_only" | "v2" => Ok(Self::GraphOnly),
            other => Err(format!(
                "invalid eval engine `{other}`: expected `auto` or `graph`"
            )),
        }
    }

    /// Resolve mode from `CAD_EVAL_ENGINE`, defaulting to `EvalEngineMode::Auto`.
    pub fn from_env() -> Result<Self, String> {
        match std::env::var(EVAL_ENGINE_ENV) {
            Ok(val) if !val.trim().is_empty() => Self::parse(&val),
            _ => Ok(Self::Auto),
        }
    }

    /// Resolve an optional CLI flag override, falling back to `CAD_EVAL_ENGINE` and then `Auto`.
    pub fn resolve(cli_override: Option<&str>) -> Result<Self, String> {
        if let Some(raw) = cli_override {
            return Self::parse(raw);
        }
        Self::from_env()
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::GraphOnly => "graph",
        }
    }
}

/// Which KCL syntax dialect a source text uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KclSourceFlavor {
    /// `.kcl 2.0` canonical graph view (`@ir_schema(...)`).
    CanonicalKclV2,
    /// CAD-DSL v0.2 (`part` / `assembly`).
    DslV02,
    /// Unrecognized or incomplete source text.
    Unknown,
}

impl KclSourceFlavor {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CanonicalKclV2 => "kcl_v2",
            Self::DslV02 => "dsl_v02",
            Self::Unknown => "unknown",
        }
    }
}

/// Which evaluator path actually executed the compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalPath {
    /// Evaluated via `CanonicalGraph` -> `GraphEvaluator`.
    GraphEvaluator,
}

impl EvalPath {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GraphEvaluator => "graph_evaluator",
        }
    }
}

/// Result of inspecting a KCL/DSL source for `GraphEvaluator` readiness.
#[derive(Debug, Clone)]
pub struct GraphReadinessReport {
    pub flavor: KclSourceFlavor,
    pub graph: Option<CanonicalGraph>,
    pub bridge_error: Option<BridgeError>,
    /// Deterministic list of reasons why `GraphEvaluator` cannot yet evaluate
    /// this source with full legacy parity. Empty when graph-ready.
    pub fallback_reasons: Vec<String>,
}

impl GraphReadinessReport {
    /// Returns `true` when `graph` is present and has no known `GraphEvaluator` gaps.
    pub fn is_graph_ready(&self) -> bool {
        self.graph.is_some() && self.fallback_reasons.is_empty()
    }
}

/// Token-based dialect detection matching [`parse_any_kcl_to_graph`].
pub fn detect_source_flavor(source: &str) -> KclSourceFlavor {
    use TokenKind::*;
    let source = source.trim_start_matches('\u{feff}');
    let (tokens, errors) = tokenize(source);
    if !errors.is_empty() {
        return KclSourceFlavor::Unknown;
    }
    let mut depth = 0usize;
    for pair in tokens.windows(2) {
        match &pair[0].kind {
            At if depth == 0 && matches!(&pair[1].kind, Ident(name) if name == "ir_schema") => {
                return KclSourceFlavor::CanonicalKclV2;
            }
            Part | Assembly if depth == 0 => return KclSourceFlavor::DslV02,
            LParen | LBrace | LBracket => depth += 1,
            RParen | RBrace | RBracket => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    KclSourceFlavor::Unknown
}

/// Lower or parse `source` and audit whether `GraphEvaluator` can evaluate it
/// without dropping legacy semantics.
pub fn inspect_graph_eval_readiness(source: &str) -> GraphReadinessReport {
    let flavor = detect_source_flavor(source);
    match parse_any_kcl_to_graph(source) {
        Err(err) => {
            let reason = format!("bridge_lower_failed: {err}");
            GraphReadinessReport {
                flavor,
                graph: None,
                bridge_error: Some(err),
                fallback_reasons: vec![reason],
            }
        }
        Ok(graph) => {
            // Native `.kcl 2.0` graphs are authored directly for `GraphEvaluator`
            // and can never fall back to the legacy DSL v0.2 evaluator.
            if flavor == KclSourceFlavor::CanonicalKclV2 {
                return GraphReadinessReport {
                    flavor,
                    graph: Some(graph),
                    bridge_error: None,
                    fallback_reasons: Vec::new(),
                };
            }
            let fallback_reasons = collect_dsl_graph_gaps(&graph);
            GraphReadinessReport {
                flavor,
                graph: Some(graph),
                bridge_error: None,
                fallback_reasons,
            }
        }
    }
}

fn collect_dsl_graph_gaps(graph: &CanonicalGraph) -> Vec<String> {
    let mut reasons = BTreeSet::new();

    if graph.outputs.is_empty() {
        reasons.insert("empty_graph_outputs".to_owned());
    }

    for node in graph.nodes.values() {
        let has_spec = node.inputs.contains_key(FEATURE_SPEC_PORT);
        match &node.kind {
            NodeKind::AssemblyComponent
                if !node.inputs.contains_key("external_ref")
                    && !local_component_spec_matches(graph, node) =>
            {
                reasons.insert("local_assembly_component_unsupported_in_graph_eval".to_owned());
            }
            NodeKind::ImportAsset
                if !matches!(node.inputs.get("blob"), Some(IrValue::BlobRef(_)))
                    && !matches!(
                        node.inputs.get("path"),
                        Some(IrValue::String(p)) if !p.trim().is_empty()
                    ) =>
            {
                reasons.insert("import_asset_fs_path_unsupported_in_graph_eval".to_owned());
            }
            NodeKind::Hole if !has_spec && !node.inputs.contains_key("solid") => {
                reasons.insert("standalone_hole_tool_unsupported_in_graph_eval".to_owned());
            }
            NodeKind::Sketch2D | NodeKind::Mirror | NodeKind::Sweep | NodeKind::LoftGuided
                if !has_spec =>
            {
                reasons.insert(format!("{:?}_missing_feature_spec", node.kind));
            }
            NodeKind::StdLibCall if !has_spec => {
                let func = match node
                    .inputs
                    .get("function")
                    .or_else(|| node.inputs.get("spec"))
                {
                    Some(IrValue::String(name)) => name.as_str(),
                    _ => "unknown",
                };
                if !matches!(
                    func,
                    "pmi::datum"
                        | "pmi::perpendicularity"
                        | "pmi::flatness"
                        | "pmi::tolerance"
                        | "pmi::thread"
                        | "force::distributed"
                        | "field::stress_driven"
                ) {
                    reasons.insert(format!("stdlib_call_unsupported_in_graph_eval:{func}"));
                }
            }
            NodeKind::Shell | NodeKind::Fillet | NodeKind::Chamfer => {
                for port in ["open_faces", "edges"] {
                    if let Some(val) = node.inputs.get(port)
                        && contains_node_ref(val)
                    {
                        reasons.insert(format!(
                            "{:?}_selector_node_ref_unsupported_in_graph_eval",
                            node.kind
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    reasons.into_iter().collect()
}

/// A local component is graph-evaluable once its `ComponentInstance` spec names
/// the same geometry node and the same nominal pose as the legacy ports.
/// `GraphEvaluator` still places the component from `origin` and `rotation`.
fn local_component_spec_matches(graph: &CanonicalGraph, node: &cad_ir::IrNode) -> bool {
    let Ok(FeatureSpec::ComponentInstance {
        source: ComponentSourceSpec::LocalNode { node_id },
        translation_mm,
        rotation_deg,
        ..
    }) = node.try_feature_spec()
    else {
        return false;
    };
    let Some(IrValue::NodeRef {
        node_id: geometry, ..
    }) = node.inputs.get("geometry")
    else {
        return false;
    };
    if geometry != &node_id {
        return false;
    }
    pose_matches(graph, node.inputs.get("origin"), translation_mm)
        && pose_matches(graph, node.inputs.get("rotation"), rotation_deg)
}

fn pose_matches(graph: &CanonicalGraph, port: Option<&IrValue>, spec: [f64; 3]) -> bool {
    let Some(port) = port else {
        return spec == [0.0, 0.0, 0.0];
    };
    nominal_vec3(graph, port) == Some(spec)
}

fn nominal_vec3(graph: &CanonicalGraph, value: &IrValue) -> Option<[f64; 3]> {
    let resolved = if let IrValue::NodeRef { node_id, .. } = value {
        let node = graph.nodes.get(&node_id.0)?;
        let NodeKind::ParameterRef { param_id } = &node.kind else {
            return None;
        };
        &graph.parameters.get(&param_id.0)?.value
    } else {
        value
    };
    match resolved {
        IrValue::Vec3(point) => Some(*point),
        _ => None,
    }
}

fn contains_node_ref(value: &IrValue) -> bool {
    match value {
        IrValue::NodeRef { .. } => true,
        IrValue::List(items) => items.iter().any(contains_node_ref),
        IrValue::Map(entries) => entries.values().any(contains_node_ref),
        _ => false,
    }
}

/// Convert a [`BridgeError`] into a structured [`cad_hir::Diagnostic`], preserving
/// any `E...:` error code prefix emitted by HIR checking.
pub fn bridge_error_diagnostic(err: &BridgeError) -> Diagnostic {
    let BridgeError::At {
        line,
        column,
        message,
    } = err;
    let (code, detail) = match message.split_once(": ") {
        Some((prefix, rest))
            if prefix.starts_with('E')
                && prefix
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') =>
        {
            (prefix.to_owned(), rest.to_owned())
        }
        _ => ("E002_SYNTAX_ERROR".to_owned(), message.clone()),
    };
    Diagnostic {
        code,
        severity: Severity::Error,
        message: detail,
        span: Span {
            start_line: *line,
            start_col: *column,
            end_line: *line,
            end_col: *column,
            start_byte: 0,
            end_byte: 0,
        },
    }
}

/// Build a structured [`cad_hir::Diagnostic`] with a default source span.
pub fn diagnostic_error(code: impl Into<String>, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: code.into(),
        severity: Severity::Error,
        message: message.into(),
        span: Span::default(),
    }
}
