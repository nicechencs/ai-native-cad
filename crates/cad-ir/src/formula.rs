//! Deterministic strong-dimensional parameter formula AST, parser, DAG scheduler,
//! and evaluator.

use std::{
    collections::{BTreeMap, BTreeSet},
    f64::consts::PI,
};

use serde::{Deserialize, Serialize};

use crate::{
    AngleUnit, CanonicalGraph, ForceUnit, IrError, IrType, IrValue, LengthUnit, MassUnit, ParamId,
    PressureUnit, Quantity, UnitSystem, wire,
};

/// Maximum expression nesting / AST height accepted by the parser and evaluator
/// to guard against stack overflow on adversarial inputs.
pub const MAX_FORMULA_DEPTH: usize = 64;

/// Maximum number of AST nodes allowed in a single parameter formula.
pub const MAX_FORMULA_NODES: usize = 512;

const MAX_FORMULA_TOKENS: usize = MAX_FORMULA_NODES * 4;

/// `2^63` represented exactly in IEEE-754 `f64` (`i64::MAX` is `2^63 - 1`, which
/// rounds up to `2^63` in `f64`; values `>= 2^63` cannot fit in `i64`).
const I64_UPPER_EXCLUSIVE_F64: f64 = 9_223_372_036_854_775_808.0;

/// Physical dimension tracked by the formula evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    Scalar,
    Length,
    Angle,
    Force,
    Pressure,
    Mass,
    Density,
}

impl Dimension {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::Length => "length",
            Self::Angle => "angle",
            Self::Force => "force",
            Self::Pressure => "pressure",
            Self::Mass => "mass",
            Self::Density => "density",
        }
    }
}

/// Unary prefix operators supported by parameter formulas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnaryOp {
    Plus,
    Neg,
}

/// Binary arithmetic operators supported by parameter formulas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// Built-in deterministic functions supported by parameter formulas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinFunc {
    Min,
    Max,
    Abs,
    Sqrt,
    Sin,
    Cos,
    Tan,
}

impl BuiltinFunc {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "min" => Some(Self::Min),
            "max" => Some(Self::Max),
            "abs" => Some(Self::Abs),
            "sqrt" => Some(Self::Sqrt),
            "sin" => Some(Self::Sin),
            "cos" => Some(Self::Cos),
            "tan" => Some(Self::Tan),
            _ => None,
        }
    }

    pub const fn arity(self) -> usize {
        match self {
            Self::Min | Self::Max => 2,
            Self::Abs | Self::Sqrt | Self::Sin | Self::Cos | Self::Tan => 1,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Max => "max",
            Self::Abs => "abs",
            Self::Sqrt => "sqrt",
            Self::Sin => "sin",
            Self::Cos => "cos",
            Self::Tan => "tan",
        }
    }
}

/// Typed abstract syntax tree for a parameter formula.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "expr", rename_all = "snake_case")]
pub enum FormulaExpr {
    Scalar(f64),
    Quantity(Quantity),
    ConstPi,
    ParamRef(ParamId),
    Unary {
        op: UnaryOp,
        operand: Box<FormulaExpr>,
    },
    Binary {
        op: BinaryOp,
        left: Box<FormulaExpr>,
        right: Box<FormulaExpr>,
    },
    Call {
        func: BuiltinFunc,
        args: Vec<FormulaExpr>,
    },
    Vec3([Box<FormulaExpr>; 3]),
}

impl FormulaExpr {
    /// Collect all parameter identifiers referenced by this formula expression
    /// using an explicit stack (safe against arbitrary tree depth).
    pub fn referenced_params(&self) -> BTreeSet<ParamId> {
        let mut deps = BTreeSet::new();
        let mut stack = vec![self];
        while let Some(node) = stack.pop() {
            match node {
                Self::Scalar(_) | Self::Quantity(_) | Self::ConstPi => {}
                Self::ParamRef(id) => {
                    deps.insert(id.clone());
                }
                Self::Unary { operand, .. } => stack.push(operand),
                Self::Binary { left, right, .. } => {
                    stack.push(right);
                    stack.push(left);
                }
                Self::Call { args, .. } => {
                    for arg in args.iter().rev() {
                        stack.push(arg);
                    }
                }
                Self::Vec3(components) => {
                    for comp in components.iter().rev() {
                        stack.push(comp);
                    }
                }
            }
        }
        deps
    }
}

impl Drop for FormulaExpr {
    fn drop(&mut self) {
        let mut stack = Vec::new();
        let detach = |node: &mut FormulaExpr, stack: &mut Vec<FormulaExpr>| match node {
            FormulaExpr::Unary { operand, .. } => {
                stack.push(std::mem::replace(operand.as_mut(), FormulaExpr::ConstPi));
            }
            FormulaExpr::Binary { left, right, .. } => {
                stack.push(std::mem::replace(left.as_mut(), FormulaExpr::ConstPi));
                stack.push(std::mem::replace(right.as_mut(), FormulaExpr::ConstPi));
            }
            FormulaExpr::Call { args, .. } => {
                for arg in std::mem::take(args) {
                    stack.push(arg);
                }
            }
            FormulaExpr::Vec3(components) => {
                for comp in components.iter_mut() {
                    stack.push(std::mem::replace(comp.as_mut(), FormulaExpr::ConstPi));
                }
            }
            FormulaExpr::Scalar(_)
            | FormulaExpr::Quantity(_)
            | FormulaExpr::ConstPi
            | FormulaExpr::ParamRef(_) => {}
        };
        detach(self, &mut stack);
        while let Some(mut child) = stack.pop() {
            detach(&mut child, &mut stack);
        }
    }
}

