//! Statepoint insertion (`Builder::insert_statepoints`), with the validator as
//! the oracle: random programs keep GC references in variables and use them
//! across allocations, polls, explicit safepoints, and invokes without any
//! relocation; after insertion they validate, and the interpreter (whose moving
//! collector simulation rejects any stale reference) computes what an
//! independent evaluator computes. Plus the hand-built edge cases.

// The evaluator serves the interpreter checks.
#![cfg_attr(not(feature = "interp"), allow(dead_code))]

use ir_lang::{
    Attrs, BlockArg, BlockCall, BuildError, Call, CmpOp, ConvOp, FuncId, InstData, Linkage,
    MemFlags, Module, Overflow, Policy, Signature, Terminator, Type, ValidationErrorKind, Variable,
};
use proptest::prelude::*;

const REFS: usize = 3;
const INTS: usize = 2;
const F8: MemFlags = MemFlags {
    align: 8,
    volatile: false,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

#[derive(Clone, Debug)]
enum G {
    /// `r = alloc(); r.f0 = k`
    Alloc(usize, i64),
    /// `r = s`
    Copy(usize, usize),
    /// `r.f0 = i`
    Store(usize, usize),
    /// `i = i + r.f0`
    Load(usize, usize),
    /// A call to `poll` (a safepoint).
    Poll,
    /// A call to `poll` whose GC list the front end already filled with every
    /// reference variable, reading the relocated values back.
    PollRelocated,
    /// An explicit `safepoint` with an empty list.
    Safepoint,
    /// `i = may_throw(j)`, or `i = -1` if it throws (an `invoke`).
    Invoke(usize, usize),
    /// A call to a `nogc` function (not a safepoint): `i = i + 1`.
    NoGc(usize),
    /// `if i < j { .. } else { .. }`
    If(usize, usize, Vec<G>, Vec<G>),
    /// `n` iterations.
    Loop(u8, Vec<G>),
}

fn gen_stmts(rng: &mut Rng, depth: u32, budget: &mut u32) -> Vec<G> {
    let n = rng.below(5) + 1;
    let mut out = Vec::new();
    for _ in 0..n {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let r = rng.below(REFS as u64) as usize;
        let s = rng.below(REFS as u64) as usize;
        let i = rng.below(INTS as u64) as usize;
        let j = rng.below(INTS as u64) as usize;
        out.push(match rng.below(13) {
            0 => G::Alloc(r, (rng.next() as i64) >> 40),
            1 => G::Copy(r, s),
            2 => G::Store(r, i),
            3 | 4 => G::Load(i, r),
            5 => G::Poll,
            6 => G::PollRelocated,
            7 => G::Safepoint,
            8 => G::Invoke(i, j),
            9 => G::NoGc(i),
            10 if depth > 0 => G::If(
                i,
                j,
                gen_stmts(rng, depth - 1, budget),
                gen_stmts(rng, depth - 1, budget),
            ),
            11 if depth > 0 => G::Loop(rng.below(4) as u8, gen_stmts(rng, depth - 1, budget)),
            _ => G::Load(i, r),
        });
    }
    out
}

fn generate(seed: u64) -> Vec<G> {
    let mut rng = Rng(seed | 1);
    let mut budget = 40;
    gen_stmts(&mut rng, 3, &mut budget)
}

// ------------------------------------------------------------- evaluation

fn may_throw(x: i64) -> Option<i64> {
    if x & 3 == 0 { None } else { Some(x >> 1) }
}

struct Eval {
    objects: Vec<i64>,
    refs: [usize; REFS],
    ints: [i64; INTS],
}

impl Eval {
    fn run(prog: &[G], seed: i64) -> i64 {
        let mut e = Eval {
            objects: (0..REFS as i64).map(|k| k + 100).collect(),
            refs: [0, 1, 2],
            ints: [seed, 0],
        };
        e.stmts(prog);
        let mut sum = e.ints[0].wrapping_add(e.ints[1]);
        for k in 0..REFS {
            sum = sum.wrapping_add(e.objects[e.refs[k]]);
        }
        sum
    }

    fn stmts(&mut self, prog: &[G]) {
        for g in prog {
            match g {
                G::Alloc(r, k) => {
                    self.objects.push(*k);
                    self.refs[*r] = self.objects.len() - 1;
                }
                G::Copy(r, s) => self.refs[*r] = self.refs[*s],
                G::Store(r, i) => self.objects[self.refs[*r]] = self.ints[*i],
                G::Load(i, r) => {
                    self.ints[*i] = self.ints[*i].wrapping_add(self.objects[self.refs[*r]]);
                }
                G::Poll | G::PollRelocated | G::Safepoint => {}
                G::Invoke(i, j) => self.ints[*i] = may_throw(self.ints[*j]).unwrap_or(-1),
                G::NoGc(i) => self.ints[*i] = self.ints[*i].wrapping_add(1),
                G::If(i, j, t, e) => {
                    if self.ints[*i] < self.ints[*j] {
                        self.stmts(t);
                    } else {
                        self.stmts(e);
                    }
                }
                G::Loop(n, body) => {
                    for _ in 0..*n {
                        self.stmts(body);
                    }
                }
            }
        }
    }
}

// --------------------------------------------------------------- lowering

struct Decls {
    alloc: FuncId,
    poll: FuncId,
    may_throw: FuncId,
    bump: FuncId,
    f: FuncId,
}

fn declare(m: &mut Module) -> Decls {
    let alloc = m
        .declare_function(
            "alloc",
            &Signature::new(&[Type::U64], &[Type::Ref]),
            Linkage::Import,
        )
        .unwrap();
    let poll = m
        .declare_function("poll", &Signature::new(&[], &[]), Linkage::Import)
        .unwrap();
    let may_throw = m
        .declare_function(
            "may_throw",
            &Signature::new(&[Type::I64], &[Type::I64]),
            Linkage::Import,
        )
        .unwrap();
    let bump = m
        .declare_function(
            "bump",
            &Signature::new(&[Type::I64], &[Type::I64]),
            Linkage::Import,
        )
        .unwrap();
    m.set_attrs(bump, Attrs::NOGC | Attrs::NOUNWIND).unwrap();
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[Type::I64], &[Type::I64]),
            Linkage::Export,
        )
        .unwrap();
    Decls {
        alloc,
        poll,
        may_throw,
        bump,
        f,
    }
}

