//! Lowering mutable locals and GC references the easy way: the builder places
//! the block parameters for variables, and `insert_statepoints` relocates the
//! references live across calls.
//!
//! The source, in a toy language:
//!
//! ```text
//! fn count(list: ref, n: i64) -> (ref, i64) {
//!     let mut total = 0;
//!     let mut i = 0;
//!     while i < n {
//!         total = total + i;
//!         poll();          // may collect garbage: `list` must survive it
//!         i = i + 1;
//!     }
//!     (list, total)
//! }
//! ```
//!
//! Run with `cargo run --example variables`.

use ir_lang::{BuildError, Call, CmpOp, Linkage, Module, Overflow, Signature, Type};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut m = Module::new("variables");
    let poll = m.declare_function("poll", &Signature::new(&[], &[]), Linkage::Import)?;
    let f = m.declare_function(
        "count",
        &Signature::new(&[Type::Ref, Type::I64], &[Type::Ref, Type::I64]),
        Linkage::Export,
    )?;
    let mut b = m.build(f)?;
    lower(&mut b, poll)?;
    let stats = b.insert_statepoints()?;
    println!(
        "statepoints: {} safepoint(s) updated, {} relocation(s), {} block parameter(s)",
        stats.safepoints, stats.relocations, stats.params
    );
    m.validate()?;
    print!("{m}");
    Ok(())
}

fn lower(b: &mut ir_lang::Builder<'_>, poll: ir_lang::FuncId) -> Result<(), BuildError> {
    let list = b.declare_var(Type::Ref)?;
    let total = b.declare_var(Type::I64)?;
    let i = b.declare_var(Type::I64)?;
    let (l, n) = (b.param(0)?, b.param(1)?);
    b.def_var(list, l)?;
    let zero = b.iconst(Type::I64, 0)?;
    b.def_var(total, zero)?;
    b.def_var(i, zero)?;

    let head = b.create_block(&[])?;
    let body = b.create_block(&[])?;
    let exit = b.create_block(&[])?;
    b.jump(head, &[])?;

    // The loop head stays unsealed until the back edge exists.
    b.switch_to(head)?;
    let iv = b.use_var(i)?;
    let more = b.compare(CmpOp::Lt, iv, n)?;
    b.branch(more, body, &[], exit, &[])?;

    b.switch_to(body)?;
    b.seal_block(body)?;
    let (t, iv) = (b.use_var(total)?, b.use_var(i)?);
    let t2 = b.add(t, iv, Overflow::Wrap)?;
    b.def_var(total, t2)?;
    b.call(Call::direct(poll, &[]))?;
    let one = b.iconst(Type::I64, 1)?;
    let i2 = b.add(iv, one, Overflow::Wrap)?;
    b.def_var(i, i2)?;
    b.jump(head, &[])?;
    b.seal_block(head)?;

    b.switch_to(exit)?;
    b.seal_block(exit)?;
    let (lv, tv) = (b.use_var(list)?, b.use_var(total)?);
    b.ret(&[lv, tv])?;
    b.seal_all_blocks()
}
