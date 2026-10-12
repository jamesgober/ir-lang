//! The errors of declaring, building, and validating IR.

use alloc::string::String;
use core::fmt;

use crate::entity::{AggId, Block, FuncId, GlobalId, Inst, SigId, StackSlot, Value, Variable};
use crate::ops::PolicyFields;
use crate::types::Type;

/// Why a module-level declaration was refused.
///
/// Every variant names what was wrong, so a front end can report it against its own
/// source. The enum is `#[non_exhaustive]`.
///
/// # Examples
///
/// ```
/// use ir_lang::{Linkage, Module, ModuleError, Signature};
///
/// let mut m = Module::new("m");
/// let sig = Signature::new(&[], &[]);
/// m.declare_function("f", &sig, Linkage::Export)?;
/// let err = m.declare_function("f", &sig, Linkage::Export).unwrap_err();
/// assert_eq!(err, ModuleError::DuplicateName { name: "f".into() });
/// # Ok::<(), ModuleError>(())
/// ```
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ModuleError {
    /// A function or global with this name already exists. Symbol names are one
    /// namespace per module.
    DuplicateName {
        /// The repeated name.
        name: String,
    },
    /// A symbol name was empty.
    EmptyName,
    /// The function handle does not name a declaration of this module.
    UnknownFunction {
        /// The handle.
        func: FuncId,
    },
    /// The global handle does not name a declaration of this module.
    UnknownGlobal {
        /// The handle.
        global: GlobalId,
    },
    /// An aggregate names an aggregate that is not declared before it.
    UnknownAggregate {
        /// The handle.
        agg: AggId,
    },
    /// An aggregate's size exceeds [`MAX_AGGREGATE_SIZE`](crate::MAX_AGGREGATE_SIZE).
    AggregateTooLarge,
    /// An alignment is zero, not a power of two, or larger than allowed.
    BadAlignment {
        /// The alignment.
        align: u64,
    },
    /// A global's initializer length differs from its declared size.
    InitSizeMismatch {
        /// The declared size.
        size: u64,
        /// The initializer length.
        found: u64,
    },
    /// A relocation does not fit inside its global (`offset + pointer size > size`).
    RelocOutOfRange {
        /// The relocation offset.
        offset: u64,
    },
    /// An imported global was given an initializer or relocations.
    ImportWithData,
    /// An imported function cannot have a body.
    CannotDefineImport {
        /// The function.
        func: FuncId,
    },
    /// The function already has a body (use `Module::edit` to change it).
    AlreadyDefined {
        /// The function.
        func: FuncId,
    },
    /// The function has no body yet (use `Module::build` to create one).
    NotDefined {
        /// The function.
        func: FuncId,
    },
    /// A table of the module reached its capacity (`u32::MAX - 1` entries).
    CapacityExceeded,
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModuleError::DuplicateName { name } => write!(f, "symbol `{name}` is already declared"),
            ModuleError::EmptyName => f.write_str("symbol names must not be empty"),
            ModuleError::UnknownFunction { func } => write!(f, "unknown function {func}"),
            ModuleError::UnknownGlobal { global } => write!(f, "unknown global {global}"),
            ModuleError::UnknownAggregate { agg } => {
                write!(f, "aggregate {agg} is not declared before its use")
            }
            ModuleError::AggregateTooLarge => f.write_str("aggregate size exceeds 2^48 bytes"),
            ModuleError::BadAlignment { align } => {
                write!(f, "alignment {align} is not a supported power of two")
            }
            ModuleError::InitSizeMismatch { size, found } => {
                write!(
                    f,
                    "initializer has {found} bytes but the global is {size} bytes"
                )
            }
            ModuleError::RelocOutOfRange { offset } => {
                write!(
                    f,
                    "relocation at offset {offset} does not fit in its global"
                )
            }
            ModuleError::ImportWithData => {
                f.write_str("an imported global cannot have an initializer or relocations")
            }
            ModuleError::CannotDefineImport { func } => {
                write!(f, "function {func} is imported and cannot have a body")
            }
            ModuleError::AlreadyDefined { func } => write!(f, "function {func} already has a body"),
            ModuleError::NotDefined { func } => write!(f, "function {func} has no body"),
            ModuleError::CapacityExceeded => f.write_str("module table capacity exceeded"),
        }
    }
}

