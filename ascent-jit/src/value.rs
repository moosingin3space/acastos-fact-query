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
}
