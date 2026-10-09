//! The mutation API keeps IR valid and meaning-preserving under random rewrite
//! sequences: every step is followed by validation, and the rewritten program
//! must still compute exactly what the independent evaluator computes.

mod common;

use common::{Eval, Rng, generate, lower, outcome};
use ir_lang::interp::{self, Val};
use ir_lang::{BinaryOp, Builder, Inst, InstData, Position, Value};
use proptest::prelude::*;

/// Every live instruction of the function, in layout order.
fn live_insts(b: &Builder<'_>) -> Vec<Inst> {
    let f = b.func();
    f.blocks()
        .flat_map(|blk| f.insts(blk).collect::<Vec<_>>())
        .collect()
}

/// How many times each (resolved) value is used.
fn use_counts(b: &Builder<'_>) -> Vec<u32> {
    let f = b.func();
    let mut uses = vec![0u32; f.value_count()];
    for blk in f.blocks() {
        for i in f.insts(blk) {
            if let Some(d) = f.inst(i) {
                d.for_each_operand(|v| uses[f.resolve(v).index()] += 1);
            }
        }
        if let Some(t) = f.terminator(blk) {
            t.for_each_operand(|v| uses[f.resolve(v).index()] += 1);
        }
    }
    uses
}

fn is_pure_single(d: &InstData) -> bool {
    !d.has_side_effects() && !matches!(d, InstData::Call(_))
}

/// One random meaning-preserving rewrite. Returns a name for the failure report.
fn rewrite(b: &mut Builder<'_>, rng: &mut Rng) -> &'static str {
    let insts = live_insts(b);
    if insts.is_empty() {
        return "none";
    }
    let pick = insts[rng.below(insts.len() as u64) as usize];
    match rng.below(7) {
        0 => {
            b.split_block(pick).unwrap();
            "split_block"
        }
        1 => {
            // Recompute a pure value right after itself and forward every use.
            let data = b.func().inst(pick).cloned().unwrap();
            let results = b.func().results(pick);
            if !is_pure_single(&data) || results.len() != 1 {
                return "skip";
            }
            b.set_position(Position::After(pick)).unwrap();
            let dup = b.insert(data).unwrap();
            let old = results.first().unwrap();
            let new = b.results(dup).first().unwrap();
            b.replace_all_uses(old, new).unwrap();
            "duplicate+replace_all_uses"
        }
        2 => {
            // A dead constant anywhere.
            b.set_position(Position::Before(pick)).unwrap();
            let _ = b.iconst(ir_lang::Type::I16, -5).unwrap();
            "insert_dead"
        }
        3 => {
            // Remove one unused pure instruction.
            let uses = use_counts(b);
            let dead = insts.iter().copied().find(|&i| {
                let d = b.func().inst(i).unwrap();
                is_pure_single(d) && b.func().results(i).iter().all(|v| uses[v.index()] == 0)
            });
            if let Some(i) = dead {
                b.remove_inst(i).unwrap();
            }
            "remove_dead"
        }
        4 => {
            // Swap the operands of a commutative operation.
            let data = b.func().inst(pick).cloned().unwrap();
            if let InstData::Binary { op, policy, args } = data {
                let commutes = matches!(
                    op,
                    BinaryOp::And | BinaryOp::Or | BinaryOp::Xor | BinaryOp::Min | BinaryOp::Max
                ) || (matches!(op, BinaryOp::Add | BinaryOp::Mul)
                    && !policy.can_error());
                if commutes {
                    let swapped = InstData::Binary {
                        op,
                        policy,
                        args: [args[1], args[0]],
                    };
                    b.replace_inst(pick, swapped).unwrap();
                }
            }
            "swap_commutative"
        }
        5 => {
            b.resolve_aliases();
            "resolve_aliases"
        }
        _ => {
            let _ = b.compact();
            "compact"
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn prop_rewrites_keep_ir_valid_and_meaning(seed in any::<u64>(), rseed in any::<u64>(), steps in 1usize..24, a in any::<i64>(), b in any::<i64>()) {
        let prog = generate(seed);
        let mut low = lower(&prog);
        let expect = Eval::new(&prog).run(a, b);
        let mut rng = Rng::new(rseed);
        let funcs = low.funcs.clone();
        for _ in 0..steps {
            let f = funcs[rng.below(funcs.len() as u64) as usize];
            let mut bld = low.module.edit(f).unwrap();
            let what = rewrite(&mut bld, &mut rng);
            prop_assert!(bld.validate().is_ok(), "after {what}: {:?}", bld.validate());
        }
        prop_assert!(low.module.validate().is_ok());
        let got = interp::run(&low.module, low.funcs[0], &[Val::i64(a), Val::i64(b)]).unwrap();
        prop_assert_eq!(outcome(got), expect.clone());

        // The rewritten module still round-trips through text.
        let text = low.module.to_string();
        let again = ir_lang::parse(&text).unwrap();
        prop_assert_eq!(again.to_string(), text);
        let got = interp::run(&again, low.funcs[0], &[Val::i64(a), Val::i64(b)]).unwrap();
        prop_assert_eq!(outcome(got), expect);
    }

    /// Hostile rewrites (forwarding a value to an arbitrary same-typed value,
    /// removing arbitrary instructions or blocks) may break the IR; validation must
    /// say so without panicking, and anything it accepts must run without panicking.
    #[test]
    fn prop_hostile_rewrites_never_panic(seed in any::<u64>(), rseed in any::<u64>(), steps in 1usize..8) {
        let mut low = lower(&generate(seed));
        let mut rng = Rng::new(rseed);
        let funcs = low.funcs.clone();
        for _ in 0..steps {
            let f = funcs[rng.below(funcs.len() as u64) as usize];
            let mut bld = low.module.edit(f).unwrap();
            let n = bld.func().value_count() as u64;
            match rng.below(4) {
                0 => {
                    let old = Value::from_u32(rng.below(n) as u32);
                    let new = Value::from_u32(rng.below(n) as u32);
                    let _ = bld.replace_all_uses(old, new);
                }
                1 => {
                    let insts = live_insts(&bld);
                    if !insts.is_empty() {
                        let i = insts[rng.below(insts.len() as u64) as usize];
                        bld.remove_inst(i).unwrap();
                    }
                }
                2 => {
                    let blocks: Vec<_> = bld.func().blocks().collect();
                    let blk = blocks[rng.below(blocks.len() as u64) as usize];
                    let _ = bld.remove_block(blk);
                }
                _ => {
                    let blocks: Vec<_> = bld.func().blocks().collect();
                    let blk = blocks[rng.below(blocks.len() as u64) as usize];
                    let _ = bld.add_block_param(blk, ir_lang::Type::I64);
                }
            }
            let _ = bld.validate();
        }
        if low.module.validate().is_ok() {
            let mut vm = interp::Interpreter::with_limits(
                &low.module,
                interp::NoHost,
                interp::Limits { fuel: 200_000, ..interp::Limits::default() },
            ).unwrap();
            let _ = vm.call(low.funcs[0], &[Val::i64(1), Val::i64(2)]);
        }
        let text = low.module.to_string();
        if let Ok(m) = ir_lang::parse(&text) {
            let _ = m.validate();
        }
    }
}
