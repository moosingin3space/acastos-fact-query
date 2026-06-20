# 0002: The `fact-query` proposer/verifier substrate

- **Status:** Accepted
- **Crate:** `fact-query`
- **Builds on:** [0001](0001-ascent-jit-runtime-engine.md) — the engine
  capabilities this stands on: speculative `fork / assert / run / discard`
  evaluation, provenance, and the bounded fixed point.

## Context

A recurring pattern shows up wherever a generative or otherwise untrusted
component proposes something a deterministic engine must vet:

- **a proposed change** — apply a candidate rule or fact change to a fork, run to
  a fixed point, diff, and surface what it derives;
- **a constraint check** — assert candidate facts on a fork, run, and read off
  whether any denial/`violation` relation fired;
- **a generated query** — synthesize a query against the fact base, evaluate it,
  and return the result set.

All three are one shape: **an untrusted artifact is proposed, the deterministic
engine speculatively evaluates it and reports what it *does* (with provenance),
and some net — a human, a vote, or nothing — decides whether that is what was
wanted.**

The reusable artifact is not any one of those surfaces; it is the
proposer/verifier primitive itself, made **governance-free** — carrying no
opinion about who may write, what is denied, or what a verdict means. That is
what lets the same primitive serve very different applications over fact bases
whose relations are nothing alike.

## Decision drivers

- **One pattern, several grains — not several code paths.** Facts, queries, and
  rules are different *grains* of the same primitive; expose it once.
- **Governance-free substrate.** The reusable core carries no policy: no
  single-writer rule, no default-deny, no denial vocabulary, no human gate. Those
  are an **opt-in layer** an application adds, because they are exactly what
  differs between applications.
- **An honest contract.** The substrate guarantees a proposal is **well-formed,
  safe, read-only, and resource-bounded** — and explicitly **not** that it
  captures intent. The engine checks *form*; it has no opinion about *meaning*.
- **Provenance is first-class.** With intent-checking pushed onto the caller, the
  one affordance the substrate owes them is *why each result is there* — the raw
  material for whatever net they build.
- **Keep the engine ignorant of policy.** The day the engine knows an
  application's denial vocabulary, the substrate has stopped being general. Policy
  must not leak downward.

## Decision

### A crate, `fact-query`, and what it deliberately does not contain

`fact-query` exposes the proposer/verifier primitive over an `ascent-jit` fact
base. It depends on `ascent-jit` and nothing application-specific. It contains
**no** LLM client, **no** commit path, **no** denial/trust/origin vocabulary, and
**no** human gate. Applications depend on `fact-query`; `fact-query` never depends
on an application. **That dependency direction is the governance-free guarantee** —
a separate crate that cannot name an application's concepts physically cannot
smuggle them downward, where an in-module boundary could quietly reach for them.

### The primitive: `propose → speculate(bounded) → (delta, provenance) → net`

The substrate models one operation at three **grains** of proposal:

- **facts** — "if these tuples were asserted, what fires?"
- **queries** — "what currently-true tuples match this pattern?"
- **rules** — "if this inference existed, what would it derive?"

The contract is identical across grains: the proposal is parsed, form-checked,
evaluated **on a discarded fork under a resource bound**, and returned as a delta
plus provenance. What the caller *does* with the delta — treat a non-empty denial
extension as a refusal, return rows as an answer, show a human the consequences —
is not the substrate's concern.

### v1 scope: the queries grain, conjunctive and form-checks-only

v1 ships exactly one grain — **conjunctive queries** (joins, filters, aggregates
over existing relations; no new derived relations, no recursion, no
negation-as-failure) — under a **form-checks-only** contract. This grain is first
because it is structurally the cleanest:

- **Read-only by language, not by handle.** A conjunctive query has no write
  semantics, so an untrusted generated artifact cannot mutate persisted state at
  the grammar level — no read/write handle dance is needed to defend that.
- **Stratification is a non-issue.** A single non-recursive positive query body
  over already-materialized relations is always stratifiable.
- **Termination is free.** A non-recursive query over finite relations always
  terminates; the only blast radius is *space* (see the contract below).

The **facts** and **rules** grains are deliberately left for later grains of the
same crate, added as their consumers earn them.

### The `FactStore` boundary

The query seam is a trait describing what any fact base must expose to host the
queries grain. Note what is **absent** versus a full rule pipeline: no
stratification step, no rule fork, nothing that commits.

```rust
pub trait FactStore {
    type Schema; type Query; type ResultSet; type Provenance; type Error;

    /// Relations, arity, column types, and doc-strings — for grounding.
    fn schema(&self) -> Self::Schema;

    /// Surface text -> typed conjunctive query IR.
    fn parse_query(&mut self, text: &str) -> Result<Self::Query, Self::Error>;

    /// Form checks only: schema-validity + safety (range-restriction).
    fn check(&self, q: &Self::Query) -> Result<(), Self::Error>;

    /// Read-only, cardinality-bounded evaluation against the current fixed
    /// point. Returns results AND provenance.
    fn eval(&mut self, q: &Self::Query, max: Cardinality)
        -> Result<(Self::ResultSet, Self::Provenance), Self::Error>;
}
```

