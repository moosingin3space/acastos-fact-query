//! Bottom-up evaluation: stratification plus a semi-naïve, indexed fixed-point
//! loop over a relational database.
//!
//! The relational core is an interpreter (a nested-loop walk of the rule
//! bodies); `if`/`let`/head expressions are evaluated by [`Expr::eval`]. See
//! [`run`] for the evaluation strategy and ADR 0009 for why it is shaped this
//! way.

use std::collections::HashMap;
use std::ops::Range;

use crate::compile::{Code, ColOp, Scope, SlotId, match_ops, match_row, negated_ops};
use crate::expr::{Expr, ExprError};
use crate::ir::{AggFunc, Arg, Atom, BodyClause, Program, RelationDecl, RelationKind, Rule};
use crate::stack;
use crate::table::{BitSet, IdMap, KeyTable, hash_values, row_at};
use crate::value::{Type, Value};

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

/// A row id as the tables store it. Rows are bounded far below `u32::MAX` by
/// memory long before this could truncate.
#[inline]
fn rid(id: RowId) -> u32 {
    u32::try_from(id).unwrap_or(u32::MAX - 1)
}

/// A hash index over a fixed set of key columns: key → the ids of the rows
/// carrying it. Row ids are pushed in append order, so every posting list is
/// sorted — which is what lets a lookup be restricted to a delta window with
/// two binary searches. Keys are not stored: the table compares against rows.
#[derive(Debug, Clone)]
struct Index {
    table: KeyTable,
    postings: Vec<Vec<u32>>,
}

impl Index {
    fn build(cols: Vec<usize>, store: &RelationStore) -> Self {
        let mut index = Index {
            table: KeyTable::new(cols),
            postings: Vec::new(),
        };
        for id in 0..store.len {
            index.add(id, &store.data, store.arity());
        }
        index
    }

    fn cols(&self) -> &[usize] {
        self.table.cols()
    }

    fn add(&mut self, id: RowId, data: &[Value], arity: usize) {
        let row = row_at(data, arity, rid(id));
        let hash = self.table.hash_row(row);
        if let Some(g) = self.table.find_row(row, hash, data, arity) {
            self.postings[g as usize].push(rid(id));
        } else {
            self.table.insert(hash, rid(id));
            self.postings.push(vec![rid(id)]);
        }
    }

    fn get(&self, key: &[Value], data: &[Value], arity: usize) -> &[u32] {
        let hash = hash_values(key.iter().copied());
        self.table
            .find(key, hash, data, arity)
            .map_or(&[], |g| self.postings[g as usize].as_slice())
    }
}

/// The stored contents of a single relation.
///
/// Tuples live once, in a flat append-only row log (`data`, row-major with
/// arity stride), so "the tuples derived since point *p*" is just the row range
/// `p..`: semi-naïve deltas are ranges, not copies. Deduplication (for a
/// lattice: the key columns) and every index are open-addressing key tables
/// over that log, which compare against stored rows rather than copying keys.
/// A lattice tuple superseded by a joined value is tombstoned (its `live` bit
/// cleared) and the joined tuple appended; every reader skips dead rows.
#[derive(Debug, Clone)]
pub struct RelationStore {
    name: String,
    kind: RelationKind,
    /// The declared column types, in order. Its length is the relation's arity.
    schema: Vec<Type>,
    data: Vec<Value>,
    /// The number of rows (live or dead) in `data`.
    len: usize,
    live: BitSet,
    /// Set relations: every column. Lattice relations: the key columns, each
    /// group pointing at that key's live row.
    keys: KeyTable,
    indexes: Vec<Index>,
    /// Rows below this mark were closed under the rules at the end of the last
    /// successful [`run`]; meaningful only while [`Database::at_fixpoint`].
    closed: usize,
}

impl RelationStore {
    fn new(decl: &RelationDecl) -> Self {
        let arity = decl.schema.len();
        let key_cols = match decl.kind {
            RelationKind::Relation => (0..arity).collect(),
            RelationKind::Lattice(_) => (0..arity.saturating_sub(1)).collect(),
        };
        RelationStore {
            name: decl.name.clone(),
            kind: decl.kind,
            schema: decl.schema.clone(),
            data: Vec::new(),
            len: 0,
            live: BitSet::default(),
            keys: KeyTable::new(key_cols),
            indexes: Vec::new(),
            closed: 0,
        }
    }

    fn arity(&self) -> usize {
        self.schema.len()
    }

    fn len(&self) -> usize {
        self.len
    }

    /// Row `id` (live or dead).
    #[inline]
    fn row(&self, id: RowId) -> &[Value] {
        row_at(&self.data, self.arity(), rid(id))
    }

