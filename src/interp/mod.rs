//! The reference interpreter: executes validated IR with the operation semantics
//! of `specs/OPS.md` implemented exactly, as the oracle optimizers and backends
//! test against.
//!
//! It is written for clarity and exactness, not speed. Execution is iterative (an
//! explicit frame stack), bounded by [`Limits`] (fuel, call depth, memory), and
//! never panics: undefined behaviour in the program (an out-of-bounds or misaligned
//! access, overlapping `memcpy`, reaching `unreachable`, a stale GC reference) is
//! reported as an [`InterpError`], not executed.
//!
//! # Memory model
//!
//! One flat little-endian address space of [`Limits::memory`] bytes. Addresses
//! below 4096 are never valid (null and near-null faults). Globals are laid out
//! from 4096 in declaration order; stack slots are allocated on frame entry
//! (zero-filled, so runs are deterministic) and freed on return, so a dangling
//! slot address faults; the host heap ([`Memory::alloc`]) grows down from the top.
//! A function's address is an opaque code address that faults if accessed.
//!
//! # GC references
//!
//! A `ref` is a handle to an object allocated with [`Memory::alloc_object`]. Every
//! safepoint (a call that is not `nogc`, an `invoke`, a `safepoint`) starts a new
//! *epoch*; using a `ref` minted in an earlier epoch (one that was not relocated
//! through the safepoint's GC list) is [`InterpError::StaleRef`]. This simulates a
//! moving collector, so a missing relocation is caught at run time. Objects do not
//! actually move, so a stale *derived* pointer is caught only by the validator.
//!
//! # Examples
//!
//! ```
//! use ir_lang::interp::{self, Outcome, Val};
//! use ir_lang::{Linkage, Module, Overflow, Signature, Type};
//!
//! let mut m = Module::new("demo");
//! let f = m.declare_function("twice", &Signature::new(&[Type::I32], &[Type::I32]), Linkage::Export)?;
//! let mut b = m.build(f)?;
//! let x = b.param(0)?;
//! let y = b.add(x, x, Overflow::Wrap)?;
//! b.ret(&[y])?;
//!
//! assert_eq!(interp::run(&m, f, &[Val::i32(21)])?, Outcome::Return(vec![Val::i32(42)]));
//! // Wrapping is exact two's complement.
//! assert_eq!(interp::run(&m, f, &[Val::i32(i32::MAX)])?, Outcome::Return(vec![Val::i32(-2)]));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::error::ValidationError;
use crate::ops::{
    BinaryOp, CmpOp, ConvOp, DivZero, FloatToInt, OpError, Overflow, Policy, Shift, UnaryOp,
};
use crate::types::{PointerWidth, Type};

// ------------------------------------------------------------------- values

/// A runtime value: a type and its bit pattern, masked to the type's width.
///
/// # Examples
///
/// ```
/// use ir_lang::interp::Val;
/// use ir_lang::Type;
///
/// let v = Val::i8(-1);
/// assert_eq!(v.bits(), 0xff);
/// assert_eq!(v.as_i128(), Some(-1));
/// assert_eq!(Val::new(Type::U8, 0x1ff).bits(), 0xff);
/// assert_eq!(Val::f64(0.5).as_f64(), Some(0.5));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Val {
    ty: Type,
    bits: u64,
}

fn mask_for(ty: Type, ptr: PointerWidth) -> u64 {
    let w = ty.bits(ptr);
    if w >= 64 { u64::MAX } else { (1u64 << w) - 1 }
}

impl Val {
    /// A value of `ty` with the given bits (masked to the width, 64-bit pointers).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{interp::Val, Type};
    ///
    /// assert_eq!(Val::new(Type::I8, 0x1ff).bits(), 0xff);
    /// ```
    #[must_use]
    pub fn new(ty: Type, bits: u64) -> Val {
        Val {
            ty,
            bits: bits & mask_for(ty, PointerWidth::P64),
        }
    }

    /// An `i8`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::i8(-1).bits(), 0xff);
    /// ```
    #[must_use]
    pub fn i8(x: i8) -> Val {
        Val::new(Type::I8, x as u8 as u64)
    }

    /// An `i16`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::i16(-1).bits(), 0xffff);
    /// ```
    #[must_use]
    pub fn i16(x: i16) -> Val {
        Val::new(Type::I16, x as u16 as u64)
    }

    /// An `i32`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::i32(-2).as_i128(), Some(-2));
    /// ```
    #[must_use]
    pub fn i32(x: i32) -> Val {
        Val::new(Type::I32, x as u32 as u64)
    }

    /// An `i64`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::i64(i64::MIN).as_i128(), Some(i64::MIN as i128));
    /// ```
    #[must_use]
    pub fn i64(x: i64) -> Val {
        Val::new(Type::I64, x as u64)
    }

    /// A `u8`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::u8(255).as_i128(), Some(255));
    /// ```
    #[must_use]
    pub fn u8(x: u8) -> Val {
        Val::new(Type::U8, u64::from(x))
    }

    /// A `u16`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::u16(7).bits(), 7);
    /// ```
    #[must_use]
    pub fn u16(x: u16) -> Val {
        Val::new(Type::U16, u64::from(x))
    }

    /// A `u32`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::u32(u32::MAX).as_i128(), Some(u32::MAX as i128));
    /// ```
    #[must_use]
    pub fn u32(x: u32) -> Val {
        Val::new(Type::U32, u64::from(x))
    }

    /// A `u64`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::u64(1).bits(), 1);
    /// ```
    #[must_use]
    pub fn u64(x: u64) -> Val {
        Val::new(Type::U64, x)
    }

    /// An `f32`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::f32(0.5).as_f64(), Some(0.5));
    /// ```
    #[must_use]
    pub fn f32(x: f32) -> Val {
        Val::new(Type::F32, u64::from(x.to_bits()))
    }

