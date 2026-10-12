//! # ir_lang
//!
//! The SSA intermediate representation of the `-lang` family's native compiler
//! path: what a front end lowers into after monomorphization, closure conversion,
//! pattern compilation, and effect lowering, what the optimizer rewrites, and what
//! instruction selection reads.
//!
//! A [`Module`] holds functions (with [`Signature`]s and calling-convention tags),
//! globals, and aggregate types. A function body is a control-flow graph of
//! blocks; values cross control-flow joins as **block parameters** (no phi nodes);
//! every value has a [`Type`] and exactly one definition. Instructions cover every
//! operation of the shared operation semantics (`specs/OPS.md`), each instance
//! carrying its policy; conversions; comparisons; `select`; direct, indirect, and
//! tail calls; `invoke` with an exception edge; memory (stack slots, loads and
//! stores with alignment and volatility, atomics, `memcpy`/`memset`, address
//! computation); and safepoints that relocate GC references, so native frames
//! can be scanned by a moving collector.
//!
//! A front end lowers mutable locals through the builder's SSA construction
//! ([`Builder::declare_var`], [`Builder::def_var`], [`Builder::use_var`],
//! [`Builder::seal_block`]), and may use GC references freely across calls and
//! let [`Builder::insert_statepoints`] fill the GC lists from liveness. The
//! operations are OPS v2 (`pow`, `shift = saturate`, `NegativeExponent`), with
//! the shared float power routine [`ls_pow`] available without `std`.
//!
//! ## Quick start
//!
//! Build, validate, print, and parse back `fn add1(x: i64) -> i64 { x + 1 }`:
//!
//! ```
//! use ir_lang::{Linkage, Module, Overflow, Signature, Type};
//!
//! let mut m = Module::new("demo");
//! let f = m.declare_function("add1", &Signature::new(&[Type::I64], &[Type::I64]), Linkage::Export)?;
//! let mut b = m.build(f)?;
//! let x = b.param(0)?;
//! let one = b.iconst(Type::I64, 1)?;
//! let y = b.add(x, one, Overflow::Wrap)?;
//! b.ret(&[y])?;
//!
//! m.validate()?;
//! let text = m.to_string();
//! assert!(text.contains("v2: i64 = add<overflow=wrap> v0, v1"));
//! let again = ir_lang::parse(&text)?;
//! assert_eq!(again.to_string(), text);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## What is checked
//!
//! [`Module::validate`] is total (it accepts or reports, never panics) and checks:
//! entry parameters against the signature, single definition, dominance (the
//! Cooper–Harvey–Kennedy dominator tree and one linear walk), types and exact
//! policies of every instruction, exception-edge rules (landing pads, `nounwind`,
//! `noreturn`, `nogc`), and the safepoint rules (no GC reference or pointer derived
//! from one is live across a safepoint). The [`Builder`] runs the same
//! per-instruction checks as it goes, so lowering bugs surface at the call that
//! made them. The normative rules are in `specs/IR.md`.
//!
//! ## Features
//!
//! - `std` (default): the standard library; without it the crate is `#![no_std]`
//!   and needs only `alloc`.
//! - `interp`: the reference interpreter ([`interp`]), which implements the
//!   operation semantics exactly and is the oracle for optimizers and backends.
//!   Requires `std` (for the float functions of OPS §4).
//!
//! ## Stability
//!
//! This is `2.0.0-alpha.2`, a pre-release of the 2.0 redesign. The surface may
//! change before `2.0.0`; see `dev/ROADMAP.md`.

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![deny(unused_must_use, unused_results)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::print_stderr
)]

extern crate alloc;

mod builder;
mod compact;
mod entity;
mod error;
mod function;
mod inst;
mod module;
mod ops;
mod parse;
mod pow;
mod print;
mod ssa;
mod statepoint;
mod types;
mod typing;
mod validate;

#[cfg(feature = "interp")]
#[cfg_attr(docsrs, doc(cfg(feature = "interp")))]
pub mod interp;

pub use builder::{Builder, Position, Remap};
pub use entity::{
    AggId, Block, FuncId, GlobalId, Inst, SigId, StackSlot, Value, ValueRange, Variable,
};
pub use error::{BuildError, ModuleError, ValidationError, ValidationErrorKind};
pub use function::{Function, Insts, MAX_ENTITIES, SlotData, ValueDef};
pub use inst::{
    Attrs, BlockArg, BlockCall, Call, CallTarget, InstData, MemFlags, MemOrder, RmwOp, Successors,
    SwitchCase, Terminator,
};
pub use module::{FuncDecl, Global, Init, Linkage, Module, Reloc, SymbolRef};
pub use ops::{
    BinaryOp, CmpOp, ConvOp, DivZero, FloatToInt, OpError, Overflow, Policy, PolicyFields, Shift,
    UnaryOp,
};
pub use parse::{ParseError, ParseErrorKind, ParseLimits, parse, parse_with};
pub use pow::ls_pow;
pub use statepoint::{DEFAULT_STATEPOINT_BUDGET, StatepointStats};
pub use types::{
    Aggregate, CallConv, Layout, MAX_AGGREGATE_SIZE, MemType, PointerWidth, Signature, Type,
};

/// Runs the README's examples as doctests, so its claims stay true.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// Runs the API reference's examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../docs/API.md")]
struct ApiDoctests;
