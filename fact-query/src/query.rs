//! The conjunctive-query IR and its safety (range-restriction) check.

use std::collections::HashSet;

use ascent_jit::Symbol;
use ascent_jit::expr::Expr;
use ascent_jit::ir::{Arg, Atom, BodyClause};

use crate::error::FactQueryError;

/// A conjunctive query: an output tuple of expressions over the variables bound
/// by a conjunctive body.
///
/// The body is the conjunctive fragment — positive atoms (joins), `if` filters,
/// `let` bindings, and aggregates over already-materialized relations. It has no
/// head relation (so it cannot write) and no recursion or negation (so it always
/// terminates and is trivially stratifiable). The leaf IR ([`Expr`],
/// [`BodyClause`]) is `ascent-jit`'s; this type adds the queries-grain framing
/// and the range-restriction check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConjunctiveQuery {
    outputs: Vec<Expr>,
    body: Vec<BodyClause>,
}

impl ConjunctiveQuery {
    /// Assembles a query from output expressions and a body. Performs no
    /// checking — see [`ConjunctiveQuery::check_safety`].
    #[must_use]
    pub fn new(outputs: Vec<Expr>, body: Vec<BodyClause>) -> Self {
        Self { outputs, body }
    }

    /// The output column expressions.
    #[must_use]
    pub fn outputs(&self) -> &[Expr] {
        &self.outputs
    }

    /// The body clauses.
    #[must_use]
    pub fn body(&self) -> &[BodyClause] {
        &self.body
    }

    /// Checks that the query is **safe** (contract guarantee 3): it stays inside
    /// the conjunctive fragment (no negation) and is **range-restricted** —
    /// every variable used in an output expression or a filter is bound by a
    /// positive body literal (or a `let` / aggregate over bound variables). This
    /// is what stops an unbound output variable from ranging over an unbounded
    /// domain. It is purely structural and engine-agnostic; `resolve` is used
    /// only to name offending variables in error messages.
    ///
    /// # Errors
    ///
    /// Returns [`FactQueryError::Unsafe`] if the body uses negation or a variable
    /// is not range-restricted.
    pub fn check_safety(
        &self,
        resolve: impl Fn(Symbol) -> Option<String>,
    ) -> Result<(), FactQueryError> {
        let mut bound: HashSet<Symbol> = HashSet::new();
        for clause in &self.body {
            match clause {
                BodyClause::Positive(atom) => collect_atom_vars(atom, &mut bound),
                BodyClause::Let { var, .. } => {
                    bound.insert(*var);
                }
                BodyClause::Aggregate(agg) => {
                    bound.insert(agg.output);
                }
                BodyClause::Negative(_) => {
                    return Err(FactQueryError::Unsafe(
                        "negation is not allowed in a conjunctive query".to_owned(),
                    ));
                }
                BodyClause::Condition(_) => {}
            }
        }

        let name = |s: Symbol| resolve(s).unwrap_or_else(|| format!("<sym {}>", s.0));
        let require_bound = |expr: &Expr, role: &str| -> Result<(), FactQueryError> {
            for var in expr_vars(expr) {
                if !bound.contains(&var) {
                    return Err(FactQueryError::Unsafe(format!(
                        "{role} uses variable `{}`, which no positive body literal binds",
                        name(var)
                    )));
                }
            }
            Ok(())
        };

        for output in &self.outputs {
            require_bound(output, "output expression")?;
        }
        for clause in &self.body {
            if let BodyClause::Condition(expr) = clause {
                require_bound(expr, "filter")?;
            }
        }
        Ok(())
    }
}

/// Adds every variable an atom *binds* (positional `Var` and lattice-`?x` reads)
/// to `bound`. Wildcards and literals bind nothing.
fn collect_atom_vars(atom: &Atom, bound: &mut HashSet<Symbol>) {
    for arg in &atom.args {
        if let Arg::Var(s) | Arg::LatticeBind(s) = arg {
            bound.insert(*s);
        }
    }
}

/// The variables referenced by an expression.
fn expr_vars(expr: &Expr) -> HashSet<Symbol> {
    let mut out = HashSet::new();
    collect_expr_vars(expr, &mut out);
    out
}

fn collect_expr_vars(expr: &Expr, out: &mut HashSet<Symbol>) {
    match expr {
        Expr::Var(s) => {
            out.insert(*s);
        }
        Expr::Lit(_) => {}
        Expr::Unary(_, inner) => collect_expr_vars(inner, out),
        Expr::Binary(_, lhs, rhs) => {
            collect_expr_vars(lhs, out);
            collect_expr_vars(rhs, out);
        }
    }
}
