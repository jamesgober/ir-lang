//! Benchmarks for the two hot paths: constructing IR with the [`Builder`] and
//! checking it with [`Function::validate`]. Both are measured on a straight-line
//! function and on a control-flow-heavy chain of diamonds, since the validator's
//! dominance pass scales with the shape of the graph, not just its size.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use ir_lang::{BinOp, Builder, Function, Type};
use std::hint::black_box;

/// Builds `fn(int) -> int` whose body is `n` chained arithmetic operations over a
/// single straight-line block.
fn build_straightline(n: usize) -> Function {
    let mut b = Builder::new("straightline", &[Type::Int], Type::Int);
    let mut acc = b.block_params(b.entry())[0];
    for i in 0..n {
        let k = b.iconst(i as i64);
        acc = b.bin(BinOp::Add, acc, k);
    }
    b.ret(Some(acc));
    b.finish()
}

/// Builds `fn(int) -> int` whose body is `n` nested `if`/`else` diamonds, each
/// threading its result through a join block — `3n + 1` blocks of control flow.
fn build_diamonds(n: usize) -> Function {
    let mut b = Builder::new("diamonds", &[Type::Int], Type::Int);
    let mut acc = b.block_params(b.entry())[0];
    for _ in 0..n {
        let zero = b.iconst(0);
        let cond = b.bin(BinOp::Gt, acc, zero);
        let join = b.create_block(&[Type::Int]);
        let then_blk = b.create_block(&[]);
        let else_blk = b.create_block(&[]);
        b.branch(cond, then_blk, &[], else_blk, &[]);

        b.switch_to(then_blk);
        let one = b.iconst(1);
        let inc = b.bin(BinOp::Add, acc, one);
        b.jump(join, &[inc]);

        b.switch_to(else_blk);
        let neg = b.bin(BinOp::Sub, acc, acc);
        b.jump(join, &[neg]);

        b.switch_to(join);
        acc = b.block_params(join)[0];
    }
    b.ret(Some(acc));
    b.finish()
}

fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("build");
    for &n in &[16usize, 256, 1024] {
        group.bench_with_input(BenchmarkId::new("straightline", n), &n, |bench, &n| {
            bench.iter(|| build_straightline(black_box(n)));
        });
        group.bench_with_input(BenchmarkId::new("diamonds", n), &n, |bench, &n| {
            bench.iter(|| build_diamonds(black_box(n)));
        });
    }
    group.finish();
}

fn bench_validate(c: &mut Criterion) {
    let mut group = c.benchmark_group("validate");
    for &n in &[16usize, 256, 1024] {
        let straight = build_straightline(n);
        group.bench_with_input(
            BenchmarkId::new("straightline", n),
            &straight,
            |bench, f| {
                bench.iter(|| black_box(f).validate());
            },
        );
        let diamonds = build_diamonds(n);
        group.bench_with_input(BenchmarkId::new("diamonds", n), &diamonds, |bench, f| {
            bench.iter(|| black_box(f).validate());
        });
    }
    group.finish();
}

criterion_group!(benches, bench_build, bench_validate);
criterion_main!(benches);
