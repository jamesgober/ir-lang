//! The body of a function: blocks, instructions, values, and stack slots.

use alloc::vec::Vec;

use crate::entity::{Block, Inst, NONE, StackSlot, Value, ValueRange};
use crate::inst::{InstData, Terminator};
use crate::types::Type;

/// Where a value comes from.
///
/// # Examples
///
/// ```
/// use ir_lang::{Linkage, Module, Signature, Type, ValueDef};
///
/// let mut m = Module::new("demo");
/// let f = m.declare_function("f", &Signature::new(&[Type::I32], &[]), Linkage::Export)?;
/// let b = m.build(f)?;
/// let x = b.param(0)?;
/// assert_eq!(b.func().value_def(x), Some(ValueDef::Param { block: b.func().entry(), index: 0 }));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum ValueDef {
    /// The `index`-th parameter of `block`.
    Param {
        /// The block.
        block: Block,
        /// The parameter position.
        index: u32,
    },
    /// The `index`-th result of `inst`.
    Result {
        /// The instruction.
        inst: Inst,
        /// The result position.
        index: u32,
    },
    /// No longer defined: its instruction or block was removed, or its block
    /// parameter was removed. Using a detached value is a validation error.
    Detached,
}

/// The size and alignment of a stack slot, in bytes.
///
/// # Examples
///
/// ```
/// use ir_lang::SlotData;
///
/// let s = SlotData { size: 16, align: 8 };
/// assert_eq!(s.size, 16);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SlotData {
    /// Size in bytes.
    pub size: u32,
    /// Alignment in bytes (a power of two, at most 2^16).
    pub align: u32,
}

impl SlotData {
    /// The largest alignment a stack slot may request.
    pub const MAX_ALIGN: u32 = 1 << 16;
}

#[derive(Clone, Debug)]
pub(crate) struct BlockNode {
    pub(crate) params: Vec<Value>,
    pub(crate) first: u32,
    pub(crate) last: u32,
    pub(crate) term: Option<Terminator>,
    pub(crate) live: bool,
}

impl BlockNode {
    pub(crate) fn new(params: Vec<Value>) -> BlockNode {
        BlockNode {
            params,
            first: NONE,
            last: NONE,
            term: None,
            live: true,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct InstNode {
    pub(crate) data: InstData,
    pub(crate) results: ValueRange,
    pub(crate) prev: u32,
    pub(crate) next: u32,
    /// The containing block, or `NONE` once removed.
    pub(crate) block: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct ValueNode {
    pub(crate) ty: Type,
    pub(crate) def: ValueDef,
    /// The value this one forwards to after `replace_all_uses`, or `NONE`.
    pub(crate) alias: u32,
}

/// The largest number of entities of one kind (values, blocks, instructions, slots)
/// a function may hold.
pub const MAX_ENTITIES: u32 = u32::MAX - 1;

/// A function body in SSA form: a control-flow graph of basic blocks.
///
/// Each block has parameters (the SSA values that flow in along its incoming edges,
/// in place of phi nodes), a sequence of instructions, and one [`Terminator`]. Every
/// value is defined once, by a block parameter or an instruction result, and has
/// one [`Type`]. The entry block's parameters are the function's parameters.
///
/// A `Function` lives in a [`Module`](crate::Module), which holds its declaration
/// (name, [`Signature`](crate::Signature), linkage, attributes). It is created and
/// changed through a [`Builder`](crate::Builder) (`Module::build`, `Module::edit`)
/// and read through the accessors here.
///
/// Instructions of a block form a linked list over a dense arena, so inserting or
/// removing an instruction is O(1) and handles never move. Removed instructions and
/// blocks leave tombstones; [`Builder::compact`](crate::Builder::compact) renumbers densely and
/// reports the mapping.
///
/// # Examples
///
/// ```
/// use ir_lang::{Linkage, Module, Overflow, Signature, Type};
///
/// let mut m = Module::new("demo");
/// let f = m.declare_function("double", &Signature::new(&[Type::I64], &[Type::I64]), Linkage::Export)?;
/// let mut b = m.build(f)?;
/// let x = b.param(0)?;
/// let y = b.add(x, x, Overflow::Wrap)?;
/// b.ret(&[y])?;
///
/// let func = m.function(f).unwrap();
/// let entry = func.entry();
/// assert_eq!(func.block_params(entry), &[x]);
/// assert_eq!(func.insts(entry).count(), 1);
/// assert_eq!(func.value_type(y), Some(Type::I64));
/// m.validate()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct Function {
    pub(crate) blocks: Vec<BlockNode>,
    pub(crate) insts: Vec<InstNode>,
    pub(crate) values: Vec<ValueNode>,
    pub(crate) slots: Vec<SlotData>,
    pub(crate) entry: Block,
}

/// An iterator over the instructions of a block, in order.
#[derive(Clone, Debug)]
pub struct Insts<'f> {
    func: &'f Function,
    next: u32,
    budget: usize,
}

impl Iterator for Insts<'_> {
    type Item = Inst;

    fn next(&mut self) -> Option<Inst> {
        // The budget bounds the walk even if the list were corrupt (it never is
        // through the public API), so iteration always terminates.
        if self.next == NONE || self.budget == 0 {
            return None;
        }
        self.budget -= 1;
        let inst = Inst::from_u32(self.next);
        self.next = self
            .func
            .insts
            .get(self.next as usize)
            .map_or(NONE, |n| n.next);
        Some(inst)
    }
}

impl Function {
    /// A function whose entry block has parameters of the given types.
    pub(crate) fn new(params: &[Type]) -> Function {
        let mut f = Function {
            blocks: Vec::new(),
            insts: Vec::new(),
            values: Vec::new(),
            slots: Vec::new(),
            entry: Block::from_u32(0),
        };
        let mut values = Vec::with_capacity(params.len());
        for (i, &ty) in params.iter().enumerate() {
            let v = Value::from_u32(f.values.len() as u32);
            f.values.push(ValueNode {
                ty,
                def: ValueDef::Param {
                    block: Block::from_u32(0),
                    index: i as u32,
                },
                alias: NONE,
            });
            values.push(v);
        }
        f.blocks.push(BlockNode::new(values));
        f
    }

