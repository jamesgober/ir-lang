//! Dominators (Lengauer–Tarjan) and the linear SSA dominance check.

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

/// A depth-first walk from `entry` with an explicit stack: the reachable blocks in
/// preorder and in postorder, and each block's depth-first tree parent.
struct Dfs {
    pre: Vec<u32>,
    post: Vec<u32>,
    parent: Vec<u32>,
}

fn dfs(entry: usize, cfg: &Cfg) -> Dfs {
    let n = cfg.start.len().saturating_sub(1);
    let mut out = Dfs {
        pre: Vec::new(),
        post: Vec::new(),
        parent: alloc::vec![u32::MAX; n],
    };
    if entry >= n {
        return out;
    }
    let mut visited = alloc::vec![false; n];
    let mut stack: Vec<(u32, u32)> = Vec::new();
    visited[entry] = true;
    out.pre.push(entry as u32);
    stack.push((entry as u32, 0));
    while let Some(top) = stack.last_mut() {
        let (b, i) = *top;
        let succs = cfg.succs(b as usize);
        if let Some(&s) = succs.get(i as usize) {
            top.1 += 1;
            if let Some(v) = visited.get_mut(s as usize) {
                if !*v {
                    *v = true;
                    out.pre.push(s);
                    out.parent[s as usize] = b;
                    stack.push((s, 0));
                }
            }
        } else {
            out.post.push(b);
            let _ = stack.pop();
        }
    }
    out
}

