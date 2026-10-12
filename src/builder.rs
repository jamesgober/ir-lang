//! The [`Builder`]: lowering into a function body, and rewriting it.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::entity::{AggId, Block, FuncId, GlobalId, Inst, NONE, StackSlot, Value, ValueRange};
use crate::error::{BuildError, ValidationError, ValidationErrorKind as K};
use crate::function::{Function, MAX_ENTITIES, SlotData, ValueDef};
use crate::inst::{BlockCall, Call, InstData, MemFlags, MemOrder, RmwOp, SwitchCase, Terminator};
use crate::module::Decls;
use crate::ops::{BinaryOp, CmpOp, ConvOp, Overflow, Policy, UnaryOp};
use crate::ssa::SsaState;
use crate::types::{CallConv, MemType, Signature, Type};
use crate::typing;

static EMPTY_SIG: Signature = Signature {
    params: Vec::new(),
    returns: Vec::new(),
    conv: CallConv::Fast,
};

/// Where the builder inserts the next instruction.
///
/// # Examples
///
/// ```
/// use ir_lang::{Linkage, Module, Position, Signature, Type};
///
/// let mut m = Module::new("m");
/// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
/// let mut b = m.build(f)?;
/// let second = b.iconst(Type::I32, 2)?;
/// let inst = b.func().defining_inst(second).unwrap();
/// b.set_position(Position::Before(inst))?;
/// let first = b.iconst(Type::I32, 1)?;
/// let entry = b.func().entry();
/// let order: Vec<_> = b.func().insts(entry).map(|i| b.func().results(i).first().unwrap()).collect();
/// assert_eq!(order, [first, second]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Position {
    /// Append at the end of the block (before its terminator).
    End(Block),
    /// Insert before the instruction; consecutive insertions stay in order.
    Before(Inst),
    /// Insert after the instruction; the position then moves past each inserted
    /// instruction, so consecutive insertions stay in order.
    After(Inst),
}

/// The renumbering performed by [`Builder::compact`]: old handle to new handle, or
/// `None` for an entity that was dropped (removed, detached, or an alias).
///
/// # Examples
///
/// ```
/// use ir_lang::{Linkage, Module, Signature, Type};
///
/// let mut m = Module::new("m");
/// let f = m.declare_function("f", &Signature::new(&[], &[Type::I32]), Linkage::Export)?;
/// let mut b = m.build(f)?;
/// let dead = b.iconst(Type::I32, 1)?;
/// let live = b.iconst(Type::I32, 2)?;
/// b.ret(&[live])?;
/// let dead_inst = b.func().defining_inst(dead).unwrap();
/// b.remove_inst(dead_inst)?;
/// let remap = b.compact();
/// assert_eq!(remap.value(dead), None);
/// assert_eq!(remap.value(live).map(|v| v.index()), Some(0));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Remap {
    values: Vec<u32>,
    blocks: Vec<u32>,
    insts: Vec<u32>,
}

impl Remap {
    /// The new handle of a value, if it survived.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// b.ret(&[])?;
    /// let remap = b.compact();
    /// assert_eq!(remap.value(x), Some(x));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn value(&self, old: Value) -> Option<Value> {
        self.values
            .get(old.index())
            .filter(|&&v| v != NONE)
            .map(|&v| Value::from_u32(v))
    }

    /// The new handle of a block, if it survived.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let dead = b.create_block(&[])?;
    /// b.ret(&[])?;
    /// b.remove_block(dead)?;
    /// let entry = b.func().entry();
    /// let remap = b.compact();
    /// assert_eq!(remap.block(dead), None);
    /// assert_eq!(remap.block(entry), Some(entry));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn block(&self, old: Block) -> Option<Block> {
        self.blocks
            .get(old.index())
            .filter(|&&v| v != NONE)
            .map(|&v| Block::from_u32(v))
    }

    /// The new handle of an instruction, if it survived.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[Type::U8]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let v = b.iconst(Type::U8, 1)?;
    /// b.ret(&[v])?;
    /// let i = b.func().defining_inst(v).unwrap();
    /// assert_eq!(b.compact().inst(i).map(|i| i.index()), Some(0));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn inst(&self, old: Inst) -> Option<Inst> {
        self.insts
            .get(old.index())
            .filter(|&&v| v != NONE)
            .map(|&v| Inst::from_u32(v))
    }
}

/// Builds and rewrites one function body.
///
/// A front end lowers into IR by walking its own tree and calling one method per
/// construct (the shape of Cranelift's `FunctionBuilder` or LLVM's `IRBuilder`);
/// an optimization pass uses the same builder to rewrite. Every instruction and
/// terminator is type-checked as it is added (by the same code the validator runs),
/// so a lowering bug fails at the call that caused it, with nothing changed. Result
/// types are derived from the operands.
///
/// Get one from [`Module::build`](crate::Module::build) (a new body) or
/// [`Module::edit`](crate::Module::edit) (an existing body). It borrows the module's
/// declarations, so declare every function, global, signature, and aggregate the
/// body needs first.
///
/// Rewriting keeps handles stable: [`replace_all_uses`](Builder::replace_all_uses),
/// [`remove_inst`](Builder::remove_inst), [`replace_inst`](Builder::replace_inst),
/// [`split_block`](Builder::split_block), and the block-parameter edits never
/// renumber anything; [`compact`](Builder::compact) renumbers densely and returns the
/// mapping. A terminator is never overwritten silently: setting one on a terminated
/// block is [`BuildError::AlreadyTerminated`].
///
/// # Examples
///
/// ```
/// use ir_lang::{CmpOp, Linkage, Module, Overflow, Signature, Type};
///
/// // fn sum(n: i64) -> i64 { let mut s = 0; let mut i = 0; while i < n { s += i; i += 1 } s }
/// let mut m = Module::new("demo");
/// let f = m.declare_function("sum", &Signature::new(&[Type::I64], &[Type::I64]), Linkage::Export)?;
/// let mut b = m.build(f)?;
/// let n = b.param(0)?;
/// let head = b.create_block(&[Type::I64, Type::I64])?;
/// let body = b.create_block(&[])?;
/// let exit = b.create_block(&[])?;
/// let zero = b.iconst(Type::I64, 0)?;
/// b.jump(head, &[zero, zero])?;
///
/// b.switch_to(head)?;
/// let (s, i) = (b.block_param(head, 0)?, b.block_param(head, 1)?);
/// let more = b.compare(CmpOp::Lt, i, n)?;
/// b.branch(more, body, &[], exit, &[])?;
///
/// b.switch_to(body)?;
/// let s2 = b.add(s, i, Overflow::Wrap)?;
/// let one = b.iconst(Type::I64, 1)?;
/// let i2 = b.add(i, one, Overflow::Wrap)?;
/// b.jump(head, &[s2, i2])?;
///
/// b.switch_to(exit)?;
/// b.ret(&[s])?;
/// m.validate()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Builder<'m> {
    pub(crate) decls: &'m Decls,
    pub(crate) func: &'m mut Function,
    pub(crate) id: FuncId,
    pub(crate) pos: Position,
    pub(crate) scratch: Vec<Type>,
    /// SSA construction state, created by the first variable declaration or
    /// block seal.
    pub(crate) ssa: Option<Box<SsaState>>,
}

