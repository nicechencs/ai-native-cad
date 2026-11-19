use crate::span::Span;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceFile {
    pub annotations: Vec<Annotation>,
    pub items: Vec<TopItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TopItem {
    Part(PartDecl),
    Assembly(AssemblyDecl),
    Const(LetStmt),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssemblyDecl {
    pub name: String,
    pub params: Vec<ParamDecl>,
    pub return_type: Option<String>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub path: Vec<String>,
    pub args: Vec<CallArg>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartDecl {
    pub name: String,
    pub params: Vec<ParamDecl>,
    pub return_type: Option<String>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamDecl {
    pub name: String,
    pub ty: String,
    pub default: Option<Expr>,
    pub annotations: Vec<Annotation>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Stmt {
    Let(LetStmt),
    Return(ReturnStmt),
    Expr(Expr),
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let(s) => s.span,
            Stmt::Return(s) => s.span,
            Stmt::Expr(e) => e.span(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LetStmt {
    pub name: String,
    pub name_span: Span,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReturnStmt {
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallArg {
    pub name: Option<String>,
    pub value: Expr,
    pub annotations: Vec<Annotation>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOp {
    Plus,
    Minus,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Quantity {
        value: f64,
        raw_number: String,
        unit: Option<String>,
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
    Path {
        segments: Vec<String>,
        span: Span,
    },
    Array {
        elements: Vec<Expr>,
        span: Span,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<CallArg>,
        span: Span,
    },
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        args: Vec<CallArg>,
        span: Span,
    },
    FieldAccess {
        receiver: Box<Expr>,
        field: String,
        span: Span,
    },
    Sketch {
        args: Vec<CallArg>,
        body: Block,
        span: Span,
    },
    Pipe {
        left: Box<Expr>,
        right: Box<Expr>,
        span: Span,
    },
    Paren {
        inner: Box<Expr>,
        span: Span,
    },
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Quantity { span, .. }
            | Expr::Str { span, .. }
            | Expr::Bool { span, .. }
            | Expr::Path { span, .. }
            | Expr::Array { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Call { span, .. }
            | Expr::MethodCall { span, .. }
            | Expr::FieldAccess { span, .. }
            | Expr::Sketch { span, .. }
            | Expr::Pipe { span, .. }
            | Expr::Paren { span, .. } => *span,
        }
    }
}
