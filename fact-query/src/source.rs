//! The `FactSource` ingestion seam: the produce-side complement to
//! [`FactStore`](crate::FactStore).
//!
//! Where [`FactStore`](crate::FactStore) is the **read** seam — "what currently-true
//! tuples match this pattern?" — `FactSource` is the **produce** seam: a backend
//! that describes a batch of facts (their schema and a content identity) and streams
//! them *already lowered* into the engine's value model. The two compose: a
//! `FactSource` populates the EDB that a `FactStore` then queries.
//!
//! This module is **governance-free**: it knows nothing of trust, origin
//! namespaces, taint, or commit. A host stamps origin and trust *above* this
//! seam; the substrate only moves typed, content-identified tuples. The crate
//! depends on `ascent-jit` and never on any application, and that dependency
//! direction is the guarantee.

use ascent_jit::{Interner, Type, Value};

/// A stable `u32` identifier for a predicate, assigned by the producer.
///
/// The id is the producer's own; it is stable across schema *versions* of the same
/// predicate (the [`SchemaFingerprint`] is what moves when the schema does). A host
/// ingesting a producer matches predicates by this id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PredicateId(u32);

impl PredicateId {
    /// Wraps a raw predicate id.
    #[must_use]
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    /// The raw id.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

/// An opaque 256-bit digest of a single predicate's schema, supplied by the producer.
///
/// It lets a host detect **schema drift**: a producer built against a schema this
/// binary no longer agrees with is refused at ingest, the column-grain
/// analogue of the engine's arity check. The substrate does not dictate the hash
/// function — only that the digest is stable and collision-resistant for a given
/// schema; comparison is the host's, against the fingerprint it expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SchemaFingerprint([u8; 32]);

impl SchemaFingerprint {
    /// Wraps a raw 256-bit digest.
    #[must_use]
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A stable content identity for a batch of facts, computed over the producer's own
/// bytes **before any interning**.
///
/// It is both the **cache key** (an unchanged `ContentId` means an unchanged set of
/// facts, so ingestion may be skipped) and the **origin handle** a host stamps onto
/// the facts it ingests. It must **not** depend on engine-local interner state: an
/// insertion-order [`Symbol`](ascent_jit::Symbol) id is not stable across runs, so
/// letting one leak in here would make identity run-dependent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentId([u8; 32]);

impl ContentId {
    /// Wraps a raw 256-bit content digest.
    #[must_use]
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One predicate as a producer describes it, for validation and grounding.
///
/// This is the produce-side analogue of [`RelationSchema`](crate::RelationSchema),
/// with two differences that matter at ingest: it carries the producer's stable
/// [`PredicateId`] and [`SchemaFingerprint`], and its `doc` is **required** rather
/// than optional — relation doc-strings are load-bearing for grounding,
/// and a producer is expected to supply them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredicateDescriptor {
    /// The producer's stable identifier for this predicate.
    pub id: PredicateId,
    /// The predicate's namespaced name, e.g. `tree_sitter.symbol`.
    pub name: String,
    /// A digest of this predicate's schema, for drift detection.
    pub fingerprint: SchemaFingerprint,
    /// The lowered column types, in order. The arity is `columns.len()`.
    pub columns: Vec<Type>,
    /// A human-readable description of what the predicate *means* (required).
    pub doc: String,
}

impl PredicateDescriptor {
    /// The predicate's arity.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.columns.len()
    }

