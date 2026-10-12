//! Random structured programs, an independent evaluator for them, and their
//! lowering to IR through the public `Builder`.
//!
//! The evaluator implements the operation semantics with Rust's own checked and
//! wrapping arithmetic (no code shared with the crate's interpreter), so running
//! the lowered IR in the interpreter and comparing is a differential test of the
//! builder, the validator, the printer/parser, and the interpreter at once.
//!
//! Every function has the signature `(i64, i64) -> (i64, u32)`: a value and a
//! status (0 = ok, an OPS error code, or 100 + k for a caught exception k).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use ir_lang::{
    BinaryOp, Block, BlockArg, BlockCall, Builder, Call, CmpOp, ConvOp, DivZero, FloatToInt,
    FuncId, Global, GlobalId, InstData, Linkage, MemFlags, Module, Overflow, Policy, Shift,
    Signature, StackSlot, Type, Value, Variable,
};

/// A small deterministic generator (xorshift64*), seeded by proptest.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    pub fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    pub fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize]
    }

    pub fn lit(&mut self) -> i64 {
        match self.below(6) {
            0 => self.pick(&[0, 1, -1, 2, 7, 63, 64, -64]),
            1 => self.pick(&[i64::MIN, i64::MAX, i64::MIN + 1, i64::MAX - 1]),
            2 => self.pick(&[i32::MIN as i64, i32::MAX as i64, u32::MAX as i64, 255, 256]),
            _ => (self.next() as i64) >> self.below(60),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    FloorDiv,
    FloorMod,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Min,
    Max,
    /// OPS v2 integer power.
    Pow,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FArith {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Lit(i64),
    Param(usize),
    Var(usize),
    Bin(Arith, Policy, Box<Expr>, Box<Expr>),
    Neg(Overflow, Box<Expr>),
    /// `int_cast` to i32 with the policy, then sign-extend back.
    CastI32(Overflow, Box<Expr>),
    /// `narrow` to u8, then zero-extend back.
    LowByte(Box<Expr>),
    /// Convert both to f64, combine, convert back with the policy.
    Float(FArith, FloatToInt, Box<Expr>, Box<Expr>),
    Select(CmpOp, Box<Expr>, Box<Expr>, Box<Expr>, Box<Expr>),
    /// Load `array[index & 7]`.
    Load(Box<Expr>),
    Global(usize),
    Call {
        func: usize,
        args: Vec<Expr>,
        invoke: bool,
        indirect: bool,
    },
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Set(usize, Expr),
    If(CmpOp, Expr, Expr, Vec<Stmt>, Vec<Stmt>),
    Loop(u8, Vec<Stmt>),
    Store(Expr, Expr),
    SetGlobal(usize, Expr),
    Raise(u8),
    Trap(u8),
}

#[derive(Clone, Debug)]
pub enum Tail {
    Ret(Expr),
    TailCall(usize, Expr, Expr),
}

#[derive(Clone, Debug)]
pub struct FuncDef {
    pub body: Vec<Stmt>,
    pub tail: Tail,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub funcs: Vec<FuncDef>,
}

pub const VARS: usize = 3;
pub const GLOBALS: usize = 2;

// ------------------------------------------------------------------ generation

struct Gen<'r> {
    rng: &'r mut Rng,
    func: usize,
    nfuncs: usize,
    budget: u32,
}

impl Gen<'_> {
    fn policy_for(&mut self, op: Arith) -> Policy {
        let ov = self.rng.pick(&[
            Overflow::Wrap,
            Overflow::Wrap,
            Overflow::Trap,
            Overflow::Error,
        ]);
        let dz = self.rng.pick(&[DivZero::Error, DivZero::Trap]);
        match op {
            Arith::Add | Arith::Sub | Arith::Mul | Arith::Pow => Policy::overflow(ov),
            Arith::Div | Arith::FloorDiv => Policy::overflow(ov).with_div_zero(dz),
            Arith::Rem | Arith::FloorMod => Policy::div_zero(dz),
            Arith::Shl | Arith::Shr => Policy::shift(self.rng.pick(&[
                Shift::Mask,
                Shift::Mask,
                Shift::Error,
                Shift::Saturate,
            ])),
            _ => Policy::NONE,
        }
    }

    fn expr(&mut self, depth: u32) -> Expr {
        self.budget = self.budget.saturating_sub(1);
        if depth == 0 || self.budget == 0 || self.rng.chance(30) {
            return match self.rng.below(4) {
                0 => Expr::Lit(self.rng.lit()),
                1 => Expr::Param(self.rng.below(2) as usize),
                2 => Expr::Var(self.rng.below(VARS as u64) as usize),
                _ => Expr::Global(self.rng.below(GLOBALS as u64) as usize),
            };
        }
        let d = depth - 1;
        match self.rng.below(12) {
            0..=3 => {
                let op = self.rng.pick(&[
                    Arith::Add,
                    Arith::Sub,
                    Arith::Mul,
                    Arith::Div,
                    Arith::Rem,
                    Arith::FloorDiv,
                    Arith::FloorMod,
                    Arith::And,
                    Arith::Or,
                    Arith::Xor,
                    Arith::Shl,
                    Arith::Shr,
                    Arith::Min,
                    Arith::Max,
                    Arith::Pow,
                ]);
                let p = self.policy_for(op);
                Expr::Bin(op, p, Box::new(self.expr(d)), Box::new(self.expr(d)))
            }
            4 => Expr::Neg(
                self.rng
                    .pick(&[Overflow::Wrap, Overflow::Error, Overflow::Trap]),
                Box::new(self.expr(d)),
            ),
            5 => Expr::CastI32(
                self.rng
                    .pick(&[Overflow::Wrap, Overflow::Error, Overflow::Trap]),
                Box::new(self.expr(d)),
            ),
            6 => Expr::LowByte(Box::new(self.expr(d))),
            7 => Expr::Float(
                self.rng
                    .pick(&[FArith::Add, FArith::Sub, FArith::Mul, FArith::Div]),
                self.rng.pick(&[FloatToInt::Saturate, FloatToInt::Error]),
                Box::new(self.expr(d)),
                Box::new(self.expr(d)),
            ),
            8 => Expr::Select(
                self.cmp(),
                Box::new(self.expr(d)),
                Box::new(self.expr(d)),
                Box::new(self.expr(d)),
                Box::new(self.expr(d)),
            ),
            9 => Expr::Load(Box::new(self.expr(d))),
            _ if self.func + 1 < self.nfuncs => {
                let func =
                    self.func + 1 + self.rng.below((self.nfuncs - self.func - 1) as u64) as usize;
                Expr::Call {
                    func,
                    args: vec![self.expr(d), self.expr(d)],
                    invoke: self.rng.chance(60),
                    indirect: self.rng.chance(25),
                }
            }
            _ => Expr::Lit(self.rng.lit()),
        }
    }

    fn cmp(&mut self) -> CmpOp {
        self.rng.pick(&[
            CmpOp::Eq,
            CmpOp::Ne,
            CmpOp::Lt,
            CmpOp::Le,
            CmpOp::Gt,
            CmpOp::Ge,
        ])
    }

    fn stmts(&mut self, depth: u32, n: u64) -> Vec<Stmt> {
        let count = self.rng.below(n) + 1;
        (0..count).map(|_| self.stmt(depth)).collect()
    }

    fn stmt(&mut self, depth: u32) -> Stmt {
        let nested = depth > 0 && self.budget > 0;
        match self.rng.below(16) {
            0..=5 => Stmt::Set(self.rng.below(VARS as u64) as usize, self.expr(3)),
            6 | 7 if nested => Stmt::If(
                self.cmp(),
                self.expr(2),
                self.expr(2),
                self.stmts(depth - 1, 3),
                self.stmts(depth - 1, 2),
            ),
            8 | 9 if nested => Stmt::Loop(self.rng.below(5) as u8, self.stmts(depth - 1, 3)),
            10 | 11 => Stmt::Store(self.expr(2), self.expr(2)),
            12 => Stmt::SetGlobal(self.rng.below(GLOBALS as u64) as usize, self.expr(2)),
            13 if self.rng.chance(20) => Stmt::Raise(self.rng.below(9) as u8 + 1),
            14 if self.rng.chance(10) => Stmt::Trap(self.rng.below(9) as u8 + 10),
            _ => Stmt::Set(self.rng.below(VARS as u64) as usize, self.expr(2)),
        }
    }
}

