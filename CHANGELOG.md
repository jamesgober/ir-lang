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
  block, valid branch targets, argument count and type matching, operand typing), the
  SSA dominance property, and value-table integrity (handle ranges, definition sites,
  recorded result types) — a complete check for a hand-assembled or deserialized
  function, not only a builder-produced one. Dominators are computed by the
  Cooper–Harvey–Kennedy algorithm, and the dominance check itself is a single linear
  pre-order walk of the dominator tree carrying one reused availability set — no
  per-block allocation. The reachability, dominator, and dominance walks all use an
  explicit stack, so a deeply nested function cannot overflow the call stack.
- Textual IR: `Display` for `Function`.
- `serde` derives for every IR type behind the `serde` feature.
- Runnable examples (`examples/lower_ast.rs`, `examples/validation.rs`) and criterion
  benchmarks for building and validating straight-line and control-flow-heavy
  functions.

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

[Unreleased]: https://github.com/jamesgober/ir-lang/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jamesgober/ir-lang/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jamesgober/ir-lang/releases/tag/v0.1.0
