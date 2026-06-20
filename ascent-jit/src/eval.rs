//! Bottom-up evaluation: stratification plus a fixed-point loop over a
//! relational database.
//!
//! The relational core is an interpreter (a tree-walk of the rule bodies);
//! expression evaluation is delegated to an [`ExprEval`] so the same evaluator
//! runs over either the pure interpreter ([`crate::expr`]) or the WASM tier
//! ([`crate::wasm`]). Within a stratum we iterate every rule to a fixed point;
//! this yields exactly the same relations as semi-naïve evaluation, which is
//! purely a performance refinement over this baseline.

use std::collections::{HashMap, HashSet};

use crate::expr::{Expr, ExprError};
use crate::ir::{
    AggFunc, Aggregate, Arg, Atom, BodyClause, LatticeKind, Program, RelationDecl, RelationKind,
};
use crate::value::{Symbol, Type, Value};

/// Evaluates expressions on behalf of the relational interpreter.
pub trait ExprEval: std::fmt::Debug {
    /// Evaluates `expr` under the variable bindings `env`.
    ///
    /// # Errors
    ///
    /// Returns an [`ExprError`] if evaluation fails (e.g. unbound variable).
    fn eval_expr(&mut self, expr: &Expr, env: &HashMap<Symbol, Value>) -> Result<Value, ExprError>;
}

/// An error raised during program validation or evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// A rule or fact referenced a relation that was never declared.
    UnknownRelation(String),
    /// A tuple or atom had the wrong number of columns for its relation.
    Arity {
        /// The relation name.
        relation: String,
        /// The declared arity.
        expected: usize,
        /// The arity supplied.
        got: usize,
    },
    /// A fact's value at some column had a type that disagrees with the relation's
    /// declared column type. Raised at the ingestion boundary.
    ColumnType {
        /// The relation name.
        relation: String,
        /// The zero-based column index.
        column: usize,
        /// The declared column type.
        expected: Type,
        /// The type of the value supplied.
        got: Type,
    },
    /// The rules cannot be stratified (a cycle passes through negation or
    /// aggregation).
    Stratification(String),
    /// A stratum did not reach a fixed point within the iteration budget. This
    /// is a guard against pathological (e.g. fuzz-generated) programs; it fires
    /// identically regardless of the expression backend.
    IterationLimit,
    /// An expression failed to evaluate.
    Expr(ExprError),
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::UnknownRelation(r) => write!(f, "unknown relation `{r}`"),
            EvalError::Arity {
                relation,
                expected,
                got,
            } => write!(f, "relation `{relation}` has arity {expected}, got {got}"),
            EvalError::ColumnType {
                relation,
                column,
                expected,
                got,
            } => write!(
                f,
                "relation `{relation}` column {column} expects {expected:?}, got {got:?}"
            ),
            EvalError::Stratification(m) => write!(f, "cannot stratify: {m}"),
            EvalError::IterationLimit => write!(f, "stratum did not converge within the budget"),
            EvalError::Expr(e) => write!(f, "expression error: {e}"),
        }
    }
}

impl std::error::Error for EvalError {}

impl From<ExprError> for EvalError {
    fn from(e: ExprError) -> Self {
        EvalError::Expr(e)
    }
}

/// The stored contents of a single relation.
#[derive(Debug, Clone)]
pub struct RelationStore {
    kind: RelationKind,
    /// The declared column types, in order. Its length is the relation's arity.
    schema: Vec<Type>,
    tuples: HashSet<Vec<Value>>,
    /// For lattice relations: key columns → joined value column.
    lattice: HashMap<Vec<Value>, Value>,
}

impl RelationStore {
    fn new(decl: &RelationDecl) -> Self {
        RelationStore {
            kind: decl.kind,
            schema: decl.schema.clone(),
            tuples: HashSet::new(),
            lattice: HashMap::new(),
        }
    }

    fn arity(&self) -> usize {
        self.schema.len()
    }

    /// Inserts a tuple, applying lattice join for lattice relations. Returns
    /// whether the relation changed.
    fn insert(&mut self, tuple: Vec<Value>) -> bool {
        match self.kind {
            RelationKind::Relation => self.tuples.insert(tuple),
            RelationKind::Lattice(kind) => self.insert_lattice(kind, &tuple),
        }
    }

