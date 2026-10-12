//! Statepoint insertion: fills the GC lists of every safepoint from liveness and
//! repairs SSA so every use after a safepoint reads the relocated reference.
//!
//! A front end can lower GC references as if nothing moved: keep a `ref` in an
//! SSA value (or a [`Variable`](crate::Variable)) and use it after calls. Then
//! [`Builder::insert_statepoints`] makes the function satisfy IR.md §9 rule 1:
//!
//! 1. **Liveness.** For every `ref` value, the safepoints it is live across,
//!    computed exactly as the validator defines it (a backward walk from each
//!    use to the definition; an `invoke`'s edge arguments are uses after its
//!    call).
//! 2. **Lists.** Each safepoint's GC list gains the references live across it
//!    that it does not list yet (entries already there are kept and reused). A
//!    `call` or `safepoint` instruction whose list grows is replaced by one with
//!    the longer list (its old results forward to the new ones); an `invoke`'s
//!    list grows in place, its relocated references passed as `resN` edge
//!    arguments.
//! 3. **SSA repair.** Each reference is then a variable with several
//!    definitions (its own and one per relocation), and every use is rewritten
//!    to the reaching definition, adding block parameters where definitions
//!    meet (Braun et al., over the finished control-flow graph, iteratively).
//!    Parameters that turn out trivial are removed.
//!
//! The validator is the oracle: the result is validated, and on any failure the
//! function is restored exactly. A pointer *derived* from a reference that is
//! live across a safepoint cannot be relocated (it must be recomputed from the
//! relocated reference), so such input is refused with the validator's
//! `LiveAcrossSafepoint`.

use alloc::vec::Vec;

use crate::builder::{Builder, Position};
use crate::entity::{Block, Inst, NONE, Value};
use crate::error::BuildError;
use crate::function::{Function, ValueDef};
use crate::inst::{BlockArg, InstData, Terminator};
use crate::ssa::{DefMap, distinct_preds, eliminate_trivial, key, remove_params};
use crate::types::Type;
use crate::typing;
use crate::validate::{self, dom, gc};

/// What [`Builder::insert_statepoints`] changed.
///
/// # Examples
///
/// ```
/// use ir_lang::{Call, Linkage, Module, Signature, Type};
///
/// let mut m = Module::new("m");
/// let poll = m.declare_function("poll", &Signature::new(&[], &[]), Linkage::Import)?;
/// let f = m.declare_function("f", &Signature::new(&[Type::Ref], &[Type::Ref]), Linkage::Export)?;
/// let mut b = m.build(f)?;
/// let r = b.param(0)?;
/// b.call(Call::direct(poll, &[]))?; // `r` is live across this safepoint
/// b.ret(&[r])?;
/// let stats = b.insert_statepoints()?;
/// assert_eq!((stats.safepoints, stats.relocations, stats.params), (1, 1, 0));
/// m.validate()?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
#[non_exhaustive]
pub struct StatepointStats {
    /// Safepoints whose GC lists grew.
    pub safepoints: usize,
    /// GC-list entries added, over all safepoints.
    pub relocations: usize,
    /// Block parameters added to carry relocated references where control flow
    /// joins (trivial ones already removed).
    pub params: usize,
}

/// The work budget [`Builder::insert_statepoints`] uses: 2^26 (about 67 million)
/// relocations plus block parameters, far beyond real functions, so a hostile
/// input cannot exhaust memory through the quadratic worst case (every
/// reference live across every safepoint).
pub const DEFAULT_STATEPOINT_BUDGET: usize = 1 << 26;

/// A safepoint: an instruction, or the `invoke` ending its block.
#[derive(Clone, Copy, Debug)]
struct Sp {
    block: u32,
    pos: u32,
    inst: Option<Inst>,
}

