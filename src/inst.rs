//! Instructions, terminators, and the pieces they are built from.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use crate::entity::{AggId, Block, FuncId, GlobalId, SigId, StackSlot, Value};
use crate::ops::{BinaryOp, CmpOp, ConvOp, Policy, UnaryOp};
use crate::types::{MemType, Type};

/// Alignment and volatility of a memory access.
///
/// `align` is the alignment the access may assume, in bytes (a power of two, at most
/// 2^29). An access whose address is not a multiple of it is undefined behaviour
/// (the reference interpreter reports it). A volatile access is never removed,
/// duplicated, merged, or reordered with other volatile accesses.
///
/// # Examples
///
/// ```
/// use ir_lang::MemFlags;
///
/// let f = MemFlags::new(8).volatile();
/// assert_eq!((f.align, f.volatile), (8, true));
/// assert_eq!(MemFlags::new(4).to_string(), "align 4");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct MemFlags {
    /// Assumed alignment in bytes (power of two).
    pub align: u32,
    /// Whether the access is volatile.
    pub volatile: bool,
}

impl MemFlags {
    /// The largest alignment an access may claim.
    pub const MAX_ALIGN: u32 = 1 << 29;

    /// A non-volatile access with the given alignment.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::MemFlags;
    ///
    /// assert!(!MemFlags::new(1).volatile);
    /// ```
    #[must_use]
    pub const fn new(align: u32) -> MemFlags {
        MemFlags {
            align,
            volatile: false,
        }
    }

    /// The same flags, volatile.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::MemFlags;
    ///
    /// assert!(MemFlags::new(1).volatile().volatile);
    /// ```
    #[must_use]
    pub const fn volatile(mut self) -> MemFlags {
        self.volatile = true;
        self
    }
}

impl fmt::Display for MemFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "align {}", self.align)?;
        if self.volatile {
            f.write_str(" volatile")?;
        }
        Ok(())
    }
}

/// An atomic memory ordering (C++20), mirroring HIR v2's `Intrinsic` orderings.
///
/// Loads are not `Release`/`AcqRel`, stores are not `Acquire`/`AcqRel`, fences are
/// not `Relaxed`, and a compare-exchange failure ordering is not `Release`/`AcqRel`
/// and not stronger than its success ordering.
///
/// # Examples
///
/// ```
/// use ir_lang::MemOrder;
///
/// assert_eq!(MemOrder::AcqRel.name(), "acq_rel");
/// assert!(MemOrder::SeqCst.strength() > MemOrder::Acquire.strength());
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum MemOrder {
    /// No ordering beyond atomicity.
    Relaxed,
    /// Acquire.
    Acquire,
    /// Release.
    Release,
    /// Acquire and release.
    AcqRel,
    /// Sequentially consistent.
    SeqCst,
}

impl MemOrder {
    /// Every ordering, in a fixed order.
    pub const ALL: [MemOrder; 5] = [
        MemOrder::Relaxed,
        MemOrder::Acquire,
        MemOrder::Release,
        MemOrder::AcqRel,
        MemOrder::SeqCst,
    ];

    /// The textual name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::MemOrder;
    ///
    /// assert_eq!(MemOrder::SeqCst.name(), "seq_cst");
    /// ```
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            MemOrder::Relaxed => "relaxed",
            MemOrder::Acquire => "acquire",
            MemOrder::Release => "release",
            MemOrder::AcqRel => "acq_rel",
            MemOrder::SeqCst => "seq_cst",
        }
    }

    /// Looks an ordering up by its textual name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::MemOrder;
    ///
    /// assert_eq!(MemOrder::from_name("release"), Some(MemOrder::Release));
    /// ```
    #[must_use]
    pub fn from_name(name: &str) -> Option<MemOrder> {
        MemOrder::ALL.iter().copied().find(|o| o.name() == name)
    }

    /// The strength used for the "failure not stronger than success" rule:
    /// relaxed 0, acquire and release 1, acq_rel 2, seq_cst 3.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::MemOrder;
    ///
    /// assert_eq!(MemOrder::Relaxed.strength(), 0);
    /// ```
    #[must_use]
    pub const fn strength(self) -> u8 {
        match self {
            MemOrder::Relaxed => 0,
            MemOrder::Acquire | MemOrder::Release => 1,
            MemOrder::AcqRel => 2,
            MemOrder::SeqCst => 3,
        }
    }
}

impl fmt::Display for MemOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The operation of an atomic read-modify-write. Signedness (for `min`/`max`)
/// comes from the value type; `add`/`sub` wrap.
///
/// # Examples
///
/// ```
/// use ir_lang::RmwOp;
///
/// assert_eq!(RmwOp::from_name("xchg"), Some(RmwOp::Xchg));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[non_exhaustive]
pub enum RmwOp {
    /// Store the operand; return the old value.
    Xchg,
    /// Wrapping add.
    Add,
    /// Wrapping subtract.
    Sub,
    /// Bitwise and.
    And,
    /// Bitwise or.
    Or,
    /// Bitwise xor.
    Xor,
    /// Bitwise not-and: `!(old & operand)`.
    Nand,
    /// Minimum (signedness from the type).
    Min,
    /// Maximum (signedness from the type).
    Max,
}

impl RmwOp {
    /// Every operation, in a fixed order.
    pub const ALL: [RmwOp; 9] = [
        RmwOp::Xchg,
        RmwOp::Add,
        RmwOp::Sub,
        RmwOp::And,
        RmwOp::Or,
        RmwOp::Xor,
        RmwOp::Nand,
        RmwOp::Min,
        RmwOp::Max,
    ];

