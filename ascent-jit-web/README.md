# ascent-jit-web

A browser [`WasmExecutor`](https://docs.rs/ascent-jit) for
[`ascent-jit`](../ascent-jit): it plugs the browser's own `WebAssembly` engine
into `ascent-jit`'s pluggable expression-execution seam, so a Datalog fact base
— and the [`fact-query`](../fact-query) substrate over it — can be evaluated
**in place inside a `wasm32` page**, with no native `wasmtime` dependency.

Only *execution* differs from native: the module encoding is reused unchanged
from `ascent-jit`, and the pure interpreter remains the differential oracle, so
the in-browser executor is pinned to identical semantics. See
[`docs/0004-pluggable-wasm-execution.md`](../docs/0004-pluggable-wasm-execution.md).

## Usage

```rust
let mut engine = ascent_jit_web::engine_from_source(src)?;
engine.add_fact("edge", vec![Value::Int(1), Value::Int(2)])?;
engine.run()?;
```

## Building

This crate is `wasm32`-only and is detached from the parent workspace:

```sh
rustup target add wasm32-unknown-unknown
cargo build -p ascent-jit-web --target wasm32-unknown-unknown
```
