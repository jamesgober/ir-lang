//! SSA construction for mutable variables, and the block-parameter machinery it
//! shares with statepoint insertion.
//!
//! The construction is Braun, Buchwald, Hack, Leißa, Mallon, and Zwinkau,
//! "Simple and Efficient Construction of Static Single Assignment Form" (CC
//! 2013), with block parameters in place of phi nodes: a phi of variable `x` at
//! block `b` is a parameter of `b`, and its operands are the arguments every edge
//! into `b` passes for it. A front end names its mutable locals with
//! [`Variable`]s, records assignments with [`Builder::def_var`] and reads with
//! [`Builder::use_var`], and *seals* a block ([`Builder::seal_block`]) once every
//! edge into it exists. A read in a sealed block with one predecessor looks
//! through to it; a read anywhere else adds a parameter (an *incomplete* one in
//! an unsealed block, completed when the block is sealed). Parameters whose
//! operands are all one value (or the parameter itself) are *trivial*;
//! [`Builder::seal_all_blocks`] removes them, to a fixpoint, so the result is the
//! paper's pruned, trivially-minimal SSA.
//!
//! Every walk is iterative. A read walks up single-predecessor chains in a loop
//! and defers the operands of each new parameter to an explicit work stack, so
//! neither deep straight-line code nor deep loop nests recurse. Definitions live
//! in an open-addressing table keyed by `(variable, block)`, so memory is
//! proportional to the definitions made, not to variables × blocks.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::builder::Builder;
use crate::entity::{Block, Inst, NONE, Value, ValueRange, Variable};
use crate::error::{BuildError, ValidationErrorKind as K};
use crate::function::{Function, MAX_ENTITIES, ValueDef};
use crate::inst::{BlockArg, InstData, Terminator};
use crate::types::Type;

// ------------------------------------------------------------------ DefMap

const EMPTY: u64 = u64::MAX;

/// The key of a pair of dense ids.
#[inline]
pub(crate) fn key(a: u32, b: u32) -> u64 {
    (u64::from(a) << 32) | u64::from(b)
}

/// An open-addressing hash map from a pair of dense ids (packed in a `u64`) to a
/// `u32`, with linear probing and no deletion. Entries are only ever inserted or
/// overwritten, which is all the SSA constructions need.
#[derive(Clone, Debug, Default)]
pub(crate) struct DefMap {
    keys: Vec<u64>,
    vals: Vec<u32>,
    len: usize,
}

impl DefMap {
    fn slot(&self, k: u64) -> usize {
        let mask = self.keys.len() - 1;
        let h = k.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut i = (h ^ (h >> 29)) as usize & mask;
        loop {
            let at = self.keys[i];
            if at == k || at == EMPTY {
                return i;
            }
            i = (i + 1) & mask;
        }
    }

    pub(crate) fn get(&self, k: u64) -> Option<u32> {
        if self.keys.is_empty() {
            return None;
        }
        let i = self.slot(k);
        (self.keys[i] == k).then(|| self.vals[i])
    }

    pub(crate) fn insert(&mut self, k: u64, v: u32) {
        // Keep the load at most 3/4 so probes stay short and always end.
        if (self.len + 1) * 4 > self.keys.len() * 3 {
            self.grow();
        }
        let i = self.slot(k);
        if self.keys[i] == EMPTY {
            self.keys[i] = k;
            self.len += 1;
        }
        self.vals[i] = v;
    }

    fn grow(&mut self) {
        let cap = (self.keys.len() * 2).max(16);
        let keys = core::mem::replace(&mut self.keys, alloc::vec![EMPTY; cap]);
        let vals = core::mem::replace(&mut self.vals, alloc::vec![0; cap]);
        self.len = 0;
        for (k, v) in keys.into_iter().zip(vals) {
            if k != EMPTY {
                let i = self.slot(k);
                self.keys[i] = k;
                self.vals[i] = v;
                self.len += 1;
            }
        }
    }
}

// ----------------------------------------------------- shared param helpers

/// The distinct predecessors of every block (live blocks with a terminator),
/// in a deterministic order.
pub(crate) fn distinct_preds(func: &Function) -> Vec<Vec<Block>> {
    let mut preds: Vec<Vec<Block>> = alloc::vec![Vec::new(); func.blocks.len()];
    for (i, node) in func.blocks.iter().enumerate() {
        if !node.live {
            continue;
        }
        let from = Block::from_u32(i as u32);
        if let Some(term) = &node.term {
            for dest in term.successors() {
                if let Some(list) = preds.get_mut(dest.block.index()) {
                    // One terminator's edges are recorded together, so a repeat
                    // from this block is always the last entry.
                    if list.last() != Some(&from) {
                        list.push(from);
                    }
                }
            }
        }
    }
    preds
}

