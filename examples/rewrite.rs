//! A minimal optimization pass with the mutation API: fold an `add` of two
//! constants, forward every use to the folded constant, and delete the dead
//! instructions. Handles never change while rewriting; `compact` renumbers at the
//! end and reports the mapping.
//!
//! Run with `cargo run --example rewrite`.

use ir_lang::{BinaryOp, InstData, Linkage, Module, Overflow, Position, Signature, Type};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut module = Module::new("example");
    let f =
        module.declare_function("seven", &Signature::new(&[], &[Type::I32]), Linkage::Export)?;
    let mut b = module.build(f)?;
    let three = b.iconst(Type::I32, 3)?;
    let four = b.iconst(Type::I32, 4)?;
    let sum = b.add(three, four, Overflow::Wrap)?;
    b.ret(&[sum])?;
    println!("before:\n{}", module.display_function(f));

    let mut b = module.edit(f)?;
    let entry = b.func().entry();
    let insts: Vec<_> = b.func().insts(entry).collect();
    for inst in insts {
        let Some(InstData::Binary {
            op: BinaryOp::Add,
            args,
            ..
        }) = b.func().inst(inst).cloned()
        else {
            continue;
        };
        let func = b.func();
        let constant = |v| match func
            .defining_inst(func.resolve(v))
            .and_then(|i| func.inst(i))
        {
            Some(InstData::Const { bits, .. }) => Some(*bits as u32),
            _ => None,
        };
        if let (Some(x), Some(y)) = (constant(args[0]), constant(args[1])) {
            b.set_position(Position::Before(inst))?;
            let folded = b.iconst(Type::I32, i128::from(x.wrapping_add(y) as i32))?;
            let old = b.results(inst).first().ok_or("add has a result")?;
            b.replace_all_uses(old, folded)?;
            b.remove_inst(inst)?;
        }
    }
    // The constants 3 and 4 are now unused.
    for v in [three, four] {
        if let Some(inst) = b.func().defining_inst(v) {
            b.remove_inst(inst)?;
        }
    }
    let _remap = b.compact();
    b.validate()?;
    println!("after:\n{}", module.display_function(f));
    Ok(())
}
