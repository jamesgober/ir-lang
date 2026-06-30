//! The [`Function`]: the unit of IR, and the textual form it prints in.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::entity::{Block, Value};
use crate::inst::{Inst, Terminator};
use crate::ty::Type;
use crate::validate::ValidationError;

/// How a [`Value`] comes to be: either a parameter of a block, or the result of an
/// instruction in a block.
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum ValueDef {
    /// A parameter of the named block.
    Param(Block),
    /// The result of an instruction located in the named block.
    Inst(Block, Inst),
}

/// The type and origin of a single value.
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct ValueData {
    pub(crate) ty: Type,
    pub(crate) def: ValueDef,
}

/// The contents of a single basic block.
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct BlockData {
    /// The parameter values of this block, in order.
    pub(crate) params: Vec<Value>,
    /// The values defined by this block's instructions, in program order.
    pub(crate) insts: Vec<Value>,
    /// The terminator that ends the block, or `None` if one was never set.
    pub(crate) term: Option<Terminator>,
}

/// A function in SSA form: a control-flow graph of basic blocks over a single flat
/// store of values.
///
/// A function is the unit ir-lang represents and the thing a front-end lowers into.
/// It has a name, a parameter list, a return type, an entry block, and a set of
/// blocks; each block is a run of value-producing [`Inst`]s ended by one
/// [`Terminator`]. Values are named by [`Value`] handles and defined exactly once,
/// either as a block parameter or as an instruction result.
///
/// You do not construct a `Function` field by field — a [`Builder`](crate::Builder)
/// produces one. Once you hold it, the accessors here read it back, and
/// [`validate`](Function::validate) checks it is well-formed. A function also prints
/// as a readable textual IR through its [`Display`](core::fmt::Display)
/// implementation.
///
/// # Examples
///
/// ```
/// use ir_lang::{Builder, BinOp, Type};
///
/// // fn double(x: int) -> int { x + x }
/// let mut b = Builder::new("double", &[Type::Int], Type::Int);
/// let x = b.block_params(b.entry())[0];
/// let sum = b.bin(BinOp::Add, x, x);
/// b.ret(Some(sum));
/// let func = b.finish();
///
/// assert_eq!(func.name(), "double");
/// assert_eq!(func.params(), &[Type::Int]);
/// assert_eq!(func.ret(), Type::Int);
/// assert!(func.validate().is_ok());
/// ```
#[derive(Clone, PartialEq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Function {
    pub(crate) name: String,
    pub(crate) params: Vec<Type>,
    pub(crate) ret: Type,
    pub(crate) entry: Block,
    pub(crate) blocks: Vec<BlockData>,
    pub(crate) values: Vec<ValueData>,
}

impl Function {
    pub(crate) fn from_parts(
        name: String,
        params: Vec<Type>,
        ret: Type,
        entry: Block,
        blocks: Vec<BlockData>,
        values: Vec<ValueData>,
    ) -> Self {
        Self {
            name,
            params,
            ret,
            entry,
            blocks,
            values,
        }
    }

    /// Returns the function's name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let b = Builder::new("main", &[], Type::Unit);
    /// assert_eq!(b.finish().name(), "main");
    /// ```
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the function's parameter types, in order. These are also the types
    /// of the entry block's parameters.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let b = Builder::new("f", &[Type::Int, Type::Bool], Type::Unit);
    /// assert_eq!(b.finish().params(), &[Type::Int, Type::Bool]);
    /// ```
    #[must_use]
    pub fn params(&self) -> &[Type] {
        &self.params
    }

    /// Returns the function's return type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let b = Builder::new("f", &[], Type::Float);
    /// assert_eq!(b.finish().ret(), Type::Float);
    /// ```
    #[must_use]
    pub const fn ret(&self) -> Type {
        self.ret
    }

    /// Returns the entry block — where execution begins. It is always block zero
    /// and its parameters are the function's parameters.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type, Block};
    ///
    /// let b = Builder::new("f", &[], Type::Unit);
    /// assert_eq!(b.finish().entry().index(), 0);
    /// ```
    #[must_use]
    pub const fn entry(&self) -> Block {
        self.entry
    }