/// Forwards `old` to `new` (both resolved, same type, `new` not reaching `old`).
fn forward(func: &mut Function, old: Value, new: Value) {
    if let Some(node) = func.values.get_mut(old.index()) {
        node.alias = new.as_u32();
    }
}

/// The parameter's block and index, if it is a live block's parameter.
fn param_site(func: &Function, p: Value) -> Option<(Block, usize)> {
    match func.value_def(p) {
        Some(ValueDef::Param { block, index })
            if func.is_live_block(block)
                && func.block_params(block).get(index as usize) == Some(&p) =>
        {
            Some((block, index as usize))
        }
        _ => None,
    }
}

/// What a parameter receives on every edge into its block: `Some(value)`
/// (resolved) or `None` for a non-value argument (`resN`, `exn`, `err`) or a
/// missing one. Pushes into `out`.
fn param_operands(
    func: &Function,
    preds: &[Vec<Block>],
    block: Block,
    index: usize,
    out: &mut Vec<Option<Value>>,
) {
    out.clear();
    for &p in preds.get(block.index()).map_or(&[][..], Vec::as_slice) {
        let Some(term) = func.terminator(p) else {
            continue;
        };
        for dest in term.successors().filter(|d| d.block == block) {
            out.push(match dest.args.get(index) {
                Some(BlockArg::Value(v)) => Some(func.resolve(*v)),
                _ => None,
            });
        }
    }
}

/// Removes trivial parameters among `cands` to a fixpoint (Braun et al.
/// `tryRemoveTrivialPhi`, run as a worklist): a parameter whose operands are all
/// one value `v` or itself forwards to `v`, and every candidate that received
/// it is checked again. A parameter that only receives itself (an unreachable
/// cycle) is kept. Returns the forwarded parameters; they are still listed in
/// their blocks (see [`remove_params`]).
pub(crate) fn eliminate_trivial(
    func: &mut Function,
    preds: &[Vec<Block>],
    cands: &[Value],
) -> Vec<Value> {
    let n = func.values.len();
    let mut index = alloc::vec![NONE; n];
    for (i, &c) in cands.iter().enumerate() {
        if let Some(slot) = index.get_mut(c.index()) {
            *slot = i as u32;
        }
    }
    let mut users: Vec<Vec<u32>> = alloc::vec![Vec::new(); cands.len()];
    let mut ops = Vec::new();
    for (i, &c) in cands.iter().enumerate() {
        let Some((block, at)) = param_site(func, c) else {
            continue;
        };
        param_operands(func, preds, block, at, &mut ops);
        for &op in ops.iter().flatten() {
            if let Some(&j) = index.get(op.index()) {
                if j != NONE && j as usize != i {
                    users[j as usize].push(i as u32);
                }
            }
        }
    }
    let mut removed = alloc::vec![false; cands.len()];
    let mut out = Vec::new();
    let mut work: Vec<u32> = (0..cands.len() as u32).rev().collect();
    while let Some(i) = work.pop() {
        let i = i as usize;
        if removed[i] {
            continue;
        }
        let p = cands[i];
        let Some((block, at)) = param_site(func, p) else {
            continue;
        };
        param_operands(func, preds, block, at, &mut ops);
        let mut same: Option<Value> = None;
        let mut trivial = true;
        for op in &ops {
            match *op {
                Some(v) if v == p || Some(v) == same => {}
                Some(v) if same.is_none() => same = Some(v),
                _ => {
                    trivial = false;
                    break;
                }
            }
        }
        let Some(v) = same.filter(|_| trivial) else {
            continue;
        };
        forward(func, p, v);
        removed[i] = true;
        out.push(p);
        let mut moved = core::mem::take(&mut users[i]);
        work.extend(moved.iter().copied());
        // Whoever read `p` now reads `v`: they must be checked again when `v`
        // itself turns out trivial. Small-to-large keeps the merging linear-log.
        if let Some(&j) = index.get(v.index()) {
            if j != NONE && !removed[j as usize] {
                let target = &mut users[j as usize];
                if target.len() < moved.len() {
                    core::mem::swap(target, &mut moved);
                }
                target.extend(moved);
            }
        }
    }
    out
}