    /// Inserts a tuple, applying lattice join for lattice relations. Returns
    /// whether the relation changed.
    fn insert(&mut self, tuple: &[Value]) -> bool {
        let hash = self.keys.hash_row(tuple);
        let found = self.keys.find_row(tuple, hash, &self.data, self.arity());
        match (self.kind, found) {
            (RelationKind::Relation, Some(_)) => false,
            (RelationKind::Relation | RelationKind::Lattice(_), None) => {
                self.data.extend_from_slice(tuple);
                let id = self.push_row();
                self.keys.insert(hash, rid(id));
                true
            }
            (RelationKind::Lattice(kind), Some(g)) => {
                let split = self.arity() - 1;
                let old = self.keys.rep(g) as usize;
                let existing = self.row(old)[split];
                let joined = kind.join(existing, tuple[split]);
                if joined == existing {
                    return false;
                }
                self.live.clear(old);
                self.data.extend_from_slice(&tuple[..split]);
                self.data.push(joined);
                let id = self.push_row();
                self.keys.set_rep(g, rid(id));
                true
            }
        }
    }

    /// Registers the row just appended to `data`, returning its id.
    fn push_row(&mut self) -> RowId {
        let id = self.len;
        self.live.push_set(id);
        self.len += 1;
        let arity = self.arity();
        for index in &mut self.indexes {
            index.add(id, &self.data, arity);
        }
        id
    }

    /// Whether `tuple` is currently stored (for a lattice: with exactly this
    /// joined value).
    fn contains(&self, tuple: &[Value]) -> bool {
        let hash = self.keys.hash_row(tuple);
        let Some(g) = self.keys.find_row(tuple, hash, &self.data, self.arity()) else {
            return false;
        };
        match self.kind {
            RelationKind::Relation => true,
            RelationKind::Lattice(_) => {
                let split = self.arity() - 1;
                self.row(self.keys.rep(g) as usize)[split] == tuple[split]
            }
        }
    }

    fn live_rows(&self) -> impl Iterator<Item = &[Value]> {
        (0..self.len)
            .filter(|&id| self.live.get(id))
            .map(|id| self.row(id))
    }

    fn index_of(&self, cols: &[usize]) -> Option<usize> {
        self.indexes.iter().position(|i| i.cols() == cols)
    }

    fn ensure_index(&mut self, cols: &[usize]) -> usize {
        if let Some(i) = self.index_of(cols) {
            return i;
        }
        let index = Index::build(cols.to_vec(), self);
        self.indexes.push(index);
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
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public API takes the tuple by value; storage copies it into the flat row log"
    )]
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
        Ok(store.insert(&tuple))
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
            .map(|s| s.live_rows().map(<[Value]>::to_vec).collect())
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
            .filter(|&id| fork.live.get(id) && !before.contains(fork.row(id)))
            .map(|id| fork.row(id).to_vec())
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
    Var(SlotId),
}

/// Which index (if any) an atom probes.
#[derive(Debug, Clone, Copy)]
enum Access {
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
    access: Access,
}

impl Probe {
    fn new(db: &Database, atom: &Atom, scope: &Scope, negated: bool) -> Result<Self, EvalError> {
        let rel = db.id(&atom.relation)?;
        let mut key = Vec::new();
        for (col, arg) in atom.args.iter().enumerate() {
            match arg {
                Arg::Lit(v) => key.push((col, KeyPart::Lit(*v))),
                // A negated `?x` matches anything, so it is only a key column
                // in a positive atom.
                Arg::Var(s) => {
                    if let Some(slot) = scope.get(*s) {
                        key.push((col, KeyPart::Var(slot)));
                    }
                }
                Arg::LatticeBind(s) if !negated => {
                    if let Some(slot) = scope.get(*s) {
                        key.push((col, KeyPart::Var(slot)));
                    }
                }
                Arg::LatticeBind(_) | Arg::Wildcard => {}
            }
        }
        Ok(Probe {
            rel,
            key,
            access: Access::Scan,
        })
    }

    fn cols(&self) -> Vec<usize> {
        self.key.iter().map(|&(c, _)| c).collect()
    }

    /// Writes the key values under `env` into `out`.
    fn key_into(&self, env: &[Value], out: &mut Vec<Value>) {
        out.clear();
        out.extend(self.key.iter().map(|&(_, part)| match part {
            KeyPart::Lit(v) => v,
            KeyPart::Var(slot) => env[slot],
        }));
    }
}

/// One body clause, compiled against a database.
#[derive(Debug)]
enum Step {
    Positive {
        probe: Probe,
        ops: Vec<ColOp>,
    },
    Negative {
        probe: Probe,
        ops: Vec<ColOp>,
    },
    Condition(Code),
    Let {
        slot: SlotId,
        code: Code,
    },
    Aggregate {
        probe: Probe,
        ops: Vec<ColOp>,
        func: AggFunc,
        arg: Option<Code>,
        output: SlotId,
    },
}

