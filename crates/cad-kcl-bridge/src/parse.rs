//! `.kcl 2.0` text -> validated `CanonicalGraph`.

use std::collections::{BTreeMap, BTreeSet};

use cad_ir::{
    BlobHandle, BlobManifestEntry, CanonicalGraph, ExternalRefDef, ExternalRefId,
    IR_SCHEMA_VERSION, IrError, IrNode, IrType, IrValue, NodeId, NodeKind, NodeState, OUTPUT_PORT,
    ParamId, ParameterDef, PayloadPolicy, UnitSystem, validate_graph,
};
use cad_syntax::{Token, TokenKind, tokenize};

use crate::BridgeError;
use crate::model::{
    DecodedOp, SuffixParse, angle_unit_from_name, blob_handle_from_keyword, blob_kind_from_name,
    decode_operator, force_unit_from_name, length_unit_from_name, mass_unit_from_name,
    payload_policy_from_name, pressure_unit_from_name, quantity_from_suffix, type_from_name,
    type_name,
};

/// Parse a `.kcl 2.0` view into a validated canonical graph.
///
/// The result keeps `graph_id`, units, parameters, blobs, external references,
/// nodes (including kind, inputs, output type, and state), and outputs. Syntax
/// and validation failures include a 1-based line and column.
pub fn parse_kcl_to_graph(source: &str) -> Result<CanonicalGraph, BridgeError> {
    let (tokens, lex_errors) = tokenize(source);
    if let Some(error) = lex_errors.first() {
        return Err(BridgeError::at(
            error.span.start_line,
            error.span.start_col,
            error.message.clone(),
        ));
    }
    Parser::new(&tokens).parse_file()
}

struct Arg {
    name: Option<String>,
    value: IrValue,
    line: usize,
    column: usize,
}

struct NumberTok {
    value: f64,
    raw: String,
    unit: Option<String>,
}

struct Pending {
    state: Option<NodeState>,
    name: Option<String>,
    kernel: Option<String>,
    meta: BTreeMap<String, String>,
    has_meta: bool,
}

impl Pending {
    fn new() -> Self {
        Self {
            state: None,
            name: None,
            kernel: None,
            meta: BTreeMap::new(),
            has_meta: false,
        }
    }

    fn is_empty(&self) -> bool {
        self.state.is_none() && self.name.is_none() && self.kernel.is_none() && !self.has_meta
    }

    fn take(&mut self) -> Self {
        std::mem::replace(self, Self::new())
    }
}

struct Locations {
    nodes: BTreeMap<String, (usize, usize)>,
    params: BTreeMap<String, (usize, usize)>,
    blobs: BTreeMap<String, (usize, usize)>,
    extrefs: BTreeMap<String, (usize, usize)>,
    node_refs: BTreeMap<String, (usize, usize)>,
    param_uses: BTreeMap<String, (usize, usize)>,
    blob_uses: BTreeMap<String, (usize, usize)>,
    extref_uses: BTreeMap<String, (usize, usize)>,
    graph: (usize, usize),
    ret: (usize, usize),
}

impl Locations {
    fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
            params: BTreeMap::new(),
            blobs: BTreeMap::new(),
            extrefs: BTreeMap::new(),
            node_refs: BTreeMap::new(),
            param_uses: BTreeMap::new(),
            blob_uses: BTreeMap::new(),
            extref_uses: BTreeMap::new(),
            graph: (1, 1),
            ret: (1, 1),
        }
    }
}

