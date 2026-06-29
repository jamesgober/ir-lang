# ir-lang &mdash; Engineering Directives

> Engineering standards and the definition of done for this project. Read alongside `REPS.md` (root, authoritative) and `dev/ROADMAP.md` (current phase). If anything here conflicts with `REPS.md`, `REPS.md` wins.

---

## 0. Philosophy

This library is built and maintained to a production standard and treated as a flagship piece of work. Plan the full path, then build one verified step at a time. "Good enough" is treated as a defect. ir-lang is the substrate every later compiler stage reads and rewrites: each optimization pass, each backend, and each interpreter trusts that the IR it is handed is well-formed. A malformed instruction or a lowering that drops a side effect is not a cosmetic bug — it is a miscompile that surfaces far from its cause. The public surface stays small on purpose: represent the IR, lower the AST into it, and let it be validated and walked — nothing more.

---

## 1. What this is

ir-lang defines the intermediate representation a compiler optimizes and lowers code through, and the AST-to-IR lowering that produces it. It takes a type-checked AST — the program a front-end has already parsed and resolved — and lowers it into a flat, explicit form: functions made of basic blocks, each block a straight-line sequence of instructions ending in a single terminator, with values named by stable handles rather than by tree position. That form is what optimization passes rewrite and what backends read. It sits in the SEMA tier of the `-lang` language-construction family, above [`ast-lang`](https://docs.rs/ast-lang) (the tree it lowers from) and [`type-lang`](https://docs.rs/type-lang) (the types it lowers against), and below the pass manager and the code generators that consume it. It owns the IR and the lowering only — no parsing, no name resolution, no register allocation, no code emission. Those live in their own crates.

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

- Every basic block ends in exactly one terminator instruction, and a terminator appears only as the last instruction of its block.
- Every branch or jump targets a block that exists in the same function; there are no dangling block references.
- Every value is defined exactly once and every use of a value is dominated by its definition — no use-before-def, no redefinition.
- A value handle, block handle, and function handle is unique and stable for the life of the IR unit it belongs to; it never silently rebinds to a different entity.
- A well-typed AST lowers to IR that passes validation, and lowering never loses an observable side effect or reorders one past another in a way that changes program meaning.
- Validation is total: any IR unit either passes with a well-formedness guarantee or is rejected with a defined error that points at the offending instruction — it never reports success on malformed IR.
- Lowering and validation are deterministic: the same AST always produces the same IR, independent of incidental ordering beyond what the rules specify.
