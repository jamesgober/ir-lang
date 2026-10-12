//! Per-instruction and per-terminator typing, shared by the builder (which checks
//! each instruction as it is added) and the validator (which checks the whole
//! function). One implementation means the builder can never accept what the
//! validator rejects.

use alloc::vec::Vec;

use crate::entity::{Block, Value};
use crate::error::ValidationErrorKind as K;
use crate::function::Function;
use crate::inst::{
    Attrs, BlockArg, BlockCall, Call, CallTarget, InstData, MemFlags, MemOrder, RmwOp, Terminator,
};
use crate::module::Decls;
use crate::ops::{BinaryOp, CmpOp, ConvOp, Policy, UnaryOp};
use crate::types::{Aggregate, MemType, Signature, Type};

/// The type of an operand, following aliases.
pub(crate) fn value_ty(func: &Function, value: Value) -> Result<Type, K> {
    let v = func.resolve(value);
    func.value_type(v).ok_or(K::ValueOutOfRange { value })
}

fn expect(func: &Function, value: Value, ty: Type) -> Result<(), K> {
    let found = value_ty(func, value)?;
    if found == ty {
        Ok(())
    } else {
        Err(K::TypeMismatch {
            value: Some(value),
            expected: ty,
            found,
        })
    }
}

fn same(func: &Function, a: Value, b: Value) -> Result<Type, K> {
    let ta = value_ty(func, a)?;
    let tb = value_ty(func, b)?;
    if ta == tb {
        Ok(ta)
    } else {
        Err(K::TypeMismatch {
            value: Some(b),
            expected: ta,
            found: tb,
        })
    }
}

fn check_policy(op: &'static str, need: crate::ops::PolicyFields, policy: Policy) -> Result<(), K> {
    if need.matches(policy) {
        Ok(())
    } else {
        Err(K::PolicyMismatch { op, required: need })
    }
}

fn check_flags(flags: MemFlags) -> Result<(), K> {
    if crate::module::valid_align(u64::from(flags.align), u64::from(MemFlags::MAX_ALIGN)) {
        Ok(())
    } else {
        Err(K::BadAlignment {
            align: u64::from(flags.align),
        })
    }
}

fn atomic_type(op: &'static str, ty: Type) -> Result<(), K> {
    if ty.is_int() || ty == Type::Ptr {
        Ok(())
    } else {
        Err(K::BadOperandType { op, found: ty })
    }
}

/// Whether an error-raising operation appears where it is allowed.
fn check_error_placement(op: &'static str, can_error: bool, in_check: bool) -> Result<(), K> {
    match (can_error, in_check) {
        (true, false) => Err(K::ErrorOutsideCheck { op }),
        (false, true) => Err(K::NotFallible { op }),
        _ => Ok(()),
    }
}

/// The signature a call uses.
pub(crate) fn call_sig<'d>(
    decls: &'d Decls,
    func: &Function,
    call: &Call,
) -> Result<&'d Signature, K> {
    match call.target {
        CallTarget::Direct(f) => decls.func_sig(f).ok_or(K::UnknownFunction { func: f }),
        CallTarget::Indirect { sig, callee } => {
            expect(func, callee, Type::Ptr)?;
            decls
                .sigs
                .get(sig.index())
                .ok_or(K::UnknownSignature { sig })
        }
    }
}

/// The attributes in effect at a call: the site's plus, for a direct call, the
/// callee declaration's.
pub(crate) fn call_attrs(decls: &Decls, call: &Call) -> Attrs {
    match call.target {
        CallTarget::Direct(f) => decls
            .funcs
            .get(f.index())
            .map_or(call.attrs, |d| d.attrs.union(call.attrs)),
        CallTarget::Indirect { .. } => call.attrs,
    }
}

/// Whether a call is a safepoint (it may collect garbage).
pub(crate) fn call_is_safepoint(decls: &Decls, call: &Call) -> bool {
    !call_attrs(decls, call).contains(Attrs::NOGC)
}