    /// The textual name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::RmwOp;
    ///
    /// assert_eq!(RmwOp::Nand.name(), "nand");
    /// ```
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            RmwOp::Xchg => "xchg",
            RmwOp::Add => "add",
            RmwOp::Sub => "sub",
            RmwOp::And => "and",
            RmwOp::Or => "or",
            RmwOp::Xor => "xor",
            RmwOp::Nand => "nand",
            RmwOp::Min => "min",
            RmwOp::Max => "max",
        }
    }

    /// Looks an operation up by its textual name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::RmwOp;
    ///
    /// assert_eq!(RmwOp::from_name("max"), Some(RmwOp::Max));
    /// ```
    #[must_use]
    pub fn from_name(name: &str) -> Option<RmwOp> {
        RmwOp::ALL.iter().copied().find(|o| o.name() == name)
    }
}

/// Attributes of a function declaration or a call site, as a bit set.
///
/// `NOUNWIND`, `NORETURN`, and `NOGC` are checked by the validator (see the
/// `specs/IR.md` rules); the others are optimization hints.
///
/// # Examples
///
/// ```
/// use ir_lang::Attrs;
///
/// let a = Attrs::NOUNWIND | Attrs::COLD;
/// assert!(a.contains(Attrs::NOUNWIND));
/// assert!(!a.contains(Attrs::NOGC));
/// assert_eq!(a.to_string(), "[nounwind, cold]");
/// assert_eq!(Attrs::NONE.to_string(), "");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Attrs(u16);

impl Attrs {
    /// No attributes.
    pub const NONE: Attrs = Attrs(0);
    /// Never unwinds. A `nounwind` function contains no `resume`, and every call in
    /// it is `nounwind` (at the site or by the callee's declaration) or an `invoke`.
    pub const NOUNWIND: Attrs = Attrs(1);
    /// Never returns normally. A `noreturn` function contains no `ret`.
    pub const NORETURN: Attrs = Attrs(1 << 1);
    /// Never collects garbage: a call to it is not a safepoint. A `nogc` function
    /// contains no safepoint.
    pub const NOGC: Attrs = Attrs(1 << 2);
    /// Reads but never writes memory visible to the caller (hint).
    pub const READONLY: Attrs = Attrs(1 << 3);
    /// Neither reads nor writes memory visible to the caller (hint).
    pub const READNONE: Attrs = Attrs(1 << 4);
    /// Rarely executed (hint).
    pub const COLD: Attrs = Attrs(1 << 5);
    /// Do not inline (hint).
    pub const NOINLINE: Attrs = Attrs(1 << 6);
    /// Inline whenever possible (hint).
    pub const INLINE: Attrs = Attrs(1 << 7);

    const NAMES: [(Attrs, &'static str); 8] = [
        (Attrs::NOUNWIND, "nounwind"),
        (Attrs::NORETURN, "noreturn"),
        (Attrs::NOGC, "nogc"),
        (Attrs::READONLY, "readonly"),
        (Attrs::READNONE, "readnone"),
        (Attrs::COLD, "cold"),
        (Attrs::NOINLINE, "noinline"),
        (Attrs::INLINE, "inline"),
    ];

    /// Whether every attribute of `other` is set.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Attrs;
    ///
    /// assert!(Attrs::COLD.contains(Attrs::NONE));
    /// ```
    #[must_use]
    pub const fn contains(self, other: Attrs) -> bool {
        self.0 & other.0 == other.0
    }

    /// The union of two sets.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Attrs;
    ///
    /// assert_eq!(Attrs::COLD.union(Attrs::INLINE), Attrs::COLD | Attrs::INLINE);
    /// ```
    #[must_use]
    pub const fn union(self, other: Attrs) -> Attrs {
        Attrs(self.0 | other.0)
    }

    /// Whether no attribute is set.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Attrs;
    ///
    /// assert!(Attrs::NONE.is_empty());
    /// ```
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Looks one attribute up by its textual name.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Attrs;
    ///
    /// assert_eq!(Attrs::from_name("nogc"), Some(Attrs::NOGC));
    /// ```
    #[must_use]
    pub fn from_name(name: &str) -> Option<Attrs> {
        Attrs::NAMES
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(a, _)| *a)
    }

    /// The raw bits (for side tables).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Attrs;
    ///
    /// assert_eq!(Attrs::NOUNWIND.bits(), 1);
    /// ```
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }
}

impl core::ops::BitOr for Attrs {
    type Output = Attrs;

    fn bitor(self, rhs: Attrs) -> Attrs {
        self.union(rhs)
    }
}

impl core::ops::BitOrAssign for Attrs {
    fn bitor_assign(&mut self, rhs: Attrs) {
        *self = self.union(rhs);
    }
}

impl fmt::Display for Attrs {
    /// Prints `[name, ...]`, or nothing for the empty set.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return Ok(());
        }
        let mut sep = "[";
        for (attr, name) in Attrs::NAMES {
            if self.contains(attr) {
                write!(f, "{sep}{name}")?;
                sep = ", ";
            }
        }
        f.write_str("]")
    }
}

/// What a call calls.
///
/// # Examples
///
/// ```
/// use ir_lang::{CallTarget, FuncId};
///
/// let t = CallTarget::Direct(FuncId::from_u32(0));
/// assert!(matches!(t, CallTarget::Direct(_)));
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum CallTarget {
    /// A function declared in the module (defined or imported).
    Direct(FuncId),
    /// The function whose address is `callee` (a `ptr`), which must have signature
    /// `sig`.
    Indirect {
        /// The callee's signature.
        sig: SigId,
        /// The callee's address.
        callee: Value,
    },
}

