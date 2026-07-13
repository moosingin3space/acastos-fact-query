//! A Just-in-Time Compiler for the Ascent library.
//!
//! `ascent-jit` evaluates an Ascent program supplied **as data** at runtime,
//! without invoking the Rust compiler. The relational core is an interpreter
//! (a tree-walk of a relational IR with a stratified fixed-point loop); the
//! `if`/`let`/head expressions are lowered to WebAssembly and JIT-compiled by
//! `wasmtime` (see [`wasm`]).
//!
//! ```
//! use ascent_jit::{Engine, Value};
//!
//! let mut engine = Engine::from_source(
//!     "relation edge(int, int);
//!      relation path(int, int);
//!      path(x, y) <-- edge(x, y);
//!      path(x, z) <-- edge(x, y), path(y, z);",
//! )
//! .unwrap();
//! engine.add_fact("edge", vec![Value::Int(1), Value::Int(2)]).unwrap();
//! engine.add_fact("edge", vec![Value::Int(2), Value::Int(3)]).unwrap();
//! engine.run().unwrap();
//! assert_eq!(engine.query("path").len(), 3);
//! ```

pub mod eval;
pub mod expr;
#[cfg(feature = "arbitrary")]
pub mod fuzz;
pub mod ir;
pub mod parser;
pub mod value;
pub mod wasm;

use std::collections::{HashMap, HashSet};

use crate::eval::{Database, EvalError};
use crate::expr::Expr;
use crate::ir::{BodyClause, Program};
use crate::parser::ParseError;

pub use crate::eval::{Derivation, ExprEval, QueryOutput, QuerySolution};
pub use crate::expr::ExprError;
pub use crate::value::{Interner, Symbol, Type, Value};
#[cfg(feature = "wasmtime")]
pub use crate::wasm::WasmtimeExecutor;
pub use crate::wasm::{WasmEval, WasmExecutor};

/// Any error produced by the engine.
#[derive(Debug)]
pub enum Error {
    /// A textual program failed to parse.
    Parse(ParseError),
    /// Validation or evaluation failed.
    Eval(EvalError),
    /// The WASM expression tier failed to initialise.
    #[cfg(feature = "wasmtime")]
    Wasm(ExprError),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(e) => write!(f, "{e}"),
            Error::Eval(e) => write!(f, "{e}"),
            #[cfg(feature = "wasmtime")]
            Error::Wasm(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<ParseError> for Error {
    fn from(e: ParseError) -> Self {
        Error::Parse(e)
    }
}

impl From<EvalError> for Error {
    fn from(e: EvalError) -> Self {
        Error::Eval(e)
    }
}

/// The pure tree-walking expression evaluator. It is the differential oracle
/// for the WASM tier and is available as an alternative backend.
#[derive(Debug, Default, Clone, Copy)]
pub struct Interpreter;

impl ExprEval for Interpreter {
    fn eval_expr(&mut self, expr: &Expr, env: &HashMap<Symbol, Value>) -> Result<Value, ExprError> {
        expr.eval(env)
    }
}

/// The tuples that become newly derivable when candidate facts are
/// speculatively added, keyed by relation.
///
/// v1 evaluation is monotone (no retraction), so a speculative addition can
/// only *add* derivations; `added` is therefore the complete delta against the
/// baseline. This is the substrate for "what-if" checks — for example "does any
/// `violation`/denial relation gain a tuple if these candidate facts are
/// added?" — and for previewing the consequences of a proposed change before
/// any decision is made on it.
#[derive(Debug, Default, Clone)]
pub struct Consequences {
    added: HashMap<String, Vec<Vec<Value>>>,
}

impl Consequences {
    /// The tuples newly derivable in `relation` (empty if none).
    #[must_use]
    pub fn added(&self, relation: &str) -> &[Vec<Value>] {
        self.added.get(relation).map_or(&[], Vec::as_slice)
    }

    /// Every relation that gained tuples, paired with those tuples. Iteration
    /// order is unspecified.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[Vec<Value>])> {
        self.added.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    /// True if the candidate facts produced no new derivations anywhere.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
    }
}

/// A loaded Ascent program plus its database, ready to accept facts and run.
#[derive(Debug)]
pub struct Engine {
    program: Program,
    interner: Interner,
    db: Database,
    evaluator: Box<dyn ExprEval>,
}

impl Engine {
    /// Parses `src` and builds an engine backed by the default `wasmtime` WASM
    /// expression tier.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the source fails to parse, the program fails
    /// validation/stratification, or the WASM engine cannot be initialised.
    #[cfg(feature = "wasmtime")]
    pub fn from_source(src: &str) -> Result<Self, Error> {
        let evaluator = WasmEval::new().map_err(Error::Wasm)?;
        Self::from_source_with_evaluator(src, Box::new(evaluator))
    }

