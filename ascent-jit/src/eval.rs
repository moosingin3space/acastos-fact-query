//! Bottom-up evaluation: stratification plus a semi-naïve, indexed fixed-point
//! loop over a relational database.
//!
//! The relational core is an interpreter (a nested-loop walk of the rule
//! bodies); `if`/`let`/head expressions are evaluated by [`Expr::eval`]. See
//! [`run`] for the evaluation strategy and ADR 0009 for why it is shaped this
//! way.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::expr::{Expr, ExprError};
use crate::ir::{
    AggFunc, Aggregate, Arg, Atom, BodyClause, HeadAtom, LatticeKind, Program, RelationDecl,
    RelationKind, Rule,
};
use crate::stack;
use crate::value::{Symbol, Type, Value};

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
/// A row's position in its relation's append-only row log.
type RowId = usize;

/// Variable bindings threaded through a body traversal.
type Env = HashMap<Symbol, Value>;

/// A hash index over a fixed set of key columns: key values → the ids of the
/// rows carrying them. Row ids are pushed in append order, so every posting
/// list is sorted — which is what lets a lookup be restricted to a delta window
/// with two binary searches.
#[derive(Debug, Clone)]
struct Index {
    cols: Vec<usize>,
    map: HashMap<Vec<Value>, Vec<RowId>>,
}

impl Index {
    fn build(cols: Vec<usize>, rows: &[Vec<Value>]) -> Self {
        let mut index = Index {
            cols,
            map: HashMap::new(),
        };
        for (id, row) in rows.iter().enumerate() {
            index.add(id, row);
        }
        index
    }

    fn add(&mut self, id: RowId, row: &[Value]) {
        let key = self.cols.iter().map(|&c| row[c]).collect();
        self.map.entry(key).or_default().push(id);
    }

    fn get(&self, key: &[Value]) -> &[RowId] {
        self.map.get(key).map_or(&[], Vec::as_slice)
    }
}

/// The stored contents of a single relation.
///
/// Tuples live in an append-only row log, so "the tuples derived since point
/// *p*" is just the row range `p..`: semi-naïve deltas are ranges, not copies.
/// A lattice tuple superseded by a joined value is tombstoned (its `live` bit
/// cleared) and the joined tuple appended; every reader skips dead rows.
#[derive(Debug, Clone)]
pub struct RelationStore {
    name: String,
    kind: RelationKind,
    /// The declared column types, in order. Its length is the relation's arity.
    schema: Vec<Type>,
    rows: Vec<Vec<Value>>,
    live: Vec<bool>,
    /// Set relations: the live tuples, for deduplication.
    members: HashSet<Vec<Value>>,
    /// Lattice relations: key columns → (joined value, id of its live row).
    lattice: HashMap<Vec<Value>, (Value, RowId)>,
    indexes: Vec<Index>,
    /// Rows below this mark were closed under the rules at the end of the last
    /// successful [`run`]; meaningful only while [`Database::at_fixpoint`].
    closed: usize,
}

impl RelationStore {
    fn new(decl: &RelationDecl) -> Self {
        RelationStore {
            name: decl.name.clone(),
            kind: decl.kind,
            schema: decl.schema.clone(),
            rows: Vec::new(),
            live: Vec::new(),
            members: HashSet::new(),
            lattice: HashMap::new(),
            indexes: Vec::new(),
            closed: 0,
        }
    }

