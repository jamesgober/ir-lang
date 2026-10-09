//! Dominators (Cooper–Harvey–Kennedy) and the linear SSA dominance check.

use alloc::vec::Vec;

use super::{Cfg, at};
use crate::entity::{Block, FuncId, Value};
use crate::error::{ValidationError, ValidationErrorKind as K};
use crate::function::Function;

/// Immediate dominators: `idom[b]` is `b`'s immediate dominator for a block
/// reachable from `entry` (the entry's is itself) and `u32::MAX` otherwise.
pub(crate) struct Idom {
    pub(crate) idom: Vec<u32>,
    /// Reachable blocks in reverse postorder.
    pub(crate) rpo: Vec<u32>,
}

impl Idom {
    pub(crate) fn reachable(&self, b: usize) -> bool {
        self.idom.get(b).is_some_and(|&d| d != u32::MAX)
    }
}

/// Postorder of the blocks reachable from `entry`, with an explicit stack.
fn postorder(entry: usize, cfg: &Cfg) -> Vec<u32> {
    let n = cfg.start.len().saturating_sub(1);
    let mut order = Vec::new();
    if entry >= n {
        return order;
    }
    let mut visited = alloc::vec![false; n];
    let mut stack: Vec<(u32, u32)> = Vec::new();
    visited[entry] = true;
    stack.push((entry as u32, 0));
    while let Some(top) = stack.last_mut() {
        let (b, i) = *top;
        let succs = cfg.succs(b as usize);
        if let Some(&s) = succs.get(i as usize) {
            top.1 += 1;
            if let Some(v) = visited.get_mut(s as usize) {
                if !*v {
                    *v = true;
                    stack.push((s, 0));
                }
            }
        } else {
            order.push(b);
            let _ = stack.pop();
        }
    }
    order
}

/// Cooper, Harvey, and Kennedy, "A Simple, Fast Dominance Algorithm" (2001).
pub(crate) fn compute_idoms(entry: usize, cfg: &Cfg) -> Idom {
    let n = cfg.start.len().saturating_sub(1);
    let po = postorder(entry, cfg);
    let mut po_num = alloc::vec![u32::MAX; n];
    for (i, &b) in po.iter().enumerate() {
        po_num[b as usize] = i as u32;
    }
    let preds = cfg.reversed();
    let mut idom = alloc::vec![u32::MAX; n];
    if entry < n {
        idom[entry] = entry as u32;
    }
    let rpo: Vec<u32> = po.iter().rev().copied().collect();
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &rpo {
            if b as usize == entry {
                continue;
            }
            let mut new = u32::MAX;
            for &p in preds.succs(b as usize) {
                if idom.get(p as usize).copied().unwrap_or(u32::MAX) == u32::MAX {
                    continue;
                }
                new = if new == u32::MAX {
                    p
                } else {
                    intersect(p, new, &idom, &po_num)
                };
            }
            if idom[b as usize] != new {
                idom[b as usize] = new;
                changed = true;
            }
        }
    }
    Idom { idom, rpo }
}

fn intersect(mut a: u32, mut b: u32, idom: &[u32], po_num: &[u32]) -> u32 {
    // Both fingers only ever climb toward the entry (whose postorder number is the
    // largest), so the loop ends; the step bound and checked reads keep it total
    // regardless.
    let num = |x: u32| po_num.get(x as usize).copied().unwrap_or(u32::MAX);
    let up = |x: u32| idom.get(x as usize).copied().unwrap_or(x);
    let mut budget = 2 * idom.len() + 2;
    while a != b && budget > 0 {
        budget -= 1;
        while num(a) < num(b) && budget > 0 {
            budget -= 1;
            a = up(a);
        }
        while num(b) < num(a) && budget > 0 {
            budget -= 1;
            b = up(b);
        }
    }
    a
}

enum Visit {
    Enter(u32),
    Exit(u32),
}

/// Every value used in a reachable block is available there: defined by a block
/// that dominates the use, earlier in the same block, or as a parameter of a
/// dominating block. One pre-order walk of the dominator tree carries a single
/// availability set (definitions added on entry, removed on exit), so the check is
/// linear with no per-block allocation.
pub(crate) fn check_dominance(
    id: FuncId,
    func: &Function,
    cfg: &Cfg,
    idom: &Idom,
    defined: &[bool],
) -> Result<(), ValidationError> {
    let n = cfg.start.len().saturating_sub(1);
    let entry = func.entry().index();
    // Dominator-tree children, compressed.
    let mut count = alloc::vec![0u32; n + 1];
    for (b, &d) in idom.idom.iter().enumerate() {
        if d != u32::MAX && b != entry {
            count[d as usize + 1] += 1;
        }
    }
    for i in 0..n {
        count[i + 1] += count[i];
    }
    let mut fill = count.clone();
    let mut kids = alloc::vec![0u32; count[n] as usize];
    for (b, &d) in idom.idom.iter().enumerate() {
        if d != u32::MAX && b != entry {
            kids[fill[d as usize] as usize] = b as u32;
            fill[d as usize] += 1;
        }
    }

    let mut available = alloc::vec![false; func.values.len()];
    let require =
        |v: Value, available: &[bool], block: Block, inst| -> Result<(), ValidationError> {
            let r = func.resolve(v);
            if available.get(r.index()).copied().unwrap_or(false) {
                return Ok(());
            }
            let kind = if r.index() < defined.len() && !defined[r.index()] {
                K::UseOfDetached { value: v }
            } else if r.index() >= func.values.len() {
                K::ValueOutOfRange { value: v }
            } else {
                K::UseBeforeDef { value: v }
            };
            Err(at(id, Some(block), inst, kind))
        };

    if entry >= n {
        return Ok(());
    }
    let mut stack = alloc::vec![Visit::Enter(entry as u32)];
    while let Some(visit) = stack.pop() {
        match visit {
            Visit::Exit(b) => {
                let block = Block::from_u32(b);
                for &p in func.block_params(block) {
                    if let Some(a) = available.get_mut(p.index()) {
                        *a = false;
                    }
                }
                for inst in func.insts(block) {
                    for v in func.results(inst) {
                        if let Some(a) = available.get_mut(v.index()) {
                            *a = false;
                        }
                    }
                }
            }
            Visit::Enter(b) => {
                let block = Block::from_u32(b);
                for &p in func.block_params(block) {
                    if let Some(a) = available.get_mut(p.index()) {
                        *a = true;
                    }
                }
                for inst in func.insts(block) {
                    let mut err = Ok(());
                    func.insts[inst.index()].data.for_each_operand(|v| {
                        if err.is_ok() {
                            err = require(v, &available, block, Some(inst));
                        }
                    });
                    err?;
                    for v in func.results(inst) {
                        if let Some(a) = available.get_mut(v.index()) {
                            *a = true;
                        }
                    }
                }
                if let Some(term) = func.terminator(block) {
                    let mut err = Ok(());
                    term.for_each_operand(|v| {
                        if err.is_ok() {
                            err = require(v, &available, block, None);
                        }
                    });
                    err?;
                }
                stack.push(Visit::Exit(b));
                let lo = count[b as usize] as usize;
                let hi = count[b as usize + 1] as usize;
                for &k in kids.get(lo..hi).unwrap_or(&[]) {
                    stack.push(Visit::Enter(k));
                }
            }
        }
    }
    Ok(())
}