/// A call: target, arguments, the GC references it relocates, and call-site
/// attributes.
///
/// A call is a **safepoint** unless it is `nogc` (at the site or by the callee's
/// declaration). Every `ref` value (and every pointer derived from one) that is
/// needed after a safepoint must be listed in `gc`; the call then defines one
/// relocated `ref` per entry, after its return values. Using the old value after
/// the safepoint is invalid. A `nogc` call must have an empty `gc` list.
///
/// # Examples
///
/// ```
/// use ir_lang::{Attrs, Call, FuncId, Value};
///
/// let c = Call::direct(FuncId::from_u32(1), &[Value::from_u32(0)]).with_attrs(Attrs::NOUNWIND);
/// assert_eq!(c.args.len(), 1);
/// assert!(c.gc.is_empty());
/// ```
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Call {
    /// What is called.
    pub target: CallTarget,
    /// The arguments, matching the callee's parameters.
    pub args: Vec<Value>,
    /// The `ref` values relocated by this safepoint.
    pub gc: Vec<Value>,
    /// Call-site attributes.
    pub attrs: Attrs,
}

impl Call {
    /// A direct call with no GC list and no attributes.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Call, FuncId};
    ///
    /// assert!(Call::direct(FuncId::from_u32(0), &[]).args.is_empty());
    /// ```
    #[must_use]
    pub fn direct(func: FuncId, args: &[Value]) -> Call {
        Call {
            target: CallTarget::Direct(func),
            args: args.to_vec(),
            gc: Vec::new(),
            attrs: Attrs::NONE,
        }
    }

    /// An indirect call through `callee` with signature `sig`.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Call, SigId, Value};
    ///
    /// let c = Call::indirect(SigId::from_u32(0), Value::from_u32(2), &[]);
    /// assert!(c.args.is_empty());
    /// ```
    #[must_use]
    pub fn indirect(sig: SigId, callee: Value, args: &[Value]) -> Call {
        Call {
            target: CallTarget::Indirect { sig, callee },
            args: args.to_vec(),
            gc: Vec::new(),
            attrs: Attrs::NONE,
        }
    }

    /// The same call relocating the given `ref` values.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Call, FuncId, Value};
    ///
    /// let c = Call::direct(FuncId::from_u32(0), &[]).with_gc(&[Value::from_u32(3)]);
    /// assert_eq!(c.gc.len(), 1);
    /// ```
    #[must_use]
    pub fn with_gc(mut self, gc: &[Value]) -> Call {
        self.gc = gc.to_vec();
        self
    }

    /// The same call with the given call-site attributes.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Attrs, Call, FuncId};
    ///
    /// let c = Call::direct(FuncId::from_u32(0), &[]).with_attrs(Attrs::COLD);
    /// assert!(c.attrs.contains(Attrs::COLD));
    /// ```
    #[must_use]
    pub fn with_attrs(mut self, attrs: Attrs) -> Call {
        self.attrs = attrs;
        self
    }

    /// Calls `f` on every value the call reads (callee, arguments, GC list).
    pub(crate) fn for_each_operand(&self, mut f: impl FnMut(Value)) {
        if let CallTarget::Indirect { callee, .. } = self.target {
            f(callee);
        }
        self.args.iter().copied().for_each(&mut f);
        self.gc.iter().copied().for_each(f);
    }

    /// Rewrites every value the call reads.
    pub(crate) fn map_operands(&mut self, mut f: impl FnMut(Value) -> Value) {
        if let CallTarget::Indirect { callee, .. } = &mut self.target {
            *callee = f(*callee);
        }
        for v in self.args.iter_mut().chain(self.gc.iter_mut()) {
            *v = f(*v);
        }
    }
}

