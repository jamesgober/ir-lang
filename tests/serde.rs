//! Round-trip test for the `serde` feature: a function survives serialization and
//! deserialization unchanged and still validates.

#![cfg(feature = "serde")]

use ir_lang::{BinOp, Builder, Type, UnOp};

#[test]
fn test_function_round_trips_through_json() {
    let mut b = Builder::new("compute", &[Type::Int, Type::Int], Type::Int);
    let a = b.block_params(b.entry())[0];
    let bb = b.block_params(b.entry())[1];

    let join = b.create_block(&[Type::Int]);
    let then_blk = b.create_block(&[]);
    let else_blk = b.create_block(&[]);

    let cond = b.bin(BinOp::Ge, a, bb);
    b.branch(cond, then_blk, &[], else_blk, &[]);

    b.switch_to(then_blk);
    let neg = b.un(UnOp::Neg, a);
    b.jump(join, &[neg]);

    b.switch_to(else_blk);
    b.jump(join, &[bb]);

    b.switch_to(join);
    let result = b.block_params(join)[0];
    b.ret(Some(result));

    let original = b.finish();
    assert_eq!(original.validate(), Ok(()));

    let json = serde_json::to_string(&original).expect("serialization succeeds");
    let restored: ir_lang::Function =
        serde_json::from_str(&json).expect("deserialization succeeds");

    assert_eq!(restored, original);
    assert_eq!(restored.validate(), Ok(()));
}

#[test]
fn test_validation_error_round_trips_through_json() {
    let err = Builder::new("g", &[], Type::Int)
        .finish()
        .validate()
        .expect_err("a function with an unterminated block is invalid");

    let json = serde_json::to_string(&err).expect("serialization succeeds");
    let restored: ir_lang::ValidationError =
        serde_json::from_str(&json).expect("deserialization succeeds");
    assert_eq!(restored, err);
}