    fn insert_lattice(&mut self, kind: LatticeKind, tuple: &[Value]) -> bool {
        let (key, val) = tuple.split_at(self.arity() - 1);
        let key = key.to_vec();
        let new_val = val[0];
        if let Some(existing) = self.lattice.get(&key).copied() {
            let joined = kind.join(existing, new_val);
            if joined == existing {
                return false;
            }
            let mut old_tuple = key.clone();
            old_tuple.push(existing);
            self.tuples.remove(&old_tuple);
            self.lattice.insert(key.clone(), joined);
            let mut new_tuple = key;
            new_tuple.push(joined);
            self.tuples.insert(new_tuple);
            true
        } else {
            self.lattice.insert(key.clone(), new_val);
            let mut new_tuple = key;
            new_tuple.push(new_val);
            self.tuples.insert(new_tuple)
        }
    }
}

/// A relational database: one [`RelationStore`] per declared relation.
#[derive(Debug, Clone)]
pub struct Database {
    relations: HashMap<String, RelationStore>,
}

impl Database {
    /// Builds an empty database with a store per declared relation.
    #[must_use]
    pub fn new(program: &Program) -> Self {
        let relations = program
            .relations
            .iter()
            .map(|d| (d.name.clone(), RelationStore::new(d)))
            .collect();
        Database { relations }
    }

    /// Inserts a tuple into `relation`, validating arity and applying lattice
    /// join. Returns whether the relation changed.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError`] if the relation is unknown or the arity is wrong.
    pub fn insert(&mut self, relation: &str, tuple: Vec<Value>) -> Result<bool, EvalError> {
        let store = self
            .relations
            .get_mut(relation)
            .ok_or_else(|| EvalError::UnknownRelation(relation.to_owned()))?;
        if tuple.len() != store.arity() {
            return Err(EvalError::Arity {
                relation: relation.to_owned(),
                expected: store.arity(),
                got: tuple.len(),
            });
        }
        Ok(store.insert(tuple))
    }

    /// Asserts an externally-supplied fact into `relation`, validating arity **and**
    /// column types against the declared schema before inserting.
    ///
    /// This is the ingestion boundary: facts entering from *outside* the
    /// engine are type-checked here. The engine's own derivations go through
    /// [`Database::insert`] without the per-column check, because a well-typed program
    /// keeps derived tuples type-correct by construction and the derive path is hot.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::UnknownRelation`] if the relation is undeclared,
    /// [`EvalError::Arity`] on an arity mismatch, or [`EvalError::ColumnType`] if a
    /// value's type disagrees with its declared column type.
    pub fn insert_fact(&mut self, relation: &str, tuple: Vec<Value>) -> Result<bool, EvalError> {
        let store = self
            .relations
            .get(relation)
            .ok_or_else(|| EvalError::UnknownRelation(relation.to_owned()))?;
        if tuple.len() != store.arity() {
            return Err(EvalError::Arity {
                relation: relation.to_owned(),
                expected: store.arity(),
                got: tuple.len(),
            });
        }
        for (column, (value, expected)) in tuple.iter().zip(&store.schema).enumerate() {
            let got = value.type_of();
            if got != *expected {
                return Err(EvalError::ColumnType {
                    relation: relation.to_owned(),
                    column,
                    expected: *expected,
                    got,
                });
            }
        }
        self.insert(relation, tuple)
    }

