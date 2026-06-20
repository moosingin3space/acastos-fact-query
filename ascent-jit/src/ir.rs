//! The runtime program IR: the single source of truth both the parser and
//! (eventually) the agent's tools produce.

use crate::expr::Expr;
use crate::value::{Symbol, Type, Value};

/// The lattice ordering applied to a lattice relation's value column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatticeKind {
    /// Join is `max` — the standard ordered lattice on integers.
    Max,
    /// Join is `min` — `Dual<T>`, i.e. "keep the minimum".
    Min,
}

impl LatticeKind {
    /// Joins two lattice values under this ordering.
    #[must_use]
    pub fn join(self, a: Value, b: Value) -> Value {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => match self {
                LatticeKind::Max => Value::Int(x.max(y)),
                LatticeKind::Min => Value::Int(x.min(y)),
            },
            // Non-integer lattices are not supported; fall back to `b`.
            _ => b,
        }
    }
}

/// Whether a relation has set semantics or lattice semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// Ordinary relation: a set of tuples.
    Relation,
    /// Lattice relation: the last column is a lattice value joined on duplicate
    /// keys (the remaining columns).
    Lattice(LatticeKind),
}

/// A relation declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationDecl {
    /// The relation's name.
    pub name: String,
    /// The column types, in order.
    pub schema: Vec<Type>,
    /// Set vs lattice semantics.
    pub kind: RelationKind,
}

impl RelationDecl {
    /// The number of columns.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.schema.len()
    }
}

/// An argument in a body atom (a position to match or bind).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Arg {
    /// Binds (on first occurrence) or constrains (on repeat) a variable.
    Var(Symbol),
    /// Matches a literal constant.
    Lit(Value),
    /// Matches anything, binding nothing.
    Wildcard,
    /// Reads a lattice value column, binding the stored value to a variable
    /// (the `?x` / `?Dual(x)` form). Valid only on a lattice's value column.
    LatticeBind(Symbol),
}

/// A relational atom: a relation name applied to positional arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atom {
    /// The relation referenced.
    pub relation: String,
    /// The positional arguments.
    pub args: Vec<Arg>,
}

/// A built-in aggregation function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc {
    /// Number of matching tuples.
    Count,
    /// Sum of the aggregated expression over matching tuples.
    Sum,
    /// Minimum of the aggregated expression.
    Min,
    /// Maximum of the aggregated expression.
    Max,
}

/// An `agg out = func(arg) in source(..)` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aggregate {
    /// The variable bound to the aggregate result.
    pub output: Symbol,
    /// The aggregation function.
    pub func: AggFunc,
    /// The expression aggregated (absent for [`AggFunc::Count`]).
    pub arg: Option<Expr>,
    /// The relation iterated; its atom may bind locals and filter on outer vars.
    pub source: Atom,
}

/// A clause in a rule body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyClause {
    /// A positive atom: tuples must be present.
    Positive(Atom),
    /// A negated atom: no matching tuple may be present.
    Negative(Atom),
    /// An `if` guard.
    Condition(Expr),
    /// A `let` binding.
    Let {
        /// The bound variable.
        var: Symbol,
        /// The bound expression.
        expr: Expr,
    },
    /// An aggregation.
    Aggregate(Aggregate),
}

/// A head atom; its arguments are expressions over the body's bound variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadAtom {
    /// The relation written to.
    pub relation: String,
    /// The column expressions, in order.
    pub args: Vec<Expr>,
}

/// A rule: one or more heads derived from a conjunctive body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The head atoms derived when the body is satisfied.
    pub heads: Vec<HeadAtom>,
    /// The body clauses, evaluated left to right.
    pub body: Vec<BodyClause>,
}

/// A complete program: relation declarations plus rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Program {
    /// The relation declarations.
    pub relations: Vec<RelationDecl>,
    /// The rules.
    pub rules: Vec<Rule>,
}

impl Program {
    /// Looks up a relation declaration by name.
    #[must_use]
    pub fn relation(&self, name: &str) -> Option<&RelationDecl> {
        self.relations.iter().find(|r| r.name == name)
    }

    /// Collects every top-level expression the rules evaluate: each head-atom
    /// column expression and each body `if` condition, `let` binding, and
    /// aggregate argument. These are exactly the expressions handed to
    /// [`ExprEval::eval_expr`](crate::eval::ExprEval::eval_expr) during a run, so
    /// a compiling backend can prime them all up front
    /// ([`ExprEval::prime`](crate::eval::ExprEval::prime)). Sub-expressions are
    /// not listed separately — they are inlined into their enclosing
    /// expression's compiled function.
    #[must_use]
    pub fn exprs(&self) -> Vec<&Expr> {
        let mut out = Vec::new();
        for rule in &self.rules {
            for head in &rule.heads {
                out.extend(head.args.iter());
            }
            for clause in &rule.body {
                match clause {
                    BodyClause::Condition(e) | BodyClause::Let { expr: e, .. } => out.push(e),
                    BodyClause::Aggregate(agg) => out.extend(agg.arg.iter()),
                    BodyClause::Positive(_) | BodyClause::Negative(_) => {}
                }
            }
        }
        out
    }
}
