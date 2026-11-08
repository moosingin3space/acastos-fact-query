//! Expressions used in `if` conditions, `let` bindings, and head construction.
//!
//! Expressions have a small, total, side-effect-free semantics. This module
//! provides the IR ([`Expr`]), a type inferencer, and a tree-walking
//! interpreter — the only expression backend (ADR 0010).

use std::collections::HashMap;

use crate::stack;
use crate::value::{Symbol, Type, Value};

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnOp {
    /// Arithmetic negation (`-x`).
    Neg,
    /// Logical negation (`!x`).
    Not,
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    /// `a + b`.
    Add,
    /// `a - b`.
    Sub,
    /// `a * b`.
    Mul,
    /// `a / b` (truncating, like Rust integer division).
    Div,
    /// `a % b` (remainder, like Rust).
    Rem,
    /// `a == b`.
    Eq,
    /// `a != b`.
    Ne,
    /// `a < b`.
    Lt,
    /// `a <= b`.
    Le,
    /// `a > b`.
    Gt,
    /// `a >= b`.
    Ge,
    /// `a && b`.
    And,
    /// `a || b`.
    Or,
}

impl BinOp {
    /// Whether this operator compares operands and yields a boolean.
    #[must_use]
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }

    /// Whether this operator is a boolean connective.
    #[must_use]
    pub fn is_logical(self) -> bool {
        matches!(self, BinOp::And | BinOp::Or)
    }
}

/// The maximum nesting depth of an expression tree: a literal or variable has
/// depth 1, and each operator adds one level above its deepest operand.
///
/// The parser rejects deeper expressions, and [`Expr::eval`] / [`Expr::infer`]
/// fail with [`ExprError::DepthLimit`] on hand-built ones, so every walk over an
/// expression has bounded recursion. On native targets those walks also grow
/// the stack on demand (`stacker`); on `wasm32` this limit is the only guard.
/// Real rule expressions are a handful of levels deep; 256 leaves ample room
/// while keeping the worst case far inside a default stack.
pub const MAX_EXPR_DEPTH: usize = 256;

/// An expression over bound variables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    /// A reference to a bound variable.
    Var(Symbol),
    /// A literal value.
    Lit(Value),
    /// A unary operation.
    Unary(UnOp, Box<Expr>),
    /// A binary operation.
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

/// An error encountered while type-checking or evaluating an expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExprError {
    /// A variable was referenced that is not in scope.
    UnboundVar(Symbol),
    /// An operator was applied to operands of the wrong type.
    TypeMismatch(String),
    /// An arithmetic fault: division by zero or signed overflow. Surfaced as
    /// an error rather than a panic, with the same conditions as the `ascent!`
    /// macro's checked arithmetic.
    Arithmetic(&'static str),
    /// The expression is nested deeper than [`MAX_EXPR_DEPTH`].
    DepthLimit,
}

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExprError::UnboundVar(s) => write!(f, "unbound variable {}", s.0),
            ExprError::TypeMismatch(m) => write!(f, "type mismatch: {m}"),
            ExprError::Arithmetic(m) => write!(f, "arithmetic fault: {m}"),
            ExprError::DepthLimit => {
                write!(f, "expression nested deeper than {MAX_EXPR_DEPTH} levels")
            }
        }
    }
}

impl std::error::Error for ExprError {}

impl Expr {
    /// Collects the variables referenced by this expression into `out`.
    pub fn collect_vars(&self, out: &mut Vec<Symbol>) {
        match self {
            Expr::Var(s) => {
                if !out.contains(s) {
                    out.push(*s);
                }
            }
            Expr::Lit(_) => {}
            Expr::Unary(_, e) => e.collect_vars(out),
            Expr::Binary(_, a, b) => {
                a.collect_vars(out);
                b.collect_vars(out);
            }
        }
    }

    /// Infers the result [`Type`] of this expression under `env`.
    ///
    /// # Errors
    ///
    /// Returns [`ExprError`] if a variable is unbound or an operator is applied
    /// to incompatible operand types.
    pub fn infer(&self, env: &HashMap<Symbol, Type>) -> Result<Type, ExprError> {
        self.infer_at(env, 1)
    }

    fn infer_at(&self, env: &HashMap<Symbol, Type>, depth: usize) -> Result<Type, ExprError> {
        if depth > MAX_EXPR_DEPTH {
            return Err(ExprError::DepthLimit);
        }
        let next = depth + 1;
        match self {
            Expr::Var(s) => env.get(s).copied().ok_or(ExprError::UnboundVar(*s)),
            Expr::Lit(v) => Ok(v.type_of()),
            Expr::Unary(UnOp::Neg, e) => expect_type(e, env, Type::Int, next).map(|()| Type::Int),
            Expr::Unary(UnOp::Not, e) => expect_type(e, env, Type::Bool, next).map(|()| Type::Bool),
            Expr::Binary(op, a, b) => {
                let ta = stack::guard(|| a.infer_at(env, next))?;
                let tb = stack::guard(|| b.infer_at(env, next))?;
                if op.is_logical() {
                    require(
                        ta == Type::Bool && tb == Type::Bool,
                        "logical op needs bools",
                    )?;
                    Ok(Type::Bool)
                } else if op.is_comparison() {
                    if matches!(op, BinOp::Eq | BinOp::Ne) {
                        require(ta == tb, "== / != needs matching types")?;
                    } else {
                        require(ta == Type::Int && tb == Type::Int, "ordering needs ints")?;
                    }
                    Ok(Type::Bool)
                } else {
                    require(ta == Type::Int && tb == Type::Int, "arithmetic needs ints")?;
                    Ok(Type::Int)
                }
            }
        }
    }