/// The data of one non-terminator instruction.
///
/// Result types are not stored here: each result value carries its type, and the
/// validator checks it against the type the instruction produces. Instructions with
/// a policy whose fields include `error` are only valid inside a
/// [`Terminator::Check`]; as plain instructions they are rejected.
///
/// # Examples
///
/// ```
/// use ir_lang::{BinaryOp, InstData, Overflow, Policy, Value};
///
/// let add = InstData::Binary {
///     op: BinaryOp::Add,
///     policy: Policy::overflow(Overflow::Wrap),
///     args: [Value::from_u32(0), Value::from_u32(1)],
/// };
/// let mut used = Vec::new();
/// add.for_each_operand(|v| used.push(v));
/// assert_eq!(used.len(), 2);
/// assert!(!add.has_side_effects());
/// ```
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum InstData {
    /// A constant of a scalar type. `bits` holds the value's bit pattern masked to
    /// the type's width (integers as two's complement, floats as IEEE bits, `bool`
    /// as 0/1, `ptr`/`ref` only 0, the null address/reference).
    Const {
        /// The constant's type.
        ty: Type,
        /// The bit pattern.
        bits: u64,
    },
    /// A unary operation.
    Unary {
        /// The operation.
        op: UnaryOp,
        /// Its policy.
        policy: Policy,
        /// The operand.
        arg: Value,
    },
    /// A binary operation.
    Binary {
        /// The operation.
        op: BinaryOp,
        /// Its policy.
        policy: Policy,
        /// The operands.
        args: [Value; 2],
    },
    /// Fused multiply-add `a * b + c` on floats, correctly rounded once.
    Fma {
        /// `a`, `b`, `c`.
        args: [Value; 3],
    },
    /// A comparison giving `bool`.
    Compare {
        /// The comparison.
        op: CmpOp,
        /// The operands.
        args: [Value; 2],
    },
    /// A conversion to `to`.
    Convert {
        /// The conversion.
        op: ConvOp,
        /// Its policy.
        policy: Policy,
        /// The result type.
        to: Type,
        /// The operand.
        arg: Value,
    },
    /// `cond ? args[0] : args[1]`.
    Select {
        /// The `bool` condition.
        cond: Value,
        /// The values chosen when true and when false.
        args: [Value; 2],
    },
    /// The address of a stack slot of this function.
    StackAddr {
        /// The slot.
        slot: StackSlot,
    },
    /// The address of a global.
    GlobalAddr {
        /// The global.
        global: GlobalId,
    },
    /// The address of a function (for indirect calls).
    FuncAddr {
        /// The function.
        func: FuncId,
    },
    /// `base` plus the offset of field `field` of struct `agg`.
    FieldAddr {
        /// The struct's address.
        base: Value,
        /// The struct type.
        agg: AggId,
        /// The field index.
        field: u32,
    },
    /// `base + index * size(elem)`, wrapping at the address width. A signed index
    /// is sign-extended, an unsigned one zero-extended.
    ElemAddr {
        /// The first element's address.
        base: Value,
        /// The element type (its size is the stride).
        elem: MemType,
        /// The element index (any integer type).
        index: Value,
    },
    /// `base + offset` bytes, wrapping at the address width.
    PtrOffset {
        /// The address.
        base: Value,
        /// The byte offset (any integer type; signed is sign-extended).
        offset: Value,
    },
    /// Read a `ty` from `addr`.
    Load {
        /// The loaded type.
        ty: Type,
        /// The address.
        addr: Value,
        /// Alignment and volatility.
        flags: MemFlags,
    },
    /// Write `value` to `addr`.
    Store {
        /// The stored value.
        value: Value,
        /// The address.
        addr: Value,
        /// Alignment and volatility.
        flags: MemFlags,
    },
    /// Atomically read a `ty` (an integer or `ptr`) from naturally aligned `addr`.
    AtomicLoad {
        /// The loaded type.
        ty: Type,
        /// The address.
        addr: Value,
        /// The ordering.
        order: MemOrder,
    },
    /// Atomically write `value` (an integer or `ptr`) to naturally aligned `addr`.
    AtomicStore {
        /// The stored value.
        value: Value,
        /// The address.
        addr: Value,
        /// The ordering.
        order: MemOrder,
    },
    /// Atomically replace the integer at `addr` with `op(old, value)`; gives `old`.
    AtomicRmw {
        /// The operation.
        op: RmwOp,
        /// The address.
        addr: Value,
        /// The operand.
        value: Value,
        /// The ordering.
        order: MemOrder,
    },
    /// Atomic compare-exchange; gives the old value and whether it was replaced.
    CmpXchg {
        /// The address.
        addr: Value,
        /// The expected old value.
        expected: Value,
        /// The replacement.
        new: Value,
        /// The ordering on success.
        success: MemOrder,
        /// The ordering on failure.
        failure: MemOrder,
    },
    /// A memory fence.
    Fence {
        /// The ordering (not `relaxed`).
        order: MemOrder,
    },
    /// Copy `len` bytes from `src` to `dst`; the ranges must not overlap.
    MemCopy {
        /// Destination address.
        dst: Value,
        /// Source address.
        src: Value,
        /// Byte count (an unsigned integer).
        len: Value,
        /// Alignment of both addresses, and volatility.
        flags: MemFlags,
    },
    /// Copy `len` bytes from `src` to `dst`; the ranges may overlap.
    MemMove {
        /// Destination address.
        dst: Value,
        /// Source address.
        src: Value,
        /// Byte count (an unsigned integer).
        len: Value,
        /// Alignment of both addresses, and volatility.
        flags: MemFlags,
    },
    /// Set `len` bytes at `dst` to `byte` (a `u8`).
    MemSet {
        /// Destination address.
        dst: Value,
        /// The fill byte.
        byte: Value,
        /// Byte count (an unsigned integer).
        len: Value,
        /// Alignment and volatility.
        flags: MemFlags,
    },
    /// A call. Results: the callee's returns, then one relocated `ref` per `gc`
    /// entry.
    Call(Box<Call>),
    /// An explicit safepoint (a GC poll). Results: one relocated `ref` per `gc`
    /// entry.
    Safepoint {
        /// The `ref` values relocated.
        gc: Box<[Value]>,
    },
}

impl InstData {
    /// Calls `f` on every value the instruction reads, in operand order.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{InstData, Value};
    ///
    /// let mut n = 0;
    /// InstData::Fma { args: [Value::from_u32(0); 3] }.for_each_operand(|_| n += 1);
    /// assert_eq!(n, 3);
    /// ```
    pub fn for_each_operand(&self, mut f: impl FnMut(Value)) {
        match self {
            InstData::Const { .. }
            | InstData::StackAddr { .. }
            | InstData::GlobalAddr { .. }
            | InstData::FuncAddr { .. }
            | InstData::Fence { .. } => {}
            InstData::Unary { arg, .. } | InstData::Convert { arg, .. } => f(*arg),
            InstData::Binary { args, .. } | InstData::Compare { args, .. } => {
                f(args[0]);
                f(args[1]);
            }
            InstData::Fma { args } => args.iter().copied().for_each(f),
            InstData::Select { cond, args } => {
                f(*cond);
                f(args[0]);
                f(args[1]);
            }
            InstData::FieldAddr { base, .. } => f(*base),
            InstData::ElemAddr { base, index, .. } => {
                f(*base);
                f(*index);
            }
            InstData::PtrOffset { base, offset } => {
                f(*base);
                f(*offset);
            }
            InstData::Load { addr, .. } | InstData::AtomicLoad { addr, .. } => f(*addr),
            InstData::Store { value, addr, .. } | InstData::AtomicStore { value, addr, .. } => {
                f(*value);
                f(*addr);
            }
            InstData::AtomicRmw { addr, value, .. } => {
                f(*addr);
                f(*value);
            }
            InstData::CmpXchg {
                addr,
                expected,
                new,
                ..
            } => {
                f(*addr);
                f(*expected);
                f(*new);
            }
            InstData::MemCopy { dst, src, len, .. } | InstData::MemMove { dst, src, len, .. } => {
                f(*dst);
                f(*src);
                f(*len);
            }
            InstData::MemSet { dst, byte, len, .. } => {
                f(*dst);
                f(*byte);
                f(*len);
            }
            InstData::Call(call) => call.for_each_operand(f),
            InstData::Safepoint { gc } => gc.iter().copied().for_each(f),
        }
    }

