# ir-lang - Roadmap

> Path from scaffold to a stable 1.0, then the 2.0 redesign for LexerSketch's native path
> (`_lexersketch/specs/IR.md`). Hard parts are front-loaded; each phase has hard exit criteria.
> Master plan: ../../_strategy/LANG_COLLECTION.md
>
> **Anti-deferral rule:** no listed hard task moves to a later phase unless this file records the move and the reason.

## v0.1.0 - Scaffold (DONE)
Compiles, CI green, structure correct, no domain logic.
- [x] Manifest, README, CHANGELOG, REPS, dual license, CI, deny, clippy, rustfmt.

## v0.2.0 - Core (THE HARD PART, NOT DEFERRED)
An intermediate representation and AST-to-IR lowering, where optimization passes run.
Exit criteria:
- [x] Every public item has rustdoc + a runnable example.
- [x] Core invariants property-tested (full DIRECTIVES + API authored at this stage).

### Dependency wiring — deferred, with reason (anti-deferral rule)
`ast-lang` and `type-lang` are **not** wired at v0.2.0. The reason:

- `ast-lang` is a generic syntax-tree *substrate* — it owns no grammar; each
  language defines its own node type. There is no concrete AST to lower *from*, so
  a hard dependency would be unused. Lowering is instead expressed as the `Builder`:
  a front-end walks its own tree (with `ast-lang`'s `walk` if it uses that crate)
  and drives the builder, exactly as a consumer drives Cranelift's `FunctionBuilder`
  or LLVM's `IRBuilder`. The builder *is* the AST-to-IR lowering interface.
- `type-lang` models *source-level* types (unification, inference). An IR carries
  *machine-level* value types (`int`, `float`, `bool`, `unit`) — a different layer,
  the same way LLVM IR's type system is independent of any source language's.
  ir-lang defines its own small `Type`; mapping source types onto IR types is the
  consumer's lowering step. Wiring `type-lang` would duplicate, not reuse.

Both stay unwired until a concrete in-crate use appears; the integration is the
consumer's job. Same precedent as `type-lang` (which left `ast`/`symbol`/`diag`
unwired for the identical reason).

### Scope held for a later minor (additive, non-breaking)
Function calls and a multi-function module/program container are **not** in v0.2.0.
The unit is a single self-contained `Function` with SSA values, machine types, and
full intra-function control flow. Calls (`Call` instruction + opaque callee handle)
and a `Module` are additive — they arrive in a later 0.x minor without breaking the
v0.2.0 surface.

## v1.0.0 - API freeze (DONE)
Public surface stable and frozen until 2.0.
- [x] docs/API.md marked stable; SemVer promise recorded.
- [x] Full test + benchmark suite green on all three platforms.

The 0.2.0 surface is frozen as the 1.0 contract; no breaking change. The freeze added
a faster (linear) dominance validator, a complete check for hand-assembled / `serde`
IR (one additive `ValidationError::InconsistentDefinition` variant), runnable
examples, and the stability docs. The dominator and SSA-dominance algorithms were
fuzzed against a brute-force reference by an independent adversarial review.

Post-1.0 additive candidates (each a minor release): function calls (a `Call`
instruction + opaque callee handle) and a multi-function module container.

---

## v2.0.0-alpha.1 - the 2.0 redesign (DONE, 2026-10-08)
The SSA IR for LexerSketch's native path (ARCHITECTURE §9), designed once for every later
phase (Phase 4.1; ISSUES H05, M42, M43, M44, M45). Normative spec:
`_lexersketch/specs/IR.md`. Pre-release: the surface may still change before 2.0.0.

Delivered:
- `Module`: functions (interned `Signature`s with `CallConv` tags, `Linkage`, `Attrs`), globals
  (`Global`, `Init`, `Reloc`), aggregates (`Aggregate`, `Layout`, C and packed layout), pointer
  width.
- Types: sized signed/unsigned integers (decision and justification in IR.md §2.1), `f32`/`f64`,
  `bool`, `ptr`, `ref`; aggregates as memory types; vectors reserved.
- Every OPS operation with its `Policy` on the instance, exact required fields, `promote`
  unrepresentable; conversions; comparisons; `select`; `fma`.
- Calls (direct, indirect, tail) with call-site attributes; `invoke` with landing pads; `check`
  with an error edge carrying the OPS code; `resume`; `trap`; `switch`.
