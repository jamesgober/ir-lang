//! Integration tests that drive the public API the way a front-end would: build a
//! function with the [`Builder`], validate it, and read it back.

use ir_lang::{BinOp, Builder, Inst, Terminator, Type, UnOp};

/// `fn max(a: int, b: int) -> int { if a < b { b } else { a } }`
#[test]
fn test_lower_if_expression_to_diamond() {
    let mut b = Builder::new("max", &[Type::Int, Type::Int], Type::Int);
    let a = b.block_params(b.entry())[0];
    let bb = b.block_params(b.entry())[1];

    let join = b.create_block(&[Type::Int]);
    let then_blk = b.create_block(&[]);
    let else_blk = b.create_block(&[]);

    let cond = b.bin(BinOp::Lt, a, bb);
    b.branch(cond, then_blk, &[], else_blk, &[]);

    b.switch_to(then_blk);
    b.jump(join, &[bb]);

    b.switch_to(else_blk);
    b.jump(join, &[a]);

    b.switch_to(join);
    let result = b.block_params(join)[0];
    b.ret(Some(result));

    let func = b.finish();
    assert_eq!(func.validate(), Ok(()));
    assert_eq!(func.block_count(), 4);
    assert_eq!(func.params(), &[Type::Int, Type::Int]);
    assert_eq!(func.ret(), Type::Int);

    // The entry ends in a conditional branch on the comparison.
    assert!(matches!(
        func.terminator(func.entry()),
        Some(Terminator::Branch { .. })
    ));
    assert_eq!(func.value_type(cond), Some(Type::Bool));
}

/// `fn abs(x: int) -> int { if x < 0 { -x } else { x } }`
#[test]
fn test_lower_negation_in_a_branch() {
    let mut b = Builder::new("abs", &[Type::Int], Type::Int);
    let x = b.block_params(b.entry())[0];

    let join = b.create_block(&[Type::Int]);
    let neg_blk = b.create_block(&[]);
    let pos_blk = b.create_block(&[]);

    let zero = b.iconst(0);
    let is_neg = b.bin(BinOp::Lt, x, zero);
    b.branch(is_neg, neg_blk, &[], pos_blk, &[]);

    b.switch_to(neg_blk);
    let negated = b.un(UnOp::Neg, x);
    b.jump(join, &[negated]);

    b.switch_to(pos_blk);
    b.jump(join, &[x]);

    b.switch_to(join);
    let result = b.block_params(join)[0];
    b.ret(Some(result));

    let func = b.finish();
    assert_eq!(func.validate(), Ok(()));
    assert!(matches!(func.inst(negated), Some(Inst::Un(UnOp::Neg, _))));
}

/// A counted loop: `i` starts at the parameter and counts down to zero. Exercises a
/// back-edge, so the dominator-based validation must accept a cyclic CFG.
#[test]
fn test_lower_countdown_loop() {
    let mut b = Builder::new("countdown", &[Type::Int], Type::Unit);
    let start = b.block_params(b.entry())[0];

    let header = b.create_block(&[Type::Int]);
    let body = b.create_block(&[]);
    let exit = b.create_block(&[]);

    b.jump(header, &[start]);

    b.switch_to(header);
    let i = b.block_params(header)[0];
    let zero = b.iconst(0);
    let more = b.bin(BinOp::Gt, i, zero);
    b.branch(more, body, &[], exit, &[]);

    b.switch_to(body);
    let one = b.iconst(1);
    let next = b.bin(BinOp::Sub, i, one);
    b.jump(header, &[next]);

    b.switch_to(exit);
    b.ret(None);

    let func = b.finish();
    assert_eq!(func.validate(), Ok(()));
    // The header's parameter is live across the back-edge from the body.
    assert_eq!(func.block_params(header).len(), 1);
}

/// The textual IR reflects the structure that was built.
#[test]
fn test_display_reflects_built_structure() {
    let mut b = Builder::new("poly", &[Type::Int, Type::Int], Type::Int);
    let a = b.block_params(b.entry())[0];
    let bb = b.block_params(b.entry())[1];
    let sum = b.bin(BinOp::Add, a, bb);
    let diff = b.bin(BinOp::Sub, a, bb);
    let product = b.bin(BinOp::Mul, sum, diff);
    b.ret(Some(product));

    let text = b.finish().to_string();
    assert!(text.contains("fn poly(int, int) -> int {"));
    assert!(text.contains("b0(v0: int, v1: int):"));
    assert!(text.contains("v2: int = add v0, v1"));
    assert!(text.contains("v3: int = sub v0, v1"));
    assert!(text.contains("v4: int = mul v2, v3"));
    assert!(text.contains("return v4"));
}

/// Building the same program twice produces equal functions — lowering is
/// deterministic.
#[test]
fn test_building_is_deterministic() {
    fn build() -> ir_lang::Function {
        let mut b = Builder::new("k", &[Type::Int], Type::Int);
        let x = b.block_params(b.entry())[0];
        let two = b.iconst(2);
        let doubled = b.bin(BinOp::Mul, x, two);
        b.ret(Some(doubled));
        b.finish()
    }
    assert_eq!(build(), build());
}