    /// Rewrites every value the instruction reads.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{InstData, UnaryOp, Policy, Value};
    ///
    /// let mut i = InstData::Unary { op: UnaryOp::Not, policy: Policy::NONE, arg: Value::from_u32(1) };
    /// i.map_operands(|v| Value::from_u32(v.as_u32() + 10));
    /// assert_eq!(i, InstData::Unary { op: UnaryOp::Not, policy: Policy::NONE, arg: Value::from_u32(11) });
    /// ```
    pub fn map_operands(&mut self, mut f: impl FnMut(Value) -> Value) {
        match self {
            InstData::Const { .. }
            | InstData::StackAddr { .. }
            | InstData::GlobalAddr { .. }
            | InstData::FuncAddr { .. }
            | InstData::Fence { .. } => {}
            InstData::Unary { arg, .. } | InstData::Convert { arg, .. } => *arg = f(*arg),
            InstData::Binary { args, .. } | InstData::Compare { args, .. } => {
                args[0] = f(args[0]);
                args[1] = f(args[1]);
            }
            InstData::Fma { args } => {
                for a in args.iter_mut() {
                    *a = f(*a);
                }
            }
            InstData::Select { cond, args } => {
                *cond = f(*cond);
                args[0] = f(args[0]);
                args[1] = f(args[1]);
            }
            InstData::FieldAddr { base, .. } => *base = f(*base),
            InstData::ElemAddr { base, index, .. } => {
                *base = f(*base);
                *index = f(*index);
            }
            InstData::PtrOffset { base, offset } => {
                *base = f(*base);
                *offset = f(*offset);
            }
            InstData::Load { addr, .. } | InstData::AtomicLoad { addr, .. } => *addr = f(*addr),
            InstData::Store { value, addr, .. } | InstData::AtomicStore { value, addr, .. } => {
                *value = f(*value);
                *addr = f(*addr);
            }
            InstData::AtomicRmw { addr, value, .. } => {
                *addr = f(*addr);
                *value = f(*value);
            }
            InstData::CmpXchg {
                addr,
                expected,
                new,
                ..
            } => {
                *addr = f(*addr);
                *expected = f(*expected);
                *new = f(*new);
            }
            InstData::MemCopy { dst, src, len, .. } | InstData::MemMove { dst, src, len, .. } => {
                *dst = f(*dst);
                *src = f(*src);
                *len = f(*len);
            }
            InstData::MemSet { dst, byte, len, .. } => {
                *dst = f(*dst);
                *byte = f(*byte);
                *len = f(*len);
            }
            InstData::Call(call) => call.map_operands(f),
            InstData::Safepoint { gc } => {
                for v in gc.iter_mut() {
                    *v = f(*v);
                }
            }
        }
    }

    /// Whether the instruction may do anything besides computing its results:
    /// write memory, read volatile memory, synchronize, call, poll the GC, or trap
    /// under a `trap` policy. An instruction without side effects whose results are
    /// unused can be removed.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{BinaryOp, InstData, Overflow, Policy, Value};
    ///
    /// let v = Value::from_u32(0);
    /// let trapping = InstData::Binary { op: BinaryOp::Add, policy: Policy::overflow(Overflow::Trap), args: [v, v] };
    /// assert!(trapping.has_side_effects());
    /// ```
    #[must_use]
    pub fn has_side_effects(&self) -> bool {
        use crate::ops::{DivZero, Overflow};
        let traps = |p: &Policy| {
            matches!(p.overflow, Some(Overflow::Trap)) || matches!(p.div_zero, Some(DivZero::Trap))
        };
        match self {
            InstData::Unary { policy, .. }
            | InstData::Binary { policy, .. }
            | InstData::Convert { policy, .. } => traps(policy) || policy.can_error(),
            InstData::Load { flags, .. } => flags.volatile,
            InstData::Store { .. }
            | InstData::AtomicLoad { .. }
            | InstData::AtomicStore { .. }
            | InstData::AtomicRmw { .. }
            | InstData::CmpXchg { .. }
            | InstData::Fence { .. }
            | InstData::MemCopy { .. }
            | InstData::MemMove { .. }
            | InstData::MemSet { .. }
            | InstData::Call(_)
            | InstData::Safepoint { .. } => true,
            InstData::Const { .. }
            | InstData::Fma { .. }
            | InstData::Compare { .. }
            | InstData::Select { .. }
            | InstData::StackAddr { .. }
            | InstData::GlobalAddr { .. }
            | InstData::FuncAddr { .. }
            | InstData::FieldAddr { .. }
            | InstData::ElemAddr { .. }
            | InstData::PtrOffset { .. } => false,
        }
    }

    /// The textual opcode of the instruction (`add`, `load`, `call`, ...).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{InstData, Type};
    ///
    /// assert_eq!(InstData::Const { ty: Type::I8, bits: 0 }.opcode(), "const");
    /// ```
    #[must_use]
    pub fn opcode(&self) -> &'static str {
        match self {
            InstData::Const { .. } => "const",
            InstData::Unary { op, .. } => op.name(),
            InstData::Binary { op, .. } => op.name(),
            InstData::Fma { .. } => "fma",
            InstData::Compare { op, .. } => op.name(),
            InstData::Convert { op, .. } => op.name(),
            InstData::Select { .. } => "select",
            InstData::StackAddr { .. } => "stack_addr",
            InstData::GlobalAddr { .. } => "global_addr",
            InstData::FuncAddr { .. } => "func_addr",
            InstData::FieldAddr { .. } => "field_addr",
            InstData::ElemAddr { .. } => "elem_addr",
            InstData::PtrOffset { .. } => "ptr_offset",
            InstData::Load { .. } => "load",
            InstData::Store { .. } => "store",
            InstData::AtomicLoad { .. } => "atomic_load",
            InstData::AtomicStore { .. } => "atomic_store",
            InstData::AtomicRmw { .. } => "atomic_rmw",
            InstData::CmpXchg { .. } => "cmpxchg",
            InstData::Fence { .. } => "fence",
            InstData::MemCopy { .. } => "memcpy",
            InstData::MemMove { .. } => "memmove",
            InstData::MemSet { .. } => "memset",
            InstData::Call(call) => match call.target {
                CallTarget::Direct(_) => "call",
                CallTarget::Indirect { .. } => "call_indirect",
            },
            InstData::Safepoint { .. } => "safepoint",
        }
    }
}