pub fn generate(seed: u64) -> Program {
    let mut rng = Rng::new(seed);
    let nfuncs = rng.below(4) as usize + 1;
    let mut funcs = Vec::new();
    for func in 0..nfuncs {
        let mut g = Gen {
            rng: &mut rng,
            func,
            nfuncs,
            budget: 60,
        };
        let body = g.stmts(2, 5);
        let tail = if func + 1 < nfuncs && g.rng.chance(25) {
            let callee = func + 1 + g.rng.below((nfuncs - func - 1) as u64) as usize;
            Tail::TailCall(callee, g.expr(2), g.expr(2))
        } else {
            Tail::Ret(g.expr(3))
        };
        funcs.push(FuncDef { body, tail });
    }
    Program { funcs }
}

// ------------------------------------------------------------------ evaluation

/// How a run ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    Return(i64, u32),
    Trap(u32),
    Unwind(u64),
}

/// Non-local exits while evaluating.
#[derive(Clone, Debug)]
enum Exit {
    /// An OPS error: the function returns `(0, code)`.
    Error(u32),
    /// A trap: the program stops.
    Trap(u32),
    /// An exception with payload k.
    Raise(u64),
    /// A status propagated from a callee: the function returns `(0, status)`.
    Status(u32),
}

pub struct Eval<'p> {
    prog: &'p Program,
    globals: [i64; GLOBALS],
}

