//! The [`Builder`]: how IR is constructed, and the interface a front-end lowers
//! through.

use alloc::string::String;
use alloc::vec::Vec;

use crate::entity::{Block, Value};
use crate::function::{BlockData, Function, ValueData, ValueDef};
use crate::inst::{BinOp, Inst, Terminator, UnOp};
use crate::ty::Type;

/// Constructs a [`Function`] one instruction at a time.
///
/// The builder is ir-lang's lowering interface. A front-end walks its own syntax
/// tree and, for each construct, calls a builder method: a literal becomes a
/// constant, an operator becomes a [`bin`](Builder::bin) or [`un`](Builder::un), an
/// `if` becomes two blocks and a [`branch`](Builder::branch), a join becomes a block
/// with parameters reached by [`jump`](Builder::jump). The builder mints a fresh
/// [`Value`] for every result and hands it back, so the tree's structure is captured
/// as flat SSA without the caller tracking numbering. Result types are inferred from
/// the operation, so they never have to be supplied.
///
/// Construction does not check well-formedness as it goes — that keeps the hot path
/// of lowering allocation-light and lets the caller emit blocks in whatever order is
/// convenient. Call [`Function::validate`](crate::Function::validate) on the result
/// (or build it and validate before handing it to a pass) to confirm the IR is
/// sound.
///
/// # Examples
///
/// Lower `fn max(a: int, b: int) -> int { if a < b { b } else { a } }`:
///
/// ```
/// use ir_lang::{Builder, BinOp, Type};
///
/// let mut b = Builder::new("max", &[Type::Int, Type::Int], Type::Int);
/// let entry = b.entry();
/// let a = b.block_params(entry)[0];
/// let bb = b.block_params(entry)[1];
///
/// // The join block takes the chosen value as a parameter.
/// let join = b.create_block(&[Type::Int]);
/// let then_blk = b.create_block(&[]);
/// let else_blk = b.create_block(&[]);
///
/// let cond = b.bin(BinOp::Lt, a, bb);
/// b.branch(cond, then_blk, &[], else_blk, &[]);
///
/// b.switch_to(then_blk);
/// b.jump(join, &[bb]);
///
/// b.switch_to(else_blk);
/// b.jump(join, &[a]);
///
/// b.switch_to(join);
/// let result = b.block_params(join)[0];
/// b.ret(Some(result));
///
/// let func = b.finish();
/// assert!(func.validate().is_ok());
/// ```
pub struct Builder {
    name: String,
    params: Vec<Type>,
    ret: Type,
    entry: Block,
    blocks: Vec<BlockData>,
    values: Vec<ValueData>,
    current: Block,
}

impl Builder {
    /// Starts a new function with the given name, parameter types, and return type.
    ///
    /// The entry block is created automatically with one parameter per function
    /// parameter; those parameter values are the function's inputs and are read with
    /// [`block_params`](Builder::block_params) on [`entry`](Builder::entry). The
    /// entry block is the current block, so emission can begin immediately.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let b = Builder::new("identity", &[Type::Int], Type::Int);
    /// assert_eq!(b.block_params(b.entry()).len(), 1);
    /// ```
    #[must_use]
    pub fn new(name: impl Into<String>, params: &[Type], ret: Type) -> Self {
        let entry = Block::from_raw(0);
        let mut values = Vec::with_capacity(params.len());
        let mut entry_params = Vec::with_capacity(params.len());
        for &ty in params {
            let value = Value::from_raw(values.len() as u32);
            values.push(ValueData {
                ty,
                def: ValueDef::Param(entry),
            });
            entry_params.push(value);
        }
        let entry_data = BlockData {
            params: entry_params,
            insts: Vec::new(),
            term: None,
        };
        Self {
            name: name.into(),
            params: params.to_vec(),
            ret,
            entry,
            blocks: alloc::vec![entry_data],
            values,
            current: entry,
        }
    }

    /// Returns the entry block.
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
    pub const fn entry(&self) -> Block {
        self.entry
    }

    /// Returns the block that emission currently targets — the one the next
    /// instruction or terminator is added to.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Unit);
    /// let next = b.create_block(&[]);
    /// b.switch_to(next);
    /// assert_eq!(b.current_block(), next);
    /// ```
    #[must_use]
    pub const fn current_block(&self) -> Block {
        self.current
    }