/// An argument passed along a control-flow edge to a block parameter.
///
/// Besides ordinary values, the edges of `invoke` and `check` terminators can pass
/// what the terminator itself produces, which is not an SSA value before the edge
/// is taken:
///
/// | Argument | Valid on | Meaning |
/// |---|---|---|
/// | `Result(i)` | `invoke` normal edge; `check` normal edge (`i = 0`) | the `i`-th result (returns, then relocated refs) |
/// | `Result(i)` | `invoke` unwind edge, `i >= returns` | a relocated ref |
/// | `Exception` | `invoke` unwind edge | the exception payload (`ptr`) |
/// | `ErrorCode` | `check` error edge | the OPS error code (`u32`) |
///
/// # Examples
///
/// ```
/// use ir_lang::{BlockArg, Value};
///
/// let a: BlockArg = Value::from_u32(3).into();
/// assert_eq!(a, BlockArg::Value(Value::from_u32(3)));
/// assert_eq!(BlockArg::Result(1).to_string(), "res1");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum BlockArg {
    /// An SSA value.
    Value(Value),
    /// A result of the `invoke` or `check` terminator.
    Result(u32),
    /// The exception payload of an `invoke`'s unwind edge.
    Exception,
    /// The error code of a `check`'s error edge.
    ErrorCode,
}

impl From<Value> for BlockArg {
    fn from(v: Value) -> Self {
        BlockArg::Value(v)
    }
}

impl fmt::Display for BlockArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockArg::Value(v) => write!(f, "{v}"),
            BlockArg::Result(i) => write!(f, "res{i}"),
            BlockArg::Exception => f.write_str("exn"),
            BlockArg::ErrorCode => f.write_str("err"),
        }
    }
}

/// A control-flow edge: a target block and the arguments for its parameters.
///
/// # Examples
///
/// ```
/// use ir_lang::{Block, BlockCall, Value};
///
/// let c = BlockCall::new(Block::from_u32(2), &[Value::from_u32(0)]);
/// assert_eq!(c.to_string(), "b2(v0)");
/// assert_eq!(BlockCall::new(Block::from_u32(1), &[]).to_string(), "b1");
/// ```
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct BlockCall {
    /// The target block.
    pub block: Block,
    /// One argument per target parameter.
    pub args: Vec<BlockArg>,
}

impl BlockCall {
    /// An edge passing SSA values.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Block, BlockCall};
    ///
    /// assert!(BlockCall::new(Block::from_u32(0), &[]).args.is_empty());
    /// ```
    #[must_use]
    pub fn new(block: Block, args: &[Value]) -> BlockCall {
        BlockCall {
            block,
            args: args.iter().copied().map(BlockArg::Value).collect(),
        }
    }

    /// An edge passing arbitrary [`BlockArg`]s.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Block, BlockArg, BlockCall};
    ///
    /// let c = BlockCall::with_args(Block::from_u32(1), &[BlockArg::Exception]);
    /// assert_eq!(c.to_string(), "b1(exn)");
    /// ```
    #[must_use]
    pub fn with_args(block: Block, args: &[BlockArg]) -> BlockCall {
        BlockCall {
            block,
            args: args.to_vec(),
        }
    }

    fn for_each_value(&self, mut f: impl FnMut(Value)) {
        for arg in &self.args {
            if let BlockArg::Value(v) = arg {
                f(*v);
            }
        }
    }

    fn map_values(&mut self, mut f: impl FnMut(Value) -> Value) {
        for arg in &mut self.args {
            if let BlockArg::Value(v) = arg {
                *v = f(*v);
            }
        }
    }
}

impl fmt::Display for BlockCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.block)?;
        if !self.args.is_empty() {
            f.write_str("(")?;
            for (i, a) in self.args.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                write!(f, "{a}")?;
            }
            f.write_str(")")?;
        }
        Ok(())
    }
}

/// One case of a [`Terminator::Switch`].
///
/// # Examples
///
/// ```
/// use ir_lang::{Block, BlockCall, SwitchCase};
///
/// let c = SwitchCase { value: 7, dest: BlockCall::new(Block::from_u32(1), &[]) };
/// assert_eq!(c.value, 7);
/// ```
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SwitchCase {
    /// The matched value, as bits masked to the scrutinee's width.
    pub value: u64,
    /// Where control goes when the scrutinee equals `value`.
    pub dest: BlockCall,
}

