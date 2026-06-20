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
//! - **Encoding** (`encode_module`) — pure: it turns a set of [`Expr`]s into the
//!   bytes of a **single** module exporting one function `f{i}(i64..) -> i64`
//!   per expression. It has no runtime dependency and compiles to any target,
//!   `wasm32` included. The bytes of each function depend only on the
//!   expression's structure and the order of its free variables, never on their
//!   types (see ADR 0005), so one function per distinct [`Expr`] suffices.
//! - **Execution** — the swappable [`WasmExecutor`] seam: it instantiates those
//!   bytes and calls an exported function by name. [`WasmtimeExecutor`] is the
//!   default, native backend (behind the `wasmtime` feature); a browser host can
//!   supply its own executor over the platform `WebAssembly` engine to evaluate
//!   in place. [`WasmEval`] is the [`ExprEval`] adapter that owns one compiled
//!   module over an executor, extending it as new expressions appear.
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

/// Runs an encoded, import-free WASM module that exports one function
/// `f{i}(i64..) -> i64` per expression.
///
/// This is the seam that makes the expression runtime swappable. The module
/// *encoding* (`encode_module`) is shared and pure; an executor only decides
/// how the resulting bytes are instantiated and which exported function a call
/// selects. [`WasmtimeExecutor`] is the default native implementation; a browser
/// host can implement this over the platform `WebAssembly` engine so fact
/// queries evaluate in place without a native runtime.
///
/// Implementations marshal each argument as an `i64` (integers directly,
/// booleans as `0`/`1`, symbols as their interned id) and return the selected
/// function's single `i64` result; the caller re-tags it using the expression's
/// inferred [`Type`].
pub trait WasmExecutor {
    /// A compiled module, instantiated and ready to have any of its exported
    /// functions called repeatedly (compile-once / call-many — [`WasmEval`]
    /// keeps a single instance covering every expression seen so far).
    type Module;

    /// Instantiates the module `bytes` and returns a handle to the instance.
    ///
    /// # Errors
    ///
    /// Returns [`ExprError::Runtime`] if the bytes fail to compile or
    /// instantiate.
    fn instantiate(&mut self, bytes: &[u8]) -> Result<Self::Module, ExprError>;

    /// Calls the previously instantiated `module`'s exported function `func`
    /// with `args` and returns its `i64` result.
    ///
    /// # Errors
    ///
    /// Returns [`ExprError::Runtime`] if `func` is not exported, or the call
    /// traps or returns an unexpected result.
    fn call(&mut self, module: &Self::Module, func: &str, args: &[i64]) -> Result<i64, ExprError>;
}

/// The export name of the function for expression index `i` (`f0`, `f1`, …).
fn func_name(index: usize) -> String {
    format!("f{index}")
}

/// Lowers expressions to WASM and evaluates them over a pluggable
/// [`WasmExecutor`].
///
/// All known expressions live in **one** module exporting `f{i}` per expression
/// (ADR 0005). The module is (re-)instantiated whenever a not-yet-seen
/// expression is registered — eagerly in bulk via [`WasmEval::prime`], or one at
/// a time when [`eval_expr`](ExprEval::eval_expr) meets an ad-hoc query
/// expression. Because a function's bytes are type-independent, there is exactly
/// one function per distinct [`Expr`]; the free-variable types only choose how
/// the `i64` result is re-tagged, which is cached as metadata.
pub struct WasmEval<E: WasmExecutor> {
    executor: E,
    /// Distinct expressions, in stable index order; expression `i` is exported
    /// as `f{i}`. Parallel to `params`.
    exprs: Vec<Expr>,
    /// The free variables of each expression, in [`Expr::collect_vars`] order —
    /// the argument order for `f{i}`. Parallel to `exprs`.
    params: Vec<Vec<Symbol>>,
    /// `Expr` → its index in `exprs`.
    index: HashMap<Expr, usize>,
    /// The single instance covering `f0..f{exprs.len()-1}`; `None` until the
    /// first expression is registered.
    module: Option<E::Module>,
    /// Cached result types for re-tagging, keyed on `(expression index, free-var
    /// types in `params` order)`. The same expression re-tags differently when
    /// its variables bind columns of different types.
    result_ty: HashMap<(usize, Vec<Type>), Type>,
}