    /// Returns the tuples of `relation`, or an empty slice's worth if unknown.
    #[must_use]
    pub fn tuples(&self, relation: &str) -> Vec<Vec<Value>> {
        self.relations
            .get(relation)
            .map(|s| s.tuples.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn snapshot(&self, relation: &str) -> Result<Vec<Vec<Value>>, EvalError> {
        self.relations
            .get(relation)
            .map(|s| s.tuples.iter().cloned().collect())
            .ok_or_else(|| EvalError::UnknownRelation(relation.to_owned()))
    }
}

/// Validates that every relation referenced by the program exists and that
/// every atom matches its declared arity, then computes the stratification.
///
/// # Errors
///
/// Returns [`EvalError`] on unknown relations, arity mismatches, or rules that
/// cannot be stratified.
pub fn stratify(program: &Program) -> Result<Vec<Vec<usize>>, EvalError> {
    validate_arities(program)?;
    let strata = relation_strata(program)?;
    let max_stratum = strata.values().copied().max().unwrap_or(0);
    let mut groups = vec![Vec::new(); max_stratum + 1];
    for (idx, rule) in program.rules.iter().enumerate() {
        let s = rule
            .heads
            .iter()
            .map(|h| strata.get(&h.relation).copied().unwrap_or(0))
            .max()
            .unwrap_or(0);
        groups[s].push(idx);
    }
    Ok(groups)
}

fn validate_arities(program: &Program) -> Result<(), EvalError> {
    for rule in &program.rules {
        for head in &rule.heads {
            let want = relation_arity(program, &head.relation)?;
            if head.args.len() != want {
                return Err(EvalError::Arity {
                    relation: head.relation.clone(),
                    expected: want,
                    got: head.args.len(),
                });
            }
        }
        check_query_arities(program, &rule.body)?;
    }
    Ok(())
}

/// The declared arity of `name`, or [`EvalError::UnknownRelation`] if undeclared.
fn relation_arity(program: &Program, name: &str) -> Result<usize, EvalError> {
    program
        .relation(name)
        .map(RelationDecl::arity)
        .ok_or_else(|| EvalError::UnknownRelation(name.to_owned()))
}

/// Validates a single body atom against its declared relation.
fn check_atom_arity(program: &Program, atom: &Atom) -> Result<(), EvalError> {
    let want = relation_arity(program, &atom.relation)?;
    if atom.args.len() != want {
        return Err(EvalError::Arity {
            relation: atom.relation.clone(),
            expected: want,
            got: atom.args.len(),
        });
    }
    Ok(())
}

/// Validates that every relation referenced by a conjunctive body exists and
/// that every atom matches its declared arity. This is the queries-grain
/// schema check: the body has no head relation, so unlike full-rule arity
/// validation there is nothing written to validate — only the atoms read. The
/// same per-clause check is reused when validating ordinary rule bodies.
///
/// # Errors
///
/// Returns [`EvalError`] on an unknown relation or arity mismatch.
pub fn check_query_arities(program: &Program, body: &[BodyClause]) -> Result<(), EvalError> {
    for clause in body {
        match clause {
            BodyClause::Positive(a) | BodyClause::Negative(a) => check_atom_arity(program, a)?,
            BodyClause::Aggregate(agg) => check_atom_arity(program, &agg.source)?,
            BodyClause::Condition(_) | BodyClause::Let { .. } => {}
        }
    }
    Ok(())
}

/// Assigns a stratum number to each relation via longest-path relaxation;
/// detects positive-weight cycles (a cycle through negation/aggregation).
fn relation_strata(program: &Program) -> Result<HashMap<String, usize>, EvalError> {
    // edges: (source relation, dest relation, strict?) — stratum(dest) must be
    // >= stratum(source) (+1 if strict).
    let mut edges: Vec<(String, String, bool)> = Vec::new();
    for rule in &program.rules {
        for head in &rule.heads {
            for clause in &rule.body {
                match clause {
                    BodyClause::Positive(a) => {
                        edges.push((a.relation.clone(), head.relation.clone(), false));
                    }
                    BodyClause::Negative(a) => {
                        edges.push((a.relation.clone(), head.relation.clone(), true));
                    }
                    BodyClause::Aggregate(agg) => {
                        edges.push((agg.source.relation.clone(), head.relation.clone(), true));
                    }
                    BodyClause::Condition(_) | BodyClause::Let { .. } => {}
                }
            }
        }
    }
    let mut strata: HashMap<String, usize> = program
        .relations
        .iter()
        .map(|d| (d.name.clone(), 0))
        .collect();
    let bound = strata.len() + 1;
    for pass in 0..=bound {
        let mut changed = false;
        for (src, dst, strict) in &edges {
            let s = strata.get(src).copied().unwrap_or(0);
            let candidate = s + usize::from(*strict);
            let entry = strata.entry(dst.clone()).or_insert(0);
            if candidate > *entry {
                *entry = candidate;
                changed = true;
            }
        }
        if !changed {
            return Ok(strata);
        }
        if pass == bound {
            return Err(EvalError::Stratification(
                "a cycle passes through negation or aggregation".to_owned(),
            ));
        }
    }
    Ok(strata)
}

/// Runs `program` to a fixed point over `db`, using `evaluator` for expressions.
///
/// # Errors
///
/// Returns [`EvalError`] on stratification failure or any evaluation error.
pub fn run(
    program: &Program,
    db: &mut Database,
    evaluator: &mut dyn ExprEval,
) -> Result<(), EvalError> {
    let strata = stratify(program)?;
    for group in &strata {
        let mut iterations = 0_u32;
        loop {
            let mut changed = false;
            for &rule_idx in group {
                changed |= eval_rule(program, rule_idx, db, evaluator)?;
            }
            if !changed {
                break;
            }
            iterations += 1;
            if iterations > MAX_STRATUM_ITERATIONS {
                return Err(EvalError::IterationLimit);
            }
        }
    }
    Ok(())
}

/// Per-stratum fixed-point iteration cap. Real stratified programs converge in
/// a number of iterations bounded by the active domain; this only bounds the
/// pathological case so the engine never hangs.
const MAX_STRATUM_ITERATIONS: u32 = 1_000_000;

fn eval_rule(
    program: &Program,
    rule_idx: usize,
    db: &mut Database,
    evaluator: &mut dyn ExprEval,
) -> Result<bool, EvalError> {
    let rule = &program.rules[rule_idx];
    let mut env: HashMap<Symbol, Value> = HashMap::new();
    let mut changed = false;
    solve(rule, 0, &mut env, db, evaluator, &mut changed)?;
    Ok(changed)
}

fn solve(
    rule: &crate::ir::Rule,
    idx: usize,
    env: &mut HashMap<Symbol, Value>,
    db: &mut Database,
    evaluator: &mut dyn ExprEval,
    changed: &mut bool,
) -> Result<(), EvalError> {
    if idx == rule.body.len() {
        for head in &rule.heads {
            let mut tuple = Vec::with_capacity(head.args.len());
            for arg in &head.args {
                tuple.push(evaluator.eval_expr(arg, env)?);
            }
            if db.insert(&head.relation, tuple)? {
                *changed = true;
            }
        }
        return Ok(());
    }
    match &rule.body[idx] {
        BodyClause::Positive(atom) => {
            let snapshot = db.snapshot(&atom.relation)?;
            for tuple in &snapshot {
                if let Some(bindings) = unify(&atom.args, tuple, env) {
                    let keys: Vec<Symbol> = bindings.iter().map(|(s, _)| *s).collect();
                    for (s, v) in bindings {
                        env.insert(s, v);
                    }
                    solve(rule, idx + 1, env, db, evaluator, changed)?;
                    for s in keys {
                        env.remove(&s);
                    }
                }
            }
            Ok(())
        }
        BodyClause::Negative(atom) => {
            let snapshot = db.snapshot(&atom.relation)?;
            if !any_match(&atom.args, &snapshot, env) {
                solve(rule, idx + 1, env, db, evaluator, changed)?;
            }
            Ok(())
        }
        BodyClause::Condition(expr) => {
            if evaluator.eval_expr(expr, env)? == Value::Bool(true) {
                solve(rule, idx + 1, env, db, evaluator, changed)?;
            }
            Ok(())
        }
        BodyClause::Let { var, expr } => {
            let v = evaluator.eval_expr(expr, env)?;
            env.insert(*var, v);
            solve(rule, idx + 1, env, db, evaluator, changed)?;
            env.remove(var);
            Ok(())
        }
        BodyClause::Aggregate(agg) => {
            if let Some(result) = eval_aggregate(agg, env, db, evaluator)? {
                env.insert(agg.output, result);
                solve(rule, idx + 1, env, db, evaluator, changed)?;
                env.remove(&agg.output);
            }
            Ok(())
        }
    }
}

/// Unifies `args` against `tuple` given current `env`, returning the new
/// bindings to apply, or `None` if they do not match.
fn unify(
    args: &[Arg],
    tuple: &[Value],
    env: &HashMap<Symbol, Value>,
) -> Option<Vec<(Symbol, Value)>> {
    if args.len() != tuple.len() {
        return None;
    }
    let mut new: Vec<(Symbol, Value)> = Vec::new();
    for (arg, &val) in args.iter().zip(tuple) {
        match arg {
            Arg::Wildcard => {}
            Arg::Lit(v) => {
                if *v != val {
                    return None;
                }
            }
            Arg::Var(s) | Arg::LatticeBind(s) => {
                let bound = env
                    .get(s)
                    .copied()
                    .or_else(|| new.iter().find(|(k, _)| k == s).map(|(_, v)| *v));
                match bound {
                    Some(existing) if existing != val => return None,
                    Some(_) => {}
                    None => new.push((*s, val)),
                }
            }
        }
    }
    Some(new)
}

/// Negation-as-failure existence check: returns true if any tuple matches the
/// atom under `env` (unbound variables match anything and bind nothing).
fn any_match(args: &[Arg], tuples: &[Vec<Value>], env: &HashMap<Symbol, Value>) -> bool {
    tuples.iter().any(|tuple| {
        if args.len() != tuple.len() {
            return false;
        }
        args.iter().zip(tuple).all(|(arg, &val)| match arg {
            Arg::Wildcard | Arg::LatticeBind(_) => true,
            Arg::Lit(v) => *v == val,
            Arg::Var(s) => env.get(s).is_none_or(|&bound| bound == val),
        })
    })
}

fn eval_aggregate(
    agg: &Aggregate,
    env: &HashMap<Symbol, Value>,
    db: &Database,
    evaluator: &mut dyn ExprEval,
) -> Result<Option<Value>, EvalError> {
    let snapshot = db.snapshot(&agg.source.relation)?;
    let mut values: Vec<Value> = Vec::new();
    let mut count: i64 = 0;
    for tuple in &snapshot {
        let Some(bindings) = unify(&agg.source.args, tuple, env) else {
            continue;
        };
        count += 1;
        if let Some(arg) = &agg.arg {
            let mut local = env.clone();
            for (s, v) in bindings {
                local.insert(s, v);
            }
            values.push(evaluator.eval_expr(arg, &local)?);
        }
    }
    let ints = || values.iter().map(value_as_int);
    let result = match agg.func {
        AggFunc::Count => Some(Value::Int(count)),
        AggFunc::Sum => Some(Value::Int(ints().sum())),
        AggFunc::Min => ints().min().map(Value::Int),
        AggFunc::Max => ints().max().map(Value::Int),
    };
    Ok(result)
}

fn value_as_int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        Value::Bool(b) => i64::from(*b),
        Value::Sym(s) => i64::from(s.0),
    }
}

