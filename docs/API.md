# ir-lang &mdash; API Reference

> Complete reference for every public item in `ir-lang`, with examples.
> **Status: pre-1.0 (v0.2.0).** The surface below is the core release; it is still
> being designed across the 0.x series and freezes at `1.0.0`. Minor 0.x releases may
> make breaking changes, each noted in [`../CHANGELOG.md`](../CHANGELOG.md). See
> [`../dev/ROADMAP.md`](../dev/ROADMAP.md).

## Table of Contents

- [Overview](#overview)
- [Installation](#installation)
- [Quick start](#quick-start)
- [The model](#the-model)
- [`Type`](#type)
- [`Value`](#value)
- [`Block`](#block)
- [`BinOp`](#binop)
- [`UnOp`](#unop)
- [`Inst`](#inst)
- [`Terminator`](#terminator)
- [`Builder`](#builder)
  - [`Builder::new`](#buildernew)
  - [`Builder::entry` / `current_block`](#builderentry--current_block)
  - [`Builder::create_block`](#buildercreate_block)
  - [`Builder::block_params`](#builderblock_params)
  - [`Builder::switch_to`](#builderswitch_to)
  - [`Builder::iconst` / `fconst` / `bconst`](#buildericonst--fconst--bconst)
  - [`Builder::bin`](#builderbin)
  - [`Builder::un`](#builderun)
  - [`Builder::ret` / `jump` / `branch`](#builderret--jump--branch)
  - [`Builder::finish`](#builderfinish)
- [`Function`](#function)
  - [Signature accessors](#signature-accessors)
  - [Graph accessors](#graph-accessors)
  - [Value accessors](#value-accessors)
  - [`Function::validate`](#functionvalidate)
  - [Textual form (`Display`)](#textual-form-display)
- [`ValidationError`](#validationerror)
- [Feature flags](#feature-flags)
- [Stability & SemVer](#stability--semver)

---

## Overview

ir-lang is the intermediate representation a compiler optimizes and lowers code
through. A [`Function`](#function) is a control-flow graph of basic blocks in SSA
form: each block is a straight-line run of value-producing [instructions](#inst)
ended by one [terminator](#terminator), and every [value](#value) is named by a small
handle and defined exactly once. Values cross control-flow joins as block parameters
rather than phi nodes.

There is no AST type here to lower *from* — a language brings its own syntax tree —
so lowering is expressed through the [`Builder`](#builder): a front-end walks its tree
and calls a builder method per construct, the same shape as Cranelift's
`FunctionBuilder` or LLVM's `IRBuilder`. The crate is self-contained: it defines its
own machine-level [`Type`](#type) and wires no first-party dependency. Mapping a
source language's AST and types onto the IR is the consumer's lowering step.

---

## Installation

```toml
[dependencies]
ir-lang = "0.2"
```

Or from the terminal:

```bash
cargo add ir-lang
```

MSRV: Rust 1.85 (Rust 2024 edition).

---

## Quick start

Lower and validate `fn max(a: int, b: int) -> int { if a < b { b } else { a } }`:

```rust
use ir_lang::{Builder, BinOp, Type};

let mut b = Builder::new("max", &[Type::Int, Type::Int], Type::Int);
let a = b.block_params(b.entry())[0];
let bb = b.block_params(b.entry())[1];

let join = b.create_block(&[Type::Int]);   // takes the chosen value
let then_blk = b.create_block(&[]);
let else_blk = b.create_block(&[]);

let cond = b.bin(BinOp::Lt, a, bb);
b.branch(cond, then_blk, &[], else_blk, &[]);

b.switch_to(then_blk);
b.jump(join, &[bb]);

b.switch_to(else_blk);
b.jump(join, &[a]);

b.switch_to(join);
let result = b.block_params(join)[0];
b.ret(Some(result));

let func = b.finish();
assert!(func.validate().is_ok());
```

---

## The model

A function owns two flat arenas: one of blocks, one of values. A [`Block`](#block)
and a [`Value`](#value) are dense `u32` indices into those arenas — `Copy`, stable
for the life of the function, and usable as a key into a side table a pass keeps
alongside the IR. A block holds its parameter values, the values its instructions
define in order, and one terminator. A value records its type and where it is
defined: either a block parameter or the result of an instruction.

Construction (the [`Builder`](#builder)) and verification
([`Function::validate`](#functionvalidate)) are separate steps. The builder records
what you tell it without checking well-formedness, which keeps lowering cheap and
lets you emit blocks in any order; `validate` then confirms the structural and SSA
invariants in one pass. Run it once the function is built, and again on the output of
any pass that rewrites the IR.

---

## `Type`

The machine-level type of a value — the IR's own small type system, independent of
any source language.

| Variant | Meaning |
| ------- | ------- |
| `Type::Int` | A signed integer value. |
| `Type::Float` | A floating-point value. |
| `Type::Bool` | A boolean, as produced by a comparison or logical operation. |
| `Type::Unit` | The absence of a value; the return type of a function that returns nothing. |

**Method** — `Type::is_numeric(self) -> bool`: `true` for `Int` and `Float`, the
types arithmetic and ordering accept.

```rust
use ir_lang::Type;

assert_eq!(Type::Int.to_string(), "int");
assert!(Type::Float.is_numeric());
assert!(!Type::Bool.is_numeric());
```

`Type` is `Copy`, `Eq`, `Ord`, `Hash`, and `Display`.

---

## `Value`

A handle to an SSA value — the result of one instruction or one block parameter. It
is `Copy`, prints as `v<n>`, and is dense from zero.

**Method** — `Value::index(self) -> usize`: the zero-based index, for use as a key
into a side table.

```rust
use ir_lang::{Builder, Type};

let mut b = Builder::new("k", &[Type::Int], Type::Int);
let x = b.block_params(b.entry())[0];
let one = b.iconst(1);

assert_eq!(x.to_string(), "v0");
assert_eq!(one.index(), 1);
```

Handles are scoped to the function that minted them — do not use one function's
values in another.

---

## `Block`

A handle to a basic block. `Copy`, prints as `b<n>`, dense from zero; the entry block
is always `b0`.

**Method** — `Block::index(self) -> usize`: the zero-based index.

```rust
use ir_lang::{Builder, Type};

let mut b = Builder::new("f", &[], Type::Unit);
let entry = b.entry();
let exit = b.create_block(&[]);

assert_eq!(entry.to_string(), "b0");
assert_eq!(exit.index(), 1);
```

---

## `BinOp`

A binary operation. Arithmetic (`Add`, `Sub`, `Mul`, `Div`) takes two operands of
the same numeric type and yields that type. Comparisons (`Eq`, `Ne`, `Lt`, `Le`,
`Gt`, `Ge`) take two operands of the same type and yield `Bool`. Logical operations
(`And`, `Or`) take two `Bool`s and yield `Bool`.

**Methods** — `is_comparison(self) -> bool`, `is_logical(self) -> bool`.

```rust
use ir_lang::BinOp;

assert_eq!(BinOp::Add.to_string(), "add");
assert!(BinOp::Lt.is_comparison());
assert!(BinOp::And.is_logical());
assert!(!BinOp::Mul.is_comparison());
```

---

## `UnOp`

A unary operation. `Neg` negates a numeric operand and yields its type; `Not` inverts
a `Bool` and yields `Bool`.

```rust
use ir_lang::UnOp;

assert_eq!(UnOp::Neg.to_string(), "neg");
assert_eq!(UnOp::Not.to_string(), "not");
```

---

## `Inst`

A value-producing instruction, read back from a function. You do not build one
directly — the [`Builder`](#builder) emits one per call — but you match on it when
inspecting or rewriting IR.

| Variant | Result type |
| ------- | ----------- |
| `Inst::Iconst(i64)` | `Int` |
| `Inst::Fconst(f64)` | `Float` |
| `Inst::Bconst(bool)` | `Bool` |
| `Inst::Bin(BinOp, Value, Value)` | follows the operation |
| `Inst::Un(UnOp, Value)` | follows the operation |

```rust
use ir_lang::{Builder, BinOp, Inst, Type};

let mut b = Builder::new("k", &[], Type::Int);
let one = b.iconst(1);
let sum = b.bin(BinOp::Add, one, one);
b.ret(Some(sum));
let func = b.finish();

assert!(matches!(func.inst(sum), Some(Inst::Bin(BinOp::Add, _, _))));
assert!(matches!(func.inst(one), Some(Inst::Iconst(1))));
```

---

## `Terminator`

The single instruction that ends a block and transfers control.

| Variant | Meaning |
| ------- | ------- |
| `Terminator::Return(Option<Value>)` | Leave the function, with a value or none. |
| `Terminator::Jump(Block, Vec<Value>)` | Jump to a block, one argument per target parameter. |
| `Terminator::Branch { cond, then_block, then_args, else_block, else_args }` | Branch on a `Bool`; each arm passes arguments to its target's parameters. |

**Method** — `each_successor(&self, f: impl FnMut(Block))`: calls `f` for each block
this terminator can transfer to, in order. A `Return` calls `f` zero times — this is
how the control-flow graph is read.

```rust
use ir_lang::{Builder, Terminator, Type};

let mut b = Builder::new("f", &[], Type::Unit);
let exit = b.create_block(&[]);
b.jump(exit, &[]);
b.switch_to(exit);
b.ret(None);
let func = b.finish();

let mut succs = Vec::new();
if let Some(term) = func.terminator(func.entry()) {
    term.each_successor(|blk| succs.push(blk));
}
assert_eq!(succs, vec![exit]);
assert!(matches!(func.terminator(exit), Some(Terminator::Return(None))));
```

---

## `Builder`

Constructs a [`Function`](#function) one instruction at a time. This is the lowering
interface: a front-end calls a method per syntax-tree construct, and the builder
hands back the [`Value`](#value) each result defines.

### `Builder::new`

```text
Builder::new(name: impl Into<String>, params: &[Type], ret: Type) -> Builder
```

Starts a function with a name, parameter types, and return type. The entry block is
created with one parameter per function parameter; read those input values with
[`block_params`](#builderblock_params) on [`entry`](#builderentry--current_block).
The entry block is current, so emission can begin at once.

```rust
use ir_lang::{Builder, Type};

let b = Builder::new("identity", &[Type::Int], Type::Int);
assert_eq!(b.block_params(b.entry()).len(), 1);
```

### `Builder::entry` / `current_block`

`entry(&self) -> Block` returns the entry block. `current_block(&self) -> Block`
returns the block emission currently targets.

```rust
use ir_lang::{Builder, Type};

let mut b = Builder::new("f", &[], Type::Unit);
let next = b.create_block(&[]);
b.switch_to(next);
assert_eq!(b.current_block(), next);
assert_eq!(b.entry().index(), 0);
```

### `Builder::create_block`

```text
create_block(&mut self, params: &[Type]) -> Block
```

Creates a block with the given parameter types and returns its handle. Block
parameters are how a value crosses a control-flow join in SSA form. The new block
does not become current — call [`switch_to`](#builderswitch_to) to emit into it.

```rust
use ir_lang::{Builder, Type};

let mut b = Builder::new("f", &[], Type::Int);
let join = b.create_block(&[Type::Int]);
assert_eq!(b.block_params(join).len(), 1);
```

### `Builder::block_params`

```text
block_params(&self, block: Block) -> &[Value]
```

Returns a block's parameter values, in order, or an empty slice for an out-of-range
block.

```rust
use ir_lang::{Builder, Type};

let b = Builder::new("f", &[Type::Int, Type::Bool], Type::Unit);
let params = b.block_params(b.entry());
assert_eq!(params.len(), 2);
```

### `Builder::switch_to`

```text
switch_to(&mut self, block: Block)
```

Switches emission to `block`; subsequent instructions and the terminator are added to
it. Emit a block's instructions before its terminator.

```rust
use ir_lang::{Builder, Type};

let mut b = Builder::new("f", &[], Type::Unit);
let other = b.create_block(&[]);
b.switch_to(other);
b.ret(None);
```

### `Builder::iconst` / `fconst` / `bconst`

```text
iconst(&mut self, value: i64) -> Value
fconst(&mut self, value: f64) -> Value
bconst(&mut self, value: bool) -> Value
```

Emit a constant of the matching type into the current block and return its value.

```rust
use ir_lang::{Builder, Type};

let mut b = Builder::new("k", &[], Type::Int);
let n = b.iconst(42);
let pi = b.fconst(3.14);
let t = b.bconst(true);
b.ret(Some(n));
let func = b.finish();

assert_eq!(func.value_type(n), Some(Type::Int));
assert_eq!(func.value_type(pi), Some(Type::Float));
assert_eq!(func.value_type(t), Some(Type::Bool));
```

### `Builder::bin`

```text
bin(&mut self, op: BinOp, lhs: Value, rhs: Value) -> Value
```

Emits a binary operation and returns the result. The result type is inferred from the
operation — `Bool` for a comparison or logical op, the operand type for arithmetic —
so it is never passed in. Whether the operands satisfy the operation is checked by
[`validate`](#functionvalidate), not here.

```rust
use ir_lang::{Builder, BinOp, Type};

let mut b = Builder::new("f", &[Type::Int, Type::Int], Type::Bool);
let a = b.block_params(b.entry())[0];
let c = b.block_params(b.entry())[1];
let sum = b.bin(BinOp::Add, a, c);   // Int
let lt = b.bin(BinOp::Lt, a, c);     // Bool
b.ret(Some(lt));
let func = b.finish();

assert_eq!(func.value_type(sum), Some(Type::Int));
assert_eq!(func.value_type(lt), Some(Type::Bool));
```

### `Builder::un`

```text
un(&mut self, op: UnOp, operand: Value) -> Value
```

Emits a unary operation and returns the result. `Neg` yields the operand's type;
`Not` yields `Bool`.

```rust
use ir_lang::{Builder, UnOp, Type};

let mut b = Builder::new("f", &[Type::Int], Type::Int);
let x = b.block_params(b.entry())[0];
let neg = b.un(UnOp::Neg, x);
b.ret(Some(neg));
assert_eq!(b.finish().value_type(neg), Some(Type::Int));
```

### `Builder::ret` / `jump` / `branch`

```text
ret(&mut self, value: Option<Value>)
jump(&mut self, target: Block, args: &[Value])
branch(&mut self, cond: Value, then_block: Block, then_args: &[Value],
       else_block: Block, else_args: &[Value])
```

Set the current block's terminator. `ret(Some(v))` returns `v`; `ret(None)` returns
from a `Unit` function. `jump` and `branch` pass one argument per parameter of each
target block.

```rust
use ir_lang::{Builder, Type};

let mut b = Builder::new("pick", &[Type::Bool], Type::Int);
let cond = b.block_params(b.entry())[0];
let join = b.create_block(&[Type::Int]);
let yes = b.create_block(&[]);
let no = b.create_block(&[]);
b.branch(cond, yes, &[], no, &[]);

b.switch_to(yes);
let one = b.iconst(1);
b.jump(join, &[one]);

b.switch_to(no);
let zero = b.iconst(0);
b.jump(join, &[zero]);

b.switch_to(join);
let r = b.block_params(join)[0];
b.ret(Some(r));
assert!(b.finish().validate().is_ok());
```

### `Builder::finish`

```text
finish(self) -> Function
```

Finishes construction and returns the assembled function. It is not validated by this
call — run [`Function::validate`](#functionvalidate) on the result.

```rust
use ir_lang::{Builder, Type};

let mut b = Builder::new("f", &[], Type::Unit);
b.ret(None);
let func = b.finish();
assert_eq!(func.block_count(), 1);
```

---

## `Function`

A function in SSA form. Produced by [`Builder::finish`](#builderfinish), then read
through the accessors below.

### Signature accessors

| Method | Returns |
| ------ | ------- |
| `name(&self) -> &str` | the function's name |
| `params(&self) -> &[Type]` | the parameter types (also the entry block's parameters) |
| `ret(&self) -> Type` | the return type |
| `entry(&self) -> Block` | the entry block (always `b0`) |

```rust
use ir_lang::{Builder, Type};

let b = Builder::new("f", &[Type::Int, Type::Bool], Type::Float);
let func = b.finish();
assert_eq!(func.name(), "f");
assert_eq!(func.params(), &[Type::Int, Type::Bool]);
assert_eq!(func.ret(), Type::Float);
```

### Graph accessors

| Method | Returns |
| ------ | ------- |
| `block_count(&self) -> usize` | number of blocks |
| `value_count(&self) -> usize` | number of values (handles run `0..count`) |
| `blocks(&self) -> impl Iterator<Item = Block>` | every block, entry first |
| `block_params(&self, block) -> &[Value]` | a block's parameter values |
| `insts(&self, block) -> &[Value]` | values defined by a block's instructions, in order |
| `terminator(&self, block) -> Option<&Terminator>` | the block's terminator |

```rust
use ir_lang::{Builder, BinOp, Type};

let mut b = Builder::new("f", &[Type::Int], Type::Int);
let x = b.block_params(b.entry())[0];
let _ = b.bin(BinOp::Add, x, x);
b.ret(Some(x));
let func = b.finish();

assert_eq!(func.block_count(), 1);
assert_eq!(func.blocks().count(), 1);
assert_eq!(func.insts(func.entry()).len(), 1);
assert!(func.terminator(func.entry()).is_some());
```

### Value accessors

| Method | Returns |
| ------ | ------- |
| `inst(&self, value) -> Option<&Inst>` | the defining instruction, or `None` for a block parameter |
| `value_type(&self, value) -> Option<Type>` | the value's type |
| `value_block(&self, value) -> Option<Block>` | the block the value is defined in |

Out-of-range handles return `None` rather than panicking.

```rust
use ir_lang::{Builder, Type};

let mut b = Builder::new("f", &[Type::Int], Type::Int);
let param = b.block_params(b.entry())[0];
let five = b.iconst(5);
b.ret(Some(param));
let func = b.finish();

assert!(func.inst(param).is_none());            // a parameter has no instruction
assert!(func.inst(five).is_some());
assert_eq!(func.value_type(five), Some(Type::Int));
assert_eq!(func.value_block(five), Some(func.entry()));
```

### `Function::validate`

```text
validate(&self) -> Result<(), ValidationError>
```

Checks the function is well-formed, returning the first violation found. A function
that validates satisfies the SSA invariants the rest of a compiler relies on: one
terminator per block; branch targets that exist with matching argument counts and
types; every use reached by its single definition; operations applied to operands of
the right type; and the entry block never a branch target.

```rust
use ir_lang::{Builder, BinOp, Type, ValidationError};

// Well-formed.
let mut b = Builder::new("f", &[Type::Int], Type::Int);
let x = b.block_params(b.entry())[0];
let two = b.iconst(2);
let doubled = b.bin(BinOp::Mul, x, two);
b.ret(Some(doubled));
assert!(b.finish().validate().is_ok());

// A block with no terminator is rejected.
let unfinished = Builder::new("g", &[], Type::Unit).finish();
assert!(matches!(
    unfinished.validate(),
    Err(ValidationError::MissingTerminator { .. })
));
```

### Textual form (`Display`)

`Function` implements `Display`, printing a readable textual IR — useful in tests and
when debugging a pass.

```rust
use ir_lang::{Builder, BinOp, Type};

let mut b = Builder::new("double", &[Type::Int], Type::Int);
let x = b.block_params(b.entry())[0];
let sum = b.bin(BinOp::Add, x, x);
b.ret(Some(sum));
let text = b.finish().to_string();

assert!(text.contains("fn double(int) -> int {"));
assert!(text.contains("b0(v0: int):"));
assert!(text.contains("v1: int = add v0, v0"));
assert!(text.contains("return v1"));
```

---

## `ValidationError`

The reason a function is not well-formed, returned by
[`Function::validate`](#functionvalidate). It is `#[non_exhaustive]`, so a `match`
must include a wildcard arm.

| Variant | Meaning |
| ------- | ------- |
| `MissingTerminator { block }` | a block has no terminator |
| `BlockOutOfRange { block }` | a terminator targets a block that does not exist |
| `ValueOutOfRange { value }` | a reference to a value that does not exist |
| `EntryBranchTarget { block }` | a terminator branches to the entry block |
| `ArgCountMismatch { block, expected, found }` | a jump/branch passes the wrong number of arguments |
| `TypeMismatch { value, expected, found }` | a value is used where another type was required |
| `NotNumeric { value, found }` | an arithmetic operation was applied to a non-numeric value |
| `ReturnValueExpected { expected }` | a valueless `return` in a non-`Unit` function |
| `UseBeforeDef { value, block }` | a value is used where its definition does not reach |

`ValidationError` implements `Display` and `std::error::Error`.

```rust
use ir_lang::{Builder, BinOp, Type, ValidationError};

let mut b = Builder::new("f", &[Type::Int, Type::Bool], Type::Int);
let x = b.block_params(b.entry())[0];
let flag = b.block_params(b.entry())[1];
let bad = b.bin(BinOp::Add, x, flag);   // int + bool
b.ret(Some(bad));

match b.finish().validate() {
    Err(ValidationError::TypeMismatch { found, .. }) => assert_eq!(found, Type::Bool),
    other => panic!("expected a type mismatch, got {other:?}"),
}
```

---

## Feature flags

| Feature | Default | Description |
| ------- | ------- | ----------- |
| `std`   | yes     | Standard-library support. With it disabled the crate is `#![no_std]` and runs on `alloc` alone. |
| `serde` | no      | Derives `serde::Serialize` / `Deserialize` for the IR types ([`Type`](#type), [`Value`](#value), [`Block`](#block), [`BinOp`](#binop), [`UnOp`](#unop), [`Inst`](#inst), [`Terminator`](#terminator), [`Function`](#function), and [`ValidationError`](#validationerror)) so a function can be cached, inspected, or moved between tools. |

Features are additive: enabling one never removes or changes behaviour provided by
another, per the project's SemVer policy.

---

## Stability & SemVer

The crate follows [Semantic Versioning](https://semver.org). During the 0.x series
the public surface is still being designed, so a minor release may make a breaking
change — each is documented in [`../CHANGELOG.md`](../CHANGELOG.md) with a migration
note. At `1.0.0` the surface freezes: no breaking change before `2.0`, additions
arrive in minor releases, and the MSRV only rises in a minor release. This file is
updated in lockstep with every release so it always matches the code.

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>