struct Lower<'b, 'm> {
    b: &'b mut ir_lang::Builder<'m>,
    d: &'b Decls,
    refs: [Variable; REFS],
    ints: [Variable; INTS],
}

impl Lower<'_, '_> {
    fn new_object(&mut self, k: i64) -> ir_lang::Value {
        let size = self.b.iconst(Type::U64, 16).unwrap();
        let c = self.b.call(Call::direct(self.d.alloc, &[size])).unwrap();
        let r = self.b.results(c).first().unwrap();
        let p = self
            .b
            .convert(ConvOp::RefToPtr, Policy::NONE, r, Type::Ptr)
            .unwrap();
        let v = self.b.iconst(Type::I64, i128::from(k)).unwrap();
        self.b.store(v, p, F8).unwrap();
        r
    }

    fn field(&mut self, r: usize) -> ir_lang::Value {
        let rv = self.b.use_var(self.refs[r]).unwrap();
        self.b
            .convert(ConvOp::RefToPtr, Policy::NONE, rv, Type::Ptr)
            .unwrap()
    }

    fn stmts(&mut self, prog: &[G]) {
        for g in prog {
            self.stmt(g);
        }
    }

    fn stmt(&mut self, g: &G) {
        let b = &mut *self.b;
        match g {
            G::Alloc(r, k) => {
                let v = self.new_object(*k);
                self.b.def_var(self.refs[*r], v).unwrap();
            }
            G::Copy(r, s) => {
                let v = b.use_var(self.refs[*s]).unwrap();
                b.def_var(self.refs[*r], v).unwrap();
            }
            G::Store(r, i) => {
                let p = self.field(*r);
                let v = self.b.use_var(self.ints[*i]).unwrap();
                self.b.store(v, p, F8).unwrap();
            }
            G::Load(i, r) => {
                let p = self.field(*r);
                let x = self.b.load(Type::I64, p, F8).unwrap();
                let a = self.b.use_var(self.ints[*i]).unwrap();
                let s = self.b.add(a, x, Overflow::Wrap).unwrap();
                self.b.def_var(self.ints[*i], s).unwrap();
            }
            G::Poll => {
                b.call(Call::direct(self.d.poll, &[])).unwrap();
            }
            G::PollRelocated => {
                let vals: Vec<_> = self.refs.iter().map(|&v| b.use_var(v).unwrap()).collect();
                let c = b
                    .call(Call::direct(self.d.poll, &[]).with_gc(&vals))
                    .unwrap();
                let res = b.results(c);
                for (k, &var) in self.refs.iter().enumerate() {
                    b.def_var(var, res.get(k).unwrap()).unwrap();
                }
            }
            G::Safepoint => {
                b.safepoint(&[]).unwrap();
            }
            G::Invoke(i, j) => {
                let x = b.use_var(self.ints[*j]).unwrap();
                let normal = b.create_block(&[Type::I64]).unwrap();
                let pad = b.create_block(&[Type::Ptr]).unwrap();
                let join = b.create_block(&[]).unwrap();
                b.invoke(
                    Call::direct(self.d.may_throw, &[x]),
                    BlockCall::with_args(normal, &[BlockArg::Result(0)]),
                    BlockCall::with_args(pad, &[BlockArg::Exception]),
                )
                .unwrap();
                b.switch_to(normal).unwrap();
                b.seal_block(normal).unwrap();
                let v = b.block_param(normal, 0).unwrap();
                b.def_var(self.ints[*i], v).unwrap();
                b.jump(join, &[]).unwrap();
                b.switch_to(pad).unwrap();
                b.seal_block(pad).unwrap();
                let m1 = b.iconst(Type::I64, -1).unwrap();
                b.def_var(self.ints[*i], m1).unwrap();
                b.jump(join, &[]).unwrap();
                b.switch_to(join).unwrap();
                b.seal_block(join).unwrap();
            }
            G::NoGc(i) => {
                let x = b.use_var(self.ints[*i]).unwrap();
                let c = b.call(Call::direct(self.d.bump, &[x])).unwrap();
                let y = b.results(c).first().unwrap();
                b.def_var(self.ints[*i], y).unwrap();
            }
            G::If(i, j, t, e) => {
                let (x, y) = (
                    b.use_var(self.ints[*i]).unwrap(),
                    b.use_var(self.ints[*j]).unwrap(),
                );
                let c = b.compare(CmpOp::Lt, x, y).unwrap();
                let (tb, eb, join) = (
                    b.create_block(&[]).unwrap(),
                    b.create_block(&[]).unwrap(),
                    b.create_block(&[]).unwrap(),
                );
                b.branch(c, tb, &[], eb, &[]).unwrap();
                b.switch_to(tb).unwrap();
                b.seal_block(tb).unwrap();
                self.stmts(t);
                self.b.jump(join, &[]).unwrap();
                self.b.switch_to(eb).unwrap();
                self.b.seal_block(eb).unwrap();
                self.stmts(e);
                self.b.jump(join, &[]).unwrap();
                self.b.switch_to(join).unwrap();
                self.b.seal_block(join).unwrap();
            }
            G::Loop(n, body) => {
                let head = b.create_block(&[Type::U32]).unwrap();
                let inner = b.create_block(&[]).unwrap();
                let exit = b.create_block(&[]).unwrap();
                let zero = b.iconst(Type::U32, 0).unwrap();
                b.jump(head, &[zero]).unwrap();
                b.switch_to(head).unwrap();
                let k = b.block_param(head, 0).unwrap();
                let lim = b.iconst(Type::U32, i128::from(*n)).unwrap();
                let more = b.compare(CmpOp::Lt, k, lim).unwrap();
                b.branch(more, inner, &[], exit, &[]).unwrap();
                b.switch_to(inner).unwrap();
                b.seal_block(inner).unwrap();
                self.stmts(body);
                let one = self.b.iconst(Type::U32, 1).unwrap();
                let k2 = self.b.add(k, one, Overflow::Wrap).unwrap();
                self.b.jump(head, &[k2]).unwrap();
                self.b.seal_block(head).unwrap();
                self.b.switch_to(exit).unwrap();
                self.b.seal_block(exit).unwrap();
            }
        }
    }
}