struct Parser<'a> {
    tokens: &'a [Token],
    cursor: usize,
    schema: Option<u32>,
    graph_id: Option<String>,
    units: Option<UnitSystem>,
    parameters: BTreeMap<String, ParameterDef>,
    blobs: BTreeMap<String, BlobManifestEntry>,
    external_refs: BTreeMap<String, ExternalRefDef>,
    nodes: BTreeMap<String, IrNode>,
    outputs: Vec<NodeId>,
    pending: Pending,
    locs: Locations,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token]) -> Self {
        Self {
            tokens,
            cursor: 0,
            schema: None,
            graph_id: None,
            units: None,
            parameters: BTreeMap::new(),
            blobs: BTreeMap::new(),
            external_refs: BTreeMap::new(),
            nodes: BTreeMap::new(),
            outputs: Vec::new(),
            pending: Pending::new(),
            locs: Locations::new(),
        }
    }

    fn parse_file(mut self) -> Result<CanonicalGraph, BridgeError> {
        loop {
            if self.at(|kind| matches!(kind, TokenKind::Eof)) {
                return Err(self.unexpected("`return`"));
            }
            if self.at(|kind| matches!(kind, TokenKind::At)) {
                self.parse_at_item()?;
                continue;
            }
            if self.peek_ident() == Some("param") {
                if !self.pending.is_empty() {
                    return Err(self.error("a node annotation must be followed by `let`"));
                }
                self.parse_param()?;
                continue;
            }
            if self.at(|kind| matches!(kind, TokenKind::Let)) {
                self.parse_let()?;
                continue;
            }
            if self.at(|kind| matches!(kind, TokenKind::Return)) {
                if !self.pending.is_empty() {
                    return Err(self.error("a node annotation must be followed by `let`"));
                }
                self.parse_return()?;
                break;
            }
            return Err(self.unexpected("a declaration"));
        }
        if !self.at(|kind| matches!(kind, TokenKind::Eof)) {
            return Err(self.unexpected("end of file"));
        }
        self.finish()
    }

    fn parse_at_item(&mut self) -> Result<(), BridgeError> {
        let at = self.location();
        self.bump();
        let name = self.take_ident()?;
        match name.as_str() {
            "ir_schema" => self.parse_schema(at),
            "graph_id" => self.parse_graph_id(at),
            "unit" => self.parse_units(at),
            "blob" => self.parse_blob(at),
            "extref" => self.parse_extref(at),
            "suppressed" => {
                self.finish_marker()?;
                self.pending.set_state(NodeState::Suppressed, at)?;
                Ok(())
            }
            "stale" => {
                self.finish_marker()?;
                self.pending.set_state(NodeState::Stale, at)?;
                Ok(())
            }
            "failed" => {
                let state = self.parse_failed()?;
                self.pending.set_state(state, at)?;
                Ok(())
            }
            "name" => {
                let name = self.parse_wrapped_string()?;
                self.pending.set_name(name, at)?;
                Ok(())
            }
            "kernel" => {
                let kernel = self.parse_wrapped_string()?;
                self.pending.set_kernel(kernel, at)?;
                Ok(())
            }
            "meta" => {
                let meta = self.parse_string_map("metadata key")?;
                self.pending.set_meta(meta, at)?;
                Ok(())
            }
            "formula" => Err(self.error("put `@formula(\"...\")` after a parameter value")),
            other => Err(BridgeError::at(
                at.0,
                at.1,
                format!("unknown annotation `@{other}`"),
            )),
        }
    }

    fn ensure_annotations_closed(&self, at: (usize, usize)) -> Result<(), BridgeError> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(BridgeError::at(
                at.0,
                at.1,
                "a node annotation must be followed by `let`",
            ))
        }
    }

    fn parse_schema(&mut self, at: (usize, usize)) -> Result<(), BridgeError> {
        self.ensure_annotations_closed(at)?;
        if self.schema.is_some() {
            return Err(BridgeError::at(at.0, at.1, "duplicate `@ir_schema`"));
        }
        self.schema = Some(self.parse_wrapped_u32()?);
        Ok(())
    }

    fn parse_graph_id(&mut self, at: (usize, usize)) -> Result<(), BridgeError> {
        self.ensure_annotations_closed(at)?;
        if self.graph_id.is_some() {
            return Err(BridgeError::at(at.0, at.1, "duplicate `@graph_id`"));
        }
        let id = self.parse_wrapped_string()?;
        self.locs.graph = at;
        self.graph_id = Some(id);
        Ok(())
    }

    fn parse_units(&mut self, at: (usize, usize)) -> Result<(), BridgeError> {
        self.ensure_annotations_closed(at)?;
        if self.units.is_some() {
            return Err(BridgeError::at(at.0, at.1, "duplicate `@unit`"));
        }
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let mut length = None;
        let mut angle = None;
        let mut force = None;
        let mut pressure = None;
        let mut mass = None;
        if !self.at(|kind| matches!(kind, TokenKind::RParen)) {
            loop {
                let key_at = self.location();
                let key = self.take_ident()?;
                self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
                let value_at = self.location();
                let value = self.take_ident()?;
                match key.as_str() {
                    "length" => {
                        let unit = length_unit_from_name(&value).ok_or_else(|| {
                            BridgeError::at(
                                value_at.0,
                                value_at.1,
                                format!("unknown length unit `{value}`"),
                            )
                        })?;
                        assign_once(&mut length, unit, "length", key_at)?;
                    }
                    "angle" => {
                        let unit = angle_unit_from_name(&value).ok_or_else(|| {
                            BridgeError::at(
                                value_at.0,
                                value_at.1,
                                format!("unknown angle unit `{value}`"),
                            )
                        })?;
                        assign_once(&mut angle, unit, "angle", key_at)?;
                    }
                    "force" => {
                        let unit = force_unit_from_name(&value).ok_or_else(|| {
                            BridgeError::at(
                                value_at.0,
                                value_at.1,
                                format!("unknown force unit `{value}`"),
                            )
                        })?;
                        assign_once(&mut force, unit, "force", key_at)?;
                    }
                    "pressure" => {
                        let unit = pressure_unit_from_name(&value).ok_or_else(|| {
                            BridgeError::at(
                                value_at.0,
                                value_at.1,
                                format!("unknown pressure unit `{value}`"),
                            )
                        })?;
                        assign_once(&mut pressure, unit, "pressure", key_at)?;
                    }
                    "mass" => {
                        let unit = mass_unit_from_name(&value).ok_or_else(|| {
                            BridgeError::at(
                                value_at.0,
                                value_at.1,
                                format!("unknown mass unit `{value}`"),
                            )
                        })?;
                        assign_once(&mut mass, unit, "mass", key_at)?;
                    }
                    other => {
                        return Err(BridgeError::at(
                            key_at.0,
                            key_at.1,
                            format!("unknown unit field `{other}`"),
                        ));
                    }
                }
                if !self.continue_list(|kind| matches!(kind, TokenKind::RParen))? {
                    break;
                }
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        self.units = Some(UnitSystem {
            length: length
                .ok_or_else(|| BridgeError::at(at.0, at.1, "`@unit` is missing `length`"))?,
            angle: angle
                .ok_or_else(|| BridgeError::at(at.0, at.1, "`@unit` is missing `angle`"))?,
            force: force
                .ok_or_else(|| BridgeError::at(at.0, at.1, "`@unit` is missing `force`"))?,
            pressure: pressure
                .ok_or_else(|| BridgeError::at(at.0, at.1, "`@unit` is missing `pressure`"))?,
            mass: mass.ok_or_else(|| BridgeError::at(at.0, at.1, "`@unit` is missing `mass`"))?,
        });
        Ok(())
    }

    fn parse_blob(&mut self, at: (usize, usize)) -> Result<(), BridgeError> {
        self.ensure_annotations_closed(at)?;
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let handle = self.take_string()?;
        if handle.trim().is_empty() {
            return Err(BridgeError::at(at.0, at.1, "blob handle is empty"));
        }
        self.expect(|kind| matches!(kind, TokenKind::Comma), "`,`")?;
        let mut kind = None;
        let mut sha256 = None;
        let mut path = None;
        let mut bytes = None;
        if !self.at(|kind| matches!(kind, TokenKind::RParen)) {
            loop {
                let key_at = self.location();
                let key = self.take_ident()?;
                self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
                match key.as_str() {
                    "kind" => {
                        let name_at = self.location();
                        let name = self.take_string()?;
                        let parsed = blob_kind_from_name(&name).ok_or_else(|| {
                            BridgeError::at(
                                name_at.0,
                                name_at.1,
                                format!("unknown blob kind `{name}`"),
                            )
                        })?;
                        assign_once(&mut kind, parsed, "kind", key_at)?;
                    }
                    "sha256" => assign_once(&mut sha256, self.take_string()?, "sha256", key_at)?,
                    "path" => assign_once(&mut path, self.take_string()?, "path", key_at)?,
                    "bytes" => {
                        let number_at = self.location();
                        let number = self.take_number()?;
                        if number.unit.is_some() || is_float_literal(&number.raw) {
                            return Err(BridgeError::at(
                                number_at.0,
                                number_at.1,
                                "byte length must be an integer",
                            ));
                        }
                        let len = parse_u64_literal(&number.raw).map_err(|message| {
                            BridgeError::at(number_at.0, number_at.1, message)
                        })?;
                        assign_once(&mut bytes, len, "bytes", key_at)?;
                    }
                    other => {
                        return Err(BridgeError::at(
                            key_at.0,
                            key_at.1,
                            format!("unknown blob field `{other}`"),
                        ));
                    }
                }
                if !self.continue_list(|kind| matches!(kind, TokenKind::RParen))? {
                    break;
                }
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        if self.blobs.contains_key(&handle) {
            return Err(BridgeError::at(
                at.0,
                at.1,
                format!("duplicate blob `{handle}`"),
            ));
        }
        self.locs.blobs.insert(handle.clone(), at);
        self.blobs.insert(
            handle.clone(),
            BlobManifestEntry {
                handle: BlobHandle::from(handle),
                kind: kind.ok_or_else(|| BridgeError::at(at.0, at.1, "blob is missing `kind`"))?,
                sha256: sha256
                    .ok_or_else(|| BridgeError::at(at.0, at.1, "blob is missing `sha256`"))?,
                byte_len: bytes
                    .ok_or_else(|| BridgeError::at(at.0, at.1, "blob is missing `bytes`"))?,
                relative_path: path
                    .ok_or_else(|| BridgeError::at(at.0, at.1, "blob is missing `path`"))?,
            },
        );
        Ok(())
    }

    fn parse_extref(&mut self, at: (usize, usize)) -> Result<(), BridgeError> {
        self.ensure_annotations_closed(at)?;
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let ref_id = self.take_binding()?;
        if ref_id.trim().is_empty() {
            return Err(BridgeError::at(at.0, at.1, "external_ref id is empty"));
        }
        let mut path = None;
        let mut policy = None;
        let mut sha256 = None;
        let mut target_graph = None;
        let mut overrides = None;
        if !self.at(|kind| matches!(kind, TokenKind::RParen)) {
            self.expect(|kind| matches!(kind, TokenKind::Comma), "`,`")?;
            if !self.at(|kind| matches!(kind, TokenKind::RParen)) {
                loop {
                    let key_at = self.location();
                    let key = self.take_ident()?;
                    self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
                    match key.as_str() {
                        "path" => {
                            let path_at = self.location();
                            let value = self.take_string()?;
                            if value.trim().is_empty() {
                                return Err(BridgeError::at(
                                    path_at.0,
                                    path_at.1,
                                    "external_ref `path` is empty",
                                ));
                            }
                            assign_once(&mut path, value, "path", key_at)?;
                        }
                        "policy" => {
                            let policy_at = self.location();
                            let name = self.take_binding()?;
                            let parsed = payload_policy_from_name(&name).ok_or_else(|| {
                                BridgeError::at(
                                    policy_at.0,
                                    policy_at.1,
                                    format!("unknown payload policy `{name}`"),
                                )
                            })?;
                            assign_once(&mut policy, parsed, "policy", key_at)?;
                        }
                        "sha256" => {
                            assign_once(&mut sha256, self.take_string()?, "sha256", key_at)?;
                        }
                        "target_graph" => {
                            let tg_at = self.location();
                            let value = self.take_string()?;
                            if value.trim().is_empty() {
                                return Err(BridgeError::at(
                                    tg_at.0,
                                    tg_at.1,
                                    "external_ref `target_graph` is empty",
                                ));
                            }
                            assign_once(&mut target_graph, value, "target_graph", key_at)?;
                        }
                        "overrides" => {
                            let parsed = self.parse_override_map()?;
                            assign_once(&mut overrides, parsed, "overrides", key_at)?;
                        }
                        other => {
                            return Err(BridgeError::at(
                                key_at.0,
                                key_at.1,
                                format!("unknown external_ref field `{other}`"),
                            ));
                        }
                    }
                    if !self.continue_list(|kind| matches!(kind, TokenKind::RParen))? {
                        break;
                    }
                }
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        if self.external_refs.contains_key(&ref_id) {
            return Err(BridgeError::at(
                at.0,
                at.1,
                format!("duplicate external_ref `{ref_id}`"),
            ));
        }
        self.locs.extrefs.insert(ref_id.clone(), at);
        self.external_refs.insert(
            ref_id.clone(),
            ExternalRefDef {
                ref_id: ExternalRefId::from(ref_id),
                container_path: path
                    .ok_or_else(|| BridgeError::at(at.0, at.1, "external_ref is missing `path`"))?,
                expected_sha256: sha256,
                target_graph_id: target_graph,
                payload_policy: policy.unwrap_or(PayloadPolicy::Eager),
                overrides: overrides.unwrap_or_default(),
            },
        );
        Ok(())
    }

    fn parse_param(&mut self) -> Result<(), BridgeError> {
        let at = self.location();
        self.bump();
        let name = self.take_binding()?;
        if name.trim().is_empty() {
            return Err(BridgeError::at(at.0, at.1, "parameter name is empty"));
        }
        self.expect(|kind| matches!(kind, TokenKind::Colon), "`:`")?;
        let ir_type = self.parse_type()?;
        self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
        let value = self.parse_value()?;
        let formula = if self.at(|kind| matches!(kind, TokenKind::At))
            && matches!(self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                Some(TokenKind::Ident(keyword)) if keyword == "formula")
        {
            self.bump();
            self.bump();
            Some(self.parse_wrapped_string()?)
        } else {
            None
        };
        if self.parameters.contains_key(&name) {
            return Err(BridgeError::at(
                at.0,
                at.1,
                format!("duplicate parameter `{name}`"),
            ));
        }
        self.locs.params.insert(name.clone(), at);
        self.parameters.insert(
            name.clone(),
            ParameterDef {
                name: ParamId::from(name),
                ir_type,
                value,
                formula,
            },
        );
        Ok(())
    }

    fn parse_let(&mut self) -> Result<(), BridgeError> {
        let at = self.location();
        self.bump();
        let id = self.take_binding()?;
        if id.trim().is_empty() {
            return Err(BridgeError::at(at.0, at.1, "node id is empty"));
        }
        self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
        let (op, op_at) = self.parse_path()?;
        if self.at(|kind| matches!(kind, TokenKind::LBrace)) {
            return self.parse_feature_let(at, id, op, op_at);
        }
        let args = self.parse_args()?;
        self.expect(|kind| matches!(kind, TokenKind::Arrow), "`->`")?;
        let output_type = self.parse_type()?;
        let pending = self.pending.take();
        let kind = self.kind_from(&op, &args, op_at)?;
        let inputs = inputs_from(&args)?;
        if self.nodes.contains_key(&id) {
            return Err(BridgeError::at(
                at.0,
                at.1,
                format!("duplicate node `{id}`"),
            ));
        }
        if let NodeKind::ParameterRef { param_id } = &kind {
            self.locs
                .param_uses
                .entry(param_id.0.clone())
                .or_insert(op_at);
        }
        let name = pending.name.unwrap_or_else(|| id.clone());
        self.locs.nodes.insert(id.clone(), at);
        self.nodes.insert(
            id.clone(),
            IrNode {
                id: NodeId::from(id),
                name,
                kind,
                inputs,
                output_type,
                state: pending.state.unwrap_or(NodeState::Valid),
                kernel_op_version: pending.kernel,
                metadata: pending.meta,
            },
        );
        Ok(())
    }

    /// `let id = operator { ... } -> Type` for a V2 `FeatureSpec` block.
    /// Legacy `operator(...)` calls stay on [`Self::parse_let`].
    fn parse_feature_let(
        &mut self,
        at: (usize, usize),
        id: String,
        op: String,
        op_at: (usize, usize),
    ) -> Result<(), BridgeError> {
        let parsed = crate::feature_kcl::parse_feature_block(self.tokens, self.cursor, &op, op_at)?;
        self.cursor = parsed.cursor;
        self.expect(|kind| matches!(kind, TokenKind::Arrow), "`->`")?;
        let output_type = self.parse_type()?;
        let spec_type = parsed.spec.output_type();
        if output_type != spec_type {
            return Err(BridgeError::at(
                at.0,
                at.1,
                format!(
                    "feature `{op}` produces {}, not {}",
                    type_name(spec_type),
                    type_name(output_type),
                ),
            ));
        }
        for (name, line, column) in parsed.node_refs {
            self.locs.node_refs.entry(name).or_insert((line, column));
        }
        for (name, line, column) in parsed.param_uses {
            self.locs.param_uses.entry(name).or_insert((line, column));
        }
        for (name, line, column) in parsed.blob_uses {
            self.locs.blob_uses.entry(name).or_insert((line, column));
        }
        for (name, line, column) in parsed.extref_uses {
            self.locs
                .extref_uses
                .entry(name)
                .or_insert((line, column));
        }
        if self.nodes.contains_key(&id) {
            return Err(BridgeError::at(
                at.0,
                at.1,
                format!("duplicate node `{id}`"),
            ));
        }
        let pending = self.pending.take();
        let name = pending.name.unwrap_or_else(|| id.clone());
        let mut node = IrNode::from_feature_spec(id.as_str(), name, parsed.spec)
            .map_err(|err| BridgeError::at(at.0, at.1, err.to_string()))?;
        node.state = pending.state.unwrap_or(NodeState::Valid);
        node.kernel_op_version = pending.kernel;
        node.metadata = pending.meta;
        self.locs.nodes.insert(id.clone(), at);
        self.nodes.insert(id, node);
        Ok(())
    }

    fn parse_return(&mut self) -> Result<(), BridgeError> {
        self.locs.ret = self.location();
        self.bump();
        self.expect(|kind| matches!(kind, TokenKind::LBracket), "`[`")?;
        if !self.at(|kind| matches!(kind, TokenKind::RBracket)) {
            loop {
                let name = self.parse_ref_name()?;
                self.outputs.push(NodeId::from(name));
                if !self.continue_list(|kind| matches!(kind, TokenKind::RBracket))? {
                    break;
                }
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RBracket), "`]`")?;
        Ok(())
    }

    fn parse_path(&mut self) -> Result<(String, (usize, usize)), BridgeError> {
        let at = self.location();
        let mut name = self.take_name()?;
        while self.eat(|kind| matches!(kind, TokenKind::ColonColon)) {
            name.push_str("::");
            name.push_str(&self.take_name()?);
        }
        Ok((name, at))
    }

    /// Operator segments may be words the lexer classifies as keywords, such as `assembly`.
    fn take_name(&mut self) -> Result<String, BridgeError> {
        let name = match &self.tokens[self.cursor].kind {
            TokenKind::Ident(name) => name.clone(),
            TokenKind::Part => "part".to_owned(),
            TokenKind::Assembly => "assembly".to_owned(),
            TokenKind::Let => "let".to_owned(),
            TokenKind::Return => "return".to_owned(),
            TokenKind::Sketch => "sketch".to_owned(),
            TokenKind::Const => "const".to_owned(),
            TokenKind::True => "true".to_owned(),
            TokenKind::False => "false".to_owned(),
            _ => return Err(self.unexpected("an identifier")),
        };
        self.bump();
        Ok(name)
    }

    fn parse_args(&mut self) -> Result<Vec<Arg>, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let mut args = Vec::new();
        if self.eat(|kind| matches!(kind, TokenKind::RParen)) {
            return Ok(args);
        }
        loop {
            let at = self.location();
            if self.looks_like_named() {
                let name = self.take_key()?;
                self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
                let value = self.parse_value()?;
                args.push(Arg {
                    name: Some(name),
                    value,
                    line: at.0,
                    column: at.1,
                });
            } else {
                let value = self.parse_value()?;
                args.push(Arg {
                    name: None,
                    value,
                    line: at.0,
                    column: at.1,
                });
            }
            if !self.continue_list(|kind| matches!(kind, TokenKind::RParen))? {
                break;
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        Ok(args)
    }

    fn kind_from(
        &self,
        op: &str,
        args: &[Arg],
        at: (usize, usize),
    ) -> Result<NodeKind, BridgeError> {
        match decode_operator(op) {
            None => Err(BridgeError::at(
                at.0,
                at.1,
                format!("unknown operator `{op}`"),
            )),
            Some(DecodedOp::Fixed(_)) if args.iter().any(|arg| arg.name.is_none()) => {
                Err(BridgeError::at(
                    at.0,
                    at.1,
                    format!("`{op}` does not take positional arguments"),
                ))
            }
            Some(DecodedOp::Fixed(kind)) => Ok(kind),
            Some(DecodedOp::ParameterRef) => {
                let mut positional = args.iter().filter(|arg| arg.name.is_none());
                let Some(first) = positional.next() else {
                    return Err(BridgeError::at(
                        at.0,
                        at.1,
                        "`parameter_ref` needs a parameter name string",
                    ));
                };
                if positional.next().is_some() {
                    return Err(BridgeError::at(
                        first.line,
                        first.column,
                        "`parameter_ref` takes one positional parameter name",
                    ));
                }
                let IrValue::String(param_id) = &first.value else {
                    return Err(BridgeError::at(
                        first.line,
                        first.column,
                        "parameter name must be a string, for example `parameter_ref(\"width\")`",
                    ));
                };
                if param_id.trim().is_empty() {
                    return Err(BridgeError::at(
                        first.line,
                        first.column,
                        "parameter name is empty",
                    ));
                }
                Ok(NodeKind::ParameterRef {
                    param_id: ParamId::from(param_id.clone()),
                })
            }
        }
    }

    fn parse_value(&mut self) -> Result<IrValue, BridgeError> {
        if self.at(|kind| matches!(kind, TokenKind::Minus)) {
            let at = self.location();
            self.bump();
            let number = self.take_number()?;
            return self.number_value(number, true, at);
        }
        if self.eat(|kind| matches!(kind, TokenKind::True)) {
            return Ok(IrValue::Bool(true));
        }
        if self.eat(|kind| matches!(kind, TokenKind::False)) {
            return Ok(IrValue::Bool(false));
        }
        if self.at(|kind| matches!(kind, TokenKind::Str(_))) {
            return Ok(IrValue::String(self.take_string()?));
        }
        if self.at(|kind| matches!(kind, TokenKind::Number { .. })) {
            let at = self.location();
            let number = self.take_number()?;
            return self.number_value(number, false, at);
        }
        if self.at(|kind| matches!(kind, TokenKind::LBracket)) {
            return self.parse_list();
        }
        if self.at(|kind| matches!(kind, TokenKind::LBrace)) {
            return self.parse_map();
        }
        if self.at(|kind| matches!(kind, TokenKind::At)) {
            return self.parse_handle();
        }
        if self.peek_ident().is_some() {
            return self.parse_ident_value();
        }
        Err(self.unexpected("a value"))
    }

    fn parse_ident_value(&mut self) -> Result<IrValue, BridgeError> {
        let at = self.location();
        let name = self.take_ident()?;
        if name == "vec3" && self.at(|kind| matches!(kind, TokenKind::LParen)) {
            return self.parse_vec3();
        }
        if name == "node" && self.at(|kind| matches!(kind, TokenKind::LParen)) {
            return self.parse_node_ctor();
        }
        self.remember_node(&name, at);
        Ok(IrValue::NodeRef {
            node_id: NodeId::from(name),
            port: OUTPUT_PORT.to_owned(),
        })
    }

    fn parse_vec3(&mut self) -> Result<IrValue, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let x = self.parse_component()?;
        self.expect(|kind| matches!(kind, TokenKind::Comma), "`,`")?;
        let y = self.parse_component()?;
        self.expect(|kind| matches!(kind, TokenKind::Comma), "`,`")?;
        let z = self.parse_component()?;
        let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        Ok(IrValue::Vec3([x, y, z]))
    }

    fn parse_component(&mut self) -> Result<f64, BridgeError> {
        let at = self.location();
        let negated = self.eat(|kind| matches!(kind, TokenKind::Minus));
        let number = self.take_number()?;
        if number.unit.is_some() {
            return Err(BridgeError::at(
                at.0,
                at.1,
                "vec3 component must be a plain number",
            ));
        }
        let signed = if negated { -number.value } else { number.value };
        if !signed.is_finite() {
            return Err(BridgeError::at(at.0, at.1, "number is not finite"));
        }
        Ok(if signed == 0.0 { 0.0 } else { signed })
    }

    fn parse_node_ctor(&mut self) -> Result<IrValue, BridgeError> {
        let at = self.location();
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let id = self.take_binding()?;
        let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        if id.trim().is_empty() {
            return Err(BridgeError::at(at.0, at.1, "node id is empty"));
        }
        self.remember_node(&id, at);
        Ok(IrValue::NodeRef {
            node_id: NodeId::from(id),
            port: OUTPUT_PORT.to_owned(),
        })
    }

    fn parse_handle(&mut self) -> Result<IrValue, BridgeError> {
        let at = self.location();
        self.bump();
        let keyword = self.take_ident()?;
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        if keyword == "extref" {
            let ref_id = self.take_binding()?;
            let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
            self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
            if ref_id.trim().is_empty() {
                return Err(BridgeError::at(at.0, at.1, "external_ref id is empty"));
            }
            self.locs.extref_uses.entry(ref_id.clone()).or_insert(at);
            return Ok(IrValue::ExternalRef(ExternalRefId::from(ref_id)));
        }
        let body = self.take_string()?;
        let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        if keyword == "topo" {
            return Ok(IrValue::TopoRefExpr(body));
        }
        let handle = if keyword == "blob_ref" {
            body
        } else if let Some(handle) = blob_handle_from_keyword(&keyword, &body) {
            handle
        } else {
            return Err(BridgeError::at(
                at.0,
                at.1,
                format!("unknown handle `@{keyword}`"),
            ));
        };
        if handle.trim().is_empty() {
            return Err(BridgeError::at(at.0, at.1, "blob handle is empty"));
        }
        self.locs.blob_uses.entry(handle.clone()).or_insert(at);
        Ok(IrValue::BlobRef(BlobHandle::from(handle)))
    }

    fn parse_list(&mut self) -> Result<IrValue, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LBracket), "`[`")?;
        let mut items = Vec::new();
        if !self.at(|kind| matches!(kind, TokenKind::RBracket)) {
            loop {
                items.push(self.parse_value()?);
                if !self.continue_list(|kind| matches!(kind, TokenKind::RBracket))? {
                    break;
                }
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RBracket), "`]`")?;
        Ok(IrValue::List(items))
    }

    fn parse_map(&mut self) -> Result<IrValue, BridgeError> {
        Ok(IrValue::Map(self.parse_map_entries(false)?))
    }

    fn parse_override_map(&mut self) -> Result<BTreeMap<String, IrValue>, BridgeError> {
        self.parse_map_entries(true)
    }

    fn parse_map_entries(
        &mut self,
        reject_empty_keys: bool,
    ) -> Result<BTreeMap<String, IrValue>, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LBrace), "`{`")?;
        let mut entries = BTreeMap::new();
        if !self.at(|kind| matches!(kind, TokenKind::RBrace)) {
            loop {
                let at = self.location();
                let key = self.take_key()?;
                if reject_empty_keys && key.trim().is_empty() {
                    return Err(BridgeError::at(at.0, at.1, "override key is empty"));
                }
                self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
                let value = self.parse_value()?;
                if entries.insert(key.clone(), value).is_some() {
                    return Err(BridgeError::at(
                        at.0,
                        at.1,
                        format!("duplicate map key `{key}`"),
                    ));
                }
                if !self.continue_list(|kind| matches!(kind, TokenKind::RBrace))? {
                    break;
                }
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RBrace), "`}`")?;
        Ok(entries)
    }

    fn parse_type(&mut self) -> Result<IrType, BridgeError> {
        let at = self.location();
        let name = self.take_ident()?;
        type_from_name(&name)
            .ok_or_else(|| BridgeError::at(at.0, at.1, format!("unknown type `{name}`")))
    }

    fn parse_ref_name(&mut self) -> Result<String, BridgeError> {
        let at = self.location();
        if self.at(|kind| matches!(kind, TokenKind::Str(_))) {
            let name = self.take_string()?;
            if name.trim().is_empty() {
                return Err(BridgeError::at(at.0, at.1, "node id is empty"));
            }
            self.remember_node(&name, at);
            return Ok(name);
        }
        if self.peek_ident() == Some("node")
            && self.next_is(|kind| matches!(kind, TokenKind::LParen))
        {
            let IrValue::NodeRef { node_id, .. } = self.parse_ident_value()? else {
                return Err(self.error("expected `node(\"id\")`"));
            };
            return Ok(node_id.0);
        }
        let name = self.take_ident()?;
        self.remember_node(&name, at);
        Ok(name)
    }

    fn parse_failed(&mut self) -> Result<NodeState, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let reason = self.take_string()?;
        let mut failed_topo_refs = Vec::new();
        if self.eat(|kind| matches!(kind, TokenKind::Comma))
            && !self.at(|kind| matches!(kind, TokenKind::RParen))
        {
            let key_at = self.location();
            let key = self.take_ident()?;
            if key != "refs" {
                return Err(BridgeError::at(
                    key_at.0,
                    key_at.1,
                    "expected `refs` in `@failed`",
                ));
            }
            self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
            self.expect(|kind| matches!(kind, TokenKind::LBracket), "`[`")?;
            if !self.at(|kind| matches!(kind, TokenKind::RBracket)) {
                loop {
                    failed_topo_refs.push(self.take_string()?);
                    if !self.continue_list(|kind| matches!(kind, TokenKind::RBracket))? {
                        break;
                    }
                }
            }
            self.expect(|kind| matches!(kind, TokenKind::RBracket), "`]`")?;
        }
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        Ok(NodeState::FailedWithRollback {
            reason,
            failed_topo_refs,
        })
    }

    fn parse_string_map(&mut self, label: &str) -> Result<BTreeMap<String, String>, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let mut entries = BTreeMap::new();
        if !self.at(|kind| matches!(kind, TokenKind::RParen)) {
            loop {
                let at = self.location();
                let key = self.take_key()?;
                self.expect(|kind| matches!(kind, TokenKind::Eq), "`=`")?;
                let value = self.take_string()?;
                if entries.insert(key.clone(), value).is_some() {
                    return Err(BridgeError::at(
                        at.0,
                        at.1,
                        format!("duplicate {label} `{key}`"),
                    ));
                }
                if !self.continue_list(|kind| matches!(kind, TokenKind::RParen))? {
                    break;
                }
            }
        }
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        Ok(entries)
    }

    fn parse_wrapped_string(&mut self) -> Result<String, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let value = self.take_string()?;
        let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        Ok(value)
    }

    fn parse_wrapped_u32(&mut self) -> Result<u32, BridgeError> {
        self.expect(|kind| matches!(kind, TokenKind::LParen), "`(`")?;
        let at = self.location();
        let number = self.take_number()?;
        if number.unit.is_some() || is_float_literal(&number.raw) {
            return Err(BridgeError::at(
                at.0,
                at.1,
                "schema version must be an integer",
            ));
        }
        let value = parse_u32_literal(&number.raw)
            .map_err(|message| BridgeError::at(at.0, at.1, message))?;
        let _ = self.eat(|kind| matches!(kind, TokenKind::Comma));
        self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        Ok(value)
    }

    fn finish_marker(&mut self) -> Result<(), BridgeError> {
        if self.eat(|kind| matches!(kind, TokenKind::LParen)) {
            self.expect(|kind| matches!(kind, TokenKind::RParen), "`)`")?;
        }
        Ok(())
    }

    fn number_value(
        &self,
        number: NumberTok,
        negated: bool,
        at: (usize, usize),
    ) -> Result<IrValue, BridgeError> {
        let signed = if negated { -number.value } else { number.value };
        if let Some(suffix) = number.unit {
            return match quantity_from_suffix(signed, &suffix) {
                SuffixParse::Quantity(quantity) => Ok(IrValue::Quantity(quantity)),
                SuffixParse::Unknown => Err(BridgeError::at(
                    at.0,
                    at.1,
                    format!("unknown unit `{suffix}`"),
                )),
                SuffixParse::NonFinite => Err(BridgeError::at(
                    at.0,
                    at.1,
                    "quantity is not a finite number",
                )),
            };
        }
        if is_float_literal(&number.raw) {
            if !signed.is_finite() {
                return Err(BridgeError::at(at.0, at.1, "number is not finite"));
            }
            let signed = if signed == 0.0 { 0.0 } else { signed };
            return Ok(IrValue::Scalar(signed));
        }
        let integer = parse_i64_literal(&number.raw, negated)
            .map_err(|message| BridgeError::at(at.0, at.1, message))?;
        Ok(IrValue::Int(integer))
    }

    fn finish(self) -> Result<CanonicalGraph, BridgeError> {
        let Some(graph_id) = self.graph_id else {
            return Err(BridgeError::at(1, 1, "missing `@graph_id(\"...\")`"));
        };
        let mut graph = CanonicalGraph::new(graph_id);
        graph.ir_schema_version = self.schema.unwrap_or(IR_SCHEMA_VERSION);
        if let Some(units) = self.units {
            graph.units = units;
        }
        graph.parameters = self.parameters;
        graph.blobs = self.blobs;
        graph.external_refs = self.external_refs;
        graph.nodes = self.nodes;
        graph.outputs = self.outputs;
        validate_graph(&graph).map_err(|err| locate(&err, &graph, &self.locs))?;
        Ok(graph)
    }

    fn remember_node(&mut self, id: &str, at: (usize, usize)) {
        self.locs.node_refs.entry(id.to_owned()).or_insert(at);
    }

    /// After a comma, continue the list unless the closer is next.
    /// Returns true when another item should be parsed.
    fn continue_list(&mut self, close: impl Fn(&TokenKind) -> bool) -> Result<bool, BridgeError> {
        if self.eat(|kind| matches!(kind, TokenKind::Comma)) {
            Ok(!self.at(&close))
        } else {
            Ok(false)
        }
    }

    fn looks_like_named(&self) -> bool {
        let key = matches!(
            self.tokens[self.cursor].kind,
            TokenKind::Ident(_) | TokenKind::Str(_)
        );
        key && self.next_is(|kind| matches!(kind, TokenKind::Eq))
    }

    fn take_binding(&mut self) -> Result<String, BridgeError> {
        if self.at(|kind| matches!(kind, TokenKind::Str(_))) {
            self.take_string()
        } else {
            self.take_ident()
        }
    }

    fn take_key(&mut self) -> Result<String, BridgeError> {
        self.take_binding()
    }

    fn take_ident(&mut self) -> Result<String, BridgeError> {
        match &self.tokens[self.cursor].kind {
            TokenKind::Ident(name) => {
                let name = name.clone();
                self.bump();
                Ok(name)
            }
            _ => Err(self.unexpected("an identifier")),
        }
    }

    fn take_string(&mut self) -> Result<String, BridgeError> {
        match &self.tokens[self.cursor].kind {
            TokenKind::Str(value) => {
                let value = value.clone();
                self.bump();
                Ok(value)
            }
            _ => Err(self.unexpected("a string")),
        }
    }

    fn take_number(&mut self) -> Result<NumberTok, BridgeError> {
        match &self.tokens[self.cursor].kind {
            TokenKind::Number {
                value,
                raw_number,
                unit,
            } => {
                let number = NumberTok {
                    value: *value,
                    raw: raw_number.clone(),
                    unit: unit.clone(),
                };
                self.bump();
                Ok(number)
            }
            _ => Err(self.unexpected("a number")),
        }
    }

    fn peek_ident(&self) -> Option<&str> {
        match &self.tokens[self.cursor].kind {
            TokenKind::Ident(name) => Some(name.as_str()),
            _ => None,
        }
    }

    fn at(&self, pred: impl Fn(&TokenKind) -> bool) -> bool {
        pred(&self.tokens[self.cursor].kind)
    }

    fn next_is(&self, pred: impl Fn(&TokenKind) -> bool) -> bool {
        self.tokens
            .get(self.cursor + 1)
            .is_some_and(|token| pred(&token.kind))
    }

    fn eat(&mut self, pred: impl Fn(&TokenKind) -> bool) -> bool {
        if self.at(pred) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, pred: impl Fn(&TokenKind) -> bool, what: &str) -> Result<(), BridgeError> {
        if self.eat(pred) {
            Ok(())
        } else {
            Err(self.unexpected(what))
        }
    }

    fn bump(&mut self) {
        if self.cursor + 1 < self.tokens.len() {
            self.cursor += 1;
        }
    }

    fn location(&self) -> (usize, usize) {
        let span = self.tokens[self.cursor].span;
        (span.start_line, span.start_col)
    }

    fn error(&self, message: impl Into<String>) -> BridgeError {
        let (line, column) = self.location();
        BridgeError::at(line, column, message)
    }

    fn unexpected(&self, what: &str) -> BridgeError {
        let lexeme = self.tokens[self.cursor].lexeme.as_str();
        let found = if lexeme.is_empty() {
            "end of file".to_owned()
        } else {
            format!("`{lexeme}`")
        };
        self.error(format!("expected {what}, found {found}"))
    }
}