`ascent-jit` gets the canonical `impl FactStore`. A different backend (another
Datalog engine, a relational store) can implement the same trait; that is what
makes any loop written over it portable.

### The contract — five guarantees, one disclaimer

`fact-query` guarantees, deterministically and with no LLM in the verification
loop, that an evaluated query is:

1. **Parsed** — it is valid IR;
2. **Schema-valid** — every referenced relation exists, arity matches, column
   types match;
3. **Safe / range-restricted** — every output variable and every variable in a
   filter is bound by a positive body literal;
4. **Read-only** — guaranteed by the query class;
5. **Cardinality-bounded** — evaluation cannot blow up space past a supplied cap.

It **explicitly disclaims** the sixth thing a caller might wish for: that the
query *answers the question asked*. The substrate makes no intent-fidelity claim.
This is encoded in **types and docs** — the return is a `ResultSet`, never an
`Answer` — and stated in bold in the crate README so no consumer mistakes one for
the other.

**The bound is cardinality, not time.** Because a conjunctive query always
terminates, the resource limit is not fuel/iterations but a cap on result tuple
count, so that an unfiltered join (a cartesian product) is truncated rather than
allowed to exhaust memory. Hitting the cap is a **first-class outcome**, surfaced,
not an error.

### Provenance is first-class, not a debug extra

`eval` returns provenance as part of its result, always. With intent-checking
delegated to the caller, *which facts joined to produce each result tuple* is the
only bridge the substrate offers between "a well-formed query ran" and "here is
why to believe the result." Every net a caller might build — show-the-evidence,
paraphrase-and-confirm, N-candidate vote — is built on it. Demoting it to
optional would gut the substrate's usefulness for safety-conscious applications.

### Keeping the substrate clean: provenance is the engine's, policy is the app's

The litmus case for "no policy leaks downward" is provenance versus trust.
*Which facts derived a tuple* is an engine concern — it stays in `ascent-jit` and
is what makes the primitive work. But *trust semantics over provenance* (e.g.
ranking some origins above others, propagating a minimum trust through a
derivation) is an **application** concern and must not be an engine builtin. It is
expressed as a **lattice-valued annotation computed in user rules** (the engine
already supports lattices), so a different application can swap it out or omit it.

## Considered alternatives

- **Keep it inside the application (traits, no separate crate).** Lower-cost, and
  the right call if there were only one host. But the explicit goal is reuse
  across applications, and a crate boundary is what enforces governance-freedom:
  an in-crate module can quietly reach for a policy constant; a separate crate
  that does not depend on the application *cannot*.
- **Generalize the synthesis + grounding (LLM) layer too.** Rejected as unearned.
  Building "the neurosymbolic framework" before a second real application exists
  risks abstractions fitted to imagined needs. The synthesis/grounding layer
  stays an application concern until a second consumer reveals the seams.
- **Allow rules / recursion / negation in v1.** Deferred. It drags in
  stratification recompute, range-restriction over negation, and non-termination
  (fuel bounds), none of which the queries grain needs.
- **Build the intent-net into the library** (a second pass that confirms the
  query "means" the question). Rejected: it would re-import an LLM or a human into
  a substrate valuable precisely for having neither, and oversell the guarantee.
  The net is the caller's; the substrate hands them provenance to build it.

## Consequences

### Positive

- One primitive, stated once: the bespoke speculative-eval call sites collapse to
  grains of a single contract.
- A governance-free, genuinely reusable substrate — other applications get
  "propose-then-verify against a fact base" without inheriting a writer model or
  denial vocabulary, because the crate physically cannot express them.
- An honest, defensible guarantee: "well-formed + safe + read-only + bounded" is
  fully deterministic; the disclaimed intent-fidelity is stated plainly.

### Negative / risks

- **"Valid but wrong" is the dominant residual gap, deliberately not closed in
  the substrate.** A query can parse, type-check, run, and return a clean result
  set that does not mean what was asked — the spurious-program problem that
  dominates the text-to-query literature. The substrate's contribution is
  provenance and an honest disclaimer; the intent-net is the caller's. The clean
  API must not be allowed to read as authoritative about answers.
- **Grounding is the real cost and has a hard precondition.** Writing a correct
  query over an arbitrary schema requires the proposer to know that schema, and
  ideally the *meaning* of relations, not just arity. **Relation doc-strings
  become load-bearing**; a fact base of cryptic names with no descriptions is
  close to ungroundable.
- **Cardinality blow-up is the v1 failure mode.** Untrusted generated joins can
  cartesian-product; the bound must be threaded through *every* `eval` and
  exhaustion surfaced as a first-class outcome. A forgotten cap is a
  memory-exhaustion DoS.
- **The platform trap.** "Useful for a variety of applications" precedes many
  dead frameworks. The mitigation is structural: extract only the earned
  primitive, and let the *second* real application drive any further extraction.