/// Checks a call's operands and pushes its result types (returns, then one `ref`
/// per GC entry).
fn call_types(decls: &Decls, func: &Function, call: &Call, out: &mut Vec<Type>) -> Result<(), K> {
    let sig = call_sig(decls, func, call)?;
    if sig.params.len() != call.args.len() {
        return Err(K::ArgCount {
            expected: sig.params.len(),
            found: call.args.len(),
        });
    }
    for (&arg, &ty) in call.args.iter().zip(&sig.params) {
        expect(func, arg, ty)?;
    }
    if !call.gc.is_empty() && !call_is_safepoint(decls, call) {
        return Err(K::BadGcList);
    }
    for &g in &call.gc {
        if value_ty(func, g)? != Type::Ref {
            return Err(K::BadGcList);
        }
    }
    out.extend_from_slice(&sig.returns);
    out.extend(core::iter::repeat_n(Type::Ref, call.gc.len()));
    Ok(())
}

fn check_const(ty: Type, bits: u64, ptr: crate::types::PointerWidth) -> Result<(), K> {
    let ok = match ty {
        Type::Bool => bits <= 1,
        Type::Ptr | Type::Ref => bits == 0,
        _ => {
            let width = ty.bits(ptr);
            width >= 64 || bits >> width == 0
        }
    };
    if ok {
        Ok(())
    } else {
        Err(K::BadConst { ty, bits })
    }
}

fn unary_types(
    func: &Function,
    op: UnaryOp,
    policy: Policy,
    arg: Value,
    in_check: bool,
) -> Result<Type, K> {
    let ty = value_ty(func, arg)?;
    let ok = match op {
        UnaryOp::Neg | UnaryOp::Abs => ty.is_numeric(),
        UnaryOp::Not => ty.is_int() || ty == Type::Bool,
        UnaryOp::Sqrt
        | UnaryOp::Floor
        | UnaryOp::Ceil
        | UnaryOp::Trunc
        | UnaryOp::Round
        | UnaryOp::RoundEven => ty.is_float(),
    };
    if !ok {
        return Err(K::BadOperandType {
            op: op.name(),
            found: ty,
        });
    }
    check_policy(op.name(), Policy::required_unary(op, ty), policy)?;
    check_error_placement(op.name(), op.can_error(policy), in_check)?;
    Ok(ty)
}

fn binary_types(
    func: &Function,
    op: BinaryOp,
    policy: Policy,
    args: [Value; 2],
    in_check: bool,
) -> Result<Type, K> {
    let ty = same(func, args[0], args[1])?;
    let ok = match op {
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::Div
        | BinaryOp::Rem
        | BinaryOp::Min
        | BinaryOp::Max
        | BinaryOp::Pow => ty.is_numeric(),
        BinaryOp::FloorDiv | BinaryOp::FloorMod | BinaryOp::Shl | BinaryOp::Shr => ty.is_int(),
        BinaryOp::IeeeRem | BinaryOp::TotalCmp => ty.is_float(),
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => ty.is_int() || ty == Type::Bool,
    };
    if !ok {
        return Err(K::BadOperandType {
            op: op.name(),
            found: ty,
        });
    }
    check_policy(op.name(), Policy::required_binary(op, ty), policy)?;
    check_error_placement(op.name(), op.can_error(policy, ty), in_check)?;
    Ok(if op == BinaryOp::TotalCmp {
        Type::I8
    } else {
        ty
    })
}

fn conversion_ok(op: ConvOp, from: Type, to: Type, ptr: crate::types::PointerWidth) -> bool {
    let fbits = from.int_bits();
    let tbits = to.int_bits();
    match op {
        ConvOp::IntCast => from.is_int() && to.is_int() && from != to,
        ConvOp::Zext | ConvOp::Sext => matches!((fbits, tbits), (Some(f), Some(t)) if t > f),
        ConvOp::Narrow => matches!((fbits, tbits), (Some(f), Some(t)) if t < f),
        ConvOp::IntToFloat => from.is_int() && to.is_float(),
        ConvOp::FloatToInt => from.is_float() && to.is_int(),
        ConvOp::FloatCast => from.is_float() && to.is_float() && from != to,
        ConvOp::Bitcast => {
            from != to && from.is_numeric() && to.is_numeric() && from.bits(ptr) == to.bits(ptr)
        }
        ConvOp::CharFromU32 => from == Type::U32 && to == Type::U32,
        ConvOp::BoolToInt => from == Type::Bool && to.is_int(),
        ConvOp::PtrToInt => from == Type::Ptr && ptr.is_address_int(to),
        ConvOp::IntToPtr => ptr.is_address_int(from) && to == Type::Ptr,
        ConvOp::RefToPtr => from == Type::Ref && to == Type::Ptr,
    }
}