    /// Parses `src` and builds an engine backed by the pure interpreter.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the source fails to parse or validate.
    pub fn from_source_interpreted(src: &str) -> Result<Self, Error> {
        Self::from_source_with_evaluator(src, Box::new(Interpreter))
    }

    /// Parses `src` and builds an engine over a caller-supplied expression
    /// evaluator. This is the executor-swapping entry point: a host without
    /// `wasmtime` (e.g. browser-wasm) passes a [`WasmEval`] built over its own
    /// [`WasmExecutor`], reusing the entire encoding pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the source fails to parse or validate.
    pub fn from_source_with_evaluator(
        src: &str,
        mut evaluator: Box<dyn ExprEval>,
    ) -> Result<Self, Error> {
        let mut interner = Interner::new();
        let program = parser::parse(src, &mut interner)?;
        // Validate eagerly so a malformed program is rejected at load time.
        eval::stratify(&program)?;
        // Compile the whole program's expression tier up front (ADR 0005): a
        // compiling backend builds its single module here so `run` instantiates
        // nothing. The interpreter's `prime` is a no-op.
        evaluator
            .prime(&program.exprs())
            .map_err(|e| Error::Eval(e.into()))?;
        let db = Database::new(&program);
        Ok(Engine {
            program,
            interner,
            db,
            evaluator,
        })
    }

    /// Interns `s`, for building or querying tuples that contain symbols.
    pub fn intern(&mut self, s: &str) -> Symbol {
        self.interner.intern(s)
    }

    /// Resolves a previously interned symbol back to its string.
    #[must_use]
    pub fn resolve(&self, sym: Symbol) -> Option<&str> {
        self.interner.resolve(sym)
    }

    /// Asserts a ground fact into `relation`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the relation is unknown or the arity is wrong.
    pub fn add_fact(&mut self, relation: &str, tuple: Vec<Value>) -> Result<(), Error> {
        self.db.insert_fact(relation, tuple)?;
        Ok(())
    }

    /// Runs the program to a fixed point.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on stratification or evaluation failure.
    pub fn run(&mut self) -> Result<(), Error> {
        eval::run(&self.program, &mut self.db, self.evaluator.as_mut())?;
        Ok(())
    }

    /// Speculatively evaluates the effect of adding `facts`, **without mutating
    /// the engine**: forks the database, inserts the candidates, runs the fork
    /// to a fixed point, and returns the tuples that become newly derivable —
    /// then discards the fork.
    ///
    /// The baseline is the engine's *current* materialized state, so call
    /// [`Engine::run`] first if you want the delta against the committed fixed
    /// point. Each `facts` entry is `(relation, tuple)`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if a candidate names an unknown relation or wrong
    /// arity, or if evaluation fails. An evaluation error (including the
    /// iteration bound, [`EvalError::IterationLimit`]) is *indeterminate* — a
    /// safety-conscious caller must treat it as fail-closed (deny), never as
    /// "no new violations".
    pub fn speculate(&mut self, facts: &[(&str, Vec<Value>)]) -> Result<Consequences, Error> {
        let mut fork = self.db.clone();
        for (relation, tuple) in facts {
            fork.insert_fact(relation, tuple.clone())?;
        }
        eval::run(&self.program, &mut fork, self.evaluator.as_mut())?;

        let mut added = HashMap::new();
        for decl in &self.program.relations {
            let before: HashSet<Vec<Value>> = self.db.tuples(&decl.name).into_iter().collect();
            let mut delta: Vec<Vec<Value>> = fork
                .tuples(&decl.name)
                .into_iter()
                .filter(|t| !before.contains(t))
                .collect();
            if !delta.is_empty() {
                delta.sort();
                added.insert(decl.name.clone(), delta);
            }
        }
        Ok(Consequences { added })
    }

