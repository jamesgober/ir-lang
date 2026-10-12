//! SSA construction (`declare_var` / `def_var` / `use_var` / `seal_block`): random
//! mutable-variable programs lowered through it agree with an independent
//! evaluator and with the memory-based lowering, under eager and lazy sealing;
//! no trivial block parameter survives; hostile shapes stay iterative; misuse is
//! refused. Also the 2.0.0-alpha.2 rewrite helpers `move_inst` and
//! `remove_block_params`.

mod common;

use common::{VarMode, generate, lower_with};
use ir_lang::{
    BlockArg, BuildError, CmpOp, Function, Linkage, Module, Overflow, Position, Signature, Type,
    ValidationErrorKind, Value,
};
use proptest::prelude::*;

/// Whether any block parameter receives one value (or itself) on every edge:
/// the construction must leave none.
fn trivial_params(func: &Function) -> Vec<Value> {
    let preds = func.predecessors();
    let mut out = Vec::new();
    for b in func.blocks() {
        for (i, &p) in func.block_params(b).iter().enumerate() {
            let mut same: Option<Value> = None;
            let mut trivial = true;
            let mut any = false;
            let mut seen = Vec::new();
            for &pred in &preds[b.index()] {
                if seen.contains(&pred) {
                    continue;
                }
                seen.push(pred);
                for dest in func.terminator(pred).unwrap().successors() {
                    if dest.block != b {
                        continue;
                    }
                    any = true;
                    match dest.args[i] {
                        BlockArg::Value(v) => {
                            let v = func.resolve(v);
                            if v == p || Some(v) == same {
                                continue;
                            }
                            if same.is_some() {
                                trivial = false;
                            }
                            same = Some(v);
                        }
                        _ => trivial = false,
                    }
                }
            }
            if any && trivial && same.is_some() {
                out.push(p);
            }
        }
    }
    out
}

#[cfg(feature = "interp")]
mod differential {
    use super::*;
    use common::{Eval, outcome};
    use ir_lang::interp::{self, Val};

