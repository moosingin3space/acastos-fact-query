//! The dynamically-typed value model and string interning.
//!
//! Because schemas are not known until runtime, tuples are `Vec<Value>` and a
//! [`Value`] is a small enum. Strings are interned to [`Symbol`]s so that
//! equality and hashing are integer operations.

use std::collections::HashMap;

/// An interned string, represented by a small integer id.
///
/// Symbols are only meaningful relative to the [`Interner`] that produced them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(pub u32);

/// Interns strings to [`Symbol`]s and resolves them back.
#[derive(Debug, Default, Clone)]
pub struct Interner {
    map: HashMap<String, Symbol>,
    names: Vec<String>,
}

impl Interner {
    /// Creates an empty interner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns `s`, returning a symbol that is stable for this interner.
    ///
    /// # Panics
    ///
    /// Panics if more than `u32::MAX` distinct strings are interned.
    pub fn intern(&mut self, s: &str) -> Symbol {
        if let Some(sym) = self.map.get(s) {
            return *sym;
        }
        let id = u32::try_from(self.names.len()).expect("symbol count exceeds u32::MAX");
        let sym = Symbol(id);
        self.names.push(s.to_owned());
        self.map.insert(s.to_owned(), sym);
        sym
    }

    /// Resolves `sym` to its string, if it belongs to this interner.
    #[must_use]
    pub fn resolve(&self, sym: Symbol) -> Option<&str> {
        self.names.get(sym.0 as usize).map(String::as_str)
    }
}

/// The runtime type of a column, value, or expression result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    /// A signed 64-bit integer.
    Int,
    /// A boolean.
    Bool,
    /// An interned symbol (string).
    Sym,
}

/// A runtime value: the contents of a single tuple column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Value {
    /// A signed 64-bit integer. Covers `i32`/`u32`/`i64`/`usize` fixtures.
    Int(i64),
    /// A boolean.
    Bool(bool),
    /// An interned symbol (string).
    Sym(Symbol),
}

impl Value {
    /// Returns the [`Type`] of this value.
    #[must_use]
    pub fn type_of(self) -> Type {
        match self {
            Value::Int(_) => Type::Int,
            Value::Bool(_) => Type::Bool,
            Value::Sym(_) => Type::Sym,
        }
    }

    /// Encodes this value into the `i64` representation used by the WASM
    /// expression tier. Equality on the bit pattern matches equality on the
    /// value for every variant (interned ids compare equal iff equal).
    #[must_use]
    pub fn to_bits(self) -> i64 {
        match self {
            Value::Int(i) => i,
            Value::Bool(b) => i64::from(b),
            Value::Sym(s) => i64::from(s.0),
        }
    }

    /// Decodes an `i64` produced by the WASM tier back into a value of `ty`.
    ///
    /// # Panics
    ///
    /// Panics if `ty` is [`Type::Sym`] and `bits` is not a valid symbol id.
    #[must_use]
    pub fn from_bits(bits: i64, ty: Type) -> Value {
        match ty {
            Type::Int => Value::Int(bits),
            Type::Bool => Value::Bool(bits != 0),
            Type::Sym => Value::Sym(Symbol(u32::try_from(bits).expect("symbol id out of range"))),
        }
    }
}