/// Removes the given block parameters (live ones; others are ignored) and the
/// matching argument from every edge into their blocks, in one pass: over the
/// predecessors in `preds` when given, else over every block. Removed values
/// become detached (their forwarding, if any, is kept). Returns the blocks
/// whose parameter lists changed.
pub(crate) fn remove_params(
    func: &mut Function,
    params: &[Value],
    preds: Option<&[Vec<Block>]>,
) -> Vec<Block> {
    let mut sites: Vec<(u32, u32)> = params
        .iter()
        .filter_map(|&p| param_site(func, p).map(|(b, i)| (b.as_u32(), i as u32)))
        .collect();
    sites.sort_unstable();
    sites.dedup();
    if sites.is_empty() {
        return Vec::new();
    }
    let mut mask_of = alloc::vec![NONE; func.blocks.len()];
    let mut masks: Vec<Vec<bool>> = Vec::new();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < sites.len() {
        let b = sites[i].0;
        let old = core::mem::take(&mut func.blocks[b as usize].params);
        let mut keep = alloc::vec![true; old.len()];
        while i < sites.len() && sites[i].0 == b {
            keep[sites[i].1 as usize] = false;
            i += 1;
        }
        let mut kept = Vec::with_capacity(old.len());
        for (k, &v) in old.iter().enumerate() {
            if keep[k] {
                if let Some(node) = func.values.get_mut(v.index()) {
                    node.def = ValueDef::Param {
                        block: Block::from_u32(b),
                        index: kept.len() as u32,
                    };
                }
                kept.push(v);
            } else if let Some(node) = func.values.get_mut(v.index()) {
                node.def = ValueDef::Detached;
            }
        }
        func.blocks[b as usize].params = kept;
        mask_of[b as usize] = masks.len() as u32;
        masks.push(keep);
        blocks.push(Block::from_u32(b));
    }
    let filter = |term: &mut Terminator| {
        term.for_each_successor_mut(|dest| {
            let Some(&m) = mask_of.get(dest.block.index()) else {
                return;
            };
            if m == NONE {
                return;
            }
            let keep = &masks[m as usize];
            // An edge that does not pass one argument per old parameter is left
            // for the validator to report.
            if dest.args.len() == keep.len() {
                let mut k = 0;
                dest.args.retain(|_| {
                    let kept = keep[k];
                    k += 1;
                    kept
                });
            }
        });
    };
    match preds {
        Some(preds) => {
            for &b in &blocks {
                for &p in preds.get(b.index()).map_or(&[][..], Vec::as_slice) {
                    if let Some(term) = func.blocks.get_mut(p.index()).and_then(|n| n.term.as_mut())
                    {
                        filter(term);
                    }
                }
            }
        }
        None => {
            for node in func.blocks.iter_mut().filter(|n| n.live) {
                if let Some(term) = &mut node.term {
                    filter(term);
                }
            }
        }
    }
    blocks
}

/// Inserts `const 0` of `ty` (`false`, `+0.0`, or `null`) at the start of the
/// entry block, where it dominates every block.
fn insert_zero(func: &mut Function, ty: Type) -> Result<Value, BuildError> {
    if func.values.len() >= MAX_ENTITIES as usize {
        return Err(BuildError::CapacityExceeded);
    }
    let inst = func
        .push_inst(InstData::Const { ty, bits: 0 })
        .ok_or(BuildError::CapacityExceeded)?;
    let start = func.values.len() as u32;
    let v = func
        .push_value(ty, ValueDef::Result { inst, index: 0 })
        .ok_or(BuildError::CapacityExceeded)?;
    func.insts[inst.index()].results = ValueRange { start, len: 1 };
    let entry = func.entry();
    match func.first_inst(entry) {
        Some(first) => func.link_before(inst, first),
        None => func.link_at_end(inst, entry),
    }
    Ok(v)
}

// ------------------------------------------------------------- the state

/// The builder's SSA construction state, created by the first
/// [`Builder::declare_var`].
#[derive(Clone, Debug, Default)]
pub(crate) struct SsaState {
    var_ty: Vec<Type>,
    /// `(variable, block)` → the variable's value at the end of the block (or
    /// the parameter that stands for it).
    defs: DefMap,
    /// By block: sealed (every edge into it exists).
    sealed: Vec<bool>,
    /// By block: distinct predecessors, kept current by the builder.
    pub(crate) preds: Vec<Vec<Block>>,
    /// By value: the variable a construction parameter stands for, or `NONE`.
    param_var: Vec<u32>,
    /// By block: index of its first construction parameter, or `NONE`. They
    /// always come after the block's own parameters.
    pub(crate) first_ssa: Vec<u32>,
    /// Construction parameters not yet checked for triviality.
    pending: Vec<Value>,
    /// One zero constant per type, for reads of an undefined variable.
    zeros: Vec<(Type, Value)>,
    /// Blocks whose incoming edges may lack construction arguments.
    stack: Vec<Block>,
    chain: Vec<Block>,
    stamp: Vec<u32>,
    epoch: u32,
}

