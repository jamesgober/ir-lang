//! Stable handles into a module and its functions.
//!
//! Every entity is named by a small `Copy` handle wrapping a dense `u32` index, so a
//! pass can keep a side table as a plain `Vec` indexed by `handle.index()`. Handles are
//! scoped to the module (functions, globals, signatures, aggregates) or the function
//! (values, blocks, instructions, stack slots) that minted them. They are never reused:
//! removing an instruction or a block leaves a tombstone, so a handle never silently
//! starts naming a different entity. [`Builder::compact`](crate::Builder::compact)
//! is the one operation that renumbers, and it returns an explicit [`Remap`](crate::Remap).

use core::fmt;

macro_rules! handle {
    ($(#[$doc:meta])* $name:ident, $prefix:literal, $example:literal) => {
        $(#[$doc])*
        ///
        /// # Examples
        ///
        /// ```
        #[doc = concat!("use ir_lang::", stringify!($name), ";")]
        ///
        #[doc = concat!("let h = ", stringify!($name), "::from_u32(3);")]
        /// assert_eq!(h.index(), 3);
        #[doc = concat!("assert_eq!(h.to_string(), \"", $example, "\");")]
        /// ```
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
        pub struct $name(u32);

        impl $name {
            /// Makes a handle from a raw index.
            ///
            /// Handles normally come from the module or function that owns the
            /// entity. This constructor exists for side tables and tests; a handle
            /// made up this way is checked like any other (an out-of-range handle is
            /// a validation error, never a panic).
            ///
            /// # Examples
            ///
            /// ```
            #[doc = concat!("use ir_lang::", stringify!($name), ";")]
            ///
            #[doc = concat!("assert_eq!(", stringify!($name), "::from_u32(0).index(), 0);")]
            /// ```
            #[inline]
            #[must_use]
            pub const fn from_u32(index: u32) -> Self {
                Self(index)
            }

            /// Returns the dense zero-based index of this handle.
            ///
            /// # Examples
            ///
            /// ```
            #[doc = concat!("use ir_lang::", stringify!($name), ";")]
            ///
            #[doc = concat!("assert_eq!(", stringify!($name), "::from_u32(7).index(), 7);")]
            /// ```
            #[inline]
            #[must_use]
            pub const fn index(self) -> usize {
                self.0 as usize
            }

            /// Returns the raw `u32` index.
            ///
            /// # Examples
            ///
            /// ```
            #[doc = concat!("use ir_lang::", stringify!($name), ";")]
            ///
            #[doc = concat!("assert_eq!(", stringify!($name), "::from_u32(9).as_u32(), 9);")]
            /// ```
            #[inline]
            #[must_use]
            pub const fn as_u32(self) -> u32 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($prefix, "{}"), self.0)
            }
        }
    };
}

handle!(
    /// An SSA value in a function: a block parameter or an instruction result.
    ///
    /// Every value has one type and exactly one definition. It prints as `v<n>`.
    Value,
    "v",
    "v3"
);

handle!(
    /// A basic block in a function. It prints as `b<n>`.
    Block,
    "b",
    "b3"
);

handle!(
    /// An instruction in a function. Instruction handles are not printed in the
    /// textual form (an instruction is named by its results); in messages it prints
    /// as `inst<n>`.
    Inst,
    "inst",
    "inst3"
);

handle!(
    /// A stack slot of a function: frame memory addressed with
    /// [`InstData::StackAddr`](crate::InstData::StackAddr). It prints as `ss<n>`.
    StackSlot,
    "ss",
    "ss3"
);

handle!(
    /// A function declaration in a module (defined or imported). In messages it
    /// prints as `f<n>`; the textual form names functions by symbol (`@name`).
    FuncId,
    "f",
    "f3"
);

handle!(
    /// A global (data) declaration in a module. In messages it prints as `g<n>`;
    /// the textual form names globals by symbol (`@name`).
    GlobalId,
    "g",
    "g3"
);

handle!(
    /// An interned function signature in a module. It prints as `s<n>`.
    SigId,
    "s",
    "s3"
);

handle!(
    /// An aggregate (struct or array) memory type in a module. It prints as `t<n>`.
    AggId,
    "t",
    "t3"
);

handle!(
    /// A mutable local variable of the builder's SSA construction
    /// ([`Builder::declare_var`](crate::Builder::declare_var)): a name for "the
    /// current value of `x`" that the builder turns into SSA values and block
    /// parameters. Scoped to one builder. In messages it prints as `var<n>`.
    Variable,
    "var",
    "var3"
);

