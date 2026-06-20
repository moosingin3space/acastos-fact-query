//! The WASM expression tier: the part of `ascent-jit` that is actually
//! JIT-compiled.
//!
//! Each `if`/`let`/head expression is lowered from the [`Expr`] IR directly to
//! WebAssembly bytecode (via `wasm-encoder` — no compiler invocation). The
//! module imports **nothing** (no I/O, no syscalls), so an LLM-authored
//! expression is sandboxed structurally rather than by hand.
//!
//! This file splits into two responsibilities:
//!
//! - **Encoding** (`encode_module`) — pure: it turns an [`Expr`] into the
//!   bytes of a single-function module `f(i64..) -> i64`. It has no runtime
//!   dependency and compiles to any target, `wasm32` included.
//! - **Execution** — the swappable [`WasmExecutor`] seam: it instantiates those
//!   bytes and calls `f`. [`WasmtimeExecutor`] is the default, native backend
//!   (behind the `wasmtime` feature); a browser host can supply its own executor
//!   over the platform `WebAssembly` engine to evaluate in place. [`WasmEval`]
//!   is the [`ExprEval`] adapter that caches compiled modules over an executor.
//!
//! Every value is represented as an `i64` inside WASM (integers directly,
//! booleans as `0`/`1`, symbols as their interned id). The host re-tags the
//! result using the expression's inferred [`Type`]. The pure interpreter in
//! [`crate::expr`] is the differential oracle: every executor must agree with
//! it. Because the *bytes* are shared, executors differ only in how they run an
//! identical module — so the oracle pins them all.

use std::collections::HashMap;

use wasm_encoder::{
    BlockType, CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction,
    Module, TypeSection, ValType,
};

use crate::eval::ExprEval;
use crate::expr::{BinOp, Expr, ExprError, UnOp};
use crate::value::{Symbol, Type, Value};

/// Runs an encoded, import-free WASM module that exports a single function
/// `f(i64..) -> i64`.
///
/// This is the seam that makes the expression runtime swappable. The module
/// *encoding* (`encode_module`) is shared and pure; an executor only decides
/// how the resulting bytes are instantiated and called. [`WasmtimeExecutor`] is
/// the default native implementation; a browser host can implement this over the
/// platform `WebAssembly` engine so fact queries evaluate in place without a
/// native runtime.
///
/// Implementations marshal each argument as an `i64` (integers directly,
/// booleans as `0`/`1`, symbols as their interned id) and return the function's
/// single `i64` result; the caller re-tags it using the expression's inferred
/// [`Type`].
pub trait WasmExecutor {
    /// A compiled module, instantiated and ready to be called repeatedly
    /// (compile-once / call-many — [`WasmEval`] caches one per expression shape).
    type Module;

    /// Instantiates the module `bytes` and returns a handle to its exported `f`.
    ///
    /// # Errors
    ///
    /// Returns [`ExprError::Runtime`] if the bytes fail to compile or
    /// instantiate, or do not export `f`.
    fn instantiate(&mut self, bytes: &[u8]) -> Result<Self::Module, ExprError>;

    /// Calls the previously instantiated `module`'s `f` with `args` and returns
    /// its `i64` result.
    ///
    /// # Errors
    ///
    /// Returns [`ExprError::Runtime`] if the call traps or returns an
    /// unexpected result.
    fn call(&mut self, module: &Self::Module, args: &[i64]) -> Result<i64, ExprError>;
}

/// A compiled expression: its executor-specific module handle plus the metadata
/// needed to marshal arguments in and tag the result.
struct Compiled<M> {
    module: M,
    params: Vec<Symbol>,
    result_ty: Type,
}

/// Cache key: an expression *plus* the types of its free variables (in
/// [`Expr::collect_vars`] order). The types are load-bearing — the same
/// expression (e.g. `Var(x)`) can appear in two rules where `x` is bound to
/// columns of different types, which changes how the `i64` result is re-tagged.
/// Keying on the `Expr` alone would alias those and decode with the wrong type.
type CacheKey = (Expr, Vec<Type>);

/// Lowers expressions to WASM and evaluates them over a pluggable
/// [`WasmExecutor`], caching one compiled module per expression shape.
pub struct WasmEval<E: WasmExecutor> {
    executor: E,
    cache: HashMap<CacheKey, Compiled<E::Module>>,
}

impl<E: WasmExecutor> std::fmt::Debug for WasmEval<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmEval")
            .field("cached", &self.cache.len())
            .finish_non_exhaustive()
    }
}

impl<E: WasmExecutor> WasmEval<E> {
    /// Builds an expression evaluator over a given executor.
    ///
    /// This is the executor-swapping entry point: a host that cannot run
    /// `wasmtime` (e.g. a browser-wasm context) supplies its own
    /// [`WasmExecutor`] here and otherwise reuses the whole encoding pipeline.
    pub fn with_executor(executor: E) -> Self {
        WasmEval {
            executor,
            cache: HashMap::new(),
        }
    }

