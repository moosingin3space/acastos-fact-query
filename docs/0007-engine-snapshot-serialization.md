# 0007: Serializing an engine to a re-materializable snapshot

- **Status:** Proposed
- **Crate:** `ascent-jit` (inherited for free by `fact-query`; portable to the
  `wasm32` consumers `ascent-jit-web` / `fact-query-node`)
- **Builds on:** [0001](0001-ascent-jit-runtime-engine.md) (the engine state and
  the fixed point), [0004](0004-pluggable-wasm-execution.md) /
  [0005](0005-single-wasm-module.md) (the expression module is pure-derivable
  from the IR)

## Context

Building an [`Engine`] is not free. From source text the constructor parses
(`parser.rs` is the largest file in the crate), validates and stratifies, primes
the WASM expression tier (encode + instantiate, ADR 0005), and then — for a
useful fact base — the caller runs the program to a fixed point, which is the
dominant cost over any non-trivial set of facts. A propose-then-verify loop, a
browser tab reloading a fact base, or a Node process answering successive
queries (docs/0006) wants to pay that *once* and re-materialize a ready-to-query
engine cheaply thereafter.

So we want to serialize an engine to an efficient on-disk/over-the-wire form and
re-materialize it without redoing the expensive work. The request is usually
phrased "serialize the compiled wasm plus the fact database." This record argues
that the right artifact is **not** quite that — it is the *logical* state, from
which the wasm is regenerated — and specifies that format.

## What is actually in an engine, and what is expensive

The engine holds four things:

| Field | Re-materialization cost if discarded | Serializable? |
|-------|--------------------------------------|---------------|
| `program: Program` (IR) | re-parse + re-stratify | yes — plain data |
| `interner: Interner` | n/a (it *is* the identity of every `Sym`) | yes — `Vec<String>` |
| `db: Database` (materialized tuples) | **re-run the fixed point** (dominant) | yes — `Value` is closed + `Copy` |
| `evaluator: Box<dyn ExprEval>` | re-encode (pure) + re-instantiate (Cranelift / `WebAssembly`) | **no** — a live `wasmtime::Instance` / JS object |

Two observations drive the decision.

**The wasm is pure-derivable from the IR.** Per ADR 0005, the module bytes
`encode_module` emits depend only on the program's distinct `Expr`s and their
free-variable order — never on types, never on facts. So the entire expression
tier is reconstructable from `program.exprs()` by the same `prime` call the
constructor already makes. Re-encoding is allocation-only and fast; the only
non-trivial step is the backend instantiating the bytes (Cranelift compiling a
handful of straight-line arithmetic functions), and even that is small beside
re-parsing and re-running.

**Symbols are interner-relative.** A tuple is `Vec<Value>` and a `Value::Sym`
is an index into *one* `Interner` (`value.rs`). Tuples are meaningless without
the interner that minted their symbols, so the interner is not optional metadata
— it is load-bearing identity and must travel in the same artifact, canonically
ordered.

## Decision

Serialize the **logical snapshot** — IR + interner + database — into a compact,
self-describing, content-addressed binary. Re-materialization rebuilds the
engine structurally, **regenerates** the wasm via the existing `prime` path, and
loads the materialized tuples directly, so it skips parsing, stratification, and
the fixed-point run. The live executor instance is rebuilt, never stored.

1. **The artifact is IR + interner + database. The wasm is regenerated, not
   stored.** Storing module bytes would be redundant with the IR (they are a
   pure function of it) and could *drift* from it; regenerating on load
   guarantees the bytes match the program by construction. The expensive thing we
   are actually buying back is the **fixed point** — the `db`'s derived tuples —
   not the compile. We therefore serialize the full `Database` (base *and*
   derived tuples) and re-materialize it without calling `run`.

2. **A framed, versioned, content-addressed container.** A fixed magic, a format
   version, and the crate's semantic version head the file; the body is the
   interner (as an ordered `Vec<String>`, so symbol ids are positional and need
   no remap), the program IR, and one section per relation of its tuples (and
   lattice map). A trailing hash over the canonical encoding makes the artifact
   tamper-evident. Serialization is behind a `serde`-based, **feature-gated**
   optional dependency (mirroring how `wasmtime` is optional), so the
   `--no-default-features` / `wasm32` builds stay lean; `serde` itself compiles
   everywhere, so the format is portable across native, browser, and Node.

