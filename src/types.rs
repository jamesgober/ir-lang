//! Value types, memory types, aggregate layout, and signatures.

use alloc::vec::Vec;
use core::fmt;

use crate::entity::AggId;

/// The type of an SSA value.
///
/// Integers are sized and carry their signedness, mirroring the value domains of the
/// operation semantics (`specs/OPS.md` §1): every signedness-dependent operation
/// (overflow detection, division, right shift, ordering, conversions) takes its
/// signedness from its operand type, so an instruction cannot disagree with the
/// domain it computes in. [`Ptr`](Type::Ptr) is an untraced address; [`Ref`](Type::Ref)
/// is a garbage-collected reference that a moving collector may relocate at a
/// safepoint (see the safepoint rules in `specs/IR.md` §9).
///
/// Aggregates are memory types only ([`MemType`]): they live in stack slots and
/// globals and are reached through addresses, never held in an SSA value.
/// Vector types are reserved; the enum is `#[non_exhaustive]` so they can be added
/// in a minor release.
///
/// # Examples
///
/// ```
/// use ir_lang::{PointerWidth, Type};
///
/// assert!(Type::I32.is_signed_int());
/// assert!(Type::U8.is_unsigned_int());
/// assert_eq!(Type::I16.int_bits(), Some(16));
/// assert_eq!(Type::Ptr.size(PointerWidth::P64), 8);
/// assert_eq!(Type::F32.to_string(), "f32");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum Type {
    /// A boolean: `false` or `true` (one byte in memory, `0` or `1`).
    Bool,
    /// A signed 8-bit integer.
    I8,
    /// A signed 16-bit integer.
    I16,
    /// A signed 32-bit integer.
    I32,
    /// A signed 64-bit integer.
    I64,
    /// An unsigned 8-bit integer.
    U8,
    /// An unsigned 16-bit integer.
    U16,
    /// An unsigned 32-bit integer.
    U32,
    /// An unsigned 64-bit integer.
    U64,
    /// An IEEE 754 binary32 float.
    F32,
    /// An IEEE 754 binary64 float.
    F64,
    /// An untraced address, as wide as the module's [`PointerWidth`].
    Ptr,
    /// A garbage-collected reference, as wide as the module's [`PointerWidth`].
    Ref,
}

impl Type {
    /// Every value type, in a fixed order (useful for generators and tables).
    pub const ALL: [Type; 13] = [
        Type::Bool,
        Type::I8,
        Type::I16,
        Type::I32,
        Type::I64,
        Type::U8,
        Type::U16,
        Type::U32,
        Type::U64,
        Type::F32,
        Type::F64,
        Type::Ptr,
        Type::Ref,
    ];

    /// Whether this is an integer type (signed or unsigned).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Type;
    ///
    /// assert!(Type::U64.is_int());
    /// assert!(!Type::Bool.is_int());
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_int(self) -> bool {
        self.int_bits().is_some()
    }

    /// Whether this is a signed integer type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Type;
    ///
    /// assert!(Type::I8.is_signed_int());
    /// assert!(!Type::U8.is_signed_int());
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_signed_int(self) -> bool {
        matches!(self, Type::I8 | Type::I16 | Type::I32 | Type::I64)
    }

    /// Whether this is an unsigned integer type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Type;
    ///
    /// assert!(Type::U16.is_unsigned_int());
    /// assert!(!Type::I16.is_unsigned_int());
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_unsigned_int(self) -> bool {
        matches!(self, Type::U8 | Type::U16 | Type::U32 | Type::U64)
    }

