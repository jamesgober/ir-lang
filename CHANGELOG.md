<h1 align="center">
    <img width="90px" height="auto" src="https://raw.githubusercontent.com/jamesgober/jamesgober/main/media/icons/hexagon-3.svg" alt="Triple Hexagon">
    <br><b>CHANGELOG</b>
</h1>
<p>
  All notable changes to <code>ir-lang</code> will be documented in this file. The format is based on <a href="https://keepachangelog.com/en/1.1.0/">Keep a Changelog</a>,
  and this project adheres to <a href="https://semver.org/spec/v2.0.0.html/">Semantic Versioning</a>.
</p>

---

## [Unreleased]

### Added

### Changed

### Fixed

### Security

---

## [2.0.0-alpha.2] - 2026-10-11

OPS v2 in the IR, SSA construction for mutable locals, and automatic statepoint
insertion: the gaps the alpha.1 consumers (opt-lang 0.2.0, backend-lang 0.3.0)
and the HIR lowering (irgen-lang, ISSUES P29) reported. Normative spec:
`_lexersketch/specs/IR.md` (updated); operation semantics `specs/OPS.md` v2.

### Breaking (vs 2.0.0-alpha.1)

- **Which operations need a `check` now depends on the operand type, not only
  the policy.** On a **signed** integer type, `pow` under `overflow = wrap` or
  `error` (a negative exponent raises `NegativeExponent`) and `shl`/`shr` under
  `shift = saturate` (a negative amount raises `ShiftOutOfRange`) can raise an
  error without an `error` field, so they are valid only as the operation of a
  `check` terminator, and as plain instructions they are refused
  (`ErrorOutsideCheck`). On unsigned types they are plain instructions. Code that
  decides "plain instruction or `check`" with `Policy::can_error()` must use
  `BinaryOp::can_error(policy, ty)` (and `UnaryOp::can_error`,
  `ConvOp::can_error`). `Policy::can_error` itself is unchanged (any field is
  `error`); only its documentation now says it is not the whole rule.
- **OPS error code 6 is taken.** `OpError::from_code(6)` is now
  `Some(NegativeExponent)` (was `None`), and the `check` error edge or trap of an
  integer `pow` can carry code 6. A language that used `trap 6` for its own
  purpose now collides with an OPS code (IR.md §7: codes 1–6 are OPS).
- New enum variants (all enums are `#[non_exhaustive]`, so a `match` with a
  wildcard arm compiles unchanged, but behaviour behind the wildcard is now
  reachable): `BinaryOp::Pow`, `Shift::Saturate`, `OpError::NegativeExponent`,
  `BuildError::{UnknownVariable, SealedBlock, NotBlockParam, VariablesActive,
  BudgetExceeded}`.
  `BinaryOp::ALL` and `OpError` codes grew accordingly.
- `Builder::compact` drops the builder's variable-construction state (variables
  declared before are unknown afterwards). `Builder::split_block` returns
  `VariablesActive` while variable construction has unsealed blocks. Builders
  that never declare a variable or seal a block are unaffected.

### Added

- **OPS v2.** `BinaryOp::Pow` on integers (exact repeated-squaring power, `0 ** 0
  = 1`, overflow per policy, `wrap` keeps the low bits; negative exponent →
  `NegativeExponent` E0006, a trap with that code under `trap`) and on floats
  (no policy; the shared routine). `Shift::Saturate` (amount ≥ width: `0`, or
  `-1` for `shr` of a negative signed value; a negative amount is still
  `ShiftOutOfRange`). `OpError::NegativeExponent` (code 6). The interpreter,
  validator, printer, and parser support all of it; `promote` stays
  unrepresentable.
- `ls_pow(x, y)`: the family's shared float `pow`
  (`_lexersketch/specs/ops-vectors/pow.md`), bit-identical to every other tier,
  in `core` (no `std` needed, so `no_std` optimizers can fold `pow`). The
  interpreter's float `pow` is this routine; `f32` widens, computes, rounds once.
- `BinaryOp::can_error(Policy, Type)`, `UnaryOp::can_error(Policy)`,
  `ConvOp::can_error(Policy)`: the exact fallibility rule the validator applies.
- **SSA construction** (Braun et al. 2013, block parameters for phis):
  `Variable`, `Builder::{declare_var, def_var, use_var, seal_block,
  seal_all_blocks, is_sealed}`. Edges into a block pass only the block's own
  arguments; the builder appends the variables'. Trivial parameters are
  forwarded when their block is sealed and removed, to a fixpoint, by
  `seal_all_blocks`. Unassigned reads give zero of the type. Iterative
  throughout, definitions in an open-addressing `(variable, block)` table.
