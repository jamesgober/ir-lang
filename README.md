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
        <strong>ir-lang</strong> is the SSA intermediate representation of a native compiler: what a front end lowers into after monomorphization, closure conversion, pattern compilation, and effect lowering, what an optimizer rewrites, and what instruction selection reads. A <code>Module</code> holds functions, globals, and aggregate types; a function is a control-flow graph whose blocks take <strong>parameters</strong> instead of phi nodes, and every value has one type and one definition.
    </p>
    <p>
        The instruction set covers every operation of the family's shared operation semantics (OPS v2, including <code>pow</code> and saturating shifts) with its overflow, division, shift, and conversion <strong>policy on each instance</strong>, so every tier computes the same result; calls (direct, indirect, tail) and <code>invoke</code> with exception edges; operation errors routed to explicit error edges; memory with alignment, volatility, atomics, and address computation; and <strong>safepoints that relocate GC references</strong>, so native frames can be scanned by a moving collector. A validator checks all of it, a builder doubles as a rewrite API for optimization passes and <strong>constructs SSA from mutable variables</strong>, a <strong>statepoint pass</strong> fills GC lists from liveness, a printer and a budgeted parser round-trip exactly, and a reference interpreter implements the semantics as the oracle for optimizers and backends. It is a SEMA/CODE-tier crate of the <code>-lang</code> language-construction family.
    </p>
    <br>
    <hr>
    <p>
        <strong>MSRV is 1.85+</strong> (Rust 2024 edition). <code>no_std</code>-compatible (needs only <code>alloc</code>; the interpreter needs <code>std</code>), <code>#![forbid(unsafe_code)]</code>, no dependencies.
    </p>
    <blockquote>
        <strong>2.0.0-alpha.3 is a pre-release</strong> of the 2.0 redesign. alpha.3 is a conformance fix: integer <code>pow</code> with a negative exponent is now the <code>NegativeExponent</code> error under every overflow policy, <code>trap</code> included, as OPS requires (alpha.2 trapped), so a signed <code>pow</code> is always a <code>check</code>. The surface may change before <code>2.0.0</code>, which follows once a real consumer (the HIR lowering, opt-lang, and instruction selection) has exercised it. The normative specification is <code>_lexersketch/specs/IR.md</code>; 1.x stays available for current users. See <a href="./CHANGELOG.md"><code>CHANGELOG.md</code></a> for the migration guide and <a href="./dev/ROADMAP.md"><code>dev/ROADMAP.md</code></a> for the path to 2.0.0.
    </blockquote>
</div>

<hr>
<br>

## The model

