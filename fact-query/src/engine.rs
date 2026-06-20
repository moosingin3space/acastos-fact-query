//! The canonical [`FactStore`] implementation, over an `ascent-jit` [`Engine`].
//!
//! This is the one in-tree backend. It lowers a [`ConjunctiveQuery`] onto the
//! engine's read-only ad-hoc query primitive and groups the per-join solutions
//! it returns into distinct rows plus their provenance. It carries no policy:
//! the `Engine` it borrows is just a fact base, and nothing here knows any
//! application's denial, trust, or origin vocabulary — that layers on top.

use std::collections::HashMap;

use ascent_jit::{Engine, QueryOutput, Value};

use crate::error::FactQueryError;
use crate::query::ConjunctiveQuery;
use crate::result::{Justification, Provenance, ResultSet, RowProvenance, SupportTuple};
use crate::schema::{RelationSchema, Schema};
use crate::{Cardinality, FactStore};

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
        max: Cardinality,
    ) -> Result<(ResultSet, Provenance), FactQueryError> {
        let output = self
            .evaluate_query(query.outputs(), query.body(), max.get())
            .map_err(|e| FactQueryError::Eval(e.to_string()))?;
        Ok(group(output))
    }
}

/// Groups raw per-join solutions into distinct result rows (first-seen order)
/// paired with index-aligned provenance — one [`Justification`] per join
/// combination that produced the row.
fn group(output: QueryOutput) -> (ResultSet, Provenance) {
    let mut rows: Vec<Vec<Value>> = Vec::new();
    let mut index: HashMap<Vec<Value>, usize> = HashMap::new();
    let mut provs: Vec<RowProvenance> = Vec::new();

    for solution in output.solutions {
        let justification = Justification {
            support: solution
                .support
                .into_iter()
                .map(|(relation, tuple)| SupportTuple { relation, tuple })
                .collect(),
        };
        if let Some(&i) = index.get(&solution.tuple) {
            provs[i].justifications.push(justification);
        } else {
            let i = rows.len();
            index.insert(solution.tuple.clone(), i);
            rows.push(solution.tuple.clone());
            provs.push(RowProvenance {
                row: solution.tuple,
                justifications: vec![justification],
            });
        }
    }

    (
        ResultSet::new(rows, output.truncated),
        Provenance::new(provs),
    )
}