    /// Returns the number of blocks in the function.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Unit);
    /// let _ = b.create_block(&[]);
    /// b.ret(None);
    /// assert_eq!(b.finish().block_count(), 2);
    /// ```
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Returns the number of values defined in the function (block parameters and
    /// instruction results together). Value handles run densely over `0..count`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[Type::Int], Type::Int);
    /// let one = b.iconst(1);
    /// b.ret(Some(one));
    /// // one parameter value + one constant value
    /// assert_eq!(b.finish().value_count(), 2);
    /// ```
    #[must_use]
    pub fn value_count(&self) -> usize {
        self.values.len()
    }

    /// Iterates over every block handle, entry first, in creation order.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Unit);
    /// let _ = b.create_block(&[]);
    /// b.ret(None);
    /// let func = b.finish();
    /// assert_eq!(func.blocks().count(), 2);
    /// ```
    pub fn blocks(&self) -> impl Iterator<Item = Block> {
        (0..self.blocks.len() as u32).map(Block::from_raw)
    }

    /// Returns a block's parameter values, in order, or an empty slice if the block
    /// handle is out of range.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let b = Builder::new("f", &[Type::Int, Type::Int], Type::Unit);
    /// let func = b.finish();
    /// assert_eq!(func.block_params(func.entry()).len(), 2);
    /// ```
    #[must_use]
    pub fn block_params(&self, block: Block) -> &[Value] {
        match self.blocks.get(block.index()) {
            Some(data) => &data.params,
            None => &[],
        }
    }

    /// Returns the values defined by a block's instructions, in program order, or an
    /// empty slice if the block handle is out of range.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, BinOp, Type};
    ///
    /// let mut b = Builder::new("f", &[Type::Int], Type::Int);
    /// let x = b.block_params(b.entry())[0];
    /// let _ = b.bin(BinOp::Add, x, x);
    /// b.ret(Some(x));
    /// let func = b.finish();
    /// assert_eq!(func.insts(func.entry()).len(), 1);
    /// ```
    #[must_use]
    pub fn insts(&self, block: Block) -> &[Value] {
        match self.blocks.get(block.index()) {
            Some(data) => &data.insts,
            None => &[],
        }
    }

    /// Returns a block's terminator, or `None` if the block handle is out of range
    /// or no terminator was set (an unterminated block — which
    /// [`validate`](Function::validate) rejects).
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
    #[must_use]
    pub fn terminator(&self, block: Block) -> Option<&Terminator> {
        self.blocks.get(block.index())?.term.as_ref()
    }

    /// Returns the instruction that defined a value, or `None` if the value is a
    /// block parameter or the handle is out of range.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Inst, Type};
    ///
    /// let mut b = Builder::new("f", &[Type::Int], Type::Int);
    /// let param = b.block_params(b.entry())[0];
    /// let five = b.iconst(5);
    /// b.ret(Some(param));
    /// let func = b.finish();
    ///
    /// assert!(matches!(func.inst(five), Some(Inst::Iconst(5))));
    /// assert!(func.inst(param).is_none()); // a parameter has no defining instruction
    /// ```
    #[must_use]
    pub fn inst(&self, value: Value) -> Option<&Inst> {
        match &self.values.get(value.index())?.def {
            ValueDef::Inst(_, inst) => Some(inst),
            ValueDef::Param(_) => None,
        }
    }

    /// Returns the type of a value, or `None` if the handle is out of range.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, BinOp, Type};
    ///
    /// let mut b = Builder::new("f", &[Type::Int], Type::Bool);
    /// let x = b.block_params(b.entry())[0];
    /// let cmp = b.bin(BinOp::Lt, x, x);
    /// b.ret(Some(cmp));
    /// let func = b.finish();
    ///
    /// assert_eq!(func.value_type(x), Some(Type::Int));
    /// assert_eq!(func.value_type(cmp), Some(Type::Bool));
    /// ```
    #[must_use]
    pub fn value_type(&self, value: Value) -> Option<Type> {
        Some(self.values.get(value.index())?.ty)
    }

    /// Returns the block a value is defined in, or `None` if the handle is out of
    /// range.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[Type::Int], Type::Int);
    /// let x = b.block_params(b.entry())[0];
    /// b.ret(Some(x));
    /// let func = b.finish();
    /// assert_eq!(func.value_block(x), Some(func.entry()));
    /// ```
    #[must_use]
    pub fn value_block(&self, value: Value) -> Option<Block> {
        match self.values.get(value.index())?.def {
            ValueDef::Param(block) | ValueDef::Inst(block, _) => Some(block),
        }
    }

    /// Checks that the function is well-formed, returning the first violation found.
    ///
    /// A function that validates satisfies the SSA invariants the rest of a
    /// compiler relies on: every block ends in exactly one terminator; every branch
    /// targets a real block with a matching number and type of arguments; every
    /// value is referenced only where its single definition reaches it; operations
    /// are applied to operands of the right type; and the entry block is never a
    /// branch target. The [`Builder`](crate::Builder) does not check these as it
    /// goes, so run this once construction is complete — and again on the output of
    /// any pass that rewrites the IR.
    ///
    /// # Errors
    ///
    /// Returns the first [`ValidationError`] encountered. Each variant names the
    /// offending block or value; see [`ValidationError`] for the meaning of each and
    /// how to fix it.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, BinOp, Type, ValidationError};
    ///
    /// // A well-formed function validates.
    /// let mut b = Builder::new("f", &[Type::Int], Type::Int);
    /// let x = b.block_params(b.entry())[0];
    /// let two = b.iconst(2);
    /// let doubled = b.bin(BinOp::Mul, x, two);
    /// b.ret(Some(doubled));
    /// assert!(b.finish().validate().is_ok());
    ///
    /// // A block with no terminator does not.
    /// let unfinished = Builder::new("g", &[], Type::Unit).finish();
    /// assert!(matches!(
    ///     unfinished.validate(),
    ///     Err(ValidationError::MissingTerminator { .. })
    /// ));
    /// ```
    pub fn validate(&self) -> Result<(), ValidationError> {
        crate::validate::validate(self)
    }
}

impl fmt::Display for Function {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fn {}(", self.name)?;
        for (i, ty) in self.params.iter().enumerate() {
            if i != 0 {
                f.write_str(", ")?;
            }
            write!(f, "{ty}")?;
        }
        writeln!(f, ") -> {} {{", self.ret)?;

        for block in self.blocks() {
            write_block(f, self, block)?;
        }

        f.write_str("}")
    }
}

fn write_block(f: &mut fmt::Formatter<'_>, func: &Function, block: Block) -> fmt::Result {
    write!(f, "  {block}(")?;
    for (i, &param) in func.block_params(block).iter().enumerate() {
        if i != 0 {
            f.write_str(", ")?;
        }
        let ty = func.value_type(param).unwrap_or(Type::Unit);
        write!(f, "{param}: {ty}")?;
    }
    writeln!(f, "):")?;

    for &value in func.insts(block) {
        let ty = func.value_type(value).unwrap_or(Type::Unit);
        match func.inst(value) {
            Some(inst) => writeln!(f, "    {value}: {ty} = {}", FmtInst(inst))?,
            None => writeln!(f, "    {value}: {ty} = ?")?,
        }
    }

    match func.terminator(block) {
        Some(term) => writeln!(f, "    {}", FmtTerm(term))?,
        None => writeln!(f, "    <missing terminator>")?,
    }
    Ok(())
}

/// Adapter that renders an [`Inst`] in the textual IR form.
struct FmtInst<'a>(&'a Inst);