impl Step {
    fn probe_mut(&mut self) -> Option<&mut Probe> {
        match self {
            Step::Positive { probe, .. }
            | Step::Negative { probe, .. }
            | Step::Aggregate { probe, .. } => Some(probe),
            Step::Condition(_) | Step::Let { .. } => None,
        }
    }
}

/// A body compiled left to right: its steps, the variables bound at its end
/// (for compiling heads or query outputs), and its environment size.
struct Body {
    steps: Vec<Step>,
    scope: Scope,
    slots: usize,
}

/// Compiles a body, tracking which variables are bound at each clause so every
/// atom knows its key columns and every variable use resolves to a slot.
/// Probes start as [`Access::Scan`]; an `attach_*` pass assigns indexes.
fn plan_body(db: &Database, body: &[BodyClause]) -> Result<Body, EvalError> {
    let mut scope = Scope::default();
    let mut next: SlotId = 0;
    let mut steps = Vec::with_capacity(body.len());
    for clause in body {
        steps.push(match clause {
            BodyClause::Positive(atom) => {
                let probe = Probe::new(db, atom, &scope, false)?;
                let ops = match_ops(&atom.args, &mut scope, &mut next);
                Step::Positive { probe, ops }
            }
            BodyClause::Negative(atom) => Step::Negative {
                probe: Probe::new(db, atom, &scope, true)?,
                ops: negated_ops(&atom.args, &scope),
            },
            BodyClause::Condition(expr) => Step::Condition(Code::compile(expr, &scope)?),
            BodyClause::Let { var, expr } => {
                let code = Code::compile(expr, &scope)?;
                let slot = scope.bind_fresh(*var, &mut next);
                Step::Let { slot, code }
            }
            BodyClause::Aggregate(agg) => {
                let probe = Probe::new(db, &agg.source, &scope, false)?;
                // Source-local variables are visible to the argument only.
                let mut inner = scope.clone();
                let ops = match_ops(&agg.source.args, &mut inner, &mut next);
                let arg = agg
                    .arg
                    .as_ref()
                    .map(|e| Code::compile(e, &inner))
                    .transpose()?;
                let output = scope.bind_fresh(agg.output, &mut next);
                Step::Aggregate {
                    probe,
                    ops,
                    func: agg.func,
                    arg,
                    output,
                }
            }
        });
    }
    Ok(Body {
        steps,
        scope,
        slots: next,
    })
}

/// Points every keyed probe at a persistent index, building any that are
/// missing. Indexes are maintained on insert from then on.
fn attach_persistent(db: &mut Database, steps: &mut [Step]) {
    for probe in steps.iter_mut().filter_map(Step::probe_mut) {
        if !probe.key.is_empty() {
            let cols = probe.cols();
            probe.access = Access::Db(db.relations[probe.rel].ensure_index(&cols));
        }
    }
}

/// Read-only variant of [`attach_persistent`]: reuses a persistent index when
/// one exists, otherwise builds a transient one into `locals` (keyed by
/// relation and columns). This keeps the read-only entry points
/// ([`eval_query`], [`explain`]) literally unable to mutate the database, at
/// the cost of one linear build per access path the program did not declare.
fn attach_transient(db: &Database, steps: &mut [Step], locals: &mut Vec<(usize, Index)>) {
    for probe in steps.iter_mut().filter_map(Step::probe_mut) {
        if probe.key.is_empty() {
            continue;
        }
        let cols = probe.cols();
        let store = &db.relations[probe.rel];
        probe.access = if let Some(i) = store.index_of(&cols) {
            Access::Db(i)
        } else if let Some(j) = locals
            .iter()
            .position(|(rel, ix)| *rel == probe.rel && ix.cols() == cols)
        {
            Access::Local(j)
        } else {
            locals.push((probe.rel, Index::build(cols, store)));
            Access::Local(locals.len() - 1)
        };
    }
}

// ----- traversal -----------------------------------------------------------

/// The live row ids a probe yields: either a range scan or an index posting
/// list, both already restricted to the visit's row window.
enum Rows<'d> {
    Scan {
        live: &'d BitSet,
        next: RowId,
        end: RowId,
    },
    Ids {
        live: &'d BitSet,
        ids: std::slice::Iter<'d, u32>,
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
                    if live.get(id) {
                        return Some(id);
                    }
                }
                None
            }
            Rows::Ids { live, ids } => ids.by_ref().map(|&id| id as usize).find(|&id| live.get(id)),
        }
    }
}

/// The whole relation, whatever its current length.
const ALL: Range<RowId> = 0..RowId::MAX;

/// How many body levels the walk descends between stack checks. A check costs
/// a few nanoseconds; eight frames are far inside its red zone.
const GUARD_EVERY: usize = 8;

/// Receives each satisfying assignment of a body.
trait Sink {
    /// True once the sink wants no more solutions (the query cap).
    fn stop(&self) -> bool {
        false
    }