struct Frame {
    params: [i64; 2],
    vars: [i64; VARS],
    array: [i64; 8],
}

fn bin(op: Arith, p: Policy, a: i64, b: i64) -> Result<i64, Exit> {
    let ovf = |wrapped: i64| match p.overflow {
        Some(Overflow::Wrap) => Ok(wrapped),
        Some(Overflow::Trap) => Err(Exit::Trap(1)),
        _ => Err(Exit::Error(1)),
    };
    let dz = || match p.div_zero {
        Some(DivZero::Trap) => Exit::Trap(2),
        _ => Exit::Error(2),
    };
    Ok(match op {
        Arith::Add => match a.checked_add(b) {
            Some(v) => v,
            None => ovf(a.wrapping_add(b))?,
        },
        Arith::Sub => match a.checked_sub(b) {
            Some(v) => v,
            None => ovf(a.wrapping_sub(b))?,
        },
        Arith::Mul => match a.checked_mul(b) {
            Some(v) => v,
            None => ovf(a.wrapping_mul(b))?,
        },
        Arith::Div => {
            if b == 0 {
                return Err(dz());
            }
            match a.checked_div(b) {
                Some(v) => v,
                None => ovf(a.wrapping_div(b))?,
            }
        }
        Arith::Rem => {
            if b == 0 {
                return Err(dz());
            }
            a.wrapping_rem(b)
        }
        Arith::FloorDiv | Arith::FloorMod => {
            if b == 0 {
                return Err(dz());
            }
            let (a, b) = (a as i128, b as i128);
            let m = ((a % b) + b) % b;
            if op == Arith::FloorMod {
                m as i64
            } else {
                let q = (a - m) / b;
                match i64::try_from(q) {
                    Ok(v) => v,
                    Err(_) => ovf(q as i64)?,
                }
            }
        }
        Arith::And => a & b,
        Arith::Or => a | b,
        Arith::Xor => a ^ b,
        Arith::Shl | Arith::Shr => {
            let n = if (0..64).contains(&b) {
                b as u32
            } else if p.shift == Some(Shift::Mask) {
                (b & 63) as u32
            } else if p.shift == Some(Shift::Saturate) && b >= 64 {
                // OPS v2: every bit shifted out; `shr` keeps the sign.
                return Ok(if op == Arith::Shr && a < 0 { -1 } else { 0 });
            } else {
                return Err(Exit::Error(3));
            };
            if op == Arith::Shl {
                a.wrapping_shl(n)
            } else {
                a >> n
            }
        }
        Arith::Min => a.min(b),
        Arith::Max => a.max(b),
        Arith::Pow => {
            if b < 0 {
                return Err(match p.overflow {
                    Some(Overflow::Trap) => Exit::Trap(6),
                    _ => Exit::Error(6),
                });
            }
            match pow_exact(a, b as u64) {
                Some(v) => v,
                None => ovf(pow_wrapped(a, b as u64))?,
            }
        }
    })
}

/// The exact `a ** e` if it fits `i64` (an independent formulation: naive
/// multiplication for small exponents, closed forms for |a| < 2).
pub fn pow_exact(a: i64, e: u64) -> Option<i64> {
    match a {
        0 => return Some(i64::from(e == 0)),
        1 => return Some(1),
        -1 => return Some(if e % 2 == 0 { 1 } else { -1 }),
        _ => {}
    }
    if e >= 64 {
        return None; // |a| >= 2: at least 2^64
    }
    let mut acc: i64 = 1;
    for _ in 0..e {
        acc = acc.checked_mul(a)?;
    }
    Some(acc)
}

/// The low 64 bits of `a ** e` (most significant bit first, unlike the
/// interpreter's squaring order).
pub fn pow_wrapped(a: i64, e: u64) -> i64 {
    let mut acc: i64 = 1;
    for bit in (0..64).rev() {
        acc = acc.wrapping_mul(acc);
        if (e >> bit) & 1 == 1 {
            acc = acc.wrapping_mul(a);
        }
    }
    acc
}

