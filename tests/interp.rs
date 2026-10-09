//! The reference interpreter: operation semantics against independent references
//! on edge values (OPS §3–§5), and execution of memory, calls, exceptions,
//! safepoints, and budgets.

use ir_lang::interp::{
    self, Host, HostReturn, InterpError, Interpreter, Limits, Memory, NoHost, OpFault, Outcome,
    Val, eval_binary, eval_compare, eval_convert, eval_unary,
};
use ir_lang::{
    Aggregate, Attrs, BinaryOp, BlockArg, BlockCall, Call, CmpOp, ConvOp, DivZero, FloatToInt,
    Global, InstData, Linkage, MemFlags, MemOrder, Module, OpError, Overflow, Policy, Reloc, RmwOp,
    Shift, Signature, SymbolRef, Type, UnaryOp,
};

const INT_TYPES: [Type; 8] = [
    Type::I8,
    Type::I16,
    Type::I32,
    Type::I64,
    Type::U8,
    Type::U16,
    Type::U32,
    Type::U64,
];

fn width(ty: Type) -> u32 {
    ty.int_bits().unwrap()
}

fn range(ty: Type) -> (i128, i128) {
    let w = width(ty);
    if ty.is_signed_int() {
        (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1)
    } else {
        (0, (1i128 << w) - 1)
    }
}

fn to_bits(ty: Type, v: i128) -> u64 {
    let w = width(ty);
    let m: u128 = if w == 64 {
        u64::MAX as u128
    } else {
        (1u128 << w) - 1
    };
    ((v as u128) & m) as u64
}

fn from_bits(ty: Type, bits: u64) -> i128 {
    let w = width(ty);
    if ty.is_signed_int() {
        let v = bits as i128;
        if v >= 1i128 << (w - 1) {
            v - (1i128 << w)
        } else {
            v
        }
    } else {
        bits as i128
    }
}

/// The OPS edge values for an integer type: 0, 1, -1, MIN, MAX, MIN+1, MAX-1, and
/// a few in between.
fn edges(ty: Type) -> Vec<i128> {
    let (lo, hi) = range(ty);
    let mut v = vec![
        0,
        1,
        -1,
        lo,
        hi,
        lo + 1,
        hi - 1,
        2,
        -2,
        7,
        -8,
        (width(ty) as i128),
        hi / 3,
    ];
    v.retain(|x| *x >= lo && *x <= hi);
    v
}

/// The reference result of an integer operation: `Ok(Some(value))`, or the error
/// kind the operation must raise when its policy is `error`.
fn reference(op: BinaryOp, ty: Type, a: i128, b: i128, mask_shift: bool) -> Result<i128, OpError> {
    let (lo, hi) = range(ty);
    let fits = |v: i128| v >= lo && v <= hi;
    let w = width(ty) as i128;
    match op {
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
            let v = match op {
                BinaryOp::Add => a + b,
                BinaryOp::Sub => a - b,
                _ => a.checked_mul(b).unwrap_or(i128::MAX),
            };
            if fits(v) {
                Ok(v)
            } else {
                Err(OpError::ArithOverflow)
            }
        }
        BinaryOp::Div | BinaryOp::FloorDiv | BinaryOp::Rem | BinaryOp::FloorMod => {
            if b == 0 {
                return Err(OpError::DivByZero);
            }
            // Independent formulas: truncation via f64-free sign handling, floor via
            // the Euclidean remainder.
            let trunc_q = (a.abs() / b.abs()) * if (a < 0) == (b < 0) { 1 } else { -1 };
            let trunc_r = a - b * trunc_q;
            let floor_r = if trunc_r != 0 && ((trunc_r < 0) != (b < 0)) {
                trunc_r + b
            } else {
                trunc_r
            };
            let floor_q = (a - floor_r) / b;
            let v = match op {
                BinaryOp::Div => trunc_q,
                BinaryOp::FloorDiv => floor_q,
                BinaryOp::Rem => trunc_r,
                _ => floor_r,
            };
            if fits(v) {
                Ok(v)
            } else {
                Err(OpError::ArithOverflow)
            }
        }
        BinaryOp::Shl | BinaryOp::Shr => {
            let n = if b < 0 || b >= w {
                if !mask_shift {
                    return Err(OpError::ShiftOutOfRange);
                }
                (to_bits(ty, b) as i128) & (w - 1)
            } else {
                b
            };
            if op == BinaryOp::Shl {
                Ok(from_bits(ty, to_bits(ty, a << n)))
            } else {
                // Floor division by 2^n is the arithmetic shift for signed values
                // and the logical shift for unsigned ones.
                Ok(a.div_euclid(1i128 << n))
            }
        }
        BinaryOp::Min => Ok(a.min(b)),
        BinaryOp::Max => Ok(a.max(b)),
        _ => Ok(0),
    }
}