    fn arity(&self) -> usize {
        self.schema.len()
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    /// Inserts a tuple, applying lattice join for lattice relations. Returns
    /// whether the relation changed.
    fn insert(&mut self, tuple: Vec<Value>) -> bool {
        match self.kind {
            RelationKind::Relation => {
                if !self.members.insert(tuple.clone()) {
                    return false;
                }
                self.push(tuple);
                true
            }
            RelationKind::Lattice(kind) => self.insert_lattice(kind, tuple),
        }
    }

    fn insert_lattice(&mut self, kind: LatticeKind, tuple: Vec<Value>) -> bool {
        let split = self.arity() - 1;
        let key = tuple[..split].to_vec();
        let new_val = tuple[split];
        if let Some((existing, old_id)) = self.lattice.get(&key).copied() {
            let joined = kind.join(existing, new_val);
            if joined == existing {
                return false;
            }
            self.live[old_id] = false;
            let mut row = key.clone();
            row.push(joined);
            let id = self.push(row);
            self.lattice.insert(key, (joined, id));
        } else {
            let id = self.push(tuple);
            self.lattice.insert(key, (new_val, id));
        }
        true
    }

    fn push(&mut self, row: Vec<Value>) -> RowId {
        let id = self.rows.len();
        for index in &mut self.indexes {
            index.add(id, &row);
        }
        self.rows.push(row);
        self.live.push(true);
        id
    }

    /// Whether `tuple` is currently stored (for a lattice: with exactly this
    /// joined value).
    fn contains(&self, tuple: &[Value]) -> bool {
        match self.kind {
            RelationKind::Relation => self.members.contains(tuple),
            RelationKind::Lattice(_) => {
                let split = self.arity() - 1;
                self.lattice
                    .get(&tuple[..split])
                    .is_some_and(|&(v, _)| v == tuple[split])
            }
        }
    }

    fn live_rows(&self) -> impl Iterator<Item = &Vec<Value>> {
        self.rows
            .iter()
            .zip(&self.live)
            .filter_map(|(row, &live)| live.then_some(row))
    }

    fn index_of(&self, cols: &[usize]) -> Option<usize> {
        self.indexes.iter().position(|i| i.cols == cols)
    }

    fn ensure_index(&mut self, cols: &[usize]) -> usize {
        if let Some(i) = self.index_of(cols) {
            return i;
        }
        self.indexes.push(Index::build(cols.to_vec(), &self.rows));
        self.indexes.len() - 1
    }
}

/// A relational database: one [`RelationStore`] per declared relation.
#[derive(Debug, Clone)]
pub struct Database {
    relations: Vec<RelationStore>,
    ids: HashMap<String, usize>,
    /// True once a [`run`] has completed: every relation's rows below its
    /// `closed` mark are then closed under the rules, so the next run only has
    /// to process the rows appended since (incremental re-evaluation).
    at_fixpoint: bool,
}

impl Database {
    /// Builds an empty database with a store per declared relation, plus the
    /// hash indexes the program's rule bodies will probe.
    #[must_use]
    pub fn new(program: &Program) -> Self {
        let relations: Vec<RelationStore> =
            program.relations.iter().map(RelationStore::new).collect();
        let ids = relations
            .iter()
            .enumerate()
            .map(|(i, s)| (s.name.clone(), i))
            .collect();
        let mut db = Database {
            relations,
            ids,
            at_fixpoint: false,
        };
        // An invalid program is rejected by `stratify`; here we only skip it.
        let _ = plan_rules(program, &mut db);
        db
    }

    fn id(&self, relation: &str) -> Result<usize, EvalError> {
        self.ids
            .get(relation)
            .copied()
            .ok_or_else(|| EvalError::UnknownRelation(relation.to_owned()))
    }

    fn store(&self, relation: &str) -> Result<&RelationStore, EvalError> {
        Ok(&self.relations[self.id(relation)?])
    }

    fn lens(&self) -> Vec<usize> {
        self.relations.iter().map(RelationStore::len).collect()
    }