/// A single one-step justification for a tuple: the rule that fired and the
/// positive body tuples (its support) that satisfied it in the materialized
/// database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derivation {
    /// Index into [`Program::rules`] of the rule that fired.
    pub rule: usize,
    /// The positive body tuples that justified the firing, `(relation, tuple)`.
    pub support: Vec<(String, Vec<Value>)>,
}

/// Read-only context threaded through [`explain_solve`], kept in one struct so
/// the recursion stays well under clippy's argument-count limit and so the
/// `rule` being matched is borrowed independently of this mutable state.
struct ExplainState<'a> {
    relation: &'a str,
    target: &'a [Value],
    is_lattice: bool,
    db: &'a Database,
    evaluator: &'a mut dyn ExprEval,
    out: &'a mut Vec<Derivation>,
    rule_idx: usize,
}

/// Finds one-step derivations of `tuple` in `relation` against the materialized
/// `db`: every rule whose head produces `tuple`, paired with the positive body
/// tuples that justify it. Justifications are recomputed on demand, so there is
/// no per-run tracking cost (provenance is "toggleable" by simply not calling
/// this).
///
/// For a lattice relation the value column is the *join* of potentially many
/// firings, so matching is on the key columns only; a returned derivation's
/// computed value need not equal the stored (joined) value.
///
/// # Errors
///
/// Returns [`EvalError`] if `relation` is unknown or a body clause / head
/// expression fails to evaluate.
pub fn explain(
    program: &Program,
    db: &Database,
    evaluator: &mut dyn ExprEval,
    relation: &str,
    tuple: &[Value],
) -> Result<Vec<Derivation>, EvalError> {
    let is_lattice = matches!(
        program
            .relation(relation)
            .ok_or_else(|| EvalError::UnknownRelation(relation.to_owned()))?
            .kind,
        RelationKind::Lattice(_)
    );
    let mut out = Vec::new();
    {
        let mut state = ExplainState {
            relation,
            target: tuple,
            is_lattice,
            db,
            evaluator,
            out: &mut out,
            rule_idx: 0,
        };
        for (rule_idx, rule) in program.rules.iter().enumerate() {
            if !rule.heads.iter().any(|h| h.relation == relation) {
                continue;
            }
            state.rule_idx = rule_idx;
            let mut env: HashMap<Symbol, Value> = HashMap::new();
            let mut support: Vec<(String, Vec<Value>)> = Vec::new();
            explain_solve(&mut state, rule, 0, &mut env, &mut support)?;
        }
    }
    Ok(out)
}

