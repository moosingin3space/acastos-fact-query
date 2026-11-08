//! `fact-query`: a governance-free proposer/verifier substrate over a fact base.
//!
//! This crate factors out a single recurring primitive of neurosymbolic systems:
//! **an untrusted artifact is proposed, the deterministic engine speculatively
//! evaluates it under a resource bound and reports what it *does* (with
//! provenance), and some net — a human, a vote, or nothing — decides whether that
//! is what was wanted.** It is deliberately *governance free*: it carries no LLM,
//! no commit path, no policy or denial vocabulary, and no human gate. Those are an
//! application's opt-in layer. The crate depends only on [`ascent_jit`] and never
//! on any application; that dependency direction is the governance-free guarantee.
//!
//! # The contract
//!
//! v1 ships exactly one grain — the **queries grain**: conjunctive queries
//! (joins, filters, aggregates over existing relations; no new derived
//! relations, no recursion, no negation). For an evaluated query the substrate
//! guarantees, deterministically and with no LLM in the loop, that it is:
//!
//! 1. **Parsed** — valid query IR;
//! 2. **Schema-valid** — every referenced relation exists, arity matches, column
//!    types match;
//! 3. **Safe / range-restricted** — every output variable and every variable in
//!    a filter is bound by a positive body literal;
//! 4. **Read-only** — guaranteed by the query class: a conjunctive query has no
//!    write semantics, so it cannot mutate persisted state at the grammar level,
//!    without any handle plumbing to defend that;
//! 5. **Cardinality-bounded** — evaluation cannot blow up space past the
//!    supplied [`Limits`]: at most so many rows, each with at most so many
//!    justifications. Hitting either cap is a first-class outcome
//!    ([`ResultSet::is_truncated`] for rows, [`Provenance::is_truncated`] for
//!    evidence), not an error.
//!
//! **It explicitly does _not_ guarantee the sixth thing a caller might wish for:
//! that the query _answers the question asked_.** The substrate checks *form*,
//! not *meaning*. That is why [`eval`](FactStore::eval) returns a
//! [`ResultSet`], never an `Answer`: a query can parse, type-check, run, and
//! return a clean result set that does not mean what was intended — the
//! "valid but wrong" problem. Closing it (show-the-evidence-and-confirm, a
//! paraphrase round-trip, N-candidate self-consistency) is the **caller's** net
//! to build; the substrate hands them [`Provenance`] to build it on, and
//! nothing more. Do not let this crate's clean API read as authoritative about
//! answers.

mod engine;
pub mod error;
pub mod query;
pub mod result;
pub mod schema;
pub mod source;

/// Compiles the `README.md` code example as a doctest (under `cfg(doctest)`
/// only, so it never appears in the rendered crate docs) — keeping the README's
/// usage snippet honest against the public API.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct ReadmeDoctests;

/// Compiles the `tutorial/` Rust snippets as doctests (under `cfg(doctest)`
/// only), so the data-modeling guide cannot drift from the real engine and
/// query API. See [`../../tutorial`](https://github.com/moosingin3space/acastos-fact-query/tree/main/tutorial).
#[cfg(doctest)]
#[doc = include_str!("../../tutorial/README.md")]
#[doc = include_str!("../../tutorial/01-foundations.md")]
#[doc = include_str!("../../tutorial/02-modeling-structure.md")]
#[doc = include_str!("../../tutorial/03-collections-keys-grounding.md")]
pub struct TutorialDoctests;

pub use crate::error::FactQueryError;
pub use crate::query::ConjunctiveQuery;
pub use crate::result::{FactView, Justification, Provenance, ResultSet, SupportFact};
pub use crate::schema::{RelationSchema, Schema};
pub use crate::source::{
    ContentId, FactSource, LoweredTuple, PredicateDescriptor, PredicateId, SchemaDrift,
    SchemaFingerprint, TupleStream,
};