    /// Called once per satisfying assignment, with the environment, a scratch
    /// stack for evaluating code, and the matched positive rows.
    fn emit(
        &mut self,
        db: &Database,
        env: &[Value],
        stack: &mut Vec<Value>,
        support: &[(usize, RowId)],
    ) -> Result<(), EvalError>;
}

/// The single nested-loop join every entry point shares: [`run`] (deriving),
/// [`explain`] (matching one target), and [`eval_query`] (collecting). Reads
/// never mutate the database, so rows are borrowed, never cloned, and the
/// scratch buffers are reused across every row.
struct Walk<'a> {
    db: &'a Database,
    locals: &'a [(usize, Index)],
    /// Scratch for expression evaluation.
    stack: Vec<Value>,
    /// Scratch for building index keys.
    key: Vec<Value>,
}

impl<'a> Walk<'a> {
    fn new(db: &'a Database, locals: &'a [(usize, Index)]) -> Self {
        Walk {
            db,
            locals,
            stack: Vec::new(),
            key: Vec::new(),
        }
    }

    fn rows(&mut self, probe: &Probe, env: &[Value], window: &Range<RowId>) -> Rows<'a> {
        let db: &'a Database = self.db;
        let store = &db.relations[probe.rel];
        let end = window.end.min(store.len());
        let start = window.start.min(end);
        let index = match probe.access {
            Access::Scan => None,
            Access::Db(i) => Some(&store.indexes[i]),
            Access::Local(i) => Some(&self.locals[i].1),
        };
        if let Some(index) = index {
            probe.key_into(env, &mut self.key);
            let ids = index.get(&self.key, &store.data, store.arity());
            let lo = ids.partition_point(|&id| (id as usize) < start);
            let hi = ids.partition_point(|&id| (id as usize) < end);
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
    /// every [`GUARD_EVERY`]th level runs under [`stack::guard`].
    fn walk<S: Sink>(
        &mut self,
        steps: &[Step],
        windows: &[Range<RowId>],
        idx: usize,
        env: &mut [Value],
        support: &mut Vec<(usize, RowId)>,
        sink: &mut S,
    ) -> Result<(), EvalError> {
        if idx.is_multiple_of(GUARD_EVERY) {
            stack::guard(|| self.walk_step(steps, windows, idx, env, support, sink))
        } else {
            self.walk_step(steps, windows, idx, env, support, sink)
        }
    }

    fn walk_step<S: Sink>(
        &mut self,
        steps: &[Step],
        windows: &[Range<RowId>],
        idx: usize,
        env: &mut [Value],
        support: &mut Vec<(usize, RowId)>,
        sink: &mut S,
    ) -> Result<(), EvalError> {
        if sink.stop() {
            return Ok(());
        }
        let Some(step) = steps.get(idx) else {
            return sink.emit(self.db, env, &mut self.stack, support);
        };
        match step {
            Step::Positive { probe, ops } => {
                let db: &'a Database = self.db;
                let store = &db.relations[probe.rel];
                for id in self.rows(probe, env, &windows[idx]) {
                    if match_row(ops, store.row(id), env) {
                        support.push((probe.rel, id));
                        let result = self.walk(steps, windows, idx + 1, env, support, sink);
                        support.pop();
                        result?;
                    }
                }
                Ok(())
            }
            Step::Negative { probe, ops } => {
                let db: &'a Database = self.db;
                let store = &db.relations[probe.rel];
                let mut found = false;
                for id in self.rows(probe, env, &ALL) {
                    // Negated ops never write, so `env` is untouched.
                    if match_row(ops, store.row(id), env) {
                        found = true;
                        break;
                    }
                }
                if !found {
                    self.walk(steps, windows, idx + 1, env, support, sink)?;
                }
                Ok(())
            }
            Step::Condition(code) => {
                if code.run(env, &mut self.stack)? == Value::Bool(true) {
                    self.walk(steps, windows, idx + 1, env, support, sink)?;
                }
                Ok(())
            }
            Step::Let { slot, code } => {
                env[*slot] = code.run(env, &mut self.stack)?;
                self.walk(steps, windows, idx + 1, env, support, sink)
            }
            Step::Aggregate {
                probe,
                ops,
                func,
                arg,
                output,
            } => {
                if let Some(result) = self.aggregate(probe, ops, *func, arg.as_ref(), env)? {
                    env[*output] = result;
                    self.walk(steps, windows, idx + 1, env, support, sink)?;
                }
                Ok(())
            }
        }
    }

    fn aggregate(
        &mut self,
        probe: &Probe,
        ops: &[ColOp],
        func: AggFunc,
        arg: Option<&Code>,
        env: &mut [Value],
    ) -> Result<Option<Value>, EvalError> {
        let db: &'a Database = self.db;
        let store = &db.relations[probe.rel];
        let mut count: i64 = 0;
        let mut acc: Option<i64> = None;
        for id in self.rows(probe, env, &ALL) {
            if !match_row(ops, store.row(id), env) {
                continue;
            }
            count += 1;
            if let Some(code) = arg {
                let v = value_as_int(&code.run(env, &mut self.stack)?);
                acc = Some(match (func, acc) {
                    (_, None) => v,
                    (AggFunc::Sum, Some(a)) => a.wrapping_add(v),
                    (AggFunc::Min, Some(a)) => a.min(v),
                    (AggFunc::Max, Some(a)) => a.max(v),
                    (AggFunc::Count, Some(a)) => a,
                });
            }
        }
        Ok(match func {
            AggFunc::Count => Some(Value::Int(count)),
            AggFunc::Sum => Some(Value::Int(acc.unwrap_or(0))),
            AggFunc::Min | AggFunc::Max => acc.map(Value::Int),
        })
    }
}

fn value_as_int(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        Value::Bool(b) => i64::from(*b),
        Value::Sym(s) => i64::from(s.0),
    }
}

