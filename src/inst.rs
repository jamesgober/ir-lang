//! Instructions and terminators: the operations a block is made of.

use alloc::vec::Vec;
use core::fmt;

use crate::entity::{Block, Value};

/// A binary operation.
///
/// The arithmetic operations ([`Add`](BinOp::Add) through [`Div`](BinOp::Div))
/// take two operands of the same numeric type and produce that type. The comparison
/// operations ([`Eq`](BinOp::Eq) through [`Ge`](BinOp::Ge)) take two operands of the
/// same type and produce a [`Bool`](crate::Type::Bool). The logical operations
/// ([`And`](BinOp::And), [`Or`](BinOp::Or)) take two `Bool`s and produce a `Bool`.
/// The validator enforces these operand rules; the result type is determined by the
/// operation, so the builder never has to be told it.
///
/// # Examples
///
/// ```
/// use ir_lang::BinOp;
///
/// assert_eq!(BinOp::Add.to_string(), "add");
/// assert!(BinOp::Lt.is_comparison());
/// assert!(!BinOp::Add.is_comparison());
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BinOp {
    /// Addition of two numeric operands.
    Add,
    /// Subtraction of the second numeric operand from the first.
    Sub,
    /// Multiplication of two numeric operands.
    Mul,
    /// Division of the first numeric operand by the second.
    Div,
    /// Equality comparison; result is `Bool`.
    Eq,
    /// Inequality comparison; result is `Bool`.
    Ne,
    /// Less-than comparison; result is `Bool`.
    Lt,
    /// Less-than-or-equal comparison; result is `Bool`.
    Le,
    /// Greater-than comparison; result is `Bool`.
    Gt,
    /// Greater-than-or-equal comparison; result is `Bool`.
    Ge,
    /// Logical conjunction of two `Bool` operands; result is `Bool`.
    And,
    /// Logical disjunction of two `Bool` operands; result is `Bool`.
    Or,
}

impl BinOp {
    /// Returns `true` for the comparison operations, whose result is a
    /// [`Bool`](crate::Type::Bool) regardless of the operand type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::BinOp;
    ///
    /// assert!(BinOp::Eq.is_comparison());
    /// assert!(BinOp::Ge.is_comparison());
    /// assert!(!BinOp::Mul.is_comparison());
    /// ```
    #[must_use]
    pub const fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }

    /// Returns `true` for the logical operations, which take and produce
    /// [`Bool`](crate::Type::Bool).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::BinOp;
    ///
    /// assert!(BinOp::And.is_logical());
    /// assert!(!BinOp::Add.is_logical());
    /// ```
    #[must_use]
    pub const fn is_logical(self) -> bool {
        matches!(self, BinOp::And | BinOp::Or)
    }
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::Div => "div",
            BinOp::Eq => "eq",
            BinOp::Ne => "ne",
            BinOp::Lt => "lt",
            BinOp::Le => "le",
            BinOp::Gt => "gt",
            BinOp::Ge => "ge",
            BinOp::And => "and",
            BinOp::Or => "or",
        };
        f.write_str(name)
    }
}

/// A unary operation.
///
/// [`Neg`](UnOp::Neg) negates a numeric operand and produces the same numeric type.
/// [`Not`](UnOp::Not) inverts a [`Bool`](crate::Type::Bool) and produces a `Bool`.
///
/// # Examples
///
/// ```
/// use ir_lang::UnOp;
///
/// assert_eq!(UnOp::Neg.to_string(), "neg");
/// assert_eq!(UnOp::Not.to_string(), "not");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum UnOp {
    /// Arithmetic negation of a numeric operand.
    Neg,
    /// Logical negation of a `Bool` operand.
    Not,
}

impl fmt::Display for UnOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            UnOp::Neg => "neg",
            UnOp::Not => "not",
        };
        f.write_str(name)
    }
}

/// A value-producing instruction.
///
/// Every variant defines exactly one [`Value`], whose type is recorded by the
/// [`Function`](crate::Function) and can be read with
/// [`Function::value_type`](crate::Function::value_type). Instructions reference
/// their operands by `Value` handle, never by nesting a sub-expression, so the IR
/// stays flat and walkable in program order. The terminator that ends a block is a
/// separate type, [`Terminator`].
///
/// You do not build an `Inst` directly; the [`Builder`](crate::Builder) emits one
/// per call and hands back the value it defines. This type is what you read back
/// when inspecting a function, for example with
/// [`Function::inst`](crate::Function::inst).
///
/// # Examples
///
/// ```
/// use ir_lang::{Builder, BinOp, Inst};
///
/// let mut b = Builder::new("k", &[], ir_lang::Type::Int);
/// let one = b.iconst(1);
/// let sum = b.bin(BinOp::Add, one, one);
/// b.ret(Some(sum));
/// let func = b.finish();
///
/// // The instruction that defined `sum` is the add.
/// assert!(matches!(func.inst(sum), Some(Inst::Bin(BinOp::Add, _, _))));
/// // A block parameter is not produced by an instruction.
/// assert!(func.inst(one).is_some());
/// ```
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Inst {
    /// An integer constant. Result type is [`Int`](crate::Type::Int).
    Iconst(i64),
    /// A floating-point constant. Result type is [`Float`](crate::Type::Float).
    Fconst(f64),
    /// A boolean constant. Result type is [`Bool`](crate::Type::Bool).
    Bconst(bool),
    /// A binary operation over two values. Result type follows the operation.
    Bin(BinOp, Value, Value),
    /// A unary operation over one value. Result type follows the operation.
    Un(UnOp, Value),
}

