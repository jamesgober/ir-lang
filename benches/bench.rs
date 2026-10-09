//! Benchmarks at realistic scale (100k and 1M instructions): building,
//! validating, printing, parsing, and compacting; plus the safepoint check with
//! many relocated references.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use ir_lang::{Call, CmpOp, FuncId, Linkage, Module, Overflow, Signature, Type};

/// One block of `n` dependent additions.
fn straight_line(n: usize) -> (Module, FuncId) {
    let mut m = Module::new("bench");
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[Type::I64], &[Type::I64]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let mut acc = b.param(0).unwrap();
    let one = b.iconst(Type::I64, 1).unwrap();
    for _ in 0..n {
        acc = b.add(acc, one, Overflow::Wrap).unwrap();
    }
    b.ret(&[acc]).unwrap();
    (m, f)
}

/// About `n` instructions in a chain of diamonds (a branch, two arms, a join with
/// a block parameter), so the dominator tree and the CFG are large too.
fn diamonds(n: usize) -> (Module, FuncId) {
    let mut m = Module::new("bench");
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[Type::I64], &[Type::I64]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let mut acc = b.param(0).unwrap();
    let mut emitted = 0;
    while emitted < n {
        let zero = b.iconst(Type::I64, 0).unwrap();
        let c = b.compare(CmpOp::Lt, acc, zero).unwrap();
        let t = b.create_block(&[]).unwrap();
        let e = b.create_block(&[]).unwrap();
        let j = b.create_block(&[Type::I64]).unwrap();
        b.branch(c, t, &[], e, &[]).unwrap();
        b.switch_to(t).unwrap();
        let x = b.sub(zero, acc, Overflow::Wrap).unwrap();
        b.jump(j, &[x]).unwrap();
        b.switch_to(e).unwrap();
        let y = b.add(acc, acc, Overflow::Wrap).unwrap();
        b.jump(j, &[y]).unwrap();
        b.switch_to(j).unwrap();
        acc = b.block_param(j, 0).unwrap();
        emitted += 4;
    }
    b.ret(&[acc]).unwrap();
    (m, f)
}

/// `n` calls that are safepoints, each relocating the same reference.
fn safepoints(n: usize) -> Module {
    let mut m = Module::new("bench");
    let poll = m
        .declare_function("poll", &Signature::new(&[], &[]), Linkage::Import)
        .unwrap();
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[Type::Ref], &[Type::Ref]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let mut r = b.param(0).unwrap();
    for _ in 0..n {
        let c = b.call(Call::direct(poll, &[]).with_gc(&[r])).unwrap();
        r = b.results(c).first().unwrap();
    }
    b.ret(&[r]).unwrap();
    m
}

fn bench(c: &mut Criterion) {
    for &n in &[100_000usize, 1_000_000] {
        let mut g = c.benchmark_group(format!("ir-{n}"));
        g.sample_size(10);
        g.throughput(Throughput::Elements(n as u64));

        g.bench_function(BenchmarkId::new("build", "straight"), |bch| {
            bch.iter(|| black_box(straight_line(n)))
        });
        g.bench_function(BenchmarkId::new("build", "diamonds"), |bch| {
            bch.iter(|| black_box(diamonds(n)))
        });

        let (straight, _) = straight_line(n);
        let (cfg, f) = diamonds(n);
        g.bench_function(BenchmarkId::new("validate", "straight"), |bch| {
            bch.iter(|| straight.validate().unwrap())
        });
        g.bench_function(BenchmarkId::new("validate", "diamonds"), |bch| {
            bch.iter(|| cfg.validate().unwrap())
        });

        let text = cfg.to_string();
        g.bench_function(BenchmarkId::new("print", "diamonds"), |bch| {
            bch.iter(|| black_box(cfg.to_string()))
        });
        g.bench_function(BenchmarkId::new("parse", "diamonds"), |bch| {
            bch.iter(|| black_box(ir_lang::parse(&text).unwrap()))
        });
        g.bench_function(BenchmarkId::new("compact", "diamonds"), |bch| {
            bch.iter_batched(
                || cfg.clone(),
                |mut m| {
                    let mut b = m.edit(f).unwrap();
                    black_box(b.compact());
                },
                criterion::BatchSize::LargeInput,
            )
        });
        g.finish();
    }

    let mut g = c.benchmark_group("safepoints-100000");
    g.sample_size(10);
    g.throughput(Throughput::Elements(100_000));
    let m = safepoints(100_000);
    g.bench_function("validate", |bch| bch.iter(|| m.validate().unwrap()));
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
