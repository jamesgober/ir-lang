//! Operations and their policies, as specified by `specs/OPS.md`.
//!
//! Every operation instance carries its policy. The policy fields an instance must
//! carry are fixed by the operation and its operand class (integer or float), so no
//! tier ever guesses a default: an integer `add` carries exactly `overflow`, a float
//! `add` carries nothing, an integer `div` carries `overflow` and `div_zero`.
//! [`Policy::required_unary`], [`Policy::required_binary`], and [`Policy::required_conversion`] are that table.
//!
//! `promote` (OPS §2) is not representable: it is only valid on dynamically typed
//! results, and every ir-lang value has a static type.

use core::fmt;

use crate::types::Type;

/// A unary operation.
///
/// # Examples
///
/// ```
/// use ir_lang::UnaryOp;
///
/// assert_eq!(UnaryOp::RoundEven.name(), "round_even");
/// assert_eq!(UnaryOp::from_name("neg"), Some(UnaryOp::Neg));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum UnaryOp {
    /// Integer `0 - x` (policy `overflow`), or float sign flip.
    Neg,
    /// Integer absolute value (policy `overflow`; identity on unsigned), or float
    /// sign clear.
    Abs,
    /// Bitwise complement (integers) or logical negation (`bool`).
    Not,
    /// Float square root, correctly rounded.
    Sqrt,
    /// Float round toward negative infinity.
    Floor,
    /// Float round toward positive infinity.
    Ceil,
    /// Float round toward zero.
    Trunc,
    /// Float round to nearest, ties away from zero.
    Round,
    /// Float round to nearest, ties to even.
    RoundEven,
}

/// A binary operation. Both operands have the same type.
///
/// # Examples
///
/// ```
/// use ir_lang::BinaryOp;
///
/// assert_eq!(BinaryOp::FloorDiv.name(), "floor_div");
/// assert_eq!(BinaryOp::from_name("ieee_rem"), Some(BinaryOp::IeeeRem));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum BinaryOp {
    /// Addition (integer: `overflow`; float: IEEE).
    Add,
    /// Subtraction (integer: `overflow`; float: IEEE).
    Sub,
    /// Multiplication (integer: `overflow`; float: IEEE).
    Mul,
    /// Division truncating toward zero (integer: `overflow`, `div_zero`; float: IEEE).
    Div,
    /// Remainder with the dividend's sign (integer: `div_zero`; float: C `fmod`).
    Rem,
    /// Integer division rounding toward negative infinity (`overflow`, `div_zero`).
    FloorDiv,
    /// Integer remainder with the divisor's sign (`div_zero`).
    FloorMod,
    /// Float IEEE 754 `remainder` (round-to-nearest-even quotient).
    IeeeRem,
    /// Bitwise and (integers) or logical and (`bool`).
    And,
    /// Bitwise or (integers) or logical or (`bool`).
    Or,
    /// Bitwise xor (integers) or logical xor (`bool`).
    Xor,
    /// Integer shift left (`shift`). The amount has the operand type.
    Shl,
    /// Integer shift right: arithmetic for signed, logical for unsigned (`shift`).
    Shr,
    /// Minimum (integer: numeric; float: NaN-propagating, `min(-0, +0) = -0`).
    Min,
    /// Maximum (integer: numeric; float: NaN-propagating, `max(-0, +0) = +0`).
    Max,
    /// Float IEEE 754 totalOrder comparison, giving an `i8` of `-1`, `0`, or `1`.
    TotalCmp,
}

/// A comparison. Both operands have the same type; the result is `bool`.
///
/// Integer comparisons take their signedness from the operand type; float
/// comparisons are IEEE (any comparison with NaN is false, except `ne`). `bool`,
/// `ptr`, and `ref` support `eq` and `ne` only.
///
/// # Examples
///
/// ```
/// use ir_lang::CmpOp;
///
/// assert_eq!(CmpOp::Le.name(), "le");
/// assert_eq!(CmpOp::from_name("gt"), Some(CmpOp::Gt));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum CmpOp {
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Less than.
    Lt,
    /// Less than or equal.
    Le,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Ge,
}