/// Mirrors [`solve`] but read-only: it accumulates the matched positive body
/// tuples as `support` and, at the base case, records a [`Derivation`] for each
/// head that produces the target tuple — instead of inserting anything.
fn explain_solve(
    state: &mut ExplainState,
    rule: &crate::ir::Rule,
    idx: usize,
    env: &mut HashMap<Symbol, Value>,
    support: &mut Vec<(String, Vec<Value>)>,
) -> Result<(), EvalError> {
    if idx == rule.body.len() {
        for head in &rule.heads {
            if head.relation != state.relation {
                continue;
            }
            let mut tuple = Vec::with_capacity(head.args.len());
            for arg in &head.args {
                tuple.push(state.evaluator.eval_expr(arg, env)?);
            }
            let matches = if state.is_lattice {
                tuple.len() == state.target.len()
                    && !tuple.is_empty()
                    && tuple[..tuple.len() - 1] == state.target[..state.target.len() - 1]
            } else {
                tuple == state.target
            };
            if matches {
                state.out.push(Derivation {
                    rule: state.rule_idx,
                    support: support.clone(),
                });
            }
        }
        return Ok(());
    }
    match &rule.body[idx] {
        BodyClause::Positive(atom) => {
            let snapshot = state.db.snapshot(&atom.relation)?;
            for tuple in &snapshot {
                if let Some(bindings) = unify(&atom.args, tuple, env) {
                    let keys: Vec<Symbol> = bindings.iter().map(|(s, _)| *s).collect();
                    for (s, v) in bindings {
                        env.insert(s, v);
                    }
                    support.push((atom.relation.clone(), tuple.clone()));
                    explain_solve(state, rule, idx + 1, env, support)?;
                    support.pop();
                    for s in keys {
                        env.remove(&s);
                    }
                }
            }
            Ok(())
        }
        BodyClause::Negative(atom) => {
            let snapshot = state.db.snapshot(&atom.relation)?;
            if !any_match(&atom.args, &snapshot, env) {
                explain_solve(state, rule, idx + 1, env, support)?;
            }
            Ok(())
        }
        BodyClause::Condition(expr) => {
            if state.evaluator.eval_expr(expr, env)? == Value::Bool(true) {
                explain_solve(state, rule, idx + 1, env, support)?;
            }
            Ok(())
        }
        BodyClause::Let { var, expr } => {
            let v = state.evaluator.eval_expr(expr, env)?;
            env.insert(*var, v);
            explain_solve(state, rule, idx + 1, env, support)?;
            env.remove(var);
            Ok(())
        }
        BodyClause::Aggregate(agg) => {
            if let Some(result) = eval_aggregate(agg, env, state.db, state.evaluator)? {
                env.insert(agg.output, result);
                explain_solve(state, rule, idx + 1, env, support)?;
                env.remove(&agg.output);
            }
            Ok(())
        }
    }
}