impl core::error::Error for ModuleError {}

/// What is wrong, for a [`ValidationError`] (and for a [`BuildError::Invalid`]).
///
/// The enum is `#[non_exhaustive]`: new checks may add variants in a minor release.
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum ValidationErrorKind {
    // ----- module level -----
    /// Two symbols share a name.
    DuplicateName {
        /// The name.
        name: String,
    },
    /// A symbol name is empty.
    EmptyName,
    /// A signature handle is out of range.
    UnknownSignature {
        /// The handle.
        sig: SigId,
    },
    /// A function handle is out of range.
    UnknownFunction {
        /// The handle.
        func: FuncId,
    },
    /// A global handle is out of range.
    UnknownGlobal {
        /// The handle.
        global: GlobalId,
    },
    /// An aggregate handle is out of range, or names a later aggregate.
    UnknownAggregate {
        /// The handle.
        agg: AggId,
    },
    /// An aggregate's recorded layout is inconsistent or too large.
    BadAggregate {
        /// The aggregate.
        agg: AggId,
    },
    /// An alignment is not a supported power of two.
    BadAlignment {
        /// The alignment.
        align: u64,
    },
    /// A global's initializer or relocations are inconsistent with its size or
    /// linkage.
    BadGlobal {
        /// The global.
        global: GlobalId,
    },
    /// A defined (local or exported) function has no body.
    MissingBody,
    /// An imported function has a body.
    ImportWithBody,

    // ----- function structure -----
    /// The entry block handle is out of range or the block was removed.
    BadEntry,
    /// The entry block's parameters do not match the signature's parameters.
    EntryParams {
        /// The signature's parameter types count.
        expected: usize,
        /// The entry block's parameter count.
        found: usize,
    },
    /// A block has no terminator.
    MissingTerminator,
    /// An edge targets a block that does not exist or was removed.
    BadBlockTarget {
        /// The target.
        target: Block,
    },
    /// An edge targets the entry block, which must have no predecessors.
    EntryBranchTarget,
    /// The block's instruction list is corrupt (a link is out of range, points
    /// into another block, or loops).
    CorruptLayout,
    /// A value handle is out of range.
    ValueOutOfRange {
        /// The handle.
        value: Value,
    },
    /// A value's recorded definition disagrees with the block parameter list or the
    /// instruction result range that lists it.
    InconsistentDefinition {
        /// The value.
        value: Value,
    },
    /// A value is listed as defined at two sites.
    DefinedTwice {
        /// The value.
        value: Value,
    },
    /// A value whose definition was removed is still used.
    UseOfDetached {
        /// The value.
        value: Value,
    },
    /// A value's forwarding (from `replace_all_uses`) is inconsistent: it loops or
    /// changes type.
    BadAlias {
        /// The value.
        value: Value,
    },
    /// A value is used where its definition does not dominate the use.
    UseBeforeDef {
        /// The value.
        value: Value,
    },
    /// A stack slot handle is out of range, or a slot has a bad size or alignment.
    BadSlot {
        /// The slot.
        slot: StackSlot,
    },

    // ----- typing -----
    /// A value has the wrong type for its position.
    TypeMismatch {
        /// The value (or `None` for a terminator-produced edge argument).
        value: Option<Value>,
        /// The required type.
        expected: Type,
        /// The actual type.
        found: Type,
    },
    /// An operand's type is not one the operation accepts.
    BadOperandType {
        /// The operation's opcode.
        op: &'static str,
        /// The operand's type.
        found: Type,
    },
    /// A conversion between these types is not allowed for this conversion op.
    BadConversion {
        /// The conversion's opcode.
        op: &'static str,
        /// The source type.
        from: Type,
        /// The result type.
        to: Type,
    },
    /// An operation instance does not carry exactly the policy fields it needs.
    PolicyMismatch {
        /// The operation's opcode.
        op: &'static str,
        /// The fields it must carry.
        required: PolicyFields,
    },
    /// An operation that can raise an error (a policy field is `error`, or
    /// `char_from_u32`) appears as a plain instruction; it must be the operation of
    /// a `check` terminator.
    ErrorOutsideCheck {
        /// The opcode.
        op: &'static str,
    },
    /// A `check` terminator holds an operation that cannot raise an error, or an
    /// instruction that is not a unary, binary, or conversion operation.
    NotFallible {
        /// The opcode.
        op: &'static str,
    },
    /// A constant's bits do not fit its type (or a `ptr`/`ref` constant is not 0).
    BadConst {
        /// The type.
        ty: Type,
        /// The bits.
        bits: u64,
    },
    /// A struct field index is out of range, or the aggregate is not a struct.
    BadField {
        /// The aggregate.
        agg: AggId,
        /// The field index.
        field: u32,
    },
    /// An atomic operation has an ordering it may not have.
    BadOrdering,
    /// An instruction's recorded result values disagree with what it produces (count
    /// or types).
    ResultMismatch,
    /// A call passes the wrong number of arguments, or a return passes the wrong
    /// number of values.
    ArgCount {
        /// The required count.
        expected: usize,
        /// The supplied count.
        found: usize,
    },
    /// An edge passes the wrong number of arguments for its target's parameters.
    EdgeArgCount {
        /// The target block.
        target: Block,
        /// The target's parameter count.
        expected: usize,
        /// The supplied count.
        found: usize,
    },
    /// A switch has two cases with the same value, or a case value does not fit
    /// the scrutinee's type.
    BadSwitchCase {
        /// The case value.
        value: u64,
    },
    /// A tail call's callee returns or convention differ from this function's, or
    /// a tail call carries a GC list.
    BadTailCall,
    /// A GC list entry is not a `ref`, or a `nogc` call carries a GC list.
    BadGcList,

    // ----- exception edges -----
    /// An edge argument is a `Result`, `Exception`, or `ErrorCode` where that kind is
    /// not allowed, or a `Result` index is out of range.
    BadEdgeArg,
    /// A landing pad (the target of an unwind edge) also has a non-unwind
    /// predecessor, or an `invoke`'s normal and unwind edges target one block.
    LandingPadPredecessor {
        /// The landing pad.
        block: Block,
    },
    /// A `nounwind` function contains a `resume`, or a call that may unwind
    /// without an `invoke`.
    UnwindInNounwind,
    /// A `noreturn` function contains a `ret`.
    ReturnInNoreturn,
    /// A `nogc` function contains a safepoint.
    SafepointInNogc,

    // ----- safepoints -----
    /// A `ref` value, or a pointer derived from one, is live across a safepoint
    /// without being relocated through its GC list.
    LiveAcrossSafepoint {
        /// The value.
        value: Value,
    },
    /// A pointer derived from a `ref` escapes tracking: it is stored to memory or
    /// passed to a call that is a safepoint.
    DerivedPointerEscapes {
        /// The value.
        value: Value,
    },
}