    /// Evaluates this expression under `env`.
    ///
    /// # Errors
    ///
    /// Returns [`ExprError`] if a variable is unbound or a type rule is broken.
    pub fn eval(&self, env: &HashMap<Symbol, Value>) -> Result<Value, ExprError> {
        self.eval_at(env, 1)
    }

    /// The tree depth of this expression (see [`MAX_EXPR_DEPTH`]), computed
    /// iteratively so it is safe on an arbitrarily deep, hand-built tree.
    #[must_use]
    pub fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut stack = vec![(self, 1_usize)];
        while let Some((e, d)) = stack.pop() {
            deepest = deepest.max(d);
            match e {
                Expr::Var(_) | Expr::Lit(_) => {}
                Expr::Unary(_, a) => stack.push((a, d + 1)),
                Expr::Binary(_, a, b) => {
                    stack.push((a, d + 1));
                    stack.push((b, d + 1));
                }
            }
        }
        deepest
    }

    /// [`Expr::eval`] at nesting `depth`. Only every 16th level checks the
    /// stack: a guard costs a few nanoseconds and this is the hot path, while
    /// sixteen frames are far inside the guard's red zone.
    fn eval_at(&self, env: &HashMap<Symbol, Value>, depth: usize) -> Result<Value, ExprError> {
        if depth > MAX_EXPR_DEPTH {
            return Err(ExprError::DepthLimit);
        }
        if depth.is_multiple_of(16) {
            stack::guard(|| self.eval_node(env, depth))
        } else {
            self.eval_node(env, depth)
        }
    }

    fn eval_node(&self, env: &HashMap<Symbol, Value>, depth: usize) -> Result<Value, ExprError> {
        let next = depth + 1;
        match self {
            Expr::Var(s) => env.get(s).copied().ok_or(ExprError::UnboundVar(*s)),
            Expr::Lit(v) => Ok(*v),
            Expr::Unary(UnOp::Neg, e) => {
                Ok(Value::Int(as_int(&e.eval_at(env, next)?)?.wrapping_neg()))
            }
            Expr::Unary(UnOp::Not, e) => Ok(Value::Bool(!as_bool(&e.eval_at(env, next)?)?)),
            // `&&` and `||` short-circuit, exactly as Rust (and therefore the
            // `ascent!` macro) does: the right operand is not
            // evaluated — and so cannot fault — when the left decides the result.
            Expr::Binary(BinOp::And, a, b) => {
                if as_bool(&a.eval_at(env, next)?)? {
                    Ok(Value::Bool(as_bool(&b.eval_at(env, next)?)?))
                } else {
                    Ok(Value::Bool(false))
                }
            }
            Expr::Binary(BinOp::Or, a, b) => {
                if as_bool(&a.eval_at(env, next)?)? {
                    Ok(Value::Bool(true))
                } else {
                    Ok(Value::Bool(as_bool(&b.eval_at(env, next)?)?))
                }
            }
            Expr::Binary(op, a, b) => {
                let va = a.eval_at(env, next)?;
                let vb = b.eval_at(env, next)?;
                eval_binary(*op, va, vb)
            }
        }
    }
}

fn eval_binary(op: BinOp, a: Value, b: Value) -> Result<Value, ExprError> {
    match op {
        BinOp::Add => Ok(Value::Int(as_int(&a)?.wrapping_add(as_int(&b)?))),
        BinOp::Sub => Ok(Value::Int(as_int(&a)?.wrapping_sub(as_int(&b)?))),
        BinOp::Mul => Ok(Value::Int(as_int(&a)?.wrapping_mul(as_int(&b)?))),
        BinOp::Div => as_int(&a)?
            .checked_div(as_int(&b)?)
            .map(Value::Int)
            .ok_or(ExprError::Arithmetic("division by zero or overflow")),
        BinOp::Rem => as_int(&a)?
            .checked_rem(as_int(&b)?)
            .map(Value::Int)
            .ok_or(ExprError::Arithmetic("remainder by zero or overflow")),
        BinOp::Eq => Ok(Value::Bool(a == b)),
        BinOp::Ne => Ok(Value::Bool(a != b)),
        BinOp::Lt => Ok(Value::Bool(as_int(&a)? < as_int(&b)?)),
        BinOp::Le => Ok(Value::Bool(as_int(&a)? <= as_int(&b)?)),
        BinOp::Gt => Ok(Value::Bool(as_int(&a)? > as_int(&b)?)),
        BinOp::Ge => Ok(Value::Bool(as_int(&a)? >= as_int(&b)?)),
        BinOp::And => Ok(Value::Bool(as_bool(&a)? && as_bool(&b)?)),
        BinOp::Or => Ok(Value::Bool(as_bool(&a)? || as_bool(&b)?)),
    }
}

fn expect_type(
    e: &Expr,
    env: &HashMap<Symbol, Type>,
    want: Type,
    depth: usize,
) -> Result<(), ExprError> {
    let got = stack::guard(|| e.infer_at(env, depth))?;
    require(got == want, "operand type")
}

fn require(cond: bool, msg: &str) -> Result<(), ExprError> {
    if cond {
        Ok(())
    } else {
        Err(ExprError::TypeMismatch(msg.to_owned()))
    }
}

fn as_int(v: &Value) -> Result<i64, ExprError> {
    match v {
        Value::Int(i) => Ok(*i),
        _ => Err(ExprError::TypeMismatch("expected int".to_owned())),
    }
}

fn as_bool(v: &Value) -> Result<bool, ExprError> {
    match v {
        Value::Bool(b) => Ok(*b),
        _ => Err(ExprError::TypeMismatch("expected bool".to_owned())),
    }
}