fn convert_types(
    decls: &Decls,
    func: &Function,
    op: ConvOp,
    policy: Policy,
    to: Type,
    arg: Value,
    in_check: bool,
) -> Result<Type, K> {
    let from = value_ty(func, arg)?;
    if !conversion_ok(op, from, to, decls.ptr) {
        return Err(K::BadConversion {
            op: op.name(),
            from,
            to,
        });
    }
    check_policy(op.name(), Policy::required_conversion(op), policy)?;
    check_error_placement(op.name(), op.can_error(policy), in_check)?;
    Ok(to)
}

/// Checks an instruction's operands and pushes the types of the values it
/// defines. `in_check` says whether it is the operation of a `check` terminator.
pub(crate) fn inst_types(
    decls: &Decls,
    func: &Function,
    data: &InstData,
    in_check: bool,
    out: &mut Vec<Type>,
) -> Result<(), K> {
    if in_check
        && !matches!(
            data,
            InstData::Unary { .. } | InstData::Binary { .. } | InstData::Convert { .. }
        )
    {
        return Err(K::NotFallible { op: data.opcode() });
    }
    match data {
        InstData::Const { ty, bits } => {
            check_const(*ty, *bits, decls.ptr)?;
            out.push(*ty);
        }
        InstData::Unary { op, policy, arg } => {
            out.push(unary_types(func, *op, *policy, *arg, in_check)?)
        }
        InstData::Binary { op, policy, args } => {
            out.push(binary_types(func, *op, *policy, *args, in_check)?)
        }
        InstData::Fma { args } => {
            let ty = same(func, args[0], args[1])?;
            let _ = same(func, args[0], args[2])?;
            if !ty.is_float() {
                return Err(K::BadOperandType {
                    op: "fma",
                    found: ty,
                });
            }
            out.push(ty);
        }
        InstData::Compare { op, args } => {
            let ty = same(func, args[0], args[1])?;
            let ordered = !matches!(op, CmpOp::Eq | CmpOp::Ne);
            if ordered && !ty.is_numeric() {
                return Err(K::BadOperandType {
                    op: op.name(),
                    found: ty,
                });
            }
            out.push(Type::Bool);
        }
        InstData::Convert {
            op,
            policy,
            to,
            arg,
        } => {
            out.push(convert_types(
                decls, func, *op, *policy, *to, *arg, in_check,
            )?);
        }
        InstData::Select { cond, args } => {
            expect(func, *cond, Type::Bool)?;
            out.push(same(func, args[0], args[1])?);
        }
        InstData::StackAddr { slot } => {
            if func.slot(*slot).is_none() {
                return Err(K::BadSlot { slot: *slot });
            }
            out.push(Type::Ptr);
        }
        InstData::GlobalAddr { global } => {
            if decls.globals.get(global.index()).is_none() {
                return Err(K::UnknownGlobal { global: *global });
            }
            out.push(Type::Ptr);
        }
        InstData::FuncAddr { func: f } => {
            if decls.funcs.get(f.index()).is_none() {
                return Err(K::UnknownFunction { func: *f });
            }
            out.push(Type::Ptr);
        }
        InstData::FieldAddr { base, agg, field } => {
            expect(func, *base, Type::Ptr)?;
            let data = decls
                .aggs
                .get(agg.index())
                .ok_or(K::UnknownAggregate { agg: *agg })?;
            let ok = matches!(&data.agg, Aggregate::Struct { fields, .. } if (*field as usize) < fields.len());
            if !ok {
                return Err(K::BadField {
                    agg: *agg,
                    field: *field,
                });
            }
            out.push(Type::Ptr);
        }
        InstData::ElemAddr { base, elem, index } => {
            expect(func, *base, Type::Ptr)?;
            if let MemType::Agg(a) = elem {
                if decls.aggs.get(a.index()).is_none() {
                    return Err(K::UnknownAggregate { agg: *a });
                }
            }
            let ity = value_ty(func, *index)?;
            if !ity.is_int() {
                return Err(K::BadOperandType {
                    op: "elem_addr",
                    found: ity,
                });
            }
            out.push(Type::Ptr);
        }
        InstData::PtrOffset { base, offset } => {
            expect(func, *base, Type::Ptr)?;
            let oty = value_ty(func, *offset)?;
            if !oty.is_int() {
                return Err(K::BadOperandType {
                    op: "ptr_offset",
                    found: oty,
                });
            }
            out.push(Type::Ptr);
        }
        InstData::Load { ty, addr, flags } => {
            expect(func, *addr, Type::Ptr)?;
            check_flags(*flags)?;
            out.push(*ty);
        }
        InstData::Store { value, addr, flags } => {
            let _ = value_ty(func, *value)?;
            expect(func, *addr, Type::Ptr)?;
            check_flags(*flags)?;
        }
        InstData::AtomicLoad { ty, addr, order } => {
            expect(func, *addr, Type::Ptr)?;
            atomic_type("atomic_load", *ty)?;
            if matches!(order, MemOrder::Release | MemOrder::AcqRel) {
                return Err(K::BadOrdering);
            }
            out.push(*ty);
        }
        InstData::AtomicStore { value, addr, order } => {
            atomic_type("atomic_store", value_ty(func, *value)?)?;
            expect(func, *addr, Type::Ptr)?;
            if matches!(order, MemOrder::Acquire | MemOrder::AcqRel) {
                return Err(K::BadOrdering);
            }
        }
        InstData::AtomicRmw {
            op, addr, value, ..
        } => {
            expect(func, *addr, Type::Ptr)?;
            let ty = value_ty(func, *value)?;
            let ok = ty.is_int() || (*op == RmwOp::Xchg && ty == Type::Ptr);
            if !ok {
                return Err(K::BadOperandType {
                    op: "atomic_rmw",
                    found: ty,
                });
            }
            out.push(ty);
        }
        InstData::CmpXchg {
            addr,
            expected,
            new,
            success,
            failure,
        } => {
            expect(func, *addr, Type::Ptr)?;
            let ty = same(func, *expected, *new)?;
            atomic_type("cmpxchg", ty)?;
            if matches!(failure, MemOrder::Release | MemOrder::AcqRel)
                || failure.strength() > success.strength()
            {
                return Err(K::BadOrdering);
            }
            out.push(ty);
            out.push(Type::Bool);
        }
        InstData::Fence { order } => {
            if *order == MemOrder::Relaxed {
                return Err(K::BadOrdering);
            }
        }
        InstData::MemCopy {
            dst,
            src,
            len,
            flags,
        }
        | InstData::MemMove {
            dst,
            src,
            len,
            flags,
        } => {
            expect(func, *dst, Type::Ptr)?;
            expect(func, *src, Type::Ptr)?;
            let lty = value_ty(func, *len)?;
            if !lty.is_unsigned_int() {
                return Err(K::BadOperandType {
                    op: data.opcode(),
                    found: lty,
                });
            }
            check_flags(*flags)?;
        }
        InstData::MemSet {
            dst,
            byte,
            len,
            flags,
        } => {
            expect(func, *dst, Type::Ptr)?;
            expect(func, *byte, Type::U8)?;
            let lty = value_ty(func, *len)?;
            if !lty.is_unsigned_int() {
                return Err(K::BadOperandType {
                    op: "memset",
                    found: lty,
                });
            }
            check_flags(*flags)?;
        }
        InstData::Call(call) => call_types(decls, func, call, out)?,
        InstData::Safepoint { gc } => {
            for &g in gc.iter() {
                if value_ty(func, g)? != Type::Ref {
                    return Err(K::BadGcList);
                }
            }
            out.extend(core::iter::repeat_n(Type::Ref, gc.len()));
        }
    }
    Ok(())
}