/// One solution to an ad-hoc conjunctive query against the current materialized
/// database: the output tuple plus the positive body tuples that justified it
/// (its provenance). Two distinct join combinations that yield the same output
/// tuple appear as two solutions — grouping into distinct rows is left to the
/// caller, who needs the per-combination support to do it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuerySolution {
    /// The output tuple, one value per output expression.
    pub tuple: Vec<Value>,
    /// The positive body tuples that justified this solution, `(relation, tuple)`.
    pub support: Vec<(String, Vec<Value>)>,
}

/// The outcome of an ad-hoc conjunctive query: the solutions found and whether
/// the cardinality cap was reached. When `truncated` is true the solution list
/// is a prefix of the full result set, not the whole of it — a first-class
/// outcome the caller must surface, never an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOutput {
    /// The solutions found, at most `max` of them.
    pub solutions: Vec<QuerySolution>,
    /// True if evaluation stopped at the cardinality cap.
    pub truncated: bool,
}

/// Read-only context threaded through [`query_solve`], mirroring [`ExplainState`]
/// but collecting *every* solution rather than matching one target tuple.
struct QueryState<'a> {
    outputs: &'a [Expr],
    evaluator: &'a mut dyn ExprEval,
    db: &'a Database,
    out: &'a mut Vec<QuerySolution>,
    max: usize,
    truncated: bool,
}