/// The bounds on one [`eval`](FactStore::eval): a cap on result **rows** and,
/// independently, a cap on **justifications kept per row**.
///
/// The bound is on **cardinality, not time**: a conjunctive query always
/// terminates, so the blast radius that must be capped is *space*. The output
/// holds at most `max_rows` rows with at most `max_justifications_per_row`
/// justifications each, so an unfiltered (cartesian-product) join is truncated
/// rather than allowed to exhaust memory. A forgotten cap is a
/// memory-exhaustion `DoS`, so the bound is threaded through every evaluation.
///
/// The caps are separate so the two kinds of incompleteness stay apart: "these
/// are not all the answers" ([`ResultSet::is_truncated`]) versus "these are
/// all the answers, but not every reason" ([`Provenance::is_truncated`]). If
/// the row cap stops evaluation, both are reported, since the remaining
/// evidence was not examined either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    max_rows: usize,
    max_justifications_per_row: usize,
}

impl Limits {
    /// The per-row justification cap used by [`Limits::rows`]: enough evidence
    /// to show for a row, small enough that a highly-redundant join stays
    /// cheap.
    pub const DEFAULT_JUSTIFICATIONS_PER_ROW: usize = 16;

    /// At most `max_rows` rows, with the default per-row justification cap
    /// ([`Limits::DEFAULT_JUSTIFICATIONS_PER_ROW`]).
    #[must_use]
    pub fn rows(max_rows: usize) -> Self {
        Self {
            max_rows,
            max_justifications_per_row: Self::DEFAULT_JUSTIFICATIONS_PER_ROW,
        }
    }

    /// Replaces the per-row justification cap.
    #[must_use]
    pub fn with_justifications_per_row(self, max: usize) -> Self {
        Self {
            max_justifications_per_row: max,
            ..self
        }
    }

    /// The row cap.
    #[must_use]
    pub fn max_rows(self) -> usize {
        self.max_rows
    }

    /// The per-row justification cap.
    #[must_use]
    pub fn max_justifications_per_row(self) -> usize {
        self.max_justifications_per_row
    }
}

/// What any fact base must expose to host the queries grain.
///
/// Note what is **absent** versus a full rule pipeline: there is no
/// stratification step, no rule fork, and nothing that commits. The
/// `ascent-jit` engine gets the canonical implementation
/// ([`impl FactStore for Engine`](ascent_jit::Engine)); a different backend (a
/// different Datalog engine, a relational store) can implement the same trait,
/// and that is what makes any loop written over this trait portable.
pub trait FactStore {
    /// How the backend describes its relations for grounding ([`Schema`]).
    type Schema;
    /// The backend's parsed, typed conjunctive query ([`ConjunctiveQuery`]).
    type Query;
    /// The rows an evaluation returns ([`ResultSet`]).
    type ResultSet;
    /// Why each row is there ([`Provenance`]).
    type Provenance;
    /// What can go wrong ([`FactQueryError`]).
    type Error;

    /// Relations, arity, column types, and (where available) doc-strings — the
    /// material a proposer needs to ground a query.
    fn schema(&self) -> Self::Schema;

    /// Parses surface text into a typed conjunctive query (guarantee 1). Takes
    /// `&mut self` because parsing interns symbols into the backend.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`](FactStore::Error) if the text is not well-formed.
    fn parse_query(&mut self, text: &str) -> Result<Self::Query, Self::Error>;

    /// Form checks only: schema-validity (guarantee 2) and safety /
    /// range-restriction (guarantee 3). Makes no claim about intent.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`](FactStore::Error) if the query is not
    /// schema-valid or not range-restricted.
    fn check(&self, query: &Self::Query) -> Result<(), Self::Error>;

    /// Read-only, cardinality-bounded evaluation against the current fixed
    /// point (guarantees 4 and 5), under the row and evidence caps in
    /// `limits`. Returns the result rows **and** their provenance — provenance
    /// is always part of the result, never optional, because it is the one
    /// bridge to "why believe this row".
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`](FactStore::Error) if evaluation faults. Such an
    /// error is indeterminate — a safety-conscious caller treats it fail-closed,
    /// not as an empty result.
    fn eval(
        &mut self,
        query: &Self::Query,
        limits: Limits,
    ) -> Result<(Self::ResultSet, Self::Provenance), Self::Error>;
}
