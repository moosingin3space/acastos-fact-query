//! The return shape of [`eval`](crate::FactStore::eval): a result set plus its
//! provenance.
//!
//! The substrate deliberately returns a [`ResultSet`], never an `Answer`: it
//! guarantees the rows are well-formed, safe, read-only, and bounded — **not**
//! that they mean what the question asked. With intent-fidelity
//! delegated to the caller, [`Provenance`] — which facts joined to produce each
//! row — is the one bridge the substrate offers between "a query ran" and "here
//! is why to believe this row", and the raw material for whatever net (show the
//! evidence, paraphrase-and-confirm, N-candidate vote) the caller builds.

use ascent_jit::Value;

/// One supporting tuple: a body fact that participated in a justification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportTuple {
    /// The relation the supporting fact belongs to.
    pub relation: String,
    /// The supporting fact itself.
    pub tuple: Vec<Value>,
}

/// One way a row was produced: the set of body facts that joined to yield it.
/// A row can have several justifications (distinct joins giving the same row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Justification {
    /// The body facts that joined, in body order.
    pub support: Vec<SupportTuple>,
}

/// The provenance of a single result row: the row, plus every justification the
/// evaluator found for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowProvenance {
    /// The result row this provenance explains.
    pub row: Vec<Value>,
    /// Every justification for the row (at least one).
    pub justifications: Vec<Justification>,
}

/// The result rows of a query: distinct output tuples, plus whether the result
/// was truncated at the cardinality cap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultSet {
    rows: Vec<Vec<Value>>,
    truncated: bool,
}

impl ResultSet {
    /// Builds a result set.
    #[must_use]
    pub fn new(rows: Vec<Vec<Value>>, truncated: bool) -> Self {
        Self { rows, truncated }
    }

    /// The distinct result rows.
    #[must_use]
    pub fn rows(&self) -> &[Vec<Value>] {
        &self.rows
    }

    /// The number of rows returned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the result is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Whether evaluation stopped at the cardinality cap. When true the rows are
    /// a prefix of the full result set — a first-class outcome, not an error.
    /// A decision keyed on "the result is empty" must distinguish
    /// this from a genuinely empty result.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
}

/// The provenance for a whole result set: one [`RowProvenance`] per row, aligned
/// by index with [`ResultSet::rows`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    rows: Vec<RowProvenance>,
}

impl Provenance {
    /// Builds provenance from per-row entries (same order as the result rows).
    #[must_use]
    pub fn new(rows: Vec<RowProvenance>) -> Self {
        Self { rows }
    }

    /// The per-row provenance entries, aligned with [`ResultSet::rows`].
    #[must_use]
    pub fn rows(&self) -> &[RowProvenance] {
        &self.rows
    }
}