/// The instruction that ends a block and says where control goes next.
///
/// # Examples
///
/// ```
/// use ir_lang::{Block, BlockCall, Terminator};
///
/// let t = Terminator::Jump(BlockCall::new(Block::from_u32(1), &[]));
/// assert_eq!(t.successors().count(), 1);
/// assert!(Terminator::Unreachable.successors().next().is_none());
/// ```
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Terminator {
    /// Unconditional jump.
    Jump(BlockCall),
    /// Two-way branch on a `bool`.
    Branch {
        /// The condition.
        cond: Value,
        /// Taken when `cond` is true.
        then_dest: BlockCall,
        /// Taken when `cond` is false.
        else_dest: BlockCall,
    },
    /// Multi-way branch on an integer; case values are distinct.
    Switch {
        /// The scrutinee.
        value: Value,
        /// The cases.
        cases: Vec<SwitchCase>,
        /// Taken when no case matches.
        default: BlockCall,
    },
    /// Return from the function with values matching its signature's returns.
    Return {
        /// The returned values.
        values: Vec<Value>,
    },
    /// Control never reaches here; reaching it is undefined behaviour.
    Unreachable,
    /// Abort the program deterministically with `code` (OPS §6 codes 1–5 for
    /// operation errors; other codes are the language's own).
    Trap {
        /// The trap code.
        code: u32,
    },
    /// A tail call: the callee's returns become this function's returns. The
    /// callee's convention and returns must equal this function's; `gc` must be
    /// empty (the frame is gone).
    TailCall(Box<Call>),
    /// A call with an exception edge: on normal return control goes to `normal`,
    /// if the callee unwinds it goes to `unwind`, a landing pad.
    Invoke {
        /// The call.
        call: Box<Call>,
        /// The normal-return edge.
        normal: BlockCall,
        /// The unwind edge.
        unwind: BlockCall,
    },
    /// An operation whose policy has an `error` field (or `char_from_u32`): on
    /// success control goes to `normal` (its result is `Result(0)`); on failure to
    /// `error` with the error code as `ErrorCode`.
    Check {
        /// The operation: a `Unary`, `Binary`, or `Convert` instruction.
        op: Box<InstData>,
        /// The success edge.
        normal: BlockCall,
        /// The error edge.
        error: BlockCall,
    },
    /// Unwind out of this function with `payload` (a `ptr`): rethrows after
    /// cleanup in a landing pad, or raises an exception the runtime prepared.
    Resume {
        /// The exception payload.
        payload: Value,
    },
}

/// An iterator over the outgoing edges of a terminator.
#[derive(Debug)]
pub struct Successors<'a> {
    first: [Option<&'a BlockCall>; 2],
    cases: core::slice::Iter<'a, SwitchCase>,
}

impl<'a> Iterator for Successors<'a> {
    type Item = &'a BlockCall;

    fn next(&mut self) -> Option<&'a BlockCall> {
        if let Some(c) = self.cases.next() {
            return Some(&c.dest);
        }
        if let Some(c) = self.first[0].take() {
            return Some(c);
        }
        self.first[1].take()
    }
}

