//! Lowering a small expression language into IR: the Tier-1 path.
//!
//! The front end owns its tree; it walks it and calls one builder method per
//! construct. `if` becomes a branch to two blocks that meet at a join block whose
//! parameter carries the result (block parameters instead of phi nodes), and
//! division becomes a `check` terminator because the language reports division by
//! zero as an error value.
//!
//! Run with `cargo run --example lower`.

use ir_lang::{
    BinaryOp, BlockArg, BlockCall, BuildError, Builder, CmpOp, DivZero, InstData, Linkage, Module,
    Overflow, Policy, Signature, Type, Value,
};

/// The front end's tree: integer expressions over one parameter `x`.
enum Expr {
    X,
    Lit(i64),
    Add(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    /// `if a < b { then } else { other }`
    IfLess(Box<Expr>, Box<Expr>, Box<Expr>, Box<Expr>),
}

struct Lower<'b, 'm> {
    b: &'b mut Builder<'m>,
    /// Where a division by zero goes: it returns the error code.
    on_error: ir_lang::Block,
}

impl Lower<'_, '_> {
    fn expr(&mut self, e: &Expr) -> Result<Value, BuildError> {
        match e {
            Expr::X => self.b.param(0),
            Expr::Lit(n) => self.b.iconst(Type::I64, i128::from(*n)),
            Expr::Add(a, b) => {
                let (a, b) = (self.expr(a)?, self.expr(b)?);
                self.b.add(a, b, Overflow::Wrap)
            }
            Expr::Div(a, b) => {
                let (a, b) = (self.expr(a)?, self.expr(b)?);
                let ok = self.b.create_block(&[Type::I64])?;
                self.b.check(
                    InstData::Binary {
                        op: BinaryOp::Div,
                        policy: Policy::overflow(Overflow::Wrap).with_div_zero(DivZero::Error),
                        args: [a, b],
                    },
                    BlockCall::with_args(ok, &[BlockArg::Result(0)]),
                    BlockCall::with_args(self.on_error, &[BlockArg::ErrorCode]),
                )?;
                self.b.switch_to(ok)?;
                self.b.block_param(ok, 0)
            }
            Expr::IfLess(a, b, then, other) => {
                let (a, b) = (self.expr(a)?, self.expr(b)?);
                let less = self.b.compare(CmpOp::Lt, a, b)?;
                let (t, f) = (self.b.create_block(&[])?, self.b.create_block(&[])?);
                let join = self.b.create_block(&[Type::I64])?;
                self.b.branch(less, t, &[], f, &[])?;
                self.b.switch_to(t)?;
                let v = self.expr(then)?;
                self.b.jump(join, &[v])?;
                self.b.switch_to(f)?;
                let v = self.expr(other)?;
                self.b.jump(join, &[v])?;
                self.b.switch_to(join)?;
                self.b.block_param(join, 0)
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // f(x) = if x < 10 { 100 / x } else { x + 1 }
    let tree = Expr::IfLess(
        Box::new(Expr::X),
        Box::new(Expr::Lit(10)),
        Box::new(Expr::Div(Box::new(Expr::Lit(100)), Box::new(Expr::X))),
        Box::new(Expr::Add(Box::new(Expr::X), Box::new(Expr::Lit(1)))),
    );

    let mut module = Module::new("example");
    // Returns (value, error code); the code is 0 on success.
    let sig = Signature::new(&[Type::I64], &[Type::I64, Type::U32]);
    let f = module.declare_function("f", &sig, Linkage::Export)?;
    let mut b = module.build(f)?;

    let on_error = b.create_block(&[Type::U32])?;
    let mut lower = Lower {
        b: &mut b,
        on_error,
    };
    let result = lower.expr(&tree)?;
    let ok = b.iconst(Type::U32, 0)?;
    b.ret(&[result, ok])?;
    b.switch_to(on_error)?;
    let code = b.block_param(on_error, 0)?;
    let zero = b.iconst(Type::I64, 0)?;
    b.ret(&[zero, code])?;

    module.validate()?;
    print!("{module}");
    Ok(())
}