    /// Inserts a tuple into `relation`, validating arity and applying lattice
    /// join. Returns whether the relation changed.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError`] if the relation is unknown or the arity is wrong.
    pub fn insert(&mut self, relation: &str, tuple: Vec<Value>) -> Result<bool, EvalError> {
        let id = self.id(relation)?;
        let store = &mut self.relations[id];
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
        let store = self.store(relation)?;
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

    /// Returns the tuples of `relation`, in insertion order, or an empty vec if
    /// the relation is unknown.
    #[must_use]
    pub fn tuples(&self, relation: &str) -> Vec<Vec<Value>> {
        self.store(relation)
            .map(|s| s.live_rows().cloned().collect())
            .unwrap_or_default()
    }

    /// The tuples of `relation` present here but not in `base`, where `self`
    /// is a fork of `base` that has only been appended to since (the shape
    /// [`crate::Engine::speculate`] produces). Only the rows past `base`'s log
    /// can differ, so this costs the size of the delta, not of the relation.
    pub(crate) fn added_since(&self, base: &Database, relation: &str) -> Vec<Vec<Value>> {
        let (Ok(fork), Ok(before)) = (self.store(relation), base.store(relation)) else {
            return Vec::new();
        };
        (before.len()..fork.len())
            .filter(|&id| fork.live[id] && !before.contains(&fork.rows[id]))
            .map(|id| fork.rows[id].clone())
            .collect()
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

// ----- planning ------------------------------------------------------------

/// Where one column of an index key comes from at probe time.
#[derive(Debug, Clone, Copy)]
enum KeyPart {
    Lit(Value),
    Var(Symbol),
}

/// Which index (if any) an atom probes.
#[derive(Debug, Clone, Copy)]
enum Slot {
    /// No usable key: scan the row range.
    Scan,
    /// A persistent index owned by the relation's store.
    Db(usize),
    /// A transient index built for one read-only call (see [`attach_transient`]).
    Local(usize),
}

/// How a body atom reaches its relation: the columns already bound when the
/// atom is visited (literals, plus variables bound by earlier clauses), and the
/// index keyed on exactly those columns.
#[derive(Debug, Clone)]
struct Probe {
    rel: usize,
    key: Vec<(usize, KeyPart)>,
    slot: Slot,
}

impl Probe {
    fn new(
        db: &Database,
        atom: &Atom,
        bound: &HashSet<Symbol>,
        negated: bool,
    ) -> Result<Self, EvalError> {
        let rel = db.id(&atom.relation)?;
        let mut key = Vec::new();
        for (col, arg) in atom.args.iter().enumerate() {
            match arg {
                Arg::Lit(v) => key.push((col, KeyPart::Lit(*v))),
                Arg::Var(s) if bound.contains(s) => key.push((col, KeyPart::Var(*s))),
                // A negated `?x` matches anything (see `any_match`), so it is
                // only a key column in a positive atom.
                Arg::LatticeBind(s) if !negated && bound.contains(s) => {
                    key.push((col, KeyPart::Var(*s)));
                }
                Arg::Var(_) | Arg::LatticeBind(_) | Arg::Wildcard => {}
            }
        }
        Ok(Probe {
            rel,
            key,
            slot: Slot::Scan,
        })
    }

    fn cols(&self) -> Vec<usize> {
        self.key.iter().map(|&(c, _)| c).collect()
    }

    /// The key values under `env`, or `None` if a key variable is unbound (which
    /// planning rules out; the caller then falls back to a scan).
    fn key_values(&self, env: &Env) -> Option<Vec<Value>> {
        self.key
            .iter()
            .map(|&(_, part)| match part {
                KeyPart::Lit(v) => Some(v),
                KeyPart::Var(s) => env.get(&s).copied(),
            })
            .collect()
    }
}

/// One body clause, resolved against a database.
#[derive(Debug)]
enum Step<'p> {
    Positive { atom: &'p Atom, probe: Probe },
    Negative { atom: &'p Atom, probe: Probe },
    Condition(&'p Expr),
    Let { var: Symbol, expr: &'p Expr },
    Aggregate { agg: &'p Aggregate, probe: Probe },
}

impl Step<'_> {
    fn probe_mut(&mut self) -> Option<&mut Probe> {
        match self {
            Step::Positive { probe, .. }
            | Step::Negative { probe, .. }
            | Step::Aggregate { probe, .. } => Some(probe),
            Step::Condition(_) | Step::Let { .. } => None,
        }
    }
}

/// Resolves a body left to right, tracking which variables are bound at each
/// clause so every atom knows its key columns. Slots start as [`Slot::Scan`];
/// an `attach_*` pass assigns indexes.
fn plan_body<'p>(db: &Database, body: &'p [BodyClause]) -> Result<Vec<Step<'p>>, EvalError> {
    let mut bound: HashSet<Symbol> = HashSet::new();
    let mut steps = Vec::with_capacity(body.len());
    for clause in body {
        steps.push(match clause {
            BodyClause::Positive(atom) => {
                let probe = Probe::new(db, atom, &bound, false)?;
                for arg in &atom.args {
                    if let Arg::Var(s) | Arg::LatticeBind(s) = arg {
                        bound.insert(*s);
                    }
                }
                Step::Positive { atom, probe }
            }
            BodyClause::Negative(atom) => Step::Negative {
                atom,
                probe: Probe::new(db, atom, &bound, true)?,
            },
            BodyClause::Condition(expr) => Step::Condition(expr),
            BodyClause::Let { var, expr } => {
                bound.insert(*var);
                Step::Let { var: *var, expr }
            }
            BodyClause::Aggregate(agg) => {
                let probe = Probe::new(db, &agg.source, &bound, false)?;
                bound.insert(agg.output);
                Step::Aggregate { agg, probe }
            }
        });
    }
    Ok(steps)
}

/// Points every keyed probe at a persistent index, building any that are
/// missing. Indexes are maintained on insert from then on.
fn attach_persistent(db: &mut Database, steps: &mut [Step<'_>]) {
    for probe in steps.iter_mut().filter_map(Step::probe_mut) {
        if !probe.key.is_empty() {
            let cols = probe.cols();
            probe.slot = Slot::Db(db.relations[probe.rel].ensure_index(&cols));
        }
    }
}

/// Read-only variant of [`attach_persistent`]: reuses a persistent index when
/// one exists, otherwise builds a transient one into `locals` (keyed by
/// relation and columns). This keeps the read-only entry points
/// ([`eval_query`], [`explain`]) literally unable to mutate the database, at
/// the cost of one linear build per access path the program did not declare.
fn attach_transient(db: &Database, steps: &mut [Step<'_>], locals: &mut Vec<(usize, Index)>) {
    for probe in steps.iter_mut().filter_map(Step::probe_mut) {
        if probe.key.is_empty() {
            continue;
        }
        let cols = probe.cols();
        let store = &db.relations[probe.rel];
        probe.slot = if let Some(i) = store.index_of(&cols) {
            Slot::Db(i)
        } else if let Some(j) = locals
            .iter()
            .position(|(rel, ix)| *rel == probe.rel && ix.cols == cols)
        {
            Slot::Local(j)
        } else {
            locals.push((probe.rel, Index::build(cols, &store.rows)));
            Slot::Local(locals.len() - 1)
        };
    }
}

// ----- traversal -----------------------------------------------------------

/// The live row ids a probe yields: either a range scan or an index posting
/// list, both already restricted to the visit's row window.
enum Rows<'d> {
    Scan {
        live: &'d [bool],
        next: RowId,
        end: RowId,
    },
    Ids {
        live: &'d [bool],
        ids: std::slice::Iter<'d, RowId>,
    },
}

impl Iterator for Rows<'_> {
    type Item = RowId;

    fn next(&mut self) -> Option<RowId> {
        match self {
            Rows::Scan { live, next, end } => {
                while *next < *end {
                    let id = *next;
                    *next += 1;
                    if live[id] {
                        return Some(id);
                    }
                }
                None
            }
            Rows::Ids { live, ids } => ids.by_ref().copied().find(|&id| live[id]),
        }
    }
}

/// The whole relation, whatever its current length.
const ALL: Range<RowId> = 0..RowId::MAX;

/// Receives each satisfying assignment of a body.
trait Sink {
    /// True once the sink wants no more solutions (the query cap).
    fn stop(&self) -> bool {
        false
    }

    /// Called once per satisfying assignment, with the matched positive rows.
    fn emit(
        &mut self,
        db: &Database,
        env: &Env,
        support: &[(usize, RowId)],
    ) -> Result<(), EvalError>;
}

/// The single nested-loop join every entry point shares: [`run`] (deriving),
/// [`explain`] (matching one target), and [`eval_query`] (collecting). Reads
/// never mutate the database, so rows are borrowed, never cloned.
struct Walk<'a> {
    db: &'a Database,
    locals: &'a [(usize, Index)],
}

impl<'a> Walk<'a> {
    fn rows(&self, probe: &Probe, env: &Env, window: &Range<RowId>) -> Rows<'a> {
        let db: &'a Database = self.db;
        let store = &db.relations[probe.rel];
        let end = window.end.min(store.len());
        let start = window.start.min(end);
        let index = match probe.slot {
            Slot::Scan => None,
            Slot::Db(i) => Some(&store.indexes[i]),
            Slot::Local(i) => Some(&self.locals[i].1),
        };
        if let (Some(index), Some(key)) = (index, probe.key_values(env)) {
            let ids = index.get(&key);
            let lo = ids.partition_point(|&id| id < start);
            let hi = ids.partition_point(|&id| id < end);
            return Rows::Ids {
                live: &store.live,
                ids: ids[lo..hi].iter(),
            };
        }
        Rows::Scan {
            live: &store.live,
            next: start,
            end,
        }
    }

    /// Solves `steps[idx..]`. `windows[i]` is the row range positive step `i`
    /// may read (the semi-naïve delta discipline); other steps read everything.
    ///
    /// Recursion depth is the body's clause count, which nothing bounds, so
    /// each level runs under [`stack::guard`].
    fn walk<S: Sink>(
        &mut self,
        steps: &[Step<'_>],
        windows: &[Range<RowId>],
        idx: usize,
        env: &mut Env,
        support: &mut Vec<(usize, RowId)>,
        sink: &mut S,
    ) -> Result<(), EvalError> {
        stack::guard(|| self.walk_step(steps, windows, idx, env, support, sink))
    }

    fn walk_step<S: Sink>(
        &mut self,
        steps: &[Step<'_>],
        windows: &[Range<RowId>],
        idx: usize,
        env: &mut Env,
        support: &mut Vec<(usize, RowId)>,
        sink: &mut S,
    ) -> Result<(), EvalError> {
        if sink.stop() {
            return Ok(());
        }
        let Some(step) = steps.get(idx) else {
            return sink.emit(self.db, env, support);
        };
        match step {
            Step::Positive { atom, probe } => {
                let db: &'a Database = self.db;
                let store = &db.relations[probe.rel];
                for id in self.rows(probe, env, &windows[idx]) {
                    let Some(bindings) = unify(&atom.args, &store.rows[id], env) else {
                        continue;
                    };
                    let keys: Vec<Symbol> = bindings.iter().map(|(s, _)| *s).collect();
                    env.extend(bindings);
                    support.push((probe.rel, id));
                    self.walk(steps, windows, idx + 1, env, support, sink)?;
                    support.pop();
                    for s in keys {
                        env.remove(&s);
                    }
                }
                Ok(())
            }
            Step::Negative { atom, probe } => {
                let store = &self.db.relations[probe.rel];
                let found = self
                    .rows(probe, env, &ALL)
                    .any(|id| neg_match(&atom.args, &store.rows[id], env));
                if !found {
                    self.walk(steps, windows, idx + 1, env, support, sink)?;
                }
                Ok(())
            }
            Step::Condition(expr) => {
                if expr.eval(env)? == Value::Bool(true) {
                    self.walk(steps, windows, idx + 1, env, support, sink)?;
                }
                Ok(())
            }
            Step::Let { var, expr } => {
                let v = expr.eval(env)?;
                env.insert(*var, v);
                self.walk(steps, windows, idx + 1, env, support, sink)?;
                env.remove(var);
                Ok(())
            }
            Step::Aggregate { agg, probe } => {
                if let Some(result) = self.aggregate(agg, probe, env)? {
                    env.insert(agg.output, result);
                    self.walk(steps, windows, idx + 1, env, support, sink)?;
                    env.remove(&agg.output);
                }
                Ok(())
            }
        }
    }

    fn aggregate(
        &mut self,
        agg: &Aggregate,
        probe: &Probe,
        env: &Env,
    ) -> Result<Option<Value>, EvalError> {
        let db: &'a Database = self.db;
        let store = &db.relations[probe.rel];
        let mut values: Vec<Value> = Vec::new();
        let mut count: i64 = 0;
        for id in self.rows(probe, env, &ALL) {
            let Some(bindings) = unify(&agg.source.args, &store.rows[id], env) else {
                continue;
            };
            count += 1;
            if let Some(arg) = &agg.arg {
                let mut local = env.clone();
                local.extend(bindings);
                values.push(arg.eval(&local)?);
            }
        }
        let ints = || values.iter().map(value_as_int);
        Ok(match agg.func {
            AggFunc::Count => Some(Value::Int(count)),
            AggFunc::Sum => Some(Value::Int(ints().sum())),
            AggFunc::Min => ints().min().map(Value::Int),
            AggFunc::Max => ints().max().map(Value::Int),
        })
    }
}

/// Unifies `args` against `tuple` given current `env`, returning the new
/// bindings to apply, or `None` if they do not match.
fn unify(args: &[Arg], tuple: &[Value], env: &Env) -> Option<Vec<(Symbol, Value)>> {
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

/// Negation-as-failure match for one tuple: unbound variables match anything
/// and bind nothing.
fn neg_match(args: &[Arg], tuple: &[Value], env: &Env) -> bool {
    args.len() == tuple.len()
        && args.iter().zip(tuple).all(|(arg, &val)| match arg {
            Arg::Wildcard | Arg::LatticeBind(_) => true,
            Arg::Lit(v) => *v == val,
            Arg::Var(s) => env.get(s).is_none_or(|&bound| bound == val),
        })
}

fn value_as_int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        Value::Bool(b) => i64::from(*b),
        Value::Sym(s) => i64::from(s.0),
    }
}

fn support_tuples(db: &Database, support: &[(usize, RowId)]) -> Vec<(String, Vec<Value>)> {
    support
        .iter()
        .map(|&(rel, id)| {
            let store = &db.relations[rel];
            (store.name.clone(), store.rows[id].clone())
        })
        .collect()
}

// ----- the fixed point -----------------------------------------------------

/// A rule resolved for evaluation.
struct RulePlan<'p> {
    steps: Vec<Step<'p>>,
    heads: Vec<(usize, &'p HeadAtom)>,
    /// Indices into `steps` of the positive atoms (the delta candidates).
    positives: Vec<usize>,
    /// Relations read under negation or aggregation. These are not monotone
    /// inputs, so a change to one re-fires the whole rule rather than a delta.
    guards: Vec<usize>,
}

impl<'p> RulePlan<'p> {
    fn new(db: &Database, rule: &'p Rule) -> Result<Self, EvalError> {
        let steps = plan_body(db, &rule.body)?;
        let heads = rule
            .heads
            .iter()
            .map(|h| Ok((db.id(&h.relation)?, h)))
            .collect::<Result<_, EvalError>>()?;
        let mut positives = Vec::new();
        let mut guards = Vec::new();
        for (i, step) in steps.iter().enumerate() {
            match step {
                Step::Positive { .. } => positives.push(i),
                Step::Negative { probe, .. } | Step::Aggregate { probe, .. } => {
                    guards.push(probe.rel);
                }
                Step::Condition(_) | Step::Let { .. } => {}
            }
        }
        Ok(RulePlan {
            steps,
            heads,
            positives,
            guards,
        })
    }

    fn positive_rel(&self, step: usize) -> usize {
        match &self.steps[step] {
            Step::Positive { probe, .. } => probe.rel,
            _ => unreachable!("`positives` only lists positive steps"),
        }
    }
}

/// Plans every rule and registers the indexes they probe.
fn plan_rules<'p>(program: &'p Program, db: &mut Database) -> Result<Vec<RulePlan<'p>>, EvalError> {
    let mut plans = Vec::with_capacity(program.rules.len());
    for rule in &program.rules {
        let mut plan = RulePlan::new(db, rule)?;
        attach_persistent(db, &mut plan.steps);
        plans.push(plan);
    }
    Ok(plans)
}

/// Collects a rule's head tuples. Insertion is deferred until the walk ends
/// (the walk holds the database immutably); tuples already stored, or already
/// pending, are dropped on the spot so buffering cannot outgrow the new facts.
struct Derive<'h> {
    heads: &'h [(usize, &'h HeadAtom)],
    pending: Vec<(usize, Vec<Value>)>,
    seen: HashSet<(usize, Vec<Value>)>,
}