/// A conversion from the operand's type to the instruction's result type.
///
/// # Examples
///
/// ```
/// use ir_lang::ConvOp;
///
/// assert_eq!(ConvOp::FloatToInt.name(), "float_to_int");
/// assert_eq!(ConvOp::from_name("narrow"), Some(ConvOp::Narrow));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum ConvOp {
    /// Integer to integer, value-preserving: exact if the value fits, else per
    /// `overflow` (`wrap` truncates or extends the bits).
    IntCast,
    /// Zero-extend an integer to a strictly wider integer.
    Zext,
    /// Sign-extend an integer to a strictly wider integer.
    Sext,
    /// Truncate an integer to a strictly narrower integer (OPS `trunc`; never fails).
    Narrow,
    /// Integer to float, round to nearest, ties to even.
    IntToFloat,
    /// Float to integer, truncating toward zero; NaN or out of range per
    /// `float_to_int`.
    FloatToInt,
    /// Float to the other float width (round to nearest even).
    FloatCast,
    /// Same-width reinterpretation between integer and float types (or between
    /// integers of the same width).
    Bitcast,
    /// A `u32` that must be a Unicode scalar value; otherwise the `InvalidChar`
    /// error. Always fallible, so only valid in a `check` terminator.
    CharFromU32,
    /// `bool` to an integer: `false` is 0, `true` is 1.
    BoolToInt,
    /// `ptr` to an address-width integer.
    PtrToInt,
    /// An address-width integer to `ptr`.
    IntToPtr,
    /// `ref` to the `ptr` of the object it references: a *derived* pointer, which
    /// must not be live across a safepoint.
    RefToPtr,
}

macro_rules! named {
    ($ty:ident { $($variant:ident => $name:literal),* $(,)? }) => {
        impl $ty {
            /// Every variant, in a fixed order.
            pub const ALL: &'static [$ty] = &[$($ty::$variant),*];

            /// The textual name used by the printer and the parser.
            ///
            /// # Examples
            ///
            /// ```
            #[doc = concat!("use ir_lang::", stringify!($ty), ";")]
            ///
            #[doc = concat!("let op = ", stringify!($ty), "::ALL[0];")]
            #[doc = concat!("assert_eq!(", stringify!($ty), "::from_name(op.name()), Some(op));")]
            /// ```
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $($ty::$variant => $name),*
                }
            }

            /// Looks the operation up by its textual name.
            ///
            /// # Examples
            ///
            /// ```
            #[doc = concat!("use ir_lang::", stringify!($ty), ";")]
            ///
            #[doc = concat!("let op = ", stringify!($ty), "::ALL[0];")]
            #[doc = concat!("assert_eq!(", stringify!($ty), "::from_name(op.name()), Some(op));")]
            /// ```
            #[must_use]
            pub fn from_name(name: &str) -> Option<$ty> {
                match name {
                    $($name => Some($ty::$variant),)*
                    _ => None,
                }
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.name())
            }
        }
    };
}

named!(UnaryOp {
    Neg => "neg",
    Abs => "abs",
    Not => "not",
    Sqrt => "sqrt",
    Floor => "floor",
    Ceil => "ceil",
    Trunc => "trunc",
    Round => "round",
    RoundEven => "round_even",
});

named!(BinaryOp {
    Add => "add",
    Sub => "sub",
    Mul => "mul",
    Div => "div",
    Rem => "rem",
    FloorDiv => "floor_div",
    FloorMod => "floor_mod",
    IeeeRem => "ieee_rem",
    And => "and",
    Or => "or",
    Xor => "xor",
    Shl => "shl",
    Shr => "shr",
    Min => "min",
    Max => "max",
    TotalCmp => "total_cmp",
});

named!(CmpOp {
    Eq => "eq",
    Ne => "ne",
    Lt => "lt",
    Le => "le",
    Gt => "gt",
    Ge => "ge",
});

named!(ConvOp {
    IntCast => "int_cast",
    Zext => "zext",
    Sext => "sext",
    Narrow => "narrow",
    IntToFloat => "int_to_float",
    FloatToInt => "float_to_int",
    FloatCast => "float_cast",
    Bitcast => "bitcast",
    CharFromU32 => "char_from_u32",
    BoolToInt => "bool_to_int",
    PtrToInt => "ptr_to_int",
    IntToPtr => "int_to_ptr",
    RefToPtr => "ref_to_ptr",
});

