//! Dense renumbering of a function body.

use alloc::vec::Vec;

use crate::builder::{Builder, Remap};
use crate::entity::{Block, Inst, NONE, Value, ValueRange};
use crate::function::{BlockNode, Function, InstNode, ValueDef, ValueNode};

/// Renumbers `func` densely in layout order and returns the old-to-new mapping.
/// Operands must already be alias-resolved.
pub(crate) fn compact(func: &mut Function) -> Remap {
    let old_blocks = func.blocks.len();
    let mut order: Vec<Block> = Vec::with_capacity(old_blocks);
    let entry = func.entry();
    if func.is_live_block(entry) {
        order.push(entry);
    }
    order.extend(func.blocks().filter(|&b| b != entry));

    let mut block_map = alloc::vec![NONE; old_blocks];
    for (new, &b) in order.iter().enumerate() {
        block_map[b.index()] = new as u32;
    }

    // Values in definition order: each block's parameters, then each instruction's
    // results in layout order. Instructions in layout order.
    let mut value_map = alloc::vec![NONE; func.values.len()];
    let mut inst_map = alloc::vec![NONE; func.insts.len()];
    let mut new_values: Vec<ValueNode> = Vec::with_capacity(func.values.len());
    let mut inst_order: Vec<Inst> = Vec::with_capacity(func.insts.len());
    for (nb, &b) in order.iter().enumerate() {
        for (i, &p) in func.block_params(b).iter().enumerate() {
            value_map[p.index()] = new_values.len() as u32;
            new_values.push(ValueNode {
                ty: func.values[p.index()].ty,
                def: ValueDef::Param {
                    block: Block::from_u32(nb as u32),
                    index: i as u32,
                },
                alias: NONE,
            });
        }
        for inst in func.insts(b) {
            let ni = inst_order.len() as u32;
            inst_map[inst.index()] = ni;
            inst_order.push(inst);
            for (k, v) in func.results(inst).iter().enumerate() {
                if let Some(node) = func.values.get(v.index()) {
                    value_map[v.index()] = new_values.len() as u32;
                    new_values.push(ValueNode {
                        ty: node.ty,
                        def: ValueDef::Result {
                            inst: Inst::from_u32(ni),
                            index: k as u32,
                        },
                        alias: NONE,
                    });
                }
            }
        }
    }

    // A use of a value that no longer has a definition (invalid IR) keeps a
    // detached slot, so compaction never invents or loses a reference.
    let keep_detached = |v: Value, map: &mut Vec<u32>, values: &mut Vec<ValueNode>| -> Value {
        match map.get(v.index()) {
            Some(&n) if n != NONE => Value::from_u32(n),
            Some(_) => {
                let n = values.len() as u32;
                map[v.index()] = n;
                values.push(ValueNode {
                    ty: func.values[v.index()].ty,
                    def: ValueDef::Detached,
                    alias: NONE,
                });
                Value::from_u32(n)
            }
            None => v,
        }
    };

    let mut new_insts: Vec<InstNode> = Vec::with_capacity(inst_order.len());
    let mut new_blocks: Vec<BlockNode> = Vec::with_capacity(order.len());
    for &b in &order {
        let nb = new_blocks.len() as u32;
        let params: Vec<Value> = func
            .block_params(b)
            .iter()
            .map(|p| Value::from_u32(value_map[p.index()]))
            .collect();
        let mut node = BlockNode::new(params);
        let mut prev = NONE;
        for inst in func.insts(b) {
            let ni = inst_map[inst.index()];
            let old = &func.insts[inst.index()];
            let mut data = old.data.clone();
            data.map_operands(|v| keep_detached(v, &mut value_map, &mut new_values));
            let results = old.results;
            let start = results
                .first()
                .map_or(new_values.len() as u32, |v| value_map[v.index()]);
            if prev == NONE {
                node.first = ni;
            } else if let Some(p) = new_insts.get_mut(prev as usize) {
                p.next = ni;
            }
            new_insts.push(InstNode {
                data,
                results: ValueRange {
                    start,
                    len: results.len,
                },
                prev,
                next: NONE,
                block: nb,
            });
            prev = ni;
        }
        node.last = prev;
        node.term = func.blocks[b.index()].term.clone();
        new_blocks.push(node);
    }

    // Terminators: remap operands and targets. A target that is not a live block
    // (invalid IR) gets a fresh dead block, so the reference stays invalid rather
    // than silently pointing somewhere else.
    let mut dead_blocks = 0u32;
    let live_count = new_blocks.len() as u32;
    for node in &mut new_blocks {
        if let Some(mut term) = node.term.take() {
            term.map_operands(|v| keep_detached(v, &mut value_map, &mut new_values));
            term.for_each_successor_mut(|dest| {
                let old = dest.block.index();
                let mapped = block_map.get(old).copied().unwrap_or(NONE);
                dest.block = if mapped != NONE {
                    Block::from_u32(mapped)
                } else {
                    let fresh = live_count + dead_blocks;
                    dead_blocks += 1;
                    if let Some(slot) = block_map.get_mut(old) {
                        *slot = fresh;
                    }
                    Block::from_u32(fresh)
                };
            });
            node.term = Some(term);
        }
    }
    for _ in 0..dead_blocks {
        let mut node = BlockNode::new(Vec::new());
        node.live = false;
        new_blocks.push(node);
    }
    // Dead targets are not part of the mapping the caller sees.
    for slot in block_map.iter_mut() {
        if *slot != NONE && *slot >= live_count {
            *slot = NONE;
        }
    }
    func.blocks = new_blocks;
    func.insts = new_insts;
    func.values = new_values;
    func.entry = Block::from_u32(0);
    Builder::remap_from_parts(value_map, block_map, inst_map)
}
