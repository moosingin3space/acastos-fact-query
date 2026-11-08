//! The canonical [`FactStore`] implementation, over an `ascent-jit` [`Engine`].
//!
//! This is the one in-tree backend. It lowers a [`ConjunctiveQuery`] onto the
//! engine's read-only ad-hoc query primitive, which already groups solutions
//! into distinct rows with normalized provenance, and resolves each distinct
//! supporting fact once. It carries no policy:
//! the `Engine` it borrows is just a fact base, and nothing here knows any
//! application's denial, trust, or origin vocabulary — that layers on top.

use ascent_jit::{Engine, QueryLimits, QueryOutput, Value};

use crate::error::FactQueryError;
use crate::query::ConjunctiveQuery;
use crate::result::{Provenance, ResultSet, SupportFact};
use crate::schema::{RelationSchema, Schema};
use crate::{FactStore, Limits};

impl FactStore for Engine {
    type Schema = Schema;
    type Query = ConjunctiveQuery;
    type ResultSet = ResultSet;
    type Provenance = Provenance;
    type Error = FactQueryError;

    fn schema(&self) -> Schema {
        let relations = self
            .program()
            .relations
            .iter()
            .map(|decl| RelationSchema {
                name: decl.name.clone(),
                columns: decl.schema.clone(),
                // The engine's IR carries no relation doc-strings yet; this is
                // a known grounding gap, not a contract violation.
                doc: None,
            })
            .collect();
        Schema::new(relations)
    }

    fn parse_query(&mut self, text: &str) -> Result<ConjunctiveQuery, FactQueryError> {
        let (outputs, body) = self
            .parse_query_parts(text)
            .map_err(|e| FactQueryError::Parse(e.to_string()))?;
        Ok(ConjunctiveQuery::new(outputs, body))
    }

    fn check(&self, query: &ConjunctiveQuery) -> Result<(), FactQueryError> {
        // Guarantee 2 (schema-valid): relations exist, arities match. Engine-side.
        self.check_query(query.body())
            .map_err(|e| FactQueryError::Schema(e.to_string()))?;
        // Guarantee 3 (safe / range-restricted): engine-agnostic; resolve symbols
        // only so the error names the offending variable.
        query.check_safety(|s| self.resolve(s).map(str::to_owned))
    }

    fn eval(
        &mut self,
        query: &ConjunctiveQuery,
        limits: Limits,
    ) -> Result<(ResultSet, Provenance), FactQueryError> {
        let output = self
            .evaluate_query(
                query.outputs(),
                query.body(),
                QueryLimits {
                    max_rows: limits.max_rows(),
                    max_justifications_per_row: limits.max_justifications_per_row(),
                },
            )
            .map_err(|e| FactQueryError::Eval(e.to_string()))?;
        Ok(lift(self, &output))
    }
}

/// Lifts the engine's grouped output into the substrate's result types,
/// resolving each distinct supporting fact (once) from its handle.
fn lift(engine: &Engine, output: &QueryOutput) -> (ResultSet, Provenance) {
    let rows = output.rows().map(<[Value]>::to_vec).collect();

    let mut relations: Vec<String> = Vec::new();
    let mut relation_index = |name: &str| -> usize {
        relations.iter().position(|r| r == name).unwrap_or_else(|| {
            relations.push(name.to_owned());
            relations.len() - 1
        })
    };
    let atoms: Vec<usize> = output.atoms().iter().map(|a| relation_index(a)).collect();
    let facts: Vec<SupportFact> = output
        .facts()
        .iter()
        .map(|&handle| {
            // Every handle in a query output names a row of this engine.
            let (relation, tuple) = engine.fact(handle).unwrap_or(("", &[]));
            SupportFact {
                relation: relation_index(relation),
                tuple: tuple.to_vec(),
            }
        })
        .collect();

    let mut offsets = Vec::with_capacity(output.len() + 1);
    let mut support = Vec::new();
    offsets.push(0);
    for row in 0..output.len() {
        for justification in output.justifications(row) {
            support.extend(justification.iter().map(|&f| f as usize));
        }
        offsets.push(offsets[row] + output.justifications(row).len());
    }

    (
        ResultSet::new(rows, output.rows_truncated()),
        Provenance::new(
            relations,
            atoms,
            facts,
            offsets,
            support,
            output.evidence_truncated(),
        ),
    )
}