/// What an integer operation does when its exact result is not representable.
///
/// # Examples
///
/// ```
/// use ir_lang::Overflow;
///
/// assert_eq!(Overflow::Wrap.name(), "wrap");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum Overflow {
    /// Raise the `ArithOverflow` error: control leaves through the error edge of
    /// the `check` terminator that holds the operation.
    Error,
    /// Two's-complement wrap-around; never fails.
    Wrap,
    /// Abort the program deterministically with the `ArithOverflow` code.
    Trap,
}

/// What an integer division or remainder does when the divisor is zero.
///
/// # Examples
///
/// ```
/// use ir_lang::DivZero;
///
/// assert_eq!(DivZero::Trap.name(), "trap");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum DivZero {
    /// Raise the `DivByZero` error through a `check` terminator's error edge.
    Error,
    /// Abort the program with the `DivByZero` code.
    Trap,
}

/// What a shift does when its amount is at least the bit width (or negative).
///
/// # Examples
///
/// ```
/// use ir_lang::Shift;
///
/// assert_eq!(Shift::Mask.name(), "mask");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum Shift {
    /// Raise the `ShiftOutOfRange` error through a `check` terminator's error edge.
    Error,
    /// Use the amount modulo the bit width (`amount & (width - 1)`).
    Mask,
}

/// What `float_to_int` does with NaN or an out-of-range value.
///
/// # Examples
///
/// ```
/// use ir_lang::FloatToInt;
///
/// assert_eq!(FloatToInt::Saturate.name(), "saturate");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum FloatToInt {
    /// Raise the `InvalidConversion` error through a `check` terminator's error edge.
    Error,
    /// Clamp to the target's MIN/MAX; NaN becomes 0.
    Saturate,
}

