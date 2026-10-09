//! The safepoint rules.
//!
//! A *tracked* value is a `ref`, or a value derived from one (the result of
//! `ref_to_ptr`, and anything computed from a tracked value by address arithmetic,
//! conversion, arithmetic, or `select`, or passed into a block parameter). Rules:
//!
//! 1. No tracked value is live across a safepoint. A `ref` needed afterwards goes
//!    through the safepoint's GC list and the relocated result is used instead; a
//!    derived pointer cannot be relocated, so it must be recomputed.
//! 2. A derived (non-`ref`) value never escapes tracking: it is not stored to
//!    memory, returned, passed to a call that is a safepoint or to a tail call, or
//!    used as an exception payload.
//!
//! Liveness is computed per tracked value by walking backwards from each use to
//! the definition (the path-exploration algorithm), so the cost is the size of the
//! tracked values' live ranges, and nothing at all for a function without tracked
//! values or without safepoints.

use alloc::vec::Vec;

use super::dom::Idom;
use super::{Cfg, at};
use crate::entity::{Block, FuncId, Inst, Value};
use crate::error::{ValidationError, ValidationErrorKind as K};
use crate::function::{Function, ValueDef};
use crate::inst::{BlockArg, InstData, Terminator};
use crate::module::Decls;
use crate::ops::ConvOp;
use crate::types::Type;
use crate::typing;

fn is_safepoint_inst(decls: &Decls, data: &InstData) -> bool {
    match data {
        InstData::Call(call) => typing::call_is_safepoint(decls, call),
        InstData::Safepoint { .. } => true,
        _ => false,
    }
}

fn is_safepoint_term(decls: &Decls, term: &Terminator) -> bool {
    matches!(term, Terminator::Invoke { call, .. } if typing::call_is_safepoint(decls, call))
}

/// Instructions through which a tracked operand makes the result derived.
fn propagates(data: &InstData) -> bool {
    matches!(
        data,
        InstData::Unary { .. }
            | InstData::Binary { .. }
            | InstData::Fma { .. }
            | InstData::Convert { .. }
            | InstData::Select { .. }
            | InstData::FieldAddr { .. }
            | InstData::ElemAddr { .. }
            | InstData::PtrOffset { .. }
    )
}

/// Marks every derived value. Returns `None` when the function has none.
fn derived_values(func: &Function, idom: &Idom) -> Option<Vec<bool>> {
    let n = func.values.len();
    let mut derived = alloc::vec![false; n];
    let mut work = Vec::new();
    // Edges from a value to the values computed from it.
    let mut edges: Vec<(u32, u32)> = Vec::new();
    for &b in &idom.rpo {
        let block = Block::from_u32(b);
        for inst in func.insts(block) {
            let node = &func.insts[inst.index()];
            if let InstData::Convert {
                op: ConvOp::RefToPtr,
                ..
            } = node.data
            {
                for v in node.results {
                    if let Some(d) = derived.get_mut(v.index()) {
                        *d = true;
                        work.push(v.as_u32());
                    }
                }
            }
            if propagates(&node.data) {
                node.data.for_each_operand(|op| {
                    for r in node.results {
                        edges.push((func.resolve(op).as_u32(), r.as_u32()));
                    }
                });
            }
        }
        if let Some(term) = func.terminator(block) {
            for dest in term.successors() {
                let params = func.block_params(dest.block);
                for (arg, &p) in dest.args.iter().zip(params) {
                    if let BlockArg::Value(v) = arg {
                        edges.push((func.resolve(*v).as_u32(), p.as_u32()));
                    }
                }
            }
        }
    }
    if work.is_empty() {
        return None;
    }
    edges.sort_unstable();
    while let Some(v) = work.pop() {
        let lo = edges.partition_point(|&(from, _)| from < v);
        for &(from, to) in &edges[lo..] {
            if from != v {
                break;
            }
            if let Some(d) = derived.get_mut(to as usize) {
                if !*d {
                    *d = true;
                    work.push(to);
                }
            }
        }
    }
    Some(derived)
}