impl Pending {
    fn set_state(&mut self, state: NodeState, at: (usize, usize)) -> Result<(), BridgeError> {
        if self.state.is_some() {
            return Err(BridgeError::at(
                at.0,
                at.1,
                "a node can only have one state annotation",
            ));
        }
        self.state = Some(state);
        Ok(())
    }

    fn set_name(&mut self, name: String, at: (usize, usize)) -> Result<(), BridgeError> {
        if self.name.is_some() {
            return Err(BridgeError::at(at.0, at.1, "duplicate `@name`"));
        }
        self.name = Some(name);
        Ok(())
    }

    fn set_kernel(&mut self, kernel: String, at: (usize, usize)) -> Result<(), BridgeError> {
        if self.kernel.is_some() {
            return Err(BridgeError::at(at.0, at.1, "duplicate `@kernel`"));
        }
        self.kernel = Some(kernel);
        Ok(())
    }

    fn set_meta(
        &mut self,
        meta: BTreeMap<String, String>,
        at: (usize, usize),
    ) -> Result<(), BridgeError> {
        if self.has_meta {
            return Err(BridgeError::at(at.0, at.1, "duplicate `@meta`"));
        }
        self.meta = meta;
        self.has_meta = true;
        Ok(())
    }
}

fn inputs_from(args: &[Arg]) -> Result<BTreeMap<String, IrValue>, BridgeError> {
    let mut inputs = BTreeMap::new();
    for arg in args {
        let Some(name) = &arg.name else {
            continue;
        };
        if inputs.insert(name.clone(), arg.value.clone()).is_some() {
            return Err(BridgeError::at(
                arg.line,
                arg.column,
                format!("duplicate input `{name}`"),
            ));
        }
    }
    Ok(inputs)
}

