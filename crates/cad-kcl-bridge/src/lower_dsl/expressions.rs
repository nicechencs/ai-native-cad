use cad_hir::{DimType, IrExpr};
use cad_ir::{IrType, IrValue};
use cad_syntax::{BinaryOp, Span, UnaryOp};

use super::{Lowering, Result, at};
use crate::model::{self, SuffixParse};

impl Lowering {
    pub(super) fn value(&mut self, expr: &IrExpr, name: Option<&str>) -> Result<IrValue> {
        let span = expr.span();
        match expr {
            IrExpr::Quantity {
                canonical_value,
                ty,
                ..
            } => number(*canonical_value, ty, span),
            IrExpr::Str { value, .. } => Ok(IrValue::String(value.clone())),
            IrExpr::Bool { value, .. } => Ok(IrValue::Bool(*value)),
            IrExpr::Var { name, .. } => {
                if let Some(value) = self.bindings.get(name).cloned().or_else(|| builtin(name)) {
                    Ok(value)
                } else if self.parts.contains_key(name) {
                    self.instantiate_part(name, &std::collections::HashMap::new(), span)
                } else {
                    Err(at(span, format!("unsupported DSL reference `{name}`")))
                }
            }
            IrExpr::ConstDir { name, .. } => {
                Ok(builtin(name).unwrap_or_else(|| IrValue::String(name.clone())))
            }
            IrExpr::Array { elements, .. } => elements
                .iter()
                .map(|e| self.value(e, None))
                .collect::<Result<Vec<_>>>()
                .map(IrValue::List),
            IrExpr::Unary {
                op, operand, ty, ..
            } => {
                let operand = self.value(operand, None)?;
                if *op == UnaryOp::Plus {
                    return Ok(operand);
                }
                let literal = self.nominal(&operand);
                let value = match (op, literal) {
                    (UnaryOp::Minus, IrValue::Vec3(v)) => IrValue::Vec3(v.map(|x| -x)),
                    (UnaryOp::Minus, _) => number(-as_number(literal, span)?, ty, span)?,
                    (UnaryOp::Not, IrValue::Bool(value)) => IrValue::Bool(!value),
                    _ => return Err(at(span, "invalid DSL unary operand")),
                };
                let symbol = if *op == UnaryOp::Minus { "-" } else { "!" };
                let formula = self
                    .parameter(&operand)
                    .map(|parameter| format!("{symbol}({})", parameter_symbol(&parameter.name.0)));
                Ok(self.computed(name, value, formula))
            }
            IrExpr::Binary {
                op,
                left,
                right,
                ty,
                ..
            } => {
                let left = self.value(left, None)?;
                let right = self.value(right, None)?;
                let value = binary(*op, self.nominal(&left), self.nominal(&right), ty, span)?;
                let formula = (self.parameter(&left).is_some() || self.parameter(&right).is_some())
                    .then(|| {
                        Ok(format!(
                            "({} {} {})",
                            self.formula_atom(&left, span)?,
                            binary_symbol(*op),
                            self.formula_atom(&right, span)?
                        ))
                    })
                    .transpose()?;
                Ok(self.computed(name, value, formula))
            }
            IrExpr::Pipe { left, right, .. } => {
                let piped = self.value(left, None)?;
                match right.as_ref() {
                    IrExpr::Call {
                        callee, args, span, ..
                    } => self.call(callee, args, Some(piped), name, *span),
                    IrExpr::MethodCall {
                        method, args, span, ..
                    } => self.call(method, args, Some(piped), name, *span),
                    _ => Err(at(right.span(), "expected an operation after `|>`")),
                }
            }
            IrExpr::Call { callee, args, .. } => self.call(callee, args, None, name, span),
            IrExpr::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                let receiver = self.value(receiver, None)?;
                self.call(method, args, Some(receiver), name, span)
            }
            IrExpr::Sketch { args, stmts, .. } => self.sketch(args, stmts, name, span),
            IrExpr::FieldAccess { field, .. } => {
                Err(at(span, format!("unsupported DSL field access `{field}`")))
            }
        }
    }

    pub(super) fn nominal<'a>(&'a self, value: &'a IrValue) -> &'a IrValue {
        self.parameter(value)
            .map_or(value, |parameter| &parameter.value)
    }

    pub(super) fn parameter_default(
        &mut self,
        expr: &IrExpr,
        ty: IrType,
    ) -> Result<(IrValue, Option<String>)> {
        let reference = self.value(expr, None)?;
        let formula = self
            .parameter(&reference)
            .map(|parameter| parameter_symbol(&parameter.name.0));
        let value = self.nominal(&reference);
        let value = match (ty, value) {
            (IrType::Int, IrValue::Scalar(x)) => integer(*x, expr.span())?,
            (IrType::Scalar, IrValue::Int(x)) => IrValue::Scalar(*x as f64),
            _ if literal_type(value) == Some(ty) => value.clone(),
            _ => {
                return Err(at(
                    expr.span(),
                    format!("expected a {ty:?} parameter default"),
                ));
            }
        };
        Ok((value, formula))
    }

    pub(super) fn vector(&mut self, value: IrValue, ty: IrType, span: Span) -> Result<IrValue> {
        let IrValue::List(items) = value else {
            return Ok(value);
        };
        let [x, y, z]: &[IrValue; 3] = items
            .as_slice()
            .try_into()
            .map_err(|_| at(span, "expected three vector components"))?;
        let mut v = [0.0; 3];
        for (coord, item) in v.iter_mut().zip([x, y, z]) {
            let literal = self.nominal(item);
            let expected = if ty == IrType::Vec3Length {
                IrType::Length
            } else {
                IrType::Scalar
            };
            if literal_type(literal) != Some(expected)
                && !(expected == IrType::Scalar && matches!(literal, IrValue::Int(_)))
            {
                return Err(at(span, format!("expected {expected:?} vector components")));
            }
            *coord = as_number(literal, span)?;
        }
        let literal = IrValue::Vec3(v);
        if items.iter().all(|item| self.parameter(item).is_none()) {
            return Ok(literal);
        }
        let formula = format!(
            "[{}, {}, {}]",
            self.formula_atom(x, span)?,
            self.formula_atom(y, span)?,
            self.formula_atom(z, span)?
        );
        let id = self.generated_id();
        Ok(self.add_parameter(id, ty, literal, Some(formula)))
    }

    pub(super) fn angle_vector(&mut self, value: IrValue, span: Span) -> Result<IrValue> {
        let IrValue::List(items) = value else {
            return Ok(value);
        };
        let [x, y, z]: &[IrValue; 3] = items
            .as_slice()
            .try_into()
            .map_err(|_| at(span, "expected three rotation components"))?;
        let mut v = [0.0; 3];
        for (coord, item) in v.iter_mut().zip([x, y, z]) {
            let literal = self.nominal(item);
            if !matches!(
                literal_type(literal),
                Some(IrType::Angle | IrType::Scalar | IrType::Int)
            ) {
                return Err(at(span, "expected Angle vector components"));
            }
            *coord = as_number(literal, span)?;
        }
        let literal = IrValue::Vec3(v);
        if items.iter().all(|item| self.parameter(item).is_none()) {
            return Ok(literal);
        }
        let formula = format!(
            "[{}, {}, {}]",
            self.formula_atom(x, span)?,
            self.formula_atom(y, span)?,
            self.formula_atom(z, span)?
        );
        let id = self.generated_id();
        Ok(self.add_parameter(id, IrType::Vec3Dir, literal, Some(formula)))
    }

    pub(super) fn count(&mut self, value: IrValue, span: Span) -> Result<IrValue> {
        match self.nominal(&value) {
            IrValue::Int(n) if *n >= 0 => Ok(value),
            IrValue::Scalar(x) => {
                let literal = integer(*x, span)?;
                let formula = self
                    .parameter(&value)
                    .map(|parameter| parameter_symbol(&parameter.name.0));
                Ok(self.computed(None, literal, formula))
            }
            _ => Err(at(span, "expected a nonnegative integer array count")),
        }
    }

    fn computed(&mut self, name: Option<&str>, value: IrValue, formula: Option<String>) -> IrValue {
        let Some(formula) = formula else { return value };
        let ty = literal_type(&value).expect("computed expressions have a literal type");
        let name = match name {
            Some(name) if !self.graph.parameters.contains_key(name) => name.to_owned(),
            _ => self.generated_id(),
        };
        self.add_parameter(name, ty, value, Some(formula))
    }

    fn formula_atom(&self, value: &IrValue, span: Span) -> Result<String> {
        if let Some(parameter) = self.parameter(value) {
            return Ok(parameter_symbol(&parameter.name.0));
        }
        Ok(match value {
            IrValue::Quantity(q) => model::format_quantity(*q),
            IrValue::Scalar(x) => model::format_scalar(*x),
            IrValue::Int(x) => x.to_string(),
            IrValue::Bool(x) => x.to_string(),
            _ => return Err(at(span, "unsupported DSL formula operand")),
        })
    }
}