/// Immediate dominators by Lengauer and Tarjan, "A Fast Algorithm for Finding
/// Dominators in a Flowgraph" (1979), simple version: path compression without
/// balanced linking, `O(e log n)` for `e` edges and `n` blocks. Everything runs in
/// preorder numbers, and compression uses an explicit stack, so no shape of graph
/// recurses or goes quadratic.
pub(crate) fn compute_idoms(entry: usize, cfg: &Cfg) -> Idom {
    const NO: u32 = u32::MAX;
    let n = cfg.start.len().saturating_sub(1);
    let walk = dfs(entry, cfg);
    let rpo: Vec<u32> = walk.post.iter().rev().copied().collect();
    let mut idom = alloc::vec![NO; n];
    let count = walk.pre.len();
    if count == 0 {
        return Idom { idom, rpo };
    }
    // Preorder number of each block (NO when unreachable).
    let mut num = alloc::vec![NO; n];
    for (i, &b) in walk.pre.iter().enumerate() {
        num[b as usize] = i as u32;
    }
    // From here on every array is indexed by preorder number.
    let parent: Vec<u32> = walk
        .pre
        .iter()
        .map(|&b| {
            let p = walk.parent[b as usize];
            if p == NO { 0 } else { num[p as usize] }
        })
        .collect();
    let mut semi: Vec<u32> = (0..count as u32).collect();
    let mut label: Vec<u32> = (0..count as u32).collect();
    let mut ancestor = alloc::vec![NO; count];
    let mut dom = alloc::vec![NO; count];
    // Buckets as intrusive lists: `head[v]` is the first vertex whose
    // semidominator is `v`, `next[w]` the one after `w`.
    let mut head = alloc::vec![NO; count];
    let mut next = alloc::vec![NO; count];
    let mut path: Vec<u32> = Vec::new();
    let preds = cfg.reversed();

    // The vertex with the least semidominator on the forest path from `v` up to
    // (not including) its root, compressing the path on the way.
    let mut eval = |v: u32, ancestor: &mut [u32], label: &mut [u32], semi: &[u32]| -> u32 {
        if ancestor[v as usize] == NO {
            return v;
        }
        path.clear();
        let mut x = v;
        while ancestor[ancestor[x as usize] as usize] != NO {
            path.push(x);
            x = ancestor[x as usize];
        }
        while let Some(x) = path.pop() {
            let a = ancestor[x as usize] as usize;
            if semi[label[a] as usize] < semi[label[x as usize] as usize] {
                label[x as usize] = label[a];
            }
            ancestor[x as usize] = ancestor[a];
        }
        label[v as usize]
    };

    for w in (1..count).rev() {
        let block = walk.pre[w] as usize;
        for &p in preds.succs(block) {
            let v = num.get(p as usize).copied().unwrap_or(NO);
            if v == NO {
                continue;
            }
            let u = eval(v, &mut ancestor, &mut label, &semi);
            if semi[u as usize] < semi[w] {
                semi[w] = semi[u as usize];
            }
        }
        let s = semi[w] as usize;
        next[w] = head[s];
        head[s] = w as u32;
        let p = parent[w];
        ancestor[w] = p;
        let mut v = core::mem::replace(&mut head[p as usize], NO);
        while v != NO {
            let u = eval(v, &mut ancestor, &mut label, &semi);
            dom[v as usize] = if semi[u as usize] < semi[v as usize] {
                u
            } else {
                p
            };
            v = next[v as usize];
        }
    }
    for w in 1..count {
        if dom[w] != semi[w] {
            dom[w] = dom[dom[w] as usize];
        }
    }
    dom[0] = 0;
    for (i, &b) in walk.pre.iter().enumerate() {
        idom[b as usize] = walk.pre[dom[i] as usize];
    }
    Idom { idom, rpo }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The previous algorithm (Cooper, Harvey, and Kennedy, iterative), kept as the
    /// oracle: simple enough to trust, quadratic on some graphs.
    fn reference(entry: usize, cfg: &Cfg) -> Vec<u32> {
        let n = cfg.start.len().saturating_sub(1);
        let po = dfs(entry, cfg).post;
        let mut po_num = alloc::vec![u32::MAX; n];
        for (i, &b) in po.iter().enumerate() {
            po_num[b as usize] = i as u32;
        }
        let preds = cfg.reversed();
        let mut idom = alloc::vec![u32::MAX; n];
        if entry < n {
            idom[entry] = entry as u32;
        }
        let mut changed = true;
        while changed {
            changed = false;
            for &b in po.iter().rev() {
                if b as usize == entry {
                    continue;
                }
                let mut new = u32::MAX;
                for &p in preds.succs(b as usize) {
                    if idom[p as usize] == u32::MAX {
                        continue;
                    }
                    new = if new == u32::MAX {
                        p
                    } else {
                        let (mut a, mut c) = (p, new);
                        while a != c {
                            while po_num[a as usize] < po_num[c as usize] {
                                a = idom[a as usize];
                            }
                            while po_num[c as usize] < po_num[a as usize] {
                                c = idom[c as usize];
                            }
                        }
                        a
                    };
                }
                if idom[b as usize] != new {
                    idom[b as usize] = new;
                    changed = true;
                }
            }
        }
        idom
    }

    fn cfg_of(n: usize, edges: &[(u32, u32)]) -> Cfg {
        let mut start = Vec::with_capacity(n + 1);
        let mut succ = Vec::new();
        for b in 0..n as u32 {
            start.push(succ.len() as u32);
            succ.extend(edges.iter().filter(|e| e.0 == b).map(|e| e.1));
        }
        start.push(succ.len() as u32);
        Cfg { start, succ }
    }

    #[test]
    fn test_matches_the_iterative_algorithm_on_random_graphs() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rand = |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        for round in 0..3000 {
            let n = 1 + rand(40) as usize;
            let density = 1 + rand(4);
            let mut edges = Vec::new();
            for b in 0..n as u64 {
                for _ in 0..rand(density + 1) {
                    edges.push((b as u32, rand(n as u64) as u32));
                }
                // Mostly forward chains, so most blocks are reachable.
                if b + 1 < n as u64 && rand(3) != 0 {
                    edges.push((b as u32, b as u32 + 1));
                }
            }
            let entry = rand(n as u64) as usize;
            let cfg = cfg_of(n, &edges);
            let fast = compute_idoms(entry, &cfg);
            assert_eq!(fast.idom, reference(entry, &cfg), "round {round}");
            let rpo: Vec<u32> = dfs(entry, &cfg).post.into_iter().rev().collect();
            assert_eq!(fast.rpo, rpo);
        }
    }

    #[test]
    fn test_empty_and_out_of_range_entry() {
        let cfg = cfg_of(0, &[]);
        assert!(compute_idoms(0, &cfg).idom.is_empty());
        let cfg = cfg_of(3, &[(0, 1)]);
        let d = compute_idoms(5, &cfg);
        assert_eq!(d.idom, alloc::vec![u32::MAX; 3]);
        assert!(d.rpo.is_empty());
    }
}