    /// An `f64`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::f64(-0.0).bits(), 1 << 63);
    /// ```
    #[must_use]
    pub fn f64(x: f64) -> Val {
        Val::new(Type::F64, x.to_bits())
    }

    /// A `bool`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::bool(true).as_bool(), Some(true));
    /// ```
    #[must_use]
    pub fn bool(x: bool) -> Val {
        Val::new(Type::Bool, u64::from(x))
    }

    /// A `ptr`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{interp::Val, Type};
    ///
    /// assert_eq!(Val::ptr(4096).ty(), Type::Ptr);
    /// ```
    #[must_use]
    pub fn ptr(addr: u64) -> Val {
        Val::new(Type::Ptr, addr)
    }

    /// The type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{interp::Val, Type};
    ///
    /// assert_eq!(Val::u16(1).ty(), Type::U16);
    /// ```
    #[must_use]
    pub fn ty(self) -> Type {
        self.ty
    }

    /// The bit pattern.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::i32(-1).bits(), 0xffff_ffff);
    /// ```
    #[must_use]
    pub fn bits(self) -> u64 {
        self.bits
    }

    /// The integer value (sign-extended for signed types), or `None` if not an
    /// integer.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::i8(-3).as_i128(), Some(-3));
    /// assert_eq!(Val::f64(1.0).as_i128(), None);
    /// ```
    #[must_use]
    pub fn as_i128(self) -> Option<i128> {
        self.ty.int_bits().map(|_| sval(self.ty, self.bits))
    }

    /// The float value (widened from `f32` exactly), or `None` if not a float.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::f32(2.5).as_f64(), Some(2.5));
    /// assert_eq!(Val::bool(true).as_f64(), None);
    /// ```
    #[must_use]
    pub fn as_f64(self) -> Option<f64> {
        match self.ty {
            Type::F64 => Some(f64::from_bits(self.bits)),
            Type::F32 => Some(f64::from(f32::from_bits(self.bits as u32))),
            _ => None,
        }
    }

    /// The boolean value, or `None` if not a `bool`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert_eq!(Val::bool(false).as_bool(), Some(false));
    /// assert_eq!(Val::u8(1).as_bool(), None);
    /// ```
    #[must_use]
    pub fn as_bool(self) -> Option<bool> {
        (self.ty == Type::Bool).then_some(self.bits != 0)
    }

    /// Whether two values are the same, treating any two NaNs of one float type as
    /// equal (OPS §4: NaN payloads are not observable except through bit casts).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::Val;
    ///
    /// assert!(Val::f64(f64::NAN).same_as(Val::f64(-f64::NAN)));
    /// assert!(!Val::f64(0.0).same_as(Val::f64(-0.0)));
    /// ```
    #[must_use]
    pub fn same_as(self, other: Val) -> bool {
        if self.ty != other.ty {
            return false;
        }
        match (self.as_f64(), other.as_f64()) {
            (Some(a), Some(b)) if a.is_nan() && b.is_nan() => true,
            _ => self.bits == other.bits,
        }
    }
}

impl fmt::Display for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        crate::print::write_const(f, self.ty, self.bits, PointerWidth::P64)?;
        write!(f, ": {}", self.ty)
    }
}

/// The integer value of `bits` read as `ty` (sign-extended for signed types).
fn sval(ty: Type, bits: u64) -> i128 {
    let w = ty.int_bits().unwrap_or(64);
    if ty.is_signed_int() {
        let shift = 64 - w;
        i128::from(((bits << shift) as i64) >> shift)
    } else {
        i128::from(bits)
    }
}

fn int_range(ty: Type) -> (i128, i128) {
    let w = ty.int_bits().unwrap_or(64);
    if ty.is_signed_int() {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    }
}

fn wrap(ty: Type, v: i128) -> u64 {
    (v as u64) & mask_for(ty, PointerWidth::P64)
}

// --------------------------------------------------------- operation semantics

/// Why an operation did not produce a value: an `error` policy (control leaves
/// through the `check` terminator's error edge) or a `trap` policy (the program
/// aborts), with the OPS error kind.
///
/// # Examples
///
/// ```
/// use ir_lang::interp::{eval_binary, OpFault};
/// use ir_lang::{BinaryOp, DivZero, OpError, Overflow, Policy, Type};
///
/// let p = Policy::overflow(Overflow::Wrap).with_div_zero(DivZero::Trap);
/// assert_eq!(eval_binary(BinaryOp::Div, p, Type::I32, 7, 0), Err(OpFault::Trap(OpError::DivByZero)));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum OpFault {
    /// The policy is `error`.
    Error(OpError),
    /// The policy is `trap`.
    Trap(OpError),
}

fn overflow(policy: Policy, wrapped: u64) -> Result<u64, OpFault> {
    match policy.overflow {
        Some(Overflow::Wrap) | None => Ok(wrapped),
        Some(Overflow::Trap) => Err(OpFault::Trap(OpError::ArithOverflow)),
        Some(Overflow::Error) => Err(OpFault::Error(OpError::ArithOverflow)),
    }
}

fn div_zero(policy: Policy) -> OpFault {
    match policy.div_zero {
        Some(DivZero::Trap) => OpFault::Trap(OpError::DivByZero),
        _ => OpFault::Error(OpError::DivByZero),
    }
}

/// An exact integer result: its bits if representable in `ty`, else the
/// `overflow` policy applied to the wrapped bits.
fn exact(ty: Type, policy: Policy, v: Option<i128>, wrapped: u64) -> Result<u64, OpFault> {
    let (lo, hi) = int_range(ty);
    match v {
        Some(v) if v >= lo && v <= hi => Ok(wrap(ty, v)),
        _ => overflow(policy, wrapped),
    }
}

fn float_op(
    ty: Type,
    a: u64,
    b: u64,
    f32op: fn(f32, f32) -> f32,
    f64op: fn(f64, f64) -> f64,
) -> u64 {
    if ty == Type::F32 {
        u64::from(f32op(f32::from_bits(a as u32), f32::from_bits(b as u32)).to_bits())
    } else {
        f64op(f64::from_bits(a), f64::from_bits(b)).to_bits()
    }
}