fn assign_once<T>(
    slot: &mut Option<T>,
    value: T,
    name: &str,
    at: (usize, usize),
) -> Result<(), BridgeError> {
    if slot.is_some() {
        return Err(BridgeError::at(at.0, at.1, format!("duplicate `{name}`")));
    }
    *slot = Some(value);
    Ok(())
}

fn is_float_literal(raw: &str) -> bool {
    raw.contains('.') || raw.contains('e') || raw.contains('E')
}

fn parse_i64_literal(raw: &str, negated: bool) -> Result<i64, String> {
    let digits = raw.replace('_', "");
    let text = if negated {
        format!("-{digits}")
    } else {
        digits
    };
    text.parse::<i64>()
        .map_err(|_| format!("integer `{text}` is out of range"))
}

fn parse_u64_literal(raw: &str) -> Result<u64, String> {
    let digits = raw.replace('_', "");
    digits
        .parse::<u64>()
        .map_err(|_| format!("integer `{digits}` is out of range"))
}

fn parse_u32_literal(raw: &str) -> Result<u32, String> {
    let digits = raw.replace('_', "");
    digits
        .parse::<u32>()
        .map_err(|_| format!("integer `{digits}` is out of range"))
}

fn locate(err: &IrError, graph: &CanonicalGraph, locs: &Locations) -> BridgeError {
    let (line, column) = match err {
        IrError::CycleDetected => cycle_member(graph)
            .and_then(|id| locs.nodes.get(&id).copied())
            .or_else(|| extref_cycle_member(graph).and_then(|id| locs.extrefs.get(&id).copied()))
            .unwrap_or(locs.ret),
        IrError::DanglingNode(id) => locs
            .node_refs
            .get(&id.0)
            .or_else(|| locs.nodes.get(&id.0))
            .copied()
            .unwrap_or((1, 1)),
        IrError::DanglingParameter(id) => locs
            .param_uses
            .get(&id.0)
            .or_else(|| locs.params.get(&id.0))
            .copied()
            .unwrap_or((1, 1)),
        IrError::DanglingBlob(id) => locs
            .blob_uses
            .get(&id.0)
            .or_else(|| locs.blobs.get(&id.0))
            .copied()
            .unwrap_or((1, 1)),
        IrError::DanglingExternalRef(id) => locs
            .extref_uses
            .get(&id.0)
            .or_else(|| locs.extrefs.get(&id.0))
            .copied()
            .unwrap_or((1, 1)),
        IrError::InvalidSha256 { entity, id, .. } => match *entity {
            "external_ref" => locs.extrefs.get(id).copied().unwrap_or(locs.graph),
            "blob" => locs.blobs.get(id).copied().unwrap_or(locs.graph),
            _ => locs.graph,
        },
        IrError::UnknownOutputPort { node_id, .. }
        | IrError::UnknownInputPort { node_id, .. }
        | IrError::MissingInput { node_id, .. } => {
            locs.nodes.get(&node_id.0).copied().unwrap_or((1, 1))
        }
        IrError::NonLiteralParameter(id) => locs.params.get(&id.0).copied().unwrap_or((1, 1)),
        IrError::TypeMismatch { location, .. } => location_from_text(location, locs),
        IrError::EmptyId { entity } if *entity == "graph" => locs.graph,
        IrError::DuplicateId { entity, .. } if *entity == "output" => locs.ret,
        IrError::DuplicateId { id, .. } | IrError::IdMismatch { key: id, .. } => locs
            .nodes
            .get(id)
            .or_else(|| locs.params.get(id))
            .or_else(|| locs.blobs.get(id))
            .or_else(|| locs.extrefs.get(id))
            .copied()
            .unwrap_or(locs.graph),
        _ => locs.graph,
    };
    BridgeError::at(line, column, err.to_string())
}

