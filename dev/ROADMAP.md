# ir-lang - Roadmap

> Path from scaffold to a stable 1.0. Hard parts are front-loaded; each phase has hard exit criteria.
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

## v1.0.0 - API freeze
Public surface stable and frozen until 2.0.
- [ ] docs/API.md marked stable; SemVer promise recorded.
- [ ] Full test + benchmark suite green on all three platforms.