/// IEEE 754 `remainder`: `x - n * y` with `n` the integer nearest `x / y` (ties to
/// even), computed exactly (after fdlibm's `__ieee754_remainder`).
fn ieee_remainder(x: f64, y: f64) -> f64 {
    if y == 0.0 || x.is_infinite() || x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if y.is_infinite() {
        return x;
    }
    let negative = x.is_sign_negative();
    let p = y.abs();
    let mut r = if p <= f64::MAX / 2.0 { x % (p + p) } else { x };
    r = r.abs();
    if p < 2.0 * f64::MIN_POSITIVE {
        if r + r > p {
            r -= p;
            if r + r >= p {
                r -= p;
            }
        }
    } else {
        let half = 0.5 * p;
        if r > half {
            r -= p;
            if r >= half {
                r -= p;
            }
        }
    }
    if negative { -r } else { r }
}

fn fmin(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else if b < a {
        b
    } else if a.is_sign_negative() {
        a
    } else {
        b
    }
}

fn fmax(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b || (a == b && b.is_sign_negative()) {
        // Equal operands differ only in the sign of zero: `max` takes +0.
        a
    } else {
        b
    }
}

fn fmin32(a: f32, b: f32) -> f32 {
    fmin(f64::from(a), f64::from(b)) as f32
}

fn fmax32(a: f32, b: f32) -> f32 {
    fmax(f64::from(a), f64::from(b)) as f32
}

/// Evaluates a unary operation on bits of type `ty` (OPS §3–§4).
///
/// # Errors
///
/// [`OpFault`] when an integer `neg`/`abs` overflows under `error` or `trap`.
///
/// # Examples
///
/// ```
/// use ir_lang::interp::{eval_unary, OpFault};
/// use ir_lang::{OpError, Overflow, Policy, Type, UnaryOp};
///
/// let p = Policy::overflow(Overflow::Error);
/// assert_eq!(eval_unary(UnaryOp::Neg, p, Type::I8, 0x80), Err(OpFault::Error(OpError::ArithOverflow)));
/// assert_eq!(eval_unary(UnaryOp::Neg, Policy::overflow(Overflow::Wrap), Type::I8, 0x80), Ok(0x80));
/// ```
pub fn eval_unary(op: UnaryOp, policy: Policy, ty: Type, a: u64) -> Result<u64, OpFault> {
    if ty.is_int() {
        let x = sval(ty, a);
        return match op {
            UnaryOp::Neg => exact(ty, policy, Some(-x), wrap(ty, 0i128.wrapping_sub(x))),
            UnaryOp::Abs => exact(ty, policy, Some(x.abs()), wrap(ty, x.abs())),
            _ => Ok(!a & mask_for(ty, PointerWidth::P64)),
        };
    }
    if ty == Type::Bool {
        return Ok(a ^ 1);
    }
    let sign = if ty == Type::F32 {
        1u64 << 31
    } else {
        1u64 << 63
    };
    match op {
        UnaryOp::Neg => return Ok(a ^ sign),
        UnaryOp::Abs => return Ok(a & !sign),
        _ => {}
    }
    if ty == Type::F32 {
        let x = f32::from_bits(a as u32);
        let r = match op {
            UnaryOp::Sqrt => x.sqrt(),
            UnaryOp::Floor => x.floor(),
            UnaryOp::Ceil => x.ceil(),
            UnaryOp::Trunc => x.trunc(),
            UnaryOp::Round => x.round(),
            _ => x.round_ties_even(),
        };
        Ok(u64::from(r.to_bits()))
    } else {
        let x = f64::from_bits(a);
        let r = match op {
            UnaryOp::Sqrt => x.sqrt(),
            UnaryOp::Floor => x.floor(),
            UnaryOp::Ceil => x.ceil(),
            UnaryOp::Trunc => x.trunc(),
            UnaryOp::Round => x.round(),
            _ => x.round_ties_even(),
        };
        Ok(r.to_bits())
    }
}

fn int_binary(op: BinaryOp, policy: Policy, ty: Type, a: u64, b: u64) -> Result<u64, OpFault> {
    let (x, y) = (sval(ty, a), sval(ty, b));
    let m = mask_for(ty, PointerWidth::P64);
    let w = ty.int_bits().unwrap_or(64);
    match op {
        BinaryOp::Add => exact(ty, policy, x.checked_add(y), a.wrapping_add(b) & m),
        BinaryOp::Sub => exact(ty, policy, x.checked_sub(y), a.wrapping_sub(b) & m),
        BinaryOp::Mul => exact(ty, policy, x.checked_mul(y), a.wrapping_mul(b) & m),
        BinaryOp::Div | BinaryOp::FloorDiv => {
            if y == 0 {
                return Err(div_zero(policy));
            }
            let mut q = x / y;
            if op == BinaryOp::FloorDiv && x % y != 0 && ((x < 0) != (y < 0)) {
                q -= 1;
            }
            exact(ty, policy, Some(q), wrap(ty, q))
        }
        BinaryOp::Rem | BinaryOp::FloorMod => {
            if y == 0 {
                return Err(div_zero(policy));
            }
            let mut r = x % y;
            if op == BinaryOp::FloorMod && r != 0 && ((r < 0) != (y < 0)) {
                r += y;
            }
            Ok(wrap(ty, r))
        }
        BinaryOp::And => Ok(a & b),
        BinaryOp::Or => Ok(a | b),
        BinaryOp::Xor => Ok(a ^ b),
        BinaryOp::Shl | BinaryOp::Shr => {
            let n = if y < 0 || y >= i128::from(w) {
                match policy.shift {
                    Some(Shift::Mask) => (b & u64::from(w - 1)) as u32,
                    _ => return Err(OpFault::Error(OpError::ShiftOutOfRange)),
                }
            } else {
                y as u32
            };
            if op == BinaryOp::Shl {
                Ok(a.checked_shl(n).unwrap_or(0) & m)
            } else if ty.is_signed_int() {
                Ok(wrap(ty, x >> n))
            } else {
                Ok(a >> n)
            }
        }
        BinaryOp::Min => Ok(if x <= y { a } else { b }),
        BinaryOp::Max => Ok(if x >= y { a } else { b }),
        BinaryOp::IeeeRem | BinaryOp::TotalCmp => Ok(0),
    }
}