    /// Whether this is a float type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Type;
    ///
    /// assert!(Type::F64.is_float());
    /// assert!(!Type::I64.is_float());
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self, Type::F32 | Type::F64)
    }

    /// Whether this is an integer or float type.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Type;
    ///
    /// assert!(Type::F32.is_numeric());
    /// assert!(!Type::Ptr.is_numeric());
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_numeric(self) -> bool {
        self.is_int() || self.is_float()
    }

    /// The width of an integer type in bits, or `None` for other types.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Type;
    ///
    /// assert_eq!(Type::U32.int_bits(), Some(32));
    /// assert_eq!(Type::F32.int_bits(), None);
    /// ```
    #[inline]
    #[must_use]
    pub const fn int_bits(self) -> Option<u32> {
        match self {
            Type::I8 | Type::U8 => Some(8),
            Type::I16 | Type::U16 => Some(16),
            Type::I32 | Type::U32 => Some(32),
            Type::I64 | Type::U64 => Some(64),
            _ => None,
        }
    }

    /// The width of the type's value in bits, for the given pointer width
    /// (`Bool` is 1 bit).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{PointerWidth, Type};
    ///
    /// assert_eq!(Type::Bool.bits(PointerWidth::P64), 1);
    /// assert_eq!(Type::Ref.bits(PointerWidth::P32), 32);
    /// ```
    #[inline]
    #[must_use]
    pub const fn bits(self, ptr: PointerWidth) -> u32 {
        match self {
            Type::Bool => 1,
            Type::F32 => 32,
            Type::F64 => 64,
            Type::Ptr | Type::Ref => ptr.bits(),
            other => match other.int_bits() {
                Some(b) => b,
                None => 0,
            },
        }
    }

    /// The size of the type in memory, in bytes (also its natural alignment).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{PointerWidth, Type};
    ///
    /// assert_eq!(Type::Bool.size(PointerWidth::P64), 1);
    /// assert_eq!(Type::I16.size(PointerWidth::P64), 2);
    /// assert_eq!(Type::Ptr.size(PointerWidth::P32), 4);
    /// ```
    #[inline]
    #[must_use]
    pub const fn size(self, ptr: PointerWidth) -> u32 {
        match self {
            Type::Bool => 1,
            other => other.bits(ptr) / 8,
        }
    }

    /// The integer type of the given width and signedness, if there is one.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Type;
    ///
    /// assert_eq!(Type::int(32, true), Some(Type::I32));
    /// assert_eq!(Type::int(12, false), None);
    /// ```
    #[must_use]
    pub const fn int(bits: u32, signed: bool) -> Option<Type> {
        Some(match (bits, signed) {
            (8, true) => Type::I8,
            (16, true) => Type::I16,
            (32, true) => Type::I32,
            (64, true) => Type::I64,
            (8, false) => Type::U8,
            (16, false) => Type::U16,
            (32, false) => Type::U32,
            (64, false) => Type::U64,
            _ => return None,
        })
    }

    /// The textual name of the type (`i32`, `ptr`, ...).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Type;
    ///
    /// assert_eq!(Type::Ref.name(), "ref");
    /// ```
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Type::Bool => "bool",
            Type::I8 => "i8",
            Type::I16 => "i16",
            Type::I32 => "i32",
            Type::I64 => "i64",
            Type::U8 => "u8",
            Type::U16 => "u16",
            Type::U32 => "u32",
            Type::U64 => "u64",
            Type::F32 => "f32",
            Type::F64 => "f64",
            Type::Ptr => "ptr",
            Type::Ref => "ref",
        }
    }

    /// Looks a type up by its textual name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Type;
    ///
    /// assert_eq!(Type::from_name("u16"), Some(Type::U16));
    /// assert_eq!(Type::from_name("int"), None);
    /// ```
    #[must_use]
    pub fn from_name(name: &str) -> Option<Type> {
        Some(match name {
            "bool" => Type::Bool,
            "i8" => Type::I8,
            "i16" => Type::I16,
            "i32" => Type::I32,
            "i64" => Type::I64,
            "u8" => Type::U8,
            "u16" => Type::U16,
            "u32" => Type::U32,
            "u64" => Type::U64,
            "f32" => Type::F32,
            "f64" => Type::F64,
            "ptr" => Type::Ptr,
            "ref" => Type::Ref,
            _ => return None,
        })
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The width of addresses (`ptr` and `ref`) in a module.
///
/// # Examples
///
/// ```
/// use ir_lang::PointerWidth;
///
/// assert_eq!(PointerWidth::P64.bytes(), 8);
/// assert_eq!(PointerWidth::P32.bits(), 32);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[non_exhaustive]
pub enum PointerWidth {
    /// 32-bit addresses (wasm32, 32-bit targets).
    P32,
    /// 64-bit addresses (x86-64, AArch64, riscv64). The default.
    #[default]
    P64,
}

impl PointerWidth {
    /// The address width in bytes.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::PointerWidth;
    ///
    /// assert_eq!(PointerWidth::P32.bytes(), 4);
    /// ```
    #[inline]
    #[must_use]
    pub const fn bytes(self) -> u32 {
        match self {
            PointerWidth::P32 => 4,
            PointerWidth::P64 => 8,
        }
    }

    /// The address width in bits.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::PointerWidth;
    ///
    /// assert_eq!(PointerWidth::P64.bits(), 64);
    /// ```
    #[inline]
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.bytes() * 8
    }

    /// Whether `ty` is an integer exactly as wide as an address (the integer side
    /// of `ptr_to_int` / `int_to_ptr`).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{PointerWidth, Type};
    ///
    /// assert!(PointerWidth::P64.is_address_int(Type::U64));
    /// assert!(!PointerWidth::P64.is_address_int(Type::U32));
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_address_int(self, ty: Type) -> bool {
        match ty.int_bits() {
            Some(bits) => bits == self.bits(),
            None => false,
        }
    }
}

