//! Validation: the contract every pass and backend relies on.
//!
//! Order of checks (the first failure is reported): module declarations, then
//! each function in handle order. Within a function: entry and signature, value
//! forwarding, block layout, definitions, terminator presence, instruction typing,
//! terminator typing, exception-edge and attribute rules, dominance, safepoints.
//! Every walk is iterative and every check is linear in the size of the function
//! (the safepoint check is linear in the live ranges of GC references).

mod dom;
mod gc;

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

use crate::entity::{Block, FuncId, Inst, NONE, Value};
use crate::error::{ValidationError, ValidationErrorKind as K};
use crate::function::{Function, ValueDef};
use crate::inst::{Attrs, InstData, Terminator};
use crate::module::{Decls, Linkage, Module, SymbolRef, check_global};
use crate::types::Type;
use crate::typing;

/// Successor lists in compressed form: `succ[start[b]..start[b + 1]]`.
pub(crate) struct Cfg {
    pub(crate) start: Vec<u32>,
    pub(crate) succ: Vec<u32>,
}

impl Cfg {
    pub(crate) fn of(func: &Function) -> Cfg {
        let n = func.blocks.len();
        let mut start = Vec::with_capacity(n + 1);
        let mut succ = Vec::new();
        for node in &func.blocks {
            start.push(succ.len() as u32);
            if !node.live {
                continue;
            }
            if let Some(term) = &node.term {
                for dest in term.successors() {
                    if dest.block.index() < n {
                        succ.push(dest.block.as_u32());
                    }
                }
            }
        }
        start.push(succ.len() as u32);
        Cfg { start, succ }
    }

    pub(crate) fn succs(&self, b: usize) -> &[u32] {
        let lo = self.start.get(b).copied().unwrap_or(0) as usize;
        let hi = self.start.get(b + 1).copied().unwrap_or(0) as usize;
        self.succ.get(lo..hi).unwrap_or(&[])
    }

    /// The reverse graph.
    pub(crate) fn reversed(&self) -> Cfg {
        let n = self.start.len().saturating_sub(1);
        let mut count = alloc::vec![0u32; n + 1];
        for &s in &self.succ {
            count[s as usize + 1] += 1;
        }
        for i in 0..n {
            count[i + 1] += count[i];
        }
        let mut fill = count.clone();
        let mut pred = alloc::vec![0u32; self.succ.len()];
        for b in 0..n {
            for &s in self.succs(b) {
                let slot = &mut fill[s as usize];
                pred[*slot as usize] = b as u32;
                *slot += 1;
            }
        }
        Cfg {
            start: count,
            succ: pred,
        }
    }
}

fn at(func: FuncId, block: Option<Block>, inst: Option<Inst>, kind: K) -> ValidationError {
    ValidationError {
        func: Some(func),
        block,
        inst,
        kind,
    }
}

pub(crate) fn validate_module(m: &Module) -> Result<(), ValidationError> {
    validate_decls(m)?;
    for f in m.functions() {
        validate_decl_body(m, f)?;
    }
    Ok(())
}

pub(crate) fn validate_one(m: &Module, f: FuncId) -> Result<(), ValidationError> {
    if m.decls.funcs.get(f.index()).is_none() {
        return Err(ValidationError::module(K::UnknownFunction { func: f }));
    }
    validate_decl_body(m, f)
}

fn validate_decl_body(m: &Module, f: FuncId) -> Result<(), ValidationError> {
    let Some(decl) = m.decls.funcs.get(f.index()) else {
        return Err(ValidationError::module(K::UnknownFunction { func: f }));
    };
    if m.decls.sigs.get(decl.sig.index()).is_none() {
        return Err(at(f, None, None, K::UnknownSignature { sig: decl.sig }));
    }
    match (decl.linkage, m.function(f)) {
        (Linkage::Import, Some(_)) => Err(at(f, None, None, K::ImportWithBody)),
        (Linkage::Import, None) => Ok(()),
        (_, None) => Err(at(f, None, None, K::MissingBody)),
        (_, Some(body)) => validate_body(&m.decls, f, body),
    }
}

