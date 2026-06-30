//! What `Function::validate` catches.
//!
//! Construction does not check well-formedness as it goes — that keeps lowering
//! cheap and lets a front-end emit blocks in any order. `validate` is the safety net:
//! it returns a defined [`ValidationError`] rather than panicking when the IR is
//! wrong. This example builds several deliberately broken functions and prints the
//! error each one produces.
//!
//! Run it with:
//!
//! ```text
//! cargo run --example validation
//! ```

use ir_lang::{BinOp, Builder, Function, Type, UnOp};

/// Builds `int + bool`, a type error: arithmetic needs matching operands.
fn mismatched_operands() -> Function {
    let mut b = Builder::new("mismatched_operands", &[Type::Int, Type::Bool], Type::Int);
    let x = b.block_params(b.entry())[0];
    let flag = b.block_params(b.entry())[1];
    let bad = b.bin(BinOp::Add, x, flag);
    b.ret(Some(bad));
    b.finish()
}

/// Builds `not` of an integer: `not` requires a boolean.
fn not_on_an_int() -> Function {
    let mut b = Builder::new("not_on_an_int", &[Type::Int], Type::Bool);
    let x = b.block_params(b.entry())[0];
    let bad = b.un(UnOp::Not, x);
    b.ret(Some(bad));
    b.finish()
}

/// Branches on an integer condition, which must be a boolean.
fn non_boolean_condition() -> Function {
    let mut b = Builder::new("non_boolean_condition", &[Type::Int], Type::Unit);
    let x = b.block_params(b.entry())[0];
    let yes = b.create_block(&[]);
    let no = b.create_block(&[]);
    b.branch(x, yes, &[], no, &[]);
    b.switch_to(yes);
    b.ret(None);
    b.switch_to(no);
    b.ret(None);
    b.finish()
}

/// Jumps to a block passing no argument for its one parameter.
fn wrong_argument_count() -> Function {
    let mut b = Builder::new("wrong_argument_count", &[], Type::Int);
    let exit = b.create_block(&[Type::Int]);
    b.jump(exit, &[]); // exit expects one argument
    b.switch_to(exit);
    let r = b.block_params(exit)[0];
    b.ret(Some(r));
    b.finish()
}

/// Leaves a block with no terminator.
fn unterminated_block() -> Function {
    Builder::new("unterminated_block", &[], Type::Unit).finish()
}

fn main() {
    let cases = [
        mismatched_operands(),
        not_on_an_int(),
        non_boolean_condition(),
        wrong_argument_count(),
        unterminated_block(),
    ];

    for func in &cases {
        match func.validate() {
            Ok(()) => println!("{:>22}: unexpectedly valid", func.name()),
            Err(e) => println!("{:>22}: {e}", func.name()),
        }
    }
}