impl Builder<'_> {
    /// Inserts and updates the GC lists of every safepoint so no `ref` is live
    /// across one, and rewrites every later use to the relocated reference
    /// (adding block parameters where needed): the "rewrite statepoints" step a
    /// front end runs after lowering references as ordinary values. See the
    /// module documentation in `specs/IR.md` §9 and [`StatepointStats`].
    ///
    /// Variable construction in progress is finished first (as by
    /// [`seal_all_blocks`](Builder::seal_all_blocks)) and its state dropped;
    /// that step is kept whatever happens next. The result is validated; on any
    /// error the function is left exactly as it was before insertion began.
    /// Cost: linear in the function plus the live ranges of its references; the
    /// work is capped by [`DEFAULT_STATEPOINT_BUDGET`] (see
    /// [`insert_statepoints_within`](Builder::insert_statepoints_within)).
    ///
    /// # Errors
    ///
    /// [`BuildError::Invalid`] with the first validation error if the function
    /// is not valid apart from references live across safepoints, or if the
    /// result still is not (a derived pointer live across a safepoint, which
    /// cannot be relocated, reports `LiveAcrossSafepoint`);
    /// [`BuildError::BudgetExceeded`]; [`BuildError::CapacityExceeded`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Call, InstData, Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let poll = m.declare_function("poll", &Signature::new(&[], &[]), Linkage::Import)?;
    /// let f = m.declare_function("f", &Signature::new(&[Type::Ref], &[Type::Ref]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let r = b.param(0)?;
    /// b.call(Call::direct(poll, &[]))?;
    /// b.ret(&[r])?;
    /// assert!(b.validate().is_err()); // `r` is used after the safepoint
    /// b.insert_statepoints()?;
    /// b.validate()?;
    /// let entry = b.func().entry();
    /// let call = b.func().insts(entry).next().unwrap();
    /// assert!(matches!(b.func().inst(call), Some(InstData::Call(c)) if c.gc == [r]));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn insert_statepoints(&mut self) -> Result<StatepointStats, BuildError> {
        self.insert_statepoints_within(DEFAULT_STATEPOINT_BUDGET)
    }

    /// [`insert_statepoints`](Builder::insert_statepoints) with an explicit
    /// budget: the number of (reference, safepoint) relocations plus block
    /// parameters it may create. Over budget, it stops with
    /// [`BuildError::BudgetExceeded`] and the function is unchanged.
    ///
    /// # Errors
    ///
    /// As [`insert_statepoints`](Builder::insert_statepoints).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{BuildError, Call, Linkage, Module, Signature, Type};
    ///
    /// let mut m = Module::new("m");
    /// let poll = m.declare_function("poll", &Signature::new(&[], &[]), Linkage::Import)?;
    /// let f = m.declare_function("f", &Signature::new(&[Type::Ref], &[Type::Ref]), Linkage::Export)?;
    /// let mut b = m.build(f)?;
    /// let r = b.param(0)?;
    /// b.call(Call::direct(poll, &[]))?;
    /// b.call(Call::direct(poll, &[]))?;
    /// b.ret(&[r])?;
    /// // Two relocations are needed; a budget of one refuses, changing nothing.
    /// assert_eq!(b.insert_statepoints_within(1), Err(BuildError::BudgetExceeded));
    /// assert_eq!(b.insert_statepoints_within(2)?.relocations, 2);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn insert_statepoints_within(
        &mut self,
        budget: usize,
    ) -> Result<StatepointStats, BuildError> {
        if self.ssa.is_some() {
            self.seal_all_blocks()?;
            self.ssa = None;
        }
        let backup = self.func.clone();
        let pos = self.pos;
        let result = run(self, budget);
        if result.is_err() {
            *self.func = backup;
            self.pos = pos;
        }
        result
    }

    /// Inserts `data` immediately before `at`, keeping the insertion position.
    fn insert_before(&mut self, at: Inst, data: InstData) -> Result<Inst, BuildError> {
        let saved = self.pos;
        self.pos = Position::Before(at);
        let r = self.insert(data);
        self.pos = saved;
        r
    }
}

fn invalid(e: crate::error::ValidationError) -> BuildError {
    BuildError::Invalid(e.kind().clone())
}