/// Parse a formula string into a deterministic [`FormulaExpr`] AST.
pub fn parse_formula(param: &ParamId, input: &str) -> Result<FormulaExpr, IrError> {
    let tokens = tokenize(param, input)?;
    if tokens.is_empty() {
        return Err(IrError::FormulaParseError {
            param: param.clone(),
            message: "formula cannot be empty".to_owned(),
        });
    }
    let mut parser = Parser {
        param,
        tokens: &tokens,
        pos: 0,
        node_count: 0,
    };
    let (expr, _height) = parser.parse_expr(0)?;
    if parser.pos < parser.tokens.len() {
        return Err(IrError::FormulaParseError {
            param: param.clone(),
            message: format!("unexpected trailing token `{:?}`", parser.tokens[parser.pos]),
        });
    }
    Ok(expr)
}

/// Collect parameter dependencies directly from a raw formula string.
pub fn collect_formula_dependencies(
    param: &ParamId,
    input: &str,
) -> Result<BTreeSet<ParamId>, IrError> {
    Ok(parse_formula(param, input)?.referenced_params())
}

/// Validate and evaluate every parameter with a `formula` in topological order
/// without mutating `graph`.
///
/// Returns `(evaluation_order, resolved_values)` where `evaluation_order` contains
/// every parameter that carries a formula in deterministic dependency order, and
/// `resolved_values` maps every evaluated parameter to its computed [`IrValue`].
pub fn resolve_formula_values(
    graph: &CanonicalGraph,
) -> Result<(Vec<ParamId>, BTreeMap<ParamId, IrValue>), IrError> {
    let mut parsed = BTreeMap::<&str, FormulaExpr>::new();
    let mut incoming = BTreeMap::<&str, usize>::new();
    let mut dependents = BTreeMap::<&str, Vec<&str>>::new();

    for (name, def) in &graph.parameters {
        let Some(raw_formula) = def.formula.as_deref() else {
            continue;
        };
        let expr = parse_formula(&def.name, raw_formula)?;
        let deps = expr.referenced_params();
        for dep in &deps {
            if !graph.parameters.contains_key(&dep.0) {
                return Err(IrError::DanglingParameter(dep.clone()));
            }
        }
        parsed.insert(name.as_str(), expr);
        incoming.insert(name.as_str(), 0);
    }

    if parsed.is_empty() {
        return Ok((Vec::new(), BTreeMap::new()));
    }

    for (&param_name, expr) in &parsed {
        let deps = expr.referenced_params();
        for dep in deps {
            if dep.0 == param_name {
                return Err(IrError::ParameterCycle {
                    params: vec![ParamId::from(param_name)],
                });
            }
            if let Some((&dep_key, _)) = parsed.get_key_value(dep.0.as_str()) {
                *incoming
                    .get_mut(param_name)
                    .expect("formula parameter registered in incoming map") += 1;
                dependents.entry(dep_key).or_default().push(param_name);
            }
        }
    }

    let topo_order = topological_sort_params(incoming, dependents)?;

    let mut dim_env = BTreeMap::<&str, EvalValue>::new();
    for (name, def) in &graph.parameters {
        if !parsed.contains_key(name.as_str())
            && let Some(val) = ir_value_to_eval_value(def.ir_type, &def.value)?
        {
            dim_env.insert(name.as_str(), val);
        }
    }

    let mut resolved = BTreeMap::<ParamId, IrValue>::new();
    for param_id in &topo_order {
        let def = &graph.parameters[&param_id.0];
        let expr = &parsed[param_id.0.as_str()];
        let computed = eval_expr(&def.name, expr, &dim_env, &graph.parameters, 0)?;
        let ir_value = eval_value_to_ir_value(&def.name, def.ir_type, computed, graph.units)?;
        dim_env.insert(def.name.0.as_str(), computed);
        resolved.insert(param_id.clone(), ir_value);
    }

    Ok((topo_order, resolved))
}

