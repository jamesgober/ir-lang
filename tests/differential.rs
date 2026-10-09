//! Differential tests: random programs (loops, calls, invokes, tail calls,
//! memory, globals, checked arithmetic, exceptions, traps) are run by an
//! independent evaluator and, lowered to IR, by the reference interpreter, before
//! and after a print/parse round trip. All three must agree exactly.

mod common;

use common::{Eval, generate, lower, outcome};
use ir_lang::interp::{self, Val};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn prop_interpreter_matches_independent_evaluator(seed in any::<u64>(), a in any::<i64>(), b in any::<i64>()) {
        let prog = generate(seed);
        let low = lower(&prog);
        prop_assert!(low.module.validate().is_ok(), "{:?}\n{}", low.module.validate(), low.module);
        let expect = Eval::new(&prog).run(a, b);
        let got = interp::run(&low.module, low.funcs[0], &[Val::i64(a), Val::i64(b)]);
        prop_assert!(got.is_ok(), "{got:?}\n{}", low.module);
        prop_assert_eq!(outcome(got.unwrap()), expect.clone(), "\n{}", low.module);

        let text = low.module.to_string();
        let again = ir_lang::parse(&text).unwrap();
        prop_assert!(again.validate().is_ok());
        prop_assert_eq!(again.to_string(), text);
        let got = interp::run(&again, low.funcs[0], &[Val::i64(a), Val::i64(b)]).unwrap();
        prop_assert_eq!(outcome(got), expect);
    }
}

#[test]
fn test_edge_arguments_agree_on_many_programs() {
    // Deterministic sweep with boundary arguments, independent of proptest's
    // random choice of arguments.
    let edges = [0i64, 1, -1, i64::MIN, i64::MAX, 7, -8];
    for seed in 0..300u64 {
        let prog = generate(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let low = lower(&prog);
        low.module.validate().unwrap();
        for &a in &edges {
            for &b in &edges[..3] {
                let expect = Eval::new(&prog).run(a, b);
                let got =
                    interp::run(&low.module, low.funcs[0], &[Val::i64(a), Val::i64(b)]).unwrap();
                assert_eq!(
                    outcome(got),
                    expect,
                    "seed {seed} args {a} {b}\n{}",
                    low.module
                );
            }
        }
    }
}
