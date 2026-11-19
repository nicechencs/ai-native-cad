use crate::types::{DimType, EditHandleCandidate, HoverInfo, SyncTier};
use cad_syntax::Span;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IrModule {
    pub annotations: Vec<IrAnnotation>,
    pub constants: Vec<IrNode>,
    pub parts: Vec<IrPart>,
    pub assemblies: Vec<IrPart>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IrAnnotation {
    pub path: String,
    pub args: Vec<IrArg>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IrPart {
    pub name: String,
    pub params: Vec<IrParam>,
    pub return_type: DimType,
    pub nodes: Vec<IrNode>,
    pub return_node: Option<usize>,
    pub edit_handles: Vec<EditHandleCandidate>,
    pub hover_symbols: Vec<HoverInfo>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IrParam {
    pub name: String,
    pub ty: DimType,
    pub default_expr: Option<IrExpr>,
    pub annotations: Vec<IrAnnotation>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IrNode {
    pub id: usize,
    pub name: Option<String>,
    pub expr: IrExpr,
    pub ty: DimType,
    pub tier: SyncTier,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IrArg {
    pub name: Option<String>,
    pub value: IrExpr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IrExpr {
    Quantity {
        canonical_value: f64,
        unit: String,
        ty: DimType,
        span: Span,
    },
    Str {
        value: String,
        span: Span,
    },
    Bool {
        value: bool,
        span: Span,
    },
    Var {
        name: String,
        ty: DimType,
        span: Span,
    },
    ConstDir {
        name: String,
        ty: DimType,
        span: Span,
    },
    Array {
        elements: Vec<IrExpr>,
        ty: DimType,
        span: Span,
    },
    Unary {
        op: cad_syntax::UnaryOp,
        operand: Box<IrExpr>,
        ty: DimType,
        span: Span,
    },
    Binary {
        op: cad_syntax::BinaryOp,
        left: Box<IrExpr>,
        right: Box<IrExpr>,
        ty: DimType,
        span: Span,
    },
    Call {
        callee: String,
        args: Vec<IrArg>,
        ty: DimType,
        is_phase2_fallback: bool,
        span: Span,
    },
    MethodCall {
        receiver: Box<IrExpr>,
        method: String,
        args: Vec<IrArg>,
        ty: DimType,
        span: Span,
    },
    FieldAccess {
        receiver: Box<IrExpr>,
        field: String,
        ty: DimType,
        span: Span,
    },
    Sketch {
        args: Vec<IrArg>,
        stmts: Vec<IrSketchStmt>,
        span: Span,
    },
    Pipe {
        left: Box<IrExpr>,
        right: Box<IrExpr>,
        ty: DimType,
        span: Span,
    },
}

impl IrExpr {
    pub fn ty(&self) -> &DimType {
        match self {
            IrExpr::Quantity { ty, .. }
            | IrExpr::Var { ty, .. }
            | IrExpr::ConstDir { ty, .. }
            | IrExpr::Array { ty, .. }
            | IrExpr::Unary { ty, .. }
            | IrExpr::Binary { ty, .. }
            | IrExpr::Call { ty, .. }
            | IrExpr::MethodCall { ty, .. }
            | IrExpr::FieldAccess { ty, .. }
            | IrExpr::Pipe { ty, .. } => ty,
            IrExpr::Str { .. } => &DimType::Str,
            IrExpr::Bool { .. } => &DimType::Bool,
            IrExpr::Sketch { .. } => &DimType::Sketch2D,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            IrExpr::Quantity { span, .. }
            | IrExpr::Str { span, .. }
            | IrExpr::Bool { span, .. }
            | IrExpr::Var { span, .. }
            | IrExpr::ConstDir { span, .. }
            | IrExpr::Array { span, .. }
            | IrExpr::Unary { span, .. }
            | IrExpr::Binary { span, .. }
            | IrExpr::Call { span, .. }
            | IrExpr::MethodCall { span, .. }
            | IrExpr::FieldAccess { span, .. }
            | IrExpr::Sketch { span, .. }
            | IrExpr::Pipe { span, .. } => *span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IrSketchStmt {
    Let {
        name: String,
        expr: IrExpr,
        span: Span,
    },
    Expr {
        expr: IrExpr,
        span: Span,
    },
}

pub type Env = HashMap<String, DimType>;