fn float_to_i64(x: f64, p: FloatToInt) -> Result<i64, Exit> {
    let t = x.trunc();
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&t) {
        return Ok(t as i64);
    }
    match p {
        FloatToInt::Saturate => Ok(x as i64), // Rust `as` saturates, NaN -> 0
        _ => Err(Exit::Error(4)),
    }
}

fn cmp(op: CmpOp, a: i64, b: i64) -> bool {
    match op {
        CmpOp::Eq => a == b,
        CmpOp::Ne => a != b,
        CmpOp::Lt => a < b,
        CmpOp::Le => a <= b,
        CmpOp::Gt => a > b,
        _ => a >= b,
    }
}

impl<'p> Eval<'p> {
    pub fn new(prog: &'p Program) -> Eval<'p> {
        Eval {
            prog,
            globals: [0; GLOBALS],
        }
    }

    pub fn run(&mut self, a: i64, b: i64) -> Expect {
        match self.call(0, a, b) {
            Ok((v, s)) => Expect::Return(v, s),
            Err(Exit::Trap(c)) => Expect::Trap(c),
            Err(Exit::Raise(k)) => Expect::Unwind(k),
            Err(Exit::Error(c) | Exit::Status(c)) => Expect::Return(0, c),
        }
    }

    /// Runs function `f`: `Ok((value, status))`, or a trap / an exception.
    fn call(&mut self, f: usize, a: i64, b: i64) -> Result<(i64, u32), Exit> {
        let mut frame = Frame {
            params: [a, b],
            vars: [0; VARS],
            array: [0; 8],
        };
        let prog = self.prog;
        let def = &prog.funcs[f];
        let r = self
            .stmts(&def.body, &mut frame)
            .and_then(|()| match &def.tail {
                Tail::Ret(e) => self.expr(e, &mut frame).map(|v| (v, 0)),
                // A tail call's result and exits are the callee's, unchanged.
                Tail::TailCall(g, x, y) => {
                    let x = self.expr(x, &mut frame)?;
                    let y = self.expr(y, &mut frame)?;
                    self.call(*g, x, y)
                }
            });
        match r {
            Ok(v) => Ok(v),
            Err(Exit::Error(c) | Exit::Status(c)) => Ok((0, c)),
            Err(other) => Err(other),
        }
    }

    fn stmts(&mut self, stmts: &[Stmt], fr: &mut Frame) -> Result<(), Exit> {
        for s in stmts {
            self.stmt(s, fr)?;
        }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt, fr: &mut Frame) -> Result<(), Exit> {
        match s {
            Stmt::Set(v, e) => fr.vars[*v] = self.expr(e, fr)?,
            Stmt::If(op, a, b, t, e) => {
                let (a, b) = (self.expr(a, fr)?, self.expr(b, fr)?);
                if cmp(*op, a, b) {
                    self.stmts(t, fr)?;
                } else {
                    self.stmts(e, fr)?;
                }
            }
            Stmt::Loop(n, body) => {
                for _ in 0..*n {
                    self.stmts(body, fr)?;
                }
            }
            Stmt::Store(i, v) => {
                let i = self.expr(i, fr)?;
                let v = self.expr(v, fr)?;
                fr.array[(i & 7) as usize] = v;
            }
            Stmt::SetGlobal(g, e) => self.globals[*g] = self.expr(e, fr)?,
            Stmt::Raise(k) => return Err(Exit::Raise(u64::from(*k))),
            Stmt::Trap(c) => return Err(Exit::Trap(u32::from(*c))),
        }
        Ok(())
    }

    fn expr(&mut self, e: &Expr, fr: &mut Frame) -> Result<i64, Exit> {
        Ok(match e {
            Expr::Lit(v) => *v,
            Expr::Param(i) => fr.params[*i],
            Expr::Var(i) => fr.vars[*i],
            Expr::Global(g) => self.globals[*g],
            Expr::Bin(op, p, a, b) => {
                let a = self.expr(a, fr)?;
                let b = self.expr(b, fr)?;
                bin(*op, *p, a, b)?
            }
            Expr::Neg(o, a) => {
                let a = self.expr(a, fr)?;
                bin(Arith::Sub, Policy::overflow(*o), 0, a)?
            }
            Expr::CastI32(o, a) => {
                let a = self.expr(a, fr)?;
                match i32::try_from(a) {
                    Ok(v) => i64::from(v),
                    Err(_) => match o {
                        Overflow::Wrap => i64::from(a as i32),
                        Overflow::Trap => return Err(Exit::Trap(1)),
                        _ => return Err(Exit::Error(1)),
                    },
                }
            }
            Expr::LowByte(a) => i64::from(self.expr(a, fr)? as u8),
            Expr::Float(op, p, a, b) => {
                let a = self.expr(a, fr)? as f64;
                let b = self.expr(b, fr)? as f64;
                let x = match op {
                    FArith::Add => a + b,
                    FArith::Sub => a - b,
                    FArith::Mul => a * b,
                    FArith::Div => a / b,
                };
                float_to_i64(x, *p)?
            }
            Expr::Select(op, a, b, t, f) => {
                let a = self.expr(a, fr)?;
                let b = self.expr(b, fr)?;
                let t = self.expr(t, fr)?;
                let f = self.expr(f, fr)?;
                if cmp(*op, a, b) { t } else { f }
            }
            Expr::Load(i) => fr.array[(self.expr(i, fr)? & 7) as usize],
            Expr::Call {
                func, args, invoke, ..
            } => {
                let x = self.expr(&args[0], fr)?;
                let y = self.expr(&args[1], fr)?;
                match self.call(*func, x, y) {
                    Ok((v, 0)) => v,
                    Ok((_, s)) => return Err(Exit::Status(s)),
                    Err(Exit::Raise(k)) if *invoke => return Err(Exit::Status(100 + k as u32)),
                    Err(other) => return Err(other),
                }
            }
        })
    }
}

// --------------------------------------------------------------------- lowering

pub struct Lowered {
    pub module: Module,
    pub funcs: Vec<FuncId>,
    pub globals: Vec<GlobalId>,
}

pub fn signature() -> Signature {
    Signature::new(&[Type::I64, Type::I64], &[Type::I64, Type::U32])
}

struct Lower<'b, 'm> {
    b: &'b mut Builder<'m>,
    funcs: &'b [FuncId],
    globals: &'b [GlobalId],
    sig: ir_lang::SigId,
    vars: Value,
    array: Value,
    mode: VarMode,
    ssa: Vec<Variable>,
}

const F8: MemFlags = MemFlags {
    align: 8,
    volatile: false,
};

fn arith_op(op: Arith) -> BinaryOp {
    match op {
        Arith::Add => BinaryOp::Add,
        Arith::Sub => BinaryOp::Sub,
        Arith::Mul => BinaryOp::Mul,
        Arith::Div => BinaryOp::Div,
        Arith::Rem => BinaryOp::Rem,
        Arith::FloorDiv => BinaryOp::FloorDiv,
        Arith::FloorMod => BinaryOp::FloorMod,
        Arith::And => BinaryOp::And,
        Arith::Or => BinaryOp::Or,
        Arith::Xor => BinaryOp::Xor,
        Arith::Shl => BinaryOp::Shl,
        Arith::Shr => BinaryOp::Shr,
        Arith::Min => BinaryOp::Min,
        Arith::Max => BinaryOp::Max,
        Arith::Pow => BinaryOp::Pow,
    }
}

/// How the lowering keeps the program's mutable variables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VarMode {
    /// In a stack slot, with loads and stores.
    Memory,
    /// As SSA values through the builder's variable construction, sealing each
    /// block as soon as its predecessors are known.
    SsaEager,
    /// As SSA values, sealing nothing until the end (every read in an unsealed
    /// block makes an incomplete parameter).
    SsaLazy,
}