impl<'m> Builder<'m> {
    pub(crate) fn new(decls: &'m Decls, func: &'m mut Function, id: FuncId) -> Builder<'m> {
        let entry = func.entry();
        Builder {
            decls,
            func,
            id,
            pos: Position::End(entry),
            scratch: Vec::new(),
            ssa: None,
        }
    }

    // ------------------------------------------------------------------ reading

    /// The function being built, for reading.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// assert_eq!(b.func().block_count(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn func(&self) -> &Function {
        self.func
    }

    /// The handle of the function being built.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// assert_eq!(b.id(), f);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn id(&self) -> FuncId {
        self.id
    }

    /// The function's signature.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// assert_eq!(b.signature().returns, [Type::I32]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn signature(&self) -> &Signature {
        self.decls.func_sig(self.id).unwrap_or(&EMPTY_SIG)
    }

    /// The `index`-th function parameter (a parameter of the entry block).
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownParam`] if there is no such parameter.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{BuildError, Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::Ptr], &[]), Linkage::Export)?;
    /// let b = m.build(f)?;
    /// assert_eq!(b.func().value_type(b.param(0)?), Some(Type::Ptr));
    /// assert_eq!(b.param(1), Err(BuildError::UnknownParam { index: 1 }));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn param(&self, index: usize) -> Result<Value, BuildError> {
        self.func
            .block_params(self.func.entry())
            .get(index)
            .copied()
            .ok_or(BuildError::UnknownParam { index })
    }

    /// The `index`-th parameter of `block`.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownBlockParam`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let blk = b.create_block(&[Type::U8])?;
    /// assert_eq!(b.func().value_type(b.block_param(blk, 0)?), Some(Type::U8));
    /// assert!(b.block_param(blk, 1).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn block_param(&self, block: Block, index: usize) -> Result<Value, BuildError> {
        self.func
            .block_params(block)
            .get(index)
            .copied()
            .ok_or(BuildError::UnknownBlockParam { block, index })
    }

    /// The values an instruction defines.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let p = b.param(1)?;
    /// let s = b.store(x, p, ir_lang::MemFlags::new(4))?;
    /// assert!(b.results(s).is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn results(&self, inst: Inst) -> ValueRange {
        self.func.results(inst)
    }

    /// The current insertion position.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// assert_eq!(b.position(), ir_lang::Position::End(b.func().entry()));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn position(&self) -> Position {
        self.pos
    }

    /// The block instructions are being inserted into, if the position is valid.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// assert_eq!(b.current_block(), Some(b.func().entry()));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn current_block(&self) -> Option<Block> {
        match self.pos {
            Position::End(b) => self.func.is_live_block(b).then_some(b),
            Position::Before(i) | Position::After(i) => self.func.inst_block(i),
        }
    }

    /// Checks this function body against the module's declarations, exactly as
    /// [`Module::validate_function`](crate::Module::validate_function) would. Useful
    /// between rewrites.
    ///
    /// # Errors
    ///
    /// The first [`ValidationError`] found.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// assert!(b.validate().is_err());
    /// b.ret(&[])?;
    /// assert!(b.validate().is_ok());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn validate(&self) -> Result<(), ValidationError> {
        crate::validate::validate_body(self.decls, self.id, self.func)
    }

    // --------------------------------------------------------------- positioning

    /// Moves the insertion point.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownBlock`] or [`BuildError::UnknownInst`] if the position
    /// names a removed or unknown entity.
    ///
    /// See [`Position`] for an example.
    pub fn set_position(&mut self, pos: Position) -> Result<(), BuildError> {
        match pos {
            Position::End(b) => {
                if !self.func.is_live_block(b) {
                    return Err(BuildError::UnknownBlock { block: b });
                }
            }
            Position::Before(i) | Position::After(i) => {
                if self.func.inst_block(i).is_none() {
                    return Err(BuildError::UnknownInst { inst: i });
                }
            }
        }
        self.pos = pos;
        Ok(())
    }

    /// Moves the insertion point to the end of `block`.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownBlock`].
    ///
    /// See [`Builder`] for an example.
    pub fn switch_to(&mut self, block: Block) -> Result<(), BuildError> {
        self.set_position(Position::End(block))
    }

    fn current(&self) -> Result<Block, BuildError> {
        self.current_block().ok_or(match self.pos {
            Position::End(block) => BuildError::UnknownBlock { block },
            Position::Before(inst) | Position::After(inst) => BuildError::UnknownInst { inst },
        })
    }

    // ------------------------------------------------------------------- blocks

    /// Creates a block with parameters of the given types. It is not entered;
    /// use [`switch_to`](Builder::switch_to).
    ///
    /// # Errors
    ///
    /// [`BuildError::CapacityExceeded`].
    ///
    /// See [`Builder`] for an example.
    pub fn create_block(&mut self, params: &[Type]) -> Result<Block, BuildError> {
        if self.func.values.len() + params.len() > MAX_ENTITIES as usize {
            return Err(BuildError::CapacityExceeded);
        }
        let block = self.func.push_block().ok_or(BuildError::CapacityExceeded)?;
        for &ty in params {
            let _ = self.add_block_param(block, ty)?;
        }
        Ok(block)
    }

    /// Appends a parameter to `block`. Every edge into the block must then pass an
    /// argument for it (the validator checks).
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownBlock`], [`BuildError::CapacityExceeded`];
    /// [`BuildError::VariablesActive`] if variable reads already added parameters
    /// to the block (see [`declare_var`](Builder::declare_var)).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let blk = b.create_block(&[])?;
    /// let p = b.add_block_param(blk, Type::F64)?;
    /// assert_eq!(b.func().block_params(blk), &[p]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn add_block_param(&mut self, block: Block, ty: Type) -> Result<Value, BuildError> {
        if !self.func.is_live_block(block) {
            return Err(BuildError::UnknownBlock { block });
        }
        if self
            .ssa
            .as_deref()
            .is_some_and(|st| st.first_ssa.get(block.index()).is_some_and(|&f| f != NONE))
        {
            // Parameters the variable construction added must stay last.
            return Err(BuildError::VariablesActive);
        }
        let index = self.func.block_params(block).len() as u32;
        let v = self
            .func
            .push_value(ty, ValueDef::Param { block, index })
            .ok_or(BuildError::CapacityExceeded)?;
        if let Some(node) = self.func.blocks.get_mut(block.index()) {
            node.params.push(v);
        }
        Ok(v)
    }

    /// Removes the `index`-th parameter of `block` and the matching argument from
    /// every edge into it. The parameter value becomes detached; replace its uses
    /// first. Costs time linear in the number of blocks; to remove many
    /// parameters, use [`remove_block_params`](Builder::remove_block_params),
    /// which removes any number in one pass.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownBlock`], [`BuildError::UnknownBlockParam`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let next = b.create_block(&[Type::I32])?;
    /// let c = b.iconst(Type::I32, 7)?;
    /// b.jump(next, &[c])?;
    /// b.switch_to(next)?;
    /// b.ret(&[])?;
    /// b.remove_block_param(next, 0)?;
    /// assert!(b.func().block_params(next).is_empty());
    /// b.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn remove_block_param(&mut self, block: Block, index: usize) -> Result<(), BuildError> {
        if !self.func.is_live_block(block) {
            return Err(BuildError::UnknownBlock { block });
        }
        let params = &mut self.func.blocks[block.index()].params;
        if index >= params.len() {
            return Err(BuildError::UnknownBlockParam { block, index });
        }
        let removed = params.remove(index);
        let rest: Vec<Value> = params[index..].to_vec();
        if let Some(node) = self.func.values.get_mut(removed.index()) {
            node.def = ValueDef::Detached;
        }
        for (k, v) in rest.into_iter().enumerate() {
            if let Some(node) = self.func.values.get_mut(v.index()) {
                node.def = ValueDef::Param {
                    block,
                    index: (index + k) as u32,
                };
            }
        }
        for node in self.func.blocks.iter_mut().filter(|b| b.live) {
            if let Some(term) = &mut node.term {
                term.for_each_successor_mut(|dest| {
                    if dest.block == block && index < dest.args.len() {
                        let _ = dest.args.remove(index);
                    }
                });
            }
        }
        if let Some(st) = self.ssa.as_deref_mut() {
            st.refresh_first(self.func, block);
        }
        Ok(())
    }

    /// Removes a block: its instructions and parameters are detached and it no
    /// longer appears in the layout. Edges into it must be removed too (the
    /// validator reports any left).
    ///
    /// # Errors
    ///
    /// [`BuildError::EntryBlock`] for the entry block, [`BuildError::UnknownBlock`].
    ///
    /// See [`Function::blocks`](crate::Function::blocks) for an example.
    pub fn remove_block(&mut self, block: Block) -> Result<(), BuildError> {
        if block == self.func.entry() {
            return Err(BuildError::EntryBlock);
        }
        if !self.func.is_live_block(block) {
            return Err(BuildError::UnknownBlock { block });
        }
        let insts: Vec<Inst> = self.func.insts(block).collect();
        for inst in insts {
            self.detach_inst(inst);
        }
        if let (Some(st), Some(term)) = (
            self.ssa.as_deref_mut(),
            self.func.blocks[block.index()].term.as_ref(),
        ) {
            st.remove_edges(block, term);
        }
        let node = &mut self.func.blocks[block.index()];
        node.live = false;
        node.term = None;
        let params = core::mem::take(&mut node.params);
        for p in params {
            if let Some(v) = self.func.values.get_mut(p.index()) {
                v.def = ValueDef::Detached;
            }
        }
        if self.current_block().is_none() {
            self.pos = Position::End(self.func.entry());
        }
        Ok(())
    }

    /// Splits the block containing `at`: `at` and every instruction after it, and
    /// the terminator, move to a new block (with no parameters), and the original
    /// block ends with a jump to it. The new block is dominated by the original, so
    /// every use stays valid. No handle changes.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownInst`]; [`BuildError::VariablesActive`] while
    /// variable construction ([`declare_var`](Builder::declare_var)) has unsealed
    /// blocks. Once every block is sealed, splitting ends the construction: the
    /// builder's variables are unknown afterwards.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Terminator, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.iconst(Type::I32, 1)?;
    /// let y = b.iconst(Type::I32, 2)?;
    /// b.ret(&[y])?;
    /// let at = b.func().defining_inst(y).unwrap();
    /// let tail = b.split_block(at)?;
    /// let entry = b.func().entry();
    /// assert!(matches!(b.func().terminator(entry), Some(Terminator::Jump(_))));
    /// assert_eq!(b.func().insts(tail).count(), 1);
    /// b.validate()?;
    /// # let _ = x;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn split_block(&mut self, at: Inst) -> Result<Block, BuildError> {
        if let Some(st) = self.ssa.as_deref() {
            // The variables' definitions are recorded per block, so which of
            // them belong before `at` is not known: splitting ends the
            // construction, which must be complete.
            if !st.is_complete(self.func) {
                return Err(BuildError::VariablesActive);
            }
            self.ssa = None;
        }
        let block = self
            .func
            .inst_block(at)
            .ok_or(BuildError::UnknownInst { inst: at })?;
        let new = self.func.push_block().ok_or(BuildError::CapacityExceeded)?;
        let mut moving = Vec::new();
        let mut cur = Some(at);
        while let Some(i) = cur {
            moving.push(i);
            cur = self.func.next_inst(i);
        }
        for i in moving {
            self.func.unlink(i);
            self.func.link_at_end(i, new);
        }
        let term = self.func.blocks[block.index()].term.take();
        self.func.blocks[new.index()].term = term;
        self.func.blocks[block.index()].term = Some(Terminator::Jump(BlockCall::new(new, &[])));
        Ok(new)
    }

    // -------------------------------------------------------------- instructions

    /// Inserts an instruction at the current position, after checking it; returns
    /// its handle. Its results are fresh, consecutive values typed from the
    /// operands.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] if the instruction is ill-typed or names unknown
    /// entities (including an `error` policy, which belongs in a `check`
    /// terminator); [`BuildError::UnknownBlock`] / [`BuildError::UnknownInst`] for a
    /// stale position; [`BuildError::CapacityExceeded`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{BinaryOp, InstData, Linkage, Module, Overflow, Policy, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::U32], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let inst = b.insert(InstData::Binary { op: BinaryOp::Mul, policy: Policy::overflow(Overflow::Wrap), args: [x, x] })?;
    /// assert_eq!(b.func().value_type(b.results(inst).first().unwrap()), Some(Type::U32));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn insert(&mut self, data: InstData) -> Result<Inst, BuildError> {
        self.scratch.clear();
        typing::inst_types(self.decls, self.func, &data, false, &mut self.scratch)?;
        let block = self.current()?;
        if self.func.insts.len() >= MAX_ENTITIES as usize
            || self.func.values.len() + self.scratch.len() > MAX_ENTITIES as usize
        {
            return Err(BuildError::CapacityExceeded);
        }
        let inst = self
            .func
            .push_inst(data)
            .ok_or(BuildError::CapacityExceeded)?;
        let start = self.func.values.len() as u32;
        for (i, &ty) in self.scratch.iter().enumerate() {
            let _ = self.func.push_value(
                ty,
                ValueDef::Result {
                    inst,
                    index: i as u32,
                },
            );
        }
        self.func.insts[inst.index()].results = ValueRange {
            start,
            len: self.scratch.len() as u32,
        };
        match self.pos {
            Position::End(_) => self.func.link_at_end(inst, block),
            Position::Before(at) => self.func.link_before(inst, at),
            Position::After(at) => {
                self.func.link_after(inst, at);
                self.pos = Position::After(inst);
            }
        }
        Ok(inst)
    }

    fn insert1(&mut self, data: InstData) -> Result<Value, BuildError> {
        let inst = self.insert(data)?;
        self.func
            .results(inst)
            .first()
            .ok_or(BuildError::Invalid(K::ResultMismatch))
    }

    /// An integer constant. `value` must be in the range of `ty`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] with [`ValidationErrorKind::BadConst`](crate::ValidationErrorKind::BadConst)
    /// if `ty` is not an integer type or `value` does not fit.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// assert!(b.iconst(Type::U8, 255).is_ok());
    /// assert!(b.iconst(Type::U8, 256).is_err());
    /// assert!(b.iconst(Type::I8, -128).is_ok());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn iconst(&mut self, ty: Type, value: i128) -> Result<Value, BuildError> {
        let bad = BuildError::Invalid(K::BadConst {
            ty,
            bits: value as u64,
        });
        let Some(width) = ty.int_bits() else {
            return Err(bad);
        };
        let (lo, hi) = if ty.is_signed_int() {
            (-(1i128 << (width - 1)), (1i128 << (width - 1)) - 1)
        } else {
            (0, (1i128 << width) - 1)
        };
        if value < lo || value > hi {
            return Err(bad);
        }
        let mask: u128 = if width == 64 {
            u64::MAX as u128
        } else {
            (1u128 << width) - 1
        };
        let bits = ((value as u128) & mask) as u64;
        self.insert1(InstData::Const { ty, bits })
    }

    /// An `f64` constant (any bit pattern, NaN payloads included).
    ///
    /// # Errors
    ///
    /// Only a stale position or capacity.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let v = b.f64const(-0.0)?;
    /// assert_eq!(b.func().value_type(v), Some(Type::F64));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn f64const(&mut self, value: f64) -> Result<Value, BuildError> {
        self.insert1(InstData::Const {
            ty: Type::F64,
            bits: value.to_bits(),
        })
    }

    /// An `f32` constant.
    ///
    /// # Errors
    ///
    /// Only a stale position or capacity.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let v = b.f32const(1.5)?;
    /// assert_eq!(b.func().value_type(v), Some(Type::F32));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn f32const(&mut self, value: f32) -> Result<Value, BuildError> {
        self.insert1(InstData::Const {
            ty: Type::F32,
            bits: u64::from(value.to_bits()),
        })
    }

    /// A `bool` constant.
    ///
    /// # Errors
    ///
    /// Only a stale position or capacity.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let t = b.bconst(true)?;
    /// assert_eq!(b.func().value_type(t), Some(Type::Bool));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn bconst(&mut self, value: bool) -> Result<Value, BuildError> {
        self.insert1(InstData::Const {
            ty: Type::Bool,
            bits: u64::from(value),
        })
    }

    /// The null `ptr` or `ref`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] if `ty` is not `ptr` or `ref`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// assert!(b.null(Type::Ref).is_ok());
    /// assert!(b.null(Type::I32).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn null(&mut self, ty: Type) -> Result<Value, BuildError> {
        if !matches!(ty, Type::Ptr | Type::Ref) {
            return Err(BuildError::Invalid(K::BadConst { ty, bits: 0 }));
        }
        self.insert1(InstData::Const { ty, bits: 0 })
    }

    /// A unary operation.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for a bad operand type or policy.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Overflow, Policy, Signature, Type, UnaryOp};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::F64], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let (i, x) = (b.param(0)?, b.param(1)?);
    /// b.unary(UnaryOp::Neg, Policy::overflow(Overflow::Wrap), i)?;
    /// b.unary(UnaryOp::Sqrt, Policy::NONE, x)?;
    /// assert!(b.unary(UnaryOp::Sqrt, Policy::NONE, i).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn unary(&mut self, op: UnaryOp, policy: Policy, arg: Value) -> Result<Value, BuildError> {
        self.insert1(InstData::Unary { op, policy, arg })
    }

    /// A binary operation.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for mismatched or unsupported operand types or a
    /// policy without exactly the required fields.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{BinaryOp, DivZero, Linkage, Module, Overflow, Policy, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I64, Type::I64], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let (x, y) = (b.param(0)?, b.param(1)?);
    /// let p = Policy::overflow(Overflow::Wrap).with_div_zero(DivZero::Trap);
    /// b.binary(BinaryOp::Div, p, x, y)?;
    /// // A missing field is refused.
    /// assert!(b.binary(BinaryOp::Div, Policy::overflow(Overflow::Wrap), x, y).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn binary(
        &mut self,
        op: BinaryOp,
        policy: Policy,
        lhs: Value,
        rhs: Value,
    ) -> Result<Value, BuildError> {
        self.insert1(InstData::Binary {
            op,
            policy,
            args: [lhs, rhs],
        })
    }

    fn arith(
        &mut self,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
        overflow: Overflow,
    ) -> Result<Value, BuildError> {
        let ty = typing::value_ty(self.func, lhs)?;
        let policy = if ty.is_int() {
            Policy::overflow(overflow)
        } else {
            Policy::NONE
        };
        self.binary(op, policy, lhs, rhs)
    }

    /// `lhs + rhs`. For integers the overflow policy is attached; for floats it is
    /// dropped (IEEE addition has no overflow policy).
    ///
    /// # Errors
    ///
    /// As [`binary`](Builder::binary); `Overflow::Error` is refused here (it needs a
    /// [`check`](Builder::check) terminator).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Overflow, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::F64], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let y = b.add(x, x, Overflow::Wrap)?;
    /// assert_eq!(b.func().value_type(y), Some(Type::F64));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn add(&mut self, lhs: Value, rhs: Value, overflow: Overflow) -> Result<Value, BuildError> {
        self.arith(BinaryOp::Add, lhs, rhs, overflow)
    }

    /// `lhs - rhs`, as [`add`](Builder::add).
    ///
    /// # Errors
    ///
    /// As [`add`](Builder::add).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Overflow, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let d = b.sub(x, x, Overflow::Wrap)?;
    /// assert_eq!(b.func().value_type(d), Some(Type::I32));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn sub(&mut self, lhs: Value, rhs: Value, overflow: Overflow) -> Result<Value, BuildError> {
        self.arith(BinaryOp::Sub, lhs, rhs, overflow)
    }

    /// `lhs * rhs`, as [`add`](Builder::add).
    ///
    /// # Errors
    ///
    /// As [`add`](Builder::add).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Overflow, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let sq = b.mul(x, x, Overflow::Trap)?;
    /// assert_eq!(b.func().value_type(sq), Some(Type::I32));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn mul(&mut self, lhs: Value, rhs: Value, overflow: Overflow) -> Result<Value, BuildError> {
        self.arith(BinaryOp::Mul, lhs, rhs, overflow)
    }

    /// A comparison giving `bool`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for mismatched types, or an ordering comparison on a
    /// non-numeric type.
    ///
    /// See [`Builder`] for an example.
    pub fn compare(&mut self, op: CmpOp, lhs: Value, rhs: Value) -> Result<Value, BuildError> {
        self.insert1(InstData::Compare {
            op,
            args: [lhs, rhs],
        })
    }

    /// Fused multiply-add on floats.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] unless all three are the same float type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let a = b.f64const(2.0)?;
    /// let r = b.fma(a, a, a)?;
    /// assert_eq!(b.func().value_type(r), Some(Type::F64));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn fma(&mut self, a: Value, b: Value, c: Value) -> Result<Value, BuildError> {
        self.insert1(InstData::Fma { args: [a, b, c] })
    }

    /// `cond ? if_true : if_false`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] unless `cond` is `bool` and the others share a type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let c = b.bconst(true)?;
    /// let zero = b.iconst(Type::I32, 0)?;
    /// let r = b.select(c, x, zero)?;
    /// assert_eq!(b.func().value_type(r), Some(Type::I32));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn select(
        &mut self,
        cond: Value,
        if_true: Value,
        if_false: Value,
    ) -> Result<Value, BuildError> {
        self.insert1(InstData::Select {
            cond,
            args: [if_true, if_false],
        })
    }

    /// A conversion to `to`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] if the conversion is not allowed between the types,
    /// or the policy is wrong.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{ConvOp, Linkage, Module, Policy, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let wide = b.convert(ConvOp::Sext, Policy::NONE, x, Type::I64)?;
    /// assert_eq!(b.func().value_type(wide), Some(Type::I64));
    /// assert!(b.convert(ConvOp::Sext, Policy::NONE, x, Type::I16).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn convert(
        &mut self,
        op: ConvOp,
        policy: Policy,
        arg: Value,
        to: Type,
    ) -> Result<Value, BuildError> {
        self.insert1(InstData::Convert {
            op,
            policy,
            to,
            arg,
        })
    }

    /// Adds a stack slot to the function.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] with `BadAlignment` unless `align` is a power of
    /// two no larger than [`SlotData::MAX_ALIGN`]; [`BuildError::CapacityExceeded`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let ss = b.stack_slot(32, 16)?;
    /// let addr = b.stack_addr(ss)?;
    /// assert!(b.stack_slot(8, 3).is_err());
    /// # let _ = addr;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn stack_slot(&mut self, size: u32, align: u32) -> Result<StackSlot, BuildError> {
        if !crate::module::valid_align(u64::from(align), u64::from(SlotData::MAX_ALIGN)) {
            return Err(BuildError::Invalid(K::BadAlignment {
                align: u64::from(align),
            }));
        }
        let index = u32::try_from(self.func.slots.len())
            .ok()
            .filter(|&i| i < MAX_ENTITIES)
            .ok_or(BuildError::CapacityExceeded)?;
        self.func.slots.push(SlotData { size, align });
        Ok(StackSlot::from_u32(index))
    }

    /// Adds a stack slot sized and aligned for a memory type.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for an unknown aggregate or one larger than
    /// `u32::MAX` bytes or aligned above [`SlotData::MAX_ALIGN`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Aggregate, Linkage, Module, Signature, SlotData, Type};
    ///
    /// let mut m = Module::new("m");
    /// let pair = m.declare_aggregate(Aggregate::structure(&[Type::I64.into(), Type::I64.into()]))?;
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let ss = b.stack_slot_for(pair.into())?;
    /// assert_eq!(b.func().slot(ss), Some(SlotData { size: 16, align: 8 }));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn stack_slot_for(&mut self, ty: MemType) -> Result<StackSlot, BuildError> {
        let layout = self.decls.layout(ty).ok_or(match ty {
            MemType::Agg(agg) => BuildError::Invalid(K::UnknownAggregate { agg }),
            MemType::Scalar(_) => BuildError::Invalid(K::ResultMismatch),
        })?;
        let size = u32::try_from(layout.size()).map_err(|_| match ty {
            MemType::Agg(agg) => BuildError::Invalid(K::BadAggregate { agg }),
            MemType::Scalar(_) => BuildError::CapacityExceeded,
        })?;
        self.stack_slot(size, layout.align())
    }

    /// The address of a stack slot.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for an unknown slot.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let ss = b.stack_slot(8, 8)?;
    /// let p = b.stack_addr(ss)?;
    /// assert_eq!(b.func().value_type(p), Some(Type::Ptr));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn stack_addr(&mut self, slot: StackSlot) -> Result<Value, BuildError> {
        self.insert1(InstData::StackAddr { slot })
    }

    /// The address of a global.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for an unknown global.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Global, Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let g = m.declare_global("g", Global::zeroed(4, 4))?;
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let p = b.global_addr(g)?;
    /// assert_eq!(b.func().value_type(p), Some(Type::Ptr));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn global_addr(&mut self, global: GlobalId) -> Result<Value, BuildError> {
        self.insert1(InstData::GlobalAddr { global })
    }

    /// The address of a function.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for an unknown function.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let p = b.func_addr(f)?;
    /// assert_eq!(b.func().value_type(p), Some(Type::Ptr));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn func_addr(&mut self, func: FuncId) -> Result<Value, BuildError> {
        self.insert1(InstData::FuncAddr { func })
    }

    /// The address of field `field` of the struct `agg` at `base`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] unless `base` is a `ptr` and `agg` is a struct with
    /// that field.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Aggregate, Linkage, MemFlags, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let pt = m.declare_aggregate(Aggregate::structure(&[Type::I32.into(), Type::I32.into()]))?;
    /// let f = m.declare_function("y_of", &Signature::new(&[Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let p = b.param(0)?;
    /// let y_addr = b.field_addr(p, pt, 1)?;
    /// let y = b.load(Type::I32, y_addr, MemFlags::new(4))?;
    /// b.ret(&[y])?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn field_addr(&mut self, base: Value, agg: AggId, field: u32) -> Result<Value, BuildError> {
        self.insert1(InstData::FieldAddr { base, agg, field })
    }

    /// `base + index * size(elem)`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] unless `base` is a `ptr` and `index` an integer.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let (i, base) = (b.param(0)?, b.param(1)?);
    /// let p = b.elem_addr(base, Type::I64.into(), i)?;
    /// assert_eq!(b.func().value_type(p), Some(Type::Ptr));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn elem_addr(
        &mut self,
        base: Value,
        elem: MemType,
        index: Value,
    ) -> Result<Value, BuildError> {
        self.insert1(InstData::ElemAddr { base, elem, index })
    }

    /// `base + offset` bytes.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] unless `base` is a `ptr` and `offset` an integer.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let (off, base) = (b.param(0)?, b.param(1)?);
    /// let p = b.ptr_offset(base, off)?;
    /// assert_eq!(b.func().value_type(p), Some(Type::Ptr));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn ptr_offset(&mut self, base: Value, offset: Value) -> Result<Value, BuildError> {
        self.insert1(InstData::PtrOffset { base, offset })
    }

    /// Loads a `ty` from `addr`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] unless `addr` is a `ptr` and the alignment valid.
    ///
    /// See [`field_addr`](Builder::field_addr) for an example.
    pub fn load(&mut self, ty: Type, addr: Value, flags: MemFlags) -> Result<Value, BuildError> {
        self.insert1(InstData::Load { ty, addr, flags })
    }

    /// Stores `value` to `addr`.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] unless `addr` is a `ptr` and the alignment valid.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, MemFlags, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[Type::I64]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let ss = b.stack_slot(8, 8)?;
    /// let p = b.stack_addr(ss)?;
    /// let v = b.iconst(Type::I64, 42)?;
    /// b.store(v, p, MemFlags::new(8))?;
    /// let back = b.load(Type::I64, p, MemFlags::new(8))?;
    /// b.ret(&[back])?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn store(
        &mut self,
        value: Value,
        addr: Value,
        flags: MemFlags,
    ) -> Result<Inst, BuildError> {
        self.insert(InstData::Store { value, addr, flags })
    }

    /// Atomically loads a `ty` (integer or `ptr`).
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for a bad type or a `release`/`acq_rel` ordering.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, MemOrder, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let p = b.param(1)?;
    /// let v = b.atomic_load(Type::I32, p, MemOrder::Acquire)?;
    /// assert!(b.atomic_load(Type::I32, p, MemOrder::Release).is_err());
    /// # let _ = v;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn atomic_load(
        &mut self,
        ty: Type,
        addr: Value,
        order: MemOrder,
    ) -> Result<Value, BuildError> {
        self.insert1(InstData::AtomicLoad { ty, addr, order })
    }

    /// Atomically stores `value` (integer or `ptr`).
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for a bad type or an `acquire`/`acq_rel` ordering.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, MemOrder, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let (x, p) = (b.param(0)?, b.param(1)?);
    /// b.atomic_store(x, p, MemOrder::Release)?;
    /// assert!(b.atomic_store(x, p, MemOrder::Acquire).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn atomic_store(
        &mut self,
        value: Value,
        addr: Value,
        order: MemOrder,
    ) -> Result<Inst, BuildError> {
        self.insert(InstData::AtomicStore { value, addr, order })
    }

    /// Atomic read-modify-write; gives the old value.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for a bad type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, MemOrder, Module, RmwOp, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let (x, p) = (b.param(0)?, b.param(1)?);
    /// let old = b.atomic_rmw(RmwOp::Add, p, x, MemOrder::SeqCst)?;
    /// assert_eq!(b.func().value_type(old), Some(Type::I32));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn atomic_rmw(
        &mut self,
        op: RmwOp,
        addr: Value,
        value: Value,
        order: MemOrder,
    ) -> Result<Value, BuildError> {
        self.insert1(InstData::AtomicRmw {
            op,
            addr,
            value,
            order,
        })
    }

    /// Atomic compare-exchange; the instruction's results are the old value and a
    /// `bool` that is true if the exchange happened.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for a bad type or ordering pair.
    ///
    /// See [`ValueRange`] for an example.
    pub fn cmpxchg(
        &mut self,
        addr: Value,
        expected: Value,
        new: Value,
        success: MemOrder,
        failure: MemOrder,
    ) -> Result<Inst, BuildError> {
        self.insert(InstData::CmpXchg {
            addr,
            expected,
            new,
            success,
            failure,
        })
    }

    /// A memory fence.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for `relaxed`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, MemOrder, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// b.fence(MemOrder::SeqCst)?;
    /// assert!(b.fence(MemOrder::Relaxed).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn fence(&mut self, order: MemOrder) -> Result<Inst, BuildError> {
        self.insert(InstData::Fence { order })
    }

    /// Copies `len` bytes between non-overlapping ranges.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] unless both addresses are `ptr` and `len` is an
    /// unsigned integer.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, MemFlags, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let p = b.param(1)?;
    /// let ss = b.stack_slot(16, 8)?;
    /// let dst = b.stack_addr(ss)?;
    /// let n = b.iconst(Type::U64, 16)?;
    /// b.memcpy(dst, p, n, MemFlags::new(1))?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn memcpy(
        &mut self,
        dst: Value,
        src: Value,
        len: Value,
        flags: MemFlags,
    ) -> Result<Inst, BuildError> {
        self.insert(InstData::MemCopy {
            dst,
            src,
            len,
            flags,
        })
    }

    /// Copies `len` bytes between possibly overlapping ranges.
    ///
    /// # Errors
    ///
    /// As [`memcpy`](Builder::memcpy).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, MemFlags, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let p = b.param(1)?;
    /// let n = b.iconst(Type::U32, 3)?;
    /// b.memmove(p, p, n, MemFlags::new(1))?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn memmove(
        &mut self,
        dst: Value,
        src: Value,
        len: Value,
        flags: MemFlags,
    ) -> Result<Inst, BuildError> {
        self.insert(InstData::MemMove {
            dst,
            src,
            len,
            flags,
        })
    }

    /// Sets `len` bytes at `dst` to `byte` (a `u8`).
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for bad operand types.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, MemFlags, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let p = b.param(1)?;
    /// let zero = b.iconst(Type::U8, 0)?;
    /// let n = b.iconst(Type::U64, 8)?;
    /// b.memset(p, zero, n, MemFlags::new(8))?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn memset(
        &mut self,
        dst: Value,
        byte: Value,
        len: Value,
        flags: MemFlags,
    ) -> Result<Inst, BuildError> {
        self.insert(InstData::MemSet {
            dst,
            byte,
            len,
            flags,
        })
    }

    /// A call. Its results are the callee's return values, then one relocated
    /// `ref` per GC-list entry.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] for an unknown callee, argument count or type
    /// mismatch, or a bad GC list.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Call, Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let sq = m.declare_function("square", &Signature::new(&[Type::I32], &[Type::I32]), Linkage::Import)?;
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let call = b.call(Call::direct(sq, &[x]))?;
    /// let r = b.results(call).first().unwrap();
    /// b.ret(&[r])?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn call(&mut self, call: Call) -> Result<Inst, BuildError> {
        self.insert(InstData::Call(Box::new(call)))
    }

    /// An explicit safepoint (GC poll) relocating `gc`; its results are the
    /// relocated references.
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] if a GC entry is not a `ref`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("keep", &Signature::new(&[Type::Ref], &[Type::Ref]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let r = b.param(0)?;
    /// let sp = b.safepoint(&[r])?;
    /// let moved = b.results(sp).first().unwrap();
    /// b.ret(&[moved])?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn safepoint(&mut self, gc: &[Value]) -> Result<Inst, BuildError> {
        self.insert(InstData::Safepoint { gc: gc.into() })
    }

    // --------------------------------------------------------------- terminators

    /// Ends the current block with `term`, after checking it.
    ///
    /// With variable construction in use ([`declare_var`](Builder::declare_var)),
    /// an edge passes arguments for its target's own parameters only; the builder
    /// appends the variables' values for the parameters it added. (Variable reads
    /// made to complete an edge stay even if the terminator is then refused.)
    ///
    /// # Errors
    ///
    /// [`BuildError::AlreadyTerminated`] if the block has a terminator (it is never
    /// overwritten); [`BuildError::Invalid`] if the terminator is ill-formed;
    /// [`BuildError::SealedBlock`] for an edge into a sealed block; a stale
    /// position.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Terminator};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// b.set_terminator(Terminator::Return { values: vec![] })?;
    /// assert!(b.set_terminator(Terminator::Unreachable).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn set_terminator(&mut self, mut term: Terminator) -> Result<(), BuildError> {
        let block = self.current()?;
        if self.func.terminator(block).is_some() {
            return Err(BuildError::AlreadyTerminated { block });
        }
        if let Some(st) = self.ssa.as_deref_mut() {
            let entry = self.func.entry();
            if let Some(dest) = term
                .successors()
                .find(|d| d.block != entry && st.is_sealed(d.block))
            {
                return Err(BuildError::SealedBlock { block: dest.block });
            }
            st.complete_new(self.func, block, &mut term)?;
        }
        let sig = self.decls.func_sig(self.id).unwrap_or(&EMPTY_SIG);
        typing::check_terminator(self.decls, self.func, sig, &term, &mut self.scratch)?;
        if let Some(st) = self.ssa.as_deref_mut() {
            st.add_edges(block, &term);
        }
        self.func.blocks[block.index()].term = Some(term);
        Ok(())
    }

    /// Takes the terminator off `block` (to replace it), returning it.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownBlock`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Terminator};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// b.unreachable()?;
    /// let entry = b.func().entry();
    /// assert_eq!(b.remove_terminator(entry)?, Some(Terminator::Unreachable));
    /// b.ret(&[])?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn remove_terminator(&mut self, block: Block) -> Result<Option<Terminator>, BuildError> {
        if !self.func.is_live_block(block) {
            return Err(BuildError::UnknownBlock { block });
        }
        let term = self.func.blocks[block.index()].term.take();
        if let (Some(st), Some(t)) = (self.ssa.as_deref_mut(), term.as_ref()) {
            st.remove_edges(block, t);
        }
        Ok(term)
    }

    /// Ends the block with a jump.
    ///
    /// # Errors
    ///
    /// As [`set_terminator`](Builder::set_terminator).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let next = b.create_block(&[Type::I32])?;
    /// b.jump(next, &[x])?;
    /// b.switch_to(next)?;
    /// let y = b.block_param(next, 0)?;
    /// b.ret(&[y])?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn jump(&mut self, block: Block, args: &[Value]) -> Result<(), BuildError> {
        self.set_terminator(Terminator::Jump(BlockCall::new(block, args)))
    }

    /// Ends the block with a two-way branch on a `bool`.
    ///
    /// # Errors
    ///
    /// As [`set_terminator`](Builder::set_terminator).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{CmpOp, Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let zero = b.iconst(Type::I32, 0)?;
    /// let neg = b.compare(CmpOp::Lt, x, zero)?;
    /// let (t, e) = (b.create_block(&[])?, b.create_block(&[])?);
    /// b.branch(neg, t, &[], e, &[])?;
    /// b.switch_to(t)?;
    /// b.ret(&[zero])?;
    /// b.switch_to(e)?;
    /// b.ret(&[x])?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn branch(
        &mut self,
        cond: Value,
        then_block: Block,
        then_args: &[Value],
        else_block: Block,
        else_args: &[Value],
    ) -> Result<(), BuildError> {
        self.set_terminator(Terminator::Branch {
            cond,
            then_dest: BlockCall::new(then_block, then_args),
            else_dest: BlockCall::new(else_block, else_args),
        })
    }

    /// Ends the block with a multi-way branch on an integer.
    ///
    /// # Errors
    ///
    /// As [`set_terminator`](Builder::set_terminator); repeated or out-of-range
    /// case values are refused.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Block, BlockCall, Linkage, Module, Signature, SwitchCase, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::U8], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let one = b.create_block(&[])?;
    /// let other = b.create_block(&[])?;
    /// let cases = vec![SwitchCase { value: 1, dest: BlockCall::new(one, &[]) }];
    /// b.switch(x, cases, BlockCall::new(other, &[]))?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn switch(
        &mut self,
        value: Value,
        cases: Vec<SwitchCase>,
        default: BlockCall,
    ) -> Result<(), BuildError> {
        self.set_terminator(Terminator::Switch {
            value,
            cases,
            default,
        })
    }

    /// Ends the block with a return.
    ///
    /// # Errors
    ///
    /// As [`set_terminator`](Builder::set_terminator); the values must match the
    /// signature's returns.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// assert!(b.ret(&[]).is_err()); // one return value is required
    /// b.ret(&[x])?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn ret(&mut self, values: &[Value]) -> Result<(), BuildError> {
        self.set_terminator(Terminator::Return {
            values: values.to_vec(),
        })
    }

    /// Ends the block as unreachable.
    ///
    /// # Errors
    ///
    /// As [`set_terminator`](Builder::set_terminator).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// b.unreachable()?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn unreachable(&mut self) -> Result<(), BuildError> {
        self.set_terminator(Terminator::Unreachable)
    }

    /// Ends the block with a deterministic trap.
    ///
    /// # Errors
    ///
    /// As [`set_terminator`](Builder::set_terminator).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// b.trap(42)?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn trap(&mut self, code: u32) -> Result<(), BuildError> {
        self.set_terminator(Terminator::Trap { code })
    }

    /// Ends the block with a tail call.
    ///
    /// # Errors
    ///
    /// As [`set_terminator`](Builder::set_terminator); the callee's returns and
    /// convention must equal this function's and the GC list must be empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Call, Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let (x, p) = (b.param(0)?, b.param(1)?);
    /// b.tail_call(Call::direct(f, &[x, p]))?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn tail_call(&mut self, call: Call) -> Result<(), BuildError> {
        self.set_terminator(Terminator::TailCall(Box::new(call)))
    }

    /// Ends the block with a call that has an exception edge.
    ///
    /// # Errors
    ///
    /// As [`set_terminator`](Builder::set_terminator).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{BlockArg, BlockCall, Call, Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let may_throw = m.declare_function("may_throw", &Signature::new(&[], &[Type::I32]), Linkage::Import)?;
    /// let f = m.declare_function("f", &Signature::new(&[], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let ok = b.create_block(&[Type::I32])?;
    /// let pad = b.create_block(&[Type::Ptr])?;
    /// b.invoke(
    ///     Call::direct(may_throw, &[]),
    ///     BlockCall::with_args(ok, &[BlockArg::Result(0)]),
    ///     BlockCall::with_args(pad, &[BlockArg::Exception]),
    /// )?;
    /// b.switch_to(ok)?;
    /// let r = b.block_param(ok, 0)?;
    /// b.ret(&[r])?;
    /// b.switch_to(pad)?;
    /// let exn = b.block_param(pad, 0)?;
    /// b.resume(exn)?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn invoke(
        &mut self,
        call: Call,
        normal: BlockCall,
        unwind: BlockCall,
    ) -> Result<(), BuildError> {
        self.set_terminator(Terminator::Invoke {
            call: Box::new(call),
            normal,
            unwind,
        })
    }

    /// Ends the block with a fallible operation: a `Unary`, `Binary`, or `Convert`
    /// whose policy has an `error` field (or `char_from_u32`).
    ///
    /// # Errors
    ///
    /// As [`set_terminator`](Builder::set_terminator); an operation that cannot
    /// raise an error is refused (use a plain instruction).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{BinaryOp, BlockArg, BlockCall, InstData, Linkage, Module, Overflow, Policy, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::I32], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let (x, y) = (b.param(0)?, b.param(1)?);
    /// let ok = b.create_block(&[Type::I32])?;
    /// let err = b.create_block(&[Type::U32])?;
    /// b.check(
    ///     InstData::Binary { op: BinaryOp::Add, policy: Policy::overflow(Overflow::Error), args: [x, y] },
    ///     BlockCall::with_args(ok, &[BlockArg::Result(0)]),
    ///     BlockCall::with_args(err, &[BlockArg::ErrorCode]),
    /// )?;
    /// b.switch_to(ok)?;
    /// let sum = b.block_param(ok, 0)?;
    /// b.ret(&[sum])?;
    /// b.switch_to(err)?;
    /// b.trap(1)?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn check(
        &mut self,
        op: InstData,
        normal: BlockCall,
        error: BlockCall,
    ) -> Result<(), BuildError> {
        self.set_terminator(Terminator::Check {
            op: Box::new(op),
            normal,
            error,
        })
    }

    /// Ends the block by unwinding with `payload`.
    ///
    /// # Errors
    ///
    /// As [`set_terminator`](Builder::set_terminator).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Ptr], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let p = b.param(1)?;
    /// b.resume(p)?;
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn resume(&mut self, payload: Value) -> Result<(), BuildError> {
        self.set_terminator(Terminator::Resume { payload })
    }

    // ------------------------------------------------------------------ rewriting

    /// Makes every use of `old` read `new` instead, in O(1): `old` forwards to
    /// `new` (see [`Function::resolve`]). `old`'s own definition is untouched (and
    /// now unused, so it can be removed). Uses added later also forward.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownValue`]; [`BuildError::Invalid`] with
    /// `TypeMismatch` if the types differ.
    ///
    /// See [`Function::resolve`] for an example.
    pub fn replace_all_uses(&mut self, old: Value, new: Value) -> Result<(), BuildError> {
        if old.index() >= self.func.values.len() {
            return Err(BuildError::UnknownValue { value: old });
        }
        if new.index() >= self.func.values.len() {
            return Err(BuildError::UnknownValue { value: new });
        }
        let (r_old, r_new) = (self.func.resolve(old), self.func.resolve(new));
        if r_old == r_new {
            return Ok(());
        }
        let (t_old, t_new) = (
            self.func.values[r_old.index()].ty,
            self.func.values[r_new.index()].ty,
        );
        if t_old != t_new {
            return Err(BuildError::Invalid(K::TypeMismatch {
                value: Some(new),
                expected: t_old,
                found: t_new,
            }));
        }
        // `r_new` does not resolve through `old` (else both would resolve alike),
        // so this can never close a cycle.
        self.func.values[old.index()].alias = r_new.as_u32();
        Ok(())
    }

    fn detach_inst(&mut self, inst: Inst) {
        self.func.unlink(inst);
        let results = self.func.results(inst);
        for v in results {
            if let Some(node) = self.func.values.get_mut(v.index()) {
                node.def = ValueDef::Detached;
            }
        }
    }

    /// Removes an instruction from its block; its results become detached (any
    /// remaining use is a validation error, so replace uses first). The handle stays
    /// reserved.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownInst`] if it is unknown or already removed.
    ///
    /// See [`Remap`] for an example.
    pub fn remove_inst(&mut self, inst: Inst) -> Result<(), BuildError> {
        let block = self
            .func
            .inst_block(inst)
            .ok_or(BuildError::UnknownInst { inst })?;
        let (prev, next) = (self.func.prev_inst(inst), self.func.next_inst(inst));
        self.detach_inst(inst);
        self.pos = match self.pos {
            Position::Before(i) if i == inst => next.map_or(Position::End(block), Position::Before),
            Position::After(i) if i == inst => match (prev, next) {
                (Some(p), _) => Position::After(p),
                (None, Some(n)) => Position::Before(n),
                (None, None) => Position::End(block),
            },
            other => other,
        };
        Ok(())
    }

    /// Replaces an instruction's data in place, keeping its result values. The new
    /// instruction must produce the same number and types of results.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownInst`]; [`BuildError::Invalid`] if the new data is
    /// ill-typed or its results differ (`ResultMismatch`).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{BinaryOp, InstData, Linkage, Module, Overflow, Policy, Shift, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::U32], &[Type::U32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let two = b.iconst(Type::U32, 2)?;
    /// let y = b.mul(x, two, Overflow::Wrap)?;
    /// b.ret(&[y])?;
    /// // Strength reduction: x * 2 => x << 1, same result value.
    /// let one = b.iconst(Type::U32, 1)?;
    /// let inst = b.func().defining_inst(y).unwrap();
    /// b.replace_inst(inst, InstData::Binary { op: BinaryOp::Shl, policy: Policy::shift(Shift::Mask), args: [x, one] })?;
    /// assert!(b.validate().is_err()); // `one` is defined after its new use
    /// let one_inst = b.func().defining_inst(one).unwrap();
    /// b.remove_inst(one_inst)?;
    /// b.set_position(ir_lang::Position::Before(inst))?;
    /// let one2 = b.iconst(Type::U32, 1)?;
    /// b.replace_all_uses(one, one2)?;
    /// b.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn replace_inst(&mut self, inst: Inst, data: InstData) -> Result<(), BuildError> {
        if self.func.inst_block(inst).is_none() {
            return Err(BuildError::UnknownInst { inst });
        }
        self.scratch.clear();
        typing::inst_types(self.decls, self.func, &data, false, &mut self.scratch)?;
        let results = self.func.results(inst);
        let same = results.len() == self.scratch.len()
            && results
                .iter()
                .zip(self.scratch.iter())
                .all(|(v, &t)| self.func.value_type(v) == Some(t));
        if !same {
            return Err(BuildError::Invalid(K::ResultMismatch));
        }
        self.func.insts[inst.index()].data = data;
        Ok(())
    }

    /// Rewrites every operand in the function to its final forwarding target, so
    /// operands can be read without [`Function::resolve`]. Forwarding itself is
    /// kept (a later use of an old value still forwards).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{InstData, Linkage, Module, Overflow, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32], &[Type::I32]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.param(0)?;
    /// let y = b.add(x, x, Overflow::Wrap)?;
    /// let z = b.add(y, y, Overflow::Wrap)?;
    /// b.ret(&[z])?;
    /// b.replace_all_uses(y, x)?;
    /// b.resolve_aliases();
    /// let zi = b.func().defining_inst(z).unwrap();
    /// assert!(matches!(b.func().inst(zi), Some(InstData::Binary { args, .. }) if args == &[x, x]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn resolve_aliases(&mut self) {
        let has_alias = self.func.values.iter().any(|v| v.alias != NONE);
        if !has_alias {
            return;
        }
        let snapshot: Vec<u32> = (0..self.func.values.len() as u32)
            .map(|v| self.func.resolve(Value::from_u32(v)).as_u32())
            .collect();
        let resolve =
            |v: Value| Value::from_u32(snapshot.get(v.index()).copied().unwrap_or(v.as_u32()));
        for node in self.func.insts.iter_mut().filter(|n| n.block != NONE) {
            node.data.map_operands(resolve);
        }
        for node in self.func.blocks.iter_mut().filter(|b| b.live) {
            if let Some(term) = &mut node.term {
                term.map_operands(resolve);
            }
        }
    }

    /// Renumbers the function densely: blocks in layout order (entry first, then
    /// live blocks by handle), instructions in layout order, values in definition
    /// order; drops removed instructions and blocks, detached values, and
    /// forwarding (operands are resolved first). Returns the mapping. The insertion
    /// point moves to the end of the entry block. Variable construction state is
    /// dropped (call [`seal_all_blocks`](Builder::seal_all_blocks) first): the
    /// builder's [`Variable`](crate::Variable)s are unknown afterwards.
    ///
    /// A compacted function prints, parses back, and compares equal.
    ///
    /// See [`Remap`] for an example.
    pub fn compact(&mut self) -> Remap {
        // Renumbering invalidates the variable construction's tables.
        self.ssa = None;
        self.resolve_aliases();
        let remap = crate::compact::compact(self.func);
        self.pos = Position::End(self.func.entry());
        remap
    }

    pub(crate) fn remap_from_parts(values: Vec<u32>, blocks: Vec<u32>, insts: Vec<u32>) -> Remap {
        Remap {
            values,
            blocks,
            insts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{Linkage, Module};

    fn module_with(params: &[Type], returns: &[Type]) -> (Module, FuncId) {
        let mut m = Module::new("t");
        let f = m
            .declare_function("f", &Signature::new(params, returns), Linkage::Export)
            .unwrap_or(FuncId::from_u32(0));
        (m, f)
    }

    #[test]
    fn test_terminator_is_never_overwritten() {
        let (mut m, f) = module_with(&[], &[]);
        let Ok(mut b) = m.build(f) else { return };
        assert_eq!(b.ret(&[]), Ok(()));
        let entry = b.func().entry();
        assert_eq!(
            b.ret(&[]),
            Err(BuildError::AlreadyTerminated { block: entry })
        );
        assert_eq!(
            b.remove_terminator(entry),
            Ok(Some(Terminator::Return { values: Vec::new() }))
        );
        assert_eq!(b.unreachable(), Ok(()));
    }

    #[test]
    fn test_refused_instruction_changes_nothing() {
        let (mut m, f) = module_with(&[Type::I32, Type::I64], &[]);
        let Ok(mut b) = m.build(f) else { return };
        let (x, y) = (
            b.param(0).unwrap_or(Value::from_u32(0)),
            b.param(1).unwrap_or(Value::from_u32(0)),
        );
        let before = (b.func().value_count(), b.func().inst_count());
        assert!(b.add(x, y, Overflow::Wrap).is_err());
        assert!(b.add(x, x, Overflow::Error).is_err());
        assert_eq!((b.func().value_count(), b.func().inst_count()), before);
    }

    #[test]
    fn test_position_after_keeps_insertion_order() {
        let (mut m, f) = module_with(&[], &[]);
        let Ok(mut b) = m.build(f) else { return };
        let a = b.iconst(Type::I32, 1).unwrap_or(Value::from_u32(0));
        let last = b.iconst(Type::I32, 9).unwrap_or(Value::from_u32(0));
        let ai = b.func().defining_inst(a).unwrap_or(Inst::from_u32(0));
        assert!(b.set_position(Position::After(ai)).is_ok());
        let x = b.iconst(Type::I32, 2).unwrap_or(Value::from_u32(0));
        let y = b.iconst(Type::I32, 3).unwrap_or(Value::from_u32(0));
        let entry = b.func().entry();
        let order: Vec<Value> = b
            .func()
            .insts(entry)
            .filter_map(|i| b.func().results(i).first())
            .collect();
        assert_eq!(order, [a, x, y, last]);
    }

    #[test]
    fn test_remove_inst_moves_position_off_it() {
        let (mut m, f) = module_with(&[], &[]);
        let Ok(mut b) = m.build(f) else { return };
        let a = b.iconst(Type::I32, 1).unwrap_or(Value::from_u32(0));
        let ai = b.func().defining_inst(a).unwrap_or(Inst::from_u32(0));
        assert!(b.set_position(Position::After(ai)).is_ok());
        assert!(b.remove_inst(ai).is_ok());
        assert_eq!(b.position(), Position::End(b.func().entry()));
        assert!(b.iconst(Type::I32, 2).is_ok());
        assert_eq!(b.remove_inst(ai), Err(BuildError::UnknownInst { inst: ai }));
    }

    #[test]
    fn test_replace_all_uses_rejects_type_change_and_is_idempotent() {
        let (mut m, f) = module_with(&[Type::I32, Type::I64, Type::I32], &[]);
        let Ok(mut b) = m.build(f) else { return };
        let (x, y, z) = (
            b.param(0).unwrap_or(Value::from_u32(0)),
            b.param(1).unwrap_or(Value::from_u32(0)),
            b.param(2).unwrap_or(Value::from_u32(0)),
        );
        assert!(b.replace_all_uses(x, y).is_err());
        assert!(b.replace_all_uses(x, z).is_ok());
        assert!(b.replace_all_uses(z, x).is_ok()); // already the same: no-op
        assert!(b.replace_all_uses(x, x).is_ok());
        assert_eq!(b.func().resolve(x), z);
        assert_eq!(b.func().resolve(z), z);
        assert_eq!(
            b.replace_all_uses(Value::from_u32(99), x),
            Err(BuildError::UnknownValue {
                value: Value::from_u32(99)
            })
        );
    }

    #[test]
    fn test_remove_block_param_fixes_edges() {
        let (mut m, f) = module_with(&[Type::I32], &[]);
        let Ok(mut b) = m.build(f) else { return };
        let x = b.param(0).unwrap_or(Value::from_u32(0));
        let next = b
            .create_block(&[Type::I32, Type::I32])
            .unwrap_or(Block::from_u32(0));
        assert!(b.jump(next, &[x, x]).is_ok());
        assert!(b.switch_to(next).is_ok());
        assert!(b.ret(&[]).is_ok());
        assert!(b.remove_block_param(next, 0).is_ok());
        assert!(b.validate().is_ok());
        let p = b.block_param(next, 0).unwrap_or(Value::from_u32(0));
        assert_eq!(
            b.func().value_def(p),
            Some(ValueDef::Param {
                block: next,
                index: 0
            })
        );
        assert!(b.remove_block_param(next, 3).is_err());
    }
}