macro_rules! policy_names {
    ($ty:ident { $($variant:ident => $name:literal),* }) => {
        impl $ty {
            /// The textual name used by the printer and the parser.
            ///
            /// # Examples
            ///
            /// ```
            #[doc = concat!("use ir_lang::", stringify!($ty), ";")]
            ///
            #[doc = concat!("assert_eq!(", stringify!($ty), "::from_name(\"error\"), Some(", stringify!($ty), "::Error));")]
            #[doc = concat!("assert_eq!(", stringify!($ty), "::Error.name(), \"error\");")]
            /// ```
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $($ty::$variant => $name),*
                }
            }

            /// Looks the policy value up by its textual name.
            ///
            /// # Examples
            ///
            /// ```
            #[doc = concat!("use ir_lang::", stringify!($ty), ";")]
            ///
            #[doc = concat!("assert_eq!(", stringify!($ty), "::from_name(\"error\"), Some(", stringify!($ty), "::Error));")]
            #[doc = concat!("assert_eq!(", stringify!($ty), "::Error.name(), \"error\");")]
            /// ```
            #[must_use]
            pub fn from_name(name: &str) -> Option<$ty> {
                match name {
                    $($name => Some($ty::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

policy_names!(Overflow { Error => "error", Wrap => "wrap", Trap => "trap" });
policy_names!(DivZero { Error => "error", Trap => "trap" });
policy_names!(Shift { Error => "error", Mask => "mask" });
policy_names!(FloatToInt { Error => "error", Saturate => "saturate" });

/// The policy carried by one operation instance.
///
/// Each field is present exactly when the operation needs it ([`Policy::required_binary`] and its siblings);
/// the validator rejects a missing or an extra field.
///
/// # Examples
///
/// ```
/// use ir_lang::{DivZero, Overflow, Policy};
///
/// let p = Policy::overflow(Overflow::Wrap).with_div_zero(DivZero::Trap);
/// assert!(!p.can_error());
/// assert_eq!(p.to_string(), "<overflow=wrap,div_zero=trap>");
/// assert_eq!(Policy::NONE.to_string(), "");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Policy {
    /// The `overflow` field.
    pub overflow: Option<Overflow>,
    /// The `div_zero` field.
    pub div_zero: Option<DivZero>,
    /// The `shift` field.
    pub shift: Option<Shift>,
    /// The `float_to_int` field.
    pub float_to_int: Option<FloatToInt>,
}

/// Which policy fields an operation instance must carry.
///
/// # Examples
///
/// ```
/// use ir_lang::{BinaryOp, Policy, Type};
///
/// let fields = Policy::required_binary(BinaryOp::Div, Type::I64);
/// assert!(fields.overflow && fields.div_zero && !fields.shift);
/// assert!(Policy::required_binary(BinaryOp::Div, Type::F64).is_empty());
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct PolicyFields {
    /// `overflow` is required.
    pub overflow: bool,
    /// `div_zero` is required.
    pub div_zero: bool,
    /// `shift` is required.
    pub shift: bool,
    /// `float_to_int` is required.
    pub float_to_int: bool,
}

impl PolicyFields {
    /// Whether no field is required.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::PolicyFields;
    ///
    /// assert!(PolicyFields::default().is_empty());
    /// ```
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !(self.overflow || self.div_zero || self.shift || self.float_to_int)
    }

    /// Whether `policy` carries exactly these fields.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Overflow, Policy, PolicyFields};
    ///
    /// let need = PolicyFields { overflow: true, ..PolicyFields::default() };
    /// assert!(need.matches(Policy::overflow(Overflow::Wrap)));
    /// assert!(!need.matches(Policy::NONE));
    /// ```
    #[must_use]
    pub const fn matches(self, policy: Policy) -> bool {
        self.overflow == policy.overflow.is_some()
            && self.div_zero == policy.div_zero.is_some()
            && self.shift == policy.shift.is_some()
            && self.float_to_int == policy.float_to_int.is_some()
    }
}

impl fmt::Display for PolicyFields {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = [
            (self.overflow, "overflow"),
            (self.div_zero, "div_zero"),
            (self.shift, "shift"),
            (self.float_to_int, "float_to_int"),
        ];
        let mut first = true;
        f.write_str("{")?;
        for (on, name) in names {
            if on {
                if !first {
                    f.write_str(", ")?;
                }
                first = false;
                f.write_str(name)?;
            }
        }
        f.write_str("}")
    }
}

impl Policy {
    /// No policy fields (comparisons, bitwise ops, float ops, most conversions).
    pub const NONE: Policy = Policy {
        overflow: None,
        div_zero: None,
        shift: None,
        float_to_int: None,
    };

    /// A policy with only `overflow`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Overflow, Policy};
    ///
    /// assert_eq!(Policy::overflow(Overflow::Trap).overflow, Some(Overflow::Trap));
    /// ```
    #[must_use]
    pub const fn overflow(o: Overflow) -> Policy {
        Policy {
            overflow: Some(o),
            ..Policy::NONE
        }
    }

    /// A policy with only `div_zero`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{DivZero, Policy};
    ///
    /// assert_eq!(Policy::div_zero(DivZero::Error).div_zero, Some(DivZero::Error));
    /// ```
    #[must_use]
    pub const fn div_zero(d: DivZero) -> Policy {
        Policy {
            div_zero: Some(d),
            ..Policy::NONE
        }
    }

    /// A policy with only `shift`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Policy, Shift};
    ///
    /// assert_eq!(Policy::shift(Shift::Mask).shift, Some(Shift::Mask));
    /// ```
    #[must_use]
    pub const fn shift(s: Shift) -> Policy {
        Policy {
            shift: Some(s),
            ..Policy::NONE
        }
    }

    /// A policy with only `float_to_int`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{FloatToInt, Policy};
    ///
    /// assert!(Policy::float_to_int(FloatToInt::Saturate).float_to_int.is_some());
    /// ```
    #[must_use]
    pub const fn float_to_int(p: FloatToInt) -> Policy {
        Policy {
            float_to_int: Some(p),
            ..Policy::NONE
        }
    }

    /// This policy with `div_zero` set.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{DivZero, Overflow, Policy};
    ///
    /// let p = Policy::overflow(Overflow::Wrap).with_div_zero(DivZero::Trap);
    /// assert_eq!(p.div_zero, Some(DivZero::Trap));
    /// ```
    #[must_use]
    pub const fn with_div_zero(mut self, d: DivZero) -> Policy {
        self.div_zero = Some(d);
        self
    }

    /// Whether any field is `error`: such an operation can fail, and is only valid
    /// inside a `check` terminator.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Overflow, Policy};
    ///
    /// assert!(Policy::overflow(Overflow::Error).can_error());
    /// assert!(!Policy::overflow(Overflow::Trap).can_error());
    /// ```
    #[must_use]
    pub const fn can_error(self) -> bool {
        matches!(self.overflow, Some(Overflow::Error))
            || matches!(self.div_zero, Some(DivZero::Error))
            || matches!(self.shift, Some(Shift::Error))
            || matches!(self.float_to_int, Some(FloatToInt::Error))
    }

    /// The fields a unary operation on an operand of type `ty` must carry.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Policy, Type, UnaryOp};
    ///
    /// assert!(Policy::required_unary(UnaryOp::Neg, Type::I32).overflow);
    /// assert!(Policy::required_unary(UnaryOp::Neg, Type::F32).is_empty());
    /// ```
    #[must_use]
    pub const fn required_unary(op: UnaryOp, ty: Type) -> PolicyFields {
        let int = ty.is_int();
        PolicyFields {
            overflow: int && matches!(op, UnaryOp::Neg | UnaryOp::Abs),
            div_zero: false,
            shift: false,
            float_to_int: false,
        }
    }

    /// The fields a binary operation on operands of type `ty` must carry.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{BinaryOp, Policy, Type};
    ///
    /// assert!(Policy::required_binary(BinaryOp::Shl, Type::U8).shift);
    /// assert!(Policy::required_binary(BinaryOp::Rem, Type::I8).div_zero);
    /// ```
    #[must_use]
    pub const fn required_binary(op: BinaryOp, ty: Type) -> PolicyFields {
        let int = ty.is_int();
        PolicyFields {
            overflow: int
                && matches!(
                    op,
                    BinaryOp::Add
                        | BinaryOp::Sub
                        | BinaryOp::Mul
                        | BinaryOp::Div
                        | BinaryOp::FloorDiv
                ),
            div_zero: int
                && matches!(
                    op,
                    BinaryOp::Div | BinaryOp::Rem | BinaryOp::FloorDiv | BinaryOp::FloorMod
                ),
            shift: int && matches!(op, BinaryOp::Shl | BinaryOp::Shr),
            float_to_int: false,
        }
    }

    /// The fields a conversion must carry.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{ConvOp, Policy};
    ///
    /// assert!(Policy::required_conversion(ConvOp::IntCast).overflow);
    /// assert!(Policy::required_conversion(ConvOp::FloatToInt).float_to_int);
    /// assert!(Policy::required_conversion(ConvOp::Zext).is_empty());
    /// ```
    #[must_use]
    pub const fn required_conversion(op: ConvOp) -> PolicyFields {
        PolicyFields {
            overflow: matches!(op, ConvOp::IntCast),
            div_zero: false,
            shift: false,
            float_to_int: matches!(op, ConvOp::FloatToInt),
        }
    }
}

impl fmt::Display for Policy {
    /// Prints `<field=value,...>`, or nothing when no field is set.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Policy::NONE {
            return Ok(());
        }
        let mut sep = "<";
        if let Some(o) = self.overflow {
            write!(f, "{sep}overflow={}", o.name())?;
            sep = ",";
        }
        if let Some(d) = self.div_zero {
            write!(f, "{sep}div_zero={}", d.name())?;
            sep = ",";
        }
        if let Some(s) = self.shift {
            write!(f, "{sep}shift={}", s.name())?;
            sep = ",";
        }
        if let Some(p) = self.float_to_int {
            write!(f, "{sep}float_to_int={}", p.name())?;
        }
        f.write_str(">")
    }
}

