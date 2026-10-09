//! Running IR in the reference interpreter (feature `interp`): an iterative
//! factorial whose multiplication raises `ArithOverflow` through a `check`.
//!
//! Run with `cargo run --example run --features interp`.

use ir_lang::interp::{self, Outcome, Val};
use ir_lang::{
    BinaryOp, BlockArg, BlockCall, CmpOp, InstData, Linkage, Module, Overflow, Policy, Signature,
    Type,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut m = Module::new("example");
    let sig = Signature::new(&[Type::U64], &[Type::U64, Type::U32]);
    let f = m.declare_function("fact", &sig, Linkage::Export)?;
    let mut b = m.build(f)?;
    let n = b.param(0)?;
    let head = b.create_block(&[Type::U64, Type::U64])?; // (i, acc)
    let body = b.create_block(&[])?;
    let next = b.create_block(&[Type::U64])?;
    let done = b.create_block(&[])?;
    let overflow = b.create_block(&[Type::U32])?;
    let one = b.iconst(Type::U64, 1)?;
    b.jump(head, &[one, one])?;

    b.switch_to(head)?;
    let (i, acc) = (b.block_param(head, 0)?, b.block_param(head, 1)?);
    let more = b.compare(CmpOp::Le, i, n)?;
    b.branch(more, body, &[], done, &[])?;

    b.switch_to(body)?;
    b.check(
        InstData::Binary {
            op: BinaryOp::Mul,
            policy: Policy::overflow(Overflow::Error),
            args: [acc, i],
        },
        BlockCall::with_args(next, &[BlockArg::Result(0)]),
        BlockCall::with_args(overflow, &[BlockArg::ErrorCode]),
    )?;

    b.switch_to(next)?;
    let acc2 = b.block_param(next, 0)?;
    let i2 = b.add(i, one, Overflow::Wrap)?;
    b.jump(head, &[i2, acc2])?;

    b.switch_to(done)?;
    let ok = b.iconst(Type::U32, 0)?;
    b.ret(&[acc, ok])?;

    b.switch_to(overflow)?;
    let code = b.block_param(overflow, 0)?;
    let zero = b.iconst(Type::U64, 0)?;
    b.ret(&[zero, code])?;
    m.validate()?;

    for k in [5u64, 20, 21] {
        match interp::run(&m, f, &[Val::u64(k)])? {
            Outcome::Return(v) if v[1] == Val::u32(0) => println!("{k}! = {}", v[0].bits()),
            Outcome::Return(v) => println!("{k}! fails with error code {}", v[1].bits()),
            other => println!("{k}! -> {other:?}"),
        }
    }
    Ok(())
}