impl CanonicalGraph {
    /// Validate and evaluate all parameter formulas in topological order, writing
    /// each recomputed value back to `ParameterDef.value` using the parameter's
    /// declared `ir_type` and the graph's `UnitSystem`.
    ///
    /// Returns the evaluated formula `ParamId`s in topological execution order.
    pub fn evaluate_formulas(&mut self) -> Result<Vec<ParamId>, IrError> {
        let (order, resolved) = resolve_formula_values(self)?;
        for (param_id, value) in resolved {
            if let Some(def) = self.parameters.get_mut(&param_id.0) {
                def.value = value;
            }
        }
        Ok(order)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct DimValue {
    dim: Dimension,
    canonical: f64,
}

impl DimValue {
    fn checked(dim: Dimension, canonical: f64) -> Result<Self, IrError> {
        if !canonical.is_finite() {
            return Err(IrError::NonFiniteNumber);
        }
        Ok(Self {
            dim,
            canonical: wire::canonical_zero(canonical),
        })
    }

    fn from_quantity(q: Quantity) -> Result<Self, IrError> {
        let dim = match q {
            Quantity::Length { .. } => Dimension::Length,
            Quantity::Angle { .. } => Dimension::Angle,
            Quantity::Force { .. } => Dimension::Force,
            Quantity::Pressure { .. } => Dimension::Pressure,
            Quantity::Mass { .. } => Dimension::Mass,
            Quantity::Density { .. } => Dimension::Density,
        };
        Self::checked(dim, q.canonical_value())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum EvalValue {
    Dim(DimValue),
    Vec3Length([f64; 3]),
    Vec3Dir([f64; 3]),
}

impl EvalValue {
    fn dim_name(self) -> &'static str {
        match self {
            Self::Dim(d) => d.dim.as_str(),
            Self::Vec3Length(_) => "vec3_length",
            Self::Vec3Dir(_) => "vec3_dir",
        }
    }

    fn expect_dim(self, param: &ParamId, context: &str) -> Result<DimValue, IrError> {
        match self {
            Self::Dim(d) => Ok(d),
            other => Err(IrError::FormulaDimensionMismatch {
                param: param.clone(),
                message: format!("{context} does not accept {}", other.dim_name()),
            }),
        }
    }
}

fn check_vec3_finite(v: [f64; 3]) -> Result<[f64; 3], IrError> {
    if v.iter().all(|x| x.is_finite()) {
        Ok([
            wire::canonical_zero(v[0]),
            wire::canonical_zero(v[1]),
            wire::canonical_zero(v[2]),
        ])
    } else {
        Err(IrError::NonFiniteNumber)
    }
}

fn ir_value_to_eval_value(ir_type: IrType, value: &IrValue) -> Result<Option<EvalValue>, IrError> {
    match (ir_type, value) {
        (IrType::Scalar, IrValue::Scalar(v)) => {
            Ok(Some(EvalValue::Dim(DimValue::checked(Dimension::Scalar, *v)?)))
        }
        (IrType::Int, IrValue::Int(v)) => Ok(Some(EvalValue::Dim(DimValue::checked(
            Dimension::Scalar,
            *v as f64,
        )?))),
        (
            IrType::Length
            | IrType::Angle
            | IrType::Force
            | IrType::Pressure
            | IrType::Mass
            | IrType::Density,
            IrValue::Quantity(q),
        ) => Ok(Some(EvalValue::Dim(DimValue::from_quantity(*q)?))),
        (IrType::Vec3Length, IrValue::Vec3(v)) => Ok(Some(EvalValue::Vec3Length(check_vec3_finite(*v)?))),
        (IrType::Vec3Dir, IrValue::Vec3(v)) => Ok(Some(EvalValue::Vec3Dir(check_vec3_finite(*v)?))),
        _ => Ok(None),
    }
}

fn eval_value_to_ir_value(
    param: &ParamId,
    target_type: IrType,
    computed: EvalValue,
    units: UnitSystem,
) -> Result<IrValue, IrError> {
    match (target_type, computed) {
        (IrType::Vec3Length, EvalValue::Vec3Length(v)) => Ok(IrValue::Vec3(check_vec3_finite(v)?)),
        (IrType::Vec3Dir, EvalValue::Vec3Dir(v)) => Ok(IrValue::Vec3(check_vec3_finite(v)?)),
        (target, EvalValue::Dim(dim_val)) => dim_value_to_ir_value(param, target, dim_val, units),
        (target, other) => Err(IrError::FormulaDimensionMismatch {
            param: param.clone(),
            message: format!(
                "parameter expects {:?}, but formula evaluates to {}",
                target,
                other.dim_name()
            ),
        }),
    }
}

fn dim_value_to_ir_value(
    param: &ParamId,
    target_type: IrType,
    computed: DimValue,
    units: UnitSystem,
) -> Result<IrValue, IrError> {
    match (target_type, computed.dim) {
        (IrType::Length, Dimension::Length) => Ok(IrValue::Quantity(Quantity::length(
            wire::canonical_zero(computed.canonical / units.length.to_mm()),
            units.length,
        )?)),
        (IrType::Angle, Dimension::Angle) => Ok(IrValue::Quantity(Quantity::angle(
            wire::canonical_zero(computed.canonical / units.angle.to_deg()),
            units.angle,
        )?)),
        (IrType::Force, Dimension::Force) => Ok(IrValue::Quantity(Quantity::force(
            wire::canonical_zero(computed.canonical / units.force.to_newtons()),
            units.force,
        )?)),
        (IrType::Pressure, Dimension::Pressure) => Ok(IrValue::Quantity(Quantity::pressure(
            wire::canonical_zero(computed.canonical / units.pressure.to_mpa()),
            units.pressure,
        )?)),
        (IrType::Mass, Dimension::Mass) => Ok(IrValue::Quantity(Quantity::mass(
            wire::canonical_zero(computed.canonical / units.mass.to_kg()),
            units.mass,
        )?)),
        (IrType::Density, Dimension::Density) => {
            Ok(IrValue::Quantity(Quantity::density(computed.canonical)?))
        }
        (IrType::Scalar, Dimension::Scalar) => Ok(IrValue::Scalar(computed.canonical)),
        (IrType::Int, Dimension::Scalar) => {
            let rounded = computed.canonical.round();
            if (computed.canonical - rounded).abs() > 1e-9
                || rounded < (i64::MIN as f64)
                || rounded >= I64_UPPER_EXCLUSIVE_F64
            {
                return Err(IrError::FormulaEvalError {
                    param: param.clone(),
                    message: format!(
                        "int parameter requires an integral value within i64 range, got {}",
                        computed.canonical
                    ),
                });
            }
            Ok(IrValue::Int(rounded as i64))
        }
        _ => Err(IrError::FormulaDimensionMismatch {
            param: param.clone(),
            message: format!(
                "parameter expects {:?}, but formula evaluates to {}",
                target_type,
                computed.dim.as_str()
            ),
        }),
    }
}

fn eval_expr(
    param: &ParamId,
    expr: &FormulaExpr,
    env: &BTreeMap<&str, EvalValue>,
    all_params: &BTreeMap<String, crate::ParameterDef>,
    depth: usize,
) -> Result<EvalValue, IrError> {
    if depth > MAX_FORMULA_DEPTH {
        return Err(IrError::FormulaParseError {
            param: param.clone(),
            message: format!("formula nesting exceeds limit ({MAX_FORMULA_DEPTH})"),
        });
    }
    match expr {
        FormulaExpr::Scalar(v) => Ok(EvalValue::Dim(DimValue::checked(Dimension::Scalar, *v)?)),
        FormulaExpr::Quantity(q) => Ok(EvalValue::Dim(DimValue::from_quantity(*q)?)),
        FormulaExpr::ConstPi => Ok(EvalValue::Dim(DimValue::checked(Dimension::Scalar, PI)?)),
        FormulaExpr::ParamRef(dep_id) => {
            if let Some(&val) = env.get(dep_id.0.as_str()) {
                Ok(val)
            } else if let Some(def) = all_params.get(&dep_id.0) {
                Err(IrError::FormulaDimensionMismatch {
                    param: param.clone(),
                    message: format!(
                        "referenced parameter `{}` has non-numeric type {:?}",
                        dep_id, def.ir_type
                    ),
                })
            } else {
                Err(IrError::DanglingParameter(dep_id.clone()))
            }
        }
        FormulaExpr::Unary { op, operand } => {
            let val = eval_expr(param, operand, env, all_params, depth + 1)?;
            match (op, val) {
                (UnaryOp::Plus, v) => Ok(v),
                (UnaryOp::Neg, EvalValue::Dim(d)) => {
                    Ok(EvalValue::Dim(DimValue::checked(d.dim, -d.canonical)?))
                }
                (UnaryOp::Neg, EvalValue::Vec3Length(v)) => {
                    Ok(EvalValue::Vec3Length(check_vec3_finite([-v[0], -v[1], -v[2]])?))
                }
                (UnaryOp::Neg, EvalValue::Vec3Dir(v)) => {
                    Ok(EvalValue::Vec3Dir(check_vec3_finite([-v[0], -v[1], -v[2]])?))
                }
            }
        }
        FormulaExpr::Binary { op, left, right } => {
            let lhs = eval_expr(param, left, env, all_params, depth + 1)?
                .expect_dim(param, "binary arithmetic")?;
            let rhs = eval_expr(param, right, env, all_params, depth + 1)?
                .expect_dim(param, "binary arithmetic")?;
            Ok(EvalValue::Dim(eval_binary(param, *op, lhs, rhs)?))
        }
        FormulaExpr::Call { func, args } => {
            let mut vals = Vec::with_capacity(args.len());
            for arg in args {
                vals.push(
                    eval_expr(param, arg, env, all_params, depth + 1)?
                        .expect_dim(param, func.as_str())?,
                );
            }
            Ok(EvalValue::Dim(eval_builtin(param, *func, &vals)?))
        }
        FormulaExpr::Vec3(components) => {
            let x = eval_expr(param, &components[0], env, all_params, depth + 1)?
                .expect_dim(param, "vector component")?;
            let y = eval_expr(param, &components[1], env, all_params, depth + 1)?
                .expect_dim(param, "vector component")?;
            let z = eval_expr(param, &components[2], env, all_params, depth + 1)?
                .expect_dim(param, "vector component")?;
            match (x.dim, y.dim, z.dim) {
                (Dimension::Length, Dimension::Length, Dimension::Length) => {
                    Ok(EvalValue::Vec3Length(check_vec3_finite([
                        x.canonical,
                        y.canonical,
                        z.canonical,
                    ])?))
                }
                (Dimension::Scalar, Dimension::Scalar, Dimension::Scalar) => {
                    Ok(EvalValue::Vec3Dir(check_vec3_finite([
                        x.canonical,
                        y.canonical,
                        z.canonical,
                    ])?))
                }
                _ => Err(IrError::FormulaDimensionMismatch {
                    param: param.clone(),
                    message: format!(
                        "vector components must all be length or all scalar, got [{}, {}, {}]",
                        x.dim.as_str(),
                        y.dim.as_str(),
                        z.dim.as_str()
                    ),
                }),
            }
        }
    }
}

fn eval_binary(
    param: &ParamId,
    op: BinaryOp,
    lhs: DimValue,
    rhs: DimValue,
) -> Result<DimValue, IrError> {
    match op {
        BinaryOp::Add => {
            if lhs.dim != rhs.dim {
                return Err(IrError::FormulaDimensionMismatch {
                    param: param.clone(),
                    message: format!(
                        "cannot add {} and {}",
                        lhs.dim.as_str(),
                        rhs.dim.as_str()
                    ),
                });
            }
            DimValue::checked(lhs.dim, lhs.canonical + rhs.canonical)
        }
        BinaryOp::Sub => {
            if lhs.dim != rhs.dim {
                return Err(IrError::FormulaDimensionMismatch {
                    param: param.clone(),
                    message: format!(
                        "cannot subtract {} from {}",
                        rhs.dim.as_str(),
                        lhs.dim.as_str()
                    ),
                });
            }
            DimValue::checked(lhs.dim, lhs.canonical - rhs.canonical)
        }
        BinaryOp::Mul => match (lhs.dim, rhs.dim) {
            (Dimension::Scalar, d) | (d, Dimension::Scalar) => {
                DimValue::checked(d, lhs.canonical * rhs.canonical)
            }
            _ => Err(IrError::FormulaDimensionMismatch {
                param: param.clone(),
                message: format!(
                    "cannot multiply {} by {} (product dimension is unsupported)",
                    lhs.dim.as_str(),
                    rhs.dim.as_str()
                ),
            }),
        },
        BinaryOp::Div => {
            if rhs.canonical == 0.0 {
                return Err(IrError::DivisionByZero);
            }
            match (lhs.dim, rhs.dim) {
                (d, Dimension::Scalar) => DimValue::checked(d, lhs.canonical / rhs.canonical),
                (d1, d2) if d1 == d2 => {
                    DimValue::checked(Dimension::Scalar, lhs.canonical / rhs.canonical)
                }
                _ => Err(IrError::FormulaDimensionMismatch {
                    param: param.clone(),
                    message: format!(
                        "cannot divide {} by {}",
                        lhs.dim.as_str(),
                        rhs.dim.as_str()
                    ),
                }),
            }
        }
    }
}

fn eval_builtin(
    param: &ParamId,
    func: BuiltinFunc,
    args: &[DimValue],
) -> Result<DimValue, IrError> {
    if args.len() != func.arity() {
        return Err(IrError::FormulaParseError {
            param: param.clone(),
            message: format!(
                "function `{}` expects {} argument(s), got {}",
                func.as_str(),
                func.arity(),
                args.len()
            ),
        });
    }
    match func {
        BuiltinFunc::Min => {
            let (a, b) = (args[0], args[1]);
            if a.dim != b.dim {
                return Err(IrError::FormulaDimensionMismatch {
                    param: param.clone(),
                    message: format!(
                        "min() requires matching dimensions, got {} and {}",
                        a.dim.as_str(),
                        b.dim.as_str()
                    ),
                });
            }
            DimValue::checked(a.dim, a.canonical.min(b.canonical))
        }
        BuiltinFunc::Max => {
            let (a, b) = (args[0], args[1]);
            if a.dim != b.dim {
                return Err(IrError::FormulaDimensionMismatch {
                    param: param.clone(),
                    message: format!(
                        "max() requires matching dimensions, got {} and {}",
                        a.dim.as_str(),
                        b.dim.as_str()
                    ),
                });
            }
            DimValue::checked(a.dim, a.canonical.max(b.canonical))
        }
        BuiltinFunc::Abs => DimValue::checked(args[0].dim, args[0].canonical.abs()),
        BuiltinFunc::Sqrt => {
            let x = args[0];
            if x.dim != Dimension::Scalar {
                return Err(IrError::FormulaDimensionMismatch {
                    param: param.clone(),
                    message: format!(
                        "sqrt() requires a dimensionless scalar, got {}",
                        x.dim.as_str()
                    ),
                });
            }
            if x.canonical < 0.0 {
                return Err(IrError::FormulaEvalError {
                    param: param.clone(),
                    message: format!("sqrt() domain error: negative input {}", x.canonical),
                });
            }
            DimValue::checked(Dimension::Scalar, x.canonical.sqrt())
        }
        BuiltinFunc::Sin | BuiltinFunc::Cos | BuiltinFunc::Tan => {
            let angle = args[0];
            if angle.dim != Dimension::Angle {
                return Err(IrError::FormulaDimensionMismatch {
                    param: param.clone(),
                    message: format!(
                        "{}() requires an angle argument, got {}",
                        func.as_str(),
                        angle.dim.as_str()
                    ),
                });
            }
            let rad = angle.canonical.to_radians();
            let result = match func {
                BuiltinFunc::Sin => rad.sin(),
                BuiltinFunc::Cos => rad.cos(),
                BuiltinFunc::Tan => {
                    if rad.cos().abs() < 1e-15 {
                        return Err(IrError::DivisionByZero);
                    }
                    rad.tan()
                }
                _ => unreachable!(),
            };
            DimValue::checked(Dimension::Scalar, result)
        }
    }
}

fn topological_sort_params<'a>(
    mut incoming: BTreeMap<&'a str, usize>,
    dependents: BTreeMap<&'a str, Vec<&'a str>>,
) -> Result<Vec<ParamId>, IrError> {
    let mut ready: BTreeSet<&'a str> = incoming
        .iter()
        .filter_map(|(&id, &count)| (count == 0).then_some(id))
        .collect();
    let mut order = Vec::with_capacity(incoming.len());

    while let Some(id) = ready.pop_first() {
        order.push(ParamId::from(id));
        if let Some(children) = dependents.get(id) {
            for &child in children {
                let count = incoming
                    .get_mut(child)
                    .expect("dependent parameter has in-degree entry");
                *count -= 1;
                if *count == 0 {
                    ready.insert(child);
                }
            }
        }
    }

    if order.len() == incoming.len() {
        Ok(order)
    } else {
        let cycle_params: Vec<ParamId> = incoming
            .into_iter()
            .filter_map(|(id, count)| (count > 0).then_some(ParamId::from(id)))
            .collect();
        Err(IrError::ParameterCycle {
            params: cycle_params,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    StringLit(String),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
}

fn tokenize(param: &ParamId, input: &str) -> Result<Vec<Token>, IrError> {
    let bytes = input.as_bytes();
    let mut idx = 0;
    let mut tokens = Vec::new();

    while idx < bytes.len() {
        let b = bytes[idx];
        if b.is_ascii_whitespace() {
            idx += 1;
            continue;
        }
        if tokens.len() >= MAX_FORMULA_TOKENS {
            return Err(IrError::FormulaParseError {
                param: param.clone(),
                message: format!("formula exceeds maximum token limit ({MAX_FORMULA_TOKENS})"),
            });
        }
        match b {
            b'+' => {
                tokens.push(Token::Plus);
                idx += 1;
            }
            b'-' => {
                tokens.push(Token::Minus);
                idx += 1;
            }
            b'*' => {
                tokens.push(Token::Star);
                idx += 1;
            }
            b'/' => {
                tokens.push(Token::Slash);
                idx += 1;
            }
            b'(' => {
                tokens.push(Token::LParen);
                idx += 1;
            }
            b')' => {
                tokens.push(Token::RParen);
                idx += 1;
            }
            b'[' => {
                tokens.push(Token::LBracket);
                idx += 1;
            }
            b']' => {
                tokens.push(Token::RBracket);
                idx += 1;
            }
            b',' => {
                tokens.push(Token::Comma);
                idx += 1;
            }
            b'"' => {
                idx += 1;
                let mut value = String::new();
                let mut closed = false;
                while idx < bytes.len() {
                    let c = bytes[idx];
                    if c == b'"' {
                        idx += 1;
                        closed = true;
                        break;
                    }
                    if c == b'\\' {
                        idx += 1;
                        if idx >= bytes.len() {
                            return Err(IrError::FormulaParseError {
                                param: param.clone(),
                                message: "unterminated escape sequence in string literal".to_owned(),
                            });
                        }
                        let escaped = match bytes[idx] {
                            b'"' => '"',
                            b'\\' => '\\',
                            b'n' => '\n',
                            b't' => '\t',
                            b'r' => '\r',
                            other => other as char,
                        };
                        value.push(escaped);
                        idx += 1;
                    } else {
                        let ch = input[idx..].chars().next().unwrap_or('?');
                        value.push(ch);
                        idx += ch.len_utf8();
                    }
                }
                if !closed {
                    return Err(IrError::FormulaParseError {
                        param: param.clone(),
                        message: "unterminated string literal".to_owned(),
                    });
                }
                tokens.push(Token::StringLit(value));
            }
            b'0'..=b'9'
            | b'.'
                if b != b'.'
                    || (idx + 1 < bytes.len() && bytes[idx + 1].is_ascii_digit()) =>
            {
                let start = idx;
                while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                    idx += 1;
                }
                if idx < bytes.len() && bytes[idx] == b'.' {
                    idx += 1;
                    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                        idx += 1;
                    }
                }
                if idx < bytes.len() && matches!(bytes[idx], b'e' | b'E') {
                    let exp_start = idx;
                    let mut look = idx + 1;
                    if look < bytes.len() && matches!(bytes[look], b'+' | b'-') {
                        look += 1;
                    }
                    if look < bytes.len() && bytes[look].is_ascii_digit() {
                        idx = look + 1;
                        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
                            idx += 1;
                        }
                    } else {
                        idx = exp_start;
                    }
                }
                let slice = &input[start..idx];
                let num: f64 = slice.parse().map_err(|_| IrError::FormulaParseError {
                    param: param.clone(),
                    message: format!("invalid number literal `{slice}`"),
                })?;
                if !num.is_finite() {
                    return Err(IrError::NonFiniteNumber);
                }
                tokens.push(Token::Number(num));
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let start = idx;
                idx += 1;
                while idx < bytes.len()
                    && (bytes[idx].is_ascii_alphanumeric() || bytes[idx] == b'_')
                {
                    idx += 1;
                }
                tokens.push(Token::Ident(input[start..idx].to_owned()));
            }
            _ => {
                let ch = input[idx..].chars().next().unwrap_or('?');
                return Err(IrError::FormulaParseError {
                    param: param.clone(),
                    message: format!("unexpected character `{ch}` at byte {idx}"),
                });
            }
        }
    }

    Ok(tokens)
}

enum ParsedUnit {
    Length(LengthUnit),
    Angle(AngleUnit),
    Force(ForceUnit),
    Pressure(PressureUnit),
    Mass(MassUnit),
}

fn parse_unit(ident: &str) -> Option<ParsedUnit> {
    match ident {
        "mm" => Some(ParsedUnit::Length(LengthUnit::Mm)),
        "cm" => Some(ParsedUnit::Length(LengthUnit::Cm)),
        "m" => Some(ParsedUnit::Length(LengthUnit::M)),
        "in" => Some(ParsedUnit::Length(LengthUnit::In)),
        "um" => Some(ParsedUnit::Length(LengthUnit::Um)),
        "ft" => Some(ParsedUnit::Length(LengthUnit::Ft)),
        "deg" => Some(ParsedUnit::Angle(AngleUnit::Deg)),
        "rad" => Some(ParsedUnit::Angle(AngleUnit::Rad)),
        "N" => Some(ParsedUnit::Force(ForceUnit::N)),
        "kN" => Some(ParsedUnit::Force(ForceUnit::Kn)),
        "Pa" => Some(ParsedUnit::Pressure(PressureUnit::Pa)),
        "MPa" => Some(ParsedUnit::Pressure(PressureUnit::Mpa)),
        "g" => Some(ParsedUnit::Mass(MassUnit::G)),
        "kg" => Some(ParsedUnit::Mass(MassUnit::Kg)),
        _ => None,
    }
}

fn build_quantity(value: f64, unit: ParsedUnit) -> Result<Quantity, IrError> {
    match unit {
        ParsedUnit::Length(u) => Quantity::length(value, u),
        ParsedUnit::Angle(u) => Quantity::angle(value, u),
        ParsedUnit::Force(u) => Quantity::force(value, u),
        ParsedUnit::Pressure(u) => Quantity::pressure(value, u),
        ParsedUnit::Mass(u) => Quantity::mass(value, u),
    }
}

struct Parser<'a> {
    param: &'a ParamId,
    tokens: &'a [Token],
    pos: usize,
    node_count: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<&'a Token> {
        let tok = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(tok)
    }