/// Evaluates each of `codes` into `out`.
fn eval_into(
    codes: &[Code],
    env: &[Value],
    stack: &mut Vec<Value>,
    out: &mut Vec<Value>,
) -> Result<(), EvalError> {
    out.clear();
    for code in codes {
        out.push(code.run(env, stack)?);
    }
    Ok(())
}

// ----- the fixed point -----------------------------------------------------

/// A rule compiled for evaluation.
struct RulePlan {
    steps: Vec<Step>,
    /// Each head's relation and compiled column expressions.
    heads: Vec<(usize, Vec<Code>)>,
    slots: usize,
    /// Indices into `steps` of the positive atoms (the delta candidates).
    positives: Vec<usize>,
    /// Relations read under negation or aggregation. These are not monotone
    /// inputs, so a change to one re-fires the whole rule rather than a delta.
    guards: Vec<usize>,
}

impl RulePlan {
    fn new(db: &Database, rule: &Rule) -> Result<Self, EvalError> {
        let body = plan_body(db, &rule.body)?;
        let heads = rule
            .heads
            .iter()
            .map(|h| {
                let codes = h
                    .args
                    .iter()
                    .map(|e| Code::compile(e, &body.scope))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((db.id(&h.relation)?, codes))
            })
            .collect::<Result<_, EvalError>>()?;
        let mut positives = Vec::new();
        let mut guards = Vec::new();
        for (i, step) in body.steps.iter().enumerate() {
            match step {
                Step::Positive { .. } => positives.push(i),
                Step::Negative { probe, .. } | Step::Aggregate { probe, .. } => {
                    guards.push(probe.rel);
                }
                Step::Condition(_) | Step::Let { .. } => {}
            }
        }
        Ok(RulePlan {
            steps: body.steps,
            heads,
            slots: body.slots,
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
fn plan_rules(program: &Program, db: &mut Database) -> Result<Vec<RulePlan>, EvalError> {
    let mut plans = Vec::with_capacity(program.rules.len());
    for rule in &program.rules {
        let mut plan = RulePlan::new(db, rule)?;
        attach_persistent(db, &mut plan.steps);
        plans.push(plan);
    }
    Ok(plans)
}

/// Pending buffers below this size are never compacted.
const COMPACT_MIN: usize = 1024;

/// Collects a rule's head tuples. Insertion is deferred until the walk ends
/// (the walk holds the database immutably). Tuples already stored are dropped
/// on the spot; duplicates *within* one firing (a projection like
/// `a(1) <-- b(x), c(y)`) are removed by sorting and deduplicating the buffer
/// whenever it doubles, which keeps it within twice the number of distinct new
/// tuples at amortised `O(n log n)`, deterministically.
struct Derive<'h> {
    heads: &'h [(usize, Vec<Code>)],
    pending: Vec<(usize, Vec<Value>)>,
    compact_at: usize,
    scratch: Vec<Value>,
}

impl<'h> Derive<'h> {
    fn new(heads: &'h [(usize, Vec<Code>)]) -> Self {
        Derive {
            heads,
            pending: Vec::new(),
            compact_at: COMPACT_MIN,
            scratch: Vec::new(),
        }
    }

    fn flush(self, db: &mut Database) -> bool {
        let mut changed = false;
        for (rel, tuple) in self.pending {
            changed |= db.relations[rel].insert(&tuple);
        }
        changed
    }
}

impl Sink for Derive<'_> {
    fn emit(
        &mut self,
        db: &Database,
        env: &[Value],
        stack: &mut Vec<Value>,
        _support: &[(usize, RowId)],
    ) -> Result<(), EvalError> {
        for (rel, codes) in self.heads {
            eval_into(codes, env, stack, &mut self.scratch)?;
            if !db.relations[*rel].contains(&self.scratch) {
                self.pending.push((*rel, self.scratch.clone()));
            }
        }
        if self.pending.len() >= self.compact_at {
            self.pending.sort_unstable();
            self.pending.dedup();
            self.compact_at = (self.pending.len() * 2).max(COMPACT_MIN);
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
/// scanning its relation. Rule bodies are compiled first: variables live in
/// dense slots, row matching is a list of per-column operations, and
/// expressions run as bytecode.
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
    plans: &[RulePlan],
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
    plan: &RulePlan,
    db: &mut Database,
    lo: &[RowId],
    hi: &[RowId],
    from_scratch: bool,
) -> Result<bool, EvalError> {
    let changed = |r: usize| lo[r] < hi[r];
    let full = from_scratch || plan.guards.iter().any(|&r| changed(r));
    let mut sink = Derive::new(&plan.heads);
    {
        let mut walk = Walk::new(db, &[]);
        let mut windows = vec![0..0; plan.steps.len()];
        let mut env = vec![Value::Int(0); plan.slots];
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

/// An opaque handle to one stored fact: a relation and a row in its log.
///
/// Rows are only ever appended (a lattice tuple superseded by a joined value is
/// marked dead, never moved or reused), so a handle stays valid, and keeps
/// naming the same tuple, for the lifetime of the database it came from. It is
/// meaningless for any other database, including a speculative fork's extra
/// rows. Resolve one with [`crate::Engine::fact`]; chain explanations with
/// [`crate::Engine::explain_fact`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FactRef {
    relation: u32,
    row: u32,
}

impl FactRef {
    fn new(relation: usize, row: RowId) -> Self {
        FactRef {
            relation: rid(relation),
            row: rid(row),
        }
    }

    fn key(self) -> u64 {
        (u64::from(self.relation) << 32) | u64::from(self.row)
    }
}

impl Database {
    /// Resolves a [`FactRef`] to its relation name and tuple, or `None` if the
    /// handle does not name a row of this database. A lattice row superseded
    /// since the handle was issued still resolves: it is the tuple that
    /// participated.
    #[must_use]
    pub fn fact(&self, fact: FactRef) -> Option<(&str, &[Value])> {
        let store = self.relations.get(fact.relation as usize)?;
        let row = fact.row as usize;
        (row < store.len()).then(|| (store.name.as_str(), store.row(row)))
    }
}

/// A single one-step justification for a tuple: the rule that fired and the
/// positive body facts (its support) that satisfied it in the materialized
/// database, in body order. Each support fact is a handle, so explanations can
/// be chained by explaining a support fact in turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derivation {
    /// Index into [`Program::rules`] of the rule that fired.
    pub rule: usize,
    /// The positive body facts that justified the firing, in body order.
    pub support: Vec<FactRef>,
}

fn handles(support: &[(usize, RowId)]) -> Vec<FactRef> {
    support
        .iter()
        .map(|&(rel, id)| FactRef::new(rel, id))
        .collect()
}

/// Matches each firing of one rule against a target tuple.
struct Explain<'t> {
    relation: usize,
    target: &'t [Value],
    is_lattice: bool,
    rule: usize,
    heads: &'t [(usize, Vec<Code>)],
    scratch: Vec<Value>,
    out: Vec<Derivation>,
}

impl Sink for Explain<'_> {
    fn emit(
        &mut self,
        _db: &Database,
        env: &[Value],
        stack: &mut Vec<Value>,
        support: &[(usize, RowId)],
    ) -> Result<(), EvalError> {
        for (rel, codes) in self.heads {
            if *rel != self.relation {
                continue;
            }
            eval_into(codes, env, stack, &mut self.scratch)?;
            let tuple = &self.scratch;
            let matches = if self.is_lattice {
                tuple.len() == self.target.len()
                    && !tuple.is_empty()
                    && tuple[..tuple.len() - 1] == self.target[..self.target.len() - 1]
            } else {
                *tuple == self.target
            };
            if matches {
                self.out.push(Derivation {
                    rule: self.rule,
                    support: handles(support),
                });
            }
        }
        Ok(())
    }
}

/// Finds one-step derivations of `tuple` in `relation` against the materialized
/// `db`: every rule whose head produces `tuple`, paired with the positive body
/// facts that justify it. Justifications are recomputed on demand, so there is
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
    let target_rel = db.id(relation)?;
    let mut out = Vec::new();
    let mut locals = Vec::new();
    for (rule_idx, rule) in program.rules.iter().enumerate() {
        if !rule.heads.iter().any(|h| h.relation == relation) {
            continue;
        }
        let mut plan = RulePlan::new(db, rule)?;
        attach_transient(db, &mut plan.steps, &mut locals);
        let windows = vec![ALL; plan.steps.len()];
        let mut sink = Explain {
            relation: target_rel,
            target: tuple,
            is_lattice,
            rule: rule_idx,
            heads: &plan.heads,
            scratch: Vec::new(),
            out: Vec::new(),
        };
        let mut env = vec![Value::Int(0); plan.slots];
        Walk::new(db, &locals).walk(
            &plan.steps,
            &windows,
            0,
            &mut env,
            &mut Vec::new(),
            &mut sink,
        )?;
        out.append(&mut sink.out);
    }
    Ok(out)
}

// ----- the queries grain ---------------------------------------------------

/// The two independent caps on an ad-hoc query.
///
/// Rows and evidence are bounded separately, so a caller can tell "these are
/// not all the answers" apart from "these are all the answers, but not all the
/// reasons". Together they bound the output's size: at most `max_rows` rows,
/// each with at most `max_justifications_per_row` justifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryLimits {
    /// At most this many distinct result rows.
    pub max_rows: usize,
    /// At most this many justifications (join combinations) kept per row.
    pub max_justifications_per_row: usize,
}