/// Evaluates a binary operation on bits of type `ty` (OPS §3–§4). `total_cmp`
/// gives the bits of an `i8`.
///
/// # Errors
///
/// [`OpFault`] for integer overflow, division by zero, or an out-of-range shift
/// under `error` or `trap`.
///
/// # Examples
///
/// ```
/// use ir_lang::interp::eval_binary;
/// use ir_lang::{BinaryOp, DivZero, Policy, Type};
///
/// // floor_mod: the sign follows the divisor.
/// let p = Policy::div_zero(DivZero::Error);
/// assert_eq!(eval_binary(BinaryOp::FloorMod, p, Type::I32, (-7i32) as u32 as u64, 3), Ok(2));
/// // rem: the sign follows the dividend.
/// assert_eq!(eval_binary(BinaryOp::Rem, p, Type::I32, (-7i32) as u32 as u64, 3), Ok((-1i32) as u32 as u64));
/// ```
pub fn eval_binary(op: BinaryOp, policy: Policy, ty: Type, a: u64, b: u64) -> Result<u64, OpFault> {
    if ty.is_int() {
        return int_binary(op, policy, ty, a, b);
    }
    if ty == Type::Bool {
        return Ok(match op {
            BinaryOp::And => a & b,
            BinaryOp::Or => a | b,
            _ => a ^ b,
        });
    }
    Ok(match op {
        BinaryOp::Add => float_op(ty, a, b, |x, y| x + y, |x, y| x + y),
        BinaryOp::Sub => float_op(ty, a, b, |x, y| x - y, |x, y| x - y),
        BinaryOp::Mul => float_op(ty, a, b, |x, y| x * y, |x, y| x * y),
        BinaryOp::Div => float_op(ty, a, b, |x, y| x / y, |x, y| x / y),
        BinaryOp::Rem => float_op(ty, a, b, |x, y| x % y, |x, y| x % y),
        BinaryOp::IeeeRem => float_op(
            ty,
            a,
            b,
            |x, y| ieee_remainder(f64::from(x), f64::from(y)) as f32,
            ieee_remainder,
        ),
        BinaryOp::Min => float_op(ty, a, b, fmin32, fmin),
        BinaryOp::Max => float_op(ty, a, b, fmax32, fmax),
        BinaryOp::TotalCmp => {
            let ord = if ty == Type::F32 {
                f32::from_bits(a as u32).total_cmp(&f32::from_bits(b as u32))
            } else {
                f64::from_bits(a).total_cmp(&f64::from_bits(b))
            };
            wrap(Type::I8, ord as i128)
        }
        _ => 0,
    })
}

/// Evaluates a comparison of two values of type `ty`.
///
/// # Examples
///
/// ```
/// use ir_lang::interp::eval_compare;
/// use ir_lang::{CmpOp, Type};
///
/// assert!(eval_compare(CmpOp::Lt, Type::I8, 0xff, 0)); // -1 < 0
/// assert!(!eval_compare(CmpOp::Lt, Type::U8, 0xff, 0)); // 255 < 0
/// assert!(eval_compare(CmpOp::Ne, Type::F64, f64::NAN.to_bits(), f64::NAN.to_bits()));
/// ```
#[must_use]
pub fn eval_compare(op: CmpOp, ty: Type, a: u64, b: u64) -> bool {
    if ty.is_float() {
        let (x, y) = if ty == Type::F32 {
            (
                f64::from(f32::from_bits(a as u32)),
                f64::from(f32::from_bits(b as u32)),
            )
        } else {
            (f64::from_bits(a), f64::from_bits(b))
        };
        return match op {
            CmpOp::Eq => x == y,
            CmpOp::Ne => x != y,
            CmpOp::Lt => x < y,
            CmpOp::Le => x <= y,
            CmpOp::Gt => x > y,
            CmpOp::Ge => x >= y,
        };
    }
    let (x, y) = if ty.is_int() {
        (sval(ty, a), sval(ty, b))
    } else {
        (i128::from(a), i128::from(b))
    };
    match op {
        CmpOp::Eq => x == y,
        CmpOp::Ne => x != y,
        CmpOp::Lt => x < y,
        CmpOp::Le => x <= y,
        CmpOp::Gt => x > y,
        CmpOp::Ge => x >= y,
    }
}

/// Evaluates fused multiply-add on floats of type `ty`.
///
/// # Examples
///
/// ```
/// use ir_lang::interp::eval_fma;
/// use ir_lang::Type;
///
/// let r = eval_fma(Type::F64, 2.0f64.to_bits(), 3.0f64.to_bits(), 1.0f64.to_bits());
/// assert_eq!(f64::from_bits(r), 7.0);
/// ```
#[must_use]
pub fn eval_fma(ty: Type, a: u64, b: u64, c: u64) -> u64 {
    if ty == Type::F32 {
        let r =
            f32::from_bits(a as u32).mul_add(f32::from_bits(b as u32), f32::from_bits(c as u32));
        u64::from(r.to_bits())
    } else {
        f64::from_bits(a)
            .mul_add(f64::from_bits(b), f64::from_bits(c))
            .to_bits()
    }
}

