//! A browser [`WasmExecutor`] for `ascent-jit`.
//!
//! `ascent-jit` lowers each expression to a tiny, import-free WebAssembly module
//! and runs it through a swappable executor. The default executor uses
//! `wasmtime`, a native runtime that cannot run inside a browser-wasm context.
//! This crate plugs the **browser's own `WebAssembly` engine** into that seam
//! ([`WebExecutor`]) so a fact base — and the `fact-query` substrate over it —
//! can be evaluated in place, in the page, with no native dependency.
//!
//! The module *encoding* is reused unchanged from `ascent-jit`; only execution
//! differs. Each value crosses the JS boundary as an `i64` (carried by a JS
//! `BigInt`, per the WebAssembly BigInt-integration the function's `i64`
//! signature triggers). The pure interpreter in `ascent-jit` remains the
//! differential oracle: because the executed bytes are identical, this executor
//! is pinned to the same semantics.
//!
//! ```ignore
//! // In a wasm32 browser target:
//! let mut engine = ascent_jit_web::engine_from_source(
//!     "relation edge(int, int);
//!      relation path(int, int);
//!      path(x, y) <-- edge(x, y);
//!      path(x, z) <-- edge(x, y), path(y, z);",
//! )?;
//! engine.add_fact("edge", vec![Value::Int(1), Value::Int(2)])?;
//! engine.run()?;
//! ```
//!
//! Note there is no fuel metering here (the browser engine offers none); it is
//! unnecessary because the modules `ascent-jit` emits are loop-free and
//! terminate structurally.
#![cfg(target_arch = "wasm32")]

use ascent_jit::{Engine, Error, ExprError, WasmEval, WasmExecutor};
use js_sys::{Array, BigInt, Function, Object, Reflect, Uint8Array, WebAssembly};
use wasm_bindgen::{JsCast, JsValue};

/// A [`WasmExecutor`] backed by the browser's `WebAssembly` engine.
///
/// Instantiates each encoded module synchronously
/// (`WebAssembly.Module`/`Instance`) and calls its exported `f`. The imports
/// object is empty — `ascent-jit`'s modules import nothing.
#[derive(Debug)]
pub struct WebExecutor {
    imports: Object,
}

impl WebExecutor {
    /// Creates a browser executor with an empty (import-free) imports object.
    #[must_use]
    pub fn new() -> Self {
        WebExecutor {
            imports: Object::new(),
        }
    }
}

impl Default for WebExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmExecutor for WebExecutor {
    /// The module's exported `f`, retained for repeated calls.
    type Module = Function;

    fn instantiate(&mut self, bytes: &[u8]) -> Result<Self::Module, ExprError> {
        let source = Uint8Array::from(bytes);
        let module = WebAssembly::Module::new(source.as_ref()).map_err(|e| js_err(&e))?;
        let instance = WebAssembly::Instance::new(&module, &self.imports).map_err(|e| js_err(&e))?;
        let exports = instance.exports();
        let f = Reflect::get(exports.as_ref(), &JsValue::from_str("f")).map_err(|e| js_err(&e))?;
        f.dyn_into::<Function>()
            .map_err(|_| ExprError::Runtime("export `f` is not a function".to_owned()))
    }

    fn call(&mut self, module: &Self::Module, args: &[i64]) -> Result<i64, ExprError> {
        let js_args = Array::new();
        for &arg in args {
            js_args.push(&BigInt::from(arg).into());
        }
        let result = module.apply(&JsValue::NULL, &js_args).map_err(|e| js_err(&e))?;
        let bigint = result
            .dyn_into::<BigInt>()
            .map_err(|_| ExprError::Runtime("result `f` did not return a BigInt".to_owned()))?;
        i64::try_from(bigint).map_err(|_| ExprError::Runtime("result out of i64 range".to_owned()))
    }
}

/// Parses `src` and builds an [`Engine`] whose expression tier runs on the
/// browser's `WebAssembly` engine via [`WebExecutor`].
///
/// This is the in-browser counterpart to `ascent_jit::Engine::from_source`. Use
/// the returned engine — and any `fact-query` `FactStore` layered on it —
/// exactly as on native.
///
/// # Errors
///
/// Returns [`Error`] if `src` fails to parse or validate.
pub fn engine_from_source(src: &str) -> Result<Engine, Error> {
    let evaluator = WasmEval::with_executor(WebExecutor::new());
    Engine::from_source_with_evaluator(src, Box::new(evaluator))
}

/// Renders a `WebAssembly` API `JsValue` error as an [`ExprError::Runtime`].
fn js_err(value: &JsValue) -> ExprError {
    ExprError::Runtime(value.as_string().unwrap_or_else(|| format!("{value:?}")))
}