#[test]
fn test_ops_integer_binary_matches_reference_under_every_policy() {
    let ops = [
        BinaryOp::Add,
        BinaryOp::Sub,
        BinaryOp::Mul,
        BinaryOp::Div,
        BinaryOp::Rem,
        BinaryOp::FloorDiv,
        BinaryOp::FloorMod,
        BinaryOp::Shl,
        BinaryOp::Shr,
        BinaryOp::Min,
        BinaryOp::Max,
    ];
    let mut checked = 0u32;
    for ty in INT_TYPES {
        for op in ops {
            for (ov, dz, sh) in [
                (Overflow::Error, DivZero::Error, Shift::Error),
                (Overflow::Wrap, DivZero::Trap, Shift::Mask),
                (Overflow::Trap, DivZero::Error, Shift::Mask),
            ] {
                let need = Policy::required_binary(op, ty);
                let policy = Policy {
                    overflow: need.overflow.then_some(ov),
                    div_zero: need.div_zero.then_some(dz),
                    shift: need.shift.then_some(sh),
                    float_to_int: None,
                };
                for a in edges(ty) {
                    for b in edges(ty) {
                        let got = eval_binary(op, policy, ty, to_bits(ty, a), to_bits(ty, b));
                        let want = reference(op, ty, a, b, sh == Shift::Mask);
                        let expected = match want {
                            Ok(v) => Ok(to_bits(ty, v)),
                            Err(OpError::DivByZero) => Err(match dz {
                                DivZero::Trap => OpFault::Trap(OpError::DivByZero),
                                _ => OpFault::Error(OpError::DivByZero),
                            }),
                            Err(OpError::ArithOverflow) => match ov {
                                // Wrap: the exact result's low bits.
                                Overflow::Wrap => Ok(wrapped(op, ty, a, b)),
                                Overflow::Trap => Err(OpFault::Trap(OpError::ArithOverflow)),
                                _ => Err(OpFault::Error(OpError::ArithOverflow)),
                            },
                            Err(e) => Err(OpFault::Error(e)),
                        };
                        assert_eq!(got, expected, "{op:?} {ty} {a} {b} {policy}");
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked > 10_000);
}

/// Two's-complement wrap of the exact result.
fn wrapped(op: BinaryOp, ty: Type, a: i128, b: i128) -> u64 {
    let v = match op {
        BinaryOp::Add => a.wrapping_add(b),
        BinaryOp::Sub => a.wrapping_sub(b),
        BinaryOp::Mul => a.wrapping_mul(b),
        // Only MIN / -1 overflows a division: it wraps to MIN.
        _ => -a,
    };
    to_bits(ty, v)
}

#[test]
fn test_ops_unary_and_casts_match_reference() {
    for ty in INT_TYPES {
        let (lo, hi) = range(ty);
        for a in edges(ty) {
            let neg = -a;
            let got = eval_unary(
                UnaryOp::Neg,
                Policy::overflow(Overflow::Error),
                ty,
                to_bits(ty, a),
            );
            if neg >= lo && neg <= hi {
                assert_eq!(got, Ok(to_bits(ty, neg)));
            } else {
                assert_eq!(got, Err(OpFault::Error(OpError::ArithOverflow)));
            }
            let abs = eval_unary(
                UnaryOp::Abs,
                Policy::overflow(Overflow::Wrap),
                ty,
                to_bits(ty, a),
            );
            assert_eq!(abs, Ok(to_bits(ty, a.abs())));
            assert_eq!(
                eval_unary(UnaryOp::Not, Policy::NONE, ty, to_bits(ty, a)),
                Ok(to_bits(ty, !a))
            );
            for to in INT_TYPES {
                if to == ty {
                    continue;
                }
                let (tlo, thi) = range(to);
                let got = eval_convert(
                    ConvOp::IntCast,
                    Policy::overflow(Overflow::Error),
                    ty,
                    to,
                    to_bits(ty, a),
                );
                if a >= tlo && a <= thi {
                    assert_eq!(got, Ok(to_bits(to, a)), "{ty}->{to} {a}");
                } else {
                    assert_eq!(got, Err(OpFault::Error(OpError::ArithOverflow)));
                    let w = eval_convert(
                        ConvOp::IntCast,
                        Policy::overflow(Overflow::Wrap),
                        ty,
                        to,
                        to_bits(ty, a),
                    );
                    assert_eq!(w, Ok(to_bits(to, a)));
                }
            }
        }
    }
}

#[test]
fn test_ops_shift_amount_rules() {
    let err = Policy::shift(Shift::Error);
    let mask = Policy::shift(Shift::Mask);
    // Negative amounts of a signed type are out of range.
    assert_eq!(
        eval_binary(BinaryOp::Shl, err, Type::I32, 1, to_bits(Type::I32, -1)),
        Err(OpFault::Error(OpError::ShiftOutOfRange))
    );
    assert_eq!(
        eval_binary(BinaryOp::Shl, mask, Type::I32, 1, to_bits(Type::I32, -1)),
        Ok(1 << 31)
    );
    // An unsigned amount equal to the width is out of range; masked it is 0.
    assert_eq!(
        eval_binary(BinaryOp::Shr, mask, Type::U8, 0x80, 8),
        Ok(0x80)
    );
    // Arithmetic vs logical right shift.
    assert_eq!(
        eval_binary(BinaryOp::Shr, mask, Type::I8, 0x80, 1),
        Ok(0xc0)
    );
    assert_eq!(
        eval_binary(BinaryOp::Shr, mask, Type::U8, 0x80, 1),
        Ok(0x40)
    );
    // Bits shifted out of `shl` are discarded without an overflow error.
    assert_eq!(eval_binary(BinaryOp::Shl, err, Type::U8, 0xff, 4), Ok(0xf0));
}

/// IEEE `remainder` checked with exact integer arithmetic on dyadic values.
#[test]
fn test_ops_ieee_rem_is_exact() {
    for a in -40i64..=40 {
        for b in [-7i64, -4, -3, -1, 1, 2, 3, 4, 5, 8] {
            for k in [1i32, 4, 10] {
                let scale = 2f64.powi(k);
                let (x, y) = (a as f64 / scale, b as f64 / scale);
                // n = a / b rounded half to even, in integers.
                let (q, r) = (a.div_euclid(b), a.rem_euclid(b));
                let twice = 2 * r;
                let n = if twice > b.abs() || (twice == b.abs() && q.rem_euclid(2) != 0) {
                    if b > 0 { q + 1 } else { q - 1 }
                } else {
                    q
                };
                let want = (a - n * b) as f64 / scale;
                let got = f64::from_bits(
                    eval_binary(
                        BinaryOp::IeeeRem,
                        Policy::NONE,
                        Type::F64,
                        x.to_bits(),
                        y.to_bits(),
                    )
                    .unwrap(),
                );
                assert_eq!(got, want, "{x} ieee_rem {y}");
                // C fmod: the sign of the dividend.
                let fm = f64::from_bits(
                    eval_binary(
                        BinaryOp::Rem,
                        Policy::NONE,
                        Type::F64,
                        x.to_bits(),
                        y.to_bits(),
                    )
                    .unwrap(),
                );
                assert_eq!(fm, ((a % b) as f64) / scale);
            }
        }
    }
}

#[test]
fn test_ops_float_specials() {
    let f = |x: f64| x.to_bits();
    let min = |a, b| {
        f64::from_bits(eval_binary(BinaryOp::Min, Policy::NONE, Type::F64, f(a), f(b)).unwrap())
    };
    let max = |a, b| {
        f64::from_bits(eval_binary(BinaryOp::Max, Policy::NONE, Type::F64, f(a), f(b)).unwrap())
    };
    assert!(min(f64::NAN, 1.0).is_nan());
    assert!(max(1.0, f64::NAN).is_nan());
    assert!(min(0.0, -0.0).is_sign_negative());
    assert!(max(-0.0, 0.0).is_sign_positive());
    assert_eq!(min(-1.0, 3.0), -1.0);
    // Comparisons with NaN are false except `ne`.
    for op in [CmpOp::Eq, CmpOp::Lt, CmpOp::Le, CmpOp::Gt, CmpOp::Ge] {
        assert!(!eval_compare(op, Type::F64, f(f64::NAN), f(1.0)));
    }
    assert!(eval_compare(CmpOp::Ne, Type::F64, f(f64::NAN), f(f64::NAN)));
    // Division by zero is never an error.
    let d = eval_binary(BinaryOp::Div, Policy::NONE, Type::F64, f(1.0), f(0.0)).unwrap();
    assert_eq!(f64::from_bits(d), f64::INFINITY);
    // total_cmp gives -1/0/1 as i8 bits and orders -0 before +0.
    let t = |a: f64, b: f64| {
        eval_binary(BinaryOp::TotalCmp, Policy::NONE, Type::F64, f(a), f(b)).unwrap()
    };
    assert_eq!(t(-0.0, 0.0), 0xff);
    assert_eq!(t(1.0, 1.0), 0);
    assert_eq!(t(f64::NAN, f64::INFINITY), 1);
    // Rounding modes.
    let r = |op, x: f64| f64::from_bits(eval_unary(op, Policy::NONE, Type::F64, f(x)).unwrap());
    assert_eq!(r(UnaryOp::Round, 2.5), 3.0);
    assert_eq!(r(UnaryOp::Round, -2.5), -3.0);
    assert_eq!(r(UnaryOp::RoundEven, 2.5), 2.0);
    assert_eq!(r(UnaryOp::RoundEven, 3.5), 4.0);
    assert_eq!(r(UnaryOp::Trunc, -2.7), -2.0);
    assert_eq!(r(UnaryOp::Floor, -2.5), -3.0);
    assert_eq!(r(UnaryOp::Ceil, -2.5), -2.0);
    // neg/abs are sign-bit operations that keep NaN payloads.
    let payload = 0x7ff8_0000_0000_1234u64;
    assert_eq!(
        eval_unary(UnaryOp::Neg, Policy::NONE, Type::F64, payload).unwrap(),
        payload | (1 << 63)
    );
    assert_eq!(
        eval_unary(UnaryOp::Abs, Policy::NONE, Type::F64, payload | (1 << 63)).unwrap(),
        payload
    );
    // f32 arithmetic is single precision, not double rounded.
    let a = 16_777_216.0f32;
    let s = eval_binary(
        BinaryOp::Add,
        Policy::NONE,
        Type::F32,
        u64::from(a.to_bits()),
        u64::from(1.0f32.to_bits()),
    )
    .unwrap();
    assert_eq!(f32::from_bits(s as u32), a);
}

#[test]
fn test_ops_float_to_int_boundaries() {
    let err = Policy::float_to_int(FloatToInt::Error);
    let sat = Policy::float_to_int(FloatToInt::Saturate);
    let cases: [(f64, Type, Option<i128>); 10] = [
        (-0.9, Type::U8, Some(0)),
        (255.9, Type::U8, Some(255)),
        (256.0, Type::U8, None),
        (-1.0, Type::U8, None),
        (-128.9, Type::I8, Some(-128)),
        (-129.0, Type::I8, None),
        (9_223_372_036_854_775_807.0, Type::I64, None), // rounds to 2^63
        (
            -9_223_372_036_854_775_808.0,
            Type::I64,
            Some(i64::MIN as i128),
        ),
        (
            18_446_744_073_709_549_568.0,
            Type::U64,
            Some(18_446_744_073_709_549_568),
        ),
        (f64::INFINITY, Type::I32, None),
    ];
    for (x, to, want) in cases {
        let got = eval_convert(ConvOp::FloatToInt, err, Type::F64, to, x.to_bits());
        match want {
            Some(v) => assert_eq!(got, Ok(to_bits(to, v)), "{x} -> {to}"),
            None => {
                assert_eq!(
                    got,
                    Err(OpFault::Error(OpError::InvalidConversion)),
                    "{x} -> {to}"
                );
                let s = eval_convert(ConvOp::FloatToInt, sat, Type::F64, to, x.to_bits()).unwrap();
                let (lo, hi) = range(to);
                assert_eq!(s, to_bits(to, if x < 0.0 { lo } else { hi }));
            }
        }
    }
    let nan = eval_convert(
        ConvOp::FloatToInt,
        sat,
        Type::F32,
        Type::I16,
        u64::from(f32::NAN.to_bits()),
    );
    assert_eq!(nan, Ok(0));
}

#[test]
fn test_ops_other_conversions() {
    let cv = |op, from, to, a| eval_convert(op, Policy::NONE, from, to, a);
    assert_eq!(
        cv(ConvOp::Sext, Type::I8, Type::I64, 0x80),
        Ok(0xffff_ffff_ffff_ff80)
    );
    assert_eq!(cv(ConvOp::Sext, Type::U8, Type::U16, 0x80), Ok(0xff80));
    assert_eq!(cv(ConvOp::Zext, Type::I8, Type::I64, 0x80), Ok(0x80));
    assert_eq!(cv(ConvOp::Narrow, Type::U64, Type::I8, 0x1234), Ok(0x34));
    assert_eq!(
        cv(ConvOp::Bitcast, Type::F64, Type::U64, 1.0f64.to_bits()),
        Ok(1.0f64.to_bits())
    );
    assert_eq!(cv(ConvOp::BoolToInt, Type::Bool, Type::I32, 1), Ok(1));
    assert_eq!(
        cv(ConvOp::CharFromU32, Type::U32, Type::U32, 0x1F600),
        Ok(0x1F600)
    );
    assert_eq!(
        cv(ConvOp::CharFromU32, Type::U32, Type::U32, 0xD800),
        Err(OpFault::Error(OpError::InvalidChar))
    );
    assert_eq!(
        cv(ConvOp::CharFromU32, Type::U32, Type::U32, 0x110000),
        Err(OpFault::Error(OpError::InvalidChar))
    );
    // int_to_float rounds to nearest, ties to even.
    let r = cv(ConvOp::IntToFloat, Type::U64, Type::F32, 16_777_217).unwrap();
    assert_eq!(f32::from_bits(r as u32), 16_777_216.0);
    let r = cv(ConvOp::IntToFloat, Type::I64, Type::F64, (i64::MAX) as u64).unwrap();
    assert_eq!(f64::from_bits(r), 9_223_372_036_854_775_808.0);
    // f64 -> f32 rounds to nearest even.
    let r = cv(
        ConvOp::FloatCast,
        Type::F64,
        Type::F32,
        1.000_000_059_604_644_8f64.to_bits(),
    )
    .unwrap();
    assert_eq!(f32::from_bits(r as u32), 1.0);
}

// ----------------------------------------------------------------- execution

fn module() -> Module {
    Module::new("t")
}

#[test]
fn test_check_terminator_routes_errors_with_their_codes() {
    let mut m = module();
    let f = m
        .declare_function(
            "div",
            &Signature::new(&[Type::I32, Type::I32], &[Type::I32, Type::U32]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let (x, y) = (b.param(0).unwrap(), b.param(1).unwrap());
    let ok = b.create_block(&[Type::I32]).unwrap();
    let err = b.create_block(&[Type::U32]).unwrap();
    let p = Policy::overflow(Overflow::Error).with_div_zero(DivZero::Error);
    b.check(
        InstData::Binary {
            op: BinaryOp::Div,
            policy: p,
            args: [x, y],
        },
        BlockCall::with_args(ok, &[BlockArg::Result(0)]),
        BlockCall::with_args(err, &[BlockArg::ErrorCode]),
    )
    .unwrap();
    b.switch_to(ok).unwrap();
    let q = b.block_param(ok, 0).unwrap();
    let zero = b.iconst(Type::U32, 0).unwrap();
    b.ret(&[q, zero]).unwrap();
    b.switch_to(err).unwrap();
    let code = b.block_param(err, 0).unwrap();
    let z = b.iconst(Type::I32, 0).unwrap();
    b.ret(&[z, code]).unwrap();
    m.validate().unwrap();

    let run = |a: i32, c: i32| interp::run(&m, f, &[Val::i32(a), Val::i32(c)]).unwrap();
    assert_eq!(run(7, 2), Outcome::Return(vec![Val::i32(3), Val::u32(0)]));
    assert_eq!(run(7, 0), Outcome::Return(vec![Val::i32(0), Val::u32(2)]));
    assert_eq!(
        run(i32::MIN, -1),
        Outcome::Return(vec![Val::i32(0), Val::u32(1)])
    );
}

#[test]
fn test_trap_policy_aborts_the_program() {
    let mut m = module();
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[Type::U8], &[Type::U8]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let x = b.param(0).unwrap();
    let y = b.add(x, x, Overflow::Trap).unwrap();
    b.ret(&[y]).unwrap();
    assert_eq!(
        interp::run(&m, f, &[Val::u8(100)]).unwrap(),
        Outcome::Return(vec![Val::u8(200)])
    );
    assert_eq!(
        interp::run(&m, f, &[Val::u8(200)]).unwrap(),
        Outcome::Trap(1)
    );
}

#[test]
fn test_memory_aggregates_and_faults() {
    let mut m = module();
    let pair = m
        .declare_aggregate(Aggregate::structure(&[Type::U8.into(), Type::I64.into()]))
        .unwrap();
    let arr = m
        .declare_aggregate(Aggregate::array(pair.into(), 4))
        .unwrap();
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[Type::I64], &[Type::I64]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let i = b.param(0).unwrap();
    let ss = b.stack_slot_for(arr.into()).unwrap();
    let base = b.stack_addr(ss).unwrap();
    let elem = b.elem_addr(base, pair.into(), i).unwrap();
    let field = b.field_addr(elem, pair, 1).unwrap();
    let v = b.iconst(Type::I64, -42).unwrap();
    b.store(v, field, MemFlags::new(8)).unwrap();
    let back = b.load(Type::I64, field, MemFlags::new(8)).unwrap();
    b.ret(&[back]).unwrap();
    m.validate().unwrap();
    assert_eq!(
        interp::run(&m, f, &[Val::i64(3)]).unwrap(),
        Outcome::Return(vec![Val::i64(-42)])
    );
    // Index 4 is past the 64-byte slot.
    assert!(matches!(
        interp::run(&m, f, &[Val::i64(9)]),
        Err(InterpError::MemoryFault { .. })
    ));
}

#[test]
fn test_misaligned_and_null_accesses_fault() {
    let mut m = module();
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[Type::I64], &[Type::U32]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let off = b.param(0).unwrap();
    let ss = b.stack_slot(16, 8).unwrap();
    let base = b.stack_addr(ss).unwrap();
    let p = b.ptr_offset(base, off).unwrap();
    let v = b.load(Type::U32, p, MemFlags::new(4)).unwrap();
    b.ret(&[v]).unwrap();
    assert_eq!(
        interp::run(&m, f, &[Val::i64(4)]).unwrap(),
        Outcome::Return(vec![Val::u32(0)])
    );
    assert!(matches!(
        interp::run(&m, f, &[Val::i64(2)]),
        Err(InterpError::Misaligned { .. })
    ));

    let mut m = module();
    let g = m
        .declare_function("g", &Signature::new(&[], &[Type::U8]), Linkage::Export)
        .unwrap();
    let mut b = m.build(g).unwrap();
    let null = b.null(Type::Ptr).unwrap();
    let v = b.load(Type::U8, null, MemFlags::new(1)).unwrap();
    b.ret(&[v]).unwrap();
    assert_eq!(
        interp::run(&m, g, &[]),
        Err(InterpError::MemoryFault { addr: 0, len: 1 })
    );
}

#[test]
fn test_memcpy_overlap_is_undefined_but_memmove_is_fine() {
    for overlapping_copy in [true, false] {
        let mut m = module();
        let f = m
            .declare_function("f", &Signature::new(&[], &[Type::U8]), Linkage::Export)
            .unwrap();
        let mut b = m.build(f).unwrap();
        let ss = b.stack_slot(16, 1).unwrap();
        let base = b.stack_addr(ss).unwrap();
        let seven = b.iconst(Type::U8, 7).unwrap();
        let n8 = b.iconst(Type::U64, 8).unwrap();
        b.memset(base, seven, n8, MemFlags::new(1)).unwrap();
        let four = b.iconst(Type::I64, 4).unwrap();
        let dst = b.ptr_offset(base, four).unwrap();
        if overlapping_copy {
            b.memcpy(dst, base, n8, MemFlags::new(1)).unwrap();
        } else {
            b.memmove(dst, base, n8, MemFlags::new(1)).unwrap();
        }
        let eleven = b.iconst(Type::I64, 11).unwrap();
        let last = b.ptr_offset(base, eleven).unwrap();
        let v = b.load(Type::U8, last, MemFlags::new(1)).unwrap();
        b.ret(&[v]).unwrap();
        let got = interp::run(&m, f, &[]);
        if overlapping_copy {
            assert_eq!(got, Err(InterpError::Overlap));
        } else {
            assert_eq!(got.unwrap(), Outcome::Return(vec![Val::u8(7)]));
        }
    }
}

#[test]
fn test_atomics() {
    let mut m = module();
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[], &[Type::I32, Type::I32, Type::Bool, Type::I32]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let ss = b.stack_slot(4, 4).unwrap();
    let p = b.stack_addr(ss).unwrap();
    let five = b.iconst(Type::I32, 5).unwrap();
    b.atomic_store(five, p, MemOrder::Release).unwrap();
    let neg = b.iconst(Type::I32, -3).unwrap();
    let old = b.atomic_rmw(RmwOp::Min, p, neg, MemOrder::SeqCst).unwrap();
    let cx = b
        .cmpxchg(p, neg, five, MemOrder::AcqRel, MemOrder::Acquire)
        .unwrap();
    let r = b.results(cx);
    b.fence(MemOrder::SeqCst).unwrap();
    let now = b.atomic_load(Type::I32, p, MemOrder::Acquire).unwrap();
    b.ret(&[old, r.get(0).unwrap(), r.get(1).unwrap(), now])
        .unwrap();
    m.validate().unwrap();
    assert_eq!(
        interp::run(&m, f, &[]).unwrap(),
        Outcome::Return(vec![
            Val::i32(5),
            Val::i32(-3),
            Val::bool(true),
            Val::i32(5)
        ])
    );
}

#[test]
fn test_globals_relocations_and_indirect_calls() {
    let mut m = module();
    let sig = Signature::new(&[Type::I64], &[Type::I64]);
    let sig_id = m.intern_signature(&sig).unwrap();
    let inc = m.declare_function("inc", &sig, Linkage::Local).unwrap();
    let table = m
        .declare_global(
            "table",
            Global::zeroed(8, 8).with_reloc(Reloc {
                offset: 0,
                target: SymbolRef::Func(inc),
                addend: 0,
            }),
        )
        .unwrap();
    let main = m.declare_function("main", &sig, Linkage::Export).unwrap();
    let mut b = m.build(inc).unwrap();
    let x = b.param(0).unwrap();
    let one = b.iconst(Type::I64, 1).unwrap();
    let y = b.add(x, one, Overflow::Wrap).unwrap();
    b.ret(&[y]).unwrap();
    let mut b = m.build(main).unwrap();
    let x = b.param(0).unwrap();
    let t = b.global_addr(table).unwrap();
    let fp = b.load(Type::Ptr, t, MemFlags::new(8)).unwrap();
    let c = b.call(Call::indirect(sig_id, fp, &[x])).unwrap();
    let r = b.results(c).first().unwrap();
    b.ret(&[r]).unwrap();
    m.validate().unwrap();
    assert_eq!(
        interp::run(&m, main, &[Val::i64(41)]).unwrap(),
        Outcome::Return(vec![Val::i64(42)])
    );
}

#[test]
fn test_indirect_call_with_wrong_signature_faults() {
    let mut m = module();
    let s1 = Signature::new(&[], &[]);
    let s2 = Signature::new(&[Type::I32], &[]);
    let s2id = m.intern_signature(&s2).unwrap();
    let callee = m.declare_function("c", &s1, Linkage::Local).unwrap();
    let main = m.declare_function("main", &s1, Linkage::Export).unwrap();
    let mut b = m.build(callee).unwrap();
    b.ret(&[]).unwrap();
    let mut b = m.build(main).unwrap();
    let fp = b.func_addr(callee).unwrap();
    let arg = b.iconst(Type::I32, 0).unwrap();
    b.call(Call::indirect(s2id, fp, &[arg])).unwrap();
    b.ret(&[]).unwrap();
    m.validate().unwrap();
    assert!(matches!(
        interp::run(&m, main, &[]),
        Err(InterpError::BadFunctionPointer { .. })
    ));
}

/// `thrower` raises; `middle` calls it plainly (the exception passes through);
/// `main` invokes `middle` and catches.
#[test]
fn test_exceptions_unwind_through_plain_calls_to_the_invoke() {
    let mut m = module();
    let s = Signature::new(&[], &[Type::I64]);
    let thrower = m.declare_function("thrower", &s, Linkage::Local).unwrap();
    let middle = m.declare_function("middle", &s, Linkage::Local).unwrap();
    let main = m.declare_function("main", &s, Linkage::Export).unwrap();

    let mut b = m.build(thrower).unwrap();
    let k = b.iconst(Type::U64, 4096 + 77).unwrap();
    let p = b
        .convert(ConvOp::IntToPtr, Policy::NONE, k, Type::Ptr)
        .unwrap();
    b.resume(p).unwrap();

    let mut b = m.build(middle).unwrap();
    let c = b.call(Call::direct(thrower, &[])).unwrap();
    let r = b.results(c).first().unwrap();
    b.ret(&[r]).unwrap();

    let mut b = m.build(main).unwrap();
    let ok = b.create_block(&[Type::I64]).unwrap();
    let pad = b.create_block(&[Type::Ptr]).unwrap();
    b.invoke(
        Call::direct(middle, &[]),
        BlockCall::with_args(ok, &[BlockArg::Result(0)]),
        BlockCall::with_args(pad, &[BlockArg::Exception]),
    )
    .unwrap();
    b.switch_to(ok).unwrap();
    let v = b.block_param(ok, 0).unwrap();
    b.ret(&[v]).unwrap();
    b.switch_to(pad).unwrap();
    let e = b.block_param(pad, 0).unwrap();
    let as_int = b
        .convert(ConvOp::PtrToInt, Policy::NONE, e, Type::I64)
        .unwrap();
    b.ret(&[as_int]).unwrap();
    m.validate().unwrap();

    assert_eq!(
        interp::run(&m, main, &[]).unwrap(),
        Outcome::Return(vec![Val::i64(4096 + 77)])
    );
    // Without the invoke the exception leaves the program.
    assert_eq!(
        interp::run(&m, middle, &[]).unwrap(),
        Outcome::Unwind(Val::ptr(4096 + 77))
    );
}

#[test]
fn test_tail_calls_do_not_grow_the_stack() {
    let mut m = module();
    let s = Signature::new(&[Type::U64, Type::U64], &[Type::U64]);
    let f = m.declare_function("count", &s, Linkage::Export).unwrap();
    let mut b = m.build(f).unwrap();
    let (n, acc) = (b.param(0).unwrap(), b.param(1).unwrap());
    let zero = b.iconst(Type::U64, 0).unwrap();
    let done = b.compare(CmpOp::Eq, n, zero).unwrap();
    let base = b.create_block(&[]).unwrap();
    let rec = b.create_block(&[]).unwrap();
    b.branch(done, base, &[], rec, &[]).unwrap();
    b.switch_to(base).unwrap();
    b.ret(&[acc]).unwrap();
    b.switch_to(rec).unwrap();
    let one = b.iconst(Type::U64, 1).unwrap();
    let n1 = b.sub(n, one, Overflow::Wrap).unwrap();
    let a1 = b.add(acc, one, Overflow::Wrap).unwrap();
    b.tail_call(Call::direct(f, &[n1, a1])).unwrap();
    m.validate().unwrap();
    let mut vm = Interpreter::with_limits(
        &m,
        NoHost,
        Limits {
            max_depth: 4,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        vm.call(f, &[Val::u64(100_000), Val::u64(0)]).unwrap(),
        Outcome::Return(vec![Val::u64(100_000)])
    );
}

#[test]
fn test_budgets_stop_runaway_programs() {
    let mut m = module();
    let s = Signature::new(&[], &[]);
    let f = m.declare_function("forever", &s, Linkage::Export).unwrap();
    let mut b = m.build(f).unwrap();
    b.call(Call::direct(f, &[])).unwrap();
    b.ret(&[]).unwrap();
    assert_eq!(interp::run(&m, f, &[]), Err(InterpError::StackOverflow));
    let mut vm = Interpreter::with_limits(
        &m,
        NoHost,
        Limits {
            fuel: 50,
            max_depth: 1_000_000,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(vm.call(f, &[]), Err(InterpError::FuelExhausted));
}

#[test]
fn test_invalid_modules_and_arguments_are_refused() {
    let mut m = module();
    let f = m
        .declare_function("f", &Signature::new(&[Type::I32], &[]), Linkage::Export)
        .unwrap();
    assert!(matches!(
        interp::run(&m, f, &[Val::i32(1)]),
        Err(InterpError::Invalid(_))
    ));
    let mut b = m.build(f).unwrap();
    b.ret(&[]).unwrap();
    assert_eq!(interp::run(&m, f, &[]), Err(InterpError::BadArguments));
    assert_eq!(
        interp::run(&m, f, &[Val::i64(1)]),
        Err(InterpError::BadArguments)
    );
    let g = m
        .declare_function("g", &Signature::new(&[], &[]), Linkage::Import)
        .unwrap();
    assert_eq!(
        interp::run(&m, g, &[]),
        Err(InterpError::UnresolvedImport { name: "g".into() })
    );
}

// ------------------------------------------------------------------- safepoints

struct Gc;

impl Host for Gc {
    fn call(
        &mut self,
        name: &str,
        args: &[Val],
        mem: &mut Memory,
    ) -> Result<HostReturn, InterpError> {
        match name {
            "alloc" => Ok(HostReturn::Values(vec![mem.alloc_object(args[0].bits())?])),
            "poll" => Ok(HostReturn::Values(vec![])),
            _ => Err(InterpError::UnresolvedImport { name: name.into() }),
        }
    }
}

/// Allocates an object, stores 7 into it, crosses safepoints relocating the
/// reference each time, and reads it back through the relocated reference.
#[test]
fn test_relocated_references_survive_safepoints() {
    let mut m = module();
    let alloc = m
        .declare_function(
            "alloc",
            &Signature::new(&[Type::U64], &[Type::Ref]),
            Linkage::Import,
        )
        .unwrap();
    let poll = m
        .declare_function("poll", &Signature::new(&[], &[]), Linkage::Import)
        .unwrap();
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[], &[Type::I64, Type::Bool]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let size = b.iconst(Type::U64, 16).unwrap();
    let c = b.call(Call::direct(alloc, &[size])).unwrap();
    let obj = b.results(c).first().unwrap();
    let p = b
        .convert(ConvOp::RefToPtr, Policy::NONE, obj, Type::Ptr)
        .unwrap();
    let seven = b.iconst(Type::I64, 7).unwrap();
    b.store(seven, p, MemFlags::new(8)).unwrap();
    // A call safepoint relocating `obj` ...
    let c = b.call(Call::direct(poll, &[]).with_gc(&[obj])).unwrap();
    let obj2 = b.results(c).first().unwrap();
    // ... and an explicit one.
    let sp = b.safepoint(&[obj2]).unwrap();
    let obj3 = b.results(sp).first().unwrap();
    let null = b.null(Type::Ref).unwrap();
    let nonnull = b.compare(CmpOp::Ne, obj3, null).unwrap();
    let p3 = b
        .convert(ConvOp::RefToPtr, Policy::NONE, obj3, Type::Ptr)
        .unwrap();
    let v = b.load(Type::I64, p3, MemFlags::new(8)).unwrap();
    b.ret(&[v, nonnull]).unwrap();
    m.validate().unwrap();
    let mut vm = Interpreter::new(&m, Gc).unwrap();
    assert_eq!(
        vm.call(f, &[]).unwrap(),
        Outcome::Return(vec![Val::i64(7), Val::bool(true)])
    );
}

/// A `nogc` call is not a safepoint: the reference stays valid without a GC list.
#[test]
fn test_nogc_calls_need_no_relocation() {
    let mut m = module();
    let alloc = m
        .declare_function(
            "alloc",
            &Signature::new(&[Type::U64], &[Type::Ref]),
            Linkage::Import,
        )
        .unwrap();
    let poll = m
        .declare_function("poll", &Signature::new(&[], &[]), Linkage::Import)
        .unwrap();
    m.set_attrs(poll, Attrs::NOGC).unwrap();
    let f = m
        .declare_function("f", &Signature::new(&[], &[Type::Ref]), Linkage::Export)
        .unwrap();
    let mut b = m.build(f).unwrap();
    let size = b.iconst(Type::U64, 8).unwrap();
    let c = b.call(Call::direct(alloc, &[size])).unwrap();
    let obj = b.results(c).first().unwrap();
    b.call(Call::direct(poll, &[])).unwrap();
    b.ret(&[obj]).unwrap();
    m.validate().unwrap();
    let mut vm = Interpreter::new(&m, Gc).unwrap();
    assert!(matches!(vm.call(f, &[]).unwrap(), Outcome::Return(_)));
}

/// A host that hands back a reference minted before a safepoint: the interpreter
/// catches the stale use at run time.
#[test]
fn test_stale_reference_from_host_is_caught() {
    struct Stale(Option<Val>);
    impl Host for Stale {
        fn call(
            &mut self,
            name: &str,
            _: &[Val],
            mem: &mut Memory,
        ) -> Result<HostReturn, InterpError> {
            match name {
                "keep" => {
                    let r = mem.alloc_object(8)?;
                    self.0 = Some(r);
                    Ok(HostReturn::Values(vec![]))
                }
                _ => Ok(HostReturn::Values(vec![
                    self.0.unwrap_or(Val::new(Type::Ref, 0)),
                ])),
            }
        }
    }
    let mut m = module();
    let keep = m
        .declare_function("keep", &Signature::new(&[], &[]), Linkage::Import)
        .unwrap();
    let get = m
        .declare_function("get", &Signature::new(&[], &[Type::Ref]), Linkage::Import)
        .unwrap();
    m.set_attrs(get, Attrs::NOGC).unwrap();
    let f = m
        .declare_function("f", &Signature::new(&[], &[Type::Ptr]), Linkage::Export)
        .unwrap();
    let mut b = m.build(f).unwrap();
    b.call(Call::direct(keep, &[])).unwrap();
    let c = b.call(Call::direct(get, &[])).unwrap();
    let r = b.results(c).first().unwrap();
    let p = b
        .convert(ConvOp::RefToPtr, Policy::NONE, r, Type::Ptr)
        .unwrap();
    b.ret(&[p]).unwrap();
    // The derived pointer escapes through `ret`: the validator refuses it first.
    assert!(m.validate().is_err());
    let mut b = m.edit(f).unwrap();
    let entry = b.func().entry();
    b.remove_terminator(entry).unwrap();
    let null = b.null(Type::Ptr).unwrap();
    let null_ref = b.null(Type::Ref).unwrap();
    let same = b.compare(CmpOp::Eq, r, null_ref).unwrap();
    let out = b.select(same, null, null).unwrap();
    b.ret(&[out]).unwrap();
    m.validate().unwrap();
    let mut vm = Interpreter::new(&m, Stale(None)).unwrap();
    assert_eq!(vm.call(f, &[]), Err(InterpError::StaleRef));
}

#[test]
fn test_32_bit_modules_wrap_addresses_at_32_bits() {
    let mut m = Module::with_pointer_width("w", ir_lang::PointerWidth::P32);
    let f = m
        .declare_function(
            "f",
            &Signature::new(&[Type::U32], &[Type::U32]),
            Linkage::Export,
        )
        .unwrap();
    let mut b = m.build(f).unwrap();
    let x = b.param(0).unwrap();
    let p = b
        .convert(ConvOp::IntToPtr, Policy::NONE, x, Type::Ptr)
        .unwrap();
    let big = b.iconst(Type::I64, 0x1_0000_0010).unwrap();
    let q = b.ptr_offset(p, big).unwrap();
    let back = b
        .convert(ConvOp::PtrToInt, Policy::NONE, q, Type::U32)
        .unwrap();
    b.ret(&[back]).unwrap();
    m.validate().unwrap();
    assert_eq!(
        interp::run(&m, f, &[Val::u32(5)]).unwrap(),
        Outcome::Return(vec![Val::u32(0x15)])
    );
}

#[test]
fn test_large_memory_limits_cost_only_what_is_touched() {
    let mut m = module();
    let f = m
        .declare_function("f", &Signature::new(&[], &[Type::U8]), Linkage::Export)
        .unwrap();
    let mut b = m.build(f).unwrap();
    let ss = b.stack_slot(1024, 8).unwrap();
    let p = b.stack_addr(ss).unwrap();
    let v = b.load(Type::U8, p, MemFlags::new(1)).unwrap();
    b.ret(&[v]).unwrap();
    for _ in 0..200 {
        let mut vm = Interpreter::with_limits(
            &m,
            NoHost,
            Limits {
                memory: u64::MAX,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(vm.call(f, &[]).unwrap(), Outcome::Return(vec![Val::u8(0)]));
        let a = vm.memory_mut().alloc(1 << 20, 16).unwrap();
        assert_eq!(vm.memory().read(a + (1 << 20) - 1, 1).unwrap(), &[0]);
    }
}