/// Evaluates a conversion from `from` to `to` (OPS §5). `ref_to_ptr` is not
/// evaluated here (it needs the heap); it returns its input.
///
/// # Errors
///
/// [`OpFault`] for an `int_cast` that does not fit, a `float_to_int` of NaN or an
/// out-of-range value under `error`, or `char_from_u32` of a non-scalar value.
///
/// # Examples
///
/// ```
/// use ir_lang::interp::{eval_convert, OpFault};
/// use ir_lang::{ConvOp, FloatToInt, OpError, Policy, Type};
///
/// let sat = Policy::float_to_int(FloatToInt::Saturate);
/// assert_eq!(eval_convert(ConvOp::FloatToInt, sat, Type::F64, Type::U8, 300.7f64.to_bits()), Ok(255));
/// assert_eq!(eval_convert(ConvOp::FloatToInt, sat, Type::F64, Type::I32, f64::NAN.to_bits()), Ok(0));
/// let err = Policy::float_to_int(FloatToInt::Error);
/// assert_eq!(
///     eval_convert(ConvOp::FloatToInt, err, Type::F64, Type::U8, (-1.0f64).to_bits()),
///     Err(OpFault::Error(OpError::InvalidConversion)),
/// );
/// ```
pub fn eval_convert(
    op: ConvOp,
    policy: Policy,
    from: Type,
    to: Type,
    a: u64,
) -> Result<u64, OpFault> {
    let tm = mask_for(to, PointerWidth::P64);
    match op {
        ConvOp::IntCast => {
            let v = sval(from, a);
            exact(to, policy, Some(v), wrap(to, v))
        }
        ConvOp::Zext
        | ConvOp::Narrow
        | ConvOp::Bitcast
        | ConvOp::BoolToInt
        | ConvOp::PtrToInt
        | ConvOp::IntToPtr
        | ConvOp::RefToPtr => Ok(a & tm),
        ConvOp::Sext => {
            let signed = Type::int(from.int_bits().unwrap_or(64), true).unwrap_or(Type::I64);
            Ok(wrap(to, sval(signed, a)))
        }
        ConvOp::IntToFloat => {
            let v = sval(from, a);
            Ok(if to == Type::F32 {
                u64::from((v as f32).to_bits())
            } else {
                (v as f64).to_bits()
            })
        }
        ConvOp::FloatToInt => {
            let x = if from == Type::F32 {
                f64::from(f32::from_bits(a as u32))
            } else {
                f64::from_bits(a)
            };
            let w = to.int_bits().unwrap_or(64);
            let (lo, hi_excl) = if to.is_signed_int() {
                (-pow2(w - 1), pow2(w - 1))
            } else {
                (0.0, pow2(w))
            };
            let t = x.trunc();
            if !x.is_nan() && t >= lo && t < hi_excl {
                return Ok(wrap(to, t as i128));
            }
            match policy.float_to_int {
                Some(FloatToInt::Saturate) => {
                    let (min, max) = int_range(to);
                    Ok(if x.is_nan() {
                        0
                    } else if x < 0.0 {
                        wrap(to, min)
                    } else {
                        wrap(to, max)
                    })
                }
                _ => Err(OpFault::Error(OpError::InvalidConversion)),
            }
        }
        ConvOp::FloatCast => Ok(if to == Type::F64 {
            f64::from(f32::from_bits(a as u32)).to_bits()
        } else {
            u64::from((f64::from_bits(a) as f32).to_bits())
        }),
        ConvOp::CharFromU32 => {
            if a <= 0x10_FFFF && !(0xD800..=0xDFFF).contains(&a) {
                Ok(a)
            } else {
                Err(OpFault::Error(OpError::InvalidChar))
            }
        }
    }
}

fn pow2(n: u32) -> f64 {
    // Exact for every n used here (n <= 64).
    let mut x = 1.0f64;
    for _ in 0..n {
        x *= 2.0;
    }
    x
}

// ------------------------------------------------------------------- errors

/// Why execution stopped without an [`Outcome`]: a budget ran out, the program
/// did something undefined, or the host failed.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum InterpError {
    /// The module does not validate; nothing was run.
    Invalid(ValidationError),
    /// The function has no body and no host handled it.
    UnresolvedImport {
        /// The function's name.
        name: String,
    },
    /// The arguments do not match the function's parameters.
    BadArguments,
    /// [`Limits::fuel`] instructions were executed.
    FuelExhausted,
    /// [`Limits::max_depth`] frames were live.
    StackOverflow,
    /// [`Limits::memory`] is exhausted.
    OutOfMemory,
    /// An access outside every live allocation (null, freed stack, code).
    MemoryFault {
        /// The address.
        addr: u64,
        /// The access size.
        len: u64,
    },
    /// An address is not a multiple of the alignment the access claims.
    Misaligned {
        /// The address.
        addr: u64,
        /// The alignment.
        align: u64,
    },
    /// `memcpy` ranges overlap.
    Overlap,
    /// A loaded `bool` was neither 0 nor 1.
    InvalidBool,
    /// Control reached `unreachable`.
    UnreachableReached,
    /// A `ref` from before a safepoint was used without relocation, or a `ref`
    /// names no object.
    StaleRef,
    /// An indirect call's address is not a function, or the function's signature
    /// differs from the call's.
    BadFunctionPointer {
        /// The address.
        addr: u64,
    },
    /// An operation with an `error` policy ran outside a `check` terminator (only
    /// possible for a module that skipped validation).
    UncheckedError,
    /// A host function reported a failure.
    Host(String),
}