impl Lower<'_, '_> {
    fn i64(&mut self, v: i64) -> Value {
        self.b.iconst(Type::I64, i128::from(v)).unwrap()
    }

    /// Ends the current block with `ret 0, status`.
    fn ret_status(&mut self, status: Value) {
        let zero = self.i64(0);
        self.b.ret(&[zero, status]).unwrap();
    }

    /// Seals `block` when sealing eagerly.
    fn seal(&mut self, block: Block) {
        if self.mode == VarMode::SsaEager {
            self.b.seal_block(block).unwrap();
        }
    }

    /// Emits `data` as a plain instruction, or as a `check` terminator when it
    /// can raise an error; returns the result.
    fn op(&mut self, data: InstData) -> Value {
        let can_error = match &data {
            InstData::Unary { op, policy, .. } => op.can_error(*policy),
            InstData::Binary { op, policy, args } => {
                let ty = self.b.func().value_type(args[0]).unwrap();
                op.can_error(*policy, ty)
            }
            InstData::Convert { op, policy, .. } => op.can_error(*policy),
            _ => false,
        };
        if !can_error {
            let inst = self.b.insert(data).unwrap();
            return self.b.results(inst).first().unwrap();
        }
        let ty = match &data {
            InstData::Convert { to, .. } => *to,
            _ => Type::I64,
        };
        let ok = self.b.create_block(&[ty]).unwrap();
        let err = self.b.create_block(&[Type::U32]).unwrap();
        self.b
            .check(
                data,
                BlockCall::with_args(ok, &[BlockArg::Result(0)]),
                BlockCall::with_args(err, &[BlockArg::ErrorCode]),
            )
            .unwrap();
        self.b.switch_to(err).unwrap();
        self.seal(err);
        let code = self.b.block_param(err, 0).unwrap();
        self.ret_status(code);
        self.b.switch_to(ok).unwrap();
        self.seal(ok);
        self.b.block_param(ok, 0).unwrap()
    }