/// A type as laid out in memory: a scalar value type or an aggregate.
///
/// # Examples
///
/// ```
/// use ir_lang::{AggId, MemType, Type};
///
/// let m: MemType = Type::I32.into();
/// assert_eq!(m, MemType::Scalar(Type::I32));
/// assert_eq!(MemType::from(AggId::from_u32(0)).to_string(), "t0");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum MemType {
    /// A scalar of the given value type.
    Scalar(Type),
    /// An aggregate declared in the module.
    Agg(AggId),
}

impl From<Type> for MemType {
    fn from(ty: Type) -> Self {
        MemType::Scalar(ty)
    }
}

impl From<AggId> for MemType {
    fn from(agg: AggId) -> Self {
        MemType::Agg(agg)
    }
}

impl fmt::Display for MemType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemType::Scalar(ty) => write!(f, "{ty}"),
            MemType::Agg(agg) => write!(f, "{agg}"),
        }
    }
}

/// An aggregate memory type: a struct or a fixed-length array.
///
/// An aggregate may name only aggregates declared before it, so layouts are computed
/// in declaration order and a recursive aggregate is impossible (recursion goes
/// through `ptr`, which is opaque).
///
/// # Examples
///
/// ```
/// use ir_lang::{Aggregate, MemType, Module, Type};
///
/// let mut m = Module::new("demo");
/// let pair = m.declare_aggregate(Aggregate::structure(&[Type::U8.into(), Type::I64.into()]))?;
/// let layout = m.layout(pair.into()).unwrap();
/// assert_eq!((layout.size(), layout.align()), (16, 8));
/// assert_eq!(m.field_offset(pair, 1), Some(8));
/// # Ok::<(), ir_lang::ModuleError>(())
/// ```
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Aggregate {
    /// Fields laid out in order. Unless `packed`, each field is placed at the next
    /// multiple of its alignment and the size is rounded up to the struct's
    /// alignment (the C rule); a packed struct has alignment 1 and no padding.
    Struct {
        /// The field types, in order.
        fields: Vec<MemType>,
        /// Whether the struct is packed.
        packed: bool,
    },
    /// `len` elements of `elem`, each at a stride of the element's size.
    Array {
        /// The element type.
        elem: MemType,
        /// The element count.
        len: u64,
    },
}

impl Aggregate {
    /// A (non-packed) struct with the given fields.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Aggregate, Type};
    ///
    /// let s = Aggregate::structure(&[Type::I32.into()]);
    /// assert!(matches!(s, Aggregate::Struct { packed: false, .. }));
    /// ```
    #[must_use]
    pub fn structure(fields: &[MemType]) -> Aggregate {
        Aggregate::Struct {
            fields: fields.to_vec(),
            packed: false,
        }
    }

    /// A packed struct (alignment 1, no padding) with the given fields.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Aggregate, Type};
    ///
    /// let s = Aggregate::packed(&[Type::U8.into(), Type::U32.into()]);
    /// assert!(matches!(s, Aggregate::Struct { packed: true, .. }));
    /// ```
    #[must_use]
    pub fn packed(fields: &[MemType]) -> Aggregate {
        Aggregate::Struct {
            fields: fields.to_vec(),
            packed: true,
        }
    }

    /// An array of `len` elements.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Aggregate, Type};
    ///
    /// let a = Aggregate::array(Type::F64.into(), 4);
    /// assert!(matches!(a, Aggregate::Array { len: 4, .. }));
    /// ```
    #[must_use]
    pub fn array(elem: MemType, len: u64) -> Aggregate {
        Aggregate::Array { elem, len }
    }
}

/// The size and alignment of a memory type, in bytes.
///
/// # Examples
///
/// ```
/// use ir_lang::{Module, Type};
///
/// let m = Module::new("demo");
/// let l = m.layout(Type::I32.into()).unwrap();
/// assert_eq!((l.size(), l.align()), (4, 4));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Layout {
    pub(crate) size: u64,
    pub(crate) align: u32,
}

impl Layout {
    /// The size in bytes (a multiple of the alignment).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Module, Type};
    ///
    /// assert_eq!(Module::new("m").layout(Type::U16.into()).unwrap().size(), 2);
    /// ```
    #[inline]
    #[must_use]
    pub const fn size(self) -> u64 {
        self.size
    }

    /// The alignment in bytes (a power of two).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Module, Type};
    ///
    /// assert_eq!(Module::new("m").layout(Type::F64.into()).unwrap().align(), 8);
    /// ```
    #[inline]
    #[must_use]
    pub const fn align(self) -> u32 {
        self.align
    }
}