impl fmt::Display for InterpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InterpError::Invalid(e) => write!(f, "invalid module: {e}"),
            InterpError::UnresolvedImport { name } => write!(f, "unresolved import `{name}`"),
            InterpError::BadArguments => f.write_str("arguments do not match the signature"),
            InterpError::FuelExhausted => f.write_str("fuel exhausted"),
            InterpError::StackOverflow => f.write_str("call depth limit reached"),
            InterpError::OutOfMemory => f.write_str("out of memory"),
            InterpError::MemoryFault { addr, len } => {
                write!(f, "invalid access of {len} bytes at {addr:#x}")
            }
            InterpError::Misaligned { addr, align } => {
                write!(f, "address {addr:#x} is not aligned to {align}")
            }
            InterpError::Overlap => f.write_str("memcpy ranges overlap"),
            InterpError::InvalidBool => f.write_str("loaded bool is not 0 or 1"),
            InterpError::UnreachableReached => f.write_str("reached unreachable"),
            InterpError::StaleRef => f.write_str("stale or invalid GC reference"),
            InterpError::BadFunctionPointer { addr } => write!(f, "bad function pointer {addr:#x}"),
            InterpError::UncheckedError => f.write_str("error policy outside a check"),
            InterpError::Host(msg) => write!(f, "host: {msg}"),
        }
    }
}

impl core::error::Error for InterpError {}

/// How a call ended.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Outcome {
    /// Normal return with the function's return values.
    Return(Vec<Val>),
    /// An exception unwound out of the function, with its payload (a `ptr`).
    Unwind(Val),
    /// The program trapped with this code.
    Trap(u32),
}

/// Execution budgets.
///
/// # Examples
///
/// ```
/// use ir_lang::interp::Limits;
///
/// let l = Limits { fuel: 1_000, ..Limits::default() };
/// assert_eq!(l.fuel, 1_000);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// Instructions (and terminators) executed before [`InterpError::FuelExhausted`]
    /// (default 10 million). Bulk memory operations cost one unit per 64 bytes.
    pub fuel: u64,
    /// Live frames before [`InterpError::StackOverflow`] (default 1000).
    pub max_depth: u32,
    /// Bytes of address space (default 4 MiB; capped at 4 GiB, 3.5 GiB for 32-bit
    /// modules). Backing storage grows on demand; growth the system refuses is
    /// [`InterpError::OutOfMemory`].
    pub memory: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            fuel: 10_000_000,
            max_depth: 1000,
            memory: 4 << 20,
        }
    }
}

// ------------------------------------------------------------------- memory

const NULL_GUARD: u64 = 4096;

/// The interpreter's address space, also handed to host functions.
///
/// Backing storage grows on demand from both ends (globals and the stack from the
/// bottom, the host heap from the top), so a large [`Limits::memory`] costs only
/// what a program touches.
#[derive(Clone, Debug)]
pub struct Memory {
    /// Bytes `[0, low.len())`: globals and the stack.
    low: Vec<u8>,
    /// Bytes `[size - high.len(), size)`: the host heap.
    high: Vec<u8>,
    size: u64,
    ptr: PointerWidth,
    globals_end: u64,
    stack_base: u64,
    sp: u64,
    heap_floor: u64,
    epoch: u64,
    objects: Vec<(u64, u64)>,
}

impl Memory {
    fn new(size: u64, ptr: PointerWidth) -> Result<Memory, InterpError> {
        let cap = match ptr {
            PointerWidth::P32 => 0xE000_0000,
            PointerWidth::P64 => 1 << 32,
        };
        let size = size.min(cap).max(NULL_GUARD);
        Ok(Memory {
            low: Vec::new(),
            high: Vec::new(),
            size,
            ptr,
            globals_end: NULL_GUARD,
            stack_base: NULL_GUARD,
            sp: NULL_GUARD,
            heap_floor: size,
            epoch: 0,
            objects: Vec::new(),
        })
    }

    fn size(&self) -> u64 {
        self.size
    }

    /// Makes `[0, end)` backed. Growth is fallible, so an exhausted host is an
    /// error, not an abort.
    fn grow_low(&mut self, end: u64) -> Result<(), InterpError> {
        let end = usize::try_from(end).map_err(|_| InterpError::OutOfMemory)?;
        if end > self.low.len() {
            let want = end
                .max(self.low.len().saturating_mul(2))
                .min(self.size as usize);
            self.low
                .try_reserve_exact(want - self.low.len())
                .map_err(|_| InterpError::OutOfMemory)?;
            self.low.resize(want, 0);
        }
        Ok(())
    }

    /// Makes `[floor, size)` backed.
    fn grow_high(&mut self, floor: u64) -> Result<(), InterpError> {
        let need = usize::try_from(self.size - floor).map_err(|_| InterpError::OutOfMemory)?;
        if need > self.high.len() {
            let want = need
                .max(self.high.len().saturating_mul(2))
                .min(self.size as usize);
            let mut grown = Vec::new();
            grown
                .try_reserve_exact(want)
                .map_err(|_| InterpError::OutOfMemory)?;
            grown.resize(want - self.high.len(), 0);
            grown.extend_from_slice(&self.high);
            self.high = grown;
        }
        Ok(())
    }

    /// Reserves and zero-fills `[addr, end)` of the stack.
    fn claim_stack(&mut self, addr: u64, end: u64) -> Result<(), InterpError> {
        self.grow_low(end)?;
        if let Some(bytes) = self.low.get_mut(addr as usize..end as usize) {
            bytes.fill(0);
        }
        Ok(())
    }

    fn live(&self, addr: u64, len: u64) -> bool {
        let Some(end) = addr.checked_add(len) else {
            return false;
        };
        let within = |lo: u64, hi: u64| addr >= lo && end <= hi;
        within(NULL_GUARD, self.globals_end)
            || within(self.stack_base, self.sp)
            || within(self.heap_floor, self.size())
    }

    /// The backing bytes of a live range: globals and the stack lie below the heap.
    fn range(&self, addr: u64, len: u64) -> Option<&[u8]> {
        if addr < self.heap_floor {
            self.low.get(addr as usize..(addr + len) as usize)
        } else {
            let base = self.size - self.high.len() as u64;
            let start = (addr - base) as usize;
            self.high.get(start..start + len as usize)
        }
    }

