# ir-lang &mdash; Engineering Directives

> Engineering standards and the definition of done for this project. Read alongside `REPS.md` (root, authoritative) and `dev/ROADMAP.md` (current phase). If anything here conflicts with `REPS.md`, `REPS.md` wins.

---

## 0. Philosophy

This library is built and maintained to a production standard and treated as a flagship piece of work. Plan the full path, then build one verified step at a time. "Good enough" is treated as a defect. ir-lang is the substrate every later compiler stage reads and rewrites: each optimization pass, each backend, and each interpreter trusts that the IR it is handed is well-formed. A malformed instruction or a lowering that drops a side effect is not a cosmetic bug — it is a miscompile that surfaces far from its cause. The public surface stays small on purpose: represent the IR, lower the AST into it, and let it be validated and walked — nothing more.

---

## 1. What this is

ir-lang is the SSA intermediate representation of the family's native compiler path (2.0;
normative spec: `_lexersketch/specs/IR.md`). A front end lowers into it after
monomorphization, closure conversion, pattern compilation, and effect lowering; opt-lang
rewrites it; instruction selection reads it into backend-lang MIR. A `Module` holds functions
(signatures with calling-convention tags, linkage, attributes), globals with relocations, and
aggregate types; a function body is a control-flow graph of blocks with block parameters (no
phi nodes). Instructions cover every operation of `specs/OPS.md` with its policy on the
instance, conversions, comparisons, `select`, calls (direct, indirect, tail), `invoke` with an
exception edge, `check` with an error edge, memory (stack slots, loads/stores with alignment and
volatility, atomics, `memcpy`/`memset`, address computation), and safepoints that relocate GC
references. It owns the IR, its builder and rewrite API, its validator, its textual form, and a
reference interpreter (feature `interp`) that is the semantic oracle. It does not own parsing of
source, name resolution, type inference, optimization passes, instruction selection, register
allocation, or encoding. Bytecode (LSB) does not pass through it (D19).

---

## 2. Engineering law (non-negotiable)

- **Performance** — peak is the baseline; the IR is laid out for cache-friendly linear walks (instructions in flat arenas, values addressed by index, not chased through pointers); lowering is a single pass over the AST with no per-node heap churn on the hot path; no "faster" claim without `criterion` numbers.
- **Correctness** — the invariants in section 4 are covered by property tests; a lowered function is well-formed by construction, and a well-typed AST always lowers to IR that validates; an optimization pass takes well-formed IR to well-formed IR.
- **Security** — every AST handed in is treated as untrusted; malformed or impossible input is a defined error, never UB, never a panic, never unbounded recursion; lowering depth and IR size are bounded by the input, not by an attacker.
- **Architecture** — SOLID, KISS, YAGNI; one responsibility; the dependencies (`ast-lang`, `type-lang`) sit behind narrow seams and are wired only where first used.
- **Cross-platform** — Linux/macOS/Windows first-class, verified by CI; nothing here is platform-specific, and it stays that way.
- **Error handling** — every fallible path (a lowering that cannot proceed, an IR that fails validation) returns `Result` per the documented contract; no panics in shipping code.
- **Production-ready** — `#![forbid(unsafe_code)]` and `#![deny(missing_docs)]` from the first commit; no stray `println!`/`dbg!`; every public item has rustdoc with a runnable example.

---

## 3. Definition of done

1. Compiles clean on Linux/macOS/Windows, stable and MSRV 1.85.
2. `fmt`, `clippy -D warnings`, `test --all-features`, `cargo doc -D warnings` clean.
3. `cargo audit` + `cargo deny check` pass.
4. No `unwrap`/`expect`/`todo!`/`dbg!` in shipping code.
5. A Tier-1 API exists and headlines the docs.
6. Property tests cover every section-4 invariant.
7. Hot-path changes carry benchmarks; no regression over 5%.
8. Docs and `CHANGELOG.md` updated; the matching `docs/release/vX.Y.Z.md` written before the tag.

---

## 4. Project-specific invariants

Each is property-tested (file in brackets).

- **Builder output validates**: a well-typed program lowered through the builder passes
  validation. [`tests/proptest.rs` `prop_builder_output_validates`, `tests/differential.rs`]
- **Validation is total**: any module, including corrupted internal state and mutated text,
  is accepted or rejected with a defined error, never a panic. [`src/validate/tests.rs`
  corruption sweep, `tests/proptest.rs` `prop_validator_never_panics_on_mutated_text`,
  `tests/rewrite.rs` `prop_hostile_rewrites_never_panic`]
- **Single definition and dominance**: every value is defined once, and every use in a reachable
  block is dominated by its definition; the entry block's parameters equal the signature's.
  [`tests/brute.rs` `prop_dominance_matches_brute_force` (against path-removal dominance on
  random CFGs), `tests/validate.rs`, `src/validate/tests.rs`]
- **Exact policies**: every operation instance carries exactly the policy fields OPS requires;
  `error` policies only inside `check`; `promote` is unrepresentable. [`tests/validate.rs`,
  `tests/parse.rs`]
- **OPS semantics**: the interpreter computes exactly OPS (v2) on every type, policy, and edge
  value; float `pow` is `ls_pow` bit for bit. [`tests/interp.rs` reference comparisons,
  `tests/ops_v2.rs`, `tests/pow_vectors.rs` (all spec vectors), `tests/differential.rs`]
- **SSA construction preserves meaning and is trivially minimal**: a program lowered through
  `def_var`/`use_var` (blocks sealed eagerly or lazily) computes what the independent evaluator
  computes, validates, is deterministic, and keeps no trivial block parameter.
  [`tests/ssa.rs` `prop_ssa_eager_matches_evaluator`, `prop_ssa_lazy_matches_evaluator`,
  `prop_ssa_output_validates_and_is_deterministic`]
- **Statepoint insertion establishes the safepoint rule without changing meaning**: on random
  GC-reference programs the result validates, a second run changes nothing, and the
  interpreter's moving-collector simulation agrees with an independent evaluator; on refused
  input the function is unchanged. [`tests/statepoint.rs`
  `prop_statepoints_make_gc_programs_valid_and_correct`, refusal tests]
- **Safepoint rule**: no GC reference or pointer derived from one is live across a safepoint, and
  derived pointers never escape. [`tests/brute.rs` `prop_safepoint_liveness_matches_brute_force`
  (against path search on random CFGs with block-parameter references), `tests/validate.rs`,
  interpreter epoch checks in `tests/interp.rs`]
- **Round trip**: `parse(print(m))` prints identically and keeps every value, block, slot,
  signature, aggregate, function, and global handle. [`tests/proptest.rs`
  `prop_print_parse_round_trip`, `tests/parse.rs` `SINK`]
- **Stable handles under rewriting**: no mutation renumbers except `compact`, which returns the
  mapping; rewrites that preserve meaning keep the IR valid and its behaviour identical.
  [`tests/rewrite.rs` `prop_rewrites_keep_ir_valid_and_meaning`, `prop_compact_round_trips`]
- **No silent overwrite**: a terminator is never replaced implicitly. [`src/builder.rs` tests]
- **Determinism**: lowering, printing, validation, and interpretation are deterministic.
  [`tests/proptest.rs` `prop_lowering_is_deterministic`]
- **Bounded on hostile input**: the parser caps input size and entity ids; the interpreter caps
  fuel, depth, and memory; every walk is iterative. [`tests/parse.rs` budgets,
  `tests/interp.rs` budgets]