impl<E: WasmExecutor> std::fmt::Debug for WasmEval<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmEval")
            .field("functions", &self.exprs.len())
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
            exprs: Vec::new(),
            params: Vec::new(),
            index: HashMap::new(),
            module: None,
            result_ty: HashMap::new(),
        }
    }

    /// Registers `expr` (computing its argument order) without rebuilding, and
    /// returns whether it was newly added.
    fn register(&mut self, expr: &Expr) -> bool {
        if self.index.contains_key(expr) {
            return false;
        }
        let i = self.exprs.len();
        let mut params = Vec::new();
        expr.collect_vars(&mut params);
        self.exprs.push(expr.clone());
        self.params.push(params);
        self.index.insert(expr.clone(), i);
        true
    }

    /// Re-encodes and re-instantiates the single module over every registered
    /// expression.
    fn rebuild(&mut self) -> Result<(), ExprError> {
        let bytes = encode_module(&self.exprs, &self.params);
        self.module = Some(self.executor.instantiate(&bytes)?);
        Ok(())
    }

    /// Ensures `expr` has a compiled function, rebuilding the module if it was
    /// not already present, and returns its index.
    fn ensure(&mut self, expr: &Expr) -> Result<usize, ExprError> {
        if self.register(expr) {
            self.rebuild()?;
        }
        Ok(self.index[expr])
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
    fn prime(&mut self, exprs: &[&Expr]) -> Result<(), ExprError> {
        let mut added = false;
        for expr in exprs {
            added |= self.register(expr);
        }
        if added {
            self.rebuild()?;
        }
        Ok(())
    }

    fn eval_expr(&mut self, expr: &Expr, env: &HashMap<Symbol, Value>) -> Result<Value, ExprError> {
        let i = self.ensure(expr)?;
        // Marshal arguments in the function's parameter order and read off the
        // free-variable types for the result-type cache key.
        let mut param_types = Vec::with_capacity(self.params[i].len());
        let mut args = Vec::with_capacity(self.params[i].len());
        for p in &self.params[i] {
            let v = env.get(p).ok_or(ExprError::UnboundVar(*p))?;
            param_types.push(v.type_of());
            args.push(v.to_bits());
        }
        let result_ty = self.result_type(i, param_types)?;
        let func = func_name(i);
        // `module` and `executor` are disjoint fields, so the shared borrow of
        // the instance coexists with the `&mut` call into the executor.
        let module = self.module.as_ref().expect("module built by ensure");
        let bits = self.executor.call(module, &func, &args)?;
        Ok(Value::from_bits(bits, result_ty))
    }
}

impl<E: WasmExecutor> WasmEval<E> {
    /// Returns the [`Type`] to re-tag `f{index}`'s `i64` result with, given its
    /// free-variable `param_types` (in `params` order), inferring and caching it
    /// on first use.
    fn result_type(&mut self, index: usize, param_types: Vec<Type>) -> Result<Type, ExprError> {
        let key = (index, param_types);
        if let Some(ty) = self.result_ty.get(&key) {
            return Ok(*ty);
        }
        let type_env: HashMap<Symbol, Type> = self.params[index]
            .iter()
            .copied()
            .zip(key.1.iter().copied())
            .collect();
        let ty = self.exprs[index].infer(&type_env)?;
        self.result_ty.insert(key, ty);
        Ok(ty)
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
    type Module = wasmtime::Instance;

    fn instantiate(&mut self, bytes: &[u8]) -> Result<Self::Module, ExprError> {
        let module = wasmtime::Module::new(&self.engine, bytes).map_err(runtime)?;
        wasmtime::Instance::new(&mut self.store, &module, &[]).map_err(runtime)
    }

    fn call(&mut self, module: &Self::Module, func: &str, args: &[i64]) -> Result<i64, ExprError> {
        let f = module
            .get_func(&mut self.store, func)
            .ok_or_else(|| ExprError::Runtime(format!("missing export `{func}`")))?;
        let args: Vec<wasmtime::Val> = args.iter().map(|&a| wasmtime::Val::I64(a)).collect();
        self.store.set_fuel(FUEL_PER_CALL).map_err(runtime)?;
        let mut results = [wasmtime::Val::I64(0)];
        f.call(&mut self.store, &args, &mut results)
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

/// Encodes `exprs` as a single WASM module exporting one function
/// `f{i}(params...) -> i64` per expression. `params[i]` is the argument order
/// (free variables) of `exprs[i]`; the two slices are parallel.
fn encode_module(exprs: &[Expr], params: &[Vec<Symbol>]) -> Vec<u8> {
    let mut module = Module::new();

    // One function type per expression — arities differ, and a distinct type per
    // function keeps the index alignment (type i ↔ function i ↔ export `f{i}`)
    // trivial.
    let mut types = TypeSection::new();
    for p in params {
        types
            .ty()
            .function(vec![ValType::I64; p.len()], vec![ValType::I64]);
    }
    module.section(&types);

    let mut functions = FunctionSection::new();
    for i in 0..exprs.len() {
        functions.function(idx(i));
    }
    module.section(&functions);

    let mut exports = ExportSection::new();
    for i in 0..exprs.len() {
        exports.export(&func_name(i), ExportKind::Func, idx(i));
    }
    module.section(&exports);

    let mut codes = CodeSection::new();
    for (expr, p) in exprs.iter().zip(params) {
        let mut f = Function::new(Vec::new());
        let mut instrs = Vec::new();
        emit(expr, p, &mut instrs);
        for instr in &instrs {
            f.instruction(instr);
        }
        f.instruction(&Instruction::End);
        codes.function(&f);
    }
    module.section(&codes);

    module.finish()
}

/// A `usize` index narrowed to the `u32` the WASM sections use.
fn idx(i: usize) -> u32 {
    u32::try_from(i).expect("function index fits u32")
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
