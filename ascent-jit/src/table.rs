//! Flat-storage building blocks for relations: a liveness bit set, a seeded
//! hash over values, and an open-addressing table keyed by a projection of the
//! stored rows.
//!
//! Rows live once, in a relation's flat `Vec<Value>` (row-major, arity
//! stride). A [`KeyTable`] never copies a key: it maps a key's hash to a
//! *group* and compares candidates against a representative row in place. Set
//! deduplication, lattice keys, and every index are one table each.

use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;

use crate::value::Value;

/// A growable bit set, used for row liveness.
#[derive(Debug, Clone, Default)]
pub(crate) struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    /// Appends a set bit at index `len`, where `len` is the current length.
    pub(crate) fn push_set(&mut self, len: usize) {
        if len.is_multiple_of(64) {
            self.words.push(0);
        }
        self.words[len / 64] |= 1 << (len % 64);
    }

    pub(crate) fn clear(&mut self, i: usize) {
        self.words[i / 64] &= !(1 << (i % 64));
    }

    #[inline]
    pub(crate) fn get(&self, i: usize) -> bool {
        self.words[i / 64] & (1 << (i % 64)) != 0
    }
}

/// A per-process random seed, so colliding keys cannot be precomputed (a
/// hash-flooding guard; inputs may be untrusted).
fn seed() -> u64 {
    static SEED: OnceLock<u64> = OnceLock::new();
    *SEED.get_or_init(|| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(0x5eed);
        h.finish() | 1
    })
}

const MUL: u64 = 0x9e37_79b9_7f4a_7c15;

/// Folded 64×64→128-bit multiply: cheap, and mixes every input bit.
#[inline]
#[expect(
    clippy::cast_possible_truncation,
    reason = "folding the product's halves together is the point"
)]
fn fold(a: u64, b: u64) -> u64 {
    let m = u128::from(a) * u128::from(b);
    (m as u64) ^ ((m >> 64) as u64)
}

/// Hashes values in order. Equal sequences hash equally; the kind tag keeps
/// `Int(1)`, `Bool(true)`, and `Sym(1)` apart (collisions are only slower).
#[inline]
pub(crate) fn hash_values(values: impl Iterator<Item = Value>) -> u64 {
    let mut h = seed();
    for v in values {
        let (tag, bits) = match v {
            Value::Int(i) => (1, i.cast_unsigned()),
            Value::Bool(b) => (2, u64::from(b)),
            Value::Sym(s) => (3, u64::from(s.0)),
        };
        h = fold(h ^ bits, MUL ^ tag);
    }
    h
}

/// Row `id` of flat storage `data` with stride `arity`.
#[inline]
pub(crate) fn row_at(data: &[Value], arity: usize, id: u32) -> &[Value] {
    let start = id as usize * arity;
    &data[start..start + arity]
}

/// The home slot of `hash` in a table of `mask + 1` slots.
#[inline]
#[expect(
    clippy::cast_possible_truncation,
    reason = "only the low bits select a slot; the mask discards the rest anyway"
)]
fn slot_of(hash: u64, mask: usize) -> usize {
    (hash as usize) & mask
}

/// The empty marker in [`KeyTable::slots`].
const EMPTY: u32 = u32::MAX;

/// An open-addressing (linear probing) table from keys to dense group ids,
/// where a key is a row projected onto `cols`. Each group remembers its hash
/// and a representative row; lookups compare the probe key against that row's
/// columns in the relation's flat storage, so keys are never stored.
#[derive(Debug, Clone)]
pub(crate) struct KeyTable {
    cols: Vec<usize>,
    /// Power-of-two sized; each entry is a group id or [`EMPTY`].
    slots: Vec<u32>,
    hashes: Vec<u64>,
    reps: Vec<u32>,
}

impl KeyTable {
    pub(crate) fn new(cols: Vec<usize>) -> Self {
        KeyTable {
            cols,
            slots: vec![EMPTY; 16],
            hashes: Vec::new(),
            reps: Vec::new(),
        }
    }

    pub(crate) fn cols(&self) -> &[usize] {
        &self.cols
    }

    /// The hash of `row` projected onto this table's columns.
    #[inline]
    pub(crate) fn hash_row(&self, row: &[Value]) -> u64 {
        hash_values(self.cols.iter().map(|&c| row[c]))
    }

