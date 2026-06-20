//! The error type for the queries grain.

/// Anything that can go wrong proposing or evaluating a conjunctive query.
///
/// The variants line up with the guarantees of the contract (see the crate
/// docs): [`Parse`](FactQueryError::Parse) and [`Schema`](FactQueryError::Schema)
/// reject ill-formed proposals, [`Unsafe`](FactQueryError::Unsafe) rejects ones
/// that are not range-restricted (or use disallowed features), and
/// [`Eval`](FactQueryError::Eval) reports an engine fault *during* evaluation.
///
/// An [`Eval`](FactQueryError::Eval) error is **indeterminate**: it means the
/// engine could not finish, not that the result is empty. A safety-conscious
/// caller that keys a decision on query results must treat it fail-closed, never
/// as "no results".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactQueryError {
    /// The query text is not well-formed (guarantee 1: *parsed*).
    Parse(String),
    /// The query references a relation/arity/type the schema does not have
    /// (guarantee 2: *schema-valid*).
    Schema(String),
    /// The query is not range-restricted, or uses a feature outside the
    /// conjunctive fragment such as negation (guarantee 3: *safe*).
    Unsafe(String),
    /// Evaluation faulted. Indeterminate — treat fail-closed, not as empty.
    Eval(String),
}

impl std::fmt::Display for FactQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FactQueryError::Parse(m) => write!(f, "parse error: {m}"),
            FactQueryError::Schema(m) => write!(f, "schema error: {m}"),
            FactQueryError::Unsafe(m) => write!(f, "unsafe query: {m}"),
            FactQueryError::Eval(m) => write!(f, "evaluation error: {m}"),
        }
    }
}

impl std::error::Error for FactQueryError {}
