//! Stack-overflow protection for the engine's recursive walks.
//!
//! The parser, the expression interpreter, and the body traversal in
//! [`crate::eval`] are recursive. Expression recursion is bounded by
//! [`MAX_EXPR_DEPTH`](crate::expr::MAX_EXPR_DEPTH); a rule body's clause count
//! is not. [`guard`] runs a closure on a freshly allocated stack segment when
//! the current one is nearly exhausted, so deep input degrades into heap use
//! rather than a crash.
//!
//! On `wasm32`, `stacker` cannot discover the stack limit and would allocate a
//! segment on *every* call, so [`guard`] is a plain call there. The depth limit
//! still bounds expression recursion, and an overflow is a WebAssembly trap
//! (a thrown `RangeError` in JS), never memory corruption.

/// Grow when less than this much stack remains.
#[cfg(not(target_arch = "wasm32"))]
const RED_ZONE: usize = 64 * 1024;

/// The size of each newly allocated stack segment.
#[cfg(not(target_arch = "wasm32"))]
const SEGMENT: usize = 1024 * 1024;

/// Runs `f`, first moving to a new stack segment if the current one has less
/// than the red zone left.
#[cfg(not(target_arch = "wasm32"))]
#[inline]
pub(crate) fn guard<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, SEGMENT, f)
}

/// Runs `f` directly (see the module docs for why `wasm32` does not grow).
#[cfg(target_arch = "wasm32")]
#[inline]
pub(crate) fn guard<R>(f: impl FnOnce() -> R) -> R {
    f()
}