/// Rule 2: derived values do not escape.
fn check_escapes(
    decls: &Decls,
    id: FuncId,
    func: &Function,
    idom: &Idom,
    derived: &[bool],
) -> Result<(), ValidationError> {
    let is_derived = |v: Value| {
        derived
            .get(func.resolve(v).index())
            .copied()
            .unwrap_or(false)
    };
    for &b in &idom.rpo {
        let block = Block::from_u32(b);
        for inst in func.insts(block) {
            let escaping: Option<Value> = match &func.insts[inst.index()].data {
                InstData::Store { value, .. } | InstData::AtomicStore { value, .. } => {
                    is_derived(*value).then_some(*value)
                }
                InstData::AtomicRmw { value, .. } => is_derived(*value).then_some(*value),
                InstData::CmpXchg { expected, new, .. } => {
                    [*expected, *new].into_iter().find(|&v| is_derived(v))
                }
                InstData::Call(call) if typing::call_is_safepoint(decls, call) => {
                    call.args.iter().copied().find(|&v| is_derived(v))
                }
                _ => None,
            };
            if let Some(value) = escaping {
                return Err(at(
                    id,
                    Some(block),
                    Some(inst),
                    K::DerivedPointerEscapes { value },
                ));
            }
        }
        let escaping = match func.terminator(block) {
            Some(Terminator::Return { values }) => values.iter().copied().find(|&v| is_derived(v)),
            Some(Terminator::Resume { payload }) => is_derived(*payload).then_some(*payload),
            Some(Terminator::TailCall(call)) => call.args.iter().copied().find(|&v| is_derived(v)),
            Some(Terminator::Invoke { call, .. }) if typing::call_is_safepoint(decls, call) => {
                call.args.iter().copied().find(|&v| is_derived(v))
            }
            _ => None,
        };
        if let Some(value) = escaping {
            return Err(at(
                id,
                Some(block),
                None,
                K::DerivedPointerEscapes { value },
            ));
        }
    }
    Ok(())
}

/// Positions inside a block: parameters are defined at 0, the `k`-th instruction
/// at `k + 1`, the terminator's own operands are read at `len + 1`, and edge
/// arguments are read at `len + 2` (after an `invoke`'s call has returned).
struct Positions {
    /// Position of each instruction in its block.
    inst_pos: Vec<u32>,
    /// Instruction count of each block.
    len: Vec<u32>,
    /// Safepoint positions, compressed per block and sorted.
    sp_start: Vec<u32>,
    sp: Vec<u32>,
}

impl Positions {
    fn build(decls: &Decls, func: &Function, idom: &Idom) -> Positions {
        let nb = func.blocks.len();
        let mut inst_pos = alloc::vec![0u32; func.insts.len()];
        let mut len = alloc::vec![0u32; nb];
        let mut sp_start = alloc::vec![0u32; nb + 1];
        let mut owners: Vec<(u32, u32)> = Vec::new();
        for &b in &idom.rpo {
            let block = Block::from_u32(b);
            let mut k = 0u32;
            for inst in func.insts(block) {
                k += 1;
                inst_pos[inst.index()] = k;
                if is_safepoint_inst(decls, &func.insts[inst.index()].data) {
                    owners.push((b, k));
                }
            }
            len[b as usize] = k;
            if let Some(term) = func.terminator(block) {
                if is_safepoint_term(decls, term) {
                    owners.push((b, k + 1));
                }
            }
        }
        owners.sort_unstable();
        let mut sp = Vec::with_capacity(owners.len());
        for &(b, _) in &owners {
            sp_start[b as usize + 1] += 1;
        }
        for i in 0..nb {
            sp_start[i + 1] += sp_start[i];
        }
        for &(_, p) in &owners {
            sp.push(p);
        }
        Positions {
            inst_pos,
            len,
            sp_start,
            sp,
        }
    }

    fn safepoints(&self, b: usize) -> &[u32] {
        let lo = self.sp_start.get(b).copied().unwrap_or(0) as usize;
        let hi = self.sp_start.get(b + 1).copied().unwrap_or(0) as usize;
        self.sp.get(lo..hi).unwrap_or(&[])
    }