/// An operation error kind, with the stable code from OPS §6.
///
/// A failing `check` terminator passes the code (as a `u32`) to its error edge; a
/// trap reports the same code.
///
/// # Examples
///
/// ```
/// use ir_lang::OpError;
///
/// assert_eq!(OpError::DivByZero.code(), 2);
/// assert_eq!(OpError::from_code(5), Some(OpError::InvalidChar));
/// assert_eq!(OpError::ArithOverflow.to_string(), "E0001 ArithOverflow");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum OpError {
    /// `E0001`: an integer result, `MIN / -1`, or `int_cast` did not fit.
    ArithOverflow,
    /// `E0002`: an integer division or remainder by zero.
    DivByZero,
    /// `E0003`: a shift amount at least the bit width, or negative.
    ShiftOutOfRange,
    /// `E0004`: `float_to_int` of NaN or an out-of-range value.
    InvalidConversion,
    /// `E0005`: `char_from_u32` of a value that is not a Unicode scalar value.
    InvalidChar,
}

impl OpError {
    /// The stable numeric code (`1` for `E0001`, ...).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::OpError;
    ///
    /// assert_eq!(OpError::ShiftOutOfRange.code(), 3);
    /// ```
    #[must_use]
    pub const fn code(self) -> u32 {
        match self {
            OpError::ArithOverflow => 1,
            OpError::DivByZero => 2,
            OpError::ShiftOutOfRange => 3,
            OpError::InvalidConversion => 4,
            OpError::InvalidChar => 5,
        }
    }