/// One justification's support: an index into [`QueryOutput::facts`] per
/// positive body atom, in body order (see [`QueryOutput::atoms`]).
pub type Support<'a> = &'a [u32];

/// The outcome of an ad-hoc conjunctive query: its distinct rows (first-seen
/// order) and their provenance, in a flat, normalized form.
///
/// Every justification has exactly one supporting fact per positive body
/// atom, so the relation of support column `i` is [`QueryOutput::atoms`]`[i]`
/// for every justification. Supporting facts are stored once, as handles, in
/// [`QueryOutput::facts`]; a justification refers to them by index.
///
/// Both truncation flags mean *may be incomplete*; `false` means complete. If
/// the row cap stops evaluation, both are set, because the evidence for the
/// rows already found was not fully enumerated either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOutput {
    arity: usize,
    len: usize,
    rows: Vec<Value>,
    atoms: Vec<String>,
    facts: Vec<FactRef>,
    /// Row `r`'s justifications are `offsets[r]..offsets[r + 1]`.
    offsets: Vec<u32>,
    /// Justification `j`'s support is `support[j * k..(j + 1) * k]`, `k = atoms.len()`.
    support: Vec<u32>,
    rows_truncated: bool,
    evidence_truncated: bool,
}

impl QueryOutput {
    /// The number of distinct result rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether there are no result rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Result row `i`, if it exists.
    #[must_use]
    pub fn row(&self, i: usize) -> Option<&[Value]> {
        (i < self.len).then(|| &self.rows[i * self.arity..(i + 1) * self.arity])
    }

