//! Lowering a tiny language's syntax tree into ir-lang IR.
//!
//! ir-lang has no AST type of its own — a language brings its own tree and walks it,
//! driving the [`Builder`]. This example defines a small expression-and-statement
//! language, lowers a function written in it to SSA IR, validates the result, and
//! prints the textual form. It is the shape a real front-end's lowering pass takes.
//!
//! Run it with:
//!
//! ```text
//! cargo run --example lower_ast
//! ```

use ir_lang::{BinOp, Builder, Type, Value};

/// An expression in the toy language. Every expression has type `int`, except the
/// comparison inside an `If`, which is the boolean it branches on.
enum Expr {
    /// A reference to the function's parameter by index.
    Param(usize),
    /// An integer literal.
    Int(i64),
    /// `lhs <op> rhs`, arithmetic over two integer expressions.
    Arith(BinOp, Box<Expr>, Box<Expr>),
    /// `if lhs < rhs { then } else { els }` — an expression that yields an int.
    IfLess {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
}

/// Recursively lowers `expr` into the builder's current block, returning the value it
/// produces. Control flow (`IfLess`) creates blocks and a join with a parameter — the
/// SSA way to merge the two arms without a phi node.
fn lower(b: &mut Builder, expr: &Expr, params: &[Value]) -> Value {
    match expr {
        Expr::Param(i) => params[*i],
        Expr::Int(n) => b.iconst(*n),
        Expr::Arith(op, lhs, rhs) => {
            let l = lower(b, lhs, params);
            let r = lower(b, rhs, params);
            b.bin(*op, l, r)
        }
        Expr::IfLess {
            lhs,
            rhs,
            then,
            els,
        } => {
            let l = lower(b, lhs, params);
            let r = lower(b, rhs, params);
            let cond = b.bin(BinOp::Lt, l, r);

            // The join block receives the selected value as its parameter.
            let join = b.create_block(&[Type::Int]);
            let then_blk = b.create_block(&[]);
            let else_blk = b.create_block(&[]);
            b.branch(cond, then_blk, &[], else_blk, &[]);

            b.switch_to(then_blk);
            let then_val = lower(b, then, params);
            b.jump(join, &[then_val]);

            b.switch_to(else_blk);
            let else_val = lower(b, els, params);
            b.jump(join, &[else_val]);

            b.switch_to(join);
            b.block_params(join)[0]
        }
    }
}

/// Lowers a complete `fn(int, int) -> int` whose body is `body`.
fn lower_function(name: &str, arity: usize, body: &Expr) -> ir_lang::Function {
    let param_types = vec![Type::Int; arity];
    let mut b = Builder::new(name, &param_types, Type::Int);
    let params: Vec<Value> = b.block_params(b.entry()).to_vec();
    let result = lower(&mut b, body, &params);
    b.ret(Some(result));
    b.finish()
}

fn main() {
    // fn relu(x) -> int { if x < 0 { 0 } else { x } }
    let relu = lower_function(
        "relu",
        1,
        &Expr::IfLess {
            lhs: Box::new(Expr::Param(0)),
            rhs: Box::new(Expr::Int(0)),
            then: Box::new(Expr::Int(0)),
            els: Box::new(Expr::Param(0)),
        },
    );

    // fn poly(a, b) -> int { (a + b) * (a - b) }
    let poly = lower_function(
        "poly",
        2,
        &Expr::Arith(
            BinOp::Mul,
            Box::new(Expr::Arith(
                BinOp::Add,
                Box::new(Expr::Param(0)),
                Box::new(Expr::Param(1)),
            )),
            Box::new(Expr::Arith(
                BinOp::Sub,
                Box::new(Expr::Param(0)),
                Box::new(Expr::Param(1)),
            )),
        ),
    );

    for func in [&relu, &poly] {
        match func.validate() {
            Ok(()) => {
                println!("{func}");
                println!("// validated ok\n");
            }
            Err(e) => println!("// {} failed validation: {e}\n", func.name()),
        }
    }
}