    /// An empty function with no blocks (the parser fills it in).
    pub(crate) fn empty() -> Function {
        Function {
            blocks: Vec::new(),
            insts: Vec::new(),
            values: Vec::new(),
            slots: Vec::new(),
            entry: Block::from_u32(0),
        }
    }

    /// The entry block, where execution begins. Its parameters are the function's
    /// parameters, and no edge may target it.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let b = m.build(f)?;
    /// assert_eq!(b.func().entry().index(), 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    #[must_use]
    pub const fn entry(&self) -> Block {
        self.entry
    }

    /// The number of block handles ever created (live and removed): block handles
    /// run over `0..block_count()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let _ = b.create_block(&[])?;
    /// assert_eq!(b.func().block_count(), 2);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Iterates over the live blocks in handle order (the entry is block 0 for a
    /// function from the builder).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let extra = b.create_block(&[])?;
    /// b.remove_block(extra)?;
    /// assert_eq!(b.func().blocks().count(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn blocks(&self) -> impl Iterator<Item = Block> + '_ {
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.live)
            .map(|(i, _)| Block::from_u32(i as u32))
    }

    /// Whether `block` exists and has not been removed.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Block, Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let b = m.build(f)?;
    /// assert!(b.func().is_live_block(Block::from_u32(0)));
    /// assert!(!b.func().is_live_block(Block::from_u32(9)));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn is_live_block(&self, block: Block) -> bool {
        self.blocks.get(block.index()).is_some_and(|b| b.live)
    }

    /// The parameters of `block`, or an empty slice for an unknown block.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let blk = b.create_block(&[Type::Bool, Type::F64])?;
    /// assert_eq!(b.func().block_params(blk).len(), 2);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn block_params(&self, block: Block) -> &[Value] {
        self.blocks
            .get(block.index())
            .map_or(&[], |b| b.params.as_slice())
    }

    /// Iterates over the instructions of `block` in order (empty for an unknown
    /// block). The terminator is not an instruction; see
    /// [`terminator`](Function::terminator).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let _ = b.iconst(Type::I32, 1)?;
    /// let _ = b.iconst(Type::I32, 2)?;
    /// let entry = b.func().entry();
    /// assert_eq!(b.func().insts(entry).count(), 2);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn insts(&self, block: Block) -> Insts<'_> {
        let first = self.blocks.get(block.index()).map_or(NONE, |b| b.first);
        Insts {
            func: self,
            next: first,
            budget: self.insts.len(),
        }
    }

    /// The first instruction of `block`, if any.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let b = m.build(f)?;
    /// assert_eq!(b.func().first_inst(b.func().entry()), None);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn first_inst(&self, block: Block) -> Option<Inst> {
        self.blocks
            .get(block.index())
            .and_then(|b| (b.first != NONE).then(|| Inst::from_u32(b.first)))
    }

    /// The last instruction of `block` (before its terminator), if any.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let _ = b.iconst(Type::U8, 1)?;
    /// let entry = b.func().entry();
    /// assert!(b.func().last_inst(entry).is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn last_inst(&self, block: Block) -> Option<Inst> {
        self.blocks
            .get(block.index())
            .and_then(|b| (b.last != NONE).then(|| Inst::from_u32(b.last)))
    }

    /// The instruction after `inst` in its block, if any.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let a = b.iconst(Type::U8, 1)?;
    /// let c = b.iconst(Type::U8, 2)?;
    /// let (ia, ic) = (b.func().defining_inst(a).unwrap(), b.func().defining_inst(c).unwrap());
    /// assert_eq!(b.func().next_inst(ia), Some(ic));
    /// assert_eq!(b.func().prev_inst(ic), Some(ia));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn next_inst(&self, inst: Inst) -> Option<Inst> {
        self.insts
            .get(inst.index())
            .filter(|n| n.block != NONE && n.next != NONE)
            .map(|n| Inst::from_u32(n.next))
    }

    /// The instruction before `inst` in its block, if any.
    ///
    /// See [`next_inst`](Function::next_inst) for an example.
    #[must_use]
    pub fn prev_inst(&self, inst: Inst) -> Option<Inst> {
        self.insts
            .get(inst.index())
            .filter(|n| n.block != NONE && n.prev != NONE)
            .map(|n| Inst::from_u32(n.prev))
    }

    /// The number of instruction handles ever created (live and removed).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let _ = b.iconst(Type::U8, 1)?;
    /// assert_eq!(b.func().inst_count(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn inst_count(&self) -> usize {
        self.insts.len()
    }

    /// The data of `inst` (also for a removed instruction), or `None` for an
    /// unknown handle.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{InstData, Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let v = b.iconst(Type::I8, -1)?;
    /// let inst = b.func().defining_inst(v).unwrap();
    /// assert_eq!(b.func().inst(inst), Some(&InstData::Const { ty: Type::I8, bits: 0xff }));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn inst(&self, inst: Inst) -> Option<&InstData> {
        self.insts.get(inst.index()).map(|n| &n.data)
    }

    /// The block containing `inst`, or `None` if it was removed or is unknown.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let v = b.iconst(Type::I8, 0)?;
    /// let inst = b.func().defining_inst(v).unwrap();
    /// assert_eq!(b.func().inst_block(inst), Some(b.func().entry()));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn inst_block(&self, inst: Inst) -> Option<Block> {
        self.insts
            .get(inst.index())
            .filter(|n| n.block != NONE)
            .map(|n| Block::from_u32(n.block))
    }

    /// The values `inst` defines (empty for an unknown handle).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let v = b.iconst(Type::I8, 0)?;
    /// let inst = b.func().defining_inst(v).unwrap();
    /// assert_eq!(b.func().results(inst).first(), Some(v));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn results(&self, inst: Inst) -> ValueRange {
        self.insts
            .get(inst.index())
            .map_or(ValueRange::default(), |n| n.results)
    }

    /// The terminator of `block`, or `None` if it has none yet (or is unknown).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Terminator};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// b.unreachable()?;
    /// let entry = b.func().entry();
    /// assert_eq!(b.func().terminator(entry), Some(&Terminator::Unreachable));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn terminator(&self, block: Block) -> Option<&Terminator> {
        self.blocks.get(block.index()).and_then(|b| b.term.as_ref())
    }

    /// The number of value handles ever created: value handles run over
    /// `0..value_count()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::I32], &[]), Linkage::Local)?;
    /// let b = m.build(f)?;
    /// assert_eq!(b.func().value_count(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn value_count(&self) -> usize {
        self.values.len()
    }

    /// The type of `value`, or `None` for an unknown handle.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::F32], &[]), Linkage::Local)?;
    /// let b = m.build(f)?;
    /// assert_eq!(b.func().value_type(b.param(0)?), Some(Type::F32));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn value_type(&self, value: Value) -> Option<Type> {
        self.values.get(value.index()).map(|v| v.ty)
    }

    /// Where `value` is defined, or `None` for an unknown handle.
    ///
    /// See [`ValueDef`] for an example.
    #[must_use]
    pub fn value_def(&self, value: Value) -> Option<ValueDef> {
        self.values.get(value.index()).map(|v| v.def)
    }

    /// The instruction defining `value`, if it is an instruction result.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[Type::F32], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let p = b.param(0)?;
    /// let c = b.iconst(Type::U8, 3)?;
    /// assert!(b.func().defining_inst(c).is_some());
    /// assert!(b.func().defining_inst(p).is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn defining_inst(&self, value: Value) -> Option<Inst> {
        match self.value_def(value) {
            Some(ValueDef::Result { inst, .. }) => Some(inst),
            _ => None,
        }
    }

    /// Follows `replace_all_uses` forwarding: the value every use of `value` now
    /// reads. A value that was never replaced resolves to itself.
    ///
    /// Operands stored in instructions are not rewritten eagerly by
    /// [`Builder::replace_all_uses`](crate::Builder::replace_all_uses) (that keeps it
    /// O(1)), so a pass reading operands should resolve them, or call
    /// [`Builder::resolve_aliases`](crate::Builder::resolve_aliases) once first.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let one = b.iconst(Type::I32, 1)?;
    /// let two = b.iconst(Type::I32, 2)?;
    /// b.replace_all_uses(one, two)?;
    /// assert_eq!(b.func().resolve(one), two);
    /// assert_eq!(b.func().resolve(two), two);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn resolve(&self, value: Value) -> Value {
        let mut v = value.as_u32();
        // Alias chains are acyclic by construction (replace_all_uses refuses a
        // cycle); the step bound keeps resolution total on any input regardless.
        for _ in 0..=self.values.len() {
            match self.values.get(v as usize) {
                Some(node) if node.alias != NONE => v = node.alias,
                _ => return Value::from_u32(v),
            }
        }
        Value::from_u32(v)
    }

    /// The number of stack slots.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let _ = b.stack_slot(8, 8)?;
    /// assert_eq!(b.func().slot_count(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// The size and alignment of a stack slot.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature, SlotData};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let ss = b.stack_slot(24, 8)?;
    /// assert_eq!(b.func().slot(ss), Some(SlotData { size: 24, align: 8 }));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn slot(&self, slot: StackSlot) -> Option<SlotData> {
        self.slots.get(slot.index()).copied()
    }

    /// The predecessor blocks of every block, indexed by block handle, listing a
    /// predecessor once per edge (a branch with both edges to one block lists it
    /// twice). Only live blocks contribute edges.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Local)?;
    /// let mut b = m.build(f)?;
    /// let exit = b.create_block(&[])?;
    /// b.jump(exit, &[])?;
    /// b.switch_to(exit)?;
    /// b.ret(&[])?;
    /// let preds = b.func().predecessors();
    /// assert_eq!(preds[exit.index()], [b.func().entry()]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn predecessors(&self) -> Vec<Vec<Block>> {
        let mut preds = alloc::vec![Vec::new(); self.blocks.len()];
        for (i, node) in self.blocks.iter().enumerate() {
            if !node.live {
                continue;
            }
            if let Some(term) = &node.term {
                for dest in term.successors() {
                    if let Some(p) = preds.get_mut(dest.block.index()) {
                        p.push(Block::from_u32(i as u32));
                    }
                }
            }
        }
        preds
    }

    // ----- internal construction helpers (used by the builder and the parser) -----

    pub(crate) fn push_value(&mut self, ty: Type, def: ValueDef) -> Option<Value> {
        let index = u32::try_from(self.values.len())
            .ok()
            .filter(|&i| i < MAX_ENTITIES)?;
        self.values.push(ValueNode {
            ty,
            def,
            alias: NONE,
        });
        Some(Value::from_u32(index))
    }

    pub(crate) fn push_block(&mut self) -> Option<Block> {
        let index = u32::try_from(self.blocks.len())
            .ok()
            .filter(|&i| i < MAX_ENTITIES)?;
        self.blocks.push(BlockNode::new(Vec::new()));
        Some(Block::from_u32(index))
    }

    /// Allocates an unlinked instruction node.
    pub(crate) fn push_inst(&mut self, data: InstData) -> Option<Inst> {
        let index = u32::try_from(self.insts.len())
            .ok()
            .filter(|&i| i < MAX_ENTITIES)?;
        self.insts.push(InstNode {
            data,
            results: ValueRange::default(),
            prev: NONE,
            next: NONE,
            block: NONE,
        });
        Some(Inst::from_u32(index))
    }

    /// Links an unlinked instruction at the end of `block`.
    pub(crate) fn link_at_end(&mut self, inst: Inst, block: Block) {
        let last = match self.blocks.get(block.index()) {
            Some(b) => b.last,
            None => return,
        };
        self.link_between(inst, block, last, NONE);
    }

    /// Links an unlinked instruction immediately before `before`.
    pub(crate) fn link_before(&mut self, inst: Inst, before: Inst) {
        let Some(node) = self.insts.get(before.index()) else {
            return;
        };
        let (block, prev) = (node.block, node.prev);
        if block == NONE {
            return;
        }
        self.link_between(inst, Block::from_u32(block), prev, before.as_u32());
    }

    /// Links an unlinked instruction immediately after `after`.
    pub(crate) fn link_after(&mut self, inst: Inst, after: Inst) {
        let Some(node) = self.insts.get(after.index()) else {
            return;
        };
        let (block, next) = (node.block, node.next);
        if block == NONE {
            return;
        }
        self.link_between(inst, Block::from_u32(block), after.as_u32(), next);
    }

    fn link_between(&mut self, inst: Inst, block: Block, prev: u32, next: u32) {
        let i = inst.as_u32();
        if let Some(n) = self.insts.get_mut(inst.index()) {
            n.prev = prev;
            n.next = next;
            n.block = block.as_u32();
        }
        if prev == NONE {
            if let Some(b) = self.blocks.get_mut(block.index()) {
                b.first = i;
            }
        } else if let Some(p) = self.insts.get_mut(prev as usize) {
            p.next = i;
        }
        if next == NONE {
            if let Some(b) = self.blocks.get_mut(block.index()) {
                b.last = i;
            }
        } else if let Some(n) = self.insts.get_mut(next as usize) {
            n.prev = i;
        }
    }

    /// Unlinks a linked instruction from its block (its node stays in the arena).
    pub(crate) fn unlink(&mut self, inst: Inst) {
        let Some(node) = self.insts.get(inst.index()) else {
            return;
        };
        let (block, prev, next) = (node.block, node.prev, node.next);
        if block == NONE {
            return;
        }
        if prev == NONE {
            if let Some(b) = self.blocks.get_mut(block as usize) {
                b.first = next;
            }
        } else if let Some(p) = self.insts.get_mut(prev as usize) {
            p.next = next;
        }
        if next == NONE {
            if let Some(b) = self.blocks.get_mut(block as usize) {
                b.last = prev;
            }
        } else if let Some(n) = self.insts.get_mut(next as usize) {
            n.prev = prev;
        }
        if let Some(n) = self.insts.get_mut(inst.index()) {
            n.prev = NONE;
            n.next = NONE;
            n.block = NONE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_link_and_unlink_keep_list_consistent() {
        let mut f = Function::new(&[]);
        let entry = f.entry();
        let mut made = Vec::new();
        for _ in 0..3 {
            let i = f
                .push_inst(InstData::Fence {
                    order: crate::inst::MemOrder::SeqCst,
                })
                .unwrap_or(Inst::from_u32(NONE));
            f.link_at_end(i, entry);
            made.push(i);
        }
        assert_eq!(f.insts(entry).collect::<Vec<_>>(), made);
        f.unlink(made[1]);
        assert_eq!(f.insts(entry).collect::<Vec<_>>(), [made[0], made[2]]);
        f.link_before(made[1], made[0]);
        assert_eq!(
            f.insts(entry).collect::<Vec<_>>(),
            [made[1], made[0], made[2]]
        );
        f.unlink(made[2]);
        f.link_after(made[2], made[1]);
        assert_eq!(
            f.insts(entry).collect::<Vec<_>>(),
            [made[1], made[2], made[0]]
        );
        assert_eq!(f.first_inst(entry), Some(made[1]));
        assert_eq!(f.last_inst(entry), Some(made[0]));
    }

    #[test]
    fn test_resolve_terminates_on_a_corrupt_cycle() {
        let mut f = Function::new(&[Type::I32, Type::I32]);
        f.values[0].alias = 1;
        f.values[1].alias = 0;
        // A cycle cannot be built through the API; resolution still terminates.
        let _ = f.resolve(Value::from_u32(0));
    }

    #[test]
    fn test_unknown_handles_read_as_empty() {
        let f = Function::new(&[]);
        assert!(f.block_params(Block::from_u32(5)).is_empty());
        assert_eq!(f.insts(Block::from_u32(5)).count(), 0);
        assert_eq!(f.value_type(Value::from_u32(3)), None);
        assert_eq!(f.inst(Inst::from_u32(0)), None);
        assert!(f.results(Inst::from_u32(0)).is_empty());
    }
}