fn gc_list(data: &InstData) -> &[Value] {
    match data {
        InstData::Call(call) => &call.gc,
        InstData::Safepoint { gc } => gc,
        _ => &[],
    }
}

fn run(b: &mut Builder<'_>, budget: usize) -> Result<StatepointStats, BuildError> {
    let (decls, id) = (b.decls, b.id);
    let _ = validate::validate_structure(decls, id, b.func).map_err(invalid)?;
    b.resolve_aliases();
    let mut stats = StatepointStats::default();

    // ---------------------------------------------------- 1. liveness
    let (sps, pairs, idom_rpo) = {
        let func: &Function = b.func;
        let cfg = validate::Cfg::of(func);
        let idom = dom::compute_idoms(func.entry().index(), &cfg);
        let preds = cfg.reversed();
        let nb = func.blocks.len();
        let mut inst_pos = alloc::vec![0u32; func.insts.len()];
        let mut len = alloc::vec![0u32; nb];
        let mut sps: Vec<Sp> = Vec::new();
        let mut sp_range = alloc::vec![(0u32, 0u32); nb];
        for &bb in &idom.rpo {
            let block = Block::from_u32(bb);
            let start = sps.len() as u32;
            let mut k = 0u32;
            for inst in func.insts(block) {
                k += 1;
                inst_pos[inst.index()] = k;
                if gc::is_safepoint_inst(decls, &func.insts[inst.index()].data) {
                    sps.push(Sp {
                        block: bb,
                        pos: k,
                        inst: Some(inst),
                    });
                }
            }
            len[bb as usize] = k;
            if func
                .terminator(block)
                .is_some_and(|t| gc::is_safepoint_term(decls, t))
            {
                sps.push(Sp {
                    block: bb,
                    pos: k + 1,
                    inst: None,
                });
            }
            sp_range[bb as usize] = (start, sps.len() as u32);
        }
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        if !sps.is_empty() {
            let is_ref = |v: Value| func.value_type(v) == Some(Type::Ref);
            let mut uses: Vec<(u32, u32, u32)> = Vec::new();
            for &bb in &idom.rpo {
                let block = Block::from_u32(bb);
                for inst in func.insts(block) {
                    let p = inst_pos[inst.index()];
                    func.insts[inst.index()].data.for_each_operand(|v| {
                        let r = func.resolve(v);
                        if is_ref(r) {
                            uses.push((r.as_u32(), bb, p));
                        }
                    });
                }
                if let Some(term) = func.terminator(block) {
                    let l = len[bb as usize];
                    term.for_each_own_operand(|v| {
                        let r = func.resolve(v);
                        if is_ref(r) {
                            uses.push((r.as_u32(), bb, l + 1));
                        }
                    });
                    for dest in term.successors() {
                        for arg in &dest.args {
                            if let BlockArg::Value(v) = arg {
                                let r = func.resolve(*v);
                                if is_ref(r) {
                                    uses.push((r.as_u32(), bb, l + 2));
                                }
                            }
                        }
                    }
                }
            }
            uses.sort_unstable();
            let mut stamp_in = alloc::vec![NONE; nb];
            let mut stamp_out = alloc::vec![NONE; nb];
            let mut work: Vec<u32> = Vec::new();
            let mut outs: Vec<u32> = Vec::new();
            let mut use_blocks: Vec<(u32, u32)> = Vec::new();
            let cross = |pairs: &mut Vec<(u32, u32)>, bb: u32, lo: u32, hi: u32, v: u32| {
                let (s, e) = sp_range[bb as usize];
                for i in s..e {
                    let p = sps[i as usize].pos;
                    if p > lo && p < hi {
                        pairs.push((i, v));
                    }
                }
            };
            let mut i = 0;
            while i < uses.len() {
                let v = uses[i].0;
                let value = Value::from_u32(v);
                let (def_block, def_pos) = match func.value_def(value) {
                    Some(ValueDef::Param { block, .. }) => (block.as_u32(), 0),
                    Some(ValueDef::Result { inst, .. }) => (
                        func.inst_block(inst).map_or(NONE, Block::as_u32),
                        inst_pos.get(inst.index()).copied().unwrap_or(0),
                    ),
                    _ => (NONE, 0),
                };
                use_blocks.clear();
                outs.clear();
                work.clear();
                while i < uses.len() && uses[i].0 == v {
                    let (_, bb, p) = uses[i];
                    i += 1;
                    match use_blocks.last_mut() {
                        Some(last) if last.0 == bb => last.1 = last.1.max(p),
                        _ => use_blocks.push((bb, p)),
                    }
                    if bb != def_block && stamp_in[bb as usize] != v {
                        stamp_in[bb as usize] = v;
                        work.push(bb);
                    }
                }
                while let Some(bb) = work.pop() {
                    for &p in preds.succs(bb as usize) {
                        if !idom.reachable(p as usize) {
                            continue;
                        }
                        if stamp_out[p as usize] != v {
                            stamp_out[p as usize] = v;
                            outs.push(p);
                        }
                        if p != def_block && stamp_in[p as usize] != v {
                            stamp_in[p as usize] = v;
                            work.push(p);
                        }
                    }
                }
                for &p in &outs {
                    let lo = if p == def_block { def_pos } else { 0 };
                    cross(&mut pairs, p, lo, len[p as usize] + 2, v);
                }
                for &(bb, hi) in &use_blocks {
                    if stamp_out[bb as usize] == v {
                        continue; // covered by the live-out range
                    }
                    let lo = if bb == def_block { def_pos } else { 0 };
                    cross(&mut pairs, bb, lo, hi, v);
                }
                // Each value's pairs are distinct, so this bounds the work.
                if pairs.len() > budget {
                    return Err(BuildError::BudgetExceeded);
                }
            }
            pairs.sort_unstable();
            pairs.dedup();
        }
        (sps, pairs, idom.rpo)
    };
    if pairs.is_empty() {
        validate::validate_body(decls, id, b.func).map_err(invalid)?;
        return Ok(stats);
    }

    // ---------------------------------------------- 2. the GC lists
    let nvalues = b.func.values.len();
    let mut var_of = alloc::vec![NONE; nvalues];
    let mut vars: Vec<Value> = Vec::new();
    {
        let mut seen: Vec<u32> = pairs.iter().map(|&(_, v)| v).collect();
        seen.sort_unstable();
        seen.dedup();
        for v in seen {
            var_of[v as usize] = vars.len() as u32;
            vars.push(Value::from_u32(v));
        }
    }
    // (safepoint instruction, var) -> relocated value; (var, block) -> `resN` of
    // the invoke ending the block.
    let mut reloc_inst = DefMap::default();
    let mut inv_reloc = DefMap::default();
    let mut i = 0;
    while i < pairs.len() {
        let s = pairs[i].0;
        let sp = sps[s as usize];
        let mut live: Vec<Value> = Vec::new();
        while i < pairs.len() && pairs[i].0 == s {
            live.push(Value::from_u32(pairs[i].1));
            i += 1;
        }
        let block = Block::from_u32(sp.block);
        match sp.inst {
            Some(inst) => {
                let mut data = b.func.insts[inst.index()].data.clone();
                let (returns, mut gc): (usize, Vec<Value>) = match &data {
                    InstData::Call(call) => (
                        typing::call_sig(decls, b.func, call).map_or(0, |s| s.returns.len()),
                        call.gc.clone(),
                    ),
                    _ => (0, gc_list(&data).to_vec()),
                };
                let mut at: Vec<(u32, usize)> = Vec::with_capacity(live.len());
                let old_len = gc.len();
                for &r in &live {
                    let rr = b.func.resolve(r);
                    let found = gc.iter().position(|&e| b.func.resolve(e) == rr);
                    let idx = found.unwrap_or_else(|| {
                        gc.push(r);
                        gc.len() - 1
                    });
                    at.push((var_of[r.index()], idx));
                }
                let target = if gc.len() > old_len {
                    stats.safepoints += 1;
                    stats.relocations += gc.len() - old_len;
                    match &mut data {
                        InstData::Call(call) => call.gc = gc,
                        _ => data = InstData::Safepoint { gc: gc.into() },
                    }
                    let new = b.insert_before(inst, data)?;
                    let (old_res, new_res) = (b.func.results(inst), b.func.results(new));
                    for k in 0..old_res.len() {
                        if let (Some(o), Some(n)) = (old_res.get(k), new_res.get(k)) {
                            b.replace_all_uses(o, n)?;
                            // A reference this call defines (an allocation's
                            // result) is now defined by the replacement.
                            if let Some(&var) = var_of.get(o.index()).filter(|&&x| x != NONE) {
                                if var_of.len() <= n.index() {
                                    var_of.resize(n.index() + 1, NONE);
                                }
                                var_of[n.index()] = var;
                                vars[var as usize] = n;
                            }
                        }
                    }
                    b.remove_inst(inst)?;
                    b.pos = match b.pos {
                        Position::Before(x) if x == inst => Position::Before(new),
                        Position::After(x) if x == inst => Position::After(new),
                        other => other,
                    };
                    new
                } else {
                    inst
                };
                let res = b.func.results(target);
                for (var, idx) in at {
                    if let Some(v) = res.get(returns + idx) {
                        reloc_inst.insert(key(target.as_u32(), var), v.as_u32());
                    }
                }
            }
            None => {
                let returns = match b.func.terminator(block) {
                    Some(Terminator::Invoke { call, .. }) => {
                        typing::call_sig(decls, b.func, call).map_or(0, |s| s.returns.len())
                    }
                    _ => 0,
                };
                let func: &mut Function = b.func;
                let live_resolved: Vec<Value> = live.iter().map(|&r| func.resolve(r)).collect();
                let resolved: Vec<Value> = match func.terminator(block) {
                    Some(Terminator::Invoke { call, .. }) => {
                        call.gc.iter().map(|&e| func.resolve(e)).collect()
                    }
                    _ => Vec::new(),
                };
                if let Some(Terminator::Invoke { call, .. }) =
                    func.blocks[block.index()].term.as_mut()
                {
                    let old_len = call.gc.len();
                    for (&r, &rr) in live.iter().zip(&live_resolved) {
                        let idx = match resolved.iter().position(|&e| e == rr) {
                            Some(idx) => idx,
                            None => {
                                call.gc.push(r);
                                call.gc.len() - 1
                            }
                        };
                        inv_reloc.insert(
                            key(var_of[r.index()], block.as_u32()),
                            (returns + idx) as u32,
                        );
                    }
                    if call.gc.len() > old_len {
                        stats.safepoints += 1;
                        stats.relocations += call.gc.len() - old_len;
                    }
                }
            }
        }
    }

    // ---------------------------------------------------- 3. SSA repair
    // Operands read the replaced safepoints' results through forwarding; make
    // them direct so the repair compares handles.
    b.resolve_aliases();
    let mut rc = Repair::new(b.func, vars, var_of, reloc_inst, inv_reloc);
    rc.budget = budget.saturating_sub(pairs.len());
    rc.prescan(b.func, &idom_rpo);
    rc.rewrite(b.func, &idom_rpo)?;
    let created = core::mem::take(&mut rc.created);
    let removed = eliminate_trivial(b.func, &rc.preds, &created);
    let _ = remove_params(b.func, &removed, Some(&rc.preds));
    stats.params = created.len() - removed.len();
    b.resolve_aliases();
    validate::validate_body(decls, id, b.func).map_err(invalid)?;
    Ok(stats)
}