    fn compile(
        &mut self,
        expr: &Expr,
        params: &[Symbol],
        param_types: &[Type],
    ) -> Result<(), ExprError> {
        let type_env: HashMap<Symbol, Type> = params
            .iter()
            .copied()
            .zip(param_types.iter().copied())
            .collect();
        let result_ty = expr.infer(&type_env)?;
        let bytes = encode_module(expr, params);
        let module = self.executor.instantiate(&bytes)?;
        self.cache.insert(
            (expr.clone(), param_types.to_vec()),
            Compiled {
                module,
                params: params.to_vec(),
                result_ty,
            },
        );
        Ok(())
    }
}

#[cfg(feature = "wasmtime")]
impl WasmEval<WasmtimeExecutor> {
    /// Creates a fuel-metered, import-free WASM expression engine backed by
    /// `wasmtime` — the default native executor.
    ///
    /// # Errors
    ///
    /// Returns [`ExprError::Runtime`] if the `wasmtime` engine cannot be built.
    pub fn new() -> Result<Self, ExprError> {
        Ok(WasmEval::with_executor(WasmtimeExecutor::new()?))
    }
}

impl<E: WasmExecutor> ExprEval for WasmEval<E> {
    fn eval_expr(&mut self, expr: &Expr, env: &HashMap<Symbol, Value>) -> Result<Value, ExprError> {
        let mut params = Vec::new();
        expr.collect_vars(&mut params);
        let mut param_types = Vec::with_capacity(params.len());
        for p in &params {
            param_types.push(env.get(p).ok_or(ExprError::UnboundVar(*p))?.type_of());
        }
        let key = (expr.clone(), param_types);
        if !self.cache.contains_key(&key) {
            self.compile(expr, &params, &key.1)?;
        }
        // Build the argument vector and read the result type while borrowing the
        // cache, then drop that borrow before the call — which needs `&mut` on a
        // disjoint field (the executor).
        let (args, result_ty) = {
            let compiled = &self.cache[&key];
            let mut args = Vec::with_capacity(compiled.params.len());
            for p in &compiled.params {
                let v = env.get(p).ok_or(ExprError::UnboundVar(*p))?;
                args.push(v.to_bits());
            }
            (args, compiled.result_ty)
        };
        let bits = self.executor.call(&self.cache[&key].module, &args)?;
        Ok(Value::from_bits(bits, result_ty))
    }
}

/// The default native [`WasmExecutor`]: runs modules under `wasmtime`, whose
/// Cranelift backend compiles them to machine code. Execution is
/// **fuel-metered** so an expression cannot wedge the fixed-point loop.
#[cfg(feature = "wasmtime")]
#[derive(Debug)]
pub struct WasmtimeExecutor {
    engine: wasmtime::Engine,
    store: wasmtime::Store<()>,
}

/// Fuel granted to a single expression evaluation. Generous for the trivial
/// arithmetic and boolean expressions Ascent rules contain, but finite.
///
/// Fuel is a `wasmtime`-specific backstop, not a correctness requirement: the
/// modules `encode_module` emits are straight-line (arithmetic plus
/// `if`/`else` blocks — no loops, calls, or back-edges), so they terminate
/// structurally. An executor on a runtime without fuel is therefore still safe
/// for *these* modules.
#[cfg(feature = "wasmtime")]
const FUEL_PER_CALL: u64 = 100_000;

#[cfg(feature = "wasmtime")]
impl WasmtimeExecutor {
    /// Builds a fuel-metered `wasmtime` engine and store.
    ///
    /// # Errors
    ///
    /// Returns [`ExprError::Runtime`] if the `wasmtime` engine cannot be built.
    pub fn new() -> Result<Self, ExprError> {
        let mut config = wasmtime::Config::new();
        config.consume_fuel(true);
        let engine = wasmtime::Engine::new(&config).map_err(runtime)?;
        let store = wasmtime::Store::new(&engine, ());
        Ok(WasmtimeExecutor { engine, store })
    }
}

#[cfg(feature = "wasmtime")]
impl WasmExecutor for WasmtimeExecutor {
    type Module = wasmtime::Func;

    fn instantiate(&mut self, bytes: &[u8]) -> Result<Self::Module, ExprError> {
        let module = wasmtime::Module::new(&self.engine, bytes).map_err(runtime)?;
        let instance = wasmtime::Instance::new(&mut self.store, &module, &[]).map_err(runtime)?;
        instance
            .get_func(&mut self.store, "f")
            .ok_or_else(|| ExprError::Runtime("missing export `f`".to_owned()))
    }

    fn call(&mut self, module: &Self::Module, args: &[i64]) -> Result<i64, ExprError> {
        let args: Vec<wasmtime::Val> = args.iter().map(|&a| wasmtime::Val::I64(a)).collect();
        self.store.set_fuel(FUEL_PER_CALL).map_err(runtime)?;
        let mut results = [wasmtime::Val::I64(0)];
        module
            .call(&mut self.store, &args, &mut results)
            .map_err(runtime)?;
        match results[0] {
            wasmtime::Val::I64(b) => Ok(b),
            ref other => Err(ExprError::Runtime(format!("unexpected result {other:?}"))),
        }
    }
}