impl fmt::Display for FmtInst<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Inst::Iconst(v) => write!(f, "iconst {v}"),
            Inst::Fconst(v) => write!(f, "fconst {v}"),
            Inst::Bconst(v) => write!(f, "bconst {v}"),
            Inst::Bin(op, a, b) => write!(f, "{op} {a}, {b}"),
            Inst::Un(op, a) => write!(f, "{op} {a}"),
        }
    }
}

/// Adapter that renders a [`Terminator`] in the textual IR form.
struct FmtTerm<'a>(&'a Terminator);

impl fmt::Display for FmtTerm<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Terminator::Return(None) => f.write_str("return"),
            Terminator::Return(Some(v)) => write!(f, "return {v}"),
            Terminator::Jump(target, args) => {
                write!(f, "jump {target}")?;
                write_args(f, args)
            }
            Terminator::Branch {
                cond,
                then_block,
                then_args,
                else_block,
                else_args,
            } => {
                write!(f, "branch {cond}, {then_block}")?;
                write_args(f, then_args)?;
                write!(f, ", {else_block}")?;
                write_args(f, else_args)
            }
        }
    }
}

fn write_args(f: &mut fmt::Formatter<'_>, args: &[Value]) -> fmt::Result {
    if args.is_empty() {
        return Ok(());
    }
    f.write_str("(")?;
    for (i, arg) in args.iter().enumerate() {
        if i != 0 {
            f.write_str(", ")?;
        }
        write!(f, "{arg}")?;
    }
    f.write_str(")")
}

#[cfg(test)]
mod tests {
    use crate::{BinOp, Builder, Type};

    #[test]
    fn test_display_renders_signature_block_and_terminator() {
        let mut b = Builder::new("double", &[Type::Int], Type::Int);
        let x = b.block_params(b.entry())[0];
        let sum = b.bin(BinOp::Add, x, x);
        b.ret(Some(sum));
        let text = b.finish().to_string();

        assert!(text.starts_with("fn double(int) -> int {"));
        assert!(text.contains("b0(v0: int):"));
        assert!(text.contains("v1: int = add v0, v0"));
        assert!(text.contains("return v1"));
    }

    #[test]
    fn test_accessors_report_value_origin() {
        let mut b = Builder::new("f", &[Type::Int], Type::Int);
        let p = b.block_params(b.entry())[0];
        let five = b.iconst(5);
        b.ret(Some(p));
        let func = b.finish();

        assert_eq!(func.value_block(p), Some(func.entry()));
        assert_eq!(func.value_block(five), Some(func.entry()));
        assert!(func.inst(p).is_none());
        assert!(func.inst(five).is_some());
        assert_eq!(func.value_type(five), Some(Type::Int));
    }

    #[test]
    fn test_out_of_range_handles_return_none_not_panic() {
        let func = Builder::new("f", &[], Type::Unit).finish();
        let bogus_value = crate::Value::from_raw(99);
        let bogus_block = crate::Block::from_raw(99);
        assert_eq!(func.value_type(bogus_value), None);
        assert_eq!(func.value_block(bogus_value), None);
        assert!(func.inst(bogus_value).is_none());
        assert!(func.terminator(bogus_block).is_none());
        assert!(func.block_params(bogus_block).is_empty());
        assert!(func.insts(bogus_block).is_empty());
    }
}