    fn check_depth(&self, depth: usize) -> Result<(), IrError> {
        if depth > MAX_FORMULA_DEPTH {
            return Err(IrError::FormulaParseError {
                param: self.param.clone(),
                message: format!("formula nesting exceeds limit ({MAX_FORMULA_DEPTH})"),
            });
        }
        Ok(())
    }

    fn alloc_node(&mut self) -> Result<(), IrError> {
        self.node_count += 1;
        if self.node_count > MAX_FORMULA_NODES {
            return Err(IrError::FormulaParseError {
                param: self.param.clone(),
                message: format!("formula exceeds maximum AST node limit ({MAX_FORMULA_NODES})"),
            });
        }
        Ok(())
    }

    fn parse_expr(&mut self, depth: usize) -> Result<(FormulaExpr, usize), IrError> {
        self.check_depth(depth)?;
        self.parse_additive(depth)
    }

    fn parse_additive(&mut self, depth: usize) -> Result<(FormulaExpr, usize), IrError> {
        let (mut left, mut left_h) = self.parse_multiplicative(depth)?;
        while let Some(op) = match self.peek() {
            Some(Token::Plus) => Some(BinaryOp::Add),
            Some(Token::Minus) => Some(BinaryOp::Sub),
            _ => None,
        } {
            self.next();
            let (right, right_h) = self.parse_multiplicative(depth)?;
            let height = left_h.max(right_h) + 1;
            self.check_depth(height)?;
            self.alloc_node()?;
            left = FormulaExpr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
            left_h = height;
        }
        Ok((left, left_h))
    }