#[cfg(feature = "wasmtime")]
fn runtime(e: impl std::fmt::Display) -> ExprError {
    ExprError::Runtime(e.to_string())
}

/// Encodes `expr` as a single-function WASM module `f(params...) -> i64`.
fn encode_module(expr: &Expr, params: &[Symbol]) -> Vec<u8> {
    let mut module = Module::new();

    let mut types = TypeSection::new();
    let param_types = vec![ValType::I64; params.len()];
    types.ty().function(param_types, vec![ValType::I64]);
    module.section(&types);

    let mut functions = FunctionSection::new();
    functions.function(0);
    module.section(&functions);

    let mut exports = ExportSection::new();
    exports.export("f", ExportKind::Func, 0);
    module.section(&exports);

    let mut codes = CodeSection::new();
    let mut f = Function::new(Vec::new());
    let mut instrs = Vec::new();
    emit(expr, params, &mut instrs);
    for instr in &instrs {
        f.instruction(instr);
    }
    f.instruction(&Instruction::End);
    codes.function(&f);
    module.section(&codes);

    module.finish()
}

/// Emits instructions that leave the `i64` value of `expr` on the stack.
fn emit(expr: &Expr, params: &[Symbol], out: &mut Vec<Instruction<'static>>) {
    match expr {
        Expr::Var(s) => {
            let idx = params
                .iter()
                .position(|p| p == s)
                .expect("variable collected into params");
            out.push(Instruction::LocalGet(
                u32::try_from(idx).expect("param index fits u32"),
            ));
        }
        Expr::Lit(v) => out.push(Instruction::I64Const(v.to_bits())),
        Expr::Unary(UnOp::Neg, e) => {
            out.push(Instruction::I64Const(0));
            emit(e, params, out);
            out.push(Instruction::I64Sub);
        }
        Expr::Unary(UnOp::Not, e) => {
            emit(e, params, out);
            out.push(Instruction::I64Eqz);
            out.push(Instruction::I64ExtendI32U);
        }
        // `&&` and `||` must short-circuit, matching Rust/Ascent semantics: the
        // right operand is only evaluated when the left does not decide the
        // result. Emitting it unconditionally would, for example, trap on
        // `false && (x / 0 == 0)` where the interpreter returns `false`.
        Expr::Binary(BinOp::And, a, b) => emit_short_circuit(a, b, true, params, out),
        Expr::Binary(BinOp::Or, a, b) => emit_short_circuit(a, b, false, params, out),
        Expr::Binary(op, a, b) => {
            emit(a, params, out);
            emit(b, params, out);
            emit_binop(*op, out);
        }
    }
}

/// Emits a short-circuiting boolean connective. Operands are `0`/`1` `i64`s.
fn emit_short_circuit(
    a: &Expr,
    b: &Expr,
    is_and: bool,
    params: &[Symbol],
    out: &mut Vec<Instruction<'static>>,
) {
    emit(a, params, out);
    out.push(Instruction::I64Const(0));
    out.push(Instruction::I64Ne); // i32 condition: `a != 0`
    out.push(Instruction::If(BlockType::Result(ValType::I64)));
    if is_and {
        emit(b, params, out);
        out.push(Instruction::Else);
        out.push(Instruction::I64Const(0));
    } else {
        out.push(Instruction::I64Const(1));
        out.push(Instruction::Else);
        emit(b, params, out);
    }
    out.push(Instruction::End);
}

fn emit_binop(op: BinOp, out: &mut Vec<Instruction<'static>>) {
    match op {
        BinOp::Add => out.push(Instruction::I64Add),
        BinOp::Sub => out.push(Instruction::I64Sub),
        BinOp::Mul => out.push(Instruction::I64Mul),
        BinOp::Div => out.push(Instruction::I64DivS),
        BinOp::Rem => out.push(Instruction::I64RemS),
        // `And`/`Or` are lowered with short-circuit control flow in `emit`.
        BinOp::And | BinOp::Or => unreachable!("and/or are short-circuited in emit"),
        BinOp::Eq => extend(out, Instruction::I64Eq),
        BinOp::Ne => extend(out, Instruction::I64Ne),
        BinOp::Lt => extend(out, Instruction::I64LtS),
        BinOp::Le => extend(out, Instruction::I64LeS),
        BinOp::Gt => extend(out, Instruction::I64GtS),
        BinOp::Ge => extend(out, Instruction::I64GeS),
    }
}

/// Pushes a comparison (which yields `i32` in WASM) and widens it to `i64`.
fn extend(out: &mut Vec<Instruction<'static>>, cmp: Instruction<'static>) {
    out.push(cmp);
    out.push(Instruction::I64ExtendI32U);
}