fn location_from_text(location: &str, locs: &Locations) -> (usize, usize) {
    if let Some(rest) = location.strip_prefix("node `")
        && let Some(id) = rest.split('`').next()
    {
        return locs.nodes.get(id).copied().unwrap_or((1, 1));
    }
    if let Some(rest) = location.strip_prefix("parameter `")
        && let Some(id) = rest.split('`').next()
    {
        return locs.params.get(id).copied().unwrap_or((1, 1));
    }
    (1, 1)
}

fn cycle_member(graph: &CanonicalGraph) -> Option<String> {
    let mut state: BTreeMap<&str, u8> = BTreeMap::new();
    for id in graph.nodes.keys() {
        if let Some(found) = visit(graph, id, &mut state) {
            return Some(found.to_owned());
        }
    }
    None
}

fn visit<'a>(
    graph: &'a CanonicalGraph,
    id: &'a str,
    state: &mut BTreeMap<&'a str, u8>,
) -> Option<&'a str> {
    match state.get(id).copied().unwrap_or(0) {
        1 => return Some(id),
        2 => return None,
        _ => {}
    }
    state.insert(id, 1);
    if let Some(node) = graph.nodes.get(id) {
        let mut deps = BTreeSet::new();
        let mut visited_ext = BTreeSet::new();
        for value in node.inputs.values() {
            collect_deps(graph, value, &mut visited_ext, &mut deps);
        }
        for dep in deps {
            if let Some((key, _)) = graph.nodes.get_key_value(&dep)
                && let Some(found) = visit(graph, key, state)
            {
                return Some(found);
            }
        }
    }
    state.insert(id, 2);
    None
}