    fn parse_multiplicative(&mut self, depth: usize) -> Result<(FormulaExpr, usize), IrError> {
        let (mut left, mut left_h) = self.parse_unary(depth)?;
        while let Some(op) = match self.peek() {
            Some(Token::Star) => Some(BinaryOp::Mul),
            Some(Token::Slash) => Some(BinaryOp::Div),
            _ => None,
        } {
            self.next();
            let (right, right_h) = self.parse_unary(depth)?;
            let height = left_h.max(right_h) + 1;
            self.check_depth(height)?;
            self.alloc_node()?;
            left = FormulaExpr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
            left_h = height;
        }
        Ok((left, left_h))
    }

    fn parse_unary(&mut self, depth: usize) -> Result<(FormulaExpr, usize), IrError> {
        self.check_depth(depth)?;
        match self.peek() {
            Some(Token::Plus) => {
                self.next();
                let (operand, operand_h) = self.parse_unary(depth + 1)?;
                let height = operand_h + 1;
                self.check_depth(height)?;
                self.alloc_node()?;
                Ok((
                    FormulaExpr::Unary {
                        op: UnaryOp::Plus,
                        operand: Box::new(operand),
                    },
                    height,
                ))
            }
            Some(Token::Minus) => {
                self.next();
                let (operand, operand_h) = self.parse_unary(depth + 1)?;
                let height = operand_h + 1;
                self.check_depth(height)?;
                self.alloc_node()?;
                Ok((
                    FormulaExpr::Unary {
                        op: UnaryOp::Neg,
                        operand: Box::new(operand),
                    },
                    height,
                ))
            }
            _ => self.parse_primary(depth),
        }
    }

