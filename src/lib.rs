//! # ir_lang
//!
//! An intermediate representation and the lowering interface a compiler builds it
//! through.
//!
//! ir-lang gives a front-end a place to put a program once it has been parsed and
//! type-checked, in the flat, explicit form that optimization passes rewrite and
//! backends read. A [`Function`] is a control-flow graph of basic blocks; each block
//! is a straight-line run of value-producing [instructions](Inst) ended by one
//! [`Terminator`]; every value is named by a small [`Value`] handle and defined
//! exactly once, in SSA form. Values cross control-flow joins as block parameters
//! rather than through phi nodes, which keeps the representation flat and the
//! validation simple.
//!
//! ## Lowering
//!
//! There is no AST type here to lower *from* — a language brings its own syntax
//! tree. Lowering is instead expressed through the [`Builder`]: a front-end walks
//! its tree and, for each construct, calls a builder method that emits the matching
//! IR and hands back the [`Value`] it defines. This is the same shape as Cranelift's
//! `FunctionBuilder` or LLVM's `IRBuilder`. When the function is built, check it with
//! [`Function::validate`].
//!
//! ## Example
//!
//! Lower and validate `fn abs(x: int) -> int { if x < 0 { -x } else { x } }`:
//!
//! ```
//! use ir_lang::{Builder, BinOp, Type, UnOp};
//!
//! let mut b = Builder::new("abs", &[Type::Int], Type::Int);
//! let x = b.block_params(b.entry())[0];
//!
//! // The merge point takes the result as a parameter.
//! let join = b.create_block(&[Type::Int]);
//! let neg_blk = b.create_block(&[]);
//! let pos_blk = b.create_block(&[]);
//!
//! let zero = b.iconst(0);
//! let is_neg = b.bin(BinOp::Lt, x, zero);
//! b.branch(is_neg, neg_blk, &[], pos_blk, &[]);
//!
//! b.switch_to(neg_blk);
//! let negated = b.un(UnOp::Neg, x);
//! b.jump(join, &[negated]);
//!
//! b.switch_to(pos_blk);
//! b.jump(join, &[x]);
//!
//! b.switch_to(join);
//! let result = b.block_params(join)[0];
//! b.ret(Some(result));
//!
//! let func = b.finish();
//! func.validate().expect("the lowered function is well-formed");
//! assert_eq!(func.name(), "abs");
//! ```
//!
//! ## Features
//!
//! - `std` (default) — the standard library; without it the crate is `#![no_std]`
//!   and needs only `alloc`.
//! - `serde` — derives `serde::Serialize` / `Deserialize` for the IR types so a
//!   function can be cached, inspected, or moved between tools.
//!
//! ## Stability
//!
//! The public surface is frozen and stable as of `1.0.0`: it follows Semantic
//! Versioning, with no breaking changes before `2.0`. The full surface and the
//! SemVer promise are catalogued in
//! [`docs/API.md`](https://github.com/jamesgober/ir-lang/blob/main/docs/API.md#semver-promise).

#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![forbid(unsafe_code)]
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
mod entity;
mod function;
mod inst;
mod ty;
mod validate;

pub use builder::Builder;
pub use entity::{Block, Value};
pub use function::Function;
pub use inst::{BinOp, Inst, Terminator, UnOp};
pub use ty::Type;
pub use validate::ValidationError;
