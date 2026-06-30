//! Property tests for the core invariants: a function lowered from any
//! well-typed expression validates, lowering is deterministic, and value handles
//! stay dense.
//!
//! The `Expr` type here stands in for a front-end's syntax tree; `lower` is the
//! recursive walk that drives the [`Builder`], exactly as a real lowering would. The
//! generator produces arbitrary trees, so the properties hold over a wide space of
//! shapes, including deeply nested control flow.

use ir_lang::{BinOp, Builder, Function, Type, Value};
use proptest::prelude::*;

/// A tiny integer-expression tree to lower. Every `Expr` has type `int`.
#[derive(Clone, Debug)]
enum Expr {
    /// The function's single integer parameter.
    Param,
    /// An integer literal.
    Lit(i64),
    /// An arithmetic combination of two integer sub-expressions.
    Bin(ArithOp, Box<Expr>, Box<Expr>),
    /// `if lhs < rhs { then } else { els }` — all four sub-expressions are `int`.
    IfLt(Box<Expr>, Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Clone, Copy, Debug)]
enum ArithOp {
    Add,
    Sub,
    Mul,
}

impl ArithOp {
    fn to_binop(self) -> BinOp {
        match self {
            ArithOp::Add => BinOp::Add,
            ArithOp::Sub => BinOp::Sub,
            ArithOp::Mul => BinOp::Mul,
        }
    }
}

/// Recursively lowers `expr` into the current block, returning the value it produces.
/// This is the AST-to-IR lowering a consumer writes against the builder.
fn lower(b: &mut Builder, expr: &Expr, param: Value) -> Value {
    match expr {
        Expr::Param => param,
        Expr::Lit(n) => b.iconst(*n),
        Expr::Bin(op, lhs, rhs) => {
            let l = lower(b, lhs, param);
            let r = lower(b, rhs, param);
            b.bin(op.to_binop(), l, r)
        }
        Expr::IfLt(lhs, rhs, then_e, else_e) => {
            let l = lower(b, lhs, param);
            let r = lower(b, rhs, param);
            let cond = b.bin(BinOp::Lt, l, r);

            let join = b.create_block(&[Type::Int]);
            let then_blk = b.create_block(&[]);
            let else_blk = b.create_block(&[]);
            b.branch(cond, then_blk, &[], else_blk, &[]);

            b.switch_to(then_blk);
            let tv = lower(b, then_e, param);
            b.jump(join, &[tv]);

            b.switch_to(else_blk);
            let ev = lower(b, else_e, param);
            b.jump(join, &[ev]);

            b.switch_to(join);
            b.block_params(join)[0]
        }
    }
}

/// Builds a complete `fn(int) -> int` from an expression.
fn build(expr: &Expr) -> Function {
    let mut b = Builder::new("test", &[Type::Int], Type::Int);
    let param = b.block_params(b.entry())[0];
    let result = lower(&mut b, expr, param);
    b.ret(Some(result));
    b.finish()
}

fn arb_arith() -> impl Strategy<Value = ArithOp> {
    prop_oneof![Just(ArithOp::Add), Just(ArithOp::Sub), Just(ArithOp::Mul)]
}

fn arb_expr() -> impl Strategy<Value = Expr> {
    let leaf = prop_oneof![
        Just(Expr::Param),
        any::<i32>().prop_map(|n| Expr::Lit(i64::from(n))),
    ];
    // Up to 4 levels of nesting, ~64 nodes total, branching factor 4.
    leaf.prop_recursive(4, 64, 4, |inner| {
        prop_oneof![
            (arb_arith(), inner.clone(), inner.clone()).prop_map(|(op, l, r)| Expr::Bin(
                op,
                Box::new(l),
                Box::new(r)
            )),
            (inner.clone(), inner.clone(), inner.clone(), inner.clone()).prop_map(
                |(a, b, c, d)| Expr::IfLt(Box::new(a), Box::new(b), Box::new(c), Box::new(d))
            ),
        ]
    })
}

proptest! {
    /// A function lowered from any well-typed expression is well-formed.
    #[test]
    fn prop_lowered_expression_validates(expr in arb_expr()) {
        let func = build(&expr);
        prop_assert_eq!(func.validate(), Ok(()));
    }

    /// Lowering the same expression twice produces identical functions.
    #[test]
    fn prop_lowering_is_deterministic(expr in arb_expr()) {
        prop_assert_eq!(build(&expr), build(&expr));
    }

    /// Every value handle is dense: indices run `0..value_count` with no gaps, and
    /// no block references a value outside that range.
    #[test]
    fn prop_value_handles_are_dense(expr in arb_expr()) {
        let func = build(&expr);
        let count = func.value_count();

        let mut seen = vec![false; count];
        for block in func.blocks() {
            for &v in func.block_params(block) {
                prop_assert!(v.index() < count);
                seen[v.index()] = true;
            }
            for &v in func.insts(block) {
                prop_assert!(v.index() < count);
                seen[v.index()] = true;
            }
        }
        // No gaps: every index in range is the handle of some value.
        prop_assert!(seen.iter().all(|&s| s));
    }

    /// Every block ends in exactly one terminator and every branch target exists.
    #[test]
    fn prop_blocks_are_terminated_with_valid_targets(expr in arb_expr()) {
        let func = build(&expr);
        let block_count = func.block_count();
        for block in func.blocks() {
            let term = func.terminator(block);
            prop_assert!(term.is_some());
            if let Some(term) = term {
                let mut targets = Vec::new();
                term.each_successor(|target| targets.push(target));
                for target in targets {
                    prop_assert!(target.index() < block_count);
                }
            }
        }
    }
}