    fn parse_primary(&mut self, depth: usize) -> Result<(FormulaExpr, usize), IrError> {
        self.check_depth(depth)?;
        let Some(token) = self.next() else {
            return Err(IrError::FormulaParseError {
                param: self.param.clone(),
                message: "unexpected end of formula".to_owned(),
            });
        };
        match token {
            Token::Number(value) => {
                self.alloc_node()?;
                if let Some(Token::Ident(unit_name)) = self.peek()
                    && let Some(unit) = parse_unit(unit_name)
                {
                    self.next();
                    return Ok((FormulaExpr::Quantity(build_quantity(*value, unit)?), 0));
                }
                Ok((FormulaExpr::Scalar(*value), 0))
            }
            Token::Ident(name) => {
                if matches!(self.peek(), Some(Token::LParen)) {
                    self.next(); // consume '('
                    if name == "param" {
                        let Some(Token::StringLit(param_name)) = self.next() else {
                            return Err(IrError::FormulaParseError {
                                param: self.param.clone(),
                                message: "param() expects a quoted parameter name".to_owned(),
                            });
                        };
                        if !matches!(self.next(), Some(Token::RParen)) {
                            return Err(IrError::FormulaParseError {
                                param: self.param.clone(),
                                message: "expected `)` after param(...)".to_owned(),
                            });
                        }
                        self.alloc_node()?;
                        return Ok((FormulaExpr::ParamRef(ParamId(param_name.clone())), 0));
                    }
                    let Some(func) = BuiltinFunc::parse(name) else {
                        return Err(IrError::FormulaParseError {
                            param: self.param.clone(),
                            message: format!("unknown function `{name}`"),
                        });
                    };
                    let mut args = Vec::new();
                    let mut max_arg_h = 0usize;
                    if !matches!(self.peek(), Some(Token::RParen)) {
                        loop {
                            let (arg, arg_h) = self.parse_expr(depth + 1)?;
                            max_arg_h = max_arg_h.max(arg_h);
                            args.push(arg);
                            match self.peek() {
                                Some(Token::Comma) => {
                                    self.next();
                                }
                                Some(Token::RParen) => break,
                                _ => {
                                    return Err(IrError::FormulaParseError {
                                        param: self.param.clone(),
                                        message: format!(
                                            "expected `,` or `)` in call to `{name}`"
                                        ),
                                    });
                                }
                            }
                        }
                    }
                    self.next(); // consume ')'
                    if args.len() != func.arity() {
                        return Err(IrError::FormulaParseError {
                            param: self.param.clone(),
                            message: format!(
                                "function `{name}` expects {} argument(s), got {}",
                                func.arity(),
                                args.len()
                            ),
                        });
                    }
                    let height = max_arg_h + 1;
                    self.check_depth(height)?;
                    self.alloc_node()?;
                    Ok((FormulaExpr::Call { func, args }, height))
                } else if name == "PI" {
                    self.alloc_node()?;
                    if let Some(Token::Ident(unit_name)) = self.peek()
                        && let Some(unit) = parse_unit(unit_name)
                    {
                        self.next();
                        return Ok((FormulaExpr::Quantity(build_quantity(PI, unit)?), 0));
                    }
                    Ok((FormulaExpr::ConstPi, 0))
                } else {
                    self.alloc_node()?;
                    Ok((FormulaExpr::ParamRef(ParamId(name.clone())), 0))
                }
            }
            Token::LParen => {
                let (inner, inner_h) = self.parse_expr(depth + 1)?;
                if !matches!(self.next(), Some(Token::RParen)) {
                    return Err(IrError::FormulaParseError {
                        param: self.param.clone(),
                        message: "missing closing `)`".to_owned(),
                    });
                }
                Ok((inner, inner_h))
            }
            Token::LBracket => {
                let (x, x_h) = self.parse_expr(depth + 1)?;
                if !matches!(self.next(), Some(Token::Comma)) {
                    return Err(IrError::FormulaParseError {
                        param: self.param.clone(),
                        message: "expected `,` after first vector component".to_owned(),
                    });
                }
                let (y, y_h) = self.parse_expr(depth + 1)?;
                if !matches!(self.next(), Some(Token::Comma)) {
                    return Err(IrError::FormulaParseError {
                        param: self.param.clone(),
                        message: "expected `,` after second vector component".to_owned(),
                    });
                }
                let (z, z_h) = self.parse_expr(depth + 1)?;
                if !matches!(self.next(), Some(Token::RBracket)) {
                    return Err(IrError::FormulaParseError {
                        param: self.param.clone(),
                        message: "missing closing `]` in vector formula".to_owned(),
                    });
                }
                let height = x_h.max(y_h).max(z_h) + 1;
                self.check_depth(height)?;
                self.alloc_node()?;
                Ok((FormulaExpr::Vec3([Box::new(x), Box::new(y), Box::new(z)]), height))
            }
            other => Err(IrError::FormulaParseError {
                param: self.param.clone(),
                message: format!("unexpected token `{other:?}`"),
            }),
        }
    }
}