/// The SSA repair: every reference crossing a safepoint is a variable whose
/// definitions are its own and its relocations.
struct Repair {
    vars: Vec<Value>,
    /// By value: its variable, or `NONE`.
    var_of: Vec<u32>,
    reloc_inst: DefMap,
    inv_reloc: DefMap,
    /// `(var, block)` → the variable's value at the end of a block that defines
    /// or relocates it.
    end_def: DefMap,
    /// `(var, block)` → the variable's value on entry to a block.
    entry_def: DefMap,
    /// Distinct predecessors of every block (reachable or not).
    preds: Vec<Vec<Block>>,
    /// By block: the parameter count before repair.
    orig_params: Vec<u32>,
    /// By value: the variable a new parameter carries, or `NONE`.
    param_var: Vec<u32>,
    created: Vec<Value>,
    stack: Vec<Block>,
    chain: Vec<Block>,
    stamp: Vec<u32>,
    epoch: u32,
    /// By variable: the block (+1) whose scan last defined it, and the value.
    cur_block: Vec<u32>,
    cur_val: Vec<Value>,
    /// Block parameters this repair may still create.
    budget: usize,
}

impl Repair {
    fn new(
        func: &Function,
        vars: Vec<Value>,
        var_of: Vec<u32>,
        reloc_inst: DefMap,
        inv_reloc: DefMap,
    ) -> Repair {
        let nb = func.blocks.len();
        let nv = vars.len();
        Repair {
            vars,
            var_of,
            reloc_inst,
            inv_reloc,
            end_def: DefMap::default(),
            entry_def: DefMap::default(),
            preds: distinct_preds(func),
            orig_params: func.blocks.iter().map(|n| n.params.len() as u32).collect(),
            param_var: Vec::new(),
            created: Vec::new(),
            stack: Vec::new(),
            chain: Vec::new(),
            stamp: alloc::vec![0; nb],
            epoch: 0,
            cur_block: alloc::vec![0; nv],
            cur_val: alloc::vec![Value::from_u32(0); nv],
            budget: 0,
        }
    }

