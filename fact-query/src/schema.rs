//! The schema a [`FactStore`](crate::FactStore) exposes for grounding.
//!
//! Grounding — writing a query that means what was asked — depends on the
//! proposer knowing the relations, their shapes, and ideally their *meaning*.
//! Relation doc-strings are load-bearing for exactly this reason, so
//! [`RelationSchema`] carries an optional `doc`. A backend whose underlying IR
//! has no doc-strings (the `ascent-jit` engine, today) reports `None`; that is a
//! known grounding gap, not a contract violation.

use ascent_jit::Type;

/// One relation as the substrate describes it for grounding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationSchema {
    /// The relation name (possibly namespaced, e.g. `policy::grant`).
    pub name: String,
    /// The column types, in order. The arity is `columns.len()`.
    pub columns: Vec<Type>,
    /// A human-readable description of what the relation *means*, if the backend
    /// has one. `None` is a grounding gap, not an error.
    pub doc: Option<String>,
}

impl RelationSchema {
    /// The relation's arity.
    #[must_use]
    pub fn arity(&self) -> usize {
        self.columns.len()
    }
}

/// The relations a fact base exposes, for grounding a query proposer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schema {
    relations: Vec<RelationSchema>,
}

impl Schema {
    /// Builds a schema from its relations.
    #[must_use]
    pub fn new(relations: Vec<RelationSchema>) -> Self {
        Self { relations }
    }

    /// Every relation in the schema.
    #[must_use]
    pub fn relations(&self) -> &[RelationSchema] {
        &self.relations
    }

    /// Looks up a relation by name.
    #[must_use]
    pub fn relation(&self, name: &str) -> Option<&RelationSchema> {
        self.relations.iter().find(|r| r.name == name)
    }
}