impl fmt::Display for ValidationErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ValidationErrorKind as K;
        match self {
            K::DuplicateName { name } => write!(f, "symbol `{name}` is declared twice"),
            K::EmptyName => f.write_str("a symbol name is empty"),
            K::UnknownSignature { sig } => write!(f, "unknown signature {sig}"),
            K::UnknownFunction { func } => write!(f, "unknown function {func}"),
            K::UnknownGlobal { global } => write!(f, "unknown global {global}"),
            K::UnknownAggregate { agg } => write!(f, "unknown or forward aggregate {agg}"),
            K::BadAggregate { agg } => write!(f, "aggregate {agg} has an invalid layout"),
            K::BadAlignment { align } => write!(f, "alignment {align} is not supported"),
            K::BadGlobal { global } => write!(f, "global {global} has an invalid initializer"),
            K::MissingBody => f.write_str("a defined function has no body"),
            K::ImportWithBody => f.write_str("an imported function has a body"),
            K::BadEntry => f.write_str("the entry block is missing"),
            K::EntryParams { expected, found } => write!(
                f,
                "the entry block has {found} parameters but the signature has {expected}"
            ),
            K::MissingTerminator => f.write_str("block has no terminator"),
            K::BadBlockTarget { target } => write!(f, "edge targets missing block {target}"),
            K::EntryBranchTarget => f.write_str("an edge targets the entry block"),
            K::CorruptLayout => f.write_str("the block's instruction list is corrupt"),
            K::ValueOutOfRange { value } => write!(f, "reference to nonexistent value {value}"),
            K::InconsistentDefinition { value } => {
                write!(f, "value {value} disagrees with its definition site")
            }
            K::DefinedTwice { value } => write!(f, "value {value} is defined twice"),
            K::UseOfDetached { value } => write!(f, "value {value} is used but no longer defined"),
            K::BadAlias { value } => write!(f, "value {value} has an invalid forwarding"),
            K::UseBeforeDef { value } => {
                write!(
                    f,
                    "value {value} is used where its definition does not dominate"
                )
            }
            K::BadSlot { slot } => write!(f, "invalid stack slot {slot}"),
            K::TypeMismatch {
                value: Some(v),
                expected,
                found,
            } => write!(f, "value {v} has type {found}, expected {expected}"),
            K::TypeMismatch {
                value: None,
                expected,
                found,
            } => write!(f, "edge argument has type {found}, expected {expected}"),
            K::BadOperandType { op, found } => write!(f, "`{op}` does not accept {found}"),
            K::BadConversion { op, from, to } => write!(f, "`{op}` cannot convert {from} to {to}"),
            K::PolicyMismatch { op, required } => {
                write!(f, "`{op}` must carry exactly the policy fields {required}")
            }
            K::ErrorOutsideCheck { op } => write!(
                f,
                "`{op}` can raise an error and must be the operation of a `check` terminator"
            ),
            K::NotFallible { op } => write!(f, "`check` holds `{op}`, which cannot raise an error"),
            K::BadConst { ty, bits } => write!(f, "constant {bits:#x} does not fit {ty}"),
            K::BadField { agg, field } => write!(f, "{agg} has no field {field}"),
            K::BadOrdering => f.write_str("invalid memory ordering for this atomic operation"),
            K::ResultMismatch => {
                f.write_str("the instruction's results disagree with what it produces")
            }
            K::ArgCount { expected, found } => {
                write!(f, "expected {expected} arguments, found {found}")
            }
            K::EdgeArgCount {
                target,
                expected,
                found,
            } => write!(
                f,
                "edge to {target} passes {found} arguments for {expected} parameters"
            ),
            K::BadSwitchCase { value } => {
                write!(f, "switch case {value} is repeated or out of range")
            }
            K::BadTailCall => {
                f.write_str("tail call does not match this function's returns and convention")
            }
            K::BadGcList => f.write_str("invalid GC list"),
            K::BadEdgeArg => f.write_str("edge argument not allowed on this edge"),
            K::LandingPadPredecessor { block } => {
                write!(f, "landing pad {block} is reached by a non-unwind edge")
            }
            K::UnwindInNounwind => f.write_str("a `nounwind` function may unwind"),
            K::ReturnInNoreturn => f.write_str("a `noreturn` function returns"),
            K::SafepointInNogc => f.write_str("a `nogc` function contains a safepoint"),
            K::LiveAcrossSafepoint { value } => write!(
                f,
                "value {value} is a GC reference (or derived from one) live across a safepoint"
            ),
            K::DerivedPointerEscapes { value } => {
                write!(f, "pointer {value} derived from a GC reference escapes")
            }
        }
    }
}