    /// Returns every tuple currently in `relation` (empty if unknown).
    #[must_use]
    pub fn query(&self, relation: &str) -> Vec<Vec<Value>> {
        self.db.tuples(relation)
    }

    /// Explains why `tuple` is present in `relation`: the one-step derivations
    /// (rule + supporting body tuples) that produce it against the current
    /// materialized state. Recomputed on demand — call [`Engine::run`] first so
    /// the database is at a fixed point. Returns an empty vec if the tuple is
    /// not derivable (e.g. it is a base fact with no deriving rule, or absent).
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if `relation` is unknown or a clause fails to evaluate.
    pub fn explain(&mut self, relation: &str, tuple: &[Value]) -> Result<Vec<Derivation>, Error> {
        Ok(eval::explain(
            &self.program,
            &self.db,
            self.evaluator.as_mut(),
            relation,
            tuple,
        )?)
    }

    /// Parses a conjunctive query in surface syntax — `(e1, e2, ..) <-- clause,
    /// ..` — into its output expressions and body clauses, interning symbols into
    /// the engine's interner. This is the queries-grain front end; it performs
    /// no schema or safety checks (see [`Engine::check_query`]).
    ///
    /// Named `_parts` because it returns the building blocks (outputs + body), not
    /// a packaged query type — that framing belongs to the `fact-query` substrate
    /// layered on top, whose `FactStore::parse_query` wraps this.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the text fails to parse.
    pub fn parse_query_parts(&mut self, src: &str) -> Result<(Vec<Expr>, Vec<BodyClause>), Error> {
        Ok(parser::parse_query(src, &mut self.interner)?)
    }

    /// Form-checks a query `body` against the loaded schema: every referenced
    /// relation exists and every atom matches its declared arity. (Range
    /// restriction / safety is engine-agnostic and is the caller's.) The
    /// outputs need no schema check — they are expressions over the
    /// body's bound variables, not relation references.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on an unknown relation or arity mismatch.
    pub fn check_query(&self, body: &[BodyClause]) -> Result<(), Error> {
        Ok(eval::check_query_arities(&self.program, body)?)
    }

    /// Evaluates a parsed conjunctive query read-only against the current
    /// materialized state, returning every solution with provenance, capped at
    /// `max` solutions ([`QueryOutput::truncated`] is set if the cap is hit).
    /// Call [`Engine::run`] first so the query observes the fixed point.
    ///
    /// This neither forks nor mutates the database — it is the queries-grain
    /// primitive and cannot affect persisted state. Callers should
    /// [`Engine::check_query`] (and check range-restriction) first; an unbound
    /// variable here surfaces as an evaluation [`Error`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on an unknown relation or an expression-evaluation
    /// failure.
    pub fn evaluate_query(
        &mut self,
        outputs: &[Expr],
        body: &[BodyClause],
        max: usize,
    ) -> Result<QueryOutput, Error> {
        Ok(eval::eval_query(
            &self.db,
            self.evaluator.as_mut(),
            outputs,
            body,
            max,
        )?)
    }

    /// The loaded program IR.
    #[must_use]
    pub fn program(&self) -> &Program {
        &self.program
    }
}