    fn var(&self, v: Value) -> Option<u32> {
        self.var_of.get(v.index()).copied().filter(|&x| x != NONE)
    }

    /// The variables a safepoint instruction relocates, with the new values.
    fn relocs(&self, func: &Function, inst: Inst, out: &mut Vec<(u32, Value)>) {
        out.clear();
        for &e in gc_list(&func.insts[inst.index()].data) {
            if let Some(var) = self.var(e) {
                if let Some(v) = self.reloc_inst.get(key(inst.as_u32(), var)) {
                    out.push((var, Value::from_u32(v)));
                }
            }
        }
    }

    /// Records each block's last definition of each variable.
    fn prescan(&mut self, func: &Function, rpo: &[u32]) {
        let mut rel = Vec::new();
        for &bb in rpo {
            let block = Block::from_u32(bb);
            for &p in func.block_params(block) {
                if let Some(var) = self.var(p) {
                    self.end_def.insert(key(var, bb), p.as_u32());
                }
            }
            for inst in func.insts(block) {
                for r in func.results(inst) {
                    if let Some(var) = self.var(r) {
                        self.end_def.insert(key(var, bb), r.as_u32());
                    }
                }
                self.relocs(func, inst, &mut rel);
                for &(var, v) in &rel {
                    self.end_def.insert(key(var, bb), v.as_u32());
                }
            }
        }
    }