/// Why a module is not well-formed: what is wrong, and where.
///
/// [`Module::validate`](crate::Module::validate) returns the first problem it finds
/// in a fixed order (module declarations, then each function in handle order;
/// within a function: structure, instructions, terminators, exception edges,
/// dominance, safepoints).
///
/// # Examples
///
/// ```
/// use ir_lang::{Linkage, Module, Signature, Type, ValidationErrorKind};
///
/// let mut m = Module::new("m");
/// let f = m.declare_function("f", &Signature::new(&[], &[Type::I32]), Linkage::Export)?;
/// // Exported but never given a body.
/// let err = m.validate().unwrap_err();
/// assert_eq!(err.func(), Some(f));
/// assert_eq!(err.kind(), &ValidationErrorKind::MissingBody);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ValidationError {
    pub(crate) func: Option<FuncId>,
    pub(crate) block: Option<Block>,
    pub(crate) inst: Option<Inst>,
    pub(crate) kind: ValidationErrorKind,
}

impl ValidationError {
    pub(crate) fn module(kind: ValidationErrorKind) -> ValidationError {
        ValidationError {
            func: None,
            block: None,
            inst: None,
            kind,
        }
    }

    /// The function the problem is in, if it is inside one.
    ///
    /// See [`ValidationError`] for an example.
    #[must_use]
    pub fn func(&self) -> Option<FuncId> {
        self.func
    }