/// The largest size, in bytes, an aggregate may have (2^48).
pub const MAX_AGGREGATE_SIZE: u64 = 1 << 48;

/// A calling convention tag on a signature.
///
/// The IR records the convention; the backend implements it. Tail calls require the
/// caller's and the callee's conventions to be equal.
///
/// # Examples
///
/// ```
/// use ir_lang::CallConv;
///
/// assert_eq!(CallConv::default(), CallConv::Fast);
/// assert_eq!(CallConv::C.name(), "c");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
#[non_exhaustive]
pub enum CallConv {
    /// The internal convention for code compiled together: the backend may choose
    /// anything, and tail calls are guaranteed. The default.
    #[default]
    Fast,
    /// The target platform's C convention (resolved by the backend per target).
    C,
    /// The System V AMD64 convention.
    SysV64,
    /// The Windows x64 convention.
    Win64,
    /// The AArch64 procedure-call standard.
    Aapcs64,
}

impl CallConv {
    /// Every convention, in a fixed order.
    pub const ALL: [CallConv; 5] = [
        CallConv::Fast,
        CallConv::C,
        CallConv::SysV64,
        CallConv::Win64,
        CallConv::Aapcs64,
    ];

    /// The textual name of the convention.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::CallConv;
    ///
    /// assert_eq!(CallConv::Win64.name(), "win64");
    /// ```
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            CallConv::Fast => "fast",
            CallConv::C => "c",
            CallConv::SysV64 => "sysv64",
            CallConv::Win64 => "win64",
            CallConv::Aapcs64 => "aapcs64",
        }
    }

    /// Looks a convention up by its textual name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::CallConv;
    ///
    /// assert_eq!(CallConv::from_name("sysv64"), Some(CallConv::SysV64));
    /// ```
    #[must_use]
    pub fn from_name(name: &str) -> Option<CallConv> {
        CallConv::ALL.iter().copied().find(|c| c.name() == name)
    }
}

impl fmt::Display for CallConv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A function signature: parameter types, return types, and a calling convention.
///
/// A function may return any number of values (zero for "returns nothing").
/// Signatures are interned per module ([`Module::intern_signature`](crate::Module::intern_signature)),
/// so two equal signatures share one [`SigId`](crate::SigId).
///
/// # Examples
///
/// ```
/// use ir_lang::{CallConv, Signature, Type};
///
/// let sig = Signature::new(&[Type::I64, Type::Ptr], &[Type::Bool]).with_conv(CallConv::C);
/// assert_eq!(sig.params, [Type::I64, Type::Ptr]);
/// assert_eq!(sig.to_string(), "(i64, ptr) -> (bool) c");
/// ```
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Signature {
    /// Parameter types, in order.
    pub params: Vec<Type>,
    /// Return types, in order.
    pub returns: Vec<Type>,
    /// The calling convention.
    pub conv: CallConv,
}

impl Signature {
    /// A signature with the [`Fast`](CallConv::Fast) convention.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Signature, Type};
    ///
    /// let s = Signature::new(&[], &[Type::I32]);
    /// assert!(s.params.is_empty());
    /// ```
    #[must_use]
    pub fn new(params: &[Type], returns: &[Type]) -> Signature {
        Signature {
            params: params.to_vec(),
            returns: returns.to_vec(),
            conv: CallConv::Fast,
        }
    }

    /// The same signature with a different calling convention.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{CallConv, Signature};
    ///
    /// assert_eq!(Signature::new(&[], &[]).with_conv(CallConv::Win64).conv, CallConv::Win64);
    /// ```
    #[must_use]
    pub fn with_conv(mut self, conv: CallConv) -> Signature {
        self.conv = conv;
        self
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(")?;
        for (i, ty) in self.params.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{ty}")?;
        }
        f.write_str(") -> (")?;
        for (i, ty) in self.returns.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{ty}")?;
        }
        write!(f, ") {}", self.conv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn test_type_names_round_trip() {
        for ty in Type::ALL {
            assert_eq!(Type::from_name(ty.name()), Some(ty));
        }
        for conv in CallConv::ALL {
            assert_eq!(CallConv::from_name(conv.name()), Some(conv));
        }
    }

    #[test]
    fn test_type_sizes_follow_pointer_width() {
        assert_eq!(Type::Ptr.size(PointerWidth::P32), 4);
        assert_eq!(Type::Ref.size(PointerWidth::P64), 8);
        assert_eq!(Type::U64.size(PointerWidth::P32), 8);
        assert_eq!(Type::Bool.size(PointerWidth::P32), 1);
    }

    #[test]
    fn test_signature_display() {
        let s = Signature::new(&[], &[]);
        assert_eq!(s.to_string(), "() -> () fast");
    }
}