fn validate_decls(m: &Module) -> Result<(), ValidationError> {
    let d = &m.decls;
    let mut names = BTreeSet::new();
    let all = d
        .funcs
        .iter()
        .map(|f| f.name.as_str())
        .chain(d.globals.iter().map(|g| g.name.as_str()));
    for name in all {
        if name.is_empty() {
            return Err(ValidationError::module(K::EmptyName));
        }
        if !names.insert(name) {
            return Err(ValidationError::module(K::DuplicateName {
                name: name.into(),
            }));
        }
    }
    for (i, data) in d.aggs.iter().enumerate() {
        let agg = crate::entity::AggId::from_u32(i as u32);
        match d.compute_layout(&data.agg, i) {
            Ok((layout, offsets)) if layout == data.layout && offsets == data.offsets => {}
            Err(crate::error::ModuleError::UnknownAggregate { agg }) => {
                return Err(ValidationError::module(K::UnknownAggregate { agg }));
            }
            _ => return Err(ValidationError::module(K::BadAggregate { agg })),
        }
    }
    for (i, g) in d.globals.iter().enumerate() {
        let global = crate::entity::GlobalId::from_u32(i as u32);
        if let Err(e) = check_global(&g.global, d.ptr) {
            return Err(ValidationError::module(match e {
                crate::error::ModuleError::BadAlignment { align } => K::BadAlignment { align },
                _ => K::BadGlobal { global },
            }));
        }
        for r in &g.global.relocs {
            match r.target {
                SymbolRef::Func(func) if d.funcs.get(func.index()).is_none() => {
                    return Err(ValidationError::module(K::UnknownFunction { func }));
                }
                SymbolRef::Global(gl) if d.globals.get(gl.index()).is_none() => {
                    return Err(ValidationError::module(K::UnknownGlobal { global: gl }));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Validates one body against the declarations.
pub(crate) fn validate_body(
    decls: &Decls,
    id: FuncId,
    func: &Function,
) -> Result<(), ValidationError> {
    let Some(decl) = decls.funcs.get(id.index()) else {
        return Err(ValidationError::module(K::UnknownFunction { func: id }));
    };
    let Some(sig) = decls.sigs.get(decl.sig.index()) else {
        return Err(at(id, None, None, K::UnknownSignature { sig: decl.sig }));
    };
    let attrs = decl.attrs;

    check_entry(id, func, &sig.params)?;
    check_aliases(id, func)?;
    check_layout(id, func)?;
    let defined = check_definitions(id, func)?;
    check_slots(id, func)?;

    let mut scratch = Vec::new();
    for b in func.blocks() {
        let node = &func.blocks[b.index()];
        let Some(term) = &node.term else {
            return Err(at(id, Some(b), None, K::MissingTerminator));
        };
        for inst in func.insts(b) {
            scratch.clear();
            let data = &func.insts[inst.index()].data;
            typing::inst_types(decls, func, data, false, &mut scratch)
                .map_err(|k| at(id, Some(b), Some(inst), k))?;
            let results = func.results(inst);
            let ok = results.len() == scratch.len()
                && results
                    .iter()
                    .zip(scratch.iter())
                    .all(|(v, &t)| func.value_type(v) == Some(t));
            if !ok {
                return Err(at(id, Some(b), Some(inst), K::ResultMismatch));
            }
        }
        typing::check_terminator(decls, func, sig, term, &mut scratch)
            .map_err(|k| at(id, Some(b), None, k))?;
    }

    check_eh_and_attrs(decls, id, func, attrs)?;

    let cfg = Cfg::of(func);
    let idom = dom::compute_idoms(func.entry().index(), &cfg);
    dom::check_dominance(id, func, &cfg, &idom, &defined)?;
    gc::check_safepoints(decls, id, func, &cfg, &idom)
}

fn check_entry(id: FuncId, func: &Function, params: &[Type]) -> Result<(), ValidationError> {
    let entry = func.entry();
    if !func.is_live_block(entry) {
        return Err(at(id, None, None, K::BadEntry));
    }
    let actual = func.block_params(entry);
    if actual.len() != params.len() {
        return Err(at(
            id,
            Some(entry),
            None,
            K::EntryParams {
                expected: params.len(),
                found: actual.len(),
            },
        ));
    }
    for (&v, &ty) in actual.iter().zip(params) {
        let found = func
            .value_type(v)
            .ok_or_else(|| at(id, Some(entry), None, K::ValueOutOfRange { value: v }))?;
        if found != ty {
            return Err(at(
                id,
                Some(entry),
                None,
                K::TypeMismatch {
                    value: Some(v),
                    expected: ty,
                    found,
                },
            ));
        }
    }
    Ok(())
}

/// Forwarding targets exist, keep the type, and never loop.
fn check_aliases(id: FuncId, func: &Function) -> Result<(), ValidationError> {
    let n = func.values.len();
    // 0 = not visited, 1 = on the current chain, 2 = known to terminate.
    let mut state = alloc::vec![0u8; n];
    let mut chain = Vec::new();
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        chain.clear();
        let mut v = start;
        loop {
            match state[v] {
                2 => break,
                1 => {
                    return Err(at(
                        id,
                        None,
                        None,
                        K::BadAlias {
                            value: Value::from_u32(v as u32),
                        },
                    ));
                }
                _ => {}
            }
            state[v] = 1;
            chain.push(v);
            let alias = func.values[v].alias;
            if alias == NONE {
                break;
            }
            let Some(target) = func.values.get(alias as usize) else {
                return Err(at(
                    id,
                    None,
                    None,
                    K::BadAlias {
                        value: Value::from_u32(v as u32),
                    },
                ));
            };
            if target.ty != func.values[v].ty {
                return Err(at(
                    id,
                    None,
                    None,
                    K::BadAlias {
                        value: Value::from_u32(v as u32),
                    },
                ));
            }
            v = alias as usize;
        }
        for &c in &chain {
            state[c] = 2;
        }
    }
    Ok(())
}

/// Each live block's instruction list is a well-formed chain of instructions that
/// belong to it, and no instruction claims a block whose list misses it.
fn check_layout(id: FuncId, func: &Function) -> Result<(), ValidationError> {
    let mut seen = alloc::vec![false; func.insts.len()];
    let mut linked = 0usize;
    for b in func.blocks() {
        let node = &func.blocks[b.index()];
        let corrupt = || at(id, Some(b), None, K::CorruptLayout);
        let mut prev = NONE;
        let mut cur = node.first;
        while cur != NONE {
            let Some(inst) = func.insts.get(cur as usize) else {
                return Err(corrupt());
            };
            if seen[cur as usize] || inst.block != b.as_u32() || inst.prev != prev {
                return Err(corrupt());
            }
            seen[cur as usize] = true;
            linked += 1;
            prev = cur;
            cur = inst.next;
        }
        if node.last != prev {
            return Err(corrupt());
        }
    }
    let claimed = func.insts.iter().filter(|n| n.block != NONE).count();
    if claimed != linked {
        let stray = func
            .insts
            .iter()
            .enumerate()
            .find(|(i, n)| n.block != NONE && !seen[*i])
            .map(|(i, n)| (Inst::from_u32(i as u32), Block::from_u32(n.block)));
        return Err(at(
            id,
            stray.map(|s| s.1),
            stray.map(|s| s.0),
            K::CorruptLayout,
        ));
    }
    Ok(())
}

/// Every listed parameter and result agrees with its recorded definition, and no
/// value is listed twice. Returns which values are defined.
fn check_definitions(id: FuncId, func: &Function) -> Result<Vec<bool>, ValidationError> {
    let mut defined = alloc::vec![false; func.values.len()];
    for b in func.blocks() {
        for (i, &p) in func.block_params(b).iter().enumerate() {
            let Some(node) = func.values.get(p.index()) else {
                return Err(at(id, Some(b), None, K::ValueOutOfRange { value: p }));
            };
            if node.def
                != (ValueDef::Param {
                    block: b,
                    index: i as u32,
                })
            {
                return Err(at(
                    id,
                    Some(b),
                    None,
                    K::InconsistentDefinition { value: p },
                ));
            }
            if core::mem::replace(&mut defined[p.index()], true) {
                return Err(at(id, Some(b), None, K::DefinedTwice { value: p }));
            }
        }
        for inst in func.insts(b) {
            for (k, v) in func.results(inst).iter().enumerate() {
                let Some(node) = func.values.get(v.index()) else {
                    return Err(at(id, Some(b), Some(inst), K::ValueOutOfRange { value: v }));
                };
                if node.def
                    != (ValueDef::Result {
                        inst,
                        index: k as u32,
                    })
                {
                    return Err(at(
                        id,
                        Some(b),
                        Some(inst),
                        K::InconsistentDefinition { value: v },
                    ));
                }
                if core::mem::replace(&mut defined[v.index()], true) {
                    return Err(at(id, Some(b), Some(inst), K::DefinedTwice { value: v }));
                }
            }
        }
    }
    Ok(defined)
}

fn check_slots(id: FuncId, func: &Function) -> Result<(), ValidationError> {
    for (i, s) in func.slots.iter().enumerate() {
        if !crate::module::valid_align(
            u64::from(s.align),
            u64::from(crate::function::SlotData::MAX_ALIGN),
        ) {
            return Err(at(
                id,
                None,
                None,
                K::BadSlot {
                    slot: crate::entity::StackSlot::from_u32(i as u32),
                },
            ));
        }
    }
    Ok(())
}

/// Landing pads are reached only by unwind edges; `nounwind`, `noreturn`, and
/// `nogc` function attributes hold.
fn check_eh_and_attrs(
    decls: &Decls,
    id: FuncId,
    func: &Function,
    attrs: Attrs,
) -> Result<(), ValidationError> {
    let mut pad = alloc::vec![false; func.blocks.len()];
    for b in func.blocks() {
        if let Some(target) = func.terminator(b).and_then(typing::unwind_target) {
            if let Some(p) = pad.get_mut(target.index()) {
                *p = true;
            }
        }
    }
    let nounwind = attrs.contains(Attrs::NOUNWIND);
    let noreturn = attrs.contains(Attrs::NORETURN);
    let nogc = attrs.contains(Attrs::NOGC);
    for b in func.blocks() {
        let Some(term) = func.terminator(b) else {
            continue;
        };
        let mut bad_pad = None;
        match term {
            Terminator::Invoke { normal, .. } => {
                if pad.get(normal.block.index()).copied().unwrap_or(false) {
                    bad_pad = Some(normal.block);
                }
            }
            other => {
                for dest in other.successors() {
                    if pad.get(dest.block.index()).copied().unwrap_or(false) {
                        bad_pad = Some(dest.block);
                    }
                }
            }
        }
        if let Some(block) = bad_pad {
            return Err(at(id, Some(b), None, K::LandingPadPredecessor { block }));
        }
        let term_err = |k| Err(at(id, Some(b), None, k));
        match term {
            Terminator::Resume { .. } if nounwind => return term_err(K::UnwindInNounwind),
            Terminator::Return { .. } if noreturn => return term_err(K::ReturnInNoreturn),
            Terminator::TailCall(call) => {
                let ca = typing::call_attrs(decls, call);
                if nounwind && !ca.contains(Attrs::NOUNWIND) {
                    return term_err(K::UnwindInNounwind);
                }
                if noreturn && !ca.contains(Attrs::NORETURN) {
                    return term_err(K::ReturnInNoreturn);
                }
                if nogc && !ca.contains(Attrs::NOGC) {
                    return term_err(K::SafepointInNogc);
                }
            }
            Terminator::Invoke { call, .. } if nogc && typing::call_is_safepoint(decls, call) => {
                return term_err(K::SafepointInNogc);
            }
            _ => {}
        }
        if nounwind || nogc {
            for inst in func.insts(b) {
                let inst_err = |k| Err(at(id, Some(b), Some(inst), k));
                match &func.insts[inst.index()].data {
                    InstData::Call(call) => {
                        let ca = typing::call_attrs(decls, call);
                        if nounwind && !ca.contains(Attrs::NOUNWIND) {
                            return inst_err(K::UnwindInNounwind);
                        }
                        if nogc && !ca.contains(Attrs::NOGC) {
                            return inst_err(K::SafepointInNogc);
                        }
                    }
                    InstData::Safepoint { .. } if nogc => return inst_err(K::SafepointInNogc),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
