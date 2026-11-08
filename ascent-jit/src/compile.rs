//! Rule bodies compiled for evaluation: dense variable slots, per-column match
//! operations, and expression bytecode.
//!
//! Which variables are bound at each clause of a body is known statically
//! (bodies are evaluated left to right), so planning assigns every variable a
//! slot in a flat `[Value]` environment and resolves each use to either a slot
//! read or a slot write. At run time matching a row is a loop over [`ColOp`]s
//! and evaluating an expression is a loop over [`Code`]: no hashing, no undo
//! log, and no recursion.
//!
//! Binding sites follow Ascent's (Rust's) scoping: a `let` or an aggregate's
//! output always introduces a *fresh* slot, shadowing any outer variable of the
//! same name for the rest of the body, and an aggregate's source-local
//! variables are visible only to its argument. A variable used where it is not
//! bound compiles to [`Op::Unbound`], which fails with
//! [`ExprError::UnboundVar`] only if that code is actually evaluated.

use std::collections::HashMap;

use crate::expr::{BinOp, Expr, ExprError, MAX_EXPR_DEPTH, UnOp, as_bool, as_int, eval_binary};
use crate::ir::Arg;
use crate::stack;
use crate::value::{Symbol, Value};

/// An index into a body's flat environment.
pub(crate) type SlotId = usize;

/// The variables bound at one point in a body, mapped to their slots. (The
/// body-wide slot counter is threaded alongside, so cloned scopes never reuse
/// a slot.)
#[derive(Debug, Clone, Default)]
pub(crate) struct Scope {
    bound: HashMap<Symbol, SlotId>,
}

impl Scope {
    /// The slot holding `var`, if it is bound here.
    pub(crate) fn get(&self, var: Symbol) -> Option<SlotId> {
        self.bound.get(&var).copied()
    }

    /// Binds `var` to a fresh slot from `next`, shadowing any outer binding.
    pub(crate) fn bind_fresh(&mut self, var: Symbol, next: &mut SlotId) -> SlotId {
        let slot = *next;
        *next += 1;
        self.bound.insert(var, slot);
        slot
    }
}

/// What matching one column of a stored row does.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ColOp {
    /// Matches anything (a wildcard, or an unbound variable under negation).
    Skip,
    /// Must equal this constant.
    Lit(Value),
    /// Writes the column into a slot (a variable's first occurrence).
    Bind(SlotId),
    /// Must equal the value already in a slot.
    Check(SlotId),
}

/// Compiles a positive atom's (or an aggregate source's) arguments, binding
/// each not-yet-bound variable to a fresh slot in `scope`.
pub(crate) fn match_ops(args: &[Arg], scope: &mut Scope, next: &mut SlotId) -> Vec<ColOp> {
    args.iter()
        .map(|arg| match arg {
            Arg::Wildcard => ColOp::Skip,
            Arg::Lit(v) => ColOp::Lit(*v),
            Arg::Var(s) | Arg::LatticeBind(s) => match scope.get(*s) {
                Some(slot) => ColOp::Check(slot),
                None => ColOp::Bind(scope.bind_fresh(*s, next)),
            },
        })
        .collect()
}

/// Compiles a negated atom's arguments. Negation binds nothing: an unbound
/// variable (and a `?x` lattice read) matches anything.
pub(crate) fn negated_ops(args: &[Arg], scope: &Scope) -> Vec<ColOp> {
    args.iter()
        .map(|arg| match arg {
            Arg::Wildcard | Arg::LatticeBind(_) => ColOp::Skip,
            Arg::Lit(v) => ColOp::Lit(*v),
            Arg::Var(s) => scope.get(*s).map_or(ColOp::Skip, ColOp::Check),
        })
        .collect()
}

/// Matches `row` against `ops`, writing bound columns into `env`. On a
/// mismatch some slots may already have been written; that is harmless, since
/// planning never reads a slot that is not bound at that point.
#[inline]
pub(crate) fn match_row(ops: &[ColOp], row: &[Value], env: &mut [Value]) -> bool {
    for (op, &val) in ops.iter().zip(row) {
        match *op {
            ColOp::Skip => {}
            ColOp::Lit(v) => {
                if v != val {
                    return false;
                }
            }
            ColOp::Bind(slot) => env[slot] = val,
            ColOp::Check(slot) => {
                if env[slot] != val {
                    return false;
                }
            }
        }
    }
    true
}