impl SsaState {
    pub(crate) fn new(func: &Function) -> SsaState {
        let n = func.blocks.len();
        let mut sealed = alloc::vec![false; n];
        if let Some(s) = sealed.get_mut(func.entry().index()) {
            // No edge may target the entry block, so its predecessors are known.
            *s = true;
        }
        SsaState {
            preds: distinct_preds(func),
            sealed,
            first_ssa: alloc::vec![NONE; n],
            stamp: alloc::vec![0; n],
            ..SsaState::default()
        }
    }

    fn grow_blocks(&mut self, n: usize) {
        if self.preds.len() < n {
            self.preds.resize(n, Vec::new());
            self.sealed.resize(n, false);
            self.first_ssa.resize(n, NONE);
            self.stamp.resize(n, 0);
        }
    }

    /// Whether construction is complete: every live block sealed and no
    /// incomplete edge pending.
    pub(crate) fn is_complete(&self, func: &Function) -> bool {
        self.stack.is_empty()
            && func
                .blocks()
                .all(|b| self.sealed.get(b.index()).copied().unwrap_or(false))
    }

    pub(crate) fn is_sealed(&self, b: Block) -> bool {
        self.sealed.get(b.index()).copied().unwrap_or(false)
    }

    fn param_var(&self, v: Value) -> u32 {
        self.param_var.get(v.index()).copied().unwrap_or(NONE)
    }

    fn first_ssa(&self, b: Block) -> u32 {
        self.first_ssa.get(b.index()).copied().unwrap_or(NONE)
    }

    /// Records the edges of a newly installed terminator of `from`.
    pub(crate) fn add_edges(&mut self, from: Block, term: &Terminator) {
        for dest in term.successors() {
            self.grow_blocks(dest.block.index() + 1);
            if let Some(list) = self.preds.get_mut(dest.block.index()) {
                if list.last() != Some(&from) {
                    list.push(from);
                }
            }
        }
    }

    /// Forgets the edges of a terminator of `from` that is being removed.
    pub(crate) fn remove_edges(&mut self, from: Block, term: &Terminator) {
        for dest in term.successors() {
            if let Some(list) = self.preds.get_mut(dest.block.index()) {
                if let Some(at) = list.iter().position(|&p| p == from) {
                    let _ = list.remove(at);
                }
            }
        }
    }

    /// Recomputes a block's first construction parameter after its parameter
    /// list changed.
    pub(crate) fn refresh_first(&mut self, func: &Function, b: Block) {
        let first = func
            .block_params(b)
            .iter()
            .position(|&p| self.param_var(p) != NONE)
            .map_or(NONE, |i| i as u32);
        if let Some(f) = self.first_ssa.get_mut(b.index()) {
            *f = first;
        }
    }

    fn zero(&mut self, func: &mut Function, ty: Type) -> Result<Value, BuildError> {
        if let Some(&(_, v)) = self.zeros.iter().find(|(t, v)| {
            *t == ty && matches!(func.value_def(*v), Some(ValueDef::Result { inst, .. }) if func.inst_block(inst).is_some())
        }) {
            return Ok(v);
        }
        let v = insert_zero(func, ty)?;
        self.zeros.retain(|(t, _)| *t != ty);
        self.zeros.push((ty, v));
        Ok(v)
    }

    /// Adds a construction parameter for `var` to `b` and defines `var` there.
    fn new_param(&mut self, func: &mut Function, b: Block, var: u32) -> Result<Value, BuildError> {
        let ty = self.var_ty[var as usize];
        let index = func.blocks[b.index()].params.len() as u32;
        let v = func
            .push_value(ty, ValueDef::Param { block: b, index })
            .ok_or(BuildError::CapacityExceeded)?;
        func.blocks[b.index()].params.push(v);
        if self.param_var.len() <= v.index() {
            self.param_var.resize(v.index() + 1, NONE);
        }
        self.param_var[v.index()] = var;
        self.grow_blocks(b.index() + 1);
        if self.first_ssa[b.index()] == NONE {
            self.first_ssa[b.index()] = index;
        }
        self.pending.push(v);
        self.defs.insert(key(var, b.as_u32()), v.as_u32());
        Ok(v)
    }

