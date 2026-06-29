# ir-lang &mdash; API Reference

> **Status: pre-1.0 scaffold (v0.1.0).** This release stands up the crate, the
> tooling, and the quality gates; it exports **no public API yet**. The surface
> sketched below is the intended shape, not an implemented contract — each item is
> marked *planned* and lands across the 0.x series before the freeze at `1.0`.
> See [`../dev/ROADMAP.md`](../dev/ROADMAP.md) for the phase plan and
> [`../dev/DIRECTIVES.md`](../dev/DIRECTIVES.md) for the invariants it will hold to.

## Table of Contents

- [Overview](#overview)
- [Installation](#installation)
- [Feature flags](#feature-flags)
- [Planned surface](#planned-surface)
  - [`Function` (planned)](#function-planned)
  - [`Block` (planned)](#block-planned)
  - [`Inst` and `Value` (planned)](#inst-and-value-planned)
  - [`lower` (planned)](#lower-planned)
  - [`LowerError` (planned)](#lowererror-planned)
- [Stability & SemVer](#stability--semver)

---

## Overview

ir-lang defines the intermediate representation a compiler optimizes and lowers
code through, and the AST-to-IR lowering that produces it. Given a type-checked
AST — the program a front-end has already parsed and resolved — it lowers each
function into a flat, explicit form: basic blocks of straight-line instructions,
each block ending in a single terminator, with values named by stable handles
rather than by position in a tree. That form is what optimization passes rewrite
and what backends read.

It sits in the SEMA tier of the `-lang` language-construction family, above
[`ast-lang`](https://docs.rs/ast-lang) (the tree it lowers from) and
[`type-lang`](https://docs.rs/type-lang) (the types it lowers against), and below
the pass manager and code generators that consume it. It owns the IR and the
lowering only — no parsing, no name resolution, no code emission. Those
dependencies are wired at v0.2.0, where the lowering core first uses them, so the
scaffold carries no unused dependency.

---

## Installation

```toml
[dependencies]
ir-lang = "0.1"
```

MSRV: Rust 1.85 (Rust 2024 edition).

---

## Feature flags

| Feature | Default | Description |
| ------- | ------- | ----------- |
| `std`   | yes     | Standard-library support. With it disabled the crate is `#![no_std]`; the IR and lowering core is being designed to hold on `alloc` alone. |
| `serde` | no      | Derives `serde::Serialize` / `Deserialize` for the IR types, so a lowered unit can be cached, inspected, or moved between tools. |

Features are additive: enabling one never removes or changes behaviour provided by
another, per the project's SemVer policy.

---

## Planned surface

> Nothing in this section is exported in v0.1.0. It records the intended shape so
> the public contract is legible before it is implemented, and so reviewers can
> hold the implementation to it. Names and signatures may still change before they
> first ship in a tagged release; once a symbol appears in a release it follows the
> [SemVer policy](#stability--semver).

### `Function` (planned)

The unit of lowered code: a control-flow graph of basic blocks over a single
flat store of values and instructions. A function is well-formed by construction —
every block ends in exactly one terminator, and every branch target names a block
that exists. Blocks and values are addressed by small, stable handles, so passes
can rewrite the body without chasing pointers or invalidating references they
already hold.

### `Block` (planned)

A basic block: a straight-line sequence of instructions with a single entry and a
single terminating instruction (a return, a branch, or a jump). Blocks carry no
ownership of their instructions — they index into the function's instruction
store — so splitting, threading, and merging blocks during optimization stays
cheap.

### `Inst` and `Value` (planned)

`Inst` is a single IR operation; `Value` is the result it defines, named by a
stable handle. Every value is defined exactly once and every use is dominated by
its definition, which is what lets later passes reason locally about data flow.
Instructions reference their operands by `Value` handle, never by re-nesting
sub-expressions, so the representation stays flat and walkable in source order.

### `lower` (planned)

The AST-to-IR entry point: it takes a type-checked AST function and produces a
well-formed `Function`, threading control flow into blocks and naming every
intermediate result. Lowering is a single pass over the tree, preserves observable
side effects in order, and returns a [`LowerError`](#lowererror-planned) rather
than panicking when it is handed an input it cannot lower.

### `LowerError` (planned)

The single error type for the lowering surface, with one variant per defined
failure. Every variant carries enough context to point a diagnostic at the
offending AST node; none of them is a panic. Validation of an already-built IR
unit reports through the same defined-error discipline, so a malformed unit is
always rejected, never silently accepted.

---

## Stability & SemVer

The crate follows [Semantic Versioning](https://semver.org). During the 0.x series
the public surface is still being designed, so minor releases may make breaking
changes — each is documented in [`../CHANGELOG.md`](../CHANGELOG.md) with a
migration note. At `1.0.0` the surface freezes: no breaking change before `2.0`,
additions arrive in minor releases, and the MSRV only rises in a minor release.
This file is updated in lockstep with every release so it always matches the code.

<sub>Copyright &copy; 2026 <strong>James Gober</strong>.</sub>