impl Terminator {
    /// The outgoing edges, in a fixed order (switch cases first, then the
    /// default; `then` before `else`; `normal` before `unwind`/`error`).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Block, BlockCall, Terminator, Value};
    ///
    /// let t = Terminator::Branch {
    ///     cond: Value::from_u32(0),
    ///     then_dest: BlockCall::new(Block::from_u32(1), &[]),
    ///     else_dest: BlockCall::new(Block::from_u32(2), &[]),
    /// };
    /// let targets: Vec<_> = t.successors().map(|c| c.block.index()).collect();
    /// assert_eq!(targets, [1, 2]);
    /// ```
    #[must_use]
    pub fn successors(&self) -> Successors<'_> {
        let none: [Option<&BlockCall>; 2] = [None, None];
        match self {
            Terminator::Jump(c) => Successors {
                first: [Some(c), None],
                cases: [].iter(),
            },
            Terminator::Branch {
                then_dest,
                else_dest,
                ..
            } => Successors {
                first: [Some(then_dest), Some(else_dest)],
                cases: [].iter(),
            },
            Terminator::Switch { cases, default, .. } => Successors {
                first: [Some(default), None],
                cases: cases.iter(),
            },
            Terminator::Invoke { normal, unwind, .. } => Successors {
                first: [Some(normal), Some(unwind)],
                cases: [].iter(),
            },
            Terminator::Check { normal, error, .. } => Successors {
                first: [Some(normal), Some(error)],
                cases: [].iter(),
            },
            Terminator::Return { .. }
            | Terminator::Unreachable
            | Terminator::Trap { .. }
            | Terminator::TailCall(_)
            | Terminator::Resume { .. } => Successors {
                first: none,
                cases: [].iter(),
            },
        }
    }

    /// Mutable access to the outgoing edges, in the same order as
    /// [`successors`](Terminator::successors).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Block, BlockCall, Terminator};
    ///
    /// let mut t = Terminator::Jump(BlockCall::new(Block::from_u32(1), &[]));
    /// t.for_each_successor_mut(|c| c.block = Block::from_u32(5));
    /// assert_eq!(t.successors().next().unwrap().block, Block::from_u32(5));
    /// ```
    pub fn for_each_successor_mut(&mut self, mut f: impl FnMut(&mut BlockCall)) {
        match self {
            Terminator::Jump(c) => f(c),
            Terminator::Branch {
                then_dest,
                else_dest,
                ..
            } => {
                f(then_dest);
                f(else_dest);
            }
            Terminator::Switch { cases, default, .. } => {
                for c in cases {
                    f(&mut c.dest);
                }
                f(default);
            }
            Terminator::Invoke { normal, unwind, .. } => {
                f(normal);
                f(unwind);
            }
            Terminator::Check { normal, error, .. } => {
                f(normal);
                f(error);
            }
            Terminator::Return { .. }
            | Terminator::Unreachable
            | Terminator::Trap { .. }
            | Terminator::TailCall(_)
            | Terminator::Resume { .. } => {}
        }
    }

    /// Calls `f` on every SSA value the terminator reads, including the values
    /// passed along its edges.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Terminator, Value};
    ///
    /// let mut n = 0;
    /// Terminator::Return { values: vec![Value::from_u32(0)] }.for_each_operand(|_| n += 1);
    /// assert_eq!(n, 1);
    /// ```
    pub fn for_each_operand(&self, mut f: impl FnMut(Value)) {
        self.for_each_own_operand(&mut f);
        for dest in self.successors() {
            dest.for_each_value(&mut f);
        }
    }

    /// Calls `f` on the values the terminator itself consumes, excluding edge
    /// arguments (the call's operands, the condition, the returned values).
    pub(crate) fn for_each_own_operand(&self, mut f: impl FnMut(Value)) {
        match self {
            Terminator::Jump(_) | Terminator::Unreachable | Terminator::Trap { .. } => {}
            Terminator::Branch { cond, .. } => f(*cond),
            Terminator::Switch { value, .. } => f(*value),
            Terminator::Return { values } => values.iter().copied().for_each(f),
            Terminator::TailCall(call) => call.for_each_operand(f),
            Terminator::Invoke { call, .. } => call.for_each_operand(f),
            Terminator::Check { op, .. } => op.for_each_operand(f),
            Terminator::Resume { payload } => f(*payload),
        }
    }

    /// Rewrites every SSA value the terminator reads, including edge arguments.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Terminator, Value};
    ///
    /// let mut t = Terminator::Resume { payload: Value::from_u32(0) };
    /// t.map_operands(|_| Value::from_u32(9));
    /// assert_eq!(t, Terminator::Resume { payload: Value::from_u32(9) });
    /// ```
    pub fn map_operands(&mut self, mut f: impl FnMut(Value) -> Value) {
        self.map_own_operands(&mut f);
        self.for_each_successor_mut(|c| c.map_values(&mut f));
    }

    /// Rewrites the values the terminator itself consumes, excluding edge
    /// arguments.
    pub(crate) fn map_own_operands(&mut self, mut f: impl FnMut(Value) -> Value) {
        match self {
            Terminator::Jump(_) | Terminator::Unreachable | Terminator::Trap { .. } => {}
            Terminator::Branch { cond, .. } => *cond = f(*cond),
            Terminator::Switch { value, .. } => *value = f(*value),
            Terminator::Return { values } => {
                for v in values.iter_mut() {
                    *v = f(*v);
                }
            }
            Terminator::TailCall(call) | Terminator::Invoke { call, .. } => {
                call.map_operands(&mut f);
            }
            Terminator::Check { op, .. } => op.map_operands(&mut f),
            Terminator::Resume { payload } => *payload = f(*payload),
        }
    }

    /// The textual opcode (`jump`, `br`, `invoke`, ...).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::Terminator;
    ///
    /// assert_eq!(Terminator::Unreachable.opcode(), "unreachable");
    /// ```
    #[must_use]
    pub fn opcode(&self) -> &'static str {
        match self {
            Terminator::Jump(_) => "jump",
            Terminator::Branch { .. } => "br",
            Terminator::Switch { .. } => "switch",
            Terminator::Return { .. } => "ret",
            Terminator::Unreachable => "unreachable",
            Terminator::Trap { .. } => "trap",
            Terminator::TailCall(call) => match call.target {
                CallTarget::Direct(_) => "tail_call",
                CallTarget::Indirect { .. } => "tail_call_indirect",
            },
            Terminator::Invoke { call, .. } => match call.target {
                CallTarget::Direct(_) => "invoke",
                CallTarget::Indirect { .. } => "invoke_indirect",
            },
            Terminator::Check { .. } => "check",
            Terminator::Resume { .. } => "resume",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn test_switch_successors_list_cases_then_default() {
        let t = Terminator::Switch {
            value: Value::from_u32(0),
            cases: vec![
                SwitchCase {
                    value: 1,
                    dest: BlockCall::new(Block::from_u32(1), &[]),
                },
                SwitchCase {
                    value: 2,
                    dest: BlockCall::new(Block::from_u32(2), &[]),
                },
            ],
            default: BlockCall::new(Block::from_u32(3), &[]),
        };
        let order: Vec<usize> = t.successors().map(|c| c.block.index()).collect();
        assert_eq!(order, [1, 2, 3]);
    }

    #[test]
    fn test_terminator_operands_include_edge_values_only() {
        let t = Terminator::Invoke {
            call: Box::new(Call::direct(FuncId::from_u32(0), &[Value::from_u32(1)])),
            normal: BlockCall::with_args(
                Block::from_u32(1),
                &[BlockArg::Result(0), BlockArg::Value(Value::from_u32(2))],
            ),
            unwind: BlockCall::with_args(Block::from_u32(2), &[BlockArg::Exception]),
        };
        let mut seen = Vec::new();
        t.for_each_operand(|v| seen.push(v.index()));
        assert_eq!(seen, [1, 2]);
    }

    #[test]
    fn test_attrs_display_and_parse() {
        let a = Attrs::NOGC | Attrs::NORETURN;
        assert_eq!(a.to_string(), "[noreturn, nogc]");
        assert_eq!(Attrs::from_name("inline"), Some(Attrs::INLINE));
        assert_eq!(Attrs::from_name("bogus"), None);
    }

    #[test]
    fn test_mem_order_names_round_trip() {
        for o in MemOrder::ALL {
            assert_eq!(MemOrder::from_name(o.name()), Some(o));
        }
        for o in RmwOp::ALL {
            assert_eq!(RmwOp::from_name(o.name()), Some(o));
        }
    }

    #[test]
    fn test_inst_data_stays_small() {
        // Instructions live in a dense arena; keep the record compact.
        assert!(core::mem::size_of::<InstData>() <= 24);
    }

    #[test]
    fn test_block_arg_display() {
        assert_eq!(BlockArg::Exception.to_string(), "exn");
        assert_eq!(BlockArg::ErrorCode.to_string(), "err");
        assert_eq!(BlockArg::Value(Value::from_u32(2)).to_string(), "v2");
    }
}