- **Statepoint insertion**: `Builder::insert_statepoints() ->
  StatepointStats`. Computes reference liveness as the validator defines it,
  adds the missing GC-list entries at every safepoint (existing entries reused;
  `invoke`s pass relocated references as `resN` on both edges), repairs SSA over
  the finished graph with new block parameters where relocated versions meet,
  removes the trivial ones, and validates the result; on any error the function
  is restored exactly. `StatepointStats { safepoints, relocations, params }`.
  Budgeted: `insert_statepoints_within(budget)` caps relocations plus block
  parameters (`BudgetExceeded`, nothing changed); `insert_statepoints()` uses
  `DEFAULT_STATEPOINT_BUDGET` (2^26), since the output can be quadratic in the
  input.
- `Builder::remove_block_params(&[Value])`: removes any number of block
  parameters, from any blocks, with their edge arguments, in one linear pass
  (opt-lang 0.2 had to rebuild blocks to avoid the per-parameter scan of
  `remove_block_param`).
- `Builder::move_inst(Inst, Position)`: moves an instruction in O(1), keeping
  its handle and results (opt-lang 0.2 had to re-insert and forward).
- Variable construction also works when adding code to an existing body
  (`Module::edit`): the builder reads the existing edges when it starts. It is a
  lowering tool, not an SSA updater for existing definitions (`def_var` is
  refused on a block that already has its terminator).
- Example `variables` (SSA construction and statepoint insertion); tests
  `ops_v2`, `pow_vectors` (all 122 spec vectors), `ssa`, `statepoint`; benches
  for SSA construction, statepoint insertion, and `ls_pow`.

### Changed

- The validator's structural checks and safepoint checks are separate internal
  steps (the statepoint pass validates structure first, then the whole result).
  No change in what is accepted or in the order errors are reported.
- `remove_block_param` documents its linear-in-blocks cost and points to
  `remove_block_params`.

### Migrating from 2.0.0-alpha.1

| alpha.1 | alpha.2 |
|---|---|
| `if policy.can_error() { check } else { plain }` for a binary op | `if op.can_error(policy, operand_ty) { check } else { plain }` |
| `if policy.can_error() \|\| op == ConvOp::CharFromU32` | `op.can_error(policy)` |
| A `match` on `BinaryOp`/`Shift`/`OpError` with a wildcard that returns "unsupported" | Still compiles; handle `Pow`, `Saturate`, `NegativeExponent` (code 6) to support OPS v2 |
| Rebuilding blocks to drop many parameters | `b.remove_block_params(&params)?` |
| Re-insert + `replace_all_uses` to move an instruction | `b.move_inst(inst, pos)?` |
| Hand-written GC lists at every call | Optional: use references freely, then `b.insert_statepoints()?` |
| Stack slots for mutable locals, or hand-placed block parameters | Optional: `declare_var` / `def_var` / `use_var` / `seal_block` / `seal_all_blocks` |

---

## [2.0.0-alpha.1] - 2026-10-09

The 2.0 redesign, as a pre-release: the SSA IR of the native compiler path
(LexerSketch ARCHITECTURE §9), designed once for every later phase. The normative
specification is `_lexersketch/specs/IR.md`. Consumers (codegen-lang, jit-lang,
aot-lang, linker-lang) stay on 1.x until the coordinated Wave M.

### Breaking

- **Everything is reshaped around a `Module`.** A `Function` no longer stands alone:
  it is a body inside a `Module`, which holds function declarations (name,
  `Signature`, `Linkage`, `Attrs`), globals, interned signatures, and aggregates.
  `Builder::new(name, params, ret)` is gone; use `Module::declare_function` then
  `Module::build`.
- **Types.** `Type::{Int, Float, Bool, Unit}` became sized, signed/unsigned
  integers `I8..I64`, `U8..U64`, `F32`, `F64`, `Bool`, `Ptr`, and `Ref`. There is no
  `Unit`: a function returns a list of values (possibly empty).
- **Instructions.** `Inst` is now an instruction *handle*; the data enum is
  `InstData`. `BinOp`/`UnOp` became `BinaryOp`, `UnaryOp`, `CmpOp`, and `ConvOp`, and
  every arithmetic instance carries a `Policy` (OPS overflow / div_zero / shift /
  float_to_int). Comparisons are their own instruction (`InstData::Compare`).
- **Terminators.** `Terminator::Jump(Block, Vec<Value>)` and `Branch { .. }` now
  take `BlockCall`s; `Return(Option<Value>)` became `Return { values }`. New:
  `Switch`, `Unreachable`, `Trap`, `TailCall`, `Invoke`, `Check`, `Resume`.
- **Every public enum is `#[non_exhaustive]`** (ISSUES H05), so a `match` needs a
  wildcard arm; new variants are additive from 2.0.0 on.
