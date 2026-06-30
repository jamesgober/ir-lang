//! Stable handles into a function: [`Value`] and [`Block`].

use core::fmt;

/// A handle to an SSA value inside a [`Function`](crate::Function).
///
/// A value is the result of one instruction or one block parameter. It is named by
/// this small, copyable handle rather than by an expression position, which is what
/// lets a later pass refer to a result without holding a borrow of the function and
/// lets the IR stay flat. A handle is stable for the life of the function that
/// minted it and is dense (the handles of a function are `0..n`), so it doubles as
/// an index into a side table a pass keeps alongside the IR.
///
/// Handles are scoped to the function they come from: a `Value` minted by one
/// function's [`Builder`](crate::Builder) is meaningless in another. The validator
/// catches a value used where its definition does not reach, but it cannot tell two
/// functions' identically-numbered handles apart, so do not mix them.
///
/// # Examples
///
/// ```
/// use ir_lang::{Builder, Type};
///
/// let mut b = Builder::new("id", &[Type::Int], Type::Int);
/// let entry = b.entry();
/// // The entry block's parameters are the function's parameters, as values.
/// let x = b.block_params(entry)[0];
/// b.ret(Some(x));
///
/// // A value handle is `Copy` and prints as `v<n>`.
/// assert_eq!(x.to_string(), "v0");
/// assert_eq!(x.index(), 0);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Value(u32);

impl Value {
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the zero-based index of this value, for use as a key into a side
    /// table a pass keeps alongside the function.
    ///
    /// The values of a function are numbered densely from zero in the order they
    /// are created, so a `Vec` sized to the value count can be indexed directly by
    /// `value.index()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("k", &[], Type::Int);
    /// let a = b.iconst(1);
    /// let c = b.iconst(2);
    /// assert_eq!(a.index(), 0);
    /// assert_eq!(c.index(), 1);
    /// ```
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A handle to a basic block inside a [`Function`](crate::Function).
///
/// A block is a straight-line run of instructions with one entry and one
/// terminator. This handle names a block so a terminator can branch to it and so a
/// pass can walk the control-flow graph. Like [`Value`], it is `Copy`, stable for
/// the life of its function, and dense from zero.
///
/// # Examples
///
/// ```
/// use ir_lang::{Builder, Type};
///
/// let mut b = Builder::new("f", &[], Type::Unit);
/// let entry = b.entry();
/// let exit = b.create_block(&[]);
///
/// assert_eq!(entry.to_string(), "b0");
/// assert_eq!(exit.to_string(), "b1");
/// assert_eq!(exit.index(), 1);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Block(u32);

impl Block {
    pub(crate) const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the zero-based index of this block, for use as a key into a side
    /// table a pass keeps alongside the function.
    ///
    /// Blocks are numbered densely from zero in creation order; the entry block is
    /// always block zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let b = Builder::new("f", &[], Type::Unit);
    /// assert_eq!(b.entry().index(), 0);
    /// ```
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_value_display_and_index_agree() {
        let v = Value::from_raw(7);
        assert_eq!(v.to_string(), "v7");
        assert_eq!(v.index(), 7);
    }

    #[test]
    fn test_block_display_and_index_agree() {
        let b = Block::from_raw(3);
        assert_eq!(b.to_string(), "b3");
        assert_eq!(b.index(), 3);
    }

    #[test]
    fn test_handles_order_by_raw_index() {
        assert!(Value::from_raw(0) < Value::from_raw(1));
        assert!(Block::from_raw(2) > Block::from_raw(1));
    }
}