/// Lowers `prog`: references kept in variables and used freely across
/// safepoints, exactly as a front end would before statepoint insertion.
fn lower(prog: &[G]) -> (Module, Decls) {
    let mut m = Module::new("gc");
    let d = declare(&mut m);
    {
        let mut b = m.build(d.f).unwrap();
        let refs = [(); REFS].map(|()| b.declare_var(Type::Ref).unwrap());
        let ints = [(); INTS].map(|()| b.declare_var(Type::I64).unwrap());
        let mut l = Lower {
            b: &mut b,
            d: &d,
            refs,
            ints,
        };
        for (k, &var) in refs.iter().enumerate() {
            let r = l.new_object(k as i64 + 100);
            l.b.def_var(var, r).unwrap();
        }
        let seed = l.b.param(0).unwrap();
        l.b.def_var(ints[0], seed).unwrap();
        let zero = l.b.iconst(Type::I64, 0).unwrap();
        l.b.def_var(ints[1], zero).unwrap();
        l.stmts(prog);
        let mut sum = {
            let a = l.b.use_var(ints[0]).unwrap();
            let c = l.b.use_var(ints[1]).unwrap();
            l.b.add(a, c, Overflow::Wrap).unwrap()
        };
        for k in 0..REFS {
            let p = l.field(k);
            let x = l.b.load(Type::I64, p, F8).unwrap();
            sum = l.b.add(sum, x, Overflow::Wrap).unwrap();
        }
        l.b.ret(&[sum]).unwrap();
        b.seal_all_blocks().unwrap();
    }
    (m, d)
}