    fn range_mut(&mut self, addr: u64, len: u64) -> Option<&mut [u8]> {
        if addr < self.heap_floor {
            self.low.get_mut(addr as usize..(addr + len) as usize)
        } else {
            let base = self.size - self.high.len() as u64;
            let start = (addr - base) as usize;
            self.high.get_mut(start..start + len as usize)
        }
    }

    /// Reads `len` bytes at `addr`.
    ///
    /// # Errors
    ///
    /// [`InterpError::MemoryFault`] unless the range is inside one live allocation.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost, InterpError};
    /// use ir_lang::Module;
    ///
    /// let m = Module::new("m");
    /// let vm = Interpreter::new(&m, NoHost)?;
    /// assert!(matches!(vm.memory().read(0, 1), Err(InterpError::MemoryFault { .. })));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read(&self, addr: u64, len: u64) -> Result<&[u8], InterpError> {
        if !self.live(addr, len) {
            return Err(InterpError::MemoryFault { addr, len });
        }
        self.range(addr, len)
            .ok_or(InterpError::MemoryFault { addr, len })
    }

    /// Writes `data` at `addr`.
    ///
    /// # Errors
    ///
    /// [`InterpError::MemoryFault`] unless the range is inside one live allocation.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost};
    /// use ir_lang::Module;
    ///
    /// let m = Module::new("m");
    /// let mut vm = Interpreter::new(&m, NoHost)?;
    /// let a = vm.memory_mut().alloc(4, 4)?;
    /// vm.memory_mut().write(a, &[9; 4])?;
    /// assert!(vm.memory_mut().write(a, &[9; 5]).is_err());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn write(&mut self, addr: u64, data: &[u8]) -> Result<(), InterpError> {
        let len = data.len() as u64;
        if !self.live(addr, len) {
            return Err(InterpError::MemoryFault { addr, len });
        }
        self.range_mut(addr, len)
            .ok_or(InterpError::MemoryFault { addr, len })?
            .copy_from_slice(data);
        Ok(())
    }

    /// Allocates `size` zeroed bytes on the host heap (never freed).
    ///
    /// # Errors
    ///
    /// [`InterpError::OutOfMemory`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost};
    /// use ir_lang::Module;
    ///
    /// let m = Module::new("m");
    /// let mut vm = Interpreter::new(&m, NoHost)?;
    /// let a = vm.memory_mut().alloc(64, 16)?;
    /// assert_eq!(a % 16, 0);
    /// assert_eq!(vm.memory().read(a, 64)?, &[0; 64]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn alloc(&mut self, size: u64, align: u64) -> Result<u64, InterpError> {
        let align = align.max(1).next_power_of_two();
        let floor = self
            .heap_floor
            .checked_sub(size)
            .ok_or(InterpError::OutOfMemory)?
            & !(align - 1);
        if floor < self.sp.max(self.globals_end) {
            return Err(InterpError::OutOfMemory);
        }
        self.grow_high(floor)?;
        self.heap_floor = floor;
        Ok(floor)
    }

    /// Allocates a GC object of `size` zeroed bytes and returns a `ref` to it,
    /// valid until the next safepoint.
    ///
    /// # Errors
    ///
    /// [`InterpError::OutOfMemory`].
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost};
    /// use ir_lang::{Module, Type};
    ///
    /// let m = Module::new("m");
    /// let mut vm = Interpreter::new(&m, NoHost)?;
    /// let r = vm.memory_mut().alloc_object(24)?;
    /// assert_eq!(r.ty(), Type::Ref);
    /// assert_ne!(vm.memory().ref_address(r)?, 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn alloc_object(&mut self, size: u64) -> Result<Val, InterpError> {
        let addr = self.alloc(size.max(1), 16)?;
        self.objects.push((addr, size));
        let index = self.objects.len() as u64;
        Ok(Val::new(Type::Ref, self.encode_ref(index)))
    }

    fn idx_bits(&self) -> u32 {
        match self.ptr {
            PointerWidth::P32 => 20,
            PointerWidth::P64 => 32,
        }
    }

    /// Encodes object number `index` (1-based; 0 is null) in the current epoch.
    fn encode_ref(&self, index: u64) -> u64 {
        if index == 0 {
            return 0;
        }
        let bits = self.idx_bits();
        let epoch_mask = (1u64 << (self.ptr.bits() - bits)) - 1;
        ((self.epoch & epoch_mask) << bits) | index
    }

    /// Decodes a `ref` minted in the current epoch to its object number.
    fn decode_ref(&self, bits: u64) -> Result<u64, InterpError> {
        if bits == 0 {
            return Ok(0);
        }
        let ib = self.idx_bits();
        let epoch_mask = (1u64 << (self.ptr.bits() - ib)) - 1;
        let index = bits & ((1u64 << ib) - 1);
        if bits >> ib != self.epoch & epoch_mask || index == 0 || index > self.objects.len() as u64
        {
            return Err(InterpError::StaleRef);
        }
        Ok(index)
    }

    /// The address of the object a `ref` names (0 for null).
    ///
    /// # Errors
    ///
    /// [`InterpError::StaleRef`] for a reference from an earlier epoch.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::interp::{Interpreter, NoHost, Val};
    /// use ir_lang::{Module, Type};
    ///
    /// let m = Module::new("m");
    /// let vm = Interpreter::new(&m, NoHost)?;
    /// assert_eq!(vm.memory().ref_address(Val::new(Type::Ref, 0))?, 0); // null
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn ref_address(&self, r: Val) -> Result<u64, InterpError> {
        let index = self.decode_ref(r.bits())?;
        if index == 0 {
            return Ok(0);
        }
        self.objects
            .get(index as usize - 1)
            .map(|o| o.0)
            .ok_or(InterpError::StaleRef)
    }

    fn load(&self, ty: Type, addr: u64) -> Result<u64, InterpError> {
        let size = u64::from(ty.size(self.ptr));
        let raw = self.read(addr, size)?;
        let mut bits = 0u64;
        for (i, &b) in raw.iter().enumerate() {
            bits |= u64::from(b) << (8 * i);
        }
        match ty {
            Type::Bool if bits > 1 => Err(InterpError::InvalidBool),
            Type::Ref => {
                if bits > self.objects.len() as u64 {
                    return Err(InterpError::StaleRef);
                }
                Ok(self.encode_ref(bits))
            }
            _ => Ok(bits),
        }
    }

    fn store(&mut self, ty: Type, addr: u64, bits: u64) -> Result<(), InterpError> {
        let size = ty.size(self.ptr) as usize;
        let bits = if ty == Type::Ref {
            self.decode_ref(bits)?
        } else {
            bits
        };
        let le = bits.to_le_bytes();
        self.write(addr, le.get(..size).unwrap_or(&le))
    }

    fn new_epoch(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }
}