    /// The block the problem is in, if any.
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let _ = m.build(f)?; // the entry block is never terminated
    /// let err = m.validate().unwrap_err();
    /// assert_eq!(err.block().map(|b| b.index()), Some(0));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn block(&self) -> Option<Block> {
        self.block
    }

    /// The instruction the problem is in, if it is in one (`None` for a terminator
    /// or a block-level problem).
    ///
    /// # Examples
    ///
    /// ```
    /// use ir_lang::{Linkage, Module, Signature};
    ///
    /// let mut m = Module::new("m");
    /// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
    /// let _ = m.build(f)?;
    /// assert_eq!(m.validate().unwrap_err().inst(), None);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn inst(&self) -> Option<Inst> {
        self.inst
    }

    /// What is wrong.
    ///
    /// See [`ValidationError`] for an example.
    #[must_use]
    pub fn kind(&self) -> &ValidationErrorKind {
        &self.kind
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut sep = "";
        if let Some(func) = self.func {
            write!(f, "{sep}{func}")?;
            sep = " ";
        }
        if let Some(block) = self.block {
            write!(f, "{sep}{block}")?;
            sep = " ";
        }
        if let Some(inst) = self.inst {
            write!(f, "{sep}{inst}")?;
            sep = " ";
        }
        if sep.is_empty() {
            write!(f, "{}", self.kind)
        } else {
            write!(f, ": {}", self.kind)
        }
    }
}

impl core::error::Error for ValidationError {}