    /// Every result row, in first-seen order.
    #[must_use = "the iterator borrows the rows; iterate it"]
    pub fn rows(&self) -> impl ExactSizeIterator<Item = &[Value]> {
        (0..self.len).map(|i| &self.rows[i * self.arity..(i + 1) * self.arity])
    }

    /// The relation of each support column: one entry per positive body atom,
    /// in body order.
    #[must_use]
    pub fn atoms(&self) -> &[String] {
        &self.atoms
    }

    /// Every distinct supporting fact, each once. Resolve with
    /// [`crate::Engine::fact`].
    #[must_use]
    pub fn facts(&self) -> &[FactRef] {
        &self.facts
    }

    /// Row `i`'s justifications, each a [`Support`] (indices into
    /// [`QueryOutput::facts`], one per atom). Empty if `i` is out of range.
    #[must_use = "the iterator borrows the justifications; iterate it"]
    pub fn justifications(&self, i: usize) -> impl ExactSizeIterator<Item = Support<'_>> {
        let k = self.atoms.len();
        let (start, end) = if i < self.len {
            (self.offsets[i] as usize, self.offsets[i + 1] as usize)
        } else {
            (0, 0)
        };
        (start..end).map(move |j| &self.support[j * k..(j + 1) * k])
    }

    /// True if there may be result rows that were not returned (the row cap was
    /// reached). A first-class outcome, never an error.
    #[must_use]
    pub fn rows_truncated(&self) -> bool {
        self.rows_truncated
    }

    /// True if some returned row may have justifications that were not returned
    /// (the per-row cap was reached, or the row cap stopped evaluation).
    #[must_use]
    pub fn evidence_truncated(&self) -> bool {
        self.evidence_truncated
    }
}

/// Groups solutions into distinct rows while collecting their provenance,
/// applying both caps as it goes.
struct Collect {
    outputs: Vec<Code>,
    arity: usize,
    limits: QueryLimits,
    rows: Vec<Value>,
    row_keys: KeyTable,
    justifications: Vec<u32>,
    /// Per kept justification, in solution order: its row, and its support.
    sol_rows: Vec<u32>,
    sol_support: Vec<u32>,
    fact_ids: IdMap,
    facts: Vec<FactRef>,
    scratch: Vec<Value>,
    rows_truncated: bool,
    evidence_truncated: bool,
}