    /// Validates this descriptor against the columns and fingerprint a host expects,
    /// detecting **schema drift** before ingestion.
    ///
    /// `expected_columns` is typically the target relation's declared column types
    /// (e.g. from a [`RelationSchema`](crate::RelationSchema)); `expected_fingerprint`
    /// is the digest the host was built against. Columns are checked first because they
    /// are the most actionable diagnostic; the fingerprint is checked last because it
    /// also catches drift the lowered column types *cannot* see — two different schemas
    /// can lower to the same `Vec<Type>` (a field rename, a column whose meaning moved),
    /// and only the producer's fingerprint distinguishes them.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaDrift`] on an arity, column-type, or fingerprint mismatch.
    pub fn validate(
        &self,
        expected_columns: &[Type],
        expected_fingerprint: SchemaFingerprint,
    ) -> Result<(), SchemaDrift> {
        if self.columns.len() != expected_columns.len() {
            return Err(SchemaDrift::Arity {
                expected: expected_columns.len(),
                got: self.columns.len(),
            });
        }
        for (column, (got, expected)) in self.columns.iter().zip(expected_columns).enumerate() {
            if got != expected {
                return Err(SchemaDrift::ColumnType {
                    column,
                    expected: *expected,
                    got: *got,
                });
            }
        }
        if self.fingerprint != expected_fingerprint {
            return Err(SchemaDrift::Fingerprint {
                expected: expected_fingerprint,
                got: self.fingerprint,
            });
        }
        Ok(())
    }
}

/// Why a producer's predicate failed validation against the schema a host expects —
/// the column-grain analogue of the engine's arity check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaDrift {
    /// The producer's column count disagrees with the expected arity.
    Arity {
        /// The arity the host expects.
        expected: usize,
        /// The arity the producer declares.
        got: usize,
    },
    /// A column's lowered type disagrees with the expected column type.
    ColumnType {
        /// The zero-based column index.
        column: usize,
        /// The type the host expects.
        expected: Type,
        /// The type the producer declares.
        got: Type,
    },
    /// The producer's schema fingerprint disagrees with the one the host expects: the
    /// producer was built against a schema this host no longer agrees with.
    Fingerprint {
        /// The fingerprint the host expects.
        expected: SchemaFingerprint,
        /// The fingerprint the producer declares.
        got: SchemaFingerprint,
    },
}

impl std::fmt::Display for SchemaDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaDrift::Arity { expected, got } => {
                write!(f, "schema drift: expected arity {expected}, got {got}")
            }
            SchemaDrift::ColumnType {
                column,
                expected,
                got,
            } => write!(
                f,
                "schema drift: column {column} expects {expected:?}, got {got:?}"
            ),
            SchemaDrift::Fingerprint { .. } => {
                write!(f, "schema drift: predicate fingerprint mismatch")
            }
        }
    }
}

impl std::error::Error for SchemaDrift {}

/// A single fact, already lowered into the engine's value model and tagged with the
/// predicate it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredTuple {
    /// Which predicate this tuple is a row of.
    pub predicate: PredicateId,
    /// The lowered column values, in declared order. Its length and per-column
    /// types are expected to match the predicate's [`PredicateDescriptor::columns`].
    pub values: Vec<Value>,
}

/// A stream of [`LoweredTuple`]s produced by a [`FactSource`].
///
/// Per-tuple iteration is infallible: any structural/decoding fault is surfaced up
/// front as the `Err` of [`FactSource::tuples`], not mid-stream, and symbol columns
/// are interned into the engine's [`Interner`] when the stream is built (so the
/// stream retains no borrow of it). Lowering is a total function.
pub struct TupleStream<'a> {
    inner: Box<dyn Iterator<Item = LoweredTuple> + 'a>,
}

impl<'a> TupleStream<'a> {
    /// Builds a stream from any iterator of lowered tuples.
    #[must_use]
    pub fn new<I>(iter: I) -> Self
    where
        I: Iterator<Item = LoweredTuple> + 'a,
    {
        Self {
            inner: Box::new(iter),
        }
    }
}

impl Iterator for TupleStream<'_> {
    type Item = LoweredTuple;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

/// A backend that produces a batch of facts for ingestion into the engine's EDB.
///
/// The trait is deliberately small and carries **no** commit path, namespace
/// opinion, or trust tag: a `FactSource` describes a schema, names a
/// content identity, and yields lowered tuples. Where those tuples land, what origin
/// they are stamped with, and whether they may justify an action are the host's
/// decisions, applied above this seam.
pub trait FactSource {
    /// What can go wrong setting up a tuple stream (e.g. decoding the producer's
    /// container).
    type Error;

    /// The producer's self-describing schema: one [`PredicateDescriptor`] per
    /// predicate it emits.
    #[must_use]
    fn schema(&self) -> &[PredicateDescriptor];

    /// A stable content identity for this batch (the cache key and origin handle).
    #[must_use]
    fn content_id(&self) -> ContentId;

    /// Streams this batch's tuples, lowered into the engine's value model, interning
    /// symbol columns into `interner` so their [`Symbol`](ascent_jit::Symbol) ids match
    /// the engine they will be inserted into.
    ///
    /// Interning is performed against `interner` when the stream is built, so the
    /// returned stream yields already-interned tuples and retains no borrow of
    /// `interner`. A batch's [`content_id`](FactSource::content_id) is independent of
    /// this — it is computed over the producer's bytes *before* interning,
    /// so it never depends on the engine-local interner state mutated here.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`](FactSource::Error) if the stream cannot be set up
    /// (e.g. the producer's bytes fail to decode). A host should treat such an error
    /// fail-closed — ingest nothing — rather than ingest a partial batch.
    fn tuples(&self, interner: &mut Interner) -> Result<TupleStream<'_>, Self::Error>;
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use ascent_jit::{Interner, Type, Value};