/// One bytecode instruction. Operands are taken from, and results pushed to, a
/// value stack.
#[derive(Debug, Clone, Copy)]
enum Op {
    Lit(Value),
    Load(SlotId),
    /// A variable that is not bound at this point.
    Unbound(Symbol),
    Neg,
    Not,
    /// A non-short-circuiting binary operator.
    Bin(BinOp),
    /// `&&`: pop a bool; if false, push `false` and skip the right operand.
    AndThen(u32),
    /// `||`: pop a bool; if true, push `true` and skip the right operand.
    OrElse(u32),
    /// Pop a value that must be a bool and push it back as one (the right
    /// operand of `&&` / `||`).
    ToBool,
}

/// A compiled expression: postfix bytecode over slots.
///
/// It has exactly the semantics of [`Expr::eval`] (the oracle it is fuzzed
/// against): operands left to right, `&&`/`||` short-circuiting, and the same
/// error for the same input.
#[derive(Debug, Clone)]
pub(crate) struct Code {
    ops: Vec<Op>,
}

impl Code {
    /// Compiles `expr` against the variables bound in `scope`.
    ///
    /// # Errors
    ///
    /// [`ExprError::DepthLimit`] if `expr` is nested deeper than
    /// [`MAX_EXPR_DEPTH`] (checked without recursing, before compiling).
    pub(crate) fn compile(expr: &Expr, scope: &Scope) -> Result<Self, ExprError> {
        if expr.depth() > MAX_EXPR_DEPTH {
            return Err(ExprError::DepthLimit);
        }
        let mut ops = Vec::new();
        emit(expr, scope, &mut ops);
        Ok(Code { ops })
    }

    /// Evaluates against `env`, using `stack` as scratch space.
    ///
    /// # Errors
    ///
    /// The same [`ExprError`] [`Expr::eval`] would return.
    pub(crate) fn run(&self, env: &[Value], stack: &mut Vec<Value>) -> Result<Value, ExprError> {
        stack.clear();
        let mut pc = 0;
        while let Some(&op) = self.ops.get(pc) {
            pc += 1;
            match op {
                Op::Lit(v) => stack.push(v),
                Op::Load(slot) => stack.push(env[slot]),
                Op::Unbound(var) => return Err(ExprError::UnboundVar(var)),
                Op::Neg => {
                    let v = pop(stack);
                    stack.push(Value::Int(as_int(&v)?.wrapping_neg()));
                }
                Op::Not => {
                    let v = pop(stack);
                    stack.push(Value::Bool(!as_bool(&v)?));
                }
                Op::Bin(op) => {
                    let b = pop(stack);
                    let a = pop(stack);
                    stack.push(eval_binary(op, a, b)?);
                }
                Op::AndThen(skip) => {
                    if !as_bool(&pop(stack))? {
                        stack.push(Value::Bool(false));
                        pc += skip as usize;
                    }
                }
                Op::OrElse(skip) => {
                    if as_bool(&pop(stack))? {
                        stack.push(Value::Bool(true));
                        pc += skip as usize;
                    }
                }
                Op::ToBool => {
                    let v = pop(stack);
                    stack.push(Value::Bool(as_bool(&v)?));
                }
            }
        }
        Ok(pop(stack))
    }
}

/// Pops an operand. Compiled code is stack-balanced by construction, so the
/// stack is never empty here.
#[inline]
fn pop(stack: &mut Vec<Value>) -> Value {
    stack.pop().unwrap_or(Value::Bool(false))
}

/// Emits postfix code for `expr`. Recursion depth is bounded by
/// [`MAX_EXPR_DEPTH`] (checked by [`Code::compile`]) and stack-guarded.
fn emit(expr: &Expr, scope: &Scope, ops: &mut Vec<Op>) {
    stack::guard(|| match expr {
        Expr::Lit(v) => ops.push(Op::Lit(*v)),
        Expr::Var(s) => ops.push(scope.get(*s).map_or(Op::Unbound(*s), Op::Load)),
        Expr::Unary(op, e) => {
            emit(e, scope, ops);
            ops.push(match op {
                UnOp::Neg => Op::Neg,
                UnOp::Not => Op::Not,
            });
        }
        Expr::Binary(op @ (BinOp::And | BinOp::Or), a, b) => {
            emit(a, scope, ops);
            let jump = ops.len();
            ops.push(Op::ToBool); // placeholder, patched below
            emit(b, scope, ops);
            ops.push(Op::ToBool);
            let skip = u32::try_from(ops.len() - jump - 1).unwrap_or(u32::MAX);
            ops[jump] = if *op == BinOp::And {
                Op::AndThen(skip)
            } else {
                Op::OrElse(skip)
            };
        }
        Expr::Binary(op, a, b) => {
            emit(a, scope, ops);
            emit(b, scope, ops);
            ops.push(Op::Bin(*op));
        }
    });
}