/// Why a [`Builder`](crate::Builder) operation was refused.
///
/// The builder checks each instruction and terminator as it is added, so a
/// lowering bug is reported at the call that caused it. Nothing is ever changed by
/// a refused operation.
///
/// # Examples
///
/// ```
/// use ir_lang::{BuildError, Linkage, Module, Signature};
///
/// let mut m = Module::new("m");
/// let f = m.declare_function("f", &Signature::new(&[], &[]), Linkage::Export)?;
/// let mut b = m.build(f)?;
/// b.ret(&[])?;
/// // A terminator is never silently overwritten.
/// let entry = b.func().entry();
/// assert_eq!(b.unreachable(), Err(BuildError::AlreadyTerminated { block: entry }));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum BuildError {
    /// The block already has a terminator. Remove it first
    /// ([`Builder::remove_terminator`](crate::Builder::remove_terminator)) to replace it.
    AlreadyTerminated {
        /// The block.
        block: Block,
    },
    /// The block handle is unknown or the block was removed.
    UnknownBlock {
        /// The block.
        block: Block,
    },
    /// The instruction handle is unknown or the instruction was removed.
    UnknownInst {
        /// The instruction.
        inst: Inst,
    },
    /// The value handle is unknown.
    UnknownValue {
        /// The value.
        value: Value,
    },
    /// There is no function parameter at this index.
    UnknownParam {
        /// The index.
        index: usize,
    },
    /// The entry block cannot be removed or split off.
    EntryBlock,
    /// A block parameter index is out of range.
    UnknownBlockParam {
        /// The block.
        block: Block,
        /// The index.
        index: usize,
    },
    /// The instruction or terminator is ill-formed.
    Invalid(ValidationErrorKind),
    /// A function table reached its capacity (`u32::MAX - 1` entries).
    CapacityExceeded,
    /// The variable was not declared by this builder (or the builder's variable
    /// state was dropped by [`compact`](crate::Builder::compact)).
    UnknownVariable {
        /// The variable.
        var: Variable,
    },
    /// An edge targets a block that was sealed: every edge into it was declared
    /// to exist already ([`Builder::seal_block`](crate::Builder::seal_block)).
    SealedBlock {
        /// The sealed block.
        block: Block,
    },
    /// The value is not a parameter of a live block.
    NotBlockParam {
        /// The value.
        value: Value,
    },
    /// The operation conflicts with variable construction in progress: splitting
    /// a block while blocks are unsealed, or adding a parameter to a block the
    /// construction already added parameters to.
    VariablesActive,
    /// A whole-function transformation would exceed its work budget
    /// ([`Builder::insert_statepoints_within`](crate::Builder::insert_statepoints_within));
    /// nothing was changed.
    BudgetExceeded,
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::AlreadyTerminated { block } => {
                write!(f, "block {block} already has a terminator")
            }
            BuildError::UnknownBlock { block } => write!(f, "unknown or removed block {block}"),
            BuildError::UnknownInst { inst } => write!(f, "unknown or removed instruction {inst}"),
            BuildError::UnknownValue { value } => write!(f, "unknown value {value}"),
            BuildError::UnknownParam { index } => write!(f, "no function parameter {index}"),
            BuildError::EntryBlock => f.write_str("the entry block cannot be removed or split off"),
            BuildError::UnknownBlockParam { block, index } => {
                write!(f, "block {block} has no parameter {index}")
            }
            BuildError::Invalid(kind) => write!(f, "{kind}"),
            BuildError::CapacityExceeded => f.write_str("function table capacity exceeded"),
            BuildError::UnknownVariable { var } => write!(f, "unknown variable {var}"),
            BuildError::SealedBlock { block } => {
                write!(f, "block {block} is sealed and cannot get a new edge")
            }
            BuildError::NotBlockParam { value } => {
                write!(f, "value {value} is not a block parameter")
            }
            BuildError::VariablesActive => {
                f.write_str("not allowed while variable construction is in progress")
            }
            BuildError::BudgetExceeded => f.write_str("the operation's work budget was exceeded"),
        }
    }
}

impl core::error::Error for BuildError {}

impl From<ValidationErrorKind> for BuildError {
    fn from(kind: ValidationErrorKind) -> Self {
        BuildError::Invalid(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn test_validation_error_display_includes_location() {
        let e = ValidationError {
            func: Some(FuncId::from_u32(1)),
            block: Some(Block::from_u32(2)),
            inst: None,
            kind: ValidationErrorKind::MissingTerminator,
        };
        assert_eq!(e.to_string(), "f1 b2: block has no terminator");
        let m = ValidationError::module(ValidationErrorKind::EmptyName);
        assert_eq!(m.to_string(), "a symbol name is empty");
    }

    #[test]
    fn test_every_build_error_displays() {
        let errs = [
            BuildError::EntryBlock,
            BuildError::CapacityExceeded,
            BuildError::UnknownParam { index: 3 },
            BuildError::Invalid(ValidationErrorKind::BadOrdering),
            BuildError::UnknownVariable {
                var: Variable::from_u32(1),
            },
            BuildError::SealedBlock {
                block: Block::from_u32(1),
            },
            BuildError::NotBlockParam {
                value: Value::from_u32(1),
            },
            BuildError::VariablesActive,
            BuildError::BudgetExceeded,
        ];
        for e in errs {
            assert!(!e.to_string().is_empty());
        }
    }
}