    /// Creates a new block with the given parameter types and returns its handle.
    ///
    /// Block parameters are how a value crosses a control-flow join in SSA form:
    /// each predecessor passes a matching argument on its
    /// [`jump`](Builder::jump) or [`branch`](Builder::branch). The new block does
    /// not become current — call [`switch_to`](Builder::switch_to) to emit into it.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Int);
    /// let join = b.create_block(&[Type::Int]);
    /// assert_eq!(b.block_params(join).len(), 1);
    /// ```
    pub fn create_block(&mut self, params: &[Type]) -> Block {
        let block = Block::from_raw(self.blocks.len() as u32);
        let mut block_params = Vec::with_capacity(params.len());
        for &ty in params {
            let value = Value::from_raw(self.values.len() as u32);
            self.values.push(ValueData {
                ty,
                def: ValueDef::Param(block),
            });
            block_params.push(value);
        }
        self.blocks.push(BlockData {
            params: block_params,
            insts: Vec::new(),
            term: None,
        });
        block
    }

    /// Returns a block's parameter values, in order, or an empty slice if the block
    /// handle is out of range.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let b = Builder::new("f", &[Type::Bool], Type::Unit);
    /// assert_eq!(b.block_params(b.entry()).len(), 1);
    /// ```
    #[must_use]
    pub fn block_params(&self, block: Block) -> &[Value] {
        match self.blocks.get(block.index()) {
            Some(data) => &data.params,
            None => &[],
        }
    }

    /// Switches emission to `block`. Subsequent instructions and the terminator are
    /// added to it.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Unit);
    /// let other = b.create_block(&[]);
    /// b.switch_to(other);
    /// b.ret(None);
    /// ```
    pub fn switch_to(&mut self, block: Block) {
        self.current = block;
    }

    /// Emits an integer constant into the current block and returns its value.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Int);
    /// let n = b.iconst(42);
    /// b.ret(Some(n));
    /// assert_eq!(b.finish().value_type(n), Some(Type::Int));
    /// ```
    pub fn iconst(&mut self, value: i64) -> Value {
        self.push_inst(Inst::Iconst(value), Type::Int)
    }

    /// Emits a floating-point constant into the current block and returns its value.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Float);
    /// let pi = b.fconst(3.14);
    /// b.ret(Some(pi));
    /// assert_eq!(b.finish().value_type(pi), Some(Type::Float));
    /// ```
    pub fn fconst(&mut self, value: f64) -> Value {
        self.push_inst(Inst::Fconst(value), Type::Float)
    }

    /// Emits a boolean constant into the current block and returns its value.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Bool);
    /// let t = b.bconst(true);
    /// b.ret(Some(t));
    /// assert_eq!(b.finish().value_type(t), Some(Type::Bool));
    /// ```
    pub fn bconst(&mut self, value: bool) -> Value {
        self.push_inst(Inst::Bconst(value), Type::Bool)
    }

    /// Emits a binary operation over two values and returns the result.
    ///
    /// The result type follows the operation: a comparison or a logical operation
    /// yields [`Bool`](crate::Type::Bool); an arithmetic operation yields the type
    /// of its operands. Whether the operands actually satisfy the operation is
    /// checked by [`validate`](crate::Function::validate), not here.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, BinOp, Type};
    ///
    /// let mut b = Builder::new("f", &[Type::Int, Type::Int], Type::Bool);
    /// let a = b.block_params(b.entry())[0];
    /// let c = b.block_params(b.entry())[1];
    /// let lt = b.bin(BinOp::Lt, a, c);
    /// b.ret(Some(lt));
    /// assert_eq!(b.finish().value_type(lt), Some(Type::Bool));
    /// ```
    pub fn bin(&mut self, op: BinOp, lhs: Value, rhs: Value) -> Value {
        let ty = if op.is_comparison() || op.is_logical() {
            Type::Bool
        } else {
            self.value_ty(lhs)
        };
        self.push_inst(Inst::Bin(op, lhs, rhs), ty)
    }

    /// Emits a unary operation over one value and returns the result.
    ///
    /// [`Neg`](crate::UnOp::Neg) yields the operand's type;
    /// [`Not`](crate::UnOp::Not) yields [`Bool`](crate::Type::Bool).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, UnOp, Type};
    ///
    /// let mut b = Builder::new("f", &[Type::Int], Type::Int);
    /// let x = b.block_params(b.entry())[0];
    /// let neg = b.un(UnOp::Neg, x);
    /// b.ret(Some(neg));
    /// assert_eq!(b.finish().value_type(neg), Some(Type::Int));
    /// ```
    pub fn un(&mut self, op: UnOp, operand: Value) -> Value {
        let ty = match op {
            UnOp::Neg => self.value_ty(operand),
            UnOp::Not => Type::Bool,
        };
        self.push_inst(Inst::Un(op, operand), ty)
    }

    /// Sets the current block's terminator to a return.
    ///
    /// `Some(v)` returns the value `v`; `None` returns from a function whose return
    /// type is [`Unit`](crate::Type::Unit).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Unit);
    /// b.ret(None);
    /// assert!(b.finish().validate().is_ok());
    /// ```
    pub fn ret(&mut self, value: Option<Value>) {
        self.set_terminator(Terminator::Return(value));
    }

