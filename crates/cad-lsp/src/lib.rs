use cad_hir::{HoverInfo, ImportArgumentHover, IrExpr, IrSketchStmt, Severity, check_and_lower};
use cad_syntax::{Span, TopItem, parse};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspPosition {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspRange {
    pub start: LspPosition,
    pub end: LspPosition,
}

impl From<Span> for LspRange {
    fn from(s: Span) -> Self {
        Self {
            start: LspPosition {
                line: s.start_line.saturating_sub(1) as u32,
                character: s.start_col.saturating_sub(1) as u32,
            },
            end: LspPosition {
                line: s.end_line.saturating_sub(1) as u32,
                character: s.end_col.saturating_sub(1) as u32,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspDiagnostic {
    pub range: LspRange,
    pub severity: u8,
    pub code: String,
    pub source: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkupContent {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspHover {
    pub contents: MarkupContent,
    pub range: LspRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SymbolKind {
    Module = 2,
    Function = 12,
    Variable = 13,
    Struct = 23,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSymbol {
    pub name: String,
    pub detail: Option<String>,
    pub kind: u8,
    pub range: LspRange,
    pub selection_range: LspRange,
    pub children: Vec<DocumentSymbol>,
}

fn document_symbol(
    name: String,
    detail: Option<String>,
    kind: SymbolKind,
    span: Span,
    selection: Span,
) -> DocumentSymbol {
    DocumentSymbol {
        name,
        detail,
        kind: kind as u8,
        range: span.into(),
        selection_range: selection.into(),
        children: Vec::new(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CompletionItem {
    pub label: String,
    pub kind: &'static str,
    pub detail: String,
    pub insert_text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryHover {
    pub symbol: String,
    pub dimension_type: String,
    pub summary: String,
    pub span: Span,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueryResult {
    pub completions: Vec<CompletionItem>,
    pub variables: Vec<CompletionItem>,
    pub hover: Option<QueryHover>,
    pub symbols: Vec<DocumentSymbol>,
    pub diagnostics: Vec<LspDiagnostic>,
}

struct DocState {
    source: String,
    diagnostics: Vec<LspDiagnostic>,
    hover_items: Vec<HoverInfo>,
    import_argument_hovers: Vec<ImportArgumentHover>,
    symbols: Vec<DocumentSymbol>,
}

#[derive(Default)]
pub struct LspServerState {
    docs: HashMap<String, DocState>,
}

impl LspServerState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open_or_update(&mut self, uri: impl Into<String>, source: &str) {
        let uri = uri.into();
        let parsed = parse(source);
        let mut diags = Vec::new();

        for e in &parsed.lex_errors {
            diags.push(LspDiagnostic {
                range: e.span.into(),
                severity: 1,
                code: "E002_SYNTAX_ERROR".to_string(),
                source: "cad-syntax".to_string(),
                message: e.message.clone(),
            });
        }
        for e in &parsed.syntax_errors {
            diags.push(LspDiagnostic {
                range: e.span.into(),
                severity: 1,
                code: "E002_SYNTAX_ERROR".to_string(),
                source: "cad-syntax".to_string(),
                message: e.message.clone(),
            });
        }

        let hir = check_and_lower(&parsed.file);
        for d in &hir.diagnostics {
            let sev = match d.severity {
                Severity::Error => 1,
                Severity::Warning => 2,
                Severity::Info => 3,
            };
            diags.push(LspDiagnostic {
                range: d.span.into(),
                severity: sev,
                code: d.code.clone(),
                source: "cad-hir".to_string(),
                message: d.message.clone(),
            });
        }

        let mut hover_items = Vec::new();
        let mut symbols = Vec::new();

        for item in &parsed.file.items {
            let (name, span, params, detail, kind, parts) = match item {
                TopItem::Part(p) => (
                    &p.name,
                    p.span,
                    &p.params,
                    &p.return_type,
                    SymbolKind::Struct,
                    &hir.module.parts,
                ),
                TopItem::Assembly(p) => (
                    &p.name,
                    p.span,
                    &p.params,
                    &p.return_type,
                    SymbolKind::Module,
                    &hir.module.assemblies,
                ),
                TopItem::Const(c) => {
                    let ty = hir
                        .module
                        .constants
                        .iter()
                        .find(|node| node.span == c.span)
                        .map(|node| node.ty.to_string());
                    symbols.push(document_symbol(
                        c.name.clone(),
                        ty,
                        SymbolKind::Variable,
                        c.span,
                        c.name_span,
                    ));
                    continue;
                }
            };
            let mut children: Vec<_> = params
                .iter()
                .map(|p| {
                    document_symbol(
                        p.name.clone(),
                        Some(p.ty.clone()),
                        SymbolKind::Variable,
                        p.span,
                        p.span,
                    )
                })
                .collect();
            if let Some(part) = parts.iter().find(|p| p.span == span && p.name == *name) {
                hover_items.extend(part.hover_symbols.clone());
                for node in &part.nodes {
                    let mut nested = Vec::new();
                    sketch_symbols(&node.expr, &mut nested);
                    if let Some(name) = &node.name {
                        let mut binding = document_symbol(
                            name.clone(),
                            Some(node.ty.to_string()),
                            SymbolKind::Variable,
                            node.span,
                            name_selection(node.span, name, source),
                        );
                        binding.children = nested;
                        children.push(binding);
                    } else {
                        children.extend(nested);
                    }
                }
            }
            let mut symbol = document_symbol(name.clone(), detail.clone(), kind, span, span);
            symbol.children = children;
            symbols.push(symbol);
        }

        self.docs.insert(
            uri,
            DocState {
                source: source.to_owned(),
                diagnostics: diags,
                hover_items,
                import_argument_hovers: hir.import_argument_hovers,
                symbols,
            },
        );
    }

    pub fn diagnostics(&self, uri: &str) -> &[LspDiagnostic] {
        self.docs
            .get(uri)
            .map(|d| d.diagnostics.as_slice())
            .unwrap_or(&[])
    }

    pub fn hover(&self, uri: &str, pos: LspPosition) -> Option<LspHover> {
        let hit = self.query(uri, Some(pos))?.hover?;
        Some(LspHover {
            contents: MarkupContent {
                kind: "markdown".into(),
                value: hit.summary,
            },
            range: hit.span.into(),
        })
    }

    /// Query completions, hover and outline for an already-open document.
    /// LSP positions here are zero-based; the CLI translates its one-based flags.
    pub fn query(&self, uri: &str, pos: Option<LspPosition>) -> Option<QueryResult> {
        let doc = self.docs.get(uri)?;
        let token = pos.and_then(|pos| token_at(&doc.source, pos));
        let mut visible = BTreeMap::new();
        for symbol in &doc.symbols {
            if symbol.kind == SymbolKind::Variable as u8 {
                visible.insert(symbol.name.as_str(), symbol);
            }
        }
        if let Some((pos, scope)) = pos.and_then(|pos| {
            doc.symbols
                .iter()
                .find(|s| s.kind != SymbolKind::Variable as u8 && range_contains(s.range, pos))
                .map(|scope| (pos, scope))
        }) {
            visible_symbols(&scope.children, pos, &mut visible);
        }
        let variables: Vec<_> = visible
            .values()
            .map(|s| CompletionItem {
                label: s.name.clone(),
                kind: "variable",
                detail: s.detail.clone().unwrap_or_else(|| "Unknown".into()),
                insert_text: s.name.clone(),
            })
            .collect();
        let prefix = token
            .map(|(token, _)| token.trim_end_matches(':').to_ascii_lowercase())
            .unwrap_or_default();
        let matches_prefix =
            |item: &CompletionItem| item.label.to_ascii_lowercase().starts_with(&prefix);
        let completions = variables
            .iter()
            .filter(|c| matches_prefix(c))
            .cloned()
            .chain(
                OPERATORS
                    .iter()
                    .map(|op| op.completion())
                    .filter(matches_prefix),
            )
            .collect();
        let hover = pos.and_then(|pos| {
            if let Some(hit) = doc
                .import_argument_hovers
                .iter()
                .find(|hit| range_contains(hit.span.into(), pos))
            {
                let parameter = hit.parameter;
                return Some(QueryHover {
                    symbol: parameter.name.into(),
                    dimension_type: parameter.ty.to_string(),
                    summary: format!(
                        "**`{}: {}`**\n\n{}",
                        parameter.name, parameter.ty, parameter.documentation
                    ),
                    span: hit.span,
                });
            }
            let hit = doc.hover_item(pos.line as usize + 1, pos.character as usize + 1);
            if let Some((token, span)) = token {
                if let Some(op) = OPERATORS.iter().find(|op| op.name == token) {
                    return Some(QueryHover {
                        symbol: token.into(),
                        dimension_type: op.signature.into(),
                        summary: format!("{token}{}", op.signature),
                        span,
                    });
                }
                if let Some(sym) = visible.get(token) {
                    if let Some(hit) = hit.filter(|h| h.symbol == token) {
                        return Some(query_hover(hit));
                    }
                    let ty = sym.detail.clone().unwrap_or_else(|| "Unknown".into());
                    return Some(QueryHover {
                        symbol: token.into(),
                        summary: format!("{token}: {ty}"),
                        dimension_type: ty,
                        span,
                    });
                }
            }
            hit.map(query_hover)
        });
        Some(QueryResult {
            completions,
            variables,
            hover,
            symbols: doc.symbols.clone(),
            diagnostics: doc.diagnostics.clone(),
        })
    }

    pub fn document_symbols(&self, uri: &str) -> &[DocumentSymbol] {
        self.docs
            .get(uri)
            .map(|d| d.symbols.as_slice())
            .unwrap_or(&[])
    }
}

impl DocState {
    fn hover_item(&self, line: usize, col: usize) -> Option<&HoverInfo> {
        self.hover_items
            .iter()
            .filter(|h| h.span.contains_line_col(line, col))
            .min_by_key(|h| h.span.end_byte.saturating_sub(h.span.start_byte))
    }
}

fn query_hover(hit: &HoverInfo) -> QueryHover {
    QueryHover {
        symbol: hit.symbol.clone(),
        dimension_type: hit.dim_type.to_string(),
        summary: hover_text(hit),
        span: hit.span,
    }
}

fn hover_text(hit: &HoverInfo) -> String {
    let tier_note = match hit.sync_tier {
        Some(cad_hir::SyncTier::Tier1) => "Tier 1 (Direct Editable)",
        Some(cad_hir::SyncTier::Tier2) => "Tier 2 (Single-Variable Offset)",
        Some(cad_hir::SyncTier::Tier3) => "Tier 3 (Coupled Expression)",
        None => "Reference",
    };
    let unit_note = if hit.canonical_unit.is_empty() {
        String::new()
    } else {
        format!(" | Canonical Unit: `{}`", hit.canonical_unit)
    };
    format!(
        "**`{}`**: `{}`{unit_note}\n\nSync Policy: _{tier_note}_",
        hit.symbol, hit.dim_type
    )
}

fn sketch_symbols(expr: &IrExpr, output: &mut Vec<DocumentSymbol>) {
    match expr {
        IrExpr::Sketch { stmts, span, .. } => {
            let mut sketch = document_symbol(
                "sketch".into(),
                Some("Sketch2D".into()),
                SymbolKind::Module,
                *span,
                *span,
            );
            for stmt in stmts {
                match stmt {
                    IrSketchStmt::Let { name, expr, span } => {
                        let mut local = document_symbol(
                            name.clone(),
                            Some(expr.ty().to_string()),
                            SymbolKind::Variable,
                            *span,
                            *span,
                        );
                        sketch_symbols(expr, &mut local.children);
                        sketch.children.push(local);
                    }
                    IrSketchStmt::Expr { expr, .. } => sketch_symbols(expr, &mut sketch.children),
                }
            }
            output.push(sketch);
        }
        IrExpr::Array { elements, .. } => elements.iter().for_each(|e| sketch_symbols(e, output)),
        IrExpr::Unary { operand, .. } => sketch_symbols(operand, output),
        IrExpr::Binary { left, right, .. } | IrExpr::Pipe { left, right, .. } => {
            sketch_symbols(left, output);
            sketch_symbols(right, output);
        }
        IrExpr::Call { args, .. } => args.iter().for_each(|a| sketch_symbols(&a.value, output)),
        IrExpr::FieldAccess { receiver, .. } => sketch_symbols(receiver, output),
        IrExpr::MethodCall { receiver, args, .. } => {
            sketch_symbols(receiver, output);
            args.iter().for_each(|a| sketch_symbols(&a.value, output));
        }
        _ => {}
    }
}

fn name_selection(span: Span, name: &str, source: &str) -> Span {
    let Some(decl) = source.get(span.start_byte..span.end_byte) else {
        return span;
    };
    let line = decl.lines().next().unwrap_or_default();
    let Some(keyword_end) = line.find(char::is_whitespace) else {
        return span;
    };
    let rest = line[keyword_end..].trim_start();
    if !rest.starts_with(name) {
        return span;
    }
    let offset = line.len() - rest.len();
    let col = span.start_col + line[..offset].chars().count();
    Span::new(
        span.start_line,
        col,
        span.start_line,
        col + name.len(),
        span.start_byte + offset,
        span.start_byte + offset + name.len(),
    )
}

fn visible_symbols<'a>(
    symbols: &'a [DocumentSymbol],
    pos: LspPosition,
    visible: &mut BTreeMap<&'a str, &'a DocumentSymbol>,
) {
    for symbol in symbols {
        if symbol.kind == SymbolKind::Variable as u8
            && (position_key(symbol.range.end) <= position_key(pos)
                || range_contains(symbol.selection_range, pos)
                    && position_key(symbol.selection_range.end) != position_key(symbol.range.end))
        {
            visible.insert(symbol.name.as_str(), symbol);
        }
        if range_contains(symbol.range, pos) {
            visible_symbols(&symbol.children, pos, visible);
        }
    }
}

fn position_key(p: LspPosition) -> (u32, u32) {
    (p.line, p.character)
}

fn range_contains(range: LspRange, p: LspPosition) -> bool {
    let p = position_key(p);
    position_key(range.start) <= p && p < position_key(range.end)
}

fn token_at(source: &str, pos: LspPosition) -> Option<(&str, Span)> {
    let line = source.lines().nth(pos.line as usize)?;
    let col = pos.character as usize;
    let byte = line
        .char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .or_else(|| (line.chars().count() == col).then_some(line.len()))?;
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let bytes = line.as_bytes();
    let mut lo = byte;
    let mut hi = byte;
    while lo > 0 {
        if is_word(bytes[lo - 1]) {
            lo -= 1;
        } else if lo >= 2 && &bytes[lo - 2..lo] == b"::" {
            lo -= 2;
        } else {
            break;
        }
    }
    while hi < bytes.len() {
        if is_word(bytes[hi]) {
            hi += 1;
        } else if bytes[hi..].starts_with(b"::") {
            hi += 2;
        } else {
            break;
        }
    }
    let token = &line[lo..hi];
    if token.is_empty() || !token.as_bytes()[0].is_ascii_alphabetic() && !token.starts_with('_') {
        return None;
    }
    let line_num = pos.line as usize + 1;
    let start_col = line[..lo].chars().count() + 1;
    Some((
        token,
        Span::new(line_num, start_col, line_num, start_col + token.len(), 0, 0),
    ))
}

struct Builtin {
    name: &'static str,
    signature: &'static str,
}
impl Builtin {
    fn completion(&self) -> CompletionItem {
        CompletionItem {
            label: self.name.into(),
            kind: if self.name == "assembly" || self.name == "part" {
                "keyword"
            } else if self.name.contains("::") {
                "namespace"
            } else {
                "operator"
            },
            detail: self.signature.into(),
            insert_text: if self.name == "assembly" || self.name == "part" {
                self.name.into()
            } else {
                format!("{}(", self.name)
            },
        }
    }
}

macro_rules! builtins {
    ($($name:literal => $signature:literal),* $(,)?) => {
        const OPERATORS: &[Builtin] = &[$(Builtin { name: $name, signature: $signature }),*];
    };
}

builtins! {
    "part" => " Name(params) -> Solid { ... }",
    "assembly" => " Name(params) -> Assembly { ... }",
    "box" => "(size: Length[3], origin?: Length[3]) -> Solid",
    "cylinder" => "(d: Length, h: Length, origin?: Length[3], axis?: Direction) -> Solid",
    "sketch" => "(plane: Plane) { ... } -> Sketch2D",
    "extrude" => "(h: Length) -> Solid",
    "extrude_symmetric" => "(thickness: Length) -> Solid",
    "revolve" => "(angle?: Angle, axis?: Direction) -> Solid",
    "sweep" => "(profile: Sketch2D, path: Length[3][]) -> Solid",
    "loft" => "(sections: Sketch2D[], height?: Length, offsets?: Length[]) -> Solid",
    "point" => "(x: Length, y: Length) -> SketchPoint",
    "line" => "(from: SketchPoint, to: SketchPoint) -> SketchEntity",
    "arc" => "(from: SketchPoint, to: SketchPoint) -> SketchEntity",
    "circle" => "(center: Length[2], d: Length) -> SketchEntity",
    "ellipse" => "(rx: Length, ry: Length, center?: Length[2], segments?: u32) -> SketchEntity",
    "profile::ellipse" => "(rx: Length, ry: Length, center?: Length[2], segments?: u32, plane?: Plane) -> Sketch2D",
    "spline" => "(points: Length[2][], closed?: Bool, segments_per_span?: u32) -> SketchEntity",
    "profile::spline" => "(points: Length[2][], closed?: Bool, segments_per_span?: u32, plane?: Plane) -> Sketch2D",
    "path::helix" => "(radius: Length, pitch: Length, turns?: f64, height?: Length, origin?: Length[3], segments_per_turn?: u32, left_handed?: Bool) -> Path3D",
    "path::spline3d" => "(points: Length[3][], segments_per_span?: u32) -> Path3D",
    "region::closed" => "(SketchEntity[]) -> Sketch2D",
    "region::path" => "(SketchEntity[]) -> Path",
    "shell" => "(thickness: Length, open_faces?: FaceSet[]) -> Solid",
    "mirror" => "(plane: Plane, merge?: Bool) -> Solid",
    "fillet" => "(r: Length) -> Solid",
    "chamfer" => "(d: Length) -> Solid",
    "boolean::union" => "(items: Solid[]) -> Solid",
    "boolean::subtract" => "(tools: Solid[], preserve_exact_interfaces?: Bool) -> Solid",
    "hole::simple" => "(d: Length, depth: Length, origin?: Length[3]) -> Solid",
    "hole::counterbore" => "(d_through: Length, d_cbore: Length, depth_cbore: Length) -> Solid",
    "hole::tapped" => "(spec: String, depth: Length, origin?: Length[3]) -> Solid",
    "array::linear" => "(feature: Solid, count: u32, spacing: Length, axis?: Direction) -> Solid",
    "array::polar" => "(feature: Solid, count: u32, axis?: Direction) -> Solid",
    "component" => "(part: Solid, origin?: Length[3], rotation?: Angle[3]) -> Component",
    "mate::coaxial" => "(a: Component, b: Component, axis?: Direction) -> Bool",
    "mate::coincident" => "(a: Component, b: Component, offset?: Length) -> Bool",
    "import::step" => "(path: String, sha256?: String, deflection?: Length, component?: String, allow_open_shell?: Bool, heal?: Bool, sewing_tol?: Length) -> Solid",
    "import::mesh" => "(path: String, source_unit?: String, sha256?: String, cluster_analytic_faces?: Bool, allow_open_shell?: Bool) -> Solid",
    "tag::faces" => "(tag: String, faces?: FaceSet) -> Solid | FaceSet (matches pipe input)",
    "face::offset" => "Solid |> (faces: FaceSet, delta: Length) -> Solid; select one full cylindrical through-hole wall from the input solid. Positive delta grows the radius.",
    "face::move" => "Solid |> (faces: FaceSet, offset: [Length, Length, Length]) -> Solid; move a full cylindrical through-hole perpendicular to its axis.",
    "face::delete" => "Solid |> (faces: FaceSet) -> Solid; fill a full cylindrical through-hole on the input solid.",
    "pmi::datum" => "(label: String)",
    "pmi::flatness" => "(tol: Length)",
    "pmi::perpendicularity" => "(datum: String, tol: Length)",
    "assert::bbox" => "(target: Solid, max_size: Length[3]) -> Bool",
    "assert::no_interference" => "(a: Solid | Component, b: Solid | Component, min_clearance: Length) -> Bool; requires exact OCCT solids. Use 0mm for overlap only.",
}

impl LspServerState {
    /// Dispatches a minimal JSON-RPC 2.0 request and returns a serialized JSON-RPC response.
    pub fn handle_json_rpc(&mut self, request_json: &str) -> String {
        let req: serde_json::Value = match serde_json::from_str(request_json) {
            Ok(v) => v,
            Err(e) => {
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                })
                .to_string();
            }
        };
        let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let result = match method {
            "textDocument/didOpen" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                let text = params["textDocument"]["text"].as_str().unwrap_or("");
                self.open_or_update(uri, text);
                serde_json::json!({ "diagnostics": self.diagnostics(uri) })
            }
            "textDocument/completion" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                let line = params["position"]["line"].as_u64().unwrap_or(0) as u32;
                let character = params["position"]["character"].as_u64().unwrap_or(0) as u32;
                let items = self
                    .query(uri, Some(LspPosition { line, character }))
                    .map(|q| q.completions)
                    .unwrap_or_default();
                serde_json::Value::Array(items.into_iter().map(|item| serde_json::json!({
                    "label": item.label,
                    "kind": match item.kind { "variable" => 6, "keyword" => 14, _ => 3 },
                    "detail": item.detail,
                    "insertText": item.insert_text,
                })).collect())
            }
            "textDocument/hover" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                let line = params["position"]["line"].as_u64().unwrap_or(0) as u32;
                let character = params["position"]["character"].as_u64().unwrap_or(0) as u32;
                serde_json::to_value(self.hover(uri, LspPosition { line, character }))
                    .unwrap_or(serde_json::Value::Null)
            }
            "textDocument/documentSymbol" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                serde_json::to_value(self.document_symbols(uri))
                    .unwrap_or(serde_json::Value::Array(Vec::new()))
            }
            _ => serde_json::Value::Null,
        };

        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        })
        .to_string()
    }
}