fn collect_deps<'a>(
    graph: &'a CanonicalGraph,
    value: &'a IrValue,
    visited_ext: &mut BTreeSet<&'a str>,
    deps: &mut BTreeSet<String>,
) {
    match value {
        IrValue::NodeRef { node_id, .. } => {
            deps.insert(node_id.0.clone());
        }
        IrValue::ExternalRef(ref_id) => {
            if visited_ext.insert(ref_id.0.as_str())
                && let Some(ext_def) = graph.external_refs.get(&ref_id.0)
            {
                for override_val in ext_def.overrides.values() {
                    collect_deps(graph, override_val, visited_ext, deps);
                }
            }
        }
        IrValue::List(values) => {
            for value in values {
                collect_deps(graph, value, visited_ext, deps);
            }
        }
        IrValue::Map(values) => {
            for value in values.values() {
                collect_deps(graph, value, visited_ext, deps);
            }
        }
        _ => {}
    }
}

fn extref_cycle_member(graph: &CanonicalGraph) -> Option<String> {
    let mut state: BTreeMap<&str, u8> = BTreeMap::new();
    for id in graph.external_refs.keys() {
        if let Some(found) = visit_extref(graph, id, &mut state) {
            return Some(found.to_owned());
        }
    }
    None
}

fn visit_extref<'a>(
    graph: &'a CanonicalGraph,
    id: &'a str,
    state: &mut BTreeMap<&'a str, u8>,
) -> Option<&'a str> {
    match state.get(id).copied().unwrap_or(0) {
        1 => return Some(id),
        2 => return None,
        _ => {}
    }
    state.insert(id, 1);
    if let Some(ext_def) = graph.external_refs.get(id) {
        let mut ext_deps = BTreeSet::new();
        for value in ext_def.overrides.values() {
            collect_extref_deps(value, &mut ext_deps);
        }
        for dep in ext_deps {
            if let Some((key, _)) = graph.external_refs.get_key_value(&dep)
                && let Some(found) = visit_extref(graph, key, state)
            {
                return Some(found);
            }
        }
    }
    state.insert(id, 2);
    None
}

fn collect_extref_deps(value: &IrValue, deps: &mut BTreeSet<String>) {
    match value {
        IrValue::ExternalRef(ref_id) => {
            deps.insert(ref_id.0.clone());
        }
        IrValue::List(values) => {
            for value in values {
                collect_extref_deps(value, deps);
            }
        }
        IrValue::Map(values) => {
            for value in values.values() {
                collect_extref_deps(value, deps);
            }
        }
        _ => {}
    }
}