/// The results of one instruction: a run of consecutive values.
///
/// An instruction defines zero or more values, always with consecutive handles, so
/// the run is stored as a start and a length rather than a list.
///
/// # Examples
///
/// ```
/// use ir_lang::{Module, Signature, Type, Linkage, MemOrder};
///
/// let mut m = Module::new("demo");
/// let f = m.declare_function("f", &Signature::new(&[Type::Ptr], &[]), Linkage::Export)?;
/// let mut b = m.build(f)?;
/// let p = b.param(0)?;
/// let old = b.iconst(Type::I64, 1)?;
/// let new = b.iconst(Type::I64, 2)?;
/// let inst = b.cmpxchg(p, old, new, MemOrder::SeqCst, MemOrder::SeqCst)?;
/// let results = b.results(inst);
/// assert_eq!(results.len(), 2);
/// assert_eq!(results.get(1), Some(results.first().unwrap().next()));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct ValueRange {
    pub(crate) start: u32,
    pub(crate) len: u32,
}

impl ValueRange {
    /// The number of values in the run.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::ValueRange;
    ///
    /// assert_eq!(ValueRange::default().len(), 0);
    /// ```
    #[inline]
    #[must_use]
    pub const fn len(self) -> usize {
        self.len as usize
    }

    /// Whether the run is empty (the instruction defines nothing).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::ValueRange;
    ///
    /// assert!(ValueRange::default().is_empty());
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// The `i`-th value of the run, if there is one.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::ValueRange;
    ///
    /// assert_eq!(ValueRange::default().get(0), None);
    /// ```
    #[inline]
    #[must_use]
    pub const fn get(self, i: usize) -> Option<Value> {
        if i < self.len as usize {
            match self.start.checked_add(i as u32) {
                Some(v) => Some(Value(v)),
                None => None,
            }
        } else {
            None
        }
    }

    /// The first value of the run, if there is one.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::ValueRange;
    ///
    /// assert_eq!(ValueRange::default().first(), None);
    /// ```
    #[inline]
    #[must_use]
    pub const fn first(self) -> Option<Value> {
        self.get(0)
    }

    /// Whether `value` is one of the run's values.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Value, ValueRange};
    ///
    /// assert!(!ValueRange::default().contains(Value::from_u32(0)));
    /// ```
    #[inline]
    #[must_use]
    pub const fn contains(self, value: Value) -> bool {
        value.0 >= self.start && value.0 - self.start < self.len
    }

    /// Iterates over the run's values in order.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::ValueRange;
    ///
    /// assert_eq!(ValueRange::default().iter().count(), 0);
    /// ```
    #[inline]
    pub fn iter(self) -> impl ExactSizeIterator<Item = Value> + DoubleEndedIterator {
        (self.start..self.start.saturating_add(self.len)).map(Value)
    }
}

impl IntoIterator for ValueRange {
    type Item = Value;
    type IntoIter = core::iter::Map<core::ops::Range<u32>, fn(u32) -> Value>;

    fn into_iter(self) -> Self::IntoIter {
        (self.start..self.start.saturating_add(self.len)).map(Value as fn(u32) -> Value)
    }
}

impl Value {
    /// The value with the next index. Results of one instruction are consecutive,
    /// so `first.next()` is the second result.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Value;
    ///
    /// assert_eq!(Value::from_u32(4).next(), Value::from_u32(5));
    /// ```
    #[inline]
    #[must_use]
    pub const fn next(self) -> Value {
        Value(self.0.saturating_add(1))
    }
}

/// The sentinel used for "no entity" in the internal linked lists.
pub(crate) const NONE: u32 = u32::MAX;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn test_handles_display_with_their_prefixes() {
        assert_eq!(Value::from_u32(1).to_string(), "v1");
        assert_eq!(Block::from_u32(2).to_string(), "b2");
        assert_eq!(StackSlot::from_u32(0).to_string(), "ss0");
        assert_eq!(AggId::from_u32(5).to_string(), "t5");
        assert_eq!(SigId::from_u32(5).to_string(), "s5");
    }

    #[test]
    fn test_value_range_contains_and_iterates() {
        let r = ValueRange { start: 4, len: 2 };
        assert!(r.contains(Value::from_u32(4)));
        assert!(r.contains(Value::from_u32(5)));
        assert!(!r.contains(Value::from_u32(6)));
        assert!(!r.contains(Value::from_u32(3)));
        assert_eq!(r.iter().collect::<alloc::vec::Vec<_>>().len(), 2);
        assert_eq!(r.into_iter().next_back(), Some(Value::from_u32(5)));
    }
}