    /// Sets the current block's terminator to an unconditional jump, passing one
    /// argument per parameter of the target block.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Int);
    /// let exit = b.create_block(&[Type::Int]);
    /// let n = b.iconst(7);
    /// b.jump(exit, &[n]);
    /// b.switch_to(exit);
    /// let r = b.block_params(exit)[0];
    /// b.ret(Some(r));
    /// assert!(b.finish().validate().is_ok());
    /// ```
    pub fn jump(&mut self, target: Block, args: &[Value]) {
        self.set_terminator(Terminator::Jump(target, args.to_vec()));
    }

    /// Sets the current block's terminator to a conditional branch.
    ///
    /// Control takes `then_block` (with `then_args`) when `cond` is true, and
    /// `else_block` (with `else_args`) otherwise. Each arm's arguments are matched
    /// against the parameters of the block it targets.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[Type::Bool], Type::Unit);
    /// let cond = b.block_params(b.entry())[0];
    /// let yes = b.create_block(&[]);
    /// let no = b.create_block(&[]);
    /// b.branch(cond, yes, &[], no, &[]);
    /// b.switch_to(yes);
    /// b.ret(None);
    /// b.switch_to(no);
    /// b.ret(None);
    /// assert!(b.finish().validate().is_ok());
    /// ```
    pub fn branch(
        &mut self,
        cond: Value,
        then_block: Block,
        then_args: &[Value],
        else_block: Block,
        else_args: &[Value],
    ) {
        self.set_terminator(Terminator::Branch {
            cond,
            then_block,
            then_args: then_args.to_vec(),
            else_block,
            else_args: else_args.to_vec(),
        });
    }

    /// Finishes construction and returns the assembled [`Function`].
    ///
    /// The function is not validated by this call; run
    /// [`Function::validate`](crate::Function::validate) on the result before relying
    /// on it.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Builder, Type};
    ///
    /// let mut b = Builder::new("f", &[], Type::Unit);
    /// b.ret(None);
    /// let func = b.finish();
    /// assert_eq!(func.block_count(), 1);
    /// ```
    #[must_use]
    pub fn finish(self) -> Function {
        Function::from_parts(
            self.name,
            self.params,
            self.ret,
            self.entry,
            self.blocks,
            self.values,
        )
    }

    fn value_ty(&self, value: Value) -> Type {
        self.values
            .get(value.index())
            .map_or(Type::Unit, |data| data.ty)
    }

    fn push_inst(&mut self, inst: Inst, ty: Type) -> Value {
        let value = Value::from_raw(self.values.len() as u32);
        self.values.push(ValueData {
            ty,
            def: ValueDef::Inst(self.current, inst),
        });
        if let Some(block) = self.blocks.get_mut(self.current.index()) {
            block.insts.push(value);
        }
        value
    }

    fn set_terminator(&mut self, term: Terminator) {
        if let Some(block) = self.blocks.get_mut(self.current.index()) {
            block.term = Some(term);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_seeds_entry_with_function_params() {
        let b = Builder::new("f", &[Type::Int, Type::Bool], Type::Unit);
        let params = b.block_params(b.entry());
        assert_eq!(params.len(), 2);
        assert_eq!(b.current_block(), b.entry());
    }

    #[test]
    fn test_bin_result_type_follows_operation() {
        let mut b = Builder::new("f", &[Type::Int, Type::Int], Type::Bool);
        let a = b.block_params(b.entry())[0];
        let c = b.block_params(b.entry())[1];
        let sum = b.bin(BinOp::Add, a, c);
        let cmp = b.bin(BinOp::Lt, a, c);
        let and = b.bin(BinOp::And, cmp, cmp);
        b.ret(Some(cmp));
        let func = b.finish();
        assert_eq!(func.value_type(sum), Some(Type::Int));
        assert_eq!(func.value_type(cmp), Some(Type::Bool));
        assert_eq!(func.value_type(and), Some(Type::Bool));
    }

    #[test]
    fn test_un_result_type_follows_operation() {
        let mut b = Builder::new("f", &[Type::Int, Type::Bool], Type::Int);
        let x = b.block_params(b.entry())[0];
        let flag = b.block_params(b.entry())[1];
        let neg = b.un(UnOp::Neg, x);
        let not = b.un(UnOp::Not, flag);
        b.ret(Some(neg));
        let func = b.finish();
        assert_eq!(func.value_type(neg), Some(Type::Int));
        assert_eq!(func.value_type(not), Some(Type::Bool));
    }

    #[test]
    fn test_handles_are_dense_from_zero() {
        let mut b = Builder::new("f", &[Type::Int], Type::Int);
        let p = b.block_params(b.entry())[0];
        let one = b.iconst(1);
        let two = b.iconst(2);
        assert_eq!(p.index(), 0);
        assert_eq!(one.index(), 1);
        assert_eq!(two.index(), 2);
    }
}