// Generated parameter IDs use punctuation unavailable to DSL identifiers.
pub(super) fn parameter_symbol(name: &str) -> String {
    if model::is_plain_ident(name) {
        name.to_owned()
    } else {
        format!("param({})", model::format_string(name))
    }
}

fn builtin(name: &str) -> Option<IrValue> {
    Some(match name {
        "X" | "X_AXIS" => IrValue::Vec3([1.0, 0.0, 0.0]),
        "Y" | "Y_AXIS" => IrValue::Vec3([0.0, 1.0, 0.0]),
        "Z" | "Z_AXIS" => IrValue::Vec3([0.0, 0.0, 1.0]),
        "XY_PLANE" | "XZ_PLANE" | "YZ_PLANE" => IrValue::String(name.to_owned()),
        _ => return None,
    })
}

pub(super) fn parameter_type(ty: &DimType, span: Span) -> Result<IrType> {
    Ok(match ty {
        DimType::F64 => IrType::Scalar,
        DimType::U32 => IrType::Int,
        DimType::Length => IrType::Length,
        DimType::Angle => IrType::Angle,
        DimType::Force => IrType::Force,
        DimType::Pressure => IrType::Pressure,
        DimType::Bool => IrType::Bool,
        DimType::Str => IrType::String,
        _ => {
            return Err(at(
                span,
                format!("unsupported DSL parameter or expression type `{ty}`"),
            ));
        }
    })
}