    /// The value of `var` at the end of `block` (Braun's `readVariable`),
    /// without recursion: single-predecessor chains are walked in a loop, and a
    /// new parameter's operands are left to [`drain`](SsaState::drain).
    fn read(&mut self, func: &mut Function, var: u32, block: Block) -> Result<Value, BuildError> {
        self.grow_blocks(func.blocks.len());
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.stamp.iter_mut().for_each(|s| *s = 0);
            self.epoch = 1;
        }
        self.chain.clear();
        let mut b = block;
        let value = loop {
            if let Some(v) = self.defs.get(key(var, b.as_u32())) {
                break func.resolve(Value::from_u32(v));
            }
            if !self.is_sealed(b) {
                // An incomplete parameter: its operands come when `b` is sealed.
                break self.new_param(func, b, var)?;
            }
            let preds = self.preds.get(b.index()).map_or(0, Vec::len);
            if preds == 0 {
                // The entry block (or an unreachable one): the variable was never
                // assigned on the way here. Reads of it give zero.
                let ty = self.var_ty[var as usize];
                break self.zero(func, ty)?;
            }
            if preds == 1 && self.stamp[b.index()] != self.epoch {
                self.stamp[b.index()] = self.epoch;
                self.chain.push(b);
                b = self.preds[b.index()][0];
                continue;
            }
            // Several predecessors (or a single-predecessor cycle, which is
            // unreachable): a parameter, completed from the work stack.
            let p = self.new_param(func, b, var)?;
            self.stack.push(b);
            break p;
        };
        self.defs.insert(key(var, b.as_u32()), value.as_u32());
        for i in 0..self.chain.len() {
            let c = self.chain[i];
            self.defs.insert(key(var, c.as_u32()), value.as_u32());
        }
        Ok(value)
    }

    /// Fills the construction arguments missing on edges into `b` (always a
    /// suffix of each edge's arguments).
    fn complete(&mut self, func: &mut Function, b: Block) -> Result<(), BuildError> {
        let first = self.first_ssa(b);
        if first == NONE {
            return Ok(());
        }
        let mut i = 0;
        while let Some(&p) = self.preds.get(b.index()).and_then(|l| l.get(i)) {
            i += 1;
            loop {
                let nparams = func.blocks[b.index()].params.len();
                let Some(term) = func.terminator(p) else {
                    break;
                };
                let min = term
                    .successors()
                    .filter(|d| d.block == b)
                    .map(|d| d.args.len())
                    .min();
                let Some(min) = min.filter(|&m| m < nparams && m >= first as usize) else {
                    break;
                };
                let q = func.blocks[b.index()].params[min];
                let var = self.param_var(q);
                if var == NONE {
                    break;
                }
                let v = self.read(func, var, p)?;
                if let Some(term) = func.blocks[p.index()].term.as_mut() {
                    term.for_each_successor_mut(|d| {
                        if d.block == b && d.args.len() == min {
                            d.args.push(BlockArg::Value(v));
                        }
                    });
                }
            }
        }
        Ok(())
    }

    fn drain(&mut self, func: &mut Function) -> Result<(), BuildError> {
        while let Some(b) = self.stack.pop() {
            self.complete(func, b)?;
        }
        Ok(())
    }

    /// Braun's on-the-fly `tryRemoveTrivialPhi`, without the propagation to
    /// users (the final pass in `seal_all_blocks` does that): forwards each
    /// construction parameter in `pending[start..]` whose block is sealed and
    /// whose arguments are all one value (or itself). Linear in the edges into
    /// those blocks.
    fn forward_trivial(&mut self, func: &mut Function, start: usize) {
        let mut ops = Vec::new();
        for i in start..self.pending.len() {
            let p = self.pending[i];
            if func.resolve(p) != p {
                continue;
            }
            let Some((block, at)) = param_site(func, p) else {
                continue;
            };
            if !self.is_sealed(block) {
                continue;
            }
            param_operands(func, &self.preds, block, at, &mut ops);
            let mut same: Option<Value> = None;
            let mut trivial = true;
            for op in &ops {
                match *op {
                    Some(v) if v == p || Some(v) == same => {}
                    Some(v) if same.is_none() => same = Some(v),
                    _ => {
                        trivial = false;
                        break;
                    }
                }
            }
            if let Some(v) = same.filter(|_| trivial) {
                forward(func, p, v);
            }
        }
    }

    /// Fills the construction arguments of a terminator of `from` that is not
    /// installed yet: the caller passes arguments for the target's own
    /// parameters, and the builder adds the variables' values.
    pub(crate) fn complete_new(
        &mut self,
        func: &mut Function,
        from: Block,
        term: &mut Terminator,
    ) -> Result<(), BuildError> {
        let mut wanted: Vec<(usize, Value)> = Vec::new();
        loop {
            wanted.clear();
            for (k, dest) in term.successors().enumerate() {
                let first = self.first_ssa(dest.block);
                let params = func.block_params(dest.block);
                let have = dest.args.len();
                if first != NONE && have >= first as usize && have < params.len() {
                    wanted.push((k, params[have]));
                }
            }
            if wanted.is_empty() {
                return Ok(());
            }
            let mut values = Vec::with_capacity(wanted.len());
            for &(_, q) in &wanted {
                let var = self.param_var(q);
                values.push(if var == NONE {
                    None
                } else {
                    Some(self.read(func, var, from)?)
                });
            }
            if values.iter().all(Option::is_none) {
                return Ok(());
            }
            let mut k = 0;
            let mut w = 0;
            term.for_each_successor_mut(|dest| {
                if wanted.get(w).is_some_and(|&(at, _)| at == k) {
                    if let Some(Some(v)) = values.get(w) {
                        dest.args.push(BlockArg::Value(*v));
                    }
                    w += 1;
                }
                k += 1;
            });
            self.drain(func)?;
        }
    }
}