    fn slot_addr(&mut self, base: Value, index: Value) -> Value {
        self.b.elem_addr(base, Type::I64.into(), index).unwrap()
    }

    fn expr(&mut self, e: &Expr) -> Value {
        match e {
            Expr::Lit(v) => self.i64(*v),
            Expr::Param(i) => self.b.param(*i).unwrap(),
            Expr::Var(i) => {
                if self.mode != VarMode::Memory {
                    return self.b.use_var(self.ssa[*i]).unwrap();
                }
                let idx = self.i64(*i as i64);
                let a = self.slot_addr(self.vars, idx);
                self.b.load(Type::I64, a, F8).unwrap()
            }
            Expr::Global(g) => {
                let a = self.b.global_addr(self.globals[*g]).unwrap();
                self.b.load(Type::I64, a, F8).unwrap()
            }
            Expr::Bin(op, p, a, b) => {
                let a = self.expr(a);
                let b = self.expr(b);
                self.op(InstData::Binary {
                    op: arith_op(*op),
                    policy: *p,
                    args: [a, b],
                })
            }
            Expr::Neg(o, a) => {
                let a = self.expr(a);
                self.op(InstData::Unary {
                    op: ir_lang::UnaryOp::Neg,
                    policy: Policy::overflow(*o),
                    arg: a,
                })
            }
            Expr::CastI32(o, a) => {
                let a = self.expr(a);
                let n = self.op(InstData::Convert {
                    op: ConvOp::IntCast,
                    policy: Policy::overflow(*o),
                    to: Type::I32,
                    arg: a,
                });
                self.b
                    .convert(ConvOp::Sext, Policy::NONE, n, Type::I64)
                    .unwrap()
            }
            Expr::LowByte(a) => {
                let a = self.expr(a);
                let n = self
                    .b
                    .convert(ConvOp::Narrow, Policy::NONE, a, Type::U8)
                    .unwrap();
                self.b
                    .convert(ConvOp::Zext, Policy::NONE, n, Type::I64)
                    .unwrap()
            }
            Expr::Float(op, p, a, b) => {
                let a = self.expr(a);
                let b = self.expr(b);
                let fa = self
                    .b
                    .convert(ConvOp::IntToFloat, Policy::NONE, a, Type::F64)
                    .unwrap();
                let fb = self
                    .b
                    .convert(ConvOp::IntToFloat, Policy::NONE, b, Type::F64)
                    .unwrap();
                let bop = match op {
                    FArith::Add => BinaryOp::Add,
                    FArith::Sub => BinaryOp::Sub,
                    FArith::Mul => BinaryOp::Mul,
                    FArith::Div => BinaryOp::Div,
                };
                let x = self.b.binary(bop, Policy::NONE, fa, fb).unwrap();
                self.op(InstData::Convert {
                    op: ConvOp::FloatToInt,
                    policy: Policy::float_to_int(*p),
                    to: Type::I64,
                    arg: x,
                })
            }
            Expr::Select(op, a, b, t, f) => {
                let a = self.expr(a);
                let b = self.expr(b);
                let t = self.expr(t);
                let f = self.expr(f);
                let c = self.b.compare(*op, a, b).unwrap();
                self.b.select(c, t, f).unwrap()
            }
            Expr::Load(i) => {
                let i = self.expr(i);
                let seven = self.i64(7);
                let i = self
                    .b
                    .binary(BinaryOp::And, Policy::NONE, i, seven)
                    .unwrap();
                let a = self.slot_addr(self.array, i);
                self.b.load(Type::I64, a, F8).unwrap()
            }
            Expr::Call {
                func,
                args,
                invoke,
                indirect,
            } => {
                let x = self.expr(&args[0]);
                let y = self.expr(&args[1]);
                let callee = self.funcs[*func];
                let call = if *indirect {
                    let addr = self.b.func_addr(callee).unwrap();
                    Call::indirect(self.sig, addr, &[x, y])
                } else {
                    Call::direct(callee, &[x, y])
                };
                let (value, status) = if *invoke {
                    let normal = self.b.create_block(&[Type::I64, Type::U32]).unwrap();
                    let pad = self.b.create_block(&[Type::Ptr]).unwrap();
                    self.b
                        .invoke(
                            call,
                            BlockCall::with_args(
                                normal,
                                &[BlockArg::Result(0), BlockArg::Result(1)],
                            ),
                            BlockCall::with_args(pad, &[BlockArg::Exception]),
                        )
                        .unwrap();
                    // Landing pad: status = 100 + payload.
                    self.b.switch_to(pad).unwrap();
                    self.seal(pad);
                    let exn = self.b.block_param(pad, 0).unwrap();
                    let k = self
                        .b
                        .convert(ConvOp::PtrToInt, Policy::NONE, exn, Type::U64)
                        .unwrap();
                    let k = self
                        .b
                        .convert(ConvOp::Narrow, Policy::NONE, k, Type::U32)
                        .unwrap();
                    let hundred = self.b.iconst(Type::U32, 100).unwrap();
                    let s = self.b.add(k, hundred, Overflow::Wrap).unwrap();
                    self.ret_status(s);
                    self.b.switch_to(normal).unwrap();
                    self.seal(normal);
                    (
                        self.b.block_param(normal, 0).unwrap(),
                        self.b.block_param(normal, 1).unwrap(),
                    )
                } else {
                    let inst = self.b.call(call).unwrap();
                    let r = self.b.results(inst);
                    (r.get(0).unwrap(), r.get(1).unwrap())
                };
                // A non-zero status propagates.
                let zero = self.b.iconst(Type::U32, 0).unwrap();
                let failed = self.b.compare(CmpOp::Ne, status, zero).unwrap();
                let fail = self.b.create_block(&[]).unwrap();
                let cont = self.b.create_block(&[]).unwrap();
                self.b.branch(failed, fail, &[], cont, &[]).unwrap();
                self.b.switch_to(fail).unwrap();
                self.seal(fail);
                self.ret_status(status);
                self.b.switch_to(cont).unwrap();
                self.seal(cont);
                value
            }
        }
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Set(v, e) if self.mode != VarMode::Memory => {
                let x = self.expr(e);
                self.b.def_var(self.ssa[*v], x).unwrap();
            }
            Stmt::Set(v, e) => {
                let x = self.expr(e);
                let idx = self.i64(*v as i64);
                let a = self.slot_addr(self.vars, idx);
                self.b.store(x, a, F8).unwrap();
            }
            Stmt::If(op, a, b, t, e) => {
                let a = self.expr(a);
                let b = self.expr(b);
                let c = self.b.compare(*op, a, b).unwrap();
                let tb = self.b.create_block(&[]).unwrap();
                let eb = self.b.create_block(&[]).unwrap();
                let join = self.b.create_block(&[]).unwrap();
                self.b.branch(c, tb, &[], eb, &[]).unwrap();
                self.b.switch_to(tb).unwrap();
                self.seal(tb);
                self.stmts(t);
                self.b.jump(join, &[]).unwrap();
                self.b.switch_to(eb).unwrap();
                self.seal(eb);
                self.stmts(e);
                self.b.jump(join, &[]).unwrap();
                self.b.switch_to(join).unwrap();
                self.seal(join);
            }
            Stmt::Loop(n, body) => {
                let head = self.b.create_block(&[Type::U32]).unwrap();
                let inner = self.b.create_block(&[]).unwrap();
                let exit = self.b.create_block(&[]).unwrap();
                let zero = self.b.iconst(Type::U32, 0).unwrap();
                self.b.jump(head, &[zero]).unwrap();
                self.b.switch_to(head).unwrap();
                let i = self.b.block_param(head, 0).unwrap();
                let limit = self.b.iconst(Type::U32, i128::from(*n)).unwrap();
                let more = self.b.compare(CmpOp::Lt, i, limit).unwrap();
                self.b.branch(more, inner, &[], exit, &[]).unwrap();
                self.b.switch_to(inner).unwrap();
                self.seal(inner);
                self.stmts(body);
                let one = self.b.iconst(Type::U32, 1).unwrap();
                let next = self.b.add(i, one, Overflow::Wrap).unwrap();
                self.b.jump(head, &[next]).unwrap();
                // Both edges into the loop head exist now.
                self.seal(head);
                self.b.switch_to(exit).unwrap();
                self.seal(exit);
            }
            Stmt::Store(i, v) => {
                let i = self.expr(i);
                let v = self.expr(v);
                let seven = self.i64(7);
                let i = self
                    .b
                    .binary(BinaryOp::And, Policy::NONE, i, seven)
                    .unwrap();
                let a = self.slot_addr(self.array, i);
                self.b.store(v, a, F8).unwrap();
            }
            Stmt::SetGlobal(g, e) => {
                let x = self.expr(e);
                let a = self.b.global_addr(self.globals[*g]).unwrap();
                self.b.store(x, a, F8).unwrap();
            }
            Stmt::Raise(k) => {
                let k = self.b.iconst(Type::U64, i128::from(*k)).unwrap();
                let p = self
                    .b
                    .convert(ConvOp::IntToPtr, Policy::NONE, k, Type::Ptr)
                    .unwrap();
                self.b.resume(p).unwrap();
                // Code after a raise is unreachable; give it a fresh block.
                let dead = self.b.create_block(&[]).unwrap();
                self.b.switch_to(dead).unwrap();
                self.seal(dead);
            }
            Stmt::Trap(c) => {
                self.b.trap(u32::from(*c)).unwrap();
                let dead = self.b.create_block(&[]).unwrap();
                self.b.switch_to(dead).unwrap();
                self.seal(dead);
            }
        }
    }
}

