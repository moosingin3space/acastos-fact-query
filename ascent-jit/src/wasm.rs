//! The WASM expression tier: the part of `ascent-jit` that is actually
//! JIT-compiled.
//!
//! Each `if`/`let`/head expression is lowered from the [`Expr`] IR directly to
//! WebAssembly bytecode (via `wasm-encoder` — no compiler invocation) and run
//! under `wasmtime`, whose Cranelift backend compiles it to machine code.
//! Execution is **fuel-metered** (an expression cannot wedge the fixed-point
//! loop) and the module imports **nothing** (no I/O, no syscalls), so an
//! LLM-authored expression is sandboxed structurally rather than by hand.
//!
//! Every value is represented as an `i64` inside WASM (integers directly,
//! booleans as `0`/`1`, symbols as their interned id). The host re-tags the
//! result using the expression's inferred [`Type`]. The pure interpreter in
//! [`crate::expr`] is the differential oracle: the two must always agree.

use std::collections::HashMap;

use wasm_encoder::{
    BlockType, CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction,
    Module, TypeSection, ValType,
};
use wasmtime::{Config, Engine, Func, Instance, Store, Val};

use crate::eval::ExprEval;
use crate::expr::{BinOp, Expr, ExprError, UnOp};
use crate::value::{Symbol, Type, Value};

/// Fuel granted to a single expression evaluation. Generous for the trivial
/// arithmetic and boolean expressions Ascent rules contain, but finite.
const FUEL_PER_CALL: u64 = 100_000;

/// A compiled expression: its WASM function plus the metadata needed to marshal
/// arguments in and tag the result.
struct Compiled {
    func: Func,
    params: Vec<Symbol>,
    result_ty: Type,
}

/// Cache key: an expression *plus* the types of its free variables (in
/// [`Expr::collect_vars`] order). The types are load-bearing — the same
/// expression (e.g. `Var(x)`) can appear in two rules where `x` is bound to
/// columns of different types, which changes how the `i64` result is re-tagged.
/// Keying on the `Expr` alone would alias those and decode with the wrong type.
type CacheKey = (Expr, Vec<Type>);

/// Lowers expressions to WASM and evaluates them under `wasmtime`.
pub struct WasmEval {
    engine: Engine,
    store: Store<()>,
    cache: HashMap<CacheKey, Compiled>,
}

impl std::fmt::Debug for WasmEval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmEval")
            .field("cached", &self.cache.len())
            .finish_non_exhaustive()
    }
}

impl WasmEval {
    /// Creates a fuel-metered, import-free WASM expression engine.
    ///
    /// # Errors
    ///
    /// Returns [`ExprError::Runtime`] if the `wasmtime` engine cannot be built.
    pub fn new() -> Result<Self, ExprError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(runtime)?;
        let store = Store::new(&engine, ());
        Ok(WasmEval {
            engine,
            store,
            cache: HashMap::new(),
        })
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
        let module = wasmtime::Module::new(&self.engine, &bytes).map_err(runtime)?;
        let instance = Instance::new(&mut self.store, &module, &[]).map_err(runtime)?;
        let func = instance
            .get_func(&mut self.store, "f")
            .ok_or_else(|| ExprError::Runtime("missing export `f`".to_owned()))?;
        self.cache.insert(
            (expr.clone(), param_types.to_vec()),
            Compiled {
                func,
                params: params.to_vec(),
                result_ty,
            },
        );
        Ok(())
    }
}

impl ExprEval for WasmEval {
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
        let compiled = &self.cache[&key];
        let mut args = Vec::with_capacity(compiled.params.len());
        for p in &compiled.params {
            let v = env.get(p).ok_or(ExprError::UnboundVar(*p))?;
            args.push(Val::I64(v.to_bits()));
        }
        let func = compiled.func;
        let result_ty = compiled.result_ty;
        self.store.set_fuel(FUEL_PER_CALL).map_err(runtime)?;
        let mut results = [Val::I64(0)];
        func.call(&mut self.store, &args, &mut results)
            .map_err(runtime)?;
        let bits = match results[0] {
            Val::I64(b) => b,
            ref other => return Err(ExprError::Runtime(format!("unexpected result {other:?}"))),
        };
        Ok(Value::from_bits(bits, result_ty))
    }
}

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