// ----------------------------------------------------------- builder API

impl Builder<'_> {
    fn ssa_state(&mut self) -> &mut SsaState {
        let func = &*self.func;
        self.ssa
            .get_or_insert_with(|| Box::new(SsaState::new(func)))
    }

    fn var_index(&self, var: Variable) -> Result<u32, BuildError> {
        match &self.ssa {
            Some(st) if var.index() < st.var_ty.len() => Ok(var.as_u32()),
            _ => Err(BuildError::UnknownVariable { var }),
        }
    }

    /// Declares a mutable variable of type `ty` for SSA construction.
    ///
    /// Lower a mutable local with [`def_var`](Builder::def_var) at each
    /// assignment and [`use_var`](Builder::use_var) at each read, in the order
    /// the code is lowered, and [`seal_block`](Builder::seal_block) each block
    /// once every edge into it exists; the builder places the block parameters
    /// (phis). Finish with [`seal_all_blocks`](Builder::seal_all_blocks), which
    /// also removes parameters that turned out trivial.
    ///
    /// A variable read on a path where it was never assigned reads zero of its
    /// type (`0`, `false`, `+0.0`, or `null`), defined at the start of the entry
    /// block.
    ///
    /// # Errors
    ///
    /// [`BuildError::CapacityExceeded`] after `u32::MAX - 1` variables.
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
    /// let (s, i) = (b.declare_var(Type::I64)?, b.declare_var(Type::I64)?);
    /// let n = b.param(0)?;
    /// let zero = b.iconst(Type::I64, 0)?;
    /// b.def_var(s, zero)?;
    /// b.def_var(i, zero)?;
    /// let (head, body, exit) = (b.create_block(&[])?, b.create_block(&[])?, b.create_block(&[])?);
    /// b.jump(head, &[])?;
    ///
    /// b.switch_to(head)?; // not sealed yet: the back edge is still to come
    /// let iv = b.use_var(i)?;
    /// let more = b.compare(CmpOp::Lt, iv, n)?;
    /// b.branch(more, body, &[], exit, &[])?;
    ///
    /// b.switch_to(body)?;
    /// b.seal_block(body)?;
    /// let (sv, iv) = (b.use_var(s)?, b.use_var(i)?);
    /// let s2 = b.add(sv, iv, Overflow::Wrap)?;
    /// b.def_var(s, s2)?;
    /// let one = b.iconst(Type::I64, 1)?;
    /// let i2 = b.add(iv, one, Overflow::Wrap)?;
    /// b.def_var(i, i2)?;
    /// b.jump(head, &[])?;
    /// b.seal_block(head)?; // both edges into `head` exist now
    ///
    /// b.switch_to(exit)?;
    /// b.seal_block(exit)?;
    /// let result = b.use_var(s)?;
    /// b.ret(&[result])?;
    /// b.seal_all_blocks()?;
    ///
    /// assert_eq!(b.func().block_params(head).len(), 2); // s and i
    /// m.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn declare_var(&mut self, ty: Type) -> Result<Variable, BuildError> {
        let st = self.ssa_state();
        let index = u32::try_from(st.var_ty.len())
            .ok()
            .filter(|&i| i < MAX_ENTITIES)
            .ok_or(BuildError::CapacityExceeded)?;
        st.var_ty.push(ty);
        Ok(Variable::from_u32(index))
    }

    /// Assigns `value` to `var` in the current block: later reads in this block,
    /// and reads that reach the end of this block, see `value`.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownVariable`]; [`BuildError::UnknownValue`];
    /// [`BuildError::Invalid`] with `TypeMismatch` if `value`'s type is not the
    /// variable's; [`BuildError::AlreadyTerminated`] if the current block already
    /// has its terminator (its successors may have read the old value);
    /// a stale position.
    ///
    /// See [`declare_var`](Builder::declare_var) for an example.
    pub fn def_var(&mut self, var: Variable, value: Value) -> Result<(), BuildError> {
        let v = self.var_index(var)?;
        let found = self
            .func
            .value_type(self.func.resolve(value))
            .ok_or(BuildError::UnknownValue { value })?;
        let block = self.current_checked()?;
        if self.func.terminator(block).is_some() {
            return Err(BuildError::AlreadyTerminated { block });
        }
        let Some(st) = self.ssa.as_deref_mut() else {
            return Err(BuildError::UnknownVariable { var });
        };
        let expected = st.var_ty[v as usize];
        if found != expected {
            return Err(BuildError::Invalid(K::TypeMismatch {
                value: Some(value),
                expected,
                found,
            }));
        }
        st.grow_blocks(self.func.blocks.len());
        st.defs.insert(key(v, block.as_u32()), value.as_u32());
        Ok(())
    }

    /// The current value of `var` in the current block, adding block parameters
    /// where control-flow paths with different values meet.
    ///
    /// A parameter added in an unsealed block is completed when the block is
    /// sealed and may then turn out trivial; it is forwarded to the value it
    /// stands for (operands that read it are rewritten by
    /// [`seal_all_blocks`](Builder::seal_all_blocks)). A front end that keeps
    /// returned values in its own tables should look them up through
    /// [`Function::resolve`](crate::Function::resolve) after sealing.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownVariable`]; a stale position;
    /// [`BuildError::CapacityExceeded`].
    ///
    /// See [`declare_var`](Builder::declare_var) for an example.
    pub fn use_var(&mut self, var: Variable) -> Result<Value, BuildError> {
        let v = self.var_index(var)?;
        let block = self.current_checked()?;
        let Some(st) = self.ssa.as_deref_mut() else {
            return Err(BuildError::UnknownVariable { var });
        };
        let start = st.pending.len();
        let value = st.read(self.func, v, block)?;
        st.drain(self.func)?;
        st.forward_trivial(self.func, start);
        Ok(self.func.resolve(value))
    }

    /// Declares that every edge into `block` now exists: its incomplete
    /// parameters get their arguments, and an edge into it later is refused
    /// ([`BuildError::SealedBlock`]). Sealing a sealed block does nothing. The
    /// entry block is always sealed.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownBlock`]; [`BuildError::CapacityExceeded`].
    ///
    /// See [`declare_var`](Builder::declare_var) for an example.
    pub fn seal_block(&mut self, block: Block) -> Result<(), BuildError> {
        if !self.func.is_live_block(block) {
            return Err(BuildError::UnknownBlock { block });
        }
        let func = &mut *self.func;
        let st = self
            .ssa
            .get_or_insert_with(|| Box::new(SsaState::new(func)));
        st.grow_blocks(func.blocks.len());
        if st.sealed[block.index()] {
            return Ok(());
        }
        st.sealed[block.index()] = true;
        let start = st.pending.len();
        st.stack.push(block);
        st.drain(func)?;
        // The block's incomplete parameters are complete now: forward the
        // trivial ones (and any made while completing them).
        let mut check: Vec<Value> = func
            .block_params(block)
            .iter()
            .copied()
            .filter(|&p| st.param_var(p) != NONE)
            .collect();
        check.extend_from_slice(&st.pending[start..]);
        let first = st.pending.len();
        st.pending.extend_from_slice(&check);
        st.forward_trivial(func, first);
        st.pending.truncate(first);
        Ok(())
    }

    /// Whether `block` is sealed (see [`seal_block`](Builder::seal_block)).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let next = b.create_block(&[])?;
    /// assert!(b.is_sealed(b.func().entry()));
    /// assert!(!b.is_sealed(next));
    /// b.seal_block(next)?;
    /// assert!(b.is_sealed(next));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn is_sealed(&self, block: Block) -> bool {
        match &self.ssa {
            Some(st) => st.is_sealed(block),
            None => block == self.func.entry(),
        }
    }

    /// Seals every block, then removes the block parameters the construction
    /// added that turned out trivial (all their arguments are one value), to a
    /// fixpoint, rewriting operands to the surviving values. Call it once
    /// lowering is complete (it can be called again after more variable reads).
    ///
    /// # Errors
    ///
    /// [`BuildError::CapacityExceeded`].
    ///
    /// See [`declare_var`](Builder::declare_var) for an example.
    pub fn seal_all_blocks(&mut self) -> Result<(), BuildError> {
        let func = &mut *self.func;
        let st = self
            .ssa
            .get_or_insert_with(|| Box::new(SsaState::new(func)));
        st.grow_blocks(func.blocks.len());
        for (i, node) in func.blocks.iter().enumerate() {
            if node.live && !st.sealed[i] {
                st.sealed[i] = true;
                st.stack.push(Block::from_u32(i as u32));
            }
        }
        st.drain(func)?;
        let pending = core::mem::take(&mut st.pending);
        if pending.is_empty() {
            return Ok(());
        }
        let removed = eliminate_trivial(func, &st.preds, &pending);
        if removed.is_empty() {
            return Ok(());
        }
        for b in remove_params(func, &removed, Some(&st.preds)) {
            st.refresh_first(func, b);
        }
        self.resolve_aliases();
        Ok(())
    }

    fn current_checked(&self) -> Result<Block, BuildError> {
        self.current_block().ok_or(match self.position() {
            crate::builder::Position::End(block) => BuildError::UnknownBlock { block },
            crate::builder::Position::Before(inst) | crate::builder::Position::After(inst) => {
                BuildError::UnknownInst { inst }
            }
        })
    }

    /// Removes the given block parameters (from any blocks) and the matching
    /// argument from every edge into their blocks, all in one pass over the
    /// function: linear in its size however many parameters go. The removed
    /// values become detached; replace their uses first.
    ///
    /// # Errors
    ///
    /// [`BuildError::NotBlockParam`] if a value is not a parameter of a live
    /// block (nothing is changed).
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
    /// let next = b.create_block(&[Type::I32, Type::I32, Type::I32])?;
    /// b.jump(next, &[x, x, x])?;
    /// b.switch_to(next)?;
    /// b.ret(&[])?;
    /// let params = b.func().block_params(next).to_vec();
    /// b.remove_block_params(&[params[0], params[2]])?;
    /// assert_eq!(b.func().block_params(next), &[params[1]]);
    /// b.validate()?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn remove_block_params(&mut self, params: &[Value]) -> Result<(), BuildError> {
        for &p in params {
            if param_site(self.func, p).is_none() {
                return Err(BuildError::NotBlockParam { value: p });
            }
        }
        let preds = self.ssa.as_deref().map(|st| st.preds.as_slice());
        let changed = remove_params(self.func, params, preds);
        if let Some(st) = self.ssa.as_deref_mut() {
            for b in changed {
                st.refresh_first(self.func, b);
            }
        }
        Ok(())
    }

    /// Moves an instruction to `pos`, in O(1). Its handle and results are kept;
    /// dominance is not checked here (the validator does). A position naming the
    /// moved instruction moves with it; moving an instruction to before or after
    /// itself does nothing.
    ///
    /// # Errors
    ///
    /// [`BuildError::UnknownInst`] if `inst` or the instruction `pos` names is
    /// unknown or removed; [`BuildError::UnknownBlock`] for a removed block.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Position, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let x = b.iconst(Type::I32, 1)?;
    /// let y = b.iconst(Type::I32, 2)?;
    /// let (xi, yi) = (b.func().defining_inst(x).unwrap(), b.func().defining_inst(y).unwrap());
    /// b.move_inst(yi, Position::Before(xi))?;
    /// let entry = b.func().entry();
    /// assert_eq!(b.func().insts(entry).collect::<Vec<_>>(), [yi, xi]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn move_inst(
        &mut self,
        inst: Inst,
        pos: crate::builder::Position,
    ) -> Result<(), BuildError> {
        use crate::builder::Position;
        if self.func.inst_block(inst).is_none() {
            return Err(BuildError::UnknownInst { inst });
        }
        match pos {
            Position::End(b) => {
                if !self.func.is_live_block(b) {
                    return Err(BuildError::UnknownBlock { block: b });
                }
            }
            Position::Before(at) | Position::After(at) => {
                if self.func.inst_block(at).is_none() {
                    return Err(BuildError::UnknownInst { inst: at });
                }
                if at == inst {
                    return Ok(());
                }
            }
        }
        self.func.unlink(inst);
        match pos {
            Position::End(b) => self.func.link_at_end(inst, b),
            Position::Before(at) => self.func.link_before(inst, at),
            Position::After(at) => self.func.link_after(inst, at),
        }
        Ok(())
    }
}