pub fn lower(prog: &Program) -> Lowered {
    lower_with(prog, VarMode::Memory)
}

/// Lowers with the program's variables kept as `mode` says.
pub fn lower_with(prog: &Program, mode: VarMode) -> Lowered {
    let mut module = Module::new("random");
    let sig = signature();
    let sig_id = module.intern_signature(&sig).unwrap();
    let globals: Vec<GlobalId> = (0..GLOBALS)
        .map(|i| {
            module
                .declare_global(&format!("g{i}"), Global::zeroed(8, 8).mutable())
                .unwrap()
        })
        .collect();
    let funcs: Vec<FuncId> = (0..prog.funcs.len())
        .map(|i| {
            module
                .declare_function(&format!("f{i}"), &sig, Linkage::Export)
                .unwrap()
        })
        .collect();
    for (i, def) in prog.funcs.iter().enumerate() {
        let mut b = module.build(funcs[i]).unwrap();
        let vars_slot: StackSlot = b.stack_slot((VARS * 8) as u32, 8).unwrap();
        let array_slot = b.stack_slot(64, 8).unwrap();
        let vars = b.stack_addr(vars_slot).unwrap();
        let array = b.stack_addr(array_slot).unwrap();
        // Initialize explicitly: the IR does not promise zeroed stack memory.
        let zero = b.iconst(Type::U8, 0).unwrap();
        let vlen = b.iconst(Type::U64, (VARS * 8) as i128).unwrap();
        b.memset(vars, zero, vlen, F8).unwrap();
        let alen = b.iconst(Type::U64, 64).unwrap();
        b.memset(array, zero, alen, F8).unwrap();
        let ssa: Vec<Variable> = if mode == VarMode::Memory {
            Vec::new()
        } else {
            let vs: Vec<Variable> = (0..VARS)
                .map(|_| b.declare_var(Type::I64).unwrap())
                .collect();
            // The evaluator starts every variable at 0. Define some explicitly;
            // the others rely on reads of an unassigned variable giving zero.
            for (k, &v) in vs.iter().enumerate() {
                if (i + k) % 2 == 0 {
                    let z = b.iconst(Type::I64, 0).unwrap();
                    b.def_var(v, z).unwrap();
                }
            }
            vs
        };
        let mut l = Lower {
            b: &mut b,
            funcs: &funcs,
            globals: &globals,
            sig: sig_id,
            vars,
            array,
            mode,
            ssa,
        };
        l.stmts(&def.body);
        match &def.tail {
            Tail::Ret(e) => {
                let v = l.expr(e);
                let ok = l.b.iconst(Type::U32, 0).unwrap();
                l.b.ret(&[v, ok]).unwrap();
            }
            Tail::TailCall(g, x, y) => {
                let x = l.expr(x);
                let y = l.expr(y);
                l.b.tail_call(Call::direct(funcs[*g], &[x, y])).unwrap();
            }
        }
        if mode != VarMode::Memory {
            b.seal_all_blocks().unwrap();
        }
    }
    Lowered {
        module,
        funcs,
        globals,
    }
}

/// Converts an interpreter outcome to the evaluator's form.
#[cfg(feature = "interp")]
pub fn outcome(o: ir_lang::interp::Outcome) -> Expect {
    use ir_lang::interp::Outcome;
    match o {
        Outcome::Return(vals) => Expect::Return(
            vals[0].as_i128().unwrap() as i64,
            vals[1].as_i128().unwrap() as u32,
        ),
        Outcome::Trap(c) => Expect::Trap(c),
        Outcome::Unwind(p) => Expect::Unwind(p.bits()),
        _ => Expect::Trap(u32::MAX),
    }
}