    /// The kind with the given code, if it is one of OPS §6.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::OpError;
    ///
    /// assert_eq!(OpError::from_code(4), Some(OpError::InvalidConversion));
    /// assert_eq!(OpError::from_code(0), None);
    /// ```
    #[must_use]
    pub const fn from_code(code: u32) -> Option<OpError> {
        Some(match code {
            1 => OpError::ArithOverflow,
            2 => OpError::DivByZero,
            3 => OpError::ShiftOutOfRange,
            4 => OpError::InvalidConversion,
            5 => OpError::InvalidChar,
            _ => return None,
        })
    }

    const fn kind_name(self) -> &'static str {
        match self {
            OpError::ArithOverflow => "ArithOverflow",
            OpError::DivByZero => "DivByZero",
            OpError::ShiftOutOfRange => "ShiftOutOfRange",
            OpError::InvalidConversion => "InvalidConversion",
            OpError::InvalidChar => "InvalidChar",
        }
    }
}

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{:04} {}", self.code(), self.kind_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn test_op_names_round_trip() {
        for &op in UnaryOp::ALL {
            assert_eq!(UnaryOp::from_name(op.name()), Some(op));
        }
        for &op in BinaryOp::ALL {
            assert_eq!(BinaryOp::from_name(op.name()), Some(op));
        }
        for &op in CmpOp::ALL {
            assert_eq!(CmpOp::from_name(op.name()), Some(op));
        }
        for &op in ConvOp::ALL {
            assert_eq!(ConvOp::from_name(op.name()), Some(op));
        }
    }

    #[test]
    fn test_policy_display_orders_fields() {
        let p = Policy {
            overflow: Some(Overflow::Error),
            div_zero: Some(DivZero::Error),
            shift: None,
            float_to_int: None,
        };
        assert_eq!(p.to_string(), "<overflow=error,div_zero=error>");
        assert_eq!(
            Policy::float_to_int(FloatToInt::Error).to_string(),
            "<float_to_int=error>"
        );
    }

    #[test]
    fn test_required_fields_follow_ops_table() {
        // OPS §2 / HIR §12: add sub mul neg abs int_cast → overflow; div floor_div →
        // overflow + div_zero; rem floor_mod → div_zero; shl shr → shift.
        for op in [BinaryOp::Add, BinaryOp::Sub, BinaryOp::Mul] {
            let f = Policy::required_binary(op, Type::I32);
            assert!(f.overflow && !f.div_zero && !f.shift);
        }
        for op in [BinaryOp::Div, BinaryOp::FloorDiv] {
            let f = Policy::required_binary(op, Type::U16);
            assert!(f.overflow && f.div_zero);
        }
        for op in [BinaryOp::Rem, BinaryOp::FloorMod] {
            let f = Policy::required_binary(op, Type::I64);
            assert!(!f.overflow && f.div_zero);
        }
        for op in [BinaryOp::And, BinaryOp::Min, BinaryOp::TotalCmp] {
            assert!(Policy::required_binary(op, Type::I64).is_empty());
        }
        assert!(Policy::required_binary(BinaryOp::Add, Type::F64).is_empty());
        assert_eq!(PolicyFields::default().to_string(), "{}");
    }

    #[test]
    fn test_error_codes_round_trip() {
        for code in 1..=5 {
            let e = OpError::from_code(code);
            assert_eq!(e.map(OpError::code), Some(code));
        }
    }
}