// --------------------------------------------------------------------- host

/// What a host function returned.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum HostReturn {
    /// Normal return values (matching the import's signature).
    Values(Vec<Val>),
    /// Unwind with this payload (a `ptr`).
    Unwind(Val),
}

/// Implements imported functions for the interpreter.
///
/// # Examples
///
/// ```
/// use ir_lang::interp::{Host, HostReturn, InterpError, Memory, Val};
///
/// struct Alloc;
/// impl Host for Alloc {
///     fn call(&mut self, name: &str, args: &[Val], mem: &mut Memory) -> Result<HostReturn, InterpError> {
///         match name {
///             "gc_alloc" => Ok(HostReturn::Values(vec![mem.alloc_object(args[0].bits())?])),
///             _ => Err(InterpError::UnresolvedImport { name: name.into() }),
///         }
///     }
/// }
/// # let _ = Alloc;
/// ```
pub trait Host {
    /// Runs the import `name` with `args`.
    ///
    /// # Errors
    ///
    /// Any [`InterpError`]; it stops execution.
    fn call(
        &mut self,
        name: &str,
        args: &[Val],
        mem: &mut Memory,
    ) -> Result<HostReturn, InterpError>;
}

/// A host with no functions: every import is [`InterpError::UnresolvedImport`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHost;

impl Host for NoHost {
    fn call(&mut self, name: &str, _: &[Val], _: &mut Memory) -> Result<HostReturn, InterpError> {
        Err(InterpError::UnresolvedImport { name: name.into() })
    }
}

mod exec;

pub use exec::{Interpreter, run};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ieee_remainder_ties_to_even() {
        assert_eq!(ieee_remainder(5.0, 2.0), 1.0); // 2.5 rounds to 2
        assert_eq!(ieee_remainder(7.0, 2.0), -1.0); // 3.5 rounds to 4
        assert_eq!(ieee_remainder(-5.0, 2.0), -1.0);
        assert!(ieee_remainder(1.0, 0.0).is_nan());
        assert!(ieee_remainder(f64::INFINITY, 1.0).is_nan());
        assert_eq!(ieee_remainder(3.0, f64::INFINITY), 3.0);
        assert!(ieee_remainder(f64::MAX, 3.0).abs() <= 1.5);
        let tiny = f64::MIN_POSITIVE;
        assert_eq!(ieee_remainder(3.0 * tiny, 2.0 * tiny), -tiny);
    }

    #[test]
    fn test_float_min_max_signed_zero() {
        assert!(fmin(-0.0, 0.0).is_sign_negative());
        assert!(fmin(0.0, -0.0).is_sign_negative());
        assert!(fmax(-0.0, 0.0).is_sign_positive());
        assert!(fmax(0.0, -0.0).is_sign_positive());
        assert!(fmin(f64::NAN, 1.0).is_nan());
        assert_eq!(fmax(2.0, 1.0), 2.0);
        assert_eq!(fmax(1.0, 2.0), 2.0);
    }

    #[test]
    fn test_ref_encoding_detects_stale_epoch() {
        let Ok(mut m) = Memory::new(1 << 16, PointerWidth::P64) else {
            return;
        };
        let Ok(r) = m.alloc_object(8) else { return };
        assert!(m.ref_address(r).is_ok());
        m.new_epoch();
        assert_eq!(m.ref_address(r), Err(InterpError::StaleRef));
        assert_eq!(m.ref_address(Val::new(Type::Ref, 0)), Ok(0));
        assert_eq!(
            m.ref_address(Val::new(Type::Ref, 99)),
            Err(InterpError::StaleRef)
        );
    }

    #[test]
    fn test_32_bit_refs_fit_the_address_width() {
        let Ok(mut m) = Memory::new(1 << 16, PointerWidth::P32) else {
            return;
        };
        for _ in 0..5000 {
            m.new_epoch();
        }
        let Ok(r) = m.alloc_object(4) else { return };
        assert!(r.bits() <= u64::from(u32::MAX));
        assert!(m.ref_address(r).is_ok());
    }

    #[test]
    fn test_memory_rejects_null_overflow_and_unallocated_ranges() {
        let Ok(mut m) = Memory::new(1 << 16, PointerWidth::P64) else {
            return;
        };
        assert!(m.read(0, 1).is_err());
        assert!(m.read(NULL_GUARD, 1).is_err()); // no globals, empty stack
        assert!(m.read(u64::MAX, 2).is_err());
        let Ok(a) = m.alloc(16, 8) else { return };
        assert!(m.write(a, &[1; 16]).is_ok());
        assert!(m.write(a, &[1; 17]).is_err());
        assert_eq!(m.alloc(1 << 20, 8), Err(InterpError::OutOfMemory));
    }

    #[test]
    fn test_val_display_and_accessors() {
        assert_eq!(Val::i16(-2).to_string(), "-2: i16");
        assert_eq!(Val::bool(true).as_bool(), Some(true));
        assert_eq!(Val::u8(3).as_f64(), None);
        assert_eq!(Val::f32(1.5).as_f64(), Some(1.5));
        assert!(!Val::i32(1).same_as(Val::u32(1)));
    }
}