#[cfg(feature = "interp")]
mod run {
    use super::*;
    use ir_lang::interp::{Host, HostReturn, InterpError, Interpreter, Memory, Outcome, Val};

    pub struct Gc;

    impl Host for Gc {
        fn call(
            &mut self,
            name: &str,
            args: &[Val],
            mem: &mut Memory,
        ) -> Result<HostReturn, InterpError> {
            match name {
                "alloc" => Ok(HostReturn::Values(vec![mem.alloc_object(args[0].bits())?])),
                "poll" => Ok(HostReturn::Values(vec![])),
                "bump" => Ok(HostReturn::Values(vec![Val::i64(
                    (args[0].bits() as i64).wrapping_add(1),
                )])),
                "may_throw" => match may_throw(args[0].bits() as i64) {
                    Some(v) => Ok(HostReturn::Values(vec![Val::i64(v)])),
                    None => Ok(HostReturn::Unwind(Val::ptr(8))),
                },
                _ => Err(InterpError::UnresolvedImport { name: name.into() }),
            }
        }
    }

    pub fn exec(m: &Module, f: FuncId, seed: i64) -> Result<Outcome, InterpError> {
        Interpreter::new(m, Gc)?.call(f, &[Val::i64(seed)])
    }
}

fn insert(m: &mut Module, f: FuncId) -> Result<ir_lang::StatepointStats, BuildError> {
    let mut b = m.edit(f).unwrap();
    b.insert_statepoints()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Invariant: after statepoint insertion every random GC-reference program
    /// validates, a second insertion changes nothing, the text round-trips, and
    /// (with the interpreter) it computes what the evaluator computes, with no
    /// stale reference.
    #[test]
    fn prop_statepoints_make_gc_programs_valid_and_correct(seed in any::<u64>(), arg in -40i64..40) {
        let prog = generate(seed);
        let (mut m, d) = lower(&prog);
        // Before insertion only the safepoint rule may fail.
        if let Err(e) = m.validate() {
            prop_assert!(
                matches!(e.kind(), ValidationErrorKind::LiveAcrossSafepoint { .. }),
                "{e}\n{m}"
            );
        }
        let stats = insert(&mut m, d.f);
        prop_assert!(stats.is_ok(), "{stats:?}\n{m}");
        prop_assert!(m.validate().is_ok(), "{:?}\n{m}", m.validate());
        let text = m.to_string();
        let again = insert(&mut m, d.f).unwrap();
        prop_assert_eq!((again.safepoints, again.relocations, again.params), (0, 0, 0));
        prop_assert_eq!(m.to_string(), text.clone());
        let parsed = ir_lang::parse(&text).unwrap();
        prop_assert_eq!(parsed.to_string(), text);
        #[cfg(feature = "interp")]
        {
            let want = Eval::run(&prog, arg);
            let got = run::exec(&m, d.f, arg);
            prop_assert_eq!(
                got,
                Ok(ir_lang::interp::Outcome::Return(vec![ir_lang::interp::Val::i64(want)])),
                "\n{}",
                m
            );
        }
        let _ = arg;
    }
}

#[cfg(feature = "interp")]
#[test]
fn test_unrelocated_programs_fail_at_run_time_before_insertion() {
    // The interpreter's moving-collector simulation is the run-time oracle: the
    // same program without insertion uses stale references.
    let prog = vec![G::Poll, G::Load(0, 1)];
    let (mut m, d) = lower(&prog);
    assert!(m.validate().is_err());
    let stats = insert(&mut m, d.f).unwrap();
    // The poll, and the two allocations after the first (each a safepoint).
    assert_eq!(stats.safepoints, 3);
    m.validate().unwrap();
    let want = Eval::run(&prog, 5);
    assert_eq!(
        run::exec(&m, d.f, 5),
        Ok(ir_lang::interp::Outcome::Return(vec![
            ir_lang::interp::Val::i64(want)
        ]))
    );
}