impl<'h> Derive<'h> {
    fn new(heads: &'h [(usize, &'h HeadAtom)]) -> Self {
        Derive {
            heads,
            pending: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn flush(self, db: &mut Database) -> bool {
        let mut changed = false;
        for (rel, tuple) in self.pending {
            changed |= db.relations[rel].insert(tuple);
        }
        changed
    }
}

impl Sink for Derive<'_> {
    fn emit(
        &mut self,
        db: &Database,
        env: &Env,
        _support: &[(usize, RowId)],
    ) -> Result<(), EvalError> {
        for &(rel, head) in self.heads {
            let tuple = head
                .args
                .iter()
                .map(|arg| arg.eval(env))
                .collect::<Result<Vec<_>, _>>()?;
            if !db.relations[rel].contains(&tuple) && self.seen.insert((rel, tuple.clone())) {
                self.pending.push((rel, tuple));
            }
        }
        Ok(())
    }
}

/// Runs `program` to a fixed point over `db`.
///
/// Evaluation is **semi-naïve** and **indexed**. Each stratum iterates to a
/// fixed point, but an iteration only joins against the tuples derived by the
/// previous one: a rule with positive atoms `a₁..aₖ` fires once per atom `aᵢ`
/// whose relation grew, with `aᵢ` restricted to the new rows, the atoms before
/// it to the rows that predate them, and the atoms after it to everything —
/// so each combination of body tuples is joined exactly once. Every atom probes
/// a hash index on the columns already bound when it is reached, rather than
/// scanning its relation.
///
/// Evaluation is also **incremental**: once a run completes, the database
/// remembers where its fixed point ended, and the next run (after
/// [`Database::insert`]/[`Database::insert_fact`], or on a fork) starts from the
/// tuples added since rather than from scratch. Results are identical to
/// re-running from scratch under v1's monotone, no-retraction semantics.
///
/// # Errors
///
/// Returns [`EvalError`] on stratification failure or any evaluation error,
/// including [`EvalError::IterationLimit`] — which is indeterminate, never "no
/// more derivations".
pub fn run(program: &Program, db: &mut Database) -> Result<(), EvalError> {
    let strata = stratify(program)?;
    let plans = plan_rules(program, db)?;
    let fresh = !db.at_fixpoint;
    let base: Vec<RowId> = if fresh {
        vec![0; db.relations.len()]
    } else {
        db.relations.iter().map(|s| s.closed).collect()
    };
    for group in &strata {
        run_stratum(group, &plans, db, &base, fresh)?;
    }
    for store in &mut db.relations {
        store.closed = store.len();
    }
    db.at_fixpoint = true;
    Ok(())
}