    /// The group whose key equals `key` (already projected onto this table's
    /// columns; `hash` from [`hash_values`] over it), if any. `data` is the
    /// relation's flat row storage with stride `arity`.
    #[inline]
    pub(crate) fn find(
        &self,
        key: &[Value],
        hash: u64,
        data: &[Value],
        arity: usize,
    ) -> Option<u32> {
        self.probe(hash, |g| {
            let rep = row_at(data, arity, self.reps[g as usize]);
            self.cols.iter().zip(key).all(|(&c, &k)| rep[c] == k)
        })
    }

    /// The group whose key equals `row`'s projection, if any.
    #[inline]
    pub(crate) fn find_row(
        &self,
        row: &[Value],
        hash: u64,
        data: &[Value],
        arity: usize,
    ) -> Option<u32> {
        self.probe(hash, |g| {
            let rep = row_at(data, arity, self.reps[g as usize]);
            self.cols.iter().all(|&c| rep[c] == row[c])
        })
    }

    #[inline]
    fn probe(&self, hash: u64, mut same: impl FnMut(u32) -> bool) -> Option<u32> {
        let mask = self.slots.len() - 1;
        let mut i = slot_of(hash, mask);
        loop {
            let g = self.slots[i];
            if g == EMPTY {
                return None;
            }
            if self.hashes[g as usize] == hash && same(g) {
                return Some(g);
            }
            i = (i + 1) & mask;
        }
    }

    /// Adds a new group (whose key must be absent) represented by row `rep`,
    /// returning its id.
    pub(crate) fn insert(&mut self, hash: u64, rep: u32) -> u32 {
        if (self.hashes.len() + 1) * 2 > self.slots.len() {
            self.grow();
        }
        let g = u32::try_from(self.hashes.len()).unwrap_or(EMPTY - 1);
        self.hashes.push(hash);
        self.reps.push(rep);
        self.place(hash, g);
        g
    }

    /// Re-points group `g` at a new representative row (a lattice key's
    /// joined value replacing the old row).
    pub(crate) fn set_rep(&mut self, g: u32, rep: u32) {
        self.reps[g as usize] = rep;
    }

    pub(crate) fn rep(&self, g: u32) -> u32 {
        self.reps[g as usize]
    }

    fn place(&mut self, hash: u64, g: u32) {
        let mask = self.slots.len() - 1;
        let mut i = slot_of(hash, mask);
        while self.slots[i] != EMPTY {
            i = (i + 1) & mask;
        }
        self.slots[i] = g;
    }

    fn grow(&mut self) {
        self.slots = vec![EMPTY; self.slots.len() * 2];
        for g in 0..self.hashes.len() {
            let g = u32::try_from(g).unwrap_or(EMPTY - 1);
            self.place(self.hashes[g as usize], g);
        }
    }
}

/// A small open-addressing map from `u64` keys to dense `u32` ids, assigned in
/// insertion order. Used to deduplicate fact handles in query provenance.
#[derive(Debug, Clone)]
pub(crate) struct IdMap {
    /// Power-of-two sized; each entry is an index into `keys`, or [`EMPTY`].
    slots: Vec<u32>,
    keys: Vec<u64>,
}

impl Default for IdMap {
    fn default() -> Self {
        IdMap {
            slots: vec![EMPTY; 16],
            keys: Vec::new(),
        }
    }
}

impl IdMap {
    /// The id of `key`, assigning the next id if it is new. Returns the id and
    /// whether it was newly assigned.
    pub(crate) fn get_or_insert(&mut self, key: u64) -> (u32, bool) {
        let hash = fold(key ^ seed(), MUL);
        let mask = self.slots.len() - 1;
        let mut i = slot_of(hash, mask);
        loop {
            let id = self.slots[i];
            if id == EMPTY {
                break;
            }
            if self.keys[id as usize] == key {
                return (id, false);
            }
            i = (i + 1) & mask;
        }
        let id = u32::try_from(self.keys.len()).unwrap_or(EMPTY - 1);
        self.keys.push(key);
        if self.keys.len() * 2 > self.slots.len() {
            self.slots = vec![EMPTY; self.slots.len() * 2];
            for n in 0..self.keys.len() {
                let k = self.keys[n];
                let n = u32::try_from(n).unwrap_or(EMPTY - 1);
                self.place(fold(k ^ seed(), MUL), n);
            }
        } else {
            self.slots[i] = id;
        }
        (id, true)
    }

    fn place(&mut self, hash: u64, id: u32) {
        let mask = self.slots.len() - 1;
        let mut i = slot_of(hash, mask);
        while self.slots[i] != EMPTY {
            i = (i + 1) & mask;
        }
        self.slots[i] = id;
    }
}