- **Builder methods return `Result`.** Each instruction and terminator is checked as
  it is added; `iconst(i64)`/`fconst`/`bconst` became `iconst(Type, i128)`,
  `f64const`, `f32const`, `bconst`; `bin`/`un` became `binary`/`unary` (plus `add`,
  `sub`, `mul`, `compare`, `convert`, ...).
- **`set_terminator` refuses to overwrite** an existing terminator
  (`BuildError::AlreadyTerminated`, ISSUES M42); remove it explicitly with
  `remove_terminator`.
- **Validation** is `Module::validate` (or `validate_function`); errors are a
  `ValidationError` (function, block, instruction, and a `ValidationErrorKind`).
- **`serde` feature removed**; the textual form (`Display` + `parse`) is the
  serialization format.
- **Textual form** changed completely (and now round-trips; see below).

### Added

- Calls: direct, indirect (`call_indirect`), and tail calls, with call-site
  attributes (`nounwind`, `noreturn`, `nogc`, and hints); `invoke` with landing pads;
  `resume`; `check` terminators that route OPS errors to an error edge with the error
  code.
- Memory: stack slots, `load`/`store` with alignment and volatility, atomics
  (`atomic_load`, `atomic_store`, `atomic_rmw`, `cmpxchg`, `fence`, C++20 orderings),
  `memcpy`/`memmove`/`memset`, and address computation (`stack_addr`, `global_addr`,
  `func_addr`, `field_addr`, `elem_addr`, `ptr_offset`).
- Aggregates (structs, packed structs, arrays) with C layout; globals with
  initializers and relocations; calling-convention tags (`fast`, `c`, `sysv64`,
  `win64`, `aapcs64`); 32- and 64-bit pointer widths.
- Safepoints with GC lists and relocated results (calls, invokes, `safepoint`), and
  the validator's safepoint rules: no `ref` or derived pointer live across a
  safepoint; derived pointers never escape.
- The rewrite API (ISSUES M45): insertion positions, O(1) `replace_all_uses`,
  `remove_inst`, `replace_inst`, `split_block`, `add_block_param`,
  `remove_block_param`, `remove_block`, `resolve_aliases`, and `compact` with an
  explicit `Remap`.
- Validator checks: entry parameters against the signature and values defined twice
  (ISSUES M43); exact policies (M44); exception-edge, attribute, and safepoint rules;
  layout and forwarding consistency.
- `parse` / `parse_with` with `ParseLimits`: the printer's output parses back to the
  same module, handles included.
- `interp` feature: the reference interpreter (`interp::run`, `Interpreter`,
  `Limits`, `Host`) implementing `specs/OPS.md` exactly, with GC-epoch simulation,
  and the per-operation `eval_*` functions.
- Examples: `lower` (Tier-1 lowering), `rewrite` (a folding pass), `run`
  (interpreter).

### Fixed