/// Per-stratum fixed-point iteration cap. Real stratified programs converge in
/// a number of iterations bounded by the active domain; this only bounds the
/// pathological case so the engine never hangs.
const MAX_STRATUM_ITERATIONS: u32 = 1_000_000;

fn run_stratum(
    group: &[usize],
    plans: &[RulePlan<'_>],
    db: &mut Database,
    base: &[RowId],
    fresh: bool,
) -> Result<(), EvalError> {
    // Rows in `lo[r]..hi[r]` are the delta: not yet joined by this stratum.
    let mut lo = base.to_vec();
    let mut first = true;
    let mut iterations = 0_u32;
    loop {
        let hi = db.lens();
        if !first && hi == lo {
            return Ok(());
        }
        for &rule in group {
            fire(&plans[rule], db, &lo, &hi, first && fresh)?;
        }
        if !first {
            iterations += 1;
            if iterations > MAX_STRATUM_ITERATIONS {
                return Err(EvalError::IterationLimit);
            }
        }
        lo = hi;
        first = false;
    }
}

/// Fires one rule against the delta `lo..hi`, then inserts what it derived.
fn fire(
    plan: &RulePlan<'_>,
    db: &mut Database,
    lo: &[RowId],
    hi: &[RowId],
    from_scratch: bool,
) -> Result<bool, EvalError> {
    let changed = |r: usize| lo[r] < hi[r];
    let full = from_scratch || plan.guards.iter().any(|&r| changed(r));
    let mut sink = Derive::new(&plan.heads);
    {
        let mut walk = Walk {
            db: &*db,
            locals: &[],
        };
        let mut windows = vec![0..0; plan.steps.len()];
        let mut env = Env::new();
        let mut support = Vec::new();
        if full {
            // Everything up to `hi`: the first firing, or a negated/aggregated
            // input changed (not a monotone delta).
            for &i in &plan.positives {
                windows[i] = 0..hi[plan.positive_rel(i)];
            }
            walk.walk(&plan.steps, &windows, 0, &mut env, &mut support, &mut sink)?;
        } else {
            for (k, &i) in plan.positives.iter().enumerate() {
                let r = plan.positive_rel(i);
                if !changed(r) {
                    continue;
                }
                for (k2, &j) in plan.positives.iter().enumerate() {
                    let rj = plan.positive_rel(j);
                    windows[j] = match k2.cmp(&k) {
                        std::cmp::Ordering::Less => 0..lo[rj],
                        std::cmp::Ordering::Equal => lo[rj]..hi[rj],
                        std::cmp::Ordering::Greater => 0..hi[rj],
                    };
                }
                walk.walk(&plan.steps, &windows, 0, &mut env, &mut support, &mut sink)?;
            }
        }
    }
    Ok(sink.flush(db))
}

/// The reference evaluator the semi-naïve loop is differentially tested
/// against: every rule re-fired over the whole database, with no indexes,
/// until nothing changes. Never marks a fixed point, so each call starts over.
#[cfg(feature = "arbitrary")]
pub(crate) fn run_naive(program: &Program, db: &mut Database) -> Result<(), EvalError> {
    let strata = stratify(program)?;
    let plans = program
        .rules
        .iter()
        .map(|rule| RulePlan::new(db, rule))
        .collect::<Result<Vec<_>, _>>()?;
    for group in &strata {
        let mut iterations = 0_u32;
        loop {
            let mut changed = false;
            for &rule in group {
                let zeros = vec![0; db.relations.len()];
                let hi = db.lens();
                changed |= fire(&plans[rule], db, &zeros, &hi, true)?;
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
    db.at_fixpoint = false;
    Ok(())
}

// ----- provenance ----------------------------------------------------------

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

/// Matches each firing of one rule against a target tuple.
struct Explain<'t> {
    relation: &'t str,
    target: &'t [Value],
    is_lattice: bool,
    rule: usize,
    heads: &'t [HeadAtom],
    out: Vec<Derivation>,
}

impl Sink for Explain<'_> {
    fn emit(
        &mut self,
        db: &Database,
        env: &Env,
        support: &[(usize, RowId)],
    ) -> Result<(), EvalError> {
        for head in self.heads {
            if head.relation != self.relation {
                continue;
            }
            let tuple = head
                .args
                .iter()
                .map(|arg| arg.eval(env))
                .collect::<Result<Vec<_>, _>>()?;
            let matches = if self.is_lattice {
                tuple.len() == self.target.len()
                    && !tuple.is_empty()
                    && tuple[..tuple.len() - 1] == self.target[..self.target.len() - 1]
            } else {
                tuple == self.target
            };
            if matches {
                self.out.push(Derivation {
                    rule: self.rule,
                    support: support_tuples(db, support),
                });
            }
        }
        Ok(())
    }
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
    let mut locals = Vec::new();
    for (rule_idx, rule) in program.rules.iter().enumerate() {
        if !rule.heads.iter().any(|h| h.relation == relation) {
            continue;
        }
        let mut steps = plan_body(db, &rule.body)?;
        attach_transient(db, &mut steps, &mut locals);
        let windows = vec![ALL; steps.len()];
        let mut sink = Explain {
            relation,
            target: tuple,
            is_lattice,
            rule: rule_idx,
            heads: &rule.heads,
            out: Vec::new(),
        };
        Walk {
            db,
            locals: &locals,
        }
        .walk(
            &steps,
            &windows,
            0,
            &mut Env::new(),
            &mut Vec::new(),
            &mut sink,
        )?;
        out.append(&mut sink.out);
    }
    Ok(out)
}

// ----- the queries grain ---------------------------------------------------

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

/// Collects every solution, up to the cardinality cap.
struct Collect<'o> {
    outputs: &'o [Expr],
    max: usize,
    truncated: bool,
    out: Vec<QuerySolution>,
}

impl Sink for Collect<'_> {
    fn stop(&self) -> bool {
        self.truncated
    }

    fn emit(
        &mut self,
        db: &Database,
        env: &Env,
        support: &[(usize, RowId)],
    ) -> Result<(), EvalError> {
        if self.out.len() >= self.max {
            self.truncated = true;
            return Ok(());
        }
        let tuple = self
            .outputs
            .iter()
            .map(|expr| expr.eval(env))
            .collect::<Result<Vec<_>, _>>()?;
        self.out.push(QuerySolution {
            tuple,
            support: support_tuples(db, support),
        });
        Ok(())
    }
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
/// state a host keeps over it. Atoms probe the program's indexes where one fits;
/// any other bound-column pattern gets a transient index for this call only.
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
    outputs: &[Expr],
    body: &[BodyClause],
    max: usize,
) -> Result<QueryOutput, EvalError> {
    let mut steps = plan_body(db, body)?;
    let mut locals = Vec::new();
    attach_transient(db, &mut steps, &mut locals);
    let windows = vec![ALL; steps.len()];
    let mut sink = Collect {
        outputs,
        max,
        truncated: false,
        out: Vec::new(),
    };
    Walk {
        db,
        locals: &locals,
    }
    .walk(
        &steps,
        &windows,
        0,
        &mut Env::new(),
        &mut Vec::new(),
        &mut sink,
    )?;
    Ok(QueryOutput {
        solutions: sink.out,
        truncated: sink.truncated,
    })
}