/// The kind of edge being checked, which decides which special arguments it may
/// carry.
#[derive(Clone, Copy)]
enum Edge<'a> {
    Plain,
    InvokeNormal(&'a [Type]),
    InvokeUnwind { results: &'a [Type], returns: usize },
    CheckNormal(Type),
    CheckError,
}

fn check_edge(func: &Function, call: &BlockCall, edge: Edge<'_>) -> Result<(), K> {
    let target = call.block;
    if !func.is_live_block(target) {
        return Err(K::BadBlockTarget { target });
    }
    if target == func.entry() {
        return Err(K::EntryBranchTarget);
    }
    let params = func.block_params(target);
    if params.len() != call.args.len() {
        return Err(K::EdgeArgCount {
            target,
            expected: params.len(),
            found: call.args.len(),
        });
    }
    for (&arg, &param) in call.args.iter().zip(params) {
        let want = func
            .value_type(param)
            .ok_or(K::ValueOutOfRange { value: param })?;
        let (value, found) = match (arg, edge) {
            (BlockArg::Value(v), _) => (Some(v), value_ty(func, v)?),
            (BlockArg::Result(i), Edge::InvokeNormal(results)) => {
                (None, *results.get(i as usize).ok_or(K::BadEdgeArg)?)
            }
            (BlockArg::Result(i), Edge::InvokeUnwind { results, returns }) => {
                if (i as usize) < returns {
                    return Err(K::BadEdgeArg);
                }
                (None, *results.get(i as usize).ok_or(K::BadEdgeArg)?)
            }
            (BlockArg::Result(0), Edge::CheckNormal(ty)) => (None, ty),
            (BlockArg::Exception, Edge::InvokeUnwind { .. }) => (None, Type::Ptr),
            (BlockArg::ErrorCode, Edge::CheckError) => (None, Type::U32),
            _ => return Err(K::BadEdgeArg),
        };
        if found != want {
            return Err(K::TypeMismatch {
                value,
                expected: want,
                found,
            });
        }
    }
    Ok(())
}

/// Checks a terminator's operands and edges against the function's signature.
pub(crate) fn check_terminator(
    decls: &Decls,
    func: &Function,
    sig: &Signature,
    term: &Terminator,
    scratch: &mut Vec<Type>,
) -> Result<(), K> {
    match term {
        Terminator::Jump(dest) => check_edge(func, dest, Edge::Plain),
        Terminator::Branch {
            cond,
            then_dest,
            else_dest,
        } => {
            expect(func, *cond, Type::Bool)?;
            check_edge(func, then_dest, Edge::Plain)?;
            check_edge(func, else_dest, Edge::Plain)
        }
        Terminator::Switch {
            value,
            cases,
            default,
        } => {
            let ty = value_ty(func, *value)?;
            let Some(width) = ty.int_bits() else {
                return Err(K::BadOperandType {
                    op: "switch",
                    found: ty,
                });
            };
            let mut seen: Vec<u64> = Vec::with_capacity(cases.len());
            for case in cases {
                if width < 64 && case.value >> width != 0 {
                    return Err(K::BadSwitchCase { value: case.value });
                }
                seen.push(case.value);
                check_edge(func, &case.dest, Edge::Plain)?;
            }
            seen.sort_unstable();
            if let Some(w) = seen.windows(2).find(|w| w[0] == w[1]) {
                return Err(K::BadSwitchCase { value: w[0] });
            }
            check_edge(func, default, Edge::Plain)
        }
        Terminator::Return { values } => {
            if values.len() != sig.returns.len() {
                return Err(K::ArgCount {
                    expected: sig.returns.len(),
                    found: values.len(),
                });
            }
            for (&v, &ty) in values.iter().zip(&sig.returns) {
                expect(func, v, ty)?;
            }
            Ok(())
        }
        Terminator::Unreachable | Terminator::Trap { .. } => Ok(()),
        Terminator::TailCall(call) => {
            scratch.clear();
            call_types(decls, func, call, scratch)?;
            let callee = call_sig(decls, func, call)?;
            if !call.gc.is_empty() || callee.returns != sig.returns || callee.conv != sig.conv {
                return Err(K::BadTailCall);
            }
            Ok(())
        }
        Terminator::Invoke {
            call,
            normal,
            unwind,
        } => {
            scratch.clear();
            call_types(decls, func, call, scratch)?;
            let returns = call_sig(decls, func, call)?.returns.len();
            if normal.block == unwind.block {
                return Err(K::LandingPadPredecessor {
                    block: unwind.block,
                });
            }
            check_edge(func, normal, Edge::InvokeNormal(scratch))?;
            check_edge(
                func,
                unwind,
                Edge::InvokeUnwind {
                    results: scratch,
                    returns,
                },
            )
        }
        Terminator::Check { op, normal, error } => {
            scratch.clear();
            inst_types(decls, func, op, true, scratch)?;
            let ty = scratch
                .first()
                .copied()
                .ok_or(K::NotFallible { op: op.opcode() })?;
            check_edge(func, normal, Edge::CheckNormal(ty))?;
            check_edge(func, error, Edge::CheckError)
        }
        Terminator::Resume { payload } => expect(func, *payload, Type::Ptr),
    }
}

/// Whether `block` is the unwind target of `term`.
pub(crate) fn unwind_target(term: &Terminator) -> Option<Block> {
    match term {
        Terminator::Invoke { unwind, .. } => Some(unwind.block),
        _ => None,
    }
}