    /// Whether block `b` has a safepoint at a position strictly between `lo` and
    /// `hi`.
    fn crosses(&self, b: usize, lo: u32, hi: u32) -> bool {
        let sps = self.safepoints(b);
        let i = sps.partition_point(|&s| s <= lo);
        sps.get(i).is_some_and(|&s| s < hi)
    }
}

/// Rule 1: no tracked value is live across a safepoint.
pub(crate) fn check_safepoints(
    decls: &Decls,
    id: FuncId,
    func: &Function,
    cfg: &Cfg,
    idom: &Idom,
) -> Result<(), ValidationError> {
    let derived = derived_values(func, idom);
    if let Some(d) = &derived {
        check_escapes(decls, id, func, idom, d)?;
    }
    let pos = Positions::build(decls, func, idom);
    if pos.sp.is_empty() {
        return Ok(());
    }
    let tracked = |v: Value| -> bool {
        func.value_type(v) == Some(Type::Ref)
            || derived
                .as_ref()
                .is_some_and(|d| d.get(v.index()).copied().unwrap_or(false))
    };

    // Uses of tracked values in reachable blocks: (value, block, position).
    let mut uses: Vec<(u32, u32, u32)> = Vec::new();
    for &b in &idom.rpo {
        let block = Block::from_u32(b);
        for inst in func.insts(block) {
            let p = pos.inst_pos[inst.index()];
            func.insts[inst.index()].data.for_each_operand(|v| {
                let r = func.resolve(v);
                if tracked(r) {
                    uses.push((r.as_u32(), b, p));
                }
            });
        }
        if let Some(term) = func.terminator(block) {
            let len = pos.len[b as usize];
            term.for_each_own_operand(|v| {
                let r = func.resolve(v);
                if tracked(r) {
                    uses.push((r.as_u32(), b, len + 1));
                }
            });
            for dest in term.successors() {
                for arg in &dest.args {
                    if let BlockArg::Value(v) = arg {
                        let r = func.resolve(*v);
                        if tracked(r) {
                            uses.push((r.as_u32(), b, len + 2));
                        }
                    }
                }
            }
        }
    }
    if uses.is_empty() {
        return Ok(());
    }
    uses.sort_unstable();

    let preds = cfg.reversed();
    let mut stamp = alloc::vec![u32::MAX; func.blocks.len()];
    let mut work: Vec<u32> = Vec::new();
    let mut i = 0;
    while i < uses.len() {
        let v = uses[i].0;
        let value = Value::from_u32(v);
        let (def_block, def_pos) = match func.value_def(value) {
            Some(ValueDef::Param { block, .. }) => (block.as_u32(), 0),
            Some(ValueDef::Result { inst, .. }) => (
                func.inst_block(inst).map_or(u32::MAX, Block::as_u32),
                pos.inst_pos.get(inst.index()).copied().unwrap_or(0),
            ),
            _ => (u32::MAX, 0),
        };
        let fail = |b: u32, inst: Option<Inst>| {
            Err(at(
                id,
                Some(Block::from_u32(b)),
                inst,
                K::LiveAcrossSafepoint { value },
            ))
        };
        work.clear();
        while i < uses.len() && uses[i].0 == v {
            let (_, b, p) = uses[i];
            i += 1;
            if b == def_block {
                if pos.crosses(b as usize, def_pos, p) {
                    return fail(b, None);
                }
            } else {
                if pos.crosses(b as usize, 0, p) {
                    return fail(b, None);
                }
                if stamp[b as usize] != v {
                    stamp[b as usize] = v;
                    work.push(b);
                }
            }
        }
        // `v` is live into every block on the work list; it is live out of each
        // reachable predecessor.
        while let Some(b) = work.pop() {
            for &p in preds.succs(b as usize) {
                if !idom.reachable(p as usize) {
                    continue;
                }
                let end = pos.len[p as usize] + 2;
                if p == def_block {
                    if pos.crosses(p as usize, def_pos, end) {
                        return fail(p, None);
                    }
                } else {
                    if pos.crosses(p as usize, 0, end) {
                        return fail(p, None);
                    }
                    if stamp[p as usize] != v {
                        stamp[p as usize] = v;
                        work.push(p);
                    }
                }
            }
        }
    }
    Ok(())
}