3. **Load is fail-closed (per the engine's safety posture).** A wrong magic,
   an unknown/newer format version, a hash mismatch, or a relation whose name or
   arity is absent from the loaded IR is a **load error**, never a silently
   partial engine. There is no "best-effort" restore.

4. **Two load modes, trusted and verified.**
   - `deserialize` — trusts the snapshot's tuples as a faithful fixed point and
     loads them directly. This is the in-process save/restore path: you produced
     the bytes, you trust them, you skip the run.
   - `deserialize_verified` — loads the IR + base facts, *re-runs the fixed
     point*, and asserts the result equals the serialized `db`. A tampered or
     stale snapshot becomes a load error rather than a wrong answer. This is the
     path for an artifact from an untrusted producer.

   The producer is responsible for running to a fixed point before serializing if
   it wants a materialized snapshot; the format records whether it did, so a
   consumer is never misled into trusting an un-run database as if it were
   closed.

5. **Only program-derived expression state is captured.** `WasmEval` may have
   accreted ad-hoc query expressions at runtime beyond `program.exprs()`. These
   are *not* serialized; on load only the program's expressions are primed, and
   an ad-hoc query re-registers its expressions lazily on next use — exactly as a
   fresh engine does. The snapshot is a property of the *program and its facts*,
   not of a session's query history.

## Why precompiled native code (`.cwasm`) is out of scope

The literal reading of "serialize the compiled wasm" is `wasmtime`'s
`Module::serialize` — Cranelift's machine-code output, loadable via
`Module::deserialize` to skip compilation. We deliberately exclude it, and the
arguments are worth stating because they are the strongest case *against* this
record's framing:

- **It reintroduces the trust boundary the structural sandbox exists to remove.**
  ADR 0001/0004 sandbox LLM-authored expressions *structurally* — import-free
  modules, no `unsafe`. `Module::deserialize` loads native machine code and is
  documented as `unsafe` precisely because a malformed/hostile `.cwasm` is
  arbitrary code execution. The workspace denies `unsafe_code` crate-wide; a
  native-code loader could not even live in these crates without a denied
  `#[allow]`. Loading native code from a file is a categorically larger trust
  decision than loading data.
- **It is non-portable.** A `.cwasm` is keyed to the exact `wasmtime` version,
  `Config`, and target triple. It is useless to the browser/Node `WebAssembly`
  backends, and stale across a `wasmtime` bump — the opposite of a durable
  snapshot.
- **It buys back the cheap part.** It skips Cranelift on a few tiny arithmetic
  functions, while the snapshot already skips the parse and the fixed-point run.

If a native-code cache is ever justified by measurement, it belongs **outside**
the no-`unsafe` core — in an isolated, workspace-excluded crate behind the
executor seam (as `ascent-jit-web` is excluded) — as an *untrusted cache only*:
keyed to `(wasmtime version, target, module-bytes hash)`, and on any mismatch or
validation failure it falls back to recompiling from the encoded bytes, which are
always reconstructable. It is never a source of semantics, only of speed. This
record does not adopt it.

## Why this keeps every invariant

- **The value model stays closed and `Copy` (0001/0002).** `Value` is
  `{Int | Bool | Sym}`; serializing it is three small cases and re-materializing
  is exact. Nothing in the format widens or reinterprets it.
- **Governance-free boundary intact (0002).** The artifact is program + symbols +
  tuples. It carries no denial/trust/origin vocabulary — there is none in these
  crates to carry. Policy that layers on top serializes its own state on top;
  nothing leaks down.
- **Provenance stays the engine's and is recomputed, not stored.** `explain` and
  query justifications are derived on demand from the materialized `db` (ADR
  0001); the snapshot stores only the tuples, so provenance after a load is
  recomputed identically and cannot go stale.
- **Fail closed.** Every ambiguous load outcome is an error, and
  `deserialize_verified` turns a database that is *not* a true fixed point under
  the rules into a load error rather than a wrong (and silently under-derived)
  answer — the same posture as treating an indeterminate evaluation as denial.
- **Bound every operation.** Deserialization reads framed, length-prefixed
  sections and is bounded by the declared sizes, with a cap so a malformed length
  cannot drive an unbounded allocation — load is not a DoS vector.
- **The differential oracle is untouched.** Nothing about the bytes the engine
  executes changes; they are regenerated by the same encoder, and
  `deserialize_verified` re-runs the same evaluator, so the interp-vs-wasm
  guarantee is unaffected.

## Consequences

- **`fact-query` gets persistence for free.** Its `FactStore` is implemented
  directly on `Engine` with no extra persistent state, so serializing the engine
  serializes the whole substrate. The browser/Node bindings (docs/0006) can
  persist a `FactEngine` and reload it across page loads / process restarts using
  the same portable bytes.
- **Re-materialization cost drops to encode + instantiate.** Load skips parse,
  stratify, and the fixed-point run; it pays only the pure re-encode and one
  backend instantiation — the same `prime` the constructor runs, minus
  everything else.
- **Round-trip testing has an obvious oracle.** `serialize` then `deserialize`
  must produce an engine whose every relation queries equal to the original's,
  and `deserialize_verified` must agree with a from-scratch `run`. Both fold
  naturally into the existing `arbtest`/differential harness.
- **Cost.** One optional `serde` dependency and a feature flag; a versioned
  binary format to maintain (forward-compat handled by refusing unknown
  versions); and the discipline that the format is logical state only — if the IR
  or value model grows, the format and its version grow with it, while compiled
  artifacts remain regenerated, never stored.
