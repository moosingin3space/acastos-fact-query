# 0001: A runtime interpreter for Ascent

- **Status:** Accepted
- **Crate:** `ascent-jit`

## Context

[Ascent](https://github.com/s-arash/ascent) is an excellent Datalog engine for
Rust, but it is a **compile-time tool**. The `ascent!{ … }` proc macro parses a
fixed program with `syn` and expands it into a concrete struct — relations become
typed, indexed fields and `run()` is a monomorphized, semi-naïve, stratified
fixed-point loop specialized to *those exact rules and types*. The program is
frozen at `rustc` time.

That is the wrong shape for a system whose **rules change at runtime** — where
some upstream process (a user, a generative model, a configuration source) adds,
edits, and retracts rules and facts during execution, and the engine must reason
over the new ruleset immediately. A compile-time macro cannot express "here is a
rule I just received as a string; evaluate it now."

`ascent-jit` fills that gap: it takes an Ascent program **as data** and evaluates
it at runtime, without invoking the compiler.

## Decision drivers

- **Runtime-authored rules.** Rules arrive as data during execution; no `rustc`
  in the request path.
- **Deterministic, inspectable evaluation.** Same facts + same rules ⇒ same
  derivations, every time, and it must be possible to explain *why* a fact was
  derived.
- **Fidelity to Ascent semantics.** Stratified negation, aggregation, and
  lattices are what make Ascent more than toy Datalog and are load-bearing for
  real programs. The interpreter should match the macro's semantics, not a subset
  that quietly diverges.
- **Safety.** When rules can be authored by an untrusted producer, expression
  evaluation must not be able to execute arbitrary code or wedge the engine. This
  is obtained *structurally* — by compiling rule expressions to WebAssembly and
  running them under a fuel-metered sandbox with no host imports — rather than by
  hand-restricting a tiny expression language.
- **Latency.** Re-evaluation should be milliseconds, not seconds, so that tight
  propose/evaluate loops are practical.

## Decision

The **relational core is an interpreter**: a tree-walk of a relational IR with
semi-naïve evaluation, because that is the shortest path to correct, faithful
Ascent semantics. The **"JIT" is real but scoped to expressions**: the
conditions and bindings in `if`/`let` clauses are lowered to WebAssembly and
executed by [`wasmtime`](https://wasmtime.dev/), whose Cranelift backend
JIT-compiles them to machine code. Compiling whole *rule bodies* (joins included)
to WASM is a deliberately deferred, ambitious tier — not part of this design.

The crate provides:

1. **A program IR** — a runtime data model for an Ascent program: relations (with
   a column schema and a relation/lattice kind), rules (a head clause plus a body
   of positive atoms, negated atoms, `if` conditions, `let` bindings, and
   aggregations), and an expression type for conditions and bindings. The IR is
   the single source of truth; both the textual parser and any programmatic
   producer target it.

2. **A dynamically-typed value model.** Because schemas are not known until
   runtime, tuples are `Vec<Value>` where `Value` is a closed, `Copy` enum
   (`Int(i64)`, `Bool`, interned `Sym`). This is the central performance tax
   versus the macro's monomorphized tuples, accepted in exchange for runtime
   flexibility and mitigated by string interning and per-rule column indexes.

3. **A semi-naïve evaluator** with:
   - **stratification** computed from the rule dependency graph, with negation,
     aggregation, and lattice rules forced into higher strata than the relations
     they consume; a cycle through negation is a load-time error, exactly as the
     macro rejects it at compile time;
   - **indexing** of relations on the bindings each rule needs, rebuilt
     incrementally per iteration;
   - **lattice relations** keyed on their non-lattice columns, joining values via
     a `Lattice` trait (`join`/`bottom`), with `Dual<T>` provided for
     "keep the minimum" cases such as shortest-path;
   - **a bounded fixed point** — a configurable maximum iteration count on the
     semi-naïve loop. Because rules can arrive at runtime, a non-terminating
     program (`foo(x+1) <-- foo(x)`, an unbounded lattice) is reachable by
     construction, and WASM fuel bounds an *expression*, not the loop. Exceeding
     the bound **aborts with an error** rather than hanging or exhausting memory.
     The abort is **indeterminate** and must be treated fail-closed by any
     safety-conscious caller — never silently as "nothing derived."

4. **A textual frontend** that mirrors Ascent's surface syntax
   (`relation foo(int);`, `head(x) <-- body(x), if cond;`) for fixtures, tests,
   and human authoring, alongside the programmatic IR path.

5. **A query + provenance API.** Load a program, assert facts, `run()` to a fixed
   point, query a relation, and — critically — ask **why** a tuple is present:
   the rule and the body tuples that fired it. Provenance is a first-class
   requirement, not an afterthought, because callers need it to explain and to
   trust derived results. It is recomputed on demand, so it carries no per-run
   tracking cost when unused.

6. **A WebAssembly expression sandbox.** Expressions are lowered from the IR
   directly to WASM bytecode (via `wasm-encoder` — *not* by shelling out to a
   Rust→WASM compile, which would reintroduce recompile latency) and executed by
   `wasmtime` with **fuel metering** (an expression cannot wedge the fixed-point
   loop) and **no host imports** (no I/O, no syscalls, no arbitrary code). This is
   simultaneously the sandbox and the "JIT": Cranelift compiles the modules to
   machine code and compiled modules are cached, keyed by expression. The sandbox
   is what lets the engine offer a *rich* expression language safely instead of a
   hand-built total one. A pure tree-walking interpreter for the same expression
   IR is kept as the differential oracle for the WASM tier (and as an alternative
   backend).

7. **Speculative ("hypothetical") evaluation.** Given a loaded program, cheaply
   fork it, assert candidate facts, run to a fixed point, inspect the result
   (e.g. "is any `violation(...)` tuple now derivable?"), and discard the fork
   without mutating the canonical program. This `fork / assert / run / discard`
   operation is the load-bearing primitive for the propose/verify substrate built
   on top (see [0002](0002-fact-query-substrate.md)).

## Considered alternatives

- **Codegen + recompile** (generate an `ascent!{}` block, compile to a `cdylib`,
  load it). Maximum fidelity and speed — it *is* Ascent — but multi-second
  latency per ruleset change and a toolchain in the request path; native dynamic
  loading also needs `unsafe`, which this workspace forbids. Rejected as the
  primary path. Note this is **distinct** from the WASM expression tier adopted
  here, which emits bytecode directly from the IR with no compiler invocation.
- **Embed a different runtime Datalog engine.** Avoids reinventing evaluation,
  but means reconciling a different engine's semantics with Ascent's lattices,
  aggregation, and stratification rules. Rejected to keep one semantics — though
  benchmarking against such an engine remains worthwhile.
- **A hand-built total expression language** instead of the WASM sandbox. The
  sandbox lets the expression language be generous (fuel + no imports make even a
  Turing-complete expression safe) without enumerating a safe subset by hand.

## Consequences

### Positive

- Rules become runtime data, which is the entire premise of the engine.
- The engine, not any upstream proposer, is the authority on what is true; a rule
  that does not type-check or breaks stratification never enters the ruleset.
- A clean crate boundary: `ascent-jit` is useful and testable with no application
  involvement, and can be differential-tested against the real `ascent!` macro.
- Safety is structural — expression sandboxing rides on `wasmtime`'s fuel and
  capability model rather than on enumerating a safe subset.

### Negative / risks

- **Dynamic-typing tax.** `Vec<Value>` tuples with enum dispatch are materially
  slower than monomorphized tuples — plausibly several-fold on join-heavy
  workloads — from boxing, hashing `Value`, and lost cache locality. Mitigated by
  interning and indexes; the honest number wants benchmarking against the macro.
- **Expressions are a sandboxed language, not Rust.** The macro lets `if`/`let`
  run arbitrary Rust; the WASM tier cannot, and given untrusted rules, must not.
  The sandbox is far more generous than a hand-built total language but is still
  not Rust (no user crates, no host effects). This is a real divergence from
  `ascent!`; the lowering must reject what it cannot compile rather than
  mis-evaluate.
- **WASM boundary and instantiation cost.** Every expression evaluation crosses
  the host↔WASM boundary, and module compile/instantiate has fixed overhead;
  hence compiled-module caching and the pure-interpreter fast path. This same
  per-crossing cost is what makes compiling whole joins to WASM a hard, unproven
  win, and why it is deferred.
- **Re-implementing subtle semantics.** Stratification, lattice fixed points, and
  aggregation-in-a-stratum are easy to get *almost* right. The mitigation is a
  differential test harness that runs the same program through `ascent!` and
  through `ascent-jit` and asserts identical relations.

### Neutral

- The crate is named `ascent-jit`. The relational core is interpreted; the name
  is earned by the WASM/Cranelift expression tier and points at the eventual
  whole-rule-body JIT.
