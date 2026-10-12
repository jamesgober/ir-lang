# ir-lang &mdash; API Reference

> Complete reference for every public item in `ir-lang` 2.0.0-alpha.3, with examples.
> **Status: pre-release.** The surface may change before `2.0.0`; see
> [`../dev/ROADMAP.md`](../dev/ROADMAP.md). The normative semantics are
> `_lexersketch/specs/IR.md` (the IR) and `_lexersketch/specs/OPS.md` (operations).

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>

## Table of contents

- [Overview](#overview)
- [Installation](#installation)
- [Quick start](#quick-start)
- [Concepts](#concepts)
  - [Modules, functions, blocks, values](#modules-functions-blocks-values)
  - [Handles](#handles)
  - [Types](#types)
  - [Policies](#policies)
  - [Error and exception edges](#error-and-exception-edges)
  - [Safepoints and GC references](#safepoints-and-gc-references)
  - [Rewriting](#rewriting)
  - [The textual form](#the-textual-form)
- [`Module`](#module)
- [Declarations: `FuncDecl`, `Linkage`, `Global`, `Init`, `Reloc`, `SymbolRef`](#declarations)
- [Types: `Type`, `PointerWidth`, `MemType`, `Aggregate`, `Layout`, `Signature`, `CallConv`](#type)
- [`Function`](#function)
- [Handles: `Value`, `Block`, `Inst`, `StackSlot`, `FuncId`, `GlobalId`, `SigId`, `AggId`, `ValueRange`](#handles-1)
- [`Builder`](#builder)
  - [SSA construction: `Variable`, `declare_var`, `def_var`, `use_var`, `seal_block`](#ssa-construction)
  - [`Builder::insert_statepoints`, `insert_statepoints_within`, `StatepointStats`, `DEFAULT_STATEPOINT_BUDGET`](#builderinsert_statepoints)
- [`InstData`](#instdata)
- [`Terminator`](#terminator)
- [Edges: `BlockCall`, `BlockArg`, `SwitchCase`, `Successors`](#edges)
- [Calls: `Call`, `CallTarget`, `Attrs`](#calls)
- [Memory: `MemFlags`, `MemOrder`, `RmwOp`](#memory)
- [Operations: `UnaryOp`, `BinaryOp`, `CmpOp`, `ConvOp`](#operations)
- [Policies: `Policy`, `PolicyFields`, `Overflow`, `DivZero`, `Shift`, `FloatToInt`, `OpError`](#policy)
- [`ls_pow`](#ls_pow)
- [Errors: `ModuleError`, `BuildError`, `ValidationError`, `ValidationErrorKind`](#errors)
- [`parse`](#parse)
- [`interp` (feature `interp`)](#interp-feature-interp)
- [Feature flags](#feature-flags)
- [Limits](#limits)
- [Stability](#stability)

## Overview

| Item | Kind | Purpose |
|---|---|---|
| [`Module`](#module) | struct | Declarations and function bodies; validation; printing. |
| [`Builder`](#builder) | struct | Lowers into a body and rewrites it; constructs SSA from mutable variables; inserts statepoints. |
| [`Variable`](#ssa-construction) | handle | A mutable local of SSA construction. |
| [`StatepointStats`](#builderinsert_statepoints) | struct | What statepoint insertion changed. |
| [`ls_pow`](#ls_pow) | function | The family's shared float `pow` routine (OPS v2), bit-identical on every tier. |
| [`Function`](#function) | struct | A body: blocks, instructions, values, stack slots (read access). |
| [`InstData`](#instdata), [`Terminator`](#terminator) | enums | Instruction and terminator data. |
| [`Type`](#type), [`Aggregate`](#aggregate), [`Signature`](#signature) | types | Value types, memory layout, signatures. |
| [`Policy`](#policy) | struct | The OPS policy every operation instance carries. |
| [`parse`](#parse) | function | Reads the textual form. |
| [`interp`](#interp-feature-interp) | module | Reference interpreter and per-operation semantics (feature `interp`). |
| [`ValidationError`](#validationerror), [`BuildError`](#builderror), [`ModuleError`](#moduleerror), [`ParseError`](#parseerror) | errors | Why something was refused. |

## Installation

```toml
[dependencies]
ir-lang = "2.0.0-alpha.3"                                              # the IR
ir-lang = { version = "2.0.0-alpha.3", features = ["interp"] }         # plus the interpreter
ir-lang = { version = "2.0.0-alpha.3", default-features = false }      # no_std + alloc
```

## Quick start

```rust
use ir_lang::{CmpOp, Linkage, Module, Overflow, Signature, Type};

// fn sum(n: i64) -> i64 { let mut s = 0; let mut i = 0; while i < n { s += i; i += 1 } s }
let mut m = Module::new("demo");
let f = m.declare_function("sum", &Signature::new(&[Type::I64], &[Type::I64]), Linkage::Export)?;
let mut b = m.build(f)?;
let n = b.param(0)?;
let head = b.create_block(&[Type::I64, Type::I64])?;
let body = b.create_block(&[])?;
let exit = b.create_block(&[])?;
let zero = b.iconst(Type::I64, 0)?;
b.jump(head, &[zero, zero])?;
b.switch_to(head)?;
let (s, i) = (b.block_param(head, 0)?, b.block_param(head, 1)?);
let more = b.compare(CmpOp::Lt, i, n)?;
b.branch(more, body, &[], exit, &[])?;
b.switch_to(body)?;
let s2 = b.add(s, i, Overflow::Wrap)?;
let one = b.iconst(Type::I64, 1)?;
let i2 = b.add(i, one, Overflow::Wrap)?;
b.jump(head, &[s2, i2])?;
b.switch_to(exit)?;
b.ret(&[s])?;
m.validate()?;
println!("{m}");
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Concepts

### Modules, functions, blocks, values

A [`Module`](#module) declares functions (defined or imported), globals, signatures, and
aggregates, and holds function bodies. A body ([`Function`](#function)) is blocks; each block
has typed **parameters**, instructions, and one [terminator](#terminator). Edges pass one
argument per target parameter; the parameters receive them all at once (a parallel copy), so
there are no phi nodes. The entry block's parameters are the function's parameters, and no edge
targets it. Every value has one type and one definition.

### Handles

Entities are named by dense `u32` handles (`Value`, `Block`, `Inst`, `StackSlot` per function;
`FuncId`, `GlobalId`, `SigId`, `AggId` per module). They are never reused: removal leaves a
tombstone. Only [`Builder::compact`](#buildercompact) renumbers, and it returns a
[`Remap`](#remap).

### Types

Integers are sized and signed or unsigned (`i8`..`i64`, `u8`..`u64`), as in OPS; every
signedness-dependent operation takes its signedness from its operand type. Floats are `f32` and
`f64`; `bool` is its own type; `ptr` is an untraced address and `ref` a GC reference, both as
wide as the module's [`PointerWidth`](#pointerwidth). Aggregates are memory-only. See IR.md §2.

### Policies

Every operation instance carries a [`Policy`](#policy) with exactly the fields its operation
needs for its operand class ([`Policy::required_binary`](#policy) and siblings). A field set to
`error` means the operation can raise an error, so it must be the operation of a `check`
terminator; `trap` aborts the program; `wrap`, `mask`, `saturate` never fail by themselves.
Two OPS v2 rules raise errors on **signed** types without an `error` field: integer `pow` with a
negative exponent (`NegativeExponent`, E0006, under **every** `overflow` policy, `trap`
included: `trap` covers an unrepresentable power, never a negative exponent), and `shl`/`shr`
under `shift = saturate` with a negative amount (`ShiftOutOfRange`). So on a signed type every
integer `pow` (`<overflow=wrap>`, `<overflow=error>`, `<overflow=trap>`) and
`shl<shift=saturate>` belong in a `check`; on an unsigned type they do not (except an `error`
field). The rule is [`BinaryOp::can_error`](#operations) (and `UnaryOp::can_error`,
`ConvOp::can_error`). `promote` does not exist here (every value is statically typed).

### Error and exception edges

- `check OP to normal(res0) error on_error(err)`: a fallible operation; the error edge receives
  the OPS code (`u32`).
- `invoke CALL to normal(res0, …) unwind pad(exn, …)`: a call whose exception goes to a landing
  pad; a landing pad is reached only by unwind edges.
- `resume payload`: unwinds out of the function.
- A plain `call` whose callee unwinds propagates the exception out of the caller.

### Safepoints and GC references

A call or invoke that is not `nogc`, and every `safepoint` instruction, is a safepoint. A `ref`
needed after a safepoint goes through its **GC list**; the safepoint defines the relocated
reference (after the call's returns), and the old value must not be used again. Pointers derived
from a `ref` (`ref_to_ptr`, then arithmetic) must not be live across any safepoint and must not
escape. The validator enforces both (IR.md §9). A front end may instead use references freely
and run [`Builder::insert_statepoints`](#builderinsert_statepoints), which fills the lists from
liveness and rewrites later uses to the relocated values.

### Rewriting

The same [`Builder`](#builder) rewrites an existing body ([`Module::edit`](#moduleedit)):
insert at a [`Position`](#position), [`replace_all_uses`](#builderreplace_all_uses) (O(1)
forwarding), [`remove_inst`](#builderremove_inst), [`replace_inst`](#builderreplace_inst),
[`split_block`](#buildersplit_block), [`move_inst`](#buildermove_inst), block-parameter edits
(one at a time, or any number in one linear pass with
[`remove_block_params`](#builderremove_block_params)), [`compact`](#buildercompact). Validate
between passes with [`Builder::validate`](#buildervalidate).

### The textual form

`Display for Module` prints a deterministic text; [`parse`](#parse) reads it back to the same
module with the same handles. The grammar is in IR.md §13.

---

## `Module`

A compilation unit. Declare first, then build bodies.

```rust
use ir_lang::{Aggregate, Global, Linkage, Module, Signature, Type};

let mut m = Module::new("app");
let point = m.declare_aggregate(Aggregate::structure(&[Type::F64.into(), Type::F64.into()]))?;
let origin = m.declare_global("origin", Global::zeroed(16, 8))?;
let f = m.declare_function("len", &Signature::new(&[Type::Ptr], &[Type::F64]), Linkage::Export)?;
assert_eq!(m.layout(point.into()).unwrap().size(), 16);
assert!(m.function(f).is_none()); // declared, no body yet
# let _ = origin;
# Ok::<(), ir_lang::ModuleError>(())
```

| Method | Description | Errors |
|---|---|---|
| `new(name)` | An empty module with 64-bit addresses. | — |
| `with_pointer_width(name, PointerWidth)` | An empty module with the given address width. | — |
| `name()`, `pointer_width()` | The module name; the address width. | — |
| `intern_signature(&Signature) -> SigId` | Interns a signature (equal signatures share a handle). | `CapacityExceeded` |
| `signature(SigId) -> Option<&Signature>`, `signature_count()` | Reads interned signatures. | — |
| `declare_aggregate(Aggregate) -> AggId` | Declares an aggregate and computes its layout. Components must be declared earlier. | `UnknownAggregate`, `AggregateTooLarge`, `CapacityExceeded` |
| `aggregate(AggId)`, `aggregate_count()` | Reads aggregates. | — |
| `layout(MemType) -> Option<Layout>` | Size and alignment of a scalar or aggregate. | — |
| `field_offset(AggId, field) -> Option<u64>` | Byte offset of a struct field. | — |
| `declare_function(name, &Signature, Linkage) -> FuncId` | Declares a function (signature interned). | `EmptyName`, `DuplicateName`, `CapacityExceeded` |
| `set_attrs(FuncId, Attrs)` | Sets a function's attributes. | `UnknownFunction` |
| `func_decl(FuncId) -> Option<&FuncDecl>`, `func_signature(FuncId)` | Reads a declaration. | — |
| `function(FuncId) -> Option<&Function>` | The body, if built. | — |
| `functions()`, `function_count()` | All function handles. | — |
| `declare_global(name, Global) -> GlobalId` | Declares a global. Relocation targets are checked by `validate`. | `EmptyName`, `DuplicateName`, `BadAlignment`, `InitSizeMismatch`, `RelocOutOfRange`, `ImportWithData`, `CapacityExceeded` |
| `global(GlobalId)`, `global_name(GlobalId)`, `globals()`, `global_count()` | Reads globals. | — |
| `lookup(name) -> Option<SymbolRef>` | A function or global by name. | — |
| `build(FuncId) -> Builder` | Creates the body (entry parameters from the signature); the builder is at the end of the entry block. | `UnknownFunction`, `CannotDefineImport`, `AlreadyDefined` |
| `edit(FuncId) -> Builder` | A builder over an existing body, for rewriting. | `UnknownFunction`, `NotDefined` |
| `validate()` | Checks the whole module (IR.md §11). | the first `ValidationError` |
| `validate_function(FuncId)` | Checks one body. | `ValidationError` |
| `display_function(FuncId)` | Prints one function. | — |
| `Display` | Prints the module in the textual form. | — |

### `Module::build`

Creates the body of a declared, non-imported function. While the returned `Builder` lives, the
module's declarations are borrowed: declare everything the body needs first.

### `Module::edit`

Returns a builder over an existing body (positioned at the end of the entry block) for
rewriting.

### `Module::validate`

Total (never panics), linear in the module (plus the live ranges of GC references for the
safepoint rule). Returns the first violation in the documented order.

```rust
use ir_lang::{Linkage, Module, Signature, ValidationErrorKind};

let mut m = Module::new("m");
let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
assert_eq!(m.validate().unwrap_err().kind(), &ValidationErrorKind::MissingBody);
let mut b = m.build(f)?;
b.ret(&[])?;
m.validate()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Declarations

### `FuncDecl`

A function declaration. Accessors: `name()`, `sig() -> SigId`, `linkage()`, `attrs()`.

### `Linkage`

`Local` (defined, module-private), `Export` (defined, visible), `Import` (defined elsewhere; no
body). `name()`/`from_name()` give the textual names. `#[non_exhaustive]`.

### `Global`

```rust
use ir_lang::{FuncId, Global, Linkage, Module, Reloc, SymbolRef};

let mut m = Module::new("m");
let msg = m.declare_global("msg", Global::bytes(b"hi\0", 1))?;
let table = m.declare_global(
    "table",
    Global::zeroed(8, 8)
        .mutable()
        .with_linkage(Linkage::Local)
        .with_reloc(Reloc { offset: 0, target: SymbolRef::Global(msg), addend: 1 }),
)?;
assert_eq!(m.global(table).unwrap().relocs.len(), 1);
# Ok::<(), ir_lang::ModuleError>(())
```

Fields: `linkage`, `size` (bytes), `align` (power of two ≤ 2^16), `mutable`, `init`, `relocs`.
Constructors: `zeroed(size, align)`, `bytes(&[u8], align)`, `import(size, align)`; builders
`mutable()`, `with_linkage()`, `with_reloc()`. Constant `MAX_ALIGN`.

### `Init`

`Zeroed` or `Bytes(Vec<u8>)` (exactly `size` bytes). `#[non_exhaustive]`.

### `Reloc`

`{ offset, target: SymbolRef, addend }`: the address of `target` plus `addend`, stored as a
pointer-width little-endian word at `offset`.

### `SymbolRef`

`Func(FuncId)` or `Global(GlobalId)`. `#[non_exhaustive]`.

## Type

`Bool, I8, I16, I32, I64, U8, U16, U32, U64, F32, F64, Ptr, Ref` (`#[non_exhaustive]`; vectors
reserved). Methods: `is_int`, `is_signed_int`, `is_unsigned_int`, `is_float`, `is_numeric`,
`int_bits`, `bits(PointerWidth)`, `size(PointerWidth)`, `int(bits, signed)`, `name`,
`from_name`; constant `ALL`.

```rust
use ir_lang::{PointerWidth, Type};

assert_eq!(Type::int(16, false), Some(Type::U16));
assert_eq!(Type::Ref.size(PointerWidth::P32), 4);
assert_eq!(Type::from_name("f64"), Some(Type::F64));
```

### `PointerWidth`

`P32` or `P64` (default). `bytes()`, `bits()`, `is_address_int(Type)` (the integer side of
`ptr_to_int`/`int_to_ptr`).

### `MemType`

`Scalar(Type)` or `Agg(AggId)`; `From<Type>` and `From<AggId>`.

### `Aggregate`

`Struct { fields, packed }` or `Array { elem, len }`; constructors `structure`, `packed`,
`array`. Layout follows the C rule (packed: alignment 1, no padding); an aggregate may name only
earlier aggregates. Sizes are capped at `MAX_AGGREGATE_SIZE` (2^48).

### `Layout`

`size()` and `align()` in bytes.

### `Signature`

`{ params, returns, conv }`; `new(params, returns)` (convention `Fast`), `with_conv(CallConv)`.
`Display` prints `(i64) -> (bool) fast`.

### `CallConv`

`Fast` (default), `C`, `SysV64`, `Win64`, `Aapcs64` (`#[non_exhaustive]`); `name`, `from_name`,
`ALL`.

## `Function`

Read access to a body (get one from [`Module::function`](#module) or
[`Builder::func`](#builder)).

| Method | Description |
|---|---|
| `entry()` | The entry block. |
| `block_count()` | Block handles ever created (`0..block_count()`). |
| `blocks()` | Live blocks in handle order. |
| `is_live_block(Block)` | Exists and not removed. |
| `block_params(Block) -> &[Value]` | A block's parameters (empty for unknown blocks). |
| `insts(Block) -> Insts` | A block's instructions in order (the terminator is separate). |
| `first_inst`, `last_inst`, `next_inst`, `prev_inst` | Layout navigation. |
| `inst_count()` | Instruction handles ever created. |
| `inst(Inst) -> Option<&InstData>` | An instruction's data. |
| `inst_block(Inst) -> Option<Block>` | Its block (`None` once removed). |
| `results(Inst) -> ValueRange` | The values it defines. |
| `terminator(Block) -> Option<&Terminator>` | A block's terminator. |
| `value_count()`, `value_type(Value)`, `value_def(Value) -> Option<ValueDef>`, `defining_inst(Value)` | Values. |
| `resolve(Value) -> Value` | Follows `replace_all_uses` forwarding. |
| `slot_count()`, `slot(StackSlot) -> Option<SlotData>` | Stack slots. |
| `predecessors() -> Vec<Vec<Block>>` | Predecessors of every block, once per edge. |

```rust
use ir_lang::{Linkage, Module, Overflow, Signature, Type};

let mut m = Module::new("m");
let f = m.declare_function("f", &Signature::new(&[Type::I64], &[Type::I64]), Linkage::Export)?;
let mut b = m.build(f)?;
let x = b.param(0)?;
let y = b.add(x, x, Overflow::Wrap)?;
b.ret(&[y])?;
let func = m.function(f).unwrap();
let entry = func.entry();
let inst = func.insts(entry).next().unwrap();
assert_eq!(func.results(inst).first(), Some(y));
assert_eq!(func.value_type(y), Some(Type::I64));
# Ok::<(), Box<dyn std::error::Error>>(())
```

### `ValueDef`

`Param { block, index }`, `Result { inst, index }`, or `Detached` (its definition was removed).

### `SlotData`

`{ size, align }` of a stack slot; `MAX_ALIGN` = 2^16.

### `Insts`

The iterator returned by `Function::insts`.

### `MAX_ENTITIES`

The largest number of values, blocks, instructions, or slots per function (`u32::MAX - 1`).

## Handles

`Value` (`v3`), `Block` (`b3`), `Inst` (`inst3`), `StackSlot` (`ss3`), `FuncId` (`f3`),
`GlobalId` (`g3`), `SigId` (`s3`), `AggId` (`t3`), `Variable` (`var3`, scoped to one
[`Builder`](#ssa-construction)). Each: `from_u32`, `index`, `as_u32`,
`Display`, `Copy + Ord + Hash`. `Value::next()` gives the next consecutive value.

### `ValueRange`

The consecutive results of one instruction: `len`, `is_empty`, `get(i)`, `first`, `contains`,
`iter`, `IntoIterator`.

## `Builder`

Lowers into and rewrites one body. Every instruction and terminator is checked when added (by
the validator's own code); a refused operation changes nothing.

### Reading and positioning

| Method | Description | Errors |
|---|---|---|
| `func()`, `id()`, `signature()` | The body, its handle, its signature. | — |
| `param(i)` | The `i`-th function parameter. | `UnknownParam` |
| `block_param(block, i)` | A block parameter. | `UnknownBlockParam` |
| `results(Inst)` | An instruction's values. | — |
| `position()`, `current_block()` | The insertion point. | — |
| `set_position(Position)` | Move it. | `UnknownBlock`, `UnknownInst` |
| `switch_to(block)` | Move to the end of `block`. | `UnknownBlock` |
| `validate()` | Validate this body against the module's declarations. | `ValidationError` |

### Blocks

| Method | Description | Errors |
|---|---|---|
| `create_block(&[Type])` | A new block with parameters. | `CapacityExceeded` |
| `add_block_param(block, Type)` | Append a parameter (edges must then pass it). | `UnknownBlock`, `VariablesActive` (the block has construction parameters) |
| `remove_block_param(block, i)` | Remove a parameter and the matching argument of every incoming edge (linear in the blocks). | `UnknownBlock`, `UnknownBlockParam` |
| `remove_block_params(&[Value])` | Remove any number of parameters, from any blocks, and their edge arguments, in one linear pass. | `NotBlockParam` (nothing changed) |
| `remove_block(block)` | Detach its instructions and parameters; tombstone it. | `EntryBlock`, `UnknownBlock` |
| `split_block(at: Inst) -> Block` | Move `at`, the rest, and the terminator to a new block; jump to it. | `UnknownInst`, `VariablesActive` (unsealed blocks) |

### Instructions

All return `Result<Value, BuildError>` (single-result instructions) or `Result<Inst,
BuildError>` (zero or several results; read them with `results`). Errors: `Invalid(kind)` for an
ill-typed instruction (see [`ValidationErrorKind`](#validationerrorkind)), a stale position, or
`CapacityExceeded`.

| Method | Instruction |
|---|---|
| `insert(InstData) -> Inst` | Any instruction (generic door). |
| `iconst(Type, i128)`, `f64const(f64)`, `f32const(f32)`, `bconst(bool)`, `null(Type)` | Constants (`iconst` checks the range of the type). |
| `unary(UnaryOp, Policy, a)`, `binary(BinaryOp, Policy, a, b)` | OPS operations. |
| `add`, `sub`, `mul (a, b, Overflow)` | Shortcuts: the overflow policy for integers, none for floats. |
| `compare(CmpOp, a, b)`, `fma(a, b, c)`, `select(c, t, f)` | Comparison, fused multiply-add, select. |
| `convert(ConvOp, Policy, a, Type)` | Conversions. |
| `stack_slot(size, align) -> StackSlot`, `stack_slot_for(MemType)` | Frame memory. |
| `stack_addr`, `global_addr`, `func_addr`, `field_addr`, `elem_addr`, `ptr_offset` | Addresses. |
| `load(Type, addr, MemFlags)`, `store(v, addr, MemFlags) -> Inst` | Memory. |
| `atomic_load`, `atomic_store -> Inst`, `atomic_rmw`, `cmpxchg -> Inst`, `fence -> Inst` | Atomics. |
| `memcpy`, `memmove`, `memset -> Inst` | Bulk memory. |
| `call(Call) -> Inst` | A call (results: returns, then relocated refs). |
| `safepoint(&[Value]) -> Inst` | A GC poll relocating references. |

### Terminators

All return `Result<(), BuildError>`; `AlreadyTerminated` if the current block has one (never
overwritten — M42).

| Method | Terminator |
|---|---|
| `set_terminator(Terminator)` | Any terminator (generic door). |
| `remove_terminator(block) -> Option<Terminator>` | Take one off explicitly. |
| `jump`, `branch`, `switch`, `ret`, `unreachable`, `trap` | Control flow. |
| `tail_call(Call)`, `invoke(Call, normal, unwind)`, `check(InstData, normal, error)`, `resume(payload)` | Calls, exceptions, errors. |

### `Builder::move_inst`

`move_inst(inst, Position)`: unlinks an instruction and links it at the position, in O(1); its
handle and results are kept, dominance is the validator's to check. A position naming the moved
instruction moves with it; moving next to itself does nothing. Errors: `UnknownInst`,
`UnknownBlock`.

```rust
use ir_lang::{Linkage, Module, Position, Signature, Type};

let mut m = Module::new("m");
let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
let mut b = m.build(f)?;
let x = b.iconst(Type::I32, 1)?;
let y = b.iconst(Type::I32, 2)?;
let (xi, yi) = (b.func().defining_inst(x).unwrap(), b.func().defining_inst(y).unwrap());
b.move_inst(yi, Position::Before(xi))?;
assert_eq!(b.func().insts(b.func().entry()).collect::<Vec<_>>(), [yi, xi]);
# Ok::<(), Box<dyn std::error::Error>>(())
```

### `Builder::remove_block_params`

`remove_block_params(&[Value])` removes the given parameters (any blocks, duplicates ignored)
and the matching argument of every edge into their blocks in one pass over the function, so a
pass dropping thousands of dead parameters stays linear. The values become detached. Errors:
`NotBlockParam` if any value is not a live block's parameter (checked first; nothing changes).

### SSA construction

Braun et al., "Simple and Efficient Construction of SSA Form" (CC 2013), with block parameters
for phis, built into the `Builder`. A front end lowers a mutable local as a `Variable`:

| Method | Description | Errors |
|---|---|---|
| `declare_var(Type) -> Variable` | A new variable. | `CapacityExceeded` |
| `def_var(Variable, Value)` | Assign in the current block (types must match). | `UnknownVariable`, `UnknownValue`, `Invalid(TypeMismatch)`, `AlreadyTerminated` |
| `use_var(Variable) -> Value` | The current value in the current block; adds block parameters where paths meet. | `UnknownVariable`, `CapacityExceeded` |
| `seal_block(Block)` | Every edge into the block exists: complete its incomplete parameters; later edges into it are refused. Idempotent; the entry is always sealed. | `UnknownBlock` |
| `is_sealed(Block) -> bool` | Whether sealed. | — |
| `seal_all_blocks()` | Seal everything, then remove the construction's trivial parameters to a fixpoint and rewrite operands. | `CapacityExceeded` |

Rules of use:

- Lower in order: `def_var` and `use_var` act at the end of the current block.
- Seal a block once all its predecessors are wired (a loop header after its back edge); sealing
  early gives fewer parameters, sealing late is always correct.
- An edge passes arguments for its target's **own** parameters only; the builder appends the
  variables' values for the parameters it added (a terminator into a sealed block is refused
  with `SealedBlock`).
- A parameter cannot be added to a block after the construction added one (`VariablesActive`);
  create blocks with their own parameters first.
- A variable read where it was never assigned reads zero of its type (`0`, `false`, `+0.0`,
  `null`), defined at the start of the entry block.
- A returned value may later be forwarded (a parameter that turned out trivial); operands are
  rewritten by `seal_all_blocks`, and handles kept in a front end's own tables resolve with
  `Function::resolve`.
- `split_block` ends the construction once every block is sealed (refused before); `compact`
  ends it unconditionally. `Variable`s are unknown afterwards.

Every walk is iterative (a 60,000-block chain and 400 nested loops are in the tests). Definitions
are kept in an open-addressing table keyed by (variable, block).

```rust
use ir_lang::{Linkage, Module, Signature, Type};

// let mut x = a; if c { x = 1 } return x
let mut m = Module::new("m");
let f = m.declare_function("f", &Signature::new(&[Type::I32, Type::Bool], &[Type::I32]), Linkage::Export)?;
let mut b = m.build(f)?;
let x = b.declare_var(Type::I32)?;
let (a, c) = (b.param(0)?, b.param(1)?);
b.def_var(x, a)?;
let (then, join) = (b.create_block(&[])?, b.create_block(&[])?);
b.branch(c, then, &[], join, &[])?;
b.switch_to(then)?;
b.seal_block(then)?;
let one = b.iconst(Type::I32, 1)?;
b.def_var(x, one)?;
b.jump(join, &[])?;
b.switch_to(join)?;
b.seal_block(join)?;
let v = b.use_var(x)?; // a parameter of `join`: `a` from the entry, `1` from `then`
b.ret(&[v])?;
b.seal_all_blocks()?;
assert_eq!(b.func().block_params(join), &[v]);
m.validate()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### `Builder::insert_statepoints`

`insert_statepoints() -> Result<StatepointStats, BuildError>`: makes every safepoint's GC list
hold exactly the references that need it (IR.md §9 rule 1) and rewrites every later use to the
relocated reference. In order: reference liveness exactly as the validator defines it; each
safepoint's list gains the references live across it (entries already present are kept and
reused; a `call`/`safepoint` whose list grows is replaced, its old results forwarding to the
new ones; an `invoke` passes relocated references as `resN` on both edges); then SSA repair over
the finished graph, adding block parameters where relocated versions meet, and removing the
trivial ones. Variable construction in progress is finished first.

The validator is the oracle: the result is validated, and on any error the function is restored
exactly. A *derived* pointer live across a safepoint cannot be relocated (it must be recomputed
from the relocated reference), so it is refused with `Invalid(LiveAcrossSafepoint)`; input that
is invalid for any reason other than the safepoint rules is refused with its first validation
error. Cost: linear in the function plus the references' live ranges.

**Budget.** The output can be quadratic in the input (every reference live across every
safepoint), so the work is capped: `insert_statepoints()` uses `DEFAULT_STATEPOINT_BUDGET`
(2^26) and `insert_statepoints_within(budget)` takes an explicit one, counting (reference,
safepoint) relocations plus block parameters created. Over budget it returns `BudgetExceeded`
with the function unchanged. Variable construction finished at the start is kept either way.

`StatepointStats { safepoints, relocations, params }` (`#[non_exhaustive]`, `Default`): the
safepoints whose lists grew, the entries added, and the block parameters added (after trivial
ones are removed). A second run on its own output returns all zeros.

```rust
use ir_lang::{BlockArg, BlockCall, Call, Linkage, Module, Signature, Type};

let mut m = Module::new("m");
let thrower = m.declare_function("thrower", &Signature::new(&[], &[]), Linkage::Import)?;
let f = m.declare_function("f", &Signature::new(&[Type::Ref], &[Type::Ref]), Linkage::Export)?;
let mut b = m.build(f)?;
let r = b.param(0)?;
let (ok, pad) = (b.create_block(&[])?, b.create_block(&[Type::Ptr])?);
b.invoke(Call::direct(thrower, &[]), BlockCall::new(ok, &[]), BlockCall::with_args(pad, &[BlockArg::Exception]))?;
b.switch_to(ok)?;
b.ret(&[r])?; // used after the call on the normal edge, without relocation
b.switch_to(pad)?;
let exn = b.block_param(pad, 0)?;
b.resume(exn)?;
let stats = b.insert_statepoints()?;
assert_eq!((stats.relocations, stats.params), (1, 1));
assert!(m.to_string().contains("invoke @thrower() gc(v0) to b1(res0) unwind b2(exn)"));
m.validate()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### `Builder::replace_all_uses`

`replace_all_uses(old, new)`: every use of `old` reads `new`, in O(1) (forwarding). Types must
match; making two values that already resolve alike forward is a no-op; a cycle is impossible.
Errors: `UnknownValue`, `Invalid(TypeMismatch)`.

### `Builder::remove_inst`

Unlinks an instruction; its results become detached (a remaining use is a validation error).
Errors: `UnknownInst`.

### `Builder::replace_inst`

Replaces an instruction's data in place, keeping its results; the new data must produce the same
result types. Errors: `UnknownInst`, `Invalid`.

### `Builder::split_block`

See the table above; no handle changes, and every use stays dominated. With variable
construction in use it is refused while blocks are unsealed, and ends the construction
otherwise.

### `Builder::resolve_aliases`

Rewrites every operand to its final forwarding target.

### `Builder::compact`

Renumbers densely (entry, then live blocks by handle; instructions and values in layout order),
drops tombstones and forwarding, and returns the [`Remap`](#remap). A compacted module prints,
parses back, and compares equal as text. Variable construction state is dropped (call
`seal_all_blocks` first).

```rust
use ir_lang::{Linkage, Module, Signature, Type};

let mut m = Module::new("m");
let f = m.declare_function("f", &Signature::new(&[], &[Type::I32]), Linkage::Export)?;
let mut b = m.build(f)?;
let dead = b.iconst(Type::I32, 1)?;
let live = b.iconst(Type::I32, 2)?;
b.ret(&[live])?;
let i = b.func().defining_inst(dead).unwrap();
b.remove_inst(i)?;
let remap = b.compact();
assert_eq!(remap.value(dead), None);
assert_eq!(remap.value(live).map(|v| v.index()), Some(0));
# Ok::<(), Box<dyn std::error::Error>>(())
```

### `Builder::validate`

Validates the body being built or rewritten, as `Module::validate_function` would.

### `Position`

`End(Block)`, `Before(Inst)`, `After(Inst)` (`#[non_exhaustive]`). Consecutive insertions keep
their order in every mode.

### `Remap`

`value(old)`, `block(old)`, `inst(old)`: the new handle, or `None` if dropped.

## `InstData`

The data of a non-terminator instruction (`#[non_exhaustive]`). Variants, with typing (IR.md §5–§6):

| Variant | Text | Results |
|---|---|---|
| `Const { ty, bits }` | `const 42` | `ty` |
| `Unary { op, policy, arg }` | `neg<overflow=wrap> v1` | operand type |
| `Binary { op, policy, args }` | `add<overflow=wrap> v1, v2` | operand type (`total_cmp`: `i8`) |
| `Fma { args }` | `fma v1, v2, v3` | operand type |
| `Compare { op, args }` | `lt v1, v2` | `bool` |
| `Convert { op, policy, to, arg }` | `int_cast<overflow=error> v1` | `to` |
| `Select { cond, args }` | `select v0, v1, v2` | operand type |
| `StackAddr`, `GlobalAddr`, `FuncAddr` | `stack_addr ss0`, `global_addr @g`, `func_addr @f` | `ptr` |
| `FieldAddr { base, agg, field }` | `field_addr v1, t0, 2` | `ptr` |
| `ElemAddr { base, elem, index }` | `elem_addr v1, i64, v2` | `ptr` |
| `PtrOffset { base, offset }` | `ptr_offset v1, v2` | `ptr` |
| `Load { ty, addr, flags }` / `Store { value, addr, flags }` | `load v1 align 8` / `store v2, v1 align 8` | `ty` / none |
| `AtomicLoad`, `AtomicStore`, `AtomicRmw`, `CmpXchg`, `Fence` | `atomic_rmw add v1, v2 seq_cst` | as the op (`cmpxchg`: value, `bool`) |
| `MemCopy`, `MemMove`, `MemSet` | `memcpy v1, v2, v3 align 1` | none |
| `Call(Box<Call>)` | `call @f(v1) gc(v2) [cold]` | returns, then relocated refs |
| `Safepoint { gc }` | `safepoint gc(v1)` | relocated refs |

Methods: `for_each_operand`, `map_operands`, `has_side_effects` (writes, volatile reads,
synchronization, calls, safepoints, or a `trap`/`error` policy), `opcode`.

## `Terminator`

`Jump(BlockCall)`, `Branch { cond, then_dest, else_dest }`, `Switch { value, cases, default }`,
`Return { values }`, `Unreachable`, `Trap { code }`, `TailCall(Box<Call>)`,
`Invoke { call, normal, unwind }`, `Check { op, normal, error }`, `Resume { payload }`
(`#[non_exhaustive]`). Methods: `successors()` (cases, then default; then/else; normal, then
unwind/error), `for_each_successor_mut`, `for_each_operand`, `map_operands`, `opcode`.

## Edges

### `BlockCall`

`{ block, args: Vec<BlockArg> }`; `new(block, &[Value])`, `with_args(block, &[BlockArg])`.

### `BlockArg`

`Value(Value)`, `Result(u32)` (`resN`), `Exception` (`exn`), `ErrorCode` (`err`)
(`#[non_exhaustive]`). Where each is allowed is in IR.md §7.

```rust
use ir_lang::{Block, BlockArg, BlockCall};

let normal = BlockCall::with_args(Block::from_u32(1), &[BlockArg::Result(0)]);
assert_eq!(normal.to_string(), "b1(res0)");
```

### `SwitchCase`

`{ value, dest }`; `value` is the raw bits of the case.

### `Successors`

The iterator returned by `Terminator::successors`.

## Calls

### `Call`

`{ target, args, gc, attrs }`; `direct(FuncId, &[Value])`, `indirect(SigId, Value, &[Value])`,
`with_gc(&[Value])`, `with_attrs(Attrs)`.

### `CallTarget`

`Direct(FuncId)` or `Indirect { sig, callee }` (`#[non_exhaustive]`).

### `Attrs`

A bit set: `NOUNWIND`, `NORETURN`, `NOGC` (checked), `READONLY`, `READNONE`, `COLD`,
`NOINLINE`, `INLINE` (hints). `contains`, `union`, `|`, `is_empty`, `from_name`, `bits`;
`Display` prints `[nounwind, cold]`.

## Memory

### `MemFlags`

`{ align, volatile }`; `new(align)`, `volatile()`; `MAX_ALIGN` = 2^29.

### `MemOrder`

`Relaxed`, `Acquire`, `Release`, `AcqRel`, `SeqCst`; `name`, `from_name`, `strength`.

### `RmwOp`

`Xchg`, `Add`, `Sub`, `And`, `Or`, `Xor`, `Nand`, `Min`, `Max`; `name`, `from_name`.

## Operations

`UnaryOp` (`neg abs not sqrt floor ceil trunc round round_even`), `BinaryOp` (`add sub mul div
rem floor_div floor_mod ieee_rem and or xor shl shr min max total_cmp pow`), `CmpOp` (`eq ne lt
le gt ge`), `ConvOp` (`int_cast zext sext narrow int_to_float float_to_int float_cast bitcast
char_from_u32 bool_to_int ptr_to_int int_to_ptr ref_to_ptr`). Each has `ALL`, `name`,
`from_name`, `Display`; all are `#[non_exhaustive]`. Semantics: OPS §3–§5 (v2).

`pow` (OPS v2) takes two operands of one integer or float type. Integer: the exact power,
`0 ** 0 = 1`, an unrepresentable result per `overflow` (`wrap` keeps the low bits); a negative
exponent is the `NegativeExponent` error (E0006) under every `overflow` policy, `trap` included
(OPS §3, §6), so a signed integer `pow` is always the operation of a `check`. Float:
[`ls_pow`](#ls_pow), no policy; `f32` widens, computes, and rounds once.

Fallibility (what must be in a `check`): `UnaryOp::can_error(Policy)`,
`BinaryOp::can_error(Policy, Type)`, `ConvOp::can_error(Policy)`.

```rust
use ir_lang::{BinaryOp, ConvOp, Overflow, Policy, Shift, Type, UnaryOp};

assert!(BinaryOp::Pow.can_error(Policy::overflow(Overflow::Wrap), Type::I64));
assert!(!BinaryOp::Pow.can_error(Policy::overflow(Overflow::Wrap), Type::U64));
assert!(BinaryOp::Pow.can_error(Policy::overflow(Overflow::Trap), Type::I64)); // since alpha.3
assert!(!BinaryOp::Pow.can_error(Policy::overflow(Overflow::Trap), Type::U64));
assert!(BinaryOp::Shr.can_error(Policy::shift(Shift::Saturate), Type::I32));
assert!(ConvOp::CharFromU32.can_error(Policy::NONE));
assert!(!UnaryOp::Neg.can_error(Policy::overflow(Overflow::Trap)));
```

## Policy

```rust
use ir_lang::{BinaryOp, DivZero, Overflow, Policy, Type};

let p = Policy::overflow(Overflow::Wrap).with_div_zero(DivZero::Trap);
assert!(Policy::required_binary(BinaryOp::Div, Type::I32).matches(p));
assert!(!p.can_error());
assert_eq!(p.to_string(), "<overflow=wrap,div_zero=trap>");
```

`Policy { overflow, div_zero, shift, float_to_int }` (each optional). Constructors `NONE`,
`overflow`, `div_zero`, `shift`, `float_to_int`, `with_div_zero`; `can_error` (whether any field
is `error`; see [Operations](#operations) for the full fallibility rule);
`required_unary(UnaryOp, Type)`, `required_binary(BinaryOp, Type)`,
`required_conversion(ConvOp)` give the [`PolicyFields`](#policy) an instance must carry
(`matches`, `is_empty`). Field values: `Overflow { Error, Wrap, Trap }`,
`DivZero { Error, Trap }`, `Shift { Error, Mask, Saturate }`, `FloatToInt { Error, Saturate }`,
each with `name`/`from_name`. `Shift::Saturate` (OPS v2): an amount at least the width gives `0`
(`shl`, and `shr` of a non-negative value) or `-1` (`shr` of a negative signed value); a
negative amount is still `ShiftOutOfRange`.

### `OpError`

`ArithOverflow` (1), `DivByZero` (2), `ShiftOutOfRange` (3), `InvalidConversion` (4),
`InvalidChar` (5), `NegativeExponent` (6, OPS v2): the OPS §6 codes. `code`, `from_code`;
`Display` prints `E0001 ArithOverflow`.

## `ls_pow`

`ls_pow(x: f64, y: f64) -> f64`: the float `pow` of OPS v2, specified operation by operation in
`_lexersketch/specs/ops-vectors/pow.md` so every tier produces the same bits (OPS forbids a
platform libm). C99 Annex F special cases first; a NaN result is the canonical
`0x7FF8_0000_0000_0000`; otherwise correctly rounded except within about 2^-85 of a rounding
boundary, where it is still deterministic. Needs only `core`, so `no_std` optimizers can fold
`pow` with it. Checked against all 122 vectors of the spec.

```rust
use ir_lang::ls_pow;

assert_eq!(ls_pow(2.0, -1.0), 0.5);
assert_eq!(ls_pow(-0.0, -3.0), f64::NEG_INFINITY);
assert_eq!(ls_pow(1.0, f64::NAN), 1.0);
// f32: widen both operands, compute, round once.
assert_eq!(ls_pow(f64::from(3.0f32), f64::from(2.0f32)) as f32, 9.0f32);
```

## Errors

### `ModuleError`

Why a declaration was refused: `DuplicateName`, `EmptyName`, `UnknownFunction`,
`UnknownGlobal`, `UnknownAggregate`, `AggregateTooLarge`, `BadAlignment`, `InitSizeMismatch`,
`RelocOutOfRange`, `ImportWithData`, `CannotDefineImport`, `AlreadyDefined`, `NotDefined`,
`CapacityExceeded`.

### `BuildError`

Why a builder operation was refused (nothing changed): `AlreadyTerminated`, `UnknownBlock`,
`UnknownInst`, `UnknownValue`, `UnknownParam`, `EntryBlock`, `UnknownBlockParam`,
`Invalid(ValidationErrorKind)`, `CapacityExceeded`; new in 2.0.0-alpha.2: `UnknownVariable`,
`SealedBlock` (an edge into a sealed block), `NotBlockParam`, `VariablesActive` (see
[SSA construction](#ssa-construction)), `BudgetExceeded` (see
[`insert_statepoints`](#builderinsert_statepoints)).

```rust
use ir_lang::{BuildError, Linkage, Module, Overflow, Signature, Type};

let mut m = Module::new("m");
let f = m.declare_function("f", &Signature::new(&[Type::I32], &[]), Linkage::Export)?;
let mut b = m.build(f)?;
let x = b.param(0)?;
// An `error` policy needs a `check` terminator.
assert!(matches!(b.add(x, x, Overflow::Error), Err(BuildError::Invalid(_))));
# Ok::<(), Box<dyn std::error::Error>>(())
```

### `ValidationError`

`func()`, `block()`, `inst()` (where), `kind()` (what); `Display` prints `f1 b2 inst3: …`.

### `ValidationErrorKind`

One variant per rule (IR.md §11), `#[non_exhaustive]`:

| Group | Variants |
|---|---|
| Module | `DuplicateName`, `EmptyName`, `UnknownSignature`, `UnknownFunction`, `UnknownGlobal`, `UnknownAggregate`, `BadAggregate`, `BadAlignment`, `BadGlobal`, `MissingBody`, `ImportWithBody` |
| Structure | `BadEntry`, `EntryParams`, `MissingTerminator`, `BadBlockTarget`, `EntryBranchTarget`, `CorruptLayout`, `ValueOutOfRange`, `InconsistentDefinition`, `DefinedTwice`, `UseOfDetached`, `BadAlias`, `UseBeforeDef`, `BadSlot` |
| Typing | `TypeMismatch`, `BadOperandType`, `BadConversion`, `PolicyMismatch`, `ErrorOutsideCheck`, `NotFallible`, `BadConst`, `BadField`, `BadOrdering`, `ResultMismatch`, `ArgCount`, `EdgeArgCount`, `BadSwitchCase`, `BadTailCall`, `BadGcList` |
| Exception edges | `BadEdgeArg`, `LandingPadPredecessor`, `UnwindInNounwind`, `ReturnInNoreturn`, `SafepointInNogc` |
| Safepoints | `LiveAcrossSafepoint`, `DerivedPointerEscapes` |

## `parse`

`parse(&str) -> Result<Module, ParseError>` and `parse_with(&str, &ParseLimits)`. Syntax only;
call `validate` afterwards. The printer's output parses back to the same module with the same
handles.

```rust
let text = "module \"m\" ptr64\nsig s0 = (i32) -> (i32) fast\n\nfunc @id s0 export {\nb0(v0: i32):\n    ret v0\n}\n";
let m = ir_lang::parse(text)?;
m.validate()?;
assert_eq!(m.to_string(), text);
# Ok::<(), Box<dyn std::error::Error>>(())
```

### `ParseLimits`

`{ max_bytes (default 256 MiB), max_id (default 2^26) }`. Entity ids are also capped at
`64 × input length + 4096`.

### `ParseError`

`line()`, `column()` (1-based, characters), `kind()`.

### `ParseErrorKind`

`InputTooLarge`, `UnexpectedChar`, `UnterminatedString`, `BadEscape`, `Expected`,
`UnknownOpcode`, `UnknownType`, `UnknownSymbol`, `BadNumber`, `IdTooLarge`, `OutOfOrder`,
`Duplicate`, `Undefined`, `NonConsecutiveResults`, `MissingResult`, `Module(ModuleError)`
(`#[non_exhaustive]`).

## `interp` (feature `interp`)

The reference interpreter: OPS semantics exactly, undefined behaviour reported as errors,
iterative, budgeted.

```rust
# #[cfg(feature = "interp")] {
use ir_lang::interp::{self, Outcome, Val};
use ir_lang::{Linkage, Module, Overflow, Signature, Type};

let mut m = Module::new("m");
let f = m.declare_function("twice", &Signature::new(&[Type::U8], &[Type::U8]), Linkage::Export)?;
let mut b = m.build(f)?;
let x = b.param(0)?;
let y = b.add(x, x, Overflow::Trap)?;
b.ret(&[y])?;
assert_eq!(interp::run(&m, f, &[Val::u8(100)])?, Outcome::Return(vec![Val::u8(200)]));
assert_eq!(interp::run(&m, f, &[Val::u8(200)])?, Outcome::Trap(1));
# }
# Ok::<(), Box<dyn std::error::Error>>(())
```

| Item | Description |
|---|---|
| `run(&Module, FuncId, &[Val]) -> Result<Outcome, InterpError>` | Tier-1: default limits, no host. |
| `Interpreter::new(&Module, H)`, `with_limits(…, Limits)` | Validates the module and lays out globals (relocations applied). Errors: `Invalid`, `OutOfMemory`. |
| `Interpreter::call(FuncId, &[Val])` | Runs a function to completion; memory persists across calls. |
| `memory()`, `memory_mut()`, `global_address(GlobalId)`, `fuel_used()`, `host_mut()` | Inspection. |
| `Limits { fuel, max_depth, memory }` | Defaults 10M, 1000, 4 MiB (memory capped at 4 GiB). |
| `Outcome` | `Return(Vec<Val>)`, `Unwind(Val)`, `Trap(u32)`. |
| `InterpError` | `Invalid`, `UnresolvedImport`, `BadArguments`, `FuelExhausted`, `StackOverflow`, `OutOfMemory`, `MemoryFault`, `Misaligned`, `Overlap`, `InvalidBool`, `UnreachableReached`, `StaleRef`, `BadFunctionPointer`, `UncheckedError`, `Host`. |
| `Val` | A typed bit pattern: `new`, `i8`…`u64`, `f32`, `f64`, `bool`, `ptr`; `ty`, `bits`, `as_i128`, `as_f64`, `as_bool`, `same_as` (NaN-equivalent). |
| `Memory` | `read`, `write`, `alloc` (host heap), `alloc_object` (a GC object; returns a `ref`), `ref_address`. |
| `Host`, `HostReturn`, `NoHost` | Implement imported functions. |
| `eval_unary`, `eval_binary`, `eval_compare`, `eval_convert`, `eval_fma`, `OpFault` | Per-operation OPS semantics on bits, for constant folding (OPS v2: `pow` via `ls_pow`, `shift = saturate`, `NegativeExponent`). `OpFault::Trap` only for a `trap` policy field; a negative `pow` exponent is `OpFault::Error(NegativeExponent)` under every policy. |

Every safepoint starts a new GC epoch; using a reference from an earlier epoch is `StaleRef`, so
a missing relocation is caught at run time.

## Feature flags

| Feature | Default | Effect |
|---|---|---|
| `std` | yes | The standard library; without it the crate is `no_std` + `alloc`. |
| `interp` | no | The [`interp`](#interp-feature-interp) module (implies `std`). |

## Limits

| Limit | Value |
|---|---|
| Entities per function (values, blocks, instructions, slots) | `MAX_ENTITIES` = 2^32 − 2 |
| Aggregate size | 2^48 bytes |
| Global / stack slot alignment | 2^16 |
| Access alignment (`MemFlags`) | 2^29 |
| Parser input | 256 MiB by default (`ParseLimits`) |
| Statepoint insertion work | `DEFAULT_STATEPOINT_BUDGET` = 2^26 relocations + parameters (`insert_statepoints_within` to change) |

## Stability

`2.0.0-alpha.3` is a pre-release. Breaking changes may land before `2.0.0`; each is recorded in
[`CHANGELOG.md`](../CHANGELOG.md) (2.0.0-alpha.2 lists those since alpha.1, 2.0.0-alpha.3 those
since alpha.2). Every public enum is `#[non_exhaustive]`, so from `2.0.0` on
new types, operations, terminators, and error kinds are additive. MSRV is 1.85.
