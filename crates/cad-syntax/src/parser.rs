use crate::ast::*;
use crate::lexer::{Token, TokenKind};
use crate::span::Span;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntaxError {
    pub message: String,
    pub span: Span,
}

pub struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    pub errors: Vec<SyntaxError>,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Token]) -> Self {
        Self {
            tokens,
            pos: 0,
            errors: Vec::new(),
        }
    }

    fn peek(&self) -> &'a Token {
        self.tokens.get(self.pos).unwrap_or_else(|| self.tokens.last().unwrap())
    }

    fn peek_next(&self) -> &'a Token {
        self.tokens
            .get(self.pos + 1)
            .unwrap_or_else(|| self.tokens.last().unwrap())
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    fn advance(&mut self) -> &'a Token {
        let tok = self.peek();
        if !matches!(tok.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        tok
    }

    fn prev_span(&self) -> Span {
        if self.pos > 0 {
            self.tokens[self.pos - 1].span
        } else {
            self.peek().span
        }
    }

    fn eat_semi(&mut self) {
        while matches!(self.peek().kind, TokenKind::Semi) {
            self.advance();
        }
    }

    fn expect(&mut self, check: impl Fn(&TokenKind) -> bool, expected: &str) -> Option<Token> {
        if check(&self.peek().kind) {
            Some(self.advance().clone())
        } else {
            let span = self.peek().span;
            let got = &self.peek().lexeme;
            self.errors.push(SyntaxError {
                message: format!("expected {expected}, found `{got}`"),
                span,
            });
            None
        }
    }

    fn expect_ident(&mut self, expected: &str) -> Option<(String, Span)> {
        match &self.peek().kind {
            TokenKind::Ident(name) => {
                let name = name.clone();
                let span = self.advance().span;
                Some((name, span))
            }
            _ => {
                let span = self.peek().span;
                let got = &self.peek().lexeme;
                self.errors.push(SyntaxError {
                    message: format!("expected {expected}, found `{got}`"),
                    span,
                });
                None
            }
        }
    }

    pub fn parse_source_file(&mut self) -> SourceFile {
        let mut annotations = Vec::new();
        while matches!(self.peek().kind, TokenKind::At) {
            annotations.push(self.parse_annotation());
        }

        let mut items = Vec::new();
        while !self.at_eof() {
            // Skip stray annotations before items if any
            if matches!(self.peek().kind, TokenKind::At) {
                annotations.push(self.parse_annotation());
                continue;
            }
            match self.peek().kind {
                TokenKind::Part => items.push(TopItem::Part(self.parse_part_decl())),
                TokenKind::Assembly => items.push(TopItem::Assembly(self.parse_assembly_decl())),
                TokenKind::Const | TokenKind::Let => {
                    items.push(TopItem::Const(self.parse_const_or_let()));
                    self.eat_semi();
                }
                _ => {
                    let span = self.peek().span;
                    self.errors.push(SyntaxError {
                        message: format!(
                            "unexpected top-level token `{}`",
                            self.peek().lexeme
                        ),
                        span,
                    });
                    self.advance();
                }
            }
        }

        SourceFile { annotations, items }
    }

    fn parse_annotation(&mut self) -> Annotation {
        let start = self.advance().span; // '@'
        let (segments, path_span) = self.parse_path_segments();
        let mut end_span = path_span;
        let args = if matches!(self.peek().kind, TokenKind::LParen) {
            let (parsed_args, close_span) = self.parse_arg_list();
            end_span = close_span;
            parsed_args
        } else {
            Vec::new()
        };
        Annotation {
            path: segments,
            args,
            span: start.merge(end_span),
        }
    }

    fn parse_path_segments(&mut self) -> (Vec<String>, Span) {
        let Some((first, first_span)) = self.expect_ident("identifier") else {
            return (vec!["<error>".to_string()], self.peek().span);
        };
        let mut segments = vec![first];
        let mut span = first_span;
        while matches!(self.peek().kind, TokenKind::ColonColon) {
            self.advance();
            if let Some((seg, seg_span)) = self.expect_ident("path segment") {
                span = span.merge(seg_span);
                segments.push(seg);
            } else {
                break;
            }
        }
        (segments, span)
    }

    fn parse_part_decl(&mut self) -> PartDecl {
        let start = self.advance().span; // 'part'
        let (name, _) = self
            .expect_ident("part name")
            .unwrap_or_else(|| ("<unnamed>".to_string(), start));

        let mut params = Vec::new();
        if self
            .expect(|k| matches!(k, TokenKind::LParen), "`(`")
            .is_some()
        {
            while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
                params.push(self.parse_param_decl());
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            self.expect(|k| matches!(k, TokenKind::RParen), "`)`");
        }

        let return_type = if matches!(self.peek().kind, TokenKind::Arrow) {
            self.advance();
            self.expect_ident("return type").map(|(t, _)| t)
        } else {
            None
        };

        let body = self.parse_block();
        let span = start.merge(body.span);
        PartDecl {
            name,
            params,
            return_type,
            body,
            span,
        }
    }

    fn parse_assembly_decl(&mut self) -> AssemblyDecl {
        let start = self.advance().span; // 'assembly'
        let (name, _) = self
            .expect_ident("assembly name")
            .unwrap_or_else(|| ("<unnamed_assembly>".to_string(), start));

        let mut params = Vec::new();
        if self
            .expect(|k| matches!(k, TokenKind::LParen), "`(`")
            .is_some()
        {
            while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
                params.push(self.parse_param_decl());
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            self.expect(|k| matches!(k, TokenKind::RParen), "`)`");
        }

        let return_type = if matches!(self.peek().kind, TokenKind::Arrow) {
            self.advance();
            self.expect_ident("return type").map(|(t, _)| t)
        } else {
            None
        };

        let body = self.parse_block();
        let span = start.merge(body.span);
        AssemblyDecl {
            name,
            params,
            return_type,
            body,
            span,
        }
    }

    fn parse_param_decl(&mut self) -> ParamDecl {
        let (name, start) = self
            .expect_ident("parameter name")
            .unwrap_or_else(|| ("_".to_string(), self.peek().span));
        let mut end = start;

        let ty = if self
            .expect(|k| matches!(k, TokenKind::Colon), "`:`")
            .is_some()
        {
            if let Some((ty_name, ty_span)) = self.expect_ident("parameter type") {
                end = ty_span;
                ty_name
            } else {
                "Unknown".to_string()
            }
        } else {
            "Unknown".to_string()
        };

        let mut annotations = Vec::new();
        while matches!(self.peek().kind, TokenKind::At) {
            let ann = self.parse_annotation();
            end = ann.span;
            annotations.push(ann);
        }

        let default = if matches!(self.peek().kind, TokenKind::Eq) {
            self.advance();
            let expr = self.parse_expr();
            end = expr.span();
            Some(expr)
        } else {
            None
        };

        while matches!(self.peek().kind, TokenKind::At) {
            let ann = self.parse_annotation();
            end = ann.span;
            annotations.push(ann);
        }

        ParamDecl {
            name,
            ty,
            default,
            annotations,
            span: start.merge(end),
        }
    }

    fn parse_block(&mut self) -> Block {
        let start = self
            .expect(|k| matches!(k, TokenKind::LBrace), "`{`")
            .map(|t| t.span)
            .unwrap_or_else(|| self.peek().span);

        let mut stmts = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            let prev_pos = self.pos;
            stmts.push(self.parse_stmt());
            self.eat_semi();
            if self.pos == prev_pos {
                self.advance();
            }
        }

        let end = self
            .expect(|k| matches!(k, TokenKind::RBrace), "`}`")
            .map(|t| t.span)
            .unwrap_or_else(|| self.prev_span());

        Block {
            stmts,
            span: start.merge(end),
        }
    }

    fn parse_const_or_let(&mut self) -> LetStmt {
        let start = self.advance().span; // let / const
        let (name, name_span) = self
            .expect_ident("variable name")
            .unwrap_or_else(|| ("_".to_string(), start));
        self.expect(|k| matches!(k, TokenKind::Eq), "`=`");
        let value = self.parse_expr();
        let span = start.merge(value.span());
        LetStmt {
            name,
            name_span,
            value,
            span,
        }
    }

    fn parse_stmt(&mut self) -> Stmt {
        match self.peek().kind {
            TokenKind::Let | TokenKind::Const => Stmt::Let(self.parse_const_or_let()),
            TokenKind::Return => {
                let start = self.advance().span;
                let value = self.parse_expr();
                let span = start.merge(value.span());
                Stmt::Return(ReturnStmt { value, span })
            }
            _ => Stmt::Expr(self.parse_expr()),
        }
    }

    pub fn parse_expr(&mut self) -> Expr {
        self.parse_pipe()
    }

    fn parse_pipe(&mut self) -> Expr {
        let mut expr = self.parse_or();
        while matches!(self.peek().kind, TokenKind::PipeOp) {
            self.advance();
            let rhs = self.parse_or();
            let span = expr.span().merge(rhs.span());
            expr = Expr::Pipe {
                left: Box::new(expr),
                right: Box::new(rhs),
                span,
            };
        }
        expr
    }

    fn parse_or(&mut self) -> Expr {
        let mut expr = self.parse_and();
        while matches!(self.peek().kind, TokenKind::OrOr) {
            self.advance();
            let rhs = self.parse_and();
            let span = expr.span().merge(rhs.span());
            expr = Expr::Binary {
                op: BinaryOp::Or,
                left: Box::new(expr),
                right: Box::new(rhs),
                span,
            };
        }
        expr
    }

    fn parse_and(&mut self) -> Expr {
        let mut expr = self.parse_cmp();
        while matches!(self.peek().kind, TokenKind::AndAnd) {
            self.advance();
            let rhs = self.parse_cmp();
            let span = expr.span().merge(rhs.span());
            expr = Expr::Binary {
                op: BinaryOp::And,
                left: Box::new(expr),
                right: Box::new(rhs),
                span,
            };
        }
        expr
    }

    fn parse_cmp(&mut self) -> Expr {
        let mut expr = self.parse_add();
        loop {
            let op = match self.peek().kind {
                TokenKind::EqEq => BinaryOp::Eq,
                TokenKind::BangEq => BinaryOp::Ne,
                TokenKind::Lt => BinaryOp::Lt,
                TokenKind::LtEq => BinaryOp::Le,
                TokenKind::Gt => BinaryOp::Gt,
                TokenKind::GtEq => BinaryOp::Ge,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_add();
            let span = expr.span().merge(rhs.span());
            expr = Expr::Binary {
                op,
                left: Box::new(expr),
                right: Box::new(rhs),
                span,
            };
        }
        expr
    }

    fn parse_add(&mut self) -> Expr {
        let mut expr = self.parse_mul();
        loop {
            let op = match self.peek().kind {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Sub,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_mul();
            let span = expr.span().merge(rhs.span());
            expr = Expr::Binary {
                op,
                left: Box::new(expr),
                right: Box::new(rhs),
                span,
            };
        }
        expr
    }

    fn parse_mul(&mut self) -> Expr {
        let mut expr = self.parse_unary();
        loop {
            let op = match self.peek().kind {
                TokenKind::Star => BinaryOp::Mul,
                TokenKind::Slash => BinaryOp::Div,
                TokenKind::Percent => BinaryOp::Rem,
                _ => break,
            };
            self.advance();
            let rhs = self.parse_unary();
            let span = expr.span().merge(rhs.span());
            expr = Expr::Binary {
                op,
                left: Box::new(expr),
                right: Box::new(rhs),
                span,
            };
        }
        expr
    }

    fn parse_unary(&mut self) -> Expr {
        let op_opt = match self.peek().kind {
            TokenKind::Plus => Some(UnaryOp::Plus),
            TokenKind::Minus => Some(UnaryOp::Minus),
            TokenKind::Bang => Some(UnaryOp::Not),
            _ => None,
        };
        if let Some(op) = op_opt {
            let start = self.advance().span;
            let operand = self.parse_unary();
            let span = start.merge(operand.span());
            return Expr::Unary {
                op,
                expr: Box::new(operand),
                span,
            };
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Expr {
        let mut expr = self.parse_primary();
        loop {
            match self.peek().kind {
                TokenKind::LParen => {
                    let (args, close_span) = self.parse_arg_list();
                    let span = expr.span().merge(close_span);
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args,
                        span,
                    };
                }
                TokenKind::Dot => {
                    self.advance();
                    let (member, member_span) = self
                        .expect_ident("method or field name")
                        .unwrap_or_else(|| ("<error>".to_string(), self.peek().span));
                    if matches!(self.peek().kind, TokenKind::LParen) {
                        let (args, close_span) = self.parse_arg_list();
                        let span = expr.span().merge(close_span);
                        expr = Expr::MethodCall {
                            receiver: Box::new(expr),
                            method: member,
                            args,
                            span,
                        };
                    } else {
                        let span = expr.span().merge(member_span);
                        expr = Expr::FieldAccess {
                            receiver: Box::new(expr),
                            field: member,
                            span,
                        };
                    }
                }
                _ => break,
            }
        }
        expr
    }

    fn parse_primary(&mut self) -> Expr {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Number {
                value,
                raw_number,
                unit,
            } => {
                self.advance();
                Expr::Quantity {
                    value,
                    raw_number,
                    unit,
                    span: tok.span,
                }
            }
            TokenKind::Str(s) => {
                self.advance();
                Expr::Str {
                    value: s,
                    span: tok.span,
                }
            }
            TokenKind::True => {
                self.advance();
                Expr::Bool {
                    value: true,
                    span: tok.span,
                }
            }
            TokenKind::False => {
                self.advance();
                Expr::Bool {
                    value: false,
                    span: tok.span,
                }
            }
            TokenKind::Sketch => {
                let start = self.advance().span;
                let args = if matches!(self.peek().kind, TokenKind::LParen) {
                    self.parse_arg_list().0
                } else {
                    Vec::new()
                };
                let body = self.parse_block();
                let span = start.merge(body.span);
                Expr::Sketch { args, body, span }
            }
            TokenKind::Ident(_) => {
                let (segments, span) = self.parse_path_segments();
                Expr::Path { segments, span }
            }
            TokenKind::LParen => {
                let start = self.advance().span;
                let inner = self.parse_expr();
                let end = self
                    .expect(|k| matches!(k, TokenKind::RParen), "`)`")
                    .map(|t| t.span)
                    .unwrap_or_else(|| inner.span());
                Expr::Paren {
                    inner: Box::new(inner),
                    span: start.merge(end),
                }
            }
            TokenKind::LBracket => {
                let start = self.advance().span;
                let mut elements = Vec::new();
                while !matches!(self.peek().kind, TokenKind::RBracket | TokenKind::Eof) {
                    elements.push(self.parse_expr());
                    if matches!(self.peek().kind, TokenKind::Comma) {
                        self.advance();
                    } else {
                        break;
                    }
                }
                let end = self
                    .expect(|k| matches!(k, TokenKind::RBracket), "`]`")
                    .map(|t| t.span)
                    .unwrap_or_else(|| self.prev_span());
                Expr::Array {
                    elements,
                    span: start.merge(end),
                }
            }
            _ => {
                self.errors.push(SyntaxError {
                    message: format!("expected expression, found `{}`", tok.lexeme),
                    span: tok.span,
                });
                self.advance();
                Expr::Path {
                    segments: vec!["<error>".to_string()],
                    span: tok.span,
                }
            }
        }
    }

    fn parse_arg_list(&mut self) -> (Vec<CallArg>, Span) {
        self.expect(|k| matches!(k, TokenKind::LParen), "`(`");
        let mut args = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
            let is_named = matches!(
                self.peek().kind,
                TokenKind::Ident(_) | TokenKind::Part | TokenKind::Sketch
            ) && matches!(self.peek_next().kind, TokenKind::Eq);
            if is_named {
                let (name, name_span) = match &self.peek().kind {
                    TokenKind::Ident(n) => (n.clone(), self.advance().span),
                    TokenKind::Part => ("part".to_string(), self.advance().span),
                    TokenKind::Sketch => ("sketch".to_string(), self.advance().span),
                    _ => unreachable!(),
                };
                self.advance(); // '='
                let value = self.parse_expr();
                let mut end_span = value.span();
                let mut annotations = Vec::new();
                while matches!(self.peek().kind, TokenKind::At) {
                    let ann = self.parse_annotation();
                    end_span = ann.span;
                    annotations.push(ann);
                }
                let span = name_span.merge(end_span);
                args.push(CallArg {
                    name: Some(name),
                    value,
                    annotations,
                    span,
                });
            } else {
                let value = self.parse_expr();
                let mut end_span = value.span();
                let mut annotations = Vec::new();
                while matches!(self.peek().kind, TokenKind::At) {
                    let ann = self.parse_annotation();
                    end_span = ann.span;
                    annotations.push(ann);
                }
                let span = value.span().merge(end_span);
                args.push(CallArg {
                    name: None,
                    value,
                    annotations,
                    span,
                });
            }
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        let close = self
            .expect(|k| matches!(k, TokenKind::RParen), "`)`")
            .map(|t| t.span)
            .unwrap_or_else(|| self.prev_span());
        (args, close)
    }
}