/// The single instruction that ends a basic block and transfers control.
///
/// Exactly one terminator ends every block. A [`Jump`](Terminator::Jump) or the two
/// arms of a [`Branch`](Terminator::Branch) carry an argument per parameter of the
/// target block — that is how a value is threaded across a control-flow join in SSA
/// form, in place of a phi node. A [`Return`](Terminator::Return) leaves the
/// function.
///
/// # Examples
///
/// ```
/// use ir_lang::{Builder, Type, Terminator};
///
/// let mut b = Builder::new("f", &[], Type::Unit);
/// b.ret(None);
/// let func = b.finish();
/// assert!(matches!(func.terminator(func.entry()), Some(Terminator::Return(None))));
/// ```
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Terminator {
    /// Return from the function, optionally with a value. `Some(v)` returns `v`,
    /// whose type must match the function's return type; `None` returns from a
    /// function whose return type is [`Unit`](crate::Type::Unit).
    Return(Option<Value>),
    /// Jump unconditionally to a block, passing one argument per target parameter.
    Jump(Block, Vec<Value>),
    /// Branch on a [`Bool`](crate::Type::Bool) condition: take the first block (and
    /// its arguments) when the condition is true, the second otherwise. Each block's
    /// arguments are matched against that block's parameters.
    Branch {
        /// The boolean condition selecting which arm runs.
        cond: Value,
        /// The block taken when `cond` is true.
        then_block: Block,
        /// Arguments passed to `then_block`'s parameters.
        then_args: Vec<Value>,
        /// The block taken when `cond` is false.
        else_block: Block,
        /// Arguments passed to `else_block`'s parameters.
        else_args: Vec<Value>,
    },
}

impl Terminator {
    /// Calls `f` once for each block this terminator can transfer control to, in
    /// order. A [`Return`](Terminator::Return) calls `f` zero times.
    ///
    /// This is how the control-flow graph is read: the successors of a block are the
    /// targets of its terminator.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Unit);
    /// let exit = b.create_block(&[]);
    /// b.jump(exit, &[]);
    /// b.switch_to(exit);
    /// b.ret(None);
    /// let func = b.finish();
    ///
    /// let mut succs = Vec::new();
    /// if let Some(term) = func.terminator(func.entry()) {
    ///     term.each_successor(|blk| succs.push(blk));
    /// }
    /// assert_eq!(succs, vec![exit]);
    /// ```
    pub fn each_successor(&self, mut f: impl FnMut(Block)) {
        match self {
            Terminator::Return(_) => {}
            Terminator::Jump(target, _) => f(*target),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                f(*then_block);
                f(*else_block);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn test_binop_classification_partitions_operations() {
        for op in [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div] {
            assert!(!op.is_comparison() && !op.is_logical());
        }
        for op in [
            BinOp::Eq,
            BinOp::Ne,
            BinOp::Lt,
            BinOp::Le,
            BinOp::Gt,
            BinOp::Ge,
        ] {
            assert!(op.is_comparison() && !op.is_logical());
        }
        for op in [BinOp::And, BinOp::Or] {
            assert!(op.is_logical() && !op.is_comparison());
        }
    }

    #[test]
    fn test_each_successor_reports_targets_in_order() {
        let mut got = Vec::new();
        Terminator::Return(None).each_successor(|b| got.push(b));
        assert!(got.is_empty());

        let mut got = Vec::new();
        Terminator::Jump(Block::from_raw(2), vec![]).each_successor(|b| got.push(b));
        assert_eq!(got, vec![Block::from_raw(2)]);

        let mut got = Vec::new();
        Terminator::Branch {
            cond: Value::from_raw(0),
            then_block: Block::from_raw(1),
            then_args: vec![],
            else_block: Block::from_raw(2),
            else_args: vec![],
        }
        .each_successor(|b| got.push(b));
        assert_eq!(got, vec![Block::from_raw(1), Block::from_raw(2)]);
    }
}
