//! The validator's dominance and safepoint-liveness algorithms against brute-force
//! references on random control-flow graphs (written in the textual form, so any
//! shape is reachable, including unreachable blocks, loops, and self-loops).

mod common;

use common::Rng;
use ir_lang::ValidationErrorKind as K;
use proptest::prelude::*;

/// A random CFG over `n` blocks: per block, a terminator choice and targets.
struct Cfg {
    succ: Vec<Vec<usize>>,
}

fn random_cfg(rng: &mut Rng, n: usize) -> Cfg {
    let mut succ = Vec::with_capacity(n);
    for _ in 0..n {
        let s = match rng.below(4) {
            0 => vec![],                                     // ret
            1 => vec![1 + rng.below(n as u64 - 1) as usize], // jump (never to b0)
            _ => vec![
                1 + rng.below(n as u64 - 1) as usize,
                1 + rng.below(n as u64 - 1) as usize,
            ], // br
        };
        succ.push(s);
    }
    Cfg { succ }
}

fn terminator(succ: &[usize]) -> String {
    match succ {
        [] => "ret".into(),
        [t] => format!("jump b{t}"),
        [t, e, ..] => format!("br v0, b{t}, b{e}"),
    }
}

/// Blocks reachable from the entry when `removed` is deleted.
fn reachable(cfg: &Cfg, removed: Option<usize>) -> Vec<bool> {
    let n = cfg.succ.len();
    let mut seen = vec![false; n];
    if removed == Some(0) {
        return seen;
    }
    let mut stack = vec![0];
    seen[0] = true;
    while let Some(b) = stack.pop() {
        for &s in &cfg.succ[b] {
            if Some(s) != removed && !seen[s] {
                seen[s] = true;
                stack.push(s);
            }
        }
    }
    seen
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Every block defines one constant and uses up to two values of other
    /// blocks; the validator must accept exactly when every use in a reachable
    /// block is dominated by its definition.
    #[test]
    fn prop_dominance_matches_brute_force(seed in any::<u64>(), n in 2usize..9) {
        let mut rng = Rng::new(seed);
        let cfg = random_cfg(&mut rng, n);
        // Value of block b's constant: v(1 + b). v0 is the bool parameter.
        let mut text = String::from("module \"t\" ptr64\nsig s0 = (bool) -> () fast\n\nfunc @f s0 export {\n");
        let mut uses: Vec<(usize, usize, bool)> = Vec::new(); // (use block, def block, def before use in same block)
        let mut next = 1 + n;
        for b in 0..n {
            if b == 0 {
                text.push_str("b0(v0: bool):
");
            } else {
                text.push_str(&format!("b{b}:
"));
            }
            let use_first = rng.chance(30);
            let other = rng.below(n as u64) as usize;
            if use_first {
                // A use of `other`'s constant before this block's own definition.
                text.push_str(&format!("    v{next}: i32 = add<overflow=wrap> v{}, v{}\n", 1 + other, 1 + other));
                next += 1;
                uses.push((b, other, false));
            }
            text.push_str(&format!("    v{}: i32 = const {b}\n", 1 + b));
            let other = rng.below(n as u64) as usize;
            text.push_str(&format!("    v{next}: i32 = add<overflow=wrap> v{}, v{}\n", 1 + other, 1 + other));
            next += 1;
            uses.push((b, other, true));
            text.push_str(&format!("    {}\n", terminator(&cfg.succ[b])));
        }
        text.push_str("}\n");

        let live = reachable(&cfg, None);
        let valid = uses.iter().all(|&(ub, db, after_def)| {
            if !live[ub] {
                return true;
            }
            if ub == db {
                return after_def;
            }
            // `db` dominates `ub` iff `ub` is unreachable once `db` is removed.
            !reachable(&cfg, Some(db))[ub]
        });
        let m = ir_lang::parse(&text).unwrap();
        let got = m.validate();
        match &got {
            Ok(()) => prop_assert!(valid, "accepted invalid IR:\n{text}"),
            Err(e) => {
                prop_assert!(!valid, "rejected valid IR: {e}\n{text}");
                prop_assert!(matches!(e.kind(), K::UseBeforeDef { .. }), "{e}");
            }
        }
    }

    /// Two `ref` parameters, a `ref` block parameter in every other block (fed by
    /// edge arguments), random safepoints, and random uses: the validator must
    /// report a reference live across a safepoint exactly when, from a reachable
    /// safepoint, some use of a reference defined before it is reachable without
    /// passing the reference's definition. Edge arguments are uses at the end of
    /// the predecessor.
    #[test]
    fn prop_safepoint_liveness_matches_brute_force(seed in any::<u64>(), n in 2usize..8) {
        let mut rng = Rng::new(seed);
        let cfg = random_cfg(&mut rng, n);
        // Reference names: 0 = v1, 1 = v2 (entry parameters), 2 = this block's parameter.
        #[derive(Clone, Copy, PartialEq)]
        enum St { Safepoint, Use(usize) }
        let param = |b: usize| 3 + b; // value id of block b's ref parameter (b >= 1)
        let mut stmts: Vec<Vec<St>> = Vec::new();
        let mut edge_arg: Vec<Vec<usize>> = Vec::new(); // per block, per successor: 0 or 1
        let mut text = String::from("module \"t\" ptr64
sig s0 = (bool, ref, ref) -> () fast

func @f s0 export {
");
        let mut next = 3 + n;
        for b in 0..n {
            if b == 0 {
                text.push_str("b0(v0: bool, v1: ref, v2: ref):
");
            } else {
                text.push_str(&format!("b{b}(v{}: ref):
", param(b)));
            }
            let mut list = Vec::new();
            for _ in 0..rng.below(4) {
                let st = match rng.below(6) {
                    0 | 1 => St::Safepoint,
                    2 => St::Use(0),
                    3 => St::Use(1),
                    _ if b > 0 => St::Use(2),
                    _ => St::Use(0),
                };
                match st {
                    St::Safepoint => text.push_str("    safepoint gc()
"),
                    St::Use(r) => {
                        let v = if r == 2 { param(b) } else { 1 + r };
                        text.push_str(&format!("    v{next}: bool = eq v{v}, v{v}
"));
                        next += 1;
                    }
                }
                list.push(st);
            }
            stmts.push(list);
            let args: Vec<usize> = cfg.succ[b].iter().map(|_| rng.below(2) as usize).collect();
            let term = match (cfg.succ[b].as_slice(), args.as_slice()) {
                ([], _) => "ret".to_string(),
                ([t], [a]) => format!("jump b{t}(v{})", 1 + a),
                ([t, e], [a, c]) => format!("br v0, b{t}(v{}), b{e}(v{})", 1 + a, 1 + c),
                _ => unreachable!(),
            };
            text.push_str(&format!("    {term}
"));
            edge_arg.push(args);
        }
        text.push_str("}
");

        let live = reachable(&cfg, None);
        let after = |b: usize| -> Vec<bool> {
            let mut seen = vec![false; n];
            let mut stack: Vec<usize> = cfg.succ[b].clone();
            while let Some(x) = stack.pop() {
                if !seen[x] {
                    seen[x] = true;
                    stack.extend(cfg.succ[x].iter().copied());
                }
            }
            seen
        };
        // Uses of entry parameter r anywhere in block x (statements or edge args).
        let uses_entry_ref = |x: usize, r: usize| {
            stmts[x].contains(&St::Use(r)) || edge_arg[x].contains(&r)
        };
        let mut expect_error = false;
        for b in 0..n {
            if !live[b] {
                continue;
            }
            for (i, s) in stmts[b].iter().enumerate() {
                if *s != St::Safepoint {
                    continue;
                }
                let rest = &stmts[b][i + 1..];
                // The block's own parameter: only used inside its block.
                if rest.contains(&St::Use(2)) {
                    expect_error = true;
                }
                for r in 0..2 {
                    let later_here = rest.contains(&St::Use(r)) || edge_arg[b].contains(&r);
                    let reach = after(b);
                    let later_elsewhere = (0..n).any(|x| reach[x] && uses_entry_ref(x, r));
                    if later_here || later_elsewhere {
                        expect_error = true;
                    }
                }
            }
        }
        let m = ir_lang::parse(&text).unwrap();
        match m.validate() {
            Ok(()) => prop_assert!(!expect_error, "missed a live reference:
{text}"),
            Err(e) => {
                prop_assert!(expect_error, "false positive: {e}
{text}");
                prop_assert!(matches!(e.kind(), K::LiveAcrossSafepoint { .. }), "{e}");
            }
        }
    }
}