- Memory: stack slots, loads/stores with alignment and volatility, atomics (C++20 orderings as
  HIR v2), `memcpy`/`memmove`/`memset`, `field_addr`/`elem_addr`/`ptr_offset`.
- Safepoints: statepoint-style GC lists with relocated results on calls, invokes, and `safepoint`;
  derived-pointer tracking.
- `Builder`: lowering and the rewrite API (positions, `replace_all_uses` in O(1),
  `remove_inst`, `replace_inst`, `split_block`, block-parameter edits, `remove_block`,
  `resolve_aliases`, `compact` with `Remap`); `set_terminator` refuses to overwrite (M42).
- Validator: entry vs signature and single definition (M43), typing and exact policies (M44),
  CHK dominance with the linear walk, exception-edge and attribute rules, safepoint rules.
- Deterministic printer and budgeted parser with exact round trip.
- Reference interpreter (`interp` feature) with exact OPS semantics, GC-epoch simulation, and
  budgets; per-operation `eval_*` functions for constant folding.
- Proof: unit, integration, and property tests (builder output validates; dominance and
  safepoint liveness against brute force on random CFGs; print/parse round trip; validator total on corrupted state and mutated text; rewrite sequences keep IR valid
  and meaning; OPS edge-value conformance; differential random programs with loops, calls,
  invokes, tail calls, memory, globals, exceptions, traps); criterion benches at 100k and 1M
  instructions.

Dependency wiring (decided here, recorded per the anti-deferral rule):
- **No first-party dependency.** ir-lang 2.0 is the substrate opt-lang and backend-lang build on;
  it needs nothing below it. hir-lang is *not* wired: the HIR → IR lowering is its own crate
  (Phase 4.2), so ir-lang stays independent of the front end. span/diag are not wired:
  validation errors name IR entities, and a lowering maps them back to HIR origins.
- **`serde` feature removed**: the textual form is the serialization format (exact round trip,
  budgeted parser). A binary encoding or `serde` can be added in a minor release.
- **`interp` feature** (needs `std` for IEEE float functions that `core` lacks on 1.85).

Moved to 2.0.0 (not dropped), with the reason:
- **Automatic statepoint insertion** (compute reference liveness, fill GC lists, repair SSA with
  new block parameters). The validator already checks the rule; the utility needs an SSA updater,
  which opt-lang also needs (loop rotation, jump threading). Designing both together, against
  opt-lang's first passes, avoids building the updater twice.

## v2.0.0-beta.1 - consumer-driven fixes
Exit criteria:
- [ ] HIR → IR lowering (Phase 4.2) lowers the Mox/Iron/Zero sample programs and they validate
      and run in the interpreter with the same results as the bytecode tier.
- [ ] opt-lang's first passes (SCCP via `interp::eval_*`, DCE, GVN) run on the rewrite API with
      the validator between passes; every API gap they hit is fixed here.
- [ ] SSA updater + automatic statepoint insertion (moved here from alpha.1, see above).
- [ ] Use lists or a cheap use-count query if opt-lang's profiles show full scans dominate.
- [ ] Benchmarks: parse and print throughput (now ~1.5M and ~4M instructions/s) measured
      against the HIR → IR lowering's output sizes; improve the parser's token handling if it
      is on a hot path.

## v2.0.0 - API freeze (after a real consumer, D18)
- [ ] isel into backend-lang MIR exercises calls, invokes, safepoints (stack maps), and memory
      end to end on x86-64.
- [ ] docs/API.md marked stable; SemVer promise recorded.

## Wave M - consumer updates (coordinated, not done here)
ir-lang 1.x stays published for its current users until Wave M:
- **codegen-lang 2.0**: stays the ir-lang → LSB backend for 1.x users per D19; moves to ir-lang
  2.0 or is retired when bcgen-lang covers its users.
- **jit-lang 2.0**: replaces Cranelift with backend-lang; reads ir-lang 2.0.
- **aot-lang 2.0**: backend-lang → object-lang → linker; reads ir-lang 2.0.
- **linker-lang 2.0**: dev-dependency on ir-lang moves to 2.0.
Migration guide for all of them: `CHANGELOG.md` → 2.0.0-alpha.1 → "Migrating from 1.0".