    fn def_block(func: &Function, r: Value) -> u32 {
        match func.value_def(r) {
            Some(ValueDef::Param { block, .. }) => block.as_u32(),
            Some(ValueDef::Result { inst, .. }) => {
                func.inst_block(inst).map_or(NONE, Block::as_u32)
            }
            _ => NONE,
        }
    }

    fn new_param(&mut self, func: &mut Function, b: Block, var: u32) -> Result<Value, BuildError> {
        self.budget = self
            .budget
            .checked_sub(1)
            .ok_or(BuildError::BudgetExceeded)?;
        let index = func.blocks[b.index()].params.len() as u32;
        let v = func
            .push_value(Type::Ref, ValueDef::Param { block: b, index })
            .ok_or(BuildError::CapacityExceeded)?;
        func.blocks[b.index()].params.push(v);
        if self.param_var.len() <= v.index() {
            self.param_var.resize(v.index() + 1, NONE);
        }
        self.param_var[v.index()] = var;
        self.created.push(v);
        self.entry_def.insert(key(var, b.as_u32()), v.as_u32());
        self.stack.push(b);
        Ok(v)
    }

    /// The variable's value on entry to `block`, iteratively.
    fn entry(&mut self, func: &mut Function, var: u32, block: Block) -> Result<Value, BuildError> {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.stamp.iter_mut().for_each(|s| *s = 0);
            self.epoch = 1;
        }
        self.chain.clear();
        let r = self.vars[var as usize];
        let home = Repair::def_block(func, r);
        let mut cur = block;
        let value = loop {
            if let Some(v) = self.entry_def.get(key(var, cur.as_u32())) {
                break Value::from_u32(v);
            }
            if cur.as_u32() == home {
                break r;
            }
            let preds = self.preds.get(cur.index()).map_or(&[][..], Vec::as_slice);
            if preds.is_empty() {
                break r;
            }
            let p = preds[0];
            if preds.len() == 1
                && self.stamp[cur.index()] != self.epoch
                && self.inv_reloc.get(key(var, p.as_u32())).is_none()
            {
                self.stamp[cur.index()] = self.epoch;
                self.chain.push(cur);
                if let Some(v) = self.end_def.get(key(var, p.as_u32())) {
                    break Value::from_u32(v);
                }
                cur = p;
                continue;
            }
            break self.new_param(func, cur, var)?;
        };
        for i in 0..self.chain.len() {
            let c = self.chain[i];
            self.entry_def.insert(key(var, c.as_u32()), value.as_u32());
        }
        Ok(value)
    }

    /// The argument an edge from `p` passes for `var`.
    fn edge_value(
        &mut self,
        func: &mut Function,
        var: u32,
        p: Block,
    ) -> Result<BlockArg, BuildError> {
        if matches!(func.terminator(p), Some(Terminator::Invoke { .. })) {
            if let Some(k) = self.inv_reloc.get(key(var, p.as_u32())) {
                return Ok(BlockArg::Result(k));
            }
        }
        if let Some(v) = self.end_def.get(key(var, p.as_u32())) {
            return Ok(BlockArg::Value(Value::from_u32(v)));
        }
        Ok(BlockArg::Value(self.entry(func, var, p)?))
    }

    /// Gives every new parameter of `b` its argument on every edge into `b`.
    fn complete(&mut self, func: &mut Function, b: Block) -> Result<(), BuildError> {
        let first = self.orig_params.get(b.index()).copied().unwrap_or(0) as usize;
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
                let Some(min) = min.filter(|&m| m < nparams && m >= first) else {
                    break;
                };
                let q = func.blocks[b.index()].params[min];
                let var = self.param_var.get(q.index()).copied().unwrap_or(NONE);
                if var == NONE {
                    break;
                }
                let arg = self.edge_value(func, var, p)?;
                if let Some(term) = func.blocks[p.index()].term.as_mut() {
                    term.for_each_successor_mut(|d| {
                        if d.block == b && d.args.len() == min {
                            d.args.push(arg);
                        }
                    });
                }
            }
        }
        Ok(())
    }

    /// The reaching value of `var` at the current point of block `bb`'s scan.
    fn reaching(&mut self, func: &mut Function, var: u32, bb: u32) -> Result<Value, BuildError> {
        if self.cur_block[var as usize] == bb + 1 {
            return Ok(self.cur_val[var as usize]);
        }
        self.entry(func, var, Block::from_u32(bb))
    }

    fn set_cur(&mut self, var: u32, bb: u32, v: Value) {
        self.cur_block[var as usize] = bb + 1;
        self.cur_val[var as usize] = v;
    }

    /// Rewrites every use in the reachable blocks to the reaching definition.
    fn rewrite(&mut self, func: &mut Function, rpo: &[u32]) -> Result<(), BuildError> {
        let mut insts = Vec::new();
        let mut rel = Vec::new();
        let mut operands: Vec<Value> = Vec::new();
        let mut map: Vec<(Value, Value)> = Vec::new();
        for &bb in rpo {
            let block = Block::from_u32(bb);
            let nparams = self.orig_params.get(bb as usize).copied().unwrap_or(0) as usize;
            for k in 0..nparams {
                let p = func.blocks[bb as usize].params[k];
                if let Some(var) = self.var(p) {
                    self.set_cur(var, bb, p);
                }
            }
            insts.clear();
            insts.extend(func.insts(block));
            for &inst in &insts {
                self.relocs(func, inst, &mut rel);
                map.clear();
                operands.clear();
                func.insts[inst.index()]
                    .data
                    .for_each_operand(|u| operands.push(u));
                for &u in &operands {
                    if let Some(var) = self.var(u) {
                        if !map.iter().any(|&(o, _)| o == u) {
                            let v = self.reaching(func, var, bb)?;
                            map.push((u, v));
                        }
                    }
                }
                if !map.is_empty() {
                    let m: &[(Value, Value)] = &map;
                    func.insts[inst.index()]
                        .data
                        .map_operands(|u| m.iter().find(|&&(o, _)| o == u).map_or(u, |&(_, n)| n));
                }
                for r in func.results(inst) {
                    if let Some(var) = self.var(r) {
                        self.set_cur(var, bb, r);
                    }
                }
                for &(var, v) in &rel {
                    self.set_cur(var, bb, v);
                }
            }
            self.rewrite_terminator(func, bb, &mut map)?;
        }
        while let Some(b) = self.stack.pop() {
            self.complete(func, b)?;
        }
        Ok(())
    }

    fn rewrite_terminator(
        &mut self,
        func: &mut Function,
        bb: u32,
        map: &mut Vec<(Value, Value)>,
    ) -> Result<(), BuildError> {
        let Some(term) = func.blocks[bb as usize].term.as_ref() else {
            return Ok(());
        };
        let is_invoke = matches!(term, Terminator::Invoke { .. });
        let mut own = Vec::new();
        term.for_each_own_operand(|u| own.push(u));
        // Edge arguments for the targets' original parameters: (edge, index, var).
        let mut edges: Vec<(usize, usize, u32)> = Vec::new();
        for (k, dest) in term.successors().enumerate() {
            let n = self
                .orig_params
                .get(dest.block.index())
                .copied()
                .unwrap_or(0) as usize;
            for (i, arg) in dest.args.iter().enumerate().take(n) {
                if let BlockArg::Value(u) = arg {
                    if let Some(var) = self.var(*u) {
                        edges.push((k, i, var));
                    }
                }
            }
        }
        map.clear();
        for u in own {
            if let Some(var) = self.var(u) {
                if !map.iter().any(|&(o, _)| o == u) {
                    let v = self.reaching(func, var, bb)?;
                    map.push((u, v));
                }
            }
        }
        let mut new_args: Vec<BlockArg> = Vec::with_capacity(edges.len());
        for &(_, _, var) in &edges {
            let arg = match self.inv_reloc.get(key(var, bb)) {
                Some(k) if is_invoke => BlockArg::Result(k),
                _ => BlockArg::Value(self.reaching(func, var, bb)?),
            };
            new_args.push(arg);
        }
        let Some(term) = func.blocks[bb as usize].term.as_mut() else {
            return Ok(());
        };
        if !map.is_empty() {
            let m: &[(Value, Value)] = map;
            term.map_own_operands(|u| m.iter().find(|&&(o, _)| o == u).map_or(u, |&(_, n)| n));
        }
        if !edges.is_empty() {
            let mut k = 0;
            let mut e = 0;
            term.for_each_successor_mut(|dest| {
                while let Some(&(at, i, _)) = edges.get(e) {
                    if at != k {
                        break;
                    }
                    if let (Some(slot), Some(&arg)) = (dest.args.get_mut(i), new_args.get(e)) {
                        *slot = arg;
                    }
                    e += 1;
                }
                k += 1;
            });
        }
        Ok(())
    }
}