    use super::{
        ContentId, FactSource, LoweredTuple, PredicateDescriptor, PredicateId, SchemaDrift,
        SchemaFingerprint, TupleStream,
    };

    const SYMBOL: PredicateId = PredicateId(1);

    /// A trivial in-memory `FactSource` whose rows hold an *un-interned* name plus a
    /// scalar, so `tuples` exercises interning into the caller's [`Interner`].
    struct VecSource {
        schema: Vec<PredicateDescriptor>,
        id: ContentId,
        rows: Vec<(String, i64)>,
    }

    impl FactSource for VecSource {
        type Error = Infallible;

        fn schema(&self) -> &[PredicateDescriptor] {
            &self.schema
        }

        fn content_id(&self) -> ContentId {
            self.id
        }

        fn tuples(&self, interner: &mut Interner) -> Result<TupleStream<'_>, Self::Error> {
            let lowered: Vec<LoweredTuple> = self
                .rows
                .iter()
                .map(|(name, line)| LoweredTuple {
                    predicate: SYMBOL,
                    values: vec![Value::Sym(interner.intern(name)), Value::Int(*line)],
                })
                .collect();
            Ok(TupleStream::new(lowered.into_iter()))
        }
    }

    fn sample() -> VecSource {
        VecSource {
            schema: vec![PredicateDescriptor {
                id: SYMBOL,
                name: "tree_sitter.symbol".to_owned(),
                fingerprint: SchemaFingerprint::new([7; 32]),
                columns: vec![Type::Sym, Type::Int],
                doc: "A definition site of a named symbol.".to_owned(),
            }],
            id: ContentId::new([3; 32]),
            rows: vec![("alpha".to_owned(), 10), ("beta".to_owned(), 20)],
        }
    }

    #[test]
    fn describes_its_schema_and_identity() {
        let src = sample();
        assert_eq!(src.schema().len(), 1);
        assert_eq!(src.schema()[0].id, SYMBOL);
        assert_eq!(src.schema()[0].arity(), 2);
        assert_eq!(src.content_id().as_bytes(), &[3; 32]);
    }

    #[test]
    fn streams_tuples_interned_into_the_caller_interner() {
        let src = sample();
        let mut interner = Interner::new();
        let collected: Vec<LoweredTuple> = src.tuples(&mut interner).expect("infallible").collect();

        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0].predicate, SYMBOL);

        // The symbol column was interned into *our* interner: interning the same name
        // again yields the same id, and it resolves back to the source string.
        let Value::Sym(first) = collected[0].values[0] else {
            panic!("first column should be a symbol");
        };
        assert_eq!(first, interner.intern("alpha"));
        assert_eq!(interner.resolve(first), Some("alpha"));
        assert_eq!(collected[0].values[1], Value::Int(10));
    }

    #[test]
    fn validate_accepts_a_matching_descriptor() {
        let src = sample();
        src.schema[0]
            .validate(&[Type::Sym, Type::Int], SchemaFingerprint::new([7; 32]))
            .expect("columns and fingerprint match");
    }

    #[test]
    fn validate_detects_arity_drift() {
        let src = sample();
        let err = src.schema[0]
            .validate(&[Type::Sym], SchemaFingerprint::new([7; 32]))
            .expect_err("arity differs");
        assert_eq!(
            err,
            SchemaDrift::Arity {
                expected: 1,
                got: 2
            }
        );
    }

    #[test]
    fn validate_detects_column_type_drift() {
        let src = sample();
        let err = src.schema[0]
            .validate(&[Type::Int, Type::Int], SchemaFingerprint::new([7; 32]))
            .expect_err("column 0 differs");
        assert_eq!(
            err,
            SchemaDrift::ColumnType {
                column: 0,
                expected: Type::Int,
                got: Type::Sym,
            }
        );
    }

    #[test]
    fn validate_detects_fingerprint_drift_when_columns_match() {
        let src = sample();
        // Columns are identical, but the fingerprint differs — the case only the
        // digest can catch.
        let err = src.schema[0]
            .validate(&[Type::Sym, Type::Int], SchemaFingerprint::new([9; 32]))
            .expect_err("fingerprint differs");
        assert!(matches!(err, SchemaDrift::Fingerprint { .. }));
    }
}