- A **[`Module`](./docs/API.md#module)** declares functions (with a [`Signature`](./docs/API.md#signature) and a calling-convention tag, a linkage, and attributes), [globals](./docs/API.md#global) with initializers and relocations, and [aggregates](./docs/API.md#aggregate) (structs, packed structs, arrays) with C layout.
- A **[`Function`](./docs/API.md#function)** body is blocks of [instructions](./docs/API.md#instdata) ended by one [terminator](./docs/API.md#terminator). Values cross edges as block arguments.
- A **[`Builder`](./docs/API.md#builder)** lowers into a body and rewrites it. Every instruction is type-checked as it is added. Mutable locals lower through **[variables](./docs/API.md#ssa-construction)** (`declare_var` / `def_var` / `use_var` / `seal_block`); GC references can be used freely and **[`insert_statepoints`](./docs/API.md#builderinsert_statepoints)** relocates them.
- **[`Module::validate`](./docs/API.md#modulevalidate)** checks everything; **`Display`** and **[`parse`](./docs/API.md#parse)** are the textual form.
- **[`interp`](./docs/API.md#interp-feature-interp)** (feature `interp`) runs it.

<br>

What it guarantees, and how each guarantee is checked:

| Guarantee | How it is held |
|---|---|
| A well-typed program built with the builder validates. | Property tests lower thousands of random programs (loops, calls, invokes, tail calls, memory, globals, checked arithmetic, exceptions) and validate each. |
| Dominance and the safepoint rule are decided exactly. | Property tests compare the validator with brute-force references on random control-flow graphs (loops, unreachable blocks, block parameters): dominance by path removal, reference liveness by path search. |
| Validation is total: it accepts or reports, never panics. | A sweep corrupts internal state thousands of times; property tests validate mutated text and the results of hostile rewrites. |
| Operations follow `specs/OPS.md` (v2) exactly, on every type and policy. | The interpreter is compared against independent reference arithmetic on the OPS edge values of every integer type and policy (integer `pow` and `shift = saturate` included; a negative `pow` exponent is pinned as the E0006 error under every policy and signedness, in the interpreter and in the validator's `check` placement), IEEE special cases, and exact rational checks of `ieee_rem`; float `pow` (`ls_pow`) matches all 122 vectors of `specs/ops-vectors/pow.md` bit for bit. |
| SSA construction from mutable variables preserves meaning and leaves no trivial block parameter. | Random programs with mutable variables, loops, invokes, and checked arithmetic are lowered through `def_var`/`use_var`, sealing blocks eagerly and lazily, and run against the independent evaluator; every result is checked for trivial parameters. |
| Statepoint insertion makes reference-carrying code valid without changing it. | Random programs keep references in variables across allocations, polls, safepoints, and invokes; after insertion they validate (the validator is the oracle), a second insertion changes nothing, and the interpreter's moving-collector simulation computes what an independent evaluator does. |
| Lowered programs mean what the source means. | Differential tests run each random program in an independent evaluator and, lowered, in the interpreter, before and after a print/parse round trip; results, traps, and exceptions agree exactly. |
| The textual form round-trips exactly. | `parse(print(m))` prints identically and keeps every handle, over random programs and a module containing every instruction form. |
| Rewrites keep IR valid and meaning intact. | Random sequences of rewrites (split, duplicate-and-forward, remove dead code, swap operands, compact) are validated after every step and the result run against the evaluator. |
| GC references are never live across a safepoint without relocation. | The validator enforces it; the interpreter starts a new GC epoch at every safepoint and reports any stale reference. |
| Hostile input is bounded. | The parser caps input size and entity ids; the interpreter caps fuel, depth, and memory; every walk is iterative. Fuzzed with random and mutated text. |

<hr>
<br>

## Installation

```toml
[dependencies]
ir-lang = "2.0.0-alpha.3"
```

With the reference interpreter:

```toml
[dependencies]
ir-lang = { version = "2.0.0-alpha.3", features = ["interp"] }
```

Without the standard library:

```toml
[dependencies]
ir-lang = { version = "2.0.0-alpha.3", default-features = false }
```

<hr>
<br>

## Quick start

Build, validate, print, and parse back `fn add1(x: i64) -> i64 { x + 1 }`:

```rust
use ir_lang::{Linkage, Module, Overflow, Signature, Type};

let mut m = Module::new("demo");
let f = m.declare_function("add1", &Signature::new(&[Type::I64], &[Type::I64]), Linkage::Export)?;
let mut b = m.build(f)?;
let x = b.param(0)?;
let one = b.iconst(Type::I64, 1)?;
let y = b.add(x, one, Overflow::Wrap)?;
b.ret(&[y])?;

m.validate()?;
assert_eq!(
    m.to_string(),
    "module \"demo\" ptr64\nsig s0 = (i64) -> (i64) fast\n\nfunc @add1 s0 export {\nb0(v0: i64):\n    v1: i64 = const 1\n    v2: i64 = add<overflow=wrap> v0, v1\n    ret v2\n}\n"
);
assert_eq!(ir_lang::parse(&m.to_string())?.to_string(), m.to_string());
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Errors and exceptions are edges

An operation whose policy is `error` is a `check` terminator with an error edge carrying the OPS error code; a call that may throw is an `invoke` with a landing pad:

```rust
use ir_lang::{BinaryOp, BlockArg, BlockCall, Call, InstData, Linkage, Module, Overflow, Policy, Signature, Type};

let mut m = Module::new("demo");
let may_throw = m.declare_function("may_throw", &Signature::new(&[], &[Type::I32]), Linkage::Import)?;
let f = m.declare_function("f", &Signature::new(&[Type::I32], &[Type::I32]), Linkage::Export)?;
let mut b = m.build(f)?;
let x = b.param(0)?;
let got = b.create_block(&[Type::I32])?;
let pad = b.create_block(&[Type::Ptr])?;
b.invoke(
    Call::direct(may_throw, &[]),
    BlockCall::with_args(got, &[BlockArg::Result(0)]),
    BlockCall::with_args(pad, &[BlockArg::Exception]),
)?;

b.switch_to(got)?;
let y = b.block_param(got, 0)?;
let sum = b.create_block(&[Type::I32])?;
let overflow = b.create_block(&[Type::U32])?;
b.check(
    InstData::Binary { op: BinaryOp::Add, policy: Policy::overflow(Overflow::Error), args: [x, y] },
    BlockCall::with_args(sum, &[BlockArg::Result(0)]),
    BlockCall::with_args(overflow, &[BlockArg::ErrorCode]),
)?;
b.switch_to(sum)?;
let s = b.block_param(sum, 0)?;
b.ret(&[s])?;
b.switch_to(overflow)?;
b.trap(1)?;
b.switch_to(pad)?;
let exn = b.block_param(pad, 0)?;
b.resume(exn)?;

m.validate()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Mutable locals and GC references

A front end names its mutable locals with variables and lets the builder place the block parameters; it keeps GC references in ordinary values across calls and lets `insert_statepoints` relocate them:

```rust
use ir_lang::{Call, CmpOp, Linkage, Module, Signature, Type};

// fn keep(r: ref, n: i64) -> ref { let mut x = r; let mut i = 0; while i < n { poll(); i += 1 } x }
let mut m = Module::new("demo");
let poll = m.declare_function("poll", &Signature::new(&[], &[]), Linkage::Import)?;
let f = m.declare_function("keep", &Signature::new(&[Type::Ref, Type::I64], &[Type::Ref]), Linkage::Export)?;
let mut b = m.build(f)?;
let (x, i) = (b.declare_var(Type::Ref)?, b.declare_var(Type::I64)?);
let (r, n) = (b.param(0)?, b.param(1)?);
b.def_var(x, r)?;
let zero = b.iconst(Type::I64, 0)?;
b.def_var(i, zero)?;
let (head, body, exit) = (b.create_block(&[])?, b.create_block(&[])?, b.create_block(&[])?);
b.jump(head, &[])?;
b.switch_to(head)?;
let iv = b.use_var(i)?;
let more = b.compare(CmpOp::Lt, iv, n)?;
b.branch(more, body, &[], exit, &[])?;
b.switch_to(body)?;
b.seal_block(body)?;
b.call(Call::direct(poll, &[]))?; // a safepoint; `x` is live across it
let iv = b.use_var(i)?;
let one = b.iconst(Type::I64, 1)?;
let i2 = b.add(iv, one, ir_lang::Overflow::Wrap)?;
b.def_var(i, i2)?;
b.jump(head, &[])?;
b.seal_block(head)?;
b.switch_to(exit)?;
b.seal_block(exit)?;
let xv = b.use_var(x)?;
b.ret(&[xv])?;
b.seal_all_blocks()?;

assert!(b.validate().is_err()); // `x` is used after the poll without relocation
let stats = b.insert_statepoints()?;
assert_eq!(stats.relocations, 1);
m.validate()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Running it

With the `interp` feature, the reference interpreter runs a module with the exact operation semantics:

```rust
# #[cfg(feature = "interp")] {
use ir_lang::interp::{self, Outcome, Val};
use ir_lang::{Linkage, Module, Overflow, Signature, Type};

let mut m = Module::new("demo");
let f = m.declare_function("add1", &Signature::new(&[Type::I64], &[Type::I64]), Linkage::Export)?;
let mut b = m.build(f)?;
let x = b.param(0)?;
let one = b.iconst(Type::I64, 1)?;
let y = b.add(x, one, Overflow::Trap)?;
b.ret(&[y])?;

assert_eq!(interp::run(&m, f, &[Val::i64(41)])?, Outcome::Return(vec![Val::i64(42)]));
assert_eq!(interp::run(&m, f, &[Val::i64(i64::MAX)])?, Outcome::Trap(1)); // E0001
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

More in [`examples/`](./examples): `lower` (a front end's tree into IR), `rewrite` (a folding pass with the rewrite API), `run` (the interpreter).

<hr>
<br>

## Performance

Measured with `cargo bench` (criterion) on the development machine (Windows, x86-64), library only. Throughput scales linearly from 100k to 1M instructions.

| Operation (1M instructions) | Time | Throughput |
|---|---|---|
| Build, straight line | 49 ms | 20 M instructions/s |
| Build, chain of diamonds (250k blocks) | 134 ms | 7.5 M/s |
| Validate, straight line | 34 ms | 30 M/s |
| Validate, diamonds | 120 ms | 8.3 M/s |
| Print, diamonds | 244 ms | 4.1 M/s |
| Parse, diamonds | 652 ms | 1.5 M/s |
| Compact, diamonds | 144 ms | 7.0 M/s |
| Validate 100k safepoints relocating a reference | 12 ms | 8.1 M/s |

New in 2.0.0-alpha.2, measured while other builds shared the machine (the unchanged safepoint validation above varied between 11 and 40 ms in the same session, so read these as upper bounds):

| Operation | Time | Throughput |
|---|---|---|
| SSA construction, 100k instructions of diamonds through variables | 59–112 ms | about 1.3 M instructions/s |
| SSA construction, 1M instructions | 0.75–1.24 s | about 1.1 M/s |
| `insert_statepoints`, 100k instructions (33k safepoints, two references live across each, 66k join parameters) | 165–229 ms | about 0.5 M/s |
| `ls_pow`, 10,000 calls | 4.6–5.4 ms | about 2 M calls/s |

The textual form is for tests and debugging; nothing on the compile path parses text.

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