fn literal_type(value: &IrValue) -> Option<IrType> {
    Some(match value {
        IrValue::Scalar(_) => IrType::Scalar,
        IrValue::Int(_) => IrType::Int,
        IrValue::Bool(_) => IrType::Bool,
        IrValue::String(_) => IrType::String,
        IrValue::Vec3(_) => IrType::Vec3Dir,
        IrValue::Quantity(q) => q.ir_type(),
        _ => return None,
    })
}

fn as_number(value: &IrValue, span: Span) -> Result<f64> {
    match value {
        IrValue::Scalar(x) => Ok(*x),
        IrValue::Int(x) => Ok(*x as f64),
        IrValue::Quantity(q) => Ok(q.canonical_value()),
        _ => Err(at(span, "expected a numeric DSL expression")),
    }
}

fn integer(value: f64, span: Span) -> Result<IrValue> {
    if value.fract() != 0.0 || !(0.0..=f64::from(u32::MAX)).contains(&value) {
        return Err(at(span, "expected a whole number in the u32 range"));
    }
    Ok(IrValue::Int(value as i64))
}

fn number(value: f64, ty: &DimType, span: Span) -> Result<IrValue> {
    if !value.is_finite() {
        return Err(at(span, "DSL expression produced a non-finite number"));
    }
    match parameter_type(ty, span)? {
        IrType::Scalar => Ok(IrValue::Scalar(value)),
        IrType::Int => integer(value, span),
        _ => match model::quantity_from_suffix(value, ty.canonical_unit()) {
            SuffixParse::Quantity(q) => Ok(IrValue::Quantity(q)),
            _ => Err(at(span, format!("unsupported numeric DSL type `{ty}`"))),
        },
    }
}

fn binary(
    op: BinaryOp,
    left: &IrValue,
    right: &IrValue,
    ty: &DimType,
    span: Span,
) -> Result<IrValue> {
    use BinaryOp::*;
    if let (IrValue::Bool(a), IrValue::Bool(b)) = (left, right) {
        return Ok(IrValue::Bool(match op {
            And => *a && *b,
            Or => *a || *b,
            Eq => a == b,
            Ne => a != b,
            _ => return Err(at(span, "invalid DSL boolean operation")),
        }));
    }
    let a = as_number(left, span)?;
    let b = as_number(right, span)?;
    let result = match op {
        Add => a + b,
        Sub => a - b,
        Mul => a * b,
        Div | Rem if b == 0.0 => return Err(at(span, "division or remainder by zero")),
        Div => a / b,
        Rem => a % b,
        Eq => return Ok(IrValue::Bool((a - b).abs() < 1e-9)),
        Ne => return Ok(IrValue::Bool((a - b).abs() >= 1e-9)),
        Lt => return Ok(IrValue::Bool(a < b)),
        Le => return Ok(IrValue::Bool(a <= b)),
        Gt => return Ok(IrValue::Bool(a > b)),
        Ge => return Ok(IrValue::Bool(a >= b)),
        _ => return Err(at(span, "invalid DSL numeric operation")),
    };
    number(result, ty, span)
}

fn binary_symbol(op: BinaryOp) -> &'static str {
    use BinaryOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Rem => "%",
        Eq => "==",
        Ne => "!=",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
        And => "&&",
        Or => "||",
    }
}