/// Evaluates an ad-hoc conjunctive query — `(outputs) <-- body` — read-only
/// against the current materialized `db`, returning each solution with its
/// provenance and stopping once `max` solutions have been collected.
///
/// This is the queries-grain primitive. The body is a single non-recursive
/// conjunction over already-materialized relations, so — unlike [`run`] — there
/// is no stratification, no fixed-point iteration, and no fork: one read-only
/// traversal suffices, exactly as [`explain`] computes provenance. Nothing is
/// ever inserted, so evaluation cannot mutate the database or any persisted
/// state a host keeps over it.
///
/// The bound is on *cardinality*, not time: such a query always terminates, so
/// the only blast radius is space. Collection stops at `max` solutions and
/// [`QueryOutput::truncated`] is set, so an unfiltered (cartesian-product) join
/// is truncated rather than allowed to exhaust memory.
///
/// # Errors
///
/// Returns [`EvalError`] if the body references an unknown relation or an output
/// / body expression fails to evaluate (e.g. an unbound variable — the caller is
/// expected to have run a range-restriction check first).
pub fn eval_query(
    db: &Database,
    evaluator: &mut dyn ExprEval,
    outputs: &[Expr],
    body: &[BodyClause],
    max: usize,
) -> Result<QueryOutput, EvalError> {
    let mut out = Vec::new();
    let truncated = {
        let mut state = QueryState {
            outputs,
            evaluator,
            db,
            out: &mut out,
            max,
            truncated: false,
        };
        let mut env: HashMap<Symbol, Value> = HashMap::new();
        let mut support: Vec<(String, Vec<Value>)> = Vec::new();
        query_solve(&mut state, body, 0, &mut env, &mut support)?;
        state.truncated
    };
    Ok(QueryOutput {
        solutions: out,
        truncated,
    })
}

/// Mirrors [`explain_solve`] but, at the base case, records a [`QuerySolution`]
/// for *every* satisfying assignment (there is no target to match), honouring the
/// cardinality cap. Once the cap is hit, `truncated` is set and the traversal
/// short-circuits so work stays bounded.
fn query_solve(
    state: &mut QueryState,
    body: &[BodyClause],
    idx: usize,
    env: &mut HashMap<Symbol, Value>,
    support: &mut Vec<(String, Vec<Value>)>,
) -> Result<(), EvalError> {
    if state.truncated {
        return Ok(());
    }
    if idx == body.len() {
        if state.out.len() >= state.max {
            state.truncated = true;
            return Ok(());
        }
        let mut tuple = Vec::with_capacity(state.outputs.len());
        for expr in state.outputs {
            tuple.push(state.evaluator.eval_expr(expr, env)?);
        }
        state.out.push(QuerySolution {
            tuple,
            support: support.clone(),
        });
        return Ok(());
    }
    match &body[idx] {
        BodyClause::Positive(atom) => {
            let snapshot = state.db.snapshot(&atom.relation)?;
            for tuple in &snapshot {
                if let Some(bindings) = unify(&atom.args, tuple, env) {
                    let keys: Vec<Symbol> = bindings.iter().map(|(s, _)| *s).collect();
                    for (s, v) in bindings {
                        env.insert(s, v);
                    }
                    support.push((atom.relation.clone(), tuple.clone()));
                    query_solve(state, body, idx + 1, env, support)?;
                    support.pop();
                    for s in keys {
                        env.remove(&s);
                    }
                }
            }
            Ok(())
        }
        BodyClause::Negative(atom) => {
            let snapshot = state.db.snapshot(&atom.relation)?;
            if !any_match(&atom.args, &snapshot, env) {
                query_solve(state, body, idx + 1, env, support)?;
            }
            Ok(())
        }
        BodyClause::Condition(expr) => {
            if state.evaluator.eval_expr(expr, env)? == Value::Bool(true) {
                query_solve(state, body, idx + 1, env, support)?;
            }
            Ok(())
        }
        BodyClause::Let { var, expr } => {
            let v = state.evaluator.eval_expr(expr, env)?;
            env.insert(*var, v);
            query_solve(state, body, idx + 1, env, support)?;
            env.remove(var);
            Ok(())
        }
        BodyClause::Aggregate(agg) => {
            if let Some(result) = eval_aggregate(agg, env, state.db, state.evaluator)? {
                env.insert(agg.output, result);
                query_solve(state, body, idx + 1, env, support)?;
                env.remove(&agg.output);
            }
            Ok(())
        }
    }
}
