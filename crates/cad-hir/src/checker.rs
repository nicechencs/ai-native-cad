use crate::ir::*;
use crate::types::*;
use cad_syntax::{
    Annotation, BinaryOp, CallArg, Expr, LetStmt, ParamDecl, PartDecl, SourceFile, Span, Stmt,
    TopItem, UnaryOp,
};
use std::collections::HashMap;

pub struct CheckOutput {
    pub module: IrModule,
    pub diagnostics: Vec<Diagnostic>,
    pub import_argument_hovers: Vec<ImportArgumentHover>,
}

pub fn classify_expr(expr: &Expr) -> TierClassification {
    let inner = unwrap_paren(expr);
    if is_literal_quantity(inner) {
        return TierClassification {
            tier: SyncTier::Tier1,
            base_param: None,
            offset_value: None,
            offset_span: None,
            reason: "direct numeric/quantity literal".to_string(),
        };
    }

    if let Expr::Binary {
        op: BinaryOp::Add | BinaryOp::Sub,
        left,
        right,
        ..
    } = inner
    {
        let l = unwrap_paren(left);
        let r = unwrap_paren(right);
        if let (Some(var_name), Some((lit_val, lit_span))) =
            (extract_single_var(l), extract_literal_num(r))
        {
            let signed = match inner {
                Expr::Binary {
                    op: BinaryOp::Sub, ..
                } => -lit_val,
                _ => lit_val,
            };
            return TierClassification {
                tier: SyncTier::Tier2,
                base_param: Some(var_name),
                offset_value: Some(signed),
                offset_span: Some(lit_span),
                reason: "single-variable affine offset".to_string(),
            };
        }
        if matches!(
            inner,
            Expr::Binary {
                op: BinaryOp::Add,
                ..
            }
        ) {
            if let (Some((lit_val, lit_span)), Some(var_name)) =
                (extract_literal_num(l), extract_single_var(r))
            {
                return TierClassification {
                    tier: SyncTier::Tier2,
                    base_param: Some(var_name),
                    offset_value: Some(lit_val),
                    offset_span: Some(lit_span),
                    reason: "single-variable affine offset".to_string(),
                };
            }
        }
    }

    TierClassification {
        tier: SyncTier::Tier3,
        base_param: None,
        offset_value: None,
        offset_span: None,
        reason: "coupled or multi-variable expression".to_string(),
    }
}

fn unwrap_paren(mut expr: &Expr) -> &Expr {
    while let Expr::Paren { inner, .. } = expr {
        expr = inner;
    }
    expr
}

fn is_literal_quantity(expr: &Expr) -> bool {
    match unwrap_paren(expr) {
        Expr::Quantity { .. } => true,
        Expr::Unary {
            op: UnaryOp::Plus | UnaryOp::Minus,
            expr,
            ..
        } => matches!(unwrap_paren(expr), Expr::Quantity { .. }),
        _ => false,
    }
}

fn extract_single_var(expr: &Expr) -> Option<String> {
    match unwrap_paren(expr) {
        Expr::Path { segments, .. } if segments.len() == 1 => Some(segments[0].clone()),
        _ => None,
    }
}

fn extract_literal_num(expr: &Expr) -> Option<(f64, Span)> {
    match unwrap_paren(expr) {
        Expr::Quantity {
            value, unit, span, ..
        } => {
            let (canon, _, _) = convert_quantity(*value, unit.as_deref()).ok()?;
            Some((canon, *span))
        }
        Expr::Unary {
            op: UnaryOp::Minus,
            expr,
            span,
        } => {
            if let Expr::Quantity { value, unit, .. } = unwrap_paren(expr) {
                let (canon, _, _) = convert_quantity(*value, unit.as_deref()).ok()?;
                Some((-canon, *span))
            } else {
                None
            }
        }
        _ => None,
    }
}

struct Checker {
    diagnostics: Vec<Diagnostic>,
    env_stack: Vec<Env>,
    hover_symbols: Vec<HoverInfo>,
    import_argument_hovers: Vec<ImportArgumentHover>,
    edit_handles: Vec<EditHandleCandidate>,
    next_node_id: usize,
}

impl Checker {
    fn new() -> Self {
        let mut root_env = HashMap::new();
        for axis in ["X_AXIS", "Y_AXIS", "Z_AXIS"] {
            root_env.insert(axis.to_string(), DimType::Axis);
        }
        for plane in ["XY_PLANE", "XZ_PLANE", "YZ_PLANE"] {
            root_env.insert(plane.to_string(), DimType::Plane);
        }
        for dir in ["X", "Y", "Z"] {
            root_env.insert(dir.to_string(), DimType::Direction);
        }
        Self {
            diagnostics: Vec::new(),
            env_stack: vec![root_env],
            hover_symbols: Vec::new(),
            import_argument_hovers: Vec::new(),
            edit_handles: Vec::new(),
            next_node_id: 0,
        }
    }

    fn push_scope(&mut self) {
        self.env_stack.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.env_stack.pop();
    }

    fn insert_var(&mut self, name: String, ty: DimType) {
        if let Some(top) = self.env_stack.last_mut() {
            top.insert(name, ty);
        }
    }