- Division by zero and `MIN / -1` now have defined semantics on every tier through
  the instance policy (ISSUES M44; H10's root cause).
- The builder no longer falls back to a default type for an ill-typed operation
  (ISSUES M42); it returns `BuildError::Invalid`.

### Migrating from 1.0

| 1.0 | 2.0 |
|---|---|
| `Builder::new("f", &[Type::Int], Type::Int)` | `let f = m.declare_function("f", &Signature::new(&[Type::I64], &[Type::I64]), Linkage::Export)?; let mut b = m.build(f)?;` |
| `b.block_params(b.entry())[0]` | `b.param(0)?` |
| `b.iconst(1)` | `b.iconst(Type::I64, 1)?` |
| `b.bin(BinOp::Add, x, y)` | `b.add(x, y, Overflow::Wrap)?` (choose the language's policy) |
| `b.bin(BinOp::Div, x, y)` | `b.binary(BinaryOp::Div, Policy::overflow(o).with_div_zero(d), x, y)?`, or a `check` terminator when a policy is `error` |
| `b.bin(BinOp::Lt, x, y)` | `b.compare(CmpOp::Lt, x, y)?` |
| `b.bin(BinOp::And, p, q)` on `bool` | `b.binary(BinaryOp::And, Policy::NONE, p, q)?` |
| `b.un(UnOp::Neg, x)` / `UnOp::Not` | `b.unary(UnaryOp::Neg, Policy::overflow(o), x)?` / `UnaryOp::Not` with `Policy::NONE` |
| `b.jump(blk, &args)` / `b.branch(c, t, &ta, e, &ea)` | same names, now `-> Result<(), BuildError>` |
| `b.ret(Some(v))` / `b.ret(None)` | `b.ret(&[v])?` / `b.ret(&[])?` |
| `b.finish()` then `func.validate()` | drop the builder; `m.validate()?` |
| `Type::Unit` return | an empty return list |
| `Type::Int` / `Type::Float` | pick a width and signedness: `Type::I64`, `Type::F64`, ... |
| `match inst { Inst::Bin(..) => .. }` | `match func.inst(i) { Some(InstData::Binary { .. }) => .., _ => .. }` (non-exhaustive) |
| `func.insts(block)` gave `&[Value]` | `func.insts(block)` iterates `Inst` handles; `func.results(inst)` gives the values |
| `Display for Function` | `Display for Module`, or `m.display_function(f)` |
| `serde` | the textual form: `m.to_string()` and `ir_lang::parse` |

Backends that matched exhaustively on `Inst`, `Terminator`, `BinOp`, `UnOp`, or `Type`
(codegen-lang, jit-lang) must add wildcard arms and handle the new forms or reject
them with an error.

---

## [1.0.0] - 2026-06-29

The API freeze. v0.1.0 stood up the scaffold; v0.2.0 built the IR, the lowering
interface, and validation. This release ratifies that surface as the `1.0` contract —
it follows Semantic Versioning and carries no breaking changes before `2.0` — and
adds a performance and hardening pass. A v0.2.0 program compiles and behaves
identically against 1.0.0.

### Added

- `ValidationError::InconsistentDefinition`, reported when a value's recorded
  definition disagrees with the block that lists it. `Function::validate` now also
  gates the value table itself — handle ranges, definition sites, and recorded result
  types — so it is a complete check for a hand-assembled or `serde`-deserialized
  function, not only a builder-produced one.
- Runnable examples: `examples/lower_ast.rs` (lowering a small AST to IR) and
  `examples/validation.rs` (the errors validation reports).

### Changed

- The SSA dominance check is now a single linear pre-order walk of the dominator
  tree carrying one reused availability set, with no per-block allocation. The
  reachability, dominator, and dominance walks all use an explicit stack.
- `docs/API.md` is marked stable and the SemVer promise is recorded.

### Fixed

- Validating a hand-assembled or deserialized `Function` with an out-of-range entry
  block, or with no blocks at all, now returns a defined `BlockOutOfRange` error
  instead of panicking.
- Validation rejects a value whose recorded result type is not the type its
  defining instruction produces — a soundness gap for deserialized IR, found by an
  independent adversarial review.

---

## [0.2.0] - 2026-06-29

The core release: the intermediate representation, the lowering interface, and
well-formedness validation. The crate is self-contained — it defines its own
machine-level types and is driven entirely through the builder — so no first-party
dependency is wired (see [`dev/ROADMAP.md`](dev/ROADMAP.md) for the recorded
reasoning).

### Added

- IR data model: `Function` (an SSA control-flow graph), `Block` and `Value` handles,
  `Type` (`Int`/`Float`/`Bool`/`Unit`), `Inst`, `Terminator`, `BinOp`, and `UnOp`.
- `Builder`, the AST-to-IR lowering interface: constants, arithmetic, comparison and
  logical operations, unary operations, block parameters, jumps, conditional
  branches, and returns. Result types are inferred from the operation.
- `Function::validate` with `ValidationError`: structural checks (one terminator per
  block, valid branch targets, argument count and type matching, operand typing) and
  the SSA dominance property, with dominators computed by the Cooper–Harvey–Kennedy
  algorithm. The reachability and dominator walks use an explicit stack, so a deeply
  nested function cannot overflow the call stack.
- Textual IR: `Display` for `Function`.
- `serde` derives for every IR type behind the `serde` feature.
- Criterion benchmarks for building and validating straight-line and
  control-flow-heavy functions.

---

## [0.1.0] - 2026-06-18

Initial scaffold and repository bootstrap. No domain logic yet &mdash; this release establishes the structure, tooling, and quality gates the implementation will be built on.

### Added

- `Cargo.toml` with crate metadata, Rust 2024 edition, MSRV 1.85.
- Dual `Apache-2.0 OR MIT` license files.
- `README.md`, `CHANGELOG.md`, and a documentation skeleton.
- `REPS.md` compliance baseline.
- `.github/workflows/ci.yml` CI matrix; `deny.toml`, `clippy.toml`, `rustfmt.toml`.
- `dev/DIRECTIVES.md` and `dev/ROADMAP.md` (committed engineering standards + plan).

[Unreleased]: https://github.com/jamesgober/ir-lang/compare/v2.0.0-alpha.2...HEAD
[2.0.0-alpha.2]: https://github.com/jamesgober/ir-lang/compare/v2.0.0-alpha.1...v2.0.0-alpha.2
[2.0.0-alpha.1]: https://github.com/jamesgober/ir-lang/compare/v1.0.0...v2.0.0-alpha.1
[1.0.0]: https://github.com/jamesgober/ir-lang/compare/v0.2.0...v1.0.0
[0.2.0]: https://github.com/jamesgober/ir-lang/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/ir-lang/releases/tag/v0.1.0
