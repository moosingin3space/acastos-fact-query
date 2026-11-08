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

/// The result rows of a query: distinct output tuples, plus whether more rows
/// may exist than were returned.
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

    /// Whether more rows may exist than were returned (the row cap was
    /// reached). When true the rows are a prefix of the full result — a
    /// first-class outcome, not an error. A decision keyed on "the result is
    /// empty" must distinguish this from a genuinely empty result.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
}

/// One distinct supporting fact: a body fact that participated in at least one
/// justification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupportFact {
    /// Index into [`Provenance::relations`].
    pub relation: usize,
    /// The fact itself.
    pub tuple: Vec<Value>,
}

/// A supporting fact, resolved for reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactView<'a> {
    /// The relation the fact belongs to.
    pub relation: &'a str,
    /// The fact itself.
    pub tuple: &'a [Value],
}

/// The provenance of a whole result set, normalized: each distinct supporting
/// fact is stored once, and each justification refers to facts by index.
///
/// Every justification has exactly one supporting fact per positive body atom,
/// in body order, so support column `i` always comes from relation
/// [`atoms`](Provenance::atoms)`[i]`. Rows are aligned by index with
/// [`ResultSet::rows`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Provenance {
    relations: Vec<String>,
    atoms: Vec<usize>,
    facts: Vec<SupportFact>,
    offsets: Vec<usize>,
    support: Vec<usize>,
    truncated: bool,
}

impl Provenance {
    /// Builds provenance from its normalized parts:
    ///
    /// - `relations`: the distinct relation names referenced;
    /// - `atoms`: per support column (positive body atom, in body order), an
    ///   index into `relations`;
    /// - `facts`: the distinct supporting facts;
    /// - `offsets`: one more entry than there are rows; row `r`'s
    ///   justifications are `offsets[r]..offsets[r + 1]`;
    /// - `support`: justification-major, `atoms.len()` fact indices per
    ///   justification;
    /// - `truncated`: whether some row may have more justifications than given.
    ///
    /// # Panics
    ///
    /// Panics if the parts are inconsistent (an index out of range, `offsets`
    /// not starting at zero and non-decreasing, or `support` the wrong length).
    #[must_use]
    pub fn new(
        relations: Vec<String>,
        atoms: Vec<usize>,
        facts: Vec<SupportFact>,
        offsets: Vec<usize>,
        support: Vec<usize>,
        truncated: bool,
    ) -> Self {
        assert!(
            atoms.iter().all(|&a| a < relations.len()),
            "atom out of range"
        );
        assert!(
            facts.iter().all(|f| f.relation < relations.len()),
            "fact relation out of range"
        );
        assert!(
            offsets.first() == Some(&0) && offsets.windows(2).all(|w| w[0] <= w[1]),
            "offsets must start at 0 and not decrease"
        );
        let justifications = offsets.last().copied().unwrap_or(0);
        assert_eq!(
            support.len(),
            justifications * atoms.len(),
            "support length"
        );
        assert!(
            support.iter().all(|&f| f < facts.len()),
            "support fact out of range"
        );
        Self {
            relations,
            atoms,
            facts,
            offsets,
            support,
            truncated,
        }
    }

    /// The number of rows this provenance covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// Whether it covers no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The distinct relation names referenced.
    #[must_use]
    pub fn relations(&self) -> &[String] {
        &self.relations
    }

    /// The relation of each support column: one per positive body atom, in body
    /// order.
    #[must_use = "the iterator borrows the provenance; iterate it"]
    pub fn atoms(&self) -> impl ExactSizeIterator<Item = &str> {
        self.atoms.iter().map(|&a| self.relations[a].as_str())
    }

    /// Every distinct supporting fact, each once.
    #[must_use]
    pub fn facts(&self) -> &[SupportFact] {
        &self.facts
    }

    /// Fact `i`, resolved, if it exists.
    #[must_use]
    pub fn fact(&self, i: usize) -> Option<FactView<'_>> {
        self.facts.get(i).map(|f| FactView {
            relation: &self.relations[f.relation],
            tuple: &f.tuple,
        })
    }

    /// Row `row`'s justifications (empty if out of range).
    #[must_use = "the iterator borrows the provenance; iterate it"]
    pub fn justifications(&self, row: usize) -> impl ExactSizeIterator<Item = Justification<'_>> {
        let k = self.atoms.len();
        let (start, end) = if row < self.len() {
            (self.offsets[row], self.offsets[row + 1])
        } else {
            (0, 0)
        };
        (start..end).map(move |j| Justification {
            provenance: self,
            facts: &self.support[j * k..(j + 1) * k],
        })
    }

    /// Whether some row may have justifications that were not returned (the
    /// per-row cap was reached, or the row cap stopped evaluation). `false`
    /// means every row's evidence is complete.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }
}

/// One way a row was produced: the body facts that joined to yield it, one per
/// positive body atom, in body order.
#[derive(Debug, Clone, Copy)]
pub struct Justification<'a> {
    provenance: &'a Provenance,
    facts: &'a [usize],
}

impl<'a> Justification<'a> {
    /// The supporting facts' indices into [`Provenance::facts`].
    #[must_use]
    pub fn fact_ids(&self) -> &'a [usize] {
        self.facts
    }

    /// The supporting facts, resolved, in body order.
    #[must_use = "the iterator borrows the provenance; iterate it"]
    pub fn support(&self) -> impl ExactSizeIterator<Item = FactView<'a>> + 'a {
        let provenance = self.provenance;
        self.facts.iter().map(move |&f| {
            let fact = &provenance.facts[f];
            FactView {
                relation: &provenance.relations[fact.relation],
                tuple: &fact.tuple,
            }
        })
    }
}