    fn lookup_var(&self, name: &str) -> Option<&DimType> {
        for scope in self.env_stack.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(ty);
            }
        }
        None
    }

    fn emit(&mut self, code: &str, severity: Severity, message: impl Into<String>, span: Span) {
        self.diagnostics.push(Diagnostic {
            code: code.to_string(),
            severity,
            message: message.into(),
            span,
        });
    }

    fn check_source_file(&mut self, file: &SourceFile) -> IrModule {
        let annotations = file
            .annotations
            .iter()
            .map(|a| self.lower_annotation(a))
            .collect();

        let mut constants = Vec::new();
        let mut parts = Vec::new();
        let mut assemblies = Vec::new();
        for item in &file.items {
            match item {
                TopItem::Const(let_stmt) => {
                    let tier_info = classify_expr(&let_stmt.value);
                    let ir_expr = self.check_expr(&let_stmt.value);
                    let ty = ir_expr.ty().clone();
                    self.insert_var(let_stmt.name.clone(), ty.clone());
                    let id = self.next_node_id;
                    self.next_node_id += 1;
                    constants.push(IrNode {
                        id,
                        name: Some(let_stmt.name.clone()),
                        expr: ir_expr,
                        ty,
                        tier: tier_info.tier,
                        span: let_stmt.span,
                    });
                }
                TopItem::Part(part) => {
                    self.insert_var(part.name.clone(), DimType::Component);
                    parts.push(self.check_part(part));
                }
                TopItem::Assembly(asm) => {
                    let part_decl_equiv = PartDecl {
                        name: asm.name.clone(),
                        params: asm.params.clone(),
                        return_type: asm.return_type.clone().or_else(|| Some("Assembly".to_string())),
                        body: asm.body.clone(),
                        span: asm.span,
                    };
                    assemblies.push(self.check_part(&part_decl_equiv));
                }
            }
        }

        IrModule {
            annotations,
            constants,
            parts,
            assemblies,
        }
    }

    fn lower_annotation(&mut self, ann: &Annotation) -> IrAnnotation {
        let path = ann.path.join("::");
        let args = ann
            .args
            .iter()
            .map(|a| IrArg {
                name: a.name.clone(),
                value: self.check_expr(&a.value),
                span: a.span,
            })
            .collect();
        IrAnnotation {
            path,
            args,
            span: ann.span,
        }
    }

    fn check_part(&mut self, part: &PartDecl) -> IrPart {
        self.push_scope();
        self.hover_symbols.clear();
        self.edit_handles.clear();

        let mut ir_params = Vec::new();
        for p in &part.params {
            ir_params.push(self.check_param(p));
        }

        let mut nodes = Vec::new();
        let mut return_node = None;

        for stmt in &part.body.stmts {
            match stmt {
                Stmt::Let(LetStmt {
                    name,
                    name_span,
                    value,
                    span,
                }) => {
                    let tier_info = classify_expr(value);
                    let ir_expr = self.check_expr(value);
                    let ty = ir_expr.ty().clone();
                    self.insert_var(name.clone(), ty.clone());

                    self.hover_symbols.push(HoverInfo {
                        symbol: name.clone(),
                        dim_type: ty.clone(),
                        sync_tier: Some(tier_info.tier),
                        canonical_unit: ty.canonical_unit().to_string(),
                        span: *name_span,
                    });

                    // Expose scalar/dimensional `let` bindings that are Tier 1/2/3 as editable/inspectable handles
                    if matches!(
                        ty,
                        DimType::Length
                            | DimType::Angle
                            | DimType::Force
                            | DimType::Pressure
                            | DimType::F64
                            | DimType::U32
                    ) {
                        let current_value = eval_const_preview(&ir_expr);
                        self.edit_handles.push(EditHandleCandidate {
                            handle_id: format!("handle_let_{name}"),
                            param_name: name.clone(),
                            tier: tier_info.tier as u8,
                            dim_type: ty.clone(),
                            current_value,
                            unit: ty.canonical_unit().to_string(),
                            ast_span: value.span(),
                            classification: tier_info.clone(),
                        });
                    }

                    let id = self.next_node_id;
                    self.next_node_id += 1;
                    nodes.push(IrNode {
                        id,
                        name: Some(name.clone()),
                        expr: ir_expr,
                        ty,
                        tier: tier_info.tier,
                        span: *span,
                    });
                }
                Stmt::Return(ret) => {
                    let ir_expr = self.check_expr(&ret.value);
                    let ty = ir_expr.ty().clone();
                    let expected_ret = part
                        .return_type
                        .as_deref()
                        .map(DimType::from_type_name)
                        .unwrap_or(DimType::Solid);
                    let ret_ok = expected_ret == DimType::Unknown
                        || ty == DimType::Unknown
                        || ty == expected_ret
                        || (expected_ret == DimType::Assembly
                            && matches!(ty, DimType::Array(ref inner) if **inner == DimType::Component));
                    if !ret_ok {
                        self.emit(
                            "E_RETURN_TYPE_MISMATCH",
                            Severity::Error,
                            format!(
                                "return type mismatch: expected `{expected_ret}`, found `{ty}`"
                            ),
                            ret.span,
                        );
                    }
                    let id = self.next_node_id;
                    self.next_node_id += 1;
                    nodes.push(IrNode {
                        id,
                        name: None,
                        expr: ir_expr,
                        ty,
                        tier: SyncTier::Tier3,
                        span: ret.span,
                    });
                    return_node = Some(id);
                }
                Stmt::Expr(expr) => {
                    let ir_expr = self.check_expr(expr);
                    let ty = ir_expr.ty().clone();
                    let id = self.next_node_id;
                    self.next_node_id += 1;
                    nodes.push(IrNode {
                        id,
                        name: None,
                        expr: ir_expr,
                        ty,
                        tier: SyncTier::Tier3,
                        span: expr.span(),
                    });
                }
            }
        }

        let return_type = part
            .return_type
            .as_deref()
            .map(DimType::from_type_name)
            .unwrap_or(DimType::Solid);

        let result = IrPart {
            name: part.name.clone(),
            params: ir_params,
            return_type,
            nodes,
            return_node,
            edit_handles: std::mem::take(&mut self.edit_handles),
            hover_symbols: std::mem::take(&mut self.hover_symbols),
            span: part.span,
        };
        self.pop_scope();
        result
    }

    fn check_param(&mut self, p: &ParamDecl) -> IrParam {
        let declared_ty = DimType::from_type_name(&p.ty);
        if declared_ty == DimType::Unknown {
            self.emit(
                "E_UNKNOWN_TYPE",
                Severity::Error,
                format!("unknown parameter type `{}`", p.ty),
                p.span,
            );
        }

        let annotations = p
            .annotations
            .iter()
            .map(|a| self.lower_annotation(a))
            .collect();

        let default_expr = p.default.as_ref().map(|def| {
            let tier_info = classify_expr(def);
            let ir_def = self.check_expr(def);
            let def_ty = ir_def.ty().clone();
            if declared_ty != DimType::Unknown && !types_compatible(&declared_ty, &def_ty) {
                self.emit(
                    "E_DIM_MISMATCH",
                    Severity::Error,
                    format!(
                        "parameter `{}` expects `{declared_ty}`, found `{def_ty}`",
                        p.name
                    ),
                    def.span(),
                );
            }
            self.edit_handles.push(EditHandleCandidate {
                handle_id: format!("handle_{}", p.name),
                param_name: p.name.clone(),
                tier: tier_info.tier as u8,
                dim_type: declared_ty.clone(),
                current_value: eval_const_preview(&ir_def),
                unit: declared_ty.canonical_unit().to_string(),
                ast_span: def.span(),
                classification: tier_info,
            });
            ir_def
        });

        self.insert_var(p.name.clone(), declared_ty.clone());
        self.hover_symbols.push(HoverInfo {
            symbol: p.name.clone(),
            dim_type: declared_ty.clone(),
            sync_tier: Some(SyncTier::Tier1),
            canonical_unit: declared_ty.canonical_unit().to_string(),
            span: p.span,
        });

        IrParam {
            name: p.name.clone(),
            ty: declared_ty,
            default_expr,
            annotations,
            span: p.span,
        }
    }

    fn check_expr(&mut self, expr: &Expr) -> IrExpr {
        match expr {
            Expr::Quantity {
                value,
                unit,
                span,
                ..
            } => match convert_quantity(*value, unit.as_deref()) {
                Ok((canonical_value, ty, canon_unit)) => {
                    self.hover_symbols.push(HoverInfo {
                        symbol: format!("{value}{}", unit.as_deref().unwrap_or("")),
                        dim_type: ty.clone(),
                        sync_tier: Some(SyncTier::Tier1),
                        canonical_unit: canon_unit.to_string(),
                        span: *span,
                    });
                    IrExpr::Quantity {
                        canonical_value,
                        unit: canon_unit.to_string(),
                        ty,
                        span: *span,
                    }
                }
                Err(msg) => {
                    self.emit("E_UNKNOWN_UNIT", Severity::Error, msg, *span);
                    IrExpr::Quantity {
                        canonical_value: *value,
                        unit: unit.clone().unwrap_or_default(),
                        ty: DimType::Unknown,
                        span: *span,
                    }
                }
            },
            Expr::Str { value, span } => IrExpr::Str {
                value: value.clone(),
                span: *span,
            },
            Expr::Bool { value, span } => IrExpr::Bool {
                value: *value,
                span: *span,
            },
            Expr::Path { segments, span } => {
                if segments.len() == 1 {
                    let name = &segments[0];
                    if let Some(ty) = self.lookup_var(name).cloned() {
                        self.hover_symbols.push(HoverInfo {
                            symbol: name.clone(),
                            dim_type: ty.clone(),
                            sync_tier: None,
                            canonical_unit: ty.canonical_unit().to_string(),
                            span: *span,
                        });
                        IrExpr::Var {
                            name: name.clone(),
                            ty,
                            span: *span,
                        }
                    } else {
                        self.emit(
                            "E_UNDEFINED_SYMBOL",
                            Severity::Error,
                            format!("undefined symbol `{name}`"),
                            *span,
                        );
                        IrExpr::Var {
                            name: name.clone(),
                            ty: DimType::Unknown,
                            span: *span,
                        }
                    }
                } else {
                    let full = segments.join("::");
                    IrExpr::Var {
                        name: full,
                        ty: DimType::Unknown,
                        span: *span,
                    }
                }
            }
            Expr::Array { elements, span } => {
                let checked: Vec<IrExpr> = elements.iter().map(|e| self.check_expr(e)).collect();
                let elem_ty = checked
                    .first()
                    .map(|e| e.ty().clone())
                    .unwrap_or(DimType::Unknown);
                for elem in checked.iter().skip(1) {
                    if !types_compatible(&elem_ty, elem.ty()) {
                        self.emit(
                            "E_ARRAY_TYPE_MISMATCH",
                            Severity::Error,
                            format!(
                                "heterogeneous array element: expected `{elem_ty}`, found `{}`",
                                elem.ty()
                            ),
                            elem.span(),
                        );
                    }
                }
                IrExpr::Array {
                    elements: checked,
                    ty: DimType::Array(Box::new(elem_ty)),
                    span: *span,
                }
            }
            Expr::Unary { op, expr, span } => {
                let inner = self.check_expr(expr);
                let ty = match op {
                    UnaryOp::Plus | UnaryOp::Minus => inner.ty().clone(),
                    UnaryOp::Not => {
                        if *inner.ty() != DimType::Bool && *inner.ty() != DimType::Unknown {
                            self.emit(
                                "E_TYPE_MISMATCH",
                                Severity::Error,
                                format!("logical `!` requires `bool`, found `{}`", inner.ty()),
                                *span,
                            );
                        }
                        DimType::Bool
                    }
                };
                IrExpr::Unary {
                    op: *op,
                    operand: Box::new(inner),
                    ty,
                    span: *span,
                }
            }
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => {
                let l = self.check_expr(left);
                let r = self.check_expr(right);
                let ty = self.check_binary_dim(*op, l.ty(), r.ty(), *span);
                IrExpr::Binary {
                    op: *op,
                    left: Box::new(l),
                    right: Box::new(r),
                    ty,
                    span: *span,
                }
            }
            Expr::Paren { inner, .. } => self.check_expr(inner),
            Expr::Call { callee, args, span } => self.check_call(callee, args, *span, None),
            Expr::MethodCall {
                receiver,
                method,
                args,
                span,
            } => {
                let recv = self.check_expr(receiver);
                let checked_args = self.lower_call_args(args, true);
                let ty = infer_method_type(recv.ty(), method);
                IrExpr::MethodCall {
                    receiver: Box::new(recv),
                    method: method.clone(),
                    args: checked_args,
                    ty,
                    span: *span,
                }
            }
            Expr::FieldAccess {
                receiver,
                field,
                span,
            } => {
                let recv = self.check_expr(receiver);
                IrExpr::FieldAccess {
                    receiver: Box::new(recv),
                    field: field.clone(),
                    ty: DimType::Unknown,
                    span: *span,
                }
            }
            Expr::Sketch { args, body, span } => self.check_sketch(args, body, *span),
            Expr::Pipe { left, right, span } => {
                let lhs = self.check_expr(left);
                let rhs = match right.as_ref() {
                    Expr::Call {
                        callee,
                        args,
                        span: call_span,
                    } => self.check_call(callee, args, *call_span, Some(lhs.ty())),
                    other => self.check_expr(other),
                };
                let ty = rhs.ty().clone();
                IrExpr::Pipe {
                    left: Box::new(lhs),
                    right: Box::new(rhs),
                    ty,
                    span: *span,
                }
            }
        }
    }

    fn check_binary_dim(
        &mut self,
        op: BinaryOp,
        lt: &DimType,
        rt: &DimType,
        span: Span,
    ) -> DimType {
        if *lt == DimType::Unknown || *rt == DimType::Unknown {
            return DimType::Unknown;
        }
        match op {
            BinaryOp::Add | BinaryOp::Sub => {
                if types_compatible(lt, rt) {
                    if *lt == DimType::U32 && *rt == DimType::F64 {
                        DimType::F64
                    } else {
                        lt.clone()
                    }
                } else {
                    self.emit(
                        "E_DIM_MISMATCH",
                        Severity::Error,
                        format!(
                            "dimensional mismatch: cannot apply `{}` to `{lt}` and `{rt}`",
                            if op == BinaryOp::Add { "+" } else { "-" }
                        ),
                        span,
                    );
                    DimType::Unknown
                }
            }
            BinaryOp::Mul => {
                if lt.is_numeric_scalar() {
                    rt.clone()
                } else if rt.is_numeric_scalar() {
                    lt.clone()
                } else if *lt == DimType::Length && *rt == DimType::Length {
                    DimType::Area
                } else if (*lt == DimType::Area && *rt == DimType::Length)
                    || (*lt == DimType::Length && *rt == DimType::Area)
                {
                    DimType::Volume
                } else {
                    self.emit(
                        "E_DIM_MISMATCH",
                        Severity::Error,
                        format!("dimensional mismatch: cannot multiply `{lt}` by `{rt}`"),
                        span,
                    );
                    DimType::Unknown
                }
            }
            BinaryOp::Div => {
                if rt.is_numeric_scalar() {
                    lt.clone()
                } else if lt == rt
                    && matches!(
                        lt,
                        DimType::Length
                            | DimType::Angle
                            | DimType::Force
                            | DimType::Pressure
                            | DimType::Area
                            | DimType::Volume
                    )
                {
                    DimType::F64
                } else if *lt == DimType::Force && *rt == DimType::Area {
                    DimType::Pressure
                } else if *lt == DimType::Area && *rt == DimType::Length {
                    DimType::Length
                } else if *lt == DimType::Volume && *rt == DimType::Length {
                    DimType::Area
                } else {
                    self.emit(
                        "E_DIM_MISMATCH",
                        Severity::Error,
                        format!("dimensional mismatch: cannot divide `{lt}` by `{rt}`"),
                        span,
                    );
                    DimType::Unknown
                }
            }
            BinaryOp::Rem => {
                if types_compatible(lt, rt) {
                    lt.clone()
                } else {
                    self.emit(
                        "E_DIM_MISMATCH",
                        Severity::Error,
                        format!("dimensional mismatch in `%`: `{lt}` vs `{rt}`"),
                        span,
                    );
                    DimType::Unknown
                }
            }
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                if !types_compatible(lt, rt) {
                    self.emit(
                        "E_DIM_MISMATCH",
                        Severity::Error,
                        format!("cannot compare `{lt}` with `{rt}`"),
                        span,
                    );
                }
                DimType::Bool
            }
            BinaryOp::And | BinaryOp::Or => {
                if *lt != DimType::Bool || *rt != DimType::Bool {
                    self.emit(
                        "E_TYPE_MISMATCH",
                        Severity::Error,
                        format!("logical operator requires `bool`, found `{lt}` and `{rt}`"),
                        span,
                    );
                }
                DimType::Bool
            }
        }
    }

    fn lower_call_args(&mut self, args: &[CallArg], allow_selector_keys: bool) -> Vec<IrArg> {
        args.iter()
            .map(|arg| {
                // Selector sort keys such as `radius` are not variable references.
                let value = match &arg.value {
                    Expr::Path { segments, span }
                        if allow_selector_keys
                            && segments.len() == 1
                            && matches!(segments[0].as_str(), "X" | "Y" | "Z" | "radius" | "area") =>
                    {
                        IrExpr::ConstDir {
                            name: segments[0].clone(),
                            ty: DimType::Direction,
                            span: *span,
                        }
                    }
                    other => self.check_expr(other),
                };
                IrArg {
                    name: arg.name.clone(),
                    value,
                    span: arg.span,
                }
            })
            .collect()
    }

    fn check_call(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        span: Span,
        piped_input: Option<&DimType>,
    ) -> IrExpr {
        let callee_name = match callee {
            Expr::Path { segments, .. } => segments.join("::"),
            _ => "<dynamic>".to_string(),
        };

        let selector_receiver = match callee_name.as_str() {
            "faces" => Some(DimType::Solid),
            "planar" | "cylindrical" | "conical" | "filter" | "tagged" | "max_by" | "min_by" => {
                Some(DimType::FaceSet)
            }
            _ => None,
        };
        let selector_is_piped = piped_input.is_some() && selector_receiver.is_some();
        let mut checked_args = self.lower_call_args(args, selector_is_piped);

        // Check Phase 2 graceful degradation
        let mut is_phase2_fallback = false;
        if callee_name == "implicit::smooth_union" {
            is_phase2_fallback = true;
            self.emit(
                "I_PHASE2_IMPLICIT_FALLBACK",
                Severity::Info,
                "Phase 2 operator `implicit::smooth_union` lowered to MVP exact B-Rep `boolean::union` fallback",
                span,
            );
        } else if callee_name == "field::stress_driven" {
            is_phase2_fallback = true;
            self.emit(
                "I_PHASE2_FIELD_FALLBACK",
                Severity::Info,
                "Phase 2 operator `field::stress_driven` lowered to nominal scalar blend radius",
                span,
            );
        }

        if let Some(parameters) = import_parameters(&callee_name) {
            self.check_import_args(&callee_name, parameters, &mut checked_args, span);
        } else if callee_name == "tag::faces" {
            self.check_tag_faces_args(&checked_args, piped_input, span);
        } else if matches!(callee_name.as_str(), "face::offset" | "face::move" | "face::delete") {
            self.check_face_edit_args(&callee_name, &checked_args, piped_input, span);
        } else if callee_name == "assert::no_interference" {
            self.check_interference_args(&checked_args, piped_input, span);
        } else {
            self.validate_call_args(&callee_name, &checked_args, span);
        }

        let ret_ty = if let (Some(expected), Some(actual)) = (selector_receiver, piped_input) {
            if !types_compatible(&expected, actual) {
                self.emit(
                    "E_ARG_TYPE_MISMATCH",
                    Severity::Error,
                    format!("`{callee_name}` expects `{expected}` as pipe input, found `{actual}`"),
                    span,
                );
            }
            DimType::FaceSet
        } else {
            match infer_call_return_type(&callee_name, piped_input) {
                DimType::Unknown => self
                    .lookup_var(&callee_name)
                    .cloned()
                    .unwrap_or(DimType::Unknown),
                other => other,
            }
        };
        IrExpr::Call {
            callee: callee_name,
            args: checked_args,
            ty: ret_ty,
            is_phase2_fallback,
            span,
        }
    }

    fn check_import_args(
        &mut self,
        callee: &str,
        parameters: &'static [ImportParameter],
        args: &mut [IrArg],
        span: Span,
    ) {
        let mut seen = vec![false; parameters.len()];
        for (index, arg) in args.iter_mut().enumerate() {
            let param_index = if let Some(name) = arg.name.as_deref() {
                parameters.iter().position(|p| p.name == name)
            } else {
                (index < parameters.len()).then_some(index)
            };
            let Some(param_index) = param_index else {
                self.emit(
                    "E_ARG_UNKNOWN",
                    Severity::Error,
                    match &arg.name {
                        Some(name) => format!("`{callee}` has no parameter `{name}`"),
                        None => format!("`{callee}` has no positional parameter {}", index + 1),
                    },
                    arg.span,
                );
                continue;
            };
            let parameter = &parameters[param_index];
            if arg.name.is_some() {
                let name_len = parameter.name.len(); // Parameter labels are ASCII.
                self.import_argument_hovers.push(ImportArgumentHover {
                    parameter,
                    span: Span::new(
                        arg.span.start_line,
                        arg.span.start_col,
                        arg.span.start_line,
                        arg.span.start_col + name_len,
                        arg.span.start_byte,
                        arg.span.start_byte + name_len,
                    ),
                });
            }
            if std::mem::replace(&mut seen[param_index], true) {
                self.emit(
                    "E_ARG_DUPLICATE",
                    Severity::Error,
                    format!("`{callee}` parameter `{}` was supplied more than once", parameter.name),
                    arg.span,
                );
            }
            self.check_argument_type(callee, parameter.name, &parameter.ty, arg);
            // Give the evaluator one representation for both argument spellings.
            arg.name = Some(parameter.name.to_owned());
        }
        for (parameter, supplied) in parameters.iter().zip(seen) {
            if parameter.required && !supplied {
                self.emit(
                    "E_ARG_MISSING",
                    Severity::Error,
                    format!("`{callee}` requires parameter `{}`", parameter.name),
                    span,
                );
            }
        }
    }

    fn check_tag_faces_args(&mut self, args: &[IrArg], piped_input: Option<&DimType>, span: Span) {
        let mut seen_tag = false;
        let mut seen_faces = false;
        for (index, arg) in args.iter().enumerate() {
            let (name, ty, seen) = match (arg.name.as_deref(), index) {
                (Some("tag"), _) | (None, 0) => ("tag", DimType::Str, &mut seen_tag),
                (Some("faces"), _) => ("faces", DimType::FaceSet, &mut seen_faces),
                (name, _) => {
                    self.emit(
                        "E_ARG_UNKNOWN",
                        Severity::Error,
                        match name {
                            Some(name) => format!("`tag::faces` has no parameter `{name}`"),
                            None => format!("`tag::faces` has no positional parameter {}", index + 1),
                        },
                        arg.span,
                    );
                    continue;
                }
            };
            if std::mem::replace(seen, true) {
                self.emit(
                    "E_ARG_DUPLICATE",
                    Severity::Error,
                    format!("`tag::faces` parameter `{name}` was supplied more than once"),
                    arg.span,
                );
            }
            self.check_argument_type("tag::faces", name, &ty, arg);
        }
        if seen_faces && !seen_tag {
            self.emit(
                "E_ARG_MISSING",
                Severity::Error,
                "`tag::faces` with a face subset requires parameter `tag`",
                span,
            );
        }
        if seen_faces && !matches!(piped_input, Some(DimType::Solid | DimType::Unknown)) {
            self.emit(
                "E_ARG_TYPE_MISMATCH",
                Severity::Error,
                "`tag::faces` with a face subset requires a Solid as pipe input",
                span,
            );
        }
    }

    fn check_face_edit_args(&mut self, callee: &str, args: &[IrArg], piped_input: Option<&DimType>, span: Span) {
        let extra = match callee {
            "face::offset" => Some(("delta", DimType::Length)),
            "face::move" => Some(("offset", DimType::Array(Box::new(DimType::Length)))),
            _ => None,
        };
        let parameters = [("faces", DimType::FaceSet)].into_iter().chain(extra).collect::<Vec<_>>();
        let mut seen = [false; 2];
        if !matches!(piped_input, Some(DimType::Solid | DimType::Unknown)) {
            self.emit(
                "E_ARG_TYPE_MISMATCH",
                Severity::Error,
                format!("`{callee}` requires a Solid as pipe input"),
                span,
            );
        }
        for arg in args {
            let Some(index) = parameters
                .iter()
                .position(|(name, _)| Some(*name) == arg.name.as_deref())
            else {
                self.emit(
                    "E_ARG_UNKNOWN",
                    Severity::Error,
                    match arg.name.as_deref() {
                        Some(name) => format!("`{callee}` has no parameter `{name}`"),
                        None => format!("`{callee}` requires named parameters"),
                    },
                    arg.span,
                );
                continue;
            };
            let (name, ty) = &parameters[index];
            if std::mem::replace(&mut seen[index], true) {
                self.emit(
                    "E_ARG_DUPLICATE",
                    Severity::Error,
                    format!("`{callee}` parameter `{name}` was supplied more than once"),
                    arg.span,
                );
            }
            if *name == "offset" {
                let valid = match &arg.value {
                    IrExpr::Array { elements, .. } => elements.len() == 3 && elements.iter()
                        .all(|element| matches!(element.ty(), DimType::Length | DimType::Unknown)),
                    value => matches!(value.ty(), DimType::Unknown)
                        || matches!(value.ty(), DimType::Array(element) if **element == DimType::Length),
                };
                if !valid {
                    self.emit("E_ARG_TYPE_MISMATCH", Severity::Error,
                        "`face::move` requires `offset` with three lengths", arg.value.span());
                }
            } else {
                self.check_argument_type(callee, name, ty, arg);
            }
        }
        for ((name, _), supplied) in parameters.into_iter().zip(seen) {
            if !supplied {
                self.emit(
                    "E_ARG_MISSING",
                    Severity::Error,
                    format!("`{callee}` requires parameter `{name}`"),
                    span,
                );
            }
        }
    }

    fn check_interference_args(&mut self, args: &[IrArg], piped_input: Option<&DimType>, span: Span) {
        const CALLEE: &str = "assert::no_interference";
        const REQUIRED: [&str; 3] = ["a", "b", "min_clearance"];
        let mut seen = [false; REQUIRED.len()];
        if piped_input.is_some() {
            self.emit(
                "E_ARG_UNKNOWN",
                Severity::Error,
                format!("`{CALLEE}` does not take a pipe input; specify `a` and `b` by name"),
                span,
            );
        }
        for arg in args {
            let Some(index) = REQUIRED
                .iter()
                .position(|&name| Some(name) == arg.name.as_deref())
            else {
                self.emit(
                    "E_ARG_UNKNOWN",
                    Severity::Error,
                    match arg.name.as_deref() {
                        Some(name) => format!("`{CALLEE}` has no parameter `{name}`"),
                        None => format!("`{CALLEE}` requires named parameters"),
                    },
                    arg.span,
                );
                continue;
            };
            let name = REQUIRED[index];
            if std::mem::replace(&mut seen[index], true) {
                self.emit(
                    "E_ARG_DUPLICATE",
                    Severity::Error,
                    format!("`{CALLEE}` parameter `{name}` was supplied more than once"),
                    arg.span,
                );
            }
            if name == "min_clearance" {
                self.check_argument_type(CALLEE, name, &DimType::Length, arg);
                if eval_const_preview(&arg.value).is_some_and(|v| !v.is_finite() || v < 0.0) {
                    self.emit(
                        "E103_INVALID_VALUE",
                        Severity::Error,
                        "`min_clearance` must be a finite, nonnegative length",
                        arg.value.span(),
                    );
                }
            } else if !matches!(
                arg.value.ty(),
                DimType::Solid | DimType::Component | DimType::Unknown
            ) {
                self.emit(
                    "E_ARG_TYPE_MISMATCH",
                    Severity::Error,
                    format!(
                        "`{CALLEE}` parameter `{name}` expects `Solid` or `Component`, found `{}`",
                        arg.value.ty()
                    ),
                    arg.value.span(),
                );
            }
        }
        for (name, supplied) in REQUIRED.into_iter().zip(seen) {
            if !supplied {
                self.emit(
                    "E_ARG_MISSING",
                    Severity::Error,
                    format!("`{CALLEE}` requires parameter `{name}`"),
                    span,
                );
            }
        }
    }

    fn check_argument_type(&mut self, callee: &str, name: &str, expected: &DimType, arg: &IrArg) {
        let actual = arg.value.ty();
        if !types_compatible(expected, actual) {
            let code = if *expected == DimType::Length {
                "E102_UNIT_MISMATCH"
            } else {
                "E_ARG_TYPE_MISMATCH"
            };
            self.emit(
                code,
                Severity::Error,
                format!("`{callee}` parameter `{name}` expects `{expected}`, found `{actual}`"),
                arg.value.span(),
            );
        }
    }

    fn validate_call_args(&mut self, callee: &str, args: &[IrArg], span: Span) {
        if callee == "path::helix" {
            let has_turns_or_height = args.iter().enumerate().any(|(idx, a)| {
                matches!(a.name.as_deref(), Some("turns" | "height" | "h"))
                    || (a.name.is_none() && idx == 2)
            });
            if !has_turns_or_height {
                self.emit(
                    "E103_INVALID_VALUE",
                    Severity::Error,
                    "call to `path::helix` requires at least one of `turns` or `height` (or `h`)",
                    span,
                );
            }
        } else if callee == "path::spline3d" {
            let has_points = args.iter().enumerate().any(|(idx, a)| {
                matches!(
                    a.name.as_deref(),
                    Some("points" | "control_points" | "pts")
                ) || (a.name.is_none() && idx == 0)
            });
            if !has_points {
                self.emit(
                    "E103_INVALID_VALUE",
                    Severity::Error,
                    "call to `path::spline3d` requires `points` (or `control_points` / `pts`)",
                    span,
                );
            }
        }

        for (idx, arg) in args.iter().enumerate() {
            let effective_name = arg.name.as_deref().or_else(|| match (callee, idx) {
                ("spline" | "profile::spline" | "path::spline3d" | "path::polyline", 0) => {
                    Some("points")
                }
                ("path::helix", 0) => Some("radius"),
                ("path::helix", 1) => Some("pitch"),
                ("path::helix", 2) => Some("turns"),
                ("path::helix", 3) => Some("origin"),
                _ => None,
            });
            let Some(name) = effective_name else {
                continue;
            };
            let ty = arg.value.ty();
            if *ty == DimType::Unknown {
                continue;
            }
            match (callee, name) {
                (
                    "cylinder" | "box" | "extrude" | "extrude_symmetric" | "fillet" | "chamfer"
                    | "shell" | "hole::simple" | "hole::counterbore" | "hole::tapped"
                    | "array::polar" | "array::linear" | "profile::circle",
                    "d" | "h" | "r" | "height" | "thickness" | "depth" | "d_through" | "d_cbore"
                    | "depth_cbore" | "radius" | "spacing" | "min" | "max",
                ) => {
                    if *ty != DimType::Length {
                        self.emit(
                            "E102_UNIT_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `{name}` of `{callee}` expects `Length`, found `{ty}`"
                            ),
                            arg.span,
                        );
                    }
                }
                ("circle", "d" | "diameter" | "radius")
                | ("loft", "height")
                | ("mate::coincident", "offset") => {
                    if *ty != DimType::Length {
                        self.emit(
                            "E102_UNIT_MISMATCH",
                            Severity::Error,
                            format!("argument `{name}` of `{callee}` expects `Length`, found `{ty}`"),
                            arg.span,
                        );
                    }
                }
                (
                    "ellipse" | "profile::ellipse",
                    "rx" | "ry" | "major_radius" | "minor_radius" | "a" | "b",
                ) => {
                    if *ty != DimType::Length {
                        self.emit(
                            "E102_UNIT_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `{name}` of `{callee}` expects `Length`, found `{ty}`"
                            ),
                            arg.span,
                        );
                    } else if let Some(v) = eval_const_preview(&arg.value) {
                        if !v.is_finite() || v <= 0.0 {
                            self.emit(
                                "E103_INVALID_VALUE",
                                Severity::Error,
                                format!(
                                    "argument `{name}` of `{callee}` must be positive, found {v}"
                                ),
                                arg.span,
                            );
                        }
                    }
                }
                ("ellipse" | "profile::ellipse", "segments") => {
                    if !ty.is_numeric_scalar() {
                        self.emit(
                            "E_ARG_TYPE_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `segments` of `{callee}` expects integer/scalar, found `{ty}`"
                            ),
                            arg.span,
                        );
                    }
                }
                ("profile::ellipse" | "profile::spline", "plane") => {
                    if *ty != DimType::Plane {
                        self.emit(
                            "E_ARG_TYPE_MISMATCH",
                            Severity::Error,
                            format!("argument `plane` of `{callee}` expects `Plane`, found `{ty}`"),
                            arg.span,
                        );
                    }
                }
                ("spline", "plane") => {
                    self.emit(
                        "E_ARG_TYPE_MISMATCH",
                        Severity::Error,
                        "argument `plane` is only supported on `profile::spline`, not sketch entity `spline`",
                        arg.span,
                    );
                }
                (
                    "spline" | "profile::spline",
                    "points" | "control_points" | "pts",
                ) => {
                    self.check_spline_points(callee, name, &arg.value, arg.span);
                }
                (
                    "path::spline3d",
                    "points" | "control_points" | "pts",
                ) => {
                    self.check_spline3d_points(callee, name, &arg.value, arg.span);
                }
                ("spline" | "profile::spline", "closed") => {
                    if *ty != DimType::Bool {
                        self.emit(
                            "E_ARG_TYPE_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `closed` of `{callee}` expects `bool`, found `{ty}`"
                            ),
                            arg.span,
                        );
                    }
                }
                (
                    "spline" | "profile::spline" | "path::spline3d",
                    "segments_per_span" | "segments",
                ) => {
                    if !ty.is_numeric_scalar() {
                        self.emit(
                            "E_ARG_TYPE_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `{name}` of `{callee}` expects integer/scalar, found `{ty}`"
                            ),
                            arg.span,
                        );
                    } else if let Some(v) = eval_const_preview(&arg.value) {
                        if !v.is_finite() || v <= 0.0 {
                            self.emit(
                                "E103_INVALID_VALUE",
                                Severity::Error,
                                format!(
                                    "argument `{name}` of `{callee}` must be positive, found {v}"
                                ),
                                arg.span,
                            );
                        }
                    }
                }
                (
                    "path::helix",
                    "radius" | "r" | "d" | "diameter" | "pitch" | "height" | "h",
                ) => {
                    if *ty != DimType::Length {
                        self.emit(
                            "E102_UNIT_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `{name}` of `{callee}` expects `Length`, found `{ty}`"
                            ),
                            arg.span,
                        );
                    } else if let Some(v) = eval_const_preview(&arg.value) {
                        if !v.is_finite() || v <= 0.0 {
                            self.emit(
                                "E103_INVALID_VALUE",
                                Severity::Error,
                                format!(
                                    "argument `{name}` of `{callee}` must be positive, found {v}"
                                ),
                                arg.span,
                            );
                        }
                    }
                }
                (
                    "path::helix",
                    "turns" | "segments_per_turn" | "segments",
                ) => {
                    if !ty.is_numeric_scalar() {
                        self.emit(
                            "E_ARG_TYPE_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `{name}` of `{callee}` expects integer/scalar, found `{ty}`"
                            ),
                            arg.span,
                        );
                    } else if let Some(v) = eval_const_preview(&arg.value) {
                        if !v.is_finite() || v <= 0.0 {
                            self.emit(
                                "E103_INVALID_VALUE",
                                Severity::Error,
                                format!(
                                    "argument `{name}` of `{callee}` must be positive, found {v}"
                                ),
                                arg.span,
                            );
                        }
                    }
                }
                ("path::helix", "origin" | "center") => {
                    self.check_dim_vector(callee, name, &arg.value, &DimType::Length, 3, arg.span);
                }
                ("path::helix", "left_handed") => {
                    if *ty != DimType::Bool {
                        self.emit(
                            "E_ARG_TYPE_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `left_handed` of `{callee}` expects `bool`, found `{ty}`"
                            ),
                            arg.span,
                        );
                    }
                }
                ("loft", "offsets") => {
                    if !matches!(ty, DimType::Array(inner) if **inner == DimType::Length) {
                        self.emit("E102_UNIT_MISMATCH", Severity::Error, "loft offsets must be lengths", arg.span);
                    }
                }
                ("sweep", "path") => {
                    if let IrExpr::Array { elements, .. } = &arg.value {
                        for point in elements {
                            self.check_dim_vector(callee, name, point, &DimType::Length, 3, arg.span);
                        }
                    }
                }
                ("revolve" | "array::polar", "angle" | "start_angle") => {
                    if *ty != DimType::Angle {
                        self.emit(
                            "E102_UNIT_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `{name}` of `{callee}` expects `Angle`, found `{ty}`"
                            ),
                            arg.span,
                        );
                    }
                }
                ("array::polar" | "array::linear", "count") => {
                    if !ty.is_numeric_scalar() {
                        self.emit(
                            "E_ARG_TYPE_MISMATCH",
                            Severity::Error,
                            format!("argument `count` expects integer/scalar, found `{ty}`"),
                            arg.span,
                        );
                    }
                }
                // R05: 3D Length vectors
                (
                    "box" | "cylinder" | "hole::simple" | "hole::counterbore" | "hole::tapped"
                    | "assert::bbox" | "component",
                    "size" | "origin" | "max_size" | "min" | "max",
                ) => {
                    self.check_dim_vector(callee, name, &arg.value, &DimType::Length, 3, arg.span);
                }
                // R05: 2D Length vectors inside 2D sketches
                ("circle" | "rectangle", "center" | "origin" | "size")
                | ("ellipse" | "profile::ellipse", "center" | "origin") => {
                    self.check_dim_vector(callee, name, &arg.value, &DimType::Length, 2, arg.span);
                }
                // R05: 3D Force vector
                ("force::distributed", "vector") => {
                    self.check_dim_vector(callee, name, &arg.value, &DimType::Force, 3, arg.span);
                }
                // R05: 3D Angle rotation vector
                ("component", "rotation") => {
                    self.check_dim_vector(callee, name, &arg.value, &DimType::Angle, 3, arg.span);
                }
                // R05: Axis / direction validation
                (
                    "cylinder" | "revolve" | "hole::simple" | "hole::tapped" | "array::linear"
                    | "array::polar" | "mate::coaxial",
                    "axis",
                ) => {
                    let ok = match ty {
                        DimType::Axis | DimType::Direction => true,
                        DimType::Array(inner) if inner.is_numeric_scalar() => {
                            if let IrExpr::Array { elements, .. } = &arg.value {
                                elements.len() == 3
                            } else {
                                true
                            }
                        }
                        _ => false,
                    };
                    if !ok {
                        self.emit(
                            "E102_UNIT_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `axis` of `{callee}` expects `Axis`, `Direction`, or 3-element scalar vector, found `{ty}`"
                            ),
                            arg.span,
                        );
                    }
                }
                ("assert::dfm_accessible", "axes") => {
                    let ok = matches!(
                        ty,
                        DimType::Array(inner) if matches!(**inner, DimType::Axis | DimType::Direction)
                    );
                    if !ok {
                        self.emit(
                            "E102_UNIT_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `axes` of `{callee}` expects `[Direction]` or `[Axis]`, found `{ty}`"
                            ),
                            arg.span,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn check_dim_vector(
        &mut self,
        callee: &str,
        arg_name: &str,
        expr: &IrExpr,
        expected_elem: &DimType,
        expected_len: usize,
        span: Span,
    ) {
        let ty = expr.ty();
        match ty {
            DimType::Array(inner) => {
                if **inner != DimType::Unknown && !types_compatible(expected_elem, inner) {
                    self.emit(
                        "E102_UNIT_MISMATCH",
                        Severity::Error,
                        format!(
                            "argument `{arg_name}` of `{callee}` expects `[{expected_elem}; {expected_len}]`, found `[{inner}]`"
                        ),
                        span,
                    );
                    return;
                }
                if let IrExpr::Array { elements, .. } = expr {
                    if elements.len() != expected_len {
                        self.emit(
                            "E102_UNIT_MISMATCH",
                            Severity::Error,
                            format!(
                                "argument `{arg_name}` of `{callee}` expects {expected_len} elements, found {}",
                                elements.len()
                            ),
                            span,
                        );
                        return;
                    }
                    for elem in elements {
                        let et = elem.ty();
                        if *et != DimType::Unknown && !types_compatible(expected_elem, et) {
                            self.emit(
                                "E102_UNIT_MISMATCH",
                                Severity::Error,
                                format!(
                                    "element of `{arg_name}` in `{callee}` expects `{expected_elem}`, found `{et}`"
                                ),
                                elem.span(),
                            );
                        }
                    }
                }
            }
            DimType::Unknown => {}
            other => {
                self.emit(
                    "E102_UNIT_MISMATCH",
                    Severity::Error,
                    format!(
                        "argument `{arg_name}` of `{callee}` expects `[{expected_elem}; {expected_len}]`, found `{other}`"
                    ),
                    span,
                );
            }
        }
    }

    fn check_spline_points(
        &mut self,
        callee: &str,
        arg_name: &str,
        expr: &IrExpr,
        span: Span,
    ) {
        if let IrExpr::Array { elements, .. } = expr {
            for elem in elements {
                let et = elem.ty();
                if *et == DimType::SketchPoint || *et == DimType::Unknown {
                    continue;
                }
                self.check_dim_vector(callee, arg_name, elem, &DimType::Length, 2, elem.span());
            }
            if elements.len() < 3 {
                self.emit(
                    "E103_INVALID_VALUE",
                    Severity::Error,
                    format!(
                        "argument `{arg_name}` of `{callee}` requires at least 3 control points, found {}",
                        elements.len()
                    ),
                    span,
                );
            }
            return;
        }

        match expr.ty() {
            DimType::Unknown => {}
            DimType::Array(inner) => {
                let ok = match inner.as_ref() {
                    DimType::SketchPoint | DimType::Unknown => true,
                    DimType::Array(coord) => {
                        matches!(coord.as_ref(), DimType::Length | DimType::Unknown)
                    }
                    _ => false,
                };
                if !ok {
                    self.emit(
                        "E102_UNIT_MISMATCH",
                        Severity::Error,
                        format!(
                            "argument `{arg_name}` of `{callee}` expects `[Point2D]` or `[[Length; 2]]`, found `{}`",
                            expr.ty()
                        ),
                        span,
                    );
                }
            }
            other => {
                self.emit(
                    "E102_UNIT_MISMATCH",
                    Severity::Error,
                    format!(
                        "argument `{arg_name}` of `{callee}` expects `[Point2D]` or `[[Length; 2]]`, found `{other}`"
                    ),
                    span,
                );
            }
        }
    }

    fn check_spline3d_points(
        &mut self,
        callee: &str,
        arg_name: &str,
        expr: &IrExpr,
        span: Span,
    ) {
        if let IrExpr::Array { elements, .. } = expr {
            for elem in elements {
                if *elem.ty() == DimType::Unknown {
                    continue;
                }
                self.check_dim_vector(callee, arg_name, elem, &DimType::Length, 3, elem.span());
            }
            if elements.len() < 3 {
                self.emit(
                    "E103_INVALID_VALUE",
                    Severity::Error,
                    format!(
                        "argument `{arg_name}` of `{callee}` requires at least 3 3D control points, found {}",
                        elements.len()
                    ),
                    span,
                );
            }
            return;
        }

        match expr.ty() {
            DimType::Unknown => {}
            DimType::Array(inner) => {
                let ok = match inner.as_ref() {
                    DimType::Unknown => true,
                    DimType::Array(coord) => {
                        matches!(coord.as_ref(), DimType::Length | DimType::Unknown)
                    }
                    _ => false,
                };
                if !ok {
                    self.emit(
                        "E102_UNIT_MISMATCH",
                        Severity::Error,
                        format!(
                            "argument `{arg_name}` of `{callee}` expects `[[Length; 3]]`, found `{}`",
                            expr.ty()
                        ),
                        span,
                    );
                }
            }
            other => {
                self.emit(
                    "E102_UNIT_MISMATCH",
                    Severity::Error,
                    format!(
                        "argument `{arg_name}` of `{callee}` expects `[[Length; 3]]`, found `{other}`"
                    ),
                    span,
                );
            }
        }
    }

    fn check_sketch(&mut self, args: &[CallArg], body: &cad_syntax::Block, span: Span) -> IrExpr {
        let checked_args: Vec<IrArg> = args
            .iter()
            .map(|a| IrArg {
                name: a.name.clone(),
                value: self.check_expr(&a.value),
                span: a.span,
            })
            .collect();

        self.push_scope();
        let mut stmts = Vec::new();
        for stmt in &body.stmts {
            match stmt {
                Stmt::Let(LetStmt {
                    name,
                    name_span,
                    value,
                    span: s_span,
                }) => {
                    let ir_expr = self.check_expr(value);
                    let ty = ir_expr.ty().clone();
                    self.insert_var(name.clone(), ty.clone());
                    self.hover_symbols.push(HoverInfo {
                        symbol: name.clone(),
                        dim_type: ty,
                        sync_tier: Some(classify_expr(value).tier),
                        canonical_unit: String::new(),
                        span: *name_span,
                    });
                    stmts.push(IrSketchStmt::Let {
                        name: name.clone(),
                        expr: ir_expr,
                        span: *s_span,
                    });
                }
                Stmt::Expr(expr) => {
                    let ir_expr = self.check_expr(expr);
                    stmts.push(IrSketchStmt::Expr {
                        span: expr.span(),
                        expr: ir_expr,
                    });
                }
                Stmt::Return(ret) => {
                    self.emit(
                        "E_SKETCH_RETURN",
                        Severity::Error,
                        "`return` is not allowed inside a `sketch` block",
                        ret.span,
                    );
                }
            }
        }
        self.pop_scope();

        IrExpr::Sketch {
            args: checked_args,
            stmts,
            span,
        }
    }
}

fn types_compatible(expected: &DimType, actual: &DimType) -> bool {
    if expected == actual || *expected == DimType::Unknown || *actual == DimType::Unknown {
        return true;
    }
    if expected.is_numeric_scalar() && actual.is_numeric_scalar() {
        return true;
    }
    if let (DimType::Array(e), DimType::Array(a)) = (expected, actual) {
        return types_compatible(e, a);
    }
    false
}

fn infer_method_type(recv: &DimType, method: &str) -> DimType {
    match method {
        "faces" | "planar" | "cylindrical" | "conical" | "freeform" | "filter" | "tagged" => {
            DimType::FaceSet
        }
        "edges" => DimType::EdgeSet,
        "max_by" | "min_by" | "nearest_to" | "then_by" => match recv {
            DimType::EdgeSet => DimType::EdgeSet,
            _ => DimType::FaceSet,
        },
        _ => DimType::Unknown,
    }
}

fn infer_call_return_type(callee: &str, piped_input: Option<&DimType>) -> DimType {
    match callee {
        "box" | "cylinder" | "extrude" | "extrude_symmetric" | "revolve" | "sweep" | "loft"
        | "shell" | "mirror" | "fillet" | "chamfer" | "hole::simple" | "hole::counterbore"
        | "hole::tapped" | "array::linear" | "array::polar" | "boolean::union"
        | "boolean::subtract" | "implicit::smooth_union" | "import::step" | "import::mesh" => {
            DimType::Solid
        }
        "profile::circle" | "profile::rect" | "profile::ellipse" | "profile::spline" => {
            DimType::Sketch2D
        }
        "path::polyline" | "path::line" | "path::arc" | "path::helix" | "path::spline3d" => {
            DimType::Array(Box::new(DimType::Array(Box::new(DimType::Length))))
        }
        "face::offset" | "face::move" | "face::delete" => DimType::Solid,
        "component" => DimType::Component,
        "assembly::merge" => DimType::Assembly,
        _ if callee.starts_with("mate::") => DimType::Bool,
        "tag::faces" | "tag::cylindrical_faces" => {
            piped_input.cloned().unwrap_or(DimType::Solid)
        }
        "pmi::datum" | "pmi::perpendicularity" | "pmi::flatness" | "pmi::tolerance" => {
            piped_input.cloned().unwrap_or(DimType::FaceSet)
        }
        "point" => DimType::SketchPoint,
        "line" | "arc" | "circle" | "ellipse" | "spline" | "rectangle" | "region::closed"
        | "region::path" => {
            DimType::SketchEntity
        }
        "field::stress_driven" => DimType::Length,
        "force::distributed" => DimType::ForceLoad,
        _ if callee.starts_with("constraint::") || callee.starts_with("assert::") => DimType::Bool,
        _ => DimType::Unknown,
    }
}

fn eval_const_preview(expr: &IrExpr) -> Option<f64> {
    match expr {
        IrExpr::Quantity {
            canonical_value, ..
        } => Some(*canonical_value),
        IrExpr::Unary { op, operand, .. } => {
            let v = eval_const_preview(operand)?;
            match op {
                UnaryOp::Plus => Some(v),
                UnaryOp::Minus => Some(-v),
                UnaryOp::Not => None,
            }
        }
        IrExpr::Binary {
            op, left, right, ..
        } => {
            let l = eval_const_preview(left)?;
            let r = eval_const_preview(right)?;
            match op {
                BinaryOp::Add => Some(l + r),
                BinaryOp::Sub => Some(l - r),
                BinaryOp::Mul => Some(l * r),
                BinaryOp::Div if r.abs() > 1e-12 => Some(l / r),
                _ => None,
            }
        }
        _ => None,
    }
}

pub fn check_and_lower(file: &SourceFile) -> CheckOutput {
    let mut checker = Checker::new();
    let module = checker.check_source_file(file);
    CheckOutput {
        module,
        diagnostics: checker.diagnostics,
        import_argument_hovers: checker.import_argument_hovers,
    }
}