    fn check(seed: u64, a: i64, b: i64, mode: VarMode) -> Result<(), TestCaseError> {
        let prog = generate(seed);
        let low = lower_with(&prog, mode);
        prop_assert!(
            low.module.validate().is_ok(),
            "{:?}\n{}",
            low.module.validate(),
            low.module
        );
        for &f in &low.funcs {
            let func = low.module.function(f).unwrap();
            let t = trivial_params(func);
            prop_assert!(t.is_empty(), "trivial parameters {t:?}\n{}", low.module);
        }
        let expect = Eval::new(&prog).run(a, b);
        let got = interp::run(&low.module, low.funcs[0], &[Val::i64(a), Val::i64(b)]);
        prop_assert!(got.is_ok(), "{got:?}\n{}", low.module);
        prop_assert_eq!(outcome(got.unwrap()), expect, "\n{}", low.module);
        // The constructed IR round-trips like any other.
        let text = low.module.to_string();
        let again = ir_lang::parse(&text).unwrap();
        prop_assert_eq!(again.to_string(), text);
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(384))]

        /// Invariant: SSA construction preserves meaning (against the
        /// independent evaluator) with blocks sealed as soon as possible.
        #[test]
        fn prop_ssa_eager_matches_evaluator(seed in any::<u64>(), a in any::<i64>(), b in any::<i64>()) {
            check(seed, a, b, VarMode::SsaEager)?;
        }

        /// Invariant: the same with every block left unsealed until the end, so
        /// every read in a block makes an incomplete parameter.
        #[test]
        fn prop_ssa_lazy_matches_evaluator(seed in any::<u64>(), a in any::<i64>(), b in any::<i64>()) {
            check(seed, a, b, VarMode::SsaLazy)?;
        }
    }

    #[test]
    fn test_ssa_and_memory_lowerings_agree_on_edge_arguments() {
        let edges = [0i64, 1, -1, i64::MIN, i64::MAX, 3, 64];
        for seed in 0..200u64 {
            let prog = generate(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x55);
            let mem = lower_with(&prog, VarMode::Memory);
            let eager = lower_with(&prog, VarMode::SsaEager);
            let lazy = lower_with(&prog, VarMode::SsaLazy);
            for &a in &edges {
                for &b in &edges[..4] {
                    let args = [Val::i64(a), Val::i64(b)];
                    let want = Eval::new(&prog).run(a, b);
                    for low in [&mem, &eager, &lazy] {
                        let got = interp::run(&low.module, low.funcs[0], &args).unwrap();
                        assert_eq!(outcome(got), want, "seed {seed} {a} {b}\n{}", low.module);
                    }
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Invariant: construction output validates and is deterministic.
    #[test]
    fn prop_ssa_output_validates_and_is_deterministic(seed in any::<u64>()) {
        let prog = generate(seed);
        for mode in [VarMode::SsaEager, VarMode::SsaLazy] {
            let one = lower_with(&prog, mode);
            prop_assert!(one.module.validate().is_ok(), "{:?}\n{}", one.module.validate(), one.module);
            let two = lower_with(&prog, mode);
            prop_assert_eq!(one.module.to_string(), two.module.to_string());
            for &f in &one.funcs {
                let t = trivial_params(one.module.function(f).unwrap());
                prop_assert!(t.is_empty(), "trivial parameters {t:?}");
            }
        }
    }
}

fn module(params: &[Type], returns: &[Type]) -> (Module, ir_lang::FuncId) {
    let mut m = Module::new("ssa");
    let f = m
        .declare_function("f", &Signature::new(params, returns), Linkage::Export)
        .unwrap();
    (m, f)
}

#[test]
fn test_straight_line_reads_add_no_parameters() {
    let (mut m, f) = module(&[Type::I32], &[Type::I32]);
    let mut b = m.build(f).unwrap();
    let x = b.declare_var(Type::I32).unwrap();
    let p = b.param(0).unwrap();
    b.def_var(x, p).unwrap();
    let mut prev = b.func().entry();
    for _ in 0..10 {
        let next = b.create_block(&[]).unwrap();
        b.jump(next, &[]).unwrap();
        b.switch_to(next).unwrap();
        b.seal_block(next).unwrap();
        let v = b.use_var(x).unwrap();
        let w = b.add(v, v, Overflow::Wrap).unwrap();
        b.def_var(x, w).unwrap();
        prev = next;
    }
    let v = b.use_var(x).unwrap();
    b.ret(&[v]).unwrap();
    b.seal_all_blocks().unwrap();
    assert!(
        b.func()
            .blocks()
            .all(|blk| blk == b.func().entry() || b.func().block_params(blk).is_empty())
    );
    let _ = prev;
    m.validate().unwrap();
}

#[test]
fn test_diamond_gets_one_parameter_and_unchanged_variable_none() {
    let (mut m, f) = module(&[Type::I64, Type::Bool], &[Type::I64, Type::I64]);
    let mut b = m.build(f).unwrap();
    let (x, y) = (
        b.declare_var(Type::I64).unwrap(),
        b.declare_var(Type::I64).unwrap(),
    );
    let (p, c) = (b.param(0).unwrap(), b.param(1).unwrap());
    b.def_var(x, p).unwrap();
    b.def_var(y, p).unwrap();
    let (t, e, j) = (
        b.create_block(&[]).unwrap(),
        b.create_block(&[]).unwrap(),
        b.create_block(&[]).unwrap(),
    );
    b.branch(c, t, &[], e, &[]).unwrap();
    for (blk, k) in [(t, 1), (e, 2)] {
        b.switch_to(blk).unwrap();
        b.seal_block(blk).unwrap();
        let k = b.iconst(Type::I64, k).unwrap();
        b.def_var(x, k).unwrap();
        b.jump(j, &[]).unwrap();
    }
    b.switch_to(j).unwrap();
    b.seal_block(j).unwrap();
    let (xv, yv) = (b.use_var(x).unwrap(), b.use_var(y).unwrap());
    b.ret(&[xv, yv]).unwrap();
    b.seal_all_blocks().unwrap();
    // `x` differs on the two paths; `y` does not, so it needs no parameter.
    assert_eq!(b.func().block_params(j).len(), 1);
    assert_eq!(yv, p);
    m.validate().unwrap();
}

#[test]
fn test_unassigned_variable_reads_zero_of_its_type() {
    for ty in [
        Type::I8,
        Type::U64,
        Type::F32,
        Type::F64,
        Type::Bool,
        Type::Ptr,
        Type::Ref,
    ] {
        let (mut m, f) = module(&[], &[ty]);
        let mut b = m.build(f).unwrap();
        let x = b.declare_var(ty).unwrap();
        let v = b.use_var(x).unwrap();
        // The same zero constant is reused.
        assert_eq!(b.use_var(x).unwrap(), v);
        b.ret(&[v]).unwrap();
        m.validate().unwrap();
        assert!(m.to_string().contains("= const"), "{m}");
    }
}

#[test]
fn test_loop_with_lazy_seal_keeps_only_real_phis() {
    // i = 0; x = 7; while i < n { i = i + 1 }  -> x needs no parameter.
    let (mut m, f) = module(&[Type::I32], &[Type::I32, Type::I32]);
    let mut b = m.build(f).unwrap();
    let (i, x) = (
        b.declare_var(Type::I32).unwrap(),
        b.declare_var(Type::I32).unwrap(),
    );
    let n = b.param(0).unwrap();
    let zero = b.iconst(Type::I32, 0).unwrap();
    let seven = b.iconst(Type::I32, 7).unwrap();
    b.def_var(i, zero).unwrap();
    b.def_var(x, seven).unwrap();
    let (head, body, exit) = (
        b.create_block(&[]).unwrap(),
        b.create_block(&[]).unwrap(),
        b.create_block(&[]).unwrap(),
    );
    b.jump(head, &[]).unwrap();
    b.switch_to(head).unwrap();
    let iv = b.use_var(i).unwrap();
    let xv = b.use_var(x).unwrap(); // incomplete parameter, trivial in the end
    let more = b.compare(CmpOp::Lt, iv, n).unwrap();
    b.branch(more, body, &[], exit, &[]).unwrap();
    b.switch_to(body).unwrap();
    let iv = b.use_var(i).unwrap();
    let one = b.iconst(Type::I32, 1).unwrap();
    let i2 = b.add(iv, one, Overflow::Wrap).unwrap();
    b.def_var(i, i2).unwrap();
    b.jump(head, &[]).unwrap();
    b.switch_to(exit).unwrap();
    let (ie, xe) = (b.use_var(i).unwrap(), b.use_var(x).unwrap());
    b.ret(&[ie, xe]).unwrap();
    b.seal_all_blocks().unwrap();
    assert_eq!(b.func().block_params(head).len(), 1);
    assert_eq!(b.func().resolve(xv), seven);
    m.validate().unwrap();
}

#[test]
fn test_deep_shapes_are_iterative() {
    // A 60,000-block chain read at the bottom, and 400 nested loops sealed lazily.
    let (mut m, f) = module(&[Type::I64], &[Type::I64]);
    let mut b = m.build(f).unwrap();
    let x = b.declare_var(Type::I64).unwrap();
    let p = b.param(0).unwrap();
    b.def_var(x, p).unwrap();
    for _ in 0..60_000 {
        let next = b.create_block(&[]).unwrap();
        b.jump(next, &[]).unwrap();
        b.switch_to(next).unwrap();
        b.seal_block(next).unwrap();
    }
    let v = b.use_var(x).unwrap();
    assert_eq!(v, p);
    let mut exits = Vec::new();
    for _ in 0..400 {
        let head = b.create_block(&[]).unwrap();
        let exit = b.create_block(&[]).unwrap();
        b.jump(head, &[]).unwrap();
        b.switch_to(head).unwrap();
        let c = b.bconst(false).unwrap();
        let body = b.create_block(&[]).unwrap();
        b.branch(c, body, &[], exit, &[]).unwrap();
        b.switch_to(body).unwrap();
        exits.push((head, exit));
    }
    let v = b.use_var(x).unwrap();
    let one = b.iconst(Type::I64, 1).unwrap();
    let w = b.add(v, one, Overflow::Wrap).unwrap();
    b.def_var(x, w).unwrap();
    while let Some((head, exit)) = exits.pop() {
        b.jump(head, &[]).unwrap();
        b.switch_to(exit).unwrap();
    }
    let v = b.use_var(x).unwrap();
    b.ret(&[v]).unwrap();
    b.seal_all_blocks().unwrap();
    m.validate().unwrap();
}

#[test]
fn test_misuse_is_refused() {
    let (mut m, f) = module(&[Type::I32], &[]);
    let mut b = m.build(f).unwrap();
    let x = b.declare_var(Type::I64).unwrap();
    let p = b.param(0).unwrap();
    // Type mismatch.
    assert!(matches!(
        b.def_var(x, p),
        Err(BuildError::Invalid(
            ValidationErrorKind::TypeMismatch { .. }
        ))
    ));
    // Unknown variable.
    let bogus = ir_lang::Variable::from_u32(9);
    assert_eq!(
        b.use_var(bogus),
        Err(BuildError::UnknownVariable { var: bogus })
    );
    // An edge into a sealed block.
    let next = b.create_block(&[]).unwrap();
    b.seal_block(next).unwrap();
    assert_eq!(
        b.jump(next, &[]),
        Err(BuildError::SealedBlock { block: next })
    );
    // Splitting is refused while blocks are unsealed.
    let c = b.iconst(Type::I32, 1).unwrap();
    let ci = b.func().defining_inst(c).unwrap();
    let unsealed = b.create_block(&[]).unwrap();
    assert_eq!(b.split_block(ci), Err(BuildError::VariablesActive));
    b.seal_block(unsealed).unwrap();
    // A definition after the block's terminator.
    let other = b.create_block(&[]).unwrap();
    b.jump(other, &[]).unwrap();
    let z = b.iconst(Type::I64, 0);
    assert!(z.is_err() || b.def_var(x, z.unwrap()).is_err());
    // Compaction drops the variables.
    b.switch_to(other).unwrap();
    b.seal_block(other).unwrap();
    b.ret(&[]).unwrap();
    b.seal_all_blocks().unwrap();
    let _ = b.compact();
    assert_eq!(b.use_var(x), Err(BuildError::UnknownVariable { var: x }));
}

#[test]
fn test_construction_can_extend_an_existing_body() {
    // An existing function `f(x) = x`, reopened: its return is replaced by new
    // code lowered through a variable.
    let (mut m, f) = module(&[Type::I32, Type::Bool], &[Type::I32]);
    {
        let mut b = m.build(f).unwrap();
        let x = b.param(0).unwrap();
        b.ret(&[x]).unwrap();
    }
    let mut b = m.edit(f).unwrap();
    let entry = b.func().entry();
    let _ = b.remove_terminator(entry).unwrap();
    let v = b.declare_var(Type::I32).unwrap();
    let (x, c) = (b.param(0).unwrap(), b.param(1).unwrap());
    b.def_var(v, x).unwrap();
    let (t, j) = (b.create_block(&[]).unwrap(), b.create_block(&[]).unwrap());
    b.branch(c, t, &[], j, &[]).unwrap();
    b.switch_to(t).unwrap();
    let two = b.iconst(Type::I32, 2).unwrap();
    let y = b.mul(x, two, Overflow::Wrap).unwrap();
    b.def_var(v, y).unwrap();
    b.jump(j, &[]).unwrap();
    b.seal_all_blocks().unwrap();
    b.switch_to(j).unwrap();
    let r = b.use_var(v).unwrap();
    b.ret(&[r]).unwrap();
    assert_eq!(b.func().block_params(j), &[r]);
    m.validate().unwrap();
}

#[test]
fn test_split_after_construction_ends_it() {
    let (mut m, f) = module(&[Type::I32], &[Type::I32]);
    let mut b = m.build(f).unwrap();
    let x = b.declare_var(Type::I32).unwrap();
    let p = b.param(0).unwrap();
    b.def_var(x, p).unwrap();
    let v = b.use_var(x).unwrap();
    let w = b.add(v, v, Overflow::Wrap).unwrap();
    b.ret(&[w]).unwrap();
    b.seal_all_blocks().unwrap();
    let wi = b.func().defining_inst(w).unwrap();
    let tail = b.split_block(wi).unwrap();
    assert_eq!(b.func().inst_block(wi), Some(tail));
    assert_eq!(b.use_var(x), Err(BuildError::UnknownVariable { var: x }));
    m.validate().unwrap();
}

#[test]
fn test_parameters_cannot_follow_construction_parameters() {
    let (mut m, f) = module(&[Type::Bool], &[Type::I32]);
    let mut b = m.build(f).unwrap();
    let x = b.declare_var(Type::I32).unwrap();
    let c = b.param(0).unwrap();
    let j = b.create_block(&[Type::U8]).unwrap();
    let (t, e) = (b.create_block(&[]).unwrap(), b.create_block(&[]).unwrap());
    b.branch(c, t, &[], e, &[]).unwrap();
    for (blk, k) in [(t, 1), (e, 2)] {
        b.switch_to(blk).unwrap();
        b.seal_block(blk).unwrap();
        let v = b.iconst(Type::I32, k).unwrap();
        b.def_var(x, v).unwrap();
        let u = b.iconst(Type::U8, k).unwrap();
        // Only the block's own argument: the builder appends `x`'s value.
        b.jump(j, &[u]).unwrap();
    }
    b.switch_to(j).unwrap();
    b.seal_block(j).unwrap();
    let v = b.use_var(x).unwrap();
    assert_eq!(b.func().block_params(j).len(), 2);
    assert_eq!(
        b.add_block_param(j, Type::I8),
        Err(BuildError::VariablesActive)
    );
    b.ret(&[v]).unwrap();
    b.seal_all_blocks().unwrap();
    m.validate().unwrap();
}

#[test]
fn test_move_inst_reorders_and_validates() {
    let (mut m, f) = module(&[Type::I32], &[Type::I32]);
    let mut b = m.build(f).unwrap();
    let x = b.param(0).unwrap();
    let late = b.create_block(&[]).unwrap();
    let k = b.iconst(Type::I32, 3).unwrap();
    b.jump(late, &[]).unwrap();
    b.switch_to(late).unwrap();
    let y = b.add(x, k, Overflow::Wrap).unwrap();
    b.ret(&[y]).unwrap();
    // Sink the constant into the block that uses it, then hoist it back.
    let ki = b.func().defining_inst(k).unwrap();
    let yi = b.func().defining_inst(y).unwrap();
    b.move_inst(ki, Position::Before(yi)).unwrap();
    assert_eq!(b.func().inst_block(ki), Some(late));
    b.validate().unwrap();
    let entry = b.func().entry();
    b.move_inst(ki, Position::End(entry)).unwrap();
    assert_eq!(b.func().inst_block(ki), Some(entry));
    b.validate().unwrap();
    // Moving after itself is a no-op; a stale target is refused.
    b.move_inst(ki, Position::After(ki)).unwrap();
    b.remove_inst(yi).unwrap();
    assert_eq!(
        b.move_inst(ki, Position::After(yi)),
        Err(BuildError::UnknownInst { inst: yi })
    );
    assert_eq!(
        b.move_inst(yi, Position::End(entry)),
        Err(BuildError::UnknownInst { inst: yi })
    );
}

#[test]
fn test_remove_block_params_in_bulk_is_linear_and_exact() {
    // 2,000 blocks of 4 parameters each; drop two of every four in one call.
    let (mut m, f) = module(&[Type::I32], &[]);
    let mut b = m.build(f).unwrap();
    let x = b.param(0).unwrap();
    let mut doomed = Vec::new();
    let mut keep = Vec::new();
    for _ in 0..2_000 {
        let next = b
            .create_block(&[Type::I32, Type::I32, Type::I32, Type::I32])
            .unwrap();
        b.jump(next, &[x, x, x, x]).unwrap();
        b.switch_to(next).unwrap();
        let ps = b.func().block_params(next).to_vec();
        doomed.extend([ps[0], ps[2]]);
        keep.push((next, [ps[1], ps[3]]));
    }
    b.ret(&[]).unwrap();
    b.remove_block_params(&doomed).unwrap();
    for (blk, ps) in keep {
        assert_eq!(b.func().block_params(blk), &ps);
    }
    b.validate().unwrap();
    // Not a parameter: refused, nothing changed.
    let c = b.iconst(Type::I32, 1).unwrap();
    assert_eq!(
        b.remove_block_params(&[c]),
        Err(BuildError::NotBlockParam { value: c })
    );
    assert_eq!(
        b.remove_block_params(&[doomed[0]]),
        Err(BuildError::NotBlockParam { value: doomed[0] })
    );
}

#[test]
fn test_switch_edges_into_one_join_all_get_arguments() {
    // `switch` with 2,000 cases: odd cases go straight to the join, even ones
    // through a block that reassigns `x`. The join's parameter must be passed on
    // every edge, including the many from the switch block itself.
    let (mut m, f) = module(&[Type::U32], &[Type::I32]);
    let mut b = m.build(f).unwrap();
    let x = b.declare_var(Type::I32).unwrap();
    let s = b.param(0).unwrap();
    let zero = b.iconst(Type::I32, 0).unwrap();
    b.def_var(x, zero).unwrap();
    let (other, join) = (b.create_block(&[]).unwrap(), b.create_block(&[]).unwrap());
    let cases: Vec<ir_lang::SwitchCase> = (0..2_000u64)
        .map(|k| ir_lang::SwitchCase {
            value: k,
            dest: ir_lang::BlockCall::new(if k % 2 == 0 { other } else { join }, &[]),
        })
        .collect();
    b.switch(s, cases, ir_lang::BlockCall::new(join, &[]))
        .unwrap();
    b.switch_to(other).unwrap();
    b.seal_block(other).unwrap();
    let one = b.iconst(Type::I32, 1).unwrap();
    b.def_var(x, one).unwrap();
    b.jump(join, &[]).unwrap();
    b.switch_to(join).unwrap();
    b.seal_block(join).unwrap();
    let v = b.use_var(x).unwrap();
    b.ret(&[v]).unwrap();
    b.seal_all_blocks().unwrap();
    assert_eq!(b.func().block_params(join), &[v]);
    m.validate().unwrap();
}
