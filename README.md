<h1 align="center">
    <img width="99" alt="Rust logo" src="https://raw.githubusercontent.com/jamesgober/rust-collection/72baabd71f00e14aa9184efcb16fa3deddda3a0a/assets/rust-logo.svg">
    <br>
    <b>ir-lang</b>
    <br>
    <sub><sup>INTERMEDIATE REPRESENTATION</sup></sub>
</h1>

<div align="center">
    <a href="https://crates.io/crates/ir-lang"><img alt="Crates.io" src="https://img.shields.io/crates/v/ir-lang"></a>
    <a href="https://crates.io/crates/ir-lang"><img alt="Downloads" src="https://img.shields.io/crates/d/ir-lang?color=%230099ff"></a>
    <a href="https://docs.rs/ir-lang"><img alt="docs.rs" src="https://img.shields.io/docsrs/ir-lang"></a>
    <a href="https://github.com/jamesgober/ir-lang/actions"><img alt="CI" src="https://github.com/jamesgober/ir-lang/actions/workflows/ci.yml/badge.svg"></a>
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.85%2B-blue"></a>
</div>

<br>

<div align="left">
    <p>
        ir-lang is the intermediate representation a compiler optimizes and lowers code through. A <code>Function</code> is a control-flow graph of basic blocks in SSA form: each block is a straight-line run of value-producing instructions ended by one terminator, and every value is named by a small handle and defined exactly once. There is no AST type to lower <em>from</em> &mdash; a language brings its own syntax tree &mdash; so lowering is expressed through a <code>Builder</code>, the same shape as Cranelift's <code>FunctionBuilder</code> or LLVM's <code>IRBuilder</code>. It is a SEMA-tier crate of the <code>-lang</code> language-construction family.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition).
    </p>
    <blockquote>
        <strong>Status: stable (1.0).</strong> The public API is frozen and follows Semantic Versioning &mdash; no breaking changes before <code>2.0</code>. See <a href="./docs/API.md#semver-promise"><code>the SemVer promise</code></a> and <a href="./CHANGELOG.md"><code>CHANGELOG.md</code></a>.
    </blockquote>
</div>

<hr>
<br>

## Installation

```toml
[dependencies]
ir-lang = "1.0"
```

Or from the terminal:

```bash
cargo add ir-lang
```

<br>

## Usage

A front-end walks its own syntax tree and drives the `Builder`: a literal becomes a
constant, an operator becomes an instruction, an `if` becomes two blocks and a
branch, a join becomes a block parameter. The builder hands back a `Value` for every
result, so SSA numbering is captured without tracking it by hand. When the function
is built, `validate` confirms it is well-formed.

Lowering `fn abs(x: int) -> int { if x < 0 { -x } else { x } }`:

```rust
use ir_lang::{Builder, BinOp, Type, UnOp};

let mut b = Builder::new("abs", &[Type::Int], Type::Int);
let x = b.block_params(b.entry())[0];

// The merge point takes the result as a parameter.
let join = b.create_block(&[Type::Int]);
let neg_blk = b.create_block(&[]);
let pos_blk = b.create_block(&[]);

let zero = b.iconst(0);
let is_neg = b.bin(BinOp::Lt, x, zero);
b.branch(is_neg, neg_blk, &[], pos_blk, &[]);

b.switch_to(neg_blk);
let negated = b.un(UnOp::Neg, x);
b.jump(join, &[negated]);

b.switch_to(pos_blk);
b.jump(join, &[x]);

b.switch_to(join);
let result = b.block_params(join)[0];
b.ret(Some(result));

let func = b.finish();
assert!(func.validate().is_ok());
```

A function prints as a readable textual IR, which is handy in tests and when
debugging a pass:

```rust
use ir_lang::{Builder, BinOp, Type};

let mut b = Builder::new("poly", &[Type::Int, Type::Int], Type::Int);
let a = b.block_params(b.entry())[0];
let c = b.block_params(b.entry())[1];
let sum = b.bin(BinOp::Add, a, c);
let diff = b.bin(BinOp::Sub, a, c);
let product = b.bin(BinOp::Mul, sum, diff);
b.ret(Some(product));

let text = b.finish().to_string();
assert!(text.contains("v4: int = mul v2, v3"));
assert!(text.contains("return v4"));
```

Construction does not check well-formedness as it goes; `validate` does, returning a
defined error rather than panicking when the IR is wrong:

```rust
use ir_lang::{Builder, BinOp, Type, ValidationError};

// `int + bool` is not a valid operation.
let mut b = Builder::new("bad", &[Type::Int, Type::Bool], Type::Int);
let x = b.block_params(b.entry())[0];
let flag = b.block_params(b.entry())[1];
let oops = b.bin(BinOp::Add, x, flag);
b.ret(Some(oops));

assert!(matches!(
    b.finish().validate(),
    Err(ValidationError::TypeMismatch { .. })
));
```

See <a href="./docs/API.md"><code>docs/API.md</code></a> for the full reference, and
<a href="./examples/"><code>examples/</code></a> for runnable demonstrations
(`cargo run --example lower_ast`, `cargo run --example validation`).

<br>

## How it works

A `Function` keeps its blocks and values in flat arenas addressed by dense `Block`
and `Value` indices, so building and walking the IR is a linear pass over
contiguous memory rather than a chase through pointers, and a pass can key a side
table directly on a handle's index. Values cross control-flow joins as block
parameters — a `jump` or `branch` passes one argument per target parameter — which
keeps the representation flat and removes the bookkeeping of phi nodes.

`validate` is where correctness is enforced. It checks the structural rules (one
terminator per block, branch targets that exist, argument counts and types that
match the target parameters, a numeric or boolean operand where the operation
requires it) and then the SSA dominance property: every use of a value is reached by
its single definition. Dominators are computed with the Cooper–Harvey–Kennedy
algorithm over the reachable graph, and the dominance check is a single linear walk
of the dominator tree carrying one reused availability set — no per-block allocation.
The reachability, dominator, and dominance walks all use an explicit stack, so a
deeply nested function cannot overflow the call stack.

<br>

## Status

<code>v1.0.0</code> is the stable release: the public API is frozen and follows
Semantic Versioning, with no breaking changes before <code>2.0</code>. The surface is
the IR core — the `Function` SSA control-flow graph, the `Builder` lowering interface,
the textual `Display`, and `validate` — and the crate is self-contained: it defines
its own machine-level `Type` and is driven entirely through the builder, so it pulls
in no first-party dependency; a front-end maps its own AST and source types onto the
IR. Every core invariant is property-tested against generated programs, the dominator
and dominance algorithms were fuzzed against a brute-force reference, and the suite is
verified on Linux, macOS, and Windows. See the
<a href="./docs/API.md#semver-promise"><code>SemVer promise</code></a>.

<hr>
<br>

## Contributing

See <a href="./dev/DIRECTIVES.md"><code>dev/DIRECTIVES.md</code></a> for engineering standards and the definition of done. Before a PR: `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-features` must be clean.

<br>

<div id="license">
    <h2>License</h2>
    <p>Licensed under either of</p>
    <ul>
        <li><b>Apache License, Version 2.0</b> &mdash; <a href="./LICENSE-APACHE">LICENSE-APACHE</a></li>
        <li><b>MIT License</b> &mdash; <a href="./LICENSE-MIT">LICENSE-MIT</a></li>
    </ul>
    <p>at your option.</p>
</div>

<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>James Gober <me@jamesgober.com>.</strong></sup>
</div>