fn simple(sig: Signature) -> (Module, FuncId, FuncId) {
    let mut m = Module::new("sp");
    let poll = m
        .declare_function("poll", &Signature::new(&[], &[]), Linkage::Import)
        .unwrap();
    let f = m.declare_function("f", &sig, Linkage::Export).unwrap();
    (m, poll, f)
}

#[test]
fn test_existing_entries_are_reused_and_lists_extended() {
    let (mut m, poll, f) = simple(Signature::new(
        &[Type::Ref, Type::Ref],
        &[Type::Ref, Type::Ref],
    ));
    let mut b = m.build(f).unwrap();
    let (r, s) = (b.param(0).unwrap(), b.param(1).unwrap());
    // The front end relocated `r` but forgot `s`.
    let c = b.call(Call::direct(poll, &[]).with_gc(&[r])).unwrap();
    let r2 = b.results(c).first().unwrap();
    b.ret(&[r2, s]).unwrap();
    let stats = b.insert_statepoints().unwrap();
    assert_eq!((stats.safepoints, stats.relocations), (1, 1));
    b.validate().unwrap();
    let entry = b.func().entry();
    let call = b.func().insts(entry).next().unwrap();
    match b.func().inst(call) {
        Some(InstData::Call(c)) => assert_eq!(c.gc, [r, s]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn test_invoke_relocates_through_both_edges() {
    let mut m = Module::new("sp");
    let thrower = m
        .declare_function(
            "thrower",
            &Signature::new(&[], &[Type::I64]),
            Linkage::Import,
        )
        .unwrap();
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[Type::Ref], &[Type::Ref]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let r = b.param(0).unwrap();
    let normal = b.create_block(&[Type::I64]).unwrap();
    let pad = b.create_block(&[Type::Ptr]).unwrap();
    let join = b.create_block(&[Type::Ref]).unwrap();
    b.invoke(
        Call::direct(thrower, &[]),
        BlockCall::with_args(normal, &[BlockArg::Result(0)]),
        BlockCall::with_args(pad, &[BlockArg::Exception]),
    )
    .unwrap();
    b.switch_to(normal).unwrap();
    b.jump(join, &[r]).unwrap();
    b.switch_to(pad).unwrap();
    b.jump(join, &[r]).unwrap();
    b.switch_to(join).unwrap();
    let j = b.block_param(join, 0).unwrap();
    b.ret(&[j]).unwrap();
    assert!(b.validate().is_err());
    let stats = b.insert_statepoints().unwrap();
    assert_eq!(
        (stats.safepoints, stats.relocations, stats.params),
        (1, 1, 2)
    );
    b.validate().unwrap();
    let text = m.to_string();
    assert!(
        text.contains("to b1(res0, res1) unwind b2(exn, res1)"),
        "{text}"
    );
}

#[test]
fn test_loop_carried_reference_gets_a_header_parameter() {
    let (mut m, poll, f) = simple(Signature::new(&[Type::Ref, Type::Bool], &[Type::Ref]));
    let mut b = m.build(f).unwrap();
    let (r, c) = (b.param(0).unwrap(), b.param(1).unwrap());
    let head = b.create_block(&[]).unwrap();
    let exit = b.create_block(&[]).unwrap();
    b.jump(head, &[]).unwrap();
    b.switch_to(head).unwrap();
    b.call(Call::direct(poll, &[])).unwrap();
    b.branch(c, head, &[], exit, &[]).unwrap();
    b.switch_to(exit).unwrap();
    b.ret(&[r]).unwrap();
    let stats = b.insert_statepoints().unwrap();
    assert_eq!(
        (stats.safepoints, stats.relocations, stats.params),
        (1, 1, 1)
    );
    b.validate().unwrap();
    assert_eq!(b.func().block_params(head).len(), 1);
}

#[test]
fn test_nogc_calls_and_functions_without_references_are_untouched() {
    let (mut m, poll, f) = simple(Signature::new(
        &[Type::Ref, Type::I32],
        &[Type::Ref, Type::I32],
    ));
    m.set_attrs(poll, Attrs::NOGC).unwrap();
    let mut b = m.build(f).unwrap();
    let (r, x) = (b.param(0).unwrap(), b.param(1).unwrap());
    b.call(Call::direct(poll, &[])).unwrap();
    b.ret(&[r, x]).unwrap();
    let before = m.to_string();
    let stats = m.edit(f).unwrap().insert_statepoints().unwrap();
    assert_eq!(stats, ir_lang::StatepointStats::default());
    assert_eq!(m.to_string(), before);
}

#[test]
fn test_derived_pointer_across_a_safepoint_is_refused_unchanged() {
    let (mut m, poll, f) = simple(Signature::new(&[Type::Ref], &[Type::I64]));
    let mut b = m.build(f).unwrap();
    let r = b.param(0).unwrap();
    let p = b
        .convert(ConvOp::RefToPtr, Policy::NONE, r, Type::Ptr)
        .unwrap();
    b.call(Call::direct(poll, &[])).unwrap();
    let v = b.load(Type::I64, p, F8).unwrap();
    b.ret(&[v]).unwrap();
    let before = m.to_string();
    let err = m.edit(f).unwrap().insert_statepoints().unwrap_err();
    assert!(
        matches!(
            err,
            BuildError::Invalid(ValidationErrorKind::LiveAcrossSafepoint { value }) if value == p
        ),
        "{err:?}"
    );
    assert_eq!(m.to_string(), before);
}

#[test]
fn test_invalid_input_is_refused_unchanged() {
    let (mut m, poll, f) = simple(Signature::new(&[Type::Ref], &[Type::Ref]));
    let mut b = m.build(f).unwrap();
    let r = b.param(0).unwrap();
    b.call(Call::direct(poll, &[])).unwrap();
    let dangling = b.create_block(&[]).unwrap(); // no terminator
    b.ret(&[r]).unwrap();
    let _ = dangling;
    let before = m.to_string();
    let err = m.edit(f).unwrap().insert_statepoints().unwrap_err();
    assert_eq!(
        err,
        BuildError::Invalid(ValidationErrorKind::MissingTerminator)
    );
    assert_eq!(m.to_string(), before);
}

#[test]
fn test_many_references_across_many_safepoints_scale() {
    // 200 references each used after 500 polls: 100,000 relocations, one block.
    let (mut m, poll, f) = simple(Signature::new(&[Type::Ref], &[Type::Ref]));
    let mut b = m.build(f).unwrap();
    let r = b.param(0).unwrap();
    let null = b.null(Type::Ref).unwrap();
    let mut refs = vec![r];
    for _ in 1..200 {
        let c = b.compare(CmpOp::Eq, r, null).unwrap();
        let s = b.select(c, null, r).unwrap();
        refs.push(s);
    }
    for _ in 0..500 {
        b.call(Call::direct(poll, &[])).unwrap();
    }
    let mut acc = refs[0];
    for &s in &refs[1..] {
        let c = b.compare(CmpOp::Eq, acc, s).unwrap();
        acc = b.select(c, acc, s).unwrap();
    }
    b.ret(&[acc]).unwrap();
    let stats = b.insert_statepoints().unwrap();
    assert_eq!(stats.relocations, 200 * 500);
    b.validate().unwrap();
    assert!(matches!(
        b.func().terminator(b.func().entry()),
        Some(Terminator::Return { .. })
    ));
}

#[test]
fn test_budget_covers_relocations_and_parameters_and_refusal_changes_nothing() {
    // One relocation in a loop needs one header parameter: a budget of one
    // (the relocation) is refused, a budget of two succeeds.
    let (mut m, poll, f) = simple(Signature::new(&[Type::Ref, Type::Bool], &[Type::Ref]));
    let mut b = m.build(f).unwrap();
    let (r, c) = (b.param(0).unwrap(), b.param(1).unwrap());
    let head = b.create_block(&[]).unwrap();
    let exit = b.create_block(&[]).unwrap();
    b.jump(head, &[]).unwrap();
    b.switch_to(head).unwrap();
    b.call(Call::direct(poll, &[])).unwrap();
    b.branch(c, head, &[], exit, &[]).unwrap();
    b.switch_to(exit).unwrap();
    b.ret(&[r]).unwrap();
    let before = m.to_string();
    let mut b = m.edit(f).unwrap();
    assert_eq!(
        b.insert_statepoints_within(0),
        Err(BuildError::BudgetExceeded)
    );
    assert_eq!(
        b.insert_statepoints_within(1),
        Err(BuildError::BudgetExceeded)
    );
    assert_eq!(m.to_string(), before);
    let mut b = m.edit(f).unwrap();
    let stats = b.insert_statepoints_within(2).unwrap();
    assert_eq!((stats.relocations, stats.params), (1, 1));
}