impl Collect {
    fn new(outputs: Vec<Code>, limits: QueryLimits) -> Self {
        let arity = outputs.len();
        Collect {
            outputs,
            arity,
            limits,
            rows: Vec::new(),
            row_keys: KeyTable::new((0..arity).collect()),
            justifications: Vec::new(),
            sol_rows: Vec::new(),
            sol_support: Vec::new(),
            fact_ids: IdMap::default(),
            facts: Vec::new(),
            scratch: Vec::new(),
            rows_truncated: false,
            evidence_truncated: false,
        }
    }

    /// Lays the kept justifications out row by row (a stable counting sort,
    /// so each row's justifications stay in solution order).
    fn finish(self, atoms: Vec<String>) -> QueryOutput {
        let k = atoms.len();
        let len = self.justifications.len();
        let mut offsets = Vec::with_capacity(len + 1);
        let mut total = 0_u32;
        offsets.push(0);
        for &n in &self.justifications {
            total += n;
            offsets.push(total);
        }
        let mut cursor: Vec<u32> = offsets[..len].to_vec();
        let mut support = vec![0; self.sol_support.len()];
        for (s, &r) in self.sol_rows.iter().enumerate() {
            let j = cursor[r as usize] as usize;
            cursor[r as usize] += 1;
            support[j * k..(j + 1) * k].copy_from_slice(&self.sol_support[s * k..(s + 1) * k]);
        }
        QueryOutput {
            arity: self.arity,
            len,
            rows: self.rows,
            atoms,
            facts: self.facts,
            offsets,
            support,
            rows_truncated: self.rows_truncated,
            evidence_truncated: self.evidence_truncated,
        }
    }
}

impl Sink for Collect {
    fn stop(&self) -> bool {
        self.rows_truncated
    }

    fn emit(
        &mut self,
        _db: &Database,
        env: &[Value],
        stack: &mut Vec<Value>,
        support: &[(usize, RowId)],
    ) -> Result<(), EvalError> {
        eval_into(&self.outputs, env, stack, &mut self.scratch)?;
        let hash = hash_values(self.scratch.iter().copied());
        let row = if let Some(row) = self
            .row_keys
            .find(&self.scratch, hash, &self.rows, self.arity)
        {
            row as usize
        } else {
            let row = self.justifications.len();
            if row >= self.limits.max_rows {
                self.rows_truncated = true;
                self.evidence_truncated = true;
                return Ok(());
            }
            self.rows.extend_from_slice(&self.scratch);
            self.row_keys.insert(hash, rid(row));
            self.justifications.push(0);
            row
        };
        if self.justifications[row] as usize >= self.limits.max_justifications_per_row {
            self.evidence_truncated = true;
            return Ok(());
        }
        self.justifications[row] += 1;
        self.sol_rows.push(rid(row));
        for &(rel, id) in support {
            let fact = FactRef::new(rel, id);
            let (fid, new) = self.fact_ids.get_or_insert(fact.key());
            if new {
                self.facts.push(fact);
            }
            self.sol_support.push(fid);
        }
        Ok(())
    }
}

/// Evaluates an ad-hoc conjunctive query — `(outputs) <-- body` — read-only
/// against the current materialized `db`, returning its distinct rows and their
/// provenance under the two independent caps in `limits`.
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
/// the only blast radius is space. The output holds at most
/// `limits.max_rows` rows with at most `limits.max_justifications_per_row`
/// justifications each; reaching either cap sets the corresponding flag
/// ([`QueryOutput::rows_truncated`], [`QueryOutput::evidence_truncated`]) rather
/// than allowing an unfiltered (cartesian-product) join to exhaust memory.
/// Reaching the row cap also stops evaluation.
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
    limits: QueryLimits,
) -> Result<QueryOutput, EvalError> {
    let mut body = plan_body(db, body)?;
    let outputs = outputs
        .iter()
        .map(|e| Code::compile(e, &body.scope))
        .collect::<Result<Vec<_>, _>>()?;
    let mut locals = Vec::new();
    attach_transient(db, &mut body.steps, &mut locals);
    let atoms = body
        .steps
        .iter()
        .filter_map(|step| match step {
            Step::Positive { probe, .. } => Some(db.relations[probe.rel].name.clone()),
            _ => None,
        })
        .collect();
    let windows = vec![ALL; body.steps.len()];
    let mut sink = Collect::new(outputs, limits);
    let mut env = vec![Value::Int(0); body.slots];
    Walk::new(db, &locals).walk(
        &body.steps,
        &windows,
        0,
        &mut env,
        &mut Vec::new(),
        &mut sink,
    )?;
    Ok(sink.finish(atoms))
}
